use std::cmp::min;

use anchor_lang::{prelude::*, system_program};
use anchor_spl::{
    token::{
        self,
        spl_token::{self, native_mint},
    },
    token_2022::spl_token_2022::{
        self,
        extension::{
            transfer_fee::TransferFeeConfig, BaseStateWithExtensions, StateWithExtensions,
        },
    },
    token_interface::{self, spl_pod::primitives::PodU16, TokenAccount, TokenInterface},
};
use crate::{bebop_rfq::AmountWithExpiry, error::BebopError, instructions::utils::{transfer, unwrap_sol}, SHARED_ACCOUNT};


// Replay is prevented by Solana's recent_blockhash expiry (~1.5 min) and
// validator-level transaction signature deduplication. The maker must co-sign
// every swap tx; AmountWithExpiry carries its own on-chain expiry as a second
// line of defense. No on-chain nonce registry is needed.
//
// Off-chain policy: maker signing infrastructure MUST use recent_blockhash, not
// durable nonce accounts. A durable-nonce RFQ tx stays valid indefinitely and
// could be broadcast by anyone who obtains a copy within the AmountWithExpiry
// window. No on-chain guard is possible — this is enforced at the signer level.
pub fn handle_swap<'c: 'info, 'info>(
    ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
    input_amount: u64,
    output_amounts: Vec<AmountWithExpiry>,
    event_id: u64,
    shared_account_bump: u8, // canonical bump for [SHARED_ACCOUNT] PDA;
    // used only when taker.is_signer == false. Avoids find_program_address
    // (~2000 CU) on the shared-PDA path. Pass 0 for normal signed swaps.
    wsol_bump: u8, // canonical bump for [TEMPORARY_WSOL_TOKEN_ACCOUNT, maker];
    // used only on wSOL swap paths. Pass 0 for non-wSOL swaps.
) -> Result<()> {
    // Expiry type: use i64 to match Clock::unix_timestamp and JAM's order.expiry.
    // AmountWithExpiry.expiry is u64 in the struct definition — cast to i64 here.
    // Off-chain builders: always use positive Unix timestamps; u64::MAX → i64::MAX
    // for "never expires" (both are far future for practical purposes).
    // TODO: change AmountWithExpiry.expiry to i64 in the struct definition to
    // remove this cast and eliminate the type gap with JAM entirely.
    let now = Clock::get()?.unix_timestamp;
    let mut output_amount: u64 = 0;
    // N3: guard empty ladder — loop body never runs on empty vec, giving
    // a misleading OrderExpired error instead of an explicit invalid-input one.
    require!(!output_amounts.is_empty(), BebopError::InvalidOutputAmount);
    // Degrading quote ladder: amounts non-increasing, expiries strictly increasing.
    // The first non-expired entry is selected; entries after it are never used and
    // are therefore not validated past the break. A malicious caller could append
    // unsorted entries after the first valid one — they are harmless since the break
    // prevents any further iteration. Validation only needs to be complete up to
    // and including the selected entry.
    for (i, amount_with_expiry) in output_amounts.iter().enumerate() {
        require!(
            i == 0 ||
            (amount_with_expiry.amount <= output_amounts[i - 1].amount && (amount_with_expiry.expiry as i64) > (output_amounts[i - 1].expiry as i64)),
            BebopError::InvalidOutputAmount
        );
        if (amount_with_expiry.expiry as i64) >= now {
            output_amount = amount_with_expiry.amount;
            break;
        }
    }
    require!(output_amount > 0, BebopError::OrderExpired);
    let mut bump: u8 = 0;
    let filled_taker_amount: u64;
    if !&ctx.accounts.taker.is_signer{
        // Use caller-supplied bump: create_program_address (single sha256)
        // vs find_program_address (up to 255 sha256 iterations).
        bump = shared_account_bump;
        let expected_pda_address = Pubkey::create_program_address(
            &[SHARED_ACCOUNT, &[bump]], &crate::ID,
        ).map_err(|_| error!(BebopError::WrongSharedAccountAddress))?;
        require_keys_eq!(ctx.accounts.taker.key(), expected_pda_address, BebopError::WrongSharedAccountAddress);
        // Read the current balance of the shared PDA's input account rather than
        // trusting input_amount from instruction data.
        // N4: A third party can donate tokens to the shared PDA before this
        // instruction runs, inflating filled_taker_amount. The maker is protected
        // (filled_maker_amount is capped at output_amount). The shared PDA must
        // be controlled off-chain — only the intended taker should fund it. In a chained multi-hop tx,
        // a preceding instruction funds this account; we consume exactly what was
        // deposited, not a caller-supplied value. Anchor re-deserializes accounts
        // fresh per instruction so token_acc.amount reflects prior-instruction state.
        // filled_maker_amount is then prorated or capped at output_amount, so a
        // donated surplus in the shared account cannot cause the maker to over-pay.
        filled_taker_amount = match &ctx.accounts.taker_input_mint_token_account {
            Some(token_acc) => token_acc.amount,
            // For native SOL, taker.lamports() includes the rent-exempt reserve.
            // Transferring all lamports would drain the shared PDA to zero and
            // garbage-collect it, breaking subsequent fills until someone re-funds.
            // Subtract the rent-exempt minimum so the PDA survives the transfer.
            None => {
                let rent_reserve = Rent::get()?.minimum_balance(0);
                ctx.accounts.taker.lamports().saturating_sub(rent_reserve)
            },
        };
    } else {
        filled_taker_amount = input_amount;
    }
    let binding: [&[&[u8]]; 1] = [&[SHARED_ACCOUNT, &[bump]]];
    let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);

    require!(filled_taker_amount > 0, BebopError::ZeroTakerAmount);
    match (
        &ctx.accounts.taker_input_mint_token_account,
        &ctx.accounts.maker_input_mint_token_account,
    ) {
        (None, None) => {
            require_keys_eq!(ctx.accounts.input_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);

            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.taker.to_account_info(),
                        to: ctx.accounts.maker.to_account_info(),
                    },
                ),
                filled_taker_amount,
            )?;
        }
        (None, Some(maker_input_mint_token_account)) => {
            require_keys_eq!(ctx.accounts.input_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);

            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.taker.to_account_info(),
                        to: maker_input_mint_token_account.to_account_info(),
                    },
                ),
                filled_taker_amount,
            )?;
            token::sync_native(CpiContext::new(
                ctx.accounts.input_token_program.to_account_info(),
                token::SyncNative {
                    account: maker_input_mint_token_account.to_account_info(),
                },
            ))?;
        }
        (Some(taker_input_mint_token_account), None) => {
            require_keys_eq!(ctx.accounts.input_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);

            unwrap_sol(
                ctx.accounts.maker.to_account_info(),
                ctx.accounts.taker.to_account_info(),
                taker_input_mint_token_account.to_account_info(),
                None,
                ctx.remaining_accounts.iter().next(),
                ctx.accounts.input_mint.to_account_info(),
                ctx.accounts.input_token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                filled_taker_amount,
                wsol_bump,
            )?;
        }
        (Some(taker_input_mint_token_account), Some(maker_input_mint_token_account)) => transfer(
            ctx.accounts.input_token_program.to_account_info(),
            taker_input_mint_token_account.to_account_info(),
            maker_input_mint_token_account.to_account_info(),
            ctx.accounts.taker.to_account_info(),
            ctx.accounts.input_mint.to_account_info(),
            filled_taker_amount,
            if ctx.accounts.taker.is_signer {None} else {pda_seeds}
        )?,
    }

    // Prorate maker output for partial fills. Integer division truncates, slightly
    // favouring the maker on dust amounts — not a security issue.
    // Cap at output_amount even if filled_taker_amount > input_amount: this handles
    // the shared-PDA surplus case (donated tokens, rounding) without letting the
    // maker be forced to over-pay beyond their quoted amount.
    let filled_maker_amount: u64 = if filled_taker_amount < input_amount {
        ((output_amount as u128 * filled_taker_amount as u128) / input_amount as u128) as u64
    } else {
        output_amount
    };
    require!(filled_maker_amount > 0, BebopError::ZeroMakerAmount);
    match (
        &ctx.accounts.maker_output_mint_token_account,
        &ctx.accounts.receiver_output_mint_token_account,
    ) {
        (None, None) => {
            require_keys_eq!(ctx.accounts.output_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);

            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.maker.to_account_info(),
                        to: ctx.accounts.receiver.to_account_info(),
                    },
                ),
                filled_maker_amount,
            )?;
        }
        (Some(maker_output_mint_token_account), None) => {
            require_keys_eq!(ctx.accounts.output_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);
            unwrap_sol(
                ctx.accounts.maker.to_account_info(),
                ctx.accounts.maker.to_account_info(),
                maker_output_mint_token_account.to_account_info(),
                Some(ctx.accounts.receiver.to_account_info()),
                ctx.remaining_accounts.iter().next(),
                ctx.accounts.output_mint.to_account_info(),
                ctx.accounts.output_token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                filled_maker_amount,
                wsol_bump,
            )?;
        }
        (None, Some(receiver_output_mint_token_account)) => {
            require_keys_eq!(ctx.accounts.output_mint.key(), native_mint::ID, BebopError::InvalidNativeTokenAddress);

            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.maker.to_account_info(),
                        to: receiver_output_mint_token_account.to_account_info(),
                    },
                ),
                filled_maker_amount,
            )?;
            token::sync_native(CpiContext::new(
                ctx.accounts.output_token_program.to_account_info(),
                token::SyncNative {
                    account: receiver_output_mint_token_account.to_account_info(),
                },
            ))?;
        }
        (Some(maker_output_mint_token_account), Some(receiver_output_mint_token_account)) => transfer(
            ctx.accounts.output_token_program.to_account_info(),
            maker_output_mint_token_account.to_account_info(),
            receiver_output_mint_token_account.to_account_info(),
            ctx.accounts.maker.to_account_info(),
            ctx.accounts.output_mint.to_account_info(),
            filled_maker_amount,
            None
        )?,
    }
    emit!(BebopSwap{
        event_id: event_id,
        maker_address: ctx.accounts.maker.key(),
        taker_token: ctx.accounts.input_mint.key(),
        maker_token: ctx.accounts.output_mint.key(),
        filled_taker_amount,
        filled_maker_amount,
    });
    Ok(())
}



#[derive(Accounts)]
pub struct Swap<'info> {
    /// CHECK: taker is either a co-signing user or the shared-pda.
    /// When not a signer, the handler validates it is the canonical shared PDA via
    /// require_keys_eq against find_program_address. When it is a signer, the
    /// runtime enforces the signature. Both paths are safe.
    #[account(mut)]
    pub taker: UncheckedAccount<'info>,
    #[account(mut)]
    pub maker: Signer<'info>,
    /// CHECK: receiver is intentionally unconstrained — the maker (Signer) co-signs
    /// the transaction which includes this account explicitly, so the maker sees and
    /// accepts the receiver before signing. Unlike a taker-submitted order (JAM),
    /// here the maker drives execution and implicitly approves the destination by
    /// agreeing to sign the tx.
    #[account(mut)]
    pub receiver: UncheckedAccount<'info>,
    #[account(
        mut,
        token::authority = taker,
        token::mint = input_mint,
        token::token_program = input_token_program
    )]
    pub taker_input_mint_token_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(
        mut,
        token::authority = maker,
        token::mint = input_mint,
        token::token_program = input_token_program
    )]
    pub maker_input_mint_token_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    /// Receiver's output token account.
    ///
    /// Unlike JAM's receiver_buy_ata, no explicit runtime owner check is needed
    /// here. The accounts macro constraint token::authority = receiver fires at
    /// deserialization time and enforces ata.owner == receiver before the handler
    /// runs. This works because `receiver` is an account in the accounts struct —
    /// a known pubkey at deserialization time.
    ///
    /// JAM cannot do the same because its receiver is order.receiver, a field
    /// inside the SolanaJamOrder instruction argument. Anchor decodes accounts
    /// before it decodes instruction args, so order.receiver doesn't exist yet
    /// when the accounts macro runs, making the runtime check in the handler
    /// mandatory for JAM.
    #[account(
        mut,
        token::authority = receiver,
        token::mint = output_mint,
        token::token_program = output_token_program
    )]
    pub receiver_output_mint_token_account: Option<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::authority = maker,
        token::mint = output_mint,
        token::token_program = output_token_program
    )]
    pub maker_output_mint_token_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    /// CHECK: Validated by token account mint check
    pub input_mint: UncheckedAccount<'info>,
    pub input_token_program: Interface<'info, TokenInterface>,
    /// CHECK: Validated by token account mint check
    pub output_mint: UncheckedAccount<'info>,
    pub output_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[event]
struct BebopSwap {
    event_id: u64,
    maker_address: Pubkey,
    taker_token: Pubkey,
    maker_token: Pubkey,
    filled_taker_amount: u64,
    filled_maker_amount: u64,
}


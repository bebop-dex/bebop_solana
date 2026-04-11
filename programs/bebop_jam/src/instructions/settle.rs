
// ─── JAM Settlement — Solana ─────────────────────────────────────────────────
//
// settleBatch: not implemented.
// Pack multiple settle instructions into one transaction — atomicity is free.
//
// settleBebopBlend: not implemented.
// Solver includes bebop_rfq::Swap as a SolanaInteraction entry. Any program
// is reachable through interactions — no dedicated entrypoint needed.
//
// ─── Flash loans ─────────────────────────────────────────────────────────────
// Gated by use_jam_authority: true on flash_borrow / flash_repay interaction
// entries. run_interactions calls invoke_signed with JAM_AUTHORITY_SEED, so
// FlashLoanProvider's FlashBorrow constraint (signer + address == config.bebop_authority)
// is satisfied only via JAM. Zero compile-time binding — FlashLoanProvider stores the
// expected pubkey once via update_config(set_bebop_authority).
//
// Execution sequence (all within one settle() call):
//   interaction[i]     : quid::flash_borrow { use_jam_authority: true }
//   interaction[i+1..j]: solver-defined — JAM has no knowledge of these
//   interaction[j+1]   : quid::flash_repay { use_jam_authority: true }
//
// Solana reverts all state on any failure. Sysvar lookahead in flash_borrow
// confirms flash_repay exists later in the same transaction.
// Flash loans are free — no fee charged by FlashLoanProvider.
//
// SPL path: token_amount > 0 triggers remaining_accounts vault transfer.
// Blocked interaction targets: system_program, spl_token, spl_token_2022,
// and JAM itself (re-entrancy). All other programs are solver-defined.
//
// ─── Fees ────────────────────────────────────────────────────────────────────
//   partnerFee  → partner_account (order.partner_fee_bps)
//   protocolFee → config.treasury (config.protocol_fee_bps, default 0)
//                 pair 0 only; treasury ≠ admin multisig
//   FlashLoanProvider        → no explicit JAM fee; protocol earns SOL/USD delta on repay
//
// ─── Admin / upgrade authority — see admin_timelock.rs ───────────────────────
//
// ─── Scope divergence from EVM JamSettlement.sol ─────────────────────────────
//   usingPermit2       — not needed: taker co-signs the Solana tx (strictly
//                        stronger than EIP-712; tx commits to accounts + amounts)
//   hooksHash          — hooks_enabled: bool; hooks_enabled: true rejected with
//                        HooksNotSupported rather than silently skipped
//   JamInteraction.value — encode as explicit system_program::transfer CPI
//   partnerInfo        — partner_fee_bps: u16 direct field, no uint256 packing
//
// ─── remaining_accounts layout ───────────────────────────────────────────────
//   handle_settle:
//     [0 .. (S-1)*5)           : additional sell pairs, groups of 5:
//                                [taker_sell, custody_sell, sell_mint, sell_prog, solver_sell]
//     [(S-1)*5 .. +(B-1)*4)    : additional buy pairs, groups of 4:
//                                [custody_buy, receiver_buy, buy_mint, buy_prog]
//     [interaction_base ..)    : interaction accounts (may overlap earlier slots)
//
//   handle_settle_internal:
//     [0 .. (S-1)*4)           : additional sell pairs, groups of 4:
//                                [taker_sell, solver_sell, sell_mint, sell_prog]
//     [(S-1)*4 .. +(B-1)*4)    : additional buy pairs, groups of 4:
//                                [solver_buy, receiver_buy, buy_mint, buy_prog]
//
//   S = sell_tokens.len(), B = buy_tokens.len()

use anchor_lang::{prelude::*, solana_program::program::{invoke, invoke_signed}};
use anchor_spl::{
    token::{self, spl_token::native_mint},
    token_2022::spl_token_2022::{
        self,
        extension::{
            confidential_transfer::ConfidentialTransferMint,
            permanent_delegate::PermanentDelegate,
            transfer_fee::TransferFeeConfig,
            transfer_hook::TransferHook,
            BaseStateWithExtensions, StateWithExtensions,
        },
    },
    token_interface::{self, spl_pod::primitives::PodU16, TokenAccount, TokenInterface},
};

use crate::error::JamError;
use super::state::*;


// ─── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(order: crate::instructions::SolanaJamOrder)]
pub struct Settle<'info> {
    #[account(mut)]
    pub solver: Signer<'info>,

    /// Taker co-signs the transaction — commits to all accounts and amounts.
    /// mut required: native-SOL sell path uses system_program::transfer {from: taker, to: custody},
    /// and native-SOL buy path uses system_program::transfer {from: custody, to: taker/receiver}.
    /// system_program::transfer requires both from AND to to be writable.
    #[account(mut)]
    pub taker: Signer<'info>,

    #[account(seeds = [JAM_CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, JamConfig>,

    #[account(
        init, payer = solver, space = NonceRecord::SPACE,
        seeds = [NONCE_SEED, order.taker.as_ref(), &order.nonce.to_le_bytes()],
        bump,
    )]
    pub nonce_record: Account<'info, NonceRecord>,

    // ── Token pair 0 ──────────────────────────────────────────────────────────
    // None = native SOL (lamport transfer); Some = SPL/T22 ATA.
    // wSOL gap vs RFQ: JAM handles native SOL only (lamport path).
    // RFQ also supports wSOL-in-ATA via unwrap_sol/sync_native.
    // Clients holding wSOL must close (unwrap) before calling settle.

    /// Taker's sell token account, or None if selling native SOL.
    #[account(
        mut,
        token::authority = taker,
        token::mint = sell_mint,
        token::token_program = sell_token_program,
    )]
    pub taker_sell_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Custody sell account — holds sell tokens during interactions.
    /// None if selling native SOL (custody_authority holds lamports directly).
    #[account(
        mut,
        token::authority = custody_authority,
        token::mint = sell_mint,
        token::token_program = sell_token_program,
    )]
    pub custody_sell_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Custody buy account — interactions deliver buy tokens here.
    /// None if buying native SOL.
    #[account(
        mut,
        token::authority = custody_authority,
        token::mint = buy_mint,
        token::token_program = buy_token_program,
    )]
    pub custody_buy_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Receiver's buy token account, or None if receiving native SOL.
    ///
    /// Security note: we cannot constrain token::authority = order.receiver here
    /// because Anchor's accounts macro runs before instruction args are decoded —
    /// order.receiver is a runtime value, not available at deserialization time.
    /// Instead we enforce ata.owner == order.receiver explicitly at the top of
    /// handle_settle. Without this check a malicious solver could pass any valid
    /// ATA for the correct mint (including their own) and the transfer would
    /// succeed, delivering buy tokens to the wrong address while all balance
    /// checks pass. The taker's signature on the order is not sufficient
    /// protection alone because the taker signs the order struct, not the
    /// specific account pubkeys passed to the instruction.
    #[account(mut, token::mint = buy_mint, token::token_program = buy_token_program)]
    pub receiver_buy_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// CHECK: custody authority PDA — signs token transfers from custody.
    /// Cannot be an interaction target.
    /// mut required: native-SOL paths use system_program::transfer to/from this PDA.
    #[account(
        mut,
        seeds = [CUSTODY_SEED, order.taker.as_ref(), &order.nonce.to_le_bytes()],
        bump,
    )]
    pub custody_authority: AccountInfo<'info>,

    /// JAM authority PDA — appended as signer on every CPI via invoke_signed.
    /// Target programs gate privileged instructions on this PDA being a signer.
    /// JAM has no knowledge of any target program's instruction layout.
    /// CHECK: verified by seeds
    #[account(seeds = [JAM_AUTHORITY_SEED], bump = config.authority_bump)]
    pub jam_authority: AccountInfo<'info>,

    /// CHECK: sell mint (pair 0)
    pub sell_mint: AccountInfo<'info>,
    /// CHECK: buy mint (pair 0)
    pub buy_mint: AccountInfo<'info>,

    pub sell_token_program: Interface<'info, TokenInterface>,
    pub buy_token_program: Interface<'info, TokenInterface>,

    /// Partner's ATA for the buy token (pair 0), or wallet for native SOL buy.
    /// Required when order.partner_fee_bps > 0 and order.partner is set.
    #[account(mut)]
    pub partner_account: Option<AccountInfo<'info>>,

    /// Treasury ATA (SPL) or treasury wallet (native SOL) for protocol fee pair 0.
    /// Required when config.protocol_fee_bps > 0; pass None to skip protocol fee.
    /// Validated to belong to config.treasury in the handler.
    #[account(mut)]
    pub treasury_buy_ata: Option<AccountInfo<'info>>,

    /// Solver's sell token account (receives sell tokens after buy delivery is confirmed).
    /// None if sell side is native SOL (solver receives lamports directly).
    /// Mint validated implicitly by spl_transfer (token program enforces mint match).
    #[account(mut)]
    pub solver_sell_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Per-taker reentrancy guard. init here; closed (rent → solver) at end of handler.
    /// Prevents interaction → X → JAM re-entry using the taker's propagated signer.
    pub token_program: Program<'info, token::Token>,
    pub system_program: Program<'info, System>,
    // remaining_accounts layout:
    //   [0 .. (S-1)*5)             : additional sell pairs, (S-1) groups of 5:
    //     [taker_sell_i, custody_sell_i, sell_mint_i, sell_prog_i, solver_sell_i]
    //     native SOL: [taker_wallet, custody_authority, native_mint, system_prog, solver_wallet]
    //   [(S-1)*5 .. (S-1)*5+(B-1)*4): additional buy pairs, (B-1) groups of 4:
    //     [custody_buy_i, receiver_buy_i, buy_mint_i, buy_prog_i]
    //   [interaction_base ..)        : interaction accounts (found by pubkey)
}

#[derive(Accounts)]
#[instruction(order: crate::instructions::SolanaJamOrder)]
pub struct SettleInternal<'info> {
    #[account(mut)]
    pub solver: Signer<'info>,

    #[account(mut)]
    pub taker: Signer<'info>,

    #[account(seeds = [JAM_CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, JamConfig>,

    #[account(
        init, payer = solver, space = NonceRecord::SPACE,
        seeds = [NONCE_SEED, order.taker.as_ref(), &order.nonce.to_le_bytes()],
        bump,
    )]
    pub nonce_record: Account<'info, NonceRecord>,

    #[account(mut, token::authority = taker, token::mint = sell_mint, token::token_program = sell_token_program)]
    pub taker_sell_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    #[account(mut, token::authority = solver, token::mint = sell_mint, token::token_program = sell_token_program)]
    pub solver_sell_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    #[account(mut, token::authority = solver, token::mint = buy_mint, token::token_program = buy_token_program)]
    pub solver_buy_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Receiver's buy token account. Same reasoning as Settle.receiver_buy_ata:
    /// owner == order.receiver is enforced in handle_settle_internal, not here.
    #[account(mut, token::mint = buy_mint, token::token_program = buy_token_program)]
    pub receiver_buy_ata: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// CHECK: sell mint
    pub sell_mint: AccountInfo<'info>,
    /// CHECK: buy mint
    pub buy_mint: AccountInfo<'info>,

    pub sell_token_program: Interface<'info, TokenInterface>,
    pub buy_token_program: Interface<'info, TokenInterface>,

    pub token_program: Program<'info, token::Token>,
    pub system_program: Program<'info, System>,

    /// Partner's ATA for the buy token (pair 0), or wallet for native SOL buy.
    /// If order.partner_fee_bps > 0 and order.partner is set, this account receives the fee.
    #[account(mut)]
    pub partner_account: Option<AccountInfo<'info>>,

}

// ─── Handlers ─────────────────────────────────────────────────────────────────

// Shared validation macro — eliminates ~20 lines duplicated between
// handle_settle and handle_settle_internal. Macro avoids threading ctx
// fields through a function signature; inlines cleanly at both sites.
pub fn handle_settle<'c: 'info, 'info>(
    ctx: Context<'_, '_, 'c, 'info, Settle<'info>>,
    order: SolanaJamOrder,
    interactions: Vec<SolanaInteraction>,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(now < order.expiry, JamError::OrderExpired);
    // Expiry type note: JAM uses i64 (matches Clock::unix_timestamp).
    // RFQ uses u64 for AmountWithExpiry.expiry — off-chain builders should
    // use positive Unix timestamps only to avoid sign-extension surprises.
    require!(order.nonce != 0, JamError::ZeroNonce);
    let receiver = order.effective_receiver(); // None → taker
    // A5 (nonce front-run DoS): taker uses random 64-bit nonce; pre-creation
    // probabilistically infeasible (~0.002 SOL cost, zero attacker profit).
    require!(!order.hooks_enabled, JamError::HooksNotSupported);
    require_keys_eq!(ctx.accounts.taker.key(), order.taker, JamError::InvalidTaker);
    require!(!order.sell_amounts.is_empty(), JamError::ZeroAmount);
    require!(!order.buy_amounts.is_empty(), JamError::ZeroAmount);
    require!(order.sell_tokens.len() == order.sell_amounts.len(), JamError::LengthMismatch);
    require!(order.buy_tokens.len() == order.buy_amounts.len(), JamError::LengthMismatch);
    require_keys_eq!(ctx.accounts.sell_mint.key(), order.sell_tokens[0], JamError::MintMismatch);
    require_keys_eq!(ctx.accounts.buy_mint.key(),  order.buy_tokens[0],  JamError::MintMismatch);
    require!(order.sell_amounts[0] > 0 && order.buy_amounts[0] > 0, JamError::ZeroAmount);
    // Same-custody guard: sell_mint==buy_mint with one side None (native SOL) and
    // other Some (wSOL ATA) is valid — they use different accounts.
    require!(
        order.sell_tokens[0] != order.buy_tokens[0]
            || ctx.accounts.custody_sell_ata.is_none() != ctx.accounts.custody_buy_ata.is_none(),
        JamError::MintMismatch
    );

    if order.exclusivity_deadline.map_or(false, |d| now <= d) {
        if let Some(exec) = order.executor {
            require_keys_eq!(ctx.accounts.solver.key(), exec, JamError::ExclusivityViolation);
        }
    }

    let nr = &mut ctx.accounts.nonce_record;
    nr.expiry = order.expiry; nr.bump = ctx.bumps.nonce_record;

    // ── Receiver validation ───────────────────────────────────────────────────
    // Enforce that receiver_buy_ata is actually owned by order.receiver.
    // This cannot be done in the accounts macro (see receiver_buy_ata comment).
    // Pair 0 SPL case:
    if let Some(ata) = &ctx.accounts.receiver_buy_ata {
        require_keys_eq!(
            ata.owner,
            receiver,
            JamError::InvalidReceiver
            // A solver could pass any ATA for the correct mint. Without this
            // check they could route buy tokens to their own account, stealing
            // the taker's output while all balance checks still pass.
        );
    }
    // Native SOL pair 0: receiver is checked implicitly below by using
    // order.receiver directly as the destination rather than taker (the prior
    // code used taker as a fallback which would fail silently if receiver != taker).

    let authority_seeds: &[&[u8]] = &[JAM_AUTHORITY_SEED, &[ctx.accounts.config.authority_bump]];
    let ca_bump = ctx.bumps.custody_authority;
    let ca_seeds: &[&[u8]] = &[
        CUSTODY_SEED, order.taker.as_ref(), &order.nonce.to_le_bytes(), &[ca_bump],
    ];

    // ── Transfer all sell tokens: taker → custody ────────────────────────────

    // Pair 0
    transfer_into_custody(
        &order.sell_tokens[0],
        order.sell_amounts[0],
        ctx.accounts.taker.to_account_info(),
        ctx.accounts.taker_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.custody_authority.to_account_info(),
        ctx.accounts.custody_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.sell_mint.to_account_info(),
        ctx.accounts.sell_token_program.to_account_info(),
        ctx.accounts.system_program.to_account_info(),
        None,
    )?;

    // Additional sell pairs
    let s = order.sell_tokens.len();
    let b = order.buy_tokens.len();
    for i in 1..s {
        let base = (i - 1) * 5; // groups of 5: taker_sell, custody_sell, mint, prog, solver_sell
        require!(base + 4 < ctx.remaining_accounts.len(), JamError::AccountNotFound);
        let taker_sell = &ctx.remaining_accounts[base];
        let custody_sell = &ctx.remaining_accounts[base + 1];
        let sell_mint_i = &ctx.remaining_accounts[base + 2];
        let sell_prog_i = &ctx.remaining_accounts[base + 3];
        // A9: reject fake token programs — a no-op transfer would let sell tokens
        // bypass custody, potentially leaving the sell-side unfunded.
        require!(
            sell_prog_i.key() == anchor_spl::token::ID
                || sell_prog_i.key() == anchor_spl::token_2022::ID,
            JamError::InteractionTargetProtected
        );

        // Verify custody_sell_i is owned by the custody PDA.
        // Without this check a solver could pass their own ATA as custody_sell:
        // taker's sell tokens flow to the solver directly while the solver
        // pre-funds custody_buy from elsewhere — extracting sell tokens as a bonus
        // while the taker still receives the buy side. For native SOL the system
        // program does not have a token-account owner field; custody_authority
        // receives lamports and identity is verified by the custody PDA seeds in
        // transfer_into_custody's system_program::transfer call.
        if order.sell_tokens[i] != native_mint::ID {
            use anchor_lang::solana_program::program_pack::Pack;
            let data = custody_sell.try_borrow_data()?;
            require!(
                data.len() >= anchor_spl::token::spl_token::state::Account::LEN,
                JamError::InvalidReceiver
            );
            let ta = anchor_spl::token::spl_token::state::Account::unpack(
                &data[..anchor_spl::token::spl_token::state::Account::LEN],
            ).map_err(|_| error!(JamError::InvalidReceiver))?;
            require_keys_eq!(
                ta.owner,
                ctx.accounts.custody_authority.key(),
                JamError::InvalidReceiver
            );
        }

        transfer_into_custody(
            &order.sell_tokens[i],
            order.sell_amounts[i],
            ctx.accounts.taker.to_account_info(),
            Some(taker_sell.to_account_info()),
            ctx.accounts.custody_authority.to_account_info(),
            Some(custody_sell.to_account_info()),
            sell_mint_i.to_account_info(),
            sell_prog_i.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            None,
        )?;
    }

    // No pre-interaction snapshot needed. The custody authority PDA is derived
    // from [CUSTODY_SEED, taker, nonce] where nonce is unique per order (enforced
    // by the NonceRecord init constraint). The custody ATAs are therefore always
    // fresh — their balance before interactions is always 0 in production.
    // We check the absolute post-interaction balance against buy_amounts, which
    // is equivalent to a delta-from-zero check and allows solvers to fund custody
    // either via interactions or via a preceding instruction in the same tx.
    let buy_base = (s - 1) * 5; // sell groups are 5 wide; buy groups start here

    // ── Run interactions ─────────────────────────────────────────────────────

    run_interactions(
        ctx.remaining_accounts,
        &interactions,
        ctx.accounts.custody_authority.key(),
        &ctx.accounts.jam_authority,
        authority_seeds,
    )?;

    // ── Verify and transfer buy tokens: custody → receiver ───────────────────

    let mut buy_amounts_filled: Vec<u64> = Vec::with_capacity(b);

    // Pair 0 — no reload() needed: balance_of reads raw bytes via
    // try_borrow_data() which always reflects live post-CPI data.
    let received_0 = {
        balance_of(
            &order.buy_tokens[0],
            ctx.accounts.custody_authority.to_account_info(),
            ctx.accounts.custody_buy_ata.as_ref().map(|a| a.to_account_info()),
        )
    };
    require!(received_0 >= order.buy_amounts[0], JamError::InsufficientOutput);
    let (_net_0, partner_fee_0) = decode_partner_fee(received_0, order.partner_fee_bps)?;
    // Protocol fee: deducted from gross, separate from partner fee.
    let proto_fee_bps = ctx.accounts.config.protocol_fee_bps as u128;
    let protocol_fee_0_gross = if proto_fee_bps > 0 {
        (received_0 as u128 * proto_fee_bps / 10_000) as u64
    } else { 0 };

    // STUCK-FUNDS GUARD: only deduct a fee from the gross if the corresponding
    // destination account is actually present. If partner_account or treasury_buy_ata
    // is missing but the fee is non-zero, the fee amount would be subtracted from what
    // the receiver gets yet never transferred — leaving tokens permanently locked in
    // the per-order custody PDA (which has no reclaim instruction).
    // Resolution: if the fee cannot be forwarded, the receiver gets that amount instead.
    let will_pay_partner = partner_fee_0 > 0
        && order.partner.is_some()
        && ctx.accounts.partner_account.is_some();
    let will_pay_treasury = protocol_fee_0_gross > 0
        && ctx.accounts.treasury_buy_ata.is_some();
    let actual_partner_fee_0   = if will_pay_partner  { partner_fee_0       } else { 0 };
    let actual_protocol_fee_0  = if will_pay_treasury { protocol_fee_0_gross } else { 0 };

    let net_after_all_fees_0 = received_0
        .saturating_sub(actual_partner_fee_0)
        .saturating_sub(actual_protocol_fee_0);
    // Verify taker receives at least buy_amounts[0] AFTER all fees actually paid.
    require!(net_after_all_fees_0 >= order.buy_amounts[0], JamError::InsufficientOutput);
    buy_amounts_filled.push(net_after_all_fees_0);

    // Fee flow: partnerFee → partner_account (integration frontend)
    if actual_partner_fee_0 > 0 {
        if let (Some(pk), Some(pa)) = (order.partner, ctx.accounts.partner_account.as_ref()) {
            // pa is the partner's ATA for SPL buy tokens, or their wallet for native SOL.
            // require_keys_eq!(pa.key(), pk) would always fail for ATAs because the ATA
            // address is a derived PDA — never equal to the wallet pubkey.
            // For SPL: unpack the token account and verify owner == order.partner.
            // For native SOL: pa IS the wallet, so compare keys directly.
            // A10: spl_token base layout (165 bytes) is identical across spl_token
            // and Token-2022. A crafted non-standard account could pass unpack without
            // the owner check triggering. Low severity — solver redirects their own fees.
            // Production hardening: derive expected ATA address and compare keys.
            // Use ATA presence as discriminator — matches transfer_from_custody routing.
            // wSOL buy (native_mint + Some ATA) takes the token-account branch.
            if ctx.accounts.custody_buy_ata.is_some() {
                use anchor_lang::solana_program::program_pack::Pack;
                let data = pa.try_borrow_data()?;
                if data.len() >= anchor_spl::token::spl_token::state::Account::LEN {
                    if let Ok(ta) = anchor_spl::token::spl_token::state::Account::unpack(
                        &data[..anchor_spl::token::spl_token::state::Account::LEN]
                    ) {
                        require_keys_eq!(ta.owner, pk, JamError::InvalidReceiver);
                    }
                }
            } else {
                require_keys_eq!(pa.key(), pk, JamError::InvalidReceiver);
            }
            transfer_from_custody(
                &order.buy_tokens[0],
                actual_partner_fee_0,
                ctx.accounts.custody_authority.to_account_info(),
                ctx.accounts.custody_buy_ata.as_ref().map(|a| a.to_account_info()),
                pa.to_account_info(),
                Some(pa.to_account_info()),
                ctx.accounts.buy_mint.to_account_info(),
                ctx.accounts.buy_token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ca_seeds,
            )?;
        }
    }

    // Fee flow: protocolFee → config.treasury (Bebop revenue; default 0 on Solana)
    // treasury_buy_ata must be the treasury's ATA for SPL, or treasury wallet for SOL.
    // Skipped when None (protocol_fee_bps == 0 or treasury_buy_ata not provided).
    if actual_protocol_fee_0 > 0 {
        if let Some(tba) = ctx.accounts.treasury_buy_ata.as_ref() {
            // A13: validate treasury_buy_ata belongs to config.treasury.
            // Use ATA presence as discriminator (matches transfer_from_custody routing):
            // custody_buy_ata.is_some() = token account path (SPL, T22, or wSOL).
            // custody_buy_ata.is_none() = native SOL lamport path.
            // native_mint check:
            // sent wSOL buys (native_mint + Some ATA) to the wallet-address branch.
            if ctx.accounts.custody_buy_ata.is_some() {
                use anchor_lang::solana_program::program_pack::Pack;
                let data = tba.try_borrow_data()?;
                if data.len() >= anchor_spl::token::spl_token::state::Account::LEN {
                    if let Ok(ta) = anchor_spl::token::spl_token::state::Account::unpack(
                        &data[..anchor_spl::token::spl_token::state::Account::LEN]
                    ) {
                        require_keys_eq!(ta.owner, ctx.accounts.config.treasury, JamError::InvalidReceiver);
                    }
                }
            } else {
                // Native SOL: treasury_buy_ata IS the treasury wallet
                require_keys_eq!(tba.key(), ctx.accounts.config.treasury, JamError::InvalidReceiver);
            }
            transfer_from_custody(
                &order.buy_tokens[0],
                actual_protocol_fee_0,
                ctx.accounts.custody_authority.to_account_info(),
                ctx.accounts.custody_buy_ata.as_ref().map(|a| a.to_account_info()),
                tba.to_account_info(),
                Some(tba.to_account_info()),
                ctx.accounts.buy_mint.to_account_info(),
                ctx.accounts.buy_token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ca_seeds,
            )?;
        }
    }

    // For native SOL (receiver_buy_ata == None), transfer lamports to order.receiver
    // directly. The prior code used ctx.accounts.taker as a fallback which would
    // silently succeed even if receiver != taker, allowing a solver to redirect
    // SOL output to the taker regardless of what the order specified.
    //
    // recv_ata is always passed as recipient_ata so that transfer_from_custody can use
    // it as the SPL "to" account. For native SOL recv_ata is None, so the native-SOL
    // branch falls through to recipient (recv_account) instead — no behavioral change
    // on that path. The prior code passed None here, causing recipient_ata.unwrap()
    // to panic on every SPL transfer.
    let recv_ata = ctx.accounts.receiver_buy_ata.as_ref().map(|a| a.to_account_info());
    // For native SOL (recv_ata == None): find order.receiver in remaining_accounts.
    // The prior code fell back to ctx.accounts.taker when receiver was absent,
    // silently misfilling any order where receiver != taker. Now we require the
    // receiver wallet to be present so the error is deterministic.
    let recv_account = if let Some(ata) = recv_ata.clone() {
        ata
    } else if receiver == ctx.accounts.taker.key() {
        ctx.accounts.taker.to_account_info()
    } else {
        ctx.remaining_accounts.iter()
            .find(|a| a.key() == receiver)
            .cloned()
            .ok_or(error!(JamError::AccountNotFound))?
    };
    transfer_from_custody(
        &order.buy_tokens[0],
        net_after_all_fees_0,
        ctx.accounts.custody_authority.to_account_info(),
        ctx.accounts.custody_buy_ata.as_ref().map(|a| a.to_account_info()),
        recv_account,
        recv_ata,
        ctx.accounts.buy_mint.to_account_info(),
        ctx.accounts.buy_token_program.to_account_info(),
        ctx.accounts.system_program.to_account_info(),
        ca_seeds,
    )?;

    // Additional buy pairs
    for i in 1..b {
        let base = buy_base + (i - 1) * 4;
        let custody_buy_i = &ctx.remaining_accounts[base];
        let receiver_buy_i = &ctx.remaining_accounts[base + 1];
        let buy_mint_i = &ctx.remaining_accounts[base + 2];
        let buy_prog_i = &ctx.remaining_accounts[base + 3];
        require!(
            buy_prog_i.key() == anchor_spl::token::ID
                || buy_prog_i.key() == anchor_spl::token_2022::ID,
            JamError::InteractionTargetProtected
        );

        // Verify custody_buy_i is owned by the custody PDA — prevents a solver
        // from passing a spoofed account to inflate the apparent received amount.
        {
            use anchor_lang::solana_program::program_pack::Pack;
            let data = custody_buy_i.try_borrow_data()?;
            if data.len() >= anchor_spl::token::spl_token::state::Account::LEN {
                if let Ok(ta) = anchor_spl::token::spl_token::state::Account::unpack(
                    &data[..anchor_spl::token::spl_token::state::Account::LEN]
                ) {
                    require_keys_eq!(
                        ta.owner,
                        ctx.accounts.custody_authority.key(),
                        JamError::InvalidReceiver
                    );
                }
            }
        }

        // Verify receiver_buy_i is owned by order.receiver — same attack surface
        // as pair 0: a solver could route additional-pair buy tokens to any ATA
        // for the correct mint. order.receiver is the single receiver for all pairs
        // (multi-receiver is not in scope; all buy tokens go to the same address).
        if order.buy_tokens[i] == anchor_spl::token::spl_token::native_mint::ID {
            // Native SOL additional pair: no ATA to unpack, validate the
            // wallet address directly. Without this, the solver can pass any
            // writable account and divert the SOL away from order.receiver.
            require_keys_eq!(receiver_buy_i.key(), receiver, JamError::InvalidReceiver);
        } else {
            use anchor_lang::solana_program::program_pack::Pack;
            let data = receiver_buy_i.try_borrow_data()?;
            require!(
                data.len() >= anchor_spl::token::spl_token::state::Account::LEN,
                JamError::InvalidReceiver
            );
            let ta = anchor_spl::token::spl_token::state::Account::unpack(
                &data[..anchor_spl::token::spl_token::state::Account::LEN],
            ).map_err(|_| error!(JamError::InvalidReceiver))?;
            require_keys_eq!(ta.owner, receiver, JamError::InvalidReceiver);
        }

        let received_i = balance_of(
            &order.buy_tokens[i],
            ctx.accounts.custody_authority.to_account_info(),
            Some(custody_buy_i.to_account_info()),
        );
        require!(received_i >= order.buy_amounts[i], JamError::InsufficientOutput);
        buy_amounts_filled.push(received_i);

        transfer_from_custody(
            &order.buy_tokens[i],
            received_i,
            ctx.accounts.custody_authority.to_account_info(),
            Some(custody_buy_i.to_account_info()),
            receiver_buy_i.to_account_info(),
            // Same fix as pair 0: pass Some(receiver_buy_i) so SPL transfers don't
            // unwrap None. For native-SOL additional pairs (rare) the native-SOL
            // branch ignores this and uses recipient directly.
            Some(receiver_buy_i.to_account_info()),
            buy_mint_i.to_account_info(),
            buy_prog_i.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ca_seeds,
        )?;
    }

    // ── Drain sell tokens: custody → solver ─────────────────────────────────────
    // Sell tokens transferred to solver AFTER interactions. Doing this after
    // buy verification ensures the solver cannot receive sell tokens without having delivered the buy tokens.
    // For SPL/wSOL sell (custody_sell_ata.is_some()), solver_sell_ata must also
    // be Some — otherwise transfer_from_custody panics on recipient_ata.unwrap().
    require!(
        ctx.accounts.custody_sell_ata.is_none() || ctx.accounts.solver_sell_ata.is_some(),
        JamError::AccountNotFound
    );
    // Pair 0
    transfer_from_custody(
        &order.sell_tokens[0],
        order.sell_amounts[0],
        ctx.accounts.custody_authority.to_account_info(),
        ctx.accounts.custody_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.solver.to_account_info(),
        ctx.accounts.solver_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.sell_mint.to_account_info(),
        ctx.accounts.sell_token_program.to_account_info(),
        ctx.accounts.system_program.to_account_info(),
        ca_seeds,
    )?;
    // Additional sell pairs
    for i in 1..s {
        let base = (i - 1) * 5;
        let custody_sell_i = &ctx.remaining_accounts[base + 1];
        let sell_mint_i    = &ctx.remaining_accounts[base + 2];
        let sell_prog_i    = &ctx.remaining_accounts[base + 3];
        let solver_sell_i  = &ctx.remaining_accounts[base + 4];
        transfer_from_custody(
            &order.sell_tokens[i],
            order.sell_amounts[i],
            ctx.accounts.custody_authority.to_account_info(),
            Some(custody_sell_i.to_account_info()),
            ctx.accounts.solver.to_account_info(),
            Some(solver_sell_i.to_account_info()),
            sell_mint_i.to_account_info(),
            sell_prog_i.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ca_seeds,
        )?;
    }

    emit!(BebopJamOrderFilled {
        nonce: order.nonce,
        taker: order.taker,
        sell_tokens: order.sell_tokens,
        buy_tokens: order.buy_tokens,
        sell_amounts: order.sell_amounts,
        buy_amounts: buy_amounts_filled,
    });

    Ok(())
}

pub fn handle_settle_internal<'c: 'info, 'info>(
    ctx: Context<'_, '_, 'c, 'info, SettleInternal<'info>>,
    order: SolanaJamOrder,
    filled_amounts: Vec<u64>,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(now < order.expiry, JamError::OrderExpired);
    require!(order.nonce != 0, JamError::ZeroNonce);
    let receiver = order.effective_receiver(); // None → taker
    require!(!order.hooks_enabled, JamError::HooksNotSupported);
    require_keys_eq!(ctx.accounts.taker.key(), order.taker, JamError::InvalidTaker);
    require!(!order.sell_amounts.is_empty(), JamError::ZeroAmount);
    require!(!order.buy_amounts.is_empty(), JamError::ZeroAmount);
    require!(order.sell_tokens.len() == order.sell_amounts.len(), JamError::LengthMismatch);
    require!(order.buy_tokens.len() == order.buy_amounts.len(), JamError::LengthMismatch);
    require_keys_eq!(ctx.accounts.sell_mint.key(), order.sell_tokens[0], JamError::MintMismatch);
    require_keys_eq!(ctx.accounts.buy_mint.key(),  order.buy_tokens[0],  JamError::MintMismatch);
    require!(order.sell_amounts[0] > 0 && order.buy_amounts[0] > 0, JamError::ZeroAmount);
    require!(
        order.sell_tokens[0] != order.buy_tokens[0]
            || ctx.accounts.solver_sell_ata.is_none() != ctx.accounts.solver_buy_ata.is_none(),
        JamError::MintMismatch
    );

    if order.exclusivity_deadline.map_or(false, |d| now <= d) {
        if let Some(exec) = order.executor {
            require_keys_eq!(ctx.accounts.solver.key(), exec, JamError::ExclusivityViolation);
        }
    }

    let nr = &mut ctx.accounts.nonce_record;
    nr.expiry = order.expiry; nr.bump = ctx.bumps.nonce_record;

    // Enforce receiver_buy_ata.owner == order.receiver (see Settle for full rationale).
    if let Some(ata) = &ctx.accounts.receiver_buy_ata {
        require_keys_eq!(ata.owner, receiver, JamError::InvalidReceiver);
    }

    let s = order.sell_tokens.len();
    let b = order.buy_tokens.len();

    let filled_0 = if filled_amounts.is_empty() {
        order.buy_amounts[0]
    } else {
        require!(filled_amounts.len() == b, JamError::LengthMismatch);
        require!(filled_amounts[0] >= order.buy_amounts[0], JamError::InsufficientOutput);
        filled_amounts[0]
    };

    // ── Sell pair 0: taker → solver ────────────────────────────────────────
    transfer_spl_or_sol(
        &order.sell_tokens[0],
        order.sell_amounts[0],
        ctx.accounts.taker.to_account_info(),
        ctx.accounts.taker_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.solver.to_account_info(),
        ctx.accounts.solver_sell_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.sell_mint.to_account_info(),
        ctx.accounts.sell_token_program.to_account_info(),
        ctx.accounts.system_program.to_account_info(),
        None,
    )?;

    // ── Additional sell pairs: taker → solver via remaining_accounts ────────
    // remaining_accounts layout: [(S-1) groups of 4: taker_sell_i, solver_sell_i, sell_mint_i, sell_prog_i]
    //                             then [(B-1) groups of 4: solver_buy_i, receiver_buy_i, buy_mint_i, buy_prog_i]
    for i in 1..s {
        let base = (i - 1) * 4;
        require!(base + 3 < ctx.remaining_accounts.len(), JamError::AccountNotFound);
        let taker_sell_i  = &ctx.remaining_accounts[base];
        let solver_sell_i = &ctx.remaining_accounts[base + 1];
        let sell_mint_i   = &ctx.remaining_accounts[base + 2];
        let sell_prog_i   = &ctx.remaining_accounts[base + 3];
        require!(
            sell_prog_i.key() == anchor_spl::token::ID
                || sell_prog_i.key() == anchor_spl::token_2022::ID,
            JamError::InteractionTargetProtected
        );
        transfer_spl_or_sol(
            &order.sell_tokens[i],
            order.sell_amounts[i],
            ctx.accounts.taker.to_account_info(),
            Some(taker_sell_i.to_account_info()),
            ctx.accounts.solver.to_account_info(),
            Some(solver_sell_i.to_account_info()),
            sell_mint_i.to_account_info(),
            sell_prog_i.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            None,
        )?;
    }

    let buy_base = (s - 1) * 4;

    // ── Buy pair 0: solver → receiver ─────────────────────────────────────
    let (net_buy_0, partner_fee_0) = decode_partner_fee(filled_0, order.partner_fee_bps)?;
    // Verify receiver gets at least buy_amounts[0] after fees.
    require!(net_buy_0 >= order.buy_amounts[0], JamError::InsufficientOutput);

    let buy_recv_wallet_0 = if receiver == ctx.accounts.taker.key() {
        ctx.accounts.taker.to_account_info()
    } else if receiver == ctx.accounts.solver.key() {
        ctx.accounts.solver.to_account_info()
    } else {
        ctx.remaining_accounts.iter()
            .find(|a| a.key() == receiver)
            .cloned()
            .ok_or(error!(JamError::AccountNotFound))?
    };

    transfer_spl_or_sol(
        &order.buy_tokens[0],
        net_buy_0,
        ctx.accounts.solver.to_account_info(),
        ctx.accounts.solver_buy_ata.as_ref().map(|a| a.to_account_info()),
        buy_recv_wallet_0,
        ctx.accounts.receiver_buy_ata.as_ref().map(|a| a.to_account_info()),
        ctx.accounts.buy_mint.to_account_info(),
        ctx.accounts.buy_token_program.to_account_info(),
        ctx.accounts.system_program.to_account_info(),
        None,
    )?;

    // Partner fee for pair 0.
    if partner_fee_0 > 0 {
        if let Some(pa) = ctx.accounts.partner_account.as_ref() {
            // Same validation as handle_settle.partner_account.
            // ATA presence is the discriminator — matches transfer_spl_or_sol routing.
            if ctx.accounts.solver_buy_ata.is_some() {
                use anchor_lang::solana_program::program_pack::Pack;
                let data = pa.try_borrow_data()?;
                if data.len() >= anchor_spl::token::spl_token::state::Account::LEN {
                    if let Ok(ta) = anchor_spl::token::spl_token::state::Account::unpack(
                        &data[..anchor_spl::token::spl_token::state::Account::LEN]
                    ) {
                        if let Some(pk) = order.partner {
                            require_keys_eq!(ta.owner, pk, JamError::InvalidReceiver);
                        }
                    }
                }
            } else {
                // Native SOL buy: partner_account IS the partner wallet — verify key directly.
                // The original code had no else branch here, allowing a solver to route
                // native-SOL partner fees to any arbitrary account.
                if let Some(pk) = order.partner {
                    require_keys_eq!(pa.key(), pk, JamError::InvalidReceiver);
                }
            }
            transfer_spl_or_sol(
                &order.buy_tokens[0],
                partner_fee_0,
                ctx.accounts.solver.to_account_info(),
                ctx.accounts.solver_buy_ata.as_ref().map(|a| a.to_account_info()),
                pa.to_account_info(),
                Some(pa.to_account_info()),
                ctx.accounts.buy_mint.to_account_info(),
                ctx.accounts.buy_token_program.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                None,
            )?;
        }
    }

    // ── Additional buy pairs via remaining_accounts ────────────────────────
    let mut buy_amounts_filled = vec![net_buy_0];
    for i in 1..b {
        let base = buy_base + (i - 1) * 4;
        require!(base + 3 < ctx.remaining_accounts.len(), JamError::AccountNotFound);
        let solver_buy_i   = &ctx.remaining_accounts[base];
        let receiver_buy_i = &ctx.remaining_accounts[base + 1];
        let buy_mint_i     = &ctx.remaining_accounts[base + 2];
        let buy_prog_i     = &ctx.remaining_accounts[base + 3];
        require!(
            buy_prog_i.key() == anchor_spl::token::ID
                || buy_prog_i.key() == anchor_spl::token_2022::ID,
            JamError::InteractionTargetProtected
        );

        // Verify receiver_buy_i is owned by order.receiver.
        if order.buy_tokens[i] == anchor_spl::token::spl_token::native_mint::ID {
            require_keys_eq!(receiver_buy_i.key(), receiver, JamError::InvalidReceiver);
        } else {
            use anchor_lang::solana_program::program_pack::Pack;
            let data = receiver_buy_i.try_borrow_data()?;
            require!(data.len() >= anchor_spl::token::spl_token::state::Account::LEN, JamError::InvalidReceiver);
            let ta = anchor_spl::token::spl_token::state::Account::unpack(
                &data[..anchor_spl::token::spl_token::state::Account::LEN]
            ).map_err(|_| error!(JamError::InvalidReceiver))?;
            require_keys_eq!(ta.owner, receiver, JamError::InvalidReceiver);
        }

        let filled_i = if filled_amounts.is_empty() {
            order.buy_amounts[i]
        } else {
            require!(filled_amounts[i] >= order.buy_amounts[i], JamError::InsufficientOutput);
            filled_amounts[i]
        };
        let (net_i, _partner_fee_i) = decode_partner_fee(filled_i, order.partner_fee_bps)?;
        require!(net_i >= order.buy_amounts[i], JamError::InsufficientOutput);
        buy_amounts_filled.push(net_i);

        transfer_spl_or_sol(
            &order.buy_tokens[i],
            net_i,
            ctx.accounts.solver.to_account_info(),
            Some(solver_buy_i.to_account_info()),
            receiver_buy_i.to_account_info(),
            Some(receiver_buy_i.to_account_info()),
            buy_mint_i.to_account_info(),
            buy_prog_i.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            None,
        )?;
    }

    emit!(BebopJamOrderFilled {
        nonce: order.nonce, taker: order.taker,
        sell_tokens: order.sell_tokens, buy_tokens: order.buy_tokens,
        sell_amounts: order.sell_amounts, buy_amounts: buy_amounts_filled,
    });

    Ok(())
}

// ─── Token transfer helpers ───────────────────────────────────────────────────

/// Transfer SPL / Token-2022 / native-SOL / wSOL from a sender into the custody PDA.
///
/// Routing: `from_ata.is_none()` with native_mint → raw lamport transfer (native SOL).
/// `from_ata.is_some()` with native_mint → SPL token transfer (wSOL-in-ATA).
/// Any other mint → SPL/T22 token transfer.
/// This mirrors RFQ utils.rs `unwrap_sol`/`sync_native` logic: the ATA Option is the
/// discriminator, not the mint key alone.
#[allow(clippy::too_many_arguments)]
fn transfer_into_custody<'info>(
    mint_key: &Pubkey,
    amount: u64,
    from_wallet: AccountInfo<'info>,
    from_ata: Option<AccountInfo<'info>>,
    custody_authority: AccountInfo<'info>,
    custody_ata: Option<AccountInfo<'info>>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    signer_seeds: Option<&[&[u8]]>,
) -> Result<()> {
    if *mint_key == native_mint::ID && from_ata.is_none() {
        // Native SOL: lamport transfer from_wallet → custody_authority PDA.
        let cpi = anchor_lang::system_program::Transfer {
            from: from_wallet,
            to: custody_authority,
        };
        match signer_seeds {
            Some(s) => anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(system_program, cpi, &[s]), amount,
            ),
            None => anchor_lang::system_program::transfer(
                CpiContext::new(system_program, cpi), amount,
            ),
        }
    } else {
        // SPL / Token-2022 / wSOL (native_mint with ATA = wSOL token account)
        spl_transfer(
            amount, from_ata.unwrap(), custody_ata.unwrap(),
            from_wallet, mint, token_program, signer_seeds,
        )
    }
}

/// Transfer SPL / Token-2022 / native-SOL / wSOL from the custody PDA to a recipient.
/// Routing: `custody_ata.is_none()` with native_mint → lamports (native SOL).
///           `custody_ata.is_some()` with native_mint → wSOL token transfer.
#[allow(clippy::too_many_arguments)]
fn transfer_from_custody<'info>(
    mint_key: &Pubkey,
    amount: u64,
    custody_authority: AccountInfo<'info>,
    custody_ata: Option<AccountInfo<'info>>,
    recipient: AccountInfo<'info>,
    recipient_ata: Option<AccountInfo<'info>>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    ca_seeds: &[&[u8]],
) -> Result<()> {
    if *mint_key == native_mint::ID && custody_ata.is_none() {
        // Native SOL: lamport transfer custody PDA → recipient wallet.
        let cpi = anchor_lang::system_program::Transfer {
            from: custody_authority,
            to: recipient,
        };
        anchor_lang::system_program::transfer(
            CpiContext::new_with_signer(system_program, cpi, &[ca_seeds]), amount,
        )
    } else {
        // SPL / Token-2022 / wSOL
        spl_transfer(
            amount, custody_ata.unwrap(), recipient_ata.unwrap(),
            custody_authority, mint, token_program, Some(ca_seeds),
        )
    }
}

/// Generic SPL/Token-2022 transfer. Replicates RFQ utils.rs exactly:
/// Token-2022: uses transfer_checked with decimals; rejects non-zero transfer fee.
/// SPL: uses token::transfer.
fn spl_transfer<'info>(
    amount: u64,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    signer_seeds: Option<&[&[u8]]>,
) -> Result<()> {
    let decimals_opt = if token_program.key.eq(&spl_token_2022::ID) {
        let mint_data = mint.try_borrow_data()?;
        let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
        if let Ok(fee_config) = state.get_extension::<TransferFeeConfig>() {
            require!(
                fee_config.get_epoch_fee(Clock::get()?.epoch).transfer_fee_basis_points
                    == PodU16([0; 2]),
                JamError::Token2022FeeNotSupported,
            );
        }
        // Reject mints with a live permanent delegate. A permanent delegate can
        // transfer or burn any amount from any token account — including
        // custody_sell_ata — between transfer_into_custody and run_interactions,
        // draining funds that should belong to the solver or the protocol.
        if let Ok(pd) = state.get_extension::<PermanentDelegate>() {
            require!(
                Option::<anchor_lang::prelude::Pubkey>::from(pd.delegate).is_none(),
                JamError::Token2022FeeNotSupported
            );
        }
        // Reject mints with a transfer hook. JAM does not append the extra accounts
        // required by hook programs, so any hook-bearing mint causes AccountNotFound
        // inside transfer_checked. More critically, a hook program can CPI back into
        // JAM's custody accounts if it can derive their addresses — re-entrancy risk.
        require!(
            state.get_extension::<TransferHook>().is_err(),
            JamError::Token2022FeeNotSupported
        );
        // Reject ConfidentialTransfer extension. Tokens deposited to a confidential
        // balance are invisible to balance_of (which reads the public SPL base amount).
        // A solver could fund custody via a confidential transfer, causing
        // received_0 = 0 and the InsufficientOutput check to fail — no theft is
        // possible but the order becomes unsettleable with a confusing error.
        // Reject at the token level for a deterministic failure point.
        if let Ok(_) = state.get_extension::<ConfidentialTransferMint>() {
            return err!(JamError::Token2022FeeNotSupported);
        }
        Some(state.base.decimals)
    } else {
        None
    };

    match (decimals_opt, signer_seeds) {
        (Some(decimals), Some(seeds)) => token_interface::transfer_checked(
            CpiContext::new_with_signer(token_program,
                token_interface::TransferChecked { from, mint, to, authority }, &[seeds]),
            amount, decimals,
        ),
        (Some(decimals), None) => token_interface::transfer_checked(
            CpiContext::new(token_program,
                token_interface::TransferChecked { from, mint, to, authority }),
            amount, decimals,
        ),
        (None, Some(seeds)) => token::transfer(
            CpiContext::new_with_signer(token_program,
                token::Transfer { from, to, authority }, &[seeds]),
            amount,
        ),
        (None, None) => token::transfer(
            CpiContext::new(token_program, token::Transfer { from, to, authority }),
            amount,
        ),
    }
}

/// Generic from-to transfer (settle_internal helper, same spl_or_sol logic).
#[allow(clippy::too_many_arguments)]
fn transfer_spl_or_sol<'info>(
    mint_key: &Pubkey,
    amount: u64,
    from_wallet: AccountInfo<'info>,
    from_ata: Option<AccountInfo<'info>>,
    to_wallet: AccountInfo<'info>,
    to_ata: Option<AccountInfo<'info>>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    signer_seeds: Option<&[&[u8]]>,
) -> Result<()> {
    if *mint_key == native_mint::ID && from_ata.is_none() {
        // Native SOL lamport transfer. to_ata.unwrap_or(to_wallet) handles the
        // case where the recipient may be a SOL wallet or a wSOL ATA.
        let to = to_ata.unwrap_or(to_wallet);
        let cpi = anchor_lang::system_program::Transfer { from: from_wallet, to };
        match signer_seeds {
            Some(s) => anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(system_program, cpi, &[s]), amount),
            None => anchor_lang::system_program::transfer(
                CpiContext::new(system_program, cpi), amount),
        }
    } else {
        // SPL / Token-2022 / wSOL (native_mint with from_ata present)
        spl_transfer(amount, from_ata.unwrap(), to_ata.unwrap(),
                     from_wallet, mint, token_program, signer_seeds)
    }
}

/// Read current balance: token amount for SPL/Token-2022, lamports for native SOL.
///
/// Uses spl_token::state::Account::unpack (Pack trait) rather than Anchor's
/// TokenAccount::try_deserialize. Anchor's deserializer applies a discriminator
/// check that fails for Token-2022 accounts even though their base-state layout
/// (the first 165 bytes) is identical to SPL. Direct Pack::unpack reads only the
/// base state, so it works for both program IDs.
/// Read balance. Routing:
///   native_mint + no token_account → native SOL lamports
///   native_mint + token_account    → wSOL token amount (bytes 64-72)
///   any other mint                 → SPL/T22 token amount (bytes 64-72)
fn balance_of<'info>(
    mint_key: &Pubkey,
    native_account: AccountInfo<'info>,
    token_account: Option<AccountInfo<'info>>,
) -> u64 {
    if *mint_key == native_mint::ID && token_account.is_none() {
        native_account.lamports()
    } else {
        match token_account {
            Some(acct) => {
                // Read amount directly at offset 64 (SPL/T22 base layout:
                // [mint:32][owner:32][amount:u64 LE]). Same approach as
                // transfer_from_vaults in state.rs — avoids full Pack::unpack
                // (165-byte validation) on this hot path.
                if let Ok(data) = acct.try_borrow_data() {
                    if data.len() >= 72 {
                        return u64::from_le_bytes(data[64..72].try_into().unwrap());
                    }
                }
                0
            }
            None => 0,
        }
    }
}

// ─── run_interactions ─────────────────────────────────────────────────────────
//
// A11 — Soft limit: Solana enforces 200k compute units per tx by default
// (up to 1.4M with ComputeBudgetInstruction). Each CPI costs ~5k–20k CU.
// Practical upper bound is ~20 interactions before hitting compute limits at
// default budget. Solvers must request a higher CU budget for complex flows.
// No on-chain count check is enforced — a solver who exceeds budget fails only
// their own tx. Off-chain API should document: max ~20 interactions per settle,
// request ComputeBudget = 400_000 + 15_000 * interactions.len().
//
// Security model:
//   1. custody_authority is a hard-blocked interaction target — prevents any
//      interaction from draining custody directly via CPI.
//   2. crate::ID (JAM itself) is blocked — prevents re-entrancy.
//   3. system_program, spl_token, spl_token_2022 are blocked as direct targets.
//      invoke() propagates taker.is_signer=true through CPI chains; if a solver
//      could call system_program::transfer(from=taker) or spl_token::transfer
//      (authority=taker) directly, they could drain the taker's SOL or SPL
//      balances. Blocking the top-level target is sufficient: DEX programs that
//      internally CPI into spl_token are fine — JAM only checks the outermost
//      interaction program_id, not nested CPIs.
//   4. JAM authority PDA signing is opt-in per interaction (use_jam_authority).
//
//      use_jam_authority: true  →  invoke_signed appends jam_authority and
//        signs with JAM_AUTHORITY_SEED. The callee sees jam_authority.is_signer
//        == true and can gate on key() == its stored bebop_authority pubkey.
//        Equivalent to require(msg.sender == JAM): PDA cannot be forged,
//        any compile-time binding: JAM knows nothing about the callee; the
//        callee registers the expected pubkey once (FlashLoanProvider: update_config with
//        set_bebop_authority = jam_authority.key()).
//
//        Flash loan example — solver includes in interactions[]:
//          { program: quid::ID, accounts: [flash_authority, bank, config,
//            sol_pool, ix_sysvar, system_program, borrower],
//            data: flash_borrow_discriminator ++ lamports.to_le_bytes(),
//            use_jam_authority: true, result: true }
//          ... arb interactions ...
//          { program: quid::ID, accounts: [flash_authority, repayer, bank,
//            sol_risk, sol_pool, config, system_program],
//            data: flash_repay_discriminator,
//            use_jam_authority: true, result: true }
//        JAM dispatches each with invoke_signed; FlashLoanProvider's signer + address
//        constraint on flash_authority passes for both instructions.
//        FlashLoanProvider's instructions sysvar lookahead at borrow time confirms
//        flash_repay is present later in the same tx — atomicity at the
//        and sysvar lookahead confirms repay before funds leave.
//
//      use_jam_authority: false →  plain invoke. DEX swaps, token transfers,
//        oracle reads, bebop_rfq::Swap — any interaction not requiring JAM's
//        identity. The overwhelming majority of interactions fall here.
//
//      Security: unconditional invoke_signed made JAM an unbounded signer-
//        for-hire: any program gating on jam_authority.is_signer was reachable
//        by any solver with a valid order. Opt-in via use_jam_authority confines
//        authority delegation to interactions that explicitly request it,
//        eliminating that surface while preserving the flash loan path.

fn run_interactions<'info>(
    remaining: &'info [AccountInfo<'info>],
    interactions: &[SolanaInteraction],
    protected: Pubkey,       // custody_authority — cannot be a target
    jam_authority: &AccountInfo<'info>,
    authority_seeds: &[&[u8]],
) -> Result<()> {
    // Pre-index remaining_accounts once: O(r log r) sort → O(log r) per lookup.
    // Without this: O(k × a × r) across all interactions; with it: O(r log r + k·a·log r).
    // For 5 interactions × 5 accounts × 20 remaining: cuts ~500 comparisons to ~200.
    // O(1) account lookup: program_index and account_index are direct indices into
    // remaining_accounts. Eliminates ra_index sort + binary search.
    // infos allocated once outside the loop and cleared each iteration — reduces
    // N heap allocations to 1 (one per settle call, not one per interaction).
    let mut infos: Vec<AccountInfo<'info>> = Vec::with_capacity(16);

    for ix in interactions {
        let prog_idx = ix.program_index as usize; // u8 → usize, always < remaining.len()
        require!(prog_idx < remaining.len(), JamError::AccountNotFound);
        let prog = &remaining[prog_idx];
        require!(prog.key() != protected, JamError::InteractionTargetProtected);
        require!(prog.key() != crate::ID, JamError::InteractionTargetProtected);
        // Block signer-capable system programs as interaction targets.
        // invoke() propagates taker.is_signer=true through CPI, so an
        // interaction targeting system_program could drain taker SOL, and
        // one targeting spl_token/spl_token_2022 could drain taker ATAs.
        // No legitimate solver interaction needs to call these directly —
        // SOL/SPL transfers are handled by JAM's own transfer helpers,
        // and DEX swaps that need wrapping go through wSOL ATAs.
        let prog_key = prog.key();
        require!(
            prog_key != anchor_lang::solana_program::system_program::id()
                && prog_key != anchor_spl::token::ID
                && prog_key != anchor_spl::token_2022::ID,
            JamError::InteractionTargetProtected
        );

        let mut metas = Vec::with_capacity(ix.accounts.len());
        infos.clear();

        for ia in &ix.accounts {
            let acc_idx = ia.account_index as usize;  // u8 → usize
            require!(acc_idx < remaining.len(), JamError::AccountNotFound);
            let acct = &remaining[acc_idx];
            metas.push(anchor_lang::solana_program::instruction::AccountMeta {
                pubkey: acct.key(),
                is_signer:   ia.is_signer(),
                is_writable: ia.is_writable(),
            });
            infos.push(acct.clone());
        }
        infos.push(prog.clone());

        let instruction = anchor_lang::solana_program::instruction::Instruction {
            program_id: prog.key(), accounts: metas, data: ix.data.clone(),
        };

        // use_jam_authority: opt-in signer delegation. Unconditional signing made JAM
        // an unbounded signer-for-hire — see admin_timelock.rs for the threat model.
        let result = if ix.use_jam_authority {
            infos.push(jam_authority.clone());
            invoke_signed(&instruction, &infos, &[authority_seeds])
        } else {
            invoke(&instruction, &infos)
        };
        if ix.result {
            result.map_err(|_| JamError::InteractionFailed)?;
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Compute (net_amount, fee) for a given gross amount and fee in bps.
/// partner_fee_bps is stored directly in SolanaJamOrder as a u16 (no bit-packing).
fn decode_partner_fee(amount: u64, partner_fee_bps: u16) -> Result<(u64, u64)> {
    let bps = partner_fee_bps as u64;
    if bps == 0 { return Ok((amount, 0)); }
    require!(bps <= 10_000, JamError::InvalidPartnerFee);
    let fee = (amount as u128 * bps as u128 / 10_000) as u64;
    Ok((amount.saturating_sub(fee), fee))
}

// ─── close_nonce_record ───────────────────────────────────────────────────────
// Permissionless rent-reclaim once the nonce record is past its replay window.
// Safety: closing is safe only after order.expiry — any replay attempt before
// that would fail the expiry check in handle_settle/handle_settle_internal first.
// After expiry the nonce record is inert; no second settlement can occur because
// the order itself is expired. Refund goes to the caller to incentivise cleanup.

// taker and nonce are passed as instruction params (not stored in NonceRecord)
// so the Anchor seeds constraint can verify the PDA without redundant on-account storage.
#[derive(AnchorSerialize, AnchorDeserialize)]
#[derive(Clone)]
pub struct CloseNonceRecordParams {
    pub taker: Pubkey,
    pub nonce: u64,
}

#[derive(Accounts)]
#[instruction(params: CloseNonceRecordParams)]
pub struct CloseNonceRecord<'info> {
    /// Rent refund destination — permissionless, incentivises keeper bots.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [NONCE_SEED, params.taker.as_ref(), &params.nonce.to_le_bytes()],
        bump = record.bump,
        close = payer,
    )]
    pub record: Account<'info, NonceRecord>,

    pub system_program: Program<'info, System>,
}

pub fn handle_close_nonce_record(
    ctx: Context<CloseNonceRecord>,
    _params: CloseNonceRecordParams,
) -> Result<()> {
    require!(
        Clock::get()?.unix_timestamp > ctx.accounts.record.expiry,
        JamError::RecordNotExpired // still within replay-protection window
    );
    Ok(())
}

// ─── Event ────────────────────────────────────────────────────────────────────

#[event]
pub struct BebopJamOrderFilled {
    pub nonce: u64,
    pub taker: Pubkey,
    pub sell_tokens: Vec<Pubkey>,
    pub buy_tokens: Vec<Pubkey>,
    pub sell_amounts: Vec<u64>,
    pub buy_amounts: Vec<u64>,
}

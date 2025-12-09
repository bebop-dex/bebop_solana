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


pub fn handle_swap<'c: 'info, 'info>(
    ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
    input_amount: u64,
    output_amounts: Vec<AmountWithExpiry>,
    event_id: u64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp as u64;
    let mut output_amount: u64 = 0;
    for (i, amount_with_expiry) in output_amounts.iter().enumerate() {
        require!(
            i == 0 || 
            (amount_with_expiry.amount <= output_amounts[i - 1].amount && amount_with_expiry.expiry > output_amounts[i - 1].expiry),
            BebopError::InvalidOutputAmount
        );
        if amount_with_expiry.expiry >= now {
            output_amount = amount_with_expiry.amount;
            break;
        }
    }
    require!(output_amount > 0, BebopError::OrderExpired);
    let mut bump: u8 = 0;
    let filled_taker_amount: u64;
    if !&ctx.accounts.taker.is_signer{
        let (expected_pda_address, _bump) = Pubkey::find_program_address(
            &[SHARED_ACCOUNT],
            &crate::ID,
        );
        bump = _bump;
        require_keys_eq!(ctx.accounts.taker.key(), expected_pda_address, BebopError::WrongSharedAccountAddress);
        filled_taker_amount = match &ctx.accounts.taker_input_mint_token_account {
            Some(token_acc) => token_acc.amount,
            None => ctx.accounts.taker.lamports(),
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

    let filled_maker_amount: u64 = if filled_taker_amount < input_amount {
        ((output_amount as u128 * filled_taker_amount as u128) / input_amount as u128) as u64
    } else {
        // max maker_amount is output_amount, even if filled_taker_amount > input_amount
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
    /// CHECK: taker isn't Signer when it's shared-pda account 
    #[account(mut)]
    pub taker: UncheckedAccount<'info>,
    #[account(mut)]
    pub maker: Signer<'info>,
    /// CHECK: can be same as taker address or any other address 
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


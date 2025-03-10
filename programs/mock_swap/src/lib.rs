use anchor_lang::prelude::*;
use anchor_spl::{token::Token, token_2022::Token2022, token_interface::{Mint, TokenAccount, TokenInterface}};
mod utils;


#[cfg(not(feature = "production"))]
declare_id!("AutobNFLMzX1rFCDgwWpwr3ztG5c1oDbSrGq7Jj2LgE");

pub const POOL_ACCOUNT: &[u8] = b"pool-account";

#[program]
pub mod mock_swap {
    use crate::utils::transfer;

    use super::*;

    pub fn swap_on_raydium_cpmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockRaydiumCPMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
       let mut bump: u8 = 0;
       let (expected_pda_address, _bump) = Pubkey::find_program_address(
            &[POOL_ACCOUNT],
            &crate::ID,
        );
        bump = _bump;
        require_keys_eq!(ctx.accounts.authority.key(), expected_pda_address);
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);

        // from user to vault
        transfer(
            ctx.accounts.input_token_program.to_account_info(),
            ctx.accounts.input_token_account.to_account_info(),
            ctx.accounts.input_vault.to_account_info(),
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.input_token_mint.to_account_info(),
            amount_in,
            None
        )?;

        // from vault to receiver
        transfer(
            ctx.accounts.output_token_program.to_account_info(),
            ctx.accounts.output_vault.to_account_info(), 
            ctx.accounts.output_token_account.to_account_info(),
            ctx.accounts.authority.to_account_info(),
            ctx.accounts.output_token_mint.to_account_info(),
            minimum_amount_out,
            pda_seeds
        )?;
       Ok(())
    }

    pub fn swap_on_raydium_clmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockRaydiumCLMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
       let mut bump: u8 = 0;
       let (_, _bump) = Pubkey::find_program_address(
            &[POOL_ACCOUNT],
            &crate::ID,
        );
        bump = _bump;
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);

        // from user to vault
        transfer(
            ctx.accounts.input_token_program.to_account_info(),
            ctx.accounts.input_token_account.to_account_info(),
            ctx.accounts.input_vault.to_account_info(),
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.input_vault_mint.to_account_info(),
            amount_in,
            None
        )?;

        // from vault to receiver
        transfer(
            ctx.accounts.output_token_program.to_account_info(),
            ctx.accounts.output_vault.to_account_info(), 
            ctx.accounts.output_token_account.to_account_info(),
            ctx.accounts.pool_state.to_account_info(),
            ctx.accounts.output_vault_mint.to_account_info(),
            minimum_amount_out,
            pda_seeds
        )?;
       Ok(())
    }

    pub fn swap_on_meteora_dlmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockMeteoraDLMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
       let mut bump: u8 = 0;
       let (expected_pda_address, _bump) = Pubkey::find_program_address(
            &[POOL_ACCOUNT],
            &crate::ID,
        );
        bump = _bump;
        require_keys_eq!(ctx.accounts.lb_pair.key(), expected_pda_address);
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);

        // from user to vault
        transfer(
            ctx.accounts.token_x_program.to_account_info(),
            ctx.accounts.user_token_in.to_account_info(),
            ctx.accounts.reserve_x.to_account_info(),
            ctx.accounts.user.to_account_info(),
            ctx.accounts.token_x_mint.to_account_info(),
            amount_in,
            None
        )?;

        // from vault to receiver
        transfer(
            ctx.accounts.token_y_program.to_account_info(),
            ctx.accounts.reserve_y.to_account_info(), 
            ctx.accounts.user_token_out.to_account_info(),
            ctx.accounts.lb_pair.to_account_info(),
            ctx.accounts.token_y_mint.to_account_info(),
            minimum_amount_out,
            pda_seeds
        )?;
       Ok(())
    }
}


#[derive(Accounts)]
pub struct MockRaydiumCPMM<'info> {
    pub payer: Signer<'info>,
    #[account(
        seeds = [
          POOL_ACCOUNT,
        ],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,
    pub amm_config: UncheckedAccount<'info>, // Box<Account<'info, AmmConfig>>,
    #[account(mut)]
    pub pool_state: UncheckedAccount<'info>, // AccountLoader<'info, PoolState>,
    /// The user token account for input token
    #[account(mut)]
    pub input_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The user token account for output token
    #[account(mut)]
    pub output_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// SPL program for input token transfers
    pub input_token_program: Interface<'info, TokenInterface>,
    /// SPL program for output token transfers
    pub output_token_program: Interface<'info, TokenInterface>,
    /// The mint of input token
    pub input_token_mint: Box<InterfaceAccount<'info, Mint>>,
    /// The mint of output token
    pub output_token_mint: Box<InterfaceAccount<'info, Mint>>,
    pub observation_state: UncheckedAccount<'info>,//AccountLoader<'info, ObservationState>,
}

#[derive(Accounts)]
pub struct MockRaydiumCLMM<'info> {
    pub payer: Signer<'info>,
    pub amm_config: UncheckedAccount<'info>, // Box<Account<'info, AmmConfig>>,
    #[account(
        seeds = [
          POOL_ACCOUNT,
        ],
        bump,
    )]
    pub pool_state: UncheckedAccount<'info>, // AccountLoader<'info, PoolState>,
    /// The user token account for input token
    #[account(mut)]
    pub input_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The user token account for output token
    #[account(mut)]
    pub output_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub observation_state: UncheckedAccount<'info>,//AccountLoader<'info, ObservationState>,
    pub token_program: Program<'info, Token>,
    pub token_program_2022: Program<'info, Token2022>,
    pub memo_program:  UncheckedAccount<'info>, //Program<'info, Memo>,
    /// SPL program for input token transfers
    pub input_token_program: Interface<'info, TokenInterface>,
    /// SPL program for output token transfers
    pub output_token_program: Interface<'info, TokenInterface>,
    /// The mint of input token
    pub input_vault_mint: Box<InterfaceAccount<'info, Mint>>,
    /// The mint of output token
    pub output_vault_mint: Box<InterfaceAccount<'info, Mint>>,
    // remaining accounts
    // tickarray_bitmap_extension: must add account if need regardless the sequence
    // tick_array_account_1
    // tick_array_account_2
    // tick_array_account_...
}

#[derive(Accounts)]
pub struct MockMeteoraDLMM<'info> {
    #[account(
        seeds = [
          POOL_ACCOUNT,
        ],
        bump,
    )]
    pub lb_pair: UncheckedAccount<'info>, // pub lb_pair: AccountLoader<'info, LbPair>,
    pub bin_array_bitmap_extension: Option<UncheckedAccount<'info>>, //Option<AccountLoader<'info, BinArrayBitmapExtension>>,

    #[account(mut)]
    pub reserve_x: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub reserve_y: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub user_token_in: Box<InterfaceAccount<'info, TokenAccount>>,
    pub user_token_out: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_x_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_y_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub oracle: UncheckedAccount<'info>, //AccountLoader<'info, Oracle>,

    #[account(mut)]
    pub host_fee_in: Option<UncheckedAccount<'info>>, //Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    pub user: Signer<'info>,
    pub token_x_program: Interface<'info, TokenInterface>,
    pub token_y_program: Interface<'info, TokenInterface>,
}

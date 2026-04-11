use anchor_lang::prelude::*;
use anchor_spl::{token::Token, token_2022::Token2022, token_interface::{Mint, TokenAccount, TokenInterface}};
mod utils;

pub const POOL_ACCOUNT: &[u8] = b"pool-account";

declare_id!("CKakx79upD27ZpcDdn9JZE27zXXF1CRDYNsBWp4tFqHo");

#[program]
pub mod mock_swap {
    use crate::utils::transfer;
    use super::*;

    pub fn swap_on_raydium_cpmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockRaydiumCPMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
        // Anchor already verified authority via seeds = [POOL_ACCOUNT], bump.
        // ctx.bumps.authority is the canonical bump — no find_program_address needed.
        let bump = ctx.bumps.authority;
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);
        transfer(
            ctx.accounts.input_token_program.to_account_info(),
            ctx.accounts.input_token_account.to_account_info(),
            ctx.accounts.input_vault.to_account_info(),
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.input_token_mint.to_account_info(),
            amount_in, None
        )?;
        transfer(
            ctx.accounts.output_token_program.to_account_info(),
            ctx.accounts.output_vault.to_account_info(),
            ctx.accounts.output_token_account.to_account_info(),
            ctx.accounts.authority.to_account_info(),
            ctx.accounts.output_token_mint.to_account_info(),
            minimum_amount_out, pda_seeds
        )?;
        Ok(())
    }

    pub fn swap_on_raydium_clmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockRaydiumCLMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
        // pool_state has seeds = [POOL_ACCOUNT], bump constraint — bump already in ctx.bumps.
        let bump = ctx.bumps.pool_state;
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);
        transfer(
            ctx.accounts.input_token_program.to_account_info(),
            ctx.accounts.input_token_account.to_account_info(),
            ctx.accounts.input_vault.to_account_info(),
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.input_vault_mint.to_account_info(),
            amount_in, None
        )?;
        transfer(
            ctx.accounts.output_token_program.to_account_info(),
            ctx.accounts.output_vault.to_account_info(),
            ctx.accounts.output_token_account.to_account_info(),
            ctx.accounts.pool_state.to_account_info(),
            ctx.accounts.output_vault_mint.to_account_info(),
            minimum_amount_out, pda_seeds
        )?;
        Ok(())
    }

    pub fn swap_on_meteora_dlmm<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, MockMeteoraDLMM<'info>>,
        amount_in: u64,
        minimum_amount_out: u64
    ) -> Result<()> {
        // lb_pair has seeds = [POOL_ACCOUNT], bump constraint — bump already in ctx.bumps.
        let bump = ctx.bumps.lb_pair;
        let binding: [&[&[u8]]; 1] = [&[POOL_ACCOUNT, &[bump]]];
        let pda_seeds: Option<&[&[&[u8]]]> = Some(&binding);
        transfer(
            ctx.accounts.token_x_program.to_account_info(),
            ctx.accounts.user_token_in.to_account_info(),
            ctx.accounts.reserve_x.to_account_info(),
            ctx.accounts.user.to_account_info(),
            ctx.accounts.token_x_mint.to_account_info(),
            amount_in, None
        )?;
        transfer(
            ctx.accounts.token_y_program.to_account_info(),
            ctx.accounts.reserve_y.to_account_info(),
            ctx.accounts.user_token_out.to_account_info(),
            ctx.accounts.lb_pair.to_account_info(),
            ctx.accounts.token_y_mint.to_account_info(),
            minimum_amount_out, pda_seeds
        )?;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct MockRaydiumCPMM<'info> {
    pub payer: Signer<'info>,
    /// CHECK: PDA used as pool authority, verified by seeds constraint
    #[account(seeds = [POOL_ACCOUNT], bump)]
    pub authority: UncheckedAccount<'info>,
    /// CHECK: AMM config account, not validated in mock
    pub amm_config: UncheckedAccount<'info>,
    /// CHECK: Pool state account, not validated in mock
    #[account(mut)]
    pub pool_state: UncheckedAccount<'info>,
    #[account(mut)]
    pub input_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub input_token_program: Interface<'info, TokenInterface>,
    pub output_token_program: Interface<'info, TokenInterface>,
    pub input_token_mint: Box<InterfaceAccount<'info, Mint>>,
    pub output_token_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Observation state account, not validated in mock
    pub observation_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct MockRaydiumCLMM<'info> {
    pub payer: Signer<'info>,
    /// CHECK: AMM config account, not validated in mock
    pub amm_config: UncheckedAccount<'info>,
    /// CHECK: Pool state PDA, verified by seeds constraint
    #[account(seeds = [POOL_ACCOUNT], bump)]
    pub pool_state: UncheckedAccount<'info>,
    #[account(mut)]
    pub input_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Observation state account, not validated in mock
    pub observation_state: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub token_program_2022: Program<'info, Token2022>,
    /// CHECK: Memo program, validated by known program ID in production
    pub memo_program: UncheckedAccount<'info>,
    pub input_token_program: Interface<'info, TokenInterface>,
    pub output_token_program: Interface<'info, TokenInterface>,
    pub input_vault_mint: Box<InterfaceAccount<'info, Mint>>,
    pub output_vault_mint: Box<InterfaceAccount<'info, Mint>>,
    // remaining accounts: tick_array_account_1, tick_array_account_2, ...
}

#[derive(Accounts)]
pub struct MockMeteoraDLMM<'info> {
    /// CHECK: LB pair PDA, verified by seeds constraint
    #[account(seeds = [POOL_ACCOUNT], bump)]
    pub lb_pair: UncheckedAccount<'info>,
    /// CHECK: Bin array bitmap extension, optional account not validated in mock
    pub bin_array_bitmap_extension: Option<UncheckedAccount<'info>>,
    #[account(mut)]
    pub reserve_x: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub reserve_y: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub user_token_in: Box<InterfaceAccount<'info, TokenAccount>>,
    pub user_token_out: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_x_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_y_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Oracle account, not validated in mock
    #[account(mut)]
    pub oracle: UncheckedAccount<'info>,
    /// CHECK: Host fee account, optional not validated in mock
    #[account(mut)]
    pub host_fee_in: Option<UncheckedAccount<'info>>,
    pub user: Signer<'info>,
    pub token_x_program: Interface<'info, TokenInterface>,
    pub token_y_program: Interface<'info, TokenInterface>,
}

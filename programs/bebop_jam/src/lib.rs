// JAM Settlement — Solana
//
// Minimal implementation of Bebop's Aggregation API settlement on Solana.
// No solver registry (not in JamSettlement.sol).
// No batch settle (addable later, not needed for initial integration).
// No settleBebopBlend path (separate concern — routes through Solana RFQ program).
//
// settle() flow (from JamSettlement.sol):
//   1. validateOrder — sig, nonce, expiry, executor
//   2. transferTokens — sell tokens taker → custody
//   3. runInteractions — solver routes, delivers buy tokens to custody
//   4. transferTokensFromContract — buy tokens custody → receiver (enforces minimums)
//   5. transferSellToSolver — sell tokens custody → solver (EVM parity; after buy verified)

use anchor_lang::prelude::*;

pub mod error;
pub mod instructions;

use instructions::*;

// Placeholder — replace with `solana-keygen pubkey target/deploy/jam_settlement-keypair.json`
// after the first successful `anchor build` generates the keypair.
declare_id!("E51cxgBTJiNcBbG3DZfp4REDbcQb1PF7vmkoPqMR6bA2");

#[program]
pub mod jam_settlement {
    use super::*;

    pub fn init_config(ctx: Context<InitConfig>, params: InitConfigParams) -> Result<()> {
        let c = &mut ctx.accounts.config;
        c.admin = ctx.accounts.admin.key();
        c.treasury = params.treasury;
        c.min_share_bps = params.min_share_bps;
        c.protocol_fee_bps = params.protocol_fee_bps;
        c.authority_bump = ctx.bumps.jam_authority; // jam_authority is derivable, not stored in config
        c.bump = ctx.bumps.config;
        Ok(())
    }

    pub fn update_config(ctx: Context<UpdateConfig>, params: UpdateConfigParams) -> Result<()> {
        let c = &mut ctx.accounts.config;
        if let Some(v) = params.min_share_bps   { c.min_share_bps = v; }
        if let Some(v) = params.protocol_fee_bps {
            // protocol_fee_bps is a basis-point value; cap at 10_000 (100%).
            // Without this, an admin could set 65535 and make all settlements
            // revert with InsufficientOutput, bricking the protocol.
            require!(v <= 10_000, jam_settlement::error::JamError::InvalidPartnerFee);
            c.protocol_fee_bps = v;
        }
        if let Some(v) = params.treasury         { c.treasury = v; }
        Ok(())
    }

    /// Solver settles a taker-signed order by running arbitrary interactions.
    pub fn settle<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, Settle<'info>>,
        order: SolanaJamOrder,
        interactions: Vec<SolanaInteraction>,
    ) -> Result<()> {
        settle::handle_settle(ctx, order, interactions)
    }

    /// Solver IS the maker — direct transfer, no interactions needed.
    pub fn settle_internal<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, SettleInternal<'info>>,
        order: SolanaJamOrder,
        filled_amounts: Vec<u64>,
    ) -> Result<()> {
        settle::handle_settle_internal(ctx, order, filled_amounts)
    }

    pub fn close_nonce_record(
        ctx: Context<CloseNonceRecord>,
        params: CloseNonceRecordParams,
    ) -> Result<()> {
        settle::handle_close_nonce_record(ctx, params)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct InitConfigParams {
    pub treasury: Pubkey,
    pub min_share_bps: u16,
    pub protocol_fee_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct UpdateConfigParams {
    pub treasury: Option<Pubkey>,
    pub min_share_bps: Option<u16>,
    pub protocol_fee_bps: Option<u16>,
}

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(init, payer = admin, space = JamConfig::SPACE, seeds = [JAM_CONFIG_SEED], bump)]
    pub config: Account<'info, JamConfig>,
    /// CHECK: PDA verified by seeds
    #[account(seeds = [JAM_AUTHORITY_SEED], bump)]
    pub jam_authority: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(address = config.admin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [JAM_CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, JamConfig>,
}

// close_nonce_record — permissionless rent reclaim once the order's expiry has passed.
// Security: the settle handler checks Clock::now() < order.expiry before init-ing
// the nonce_record, so the record can only exist for orders that were not yet expired
// at settlement time. Once now > record.expiry the order can no longer be settled
// (expiry check would reject it), making the nonce_record inert and safe to close.
//
// Using settled_at + constant (e.g. 7 days) would be UNSAFE: an order settled early
// within a 30-day validity window could have its nonce_record closed while the order
// is still valid, reopening a replay window. record.expiry is the correct condition.
// CloseNonceRecord is defined in instructions::settle with params.
// Re-exported here via `use instructions::*` above.


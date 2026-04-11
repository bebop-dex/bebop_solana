mod instructions;
pub mod error;

use anchor_lang::prelude::*;
use instructions::*;

#[constant]
pub const TEMPORARY_WSOL_TOKEN_ACCOUNT: &[u8] = instructions::TEMPORARY_WSOL_TOKEN_ACCOUNT;
#[constant]
pub const SHARED_ACCOUNT: &[u8] = instructions::SHARED_ACCOUNT;


declare_id!("5kC1S7QB4xc5rbEVN6yz5PgEAEEASDthRqAwCaSuv2aW");

#[program]
pub mod bebop_rfq {
    use super::*;

    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
    pub struct AmountWithExpiry {
        pub amount: u64,
        pub expiry: i64, // matches Clock::unix_timestamp and JAM order.expiry
    }

    pub fn swap<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
        input_amount: u64,
        output_amounts: Vec<AmountWithExpiry>,
        event_id: u64,
        shared_account_bump: u8,
        wsol_bump: u8,
    ) -> Result<()> {
        handle_swap(ctx, input_amount, output_amounts, event_id, shared_account_bump, wsol_bump)
    }
}

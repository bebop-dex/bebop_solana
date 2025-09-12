mod instructions;
pub mod error;

use anchor_lang::prelude::*;
use instructions::*;

#[constant]
pub const TEMPORARY_WSOL_TOKEN_ACCOUNT: &[u8] = instructions::TEMPORARY_WSOL_TOKEN_ACCOUNT;
#[constant]
pub const SHARED_ACCOUNT: &[u8] = instructions::SHARED_ACCOUNT;


declare_id!("bbbkLKxMtHnw8tdioevBdg4jzjHrY9wT9GHwjoPMKDN");

#[program]
pub mod bebop_rfq {
    use super::*;

    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
    pub struct AmountWithExpiry {
        pub amount: u64, 
        pub expiry: u64,
    }

    pub fn swap<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
        input_amount: u64,
        output_amounts: Vec<AmountWithExpiry>,
        event_id: u64,
    ) -> Result<()> {
        handle_swap(ctx, input_amount, output_amounts, event_id)
    }
}

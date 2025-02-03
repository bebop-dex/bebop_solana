mod instructions;
mod error;

use anchor_lang::prelude::*;
use instructions::*;

#[constant]
pub const TEMPORARY_WSOL_TOKEN_ACCOUNT: &[u8] = instructions::TEMPORARY_WSOL_TOKEN_ACCOUNT;


#[cfg(not(feature = "production"))]
declare_id!("BHDQ7sHBxrvLutJTF2cDmr77Ws5kQznVShAvBxozZJ63");

#[program]
pub mod bebop_rfq {
    use super::*;

    pub fn swap<'c: 'info, 'info>(
        ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
        input_amount: u64,
        output_amount: u64,
        expire_at: i64,
    ) -> Result<()> {
        handle_swap(ctx, input_amount, output_amount, expire_at)
    }
}


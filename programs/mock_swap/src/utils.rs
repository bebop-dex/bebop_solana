use anchor_lang::{prelude::*, solana_program::program_pack::Pack};
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

#[error_code]
pub enum CustomError {
    Token2022MintExtensionNotSupported,
}

pub fn transfer<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    amount: u64,
    seeds: Option<&[&[&[u8]]]>,
) -> Result<()> {
    let decimals_for_transfer_checked = if token_program.key.eq(&spl_token_2022::ID) {
        let mint_data = mint.try_borrow_data()?;
        let mint_state_with_extensions =
            StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
        if let Ok(transfer_fee_config) = mint_state_with_extensions.get_extension::<TransferFeeConfig>(){
            require!(
                transfer_fee_config
                    .get_epoch_fee(Clock::get()?.epoch)
                    .transfer_fee_basis_points
                    == PodU16([0; 2]), CustomError::Token2022MintExtensionNotSupported
            );
        }
        Some(mint_state_with_extensions.base.decimals)
    } else {
        None
    };

    match decimals_for_transfer_checked {
        Some(decimals) => {
            let transfer_ctx = match seeds {
                Some(seeds) => CpiContext::new_with_signer(
                    token_program,
                    token_interface::TransferChecked { from, mint, to, authority },
                    seeds,
                ),
                None => CpiContext::new(token_program, token_interface::TransferChecked { from, mint, to, authority }),
            };
            token_interface::transfer_checked(transfer_ctx, amount, decimals)
        },
        None => {
            let transfer_ctx = match seeds {
                Some(seeds) => CpiContext::new_with_signer(
                    token_program,
                    token::Transfer { from, to, authority},
                    seeds,
                ),
                None => CpiContext::new(token_program, token::Transfer { from, to, authority}),
            };
            token::transfer(transfer_ctx, amount)
        }
    }
}


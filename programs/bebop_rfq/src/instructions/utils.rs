use anchor_lang::{prelude::*, solana_program::program_pack::Pack, system_program};
use anchor_spl::{
    associated_token::spl_associated_token_account::tools::account::create_pda_account,
    token::{
        self,
        spl_token,
    },
    token_2022::spl_token_2022::{
        self,
        extension::{
            transfer_fee::TransferFeeConfig, BaseStateWithExtensions, StateWithExtensions,
        },
    },
    token_interface::{self, spl_pod::primitives::PodU16, TokenInterface},
};

use crate::error::BebopError;

pub const TEMPORARY_WSOL_TOKEN_ACCOUNT: &[u8] = b"temporary-wsol-token-account";
pub const SHARED_ACCOUNT: &[u8] = b"shared-account";


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
                    == PodU16([0; 2]),
                BebopError::Token2022MintExtensionNotSupported
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


#[allow(clippy::too_many_arguments)]
pub fn unwrap_sol<'info>(
    maker: AccountInfo<'info>,
    sender: AccountInfo<'info>,
    sender_token_account: AccountInfo<'info>,
    receiver: Option<AccountInfo<'info>>,
    temporary_wsol_token_account: Option<&AccountInfo<'info>>,
    wsol_mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let temporary_wsol_token_account = temporary_wsol_token_account
        .ok_or(BebopError::MissingTemporaryWrappedSolTokenAccount)?;

    let (expected_temporary_wsol_token_account, bump) = Pubkey::find_program_address(
        &[TEMPORARY_WSOL_TOKEN_ACCOUNT, maker.key.as_ref()],
        &crate::ID,
    );
    require_keys_eq!(
        temporary_wsol_token_account.key(),
        expected_temporary_wsol_token_account
    );
    let new_pda_signer_seeds: &[&[u8]] = &[TEMPORARY_WSOL_TOKEN_ACCOUNT, maker.key.as_ref(), &[bump]];
    create_pda_account(
        &maker,
        &Rent::get()?,
        spl_token::state::Account::LEN,
        &spl_token::ID,
        &system_program,
        temporary_wsol_token_account,
        new_pda_signer_seeds,
    )?;
    token::initialize_account3(CpiContext::new(
        token_program.to_account_info(),
        token::InitializeAccount3 {
            account: temporary_wsol_token_account.clone(),
            mint: wsol_mint,
            authority: maker.clone(),
        },
    ))?;

    token::transfer(
        CpiContext::new(
            token_program.clone(),
            token::Transfer {
                from: sender_token_account.clone(),
                to: temporary_wsol_token_account.clone(),
                authority: sender.clone(),
            },
        ),
        amount,
    )?;

    // Close temporary wsol token account into the maker
    token::close_account(CpiContext::new(
        token_program.to_account_info(),
        token::CloseAccount {
            account: temporary_wsol_token_account.clone(),
            destination: maker.clone(),
            authority: maker.clone(),
        },
    ))?;
    if let Some(receiver) = receiver {
        // Transfer native sol to receipient
        system_program::transfer(
            CpiContext::new(
                system_program,
                system_program::Transfer {
                    from: maker,
                    to: receiver,
                },
            ),
            amount,
        )?;
    }
    Ok(())
}

use anchor_lang::{
    prelude::*,
    solana_program::{
        program::{invoke, invoke_signed},
        program_pack::Pack,
        system_instruction,
    },
};
use anchor_spl::{
    token::{self, spl_token::{self, native_mint}},
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
        let mint_state =
            StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
        if let Ok(fee_cfg) = mint_state.get_extension::<TransferFeeConfig>() {
            require!(
                fee_cfg.get_epoch_fee(Clock::get()?.epoch).transfer_fee_basis_points
                    == PodU16([0; 2]),
                crate::error::BebopError::Token2022MintExtensionNotSupported
            );
        }
        if let Ok(pd) = mint_state.get_extension::<PermanentDelegate>() {
            require!(
                Option::<Pubkey>::from(pd.delegate).is_none(),
                crate::error::BebopError::Token2022MintExtensionNotSupported
            );
        }
        require!(
            mint_state.get_extension::<TransferHook>().is_err(),
            crate::error::BebopError::Token2022MintExtensionNotSupported
        );
        if let Ok(_) = mint_state.get_extension::<ConfidentialTransferMint>() {
            return err!(crate::error::BebopError::Token2022MintExtensionNotSupported);
        }
        Some(mint_state.base.decimals)
    } else {
        None
    };

    match decimals_for_transfer_checked {
        Some(decimals) => {
            let ctx = match seeds {
                Some(s) => CpiContext::new_with_signer(token_program, token_interface::TransferChecked { from, mint, to, authority }, s),
                None    => CpiContext::new(token_program, token_interface::TransferChecked { from, mint, to, authority }),
            };
            token_interface::transfer_checked(ctx, amount, decimals)
        }
        None => {
            let ctx = match seeds {
                Some(s) => CpiContext::new_with_signer(token_program, token::Transfer { from, to, authority }, s),
                None    => CpiContext::new(token_program, token::Transfer { from, to, authority }),
            };
            token::transfer(ctx, amount)
        }
    }
}

/// Converts `amount` wSOL in `source_wsol_ata` into native SOL delivered to maker,
/// optionally forwarding `amount` lamports on to an explicit receiver.
///
/// Security: require_keys_eq fires before any state changes, so a wrong
/// temporary_wsol_account address causes an immediate, clean revert.
#[allow(clippy::too_many_arguments)]
pub fn unwrap_sol<'info>(
    maker: AccountInfo<'info>,
    source_authority: AccountInfo<'info>,
    source_wsol_ata: AccountInfo<'info>,
    receiver: Option<AccountInfo<'info>>,
    temporary_wsol_account: Option<&AccountInfo<'info>>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    system_program: AccountInfo<'info>,
    amount: u64,
    wsol_bump: u8, // canonical bump for [TEMPORARY_WSOL_TOKEN_ACCOUNT, maker.key]
    // Avoids find_program_address (~2000 CU) per wSOL swap.
) -> Result<()> {
    let temp = temporary_wsol_account
        .ok_or(error!(crate::error::BebopError::WrongSharedAccountAddress))?;

    let expected_pda = Pubkey::create_program_address(
        &[TEMPORARY_WSOL_TOKEN_ACCOUNT, maker.key.as_ref(), &[wsol_bump]],
        &crate::ID,
    ).map_err(|_| error!(crate::error::BebopError::WrongSharedAccountAddress))?;
    require_keys_eq!(temp.key(), expected_pda, crate::error::BebopError::WrongSharedAccountAddress);

    let bump_arr = [wsol_bump];
    let signer_seeds: &[&[u8]] = &[TEMPORARY_WSOL_TOKEN_ACCOUNT, maker.key.as_ref(), &bump_arr];

    // Create temp wSOL token account (maker pays rent; program signs for PDA).
    let space = spl_token::state::Account::LEN;
    invoke_signed(
        &system_instruction::create_account(
            maker.key, temp.key,
            Rent::get()?.minimum_balance(space),
            space as u64,
            &spl_token::ID,
        ),
        &[maker.clone(), temp.clone()],
        &[signer_seeds],
    )?;

    // Initialize as wSOL account owned by maker (no invoke_signed needed for
    // close_account below since maker is already a transaction signer).
    invoke(
        &spl_token::instruction::initialize_account3(
            &spl_token::ID, temp.key, mint.key, maker.key,
        )?,
        &[temp.clone(), mint.clone()],
    )?;

    // Transfer wSOL from source into temp. For native mint, SPL token::transfer
    // moves both the amount field AND the backing lamports, so temp gains
    // `amount` lamports on top of its rent deposit.
    token::transfer(
        CpiContext::new(token_program.clone(), token::Transfer {
            from: source_wsol_ata, to: temp.clone(), authority: source_authority,
        }),
        amount,
    )?;

    // Close temp — all lamports (rent + wSOL backing) go to maker.
    token::close_account(CpiContext::new(token_program, token::CloseAccount {
        account: temp.clone(), destination: maker.clone(), authority: maker.clone(),
    }))?;

    // Optionally forward the token-equivalent SOL to an explicit receiver.
    // Maker retains the rent reimbursement as compensation for creating the
    // temporary account.
    if let Some(recv) = receiver {
        anchor_lang::system_program::transfer(
            CpiContext::new(system_program, anchor_lang::system_program::Transfer {
                from: maker, to: recv,
            }),
            amount,
        )?;
    }

    Ok(())
}

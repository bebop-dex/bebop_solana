mod test_env;

use solana_program_test::{tokio, BanksClientError};
use assert_matches::assert_matches;
use solana_sdk::signature::Keypair;
use test_case::test_case;
use test_env::{prepare_test, process_instructions, sign_and_execute_tx, AccountKind, Accounts, TestMode};
use spl_token_client::token::{ExtensionInitializationParams, Token};



// #[test_case(Default::default())]
// #[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, ..Default::default()})]

#[test_case(TestMode { input_amounts: vec![1_000_000_000, 3_000_000_000, 900_000_000], output_amounts: vec![2_000_000_000, 6_000_000_000, 1_000_000_000], taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000, 3_000_000_000], output_amounts: vec![2_000_000_000, 6_000_000_000], taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000], output_amounts: vec![2_000_000_000], taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[tokio::test]
async fn test_swap(test_mode: TestMode) {
    let expected_error = test_mode.expected_error.clone();
    let env = prepare_test(test_mode.clone()).await;
    let swap_instr = env.create_swap_instructions();

    println!("GM!");
    let cur_makers = &env.makers_keypairs[..test_mode.input_amounts.len()];
    let result = sign_and_execute_tx(
        swap_instr.as_slice(),
        &env.payer,
        &env.taker_keypair, 
        cur_makers,
        &env.banks_client,
    )
    .await;

    match expected_error {
        Some(expected_error) => {
            let BanksClientError::TransactionError(transaction_error) = result.unwrap_err() else {
                panic!("The error was not a transaction error");
            };
            assert_eq!(transaction_error, expected_error);
            return;
        }
        None => {
            assert_matches!(result, Ok(()));
        }
    }
    println!("GOOD!");
    
}



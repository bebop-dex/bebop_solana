mod test_utils;

use anchor_spl::associated_token::spl_associated_token_account::instruction;
use solana_program_test::{tokio, BanksClientError};
use assert_matches::assert_matches;
use solana_sdk::{signer::Signer, transaction::TransactionError};
use test_case::test_case;
use test_utils::{create_onchain_swap_instruction, get_associated_token_account, mint_balance, prepare_test, sign_and_execute_tx, AccountKind, Accounts, BalanceChecker, BalanceReader, MiddleTokenInfo, OnchainSwapType, OnchainTokens, ReceiverKind, TestEnvironment, TestMode};
use spl_token_client::token::ExtensionInitializationParams;




#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token },  ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, output_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 100, maximum_fee: u64::MAX }]), expected_error: Some(solana_sdk::transaction::TransactionError::InstructionError(1, solana_sdk::instruction::InstructionError::Custom(u32::from(bebop_rfq::error::BebopError::Token2022MintExtensionNotSupported)))), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::NonTransferable]), expected_error: Some(TransactionError::InstructionError(1, solana_sdk::instruction::InstructionError::Custom(anchor_spl::token_2022::spl_token_2022::error::TokenError::NonTransferable as u32))), ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000, 3_000_000_000, 900_000_000], output_amounts: vec![2_000_000_000, 6_000_000_000, 1_000_000_000],  receiver_kind: ReceiverKind::AnotherAddress, use_shared_taker: false, taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000, 3_000_000_000], output_amounts: vec![2_000_000_000, 6_000_000_000],  receiver_kind: ReceiverKind::Taker, use_shared_taker: false, taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000], output_amounts: vec![2_000_000_000], receiver_kind: ReceiverKind::TakerWithTokenAccount, use_shared_taker: false, taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { input_amounts: vec![1_000_000_000], output_amounts: vec![2_000_000_000], receiver_kind: ReceiverKind::Taker, use_shared_taker: false, taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, ..Default::default()})]
#[tokio::test]
async fn test_direct_swap(test_mode: TestMode) {
    assert!(!test_mode.use_shared_taker);
    let env: TestEnvironment = prepare_test(test_mode.clone()).await;
    let all_instructions = env.create_single_swap_instructions(test_mode.clone(), true).await;
    let balance_checker: BalanceChecker = BalanceChecker::new(&env).await;
    let cur_makers = &env.makers_keypairs[..test_mode.input_amounts.len()];
    let result = sign_and_execute_tx(
        all_instructions.as_slice(),
        &env.payer,
        &env.taker_keypair, 
        cur_makers,
        &env.banks_client,
    )
    .await;
    match test_mode.expected_error {
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
    balance_checker.verify_balances_direct_swap(&env, test_mode).await;
}



#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token },  ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }])}), ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, output_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 100, maximum_fee: u64::MAX }]), expected_error: Some(solana_sdk::transaction::TransactionError::InstructionError(1, solana_sdk::instruction::InstructionError::Custom(u32::from(bebop_rfq::error::BebopError::Token2022MintExtensionNotSupported)))), ..Default::default()})]
#[test_case(TestMode { middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, input_mint_extensions: Some(vec![ExtensionInitializationParams::NonTransferable]), expected_error: Some(TransactionError::InstructionError(1, solana_sdk::instruction::InstructionError::Custom(anchor_spl::token_2022::spl_token_2022::error::TokenError::NonTransferable as u32))), ..Default::default()})]
#[test_case(TestMode {middle_token_info: Some(MiddleTokenInfo{token_amount: 7_000_000_000, mint_extensions: None}), ..Default::default()})]
#[tokio::test]
async fn test_2_hops_with_makers(test_mode: TestMode) {
    // token_a -> token_c -> token_b
    // token_a <-> token_c (taker=taker, maker=maker1, receiver=shared_pda)
    // token_c <-> token_b (taker=shared_pda, maker=maker2, receiver=taker)
    assert!(test_mode.middle_token_info.is_some() && test_mode.input_amounts.len() == 1);
    let env: TestEnvironment = prepare_test(test_mode.clone()).await;
    let all_instructions = env.create_2_hops_instructions(test_mode.clone()).await;
    let balance_checker: BalanceChecker = BalanceChecker::new(&env).await;
    let cur_makers = &env.makers_keypairs[..2];
    let result = sign_and_execute_tx(
        all_instructions.as_slice(),
        &env.payer,
        &env.taker_keypair, 
        cur_makers,
        &env.banks_client,
    )
    .await;
    match test_mode.expected_error {
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
    balance_checker.verify_balances_for_2_hops(&env, test_mode).await;
    
}


#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token }, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), output_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { receiver_kind: ReceiverKind::AnotherAddress, use_shared_taker: true,  onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[test_case(TestMode { receiver_kind: ReceiverKind::Taker, use_shared_taker: true, onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), input_mint_extensions: Some(vec![ExtensionInitializationParams::TransferFeeConfig { transfer_fee_config_authority: None, withdraw_withheld_authority: None, transfer_fee_basis_points: 0, maximum_fee: 0 }]), ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCLMM), receiver_kind: ReceiverKind::AnotherAddress, use_shared_taker: true, ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::MeteoraDLMM), receiver_kind: ReceiverKind::AnotherAddress, use_shared_taker: true, ..Default::default()})]
#[tokio::test]
async fn test_swap_from_pda(test_mode: TestMode) {
    // taker --onchain-swap--> shared_pda <--SWAP--> makers
    // token_c -> token_a -> token_b
    assert!(test_mode.use_shared_taker && test_mode.onchain_swap_type.is_some());
    let env: TestEnvironment = prepare_test(test_mode.clone()).await;
    let mut all_instructions = env.create_single_swap_instructions(test_mode.clone(), false).await;

    // mint 5_000_000_000 token_c to taker
    let taker_token_c_input = 5_000_000_000;
    get_associated_token_account(env.taker, &env.token_c, AccountKind::Token, true).await;
    mint_balance(taker_token_c_input, env.taker_token_c_account,
        &env.token_c, test_mode.clone().taker_accounts.input, &env.banks_client, &env.payer).await;

    let (onchain_swap_output, final_swap_output): (u64, u64) = match test_mode.onchain_swap_type {
        Some(OnchainSwapType::RaydiumCLMM) => {
            // in case of RaydiumCLMM apply negative slippage 50% for the output
            (test_mode.input_amounts.iter().sum::<u64>() / 2, test_mode.output_amounts.iter().sum::<u64>() / 2)
        }
        Some(OnchainSwapType::MeteoraDLMM) => {
            // in case of MeteoraDLMM apply positive slippage for the output, all positive slippage goes to maker
            (3 * test_mode.input_amounts.iter().sum::<u64>() / 2, test_mode.output_amounts.iter().sum::<u64>())
        }
        _ =>  (test_mode.input_amounts.iter().sum(), test_mode.output_amounts.iter().sum())
    };
    
    all_instructions.insert(0, create_onchain_swap_instruction(
        taker_token_c_input,
        onchain_swap_output,
        test_mode.clone().onchain_swap_type.unwrap(),
        OnchainTokens::C_to_A,
        &env
    ).await);

    let balance_checker: BalanceChecker = BalanceChecker::new(&env).await;
    let cur_makers = &env.makers_keypairs[..test_mode.input_amounts.len()];
    let result = sign_and_execute_tx(
        all_instructions.as_slice(),
        &env.payer,
        &env.taker_keypair, 
        cur_makers,
        &env.banks_client,
    )
    .await;
    match test_mode.expected_error {
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
    balance_checker.verify_balances_swap_from_pda(
        &env, test_mode, taker_token_c_input, onchain_swap_output, final_swap_output
    ).await;
    
}


#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token },  ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), taker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), taker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, maker_accounts: Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }, ..Default::default()})]
#[test_case(TestMode { onchain_swap_type: Some(OnchainSwapType::RaydiumCPMM), ..Default::default()})]
#[tokio::test]
async fn test_swap_then_onchain_swap(test_mode: TestMode) {
    // taker <--SWAP--> makers, then onchain swap from taker
    // token_a -(SWAP)--> token_b -(onchain)-> token_c
    assert!(!test_mode.use_shared_taker && test_mode.receiver_kind == ReceiverKind::Taker && test_mode.onchain_swap_type.is_some());
    let env: TestEnvironment = prepare_test(test_mode.clone()).await;
    let mut all_instructions = env.create_single_swap_instructions(test_mode.clone(), true).await;

    all_instructions.push(instruction::create_associated_token_account(
        &env.payer.pubkey(), &env.taker,  &env.token_c_mint, &env.token_c_program_id
    ));
    let onchain_pool_output_token_c = 5_000_000_000;    
    all_instructions.push(create_onchain_swap_instruction(
        test_mode.output_amounts.iter().sum(),
        onchain_pool_output_token_c,
        test_mode.clone().onchain_swap_type.unwrap(),
        OnchainTokens::B_to_C,
        &env
    ).await);

    let balance_checker: BalanceChecker = BalanceChecker::new(&env).await;
    let cur_makers = &env.makers_keypairs[..test_mode.input_amounts.len()];
    let result = sign_and_execute_tx(
        all_instructions.as_slice(),
        &env.payer,
        &env.taker_keypair, 
        cur_makers,
        &env.banks_client,
    )
    .await;
    match test_mode.expected_error {
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
    balance_checker.verify_balances_for_swap_then_onchain(&env, test_mode, onchain_pool_output_token_c).await;
}
use std::{str::FromStr, sync::Arc};

use anchor_lang::{
    prelude::*,
    solana_program::{self, instruction::Instruction},
    system_program, InstructionData,
};
use anchor_spl::{associated_token::spl_associated_token_account::instruction, token::{self, spl_token::native_mint}};
use assert_matches::assert_matches;
use itertools::Itertools;
use solana_program_test::{
    tokio::{self, sync::Mutex},
    BanksClient, BanksClientError, ProgramTest,
};
use solana_sdk::{
    feature_set::bpf_account_data_direct_mapping, native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature}, signer::Signer, system_instruction, transaction::{Transaction, TransactionError},
};
use spl_token_client::{
    client::{
        ProgramBanksClient, ProgramBanksClientProcessTransaction, SendTransaction,
        SimulateTransaction,
    },
    token::{ExtensionInitializationParams, Token},
};

use super::{get_associated_token_account, mint_balance, AccountKind, OnchainSwapType, TestEnvironment};


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnchainTokens {
    C_to_A,
    B_to_C
}

pub async fn create_onchain_swap_instruction(
    amount_in: u64, amount_out: u64, swap_type: OnchainSwapType, onchain_tokens: OnchainTokens, test_env: &TestEnvironment
) -> Instruction {
    let pool = Pubkey::find_program_address(
        &[mock_swap::POOL_ACCOUNT],
        &mock_swap::ID,
    ).0;
    let vault_token_a: Option<Pubkey> = get_associated_token_account(pool, &test_env.token_a, AccountKind::Token, true).await;
    let vault_token_b: Option<Pubkey> = get_associated_token_account(pool, &test_env.token_b, AccountKind::Token, true).await;
    let vault_token_c: Option<Pubkey> = get_associated_token_account(pool, &test_env.token_c, AccountKind::Token, true).await;

    match onchain_tokens {
        OnchainTokens::C_to_A => {
            mint_balance(amount_out, vault_token_a, &test_env.token_a, AccountKind::Token, &test_env.banks_client, &test_env.payer).await;

            match swap_type {
                OnchainSwapType::RaydiumCPMM => create_raydium_cpmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_c_account.unwrap(), test_env.shared_token_a_account.unwrap(),
                    test_env.token_c_program_id, test_env.token_a_program_id,
                    test_env.token_c_mint, test_env.token_a_mint,
                    pool, vault_token_c.unwrap(), vault_token_a.unwrap()
                ),
                OnchainSwapType::RaydiumCLMM => create_raydium_clmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_c_account.unwrap(), test_env.shared_token_a_account.unwrap(),
                    test_env.token_c_program_id, test_env.token_a_program_id,
                    test_env.token_c_mint, test_env.token_a_mint,
                    pool, vault_token_c.unwrap(), vault_token_a.unwrap()
                ),
                OnchainSwapType::MeteoraDLMM => create_meteora_dlmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_c_account.unwrap(), test_env.shared_token_a_account.unwrap(),
                    test_env.token_c_program_id, test_env.token_a_program_id,
                    test_env.token_c_mint, test_env.token_a_mint,
                    pool, vault_token_c.unwrap(), vault_token_a.unwrap()
                )
            }
        }
        OnchainTokens::B_to_C => {
            mint_balance(amount_out, vault_token_c, &test_env.token_c, AccountKind::Token, &test_env.banks_client, &test_env.payer).await;

            match swap_type {
                OnchainSwapType::RaydiumCPMM => create_raydium_cpmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_b_account.unwrap(), test_env.taker_token_c_account.unwrap(),
                    test_env.token_b_program_id, test_env.token_c_program_id,
                    test_env.token_b_mint, test_env.token_c_mint,
                    pool, vault_token_b.unwrap(), vault_token_c.unwrap()
                ),
                OnchainSwapType::RaydiumCLMM => create_raydium_clmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_b_account.unwrap(), test_env.taker_token_c_account.unwrap(),
                    test_env.token_b_program_id, test_env.token_c_program_id,
                    test_env.token_b_mint, test_env.token_c_mint,
                    pool, vault_token_b.unwrap(), vault_token_c.unwrap()
                ),
                OnchainSwapType::MeteoraDLMM => create_meteora_dlmm_instruction(
                    amount_in, amount_out, &test_env.taker,
                    test_env.taker_token_b_account.unwrap(), test_env.taker_token_c_account.unwrap(),
                    test_env.token_b_program_id, test_env.token_c_program_id,
                    test_env.token_b_mint, test_env.token_c_mint,
                    pool, vault_token_b.unwrap(), vault_token_c.unwrap()
                )
            }
        }
    }
}


fn create_raydium_cpmm_instruction(
    amount_in: u64, amount_out: u64, taker: &Pubkey, input_token_account: Pubkey, output_token_account: Pubkey,
    input_token_program: Pubkey, output_token_program: Pubkey, input_token_mint: Pubkey, output_token_mint: Pubkey,
    pool: Pubkey, input_token_vault: Pubkey, output_token_vault: Pubkey
) -> Instruction {
    let data = mock_swap::instruction::SwapOnRaydiumCpmm {
        amount_in: amount_in,
        minimum_amount_out: amount_out
    }
    .data();
    let instruction = Instruction {
        program_id: mock_swap::ID,
        accounts: mock_swap::accounts::MockRaydiumCPMM {
            payer: *taker,
            authority: pool,
            amm_config: Keypair::new().pubkey(),
            pool_state: Keypair::new().pubkey(),
            input_token_account: input_token_account,
            output_token_account: output_token_account,
            input_vault: input_token_vault,
            output_vault: output_token_vault,
            input_token_program: input_token_program,
            output_token_program: output_token_program,
            input_token_mint: input_token_mint,
            output_token_mint: output_token_mint,
            observation_state: Keypair::new().pubkey(),
        }
        .to_account_metas(None),
        data,
    };
    instruction

}


fn create_raydium_clmm_instruction(
    amount_in: u64, amount_out: u64, taker: &Pubkey, input_token_account: Pubkey, output_token_account: Pubkey,
    input_token_program: Pubkey, output_token_program: Pubkey, input_token_mint: Pubkey, output_token_mint: Pubkey,
    pool: Pubkey, input_token_vault: Pubkey, output_token_vault: Pubkey
) -> Instruction {
    let data = mock_swap::instruction::SwapOnRaydiumClmm {
        amount_in: amount_in,
        minimum_amount_out: amount_out
    }
    .data();

    let mut instruction = Instruction {
        program_id: mock_swap::ID,
        accounts: mock_swap::accounts::MockRaydiumCLMM {
            payer: *taker,
            amm_config: Keypair::new().pubkey(),
            pool_state: pool,
            input_token_account: input_token_account,
            output_token_account: output_token_account,
            input_vault: input_token_vault,
            output_vault: output_token_vault,
            input_token_program: input_token_program,
            output_token_program: output_token_program,
            input_vault_mint: input_token_mint,
            output_vault_mint: output_token_mint,
            observation_state: Keypair::new().pubkey(),
            memo_program: Pubkey::from_str("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr").unwrap(),            
            token_program: anchor_spl::token::ID,
            token_program_2022: anchor_spl::token_2022::ID,
        }
        .to_account_metas(None),
        data,
    };
    //tick_array_accounts:
    instruction.accounts.push(AccountMeta::new(Keypair::new().pubkey(), false));
    instruction.accounts.push(AccountMeta::new(Keypair::new().pubkey(), false));
    instruction

}

fn create_meteora_dlmm_instruction(
    amount_in: u64, amount_out: u64, taker: &Pubkey, input_token_account: Pubkey, output_token_account: Pubkey,
    input_token_program: Pubkey, output_token_program: Pubkey, input_token_mint: Pubkey, output_token_mint: Pubkey,
    pool: Pubkey, input_token_vault: Pubkey, output_token_vault: Pubkey
) -> Instruction {
    let data = mock_swap::instruction::SwapOnMeteoraDlmm {
        amount_in: amount_in,
        minimum_amount_out: amount_out
    }
    .data();
    let instruction = Instruction {
        program_id: mock_swap::ID,
        accounts: mock_swap::accounts::MockMeteoraDLMM {
            user: *taker,
            lb_pair: pool,
            user_token_in: input_token_account,
            user_token_out: output_token_account,
            reserve_x: input_token_vault,
            reserve_y: output_token_vault,
            token_x_program: input_token_program,
            token_y_program: output_token_program,
            token_x_mint: input_token_mint,
            token_y_mint: output_token_mint,
            oracle: Keypair::new().pubkey(),
            host_fee_in: Some(Keypair::new().pubkey()),
            bin_array_bitmap_extension: Some(Keypair::new().pubkey()),
        }
        .to_account_metas(None),
        data,
    };
    instruction

}
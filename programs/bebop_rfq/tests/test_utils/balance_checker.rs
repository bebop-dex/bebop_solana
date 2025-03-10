use anchor_lang::{
    prelude::*,
    solana_program::{self, instruction::Instruction},
    system_program, InstructionData,
};
use anchor_spl::{associated_token::spl_associated_token_account::instruction, token::{self, spl_token::{instruction::sync_native, native_mint}}};
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
use crate::test_utils::{AccountKind, ReceiverKind};

use super::{TestEnvironment, TestMode};



#[derive(Debug, Clone)]
pub struct Balances {
    native: u64,
    token_a: u64,
    token_b: u64,
    token_c: u64,
}


#[derive(Debug, Clone)]
pub struct BalanceChecker {
    taker_balances: Balances,
    receiver_balances: Balances,
    shared_pda_balances: Balances,
    makers_balances: Vec<Balances>,
}

impl BalanceChecker {
    pub async fn new(env: &TestEnvironment) -> Self {
        let taker_balances = Self::get_balances(
            &env, env.taker, env.taker_token_a_account, env.taker_token_b_account, env.taker_token_c_account
        ).await;
        let receiver_balances = Self::get_balances(
            &env, env.random_receiver, env.receiver_token_a_account, env.receiver_token_b_account, env.receiver_token_c_account
        ).await;
        let shared_pda_balances = Self::get_balances(
            &env, env.shared_pda, env.shared_token_a_account, env.shared_token_b_account, env.shared_token_c_account
        ).await;
        let mut makers_balances = Vec::new();
        for (i, maker) in env.makers.iter().enumerate() {
            makers_balances.push(Self::get_balances(
                &env, *maker,
                if env.makers_token_a_account.is_empty() {None} else {Some(env.makers_token_a_account[i])},
                if env.makers_token_b_account.is_empty() {None} else {Some(env.makers_token_b_account[i])},
                if env.makers_token_c_account.is_empty() {None} else {Some(env.makers_token_c_account[i])},
            ).await);
        }
        Self {
            taker_balances,
            receiver_balances,
            shared_pda_balances,
            makers_balances,
        }
    }

    pub async fn verify_balances_direct_swap(&self, env: &TestEnvironment, test_mode: TestMode){
        let new_balances: BalanceChecker = Self::new(env).await;
        
        // Verify taker balances
        assert_eq!(
            self.taker_balances.token_a.checked_sub(new_balances.taker_balances.token_a),
            Some(test_mode.input_amounts.iter().sum())
        );
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                assert_eq!(
                    new_balances.taker_balances.token_b.checked_sub(self.taker_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
            ReceiverKind::AnotherAddress => {
                assert_eq!(
                    new_balances.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
            ReceiverKind::SharedAccount => {
                assert_eq!(
                    new_balances.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
        }
        assert_eq!(new_balances.taker_balances.token_c, self.taker_balances.token_c);

        // Verify receiver balances
        assert_eq!(new_balances.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(new_balances.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(new_balances.receiver_balances.native, self.receiver_balances.native);

        // Verify shared-pda balances
        assert_eq!(new_balances.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(new_balances.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(new_balances.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(new_balances.shared_pda_balances.native, self.shared_pda_balances.native);

        // Verify makers balances
        for i in 0..test_mode.input_amounts.len() {
            assert_eq!(
                new_balances.makers_balances[i].token_a.checked_sub(self.makers_balances[i].token_a),
                Some(test_mode.input_amounts[i])
            );
            assert_eq!(
                self.makers_balances[i].token_b.checked_sub(new_balances.makers_balances[i].token_b),
                Some(test_mode.output_amounts[i])
            );
            assert_eq!(new_balances.makers_balances[i].token_c, self.makers_balances[i].token_c);
        }

        // Verify native balances
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(new_balances.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() {
                assert_eq!(new_balances.makers_balances[i].native, self.makers_balances[i].native);
            }
        }
    }

    pub async fn verify_balances_for_2_hops(&self, env: &TestEnvironment, test_mode: TestMode){
        let new_balances: BalanceChecker = Self::new(env).await;
        
        // Verify taker balances
        assert_eq!(
            self.taker_balances.token_a.checked_sub(new_balances.taker_balances.token_a),
            Some(test_mode.input_amounts.iter().sum())
        );
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                assert_eq!(
                    new_balances.taker_balances.token_b.checked_sub(self.taker_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
            ReceiverKind::AnotherAddress => {
                assert_eq!(
                    new_balances.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
            ReceiverKind::SharedAccount => {
                assert_eq!(
                    new_balances.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b),
                    Some(test_mode.output_amounts.iter().sum())
                );
            },
        }
        assert_eq!(new_balances.taker_balances.token_c, self.taker_balances.token_c);

        // Verify receiver balances
        assert_eq!(new_balances.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(new_balances.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(new_balances.receiver_balances.native, self.receiver_balances.native);

        // Verify shared-pda balances
        assert_eq!(new_balances.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(new_balances.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(new_balances.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(new_balances.shared_pda_balances.native, self.shared_pda_balances.native);

        // Verify maker-1 balance
        assert_eq!(
            new_balances.makers_balances[0].token_a.checked_sub(self.makers_balances[0].token_a),
            Some(test_mode.input_amounts[0])
        );
        assert_eq!(
            self.makers_balances[0].token_c.checked_sub(new_balances.makers_balances[0].token_c),
            Some(test_mode.middle_token_info.clone().unwrap().token_amount)
        );
        assert_eq!(new_balances.makers_balances[0].token_b, self.makers_balances[0].token_b);

        // Verify maker-2 balance
        assert_eq!(
            new_balances.makers_balances[1].token_c.checked_sub(self.makers_balances[1].token_c),
            Some(test_mode.middle_token_info.unwrap().token_amount)
        );
        assert_eq!(
            self.makers_balances[1].token_b.checked_sub(new_balances.makers_balances[1].token_b),
            Some(test_mode.output_amounts[0])
        );
        assert_eq!(new_balances.makers_balances[1].token_a, self.makers_balances[1].token_a);

        // Verify native balances
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(new_balances.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() {
                assert_eq!(new_balances.makers_balances[i].native, self.makers_balances[i].native);
            }
        }
    }

    pub async fn verify_balances_swap_from_pda(
        &self, env: &TestEnvironment, test_mode: TestMode, onchain_input_amount: u64, onchain_output_amount: u64, final_output_amount: u64
    ){
        let new_balances: BalanceChecker = Self::new(env).await;
        
        // Verify taker balances
        assert_eq!(
            self.taker_balances.token_c.checked_sub(new_balances.taker_balances.token_c),
            Some(onchain_input_amount)
        );
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                assert_eq!(
                    new_balances.taker_balances.token_b.checked_sub(self.taker_balances.token_b),
                    Some(final_output_amount)
                );
            },
            ReceiverKind::AnotherAddress => {
                assert_eq!(
                    new_balances.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b),
                    Some(final_output_amount)
                );
            },
            ReceiverKind::SharedAccount => {
                assert_eq!(
                    new_balances.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b),
                    Some(final_output_amount)
                );
            },
        }
        assert_eq!(new_balances.taker_balances.token_a, self.taker_balances.token_a);

        // Verify receiver balances
        assert_eq!(new_balances.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(new_balances.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(new_balances.receiver_balances.native, self.receiver_balances.native);

        // Verify shared-pda balances
        assert_eq!(new_balances.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(new_balances.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(new_balances.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(new_balances.shared_pda_balances.native, self.shared_pda_balances.native);

        // Verify maker-1 balances
        assert_eq!(
            new_balances.makers_balances[0].token_a.checked_sub(self.makers_balances[0].token_a),
            Some(onchain_output_amount)
        );
        assert_eq!(
            self.makers_balances[0].token_b.checked_sub(new_balances.makers_balances[0].token_b),
            Some(final_output_amount)
        );
        assert_eq!(new_balances.makers_balances[0].token_c, self.makers_balances[0].token_c);

        // Verify native balances
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(new_balances.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() {
                assert_eq!(new_balances.makers_balances[i].native, self.makers_balances[i].native);
            }
        }
    }

    pub async fn verify_balances_for_swap_then_onchain(&self, env: &TestEnvironment, test_mode: TestMode, onchain_pool_output: u64){
        let new_balances: BalanceChecker = Self::new(env).await;
        
        // Verify taker balances
        assert_eq!(
            self.taker_balances.token_a.checked_sub(new_balances.taker_balances.token_a),
            Some(test_mode.input_amounts.iter().sum())
        );
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                assert_eq!(
                    new_balances.taker_balances.token_c.checked_sub(self.taker_balances.token_c),
                    Some(onchain_pool_output)
                );
            },
            ReceiverKind::AnotherAddress => {
                assert_eq!(
                    new_balances.receiver_balances.token_c.checked_sub(self.receiver_balances.token_c),
                    Some(onchain_pool_output)
                );
            },
            ReceiverKind::SharedAccount => {
                assert_eq!(
                    new_balances.shared_pda_balances.token_c.checked_sub(self.shared_pda_balances.token_c),
                    Some(onchain_pool_output)
                );
            },
        }
        assert_eq!(new_balances.taker_balances.token_b, self.taker_balances.token_b);

        // Verify receiver balances
        assert_eq!(new_balances.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(new_balances.receiver_balances.token_b, self.receiver_balances.token_b);
        assert_eq!(new_balances.receiver_balances.native, self.receiver_balances.native);

        // Verify shared-pda balances
        assert_eq!(new_balances.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(new_balances.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(new_balances.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(new_balances.shared_pda_balances.native, self.shared_pda_balances.native);

        // Verify makers balances
        for i in 0..test_mode.input_amounts.len() {
            assert_eq!(
                new_balances.makers_balances[i].token_a.checked_sub(self.makers_balances[i].token_a),
                Some(test_mode.input_amounts[i])
            );
            assert_eq!(
                self.makers_balances[i].token_b.checked_sub(new_balances.makers_balances[i].token_b),
                Some(test_mode.output_amounts[i])
            );
            assert_eq!(new_balances.makers_balances[i].token_c, self.makers_balances[i].token_c);
        }

        // Verify native balances
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(new_balances.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() {
                assert_eq!(new_balances.makers_balances[i].native, self.makers_balances[i].native);
            }
        }
    }

    async fn get_balances(
        env: &TestEnvironment, wallet: Pubkey,
        token_a_account: Option<Pubkey>, token_b_account: Option<Pubkey>, token_c_account: Option<Pubkey>
    ) -> Balances {
        let readers = vec![
            BalanceReader::new(&env.token_a, wallet, &None),
            BalanceReader::new(&env.token_a, wallet, &token_a_account),
            BalanceReader::new(&env.token_b, wallet, &token_b_account),
            BalanceReader::new(&env.token_c, wallet, &token_c_account)
        ];
        Balances {
            native: readers[0].get_balance().await,
            token_a: readers[1].get_balance().await,
            token_b: readers[2].get_balance().await,
            token_c: readers[3].get_balance().await,
        }
    }
}


pub struct BalanceReader<'a> {
    token: &'a Token<ProgramBanksClientProcessTransaction>,
    user: Pubkey,
    token_account: &'a Option<Pubkey>,
}


impl<'a> BalanceReader<'a> {
    pub fn new(
        token: &'a Token<ProgramBanksClientProcessTransaction>,
        user: Pubkey,
        token_account: &'a Option<Pubkey>,
    ) -> Self {
        Self {
            token,
            user,
            token_account,
        }
    }

    pub async fn get_balance(&self) -> u64 {
        match self.token_account {
            Some(token_account) => {
                if self.token.get_account(*token_account).await.is_err() {
                    return 0;
                }
                self.token.get_amount(token_account).await
            },
            None => match self.token.get_account(self.user).await {
                Ok(account) => account.lamports,
                Err(_) => 0,
            }
        }
    }
}

trait TokenExtra {
    async fn get_amount(&self, account: &Pubkey) -> u64;
}

impl<T> TokenExtra for Token<T>
where
    T: SendTransaction + SimulateTransaction,
{
    async fn get_amount(&self, account: &Pubkey) -> u64 {
        self.get_account_info(account).await.unwrap().base.amount
    }
}

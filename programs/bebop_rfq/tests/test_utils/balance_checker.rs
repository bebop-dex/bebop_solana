use std::sync::Arc;

use anchor_lang::{prelude::*, solana_program::program_pack::Pack};
use anchor_spl::token::spl_token::state::Account as TokenAccount;
use solana_program_test::tokio::sync::Mutex;
use solana_program_test::BanksClient;

use crate::test_utils::{AccountKind, ReceiverKind};
use super::{TestEnvironment, TestMode};


#[derive(Debug, Clone)]
pub struct Balances { native: u64, token_a: u64, token_b: u64, token_c: u64 }

#[derive(Debug, Clone)]
pub struct BalanceChecker {
    taker_balances: Balances,
    receiver_balances: Balances,
    shared_pda_balances: Balances,
    makers_balances: Vec<Balances>,
}

impl BalanceChecker {
    pub async fn new(env: &TestEnvironment) -> Self {
        let taker_balances = get_balances(&env.banks_client, env.taker, env.taker_token_a_account, env.taker_token_b_account, env.taker_token_c_account).await;
        let receiver_balances = get_balances(&env.banks_client, env.random_receiver, env.receiver_token_a_account, env.receiver_token_b_account, env.receiver_token_c_account).await;
        let shared_pda_balances = get_balances(&env.banks_client, env.shared_pda, env.shared_token_a_account, env.shared_token_b_account, env.shared_token_c_account).await;
        let mut makers_balances = Vec::new();
        for (i, maker) in env.makers.iter().enumerate() {
            makers_balances.push(get_balances(
                &env.banks_client, *maker,
                env.makers_token_a_account.get(i).cloned(),
                env.makers_token_b_account.get(i).cloned(),
                env.makers_token_c_account.get(i).cloned(),
            ).await);
        }
        Self { taker_balances, receiver_balances, shared_pda_balances, makers_balances }
    }

    pub async fn verify_balances_direct_swap(&self, env: &TestEnvironment, test_mode: TestMode) {
        let nb = Self::new(env).await;
        let total_input: u64 = test_mode.input_amounts.iter().sum();
        let total_output: u64 = test_mode.output_amounts.iter().sum();

        // Taker input: NativeSol → check native decrease, otherwise token_a decrease.
        if test_mode.taker_accounts.input == AccountKind::NativeSol {
            assert_eq!(self.taker_balances.native.checked_sub(nb.taker_balances.native), Some(total_input));
        } else {
            assert_eq!(self.taker_balances.token_a.checked_sub(nb.taker_balances.token_a), Some(total_input));
        }

        // Receiver output: NativeSol → check native increase, otherwise token_b increase.
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                if test_mode.taker_accounts.output == AccountKind::NativeSol {
                    assert_eq!(nb.taker_balances.native.checked_sub(self.taker_balances.native), Some(total_output));
                } else {
                    assert_eq!(nb.taker_balances.token_b.checked_sub(self.taker_balances.token_b), Some(total_output));
                }
            }
            ReceiverKind::AnotherAddress =>
                assert_eq!(nb.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b), Some(total_output)),
            ReceiverKind::SharedAccount =>
                assert_eq!(nb.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b), Some(total_output)),
        }
        assert_eq!(nb.taker_balances.token_c, self.taker_balances.token_c);
        assert_eq!(nb.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(nb.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(nb.receiver_balances.native, self.receiver_balances.native);
        assert_eq!(nb.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(nb.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(nb.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(nb.shared_pda_balances.native, self.shared_pda_balances.native);
        for i in 0..test_mode.input_amounts.len() {
            // Maker input: NativeSol → maker gained native, otherwise gained token_a.
            if test_mode.maker_accounts.input == AccountKind::NativeSol {
                assert_eq!(nb.makers_balances[i].native.checked_sub(self.makers_balances[i].native), Some(test_mode.input_amounts[i]));
            } else {
                assert_eq!(nb.makers_balances[i].token_a.checked_sub(self.makers_balances[i].token_a), Some(test_mode.input_amounts[i]));
            }
            // Maker output: NativeSol → maker lost native, otherwise lost token_b.
            if test_mode.maker_accounts.output == AccountKind::NativeSol {
                assert_eq!(self.makers_balances[i].native.checked_sub(nb.makers_balances[i].native), Some(test_mode.output_amounts[i]));
            } else {
                assert_eq!(self.makers_balances[i].token_b.checked_sub(nb.makers_balances[i].token_b), Some(test_mode.output_amounts[i]));
            }
            assert_eq!(nb.makers_balances[i].token_c, self.makers_balances[i].token_c);
        }
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(nb.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() { assert_eq!(nb.makers_balances[i].native, self.makers_balances[i].native); }
        }
    }

    pub async fn verify_balances_for_2_hops(&self, env: &TestEnvironment, test_mode: TestMode) {
        let nb = Self::new(env).await;
        let total_input: u64 = test_mode.input_amounts.iter().sum();
        let total_output: u64 = test_mode.output_amounts.iter().sum();

        // Taker input: NativeSol → check native decrease, otherwise token_a decrease.
        if test_mode.taker_accounts.input == AccountKind::NativeSol {
            assert_eq!(self.taker_balances.native.checked_sub(nb.taker_balances.native), Some(total_input));
        } else {
            assert_eq!(self.taker_balances.token_a.checked_sub(nb.taker_balances.token_a), Some(total_input));
        }

        // Receiver output: NativeSol → check native increase, otherwise token_b increase.
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                if test_mode.taker_accounts.output == AccountKind::NativeSol {
                    assert_eq!(nb.taker_balances.native.checked_sub(self.taker_balances.native), Some(total_output));
                } else {
                    assert_eq!(nb.taker_balances.token_b.checked_sub(self.taker_balances.token_b), Some(total_output));
                }
            }
            ReceiverKind::AnotherAddress =>
                assert_eq!(nb.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b), Some(total_output)),
            ReceiverKind::SharedAccount =>
                assert_eq!(nb.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b), Some(total_output)),
        }
        assert_eq!(nb.taker_balances.token_c, self.taker_balances.token_c);
        assert_eq!(nb.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(nb.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(nb.receiver_balances.native, self.receiver_balances.native);
        assert_eq!(nb.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(nb.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(nb.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(nb.shared_pda_balances.native, self.shared_pda_balances.native);

        // maker[0] receives token_a from taker (hop 1 input side).
        // NativeSol → native increased, otherwise token_a increased.
        if test_mode.maker_accounts.input == AccountKind::NativeSol {
            assert_eq!(nb.makers_balances[0].native.checked_sub(self.makers_balances[0].native), Some(test_mode.input_amounts[0]));
        } else {
            assert_eq!(nb.makers_balances[0].token_a.checked_sub(self.makers_balances[0].token_a), Some(test_mode.input_amounts[0]));
        }
        assert_eq!(self.makers_balances[0].token_c.checked_sub(nb.makers_balances[0].token_c), Some(test_mode.middle_token_info.clone().unwrap().token_amount));
        assert_eq!(nb.makers_balances[0].token_b, self.makers_balances[0].token_b);
        assert_eq!(nb.makers_balances[1].token_c.checked_sub(self.makers_balances[1].token_c), Some(test_mode.middle_token_info.unwrap().token_amount));

        // maker[1] sends token_b to receiver (hop 2 output side).
        // NativeSol → native decreased, otherwise token_b decreased.
        if test_mode.maker_accounts.output == AccountKind::NativeSol {
            assert_eq!(self.makers_balances[1].native.checked_sub(nb.makers_balances[1].native), Some(test_mode.output_amounts[0]));
        } else {
            assert_eq!(self.makers_balances[1].token_b.checked_sub(nb.makers_balances[1].token_b), Some(test_mode.output_amounts[0]));
        }
        assert_eq!(nb.makers_balances[1].token_a, self.makers_balances[1].token_a);
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(nb.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() { assert_eq!(nb.makers_balances[i].native, self.makers_balances[i].native); }
        }
    }

    pub async fn verify_balances_swap_from_pda(&self, env: &TestEnvironment, test_mode: TestMode, onchain_input_amount: u64, onchain_output_amount: u64, final_output_amount: u64) {
        let nb = Self::new(env).await;
        assert_eq!(self.taker_balances.token_c.checked_sub(nb.taker_balances.token_c), Some(onchain_input_amount));

        // Receiver output: NativeSol → check native increase, otherwise token_b increase.
        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount => {
                if test_mode.taker_accounts.output == AccountKind::NativeSol {
                    assert_eq!(nb.taker_balances.native.checked_sub(self.taker_balances.native), Some(final_output_amount));
                } else {
                    assert_eq!(nb.taker_balances.token_b.checked_sub(self.taker_balances.token_b), Some(final_output_amount));
                }
            }
            ReceiverKind::AnotherAddress =>
                assert_eq!(nb.receiver_balances.token_b.checked_sub(self.receiver_balances.token_b), Some(final_output_amount)),
            ReceiverKind::SharedAccount =>
                assert_eq!(nb.shared_pda_balances.token_b.checked_sub(self.shared_pda_balances.token_b), Some(final_output_amount)),
        }
        assert_eq!(nb.taker_balances.token_a, self.taker_balances.token_a);
        assert_eq!(nb.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(nb.receiver_balances.token_c, self.receiver_balances.token_c);
        assert_eq!(nb.receiver_balances.native, self.receiver_balances.native);
        assert_eq!(nb.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(nb.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(nb.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(nb.shared_pda_balances.native, self.shared_pda_balances.native);

        // Maker[0] input: NativeSol → native increased, otherwise token_a increased.
        if test_mode.maker_accounts.input == AccountKind::NativeSol {
            assert_eq!(nb.makers_balances[0].native.checked_sub(self.makers_balances[0].native), Some(onchain_output_amount));
        } else {
            assert_eq!(nb.makers_balances[0].token_a.checked_sub(self.makers_balances[0].token_a), Some(onchain_output_amount));
        }
        // Maker[0] output: NativeSol → native decreased, otherwise token_b decreased.
        if test_mode.maker_accounts.output == AccountKind::NativeSol {
            assert_eq!(self.makers_balances[0].native.checked_sub(nb.makers_balances[0].native), Some(final_output_amount));
        } else {
            assert_eq!(self.makers_balances[0].token_b.checked_sub(nb.makers_balances[0].token_b), Some(final_output_amount));
        }
        assert_eq!(nb.makers_balances[0].token_c, self.makers_balances[0].token_c);
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(nb.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() { assert_eq!(nb.makers_balances[i].native, self.makers_balances[i].native); }
        }
    }

    pub async fn verify_balances_for_swap_then_onchain(&self, env: &TestEnvironment, test_mode: TestMode, onchain_pool_output: u64) {
        let nb = Self::new(env).await;
        let total_input: u64 = test_mode.input_amounts.iter().sum();

        // Taker input: NativeSol → check native decrease, otherwise token_a decrease.
        if test_mode.taker_accounts.input == AccountKind::NativeSol {
            assert_eq!(self.taker_balances.native.checked_sub(nb.taker_balances.native), Some(total_input));
        } else {
            assert_eq!(self.taker_balances.token_a.checked_sub(nb.taker_balances.token_a), Some(total_input));
        }

        match test_mode.receiver_kind {
            ReceiverKind::Taker | ReceiverKind::TakerWithTokenAccount =>
                assert_eq!(nb.taker_balances.token_c.checked_sub(self.taker_balances.token_c), Some(onchain_pool_output)),
            ReceiverKind::AnotherAddress =>
                assert_eq!(nb.receiver_balances.token_c.checked_sub(self.receiver_balances.token_c), Some(onchain_pool_output)),
            ReceiverKind::SharedAccount =>
                assert_eq!(nb.shared_pda_balances.token_c.checked_sub(self.shared_pda_balances.token_c), Some(onchain_pool_output)),
        }
        assert_eq!(nb.taker_balances.token_b, self.taker_balances.token_b);
        assert_eq!(nb.receiver_balances.token_a, self.receiver_balances.token_a);
        assert_eq!(nb.receiver_balances.token_b, self.receiver_balances.token_b);
        assert_eq!(nb.receiver_balances.native, self.receiver_balances.native);
        assert_eq!(nb.shared_pda_balances.token_a, self.shared_pda_balances.token_a);
        assert_eq!(nb.shared_pda_balances.token_b, self.shared_pda_balances.token_b);
        assert_eq!(nb.shared_pda_balances.token_c, self.shared_pda_balances.token_c);
        assert_eq!(nb.shared_pda_balances.native, self.shared_pda_balances.native);
        for i in 0..test_mode.input_amounts.len() {
            // Maker input: NativeSol → native increased, otherwise token_a increased.
            if test_mode.maker_accounts.input == AccountKind::NativeSol {
                assert_eq!(nb.makers_balances[i].native.checked_sub(self.makers_balances[i].native), Some(test_mode.input_amounts[i]));
            } else {
                assert_eq!(nb.makers_balances[i].token_a.checked_sub(self.makers_balances[i].token_a), Some(test_mode.input_amounts[i]));
            }
            // Maker output: NativeSol → native decreased, otherwise token_b decreased.
            if test_mode.maker_accounts.output == AccountKind::NativeSol {
                assert_eq!(self.makers_balances[i].native.checked_sub(nb.makers_balances[i].native), Some(test_mode.output_amounts[i]));
            } else {
                assert_eq!(self.makers_balances[i].token_b.checked_sub(nb.makers_balances[i].token_b), Some(test_mode.output_amounts[i]));
            }
            assert_eq!(nb.makers_balances[i].token_c, self.makers_balances[i].token_c);
        }
        if test_mode.taker_accounts.input != AccountKind::NativeSol && test_mode.taker_accounts.output != AccountKind::NativeSol {
            assert_eq!(nb.taker_balances.native, self.taker_balances.native);
        }
        if test_mode.maker_accounts.input != AccountKind::NativeSol && test_mode.maker_accounts.output != AccountKind::NativeSol {
            for i in 0..env.makers.len() { assert_eq!(nb.makers_balances[i].native, self.makers_balances[i].native); }
        }
    }
}

async fn get_balances(banks_client: &Arc<Mutex<BanksClient>>, wallet: Pubkey, token_a: Option<Pubkey>, token_b: Option<Pubkey>, token_c: Option<Pubkey>) -> Balances {
    Balances {
        native:  get_native_balance(banks_client, wallet).await,
        token_a: get_token_balance(banks_client, token_a).await,
        token_b: get_token_balance(banks_client, token_b).await,
        token_c: get_token_balance(banks_client, token_c).await,
    }
}

async fn get_native_balance(banks_client: &Arc<Mutex<BanksClient>>, wallet: Pubkey) -> u64 {
    banks_client.lock().await.get_account(wallet).await.unwrap().map(|a| a.lamports).unwrap_or(0)
}

pub async fn get_token_balance(banks_client: &Arc<Mutex<BanksClient>>, account: Option<Pubkey>) -> u64 {
    let pubkey = match account { None => return 0, Some(pk) => pk };
    match banks_client.lock().await.get_account(pubkey).await.unwrap() {
        None => 0,
        Some(acc) => TokenAccount::unpack(&acc.data[..TokenAccount::LEN]).map(|a| a.amount).unwrap_or(0),
    }
}

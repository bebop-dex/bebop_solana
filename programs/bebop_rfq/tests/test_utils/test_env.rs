
use std::{any::Any, sync::Arc};

use anchor_lang::{
    prelude::*,
    solana_program::{self, instruction::Instruction},
    system_program, InstructionData,
};
use anchor_spl::{associated_token::spl_associated_token_account::instruction, token::{self, spl_token::{instruction::sync_native, native_mint}}};
use assert_matches::assert_matches;
use bebop_rfq::bebop_rfq::AmountWithExpiry;
use itertools::Itertools;
use solana_program_test::{
    tokio::{self, sync::Mutex},
    BanksClient, BanksClientError, ProgramTest,
};
use solana_sdk::{
    feature_set::bpf_account_data_direct_mapping, message::Message, native_token::LAMPORTS_PER_SOL, signature::{Keypair, Signature}, signer::Signer, system_instruction, transaction::{Transaction, TransactionError}
};
use spl_token_client::{
    client::{
        ProgramBanksClient, ProgramBanksClientProcessTransaction, SendTransaction,
        SimulateTransaction,
    },
    token::{ExtensionInitializationParams, Token},
};

pub struct TestEnvironment {
    pub banks_client: Arc<Mutex<BanksClient>>,
    pub payer: Arc<Keypair>,
    pub taker_keypair: Keypair,
    pub makers_keypairs: Vec<Keypair>,

    pub makers: Vec<Pubkey>,
    pub taker: Pubkey,
    pub random_receiver: Pubkey,
    pub shared_pda: Pubkey,

    pub taker_token_a_account: Option<Pubkey>,
    pub makers_token_a_account: Vec<Pubkey>,  // empty array means None for all
    pub shared_token_a_account: Option<Pubkey>,
    pub receiver_token_a_account: Option<Pubkey>,

    pub taker_token_b_account: Option<Pubkey>,
    pub makers_token_b_account: Vec<Pubkey>,  // empty array means None for all
    pub shared_token_b_account: Option<Pubkey>,
    pub receiver_token_b_account: Option<Pubkey>,

    pub taker_token_c_account: Option<Pubkey>,
    pub makers_token_c_account: Vec<Pubkey>,  // empty array means None for all
    pub shared_token_c_account: Option<Pubkey>,
    pub receiver_token_c_account: Option<Pubkey>,

    pub token_a: Token<ProgramBanksClientProcessTransaction>,
    pub token_a_mint: Pubkey,
    pub token_a_program_id: Pubkey,
    pub token_b: Token<ProgramBanksClientProcessTransaction>,
    pub token_b_mint: Pubkey,
    pub token_b_program_id: Pubkey,
    pub token_c: Token<ProgramBanksClientProcessTransaction>,
    pub token_c_mint: Pubkey,
    pub token_c_program_id: Pubkey,

    pub temporary_wsol_token_accounts: Vec<Pubkey>, // empty array means None for all
}


impl TestEnvironment {

    pub async fn create_single_swap_instructions(&self, test_mode: TestMode, mint_taker_balance: bool) -> Vec<Instruction> {
        // token_a -> token_b swap (taker pov)

        let TestEnvironment {
            banks_client,
            makers,
            taker,
            random_receiver,
            temporary_wsol_token_accounts,
            payer,
            shared_pda,
            token_a,
            token_b,
            token_a_mint,
            token_a_program_id,
            token_b_mint,
            token_b_program_id,

            shared_token_a_account,
            taker_token_a_account,
            makers_token_a_account,

            taker_token_b_account,
            receiver_token_b_account,
            shared_token_b_account,
            makers_token_b_account,

            ..
        } = self;

        let (cur_receiver_address, cur_receiver_token_b_account) = match test_mode.receiver_kind {
            ReceiverKind::Taker => (taker, taker_token_b_account),
            ReceiverKind::TakerWithTokenAccount => {
                get_associated_token_account(
                    *taker, &token_b, test_mode.taker_accounts.output.clone(), true
                ).await;
                (taker, taker_token_b_account)
            }
            ReceiverKind::AnotherAddress => (random_receiver, receiver_token_b_account),
            ReceiverKind::SharedAccount => (shared_pda, shared_token_b_account),
        };
        let mut instructions = Vec::new();
        if test_mode.receiver_kind != ReceiverKind::TakerWithTokenAccount {
            instructions.push(instruction::create_associated_token_account(
                &payer.pubkey(), cur_receiver_address, token_b_mint, token_b_program_id
            ));
        }
        if mint_taker_balance {
            mint_balance(test_mode.input_amounts.iter().sum(), *taker_token_a_account,
         token_a, test_mode.clone().taker_accounts.input, banks_client, payer).await;
        }
        for (i, amount) in test_mode.output_amounts.iter().enumerate() {
            mint_balance(*amount, if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(i).cloned() },
             token_b, test_mode.clone().maker_accounts.output, banks_client, payer).await;
        }

        for i in 0..test_mode.input_amounts.len() {
            assert_eq!(test_mode.input_amounts.len(), test_mode.output_amounts.len());
            
            let data = bebop_rfq::instruction::Swap {
                input_amount: test_mode.input_amounts[i],
                output_amounts: vec![ AmountWithExpiry {
                    amount: test_mode.output_amounts[i],
                    expiry: u64::MAX,
                }],
                event_id: 0
            }
            .data();

            let accs = bebop_rfq::accounts::Swap {
                maker: makers[i],
                taker: if test_mode.use_shared_taker {*shared_pda} else {*taker},
                receiver: *cur_receiver_address,
                taker_input_mint_token_account: if test_mode.use_shared_taker {*shared_token_a_account} else {*taker_token_a_account},
                maker_input_mint_token_account: if makers_token_a_account.is_empty() { None } else { makers_token_a_account.get(i).cloned() },
                receiver_output_mint_token_account: *cur_receiver_token_b_account,
                maker_output_mint_token_account: if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(i).cloned() },
                input_mint: *token_a_mint,
                input_token_program: *token_a_program_id,
                output_mint: *token_b_mint,
                output_token_program: *token_b_program_id,
                system_program: system_program::ID,
            };
            let mut instruction = Instruction {
                program_id: bebop_rfq::ID,
                accounts: accs.to_account_metas(None),
                data,
            };
            if !test_mode.use_shared_taker {
                instruction
                    .accounts
                    .iter_mut()
                    .for_each(|account| if account.pubkey == *taker { account.is_signer = true });
            }
            if temporary_wsol_token_accounts.len() > 0 {
                instruction
                    .accounts
                    .push(AccountMeta::new(temporary_wsol_token_accounts[i].clone(), false));
            }
            instructions.push(instruction);
        }
        instructions
    }

    pub async fn create_2_hops_instructions(&self, test_mode: TestMode) -> Vec<Instruction> {
        // token_a -> token_c -> token_b
        // token_a <-> token_c (taker=taker, maker=maker1, receiver=shared_pda)
        // token_c <-> token_b (taker=shared_pda, maker=maker2, receiver=taker)

        let TestEnvironment {
            banks_client,
            makers,
            taker,
            random_receiver,
            temporary_wsol_token_accounts,
            payer,
            shared_pda,
            token_a,
            token_b,
            token_c,
            token_a_mint,
            token_a_program_id,
            token_b_mint,
            token_b_program_id,
            token_c_mint,
            token_c_program_id,

            taker_token_a_account,
            makers_token_a_account,

            taker_token_b_account,
            receiver_token_b_account,
            makers_token_b_account,
            shared_token_b_account,

            shared_token_c_account,
            makers_token_c_account,

            ..
        } = self;

        let (cur_receiver_address, cur_receiver_token_b_account) = match test_mode.receiver_kind {
            ReceiverKind::Taker => (taker, taker_token_b_account),
            ReceiverKind::TakerWithTokenAccount => {
                get_associated_token_account(
                    *taker, &token_b, test_mode.taker_accounts.output.clone(), true
                ).await;
                (taker, taker_token_b_account)
            }
            ReceiverKind::AnotherAddress => (random_receiver, receiver_token_b_account),
            ReceiverKind::SharedAccount => (shared_pda, shared_token_b_account),
        };
        let mut instructions = Vec::new();
        if test_mode.receiver_kind != ReceiverKind::TakerWithTokenAccount {
            instructions.push(instruction::create_associated_token_account(
                &payer.pubkey(), cur_receiver_address, token_b_mint, token_b_program_id
            ));
        }
        let middle_amount = test_mode.middle_token_info.clone().unwrap().token_amount;

        mint_balance(test_mode.input_amounts.iter().sum(), *taker_token_a_account,
         token_a, test_mode.clone().taker_accounts.input, banks_client, payer).await;
        mint_balance(middle_amount, if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(0).cloned() },
         token_c, AccountKind::Token, banks_client, payer).await;
        mint_balance(test_mode.output_amounts.iter().sum(), if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(1).cloned() },
         token_b, test_mode.clone().maker_accounts.output, banks_client, payer).await;
        assert_eq!(test_mode.input_amounts.len(), 1);
        assert_eq!(test_mode.input_amounts.len(), test_mode.output_amounts.len());

        let data_1 = bebop_rfq::instruction::Swap {
            input_amount: test_mode.input_amounts[0],
            output_amounts: vec![ AmountWithExpiry {
                amount: middle_amount,
                expiry: u64::MAX,
            }],
            event_id: 0
        }.data();
        let mut instruction_1 = Instruction {
            program_id: bebop_rfq::ID,
            accounts: bebop_rfq::accounts::Swap {
                maker: makers[0],
                taker: *taker,
                receiver: *shared_pda,
                taker_input_mint_token_account: *taker_token_a_account,
                maker_input_mint_token_account: if makers_token_a_account.is_empty() { None } else { makers_token_a_account.get(0).cloned() },
                receiver_output_mint_token_account: *shared_token_c_account,
                maker_output_mint_token_account: if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(0).cloned() },
                input_mint: *token_a_mint,
                input_token_program: *token_a_program_id,
                output_mint: *token_c_mint,
                output_token_program: *token_c_program_id,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: data_1,
        };
        instruction_1
            .accounts
            .iter_mut()
            .for_each(|account| if account.pubkey == *taker { account.is_signer = true });
        if temporary_wsol_token_accounts.len() > 0 {
            instruction_1
                .accounts
                .push(AccountMeta::new(temporary_wsol_token_accounts[0].clone(), false));
        }

        let data_2 = bebop_rfq::instruction::Swap {
            input_amount: middle_amount,
            output_amounts: vec![AmountWithExpiry {
                amount: test_mode.output_amounts[0],
                expiry: u64::MAX,
            }],
            event_id: 0
        }.data();
        let mut instruction_2 = Instruction {
            program_id: bebop_rfq::ID,
            accounts: bebop_rfq::accounts::Swap {
                maker: makers[1],
                taker: *shared_pda,
                receiver: *cur_receiver_address,
                taker_input_mint_token_account: *shared_token_c_account,
                maker_input_mint_token_account: if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(1).cloned() },
                receiver_output_mint_token_account: *cur_receiver_token_b_account,
                maker_output_mint_token_account: if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(1).cloned() },
                input_mint: *token_c_mint,
                input_token_program: *token_c_program_id,
                output_mint: *token_b_mint,
                output_token_program: *token_b_program_id,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: data_2,
        };
        if temporary_wsol_token_accounts.len() > 0 {
            instruction_2
                .accounts
                .push(AccountMeta::new(temporary_wsol_token_accounts[1].clone(), false));
        }
        instructions.push(instruction_1);
        instructions.push(instruction_2);
        instructions
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum AccountKind {
    #[default]
    Token,
    NativeMint,
    NativeSol,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum ReceiverKind {
    #[default]
    Taker,
    TakerWithTokenAccount,
    AnotherAddress,
    SharedAccount,
}

#[derive(Default, Clone, Debug)]
pub struct Accounts {
    pub input: AccountKind,
    pub output: AccountKind,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum OnchainSwapType {
    #[default]
    RaydiumCPMM,
    RaydiumCLMM,
    MeteoraDLMM
}

#[derive(Clone, Debug)]
pub struct TestMode {
    pub input_amounts: Vec<u64>,
    pub output_amounts: Vec<u64>,
    pub taker_accounts: Accounts,
    pub maker_accounts: Accounts,
    pub receiver_kind: ReceiverKind,
    pub use_shared_taker: bool,
    pub middle_token_info: Option<MiddleTokenInfo>,
    pub expected_error: Option<TransactionError>,
    pub input_mint_extensions: Option<Vec<ExtensionInitializationParams>>,
    pub output_mint_extensions: Option<Vec<ExtensionInitializationParams>>,
    pub onchain_swap_type: Option<OnchainSwapType>,
}

impl Default for TestMode {
    fn default() -> Self {
        Self {
            input_amounts: vec![1_000_000_000],
            output_amounts: vec![2_000_000_000],
            taker_accounts: Accounts {
                input: AccountKind::Token,
                output: AccountKind::Token,
            },
            maker_accounts: Accounts {
                input: AccountKind::Token,
                output: AccountKind::Token,
            },
            receiver_kind: ReceiverKind::Taker,
            use_shared_taker: false,
            middle_token_info: None,
            expected_error: None,
            input_mint_extensions: None,
            output_mint_extensions: None,
            onchain_swap_type: None
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct MiddleTokenInfo {
    pub token_amount: u64,
    pub mint_extensions: Option<Vec<ExtensionInitializationParams>>,
}


/// Workaround from anchor issue https://github.com/coral-xyz/anchor/issues/2738#issuecomment-2230683481
#[macro_export]
macro_rules! anchor_processor {
    ($program:ident) => {{
        fn entry(
            program_id: &solana_program::pubkey::Pubkey,
            accounts: &[solana_program::account_info::AccountInfo],
            instruction_data: &[u8],
        ) -> solana_program::entrypoint::ProgramResult {
            let accounts = Box::leak(Box::new(accounts.to_vec()));

            $program::entry(program_id, accounts, instruction_data)
        }

        solana_program_test::processor!(entry)
    }};
}


const TEST_AIRDROP: u64 = 5 * LAMPORTS_PER_SOL;

pub async fn prepare_test(test_mode: TestMode) -> TestEnvironment {
    let mut pt = ProgramTest::new(
        "bebop_rfq",
        bebop_rfq::ID,
        anchor_processor!(bebop_rfq),
    );
    pt.add_program("mock_swap", mock_swap::ID, anchor_processor!(mock_swap));
    pt.deactivate_feature(bpf_account_data_direct_mapping::ID);

    let (banks_client, payer, _) = pt.start().await;

    let taker_keypair = Keypair::new();
    let taker = taker_keypair.pubkey();
    let random_receiver = Keypair::new().pubkey();

    let shared_pda = Pubkey::find_program_address(
        &[bebop_rfq::SHARED_ACCOUNT],
        &bebop_rfq::ID,
    ).0;

    let mut makers_keypairs: Vec<Keypair> = Vec::new();
    let mut makers: Vec<Pubkey> = Vec::new();
    assert_eq!(test_mode.input_amounts.len(), test_mode.output_amounts.len());
    for _ in 0..5 {
        let cur_keypair = Keypair::new();
        makers.push(cur_keypair.pubkey());
        makers_keypairs.push(cur_keypair);
    }
    let payer = Arc::new(payer);

    let banks_client = Arc::new(Mutex::new(banks_client));
    let client = Arc::new(ProgramBanksClient::new_from_client(
        banks_client.clone(),
        ProgramBanksClientProcessTransaction,
    ));

    // Fund the taker and the maker
    let mut airdrop_instructions = vec![system_instruction::transfer(&payer.pubkey(), &taker, TEST_AIRDROP)];
    airdrop_instructions.extend(
        makers
            .iter()
            .map(|maker| system_instruction::transfer(&payer.pubkey(), maker, TEST_AIRDROP)),
    );
    process_and_assert_ok(
        airdrop_instructions.as_slice(),
        &payer,
        &[&payer],
        &banks_client,
    )
    .await;

    let (mut mint_a_keypair, mut mint_a, mut mint_b_keypair, mut mint_b, mint_c_keypair, mint_c) = {
        let mint_a_keypair = Keypair::new();
        let mint_a = mint_a_keypair.pubkey();
        let mint_b_keypair = Keypair::new();
        let mint_b = mint_b_keypair.pubkey();
        let mint_c_keypair = Keypair::new();
        let mint_c = mint_c_keypair.pubkey();
        (Some(mint_a_keypair), mint_a, Some(mint_b_keypair), mint_b, Some(mint_c_keypair), mint_c)
    };
    let mut uses_temporary_wsol_token_account = false;

    let TestMode {
        input_amounts,
        output_amounts,
        taker_accounts,
        maker_accounts,
        receiver_kind,
        use_shared_taker,
        middle_token_info,
        expected_error,
        input_mint_extensions,
        output_mint_extensions,
        onchain_swap_type
    } = test_mode;
    match (&taker_accounts, &maker_accounts) {
        (
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::Token,
            },
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::Token,
            },
        ) => (),
        (
            Accounts {
                input: AccountKind::NativeSol,
                output: AccountKind::Token,
            },
            Accounts {
                input: AccountKind::NativeSol,
                output: AccountKind::Token,
            },
        )
        | (
            Accounts {
                input: AccountKind::NativeMint,
                output: AccountKind::Token,
            },
            Accounts {
                input: AccountKind::NativeMint,
                output: AccountKind::Token,
            },
        ) => {
            mint_a_keypair = None;
            mint_a = native_mint::ID;
        }
        (
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeSol,
            },
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeSol,
            },
        )
        | (
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeMint,
            },
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeMint,
            },
        ) => {
            mint_b_keypair = None;
            mint_b = native_mint::ID;
        }
        (
            Accounts {
                input: AccountKind::NativeMint,
                output: AccountKind::Token,
            },
            Accounts {
                input: AccountKind::NativeSol,
                output: AccountKind::Token,
            },
        )
        | (
            Accounts {
                input: AccountKind::NativeSol,
                output: AccountKind::Token,
            },
            Accounts {
                input: AccountKind::NativeMint,
                output: AccountKind::Token,
            },
        ) => {
            mint_a_keypair = None;
            mint_a = native_mint::ID;
            uses_temporary_wsol_token_account = true;
        }
        (
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeMint,
            },
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeSol,
            },
        )
        | (
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeSol,
            },
            Accounts {
                input: AccountKind::Token,
                output: AccountKind::NativeMint,
            },
        ) => {
            mint_b_keypair = None;
            mint_b = native_mint::ID;
            uses_temporary_wsol_token_account = true;
        }
        _ => panic!("Invalid combo"),
    };

    let (token_a_program_id, token_a) = create_token(
        Arc::clone(&client), Arc::clone(&payer), &mint_a, mint_a_keypair, input_mint_extensions
    ).await;
    let (token_b_program_id, token_b) = create_token(
        Arc::clone(&client), Arc::clone(&payer), &mint_b, mint_b_keypair, output_mint_extensions
    ).await;
    let (token_c_program_id, token_c) = create_token(
        Arc::clone(&client), Arc::clone(&payer), &mint_c, mint_c_keypair, middle_token_info.and_then(|x| x.mint_extensions)
    ).await;


    let taker_token_a_account: Option<Pubkey> = get_associated_token_account(
        taker, &token_a, taker_accounts.input.clone(), true
    ).await;
    let taker_token_b_account: Option<Pubkey> = get_associated_token_account(
        taker, &token_b, taker_accounts.output.clone(), false
    ).await;
    let taker_token_c_account: Option<Pubkey> = get_associated_token_account(
        taker, &token_c, AccountKind::Token, false
    ).await;

    let shared_token_a_account: Option<Pubkey> = get_associated_token_account(shared_pda, &token_a, taker_accounts.input.clone(), true).await;
    let shared_token_b_account: Option<Pubkey> = get_associated_token_account(shared_pda, &token_b, taker_accounts.output.clone(), true).await;
    let shared_token_c_account: Option<Pubkey> = get_associated_token_account(shared_pda, &token_c, AccountKind::Token, true).await;

    let receiver_token_a_account: Option<Pubkey> = get_associated_token_account(random_receiver, &token_a, taker_accounts.input.clone(), false).await;
    let receiver_token_b_account: Option<Pubkey> = get_associated_token_account(random_receiver, &token_b, taker_accounts.output.clone(), false).await;
    let receiver_token_c_account: Option<Pubkey> = get_associated_token_account(random_receiver, &token_c, AccountKind::Token, false).await;
    let mut makers_token_a_account: Vec<Pubkey> = Vec::new();
    let mut makers_token_b_account: Vec<Pubkey> = Vec::new();
    let mut makers_token_c_account: Vec<Pubkey> = Vec::new();
    for (i, maker) in makers.iter().enumerate() {
        match get_associated_token_account(*maker, &token_a, maker_accounts.input.clone(), true).await {
            Some(cur_key) => makers_token_a_account.push(cur_key),
            None => {},
        }
        match get_associated_token_account(*maker, &token_b, maker_accounts.output.clone(),true).await {
            Some(cur_key) => makers_token_b_account.push(cur_key),
            None => {},
        }
        match get_associated_token_account(*maker, &token_c, AccountKind::Token,true).await {
            Some(cur_key) => makers_token_c_account.push(cur_key),
            None => {},
        }
    }

    let temporary_wsol_token_accounts: Vec<Pubkey> = if uses_temporary_wsol_token_account {
        makers.iter().map(|m| {
            Pubkey::find_program_address(
                &[bebop_rfq::TEMPORARY_WSOL_TOKEN_ACCOUNT, m.as_ref()],
                &bebop_rfq::ID,
            )
            .0
        }).collect()
    } else {
        Vec::new()
    };

    TestEnvironment {
        banks_client,
        payer,
        taker_keypair,
        makers_keypairs,
        makers,
        taker,
        random_receiver,
        shared_pda,

        taker_token_a_account,
        makers_token_a_account,
        shared_token_a_account,
        receiver_token_a_account,
        taker_token_b_account,
        makers_token_b_account,
        shared_token_b_account,
        receiver_token_b_account,
        taker_token_c_account,
        makers_token_c_account,
        shared_token_c_account,
        receiver_token_c_account,

        token_a,
        token_a_mint: mint_a,
        token_a_program_id,
        token_b,
        token_b_mint: mint_b,
        token_b_program_id,
        token_c,
        token_c_mint: mint_c,
        token_c_program_id,

        temporary_wsol_token_accounts
        
    }
}

async fn create_token(
    client: Arc<ProgramBanksClient<ProgramBanksClientProcessTransaction>>, payer: Arc<Keypair>,
    token_mint: &Pubkey, token_mint_keypair: Option<Keypair>, token_mint_extensions: Option<Vec<ExtensionInitializationParams>>
) -> (Pubkey, Token<ProgramBanksClientProcessTransaction>) {
    let token_program_id = if token_mint_extensions.is_some() {
        anchor_spl::token_2022::ID
    } else {
        anchor_spl::token::ID
    };
    let new_token = Token::new(
        client.clone(),
        &token_program_id,
        &token_mint,
        Some(9),
        payer.clone(),
    );
    if let Some(mint_a_keypair) = token_mint_keypair {
        new_token
            .create_mint(
                &payer.pubkey(),
                None,
                token_mint_extensions.unwrap_or_default(),
                &[mint_a_keypair],
            )
            .await
            .unwrap();
    }
    (token_program_id, new_token)
}


pub async fn get_associated_token_account(
    wallet: Pubkey, token: &Token<ProgramBanksClientProcessTransaction>,  kind: AccountKind, create_token: bool
) -> Option<Pubkey> {
    let ata = token.get_associated_token_address(&wallet);
    let set_ata = match kind {
        AccountKind::Token => {
            if create_token {
                token.create_associated_token_account(&wallet).await.unwrap();
            }
            true
        }
        AccountKind::NativeMint => {
            if create_token {
                token.create_associated_token_account(&wallet).await.unwrap();
            }
            true
        }
        AccountKind::NativeSol => {
            false
        }
    };
    if set_ata {
        return Some(ata);
    }
    None
}


pub async fn mint_balance(amount: u64, wallet_token_account: Option<Pubkey>, token: &Token<ProgramBanksClientProcessTransaction>, kind: AccountKind, banks_client: &Mutex<BanksClient>, payer: &Keypair) {
    match kind {
        AccountKind::Token => {
            token.mint_to(&wallet_token_account.unwrap(), &payer.pubkey(), amount, &[&payer])
                .await
                .unwrap();
        }
        AccountKind::NativeMint => {
            println!("Minting to wallet: {:?}, {:?}", wallet_token_account, token);
            // Send enough
            process_and_assert_ok(
                &[system_instruction::transfer(
                    &payer.pubkey(),
                    &wallet_token_account.unwrap(),
                    amount + 100_000_000,
                ), sync_native(&anchor_spl::token::ID, &wallet_token_account.unwrap()).unwrap()],
                &payer,
                &[&payer],
                &banks_client,
            )
            .await;
        }
        AccountKind::NativeSol => {
            // we already have SOL balance
        }
    }
}


pub async fn process_and_assert_ok(
    instructions: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
    banks_client: &Mutex<BanksClient>,
) {
    let result = process_instructions(instructions, payer, signers, banks_client).await;
    assert_matches!(result, Ok(()));
}
pub async fn process_instructions(
    instructions: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
    banks_client: &Mutex<BanksClient>,
) -> std::result::Result<(), BanksClientError> {
    let mut banks_client = banks_client.lock().await;
    let recent_blockhash = banks_client.get_latest_blockhash().await.unwrap();

    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);

    let tx = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        &all_signers,
        recent_blockhash,
    );

    banks_client.process_transaction(tx).await
}

pub async fn sign_and_execute_tx(
    instructions: &[Instruction],
    payer: &Keypair,
    taker: &Keypair,
    makers: &[Keypair],
    banks_client: &Mutex<BanksClient>,
) -> std::result::Result<(), BanksClientError> {
    // assert_eq!(instructions.len(), makers.len(), "Instructions and makers length mismatch");

    let mut banks_client = banks_client.lock().await;
    let recent_blockhash = banks_client.get_latest_blockhash().await.unwrap();

    // Create the main transaction with taker and payer as signers
    let msg = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &recent_blockhash);
    let mut tx = Transaction::new_unsigned(msg);

    tx.message.recent_blockhash = recent_blockhash;
    let mut signatures: Vec<(Pubkey, Signature)> = vec![];
    for signer in makers.iter().chain(std::iter::once(taker)).chain(std::iter::once(payer)) {
        let signature = signer.try_sign_message(&tx.message_data()).unwrap();
        signatures.push((signer.pubkey(), signature));
    }
    tx.replace_signatures(&signatures)?;
    // println!("tx:{:?}", tx);
    println!("Final TX size: {}", bincode::serialize(&tx).unwrap().len());
    banks_client.process_transaction(tx).await?;

    Ok(())
}


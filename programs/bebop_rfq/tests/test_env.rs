
use std::sync::Arc;

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


pub struct TestEnvironment {
    pub banks_client: Arc<Mutex<BanksClient>>,
    pub payer: Arc<Keypair>,
    pub taker_keypair: Keypair,
    pub makers_keypairs: Vec<Keypair>,
    pub input_amounts: Vec<u64>,
    pub output_amounts: Vec<u64>,
    // accounts
    pub makers: Vec<Pubkey>,
    pub taker: Pubkey,
    pub taker_input_mint_token_account: Option<Pubkey>,
    pub makers_input_mint_token_account: Vec<Pubkey>,  // empty array means None for all
    pub taker_output_mint_token_account: Option<Pubkey>,
    pub makers_output_mint_token_account: Vec<Pubkey>, // empty array means None for all
    pub input_mint: Pubkey,
    pub input_token_program: Pubkey,
    pub output_mint: Pubkey,
    pub output_token_program: Pubkey,
    pub temporary_wsol_token_accounts: Vec<Pubkey>, // empty array means None for all
    pub input_token: Token<ProgramBanksClientProcessTransaction>,
    pub output_token: Token<ProgramBanksClientProcessTransaction>,
}


impl TestEnvironment {

    pub fn create_swap_instructions(&self) -> Vec<Instruction> {
        let TestEnvironment {
            input_amounts,
            output_amounts,
            makers,
            taker,
            taker_input_mint_token_account,
            makers_input_mint_token_account,
            taker_output_mint_token_account,
            makers_output_mint_token_account,
            input_mint,
            input_token_program,
            output_mint,
            output_token_program,
            temporary_wsol_token_accounts,
            ..
        } = self;
        let mut instructions = Vec::new();
        for i in 0..input_amounts.len() {
            assert_eq!(input_amounts.len(), output_amounts.len());
            
            let data = bebop_rfq::instruction::Swap {
                input_amount: input_amounts[i],
                output_amount: output_amounts[i],
                expire_at: i64::MAX,
            }
            .data();

            let mut instruction = Instruction {
                program_id: bebop_rfq::ID,
                accounts: bebop_rfq::accounts::Swap {
                    maker: makers[i],
                    taker: *taker,
                    taker_input_mint_token_account: *taker_input_mint_token_account,
                    maker_input_mint_token_account: if makers_input_mint_token_account.is_empty() { None } else { makers_input_mint_token_account.get(i).cloned() },
                    taker_output_mint_token_account: *taker_output_mint_token_account,
                    maker_output_mint_token_account: if makers_output_mint_token_account.is_empty() { None } else { makers_output_mint_token_account.get(i).cloned() },
                    input_mint: *input_mint,
                    input_token_program: *input_token_program,
                    output_mint: *output_mint,
                    output_token_program: *output_token_program,
                    system_program: system_program::ID,
                }
                .to_account_metas(None),
                data,
            };
            if temporary_wsol_token_accounts.len() > 0 {
                instruction
                    .accounts
                    .push(AccountMeta::new(temporary_wsol_token_accounts[i].clone(), false));
            }
            instructions.push(instruction);
        }
        instructions
    }
}

#[derive(Default, Debug, Clone)]
pub enum AccountKind {
    #[default]
    Token,
    NativeMint,
    NativeSol,
}

#[derive(Default, Clone, Debug)]
pub struct Accounts {
    pub input: AccountKind,
    pub output: AccountKind,
}

#[derive(Default, Clone, Debug)]
pub struct TestMode {
    pub input_amounts: Vec<u64>,
    pub output_amounts: Vec<u64>,
    pub taker_accounts: Accounts,
    pub maker_accounts: Accounts,
    pub expected_error: Option<TransactionError>,
    pub input_mint_extensions: Option<Vec<ExtensionInitializationParams>>,
    pub output_mint_extensions: Option<Vec<ExtensionInitializationParams>>,
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
    pt.deactivate_feature(bpf_account_data_direct_mapping::ID);

    let (banks_client, payer, _) = pt.start().await;

    let taker_keypair = Keypair::new();
    let taker = taker_keypair.pubkey();

    let mut makers_keypairs: Vec<Keypair> = Vec::new();
    let mut makers: Vec<Pubkey> = Vec::new();
    assert_eq!(test_mode.input_amounts.len(), test_mode.output_amounts.len());
    for _ in 0..test_mode.input_amounts.len() {
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

    let (mut mint_a_keypair, mut mint_a, mut mint_b_keypair, mut mint_b) = {
        let mint_a_keypair = Keypair::new();
        let mint_a = mint_a_keypair.pubkey();
        let mint_b_keypair = Keypair::new();
        let mint_b = mint_b_keypair.pubkey();
        (Some(mint_a_keypair), mint_a, Some(mint_b_keypair), mint_b)
    };

    let mut uses_temporary_wsol_token_account = false;

    let TestMode {
        input_amounts,
        output_amounts,
        taker_accounts,
        maker_accounts,
        expected_error,
        input_mint_extensions,
        output_mint_extensions,
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

    // Setup 2 mints
    let token_a_program_id = if input_mint_extensions.is_some() {
        anchor_spl::token_2022::ID
    } else {
        anchor_spl::token::ID
    };
    let token_a = Token::new(
        client.clone(),
        &token_a_program_id,
        &mint_a,
        Some(9),
        payer.clone(),
    );
    if let Some(mint_a_keypair) = &mint_a_keypair {
        token_a
            .create_mint(
                &payer.pubkey(),
                None,
                input_mint_extensions.unwrap_or_default(),
                &[mint_a_keypair],
            )
            .await
            .unwrap();
    }

    let token_b_program_id = if output_mint_extensions.is_some() {
        anchor_spl::token_2022::ID
    } else {
        anchor_spl::token::ID
    };
    let token_b = Token::new(
        client.clone(),
        &token_b_program_id,
        &mint_b,
        Some(9),
        payer.clone(),
    );
    if let Some(mint_b_keypair) = &mint_b_keypair {
        token_b
            .create_mint(
                &payer.pubkey(),
                None,
                output_mint_extensions.unwrap_or_default(),
                &[mint_b_keypair],
            )
            .await
            .unwrap();
    }
    

    let full_taker_amount = input_amounts.iter().sum::<u64>();
    let taker_input_mint_token_account: Option<Pubkey> = create_associated_token_account(taker, &token_a, Some(full_taker_amount), taker_accounts.input, &payer, &banks_client).await;
    let taker_output_mint_token_account: Option<Pubkey> = create_associated_token_account(taker, &token_b, None, taker_accounts.output, &payer, &banks_client).await;
    
    let mut makers_input_mint_token_account: Vec<Pubkey> = Vec::new();
    let mut makers_output_mint_token_account: Vec<Pubkey> = Vec::new();
    for (i, maker) in makers.iter().enumerate() {
        match create_associated_token_account(*maker, &token_a, None, maker_accounts.input.clone(), &payer, &banks_client).await {
            Some(cur_key) => makers_input_mint_token_account.push(cur_key),
            None => {},
        }
        match create_associated_token_account(*maker, &token_b, Some(output_amounts[i]), maker_accounts.output.clone(), &payer, &banks_client).await {
            Some(cur_key) => makers_output_mint_token_account.push(cur_key),
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
        input_amounts,
        output_amounts,
        makers,
        taker,
        taker_input_mint_token_account,
        makers_input_mint_token_account,
        taker_output_mint_token_account,
        makers_output_mint_token_account,
        input_mint: *token_a.get_address(),
        input_token_program: token_a_program_id,
        output_mint: *token_b.get_address(),
        output_token_program: token_b_program_id,
        input_token: token_a,
        output_token: token_b,
        temporary_wsol_token_accounts,
    }
}

async fn create_associated_token_account(
    wallet: Pubkey, token: &Token<ProgramBanksClientProcessTransaction>, amount: Option<u64>,
    kind: AccountKind, payer: &Keypair,banks_client: &Mutex<BanksClient>
) -> Option<Pubkey> {
    println!("{wallet} {} {:?}", token.get_address(), kind);
    let ata = token.get_associated_token_address(&wallet);
    let set_ata = match (amount, kind) {
        (amount, AccountKind::Token) => {
            token.create_associated_token_account(&wallet).await.unwrap();

            if let Some(amount) = amount {
                token
                    .mint_to(&ata, &payer.pubkey(), amount, &[&payer])
                    .await
                    .unwrap();
            }
            true
        }
        (None, AccountKind::NativeMint) => {
            token.create_associated_token_account(&wallet).await.unwrap();
            true
        }
        (Some(amount), AccountKind::NativeMint) => {
            // Send enough
            process_and_assert_ok(
                &[system_instruction::transfer(
                    &payer.pubkey(),
                    &ata,
                    amount + 100_000_000,
                )],
                &payer,
                &[&payer],
                &banks_client,
            )
            .await;
            token.create_associated_token_account(&wallet).await.unwrap();
            true
        }
        (_, AccountKind::NativeSol) => {
            // Nothing to setup
            false
        }
    };
    if set_ata {
        return Some(ata);
    }
    None
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

    // println!("TX size: {}", bincode::serialize(&tx).unwrap().len());

    banks_client.process_transaction(tx).await
}

pub async fn sign_and_execute_tx(
    instructions: &[Instruction],
    payer: &Keypair,
    taker: &Keypair,
    makers: &[Keypair],
    banks_client: &Mutex<BanksClient>,
) -> std::result::Result<(), BanksClientError> {
    assert_eq!(instructions.len(), makers.len(), "Instructions and makers length mismatch");

    let mut banks_client = banks_client.lock().await;
    let recent_blockhash = banks_client.get_latest_blockhash().await.unwrap();

    // Create the main transaction with taker and payer as signers
    let mut tx = Transaction::new_with_payer(instructions, Some(&payer.pubkey()));
    tx.partial_sign(&[taker], recent_blockhash);
    tx.partial_sign(makers, recent_blockhash);
    tx.partial_sign(&[payer], recent_blockhash);

    println!("tx:{:?}", tx);

    // // Collect additional signatures from makers
    // for (i, instruction) in instructions.iter().enumerate() {
    //     // let mut maker_tx = Transaction::new_with_payer(&[instruction.clone()], Some(&payer.pubkey()));
    //     let mut maker_tx = tx.clone();
    //     maker_tx.message.instructions = vec![tx.message.instructions[i].clone()];
    //     println!("mtx:{:?}", maker_tx);
    //     let signature = makers[i].try_sign_message(&maker_tx.message_data()).unwrap();
    //     println!("sig:{:?}", signature);

    //     let tx_pos = tx.get_signing_keypair_positions(&[makers[i].pubkey()]).clone().unwrap()[0].unwrap();
    //     let maker_tx_pos = maker_tx.get_signing_keypair_positions(&[makers[i].pubkey()]).clone().unwrap()[0].unwrap();
    //     println!("tx_pos:{:?} maker_tx_pos:{:?}", tx_pos, maker_tx_pos);
    //     tx.signatures[tx_pos] = signature;

    //     // maker_tx.partial_sign(&[&makers[i]], recent_blockhash);
    //     // println!("mtx2:{:?}", maker_tx);
    //     // let tx_pos = tx.get_signing_keypair_positions(&[makers[i].pubkey()]).clone().unwrap()[0].unwrap();
    //     // let maker_tx_pos = maker_tx.get_signing_keypair_positions(&[makers[i].pubkey()]).clone().unwrap()[0].unwrap();
    //     // println!("tx_pos:{:?} maker_tx_pos:{:?}", tx_pos, maker_tx_pos);
    //     // tx.signatures[tx_pos] = maker_tx.signatures[maker_tx_pos];


    //     // for pos in 0..tx.signatures.len() {
    //     //     if tx.signatures[pos] == Signature::default() {
    //     //         maker_tx.get_signing_keypair_positions(pubkeys)
    //     //         tx.signatures[pos] = maker_tx.signatures[2];
    //     //         break;
    //     //     }
    //     // }
    // }
    

    println!("Final TX size: {}", bincode::serialize(&tx).unwrap().len());
    banks_client.process_transaction(tx).await?;

    Ok(())
}

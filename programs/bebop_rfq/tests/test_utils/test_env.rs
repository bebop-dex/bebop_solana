
use std::sync::Arc;

use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::Instruction,
        program_pack::Pack,
        system_instruction,
    },
    system_program, InstructionData,
};
use anchor_spl::{
    associated_token::spl_associated_token_account::{
        get_associated_token_address_with_program_id,
        instruction as ata_ix,
    },
    token::spl_token::{
        self,
        instruction as token_ix,
        state::{Account as TokenAccount, Mint as Mint0},
    },
    token_2022::spl_token_2022::{
        self,
        extension::{
            transfer_fee::instruction as transfer_fee_ix,
            ExtensionType,
        },
        instruction as token22_ix,
        state::Mint as Mint22,
    },
};
use bebop_rfq::bebop_rfq::AmountWithExpiry;
use solana_program_test::{
    tokio::{self, sync::Mutex},
    BanksClient, BanksClientError, ProgramTest,
};
use solana_sdk::{
    feature_set::bpf_account_data_direct_mapping,
    message::Message,
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature},
    signature::Signer,
    transaction::{Transaction, TransactionError},
};
use anchor_spl::token::spl_token::native_mint;

/// Lightweight extension descriptor replacing spl_token_client::ExtensionInitializationParams.
#[derive(Clone, Debug)]
pub enum MintExtension {
    TransferFee { basis_points: u16, max_fee: u64 },
    NonTransferable,
}

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
    pub makers_token_a_account: Vec<Pubkey>,
    pub shared_token_a_account: Option<Pubkey>,
    pub receiver_token_a_account: Option<Pubkey>,

    pub taker_token_b_account: Option<Pubkey>,
    pub makers_token_b_account: Vec<Pubkey>,
    pub shared_token_b_account: Option<Pubkey>,
    pub receiver_token_b_account: Option<Pubkey>,

    pub taker_token_c_account: Option<Pubkey>,
    pub makers_token_c_account: Vec<Pubkey>,
    pub shared_token_c_account: Option<Pubkey>,
    pub receiver_token_c_account: Option<Pubkey>,

    pub token_a_mint: Pubkey,
    pub token_a_program_id: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_b_program_id: Pubkey,
    pub token_c_mint: Pubkey,
    pub token_c_program_id: Pubkey,

    pub temporary_wsol_token_accounts: Vec<Pubkey>,
}


impl TestEnvironment {

    pub async fn create_single_swap_instructions(&self, test_mode: TestMode, mint_taker_balance: bool) -> Vec<Instruction> {
        let TestEnvironment {
            banks_client, makers, taker, random_receiver, temporary_wsol_token_accounts,
            payer, shared_pda, token_a_mint, token_a_program_id, token_b_mint, token_b_program_id,
            shared_token_a_account, taker_token_a_account, makers_token_a_account,
            taker_token_b_account, receiver_token_b_account, shared_token_b_account, makers_token_b_account,
            ..
        } = self;

        let (cur_receiver_address, cur_receiver_token_b_account) = match test_mode.receiver_kind {
            ReceiverKind::Taker => (taker, taker_token_b_account),
            ReceiverKind::TakerWithTokenAccount => {
                get_associated_token_account(
                    *taker, token_b_mint, token_b_program_id,
                    test_mode.taker_accounts.output.clone(), true, banks_client, payer,
                ).await;
                (taker, taker_token_b_account)
            }
            ReceiverKind::AnotherAddress => (random_receiver, receiver_token_b_account),
            ReceiverKind::SharedAccount => (shared_pda, shared_token_b_account),
        };
        let mut instructions = Vec::new();
        if test_mode.receiver_kind != ReceiverKind::TakerWithTokenAccount {
            instructions.push(ata_ix::create_associated_token_account(
                &payer.pubkey(), cur_receiver_address, token_b_mint, token_b_program_id,
            ));
        }
        if mint_taker_balance {
            mint_balance(
                test_mode.input_amounts.iter().sum(), *taker_token_a_account,
                token_a_mint, token_a_program_id,
                test_mode.clone().taker_accounts.input, banks_client, payer,
            ).await;
        }
        for (i, amount) in test_mode.output_amounts.iter().enumerate() {
            mint_balance(
                *amount,
                if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(i).cloned() },
                token_b_mint, token_b_program_id,
                test_mode.clone().maker_accounts.output, banks_client, payer,
            ).await;
        }

        for i in 0..test_mode.input_amounts.len() {
            assert_eq!(test_mode.input_amounts.len(), test_mode.output_amounts.len());
            let data = bebop_rfq::instruction::Swap {
                input_amount: test_mode.input_amounts[i],
                output_amounts: vec![AmountWithExpiry { amount: test_mode.output_amounts[i], expiry: i64::MAX }],
                event_id: 0,
                shared_account_bump: Pubkey::find_program_address(&[bebop_rfq::SHARED_ACCOUNT], &bebop_rfq::ID).1,
                wsol_bump: Pubkey::find_program_address(
                    &[bebop_rfq::TEMPORARY_WSOL_TOKEN_ACCOUNT, makers[i].as_ref()],
                    &bebop_rfq::ID).1,
            }.data();
            let accs = bebop_rfq::accounts::Swap {
                maker: makers[i],
                taker: if test_mode.use_shared_taker { *shared_pda } else { *taker },
                receiver: *cur_receiver_address,
                taker_input_mint_token_account: if test_mode.use_shared_taker { *shared_token_a_account } else { *taker_token_a_account },
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
                instruction.accounts.iter_mut()
                    .for_each(|a| if a.pubkey == *taker { a.is_signer = true });
            }
            if !temporary_wsol_token_accounts.is_empty() {
                instruction.accounts.push(AccountMeta::new(temporary_wsol_token_accounts[i], false));
            }
            instructions.push(instruction);
        }
        instructions
    }

    pub async fn create_2_hops_instructions(&self, test_mode: TestMode) -> Vec<Instruction> {
        let TestEnvironment {
            banks_client, makers, taker, random_receiver, temporary_wsol_token_accounts,
            payer, shared_pda, token_a_mint, token_a_program_id, token_b_mint, token_b_program_id,
            token_c_mint, token_c_program_id,
            taker_token_a_account, makers_token_a_account,
            taker_token_b_account, receiver_token_b_account, makers_token_b_account, shared_token_b_account,
            shared_token_c_account, makers_token_c_account,
            ..
        } = self;

        let (cur_receiver_address, cur_receiver_token_b_account) = match test_mode.receiver_kind {
            ReceiverKind::Taker => (taker, taker_token_b_account),
            ReceiverKind::TakerWithTokenAccount => {
                get_associated_token_account(
                    *taker, token_b_mint, token_b_program_id,
                    test_mode.taker_accounts.output.clone(), true, banks_client, payer,
                ).await;
                (taker, taker_token_b_account)
            }
            ReceiverKind::AnotherAddress => (random_receiver, receiver_token_b_account),
            ReceiverKind::SharedAccount => (shared_pda, shared_token_b_account),
        };
        let mut instructions = Vec::new();
        if test_mode.receiver_kind != ReceiverKind::TakerWithTokenAccount {
            instructions.push(ata_ix::create_associated_token_account(
                &payer.pubkey(), cur_receiver_address, token_b_mint, token_b_program_id,
            ));
        }
        let middle_amount = test_mode.middle_token_info.clone().unwrap().token_amount;

        mint_balance(test_mode.input_amounts.iter().sum(), *taker_token_a_account, token_a_mint, token_a_program_id, test_mode.clone().taker_accounts.input, banks_client, payer).await;
        mint_balance(middle_amount, if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(0).cloned() }, token_c_mint, token_c_program_id, AccountKind::Token, banks_client, payer).await;
        mint_balance(test_mode.output_amounts.iter().sum(), if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(1).cloned() }, token_b_mint, token_b_program_id, test_mode.clone().maker_accounts.output, banks_client, payer).await;
        assert_eq!(test_mode.input_amounts.len(), 1);

        let data_1 = bebop_rfq::instruction::Swap {
            input_amount: test_mode.input_amounts[0],
            output_amounts: vec![AmountWithExpiry { amount: middle_amount, expiry: i64::MAX }],
            event_id: 0,
            shared_account_bump: Pubkey::find_program_address(&[bebop_rfq::SHARED_ACCOUNT], &bebop_rfq::ID).1,
            wsol_bump: Pubkey::find_program_address(
                &[bebop_rfq::TEMPORARY_WSOL_TOKEN_ACCOUNT, makers[0].as_ref()],
                &bebop_rfq::ID).1,
        }.data();
        let mut instruction_1 = Instruction {
            program_id: bebop_rfq::ID,
            accounts: bebop_rfq::accounts::Swap {
                maker: makers[0], taker: *taker, receiver: *shared_pda,
                taker_input_mint_token_account: *taker_token_a_account,
                maker_input_mint_token_account: if makers_token_a_account.is_empty() { None } else { makers_token_a_account.get(0).cloned() },
                receiver_output_mint_token_account: *shared_token_c_account,
                maker_output_mint_token_account: if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(0).cloned() },
                input_mint: *token_a_mint, input_token_program: *token_a_program_id,
                output_mint: *token_c_mint, output_token_program: *token_c_program_id,
                system_program: system_program::ID,
            }.to_account_metas(None),
            data: data_1,
        };
        instruction_1.accounts.iter_mut().for_each(|a| if a.pubkey == *taker { a.is_signer = true });
        if !temporary_wsol_token_accounts.is_empty() {
            instruction_1.accounts.push(AccountMeta::new(temporary_wsol_token_accounts[0], false));
        }

        let data_2 = bebop_rfq::instruction::Swap {
            input_amount: middle_amount,
            output_amounts: vec![AmountWithExpiry { amount: test_mode.output_amounts[0], expiry: i64::MAX }],
            event_id: 0,
            shared_account_bump: Pubkey::find_program_address(&[bebop_rfq::SHARED_ACCOUNT], &bebop_rfq::ID).1,
            wsol_bump: Pubkey::find_program_address(
                &[bebop_rfq::TEMPORARY_WSOL_TOKEN_ACCOUNT, makers[1].as_ref()],
                &bebop_rfq::ID).1,
        }.data();
        let mut instruction_2 = Instruction {
            program_id: bebop_rfq::ID,
            accounts: bebop_rfq::accounts::Swap {
                maker: makers[1], taker: *shared_pda, receiver: *cur_receiver_address,
                taker_input_mint_token_account: *shared_token_c_account,
                maker_input_mint_token_account: if makers_token_c_account.is_empty() { None } else { makers_token_c_account.get(1).cloned() },
                receiver_output_mint_token_account: *cur_receiver_token_b_account,
                maker_output_mint_token_account: if makers_token_b_account.is_empty() { None } else { makers_token_b_account.get(1).cloned() },
                input_mint: *token_c_mint, input_token_program: *token_c_program_id,
                output_mint: *token_b_mint, output_token_program: *token_b_program_id,
                system_program: system_program::ID,
            }.to_account_metas(None),
            data: data_2,
        };
        if !temporary_wsol_token_accounts.is_empty() {
            instruction_2.accounts.push(AccountMeta::new(temporary_wsol_token_accounts[1], false));
        }
        instructions.push(instruction_1);
        instructions.push(instruction_2);
        instructions
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum AccountKind { #[default] Token, NativeMint, NativeSol }

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum ReceiverKind { #[default] Taker, TakerWithTokenAccount, AnotherAddress, SharedAccount }

#[derive(Default, Clone, Debug)]
pub struct Accounts { pub input: AccountKind, pub output: AccountKind }

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum OnchainSwapType { #[default] RaydiumCPMM, RaydiumCLMM, MeteoraDLMM }

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
    pub input_mint_extensions: Option<Vec<MintExtension>>,
    pub output_mint_extensions: Option<Vec<MintExtension>>,
    pub onchain_swap_type: Option<OnchainSwapType>,
}

impl Default for TestMode {
    fn default() -> Self {
        Self {
            input_amounts: vec![1_000_000_000],
            output_amounts: vec![2_000_000_000],
            taker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token },
            maker_accounts: Accounts { input: AccountKind::Token, output: AccountKind::Token },
            receiver_kind: ReceiverKind::Taker,
            use_shared_taker: false,
            middle_token_info: None,
            expected_error: None,
            input_mint_extensions: None,
            output_mint_extensions: None,
            onchain_swap_type: None,
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct MiddleTokenInfo {
    pub token_amount: u64,
    pub mint_extensions: Option<Vec<MintExtension>>,
}

#[macro_export]
macro_rules! anchor_processor {
    ($program:ident) => {{
        fn entry(
            program_id: &anchor_lang::solana_program::pubkey::Pubkey,
            accounts: &[anchor_lang::solana_program::account_info::AccountInfo],
            instruction_data: &[u8],
        ) -> anchor_lang::solana_program::entrypoint::ProgramResult {
            let accounts = Box::leak(Box::new(accounts.to_vec()));
            $program::entry(program_id, accounts, instruction_data)
        }
        solana_program_test::processor!(entry)
    }};
}

const TEST_AIRDROP: u64 = 5 * LAMPORTS_PER_SOL;

pub async fn prepare_test(test_mode: TestMode) -> TestEnvironment {
    let mut pt = ProgramTest::new("bebop_rfq", bebop_rfq::ID, anchor_processor!(bebop_rfq));
    pt.add_program("mock_swap", mock_swap::ID, anchor_processor!(mock_swap));
    pt.deactivate_feature(bpf_account_data_direct_mapping::ID);

    let (banks_client, payer, _) = pt.start().await;
    let taker_keypair = Keypair::new();
    let taker = taker_keypair.pubkey();
    let random_receiver = Keypair::new().pubkey();
    let (shared_pda, shared_account_bump) =
        Pubkey::find_program_address(&[bebop_rfq::SHARED_ACCOUNT], &bebop_rfq::ID);

    let mut makers_keypairs: Vec<Keypair> = Vec::new();
    let mut makers: Vec<Pubkey> = Vec::new();
    for _ in 0..5 {
        let kp = Keypair::new();
        makers.push(kp.pubkey());
        makers_keypairs.push(kp);
    }
    let payer = Arc::new(payer);
    let banks_client = Arc::new(Mutex::new(banks_client));

    let mut airdrop_ixs = vec![system_instruction::transfer(&payer.pubkey(), &taker, TEST_AIRDROP)];
    airdrop_ixs.extend(makers.iter().map(|m| system_instruction::transfer(&payer.pubkey(), m, TEST_AIRDROP)));
    process_and_assert_ok(&airdrop_ixs, &payer, &[&payer], &banks_client).await;

    let (mut mint_a_keypair, mut mint_a, mut mint_b_keypair, mut mint_b, mint_c_keypair, mint_c) = {
        let kp_a = Keypair::new(); let a = kp_a.pubkey();
        let kp_b = Keypair::new(); let b = kp_b.pubkey();
        let kp_c = Keypair::new(); let c = kp_c.pubkey();
        (Some(kp_a), a, Some(kp_b), b, Some(kp_c), c)
    };
    let mut uses_temporary_wsol = false;

    let TestMode { input_amounts, output_amounts, taker_accounts, maker_accounts, receiver_kind,
        use_shared_taker, middle_token_info, expected_error, input_mint_extensions,
        output_mint_extensions, onchain_swap_type } = test_mode;

    match (&taker_accounts, &maker_accounts) {
        (Accounts { input: AccountKind::Token, output: AccountKind::Token },
         Accounts { input: AccountKind::Token, output: AccountKind::Token }) => (),
        (Accounts { input: AccountKind::NativeSol, output: AccountKind::Token },
         Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }) |
        (Accounts { input: AccountKind::NativeMint, output: AccountKind::Token },
         Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }) =>
            { mint_a_keypair = None; mint_a = native_mint::ID; }
        (Accounts { input: AccountKind::Token, output: AccountKind::NativeSol },
         Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }) |
        (Accounts { input: AccountKind::Token, output: AccountKind::NativeMint },
         Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }) =>
            { mint_b_keypair = None; mint_b = native_mint::ID; }
        (Accounts { input: AccountKind::NativeMint, output: AccountKind::Token },
         Accounts { input: AccountKind::NativeSol, output: AccountKind::Token }) |
        (Accounts { input: AccountKind::NativeSol, output: AccountKind::Token },
         Accounts { input: AccountKind::NativeMint, output: AccountKind::Token }) =>
            { mint_a_keypair = None; mint_a = native_mint::ID; uses_temporary_wsol = true; }
        (Accounts { input: AccountKind::Token, output: AccountKind::NativeMint },
         Accounts { input: AccountKind::Token, output: AccountKind::NativeSol }) |
        (Accounts { input: AccountKind::Token, output: AccountKind::NativeSol },
         Accounts { input: AccountKind::Token, output: AccountKind::NativeMint }) =>
            { mint_b_keypair = None; mint_b = native_mint::ID; uses_temporary_wsol = true; }
        _ => panic!("Invalid combo"),
    };

    let token_a_program_id = create_token(&banks_client, &payer, &mint_a, mint_a_keypair, input_mint_extensions).await;
    let token_b_program_id = create_token(&banks_client, &payer, &mint_b, mint_b_keypair, output_mint_extensions).await;
    let token_c_program_id = create_token(&banks_client, &payer, &mint_c, mint_c_keypair, middle_token_info.and_then(|x| x.mint_extensions)).await;

    let taker_token_a_account = get_associated_token_account(taker, &mint_a, &token_a_program_id, taker_accounts.input.clone(), true, &banks_client, &payer).await;
    let taker_token_b_account = get_associated_token_account(taker, &mint_b, &token_b_program_id, taker_accounts.output.clone(), false, &banks_client, &payer).await;
    let taker_token_c_account = get_associated_token_account(taker, &mint_c, &token_c_program_id, AccountKind::Token, false, &banks_client, &payer).await;

    let shared_token_a_account = get_associated_token_account(shared_pda, &mint_a, &token_a_program_id, taker_accounts.input.clone(), true, &banks_client, &payer).await;
    let shared_token_b_account = get_associated_token_account(shared_pda, &mint_b, &token_b_program_id, taker_accounts.output.clone(), true, &banks_client, &payer).await;
    let shared_token_c_account = get_associated_token_account(shared_pda, &mint_c, &token_c_program_id, AccountKind::Token, true, &banks_client, &payer).await;

    let receiver_token_a_account = get_associated_token_account(random_receiver, &mint_a, &token_a_program_id, taker_accounts.input.clone(), false, &banks_client, &payer).await;
    let receiver_token_b_account = get_associated_token_account(random_receiver, &mint_b, &token_b_program_id, taker_accounts.output.clone(), false, &banks_client, &payer).await;
    let receiver_token_c_account = get_associated_token_account(random_receiver, &mint_c, &token_c_program_id, AccountKind::Token, false, &banks_client, &payer).await;

    let mut makers_token_a_account: Vec<Pubkey> = Vec::new();
    let mut makers_token_b_account: Vec<Pubkey> = Vec::new();
    let mut makers_token_c_account: Vec<Pubkey> = Vec::new();
    for maker in makers.iter() {
        if let Some(k) = get_associated_token_account(*maker, &mint_a, &token_a_program_id, maker_accounts.input.clone(), true, &banks_client, &payer).await { makers_token_a_account.push(k); }
        if let Some(k) = get_associated_token_account(*maker, &mint_b, &token_b_program_id, maker_accounts.output.clone(), true, &banks_client, &payer).await { makers_token_b_account.push(k); }
        if let Some(k) = get_associated_token_account(*maker, &mint_c, &token_c_program_id, AccountKind::Token, true, &banks_client, &payer).await { makers_token_c_account.push(k); }
    }

    let temporary_wsol_token_accounts: Vec<Pubkey> = if uses_temporary_wsol {
        makers.iter().map(|m| Pubkey::find_program_address(&[bebop_rfq::TEMPORARY_WSOL_TOKEN_ACCOUNT, m.as_ref()], &bebop_rfq::ID).0).collect()
    } else { Vec::new() };

    TestEnvironment {
        banks_client, payer, taker_keypair, makers_keypairs, makers, taker, random_receiver, shared_pda,
        taker_token_a_account, makers_token_a_account, shared_token_a_account, receiver_token_a_account,
        taker_token_b_account, makers_token_b_account, shared_token_b_account, receiver_token_b_account,
        taker_token_c_account, makers_token_c_account, shared_token_c_account, receiver_token_c_account,
        token_a_mint: mint_a, token_a_program_id,
        token_b_mint: mint_b, token_b_program_id,
        token_c_mint: mint_c, token_c_program_id,
        temporary_wsol_token_accounts,
    }
}

/// Create a token mint. Returns the token program ID used.
/// When mint_keypair is None (native mint), nothing is created and spl_token::ID is returned.
pub async fn create_token(
    banks_client: &Arc<Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    mint: &Pubkey,
    mint_keypair: Option<Keypair>,
    extensions: Option<Vec<MintExtension>>,
) -> Pubkey {
    let mint_kp: Keypair = match mint_keypair {
        None => return anchor_spl::token::ID,
        Some(kp) => kp,
    };
    let exts = extensions.unwrap_or_default();
    let is_22 = !exts.is_empty();
    let token_program = if is_22 { spl_token_2022::ID } else { spl_token::ID };

    let rent = banks_client.lock().await.get_rent().await.unwrap();
    let space = if is_22 {
        let ext_types: Vec<ExtensionType> = exts.iter().map(|e| match e {
            MintExtension::TransferFee { .. } => ExtensionType::TransferFeeConfig,
            MintExtension::NonTransferable   => ExtensionType::NonTransferable,
        }).collect();
        ExtensionType::try_calculate_account_len::<Mint22>(&ext_types).unwrap()
    } else {
        Mint0::LEN
    };

    let mut ixs = vec![system_instruction::create_account(
        &payer.pubkey(), &mint_kp.pubkey(),
        rent.minimum_balance(space), space as u64, &token_program,
    )];

    if is_22 {
        for ext in &exts {
            match ext {
                MintExtension::TransferFee { basis_points, max_fee } => {
                    ixs.push(transfer_fee_ix::initialize_transfer_fee_config(
                        &spl_token_2022::ID, &mint_kp.pubkey(), None, None, *basis_points, *max_fee,
                    ).unwrap());
                }
                MintExtension::NonTransferable => {
                    // initialize_non_transferable_mint lives at the top-level instruction module
                    ixs.push(token22_ix::initialize_non_transferable_mint(
                        &spl_token_2022::ID, &mint_kp.pubkey(),
                    ).unwrap());
                }
            }
        }
        ixs.push(token22_ix::initialize_mint2(
            &spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 9,
        ).unwrap());
    } else {
        ixs.push(token_ix::initialize_mint2(
            &spl_token::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 9,
        ).unwrap());
    }

    process_and_assert_ok(&ixs, payer, &[&mint_kp], banks_client).await;
    token_program
}

pub async fn get_associated_token_account(
    wallet: Pubkey, mint: &Pubkey, token_program: &Pubkey,
    kind: AccountKind, create: bool,
    banks_client: &Arc<Mutex<BanksClient>>, payer: &Arc<Keypair>,
) -> Option<Pubkey> {
    match kind {
        AccountKind::NativeSol => None,
        _ => {
            let ata = get_associated_token_address_with_program_id(&wallet, mint, token_program);
            if create {
                process_and_assert_ok(
                    &[ata_ix::create_associated_token_account(&payer.pubkey(), &wallet, mint, token_program)],
                    payer, &[payer], banks_client,
                ).await;
            }
            Some(ata)
        }
    }
}

pub async fn mint_balance(
    amount: u64, wallet_token_account: Option<Pubkey>,
    mint: &Pubkey, token_program: &Pubkey,
    kind: AccountKind,
    banks_client: &Arc<Mutex<BanksClient>>, payer: &Arc<Keypair>,
) {
    match kind {
        AccountKind::Token => {
            let dest = wallet_token_account.unwrap();
            let ix = if *token_program == spl_token_2022::ID {
                token22_ix::mint_to(&spl_token_2022::ID, mint, &dest, &payer.pubkey(), &[], amount).unwrap()
            } else {
                token_ix::mint_to(&spl_token::ID, mint, &dest, &payer.pubkey(), &[], amount).unwrap()
            };
            process_and_assert_ok(&[ix], payer, &[payer], banks_client).await;
        }
        AccountKind::NativeMint => {
            process_and_assert_ok(
                &[
                    system_instruction::transfer(&payer.pubkey(), &wallet_token_account.unwrap(), amount + 100_000_000),
                    spl_token::instruction::sync_native(&spl_token::ID, &wallet_token_account.unwrap()).unwrap(),
                ],
                payer, &[payer], banks_client,
            ).await;
        }
        AccountKind::NativeSol => {}
    }
}

pub async fn process_and_assert_ok(
    instructions: &[Instruction], payer: &Keypair, signers: &[&Keypair], banks_client: &Mutex<BanksClient>,
) {
    process_instructions(instructions, payer, signers, banks_client).await.unwrap();
}

pub async fn process_instructions(
    instructions: &[Instruction], payer: &Keypair, signers: &[&Keypair], banks_client: &Mutex<BanksClient>,
) -> std::result::Result<(), BanksClientError> {
    let mut bc = banks_client.lock().await;
    let recent_blockhash = bc.get_latest_blockhash().await.unwrap();
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(instructions, Some(&payer.pubkey()), &all_signers, recent_blockhash);
    bc.process_transaction(tx).await
}

pub async fn sign_and_execute_tx(
    instructions: &[Instruction], payer: &Keypair, taker: &Keypair, makers: &[Keypair], banks_client: &Mutex<BanksClient>,
) -> std::result::Result<(), BanksClientError> {
    let mut bc = banks_client.lock().await;
    let recent_blockhash = bc.get_latest_blockhash().await.unwrap();
    let msg = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &recent_blockhash);
    let mut tx = Transaction::new_unsigned(msg);
    tx.message.recent_blockhash = recent_blockhash;
    let mut signatures: Vec<(Pubkey, Signature)> = vec![];
    for signer in makers.iter().chain(std::iter::once(taker)).chain(std::iter::once(payer)) {
        let sig = signer.try_sign_message(&tx.message_data()).unwrap();
        signatures.push((signer.pubkey(), sig));
    }
    tx.replace_signatures(&signatures).map_err(BanksClientError::TransactionError)?;
    bc.process_transaction(tx).await?;
    Ok(())
}

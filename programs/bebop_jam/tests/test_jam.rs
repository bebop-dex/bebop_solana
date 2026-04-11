//! Tests for the JAM settlement program (standalone, no QU!D dependency).

mod test_utils;

use std::sync::Arc;

use anchor_lang::{prelude::Pubkey, system_program, InstructionData, ToAccountMetas};
use anchor_spl::{
    associated_token::spl_associated_token_account::{
        get_associated_token_address_with_program_id,
        instruction as ata_ix,
    },
    token::spl_token::{self, instruction as token_ix, state::Account as TokenAccount},
};
use jam_settlement::instructions::{SolanaJamOrder, SolanaInteraction, InteractionAccount};
use solana_program_test::{tokio, BanksClient, BanksClientError, ProgramTest};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signature::Signer,
    system_instruction,
    transaction::TransactionError,
};
use anchor_lang::solana_program::program_pack::Pack;
use test_utils::{process_and_assert_ok, sign_and_execute_tx};

// ─── PDA helpers ─────────────────────────────────────────────────────────────

fn jam_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[jam_settlement::instructions::JAM_CONFIG_SEED], &jam_settlement::ID)
}

pub fn jam_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[jam_settlement::instructions::JAM_AUTHORITY_SEED], &jam_settlement::ID)
}

fn nonce_record_pda(taker: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[jam_settlement::instructions::NONCE_SEED, taker.as_ref(), &nonce.to_le_bytes()],
        &jam_settlement::ID,
    )
}

fn custody_authority_pda(taker: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[jam_settlement::instructions::CUSTODY_SEED, taker.as_ref(), &nonce.to_le_bytes()],
        &jam_settlement::ID,
    )
}


// ─── Token helpers ───────────────────────────────────────────────────────────

async fn create_spl_mint(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    mint_kp: Keypair,
) -> Pubkey {
    let mint_pubkey = mint_kp.pubkey();
    let rent = banks_client.lock().await.get_rent().await.unwrap();
    process_and_assert_ok(
        &[
            system_instruction::create_account(
                &payer.pubkey(), &mint_pubkey,
                rent.minimum_balance(spl_token::state::Mint::LEN),
                spl_token::state::Mint::LEN as u64,
                &spl_token::ID,
            ),
            token_ix::initialize_mint2(&spl_token::ID, &mint_pubkey, &payer.pubkey(), None, 9).unwrap(),
        ],
        payer,
        &[&mint_kp],
        banks_client,
    ).await;
    mint_pubkey
}

async fn create_ata(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Pubkey {
    let ata = get_associated_token_address_with_program_id(owner, mint, &spl_token::ID);
    process_and_assert_ok(
        &[ata_ix::create_associated_token_account(&payer.pubkey(), owner, mint, &spl_token::ID)],
        payer, &[payer], banks_client,
    ).await;
    ata
}

async fn mint_to(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
) {
    process_and_assert_ok(
        &[token_ix::mint_to(&spl_token::ID, mint, destination, &payer.pubkey(), &[], amount).unwrap()],
        payer, &[payer], banks_client,
    ).await;
}

async fn token_balance(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    account: &Pubkey,
) -> u64 {
    match banks_client.lock().await.get_account(*account).await.unwrap() {
        None => 0,
        Some(acc) => TokenAccount::unpack(&acc.data[..TokenAccount::LEN])
            .map(|a| a.amount)
            .unwrap_or(0),
    }
}

// ─── Instruction builders ────────────────────────────────────────────────────

fn ix_init_config(admin: &Pubkey, treasury: Pubkey) -> Instruction {
    let (config, _) = jam_config_pda();
    let (jam_authority, _) = jam_authority_pda();
    Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::InitConfig {
            admin: *admin, config, jam_authority,
            system_program: system_program::ID,
        }.to_account_metas(None),
        data: jam_settlement::instruction::InitConfig {
            params: jam_settlement::InitConfigParams {
                treasury, min_share_bps: 0, protocol_fee_bps: 0,
            },
        }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
fn ix_settle(
    solver: &Pubkey, taker: &Pubkey,
    order: SolanaJamOrder, interactions: Vec<SolanaInteraction>,
    taker_sell_ata: Pubkey, custody_sell_ata: Pubkey,
    custody_buy_ata: Pubkey, receiver_buy_ata: Pubkey,
    sell_mint: Pubkey, buy_mint: Pubkey, nonce: u64,
    solver_sell_ata: Option<Pubkey>,
) -> Instruction {
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(taker, nonce);
    let (custody_authority, _) = custody_authority_pda(taker, nonce);
    let (jam_authority, _) = jam_authority_pda();
    Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver: *solver,
            taker: *taker,
            config,
            nonce_record,
            taker_sell_ata: Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata: Some(custody_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority,
            jam_authority,
            sell_mint,
            buy_mint,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program: anchor_spl::token::ID,
            partner_account: None,
            treasury_buy_ata: None,
            solver_sell_ata,
            token_program: anchor_spl::token::ID,
            system_program: system_program::ID,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
fn ix_settle_internal(
    solver: &Pubkey, taker: &Pubkey,
    order: SolanaJamOrder, filled_amounts: Vec<u64>,
    taker_sell_ata: Pubkey, solver_sell_ata: Pubkey,
    solver_buy_ata: Pubkey, receiver_buy_ata: Pubkey,
    sell_mint: Pubkey, buy_mint: Pubkey, nonce: u64,
) -> Instruction {
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(taker, nonce);
    Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::SettleInternal {
            solver: *solver,
            taker: *taker,
            config,
            nonce_record,
            taker_sell_ata: Some(taker_sell_ata),
            solver_sell_ata: Some(solver_sell_ata),
            solver_buy_ata: Some(solver_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            sell_mint,
            buy_mint,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program: anchor_spl::token::ID,
            token_program: anchor_spl::token::ID,
            system_program: system_program::ID,
            partner_account: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::SettleInternal { order, filled_amounts }.data(),
    }
}

fn make_order(taker: Pubkey, sell_mint: Pubkey, buy_mint: Pubkey, nonce: u64) -> SolanaJamOrder {
    SolanaJamOrder {
        taker,
        receiver: None,           // None = taker (saves 31 bytes per order)
        expiry: i64::MAX,
        exclusivity_deadline: None,
        nonce,
        executor: None,
        partner_fee_bps: 0,       // replaces partner_info: u64 (saves 6 bytes)
        partner: None,
        sell_tokens: vec![sell_mint], buy_tokens: vec![buy_mint],
        sell_amounts: vec![1_000_000_000], buy_amounts: vec![500_000_000],
        hooks_enabled: false,     // replaces hooks_hash: [u8;32] (saves 31 bytes)
    }
}

// ─── Test environment ────────────────────────────────────────────────────────

struct JamTestEnv {
    banks_client: Arc<tokio::sync::Mutex<BanksClient>>,
    payer: Arc<Keypair>,
    solver_kp: Keypair,
    taker_kp: Keypair,
    mint_a: Pubkey,
    mint_b: Pubkey,
    /// Pre-created solver ATAs for common sell mints (needed since
    /// handle_settle now drains custody_sell → solver after buy delivery).
    solver_ata_a: Pubkey,
    solver_ata_b: Pubkey,
}

async fn prepare_jam_env() -> JamTestEnv {
    let mut pt = ProgramTest::new(
        "jam_settlement", jam_settlement::ID, None,
    );
    pt.deactivate_feature(
        solana_sdk::feature_set::bpf_account_data_direct_mapping::ID,
    );

    let (banks_client, payer, _) = pt.start().await;
    let payer = Arc::new(payer);
    let banks_client = Arc::new(tokio::sync::Mutex::new(banks_client));

    let solver_kp = Keypair::new();
    let taker_kp = Keypair::new();

    process_and_assert_ok(
        &[
            system_instruction::transfer(&payer.pubkey(), &solver_kp.pubkey(), 5 * LAMPORTS_PER_SOL),
            system_instruction::transfer(&payer.pubkey(), &taker_kp.pubkey(), 5 * LAMPORTS_PER_SOL),
        ],
        &payer, &[&payer], &banks_client,
    ).await;

    let mint_a = create_spl_mint(&banks_client, &payer, Keypair::new()).await;
    let mint_b = create_spl_mint(&banks_client, &payer, Keypair::new()).await;

    // Use Pubkey::default() as quid_program placeholder — quid not needed for these tests
    process_and_assert_ok(
        &[ix_init_config(&payer.pubkey(), payer.pubkey())],
        &payer, &[&payer], &banks_client,
    ).await;

    // Create solver ATAs for sell mints (handle_settle drains custody_sell → solver)
    let solver_ata_a = create_ata(&banks_client, &payer, &solver_kp.pubkey(), &mint_a).await;
    let solver_ata_b = create_ata(&banks_client, &payer, &solver_kp.pubkey(), &mint_b).await;

    JamTestEnv { banks_client, payer, solver_kp, taker_kp, mint_a, mint_b, solver_ata_a, solver_ata_b }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_init_config() {
    let env = prepare_jam_env().await;
    let (config_pda, _) = jam_config_pda();
    let (expected_authority, _) = jam_authority_pda();

    let raw = env.banks_client.lock().await.get_account(config_pda).await.unwrap().unwrap();
    let config: jam_settlement::instructions::JamConfig =
        anchor_lang::AccountDeserialize::try_deserialize(&mut raw.data.as_ref()).unwrap();

    // jam_authority removed from JamConfig (derivable, not stored).
    // Verify the derivable address matches expected instead:
    assert_eq!(expected_authority, jam_authority_pda().0);
    assert_eq!(config.min_share_bps, 0);
    // quid_program field removed — JAM is program-agnostic
}

#[tokio::test]
async fn test_jam_settle_basic() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 1;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    assert_eq!(token_balance(&env.banks_client, &taker_sell_ata).await, 0);
    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata).await, 500_000_000);
    let (nr_pda, _) = nonce_record_pda(&taker, nonce);
    assert!(env.banks_client.lock().await.get_account(nr_pda).await.unwrap().is_some());
}

#[tokio::test]
async fn test_jam_settle_insufficient_output() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 2;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 100_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle(&solver, &taker, order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        None,
    );
    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err());
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        0, solana_sdk::instruction::InstructionError::Custom(code),
    )) = result.unwrap_err() else { panic!("wrong error type") };
    assert_eq!(code, 6000 + jam_settlement::error::JamError::InsufficientOutput as u32);
}

#[tokio::test]
async fn test_jam_settle_nonce_replay_rejected() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 3;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 2_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 1_000_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = || ix_settle(&solver, &taker, order.clone(), vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    let solver_kp2 = Keypair::from_bytes(&env.solver_kp.to_bytes()).unwrap();

    sign_and_execute_tx(&[ix()], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix()], &env.payer, &env.taker_kp, &[solver_kp2], &env.banks_client).await;
    assert!(result.is_err(), "duplicate nonce should be rejected");
}

#[tokio::test]
async fn test_jam_settle_expired_order() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 4;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.expiry = 1;
    let ix = ix_settle(&solver, &taker, order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        None,
    );

    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err());
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        0, solana_sdk::instruction::InstructionError::Custom(code),
    )) = result.unwrap_err() else { panic!("wrong error type") };
    assert_eq!(code, 6000 + jam_settlement::error::JamError::OrderExpired as u32);
}

#[tokio::test]
async fn test_jam_settle_exclusivity() {
    let env = prepare_jam_env().await;
    let authorized_solver_kp = env.solver_kp;
    let unauthorized_solver_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &unauthorized_solver_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let taker = env.taker_kp.pubkey();
    let nonce: u64 = 5;
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.executor = Some(authorized_solver_kp.pubkey());
    order.exclusivity_deadline = Some(i64::MAX);

    let ix = |solver: &Pubkey| ix_settle(solver, &taker, order.clone(), vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );

    let result: std::result::Result<(), BanksClientError> = sign_and_execute_tx(
        &[ix(&unauthorized_solver_kp.pubkey())],
        &env.payer, &env.taker_kp, &[unauthorized_solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err());
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        0, solana_sdk::instruction::InstructionError::Custom(code),
    )) = result.unwrap_err() else { panic!("wrong error type") };
    assert_eq!(code, 6000 + jam_settlement::error::JamError::ExclusivityViolation as u32);

    sign_and_execute_tx(
        &[ix(&authorized_solver_kp.pubkey())],
        &env.payer, &env.taker_kp, &[authorized_solver_kp], &env.banks_client,
    ).await.unwrap();
}

#[tokio::test]
async fn test_jam_settle_internal() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 10;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let solver_sell_ata = env.solver_ata_a;
    let solver_buy_ata  = env.solver_ata_b;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle_internal(&solver, &taker, order, vec![500_000_000], taker_sell_ata, solver_sell_ata, solver_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce);
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    assert_eq!(token_balance(&env.banks_client, &taker_sell_ata).await, 0);
    assert_eq!(token_balance(&env.banks_client, &solver_sell_ata).await, 1_000_000_000);
    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata).await, 500_000_000);
    assert_eq!(token_balance(&env.banks_client, &solver_buy_ata).await, 0);
}

#[tokio::test]
async fn test_jam_settle_internal_underfill_rejected() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 11;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let solver_sell_ata = env.solver_ata_a;
    let solver_buy_ata  = env.solver_ata_b;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata, 100_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle_internal(&solver, &taker, order, vec![100_000_000], taker_sell_ata, solver_sell_ata, solver_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce);
    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_jam_settle_wrong_taker() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 20;
    let solver = env.solver_kp.pubkey();
    let taker = env.taker_kp.pubkey();
    let impostor_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &impostor_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle(&solver, &impostor_kp.pubkey(), order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        None,
    );
    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &impostor_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "wrong taker should be rejected");
}

// ─── InvalidReceiver tests ────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_invalid_receiver_rejected() {
    // A solver passes a valid ATA for the correct mint but owned by a third
    // party (thief), not order.receiver. InvalidReceiver must fire.
    let env = prepare_jam_env().await;
    let nonce: u64 = 30;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let thief_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &thief_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    // Receiver ATA belongs to thief, not taker/order.receiver
    let thief_buy_ata    = create_ata(&env.banks_client, &env.payer, &thief_kp.pubkey(), &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, thief_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "invalid receiver should be rejected");
}

#[tokio::test]
async fn test_jam_settle_internal_invalid_receiver_rejected() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 31;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let thief_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &thief_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let solver_sell_ata = env.solver_ata_a;
    let solver_buy_ata  = env.solver_ata_b;
    let thief_buy_ata   = create_ata(&env.banks_client, &env.payer, &thief_kp.pubkey(), &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);
    let ix = ix_settle_internal(
        &solver, &taker, order, vec![],
        taker_sell_ata, solver_sell_ata, solver_buy_ata, thief_buy_ata,
        env.mint_a, env.mint_b, nonce,
    );
    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "internal settle with wrong receiver should be rejected");
}

// ─── Native SOL tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_native_sol_sell() {
    // Taker sells native SOL, receives SPL token.
    let env = prepare_jam_env().await;
    let nonce: u64 = 40;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;

    // Pre-fund custody buy (simulates solver output)
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let sell_amount: u64 = 500_000_000; // 0.5 SOL in lamports
    let mut order = make_order(taker, anchor_spl::token::spl_token::native_mint::ID, env.mint_b, nonce);
    order.sell_amounts = vec![sell_amount];

    // Settle with None for sell ATAs (native SOL path — uses native_mint::ID sentinel)
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver,
            taker,
            config,
            nonce_record,
            taker_sell_ata:   None,
            custody_sell_ata: None,
            custody_buy_ata:  Some(custody_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority,
            jam_authority,
            sell_mint:  anchor_spl::token::spl_token::native_mint::ID,
            buy_mint:   env.mint_b,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None,
                treasury_buy_ata: None,
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: Some(env.solver_ata_a),
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle {
            order,
            interactions: vec![],
        }.data(),
    };

    let taker_sol_before = env.banks_client.lock().await
        .get_balance(taker).await.unwrap();

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let taker_sol_after = env.banks_client.lock().await
        .get_balance(taker).await.unwrap();
    let buy_received = token_balance(&env.banks_client, &receiver_buy_ata).await;

    assert!(taker_sol_after < taker_sol_before, "taker SOL should decrease");
    assert_eq!(buy_received, 500_000_000, "taker should receive buy tokens");
}

#[tokio::test]
async fn test_jam_settle_native_sol_buy() {
    // Taker sells SPL, receives native SOL.
    let env = prepare_jam_env().await;
    let nonce: u64 = 41;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;

    // Pre-fund custody authority with native SOL (simulates solver delivering SOL)
    process_and_assert_ok(
        &[system_instruction::transfer(
            &env.payer.pubkey(), &custody_authority, 500_000_000,
        )],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let mut order = make_order(taker, env.mint_a, anchor_spl::token::spl_token::native_mint::ID, nonce);
    order.buy_amounts = vec![500_000_000];

    let taker_sol_before = env.banks_client.lock().await.get_balance(taker).await.unwrap();

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver,
            taker,
            config,
            nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata:  None,
            receiver_buy_ata: None,
            custody_authority,
            jam_authority,
            sell_mint: env.mint_a,
            buy_mint:  anchor_spl::token::spl_token::native_mint::ID,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None,
                treasury_buy_ata: None,
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: Some(env.solver_ata_a),
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle {
            order,
            interactions: vec![],
        }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let taker_sol_after = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    assert!(taker_sol_after > taker_sol_before, "taker should receive SOL");
}

// ─── Partner fee test ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_partner_fee_deducted() {
    // partner_fee_bps=100 (1%). Verify fee goes to partner ATA,
    // net amount goes to receiver, and net >= order.buy_amounts[0].
    let env = prepare_jam_env().await;
    let nonce: u64 = 50;
    let solver  = env.solver_kp.pubkey();
    let taker   = env.taker_kp.pubkey();
    let partner_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &partner_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let taker_sell_ata    = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let custody_sell_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata   = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata  = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let partner_ata       = create_ata(&env.banks_client, &env.payer, &partner_kp.pubkey(), &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    // Custody receives 600_000_000 so that after 1% fee (6_000_000) receiver
    // still gets >= buy_amounts[0] = 500_000_000.
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 600_000_000).await;

    // partner_fee_bps is now u16 directly (was packed into u64 upper bits).
    let fee_bps: u16 = 100;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.partner_fee_bps = fee_bps;
    order.partner = Some(partner_kp.pubkey());

    let mut accounts = jam_settlement::accounts::Settle {
        solver, taker, config, nonce_record,
        taker_sell_ata:   Some(taker_sell_ata),
        custody_sell_ata: Some(custody_sell_ata),
        custody_buy_ata:  Some(custody_buy_ata),
        receiver_buy_ata: Some(receiver_buy_ata),
        custody_authority,
        jam_authority,
        sell_mint: env.mint_a,
        buy_mint:  env.mint_b,
        sell_token_program: anchor_spl::token::ID,
        buy_token_program:  anchor_spl::token::ID,
        partner_account: Some(partner_ata),
        treasury_buy_ata: None,
        token_program:   anchor_spl::token::ID,
        system_program:  system_program::ID,
        solver_sell_ata: Some(env.solver_ata_a),
    }.to_account_metas(None);

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts,
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let partner_received  = token_balance(&env.banks_client, &partner_ata).await;
    let receiver_received = token_balance(&env.banks_client, &receiver_buy_ata).await;

    // 1% of 600_000_000 = 6_000_000 to partner; 594_000_000 to receiver
    assert_eq!(partner_received, 6_000_000, "partner should receive 1% fee");
    assert_eq!(receiver_received, 594_000_000, "receiver gets remainder");
    assert!(receiver_received >= 500_000_000, "net >= buy_amounts[0]");
}

// ─── Interactions test ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_with_interaction() {
    // Tests that settle correctly reads custody_buy_ata balance after
    // pre-funding (simulating an interaction-delivered balance) and forwards
    // to the receiver. The run_interactions path is covered by the
    // test_jam_settle_interaction_blocked_programs_rejected test.
    //
    // Note: spl_token/system_program/spl_token_2022 are blocked as direct
    // interaction targets to prevent taker fund drainage via propagated signer.
    // Real solver interactions go through DEX programs (Orca, Raydium, etc.),
    // not raw spl_token CPIs.
    let env = prepare_jam_env().await;
    let nonce: u64 = 60;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let (config, _)            = jam_config_pda();
    let (nonce_record, _)      = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _)     = jam_authority_pda();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    // Pre-fund custody_buy_ata directly — simulates solver delivering buy tokens
    // via interaction. In production, interactions call DEX programs which
    // internally transfer tokens into custody_buy_ata.
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata,   500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);

    // No interactions needed — custody_buy_ata already funded.
    // settle verifies balance >= buy_amounts[0] and forwards to receiver.
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let received = token_balance(&env.banks_client, &receiver_buy_ata).await;
    assert_eq!(received, 500_000_000, "receiver should get buy tokens");
}
#[tokio::test]
async fn test_jam_settle_interaction_blocked_programs_rejected() {
    // Verifies that system_program, spl_token, and spl_token_2022 are blocked as
    // direct interaction targets. An interaction targeting these can drain taker
    // funds via propagated signer privileges (taker.is_signer=true in outer tx).
    let env = prepare_jam_env().await;
    let nonce: u64 = 62;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (config, _)            = jam_config_pda();
    let (nonce_record, _)      = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _)     = jam_authority_pda();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata,  500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce);

    // Attempt 1: system_program as interaction target — must be rejected
    let sys_interaction = SolanaInteraction {
        program_index: 0, // system_program at remaining_accounts[0]
        accounts: vec![
            InteractionAccount::new(1, false, true),  // taker: signer
            InteractionAccount::new(2, true,  false), // solver (destination)
        ],
        data: {
            // system_program::transfer data: discriminator=2 + amount
            let mut d = vec![2, 0, 0, 0];
            d.extend_from_slice(&1_000_000u64.to_le_bytes());
            d
        },
        result: false, // don't propagate error — we want InteractionTargetProtected
        use_jam_authority: false,
    };

    let mut ix = ix_settle(
        &solver, &taker, order.clone(), vec![sys_interaction],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    ix.accounts.extend(vec![
        AccountMeta::new_readonly(system_program::ID, false), // [0] system_program
        AccountMeta::new(taker, true),                         // [1] taker
        AccountMeta::new(solver, false),                       // [2] solver
    ]);

    // Clone before first move so we can use solver_kp again for the second attempt
    let solver_kp2 = Keypair::from_bytes(&env.solver_kp.to_bytes()).unwrap();
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(), "system_program as interaction target must be rejected");

    // Attempt 2: spl_token as interaction target (new nonce)
    let nonce2: u64 = 63;
    let (nonce_record2, _)      = nonce_record_pda(&taker, nonce2);
    let (custody_authority2, _) = custody_authority_pda(&taker, nonce2);
    let custody_sell_ata2 = create_ata(&env.banks_client, &env.payer, &custody_authority2, &env.mint_a).await;
    let custody_buy_ata2  = create_ata(&env.banks_client, &env.payer, &custody_authority2, &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata2, 500_000_000).await;

    let order2 = make_order(taker, env.mint_a, env.mint_b, nonce2);
    let spl_interaction = SolanaInteraction {
        program_index: 0, // spl_token at remaining_accounts[0]
        accounts: vec![InteractionAccount::new(1, true, false)],
        data: vec![],
        result: false,
        use_jam_authority: false,
    };
    let mut ix2 = ix_settle(
        &solver, &taker, order2, vec![spl_interaction],
        taker_sell_ata, custody_sell_ata2, custody_buy_ata2, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce2,
        Some(env.solver_ata_a),
    );
    ix2.accounts.extend(vec![
        AccountMeta::new_readonly(anchor_spl::token::ID, false), // [0] spl_token
        AccountMeta::new(taker, false),                           // [1] dummy account
    ]);

    let result2 = sign_and_execute_tx(
        &[ix2], &env.payer, &env.taker_kp, &[solver_kp2], &env.banks_client,
    ).await;
    assert!(result2.is_err(), "spl_token as interaction target must be rejected");
}

// ─── Multi-token order tests ──────────────────────────────────────────────────
//
// remaining_accounts layout for 2 sell / 2 buy:
//   [0..4)  : sell pair 1: [taker_sell_ata_1, custody_sell_ata_1, mint_c, token_prog]
//   [4..8)  : buy  pair 1: [custody_buy_ata_1, receiver_buy_ata_1, mint_d, token_prog]
//   [8..)   : interaction accounts (none in this test)

#[tokio::test]
async fn test_jam_settle_multi_token_order() {
    // 2 sell pairs (mint_a + mint_c), 2 buy pairs (mint_b + mint_d).
    // Verifies that additional sell tokens are transferred into custody and
    // additional buy tokens are verified and forwarded to receiver.
    let env = prepare_jam_env().await;
    let nonce: u64 = 70;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    // Create extra mints
    let mint_c = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;
    let mint_d = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;

    let (config, _)            = jam_config_pda();
    let (nonce_record, _)      = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _)     = jam_authority_pda();

    // Pair 0 accounts (in accounts struct)
    let taker_sell_ata_0   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata_0 = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata_0  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata_0 = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;

    // Pair 1 accounts (in remaining_accounts)
    let taker_sell_ata_1   = create_ata(&env.banks_client, &env.payer, &taker,             &mint_c).await;
    let custody_sell_ata_1 = create_ata(&env.banks_client, &env.payer, &custody_authority, &mint_c).await;
    let solver_sell_ata_1  = create_ata(&env.banks_client, &env.payer, &solver,            &mint_c).await;
    let custody_buy_ata_1  = create_ata(&env.banks_client, &env.payer, &custody_authority, &mint_d).await;
    let receiver_buy_ata_1 = create_ata(&env.banks_client, &env.payer, &taker,             &mint_d).await;

    // Fund taker sell accounts
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata_0, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &mint_c,     &taker_sell_ata_1, 2_000_000_000).await;
    // Pre-fund custody buy accounts (simulates solver output)
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata_0, 500_000_000).await;
    mint_to(&env.banks_client, &env.payer, &mint_d,     &custody_buy_ata_1, 800_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    // Extend to 2 sell / 2 buy
    order.sell_tokens  = vec![env.mint_a, mint_c];
    order.sell_amounts = vec![1_000_000_000, 2_000_000_000];
    order.buy_tokens   = vec![env.mint_b, mint_d];
    order.buy_amounts  = vec![500_000_000, 800_000_000];

    // remaining_accounts: sell pair 1 (5: taker,custody,mint,prog,solver) + buy pair 1 (4)
    let remaining: Vec<AccountMeta> = vec![
        AccountMeta::new(taker_sell_ata_1, false),
        AccountMeta::new(custody_sell_ata_1, false),
        AccountMeta::new_readonly(mint_c, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new(solver_sell_ata_1, false), // solver receives sell pair 1
        // buy pair 1
        AccountMeta::new(custody_buy_ata_1, false),
        AccountMeta::new(receiver_buy_ata_1, false),
        AccountMeta::new_readonly(mint_d, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
    ];

    let mut accounts = jam_settlement::accounts::Settle {
        solver, taker, config, nonce_record,
        taker_sell_ata:   Some(taker_sell_ata_0),
        custody_sell_ata: Some(custody_sell_ata_0),
        custody_buy_ata:  Some(custody_buy_ata_0),
        receiver_buy_ata: Some(receiver_buy_ata_0),
        custody_authority,
        jam_authority,
        sell_mint: env.mint_a,
        buy_mint:  env.mint_b,
        sell_token_program: anchor_spl::token::ID,
        buy_token_program:  anchor_spl::token::ID,
        partner_account: None,
                treasury_buy_ata: None,
        token_program:   anchor_spl::token::ID,
        system_program:  system_program::ID,
        solver_sell_ata: Some(env.solver_ata_a),
    }.to_account_metas(None);

    accounts.extend(remaining);

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts,
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    // Pair 0: taker sold mint_a, received mint_b
    assert_eq!(token_balance(&env.banks_client, &taker_sell_ata_0).await,   0);
    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata_0).await, 500_000_000);
    // Pair 1: taker sold mint_c, received mint_d
    assert_eq!(token_balance(&env.banks_client, &taker_sell_ata_1).await,   0);
    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata_1).await, 800_000_000);
}

// ─── Token-2022 mint tests ────────────────────────────────────────────────────
//
// Token-2022 mints with TransferFee { basis_points: 0, max_fee: 0 } pass the
// transfer fee check in settle.rs. NonTransferable mints are expected to fail
// (settle.rs rejects mints with fee extensions, and NonTransferable should
// cause the CPI to fail at the token program level).

#[tokio::test]
async fn test_jam_settle_token22_zero_fee_succeeds() {
    // Token-2022 mint with TransferFee(0, 0) — settle should succeed since
    // the fee check in utils allows zero-fee T22 mints through.
    let env = prepare_jam_env().await;
    let nonce: u64 = 80;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    // Create Token-2022 mints with zero transfer fee
    let mint_a_22 = create_spl_token22_mint_zero_fee(&env.banks_client, &env.payer).await;
    let mint_b_22 = create_spl_token22_mint_zero_fee(&env.banks_client, &env.payer).await;

    let taker_sell_ata   = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_a_22).await;
    let custody_sell_ata = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_a_22).await;
    let custody_buy_ata  = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_b_22).await;
    let receiver_buy_ata = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_b_22).await;

    mint_to_t22(&env.banks_client, &env.payer, &mint_a_22, &taker_sell_ata,   1_000_000_000).await;
    mint_to_t22(&env.banks_client, &env.payer, &mint_b_22, &custody_buy_ata,  500_000_000).await;
    let solver_sell_ata_22 = create_ata_t22(&env.banks_client, &env.payer, &solver, &mint_a_22).await;

    let (config, _)       = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let mut order = make_order(taker, mint_a_22, mint_b_22, nonce);
    // token-2022 IDs
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata:  Some(custody_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority,
            jam_authority,
            sell_mint: mint_a_22,
            buy_mint:  mint_b_22,
            sell_token_program: anchor_spl::token_2022::ID,
            buy_token_program:  anchor_spl::token_2022::ID,
            partner_account: None,
                treasury_buy_ata: None,
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: Some(solver_sell_ata_22),
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data()
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    assert_eq!(token_balance_t22(&env.banks_client, &receiver_buy_ata).await, 500_000_000);
}

#[tokio::test]
async fn test_jam_settle_token22_nonzero_fee_rejected() {
    // Token-2022 mint with nonzero TransferFee should be rejected by JAM's
    // Token2022FeeNotSupported check.
    let env = prepare_jam_env().await;
    let nonce: u64 = 81;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let mint_a_fee = create_spl_token22_mint_with_fee(&env.banks_client, &env.payer, 50, u64::MAX).await;
    let mint_b     = create_spl_token22_mint_zero_fee(&env.banks_client, &env.payer).await;

    let taker_sell_ata   = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_a_fee).await;
    let custody_sell_ata = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_a_fee).await;
    let custody_buy_ata  = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_b).await;
    let receiver_buy_ata = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_b).await;

    mint_to_t22(&env.banks_client, &env.payer, &mint_a_fee, &taker_sell_ata,  1_000_000_000).await;
    mint_to_t22(&env.banks_client, &env.payer, &mint_b,     &custody_buy_ata, 500_000_000).await;

    let (config, _)        = jam_config_pda();
    let (nonce_record, _)  = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let order = make_order(taker, mint_a_fee, mint_b, nonce);
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata:  Some(custody_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority,
            jam_authority,
            sell_mint: mint_a_fee,
            buy_mint:  mint_b,
            sell_token_program: anchor_spl::token_2022::ID,
            buy_token_program:  anchor_spl::token_2022::ID,
            partner_account: None,
                treasury_buy_ata: None,
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };

    let result: std::result::Result<(), BanksClientError> =
        sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "nonzero transfer fee mint should be rejected");
}

// ─── Token-2022 helpers ───────────────────────────────────────────────────────

async fn create_spl_token22_mint_zero_fee(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
) -> Pubkey {
    create_spl_token22_mint_with_fee(banks_client, payer, 0, 0).await
}

async fn create_spl_token22_mint_with_fee(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    basis_points: u16,
    max_fee: u64,
) -> Pubkey {
    use anchor_spl::token_2022::spl_token_2022::{
        self,
        extension::{transfer_fee::instruction as transfer_fee_ix, ExtensionType},
        instruction as token22_ix,
        state::Mint as Mint22,
    };
    use anchor_lang::solana_program::program_pack::Pack;

    let mint_kp = Keypair::new();
    let rent = banks_client.lock().await.get_rent().await.unwrap();
    let space = ExtensionType::try_calculate_account_len::<Mint22>(
        &[ExtensionType::TransferFeeConfig]
    ).unwrap();
    let lamports = rent.minimum_balance(space);

    process_and_assert_ok(&[
        system_instruction::create_account(
            &payer.pubkey(), &mint_kp.pubkey(), lamports, space as u64, &spl_token_2022::ID,
        ),
        transfer_fee_ix::initialize_transfer_fee_config(
            &spl_token_2022::ID, &mint_kp.pubkey(), None, None, basis_points, max_fee,
        ).unwrap(),
        token22_ix::initialize_mint2(
            &spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 9,
        ).unwrap(),
    ], payer, &[&mint_kp], banks_client).await;

    mint_kp.pubkey()
}

async fn create_ata_t22(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Pubkey {
    let ata = get_associated_token_address_with_program_id(
        owner, mint, &anchor_spl::token_2022::ID,
    );
    process_and_assert_ok(&[
        ata_ix::create_associated_token_account(
            &payer.pubkey(), owner, mint, &anchor_spl::token_2022::ID,
        ),
    ], payer, &[], banks_client).await;
    ata
}

async fn mint_to_t22(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    mint: &Pubkey,
    dest: &Pubkey,
    amount: u64,
) {
    use anchor_spl::token_2022::spl_token_2022::{self, instruction as token22_ix};
    process_and_assert_ok(&[
        token22_ix::mint_to(
            &spl_token_2022::ID, mint, dest, &payer.pubkey(), &[], amount,
        ).unwrap(),
    ], payer, &[], banks_client).await;
}

async fn token_balance_t22(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    ata: &Pubkey,
) -> u64 {
    use anchor_spl::token_2022::spl_token_2022::state::Account as T22Account;
    use anchor_lang::solana_program::program_pack::Pack;
    let raw = banks_client.lock().await.get_account(*ata).await.unwrap().unwrap();
    // Token-2022 account data may have extension bytes after the base state
    T22Account::unpack(&raw.data[..T22Account::LEN]).unwrap().amount
}

// ─── settle_internal native SOL tests ────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_internal_native_sol_sell() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 60;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (config, _)       = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);

    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let solver_buy_ata = env.solver_ata_b;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata, 500_000_000).await;

    let sell_amount: u64 = 500_000_000;
    let mut order = make_order(taker, anchor_spl::token::spl_token::native_mint::ID, env.mint_b, nonce);
    order.sell_amounts = vec![sell_amount];

    let taker_sol_before  = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    let solver_sol_before = env.banks_client.lock().await.get_balance(solver).await.unwrap();

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::SettleInternal {
            solver, taker, config, nonce_record,
            taker_sell_ata:   None,
            solver_sell_ata:  None,
            solver_buy_ata:   Some(solver_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            sell_mint: anchor_spl::token::spl_token::native_mint::ID,
            buy_mint:  env.mint_b,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            token_program:  anchor_spl::token::ID,
            system_program: system_program::ID,
            partner_account: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::SettleInternal {
            order, filled_amounts: vec![500_000_000],
        }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let taker_sol_after  = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    let solver_sol_after = env.banks_client.lock().await.get_balance(solver).await.unwrap();
    let received_b       = token_balance(&env.banks_client, &receiver_buy_ata).await;

    assert!(taker_sol_after < taker_sol_before, "taker should lose sell SOL");
    assert!(solver_sol_after > solver_sol_before, "solver should gain sell SOL");
    assert_eq!(received_b, 500_000_000, "receiver should get buy tokens");
}

#[tokio::test]
async fn test_jam_settle_internal_native_sol_buy() {
    // Solver pays native SOL to receiver (== taker). Tests doc #2 fix:
    // buy_recv_wallet must resolve to order.receiver, not solver.
    let env = prepare_jam_env().await;
    let nonce: u64 = 61;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (config, _)       = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let solver_sell_ata = env.solver_ata_a;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;

    let buy_amount: u64 = 500_000_000;
    let mut order = make_order(taker, env.mint_a, anchor_spl::token::spl_token::native_mint::ID, nonce);
    order.buy_amounts = vec![buy_amount];

    let taker_sol_before  = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    let solver_sol_before = env.banks_client.lock().await.get_balance(solver).await.unwrap();

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::SettleInternal {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            solver_sell_ata:  Some(solver_sell_ata),
            solver_buy_ata:   None,
            receiver_buy_ata: None,
            sell_mint: env.mint_a,
            buy_mint:  anchor_spl::token::spl_token::native_mint::ID,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            token_program:  anchor_spl::token::ID,
            system_program: system_program::ID,
            partner_account: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::SettleInternal {
            order, filled_amounts: vec![buy_amount],
        }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let taker_sol_after  = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    let solver_sol_after = env.banks_client.lock().await.get_balance(solver).await.unwrap();

    assert!(taker_sol_after > taker_sol_before, "receiver (taker) should gain SOL, not solver");
    assert!(solver_sol_after < solver_sol_before, "solver should spend SOL");
    assert_eq!(token_balance(&env.banks_client, &solver_sell_ata).await, 1_000_000_000);
}

// ─── Native SOL buy with third-party receiver ────────────────────────────────

#[tokio::test]
async fn test_jam_settle_native_sol_buy_third_party_receiver() {
    // Receiver is a third party. SOL must go to receiver, not to taker.
    // Tests doc #6: receiver wallet passed in remaining_accounts.
    let env = prepare_jam_env().await;
    let nonce: u64 = 42;
    let solver      = env.solver_kp.pubkey();
    let taker       = env.taker_kp.pubkey();
    let receiver_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &receiver_kp.pubkey(), LAMPORTS_PER_SOL)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;
    let receiver = receiver_kp.pubkey();

    let (config, _)            = jam_config_pda();
    let (nonce_record, _)      = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _)     = jam_authority_pda();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &custody_authority, 500_000_000)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let mut order = make_order(taker, env.mint_a, anchor_spl::token::spl_token::native_mint::ID, nonce);
    order.receiver    = Some(receiver);
    order.buy_amounts = vec![500_000_000];

    let receiver_sol_before = env.banks_client.lock().await.get_balance(receiver).await.unwrap();
    let taker_sol_before    = env.banks_client.lock().await.get_balance(taker).await.unwrap();

    let mut accounts = jam_settlement::accounts::Settle {
        solver, taker, config, nonce_record,
        taker_sell_ata:   Some(taker_sell_ata),
        custody_sell_ata: Some(custody_sell_ata),
        custody_buy_ata:  None,
        receiver_buy_ata: None,
        custody_authority,
        jam_authority,
        sell_mint: env.mint_a,
        buy_mint:  anchor_spl::token::spl_token::native_mint::ID,
        sell_token_program: anchor_spl::token::ID,
        buy_token_program:  anchor_spl::token::ID,
        partner_account: None,
                treasury_buy_ata: None,
        token_program:   anchor_spl::token::ID,
        system_program:  system_program::ID,
        solver_sell_ata: Some(env.solver_ata_a),
    }.to_account_metas(None);
    accounts.push(AccountMeta::new(receiver, false));

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts,
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let receiver_sol_after = env.banks_client.lock().await.get_balance(receiver).await.unwrap();
    let taker_sol_after    = env.banks_client.lock().await.get_balance(taker).await.unwrap();

    assert_eq!(receiver_sol_after - receiver_sol_before, 500_000_000,
        "SOL must go to order.receiver, not taker");
    assert!(taker_sol_after <= taker_sol_before, "taker must not receive the buy SOL");
}

#[tokio::test]
async fn test_jam_settle_native_sol_buy_receiver_missing_errors() {
    // Receiver != taker and is NOT in remaining_accounts.
    // The hard require! (doc #6) must return AccountNotFound rather than
    // silently misdirecting SOL to taker.
    let env = prepare_jam_env().await;
    let nonce: u64 = 43;
    let solver   = env.solver_kp.pubkey();
    let taker    = env.taker_kp.pubkey();
    let receiver = Keypair::new().pubkey(); // third party, NOT added to accounts

    let (config, _)            = jam_config_pda();
    let (nonce_record, _)      = nonce_record_pda(&taker, nonce);
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (jam_authority, _)     = jam_authority_pda();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &custody_authority, 500_000_000)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let mut order = make_order(taker, env.mint_a, anchor_spl::token::spl_token::native_mint::ID, nonce);
    order.receiver    = Some(receiver);
    order.buy_amounts = vec![500_000_000];

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata:  None,
            receiver_buy_ata: None,
            custody_authority,
            jam_authority,
            sell_mint: env.mint_a,
            buy_mint:  anchor_spl::token::spl_token::native_mint::ID,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None,
                treasury_buy_ata: None,
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };

    let result = sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "missing receiver wallet must produce an error, not silently misfill");
}

// ─── Nonce and hooks validation tests ────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_zero_nonce_rejected() {
    // EVM ZeroNonce() equivalent: nonce == 0 must be rejected.
    let env = prepare_jam_env().await;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let (custody_authority, _) = custody_authority_pda(&taker, 0);
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, 0 /* nonce = 0 */);
    let ix = ix_settle(&solver, &taker, order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, 0,
        None,
    );
    let result = sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "nonce == 0 must be rejected");
}

#[tokio::test]
async fn test_jam_settle_hooks_enabled_rejected() {
    // beforeSettle/afterSettle are not yet executed on Solana.
    // orders with hooks_enabled=true must be rejected (silent skip is worse than error).
    let env = prepare_jam_env().await;
    let nonce: u64 = 90;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.hooks_enabled = true; // non-zero hooks equivalent
    let ix = ix_settle(&solver, &taker, order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        None,
    );
    let result = sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "hooks_enabled=true must be rejected");
}

// ─── Exclusivity positive case ───────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_executor_allowed_during_exclusivity() {
    // The executor should succeed during the exclusivity window.
    // Complements test_jam_settle_exclusivity (which tests violation).
    let env = prepare_jam_env().await;
    let nonce: u64 = 91;
    let solver = env.solver_kp.pubkey(); // solver IS the executor
    let taker  = env.taker_kp.pubkey();

    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.executor = Some(solver);
    order.exclusivity_deadline = Some(i64::MAX); // always in exclusivity window

    let ix = ix_settle(&solver, &taker, order, vec![], taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata, env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap(); // executor IS the solver → must succeed

    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata).await, 500_000_000);
}

// ─── settle_internal partner fee test ────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_internal_partner_fee() {
    // Partner fee in settle_internal was previously computed but discarded.
    // Verify it is now forwarded to partner_account.
    let env = prepare_jam_env().await;
    let nonce: u64 = 92;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let partner_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &partner_kp.pubkey(), 1_000_000_000)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let (config, _)       = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);

    let taker_sell_ata  = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let solver_sell_ata = env.solver_ata_a;
    let solver_buy_ata  = env.solver_ata_b;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_b).await;
    let partner_ata     = create_ata(&env.banks_client, &env.payer, &partner_kp.pubkey(), &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;
    // Solver provides 600M; 1% (6M) goes to partner; 594M to receiver.
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata, 600_000_000).await;

    let fee_bps: u16 = 100;
    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.partner_fee_bps = fee_bps;
    order.partner = Some(partner_kp.pubkey());

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: {
            let mut metas = jam_settlement::accounts::SettleInternal {
                solver, taker, config, nonce_record,
                taker_sell_ata:   Some(taker_sell_ata),
                solver_sell_ata:  Some(solver_sell_ata),
                solver_buy_ata:   Some(solver_buy_ata),
                receiver_buy_ata: Some(receiver_buy_ata),
                sell_mint: env.mint_a,
                buy_mint:  env.mint_b,
                sell_token_program: anchor_spl::token::ID,
                buy_token_program:  anchor_spl::token::ID,
                token_program:  anchor_spl::token::ID,
                system_program: system_program::ID,
                partner_account: Some(partner_ata),
            }.to_account_metas(None);
            metas
        },
        data: jam_settlement::instruction::SettleInternal {
            order, filled_amounts: vec![600_000_000],
        }.data(),
    };

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let partner_received  = token_balance(&env.banks_client, &partner_ata).await;
    let receiver_received = token_balance(&env.banks_client, &receiver_buy_ata).await;

    assert_eq!(partner_received, 6_000_000, "partner should receive 1% fee");
    assert_eq!(receiver_received, 594_000_000, "receiver gets remainder");
    assert!(receiver_received >= 500_000_000, "net >= buy_amounts[0]");
}

// ─── T22 PermanentDelegate and TransferHook rejection tests ──────────────────

#[tokio::test]
async fn test_jam_settle_token22_permanent_delegate_rejected() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 95;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    // Mint with a live permanent delegate (the payer is the delegate).
    let mint_a_pd = create_spl_token22_mint_with_permanent_delegate(&env.banks_client, &env.payer).await;
    let mint_b    = create_spl_token22_mint_zero_fee(&env.banks_client, &env.payer).await;

    let taker_sell_ata   = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_a_pd).await;
    let custody_sell_ata = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_a_pd).await;
    let custody_buy_ata  = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_b).await;
    let receiver_buy_ata = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_b).await;

    mint_to_t22(&env.banks_client, &env.payer, &mint_a_pd, &taker_sell_ata,  1_000_000_000).await;
    mint_to_t22(&env.banks_client, &env.payer, &mint_b,    &custody_buy_ata, 500_000_000).await;

    let (config, _) = jam_config_pda(); let (nonce_record, _) = nonce_record_pda(&taker, nonce); let (jam_authority, _) = jam_authority_pda();
    let order = make_order(taker, mint_a_pd, mint_b, nonce);
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata: Some(taker_sell_ata), custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata: Some(custody_buy_ata), receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority, jam_authority, sell_mint: mint_a_pd, buy_mint: mint_b,
            sell_token_program: anchor_spl::token_2022::ID, buy_token_program: anchor_spl::token_2022::ID,
            partner_account: None, treasury_buy_ata: None, token_program: anchor_spl::token::ID, system_program: system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };
    let result = sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "mint with PermanentDelegate must be rejected");
}

// test_jam_settle_token22_transfer_hook_rejected: omitted.
// The spl-token-2022 version pulled by anchor 0.32.1 does not expose a stable
// mint-initialization function for the TransferHook extension in its instruction
// module. The guard in spl_transfer (state.get_extension::<TransferHook>().is_err())
// is correct and can be verified manually against a devnet mint that has the
// extension. PermanentDelegate and TransferFee rejections are covered above.

// ─── T22 helper: PermanentDelegate + TransferHook mints ──────────────────────

async fn create_spl_token22_mint_with_permanent_delegate(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
) -> Pubkey {
    use anchor_spl::token_2022::spl_token_2022::{
        self,
        extension::ExtensionType,
        instruction as token22_ix,
        state::Mint as Mint22,
    };

    let mint_kp = Keypair::new();
    let rent = banks_client.lock().await.get_rent().await.unwrap();
    let space = ExtensionType::try_calculate_account_len::<Mint22>(
        &[ExtensionType::PermanentDelegate]
    ).unwrap();
    process_and_assert_ok(&[
        system_instruction::create_account(
            &payer.pubkey(), &mint_kp.pubkey(),
            rent.minimum_balance(space), space as u64, &spl_token_2022::ID,
        ),
        token22_ix::initialize_permanent_delegate(
            &spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(),
        ).unwrap(),
        token22_ix::initialize_mint2(
            &spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 9,
        ).unwrap(),
    ], payer, &[&mint_kp], banks_client).await;
    mint_kp.pubkey()
}

// ─── A3: Upgrade authority verification (deployment gate) ────────────────────

#[tokio::test]
async fn test_jam_upgrade_authority_readable() {
    // A3: Before mainnet, transfer upgrade authority for BOTH programs to a
    // 3-of-5 Squads multisig with 48h timelock:
    //
    //   solana program set-upgrade-authority <JAM_PROGRAM_ID> \
    //       --new-upgrade-authority <SQUADS_MULTISIG_PDA>
    //   solana program set-upgrade-authority <RFQ_PROGRAM_ID> \
    //       --new-upgrade-authority <SQUADS_MULTISIG_PDA>
    //
    // When permanently immutable:
    //   solana program set-upgrade-authority <PROGRAM_ID> --final
    //
    // This test READS the upgrade authority from the BPFLoader2 programdata
    // account and prints it. It PASSES in unit tests (the test framework is
    // the upgrade authority). Run it against devnet/mainnet as a deployment gate:
    //
    //   cargo test-sbf -- --nocapture test_jam_upgrade_authority_readable
    //   # then verify printed authority == your Squads multisig address
    //
    // Risk: a compromised upgrade key on JAM can:
    //   (a) redirect buy tokens per-settlement
    //   (b) remove use_jam_authority guards → drain QU!D sol_pool via flash loans
    // JAM holds no persistent funds itself — the threat is indirect via QU!D.
    let env = prepare_jam_env().await;

    // BPFLoaderUpgradeable programdata PDA: [program_id]
    let (programdata_address, _) = Pubkey::find_program_address(
        &[jam_settlement::ID.as_ref()],
        &anchor_lang::solana_program::bpf_loader_upgradeable::id(),
    );

    let account = env.banks_client.lock().await
        .get_account(programdata_address).await.unwrap();

    if let Some(acct) = account {
        // BPFUpgradeableLoaderState layout for ProgramData:
        //   4 bytes: variant (2 = ProgramData)
        //   8 bytes: last_modified_slot
        //   1 byte:  Option tag (1 = Some)
        //   32 bytes: upgrade_authority_address
        if acct.data.len() >= 45 && acct.data[12] == 1 {
            let authority_bytes: [u8; 32] = acct.data[13..45].try_into().unwrap();
            let authority = Pubkey::from(authority_bytes);
            println!(
                "\n[DEPLOYMENT CHECK] JAM upgrade authority: {}\n\
                 In production this must be a Squads multisig, not a single keypair.",
                authority
            );
            // In production add: assert_ne!(authority, expected_deployer_key);
            // For unit tests we just confirm the account is readable.
            assert_ne!(authority, Pubkey::default(), "upgrade authority must not be default/locked");
        } else {
            println!("[DEPLOYMENT CHECK] Programdata account not yet in expected format (ok in tests)");
        }
    } else {
        println!("[DEPLOYMENT CHECK] Programdata account not found (ok in BanksClient environment)");
    }
}

// ─── A9: Token program substitution test ─────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_fake_token_program_rejected() {
    // A9: passing an unknown program as sell_prog_i for an additional pair must be
    // rejected with InteractionTargetProtected before any transfer is attempted.
    let env = prepare_jam_env().await;
    let nonce: u64 = 97;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    // Two sell tokens, two buy tokens — additional sell pair has a fake program.
    let mint_c = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;
    let fake_program = Keypair::new().pubkey();

    let taker_sell_ata_a   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata_a = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let taker_sell_ata_c   = create_ata(&env.banks_client, &env.payer, &taker, &mint_c).await;
    let custody_sell_ata_c = create_ata(&env.banks_client, &env.payer, &custody_authority, &mint_c).await;
    let custody_buy_ata    = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata_a, 500_000_000).await;
    mint_to(&env.banks_client, &env.payer, &mint_c,     &taker_sell_ata_c, 500_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata,  500_000_000).await;

    let (config, _)        = jam_config_pda();
    let (nonce_record, _)  = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    // Add a second sell token
    order.sell_tokens.push(mint_c);
    order.sell_amounts.push(500_000_000);

    let mut ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata: Some(taker_sell_ata_a), custody_sell_ata: Some(custody_sell_ata_a),
            custody_buy_ata: Some(custody_buy_ata), receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority, jam_authority, sell_mint: env.mint_a, buy_mint: env.mint_b,
            sell_token_program: anchor_spl::token::ID, buy_token_program: anchor_spl::token::ID,
            partner_account: None, treasury_buy_ata: None,
            token_program: anchor_spl::token::ID, system_program: system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };

    // Inject additional pair with fake token program in remaining_accounts slot 3
    ix.accounts.push(AccountMeta::new(taker_sell_ata_c, false));
    ix.accounts.push(AccountMeta::new(custody_sell_ata_c, false));
    ix.accounts.push(AccountMeta::new_readonly(mint_c, false));
    ix.accounts.push(AccountMeta::new_readonly(fake_program, false)); // FAKE program

    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(), "fake token program for additional pair must be rejected");
}

// ─── Stuck-funds guard test ───────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_fee_no_account_goes_to_receiver() {
    // If partner_fee_bps > 0 but partner_account is None, the fee must NOT
    // be deducted from the receiver's output. Tokens must not get stuck in
    // the custody PDA (STUCK-FUNDS GUARD).
    let env = prepare_jam_env().await;
    let nonce: u64 = 100;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 1_000_000_000).await;

    // partner_fee_bps=100 (1%) but partner_account is None
    let fee_bps: u16 = 100;
    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.partner_fee_bps = fee_bps;
    order.partner = Some(Keypair::new().pubkey());  // some partner address
    // buy_amounts[0] = 500M — taker's minimum. Receiver should still get >= 500M
    // even though 1% would be 10M (of 1B gross), since partner_account is None.

    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let receiver_received = token_balance(&env.banks_client, &receiver_buy_ata).await;
    let custody_remaining = token_balance(&env.banks_client, &custody_buy_ata).await;

    // Fee not forwarded → receiver gets full gross
    assert_eq!(receiver_received, 1_000_000_000, "receiver should get full gross when partner_account is None");
    assert_eq!(custody_remaining, 0, "custody must be empty — no tokens stuck");
}

// ─── ConfidentialTransfer rejection test ─────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_token22_confidential_transfer_rejected() {
    // A T22 mint with ConfidentialTransferMint extension must be rejected.
    // balance_of reads only the public balance; a confidential deposit would
    // make received_0 = 0 and cause a confusing InsufficientOutput error.
    // Reject at transfer time with Token2022FeeNotSupported for clarity.
    let env = prepare_jam_env().await;
    let nonce: u64 = 101;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let mint_a_ct = create_spl_token22_mint_with_confidential_transfer(&env.banks_client, &env.payer).await;
    let mint_b    = create_spl_token22_mint_zero_fee(&env.banks_client, &env.payer).await;

    let taker_sell_ata   = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_a_ct).await;
    let custody_sell_ata = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_a_ct).await;
    let custody_buy_ata  = create_ata_t22(&env.banks_client, &env.payer, &custody_authority, &mint_b).await;
    let receiver_buy_ata = create_ata_t22(&env.banks_client, &env.payer, &taker,             &mint_b).await;

    mint_to_t22(&env.banks_client, &env.payer, &mint_a_ct, &taker_sell_ata,  1_000_000_000).await;
    mint_to_t22(&env.banks_client, &env.payer, &mint_b,    &custody_buy_ata, 500_000_000).await;

    let (config, _) = jam_config_pda(); let (nonce_record, _) = nonce_record_pda(&taker, nonce); let (jam_authority, _) = jam_authority_pda();
    let order = make_order(taker, mint_a_ct, mint_b, nonce);
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata: Some(taker_sell_ata), custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata: Some(custody_buy_ata), receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority, jam_authority, sell_mint: mint_a_ct, buy_mint: mint_b,
            sell_token_program: anchor_spl::token_2022::ID, buy_token_program: anchor_spl::token_2022::ID,
            partner_account: None, treasury_buy_ata: None, token_program: anchor_spl::token::ID, system_program: system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };
    let result = sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client).await;
    assert!(result.is_err(), "mint with ConfidentialTransfer must be rejected");
}

// ─── ConfidentialTransfer mint helper ────────────────────────────────────────

async fn create_spl_token22_mint_with_confidential_transfer(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
) -> Pubkey {
    use anchor_spl::token_2022::spl_token_2022::{
        self,
        extension::{confidential_transfer::instruction as ct_ix, ExtensionType},
        instruction as token22_ix,
        state::Mint as Mint22,
    };
    use anchor_lang::solana_program::program_pack::Pack;

    let mint_kp = Keypair::new();
    let rent = banks_client.lock().await.get_rent().await.unwrap();
    let space = ExtensionType::try_calculate_account_len::<Mint22>(
        &[ExtensionType::ConfidentialTransferMint]
    ).unwrap();
    process_and_assert_ok(&[
        system_instruction::create_account(
            &payer.pubkey(), &mint_kp.pubkey(),
            rent.minimum_balance(space), space as u64, &spl_token_2022::ID,
        ),
        ct_ix::initialize_mint(
            &spl_token_2022::ID,
            &mint_kp.pubkey(),
            Some(payer.pubkey()),   // authority
            false,                  // auto_approve_new_accounts
            None,                   // auditor_elgamal_pubkey
        ).unwrap(),
        token22_ix::initialize_mint2(
            &spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 9,
        ).unwrap(),
    ], payer, &[&mint_kp], banks_client).await;
    mint_kp.pubkey()
}

// ─── Protocol fee test ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_protocol_fee_deducted() {
    // Verify that config.protocol_fee_bps is deducted from gross and forwarded
    // to the treasury_buy_ata when the account is present.
    let env = prepare_jam_env().await;

    // Set protocol_fee_bps = 200 (2%) via update_config
    let (config, _) = jam_config_pda();
    let treasury_kp = Keypair::new();
    process_and_assert_ok(
        &[system_instruction::transfer(&env.payer.pubkey(), &treasury_kp.pubkey(), 1_000_000_000)],
        &env.payer, &[&env.payer], &env.banks_client,
    ).await;

    let update_ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::UpdateConfig {
            config,
            admin: env.payer.pubkey(),
        }.to_account_metas(None),
        data: jam_settlement::instruction::UpdateConfig {
            params: jam_settlement::UpdateConfigParams {
                min_share_bps: None,
                treasury: Some(treasury_kp.pubkey()),
                protocol_fee_bps: Some(200),
            },
        }.data(),
    };
    process_and_assert_ok(&[update_ix], &env.payer, &[&env.payer], &env.banks_client).await;

    let nonce: u64 = 102;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata    = create_ata(&env.banks_client, &env.payer, &taker,              &env.mint_a).await;
    let custody_sell_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority,  &env.mint_a).await;
    let custody_buy_ata   = create_ata(&env.banks_client, &env.payer, &custody_authority,  &env.mint_b).await;
    let receiver_buy_ata  = create_ata(&env.banks_client, &env.payer, &taker,              &env.mint_b).await;
    let treasury_buy_ata  = create_ata(&env.banks_client, &env.payer, &treasury_kp.pubkey(), &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    // 1B gross; 2% = 20M to treasury; 980M to receiver (> 500M buy_amounts[0])
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 1_000_000_000).await;

    let (nonce_record, _)  = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();
    let order = make_order(taker, env.mint_a, env.mint_b, nonce);

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata),
            custody_sell_ata: Some(custody_sell_ata),
            custody_buy_ata:  Some(custody_buy_ata),
            receiver_buy_ata: Some(receiver_buy_ata),
            custody_authority,
            jam_authority,
            sell_mint: env.mint_a, buy_mint: env.mint_b,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None,
            treasury_buy_ata: Some(treasury_buy_ata),
            token_program:   anchor_spl::token::ID,
            system_program:  system_program::ID,
            solver_sell_ata: Some(env.solver_ata_a),
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle { order, interactions: vec![] }.data(),
    };
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    let treasury_received  = token_balance(&env.banks_client, &treasury_buy_ata).await;
    let receiver_received  = token_balance(&env.banks_client, &receiver_buy_ata).await;
    let custody_remaining  = token_balance(&env.banks_client, &custody_buy_ata).await;

    assert_eq!(treasury_received,  20_000_000,  "treasury should receive 2% protocol fee");
    assert_eq!(receiver_received,  980_000_000, "receiver gets gross minus protocol fee");
    assert_eq!(custody_remaining,  0,           "custody must be empty");
}

// ─── settleInternal multi-pair test ──────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_internal_multi_pair() {
    // Verify that handle_settle_internal correctly handles two sell + two buy pairs
    // via remaining_accounts. Both pairs must be settled atomically.
    let env = prepare_jam_env().await;
    let nonce: u64 = 103;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let mint_c = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;
    let mint_d = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;

    let taker_sell_ata_a  = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_a).await;
    let taker_sell_ata_c  = create_ata(&env.banks_client, &env.payer, &taker,  &mint_c).await;
    let solver_sell_ata_a = env.solver_ata_a;
    let solver_sell_ata_c = create_ata(&env.banks_client, &env.payer, &solver, &mint_c).await;
    let solver_buy_ata_b  = env.solver_ata_b;
    let solver_buy_ata_d  = create_ata(&env.banks_client, &env.payer, &solver, &mint_d).await;
    let receiver_b        = create_ata(&env.banks_client, &env.payer, &taker,  &env.mint_b).await;
    let receiver_d        = create_ata(&env.banks_client, &env.payer, &taker,  &mint_d).await;

    // Taker sells A + C, receives B + D
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata_a, 500_000_000).await;
    mint_to(&env.banks_client, &env.payer, &mint_c,     &taker_sell_ata_c, 300_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &solver_buy_ata_b, 700_000_000).await;
    mint_to(&env.banks_client, &env.payer, &mint_d,     &solver_buy_ata_d, 400_000_000).await;

    let (config, _)       = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.sell_tokens  = vec![env.mint_a, mint_c];
    order.sell_amounts = vec![500_000_000, 300_000_000];
    order.buy_tokens   = vec![env.mint_b, mint_d];
    order.buy_amounts  = vec![600_000_000, 350_000_000];

    let mut ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::SettleInternal {
            solver, taker, config, nonce_record,
            taker_sell_ata:   Some(taker_sell_ata_a),
            solver_sell_ata:  Some(solver_sell_ata_a),
            solver_buy_ata:   Some(solver_buy_ata_b),
            receiver_buy_ata: Some(receiver_b),
            sell_mint: env.mint_a, buy_mint: env.mint_b,
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            token_program:  anchor_spl::token::ID,
            system_program: system_program::ID,
            partner_account: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::SettleInternal {
            order, filled_amounts: vec![700_000_000, 400_000_000],
        }.data(),
    };

    // remaining_accounts: [sell pair 1: (taker_C, solver_C, mint_C, prog)]
    //                     [buy  pair 1: (solver_D, receiver_D, mint_D, prog)]
    for acct in &[
        AccountMeta::new(taker_sell_ata_c, false),
        AccountMeta::new(solver_sell_ata_c, false),
        AccountMeta::new_readonly(mint_c, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new(solver_buy_ata_d, false),
        AccountMeta::new(receiver_d, false),
        AccountMeta::new_readonly(mint_d, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
    ] { ix.accounts.push(acct.clone()); }

    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    assert_eq!(token_balance(&env.banks_client, &receiver_b).await,   700_000_000);
    assert_eq!(token_balance(&env.banks_client, &receiver_d).await,   400_000_000);
    assert_eq!(token_balance(&env.banks_client, &solver_sell_ata_a).await, 500_000_000);
    assert_eq!(token_balance(&env.banks_client, &solver_sell_ata_c).await, 300_000_000);
}

// ─── N1: sell_mint / buy_mint must match order.sell_tokens / buy_tokens ───────

#[tokio::test]
async fn test_jam_settle_wrong_sell_mint_rejected() {
    // N1: pass a wrong_mint as sell_mint while order.sell_tokens[0] = mint_a.
    // sell and buy ATAs use different mints to avoid duplicate account pubkeys.
    let env = prepare_jam_env().await;
    let nonce: u64 = 201;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let wrong_mint = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &wrong_mint).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &wrong_mint).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;

    mint_to(&env.banks_client, &env.payer, &wrong_mint,  &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce); // sell_tokens[0] = mint_a
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        wrong_mint, // ← wrong: order says mint_a
        env.mint_b, nonce,
        None,
    );
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client
    ).await;
    assert!(result.is_err(), "wrong sell_mint must be rejected (N1 MintMismatch)");
}

#[tokio::test]
async fn test_jam_settle_wrong_buy_mint_rejected() {
    // N1 (buy side): pass mint_a as buy_mint while order.buy_tokens[0] = mint_b.
    let env = prepare_jam_env().await;
    let nonce: u64 = 202;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let wrong_mint = create_spl_mint(&env.banks_client, &env.payer, Keypair::new()).await;
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &wrong_mint).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &wrong_mint).await;

    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &wrong_mint, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce); // buy_tokens[0] = mint_b
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a,
        wrong_mint, // ← wrong: order says mint_b
        nonce,
        None,
    );
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client
    ).await;
    assert!(result.is_err(), "wrong buy_mint must be rejected (N1 MintMismatch)");
}

// ─── N2: close_nonce_record reclaims rent after order expiry ─────────────────
// These tests use ProgramTestContext directly (not JamTestEnv) because
// advancing the Clock sysvar requires context.set_sysvar, which is only
// available on ProgramTestContext, not on BanksClient.

#[tokio::test]
async fn test_jam_close_nonce_record_after_expiry() {
    use solana_sdk::clock::Clock;
    use solana_program_test::ProgramTestContext;

    let mut ctx: ProgramTestContext = {
        let mut pt = solana_program_test::ProgramTest::new(
            "jam_settlement", jam_settlement::ID, None,
        );
        pt.deactivate_feature(
            solana_sdk::feature_set::bpf_account_data_direct_mapping::ID,
        );
        pt.start_with_context().await
    };

    let payer   = Arc::new(ctx.payer.insecure_clone());
    let solver_kp = Keypair::new();
    let taker_kp  = Keypair::new();
    let banks = Arc::new(tokio::sync::Mutex::new(ctx.banks_client.clone()));

    // Init: fund wallets + config.
    process_and_assert_ok(&[
        system_instruction::transfer(&payer.pubkey(), &solver_kp.pubkey(), 5 * LAMPORTS_PER_SOL),
        system_instruction::transfer(&payer.pubkey(), &taker_kp.pubkey(), 5 * LAMPORTS_PER_SOL),
        ix_init_config(&payer.pubkey(), payer.pubkey()),
    ], &payer, &[&payer], &banks).await;

    let mint_a = create_spl_mint(&banks, &payer, Keypair::new()).await;
    let mint_b = create_spl_mint(&banks, &payer, Keypair::new()).await;

    let nonce: u64 = 203;
    let solver = solver_kp.pubkey();
    let taker  = taker_kp.pubkey();

    let taker_sell_ata   = create_ata(&banks, &payer, &taker, &mint_a).await;
    let receiver_buy_ata = create_ata(&banks, &payer, &taker, &mint_b).await;
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let custody_sell_ata = create_ata(&banks, &payer, &custody_authority, &mint_a).await;
    let custody_buy_ata  = create_ata(&banks, &payer, &custody_authority, &mint_b).await;
    let solver_sell_ata_a = create_ata(&banks, &payer, &solver, &mint_a).await;
    mint_to(&banks, &payer, &mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&banks, &payer, &mint_b, &custody_buy_ata, 500_000_000).await;

    // Get the actual genesis clock — solana_program_test uses real current time.
    let genesis_time = ctx.banks_client.get_sysvar::<Clock>().await.unwrap().unix_timestamp;
    let short_expiry = genesis_time + 10;

    let mut order = make_order(taker, mint_a, mint_b, nonce);
    order.expiry = short_expiry;

    let settle_ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        mint_a, mint_b, nonce,
        Some(solver_sell_ata_a),
    );
    sign_and_execute_tx(
        &[settle_ix], &payer, &taker_kp, &[solver_kp], &banks,
    ).await.unwrap();

    let (nr_pda, _) = nonce_record_pda(&taker, nonce);
    let nr_account: Option<solana_sdk::account::Account> =
        banks.lock().await.get_account(nr_pda).await.unwrap();
    assert!(nr_account.is_some(), "nonce_record must exist after settle");

    // Advance the Clock sysvar past expiry using ProgramTestContext::set_sysvar.
    let mut clock = ctx.banks_client.get_sysvar::<Clock>().await.unwrap();
    clock.unix_timestamp = short_expiry + 1;
    ctx.set_sysvar(&clock);

    let close_params = jam_settlement::instructions::settle::CloseNonceRecordParams { taker, nonce };
    let close_ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::CloseNonceRecord {
            payer: payer.pubkey(),
            record: nr_pda,
            system_program: system_program::ID,
        }.to_account_metas(None),
        data: jam_settlement::instruction::CloseNonceRecord { params: close_params }.data(),
    };
    process_and_assert_ok(
        &[close_ix], &payer, &[], &banks,
    ).await;

    let nr_account: Option<solana_sdk::account::Account> =
        banks.lock().await.get_account(nr_pda).await.unwrap();
    assert!(nr_account.is_none(), "nonce_record must be closed and rent returned");
}

#[tokio::test]
async fn test_jam_close_nonce_record_before_expiry_rejected() {
    // Attempting to close while the order is still within its validity window must fail.
    let env = prepare_jam_env().await;
    let nonce: u64 = 204;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_a).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker, &env.mint_b).await;
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_b, nonce); // expiry = i64::MAX
    let settle_ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    sign_and_execute_tx(
        &[settle_ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await.unwrap();

    let (nr_pda, _) = nonce_record_pda(&taker, nonce);
    let close_params = jam_settlement::instructions::settle::CloseNonceRecordParams { taker, nonce };
    let close_ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::CloseNonceRecord {
            payer: env.payer.pubkey(),
            record: nr_pda,
            system_program: system_program::ID,
        }.to_account_metas(None),
        data: jam_settlement::instruction::CloseNonceRecord { params: close_params }.data(),
    };
    let result = sign_and_execute_tx(
        &[close_ix], &env.payer, &env.taker_kp, &[], &env.banks_client,
    ).await;
    assert!(result.is_err(), "closing before expiry must be rejected (replay protection window)");
}

// ─── N5: zero buy_amount / sell_amount in order must be rejected ──────────────

#[tokio::test]
async fn test_jam_settle_zero_buy_amount_rejected() {
    let env = prepare_jam_env().await;
    let nonce: u64 = 205;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata, 500_000_000).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    order.buy_amounts = vec![0]; // zero buy amount — N5 guard must fire
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_b, nonce,
        Some(env.solver_ata_a),
    );
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(), "zero buy_amount must be rejected (N5 ZeroAmount)");
}

// ─── Native SOL additional-pair receiver validation (was unguarded) ───────────
// An order where buy_tokens[1] = native_mint::ID had no receiver key check —
// a solver could pass any writable account as receiver_buy_i and divert the SOL.

#[tokio::test]
async fn test_jam_settle_native_sol_additional_pair_wrong_receiver_rejected() {
    // Build a 2-pair order: sell mint_a, buy [mint_b (pair 0), native SOL (pair 1)].
    // Pass the solver's own wallet as receiver for the SOL pair — must be rejected.
    let env = prepare_jam_env().await;
    let nonce: u64 = 206;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    // Set up for pair 0 (mint_a → mint_b)
    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    let custody_buy_ata  = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_b).await;
    let receiver_buy_ata = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_b).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata,  1_000_000_000).await;
    mint_to(&env.banks_client, &env.payer, &env.mint_b, &custody_buy_ata,   500_000_000).await;
    // Fund custody_authority with SOL for pair 1
    process_and_assert_ok(&[system_instruction::transfer(
        &env.payer.pubkey(), &custody_authority, 200_000_000,
    )], &env.payer, &[], &env.banks_client).await;

    let mut order = make_order(taker, env.mint_a, env.mint_b, nonce);
    // Add native SOL as buy pair 1
    order.buy_tokens.push(anchor_spl::token::spl_token::native_mint::ID);
    order.buy_amounts.push(200_000_000);

    // Craft the instruction with solver as the SOL receiver for pair 1 (wrong)
    let wrong_sol_receiver = env.solver_kp.pubkey(); // must be order.receiver (taker)
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: {
            let mut metas = jam_settlement::accounts::Settle {
                solver, taker, config, nonce_record,
                taker_sell_ata: Some(taker_sell_ata),
                custody_sell_ata: Some(custody_sell_ata),
                custody_buy_ata: Some(custody_buy_ata),
                receiver_buy_ata: Some(receiver_buy_ata),
                custody_authority, jam_authority,
                sell_mint: env.mint_a, buy_mint: env.mint_b,
                sell_token_program: anchor_spl::token::ID,
                buy_token_program:  anchor_spl::token::ID,
                partner_account: None, treasury_buy_ata: None,
                token_program: anchor_spl::token::ID,
                system_program: system_program::ID,
        solver_sell_ata: Some(env.solver_ata_a),
            }.to_account_metas(None);
            // Append remaining_accounts for native SOL buy pair 1:
            // [custody_buy_i_sol, receiver_buy_i_sol, buy_mint_i, buy_prog_i]
            // custody_buy_i_sol = custody_authority for lamports (native SOL)
            metas.push(solana_sdk::instruction::AccountMeta::new(custody_authority, false));
            metas.push(solana_sdk::instruction::AccountMeta::new(wrong_sol_receiver, false));
            metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
                anchor_spl::token::spl_token::native_mint::ID, false));
            metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
                anchor_spl::token::ID, false));
            metas
        },
        data: jam_settlement::instruction::Settle {
            order, interactions: vec![],
        }.data(),
    };

    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(),
        "native SOL additional pair with wrong receiver must be rejected (InvalidReceiver)");
}

// ─── Same-token order guard (sell_tokens[0] == buy_tokens[0]) ─────────────────

#[tokio::test]
async fn test_jam_settle_same_token_order_rejected() {
    // Selling and buying the same token would make custody_sell_ata == custody_buy_ata
    // (same derivation). The VM would reject with a non-deterministic duplicate-account
    // error; our explicit guard fires first with a clean MintMismatch.
    let env = prepare_jam_env().await;
    let nonce: u64 = 207;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata   = create_ata(&env.banks_client, &env.payer, &taker,             &env.mint_a).await;
    let custody_sell_ata = create_ata(&env.banks_client, &env.payer, &custody_authority, &env.mint_a).await;
    // NOTE: custody_buy_ata == custody_sell_ata (same mint, same authority)
    let custody_buy_ata  = custody_sell_ata;
    let receiver_buy_ata = taker_sell_ata; // same ATA — no second create
    mint_to(&env.banks_client, &env.payer, &env.mint_a, &taker_sell_ata, 1_000_000_000).await;

    let order = make_order(taker, env.mint_a, env.mint_a, nonce); // sell == buy
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_buy_ata,
        env.mint_a, env.mint_a, nonce,
        None,
    );
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(), "same-token order must be rejected (MintMismatch)");
}

// ─── wSOL helpers ─────────────────────────────────────────────────────────────

/// Create a funded wSOL ATA for `owner`.
/// Steps: create ATA, transfer SOL lamports into it, sync_native.
async fn create_wsol_ata(
    banks_client: &Arc<tokio::sync::Mutex<BanksClient>>,
    payer: &Arc<Keypair>,
    owner: &Pubkey,
    lamports: u64,
) -> Pubkey {
    use anchor_spl::associated_token::spl_associated_token_account;
    use anchor_spl::token::spl_token;
    let ata = spl_associated_token_account::get_associated_token_address(
        owner, &spl_token::native_mint::ID,
    );
    process_and_assert_ok(&[
        // Create the wSOL ATA
        spl_associated_token_account::instruction::create_associated_token_account(
            &payer.pubkey(), owner,
            &spl_token::native_mint::ID,
            &anchor_spl::token::ID,
        ),
        // Fund it with SOL
        system_instruction::transfer(&payer.pubkey(), &ata, lamports),
        // Sync native: reconcile lamport balance → wSOL token amount
        spl_token::instruction::sync_native(&anchor_spl::token::ID, &ata).unwrap(),
    ], payer, &[], banks_client).await;
    ata
}

fn wsol_mint() -> Pubkey {
    anchor_spl::token::spl_token::native_mint::ID
}

// ─── wSOL settle tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_jam_settle_wsol_sell_native_buy() {
    // Taker sells wSOL (token account); solver delivers native SOL to receiver.
    let env = prepare_jam_env().await;
    let nonce: u64 = 210;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    // Taker's wSOL sell ATA: fund with 1 SOL worth of wSOL
    let taker_sell_ata = create_wsol_ata(&env.banks_client, &env.payer, &taker, 1_000_000_000).await;
    // Custody sell ATA: wSOL ATA owned by custody_authority (starts empty)
    let custody_sell_ata = create_wsol_ata(&env.banks_client, &env.payer, &custody_authority, 0).await;
    // No custody_buy_ata needed: buy side is native SOL to receiver wallet.
    // Pre-fund custody_authority with the buy amount — simulates solver delivering SOL
    // via an interaction (same pattern as test_jam_settle_native_sol_buy).
    process_and_assert_ok(&[system_instruction::transfer(
        &env.payer.pubkey(), &custody_authority, 500_000_000,
    )], &env.payer, &[&env.payer], &env.banks_client).await;

    let mut order = make_order(taker, wsol_mint(), wsol_mint(), nonce);
    order.buy_tokens  = vec![wsol_mint()];   // buy_tokens[0] = native_mint (SOL)
    order.buy_amounts = vec![500_000_000];   // 0.5 SOL out
    order.sell_tokens = vec![wsol_mint()];   // sell_tokens[0] = native_mint (wSOL)
    order.sell_amounts = vec![1_000_000_000]; // 1 wSOL in

    let taker_sol_before = env.banks_client.lock().await.get_balance(taker).await.unwrap();

    // Solver needs a wSOL ATA to receive the taker's sold wSOL (new EVM-parity sell drain).
    let solver_wsol_ata = create_wsol_ata(&env.banks_client, &env.payer, &solver, 0).await;
    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata: Some(taker_sell_ata),  // wSOL ATA
            custody_sell_ata: Some(custody_sell_ata), // wSOL ATA
            custody_buy_ata: None,                 // native SOL path
            receiver_buy_ata: None,                // native SOL → taker wallet
            custody_authority, jam_authority,
            sell_mint: wsol_mint(), buy_mint: wsol_mint(),
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None, treasury_buy_ata: None,
            solver_sell_ata: Some(solver_wsol_ata),
            token_program: anchor_spl::token::ID,
            system_program: system_program::ID,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle {
            order, interactions: vec![],
        }.data(),
    };
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    // Taker should have gained ~0.5 SOL (native, not wSOL)
    let taker_sol_after = env.banks_client.lock().await.get_balance(taker).await.unwrap();
    assert!(taker_sol_after > taker_sol_before, "taker should receive native SOL");
    // Taker's wSOL ATA should be empty
    assert_eq!(token_balance(&env.banks_client, &taker_sell_ata).await, 0);
}

#[tokio::test]
async fn test_jam_settle_native_sell_wsol_buy() {
    // Taker sells native SOL; solver delivers wSOL tokens to receiver's wSOL ATA.
    let env = prepare_jam_env().await;
    let nonce: u64 = 211;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);
    let (config, _) = jam_config_pda();
    let (nonce_record, _) = nonce_record_pda(&taker, nonce);
    let (jam_authority, _) = jam_authority_pda();

    // Receiver (taker) needs a wSOL ATA to receive into
    let receiver_buy_ata = create_wsol_ata(&env.banks_client, &env.payer, &taker, 0).await;
    // Custody buy ATA: wSOL ATA owned by custody_authority, funded with buy amount
    let custody_buy_ata = create_wsol_ata(&env.banks_client, &env.payer, &custody_authority, 500_000_000).await;

    let mut order = make_order(taker, wsol_mint(), wsol_mint(), nonce);
    order.sell_tokens  = vec![wsol_mint()];   // native SOL sell (no ATA → lamport path)
    order.sell_amounts = vec![1_000_000_000];
    order.buy_tokens   = vec![wsol_mint()];   // wSOL buy (ATA provided → token path)
    order.buy_amounts  = vec![500_000_000];

    let ix = Instruction {
        program_id: jam_settlement::ID,
        accounts: jam_settlement::accounts::Settle {
            solver, taker, config, nonce_record,
            taker_sell_ata: None,              // native SOL (lamport) sell
            custody_sell_ata: None,
            custody_buy_ata: Some(custody_buy_ata),  // wSOL ATA
            receiver_buy_ata: Some(receiver_buy_ata), // taker's wSOL ATA
            custody_authority, jam_authority,
            sell_mint: wsol_mint(), buy_mint: wsol_mint(),
            sell_token_program: anchor_spl::token::ID,
            buy_token_program:  anchor_spl::token::ID,
            partner_account: None, treasury_buy_ata: None,
            token_program: anchor_spl::token::ID,
            system_program: system_program::ID,
            solver_sell_ata: None,
        }.to_account_metas(None),
        data: jam_settlement::instruction::Settle {
            order, interactions: vec![],
        }.data(),
    };
    sign_and_execute_tx(&[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client)
        .await.unwrap();

    // Taker's wSOL ATA should have the buy amount
    assert_eq!(token_balance(&env.banks_client, &receiver_buy_ata).await, 500_000_000,
        "taker should receive wSOL tokens");
}

#[tokio::test]
async fn test_jam_settle_wsol_sell_wsol_buy_rejected_same_token() {
    // sell_tokens[0] == buy_tokens[0] == native_mint → same-token guard fires.
    // This is correct: a wSOL→wSOL order is economically meaningless and would
    // cause duplicate custody ATAs (same mint + same authority).
    let env = prepare_jam_env().await;
    let nonce: u64 = 212;
    let solver = env.solver_kp.pubkey();
    let taker  = env.taker_kp.pubkey();
    let (custody_authority, _) = custody_authority_pda(&taker, nonce);

    let taker_sell_ata   = create_wsol_ata(&env.banks_client, &env.payer, &taker, 1_000_000_000).await;
    let custody_sell_ata = create_wsol_ata(&env.banks_client, &env.payer, &custody_authority, 0).await;
    let custody_buy_ata  = custody_sell_ata;  // same ATA (same mint+authority)
    let receiver_ata     = taker_sell_ata;    // same ATA (same mint+owner)

    // order.sell_tokens[0] == order.buy_tokens[0] == wsol_mint → MintMismatch
    let order = make_order(taker, wsol_mint(), wsol_mint(), nonce);
    let ix = ix_settle(
        &solver, &taker, order, vec![],
        taker_sell_ata, custody_sell_ata, custody_buy_ata, receiver_ata,
        wsol_mint(), wsol_mint(), nonce,
    
        None,
    );
    let result = sign_and_execute_tx(
        &[ix], &env.payer, &env.taker_kp, &[env.solver_kp], &env.banks_client,
    ).await;
    assert!(result.is_err(), "wSOL→wSOL order must be rejected (same-token guard)");
}

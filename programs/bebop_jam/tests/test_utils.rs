use std::sync::Arc;

use anchor_lang::solana_program::instruction::Instruction;
use solana_program_test::{tokio::sync::Mutex, BanksClient, BanksClientError};
use solana_sdk::{
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signature::Signer,
    transaction::Transaction,
};

/// Workaround from anchor issue https://github.com/coral-xyz/anchor/issues/2738#issuecomment-2230683481

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

pub async fn process_and_assert_ok(
    instructions: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
    banks_client: &Mutex<BanksClient>,
) {
    let mut bc = banks_client.lock().await;
    let recent_blockhash = bc.get_latest_blockhash().await.unwrap();
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(
        instructions, Some(&payer.pubkey()), &all_signers, recent_blockhash,
    );
    bc.process_transaction(tx).await.unwrap();
}

pub async fn sign_and_execute_tx(
    instructions: &[Instruction],
    payer: &Keypair,
    taker: &Keypair,
    makers: &[Keypair],
    banks_client: &Mutex<BanksClient>,
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

use anchor_lang::error_code;

#[error_code]
pub enum JamError {
    OrderExpired,
    InsufficientOutput,          // received < buyAmounts[i], or net-after-fees < buyAmounts[i]
    InvalidTaker,                // taker != order.taker
    ExclusivityViolation,        // settled by non-executor during exclusivity window
    ZeroAmount,
    LengthMismatch,              // sellTokens.len() != sellAmounts.len()
    InteractionFailed,           // interaction.result == true and CPI failed
    InteractionTargetProtected,  // interaction.program == custody_authority, JAM itself,
                                 // system_program, spl_token, spl_token_2022 (blocks
                                 // taker SOL/SPL drainage via propagated signer), or
                                 // unknown token program for additional pairs (A9)
    AccountNotFound,             // interaction account not in remaining_accounts,
                                 // or missing pending_admin in accept_admin
    InvalidPartnerFee,           // partner_fee_bps > 10_000
    Token2022FeeNotSupported,    // non-zero transfer fee, PermanentDelegate, or TransferHook
    InvalidReceiver,             // receiver_buy_ata.owner != order.receiver, or
                                 // additional-pair receiver_buy_i.owner != order.receiver.
                                 // Also: treasury_buy_ata.owner != config.treasury (A13).
                                 // Prevents a solver routing buy tokens to any ATA for the
                                 // correct mint — including their own — while all balance
                                 // checks pass. The taker's order signature is not sufficient
                                 // protection: it covers the order struct, not the account
                                 // pubkeys passed to the instruction at settlement time.
    ZeroNonce,                   // order.nonce == 0; mirrors EVM ZeroNonce() require
    MintMismatch,                 // sell_mint.key() != order.sell_tokens[0], or
                                 // buy_mint.key() != order.buy_tokens[0].
                                 // Defense-in-depth: taker co-signs the full tx so
                                 // a solver cannot substitute mints after signing,
                                 // but an explicit on-chain check is cleaner than
                                 // relying solely on the taker signature.
    HooksNotSupported,           // hooks_enabled == true; beforeSettle/afterSettle not
                                 // yet executed on Solana. Orders must use EMPTY_HOOKS_HASH.
                                 // Accepted and planned; currently rejected to prevent
                                 // silent no-op of hooks the taker believed would run.
    RecordNotExpired,            // close_nonce_record called before order.expiry has passed;
                                 // the record is still within its replay-protection window.

}

// state.rs — JAM settlement program data model
//
// ┌─────────────────────────────────────────────────────────────────────────┐
// │  EVM ↔ Solana architectural map                                         │
// │                                                                         │
// │  JamSettlement.sol       jam_settlement (this program)                  │
// │  ─────────────────       ───────────────────────────                    │
// │  JamOrder (struct)   →   SolanaJamOrder                                 │
// │  JamInteraction.Data →   SolanaInteraction                              │
// │  EIP-712 taker sig   →   taker co-signs Solana transaction              │
// │  msg.sender==executor→   solver: Signer<'info> + require_keys_eq!       │
// │                                                                         │
// │  Solana RFQ asymmetry (bebop_rfq program):                              │
// │    Vec<AmountWithExpiry> — degrading quote ladder keyed by block time.  │
// │    BebopSettlement.sol has no equivalent: single maker_amount only.     │
// │    Solana's deterministic ~400ms slots make time-bucketed quotes useful.│
// │    JAM does not use AmountWithExpiry — it is purely a PMM (RFQ) feature.│
// │                                                                         │
// │  Blend path asymmetry:                                                  │
// │    EVM: JamSettlement.settleBebopBlend() calls BebopSettlement at a     │
// │    hardcoded address.                                                    │
// │    Solana: solver includes bebop_rfq::Swap as a SolanaInteraction.      │
// │    More general — any program (including bebop_rfq) is reachable via    │
// │    the interactions array without an explicit binding.                   │
// │                                                                         │
// │  Multi-token array model:                                               │
// │    EVM BebopSettlement.swapMulti(): arrays in one call.                 │
// │    Solana RFQ: chained Swap instructions via shared_pda (idiomatic      │
// │    Solana — accounts declared per-instruction).                         │
// │    JAM (both chains): arrays in one call. Solana uses remaining_accounts│
// │    for additional token pairs. Atomicity is identical either way.       │
// └─────────────────────────────────────────────────────────────────────────┘

use anchor_lang::prelude::*;

pub const JAM_CONFIG_SEED: &[u8] = b"jam_config";
pub const JAM_AUTHORITY_SEED: &[u8] = b"jam_authority";
pub const NONCE_SEED: &[u8] = b"nonce";
pub const CUSTODY_SEED: &[u8] = b"custody";

// ─── SolanaJamOrder ───────────────────────────────────────────────────────────
// Matches JamOrderLib.sol ORDER_TYPE exactly (confirmed from EIP-712 type string):
//   JamOrder(address taker, address receiver, uint256 expiry,
//            uint256 exclusivityDeadline, uint256 nonce, address executor,
//            uint256 partnerInfo, address[] sellTokens, address[] buyTokens,
//            uint256[] sellAmounts, uint256[] buyAmounts, bytes32 hooksHash)
//
// Intentional omissions (EVM-only constructs):
//   usingPermit2: bool   — Permit2 is an EVM contract; taker co-signs the tx instead
//   hooksHash in struct  — it's a parameter to hash(), not a field in JamOrderLib.sol
//                          (present here as a field for off-chain signing compat)
//
// Solana type substitutions (VM requirements):
//   address (20 bytes) → Pubkey (32 bytes)
//   uint256            → u64 for amounts/nonce (SPL convention; sufficient range)
//   uint256 timestamps → i64 (Clock::get().unix_timestamp is i64)
//
// partner: extra Solana field — partnerInfo on EVM is uint256 with packed address.
//   On Solana a full Pubkey can't fit in u64, so partner address is stored separately.

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct SolanaJamOrder {
    pub taker: Pubkey,
    /// Buy token destination. None = taker (saves 31 bytes for the common case).
    pub receiver: Option<Pubkey>,
    pub expiry: i64,
    /// Exclusivity window end. None = no exclusivity (saves 7 bytes vs 0i64 sentinel).
    pub exclusivity_deadline: Option<i64>,  // 0 = no exclusivity
    pub nonce: u64,
    pub executor: Option<Pubkey>,   // None = anyone may settle
    /// Partner fee in basis points (0–10_000). Replaces EVM's packed partner_info: u256
    /// where bits 16-31 encoded the fee and bits 32+ encoded the partner address.
    /// On Solana the address lives in `partner: Option<Pubkey>` — separate field, no packing.
    pub partner_fee_bps: u16,          // bps, 0–10_000
    pub partner: Option<Pubkey>,    // Solana addition: full partner address
    pub sell_tokens: Vec<Pubkey>,   // native SOL = native_mint::ID
    pub buy_tokens: Vec<Pubkey>,    // native SOL = native_mint::ID
    pub sell_amounts: Vec<u64>,
    pub buy_amounts: Vec<u64>,      // minimums (slippage floor)
    /// Replaces EVM's hooksHash: [u8;32] (saves 31 bytes — hooks are not yet executed
    /// on Solana). false = no hooks (required); true = hooks requested but rejected
    /// with HooksNotSupported until Solana hook execution is implemented.
    pub hooks_enabled: bool,
}

// ─── SolanaInteraction ────────────────────────────────────────────────────────
// Matches JamInteraction.Data exactly:
//   struct Data { bool result; address to; uint256 value; bytes data; }
//
// Intentional omissions:
//   value: uint256 — ETH forwarding. On Solana, native SOL transfers are encoded
//     as system_program::transfer CPIs in the instruction data, not forwarded
//     implicitly. Any interaction needing to move SOL includes a system_program
//     transfer in its data field.
//
// Solana addition:
//   accounts: Vec<InteractionAccount> — Solana CPIs require all accounts declared
//     before execution. EVM has no equivalent (storage slots are implicit).
//     The off-chain solver populates this from the target program's IDL.
//
// Flash loans (SOL and future SPL) targeting QU!D:
//   Solver sets use_jam_authority: true on flash_borrow and flash_repay
//   interactions. run_interactions dispatches with invoke_signed so QU!D sees
//   jam_authority.is_signer == true, satisfying its flash_authority constraint.
//   No discriminator detection needed — the flag is the entire mechanism.
//   EVM equivalent: msg.sender == JamSettlement automatically when JAM calls Aux.

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct SolanaInteraction {
    pub result: bool,                       // true = CPI must succeed (revert on fail)
    pub program_index: u16,                 // remaining_accounts index for the target program
    pub accounts: Vec<InteractionAccount>,  // Solana VM requirement — no EVM equivalent
    pub data: Vec<u8>,                      // JamInteraction.Data: bytes data
    /// Opt-in signer delegation: when true, JAM appends its authority PDA as a
    /// signer on this CPI via invoke_signed. When false, plain invoke is used.
    ///
    /// Security: unconditionally signing every CPI created an unbounded
    /// signer-for-hire surface — any program that gates a privileged instruction
    /// on jam_authority.is_signer became callable by any solver who could craft a
    /// valid order. Setting this flag to false for standard interactions (DEX
    /// swaps, token transfers) removes that exposure. Only QU!D flash-borrow
    /// calls and similar JAM-authority-gated instructions need true.
    pub use_jam_authority: bool,
}

/// One account in a CPI, referenced by u8 index into remaining_accounts.
/// u8 index (1 byte) + flags byte replaces Pubkey (32 bytes): saves 30 bytes per account.
/// 5 interactions × 5 accounts × 30 bytes = 750 bytes saved → complex arb fits in 1232-byte tx.
impl SolanaJamOrder {
    /// Resolves receiver: None means the taker receives the buy tokens.
    #[inline] pub fn effective_receiver(&self) -> Pubkey {
        self.receiver.unwrap_or(self.taker)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InteractionAccount {
    pub account_index: u8, // position in remaining_accounts (max ~64 accounts per tx)
    /// Packed flags: bit 0 = is_writable, bit 1 = is_signer.
    /// Saves 1 byte vs two separate bools (Borsh: 1 byte each).
    pub flags: u8,
}
impl InteractionAccount {
    #[inline] pub fn is_writable(&self) -> bool { self.flags & 0x01 != 0 }
    #[inline] pub fn is_signer(&self)   -> bool { self.flags & 0x02 != 0 }
    pub fn new(account_index: u8, is_writable: bool, is_signer: bool) -> Self {
        Self { account_index, flags: (is_writable as u8) | ((is_signer as u8) << 1) }
    }
}

// ─── JamConfig ────────────────────────────────────────────────────────────────

// ─── A3 / A4: Admin key and upgrade authority — Squads v4 multisig ───────────
//
// Neither jam_settlement nor bebop_rfq hold persistent user funds — there is
// nothing in JAM itself to drain. The multisig is required because:
//   (a) a malicious upgrade can redirect buy tokens on future settlements
//   (b) stripping use_jam_authority guards turns JAM into an unbounded
//       signer-for-hire, letting any solver drain QU!D's sol_pool via flash
//       loans — the multisig protects QU!D through JAM's authority delegation.
// Full threat model and setup procedure: see admin_timelock.rs.
//
// The canonical Solana solution for both threats is Squads v4 — an audited,
// open-source multisig program used in production by major protocols:
//   Repo:     https://github.com/Squads-Protocol/v4
//   Docs:     https://docs.squads.so/
//   Mainnet:  SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf8
//   npm SDK:  @sqds/multisig
//   Rust SDK: squads-multisig (crates.io)
//
// How to protect the admin key (no on-chain JAM code changes needed):
//   1. Create a Squads multisig with your desired signers and threshold.
//   2. Derive the vault PDA (index 0):
//        seeds = [b"vault", multisig_pda, &0u8.to_le_bytes()]
//        program = SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf8
//   3. init_config with admin = vault PDA (not a hot wallet keypair).
//   4. Any update_config call must be executed via a Squads transaction —
//      the vault PDA signs the Solana transaction, satisfying has_one = admin.
//      No changes to this program are needed; Anchor treats the vault PDA like
//      any other signer.
//   Ref: https://docs.squads.so/squads-v4/development/sdk
//
// How to protect the program upgrade authority:
//   Ref: https://solana.com/docs/programs/deploying#upgrade-authority
//
//   # Transfer upgrade authority to Squads vault PDA
//   solana program set-upgrade-authority <JAM_PROGRAM_ID> \
//       --new-upgrade-authority <SQUADS_VAULT_PDA>
//   solana program set-upgrade-authority <RFQ_PROGRAM_ID> \
//       --new-upgrade-authority <SQUADS_VAULT_PDA>
//
//   # When permanently immutable (irreversible):
//   solana program set-upgrade-authority <PROGRAM_ID> --final
//
// For a CI/deployment gate, run test_jam_upgrade_authority_readable against
// mainnet and assert the printed authority equals your Squads vault PDA.

#[account]
pub struct JamConfig {
    /// Admin public key.
    ///
    /// MUST be set to a Squads multisig vault PDA before mainnet deployment.
    /// See comments above for the Squads v4 setup procedure. Setting this to
    /// a hot wallet keypair leaves treasury/fee changes and QU!D bebop_authority
    /// updates exposed to a single point of compromise.
    pub admin: Pubkey,
    /// This program's authority PDA (seeds: [JAM_AUTHORITY_SEED]).
    /// Any program that wants JAM-exclusive access gates on jam_authority.is_signer
    /// — only invoke_signed from JAM can satisfy it. No registration needed here.
    /// Canonical bump for [JAM_AUTHORITY_SEED] PDA — used in invoke_signed.
    /// PDA address is derivable; storing it in config is redundant. Saves 32 bytes.
    pub authority_bump: u8,
    /// Minimum share_bps required to access QU!D flash liquidity.
    /// Mirrors Aux.sol comment: "orchestrator scores solvers by committed shareBps,
    /// routing more flow to generous solvers."
    /// Originally intended as Aux.sol's shareBps (minimum tip-share of flash
    /// loan principal). Cannot be enforced in JAM: JAM never sees the flash
    /// repay tip — that happens inside a solver interaction targeting QU!D.
    /// Enforcement lives in QU!D's ProgramConfig.min_tip_bps (clutch.rs).
    /// Kept here for EVM parity and off-chain orchestrator scoring only.
    pub min_share_bps: u16,
    /// Bebop treasury — receives protocol_fee_bps of buy output per settlement.
    /// Separate from admin (the Squads governance vault). Can be a cold wallet,
    /// a different multisig, or the same vault — governance and revenue are
    /// independent concerns.
    pub treasury: Pubkey,
    /// Bebop's per-settlement cut in bps (EVM: packed into partnerInfo bits 0-15;
    /// Solana: separate field for clarity).
    /// Defaults to 0 on Solana — primary revenue flows through QU!D's kickback,
    /// not per-swap JAM fees. Kept for EVM parity; activate via admin if needed.
    pub protocol_fee_bps: u16,
    pub bump: u8,
}

impl JamConfig {
    // discriminator(8) + admin(32) + treasury(32) + authority_bump(1) + bump(1)
    // + min_share_bps(2) + protocol_fee_bps(2) = 78 bytes
    // jam_authority Pubkey was removed (derivable from seeds; never read from config)
    pub const SPACE: usize = 8 + 32 + 32 + 1 + 1 + 2 + 2;
}

// ─── Events ───────────────────────────────────────────────────────────────────

/// Emitted whenever update_config is called, providing an on-chain audit trail.
/// Admin changes are handled off-chain via Squads transaction log; this event
/// covers fee / treasury parameter changes that monitoring systems should track.
#[event]
pub struct ConfigUpdated {
    pub admin: Pubkey,
    /// Human-readable field name that changed.
    pub field: String,
    pub timestamp: i64,
}

// ─── NonceRecord ──────────────────────────────────────────────────────────────
// Replay prevention. PDA init fails if already exists.
// Equivalent of JamValidation's usedNonces mapping.
// Seeds: [NONCE_SEED, taker, nonce.to_le_bytes()]

#[account]
pub struct NonceRecord {
    // taker and nonce are encoded in PDA seeds — no need to duplicate here.
    /// Order expiry timestamp (i64, matches Clock::unix_timestamp).
    /// close_nonce_record is gated on now > expiry — once past, the order
    /// is already invalid and the record is inert. The PDA is the replay
    /// guard; closing it early is impossible while the order is live.
    pub expiry: i64,
    pub bump:   u8,
}

impl NonceRecord {
    pub const SPACE: usize = 8 + 8 + 1; // discriminator + expiry + bump = 17 bytes
}


# Bebop PMM RFQ

Bebop is an aggregator of market-maker intents, allowing individual legs of basket trades to be split across multiple makers, in order to maximize the best overall price for the taker.

## Build

**step 1.**  
```cli
anchor build
```

**step 2.**
```cli
solana-keygen pubkey target/deploy/bebop_rfq-keypair.json
solana-keygen pubkey target/deploy/mock_swap-keypair.json
solana-keygen pubkey target/deploy/jam_settlement-keypair.json
```

**step 3.**  
copy the program ids   
into their respective  
`lib.rs` files, then  
`anchor build` again  
(in case you deploy  
with anchor, update  
`Anchor.toml` too)...  

## Test

```cli
cargo test-sbf -p bebop_rfq -- --nocapture
cargo test-sbf -p jam_settlement -- --nocapture
```
mock_swap — is a test fixture, not a production program.  
Its job is to act as a counterparty in the interaction tests.

## Flow

Bebop offers two execution options: regular and gasless

**Gasless**
1) User asks for a quote to swap
2) Bebop finds best route using multiple market makers and onchain pools.
3) Bebop constructs transaction and returns it to user.
4) User signs transaction and calls api /order endpoint using quote-id and signature as params.
5) Bebop sends signature request to all makers involved.
6) Makers respond with signatures and Bebop constructs final transaction.
7) Bebop executor sends signed transaction onchain.

**Regular**
1) User asks for a quote to swap
2) Bebop finds best route using multiple market makers and onchain pools.
3) Bebop sends signature request to all makers involved.
4) Makers respond with signatures to Bebop
5) Bebop responds with signed transaction to user
6) User could sign and submit this transaction onchain


## Swap function

```rust
pub fn swap<'c: 'info, 'info>(
    ctx: Context<'_, '_, 'c, 'info, Swap<'info>>,
    input_amount: u64,
    output_amounts: Vec<AmountWithExpiry>,
    event_id: u64,
) -> Result<()>
```

*input_amount* - maximum amount that could be executed (in case of partial fill output_amount scales proportionally) \
*output_amounts* - output amount that decreases overtime to prevent sitting on stale quotes. For example if taker submits tx onchain before X timestamp amount is Y; after X+1 - amount Y-10, etc \
*event_id* - for tracking order offchain

## Order Types

1) **Single PMM**  \
*swap: 100 USDC -> 1 WSOL* \
100 USDC from taker to maker \
1 WSOL from maker to taker


2) **Multiple PMMs** \
*swap: 100 USDC -> 1 WSOL* \
60 USDC from taker to maker#1 \
0.6 WSOL from maker#1 to taker \
40 USDC from taker to maker#2 \
0.4 WSOL from maker#2 to taker


3) **Multiple PMMs (2-hops)** \
*swap: 100 USDT -> 100 USDC -> 1 WSOL* \
100 USDT from taker to maker#1 \
100 USDC from maker#1 to Shared-account \
100 USDC from Shared-account to maker#2 \
1 WSOL from maker#2 to taker


4) **Pool + PMM (2-hops)** \
*swap: 10 BONK -> 100 USDC -> 1 WSOL* \
10 BONK from taker to pool \
100 USDC from pool to Shared-account \
100 USDC from Shared-account to maker \
1 WSOL from maker to taker


5) **PMM + Pool (2-hops)** \
*swap: 100 USDC -> 1 WSOL -> 10 PENGU* \
100 USDC from taker to maker \
1 WSOL from maker to taker \
1 WSOL from taker to pool \
10 PENGU from pool to taker

## Access control deployment guide for jam_settlement

─── Why Squads v4, not a custom timelock ────────────────────────────────────

A custom propose/accept admin pattern is the most common homebrew approach on
Solana, but it adds untested surface area and is strictly inferior to Squads v4:

  Squads v4   https://github.com/Squads-Protocol/v4
  Audited by: OtterSec, Neodyme, Trail of Bits
  Mainnet ID: SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf8
  npm SDK:    @sqds/multisig
  Rust SDK:   squads-multisig (crates.io — used for off-chain tooling)
  Docs:       https://docs.squads.so/squads-v4/

─── Two protected keys ──────────────────────────────────────────────────────

1. Program upgrade authority (jam_settlement + bebop_rfq)
   Ref: https://solana.com/docs/programs/deploying#upgrade-authority

   jam_settlement holds NO persistent user funds — there is nothing in JAM
   itself to drain. The multisig is needed for two distinct reasons:

   (a) Per-settlement redirect: a malicious upgrade can modify the buy-token
       transfer destination on future settlements. Harm is distributed across
       individual trades, not a single pool heist — but it is real.

   (b) JAM's use_jam_authority flag is the only thing preventing JAM from
       being an unbounded signer-for-hire. A malicious upgrade that removes
       this guard (or unconditionally sets it to true for all interactions)
       would let any solver construct a flash_borrow interaction that passes
       provider's flash_authority.is_signer check — because JAM would sign for
       every CPI regardless of the flag. Pools could then be drainable
       by anyone who can craft a valid JAM settle transaction.
       Multisig protects through JAM's authority delegation, not
       JAM's own state.

   Setup (run once, before mainnet):

     # Get your Squads vault address (index 0):
     #   seeds = [b"vault", multisig_pda, &[0, 0, 0, 0]]
     #   program = SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf8

     solana program set-upgrade-authority <JAM_PROGRAM_ID> \
         --new-upgrade-authority <SQUADS_VAULT_PDA>

     solana program set-upgrade-authority <RFQ_PROGRAM_ID> \
         --new-upgrade-authority <SQUADS_VAULT_PDA>

   When the program is stable and no further upgrades are planned:
     solana program set-upgrade-authority <PROGRAM_ID> --final

   Verification (run as CI deployment gate):
     The test test_jam_upgrade_authority_readable reads the BPFLoader
     programdata account and prints the current upgrade authority.
     Run it against mainnet and assert the output == your Squads vault PDA.

2. JamConfig.admin key
   admin gates update_config (treasury, protocol_fee_bps, min_share_bps).
   admin indirectly controls FlashLoanProvider flash loan access via bebop_authority.

   Setup:
     a. Create a Squads multisig with your signers and threshold.
     b. Derive vault PDA (TypeScript, using @sqds/multisig):

          import { getVaultPda } from "@sqds/multisig";
          const [vaultPda] = getVaultPda({ multisigPda, index: 0 });

     c. Call init_config with admin = vaultPda.
        Anchor's `has_one = admin` constraint is satisfied automatically
        when the Squads CPI execution signs the transaction — no changes
        to jam_settlement.rs are needed.

   How update_config looks through Squads:
     1. Create a Squads transaction with the update_config instruction.
     2. Gather M-of-N approvals from multisig members.
     3. Execute: Squads signs with vault PDA, which satisfies `admin: Signer`.
     Ref: https://docs.squads.so/squads-v4/development/sdk/execute-a-transaction

─── ConfigUpdated event ─────────────────────────────────────────────────────

update_config emits a ConfigUpdated event (see state.rs) for every field
change. Index these events on-chain (e.g. via Helius webhooks) to build an
audit trail of all parameter changes. Squads also provides a transaction log
showing who approved each change — together these give full accountability.

There is no propose_admin / accept_admin instruction in jam_settlement.
The timelock is provided by Squads (configurable per multisig, typically
requiring a cool-off period before execution). Rolling a custom timelock
adds untested code and duplicates a solved problem. Use Squads instead.

## Keeper: reclaiming `NonceRecord` rent

Every settled order creates a `NonceRecord` PDA at:

```
seeds = [NONCE_SEED, taker_pubkey, nonce.to_le_bytes()]
program = jam_settlement::ID
```

This PDA is the replay-prevention mechanism (Solana's equivalent of EVM's
`usedNonces[taker][nonce]`). It costs ~0.0024 SOL in rent-exempt deposit and
persists indefinitely — until a keeper closes it.

### When is it safe to close?

Once `Clock::unix_timestamp > record.expiry`. The settle handler enforces
`now < order.expiry` before the record is created, so after expiry the order
can never be settled again. The nonce record is inert and the rent is free to
reclaim.

> **Do not** use `settled_at + constant` as the condition. An order settled
> early within a long validity window would have its record closed while the
> order is still valid, reopening a replay window.

### How to call it

```typescript
const params = { taker: takerPubkey, nonce: new BN(nonceValue) };
await program.methods
  .closeNonceRecord(params)
  .accounts({ payer: wallet.publicKey, record: nonceRecordPda, systemProgram: SystemProgram.programId })
  .rpc();
```

The rent refund goes to `payer` — whoever calls the instruction keeps the
~0.0024 SOL. No admin permission required.

### Finding expired records

Index `BebopJamOrderFilled` events (emitted on every successful settle) which
carry `nonce` and `taker`. Store in a local DB with the corresponding
`order.expiry`. Poll for rows where `expiry < Date.now() / 1000`.

```typescript
// Derive the PDA for a known (taker, nonce) pair
const [nonceRecordPda] = PublicKey.findProgramAddressSync(
  [NONCE_SEED, takerPubkey.toBytes(), new BN(nonce).toArrayLike(Buffer, 'le', 8)],
  JAM_PROGRAM_ID,
);
```

Alternatively, `getProgramAccounts` with a `memcmp` filter on the
`NonceRecord` discriminator will return all open records; filter client-side
for `record.expiry < now`.

### Economics

| Metric | Value |
|--------|-------|
| Rent per record | ~0.0024 SOL (~17 bytes at current rates) |
| Break-even at 0 gas | 1 record per keeper tx |
| Suggested batch size | 20–30 records per tx (compute headroom) |
| Expected yield (1000 orders/day) | ~2.4 SOL/day for an active keeper |

A keeper bot watching the `BebopJamOrderFilled` event stream and calling
`close_nonce_record` as records expire is MEV-neutral, protocol-positive, and
self-funding.

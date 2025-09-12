# Bebop PMM RFQ

Bebop is market-maker aggregator, that allows individual legs of basket trades to be split across multiple makers, in order to maximize the best overall price for the taker.

## Build

```cli
cargo build-sbf
```

## Test

```cli
cargo test-sbf --package bebop_rfq --test test_swap
```

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

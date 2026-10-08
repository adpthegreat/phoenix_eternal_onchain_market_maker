# Overview 
This repo contains the code for a onchain market maker solana program for the phoenix eternal [perp program](EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih) based on the [original program for phoenix-v1](https://github.com/Ellipsis-Labs/phoenix-onchain-market-maker/) by ellipsis labs. It only has two instructions 

- `Initialize` creates a new market making strategy object with specific parameters 
- `UpdateQuotes` updates the market maker quotes (bids and asks) in accordance with the specific strategy parameters

# Additional Utils 

## Withdraw queue position
Phoenix has one exchange-wide withdraw queue (`withdrawQueue` from [`/v1/view/exchange/keys`](https://perp-api.phoenix.trade/v1/view/exchange/keys), or `GlobalConfig::withdraw_queue_key`) with no per-trader index. [`src/withdraw_queue.rs`](src/withdraw_queue.rs) walks it from the head and, for a trader account or wallet key, returns its position, the quote lots queued ahead of it, and a rough slot estimate from the withdraw throttle. Layout reference: [`withdraw_queue.rs`](https://github.com/Ellipsis-Labs/rise-public/blob/0aa44d1c763fccca2c81951ad995071362c76da0/rust/accounts/src/withdraw_queue.rs).

```bash
# prints the queue address, throttle and the wallet's position; defaults to the phoenix.trade demo account
PHOENIX_WITHDRAW_WALLET=<WALLET> cargo test --test withdraw_queue
```

# References 
- https://github.com/mubarizkyc/phoenix_onchain_mm/
- https://github.com/Ellipsis-Labs/phoenix-onchain-market-maker/
- https://github.com/Ellipsis-Labs/rise-public/tree/master/programs/ 


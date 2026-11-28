# Checkpoint chain work values (SB14b S2), read 2026-11-25

The values below were collected in a `chainwork.tsv` with one row per entry in dash-spv/checkpoints/{mainnet,testnet}.json at node/integration 1acd35620daa, giving height, block hash at that height, and absolute chain work (hex, as Core prints it).

`generate-checkpoints` does not write `chain_work`, so a regenerated JSON file fails the build until each new entry's value is added by hand and its source recorded here.

- testnet, all 31 heights: `dash-cli getblockhash H` then `getblockheader <hash>` `.chainwork`, on a synced Dash Core v24 testnet node.
- mainnet, the 41 heights up to 2,050,000: the same, on a Dash Core v24 mainnet node whose tip was 2,094,670.
- mainnet, the 10 heights from 2,100,000 to 2,550,000: the node had not reached them. They come from the public insight.dash.org block API (`/insight-api/block-index/H` → hash, `/insight-api/block/<hash>` → chainwork, height asserted).
  Cross-check: insight's chainwork equals dashd's at 50,000, 1,000,000, 2,000,000 and 2,050,000.
- Upper bound for the insight rows: Dash Core v24.0.2 sets mainnet `nMinimumChainWork` = 0x...c15981a81af44a6abe1a at height 2,563,000 (src/chainparams.cpp:231). The highest insight row, 2,550,000 = 0x...bfef61023f9f80ef413c, is below it, and its per-block work is in line with the dashd-sourced steps before it.
- All 82 rows' hashes match the checkpoint block hashes in dash-spv/checkpoints/{mainnet,testnet}.json at node/integration 1acd35620daa, and chain work strictly increases per network.

Each row's chainwork is that entry's `chain_work` field in `mainnet.json` or `testnet.json`, and its hash is the hash of the entry's last header; the tests check both against a copy of the rows. The tsv itself is not kept. Its source column, by entry:

| network | height | source |
|---|---|---|
| mainnet | 50000 | dashd v24 getblockheader |
| mainnet | 100000 | dashd v24 getblockheader |
| mainnet | 150000 | dashd v24 getblockheader |
| mainnet | 200000 | dashd v24 getblockheader |
| mainnet | 250000 | dashd v24 getblockheader |
| mainnet | 300000 | dashd v24 getblockheader |
| mainnet | 350000 | dashd v24 getblockheader |
| mainnet | 400000 | dashd v24 getblockheader |
| mainnet | 450000 | dashd v24 getblockheader |
| mainnet | 500000 | dashd v24 getblockheader |
| mainnet | 550000 | dashd v24 getblockheader |
| mainnet | 600000 | dashd v24 getblockheader |
| mainnet | 650000 | dashd v24 getblockheader |
| mainnet | 700000 | dashd v24 getblockheader |
| mainnet | 750000 | dashd v24 getblockheader |
| mainnet | 800000 | dashd v24 getblockheader |
| mainnet | 850000 | dashd v24 getblockheader |
| mainnet | 900000 | dashd v24 getblockheader |
| mainnet | 950000 | dashd v24 getblockheader |
| mainnet | 1000000 | dashd v24 getblockheader |
| mainnet | 1050000 | dashd v24 getblockheader |
| mainnet | 1100000 | dashd v24 getblockheader |
| mainnet | 1150000 | dashd v24 getblockheader |
| mainnet | 1200000 | dashd v24 getblockheader |
| mainnet | 1250000 | dashd v24 getblockheader |
| mainnet | 1300000 | dashd v24 getblockheader |
| mainnet | 1350000 | dashd v24 getblockheader |
| mainnet | 1400000 | dashd v24 getblockheader |
| mainnet | 1450000 | dashd v24 getblockheader |
| mainnet | 1500000 | dashd v24 getblockheader |
| mainnet | 1550000 | dashd v24 getblockheader |
| mainnet | 1600000 | dashd v24 getblockheader |
| mainnet | 1650000 | dashd v24 getblockheader |
| mainnet | 1700000 | dashd v24 getblockheader |
| mainnet | 1750000 | dashd v24 getblockheader |
| mainnet | 1800000 | dashd v24 getblockheader |
| mainnet | 1850000 | dashd v24 getblockheader |
| mainnet | 1900000 | dashd v24 getblockheader |
| mainnet | 1950000 | dashd v24 getblockheader |
| mainnet | 2000000 | dashd v24 getblockheader |
| mainnet | 2050000 | dashd v24 getblockheader |
| mainnet | 2100000 | insight.dash.org block API |
| mainnet | 2150000 | insight.dash.org block API |
| mainnet | 2200000 | insight.dash.org block API |
| mainnet | 2250000 | insight.dash.org block API |
| mainnet | 2300000 | insight.dash.org block API |
| mainnet | 2350000 | insight.dash.org block API |
| mainnet | 2400000 | insight.dash.org block API |
| mainnet | 2450000 | insight.dash.org block API |
| mainnet | 2500000 | insight.dash.org block API |
| mainnet | 2550000 | insight.dash.org block API |
| testnet | 50000 | dashd v24 getblockheader |
| testnet | 100000 | dashd v24 getblockheader |
| testnet | 150000 | dashd v24 getblockheader |
| testnet | 200000 | dashd v24 getblockheader |
| testnet | 250000 | dashd v24 getblockheader |
| testnet | 300000 | dashd v24 getblockheader |
| testnet | 350000 | dashd v24 getblockheader |
| testnet | 400000 | dashd v24 getblockheader |
| testnet | 450000 | dashd v24 getblockheader |
| testnet | 500000 | dashd v24 getblockheader |
| testnet | 550000 | dashd v24 getblockheader |
| testnet | 600000 | dashd v24 getblockheader |
| testnet | 650000 | dashd v24 getblockheader |
| testnet | 700000 | dashd v24 getblockheader |
| testnet | 750000 | dashd v24 getblockheader |
| testnet | 800000 | dashd v24 getblockheader |
| testnet | 850000 | dashd v24 getblockheader |
| testnet | 900000 | dashd v24 getblockheader |
| testnet | 950000 | dashd v24 getblockheader |
| testnet | 1000000 | dashd v24 getblockheader |
| testnet | 1050000 | dashd v24 getblockheader |
| testnet | 1100000 | dashd v24 getblockheader |
| testnet | 1150000 | dashd v24 getblockheader |
| testnet | 1200000 | dashd v24 getblockheader |
| testnet | 1250000 | dashd v24 getblockheader |
| testnet | 1300000 | dashd v24 getblockheader |
| testnet | 1350000 | dashd v24 getblockheader |
| testnet | 1400000 | dashd v24 getblockheader |
| testnet | 1450000 | dashd v24 getblockheader |
| testnet | 1500000 | dashd v24 getblockheader |
| testnet | 1550000 | dashd v24 getblockheader |

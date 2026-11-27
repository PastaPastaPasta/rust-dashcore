# RP2a observer goldens: `feature_llmq_simplepose`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_llmq_simplepose`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_llmq_simplepose/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `795d0a0d7ad123d41b7efb0b9a0166d556fbc4c4e5dca51f15c7e46a085ff6a5` |
| Calls | 3015 in 2 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `quorum_simplepose_matches_dashd` | `-llmqtestinstantsendparams=5:3 -llmqtestparams=5:3 -llmqtestplatformparams=5:3 -dip3params=2:2 -testactivationheight=v20@9999 -testactivationheight=mn_rr@9999 -txindex=0` | not set | 2045 | 0 |
| 1 | `quorum_simplepose_matches_dashd` | `-llmqtestinstantsendparams=5:3 -llmqtestparams=5:3 -llmqtestplatformparams=5:3 -dip3params=2:2 -testactivationheight=v20@9999 -testactivationheight=mn_rr@9999 -txindex=0 -deprecatedrpc=permissive_bool -deprecatedrpc=service` | not set | 970 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

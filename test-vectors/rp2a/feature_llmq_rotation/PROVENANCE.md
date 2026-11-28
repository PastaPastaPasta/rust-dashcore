# RP2a observer goldens: `feature_llmq_rotation`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_llmq_rotation`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_llmq_rotation/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `c6db263731557ae13e565dd8694d3bae0f75f6566920fc8f21501b47b04a53d8` |
| Calls | 3216 in 3 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_rotation_matches_dashd` | `-llmqtestinstantsendparams=4:4 -llmqtestparams=4:4 -llmqtestplatformparams=4:4 -vbparams=testdummy:999999999999:999999999999 -dip3params=2:2 -testactivationheight=v20@300 -testactivationheight=mn_rr@300 -txindex=0` | not set | 1365 | 0 |
| 1 | `quorum_rotation_matches_dashd` | `-llmqtestinstantsendparams=4:4 -llmqtestparams=4:4 -llmqtestplatformparams=4:4 -vbparams=testdummy:999999999999:999999999999 -dip3params=2:2 -testactivationheight=v20@300 -testactivationheight=mn_rr@300 -txindex=0` | not set | 1278 | 0 |
| 2 | `quorum_rotation_matches_dashd` | `-llmqtestinstantsendparams=4:4 -llmqtestparams=4:4 -llmqtestplatformparams=4:4 -vbparams=testdummy:999999999999:999999999999 -dip3params=2:2 -testactivationheight=v20@300 -testactivationheight=mn_rr@300 -txindex=0 -deprecatedrpc=permissive_bool -deprecatedrpc=service` | not set | 573 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

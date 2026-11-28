# RP2a observer goldens: `feature_new_quorum_type_activation`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_new_quorum_type_activation`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_new_quorum_type_activation/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `c81c49e909a47ce2f4cc7a0cbd2a33dc6c05b49f59ab3aa8a4fd6d1677d39550` |
| Calls | 141 in 2 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `quorum_type_activation_matches_dashd` | `-vbparams=testdummy:0:999999999999:0:10:8:6:5:0 -txindex=0` | not set | 111 | 0 |
| 1 | `quorum_type_activation_matches_dashd` | `-vbparams=testdummy:0:999999999999:0:10:8:6:5:0 -txindex=0 -deprecatedrpc=permissive_bool -deprecatedrpc=service` | not set | 30 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

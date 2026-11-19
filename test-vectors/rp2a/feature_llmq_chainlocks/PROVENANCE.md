# RP2a observer goldens: `feature_llmq_chainlocks`

Dash Core v24.0.0's replies to the evo DIFF tests' calls on `feature_llmq_chainlocks`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-19T06:34:01Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` (`contrib/setup-dashd.py`), sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_llmq_chainlocks/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `37ba41fd41a5c169094623353d0d4384b49ca47e061242eb4f7cf2d11c8a07e3` |
| Calls | 2273 in 3 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_chainlocks_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@200 -testactivationheight=mn_rr@200 -txindex=0` | not set | 1073 | 0 |
| 1 | `quorum_chainlocks_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@200 -testactivationheight=mn_rr@200 -txindex=0` | not set | 782 | 0 |
| 2 | `quorum_chainlocks_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@200 -testactivationheight=mn_rr@200 -txindex=0 -deprecatedrpc=permissive_bool -deprecatedrpc=service` | not set | 418 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

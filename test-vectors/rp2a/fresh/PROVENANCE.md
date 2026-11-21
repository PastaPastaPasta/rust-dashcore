# RP2a observer goldens: `fresh`

Dash Core v24.0.0's replies to the evo DIFF tests' calls on `fresh`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-19T06:34:01Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` (`contrib/setup-dashd.py`), sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |
| Chain | none: the pairs start on a fresh regtest chain |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `cb0ce409ebb3bc82bda9f8866d0a5ba1bde7af8225e04fab6c8e1ed8cf000d79` |
| Calls | 415 in 3 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `family_usage_matches_dashd_v24` | `-txindex=0` | not set | 17 | 0 |
| 1 | `quorum_parameter_forms_match_dashd` | `-txindex=0` | not set | 199 | 0 |
| 2 | `quorum_parameter_forms_match_dashd` | `-txindex=0 -deprecatedrpc=permissive_bool` | not set | 199 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

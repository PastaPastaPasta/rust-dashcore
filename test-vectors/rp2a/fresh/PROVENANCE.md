# RP2a observer goldens: `fresh`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `fresh`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | none: the pairs start on a fresh regtest chain |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `390ea4329bfa86543d6530b3619538d33034f9acd9e030d28c843b02f05a7c58` |
| Calls | 415 in 3 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `family_usage_matches_dashd_v24` | `-txindex=0` | not set | 17 | 0 |
| 1 | `quorum_parameter_forms_match_dashd` | `-txindex=0` | not set | 199 | 0 |
| 2 | `quorum_parameter_forms_match_dashd` | `-txindex=0 -deprecatedrpc=permissive_bool` | not set | 199 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

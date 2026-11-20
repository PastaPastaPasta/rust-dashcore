# RP2a observer goldens: `feature_dip3_deterministicmns`

Dash Core v24.0.0's replies to the evo DIFF tests' calls on `feature_dip3_deterministicmns`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-19T06:34:01Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` (`contrib/setup-dashd.py`), sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_dip3_deterministicmns/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `095bb3a0b18a24bf1a82a2d26a6075c46230e02e714c10d8c5fa9e3cfe594da8` |
| Calls | 3557 in 4 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_dip3_matches_dashd` | `-budgetparams=10:10:10 -dip3params=135:150 -txindex=0` | not set | 834 | 0 |
| 1 | `protx_diff_dip3_matches_dashd` | `-budgetparams=10:10:10 -dip3params=135:150 -txindex=0 -deprecatedrpc=permissive_bool` | not set | 323 | 0 |
| 2 | `protx_list_info_listdiff_dip3_match_dashd` | `-budgetparams=10:10:10 -dip3params=135:150 -txindex=0` | `[1767225600]` pinned after the replay | 1200 | 0 |
| 3 | `protx_list_info_listdiff_dip3_match_dashd` | `-budgetparams=10:10:10 -dip3params=135:150 -txindex=0 -deprecatedrpc=permissive_bool` | `[1767225600]` pinned after the replay | 1200 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

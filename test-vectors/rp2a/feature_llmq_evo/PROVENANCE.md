# RP2a observer goldens: `feature_llmq_evo`

Dash Core v24.0.0's replies to the evo DIFF tests' calls on `feature_llmq_evo`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-19T06:34:01Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` (`contrib/setup-dashd.py`), sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_llmq_evo/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `324937d1e3aa852fef770c79ca52c69f4dcb22082874e10e8c1404ca59880050` |
| Calls | 5473 in 4 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_evo_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0 -deprecatedrpc=service` | not set | 1122 | 0 |
| 1 | `protx_diff_evo_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0` | not set | 1113 | 0 |
| 2 | `protx_list_info_listdiff_evo_match_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0` | `[1767225600]` pinned after the replay | 1619 | 0 |
| 3 | `protx_list_info_listdiff_evo_match_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0 -deprecatedrpc=service` | `[1767225600]` pinned after the replay | 1619 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

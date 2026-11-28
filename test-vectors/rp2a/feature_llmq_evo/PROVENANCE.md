# RP2a observer goldens: `feature_llmq_evo`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_llmq_evo`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_llmq_evo/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `06f6739d53c64d7c5dacbc8d217eff061cd628d50b4b937f89af1ae6fb3fbb71` |
| Calls | 5473 in 4 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_evo_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0 -deprecatedrpc=service` | not set | 1122 | 0 |
| 1 | `protx_diff_evo_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0` | not set | 1113 | 0 |
| 2 | `protx_list_info_listdiff_evo_match_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0` | `[1767225600]` pinned after the replay | 1619 | 0 |
| 3 | `protx_list_info_listdiff_evo_match_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@320 -txindex=0 -deprecatedrpc=service` | `[1767225600]` pinned after the replay | 1619 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

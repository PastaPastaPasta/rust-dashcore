# RP2a observer goldens: `feature_masternode_shares`

Dash Core v24.0.0's replies to the evo DIFF tests' calls on `feature_masternode_shares`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-19T06:34:01Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd` (`contrib/setup-dashd.py`), sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_masternode_shares/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `b127f820e57e1a407ca408d1bfe7851899a987d660d8490397657e73ebbc1081` |
| Calls | 14055 in 2 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_shares_match_dashd` | `-vbparams=v24:0:999999999999:250:10:8:6:5:0 -vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | not set | 6090 | 0 |
| 1 | `protx_list_info_listdiff_shares_match_dashd` | `-vbparams=v24:0:999999999999:250:10:8:6:5:0 -vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | `[1767225600]` pinned after the replay | 7965 | 18 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

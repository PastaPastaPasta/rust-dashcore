# RP2a observer goldens: `feature_evo_shares_activation`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_evo_shares_activation`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_evo_shares_activation/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `5ca17e8ac6ebc5253b543cdb2f8cfa3d57b2df9c18fa9d6c2f7224bd606a7f03` |
| Calls | 2670 in 4 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_shares_match_dashd` | `-llmqtestinstantsendparams=1:1 -llmqtestparams=1:1 -llmqtestplatformparams=1:1 -vbparams=v24:0:999999999999:200:10:8:6:5:0 -vbparams=testdummy:0:999999999999:0:4:4:4:5:1 -vbparams=evo_shares:0:999999999999:0:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | not set | 732 | 0 |
| 1 | `protx_list_info_listdiff_shares_activation_match_dashd` | `-llmqtestinstantsendparams=1:1 -llmqtestparams=1:1 -llmqtestplatformparams=1:1 -vbparams=v24:0:999999999999:200:10:8:6:5:0 -vbparams=testdummy:0:999999999999:0:4:4:4:5:1 -vbparams=evo_shares:0:999999999999:0:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | `[1767225600]` pinned after the replay | 1100 | 0 |
| 2 | `quorum_single_member_matches_dashd` | `-llmqtestinstantsendparams=1:1 -llmqtestparams=1:1 -llmqtestplatformparams=1:1 -vbparams=v24:0:999999999999:200:10:8:6:5:0 -vbparams=testdummy:0:999999999999:0:4:4:4:5:1 -vbparams=evo_shares:0:999999999999:0:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | not set | 586 | 0 |
| 3 | `quorum_single_member_matches_dashd` | `-llmqtestinstantsendparams=1:1 -llmqtestparams=1:1 -llmqtestplatformparams=1:1 -vbparams=v24:0:999999999999:200:10:8:6:5:0 -vbparams=testdummy:0:999999999999:0:4:4:4:5:1 -vbparams=evo_shares:0:999999999999:0:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0 -deprecatedrpc=permissive_bool -deprecatedrpc=service` | not set | 252 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

# RP2a observer goldens: `feature_masternode_shares`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_masternode_shares`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_masternode_shares/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `ac87247527fb0ef9365a1264b04c2722e2ed0f81b3f0471baf81cc379bf057bf` |
| Calls | 14055 in 2 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_shares_match_dashd` | `-vbparams=v24:0:999999999999:250:10:8:6:5:0 -vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | not set | 6090 | 0 |
| 1 | `protx_list_info_listdiff_shares_match_dashd` | `-vbparams=v24:0:999999999999:250:10:8:6:5:0 -vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -txindex=0` | `[1767225600]` pinned after the replay | 7965 | 18 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

# RP2a observer goldens: `v19-window-r2`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `v19-window-r2`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `dashd-chainstate/tests/data/v19-window/r2/chain.txt`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `eeec09b270963dd9f2eaeec872c7dc99512c4101f9d69ff6e6f8adcbe5be2c39` |
| Calls | 370 in 1 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `quorum_pre_v19_key_after_v19_matches_dashd` | `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -testactivationheight=dip0024@1000000 -vbparams=testdummy:-2:0 -testactivationheight=v19@236 -txindex=0` | not set | 370 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

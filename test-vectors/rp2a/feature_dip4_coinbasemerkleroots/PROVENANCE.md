# RP2a observer goldens: `feature_dip4_coinbasemerkleroots`

Dash Core v24.0.3's replies to the evo DIFF tests' calls on `feature_dip4_coinbasemerkleroots`, recorded by `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give the format and the comparison kinds) while each test compared dashd with rdashd live.

| | |
|---|---|
| Recorded | 2026-11-27T11:03:17Z |
| Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |
| dashd | `~/.rust-dashcore-test/dashcore-24.0.3/bin/dashd` (`contrib/setup-dashd.py`), sha256 `640c066fd9719e26fb0db33ada211599f5a65f5a7f29b22d5580d69a4776febd` |
| Chain | `test-vectors/statedigest/regtest-v24/feature_dip4_coinbasemerkleroots/node0/blocks.dat.zst`, by `submitblock` |
| Node args | `-datadir=<tmp> -regtest -printtoconsole=0 -listen=0 -rpcport=<free> -rpcuser=u -rpcpassword=p`, dashd also `-disablewallet`, then the section's args |
| Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |
| `replies.jsonl.zst` sha256 | `81db60eb6e6066f63d212a7cb27b208146a5f8de94bbf17219f44aa3d640a490` |
| Calls | 1578 in 1 sections |

## Sections

| # | Test | Section args | Mock time | Calls | Internal bugs |
|---|---|---|---|---|---|
| 0 | `protx_diff_coinbase_roots_match_dashd` | `-testactivationheight=dip0008@432 -testactivationheight=dip0024@900 -vbparams=testdummy:999999999999:999999999999 -dip3params=2:2 -testactivationheight=v20@900 -testactivationheight=mn_rr@900 -txindex=0` | not set | 1578 | 0 |

The `protx list/info/listdiff` pairs pin the mock time to 1767225600 once the chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs compare no reply that reads the clock.

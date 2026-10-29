# Provenance of the `dashd-net` test data

## `chainparamsseeds.h`

Dash Core's fixed-seed tables, copied byte for byte. This repository computes nothing in the file.

| | |
|---|---|
| Source | `src/chainparamsseeds.h` of Dash Core at tag `v24.0.0-rc.3` (commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, DEC-82) |
| Producer | Core's `contrib/seeds/generate-seeds.py`, as the file's header says |
| Command | `git -C <dash checkout> show v24.0.0-rc.3:src/chainparamsseeds.h > dashd-net/tests/data/chainparamsseeds.h` |
| File sha256 | `b9199ffaabcf225dbefa68ba496168ed2e8bb9903746a8c388d0019775c1a124` |
| Licence | MIT (Dash Core) |

`tests/fixed_seeds.rs` reads `chainparams_seed_main` and `chainparams_seed_test`. It checks that
`dash-params` carries the same bytes, and that SB6's `convert_seeds` gives the addresses a
separate BIP155 decoder finds, in table order.

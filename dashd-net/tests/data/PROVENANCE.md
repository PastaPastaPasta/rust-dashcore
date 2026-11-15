# Provenance of the `dashd-net` test data

## `chainparamsseeds.h`

Dash Core's fixed-seed tables, copied byte for byte. This repository computes nothing in the file.

| | |
|---|---|
| Source | `src/chainparamsseeds.h` of Dash Core at tag `v24.0.0-rc.5` (commit `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88`, DEC-82) |
| Producer | Core's `contrib/seeds/generate-seeds.py`, as the file's header says |
| Command | `git -C <dash checkout> show v24.0.0-rc.5:src/chainparamsseeds.h > dashd-net/tests/data/chainparamsseeds.h` |
| File sha256 | `89ae5a5ba62fae7c83b67d3fe7be92d2b6b95a7d8eca8752dcdf492483284c76` |
| Licence | MIT (Dash Core) |

`tests/fixed_seeds.rs` reads `chainparams_seed_main` and `chainparams_seed_test`. It checks that
`dash-params` carries the same bytes, and that SB6's `convert_seeds` gives the addresses a
separate BIP155 decoder finds, in table order.

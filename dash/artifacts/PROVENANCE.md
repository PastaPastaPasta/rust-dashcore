# Provenance of `dash/artifacts/`

Both files are payloads of real Dash Core `mnlistdiff` replies, captured over P2P on 2026-10-08.
They replace earlier Rust re-encodings that were not Core-exact (finding F12). The full account,
peers and checks are in
[`dash/tests/data/test_DML_diffs/PROVENANCE.md`](../tests/data/test_DML_diffs/PROVENANCE.md).

| File | Request | sha256 |
|---|---|---|
| `mn_list_diff_0_2227096.bin` | mainnet, genesis → 2227096 (a duplicate of `tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin`) | `e3be4126a735aa8fdf0c48f292a8ce55d6d7a35d4c31bd55522d760e90f7fc7a` |
| `mn_list_diff_testnet_0_1296600.bin` | testnet, genesis → 1296600 | `27ea9691248f4e52a3f33b83f0c490b43eb315958a03f7f539b50f31309d1097` |

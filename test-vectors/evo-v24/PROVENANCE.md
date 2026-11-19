# Provenance of `test-vectors/evo-v24/`

| File | Records | Producer | Confirmed by |
|---|---|---|---|
| `derive-kat.json` | 12 basic-scheme BLS key pairs: seeds `00…00`, `42…42` and `0123456789abcdef` ×4, each at indexes 0, 1, 7 and 2^32−1 | `testkit-keys bls-derive --seed <seed> --index <index>` (dash-testkit, `dash_testkit::keys::derive_basic_bls_key`), one run per row, the row being its output line with `seed` added | Dash Core's `bls fromsecret <sk> false` returns `"scheme":"basic"` and `public` = `pk` for every row: dash-testkit's `dashd_bls_fromsecret_agrees_with_derived_keys` (`dash-testkit/tests/keys.rs`), run against dashd v24.0.0 (`938fba5028`) and v23.1.0 before the file was committed |

`sk` is the 32-byte secret key big-endian and `pk` the 48-byte serialized public key, the hex
Core's `bls generate` and `bls fromsecret` print. The derivation is
`ikm = SHA-256("dash-testkit/basic-bls/v1" ‖ seed ‖ index as 4 big-endian bytes)` followed by IETF
KeyGen; changing the domain tag changes every row. `derive_matches_the_kat` re-derives each row
byte for byte.

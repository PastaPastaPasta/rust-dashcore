# Provenance of key-wallet's foreign test data

## `core_bip39_quirks.json`

A verbatim copy of `testdata/bip39_core_quirks.json` from dashwallet-desktop
(`PastaPastaPasta/dashwallet-desktop`) at commit `71c814a78dc9191bf57468a9d359ecdf96d7935a`
(`test(testdata): add golden vectors generated from Dash Core, dash-qt and dashd`), copied with
`git show 71c814a78dc9:testdata/bip39_core_quirks.json`. Nothing in it is hand-written:

| Section | Produced by (paths in dashwallet-desktop) | What runs |
|---|---|---|
| `check` (513), `seed` (12), `trezor` (24) | `testdata/oracle/bip39/make_vectors.py --core-src <dash>/src` | Dash Core's `src/wallet/bip39.cpp` (`CMnemonic`, from develop) compiled into `testdata/oracle/bip39/bip39_oracle.cpp`; it also writes `testdata/oracle/bip39/weak_sample.txt`, the weak-checksum phrases the `dashd` section feeds to `upgradetohd` |
| `dashd` (12) | `testdata/oracle/dashd/gen_vectors.py`, copied from `testdata/oracle/dashd/dashd_bip39.json` | a regtest dashd (`/Dash Core:24.0.0/`): `upgradetohd` on blank legacy and descriptor wallets, then `dumphdinfo` (legacy seed) and `listdescriptors true` (descriptor master key) |

`core_check` is `CMnemonic::Check`, `strict_check` BIP39 with the full checksum, `core_seed`
`CMnemonic::ToSeed` and `bip39_seed` the BIP39 seed (NFKD, untruncated salt); see the file's
`notes`. To regenerate, follow `testdata/oracle/README.md` in dashwallet-desktop, then copy the
new file here and update the pin below and in `tests/core_mnemonic_tests.rs`.

sha256: `85685865c0cd61ed44c6b3447f0c02a0d069c73d205d74109f8b7980cc5b2953` (183,745 bytes)

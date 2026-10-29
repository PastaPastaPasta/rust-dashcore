# Test data provenance

Nothing here is hand-written. Dash Core pins: v24.0.0-rc.3 = `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`,
v23.1.8 = `728f5055836c6d29806412fc7223ac8fe05af991`. Their `src/hash_x11.h` and `src/crypto/x11`
are identical.

| File | sha256 | Source |
|---|---|---|
| `blockfilters.json` | `59b744f4…cf54` | Core's `src/test/data/blockfilters.json` at v24.0.0-rc.3, byte for byte (identical at v23.1.8) |
| `known-hashes.json` | `8b1c1740…07f3` | `contrib/extract-vectors.py ~/workspace/dash v24.0.0-rc.3` |
| `core-fixture.json` | `10329972…92db` | `contrib/core-fixture.sh <harness>`, Core's own X11 code |

## `blockfilters.json`

Core's BIP158 test vectors: Bitcoin testnet blocks whose hashes Dash Core recomputed with X11.
`tests/vectors.rs` hashes the first 80 bytes of each block (the header) and compares with the
"Block Hash" column.

## `known-hashes.json`

Written on 2026-10-08 by `contrib/extract-vectors.py`, which reads `src/chainparams.cpp` at
v24.0.0-rc.3 and asks the reference oracles (dashd v24.0.0-rc.3, `contrib/oracle-cli`, read-only
RPCs `getblockheader`) for the headers:

- `genesis`: the three `assert(consensus.hashGenesisBlock == uint256S(...))` hashes (mainnet,
  testnet, regtest). The mainnet and testnet headers are the oracles' `getblockheader <hash>
  false`; regtest's genesis has mainnet's coinbase and merkle root, so its header is mainnet's
  with the nTime, nNonce and nBits of regtest's `CreateGenesisBlock` call.
- `checkpoints`: all 58 `checkpointData` entries of mainnet and testnet, each header from the
  oracle, whose height the script checks against the checkpoint's.

All 61 headers also hash to the recorded hashes under Core's harness (`contrib/core-harness`,
both pins).

## `core-fixture.json`

`contrib/core-fixture.sh` with the harness built from v24.0.0-rc.3 (bench_dash's objects, B1's
build). The harnesses built from v23.1.8 (B1's objects) and from source at both pins give the
same file, and so does the v24.0.0-rc.3 harness with `DASH_X11_CORE_SOFT=1` (Core's software
ECHO and SHAvite-3 instead of its AES-NI code). For seed 0x58313131 and 2,000 cases per stream:
SHA-256 of Core's 32-byte X11 digests of the `random` inputs (length `i % 201`, about ten per
length 0..=200), the `long` inputs (length `201 + i % 900`) and the `header` inputs (80 bytes),
and of Core's 64-byte digests for each chain function (BLAKE over the `random` inputs, the others
over 64-byte inputs). The inputs come from SplitMix64, which `harness.cpp` and
`tests/common/mod.rs` both implement.

## The self-test's digests (`src/self_test/vectors.rs`)

Not test data, but from the same source: `contrib/self-test-vectors.py` with the harnesses of
v24.0.0-rc.3 (B1's objects), v23.1.8 (B1's objects) and v24.0.0-rc.3 (from source), which give
the same digests (the script refuses to write if any harness disagrees). The module's first
digest is also checked against Core's mainnet `hashGenesisBlock`.

# Core v24.0.0 platform-ban vectors

Core parity: `src/net_processing.cpp:3898,3906-3913`,
`src/llmq/signhash.cpp:14-23`, `src/bls/bls.cpp:125-141,310-325`.

Source tree: `/work/scratch/sw-v24-mn/core-v24`, tag `v24.0.0`, commit
`938fba502824b9d850b7c710601456dc3d71603e`. Output:
`dashd-masternode/tests/fixtures/platformban/core-v24.json`.

Exact command from the repository root:

```sh
export CARGO_TARGET_DIR=/work/scratch/sw-v24-mn/a5a/target TMPDIR=/work/scratch/sw-v24-mn/a5a/tmp
nice -n 10 python3 contrib/node/am1-platformban-captures/record.py /work/scratch/sw-v24-mn/core-v24 > dashd-masternode/tests/fixtures/platformban/core-v24.json
```

Requires a C++20 compiler, GNU make, autoconf, automake, libtool, and GMP development headers.
The recorder checks the Core commit and tracked tree, copies `src/dashbls` into a temporary
directory, and builds it with its own autotools configuration (`--disable-tests --disable-bench
--with-backend=gmp`). It extracts the request-id, message-hash, and sign-hash expressions and
verification call from `net_processing.cpp:3906-3913`, then compiles them with the original
`llmq/signhash.cpp`, `bls/bls.cpp`, and `crypto/sha256.cpp`. Core's source tree stays read-only.
`DISABLE_OPTIMIZED_SHA256` selects Core's portable SHA-256; `BUILD_BITCOIN_INTERNAL` omits
unused random-key generation (`bls/bls.cpp:65-84`). Unused sections are discarded at link time.

The 26 rows cover both process schemes (`bls/bls.cpp:323-325`): a valid signature; changed
proTxHash, height, quorum hash, public key, signature bytes, and LLMQ type; valid signatures for
platform types 4 and 6 as well as 106; and signed heights 0, -1, INT32_MIN, and INT32_MAX
(`net_processing.cpp:3898,3906-3913`, `llmq/signhash.cpp:14-23`). Fixed test scalars 1 and 2
supply the public keys (`bls/bls.h:275-281`, `bls/bls.cpp:112-141`). Hash hex uses stored byte
order. Signature hex retains the corrupted input bytes even when Core's `SetBytes` resets the
object (`bls/bls.h:100-125`); the verdict comes from `VerifyInsecure` (`bls/bls.cpp:310-325`).

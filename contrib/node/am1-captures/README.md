# AM1 operator-key vectors

`keyparse.py` starts a fresh Dash Core v24.0.0 regtest process for each input,
using the existing func-compat launcher's exported test tree and release binaries.
It writes `dashd-masternode/tests/fixtures/am1-key/core-v24.json`: 54 parse cases
and the public keys and constructor logs for all four `network.json` operator keys.
All keys in these fixtures are public test keys.

Recorded provenance:

- Core tag `v24.0.0`, commit `938fba502824b9d850b7c710601456dc3d71603e`.
- `dashd` SHA256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
- Regtest network v0.0.4 `network.json` SHA256:
  `1df3ee4100b42803b887ef6c96fd34df29d7982a376aea5798a31678b5b71bfb`.

From the repository root, with the Core checkout outside the writable capture tree:

```bash
export CARGO_TARGET_DIR=/work/scratch/sw-v24-mn/a1/target
export TMPDIR=/work/scratch/sw-v24-mn/a1/tmp
mkdir -p "$TMPDIR"
eval "$(DASHVERSION=24.0.0 CACHE_DIR="$PWD/target/am1-cache" python3 contrib/setup-dashd.py)"
python3 contrib/node/am1-captures/keyparse.py \
  --core /work/scratch/sw-v24-mn/core-v24 \
  --work "$PWD/target/am1-capture" \
  --cache "$PWD/target/am1-cache" \
  --network "$PWD/target/am1-cache/regtest-blockchain-v0.0.4/regtest-mn/network.json"
```

The recorder creates mode-0700 datadirs under `--work`, keeps the raw process
output there as `capture.txt`, and normalizes the key-related error/exception or
constructor log into the fixture. It also preserves each process's exit code.
`-stopafterblockimport=1` can stop a successful key parse before startup finishes,
so a key's recorded outcome follows the constructor log rather than the exit code.
An inactive input must reach the block-import stop without a constructor or error.
Exit codes for successful parses can therefore differ between reproductions.

## Signature vectors

After `keyparse.py`, append the signature vectors with:

```bash
python3 contrib/node/am1-captures/signatures.py \
  --core /work/scratch/sw-v24-mn/core-v24 \
  --work "$PWD/target/am1-signatures"
```

The recorder exports the pinned Core commit above into `--work/core`, builds its
unmodified dashbls library, and links `signatures.cpp` with Core's unmodified
`src/bls/bls.cpp`. It calls `CBLSSecretKey::Sign(hash, true/false)`, the function
`CActiveMasternodeManager::Sign` delegates to (`src/active/masternode.cpp:290-294`),
and serializes each result in its signing scheme. The separate `sign_basic`
capture follows `CActiveMasternodeManager::SignBasic`: sign with `false`, check
validity, then `ToByteVector(false)` (`src/active/masternode.cpp:297-303`). All three
variants are available through this helper.

The fixture's `signatures` section contains four rows: network keys 0 and 1, each
signing raw message-hash bytes `000102...1e1f` and 32 bytes of `42`. These are the
bytes at `uint256::begin()`, without a display-hex reversal. Each row records the
96-byte legacy, basic and `sign_basic` outputs. The Rust test compares all three
outputs directly to those recorded bytes, independently of SDK verification.

Capture provenance:

- Core commit: `938fba502824b9d850b7c710601456dc3d71603e`.
- Compiler: `g++ (Ubuntu 15.2.0-16ubuntu1) 15.2.0`, C++20, `-O2`.
- Helper source SHA256:
  `0e10634cbb70af64e572feed143e8f261e80f7c337437e5c675029f9b47ba357`.
- Helper binary SHA256:
  `db677306ffbc3fcdeba751e56113e9b3fffb4cc4984b259ad6f0c83ab1d59c4a`.
- Built dashbls archive SHA256:
  `8b76f2fb95b7a1b6fb2bdcd9d7948f881efdbe60817d7746e7e5f993bd269ba8`.

The recorder stores these provenance fields in the fixture too. Build-artifact
hashes can vary with the compiler and build path; the signature bytes are fixed.

## A1 plan amendment: Core's recorded rule

The recording determines behavior when the AM1 plan's wording differs. The key
must contain exactly 32 bytes, be nonzero, and be at most
`73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001` (`r`).
Core's comparison is `> r` in `src/dashbls/src/privatekey.cpp:66-70`.
Exactly `r` is accepted and its public key is infinity in both schemes; the
parse-matrix test requires both recorded exactly-`r` cases and compares their
public keys. Values above `r` and the all-zero key are rejected.

The departures and clarifications relative to the plan text are:

1. The plan's rejection of scalars greater than or equal to `r` is replaced by
   Core's rejection of scalars greater than `r`.
2. "Whitespace skipped" means ASCII whitespace between complete byte pairs,
   including before and after the input. A split nibble and non-ASCII whitespace
   are rejected (`src/util/strencodings.cpp:81-97`). Odd hex and invalid suffixes
   are rejected in their entirety. The zero-key and odd-hex outcomes otherwise
   agree with the plan.
3. The interface amendment's phrase "non-string settings value = Err" follows
   Core's actual `GetArg` conversion: null is inactive, booleans and numbers are
   converted to text, and arrays/objects produce the recorded `get_str` exception.
   A 64-digit JSON number can therefore encode an accepted key. This also follows
   A1's more detailed conversion rule.
4. A1(e)'s existing `cfg(test)` lockedpool observation is also available through
   the test-only `test-support` feature so a dependent crate's unit test can
   observe the cleansed chunk. Release builds reject that feature, following the
   existing `core-layout` test-feature convention.

The implementation uses zeroizing typed SDK scalars for keys below `r` and
constructs the public infinity results directly for exactly `r`. The SDK's
Core-shaped scalar does not implement `Zeroize`; no such scalar holding an
operator secret is introduced by this module.

A1(d), the rdashd process-level log/RPC check, remains deferred to W1 as assigned.
The existing `dash-args` settings logger prints settings.json values directly
(`dash-args/src/args.rs:839-856`), as Core does. A1 tests command-line/config
masking and the key loader's own logs; the settings logger needs attention in
W1's process hygiene work.

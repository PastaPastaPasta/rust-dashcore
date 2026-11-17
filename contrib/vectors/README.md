# Proof-of-work test vectors

`pow_vectors.cpp` links against Dash Core's real `src/arith_uint256.cpp`,
`src/uint256.cpp` and `src/util/strencodings.cpp` and prints
`dash/tests/data/pow_vectors.json`, which the `pow::tests::core_vectors` test in
`dash/src/pow.rs` checks against `Target`, `Work` and `CompactTarget`.

The file holds two tables:

- `compact`: for each `nBits`, the raw `SetCompact` value, the negative and
  overflow flags, `GetBlockProof` (zero when negative, overflowing or zero), and
  `GetCompact` of the raw value.
- `target`: for each 256-bit target, `GetCompact(false)` and `GetBlockProof`.

The `nBits` cover all 256 exponents with edge mantissas, the sign bit and the
8 and 16 bit boundaries, plus seeded random values. The targets cover every
power of two with its neighbours, plus seeded random values. The random source
is `std::mt19937_64`, which is fully specified, so the output does not depend on
the platform.

## Regenerating

Point `DASH_CORE` at a Dash Core checkout and run from the repository root:

```sh
DASH_CORE=~/workspace/dash
g++ -std=c++20 -O2 -I "$DASH_CORE/src" contrib/vectors/pow_vectors.cpp \
    "$DASH_CORE/src/arith_uint256.cpp" "$DASH_CORE/src/uint256.cpp" \
    "$DASH_CORE/src/util/strencodings.cpp" -o /tmp/pow_vectors
/tmp/pow_vectors > dash/tests/data/pow_vectors.json
```

The checked-in file was generated from Dash Core `develop` at
`15db47875002cc09d8a21e1e35fed4a74aaf4e4f`. These routines have not changed in
behaviour in years, so regenerating from a newer checkout should produce the
same file.

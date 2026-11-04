# Dash Hashes Library

This library re-exports the hash functions from
[`bitcoin_hashes`](https://docs.rs/bitcoin_hashes/) and adds the Dash-specific
X11 hash, along with `bincode` and `serde` support for hash newtypes.

[Documentation](https://docs.rs/dashcore_hashes/)

## X11

The `x11` feature adds `hash_x11`, computed by [`dash-x11`](https://docs.rs/dash-x11), a Rust port of Dash
Core's X11 that picks AES-NI, SSE and AVX2 backends at run time. The `x11-c` feature computes
80-byte headers with the C `rs-x11-hash` library instead (it cannot hash any other length, so
`dash-x11` still does); as with any feature, one crate asking for it switches the whole build.
`hash_x11::backend()` says which is in use.

`benches/x11.rs` prints B1's `Pow_X11_0080b` and `Pow_X11_1M` rows for the backend a build
selects, in the JSON `node-bench/run run microbench --impl rust --rust-bench-bin` reads.
`tests/x11_chain.rs` hashes a chain dumped from a reference oracle
(`contrib/dump-oracle-headers.py`) with every backend in the build.

## Minimum Supported Rust Version (MSRV)

This library should always compile with any combination of features on **Rust 1.89.0**.

## Contributions

Contributions are welcome, including additional hash function implementations.

### Githooks

To assist devs in catching errors _before_ running CI we provide some githooks. If you do not
already have locally configured githooks you can use the ones in this repository by running, in the
root directory of the repository:
```
git config --local core.hooksPath githooks/
```

Alternatively add symlinks in your `.git/hooks` directory to any of the githooks we provide.

### Running Benchmarks

We use a custom Rust compiler configuration conditional to guard the bench mark code. To run the
bench marks use: `RUSTFLAGS='--cfg=bench' cargo +nightly bench`.

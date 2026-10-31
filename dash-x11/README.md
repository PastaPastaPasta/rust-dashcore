# dash-x11

Dash's X11 proof-of-work hash in Rust, bit-exact with Dash Core's `HashX11` (`src/hash_x11.h`
and `src/crypto/x11`, identical in v23.1.8 and v24.0.0-rc.3), and cheaper than Core's on x86_64.
Node-program task XH1 (`docs/node-program/DECISIONS.md` DEC-51b), modelled on SH1's
`dash-sha256`. `no_std`, no I/O, no allocation; the only state is the cached detection.

X11 chains eleven 512-bit functions and keeps the first 32 bytes of the last digest. Inside X11
the first one (BLAKE-512) hashes the input and every other one hashes exactly one 64-byte digest,
so every function after BLAKE is specialised to that call: init, one 64-byte write, close.

| Stage | portable (Core's sph code, ported) | x86_64 backends |
|---|---|---|
| BLAKE-512, BMW-512, Skein-512, Keccak-512 | yes | |
| Grøstl-512 | yes | `aes`: AES-NI |
| JH-512 | yes | `sse`, `avx2` |
| Luffa-512 | yes | `sse`, `avx2` |
| CubeHash-512 | yes | `sse` (SSE2), `avx2` |
| SHAvite-3-512 | yes (Core's `soft_shavite`) | `aes`: AES-NI (Core: `x86_aesni_shavite`) |
| SIMD-512 | yes | `sse`, `avx2` |
| ECHO-512 | yes (Core's `soft_echo`) | `aes`: AES-NI (Core: `x86_aesni_echo`, `ssse3_echo`) |

`dash_x11::init()` picks backends at run time from CPUID, as Core's `SapphireAutoDetect` does,
checks them with a self-test and caches the choice; `dash_x11::x11` uses it. `sse` needs SSSE3
and SSE4.1, `aes` also AES-NI, `avx2` also AVX2 with the OS saving YMM state (allowing `aes` or
`avx2` allows `sse`). Other targets run the portable code.

- **Self-test** (`src/self_test`; Core has none): X11 of the mainnet genesis header, the empty
  input, 120 bytes of 0xff and a 300-byte ramp (BLAKE's 80-byte, two-final-block and multi-block
  paths), and every chain function of 64 bytes of 0x00 and of 0xff, against Core's digests
  (`contrib/self-test-vectors.py`). Cost: about 0.24M instructions with every backend, 0.91M
  with the portable code (`contrib/ins-per-call.sh <set> 2000 self-test`), well under a
  millisecond, per selection tried.
- **Fallback (DEC-61):** if the CPU's best selection fails the self-test, `init()` tries the
  next smaller one (`sse,aes`, `sse,avx2`, `sse`), down to the portable code, and panics only if
  that fails too. It returns a `Selection`: `implementation()`, `preferred()` (what CPUID
  alone picks), `fell_back()` and `failed()` (the candidates that failed). `selection()` reads
  the cached one. `Implementation::detect(Use::ALL)` is the strict variant: it panics instead.

Where this differs from Core (never the digest, only which code runs and how fast): Core
accelerates only ECHO and SHAvite-3 (AES-NI, SSSE3; ARMv8 AES and NEON on ARM); this crate also
vectorises Grøstl, JH, Luffa, CubeHash and SIMD on x86_64, and has no ARM backends yet (ARM runs
the portable code).

## API

```rust
let header = [0u8; 80];
let hash: [u8; 32] = dash_x11::x11(&header);   // Core's HashX11, in uint256 byte order

use dash_x11::{Implementation, Stage, Use};
let portable = Implementation::standard();
assert_eq!(portable.x11(&header), hash);
let blake = portable.stage(Stage::Blake, &header); // one function of the chain
```

## Adoption

**Node contract (DEC-61).** A node using this crate must:
1. call `dash_x11::init()` once at startup, on the init thread, before any hashing (Core runs
   `SapphireAutoDetect` in its kernel context's constructor), so a self-test failure is seen
   there and not on whichever thread hashes first;
2. log the selection like Core's SHA-256 line: `Using the 'sse,aes,avx2' X11 implementation`
   (`Selection`'s `Display` gives the quoted name, plus the failed candidates after a fallback);
3. warn prominently when `fell_back()`, naming `failed()` and the implementation in use, and
   expose the name where operators and benchmarks look (debug/metrics, node-bench provenance): a
   fallback costs up to about 4× in X11;
4. offer an operator option that makes a fallback fatal (check `fell_back()`, or call
   `Implementation::detect(Use::ALL)`, which panics).

Consumers:
- **CS0** (`dash-consensus`, its hash module; `docs/node-program/designs/CS0-node-primitives.md`
  §3): the single X11 entry point calls `dash_pow::hash` today; switching it is one line,
  `dash_x11::x11(data)`, plus `dash-x11 = { path = "../dash-x11" }`. The two agree on every
  mainnet header (XH1 r1 review). CS0/CH1 own the node side of the contract above.
- **BS15** (done): `dashcore_hashes::hash_x11`, and so `dashcore`'s `BlockHash`, call this crate
  instead of `rs-x11-hash` (C, 80-byte inputs only) or `dash-pow` (PLAN, DEC-61). The
  `dashcore_hashes` feature `x11-c` switches back to the C library for headers. `dashcore` does
  not call `init()` itself, so the first hash runs the detection and its self-test; a node or
  client that follows the contract above calls `dash_x11::init()` at startup.
- **Debug builds:** unoptimised, one X11 hash takes about 0.9 ms (5 µs optimised), so a workspace
  whose debug tests hash many headers should optimise this crate, as this one does:

  ```toml
  [profile.dev.package.dash-x11]
  opt-level = 3
  ```

The crate's `LICENSE` carries Core's and sphlib's MIT notices for every file; the sources have
no per-file copyright headers.

## Publishing

The crate has no dependencies, and every dev-dependency is a registry crate, so the packaged
tests build (`tests/core.rs` hashes its digest streams with `bitcoin_hashes`; a path-only
dev-dependency such as the unpublished `dash-sha256` would be dropped from the published
manifest and break that test). Check a release with `cargo package -p dash-x11`, then
`cargo test --no-run --manifest-path` on the unpacked `.crate`, and `cargo publish -p dash-x11
--dry-run`.

## Verification

- `tests/vectors.rs`: the genesis hashes Core asserts (mainnet, testnet, regtest), every mainnet
  and testnet checkpoint with the header Core's reference nodes hold, and Core's
  `blockfilters.json` block hashes. Provenance: `tests/data/PROVENANCE.md`.
- `tests/core.rs`: `core_fixture` (always runs) compares with Core's digest streams for 2,000
  inputs of lengths 0..=200, 2,000 of 201..=1100 bytes, 2,000 headers and 2,000 inputs per
  stage. With `DASH_X11_CORE_HARNESS`, `core_live_*` stream inputs to Core's own code
  (`contrib/core-harness`) and compare every case; `env DASH_X11_CORE_SOFT=1 <harness>` makes
  Core use its software ECHO and SHAvite-3. `contrib/differential.sh` runs them over many seeds
  in parallel.
- `tests/props.rs`: proptest, every implementation against the portable one, X11 as the chain
  of its stages.
- `src/x86_64/mod.rs` unit test: every backend function against the portable one.
- `examples/rehash.rs`: every header of the mainnet and testnet chains (dumped from Core's nodes)
  must hash to its successor's prev-hash and the last to the best header; every block record of
  Core's `blk*.dat` files must hash as Core's harness hashes it.

Every test runs every implementation the CPU has. Run the suite under the profile and flags the
shipping binary uses as well (at least per release): XH1 ran it under the workspace `release`
profile, under `release` with fat LTO and one codegen unit, and with `-C target-cpu=x86-64-v3`.

```
H=/path/to/harnesses   # example; agentbox used /work/scratch/xh1/bin
contrib/core-harness/build.sh ~/workspace/dash v23.1.8 $H/core-v23.1.8
contrib/core-harness/build.sh ~/workspace/dash v24.0.0-rc.3 $H/core-v24.0.0-rc.3
# or link the objects bench_dash itself was built from:
contrib/core-harness/build.sh ~/workspace/dash v24.0.0-rc.3 $H/core-b1 /work/bench/src/dash-v24.0.0-rc.3

DASH_X11_CORE_HARNESS="$H/core-v23.1.8;$H/core-v24.0.0-rc.3" cargo test -p dash-x11 --release
dash-x11/contrib/differential.sh 10 1000000 $H/core-v23.1.8 $H/core-v24.0.0-rc.3
cargo run --release -p dash-x11 --example rehash -- chain mainnet.bin <best-header-hash>
cargo run --release -p dash-x11 --example rehash -- blk <datadir>/blocks bf0c6bbd $H/core-v24.0.0-rc.3
```

## Benchmarks

`benches/x11.rs` (criterion): `x11/0080b` etc. with the shapes of Core's `bench_dash`
`Pow_X11_*` benches, the same 80-byte workload pinned per selection (`x11/0080b/standard`,
`/sse`, `/sse-aes`, `/sse-avx2`, `/all`; a selection the CPU lacks is not registered), and
`stage/<name>`. `contrib/ins-per-call.sh [implementation]` reports
user-space instructions per call (`examples/workload.rs` under `perf stat`); the Core harness's
`bench` command runs the same workloads on Core's code.

User-space instructions per call on the node program's benchmark machine (Xeon E5-2687W v4:
SSSE3, SSE4.1, AES-NI, AVX2), 2026-10-08, rustc 1.98.1 `release`. Core's X11 row is B1's
`bench_dash` (`Pow_X11_0080b`, 2,120.39 instructions per byte, the same at v23.1.8 and
v24.0.0-rc.3; Debian bookworm GCC 12.2, Core's `depends` and default flags); its per-stage rows
are the harness linked against that build's own objects (`harness bench <stage>`), whose X11 row
equals `bench_dash`'s. Stages run as inside X11: BLAKE on 80 bytes, the rest on 64.

| Stage | Core | portable | `sse` | `sse,aes` | `sse,avx2` | `sse,aes,avx2` |
|---|---|---|---|---|---|---|
| **X11, 80-byte header** | **169,617** | **149,984** | **95,833** | **55,063** | **79,840** | **39,055** |
| X11 per byte | 2,120.2 | 1,874.8 | 1,197.9 | 688.3 | 998.0 | 488.2 |
| BLAKE-512 | 3,182 | 2,716 | | | | |
| BMW-512 | 3,701 | 3,162 | | | | |
| Grøstl-512 | 24,716 | 15,028 | | 6,493 | | 6,493 |
| Skein-512 | 2,573 | 2,304 | | | | |
| JH-512 | 21,218 | 16,825 | 6,745 | 6,745 | 4,267 | 4,267 |
| Keccak-512 | 6,664 | 5,206 | | | | |
| Luffa-512 | 18,899 | 16,231 | 7,793 | 7,793 | 4,316 | 4,316 |
| CubeHash-512 | 48,000 | 34,444 | 13,376 | 13,376 | 5,882 | 5,882 |
| SHAvite-3-512 | 3,341 (AES-NI) | 14,542 | | 825 | | 825 |
| SIMD-512 | 30,924 | 18,717 | 4,159 | 4,159 | 1,608 | 1,608 |
| ECHO-512 | 6,384 (AES-NI) | 21,031 | | 2,498 | | 2,498 |

Empty cells run the portable code. With every backend, X11 costs 0.23× Core's instructions
(0.88× portable-only). For comparison, on the same workload: base-sdk `dash-pow` at 33fcc59
402,583 (5,032 per byte, 2.37× Core, with or without its `aes_hw` feature) and `rs-x11-hash` 0.1.9
(sph C, no AES-NI) 186,214 (2,328 per byte, 1.10× Core).

Core's side depends on how Core is built: Core's X11 compiled from source with GCC 15.2 takes
160,808 instructions, against which the portable code is 0.93× and every backend 0.24×. The
ratios above use B1's build, the PRD's reference.

### PF0 workloads

PF0 (`contrib/node/perfgate/workloads.toml`, branch `node/pf0-counter-gate`) is not merged yet;
these are the lines to add. They pin the selection, so a host without AES-NI or AVX2 fails loudly (a missing
ID), and a selection failing its self-test panics while the benches register, instead of
changing the counter. PF0 cannot run
Core's code, so the Core denominator is fixed: B1's `Pow_X11_0080b`, 169,617 instructions per
hash at both pins. PF0 counts with its own fixed iteration loop, while `contrib/ins-per-call.sh` computes
(instructions(N) - instructions(0)) / N over `examples/workload.rs`, so their per-hash figures
can differ by loop overhead; the 0.23× above is the latter's. P2a's §5.4 name `x11_80b` is `x11-80b-all`.

```toml
# dash-x11: X11 of an 80-byte header, pinned per selection (Core: Pow_X11_0080b = 169,617)
x11-80b-all          = { bench = "dash-x11/x11", id = "x11/0080b/all" }
x11-80b-sse-aes      = { bench = "dash-x11/x11", id = "x11/0080b/sse-aes" }
x11-80b-sse-avx2     = { bench = "dash-x11/x11", id = "x11/0080b/sse-avx2" }
x11-80b-sse          = { bench = "dash-x11/x11", id = "x11/0080b/sse" }
x11-80b-standard     = { bench = "dash-x11/x11", id = "x11/0080b/standard" }
```

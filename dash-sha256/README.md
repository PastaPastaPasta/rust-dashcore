# dash-sha256

Dash Core's SHA-256 code in Rust: the same transforms and the same runtime dispatch
(`src/crypto/sha256*.cpp`, identical in v23.1.8 and v24.0.0-rc.3). Node-program task SH1
(`docs/node-program/DECISIONS.md` DEC-28). No I/O; the only state is the cached detection.

| Backend | Core | Selected when |
|---|---|---|
| portable | `sha256::Transform`, `sha256::TransformD64` | nothing better is allowed or present |
| SSE4 single stream | `sha256_sse4::Transform`, Intel's assembly | x86_64 with SSE4.1 |
| SSE4.1 4-way double SHA-256 of 64 bytes | `sha256d64_sse41::Transform_4way` | x86_64 with SSE4.1 |
| AVX2 8-way double SHA-256 of 64 bytes | `sha256d64_avx2::Transform_8way` | and AVX2 (OS-enabled) |
| SHA-NI single stream and 2-way | `sha256_x86_shani::*`, `sha256d64_x86_shani::*` | x86_64 with SHA-NI; replaces the SSE4/AVX2 code, as in Core |
| ARMv8 SHA2 single stream and 2-way | `sha256_arm_shani::*`, `sha256d64_arm_shani::*` | aarch64 with the SHA2 extension |

`Implementation::detect(Use)` is `SHA256AutoDetect(UseImplementation)`: the same choices, Core's
names (`"sse4(1way),sse41(4way),avx2(8way)"`, …), and Core's self-test, which panics on failure
where Core's `assert` aborts. `detect` runs the self-test on every call;
`Implementation::auto()` caches `detect(Use::ALL)`.

Where detection differs from Core (never the digest, only which backend runs):
- x86_64 SSE4 also requires SSSE3, which Core's assembly uses but Core does not check. Every
  SSE4.1 CPU has it.
- aarch64 uses std's `sha2` detection, which also requires the SHA1 and ASIMD hwcaps, and works
  on every OS std supports. Core checks `HWCAP_SHA2` on Linux and the sysctl on Apple only.
- No 32-bit backends: Core also builds its SSE4.1, AVX2 and SHA-NI code for i386, and its ARM
  SHA2 code for 32-bit ARM. Those targets get the portable code here, as do x86_64 with 32-bit
  pointers (the x32 ABI, where the SSE4 assembly's block count would be undefined) and big-endian
  aarch64 (the SHA2 code's loads assume little-endian lanes).

The SSE4 transform is a naked function without unwind information. Nothing unwinds through it,
but a debugger or sampling profiler stopped inside it may show a wrong caller.

## API

```rust
use dash_sha256::{Sha256d, sha256, sha256d, sha256d64, sha256d64_in_place};

let digest: [u8; 32] = sha256(b"abc");            // CSHA256
let txid: [u8; 32] = sha256d(b"raw tx");          // Hash / CHash256
let mut writer = Sha256d::new();                  // streaming SHA256d, no serialized copy
writer.update(b"part 1");
writer.update(b"part 2");
let _ = writer.finalize();

// SHA256D64 of 64-byte inputs: input[i] is two child hashes.
let pairs = [[0u8; 64]; 4];
let mut parents = [[0u8; 32]; 4];
sha256d64(&mut parents, &pairs);
assert_eq!(parents[0], sha256d(&pairs[0]));

// One merkle level in place, as Core's ComputeMerkleRoot does. The level must have an even
// length: as in Core, the caller first runs the mutation check, then duplicates an odd level's
// last hash.
let mut level = vec![[0u8; 32]; 8];
let n = sha256d64_in_place(&mut level);
level.truncate(n);
```

`Sha256` is `Clone`: hash a common prefix once and clone the midstate for each suffix (the
sighash prefix cache, C3 in `docs/node-program/designs/CS1-script.md`).

## Adoption

- **CS0** (`dash-consensus`, `src/hash.rs`; `docs/node-program/designs/CS0-node-primitives.md`
  §3, §7): `sha256d` is `dash_sha256::sha256d`, `HashWriter` wraps `Sha256d`, and
  `sha256d_64_many` is `dash_sha256::sha256d64`, or `sha256d64_in_place` for whole merkle
  levels. `dash-consensus` keeps `#![forbid(unsafe_code)]`; all of the unsafe code is here.
- **Later, not in SH1:**
  - rust-dashcore `dashcore_hashes` re-exports `bitcoin_hashes` 0.14's `sha256`/`sha256d`, and
    `dash/src/merkle_tree` hashes pairs one at a time. Both could compute with this crate and wrap
    the result (`Hash::from_byte_array`), and the merkle code could use `sha256d64`.
  - base-sdk hashes through `bitcoin_hashes` 1.x: `pkgs/primitives/src/codec.rs` (`Hashable::hash`,
    txids and block hashes) and `pkgs/types/src/adapters.rs` (script hashes). The node never calls
    these (CS0's sole-encoder rule), so switching them only helps SPV/wallet users.

## Verification

- `tests/vectors.rs`: NIST CAVP short, long and Monte Carlo vectors; Core's `crypto_tests`
  `sha256_testvectors` through a port of Core's `TestVector`; Core's `sha256d64` test.
- `tests/core.rs`: a fixture of digests computed by Core's own code (always runs); Core's digests
  of messages past 2^32 bits (512 MiB per implementation, so opt-in:
  `cargo test -p dash-sha256 --release --test core -- --ignored`); and a live differential
  against Core when `DASH_SHA256_CORE_HARNESS` is set.
- `tests/backends.rs`: proptest, every implementation against the portable one.
- Every test runs every implementation the CPU has. Set `DASH_SHA256_EXPECT` to the names that
  must be among them (separated by `;`) so a CI runner without, say, SHA-NI fails instead of
  testing less. Data provenance: `tests/data/PROVENANCE.md`.

The live differential, and the backends a CPU lacks under emulation:

```
contrib/core-harness/build.sh ~/workspace/dash 0364ed626ec2eaa518bf1ae4a0566ce638c7fe18 $H/core-x86_64
contrib/core-harness/build.sh ~/workspace/dash 0364ed626ec2eaa518bf1ae4a0566ce638c7fe18 $H/core-aarch64 aarch64

# native
DASH_SHA256_CORE_HARNESS=$H/core-x86_64 cargo test -p dash-sha256 --release
# SHA-NI, emulated: run the test binaries and the harness under QEMU
DASH_SHA256_CORE_HARNESS="qemu-x86_64 -cpu max $H/core-x86_64" qemu-x86_64 -cpu max target/release/deps/core-<hash>
# aarch64 SHA2, emulated
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -cpu max -L /usr/aarch64-linux-gnu" \
DASH_SHA256_CORE_HARNESS="qemu-aarch64 -cpu max $H/core-aarch64" \
    cargo test -p dash-sha256 --release --target aarch64-unknown-linux-gnu
```

## Benchmarks

`benches/sha256.rs` (criterion) runs Core's `bench_dash` shapes under Core's names:
`SHA256_{STANDARD,SSE4,AVX2,SHANI}` (1,000,000 bytes), `SHA256_32b_*`, `SHA256D64_1024_*` and
`DSHA256_1M`. `contrib/ins-per-byte.sh` reports user-space instructions per byte for the same
workloads (`examples/workload.rs` under `perf stat`), the unit nanobench reports.

Instructions per byte on the node program's benchmark machine (Xeon E5-2687W v4, AVX2, no SHA-NI),
2026-10-08. The Core column is the `bench_dash` B1 built from the v24.0.0-rc.3 tag
(`node-bench/scripts/build-dashd-bench.sh`): Debian bookworm's GCC 12.2.0, Core's `depends`
and its default configure flags (`-O2`, hardening on), as nanobench reports it. dash-sha256 is
built with rustc 1.98.1 in the `release` profile.

| Bench | Core v24.0.0-rc.3 `bench_dash` (GCC 12.2) | dash-sha256 |
|---|---|---|
| SHA256_STANDARD | 52.88 | 51.78 |
| SHA256_SSE4 / _AVX2 | 36.80 | 36.80 |
| SHA256_32b_STANDARD | 115.78 | 111.75 |
| SHA256_32b_SSE4 / _AVX2 | 83.41 | 81.44 |
| SHA256D64_1024_STANDARD | 133.45 | 132.47 |
| SHA256D64_1024_SSE4 | 58.52 | 52.42 |
| SHA256D64_1024_AVX2 | 20.75 | 18.45 |
| DSHA256_1M | 36.80 | 36.80 |

`_SHANI` falls back to SSE4 on this CPU, in both.

The Core column depends on how Core is compiled, and the comparison holds as stated only for that
build:
- **Assembly and SIMD rows:** the single-stream SSE4 rows are the same assembly, so they tie
  whatever the compiler. The D64 SSE4.1 and AVX2 rows stay ahead of Core across the builds the
  SH1 review tried, GCC 15.2 `-O2` with and without Core's hardening flags (Core 60.62–62.75 and
  19.92–20.11).
- **Portable rows:** these depend on the compiler. Core's portable code built with GCC 15.2 `-O2`
  takes slightly fewer instructions than this port: 50.88 for `SHA256_STANDARD` and 109.94–110.88
  for `SHA256_32b_STANDARD`, so the two are within about 2% either way. No x86_64 node runs the
  portable code, and aarch64 nodes with the SHA2 extension don't either.

# SB2 notes: the mobile release profile

PLAN row SB2 (NFR-CMP-03): `[profile.mobile]`, its stripped size measured and recorded (target
≤ 9.0 MB), and `spv-headers-*` instruction counts under QEMU aarch64 not higher than `release`.
Branch `node/sb2-mobile-profile`, cut from node/integration 2adae7489. Measurements were made in
`/work/scratch/sb2` (scripts and logs there) with rustc 1.98.1 (the repo's pin), NDK 29.0.14206865, API 24.

## Result

| artifact (stripped) | `release` | `mobile` |
|---|---|---|
| `libdash_spv_ffi.so`, aarch64-linux-android | 13,206,000 B | **9,504,624 B** (-28.0 %) |
| `libkey_wallet_ffi.so`, aarch64-linux-android | 5,728,472 B | **4,291,888 B** (-25.1 %) |
| `libdash_spv_ffi.so`, x86_64 host (GNU `strip`) | not built | 10,625,680 B |
| `libkey_wallet_ffi.so`, x86_64 host (GNU `strip`) | not built | 4,389,496 B |

`release` is the DEC-304 reference, 13,206,000 B, reproduced to the byte at this head. The size
target (9.0 MB) is **missed by 504,624 B** (9.50 MB; 9.06 MiB, 67,440 B over 9 MiB); see D5.

`spv-headers-*` on QEMU aarch64 (host `instructions:u` per iteration, see D6):

| row | `release` | `mobile` |
|---|---|---|
| `headers/bridge/2000` | 761,913 | 492,023 (lower here; equal natively, D13) |
| `headers/hash_and_bridge/2000` | 1,312,756,055 | 1,284,719,816 (equal within noise, D13) |
| `headers2/decompress_and_bridge/8000` | 5,295,699,813 | 5,177,486,145 (equal within noise, D13) |

The percentages the first version of this table gave (-35 %, -2.1 %, -2.2 %) are withdrawn
(Opus r2 m-5, DEC-326): D13 shows the profile is equal to `release` on all three rows within the
measurable resolution, and about 0.3 % *above* it on the hash rows on x86_64.

## D-rows

**D1. Override list: the hashing crates.** `dash-pow`, `dash-x11`, `dash-sha256`,
`dashcore_hashes` (a Rust crate) and the C backends `blst` and `blake3` are at `opt-level = 3`.
BS15 has landed (dfac5e32c), but `dash-pow` is still a dependency of `dash-consensus`
(unconditional; `dash-params` mines devnet genesis blocks with it) and of base-sdk's
`dash-primitives` (`cargo tree -p dash-spv-ffi -i dash-pow --target aarch64-linux-android`), so it
stays in the list, as the brief says. No `dash_pow::` symbol is in either shipped library, in the
bench binary or in the release bench binary today (X11 is `dash-x11`), so its override changes
nothing in these artifacts now. It is a guard for a configuration that links it. Without the
hashing overrides X11 is 87 % above `release` (2,455,928,494 vs 1,312,756,055 instructions on
`hash_and_bridge/2000`), for 34,496 B of size (9,306,176 → 9,271,680 B on the intermediate
profile), so they earn their place.

**D2. No cross-crate LTO (`lto = false`), against the PLAN's "fat LTO".** With `lto = "fat"`,
`opt-level = "s"` and the D1 overrides (9,300,056 B), `hash_and_bridge/2000` was **+6.5 %** over
`release` (1,398,849,303; +6.6 %), `headers2/...8000` +7.2 % and `bridge/2000` 2.6x. The same profile with
`opt-level = 3` everywhere (10,054,024 B) matched `release` (1,312,067,995). That fits rustc running
the loop passes (unrolling, vectorising) at the *linking* crate's opt-level when LTO is on: the
per-package `opt-level = 3` then cannot make up the difference (no fat-LTO run without the overrides was made). Without LTO the overrides apply, and the size is the
same: 9,306,176 B vs 9,300,056 B, fat LTO saves 6 KB (0.07 %). The brief's acceptance (BG: not
higher) ranks above the PLAN text, so `lto = false`, `codegen-units = 1`.

Sol r1 asked for the LTO middle. Measured with the committed override list (D4) and the same
harness (stripped `libdash_spv_ffi.so`; host instructions per iteration in millions, bridge /
hash_and_bridge / headers2):

| profile | size | bridge/2000 | hash_and_bridge/2000 | headers2/8000 |
|---|---|---|---|---|
| `lto = false` (committed) | 9,504,624 | 0.492 | 1284.72 | 5177.49 |
| thin LTO | 9,493,408 | 0.326 | 1280.45 | 5153.66 |
| thin LTO, `dash-spv-ffi` at 3 | 9,614,120 | - | - | - |
| fat LTO, `dash-spv-ffi` at 3 | 9,615,504 | 0.312 | 1311.16 | 5267.98 |

Fat LTO with the linking crate at 3 is 110,880 B larger and its hash rows only match `release`
(-0.1 % / -0.5 %), so it loses on both. Thin LTO at the same settings as the committed profile is
11,216 B (0.12 %) smaller with rows 0.3 % to 0.5 % lower, which is inside the noise (D6), but that
measurement does not describe the shipped library: the bench links through `dash-spv` (already at
3), the Android library through `dash-spv-ffi`, and when that crate is also at 3, as in the bench,
the size is 109,496 B *larger* than the committed profile. Putting the linking crate at `s` would
make the bench unrepresentative the other way (the header path is then optimised at `s`, as in the
fat-LTO run above), and no existing bench links through `dash-spv-ffi` (building one is not worth 11 KB). So the 11 KB is not adopted:
`lto = false` stays (manager ruling DEC-318 accepted D2). Each variant was built in its own scratch
target and deleted.

**D3. C backends at -O3: only partly, and not as a flag.** Cargo passes a package's opt-level to its
build script as `OPT_LEVEL`, and `cc` turns that into `-O3`, so `blake3` (and `blst`) get `-O3` from
the override. `blst`'s `build.rs` calls `cc.opt_level(2)` itself, so it stays at `-O2`. Forcing
`-O3` on it means `CFLAGS_aarch64_linux_android=-O3`, which is process-wide (it would also rebuild
the bundled SQLite and `secp256k1-sys` at -O3, against the L7 size rule), cannot be scoped to one
crate, and cargo cannot set an env var per profile; the build entry points would each have to export
it. Not done; not measured. BLS verification is not on the `spv-headers-*` path. `secp256k1-sys` is
signature code, not a hashing path, and stays at `s`.

**D4. Header path at `opt-level = 3`: `dash-spv`, `dash-consensus`, `dashcore`, `dash-primitives`,
`dash-types`, `dash-chain`, `dash-num`, `dash-params`, `bitcoin_hashes`, `dash-p2p-core`.** With
only D1, the three `spv-headers-*` rows were not all at or below `release` (`headers2` +1.95 %,
`bridge/2000` 4.9x): the code that moves headers (bridge, `Headers2` decompression, the base-sdk
types) was at `s`. Variants (the three fat-LTO rows aside, `lto = false`, one codegen unit; stripped size; host instructions per
iteration, bridge / hash_and_bridge / headers2 in millions):

| profile | size | bridge/2000 | hash_and_bridge/2000 | headers2/8000 |
|---|---|---|---|---|
| `release` | 13,206,000 | 0.762 | 1312.76 | 5295.70 |
| fat LTO, `s`, D1 (the PLAN) | 9,300,056 | 1.992 | 1398.85 | 5679.20 |
| fat LTO, 3 everywhere | 10,054,024 | - | 1312.07 | - |
| fat LTO, 2 everywhere | 9,717,664 | - | 1312.86 | - |
| `s`, no overrides | 9,271,680 | - | 2455.93 | - |
| `s`, D1 | 9,306,176 | 3.712 | 1299.80 | 5399.09 |
| D1 + dash-spv, dash-consensus, dashcore | 9,533,048 | 0.982 | 1329.50 | 5411.56 |
| D1 + those + dash-primitives, dash-types | 9,523,808 | 1.494 | 1283.76 | 5178.23 |
| **D1 + the ten above (committed)** | **9,504,624** | **0.492** | **1284.72** | **5177.49** |
| 2 everywhere (+ D1) | 9,737,312 | 0.379 | 1372.43 | 5530.49 |
| 3 everywhere (+ D1) | 10,089,992 | 0.311 | 1284.75 | 5177.31 |

The committed list is the smallest I verified to pass all three rows (sufficient, not proven minimal); the five-crate subset fails
`bridge/2000` (1.49M vs 0.76M), and the other crates of the ten were not tried one by one, so some
may be unnecessary. The list is cheaper than `opt-level = 3` everywhere (-585,368 B) and passes the
same rows. If the header path changes (SB12/SB14 move headers onto `dash-consensus`), re-run the
rows against this profile.

**D5. Over 9.0 MB: contributors.** (`llvm-nm -S --size-sort -C` on the unstripped library, named
symbols only, 6.3 of the 9.5 MB; sections of the stripped file: `.text` 5.05 MB, `.rodata` 1.82 MB,
`.eh_frame` 0.78 MB, `.rela.dyn` 0.74 MB, `.data.rel.ro` 0.49 MB, `.gcc_except_table` 0.44 MB.)
- `secp256k1-sys` 1,228,775 B, of which the two precomputed ecmult tables
  (`rustsecp256k1_v0_14_pre_g`, `_pre_g_128`) are 524,288 B each and `ecmult_gen_prec_table` 88,064 B.
  Their size is a build-time window-size choice of the C library (fewer table bits shrink them by
  about 1 MB at a signature-verification cost), not a profile setting.
- `dash-spv` 969,664 B (729,596 B at `s` with fat LTO), `core` 737,748 B,
  `alloc` 616,728 B, `tokio` 543,332 B, SQLite 317,832 B, `dashcore` 292,341 B, `std` 200,326 B,
  `dash_x11` ~156 KB.
- Unwinding tables (`.eh_frame`, `.eh_frame_hdr`, `.gcc_except_table`) are ~1.33 MB. `release` sets
  `panic = "unwind"` on purpose, so `panic = "abort"` would be a behaviour change at the FFI boundary,
  not a profile tweak.
Neither was changed here (the brief says not to hack the code to reach the number). Follow-ups for
the manager: the ecmult window size, and whether the mobile products can take `panic = "abort"`.

**D6. How the instruction counts were measured.** `dash-spv`'s `headers` bench, built for
`aarch64-unknown-linux-gnu` (the Android binary cannot run under `qemu-user`; bionic's linker), once
with `--profile release` and once with `--profile mobile`, with PF0's criterion shim patched in
(`cargo update -p criterion@0.8.2` onto `contrib/node/perfgate/criterion-shim/v0.8`) in a scratch
worktree so this branch's `Cargo.lock` is untouched. Run as `PERFGATE_MODE=syscalls
PERFGATE_ITERS=N`, which executes exactly N iterations without the shim's `perf_event_open`, under
`perf stat -e instructions:u qemu-aarch64 -cpu max -L /usr/aarch64-linux-gnu`. Per iteration =
(count at N=5 - count at N=1) / 4, which removes startup, fixture decoding and QEMU's translation.
Caveats: (1) these are *host* instructions of the emulator, not guest instructions. The agentbox QEMU
(10.2.1 Debian) has no TCG plugin support, and `-d exec -one-insn-per-tb` runs at ~125 K insn/s, which
is too slow for 10^9. (2) The count depends on code layout. Two builds of identical settings (the
committed profile built as `mobile` and as a variant profile name) differ by 0.03 % on the two hash
rows and by 0.05 M on the 0.5 M-instruction bridge row, and the D4 table shows swings of up to about ±2 % on the hash rows between
nearby profiles, and two runs of the same `release` binary differ by 2.3 % on the bridge row. A result
within ~±2 % on the hash rows is not distinguishable from `release`: the committed profile's -2.1 %/-2.2 % there is inside the noise and
is not a win, and the bridge row's -35 % is withdrawn too (it does not reproduce natively; D13).
(3) `release` in the same harness: 761,913 / 1,312,756,055 / 5,295,699,813 (a second run of the
same binary agreed to within 0.02 % on the hash rows). (4) Under TCG the host cost of a guest
instruction is not uniform (NEON and crypto-extension ops under `-cpu max`), so "not higher" holds for
this emulator metric, not for hardware; an Android emulator or device run (SB11) would settle it.

**D7. L7 re-record (DEC-318).** `check-mobile.sh` builds with `--profile mobile`
once the workspace has one, which moves the L7 environment (profile release → mobile): a baseline
migration, refused by `check-layers.py size` without a decision new to this change that states the
values. DEC-318 states them and `check-mobile.sh --record --ruling DEC-318` wrote
`mobile-size.txt`; `check-layers.py size 9504624 --profile mobile ...` then exits 0. The DEC-170 allowance applies from the new value.
DEC-318 is node/integration's row (the branch carries no `DECISIONS.md` change, DEC-326 m-2); it
states the profile and bytes. `dash-spv-ffi` and `key-wallet-ffi` needed no change: their
`crate-type`s and build scripts do not depend on the profile. The record's 9,504,624 B was built from
a dirty tree and embeds git-state's 64 B `-dirty` marker; a clean build at the same head is
9,504,560 B (Opus r2 n-1). Not re-recorded: DEC-318 states 9,504,624 B, and a clean builder is
smaller than the record, so L7 passes either way. The build docs are D14.

**D8. Bench inputs.** `perfgate`'s `bench_inputs` hashes only `[profile.release]` and
`[profile.bench]` of the root manifest, so adding `[profile.mobile]` changes none: all 13 bench
packages are SAME against 2adae7489 (`/work/scratch/sb2/bench-inputs.txt`). `release` and `dev` are
unchanged.

**D9. Not done / skipped.** `cargo bloat` is not installed; `llvm-nm --size-sort` was used. The iOS
target was not built (no Apple tooling). The host `release` sizes were not measured.

**D10. Selftest cases that assume the reference's profile (waived, BL-329).** The L7 cases in
`contrib/node/selftest-checks.sh` plant records with the reference's profile (release) and expect
the working record to match it; with a recorded profile of `mobile` they fail, for example "+100 KiB"
reports "over the 9504624 B that ruling DEC-318 covers". Not run by SB2's gate (no gate script
changed, DEC-299). DEC-318 waives them at SB2's train, as DEC-304(a) did, and BL-329 has SB1 fix
them; SB2 changes no selftest. The cases Sol r1 names (`selftest-checks.sh:1490-1534, 1573-1593`),
for SB1: 1498 (+100 KiB, expects pass), 1499 (+300 KiB), 1509 (`recheck`), 1521 (`redirected`),
1528 (`ruled`), 1531 (`ruled_over`), 1533 (`migrated`), and the fixture cases 1577 (`cited`),
1578 (`cited` with the NDK left out; it now reports `gives profile release, ndk 29.0.14206865, which
DEC-99998 does not state`, and the case's regex expects `gives ndk 29.0.14206865, which`) and 1593
(`moved_constant`): ten cases. Opus r2 replayed the whole L7 block of `selftest-checks.sh`
(1490-1593) on 7efb06370 and found exactly these ten failing.

The first seven read the reference from the real merge base, so they pass again once node/integration
carries the mobile record; only the train's own merge commit, whose first parent is pre-SB2, still
fails them. **1577, 1578 and 1593 build their own fixture repo** (`no_record_base`, from HEAD's
`layers.toml` and `mobile-size.txt`), so they keep failing after the train until BL-329 lands: the
fixture's `[size]` still says `profile = "release"` (`layers.toml:170`) while its `size_text` comes
from the mobile record, so the decision rows never state the `[size]` profile. BL-329 (DEC-326) covers
them; its fix is to derive the fixture's `size_text` from `layers.toml`'s `[size]`, the record the
fixture cites (moving `[size]` to mobile would itself need a DEC-170 ruling).

**D11. Scope the measurements do not cover.** (a) The filter scan: `dashcore::bip158` delegates to
`dash-filters`, which is at `s` and was not measured (its NEON `siphash` code is in the library);
the override list is for header sync only. (b) `key-wallet` (BIP32 gap-limit derivation) and
`secp256k1-sys`'s C code are at `s` and were not measured. (c) Fixed in fix round 1 (Sol r1 minor 3): see D12. (d) The stripped library embeds ~170 absolute CARGO_HOME paths, so L7's
zero tolerance over the recorded bytes depends on the path of the builder's cargo home (not new with
SB2). (e) Without LTO the iOS/Android *static* libraries (`.a`) are not link-time-trimmed in the
build; the app's linker does it. Not measured. (f) The three rows measure less of the shipped
header path than D6 suggests (Opus r2 m-3, BL-337): `dash_spv::bridge::headers::{headers_to_consensus,
headers_from_consensus}` and `bridge::messages::headers2_to_consensus` have no production caller in
dash-spv today (only benches and tests; they are SB12/SB14's future path). Today's live sync runs
`HashedBlockHeader::from` (`sync/block_headers/segment_state.rs:289,317`: dashcore, `dashcore_hashes`,
`dash-x11`), non-generic code in crates at opt-level 3, so X11, the dominant cost, runs at `release`
speed. The generic layers around it (`BlockHeadersManager<H, M>`, `DashSpvClient<W, N, S>`, the
storage) are monomorphized inside `dash-spv-ffi` at opt-level `s`; without LTO generic code compiles at
the instantiating crate's opt-level, so `dash-spv`'s override does not reach it, and no row measures
it. PF0 builds only `release`, so nothing re-checks the mobile profile after SB2. BL-337 tracks a
mobile-profile bench of the live sync path with a gate step, and the `dash-x11` `codegen-units`
decision (D13).

**D12. The allocator proof builds the mobile products as `mobile`.** `layers.toml`'s product rows for
`dash-spv-ffi` and `key-wallet-ffi` are split: the host row stays `release`, the
`aarch64-linux-android` row has `profile = "mobile"` (`check-products.py` already supported a per-row
profile, so no script change). `contrib/node/check-products.sh dash-spv-ffi key-wallet-ffi` exits 0:
`ok: dash-spv-ffi dash_spv_ffi (host, default features): the canary links (389 s)`; `ok: dash-spv-ffi
dash_spv_ffi (aarch64-linux-android, default features): the canary links (319 s)`; `ok: key-wallet-ffi
key_wallet_ffi (host, default features): the canary links (82 s)`; `ok: key-wallet-ffi key_wallet_ffi
(aarch64-linux-android, default features): the canary links (142 s)`. `check-products.py --registry`
exits 0.

**D13. The hash rows are equal to `release` within noise, not a win (Opus r2 m-5, DEC-326).** Opus r2
measured the same `headers` bench natively on x86_64 (`perf stat -e instructions:u`, deterministic,
per iteration = (N=5 - N=1) / 4, two runs each; repeats agree to within 34 instructions on
`hash_and_bridge`, so these are not noise) at 7efb06370, built once as `release` and once as `mobile`:

| row | `release` | `mobile` | delta |
|---|---|---|---|
| bridge/2000 | 70,217 / 70,258 | 70,300 / 70,116 | equal (+-0.2 %) |
| hash_and_bridge/2000 | 78,373,872 / 78,373,838 | 78,666,693 / 78,666,744 | **+0.37 %** |
| headers2/8000 | 317,458,337 / 317,528,141 | 318,322,310 / 318,266,203 | **+0.25 %** |

The whole difference is inside `dash_x11`, which is at opt-level 3 in both profiles: `mobile` sets
`codegen-units = 1` where `release` uses 16, which changes inlining inside that crate. With
`codegen-units = 16` on the `dash-x11` override alone, the rows match `release` (+0.007 % and -0.1 %)
for +5,280 B on the Android library (9,504,560 B to 9,509,840 B, +0.06 %). The bridge row's -35 % under
QEMU (D6) does not reproduce natively (equal), so it may be a TCG or aarch64 artifact. The statement
that stands, at the resolution available (QEMU's host-instruction proxy cannot see 0.3 %): the three
rows are **equal to `release` within noise**, and the hash rows are up to about 0.4 % above it
natively. The -2 % and -35 % claims are withdrawn from the Result table and D6. The `dash-x11`
`codegen-units` change is **not made** here (DEC-326): it needs a QEMU aarch64 check and an L7
re-record, and is part of BL-337.

**D14. Build docs and scripts use the mobile profile (Opus r2 m-4, DEC-326).** Updated to
`--profile mobile` and to the `target/<triple>/mobile/` output paths (the library and the generated
`include/` header): `key-wallet-ffi/README.md` (iOS and Android lines; the host line stays
`--release`), `key-wallet-ffi/build-ios.sh`, `dash-spv-ffi/README.md`, `dash-spv-ffi/CLAUDE.md`, and
the allocator-canary comment in `.github/workflows/rust.yml`. **`build-ios.sh` is untested**: iOS
cannot be built here (no Apple tooling). Its change is limited to the `--release` to `--profile
mobile` flags and the `release` to `mobile` path components (the `target/universal/` output
directory is renamed the same way). The README's iOS line replaced `cargo lipo --release` with two
`cargo build --profile mobile --target ...` lines, because cargo-lipo's profile support is unknown to
me; also untested.

# P1 — Sol review, round 2

**Scope:** narrow confirmation of r1 F1–F3 and the reserved-bits hook's build isolation. **Range:** `a6af9505` → `3cf32aee` (9 commits), `node/p1-p2p-transport`; verified HEAD `3cf32aee4fc514e236f09e3b1c02b5204966cac5`. Followed `briefs/REVIEW-COMMON.md`. The reviewed worktree remained clean. This report is uncommitted on `node/integration`. PLAN §7 handling: codenames only; no private material inspected.

## Findings by severity

No blocker or major findings in this fix range.

### Minor R2-1 — The reserved-bits hook is callable in a normal build

**Location:** `dash-p2p-transport/src/cipher.rs:375` (and the call at line 365); `dash-p2p-transport/src/lib.rs:26` exposes the cipher module.

`Bip324Cipher::encrypt_with_header` is an unconditional public method. There is no `cfg(test)`, `cfg(fuzzing)`, or feature guard, and the crate declares no feature that isolates it. Its documentation steers production callers towards `encrypt_in_place`, but documentation does not make the hook test-only.

A separate downstream executable, built normally with no transport features, no `cfg(fuzzing)`, and `cfg(test) == false`, called `encrypt_with_header(..., 0x7f)`. A second cipher successfully authenticated the packet and recovered header `0x7f`. Thus an ordinary sender can emit nonzero reserved bits through the claimed test-only API. The regular `V2Transport` send path continues to use the zero/ignore-only API; the defect is the requested build/API isolation, not a demonstrated change in its default wire output.

**Suggested fix:** put the shared implementation in a private helper, keep `encrypt_in_place` available normally, and expose the arbitrary-header wrapper only through an explicit test/fuzz configuration or a nondefault test-support feature. Enable that configuration deliberately for the model and relevant integration tests. Add a downstream compile-fail check for an ordinary build, alongside a positive test/fuzz build check.

**Evidence:** `/work/scratch/p1-r2-sol/probes/examples/review_normal_hook.rs` and `normal-hook.log`. Confirmation item 4 fails.

## Confirmation of the r1 fixes

### F1 — Closed

The capacity check in `src/v2.rs:351` matches v24 Core's `net.cpp:1424–1441`: reserve only when the bytes being consumed do not fit, and cap reserve-ahead at 256 KiB. APP_READY still returns before consuming more input.

Reran the original r1 public-API counting-allocator probe unchanged. Its legal 3 MiB APP payload now takes **13 reallocations with one-byte fragments** and **12 with 64-byte fragments**, versus r1's 2,883,602 and 45,057.

Extended the same allocator to a bare cipher peer and legal **3 MiB VERSION contents**, followed by a **3 MiB APP payload**. Both transport roles produce the same counts:

- VERSION: **13 / 12 reallocations** for one-byte / 64-byte fragments.
- APP: **13 / 12 reallocations** for one-byte / 64-byte fragments.

Every VERSION packet authenticates, consumes completely, transitions from Detecting to V2, and delivers no application message, as Core requires. Every APP command and entire payload match the original bytes. The forced-moving allocator repeats all eight role/state/fragment combinations successfully, copying **20,447,349 bytes** per one-byte case and **17,305,731 bytes** per 64-byte case. This also removes r1's quadratic copying scenario.

The precise allocation bound follows Core's fixed reserve-ahead steps, **O(packet length / 256 KiB)**, and is independent of the number of fragments; this is not a geometric O(log length) reservation policy. The observed counts match the r1 capacity-guard control. The committed tests also cover maximum VERSION contents, the 3 MiB APP payload, and a 1 KiB prefix whose reservation must stay within received bytes + 256 KiB.

**Evidence:** `probes/examples/review_alloc.rs`, `review_alloc_extended.rs`, `alloc-r1-rerun.log`, `alloc-extended.log`, and `alloc-extended-moving.log` under `/work/scratch/p1-r2-sol`.

### F2 — Closed

`sanitize_command` uses Core's exact default character set and stops at the first NUL, matching `CMessageHeader::GetCommand()` plus `SanitizeString`. The separate probe verifies newline, ESC, invalid non-ASCII bytes/UTF-8, and embedded NUL in both formatting paths. Structured errors retain the original 12 command bytes.

`Display` gives sanitized commands `pingFORGED`, `a31mb0m`, `ping (x)`, and `ver`. `Debug` preserves escaped representations including literal `\n`, `\x1b`, `\xff`, `\xc3\xa9`, and `\x00`. Neither output contains a raw control or non-ASCII byte.

**Evidence:** `probes/examples/review_error.rs` and `error-format.log`.

### F3 — Hostile authenticated coverage closed; hook isolation remains R2-1

The new scripted mode reaches the requested states through a bare `Bip324Cipher`, after a real key exchange and the derived garbage terminator. I constructed 96 public scripts independently of the implementation's seed helper and included them in the fuzz smoke. They cover both roles, cuts inside/after the length descriptor and one byte before completion, and:

- authenticated decoys before and after VERSION, including consumption of first-packet garbage AAD;
- nonempty VERSION packets and successful following APP traffic;
- invalid long types, invalid short ID 200, and reserved short ID 5 yielding the empty command;
- descriptors above the limit, legal near-limit descriptors with the body withheld, and shortened forged descriptors;
- incorrect tags, mutated ciphertext, and incorrect AAD on first and later packets;
- reserved headers `0x7f` and `0xff`, checking their normal-packet/decoy outcomes;
- VERSION and APP contents at 256 KiB + 1, 1 MiB, and the maximum 3 MiB + 13, fragmented in one-byte and 64-byte pieces.

The expected state outcomes in `expected_scripted` (`node-fuzz/src/models/p2p_transport_model.rs:867`) come from script metadata and Core's rules, before feeding the transport. They do not call `V2Transport`, its type parser, or its short-ID validation. `expected_type` uses the separate Core-derived tables and independently checks long-type bytes. I checked length-before-body rejection, authentication failure, first-packet AAD clearing (including decoys), VERSION consumption, invalid-type drops, and reserved-bit handling against pinned v24 `net.cpp`. The shared cipher constructs authenticated inputs; cryptographic correctness is separately checked by the passing Core-vector suite.

The model installs its counting allocator in the fuzz binary and checks receive allocation counts and largest allocation. Construction is statically bounded to 32 packets, one large contents vector of at most 3 MiB + 13, and small payloads of at most 4096 bytes elsewhere. An absent large-packet selector defaults to zero and cannot enter the large branch. An explicitly supplied selector ≥240 can still request a bounded large packet when later script fields are absent; that is the exact truncation guarantee. Misaligned terminator cases use a weaker no-message/error invariant; the listed aligned hostile states have exact independent outcomes.

**Evidence:** `public-seeds/`, `seed-manifest.txt`, the seven passing model unit tests, and the saved fuzz run below.

## Validation

All reviewer builds/tests/probes used **nice 10**, **`RUSTC_WRAPPER=`**, and mode-700 **`TMPDIR=/dev/shm/p1-r2-sol-tmp`**; Cargo used 12 jobs. The fuzz runner's own temporary directory was also inside this tree and mode 700. No target directory was shared between worktrees. No shared chain instances were touched. All reviewer-started dashd instances exited, fuzz artifacts were saved to disk, and the reviewer-owned `/dev/shm` tree was removed.

- `DASHVERSION=24.0.0-rc.3 python3 contrib/setup-dashd.py`, source its exports, and set `DASHD_V24_PATH="$DASHD_PATH"`; then `cargo test --locked -p dash-p2p-transport --all-features -- --nocapture`: **42 passed, 0 failed**. All five live interop tests ran, using RPC version **240000**. Both V2 directions/session IDs, ping/pong, inbound V1 fallback, and outbound V1 reconnect passed. The 1000-sample KS statistics were **0.0220 / 0.0204 / 0.0153**, within the suite's limits. Core cipher vectors also passed.
- `cargo clippy --locked -p dash-p2p-transport --all-features --all-targets -- -D warnings` and `cargo fmt --check -p dash-p2p-transport`: **passed**. Fix-range `git diff --check`: **passed**.
- `cargo test --locked --lib models::p2p_transport_model::tests` in `node-fuzz`: the ordinary debug test-thread stack initially **overflowed in unchanged `dash-sha256` AVX2 initialization**, localized with gdb. The same command with **`RUST_MIN_STACK=33554432` passed all 7 tests**, including scripted faults/cuts and large fragmented packets. The node-fuzz workspace does not inherit the root's SHA-256 optimization override; the root already documents this debug-stack limitation. The supported optimized fuzz run required no stack override.
- `node-fuzz/run.sh p2p_transport_model --pin v24.0 --cpu-hours 0.017 --jobs 1 --public-seeds /work/scratch/p1-r2-sol/public-seeds --run-tag 0000000000000022`: **passed**, **15,832 executions**, **0 divergences / crashes / sink failures / refusals**, **0.0226 CPU hours**, four segments. The run records clean revision **`3cf32aee4fc5`**. Artifacts: `/work/scratch/p1-r2-sol/fuzz-sink/fuzz/p2p_transport_model/runs/20261009T234648Z-v24.0-2534421-0000000000000022/`, including `verdict.json`; summary in `fuzz-summary.log`.

This narrow confirmation does not repeat the full integration/workspace gate or expand the r1 scope.

## Verdict

**CHANGES-REQUIRED.** F1 and F2 are closed, and F3's requested hostile authenticated states now have independent expected outcomes. The remaining minor R2-1 directly fails the explicit normal-build isolation check for the reserved-bits hook; isolate it before passing this gate.

**Sol gate satisfied: no**

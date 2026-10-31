> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-555 narrow re-review, r2 (Sol)

Reviewed `1754a05e0a85d826a24378c6a067766fff8ba510..d51430547f37dab03ae14b132f0b9c978b0cb7cf` in the read-only worktree `/work/home/agent/workspace/rdc-wt/nd1-bl555`, following the BL-555 section of `r2-narrow-brief.md` and `briefs/REVIEW-COMMON.md`. Scope: Sol r1 findings 1 and 2, plus new defects in the fix's touched lines.

- **Finding 1 (minor): CLOSED** — `dashd-store/src/flatfile/tests.rs:1504`: a separate file in the same temporary directory now probes native `posix_fallocate` success and block accounting before the physical-reservation assertion. Unsupported allocation skips that assertion. The unconditional extension/content test remains active and passes under the unsupported-allocation shim.
- **Finding 2 (minor): CLOSED** — `dashd-store/src/flatfile/fs.rs:246`: `offset.checked_add(len)` rejects overflowing ranges before any write. For accepted ranges, every intermediate offset is bounded by the checked original end, so advancing after a failed write cannot wrap. The regression at `dashd-store/src/flatfile/tests.rs:1543` exercises the reported `(u64::MAX - 10, 65_537)` range and checks that the entire file remains unchanged; ordinary multi-piece writes and extension remain covered.

New defects in the touched lines: **none found**. The additional trait documentation agrees with Core's unused-tail assumption and platform-specific length behavior.

Verification:

- Inspected the complete fix delta and checked the relevant allocation contract against Dash Core tags `v23.1.8` (`src/util/system.cpp`) and `v24.0.0-rc.3` (`src/util/fs_helpers.cpp`). `git diff --check 1754a05e0 d51430547` passed.
- Pre-build checks: five-minute load 45.88; `/work` available space 196 GiB. The brief requires at least 200 GiB, so no build was attempted and no Cargo/shared build cache was used.
- Reflink-copied the owner's existing test binary `/work/scratch/nd1-bl555/target/debug/deps/dashd_store-168ea8e3413d8989` (mtime 2026-10-31 03:09:06 UTC) into this review's scratch. Binary SHA-256: `0f4e863f168da578a8a88b9c5fa03afbede4bdae96cff6ef7e347d21a0b50ebe`. This is execution of an existing artifact, not an independent rebuild of the reviewed commit.
- Ran that binary with `flatfile::tests --test-threads=8 --nocapture`, using its own mode-0700 `TMPDIR=/dev/shm/bl555-r2-sol-tmp`: **47 passed, 0 failed, 1 existing ignored**. The reservation test did not skip. Log: `/work/scratch/bl555-r2-sol/native-tests.log`.
- Repeated with a scratch copy of the inspected `LD_PRELOAD` shim returning `EOPNOTSUPP` from both `posix_fallocate` symbols: **47 passed, 0 failed, 1 existing ignored**. The reservation test printed its skip message; extension/content and overflow regression tests passed. Log: `/work/scratch/bl555-r2-sol/fallback-tests.log`.
- Removed its own RAM test directory after testing. Reviewed worktree remained clean at the stated head. No branch edits, commits, pushes, external posts, or reads of `/work/cache`, `/work/chains`, or restricted notes trees.

Verdict: **APPROVE**.

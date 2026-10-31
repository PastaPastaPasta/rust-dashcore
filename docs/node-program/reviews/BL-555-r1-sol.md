> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-555 correctness review, r1 (Sol)

Reviewed head `1754a05e0a85d826a24378c6a067766fff8ba510`, one commit above `695b8ae3e`, in the frozen worktree `/work/home/agent/workspace/rdc-wt/nd1-bl555`. Followed `briefs/REVIEW-COMMON.md`. The reviewed worktree and branch were not modified; no commits, pushes, or external posts were made.

No blockers or majors.

## Findings

1. **Minor — reservation test rejects a supported fallback outcome.** `dashd-store/src/flatfile/tests.rs:1510`.

   `allocate_reserves_the_file_from_its_start` is enabled on every Linux filesystem, but `OsFs::allocate` deliberately accepts a failed `posix_fallocate` and zero-fills only `[offset, offset + len)`. If the libc/filesystem cannot preallocate (for example, musl on a filesystem returning `EOPNOTSUPP`), the original 8 MiB hole legitimately remains sparse. The file has the correct 9 MiB length and zero contents, but the test requires 9 MiB of physical allocation and fails. Closing the handle addresses speculative allocation on XFS/ext4; it does not establish preallocation support or make block accounting portable across filesystems.

   Reproduced without changing the reviewed code: a scratch `LD_PRELOAD` shim returning `EOPNOTSUPP` from `posix_fallocate`/`posix_fallocate64` makes the extension test pass and this test fail with `2048 blocks` (1 MiB). Artifacts: `/work/scratch/bl555-r1-sol/no-fallocate.c` and `.so`.

   Suggested fix: probe successful native preallocation on a separate file in the same temporary directory before asserting physical reservation, and skip that assertion when unsupported. Keep an unconditional fallback length/content test. Alternatively, pin the syscall arguments through a narrow test seam rather than inferring them solely from filesystem accounting. These tests do not depend on permission denial and the two allocation tests pass as root.

2. **Minor — zero-fill can wrap an invalid range into existing data.** `dashd-store/src/flatfile/fs.rs:248`.

   The old loop bounded its work by a saturated end and returned after a failed write. The extracted loop instead advances the full `len` using unchecked `offset += now`, even after a write error. For `allocate(path, u64::MAX - 10, 65_537)`, the POSIX end conversion fails, and the first 65,536-byte write fails at the unrepresentable offset. Debug arithmetic then panics. With overflow checks disabled, the next offset wraps to 65,525, and the remaining one-byte write can overwrite existing file data there. This is a source-level boundary finding; I did not execute this case. Current block/undo callers use bounded `u32` positions and small allocation increments, so they cannot reach it; it is not a blocker for the node fix.

   Suggested fix: reject an overflowing `offset.checked_add(len)` before zero-filling, or stop when advancing the offset would overflow. Preserve the intended continue-after-write-error behavior for representable ranges.

## Required checks

- **macOS:** The `fstore_t` fields match Core: `F_ALLOCATECONTIG`, `F_PEOFPOSMODE`, offset zero, length `len`, bytes allocated zero. Only a `-1` first result triggers `F_ALLOCATEALL` and a retry; both failures are ignored, followed by `set_len(offset + len)`, whose result is also ignored. For caller-reachable values this is Core's `ftruncate`. Core's `configure.ac` defines `MAC_OSX` in its Darwin-host branch; Rust's macOS `*-apple-darwin` mapping is appropriate, and iOS takes the fallback without requiring `target_vendor`.
- **POSIX:** The descriptor call uses offset zero and length `offset + len`, returning only on success. All six selected OSes expose this symbol in the locked libc 0.2.190. The checked `off_t` conversion avoids narrowing on 32-bit glibc/Android; an unrepresentable end selects positioned zero writes instead. Targets outside the list do not reference the symbol. Some omitted Unix targets also offer POSIX allocation, but retain a functional zero-fill path rather than failing to build.
- **Fallback:** Normal ranges receive exactly `len` zeros from `offset`, in 64 KiB pieces with a short final piece. Failures are ignored and subsequent pieces are attempted. This intentionally removes the old early return; the overflow edge above is the lost defensive behavior. Rust positioned writes avoid changing a shared file cursor.
- **Contract/callers:** `seq.rs:173-197` only allocates when crossing a chunk boundary, and the block/undo callers allocate from their tracked used end before writing new records. They do not treat allocation success as proof of contents, capacity, or durability; subsequent writes/syncs report failures. macOS sets the length exactly, whereas POSIX and the faulty model's `Fallocate` mode preserve a longer file. No current caller depends on retaining live data beyond the newly requested end. The broad trait wording should be read with Core's unused-tail assumption; arbitrary interior allocations are not platform-equivalent. The toy wrapper at `toy/mod.rs:2762` simply delegates; the real model implementation is in `dash-kv/src/faulty/fs.rs:2144`.
- **Tests:** The extension and direct zero-fill tests cover prefix preservation, extension, multiple pieces, a one-byte last piece, and suffix preservation. The reservation test distinguishes the successful POSIX path subject to finding 1. None of these runtime tests pins macOS's flags or retry order; those were checked directly against Core. Native macOS execution and an independent Apple cross-build were not performed here.
- **Mutants:** A1–A6 are consistent with what the tests assert on a filesystem supporting reservation. I agree with A7 only as correctness equivalence for a fresh unused tail already reading as zero. Removing the return is not universally equivalent for arbitrary `FlatFs` inputs: POSIX preserves existing bytes while zero-fill overwrites them, and extra writes change I/O/fault traces and cost. Under Core's intended fresh-tail usage, its survival does not expose a missing live-data correctness assertion, but it does not establish coverage of the early return either.

## Verification

Core references were read from tag `v24.0.0-rc.5`: `src/util/fs_helpers.cpp:238-283`, the POSIX configure probe, the Darwin `MAC_OSX` definition, and `src/flatfile.cpp`.

Built and tested only in the required reflink copy at `/work/scratch/bl555-r1-sol/src`, with its own target directory, incremental compilation disabled, eight jobs, `nice -n 19`, and `TMPDIR=/dev/shm/bl555-r1-sol-tmp` (0700). Unset `RUSTC_WRAPPER` and additionally overrode the machine's Cargo-configured wrapper with `--config 'build.rustc-wrapper=""'` so this review did not use the shared build cache. The pre-build checks showed five-minute load 69.02 and 239 GiB free.

Command: `cargo --config 'build.rustc-wrapper=""' test --locked -p dashd-store --no-default-features --lib flatfile::tests -- --test-threads=8`.

Result: **47 passed, 0 failed, 1 existing ignored**. Both new allocation tests also passed when executing the built binary as root. With the unsupported-allocation shim, the extension test passed and the reservation test failed as described above. Test temporary files were on tmpfs; XFS/ext4 accounting was assessed by code inspection, not an additional filesystem test run. The RAM scratch directory was removed after testing.

Verdict: **APPROVE-WITH-NITS**.

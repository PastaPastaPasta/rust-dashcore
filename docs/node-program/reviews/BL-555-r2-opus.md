> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-555 review r2 (Opus, narrow): my r1 findings only

- **r1 head:** `1754a05e0`. **Fix commit / head:** `d51430547f37dab03ae14b132f0b9c978b0cb7cf` (`node/nd1-bl555`, signed `G`; the worktree is at that head).
- **Delta read:** `git diff 1754a05e0 d51430547`. It touches `dash-kv/src/fs.rs` (contract doc), `dashd-store/src/flatfile/fs.rs` (comment, `zero_fill` overflow guard) and `dashd-store/src/flatfile/tests.rs` (probe in the reservation test, overflow case in the zero-fill test).
- **Core:** `v24.0.0-rc.5:src/util/fs_helpers.cpp:234-283`.
- **No build or test run.** `df -BG /work` showed 188G free, below the brief's 200G floor. Everything below comes from reading the code, plus the owner's evidence (fmt, clippy on Linux and darwin, 47 flatfile tests passing, the shim run). I read the shim (`/work/scratch/nd1-bl555/shim/no-fallocate.c`). It returns `EOPNOTSUPP` from both `posix_fallocate` and `posix_fallocate64`, so the probe and `OsFs` both see the failure, and the reported "skipped" result is what the code should do.

## Findings

- **M1 (contract): CLOSED.**
  - The new doc at `dash-kv/src/fs.rs:32-37` says three things:
    - The file ends `offset + len` bytes long.
    - `offset` is the end of the live data. Bytes from `offset` on are dead and may read as zeros or keep stale contents.
    - A longer file stays longer, except on macOS, where Core sets the size exactly.
  - Each branch matches it:
    - posix: grows only, and leaves `[offset, old size)` alone.
    - `zero_fill`: grows only, and zeroes the range.
    - macOS: `set_len(offset + len)` can shrink the file.
  - The "never contains live data" assumption is Core's own comment (`fs_helpers.cpp:234-236`).
  - I had marked the `SetLen` mode for `FaultyFlatFiles` as optional, and it wasn't added. Since the contract now states the macOS shrink, I accept leaving it out.
- **M2 (filesystem-dependent test): CLOSED.**
  - `tests.rs:1505-1510` runs a control first: `posix_fallocate(probe, 0, 1 MiB)` on a fresh file in the same tempdir. The test returns early if that call fails or `st_blocks * 512 < 1 MiB`. This covers both ways the test could fail on correct code:
    - ZFS-style mode-0 fallocate that succeeds without reserving blocks.
    - `EOPNOTSUPP` (Sol 1), where the port falls back to `zero_fill`, which leaves the hole.
  - The A1 kill still works: the probe calls libc directly, so on tmpfs, XFS and ext4 it passes, and then the main assertion catches `posix_fallocate(fd, offset, …)`.
  - `.blocks()` resolves through the module-level `use std::os::unix::fs::MetadataExt` (`tests.rs:2`). The literal `1 << 20` infers to `off_t`.
- **N1 (MAC_OSX comment): CLOSED.** `fs.rs:225-226` now reads "Core builds its `MAC_OSX` branch for macOS alone, so other Apple OSes get the fallback too." That is the wording I suggested.
- **N2 (OSes with `posix_fallocate` left off the cfg list): NOT CLOSED, accepted by ruling.** It's a nit, and none of those OSes is a claimed target. Same size and zeros; only the reservation differs.
- **N3 (32-bit glibc `off_t` above 2 GiB): NOT CLOSED, accepted by ruling.** blk and rev files can't reach that size.
- **N4 (unreachable overflow cases): PARTLY CLOSED; the rest is accepted by ruling.**
  - The brief says N4 wasn't changed, but the delta does fix its first half. `zero_fill` now returns when `offset.checked_add(len)` is `None` (`fs.rs:246-248`), so `offset += now` can no longer overflow.
    - That matches Core in effect: Core's `FileSeek64` fails at an offset that doesn't fit in `off_t` and returns without writing.
    - The new assertion at `tests.rs:1543-1546` kills the "remove the guard" mutant. In a debug test build the old loop panics on the add. In a release build it would wrap and write one zero byte at 65525, which the before/after comparison catches.
  - The second half is unchanged and unreachable: in the macOS branch, a `try_from(len)` failure returns before the `ftruncate`.

## New defects in the touched lines
None.
- Inside the guard, offsets in `(i64::MAX, u64::MAX - len]` still go to `write_at`. There they fail with `EINVAL` (negative `off64_t`), which is ignored, and the loop ends without overflow. This is harmless and still matches Core's failed seek.
- Wording only: the skip message says "doesn't count posix_fallocate's blocks" even when the probe *failed* (rc != 0). The doc comment above the test covers both cases.

## Verdict
**APPROVE-WITH-NITS.** M1, M2 and N1 are closed, and so is the reachable half of N4. N2, N3 and the macOS half of N4 stay open as nits, accepted by the owner's ruling. The fix adds no new defect.

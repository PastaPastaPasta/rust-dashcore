> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-555 review r1 (Opus): correctness

- **Head:** `1754a05e0a85d826a24378c6a067766fff8ba510` (`node/nd1-bl555`, one commit on `695b8ae3e`). I confirmed the worktree is at that head.
- **Core:** `v24.0.0-rc.5:src/util/fs_helpers.cpp:238-283`, `configure.ac:850-943` (darwin / `MAC_OSX`) and `:1152-1164` (`HAVE_POSIX_FALLOCATE`), `src/flatfile.cpp:56-80`.
- **Bindings:** libc 0.2.190, the version in Cargo.lock.

## What I ran
I made a reflink copy at `/work/scratch/bl555-r1-opus/src`, with its own target dir, no RUSTC_WRAPPER, and `nice -n 19`.
- `cargo test --locked -p dashd-store --lib -- flatfile::` with TMPDIR on tmpfs: 47 passed, 0 failed, 1 ignored (rc=0). All three new tests passed.
- The new tests again with TMPDIR on XFS (`/work`): all passed (rc=0).
- The macOS branch can't run here. I checked it against Core and the libc `apple` bindings by reading them. I did not rerun the Apple cross-build; for that I rely on the owner's zigbuild check and on CI-S2's macOS run 41159588539.

## Check-by-check

### 1. macOS branch: matches Core
- **Fields:** `fst_flags = F_ALLOCATECONTIG`, `fst_posmode = F_PEOFPOSMODE`, `fst_offset = 0`, `fst_length = len`, `fst_bytesalloc = 0`. These are Core's fields and values. libc's `fstore_t` is `{c_uint, c_int, off_t, off_t, off_t}`, and the constants have the matching types (apple/mod.rs:385, 2643-2670).
- **Retry:** on `-1` it sets `F_ALLOCATEALL` and calls again, with the second result ignored. That is Core's retry.
- **Size:** `set_len(offset + len)` is `ftruncate(offset + length)`, ignored as in Core. Rust's `set_len` retries on EINTR and Core doesn't, which doesn't matter.
- **Results:** none is propagated, and nothing Core checks is ignored. Core has no fallback in this branch, and the port has none either (`zero_fill` is cfg'd out on macOS). That is correct.
- **`target_os = "macos"` for `MAC_OSX`:** acceptable (see nit N1).

### 2. posix_fallocate branch: matches Core
- It calls `posix_fallocate(fd, 0, offset + len)` and returns on 0, as Core does. The old code also passed 0.
- **cfg list:** every listed OS has the binding in libc 0.2.190:
  - linux: linux/mod.rs:3504
  - android: android/mod.rs:3614
  - freebsd and dragonfly: freebsdlike/mod.rs:1568
  - illumos and solaris: solarish/mod.rs:2449

  Any other Unix compiles the fallback, so nothing fails to build. Some OSes that have the binding are left off the list (nit N2).
- **`off_t` on 32-bit:** glibc's `off_t` is `i32` unless the libc file-offset-bits cfg is set. `try_from` then fails above 2 GiB and the code falls back to `zero_fill`, where Core (built with large-file support) calls `posix_fallocate64`. blk and rev files stop at 128 MiB plus one chunk, so this can't be reached (nit N3).

### 3. Fallback (`zero_fill`): matches Core
- It writes `len` zeros from `offset` in 64 KiB pieces with a short last piece, ignoring each write's result. Core's loop does the same: `fwrite` is "allowed to fail" and the loop keeps going.
- **Lost behaviour:** the old code stopped at the first failed write; the new code keeps going, which is closer to Core. One small difference stays: after a failed write, Core's stream position may not move forward, while the port always moves `offset` forward. Since the function is advisory, this doesn't matter.
- **Overflow:** the old code saturated `end`. The new `offset += now` would overflow if `offset + len` exceeded `u64::MAX`, but callers pass a `u32` position (nit N4).

### 4. Contract and callers
`seq.rs:173-197` is a faithful port of `FlatFileSeq::Allocate`: offset `pos.pos`, length `new_chunks*chunk - pos.pos`. Its callers:
- `blocks.rs:497` runs only when `!known`, which is Core's `!fKnown`. There `pos = info.size`.
- `blocks.rs:574` uses `pos = info.undo_size`.

So the range from `offset` on never holds live data, as Core's comment requires. The only read that depends on file length is `blocks.rs:910-916`, and it reads records below `info.size`. No caller depends on the length or the contents past `offset` after `allocate`, so the differences between branches are harmless. The contract text is less precise than the branches now are (minor M1).

### 5. Tests
- `allocate_extends_the_file_past_its_data` pins size and contents on every branch, including macOS when run natively.
- `zero_fill_writes_zeros_from_the_offset` pins the start offset, the 64 KiB pieces, the short last piece, and growth past the end.
- `allocate_reserves_the_file_from_its_start` kills A1 (offset passed instead of 0). It passes on tmpfs and XFS and should pass on ext4 and btrfs. It depends on the filesystem (minor M2).
- Running as root doesn't matter: no test relies on permissions.
- **Mutant A7 (no `return` after `posix_fallocate` succeeds):** I agree it is equivalent *under the contract*, not strictly. With A7, Linux also zero-writes `[offset, offset+len)`. That overwrites stale bytes in `[offset, old size)` that Core's posix branch leaves alone, and doubles the write I/O per chunk. The final size is the same, and the overwritten bytes are never live (check 4). A test that pre-filled `[offset, old size)` with non-zero bytes and asserted they survive on Linux would kill A7. But that would pin bytes the contract treats as dead, so leaving A7 unkilled is reasonable.

### 6. Anything else
- `zero_fill` is `pub(super)`, and the test reaches it through `super::fs::zero_fill`, which is visible from `flatfile::tests`.
- There is no dead code on any cfg: `zero_fill` is used by both non-macOS branches and absent on macOS.
- Moving `AsRawFd` into each branch leaves no unused import on the fallback-only targets.
- `MetadataExt` in tests.rs is still used at :340 on every Unix.
- The ratchet's cfg evaluator handles `target_os` with `any`/`not` (asserted_log_lines.rs:303, :1184).

## Findings

**M1 (minor): the `FlatFs::allocate` contract no longer describes every branch.**
- **Where:** `dash-kv/src/fs.rs:32-35`. The doc says "make the file at least `offset + len` bytes long, zero-filled".
- **What the branches do:**
  - macOS (`fs.rs:197`) sets the size to exactly `offset + len`. When the file is already longer (for example, after a crash left preallocated or unindexed bytes past a stale `info.size`), it shrinks the file, as Core's `ftruncate` does.
  - The posix branch never zeroes bytes in `[offset, old size)`. Only `zero_fill` zeroes the whole range.
- **Model gap:** `FaultyFlatFiles` (`dash-kv/src/faulty/fs.rs:2144-2198`) models `Fallocate` (grow only, keep bytes) and `ZeroFill`, but has no "set exact size" mode. Crash runs therefore never exercise the macOS shrink.
- **Impact:** no current caller is affected (check 4). But a future caller, or a model-based proof, that trusts "at least" or "zero-filled" would be wrong on macOS.
- **Fix:** reword the doc, e.g. "afterwards the file is `offset + len` bytes long, or longer except on macOS (Core's macOS branch sets the size exactly); bytes from `offset` on are dead and may keep stale contents." Optionally add a `SetLen` mode to `AllocateMode`, or a backlog row for one.

**M2 (minor): `allocate_reserves_the_file_from_its_start` can fail on correct code, depending on the filesystem.**
- **Where:** `dashd-store/src/flatfile/tests.rs:1495-1511`.
- **Scenario:** it asserts `st_blocks * 512 >= 9 MiB` after `posix_fallocate`. That holds on tmpfs, XFS, ext4 and btrfs. As far as I know, OpenZFS on Linux accepts mode-0 `fallocate` without reserving blocks, so `st_blocks` stays near zero and the test fails. Any filesystem that compresses glibc's emulated zero writes would fail the same way. A developer with TMPDIR on ZFS (Ubuntu ZFS root, Proxmox) would see a red test with nothing wrong in the code.
- **Fix:** add a control at the start. Call `posix_fallocate(0, 1 MiB)` on a fresh scratch file in the same tempdir, and return early (print "skipped: fs doesn't account fallocate") if its `st_blocks` doesn't grow. That keeps the A1 kill everywhere it can be measured.

**N1 (nit): the comment "Core's `MAC_OSX` is macOS only" is imprecise.**
- **Where:** `fs.rs:226`.
- `configure.ac:850-943` sets `-DMAC_OSX` for any `*darwin*` host. Core only builds and ships macOS. iOS isn't a Core host, and autotools sometimes spells iOS triples `*-apple-darwin*`, so routing iOS to `zero_fill` is a choice, not a strict reading.
- The choice is safe: correct size and zeros, and the file never shrinks.
- **Suggested wording:** "Core only builds `MAC_OSX` for macOS; other Apple OSes get the fallback."

**N2 (nit): some OSes with `posix_fallocate` aren't in the cfg list.**
- fuchsia, aix, cygwin, emscripten, hurd, haiku and nto have it in libc but get `zero_fill`. Core with `HAVE_POSIX_FALLOCATE` would call `posix_fallocate` there.
- The result is the same size and zeros; only the reservation of holes before `offset` differs.
- These are not targets anyone claims, so a comment is enough.

**N3 (nit): 32-bit glibc falls back to `zero_fill` above 2 GiB.**
- `off_t` is `i32` there, so ends above 2 GiB go to `zero_fill`, where Core calls `posix_fallocate64`.
- blk and rev files can't reach that size. Calling `libc::posix_fallocate64` on `linux`/`gnu` would close the gap; not needed.

**N4 (nit): two cases that can't be reached.**
- `zero_fill`'s `offset += now` can overflow (a panic in debug builds) when `offset + len > u64::MAX`.
- In the macOS branch, a `try_from(len)` failure returns before the `ftruncate`, which Core would still run.
- Both need lengths far above `u32`, so neither can happen.

## Verdict
**APPROVE-WITH-NITS.** No blockers and no majors. The three branches match Core's `AllocateFileRange`: the macOS fields, flags, retry and `ftruncate`; `posix_fallocate(fd, 0, offset + len)`; and the 64 KiB zero loop that ignores failures. The cfg split builds on every Unix, and the tests pass on tmpfs and XFS. M1 (reword the contract) and M2 (make the reservation test skip on filesystems that don't count `fallocate`) are worth folding in. Neither changes runtime behaviour.

# SB14 implementation review, round 1 (3329bc303)

Range: `201f7a04f..3329bc303`. I reviewed the accepted rev 2.3 design against the code in an isolated detached worktree. This is a review only; the probes and mutants described below were removed.

## Findings

1. **Blocker — the public direct header open deletes an existing v2 store's headers.** `dash-spv/src/storage/block_headers.rs:114-126` exposes `PersistentBlockHeaderStorage::open` and `PersistentStorage::open`, both of which call `SpvStore::open` without `StorageMigrator::migrate`. With no v4 marker, `Store::open` chooses `Committed::Empty` (`dash-spv/src/storage/store/mod.rs:399-405`) and its final `Hdr::truncate` removes every v2 segment (`dash-spv/src/storage/store/hdr/mod.rs:302-309`). A temporary regression using `V2Fixture` and the public direct open failed: `block_headers/segment_000000.dat` was present before and absent afterward. Route this public open through migration, or refuse a versioned pre-v4 directory without changing it. `DiskStorageManager::new` does migrate first; the FFI client uses that path (`dash-spv-ffi/src/client.rs:90`).

2. **Major — `persist` can return successfully with the only new slot still Buffered.** The public `PersistentStorage::persist` calls `Store::raise` (`dash-spv/src/storage/block_headers.rs:129-135`). After a lowering closes a run, a fast append starts a dirty run; `raise_locked` takes the SQLite path and `commit_sqlite_slot` uses its caller's `Commit::Buffered` (`dash-spv/src/storage/store/mod.rs:734-766`, `:784-792`). My temporary power-cut probe made A0..A5 durable, lowered to A2, appended B3..B4, called `raise` (the exact method behind `persist`), then cut power: it reopened at A2. Thus `persist` can be the only successful call the public API user makes to persist B3..B4 while its slot is lost. Make `persist` use the durable `sync_wal` path, or explicitly change and document the public persistence contract before shipping. The design allows a Buffered SQLite *raise*; this finding concerns what the existing public `persist` operation promises callers.

3. **Major — the file writer is not fail-stop across all write paths.** `commit_naming` checks the file latch only when it happens to take the header writer lock (`dash-spv/src/storage/store/lower.rs:277-354`); a SQLite-only metadata/subscriber/MN commit against an existing database bypasses it. A temporary probe created the database, set `Hdr.failed`, then successfully committed `put_metadata`; the assertion that it must refuse failed. The fast append similarly enters under the lookup lock without checking `Hdr.failed` or `HdrWriter.poisoned` (`dash-spv/src/storage/store/legacy.rs:59-82`, `:88-100`). `Store::sync_wal` can also run its SQLite barrier before `raise_locked` checks poison (`dash-spv/src/storage/store/mod.rs:815-822`). The cache's choice to evict after a failed sync and never retry is safe only if every later write/barrier is refused. Centralize the latch check at every mutating entry, including no-op and SQLite-only paths, without breaking the lookup lock's concurrency contract. `remove_segments` also returns an `io.list` error without poisoning the writer (`dash-spv/src/storage/store/mod.rs:872-875`).

4. **Major — v2 migration no longer rejects a repeated active header hash.** `scan_headers` records only the run bounds, tip hash and linkage (`dash-spv/src/storage/migrator/v3.rs:248-273`); `write_marker` then blesses those bytes without a uniqueness check (`:275-293`). SB13's previous `build` passed every header through `Store::put_headers`, whose uniqueness check rejected repeats, and kept corrupt v2 data aside. A temporary header-only v2 fixture with record 1's stored hash changed to record 0's migrated successfully and produced no `v2-unmigrated/`. This can reopen a graph with two active ids for one hash, violating the store's uniqueness contract. Validate hashes while scanning the retained run and take the existing corrupt/set-aside path before writing the marker.

5. **Minor — the design's unconditional MN-height claim is false.** `put_mn_message` clamps the height used for marker coverage but writes the original height to `mn_log` (`dash-spv/src/storage/store/legacy.rs:459-473`), and lowering rewinds subscriber tables but leaves `mn_log` intact (`dash-spv/src/storage/store/lower.rs:149-171`). The old-path replay checks the message's block hash against the current active header at that height (`dash-spv/src/storage/masternode.rs:209-229`), so I found no S1 violation from this log. Amend §3's “no MN row above the marker” statement to specify this validated, untrusted log exception, as D29 proposed.

## Crash protocol and requested decisions

| Path and crash point | Durable/recoverable state | Assessment |
| --- | --- | --- |
| Plain append, before any raise | Last marker and any earlier SQLite commits; the unraised file suffix may disappear. Eviction syncs a file but commits no height. | Matches §3's loss bound. |
| File raise after `pwrite`, after segment sync, or after directory sync but before the slot | Previous valid slot wins. New bytes or names are garbage until a slot names them. | S1 holds; the directory-sync mutant below is detected. |
| File raise during slot write or after its sync | A torn target leaves the other file slot; a complete synced target names only synced bytes and directory entries. A newer Buffered SQLite slot cannot make the file-slot survivor stale. | Matches D2 and §3. |
| SQLite raise, including dirty runs | Slot and run rows are one atomic SQLite batch after file sync. Buffered loss restores the previous durable slot; a later file raise first barriers that batch. | Structurally sound, with the public `persist` caveat above. |
| Lowering: pre-sync; each top-down batch; after final Durable batch; file rewrite; activation commit | Pre-sync makes all old parents durable. Each batch's marker is below its own copied records, and the first batch rewinds subscribers. The final batch barriers every earlier batch. Before activation commits, new file bytes are garbage; the old copied branch remains closed under S1. A lost Buffered activation restores that valid lowered prefix. | The tested crash points satisfy S1. The pre-sync mutant below is detected. |
| `clear_all` and migration | The clearing intent survives until clear completion; unlink of `tip` precedes Durable SQLite deletion, and reopen resumes a partial clear. V2 files stay until the v4 marker is synced and version 4 is written, so a cut before the version write redoes migration; v3 is discarded under its explicit rule. | Sequence matches §3, subject to the direct-open and repeated-hash findings. |

**D27:** Both oracle extensions are justified within the tested contract. A Buffered lowering batch is a valid prefix, and an unraised suffix can be lost down to the last acknowledged boundary. `check_recovered` still compares subscriber locators and S1 separately (`dash-spv/src/storage/store/crash.rs:896-1006`). The relaxation does not cover a public `persist` acknowledgment; the probe in finding 2 exposes that contract gap.

**D36:** Yes, a Buffered SQLite slot can be the only slot for headers after a successful public `persist`; finding 2 gives a concrete power-cut schedule. `stop` and the worker call `sync_wal`, which makes a dirty-run SQLite raise Durable (`dash-spv/src/storage/store/mod.rs:815-823`).

**D38:** I found every required sync in the measured durable path. The recorded `head-d10d.json` trace has 27 `pwrite64`, **27 `fdatasync`**, and **2 `fsync`** calls, plus 30 `openat`; the code syncs every snapshot file, the header directory on creation, the storage directory for `block_headers/`, and the `tip` slot (`dash-spv/src/storage/store/mod.rs:665-723`). `FD_CAP=32` holds the 27 segments used by this row (`dash-spv/src/storage/store/hdr/mod.rs:31-35`), avoiding eviction/reopen work. The 58 `fs_syscalls` against the design's 115 is therefore a conservative target or non-sync accounting difference, not evidence of a missing required sync. I did not reconstruct the prototype's full 115-call trace.

**D35:** First-poison retention and evict-without-retry are implemented in the file cache (`dash-spv/src/storage/store/hdr/mod.rs:323-340`, `:395-409`). Raises and lowerings check the latch; the write-path bypasses in finding 3 mean the store as a whole is not fail-stop after a file I/O error.

**D29 / D37:** D29 is acceptable for the old, untrusted MN message log if §3 documents its hash-validation exception. D37 is not acceptable against SB13's migration contract; the duplicate-hash fixture above is accepted instead of set aside.

**D22:** `SqliteEngine::write_barrier` forces a `sync_seq` frame even after reopen or with an empty batch (`dash-kv-sqlite/src/lib.rs:525-550`, `:838-841`). `MemoryEngine` and `FaultyEngine` use the trait's default Durable write/barrier; `LatchedEngine`, `GuardedEngine`, and `ProbeEngine` forward to the inner `write_barrier` (`dash-kv/src/latched.rs:158-160`, `dashd-chainstate/src/store/guarded.rs:350-354`, `dashd-chainstate/src/store/probe.rs:236-239`). I found no wrapper that downgrades it.

**Public API / FFI:** `DiskStorageManager::new` calls migration before open (`dash-spv/src/storage/mod.rs:155-173`), and the FFI client uses it. The direct public header open remains destructive on v2 as in finding 1. The v3-to-v4 discard is deliberate for the unreleased SB13 format and covered by tests.

## Brief item verdicts

1. **Crash consistency — CHANGES REQUIRED:** the core raise/lower/clear/migration ordering passed the tested S1 cuts, but the direct public open deletes v2 headers and `persist` can acknowledge a suffix that a cut loses.
2. **D27, D36, D38 — CHANGES REQUIRED:** D27's two oracle extensions are justified; D36 has the public `persist` gap above; D38 retains every required sync.
3. **D35 poisoning — CHANGES REQUIRED:** the local latch and eviction behavior work, but SQLite-only writes and fast appends bypass the store-wide fail-stop rule.
4. **D29 / D37 — CHANGES REQUIRED:** D29 needs its documented MN-log exception; D37 regresses SB13's repeated-hash rejection.
5. **D22 barrier — ACCEPT:** SQLite forces a frame and all reviewed wrappers preserve the inner barrier.
6. **Public API and compatibility — CHANGES REQUIRED:** the FFI manager migrates v2; the direct public header open deletes v2 segments.

## Validation

- Baseline crash suite: `cargo test -p dash-spv --lib storage::store::crash::tests --all-features` — **21 passed**. Two additional fresh `sweep(500, seed, ...)` runs with seeds `0x5b141991` and `0x5b142003` — **500 images each, clean**.
- Store contract/model suite: `cargo test -p dash-spv --lib storage::store::tests --all-features` — **27 passed**. Migration suite: `cargo test -p dash-spv --lib storage::migrator::v3::tests --all-features` — **14 passed**. SQLite forced-barrier test on clean sources — **passed**.
- Temporary contract probes (removed): direct public v2 open, new-run `persist` power cut, and duplicate v2 hash each failed the expected preservation/rejection assertion. A file-latch probe with an already-created database showed `put_metadata` succeeded after `Hdr.failed` was set.
- Four temporary production mutations, each reverted: omit the header-directory sync → `raises_are_clean_at_every_crash_point` found S1/recovery failures; omit lowering pre-sync → `a_lowering_above_the_committed_tip_is_clean` found S1/recovery failures; disable SQLite's forced `write_barrier` counter → `write_barrier_always_bumps_the_counter` failed; skip `Hdr::note`'s failure latch → `a_failed_append_write_latches` failed.
- The review worktree was restored to clean `3329bc303` before final validation. No reviewed source change was retained.

VERDICT: CHANGES-REQUIRED

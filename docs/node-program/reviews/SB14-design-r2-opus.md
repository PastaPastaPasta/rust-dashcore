# SB14 SPV header layout: Opus-h design review, round 2

**Doc:** `docs/node-program/designs/SB14-spv-header-layout.md` on `node/sb13-spv-sqlite` @ **12888c9a0** (rev 2).
I reviewed `git diff 6ade190c8..12888c9a0` and the whole rev-2 doc.

**Context read:**
- DEC-244 and DEC-247;
- my r1 (`reviews/SB14-design-r1-opus.md`);
- perfgate `README.md` (Counters, Threshold policy, Absolute floors) and `workloads.toml:74-79`;
- the base bench `dash-spv/benches/storage.rs` on `node/integration` 82fda6137, and SB13 HEAD's diff to it;
- v2's `segments.rs` (eviction, sentinels, `start_height`) and `mod.rs` (worker, `stop`) on integration;
- SB13's `migrator/{mod,v3}.rs` (version detection, `remove_v2_leftovers`, set-aside, `migrate` on an empty directory), `io.rs` (`atomic_write`) and `dash-kv-sqlite/src/lib.rs` (`open`, DDL, `synchronous`, `fullfsync`);
- the owner's probe at `/work/scratch/sb13/sb14/hyb`, read in full: `sb14_hyb.rs`, `sb14_io_v2.rs`, `common.rs`, `io.sh`, `res-*.txt`, plus the rev-1 `res-v2-2m4.txt`.

**Reviewer scratch:** `/work/scratch/sb14-design-r2-opus` (0700). I built nothing new; I ran one strace of the owner's `hyb5.bin`.

## Verdict: CHANGES REQUIRED

The hybrid is the right layout, and the perf case holds:
- Every gated counter on every measured row is at or under v2, mostly by a wide margin.
- The one-event `read_syscalls` margin is real but passes perfgate's strict `>` rule.
- **D4 (lazy SQLite) is sound and not bench gaming**, under the conditions in the D4 ruling below.

But §3's crash protocol, the part DEC-247 rests on, has four holes. Each has a concrete schedule that loses a committed header, gives a wrong `by_id`, or leaves SQLite rows above the marker:
- **M1.** A torn file-slot write falls back to an arbitrarily stale slot, and open then truncates committed headers.
- **M2.** No directory fsync. A segment or `tip` file created since the last directory sync can vanish after a power cut, and open then refuses the whole store.
- **M3.** A crash between the post-lowering file raise and the cleanup commit leaves active heights with no id run, or with ids that collide with the moved-off records.
- **M4.** Lowering commits are not required to be Durable. A Buffered lowering that is lost under a durable file raise leaves the old branch's subscriber rows standing.

There is also one product failure:
- **M5.** Every segment file stays open: about 240 fds at mainnet, against iOS's default soft limit of 256.

None of these fixes touches a measured row's counters beyond +2 `fs_syscalls` on `durable` and `persist` (well inside both thresholds). A rev 2.1 that fixes M1–M5 and the minors needs only a narrow re-check of §3, D2, D7 and D11, not a full round.

## D4 ruling (lazy SQLite creation)

**Ruling: sound, Core-irrelevant, and not gaming the bench, if the five conditions below go into the doc.**

**Why it isn't gaming.**
- DEC-247 forbids moving work *out of the measured region*: a checkpoint deferred past the region, for example. D4 moves nothing. In the bench's header-only workload, SQLite is never created at all, either in the region or after it.
- The eager variant's increment (+173 KB written, +205 KB read, +57 reads, +80 fs syscalls, about +20 M instructions: `he-store` against `hl-store`) is a fixed, once-per-datadir cost of SB13's store for **non-header** data. v2 never paid it because v2 had no SQLite.
- None of it scales with headers, and v2's zero-read base is what makes any SQLite open fail a byte counter.
- `instructions_total` would catch a variant that created SQLite after the region; this one does not.

**What it does hide (it must be written down, not argued away).**
- A real mobile first run is anchored, and §2 puts the anchor's context records in SQLite. So in the product, the **first header operation creates SQLite**. The bench rows never cover that one-time cost. That is acceptable, but the doc must say so and quantify it.
- The `open` row likewise covers only a header-only store. A product reopen also opens an existing database, which costs a few reads; that is negligible beside the 263 MB of segment reads, but unmeasured.

**Crash window at creation.**
- I checked `SqliteEngine::open`. The DDL is one `BEGIN IMMEDIATE … COMMIT` of `CREATE TABLE IF NOT EXISTS`, and an existing file "with no record and no data" is accepted. So a crash mid-creation leaves either an empty database, which is recreated, or one with the schema and no `meta` row, which is accepted.
- In both cases the file slot governs, and nothing above it was committed through SQLite. So creation itself adds no window, *provided* the conditions below hold.

**Conditions (required).**
1. **§4/§7 disclosure.** State that anchored product stores create SQLite in their first header operation, give the one-time cost above, and record it in a BL or the SB13 notes. Optional and cheap: set `journal_mode=WAL` before the DDL, so the schema is not written through a rollback journal and then the WAL. That roughly halves the creation bytes.
2. **Absence means `ENOENT` only.** "Reads against an absent database return empty" must hold only when the database file does not exist. If `kv.sqlite` exists but cannot be opened or read (I/O error, `UnknownDatabase`, `SchemaTooNew`), **the open fails**, as SB13 D44 already rules for `version.json`. Otherwise a lowered SQLite slot is skipped, and the stale, higher file slot is trusted: the exact case the brief asks about.
3. **D11's set-aside arm.** SB13's `remove_v2_leftovers` (`migrator/v3.rs:216-231`) *sets aside* every `V2_FOLDERS` entry when `spv/` is absent. Under D4, a header-only v4 store has no `spv/`, so with today's code every reopen moves `block_headers/` to `v2-unmigrated/`. D11 covers only the deletion arm (the probe's 28 `unlinkat`s came from the eager variant). `block_headers` must leave the leftover set on both arms for v4, with a test on a header-only store.
4. **Crash cases.** Add a crash during lazy creation with a live file marker, and a first lowering on a header-only store (which must create SQLite first).
5. **§1 clarity.** State that `store`'s region contains no `fdatasync` and no marker write: v2's `store` made 24 evicted segments durable (`segments.rs:198-205`), while the hybrid makes nothing durable until a raise. This is consistent with DEC-244's framing (store = foreground, durable = the judged total), and `durable` carries the work (27 `fdatasync` against v2's 28 `fsync`). Without the sentence, the `store` row reads as deferral.

## Findings

### M1: MAJOR. A torn file slot falls back to a stale slot, and open truncates committed headers (§3 "Open", D2)

**The design.** D2 overwrites one 40-byte file slot in place. "A torn file slot fails its crc, and the other slot wins." Open then truncates past whichever slot won.

**Schedule A (store with SQLite):**
1. A subscriber commit carries the SQLite slot: seq 5, tip 1,000.
2. Worker ticks raise the file slot to seq 9, tip 50,000. Subscriber commits naming heights up to 50,000 are made; they are allowed, because the marker is ≥ them.
3. The next raise writes file slot seq 10, tip 52,000. The power cut tears it, and its crc fails.
4. Open: the only valid slot is SQLite seq 5, tip 1,000. The tip-hash check at 1,000 passes. **Truncation deletes headers 1,001–50,000, which were committed.** The subscriber rows naming heights up to 50,000 now sit above the marker, breaking §3's own "no SQLite row names a height above the marker" and SB0 (c).

**Schedule B (header-only store, D4):** there is only one slot. A torn raise leaves no valid slot, and the doc doesn't say what open does then. Either answer (an empty store, or refusal and SB13's clear-and-restart) loses every committed header.

**Required (no counter cost).**
- **Two file slots** at separate sector-aligned offsets in `tip`, e.g. 0 and 4,096. Each raise writes the slot that does *not* hold the newest file seq, then one `fdatasync`. A torn write then destroys only the slot being written, and the fallback is the immediately preceding commit.
- Open takes the max valid seq over three slots: file A, file B and SQLite.
- Specify the no-valid-slot case: an empty store, with every segment file deleted, which is legitimate only if nothing was ever committed. That is exactly the A/B guarantee.
- Specify that open reads all slots **before** any truncation, and that seq is one global `u64` across all slots.
- Add `start` and a magic/format byte under the crc (see m1).
- Crash cases: a torn A while B holds the newest file seq, a torn B with SQLite newer, and a torn first-ever raise.

### M2: MAJOR. No directory fsync: a new segment or `tip` file can vanish, and open refuses the store (§3 "Raising the tip", D1)

**Schedule:**
1. A raise creates `segment_000240.dat`, `pwrite`s it, `fdatasync`s it, then writes the (existing) `tip` slot and `fdatasync`s that.
2. `block_headers/` itself is never synced, so the new directory entry is not guaranteed durable (POSIX; APFS; f2fs depends on mode).
3. Power cut. The marker names height 2,400,500, but its segment is gone.
4. Open's tip-hash check fails, and the store "is refused": SB13's recovery clears the store, every committed header with it.

The same applies to the first raise creating `tip`. The probe's traces show no directory fsync anywhere, and v2 had none either, but v2 never had a marker pointing into a file that could be missing.

**Required.**
- At a raise, if any file in `block_headers/` was created since the last raise (a segment or `tip`), `File::sync_all` the directory once, after the segments' `fdatasync` and before the marker. With a kept directory fd, that is +1 `fsync` per such raise.
- Measured rows: `durable` +1 or +2 (112 → ≤ 114, against 245); `persist` +1 or +2 (8 → ≤ 10, against 15); `store` and `warm` +0, since neither raises in-region.
- **Open checks contiguity**, not just the tip hash: every segment file from `start`'s segment to the tip's must exist, and every one below the tip's segment must be full length (10,000 × 112 B). A short or missing middle segment passes the tip-hash check today, and the index silently lacks those heights. Open reads every file anyway, so this costs nothing.

### M3: MAJOR. A crash between the post-lowering raise and the cleanup commit breaks the id runs (§3 "Lowering" steps 2–3, D5, D7)

**The design.**
- §3 step 2 raises the tip (either slot) after writing the new records.
- Step 3, a separate SQLite commit, "drops the off-chain copies … and **extends the runs**".
- D5: "a linear append extends the last run with no write", so open treats the last run as reaching the marker's tip.

**Schedule:**
1. Start with run R = (first id 0, height 0, length 1,000) and tip 999.
2. A reorg at f = 900. Batch 1 copies heights 900–999, with ids 900–999, off-chain, lowers the SQLite slot to 899, and trims R to length 900.
3. Step 2 writes 50 new headers at 900–949, `fdatasync`s, and raises the file slot to 949.
4. Crash before step 3.
5. Open: the marker says tip 949, and the runs say R covers 0–899. Either:
   - (a) the implicit extension of the last run gives heights 900–949 **ids 900–949**, which already belong to the off-chain copies of the old branch. Then `by_id(920)` returns the new branch's header for the old record's id, and subscriber rows keyed by id resolve to the wrong header. Or:
   - (b) uncovered heights are corruption, and the store is cleared.

   Both are wrong. Case (a) also happens **without a crash**: if the first append after a lowering extends the trimmed run implicitly instead of starting a new one.

**Required.**
- Merge steps 2 and 3. After the files' `fdatasync`, **one SQLite commit** raises the SQLite slot, writes the new run(s), and drops the re-activated copies. New runs take fresh ids from `meta.next_id` for new headers, or the copies' own ids for re-activated records.
- No file-slot raise may cover heights above a lowering's floor until that commit is durable.
- The lowering commit closes the trimmed run, e.g. by bumping `meta.next_id` past every id it moved off. Then no later implicit extension can re-issue a moved-off id.
- Add the property test against `mem_store` for a lowering followed by a linear append, plus the crash case already listed ("between the file raise and the off-chain cleanup commit"), asserting id uniqueness and not only "each record in one place".
- Cost: the lowering path only; no bench row.

### M4: MAJOR. Lowering commits must be Durable before any file write into the lowered range (§3, D7)

**The design.** dash-kv has `Commit::Buffered` and `Commit::Durable` (`dash-kv/src/model.rs`; the SQLite writer tracks `buffered`). A Buffered commit is atomic but may be lost on power loss, as a prefix. §3 says each batch is "one SQLite commit" and does not say Durable.

**Schedule:**
1. Start with file slot seq 10, tip 150.
2. A lowering batch commits Buffered: SQLite slot seq 11, tip 100, copies of 101–150, and the subscriber rows rewound above 100.
3. The new branch is written to 101–120, `fdatasync`ed, and the file slot is raised to seq 12, tip 120. This step is durable.
4. Power cut. SQLite loses seq 11, which was never synced.
5. Open picks the file slot (seq 12, tip 120), so the new branch is active.
6. The old branch's 121–150 is in neither the files nor SQLite, so committed headers are lost. **The subscriber rows for the old branch's heights 101–150 were never rewound**: filter and MN state for heights 101–120 now describe the wrong block.

**Required.**
- The last lowering batch before step 2 commits `Commit::Durable`; earlier batches may be Buffered, by prefix.
- Any SQLite commit that carries a slot raise may be Buffered: losing it loses only the raise and the rows in that same commit.
- State both rules in §3 and add the crash case "Buffered lowering lost under a durable file raise".

### M5: MAJOR. Every segment file is kept open, which exceeds iOS's default fd limit at mainnet (§2 "File descriptors", D1)

**The problem.**
- "Segment files stay open". The probe keeps a `HashMap<u32, File>` of every segment.
- At 2.4 M headers that is about 240 fds, against iOS's default soft `RLIMIT_NOFILE` of 256, before SQLite (3 fds), sockets and the app's own files.
- Older Android builds sit at 1,024.
- `EMFILE` on a segment open fails a raise or a read mid-sync.

**Required.**
- A bounded fd cache, e.g. LRU of 32–64, that syncs before closing a file still in the unsynced set.
- At 260 k there are 26 files, so `get` and `revindex` stay at 0 opens in-region and no row changes.
- `close` is not in perfgate's fs set.
- Open's scan closes as it goes.

### m1. "v2's format, byte for byte" is false for partial segments, and the start height has no home (§2, D1, D11)

**v2's files.**
- v2 writes each segment whole: `items.resize(ITEMS_PER_SEGMENT, sentinel)` (`segments.rs:557`).
- It derives `start_height` from the first non-sentinel record (`segments.rs:133-141`).

**The hybrid's files.**
- D1's dirty-range `pwrite` leaves the tail segment short. The probe's `persist` wrote 2,128,040 B, not 2.24 MB.
- An anchored store's first segment gets a zero-filled hole before the anchor, not sentinels.

**The gap.**
- The marker carries no `start`.
- The probe's open sets `start` to the first record at offset 0 of the first file present. For a mid-segment anchor it would index the zero records as headers.

**Required.**
- Put `start` in every marker slot (under the crc). Open treats records outside [start, tip] as absent.
- Say "v2's record format", not "byte for byte".
- For v2→4 (D11), define the marker as v3's `build()` does: the end of the first contiguous run. Sentinel tails and anything past a hole are truncated by open.
- D11 must also say that the v2→3 step's header copy into SQLite (`v3.rs` `build()`, the `HEADERS` loop) is skipped for v4.

### m2. Index details the doc must pin (D3)

**Already correct.**
- The prefix is `hash[0..5]` in **internal byte order** (`key()`). State this and why: PoW zeros sit in the last internal bytes, so a display-order prefix would collapse every mainnet hash into one key.
- Full 32-byte verification on every candidate, bounded by the current tip, is right.
- A colliding hash that is not in the chain fails verification. It then falls to `hdr_hash` only when the off-chain count is above zero. That is correct both ways (I checked `find` and the fallback rule): **no collision can give a wrong `get` (by height) or `revindex` answer**.

**Required.**
- **No candidate cap.** The probe's `find` silently keeps the first 8 candidates (`if n < 8`) and drops the rest, which is a false "not found". The design must verify every candidate.
- **Stale and duplicate entries.**
  - Lowering leaves entries above the new tip, and re-raising the same header at the same height re-inserts it, through `pend` → `dups` → `main`.
  - Both are harmless to correctness, because of verification, but they grow `main` by the reorg depth until reopen, and `dups` is scanned linearly on every lookup.
  - Dedup at merge (`sort_unstable` + `dedup`) and keep `dups` small or keyed. Either drop entries above the tip at lowering, or state the bound.
- **Lock scope (D13).**
  - Lookups run inline on the async task under the store's lock.
  - A raise's `fdatasync`s and any SQLite commit must not run under that lock: snapshot the unsynced set and sync outside it. Otherwise tokio workers block on an fsync.
  - The in-memory tip, runs and off-chain count are published under the lock in the same step as each lowering commit.

### m3. 24-bit height cap: it must be an error, never `assert!` (D3)

The probe's `Index::insert` uses `assert!(h < 1 << 24)`, which panics after the record has already been written into the segment buffer.

**Required.**
- `store_headers` refuses a batch with start + len > 2²⁴ before touching any buffer.
- Open treats a marker tip or `start` ≥ 2²⁴ as corruption.

The cap itself is fine: mainnet reaches 16.7 M heights in about 68 years.

### m4. Mainnet RSS: DEC-247 does not waive it, and the doc compares the wrong numbers (§4 "Mainnet scale", §7)

**The doc's errors.**
- §7 says mainnet RSS is "within DEC-247's waiver". DEC-247 waives SB0 §4.4/§8.2 only, and restates "peak RSS must still be ≤ v2".
- The comparison of hybrid index against v2 index (19.2 MB against 13.6 MiB steady) is the wrong one.

**The right comparison.** The rev-1 probe already measured v2 at 2.4 M (`res-v2-2m4.txt`):
- the store's peak increment is **+34.9 MiB**;
- open's VmHWM is **296 MiB**.

**The hybrid's projection** from its own structures:
- **Sync:** about 22–23 MiB. That is the index's 18.3 MiB, plus a `pend` of about 1.5 MiB, 0.5 MiB of scratch and 2.2 MiB of segment buffers.
- **Open:** about 21 MiB. That is `entries`, with exact capacity, sorted in place.

So "≤ v2" very likely holds with about 12 MiB of margin at sync and a wide one at open.

**Required.**
- Fix §7's sentence and cite v2's measured mainnet peaks.
- Measure the hybrid at N = 2.4 M in the same harness during implementation (not gated; a recorded number).
- If the index ever needs to shrink: a 16-bit bucket table (256 KiB) plus 6-byte entries (24 prefix bits + 24 height bits) keeps the 40-bit collision rate at 14.4 MB.

### m5. `read_syscalls` margin: accept it, but state the real mechanism (§4, §7, D14)

**The mechanism.**
- glibc's `check_may_shrink_heap()` reads `/proc/sys/vm/overcommit_memory` (1 byte) the first time a non-main arena heap shrinks, then caches the answer in a static.
- So it is **at most once per process** and can never exceed +1.
- My strace of `hyb5.bin store` (lazy) shows it **on the first store call**, on a blocking-pool thread, right after the `tip` lookup and before `segment_000000.dat` is created. It is not at the drop, as D14 says.
- The trigger is the first thread-arena trim, wherever it falls. Buffer reuse (D14) is good hygiene, but it doesn't remove the read.

**Why it passes.**
- perfgate's rule is `REGRESSION = shift > max(threshold, …)`: strict. With base 0 (after the shim's 4) and the floor at 1, a deterministic +1 is OK.
- The README's own no-op note ("4 vs 4, one head run with 5") suggests v2 sometimes takes the same read.

**The region can't end before the drop.** The gate uses the base's bench file, which fixes the region.

**Ruling:**
- Acceptable as a stated risk; no mandated fix.
- The doc names the mechanism.
- The implementation's perfgate run must show `read_syscalls` bit-stable at ≤ +1 on `store` and `durable`.
- A fresh open must stay read-free: `ENOENT` on `tip` and `kv.sqlite`, no slot reads.
- If a fix is ever needed, the lever is keeping per-call large owned copies (the `spawn_blocking` input) off the thread arenas, not the drop order.

### m6. D10 must land `store_warm` with a measured hybrid row, plus the workloads entries and the anchor (§1, §4, D10)

**Missing rows and entries.**
- D10 lands `store_durable`, `store_warm` and `open`. Once landed, `warm` is gated, but §4 has no `warm` row.
- §1's row list also omits it. Measure it: linked appends onto a durable 260 k chain. The tip segment must stay resident after `stop`, so the first in-region append reads nothing.
- D10's change must carry the three `workloads.toml` entries and the 15-run anchor (README "Merging the harness change that adds it").
- It must define `open`'s routine: store and `stop` in setup; the region is `DiskStorageManager::new` on the existing directory; the drop comes after the region.

**The SB13 branch.**
- HEAD today carries its own `store_durable`/`store_warm` bench edits, a changed `persist` routine, and the `durable`/`warm` `workloads.toml` lines.
- After D10, the SB13+SB14 PR must show **no** diff to `dash-spv/benches/storage.rs` or those `workloads.toml` lines. Otherwise perfgate flags BENCH-CHANGED.
- The bench comments that cite SB13 D49's overlay need rewording for the v2 landing.

### m7. Small points

- **Apple:** write "`File::sync_data`/`sync_all`" (std uses `F_FULLFSYNC` on Apple; SB0 §9.4), not `fdatasync`. This covers segments, the `tip` slots and the directory (M2).
- **r1 M3's test:** add the correctness test for a linked run whose first header duplicates an off-chain child of the tip, so the `hdr_prev(tip)` probe has coverage. No perf row exercises it.
- **§3 "Drop without `stop`":** add that v2 kept its evicted segments durable after a drop or crash, while the hybrid keeps nothing above the last raise. In the product, the 5 s tick bounds the loss to about one tick of sync. This is fine, but say it.

## Answers to the brief's scope

1. **D4.**
   - **Ruling:** sound, not gaming, with conditions 1–5 above.
   - **Crash window at creation:** none (I checked the DDL and `inspect`).
   - **Reopen:** a header-only store reopens with no database; correct.
   - **A datadir with segments but no SQLite:** correct, *except* that SB13's set-aside arm moves `block_headers/` away today (condition 3).
   - **The brief's "a lowered SQLite slot skipped" case:** it can happen only if a present-but-unreadable database is read as absent (condition 2).
2. **Crash safety.**
   - **Append:** after `pwrite` → lost, uncommitted, correct. After `fdatasync` → the same. After the marker → committed. That holds once M1 (torn slot) and M2 (missing directory entry) are fixed.
   - **Lowering:** each batch is correct (copies, slot and runs are atomic) once it is Durable (M4). The re-raise is wrong between steps 2 and 3 (M3).
   - **Slots disagreeing:** "higher seq wins" is right with one global seq. The torn-slot fallback is wrong (M1).
   - **Lowering only through SQLite:** can't leave a trusted, higher file slot, because the lowering's seq is higher. The two exceptions are a database read as absent (D4 condition 2) and a lowering lost because it was Buffered (M4).
   - **Open:** the tip-hash check is necessary but not sufficient. It needs contiguity (M2) and `start` (m1), and truncation must come after all slots are read (M1).
   - **Index against segments:** always consistent at open, because it is rebuilt from the files at or below the marker. At runtime it is a verified superset (m2).
3. **Index.**
   - **Collisions:** can't give a wrong `get` or `revindex`, because every candidate is fully verified, bounded by the tip, and falls back correctly. That holds with no candidate cap (m2).
   - **The 24-bit cap:** must become an error path (m3).
4. **Perf evidence.**
   - **Probe against perfgate:** the probe matches perfgate's counter sources: `/proc/self/io`, the same three allocator hooks, minor + major faults, VmHWM after `clear_refs 5`, `instructions:u` gated by `--control`, and perfgate's fs set without `/proc` and `/sys`.
   - **Calibration:** `store` is within 0.07 % on instructions and to the byte on writes.
   - **What the lazy variant synthesizes:** the non-SQLite open's fs syscalls and bytes. It does not add that part's instructions, allocations or RSS, so those small costs must show in the real gate.
   - **`read_syscalls` margin:** acceptable (m5).
   - **RSS:** ≤ v2 on every measured row (store 40–42 against 47.4 MiB; durable 41.7 against 49.3; open 39.0 against 79.1). Mainnet is very likely ≤ v2 against v2's measured 2.4 M peaks; record it (m4).
5. **r1 dispositions.**
   - **I agree with:** B1 resolved; M1 applied; M2 N/A (the active path builds no records); M3 applied (but add its test, m7); M4 applied (but M3 above is a new hole in run maintenance); M5.1/M5.2 N/A; M5.3/M5.4 applied (plus M4 above); m1 N/A; m2 and m3 applied; m5 applied (plus m6).
   - **m4 (r5 discard):** applied, but the set-aside arm is new (D4 condition 3).
   - **m6 (v2 migration):** only partly applied. The v3 header copy must be skipped and the v2 tip defined (m1).
   - **The slim-record probe:** N/A, agreed.
6. **Implementability.**
   - **D10 before SB13+SB14 is the right order.** perfgate then has v2 bases for `durable`, `warm` and `open`, and the PR changes no bench file.
   - **D10 must bring** the workloads entries, the anchor, `open`'s definition, and a measured `warm` row (m6).
   - **D11** needs the set-aside fix and the v3 header-copy exclusion before implementation.

## What I checked

- **Diff and doc:** the whole rev 2, and the r1 → r2 diff (542 lines).
- **Probe source:**
  - `find`'s candidate cap and its verification bounded by the tip;
  - `key()`'s byte order;
  - `Index::merge` in place from the back;
  - `write_back`'s dirty range;
  - `flush`'s order: `pwrite`, then `sync_data` per unsynced file, then the marker write and `sync_data`. There is no directory sync, and a single unversioned slot with no crc;
  - open's `start`, from the first record of the first file;
  - `assert!` on the height;
  - every file kept open.
- **Probe results:** `res-hyb.txt` and `res-lk.txt` against §4's table. The numbers match after the stated noop and SB13-open adjustments. Lazy `store` is 5 read syscalls against the noop's 4.
- **One reviewer run:** `strace -f -e openat,read,pread64,statx` over `hyb5.bin store` with `SQL=lazy`, in my scratch directory. The only in-region non-`/proc/self` read is `openat("/proc/sys/vm/overcommit_memory")` plus `read(…, "0", 1)` on a blocking-pool thread, before `segment_000000.dat` is created.
- **perfgate:** the strict `>` in both rounds; the absolute floors of 1 event and 512 B per run; peak RSS at 2 % with a 4 MiB floor for the tokio storage benches; `workloads.toml:74-79` ungates only `syscalls`/`write_syscalls`; `durable` and `warm` are on the SB13 branch only.
- **Base bench on integration:** `store`'s setup returns an un-awaited future, so the open and `stop` are in-region, the `TempDir` is gone before the open, and the drop is in-region. `persist` uses `block_headers().write().persist(dir)`. There is no `durable`, `warm` or `open`.
- **v2:** eviction persists the segment (`segments.rs:198-205`); sentinel padding and `start_height`; the 5 s worker; `stop` = abort + persist; no `Drop`.
- **SB13:**
  - `migration_loop`: an absent version → 0, then every migrator runs;
  - v3 `migrate` on an empty directory returns before creating SQLite;
  - `remove_v2_leftovers` sets aside when `spv/` is missing;
  - `atomic_write` syncs the directory;
  - `SqliteEngine::open`: one idempotent DDL transaction; an existing file with no record and no data is accepted; `synchronous=FULL`; `fullfsync` on Apple; Buffered/Durable commits.
- **Mainnet RSS:** v2's measured 2.4 M peaks (`/work/scratch/sb13/sb14/res-v2-2m4.txt`: store +35,712 KiB over `gen`; open 303,364 KiB), against the hybrid's structures sized at 2.4 M.

Scratch: the strace log and temp directory under `/work/scratch/sb14-design-r2-opus` are deleted. No builds were made.

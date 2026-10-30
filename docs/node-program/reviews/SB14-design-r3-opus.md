# SB14 SPV header layout: Opus-h design review, round 3 (narrow, crash protocol)

**Doc:** `docs/node-program/designs/SB14-spv-header-layout.md` on `node/sb13-spv-sqlite` @ **0c04b255e** (rev 2.1).
I reviewed `git diff 12888c9a0..0c04b255e` and the whole rev 2.1 doc, concentrating on §3, D2, D5, D7, D11 and §5.

**Context read:**
- my r2 (`reviews/SB14-design-r2-opus.md`);
- the rev 2.1 probe `/work/scratch/sb13/sb14/hyb/sb14_hyb.rs`: `open`, `cache`, `file`, `write_back`, `find`, `store`, `flush`, and the slot codec;
- `res-21.txt` and `res-warm.txt`, in full;
- SB13 @ 0c04b255e:
  - `migrator/mod.rs`: `migrate`, `migration_loop`'s per-step version write, and the clear-and-restart arm;
  - `migrator/v3.rs`: `migrate`, `set_aside`, `remove_v2_leftovers`, `sync_dir`;
  - `dash-kv-sqlite/src/lib.rs`: the module doc on Buffered/Durable, `SqliteEngine::open` and its `create_dir_all`, and the barrier rule at `:530`;
  - `storage/mod.rs`: `DiskStorageManager::new`'s order.

**Reviewer scratch:** none created. I built and ran nothing; this round is reading and schedule construction.

## Verdict: CHANGES REQUIRED

Rev 2.1 closes r2's M1–M5 and every minor as written. Specifically:
- the alternating file slots;
- one global `seq`;
- every slot read before truncation;
- the directory syncs;
- the contiguity check;
- the single post-lowering commit;
- the Durable last batch;
- the fd cache.

The perf case holds: I spot-checked `res-21.txt`, and every row matches §4.

But re-enumerating the crash points turns up six new holes, N1–N6:
- **Four have a concrete schedule** that either refuses a store holding committed headers (SB13's clear-and-restart then loses all of them) or trusts a mixed chain without noticing.
- **One is a D11 sequencing gap** against SB13's real `migration_loop`.
- **One is a spec gap in D5** that contradicts D4.

All six are doc rules. Only N3 touches a measured row: +1 `fdatasync` on `open` (32 → 33 fs syscalls, against v2's 81). The other five cost nothing on any row.

A rev 2.2 that fixes N1–N6 needs only a re-check of those rules, not a round.

N4 is partly mine: r2 m2 asked for syncs outside the lock but didn't say what serializes the writers.

## Findings

### N1: MAJOR. A lowering above the durable tip writes a SQLite slot over unsynced heights (§3 Lowering step 1, D7)

**The design.** Each batch "lowers the SQLite slot below the batch (a new `seq`)". Nothing bounds the new slot by the **durable** tip. In steady-state sync, the in-memory tip routinely runs up to 5 s ahead of the last raise, and a one-block reorg of an unraised tip is the common case.

**Schedule:**
1. The newest slot is file seq 9, tip 1,000 (durable).
2. Appends 1,001–1,100 are written to the tip segment's page cache. No raise yet.
3. A reorg at f = 1,050. The batch commit (the last batch, so Durable) copies 1,050–1,100 off-chain and writes **SQLite slot seq 10, tip 1,049**, with the tip hash of record 1,049.
4. Power cut before the next raise. The segment's writes for 1,001–1,049 were never `sync_data`ed.
5. Open: SQLite seq 10 wins. Either the segment is short at 1,049 ("short segment"), or the record at 1,049 is zeros or stale, and the tip hash fails.
6. The store is refused, and clear-and-restart drops all of 0–1,000, which was committed.

It is the same hole as r2 M2, through the SQLite slot. §3's "Writes that name headers" rule (slot only "after steps 2–3") covers subscriber commits, not the lowering's own slot.

**Required.**
- A lowering's slot is **min(f − 1, durable tip)**. Heights in (durable tip, f − 1] stay pending in memory. Their records are valid, and the step-3 commit or the next raise commits them.
- Lowering step 2 then means raise steps 1–3 over **everything** unsynced, not only "the new records". Step 3's slot covers (durable tip, new tip].
- Add the crash case: "a reorg above the durable tip, cut after the Durable batch".

The alternative is to run raise steps 1–3 before the first batch, which costs a sync on every shallow reorg. The min rule is free.

### N2: MAJOR. `spv/`'s directory entry is never made durable, so a lowered SQLite slot can vanish and a stale file slot is trusted (D4, §3 Open step 1, §5 condition 2)

**The code.**
- `SqliteEngine::open` creates its directory with `std::fs::create_dir_all(dir)` (`dash-kv-sqlite/src/lib.rs:269`) and never syncs the parent.
- SQLite itself directory-syncs only the directory holding the WAL (`spv/`), on the WAL's first sync.
- In SB13, `spv/` got a durable entry from `v3::migrate`'s rename plus `sync_dir(storage_path)`, and a fresh store kept nothing outside it.
- Under D4, `spv/` is created lazily, at the first lowering or the first subscriber write, long after the storage directory's last sync (`version.json`'s `atomic_write`). And the file slots now live **outside** `spv/`.

**Schedule (header-only store):**
1. File slot A: seq 40, tip 300,000.
2. A 2-deep reorg at f = 299,999 creates `spv/`, `kv.sqlite` and the schema. The last batch is Durable (WAL `fsync` plus SQLite's dirsync of `spv/`): SQLite slot seq 41, tip 299,998, with copies of 299,999 and 300,000.
3. Step 2 writes the new branch at 299,999 (one record) and `sync_data`s it.
4. Power cut. The storage directory's entry for `spv/` was never synced, so `spv/` is gone.
5. Open: `kv.sqlite` is `ENOENT`, which reads as "no database" (correctly, by condition 2). File slot A (seq 40, tip 300,000) wins.
6. The record at 300,000 is still the old branch's, so **the tip hash matches**. Open trusts [.., 299,999 new | 300,000 old].
7. With no database there are no run rows, so nothing flags it (see N6). D12 derives "unlinked" and silently runs the store as a legacy one.

If the new branch reaches 300,000, the tip hash fails instead: the store is refused, and all committed headers are lost.

**Required.**
- When D4 creates the database, `sync_all` the storage directory after `spv/` exists and before the first commit that carries a slot. That is one sync per datadir, on the lowering or subscriber path only, never in a header row.
- State that `kv.sqlite`'s own entry is made durable by SQLite's WAL dirsync (`unixOpen`'s `syncDir` for `SQLITE_OPEN_WAL`). That property is what makes "`ENOENT` means never committed" true.
- **`FaultyFiles`** must model lost directory entries for `spv/` and `kv.sqlite`, not only inside `block_headers/`. Add the case "`spv/` lost after a Durable first lowering".
- See also n3 (a persisted `linked` bit), which turns step 7's silent case into a refusal.

### N3: MAJOR. Open acts on a slot that is visible but not durable: a process crash, then a power cut (§3 Open)

**The gap.** A process kill (iOS jetsam, an Android low-memory kill, a panic) keeps the page cache. `dash-kv-sqlite`'s own doc says a Buffered commit "survives a process kill" (`lib.rs:10`). So after a kill, the reopen sees:
- Buffered SQLite slots and their rows, which are not durable;
- a file slot written but not yet `sync_data`ed.

Open trusts these and acts on them: it truncates, deletes segments, and lets new commits name heights. A later power cut then reverts the slot, but not those actions.

**Schedule A (lost committed headers):**
1. File slot A: seq 9, tip 50,000, durable.
2. A deep truncation lowers the tip to 43,855 in three batches: seq 10–12, all Buffered, with the last one Durable still to come.
3. Process kill.
4. Reopen: SQLite seq 12 (in the page cache) wins. Open deletes `segment_000005.dat` and cuts segment 4 at 43,855.
5. ext4 commits the unlink and the truncate in its 5 s journal tick, while the WAL frames for seq 10–12 are still unwritten.
6. Power cut.
7. Next open: file slot A (seq 9, tip 50,000) wins. Segment 5 is missing, so the store is refused, and 0–50,000 (committed) is lost.

**Schedule B (SB0 (c)):**
1. A raise writes file slot B (seq 10, tip 52,000) and is killed before its `sync_data`.
2. Reopen: B is valid in the page cache.
3. A Durable subscriber commit writes filter rows for 50,001–52,000.
4. Power cut. B never reached the disk.
5. Open: A (tip 50,000) wins. The SQLite rows now name heights above the marker.

**Required.** Open makes the winning slot durable **before** step 5's truncation and before any commit:
- if a file slot wins, `sync_data` on `tip`;
- if the SQLite slot wins, a real WAL sync.

Note that `sync_wal` right after an open is a no-op today: `barrier = sync == Durable && self.buffered` (`lib.rs:530`), and `buffered` starts false. So open must issue a Durable write (bump the barrier counter), or start with `buffered = true`.

**Cost:**
- the `open` row is +1 `fdatasync`, 32 → 33 fs syscalls against 81;
- `store` and `durable` open fresh directories with no slot, so they are +0.

**Crash harness:** add "kill, reopen, act, power cut" schedules. `FaultyFiles` and `FaultyEngine` must be able to keep unsynced state across a simulated kill and drop it at a later cut.

### N4: MAJOR. Once the syncs leave the lock, nothing serializes a raise against a lowering (§2 Lock scope, §3)

**The design.**
- A raise "snapshots the unsynced set and its fds under the lock, then runs every `sync_data`, the directory sync and any SQLite commit **outside** it".
- The doc doesn't say when the raise allocates its `seq` and captures (tip, tip hash).
- It doesn't say whether a lowering may run inside that window.

**Schedule:**
1. The durable tip is 900 (seq 9). The worker's raise R snapshots tip 1,000, the old branch's hash, and {seg 0}. It releases the lock and starts `sync_data(seg 0)`, about 200 ms on a busy disk.
2. Inside that window, `store_headers` makes a 111-deep reorg at f = 890. The lowering batch commits (Durable): seq 10, SQLite slot 889, copies 890–1,000.
3. Step 2 writes the new branch at 890–950 into seg 0 and syncs it. The step-3 commit is seq 11, slot 950.
4. R finishes. It allocates seq 12 and writes file slot B: tip 1,000, the **old** branch's tip hash.
5. Crash.
6. Open: seq 12 wins, and the tip hash at 1,000 matches (951–1,000 were never overwritten). The runs end at 950, so heights 951–1,000 have no run. That is corruption: the store is refused, and all committed headers are lost.

Deep lowerings are not exotic: `truncate` is an API call, and testnet and regtest reorgs run deeper.

The same race exists for a subscriber commit that carries the SQLite slot from a stale snapshot.

**Required.**
- **One writer mutex** across a raise (snapshot through marker) and a lowering (first batch through the step-3 commit). The lookup lock stays separate, so D13's inline reads still never wait on an fsync.
- The slot a raise or subscriber commit writes is the (tip, hash) **captured with the synced snapshot**, never the live in-memory tip.
- Add "a lowering racing a raise" beside "a subscriber commit racing a raise".

A cheaper alternative is to allocate the raise's `seq` under the lock at snapshot time, so any later lowering out-ranks it, and to drop R's in-memory `durable_tip` update when a lowering intervened. That is subtler to get right. The mutex is the rule I'd write.

### N5: MAJOR. D11 against SB13's `migration_loop`: the v3 discard runs over the fresh v2 → 4 result (D11)

**The code.** `migration_loop` (`migrator/mod.rs:151-166`) runs `MIGRATORS[version]` and then `atomic_write`s `version + 1` after **every** step.

**What D11 says.** "v2 → 4" is "SB13's v2→v3 steps … with three changes", and "v3 → 4 … a v3 store is discarded".

**What a literal implementation does.** With `MIGRATORS = [V1, V2, V3', V4]`, a v2 store runs V3' and gets:
- the marker;
- `spv/` with the filters, filter headers and metadata;
- `version.json = 3`.

Then V4 discards it: it deletes `spv/kv.sqlite*`, or, by "anything else is refused", fails the open on `block_headers/`.
- A crash after the version-3 write does the same at the next open.
- The user either loses the migrated filters (the expensive part of v2) or gets a store that won't open.

**Required.**
- In the v4 binary, version 2's migrator goes **straight to 4**: give `Migrator` a target version, or special-case it.
- Write `version.json = 4` only after the marker and the build are durable. Then "no valid slot → delete every segment" (§3 Open step 2) can never run on a v2 directory mid-conversion, which answers the brief's question.
- The discard runs only on a `version.json` read as 3.
- Scope "anything else is refused" to `spv/`'s contents. At the storage path, `logs/`, `peers/`, the lock file and `v2-unmigrated/` are expected.
- Add the crash cases "v2 → 4 cut before the marker" (it must redo, and nothing is deleted) and "cut after the marker, before the version write".

### N6: MAJOR (spec). No base id run for a store with no `hdr_runs` rows (D5, D4, D11)

**The gap.**
- D5 says the runs live in `hdr_runs`.
- §3 Raise step 5 says a file slot may cover a height only once its run is durable in SQLite, and "the first raise over a new run" goes through SQLite.
- Open step 5 says "a height with no run is corruption".
- Nothing defines the run of a store that has no rows:
  - a header-only store (D4), which has no database at all;
  - a v2 → 4 store, whose `build()` header loop is skipped (D11), so nothing writes a run.

**Both readings fail.**
- **Read literally**, the bench's first raise must create SQLite, which breaks D4 and every `store`/`durable` counter in §4 (the probe has no runs, so it never hit this).
- **Read the other way**, every migrated store is refused at open.

**Required.**
- Define the **base run**: when there is no database, or `hdr_runs` is empty, [start, tip] is one open run with first id 0 at `start`, and `next_id` is derived from it.
- The commit that creates the database, or first writes `hdr_runs`, materializes the base run (trimmed and closed if it is a lowering) before anything else issues an id.
- D11's migrator writes the base run explicitly, or relies on the empty-table rule, which is cheaper.
- Anchored stores, whose context records take ids first, write their first run row in that same first commit (§5 condition 1 already creates SQLite there).

### n1. The file slots are "separate sectors" but share a page on iOS and on 16 KiB-page Android (D2)

**The problem.**
- iOS uses 16 KiB VM pages, and Android 15+ ships 16 KiB-page kernels, where f2fs can use 16 KiB blocks.
- There, offsets 0 and 4,096 sit in one page, and possibly one filesystem block. Cleaning slot A's dirty page rewrites slot B's bytes too.
- Identical bytes rewritten are safe on a sector-atomic device. But on a device that garbles in-flight sectors, a tear destroys **both** slots: the "torn first-ever raise" outcome, an empty store, on a store with committed headers.

**Fix (free):** put B at **65,536** (the largest page and block size on any target). The file is sparse, so no bytes are written. Or state the sector-atomic assumption in D2.

### n2. Pin the `seq` rules the implementation can get wrong (D2)

- At open, the in-memory `seq` is the max over **all three** valid slots. The probe takes it from the file slots only ("the SQLite slot joins this max").
- "The newest file `seq`" is computed among the file slots alone, even when SQLite wins. The doc's rule is right. Writing over the newest file slot because SQLite's is newer overall is wrong when that SQLite slot is Buffered and then lost. Add it as a crash case: "torn file raise, with a Buffered SQLite slot newest and lost".
- **A tie** between valid slots with different contents is corruption. No correct schedule produces one, so say it.

### n3. D12's derived linkage turns detectable corruption into "legacy unlinked" (D12, §3 Open)

**The problem.**
- N2 step 7, and any future bug that leaves a mixed chain under a valid slot, opens silently as an unlinked store.
- Only legacy test and bench writes are supposed to make one.

**Fix:**
- Carry a `linked` flag in every slot: the format byte, or a flags byte under the crc.
- A store whose slot says `linked` and whose records break linkage inside [start, tip] is corruption.
- Open already checks every `prev`, so this costs nothing.

### n4. Sync failures must poison the writer (§3, D1, M5)

**The problem.** On Linux, a failed `fdatasync` can clear the page's dirty state, so a retry "succeeds" over lost data ("fsyncgate").
- The probe's eviction returns the error, but keeps the id in `unsynced`. The next raise's retry then passes, and the marker is written over a hole.
- The contiguity check sees lengths, not contents.

**Fix:** any `sync_data` or `sync_all` error on a segment, `tip` or a directory makes the store refuse further raises until it is reopened. Never retry a failed sync.

### n5. The fd cache against the out-of-lock sync (D1, §2)

The brief asks whether eviction can reorder durability. It can't make anything durable *later*: eviction syncs before it closes and removes the id from `unsynced` only after. Early durability above the marker is harmless.

**Two rules to write down:**
- **The raise's snapshot holds owned handles** (`Arc<File>` or `try_clone`).
  - The snapshot takes ids out of `unsynced`. An eviction during the out-of-lock sync then sees the file as synced and closes it without a sync.
  - That is safe only if the raise's own handle keeps the inode's fd alive. `fdatasync` flushes the inode's pages through any fd.
  - With raw fd numbers, the close can recycle the number under the raise.
- **Eviction's `sync_data` runs under the lock**, on the append path.
  - It is rare: more than 32 segments touched between raises means more than 320 k headers in one tick.
  - Say it's accepted, or defer the eviction to the raise.

### n6. Narrow the "no SQLite row names a height above the marker" invariant (§3, D5)

**The conflict.**
- D5 (2) closes a run "at the in-memory tip", which can be above the marker.
- Lowering copies can be of unraised records (N1).

Both are correct, because open trims runs and off-chain records are height-free facts. But they break the invariant as worded.

**Fix:**
- Word the invariant for subscriber, MN, `locks` and sync-state rows.
- Say runs are trimmed at open.
- Say open **never re-opens a closed run**: closed means it has a stored length.
- Say `meta.next_id` survives the trim.

**Why the last two matter.** Otherwise, take a run closed at 1,100 and trimmed to 1,000 after a crash. If open re-opens it, the next appends re-issue ids up to 1,100, colliding with the off-chain record that took id 1,100 in the closing commit.

### n7. Two deletion-order details

- **A no-valid-slot open with a database present** (an anchored store whose first raise tore) should also drop the run rows above "no tip". It keeps `next_id` and the anchor's context records.
- **SB13's clear-and-restart** must remove `block_headers/tip`, or the whole `block_headers/`, **before** `spv/`. Otherwise a kill mid-clear leaves a stale file slot with no database, and the next open trusts it: the N2 outcome.

Also, the probe syncs only `block_headers/`'s parent. When `DiskStorageManager::new`'s `create_dir_all` created the storage directory itself, its parent needs one sync too. Or note that a wholly vanished fresh datadir is an accepted loss.

## Answers to the brief's scope

1. **Crash points, re-enumerated.**
   - **Append and raise:** correct as written (pwrite → `sync_data` → directory → slot), given N3 at open and n1 for 16 KiB pages.
   - **Slot alternation:**
     - A torn newest file slot falls back to the immediately preceding file raise, and a newer SQLite slot still wins. Correct.
     - **Both file slots valid with SQLite newer or older:** highest `seq` is right in both cases. Once a lowering is Durable, a pre-lowering file slot can never win again, because the lowering's `seq` out-ranks it and the step-3 and run barriers keep it so.
     - **A seq tie:** unreachable with one counter (n2).
     - **A torn write spanning both slots:** only on ≥ 8 KiB pages or blocks (n1).
     - **A Buffered slot raise lost under a later durable file raise:** safe. It loses only its own rows, and step 5's barrier stops the file raise from covering a run that existed only in the lost commit.
   - **Lowering batches:** a prefix of them is a valid lowering, and the M4 case is closed. But the slot over unsynced heights (N1) and the race with a raise (N4) are open.
   - **The M3 single commit:** correct. A crash before it gives the lowered tip with the copies in place; after it, the new runs.
   - **Lazy creation:** the DDL window is still none, but `spv/`'s entry is (N2).
   - **v2 conversion:** the in-step order is correct (`sync_data`, directory, marker), but the version sequencing isn't (N5).
2. **Open-time trust.**
   - Reading every slot first, the contiguity and length check, and the tip-hash check together close r2 M1/M2 for power cuts.
   - **What they miss:**
     - a slot that is visible but not durable after a kill (N3);
     - a lowered SQLite slot made absent by a lost `spv/` entry (N2);
     - a mixed chain whose tip record happens to match, which the run check catches only when runs exist (N4; N6 and n3 make it reliable).
   - **"No valid slot → empty store, delete the segments":**
     - safe for v2 mid-conversion *only* with N5's single 2 → 4 step;
     - safe for a lost `tip` file, because `tip` is created before step 3's directory sync and is never renamed or unlinked;
     - otherwise unsafe only via n1.
3. **The M5 fd cache:** it can't delay durability. It needs owned handles in the snapshot and poison-on-error (n4, n5).
4. **Perf.** I spot-checked `res-21.txt`:
   - `store`: 32 − 5 noop + 56 synthesized = 83. Read syscalls 5 − 4 = 1.
   - `durable`: 64 − 5 + 56 = 115 (27 `fdatasync`, 2 `fsync`). Write bytes 29,120,064 + 61 = 29,120,125, against v2's 29,120,059: +66 B, under the 512 B floor.
   - `persist`: 17 − 5 = 12 against 15. A regression needs > 16 (the floor is 1, strict `>`).
   - `open`: 37 − 5 = 32 against 81.
   - `warm`: 0 read syscalls, 24 opens, 0 syncs. Page faults are noisy on both sides (`res-warm.txt`: hybrid 869–7,110, v2 831–5,620).
   - **One harness nit:** the `store` trace shows 4 `statx` against the noop's 5, so the subtraction may undercount by 1. That is immaterial at 83 against 222.
   - **My fixes:** N3 adds +1 to `open` only; the rest cost nothing on any row.
5. **Leftovers.** Every r2 finding is closed as written:
   - M1–M5 and m1–m7;
   - D4's five conditions, all in §5 (I checked condition 3's both-arms wording against `remove_v2_leftovers`);
   - the probe's `find`, which now verifies every candidate (spilling past 8 to a `Vec`);
   - the height cap, which now returns an error before touching any buffer.

   N4 refines my own r2 m2.

## What I checked

- **Diff and doc:** the full r2 → r2.1 diff (489 lines) and the whole rev 2.1 doc. I read §3 and D2/D5/D7/D11 line by line against each schedule above.
- **Probe:**
  - the slot layout (64-byte slots at 0 and 4,096; magic, format, seq, start, tip, hash, crc32 over 0..56);
  - `open`'s order (scan, read both slots, best `seq`, range check, delete outside, the contiguity/length read, cut the tip segment, the tip hash);
  - `flush`'s order: write-back; `sync_data` on `unsynced`; `tip` created, then the parent directory sync if `block_headers/` was created, then the `block_headers/` sync if anything was created; then the slot not holding the newest file `seq`, then `sync_data`;
  - `cache`'s sync-before-close and LRU;
  - `file`'s `created` tracking;
  - `find` with no cap;
  - `store`'s height-cap error.
  - It has no id runs (N6) and no concurrency (N4).
- **SB13 code:**
  - `migration_loop` writes the version after every step (N5);
  - `v3::migrate` removes `spv/` and `spv.migrating/` first, renames, then `sync_dir(storage_path)`;
  - `set_aside` and `remove_v2_leftovers`;
  - `SqliteEngine::open`'s `create_dir_all` with no parent sync (N2);
  - Buffered = `synchronous=NORMAL` (it survives a kill), Durable = `FULL`, and the barrier only when `buffered` (N3);
  - `DiskStorageManager::new`: `create_dir_all(storage_path)`, the lock, `migrate`, then `SpvStore::open`.
- **Probe results:** `res-21.txt` and `res-warm.txt`, row by row, against §4's table (point 4 above).

No branch edits, no commits, no pushes. I created no scratch files, so there was nothing to delete. `/work/scratch/sb14-design-r3-opus/brief.md` is the manager's and was left in place.

# SB14: the SPV header layout (DEC-244, DEC-247)

**Status:** rev 2.3, for the check of the lowering text (R1).
- Rev 2 (12888c9a0) replaced rev 1's packed-SQLite layout (B) with DEC-247's hybrid.
- Rev 2.1 (0c04b255e) applied Opus-h r2: M1–M5, m1–m7, and D4's five conditions (D4 accepted).
- Rev 2.2 (c61aa6ef2) applied Opus-h r3: N1–N6 and n1–n7, with D16–D20.
- Rev 2.3 applies Opus-h r4 (`reviews/SB14-design-r4-opus.md`, CHANGES REQUIRED on R1 only).
  - R1: §3 Lowering, D7 and the off-chain bullet. Each batch's slot sits below its own batch, and a
    pre-sync leaves nothing unsynced at or below it.
  - Nits a–g: D4, D11, D16, D17, D19, D20, §3 and §4.
- §8 maps every finding to its change.

**Scope:** the SPV header chain's storage in dash-spv only.
- **Waived, for the SPV header chain only:**
  - SB0 §9.5's per-record cold tier for active headers (DEC-244);
  - SB0 §4.4's O(window) open and §8.2's "no whole-chain index" (DEC-247).
- **Unchanged:** the node's B1 block index, and the rest of SB0 §9.5 (subscriber tables, MN state,
  `locks`, `meta`, the one-writer `WriteSet`).
- PLAN §4.12's SB14 row (the header sync on dash-chain's step) builds on this layout.

## 1. The bar

DEC-247 sets it:
- every counter perfgate gates on a `spv-storage-*` row stays within perfgate's threshold of v2;
- startup, get, revindex and peak RSS are ≤ v2;
- no new `ungated` entries;
- no checkpoint deferral or other work moved out of a measured region.

**The rows.**
- `store`, `persist`, `get` and `revindex` have v2 bases today.
- `durable`, `warm` and `open` get theirs when D10 lands them on integration first.

**What gets measured.**
- The gate runs the head's library with **the base's bench file** (perfgate README, "What a change
  does").
- In the base's `store` routine, the open, `stop`, 20 calls of 13,000 headers and the storage's drop
  are all inside the region. Its `TempDir` is gone before the open.

**`store` makes nothing durable; `durable` measures that work (Opus r2, D4 condition 5).**
- The hybrid's `store` region contains no `sync_data` and no marker write. Its headers are durable
  only at a raise (§3), and the base's `store` routine never raises.
- Its one sync is D20's: a one-time `sync_all` of the parent of the storage directory the open just
  created. That makes the directory's entry durable, not any header.
- v2's `store` made its 24 evicted segments durable (`segments.rs:198-205`).
- That work is in the `durable` row (`store` followed by `stop`). There the hybrid makes 27
  `sync_data`s plus 2 directory syncs, against v2's 28 `fsync`s. DEC-244 frames it the same way:
  `store` is the foreground cost, and `durable` is the judged total.

**The thresholds that bind.** Per run, the threshold is max(0.5 % of the base, the noise floor, an
absolute floor). The absolute floor is 1 event for counts and 512 B for bytes. A regression must
exceed the threshold (strict `>`).
- v2's `store` row reads nothing: 0 B in 0 read syscalls, once perfgate's own shim reads are
  subtracted. So `read_syscalls` allows +1 and `read_bytes` allows +512 B.
- `write_bytes` allows about 134 KB on 26.88 MB.
- `persist`'s `fs_syscalls` (15 per iteration), `durable`'s (245) and `open`'s (81) are the call
  counters that the protocol's syncs touch.

## 2. Layout (the hybrid)

| Part | Where | What |
|---|---|---|
| Active chain | `block_headers/segment_NNNNNN.dat` | v2's **record format**: 10,000 record slots a file, each the 80-byte header plus its 32-byte hash, at offset (h mod 10,000) × 112. Records are contiguous from the store's start height to the tip. |
| Committed tip | Three slots (D2): `block_headers/tip` at offsets 0 and 65,536, and SQLite `meta.hdr_marker` | `(seq, start, tip, tip hash, flags, crc)` with a magic and format byte. Open takes the valid slot with the highest `seq`. |
| Hash index | Memory only; rebuilt at open from the stored hashes, with no re-hashing | D3. |
| Everything else | SQLite (SB13's store), created by its first write (D4) | Off-chain records (stale branches, the anchor's context records, records moved off by an overwrite or reorg) in SB13's per-record tables; the id runs (D5); subscribers, MN state, `locks`, `meta`. |

**Files (D1).** The format is v2's record format, but not v2's files byte for byte (Opus r2 m1).
- **v2's files:** each segment is written whole, padded with v2's sentinel record
  (`segments.rs:557`). The start height is the first non-sentinel record.
- **The hybrid's files:** a segment's dirty range is written with one `pwrite`, so the tail segment
  is short (it ends at the tip). An anchored store's first segment has a zero-filled hole below the
  start height. Open reads only [start, tip], taken from the marker. Anything outside is absent, and
  bytes past the tip are truncated.
- **When files are written:** on eviction or at a raise; each dirty file is written once per raise.
- **Resident segments:** two, evicted least-recently-used as in v2. Their two 1.12 MB buffers are
  reused.
- **v2's per-segment work is dropped:** the temporary file, the rename, the per-segment `fsync`, and
  `create_dir_all` on every write. The marker makes all of it unnecessary.
- **File descriptors (Opus r2 M5):** a bounded LRU cache of 32 segment fds, against iOS's default
  soft `RLIMIT_NOFILE` of 256.
  - A file in the unsynced set is `sync_data`ed before its fd is closed.
  - Open's scan keeps only the last 32.
  - At 260 k there are 26 files, so no bench row opens a file in-region for a read (§4).

**Active records are derived (D5).** The fields SB13 kept per record come from the height:
- **Id:** from the id runs (D5).
- **Status and kind:** active means header-valid and regular. A record whose status differs is not
  active.
- **Skip ids:** from the active heights.
- **Chain work and `time_max`:** from bases every 1,024 heights, computed at open from the anchor's
  absolute work (the sum of `GetBlockProof`), and filled in on demand for cold reads.

The resident window of dash-chain's `HeaderChain` holds the full records near the tip, as before.

**HeaderStore** (dash-chain `store.rs:43-86`):

| Method | How |
|---|---|
| `active_at`, `active_from` | The run partition: height → id. |
| `by_id` | The runs, indexed by first id, give the height (an exact floor, because the id ranges are disjoint; D5), and the file gives the record. Otherwise SQLite's `hdr`. |
| `by_hash` | The index candidates, every one verified; otherwise SQLite's `hdr_hash`, only when the off-chain count is above zero. |
| `from_height` | Files, plus `hdr_h`. |
| `walk_down` | Files by height on a linked store, then off-chain parents by hash. On an unlinked store, by hash (D12). |
| `children_from(p)` | `hdr_prev(p)` in SQLite, plus the active record at height(p) + 1 when its `prev` is `p`. On an unlinked store, a file scan (D12). |
| `leaves_from` | `hdr_leaf`, plus the active tip when no off-chain record names it as `prev`. On an unlinked store, a file scan (D12). |
| `max_id` | The next id (D5) − 1. |

**Locks (D16; Opus r2 m2, r3 N4/n5).**
- **The lookup lock** guards the in-memory state: segment buffers, the index, the tip, the runs, the
  off-chain count and the fd cache.
  - Lookups run inline on the async task under it (D13).
  - Appends that do not lower take only this lock. Their `pwrite`s (eviction) are page-cache writes.
- **The writer mutex** serializes everything that writes a slot or lowers the tip. It is held across:
  - a raise, from its snapshot through its slot write;
  - a lowering, from its first batch through the post-lowering commit;
  - any SQLite commit that carries a slot;
  - open and clear.

  A `store_headers` call that would lower takes the writer mutex before it changes anything.
- **A raise's snapshot.** Under both locks, a raise:
  - writes the dirty ranges;
  - takes the unsynced set out as **owned handles** (`Arc<File>`);
  - captures `(start, tip, tip hash, flags)`;
  - captures whether a file was created.

  It then drops the lookup lock and runs every sync outside it, so no lookup waits on an fsync.
- **What the slot names.** The slot it writes carries the **captured** values, never the live tip.
  Appends made meanwhile sit above the captured tip and are simply not covered. Only after the slot
  write does the raise retake the lookup lock and publish the committed tip.
- **Owned handles and the fd cache.** Because the snapshot holds owned handles, a cache eviction
  during the sync can close its own handle without syncing: the raise's handle keeps the inode open,
  and `fdatasync` through any fd flushes the inode's pages.
- **Eviction syncs.** An eviction's `sync_data` on a file still in the unsynced set runs under the
  lookup lock, on the append path. That needs more than 32 segments touched between raises (over
  320 k headers in one 5 s tick), so it is rare and accepted.
- The in-memory tip, runs and off-chain count are published under the lookup lock in the same step
  as the commit that changes them.

**Sync failures poison the writer (D18; r3 n4).**
- What poisons: any error from `pwrite`, `sync_data` or `sync_all` on a segment, `tip` or a
  directory, and any failed slot write.
- What it does: the store refuses every later raise, lowering and slot-carrying commit until it is
  reopened. Lookups keep working.
- No sync is ever retried. On Linux a failed `fdatasync` can clear the dirty state, so a retry
  "succeeds" over lost data.

## 3. Crash protocol

**Durability calls.**
- `sync_data` means `File::sync_data`, and `sync_all` means `File::sync_all`.
- On Apple, std issues `F_FULLFSYNC` for both (SB0 §9.4); elsewhere they are `fdatasync` and
  `fsync`.
- The directory sync is `sync_all` on an fd for the directory, kept open after its first use.

**Directory entries (r2 M2, r3 N2 and n7).** Each directory entry the protocol relies on is made
durable once, before anything depends on it:
- **segment files and `tip`:** step 3 of a raise;
- **`block_headers/` itself:** a storage-directory sync, at the first raise after `block_headers/`
  was created, by an open or by a clear (r4 nit e(ii));
- **the storage directory:** when `DiskStorageManager::new` creates it, it syncs the parent once
  (D20);
- **`spv/` (D4 creation, N2):** after `SqliteEngine::open` returns (it has created `spv/` and committed
  its own DDL with `synchronous=FULL`), and before the first commit that carries a slot or rows, the
  storage directory is `sync_all`ed once (r4 nit c). Losing a DDL-only database is §5 condition 4's
  case. This happens on the lowering or
  subscriber path only, never in a header row;
- **`kv.sqlite` and `kv.sqlite-wal`:** SQLite syncs their directory, `spv/`, on the WAL's first sync
  (`unixOpen` sets `syncDir` for `SQLITE_OPEN_WAL`). Because `spv/`'s own entry was synced first,
  every Durable commit leaves the database reachable.

Together these make "`kv.sqlite` is `ENOENT`" mean that no Durable SQLite commit ever happened. That
is what Open step 1 relies on.

**The slots (D2).**
- **Layout:** two file slots in `tip`, A at offset 0 and **B at offset 65,536** (r3 n1), plus the
  SQLite slot `meta.hdr_marker`.
  - 64 KiB is above every target's page and block size (16 KiB pages on iOS and on Android 15+, and
    16 KiB f2fs blocks), so cleaning one slot's page never rewrites the other's.
  - The file is sparse, so the gap costs no bytes.
- **Contents:** each slot is 64 B: `magic "SB14", format 1, flags, seq: u64, start: u32, tip: u32,
  tip hash, crc32`, with the crc covering everything before it.
  - Flags bit 0 is `linked` (D12).
  - Flags bit 1 is `empty`: no active records; `start` and `tip` are ignored.
- **`seq` rules (r3 n2):**
  - `seq` is one global `u64` across all three slots. At open, the in-memory `seq` is the maximum over
    every **valid** slot, SQLite's included.
  - "The newest file `seq`" is computed over the two file slots alone, even when the SQLite slot is
    newer. A file raise writes the file slot that does **not** hold it, then `sync_data`.
  - Then a torn write destroys only the slot being written, and the surviving file slot is the
    previous file raise. If the newer SQLite slot was Buffered and is lost, that surviving file slot
    is still the newest durable one.
  - Two valid slots with equal `seq` and different contents are corruption. No correct schedule
    produces them.

**Raising the tip (an append, the worker's 5 s tick, `stop`, `persist`).** The writer mutex is held
throughout (D16). Step 1, with the snapshot, runs under the lookup lock; steps 2–4 run outside it.
1. Write the dirty segment ranges (`pwrite`), and take the snapshot.
2. `sync_data` every segment file in the snapshot.
3. If any file in `block_headers/` was created since the last raise, `sync_all` the directory once.
   This covers a new segment, and also `tip` itself, which is created before this step on the first
   raise.
   - Also make the other entries from "Directory entries" durable if they are still pending.
   - A raise that created nothing makes no directory sync.
4. Write the marker, with the snapshot's values:
   - **either** the file slot (above);
   - **or** the SQLite slot, inside a commit SQLite is making anyway: a subscriber `WriteSet`, or the
     post-lowering commit.

   A SQLite commit that carries a slot raise may be Buffered: losing it, as a prefix, loses only that
   raise and the rows in the same commit (r2 M4).
5. **A file-slot raise may cover a height only once that height's id run is durable** (D5).
   - Linear appends extend the open run implicitly and need nothing, and so does the base run (D5).
   - The first raise over a new run goes through the SQLite slot, or is preceded by SQLite's Durable
     barrier (SB13's `sync_wal`).

**The committed tip** is the tip of the newest slot written (Buffered or not), kept in memory. Every
segment byte at or below it was synced before that slot was written.

**Writes that name headers.** Subscriber, MN, `locks` and sync-state rows must never name a height
above the marker when they are written (r3 n6). A commit that writes them either:
- needs the committed tip ≥ that height first; or
- carries the SQLite slot itself, as a raise, after steps 2–3 under the writer mutex.

SB0's (c) is kept this way.

**What may name heights above the marker (r3 n6).**
- **Run rows:** a lowering closes a run at the in-memory tip, which can be above the marker. Open
  trims every run to [start, tip], persists the trim in its repair commit (Open step 4), and **never
  re-opens a closed run**: a run with a stored length stays closed. `meta.next_id` survives the trim.
  So no id that a closing commit issued is issued again.
- **MN log rows (D29, Sol impl r1 #5):** `mn_log` (SB13 D6's old-path messages) is an untrusted
  log, the one exception to the rule above. `put_mn_message` clamps only the height it uses for
  marker coverage and stores each message under its original height, and a lowering does not
  rewind the log. So rows may name heights above the marker, with or without a lowering. They are
  safe because the old-path replay checks each message's block hash against the active header at
  its height and skips a message that does not match (`masternode.rs:209-229`). The exception ends
  when SB15 replaces the old MN path.
- **Off-chain records:** copies of records above the marker are harmless only because **every copy's
  parent is stored** (r4 R1). That parent is either an earlier copy, or an active record at or below
  the slot. This is SB13's crash-oracle invariant S1, a closed header graph (`store/crash.rs:815-823`),
  and the lowering rule below keeps it at every crash point.

**Lowering the tip (an overwrite, a reorg, a truncation; D7).** The writer mutex is held from the
first batch through step 3. On a header-only store, the first lowering creates SQLite first (D4,
with the `spv/` directory sync).

Let f be the first height replaced, T the in-memory tip and C the committed tip.
0. **Pre-sync (r4 R1).** If T > C, run raise steps 1–3 over every unsynced range, writing no slot.
   After this, nothing at or below T is unsynced.
   - **Cost:** a lowering above an unraised tip pays at most one extra `fdatasync`, of the tip
     segment, which step 2 may sync again.
   - No bench row lowers.
1. **The batches.** Walk down from T in batches of ≤ 2,048 records. Each batch is one SQLite commit
   that:
   - copies the batch's records to the off-chain tables, with their ids and derived fields;
   - writes the SQLite slot at **b − 1**, with a new `seq`. Here b is the lowest height **this batch**
     copies, so the slot sits below its own batch (r4 R1). If b − 1 < `start`, the slot is `empty`.
     Every record at or below b − 1 was synced in step 0 or earlier, so the slot never names an
     unsynced height (r3 N1);
   - trims the runs, and closes the open run (D5: `meta.next_id` moves past every id the lowering
     moved off). On a store still on its implicit base run, the first batch writes that run's row
     first (D5);
   - rewinds every subscriber row above f − 1, read under the batch's lease: the first batch does
     the rewind, and a later one repeats it only for a subscriber commit that wrote above f − 1 in
     between (D16 (3)).

   **The last batch commits `Commit::Durable`** (r2 M4). Earlier batches may be Buffered: a Buffered
   loss is a prefix, and every prefix is a valid lowering. After batch k, the active chain is the old
   chain up to b_k − 1, every copied record's parent is an earlier copy or active at or below
   b_k − 1 (S1), and no committed record has been dropped.
2. **The new records.** Write them into the files, then run raise steps 1–3 over them. No file slot
   is written.
3. **One SQLite commit** (r2 M3) then:
   - raises the SQLite slot over the new records, to the new tip;
   - writes the new records' runs: re-activated records keep their copies' ids, and new headers take
     fresh ids from `meta.next_id`;
   - drops the off-chain copies of the re-activated records.

   It may be Buffered (raise step 4). Later file-slot raises over these heights wait for it to be
   durable (raise step 5).

**After a crash at any point:**
- every committed record is in the files at or below the newest valid slot, or in SQLite, and new
  records are in one of those places or nowhere;
- no subscriber, `locks` or sync-state row names a height above the marker (`mn_log` is the
  validated, untrusted exception: D29);
- every height in [start, tip] has exactly one id, and no active id equals an off-chain id;
- every off-chain record's parent is stored: an off-chain record, or active at or below the tip
  (S1, r4 R1);
- memory is O(batch), not O(depth) (r1 M5.3).

**Open.** It holds the writer mutex. It reads every slot before it changes anything (r2 M1), and it
makes the winning slot durable before it acts on it (r3 N3).
1. **Read the slots.** Read file slots A and B, if `tip` exists, and the SQLite slot, if `kv.sqlite`
   exists. Take the valid slot with the highest `seq` (with the `seq` rules above).
   - Only `ENOENT` means a file is absent. A `kv.sqlite` that exists but cannot be opened or read
     fails the open (D4 condition 2; SB13 D44).
   - **No valid slot, or an `empty` winner:** nothing active was committed. A torn first-ever raise
     is the only way to get here with segment files present, and its headers were not committed.
     Skip to step 4 with no active records.
2. **Checks, read-only** (r2 M2, r3 n3). Any failure is corruption, and nothing has been changed
   yet.
   - **Who clears (r4 nit e(iii)):** `SpvStore::open` runs after `migrate`, so `migrate`'s restart arm
     never sees this error.
   - Instead, `DiskStorageManager::new` maps an SB14 corruption error to D19's clear (restart-arm form,
     from step 1) and opens once more. Any other error propagates, and nothing is deleted.
   - **Range:** a slot with `start` > `tip`, or with `tip` ≥ 2²⁴, is corruption (D3).
   - **Contiguity:** every segment file from `start`'s segment to `tip`'s must exist. Each one must be
     long enough for its share of [start, tip]: full length, 1,120,000 B, for every segment below the
     tip's.
   - **Tip hash:** the record at `tip` must carry the slot's tip hash.
   - **Linkage:** if the slot's `linked` flag is set, every record in [start, tip] must link to the
     one below it.
   - **Runs:** with runs present, a height in [start, tip] with no run is corruption.
3. **The rebuild, in memory.** The same pass reads every record in [start, tip] and builds the index,
   the chain-work bases and the linkage state (D12). This is O(chain), as in v2, and waived by
   DEC-247.
4. **Make the winner durable (r3 N3, D17).** After a process kill, the page cache keeps writes that
   were never synced, so the winning slot may be visible but not durable.
   - If a file slot won, `sync_data` `tip`.
   - If the database exists, make one **Durable** SQLite commit. That WAL sync makes the winning
     SQLite slot, and every Buffered frame before it, durable.
     - The commit carries open's repairs: the runs trimmed to [start, tip], with closed runs kept
       closed (D5). With no active records it drops every run row, keeping `meta.next_id` and the
       off-chain records, such as the anchor's context records (r3 n7).
     - **It needs a `dash-kv-sqlite` change (r4 nit d):** a forced barrier on the commit (for example
       `Commit::DurableBarrier`). That barrier always bumps `meta.sync_seq`, so the commit writes a
       frame and syncs the WAL.
       - Today `transaction` bumps the counter only when `buffered` is set (`lib.rs:530`), and `open`
         starts it false (`lib.rs:324`).
       - So a commit that writes no page syncs nothing, and `sync_wal` right after an open is a
         no-op.
   - **SB13's `recover()`** (`handle.rs:56`) runs **after** this step, against SB14's winning slot as
     the active tip, because its rewinds are Durable commits that write locators naming heights.
     Its leading `sync_wal` (`recover.rs:31`) is then redundant. Folding its rewinds into this
     step's commit is an allowed implementation choice.
   - Segment bytes under either slot were synced before that slot was written, so nothing else needs
     syncing.
   - **Costs:** a header-only store (no database) pays the one `sync_data`, and a fresh store (no
     `tip`) pays nothing.
5. **Truncation, last.** Delete segment files above the tip's segment or below `start`'s (all of
   them when there are no active records), and cut the tip segment at the tip. Losing this to a later
   power cut is harmless: the next open redoes it from the same, now durable, slot.

**Clear (D19; r3 n7).** Both clear paths follow this order: SB13's `clear()`, and the
clear-and-restart arm of `StorageMigrator::migrate`.
1. Write `version.json` with `"clearing": true` (`atomic_write`, which syncs). Any open that reads it
   finishes the clear, from step 2, before it does anything else, keeping the same entries
   (`logs/`, `v2-unmigrated/`).
2. Unlink `block_headers/tip` and sync `block_headers/`.
3. Clear SQLite: `clear_all`, Durable, in `clear()`; delete `spv/` in the restart arm.
4. Delete the rest, `block_headers/` included, but **not `version.json`**, which holds the intent
   until step 5 rewrites it (r4 nit e(i)). Otherwise a kill mid-step can leave v4 segments with no
   version, and version 0 would run V2to4 over them.
5. Write `version.json` = 4 without the flag.

The intent costs nothing in the bench rows: `version.json` is read at every open anyway, and a fresh
store has none. Without it, a kill mid-clear leaves either a stale file slot with no database or a
SQLite slot with rows above it, and the next open would trust either.

**Drop without `stop`.**
- It writes nothing, as in v2. The base bench's `store` drop is inside the region.
- What a drop or crash loses:
  - v2 kept its evicted segments durable after either;
  - the hybrid keeps nothing above the last raise (r2 m7).
- In the product, the worker's 5 s tick bounds the loss to about one tick of sync, which header sync
  refetches. `stop`, `persist` and the tick are the clean flushes.
- SB13 D49's write-back overlay, its worker flush and its drop flush are removed (D8).

**Crash tests.**
- **Harness.**
  - `FaultyFiles` holds unsynced writes and unsynced directory entries: inside `block_headers/`, and
    also `spv/`, `kv.sqlite` and the storage directory's own entries (r3 N2). It runs beside SB13's
    `FaultyEngine`, under one cut point.
  - Both model two events: a **process kill**, which keeps unsynced state visible to the reopen, and
    a **power cut**, which drops it. A schedule can kill, reopen, act and then cut (r3 N3).
- **Raise:**
  - after each `pwrite`;
  - after the `sync_data`, before the directory sync;
  - after the directory sync, before the marker;
  - after the marker;
  - a new segment whose directory entry is lost: open must refuse or keep the previous tip, and never
    trust a marker that points past a missing file. With step 3 this cannot happen; the test runs the
    raise with step 3 removed and asserts that the failure is detected;
  - a sync error, which must poison the writer: the next raise fails, and no slot is written (D18).
- **Torn slots (r2 M1, r3 n2):**
  - a torn A while B holds the newest file `seq`;
  - a torn B while the SQLite slot is newer;
  - a torn file raise with a Buffered SQLite slot newest, then lost;
  - a torn first-ever raise, which must give an empty store with no segment files left.
- **Kill, reopen, act, cut (r3 N3):**
  - schedule A, a Buffered deep truncation killed before its Durable batch, then reopened, then cut;
  - schedule B, a file raise killed before its `sync_data`, then reopened, then a Durable subscriber
    commit, then cut.

  In both, no committed header may be lost, and no row may name a height above the marker.
- **Lowering.** The oracle for every case below checks **S1** (each record's parent stored), id
  uniqueness, and "each committed record in one place".
  - after each batch, including after batch 1 of a three-batch lowering below C, then a kill, reopen
    and cut (r4 R1 schedule A);
  - a Buffered earlier batch lost;
  - **a Buffered lowering lost under a durable file write** (r2 M4): with the last batch Durable, the
    old branch's subscriber rows must be rewound;
  - **a reorg above the committed tip, cut after the Durable batch, or after the pre-sync with a
    Buffered step 3** (r3 N1, r4 R1 schedule B): open must find a durable slot whose records, and
    every copy's parent, are stored;
  - between the file sync and the post-lowering commit (r2 M3), asserting **id uniqueness** across
    active and off-chain records, not only "each record in one place";
  - a first lowering on a header-only store, which creates SQLite first;
  - **`spv/` lost after a Durable first lowering** (r3 N2): with the storage-directory sync this
    cannot happen; the test removes the sync and asserts that the failure is detected.
- **D4 creation (condition 4):** a crash during lazy SQLite creation with a live file marker. Either
  the empty database is recreated, or the schema without a `meta` row is accepted; the file slot
  governs.
- **Concurrency (r3 N4):**
  - a subscriber commit racing a raise;
  - **a lowering racing a raise**: the lowering waits on the writer mutex, and the raise's slot
    carries its snapshot.
- **D11 (r3 N5):**
  - a header-only v4 store reopens with `block_headers/` in place (no set-aside, no deletion);
  - v2 → 4 cut before the marker, which must redo with nothing deleted;
  - v2 → 4 cut after the marker, before the version write, which must redo to the same slot;
  - a v3 store whose `spv/` holds an unknown file, which must refuse with nothing deleted.
  - a header-only v2 store, which must migrate to a marker with its segments kept (r4 nit a);
  - a `Corrupt` v2 build's set-aside interrupted and resumed, which must move `block_headers/` too
    (r4 nit a).
- **Clear (r3 n7, r4 nit e):** a kill after each clear step; the reopen must finish the clear. An
  SB14 Open refusal must reach D19's clear through `DiskStorageManager::new`.
- **Base run (r3 N6):**
  - a header-only store, and a v2 → 4 store, reopen with ids from the base run;
  - their first lowering materializes the base run.
- **Property test against `mem_store`:** random appends, overwrites, reorgs, clears and reopens,
  including a lowering followed by a linear append (r2 M3, no crash). The oracle checks `by_id`,
  `by_hash` and id uniqueness.
- **The r1 M3 correctness test (r2 m7):** a linked run whose first header duplicates an off-chain
  child of the tip must be refused through the `hdr_prev(tip)` probe.

## 4. Measured

**Method.**
- **Harness.** Scratch probes, not committed (`/work/scratch/sb13/sb14/hyb`): one source each for v2
  (base df42294e2's `DiskStorageManager`) and for the hybrid (the prototype `sb14_hyb.rs`, with
  D1–D3, D6, D13 and D14).
- **What the rev 2.2 prototype includes:**
  - the two file slots with crc, `start` and flags, B at 65,536 (r3 n1, n3);
  - open's `sync_data` of the winning file slot (r3 N3, D17);
  - D20's parent sync when the storage directory is created;
  - the directory syncs (and the storage-directory sync after creating `block_headers/`);
  - the 32-fd cache;
  - every hash candidate verified;
  - the height-cap error;
  - open's contiguity check and truncation.
- **The prototype's open order differs from §3 (r4 nit g):** it deletes out-of-range segments before
  the contiguity and tip-hash checks. §3's order binds the implementation: checks read-only and
  first, durability, then truncation. The counts are unaffected.
- **Not in the prototype:** id runs, SQLite slots, lowering, the writer mutex, poisoning and the clear
  intent. None of them runs in a bench row: they live on the lowering, subscriber and clear paths, or
  are lock and error handling with no syscalls.
- **Which rows were re-run for rev 2.2:** `open` (N3) twice, and `store`, `durable` and `persist`
  once, because of the slot move and D20 (`res-22.txt`). The other rows are rev 2.1's (`res-21.txt`):
  nothing on their paths changed.
- **A sanity mode:** a torn newest slot falls back to the previous raise (tip 129,999 of 259,999),
  and open deletes the 13 segment files above it.
- **Regions.** Each workload copies its bench routine's region. Counters are taken in-region the way
  perfgate takes them:
  - `/proc/self/io` for bytes and read/write syscalls;
  - a counting global allocator;
  - minor + major faults;
  - `VmHWM` after a `clear_refs` reset;
  - `instructions:u` through `perf stat --control`;
  - `fs_syscalls` from `strace -f` between region markers, with perfgate's call set and without
    `/proc` and `/sys`.
- **Noop subtracted.** The harness's own reads (4 syscalls) and statx calls (5) are subtracted.
- **Calibration.** The v2 harness reproduces perfgate's base row for `store`:

  | Counter | Harness | perfgate |
  |---|---|---|
  | instructions | 1,450.4 M | 1,449.4 M |
  | allocs | 255,273 | 255,251 |
  | alloc bytes | 116.13 MB | 116.15 MB |
  | write bytes | 26,880,044 | 26,880,046 |

- **The lazy variant (D4).** The hybrid does not run SB13's open in the probe. Its `store` and
  `durable` rows add SB13's non-SQLite open I/O, measured from the SB13 open trace: the lock file,
  `version.json` and the directory scans, 56 fs syscalls and 61 B written.
  - Their instructions, allocations and RSS are small and not synthesized. The real gate shows them.
  - The `warm`, `persist`, `get`, `revindex` and `open` regions contain no SB13 open.

Peak RSS is VmHWM in KiB.

| Row (routine) | Counter | v2 | Hybrid (rev 2.2) | Within the threshold? |
|---|---|---|---|---|
| `store` | instructions | 1,450.3 M | 196.1 M | yes (−86 %) |
| | allocs / alloc bytes | 255,273 / 116.1 MB | 159 / 40.0 MB | yes |
| | page faults | 11,460 | 2,744 | yes |
| | read bytes / read syscalls | 0 / 0 | 1 B / 1 | yes: the floor is 512 B and 1 (glibc's read, D14) |
| | write bytes | 26,880,044 | 26,880,061 | yes (+17 B) |
| | fs_syscalls | 222 | 85 (D20: one open and one `sync_all` of the new storage directory's parent) | yes |
| | peak RSS | 47,392 | 40,992 | yes |
| `durable` | instructions | 1,452.5 M | 196.1 M | yes |
| | allocs / alloc bytes | 255,352 / 120.3 MB | 176 / 40.0 MB | yes |
| | page faults | 11,953 | 2,715–9,047 | yes |
| | read bytes / read syscalls | 0 / 0 | 1 B / 1 | yes |
| | write bytes | 29,120,059 | 29,120,125 | yes (+66 B: the 64-byte slot) |
| | fs_syscalls | 245 | 115 (27 `sync_data`, 2 directory `sync_all`) | yes |
| | peak RSS | 49,336 | 41,732–41,768 | yes |
| `warm` (D10's `store_warm`) | instructions | 1,452.9–1,453.4 M | 178.1 M | yes |
| | allocs / alloc bytes | 254,888 / 112.6 MB | 119 / 33.3 MB | yes |
| | page faults † | 831–5,620 (median 4,715) | 869–7,110 (median 3,429) | † |
| | read syscalls | 0 | 0 | yes: the tip segment stays resident after `stop` |
| | write bytes | 26,880,000 | 26,880,000 | yes |
| | fs_syscalls | 193 | 24 | yes |
| | peak RSS | 109,528–111,692 | 100,684–104,172 | yes |
| `persist` (one iteration) | instructions | 3.89 M | 0.05 M | yes |
| | allocs / alloc bytes | 63 / 4.20 MB | 11 / 959 B | yes |
| | read syscalls / page faults | 0 / 257 | 0 / 0 | yes |
| | write bytes | 2,240,000 | 2,128,064 | yes: only the dirty range is written |
| | fs_syscalls | 15 | 12 (5 `sync_data`, 2 `sync_all`, 5 opens) | yes (M2 adds 4: two directory opens and two syncs) |
| | peak RSS | 41,388 | 38,036 | yes |
| `get` (20 lookups) | instructions | 18.09 M | 0.05 M | yes |
| | allocs / alloc bytes / page faults | 304 / 20.35 MB / 0 | 0 / 0 / 0 | yes |
| | read bytes / read syscalls | 20.16 MB / 2,484 | 2.26 KB / 20 | yes |
| | fs_syscalls / peak RSS | 35 / 51,388 | 0 / 37,944 | yes |
| `revindex` (500 lookups) | instructions | 26.39 M | 0.42 M | yes |
| | allocs / alloc bytes / page faults | 5,192 / 5.02 MB / 0 | 0 / 0 / 0 | yes |
| | read bytes / read syscalls | 4.24 MB / 519 | 16.1 KB / 500 | yes |
| | fs_syscalls / peak RSS | 518 / 51,384 | 0 / 37,928 | yes |
| `open` (D10) | instructions | 1,281.8 M | 158.3 M | yes |
| | allocs / alloc bytes / page faults | 254,551 / 96.2 MB / 11,261 | 158 / 3.2 MB / 510–511 | yes |
| | read bytes / read syscalls | 30.24 MB / 3,728 | 29.12 MB / 28 (two slot reads) | yes |
| | fs_syscalls / peak RSS | 81 / 79,080 | 33 (r3 N3: +1 `sync_data` of `tip`) / 37,900 | yes |

† `warm`'s page faults vary roughly eightfold between runs on both sides, over five runs each
(`res-21.txt`, `res-warm.txt`). The hybrid's median is lower. The implementation's perfgate run judges
this counter against its own noise floor.

**Notes on the table.**
- The `store` trace shows 4 `statx` where the noop shows 5, so the noop subtraction may undercount
  `store` by 1 (r3's harness nit). That makes 85 at most 86, against 222.
- `durable`'s page faults vary between runs (2,715 in rev 2.1, 9,047 in rev 2.2, on unchanged
  code). Both are under v2's 11,953.
- Rev 2's RSS column was mislabelled MiB (for example "47.4 MiB" for 47,392 KiB). The comparisons
  were unaffected.
- `instructions_total` is the whole bench process, including the `get`/`revindex` store's setup, and
  every part of it is cheaper.

**SQLite opened inside the region fails (why D4).** These are the eager variant's increments over the
lazy one:

| Row | write bytes | read bytes | read syscalls | fs_syscalls |
|---|---|---|---|---|
| `store` | +173 KB (the threshold is 134 KB) | +205 KB | +57 | +80 |
| `durable` | +91 KB | +123 KB | +36 | +68 |

**Where the increments come from:**
- **Schema creation:** 21 pages (86 KB) through a rollback journal, then 86 KB of WAL frames for the
  initial rows.
- **The close checkpoint:** it reads those frames back, 49 reads and 200 KB.
- **Any open:** it preads the database header, and in WAL mode the WAL header too.

So SQLite cannot be opened in these rows, whatever the marker.

**Mainnet scale (2.4 M headers).**
- **v2, measured in rev 1's probe** (`res-v2-2m4.txt`):
  - the store's peak increment is +34.9 MiB;
  - open's VmHWM is 296 MiB.
- **The hybrid, projected from its structures:**
  - sync: about 22–23 MiB (the 18.3 MiB index, a pending buffer of about 1.5 MiB, 0.5 MiB of merge
    scratch, and 2.2 MiB of segment buffers);
  - open: about 21 MiB (the entries at exact capacity, sorted in place).
- Both are under v2's peaks. The implementation measures N = 2.4 M in the same harness (§7).
- Open reads 263 MB, as v2 does.

## 5. D4's conditions (Opus r2, accepted)

1. **Disclosure.** An anchored product store creates SQLite in its first header operation, because
   the anchor's context records live there.
   - The one-time cost is that of SQLite's creation and first close: about 173 KB written and 205 KB
     read, plus about 20 M instructions.
   - No bench row measures it. A product reopen's few database-header reads are also unmeasured
     (negligible beside the 263 MB of segment reads).
   - D15 sets `journal_mode=WAL` before the DDL, which roughly halves the creation bytes.
   - A BL row is requested from the manager (§7).
2. **Absent means `ENOENT`.** Only a missing `kv.sqlite` reads as "no database". One that exists but
   fails to open or read (an I/O error, `UnknownDatabase`, `SchemaTooNew`) fails the open (§3 Open
   step 1).
3. **The set-aside arm.** `block_headers` leaves SB13's v2-leftover set on **both** arms of
   `remove_v2_leftovers` (`migrator/v3.rs:216-231`) for version ≥ 4: deletion when `spv/` exists, and
   the set-aside to `v2-unmigrated/` when it does not. A header-only v4 store has no `spv/`, so with
   today's code every reopen would move `block_headers/` away. D11 carries this, with a test on a
   header-only store.
4. **Crash cases:** lazy creation with a live file marker, and a first lowering on a header-only
   store (§3, Crash tests).
5. **§1's sentence** on `store` and `durable`.

## 6. D-rows

| D | Decision | Why |
|---|---|---|
| D1 | Active headers live in v2-record-format segment files (10,000 slots × 112 B, `block_headers/segment_NNNNNN.dat`). Only [start, tip] is meaningful: the tail segment ends at the tip, and an anchored first segment is zero-filled below `start`. A segment's dirty range is written with one `pwrite`, on eviction or at a raise. Two resident buffers are reused. Segment fds sit in a 32-entry LRU cache, and an unsynced file is `sync_data`ed before it is closed (Opus r2 M5). v2's temporary file, rename, per-segment fsync and per-write `create_dir_all` are dropped. | DEC-247. v2's bytes are the bar, and the same bytes in fewer calls keep every byte counter level and every call counter lower (§4). Writing only the dirty range writes less on `persist` (2.13 MB against 2.24 MB). At mainnet, about 240 files must not mean 240 fds against iOS's 256. |
| D2 | The committed tip has three 64-byte slots, each `(magic, format, flags, seq, start, tip, tip hash, crc32)`: file slots A (offset 0) and B (offset **65,536**) in `block_headers/tip`, and SQLite `meta.hdr_marker`. Flags: bit 0 `linked` (D12), bit 1 `empty`. `seq` is one global `u64`; at open, the in-memory `seq` is the maximum over every valid slot, SQLite's included. A file raise writes the file slot not holding the newest **file** `seq` (computed over the file slots alone), then `sync_data`. Two valid slots with equal `seq` and different contents are corruption. Open reads every slot first, makes the winner durable before acting (D17), and only then truncates. No valid slot, or an `empty` winner, means no active records. Lowering writes only the SQLite slot, atomically with the off-chain copies (D7). | r2 M1, r3 n1/n2/n3. A torn write loses only the slot being written, and the fallback is the previous file raise, which never lies below a committed height. 64 KiB clears every target's page and block size (16 KiB on iOS and Android 15+), so a page writeback never touches both slots. Computing the file slot over file `seq`s alone keeps the survivor durable even when a newer Buffered SQLite slot is lost. The file slots let a header-only store raise its tip with no SQLite (D4). The SQLite slot rides commits the product makes anyway, and makes lowering atomic. `start` serves anchored stores and v2 migration (r2 m1). |
| D3 | The hash index lives in memory: a sorted `Vec<u64>` of (key << 24 \| height), plus a `HashMap` buffer of ≤ 65,536 entries, merged in place. The key is `hash[0..5]` in **internal byte order** (as stored and hashed). Every candidate is verified against the stored hash and bounded by the tip, with no cap on candidates (Opus r2 m2). Merges drop exact duplicates. Lowering removes entries above the new tip from the buffer, and leaves the sorted run's stale entries, which verification rejects, until the next merge, which drops entries above the tip. Heights must be below 2²⁴: `store_headers` returns an error for a batch reaching 2²⁴ before touching any buffer, and open treats a slot with `tip` ≥ 2²⁴ as corruption (Opus r2 m3). | 8 B a header: 2.1 MB at 260 k (v2: 5.0 MiB plus rebuild transients), and no per-group `Vec`s (v2's 255 k allocations). Internal order matters because PoW's zero bytes sit at the end of the internal hash: a display-order prefix would put every mainnet hash under one key. 40 bits make false candidates about n²/2⁴¹ per chain: 0 in the bench, measured. 32 bits made 7 verify reads, which the `read_syscalls` floor of 1 rejects. Verification bounded by the tip means no collision can give a wrong `get` or `revindex`. 2²⁴ heights is about 68 years of mainnet. |
| D4 | The SQLite database is created by its first write. A store that holds only headers has none. An existing database opens at `DiskStorageManager::new`, and recovery runs as now. Only `ENOENT` means absent; an unreadable existing file fails the open. Reads against an absent database return empty. **On creation (r3 N2, r4 nit c):** after `SqliteEngine::open` returns, and before the first commit that carries a slot or rows, the storage directory is `sync_all`ed once. SQLite's own directory sync on the WAL's first sync then covers `kv.sqlite` and its WAL. **Accepted by Opus r2 under the five conditions in §5.** | §4: opening SQLite in the `store` or `durable` region costs +57 read syscalls, +205 KB read and +173 KB written. The floors are 1 syscall, 512 B, and 134 KB written. No work leaves the region: a header-only workload never needs the database. Without the `spv/` sync, a power cut after a Durable first lowering could lose `spv/` whole, and a stale file slot would be trusted (r3 N2). The sync costs one `fsync` per datadir, on a path no bench row takes. |
| D5 | Active-record fields are derived from the height (§2). **Ids:** runs partition [start, tip] into runs of consecutive ids, with disjoint id ranges. Runs live in memory and in SQLite (`hdr_runs`). (0) **The base run (r3 N6):** while there is no database, or `hdr_runs` has no rows and `meta.next_id` is unset, [start, tip] is one open run with first id 0 at `start`. The commit that first writes `hdr_runs` or `meta.next_id` writes the base run's row first: closed if that commit closes it (a lowering), open otherwise. An anchored store's first commit gives the anchor's context records ids 0 to k − 1, sets `meta.next_id` = k, and writes the open run starting at id k at `start`. (1) The last run may be **open**, meaning it extends implicitly to the tip, so a linear append writes nothing. While a run is open, the next id is the open run's implicit end; otherwise it is `meta.next_id`. (2) Any commit that issues an id other than by extension closes the open run first: an off-chain record, or a lowering's moves. Closing writes the run's length at the in-memory tip, and sets `meta.next_id` past every id issued. (3) The next active append after a close starts a new open run at `meta.next_id`, and that run's row reaches SQLite before any file slot covers its heights (§3 Raise step 5). (4) Open trims runs to [start, tip] in its Durable repair commit (D17). It **never re-opens a closed run** (a run with a stored length stays closed), and `meta.next_id` survives the trim (r3 n6). With runs present, a height with no run is corruption. | r1 M4, r2 M3, r3 N6/n6. Disjoint ranges make `by_id`'s floor lookup exact. The base run gives a header-only store (D4) and a v2 → 4 store (D11) their ids with no rows and no SQLite. Closing on every non-extension id stops an implicit extension from re-issuing a moved-off id, with or without a crash (r2 M3 case a). Step (3) stops a crash from leaving file-covered heights with no run (r2 M3 case b). Never re-opening a trimmed closed run stops re-issued ids that collide with an off-chain record from the closing commit (r3 n6). |
| D6 | The stored-header check. A linked run on a linked store needs none, except one `hdr_prev(tip)` probe when the off-chain count is above zero (Opus r1 M3's induction; tested, §3). Any other run is checked header by header against the in-memory index, plus `hdr_hash` when the off-chain count is above zero. The count is resident. | One lookup per run in the product sync, instead of one per header (SB13 D51). The bench's unlinked runs pay only in-memory lookups: 0 file reads at 260 k. |
| D7 | An overwrite, a reorg or a truncation lowers the tip under the writer mutex (D16). **First**, if the in-memory tip T is above the committed tip C, raise steps 1–3 run over every unsynced range, with no slot (the pre-sync). **Then** batches of ≤ 2,048 records, from the top. Each batch is one SQLite commit: the off-chain copies; the SQLite slot at **b − 1**, where b is the lowest height that batch copies (`empty` below `start`); the trimmed and closed runs; and the subscriber rewind above f − 1, read under the batch's lease (every batch; a no-op after the first unless a subscriber commit wrote above f − 1 in between, D16 (3)). **The last batch is `Commit::Durable`**; earlier batches may be Buffered. Then the new records are written and synced (raise steps 1–3), with no file slot. Then **one** commit raises the SQLite slot to the new tip, writes the new runs (re-activated records keep their ids; new headers take `meta.next_id`), and drops the re-activated copies. On a header-only store, the first lowering creates SQLite first (D4). | r2 M3/M4, r3 N1, r4 R1. A slot below its own batch makes every batch prefix a valid lowering. The pre-sync means that no slot names an unsynced height, and that every copy's parent is stored (S1): either an earlier copy, or active at or below the slot. Rev 2.2's per-batch min(f − 1, C) dropped committed, uncopied records after batch 1 of a deep lowering, and left copies whose parents were unsynced (r4 R1, schedules A and B). The pre-sync costs at most one `fdatasync` of the tip segment, on a path no bench row takes. A Durable last batch means no durable file write can sit over a lost lowering. Memory is O(batch) at any depth (r1 M5.3). Records above the marker are garbage, and open truncates them (DEC-247). |
| D8 | SB13 D49's write-back overlay, its worker flush, `MAX_UNFLUSHED` and the drop flush are removed. A drop without `stop` writes nothing, as in v2; it loses what a crash loses (§3). | The base bench's `store` drop is inside the region, and v2's drop wrote nothing. |
| D9 | v2's bench-facing API stays: `StorageManager::block_headers()`, and `PersistentStorage::persist(dir)` on it, which keeps v2's contract: when it returns, every stored header survives a power cut. It is `sync_wal`, a raise with every SQLite commit Durable, not the Buffered raise a tick may take (impl D40, Sol impl r1 #2). | The gate builds the head with the base's bench. Without this API, the base's bench does not compile on the head. |
| D10 | Land `storage/disk/store_durable`, `store_warm` and `open` on integration first, as one bench-only change on v2 before SB13 + SB14. It carries: (1) the three routines, written for v2 (no comment cites SB13 D49's overlay); (2) the three `workloads.toml` entries (no `ungated` beyond `syscalls`/`write_syscalls`, as for `store`); (3) the 15-run anchor (perfgate README, "Merging the harness change that adds it"). **`open`'s routine:** setup stores 260 k headers and `stop`s; the region is `DiskStorageManager::new` on that directory; the drop comes after the region. **`warm`'s routine:** setup stores 260 k linked headers and `stop`s; the region is 260 k more linked appends; the drop comes after the region. `store`'s routine stays the base's. After D10, the SB13 + SB14 PR shows **no** diff to `dash-spv/benches/storage.rs` or to those `workloads.toml` lines; SB13's own bench edits are dropped. | Opus r1 m5 and r2 m6. The three rows get v2 bases, and the PR makes no BENCH-CHANGED row. |
| D11 | **Storage version 4, checked against SB13's `migration_loop` (`migrator/mod.rs:129-166`).** That loop runs `MIGRATORS[version]` and then writes `version + 1` after **every** step (r3 N5). (a) **Targets.** `Migrator` gains `target()`. `CURRENT_VERSION` = 4, and `MIGRATORS` is indexed by source version: `[V1 (0→1), V2 (1→2), V2to4 (2→4), V3Discard (3→4)]`. The loop sets `version = m.target()` and writes it. A v2 store goes **straight to 4**; nothing ever writes 3. A fresh directory runs V1, V2 and V2to4 as no-ops and writes 1, 2 and 4: three writes, as today. (b) **V2to4** is SB13's `v3::migrate` (`v3.rs:102`) with four changes. (1) `build()`'s `HEADERS` loop (the header copy into SQLite, `v3.rs:245`) is skipped, and `block_headers` alone does not count as v2 data for the build decision, so a header-only v2 store gets no database (D4). (2) `block_headers` leaves `V2_FOLDERS` for the post-migration deletion, the leftover deletion **and** the leftover set-aside arm (§5 condition 3). A build that fails as `Corrupt` still sets aside every v2 folder, `block_headers` included: no marker exists yet. **Below version 4, the set-aside folder set includes `block_headers`, in both `set_aside` and `resume_set_aside`** (`v3.rs:111`, `mod.rs:100`). At version 4 it does not (r4 nit a). Otherwise an interrupted set-aside would be resumed without `block_headers/`, V2to4 would return `Ok`, and open would delete the segments. (3) **The marker step runs whenever `block_headers/` holds a v2 run, with a build or without one** (r4 nit a). `v3::migrate`'s early `return` when no other v2 folder is present (`v3.rs:114-116`) must not skip it: a header-only v2 store, such as the base bench's own directories, otherwise gets version 4 with no marker, and open deletes its segments. After the build's rename and `sync_dir`, when there is a build, the step writes the marker: `start` is v2's first non-sentinel record, `tip` is the end of the first contiguous run from it (as `build()` defines it), and `linked` comes from that scan. It `sync_data`s those segment files, writes slot A with `seq` 1, `sync_data`s `tip`, and syncs `block_headers/`. (4) The step returns only after the marker is durable, so `version.json` = 4 is written only then. Open's truncation later cuts the sentinel tail and anything past a hole. (c) **Redo.** A crash before the version write leaves version 2, and the step reruns. `v3::migrate` already deletes its own `spv/` and `spv.migrating/` first; the v2 folders are intact, because they are deleted only after the version write. Nothing writes `block_headers/` at version 2 (open runs only at 4), so the rerun computes and writes the same slot A bytes. No v2 data is deleted, and Open's "no valid slot → delete the segments" never runs on a v2 directory mid-conversion. (d) **V3Discard** runs only on a `version.json` read as 3 (SB13 r1–r5, unreleased). It deletes the known file names inside `spv/` and `spv.migrating/` (`kv.sqlite`, `-wal`, `-shm`, `-journal`), then the empty directories. Any other entry **inside those directories** refuses the open, with nothing deleted. At the storage path, `logs/`, `peers/`, the lock file, `version.json` and `v2-unmigrated/` are expected and untouched. Stray v2 folders, from a crash before SB13's leftover cleanup, go through `remove_v2_leftovers` after the loop as today. A stray v2 `block_headers/` has no `tip`, so open's no-valid-slot rule deletes its segments, and they are refetched. | r1 m4/m6, r2 m1 and D4 condition 3, r3 N5. A literal "v2→v3 steps, then v3 → 4" would discard the freshly migrated filters, or refuse the store, after the version-3 write. Going straight to 4, with the version written only after a durable marker, makes every crash point either a redo with nothing lost or a finished v4 store. |
| D12 | Linkage is derived at open (every record's `prev` against the record below it), and every slot also carries a `linked` flag (D2). An unlinked append clears it, and it stays clear until the store is cleared. A slot marked `linked` over records that break linkage in [start, tip] is corruption (r3 n3). On an unlinked store, `leaves_from` and `children_from` scan the files, and `walk_down` follows hashes. Only legacy test and bench writes make one. | r1 m2, r3 n3. Open reads every record anyway (§3), so the check is free. Without the flag, a mixed chain left under a valid slot would open silently as a legacy unlinked store. |
| D13 | `get_header` and `get_header_height_by_hash` run inline on the async task, as v2's do: an in-memory lookup and one `pread` from the page cache, under the lookup lock. Syncs and SQLite commits run outside that lock, under the writer mutex (D16). | Through the blocking pool, `revindex` measured 553 allocations and 26 page faults per run against v2's 0 page faults; inline, 0 and 0. |
| D14 | Large buffers are reused in the measured paths: the two segment buffers, and the index merge's scratch (`reserve_exact`, merged from the back). | Hygiene, not the fix for `read_syscalls`. glibc's `check_may_shrink_heap()` reads `/proc/sys/vm/overcommit_memory` (1 byte) the first time a thread-arena heap shrinks, then caches the answer: **at most once per process**. Opus r2's strace places it in the first store call, on a blocking-pool thread, before `segment_000000.dat` is created, not at the drop as rev 2 said. It sits on the 1-event floor, which perfgate's strict `>` accepts. BL-266 (DEC-285) traced the shrink: with `strace -k`, on a bench build with line tables, the free comes from hashbrown's resize when `Index::insert` (`index.rs`) grows the buffer `HashMap` during `Hdr::append`, on the first blocking-pool thread (not the `spawn_blocking` input, as this row said before). strace of the `store` region at c695280dd (v2) and 0025a7617 (the T20 merge), one iteration each: that `read` of `/proc/sys/vm/overcommit_memory` is the only read-family call in either region; v2 has none, since nothing it frees shrinks a thread-arena heap there. It is not the committed-tip marker read, a split header read or an open probe. `GLIBC_TUNABLES=glibc.malloc.trim_threshold=4294967295` removes it, and so does reserving the buffer to `BUFFER` at its first insert: then `read_syscalls` = 4 (v2's), instructions −1.5 % (187.6 M against 190.4 M, no rehashes), allocs 455 against 470–495, peak RSS unchanged. Neither is taken. The probe happens once per process wherever a thread-arena heap first shrinks, so moving it out of this region saves nothing in the product. `drain()` keeps the buffer's capacity, so a process that once filled it (57 345 entries) already holds the 131 072-bucket table (2 MiB) for life. Reserving it up front costs that table, its pages touched at random, in every process that never fills it, such as a synced client restarted daily (about 576 headers a day). DEC-285 waives the +1 on `spv-storage-store` instead. |
| D15 | When D4 creates the database, `journal_mode=WAL` is set before the DDL. | D4 condition 1: the schema is then not written through a rollback journal and then again through the WAL. This roughly halves the one-time creation bytes. |
| D16 | Two locks (§2 Locks). The **lookup lock** guards in-memory state, and lookups and non-lowering appends take only it. The **writer mutex** is held across a raise (snapshot through slot write), a lowering (first batch through the post-lowering commit), any slot-carrying SQLite commit, open and clear. A raise's snapshot is taken under both: the dirty ranges written, the unsynced set taken out as **owned handles** (`Arc<File>`), and `(start, tip, tip hash, flags)` captured. The syncs then run outside the lookup lock, and the slot written carries the captured values. An fd-cache eviction of a file still unsynced syncs under the lookup lock (rare, accepted). **Lock order (r4 nit b):** writer mutex → SB13 lease → fence → view/lookup lock, extending SB13's lease → fence → view (`store/mod.rs:258-268`). (1) A `store_headers` that finds it must lower, while under the lookup lock, drops that lock, takes the writer mutex, and re-reads C and the tip. That is safe because header writes are serialized by the header storage's `RwLock`, and a raise never moves the in-memory tip. (2) A subscriber commit decides whether it carries a slot under the lease, from what (3) reads there; one that must drops the lease before it takes the writer mutex. (3) A subscriber commit reads its bounds and locator, and the height h they name, under the SB13 lease, and commits there with no slot when "committed tip ≥ h". Otherwise it drops the batch and builds it again under the writer mutex, after raise steps 1–3, with the SQLite slot and h capped at that slot's tip (a fast append may move the tip after the raise). A lowering batch reads the subscribers' bounds for its rewind under the lease, and publishes its lowered committed tip and the rewound bounds before it releases the lease, so header and filter storage, which have separate `RwLock`s (`storage/mod.rs:170-175`), cannot race on "Writes that name headers" (Opus-h impl r1 M1: they did when the bounds were read before the lease). | r3 N4/n5. Without the mutex, a lowering inside a raise's sync window lets the raise write a slot over a stale tip and hash, and open then trusts a mixed chain. Owned handles let the fd cache close its own handle during the sync, because the raise's keeps the inode open. Lookups never wait on an fsync (D13). |
| D17 | Open makes the winning slot durable before it acts on it: a `sync_data` of `tip` when a file slot wins, and one **Durable** SQLite commit when the database exists. That commit carries open's repairs (runs trimmed, or dropped when there are no active records), and it uses a forced barrier, a new `dash-kv-sqlite` commit option (for example `Commit::DurableBarrier`), that always bumps `meta.sync_seq` (r4 nit d). SB13's `recover()` runs after it, against SB14's winning slot. Checks and the rebuild are read-only and come first; truncation comes last. | r3 N3. After a process kill (jetsam, a low-memory kill, a panic), the page cache keeps Buffered commits and unsynced slot writes. Acting on them, by truncating or by commits naming their heights, and then losing them to a power cut, loses committed headers or breaks SB0 (c). Cost: `open` +1 `fdatasync` (33 fs syscalls, against v2's 81). `store` and `durable` open fresh directories with no slot, so they pay nothing. |
| D18 | Any error from `pwrite`, `sync_data`, `sync_all` or a slot write poisons the writer. Raises, lowerings and slot-carrying commits fail until reopen, and lookups keep working. No sync is retried. | r3 n4 ("fsyncgate"). A failed `fdatasync` may clear the dirty state, so a retry passes over lost data, and the next slot would cover a hole that the length check cannot see. |
| D19 | Both clear paths (`clear()`, and `migrate`'s clear-and-restart arm) write `version.json` with `"clearing": true` first. Then, in order: unlink `block_headers/tip` and sync `block_headers/`; clear SQLite (`clear_all` Durable, or delete `spv/`); delete the rest except `version.json` (r4 nit e(i)); write `version.json` = 4 without the flag. An SB14 Open corruption error reaches this clear through `DiskStorageManager::new` (r4 nit e(iii)). Any open that reads the flag finishes the clear first, keeping `logs/` and `v2-unmigrated/`. | r3 n7. Today's read-dir order can leave a stale file slot with no database, or a SQLite slot with rows above it, after a kill mid-clear. The intent makes the clear resumable, and it costs nothing in bench rows: `version.json` is read at every open anyway. |
| D20 | When `DiskStorageManager::new`'s `create_dir_all` creates the storage directory, it syncs the parent of each level it created, up to the first ancestor that already existed (r4 nit f). | r3 n7 (the last point). Otherwise a power cut can remove the whole new datadir after headers were committed in it. Measured: `store` +2 fs syscalls (an open and a `sync_all`; 85 against 222). `durable`, `warm` and `persist` open existing directories, so they pay nothing. |

## 7. Risks and open points

- **`read_syscalls` on `store` and `durable`** has one event of margin (D14). Any new read in the
  region fails it, such as a version probe, or a slot or `kv.sqlite` read on a fresh store (a fresh
  open must see `ENOENT` on both and read nothing). The implementation's perfgate run must show
  `read_syscalls` bit-stable at ≤ +1 on both rows.
  - Implementation (Opus-h impl r1 m6, Sol impl r2 nit): `store` measured `[6, 6, 5]` against base
    `[4, 5, 4]`. `strace -f -k` of the region (6 runs) shows one `read` beyond the base's, glibc's
    `_int_free_maybe_trim` reading `/proc/sys/vm/overcommit_memory` on a blocking-pool thread
    running a block-header `SpvStore::run`, exactly D14's once-per-process probe. glibc caches the
    answer without synchronization, so two threads that trim at once both read it: +2 then. The full
    PF0 rechecks it when SB14 rides a train; the lever is D14's.
- **Mainnet RSS** is bound by DEC-247's "peak RSS ≤ v2"; it is not waived. The comparison is against
  v2's measured 2.4 M peaks (+34.9 MiB at store, 296 MiB at open), and the hybrid's projection is about
  22–23 MiB and 21 MiB (§4).
  - The implementation records the hybrid at N = 2.4 M in the same harness. It is not gated (the
    bench is 260 k).
  - If the index ever needs to shrink: a 16-bit bucket table (256 KiB) plus 6-byte entries (24 prefix
    bits + 24 height bits) keeps the 40-bit collision rate at 14.4 MB.
- **The writer mutex (D16)** is held across a raise's syncs. A lowering, or a subscriber commit that
  carries a slot, can therefore wait up to one raise's syncs, about one tick's segments. Lookups and
  plain appends never wait on it.
- **`persist`'s `fs_syscalls`** are 12 against v2's 15. M2's directory syncs cost 4 per first raise
  (two opens, two syncs) and are the reason for the narrower margin.
- **`warm`'s page faults** are noisy on both sides (§4 †). The gate's noise floor judges them.
- **Product fsyncs.** A raise is one sync per written segment file, plus the marker's (none when the
  marker rides a subscriber commit), plus a directory sync when a file was created: about one per
  10,000 headers.
- **BL request (D4 condition 1):** "SB14 D4: an anchored SPV store's first header operation creates
  SQLite: about 173 KB written and 205 KB read once (about half with D15). Not in any bench row." The
  manager files it.

## 8. Review findings

**Opus-h r4.**

| Finding | Where | How it closes |
|---|---|---|
| R1 (the batch slot below its batch; nothing unsynced under it) | §3 Lowering step 0 and step 1, the off-chain bullet, the post-crash invariants, the crash tests, D7 | A pre-sync when T > C, then each batch's slot at b − 1 (its own lowest copied height − 1). Every prefix is a valid lowering, no slot names an unsynced height, and every copy's parent is stored (S1). The lowering crash oracle checks S1, with r4's schedules A and B as cases. No bench row lowers, so no counter changes. |
| a (V2to4 gaps) | D11 | The marker step runs whenever `block_headers/` holds a v2 run, build or not. Below version 4, `set_aside` and `resume_set_aside` include `block_headers`. |
| b (lock order) | D16 | Writer mutex → lease → fence → view/lookup, with the lowering re-read, the slot decision under the lease (retried under the writer mutex, D16 (3)), and the published lowered tip. |
| c (N2 ordering) | §3 Directory entries, D4 | The `spv/` sync comes after `SqliteEngine::open` returns, and before the first commit that carries a slot or rows. |
| d (recovery, barrier) | §3 Open step 4, D17 | `recover()` runs after step 4, against SB14's slot. A forced-barrier commit option is named as the needed `dash-kv-sqlite` change. |
| e (D19 edges) | §3 Clear and Directory entries, Open step 2, D19 | Step 4 keeps `version.json`. The first raise after any creation of `block_headers/` syncs its entry. `DiskStorageManager::new` routes an SB14 Open refusal to D19's clear. |
| f (D20 levels) | D20 | Sync the parent of every created level. The bench creates one level, so `store` stays at 85. |
| g (probe open order) | §4 Method | §3's order binds the implementation; counts unchanged. |

**Opus-h r3.**

| Finding | Where | How it closes |
|---|---|---|
| N1 (lowering slot over unsynced heights) | §3 Lowering steps 0–3, D7 | Superseded by r4 R1: a pre-sync (step 0) and a per-batch slot at b − 1. No slot names an unsynced height. Crash case: a reorg above C, cut after the Durable batch. Free on every row. |
| N2 (`spv/` entry never durable) | §3 Directory entries, D4 | On creation, the storage directory is synced after `spv/` exists and before the first commit. SQLite's WAL dirsync covers `kv.sqlite`. `FaultyFiles` models lost `spv/`, `kv.sqlite` and storage-directory entries, with the case "`spv/` lost after a Durable first lowering". Free on every row. |
| N3 (acting on a visible but not durable slot) | §3 Open step 4, D17 | `sync_data` `tip` when a file slot wins, and a Durable repair commit (barrier bumped) when the database exists, both before truncation or any commit. The harness gains kill, reopen, act, cut schedules (A and B). **Probe: `open` 32 → 33 fs syscalls (+1 `fdatasync`) against v2's 81**; read syscalls 28, write bytes 0. |
| N4 (raise against lowering) | §2 Locks, §3, D16 | A writer mutex across raises, lowerings, slot-carrying commits, open and clear. Slots carry the snapshot's captured values. Test: a lowering racing a raise. |
| N5 (D11 against `migration_loop`) | D11 | Checked against `migrator/mod.rs:129-166` and `v3.rs:102-135`. Migrators get targets, and v2 goes straight to 4, with the version written only after a durable marker. Redo is idempotent. V3Discard runs only on version 3, and its refusal is scoped to `spv/`. Crash cases: cut before the marker, and cut after it. |
| N6 (no base id run) | D5 (0) | With no database, or no `hdr_runs` and no `meta.next_id`, [start, tip] is one open run from id 0. It is materialized by the first commit that writes runs or `next_id`, and anchored stores write their first run in their first commit. Tests for a header-only store and a v2 → 4 store. |
| n1 (shared page on 16 KiB systems) | D2, §3 slots | Slot B at 65,536 (sparse). Probe: unchanged counters. |
| n2 (`seq` rules) | D2, §3 slots | The in-memory `seq` is the max over all valid slots; the newest file `seq` is over file slots only; a tie with different contents is corruption. Crash case: a torn file raise with a Buffered SQLite slot newest, then lost. |
| n3 (derived linkage hides corruption) | D2 flags, D12, §3 Open step 2 | A `linked` flag in every slot. Linked over broken linkage is corruption. Free (open reads every record). |
| n4 (sync failures) | §2, D18 | Poison on any write or sync error; never retry. Crash case added. |
| n5 (fd cache against the out-of-lock sync) | §2 Locks, D16 | Owned `Arc<File>` handles in the snapshot. Eviction syncs under the lookup lock are rare and accepted. |
| n6 (marker invariant) | §3 "Writes that name headers" and "What may name heights above the marker", D5 (4) | The invariant is narrowed to subscriber, MN, `locks` and sync-state rows. Runs are trimmed and persisted at open, never re-opened, and `next_id` survives. Off-chain records are height-free. |
| n7 (deletion order) | §3 Open steps 1 and 4, §3 Clear, D19, D20 | A no-active-records open with a database drops the run rows and keeps `next_id` and the context records. Both clears use a resumable `clearing` intent, removing `tip` first. A created storage directory's parent is synced once (`store` +2 fs syscalls, 85 against 222). |

**Opus-h r2.**

| Finding | Disposition |
|---|---|
| D4 ruling | Accepted. The five conditions are in §5 (§1, §3, D11, D15, §7). |
| M1 (torn slot falls back to a stale slot) | Applied (D2, §3): two alternating file slots at separate sectors, one global `seq`, all slots read before truncation, an empty store when none is valid, and three torn-slot crash cases. Probe: +0 syscalls per raise. Two slot reads at open (`open` 28 read syscalls against v2's 3,728). |
| M2 (no directory sync; contiguity) | Applied (§3 Raise step 3, Open step 4): a directory `sync_all` when a raise created files, plus the storage directory when `block_headers/` was created. Open checks every segment's presence and length. Probe: `durable` 112 → 115 and `persist` 8 → 12 fs syscalls, against v2's 245 and 15. |
| M3 (runs after a lowering) | Applied (D5, D7): one post-lowering commit (slot, runs, copies); close the open run on any non-extension id; new runs durable before a file slot covers them. Tests for id uniqueness, with and without a crash. |
| M4 (Durable lowering) | Applied (D7, §3): the last batch is Durable; slot-raising commits may be Buffered. Crash case added. |
| M5 (fds) | Applied (D1): a 32-fd LRU cache that syncs before closing. No row changes (26 files at 260 k). |
| m1 (format, `start`) | Applied (§2, D1, D2, D11): "record format", the short tail and zero-filled anchor hole, `start` in every slot, and v2 → 4's marker and header-copy skip. |
| m2 (index details) | Applied (D3, §2 Locks): internal byte order, no candidate cap (the probe's cap of 8 was removed), duplicate and stale-entry handling, and syncs outside the lock. |
| m3 (24-bit cap) | Applied (D3): an error before any buffer is touched; corruption at open. |
| m4 (mainnet RSS) | Applied (§4, §7): bound by ≤ v2 against v2's measured 2.4 M peaks; measured at implementation. |
| m5 (the overcommit read) | Applied (D14): the mechanism corrected. |
| m6 (D10) | Applied (D10, §4): the `warm` row measured; workloads entries, anchor and `open`'s routine specified; no bench diff in the SB13 + SB14 PR. |
| m7 (small points) | Applied: `sync_data`/`sync_all` with `F_FULLFSYNC` (§3); the r1 M3 test (§3); drop without `stop` (§3). |
| r1 m4 / m6 (partly applied) | Completed in D11: the set-aside arm, and the skip of v3's header copy with v2's marker defined. |

**Opus-h r1 (rev 2's dispositions, still in force).**

| Finding | Disposition |
|---|---|
| B1 (I/O counters) | Resolved by DEC-247's hybrid. Every gated counter is in §4, with SQLite kept out of the header rows (D4). |
| M1 (RSS accounting) | Applied. RSS is taken in-process for both sides in one harness, with the async stack, the bench's data and the store's drop. The hybrid peaks below v2 on every row. Sub-batches are not needed. |
| M2 (header_batch calibration) | N/A for the bench rows: the active path builds no SB13 records. `header_batch` runs only when records move off-chain (D7). |
| M3 (D51 always off) | Applied (D6), with its test (§3). |
| M4 (nested runs) | Applied (D5), with the r2 M3 rules. |
| M5.1, M5.2 (watermark, L0) | N/A: the index is not persisted. |
| M5.3, M5.4 (reorg buffering, crash tests) | Applied (D7, §3). |
| m1 (merge against lease) | N/A as written. The in-memory merge is in place (D14), about 5 ms at mainnet, recorded at implementation. |
| m2, m3 | Applied (D12; §3 lowering and Open). |
| m4, m6 | Applied (D11). |
| m5 | Applied (D10). |
| The slim-record probe | N/A: the files are v2's record format. |

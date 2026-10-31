# SB14 SPV header layout: Opus-h design review, round 4 (narrow re-check, N1–N6)

**Doc:** `docs/node-program/designs/SB14-spv-header-layout.md` on `node/sb13-spv-sqlite` @ **c61aa6ef2** (rev 2.2).
I reviewed `git diff 0c04b255e..c61aa6ef2` and re-read §2 Locks, §3, §5, D2, D4, D5, D7, D11, D12 and D16–D20 in full.

**Context read:**
- my r3 (`reviews/SB14-design-r3-opus.md`);
- SB13 @ c61aa6ef2:
  - `migrator/mod.rs:60-166` (`migrate`, the restart arm, `migration_loop`);
  - `migrator/v3.rs:20-52, 102-135, 173-231` (`V2_FOLDERS`, `migrate`, `resume_set_aside`, `set_aside`, `remove_v2_leftovers`);
  - `dash-kv-sqlite/src/lib.rs:262-326` (`SqliteEngine::open`, its DDL commit, `buffered: false`) and `:520-540` (the barrier);
  - `storage/mod.rs:155-175, 292-320` (`new`, `clear`);
  - `storage/handle.rs:50-67` and `store/recover.rs:1-60` (open-time recovery);
  - `store/mod.rs:254-350` (the SB13 lease, fence and view lock order);
  - `store/crash.rs:811-823` (oracle invariant S1);
- dash-chain `store.rs:40-86` (the `HeaderStore` contract);
- the probe: `/work/scratch/sb13/sb14/hyb/res-22.txt` (every row), `run22.sh`, and `sb14_hyb.rs:196, 300-331` (slot offsets, open's `tip` sync).

**Reviewer scratch:** none created. I built and ran nothing.

## Verdict: CHANGES REQUIRED

N2–N6 close as written, and so does N1's own schedule: no r3 schedule still refuses a store that holds committed headers.

But N1's fix, as worded, opens one new hole (R1), and part of it is mine:
- The rule is written per **lowering** (min(f − 1, C)), but it is applied in **every batch**. A multi-batch lowering below C then puts its slot at f − 1 in batch 1. A crash after that batch loses committed records that were never copied, and it orphans batch 1's copies.
- Even with a single batch, the min rule leaves copies of unsynced records whose parents can be lost. That breaks SB13's crash-oracle invariant S1 ("each record's parent is stored a height below it").
- My r3 n6 called such copies "harmless"; that was wrong.

The fix is one rule and costs no bench row. A rev 2.3 that applies R1 needs only a check of the lowering text (§3 Lowering, D7, §8), not a round.

n1–n7 are applied. The probe rows match `res-22.txt` exactly. Seven nits below; none blocks.

## Findings

### R1: MAJOR. The batch slot must sit below its own batch, and nothing at or below it may be unsynced (§3 Lowering step 1, D7, §8 N1)

**The rule as written.** Every batch "writes the SQLite slot at **min(f − 1, C)**", with f the first height replaced. Rev 2.1 said "below the batch", which r2 M4's "every prefix is a valid lowering" depends on. My r3 N1 said "a lowering's slot is min(f − 1, durable tip)" and did not say per batch, so rev 2.2 applied it to every batch.

**Schedule A: a deep lowering below C.**
1. C = 50,000 (file slot, seq 9, durable).
2. A reorg at f = 45,000, depth 5,001, runs in three batches from the top:
   - batch 1: 47,953–50,000;
   - batch 2: 45,905–47,952;
   - batch 3: 45,000–45,904.
3. Batch 1 commits Buffered. It copies 47,953–50,000 and writes the SQLite slot at min(44,999, 50,000) = **44,999**, seq 10.
4. Process kill. (Or a power cut after the kernel wrote back the WAL pages, or after an auto-checkpoint synced them. A Buffered commit *may* survive.)
5. Reopen:
   - Seq 10 wins. Its tip hash at 44,999 matches, because 44,999 is below the fork.
   - Step 4 makes the slot durable.
   - Step 5 deletes segment 5 and cuts segment 4 at 44,999.
6. Records 45,000–47,952 were committed under seq 9. They are now in neither the files nor SQLite, against §3's first "after a crash" invariant.
7. Batch 1's copy of 47,953 names a parent that is not stored, so S1 fails (`store/crash.rs:815-823`).
8. `leaves_from` now offers a leaf at 50,000 whose chain work is above the active tip's, and `walk_down` from it stops at 47,953.

**Schedule B: r3's own N1 schedule, one batch.**
1. C = 1,000. Records 1,001–1,100 are unsynced.
2. A reorg at f = 1,050. The Durable batch copies 1,050–1,100 and writes slot 1,000 (correct under the min rule).
3. Step 2 has not finished, or step 3 commits Buffered.
4. Power cut. 1,001–1,049 were never synced, and open truncates above 1,000.
5. The off-chain copy of 1,050 names 1,049 as its parent, which is gone: S1 fails. This is the same orphaned high-work leaf as in schedule A.

**Required (recommended form).**
- **Before the first batch**, if the in-memory tip is above C, run raise steps 1–3 over every unsynced range, with no slot. That is the same sync step 2 already does, moved earlier.
- **Then each batch's slot is b − 1**, where b is the lowest height that batch copies (`empty` if b − 1 < `start`). C drops out of the rule. Every prefix is again a valid lowering, and every copy's parent is either an earlier copy or active at or below the slot.
- **Step 2 then syncs only the new records.**
- **Cost:** a lowering above an unraised tip pays its pending sync before the batches instead of in step 2. That is at most one extra `fdatasync`, of the tip segment, which step 2 syncs again when new records land in it. No bench row lowers.
- **Doc updates:**
  - Fix §8's N1 row, D7, and the "What may name heights above the marker" bullet. Off-chain copies are harmless *because* their parents are stored.
  - Add **S1** (a closed header graph) to the lowering crash cases' oracle. "After each batch" must check it, not only id uniqueness.

**Alternative:** keep min(b − 1, C), and have open's Durable repair commit (D17) drop off-chain records with no stored parent, recursively.
- It is free on the lowering path.
- But it adds an off-chain scan to every open that has a database, and it is more code to get right.

I recommend the pre-sync.

## Re-check of N1–N6

| r3 | Status | Notes |
|---|---|---|
| **N1** | **Closed for its own schedule; R1 above** | **The rerun:** the batch slot is 1,000, its record is durable, and the tip hash matches. Open keeps 0–1,000, so there is no refusal. The trim of the closed run, and `next_id` past 1,100, keep the ids unique. **What the rule misses:** multi-batch lowerings and the orphan (R1). |
| **N2** | **Closed** | **The rerun:** `spv/`'s entry is durable before the Durable batch, and SQLite's WAL dirsync covers `kv.sqlite` and `-wal`. So seq 41 survives and stale file slot A cannot win. `FaultyFiles` now models lost `spv/`, `kv.sqlite` and storage-directory entries. The ordering wording is nit c below. |
| **N3** | **Closed** | **Schedule A rerun:** the visible seq 12 is made durable by step 4's commit before step 5 truncates, so a later cut keeps seq 12 with segments that match it. **Schedule B rerun:** `tip` is `sync_data`ed before any commit, so the subscriber rows sit under a durable slot. **Probe:** `sb14_hyb.rs:325` syncs `tip` before acting. **Caveats:** SB13's `recover()` ordering, and the barrier needing an engine change (nit d). |
| **N4** | **Closed** | **The rerun:** the lowering's `store_headers` waits on the writer mutex. R writes seq 12 with its snapshot (1,000, old hash), which is durable and true. The lowering then commits seq 13 at 889. Open trusts seq 13. **Open's sync under the mutex:** nothing else can run then (the worker starts after `SpvStore::open`), so there is no contention and no deadlock. The lock order should be written down (nit b). |
| **N5** | **Closed against the code** | `migration_loop` (`mod.rs:129-166`) indexes `MIGRATORS` by source version and writes after every step, so `[V1, V2, V2to4, V3Discard]` plus `version = m.target()` is sound. **Redo:** `v3::migrate` deletes `spv.migrating/` and `spv/` first (`v3.rs:105-109`), and v2 folders go only in `remove_v2_leftovers` after the loop (`mod.rs:73-79`), so the redo really is idempotent and deletes no v2 data. **A fresh directory** writes 1, 2 and 4. **Two gaps** in V2to4's v2 handling are nit a. |
| **N6** | **Closed** | D5 (0) defines the base run for no database, or for no `hdr_runs` and an unset `next_id`, and its materialization comes first. **Consistent cases:** the anchored first commit, and open's "no active records" repair, which keeps `next_id`, so later appends start a new run per D5 (3) whose row precedes any file raise. **V2to4:** the build skips the header loop and so writes no chain `meta`, and the base run applies. |

**Answers to scope 2 (new holes):**
- **N4 against N3:** no hole. Open holds the writer mutex for steps 1–5, before the worker and the storage handles exist (`storage/mod.rs:166-189`).
- **D19/D20:** the intent order (flag, `tip` unlink plus sync, SQLite, rest, version) closes r3 n7's kill-mid-clear case. Three edges remain (nit e), plus D20's multi-level creation (nit f).

## Spot-checks

**Probe rows** (`res-22.txt`, subtracting the noop's 5 fs and 4 read syscalls, and adding the 56 synthesized fs syscalls on `store`/`durable`):

| Row | Raw | Derived | §4 says |
|---|---|---|---|
| `open` | fs 38 (`fdatasync` 1), reads 32, read bytes 29,120,177 | **33**, 28, about 29.12 MB | 33, 28 ✓ |
| `store` | fs 34 (`fsync` 1, the D20 parent sync), reads 5, write bytes 26,880,000 | **85**, 1, 26,880,061 | 85, 1, +17 B ✓ |
| `durable` | fs 64 (27 `fdatasync`, 2 `fsync`), write bytes 29,120,064 | **115**, 29,120,125 | 115, +66 B ✓ (the same as rev 2.1, so D20 adds nothing here, as claimed) |
| `persist` | fs 17 (5 `fdatasync`, 2 `fsync`, 5 opens) | **12** | 12 ✓ |

The probe puts slot B at 65,536 (`sb14_hyb.rs:196`).

**n1–n7:**
- **D2:** B at 65,536 (pages of 64 KiB still separate the slots); `seq` over all three valid slots; file `seq` over the file slots only; a tie is corruption; `linked` and `empty` flags. ✓
- **D18:** poison on any `pwrite`, sync or slot-write error, with no retry. ✓
- **D16:** `Arc<File>` snapshot handles. ✓
- **D5 (4):** the invariant is narrowed; runs are trimmed in the repair commit, and closed runs never re-open. ✓
- **D19/D20:** present. See nits e and f.

## Nits

**a. D11, two V2to4 gaps that delete v2 headers D39 means to keep.**
- **A header-only v2 store gets no marker.**
  - Once `block_headers` leaves `V2_FOLDERS`, `v3::migrate` returns at `if !any_v2_folder(..) { return Ok(()) }` (`v3.rs:114-116`) before any marker step.
  - Then `version.json` = 4 is written, and open's no-valid-slot rule deletes every segment.
  - The base bench's own v2 directories are such stores.
  - **Fix:** say the marker step runs whenever `block_headers/` holds a v2 run, build or not, and add the test "a header-only v2 store migrates to a marker".
- **`resume_set_aside` at version 2 must include `block_headers`.**
  - D11 says a `Corrupt` build sets aside "every v2 folder, `block_headers` included".
  - An interrupted one is resumed by `resume_set_aside` (`v3.rs:111`, `mod.rs:100`), which walks `V2_FOLDERS`. That would leave `block_headers/` at the root with no `tip`, and V2to4 would return `Ok` (`v3.rs:111-113`). Version 4 is then written, and open deletes the segments.
  - **Fix:** below version 4, the set-aside folder set (`set_aside` and `resume_set_aside`) includes `block_headers`; at version 4 it does not.

**b. Write the lock order (N4).** It is: writer mutex → SB13 lease → fence → view/lookup lock (SB13's is lease, fence, view: `store/mod.rs:258-268`).
- **Lowering.** A `store_headers` that finds it must lower, under the lookup lock, drops that lock, takes the writer mutex, and re-reads C. That is safe because header writes are serialized by the header storage's `RwLock`, and a raise does not move the in-memory tip.
- **Slot-carrying subscriber commits** decide to carry the slot before they take the lease.
- **Non-slot subscriber commits** check "committed tip ≥ h" under the SB13 lease. A lowering batch publishes its lowered committed tip before it releases the lease.
- **Why it matters:** header and filter storage have separate `RwLock`s (`storage/mod.rs:170-175`), so this pairing is what keeps "Writes that name headers" race-free against a concurrent lowering.

**c. N2's ordering is not reachable as worded.**
- `SqliteEngine::open` commits its own DDL with `synchronous=FULL` (`lib.rs:300-308`), so nothing can run "before the first commit on the new database".
- **The rule that works:** after `SqliteEngine::open` returns, and before the first commit that carries a slot or rows.
- Losing a DDL-only database is §5 condition 4's case.

**d. N3 against SB13's recovery, and the barrier.**
- **Recovery.**
  - `SpvStore::open` runs `recover()` (`handle.rs:56`), whose rewinds are Durable commits that write locators naming heights.
  - It must run after Open step 4, against SB14's winning slot as the active tip. Or fold its rewinds into step 4's commit.
  - Its own leading `sync_wal` (`recover.rs:31`) is a no-op after an open today.
- **The barrier.** "Always bumps the barrier counter" needs a `dash-kv-sqlite` change:
  - `transaction` bumps only when `buffered` (`lib.rs:530`), which `open` initializes to false (`lib.rs:324`);
  - a commit that writes no page writes no frame and syncs nothing.
  - Name the change, for example a forced-barrier flag.

**e. D19 edges.**
- **(i) Step 4 must keep `version.json`.**
  - It holds the intent, and only step 5 rewrites it.
  - Otherwise, a kill mid-step 4 can leave v4 `block_headers/` segments (the same folder name as v2's) with no version. Version 0 then runs V2to4 over a partial v4 segment set and writes a marker over it.
- **(ii) A recreated `block_headers/` needs its storage-directory sync too.**
  - After `clear()` recreates `block_headers/` in a running store, the next raise must make its entry durable.
  - "At the first raise of an open that created it" doesn't cover a clear, so say "the first raise after it was created".
- **(iii) Say which path clears on an SB14 Open refusal.**
  - §3 says "SB13's clear-and-restart applies", but `SpvStore::open` runs after `migrate` and its errors propagate (`storage/mod.rs:163-168`). `migrate`'s restart arm never sees them.

**f. D20 and multi-level creation.** `create_dir_all` may create several levels (for example `…/dash-spv/testnet/`). Sync each created level's parent, up to the first ancestor that already existed.

**g. The probe's open order.** `sb14_hyb.rs:326-331` deletes out-of-range segments before its contiguity and tip-hash checks. §3 has the checks read-only and first, so the implementation must follow the doc. The counts are unaffected.

## What I checked

- **Schedules:**
  - every r3 schedule (N1, N2, N3 A and B, N4), re-run against rev 2.2's rules;
  - new schedules for the per-batch slot, run against both readings (f against b) and both sides of C.
- **N5:** checked line by line against `migration_loop`, `v3::migrate`, `resume_set_aside`, `set_aside` and `remove_v2_leftovers`.
- **N4 and N3 together:** checked against SB13's lease, fence and view order, the separate storage `RwLock`s, and the order of `DiskStorageManager::new`.
- **N3's barrier claim:** checked against `SqliteEngine::open` and `transaction`.
- **R1's orphan consequence:** checked against SB13's S1 oracle and dash-chain's `walk_down` and `leaves_from` contract.
- **The probe:** `res-22.txt`, every row, against §4. `run22.sh`. `sb14_hyb.rs`'s slot offsets and its open-time `tip` sync.

No branch edits, no commits, no pushes. I created no scratch files. `/work/scratch/sb14-design-r4-opus/brief.md` is the manager's and was left in place.

**Verdict: CHANGES REQUIRED** (R1 only; a rev 2.3 fixing R1 needs only a check of the lowering text, not a round).

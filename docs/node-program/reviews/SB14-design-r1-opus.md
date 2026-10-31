# SB14 SPV header layout — Opus-h design review, round 1

**Doc:** `docs/node-program/designs/SB14-spv-header-layout.md` on `node/sb13-spv-sqlite` @ **6ade190c8**
**Context read:** DEC-244; SB0 §4.4, §8.2, §9.5, §9.6; SB13 notes D45–D54; dash-chain `shared.rs` (cold lookups);
dash-spv `store/legacy.rs` (`put_headers`, `PutReads`, D51); dash-kv-sqlite `SqliteConfig`/`open`; perfgate
`README.md` (counters) and `workloads.toml`; the probe at `/work/scratch/sb13/sb14/probe` (source read in full).
**Reviewer scratch:** `/work/scratch/sb14-design-r1-opus` (0700).

## Verdict: CHANGES REQUIRED

(B) is the right shape among the options costed. A single WAL, packed active rows, a derived and verified
index, and no overlay keep SB0's crash model whole. The instruction projection for the DEC-244 rows is
plausible. But the design does not meet the merge bar that will actually be applied:

- perfgate gates `write_bytes`, `read_bytes` and `read_syscalls` on `spv-storage-store`, which has a v2 base.
  I measured (B) at 4.5× v2's write bytes, at 72 MB of reads against about 0, and at 17.7 k read syscalls against about 1. The doc says perfgate gates only user
  instructions.
- No SQLite-WAL layout, and not (A) as costed either, can bring those counters to ≤ v2. That needs a manager
  ruling, and the doc must ask for it (B1).
- Five major gaps also need fixing in the doc before implementation:
  - RSS margin accounting (M1);
  - an uncalibrated durable overhead (M2);
  - D51 being always off in the product (M3);
  - an unsound `hseg_ids` floor lookup (M4);
  - incomplete watermark and reorg memory rules (M5).

## Findings (by severity)

### B1 — BLOCKER. perfgate gates I/O counters on `spv-storage-store`, and (B) regresses three of them (§1, §8 "Kernel instructions")

**Doc claim (§8).** "perfgate gates user instructions; `write_bytes` on `spv-storage-durable` has no base row."

**Fact.** perfgate's README (Counters table) gates these on every bench row unless the row lists them as
`ungated`: `instructions`, `instructions_total`, `allocs`, `alloc_bytes`, `syscalls`, `fs_syscalls`,
`read_bytes`, `write_bytes`, `read_syscalls`, `write_syscalls`, `page_faults` and `peak_rss_kib`.
`workloads.toml:74` ungates only `syscalls` and `write_syscalls` for `spv-storage-store`. That row has a v2
base, and under (B) it does all the WAL work, because without an overlay, store and durable differ only by
`stop`'s sync (the doc's own D9).

**Measured** (strace over the existing probe binaries; 260 k headers in 13,000-header calls, then stop; the
generator subtracted; K=128, B0=65,536, T=8, P=120):

| counter (gated?) | v2 (`DiskStorageManager`) | (B) | (B) + check | (B), 2,000-header calls |
|---|---|---|---|---|
| write bytes (**gated**) | 29.1 MB | 131.6 MB (4.5×) | 131.6 MB | 138.2 MB |
| read bytes (**gated**) | ≈ 0 | 72.4 MB | 86.7 MB | 68.7 MB |
| read syscalls (**gated**) | ≈ 1 | 17,674 | 21,182 | 16,783 |
| write syscalls (ungated) | ≈ 58 | 48,793 | 48,793 | 52,262 |
| fsync + fdatasync | 27 | 45 | 45 | 62 |
| fs_syscalls-like total (**gated**) | ≈ 247 | ≈ 151 | — | — |

The reads are the WAL checkpoint reading frames back (`mmap_size = 0`, `lib.rs:229`). SB13 D54 already
recorded 273 MB written by r5's durable row against v2's 27 MB, but DEC-244 does not rule on it.

**Why no tuning fixes it.**
- Under WAL every committed page is written twice (the WAL, then the checkpoint). v2 writes 112 B per header
  once.
- So a WAL store meets `write_bytes` ≤ v2 only below about 56 B per header. That is less than the 80-byte
  header itself.
- Deferring the checkpoint out of the measured region (autocheckpoint off, checkpoint at close) would move the
  bytes outside the region. That games the bench; `instructions_total` catches the instructions but no byte
  counter would.
- (A) as costed (162-byte records in files, plus `hidx` in SQLite) also misses: about 42 MB of files plus the
  index's WAL bytes and checkpoint reads.
- The only layout that matches v2's bytes is v2's: 112-byte files, plus a committed-tip marker, plus a
  non-persisted index rebuilt at open (B-mem on files). That reverses SB0 §4.4's O(window) open and §8.2's
  "no whole-chain index", which are design targets rather than tracked metrics against v2, and it needs a DEC
  of its own.

**Required.**
1. §1 lists every gated counter of every `spv-storage-*` row with a v2 base (store, persist, get, revindex)
   and gives (B)'s projection for each. get and revindex look favourable: v2's `get` reads about 1 MB per call
   and `revindex` opens and closes a file per lookup.
2. §8's sentence is corrected.
3. The doc asks the manager for an explicit ruling, with these numbers, between:
   - (i) a DEC that re-bases or ungates `read_bytes`, `write_bytes` and `read_syscalls` on the SPV store
     rows. The justification: v2 had no crash-consistent commit, and SB0 §9.5 makes the single WAL a
     requirement. Disk and write volume would be recorded in a BL;
   - (ii) the v2-format-files layout with an in-memory index, with SB0 §4.4/§8.2 waived for SPV.

   Without (i), (B) cannot merge under the current gate, whatever its instruction counts.

### M1 — MAJOR. The peak-RSS margin is mis-accounted, and the bench's case has about 1.4 MiB headroom (§4, §4.1)

- **v2 includes its stack; (B) does not.** "The product adds the same async stack to both" is wrong.
  - The v2 figure (16.2 MiB) comes from the real `DiskStorageManager` through the async API.
  - The `gen` baseline that is subtracted creates the same tokio runtime, so v2's 16.2 includes v2's whole
    product path above the runtime.
  - (B)'s 12.9 / 14.8 MiB is the bare layout: no `SpvStore`, no lease, and no legacy record building.
- **The bench pays the check.** The bench's headers are unlinked, so its case is the checked one: 14.8 against
  16.2 MiB.
- **The product adds transient buffers.** The legacy path keeps, per 13,000-header call:
  - `Written.rows`, 13,000 `(BlockId, HeaderRow)` values, about 2.3 MB;
  - `PutReads`;
  - the call's input copy.

  That is a plausible 2–4 MiB of transients, enough to cross DEC-244's literal "peak RSS ≤ v2". perfgate's
  4 MiB floor for the tokio storage benches might still pass it, but DEC-244 sets the stricter rule.
- **Required.**
  - A D-row that streams each legacy call in ≤ 2,048-header sub-batches inside its one `WriteSet` (E8's
    streaming), building records straight into row images. The probe's 2,000-header calls peaked at 11.4 MiB
    against 13.2, for +2.7 % user instructions (0.606 G against 0.590 G).
  - The doc states which rule decides peak RSS: DEC-244's ≤ v2 or the gate threshold.

### M2 — MAJOR. The durable row's product overhead is not calibrated, and the bench's legacy path may keep a per-header hash lookup (§4 "What the product adds", §8 "Record building")

- **The overhead was never calibrated.** get and rev were calibrated (r5 in the probe against perfgate);
  durable was not.
  - The "≤ 0.60 G" is D50's "earlier split". It was a subtraction on an older code version, on a linked chain,
    in 32,768-header flush transactions with `known_new = true`.
  - The probe's r5 build cannot calibrate durable: it writes only `hdr`, `hdr_hash` and `active`, with no
    record building.
- **The bench path may keep a lookup the projection omits.**
  - The bench is unlinked, so `PutReads::load` (`legacy.rs:90-130`) runs with `known_new = false` and
    multi-gets every hash of the call in `hdr_hash`. D51 measured about +2.2 G per 260 k on a populated table.
  - On (B)'s empty `hdr_hash` it is still about 1–2 k instructions a key (an estimate, not measured), which is 0.3–0.5 G at 260 k. That
    alone takes most of the 8 % margin (0.12 G).
  - The doc never says that `PutReads`' hash leg is replaced by the `hidx` check.
- **Most likely DEC-244 row to regress:** `spv-storage-durable` (setting B1's counters aside), then peak RSS
  (M1).
- **Required.**
  1. The doc states that on (B) the legacy path's only hash lookups are the stored-header check (L0 and runs)
     and the parents' ids. The `hdr_hash` leg runs only when a durable off-chain count is above zero, and a
     `meta` counter decides that without a probe.
  2. **Cheap pre-implementation probe** (under an hour): on the SB13 head, run the bench's 260 k unlinked
     headers in 13,000-header calls through the real `header_batch` with the engine's `write` replaced by a
     no-op (or with every table put but one removed). Add the async boundary, which D52/D54 bound at about
     0.02 G. That measures record building plus reads directly; replace the "≤ 0.60 G" with it.
  3. If the margin falls under about 5 %, take §8's `GetBlockProof`-per-`nBits` cache, or the slim record
     (§ Item 7 below), before implementing.

### M3 — MAJOR. D51's shortcut is always off in the product, so the "linked" row (≤ 1.19 G) is not the product's cost (§2.1 Writes, §4)

- **D51's premise fails in the product.** D51 requires `meta.next_id` to equal the active count
  (`legacy.rs:331-338`). Every off-chain record breaks that for good:
  - the anchor pack's context records (always present in an anchored store, §2.1);
  - any record a truncation or reorg moved off-chain.

  So the product's linked sync pays the `hdr_hash` leg per header for its whole life. D51 measured
  2.79 G against 0.57 G per 260 k. No perfgate row shows it: `store_warm` uses a store with no off-chain
  records.
- **(B) allows a cheaper argument.** Take a run H1…Hn that links to the packed tip T, on a store without the
  `unlinked` marker:
  - A packed duplicate is impossible, by D51's argument.
  - An off-chain duplicate Hi = R implies R.prev = H(i−1). For i = 1, R is an off-chain child of T, which one
    `hdr_prev(T)` lookup finds.
  - For i > 1, R's parent H(i−1) would be stored, which is impossible by induction. The exception is a record
    whose parent is not stored: a context record, which lies below the anchor and so below T.

  One point read per call therefore replaces 13,000 lookups.
- **Required.**
  - Restate the shortcut for (B) as "linked run, no `unlinked` marker, and no off-chain child of the tip with
    the run's first hash".
  - Keep the full check only for unlinked runs.
  - Add a probe or bench case with off-chain records present (a context record, or after a truncation).

### M4 — MAJOR. The `hseg_ids` floor lookup misses packed ids when runs nest, which reads as corruption and discards the store (§2.1 Invariants, D5)

- **The failure.** `by_id` takes the single floor run (the greatest first id ≤ id). Runs can nest when a
  legacy overwrite re-packs a middle subrange:
  1. Run R0 = (first id 0, height 0, length 1000).
  2. Overwrite height 500 with a new header (id 2000; run key 2000).
  3. Overwrite height 500 back with the original header. It is stored, so `PutReads` reuses id 500, and
     `put_ids` inserts key 500 with length 1.
  4. Now `by_id(700)` floors to key 500, finds no coverage, and falls through to `hdr`. Record 700 is packed
     only (the "never both" invariant), so the result is "missing record for an issued id": SB0 §4.4's
     corruption, which trips the latch and discards the store.

  A run put at an existing key with a shorter length (the probe's `put_ids` overwrites the key) loses coverage
  the same way. The legacy path is test-only in the product, but the dashd integration and unit tests use it,
  and the invariant as written ("maps every packed id") is false.
- **Required.**
  - Define the run maintenance: either an insert trims or splits overlapping runs, or the lookup walks down
    the keys ≤ id until a run covers id and the record's id matches. Runs are few.
  - Add a property test against `mem_store` for nested and colliding re-packs.

### M5 — MAJOR. The watermark and reorg rules leave a lost-index case and unbounded memory (§3 "Reorg below the watermark", §2.1 Writes, §4.1)

1. **Lost entries after a crash.**
   - §3 lowers the watermark for a reorg. §2.1 says a legacy truncation or overwrite is handled "as a reorg",
     but §3 never extends the rule to it.
   - The case: a truncation below W that does not lower W, then appends at those heights (in L0), then a
     crash before a spill.
   - On reopen, L0 is rebuilt only from heights ≥ W, so the new headers between the truncation height and W
     are in no index. That is a real `by_hash` miss: a duplicate is stored, and locator and mnlistdiff lookups
     fail.
   - **Required:** every `WriteSet` that rewrites `hseg` below the watermark (a reorg, a legacy overwrite or a
     truncation) lowers it to that height in the same `WriteSet`.
2. **L0 grows with the reorg's depth.** A reorg at depth d reloads L0 with tip − f entries, unbounded by B0,
   and so does an open after a crash that follows it.
   - SB0 §4.4 tests reorgs deeper than 10,000 blocks; at 100 k, L0 alone is about 2 MiB, plus the map's growth.
   - **Required:** L0 ≤ B0 always. A reload above B0 spills at once, with the normal spill rule, and the open
     path does the same.
3. **The reorg buffers its whole depth.** "The packed records at heights ≥ f are read and written to the
   off-chain tables" buffers about 160 B a record read, plus about 400 B of batch arena a record (six rows):
   about 56 MB at a depth of 100 k.
   - **Required:** stream the move in ≤ W-row batches through E8's lease, so memory is O(W), not O(depth).
4. **Crash tests** for each of the three.

### m1 — The hidx merge's ordering against the E8 lease, and its latency (§3 Merges, D10)

- **Ordering.**
  - The chunked page commits are separate transactions, so they cannot run inside the step's
    `BEGIN IMMEDIATE`.
  - State that the spill decision is made before `BEGIN` (L0 plus the step's count) and that the chunks
    commit under the lease before it.
  - State that the in-memory LSM state (the L0 drain, levels, fences, watermark) is published only after the
    step commits. The probe mutates it before the write.
- **Orphaned runs.** The orphan from a crash or failed commit is always run `meta.next_run`, which is
  persisted only at the switch. So open deletes that run's range, and no scan is needed.
- **Latency.**
  - At mainnet, one step can merge about 2.4 M entries: about 20 k pages, with WAL and checkpoint traffic of
    about 40 MB.
  - The merge's snapshot pins the WAL, which grows to about 80 MB until the checkpoint.
  - Record the worst-case step latency on mobile; subscriber `WriteSet`s queue behind it.

### m2 — Unlinked packed chains break `children_from` and `walk_down` too (§2.1 HeaderStore, D7)

- The adjacency rules assume linkage. On the bench's unlinked shape, where every header shares one `prev`:
  - `children_from(P)` returns 1 of 260,000 children;
  - `walk_down` by height follows non-parents.
- D7 covers only `leaves_from`. Either extend it (scan or refuse under the marker) or state that these
  methods are unreachable under the marker, with a debug assertion.

### m3 — Reorgs to a shorter branch and truncations (§2.1 Writes)

- **Rewrites.** Specify that the `hseg` and `hseg_hash` rows above the new tip are deleted, and that both
  tail rows are trimmed, in the same `WriteSet`.
- **Open check.** Open checks that `meta`'s tip matches the tail row's length (corruption otherwise).
- **Verify bound.** The verify must bound h ≤ tip, as the probe's `h < next` does. The different row
  boundaries (30 against 128) need nothing more, as long as both tables are rewritten from their own row
  containing f.

### m4 — D8's discard of r5 stores cannot work as specified

- `SqliteEngine::open` refuses a database that holds tables outside the schema (`UnknownDatabase`,
  `lib.rs:252-256`). An r5 store has `active`, so (B) fails to open it rather than discarding it, and
  `meta.schema` is dash-chain's shared `CHAIN_SCHEMA` = 3.
- **Required:**
  - A layout version, so (B) can tell an r5 store apart.
  - A discard path keyed on the recognised r5 layout. A foreign file must never be deleted.
  - A test.

### m5 — D9's startup comparison is not a gate

- §1 quotes v2's open (1.285 G) from the probe. perfgate cannot judge a row that has no base.
- Land `storage/disk/open` on integration first: a bench-only change on v2, which perfgate's base-bench
  overlay then carries. Otherwise "startup ≤ v2" is checked by hand.

### m6 — Migration from v2 (§5)

- **Crash safety.** It is crash-safe only with a completion marker in the final Durable commit and with v2's
  files removed after it (SB13 D15's barriers). State that it reuses them.
- **Linkage.** The migrator should verify linkage so that it runs on the shortcut (M3). Otherwise each large
  batch scans every run: O(N²/C), which is still a few G at 2.4 M with C = 13,000, but avoidable.
- **Cost.** Memory is bounded (L0 ≤ B0, plus merge buffers under 1 MiB). Time and I/O are O(chain): projected
  at 2.4 M, about 18 G instructions including record building, and about 1.2 GB written (131.6 MB × 9.2).
  Record both; D46's blocking `dash_spv_ffi_client_new` is still the BL candidate.

## Answers to the brief's scope

1. **Meets DEC-244?**
   - **Instructions.** Plausibly yes on get (61.6 k against 903 k), revindex (40.8 k against 52.9 k; 56 k
     against 97 k at mainnet) and startup.
   - **Durable.** Plausible but unproven: the ≤ 0.60 G is a subtraction, not a measurement. Run M2's probe;
     M2's `PutReads` leg could take the margin alone.
   - **WAL checkpoints.** Inside the row: autocheckpoint runs on the committing thread, and perfgate's
     counter inherits. The close's final checkpoint falls after the region (D52's drop), but the probe counts
     it, so the probe is conservative.
   - **The bench's check.** The bench's unlinked check does reflect the bench. It does not reflect the product
     after the first off-chain record (M3).
   - **Beyond DEC-244's rows.** The real gate fails on `write_bytes`, `read_bytes` and `read_syscalls` (B1).
2. **Mobile RSS.**
   - **Steady state is within budget.** L0 is about 1.2 MiB, the fences 80 KiB, the caches 4 MiB × (writer
     + 1 reader), and the merge buffers under 1 MiB. That is about 11.6 MiB at 2.4 M on mobile.
   - **Two paths are unbounded today:** the L0 reload after a deep reorg, and the reorg's off-chain move
     (M5).
   - **WAL.** Growth during merges is disk, not RSS (the shm index is about 160 KiB).
3. **Crash safety.**
   - **(a), (c) and (d) hold.** The tail row and its partial row are rewritten whole in the step's
     transaction. A reorg rewriting a row's tail is atomic. A crash mid-step loses the step, and chunk
     commits that survive a power-loss WAL prefix are an unnamed run (m1).
   - **(e) holds only once L0 is bounded (M5).**
   - **(f)** needs the cases from M5 and m1.
   - **"Stale entries are harmless".** Sound: every candidate across L0 and all levels is verified with a
     full 32-byte compare at h ≤ tip, and nothing returns before the verify, so a stale entry cannot shadow a
     real one. The only miss risks are entries never written (M5.1) and the id runs (M4).
4. **Lookups.**
   - **Collisions across the 30-hash rows.** None: the verify is exact.
   - **Packed ↔ off-chain moves.** Both directions commit with the index state (the watermark lowered, L0
     reloaded) in one `WriteSet`. That is correct once M5.1 holds.
   - **Id stability.** Kept, apart from M4.
5. **Migration.**
   - **From v2:** bounded in memory; crash-safe with the completion marker (m6).
   - **From r5:** discarding is acceptable, but D8 as written fails on open (m4).
6. **Disk 1.9× and kernel instructions 3–6×.**
   - **Kernel instructions** are not gated (`instructions` is `:u`).
   - **Disk size** has no perfgate counter, and NFR-RES-02 is the node's metric 11. It is still 476 MB
     against 257 MB at mainnet on phones.
   - **`write_bytes`, `read_bytes` and `read_syscalls` are gated (B1).** The design must ask for the DEC.
     Fold the disk footprint into the same ask.
7. **(A)'s rejection** is sound against (B) on the crash model. (A)'s fsync per step, or the anchor rewind of
   subscribers under SB0's blind recovery, is a real cost. But (A) does not rescue B1 either, unless it also
   drops the persisted index. **A simpler layout with more margin:** the slim packed record (below). It does
   not fix B1, which WAL doubling still sets, but it widens every instruction and byte margin. Worth adopting
   if the M2 probe lands near the bound.

## Item 7 — the slim packed record (probe)

This is (B) with `hseg` holding only the 80-byte header per record. The hash stays in `hseg_hash` only, and
the id, chain work, skip, `time_max`, status and kind are derived:
- the id from height → id runs;
- the chain work from a per-row base plus `GetBlockProof` per `nBits` (or a base every 16 records);
- skip from the ids;
- `time_max` from a per-row base;
- status from its active default, with exceptions kept sparse.

Same probe and binary, the two variants built together. Medians of 2 runs each, which repeat to within
0.1 %.

| 260 k headers | (B) as proposed | (B) slim | Δ |
|---|---|---|---|
| durable, u | 0.591 G | 0.532 G | −0.059 G (−10 %) |
| durable, u, check forced | 0.732 G | 0.670 G | −0.061 G |
| durable, k | 0.45 G | 0.34 G | −0.11 G |
| store RSS, with the check | 14.4–14.7 MiB | 12.5–12.6 MiB | about −2 MiB |
| `get`, u (record only) | 31.0 k | 26.5 k | −4.5 k, but the product adds a `hseg_hash` read and the work derivation |
| rev, u | 22.5 k | 22.5 k | 0 |
| disk | 53.4 MiB (1.92× v2) | 37.5 MiB (1.35× v2) | −30 % |
| write bytes | 131.6 MB | 97.3 MB | −26 %; still 3.3× v2 |
| read bytes / read syscalls | 72.4 MB / 17.7 k | 53.6 MB / 13.1 k | still ≫ v2 |

**Reading.**
- Slim raises the unlinked durable margin from about 8 % to about 12 % of v2.
- It brings the checked-case RSS headroom from about 1.4 to about 3.5 MiB (M1).
- It cuts disk and mobile write volume by a quarter to a third.
- It does not fix B1: WAL doubling keeps every byte counter well above v2.
- **Its cost:** derivation code, and the work derivation on `get`. `get` has about 840 k of headroom
  against v2, so 128 `GetBlockProof`s (about 0.1–0.3 M) or a base every 16 records both fit.
- **Recommendation:** adopt it if M2's probe leaves under about 5 % margin; otherwise take it as a recorded
  follow-up.

## Probes run (reviewer)

- **I/O counters:** `/work/scratch/sb14-design-r1-opus/io.sh` (strace over the existing `probe-v7.bin` and
  `v2.bin`, plus the rebuilt probe; read and write families, fsyncs, opens). Each run uses a fresh
  directory. Results are deterministic: two repeats were identical to the byte. The generator's own calls
  (17 reads, 1 write, 6 opens) are subtracted in B1's table.
- **fs_syscalls-like count:** strace `-c` over opens, stat-family calls, access, rename, unlink,
  (f)truncate, fallocate, getdents64 and the fsync family. (B): fstat 50, fsync 45, openat 16, ftruncate 13,
  newfstatat 10, unlink 3, mkdir 1, statx 1. v2: mkdir 59, statx 30, rename 28, fsync 28, openat 8, fstat 5,
  access 1, unlink 1.
- **Skip-target reads per call** under packed rows (`skip.py`): 92 distinct `hseg` rows over 260 k in
  13,000-header calls, and 4,912 over 2.4 M in 2,000-header calls. About 0.2 G over a mainnet sync, which is
  negligible: skip ids resolved through `hseg` are not a hidden cost.
- **Slim-record probe:** a copy of the probe with `SLIM=1`, which stores only the 80-byte header in `hseg`
  (hashes stay in `hseg_hash`). Results are in the section above.

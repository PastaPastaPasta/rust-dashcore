# ST3b S0 review, round 1 (Opus)

Scope: `f5a05edbf` (feat(dashd-node): accept -dbcache and -dbbatchsize) and `3a9d6b5a1`
(test(rdashd): compare the coins-setup refusal, not the exit status), on `node/st3b` at head
bdcd1d28e. The S0 files are unchanged between 3a9d6b5a1 and bdcd1d28e (`git diff --stat`
is empty for node.rs, sut.rs, init/caches.rs, dashd_v24.rs and flush/machine.rs), so the
line numbers below are valid at either commit. Core reference: v24.0.0-rc.4.

## Verdict: ACCEPT-WITH-NITS

M: 0 · m: 2 · n: 4

The two options parse as Core parses them, and the `-dbcache` split is `CalculateCacheSizes`
line for line. `-dbbatchsize` reaches every place that sizes a coins write in both node modes,
and no default overrides it. A split flush keeps Core's marker-first, best-block-last order, with
a WAL barrier before the final batch. I found no correctness defect. Both minor findings are test
gaps: neither tier-1 test would notice a regression on the real `Node`/RocksDB path.

## Findings

### m-1: 3a9d6b5a1's refusal check is narrower than what the race allows; compare stderr instead

- **rdashd:** `rdashd/tests/dashd_v24.rs:504`, where
  `(stderr(&out).contains("is not supported by rdashd"), lines)`. The status check at :495 still
  accepts `Some(0 | 1)`.
- **Core:** `init.cpp:2569-2580` and `validation.cpp:3823-3830`, the `-stopatheight` race. The
  same file's `stopatheight_minus_one_stops_at_genesis` (`dashd_v24.rs:371`) already asserts
  that **stderr is empty for both binaries in both branches** of that race (exit 0 after "Done
  loading", or exit 1 after "Shutdown requested. Exiting.", which goes only to debug.log).
- **The gap:** the new tuple tells "accepted, stopped at genesis" apart from the one refusal
  text only. Any other init failure exits 1 and prints `Error: …` on stderr, and if it comes
  after the cache lines are logged, the case still passes. Examples: "Error opening block
  database" because a `-dbcache` value broke the RocksDB config, or a `db_batch_size` error.
- **Surviving mutant:** in `dashd-node/src/node.rs`, just after the refusal check at :265-267,
  add `if args.is_arg_set("-dbcache") { return Err(InitFailure::Error("x".into())); }`. rdashd
  then logs the identical cache lines and exits 1 with `Error: x`, and the test passes for all
  five `-dbcache` cases.
- **Fix:** return `stderr(&out)` itself, or `out.stderr.is_empty()`, instead of the
  `contains` bool. That is race-free by the :371 evidence. It also catches the refusal (which
  is on stderr) and every other init error. The doc comment at :469-470 can stay.

### m-2: no tier-1 test sees `-dbbatchsize` on the real `Node` path, and the ST3b harness does not check that D-IBD-S splits

- **rdashd:** `dashd-node/src/node.rs:261` and `:310`. The only behavioural test is
  `dashd-node/src/sut.rs:1326` (`dbbatchsize_splits_the_coins_flush`), which runs `NodeSut`
  over `MemoryTuning` (`sut.rs:350`, `|_| Ok(coins)`). The rdashd case `&["-dbbatchsize=200000"]`
  (`dashd_v24.rs:520`) checks only that the option isn't refused.
- **Core:** `txdb.cpp:117`, `:152`.
- **Surviving mutant:** at `node.rs:310`, replace `batch_size` with
  `dashd_chainstate::tuning::DEFAULT_DB_BATCH_SIZE`. That changes `open_node`'s sub-batch, the
  RocksDB coins write buffer, the coins budget's R and W's `FlushConfig`. Tier 1 stays green: the
  sut test builds its own `Opening::on`, and the dashd_v24 case logs the same lines.
- **Why it matters for ST3b:** ST3b-D9 says D-IBD-S exists to split coins flushes
  (`-dbbatchsize=20000`, 54 partial batches in the baseline). I searched `dash-testkit`,
  `rdashd/tests` and `contrib/node` at bdcd1d28e for `partial`, `coindb` and `dbbatchsize` and
  found no assertion that a D-IBD-S run wrote a partial batch, or any `-debug=coindb` that would
  make one visible. With the mutant above, or any later regression that drops the value
  (e.g. someone "simplifies" `open_node` back to a constant), the power-loss nightly would
  silently test only unsplit flushes and still pass C2.
- **Fix, either or both:**
  - (a) Add an rdashd-level test, using `rpc_diff.rs`'s `generatetoaddress` pattern or a
    node.rs test with RocksDB: `-debug=coindb -dbbatchsize=1000`, mine past the LEFT_IBD flush,
    and require ≥ 1 "Writing partial batch" in debug.log, with 0 at the default.
  - (b) Have the S3 baseline run D-IBD-S with `-debug=coindb` and fail if the count of
    partial batches is 0. (b) is the one that protects the harness's purpose.

### n-1: the `coins/8` cap makes the effective `-dbbatchsize` diverge from Core's in both directions (documented)

- **rdashd:** `dashd-chainstate/src/tuning.rs:27-29` (`S = min(dbbatchsize, max(coins/8, 64 KiB))`).
- **Core:** `txdb.cpp:117`, `:152`. There is no cap; `batch_size` is the raw `size_t`.
- **Concrete inputs:**
  - `-dbcache=4` (coins share 1,605,632 B with txindex on, so `coins/8` = 200,704 B) at the
    default `-dbbatchsize`: Core never splits below 16 MiB, but rdashd splits at about 196 KiB.
  - `-dbbatchsize=-1`: Core's `SIZE_MAX` never splits, but rdashd still splits at `coins/8`.
- **Status:** this is ST3b-D1 and CH2 §2.5, so it's design, not a defect. Two small asks:
  - The commit message says "read as Core reads it", which is true of the parsed value but not
    of the behaviour. The doc comment on `db_batch_size` (`init/caches.rs:42-45`) already says
    so, which is enough.
  - Make sure no ST3b check compares rdashd's "Writing partial batch" count with dashd's. D9's
    54 is an rdashd-only number.

### n-2: `-dbbatchsize` now also sizes the RocksDB `coins` memtable, which RocksDB floors at 64 KiB

- **rdashd:** `tuning.rs:60-67` (`coins_write_buffer: sub_batch`, three buffers) and
  `dashd-store/src/rocks.rs:634-640`.
- **RocksDB 11.8.1:** `db/column_family.cc:237`. `ClipToRange(write_buffer_size, 64 KiB, …)` and
  the arena block size `min(1 MiB, wbs/8)` (:243).
- **Concrete input:** at D-IBD-S's `-dbbatchsize=20000`, or at `0` or `1000`, the coins
  column family runs three 64 KiB memtables instead of 16 MiB or `coins/8`. A W-small IBD then
  flushes memtables to L0 every few sub-batches. That means many tiny SSTs, more compaction, and
  possibly L0 at the stop trigger (36, DEC-164 retry loop).
- **Assessment:** this is not a correctness problem. It is arguably good, since it puts more
  memtable-flush and compaction interleavings under the power-loss harness. But it means
  D-IBD-S exercises an engine regime the default never runs, which is not what D9 describes.
  Note it in ST3b-D9 so a slow or stalled D-IBD-S run isn't misread later.

### n-3: the split decision counts B1's non-coin records, so "one batch iff it never passes S" is not Core's criterion (pre-existing)

- **rdashd:** `dashd-chainstate/src/flush/machine.rs:944-951`. `take_entries` starts from
  `b.size_bytes()` of B1, which already holds the index records, file infos, evo state and
  `evo_tip`.
- **Core:** `txdb.cpp:138-139`, `:152`. The first batch holds only the `DB_BEST_BLOCK` erase,
  the 64-byte `DB_HEAD_BLOCKS` marker and the coins.
- **Concrete input:** `-dbbatchsize=1000`, and a flush of ten new headers' index records
  (about 1.2 KB) with 500 B of dirty coins.
  - Core writes one batch.
  - rdashd writes B1 (the marker, no tip) Durable, then Bk (the coins and the tip): two durable
    writes with a marker-only state between them.
  - The "partial batch" log count is still 0 on both sides (SubBatch(0) takes the rest into Bk).
- **Assessment:** crash-consistent, since recovery replays from the marker, and outside S0's
  diff. But the comment at :944 overstates the parity. Suggest:
  "Core: txdb.cpp:152; here S is passed by B1's records plus the coins, not the marker plus the
  coins".

### n-4: a non-string, non-number `-dbbatchsize` in settings.json fails at init in rdashd, but at the first flush in Core

- **rdashd:** `init/caches.rs:47-50`, read at `node.rs:261` and `sut.rs:339`. `get_int_arg`, then
  `setting_to_int`, then `get_str()?` gives an `InitFailure`.
- **Core:** `txdb.cpp:117` reads it inside `BatchWrite`. `SettingToInt` throws there, and
  `FlushStateToDisk` turns that into `AbortNode("System error while flushing: …")`
  (`validation.cpp:3123`) after a normal start.
- **Concrete input:** settings.json `{"dbbatchsize": [1]}`.
- **Assessment:** this is a hand-edited settings file, so it's cosmetic. Leaving it as is
  (failing earlier) is fine; record it in ST3b-D1 if parity notes are meant to be exhaustive.
  The command-line and config-file inputs all match:
  - non-numeric gives 0;
  - overflow saturates to `i64::MAX`/`MIN`;
  - `-no…` gives 0;
  - negative values wrap through `as usize`.

## What I checked (by reading; I built and ran nothing)

1. **Parsing and clamps.**
   - `-dbcache` goes through `CacheSizes::calculate_capped` (`dashd-chainstate/src/caches.rs:74-114`).
     I checked it line by line against `node/caches.cpp:15-46` and `txdb.h:31-47`:
     - `wrapping_shl(20)` matches C++20 `int64_t << 20`;
     - the clamp comes after the shift, as in Core;
     - all constants match, including `nMaxTxIndexCache` 1024, `max_filter_index_cache` 1024,
       and the address/timestamp/spent caps;
     - the `/64` for the timestamp index matches;
     - the filter division matches;
     - `coins_db = min(total/2, total/4 + 8 MiB, 8 MiB)` matches.
   - I traced `-dbcache=4/0/-1/100000/2^44`. 2^44 goes through the wrap to 0 and then the 4 MiB
     clamp. That distinguishes wrap from saturate (saturate would give 16,384), and the test's
     case catches a clamp-before-shift mutant.
   - `-dbbatchsize`: `get_int_arg` uses `locale_independent_atoi` (`dash-args/src/strencodings.rs:14-28`).
     I compared it with `LocaleIndependentAtoi` (`util/strencodings.h:120-145`): trim, the `+-`
     case, saturation and invalid input → 0 all match. `as usize as u64` is Core's `(size_t)`
     cast, including on 32-bit.
   - Help: `dash-args/src/options/generated.rs:31` is `ALLOW_ANY | DEBUG_ONLY` with "default:
     16777216", which matches `init.cpp:613`.
   - Neither option has an error text in Core, and rdashd adds none.
2. **Split order** (`flush/machine.rs`):
   - B1 at :972-973 puts `head_blocks = (new, old)` and erases `tip`, Durable.
   - SubBatch(i) at :653-686 writes coins only, Buffered. The crash roll comes after the write,
     matching Core's `-dbcrashratio` placement.
   - PreBkBarrier at :688-691 runs `sync_wal` when `sub_batches > 0`.
   - Bk at :700-736 writes the last chunk plus `tip = new` and erases `head_blocks`, Durable.

   This matches `txdb.cpp:138-139` and `:167-168`. A crash between sub-batches leaves no `tip`
   and a `(new, old)` marker, which is the replay input Core expects. Every sub-batch takes at
   least one entry before testing `> S` (`take_entries` :1315-1326), so `-dbbatchsize=0`
   makes progress, as in Core. Recovery's replay (`recovery/replay.rs:517`) uses the same S
   with a non-empty guard.
3. **Wiring:**
   - `Opening::on` (`dashd-node/src/chainstate.rs:270`) is the only place S is computed for a
     node.
   - It feeds the RocksDB open (`:202-204`), `RecoveryConfig.sub_batch` (`:288`),
     `Chainstate.sub_batch` (`:338`), the coins budget's R (`:425`) and W's `FlushConfig` (`:453`).
   - Its two callers, `node.rs:305` and `sut.rs:350`, both pass `db_batch_size(args)`.
   - No `cfg`/feature gate changes the path; `test-hooks` touches only `wal_fault`.
   - `-dbcrashratio` stays refused (`support.rs:112`).
   - Core's second use, `MigrateOldIndexData` (`txdb.cpp:532`), has no rdashd counterpart.
     That's fine, because a non-default `-addressindex`/`-spentindex`/`-timestampindex` is
     refused (IX2).
4. **Tests:**
   - `db_batch_size_is_a_size_t` catches default, zero, wrap and negation mutants.
   - The sut test catches sub_batch min/max swaps and an ignored argument in NodeSut, but not
     node.rs wiring (m-2).
   - Its `> 1` would not catch a `>`→`>=` boundary mutant. That boundary is pre-existing and is
     covered by the oracle-backed flush tests.
   - The dashd_v24 cases catch clamp-constant and shift mutants, but only when `find_dashd()`
     finds a binary. Otherwise the test silently returns, a pre-existing pattern.
   - 3a9d6b5a1's diagnosis of the race is right (it's the `stopatheight_minus_one_stops_at_genesis`
     race), but its replacement check is too narrow (m-1).
5. I relied on the implementer's evidence (tier 1 at 67e89b7bd, bench inputs, the 54-batch C2
   baseline) without rerunning it. I made no copy of the worktree and ran no build, test or
   privileged command.

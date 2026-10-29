# CH2a a3 (Store) — Sol round 2

**Reviewed:** `node/ch2a-coins`, exact head `e5ffe1f7ae053a6a7a951d0932032d74a7420c80`, against round-1 head `4aebd006ffe97344e31386591292ee2e4ddc8b8a`. The reviewed worktree was clean before and after. No branch edits, commits, pushes, GitHub comments or delegation. This report is uncommitted in the integration worktree.

**Result:** the S1 fatal-path defect is closed. S2 and S4 are closed, and S3's two behavioral coverage gaps are closed. One new, nonblocking minor finding concerns cleanup on the C-API test helper's panic paths. There are no blockers or majors; the Sol implementation gate is satisfied.

**Contract and scope:** I read REVIEW-COMMON and my `CH2a-a3-r1-sol.md` first, then inspected the five actual fix commits and the implementer's `/work/scratch/ch2a/r1/report.md`. I applied the code-review-validator workflow directly. PLAN §7: codenames only; no private inputs or private root opened. BL-72's accepted custom-tuning/CH2b obligations remain out of scope and unchanged.

**Evidence:** `/work/scratch/ch2a-a3-r2-sol/`. Every build/test/probe command used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and `TMPDIR=/work/scratch/ch2a-a3-r2-sol/tmp` (mode 700). Cargo used the reviewed worktree's own target; no `CARGO_TARGET_DIR` was set. Standalone Rust probes linked its existing artifacts. No full gate or private campaign was run. Temporary datadirs were my own; the fixture generator stopped every dashd it started. The scratch TMPDIR was empty at completion.

**New finding R2-S1 — minor: the C-API control leaks native resources when its error assertion panics.** Location: `dashd-store/src/rocks.rs:1083-1085`, called at `:1103` and `:1117`; cleanup is only at `:1119-1124`.

On a DB-open error, `check(err)` panics while the cache, write-buffer manager and options are raw pointers with no Drop guards. None is destroyed, and RocksDB's malloc-allocated error string is not freed. A write error likewise bypasses cleanup of the open DB, batch and write options, in addition to those earlier objects. This is test-only and does not invalidate either behavioral discriminator or affect the production engine's RAII ownership.

**Independent proof:** `ffi-probe.rs` copies the helper, changes its FFI alias to wrappers that count native destructor calls, and directs its DB path through a nonexistent parent for the error case. A real RocksDB open error followed by `catch_unwind` records **0 destructor calls**. Successful strict and non-strict runs each record **6 calls**. The C implementation confirms `SaveError` uses `strdup`, and `rocksdb_free` is the matching deallocator. See `run-store-probes.py`, `raw-helper-original.rs` and `store-probes.log`. The write-error leak follows directly from the second assertion preceding the same cleanup sequence; I did not inject a native write I/O failure.

**Suggested fix:** give each C object a small Drop guard immediately after creation, with the DB closed before its dependent resources, and copy/free a non-null C error before raising the assertion. Preserve the current successful-path destroy order. Add a targeted open-error check if retaining this helper. The specific requested claim of *no C-object leaks on panic paths* is therefore not satisfied, despite sound success-path ownership.

**S1 — closed.** `dashd-chainstate/src/store/abort.rs:46` establishes `AbortOnUnwind` before formatting or either logger call. Its Drop at `:62-63` contains only `std::process::abort()`: no formatting, logging, fields to drop, or other fallible work. The ordinary path also explicitly aborts. The fallible stderr write at `:51` preserves the second log call on an output error.

I recompiled my unchanged round-1 `abort-probe.rs` against the new production module, then added supplemental stream/guard probes. **All 28 cases terminated with SIGABRT**, never return or caught unwind, under both `panic=unwind` (unoptimized) and `panic=abort` (optimized). Cases cover a panic at either logger call, a `println!` logger into a disconnected stdout pipe, a returning logger, disconnected stderr, stderr closed at exec, stderr closed after Rust startup, and a panicking panic hook. Every call has an outer `catch_unwind`. Both Core log lines were observed with broken and closed stderr; closing after startup avoids relying solely on std's reopening of fd 2. Evidence: `run-abort.py`, `abort-probe.log`.

Scratch negative controls also discriminate: removing the guard lets each logger panic/broken-stdout case reach the outer catch and exit 0; restoring `eprintln!` with the guard retained still aborts but loses the second log line on broken stderr. Finally, an intentionally modified scratch guard whose Drop itself panics still terminates with SIGABRT when unwinding from either logger panic (a double panic), and cannot defeat the explicit abort on the returning-logger path. Both panic strategies were checked. This injected destructor is not the production implementation. Evidence: `check-abort-mutations.py`, `abort-mutations.log`. All committed abort child tests also passed.

**S2 — closed; the i386 fixture is Core's output.** The generator copies the pinned `node/caches.cpp` and `node/caches.h` verbatim. Its regex preserves `txdb.h`'s `sizeof(void*) > 4 ? 16384 : 1024` expression, rather than substituting a chosen cap. It extracts the remaining constants/defaults and stubs only ArgsManager/translation dependencies. Every supplied boolean is 0 or 1, and the decimal integer grid is within i64, so the stub's parsing covers these inputs. The host stub must equal the full-link Core probe before generating the i386 grid. The Docker path uses `--platform linux/386`, verifies `getconf LONG_BIT = 32`, and compiles/runs the same C++ source with g++ inside that container.

I ran the actual generator, unmodified:

```sh
python3 contrib/node/cache-sizes/gen.py \
  --core /work/home/agent/workspace/dash-wt/oracle-shim-v24.0 \
  --dashd /work/chains/bin/dashcore-24.0.0-rc.3/bin/dashd \
  --out dashd-chainstate/tests/data --check
```

It reports **the recorded vectors match** for all three files (`gen-check.log`). Source validation against `v24.0.0-rc.3` and the host/full-link equivalence check succeeded as part of this run. The dashd SHA-256 matches the committed log fixture (`e204e316…7ab5bb4`). I used that binary only with fresh scratch datadirs, without touching shared instances.

My independent round-1 C++ probe, after rechecking its caches.cpp/h against the pin, still matches **1,872/1,872 64-bit rows**. A separate arithmetic check matches **1,872/1,872** rows of each fixture. The Rust grid test selects its own cap from cfg and first asserts `MAX_DB_CACHE == OWN_MAX_DB_CACHE`, so a wrong production constant cannot select a matching fixture. Both grids are exercised. The 64-bit dashd log fixture deliberately remains a 64-bit calculation on either host width.

Standalone copies of `caches.rs` pass all **4 tests** both unchanged and with the pointer-width selections simulated as 32-bit. Ignoring the supplied cap fails the grid and direct-cap tests; making the host production cap 1,024 fails the independent cfg assertion and log test. See `check-cache-grid.py`, `cache-grid.log` and `cache-*.log`. The simulation is not a native i686 Rust execution; the new fixture reproduction is an actual i386 C++ execution.

**S3 — behavioral gaps closed, with the cleanup finding above.** The committed tests passed against the production engine. My copied raw-C control independently observes **524,462 B** in the strict 1 MiB cache and **4,457,927 B** in the non-strict one. Thus the beyond-capacity assertion distinguishes strict capacity from non-strict capacity.

Success-path FFI ownership is correct: the WBM retains a shared cache reference; options retain a shared WBM reference; the DB owns its copied options. Batch keys/values are copied during insertion. The DB closes before options, WBM and cache destruction; write options and batch can be destroyed first. `CStr::from_ptr` is evaluated only for a non-null error, so the successful path does not form a CStr from null. The separate native constructors really select strict/non-strict LRU in RocksDB 11.8.1's `db/c.cc`.

The stall test exceeds the 1 MiB WBM threshold in one write, then pins the memtable before the next write's preprocessing can flush it. `MemTable::~MemTable` releases the WBM charge; the iterator prevents that destruction. `PreprocessWrite` schedules the WBM flush, then `ShouldStall()` plus `no_slowdown` yields `Incomplete("Write stall")`. Turning `allow_stall` off makes the same write succeed. I checked those native paths and the implementer's actual `allow_stall=false` mutation: the test fails at `expect_err("a stall")` because the write succeeds, while the cache control remains green. This mutation was inspected, not replanted in the reviewed branch.

**S4 — closed and independently discriminating on both engines.** The committed snapshot test passed. I reused my round-1 `LiveMulti` wrapper, which changes only snapshot `multi_get` to read the live engine, and copied the new assertion function into a standalone probe. Healthy Memory and Rocks engines pass; wrapped Memory and Rocks both fail specifically at **"the snapshot's multi_get"**, after the snapshot point-read assertion succeeds. The probe substitutes valid distinct inline P2PKH coin amounts for the branch's helper; the assertion function and key schedule are copied unchanged. The committed test uses the original coins. Creation, overwrite, deletion, duplicates, missing keys and the live-read control are all exercised. Evidence: `snapshot-probe.rs`, `store-probes.log`.

**Scope and reported checks verified.** The actual `git diff 4aebd006 e5ffe1f7a --stat` is **10 files, +2,396/−52**, including the two new stub headers and the fixture (rather than the report's "7 changed tracked files" phrasing). The five commits match the supplied sequence. Production changes are the abort protection and extraction of the cache cap parameter; RocksDB and coins-table additions are tests, tuning changes are documentation, and the final commit changes the CH2 status note. No unrelated a3 implementation changed. See `full-fix.diff`, `diff-stat.txt`, and the empty before/after status receipts.

- `cargo test --locked -p dashd-chainstate -p dashd-store --all-features --lib -- --test-threads=4`: **116 + 54 passed**, 0 failed; 1 existing store test ignored (`targeted-tests.log`).
- Targeted Clippy with `--locked` and `-D warnings`: **all six reported configurations independently pass**—both crates all-features lib/tests; chainstate model/alloc-tag/planted-bugs lib/tests; store model lib/tests; chainstate minimal lib; both crates all-features all-targets; both crates default all-targets (`clippy-*.log`, `clippy-sets.log`).
- `cargo fmt -p dashd-chainstate -p dashd-store -- --check`: **pass** (`fmt.log`). This is the scoped formatting check, not a fresh workspace gate.
- The implementer's mutation rounds A/B contain the stated assertion failures for all six mutants. The saved three-file mutation baseline equals the final source byte for byte (`implementer-evidence-check.log`). I independently replayed the abort and cache mutants in scratch, and the snapshot live-read wrapper on both engines; the production `allow_stall=false` mutation is corroborated by its recorded failure and the native-path inspection. Copies of the original mutation logs/script are retained as `implementer-*` evidence.

The new minor cleanup finding does not reopen the fatal-path requirement or the accepted BL-72 deferral. S1's required SIGABRT negative controls are now satisfied.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes

# PF0 review r1 (Opus): deterministic-counter per-PR gate

- **Branch:** `node/pf0-counter-gate` @ `0a592529` (13 commits on `59ecb763`; `node/integration` is now `2ded7d4f`, so the branch itself is stale).
- **Scope:** `contrib/node/perfgate/` (harness, policy, criterion shim, LD_PRELOAD counter, node wrapper), `dash-spv/benches/storage.rs`, `docs/node-program/PROCESS.md`.
- **Read against:** A1 §3.11, PLAN row PF0 and M0 exit 9, DEC-23/26, the memory note `pf0-counter-gate`, and the author's evidence in `/work/scratch/pf0/final/`.
- **Reviewer runs:** `/work/scratch/pf0-review/` (state roots `rootA`, `rootB`; logs and reports under `logs/`). Throwaway commits only: none on any branch, nothing pushed.

## Verdict: CHANGES-REQUIRED

The measurement core is good. The shim, pinning, ASLR-off, the fixed environment, per-package builds, the shim-marker refusal and the trimmed policy make the 16 pure workloads repeat to ~1e-5 %. I reproduced the planted-regression FAIL and the no-op PASS (numbers below).

The gate still fails open in two ordinary, non-adversarial ways that I reproduced, and in a third by design:

1. a PR branched before the current `node/integration` head is judged against a base it doesn't contain (B1);
2. a PR that also edits the bench it regresses is judged by its own edited bench (M2);
3. on any workload whose base isn't bit-stable, a regression that also adds spread is NOISY and passes (M1).

The program rule is "any regression blocks merge", and these three must close before PROCESS makes the gate mandatory. All three are small changes.

## Reproduced checks

All runs used the harness at `0a592529` from a scratch worktree, `--repeat 5`, `--jobs 4`, under load averages of 60–160 on 32 vCPUs.

| run | base → head | verdict | flagged | wall |
|---|---|---|---|---:|
| no-op, cold worktrees | `0a592529` → `4ab7f440` (doc comment in `siphash24.rs`) | **PASS** | 3 NOISY, all Δ 0.000% (`spv-storage-store` read_bytes/read_syscalls, `spv-storage-get` syscalls: head spread only) | 2540 s (builds 648 + 975 s, cargo package-cache lock contention at load 100–160; measuring 324 s) |
| planted (P1), warm base | `0a592529` → `b57a3b28` | **FAIL** | only `siphash-single-16k` instructions **REGRESSION +0.830%** (base noise 1.1e-05%) | 919 s (head rebuild 620 s; measuring 270 s; 12 of 19 base workloads from the cache) |
| no-op again, warm | `0a592529` → `4ab7f440` | **PASS** | 2 NOISY, Δ 0.000% | 264 s |
| stale base (B1) | P1 `b57a3b28` → P2 `98b282fa` (both off the tip) | **PASS** | nothing: `siphash-single-16k` 199.066M on both sides | 25 s (`--only 'siphash-*'`) |
| the true merge (B1) | P1 → P1+P2 `d9ff167c` | **FAIL** | `siphash-single-16k` +0.823% | 6 s |
| bench input change (M2) | `0a592529` → P1 + `LEN` 25→24 `286d0dca` | **PASS** | `siphash-single-16k` instructions **IMPROVED −1.660%**, no note | 109 s |

- **The plant.** `v2 ^= core::hint::black_box(0xffu64)` in the scalar `siphash` finalization, a different line from the author's plant: +0.83% instructions on `siphash-single-16k` (3,948,561 → 3,981,329 per iteration).
- **The no-op.** A doc-comment edit in `hashes/src/siphash24.rs`, so `dashcore_hashes` and every dependent rebuilds on the head.
- **Stale base (B1).** P1 and P2 are two independent ~0.8% plants on the same function. P2 is parented on the tip, not on P1. P1 plays the integration head, and P2 plays a PR branched before P1 merged.
- **Bench input (M2).** The head is P1 plus `const LEN: usize = 24` (was 25) in `hashes/benches/siphash.rs`.

## Findings

### B1 (blocker): the gate compares the PR head with the base, not the merge result. A stale PR hides its regression (reproduced)

`perfgate/cli.py:97-111` (`run_gate`) measures `--head` against `--base` as given. Nothing checks that the base is an ancestor of the head. If `node/integration` has moved since the PR branched, the delta is "PR changes − integration changes since the fork point".

- **Failure (reproduced).** The integration head contains an approved regression P1, for example a correctness fix the manager accepted. A PR cut before P1 adds a regression P2 of the same size. The gate says PASS: `siphash-single-16k` reads 199.066M on both sides, Δ 0.000%. The tree that would actually be merged (P1+P2) is +0.823%, and the gate FAILs it when given that tree.
- **Mirror image.** An integration improvement that landed after the fork shows up as a false REGRESSION on every stale PR.
- **How common it is.** This is the normal case, not a corner. The PF0 branch is itself 1+ integration commits behind (`59ecb763` vs `2ded7d4f`). Its own vs-integration evidence used an older integration head (`276685fc`).

**Fix.** Pick one:
- (a) ERROR unless `git merge-base --is-ancestor <base> <head>` holds, and tell the implementer to rebase;
- (b) synthesize the merge (`git merge-tree --write-tree base head` → `commit-tree`) and measure that as the head. ERROR on conflicts.

(b) is what the manager actually merges with `--no-ff`. Either way, record the measured head tree in the ledger. At merge, the manager checks that the ledger's tree equals the merge commit's tree.

### M1 (major): NOISY is non-blocking even when the head median moved beyond `allowed`. A regression that adds spread passes on any non-bit-stable base

`perfgate/gate.py:111-126`. The "moved median blocks" rule applies only when the trimmed base range is ≤ 0.01% (`STABLE_TOL`). On every other base, REGRESSION needs every trimmed head run above every trimmed base run. Anything else beyond `allowed` is NOISY, which never blocks. `tests/test_perfgate.py:93-96` codifies this: a median +8% with two overlapping head runs is NOISY.

**Probes** (`/work/scratch/pf0-review/probe/policy_probe.py`, using `gate.judge`):
- A base at 0.02% noise (`kwm-filter-scan-2000` instructions in the author's vs-integration run). A head median of +10%, with runs spread 0…+20%: **NOISY → PASS**. With a bit-stable base, the same head is REGRESSION.
- Which side of 0.01% a workload falls on changes between sessions. `kwm-filter-scan-2000` instructions measured 0.008% in one author session and 0.022% in another. Instructions on `spv-storage-store` and `spv-storage-revindex` are always on the noisy side, and so are most non-instruction counters of the storage benches and the allocations of `kwm-filter-scan`. So the verdict for the same regression depends on the day.
- **Why this isn't adversarial.** The regressions most likely to add spread are the ones the storage and node workloads exist to catch:
  - a new timer-driven or background task (dash-spv's 5 s persistence worker is one);
  - a std `HashMap` (per-process `RandomState`, so ASLR-off doesn't make it repeat) put on a hot path;
  - lock contention.

**Fix.** Make "|Δ median| > allowed without separation" a blocking status, for example INCONCLUSIVE. The gate then re-runs that workload automatically, with more interleaved repeats (for example 15 per side), and decides with a one-sided rank test (Mann-Whitney, p < 0.01) plus the effect-size threshold. That keeps the author's false-positive protection for random-input workloads, which mostly fail a rank test at n = 15. It also stops passing real shifts. The cheaper alternative is to fix the inputs: seed `kwm-filter-scan` the way `f60a48e2` seeded revindex (see m4), and keep NOISY blocking.

### M2 (major): the head's own bench sources decide its verdict. A one-character input change turns a regression into IMPROVED (reproduced)

The harness and `workloads.toml` come from the checkout that runs the gate. The bench targets (`<pkg>/benches/*.rs`, `[[bench]]`, dev-dependencies, `[profile.bench]`) come from the head. `gate_notes` (`perfgate/cli.py:82-94`) only looks at `contrib/node/perfgate`.

- **Failure (reproduced).** Head = P1 (+0.83% real regression) plus `LEN` 25 → 24 in `hashes/benches/siphash.rs`. Result: `siphash-single-16k` instructions IMPROVED, −1.660%, verdict PASS, no note.
- **Same class.** Each of these shifts or shrinks the measured region and is equally silent:
  - moving work from `iter` into `iter_batched` setup;
  - dropping an element count;
  - `[profile.bench] lto = "fat"`.

The author's own revindex change (open item 1) is the benign version of the same thing, and was caught only because it happened to cost +3%.

**Fix (pick one or both):**
- (a) Have `gate_notes` diff `base..head` over the bench targets of every selected workload. Use `cargo metadata` `src_path` plus the package's `benches/` directory, `Cargo.toml` `[[bench]]`/`[dev-dependencies]`, and the workspace `[profile.bench]`. Any change gives a blocking `BENCH-CHANGED` row that needs the manager's recorded approval.
- (b) The stronger option: build the head's library code with the base's bench sources overlaid. Input changes in a PR then can't move the gate at all. A bench that no longer compiles against the head API is an ERROR that needs approval.

### M3 (major): the "run it from the integration worktree" rule is only a note. Nothing checks that the running harness is the base's harness

`gate_notes` emits notes when the head changes `contrib/node/perfgate` or the running harness is dirty. The verdict and exit code are unchanged, and the ledger records PASS. Running from the PR's worktree is the default an implementer reaches for, and the proposed PROCESS step 3 asks implementers to run it. A PR that deletes or loosens the workload it regresses then gets a PASS (with a note) in the ledger. So does a PR that edits `MIN_REL` or `trim`. The same goes for `--only`, `--workloads FILE` and `--base X`. They produce ledger lines with verdict PASS that don't record the selection, the workloads file or the harness hash (`cli.py:114-118`).

**Fix:**
- ERROR (exit 2) unless the running harness tree equals `git ls-tree <base> contrib/node/perfgate` and is clean. Allow an explicit `--dev-harness` override, which the ledger marks as non-authoritative.
- Write `only`, the workloads-file hash, the harness hash and the measured head tree into every ledger line.
- Make a subset run's verdict read `PASS (partial: N of M workloads)`.

### M4 (major): the 0.5% per-PR allowance accumulates. There is no drift guard against a fixed anchor

The base is always the previous integration head (A1 §3.11 says so literally). So ten PRs at +0.45% on the same workload all pass and compound to +4.6%. The program rule is "never slower than dashd; any regression blocks merge", and per-PR slack is only safe if something watches the sum. The ledger has the data, but nothing reads it.

**Fix:** add an anchor comparison, for example `--anchor <milestone tag or last approved anchor>`. Its base is fully cached because 16 of 19 workloads are bit-stable, so it costs only the head runs already being made. Use the same policy. A cumulative drift beyond max(1%, policy) blocks until the manager either approves it, which moves the anchor, or files a fix. This needs a manager decision because it goes beyond A1's literal rule. I recommend it.

### M5 (major, readiness, not blocking this merge): the node-workload plan doesn't gate CPU work

The plan is to gate node workloads by allocations, page faults and RSS. That is sound as a **memory** gate and unsound as a **CPU** gate. In the author's dashd data (`/work/scratch/pf0/final/dashd-24.json`):
- Page faults repeat to 0.03–0.11%. They measure first-touched memory, not instructions.
- Allocation **counts** are not as stable as the README says ("0.06–1.7%" is alloc_bytes): allocs vary 6.2% (startup), 5.7% (gbt) and 0.46% (mempool-accept) after trimming.
- With instructions at 5.7–47% trimmed noise, `allowed = 2 × range` makes the instruction rows 11–95% wide. A purely algorithmic regression in mempool accept, GBT or RPC, with no new allocations or touched pages, is invisible.
- Untrimmed, one `startup` run did 4.5G instructions, 3.3M allocs and 64k read syscalls, against a median of 1.0G, 354k and 794. That's a different code path, not timer noise. node-bench's startup workload needs a look before anything is gated on it.

**What would make node workloads gateable:**
1. **Count only the measured phase.** Use `perf stat --delay=-1 --control fifo:…` with node-bench sending `enable`/`disable` around the op stream. That excludes startup and shutdown, where most timer work happens.
2. **Count per thread.** `perf stat --per-thread`, or perf counters opened by name on the work threads (`d-httpworker.*`, `d-msghand`, or rdashd's named workers). Gate the work threads and ignore the scheduler and timer threads.
3. **Make rdashd deterministic by construction.** The program controls rdashd, unlike dashd. Give it a bench mode: mock time, no periodic timers, single-threaded executors (A1 already asks for `-par=1`). dashd numbers are only ever a milestone ratio ("a ratio above 1 … must be explained"), so dashd's noise isn't the gating problem. rdashd's noise is, and it hasn't been measured.
4. **Size op counts so work dominates timer work.** Use more repeats with the rank test from M1.

Until then, don't describe node-workload instruction rows as gated: make them `gated=False` per workload kind. Keep allocs, alloc bytes, page faults and RSS as the blocking counters.

### m1 (minor): workload coverage is thinner than A1/PLAN asks

- 8 of the 10 `hashes-*` workloads measure `bitcoin_hashes` (re-exported since #1108). Only a `Cargo.lock` bump can move them.
- X11 (`hashes/src/hash_x11.rs`, in-repo and on every header) has no bench or workload.
- PLAN "dash-spv means dash-spv's benches plus `spv-headers-*`": no header-sync replay workload exists, and A1 says the microbenchmarks and `spv-headers-*` are what apply until rdashd runs.
- The cachegrind/iai fallback is a PLAN PF0 deliverable and isn't delivered. That's acceptable while the PMU works, but record it as a deferral in STATUS.

### m2 (minor): the peak-RSS 4 MiB floor ungates RSS on small benches

`gate.py:60`. On a ~3 MiB bench, RSS can grow +130% (3 → 6.9 MiB) and still be OK (probe S4). The allocation counters usually catch the cause, but not mmap-backed growth.

**Fix:** keep 4 MiB only where the base itself shows page-granular steps (rng_b ≥ 1 MiB). Otherwise use max(2%, 256 KiB).

### m3 (minor): a cached base can come from a different stack layout

`measure.py:105-111` fixes the environment "because the environment's size moves the stack with ASLR off". But `PATH` and `HOME` are inherited and not in the fingerprint. `measure <rev>` caches results under the same `cache/<sha>/` key from the `measure` worktree, and a later gate's `base` reuses them. The executable path and cwd there are 3 bytes longer than in `base`/`head`. So a cached base and a fresh head can differ in stack alignment. That's harmless for today's pure benches. It matters for anything that hashes or orders by address.

**Fix:**
- Put `PATH`/`HOME` into the fingerprint, or set them to constants.
- Run every bench through a fixed-length path, for example a hardlink at `$ROOT/run/<side>/bench`.
- Don't let `measure` results seed the gate cache.

### m4 (minor): `kwm-filter-scan-2000` still uses a random mnemonic

Its allocations vary 0.38%, which puts that counter permanently on the NOISY side of M1. Seed it the way `f60a48e2` seeded revindex. The PR that does it will need the same manager approval.

### m5 (minor): the shim counts only Rust heap allocations, only inside the region

- C/C++ allocations (secp256k1/blst scratch, and RocksDB once ST1's store lands in benches) bypass `GlobalAlloc` and are invisible in bench mode.
- Work deferred past `region.end()` is never counted, because the process `exit(0)`s right after reporting. That includes outputs dropped by `iter_batched`, background tasks and the dash-spv persistence worker.

**Fix:**
- Document both limits in the README.
- Consider a gated `instructions_total` (from `__init` to exit) with a looser threshold, so deferred work can't vanish.
- Preload the C-allocator counter in bench mode as well.

### m6 (minor): PLAN's "< 20 min per PR" holds only with warm worktrees

My cold first run took 42 min. Most of it was cargo, with two builds blocked on the shared package-cache lock at load 100–160. A head that touches `hashes` took 15 min, and a warm no-op 4.4 min. The author saw up to 20 min cold.

**Fix:** after every merge into `node/integration`, run `run measure node/integration --label base` (or a `gate --warm` mode) in the background. The base `target/` and the base cache are then hot when the next PR arrives. If measure's results are to seed the gate cache, keep the worktree label `base`, because of m3.

### n1 (nit): the report's policy line is wrong

`gate.py:232-234` and the PROCESS proposal both say a regression needs "every trimmed head run worse than every trimmed base run". On bit-stable bases a moved median is enough, which is the more important rule. Print the actual rule.

### n2 (nit): the PROCESS.md proposal edits a stale copy

The branch's `docs/node-program/PROCESS.md` predates integration's base-sdk and "Node checks" lines. Integration's copy is the live file through the `node-program` symlink. The hunk next to "Once they exist" will conflict. Rebase the branch, which B1 requires anyway.

### n3 (nit): the revindex bench shares `rng` with `get`

Under real criterion, the revindex heights depend on how many iterations `get` ran. That's irrelevant under the shim, which runs one bench per process. A separate `StdRng::seed_from_u64(SEED + 1)` per bench makes criterion runs reproducible too.

## Build isolation (checked)

- **Per-package builds** (`build.py:359-385`): one `cargo bench -p <pkg> --bench …` per package, so feature unification matches `cargo bench -p`. Correct. Note that this is not the feature set the node binary will build with. That's inherent and fine.
- **Target dir.** `CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET_DIR` are stripped, `--target-dir <wt>/target` is explicit, and every artifact must resolve inside its worktree. Correct.
- **Head-only cargo config.** A head `.cargo/config.toml` or `rust-toolchain.toml` changes `cargo_config`/`rustc` in the fingerprint and gives ERROR. That fails closed. Good. `[profile.*]` in the root `Cargo.toml` is not covered (see M2).
- **Refusal of non-shim binaries** (`uses_shim`, marker string). An uncovered criterion version (0.6/0.7), a renamed harness or a non-criterion bench all give ERROR, through either "linked against crates.io criterion" or "wrote no result". A tracked `Cargo.lock` is moved onto the shim by `cargo update -p criterion@…` (conservative update). Correct.
- **Pinning.** `taskset` to the highest CPUs, one exclusive CPU per running process, `RAYON_NUM_THREADS=1`, `setarch -R`. Correct, with one documented consequence: under this gate, parallelising code adds instructions and shows no benefit, so such PRs will need the approval path.
- **Missing runs.** A missing or renamed bench ID on the head, or a counter missing on the head, gives ERROR. A base error gives ERROR. Base missing or pending gives SKIPPED/NEW. Correct.

## Open items

### 1. The revindex bench change (`f60a48e2`): approve

- **It's justified.** The unseeded `rand::random` heights made the workload's base noise 0.8% instructions and 1.2% syscalls (vs-integration run). The seeded draw drops that to ~0.02%, which lets 0.5% apply. The +3.2% instructions and +3.0% syscalls is the cost of the specific 500 heights StdRng(42) produces (+60 `openat`/`read`/`lseek`/`close` sets). It's not a product change: storage code is identical on both sides.
- **Conditions:**
  - record it in STATUS as the first approved bench-input change, citing the ledger line;
  - merge it before, or together with, PF0, so the new base is seeded from then on;
  - apply n3 if convenient.
- **The pattern from M2 matters more.** Approving this one input change is easy. What needs fixing is that no unapproved one can slip through.

### 2. `/work/perfgate` as the state root: approve, with conditions

It's persistent tool state like `/work/bench` and `/work/chains`, not scratch, so it belongs outside `/work/scratch` (DEC-26 cleanup), on disk (597 G free). Conditions:
- **Record it in DECISIONS** (owner: the manager), including that `git worktree prune` is needed if it's ever deleted. The gate registers `worktrees/{base,head,measure}` in the main repository.
- **Budget it.** Two or three full rust-dashcore `target/` trees: a bench-only `target/` is ~1.2 GB per side here (2.5 GB per root), fine on 597 G free. Add a size check or a `clean` subcommand.
- **Version the ledger.** `ledger.jsonl` is the per-PR counter history PLAN relies on, and it's unversioned. Append each merge's ledger line to STATUS, or to a committed `research/PF0-ledger.jsonl`.
- **One gate at a time.** Only the manager's runs use the default root. Implementers who run it locally set `PERFGATE_ROOT=/work/scratch/<slug>/perfgate`, so they don't contend for the flock or the base cache. This also removes the "who runs it" ambiguity in item 3.

### 3. The PROCESS.md gate section: approve after changes

- **Move the gate from step 3 to step 6.** Step 3 ("Implement … done when … the PF0 counter gate passes") puts the run in the implementer's worktree. That's exactly the configuration M3 shows to be weak. Make it a **merge** gate: the manager runs it from the integration worktree on the exact merge candidate (B1), and step 6 cites the ledger line. Implementers may run it as advice, under their own `PERFGATE_ROOT`.
- **State the policy as implemented:**
  - the moved-median rule for bit-stable bases;
  - what NOISY means, and that it blocks after M1;
  - that ERROR (exit 2) also blocks, and isn't "not a performance verdict" in the sense of being ignorable.
- **Add:** "a PR that changes a gated bench's inputs, harness or `[profile.bench]` needs the manager's recorded approval" (M2).
- **Clarify the last bullet.** "A task that adds a benchmark adds its one-line workload in the same PR" works. That workload is NEW, so it's not gated for that PR, and the edit trips the "changes perfgate" note.
- **Rebase onto the live PROCESS.md** (n2).

## Node-workload readiness (question 5)

No, not as planned. Allocations, page faults and RSS are reliable at 0.03–1.7% (alloc counts up to 6%). But they're memory signals, and gating on them alone leaves CPU regressions in node code ungated. See M5 for the four changes that would let node workloads carry a CPU gate. The decision should wait until rdashd's own noise is measured. dashd's noise only bounds what a C++ node with many timer threads does.

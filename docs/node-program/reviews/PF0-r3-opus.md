# PF0 review r3 (Opus): closure check of the deterministic-counter gate

- **Reviewed:** `node/pf0-counter-gate` @ `824c3a09` (merge-base with `node/integration` = `2cfa1792`), in a fresh detached worktree `/work/scratch/pf0-r3-opus/repo`.
- **State root:** `/work/scratch/pf0-r3-opus/perfgate` (never `/work/perfgate`). `TMPDIR=/dev/shm/pf0-r3-opus-tmp`, `CARGO_BUILD_JOBS=8`, load average 66–82 throughout.
- **Read against:** REVIEW-COMMON, `PF0-r1-opus.md`, `PF0-r2-gpt.md`, the manager's rulings 1–7, DEC-52, and the implementer's evidence in `/work/scratch/pf0/`.
- **Rules kept:** no branch edits, commits, pushes or posts. Throwaway heads were made with git plumbing (`acceptance.commit`) and are on no branch.

Paths below are relative to `contrib/node/perfgate/` unless they start with a crate name.

## What I ran (evidence under `/work/scratch/pf0-r3-opus/`)

| check | result | evidence |
|---|---|---|
| unit tests, ruff | 79/79 OK; ruff clean | (console) |
| `tests/acceptance.py` on the new head: a full 15-run anchor at `824c3a09`, then all seven scenarios | 7/7 as expected (table below) | `acceptance.log`, `acceptance/<scenario>/` |
| GPT r2 policy probes (`probes.py`), re-pointed at the new code | escalation, exact-p and missing-anchor probes now behave as ruled | `probes/r2probes.log` |
| exact Mann-Whitney against brute-force enumeration | 400 random tied cases, max error 0 | `probes/mw_check.py` |
| GPT r2 `state_writer` binary, and 13 more escape probes, inside `sandbox.wrap` | every write or reach-out refused; ledger untouched | `probes/sandbox-probes.log` |
| can the measured process hide work from the external counters? (C probe) | `perf stat` is robust; strace is not | `probes/ext/` |
| four boundary plants through the real `run gate` (`--only siphash-batch-16k`), plus a cached gate-aware variant | see §2 | `r3-scenarios.log`, `r3-scenarios/<name>/`, `scripts/r3_scenarios.py` |
| calibration with the anchor held fixed: 40 anchors × 400 trials | see §3 | `calib-fixed-anchor-1{1..4}.log`, `scripts/calib_fixed_anchor.py` |

**Acceptance on `824c3a09`** (my root; anchor 1173 s cold, from a reflinked copy of the implementer's base `target/`):

| scenario | verdict | blocking rows |
|---|---|---|
| planted (P1) | FAIL, exit 1 (884 s) | siphash-single-16k: instructions +1.245%, instructions_total +1.244%, instructions_process +1.209%. Nothing else. |
| no-op | **PASS**, exit 0 (677 s, load ~80, no escalation) | none |
| stale base (B1) | FAIL | the merge P1+P2 at +0.820%, and DRIFT +2.075% against the anchor |
| bench input (LEN 25→24) | FAIL | BENCH-CHANGED (`hashes/benches/siphash.rs`); P1's +1.245% still shows |
| **r2 exploit: `build.rs` rewrites the bench** | **ERROR, exit 2** | BENCH-CHANGED (`hashes/build.rs`) and `(build) ERROR`: the script dies with "Read-only file system" |
| r2 exploit, write error ignored | FAIL | BENCH-CHANGED; P1's +1.245% still shows, so the bench input stayed intact |
| **r2 state writer: `build.rs` appends a forged PASS** | FAIL | BENCH-CHANGED; `ledger.jsonl` holds 7 lines with 0 `review_probe` rows |

## 1. Dispositions

### GPT round-2 findings

| # | finding | status | evidence |
|---|---|---|---|
| 1 | a build script can change bench inputs after the check | **CLOSED** | Builds run in `bwrap` with `/` read-only. Only `$ROOT/targets/<slot>` and a scratch dir are writable (`sandbox.py:76`, `build.py:500`). Every `build.rs` in the bench closure (even where absent on the base) is BENCH-CHANGED (`build.py:186`). Re-run: ERROR or FAIL as above, never a silent PASS. Latent gaps remain at the edge of the set: F3. |
| 2 | measured builds can write gate state | **CLOSED** | The root is bound read-only (`sandbox.py:56`). The reviewer's own `state_writer` gets EROFS inside the sandbox. The build-script variant leaves no ledger line. Home, `/run`, the network, PID/IPC and abstract sockets are private; systemd-run, docker, localhost TCP and `kill` of the gate all fail. |
| 3 | escalation keeps round-1 decisions | **CLOSED** | `gate.py:335`: every gated counter of an escalated workload uses `judge_escalated` on all runs. GPT's probe (instructions 1.000M ×5 then 1.015M ×10, escalated by allocs) went from PASS to REGRESSION / FAIL. |
| 4 | tied MW normal approximation | **CLOSED** | Exact DP over doubled mid-ranks (`gate.py:140`). It equals brute force on 400 random tied cases, and `[0]×15` vs `[0]×10+[1]×5` gives 0.02107 = C(15,5)/C(30,5). |
| 5 | false-FAIL on no-ops | **CLOSED** (conditional on the sessions; see §3) | Inputs were fixed (seeded filter-scan, storage worker stopped) and store's pool-driven syscalls ungated. Whole-procedure calibration: 0.205% (0.151–0.278%). My fixed-anchor variant: 0.200% mean, worst anchor 0.75%. A real no-op gate at load ~80 passed. |
| 6 | missing anchor coverage | **CLOSED** | A workload the anchor lacks, or a gated counter with no anchored median, is ERROR (`gate.py:369`, `:386`). GPT's probe went from UNANCHORED/PASS to ERROR. |
| 7 | ERROR row exits as FAIL | **CLOSED** | `verdict()` gives ERROR precedence (`gate.py:400`), and the exception path also writes ERROR / exit 2. Re-run: `buildscript` is ERROR / exit 2 with a BENCH-CHANGED row present. |

### Manager rulings

| ruling | status | notes |
|---|---|---|
| 1. read-only isolated builds, a wider BENCH-CHANGED set, refusal of toolchain/cargo-config heads | **CLOSED** | Verified as in finding 1. `toolchain_changes` refuses `rust-toolchain(.toml)`, any `.cargo/config(.toml)` and a symlinked `Cargo.lock` before any cargo runs (`build.py:470`; unit-tested). `cargo metadata/update/fetch` run outside the sandbox but execute no package code, and the build is `--offline --locked` with `RUSTC_WRAPPER=""`. Residual gaps are latent and minor (F3). |
| 2. state isolation, counters measured outside the isolated process | **State: CLOSED. Counters: PARTIAL** | `instructions_process` is external and robust: `prctl(PR_TASK_PERF_EVENTS_DISABLE)` and untraced children don't reduce it. strace is external, but the measured process decides what it sees (F1, reproduced in a real gate). The in-region counters are self-reported, as README says. The node path still counts inside the sandbox and makes `/work/bench` writable (F2). |
| 3. pooled decisions after escalation | **CLOSED** | as finding 3 |
| 4. exact Mann-Whitney | **CLOSED** | as finding 4. Nit: README says exact "up to 80 runs", but the code's `EXACT_MAX` is 400. |
| 5. false-FAIL < 1% on no-ops | **CLOSED** | §3 |
| 6. missing anchor → ERROR | **CLOSED** | as finding 6. Also, `no anchor at all` is ERROR (`gate.py:356`). |
| 7. ERROR precedence, exit 2 | **CLOSED** | as finding 7 |

### Round-1 carry-over (no change from GPT r2 unless noted)

- B1 closed and M3 closed (re-verified by `stale-base`).
- M1 is now closed (rulings 3–5).
- M2 is now closed (ruling 1).
- M4 is now closed (ruling 6).
- m4 is closed: the filter-scan mnemonic is fixed.
- m1 (X11 and header-replay coverage) and m5 (C/C++ allocations) remain recorded deferrals in README's Limits.

## 2. The measurement boundary: what a change under review can still move

The question: can a change under review move its own counts or verdict through channels that the BENCH-CHANGED set and the sandbox miss?

| channel | covered? | how / evidence |
|---|---|---|
| Environment variables, `RUSTFLAGS` | **Yes** | Runtime env is fixed by the gate (`measure.py:123`). Build env is an allowlist taken from the *operator's* shell and fingerprinted (`sandbox.py:30`, `measure.py:44`). A PR's routes to env or flags are all closed: `.cargo/config [env]/[build]` is refused, `rust-toolchain` is refused, `[profile.*]` and `[patch]` are BENCH-CHANGED, and a closure `build.rs` (`rustc-env`, `rustc-cfg`, `rustc-link-arg-benches`) is BENCH-CHANGED. |
| `include_bytes!`/`include_str!` of a file outside the declared set | **Yes for bench sources, by design not for library code** | Literal and `concat!(env!("CARGO_MANIFEST_DIR"), …)` includes in bench sources join the overlaid set (`build.py:117`). Anything else the bench crate read shows in its dep-info (`build.py:321`). Data a *library* includes under `src/` is product, measured as such; I found no non-test library include feeding a gated bench. Latent edges: F3. |
| proc-macro side effects | **Partly** | A proc macro runs in the sandbox and can't touch sources or state. But workspace proc-macro *sources* are not in the set, though they are compile-time code exactly like `build.rs`. `git-state` sits in dash-spv's bench closure; `bench_boundary` watches only nonexistent `git-state/src/test_utils*` paths, not `git-state/src/lib.rs`. A proc macro can also write anywhere in the head's `target/`, including the compiled objects the overlaid bench links. So "base bench sources" holds at the source level only. F3. |
| reads of `/proc`, `/sys` | **Not a write channel** (the sandbox gives a fresh `/proc` and read-only `/sys`). **Detection is unrestricted** | The process can see that it is being measured: `PERFGATE_*` env vars, the `/perfgate/` marker paths, `TracerPid` in syscalls mode, a private network namespace. See the gate-aware row. |
| measured process vs the **external `perf stat`** | **Robust** | Probe: `prctl(PR_TASK_PERF_EVENTS_DISABLE)` leaves the external count at 1,200,125,277 against 1,200,125,327 plain. A `CLONE_UNTRACED` child is still counted (inherit is not ptrace). Blind spot: work moved into the kernel (`instructions:u`). AF_ALG hashing and `io_uring_setup` both work inside the sandbox. |
| measured process vs **external strace** | **No** (F1) | (a) **Marker injection**, real gate: `siphash_batch` with one extra `getppid` per call gives syscalls 0 → 100, REGRESSION + DRIFT, FAIL. The *same plant* plus one `access("/perfgate/end")` on the first call reads syscalls 0 → **0** with **no blocking row** (INCOMPLETE only because of `--only`). The spans check passes, because an extra *end* adds no span. (b) **`CLONE_UNTRACED`**: a child making 1000 syscalls shows **0** lines under `strace -f` (a traced child shows 1001). |
| self-reported in-region counters (instructions, allocs, page faults, io, RSS) | **No, as documented** | PR code shares the shim's address space. It can rewrite `result.json` or `/proc/self/clear_refs`, interpose `syscall`, or allocate with raw `mmap`/`malloc`. Only `instructions_process` (1% floor) backstops them. |
| **gate-aware product code** | **No, and no counter design can fix it** | Real gate: `siphash_batch` doing a second pass over its inputs gives instructions +310%, FAIL. The same work guarded by `std::env::var_os("PERFGATE_MODE").is_none()` *per call* was caught, but only incidentally: `allocs` REGRESSION, since `var_os` allocates. Guarded once per process (one allocation, at the per-run floor), it reads instructions +0.002% with **no flagged row**. |

**What this means.** The gate now protects its *records and inputs* well: state, harness, bench sources, the build environment and the merge candidate. It does not, and cannot, protect against product code written to recognise the gate. For non-adversarial changes, none of F1's channels occur by accident. So F1 is mainly an accuracy problem in README and `sandbox.py`. It also leaves the threat model unwritten, and the manager should write it down (recommendation in F1).

## 3. Calibration method (`tests/calibrate.py`)

**Honest, as a conditional estimate.**

What it does right:
- Each trial runs the real decision code: `gate.compare(final=False)`, then `needs_escalation`, then `compare` on the pooled runs, then `drift_rows` against an anchor with 15-run medians and noise.
- It is a two-level bootstrap. A session is picked first, then runs within it, so differences between sessions (load) stay in, and a run's counter vector stays together.
- It covers every declared bench workload, including the 19 bit-stable ones.
- It mirrors `cli.run_gate` (checked line by line), and uses the shipped `ungated` declarations.

Caveats, in order of weight:
1. **Tails are truncated.** Draws are with replacement from 10–30-run pools, so no trial contains an event rarer than its pools did, and rates driven by rare bursts are underestimated. The 0.205% is the rate *of the observed populations*, not a bound.
2. **The anchor is fixed in reality, redrawn per trial in the script.** DRIFT accounts for about two thirds of the failure causes. So I held the anchor fixed: 40 anchors drawn as the script draws them, 400 trials each. Per-anchor false FAIL: mean **0.200%**, median 0.25%, p90 0.50%, max **0.75%**; **0/40 anchors above 1%**; non-DRIFT mean 0.131%. The 0.205% holds whichever anchor the manager records.
3. **Sessions are from load 22–70, before the final harness commit.** That commit only changed which counters gate, which the script applies. My no-op gate at load ~80 passed without escalating. That is one point, consistent with the estimate.
4. **The rate is per gate for today's workload set.** Each new non-bit-stable workload adds to it. Recommend making "re-run `calibrate.py` when a workload is added or ungated" a PROCESS step.

**Verdict on ruling 5:** met, with margin, for the populations measured.

## 4. Deviation rulings (recommendations)

1. **Seeded filter-scan** (fixed BIP-39 test-vector mnemonic, `key-wallet-manager/benches/filter_scan.rs`). **Approve.**
   - It is what r1 m4 asked for. Pool, UTXO and used-address counts still define the workload, and a fixed script set makes the BIP158 matching cost a constant rather than a random variable.
   - Record it as a bench-input change, as DEC-52(1) did. The first anchor after the PF0 merge covers it.
2. **Stopping the storage worker** in the store, get and reverse_index benches (`dash-spv/benches/storage.rs`). **Approve, with a follow-up.**
   - For get and reverse_index, `stop()` persists outside the region, so this is purely noise removal.
   - For store, the timed full `persist()` no longer runs in the region. Segment persistence by `store_headers` still does: about 26 fsync, 26 rename, 56 mkdir and 26.9 MB written per run.
   - So no gated workload now measures the 5 s worker's full `persist()`. Add a deterministic `storage/disk/persist` bench (explicit `persist()` in the routine) to the backlog.
3. **Ungated `syscalls` and `write_syscalls` for spv-storage-store.** **Approve as interim, with a follow-up (F4).**
   - The noise is real: tokio blocking-pool threads add 18-syscall bursts, and eventfd wake-ups add writes.
   - But the filesystem calls are exactly stable in every breakdown I checked: fsync 26, rename 26, openat 36, close 28, mkdir 56, statx 81. Ungating everything means an extra fsync per segment would pass this workload.
   - The `ungated` list lives in the base's `workloads.toml` and is validated (`spec.py:58`), so a PR can't ungate anything itself.
4. **Node-workload counters measured inside the sandbox.** **Accept only behind a guard (F2).**
   - Harmless today: no `rdashd` target exists.
   - But it activates by itself on the first PR that adds a `rdashd` bin, and the same path makes `NODE_BENCH_ROOT`, default `/work/bench`, writable to that PR's binary.

## 5. New findings

### F2 (major, latent but self-activating): node workloads give the PR's binary write access to `/work/bench`, and its counters are forgeable
- **Where:** `perfgate/node.py:117`, `:121`, `:126`, `:129`, `:133`; README:65.
- **The write grant.** `run_once` sandboxes node-bench and the node with `external=[nb_root]`, where `nb_root` is `$NODE_BENCH_ROOT`, default **`/work/bench`**. That tree holds the pinned dashd binaries (`/work/bench/bin/…`), `baseline/`, `results/` and `data/`. README:65 says each writable path "must resolve to a real directory under the root", but this one is outside it by construction.
- **Forgeable counters.** perf's, strace's and the preload counter's outputs, and node-bench's `manifest.json`, are all written into the sandbox-writable `tmp`. The gate reads them with plain `open()`, which follows symlinks, unlike the bench path's `read_regular`.
- **Trigger.** `measure_nodes` runs the head's `rdashd` whenever the head has that bin target, even if the base doesn't. So the gate of the PR that *introduces* `rdashd` runs it with `/work/bench` writable.
- **Failure.** A buggy datadir path in an early rdashd (or a hostile one) can overwrite the pinned dashd or the B1 baselines that milestone comparisons depend on. A lingering child in the PID namespace can also rewrite `c.perf`/`c.alloc.*` before node-bench exits.
- **Fix (small):**
  - Until node counting is external and node-bench gets a per-gate writable data root under `$PERFGATE_ROOT` (bind `/work/bench` fixtures read-only), make node workloads refuse to run on a PR binary. Report them as `pending` or ERROR with that reason, and add a unit test.
  - Read node outputs with `sandbox.read_regular`.
  - Fix README:65.

### F1 (minor; a manager decision on scope): the external syscall count can be cut by the measured process; README and `sandbox.py` over-claim
- **Where:** `perfgate/measure.py:53`, `:72`, `:129`; `perfgate/sandbox.py:19-20` ("cannot forge them"); README:90-92 ("Two counts can't be forged: strace's syscall log").
- **The over-claim.** The log can't be edited, but its region boundaries and its coverage are chosen by the measured process. Reproduced in a real gate: a 0 → 100 syscall regression is hidden by one injected `/perfgate/end` marker. `CLONE_UNTRACED` children are invisible to `strace -f`.
- **Why minor.** A deliberately gate-aware change passes anyway (§2), so F1 adds no capability for an adversary. It never occurs by accident.
- **Fix:**
  1. Correct the claims: only `instructions_process` is unforgeable, and the in-region counters plus the strace region are cooperative.
  2. Optionally, gate a whole-process syscall count (no markers) next to the region count, and deny `CLONE_UNTRACED`, `io_uring_setup` and `AF_ALG` sockets with a `bwrap --seccomp` filter. That restores an unforgeable syscall total.
  3. Ask the manager for a DEC on the threat model, for example: "PF0 defends against accidental regressions and against tampering with bench inputs, harness and gate state. Product code that detects the gate is a code-review matter." Add a cheap diff scan that raises a blocking `GATE-AWARE` row when non-harness code in the candidate mentions `PERFGATE`, `/perfgate/`, `perf_event`, `CLONE_UNTRACED`, `PR_TASK_PERF` or `TracerPid`. That catches the obvious form of the cached gate-aware plant.
  4. State in README that the sandbox is a write and network boundary, not a confidentiality one: the git dir, `/work/scratch` and `/work/bench` are readable from inside.

### F3 (minor, latent): edges of the BENCH-CHANGED set
- **Where:** `perfgate/build.py:180-196`, `:321-338`.
- **(a) Proc macros.** Workspace proc-macro crates in a closure are compile-time code, like `build.rs`, but only their manifests' build parts are watched. Today that means `git-state` for dash-spv. Add their `src/` to `build`.
- **(b) Build-script inputs.** Only the `build.rs` file is watched, not what it compiles in (`mod`/`include!`) or what it reads (`rerun-if-changed`). Read `target/release/build/*/build_script_build-*.d` and the `output` files.
- **(c) Dep-info staleness.** The dep-info half of the boundary comes from whatever the base slot last built, newest `.d` by mtime. It is stale or empty when every base result came from the cache after a different build. Fail closed: build the base benches when no `.d` matches the base.
- **(d) Runtime reads.** Files a bench reads at run time from its package dir aren't watched.
- **Today.** None affects the gated benches: no closure `build.rs` uses helpers, no bench uses `#[path]` outside `benches/`, and none reads package files at run time. These are gaps for the next bench someone adds.

### F4 (minor): a single head burst on a bit-stable base neither blocks nor escalates
- **Where:** `perfgate/gate.py:215`.
- **What happens.** The NOISY test uses the *trimmed* head range, so base `[X]×5` against head `[X×4, 2X]` is OK in round 1 (GPT r2 "rare burst" probe, still OK). Escalation only starts with 2 hits in 5.
- **Cost.** A regression that strikes a fraction q of runs is missed with probability P(Bin(5,q) ≤ 1): 74% at q = 0.2, 92% at q = 0.1. Medians, and so DRIFT, don't see it either.
- **Fix.** When the base is deterministic (`rng_b ≤ STABLE_TOL·mb`), escalate if any *untrimmed* head run exceeds the base by more than the threshold. That costs nothing on bit-stable no-ops (they repeat exactly) and is never a FAIL by itself.

### F5 (minor): store's syscall sensitivity can be kept
- **Where:** `workloads.toml:45`.
- **Fix.** Instead of ungating all syscalls, gate the filesystem subset (openat, close, statx, mkdir, rename, unlink, fsync, fdatasync, ftruncate, pread/pwrite, …), from `_by_name`, which already exists. Count `write` with `strace -y` so eventfd writes drop out.
- Interim approval is fine (§4.3).

### F6 (nit): documentation drift
- README:270 says exact MW "up to 80 runs"; the code is `EXACT_MAX = 400` (`gate.py:46`).
- README:223 and `measure.py:13` still say the exe is "a hardlink"; it is now a read-only copy.
- README:158 still says `--target-dir <worktree>/target`; it is now `$ROOT/targets/<slot>`.
- README:65's "each must resolve under the root" (see F2).
- README "Measured on agentbox" quotes the pre-rebase harness `7285524e`. Its numbers reproduce on `824c3a09` (this review), so cite that.

### F7 (observation, no change asked): range-based floors on bursty counters
- **What happens.** Two bursts among 15 base runs set a wide threshold. GPT's "widen floor" probe has +5% shift, p = 0.0003, against a 10% threshold: OK.
- **Drift inherits it.** `drift_rows` takes `allowed` from the comparison (`gate.py:393`), so cumulative drift on such counters (store page faults ~7%, revindex RSS ~6%) is bounded only by their noise.
- **Why no change.** It is A1's max(noise floor, 0.5%) rule, and loosening it would raise the false-FAIL rate. Worth a line in README's Limits.

## Verdict

Every round-2 finding and rulings 1, 3–7 are **closed**; I re-ran them on the rebased head, including a cold anchor and the full acceptance suite. Ruling 2 is closed for state isolation. It is partial for "counters measured outside": that shortfall is F1 (an over-claim, minor) and F2 (the node path). The measured false-FAIL rate holds up with the anchor fixed.

One change is required before PF0 can gate merges authoritatively: **F2**. Guard node workloads so the first `rdashd` PR's binary can't run with `/work/bench` writable and with self-written counters. It is a few lines plus a test, and needs no acceptance re-run. F1's doc correction and the threat-model DEC should land with it. F3–F7 can be follow-ups.

**CHANGES-REQUIRED**

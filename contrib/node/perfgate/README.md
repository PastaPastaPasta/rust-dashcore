# perfgate: the PF0 per-PR counter gate

agentbox is shared, so wall-clock comparisons there are not credible: three identical runs vary by about
30%. Per A1 §3.11 and DEC-23, every merge into `node/integration` is therefore gated on **deterministic
counters** instead. Wall-clock side-by-side runs (`node-bench pair`) are kept for milestone reports in
reserved windows.

The gate is a **manager-run merge gate** (DEC-52 5). It runs from the integration worktree, on the exact
tree that would be merged.

```bash
contrib/node/perfgate/run gate --head node/<slug>    # merge of the PR into node/integration; exit 0 PASS, 1 FAIL,
                                                     # 2 ERROR, 3 INCOMPLETE
contrib/node/perfgate/run warm &                     # after every merge: hot base target/ and cache for the next gate
contrib/node/perfgate/run anchor <rev> --justification "M0 exit"   # (re-)record the drift anchor
contrib/node/perfgate/run gate --only 'tx-*' --head X              # a subset: INCOMPLETE at best, never PASS
contrib/node/perfgate/run measure node/integration   # one revision: per-workload medians + noise floors
contrib/node/perfgate/run measure --dashd <dashd>   # dashd as the node, node workloads (the same PF0 chain)
contrib/node/perfgate/run compare base.json head.json   # re-judge two saved measurements
contrib/node/perfgate/run list                       # the declared workloads
contrib/node/perfgate/run clean [--cache]            # free the root's worktrees and scratch
```

**Requirements:**
- Python 3.11+ (stdlib only);
- `perf_event_paranoid ≤ 2` with a PMU (agentbox has both);
- `strace`, `taskset`, `setarch`, `cc`, and git ≥ 2.38 (`merge-tree --write-tree`);
- for node rows: `bwrap`, `perf`, `setpriv`, `unshare`, `nsenter`, unprivileged user namespaces, delegated
  `systemd-run --user --scope` units, and CPUs 0–1 on NUMA node 0. The gate's self-probe checks them first.

**Checks:**
- Unit tests: `python3 -m unittest discover -s contrib/node/perfgate/tests`.
- Acceptance runs (real builds, about 1 h cold): `python3 contrib/node/perfgate/tests/acceptance.py`.
- Lint: `uvx ruff check contrib/node/perfgate`.
- Shim tests: `cargo test --manifest-path contrib/node/perfgate/criterion-shim/v0.8/Cargo.toml --features async_tokio`.

## State and who runs it

State lives under `PERFGATE_ROOT`, default `/work/perfgate` (DEC-52 2): worktrees, cache, reports,
`ledger.jsonl`, `anchor.json` and `anchors.jsonl`.

- **Ownership.** Only the manager's gate runs use the default root. An implementer who wants an early,
  advisory reading sets `PERFGATE_ROOT=/work/scratch/<slug>/perfgate`. Those runs neither contend for the
  flock nor seed the manager's cache.
- **Budget.** About 1.2 GB per side for the bench-only `target/` trees. `run clean` removes the worktrees,
  and `git worktree prune` is needed if the root is ever deleted by hand.
- **Ledger.** Each merge's ledger line is copied into the committed ledger file.
- **Write access (PF0 r2 ruling 2).** Only the gate's own process writes this state. The code under test
  runs sandboxed (see below) and can't touch it. The root must be on disk, not under `/tmp` or `/dev/shm`,
  which the sandbox replaces with private mounts. Create the default once:
  `sudo install -d -o agent -g agent /work/perfgate`.

## The sandbox (PF0 r2 ruling 2)

Everything a PR controls runs under bubblewrap (`perfgate/sandbox.py`):
- **Builds:** build scripts, proc macros, and the compiler on the PR's sources.
- **Measured processes:** bench binaries. (Node workloads run in their own chain: see
  [Node workloads](#node-workloads-dec-52-4-pf0-n).)

What it shuts off:
- **Filesystem.** It is read-only. Empty mounts replace the home directory and `/run`, so credentials
  (`~/.ssh`, the gh token), the user's systemd and D-Bus sockets, and `/run/docker.sock` aren't there.
  Bound back read-only:
  - the toolchain and the cargo home;
  - the repository and its git directory;
  - the gate's root.
- **Writable paths.** Only a build's own target directory and a scratch directory are writable. Each must
  resolve to a real directory under the root.
- **Namespaces.** The network, PID, IPC, UTS and cgroup namespaces are private. There is no TCP (localhost
  included, so not the sccache server), no abstract socket, and no signal to a host process.
- **Environment.** It is cleared and rebuilt from an allowlist.
- **No sccache.** Its server would compile outside the sandbox.

**Probes that fail from inside (unit-tested):**
- `systemd-run --user`, docker, and a connect to 127.0.0.1:4226;
- `kill -0` on the gate's own PID;
- reading `~/.config/gh` or `~/.ssh`, and writing the ledger;
- binding back a path that contains the home directory (for example the repository fallback of a harness
  outside a git checkout): refused before the sandbox starts;
- the reviewer's `state_writer` binary and a build script that rewrites a bench, which fail with
  "Read-only file system";
- rewriting the counter's output through an inherited descriptor (F0).

**Target directories.** These live in `$ROOT/targets/<slot>`, outside the worktrees, so a tracked `target`
symlink can't redirect a writable bind. Every slot but the base starts each gate from a reflinked copy of
the base's: a head's build scripts can write their target directory, and the next head must not inherit
that.

**What runs outside the sandbox, and why that's safe:**
- `cargo metadata`, `cargo update` and `cargo fetch`, and the toolchain probes. They run no package code;
  deps are fetched up front, and the build is `--offline --locked`.
- A head that changes `rust-toolchain(.toml)` or any `.cargo/config(.toml)`, or tracks `Cargo.lock` as a
  symlink, could make those steps run its binaries or write through the link. The gate refuses such a head
  (ERROR) before any cargo runs in it.

**Trust in what a bench reports (DEC-76).** The gate catches accidental regressions, and accidental
damage by PR code to the gate's state. Deliberate forgery of counts is out of scope, except where
defending is cheap. So the sandbox protects the gate's inputs and state, not the counts:
- **Authoritative:** `instructions_process`, the whole process tree's user-space instructions from a
  `perf stat` around the sandbox (gated, 1% floor). The measured code can't disable it
  (`PR_TASK_PERF_EVENTS_DISABLE` doesn't reach an outside counter) or hide a child from it.
  - **Nor rewrite its output (F0, PF0-N).** `perf stat -o FILE` keeps FILE open in the process it starts.
    Inherited past bwrap, that descriptor lets the code under test reopen FILE through `/proc/self/fd/N`, although
    the sandbox never mounts it; a padded forged line then parsed as the count.
  - Every bench counter (perf and strace) therefore starts its command through
    [`launch --exec`](perfgate/launch.c). Before bwrap, it points stdin at `/dev/null` (a counter started with
    fd 0 closed opens its file as fd 0) and closes every descriptor above stderr; stdout and stderr are the
    gate's pipes. That adds a fixed ~126k user-space instructions to `instructions_process`. Node runs are
    counted by the broker, outside every process node-bench or the node can reach.
- **Self-reportable:** everything else.
  - The in-region counters come from the shim, inside the measured process; PR code shares that address
    space.
  - strace runs outside the sandbox and writes where the bench can't, but its region follows the marker
    calls the process makes. An extra `access("/perfgate/end")` hides the rest of a span, and a
    `clone(CLONE_UNTRACED)` child is invisible to `strace -f`.
  - Code that tests `PERFGATE_MODE` can behave differently under the gate.

`result.json` is read without following links and validated. Review, not the gate, is the defence against
gate-aware code.

## What a gate run does

1. **Harness check (M3).** The running harness, `contrib/node/perfgate` of the checkout running `./run`,
   must be the committed harness of the base, and clean. Otherwise the gate is an ERROR. `--dev-harness`
   overrides this for development, and the run is then non-authoritative (verdict INCOMPLETE at best).
   - This check is what stops a PR from loosening or deleting the workload it regresses, or from editing
     the policy, and passing.
   - With the base's own harness, every declared bench workload must exist on the base; a missing one is
     an ERROR (a declaration error).
   - The run is authoritative only if the base also resolves to `--integration-ref` (default
     `node/integration`, recorded in the ledger). A gate against some older base is INCOMPLETE.
2. **The merge candidate (B1).** If the head doesn't contain the base, the gate synthesizes the
   `--no-ff` merge of head into base (`git merge-tree`, then `commit-tree`; deterministic, on no branch)
   and measures that.
   - A conflict is an ERROR: rebase the branch.
   - The ledger records the measured commit and tree. The manager's merge commit must have that tree.
   - Without this, a stale PR is judged against changes it doesn't contain. An approved regression on
     integration can then cancel out the PR's own.
3. **Worktrees.** Base and the merge candidate are checked out as detached worktrees under
   `$PERFGATE_ROOT/worktrees/{base,head}`. Each slot keeps its own target directory in
   `$PERFGATE_ROOT/targets/<slot>`, so rebuilds are incremental.
   A tracked `Cargo.lock` has its criterion entries moved onto the shim (`cargo update -p criterion@…`).
4. **Bench inputs are the base's (M2, PF0 r2 ruling 1).** The gate diffs base → candidate over everything
   that can change a selected bench's inputs:
   - the bench sources (a bench under `<pkg>/benches/` is that whole directory), and every file they pull in
     with `include!`, `include_bytes!` or `include_str!`;
   - the files the base build of the benches read, from rustc's dep-info (for example `#[path]` modules and
     data), outside library `src/` trees;
   - the build scripts of every workspace or path package in the bench's dependency closure (`build.rs`,
     even where the base has none, and any declared build script), their `test_utils` input generators
     (`Block::dummy` and friends) and their bench glue, any `bench.rs` or `bench/` under `src/`
     (dashd-chainstate's `connect/bench.rs`; BL-426, DEC-514);
   - the whole crates of code that runs at compile time and can write anything (DEC-76 c): the proc macros
     in the closure, the local build-dependencies of its packages, and every local crate those use
     (`git-state` for `dash-spv`);
   - in those packages' manifests: `build`, `links`, build-dependencies and features;
   - the bench package's `[[bench]]` and dev-dependencies, their `[workspace.dependencies]` entries, and
     every package the lockfile resolves them to, transitively, as (name, version, source, checksum);
   - the workspace's `[profile.release]` and `[profile.bench]` (bench inherits release), `[patch]` and
     `[replace]`.

   The boundary is computed on both sides, so a crate the head adds to the closure is covered.

   Toolchain and cargo-config changes are refused outright (see the sandbox).

   **What a change does.** Any change adds a blocking `BENCH-CHANGED` row, and changed bench sources and
   included data are checked out from the base over the head. The head's library code is then measured
   with the base's inputs, so an edit to a bench can't move the verdict. Build scripts run in the
   read-only sandbox and can't edit sources at build time. If the head doesn't build (for example, a build
   script that tries to rewrite a bench fails), the gate is an ERROR, and the report still lists the
   BENCH-CHANGED row.
   - The manager approves a bench change by re-running with `--approve-bench-change "<justification>"`.
     The row becomes `APPROVED` (non-blocking, recorded in the ledger).
   - Even when approved, the gated head is the one with the base's benches, whenever that builds. A
     library regression can therefore never hide behind an input change that removes work.
   - The head's own benches are measured as well, in a third worktree, as INFO rows. They show the cost of
     the new inputs.
   - Only when the base's benches no longer compile against the head is the head's own variant gated.
   - After such a merge the anchor is stale for that package, and the manager re-anchors it.
5. **Build.** One `cargo bench -p <package> --no-run` per package, with this tool's criterion shim
   patched in for criterion 0.5 and 0.8.
   - **One package per invocation.** Cargo unifies dependency features across packages built together;
     that moved `storage/disk/get` by 15% between a partial and a full build.
   - **Target dir.** `--target-dir $PERFGATE_ROOT/targets/<slot>`, with `CARGO_TARGET_DIR` removed, and
     every executable must lie inside it.
   - **Same shim bytes.** Both sides build from one copy of the shim taken when the run starts.
6. **Iteration counts** come from the anchor. A workload the anchor lacks is calibrated on the base
   (about 200M instructions per run, rounded to 1/2/5×10ⁿ), and the head uses the same count.
7. **Round 1.** Every workload runs 5 times per side (`--repeat`), with base and head interleaved in each
   round. Each bench run is a counters process and an strace process. Each node row first gets a preflight
   per side, then its runs (see [Node workloads](#node-workloads-dec-52-4-pf0-n)).
8. **Escalation (M1, PF0 r2 rulings 3 and 5).** A workload escalates when it has a NOISY counter, or a
   round-1 REGRESSION on runs that weren't bit-stable on both sides. It runs 10 more times per side,
   interleaved (`--escalate`), and **every** counter is decided on the pooled runs (see the policy). If the
   extra runs fail on either side, the workload is an ERROR.
9. **Drift (M4).** The head is also compared with the anchor.
10. **Report.** `report.md`, `report.json`, `base.json` and `head.json` go to
    `$PERFGATE_ROOT/reports/<time>-<head>-vs-<base>/`. One line is appended to `ledger.jsonl`. The line
    records:
    - the verdict and whether it is authoritative;
    - base, head, the measured commit and tree;
    - the selection (`--only`, the workloads file and its hash);
    - the harness hash and tree, and the anchor;
    - any bench-change approval, the flagged rows and the wall time.

**Caching.** Base results are cached per commit, workload, iterations and repeat count. The fingerprint is
part of the cache key:
- `Cargo.lock`, `rustc -vV`, `cargo -vV`, `cc --version`;
- the cargo config files above the worktree and in `CARGO_HOME`;
- libc, kernel and CPU model;
- the online CPU count (`ncpu`: thread pools size themselves from it) and the sandbox's security label
  (`sandbox_label`: the AppArmor profile bwrap's exec moves into, which changes what perf counts of bwrap);
- the build environment;
- the harness and shim sources;
- the counting path (`counter`): everything perf counts besides the bench, and how the counts are read.
  That means `launch.c` and its compiler flags; the sandbox command line and its number of read-only binds
  (each costs bwrap ~355k instructions); the perf and strace command lines; their parsers and syscall
  classes; and the perf, strace, bwrap and util-linux versions;
- the worktree path.

How the cache is used:
- **Base reuse.** A cached base workload is reused only if all of its gated counters repeated within
  0.01%. Any other workload is re-run next to the head.
- **Head.** Head results are never reused.
- **Warming.** `run warm` (or `measure --label base`) builds and measures the integration head in the
  base slot, so the next gate starts hot.

## Counters

Every counter except `instructions_total` and peak RSS covers only the **measured region**: the
iterations, not process startup or the bench's setup code. `iter_batched` setup runs outside the region,
as it does in criterion.

| counter | source | gated (bench / node) |
|---|---|---|
| `instructions` | `instructions:u`, `perf_event_open` opened at `main` with `inherit`, so threads the bench spawns are counted | yes / no |
| `instructions_total` | the same counter from `main` to the report: setup, and drops after the last span, so work a bench edit moves out of the region still shows (1% relative floor) | yes / – |
| `allocs`, `alloc_bytes` | the shim's counting global allocator (alloc, alloc_zeroed, realloc); for node workloads the LD_PRELOAD counter | yes / bytes only |
| `syscalls` | `strace -f`, cut to the region by marker calls (`access("/perfgate/begin")`), minus the scheduling set | yes / – (node rows have no strace pass) |
| `fs_syscalls` | the same log: opens, `stat`/`statx`/`access` and friends, `mkdir`/`rmdir`, `rename`, `link`/`unlink`, `truncate`/`fallocate`, `getdents64`, `fsync`/`fdatasync`/`msync`/`sync*`, except calls on `/sys` and `/proc` (glibc reads `/sys/devices/system/cpu/online` when a new pool thread makes a malloc arena). Gated where the total `syscalls` count follows a thread pool (the storage benches) | yes / – |
| `read_bytes`, `write_bytes`, `read_syscalls`, `write_syscalls` | `/proc/self/io` (rchar, wchar, syscr, syscw) | yes / no |
| `page_faults` | `getrusage` minor + major | yes / yes |
| `peak_rss_kib` | `VmHWM` after a `clear_refs` reset just before the benchmark's closure | yes / yes |
| `sched_syscalls` | `futex`, `sched_yield`, `epoll_wait`, `poll`, `nanosleep`, … (counts follow scheduling, not work) | no |

**Syscalls without privileges (BL-07).** At `perf_event_paranoid=2`, the `raw_syscalls` tracepoints are
refused. strace (ptrace) needs no privilege and gives exact per-call names. It runs in a separate process
from the counters, so its overhead never touches the instruction counts.

**Executable copies.** Executables are read-only copies, never hardlinks. The linker rewrites cargo's
outputs in place, so a hardlink would take on a later build's code; that once made a gate run the head's
code on both sides.

**Making counts repeat:**
- **ASLR off.** Bench processes run with `setarch -R`.
- **Fixed process image.** Each process gets a fixed environment (`PATH=/usr/bin:/bin`, a root-local
  `HOME`, `LANG=C` and the shim's variables). The executable runs from a read-only copy at a path of fixed length
  (`$PERFGATE_ROOT/exe/<hash>`). With ASLR off, the environment's size and `argv[0]` place the stack.
- **One CPU per process.** Each is pinned with `taskset` to one of the highest-numbered CPUs (away from
  node-bench's NUMA 0), with `RAYON_NUM_THREADS=1`. Thread pools then size themselves to one worker, and
  idle workers don't spin: with rayon's default pool, `filter_scan`'s instructions varied by 5%; pinned,
  they vary by 0.03%.
- **Consequence.** Under this gate, code that parallelises work shows its extra instructions and none of
  its speed-up. Such PRs go through the approval path.

## Threshold policy (A1 §3.11, DEC-52)

**Round 1.** For each (workload, counter), take the 5 base runs `b` and 5 head runs `h`, each trimmed of
the one run farthest from its median (`b′`, `h′`). The noise floor is `range(b′)`. The A1 threshold is
`max(min_rel × median(b), noise floor, absolute floor)`, with `min_rel` 0.5% (peak RSS 2%,
`instructions_total` 1%).

```
REGRESSION = shift > max(A1 threshold, 2 × noise floor)   and   (min(h′) > max(b′)  or  the base is deterministic)
             (noisy base only: that bar may rise to 2 × range(h′), at most 4 × noise floor)
IMPROVED   = the mirror image, separation always required
NOISY      = any other shift beyond the A1 threshold, or range(h′) > max(2 × noise floor, min_rel × median(b), floor)
OK         = otherwise
```

**Escalation.** NOISY is never a verdict.
- **What escalates.** A workload with a NOISY counter, or with a round-1 REGRESSION on runs that weren't
  bit-stable on both sides, gets 10 more runs per side. Five runs can miss a population's rare bursts and
  look deterministic.
- **Every counter** of an escalated workload is then decided on the 15 + 15 runs. Later evidence can turn
  a round-1 OK into a REGRESSION (ruling 3). It can also show a round-1 "regression" to be a burst.
- **Undecided shifts still block.** A regression the pooled runs can't decide is NOISY-UNRESOLVED.
- **Bit-stable shifts need no confirmation.** Where both sides repeat exactly, as with a planted
  regression in a pure bench, the round-1 REGRESSION stands.

```
threshold         = the A1 threshold, noise floor from all 15 base runs (never widened by the head)
shift             = max(Hodges-Lehmann estimate, difference of the trimmed means)
REGRESSION        = shift > threshold  and  one-sided Mann-Whitney p(head > base) < α = 0.01 (exact)
IMPROVED          = both estimates < −threshold  and  p(head < base) < 0.01
NOISY-UNRESOLVED  = shift > threshold without significance          (blocks)
OK                = otherwise (an undecided improvement included)
```

- **Why two shift estimates.** The Hodges-Lehmann shift is a median: it reads 0 for a regression that hits
  fewer than half the runs, such as a timer task or a `RandomState` map on some runs. The trimmed mean
  catches that case, and the rank test is still significant for it.
- **Exact p-values (ruling 4).** The p-value is the exact permutation distribution of the head's rank sum,
  with tied values sharing their mid-rank, up to 400 runs in total. Counters tie often, and the normal
  approximation then overstates significance: `[0]×15` vs `[0]×10 + [1]×5` approximates to 0.0087, but
  is 0.0211 exactly, which doesn't cross α.
- **Calibration (ruling 5).** α is not the gate's false-FAIL rate, so the whole procedure is calibrated
  against real no-op runs (`tests/calibrate.py`; see [Measured](#measured-on-agentbox)).

**Drift (DEC-52 3).** A gated counter blocks as `DRIFT` when the head median exceeds the anchor's by
more than max(2% of the anchor, the comparison's threshold, the floor). It stays blocked until the
manager re-anchors with `run anchor <rev> --justification "…"`. `--only` re-anchors some workloads and
keeps the rest.

**What the anchor records.** Per workload:
- its declaration key and a hash of its package's bench inputs: sources, `[[bench]]`, dev-dependencies
  with their workspace entries and locked versions, and `[profile.bench]`;
- the iteration count;
- each counter's median.

**When the anchor blocks:**
- **Stale anchor (ERROR).** A gate whose base has other bench inputs or another declaration for a
  workload than the anchor, until that workload is re-anchored. An approved bench-input change therefore
  can't quietly shift the anchor's baseline.
- **Toolchain, host, shim or counting-path change.** This makes the anchor incomparable (ERROR). The
  counting path is in the anchor's fingerprint because the launcher's own instructions are part of
  `instructions_process` (PF0-N). Re-anchoring then
  *carries* the drift so far: from the last authoritative gate of the same tree under the old
  fingerprint, each counter's drift `d` is kept, and the new anchor stores `measured / (1 + d)`. Without
  such a gate, re-anchoring needs `--reset-drift` and a justification that says why. `--reset-drift`
  starts drift from zero even when such a gate exists, and is refused without a fingerprint change; the
  drift such a gate would have carried is printed and logged under `reset` in `anchors.jsonl`. A counter
  that fell to 0 against the old anchor (drift −100%) or rose from 0 (no ratio) cannot be carried: its
  measured median is anchored, so its drift restarts from that median and an earlier improvement is not
  banked. It is printed and recorded under the workload's `uncarried`. Gates read neither `carried` nor
  `uncarried`; they are the audit trail (BL-313).
- **`anchors.jsonl` lines** carry `forgiven`, `carried` (only the counters applied to the workloads
  anchored), `uncarried` and `reset`, each `{workload: {counter: drift}}` and `{}` when empty; a drift of
  `null` means "rose from 0". Lines before BL-313 have no `uncarried` or `reset`, and their `carried` is
  the gate's whole drift.
- **Plain re-anchor.** The drift it forgives is printed and logged in `anchors.jsonl`.
- **Anchor runs.** An anchor takes 15 runs and records each counter's noise floor. The drift limit is
  max(2%, the comparison's threshold, twice the anchor's noise floor): an anchor median has sampling error
  of its own.
- **Full re-anchor.** It needs every bench workload measured.
- **A new counter.** An anchor without it is an ERROR for every workload that has it ("anchor has no
  …"). Merging the harness change that adds it (PF0 r3: `fs_syscalls`, and the `spv-storage-persist`
  workload) needs a full re-anchor at the merge; the fingerprint is unchanged, so it is a plain re-anchor.
- **Why.** Ten PRs at +0.45% each pass the per-PR check, but compound to +4.6%.
- **No anchor at all** is an ERROR.
- **Unanchored workloads (ruling 6).** A gated workload the anchor lacks, or a gated counter without an
  anchored median, is an ERROR. Drift must never go unchecked: the manager anchors it with
  `run anchor <rev> --only <name> --justification ...`.
- **A stale anchor** is an ERROR: one recorded under another toolchain, host, shim or counting path, or with other
  iteration counts.

**Blocking statuses:**
- `REGRESSION`, `NOISY-UNRESOLVED`, `DRIFT`;
- `BENCH-CHANGED`, which needs the manager's recorded approval;
- `ERROR`, which covers:
  - a workload or gated counter that ran on the base but not on the head;
  - a base that failed to measure;
  - incomparable sides;
  - strace markers that don't match the bench's spans;
  - an unusable anchor;
  - a running harness that isn't the base's;
  - a merge conflict;
  - a build failure.

The manager may approve any blocking result with a justification recorded in STATUS (for example a
correctness fix).

**Verdict precedence (ruling 7):**
- **ERROR (exit 2):** any ERROR row; the gate couldn't judge something.
- **FAIL (exit 1):** any other blocking row.
- **INCOMPLETE (exit 3):** no blocking row on a non-authoritative run (a partial selection, another
  workloads file, a dev harness, or a base other than `node/integration`). It is not a PASS.
- **PASS (exit 0).**

**Why the rules are what they are:**
- **Deterministic bases decide most workloads.** After the r2 bench fixes, 21 of the 24 bench workloads
  repeat their region counters to 1.3e-04 % or better.
  0.5% applies to them, and a moved median blocks even when the head's runs spread. A PR can't hide a
  regression behind noise it adds.
- **Trimming.** One stray run can neither set the threshold nor block an unchanged PR.
- **The doubled and widened spread in round 1** keeps a random-input workload from false positives. The
  range of 4 runs underestimates the spread; an unchanged random-input workload was flagged at +2.2%
  against 1× its base range, and at +2.3% against 2× the base range alone.
- **The rank test** then settles every shift round 1 can't. A real shift that adds spread (a timer task,
  a `RandomState` map, lock contention) is significant at 15 + 15 runs. A random-input workload with no
  real shift lands in OK.
- **Absolute floors** stop meaningless flags on near-zero counters:
  - 0.01 events per iteration, and at least 1 event per run, for allocations, syscalls and page faults;
  - 1 byte per iteration, and at least 512 bytes per run, for byte counters.

  A hot loop going from 0 to 1 allocation per iteration still fails. One stray event in a 1-iteration
  workload doesn't: a no-op gate otherwise failed on `spv-storage-store` read syscalls (4 vs 4, one
  head run with 5).
- **Peak RSS** is page- and allocator-granular: 2% relative and 256 KiB absolute. The floor is 4 MiB only
  where the base's own runs step by 0.5 MiB or more; the tokio storage benches move between 49, 51, 52 and
  55 MiB with thread timing alone. A 3 MiB bench can't double its RSS unnoticed.

## Adding a workload

Add one line to [`workloads.toml`](workloads.toml):

```toml
tx-deserialize = { bench = "dashcore/transaction", id = "transaction_deserialize" }
```

- **`bench`** is `<cargo package>/<bench target>`.
- **`id`** is the full criterion ID: `group/function/parameter`, as
  `cargo bench -p dashcore --bench transaction -- --list` prints.
- **Optional:** `iters = N` pins the iteration count; `syscalls = false` skips the strace pass.

The gate runs the integration's harness and workloads, so the PR that adds the line isn't judged on it.
If the PR also adds a bench file or `[[bench]]` to a gated package, that shows as `BENCH-CHANGED`.
Approve it with `--approve-bench-change "adds bench X"`. From the next gate on, the workload is gated.
Until the manager adds it to the anchor with `run anchor <rev> --only <name> --justification "new
workload"`, every gate is an ERROR, so anchor it right after the merge.

Benches must stick to the API the shim provides (the workspace's current usage: `bench_function`,
`bench_with_input`, groups, `iter`, `iter_batched(_ref)`, `to_async(...)` with tokio). Anything else
fails to compile under the gate and names the missing item.

## Node workloads (DEC-52 4, PF0-N)

A node workload is one line too, and stays `pending` until its binary target exists. Since PF0-N (BL-128) it
runs on **both sides, PR code included**: the PR's node never sees the gate's state, its own counts or another
side's binary.

```toml
node-startup-genesis = { node = "startup-genesis", bin = "rdashd" }
```

**The chain** (PF0-N plan rev4.1 §3; `perfgate/nodegate.py`, `invocation.py`, `broker.py`, `outer_init.py`):
- **One holder per gate.** A user and mount namespace holds a small tmpfs, `GATE`, with each side's
  executable, copied and checked by sha256. Before it, the gate stops stale `pfn-` units and runs a
  self-probe of the whole chain (`perf` counting `true` in a jail).
- **One invocation per preflight or run.** Each is its own `systemd-run --user --scope` with memory, task and
  time limits. It joins the holder's namespaces, then unshares net, pid, ipc, uts and mount. `outer_init`
  makes the host read-only (except the invocation's `res` directory), mounts a fresh `RUN` tmpfs and
  starts the broker and node-bench.
- **node-bench runs in a bwrap view** (`workload <name> --pf0`). The view has `/usr` and the base's
  node-bench tree read-only, `RUN` at `/pf0/run` read-write, and nothing of `/work`, `$HOME`, `GATE` or `res`. Its daemon is the
  stub `/pf0/bin/node-launch`: every node start is a request to the broker over a socket.
- **The broker spawns the node.** Each node runs under `perf stat` in a hardened bwrap jail: no
  capabilities, its own user and pid namespace, one fresh writable datadir, ASLR off, pinned to NUMA-0
  CPUs, with the LD_PRELOAD allocation counter. The broker reads perf's output from a memfd and the
  allocation records over a pipe, and writes them to `res`. node-bench never sees the counts.
- **Bounded and accounted for.** Every process of the invocation must be one the census (outer_init) knows
  about. Every spawn has a deadline, a memory limit and a task limit; a hit limit is an ERROR naming it.

**Preflight and REFUSED (§7).** rdashd refuses an option it doesn't implement yet with one line naming the
option and its owner. Each row first runs its exact measured argv as a preflight. Each refusal drops exactly
that option and retries, up to 32 times; a retry that runs 30 s without refusing, then stops with exit 0 on
SIGTERM, accepted the rest (stopped with its refusal line, it refused after all). An RPC row's start that is
past warmup but answers `getblockcount`, or a method the row's work calls, with `-32601 Method not found` also
accepted the argv, and the row is refused on each missing method, as `rpc:<method>` owned by `PENDING` (any
other error from `getblockcount` is an ERROR); an HTTP 503 (a stopping server, or a full work queue) is not
ready yet, so the start's exit decides. A retry accepted by the 30 s settle never answered RPC, so its record
names no methods: `rpc:` entries are compared only between two records that both answered. A row whose
argv rdashd accepts is measured, with the same argv (its hash is checked); a row node-bench has no measured run
for yet is **PREFLIGHT-ONLY** (D92), which doesn't block. A row with refusals is **REFUSED** with its
`{option, owner}` list and doesn't block. For both, a PREFLIGHT-ONLY row counting as a refusal of no options:
- head refuses an option the base accepted → **REFUSAL-REGRESSION** (blocks);
- head refuses fewer options than the base → REFUSED (PREFLIGHT-ONLY when none are left), with an anchor
  obligation in the ledger;
- base REFUSED, head measured → **NEW-FROM-REFUSED**, which blocks until
  `--approve-new-from-refused "<why>"`, when key, argv and owners all match the anchor, every refused
  option is a token of the argv (a missing method is none), and the head's preflight accepted the argv on
  its first attempt by its ready signal. Anything else is an ERROR. Anchor the row right after the merge.

`run anchor` records REFUSED and PREFLIGHT-ONLY rows too (key, argv hash, refusals); an ok row also keeps its
key and argv hash. It refuses a selected node row that errored. The manager re-anchors when an obligation is
due: until then, the next gate's base disagrees with the anchor and the row is an ERROR.

**Anchor inputs (§8).** A node row's anchor stores a hash of everything but the PR binary that moves its
counts, as 17 named components: the harness's node-path sources (`inputs.node_sources()`, an import
closure checked against a reviewed snapshot), node-bench's tree, the launcher and preload sources, the
rendered argv and configs, limits, perf events, tool versions and the preflight contract. A stale anchor's
ERROR names the components that changed. A runtime guard test (`tests/test_runtime_guard.py`) runs one
gate pass under strace with every Python process audited, and fails if the node path reads an unhashed file.

**Phase-1 rows.** `node-startup-genesis` is live: a regtest node on an empty datadir, ready file, SIGTERM,
exit 0. The four fixture rows (`startup`, `mempool-accept`, `gbt`, `rpc`) are REFUSED until rdashd
implements their options and RPC methods, then PREFLIGHT-ONLY until their PREP flow exists.

**What gates them.** For now, memory signals only: page faults, allocated bytes and peak RSS. Their
instruction, allocation-count and I/O rows are shown as INFO, and they have no strace pass (D39).
- **Why.** dashd's numbers show why: instructions repeat to 5.7–47% (timer threads do work in proportion
  to wall time), allocation counts to 0.5–6%, but allocated bytes, faults and RSS to 0.03–1.7%.
- **Blind spot.** Memory signals don't see a purely algorithmic slowdown.
- **Where the counts come from.** `page_faults` (and the INFO instructions) are perf's, counted outside the jail.
  `alloc_bytes` and `peak_rss_kib` come from the preload inside the node: self-reportable (DEC-76).
- **Memory flags (plan §10, INFO).** Each run also keeps two trusted values from the broker: the wait4 maxrss
  (the largest of perf, bwrap, the node and every descendant they reaped) and spawn-k's `memory.peak` (the whole
  tree, its tmpfs and the page cache charged to it). `rss-mismatch` (VmHWM and maxrss differ by more than 4 MiB)
  and `tree-memory` (`memory.peak` above VmHWM + max(64 MiB, 50%): a forked helper, a child without the preload,
  tmpfs, file reads) go in the `peak_rss_kib` row's note and the report's flag section. A node smaller than perf
  (about 17 MiB) always shows `rss-mismatch`: the maxrss is perf's own.
- **Counter source changed.** A gated counter whose head median falls under 1% of the base's, or `allocs` 0 on
  one side only (a custom allocator, memory the preload no longer sees, or the reverse), is an ERROR, never
  IMPROVED. `--approve-counter-source "<why>"` (one text for the gate's every such row, as with the other
  approvals) makes it APPROVED with an anchor obligation naming the counters. The same rule against the anchor
  holds until the row is re-anchored; the row's other counters are still drift-checked. A counter that grows
  100-fold (a custom allocator removed) is a plain REGRESSION.
- **Approved regression (BL-406).** A REGRESSION the manager attributed and ruled on is approved per row with
  `--approve-regression WORKLOAD:COUNTER:+BOUND[%]:DEC-n` (repeat for more rows; e.g.
  `node-startup-genesis:page_faults:+1.5%:DEC-464`). Within the bound (relative to the base median with `%`, else
  absolute per run) the row is APPROVED with an anchor obligation; above it the row stays a REGRESSION and the
  gate fails. An approval no REGRESSION row used is noted in the report. A malformed approval, an unselected
  workload, a counter that does not gate that workload, or a workload and counter approved twice is a usage error
  before anything is measured. `compare` re-judges without approvals.

**The path to CPU-gating the node:**
1. **Count only the measured phase.** `perf stat --delay=-1 --control fifo:…`, with node-bench sending
   `enable`/`disable` around the op stream, excludes startup and shutdown, where most timer work happens.
2. **Count per thread.** `perf stat --per-thread`, or counters opened on the named work threads
   (`d-httpworker.*`, `d-msghand`, rdashd's named workers). Gate the work threads and ignore the timer and
   scheduler threads.
3. **Give rdashd a deterministic bench mode.** Mock time, no periodic timers, single-threaded executors
   (`-par=1`). Then measure *rdashd's* noise floors. dashd's floors only bound what a C++ node with many
   timer threads does, and dashd's counters are only a milestone ratio.
4. **Size the op counts** so the work dominates any remaining timer work, and decide with the escalation's
   rank test.

When 1–3 exist and rdashd's measured floors are within the policy, the node rows become CPU-gated, which
needs a DEC entry. Separately, node-bench's `startup` workload had one dashd run at 4.5G instructions
against a median of 1.0G; that is a different code path, and needs a look before startup is gated on
anything.

## Measured on agentbox

**PF0 r3 acceptance** (2026-10-09, harness `f1a26327`, tree-identical to `979aa6e8` after a docs-only rebase; load average 25–60;
reports in `/work/scratch/pf0/acceptance/<scenario>/`). The anchor was recorded at the base with 15 runs, in
432 s:

| scenario | verdict | blocking rows | wall |
|---|---|---|---:|
| planted | **FAIL** | siphash-single-16k instructions +1.245%, instructions_total +1.244%, instructions_process +1.210% | 714 s |
| no-op | **PASS** | none (spv-storage-persist escalated and resolved) | 692 s |
| stale base (B1) | **FAIL** | siphash-single +0.820%; DRIFT +2.075% against the anchor | 12 s |
| bench input (M2) | **FAIL** | BENCH-CHANGED (`hashes/benches/siphash.rs`); P1's +1.245% still shows | 10 s |
| build-script exploit | **ERROR** | BENCH-CHANGED (`hashes/build.rs`); "Read-only file system" | 8 s |
| the same, ignoring the write error | **FAIL** | BENCH-CHANGED; P1's +1.245% still shows | 11 s |
| state writer | **FAIL** | BENCH-CHANGED; the ledger has no forged line | 10 s |

**New counters and workloads, from the 15-run anchor:**
- `fs_syscalls` repeats exactly on every storage bench: store 230, persist 800, get 36, revindex 519.
- `spv-storage-persist` (50 iterations): instructions noise 0.04%, allocations 0.4%, write bytes exact. Page
  faults (median 259, spread 262) and peak RSS (18%) are bimodal, like the store bench's; they escalate. The
  r2 calibration below predates this workload: it needs no-op sessions of its own.

The rest of this section is the PF0 r2 runs from 2026-10-08, at a load average of 22–70 on 32 vCPUs.
- **Harness.** The acceptance runs and the first 10 no-op gates ran on the sandboxed harness at `7285524e`.
  The next commit only adds per-workload `ungated` counters; the last 5 no-op gates ran on it.
- **Root.** `/work/scratch/pf0/perfgate`.

Evidence:
- acceptance reports: `/work/scratch/pf0/acceptance/<scenario>/`;
- real no-op gates: `/work/scratch/pf0/noop-runs{,-final}/`;
- calibration: `/work/scratch/pf0/calibration-final.{log,json}`.

**Acceptance** (`tests/acceptance.py`; base `7285524e`, anchor recorded there with 15 runs in 636 s, cold
sandboxed build):

| scenario | verdict | blocking rows | wall |
|---|---|---|---:|
| planted: one `black_box` per SipHash call | **FAIL** (authoritative) | siphash-single-16k instructions +1.245%, instructions_total +1.244%, instructions_process +1.219% (pooled 15 v 15, p 6.4e-09); nothing else | 621 s |
| no-op: a doc edit that rebuilds `dashcore_hashes` and dependants | **PASS** (authoritative) | none | 618 s |
| stale base (B1): base = P1, PR = P2 cut before it | **FAIL** | the merge P1+P2: siphash-single +0.820%; DRIFT +2.07% against the anchor | 12 s |
| bench input (M2): P1 + `LEN` 25 → 24 | **FAIL** | BENCH-CHANGED (`hashes/benches/siphash.rs`); P1's +1.245% still shows | 10 s |
| r2 exploit: P1 + `hashes/build.rs` rewriting `LEN` | **ERROR** | BENCH-CHANGED (`hashes/build.rs`); the build script dies with "Read-only file system" | 9 s |
| the same, ignoring the write error | **FAIL** | BENCH-CHANGED; P1's +1.245% still shows (the bench input is intact) | 12 s |
| r2 state writer: a build script appending a forged PASS to the ledger | **FAIL** | BENCH-CHANGED; the ledger has no forged line | 11 s |

The stale-base, bench-input and exploit scenarios use `--only 'siphash-*'`.

**False-FAIL rate (ruling 5)** of the whole procedure, from `tests/calibrate.py`. Each trial is a session
bootstrap over the no-op runs, running round 1, escalation, pooled decisions on every counter, and drift
against a simulated 15-run anchor:

| | false FAIL | 95% Wilson |
|---|---:|---|
| r1 no-op pools (before the bench fixes) | 10.0% | 8.8–11.4% |
| r2, `spv-storage-store` syscalls gated | 2.31% | 2.11–2.52% |
| **r2 final** (store pool syscalls ungated), 11 sessions, 20,000 trials | **0.205%** | **0.151–0.278%** |

- **Per-session rates:** 0–0.73%.
- **Remaining causes:** peak-RSS and page-fault steps in the storage benches.
- **Real no-op gates:** 15 runs on `kwm-*` and `spv-*` (the workloads that aren't bit-stable), at load
  22–70, with `--no-cache` so both sides are re-measured each time. 10 ran on the r2 harness before the
  ungating, 5 on the final one. None had a blocking row; 5 escalated and resolved.
- **The 20 bit-stable workloads can't fail a no-op:** their counters repeat to within the floors (see below).

**Noise floors** (trimmed range over median of the base runs, from the r2 no-op gate):

| workload | instructions | other gated counters |
|---|---:|---|
| hashes-*, siphash-*, sha256-*/dsha256, tx-* (19) | ≤ 1.1e-05 % | instructions_process ≤ 0.002% (bwrap and loader), rest ≤ 1.2e-05 % |
| kw-addrgen-100 | 1.3e-04% | ≤ 1.9e-04% |
| kwm-filter-scan-2000 (now seeded) | 7.2e-06% | ≤ 3.9e-04% (was 0.38% allocs, 5% instructions_total) |
| spv-storage-store (worker stopped) | 0.011% | page faults 6.9%; syscalls 8.6% (ungated) |
| spv-storage-get | 0.003% | peak RSS 1.9% (page-granular) |
| spv-storage-revindex | 0.219% | peak RSS 5.9% (page-granular) |

**Gate time.** A gate with a warm base and a head that rebuilds `dashcore_hashes`: 10–11 minutes. Since
r2:
- **No sccache.** Gate builds bypass sccache.
- **Fresh head target.** The head's target starts from a reflinked copy of the base's, so registry crates
  are reused and workspace crates rebuild.
- **Anchors** take 15 runs.
- **A real no-op gate** on the four noisy workloads: 4.5–9.5 min.

`run warm` after each merge keeps the base hot.

## Limits

- **Rust heap only, in bench mode.** Allocations from C and C++ code (secp256k1 and blst scratch, RocksDB
  once storage benches use it) bypass `GlobalAlloc` and aren't counted. When such benches join the set,
  preload the C-allocator counter in bench mode too.
- **Work after the report is not counted.** Background tasks that would run after the last span
  (dash-spv's persistence worker) never do, because the process exits after reporting. Drops and setup
  are covered by `instructions_total`.
- **Timer-driven work in benches.** Work a background timer triggers lands inside the region or misses
  it, depending on wall time; the `dash-spv` storage benches show it. Their noise floors rise, and the
  escalation decides them.
- **No cachegrind fallback.** A1 names `valgrind --tool=cachegrind` for hosts without a PMU, but valgrind
  isn't installed and the PMU works here. The shim fails loudly when `perf_event_open` is refused. This is
  a recorded deferral.
- **Coverage gaps.**
  - X11 (`hashes/src/hash_x11.rs`, behind the `x11` feature) has no bench yet. A bench there needs a
    feature-scoped build, so as not to change the other `dashcore_hashes` benches' features.
  - The `spv-headers-*` header-sync replay workload isn't declared yet.
  - Both are on the backlog.
- **Custom allocators.** A Rust node built with a non-system allocator (PF2) shows 0 allocations under
  the LD_PRELOAD counter, and would need a counting wrapper behind a feature.

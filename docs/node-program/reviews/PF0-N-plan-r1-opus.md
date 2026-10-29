# PF0-N plan review — Opus high, round 1 (second independent reviewer)

Target: `/work/scratch/pf0-n/plan-rev3.md` (rev3.1, D1–D36, P11–P17), read against plan-rev1/rev2, Sol r1–r4, the
`node/pf0-n` worktree (285e46b4f, 3b8b81886) and the integration checkout (59faa36bf). This was read only. I ran no
probe, gate, anchor or `pf0-*` unit, and touched nothing under `/work/perfgate`, `/work/bench` or `/work/chains`. Line
numbers are plan-rev3.md lines unless a file is named.

Summary: the trust architecture (sibling broker, non-dumpable launch, clone3 placement, holder tmpfs, allowlist view)
is sound, and Sol's N1–N5 closures hold up. The problems are mostly in the parts Sol didn't focus on:
- what the *measured process* actually gets: CPU placement, initial datadir state, the strace pass versus the census;
- the PENDING table;
- what commit 2 does to the bench path and to the global fingerprint.

Every fix below is plan text plus a test. None needs a redesign.

## Major

### M1. The measured spawn's CPU/NUMA placement is unspecified, and node-bench's pinning is silently lost (§3, §8 components 9–13, D31; determinism)

- **Today:** node-bench pins the daemon by prefixing the daemon argv: `node-bench/nodebench/impls.py:465` `spawn_prefix`
  = `setpriv --pdeathsig TERM --` + `pin_prefix(numa:0)`. node-bench also moves itself to the aux slice
  (`workloads.py:341-342`).
- **In PF0 mode:** the daemon is the stub `/pf0/bin/launch`, so that pinning lands on the stub. The real chain
  (`launch --into` → perf → `launch --exec` → bwrap → rdashd) is forked by the broker and inherits *the broker's*
  affinity and memory policy. Nothing in §3–§5 sets it, and no `node_inputs` component records it.
- **Why it matters for the gated counters:**
  - The workspace's storage default sizes RocksDB's thread pool from the affinity mask:
    `dashd-store/src/rocks.rs:77-78` `max_background_jobs = available_parallelism()/4`. tokio and glibc arenas scale
    with it the same way.
  - The thread count moves `page_faults` (thread stacks, per-thread arenas) and `peak_rss_kib`, i.e. two of the three
    gated node counters (`gate.py:91`).
  - The CPU count is not in the fingerprint: `host_meta()["cpu"]` is the model name only (`measure.py:538-540`).
- **Failure scenarios:**
  - (a) pasta resizes the VM's vCPUs, or the manager starts PF0 under a unit with `CPUAffinity=` or `taskset`. The
    node rows shift against the anchor with no stale-anchor ERROR, so a clean merge gets DRIFT/REGRESSION, or a real
    shift is absorbed into a later justified re-anchor.
  - (b) An unpinned multi-threaded node at load 50–190 adds scheduling noise that the bench rows removed with
    pinning (memory note pf0-counter-gate: 5% → 0.03%).
  - (c) Bench rows run with ASLR off (`setarch -R`, same-length exe path), but the node chain doesn't. VmHWM and fault
    counts then keep an ASLR component that D31's fixed argv/env would otherwise remove.
- **Fix:**
  - The broker gives every spawn a fixed placement: `sched_setaffinity` to a deterministic set of N CPUs plus a
    memory policy, applied in the clone3 child before exec. Add `personality(ADDR_NO_RANDOMIZE)` there too; it
    survives the execs.
  - N is a row field (`cpus`, default e.g. 2) hashed in component 13, and the placement code is in component 5/11.
  - PF0 mode passes `--node-slice '' --aux-slice ''` to node-bench (numactl isn't in the view anyway).
  - Tests: `available_parallelism()` inside the jail equals N; changing N changes `node_inputs`; base and head get
    the same N; ASLR is off in the jail (`/proc/self/personality`).

### M2. The census's closed list conflicts with the node strace pass, and it can't tell trusted Python processes apart (D27 ruling, §3 l.176, §11 l.529-531, §14 l.698-703)

- **The conflict:**
  - Node rows default to `syscalls = true` (`spec.py:97`), and `node.measure` runs a strace pass every rep
    (`node.py:145`).
  - §3 keeps it: `strace -f -qq -c -o BRK/C.strace` sits in the outer pid ns, and the protocol has
    `measure: syscalls`.
  - The manager's D27 ruling makes the census a *required gate check* with the closed list (outer_init, broker,
    launch, perf, bwrap). strace isn't on it.
- **Failure scenario:** every syscalls rep fails the census. Every node row is then ERROR, which blocks every merge
  once commit 8 lands. The other way out is to extend the list outside the ruling.
- **The strace pass buys nothing for the decision:** node syscalls aren't in `NODE_GATED` (`gate.py:91`), so they're
  INFO only, and each rep pays a second full invocation for it.
- **The census can't tell trusted processes apart:** it identifies processes by `exe` and pid-ns inode, but outer_init
  and the broker are both `/usr/bin/python3`. Any other Python process in the outer ns passes as one of them.
- **No negative test:** §11 lists none. Because the census is now a gate check, a census that never fires would pass
  silently.
- **Fix:**
  - PF0-N drops the strace pass for node rows. The broker refuses `measure: syscalls`, and node rows force
    `syscalls=false`. This stays inside the ruling. If the syscall INFO is wanted, ask the manager to add strace.
  - Identify census members by identity, not exe: outer_init is ns pid 1, the broker's pid is known to outer_init,
    and every launch/perf/bwrap must descend from the broker through a live spawn.
  - Add a negative test: a test hook starts `sleep` in the outer ns during a measured run → ERROR naming it.

### M3. PENDING|PENDING hides a per-option ok → refused regression, and the anchor check for PENDING rows is unspecified (§7 table l.357-365, D20–D22)

- **The hole:** the table only says PENDING|PENDING doesn't block (with a "was …" note). Take base
  `pending_on=[-b]` and a head that refuses `-a` (which the base accepted) as well as `-b`, so head `pending_on=[-a,-b]`.
  That is exactly the "PR made rdashd refuse an option the base accepted" case that the ok|PENDING row blocks. Here it
  passes.
- **What happens next depends on unspecified behaviour:**
  - If the base-vs-anchor `pending_on` comparison of D20(4) also applies to PENDING|PENDING rows, every *later* PR's
    gate ERRORs with "pending owners changed since the anchor". The regression is blamed on innocent PRs.
  - If it doesn't apply, the regression is invisible until the owner tries to implement `-b`.
- **The same gap for partial implementation:** a PR implements `-a` of `[-a,-b]`. What do later gates do with
  base `[-b]` against anchor `[-a,-b]`?
- **Overloaded status:** `status: pending` already means "no binary target" (`measure.py:482-490`). With two meanings,
  "pending (no bin)" and "PENDING (refused)" can pair silently.
- **Fix:**
  - PENDING|PENDING is ERROR when any option in head `pending_on` is missing from base `pending_on`, compared by
    option. Owner strings come from the PR binary's own refusal line, so an owner-only difference is a NOTE.
  - State what a base-vs-anchor `pending_on` difference does in PENDING|PENDING. I suggest a NOTE plus the same
    "anchor right after the merge" obligation as NEW-FROM-PENDING, recorded in the ledger.
  - Use distinct status names for the two kinds of pending.
  - Tests: head adds a refused option → ERROR; head drops one (partial implementation) → non-blocking with a ledger
    record; owner-only change → NOTE; no-binary vs refusal-pending.

### M4. Commit 2 changes the bench path's launcher and the global fingerprint, and the plan doesn't say how (§13 commits 2 and 7, D24/D25, §9)

- **launch.c is in the global fingerprint:** `measure.counter_hash()` includes the bytes of `launch.c`
  (branch `measure.py:222-242`), and `counter` is in `COMPARABLE`/`ANCHOR_COMPARABLE` (`gate.py:280-283`).
  - Commit 2 rewrites launch.c (three personalities, prctl, handshake), so every bench row's anchor becomes
    incomparable. A second *full* carried re-anchor is needed at the 2–9 merge, beyond the one at 1+1b.
  - `--only` is refused across a fingerprint change (memory note pf0-counter-gate).
  - §13/§14 mention only the 1+1b re-anchor and, in D21, "the carried re-anchor at the merge".
- **The plan adds an unnecessary global dependency:** commit 7 "also extends `counter_hash` with node.py's
  `PERF_EVENTS`". Component 14 already covers those events per node row, so any later node-only event change would
  force a full re-anchor of all ~70 bench rows.
- **The bench `--exec` contract is undefined:** D24 says "every personality", including `--exec`, does the prctl, and
  §3/§4 give `--exec` a socket hello plus a spec memfd.
  - The F0 bench path (285e46b4f) runs `launch --exec -- bwrap …` with no broker and no socket.
  - Either the bench path breaks, or it silently changes, and §9's +122k measurement no longer describes what bench
    rows count.
- **Fix:**
  - Keep the bench `--exec` behaviour byte-for-byte as measured in §9. Give the node personalities their own names
    (e.g. `--spawn`/`--into`/`--client`), ideally in a separate source (`node_launch.c`) hashed only in component 6.
  - Don't add node `PERF_EVENTS` to `counter_hash`.
  - If launch.c must change anyway, say in §13/§14 that the 2–9 merge needs a carried full re-anchor, and repeat the
    §9 no-decision-change check for bench rows with the new binary.

### M5. The measured rep doesn't start from a defined datadir state (D31, §2 prep flow, §7 preflight)

- **Mechanism:**
  - `cmd_workload` always calls `impl.describe()` → `version()` (`node-bench/nodebench/workloads.py:351`,
    `impls.py:76-88`), which runs the daemon with `-datadir=<mkdtemp>`.
  - D31 forbids `mkdtemp` for a daemon argv in PF0 mode, and the broker accepts only `-datadir=/pf0/run/node` (or the
    seed's).
  - So the class-`version` spawn has to use `/pf0/run/node`, in the same invocation as the measured spawn.
- **What the version spawn leaves behind:** rdashd writes the datadir before it acts on options (memory: dash.conf,
  settings.json, blocks/, debug.log are written before the refusal line; and dashd `-version` rewrites settings.json).
  The measured `node-startup-genesis` run then starts on its own `-version` leftovers instead of an empty datadir.
- **Consequences:**
  - A PR that changes what `-version` leaves behind moves the gated counters of a workload that doesn't include it.
  - "the preflight argv equals the measured argv" (D31) holds, but the initial *state* doesn't.
  - The same applies to the preflight spawn if it shares an invocation with a measured spawn.
  - `describe()` also hashes the stub (`sha256_file(self.daemon)`), so the manifest no longer records the PR exe. The
    broker should record it in `res/`.
- **Fix:** the broker (trusted) empties `/pf0/run/node` before every spawn of class start/run/preflight/prep, or every
  spawn gets a fresh sub-tmpfs. Alternatively, version spawns use a separate allowed path (`/pf0/run/version`) that
  isn't in the hashed argv. Test: the measured spawn's datadir is empty, or byte-equal to the PREP copy, at exec.

## Minor

1. **Per-command values in components 8–11; an opaque stale-anchor message (§8 l.408-411, l.425-428).**
   - Placeholders must cover everything that differs between `run anchor` and `run gate`:
     - the holder's `RuntimeMaxSec=<gate timeout>` (the timeout differs by command or option);
     - the side label in `GATE/exe/<side>` (anchor label vs `base`);
     - the holder pid for `--target`;
     - unit names and spawn index.
   - Otherwise every gate reports a stale anchor. D33's test covers `argv_sha256` only; add a test that `node_inputs`
     is equal across `run anchor` and `run gate` with different `--timeout` and labels.
   - Store the 17 per-component hashes in the anchor record so the stale-anchor ERROR names the changed component. With
     D17's tool versions, an unattended `apt upgrade` (python3.14, systemd, util-linux, gcc) otherwise produces an
     unexplained "inputs changed" on every merge.
2. **The closure is rooted at the callee, not the callers (D28, l.62).** `node_sources()` starts from `node.py`. The
   code that decides what is measured and how results merge lives upstream:
   - `measure.measure_nodes`/`escalate`: binary, repeat, escalation;
   - `build.build_bins`: how rdashd is compiled.

   These are covered today only because node.py back-imports `measure` (`node.py:93`). A refactor that moves
   `split_sched`/`collect` drops `build.py` from the hash silently, and the reviewed snapshot would show it only as a
   shrink. Make `measure_nodes`, `escalate` and `build_bins` explicit roots.
3. **D26(a) mixes pid namespaces (§11 l.517-518).** "`/proc/<pid>` of the broker → ENOENT from the view, pids from the
   broker's log" compares outer-ns pid numbers inside the view's ns.
   - Both namespaces number from 1. Broker = 2 in the outer ns and node-bench ≈ 2 in the view, so the check fails
     spuriously, or passes vacuously when the numbers don't collide.
   - Instead, assert from outer_init's side (via `NSpid` in `/proc/<pid>/status`) that the view's pid ns is a child
     of the outer one and that no process visible in the view has the broker's or launch's identity (starttime + ns
     inode).
4. **Wall-clock bounds at this box's load (§5 l.268, §11 l.537-549).**
   - `deadline_cleanup` 5 s and "no survivors ≤ 1 s" are tight. The box has seen 14 s fsync stalls (memory
     agentbox-tmp-quota), and a SIGKILLed process paging its exe/libs from a saturated disk stays in D state. The
     result would be a false ERROR on clean merges. Use ≥ 30 s, and make the test a poll-until with a generous bound.
   - The kill matrix is 7 points × 10 processes × 10 repeats ≈ 700 full chains, i.e. hours at load 100+. Run 1× in
     the implementation gate and the full matrix as an explicit extended test.
5. **"/tmp and /dev/shm moved < 1 MiB" assertions (§11 l.496-497, l.566) flake on agentbox.** Both are shared usrquota
   tmpfs that every thread writes, so `statvfs` deltas are other threads' noise. Assert something per-test instead:
   - no `open*` under `/tmp` or `/dev/shm` in the D36 strace trace;
   - the `RUN`/`PREP`/`GATE` mount sources (`/proc/<pid>/mountinfo`) aren't those filesystems;
   - no file with the test marker exists there.
6. **Unit names and locking (D13, l.47).**
   - Transient scopes named `pf0-<rootid>-…` share the prefix the program uses for manager PF0 units ("check `pf0-*`
     units first", the reviewer rule "never run anything named pf0-*"). Every gate and every perfgate unit-test run in
     a gate copy would create such scopes. Use a distinct prefix (e.g. `pfn-<rootid>-…`, and `pfnt-` for tests).
   - D13's "gates on one root run one at a time under `paths.locked()`, so any unit found is stale" misses
     `run measure --dashd`, which runs node rows without the lock (`cli.py:72-73`). Take the lock there or exempt its
     units from GC, and say how `measure --dashd` runs under PF0-N.
7. **PDEATHSIG through `Popen(preexec_fn=…)` (§3 l.160, l.164).**
   - PDEATHSIG fires when the *thread* that forked exits.
   - `preexec_fn` is unsafe once threads exist, and perfgate uses a `ThreadPoolExecutor` for benches
     (`measure.py:426`).
   - Spawn the holder and the invocations from the main thread, which lives for the whole gate, with a
     `setpriv --pdeathsig KILL` exec prefix (as node-bench does) instead of `preexec_fn`. Otherwise a holder started
     from a worker thread dies when the pool shuts down, which is a false gate ERROR.
8. **`nsenter --target=<holder pid>` has a pid-reuse window (§3 l.168).** Between the holder's death and the gate
   noticing, a recycled pid could be another same-uid process. Open the holder's `ns/user` and `ns/mnt` once while its
   pidfd is known live, keep the fds, and join through them (`nsenter --user=/proc/self/fd/N …` or `setns` on a
   pidfd).
9. **Fixed `GATE` size (§2 l.127, §6).** 64m/1k holds two exe copies. rdashd release is 17.6 MB today
   (`rdc-wt/pf0-n/target/release/rdashd`), so 35 MB of 64. A legitimate size jump (static RocksDB, more of the node)
   makes "GATE full" an ERROR on every gate. Size `GATE` from the two exe sizes plus a margin, counted in the §6
   total, and name it in the ERROR.
10. **The gate-start self-probe is too narrow (D7, l.41).** It probes only clone3. Probe the whole chain at gate start,
    before bench measurement:
    - holder userns creation (`apparmor_restrict_unprivileged_userns` is a dated observation);
    - nsenter, unshare, the view and jail bwraps;
    - `perf stat` counting `instructions:u` inside;
    - `perf_event_paranoid` ≤ 2.

    Otherwise a host change shows up as node ERRORs after an hour of bench runs.
11. **Prep path (future rows; phase 1 mounts no PREP).**
    - (a) The seed is a confused deputy: it shares the netns with the jail, uses the constant RPC credentials, and its
      writable set (`RUN`, and `PREP/<row>` in prep) is larger than the jail's. Run it with `-disablewallet` and
      per-invocation seed RPC credentials (the jail can't read `/pf0/run/seed`).
    - (b) Specify the copy semantics for PR-written `PREP` content copied into `RUN`: regular files and directories
      only, no symlink following, FIFOs/devices/sockets refused (a FIFO blocks the copier until the deadline), sparse
      files counted at apparent size. Any violation is an ERROR attributed to that side.
    - (c) Component 1 hashes `MANIFEST`, not the master's files. Verify the master against it at gate start, at least
      size and mtime per file, with a periodic full sha.
12. **NEW-FROM-PENDING is non-blocking but "needs manager review" (§7 l.361).** The verdict is still PASS, so automation
    or a hurried merge can skip the review. Make it machine-visible: a distinct exit code, or a flagged row that needs
    `--approve-new-from-pending "<why>"`, like BENCH-CHANGED.
13. **Commit testability and the steps after commit 9 (§13).**
    - Commit 2's D26(a) test and the census need the view (commit 3) and a node-bench in PF0 mode (commit 4).
    - The kill-matrix rows that kill the stub or node-bench need commit 4.
    - Either move those tests or say they use a fake node-bench.
    - The §12 "15 runs + 10 no-op advisory gates" step isn't in §13. Place it before commit 8 merges, with a stated
      pass criterion (maximum false-FAIL).
    - Add the post-merge list:
      - the PF0-N merge gate itself runs the *old* harness, so node rows are refused and DEC-213 applies one last
        time; cite it;
      - the carried full re-anchor (M4);
      - anchoring the node and PENDING rows (D21);
      - the DEC number;
      - DEC-213's lapse.
14. possible security note: see manager (§3 l.179-180, jail namespace flags compared with the view's, D5).

## Things checked and found sound

- The F0 closure and the §9 helper-cost method.
- The sibling broker plus non-dumpable `launch` with the hello/SCM_RIGHTS ordering (D24–D25). P16/P16b support the
  kernel facts; the host-uid owner bypassing non-dumpability through userns caps is outside the node-bench threat
  model, and the D36 strace guard relies on it correctly.
- clone3 placement with no fallback (P11/P16).
- The holder tmpfs being quota-free (P14).
- The structural NEW-FROM-PENDING predicate (D20, with D31–D33 making argv deterministic).
- The `.pth` audit injection, which works under `-I` (P17).
- The 17-component hash contains no per-gate value other than those in Minor 1.
- The rendered-equals-real argv test.

## Rulings check

- **D4 (sibling broker, both non-dumpable before receipt, negative test):** implemented (D24–D26, §3 l.188-200, §11
  l.514-528). The view-side half of the test needs the pid-namespace correction in Minor 3.
- **D18 (kernel → row NOTE; global `kernel` unchanged):** implemented (§8 l.422-424). PF0-N changes neither tuple.
  M4 notes that commit 2/7 change the `counter` field of those tuples, which is a separate matter.
- **D21 (anchor records PENDING rows, missing record → not-in-anchor ERROR):** implemented (§7 l.342-343). M3 asks the
  plan to define how those records are compared in the PENDING|PENDING case. That completes the ruling; it doesn't
  reopen it.
- **D27 (census required, closed list in hashed source):** implemented as a requirement (§11 l.529-531, §14
  l.698-703), but it conflicts with the node strace pass (M2). The fix that stays inside the ruling is to drop
  strace from node rows; the alternative is the manager amending the list. Identification and a negative test are
  needed for the "required gate check" to mean anything.
- **Q-A / Q-B / Q-C, D17 (tool versions), D3 (whole node-bench tree), D30 (whole files):** reflected. Their re-anchor
  cost is accepted by ruling; Minor 1 makes such re-anchors explainable.
- **DEC-213:** the draft DEC's lapse wording matches. Minor 13 adds that the PF0-N merge gate itself is the last one
  under DEC-213.

**Verdict: CHANGES-REQUIRED** (M1–M5 are plan-text fixes plus tests; a narrow re-review of them and of Minors 1, 3, 4
and 6 is enough)

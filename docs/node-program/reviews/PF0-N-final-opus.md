# PF0-N final review (Opus, high): `node/pf0-n` at c2905148c, whole branch

Reviewer: Opus, independent. Scope: `git diff 0de28d9edcaf c2905148c` (47 files, +8763/−414), read in a detached
scratch worktree, which I removed afterwards. I made no branch edits. I read plan rev4.1 (§1–§7, §10, the D-rows I
cite), the owner's final report with its DEC draft, and Sol r2.

Trust model I applied: the PR-built `rdashd` is hostile to isolation. Per DEC-76, deliberate forgery of
self-reported counts is out of scope. The base's perfgate and node-bench are trusted.

## Verdict summary

I found no Blocker. Isolation holds end to end as built, and I found no path that yields PASS without a real measured
run of the preflighted argv. The anchor plan is sound, with one sequencing condition (M1). There are three Minors and
three Nits:
- the external RSS cross-checks promised in plan §10 are missing;
- the DEC draft over-claims for the self-reported gated counters;
- two guards are effectively untested.

## Findings

### M1 (Minor, process): land the outer_init nit fix before the merge gate, or the full anchor cannot carry drift

**Where:** `perfgate/anchor.py:60-77` (`carried_drift`), `perfgate/cli.py:325-334`, and the owner's in-flight fix at
`perfgate/outer_init.py:170`.

**Failure scenario:**
- pf0-anchor-t10 predates the `counter`, `ncpu` and `sandbox_label` fingerprint fields (commits 1b/1c), so the
  post-merge `run anchor <tip>` is incomparable and must carry drift.
- `carried_drift` looks only for an authoritative ledger entry whose `measured_tree` equals the anchored tip's tree.
- Suppose the Sol-r2 nit fix lands as a separate commit after the PF0-N merge gate. That gate is the last one under
  DEC-213, and it is the only gate that measures the merged tree with the old harness.
- The anchored tip's tree then differs from every gated tree, so `run anchor` refuses: "no authoritative gate
  measured tree …". The only way past is `--reset-drift`, which silently forgives all bench drift accumulated since
  t10.

Separately, `outer_init.py` is a node-input root (`inputs.ROOTS`, component 5 "sources"). Any later edit to it, or
to `broker.py`, `cgsetup.py`, `holder.py` or `node_launch.c`, even comment-only, makes every measured node row a
"stale anchor (inputs: sources)" ERROR until the next `run anchor <tip> --only node-…`. This is intended (D28/D48),
but it means a post-merge nit fix costs an extra node re-anchor.

**Suggested fix:**
- Put the outer_init fix on `node/pf0-n` before the merge gate, so that the merge gate's `measured_tree` is the tree
  that gets anchored.
- State this in step 2 of the report: "anchor exactly the merged tip whose tree the merge gate measured; nothing in
  between".
- If the fix must come later, do the full anchor first, then `run anchor <tip> --only` for the node rows after the
  fix.

### M2 (Minor, plan conformance): plan §10's external RSS cross-checks are not implemented, and the trusted evidence is discarded

**Where:** `perfgate/nodegate.py:127-154` (`Session.run`). Plan rev4.1 §10 says "§10 unchanged" in rev4.1, but its
promises aren't in the code:
- the `rss-mismatch` flag (|VmHWM − wait4 maxrss| > 4 MiB);
- the `tree-memory` flag (`memory.peak` > VmHWM + max(64 MiB, 50%));
- the test "a fake node that forks a 100 MB child keeps the gated value small and raises `tree-memory`".

`grep` finds none of them in the perfgate or node-bench code or tests.

**Failure scenario:**
- Two of the three gated counters, `alloc_bytes` and `peak_rss_kib`, come only from the preload inside the jail
  (self-reported; the PR can write fd 5 or bypass malloc).
- The broker already records the trusted values in `spawn-k.result.json`: wait4 `rusage.maxrss_kib` and the
  spawn-k `memory.peak`. `Session.run` ignores both, and `Session.close` removes the res tree, so even the evidence
  is gone.
- Plan §10 itself calls these accidental regressions a phase-1 gap that the flags were meant to show:
  - a PR that moves memory into a child (rdashd spawning a helper);
  - a PR that clears `LD_PRELOAD` for a child (`Command::env_clear`);
  - a PR that switches rdashd to a custom global allocator (the preload then sees about 0 allocations; see m1).
- Without the flags, such a regression passes with no trace at all.

**Suggested fix:**
- Return `rusage.maxrss_kib` and `memory_peak` from `Session.run` as INFO samples (ungated, so no anchor change for
  gating).
- Emit the two §10 flags in the row note.
- Add the fork-child test.
- Or record a D-row that defers §10 explicitly and drop "unchanged" from the plan header.

### M3 (Minor, DEC text): the draft says the node is "counted from outside the sandbox", but 2 of 3 gated counters are self-reported

**Where:** the DEC draft in `/work/scratch/pf0-n/final-report.md` ("jailed rdashd counted from outside the sandbox …
Phase 1 gates only the memory signals page_faults, alloc_bytes and peak_rss_kib").

**What the code does:**
- `page_faults` (and the INFO `instructions`) come from `perf stat` outside the jail. That part is accurate.
- `alloc_bytes` and `peak_rss_kib` come from `alloc_preload.c` inside the PR process (`nodegate.alloc_samples`).
  The README's DEC-76 section and plan §10 label them self-reportable; the DEC that lifts DEC-76(b) does not.

**Failure scenario:** A reader of the DEC alone concludes that the node's gated memory counts are tamper-proof. They
are not: a gate-aware or custom-allocator rdashd controls them.

**Suggested fix:** Add one sentence: "page_faults is counted by perf outside the jail; alloc_bytes and peak_rss_kib
are reported by the preload inside the node and are self-reportable under DEC-76 (review, not the gate, defends
them)."

Two other rules the code enforces are worth stating in the DEC, since it replaces DEC-213:
- every measured run must carry the preflight's `argv_sha256` (`nodegate.py:136`);
- a base-measured row whose head is PREFLIGHT-ONLY, or whose head binary is gone, is an ERROR.

### m1 (Minor, verdict robustness): a gated counter falling to about 0 is IMPROVED, not an error

**Where:** `gate.compare` and `judge` for node rows; `alloc_preload.c:7` ("a node with its own allocator shows 0
allocations").

**Failure scenario:**
- A PR sets `#[global_allocator]` (mimalloc or jemalloc) in rdashd. Head `alloc_bytes` drops from about 8.9 MB to
  near 0 and the row is IMPROVED, not blocking.
- After the merge, the drift anchor follows (re-anchor or carried), and from then on `alloc_bytes` gates nothing for
  that row.
- This is an accidental loss of signal, not forgery.

The workspace allocator-declaration checks in `selftest-checks.sh` may catch the declaration in review; I didn't
verify that they cover `rdashd`.

**Suggested fix:** When a gated node counter's head median is under 1% of the base's, or `allocs` is 0 on one side
only, make the row an ERROR ("counter source changed: approve and re-anchor"). Alternatively, record the allocator
symbol the preload resolved in the alloc record and compare it across sides.

### N1 (Nit, test coverage): `Session.run`'s `alloc_errors` guard has no test

**Where:** `perfgate/nodegate.py:152` (`if runs[0].get("alloc_errors") or not records`).

**Mutation:** dropping `runs[0].get("alloc_errors") or`. The mutant survives `test_noderows.py`, `test_pf0n.py` and
`test_chain.py`. The broker side is tested (`test_broker.py:387, 462, 473`); the consumer is not.

**Failure scenario:** A refactor that drops it would grade runs whose records were rejected, for example "no alloc
record from the jail's pid 1", where only the startupnotify shell's tiny record remains. Such a run reads as a large
IMPROVED.

**Suggested fix:** In `test_noderows`, add a unit case with a fake res dir whose `spawn-1.result.json` has
`alloc_errors: ["no alloc record from the jail's pid 1"]` and one record. Expect `RunError`.

### N2 (Nit, test coverage): the D81 guard is tested only on a branch that runs about once in 16 tries

**Where:** `perfgate/broker.py:957-960`. Its test is `test_broker.py:488-506`, whose "chain ended early" branch is
taken only when the race fires.

**Mutation:** `if False and …`. The mutant survives `test_broker.py` (the run took about 1 min, at load about 75). The
`not records` check in `Session.run` backs it up today, so this is defence in depth losing its own test.

**Suggested fix:** Add a deterministic case: a test-hook `--jail` that exits 0 before its hello (or a `chain_argv`
hook replacing the jail step with `true`). Expect `before the jail ran` in the spawn's errors.

### N3 (Nit, docs): stale wording in `sandbox.py`

**Where:** `perfgate/sandbox.py` module docstring, "(F0, PF0-N; node runs still count inside their sandbox until
PF0-N's node launcher)".

The node launcher is in this branch, so the sentence is now wrong. Also, a non-blocking `KeyError` at
`nodegate.py:154`: `perf["instructions"]` / `perf["page-faults"]` raise `KeyError` (not `RunError`) if perf's CSV
lacks an event. `Session.repeat` then lets it crash the gate. The gate still ERRORs through `cmd_gate`'s catch-all,
so the result is not a PASS, but the row loses its name.

**Suggested fix:** Fix the docstring. Turn a missing event into `RunError(f"{w.name}: perf did not report …")`.

## Focus areas: what I checked and concluded

### 1. Isolation, end to end

- **Jail (`broker.jail_args`, `node_launch.c --jail`):**
  - The hardening:
    - `--cap-drop ALL --unshare-user --disable-userns --unshare-pid/ipc/uts/cgroup --as-pid-1 --new-session
      --clearenv`;
    - one writable bind, by descriptor (`--bind-fd 8`, D80);
    - the exe and the preload read-only;
    - a private `/tmp`, no `/sys`;
    - stdin `/dev/null`, everything above fd 8 closed before the exec of bwrap.
  - My main worry was fd 8 surviving into rdashd. `/proc/self/fd/8/..` would walk to RUN's root and reach
    node-bench's `manifest.json`. `test_broker.py:333` and `:517` pin the node's descriptors to exactly
    `[0,1,2,(listing),5]`, so bwrap closes the bind fd, the spec and the info fd. The stdio fds are a regular file on
    RUN or pipes (`stdio_problem`, enforced and tested in `test_chain.py:316`; the mutant dies there). A file magic
    link cannot be walked with `..`. No traversal is possible.
- **Spec and handshakes:**
  - The spec and command are sealed memfds checked by `sealed()`.
  - The cgroup fd goes only to `--into`, and only after `{hello, dumpable:0}`.
  - Every personality calls `PR_SET_DUMPABLE 0` first.
  - The helper `close_range`s everything but its socket.
  - The broker's Popen uses `close_fds`, `pass_fds` and `env={}`. Received `SCM_RIGHTS` fds aren't CLOEXEC, but
    `close_fds=True` covers them.
- **Environment:** outer_init and the broker inherit the harness environment through `systemd-run --scope`. Nothing
  passes it further on: the helper gets `env={}`, and the view and the jail use `--clearenv`.
- **Outliving the jail:** rdashd is the jail's pid 1 (setarch execs it), so the pid-ns teardown kills every descendant
  on its exit. `cleanup_step` writes `cgroup.kill` until `cgroup.procs` is empty and only then `rmdir`s and replies.
  No PR process can be alive when the status reaches node-bench. The jail cannot leave spawn-k: the cgroup ns is
  rooted there and there is no cgroupfs or capability. Nothing PR-written persists across reps or sides:
  - RUN is a fresh tmpfs per invocation, and so is the netns;
  - the datadir is recreated with fd-based `rmtree` + `mkdir` per spawn;
  - GATE and the host are read-only (`mount_setattr` recursive RDONLY, then only res, rw and the own scope cleared).
- **D103 (reaped jail):**
  - ESRCH from `pidfd_open` can only mean the info pid is gone. The jail is bwrap's child in the broker's own pid ns,
    so a live jail is always visible.
  - A reused pid that is live and outside spawn-k is killed. That gives a spurious ERROR at worst, never a PASS, and
    it needs a pid wrap within one spawn, which isn't plausible.
  - A reused pid that already exited is treated as reaped. The status still comes from the helper's wait4, and
    `finish` still requires the jail hello and an info record (D81).
  - No live jail can be hidden.
- **D104 (root-cgroup deferral):**
  - Only depth-1 (outer pid ns) processes are judged at all, and PR processes are depth 2 or deeper. So PR code cannot
    use the deferral; it applies only to trusted forks in flight.
  - Related to Sol's nit (not re-reported): the loop also does no census pass after both children are reaped. A
    trusted-looking outer-ns process created in the last ≤ 0.25 s before exit is never judged, deferred or not.
  - The fix should therefore be one unconditional full pass after the reap and before `outer.json`, with no
    deferral on that pass. That closes both gaps; it costs one `/proc` walk.
- **pid reuse elsewhere:** the census keys on `(pid, starttime)`, `unplaced` the same way, and helpers are registered
  with their starttime. I found no problem.

### 2. Verdict correctness: no PASS without a real measurement of the preflighted argv

- **Measured runs** (`Session.run`) require all of:
  - zero chain errors, census included;
  - manifest status ok;
  - an `argv_sha256` equal to the preflight's (mutant dies: `test_noderows`);
  - exactly one class-run, measured spawn with `{"exit": 0}`;
  - a parseable perf CSV from the broker's memfd;
  - alloc records with no errors (N1).

  The argv hash is computed by trusted node-bench from the same `impl.argv(cfg)` builder for the preflight and the
  run (`workloads.py` `genesis_config` / `fixture_config`). RUN's root, where the manifest is written, is out of the
  jail's reach (above).
- **REFUSED and PREFLIGHT-ONLY rows** never produce counts. `compare` skips them, and `refusal_rows` and `drift_rows`
  apply the §7 table. My mutants confirmed:
  - ok → refused is REFUSAL-REGRESSION;
  - a head refusing a superset blocks;
  - the NEW-FROM-REFUSED predicate requires attempts == 1 and accepted_by ready, plus the refused option present in
    the argv;
  - a base error is ERROR first (`test_perfgate`);
  - anchor refuses node rows that errored.
- **Pending:** base ok with head pending gives ERROR. Base refused with head pending gives ERROR "binary target
  removed".
- **Escalation** reuses each side's preflight `argv_sha256`. A failing escalation sets `escalation_error`, which gives
  an ERROR.
- **The D93/D99 exit-0 rule** (`pf0.run_attempt`): both ready and settled acceptances must end in exit 0 after
  SIGTERM. A late refusal goes back to `preflight()` only with exit 1; anything else is a `PreflightError`.
  - Residual risk, not a finding: the "settled" acceptance relies on an rdashd property (ND1): a start stopped before
    its option check exits with its refusal or by the signal, never 0. A PR that reorders rdashd's SIGTERM handling
    ahead of option checks could shrink its own `refused_on`. That leads to "narrowed" plus an anchor obligation,
    never a measured PASS, so it is a liveness cost, not a correctness hole.
  - Under extreme load, a start not yet in its signal handler at 30 s dies by signal and the row ERRORs. The 13
    no-op gates at load 71–231 showed none.
- **Crashes:** a KeyError from a perf CSV with a missing event crashes the gate rather than naming the row (N3), but
  that still ERRORs.

### 3. Anchor flow (D97)

One full `run anchor <merged tip>` is safe as designed:
- `cmd_anchor` requires the harness of that rev (M3 check), refuses node rows that errored, and records REFUSED and
  PREFLIGHT-ONLY rows with key, argv hash and `{option, owner}`.
- Bench drift is carried from the merge gate's head.json across the fingerprint change.
- Node `inputs` are computed at the anchored sha from the running (= that rev's) harness, so later gates' bases
  compare equal until a node-input root changes.
- The one trap is M1: the anchored tree must be the merge-gated tree.
- Operational note: `tools` (bwrap, perf, systemd, python, util-linux, cc versions) and `cpu_list` are node inputs.
  A package upgrade on agentbox therefore turns every measured node row into a stale-anchor ERROR until
  `run anchor --only` (D17, by design). The manager should expect that after apt upgrades.

### 4. Draft DEC text

The refusal, settle, NEW-FROM-REFUSED, obligation and DEC-213 lapse rules match `gate.refusal_rows`,
`_anchor_problem`, `_new_from_refused_problems`, `drift_rows` and `pf0.run_attempt`. I checked each clause against
the code. The exceptions are M3 (self-reported counters) and the two unstated rules M3 lists. "REFUSED … does not
block" is true only with a matching anchor record; the later obligation sentences cover that, so I make no separate
finding.

### 5. Tests: mutation spot-check (scratch worktree only, each mutant restored by `git checkout`)

| guard | mutation | killed by |
|---|---|---|
| `nodegate.py:136` argv hash | `if False:` | `test_noderows` WiringTest |
| `nodegate.py:152` alloc_errors | drop the clause | **survives** noderows, pf0n, chain (N1) |
| `gate.py` ok→refused | REFUSED instead of REFUSAL-REGRESSION | `test_ok_to_refused` |
| NFR attempts/ready | `if False:` | `test_new_from_refused_predicate [retried]` |
| NFR option in argv | `if False:` | `test_new_from_refused_predicate [not in argv]` |
| head ⊄ base refusals | `if False:` | 3 tests incl. `LateRefusalGateTest` |
| base error first | `if False:` | `test_perfgate` `test_base_error_is_never_a_silent_pass` |
| anchor refuses errored node rows | `if False:` | `test_anchor_refuses_a_node_row_that_errored` |
| broker one live spawn | `if False:` | `test_one_spawn_at_a_time_and_one_measured_spawn` |
| broker datadir per class | `if False and …` | `test_requests_outside_the_protocol_are_refused` |
| broker stdio regular file on RUN | `if False:` | `test_chain` `test_stdio_file_off_run_refused` |
| broker D81 jail ran | `if False and …` | **survives** `test_broker` (N2) |
| pf0 `_token` original-argv clause | `False and` | survives: an equivalent mutant (cur ⊆ original) |
| pf0 "refused twice" | `if False:` | survives: an equivalent mutant (the token is already gone → "not in the row's argv") |

Baselines: `test_noderows` 25/25, node-bench `test_pf0` 36/36, `test_broker` OK (1 m 04 s at load about 75). I did not
rerun the full gate. Sol r2 already re-verified D99, D103 and D104 against their pre-fix sources.

Other checks:
- `selftest-checks.sh` is byte-identical to integration's 4f30a0aab.
- `node/integration` (5dc4ef4ce, 171 commits past 0de28d9ed, none in perfgate or node-bench) merges with c2905148c
  without conflicts (`git merge-tree`).

## Not findings, for the record

- The prep/master path (D60/D61 master `MANIFEST`, `node-root init`, `Holder.mount_prep`) is dormant: `noderow.prep()`
  returns None for every row (D86). Before a prep row is enabled, the D61 master check must be implemented; it isn't in
  this branch.
- `gc()` removes every `g-*` dir under the root's `node/`, whatever the prefix. That is safe only because gates,
  anchors and `measure --dashd` all hold `paths.locked()`, which this branch extends to `--dashd`.

VERDICT: APPROVE-WITH-NITS

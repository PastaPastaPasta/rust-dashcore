REWORK

## Major

### 1. The acceptance run is not protected against load drift, and the scheduler does not make it a paired ABAB experiment

- **Rust/Python:** `contrib/node/sp1b-timing/timing.py:578-592,635-658` (the `pick` scheduler and load recording); design claim `docs/node-program/designs/SP1-notes.md:51,53,59-65`.
- **Core:** `src/node/sync_manager.cpp:22-29,157-162` (the 1-second scheduler and integer-second tick gate).
- **Problem:** `SP1B_MAX_LOAD` is only a start condition in the wrapper; `run` never aborts, pauses, or marks the run invalid when the load rises. The scheduler balances completed plus in-flight counts, but it does not pair a dashd session with an rdashd session at the same time or record load for each sample. With 128 workers, the first dispatch is a large concurrent batch (32 sessions for each of the four node/kind pairs), and later dispatch depends on which implementation finishes first. If one implementation is slower, its samples are preferentially collected later in a changing load environment.
- **Concrete scenario:** The supplied run started below the nominal ceiling but recorded 1-minute load min/mean/max of 4.5/50.2/227.5 and still produced a passing verdict. A rising external load can lengthen the wall-clock intervals around Core's scheduler ticks differently for the two implementations; count balancing cannot remove that time correlation. The reported p-values therefore do not establish Core parity for this run, even though the four binned p-values exceed 0.05.
- **Fix:** Make the load condition part of validity: enforce a ceiling throughout sampling (abort and resume/restart the affected segment when it is exceeded), or run matched node pairs under a controlled load and record per-session timestamps/load so the comparison can adjust or stratify. Then repeat the acceptance run under the declared condition.

## Checked and correct

- **Permutation KS:** `ks_d` takes the two-sample empirical-CDF maximum after consuming tied values together (`timing.py:135-148`). `ks_permutation` pools both samples, shuffles without replacement, preserves each sample size, uses the observed-statistic tail, and applies the +1 correction (`timing.py:160-170`). The minimum reported p is therefore 1/1001. For the 10,000-versus-10,000 acceptance samples, the binned D values agree with the recorded table and the raw D values agree with the raw column. The supplied selftest also checks a brute-force integer-domain statistic against `ks_d`.
- **Resolution:** 10 ms is a defensible, explicitly declared verdict resolution for the timer-level requirement, provided the raw columns remain visible. It has a real blind spot for differences smaller than a bin; the acceptance data demonstrates this for `gap_tick` (binned D=0.0035, p=0.270 versus raw D=0.0822, p=7e-30). That is acceptable only as the stated resolution policy, not evidence that the raw distributions are identical.
- **Measure definitions:** The four gated measures correspond to Core's behavior. `first6` and `first30` wait for the `mempool` sent when the BLOCKCHAIN asset advances after the 6-second or 30-second timeout (`timing.py:450-489`; Core `src/node/sync_manager.cpp:204-238`). `reset_tick` and `gap_tick` use the node's own disconnect log lines, which measure the SyncManager decision and the following tick rather than the separate socket close path (`timing.py:467-481`; Core `src/node/sync_manager.cpp:196-201`). Starting the reset interval immediately before the reset RPC is correct: `Reset(true)` mutates state during the call, and a tick may run before the RPC reply. Dropping a separate FINISHED-to-mempool interval is consistent with Core sending the mempool request while leaving BLOCKCHAIN, before FINISHED (`src/node/sync_manager.cpp:204-238`).
- **Failure accounting:** Normal session exceptions, including event timeouts, are recorded in `errors.tsv` and retried (`timing.py:607-633`); `ks` rejects a pair with too few samples or a failure share above 1% (`timing.py:683-712`). The acceptance artifacts contain 10,000 good sessions for every node/kind and zero recorded failures.
- **Isolation/accounting:** Each session gets a fresh 0700 datadir and unique p2p/RPC/listener slot (`timing.py:214-227,564-612`); per-node RPC passwords prevent a stale process on a reused slot from answering. Session IDs and per-measure rows are written under the ledger lock, and the acceptance sample has 160,000 rows with exactly 10,000 sessions for each of the four node/kind pairs.

## Commands and results

- `git -C /work/scratch/sp1b-r1/wt log --oneline 31c3c81a6..f8c61e079` — 9 commits, 12 files, reviewed at frozen head `f8c61e079`.
- `python3 /work/scratch/sp1b-r1/wt/contrib/node/sp1b-timing/timing.py selftest` — **passed** (`selftest ok`).
- `python3 -m py_compile /work/scratch/sp1b-r1/wt/contrib/node/sp1b-timing/timing.py` — **passed**.
- Acceptance-artifact audit of `/work/scratch/sp1b/accept1` — 160,000 sample rows; 10,000 good sessions per node/kind; no failures; summary load min/mean/max 4.5/50.2/227.5.
- Independent D recomputation from `accept1/samples.tsv` — matched the recorded gated rows: first6 0.0128/0.0132 (binned/raw), reset_tick 0.0129/0.0133, gap_tick 0.0035/0.0822, first30 0.0155/0.0156.
- `git -C /work/scratch/sp1b-r1/wt diff --check 31c3c81a6..f8c61e079` — reports the fixture's intentional trailing spaces on transcript lines (the newly captured wire records); no timing.py whitespace issue.
- Full 10,000-session acceptance was not rerun, per the brief. No files were written in the frozen worktree; scratch was kept under `/work/scratch/sp1b-r1/sol-scratch/`.


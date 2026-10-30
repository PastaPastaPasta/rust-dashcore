# BL-173 narrow review — Sol r3

**Verdict: CHANGES REQUIRED**

## Findings

1. **High — A later real timeout can be hidden and its old cluster closed as phantom.** `node-fuzz/fuzzctl.py:552-565,1104-1112`. If a merge child dies after its phantom `timeout-*` write but before its own ERROR, `without_timeout_report` still looks for the first timeout ERROR after a `MERGE-OUTER` boundary. It discards the next child's unsaved timeout report. `legacy_unsaved_crash` then calls that real timeout the earlier artifact's own report, and `close-phantom` can mark the vectorless cluster fixed. I reproduced this with an interrupted empty write, `MERGE-OUTER: attempt 2`, and a later timeout ERROR without another write: `log_evidence` returned no unsaved crash, the member was marked `phantom=True`, and `--apply` closed the cluster. End eligibility for the saved artifact's report at a new-attempt boundary even if its ERROR never appeared; test the torn-before-ERROR case through both `log_evidence` and `close-phantom`.

2. **Medium — A newer phantom can replace a missing historical real hit in the count.** `node-fuzz/fuzzctl.py:1122-1145,1156-1167`. `worker_logs` lists only files still present, while `len(members) >= hits` counts phantoms from runs triaged after D11 that never contributed a hit. I reproduced an old cluster with two hits in one run (one phantom worker, one real unsaved timeout worker), followed by a post-D11 phantom run. After the real worker log disappeared, the two remaining phantom logs met `hits=2` and the first/last-run check, so `--apply` closed the real finding. The farm does not normally delete logs, but missing log evidence must fail closed as missing runs and unreadable logs do. Tie each historical hit to its original run and worker, or conservatively keep clusters when original hit attribution or expected worker logs cannot be proved; later phantom members must not make up a missing old hit.

## Requested checks

- The D12 flag prevents re-arming after the first timeout ERROR: a second ERROR and a later timeout after a completed or already-started report remain visible. Finding 1 is the gap before that first ERROR.
- `fixed_stamp=""` makes a later triaged hit reopen the cluster. Every decision and audit row is built before writes for that target; the audit append is fsynced before the state save. New audit files use mode 0600.
- Private-target details are filtered from stdout; only aggregate counts are printed. Their listing file uses mode 0600 in a mode 0700 directory.
- The normal guards retain non-empty input timeouts, empty timeouts with no prior interrupt, non-timeout crashes, sink failures, and vector-bearing clusters. Findings 1 and 2 show ways a real vectorless timeout can still be closed. Open vectorless clusters block affected-leg certification until actually repaired.

## Validation

`python3 -I node-fuzz/fuzzctl.py selftest`: **186 tests passed, 5 skipped** (real-window tests need a built node-fuzz binary). Both findings were reproduced in scratch-only synthetic trees. Review was read-only; the worktree stayed clean.

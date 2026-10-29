# PF0-N final independent review

Reviewed `0de28d9edcaf..8dc7929ae` at the requested detached worktree. I treat the PR-built `rdashd` as untrusted, the base's node-bench and perfgate sources as trusted, and a ready signal as evidence only if that same start does not subsequently report a refusal. I did not edit the branch or any shared file.

## Findings

### Blocker — a ready preflight ignores the node's refusal on exit

**Location:** `node-bench/nodebench/pf0.py:226-245`; consequence in `node-bench/nodebench/workloads.py:405-408` and `contrib/node/perfgate/perfgate/gate.py:532-550`.

**Failure scenario:** `run_attempt()` checks the post-SIGTERM exit code and refusal line only for `how == "settled"`. If a node first answers the fixture row's RPC readiness probe and then prints the exact `not_supported()` refusal line and exits 1 on SIGTERM, the function returns `("ready", "ready")`. The four fixture rows have `measured=False`, so the head becomes `PREFLIGHT-ONLY`; the gate makes no measured run and can return PASS even when the head newly refuses an option that the base accepted. A file-based ready marker left by a preceding retry can also be observed before the broker replaces `/pf0/run/node`, causing the same premature ready classification. The code's comment at `pf0.py:169-171` assumes a previous refusal makes that harmless, but a later refusal can be omitted from `refused_on`.

**Reproduction:** In a scratch copy of the existing fake stub, I made its HTTP handler answer the ready RPC and then print `Error: -mode is not supported by rdashd yet (XX1).` and exit 1 on SIGTERM. `run_attempt(..., ready="rpc")` returned `('ready', 'ready')` despite the refusal in its log. With the existing gate fixtures, `base=REFUSED(-txindex), head=PREFLIGHT-ONLY` gives a `PREFLIGHT-ONLY` row and verdict PASS without head samples. A pre-existing file marker plus the existing `dirty` fake stub (exit 3) likewise returned `('ready', 'ready')`.

**Suggested fix:** Apply the settled path's post-stop classification to ready attempts too: exact refusal with exit 1 must feed the retry logic, a clean exit 0 may be accepted, and other exits must be ERROR. For file readiness, ensure the marker belongs to the current broker spawn before treating it as ready. Add tests for ready-then-refusal, ready-then-nonzero, and a stale marker across retries.

### Minor — the high-load no-op validation is still pending

**Location:** `/work/scratch/pf0-n/plan-rev4.1.md:153` (D98); original criterion at `:900-905` and results under `/work/scratch/pf0-n/noise/`.

**Failure scenario:** The first noise series had 12/15 measures at load >= 50 but only 1/10 no-op gates at that load. This leaves the false-failure rate at representative load unmeasured. The 0.313% page-fault spread exceeded the original 0.25% half-limit; D96r at plan line 152 explicitly records the manager's decision to retain the 0.5% limit, so I do not request a limit change on that basis. D98 orders ten no-op gates on the final head at load >= 50; at review time its result was pending (only rerun gate 1 had started). The original ten reports also have overall verdict ERROR because the private root has no anchor, though their measured node rows showed no false FAIL or escalation.

**Suggested fix:** Finish D98 before merge and record the loads, row statuses, escalations and verdicts. If the private root still lacks an anchor, judge the counter rows explicitly and avoid describing an overall ERROR report as a passing gate.

## Checks and disposition

- **Isolation:** I found no branch defect in the reviewed boundary. The holder copies and hashes the binary in its private tmpfs before ready; the harness opens namespace fds after ready and checks their inodes; the outer process closes inherited fds and makes the host read-only before starting the view; the broker holds RUN/PREP roots by descriptor, rejects symlinks in copied trees, passes the datadir by fd, and receives only bounded stdio descriptors. The census drains broker registrations and rescans a suspect process before erroring. The jail's visible mounts exclude `/work`, the ledger, anchors, other gate roots and the node-bench data-root template. These are code and test inspections, not an independent live attack run.
- **Verdict flow:** `--repeat` is at least 5 for a gate, a successful measured node row requires one run-class spawn, its perf and allocation records, and the same argv hash as preflight. `pending/pending` is intentionally nonblocking per the plan; removing a binary when the base has one is an ERROR. The Blocker above is a distinct path to PASS without measuring a fixture row.
- **Anchor:** `git merge-tree --write-tree 22e458649 8dc7929ae` equals `4a8b163ae7d6f9deb93a8182498f9492b964a776`, the reported merged tree. The old harness's node records are not comparable to new measurements; one full anchor under the merged harness is necessary after the merge and is the correct re-anchor shape. I found no issue in the stated D97 merge shape.
- **Tests:** The unmodified D93 settled-exit and D92 argv-check tests passed. In separate scratch copies, deleting each respective guard made its targeted test fail. I did not build or rerun the reported 4,915-test gate; I used its existing log only as supporting evidence.

VERDICT: CHANGES-REQUESTED

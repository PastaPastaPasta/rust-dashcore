# BS6b — Sol review, round 4

2026-10-18. Narrow review of **R1-B1 only** at `campaign/bs6b-r1` commit `ff67f53ff9444bc6cab7e48cf47a9a5f866824cb`. The r2 code and seed findings remain closed. PLAN §7 handling: target names, counts, digests and run identifiers only. DEC-252 is taken as the manager's ruling, not reviewed here.

**Verdict: ACCEPT WITH NITS.** The reruns close R1-B1 under DEC-184's strict last-60-active-minute rule. `diff_payload_1` and `diff_payload_9` remain uncertified under DEC-186, as the receipt and README say; DEC-252 permits the evidence row with that status and zero divergences.

## Independent window check

I read the private farm ledgers, seed snapshots and per-process libFuzzer logs for the three evidence nights. For interrupted slices, I bounded each process's active time using its final execution count and truncated average exec/s: `executions / (rate + 1)` through `executions / rate`. The lower bound alone establishes a full 3,600 seconds in each leg. I counted only that leg's segments, once per process, excluding gaps and other legs. Status-line maxima and the final-hour baselines agree with the committed receipts.

| Leg | Evidence night | Required seeds: count / digest | Eligible slices | Active seconds, lower–upper bound | Last-hour start → end cov/ft | Gain | DEC-184 | Latest DEC-186 |
|---|---|---|---:|---:|---|---|---|---|
| `diff_payload_1` | `bs6b-r3` | 153 / `6b0340c09493a062` | 8/8 | 4818.32–4825.11 | 1485/6201 → 1485/6202 | +0 cov, +1 ft = 0.0161% | PASS | `growing` |
| `diff_payload_3` | `bs6b-r2` | 153 / `5d6a757951b14307` | 4/4 | 5190.32–5191.99 | 507/1255 → 507/1255 | +0 cov, +0 ft | PASS | `certified` |
| `diff_payload_9` | `bs6b-r5` | 296 / `ccd22412ce843b74` | 2/2 | 3644.49–3647.03 | 1068/1874 → 1068/1874 | +0 cov, +0 ft | PASS | `growing` |

All touched slices in those windows have timely required-set snapshots and initial libFuzzer seed counts above the required counts. The `diff_payload_3` window lies wholly in its fourth r2 slice. The `diff_payload_9` window begins about 44 seconds into its first r5 slice and includes its second slice. Its narrowest duration margin is still over 44 seconds under the conservative clock, while the early status lines already show 1068/1874. The r3 `diff_payload_1` window touches slices 3–8, all eligible. No previous night's slices enter any of these windows.

The r2, r3, r4 and r5 ledgers contain respectively 12, 8, 5 and 2 completed/interrupted rerun slices, each with zero recorded divergences, crashes and sink failures. Artifact-directory counts are also zero for all 27 slices. Two early interrupted r2 slices lack a `verdict.json`; their ledger counts and empty divergence/crash directories supply the available evidence. All other slice verdicts agree with the ledgers. The committed r2–r5 JSON receipts match the private receipts exactly. Regenerating the README table from the committed r5 JSON with `python3 -I readme.py` produced identical text.

`receipt.py:101–102`, `:138–183` now require 3,600 seconds of the leg's active clock, zero coverage gain and feature growth at most 0.001. The window rejects any touched slice without the required seeds. `:251–256` treats a started leg as that night's leg even if it has no finished slice, so it cannot silently inherit a prior plateau. `:299–305` derives failure and status from the strict verdict; the old loose `flagged` and exact-cov companion acceptance paths are gone. `:79–85` and `:224–270` count worker executions once. The old BL-86 inflated ledger totals remain separately identified as `executions_ledger_all`; for example, the carried `diff_payload_2` row has 23,236,577 worker executions versus 46,473,154 in the old ledger. This accounting difference does not affect the window or finding counts.

The README's certify table agrees with the nine certificate records: `diff_payload_1` has one failed setup and three `growing` runs; `diff_payload_3` has a failed replay comparison followed by a certified run; `diff_payload_9` has three `growing` runs. Its latest replay gains two features, 2/1863 = 0.1074%, even though its certification window itself is flat. No certificate record reports a divergence or crash. These DEC-186 statuses are reported as context, not independently re-certified in this narrow review.

## Nits

- **N1 — nit — `node-fuzz/campaigns/bs6b-rerun/README.md:17–20`.** The r4 explanation says its eligible slices were only **4 seconds** short. The first r4 slice with the wrong seed digest used about **239.8 active seconds**; the leg used about **3706.2 active seconds** total. Thus only about **3466.4 eligible seconds** follow that slice, about **133.6 seconds short** of an eligible hour. Correct the duration. The r4 receipt properly fails and the r5 rerun independently passes.
- **N2 — nit — `node-fuzz/campaigns/bs6b-rerun/README.md:62`.** The table labels its column simply “Executions,” while the source receipt distinguishes corrected worker-only totals from BL-86's duplicated historical ledger totals (`receipt.py:32`, `:268–270`). A short README note that these are worker-only totals would prevent readers from comparing them as though they were the old ledger figures. The displayed totals themselves are correct.

No reviewed branch was edited, committed or pushed. Only this review file was written in the workspace.

Verdict: ACCEPT WITH NITS

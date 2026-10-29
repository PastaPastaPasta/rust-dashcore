# BL-313 perfgate drift fixes: review round 1 (Opus)

- Branch `node/bl313-perfgate-drift` at `74ead82ae`, base integration `0212e7785`
- Scope: `git diff 0212e7785 74ead82ae` (anchor.py, cli.py, README.md, tests/test_bl313.py; no Rust, no gate.py, no ledger code)
- Verdict: **ACCEPT WITH NITS**. Both fixes are correct, and all three requested mutants are killed. The nits are test gaps and audit-trail improvements; none blocks the T24 rider.

## Verification of SB1's claims

| Claim | Result |
|---|---|
| Fix 1: a counter at ≤ −100% carried drift is anchored at its measured median and recorded `uncarried` in anchor.json and anchors.jsonl; the row's other counters still carry; each uncarried counter is logged | **Verified** (anchor.py:119-128, 144-145; cli.py:358-361). Probe and test output: `not carried (fell to 0, so the measured median is anchored) a: page_faults -100.000%` |
| Fix 2: `--reset-drift` starts from zero after any fingerprint change, even with a ledger gate of the tree | **Verified** (cli.py:337-338) |
| `--reset-drift` with no anchor fails before measuring | **Verified** in code (cli.py:312-313, before `measure_rev` at :314), but **no test pins the ordering** (N2) |
| `--reset-drift` with a comparable fingerprint fails before recording | **Verified** (cli.py:332-335; `anchor.record` at :350 is the only writer of anchor.json and anchors.jsonl, and `cmd_anchor` never writes ledger.jsonl). It does run after measuring, which writes only the measurement cache and the base worktree (see O3) |
| test_bl313: 5 tests; all fail on 0212e7785, 3 with ZeroDivisionError; all pass on the fix | **Verified.** Base: `FAILED (failures=2, errors=3)`, 3× `ZeroDivisionError`. Fix: `Ran 5 tests … OK` |
| Full perfgate suite passes; ruff clean | **Verified.** `Ran 282 tests in 244.6s OK` (TMPDIR=/work/tmp); `ruff check` in `contrib/node/perfgate` (its ruff.toml): `All checks passed!`; no added line over 120 columns |
| DEC-299: perfgate is not in SELFTEST_TRIGGER | **Verified.** `/work/scratch/gate-fast/lib.sh:170` matches only `contrib/node/{check-*,selftest-checks.sh,*-{deny,clippy}.toml,l5-exceptions.py,link-probe.py,base_sdk_pins.py,test-check-products.py,pin-delta/check-release-refs.sh,selftest-rocksdb.py,test-selftest-rocksdb.py}` and `node-fuzz/fuzzctl.py`. Note for the manager: DEC-305 and the BL-313 row say "selftest per DEC-299", so the mechanical answer ("not triggered") should be what the acceptance cites |
| Tier 1 6,609/1, failure out of diff | Not rerun. The diff contains no Rust, so a Rust test failure (realv_peers.rs:760) cannot come from it |

## Q1: Fix 1 correctness

- **The boundary is right.** For counts, `drift = v/old − 1 ≥ −1`, so d = −1 means the gate measured exactly 0. Carrying needs `measured / (1 + d)`, and at d = −1 the "equivalent old value" is unbounded. Anchoring at the measured median is the **strictest** choice: no headroom, so the improvement is not banked. It cannot hide a regression. It can only make the next drift check tighter than a carry would.
- **d < −1** can't happen for non-negative counters. If some counter could go negative, `1 + d[k] <= 0` (anchor.py:121) still routes it to uncarried. Probe: old 5, gate −5, d = −2 → `uncarried: {'neg': -2.0}`, anchored at the measured 1.0.
- **0 → nonzero**: `anchor.drift` (anchor.py:59) skips a counter whose old median is 0, so there is no d, no division and no NaN. The counter is stored at its measured median, but it appears in **neither** `carried` nor `uncarried` (probe: page_faults 0 → 4 → anchored 4.0, absent from both). This predates BL-313 and is not a crash. It is the mirror image of the case BL-313 now records, though (N3).
- **Tiny base, huge finite drift**: carried as designed. Probe: allocs 1e6 → 1 (d = −0.999999) with 2 measured → anchored 1,999,999.99. That follows DEC-301's carry semantics ("resets nothing"), and the per-merge comparison still catches each regression. The behaviour is discontinuous at d = −1 (huge headroom just above it, none at it), but that is the conservative side. On small integer counters such as page_faults, a carry also multiplies noise (2 → 1 gives d = −0.5 and ×2 headroom). page_faults' fixed floor of 1 (gate.py:82) bounds the effect. Observation only.
- **Effect on later PF0 runs**: none beyond the stored median. `gate.drift_rows` (gate.py:453-519) reads only `medians`, `noise`, `iters`, `key` and `inputs`, never `carried` or `uncarried`. An uncarried counter is judged like any freshly anchored one: its drift starts from zero at the new anchor. If the measured median is 0 again, the limit is `max(allowed, noise, floor_for)`, which is the pre-existing rule for 0-anchored counters (page_faults has a floor ≥ 1). sched_syscalls is ungated (gate.py:87), so its T23 case never affected a verdict.
- **The README tells a reader** (README.md:329-332) that such a counter "cannot be carried: its measured median is anchored and recorded under `uncarried`". It does not say what follows: the counter's drift restarts from that median, and its improvement before the anchor is not banked (N4).

## Q2: Fix 2 path trace (cli.py:301-363)

| reset | anchor | ledger gate of tree | fingerprint | Before (0212e7785) | After (74ead82ae) |
|---|---|---|---|---|---|
| off | none | n/a | n/a | fresh anchor | same |
| **on** | none | n/a | n/a | fresh anchor (flag ignored) | **refused before measuring**, exit 2 (cli.py:312) |
| off | yes | n/a | comparable | plain re-anchor, forgiven logged | same |
| **on** | yes | n/a | comparable | plain re-anchor (flag ignored) | **refused after measuring, before any record**, exit 2 (cli.py:332) |
| off | yes | yes | changed | carried | same (ZeroDivisionError at d = −1 is gone) |
| off | yes | no | changed | refused | same |
| **on** | yes | **yes** | changed | **carried (flag ignored: the bug)** | **reset**: carried = forgiven = None, logged (cli.py:337-338) |
| on | yes | no | changed | reset | same |
| on/off | partial (`--only`) | n/a | changed or no anchor | refused (cli.py:329) | same; the partial check runs first |

- **Comparable fingerprint**: `--reset-drift` can no longer discard drift there. It is refused, so a plain re-anchor (which logs `forgiven`) remains the only route. DEC-301 and DEC-305's carry-by-default is untouched: without the flag the code path is identical except that the d = −1 crash is gone.
- **Nothing is written before the refusals.** Neither refusal path touches anchor.json, anchors.jsonl or ledger.jsonl: `anchor.record` (cli.py:350) is the sole writer of the first two, and `cmd_anchor` never writes the ledger. Mutant M3 confirms this.
- **Exit codes**: the new refusals are `SystemExit(str)`. `main` maps those to `VERDICT_EXIT["ERROR"]` = 2, as for every other anchor refusal. The gate's 0/1/2/3 and its verdict logic are untouched (gate.py and ledger code are not in the diff).
- **Note**: when a gate exists, `--reset-drift` now knowingly discards drift that perfgate could compute, and records nothing of it (N1).

## Q3: Tests and mutants

Script: `/work/scratch/bl313-review/scripts/mutants.py`. Each mutant runs on a copy of the fix's perfgate against test_bl313.

| Mutant | Result | Killed by |
|---|---|---|
| M1 `1 + d <= 0` → `< 0` (≤ −100% as < −100%) | KILLED | test_a_counter_at_minus_100…, test_the_gate_carries… (ZeroDivisionError) |
| M2 reset honoured only when no ledger gate exists (old bug) | KILLED | test_reset_drift_applies_even_when_a_gate… |
| M3 comparable-fingerprint refusal moved after `anchor.record` | KILLED | test_reset_drift_is_refused_without_a_fingerprint_change (anchor rev changed) |
| M4 no-anchor refusal moved after `measure_rev` | **SURVIVED** | (N2) |
| M5 uncarry any improvement (`d < 0`) | **SURVIVED** | (N5) |
| M6 uncarry at d ≤ −50% | **SURVIVED** | (N5) |
| M7 cli does not log uncarried counters | **SURVIVED** | (N6) |
| M8 anchors.jsonl `carried` = raw carried_drift | KILLED | test_a_counter_at_minus_100… |
| M9 anchors.jsonl without `uncarried` | KILLED | test_a_counter_at_minus_100… |
| M10 comparable refusal removed | KILLED | test_reset_drift_is_refused_without… |
| M11 reset path still computes `carried` | KILLED | test_reset_drift_applies_even_when… |

## Q4: Nothing else changed

- The gate verdict logic, `drift_rows` and the exit codes are unchanged; gate.py and ledger code are not in the diff.
- **ledger.jsonl**: unchanged.
- **anchors.jsonl** (copied into the committed ledger, DEC-52 2) changes additively and narrows in one place:
  - Every line now has an `uncarried` key, `{}` on plain anchors.
  - `carried` now lists only the counters actually applied to the workloads measured in this anchor. Before, it was the raw `carried_drift` result, which included the −1 entries and gate workloads that were not anchored.
  - The only in-tree reader, test_review_r1.py:327, counts lines.

  This is fine, but the acceptance should call it a format change to anchors.jsonl, not "no format change".

## Findings

None blocking.

- **N1 (audit, recommended)** cli.py:337-338: with a ledger gate of the tree present, `--reset-drift` throws away drift that perfgate can compute. It records nothing of it: anchors.jsonl gets `carried: {}` and `forgiven: {}`. DEC-305 had to copy the 340-entry drift budget by hand (D-rows.md, drift-carried-dec305.md), and PF0-N-final-opus.md:33 already flagged that "`--reset-drift` silently forgives all bench drift". Suggestion: when `anchor.carried_drift(old, tree)` is not None on the reset path, print it as "discarded drift" and record it in anchors.jsonl, for example under a `reset` key next to `forgiven`. That makes the reset auditable, as the README promises for plain re-anchors ("the drift it forgives is printed and logged", README.md:333).
- **N2 (test gap)** test_bl313.py, `test_reset_drift_without_an_anchor_is_refused_before_measuring` (~line 107): the name promises "before measuring", but nothing asserts that `measure_rev` was not called, so M4 survives. Suggestion: have `anchor_cmd` expose the mock (for example, patch outside the helper or return it via `self`) and add `measure_rev.assert_not_called()`.
- **N3 (pre-existing, symmetric gap)** anchor.py:59 and :121: a counter whose old anchor median is 0 and which rose under the old fingerprint is silently anchored at its new measured median. It shows up in neither `carried` nor `uncarried`, and nothing is logged. In practice such a rise beyond the floor would already have been a DRIFT row at the carried gate. Still, the audit trail BL-313 adds for "fell to 0" has no counterpart for "rose from 0". Optional: record it as `uncarried` too (for example, d = `"inf"` or `null`), or state in the README that 0-anchored counters are never carried.
- **N4 (docs)** README.md:329-332 and the `record` docstring (anchor.py:106): say what "uncarried" means for later gates. The counter's drift restarts from the measured median (the strictest choice), its earlier improvement is not banked, and gates don't read `uncarried`; it is informational only. The docstring still says only "`carried` … is folded into the stored medians".
- **N5 (test gap)** test_bl313.py:55-68: the boundary is only exercised at d = −1 and d = +1%, so mutants that uncarry any improvement (M5 `d < 0`) or anything at ≤ −50% (M6) survive. Both would silently drop DEC-301's carry for genuinely improved counters. Suggestion: add a second counter with d in (−1, 0), for example `allocs` 1000 → 500, and assert it is carried (`medians` = measured / 0.5, present in `carried`, absent from `uncarried`).
- **N6 (test gap, minor)** cli.py:358-361: the "not carried" log line is part of the claim, but untested (M7 survives). Optional: capture `cli.log` in `test_the_gate_carries_drift_without_reset_drift` and assert the line.

## Observations (no change requested)

- **O1**: carrying a large finite negative drift on tiny integer counters multiplies noise (×1/(1+d)). That is inherent to DEC-301's carry; floors bound it.
- **O2**: for comparable fingerprints, `--reset-drift` used to be a silent no-op that degraded to a plain re-anchor; it is now refused. Nothing in-tree passes it (grep: only README, cli, tests and docs), so this breaks no scripts.
- **O3**: the comparable-fingerprint refusal can only fire after a full measurement, which is about 1 h for a full anchor (DEC-301), because the fingerprint needs a prepared worktree (`measure.fingerprint(wt)`, measure.py:258). Checking earlier would mean restructuring `measure_rev`. The current behaviour is acceptable: the refusal is loud and writes no anchor state.

## Scripts

- `/work/scratch/bl313-review/scripts/mutants.py`: the 11 mutants above against test_bl313.
- `/work/scratch/bl313-review/scripts/edges.py`: probes `anchor.record` and `carried_drift` for 0 → n, d = −0.999999 and d < −1.
- The worktree and the base and mutant copies were removed after the review.

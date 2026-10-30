# O2b BL-222 r1 review: certify's replay comparison rule (node/o2b-bl222-order @ 42531f9e6)

Reviewer: Claude Opus 5.5, standing in for the Sol r1 run that died. Range: `536240d34..42531f9e6` (fuzzctl.py, fuzzctl_test.py, README.md, targets.toml). Context: DEC-260, DEC-271, DEC-225.

## Verdict: ACCEPT WITH NITS

No must-fix findings. Every comparison goes to the right rule, and "stricter of" fails closed for every key in both directions. Malformed rules are rejected in targets.toml and in certificate.json, and the selftests pin each of these. The items below are documentation fixes plus two questions for the manager (N1, N2). None of them can turn an uncertifiable run into a certified one.

## Answers to the brief's questions

### Q1. Is each comparison routed to the right rule? Yes.

| Comparison | Processes | Rule now | Where |
|---|---|---|---|
| Replay after the window fell in cov | across | `replay.cov_slack` | fuzzctl.py:1967 |
| Replay after the window fell in ft | across | `replay.ft_allowance(base ft)` | fuzzctl.py:1968 |
| Replayed cov gain (growing) | across | `replay.cov_slack` | fuzzctl.py:1975 |
| Replayed ft gain (growing) | across | `replay.ft_allowance`, only when `rule.ft` holds | fuzzctl.py:1976 |
| Window DONE vs INITED, cov | within | `rule.cov_slack` (abs) | fuzzctl.py:1975 |
| Window DONE vs INITED, ft; discovery lines | within | `PLATEAU_FT`, only when `rule.ft` holds | fuzzctl.py:1976-1977 |
| Farm suspicion (`suspected`) | within | `plateau_rule` | fuzzctl.py:1552, 2376, 2838 |

The farm's history (`LogScan`, fuzzctl.py:1494-1540) measures each process from its own `INITED` and sums the gains, so it never compares two processes. Nothing compares the window's `INITED` against the replays. I found no other use of `cov_slack` or `PLATEAU_FT` (grep of fuzzctl.py), and nothing outside fuzzctl reads these keys (git grep). The one cross-process comparison still tied to a plateau field is the replayed ft gain, which is gated on `plateau_ft`. That is deliberate (Q3a, N1).

The selftests catch a mis-route in either direction:
- `{"final": (301, 1000)}` stays `growing` and `(299, 1000)` stays `failed` under `Plateau(6, False)`, so the plateau slack no longer reaches the replays (fuzzctl_test.py ~2300).
- `{"window_done": (301, 100000)}` stays `growing` under `Replay(30, 120)`, so the replay slack does not reach the window (~2346).

### Q2. Is "stricter of recorded and registry" right? Yes, for every key and direction.

- **Cov:** `Replay.strictest` takes `min` per key (fuzzctl.py:167-169). A smaller `cov_slack` fails a fall sooner and calls a gain "growing" sooner. Both outcomes move away from `certified`.
- **Ft:** `ft_allowance = max(PLATEAU_FT·ft, ft_slack)` rises monotonically with `ft_slack`, so `min` is the stricter choice for both the fall and the gain.
- **A registry change cannot loosen an old certificate.** A certificate from before DEC-271 gets `Replay(0, 0)` (`recorded_rules`, fuzzctl.py:2016-2020). For example, `{"plateau": {"cov_slack": 6, "ft": false}}` with `replay_cov_slack = 14` in the registry is judged at cov slack 0. I checked this directly: `recorded_rules({"plateau": {...}})` returns `Replay(0, 0)`. The test case `fffffffffffffff8` covers it.
- **A stale certificate cannot block a new run.** `certificates()` judges only the latest certify run of each leg (fuzzctl.py:2089-2103). `certify` judges a new run by the registry's rule and records that rule (fuzzctl.py:2988-3000).
- **Side effect:** a diff_chain certificate written between DEC-260 and DEC-271 was judged with cov slack 6 on its replay comparisons. It is now judged at 0, which is stricter than when it was written. That fails closed. I believe diff_chain is not in the farm yet, so no such certificate should exist. See N4 for the docstring that describes this wrongly.

### Q3a. `plateau_ft = false` keeps the replayed ft gain out of the growth test. Consistent, but the reason doesn't transfer (N1, not a must-fix).

It matches DEC-260 (2) as written ("ft is excluded from diff_chain's plateau test"), and the README states it (README.md:357-359). It is also a hole: a diff_chain window that finds only ft certifies. For example, `final = (300, 1100)` against `base = (300, 1000)` (+10 % ft) under `Plateau(6, False)` gives `certified`, and the selftest pins that.

DEC-260 (2) justified the exclusion by in-process drift: 0.22-1.0 % ft per hour in a looping replay. Certify's two replays are single passes in fresh processes. By the owner's measurement they differ by only 0.073 % in ft, which is under the 0.1 % default. So the principle behind DEC-271 ("each tolerance comes from its own measurement") would argue for testing the replayed ft gain at `ft_allowance` even when the in-window ft test is off.

I would not change this in this round. Units the window added because of drift could raise the ft of the replay after the window, and nobody has measured that. If it does, enabling the test could stop diff_chain from ever certifying. Every certify run already records `baseline` and `final` ft, so a few diff_chain certify runs will supply the measurement at no cost. **Manager decision:** after those runs, either keep DEC-260 (2) for replays or add a separate `replay_ft` switch.

### Q3b. Is `MAX_FT_SLACK = 1000` sound? Yes as a typo guard. It doesn't scale with the target (N3).

The cap is absolute, but the ft allowance it bounds is relative in spirit:
- For diff_chain (ft ≈ 89,005), 1000 is 1.1 % of ft, about 11× the ft key the measurement would justify (98). A typo of 980 for 98 would get through.
- For a small target, say ft ≈ 3,927 (the README's smoke replays), 1000 is 25 % of ft (`Replay(0, 1000).ft_allowance(3927)/3927 = 0.255`).

The key only matters once ceil(1.5 × spread) exceeds 0.1 % of ft. So a large target with a real ft spread above about 667 would hit `die()` and have to raise the cap. That fails loudly, which is acceptable. Stricter-of means a loose registry value can only affect new runs. Sound enough for its job. The constant needs a one-line rationale (N3).

### Q4. Is `ceil(1.5 × spread)` from 8 processes defensible? Yes, and its errors fail closed.

I simulated normal noise (200k trials), measuring the range of 8 draws and then comparing two fresh draws:
- P(the fall exceeds the slack) ≈ 0.9 % per certify run.
- P(|difference| > slack), meaning either a false `failed` or a false `growing`, ≈ 1.8 %.
- With only 6 processes (DEC-271's minimum), the fall rate is ≈ 1.9 %.

On diff_chain's numbers (spread 9, so σ ≈ 9/2.85 ≈ 3.2 and the difference has σ ≈ 4.5), a slack of 14 is about 3.1 σ.

Underestimating the slack costs a false `failed` or `growing` (fail closed). Overestimating it is the unsafe direction, and that is capped at 64. So the derivation errs the safe way.

Two caveats, both nits:
- **N2.** DEC-271 cites a cov spread of 18-20 between processes on one corpus (DEC-260 (1)'s rerun). That is above the slack of 14 set here. The difference is probably replay order (AABBCC/ABCABC vs libFuzzer's default order) or corpus. The targets.toml comment should say which, or the next owner will read 14 as contradicting the DEC.
- **Re-measurement trigger.** The spread likely grows with corpus size, and the certify corpus grows over farm time. Currently a re-measurement is triggered only when "the target or its fold changes" (DEC-260 (4)). Add "or the corpus has grown materially (say 2×) since the measurement, or certify fails a reproduction within 2× the slack".

On whether the next owner can repeat it: the README (README.md:349-356) gives the rule (≥ 6 processes, pinned archive, ceil(1.5 × worst pairwise spread), cap 64, ft key only above 0.1 %). It does not give the replay command: the leg's binary copy, `-runs=0`, one process, default order. It also doesn't say where the measurement's logs should be kept. That is a nice-to-have.

### Q5. Are malformed or partial rules rejected cleanly? Yes.

Registry: `replay_cov_slack = -1 | 65 | true`, `replay_ft_slack = 1001 | 0.5 | "auto"` all `die()` (tests). `is_count` rejects bool and float.

Certificate: each of these makes the certificate `invalid` (tests):
- `"replay": null`
- `{"cov_slack": 30}` (partial)
- `{"cov_slack": 65, …}`
- `ft_slack` of 1001 or 0.5
- an extra key, `"ft": false`

I also probed `from_record` directly. It returns None for `14.0`, `True`, `-1`, `10**30`, `None`, `[]` and a partial dict, and accepts the boundary `{"cov_slack": 14, "ft_slack": 1000}`.

A certificate missing the `"replay"` key gets the default, `Replay(0, 0)`. That is the strictest rule, so deleting the key from a certificate can't help a run. Fine.

### Q6. Is there anything simpler? Only small things.

`Replay` alongside `Plateau`, `listed`, and `recorded_rules` are all minimal. One simplification belongs to the decision text rather than the code (N5): always set `replay_ft_slack = ceil(1.5 × ft spread)`. `ft_allowance` already takes the max with 0.1 %, so the condition "only when the spread is over 0.1 %" buys nothing.

## Findings

No must-fix findings.

Nice-to-have, or for the manager:

- **N1 — the replayed ft gain is gated by the plateau flag.** fuzzctl.py:1976, README.md:357-359.
  - Input: diff_chain (`plateau_ft = false`), `baseline = (17560, 89005)`, `final = (17560, 89900)`. That is +1.0 % ft and +0 cov, against a measured fresh-process ft spread of 0.073 %.
  - Result: `certified`.
  - Consistent with DEC-260 (2); its drift rationale doesn't apply to single-pass replays. **Manager:** after a few diff_chain certify runs, decide whether to keep this or add a `replay_ft` switch (see Q3a). No code change this round.
- **N2 — the cov slack of 14 looks low next to DEC-271's spread of 18-20.** targets.toml:619-621.
  - Input: per DEC-271, a corpus whose replays differ by 18 cov would give `final.cov = base.cov − 15`. That reports `failed` ("not reproducible") on noise.
  - This fails closed. The fix is to say in the comment why that measurement doesn't apply (order or corpus), and to add the corpus-growth re-measurement trigger.
- **N3 — `MAX_FT_SLACK = 1000` needs a rationale.** fuzzctl.py:183.
  - Input: `replay_ft_slack = 980` on diff_chain, a typo for 98. It loads.
  - Add a comment: "absolute; ~1 % of diff_chain's ft; a target needing more raises it". Optionally make the key relative (per-mille) later.
- **N4 — the docstrings misstate history.**
  - fuzzctl.py:2001-2002 says a certificate from before DEC-271 "was judged by the defaults". In fact a DEC-260-era certificate's replays were judged with the plateau slack, and they are now judged at 0, which is stricter. Reword to "is judged by the defaults (no looser than what it was judged by)". README.md:362-364 has the same wording.
  - fuzzctl.py:1922-1923 opens with "must not be lower … (its cov, or its ft by more than 0.1 %)", and the next sentence then relaxes that. Fold the two together.
- **N5 — the ft key condition.** README.md:353-354, DEC-271 (3).
  - Diff_chain's ft spread is 65, and ceil(1.5 × 65) = 98, which is above 0.1 % of 89,005 (89). Under the rule as written the key is omitted, so the ft fall margin is 89/65 = 1.37×, against 1.5× for cov.
  - Setting the key whenever it's measured (98) is harmless because of the max, and keeps both margins at 1.5×. This needs a one-word change to the DEC wording; it is the manager's call.
- **N6 — two new README lines are over the wrapping width.** README.md:331 (121 characters) and :359 (113). Cosmetic.

## What I ran

- Read DEC-260 and DEC-271, the full diff, the `judge`, `suspected`, `LogScan`, `certificates`, `read_certificate` and `certify` paths, and git-grepped for the keys. I opened nothing else under /work/scratch.
- Ran the full suite twice: `python3 fuzzctl_test.py` from node-fuzz/, with TMPDIR set to a scratch dir.
  - **Run 1:** 181 tests, `FAILED (failures=1, skipped=5)`, 920 s at load average 234-387. My `| tail` dropped the traceback, so I can't say which test failed.
  - **Run 2:** `python3 -m unittest -v fuzzctl_test`, 181 tests, `OK (skipped=5)`, 389 s. Log: /work/scratch/o2b-bl222-r1-sol/test-run2.log, with no FAIL or ERROR lines.
  - I can't attribute run 1's failure. The suite's wall-clock tests are known to flake at this load, and every case this change touches passed in both the class run and run 2.
- Ran `python3 -m unittest fuzzctl_test.Registry fuzzctl_test.Certifying` separately: 34 tests OK.
- Probed `Replay.from_record`, `ft_allowance` and `recorded_rules` with the inputs listed above.
- Ran a Monte Carlo for Q4: 200k trials, normal noise, range of 8 (and of 6) vs a two-process difference.
- No edits, commits or pushes in the worktree, and no fuzzing or Rust builds.

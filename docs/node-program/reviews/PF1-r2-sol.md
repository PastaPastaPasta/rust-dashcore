# PF1 independent review, round 2 — Sol

Reviewed `node/pf1-profile` at `7a35cf6e`, exclusively the follow-up diff `cdba1e78..7a35cf6e`, against the six major findings in `PF1-r1-sol.md`. Followed `briefs/REVIEW-COMMON.md`. This is a narrow research-report confirmation, not a fresh baseline measurement or consensus audit. Review performed in a detached worktree at `/work/scratch/pf1-review-r2/wt`; output is uncommitted in the integration worktree. The branch under review was not modified; no commits, pushes, GitHub comments, or node launches were made. Neither `/work/chains` nor `/work/bench` was accessed. Scratch worktree and mode-700 `/dev/shm/pf1-review-r2-tmp` are removed after delivery.

## Verification

- Read the complete revised report, its generated summary tables, the r1 review and the three-file follow-up diff. Checked all projection language, including §0, §5, §11's conventions, rows, basis-strength paragraph and re-ranking summary.
- Recomputed the BLS, script Amdahl, RPC, mempool, startup and header-hashing formulas from the published shares; checked the short-window BLS absolute baseline against the unchanged committed `summary.json`.
- Compiled the changed Python module in memory and exercised `report_tables.summarize` with a temporary fixture under the required `TMPDIR`. Confirmed that inclusive CPU values come from `cpu-clock.inclusive.tsv`, legitimate zeros remain zeros, and missing CPU rows/files produce `n/a`.
- Compared every line of the committed `tables.md` with r1: the only changes are the explanatory note and removal of the erroneous inclusive CPU columns. The instruction figures remain unchanged. `git diff --check cdba1e78..7a35cf6e` passes.
- Spot-checked the revised cache-lifetime, signature-cache and compressed-header claims against pinned Core `v24.0.0-rc.3` sources (`primitives/transaction.h`, `script/sigcache.cpp`, `primitives/block.cpp`, `rpc/blockchain.cpp`). No long profiles or already-accepted metric #5 recomputation were repeated.

## Closure of the six r1 majors

| r1 finding | Status | Confirmation |
|---|---|---|
| M1 — Faster verify extrapolated to all BLS | **Not fully closed** | External A1 has no PF1 evidence and the all-BLS scenario correctly requires all BLS work to improve. However, the new verify-only absolute estimate transfers DWARF shares to a different window/denominator; see R2-M1. |
| M2 — Compaction assumed fixed/startup-only | **Closed** | Lines 50, 75 and 92 explicitly leave reopen versus replay-induced compaction unresolved, distinguish thread attribution from causal isolation, and preserve all-thread costs. Row 10 retracts the write-path estimate and requires a write-only breakdown/control. |
| M3 — Ceiling/max ratios promised as wall/tail gains | **Closed** | Lines 154–156 and row 9 distinguish a bound on an idealised ceiling from an achievable minimum, label run-pair ratios as observations, disclaim p99 inference, and include the worsening 421k maximum. |
| M4 — Validation bucket treated as removable chain work | **Closed** | Line 219 names validation/index work and its components. Row 6 separates proof-only (1.456×) from removal of the proof/comparator union (2.137×), makes the latter conditional, and leaves wall/RSS improvements unquantified. |
| M5 — Serialization entirely removable | **Closed** | Rows 3–4 explicitly identify zero-residual bounds, required output work and illustrative residuals. The v2 tree estimate is withdrawn. Line 231 and row 3 specify hashes that survive block reads rather than another per-object cache. |
| M6 — Synthetic script parallelism contradicts evidence | **Closed** | Row 2 reproduces 4.534× from instructions and 3.486× from CPU at 12 cores, disclaims a measured wall critical path, and separates unquantified pipeline/cache levers. Signature-cache instruction savings are conditional on hit rate. |

## Findings

### R2-M1 — major: verify-only absolute budget still mixes incompatible windows

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:316` (contradicts `:121`).

Row 1(a) takes the 22.7–25.3% inclusive verify shares from short DWARF windows and writes **173 M → 140–147 M/block**. The 173 M number is the long-window, ex-background baseline. The verify percentages are all-thread percentages from different block sets. §2.2 explicitly says not to apply those shares to the 2,000-block absolute costs. Marking the result as projected and conditioning the library factor does not establish that cross-window share transfer.

For example, committed `summary.json` gives `dwarf-w1520000-serial-av.instr_per_block = 230.576 M`. Applying that window's own approximately 25% verify share and the external 3–4× factor yields approximately **192.1–187.3 M/block for that short all-thread window**, not a justified budget for the 173 M long ex-background window. The approximately 1.2× conditional ratio is useful when attached to the measured DWARF workloads.

**Failure scenario:** PF2 uses 140–147 M as the verify-only target for the 173 M replay, although PF1 never measured its verify-only share. The original unsupported operation-to-whole-window budgeting survives in a new form.

**Suggested fix:** remove the 173 M absolute translation from (a) and attach the percentage/ratio to the short DWARF windows, or calculate each absolute scenario with its own window's total and share. If retaining a long-window translation, add share stability/transfer as an explicit unmeasured assumption and do not describe it as derived solely from that workload's PF1 measurements. Row 1(b)'s all-BLS conditional calculation can stay.

### R2-m1 — minor: publish the actual ceilings used to rank rows 1 and 6

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:312`, `:316`, `:329`.

The order is numerically reproducible under a consistent maximum-ceiling interpretation, but its two decisive operands are not stated clearly as ranking operands. Row 1 only prints scenarios requiring external A1; its PF1-only zero-cost BLS bound is implicit. Startup ranks above header hashing only when its **2.137× union-removal** scenario is used, rather than its **1.456× proof-only** scenario. The re-ranking summary compresses those different cases into “1.5–2.2× bounds.”

The reproducible descending operands are: all-BLS zero-cost at 96.7% **30.30×**, script **4.53×**, getblock v0 **3.04×**, GBT **2.86×**, mempool **2.24×**, startup union **2.14×**, header reuse **1.75×**, decode-union zero-cost **1.25×**, then off-thread BLS and storage with no quantified ceiling. Thus there is no necessary numerical inversion, and external A1/A8 need not participate. These are conditional mathematical bounds, not achievable gains. Decode's repeat count remains unmeasured, but its measured share supplies an outer bound.

**Failure scenario:** a reader sorts the printed A1 BLS range and proof-only startup bound, obtains a different order, and cannot reproduce the declared rule from the ranking's explicit operands.

**Suggested fix:** state that each row ranks by its largest PF1-only conditional ceiling, publish row 1's zero-cost BLS bound as the ranking basis, and name startup's union-removal scenario as its ranking basis. Keep the more realistic/external scenarios separate from that sort key.

### R2-n1 — nit: mempool cross-reference follows the old ranking

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:193`.

“See §11 row 8” now points to BLS decoding; mempool is row 5. Update the reference to row 5.

## Report-wide projection check

The revised title/status and §11 conventions consistently identify rewrite numbers as conditional ceilings or projections. §0 and the measured summary tables contain no promised rewrite gains. The re-ranking summary also calls its numbers bounds/ceilings. The old startup wall/RSS targets, RPC estimates and 6–8× script claim are explicitly retracted, and observed wall/max ratios remain observations. I found no remaining unqualified assertion that a rewrite has achieved the projected gain. R2-M1 is instead an unsupported absolute projection inside a correctly labelled projection section.

The ancillary corrections are adequate for this scope: deleted-log counts are labelled unarchived; metric #5 does not establish populated-cache flush behaviour; its attribution is qualified and spread/envelope conventions are defined; both salted-view constructors are named; worker counts and small numeric labels are corrected; false-zero inclusive CPU columns are fixed and the published false values removed.

**mergeable: no**

**Verdict: CHANGES-REQUIRED.** Five r1 majors are closed. Finish M1 by keeping the verify-only budget on its own measured workload; make the ranking operands explicit. No long measurement rerun is required.

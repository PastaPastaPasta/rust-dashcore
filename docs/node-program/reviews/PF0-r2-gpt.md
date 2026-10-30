CHANGES-REQUIRED

Reviewed `node/pf0-counter-gate` at `aeb352b2f43bcc9f2a653b66bef4e3121740309e`, against Opus round 1 and DEC-52. References below are relative to rust-dashcore at that commit; `perfgate/` means `contrib/node/perfgate/perfgate/`. This completes the interrupted review using its saved evidence, independently checked against the code. No reviewed branch edits, commits, pushes or GitHub posts.

1. **Major — build scripts can change bench inputs after the bench-change check (M2 remains partial).** `perfgate/build.py:173`, `perfgate/cli.py:136`, `perfgate/cli.py:149`.

   The check compares Git bench sources/manifests before Cargo runs. It excludes `build.rs` and never checks the effective bench bytes after the build. The saved `runtime_probe.py` injects a scratch head containing a SipHash instruction plant and `hashes/build.rs`; the latter replaces `const LEN: usize = 25;` with `24` in `benches/siphash.rs`. Real Cargo builds and measurements follow. `bench_changes` returns no changes, the existing anchor remains accepted, and `run_gate` returns PASS with no BENCH-CHANGED or blocking row. The instruction median is **197,427,778 → 194,970,176 (−1.245%)**. The same plant without the build script is **197,427,778 → 199,885,374 (+1.245%)**, REGRESSION/FAIL. This bypasses the new bench-source overlay without editing a tracked bench file in the proposed tree.

   **Fix:** build against immutable base bench sources and immutable shim sources; allow build scripts to write only their build outputs, not bench inputs. Record/validate effective generated inputs and require recorded approval for changes. Include build-script/configuration changes affecting benchmark generation in the review boundary. A post-build source hash is useful detection, but alone does not prevent a script from restoring sources after compiling altered inputs.

   Evidence: `runtime-buildscript.log`, `runtime-plant.log`, `buildscript-{base,head}.json`, `plant-{base,head}.json`. These are real targeted measurements with a synthetic head injected by the probe, not a full authoritative CLI run; CLI subset selection still correctly prevents an authoritative PASS.

2. **Major — measured builds can write trusted gate state.** `perfgate/build.py:313`, `perfgate/build.py:326`, `perfgate/paths.py:18`, `perfgate/cli.py:190`.

   Cargo/build scripts run as the gate owner with its filesystem permissions. `cargo_env()` inherits everything except the two target-directory overrides, including PERFGATE_ROOT. The saved Rust `state_writer` process, launched with that Cargo environment, appended an `authoritative:true, verdict:PASS` probe row to `state/ledger.jsonl`. The row is deliberately incomplete: this proves write access, not acceptance of a complete forged merge authorization. The same permission boundary permits alteration of `anchor.json`, reports, cached measurements, the shim snapshot and base artifacts. The gate's flock does not prevent its own child from writing them; owner-writable state cannot authenticate the measured build's result.

   **Fix:** execute build scripts and benchmarks under a separate UID or filesystem sandbox with narrowly writable build outputs/scratch. Keep the ledger, anchors, harness/shim, reports and opposite-side artifacts inaccessible for writes. The trusted parent alone should validate and persist results. Removing PERFGATE_ROOT from the environment alone is insufficient because the paths are discoverable.

   Evidence: `state_writer.rs`, `state_writer`, the appended probe row in `state/ledger.jsonl`, and the unsandboxed subprocess calls above.

3. **Major — escalation discards later evidence for initially OK counters (M1 remains partial).** `perfgate/gate.py:291`, `perfgate/gate.py:301`, `perfgate/cli.py:174`.

   Escalation reruns an entire workload, but `compare` judges every counter on its first five samples and uses the pooled samples only if that counter was initially NOISY. In the saved probe, instructions are 1,000,000 on both sides for five runs, then the head's ten additional runs are 1,015,000. Allocation spread triggers escalation. With a complete comparable anchor, both final rows are OK and the verdict is PASS, even though the pooled instruction test gives **REGRESSION, p = 0.00007763**, with a +1.5% shift. That is below the separate 2% drift threshold, so the anchor does not rescue this gate.

   **Fix:** after a workload escalates, reconsider every gated counter using all its runs; do not retain an OK or IMPROVED decision contradicted by later evidence. Preserve first-round statistics separately for explanation. Add the allocation-triggered/instruction-regression case as a policy regression test.

   Evidence: `probes.log`, `probes.py`, `additional.log`; deterministic policy probes, not a hardware benchmark claim.

4. **Major — the tied Mann–Whitney approximation overstates significance.** `perfgate/gate.py:132`, `perfgate/gate.py:150`, `perfgate/gate.py:216`.

   For base `[0] × 15` and head `[0] × 10 + [1] × 5`, the implementation returns **p = 0.00874580**; the exact one-sided permutation probability is **C(15,5)/C(30,5) = 0.02107280**. It crosses the gate's α = 0.01 incorrectly. Translating/scaling the samples preserves that p-value; a large enough counter increment also exceeds the effect threshold through the trimmed mean, despite the Hodges–Lehmann shift being zero. Deterministic integer counters commonly produce ties, so 15-versus-15 alone does not justify the approximation. An exact p-value would correct the REGRESSION classification; under the current policy an unconfirmed positive shift can still block as NOISY-UNRESOLVED.

   **Fix:** use an exact permutation distribution with average ranks for tied small samples (dynamic programming avoids enumerating all assignments), or a sufficiently resolved permutation test. Test against the exact tied oracle and remove the blanket claim that the approximation is adequate.

   Evidence: `probes.log`; independently rechecked with the combinatorial oracle above, without rerunning benchmarks.

5. **Major — identical noisy populations produce substantial false FAIL rates.** `perfgate/gate.py:165`, `perfgate/gate.py:185`, `perfgate/gate.py:217`, `perfgate/gate.py:301`.

   The prior reviewer pooled the implementer's warm no-op base/head samples for each workload, then drew both synthetic sides from that **same empirical population**, preserving within-process counter correlations by using a shared sample index. Each trial applies the five-run decision and, for NOISY counters, the 15-run escalation. With seed 714 and 5,000 trials per workload:

   | Workload | Trials with a blocking counter | Estimated false-FAIL probability | 95% Wilson interval |
   |---|---:|---:|---:|
   | `kwm-filter-scan-2000` | 252 / 5,000 | 5.04% | 4.47–5.68% |
   | `spv-storage-store` | 481 / 5,000 | 9.62% | 8.83–10.47% |

   Filter-scan failures were `instructions_total`; storage failures included syscalls, allocation bytes, write bytes and faults. `noop-example.log` supplies concrete identical-population draws incorrectly classified REGRESSION: filter-scan instructions_total +1.205%, and storage alloc_bytes +1.782%. A five-run subset can appear deterministic merely by missing the population's bursts; its terminal first-round FAIL is never reconsidered. Unresolved positive shifts also block independently of the p-value.

   These intervals quantify **Monte Carlo uncertainty conditional on the saved empirical populations**. They do not establish the live host's long-run rate: the input sample pools are small, resampling assumes exchangeable independent runs, and timer/load correlations may differ. Nor are these whole-gate rates or rates to add together. Nevertheless, they disprove reliable no-op acceptance under the supplied null populations; one observed no-op PASS is insufficient validation.

   **Fix:** seed filter-scan inputs, exclude/control timer-driven storage work, and confirm apparent first-round regressions on noisy workloads with additional independent interleaved samples. Calibrate the complete multi-counter decision procedure against repeated no-ops, including sequential selection and NOISY-UNRESOLVED, rather than treating α as its false-FAIL bound. Keep genuinely undecidable runs explicit; do not silently pass significant regressions.

   Evidence: `probes.py`, `probes.log`, `noop_example.py`, `noop-example.log`, and `/work/scratch/pf0/acceptance-warm/noop/{base,head}.json`. The recorded 5,000-trial experiment was inspected, not repeated; intervals were calculated from its recorded counts.

6. **Major — missing anchor coverage leaves cumulative drift unchecked (M4 remains partial).** `perfgate/gate.py:331`, `perfgate/gate.py:345`, `perfgate/gate.py:356`.

   A comparable anchor missing a measured workload produces only UNANCHORED, which is non-blocking. A missing counter median is silently skipped. A workload added after the milestone can therefore remain unanchored across later merges, allowing the original sequence of sub-threshold regressions to compound. The saved missing-anchor probe returns PASS. DEC-52 requires every gated workload to receive the fixed-anchor comparison; the new fixed-anchor implementation satisfies this only where coverage exists.

   **Fix:** before an authoritative PASS, require an anchor entry and every applicable gated counter for workloads already present on the base. A genuinely new workload may remain NEW for its introducing PR, but initialize its approved anchor before the next authoritative gate. Missing coverage should be ERROR or INCOMPLETE, with an explicit manager action.

7. **Minor — comparison errors use the documented FAIL exit instead of ERROR.** `perfgate/gate.py:356`, `perfgate/cli.py:239`, `perfgate/cli.py:251`.

   An ERROR row (for example an incomparable fingerprint or failed escalation) becomes verdict FAIL and exit 1. Only exceptions handled earlier produce ERROR/exit 2. `additional.log` confirms `ERROR row verdict/exit FAIL 1`, contrary to PROCESS's “ERROR (exit 2)” rule. Both block, so this is an interface/reporting defect, not another fail-open path.

   **Fix:** give ERROR rows verdict precedence and exit 2, consistently in gate, compare and JSON output.

Round-1 closeout:

| ID | Status | Code/evidence and remaining work |
|---|---|---|
| B1 | **Closed** | `build.py:71` synthesizes the merge and `cli.py:214` records its tree. Independently made a fresh detached `--no-ff --no-commit` merge: actual and measured trees both `523178a62f6cc865e5d811e7ce7d71b2163431f2`. Saved stale-base acceptance catches +0.820% instructions. |
| M1 | **Partial** | NOISY now escalates and unresolved positive shifts block (`cli.py:174`, `gate.py:219`); findings 3–5 prevent accepting the revised decision procedure. |
| M2 | **Partial** | Static bench edits, bench manifests/profiles and direct dev-dependency changes block; changed sources are overlaid (`build.py:173`, `cli.py:136`). Saved direct LEN-edit acceptance retains the planted regression. Build-time edits still evade this: finding 1. |
| M3 | **Closed for the round-1 provenance issue** | Foreign/dirty harnesses are refused (`build.py:101`, `cli.py:107`); selection, hashes and measured tree are recorded, and partial/foreign-base runs are INCOMPLETE (`cli.py:202`, `cli.py:225`). `planted.log` demonstrates refusal of a foreign harness. Child write access is the separate finding 2. |
| M4 | **Partial** | Fixed anchors and DRIFT exist (`anchor.py:73`, `gate.py:314`); explicit justified re-anchors are recorded, as DEC-52 allows. Missing coverage still passes: finding 6. |
| m1 | **Partial** | Five in-repo SHA-256 workloads were added (`workloads.toml:24`); the bitcoin_hashes rows remain, and X11/header-replay/cachegrind gaps are explicitly acknowledged in README's Limits. Coverage remains incomplete. |
| m2 | **Closed** | Small benches now have a 256 KiB/2% RSS floor, with 4 MiB reserved for stepped base RSS (`gate.py:75`, `gate.py:160`). |
| m3 | **Closed for the round-1 cache/stack issue** | Runtime PATH/HOME are fixed (`measure.py:111`), copied executables have fixed-length content-hash paths (`measure.py:290`), and worktree paths enter cache fingerprints (`measure.py:178`). Thus a `measure` slot cannot seed the `base` slot's cache. Arbitrary inherited build variables remain outside the finite fingerprint allowlist (`build.py:316`, `measure.py:181`; `additional.log`), a separate reproducibility limitation. |
| m4 | **Open** | `key-wallet-manager/benches/filter_scan.rs:58` still creates a random mnemonic. Finding 5 shows why deterministic inputs matter even with escalation. |
| m5 | **Partial** | README documents Rust-only allocations and deferred work; gated `instructions_total` covers setup/drops before reporting. C/C++ allocations still bypass bench GlobalAlloc, and work after shim exit remains invisible (`criterion-shim/src/lib.rs:429`). |
| m6 | **Closed as the requested mitigation** | `warm` uses the base slot (`cli.py:85`); PROCESS requires warming after merges. Saved warm full no-op takes 497 s. This does not promise cold builds finish within 20 minutes. |
| n1 | **Closed** | `gate.py:90` prints the deterministic-base moved-median rule and escalation policy. |
| n2 | **Closed** | PROCESS retains the live base-sdk and Node-check sections and places PF0 in manager merge step 6 (`docs/node-program/PROCESS.md:23`, `:28`, `:30`). |
| n3 | **Closed** | Reverse-index has its own seeded RNG, independent of `get`'s iteration count (`dash-spv/benches/storage.rs:78`). |
| M5 (readiness) | **Closed under DEC-52(4)** | Node CPU counters are INFO; only page faults, allocation bytes and RSS gate node workloads (`gate.py:83`, `gate.py:228`). CPU readiness remains deferred until deterministic rdashd evidence exists. |

The three round-1 manager questions are decided by DEC-52: the seeded revindex input is approved; `/work/perfgate` is approved with committed ledger copies; PROCESS now specifies the manager-run merge gate. Approval of persistent state does not resolve finding 2's write-access boundary.

Saved `unit.log` reports **61/61 unit tests passing**. The targeted SipHash no-op also passes with instruction medians 197,427,776 and 197,427,773; the independent +1.245% plant fails. The misleading `planted.log` is a harness-provenance ERROR, not a measurement of the plant; the real plant evidence is `runtime-plant.log` and `plant-{base,head}.json`. Additional checks in this completion were limited to the exact tied-case oracle, arithmetic/JSON inspection and the fresh merge-tree comparison (`report-merge-check.log`); no benchmark suite was rerun.

The stale-base blocker is resolved and the provenance, overlay and drift work is substantial. The build/state trust boundary and statistical decision procedure still require fixes before this can be an authoritative merge gate. **CHANGES-REQUIRED.**

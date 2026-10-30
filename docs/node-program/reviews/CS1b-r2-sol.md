# CS1 phase B — Sol review, round 2

Reviewed `node/cs1-phase-b` at **`1edd16baf81751c48ff64f9e878f4a678071731f`**, against base `75c1b44e8b5d3b6a7bbf9763c3afa198413c5fb6`, in detached `/work/scratch/cs1b-r2-sol/wt`. Applied `briefs/REVIEW-COMMON.md`, `designs/CS1-script.md`, `reviews/CS1b-r1-sol.md`, PLAN §7, and the current manager-docs **DEC-184**. Used the code-review-validator workflow. This is the requested narrow follow-up, not a repeat of the entire round-1 audit. The older integration base is expected and is not a finding.

**No production consensus mismatch was found. M1 remains open because the measured 60-minute plateau evidence does not support the recorded acceptance. The other five prior findings are closed, with one new minor qualification of the depth counter.**

`git diff b1c792e7..1edd16baf` contains only PLAN, design and campaign-receipt documentation. The reviewed implementation therefore matches the campaign's code revision. Since round 1, the production script changes are the observation hook, the v24-only policy/flags API and their exports; the sighash serializer, sigops algorithms and ECDSA implementation are unchanged. I checked the changed behavior and the relevant pinned Core routines rather than assuming the previous ordinary gate certified the acceptance campaign.

## Closure of prior findings

### M1 — Major: OPEN; smoke floor fixed and campaign established, plateau gate not established

**Locations:** `node-fuzz/targets.toml:85`; `docs/node-program/reviews/CS1b-r2-campaign-receipt.md:10`, `:80`, `:137`; `docs/node-program/designs/CS1-script.md:942`; manager-docs `DECISIONS.md:195` (DEC-184).

All four registry smoke floors are now **300 CPU seconds**. DEC-184 supersedes the old 24/72 CPU-hour acceptance budgets; I do not require another 24 hours per leg. Likewise, the old runner's `complete=False` budget flag is not itself a plateau failure.

I independently read only the authorized campaign ledger and per-worker aggregate log statistics: **30 slices, 796 worker logs**, all on clean `b1c792e7fc9f`, base SDK `4b4e76b27c33`, oracle `a9a483e51efd`, v24.0. They establish **56.7681 CPU-hours**, **2,183,938,042 executions**, **0 recorded divergences, 0 crashes and 0 sink failures**. The execution total comes from worker `stat::number_of_executed_units`, excluding aggregate `fuzz.out`; dividing the ledger uniformly by two would also mishandle the interrupted tails. BL-86 remains the separate owner of that runner defect.

The receipt's window measurements are reproducible. I placed status lines on each job's elapsed-time axis using executions / reported executions-per-second, concatenated the leg's active segments, and took the running maximum of `cov` and `ft` across jobs and segments. Segment duration uses the longest completed job. For the two interrupted tails I checked both the last status timestamp and terminal execution-count / average-rate estimates; that changes the reconstructed endpoint by about 43 seconds for sighash and 6 seconds for sigops, **without changing any reported window growth or pass/fail conclusion**. Pauses, other legs' time and campaign wall time do not count toward a leg's window.

| Leg | Last 60 min: cov | Last 60 min: ft | ft growth | Strict 60-min rule | Last 15 min: cov / ft growth |
|---|---|---|---|---|---|
| `diff_script_verify` | 4,166 → 4,167 | 18,784 → 18,799 | **0.07986%** | **Fails: +1 edge** | +0 / 0.02660% |
| `diff_script_primitives` | 2,462 → 2,462 | 13,027 → 13,043 | **0.12282%** | **Fails: >0.1% ft** | +0 / 0.04602% |
| `diff_sighash` | 1,026 → 1,026 | 2,580 → 2,581 | **0.03876%** | Passes | +0 / 0.03876% |
| `diff_sigops` | 1,425 → 1,426 | 4,515 → 4,529 | **0.31008%** | **Fails: +1 edge and >0.1% ft** | +0 / 0.04418% |

**Measurement judgment:** the implementer's numeric receipt is supported by the logs. DEC-184 (5)'s statement that every CS1 leg had ≤0.1% feature growth over its last hour is not. Primitives and sigops fail even the historical **≤1-edge** allowance stated in (5). Verification meets that allowance but does not meet (1)'s strict zero-edge definition. This distinction is an evidence conflict, not a production divergence.

I recognize that (5) records the manager's administrative acceptance; it does not make the conflicting measured growth disappear. I cannot certify the requested 60-minute evidence by treating that statement as an unstated waiver of the feature bound. All four legs meet a 15-minute window, but retrospective selection of that shorter window does not establish the 60-minute gate specified in this brief. Configurability down to 15 minutes must not silently change what this review certifies.

**What closes M1:** once FZ1 lands, resume/rerun **`diff_script_primitives` and `diff_sigops`** on the reviewed implementation or a newly gated successor, retaining the existing corpus, until each has a recorded full **60 minutes of its own fuzzing time with zero added edges and ≤0.1% ft growth**, with zero divergences/crashes, clean revision/build binding and the required class accounting. Do not aim at a replacement CPU-hour budget. For the **strict** 60-minute rule in this brief, `diff_script_verify` also needs a qualifying zero-edge window; `diff_sighash` already qualifies. If (5)'s one-edge allowance is retained explicitly as grandfathering for this historical campaign, only the two feature-bound failures need rerunning, and verification must be labeled grandfathered rather than strictly plateaued. Correct the acceptance wording and attach the resulting receipts. A low-coverage plateau still calls for DEC-184 (3)'s coverage review, not extra hours.

### M2 — CLOSED: actual campaign distribution and reached-stage accounting supplied

**Locations:** `node-l1/src/accounting.rs:24`, `:110`; `node-l1/src/script.rs:188`, `:218`, `:315`, `:1077`; `node-fuzz/src/lib.rs`; `node-fuzz/fuzzctl.py:1625`; receipt `:91`.

The running comparisons now count each input in its actual class and observe instruction/checker/verifier-hook entry directly. They no longer infer depth from the terminal error. The harness periodically writes input-free counters and flushes at normal exit; the ledger preserves each slice's totals, and the receipt applies the class floor to the whole campaign leg against exact shares. The balancing mutator changes exploration while leaving replay's input-to-case mapping intact.

My independent sum matches every class total and stage count in the receipt:

- Verification: **63,142,102** counted executions, **11/11 templates**, shares **9.00–9.90%**; all seven slices have `floor_met=True`. Every template reports its observed opcode share; signature and classification stages/skips are reported where applicable.
- Primitives: **481,367,745** counted executions, **4/4 modes**, shares **24.94–25.16%**; all seven slices have `floor_met=True`. Eval, pubkey, encoding and FindAndDelete have the appropriate observed counters.
- Sigops: **795,399,169** accounted executions, **3/3 classes at about 33.33%**; the transaction class reports **53,653,989** completed comparisons and **47,416,642** coins-view comparisons.
- Sighash: **843,793,102** accounted executions, including **120,557,962** completed comparisons, **714,649,726** decode skips and **8,585,414** no-input skips.

Accounting equals worker execution statistics exactly on both mandatory-floor legs. The interrupted tails leave **176,500** sighash executions and **59,424** sigops executions unaccounted, each below 0.021%; this limitation is disclosed and does not affect either required floor. The generator-accounting, first-byte/class-contract and balancing tests pass. CHECKDATASIG's reported deepest stage is intentionally the signature opcode because it bypasses the checker hook. The new minor finding below qualifies what `verify` means; it does not invalidate the measured template/mode or instruction/checker-entry counts.

### m1 — CLOSED: CHECKDATASIG trailing-byte presence and value are independent

**Locations:** `node-l1/src/script.rs:473`, `:882`, `:927`, `:1051`; `node-l1/tests/l1_script.rs:179`.

`DataSig.trailing` is now optional; a separate selector decides whether it is present, and `hash_type` independently favors all six defined types while permitting any byte. Signature mutation has its own parameter. The new oracle-linked test requires successful single and chained CHECKDATASIG spends under **STANDARD, DERSIG, LOW_S, STRICTENC and NULLFAIL**, and compares Core/Rust verdicts. It passed on the exact reviewed head. The previous generator exclusion is removed.

### m2 — CLOSED: census masks and hybrid-key positions corrected

**Locations:** `contrib/node/cs1-script-census.py:135`, `:167`, `:171`, `:189`; `docs/node-program/research/CS1-script-census.md:13`.

The census separates STRICTENC's defined-type test (`& ~0x80`) from SignatureHash's NONE/SINGLE selection (`& 0x1f`), so undefined types can also count as NONE or SINGLE/SINGLE-bug. Its eight committed synthetic controls pass. I additionally checked synthetic hybrid-key pushes in **scriptSig, output scriptPubKey, spent bare script and P2SH redeem script**: 4/4 position controls pass. The research update records mainnet's three additional SINGLE and two NONE candidates and qualifies the scan as candidate counting.

I did not independently rerun either full historical chain census; closure here certifies the corrected classifier and controls, not a new independent measurement of the historical totals.

### m3 — CLOSED: Rust-only guards distinguished and independently reached

**Locations:** `dash-consensus/src/script/input.rs:25`, `:37`, `:241`; `dash-consensus/tests/script_guards.rs:23`; `contrib/node/script-branch-census.sh:140`.

The census distinguishes `rust-only` from `unreachable` and checks the former against a separate `script_guards` profile. Both guard tests pass, including invalid-mask rejection, Display and error conversion. Independently rerunning the instrumented census gives **1,339 Core-backed reached lines, 88 error sites, 8 unreachable lines, 7 Rust-only guard lines reached, 0 missed**. These are line counts, not a branch-coverage claim.

The two round-1 `ScriptPolicy::new` repro calls no longer apply: that constructor and `of_release` were removed, fields are private, and the sole public policy is const **`V24_0`**, with compile-time mask checks. Removing those configurable paths is a valid v24-only API change, not a missing test of a surviving constructor.

### n1 — CLOSED: original API/evidence drift reconciled

**Locations:** `docs/node-program/designs/CS1-script.md:617`, `:849`, `:871`, `:878`, `:923`, `:1186`.

The as-built notes now describe validated/private InputCheck construction, `ScriptPolicy::V24_0`, `CheckInputsError`, the cfg(test) old-sighash twin and the separate node-l1 oracle comparisons. They identify the actual test locations, real-signature generation, separate guard census and campaign accounting. The ordinary gate is distinguished from the acceptance gate, and PLAN/design budget citations refer to DEC-184. M1's disputed acceptance statement remains the major item above; it is not evidence that the original API descriptions were left unchanged.

## New findings

### m4 — Minor: `verify` overstates the depth actually observed

**Locations:** `node-l1/src/accounting.rs:34`, `:143`; `dash-consensus/src/script/ecdsa.rs:164`; `docs/node-program/designs/CS1-script.md:924`; campaign receipt's `verify` columns.

`Probe::verify` sets `VERIFY_BIT` **before** calling `DirectVerifier`. `verify_lax` can then reject the curve point or DER signature before reaching `secp256k1::ecdsa::verify`. Thus the counter measures **verifier-hook entry/attempts**, whereas its API comment promises a signature reached secp256k1 verification. The measured rates cannot be quoted as rates of actual ECDSA evaluations.

**Concrete control:** an independent scratch-only Rust probe passed a shape-valid compressed key with an out-of-field x coordinate and an empty signature. The verifier returned false, the probe reported `verify` reached, and the production function returned at key parsing before cryptographic verification. No campaign input was inspected or reproduced for this control.

**Fix:** define/relabel the existing counter and receipt columns as verifier-hook attempts, including the design wording. If actual cryptographic execution depth is required, add a separate observation after successful key/signature parsing, with positive and parse-rejection controls, and collect that new counter. Existing counts can be retained with truthful labels; neither consensus behavior nor the original M2 class-floor evidence is shown wrong by this finding.

## Validation and limits

All commands ran at **Nice 10**, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER`, and `/work/scratch/cs1b-r2-sol/tmp` mode **0700**. Builds used my own `/work/scratch/cs1b-r2-sol/target` and the scratch worktree's `target/census`. Oracle-linked tests used `--features oracle`, the exact base-SDK pin and the hash-checked v24 archive from `/work/cache/oracle`. `DASH_EMBARGO_DIR` was unset for test/lint runs; private access was limited to the authorized campaign ledger and worker logs, retaining aggregate counts only.

- Selected `cargo test --locked -p dash-consensus -p node-l1 --features oracle --lib --no-fail-fast`, with explicit script/quirk/chain/accounting integration-test selectors: **exit 0; 195 tests passed, zero failed, 15 test binaries**. This includes the old-sighash twin, Core records/units/vectors, ECDSA edges, all four differential twins/fixed seeds, CHECKDATASIG encoding-flag positives, guard/observation tests, special sighash vectors and quirk 39.
- `contrib/node/script-branch-census.sh`: **exit 0**, counts recorded under m3.
- `cargo fmt --all --check`, targeted `cargo clippy --locked -p dash-consensus -p node-l1 --features oracle --all-targets -- -D warnings`, and `git diff --check 75c1b44e..HEAD`: **exit 0**.
- `contrib/node/check-pure.sh`: **exit 0; 3 crates pass**.
- `python3 -I node-fuzz/fuzzctl.py check`: **pass; 13 targets, 12 smoke legs, 12 farm legs**. `python3 -I -m unittest discover -s node-fuzz -p fuzzctl_test.py`: **56 tests pass**.
- Census classifier self-test: **8 cases pass**; additional hybrid-position controls: **4 pass**. Independent verifier-depth control: **passes and establishes m4**.
- Independent campaign totals, per-class sums and plateau-window reconstruction: counts recorded above. Both interrupted-tail timing estimates yield the same window growth and verdicts.

The reviewed worktree remained clean. No source edits, commit, push, GitHub action, systemd operation, process-pattern kill, chain/bench access or new fuzz campaign was performed. I did not rerun the full integration gate, private operator tests, the full historical census, phase-C performance tests or the exhaustive scratch sweeps from round 1. The supplied `GATE EXIT 0` at `b1c792e7` is an implementer claim, not an independently rerun full gate. This review does not certify the unimplemented CS6 caches/queue or later block/reorg integration. Fuzz counts and exercised lines do not prove unreachable input space has no bugs.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no

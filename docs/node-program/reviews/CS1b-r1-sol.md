# CS1 phase B — Sol review, round 1

Reviewed `node/cs1-phase-b` at `662b3ffae53a85e916a7568030980b2d5b924bb1`, six commits against merge-base `6767e982513aaaf08afbab82a9b1b343de5d97d5`. Reference: Dash Core `v24.0.0-rc.3`, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. Applied REVIEW-COMMON, the approved CS1 design and prior design/phase-A reviews, PLAN row CS1, DEC-82 and DEC-92, and the dash-core-security-auditor/code-review-validator workflows.

**No production consensus divergence was found. Changes are required in the evidence gate and test tooling.** The independent probes below agree with Core. The reported ordinary gate exit does not establish the approved phase-B fuzz gate.

## Findings, ranked by severity

### M1 — Major: the configured smoke budgets are below the approved floor; the merge campaign is not established

**Locations:** `node-fuzz/targets.toml:85`, `:94`, `:103`, `:112`; `docs/node-program/designs/CS1-script.md:918`, `:1145`; PLAN acceptance rules and M0 exit 10.

The four new targets configure only 60, 60, 30 and 30 CPU seconds of smoke. The approved design requires five minutes per target. It also explicitly requires at least 24 clean CPU-hours per target at phase-B merge. DEC-82 removes the v23 leg; it does not reduce those v24 budgets. The four reported execution totals and zero-divergence counts supply neither elapsed CPU credit nor evidence that the 24-hour campaigns completed on the reviewed implementation.

**Failure scenario:** `fuzzctl smoke` can finish successfully after roughly one fifth or one tenth of the required smoke budget. The ordinary gate can then be called green while the phase-B acceptance campaign remains unproved. No CS1-specific budget deferral appears in the supplied decisions/backlog; other tasks' explicit deferrals do not grant one here.

**Fix:** set each CS1 smoke floor to at least 300 CPU seconds. Supply aggregate, revision-bound receipts for the four v24 campaigns at the approved budgets, after addressing M2 and the generator issue below. If the manager deliberately defers the long campaign, record that exception, its owner and the downstream gate it blocks; do not claim M0 exit 10 or the original phase-B gate is satisfied. I did not inspect private campaign logs, so this is an evidence deficiency, not a claim about unseen run durations.

### M2 — Major: the actual fuzz campaigns do not record the required mode/template and execution-depth counts

**Locations:** `node-l1/tests/l1_script.rs:378`, `:394`, `:423`; `node-l1/src/script.rs:151`, `:248`, `:986`; integration with `node-fuzz/src/lib.rs:107`.

The per-template accounting is a separate deterministic 4,000-input test. LibFuzzer calls the shared comparisons directly; the harness discards their `Outcome` and records divergences. There are no actual-campaign counters for the eleven verification templates, the four primitives modes, decode skips, signature-check attempts or cryptographic verification. The unit test's uniform seed distribution says nothing about the distribution libFuzzer evolved. Primitives have no corresponding accounting test at all.

The stand-alone test also infers execution depth from the final error. That inference is incorrect: `BadOpcode` can occur **after** a successful signature check on a malformed tail, while empty scriptSig/scriptPubKey give `EvalFalse` without executing an opcode. Both are classified incorrectly by `parse_only`.

**Failure scenario:** a campaign can spend almost all of its budget in one mode or in decode/parse rejection, report a large execution count and zero differences, and still pass every committed accounting check. This defeats the design's requirement that each primitives mode and verification template receive at least 5% of actual campaign executions, with evaluated shares reported.

**Fix:** collect input-free aggregate accounting in the running fuzz targets and preserve it in the runner's campaign receipts. Count actual attempts/reached checks rather than inferring them solely from terminal errors. Enforce the approved distribution floor against those receipts, report skipped/completed sighash comparisons, and rerun the qualifying campaigns. Keep the deterministic test as a generator check.

### m1 — Minor: CHECKDATASIG's positive generator excludes every defined trailing hash-type byte

**Locations:** `node-l1/src/script.rs:837`; `node-l1/tests/l1_script.rs:435`.

`Spend::DataSig` appends the chosen trailing byte only when it is divisible by four. None of `01`, `02`, `03`, `81`, `82`, `83` meets that condition. Consequently an otherwise valid signature is either missing the byte required by DER checking or carries a byte STRICTENC rejects. The positive generator cannot exercise successful CHECKDATASIG under STANDARD. Its test only requires a pass under *some* flags.

**Concrete reproducer:** `build_case` on hex `0a060000000000000000010000000000000001` builds one CHECKDATASIG with a valid low-S harness signature, compressed key, empty message and STANDARD flags. Core returns `SIG_DER` (30). Sweeping the final selector through all 256 values gives **192 SIG_DER, 64 SIG_HASHTYPE, zero passes**. Appending exactly `01` to the unmutated DER signature instead makes the same Core STANDARD check pass.

**Fix:** select presence of the trailing byte independently from its value, permitting every byte, including all six defined values. Add successful single/chain cases under STANDARD and the applicable individual encoding flags. Existing Core-recorded positives mitigate this gap; the production CHECKDATASIG implementation matched Core.

### m2 — Minor: the history census uses STRICTENC's mask to classify NONE/SINGLE semantics

**Location:** `contrib/node/cs1-script-census.py:166`; affected claims in `docs/node-program/research/CS1-script-census.md:36`.

`base = hash_type & ~0x80` correctly detects defined hash types, but is then reused for NONE, SINGLE and the SINGLE-bug count. Core's signature hash selects those semantics with `hash_type & 0x1f`. Thus `23`, `43`, `a3`, etc. are undefined hash types **and** SINGLE; `22` and `a2` act as NONE.

**Concrete classifier input:** a scriptSig push of `300602010102010123`, input index 0, and no outputs. A synthetic block passed to the tool's `scan()` produces only `sig:hashtype-undefined: 1`; `sig:single` and `sig:single-bug` are both zero. They should each count one signature-shaped candidate. This is a tool reproducer, not a claimed valid spend.

**Fix:** retain a separate defined-type mask and use `& 0x1f` for hashing semantics; add candidate-count controls. Qualify the research conclusions as candidate counts unless an executed signature check is established. The tool examines signature-shaped scriptSig pushes, including unused pushes, and its hybrid-key counter omits keys embedded in output/spent/redeem scripts. Its current zero counts therefore do not prove the broad absence of hybrid-key use. Recompute the affected totals and harvested examples.

### m3 — Minor: ten of the census's excluded lines are reachable Rust API guards, not impossible paths

**Locations:** `dash-consensus/src/script/input.rs:24`, `:36`, `:104`, `:119`, `:125`, `:262`; `contrib/node/script-branch-census.sh:12`.

The census reproduced the claimed 18 excluded lines. Eight are the interpreter/encoding safety paths, whose internal preconditions hold. The other ten belong to Rust flag validation, formatting and conversion. Those paths are reachable and were added specifically to reject bad caller masks.

**Concrete calls:** `InputCheck::new(&txdata, 0, &[OP_1], CLEANSTACK)`, `ScriptPolicy::new(CLEANSTACK, P2SH)`, and `ScriptPolicy::new(STANDARD, CLEANSTACK)` all reach marked error branches. Formatting the returned error reaches the marked Display implementation. These calls returned errors correctly in my probes.

**Fix:** distinguish exclusions for Rust-only defensive behavior from consensus-unreachable branches, and add ordinary Rust tests for the constructor guards and error conversions. Core aborting on the corresponding invalid mask justifies excluding them from a *Core-backed* comparison; it does not make them unreachable or remove the need to test the new guard behavior. The 1,353/88/0 line totals are reproducible, but are not a branch-coverage proof.

### n1 — Nit: reconcile the normative API and evidence descriptions with the implementation

**Locations:** `docs/node-program/designs/CS1-script.md:615`, `:631`, `:655`, `:853`, `:1145`.

The design still advertises public InputCheck fields, release-specific ScriptPolicy constants, an `InputScriptFailure`-only return, and an oracle leg for the old-sighash twin. The implementation has validated constructors/private fields, `of_release`, `CheckInputsError`, and a cfg(test) twin without that oracle feature. Update the actual API, test locations and accepted deviations, and retain the distinction between the ordinary gate and the phase-B acceptance gate. This is documentation work; the new API choices themselves are reasonable.

## Audit results

| Surface | Result |
|---|---|
| Sighash and SignatureHashOld | Plain serializer matches Core's field order, ALL-acting undefined types, NONE/SINGLE, ANYONECANPAY and ONE shortcut. ScriptCode stripping preserves Core's malformed-tail cursor behavior. The 50,000-case old twin narrows its malformed-tail assertion appropriately. Special-payload coverage includes 864 live-checked rows over 18 transactions. Independent probes compared all 256 byte hash types and seven additional full-i32 types over every input of three-input transactions, 0–3 outputs and separator/malformed-tail scripts: **47,340 comparisons, zero differences**. |
| Interpreter, strictness, limits and error ordering | Audited encoding checks, disabled/skipped opcodes, separator/FindAndDelete behavior, multisig key charging and cleanup, DIP0020 and CHECKDATASIG. Independent boundaries included 520/521-byte pushes, 10,000/10,001-byte limits, 999–1,001 stacks, altstack movement, 200–202 opcodes, multisig charge before missing-key errors and missing/non-null dummies: **216 comparisons including failure stacks, zero differences**. L0 covers the additional encoding/NULLFAIL cases. |
| Input checks/classification | First error retained, first failing input chosen, coinbase bypass and v24 mandatory/non-mandatory retry match Core. Independent seven-gadget sweep over all 6,144 valid combinations of the thirteen defined flags: **43,008 whole-transaction classification/reason comparisons, zero differences**. |
| Signature-cache boundary | Only the pure hook is implemented here; actual stores/erase-on-hit and script-execution cache are CS6. Recording-verifier probe confirmed the trailing hash-type byte is removed, digest/key/signature are deterministic across repeated checks, and empty signatures/invalid-length keys bypass the hook. CHECKDATASIG directly verifies and bypasses the hook, matching Core. No cache correctness claim is made for unimplemented CS6 state. |
| Sigops | Script-level inaccurate/accurate counts, non-P2SH fallback, final-parser-data extraction, transaction P2SH filtering, coinbase bypass and wrapping accumulation agree with pinned Core. CHECKDATASIG counts zero. Committed differentials passed. |
| Flags per height | Source mapping agrees with Core's GetBlockScriptFlags. P2SH is unconditional; DERSIG/CLTV/CSV/NULLDUMMY follow their individual buried activations. Mainnet heights: 245817/619382/622944/939456; testnet: 2075/2431/8064/4300 in that same flag order. No DIP0020/CHECKDATASIG activation flag exists in pinned Core. Chain tests passed at all recorded boundaries. Valid history alone cannot demonstrate rejection after an activation. |
| Panics/overflow | No script-derived panic or arithmetic divergence found in the reviewed interpreter/checker paths. Public input-index and invalid low-level flag assertions are documented preconditions. Numeric interpreter operands stay within their 4/5-byte bounds; counts stay bounded. |

## Deviation dispositions

- **`ScriptPolicy::of_release`: accept.** It uses the mandatory mask supplied by the rules rather than adding a second release selector. Validated/private construction improves the API. Reconcile the design text.
- **Differentials in node-l1 with real signatures: accept the location and signing approach.** Sharing comparisons with the fuzz targets is appropriate and the comparisons check the right outputs. The existing recorder retains mock-checker evidence. Actual campaign accounting and the missing CHECKDATASIG positive combinations still require changes.
- **Python chain sample: accept.** It uses read-only RPC, has a committed fixture and hashes, and both Rust and the oracle replay its inputs. It is a limited sample, not full historical replay. The census qualifications/fixes above apply separately.
- **Omitted regtest FT-chain classification vectors: not completed.** The live classification differential and my exhaustive flag sweep support the pure classification code, but they do not replace an end-to-end activation/rejected-block check. Record an owned follow-up and its CH3a/functional-parity gate, or deliver the approved vectors. Valid mainnet/testnet inputs cannot establish those rejection paths.
- **§4.11/§9 drift: update before presenting this as the approved contract.** Accept constructor/error-type changes; do not silently move/remove fuzz evidence obligations.

The full thirteen-item implementer report was not present in the supplied material or branch documents. I requested it during the review; only the five highlighted deviations can be individually dispositioned here. The remaining eight are not certified by this review.

## Validation and limits

All builds/tests ran in a detached worktree under `/work/scratch/cs1b-review-r1/`, with `TMPDIR=/dev/shm/cs1b-r1-tmp` mode 0700 and `CARGO_BUILD_JOBS=8`. Production changes, the implementer worktree, and Core sources were not modified.

- `cargo test --locked -p dash-consensus -p node-l1 -p dash-oracle-sys --features node-l1/oracle,dash-oracle-sys/oracle --no-fail-fast`: **exit 0, 332 tests passed, zero failed**, including six new vector tests and sixteen ported Core-unit tests. Existing tests also exercise the cached v23 archive; review acceptance uses v24 only.
- `contrib/node/script-branch-census.sh`: **exit 0; 1,353 reached lines, 88 error sites, 18 excluded lines, zero missed**.
- `cargo fmt --all --check` and targeted all-features/all-targets clippy with `-D warnings`: **exit 0**.
- `contrib/node/check-pure.sh`: **exit 0, three crates pass**. `python3 -I node-fuzz/fuzzctl.py check`: **pass**.
- `contrib/node/check-private-tests.sh --selftest`: **pass**.
- Authorized private operator: **PASS, exit 0; 522 cases; 1,454 seeds, 279 required**. No private files were opened or private inputs inspected; only pass/fail and counts are reported.
- Sample fixture/provenance hashes match: **227 mainnet blocks/1,773 inputs plus 18 quirk transactions; 176 testnet blocks/244 inputs**. Special-sighash fixture: **864 rows/18 transactions**; hash matches provenance and live oracle regeneration.
- Five additional scratch-only probe tests passed. Besides the Core comparisons above, they reproduce m1 and m3. A synthetic Python classifier probe reproduces m2. Scratch worktree, probes, build artifacts and TMPDIR are removed after writing this report.

I did not rerun the full 169-binary integration gate, inspect private fuzz campaigns, complete a 24-hour campaign, rerun the full chain census, or evaluate phase-C performance. The manager's PF0 merge measurement remains separate. No GitHub action, commit or push was made.

**Verdict: CHANGES-REQUIRED.**

**Sol gate satisfied: no.** Close M1/M2 with the corrected generators and reviewable aggregate campaign evidence before advancing the critical review to Opus. This verdict does not assert a production consensus mismatch.

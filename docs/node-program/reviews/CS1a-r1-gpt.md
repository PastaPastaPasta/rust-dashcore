# CS1 phase A, round 1: independent adversarial GPT review

- **Reviewed:** rust-dashcore `node/cs1-script`, HEAD `620849c756c0bed4b1f27fd1503da82b24726137`, worktree `/work/home/agent/workspace/rdc-wt/cs1-script`.
- **Contract:** `briefs/REVIEW-COMMON.md`; `designs/CS1-script.md`, DEC-25 and DEC-38. Applied the code-review-validator skill. Read both `CS1a-r1-opus-consensus.md` and `CS1a-r1-opus-quality.md`.
- **Scope:** phase A: interpreter, parsing, numbers, encodings, ECDSA, CHECKDATASIG, flags and script-level sigops. Transaction sighash/checkers, transaction-level classification and phase C optimizations remain outside this approval.
- **Verdict:** **APPROVE-WITH-NITS**. No blockers, majors or consensus divergences found. Two small follow-ups below.
- **Isolation:** branch sources remained unchanged; no commits, pushes, GitHub comments or dashd instances. Probe sources, binaries and logs are in `/work/scratch/cs1a-gpt/`. Tests used `TMPDIR=/dev/shm/cs1a-gpt-tmp`, mode 0700; it was removed after testing.

## Findings

### Blockers / majors

None.

### n1 — nit: the instruction iterator can resume inside a malformed push

**Location:** `dash-consensus/src/script/parse.rs:62-66`.

The helper says it stops at the first instruction `get_op` cannot read, but `from_fn` does not fuse the iterator. For `[OP_PUSHDATA2, OP_1]`, the first `next()` returns `None` after consuming the PUSHDATA2 opcode; the second returns `Some(OP_1)`, interpreting an incomplete size-field byte as a new instruction. I reproduced `None, Some(OP_1), None` in `probe/src/bin/api_probe.rs`; see `api-probe.log`.

**Failure scenario:** a caller retries or otherwise resumes an iterator after a parse failure and treats push-length bytes as instructions, contrary to the helper's stopping description. Current consensus code uses `get_op` directly, and the helper's only current call is a test-side `collect`, which stops at the first `None`. This does not change validation or sigop counts today.

**Suggested fix:** append `.fuse()` to this helper's iterator, or retain a permanent failure state. Keep `get_op` unchanged: its failure cursor intentionally matches Core. Add the two-byte reproducer with repeated `next()` calls.

### n2 — nit: preserve deterministic deep-conditional coverage in the committed tests

**Location:** `dash-consensus/tests/script_properties.rs:18-40`; `dash-consensus/benches/script.rs:65`.

The structured property generator has fewer than 24 pieces and initial stacks below five elements; the raw no-panic input is shorter than 200 bytes. Neither is a dependable way to reach the conditional-depth/op-count corner. The nested-IF benchmark exercises 100 true branches, but does not cover false ancestors, repeated ELSE toggles above the first false, or the transition between deep unbalanced conditionals and OP_COUNT.

**Failure scenario:** a later edit mishandles `ConditionStack::first_false` at depth or changes limit precedence; line coverage and short random scripts can still pass. This is an evidence-retention gap, not a defect observed in this implementation: my targeted depth sweep and all-flags cases agree with both pins.

**Suggested fix:** retain a small deterministic Core-recorded set for balanced depths 99/100/101, open depths 200/201/202, and repeated ELSE at different levels under a false ancestor. Bring those seeds into phase B's oracle differential rather than depending on the scratch review campaign.

## Independent Core comparison

I extracted the exact tagged sources for `interpreter.cpp`, `script.cpp`, `script.h`, `interpreter.h`, `pubkey.cpp` and `pubkey.h` under `core/`. The two interpreter pins differ only in copyright and empty-vector construction in SPLIT; the script implementation differs only in copyright. The pubkey changes add derivation functionality outside signature verification. The oracle-shim worktrees' interpreter/script/pubkey implementations have no differences from their release tags and their working trees were clean.

I compared the interpreter's execution and failure order against those sources, concentrating on:

- Script size before parsing; push size before op count or branch execution; counted skipped opcodes; disabled opcodes and CONST_SCRIPTCODE in skipped branches; stack-size checks after an instruction rather than on entry.
- IF/NOTIF consumption only in executed branches, repeated ELSE, VERIF/VERNOTIF in skipped branches, and the constant-time first-false condition representation.
- CHECKSIG/CHECKMULTISIG taking scriptCode after the last **executed** separator, deleting canonical signature pushes before encoding checks, retaining malformed tails, and ignoring signature-like bytes inside another push.
- Multisig key count, key op charge, signature count, FindAndDelete, reached-pair encodings, early exit, partial NULLFAIL cleanup, NULLDUMMY and VERIFY in Core's order. Empty or bad encodings on unreached pairs remain unchecked, while NULLFAIL still covers unused nonempty signatures after a failed operation.
- NUM2BIN negative sizes, signed padding, negative zero and mutation before ImpossibleEncoding; BIN2NUM mutation before InvalidNumberRange; SPLIT decode/range errors; equal-sized bitwise inputs; DIV/MOD zero and numeric-minimality precedence.
- CHECKDATASIG using the whole signature over one SHA-256, including its unusual interaction with strict DER/hash-type rules; correct/bad-parity hybrid keys; lax length descriptors; high S and scalar-overflow handling.

No mismatch emerged from the source audit or the probes below. I agree with the Opus reviews' central consensus conclusion. Their documented phase B/CS0 reconciliation and census follow-ups remain relevant; this pass does not claim to re-run their merge rehearsal or performance-counter measurements.

## My targeted O1 differential

`probe/` is a separate Cargo workspace. It consumes the reviewed `dash-consensus` directly and **`dash-oracle-sys` from `node/integration`, with feature `oracle`**. The generator and comparisons were written for this review rather than copied from the recorder or either prior review. Release builds enabled debug assertions and overflow checks. The harness asserts `ARCHIVES_PINNED` and compares each case to both pins, including result, numeric error and complete final stack on success **and failure**.

Authenticated plain archives:

| Pin | Shim commit | Archive SHA-256 |
|---|---|---|
| v23.1.8 | `350ce242a59c19782dc9eebc8e23dead0a2e16ee` | `22f00737694ab069e22d01d2cee3fc749b8c5c3814023a0a086d230c03829463` |
| v24.0.0-rc.3 | `23769258c239e69282569e9f9e219ce9e2c4ee2e` | `c9cbd75604c6deef3158854ebeb674b0ff12cf21fc5dba13b38a04ffa99fe29b` |

**Results per pin:**

| Probe | Cases | Mismatches |
|---|---:|---:|
| Deep IF/NOTIF, toggles and depth-limit precedence | 78,720 | 0 |
| Opcode, op-count, size, stack and truncated-push precedence | 293,632 | 0 |
| DIP0020 sizes/signs and arithmetic boundaries | 38,460 | 0 |
| Multisig failure ordering, FindAndDelete and key op charge | 556,416 | 0 |
| CHECKDATASIG signature/key/flag matrix | 589,824 | 0 |
| **Total EvalScript comparisons** | **1,557,052** | **0** |
| Lax-DER length matrix, parser compact bytes and CheckLowS | 65,536 | 0 |
| FindAndDelete script bytes and deletion count | 11,000 | 0 |

Selected deep-conditional, opcode, multisig FindAndDelete and CHECKDATASIG templates run under **all 8,192 combinations of the 13 defined flag bits**. Other targeted cases use the relevant flag subsets, the retired DIP0020 bit, or all bits set. This is exhaustive for those templates, not a claim to enumerate every possible script.

The depth sweep runs IF/NOTIF chains through the first op-count failure, with branch closure/toggling at several positions and suitable initial stacks for true branches. DIP0020 covers lengths 0, 1, 2, 3, 4, 5, 75, 76, 127, 128, 255, 256, 511, 519 and 520; targets/ranges include negatives, zero, 519/520/521 and 2^31. The size-precedence leg separately includes over-520 pushes and over-10,000 scripts. CHECKDATASIG includes real successful signatures, high S, trailing hash-type bytes, padded/long-form DER, compressed/uncompressed keys and both hybrid parity outcomes.

Log: `targeted-diff.log`. Reproduce with:

```sh
cd /work/scratch/cs1a-gpt/probe
TMPDIR=/dev/shm/cs1a-gpt-tmp CARGO_BUILD_JOBS=12 cargo run --release --offline --bin cs1a-gpt-probe
```

Recreate the temporary directory with mode 0700 before replaying.

## Exact checker-call differential

O1's EvalScript checker always returns false. To independently cover success and exact key/signature ordering, I wrote `trace.cpp`, linked against each pin's existing Core consensus/common/util/crypto/secp256k1 archives. The checker takes a bit mask prescribing successive check outcomes and records every `(signature, pubkey, scriptCode)` triple. The Rust counterpart uses the same outcome schedule and serializes its own calls. **The full transcripts are compared byte for byte**, together with error/result and EvalScript failure stacks; they are not reduced to a hash-dependent boolean.

Templates include executed and skipped separators, multiple separators, canonical versus noncanonical signature pushes, signatures embedded as push data, empty/duplicate signatures, truncated script tails, multisig reached/unreached encodings, dummy variations, bare/P2SH VerifyScript, CLEANSTACK, and CLTV/CSV. Up to four keys use every outcome mask. Additional cases exercise complete 20-key loops with 0/1/19/20 signatures, bad encodings at both ends, all-success/all-failure/alternating outcomes, and 179/180/181 leading NOPs.

- **1,498,192 cases per pin**, with **825,152 exact checker calls per pin** and 276,214 successful evaluations/verifications in the Rust reference output.
- **326,574,004 serialized output bytes per pin**, identical to Rust; **zero mismatches** on both pins.
- Real negative control: changing the C++ checker's first outcome bit changes the comparison output on each pin (`trace-negative-control.log`). This is separate from the byte-mutation sanity check printed by the trace harness.

Sources: `trace.cpp`, `probe/src/bin/trace_check.rs`. Results: `checker-trace.log`. Run the Rust binary with `cargo run --release --offline --bin trace_check`; the Core runners are `trace-v23.1` and `trace-v24.0`. Their Core implementation files were verified against the tags as noted above.

## Resource bounds and generator assessment

I found no script-derived panic under the valid caller contract. CLEANSTACK without P2SH can intentionally panic at Core's assertion point, and `ScriptNum::decode` rejects a caller-selected width above eight by assertion; neither value is supplied by an adversarial script. Interpreter numeric reads use four or five bytes, so their arithmetic cannot reach signed-64 overflow or division overflow. Conditional depth is bounded by the op limit and uses no recursive evaluation or per-level scan.

In VerifyScript, the 10,000-byte script limit, 520-byte elements, 1,000 combined stack entries and 201 counted ops bound allocations/work. P2SH is one additional evaluation, without recursive P2SH execution. Direct EvalScript accepts caller-provided initial stacks, including oversized elements and an empty-script stack above 1,000, exactly as Core does; those caller inputs should not be confused with stacks attainable from scriptSig execution.

FindAndDelete builds its output in one forward pass. Canonical signature-push patterns constrain comparison cost to the instructions examined; deletion does not retry to a fixpoint. Multisig repeats that pass for at most 20 signatures. Repeated signature opcodes can rescan/copy scriptCode, and ROLL can shift stack entries, but both mirror Core and remain bounded by script/op/stack limits. CAT growth stops at 520 bytes; SPLIT/NUM2BIN/BIN2NUM scan bounded operands. I found no additional quadratic conditional path or unbounded script-driven growth. Wall-clock performance parity remains the separately owned benchmark gate (DEC-38), not a claim inferred from these comparisons.

The existing recorder and Opus generators cover named multisig precedence, FindAndDelete and DIP0020 boundaries well. Short random streams alone cannot establish deep-conditional coverage or reliably reach valid long multisig loops. The independent targeted and transcript probes above address those corners for this review; n2 asks to retain the depth evidence in the repository. The formal phase B oracle tests/fuzz targets and full script_tests.json execution are still required by the approved design.

## Local validation

- `cargo test -p dash-consensus --all-features --locked`: **78 passed**, zero failures (`consensus-tests.log`).
- `cargo clippy -p dash-consensus --all-features --all-targets --locked --offline -- -D warnings`: **passed** (`clippy.log`).
- `cargo fmt --check -p dash-consensus -p dash-core-vectors`: **passed** (`fmt.log`).
- `cargo test -p dash-core-vectors --locked --offline reference_serialize_script_code_edge_cases`: **passed**, validating the relocated serializer regression (`vector-test.log`).
- `api_probe`: reproduced n1 (`api-probe.log`).
- Final reviewed branch HEAD remained `620849c7`, with a clean working tree.

**Verdict: APPROVE-WITH-NITS.**

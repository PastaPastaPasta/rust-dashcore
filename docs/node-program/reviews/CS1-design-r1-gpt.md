# CS1 script design — independent review, round 1

**Verdict: APPROVE-WITH-CHANGES. No blocker; four major findings.** The interpreter, ECDSA, sighash and cache boundaries are sound enough to retain. Correct the helper contracts and test requirements below before treating the design as approved for implementation. These are design findings: no Rust implementation was supplied or reviewed.

Reviewed `designs/CS1-script.md` dated 2026-10-08, SHA256 `530633b11591bac22df1bd94cc84d07f8b37c5cac2679c6072b94daeab3143bb`, with DEC-25, A1, CS0's public contracts, R4 §1.4 and R4-A. DEC-25 resolves the ownership, writer, oracle follow-up, quirk-registration and dependency questions; this review does not reopen them. CS0's private module and private material were outside this review.

Compared Core **v23.1.8** (`728f5055836c6d29806412fc7223ac8fe05af991`) and **v24.0.0-rc.3** (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`). Inspected the interpreter, script parser/numbers, pubkey implementation, sigops, policy masks, input-check retry and block-validation call sites. Applied the `dash-core-security-auditor` workflow. Compiled and ran independent C++ probes against both pins' existing Core archives in the oracle-shim worktrees; the relevant script/pubkey/tx-verify sources have no diff from their respective tags. All probe sources, binaries, build scripts and logs are under `/work/scratch/cs1-review-gpt/`. This task added only this review file; it did not modify repository source, designs or decisions.

## Major findings

### M1 — `FindAndDelete` is not idempotent for the API's arbitrary patterns

**Section:** §4.3, lines 219–221; §7.2, line 598.  
**Core:** both pins, `src/script/interpreter.cpp:224–247`.

**Problem.** The proposed property, “`find_and_delete`'s idempotence on its own output,” is false for the public `pattern: &[u8]` interface and the arbitrary-pattern differential target. Core performs one cursor traversal; removing a match can bring earlier and later bytes together into a new match that the traversal has already passed.

Both Core probes produce:

```text
script = 51 51 52 52                 # OP_1 OP_1 OP_2 OP_2
pattern = 51 52
first call:  count = 1, script = 51 52
second call: count = 1, script = empty
```

Changing the port to repeat deletion until stable would satisfy the proposed property while disagreeing with Core. The “opcode-boundary matches only” shorthand also needs qualification for arbitrary patterns: with script `01 01` and pattern `01`, Core deletes **both** bytes, count 2, although the second byte was originally push data. Its adjacent-match loop advances by the pattern length and does not re-establish an original instruction boundary between adjacent matches.

**Fix.** Remove the unrestricted idempotence property. If retaining one, explicitly restrict it to the canonical, complete single-push patterns used by signature deletion and prove that narrower property. State the actual cursor algorithm: match at the current cursor, consume adjacent matches, then resume `GetOp` from the resulting cursor. Add both examples above to the Core-derived regressions, plus empty patterns, non-canonical signature pushes, matches inside push data, and malformed tails. Preserve Core's one-pass result and count.

### M2 — The two sigop layers need different non-P2SH behaviour

**Section:** §4.10, lines 447–460; §7.4 `diff_sigops`.  
**Core:** both pins, `src/script/script.cpp:178–199`; `src/consensus/tx_verify.cpp:134–158`.

**Problem.** The design promises both forms of `CScript::GetSigOpCount`, but the stated contract for `p2sh_script_sigop_count(spk, script_sig)` only describes extracting the last push. Core's overload first does:

```cpp
if (!IsPayToScriptHash())
    return GetSigOpCount(true);
```

For non-P2SH `spk = OP_2 CHECKMULTISIG`, and even `script_sig = OP_RETURN`, the helper returns **2**. It does not inspect or reject that scriptSig. In contrast, transaction-level `GetP2SHSigOpCount` explicitly filters spent outputs with `IsPayToScriptHash()` and contributes **0** for this input. Blindly summing the script-level overload would overcount ordinary spent scripts and could reject a valid block at the sigop limit.

“Counts the last push” also obscures a parser detail relevant to the proposed `Op::Push`/`Op::Code` representation: `GetScriptOp` clears `vData` at every call. For a P2SH spk, scriptSig `<ac> OP_1` returns **0**, whereas `<ac>` returns **1**. Keeping the last `Op::Push` data when the final opcode is `Op::Code(OP_1)` would give the wrong answer. The same clearing applies to `OP_RESERVED` and `OP_1NEGATE`; this helper does not execute the scriptSig.

**Fix.** Specify the non-P2SH accurate-count fallback on the script-level overload, and the explicit P2SH predicate in the transaction-level aggregator. Specify that extraction uses the **final successful parser call's data**, including clearing it for small-integer/reserved opcodes. Add oracle regressions for non-P2SH fallback, the transaction-level exclusion, trailing `OP_1`/`OP_RESERVED`, empty scriptSig, and truncated pushes. Verify accurate/inaccurate counts independently of script execution.

### M3 — The normative resource-limit checklist omits CHECKMULTISIG's key-count charge

**Section:** §4.5, lines 262–299; §7.2.  
**Core:** both pins, `src/script/interpreter.cpp:1100–1112`.

**Problem.** The control-flow checklist enumerates the `opcode > OP_16` charge and 201-op limit, but does not enumerate CHECKMULTISIG's additional `nKeysCount` charge. Its multisig bullets cover deletion, reached encoding checks and cleanup, leaving this consensus limit to the general “port exactly” instruction.

The exact sequence matters: decode the key count; reject outside `[0,20]`; **add the declared key count and check 201**; only then check whether the stack has those keys. This charge occurs for an executed CHECKMULTISIG, including **zero-of-20** multisig where no ECDSA verification occurs. It does not charge keys for a skipped CHECKMULTISIG.

The probes construct `N × OP_NOP`, then a dummy, zero signature count, 20 empty key elements, key count 20 and CHECKMULTISIG:

| N | Core operation count | Both pins |
|---|---|---|
| 180 | 180 + 1 + 20 = 201 | success, stack `[01]` |
| 181 | 181 + 1 + 20 = 202 | `OP_COUNT` (6) |

A script with 181 NOPs and a declared 20-key count but missing the key elements also reports `OP_COUNT`, before `INVALID_STACK_OPERATION`. An implementation following only the stated per-opcode limit could accept the complete 202-op example.

**Fix.** Add this charge and its ordering explicitly to the normative checklist and named tests. Include 201/202 boundaries, zero signatures, missing keys competing with the limit, and skipped multisig. Keep separate tests for resetting op count and altstack between scriptSig, scriptPubKey and redeem-script evaluations. This is a specification/coverage omission, not a claim that an existing Rust port already accepts the witness.

### M4 — The `tx_valid` maximality runner names the wrong O4 helper

**Section:** §7.1, line 583.  
**Supporting code:** O4 `test-vectors/src/flags.rs:139–163`.

**Problem.** The design says to check `tx_valid` maximality via `exclude_individual_flags`. O4 distinguishes two operations:

- `exclude_individual_flags(enabled)` removes an enabled flag and produces weaker sets.
- `restore_one_excluded(excluded)` starts with `DEFINED_FLAGS & !excluded` and turns each excluded flag back on, repairing CLEANSTACK/P2SH combinations.

Only the second operation directly tests `tx_valid` maximality. Calling the first with the enabled set repeats validity-under-removal tests; expecting rejection there contradicts monotonicity. Calling it with the vector's excluded set and then complementing the outputs also applies `trim_flags` in the wrong mask space. For example, exclusions `CLEANSTACK | SIGPUSHONLY` should restore those flags separately; this approach can restore both together when SIGPUSHONLY is removed, losing the individual test. O4's audit validates `restore_one_excluded`, not either mistaken interpretation.

**Fix.** Name `restore_one_excluded(case.excluded_flags)` in the runner contract. Require at least one input to fail for each returned enabled set. Continue using removal sets for passing-vector monotonicity and addition sets for `tx_invalid`; retain the decision not to assert `tx_invalid` minimality. Add a small runner self-check showing which flag is restored and which enabled mask reaches the verifier.

## Other findings

### F5 — Minor: the v23 classification summary incorrectly says “every” flag failure is non-mandatory

**Section:** §6.2, line 569; §7.4 three-way comparison language.  
**Core:** v23 `src/validation.cpp:1912–1937`; v24 `:2198–2223`.

**Problem.** The table and §4.11 algorithm are correct, but the paragraph following them says every DERSIG/CLTV/CSV/NULLDUMMY failure becomes non-mandatory on v23. This is true only when the relaxed retry **succeeds**. It remains mandatory when a later failure survives the retry, with the original error retained.

Concrete witness under `P2SH | CLTV`:

```text
scriptSig = empty
scriptPubKey = OP_1NEGATE CLTV DROP RETURN
```

Both pins first report `NEGATIVE_LOCKTIME` (27). On v23, removing the non-mandatory flags reaches `OP_RETURN` (3), so the result is still `TX_CONSENSUS` with:

```text
mandatory-script-verify-flag-failed (Negative locktime)
```

Replacing RETURN with OP_1 makes the v23 retry pass and yields the non-mandatory classification; v24 retains CLTV and reports mandatory for both examples. The probes run Core's two `VerifyScript` calls with each pin's actual policy header; classification follows the inspected `CheckInputScripts` branch. The planned direct input-check oracle adapter is not available in the reviewed v1 shim.

**Fix.** Qualify the paragraph with “when the retry passes.” Add paired single-cause/multiple-cause classification vectors, asserting result class, first error and full reason. In §7.4, require Rust/profile equality against the corresponding pin; only interpreter results/digests must agree across both pins. Expected classification differences must not be routed to O17 as unexpected deltas.

### F6 — Minor, performance risk: fixed 128-input chunks and microbenchmarks do not establish queue throughput

**Section:** §8.1, lines 685–690; §8.2–8.3.  
**Supporting code:** Core `src/checkqueue.h:112–115`; v24 `src/init.cpp:1554–1569`.

**Problem.** `par_chunks(128)` with serial work inside each chunk provides one schedulable job for 1–128 inputs, two for 129–256, and only six for the cited 680-input workload. A 64-thread cap cannot overcome that granularity. Core's 128 is a **maximum** batch size; it shrinks batches according to queue length and active/idle workers. Its cap is 15 **additional** workers, with the calling thread also participating, rather than a strict 15-total-thread cap.

Cost is also highly uneven. A single input can execute many uncached CHECKDATASIG operations, all contributing zero to sigop accounting by design. A chunk of these inputs is materially different from a chunk of P2PKH inputs. The proposed ECDSA and nested-IF benches bypass scheduling, and the sighash benches do not measure an entire verified transaction with a cold/warm sigcache. Same-library ECDSA establishes neither interpreter nor full-input parity.

**Fix.** Carry an explicit scheduling/performance requirement to CS6: benchmark adaptive smaller chunks or individual tasks with worker scratch reuse. Include 1/16/127/128/129/680 inputs, mixed expensive/cheap inputs, uncached CHECKDATASIG chains, and cold/warm caches at equal effective thread counts. Record allocation counts and end-to-end input throughput. Keep C3 initially scoped to its documented SIGHASH_ALL regime and compare its gain on full verification. Preserve A1's declared ECDSA-per-core parity and treat any 2× claim as a measured workload result, with an Amdahl budget.

## Verification requirements and behaviours confirmed

The following observations support retaining the main design. They should become committed regressions rather than relying only on random fuzzing:

| Surface | Review result / required witness |
|---|---|
| Quirk 39 | **Confirmed on both pins with real signatures.** `<pk> CHECKSIGVERIFY 05 aa`, signed using Core's truncated serializer, reaches `BAD_OPCODE` (16). Signing the full tail instead produces `CHECKSIGVERIFY` (13) without NULLFAIL. In a P2SH wrapper under STANDARD it produces `SIG_NULLFAIL` (37). Use that P2SH form for the mempool-visible witness: an arbitrary bare nonstandard prevout may be rejected by input policy before script verification. Test incomplete PUSHDATA1/2/4 lengths, complete lengths with missing data, separators before the malformed tail, and `ab` bytes inside pushed data. |
| Executed CODESEPARATOR vs serialization | The design correctly separates the last **executed** separator's scriptCode start from stripping all subsequently parsed separators in the legacy serializer. CONST_SCRIPTCODE rejects even skipped separators before execution. |
| ECDSA | The lax parser, whole-signature CHECKDATASIG verification, normalize-S and hybrid-key handling are correctly scoped. The probes confirm a correctly paritied hybrid key verifies and the opposite parity fails. S equal to curve order is accepted by LOW_S's encoding path, then gives NULLFAIL on failed verification. Preserve the zeroing of **both** scalars when either overflows; a valid high S paired with overflowing R is another useful boundary seed. |
| Encoding precedence | Keep empty-signature exemption limited to signature encoding: STRICTENC still checks a reached pubkey. Assert DER → LOW_S → hashtype → pubkey → verification/NULLFAIL precedence and compressed/uncompressed off-curve keys separately from wrong header/length. |
| Multisig cleanup | A Core probe with a nonempty unused signature, an empty tried signature and a nonempty dummy reports NULLFAIL before NULLDUMMY, leaving `[01][7f]` after partial cleanup. Compare failure stacks, reverse key/signature traversal, early exit, and keys/signatures whose encoding checks are never reached. |
| Numbers and failure stacks | `0x7fffffff 1ADD` succeeds with five-byte `[0000008000]`; another numeric operation gives UNKNOWN_ERROR. NUM2BIN's impossible-encoding failure has already popped size and minimized the number. BIN2NUM's range failure leaves its normalized operand. Invalid PICK/ROLL range has already popped the index. Empty executed IF gives UNBALANCED_CONDITIONAL, not INVALID_STACK_OPERATION. Probes confirm these observations on both pins. |
| Limits and skipped branches | Cover 520/521-byte pushes, 10,000/10,001-byte scripts, 1,000/1,001 combined stack items, 201/202 operation counts, disabled/VERIF opcodes in skipped branches, and competing errors. `EvalScript` does not impose a separate initial-stack-size check: an empty script with 1,001 initial items succeeds. Its post-op check is the authoritative rule. |
| Cache and parallel boundary | `SigVerifier` belongs after hash-type removal and CPubKey shape validity; CHECKDATASIG correctly bypasses it. The generic parallel failure and serial classification distinction matches Core. CS6/CH3a integration tests must preserve precedence of a later non-script transaction failure over queued script failures and test caches enabled/disabled and erase-on-hit. |

The four differential targets are appropriate. Make the DEC-25 O1 follow-up a completion gate for their **extended** comparisons, rather than interpreting “once §7.5 lands” as permission to finish CS1 with those comparisons skipped. Record per-mode execution/coverage so the shared `diff_script_primitives` CPU-hour budget is not spent mostly on trivial parse rejection. Signed templates should include malformed/off-curve pubkeys and deliberate reached/unreached multisig encoding failures. These are verification requirements, not additional confirmed defects in the proposed port.

## Evidence and disposition

- `/work/scratch/cs1-review-gpt/core_probe.cpp`: deletion semantics, sigop overloads, multisig charges, number/stack behaviour and policy-mask retries.
- `/work/scratch/cs1-review-gpt/crypto_probe.cpp`: signed quirk-39 examples, hybrid parity and overflow-S/LOW_S behaviour.
- `/work/scratch/cs1-review-gpt/build_probes.py`: reproducible compilation and execution against both existing Core archive sets.
- `core_probe_v23.1.log`, `core_probe_v24.0.log`, `crypto_probe_v23.1.log`, `crypto_probe_v24.0.log`: final results; both builds and runs succeeded.

No new consensus exception, sighash algorithm change or checker redesign is required. Quirk 39 is valid and DEC-25's registration is justified. Approval is conditional on resolving M1–M4 in the design; F5 should be corrected at the same time, and F6 should be assigned to the CS6/PF verification work. The private canonical-writer implementation and end-to-end runtime/cache integration remain for their designated reviews.

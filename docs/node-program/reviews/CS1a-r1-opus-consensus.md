# CS1 phase A, round 1: adversarial consensus review (Opus, reviewer 1 of 2)

- **Branch:** rust-dashcore `node/cs1-script` at `620849c7` (21 commits on `node/integration` `77d9fd71`), worktree `~/workspace/rdc-wt/cs1-script`.
- **Scope:** phase A of designs/CS1-script.md (DEC-25, DEC-38): flags, `ScriptError`, the opcode table, parsing helpers, `ScriptNum`, the encoding checks, the ECDSA layer, the interpreter with DIP0020 and CHECKDATASIG, and script-level sigops. Sighash and the transaction checker (phase B) are out of scope.
- **Brief:** REVIEW-COMMON.md; DEC-42 (GPT unavailable). This is the adversarial consensus pass.
- **Verdict: APPROVE-WITH-NITS.** No blockers and no majors. I found no divergence from Dash Core v23.1.8 or v24.0.0-rc.3: none in a line-by-line read, and none in about 32 million differential cases per pin.

## 1. What I did

### 1.1 Line-by-line comparison

I diffed the Core sources at both pins first:
- `script/interpreter.cpp` differs only in copyright and `valtype()` → `emplace_back()` in `OP_SPLIT`.
- `script.{h,cpp}` and `script_error.*` differ only in copyright.
- `pubkey.{h,cpp}` adds only `Derive256` and `IsValidNonHybrid`.

So one reading covers both pins. I then compared each ported function against its Core original:

| Port | Core | Result |
|---|---|---|
| `interpreter.rs` `eval_script` (every opcode arm), `eval_checksig`, `eval_checkmultisig`, `verify_script`, `cast_to_bool`, `ConditionStack` | `interpreter.cpp:35-1336, 1652-1717` | identical semantics, error order and failure stack |
| `parse.rs` `get_op`, `is_push_only`, `is_pay_to_script_hash`, `check_minimal_push`, `push_data`, `find_and_delete` | `script.cpp:213-400`, `script.h` `operator<<`, `interpreter.cpp:224` | identical, including the failure cursor and FindAndDelete's single pass |
| `num.rs` | `script.h:211-382` `CScriptNum`, `script.cpp` `IsMinimallyEncoded`/`MinimallyEncode` | identical; the overflow asserts are unreachable at ≤ 5-byte operands, as documented |
| `encoding.rs` | `IsValidSignatureEncoding`, `IsLowDERSignature`, `IsDefinedHashtypeSignature`, `CheckSignatureEncoding`, `CheckPubKeyEncoding` | identical, including DER → LOW_S → STRICTENC order and empty-signature early accept |
| `ecdsa.rs` | `pubkey.cpp` `ecdsa_signature_parse_der_lax`, `CPubKey::Verify`, `CheckLowS`; `pubkey.h` `GetLen`/`Set`/`IsValid` | identical. Leading-zero skip, `lenbyte >= 4` rejection and both-scalars-zeroed on overflow all match. The vendored libsecp256k1 0.8.0 `eckey_pubkey_parse` keeps the hybrid 06/07 parity check |
| `sigops.rs` | `script.cpp` both `GetSigOpCount` overloads | identical (CHECKDATASIG counts 0; the data is cleared per `GetOp`) |
| `flags.rs` | `interpreter.h` flag bits, `policy.h` 0x147DF, `standard.h` 0x001/0x615, `validation.cpp:2419` `GetBlockScriptFlags` | identical |
| `error.rs` | `script_error.h` order, `script_error.cpp` strings | identical (also checked by the recorded `error_strings` test) |

Points I checked by hand:
- **The 201-op limit.** It counts every opcode above `OP_16`, executed or not, but not `OP_RESERVED`. An executed `CHECKMULTISIG` also charges its declared key count after `PubkeyCount` and before the key-stack check. A skipped one charges only its opcode.
- **The 10,000-byte script limit.** It is checked before anything else.
- **Pushes over 520 bytes.** They are rejected before the op count and the branch test, so they fail even in unexecuted branches.
- **Stack size.** The `stack + altstack > 1000` check runs after every opcode, executed or not, and never on entry.
- **Minimal data.** `MINIMALDATA` applies only to executed `0x00..=0x4e` pushes.
- **Conditionals.** `OP_VERIF`/`OP_VERNOTIF` reach `default:` inside unexecuted branches. `OP_IF` on an empty executed stack gives `UNBALANCED_CONDITIONAL`. There is no MINIMALIF in Dash.
- **Exceptions.** Every place where Core throws inside the `try` becomes `UnknownError` at the same point: `scriptnum_error`, `stack.at`, `popstack`.
- **NULLFAIL in multisig.** The `ikey2` cleanup gives NULLFAIL before NULLDUMMY.
- **NUM2BIN size.** The size is `getint()` cast to `u64`, so a negative size becomes `PushSize`.
- **Number widths.** `OP_SPLIT` uses `int64` from `getint()`. CLTV and CSV read 5-byte operands, and CSV tests the disable bit on the unmasked value.
- **CHECKDATASIG.** It verifies the single SHA-256 of the message with the whole signature (trailing byte included), and never calls the checker.

### 1.2 Unreachable census markers

`script-branch-census.sh` reports 1,082 lines reached, 88 error sites and 0 missed. I ran it on a `git archive` copy of HEAD, so the worktree was not touched. I checked each of its 8 `census: unreachable` markers against the code:

| Marker | Why it is unreachable |
|---|---|
| `interpreter.rs:68,73,75` (`top`/`top_mut`), `:81` (`pop`) | I listed every call site (about 45). Each runs after a `stack.len()` guard that covers the depth it reads. In `eval_checkmultisig`, `isig ≤ i-1` and `ikey ≤ 1+nKeys` hold for every iteration, because the loop leaves as soon as `sigs > keys`. Even if a future edit made one reachable, the port's result (`UnknownError`) would equal Core's (`.at()` throws, then `catch(...)`). |
| `interpreter.rs:954` (dummy missing) | The guard `stack.len() ≥ i = 3+nKeys+nSigs` holds, and the cleanup pops `i-1`, so at least 1 item is left. This is dead in Core too. |
| `interpreter.rs:1008` (P2SH redeem pop) | A P2SH scriptPubKey on an empty stack fails at `OP_HASH160` with `InvalidStackOperation`. This is Core's `assert(!stack.empty())`. |
| `encoding.rs:84` | `is_low_der_signature` is reached only under `LOW_S`, and `LOW_S` is in the DER-checking set, so DER has already passed. |
| `encoding.rs:98` | `check_signature_encoding` returns early on an empty signature. |

### 1.3 Independent differential

Harness: `/work/scratch/cs1a-r1-opus/diff` (Rust) and `/work/scratch/cs1a-r1-opus/mock/runner.cpp` (C++). None of it reuses the branch's generators, recorder or mock checker.

- **O1 in-process** (`dash-oracle-sys`, pinned archives, both pins):
  - `eval_script` with `BaseSignatureChecker`: ok, error code and the full final stack are compared on success and on failure.
  - `verify_script` through a one-in/one-out transaction with Core's `TransactionSignatureChecker`. CLTV and CSV flags are cleared here; no generated signature can match a real sighash.
  - The primitives: `check_signature_encoding`, `parse_der_lax` (the compact bytes are compared), `check_low_s`, `pubkey_verify`, `check_minimal_push`, `scriptnum_decode`/`encode`/`is_minimally_encoded`/`minimally_encode`, `find_and_delete`, `script_sigops`, `script_sigops_with_sig`.
- **My own Core runner.**
  - `runner.cpp` is linked against each pin's `libbitcoin_consensus/common/util/crypto/secp256k1` from the oracle-shim trees.
  - It runs `EvalScript` and `VerifyScript` with a different deterministic mock: `CheckSig = SHA256(le32 len‖sig‖le32 len‖pk‖le32 len‖scriptCode)[0] < 160`, `CheckLockTime = n%5<3`, `CheckSequence = n%7≠2`.
  - This reaches the success paths of CHECKSIG/CHECKMULTISIG, CLTV and CSV, and it pins the post-FindAndDelete, post-CODESEPARATOR scriptCode handed to the checker.
  - Negative control: moving the Rust mock's threshold to 170 gave 156 mismatches in 20,000 cases.
- **Generators.** They mix the following, with initial stacks of 0–5 items (and sometimes 990–1,001), over random flag sets:
  - raw random bytes and weighted opcode/push token streams of 1–30 tokens, or 150–260 in the long runs;
  - DIP0020 templates:
    - CAT to 515–521 bytes;
    - SPLIT at −1, 0, L/2, L, L+1, 2³¹ or non-minimal positions;
    - NUM2BIN with up-to-520-byte non-minimal and negative-zero numbers and sizes −1…521 or 2³¹;
    - BIN2NUM on zero/0x80-heavy input;
    - AND/OR/XOR on equal and unequal sizes;
    - DIV/MOD by 0, −0 and −1;
  - CHECKDATASIG built from 12 real keys × 6 messages:
    - signatures: strict, high-S, padded-R, long-form length, r = n, r of 33 bytes, r = 0, bit-flipped, truncated, trailing garbage, empty, with 12 hash-type bytes;
    - keys: compressed, uncompressed, hybrid with good or bad parity, wrong length, wrong header, random;
  - CHECKMULTISIG:
    - 170–202 leading NOPs;
    - key counts −1/0/1/2/3/5/20/21/random, with too few keys;
    - signature counts above the key count;
    - non-minimal counts;
    - empty, non-empty and missing dummies;
    - signatures re-pushed in the scriptCode, and CODESEPARATOR;
  - op-count, stack-size, script-size and push-size boundaries;
  - CLTV/CSV operands: negative, 5- and 6-byte, the disable bit, non-minimal;
  - nested and unbalanced conditionals;
  - truncated trailing pushes;
  - P2SH redeem scripts, with non-push scriptSigs and wrong hashes.
  - Flags: every defined bit with p = 0.4, bit 15 and undefined bits sometimes, plus all-zero and STANDARD.

| Run | Cases per pin | Pins | Mismatches |
|---|---|---|---|
| `eval` (O1, BaseSignatureChecker; failure stacks compared) | 19.0 M (incl. 4 M long-script) | v23.1.8, v24.0.0-rc.3 | **0** |
| `verify` (O1, TransactionSignatureChecker) | 2.0 M | both | **0** |
| `mock` (own Core runner; ⅔ eval with stacks, ⅓ VerifyScript; ≈ 650 k CheckSig calls) | 4.8 M | both | **0** |
| `prims` (O1 primitives, about 25% `pubkey_verify` true) | 6.0 M | both | **0** |
| `named` (about 100 hand-made boundaries, `named.log`) | 100 | both | **0** |

The named boundaries, all matching on both pins:
- 9,999 and 10,000 bytes ok, 10,001 `SCRIPT_SIZE`;
- 201 NOPs ok, 202 `OP_COUNT`;
- 181 NOPs plus a 20-key CHECKMULTISIG with no keys gives `OP_COUNT`, not `INVALID_STACK_OPERATION`;
- 1,001 initial items with an empty script ok, with one NOP `STACK_SIZE`;
- the altstack counts towards the limit;
- a 520-byte −0 `NUM2BIN` into 1 byte gives `00`;
- `1 −0 DIV` gives `DIV_BY_ZERO`, and under MINIMALDATA `UNKNOWN_ERROR`;
- CHECKDATASIG over {good, high-S, r=n, empty} × {compressed, hybrid, bad-parity hybrid} × 5 flag sets, including:
  - high-S under LOW_S gives `SIG_HIGH_S`;
  - r = n under LOW_S|NULLFAIL gives `SIG_NULLFAIL` (quirk 7);
  - a hybrid key under STRICTENC gives `PUBKEYTYPE`;
  - a good-parity hybrid key without STRICTENC verifies.

### 1.4 The branch's own gates

- `cargo test -p dash-consensus`: 78 tests pass across unit, quirks, recorded, ported Core units, properties and the 4,512 + 214 O4 records.
- `cargo clippy -p dash-consensus --all-features --all-targets -D warnings`: clean. The same with `dash-core-vectors`: clean. `cargo fmt --check`: clean.
- `cargo test -p dash-core-vectors` and `-p dashcore --test core_vectors` pass. The moved `SerializeScriptCode` edge-case test now lives in `test-vectors/src/script.rs`; nothing was lost.
- `contrib/node/check-pure.sh`: both node-pure crates pass.
- `record.py --check` rebuilt the recorder against both pins: `script_recorded.json` matches both.
- `secp256k1 =0.33.1` and `secp256k1-sys =0.14.1` are pinned exactly, and `Cargo.lock` holds one copy of each.

## 2. Findings

### Blockers
None.

### Majors
None.

### Minors

**m1. The phase-A API drifts from the "normative" signatures of design §4, and §10 does not record it.**
- `parse.rs:26`: `get_op` returns `Option<Op>`, where `Op` is a struct `{opcode, data}`. §4.3 specifies `Result<Op<'a>, ()>` with `enum Op { Push{..}, Code(u8) }`.
- The port also adds `instructions()`, `ScriptFlags::intersects`, `ScriptFlags::DEFINED` and `SCRIPT_ERROR_COUNT`.
- `MANDATORY_V23_1_8` is now `MANDATORY_V23_1` (`flags.rs:46`); its doc cites DEC-41.
- The Option form loses nothing: the failure cursor is still exposed through `pc`.
- **Failure scenario:** CS0/CS3/CH3a write code against §4 and break, or reviewers check against a stale contract.
- **Fix:** update §4.1/§4.3 to the shapes in the code, or add a §10 row. No code change.

### Nits

**n1. Design and code disagree on when the CLEANSTACK⇒P2SH assert fires; the code is right.**
- §4.1 says "`verify_script` likewise `assert!`s `is_valid_combination()`", which reads as an assert up front.
- `interpreter.rs:1018-1021` asserts only when evaluation reaches the clean-stack check, which is exactly Core's `interpreter.cpp:1710`. The `# Panics` doc at `:967-971` says so.
- **Fix:** align the §4.1 wording.

**n2. Phase A's only Core evidence for the success side of CHECKSIG/CHECKMULTISIG is the branch's own recorder, in two places.**
- `record.cpp` `MockChecker` and `tests/common/mod.rs` `MockChecker` share one hash construction.
- The O1 `eval_script` entry point is BaseSignatureChecker-only.
- My independent mock runner agrees on 4.8 M cases per pin, so this is not a defect.
- **Fix:** for phase B's `diff_script_verify`, consider an O1 entry point for `EvalScript`/`VerifyScript` with a parameterised mock checker. The real-transaction target alone reaches CHECKMULTISIG success only with valid signatures. `runner.cpp` here is a 60-line starting point.

**n3. The differential here covers only the SHA-256 dispatch path agentbox has.**
- OP_SHA256, OP_HASH256, OP_HASH160 and CHECKDATASIG go through `dash-sha256` (SH1). Agentbox has SSE4 and no SHA-NI, so only that dispatch path was exercised.
- This belongs to SH1's review and evidence, not CS1, but CS1's L1 runs inherit it.
- **Fix:** none for CS1. The SH1 QEMU recipe covers the SHA-NI path.

## 3. Checklist against the brief

| Hunt item | Result |
|---|---|
| Opcode semantics at every flag combination | read every arm; 19 M + 4.8 M eval cases at random flag sets, failure stacks compared; 0 mismatches |
| Minimal push / minimal-if | `check_minimal_push` identical (also O1 primitive diff); MINIMALDATA only on executed pushes; no MINIMALIF in Dash (none in the port) |
| Stack and altstack limits | identical; boundaries 999/1000/1001 and altstack counting probed |
| 201 ops, CHECKMULTISIG key charge | identical, including the order PubkeyCount → OpCount → stack check; probed |
| 10,000-byte script | identical; probed at 9,999/10,000/10,001 |
| CScriptNum overflow, −0 | identical decode/encode/minimality/MinimallyEncode; −0 under DIV/MOD/BIN2NUM/NUM2BIN/CastToBool probed |
| DIP0020 ops | identical, including in-place AND/OR/XOR and NUM2BIN, the failure stacks, SPLIT range before pop, CAT size before modify |
| CHECKDATASIG and its sigops | identical (whole-signature lax verify of SHA-256(msg), no checker; sigops count 0 in Core and port) |
| NULLFAIL / LOW_S / STRICTENC / DERSIG order | identical; quirk 7 (r ≥ n counts as low-S) confirmed against both pins |
| First-error-wins codes | compared in every differential case; 0 mismatches |
| Census "unreachable" markers | all 8 confirmed unreachable (§1.2) |
| Tests / clippy | pass / clean |

**Verdict: APPROVE-WITH-NITS.** m1 is documentation only. Nothing blocks merging phase A.

# CS1: the script system (`dash_consensus::script`)

| | |
|---|---|
| Status | **Revision 1, for the round-2 design review** (PLAN §4.2 row CS1, design step D). Every round-1 finding is dispositioned in §12. Stop point: no code until the design is approved |
| Date | 2026-10-08 (r0 07:25; r1 after the round-1 reviews) |
| Author | CS1 implementer (Opus 5.5), design phase |
| Task | Script interpreter, legacy sighash (every tx field through `primitives::canonical`), sigops, DIP0020, CHECKDATASIG, the ECDSA consensus layer. FR-CON-10, -11, -12. Quirks 4–7 and 39 |
| Binding inputs | A1 §1.3, §2.1, §2.4 (`script`), §2.25 row 19, §3.3, §3.10, §3.11, §4.3, §4.4, Appendix A/B/E; PLAN §0, §1 (M0 exit 10), §4.2 row CS1, §7.2 (CS1's line); R4 §1.4, §1.15; R4-A §2.5, §3, §4, §7, §8; `designs/CS0-node-primitives.md` (r0 of 07:18; **rev1** re-read for this revision); **DEC-25, DEC-28, DEC-29 (O1b scope), DEC-30 (CS0 rev1: fuzz runner, private seeds, Core bench patches)**; reviews `CS1-design-r1-gpt.md`, `CS1-design-r1-opus.md` |
| Code verified at | Dash Core **v23.1.8** (mainnet pin), **v24.0.0-rc.3** (testnet pin), `develop` 15db478750; O1 shim `node/oracle-shim-v24.0` 684cb425e2 and `node/oracle-shim-v23.1` 6d5c9ac2ed, `dash-oracle-sys` on `node/o1-oracle-sys` 89f33820; O4 **merged** on `node/integration` efe889e0 (`test-vectors/` = crate `dash-core-vectors`; `dash/tests/core_vectors.rs`); base-sdk 33fcc59; `secp256k1` 0.33.1 / `secp256k1-sys` 0.14.1 (vendored libsecp256k1 0.8.0, rev 6e2c8bc4); B1 `node/bench-harness` 288c2a78 |
| r1 evidence | `/work/scratch/cs1/`: `fad_probe.cpp` + logs (FindAndDelete against Core, both pins), `c3_share.py` + `c3_share.log` (C3's saving on chain samples), `census.py` + `census_testnet.log` (full testnet DIP0020/CHECKDATASIG/CODESEPARATOR census) + `census_control.{py,log}` (its positive control). The reviewers' probes are in `/work/scratch/cs1-review-{gpt,opus}/` |
| Worktree | `~/workspace/rdc-wt/cs1-script`, branch `node/cs1-script` |

Core line anchors below are `v24.0.0-rc.3`. `script/interpreter.cpp` and `script/script.cpp` have the same line numbers in v23.1.8. In `pubkey.cpp`, `CheckLowS` is at 320 in v23.1.8 and 348 in v24.

---

## 0. Summary

1. **What CS1 builds.** A line-by-line port of Core's script system into `dash_consensus::script`:
   - `EvalScript` and `VerifyScript`, with the DIP0020 opcodes and `OP_CHECKDATASIG(VERIFY)`;
   - `CScriptNum`, the opcode table and Core's script parsing helpers;
   - the signature and pubkey encoding checks;
   - the ECDSA consensus layer (`CPubKey::Verify`: lax DER, then normalize-S);
   - the legacy `SignatureHash`, the transaction signature checker and sigop counting (both `GetSigOpCount` overloads and the three transaction-level counts; DEC-25(1));
   - the pure part of `CheckInputScripts`, which re-runs a failing input without the non-mandatory flags to classify it.
2. **Pins.** The script layer is **semantically identical** in v23.1.8, v24.0.0-rc.3 and `develop` (§2). The one release difference in CS1's scope is `MANDATORY_SCRIPT_VERIFY_FLAGS` (A1 §2.25 row 19). It changes the *classification string* of a failing input in the mempool and in `-par=1` block validation. CS1 takes the mask as a parameter, and `ReleaseProfile` supplies it.
3. **Boundary with CS0.** The sighash input is base-sdk's `dash_primitives::Transaction`, which CS0's `Tx::raw()` returns. CS1 depends on no `Tx` wrapper.
   - **PLAN §7 handling:** CS1 encodes every tx field through `primitives::canonical`. Its own code writes only bytes that are not tx fields: the counts, the scriptCode, the hash type, and the constant null output, which it also writes through `canonical`.
   - CS0 adds two field writers for this, `write_prevout` and `write_sequence` (DEC-25(2); §4.8).
4. **Sigcache hook and precompute slots.**
   - `SigVerifier` is Core's virtual `GenericTransactionSignatureChecker::VerifySignature`. CS6 implements the caching verifier. `OP_CHECKDATASIG` bypasses the checker and the cache, as Core's does.
   - `TxSigData` is Core's `PrecomputedTransactionData`: built once per transaction and shared by its inputs. It is inert in phase B, as Core's is (Dash's `SignatureHash` ignores its `cache` argument), and holds C3's midstates in phase C (DEC-28(b)).
   - The checker is a per-input object. C2's sighash memo lives inside it, so C2 changes no signature (§4.9).
5. **Errors.** `ScriptError` carries Core's numeric codes and `ScriptErrorString` text. Input failures become CS0's `TxValidationError` with Core's two formatted reasons and classes.
6. **Performance** (§8; DEC-28).
   - Every per-input check is a pure, `Send + Sync` unit for CS6's check queue.
   - Per-signature ECDSA cost is at parity: +1.7% instructions, from the same library in different versions (§2).
   - On agentbox, which has no SHA-NI, the legacy sighash is about a third of Core's per-input instructions. It is hashed at about 51.7 instructions/byte by `bitcoin_hashes` against Core's 36.8.
   - Three measures, all required before CS1's bench gate:
     - **SH1:** a Core-class SHA-256 backend behind CS0's hash module (a prerequisite owned by SH1);
     - **C3:** per-tx midstate checkpoints, which remove **46–47%** of the hashed sighash bytes on mainnet samples (measured, §8.2);
     - **C2:** a per-input sighash memo.
   - Expected result: about **−15%** instructions per input against dashd. The ceiling at equal threads is about **1.49×**, so 2× per input is a declared shortfall (DEC-28(d)).
7. **Evidence.**
   - **L0:** `script_tests.json` (1,282), `sighash.json` (500), the script parts of `tx_valid`/`tx_invalid` (83 + 66), 4,512 DIP0020 and 214 CHECKDATASIG `EvalScript` records, all through O4's merged `dash-core-vectors` loaders; oracle-generated special-tx sighash vectors.
   - **Ported Core unit tests**, and Core-checked regressions for every behaviour the round-1 reviews probed (§7.2).
   - **L1:** four differential targets, run three ways (Rust = Core v23.1.8 = Core v24): `diff_script_verify`, `diff_sighash`, `diff_script_primitives` and `diff_sigops`. Classification is compared per pin, since row 19 makes the pins differ by design (§6.2, §7.4).
   - **Real chain:** a script sample from the BS2 oracles. A full testnet census (515,730 blocks since DIP0020) found **no** DIP0020, CHECKDATASIG or CODESEPARATOR use, so L0/L1 carry that evidence (§7.6).
   - **Quirks:** 4–7 and 39 (registered by DEC-25(4); §5.2).
8. **Phasing.**
   - **A:** no dependency beyond CS0's skeleton.
   - **B:** correctness. Needs CS0's `canonical` and decoder, and O1 plus the O1b entry points.
   - **C:** performance. C2 and C3 are **required** (DEC-28(b)); C1 is measured. The bench gate also needs SH1.
9. **Decisions.** DEC-25 settled r0's D1–D5 and DEC-28 the performance plan. r1 asks for a few follow-ups (§11): CS0's `HashWriter: Clone`, the revised O1b list, PLAN-row and A1 edits, and the designated reader's confirmation of the PLAN §7 split.

---

## 1. Scope

**In scope (`dash_consensus::script`):**
- the flags, `ScriptError`, the opcode table, script parsing (`GetOp`, push-only, P2SH, minimal push, `FindAndDelete`), `CScriptNum`;
- `eval_script`, `verify_script`, `cast_to_bool`;
- the signature and pubkey encoding checks, and the ECDSA layer (`parse_der_lax`, `verify_lax`, `check_low_s`, pubkey validity);
- `signature_hash` (legacy, with the payload) and the transaction signature checker with its `SigVerifier` hook;
- `TxSigData` (Core's `PrecomputedTransactionData`) and the two required sighash optimisations C2 and C3, each with its reference twin (§8.3);
- sigop counting at script and transaction level (`GetSigOpCount` both forms, `GetLegacySigOpCount`, `GetP2SHSigOpCount`, `GetTransactionSigOpCount`; DEC-25(1));
- the pure part of `CheckInputScripts`: run one input; classify a failure; serial check without caches;
- `block_script_flags` (`GetBlockScriptFlags`) over explicit booleans;
- the L0 runners, the L1 targets, oracle-generated vectors, the real-chain sample and census, quirk tests 4–7 and 39, criterion benches, and the Core-side bench patch they are compared with (DEC-28(c); §8.4).

**Not in scope:**
- **SH1:** the SHA-256 backend (DEC-28(a)). CS1 consumes it through CS0's hash module and changes nothing when it lands.
- **CS6:** the signature cache, the script-execution cache and the check queue, including the queue's batching (§8.1 hands CS6 GPT F6's requirements). CS1 provides the hook, the unit of work and `TxSigData`.
- **CS3:** sigop *limits* (`bad-blk-sigops`, the ConnectBlock running sum) and the `legacy_sigops` slot on `Tx`.
- **MP1a:** the mempool's policy-then-consensus two-pass.
- **CH3a:** wiring `block_script_flags` to `DeploymentView`, assumevalid, and the `CheckInputScripts on … failed with …` log line.
- **CS0:** transaction decoding and encoding.
- **EV1/SP0:** non-script ECDSA uses (`RecoverCompact` for ProTx, spork and governance signatures). The `ecdsa` module is their natural home, but they are not CS1's.
- Wallet signing keeps `dashcore::sighash` (A1 §1.1). Tests cross-check it with the same vectors where cheap.

---

## 2. Pin audit (DEC-05; input to O17)

`git diff v23.1.8 v24.0.0-rc.3` over CS1's Core sources:

| File | Delta | Effect on CS1 |
|---|---|---|
| `script/interpreter.{h,cpp}` | copyright lines, include reshuffle, `valtype()` → `emplace_back()` in `OP_SPLIT` | none |
| `script/script.{h,cpp}`, `script/script_error.{h,cpp}` | copyright only | none |
| `pubkey.{h,cpp}` | adds `Derive256` (DIP-14) and `IsValidNonHybrid`, neither used by script | none (`CheckLowS` moves 320 → 348) |
| `consensus/tx_verify.cpp`, `core_read.cpp` (`ParseScript`) | none / RPC helpers only | none |
| `validation.cpp` `GetBlockScriptFlags` | identical | none |
| `validation.cpp` `CheckInputScripts`, `CScriptCheck` | cache-init API only (`InitScriptExecutionCache(size_t)`) | none (CS6 owns the init) |
| `script/sigcache.*` | `-maxsigcachesize` sizing semantics | CS6 (config parity) |
| `script/standard.h` | **`MANDATORY_SCRIPT_VERIFY_FLAGS`: `P2SH` (0x1) → `P2SH\|DERSIG\|NULLDUMMY\|CLTV\|CSV` (0x615)** | **row 19**, §6.2 |
| `policy/policy.h` | `STANDARD_SCRIPT_VERIFY_FLAGS` re-expressed; value 0x147DF in both | none |
| `src/test/data/{script_tests,sighash,tx_valid,tx_invalid}.json` | identical | one L0 set serves both pins |
| `src/secp256k1` subtree | subtree bump within libsecp256k1 **0.3.3-dev** (both pins' `configure.ac`); `ecdsa_impl.h` comment-only | semantics of parse, normalize and verify unchanged; cross-checked by `diff_script_primitives` on both pins |
| `crypto/sha256*.cpp`, `hash.{h,cpp}` | copyright lines and a config include | none. One Core-side bench baseline serves both pins (§8.4) |

`develop` (15db478750) is identical to v24.0.0-rc.3 in all of these, including `CheckInputScripts`, `GetBlockScriptFlags` and the four JSON files.

**Our side of the same question (Opus m-6; O17 class (c)).** The Rust code links a different libsecp256k1 from both Core pins:

| | Core v23.1.8 / v24.0.0-rc.3 | rdashd |
|---|---|---|
| libsecp256k1 | 0.3.3-dev, with x86_64 field and scalar asm | **0.8.0**, vendored by `secp256k1-sys` 0.14.1 (rev `6e2c8bc4`), built by `cc` with no asm (0.8.0 has none) |
| Used for | `secp256k1_ec_pubkey_parse` (hybrid 06/07 with the parity check), `ecdsa_signature_normalize`, `ecdsa_verify`; the lax DER parser is Core's own `pubkey.cpp` copy | the same three C functions; the lax DER parser is CS1's safe-Rust port of Core's copy (§4.7) |

- Nothing in CS1's scope depends on the difference. Parse, normalize and verify are deterministic functions of their inputs in both versions, and `diff_script_primitives` mode (b) checks them against both pins.
- Nothing pinned it either. The workspace asks for `secp256k1 = "0.33.1"` (a caret requirement), so a routine `cargo update` could move `secp256k1-sys` within 0.14.x to a newer vendored library.
- **Fix:** `dash-consensus` declares `secp256k1 = "=0.33.1"` and `secp256k1-sys = "=0.14.1"` exactly (§3).
- Moving either is a CON change that must re-run `quirk_07_*` and `diff_script_primitives` mode (b) at the full new-target budget (≥ 24 CPU-h per pin), like a new target.

**Row 19's effect is observable.** It changes Core's own functional tests:
- `feature_dersig.py:122,129` and `feature_cltv.py:160-162` assert `non-mandatory-script-verify-flag (…)` on v23.1.8 and `mandatory-script-verify-flag-failed (…)` on v24;
- this holds both for `sendrawtransaction` and for the `-par=1` block log line, because `ConnectBlock` wraps the tx reason in `BLOCK_CONSENSUS` (§6.2).

---

## 3. Module layout and dependencies

```
dash-consensus/src/script/
  mod.rs          re-exports; module docs (Core file map)
  flags.rs        ScriptFlags; STANDARD; MANDATORY_* ; block_script_flags
  error.rs        ScriptError (Core codes, names, strings); script_error_code
  opcodes.rs      OP_* constants (Core's opcodetype), op_name (GetOpName)
  parse.rs        get_op (GetScriptOp, Core cursor semantics), Instructions, is_push_only,
                  is_pay_to_script_hash, check_minimal_push, push_data (CScript << vector), find_and_delete
  num.rs          ScriptNum (CScriptNum), is_minimally_encoded, minimally_encode
  encoding.rs     is_valid_signature_encoding, check_signature_encoding, check_pubkey_encoding
  ecdsa.rs        PubKeyRef (CPubKey::GetLen/IsValid), parse_der_lax, verify_lax, check_low_s
  interpreter.rs  Stack, cast_to_bool, eval_script, verify_script (+ ConditionStack, EvalChecksig)
  sighash.rs      SIGHASH_*, signature_hash (plain: the reference twin), serialize_script_code,
                  TxSigData, signature_hash_with (C3 path in phase C)
  checker.rs      SignatureChecker, NoSignatureChecker, SigVerifier, DirectVerifier,
                  TransactionSignatureChecker (+ its private C2 memo in phase C)
  sigops.rs       script_sigop_count, script_sigop_count_with_sig, legacy_/p2sh_/transaction_sigop_count
  input.rs        InputCheck (CScriptCheck), ScriptPolicy, InputScriptFailure, check_input_scripts
dash-consensus/tests/   script_vectors.rs (L0), script_core_units.rs (ported Core unit tests, §7.2),
                        script_oracle.rs (proptest diff, feature `oracle`), quirks
dash-consensus/benches/ script.rs; fixtures/ (block + spent-output fixtures shared with the Core-side benches)
node-fuzz/fuzz_targets/ diff_script_verify.rs, diff_sighash.rs, diff_script_primitives.rs, diff_sigops.rs
```

**Dependencies.**
- `dash-consensus` (CS0's crate, its manifest and lints);
- `secp256k1 = "=0.33.1"` (the version `dash-pkc` uses) and `secp256k1-sys = "=0.14.1"`, default features. Both are **exact** requirements, so the vendored libsecp256k1 cannot move silently (§2, Opus m-6). Cargo unifies semver-compatible versions, so this pins the whole workspace's copy, which is what we want;
- hashes through CS0's single hash module (`hash.rs`), backed by SH1's SHA-256 (DEC-28(a)). CS1 needs:
  - `sha256` (one-shot, for `OP_SHA256` and CHECKDATASIG) and `sha256d`;
  - `HashWriter`, the streaming SHA256d sink, which must be **`Clone`** so that C3 can fork a midstate (§11 N1);
  - `sha1`, `ripemd160` and `hash160`, which stay on `bitcoin_hashes` (Core's are plain C++ too).
- `dash-core-vectors` (O4's loader crate, a leaf) as a dev-dependency.

**No base-sdk script dependency.** `dash-script`'s opcode table and its `legacy_sigop_count` (which has no `fAccurate`) are not used. A base-sdk change therefore cannot move script semantics. A dev-only test asserts that our opcode values and names equal `dash_script::opcode::Opcode` wherever both define one, so drift is caught in either direction.

**Crate rules (A1 §2.1, App. E).**
- `#![forbid(unsafe_code)]`: the lax DER parser is a safe-Rust port.
- MIT licence. The lax parser's header also keeps libsecp256k1's MIT notice.
- No `HashMap`, no floating point, no clocks, no threads, no globals of our own.
- No interior mutability, except C2's memo: a `RefCell` inside `TransactionSignatureChecker`, a per-input object that is never shared between threads (§4.9).

---

## 4. API

Signatures are normative. Bodies are line-by-line ports with `// Core: <file>:<line> <Function>` anchors. Phase A's implementation changed a few of them; the signatures below are the implemented ones, and §10 #12–#14 record what changed and why (CS1a review m1).

### 4.1 Flags (`flags.rs`)

```rust
/// Core's `unsigned int flags`. All 32 bits are retained and undefined bits are ignored, as in Core.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct ScriptFlags(u32);
impl ScriptFlags {
    pub const NONE: Self;              pub const P2SH: Self;               // 1 << 0
    pub const STRICTENC: Self;         pub const DERSIG: Self;             // 1 << 1, 1 << 2
    pub const LOW_S: Self;             pub const NULLDUMMY: Self;          // 1 << 3, 1 << 4
    pub const SIGPUSHONLY: Self;       pub const MINIMALDATA: Self;        // 1 << 5, 1 << 6
    pub const DISCOURAGE_UPGRADABLE_NOPS: Self; pub const CLEANSTACK: Self; // 1 << 7, 1 << 8
    pub const CHECKLOCKTIMEVERIFY: Self; pub const CHECKSEQUENCEVERIFY: Self; // 1 << 9, 1 << 10
    pub const NULLFAIL: Self;          pub const CONST_SCRIPTCODE: Self;   // 1 << 14, 1 << 16
    // Bit 15 is the retired SCRIPT_ENABLE_DIP0020_OPCODES: no constant, ignored like any undefined bit.

    /// policy/policy.h STANDARD_SCRIPT_VERIFY_FLAGS = 0x147DF (both pins).
    pub const STANDARD: Self;
    /// script/standard.h MANDATORY_SCRIPT_VERIFY_FLAGS of each release line (A1 §2.25 row 19).
    /// Named for release lines, not tags (DEC-41). These are the values only: callers read the
    /// running release's mask from CS0's `UngatedRules.mandatory_script_flags` (§9, phase-B notes).
    pub const MANDATORY_V23_1: Self;     // 0x001
    pub const MANDATORY_V24_0: Self;     // 0x615
    /// Every bit Core defines (the 13 above): 0x147FF.
    pub const DEFINED: Self;

    pub const fn from_bits(bits: u32) -> Self;  pub const fn bits(self) -> u32;
    pub const fn contains(self, f: Self) -> bool; pub const fn intersects(self, f: Self) -> bool;
    pub const fn union(self, f: Self) -> Self;  pub const fn difference(self, f: Self) -> Self;
    /// test/util/script.cpp IsValidFlagCombination: CLEANSTACK requires P2SH.
    pub const fn is_valid_combination(self) -> bool;
}

/// The booleans GetBlockScriptFlags reads: DeploymentActiveAt(*pindex, …) for each buried rule.
pub struct BuriedScriptRules { pub dersig: bool, pub cltv: bool, pub csv: bool, pub bip147: bool }
/// Core: validation.cpp:2419 GetBlockScriptFlags. P2SH is always set (no exception block on any network).
pub fn block_script_flags(active: BuriedScriptRules) -> ScriptFlags;
```

**Flag rules:**
- **Consensus flags** are only P2SH, DERSIG, CLTV, CSV and NULLDUMMY. Everything else is policy.
- **There is no `SigVersion`.** Dash has only `BASE`, so every `sigversion == BASE` test in Core is true here: `FindAndDelete` always runs, and the `CONST_SCRIPTCODE` checks are always live.
- **DIP0020 has no gate.** Its opcodes are enabled at every height (`61bc3002b0`), and bit 15 is ignored. A property test re-runs every L0 vector with bit 15 toggled.
- **CLEANSTACK without P2SH** is an assert in Core (`interpreter.cpp:1710`), so the process aborts. It fires only when evaluation reaches the clean-stack check, after the scriptSig, the scriptPubKey and any P2SH redeem script have run. `verify_script` asserts at the same point, not on entry (CS1a review n1), and documents it under `# Panics`. Flags never come from untrusted input: they are always constants or the block flags, and both always include P2SH. The harnesses never generate such a combination, and the O1 shim refuses it with `ORACLE_E_ARG`. Phase B also rejects the combination where its flags are built (§9, phase-B notes).

### 4.2 Errors (`error.rs`)

```rust
/// Core's ScriptError_t without SCRIPT_ERR_OK (success is `Ok(())`). Discriminants are Core's values.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ScriptError {
    UnknownError = 1, EvalFalse, OpReturn, ScriptSize, PushSize, OpCount, StackSize, SigCount,
    PubkeyCount, Verify, EqualVerify, CheckMultisigVerify, CheckSigVerify, CheckDataSigVerify,
    NumEqualVerify, BadOpcode, DisabledOpcode, InvalidStackOperation, InvalidAltstackOperation,
    UnbalancedConditional, InvalidSplitRange, InvalidOperandSize, InvalidNumberRange,
    ImpossibleEncoding, DivByZero, ModByZero, NegativeLocktime, UnsatisfiedLocktime, SigHashtype,
    SigDer, MinimalData, SigPushOnly, SigHighS, SigNullDummy, PubkeyType, CleanStack, SigNullFail,
    DiscourageUpgradableNops, OpCodeSeparator, SigFindAndDelete,   // = 40
}
impl ScriptError {
    pub const ALL: [ScriptError; 40];                // in Core's order
    pub const fn code(self) -> i32;                  // Core's numeric value (1..=40)
    pub fn from_code(code: i32) -> Option<Self>;
    pub const fn name(self) -> &'static str;         // script_tests.cpp names, + "IMPOSSIBLE_ENCODING" (§7.1)
    pub const fn message(self) -> &'static str;      // script_error.cpp ScriptErrorString, verbatim
}
impl fmt::Display for ScriptError { /* message() */ }
impl std::error::Error for ScriptError {}
/// Core's SCRIPT_ERR_ERROR_COUNT (41).
pub const SCRIPT_ERROR_COUNT: i32;
/// 0 for Ok, else code(): the integer O1's oracle returns.
pub fn script_error_code(r: &Result<(), ScriptError>) -> i32;
/// ScriptErrorString for any integer: "No error" for 0, "unknown error" outside 1..=40.
pub fn script_error_string(code: i32) -> &'static str;
```

- Codes run in Core's order: `OK` = 0, `UNKNOWN_ERROR` = 1, …, `SIG_FINDANDDELETE` = 40, `ERROR_COUNT` = 41.
- A test checks `Display` against `oracle_script_error_string(code)` on both pins for every code 0..=41: 0 is "No error", and 1 and 41 are "unknown error".

### 4.3 Script parsing (`parse.rs`)

```rust
/// One instruction, as Core's GetScriptOp returns it: the opcode and the pushed bytes (empty for
/// a non-push opcode and for OP_0, OP_1NEGATE, OP_1..OP_16 and OP_RESERVED).
pub struct Op<'a> { pub opcode: u8, pub data: &'a [u8] }
/// Core: script.cpp:259 GetScriptOp. Success advances `*pc` past the whole instruction. On failure
/// (`None`) `*pc` is left exactly where Core's iterator is left (rule below), which SerializeScriptCode observes (§5.2).
pub fn get_op<'a>(script: &'a [u8], pc: &mut usize) -> Option<Op<'a>>;
/// The instructions in order, stopping for good at the end or the first failure (fused: Core's
/// failure cursor can sit inside the failed push; CS1a GPT n1).
pub fn instructions(script: &[u8]) -> impl Iterator<Item = Op<'_>>;
pub fn is_push_only(script: &[u8]) -> bool;            // script.cpp:237; OP_RESERVED counts as push
pub fn is_pay_to_script_hash(script: &[u8]) -> bool;   // exactly a9 14 <20> 87
pub fn check_minimal_push(data: &[u8], opcode: u8) -> bool;   // script.cpp:386
pub fn push_data(script: &mut Vec<u8>, data: &[u8]);   // CScript << vector: never OP_N; empty → 0x00
/// Core: interpreter.cpp:224 FindAndDelete: one cursor pass, algorithm below. Returns the count;
/// rewrites the script only if the count is > 0. An empty pattern returns 0 and rewrites nothing.
pub fn find_and_delete(script: &mut Vec<u8>, pattern: &[u8]) -> usize;
```

**`get_op`'s failure cursor** (`script.cpp:259-301`; Opus m-1). Let `p` be the position of the opcode byte.
- At the end of the script: fail, `*pc = p`.
- Otherwise the opcode byte is always consumed (`*pc = p + 1`). Then:
  - **Direct push `0x01..=0x4b` with short data:** fail with `*pc = p + 1`.
  - **`PUSHDATA1/2/4` (`4c`/`4d`/`4e`) whose 1/2/4-byte length field is incomplete:** fail with `*pc = p + 1`. Core tests `end - pc < width` **before** reading any of it, so a partial length field is never consumed.
  - **Length field complete, data short:** fail with `*pc = p + 1 + width`.

So for `4e 01 02` (PUSHDATA4 with 2 of its 4 length bytes) the cursor stops after `4e`, not after `02`. O4's merged `core_get_op` (`dash/tests/core_vectors.rs:32`) states the same rule. CS1 moves it into `dash_core_vectors::script` as `reference_get_op`, so that both crates test against one independent copy (§7.2).

**`find_and_delete`'s algorithm** (`interpreter.cpp:224-247`; GPT M1, Opus m-2). Core makes one pass with two cursors, `pc` and `pc2` (the start of the bytes not yet copied):

```
if pattern is empty: return 0
loop:
    copy [pc2, pc) to the result
    while the remaining bytes at pc start with pattern: pc += |pattern|; count += 1   // adjacent matches
    pc2 = pc
    if !get_op(script, &mut pc): break        // also stops at the end; pc2 is NOT moved on failure
if count > 0: copy [pc2, end) to the result and replace the script
```

What follows from it:
- **Matches are tested only at the cursor.** That is an original instruction boundary until the first match. After a match, `pc` has moved by `|pattern|`, which need not be a boundary of the original parse. With script `01 01` and pattern `01`, both bytes are deleted (count 2), although the second byte was push data. Core's own `script_FindAndDelete` "odd edge case" has the same shape: `03 02ff03 03 02ff03` minus `03` gives `02ff03 02ff03`, count 2.
- **A parse failure ends the pass.** The tail from the last `pc2` is copied verbatim. Unlike `SerializeScriptCode`, nothing is dropped.
- **It is not idempotent in general.** A deletion can join bytes into a new match that the pass has already gone by. Core gives `51 51 52 52` minus `51 52` → `51 52` (count 1); a second call → empty (count 1). This was probed on both pins. The port must reproduce the single pass, never "repeat until stable".
- **The narrower property CS1 keeps is true.** If the pattern parses with `get_op` as **exactly one complete instruction**, which every consensus call's pattern is (`push_data(sig)`), then a second call on the output finds nothing.
  - *Proof sketch.*
    - Every kept segment is one whole instruction that `get_op` consumed, or the failing tail. So the output re-parses into the same kept instructions, in order, and the second pass tests only their starts.
    - Suppose the pattern matched at one of those starts. The instruction there has the same opcode and length bytes as the pattern, and those bytes fix its extent. So the instruction *is* the pattern, and the match uses only that instruction's own bytes, which the first pass saw unchanged. (A failing tail cannot match either: a complete pattern fitting inside it would make it complete.)
    - The first pass tested that same start (its match loop runs right before each `get_op`), saw the same bytes, and would have deleted them. Contradiction.
  - *Probe.* `/work/scratch/cs1/fad_probe.cpp` runs Core's own `FindAndDelete` on both pins: 1,032,264 random single-instruction patterns gave **0** second-pass hits, and 967,736 other patterns gave 388.

### 4.4 `ScriptNum` (`num.rs`)

```rust
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ScriptNum(i64);
pub const DEFAULT_MAX_NUM_SIZE: usize = 4;
pub struct ScriptNumError;   // Core's scriptnum_error: maps to ScriptError::UnknownError in eval
impl ScriptNum {
    pub const fn new(v: i64) -> Self;
    /// script.h:230: length > max_size, or non-minimal with require_minimal, is an error.
    pub fn decode(vch: &[u8], require_minimal: bool, max_size: usize) -> Result<Self, ScriptNumError>;
    pub fn encode(self) -> Vec<u8>;          // CScriptNum::serialize (sign-magnitude LE)
    pub const fn value(self) -> i64;
    pub const fn getint(self) -> i32;        // clamps to i32, as Core
    pub fn is_minimally_encoded(vch: &[u8], max_size: usize) -> bool;  // script.cpp:311
    pub fn minimally_encode(data: &mut Vec<u8>) -> bool;              // script.cpp:339, in place
}
// Add, Sub, Neg, Div, Rem, BitAnd and the comparisons with ScriptNum and i64. Core's overflow asserts
// are unreachable (operands are ≤ 4 or 5 bytes) and become debug_assert!. `/` and `%` are Rust's
// i64 ops, which truncate toward zero exactly as C++ does.
```

### 4.5 Interpreter (`interpreter.rs`)

```rust
pub type Stack = Vec<Vec<u8>>;                           // std::vector<valtype>
pub fn cast_to_bool(v: &[u8]) -> bool;                   // interpreter.cpp:35 (negative zero is false)
/// Core: interpreter.cpp:346 EvalScript(stack, script, flags, checker, SigVersion::BASE).
/// `stack` is in/out, as in Core. On error it holds exactly Core's stack at the failing op.
pub fn eval_script<C: SignatureChecker + ?Sized>(
    stack: &mut Stack, script: &[u8], flags: ScriptFlags, checker: &C,
) -> Result<(), ScriptError>;
/// Core: interpreter.cpp:1652 VerifyScript.
pub fn verify_script<C: SignatureChecker + ?Sized>(
    script_sig: &[u8], script_pubkey: &[u8], flags: ScriptFlags, checker: &C,
) -> Result<(), ScriptError>;
```

**Control flow (each item is a test):**
- **Ordering inside `eval_script`** is R4-A §3.2's, step for step:
  1. `script.len() > 10000` → `ScriptSize`.
  2. Per opcode, whether or not it executes:
     - a parse failure → `BadOpcode`;
     - a push over 520 bytes → `PushSize`;
     - `op > OP_16` increments `nOpCount`, and more than 201 → `OpCount` (`OP_RESERVED` is not counted);
     - the six disabled opcodes → `DisabledOpcode`;
     - `CONST_SCRIPTCODE` with `OP_CODESEPARATOR` → `OpCodeSeparator`.
  3. Executed pushes get the `MINIMALDATA` check and are pushed.
  4. The opcode switch runs if the opcode executes or is in `0x63..=0x68`. So `OP_VERIF` and `OP_VERNOTIF` reach `default:` → `BadOpcode` even in unexecuted branches.
  5. After every opcode, `stack.len() + altstack.len() > 1000` → `StackSize`. There is **no** check on entry: an empty script with 1,001 initial items succeeds (probed on both pins).
  6. At the end, an open conditional → `UnbalancedConditional`.
- **Per-evaluation state.** `nOpCount`, the altstack and the condition stack are locals of each `eval_script` call. Each restarts at zero or empty for the scriptSig, the scriptPubKey and the redeem script; only the main stack carries over.
- **Two places charge the 201-op limit**, both normative:
  - every opcode above `OP_16`, executed or not (step 2);
  - **an executed `CHECKMULTISIG(VERIFY)` charges its declared key count** (`interpreter.cpp:1103-1105`; GPT M3, Opus m-5). The charge applies even when no signature is checked (0-of-20). A skipped `CHECKMULTISIG` charges only its own opcode.
- **Exceptions.** Core's `catch (...)` → `SCRIPT_ERR_UNKNOWN_ERROR` becomes `?` on the fallible operations, in Core's evaluation order, all mapped to `UnknownError`:
  - `ScriptNum::decode` (overflow or non-minimal);
  - `stacktop()` out of range (`.at()`);
  - `popstack` on an empty stack.

  Order matters where a scriptnum error competes with a later check. Examples: `CHECKMULTISIG`'s key count is parsed before `PubkeyCount` is checked, `OP_SPLIT`'s position before `InvalidSplitRange`, and `OP_NUM2BIN`'s size before `PushSize`.
- **`ConditionStack`** is Core's O(1) form (`interpreter.cpp:268`). `OP_ELSE` may repeat. There is no MINIMALIF.
- **Number widths.** Numeric operands are 4-byte `ScriptNum`s; CLTV and CSV operands are 5-byte. `PICK`/`ROLL`, `CHECKMULTISIG` counts, the `OP_SPLIT` position and the `OP_NUM2BIN` size go through `getint()`. The size is then cast to `u64`, so a negative size becomes huge and gives `PushSize`.
- **DIP0020 semantics** (R4-A §3.4):
  - `OP_CAT`: `|a|+|b| > 520` → `PushSize`; appends in place.
  - `OP_SPLIT`: the range check comes first.
  - `OP_AND`/`OR`/`XOR`: the operands must be the same size, and the result is written in place into the second element.
  - `OP_DIV`/`OP_MOD`: dividing by zero gives `DivByZero`/`ModByZero`.
  - `OP_NUM2BIN`: `minimally_encode` runs in place on the number operand, which has no width limit and no minimality check. Then `ImpossibleEncoding`, or padding with the sign bit moved to the end.
  - `OP_BIN2NUM`: `minimally_encode`, then `is_minimally_encoded(_, 4)` or `InvalidNumberRange`.
- **`OP_CHECKDATASIG(VERIFY)`** (`interpreter.cpp:1048`), in Core's order:
  1. `check_signature_encoding`, then `check_pubkey_encoding`.
  2. If the signature is non-empty, `PubKeyRef::new(pubkey).map_or(false, |pk| verify_lax(pk, &sha256(msg), sig))` over the **whole** signature, trailing byte included. An invalid-length key is simply `false`, as `CPubKey::Verify`'s `IsValid` test. This calls the ECDSA layer directly: **no checker, no `SigVerifier`, no sigcache**.
  3. `NULLFAIL`.
  4. Pop 3 and push the result. The `VERIFY` variant fails with `CheckDataSigVerify`.
- **`CHECKSIG`/`CHECKMULTISIG`** port `EvalChecksig` (`:321`) and `:1091` exactly:
  - The scriptCode is `[pbegincodehash, end)`.
  - `find_and_delete` removes `push_data(sig)` for every signature first, even an empty one (pattern `0x00`, which removes every `OP_0`). The deletions run in signature order on one scriptCode, and each one sees the previous one's output. Under `CONST_SCRIPTCODE` a hit is `SigFindAndDelete`.
  - Encoding checks run only for the (sig, key) pairs the loop actually reaches.
  - The `ikey2` bookkeeping drives `NULLFAIL`, and `NULLDUMMY` is checked on the dummy element.
- **`CHECKMULTISIG`'s order** (`interpreter.cpp:1091-1198`, both pins), each step a named test:
  1. Fewer than 1 item → `InvalidStackOperation`.
  2. Decode the key count (a scriptnum error → `UnknownError`); outside `0..=20` → `PubkeyCount`.
  3. `nOpCount += nKeysCount`; above 201 → `OpCount`.
  4. Too few items for the keys → `InvalidStackOperation`.
  5. Decode the signature count; outside `0..=nKeysCount` → `SigCount`.
  6. Too few items for the signatures → `InvalidStackOperation`.
  7. The `find_and_delete` loop over the signatures.
  8. The verification loop, from the top signature and key downwards, with encoding checks for reached pairs and the early exit when more signatures remain than keys.
  9. Cleanup: pop the arguments; `NULLFAIL` on a failed operation with a non-empty signature, checked during the pops.
  10. The dummy: missing → `InvalidStackOperation`, then `NULLDUMMY`.
  11. Push the result; the `VERIFY` form fails with `CheckMultisigVerify`.

  Step 3 precedes step 4: a script with 181 `NOP`s and a declared key count of 20 but no keys fails with `OpCount`, not `InvalidStackOperation`. Step 9 precedes step 10: a non-empty unused signature with a failed operation and a non-empty dummy gives `SigNullFail` and leaves `[01][7f]` (GPT probe). The failure stack is compared in both cases.
- **`verify_script`**:
  1. `SIGPUSHONLY`.
  2. Evaluate the scriptSig, and copy the stack when P2SH is set.
  3. Evaluate the scriptPubKey; an empty or false top → `EvalFalse`.
  4. P2SH: the scriptSig must be push-only (`SigPushOnly`). Restore the stack, pop the redeem script and evaluate it, again with `EvalFalse`.
  5. `CLEANSTACK`.

  Core's `assert(!stack.empty())` in the P2SH branch is unreachable. If it were ever reached, the port returns `UnknownError`: no panic on input-derived state.

### 4.6 Encoding checks (`encoding.rs`)

```rust
pub fn is_valid_signature_encoding(sig: &[u8]) -> bool;          // :107 BIP66 strict DER + hashtype byte
/// Core: interpreter.cpp:200 CheckSignatureEncoding. Empty is always OK. DER if any of
/// DERSIG|LOW_S|STRICTENC; then LOW_S (check_low_s over sig[..len-1]); then STRICTENC hashtype.
pub fn check_signature_encoding(sig: &[u8], flags: ScriptFlags) -> Result<(), ScriptError>;
/// Core: :217 CheckPubKeyEncoding. With STRICTENC only: 33 bytes 02/03 or 65 bytes 04 (no hybrid).
pub fn check_pubkey_encoding(pubkey: &[u8], flags: ScriptFlags) -> Result<(), ScriptError>;
```

### 4.7 The ECDSA consensus layer (`ecdsa.rs`)

```rust
/// CPubKey built from bytes: valid iff GetLen(bytes[0]) == bytes.len()
/// (02/03 → 33; 04/06/07 → 65; else invalid). Mirrors CPubKey::Set + IsValid.
#[derive(Copy, Clone)] pub struct PubKeyRef<'a>(&'a [u8]);
impl<'a> PubKeyRef<'a> { pub fn new(bytes: &'a [u8]) -> Option<Self>; pub fn as_bytes(self) -> &'a [u8]; }

/// Core: pubkey.cpp:40 ecdsa_signature_parse_der_lax, ported verbatim to safe Rust.
/// Returns the 64-byte compact (r ‖ s). When EITHER r or s is ≥ n or wider than 32 bytes, BOTH
/// are zeroed (Core's "correctly-parsed but invalid signature": all 64 bytes). None where Core returns 0.
pub fn parse_der_lax(sig: &[u8]) -> Option<[u8; 64]>;
/// Core: pubkey.cpp:182 CPubKey::Verify: IsValid, secp256k1_ec_pubkey_parse (hybrid 06/07 accepted
/// with the y-parity check), parse_der_lax, normalize-S, verify. `msg` is uint256::begin() order.
pub fn verify_lax(pubkey: PubKeyRef<'_>, msg: &[u8; 32], sig: &[u8]) -> bool;
/// Core: pubkey.cpp:348 CPubKey::CheckLowS: lax parse succeeds and normalize changes nothing.
pub fn check_low_s(sig: &[u8]) -> bool;
```

**Choices:**
- **The lax parser is a port of Core's copy, not `secp256k1::ecdsa::Signature::from_der_lax`.** The two differ: `secp256k1-sys` 0.14.1's `contrib/lax_der_parsing.c` rejects a length-of-length of `>= sizeof(size_t)` (8), while Core rejects `>= 4`. The difference is unobservable for signatures of at most 520 bytes, because both then fail the `rlen > inputlen - pos` check. But a verbatim port removes the question and pins the behaviour against future `secp256k1-sys` releases. The rest uses `secp256k1` 0.33:
  - `Signature::from_compact`, with the zero fallback on `Err`;
  - `normalize_s`;
  - `ecdsa::verify` on `Message::from_digest`;
  - `PublicKey::from_slice`, which passes 33- and 65-byte inputs straight to `secp256k1_ec_pubkey_parse`, so hybrid keys behave as in Core.
- **Context.** Verification uses the crate's free `ecdsa::verify`, which borrows a thread-local `Secp256k1<All>` context. A1 §3.3 says "a static context + `from_der_lax`"; both halves deviate (§10 #7). If PF shows the thread-local borrow matters, `DirectVerifier` holds an explicit `Secp256k1<VerifyOnly>`, with no API change.
- **Versions.** The library is libsecp256k1 0.8.0 against Core's 0.3.3-dev; §2 records the difference and pins it.
- **`check_low_s`** compares the compact serialization before and after `normalize_s`, because the crate does not expose normalize's return value.
- **R4's quirk 7 note, confirmed in code:** a strict-DER signature with S ≥ n parses as zero, so it counts as low-S. Under `LOW_S+NULLFAIL` this gives `SigNullFail`, not `SigHighS` (policy only).

### 4.8 Sighash (`sighash.rs`) and the CS0 boundary

```rust
pub const SIGHASH_ALL: i32 = 1; pub const SIGHASH_NONE: i32 = 2; pub const SIGHASH_SINGLE: i32 = 3;
pub const SIGHASH_ANYONECANPAY: i32 = 0x80;
/// Core: interpreter.cpp:1512 SignatureHash(scriptCode, txTo, nIn, nHashType, amount, BASE, cache).
/// `amount` and `cache` are ignored by Core (no BIP143), so they are absent. Panics if
/// n_in >= tx.inputs.len() (Core asserts; every caller guarantees it). hash_type is the full int.
/// The plain serializer: the line-by-line port and C3's reference twin (A1 §3.10).
pub fn signature_hash(script_code: &[u8], tx: &dash_primitives::Transaction, n_in: usize, hash_type: i32) -> [u8; 32];
/// Core: interpreter.cpp:1366 SerializeScriptCode (§5.2: Core's cursor semantics on a malformed tail).
pub fn serialize_script_code(script_code: &[u8], w: &mut impl CoreWrite);

/// Core: PrecomputedTransactionData (interpreter.h:118), built once per transaction where Core calls
/// txdata.Init (CheckInputScripts, after the script-execution-cache miss) and shared by the tx's inputs.
/// It borrows the transaction, so a TxSigData can never be paired with the wrong one. Send + Sync.
/// Phase B: holds only the reference, exactly as inert as Core's (Dash's SignatureHash ignores `cache`).
/// Phase C (C3, §8.3): the blank-script serialization and a SHA-256 midstate at every input boundary.
pub struct TxSigData<'a> { tx: &'a dash_primitives::Transaction, /* C3 state, private */ }
impl<'a> TxSigData<'a> {
    pub fn new(tx: &'a dash_primitives::Transaction) -> Self;
    pub fn tx(&self) -> &'a dash_primitives::Transaction;
}
/// What the checker calls. Phase B: `signature_hash(script_code, txdata.tx(), n_in, hash_type)`.
/// Phase C: the C3 path for the SIGHASH_ALL regime, else the plain path. Under `cfg(any(test, feature =
/// "xcheck"))` it also runs the plain path and panics on any difference.
pub fn signature_hash_with(script_code: &[u8], txdata: &TxSigData<'_>, n_in: usize, hash_type: i32) -> [u8; 32];
```

**What the function writes**, in `CTransactionSignatureSerializer` order (`:1347`).

**PLAN §7 handling: every tx field is encoded through `primitives::canonical`.**

| Part | Writer |
|---|---|
| version/type field | `canonical::write_version_and_type(tx)` |
| input count (1 under ANYONECANPAY) | `serialize::write_compact_size` |
| other inputs | `canonical::write_input(&prevout, &[], seq)`, where `seq` is 0 for other inputs under SINGLE or NONE (Core's `int{0}`) and otherwise the input's own sequence |
| the signed input | `canonical::write_prevout(&prevout)`, then `serialize_script_code(script_code)`, then `canonical::write_sequence(seq)` |
| output count (0 / `nIn+1` / all) | `serialize::write_compact_size` |
| outputs | `canonical::write_output(value, spk)`; for SINGLE, the other outputs are `canonical::write_output(-1, &[])` (`CTxOut()`) |
| lock time | `canonical::write_lock_time` |
| payload | `canonical::write_extra_payload(tx)` (writes nothing unless Core serializes `vExtraPayload`) |
| hash type | 4-byte LE `i32` (not a tx field) |
| digest | CS0's `HashWriter::finish()`: SHA256d |

`(hash_type & 0x1f) == SINGLE && n_in >= tx.outputs.len()` returns `uint256::ONE` (`01 00…00`) before anything is serialized.

**What CS1 takes from CS0** (DEC-25(2) decided the first item, and CS0 rev1 §4.3 has it; §11 N1 asks for the last):
- `canonical::write_prevout(&OutPoint, w)` and `canonical::write_sequence(u32, w)`. CS0's `write_input` is their composition with the script field. The signed input cannot use `write_input`, because Core writes the scriptCode with a compact size that can exceed the bytes written (§5.2);
- `serialize::write_compact_size` and `write_i32_le` as public generic writers;
- `CoreWrite` and `HashWriter`, as in CS0 §4.2, with **`HashWriter: Clone`**: a clone copies the SHA-256 midstate, the buffered partial block and the length. C3 needs it, SH1's engine must provide it, and `bitcoin_hashes`' engines already do.

**The PLAN §7 split, fixed now (Opus n-1).** Bytes CS1 writes itself: the input and output counts, the scriptCode (compact size + body), the hash type. Everything else goes through `canonical`'s writers, in both the plain path and C3's blank serialization. §11 N6 asks the designated reader to confirm this at design approval, not at PR time.

**Why base-sdk's `Transaction`, not a new trait.** It is exactly the input `canonical` takes, so the boundary is CS0's own field writers, with no extra abstraction. Callers holding a `Tx` pass `tx.raw()`. Tests build `Transaction` values directly, or decode them with CS0's `TxDecoder`. BS4a's change of `TxOut.value` to `i64` reaches CS1 only through `write_output`'s argument.

**The checker reads, never encodes.**
- `lock_time`, `inputs[n_in].sequence` and `version` are read as values. Core's `CheckSequence` test `static_cast<uint32_t>(txTo->nVersion) < 2` (`:1610`) is ported verbatim, with C++'s conversion semantics, over base-sdk's `version` field.

### 4.9 Checkers and the sigcache hook (`checker.rs`)

```rust
/// Core: BaseSignatureChecker. Every default returns false, as Core's does.
pub trait SignatureChecker {
    /// GenericTransactionSignatureChecker::CheckSig(sig_with_hashtype, pubkey, scriptCode, BASE).
    fn check_sig(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8]) -> bool { false }
    fn check_lock_time(&self, lock_time: ScriptNum) -> bool { false }   // :1565
    fn check_sequence(&self, sequence: ScriptNum) -> bool { false }     // :1602
}
/// Core's BaseSignatureChecker (EvalScript-only vectors, fuzzing).
pub struct NoSignatureChecker;
impl SignatureChecker for NoSignatureChecker {}

/// Core: GenericTransactionSignatureChecker::VerifySignature, the virtual that
/// CachingTransactionSignatureChecker overrides. THE SIGCACHE HOOK.
/// `sig` has the hash-type byte removed; `pubkey` passed IsValid; `sighash` is the 32-byte digest.
pub trait SigVerifier: Sync {
    fn verify(&self, sig: &[u8], pubkey: PubKeyRef<'_>, sighash: &[u8; 32]) -> bool;
}
/// TransactionSignatureChecker's VerifySignature: ecdsa::verify_lax, no cache.
pub struct DirectVerifier;

/// Core: GenericTransactionSignatureChecker<CTransaction>(txTo, nIn, amount, txdata). CheckSig =
/// PubKeyRef::new (false if invalid) → empty sig false → hash_type = last byte (0..=255) →
/// signature_hash_with (via the C2 memo in phase C) → verifier.verify(sig[..len-1], …).
/// One checker per input, created where the input is verified (InputCheck::run). It is Send but
/// NOT Sync: phase C keeps C2's memo in a RefCell here. Callers never share a checker between
/// threads; they share the TxSigData.
pub struct TransactionSignatureChecker<'a, V: SigVerifier + ?Sized = DirectVerifier> {
    txdata: &'a TxSigData<'a>, n_in: usize, verifier: &'a V,
    memo: core::cell::RefCell<SighashMemo>,   // private; empty and unused in phase B
}
impl<'a, V: SigVerifier + ?Sized> TransactionSignatureChecker<'a, V> {
    pub fn new(txdata: &'a TxSigData<'a>, n_in: usize, verifier: &'a V) -> Self;  // panics if n_in out of range
}
```

**API slots for phase C (DEC-28(b); Opus m-7).** C2 and C3 change no signature after phase B:
- **C3's state** is `TxSigData`. It appears in phase B's API wherever Core passes `PrecomputedTransactionData`: `TransactionSignatureChecker::new`, `InputCheck`, `check_input_scripts` and `classify_failure` (§4.11).
- **C2's state** is the checker's private memo. The checker is already one object per input, which is exactly the memo's correct scope: the sighash is a function of `(scriptCode bytes, hash type)` once `(tx, n_in)` is fixed (§8.3). So the memo needs no parameter. Making the checker `!Sync` *later* would break callers, so phase B declares it `!Sync` now.
- **C1's scratch** is additive: `eval_script_with` / `InputCheck::run_with(&mut Scratch, …)` are new functions beside the existing ones, so it needs no slot.

Opus m-7 proposed routing C2's memo through C1's `Scratch`. That ties a per-(tx, input) cache to a per-worker buffer, which must then be reset at every input boundary. It also threads a parameter through `eval_script`'s public signature. The checker already has the right lifetime, so CS1 keeps the memo there. `Scratch` may still lend the memo its buffers.

**How CS6 uses the hook:**

```rust
impl SigVerifier for CachingVerifier<'_> {           // dashd-chainstate::sigcache (CS6)
    fn verify(&self, sig: &[u8], pk: PubKeyRef<'_>, sighash: &[u8; 32]) -> bool {
        let entry = self.cache.entry(sighash, pk.as_bytes(), sig);   // SHA256(salt ‖ sighash ‖ pubkey ‖ sig)
        if self.cache.get(&entry, /*erase=*/ !self.store) { return true; }
        if !ecdsa::verify_lax(pk, sighash, sig) { return false; }
        if self.store { self.cache.set(entry); }
        true
    }
}
```

This is Core's `CachingTransactionSignatureChecker::VerifySignature` (`sigcache.cpp`), including the erase-on-hit when not storing. The cache only memoises a deterministic function, so it cannot change a verdict. The CS6 model test checks this against `DirectVerifier`.

**Deviation from A1 Appendix A** (§10 #1). A1's `SignatureChecker` has `check_ecdsa(sig, pubkey, script_code, flags)` and `check_data_sig(…)`. CS1 keeps Core's shape instead:
- no `flags`: Core's encoding checks run in the interpreter, before `CheckSig`;
- no `check_data_sig`: `OP_CHECKDATASIG` bypasses the checker in Core. A checker that could intercept it would let fuzz harnesses with mock checkers diverge from Core's.

### 4.10 Sigops (`sigops.rs`)

```rust
/// Core: script.cpp:154 CScript::GetSigOpCount(bool fAccurate).
pub fn script_sigop_count(script: &[u8], accurate: bool) -> u32;
/// Core: script.cpp:178 CScript::GetSigOpCount(const CScript& scriptSig), called on `script_pubkey`.
/// NOT a P2SH-only function (renamed from r0's p2sh_script_sigop_count; GPT M2, Opus m-4):
/// a non-P2SH script_pubkey returns script_sigop_count(script_pubkey, true) without reading script_sig.
pub fn script_sigop_count_with_sig(script_pubkey: &[u8], script_sig: &[u8]) -> u32;
/// Core: tx_verify.cpp:118 GetLegacySigOpCount: Σ script_sigop_count(scriptSig, false) + Σ script_sigop_count(spk, false).
pub fn legacy_sigop_count(tx: &dash_primitives::Transaction) -> u32;
/// Core: tx_verify.cpp:132 GetP2SHSigOpCount. 0 for a coinbase (Core's IsCoinBase: one input with a
/// null prevout). Otherwise, for each input whose spent scriptPubKey IS P2SH, adds
/// script_sigop_count_with_sig(spent, scriptSig). Inputs spending anything else add 0.
/// `spent_script_pubkey(i)` is coin(vin[i].prevout).out.scriptPubKey; the caller asserts the coin exists.
pub fn p2sh_sigop_count<'s>(tx: &dash_primitives::Transaction, spent_script_pubkey: impl Fn(usize) -> &'s [u8]) -> u32;
/// Core: tx_verify.cpp:149 GetTransactionSigOpCount: legacy; then, unless a coinbase, + P2SH if flags contain P2SH.
pub fn transaction_sigop_count<'s>(tx: &dash_primitives::Transaction, spent: impl Fn(usize) -> &'s [u8], flags: ScriptFlags) -> u32;
```

**`script_sigop_count`:**
- It walks `get_op` and stops at the first parse failure, keeping the count so far.
- `CHECKSIG(VERIFY)` counts 1.
- `CHECKMULTISIG(VERIFY)` counts `DecodeOP_N` of the previous opcode when `accurate` and that opcode is `OP_1..OP_16`; otherwise 20. "Previous" means the last opcode parsed, whether or not it would execute; it starts as `OP_INVALIDOPCODE`.
- `OP_CHECKDATASIG(VERIFY)` counts **0** (quirk 4).
- Nothing is executed, so counts are independent of flags, branches and validity.

**`script_sigop_count_with_sig`, both branches** (`script.cpp:178-199`):
1. **`script_pubkey` is not P2SH** (`is_pay_to_script_hash` false): return `script_sigop_count(script_pubkey, true)`. `script_sig` is not read. For `OP_2 CHECKMULTISIG` with any scriptSig, even `OP_RETURN`, the result is **2**.
2. **P2SH:** walk `script_sig` with `get_op`.
   - A parse failure → 0.
   - Any opcode above `OP_16` (0x60) → 0. `OP_RESERVED` (0x50) is below it and does not stop the walk.
   - Otherwise take the data of the **final successful `get_op` call**, parse it as a script, and return `script_sigop_count(data, true)`.

   Core's `GetScriptOp` clears its data vector on **every** call. So a trailing `OP_0`, `OP_1NEGATE`, `OP_RESERVED` or `OP_1..OP_16` makes the data empty and the result 0, even after a push. For example, `<ac>` gives 1 while `<ac> OP_1` gives 0 (GPT probe). An empty scriptSig also gives 0. The port keeps "the last instruction's data", never "the last push's data".

**Transaction level:**
- `p2sh_sigop_count` applies the P2SH filter itself (`tx_verify.cpp:143`, both pins). It must **not** sum branch 1's fallback over ordinary spent scripts: that would overcount and could reject a valid block at CS3's sigop limit.
- Sums use `u32` wrapping arithmetic, as Core's `unsigned int`; no reachable input overflows.
- `is_coinbase` is Core's predicate, computed here from `inputs`. If CS0 exports one on `Transaction`, CS1 uses that instead.

### 4.11 Input checks and failure classification (`input.rs`)

```rust
/// Core: CScriptCheck (validation.cpp:2106): VerifyScript(vin[n_in].scriptSig, spent spk, flags,
/// CachingTransactionSignatureChecker(ptxTo, nIn, amount, cacheStore, *txdata)).
/// Pure, Send + Sync; the unit CS6's check queue runs. `run` creates the per-input checker.
pub struct InputCheck<'a> {
    pub txdata: &'a TxSigData<'a>, pub n_in: usize,
    pub spent_script_pubkey: &'a [u8], pub flags: ScriptFlags,
}
impl InputCheck<'_> {
    pub fn run<V: SigVerifier + ?Sized>(&self, verifier: &V) -> Result<(), ScriptError>;
}

/// The two masks CheckInputScripts reads. `mandatory` comes from CS0's
/// `UngatedRules.mandatory_script_flags` (DEC-41; A1 §2.25 row 19).
#[derive(Copy, Clone)] pub struct ScriptPolicy { pub standard: ScriptFlags, pub mandatory: ScriptFlags }
impl ScriptPolicy {
    pub const V23_1: Self;  pub const V24_0: Self;   // release lines, not tags (DEC-41)
    pub const fn standard_not_mandatory(self) -> ScriptFlags;   // STANDARD_NOT_MANDATORY_VERIFY_FLAGS
}

pub enum ScriptFailureKind { Mandatory /* TX_CONSENSUS */, NonMandatory /* TX_NOT_STANDARD */ }
pub struct InputScriptFailure { pub kind: ScriptFailureKind, pub error: ScriptError, pub n_in: usize }
impl InputScriptFailure {
    /// "mandatory-script-verify-flag-failed (<ScriptErrorString>)" / "non-mandatory-script-verify-flag (<…>)"
    pub fn reject_reason(&self) -> String;
    pub fn into_tx_error(self) -> TxValidationError;   // CS0 §8.1; debug message empty, as Core
}

/// The classification step of CheckInputScripts (validation.cpp:2197-2225) for one failed input:
/// if flags ∩ standard_not_mandatory ≠ ∅, re-run with those flags removed; a pass → NonMandatory.
/// The reported error is ALWAYS the first run's error (Core formats check.GetScriptError(), not check2's).
pub fn classify_failure<V: SigVerifier + ?Sized>(check: &InputCheck<'_>, first: ScriptError,
                                                  policy: ScriptPolicy, verifier: &V) -> InputScriptFailure;

/// CheckInputScripts without caches and without pvChecks (serial): coinbase → Ok; else the first
/// failing input in order, classified. CS6 wraps this with the script-execution cache and the
/// caching verifier; its parallel mode uses InputCheck::run directly and reports Core's generic result.
/// `txdata` is Core's `PrecomputedTransactionData& txdata` argument: the caller builds it once per tx.
pub fn check_input_scripts<'s, V: SigVerifier + ?Sized>(txdata: &TxSigData<'_>,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8], flags: ScriptFlags, policy: ScriptPolicy, verifier: &V,
) -> Result<(), InputScriptFailure>;
```

**Owning the spent outputs is the caller's job** (Opus §3 note). `InputCheck` borrows the spent script. Core copies the `CTxOut` into each `CScriptCheck`, because `UpdateCoins` spends the coins before the queued checks run. CS6 must keep the coins it queues alive until the queue is drained; A1 §3.3's `ScriptCheck { coin }` already owns them.

---

## 5. Fidelity rules (each is a named test)

### 5.1 R4's quirks owned by CS1

| Quirk | Rule | Tests |
|---|---|---|
| **4** CHECKDATASIG | DER plus exactly one trailing byte under DERSIG (any value at consensus; STRICTENC needs a defined type; LOW_S checks S without the byte); single SHA256 of `msg`; lax verify over the **whole** signature; NULLFAIL; zero sigops; never touches `SignatureChecker`/`SigVerifier` | `quirk_04_checkdatasig`: O4's 214 records; a counting `SigVerifier` stays at 0 calls; `script_sigop_count` of `ba`/`bb` = 0; a valid sig with a garbage trailing byte passes without DERSIG and fails with it |
| **5** NUM2BIN / exceptions / unexecuted failures | NUM2BIN size via `getint` → `u64`; the number operand is unbounded and not minimality-checked; exceptions → `UnknownError`; disabled opcodes, `OP_VERIF` and `OP_VERNOTIF` fail when unexecuted | `quirk_05_*`: O4's 4,512 DIP0020 records (stacks compared on success **and** failure); hand-picked rows of `script_tests.json` |
| **6** sighash | SINGLE out of range → ONE; base type `& 0x1f` (0x23, 0x43 are SINGLE; 0 and 4..31 act as ALL); the full 32-bit type appended; FindAndDelete on BASE incl. the empty-sig `0x00` deletion; OP_CODESEPARATOR stripping; the payload is signed | `quirk_06_*`: `tx_valid.json:53-56` (SINGLE bug), `:136-177` (CODESEPARATOR, FindAndDelete); `tx_invalid.json:15,25`; oracle special-tx vectors (§7.3) |
| **7** ECDSA | lax DER (negative ints, padding, long lengths, trailing garbage); R/S overflow → **both** scalars zeroed → false; normalize-S (high-S verifies); hybrid 06/07 keys with parity; `GetLen` validity | `quirk_07_*`: `diff_script_primitives` seeds and the edge seeds of §7.4; pre-BIP66 mainnet inputs from the chain sample (§7.6); a hybrid-key P2PK spend built by the oracle, with the right and the wrong parity; a valid high S paired with an overflowing R |
| **39** scriptCode serialization on a malformed tail | §5.2 | `quirk_39_*`: the matrix in §5.2, Core-produced |

O5 creates these test stubs as `#[ignore]` with CS1 as the owner, and CS1 un-ignores them.

**EMBARGO-1 and CS1** (DEC-30 N2, replacing r0's D7 text, as CS0 rev1 §13 N2 asked). Nothing tracked in CS1 changes for EMBARGO-1. A designated reader (the CS0 implementer) writes **private seeds for CS1's own public L1 targets** in the private tree, with expectations from the oracle on both pins. The gate's private-seed replay runs them. That happens **before CS1 merges**, and it is an item on CS1's merge checklist. CS1's part is only to keep the four targets' input formats stable and documented, so that seeds can be written against them.

### 5.2 Quirk 39: scriptCode serialization on a malformed tail (registered, DEC-25(4))

`SerializeScriptCode` (`interpreter.cpp:1366-1385`):
- writes `WriteCompactSize(scriptCode.size() - nCodeSeparators)`;
- then writes the bytes in segments, up to **where `GetOp`'s iterator stopped**;
- on a malformed tail, that is the failure cursor of §4.3: just after the opcode, or after a **complete** length field. A partial PUSHDATA2/4 length field is never consumed (`script.cpp:270-301`).

So for a scriptCode ending in `4d 01` (PUSHDATA2 with one length byte) or `05 aa` (push 5, one byte present):
- the compact size counts the whole script;
- the bytes stop after `4d` or `05`.

R4-A §3.7 calls this harmless because the script fails anyway. **The verdict does fail, but the error code is observable.** Take `<pk> CHECKSIGVERIFY 05 aa`, signed against Core's (truncated) preimage:
- in Core, `CHECKSIGVERIFY` passes, then `BadOpcode`;
- a port that serialized the full tail would fail with `CheckSigVerify`.

The reject strings differ: `mandatory-script-verify-flag-failed (Opcode missing or not understood)` against `(Script failed an OP_CHECKSIGVERIFY operation)`. They are visible through `sendrawtransaction`/`testmempoolaccept`, the `-par=1` block log, and L3. The same holds for `CHECKMULTISIGVERIFY` and for NULLFAIL. Bitcoin's legacy serializer has the same behaviour.

GPT's probes confirmed it on both pins with real signatures. Signing the full tail instead gives `CHECKSIGVERIFY` (13); in a P2SH wrapper under STANDARD it gives `SIG_NULLFAIL` (37). **The mempool-visible witness is the P2SH form:** a bare non-standard prevout can be rejected by input policy before any script runs.

CS1 ports it exactly (`get_op` leaves the cursor where Core does, §4.3). The manager registered it as quirk 39 and ordered the R4-A §3.7 erratum (DEC-25(4)).

**The `quirk_39_*` matrix.** Every expectation is Core-produced by `oracle_signature_hash` and `oracle_verify_script` on both pins; none is derived by reading code. Axes:
- **Tail form** (Opus m-1):
  - a direct push with short data (`05 aa`);
  - `4c` at the end; `4c n` with short data;
  - `4d` with 0 or 1 length bytes; `4d` with a full length and short data;
  - `4e` with 0–3 length bytes; `4e` with a full length and short data.
- **Before the tail** (GPT, Opus m-1): nothing; an `OP_CODESEPARATOR`; a FindAndDelete hit; `ab` bytes inside pushed data.
- **Operation:** `CHECKSIGVERIFY`, `CHECKSIG` + `NULLFAIL`, `CHECKMULTISIGVERIFY`; bare and P2SH-wrapped.
- **Signed** against Core's truncated preimage and against the full-tail preimage.

O4's `script_code_serialization_matches_core_on_edge_cases` (seven cases, derived by reading code) becomes a subset of this matrix, now backed by the oracle.

### 5.3 Other rules worth a named test

- **`MINIMALDATA` scope.** It applies to executed pushes and to every `ScriptNum` parse. It does not apply to the `OP_NUM2BIN` number operand or the `OP_BIN2NUM` input.
- **CLTV and CSV.**
  - Flag off: NOP2/NOP3, with **no** `DISCOURAGE_UPGRADABLE_NOPS` check (Core breaks before it).
  - CLTV: the lock time must be of the same kind as `nLockTime` and not above it, and **this input's** sequence must not be final.
  - CSV: an operand with bit 31 set is a NOP; otherwise the `(u32)nVersion ≥ 2` test, the input's sequence bit 31 must be clear, and the comparison uses the type flag and 16-bit mask.
- **`FindAndDelete` only matches Core's push form.** A signature pushed non-canonically (say with PUSHDATA1 under 76 bytes) is not removed (`tx_invalid.json:15`).
- **`FindAndDelete`'s cursor** (§4.3): matches after a match are not on original boundaries; a single pass; a verbatim tail after a parse failure.
- **The CHECKMULTISIG order is observable** through `CHECKMULTISIG NOT` under STRICTENC (`interpreter.cpp:1142-1145`; covered by script_tests). So are its key-count charge and its cleanup order (§4.5).
- **Encoding precedence** (GPT): DER → LOW_S → hash type → pubkey → verify/NULLFAIL. The empty-signature exemption covers only the signature encoding: under STRICTENC a reached pubkey is still checked. An off-curve compressed or uncompressed key (it fails `ec_pubkey_parse`) is distinct from a wrong header or length (it fails `CheckPubKeyEncoding`).
- **Numbers and failure stacks** (GPT probes, both pins):
  - `0x7fffffff 1ADD` succeeds with the five-byte `[0000008000]`; a further numeric op on it gives `UnknownError`;
  - NUM2BIN's `ImpossibleEncoding` leaves the size popped and the number minimally encoded in place;
  - BIN2NUM's `InvalidNumberRange` leaves the normalized operand;
  - an invalid PICK/ROLL index has already been popped;
  - an executed `IF` on an empty stack gives `UnbalancedConditional`, not `InvalidStackOperation`.
- **Limits and competing errors:** 520/521-byte pushes, 10,000/10,001-byte scripts, 1,000/1,001 combined stack items, 201/202 ops from each of the two charge sources, and disabled/`VERIF` opcodes in skipped branches.
- **Sigop extraction** (§4.10): branch 1's fallback; `<ac>` → 1, `<ac> OP_1` → 0, a trailing `OP_RESERVED` → 0, an empty scriptSig → 0, a truncated push → 0; the transaction-level P2SH filter.

---

## 6. Error mapping: `ScriptError` → Core's strings and classes

### 6.1 Script level

`ScriptError::fmt` is `ScriptErrorString` verbatim (`script/script_error.cpp`, identical in both pins). `UnknownError` prints "unknown error". `name()` gives `script_tests.cpp`'s token (e.g. `SIG_NULLFAIL`), plus `IMPOSSIBLE_ENCODING`, which Core's test table lacks (O4 §2 found it). Codes are Core's, so oracle integers compare directly.

### 6.2 Transaction and block level (`CheckInputScripts`, `ConnectBlock`)

| Situation | Core | Result class | Reason |
|---|---|---|---|
| input fails and `flags ∩ SNM = ∅` | `validation.cpp:2223` | `TX_CONSENSUS` | `mandatory-script-verify-flag-failed (<first error>)` |
| input fails, `flags ∩ SNM ≠ ∅`, re-run without SNM **passes** | `:2212` | `TX_NOT_STANDARD` | `non-mandatory-script-verify-flag (<first error>)` |
| input fails, the re-run also fails | `:2223` | `TX_CONSENSUS` | `mandatory-script-verify-flag-failed (<first error>)`: still the **first** run's error |
| serial `ConnectBlock` (`-par=1`) | `:2752-2757` | `BLOCK_CONSENSUS` | the tx reason, unchanged (`TxValidationError::into_block`, CS0) |
| parallel `ConnectBlock` | `:2778-2780` | `BLOCK_CONSENSUS` | `block-validation-failed`, empty debug (CS6/CH3a) |

`SNM = STANDARD & !mandatory` gives 0x147DE on v23.1.8 and 0x141CA on v24 (`ScriptPolicy`).

**Per-pin consequence (row 19; corrected for GPT F5).** Take an input that fails **only because of** DERSIG, CLTV, CSV or NULLDUMMY, that is, its re-run without SNM **passes**:
- under v23.1.8 it is reported as `non-mandatory-script-verify-flag (…)`: `TX_NOT_STANDARD` in the mempool, so peers are not punished, and `BLOCK_CONSENSUS` with that string in a block checked at `-par=1`;
- under v24 it is `mandatory-script-verify-flag-failed (…)` / `TX_CONSENSUS`.

If the v23 re-run also fails, the input stays mandatory on **both** pins, with the first error.

GPT's witness, under `P2SH|CLTV` with an empty scriptSig: `OP_1NEGATE CLTV DROP RETURN` first fails with `NEGATIVE_LOCKTIME` (27). The v23 re-run reaches `OP_RETURN`, so both pins report `mandatory-script-verify-flag-failed (Negative locktime)`. With `OP_1` in place of `RETURN`, the v23 re-run passes and v23 reports `non-mandatory-script-verify-flag (Negative locktime)`; v24 still reports mandatory.

CS1 needs no per-release code: `ScriptPolicy` is a parameter, and MP1a and CH3a pass `ReleaseProfile.policy`'s value.

**The one direction a pin difference can take.** v24's mandatory set contains v23's (0x615 ⊇ 0x001), so `SNM_v24 ⊆ SNM_v23`. The v24 re-run therefore keeps a superset of the flags v23's keeps. Removing flags never turns a pass into a failure:
- By inspection, every defined flag only adds failure conditions. That includes CLTV and CSV, whose "off" path is a bare `break`, before the `DISCOURAGE_UPGRADABLE_NOPS` check.
- O4's audit found 0 violations of removal monotonicity on `script_tests` and `tx_valid`.

The first run uses the same flags on both pins, so it reports the same first error. So when the classifications differ between the pins, it is always v23 `NonMandatory` against v24 `Mandatory`. §7.4 checks this, so a violation would surface as a finding rather than be assumed away.

The serial-mode string under v23.1.8 is also `non-mandatory-…` in a block. A1 §3.3's serial bullet names only the mandatory string; §11 N5 asks for it to be aligned (Opus §3 note).

---

## 7. Test plan

### 7.1 L0: Core vectors (via O4's merged `dash-core-vectors`)

O4 merged on `node/integration` (efe889e0). The loaders live in crate `dash-core-vectors` (`test-vectors/`, lib `dash_core_vectors`), a leaf that CS1 takes as a dev-dependency. Names below are that crate's.

| Source | Count | Runner semantics | Phase |
|---|---|---|---|
| `transcribed/dip0020opcodes_tests.json` | 4,512 `EvalScript` runs | records `{script, stack, flags, ok, error, result_stack}`; `eval_script` with `NoSignatureChecker`; compare ok, error **name** (`script_error::SCRIPT_ERROR_NAMES` + `IMPOSSIBLE_ENCODING`), and the final stack (on failure too) | A |
| `transcribed/checkdatasig_tests.json` | 214 runs, incl. positive sigs | same | A |
| `script_tests.json` | 1,282 | `cases::load_script_tests` → `ScriptCase`; `cases::credit_tx`/`spend_tx` bytes → CS0 `TxDecoder`; CLEANSTACK ⇒ add P2SH (DoTest); `verify_script` with `TransactionSignatureChecker<DirectVerifier>`. Monotonicity over **every defined flag**: `flags::removal_sets` for OK rows and `flags::addition_sets` for failing rows, with `FlagRng` (O4's corrected perturbation, not Core's bits 0–12 loop) | B |
| `sighash.json` | 500 | `cases::load_sighash` → `SighashCase`; decode the tx (CS0); `signature_hash(script_code, tx, n_in, hash_type)` (raw script bytes, full i32 type); compare against `expected` in `uint256::GetHex` order (`hex::uint256_to_hex`). Checked: no row has `n_in ≥ vin` (Core would assert) and none hits the SINGLE bug | B |
| `tx_valid.json` | 83 | `cases::load_tx_valid` → `TxCase` (`TxFlags::Script(excluded)`; prevouts through `TxCase::prevout_script`, last listing wins). Enabled set = `DEFINED_FLAGS & !excluded`. **Validity:** every input passes under the enabled set and under each of `flags::removal_sets(enabled, …)`. **Maximality** (GPT M4): for **each** set returned by `flags::restore_one_excluded(excluded)`, **at least one input fails**. That helper works in enabled-flag space and applies CLEANSTACK ⇒ P2SH there. Not `exclude_individual_flags`, which removes enabled flags and so would only repeat the validity check | B |
| `tx_invalid.json` | 76 (66 script + 10 `BADTX`) | `cases::load_tx_invalid`. The 66 `TxFlags::Script(flags)` rows: at least one input fails under `flags` and under each of `flags::addition_sets(flags, …)` (monotonicity, **not** minimality: O4's audit shows Core's own file breaks minimality 64 times). The 10 `TxFlags::BadTx` rows are CS3's (`check_transaction`) | B |

**Runner self-check (GPT M4).** One fixed vector: `tx_valid.json` line 171, excluded `SIGPUSHONLY,CONST_SCRIPTCODE,CLEANSTACK`. For it, the `tx_valid` runner prints which flag each maximality set restores and the enabled mask that reaches `verify_script`. A unit test pins those masks: three separate sets, each restoring exactly one of the three (`CLEANSTACK` with `P2SH`, which is already enabled). So a regression to the wrong helper fails loudly instead of silently testing nothing.

**What CS1 does not run.** Core's harness runs `CheckTransaction` before the scripts in both tx files. CS1's runner requires only that CS0 decodes the tx; CS3's runner covers `CheckTransaction` on the same rows.

**Gate.** M0 exit criterion 10 (PLAN §1) needs both of these (Opus n-2):
- the B rows at 100%;
- `diff_script_verify` and `diff_sighash` clean for ≥ 24 CPU-h per pin (§7.4).

O4's `harness_audit.json` already shows that Core's own interpreter passes these checks (0 violations for script_tests and tx_valid), so a Rust failure is a Rust bug.

### 7.2 Unit, regression and property tests (every PR)

**Ported Core unit tests** (`tests/script_core_units.rs`). Expectations come from Core: either from the test's own literals where Core computes nothing, or re-derived through the oracle. They are never hand-typed. The list is extended per Opus m-3; anchors are v24 `src/test/`.

| Core test | Anchor | What it covers |
|---|---|---|
| `scriptnum_tests` (`creation`, `operators`) | `scriptnum_tests.cpp:164, 177` | proptest against an i128 reference, plus `scriptnum10.h`'s semantics |
| `sigopcount_tests` (`GetSigOpCount`) | `sigopcount_tests.cpp:27` | through `oracle_script_sigops*` (§7.5) |
| `multisig_tests` (`multisig_verify`) | `multisig_tests.cpp:38` | shapes built and checked through the oracle |
| `script_p2sh_tests` (`sign`, `norecurse`, `set`, `is`, `switchover`) | `script_p2sh_tests.cpp:53-246` | same |
| `script_PushData` | `script_tests.cpp:909` | push parsing and evaluation of all four push forms |
| `script_cltv_truncated` | `:951` | CLTV on an empty stack |
| `script_CHECKMULTISIG12`, `script_CHECKMULTISIG23` | `:993`, `:1022` | multisig with real signatures, every key order |
| `script_IsPushOnly_on_invalid_scripts` | `:1234` | push-only on malformed scripts |
| `script_FindAndDelete` | `:1283` | about 20 hand-built cases, including the "odd edge case" |
| `sighash_test` | `sighash_tests.cpp:117` | random transactions against `SignatureHashOld` (§7.3) |

**Reference twins from O4.** O4's `core_get_op` and `core_serialize_script_code` (`dash/tests/core_vectors.rs:32, 67`) are an independent port written for the O4 review. CS1 moves them into `dash_core_vectors::script` as `reference_get_op` / `reference_serialize_script_code`, with a local compact-size writer so the crate stays a leaf, and makes `dash/tests/core_vectors.rs` import them. Then:
- a proptest checks that `get_op`'s result **and failure cursor**, and `serialize_script_code`'s bytes, equal the reference on random scripts biased to truncated pushes;
- a test checks that `parse::push_data` equals `dash_core_vectors::script::push_data`;
- O4's seven edge cases stay in place and join §5.2's oracle-backed matrix.

**Core-checked regressions** (GPT's "verification requirements"; Opus m-2, m-4, m-5). These are committed tests with oracle-produced expectations on both pins, not left to random fuzzing:
- **FindAndDelete:** `51 51 52 52` − `51 52` → `51 52` (1), then empty (1); `01 01` − `01` → empty (2); `03 02ff03 03 02ff03` − `03` → `02ff03 02ff03` (2); an empty pattern → 0 with no rewrite; non-canonical signature pushes; matches inside push data; malformed tails, each with every truncation form of §5.2;
- **CHECKMULTISIG op count:**
  - 180 `NOP`s + `0 0 <20 empty keys> 20 CHECKMULTISIG` → success with `[01]`; 181 → `OpCount`;
  - 181 `NOP`s with a declared 20 and no keys → `OpCount` before `InvalidStackOperation`;
  - 0-of-20 charges 20;
  - a skipped `CHECKMULTISIG` charges 1;
  - the op count and altstack restart for each of scriptSig, scriptPubKey and redeem script;
- **multisig cleanup and encoding:** NULLFAIL before NULLDUMMY with the `[01][7f]` failure stack; reverse key/signature traversal; early exit; signatures and keys whose encoding checks are never reached;
- **sigops:** every case listed in §5.3;
- **classification:** the paired single-cause and multiple-cause vectors of §6.2, per pin. Each asserts the class, the first error and the full reason string;
- **numbers, failure stacks and limits:** every item of §5.3;
- **ECDSA:** `S = n` accepted by LOW_S's encoding path, then NULLFAIL; a hybrid key with the right and the wrong parity; a valid high S with an overflowing R (both zeroed).

**Branch census** (Opus m-3; tightened by CS1a review m-3). `contrib/node/script-branch-census.sh` builds the **Core-backed** test binaries with `-C instrument-coverage` and requires every instrumented line of `interpreter.rs`, `encoding.rs`, `num.rs`, `parse.rs`, `checker.rs`, `ecdsa.rs`, `sigops.rs` and `flags.rs` to be reached, the `ScriptError` sites among them. Core-backed means the binaries whose expectations Dash Core computed: `script_records` (O4), `script_core_recorded` and `quirks` (the recorder) and `script_core_units` (Core's own unit tests). The in-crate unit tests and the property tests check the port against its own expectations, so they do not count. Lines that Core's logic makes unreachable carry `// census: unreachable (<reason>)`; lines whose Core evidence arrives in phase B carry `// census: phase B (<reason>)`, and phase B removes those markers. A marker above a `fn` covers the function. The census fails on any missed line or on a file without coverage data. It runs per PR, and its output is attached to the merge report. It is line-based, so a `?` arm shares its line with the success path; CS1a's review measured region and branch coverage over the same binaries and found every reachable `?` arm taken.

**Property tests:**
- flag monotonicity on random scripts;
- bit 15 and the undefined bits are ignored;
- `eval_script` never panics (also as the single-implementation fuzz target `script_eval_nopanic`);
- **`find_and_delete` idempotence, restricted** (GPT M1): when the pattern is exactly one complete instruction (`get_op` consumes all of it), a second call returns 0 and leaves the script unchanged. The general property is false (§4.3) and is not tested. A companion test asserts the known counterexample, so nobody "fixes" the port into a fixpoint loop;
- `check_low_s(normalize(sig))`, done as "`check_low_s` accepts exactly the normalized signatures" (`ecdsa.rs`);
- C2 and C3 (phase C): the memoised and checkpointed paths equal `signature_hash` on random transactions and scripts (§8.3).

**The error table:** `Display` equals `oracle_script_error_string` for codes 0..=41 on both pins.

### 7.3 Oracle-generated vectors and the second sighash twin

**Special-tx sighash vectors.** `sighash.json` has no v3 special transactions. CS1 commits `test-vectors/oracle/sighash_special.json` with a `PROVENANCE.md`. It is generated by `oracle_signature_hash` on **both** pins, which must agree, otherwise it is an O17 finding. The inputs:
- every special-tx type with at least one input from the BS2 corpus samples: ProRegTx, ProUpServ, ProUpReg, ProUpRev, CbTx, asset lock;
- types 10–12 from V24a's Core-produced vectors;
- for each input, hash types {0, 1, 2, 3, 0x23, 0x80…0x83, 0xff, −1, i32::MIN};
- scriptCodes {the prevout script, the same with interior `OP_CODESEPARATOR`s, with a malformed tail (§5.2), empty}.

The generator script lives under `contrib/node/`. The output is deterministic and pinned by sha256.

**`SignatureHashOld` as a second twin** (Opus m-3). `sighash_tests.cpp:24` keeps Bitcoin's original algorithm:
- it copies the tx and blanks the scripts;
- it strips separators with `FindAndDelete(OP_CODESEPARATOR)`;
- it serializes the whole modified `CMutableTransaction`, payload included.

CS1 ports it under `cfg(test)` as `signature_hash_old`, together with `RandomTransaction`/`RandomScript` (`:91`). Core's generator makes only versions 1–2, so the Rust port **adds v3 special types with random payloads**. `sighash_test` then compares `signature_hash` with `signature_hash_old`, and, under feature `oracle`, both with `oracle_signature_hash`.

The two algorithms agree only on **well-formed** scriptCodes. On a malformed tail, `FindAndDelete` keeps the tail verbatim while `SerializeScriptCode` drops it and keeps the full-length prefix: that is quirk 39 again. The test asserts agreement for well-formed scriptCodes and the *expected* difference for malformed ones, so this twin is a second witness for §5.2. Core's `RandomScript` draws only from a nine-opcode list and never produces a malformed tail; ours does.

### 7.4 L1: differential targets against the O1 oracle

Every target runs against **both** pins. Under CS0's runner (DEC-30 N5) that means one run per pin (`--pin v23.1|v24.0`), each comparing Rust with that pin's oracle. What must agree depends on what is compared (GPT F5):
- **Interpreter results, error codes, digests, sigop counts and ECDSA outputs** must agree three ways: Rust = Core v23.1.8 = Core v24. Clean runs on both pins give this transitively. A divergence on one pin only is replayed against the other pin, privately. If the two pins disagree with each other, it goes to O17 as a PIN-DELTA miss; otherwise it is a Rust bug.
- **Classification and reject strings** are compared **per pin**: Rust under `ScriptPolicy::V23_1` against v23.1.8, and Rust under `ScriptPolicy::V24_0` against v24.
  - The two pins may legitimately differ only in the direction §6.2 proves: v23 `NonMandatory` against v24 `Mandatory`. Such a difference is counted as an expected row-19 delta, not routed to O17.
  - A difference in the other direction, or in the reported first error, is a finding.

| Target | Input (structured, `arbitrary`) | Compared | Oracle entry points |
|---|---|---|---|
| **`diff_script_verify`** | a template (below); a fixed key set in compressed, uncompressed and hybrid encodings, plus **malformed keys**: wrong header, wrong length, off-curve compressed and uncompressed, hybrid with the wrong parity; signature mutations (high-S, BER padding, trailing garbage, R/S overflow, every hash-type byte); the tx shape (version incl. ≥ 3 with a payload, counts, sequences, lock time); flags (random u32, biased to the block flag eras and STANDARD; CLEANSTACK-without-P2SH excluded) | `script_error_code` (three-way); for failures, the classification and reject string per pin | v1 `oracle_verify_script`; O1b `oracle_check_input_scripts` |
| **`diff_sighash`** | tx bytes (decoded by both sides; inputs only one side decodes are skipped and counted, since CS0's `diff_tx_decode` owns that), scriptCode bytes, `nIn mod vin`, hash type (full i32). Phase C adds a mode that hashes every input of the tx through `signature_hash_with` on one shared `TxSigData` (C3) | the 32 bytes | v1 `oracle_signature_hash` |
| **`diff_script_primitives`** | a mode byte: (a) `eval_script(script, initial stack, flags)` with the base checker; (b) ECDSA `(pubkey bytes, msg32, sig bytes)`; (c) `check_signature_encoding(sig, flags)`; (d) `find_and_delete(script, pattern)` | (a) ok, code and final stack; (b) `verify_lax`, `check_low_s`, `parse_der_lax` compact; (c) ok and code; (d) count and script | O1b `oracle_eval_script`, `oracle_pubkey_verify`, `oracle_check_low_s`, `oracle_parse_der_lax`, `oracle_check_signature_encoding`, `oracle_find_and_delete` |
| **`diff_sigops`** | a script; a (spk, scriptSig) pair, with spk biased to P2SH and **non-P2SH** shapes; a tx with spent outputs mixing P2SH and other scripts | `script_sigop_count(_, accurate)` both ways; `script_sigop_count_with_sig`; the three tx-level counts | v1 `oracle_legacy_sigops`; O1b `oracle_script_sigops`, `oracle_script_sigops_with_sig`, `oracle_transaction_sigops` |

**`diff_script_verify`'s templates:**
- random bytes;
- P2PK, P2PKH, bare m-of-n, P2SH-wrapped;
- CLTV/CSV gadgets;
- DIP0020 op soup;
- CODESEPARATOR and FindAndDelete gadgets;
- SINGLE-bug spends;
- malformed tails (the §5.2 forms);
- **multisig encoding gadgets** (GPT): bad encodings placed on pairs the loop reaches and on pairs it never reaches;
- **CHECKDATASIG positive paths** (Opus m-3). Mode (a)'s random bytes never form a valid signature, and r0 had no template for it, so today only O4's 214 records reach the positive path. This template uses harness-key signatures over random messages, with these mutations:
  - every trailing-byte value;
  - high S;
  - lax-DER encodings;
  - hybrid and invalid keys;
  - `CHECKDATASIG` chains of several verifications in one script.

**ECDSA edge seeds for mode (b)** (Opus m-3):
- r or s equal to 0, 1, n−1, n, n+1 or 2²⁵⁶−1;
- s at ⌊n/2⌋ and ⌊n/2⌋+1;
- a pubkey x ≥ p;
- an off-curve x for a compressed key;
- a hybrid key with the wrong parity;
- the DER length-of-length values 0–9 (Core's lax parser rejects ≥ 4; `secp256k1-sys`'s rejects ≥ 8, §4.7).

**Per-mode accounting** (GPT). `diff_script_primitives` and `diff_script_verify` count executions per mode or template, and how many of them got past parsing to an evaluated opcode, a signature check or a verify. The merge report records those counts. A 24 CPU-h budget (per pin) counts only if every mode or template has at least 5% of the executions, and the evaluated share of each is reported. So the budget cannot be spent mostly on trivial parse rejections.

**Signing inside the harness.** The harness signs with Rust's own `signature_hash`. A Rust sighash bug therefore shows up as Core rejecting a signature Rust accepts. `diff_sighash` compares the digests directly, so a bug shared by both paths cannot hide.

**Completion gate** (GPT, closing paragraph). The O1b entry points of §7.5 are a **prerequisite of CS1's phase-B gate**, not an "if available". CS1 is not complete while `diff_script_primitives`, `diff_sigops`'s tx level or `diff_script_verify`'s classification leg is skipped. The PLAN row's L1 budgets apply to all four targets.

**Seeds:**
- every L0 row;
- every unique (scriptSig, spk) pair and special payload harvested by §7.6;
- `bitcoin-core/qa-assets` `script_flags`/`script` corpora (converted; they carry Bitcoin semantics, so they are seeds, not expectations);
- the §7.3 vectors.

**Runner and triage (DEC-30 N5).** The four targets live in `node-fuzz` and run **only** through CS0's runner, `node-fuzz/run.sh <target> --pin <v23.1|v24.0> --cpu-hours H` (CS0 rev1 §9.3). Each target calls `dash_conformance::l1::sink::init(target)` in libFuzzer's init hook, so it refuses to run outside the runner. Divergences never panic: the input goes to the private sink, the run fails closed, and the manager releases non-embargo reproducers (PLAN §7.5). CS1 follows CS0's output rules (opaque names, no input bytes in logs) and adds nothing to the runner.

**Budgets (NFR-CEQ-04)**, per target **per pin**, as CS0 rev1 counts them (its M3 disposition):
- Per PR: a 5-minute smoke per target and pin.
- At merge: ≥ 24 CPU-h per target per pin, all four targets: 4 × 2 × 24 ≈ **192 CPU-h**, about one and a half nightly farm slots (8 h × 16 cores). r0 said 96 CPU-h, counting one three-way run per target.
- At release: ≥ 72 CPU-h per target per pin.

**Locally, and before the farm exists.** The same comparisons run as proptest differential tests (`tests/script_oracle.rs`, feature `oracle`) on every PR. Implementers without access to the private tree fuzz through these twins (DEC-30 N5). CS0's D5 creates `node-fuzz` if O3 has not.

### 7.5 Oracle entry points: the O1b follow-up (DEC-25(3))

DEC-25(3) made these a small O1 follow-up (O1b, queued). r1 renames one entry and adds `oracle_transaction_sigops`, so the tx-level sigop leg no longer waits for O2a (GPT M2). §11 N3 asks for the revised list.

**Thin wrappers** over functions Core exports, with no adapter logic:

```c
int oracle_eval_script(const uint8_t* script, size_t, const uint8_t* stack_in, size_t /* ser vector<valtype> */,
                       uint32_t flags, uint8_t* stack_out, size_t*, int* script_err);      // EvalScript, BaseSignatureChecker, BASE
int oracle_pubkey_verify(const uint8_t* pk, size_t, const uint8_t msg[32], const uint8_t* sig, size_t);  // CPubKey::Verify
int oracle_check_low_s(const uint8_t* sig, size_t);                                         // CPubKey::CheckLowS
int oracle_parse_der_lax(const uint8_t* sig, size_t, uint8_t compact64[64]);                // ecdsa_signature_parse_der_lax + serialize_compact
int oracle_check_signature_encoding(const uint8_t* sig, size_t, uint32_t flags, int* script_err);
int oracle_find_and_delete(const uint8_t* script, size_t, const uint8_t* pat, size_t, uint8_t* out, size_t*, int* n);
int oracle_script_sigops(const uint8_t* script, size_t, int accurate, uint32_t* n);         // CScript::GetSigOpCount(bool)
int oracle_script_sigops_with_sig(const uint8_t* spk, size_t, const uint8_t* script_sig, size_t, uint32_t* n);
                                                    // spk.GetSigOpCount(scriptSig); r0 called it oracle_p2sh_script_sigops
```

**Two adapters** (CON-reviewed: they build a coins view). Both share one helper that builds a `CCoinsViewCache` over a `CCoinsView` dummy. It adds each spent output as a `Coin` at `vin[i].prevout`, with height 1 and not coinbase.

```c
/* GetLegacySigOpCount, GetP2SHSigOpCount, GetTransactionSigOpCount(tx, view, flags). */
int oracle_transaction_sigops(const uint8_t* tx, size_t, const uint8_t* spent_outputs, size_t /* ser vector<CTxOut> */,
                              uint32_t flags, uint32_t* legacy, uint32_t* p2sh, uint32_t* total);
/* CheckInputScripts(tx, state, view, flags, cacheSigStore = false, cacheFullScriptStore = false, txdata, nullptr),
   classified with the pin's own STANDARD_NOT_MANDATORY_VERIFY_FLAGS. */
int oracle_check_input_scripts(const uint8_t* tx, size_t, const uint8_t* spent_outputs, size_t /* ser vector<CTxOut> */,
                               uint32_t flags, oracle_validation* state);
```

**What `oracle_check_input_scripts` must do** (Opus m-8, checked against both pins):
- **Initialise both caches once,** at the first call, behind a `std::once_flag`. The `CScriptCheck` path runs a `CachingTransactionSignatureChecker`, and `CheckInputScripts` probes `g_scriptExecutionCache` before anything else (`validation.cpp:2174` in v24). An un-set-up CuckooCache indexes an empty table. The calls differ per pin:
  - v23.1.8: `InitSignatureCache()` and `InitScriptExecutionCache()`, which read `gArgs` (`-maxsigcachesize`, default `DEFAULT_MAX_SIG_CACHE_SIZE` = 32 MiB, halved for each cache);
  - v24.0.0-rc.3: `InitSignatureCache(size_t)` and `InitScriptExecutionCache(size_t)`, which return `[[nodiscard]] bool`. The adapter passes `DEFAULT_MAX_SIG_CACHE_BYTES / 2` (`32 << 20`, halved) to each, and fails the call with `ORACLE_E_EXCEPTION` on `false` (v1 has no closer status). The sizes do not matter, because nothing is stored.
- **Declare `CheckInputScripts` itself.** It has external linkage in both pins but no header declaration: `validation.cpp` forward-declares it with a default argument (v24 `:199`, v23 `:165`). The shim declares it with the full parameter list and passes all eight arguments.
- **Store nothing:** `cacheSigStore = false`, `cacheFullScriptStore = false`. Then neither cache is ever written, a lookup can never hit, and fuzz iterations cannot leak verdicts into each other.
- **Take `cs_main`,** as Core's annotation requires.
- **Pass a fresh `PrecomputedTransactionData`.** `CheckInputScripts` initialises it itself (`txdata.Init(tx, {})`), as in `ConnectBlock`.

**Input rules** (Opus m-8). Core's view is keyed by outpoint and CS1's closure by input index, so:
- **Duplicate prevouts** make the view and the closure disagree. CS3's `bad-txns-inputs-duplicate` owns that case.
- **A spent output with value −1** is `Coin::IsSpent()`, and `CheckInputScripts` asserts on it.
- **Coinbase-shaped transactions** (one input, null prevout) return `Ok` on both sides without touching the view.

Both adapters refuse the first two with `ORACLE_E_ARG`, O1's status for "an argument Core would assert on". The harness also never generates them, so a fuzzer can neither abort the process nor compare unlike inputs.

`diff_script_verify`'s verdict leg and `diff_sighash` need only v1 entry points. They can start as soon as O1 merges.

### 7.6 Real chain: the script sample (L1 on real data; full L2 comes with CH3a)

A `dash-conformance` subcommand `script-sample` runs read-only against the BS2 oracles' RPC (`contrib/oracle-cli`; txindex is on).
- **Windows:** each flag boundary (mainnet 245,817 / 619,382 / 622,944 / 939,456; testnet 2,075 / 2,431 / 4,300 / 8,064), the DIP3 and V19/V20 eras, a tip window, and every 10,000th block. The synced range at run time; PLAN's acceptance-later rule applies to the rest.
- **For every input:** take the prevout script from `getblock <hash> 3`, which embeds each input's prevout in Dash v24, and verify with `block_script_flags` at that height. Every input must pass, and `oracle_verify_script` on both pins must agree. The sample does not use `getrawtransaction`: on a syncing oracle it was seen to hang for minutes.
- **Census.** It also records how often history exercises each quirk: non-strict-DER signatures before BIP66, high-S, hybrid keys, undefined or ANYONECANPAY hash types, SINGLE-bug spends, `OP_CODESEPARATOR`, FindAndDelete hits, DIP0020 and CHECKDATASIG use. The census goes to `research/` and to the quirk owners, like R4 §2.3.5's scans.
- **Seeds.** Harvested pairs seed §7.4.
- **Regtest.** The v24 functional-test chains in `/work/conformance/regtest-v24/` (`feature_dersig`, `feature_cltv`, `feature_csv_activation`, `feature_nulldummy`) feed the same check. The transactions they reject give classification vectors per pin.

**What the sample can cover at merge** (Opus m-9). The oracles are still syncing. At 08:40Z on 2026-10-08, mainnet was at 846,655 of about 2.55M and testnet at 907,082 of about 1.57M.

| Network | Covered today | Not yet covered |
|---|---|---|
| mainnet | BIP66 (245,817), BIP65 (619,382) and CSV (622,944) boundaries; pre-BIP66 lax-DER history; DIP0001 (782,208); PrivateSend-era transactions | BIP147 (939,456), DIP3 (1,028,160), DIP0020 (1,516,032), DIP0024, V19/V20, MN_RR and modern CoinJoin: **no DIP0020 or CHECKDATASIG use on mainnet is reachable yet** |
| testnet | all four flag boundaries, DIP3 (7,000), **DIP0020 (414,100)**, DIP0024 (769,700), V19 (850,100), V20 (905,100) | MN_RR (1,066,900) and the tip |

That is acceptable under PLAN's acceptance-later rule, but it is now explicit:
- **Now:** the testnet DIP0020/CHECKDATASIG/CODESEPARATOR census from 414,100 (Opus m-9). `/work/scratch/cs1/census.py` scans every block from 414,100 to 929,829 (the testnet tip at 08:57Z) with `getblock … 3` and Core's instruction walk. It counts watched opcodes in output scripts, scriptSigs, P2SH redeem scripts (the spent output is embedded, so it is confirmed P2SH) and spent bare scripts.
  - **Result: zero.** None of the 12 watched opcodes (the nine DIP0020 opcodes, `CHECKDATASIG`, `CHECKDATASIGVERIFY`, `OP_CODESEPARATOR`) appears anywhere in the **515,730** testnet blocks from DIP0020 activation to 929,829 (`census_testnet.log`).
  - **The zero means absent, not missed.** As a positive control, the same `scan()` with `CHECKSIG`, `CHECKMULTISIG`, `EQUAL` and `HASH160` added finds them in outputs, spent bare scripts and P2SH redeem scripts. For example, 631 `CHECKMULTISIG` redeem scripts in 414,100–418,100 (`census_control.log`).
  - **Consequence for the evidence plan.** Testnet history exercises none of DIP0020, CHECKDATASIG or `OP_CODESEPARATOR`, so the real-chain sample adds no coverage of them, on either network today. Their evidence is:
    - L0: O4's 4,726 `EvalScript` records, and `script_tests`/`tx_valid`'s CODESEPARATOR rows;
    - L1: the DIP0020 op soup, the CHECKDATASIG positive template and the CODESEPARATOR gadgets of §7.4;
    - quirk 39's matrix.

    The remaining real-chain check is the mainnet census from 1,516,032 (N7).
  - CS1 commits `census.py` under `contrib/node/` and the result to `research/` with the implementation.
- **Follow-up (§11 N7):** a tracked backlog item, owner CS1 (or the CS1 follow-up owner the manager names). When each oracle reaches its tip: re-run the §7.6 sample and the census on both networks, re-harvest the §7.4 seeds, and re-pick the real-block bench fixture (§8.4).

### 7.7 Later layers (not CS1's gate)

- **L2:** CH3a's replay with `-assumevalid=0`, which verifies every historical input.
- **L3:** the script mutation operators.
- **FT:** `feature_dersig`, `feature_cltv`, `feature_csv_activation` and `feature_nulldummy` on both pins (M3). Those tests assert the §6.2 strings.

---

## 8. Concurrency and performance

### 8.1 Parallel verification

- **The unit of work is `InputCheck`.** It borrows the transaction's `TxSigData` and the spent script. It is `Send + Sync` and has no interior mutability. `run` creates the per-input checker on the worker thread, so the checker's C2 memo is never shared. `check_input_scripts` (serial) and `InputCheck::run` (the queue) share one code path.
- **No locks, globals or thread-locals of our own.** §4.7 covers the secp256k1 crate's thread-local context.
- **CS6 owns scheduling.** r0 repeated A1 §3.3's `par_chunks(128)`. GPT F6 showed that this misreads Core:
  - Core's `CCheckQueue` hands each worker `max(1, min(128, queue / (workers + idle + 1)))` checks. 128 (the `nBatchSize` that `validation.cpp` constructs the queue with) is a **maximum**, and batches shrink as the queue drains (`checkqueue.h:108-113`; the formula is at `:112` in v23.1.8).
  - `-par` adds at most **15 additional** workers; the thread that calls `Wait` also verifies (`init.cpp:1554-1569`, `MAX_SCRIPTCHECK_THREADS`).
  - Fixed 128-input chunks give **one** job for 1–128 inputs and six for a 680-input transaction, whatever the thread count.
- **CS1 does not prescribe a schedule.** It hands these requirements to CS6's brief (§11 N8; GPT F6):
  - adaptive or per-input scheduling, with per-worker scratch reuse;
  - benches at 1, 16, 127, 128, 129 and 680 inputs, with mixed cheap and expensive inputs (P2PKH next to uncached CHECKDATASIG chains, which cost many verifies and **zero** sigops);
  - cold and warm sigcache, at **equal effective thread counts** (Core's count is its workers plus the caller);
  - allocation counts and end-to-end input throughput recorded.

  The A1 §3.3 bullet needs the same correction (§11 N5).
- **Equal threads.** Gated metrics compare at the same validation thread counts (NFR-PERF-02(c)), so rdashd's higher `-par` cap (64) is not a lever for them (A1 D36).

### 8.2 Per-input cost on agentbox (Opus M-1; DEC-28)

agentbox's Xeon E5-2687W v4 has AVX2 and **no SHA-NI**. Instructions are the comparison; cycles under load are noisy (A1 §3.11).

**Measurements:**

| Measurement | Core v23.1.8 / v24.0.0-rc.3 | Rust as r0 designed | Source |
|---|---|---|---|
| ECDSA verify, `CPubKey::Verify` shape, 100 uncompressed keys | 379k instructions/op | 385k (+1.7%; setup included) | Opus review (bench_dash `ECDSAVerify`; a `secp256k1` 0.33.1 loop) |
| SHA256d, single stream | 36.8 instructions/byte (SSE4 transform, auto-detected) | 51.65 (`bitcoin_hashes` 1.2.0: no SSE4/AVX2 single-stream path; SHA-NI only) | Opus review |

**Sighash bytes per input** on chain samples. One sample is 120 random blocks per range, assuming SIGHASH_ALL and a 25-byte P2PKH scriptCode (`/work/scratch/cs1/c3_share.{py,log}`):

| Sample | Plain bytes/input | With C3 | C3 saving | Worst transaction |
|---|---|---|---|---|
| mainnet 300k–700k | 4,301 | 2,341 | **45.6%** | 340 inputs, 50 kB: 4.77 MB → 2.41 MB |
| mainnet 700k–819k | 5,300 | 2,822 | **46.7%** | 656 inputs, 96.8 kB: 17.7 MB → 8.9 MB (49.6%) |
| testnet 414k–872k | 497 | 363 | 26.9% | 29 inputs, 4.4 kB |

The first row's plain figure equals the Opus review's own (4,301), which cross-checks the byte model. Testnet transactions are small, so their gains are smaller.

**Estimated instructions per input** (mainnet 700k–819k, P2PKH; ECDSA plus sighash, interpreter excluded):

| Configuration | ECDSA | Sighash | Total | Against dashd |
|---|---|---|---|---|
| dashd, both pins | 379k | 5,300 × 36.8 = 195k | **574k** | — |
| Rust as r0 designed | 385k | 5,300 × 51.65 = 274k | 659k | **+15%** |
| + C3 only | 385k | 2,822 × 51.65 = 146k | 531k | −7% |
| + SH1 only (36.8/byte assumed) | 385k | 195k | 580k | +1% |
| **+ SH1 + C3 (the plan)** | 385k | 2,822 × 36.8 = 104k | **489k** | **−15%** |
| ceiling: sighash free | 385k | 0 | 385k | 1.49× fewer |

**What follows** (DEC-28):
1. **SH1 is a prerequisite of CS1's bench gate.** It is not a prerequisite of correctness. `SighashAll_1In` gets nothing from C3, so only a Core-class per-byte rate can make that row pass.
2. **C3 and C2 are required** before the gate (§8.3).
3. **ECDSA is at parity, but the instruction comparison is weak.** The +1.7% compares different builds of the library: 0.8.0 in C against 0.3.3 with x86_64 asm (§2), so instructions are a poor cost proxy. The ECDSA rows are therefore compared on cycles in a manager-reserved quiet window (DEC-23(b)). The libsecp256k1 changelog's claimed verify speed-up does not show in instructions and is not counted until that window measures it.
4. **2× per input at equal threads is a declared shortfall** (§8.5).

### 8.3 Optimisations (phase C; A1 §3.10)

| # | Optimisation | Status | Twin | Gate |
|---|---|---|---|---|
| C1 | **Scratch reuse:** `eval_script_with(&mut Scratch, …)` and `InputCheck::run_with` keep stack and altstack capacity and the scriptCode buffer across inputs. Additive API | **measured**: lands if PF0's allocation counters show a material share | the allocating path | PF0 allocation counters |
| C2 | **Per-input sighash memo** in `TransactionSignatureChecker` | **required** (DEC-28(b)) | no memo | under `xcheck` every hit is recomputed and compared; C2's property test |
| C3 | **Per-tx midstate checkpoints** in `TxSigData` | **required** (DEC-28(b)) | `signature_hash`, the plain serializer | `xcheck` inside `signature_hash_with`; `diff_sighash`'s all-inputs mode; C3's property test |

All three need two reviewers (PLAN §0) and leave L1–L3 unchanged.

**C3, exactly.**
- **Regime.** C3 applies when `(ht & 0x1f) ∉ {NONE, SINGLE}` and `ht & ANYONECANPAY == 0`; that covers 0x01, 0x00, 0x04–0x1f and the other ALL-acting bytes. In this regime every other input appears blank with its **own** sequence, and every output appears. Any other hash type takes the plain path. GPT F6 asked for C3 to start with this documented regime; checkpoints over the zero-sequence blank form for NONE/SINGLE are a later, measured extension.
- **Building it** (`TxSigData::new`, only when the tx has N ≥ 2 inputs):
  - Write `B`, Core's serialization of the transaction with every scriptSig empty, through `canonical`'s writers: version/type, N, `write_input(prevout, &[], sequence)` for each input, M, `write_output` for each output, lock time, payload. Record each input's start offset `o_i`, and `o_N` = the offset of the output count.
  - Make one streaming pass of a `HashWriter` over `B[..o_{N-1}]`, cloning it at each `o_i` into `checkpoint_i`.
  - Memory: `|B|` plus N midstates of about 104 bytes each.
- **Using it** (`signature_hash_with` in the regime, input `i`):
  1. `h = checkpoint_i.clone()`;
  2. `canonical::write_prevout(prevout_i, h)`; `serialize_script_code(script_code, h)`; `canonical::write_sequence(seq_i, h)`;
  3. `h.write(&B[o_{i+1}..])`;
  4. `write_i32_le(ht, h)`; `h.finish()`.
- **Why the bytes are identical.** In this regime the plain serializer writes exactly `B[..o_i]` before the signed input and `B[o_{i+1}..]` after it. The only per-input bytes are the signed input itself and the hash type. So the preimage is the same byte string by construction, and the twin, `xcheck` and `diff_sighash` check it.
- **SINGLE out of range** returns ONE before any of this, as in the plain path. That case is not in the regime anyway.
- **Who builds `TxSigData`:** CS6, where Core calls `txdata.Init`, i.e. after the script-execution-cache miss. A1 §3.1 lists the sighash precompute on the `prevalidate` pool. Whether to build eagerly or lazily (a `OnceLock` inside `TxSigData`) is PF's choice behind the same API. MP1a builds it per transaction it checks.
- **Gain:** 46–47% of the hashed bytes on mainnet samples; 49.6% on the worst transaction.
- **Against A1's sketch.** A1 §3.3 sketches a cached prefix/suffix "reassembled" into one buffer and hashed whole. That saves re-serialization but hashes every byte again. C3 checkpoints the hash state, so the shared prefix is hashed once per transaction (§10 #8).

**C2, exactly.**
- **Key and scope.** The key is `(scriptCode bytes, hash-type byte)` and the value the digest. The memo lives in the per-input checker, so it spans one `verify_script`: the scriptSig, scriptPubKey and redeem-script evaluations of one `(tx, n_in)`. With `(tx, n_in)` fixed, the sighash is a function of the key alone, so the memo is exact.
- **What it removes.**
  - Core computes a sighash for **every key `CHECKMULTISIG` tries**. With all-ALL signatures, a 1-of-20 with the key last costs 20 sighashes in Core and 1 with C2; a 15-of-15 costs 15 and 1.
  - Repeated `CHECKSIG`s over the same scriptCode in one input also hit.
- **Size and cost.** At most 4 entries, LRU, compared by `memcmp` of the scriptCode (≤ 10,000 bytes). A miss costs far less than the sighash it guards: `memcmp` is under 1 instruction/byte, against about 37 for SHA-256. PF may tune the entry count.
- **What it cannot do.** An adversary who cycles the hash-type byte defeats any memo. Parity in that worst case comes from SH1 and C3, and §8.4's quadratic row measures it.

### 8.4 Benchmarks and the gate (DEC-28(c))

**Rust:** criterion benches in `benches/script.rs`, using **bench_dash names** so that B1's `microbench --rust-bench-bin` lines up.

**Core:** the existing bench_dash rows, plus a local Core bench patch on branch `node/cs1-bench` (DEC-16: test-only, never pushed). The patch adds only files under `src/bench/` and their makefile lines. It is applied to both pins, although the script, SHA-256 and secp256k1 code is identical in them (§2). B1's script builds it, as it builds CS0's `DeserializeBlockHashMerkle` (DEC-30 N4). If the manager prefers one shared local bench branch for both tasks, CS1's files go there. bench_dash's `main` calls `SHA256AutoDetect()`, so Core's side runs its SSE4 transform. Fixtures are committed under `dash-consensus/benches/fixtures/` and generated by `contrib/node/script-bench-fixtures.py`. Both sides read the same bytes; the Core bench takes the directory from `CS1_BENCH_FIXTURES`.

| Bench | Workload | What it shows |
|---|---|---|
| `ECDSAVerify`, `ECDSAVerify_LargeBlock`, `VerifyNestedIfScript` | existing bench_dash rows (`src/bench/ecdsa.cpp`, `verify_script.cpp`) | per-signature and interpreter parity |
| `SighashAll_1In`, `SighashAll_100In`, `SighashAll_680In` | `SignatureHash` over synthetic txs: P2PKH scriptCode, 680 inputs ≈ 100 kB | per-byte parity (SH1); C3's gain. `_1In` isolates SH1 |
| `VerifyBlockInputs_Mainnet` | `CScriptCheck` (Rust: `InputCheck::run` with `DirectVerifier`) over every input of one real block; serial; no caches; spent outputs from the mainnet oracle. The block is the sample's heaviest in sighash bytes in 700k–819k, re-picked at the tip (§7.6) | "one real block's inputs" (DEC-28(c)) |
| `VerifyBlockInputs_Multisig` | bare and P2SH m-of-n spends: 1-of-20 with the key last, 15-of-15 | C2 |
| `VerifyBlockInputs_QuadraticSighash` | synthetic and consensus-valid. Bare prevouts, each running repeated `CHECKSIG DROP` with strict-DER, non-verifying signatures whose hash-type bytes cycle through all 256 values, so neither C2 nor a sigcache collapses them. One large transaction, sized so that one Core iteration takes about a second; the merge report adds one run at the 1 MB consensus limit in the quiet window | worst-case block time (DoS parity; DEC-28(c)) |
| `VerifyInput_CheckDataSigChain` | inputs each running about 100 uncached `CHECKDATASIGVERIFY`s (zero sigops) | uneven per-input cost (GPT F6); ECDSA-bound |

Rust-only: `ScriptVerifyP2PKH`, `CheckDataSig`.

**The gate** (BG; it replaces r0's "crit verify inputs/s vs B1 microbench script rows", §11 N4):
- **Per PR:** PF0 instruction counts against the previous integration head; a slowdown blocks the PR (NFR-PERF-01).
- **CS1's bench gate,** evaluated after SH1, C2 and C3 have merged:
  - every row except the ECDSA ones: Rust's instructions per op ≤ Core's on the same fixture;
  - the ECDSA rows: cycles in a quiet window (§8.2 item 3);
  - `VerifyBlockInputs_QuadraticSighash` is also tracked as "worst-case block validation time ≤ dashd's" in the merge report.
- **Wall clock** against bench_dash is recorded as indicative only (A1 §3.11).
- **Name the Core build** with every ratio (CS1a review n-1). B1's `bench_dash` (`/work/bench/bin`, built in B1's container) runs about 44k more instructions per `VerifyNestedIfScript` op than the same Core script code built on agentbox with the same flags: phase A's port is 17.5% under the former (554k against 670.5k/672.6k) and 11.5% under the latter (626k/627k). The ECDSA rows are within 0.5% of both.
- **If SH1 slips,** phase B still merges: correctness and M0 exit 10 do not need it. The bench gate stays open and STATUS records why.

### 8.5 Declared shortfall: 2× per input at equal threads (DEC-28(d))

Per-signature ECDSA is the same library on both sides. Even a free sighash leaves a per-input ceiling of about **1.49×** on the 700k–819k sample (574k / 385k instructions). The plan (SH1 + C3) gives about **1.17×** (574k / 489k).

So PRD G5's 2× target is not reachable for ECDSA-bound script validation at equal thread counts. This is its REL-6.3 explanation: per-signature cost is library-bound, and the equal-`-par` per-input ceiling is about 1.3–1.5× for P2PKH-dominated history. A1 §3's performance table gets the entry (§11 N5).

Parallelism changes the picture only for metrics that are not gated at equal thread counts.

---

## 9. Implementation phases and dependencies

| Phase | Content | Needs | Gate evidence |
|---|---|---|---|
| **A** | flags, errors, opcodes, parse, `ScriptNum`, encoding, ECDSA layer, interpreter + `verify_script` (generic checker), CHECKDATASIG, script-level sigops | CS0's crate skeleton (commit 1 of its plan); O4's loaders (**merged**) | the 4,726 O4 `EvalScript` records; the ported Core unit tests and regressions that need no transaction; property tests. Phase A's Core-checked expectations come from a recorder linked against both pins (§10 #15). **L0 is not complete after phase A:** `script_tests.json`, the largest corpus, needs the transaction checker |
| **B** | the plain sighash and `TxSigData` (inert); checker + `SigVerifier`; `InputCheck`; classification; tx-level sigops; `block_script_flags`; the L0 B rows; the §7.3 vectors and the `SignatureHashOld` twin; all four L1 targets; the §7.6 sample and census; the branch census | CS0 `canonical` (+ `write_prevout`/`write_sequence`, DEC-25(2)) and `TxDecoder` merged; O1 merged; **O1b merged** (§7.4's completion gate); `node-fuzz` (O3, or CS0's D5) | M0 exit 10 (L0 B rows at 100% **and** `diff_script_verify`/`diff_sighash` ≥ 24 CPU-h per pin); the PLAN row's acceptance, with all four targets at their budgets |
| **C** | **C3 and C2 (required, DEC-28(b))**; C1 (measured); the Core-side bench patch and fixtures; the benches | CS0's `HashWriter: Clone`; **SH1 merged before the bench gate is evaluated** (not before C2/C3 are written) | twins + `xcheck`; `diff_sighash`'s all-inputs mode; unchanged L1; the bench gate of §8.4 |

**Phase-B notes from the CS1a reviews** (reviews/CS1a-r1-*.md; none blocks phase A):
- **Flag validation** (Opus-quality m-1). `ScriptPolicy` and `InputCheck` reject flags with `!is_valid_combination()` where they build them, and every flag computation `debug_assert!`s it, so a future mask edit fails at construction rather than as a panic inside a check-queue worker. `verify_script`'s `assert!` stays as the last line of defence.
- **One source for the mandatory mask** (Opus-quality m-5). CS0's `UngatedRules::mandatory_script_flags` becomes a `ScriptFlags` set from `MANDATORY_V23_1`/`MANDATORY_V24_0`; classification reads only the field, so the constants never act as a second release selector.
- **Core tests phase B must port** (Opus-quality §5.3): `script_json_test` (`script_tests.json`), the script parts of `tx_valid`/`tx_invalid`, `sighash_from_data`/`sighash_test`, `script_CHECKMULTISIG12/23`, `multisig_verify`, and `script_p2sh_tests` `sign`/`norecurse`/`switchover`.
- **Oracle evidence** (Opus-quality §5.4). O1b's entry points were merged at CS1's base by the end of phase A. Phase B adds `tests/script_oracle.rs`, a proptest differential behind the `oracle` feature that needs no fuzz runner, and the four L1 targets once CS0's runner merges (DEC-30 N5). The recorder stays as a regression fixture.
- **Checker-success evidence from the oracle** (Opus-consensus n2). O1's `EvalScript` entry point only runs `BaseSignatureChecker`, and the real-transaction target reaches CHECKMULTISIG success only with valid signatures. `diff_script_verify` gets a parameterised mock checker on both sides: either an O1 entry point taking an outcome schedule and returning the `(sig, pubkey, scriptCode)` transcript (the form of CS1a GPT's `trace.cpp`), or the recorder's hash-verdict mock.
- **Deep conditionals** (GPT n2). The recorder's deep-conditional set (balanced depths 99–101, open depths 200–202, ELSE runs, false ancestors) seeds phase B's oracle differential.
- **Census markers.** `flags.rs`'s `block_script_flags`, `difference` and `is_valid_combination` carry `census: phase B`: `GetBlockScriptFlags` is static in `validation.cpp`, so its Core evidence is the §7.6 chain sample across each activation height; the other two get theirs from classification. Phase B removes the markers.
- **Allocations** (Opus-quality n-3). The two scriptCode copies per signature check (`interpreter.rs`, `parse.rs` `find_and_delete`) mirror Core and go to phase C's profiling (§8.3).

**Reconciliation with CS0's skeleton** (Opus-quality §4, m-4). Done when CS1 rebased onto `node/integration` after CS0's merge (`76ce0052`):
- `dash-consensus/clippy.toml`: **CS0's file**, plus CS1's bans it lacked (the time, I/O and thread bans, which `pure-clippy.toml` also carries for `check-pure.sh`), so a plain `cargo clippy` enforces them too. CS0's and CS2's bans are unchanged.
- `dash-consensus/Cargo.toml`: CS0's, plus `bitcoin_hashes`, the exact `secp256k1 =0.33.1`/`secp256k1-sys =0.14.1` pins with their comment, and the `script` bench.
- `src/hash.rs`: CS0's, plus `sha256`, `sha1`, `ripemd160` and `hash160`; CS1's `Sha256dEngine` was dropped (only its own test used it; CS0's `HashWriter` is the `Clone` engine). `src/lib.rs`: CS0's, plus `pub mod script`.
- CS1's commit that moved its provisional hashes to `dash-sha256` became empty and was dropped: CS0's crate already hashes through it.
- The script tests' helpers live in `tests/support/script.rs`, mounted as `script_support`, because CS2's tests own `tests/common/`.
- The root `members` list and `.github/ci-groups.yml` already had the crate; `Cargo.lock` gains its three new dependencies.

**CS1 is complete at the end of phase C.** Phase B alone is enough for M0 exit 10 and for CH3a/CS6 to start integrating, because the phase-C API is already in place (§4.9).

Phase A can start now, on CS0's skeleton commit or in the CS1 worktree rebased onto it when it merges. CS1's L0 suites do not depend on `Tx`'s hashing or release rules, only on decoding and `canonical`.

**Commit plan** (one logical change each):
1. `feat(consensus): add script flags, ScriptError and the opcode table`
2. `feat(consensus): add script parsing helpers and ScriptNum`
3. `feat(consensus): add signature encoding checks and the ECDSA consensus layer`
4. `feat(consensus): add the script interpreter with DIP0020 and CHECKDATASIG`
5. `test(consensus): run Core's DIP0020 and CHECKDATASIG EvalScript records`
6. `feat(consensus): add script sigop counting`
7. `test(vectors): share the reference GetScriptOp and SerializeScriptCode ports` (moves O4's helpers into `dash_core_vectors::script`; fixes the out-of-range doc at `cases.rs:298`)
8. `feat(consensus): add the legacy signature hash over canonical field writers` (with the inert `TxSigData`)
9. `feat(consensus): add the transaction signature checker, SigVerifier and input checks`
10. `test(consensus): run script_tests, sighash and tx_valid/tx_invalid; un-ignore quirks 4-7 and 39`
11. `test(consensus): port Core's script unit tests and the Core-checked regressions`
12. `test(consensus): add oracle-generated special-transaction sighash vectors and the SignatureHashOld twin`
13. `test(node-fuzz): add the script, sighash, primitives and sigops differential targets`
14. `test(conformance): add the script sample and the opcode census over the reference oracles`
15. `perf(consensus): checkpoint the sighash midstate per input boundary (C3)`
16. `perf(consensus): memoise sighashes per input (C2)`
17. `perf(consensus): reuse interpreter scratch across inputs (C1)`, only if measured
18. `bench(consensus): add script, sighash and block-input benches under bench_dash names`
19. `docs(consensus): map the script module to Core and list its deviations`

Separately, in `~/workspace/dash` on branch `node/cs1-bench` (DEC-16, local only): `bench: add the CS1 sighash and block-input benches`, applied to both pins.

**Size:** L (3–5 agent-weeks, up from 2–4 for C2, C3 and the benches), plus about 192 CPU-h of fuzzing at merge (4 targets × 2 pins × 24, §7.4). **Review class:** CON (two reviewers + the `code-review` skill at high effort); phase C's optimisations also need A1 §3.10's twin review. CS1 adds no code to the marked module; it only calls `canonical`'s public writers. Whether any CS1 hunk needs the marker (PLAN §7.3), and so the excluded-diff review route of §7.4, is for a designated reader to decide when the PR is ready.

---

## 10. Deviations from A1 and PLAN

| # | A1 / PLAN | CS1 | Why |
|---|---|---|---|
| 1 | App. A `SignatureChecker { check_ecdsa(…, flags), check_data_sig, … }` | Core's shape: `check_sig(sig, pubkey, script_code)`, `check_lock_time`, `check_sequence`; CHECKDATASIG bypasses the checker | Core runs the encoding checks in the interpreter and calls `CPubKey::Verify` directly for CHECKDATASIG (`:1059-1071`). A checker hook there would make mocks diverge |
| 2 | App. A `signature_hash(script_code, tx: &Tx, n_in, hash_type: u32)` | `(script_code, tx: &dash_primitives::Transaction, n_in, hash_type: i32)`, plus `signature_hash_with(script_code, &TxSigData, n_in, hash_type)` | the base-sdk shape is what `canonical` writes and keeps CS1 off `Tx` (callers pass `tx.raw()`); Core's hash type is a signed `int` and `sighash.json` uses negative values; `TxSigData` is Core's `PrecomputedTransactionData` |
| 3 | App. A `verify_script(…, checker: &dyn SignatureChecker)` | generic `&C where C: SignatureChecker + ?Sized` | `dyn` still works; the generic form inlines on the hot path |
| 4 | CS0's r0 draft said "sigop counting is CS3's code" | CS1 ports the counting functions; CS3 owns the limits and the `Tx` cache slot | **decided by DEC-25(1)**; CS0 rev1 already says so (its §1 and §5) |
| 5 | PLAN row CS1 deps "—" | CS0, O1, O3, O4, O5 (DEC-25(5)), plus O1b and, for the bench gate only, SH1 | **DEC-25(5)**, DEC-28; §11 N4 |
| 6 | R4-A §3.7: the malformed-tail scriptCode serialization is harmless | it changes error codes and reject strings (§5.2) | **registered as quirk 39, erratum ordered (DEC-25(4))** |
| 7 | A1 §3.3 ECDSA: "a static context: `from_der_lax` plus `normalize_s`" | the crate's thread-local context; Core's lax parser ported, not the crate's `from_der_lax` | §4.7: the crate's parser differs from Core's at length-of-length 4–7 (unobservable at ≤ 520 bytes, but the port removes the question); an explicit context is available if PF shows it matters (Opus n-2) |
| 8 | A1 §3.3 sighash precompute: a cached prefix/suffix "reassembled" and hashed as one buffer | C3: SHA-256 midstates checkpointed at input boundaries over the blank serialization | reassembly saves serialization but hashes every byte again; checkpoints remove 46–47% of the hashed bytes on mainnet samples (§8.2, §8.3) |
| 9 | A1 §3.3 check queue: `par_chunks(128)` | CS1 prescribes no schedule; requirements go to CS6 | Core's batches are adaptive, with 128 as a maximum (§8.1; GPT F6) |
| 10 | App. A: a checker over `(tx, n_in)` | `TransactionSignatureChecker::new(&TxSigData, n_in, &verifier)`, one per input, `!Sync`; `InputCheck` and `check_input_scripts` take `&TxSigData` | DEC-28(b)'s API slots for C2 and C3 (§4.9) |
| 11 | PLAN row CS1 BG: "crit verify inputs/s vs B1 `microbench` script rows" | §8.4's gate: Core-side sighash and block-input rows, instruction parity, quiet-window cycles for ECDSA | DEC-28(c): none of B1's script rows hashes a preimage, so the gate could not see the sighash |
| 12 | this design's r1 §4.3: `get_op(…) -> Result<Op, ()>` with `enum Op { Push { opcode, data }, Code(u8) }` | `get_op(…) -> Option<Op>` with `struct Op { opcode, data }` | accepted in CS1a review (Opus-quality §5.1): `Result<_, ()>` carries no more than `Option` and clippy flags it; the struct is Core's own shape (`opcodetype` plus `vchRet`) and makes "data cleared on every call", which `script_sigop_count_with_sig` relies on, explicit. The failure cursor is unchanged and tested against O4's reference port and Core |
| 13 | this design's r1 §4.1–§4.3 | additions: `instructions()` (fused), `ScriptFlags::intersects`, `ScriptFlags::DEFINED`, `ScriptError::ALL`/`message`, `SCRIPT_ERROR_COUNT`, `script_error_string` | used by the interpreter (`intersects` for the encoding-check set) and by the Core-backed tests; recorded per CS1a review m1 |
| 14 | this design's r1 §4.1 `MANDATORY_V23_1_8`, §4.11 `ScriptPolicy::V23_1_8` | `MANDATORY_V23_1`, `ScriptPolicy::V23_1` | **DEC-41**: release variants name release lines, not tags. The constants are values only; the running release's mask comes from CS0's `UngatedRules.mandatory_script_flags` (§9, phase-B notes) |
| 15 | §7.5: Core-checked expectations through O1b's entry points | phase A's come from `contrib/node/cs1-script-recorder`, which calls Core's own functions linked from both pins' archives, requires identical output and is re-derived by `record.py --check` | O1b merged only as phase A ended, and DEC-30 N5 puts every fuzz target behind CS0's runner. Phase B adds the oracle differentials (§9, phase-B notes); the recorder stays as a regression fixture |
| 16 | §3: a dev-dependency check of the opcode table against base-sdk's `dash_script` | not done in phase A; the names are checked against Core's `GetOpName` for all 256 bytes (recorded on both pins) and O4's table | `dash-script` is not a dependency at the base-sdk pin, and adding it guards base-sdk, not consensus. Deferred to a backlog item that puts the drift check where `dash-script` already is; base-sdk names the small integers `OP_0`/`OP_1NEGATE`/`OP_1..OP_16` where Core's `GetOpName` says `0`/`-1`/`1..16` (CS1a review §5.2) |
| 17 | CS1a review n-5: move the exact `secp256k1` pins into `[workspace.dependencies]` | the pins stay in `dash-consensus/Cargo.toml`; the root entry's comment points at them | a workspace-level `=0.33.1` would be written into the published crates' manifests (dashcore, key-wallet) and exact-pin every downstream user. Resolution already fails visibly on a bump, because cargo unifies the workspace on `=0.33.1` |

---

## 11. Decisions and follow-ups

**Settled since r0.**
- **DEC-25** settled r0's D1–D5:
  1. CS1 implements sigop counting; CS3 owns the limits and the `Tx` cache.
  2. CS0 adds `write_prevout`/`write_sequence`.
  3. The oracle entry points come as a small O1 follow-up (O1b).
  4. Quirk 39 is registered, with an R4-A §3.7 erratum.
  5. CS1's PLAN dependencies are CS0, O1, O3, O4 and O5, and phase A may start after design approval.
- **DEC-28** settled the performance plan: SH1, C2 and C3 required before the bench gate, API slots now, Core-side benches, and the declared 2× shortfall.
- **DEC-29 D3** added CS2's two entry points to O1b, beside CS1's.
- **DEC-30**, from CS0 rev1, changes how CS1 works, and this revision follows it:
  - private seeds for CS1's public targets replace r0's D7 text, written before CS1 merges (§5.1);
  - every target runs only through CS0's runner, with budgets per pin (§7.4);
  - Core bench patches are built by B1's script (§8.4).

**Follow-ups this revision asks for** (none blocks phase A):
- **N1. CS0.**
  - `HashWriter: Clone`, copying the midstate, the partial block and the length (§4.8).
  - The hash module exports `sha256` (one-shot), `sha256d`, `sha1`, `ripemd160` and `hash160`; rev1's `hash.rs` lists only `sha256d`, `sha256d_64_many` and `x11`.
  - Already done in CS0 rev1, no longer asked: `write_prevout`/`write_sequence` (its §4.3) and the DEC-25(1) sigop wording (its §1 and §5; Opus n-2).
- **N2. SH1's contract, from CS1's side.**
  - A streaming SHA-256 engine that is `Clone` and bit-exact, reached only through CS0's module.
  - A single-stream rate on agentbox at least Core's SSE4 transform (36.8 instructions/byte).
  - CS1's `SighashAll_1In` row is a ready-made acceptance row for it.
- **N3. O1b** (§7.5):
  - rename `oracle_p2sh_script_sigops` to `oracle_script_sigops_with_sig`;
  - add `oracle_transaction_sigops`;
  - adopt the `oracle_check_input_scripts` adapter specification: per-pin cache init, no stores, `ORACLE_E_ARG` for duplicate prevouts and −1 values.

  O1b is a prerequisite of CS1's phase-B gate.
- **N4. PLAN row CS1:**
  - dependencies per DEC-25(5), plus O1b, plus SH1 for the bench gate only;
  - the BG text replaced by §8.4's gate;
  - size L (3–5 agent-weeks).
- **N5. A1 edits:**
  - **§3.3** check-queue bullet: adaptive batches, 15 additional workers, CS6's requirements (§8.1);
  - **§3.3** serial-mode bullet: under v23.1.8 a DERSIG/CLTV/CSV/NULLDUMMY-only failure logs `non-mandatory-script-verify-flag (…)` with `BLOCK_CONSENSUS` (§6.2; Opus §3 note);
  - **§3.3** sighash-precompute bullet: midstate checkpoints (§10 #8);
  - **§3.3** ECDSA bullet: §10 #7;
  - **§3** performance table: the declared shortfall of §8.5 (DEC-28(d)).
- **N6. Designated reader:** confirm §4.8's PLAN §7 split at design approval (Opus n-1).
- **N7. Backlog item:** when each oracle reaches its tip, re-run §7.6's sample and census on both networks, re-harvest seeds and re-pick the real-block bench fixture. Owner: CS1, or whoever the manager names for CS1's follow-ups (Opus m-9).
- **N8. Other briefs:**
  - **CS6:**
    - the scheduling requirements of §8.1 (GPT F6);
    - owning the queued coins (§4.11);
    - building `TxSigData` where Core calls `txdata.Init` (§8.3);
    - integration tests: a later non-script failure takes precedence over queued script failures; caches on and off; erase-on-hit (GPT's cache row);
    - parallel-mode gating differs per pin: v23 `g_parallel_script_checks`, v24 `scriptcheckqueue.HasThreads()` (`validation.cpp` v24:2480);
    - `-maxsigcachesize` semantics differ per pin (§2).
  - **CH3a:** classification takes `ReleaseProfile.policy`, and the `-par=1` strings differ per pin (§6.2).
  - **MP1a:**
    - classification takes `ReleaseProfile.policy`;
    - the consensus pass uses `GetBlockScriptFlags(m_chain.Tip())` (`validation.cpp:1216`), so MP1a passes `DeploymentActiveAt(tip)` booleans to `block_script_flags`;
    - `AreInputsStandard` uses `eval_script` with `NoSignatureChecker` and `script_sigop_count(_, true)`, both already in the API;
    - MP1a builds one `TxSigData` per transaction (Opus §3 notes).
- **For O17:** §2 is the script layer's PIN-DELTA evidence. Only row 19 is in scope. The libsecp256k1 subtree bump is class (c); so is our side's 0.8.0 against Core's 0.3.3-dev.
- **For O4:** the `cases.rs:298` doc fix ("returns ONE for an input index out of range": Core asserts, `interpreter.cpp:1514`) is now done by CS1's commit 7, since O4 has merged.

---

## 12. Round-1 review dispositions

Reviews: `reviews/CS1-design-r1-gpt.md` (GPT; APPROVE-WITH-CHANGES, M1–M4 major, F5–F6) and `reviews/CS1-design-r1-opus.md` (Opus; APPROVE-WITH-CHANGES, M-1 major, m-1–m-9 minor, n-1–n-2 nits). Manager decisions applied: DEC-25 and DEC-28; the revision also follows DEC-30 (CS0 rev1).

**Counts:** 26 items. **22 accepted** as proposed, **2 accepted with a modified fix** (Opus M-1, m-7), **2 forwarded** to the owning task with CS1's part done (GPT F6, Opus's MP1a tip-flags note), **0 rejected**. The items are 6 GPT findings, 4 GPT verification requirements, 12 Opus findings and 4 Opus "notes for other tasks"; Opus n-2's five sub-items count as one. Every Core anchor a finding cites was re-read on both pins. Two claims were re-checked by running or reading the probe output: FindAndDelete with Core's own code (a new probe), and the v23 classification witness in GPT's v23 log. GPT's ECDSA, number and failure-stack probe results and Opus's instruction counts are used as reported.

| ID | Sev. | Finding (short) | Disposition | Where | Evidence and notes |
|---|---|---|---|---|---|
| GPT M1 | Major | FindAndDelete is not idempotent for arbitrary patterns; "opcode-boundary matches only" is wrong | **Accepted** | §4.3 (algorithm, consequences), §7.2 (regressions; property restricted), §5.3 | Core `interpreter.cpp:224-247` read on both pins. `51515252 − 5152` → 1, then 1 (GPT probe). The restricted property (pattern = one complete instruction) is proved in §4.3 and probed: **0** second-pass hits in 1,032,264 trials on both pins, 388 for multi-instruction patterns (`/work/scratch/cs1/fad_probe*`) |
| GPT M2 | Major | Sigops: the non-P2SH fallback and final-parser-data semantics are missing; the tx-level P2SH filter is not explicit | **Accepted** | §4.10 (rewritten; renamed `script_sigop_count_with_sig`), §5.3, §7.2, §7.4 `diff_sigops`, §7.5 (`oracle_transaction_sigops`) | `script.cpp:178-199`, `tx_verify.cpp:143` (both pins); GPT probe: non-P2SH → 2, `<ac>` → 1, `<ac> OP_1` → 0. The tx-level oracle moves from O2a to O1b so the exclusion is testable now |
| GPT M3 | Major | The resource-limit checklist omits CHECKMULTISIG's key-count charge | **Accepted** | §4.5 (two charge sources; the 11-step order), §7.2 | `interpreter.cpp:1103-1105`; GPT probe: 180 NOPs → OK, 181 → `OP_COUNT`, missing keys → `OP_COUNT` first |
| GPT M4 | Major | `tx_valid` maximality names the wrong O4 helper | **Accepted** | §7.1 (`restore_one_excluded`, "at least one input fails"; self-check on line 171) | O4 `flags.rs:139-163`; `PROVENANCE.md` §3–4 (the audit validates `restore_one_excluded`) |
| GPT F5 | Minor | The v23 summary says "every" flag failure is non-mandatory | **Accepted** | §6.2 (qualified; witness; the one-direction proof), §7.2 (paired vectors), §7.4 (per-pin classification; expected deltas not routed to O17) | GPT's v23 log: `cltv_then_return` first=27, retry fails (3) → mandatory; `cltv_only` → non-mandatory |
| GPT F6 | Minor | Fixed 128-input chunks and microbenchmarks do not establish queue throughput | **Forwarded to CS6**; CS1's part done | §8.1 (Core's facts corrected; requirements listed), §8.4 (`VerifyInput_CheckDataSigChain`, block-input rows), §11 N5/N8 | `checkqueue.h:108-113`, `init.cpp:1554-1569`. Scheduling is CS6's (PLAN row CS6); CS1 no longer prescribes `par_chunks(128)`. C3 stays in its documented ALL regime |
| GPT V1 | — | Commit the confirmed behaviours (quirk-39 forms, encoding precedence, multisig cleanup, numbers and failure stacks, limits) as regressions | **Accepted** | §5.2 matrix, §5.3, §7.2 "Core-checked regressions" | expectations from the oracle on both pins |
| GPT V2 | — | Make the O1 follow-up a completion gate for the extended comparisons | **Accepted** | §7.4 "Completion gate", §9 phase B | — |
| GPT V3 | — | Record per-mode execution and coverage so the budget is not spent on parse rejection | **Accepted** | §7.4 "Per-mode accounting" (≥ 5% per mode; evaluated share reported) | — |
| GPT V4 | — | Signed templates with malformed/off-curve keys and reached/unreached multisig encoding failures | **Accepted** | §7.4 `diff_script_verify` (keys; multisig encoding gadgets) | — |
| Opus M-1 | Major | The sighash share is underestimated; Rust SHA-256 is ~40% more instructions per byte; C2/C3 optional; no Core-side sighash row | **Accepted per DEC-28, fix modified** (C2 widened to per-input) | §0.6, §8.2 (measured profile), §8.3 (C2/C3 required, exact designs), §8.4 (Core-side benches, real-block and quadratic rows, gate), §8.5 (shortfall), §9 (phase C, SH1), §11 N2/N4/N5 | Opus's measurements, plus a new C3 measurement: **45.6% / 46.7%** of hashed bytes saved on mainnet samples, 26.9% on testnet (`/work/scratch/cs1/c3_share.log`); the model reproduces Opus's 4,301 B/input |
| Opus m-1 | Minor | `get_op`'s failure cursor is misstated for PUSHDATA2/4 | **Accepted** | §4.3 (exact rule), §5.2 (matrix of all truncation forms), §7.2 (reference twin) | `script.cpp:270-301`; O4's merged `core_get_op` states the same rule and becomes `reference_get_op` |
| Opus m-2 | Minor | `find_and_delete`'s doc: not opcode-boundary-only; the empty pattern | **Accepted** (merged with GPT M1) | §4.3 | Core's "odd edge case" example reproduced in §4.3 |
| Opus m-3 | Minor | Missing Core unit tests; no CHECKDATASIG positive generator; ECDSA edge seeds; branch census | **Accepted** | §7.2 (port table; branch census via coverage), §7.3 (`SignatureHashOld` twin with v3 types), §7.4 (CHECKDATASIG template; edge seeds) | The census uses `cargo llvm-cov` rather than counters, so consensus code carries no instrumentation. `SignatureHashOld` agrees only on well-formed scriptCodes; the test asserts the quirk-39 difference too |
| Opus m-4 | Minor | The P2SH sigop form omits the non-P2SH branch; "last push" is really "last opcode's data" | **Accepted** (merged with GPT M2) | §4.10 | — |
| Opus m-5 | Minor | CHECKMULTISIG's op-count increment is missing from the ordering list | **Accepted** (merged with GPT M3) | §4.5 | Opus's order is exactly §4.5's steps 2–7 |
| Opus m-6 | Minor | libsecp256k1 version skew, not pinned or gated | **Accepted** | §2 (our side recorded), §3 (exact `=0.33.1` / `=0.14.1`), §4.7 | vendored rev `6e2c8bc4` (0.8.0) read from `secp256k1-sys-0.14.1`; Core's `configure.ac` says 0.3.3-dev on both pins. A bump re-runs mode (b) and quirk 7 at the full budget |
| Opus m-7 | Minor | No API slot for C2/C3 state | **Accepted, fix modified** | §4.8 (`TxSigData`), §4.9 (per-input `!Sync` checker holds the memo), §4.11 | `TxSigData` as proposed (it mirrors Core's `PrecomputedTransactionData` and borrows the tx, so it cannot be mispaired). The C2 memo lives in the per-input checker rather than C1's `Scratch`: that is its natural scope, it covers repeated CHECKSIGs too, and it needs no new parameter on `eval_script` (§4.9). `HashWriter: Clone` asked of CS0 (N1) |
| Opus m-8 | Minor | The `oracle_check_input_scripts` adapter is under-specified | **Accepted** | §7.5 (per-pin cache init, store-nothing, forward declaration, `cs_main`, input rules) | v23 `InitSignatureCache()`/`InitScriptExecutionCache()` read `gArgs`; v24's take a size and return `bool`; `CheckInputScripts` has no header declaration in either pin |
| Opus m-9 | Minor | The real-chain sample can cover only pre-2018 mainnet | **Accepted** | §7.6 (coverage table; testnet census run now; follow-up N7) | Full testnet census of 515,730 blocks (414,100–929,829): **no** DIP0020, CHECKDATASIG or CODESEPARATOR use anywhere; a positive control shows the detector works (`/work/scratch/cs1/census_{testnet,control}.log`). So the L0/L1 sources in §7.6 carry that evidence |
| Opus n-1 | Nit | Settle the PLAN §7 split now | **Accepted** | §4.8, §11 N6 | — |
| Opus n-2 | Nit | Bookkeeping: D1–D5 decided; CS0's sigop text; M0 exit 10 includes L1; the §10 deviation for A1's ECDSA sentence; the `PubKeyRef::new` call | **Accepted** (all five) | §11, §10 #4–#7, §7.1, §4.5 | CS0's sigop text was already fixed in CS0 rev1 |
| Opus §3 MP1a-1 | Note | The mempool pass uses the tip's flags | **Forwarded** | §11 N8 | `validation.cpp:1216` |
| Opus §3 MP1a-2 | Note | AreInputsStandard needs `eval_script` + `script_sigop_count(_, true)` | **Accepted** (no change needed) | §11 N8 | already in the API |
| Opus §3 CS6 | Note | CS6 must own the coins it queues | **Accepted** | §4.11, §11 N8 | — |
| Opus §3 CS6/CH3a | Note | Align A1 §3.3's serial bullet with §6.2 | **Accepted** | §6.2, §11 N5 | — |

**Modified fixes, summarised.**
- **Opus m-7:** the C2 memo's home is the per-input checker, not C1's `Scratch` (§4.9). `TxSigData` is as proposed.
- **Opus M-1:** DEC-28(b) names C2 a per-opcode multisig memo. CS1 widens it to a per-input memo keyed by `(scriptCode bytes, hash type)`, which subsumes the per-opcode case and also removes repeated `CHECKSIG` sighashes. It is still bounded, and §8.3 states its limit: cycling hash types defeats any memo, so the worst case rests on SH1 + C3. Everything else in M-1 is as proposed.
- **GPT M1** is accepted as proposed. Its fix allowed keeping a restricted property if it is proved, and §4.3 proves one (for patterns that are one complete instruction, which includes the canonical single pushes) and probes it against Core.

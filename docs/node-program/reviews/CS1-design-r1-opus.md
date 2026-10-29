# CS1 design review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `designs/CS1-script.md` (draft of 2026-10-08) |
| Reviewer | Opus 5.5, independent design reviewer. Not an EMBARGO-1 reader; no private material consulted |
| Context read | `DECISIONS.md` (DEC-24, -25, -26); A1 §0.8, §3.3, D36, App. A; CS0 §1, §4, §5, §12; R4 §1.4, §1.15; R4-A §2.5, §3, §4 and the erratum; PLAN §1 (M0 exit 10), §4.2 rows CS1/CS6, §7 |
| Core checked | v23.1.8 (`79b6c5215f`) and v24.0.0-rc.3 (`da567d7dba`): `script/{interpreter,script,script_error,sigcache,standard}.*`, `pubkey.*`, `consensus/tx_verify.cpp`, `validation.cpp`, `policy/policy.h`, `primitives/transaction.h`, `test/{script,transaction,sighash}_tests.cpp`, `bench/{ecdsa,verify_script}.cpp`, `feature_{dersig,cltv}.py`, the `src/secp256k1` subtree |
| Also checked | O1 shim `src/test/oracle/oracle.{h,cpp}` (`684cb425e2`); O4 vectors (`test-vectors/core/0364ed626ec2`); base-sdk `33fcc59` `Transaction`; `secp256k1` 0.33.1 and `secp256k1-sys` 0.14.1 sources; `bitcoin_hashes` 1.2.0; `bench_dash` v23.1.8 and v24.0.0-rc.3 on agentbox; the mainnet and testnet oracles (read-only RPC) |
| Scratch | `/work/scratch/cs1-review-opus/` (extracted sources, a Rust microbench crate, chain-sample scripts) |

Severity scale: **Blocker**, the design is wrong and must change before approval. **Major**, the design must change before the affected phase starts. **Minor**, fold the fix into the implementation brief. **Nit**, wording or bookkeeping.

## Verdict: APPROVE-WITH-CHANGES

There are no blockers. The port specification is unusually exact. I checked every Core anchor in it against both tags and found no semantic error that would lead a line-by-line implementer to a different verdict or error code.

There is one major finding, M-1, in the performance section. §8 calls per-input cost "parity" and makes the sighash optimisations optional. On agentbox, which has no SHA-NI, the legacy sighash is a large share of per-input cost: about 30% of Core's instructions. The Rust SHA-256 path executes about 40% more instructions per byte than Core's. As designed, script validation would therefore run about 14% more instructions per input than dashd, against NFR-PERF-01, and none of the BG rows would detect it.

The minor findings sharpen four spec sentences that would mislead an implementer, close test-plan gaps, pin the libsecp256k1 dependency, and reserve API slots for the phase-C optimisations.

---

## 1. Verified correct (no change needed)

- **Anchors.** Every line anchor I sampled (36) matches v24.0.0-rc.3. `interpreter.cpp` and `script.cpp` have the same numbering in v23.1.8, and `CheckLowS` is at 320 and 348.
- **Pin audit (§2).** Confirmed point by point:
  - `interpreter.cpp` differs only in `valtype()` → `emplace_back()`.
  - `script.cpp` and `script_error.cpp` differ only in copyright lines. `tx_verify.cpp` and `CScriptCheck::operator()` are identical.
  - `CheckInputScripts` differs only in how it pushes to `pvChecks`.
  - The four JSON files are identical, and `secp256k1/src/ecdsa_impl.h` changes only comments.
  - `MANDATORY_SCRIPT_VERIFY_FLAGS` goes 0x001 → 0x615, and `STANDARD` is 0x147DF in both pins. SNM = 0x147DE (v23) and 0x141CA (v24) are computed correctly.
- **`EvalScript` ordering (§4.5)** matches `interpreter.cpp:364-1335` step for step:
  - SCRIPT_SIZE is checked before the `try`;
  - GetOp, then push size on unexecuted pushes, then op count, then disabled opcodes, then CONST_SCRIPTCODE;
  - executed pushes are minimality-checked;
  - VERIF and VERNOTIF fail through `default:` even when unexecuted;
  - the stack-size check runs after every opcode;
  - an unbalanced conditional is caught at the end.
- **Exceptions.** Every `stacktop()` and `popstack()` site is guarded by a size check, so in practice only `scriptnum_error` throws. No opcode mutates the stack before a throwing parse, so the `?` → `UnknownError` mapping preserves Core's failure stack.
- **DIP0020.** The CAT, SPLIT, NUM2BIN, BIN2NUM, AND/OR/XOR, DIV and MOD semantics are right. `(getint() as i32) as u64` sign-extends exactly as C++'s `uint64_t size = int`. `MinimallyEncode` runs in place, so the stack on an IMPOSSIBLE_ENCODING or INVALID_NUMBER_RANGE failure is mutated, and O4's failure-stack comparison checks exactly that.
- **CHECKDATASIG** (`:1048-1089`): the encoding checks, including the trailing byte; a single SHA256; `CPubKey(vch).Verify` over the whole signature; NULLFAIL; no checker and no sigcache. `GetSigOpCount` (`script.cpp:154-176`) has no CHECKDATASIG case, so it costs zero sigops.
- **CHECKSIG/CHECKMULTISIG.**
  - FindAndDelete runs before the encoding checks, so under CONST_SCRIPTCODE, SIG_FINDANDDELETE takes precedence over SIG_DER.
  - An empty signature gives the pattern `00`.
  - The `ikey2`/NULLFAIL bookkeeping and NULLDUMMY are right.
  - The sighash is recomputed for every `CheckSig` attempt.
- **Encoding checks.** The DER → LOW_S → STRICTENC chain is right, and `CheckPubKeyEncoding` applies only under STRICTENC. The quirk-7 note holds: a strict-DER signature with S ≥ n parses as zero, so it counts as low-S and fails with NULLFAIL.
- **ECDSA layer.**
  - Core's lax parser and `secp256k1-sys` 0.14.1's `contrib/lax_der_parsing.c` differ exactly where §4.7 says (`>= 4` vs `>= sizeof(size_t)`), plus a cosmetic `else if (rlen)` guard on a zero-length `memcpy`.
  - `PublicKey::from_slice` in 0.33.1 sends 33- and 65-byte keys to `secp256k1_ec_pubkey_parse`. libsecp256k1 0.8.0 still accepts 06/07 with the parity check (`eckey_impl.h:22-31`).
  - `ecdsa::verify` uses a thread-local `RefCell<Secp256k1<All>>`.
  - Normalizing before verify is required, because `secp256k1_ecdsa_verify` rejects high S.
- **Sighash.**
  - The serializer order is right: `int{0}` for the other sequences, `CTxOut()` = (−1, empty), ONE before any serialization, `& 0x1f`, and the full `i32` appended.
  - `CTransactionSignatureSerializer` writes the version/type field and the payload condition with the same expressions as `CTransaction::Serialize`, so reusing `canonical`'s writers is exact.
  - `sighash.json` has 500 rows: none with nIn out of range, none hitting the SINGLE bug, none special (versions 1–2 only), as §7.1 claims.
- **Quirk 39 is real.** I traced `GetScriptOp` (`script.cpp:259-301`) through `SerializeScriptCode` (`:1366-1385`): the compact size counts the whole scriptCode, while the bytes stop at the failed cursor. A script with a malformed tail always ends in BAD_OPCODE, so only the error code (and with it the reject string) is observable, as §5.2 says. Bitcoin's serializer is the same.
- **Classification (§6.2)** matches `validation.cpp:2197-2225`, `2752-2757` and `2778-2780`. `feature_dersig.py:122,129` and `feature_cltv.py:160-164` flip from `non-mandatory-script-verify-flag (…)` to `mandatory-script-verify-flag-failed (…)` between the pins, as stated. The reported error is the first run's.
- **The sigcache sketch** is `CachingTransactionSignatureChecker::VerifySignature`. The entry is `(sighash, CPubKey bytes, sig without the hash type)`, with erase-on-hit when not storing.
- **L0 counts confirmed** in O4's tree: 1,282 / 500 / 83 / 76 (10 BADTX) / 4,512 / 214.
- **CheckSequence.** base-sdk's `Transaction.version` is `i16`, so `(version as u32) < 2` reproduces `static_cast<uint32_t>(int16_t) < 2`.

---

## 2. Findings

| ID | Sev | Section | Problem | Fix |
|---|---|---|---|---|
| M-1 | **Major** | §0.6, §8.1–§8.3 | The sighash share is underestimated. The Rust SHA-256 single-stream path runs ~40% more instructions per byte on agentbox, so the per-input cost as designed is ~+14% over dashd. C1–C3 are optional, and no BG row measures the sighash | Detail below |
| m-1 | Minor | §4.3 `get_op` | "after the opcode and any length bytes it managed to read" is wrong for PUSHDATA2/4: Core consumes the length field only if all of it is present | Detail below |
| m-2 | Minor | §4.3 `find_and_delete` | "Opcode-boundary matches only" is wrong for general patterns, and the empty-pattern rule is missing | Detail below |
| m-3 | Minor | §7.2, §7.4 | Core unit tests with hand-built edge cases are missing, and no generator produces valid CHECKDATASIG signatures | Detail below |
| m-4 | Minor | §4.10 | `p2sh_script_sigop_count` omits Core's non-P2SH branch | Detail below |
| m-5 | Minor | §4.5 | The CHECKMULTISIG op-count increment is missing from the "each item is a test" ordering | Detail below |
| m-6 | Minor | §2, §3, §4.7 | The libsecp256k1 version differs from both Core pins and is not pinned or gated | Detail below |
| m-7 | Minor | §4.9, §8.2 | No API slot for C2/C3 state, although the signatures are declared normative | Detail below |
| m-8 | Minor | §7.4, §7.5 | The `oracle_check_input_scripts` adapter and its harness are under-specified (sigcache init, outpoint keying) | Detail below |
| m-9 | Minor | §7.6 | The real-chain sample can only cover pre-2018 mainnet today | Detail below |
| n-1 | Nit | §0.3, §4.8 | Settle the PLAN §7 split now | Detail below |
| n-2 | Nit | various | Bookkeeping | Detail below |

### M-1 (Major): the performance plan cannot show parity on sighash-heavy history as written

**Evidence.** Measured on agentbox (Xeon E5-2687W v4, AVX2, no SHA-NI) at a load average of about 59 on 32 vCPUs. Wall time and cycles are noisy, so instruction counts are the reliable comparison (memory note: ±0.04% under load). IPC differs between implementations, so treat instructions as a proxy.

| Measurement | Core v23.1.8 | Core v24.0.0-rc.3 | Rust as designed |
|---|---|---|---|
| ECDSA verify, `CPubKey::Verify` shape, 100 uncompressed keys (`bench_dash ECDSAVerify` vs the same loop over `secp256k1` 0.33.1: `from_slice`, lax parse, `normalize_s`, `verify`) | 379,143 ins/op | 378,080–378,971 ins/op | 385,433 ins/op (+1.7%, setup included) |
| ECDSA cycles/op (noisy) | 144,843 | 145k–232k | 170k–245k |
| SHA256d over 1 MiB, instructions/byte | 36.80 (SSE4.1 single-stream transform, auto-detected) | 36.80 | 51.65 (`bitcoin_hashes` 1.2.0 software path) |
| SHA256d cycles/byte (noisy) | 16.5 | 13.3–14.1 | 17.9–22.8 |
| For reference: Core's `SHA256_STANDARD` (generic C) | — | 52.88 ins/byte | — |

`bitcoin_hashes` 1.2.0 accelerates single-stream SHA-256 only with SHA-NI (`sha256/crypto/mod.rs:306-348`); its SSE4.1 and AVX2 code covers only the 64-byte `sha256d_64` merkle path. Core's single-stream `Transform` uses the SSE4 assembly whenever SHA-NI is absent.

**Chain shape.** I sampled 120 random mainnet blocks per range through the mainnet oracle and computed the SIGHASH_ALL preimage bytes per input (tx size minus scriptSigs, plus a 25-byte scriptCode and the hash type):
- heights 300k–700k: **4,301 bytes per input**;
- heights 700k–790k: **5,106 bytes per input**. The worst tx had 656 inputs and 96.8 kB, which gives 17.7 MB of preimage for that one tx.

The sample is CoinJoin-era, so modern blocks are unlikely to be lighter.

**Per-input instruction estimate** (ECDSA + sighash only, 5.1 kB preimage):
- Core: 379k + 5.1k × 36.8 ≈ **567k**;
- Rust as designed: 385k + 5.1k × 51.65 ≈ **649k (+14%)**;
- with C3 (half the sighash bytes): ≈ 517k (−9%);
- with C3 and an SSE4-class single-stream backend: ≈ 479k (−15%).
- Removing the sighash entirely would give 567k / 385k ≈ **1.47×**. That is the Amdahl ceiling per input at equal thread counts.

**What this means for the design.**
1. §8.1's "expected profile" treats the sighash as a small add-on to "tens of µs" of ECDSA. On this hardware it is ~30% of Core's per-input instructions and ~40% of the designed Rust per-input instructions. PF0 gates on instructions, so the regression would show at the first comparison.
2. **C3 is not optional.** It is the difference between −9% and +14%. It (or A1 §3.3's prefix/suffix precompute, which A1 already lists as architecture) must be in scope before CS1's BG gate is evaluated, not "if PF1 justifies it". The SHA-256 single-stream backend (CS0's `hash.rs` seam, PF-owned per CS0 §15) is a dependency of CS1's performance acceptance, not an independent PF item. Say so in §9.
3. **C2 also matters for worst-case blocks (DoS parity).**
   - Core recomputes the full sighash for every (sig, key) attempt in CHECKMULTISIG.
   - The quadratic-hashing worst case (many bare multisigs in a ~100 kB tx, up to the 40k sigop limit) costs Rust ~1.4× Core's time per byte unless C2 and the backend land.
   - Worst-case block validation time should be a tracked metric that is no slower than dashd's.
4. **The BG gate cannot see any of this.** B1's `microbench` script rows are `ECDSAVerify`, `ECDSAVerify_LargeBlock` and `VerifyNestedIfScript`; none hashes a preimage, and `bench_dash` has no SignatureHash bench. §8.3's `Sighash_All_{1,100,680}in` are Rust-only. Add a Core-side comparison:
   - either a SignatureHash bench on a local Core branch (allowed by DEC-16);
   - or a loop over `oracle_signature_hash` after `SHA256AutoDetect()`, compared on instructions.

   Then add a "verify one real block's inputs" bench, with spent outputs from the chain sample, against Core's `-par=1` `ConnectBlock` `-debug=bench` "Verify N txins" timing with assumevalid off.

**Fix.**
- Rewrite §8.1's profile with the numbers above.
- Make C3 (and C2) required deliverables, with their twins and `xcheck`, before CS1's BG gate.
- Record the SHA-256 single-stream backend as a prerequisite owned by PF/CS0.
- Add the Core-side sighash and real-block comparisons to §8.3, and a worst-case-block DoS row.
- State the equal-`-par` ceiling (~1.3–1.5× per input for P2PKH-dominated history) as the REL-6.3 explanation for ECDSA-bound validation. Note that rdashd's `-par` cap of 64 is not a lever for gated metrics (NFR-PERF-02(c), A1 D36).

**Answer to the brief's question.** Parity on ECDSA-bound blocks is reachable: per-signature cost is at parity within noise, and C3 plus a faster SHA-256 turn the sighash term into a gain. 2× at equal threads is not reachable (ceiling ≈ 1.47× even with zero sighash cost). A1 already declares 2× per core infeasible. The libsecp256k1 0.8.0 changelog's claimed ~10–20% verify speed-up under GCC (no field asm since 0.4.1, force-inlined 5x52 in 0.8.0) does **not** show up in instruction counts (+1.7%). Any IPC gain needs a quiet-window measurement before it is counted.

### m-1 (Minor): `get_op`'s failure cursor must be stated exactly (it drives quirk 39)

`GetScriptOp` advances past the opcode byte, then reads the length field only if all of it is present:
- `end - pc < 1/2/4` returns before advancing;
- otherwise it advances past the whole length field, and `nSize > end - pc` then fails with the cursor after the length field.

For example, for `4e 01 02` (PUSHDATA4 with 2 of 4 length bytes), Core's cursor stops after `4e`. A port following §4.3's wording ("any length bytes it managed to read") stops after `4e 01 02`, writes two more scriptCode bytes into the preimage and changes the error code.

**Fix:** state the rule exactly. Add quirk-39 vectors (Core-produced via `oracle_signature_hash`/`oracle_verify_script`) for every truncation form:
- a direct push with short data;
- `4c` at the end;
- `4c n` with short data;
- `4d` with 0 or 1 length bytes;
- `4d` with a full length and short data;
- `4e` with 0–3 length bytes;
- `4e` with a full length and short data.

Run each form with and without an OP_CODESEPARATOR before the tail, and with a FindAndDelete hit before the tail. Run each under CHECKSIGVERIFY, CHECKSIG+NULLFAIL and CHECKMULTISIGVERIFY.

### m-2 (Minor): `find_and_delete`'s doc comment

FindAndDelete does not only match at opcode boundaries. After a match the cursor advances by the pattern length and `GetOp` resumes there, which need not be an original boundary. Core's own `script_FindAndDelete` "odd edge case" (`script_tests.cpp:1283ff`) shows it: `0302ff030302ff03` minus `03` gives `02ff03 02ff03` with count 2. An empty pattern returns 0 without rewriting.

At consensus the pattern is always a canonical push, so this matters only for `diff_script_primitives` mode (d) and for implementers who read the doc rather than the anchor.

**Fix:** describe Core's cursor semantics, and port that test's cases (see m-3).

### m-3 (Minor): test-plan gaps

**Core unit tests to port** (§7.2 lists only scriptnum, sigopcount, multisig and p2sh):

| Test | Location | What it covers |
|---|---|---|
| `script_FindAndDelete` | `script_tests.cpp:1283` | about 20 hand-built cases |
| `script_PushData` | `:909` | push parsing |
| `script_cltv_truncated` | `:951` | CLTV on an empty stack |
| `script_CHECKMULTISIG12` and `23` | `:993`, `:1022` | multisig with real signatures |
| `script_IsPushOnly_on_invalid_scripts` | `:1234` | push-only on malformed scripts |
| `sighash_test` | `sighash_tests.cpp:117` | random transactions against `SignatureHashOld` |

Port `SignatureHashOld` as a second reference twin for C3. Its random transactions should include v3 special types, because Core's `RandomTransaction` only makes versions 1–2.

**CHECKDATASIG positive paths in L1.** Mode (a) feeds random bytes, which never form a valid signature, and `diff_script_verify`'s templates contain no CHECKDATASIG. Today only O4's 214 records test the positive path. Add a structured template: harness-key signatures over random messages, with these mutations:
- trailing-byte values;
- high S;
- lax-DER encodings;
- hybrid and invalid keys.

**ECDSA edge seeds for mode (b):**
- r or s equal to 0, n−1, n, or 2²⁵⁶−1;
- s at n/2 and n/2 + 1;
- pubkey x ≥ p;
- an off-curve x for a compressed key;
- a hybrid key with the wrong parity.

**Branch census.** List every `return Err(ScriptError::…)` site in `interpreter.rs`, with at least one L0 vector or committed seed that reaches it. That makes "each item is a test" checkable.

### m-4 (Minor): the P2SH sigop form

`CScript::GetSigOpCount(const CScript& scriptSig)` returns the scriptPubKey's own accurate count when it is **not** P2SH (`script.cpp:180-181`); §4.10 describes only the P2SH branch. At tx level `GetP2SHSigOpCount` guards with `IsPayToScriptHash`, so this is reachable only through the script-level API and `diff_sigops`.

Also, "counts the last push" means the data of the **last opcode**. `GetOp` clears `vData` for OP_0, OP_1NEGATE, OP_RESERVED and OP_1..OP_16, which give 0.

**Fix:** document both points, and seed `diff_sigops` with non-P2SH scriptPubKeys.

### m-5 (Minor): CHECKMULTISIG's op count

`nOpCount += nKeysCount` and its immediate OP_COUNT check sit between PUBKEY_COUNT and the key-stack size check (`interpreter.cpp:1100-1105`). §4.5's ordering list, where each item is a test, omits it.

**Fix:** add it with this ordering:

key count parse → PUBKEY_COUNT → OP_COUNT → key stack → sig count parse → SIG_COUNT → sig stack → FindAndDelete loop.

### m-6 (Minor): libsecp256k1 version skew and pinning

**The versions differ.** Rust links libsecp256k1 **0.8.0**: `secp256k1-sys` 0.14.1, vendored rev `6e2c8bc4`, built by `cc` with no `USE_ASM_X86_64`. Both Core pins bundle **0.3.3-dev** with x86_64 field and scalar asm (v24 has a later subtree). "Both use libsecp256k1" (§0.6, §8.1) hides this. Pubkey parsing (including hybrid keys), normalize and verify come from the vendored C, so CS1 has pinned only the lax parser.

**Nothing gates a change.** The workspace requirement is the caret `secp256k1 = "0.33.1"`. BS3's committed lockfile fixes the version, but a routine `cargo update` can move `secp256k1-sys` within 0.14.x to a newer vendored libsecp256k1.

**Fix:**
- Record the Rust-side version in §2 (an O17 class-(c) item on our side).
- Pin `secp256k1-sys` exactly in `dash-consensus`, or add it to the pin check.
- Require `diff_script_primitives` (b) and `quirk_07_*` on any change. The 24 CPU-h budget applies to a version bump as to a new target.

### m-7 (Minor): reserve the phase-C plug-in points now

The trait signatures are declared normative, and the design rules out interior mutability. That leaves nowhere for C2's per-opcode sighash memo or C3's per-tx midstates:
- `TransactionSignatureChecker::new(tx, n_in, verifier)` has no per-tx precompute slot. Core has `PrecomputedTransactionData*`.
- `check_sig(&self, …)` has no scratch.

Adding them later changes the API that CS6 (`InputCheck`) and MP1a call.

**Fix:**
- Add a `txdata: &'a TxSigData` argument now. It is built once per tx and is `Sync`; empty in phase B, it holds C3's checkpoints later.
- Route C2's memo through the C1 `Scratch` that `eval_script_with` passes to the checker.
- Ask CS0 now for `HashWriter: Clone`, which C3 needs to fork the SHA-256 midstate.

### m-8 (Minor): the `oracle_check_input_scripts` adapter and harness

- **Cache initialisation.** `CheckInputScripts` → `CScriptCheck` runs a `CachingTransactionSignatureChecker`. The shim today calls neither `InitSignatureCache` nor `InitScriptExecutionCache` (checked with grep in `oracle.cpp`), and an un-set-up CuckooCache indexes an empty table. The adapter must call both: v23's `InitSignatureCache()` reads `gArgs`, and v24's takes a byte size. §7.5 mentions only the script-execution cache.
- **Harness constraints.**
  - **Unique prevouts.** Core's view is keyed by outpoint, CS1's closure by input index.
  - **No spent output with value −1.** `Coin::IsSpent()` asserts.
  - **Coinbase-shaped transactions are `Ok` on both sides.**
  - **Sigcache isolation.** With `cacheSigStore = false` nothing is inserted, so the global sigcache cannot leak between fuzz iterations. Keep it that way.

### m-9 (Minor): real-chain coverage today

The mainnet oracle was at 790,251 of 2,551,737 blocks, and testnet at 857,782 of 1,568,444, when I checked (07:40Z). The §7.6 mainnet sample can therefore only cover pre-2018 history at merge: no DIP0020 or CHECKDATASIG use, no V19/V20 eras, no modern CoinJoin. That is acceptable under the acceptance-later rule, but it needs to be explicit.

**Fix:**
- Run the DIP0020/CHECKDATASIG census on testnet from height 414,100 now.
- Add a tracked follow-up with an owner to re-run the mainnet sample and census when the oracle reaches the tip, and to re-harvest seeds.

### n-1 (Nit): settle the PLAN §7 split at design time

§0.3 and §4.8 decide which sighash bytes CS1 writes itself (the counts and the scriptCode) and which go through `canonical`. Ask the designated reader to confirm that split now rather than at PR time. Moving a writer between modules later changes CS0's public surface.

### n-2 (Nit): bookkeeping

- Mark §11 D1–D5 as decided by DEC-25.
- CS0 still says sigops are CS3's code (§1 "not in scope", §12 #5); update it per DEC-25(1).
- §7.1 says M0 exit 10 is "the B rows at 100%". PLAN §1 also requires the L1 targets at ≥ 24 CPU-h.
- §10 should list the deviation from A1 §3.3's "static context + `from_der_lax`": CS1 ports the lax parser and uses the crate's thread-local context.
- `verify_lax(PubKeyRef::new(pubkey), …)` in §4.5 needs `PubKeyRef::new(pubkey).map_or(false, |pk| verify_lax(pk, …))`.

---

## 3. Notes for other tasks (not CS1 findings)

- **MP1a.** The mempool's consensus pass uses `GetBlockScriptFlags(m_chain.Tip())` (`validation.cpp:1216`): the **tip's** activation state, not the next block's. `block_script_flags` takes booleans, so MP1a must pass `DeploymentActiveAt(tip)`.
- **MP1a.** `AreInputsStandard` needs `eval_script` with `NoSignatureChecker` and `script_sigop_count(_, true)`. Both are already in CS1's API.
- **CS6.**
  - Core copies the `CTxOut` into each `CScriptCheck`, because `UpdateCoins` spends coins before the queued checks run.
  - `InputCheck` borrows the spent script, so CS6 must own the coins it queues. A1 §3.3's `ScriptCheck { coin }` already does; keep it.
- **CS6 and CH3a.** In the serial path under v23.1.8, a block's DERSIG/CLTV/CSV/NULLDUMMY failure logs `non-mandatory-script-verify-flag (…)` with `BLOCK_CONSENSUS`. A1 §3.3's serial bullet names only the mandatory string; align it with CS1 §6.2.

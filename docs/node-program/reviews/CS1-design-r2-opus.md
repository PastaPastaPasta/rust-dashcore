# CS1 design review, round 2 (Opus, focused)

| | |
|---|---|
| Reviewed | `designs/CS1-script.md` revision 1 (§12 dispositions), against `reviews/CS1-design-r1-opus.md`, DEC-25 and DEC-28 |
| Reviewer | Opus 5.5, independent design reviewer. Not an EMBARGO-1 reader; no private material consulted |
| Focus | Resolution of every round-1 Opus finding. Emphasis on the performance redesign: C2 (per-input memo vs DEC-28's per-opcode wording), C3 (equivalence with Core's preimage), the `TxSigData` / per-input `!Sync` checker slots, the exact secp256k1 pins, and the revised cost estimate |
| Core re-read | v24.0.0-rc.3 and v23.1.8: `CTransactionSignatureSerializer`, `SignatureHash`, `PrecomputedTransactionData::Init`, `GenericTransactionSignatureChecker::CheckSig`, `GetScriptOp`, `FindAndDelete`, `CheckInputScripts` (cache probe, `txdata.Init`, forward declarations), `InitSignatureCache`/`InitScriptExecutionCache` |
| Also read | `secp256k1` 0.33.1 (`Cargo.toml`, `ecdsa::verify`), `secp256k1-sys` 0.14.1 (`build.rs`, `links`, vendored rev, vendored `configure.ac`); `node/integration` `Cargo.lock`/`Cargo.toml`; SH1 branch `dash-sha256/src/lib.rs` (`Sha256: Clone`); A1 §3.1 pool table and App. A; `/work/scratch/cs1/c3_share.{py,log}` |
| Measured | ECDSA verify cycles, Rust vs Core, interleaved and pinned. Rust built with and without `USE_ASM_X86_64` and `-O2`. Raw output: `/work/scratch/cs1-r2-opus/{rust_perf,core_perf,rust2,core2,lev}.txt` |

Severity scale as in round 1: **Blocker** means the design is wrong. **Major** means it must change before the affected phase starts. **Minor** goes into the implementation brief. **Nit** is wording.

## Verdict: APPROVE-WITH-CHANGES

There are no blockers, and every round-1 Opus finding is resolved. The performance redesign is sound where it touches correctness:
- **C3 is byte-identical to Core's preimage** (§2.1), proved against the v24 serializer for every hash-type class and every scriptCode.
- **C2's widening to a per-input memo is exact** (§2.2).
- **The API slots are correct and complete** (§2.3).
- **The exact pins do what they claim** (§2.4).

The revised estimate is right **in instructions**. I reproduce its arithmetic, and the C3 byte model includes the per-tx prefix pass.

**One new major, M2-1.** The estimate's headline and §0.6 treat per-signature ECDSA as "at parity", and ECDSA is 67–79% of the per-input cost. That parity holds in instructions only. In three interleaved, CPU-pinned series, the Rust verify path ran **4–25% more cycles** than Core at near-equal instructions. The cause is lower IPC. The box was heavily loaded, so the size of the gap is uncertain. Its sign matched in every summary statistic, in all three series. If the gap is real:
- the mainnet gain shrinks from −15% to roughly −2…−9% in cycles;
- ECDSA-bound history (testnet-sized transactions, multisig, CHECKDATASIG) becomes slower than dashd;
- CS1's cycle-gated ECDSA rows fail, and the design has no remedy path for them.

The fix is cheap: measure early, in a quiet window, and name the levers and the fallback before phase C. Minors and nits are in §3.

---

## 1. Round-1 Opus findings: status

| ID | Round-1 finding | Status | Verified at |
|---|---|---|---|
| M-1 | Sighash share underestimated; C2/C3 optional; no Core-side sighash rows | **Resolved**, with a new caveat (M2-1) | §0.6, §8.2–§8.5, §9, §11 N2/N4/N5. SH1 is now the backend prerequisite. C2 and C3 are required, with exact designs and twins. Core-side `SighashAll_*`, real-block, multisig, quadratic and CHECKDATASIG-chain rows were added. The 2× shortfall is declared. `-par` is no lever at equal threads (§8.1). The real-block row is a `bench_dash` patch, not the `-debug=bench` timing I proposed: better, because it can be instruction-counted |
| m-1 | `get_op` failure cursor | **Resolved** | §4.3 matches `GetScriptOp` (`script.cpp:259-301`) exactly: a partial PUSHDATA2/4 length field is never consumed. The quirk-39 matrix (§5.2) covers every truncation form × {none, CODESEPARATOR, FindAndDelete hit, `ab` inside push data} × three operations × bare/P2SH × two signing targets |
| m-2 | FindAndDelete doc | **Resolved** | §4.3's pseudocode is `interpreter.cpp:224-247` line for line, including `pc2` not moving on failure and the empty-pattern return. I checked the restricted idempotence proof: it is sound, because a complete one-instruction pattern fixes its own extent, and the first pass tested each kept start with the same bytes |
| m-3 | Test-plan gaps | **Resolved** | §7.2 ports all six tests, adds the `SignatureHashOld` twin with v3 special types (§7.3), and documents the expected quirk-39 disagreement on malformed tails. Also added: a CHECKDATASIG positive template, ECDSA edge seeds (with length-of-length 0–9) and an `llvm-cov` branch census |
| m-4 | P2SH sigop form | **Resolved** | §4.10 is renamed to `script_sigop_count_with_sig` and has both branches. "Final successful `get_op` call's data" is stated, and the tx-level P2SH filter is explicit |
| m-5 | CHECKMULTISIG op count | **Resolved** | §4.5 lists all 11 steps. Steps 2–7 are exactly the order I gave |
| m-6 | libsecp256k1 skew, not pinned | **Resolved** (one factual correction, m2-4) | §2's table, §3's exact requirements, and the full-budget re-run rule for a bump |
| m-7 | No C2/C3 API slots | **Resolved, with a modified fix that I accept** | §4.8 `TxSigData`, §4.9 per-input `!Sync` checker, §4.11. See §2.3 |
| m-8 | `oracle_check_input_scripts` adapter | **Resolved** | Re-checked on both pins. v23 has `void InitSignatureCache()` / `InitScriptExecutionCache()`. v24 has `[[nodiscard]] bool …(size_t)`. `CheckInputScripts` is forward-declared only in `validation.cpp` (v23:165, v24:199), with the `pvChecks = nullptr` default. The cache probe precedes `txdata.Init`. Store-nothing, `cs_main`, and `ORACLE_E_ARG` for duplicate prevouts and −1 values are all specified |
| m-9 | Real-chain coverage | **Resolved** | §7.6 has the coverage table, the full testnet census with a positive control, and follow-up N7 |
| n-1 | Settle the PLAN §7 split | **Resolved** | §4.8 states the split; §11 N6 asks for confirmation at approval |
| n-2 | Bookkeeping | **Resolved** | All five sub-items are done |
| §3 notes | MP1a tip flags, AreInputsStandard, CS6 coin ownership, A1 serial bullet | **Resolved / forwarded** | §4.11, §6.2, §11 N5/N8 |

---

## 2. Focused verification

### 2.1 C3: equivalence with Core's preimage

Reference: `CTransactionSignatureSerializer` and `SignatureHash`, v24.0.0-rc.3 (re-read for this round).

Core's preimage for `(scriptCode, tx, nIn, ht)`, with `ACP = ht & 0x80`, `SINGLE = (ht & 0x1f) == 3` and `NONE = (ht & 0x1f) == 2`:

1. `assert(nIn < vin.size())`. If `SINGLE && nIn >= vout.size()`, return ONE.
2. `int32 nVersion | nType << 16`.
3. `cs(ACP ? 1 : N)`. For each serialized input `j` (only `nIn` under ACP):
   - `prevout_j`;
   - `j == nIn ? SerializeScriptCode : 00`;
   - `j != nIn && (SINGLE || NONE) ? int{0} : nSequence_j`.
4. `cs(NONE ? 0 : SINGLE ? nIn+1 : M)`, then each output, with `CTxOut()` at other indices under SINGLE.
5. `nLockTime`, then the payload under the same condition as `CTransaction::Serialize`, then `int32 ht`. SHA256d.

**The regime.** C3's regime is `!ACP && !SINGLE && !NONE`. In it, step 3 writes `cs(N)` and, for every `j ≠ nIn`, `prevout_j ‖ 00 ‖ nSequence_j`, which is exactly `write_input(prevout_j, &[], seq_j)`. Step 4 writes `cs(M)` and every output. So the plain preimage is literally:

`B[..o_i] ‖ prevout_i ‖ SSC(scriptCode) ‖ seq_i ‖ B[o_{i+1}..] ‖ ht_le32`

Here `B` is §8.3's blank serialization: the same `canonical` writers, `o_N` at the output count. C3 hashes `checkpoint_i = SHA256-state(B[..o_i])` and then the remaining bytes. SHA-256 streaming over a concatenation equals one-shot hashing of it, and SH1's `Sha256: Clone` copies `s`, `buf` and `bytes` (checked on the SH1 branch). So the digest is identical. **QED for the regime.**

Every other class takes the plain path, the reference twin itself, so there is nothing to prove:

| Class | Path | Why it is exact |
|---|---|---|
| ALL-acting: 0x00, 0x01, 0x04–0x1f, and 0x20–0x7f with low 5 bits ∉ {2,3}; also negative or wide `i32` values with bit 7 clear (e.g. `i32::MIN`), reachable only through the public `i32` API | C3 | proof above. Core derives the flags from the full `int` the same way |
| SINGLE, `nIn` in range | plain | same function |
| **SINGLE, `nIn` out of range** | plain → ONE before any serialization | identical. C2 then caches ONE, which is still exact (deterministic) |
| NONE, with or without ACP | plain | same function |
| ACP (any base) | plain | same function |
| N = 1 | plain (no checkpoints are built) | same function. **State it** in `signature_hash_with`'s doc (nit n2-5) |

**Every scriptCode.** FindAndDelete runs in the interpreter before `check_sig`, so both paths receive the same post-deletion bytes. CODESEPARATOR stripping, the compact size `size − nCodeSeparators` and the quirk-39 truncated tail all live in `serialize_script_code`, which both paths call on the same input. Neither the compact size nor any tail length interacts with the midstate, because the hash is over a byte stream. So equivalence holds for every scriptCode, malformed ones included.

**The payload** is in `B`'s suffix and is written by the same `write_extra_payload`. The version/type field is per-tx. Neither depends on `nIn`.

**Verification is adequate.** The twin, `xcheck` inside `signature_hash_with`, `diff_sighash`'s all-inputs mode and C3's property test cover it. One requirement for those tests: the random hash types must reach the less obvious ALL-acting bytes (0x00, 0x04, 0x20, 0x41, 0x7c) and the `i32` extremes, not only 0x01 (nit n2-6).

**Cost model check.** `c3_share.py` charges the prefix pass once per tx (`4 + cs(N) + 41(N−1)`) and charges each input `66 + 41(N−i−1) + rest + 4`. So the 45.6% / 46.7% savings already net out the build cost. The plain figure reproduces my round-1 4,301 B/input.

### 2.2 C2: is the per-input memo sound, given DEC-28 says "per-opcode"?

**Soundness: yes.** `GenericTransactionSignatureChecker::CheckSig`:
1. returns false on an invalid pubkey before any hash;
2. takes `nHashType = vchSig.back()`, which is in 0..=255;
3. calls `SignatureHash(scriptCode, *txTo, nIn, nHashType, amount, BASE, txdata)`.

Dash's `SignatureHash` ignores `amount` and `cache`. So for a fixed `(tx, nIn)`, the digest is a pure function of `(scriptCode bytes, hash-type byte)`. The checker is built once per `(tx, n_in)` (`InputCheck::run`), so a memo with that key and that lifetime can never return a value Core would not compute. The verifier, and with it the sigcache, is still called for every (sig, key) pair, so nothing observable changes. A classification re-run builds a fresh checker, which is harmless.

**Relation to DEC-28(b).** A per-input memo covers every case a per-opcode memo is correct for: inside one CHECKMULTISIG the scriptCode is fixed after the FindAndDelete loop, so the per-opcode key collapses to the hash type. It also removes repeated CHECKSIG sighashes across opcodes and across the scriptSig, scriptPubKey and redeem-script evaluations.

One precision. "Subsumes" is true of the *scope*, not of the *capacity*. With an LRU of 4, a 20-signature CHECKMULTISIG whose signatures cycle through 5 hash types misses every time, where an uncapped per-opcode memo would hit 15 times. It is never worse than Core, which recomputes every attempt, and SH1+C3 carry the worst case. §8.3 already says PF may tune the count. This is a wording point (n2-2), not a soundness one.

**Recommendation.** Ask the manager to amend DEC-28(b)'s wording to "per-input sighash memo (subsumes per-opcode)", so that the decision log and the design agree. That is bookkeeping, not a change to CS1.

### 2.3 API slots: `TxSigData` and the per-input `!Sync` checker

- **`TxSigData<'a>`** borrows the tx, so it cannot be mispaired. It is `Sync` provided its phase-C state is `Sync`: midstates are plain data, and a lazy `OnceLock<…>` is `Sync` when its contents are `Send + Sync`. It appears exactly where Core passes `PrecomputedTransactionData&`. I confirmed Dash's `Init` is inert on both pins: it only moves `spent_outputs` and sets `m_ready`. So phase B is faithfully inert.
- **`TransactionSignatureChecker`** holds `&TxSigData` (Send, since `TxSigData: Sync`), `&V` with `V: SigVerifier: Sync` (Send), and a `RefCell<SighashMemo>` (Send, !Sync). So it is **Send + !Sync**, as stated. Declaring the `RefCell` field in phase B is the right way to fix the auto traits before callers exist.
- **`InputCheck`** is `Send + Sync` and creates the checker on the worker. That is the correct split. Nothing in CS1's own `SignatureChecker` trait or in `eval_script`'s bound requires `Sync`. A1 App. A's trait has no `Sync` supertrait either.
- **Gaps (nits):**
  - add a compile-time assertion that the checker is `!Sync` and `InputCheck` is `Send + Sync` (n2-3), so that nobody "cleans up" the unused phase-B field;
  - keep the memo's `borrow_mut` scoped so it is not held across `verifier.verify` (n2-3).

  Neither changes the API.

### 2.4 The exact secp256k1 pins

- `node/integration`'s `Cargo.lock` holds one copy each: `secp256k1` 0.33.1 and `secp256k1-sys` 0.14.1.
- `secp256k1` 0.33.1 requires `secp256k1-sys = "0.14.0"`, a caret requirement, so the `-sys` pin is **necessary**: the facade's pin alone would let `-sys` float within 0.14.x.
- `secp256k1-sys` declares `links = "rustsecp256k1_v0_14"`, so two 0.14.x copies cannot coexist. `=0.14.1` therefore fixes the one vendored libsecp256k1 (rev `6e2c8bc4`, 0.8.0) for the whole graph. 0.14.1 is the newest release on crates.io today.
- If base-sdk's `dash-pkc` or `bitcoin-crypto` ever asks for `^0.33.2`, resolution fails loudly. That is the desired behaviour.
- No unused-dependency lint or `cargo-machete`/`udeps` check exists in the tree, so a direct, otherwise-unused `secp256k1-sys` dependency is fine. Give it a comment saying why it is there.

**Pins: correct.**

### 2.5 The revised cost estimate

The arithmetic in §8.2 is right: 574k, 659k (+15%), 531k (−7%), 580k (+1%), 489k (−15%), 1.49× ceiling. Its inputs are my round-1 instruction counts and the C3 byte model checked in §2.1. So **"−15% instructions per input on the 700k–819k mainnet sample, P2PKH, SIGHASH_ALL, interpreter excluded"** is a fair claim.

It is not a cycles or wall-time claim, and §0.6 and §8.2 item 3 read as if ECDSA cost were settled. It is not (M2-1).

---

## 3. Findings

| ID | Sev | Section | Problem | Fix |
|---|---|---|---|---|
| M2-1 | **Major** | §0.6, §8.2 item 3, §8.4, §8.5 | ECDSA "parity" is shown in instructions only. Measured cycles lean 4–25% worse for Rust. This sets the cycle outcome of the whole per-input estimate and of the ECDSA-bound rows, and there is no remedy path | Detail below |
| m2-1 | Minor | §8.4 gate | "Every row except the ECDSA ones" is ambiguous. `VerifyInput_CheckDataSigChain` is ECDSA-bound with no sighash to offset +1.7% instructions, so it fails an instruction gate by construction | List the cycle-gated rows explicitly: `ECDSAVerify`, `ECDSAVerify_LargeBlock`, `VerifyInput_CheckDataSigChain`. For the mixed rows, record the ECDSA instruction headwind in the merge report |
| m2-2 | Minor | §8.4 Core patch | The workloads are not like for like. Core's `CScriptCheck` always runs a `CachingTransactionSignatureChecker`: a salted SHA-256 of about 200 bytes plus a CuckooCache probe per signature, even with `cacheStore = false`. The Rust side uses `DirectVerifier`. Also, nothing says Rust's timed region includes `TxSigData::new`, which Core's inert `Init` makes free | Core bench: call `VerifyScript` with a plain `TransactionSignatureChecker`, or give Rust a lookup-only CS6-shaped verifier. Rust: time `TxSigData::new` inside the per-tx loop |
| m2-3 | Minor | §8.3 "Who builds `TxSigData`" | An eager build "where Core calls `txdata.Init`" puts O(input section) hashing on the serial, `cs_main`-held path of `ConnectBlock`, which Core leaves empty. It also wastes a pass on transactions that never compute a regime sighash: OP_TRUE or hash-locked spends, all-ACP or NONE inputs. That is up to one extra pass over the block (~25 ms for 2 MB at 37 instr/B), with no Core counterpart | Make the default lazy: a `OnceLock` built by the first C3-regime `signature_hash_with`, on the worker. Or build on the `prevalidate` pool, as A1 §3.1 says. Either way, CS6's equal-thread benches must include the build |
| m2-4 | Minor | §2 table, §4.7 | "built by `cc` with no asm (0.8.0 has none)" is wrong. libsecp256k1 0.8.0 still has x86_64 **scalar** inline asm (`scalar_4x64_impl.h`, `USE_ASM_X86_64`), and its own `configure` enables it by default on x86_64. `secp256k1-sys`'s `build.rs` does not define it. I measured the effect at −400 instructions/verify (−0.1%), so it does not matter for performance. It is still a build-config difference that §2 should record | Correct the text: "0.8.0 built by `cc` without `USE_ASM_X86_64` (scalar asm off; libsecp's configure would enable it); no field asm exists in 0.8.0" |
| n2-1 | Nit | §11 N2 | "A single-stream rate at least Core's SSE4 transform (36.8 instructions/byte)" mixes a rate with a cost | "≤ 36.8 instructions/byte" |
| n2-2 | Nit | §8.3 C2, §12 | "Subsumes the per-opcode case" holds for scope, not capacity (§2.2). Also, compare the hash-type byte and the length before `memcmp`ing up to 10 kB of scriptCode, so that the adversarial cycling-hash-type case misses in O(1) | Reword; fix the key-compare order in the brief. Manager: amend DEC-28(b)'s wording |
| n2-3 | Nit | §4.9 | Nothing pins the auto traits or the `RefCell` borrow discipline | Static assertions (`InputCheck: Send + Sync`, checker `Send + !Sync`); drop the memo borrow before `verifier.verify` |
| n2-4 | Nit | §4.8 `xcheck` | Cargo unifies features, so one crate enabling `xcheck` turns on double hashing and panics in production builds | A release-build check that `rdashd` is built without `xcheck`, or a `compile_error!` under `not(debug_assertions)` unless an explicit opt-in is set |
| n2-5 | Nit | §4.8 | The N = 1 fallback to the plain path is implied, not stated | State it in `signature_hash_with`'s doc |
| n2-6 | Nit | §7.2, §7.4 | The C3 property test and `diff_sighash`'s all-inputs mode must reach every ALL-acting class (§2.1 table), not only 0x01 | Bias the hash-type generator to those classes and the `i32` extremes |
| n2-7 | Nit | §8.4 | `SighashAll_1In` gets nothing from C3, and Rust adds per-field write overhead, so its instruction gate has zero margin if SH1 only equals Core's transform. SH1's backlog item BL-18 (the AVX2 2-block transform) is the margin | Note the dependency, so that a narrow fail is read as an SH1 issue, not a CS1 one |

### M2-1 (Major): ECDSA parity is shown only in instructions

**Evidence.** `cs1rbench verify` (round-1 crate, `CPubKey::Verify` shape, 100 uncompressed keys, 20,000 ops) against `bench_dash -filter=^ECDSAVerify$`. The runs alternate on one pinned CPU. Instructions are deterministic; cycles are from `perf stat cycles:u` (Rust, whole process, setup < 0.5%) and from `bench_dash`'s own counter (Core). The load average was 63–79 on 32 vCPUs, so cycles are noisy, and min/median are reported.

| Series | Rust instructions/op | Core instructions/op | Rust cycles (min / median) | Core cycles (min / median) | Rust vs Core (min / median) |
|---|---|---|---|---|---|
| 1, vs v24.0.0-rc.3, CPU 5, 8 rounds | 385,448 | 378,0–379,1k | 166k / 237k | 149k / 221k | +12% / +7% |
| 2, vs v23.1.8, CPU 11, 12 rounds | 385,448 | (379k, round 1) | 160k / 196k | 134k / 163k | +19% / +20% |
| 3, vs v23.1.8, CPU 13, 8 rounds (Rust `-O3` default) | 385,447 | — | 179k / 202k | 153k / 195k | +17% / +4% |

Variants in series 3:
- Rust with `CFLAGS=-O2`: 195k / 221k, no better.
- Rust with `-O2 -DUSE_ASM_X86_64=1`: 164k / 200k, inconclusive.

Instructions differ by +1.7%, so **the cycle gap is an IPC gap** (about 2.3–2.4 for Rust against 2.6–2.8 for Core in the cleaner runs). Plausible causes, none isolated:
- Core's 0.3.3-dev hand-written field asm against 0.8.0's C;
- `cc`'s flags or code layout;
- the `with_global_context` path.

The round-1 noisy numbers (Rust 170–245k against Core 145k) point the same way.

**Why it matters.** In cycles (Core SHA-256 at about 14 cycles/B, SH1 assumed equal; ECDSA at 150k cycles for Core):

| Rust ECDSA vs Core | mainnet 700k–819k per input (Core 224k) | testnet sample per input (Core 157k) |
|---|---|---|
| parity (the design's assumption) | 190k, **−15%** | 155k, −1% |
| +10% | 205k, −9% | 170k, **+8%** |
| +20% | 220k, −2% | 185k, **+18%** |

At +10% or more, ECDSA-bound validation is **slower than dashd**: small-transaction history, multisig after C2, CHECKDATASIG chains. That is against NFR-PERF-01. The cycle-gated `ECDSAVerify*` rows, and `VerifyInput_CheckDataSigChain` once m2-1 classifies it, would then fail CS1's bench gate. The design names no owner or lever for that case. §8.5's REL-6.3 text covers the 2× target, not a *slowdown*.

**Fix (before phase C starts; the measurement can run now, because it needs only the crate):**
1. Replace "Per-signature ECDSA cost is at parity" (§0.6) with "at parity in instructions (+1.7%); cycles unmeasured, early evidence leans slower (this review)". Label §8.2's −15% as an instruction figure, and add the cycle-sensitivity rows above.
2. Run the ECDSA cycle comparison in the **next manager-reserved quiet window** (DEC-23(b)), not at the end of phase C. Use the same Rust loop that CS1's `ECDSAVerify` row will use, against both pins.
3. If Rust is slower, name the levers and an owner (PF, or a CS1 follow-up). In rough cost order:
   - `cc` flags and compiler for `secp256k1-sys` (`-O2`/`-O3`, `USE_ASM_X86_64`, clang, cross-language LTO);
   - an explicit `Secp256k1<VerifyOnly>` in `DirectVerifier` (already allowed by §4.7);
   - a `-sys` build that matches Core's configure output;
   - last, a per-pin-matched vendored libsecp256k1 under CS1's version-bump rule (full-budget mode (b) and quirk 7).
4. If no lever closes it, the manager decides whether it becomes a declared library-bound deviation from NFR-PERF-01 (its own DEC entry, distinct from DEC-28(d)) or blocks the bench gate. Either way, decide it before the phase-C effort is spent on the assumption that ECDSA is free.

---

## 4. Answers to the brief's questions

- **C2, per-input memo against DEC-28's per-opcode wording: sound.** It is exact for the key `(scriptCode bytes, hash-type byte)` at per-`(tx, n_in)` scope, and never worse than Core. The deviation is a strict widening of scope. DEC-28(b)'s wording should be amended (n2-2).
- **C3, the per-tx sighash prefix cache: provably equivalent.** It is identical to Core's preimage for the ALL-acting regime, for every scriptCode, including after FindAndDelete or CODESEPARATOR and with the quirk-39 truncation. SINGLE out of range (ONE first), NONE and ACP never enter C3 and use the twin itself. N = 1 uses the plain path (state it).
- **API slots: correct.** `TxSigData` is `Sync` and mirrors Core's (inert) `PrecomputedTransactionData`. The checker is per input, `Send + !Sync`, fixed in phase B. `InputCheck` is `Send + Sync`.
- **The secp256k1 pins: correct and necessary.** The facade's own `-sys` requirement is a caret. Correct the "0.8.0 has no asm" wording (m2-4).
- **The ~−15% estimate:** correct in instructions, and it already nets out C3's build pass. As a cycles claim it is unproven and the evidence leans worse (M2-1).

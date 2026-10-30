# LQ1: quorum member selection, DIP24 rotation and quorum selection (`dash-evo::llmq`)

| | |
|---|---|
| Status | **rev4 (final round, minors only), after design review r4** (`reviews/LQ1-design-r4-gpt.md` = GPT r4, APPROVE-WITH-CHANGES, on rev3 at a0644ac6: all r3 findings closed, two minors). rev3 followed GPT r3 (`reviews/LQ1-design-r3-gpt.md`, on rev2 at d44631bb); rev2 followed GPT r2 (`reviews/LQ1-design-r2-gpt.md`); rev1 followed review r1 (`reviews/LQ1-design-r1-opus-core.md` = A, `reviews/LQ1-design-r1-opus-arch.md` = B). Dispositions: §10. Stop point: no code until the manager approves rev4 |
| Date | 2026-10-08 (r0 morning; rev1 afternoon; rev2, rev3 and rev4 evening) |
| Author | LQ1 designer (Opus 5.5 xhigh) |
| Task | LQ1a: non-rotated member selection, the quorum modifier before and after V20, `scan_quorums` (ancestry-resolved, with Core's cache modelled), `select_quorum_for_signing`. LQ1b: DIP24 rotation (quarters, skip lists and their modes, snapshots as effects emitted before any commitment check, the first V20 cycle). DEC-29 D4: the `std::log2(signingActiveQuorumCount)` call in `llmq/quorumsman.cpp` |
| PRD | FR-LLMQ-01 (enabled-type and rotation-enable rules; spork predicates), FR-LLMQ-02 (all of it), FR-LLMQ-05 (`ScanQuorums`, its pure part; the cache is QM1's) |
| Binding inputs | A1 §2.5, §2.10.1, §2.10.2, §2.25, §3.6, §3.10, §3.11, §4.4, §4.5, §4.6, Appendix A; PLAN §0, §4.4 rows LQ1a/LQ1b, §8.2; R4 §1.9.3, §1.15 quirks 28–30, appendix E §2.4–§3.4, §6.2, §11.3; O17 `llmq-dkg` (DKG-07, -08, -12, -13, -15, -16), `evo` (EVO-06, EVO-18), O17-pin-delta.md policy (l.118, l.189), `O17-release-profile.md`; DEC-29 D4, DEC-33(d), DEC-35 D10, DEC-37, DEC-41; `CS2-fp-release-binaries.md` §3.4, §7 F1; `CS0-node-primitives.md` §8.3–§8.4; **`EV2-dml.md` (the reconciled EV2 design)** §2.4–§2.6, §2.10–§2.12, §3.8–§3.11; the manager's rev1 rulings |
| Code verified at | Dash Core **v23.1.8** (728f5055, the mainnet pin) and **v24.0.0-rc.3** (0364ed62, the testnet pin), every claim by `git show <tag>:<path>`; rust-dashcore `dev` 67b307c6, `node/integration` 276685fc (O4 fixtures, `dash-spv` `sml_engine`), `node/pf0-counter-gate` (perfgate); base-sdk 33fcc59 |
| Evidence | `/work/scratch/lq1/`: both pins' sources (`src/v23`, `src/v24`); the `log2` evidence (`fp/`: all 18 call-site listings incl. the win64 ones, the win64 routines, the libm probes, `glibc-bridge/` with `SUMMARY.md`, `glibc-2.39-ubuntu2404.txt`, `binaries.sha256`); the r0 testnet probe (`rotationinfo-testnet-probe.txt`; see §6.3.6 for its classification); the r0, rev1 and rev2 texts (`LQ1-quorum-members.{r0,rev1,rev2}.md`); the row-digest known-answer vectors (`digest_kat.py`, `digest_kat.out`) and their cross-check by Core's own serializers (`corekey_main.cpp`, `corekey.out`). The GPT reviewers' harnesses are in `/work/scratch/lq1-r2-gpt/` (scan/cache and snapshot-builder models) and `/work/scratch/lq1-r3-gpt/` (members cache with precompute and undo, scan materialization trace, per-pin read traces, overlay model, digest ambiguity) |

Core anchors are written `v23 file:line / v24 file:line`. Paths are under `src/`.

## Changes in rev4

1. **The Platform type is non-rotated only by default (GPT r4 finding 1).**
   - Devnet's `-llmqplatform` accepts a rotated type, as does `-llmqmnhf`, and neither has the rotation guard `-llmqchainlocks` has.
   - So M4 validation can reach rotated member computation and row effects, through asset unlocks (a scan of `N + 1` plus an explicit `GetQuorum`) and MNHF (an explicit `GetQuorum`) (§3.8.4).
   - CH7b now owns the production scan facade and scan cache, alongside the materializer and quorum-object cache, for every configured type validation can select.
   - CH7b's scan-suite acceptance includes the rotated Platform override, with v24's bound-chain bypass and the cached scan that follows it.
   - QM1a extends the same runtime caches (§6.6, §7, §8 Q7, §9).
2. **The DKG path's second members access is kept (finding 2).**
   - The materializer performs the second `GetAllQuorumMembers` access exactly when Core's contribution path reaches `BuildQuorumContributions`, and its `MEMBERS` event is part of the trace. The permission to omit it is removed.
   - Core takes this path on a node with a DKG session manager: a masternode or a `-watchquorums` observer.
   - The `populate_cache` sentence is narrowed to the initial build access, the row writes and the object-cache insert (§3.8.3).
   - QM1a's DKG-configuration suite compares Core traces only against an oracle with the matching DKG configuration. Otherwise it is marked non-DKG and the DKG access stream is tested separately; the event is never normalised away (§3.8.3, §6.2, §6.6).

## Changes in rev3

1. **Precompute goes through the process-wide members facade (GPT r3 finding 1, ruling 1).**
   - CH7b owns the runtime entry point `QuorumPrecompute::precompute(block, mode, effects)`, with `mode` `Connect` (reset false: connect and template) or `Undo` (reset true). LQ1 defines the trait in `dash-evo`. LQ2's `process_block` (after its DIP3 gate) and `undo_block` call it first (§2.5, §2.8).
   - Each eligible type reaches the same cached facade that RPC, DKG, commitment and QM1 callers use (§3.7.4).
   - The facade keeps Core's order, from the gate through the indexed-cache step (§3.7.4).
   - Cache mutations are process state, not attempt state: they survive a failed or `fJustCheck` attempt. Undo resets only eligible types at cycle-base heights.
   - The pure layer only enumerates targets (`precompute_targets`) and supplies the routing steps (`members_gate`, `members_route`).
   - The uncached composition is now a named reference primitive (`reference::{get_all_quorum_members_uncached, precompute_uncached}`) that production never calls.
   - LQ1 ships a conformance suite. CH7b must pass it against the oracle with the production facade, including the cold, warm and undo fixtures (§6.2, §6.6, §7).
2. **Scans materialize quorums exactly as Core does (finding 2, ruling 2).**
   - `scan_quorums_from` takes QM1's `QuorumMaterializer` (Core `GetQuorum`, both overloads). It calls it for every newly scanned base, in order, including bases beyond the returned prefix, with Core's `populate_cache` decision (§2.5, §3.8.1).
   - The scan cache retains QM1's quorum objects, so a hit makes no `get_quorum` call and no member access. The object cache's hits are part of the contract (§3.8.3).
   - On a later `None`, the scan returns empty and does not write the scan cache, but earlier builds keep their effects.
   - Cross-cache differential sequences compare `GetQuorum`, members-cache and scan-cache event traces plus written rows, not only returned bases. They cover a cold scan, a hit, a continuation, a discarded tail and a later failure. The oracle scan runs on the context's real shared managers, with DEC-16 record-only trace hooks (§6.2).
   - New question Q7: who lands the materializer core before QM1a (§8).
3. **The L4 read order is qualified, not claimed exact (finding 3, ruling 3).**
   - §5.3 now states each pin's actual order.
   - Rust does not port either order: no read in the sequence has an effect that outlives a fail-stop abort. Each case is checked (EV2 list cache, snapshot LRU, L5, L1, members cache, the computation's single write).
   - Tests assert the set of reached inputs and the stopping markers, not the sequence.
4. **One canonical row digest (finding 4, ruling 4).**
   - Core's serialized `llmq_S` keys, sorted as unsigned bytes, with CompactSize framing over the whole namespace. An empty store hashes the single byte `0x00`.
   - Known-answer vectors, cross-checked with Core's serializers.
   - A coherent read boundary, read without warming caches or creating rows.
   - One event framing for written rows and traces, captured per call.
   - `reset_cache` is documented as a per-type members-cache reset behind Core's gates (§3.7.4, §6.2).
5. **Expected absence (finding 5, ruling 5).**
   - LQ1 §2.6 drops the dead "commitment of a base the mined index lists" example. The member computation reads no commitment, and a scan's missing commitment is Core's `nullptr` (§3.8.1).
   - The replacement text for EV2's copy goes to the manager in the rev3 report. EV2 itself is not edited here.

## Changes in rev2

1. **Snapshot rows follow Core's exact write semantics (GPT M1, ruling 1).** LQ1 reads and writes rows only through `SnapshotRows`, which has `CQuorumSnapshotManager`'s semantics: an unconditional overwrite keyed by the cycle-base hash, visible to the next read, including later reads in the same connect attempt (§2.3, §3.7.2). Writes happen at Core's write points, because the runtime members cache is now a Core-faithful port whose warm or cold state decides when a row is (re)written (§3.7.4); rev1 called it unobservable. rdashd's stored rows therefore equal Core's in every state, including states in which a recomputed modifier differs from the one used at an earlier write. rev1's write-if-absent sink, its equality assertion, the "rewrites are identical" claim and the no-overlay assertion are removed; verification compares row digests (§6.2, §6.6). Deviation D3 is withdrawn.
2. **`log2(32)` on Apple is an open evidence gap (GPT M2, ruling 2).** Gladman et al.'s 0.515 ulp is a largest *known* error, not a proven bound, so there is no parity claim for macOS-hosted dashd until a measurement exists. §3.10.5 specifies the one-file check (prints `(int)log2(32.0)` and `(int)log2(2.0)`, built with Apple's toolchain against the system libm) and what changes if it prints 4.
3. **The L4 key is built by Core's read sequence (GPT m1).** Current inputs first, then per prior cycle: ancestor, row, and only then that cycle's list and modifier; stop at a missing ancestor or row; nothing past the stopping point is read (§5.3). First-cycle and unreached-read-error tests (§6.1).
4. **Store-error documentation (GPT m2).** Universal fail-stop is the manager's ruling, with v24's typed `EvoDbInconsistencyError` as precedent (other v24 DML store faults still reach `failed-procspectxsinblock`); the stale EV2 cross-reference is updated (`EV2-dml.md` §2.11 now agrees); an absent row (`Ok(None)`) is normal, `Store` is only for unreadable data or a record an established invariant requires; both paths are tested (§2.6, §3.15 D4).
5. **Entry-state provenance is checked (GPT m3).** A rotated fixture with operator-key and service changes between an older work list and the current list; the order gate also compares each position's operator key and addresses with `quorum info`; `oracle_llmq_members` returns per-member state (§6.1, §6.2, §6.3.2).
6. **Coverage reporting.** Regtest gate runs are never reported as testnet or mainnet coverage (GPT r2 note; §6.3.2).

## Changes in rev1

1. **`ScanQuorums`' cache is modelled, not declared a deviation (A-M1, ruling).** r0's claim that cache continuation is RPC-only was wrong: on young rotated chains it reaches P2P selection (IS and recsig verification) and connection/key management. LQ1 now exposes a pure scan that takes the cache entry as input and returns the cache write (§2.5, §3.8). QM1 owns a Core-faithful runtime cache (`unordered_lru_cache` semantics, overwrite on emplace, Core's sizes and truncation), including the duplicate padding (§3.8.3). r0's Q2 is withdrawn.
2. **One shared coinbase-CL predicate (A-M2, ruling).** r0's modifier-only profile bit is replaced by one predicate with Core's `GetNonNullCoinbaseChainlock` semantics, owned by EV4 (with BS6a's CbTx decoder), with a new O17 row tied to EVO-06/EVO-18 and all five Core consumers listed (§3.3.1). LQ1 consumes it. An L2 invariant over every CbTx v ≥ 3 proves the history claim (§6.3.5).
3. **One member-source trait, owned by EV0 (B-M1, ruling).** §2.6 proposes its signature: `Arc<Dmn>` entries, an `EffectSink`, and a `MemberError` whose `Store` variant is a fail-stop node error, never `failed-procspectxsinblock` (Core v24 `evo/specialtxman.cpp:989-992`). The entry-provenance contract (rotated members carry their old state) is part of the trait doc (B-m2). r0's "EV2 needs no change" is withdrawn.
4. **Rotation memo keyed on derived inputs, with an exactness proof (B-M2, ruling)** (§5.3 L4). It lives in `dashd-chainstate`, never caches an `Err`, and still emits the effect under each new cycle-base hash.
5. **The never-slower check is set up (B-M3, ruling).** A Core bench on a DEC-16 local branch, owned by the LQ1 implementer in **LQ1a-A**, runs over the O2a-LQ1 fixture loader. It is compared first in LQ1a-B (not only in the last phase). Dependencies on PF0 and on the O2a-LQ1 follow-up are stated (§5.4).
6. **Verification gates corrected (B-M4, ruling).** The `membersSig` check certifies the signer key multiset only. `quorum info` over all quorums is the member-order gate. dash-spv disagreements are classified only when the fixture's trace shows the dash-spv gap fired (§6.3, §6.4).
7. **Operations (B-M5, ruling).** No reflinked copy of a running oracle. Heavy or writing digests run on a replica built the O7 way; only provably pure reads touch the live oracles, throttled (§6.3.6). r0's own testnet probe is reclassified: it was not a pure read (§6.3.6).
8. **dash-spv cross-check inverted (B-M6, ruling).** LQ1 writes fixture files; an in-crate dash-spv test checks them (§6.4).
9. **Dependencies.** LQ1a no longer needs BS8 (the commitment is an associated type of the mined-commitment index, §2.5); LQ1b needs EV2a (not EV2b) and i:BS8. LQ1a owns `is_quorum_type_enabled`, including v24's two override parameters, with an oracle check (§3.2, §6.2). LQ2 depends on LQ1b-E, not all of LQ1b (§7).
10. **Minors.**
    - Quirk 30b's rationale is corrected (§3.9).
    - `log2` evidence: the win64 call-site listings are archived; a glibc 2.29–2.43 source bridge and a 2.39 measurement are added; the count-2 gap is listed honestly (§3.10).
    - Pre-V19 reconstruction differs from the original new quarter (§3.6.3, 28d).
    - The start-height helper takes `i32` with Core's semantics (§2.5).
    - The decoder bound is corrected (§6.5).
    - Default regtest's pre-DIP3 rotation-enabled cycles and the v24 dual chainstate are added to "not reached" (§3.7.1).
    - Shim constraints are listed (§6.2); the cache mechanics are stated (§3.8.3).
    - A lean scratch set with `MnList::scratch()` as its twin, and a `WorkBlock` cache hint, are requested from EV2 (§2.2.4).
    - Iteration-order independence gets its own test (§6.5).
    - The L5 side table stores raw facts and falls back to a block read on a miss (§5.3).
    - L2 needs a new SHA-256 kernel task (§5.3); L6 adds a trait method (§2.5); levers live in the runtime (§5.3).
    - Runtime emissions get a defined sink; the read-your-own-writes overlay is replaced by an assertion (§3.7.2).
    - Non-vacuity counts and tip bracketing (§6.3); per-era captures plus the regtest and FT-chain captures (§6.4).
    - LQ1b-G is split into G1 and G2 (§7).
    - L2 list inputs are checked against stock `merkleRootMNList` until O8 exists (§6.3.1).
    - The adapters are re-targeted at `EV2-dml.md` names; the SML adapter's null collateral is documented; L1 memoizes sorted lists (n2); the domain check moves to `LlmqCtx` construction (n4).

---

## 0. Summary

1. **What LQ1 builds.** A line-by-line port of Core's quorum-member machinery into the pure `dash-evo::llmq` module:
   - the parameter predicates `IsQuorumTypeEnabled` (with v24's overrides), `IsQuorumRotationEnabled`, `GetEnabledQuorumParams`, `EvalSpork`;
   - the modifier (`GetHashModifier`, `GetHashModifierFromWorkBlock`), reading the work block's coinbase ChainLock through EV4's shared predicate;
   - scoring and ordering (`calculateQuorumScore`, `CalculateQuorum`);
   - non-rotated selection (`ComputeQuorumMembers`) and the DIP24 machinery (`GetQuorumQuarterMembersBySnapshot`, `BuildNewQuorumQuarterMembers`, `BuildQuorumSnapshot`, `ComputeQuorumMembersByQuarterRotation`), with the routing steps of `GetAllQuorumMembers` and the target list of `PreComputeQuorumMembers` (their caches and the facade over them are the runtime's, §3.7.4);
   - `ScanQuorums`/`ScanCommitments` as pure functions of (chain, mined index, cache entry, quorum materializer), and `SelectQuorumForSigning`/`SelectCommitmentForSigning` over a scan result.
2. **Pins.** Member selection, modifiers, rotation and snapshot content are identical at v23.1.8 and v24.0.0-rc.3 (O17 "Checked and unchanged"; confirmed by review A). The one release difference on this path, v24's stricter `GetNonNullCoinbaseChainlock`, is shared with four other consumers and now has its own O17 row (§3.3.1).
3. **No state in the pure layer; Core's write semantics.** Snapshot rows are read and written only through `SnapshotRows`, with `CQuorumSnapshotManager`'s semantics: an unconditional overwrite keyed by the cycle-base hash, visible to the next read. Inside a connect attempt a write is also recorded as `Effect::StoreQuorumSnapshot`, so it survives a later failure. Every cache and memo lives in the runtime crates; Core's members cache, quorum-object cache and scan cache are ported exactly, because their warm or cold state decides when Core (re)writes a row and what a scan returns. Every production member computation, including precompute and the quorum builds a scan triggers, goes through one process-wide members facade (§3.7.4).
4. **Neighbours by trait.** Masternode lists come through `MnListView` (six entry fields, five list operations, a scratch list with `AddMN`'s acceptance rule), which `EV2-dml.md`'s `MnList`/`Arc<Dmn>` implement. Quorum members leave through EV0's single `QuorumMemberSource` trait (§2.6), whose store errors are fail-stop.
5. **The `ScanQuorums` cache** (§3.8). Core's per-type LRU, keyed by the window-snapped store block, continues a short cached list from `back().base − 1`. On rotated chains with fewer than `count` mined commitments this appends duplicates and overwrites the cache entry, which can change rotated selection results. A miss materializes (Core `GetQuorum`) every newly scanned base, beyond the returned prefix too, so a scan warms the members cache and can rewrite rows. LQ1 ports the body, including the ordered materialization calls; QM1 ports the caches and `GetQuorum`.
6. **The `log2` site (DEC-29 D4)** (§3.10). Domain {2, 32}, all 18 call sites in 12 release builds convert exactly and truncate; measured exact on glibc 2.31/2.36/2.39/2.43 (x86_64 generic and FMA where present, aarch64 and riscv64 on 2.43) and on both win64 builds under both x87 control words; glibc's log2 data is identical across 2.29–2.43. Count 32 (mainnet, testnet) is proven for glibc (documented bound plus measurements) and win64 (measured). **Apple's libm is an open evidence gap for macOS hosts, with no parity claim**, until the one-file check of §3.10.5 runs on a Mac. Count 2 (regtest, devnet) has the same Apple gap plus unmeasured FMA-target glibc builds between 2.29 and 2.42. Rust: `u32::ilog2`.
7. **Performance** (§5). Never-slower is checked from the first members PR against a Core bench over shared fixtures. Six exact levers, each with a twin; the rotation memo (L4) now has a complete key and a proof.
8. **Verification** (§6). New oracle entry points (including `IsQuorumTypeEnabled`). L2: `quorum info` over all quorums as the member-order gate, run on an O7-style replica; the `membersSig` signer-set certificate; per-cycle snapshot rows; the CbTx-CL invariant. dash-spv checks LQ1's fixtures in its own crate. `diff_chain` effect scenarios.
9. **Plan** (§7): LQ1a in three phases (A: params, `log2`, Core bench; B: modifier, scores, non-rotated members, first Core comparison; C: scan and selection), LQ1b in five (D, E, F, G1, G2).
10. **Questions** (§8): six, each with a recommendation; PLAN row edits proposed in §9.

---

## 1. Scope and non-goals

### 1.1 In scope

| PLAN row / PRD | LQ1 delivers |
|---|---|
| LQ1a; FR-LLMQ-01 | `llmq::params`: `is_quorum_type_enabled` and `is_quorum_type_enabled_with(overrides)` (`IsQuorumTypeEnabled`, owned here and reused by LQ2, CH7b, DK1, QM1, LQ5), `is_quorum_rotation_enabled`, `enabled_quorum_params` in configured order, `eval_spork`, `signer_index_bits` (the `log2` replacement) |
| LQ1a; FR-LLMQ-02 | `llmq::modifier`, `llmq::score`, the non-rotated route of `GetAllQuorumMembers` (`members_gate`, `members_route`, `compute_quorum_members`), the members-facade contract that implements EV0's `QuorumMemberSource` (§2.6, §3.7.4; CH7b's production facade, LQ1's reference model); quirk 29 (first V20 cycle) |
| LQ1a; FR-LLMQ-05 (pure part) | `llmq::scan`: the store-block computation and Core's scan body with the cache entry and a quorum materializer as inputs and the cache write as output, calling the materializer for every newly scanned base in Core's order (§3.8); `scan_commitments` (v24 surface); `llmq::selection` over a scan result; the start-block helper; quirk 30 |
| LQ1a; NFR-PERF | the Core bench driver and the first Core comparison (§5.4) |
| LQ1b; FR-LLMQ-02 | `llmq::rotation`: quarters by snapshot (modes 0–3), the new-quarter builder with `AddMN`'s acceptance, the skip-list codec, `build_quorum_snapshot`, `compute_by_quarter_rotation`; `precompute_targets` and the `QuorumPrecompute` trait CH7b implements; the members-facade and scan conformance suites (§6.6); `Effect::StoreQuorumSnapshot` emission; quirk 28; the v24 predictor (`quorum dkginfo`, O17 DKG-15) |
| DEC-29 D4 | the `log2` domain check, the exact integer equivalent, the per-libm evidence (§3.10) |

### 1.2 Non-goals (owner in brackets)

- **The runtime scan cache** and the Core-faithful scan facade every caller goes through (**QM1**). LQ1 specifies its semantics (§3.8.3).
- **The shared coinbase-CL predicate** (**EV4**, with BS6a's CbTx decoder). LQ1 consumes it (§3.3.1).
- **The `QuorumMemberSource` trait itself** (**EV0**, types first). LQ1 proposes the signature (§2.6) and implements it.
- Mined-commitment storage, in-block commitment rules, `Verify`/`VerifyNull`, the mineable pool, `merkleRootQuorums` and its v23 cache (**LQ2**, CH7c). LQ1 defines the read trait LQ2 implements (§2.5).
- Building `CQuorum` objects (`QuorumMaterializer`, Core `GetQuorum`), the quorum object cache, contributions, warming, data recovery, `IsQuorumActive` (**QM1a/b**; the M4 runtime core, meaning the scan facade, scan cache, materializer and quorum-object cache, is CH7b's, §8 Q7). LQ1 specifies what the scan needs from them (§3.8.3).
- Persisting snapshots, applying effects, crash replay, the runtime members cache and facade, the `QuorumPrecompute` implementation and the levers' state (**CH7b**). §3.7.4 and §4 state the contract; LQ1's conformance suite checks it (§6.6).
- `qrinfo`/`quorum rotationinfo` building and the QRINFO caps (**LQ3**, P6b; O17 DKG-12/-13, RPC-39).
- `SignHash`, request ids, `VerifyRecoveredSig`, CL/IS verification (**LQ3**, LK1).
- Quorum topology (**LQ5**). The LLMQ parameter table, roles and overrides (**BS5**). The MN list, its codecs and store (**EV2a/b**). The `QuorumSnapshot` wire codec (**BS8**).
- The dual-chainstate (assumeutxo) path of v24 (NG-6; §3.7.1).

---

## 2. Data model and public API

### 2.1 Module layout

```
dash-evo/src/llmq/
  mod.rs        // re-exports; LlmqCtx (+ the domain check); LlmqError
  params.rs     // enabled/rotation predicates (+ overrides), eval_spork, signer_index_bits, cycle arithmetic
  modifier.rs   // hash_modifier, hash_modifier_from_work_block (consumes dash_evo::cbtx::nonnull_coinbase_chainlock)
  score.rs      // ScoreKey, calculate_quorum (+ reference twin)
  memo.rs       // SortedListMemo trait + NoMemo: the hook through which the runtime's L1 memo is consulted
  rows.rs       // SnapshotRows: Core's CQuorumSnapshotManager (get, unconditional store), the only path to llmq_S rows
  members.rs    // members_gate, members_route, compute_quorum_members, precompute_targets, QuorumPrecompute (trait),
                // the v24 predictor
  rotation.rs   // quarters by snapshot, new quarters, build_quorum_snapshot, compute_by_quarter_rotation
  skiplist.rs   // the skip-list encoder and decoder (quirk 28c), isolated for fuzzing
  scan.rs       // MinedCommitmentIndex, ScanEntry, QuorumMaterializer, scan_store_block, scan_quorums_from
                // (Core's body incl. continuation and materialization), scan_commitments_from, indexed_until
  selection.rs  // select_start_height, select_from_scan (rotated and non-rotated)
  #[cfg(any(test, feature = "reference"))] reference.rs
                // twins for every optimised path (§5.3); the uncached get_all_quorum_members_uncached and
                // precompute_uncached; a model members facade and a model materializer (Core's caches, §6.6)
  #[cfg(any(test, feature = "reference"))] conformance.rs
                // the members-facade and scan conformance suites, generic over the runtime's implementations (§6.6)
```

Every ported function carries its `// Core: v24 llmq/utils.cpp:161 CalculateQuorum (v23 :157)` anchor (A1 §2.1 rule 3). The pure layer holds no state (B-m7): caches and memos live in `dashd-chainstate` (the members cache and facade, L1, L4, L5) and `dashd-quorums` (the quorum-object cache, the scan cache). Snapshot rows are read and written only through `SnapshotRows` (§2.3, §3.7.2). The `reference` feature (implied by `xcheck`) exposes the uncached and model paths to tests, harnesses and the xcheck comparators; no production code path calls them (an xcheck comparator computes a twin's answer and compares it, it never returns it or stores its row).

### 2.2 What LQ1 needs from the masternode list (`MnListView`)

#### 2.2.1 Entry

```rust
/// One masternode as Core's member selection sees it (a CDeterministicMNCPtr). Cheap to clone.
pub trait MnEntryView: Clone {
    fn pro_tx_hash(&self) -> &ProTxHash;
    /// Tie-break key. Core's COutPoint operator< (hash memcmp, then n): v23 primitives/transaction.h:55 / v24 :75.
    fn collateral(&self) -> &OutPoint;
    /// MnType::Evo. Only the Platform type filters on it, non-rotated, from V19 (§3.4).
    fn is_evo(&self) -> bool;
    /// pdmnState->IsBanned(): pose_ban_height != -1 (v24 evo/dmnstate.h:146). The entry's OWN state:
    /// rotation inserts entries taken from older lists, whose state is the older one.
    fn is_banned(&self) -> bool;
    /// Only null-tested: unconfirmed masternodes are never scored (v23 llmq/utils.cpp:140 / v24 :144).
    fn confirmed_hash_is_null(&self) -> bool;
    /// The STORED field confirmedHashWithProRegTxHash (single SHA256(proTx ‖ confirmed), v24 evo/dmnstate.h:162-169),
    /// read, never recomputed (v23 llmq/utils.cpp:73-86 / v24 :72-85).
    fn confirmed_hash_with_protx(&self) -> &[u8; 32];
}
```

#### 2.2.2 List and scratch list

```rust
pub struct ListCounts { pub total: usize, pub enabled: usize }   // GetCounts().total() / .enabled()

pub trait MnListView {
    type Entry: MnEntryView;
    type Scratch: ScratchMnList<Entry = Self::Entry>;
    fn counts(&self) -> ListCounts;                    // enabled = non-banned (v24 evo/deterministicmns.h:146-155, 269-293)
    fn contains(&self, p: &ProTxHash) -> bool;         // HasMN
    fn is_pose_banned(&self, p: &ProTxHash) -> bool;   // IsMNPoSeBanned: false when absent (v24 evo/deterministicmns.cpp:66-73)
    /// ForEachMNShared(onlyValid, …). Order unspecified: LQ1 never depends on it (§3.4; tested, §6.5).
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Self::Entry));
    fn new_scratch(&self) -> Self::Scratch;            // `CDeterministicMNList()`: an empty list
}

/// MnsUsedAtH / MnsUsedAtHIndexed[i]: Core builds them with AddMN and swallows its exceptions (quirk 28b),
/// so AddMN's exact acceptance rule (EV2-dml.md §2.5 "try_add order and failures") is consensus here.
pub trait ScratchMnList {
    type Entry: MnEntryView;
    /// CDeterministicMNList::AddMN(dmn, fBumpTotalCount = true) (v23 evo/deterministicmns.cpp:420-480 /
    /// v24 :439-517), all-or-nothing, judged on the entry's OWN state.
    fn try_add(&mut self, e: &Self::Entry) -> Result<(), AddMnRejected>;
    fn contains(&self, p: &ProTxHash) -> bool;
    fn total(&self) -> usize;                          // GetCounts().total()
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Self::Entry));
}
pub struct AddMnRejected;                              // the reason is never observable (swallowed in Core)
```

#### 2.2.3 What LQ1 does *not* need

| Item | Needed? | Why |
|---|---|---|
| members, `confirmed_hash_with_protx`, the null test of `confirmed_hash` | **yes** | score and confirmed filter (§3.4) |
| collateral **outpoint** | **yes** | equal-score tie-break (§3.4); uniqueness inside `try_add` |
| `is_banned`, `is_evo`, `counts().enabled/total` | **yes** | filters, the Evo-only Platform rule, rotation's early return, snapshot sizing |
| `AddMN` acceptance (`try_add`) | **yes** | quirk 28b |
| voting **weights**, Core's CHAMP iteration order (EV2 quirk 41) | **no** | weights appear only in `Counts::m_*_weighted`; every selection path sorts by a total order (§3.4) |
| collateral **heights**, payment heights, PoSe penalty, payouts, keys, net info | **no** (except inside `try_add`) | not read by any function in §3.1. Operator keys and services are read by LQ2, DK1/LQ4 and LQ5 from the **returned entries** (§2.6) |

#### 2.2.4 Adapters (re-targeted at `EV2-dml.md`)

- **EV2 (production).** `impl MnListView for MnList { type Entry = Arc<Dmn>; type Scratch = UsedSet; }`.
  - Reads: `counts()`, `get(p).is_some()`, `is_pose_banned(p)`, `iter()`/`iter_valid()` (EV2-dml.md §2.6); entry fields `collateral`, `mn_type`, `cold.confirmed_hash`, `cold.confirmed_hash_with_pro_tx`, and the hot part's `pose_ban_height` (EV2-dml.md §2.3, §2.12 row LQ1a).
  - Lists at work blocks come from `resolve_list(at, …)` (EV2-dml.md §2.10).
  - **Lean scratch set (B-m1).** `UsedSet` holds only the unique-property map (built from EV2's `ColdDerived::unique_keys`, so the acceptance rule has one source), the proTxHash and internal-id sets, and counts. Its `try_add` reproduces `MnList::try_add`'s failure order exactly (EV2-dml.md §2.5 steps 1–7) without maintaining the payee queue, the decaying/unconfirmed/streak indexes or `core_order`. **Twin:** `MnList::scratch()` + `MnList::try_add(_, true)`; under xcheck every call's accept/reject is compared.
  - **Request to EV2 (B-m1).** Rotation reads lists at **work** blocks (`cycle − 8 − k·C`), while EV2's cache pins quorum *bases*. Add a `CacheHint::WorkBlock` so a cold cycle does not rebuild three lists from up to 575 diffs each (Q4).
- **SML (fixture writer only, §6.4).** Built from `mnlistdiff`-derived SML entries in `dash-conformance`. The SML carries no collateral and no owner key: `collateral()` returns the null outpoint (constant; B-n3), and `try_add` checks proTxHash, service and operator key only. Equal scores cannot occur on real chains (§3.4), so ordering is exact; `try_add` can only under-reject, which the fixture trace records (§6.4).

### 2.3 Other inputs

```rust
/// One reading context. Runtime callers build it from the tip view; validation from ConsensusCtx.
pub struct LlmqCtx<'a> {
    pub params: &'a ChainParams,              // BS5 (§2.7)
    pub chain: &'a dyn ChainAncestry,         // CS0 §8.4
    pub deployments: &'a dyn DeploymentView,  // CS0 §8.4: DIP0003, DIP0020, DIP0024, V19, V20 (buried), TESTDUMMY (versionbits, CS4)
    pub bls: BlsCtx,                          // the CALLER's scheme context (§3.3): for_block(h) in validation, for_tip elsewhere
    pub rules: &'a UngatedRules,              // read only through EV4's coinbase-CL predicate (§3.3.1)
}
impl<'a> LlmqCtx<'a> {
    /// Builds the context and checks the `log2` domain once (B-n4): every use_rotation type has
    /// 1 <= signing_active_quorum_count <= 65536 (§3.10). Core's tables give {2, 32}; BS5 may assert it too.
    pub fn new(…) -> Result<Self, LlmqError>;
}

/// State LQ1 reads. CH7b implements it over EvoReadView; tests over maps.
pub trait LlmqSource<L: MnListView> {
    /// Core GetListForBlock(pindex): the list after `at` (EV2 resolve_list).
    fn mn_list(&self, at: BlockId) -> Result<Arc<L>, StoreError>;
    /// Raw coinbase facts of a block, as EV4's predicate takes them (§3.3.1).
    fn coinbase_facts(&self, at: BlockId) -> Result<CoinbaseFacts, StoreError>;
}

/// Core's CQuorumSnapshotManager (v23 llmq/snapshot.cpp:264-292 / v24 :301-330): the only way LQ1 reads or writes
/// snapshot rows. CH7b supplies an attempt-scoped implementation and a runtime one (§3.7.2).
pub trait SnapshotRows {
    /// GetSnapshotForBlock. Keyed by hash: ancestry-free. Sees every earlier `store` (Core: raw write plus a
    /// write-through LRU). Ok(None) means there is no row: a normal outcome (28a, 28e, young chains), never an error.
    fn get(&self, t: LlmqType, cycle_base: &BlockHash) -> Result<Option<QuorumSnapshot>, StoreError>;
    /// StoreSnapshotForBlock: an unconditional overwrite keyed by (type, cycle-base hash). No "if absent", no comparison.
    fn store(&mut self, t: LlmqType, cycle_base: &BlockHash, snapshot: &QuorumSnapshot) -> Result<(), StoreError>;
}
```

### 2.4 Outputs and types

```rust
pub struct QuorumModifier(pub Hash256);
/// Members in Core's order. Duplicates are possible for rotated quorums (quirk 28f) and are kept.
pub type QuorumMembers<E> = Arc<[E]>;

/// What LQ1 needs to know about a stored commitment; LQ2 binds it to BS8's FinalCommitment (B-m4: no BS8 in LQ1a).
pub trait CommitmentView { fn quorum_index(&self) -> i16; fn is_null(&self) -> bool; }

/// What a scan returns and the scan cache retains (rev3, GPT r3 finding 2). In production it is QM1's quorum object
/// (Core CQuorumCPtr): its members were captured when it was built, and a scan-cache hit returns that same object
/// without a new GetQuorum or member computation (v23 quorumsman.cpp:245-254 / v24 :215-224). Tests use a plain record.
pub trait ScanEntry: Clone {
    fn base(&self) -> BlockId;               // m_quorum_base_block_index; continuation starts at base().prev
    fn quorum_hash(&self) -> QuorumHash;
    fn quorum_index(&self) -> i16;           // from the object's commitment (CQuorum::qc), as in Core
}

/// BS8's wire type (LQ1b only): mode i32 LE | compactsize n | fixed bitset(n) | compactsize m | m × i32 LE
/// (v23 llmq/snapshot.h:41-89 / v24 :42-90). Persisted and served as CoreBytes.
pub use dash_p2p_core::QuorumSnapshot;
pub enum SkipMode { NoSkipping = 0, SkippingEntries = 1, NoSkippingEntries = 2, AllSkipped = 3 }  // v23 :30 / v24 :31

/// A1 §2.10.1: what CH7b's attempt-scoped `SnapshotRows::store` records; applied (an overwrite) after the attempt.
Effect::StoreQuorumSnapshot { llmq_type: LlmqType, cycle_base: BlockHash, snapshot: QuorumSnapshot }

pub enum LlmqError {
    Store(StoreError),            // unreadable data or a record an invariant requires: fail-stop (§2.6); an absent row is not one
    UnknownLlmqType(u8),          // Core throws (validation.cpp v23:6115-6116 / v24:6707-6708); callers check types first
}
```

### 2.5 Public functions

```rust
// ---- params.rs ------------------------------------------------------------------------------
pub struct TypeEnabledOverrides { pub dip24_active: Option<bool>, pub have_dip24_quorums: Option<bool> }
pub fn is_quorum_type_enabled(ctx: &LlmqCtx, t: LlmqType, prev: Option<BlockId>) -> Result<bool, LlmqError>;
/// v24's two optional parameters (rpc/quorums.cpp v24:1078, `quorum dkginfo`); the signature has them at both pins.
pub fn is_quorum_type_enabled_with(ctx: &LlmqCtx, t: LlmqType, prev: Option<BlockId>, o: TypeEnabledOverrides)
                                   -> Result<bool, LlmqError>;
pub fn is_quorum_rotation_enabled(ctx: &LlmqCtx, p: &LlmqParams, at: BlockId) -> bool;
pub fn enabled_quorum_params<'p>(ctx: &'p LlmqCtx, prev: BlockId) -> impl Iterator<Item = &'p LlmqParams>; // configured order
pub fn eval_spork(t: LlmqType, spork_value: i64) -> bool;
pub fn signer_index_bits(signing_active_quorum_count: u32) -> u32;                // DEC-29 D4, §3.10
pub fn cycle_base_height(p: &LlmqParams, h: u32) -> u32;

// ---- modifier.rs ----------------------------------------------------------------------------
pub fn hash_modifier<L: MnListView>(ctx: &LlmqCtx, src: &dyn LlmqSource<L>, p: &LlmqParams,
                                    cycle_base: BlockId) -> Result<QuorumModifier, LlmqError>;
pub fn hash_modifier_from_work_block<L: MnListView>(ctx: &LlmqCtx, src: &dyn LlmqSource<L>, p: &LlmqParams,
                                                    work: BlockId) -> Result<QuorumModifier, LlmqError>;

// ---- score.rs / memo.rs ---------------------------------------------------------------------
pub fn calculate_quorum<E: MnEntryView>(candidates: impl IntoIterator<Item = E>, m: &QuorumModifier,
                                        max_size: usize /* 0 = all */, only_evo: bool) -> Vec<E>;
/// L1 (§5.3): the runtime memo of `calculate_quorum(list(work), m, 0, false)`, keyed by (work block hash, m).
pub trait SortedListMemo<E> { fn get(&mut self, work: &BlockHash, m: &QuorumModifier) -> Option<Arc<[E]>>;
                              fn put(&mut self, work: &BlockHash, m: &QuorumModifier, v: Arc<[E]>); }
pub struct NoMemo;   // the pure default

// ---- members.rs -----------------------------------------------------------------------------
// Core GetAllQuorumMembers (v23 llmq/utils.cpp:569-659 / v24 :634-730) is split at its cache accesses; the runtime's
// members facade (§3.7.4) interleaves its two caches with these steps. There is no public cached-or-uncached
// "get_all_quorum_members" in the pure layer (rev3, GPT r3 finding 1).

/// The gate, before any cache access: false when base has no parent or the type is not enabled at base.prev
/// (v23 :576-582 / v24 :641-647). Core returns {} without touching a cache, even with reset_cache.
pub fn members_gate(ctx: &LlmqCtx, t: LlmqType, base: BlockId) -> Result<bool, LlmqError>;
/// After the base-cache step (v23 :597-634 / v24 :662-699).
pub enum MembersRoute {
    NonRotated,                                        // compute_quorum_members(base)
    RotatedIndexOutOfRange,                            // base.h % interval >= N: Core returns {}, inserts nothing
    Rotated { cycle_base: BlockId, index: u16 },       // indexed-cache key (cycle_base.hash, index); on a miss
                                                       // compute_by_quarter_rotation(cycle_base) (§3.6.5)
}
pub fn members_route(ctx: &LlmqCtx, t: LlmqType, base: BlockId) -> MembersRoute;
/// Core ComputeQuorumMembers via GetAllQuorumMembers' non-rotated branch (v23 :644-654 / v24 :715-725), incl. the work list.
pub fn compute_quorum_members<L: MnListView>(ctx: &LlmqCtx, src: &dyn LlmqSource<L>, t: LlmqType, base: BlockId,
                                             memo: &mut dyn SortedListMemo<L::Entry>)
                                             -> Result<QuorumMembers<L::Entry>, LlmqError>;

/// The types Core's PreComputeQuorumMembers (v23 llmq/blockprocessor.cpp:30-38 / v24 :34-42) computes for `block`, in
/// configured order: enabled at block.prev (GetEnabledQuorumParams), rotation-enabled at block, block.h % interval == 0.
pub fn precompute_targets<'p>(ctx: &'p LlmqCtx, block: BlockId) -> impl Iterator<Item = LlmqType> + 'p;
pub enum PrecomputeMode {
    Connect,   // reset_cache = false: ProcessBlock (v23 :176 / v24 :223) on ConnectBlock, TestBlockValidity, VerifyDB 4, rollforward
    Undo,      // reset_cache = true: UndoBlock (v23 :387 / v24 :523) on disconnect, VerifyDB 3
}
/// The runtime precompute entry point (rev3 ruling 1). Defined here, implemented by CH7b over the process-wide members
/// facade (§3.7.4), called by LQ2: first in process_block after its DIP3 gate (v23 :171-174 / v24 :218-221), and
/// first in undo_block (no gate, as in Core). For each t in precompute_targets(block): facade.members(t, block,
/// reset = mode == Undo, effects); the members are discarded, as Core discards them. A store error is fail-stop.
pub trait QuorumPrecompute {
    fn precompute(&self, block: BlockId, mode: PrecomputeMode, effects: &mut dyn EffectSink) -> Result<(), MemberError>;
}

/// v24-only RPC predictor ComputeQuorumMembersFromWorkBlock (v24 llmq/utils.cpp:589-632): reads rows, never stores one.
/// Uncached in Core as well (it calls the rotation with predicting = true, never GetAllQuorumMembers).
pub fn compute_quorum_members_from_work_block<L: MnListView>(…) -> Result<Option<QuorumMembers<L::Entry>>, LlmqError>;

// ---- reference.rs (feature "reference"; tests, harnesses, xcheck comparators only) -------------
/// The uncached composition of the steps above. Not a production path: production reaches members only through
/// the facade (§3.7.4), so a cache hit there never computes or writes, as in Core.
pub fn get_all_quorum_members_uncached<L: MnListView>(ctx: &LlmqCtx, src: &dyn LlmqSource<L>, t: LlmqType,
        base: BlockId, memo: &mut dyn SortedListMemo<L::Entry>, rows: &mut dyn SnapshotRows)
        -> Result<QuorumMembers<L::Entry>, LlmqError>;
/// precompute_targets over get_all_quorum_members_uncached. Test-only: it writes rows without consulting or resetting
/// any members cache, which is exactly the GPT r3 counterexample (PURE 240) if used in production.
pub fn precompute_uncached<L: MnListView>(…) -> Result<(), LlmqError>;
/// Core's two members caches and facade over a SnapshotRows, and Core's GetQuorum + quorum-object cache over the
/// mined index and that facade: the models the conformance suites (§6.6) run in LQ1's own CI.
pub struct ModelMembersFacade<…>; pub struct ModelMaterializer<…>;

// ---- scan.rs --------------------------------------------------------------------------------
/// LQ2 implements it. Every read resolves through `at`'s ancestry (A1 §2.10.2; §3.8.2).
pub trait MinedCommitmentIndex {
    type Commitment: CommitmentView;
    /// GetMinedCommitmentsUntilBlock: bases of the newest `max` commitments of `t` mined at height <= at.height,
    /// newest first (v23 llmq/blockprocessor.cpp:546 / v24 :687).
    fn mined_until(&self, t: LlmqType, at: BlockId, max: usize) -> Result<Vec<BlockId>, StoreError>;
    /// GetLastMinedCommitmentsByQuorumIndexUntilBlock(t, at, index, nth) (v23 :590 / v24 :731).
    fn nth_mined_by_index(&self, t: LlmqType, at: BlockId, index: i16, nth: usize) -> Result<Option<BlockId>, StoreError>;
    /// L6 (B-m7): the newest `k` bases for one index in one walk; equal to nth_mined_by_index for nth = 0..k.
    fn newest_by_index(&self, t: LlmqType, at: BlockId, index: i16, k: usize) -> Result<Vec<BlockId>, StoreError>;
    /// GetMinedCommitment(t, quorum hash) -> (commitment, mined block hash). Used by QM1's GetQuorum and by
    /// scan_commitments_from; Ok(None) is Core's {null, ZERO}.
    fn mined_commitment(&self, t: LlmqType, q: &QuorumHash) -> Result<Option<(Arc<Self::Commitment>, BlockHash)>, StoreError>;
}

/// Core CQuorumManager::GetQuorum(type, base, populate_cache) (v23 quorumsman.cpp:391-414 / v24 :518-536) and, for a
/// materializer QM1 binds to an explicit chain, v24's chain overload (:539-555). QM1 implements it (§3.8.3): the
/// mined check, the quorum-object cache, and on a miss BuildQuorumFromCommitment, whose members come from the
/// process-wide members facade (§3.7.4) and so can warm that cache and write rows. Every effect it makes (object-cache
/// insert, members-cache state, rows, contributions) stays made whatever the scan does next.
pub trait QuorumMaterializer {
    type Quorum: ScanEntry;
    /// Ok(None) exactly where Core's GetQuorum returns nullptr in the same state (not mined in the bound chain; no
    /// mined commitment record). Err only for an unreadable record or a member-source store fault (fail-stop).
    fn get_quorum(&mut self, t: LlmqType, base: BlockId, populate_cache: bool) -> Result<Option<Self::Quorum>, LlmqError>;
}

/// Core's pindexStore (v23 quorumsman.cpp:203-225 / v24 :174-196): None when Core returns {} before touching its
/// cache (count == 0, type not enabled at `start`, or the "below genesis" bail).
pub fn scan_store_block(ctx: &LlmqCtx, t: LlmqType, start: BlockId, count: usize) -> Result<Option<BlockId>, LlmqError>;

pub enum ScanCache<'a, Q> {
    Bypass,                                    // v24's `chain` overload: no lookup, no write (v24 :162-168, :203, :237-239, :274)
    Miss,                                      // looked up, absent
    Hit(&'a [Q]),                              // the cached entry for hash(store): QM1's retained quorum objects
}
pub struct ScanOutcome<Q> {
    pub result: Vec<Q>,                        // what Core returns
    pub cache_write: Option<Vec<Q>>,           // what Core emplaces under hash(store), if anything
}
/// Core's ScanQuorums body from the cache lookup on, including the continuation and the materialization
/// (v23 :233-307 / v24 :203-286): mat.get_quorum for EVERY newly scanned base, in order, before truncation (§3.8.1).
pub fn scan_quorums_from<I: MinedCommitmentIndex, M: QuorumMaterializer>(ctx: &LlmqCtx, idx: &I, t: LlmqType,
        store: BlockId, count: usize, cache: ScanCache<'_, M::Quorum>, mat: &mut M)
        -> Result<ScanOutcome<M::Quorum>, LlmqError>;
/// v24 ScanCommitments (v24 :294-409; quorum-proof RPCs only): same shape, its own cache, its own continuation key.
/// It reads commitments (GetMinedCommitment) and builds no quorum, so it has no materializer and no member effects.
pub fn scan_commitments_from<I: MinedCommitmentIndex>(…) -> Result<ScanOutcome<(BlockId, Arc<I::Commitment>)>, LlmqError>;

// ---- selection.rs ---------------------------------------------------------------------------
/// SelectQuorumForSigningStartBlock (v23 quorumsman.cpp:698-707 inline / v24 :711-722), Core's int semantics (A-m4):
/// sign_height == -1 means the tip; start = sign_height - offset computed in i64; None if start < 0 or > tip.
pub fn select_start_height(tip_height: i32, sign_height: i32, sign_offset: i32) -> Option<u32>;
/// The selection part of SelectQuorumForSigning / SelectCommitmentForSigning (v23 :711-752 / v24 :733-774, :805-846),
/// over the scan result the caller obtained through QM1's facade with count = N.
pub fn select_from_scan<'q, Q: ScanEntry>(p: &LlmqParams, rotation_enabled: bool, scanned: &'q [Q],
                                          selection_hash: &Hash256) -> Option<&'q Q>;
```

QM1's facade (`select_quorum_for_signing`, `scan_quorums`) composes these with its caches in Core's order (§3.8.3): gate and store block, the scan-cache lookup, `scan_quorums_from` with its materializer, then the emplace. Consensus callers use the same facade: CbTx best-CL via the CL type, which Core keeps non-rotated; asset unlocks via the Platform type and MNHF via an explicit `GetQuorum`, whose types are non-rotated **only in the default configuration** (rev4, §3.8.4). Their results, their member-cache and object-cache effects and, for a rotated override, their row effects are therefore Core's. There is one path.

### 2.6 The member-source trait (EV0 owns it; this is LQ1's proposal, B-M1 ruling)

```rust
// dash-evo, landed by EV0 (types first). One trait for EV2b's PoSe branch, LQ2's Verify, QM1, DK1, LQ5.
pub trait QuorumMemberSource {
    /// Core llmq::utils::GetAllQuorumMembers(type, base) (utils.cpp v23:569-659 / v24:634-730).
    /// Members in Core's order; empty when the type is not enabled at base.prev, base has no parent, the base is not
    /// a valid quorum base for the type, or the rotation rules produce no members.
    /// A snapshot row the computation stores is written exactly as Core's StoreSnapshotForBlock (an unconditional
    /// overwrite, visible to the next read, §3.7.2); inside a connect attempt it is also recorded in `effects`
    /// (A1 §2.10.1), so it survives a later failure.
    ///
    /// Provenance (B-m2): each returned entry carries the state of the list it was taken from. For rotated quorums
    /// that is a list up to three cycles old. Consumers (LQ2's membersSig keys, DK1/LQ4's encryption keys and member
    /// index, LQ5's services) read from the returned entry and never re-resolve it by proTxHash in a newer list,
    /// as Core's Verify reads members[i]->pdmnState->pubKeyOperator (v23 llmq/commitment.cpp:72-77).
    fn members(&self, t: LlmqType, base: BlockId, effects: &mut dyn EffectSink)
               -> Result<Arc<[Arc<Dmn>]>, MemberError>;
}

pub enum MemberError {
    /// Unreadable data (a read error) or a record missing although an established store invariant requires it
    /// (the MN list of a connected block). An ABSENT snapshot row is not an error: `SnapshotRows::get` returns
    /// Ok(None) and the rotation breaks (28a, 28e, young chains), as in Core; unavailable block data is not one
    /// either (`CoinbaseFacts::BlockDataUnavailable`, §3.7.3). The member computation reads no commitment; a mined
    /// commitment missing in a scan is Core's GetQuorum nullptr, a value (§3.8.1), not this error.
    /// FAIL-STOP: the caller returns it as a node error, never as a block verdict and never as
    /// `failed-procspectxsinblock`. This is the manager's ruling under both profiles (deviation D4, §3.15). Core's
    /// precedent is v24's typed EvoDbInconsistencyError → state.Error (evo/specialtxman.cpp:989-992); Core's other
    /// store exceptions, including the DML's, still reach failed-procspectxsinblock (v24 :993-996).
    Store(StoreError),
    /// Core's IsQuorumTypeEnabled throws runtime_error for an unknown type (validation.cpp v23:6115-6116 /
    /// v24:6707-6708); Core's catch maps it to failed-procspectxsinblock. Unreachable from block validation:
    /// EV1/LQ2 reject unknown types first (bad-qc-commitment-type).
    UnknownLlmqType(u8),
}
```

- **Mapping table for EV2b/CH7a:** `Store` → node error (A1 I5); `UnknownLlmqType` → `DmlException::Members` → `failed-procspectxsinblock`. `EV2-dml.md` §2.11 now has the same mapping (`MemberError::Store` → `DmlBuildError::Store` → node error), and its l.532 records that universal fail-stop is a ruling, not v24 behaviour for the DML (GPT m2).
- **Both paths are tested:** an absent row (`Ok(None)` → break, members still computed) and an injected unreadable row or missing required list (→ `MemberError::Store`, no verdict).
- **CH7b's production implementation is the process-wide members facade** (§3.7.4) with `reset = false`; it builds an attempt-scoped `SnapshotRows` from the `effects` it is given (§3.7.2). The same facade serves `QuorumPrecompute` (§2.5) and QM1's quorum builds (§3.8.3), so every production member computation shares one pair of caches, as Core's callers share its statics.
- `EV2-dml.md`'s `build_new_list_from_block` already takes an `EffectSink`, so the emission point of A1 §2.10.1 is available to EV2b.
- The facade binds the pure steps (§2.5) to `L = MnList`, `E = Arc<Dmn>`.
- One production implementation (CH7b's facade). The uncached pass-through exists only in `reference` (unit tests, the xcheck comparator on misses); the conformance suite and the oracle comparisons run against the production facade (§6.6).

### 2.7 What LQ1 needs from BS5 (`dash-params`)

- `ChainParams::llmqs()` in `AddLLMQ` order (v24 chainparams.cpp:281-285 mainnet); `ChainParams::llmq(t)` with `use_rotation`, `size`, `dkg_interval`, `dkg_mining_window_start/end`, `signing_active_quorum_count`, `keep_old_connections` (the scan cache size).
- The roles `llmq_type_platform`, `llmq_type_dip0024_instant_send`; `dip0024_quorums_height`; the network kind (testnet and devnet exceptions); the V19 height (predictor).
- Tables are identical at both pins apart from `MAX_LLMQ_SIZE` (O17 DKG-14). The `log2` domain check is in `LlmqCtx::new` (B-n4); BS5 may assert it as well.

### 2.8 Neighbours

| Direction | Task | Exchanged |
|---|---|---|
| upstream | **EV0** | `QuorumMemberSource`, `MemberError`, `Effect`/`EffectSink` (§2.6) |
| upstream | **EV2a** | `MnList`/`Arc<Dmn>` (§2.2.4), `resolve_list`, `MnList::try_add` (twin of `UsedSet`), `ColdDerived::unique_keys`; requested `CacheHint::WorkBlock` |
| upstream | **EV4** (+ BS6a decoder) | `cbtx::nonnull_coinbase_chainlock` and `CoinbaseFacts` (§3.3.1) |
| upstream | **BS5**, **CS0/CS4**, **BS11a-t** | `ChainParams`; `BlockId`, `ChainAncestry`, `DeploymentView`, `BlsCtx`, `UngatedRules`, `CoreBytes` |
| upstream | **BS8** (LQ1b only) | `QuorumSnapshot` |
| upstream | **O2a-LQ1** (follow-up), **PF0** | the oracle entry points and fixture loader (§6.2); the counter gate (§5.4) |
| downstream | **LQ2** | implements `MinedCommitmentIndex` (incl. `newest_by_index`); calls the injected `QuorumPrecompute::precompute` first in `process_block` (after the DIP3 gate, `Connect`) and `undo_block` (`Undo`), never an uncached function; `members` in `verify`; tests against `reference::ModelMembersFacade` until CH7b exists |
| downstream | **EV2b** | `QuorumMemberSource::members` in the QcTx PoSe branch (v23 evo/specialtxman.cpp:456 / v24 :723) |
| downstream | **QM1a/b** | extends CH7b's scan facade, scan cache, materializer and quorum-object cache (the same runtime caches, never a second copy) with contributions, warming, cleanup and recovery; the DKG second access and its trace rule (§3.8.3); re-runs LQ1's scan conformance suite (§6.6); uses `scan_*_from`, `select_from_scan`, `eval_spork` |
| downstream | **EV3, EV4, LK1, SG1, IX/RP** | selection and scans through QM1's facade; EV4 owns the CbTx-CL predicate LQ1 consumes |
| downstream | **LQ3, DK1/LQ4, LQ5** | snapshot rows; members (provenance contract, §2.6); the predictor (RP3b) |
| downstream | **CH7b** | applies `Effect::StoreQuorumSnapshot`; owns the members cache and facade, the `QuorumPrecompute` implementation LQ2 calls, L1, L4, L5 and the `LlmqSource` impl; passes LQ1's members-facade conformance suite and the cold/warm/undo oracle fixtures with the production facade (§6.6); **the M4 scan runtime (rev4, Q7)**: the production scan facade and scan cache, the `QuorumMaterializer` (both `GetQuorum` routes, retained objects) and the quorum-object cache, for every configured type validation can select, passing the scan suite including the rotated Platform override |
| downstream | **dash-spv** (data only) | LQ1b-F's fixture files, checked by an in-crate dash-spv test (§6.4) |

---

## 3. Core-equivalence

### 3.1 Function map

| Core (v23 / v24) | Rust | Notes |
|---|---|---|
| `IsQuorumTypeEnabled` validation.cpp:6066-6121 / :6658-6713 | `params::is_quorum_type_enabled{,_with}` | §3.2 |
| `IsQuorumRotationEnabled` llmq/options.cpp:45-57 / :50-62 | `params::is_quorum_rotation_enabled` | §3.2 |
| `GetEnabledQuorumParams` options.cpp:119 / :125 | `params::enabled_quorum_params` | configured order |
| `EvalSpork` options.cpp:23-32 / :26-35 | `params::eval_spork` | 0 → all; 1 → all but 100_67, 400_60, 400_85 |
| `calculateQuorumScore` llmq/utils.cpp:73 / :72 | `score::score_key` | §3.4 |
| `GetHashModifier` :88-111 / :99-115; `GetHashModifierFromWorkBlock` inline / :87-97 | `modifier::*` | §3.3 |
| `GetNonNullCoinbaseChainlock` evo/cbtx.cpp:219-247 / :181-211 | consumed: `dash_evo::cbtx::nonnull_coinbase_chainlock` (EV4) | §3.3.1 |
| `CalculateScoresForQuorum` ×2, `CalculateQuorum` :113-182 / :117-186 | `score::calculate_quorum` | §3.4 |
| `ComputeQuorumMembers` :287-301 / :291-305 | `members::compute_quorum_members` | §3.5 |
| `GetQuorumQuarterMembersBySnapshot` :184-285 / :188-289 | `rotation::quarters_by_snapshot` | §3.6.2 |
| `BuildQuorumSnapshot` :303-337 / :307-341 | `rotation::build_quorum_snapshot` | §3.6.4 |
| `BuildNewQuorumQuarterMembers` :339-456 / :343-466 | `rotation::new_quarters` | §3.6.3 |
| `ComputeQuorumMembersByQuarterRotation` :458-524 / :471-544 | `rotation::compute_by_quarter_rotation` | §3.6.5 |
| `ComputeQuorumMembersFromWorkBlock` — / :589-632 | `members::compute_quorum_members_from_work_block` | v24 RPC only |
| `GetAllQuorumMembers` :569-659 / :634-730 | `members::{members_gate, members_route}` + the computations; caches and order: CH7b's facade | §3.5, §3.6.6, §3.7.4 |
| `PreComputeQuorumMembers` llmq/blockprocessor.cpp:30-38 / :34-42 | `members::precompute_targets` + `QuorumPrecompute` (CH7b) | §3.7.4 |
| `CQuorumManager::ScanQuorums` llmq/quorumsman.cpp:199-307 / :149-286 | `scan::scan_store_block`, `scan::scan_quorums_from`; caches: QM1 | §3.8 |
| `CQuorumManager::GetQuorum` quorumsman.cpp:391-414 / :518-536, :539-555; `BuildQuorumFromCommitment` :66-112 / :58-104 | `QuorumMaterializer` (QM1), over the members facade | §3.8.3 |
| `ScanCommitments` — / :288-409 | `scan::scan_commitments_from`; cache: QM1 | §3.8.5 |
| `GetMinedCommitmentsIndexedUntilBlock` / `…PerQuorumIndex…` blockprocessor.cpp:655, :638 / :796, :779 | `scan::indexed_until` | §3.8.1 |
| `unordered_lru_cache` unordered_lru_cache.h:15-113 (v24 +2 lines) | QM1's `LruCache` | §3.8.3 |
| `SelectQuorumForSigningStartBlock` (inline v23 :698-707) / :711-722 | `selection::select_start_height` | |
| `SelectQuorumForSigning` :693-753 / :724-775; `SelectCommitmentForSigning` — / :800-847 | `selection::select_from_scan` (+ QM1 facade) | §3.9; `log2` §3.10 |

### 3.2 Parameter predicates

- **`is_quorum_type_enabled(t, prev)`** is a literal port of the switch (v23 validation.cpp:6066-6121 / v24 :6658-6713):
  - `None` (genesis) → false;
  - `DEVNET`, `TEST`, `TEST_PLATFORM`, `400_60`, `400_85`, `DEVNET_PLATFORM` → true;
  - `50_60` → `!dip24_active || !have_dip24_quorums || testnet || devnet`;
  - `TEST_INSTANTSEND` → `!dip24_active || !have_dip24_quorums || llmqTypeDIP0024InstantSend == TEST_INSTANTSEND`;
  - `TEST_V17` → `active_after(prev, TESTDUMMY)` (versionbits, CS4);
  - `100_67` → `active_after(prev, DIP0020)`;
  - `60_75`, `DEVNET_DIP0024`, `TEST_DIP0024` → `dip24_active`;
  - `25_67` → `prev.height >= 847000` (quirk 34's family);
  - anything else → `UnknownLlmqType` (Core throws).

  `dip24_active = overrides.dip24_active.unwrap_or(active_after(prev, DIP0024))` and `have_dip24_quorums = overrides.have_dip24_quorums.unwrap_or(prev.height >= DIP0024QuorumsHeight)`, exactly Core's `optional::value_or` (B-m9). Only v24's `quorum dkginfo` passes overrides (rpc/quorums.cpp v24:1072-1078); the parameters exist at both pins.
- **`is_quorum_rotation_enabled(p, at)`**: `p.use_rotation`, then `cb = at.h − at.h % interval`, false if `cb < 1`, then `active_after(ancestor(at, cb − 1), DIP0024)`. A rotation type can be *enabled* at a base without rotation being enabled there when DIP24's height is not cycle-aligned; Core then takes the non-rotated path, which returns empty members because `useRotation` is set (v23 utils.cpp:294 / v24 :298; `ASSERT_IF_DEBUG` is a no-op in release builds, span.h). Mainnet's DIP0024Height (1,737,792) is cycle-aligned; testnet's (769,700) is not, but no testnet base falls in the gap (§3.6.6). Regtest can reach it. Quirk Q-LQ1-2, ported literally.

### 3.3 The modifier (quirk 29)

`hash_modifier(p, cycle_base)` (v23 utils.cpp:88-111 / v24 :99-115): `work = ancestor(cycle_base, cycle_base.h − 8)` (`WORK_DIFF_DEPTH`, v23 snapshot.h:39 / v24 :40).

1. If `active_after(work, V20)` → `hash_modifier_from_work_block(p, work)`: `cl = nonnull_coinbase_chainlock(coinbase_facts(work), work.h, active_at(work, V20), ctx.rules, ctx.bls)`; `Some((sig, _))` → `SHA256d(u8 type ‖ i32le work.height ‖ sig96)`; else `SHA256d(u8 type ‖ work.hash)`.
2. Else (pre-V20): rotated → `SHA256d(u8 type ‖ work.hash)`; non-rotated → `SHA256d(u8 type ‖ cycle_base.hash)`.

`LLMQType` serializes as one byte; `nHeight` is `int`. The signature bytes hashed are the 96 payload bytes decoded and re-encoded under the caller's global scheme, which re-encode identically when valid (`CheckMalleable`).

#### 3.3.1 The shared coinbase-CL predicate (A-M2 ruling; owner EV4, decoder BS6a)

**Core.** `GetNonNullCoinbaseChainlock(pindex)` (v23 evo/cbtx.cpp:219-247 / v24 :181-211, plus v24's `(block, height)` overload :200-211):
- both pins: `DeploymentActiveAt(pindex, V20)`, else none; `ReadBlockFromDisk`, else none; `GetTxPayload<CCbTx>(vtx[0])` (type check, no trailing bytes: evo/specialtx.h:18-35), else none; `bestCLSignature.IsValid()`, else none;
- v24 adds `vtx` non-empty, `IsCoinBase`, `nVersion >= CLSIG_AND_BALANCE` (redundant: a v2 CbTx carries no CL fields at either pin, cbtx.h v23:52-60 / v24:58-66), **`nHeight == height`** and **`bestCLHeightDiff < height`**.
- The payload decode itself is profile-dependent: v24 reads `merkleRootAssetUnlocks` for `nVersion >= 4`, v23 sees those bytes as trailing and fails (O17 EVO-06, `cbtx_v4_known`).

**The five consumers** (same sites at both pins):

| # | Core site | Use | Owner |
|---|---|---|---|
| 1 | llmq/utils.cpp v23:96 / v24:89 | quorum modifier (this section) | LQ1 |
| 2 | evo/specialtxman.cpp v23:59 / v24:166, `CheckCbTxBestChainlock` | previous block's CL: `bad-cbtx-null-clsig`, `bad-cbtx-older-clsig` (consensus) | EV4 |
| 3 | node/miner.cpp v23:139 / v24:131, `CreateNewBlock` | carries the previous coinbase CL forward | MN1 |
| 4 | evo/smldiff.cpp v23:99 / v24:98, `BuildQuorumChainlockInfo` | `quorumsCLSigs` in `mnlistdiff`/`qrinfo` (P2P wire) | EV4 (builder), LQ3 (qrinfo), P6b (serving) |
| 5 | rpc/rawtransaction.cpp v23:678 / v24:857, `getassetunlockstatuses` | the chainlocked credit pool when no P2P CL is known | the RPC task owning `getassetunlockstatuses` |

**Proposed O17 row** (the manager assigns the id): "CbTx coinbase-ChainLock predicate", class CON, keying `profile`, field `UngatedRules.cbtx_nonnull_cl_strict` (`V23_1: false`, `V24_0: true`), consumers 1–5, cross-references EVO-06 (`cbtx_v4_known` drives the decode) and EVO-18 (the range rule that makes the strict checks redundant on validated chains).

**Proposed API** (EV4's `dash-evo::cbtx`; LQ1 calls it, never decodes CbTx itself):

```rust
pub enum CoinbaseFacts { BlockDataUnavailable, Coinbase { is_coinbase: bool, n_type: u16, extra_payload: CoreBytes } }
/// Core GetNonNullCoinbaseChainlock(pindex). Decodes with BS6a's CbTx decoder under rules (cbtx_v4_known) and bls.
pub fn nonnull_coinbase_chainlock(facts: &CoinbaseFacts, height: u32, v20_active_at: bool,
                                  rules: &UngatedRules, bls: BlsCtx) -> Option<(BlsSig96, u32)>;
```

**Reachability of the delta** (unchanged from r0; confirmed by A): every CbTx that passed full CbTx validation at either pin satisfies `nHeight == h` (`CheckCbTx`) and `diff < h` (v24's EVO-18 range rule, or v23's `bad-cbtx-older-clsig` chain on mainnet). The checks differ only for a CbTx never validated: `fCheckCbTxMerkleRoots == false` under assumevalid on a crafted chain. `RollforwardBlock` (v24 validation.cpp:5197) replays blocks already validated. §6.3.5 adds the L2 invariant that proves the history part from data.

**Quirk 29 cases**, each a constructed vector (§6.1):

| Case | List block (§3.5) | Modifier |
|---|---|---|
| non-rotated, `base + 1 < V20` | `base` | `(type, base.hash)` |
| non-rotated, `V20 − 1 <= base < V20 + 7` (**first V20 cycle**; mainnet base 1,987,776 for 100_67, 400_60, 400_85) | `base − 8` | `(type, base.hash)` |
| non-rotated, `base = V20 + 7` (regtest only) | `base − 8` | `(type, work.hash)`: `work = V20 − 1` is not `ActiveAt(V20)` |
| non-rotated, `base >= V20 + 8` | `base − 8` | CL form if the predicate returns a CL, else `(type, work.hash)` |
| rotated, cycle base `< V20 + 7` (incl. mainnet 1,987,776) | `base − 8` | `(type, work.hash)` |
| rotated, later | `base − 8` | CL form or `(type, work.hash)` |

Testnet's V20 (905,100) is not a multiple of 24 or 288, so no testnet base falls in a mixed row. Mainnet has one mixed base per non-rotated type (V20 = 576·3451).

### 3.4 Scores and ordering (`calculate_quorum`)

- **Filter** (v23 utils.cpp:113-155 / v24 :117-155): skip banned (own state), skip a null `confirmedHash`, skip non-Evo when `only_evo`.
- **Score:** `SHA256(cwp ‖ modifier)`, single SHA-256 of 64 bytes, read as `arith_uint256`.
- **Order:** `std::sort(rbegin, rend, cmp)`, `cmp(a, b) = a.score == b.score ? a.collateral < b.collateral : a.score < b.score` (v23 :162-169 / v24 :166-173): **score descending, then collateral descending**. In Rust `ScoreKey = (score limbs most-significant first, OutPoint)`, sorted descending. The order is total for distinct collaterals; equal (score, collateral) pairs cannot occur in any Core caller (every input is a list or an `AddMN` scratch list, confirmed by A). Equal scores cannot occur on real chains; the tie-break is fuzzed by injecting equal `cwp` (§6.2).
- **Iteration order** never matters (total order, per-entry filters); tested with three orders (§6.5).
- **`only_evo`:** `llmqTypePlatform == t && active_after(base, V19)`, non-rotated only (v23 :290 / v24 :294).

### 3.5 Non-rotated members

Order (v23 :569-659 / v24 :634-730), empty (∅) where marked:
1. ∅ if `base.prev` is `None` or `!is_quorum_type_enabled(t, base.prev)`.
2. If `is_quorum_rotation_enabled(p, base)` → §3.6.6.
3. `list_block = active_after(base, V20) ? ancestor(base, base.h − 8) : base` (v23 :645-650 / v24 :716-721), then `compute_quorum_members`: ∅ if `p.use_rotation` (Q-LQ1-2); ∅ if `base.h % interval != 0`; else `calculate_quorum(list, hash_modifier(p, base), p.size, only_evo)`.

### 3.6 DIP24 rotation (quirk 28)

#### 3.6.1 Structure

- `quorum_index = base.h % interval` (∅ if `>= N`, v23 :615 / v24 :680); `cycle_base = ancestor(base, base.h − quorum_index)`; `work = cycle_base − 8`; `m_0 = hash_modifier(p, cycle_base)` (v24 :701-704; v23 inside `BuildNewQuorumQuarterMembers`, :354).
- All `N` quorums of the cycle are computed at once; quorum `i` = `Q(H−3C)[i] ‖ Q(H−2C)[i] ‖ Q(H−C)[i] ‖ new[i]` (v23 :509-519 / v24 :529-541).
- Quarter size `size / 4`: 60 → 15, 4 → 1, 8 → 2.
- v24 reads the previous cycles through `work->GetAncestor`, v23 through the cycle base: equal, because every rotated interval exceeds 8 (A).

#### 3.6.2 Previous quarters from snapshots (`quarters_by_snapshot`)

For `k = 1, 2, 3`:
- `c_k = ancestor(work, cycle_base.h − k·interval)`. v24: null → `break` (v24 :491-495); v23 would dereference, but genesis never has a row, so both pins break one step earlier.
- `snap = rows.get(t, c_k.hash)`; `Ok(None)` → **`break`** (28a; v23 :485 / v24 :503).
- Reconstruct (v23 :184-285 / v24 :188-289): `all = sorted(list(work_k), m_k)` (through the L1 memo, §5.3), split by `snap.active[j]` into unused (non-banned) and used, `combined = unused ‖ used`. Rows Core wrote always cover `all` (`active.len() = total(work_k) >= |all|`, 28h); `j >= active.len()` is treated as unused (imported rows only; the importer refuses them, §4.3). Empty `combined` → `N` empty quarters.
- **Fill by mode** (v23 :236-284 / v24 :240-288):
  - **0:** one cursor fills each quarter to `size/4`, wrapping (duplicates possible: 28f).
  - **1:** decode the skip list (§3.6.3); with `int idx` and a skip cursor fill each quarter to `size/4`: if the cursor has entries and `idx == *cursor`, advance the cursor, else take `combined[idx]`; `idx += 1` with wrap. The cursor never rewinds (28i). Each step pushes a member or consumes a skip entry, so the loop ends within `N·size/4 + skip_list.len()` steps (A-m5).
  - **2, 3, unknown:** `N` empty quarters (28g).

#### 3.6.3 New quarters (`new_quarters`) and the skip-list encoder

On `all = mn_list(work)` (v23 :339-456 / v24 :343-466):

1. **No snapshot, 28e-1:** `all.counts().enabled < size/4` → `N` empty quarters (v23 :356 / v24 :362).
2. **`skip_removed`** = `cycle_base.h + 1 >= V19Height || testnet` (v23 `DeploymentActiveAfter(cycle base, V19)` :363, v24 height test :371; equal because V19 is buried).
   - **28d.** Pre-V19 mainnet cycles (1,737,792 … 1,898,784) keep removed masternodes in the used sets; their old-state entries can block a current masternode through `try_add` and can be re-selected into another quorum's new quarter.
   - **28d, reconstruction (A-m3).** The encoder's `combined` then contains removed masternodes in its used tail, but `build_quorum_snapshot`'s bitset spans only `sorted(all)`, so a later cycle's decoder rebuilds a differently shaped `combined` and the skip indices address it. The reconstructed quarter can therefore differ from the original new quarter for these cycles, in addition to 28f. What consensus uses is the reconstruction: tests compare quarters against the **quarters embedded in the later quorums**, never against the original new quarter (§7 LQ1b-D).
3. **Used sets:** for `idx` in `0..N`, for each previous cycle H−C, H−2C, H−3C, for each member `mn` in quarter order: skip if `skip_removed && !all.contains(mn)`; skip if `all.is_pose_banned(mn)`; `used.try_add(mn)` and `used_by_index[idx].try_add(mn)`, errors swallowed (28b; v23 :367-385 / v24 :375-394). Insertion order decides which of two colliding entries wins.
4. **Candidates:** `not_used` = entries of `all` (`for_each(false)`) not in `used` and not banned; `combined = sorted(not_used, m_0) ‖ sorted(used, m_0)` (the latter over each entry's own older state).
5. **Fill and encode** (v23 :408-447 / v24 :416-456): `skip: Vec<i32>`, `first_skipped = 0`, `idx = 0`; for each `i`: `used_count = used_by_index[i].total()`, `updated = false`, `start = idx`; while `|q_i| < size/4 && used_count + |q_i| < |combined|`:
   - if `!used_by_index[i].contains(c[idx])` and `try_add(c[idx])` succeeds → push, `updated = true`; otherwise a **skip**;
   - skip encoding (28c): `first_skipped == 0` → `first_skipped = idx`, push `idx`; else push `idx − first_skipped` as `size_t` wrapped and narrowed to `int` (negative entries legal);
   - `idx += 1` with wrap; back at `start` with `!updated` → `N` empty quarters, **no snapshot** (28e-2; v23 :440 / v24 :448); else reset `updated`.
   - The guard can stop a quarter short (28f).
6. **Store** (v24 `storeSnapshot = !predicting`, :458-463; v23 always, :451-453): `build_quorum_snapshot(all, used, skip)` → `rows.store` (§3.7). An empty `combined` with `enabled >= size/4` (every masternode unconfirmed) stores a mode-0 row (A).

**Decoder mirror** (v23 :252-261 / v24 :255-265): `first = 0`; for each `s`: if `first == 0`, `first = s as usize` (sign-extending) and push `s`; else push `(first + s as usize) as i32`. Over every encoder output this returns the encoder's absolute indices (property-tested, §6.5); a negative first entry is reachable only from imported rows.

#### 3.6.4 `build_quorum_snapshot`

v23 :303-337 / v24 :307-341: `active` has `all.counts().total` bits, all false; walk `sorted(all, m_0)` with `j`, `active[j] = used.contains(e)`; `mode = skip.is_empty() ? 0 : 1`. v23's extra unused argument changes nothing.

#### 3.6.5 Assembly

`prev = [H−C, H−2C, H−3C]`, `new = new_quarters(…)`, quorum `i` = `prev[2][i] ‖ prev[1][i] ‖ prev[0][i] ‖ new[i]`; result carries `snapshot: Option<…>` (`None` on 28e).

#### 3.6.6 The rotated path and the first cycles

- Compute the whole cycle, return `by_index[quorum_index]`, store the snapshot (`rows.store`) if `Some`, keyed by the **cycle base** (v24 `replace_index`, :705).
- The first three rotated cycles have no previous rows, so mainnet quorums have 15, 30, 45 members and commit only nulls: `DIP0024QuorumsHeight = 1,737,792 + 3·288 + 42 = 1,738,698`.
- Testnet: rows exist from cycle base 769,824 (the first rotation-enabled base). `quorum rotationinfo` at 769,824 … 770,400 fails "Cannot find quorum snapshot"; from 770,688 it succeeds (H−C row mode 1, 529 bits, 237 set, 83 skip entries; 770,976: 109 entries). See §6.3.6 for how that probe is classified.
- A missing row also follows either early return (28e); regtest and devnet with few masternodes hit them.

#### 3.6.7 The first V20 cycle, rotated

Rotated quorums always take the `work` list; both modifier forms hash `(type, work.hash)` when there is no coinbase CL, so the first rotated V20 cycle (mainnet 1,987,776) is unaffected. Each previous cycle's modifier is recomputed with its own V20 test, so straddling cycles mix eras correctly.

#### 3.6.8 The v24 predictor (RPC only)

v24 :589-632 (`quorum dkginfo`, O17 DKG-15, RP3b): `None` for an out-of-range index or a wrong work block; `None` pre-V20; non-rotated Evo-only if `quorum_height + 1 >= V19Height`; rotated as §3.6.5 with `predicting = true` (any missing cycle or row → `None`, never emits). v24 surface only.

### 3.7 Snapshots as effects

#### 3.7.1 Where Core writes `llmq_S`

`StoreSnapshotForBlock` (v23 llmq/snapshot.cpp:283-292 / v24 :320-330): raw `GetRawDB().Write` outside the EvoDB transaction, key `("llmq_S", SHA256d(u8 type ‖ cycle-base hash))`, plus the 32-entry LRU; reached only from `new_quarters` on a members-cache miss. Rows are never erased at either pin (`llmq_S` has no erase site); `-reindex`/`-reindex-chainstate` wipe the EvoDB.

| Site (v23 / v24) | When | Row | Survives `Err` / `fJustCheck`? |
|---|---|---|---|
| `PreComputeQuorumMembers` at the start of quorum `ProcessBlock` (blockprocessor.cpp:176 / :223) | every attempt that **reaches** the quorum `ProcessBlock`: `ConnectBlock` (validation.cpp:2451 / :2663), `TestBlockValidity` on its dummy index (template hash; :4560-4564 / :4943-4947), VerifyDB 4, `RollforwardBlock` (:4787 / :5197) | first time for a new hash | yes; never erased |
| the same in `UndoBlock(reset_cache = true)` (:387 / :523) | disconnect, VerifyDB 3 | overwrite (the type's members caches were just cleared) | n/a |
| `CFinalCommitment::Verify` (`CheckLLMQCommitment`, `qfcommit` intake; commitment.cpp:173 / :177; v24 `VerifySignatureAsync` :53) | QcTx; P2P | overwrite on a members-cache miss | n/a |
| PoSe branch (specialtxman.cpp:456 / :723), `BuildQuorumFromCommitment` (quorumsman.cpp:80 / :71; reached by `GetQuorum` on a quorum-object-cache miss, which includes every newly scanned base of a `ScanQuorums` miss or continuation, beyond the returned prefix too (§3.8.1); so also `quorum list/listextended/info`, `selectquorum`, and post-V20 `mnlistdiff`/`qrinfo` via `BuildQuorumChainlockInfo`, smldiff.cpp v23:199-200 / v24:221-226), DKG init, connection management, `llmq/debug` | runtime | overwrite on a members-cache miss | n/a |

Only PreCompute can write a row for a new hash: every other caller's base is checked to be on the active chain or an ancestor of `pindexPrev` (A: `qfcommit` v24 blockprocessor.cpp:152; `CheckLLMQCommitment` :285).

**Not reached:**
- the mempool (`qc-not-allowed` before `CheckSpecialTx`, v23 validation.cpp:776-778 / v24 :855-857);
- pre-DIP3 blocks (`ProcessBlock` returns first). **Default regtest** (A-n1): `DIP0024Height = 1`, `DIP0003Height = 432` (chainparams.cpp v23:788, :795 / v24:818, :825), so cycle bases 24 … 408 are rotation-enabled but write nothing; base 432's work list is the empty pre-DIP3 list, so 28e-1 writes nothing either. Every FT chain crosses this; it is a constructed vector (§6.1);
- attempts failing **before** the quorum `ProcessBlock`: in `ProcessSpecialTxsInBlock` (v23 specialtxman.cpp:522-622 / v24 :797-907) the CbTx checks, the special-tx loop and `CheckCreditPoolDiffForBlock` come first, and `CheckBlock`, the contextual checks and `ConnectBlock`'s preamble precede all of them;
- **v24's dual chainstate** (assumeutxo: `ActivateSnapshot`, `EvoDbIdentity::SNAPSHOT`, regtest-only `m_assumeutxo_data`): shares the EvoDB, out of scope under NG-6 (A-n1).

A row's bytes are a function of the inputs at the time of the write: the work block's ancestry, the parameters, the previous rows, and the modifiers, which read the work blocks' coinbase CL and therefore their block-data availability, the BLS context and the profile (§3.3, §3.7.3). Core writes unconditionally (`GetRawDB().Write` plus the LRU insert), so a later write with unchanged inputs stores the same bytes and a later write after an input changed **replaces** the row. rdashd writes at the same points with the same semantics (§3.7.2, §3.7.4); rev1's "every rewrite is identical" claim is withdrawn (GPT M1).

#### 3.7.2 Rust writes: `CQuorumSnapshotManager` semantics (rev2, ruling 1)

- **One interface.** The pure functions read rows with `rows.get` and write them with `rows.store` (§2.3), exactly where Core calls `GetSnapshotForBlock` (§3.6.2) and `StoreSnapshotForBlock` (§3.6.3 step 6). `store` is an unconditional overwrite keyed by (type, cycle-base hash), visible to every later `get`: Core's raw `Write` plus its write-through 32-entry LRU (v23 llmq/snapshot.cpp:264-292 / v24 :301-330). There is no "write if absent" and no equality check.
- **The same write points as Core.** The pure layer has no cache and stores on every rotated computation it performs. It performs one exactly when Core does, because every production computation is reached through the process-wide members facade (§3.7.4): a hit returns members without computing or writing, as in Core; a miss computes and writes. The facade's three entry routes are Core's: `QuorumMemberSource::members` (commitment checks, PoSe, DKG, RPC), `QuorumPrecompute::precompute` (LQ2's `process_block`/`undo_block`, rev3), and QM1's `GetQuorum` builds, including those a scan makes for bases it does not return (§3.8.1). The uncached composition is `reference`-only (§2.5).
- **Inside a connect attempt** (CH7b's `AttemptRows`): `store` records `Effect::StoreQuorumSnapshot` in the attempt's effect log, so the write survives a later `Err` or `fJustCheck` (A1 §2.10.1), **and** updates an attempt overlay; `get` reads the overlay first, then the store; the last write wins. This is Core's behaviour, where a raw write is visible to the next read in the same `ConnectBlock`. After the attempt CH7b applies the log in order, regardless of the result, before the next attempt starts and before the next attempt's read view is taken. rev1 dropped the overlay on the argument that every rewrite is byte-identical; under overwrite semantics that argument does not hold in general, and the overlay is the direct model (the reversal of rev1's B-m8 choice).
- **Outside connect attempts** (runtime callers of the members source: QM1, DK1, `qfcommit` intake, RPCs): `store` is a synchronous single-row overwrite through CH7b's evo writer, serialized with validation (A1 §2.1 rule 4) and applied before the call returns; `get` reads the store. Core writes from the calling thread under its own locks; serializing with validation is one of Core's possible schedules.
- **Consequence.** Stored rows equal Core's in every state reachable by both, including states in which a recomputed modifier differs from the one used at an earlier write: Core overwrites the row at a members-cache miss, and so does rdashd. Verification compares row digests (§6.2, §6.6); no in-process assertion compares rows.

#### 3.7.3 The work-block coinbase source

CH7b implements `coinbase_facts(work)`: `BlockDataUnavailable` exactly when Core's `ReadBlockFromDisk` fails for a node in the same state, i.e. the block's data is not available (pruned); otherwise the coinbase facts. A disk read error is a node error (`StoreError`, fail-stop), not "no CL" (deviation D2). The L5 side table (§5.3) must give the same answers.

#### 3.7.4 Runtime members cache (Core-faithful; it decides when rows are written)

`dashd-chainstate` owns one `MembersCache` per process and the **members facade** over it, shared by every caller (validation, precompute, QM1's quorum builds, DK1, LQ5, RPC), as Core's function-level statics are (v23 utils.cpp:571-574 / v24 :636-639). rev1 called it unobservable; it is not, because its warm or cold state decides whether a call recomputes and therefore whether it (re)writes a row (GPT M1). It ports Core exactly:
- **Structure.** Per configured type, two `unordered_lru_cache`s with the §3.8.3 semantics: `members_by_base` (Core `mapQuorumMembers`, key: base hash) and, for rotated types, `members_by_cycle_index` (`mapIndexedQuorumMembers`, key: (cycle hash, index)). Both have capacity `keepOldConnections` (v23 `InitQuorumsCache`, utils.h:88-94; v24 `PerLlmqTypeCache::Init`, llmq/cache.h:45-50), truncation at 2× to the most recently accessed.
- **The facade**, `members(t, base, reset, effects)`, in Core's order (v23 :569-659 / v24 :634-730):
  1. `members_gate(t, base)` false → return empty. No cache is touched, not even on `reset` (v23 :579-582 / v24 :644-647).
  2. `reset` ? clear the type's `members_by_base` : `get(base)` (hit → return it).
  3. `members_route(t, base)`:
     - `NonRotated` → `compute_quorum_members` → step 5;
     - `RotatedIndexOutOfRange` → return empty, insert nothing (v23 :615-617 / v24 :680-682);
     - `Rotated { cycle_base, index }` → `reset` ? clear the type's `members_by_cycle_index` : `get((cycle_base, index))` (hit → insert into `members_by_base`, return it). The index guard therefore runs before the indexed reset, as in Core.
  4. Rotated miss: `compute_by_quarter_rotation(cycle_base)` (this writes the row through the caller's `SnapshotRows`), then insert indices `0..N` in order into `members_by_cycle_index`; the result is entry `index`.
  5. Insert into `members_by_base`; return.
  An `Err` leaves both caches as they were after step 3 (Core's exception propagates before its inserts); it is fail-stop anyway.
- **Entry routes.** `QuorumMemberSource::members(t, base, effects)` = `members(t, base, false, effects)`. `QuorumPrecompute::precompute(block, mode, effects)` = `members(t, block, mode == Undo, effects)` for each `t` in `precompute_targets(block)`, in configured order; the first `Err` is returned (fail-stop). QM1's `GetQuorum` build = `members(t, base, false, effects)` once per build (§3.8.3).
- **`reset` is a per-type members-cache reset behind Core's gates** (rev3, finding 4). It clears that type's base cache after the gate and its indexed cache after the index guard, then recomputes and repopulates. It deletes no snapshot row; it is not a scan-cache or quorum-object-cache reset and not a restart. It comes only from `QuorumPrecompute` in `Undo` mode (v23 blockprocessor.cpp:387 / v24 :523). `precompute_targets` holds only rotation-enabled types at heights `h % interval == 0`, so an undo resets exactly the eligible types at cycle-base heights; an undo at any other height touches no cache. Nothing else ever clears the caches; a restart starts cold.
- **Process state, not attempt state** (rev3 ruling 1). The caches are never snapshotted or rolled back with a connect, template (`fJustCheck`) or disconnect attempt. Mutations made inside an attempt (warm inserts, LRU bumps, undo resets) survive a later `Err`, a `fJustCheck` discard and a failed reorg, as Core's statics do (GPT r3 `members-failed-attempt-retains-cache-*`). Members computed through the attempt's overlay rows stay cached after the attempt ends; that is Core's behaviour too.
- **Hits are not recomputed.** A hit may differ from a fresh computation (for example after the work block's data became unavailable); Core returns the cached value, and so does the facade. The xcheck comparator therefore checks only misses against `reference::get_all_quorum_members_uncached`, with a throwaway `SnapshotRows` overlay whose writes are compared, never applied.
- **Not tunable.** Sizes, keys and access-order bookkeeping are Core's. Core's callers interleave under two separate locks; serializing them is one of Core's schedules, as for the scan cache (§3.8.3). `EV2-dml.md` §3.8 classifies whether Core hits this cache as observation-class in digests.

### 3.8 `scan_quorums` and Core's scan cache (A-M1 ruling)

#### 3.8.1 Body

v23 quorumsman.cpp:199-307 / v24 :170-286.

1. **Gate:** empty if `count == 0` or `!is_quorum_type_enabled(t, start)` (at `start`, not its parent). No cache access.
2. **Store block:** `cs = start.h − start.h % interval`, `ms = cs + window_start`, `me = cs + window_end`; `start.h < ms` → empty if `me < interval`, else `store = ancestor(start, me − interval)`; `start.h > me` → `store = ancestor(start, me)`; otherwise `store = start`. (`scan_store_block`.)
3. **Cache** (skipped by v24's `chain` overload): look up `hash(store)`.
   - hit with `len == count` → return it; `len > count` → return the first `count`; no write in either case;
   - hit with `0 < len < count` → **continuation**: `n = count − len`, `from = cached.last().base.prev` (none → return empty, no write);
   - miss → `n = max(count, keep_old_connections)`, `from = store`.
   - `chain` overload → as a miss, but no lookup and no write.
4. **Bases:** rotated types (`p.use_rotation`): `indexed_until(from, n)` = for `cycle = 0, 1, …` take `[nth_mined_by_index(t, from, i, cycle) for i in 0..N]` dropping `None`, stop at an empty round, truncate the concatenation to `n` (round-based, truncating inside a round; v24 blockprocessor.cpp:796-815). Non-rotated: `mined_until(t, from, n)`. All bases are read before any quorum is built.
5. **Materialize, in order, every base** (rev3, GPT r3 finding 2; v23 quorumsman.cpp:276-292 / v24 :248-271). For the `i`-th new base (from 0): `populate_cache = prefix_len + i < keep_old_connections`, where `prefix_len` is the cached prefix's length on a continuation and 0 otherwise. This is Core's `vecResultQuorums.size() < keepOldConnections`, counted before the push. Then `q = mat.get_quorum(t, base, populate_cache)`.
   - This runs for **every** base from step 4, including those past `count` that step 6 discards (a miss asks for `max(count, keep)`). A TEST_DIP0024 count-1 miss over four commitments calls `get_quorum` for bases 48, 49, 24, 25 and returns `[48]`; repeating the scan hits the scan cache and calls nothing (both pins; GPT r3 `scan-trace-*`).
   - `Ok(None)` (Core's `nullptr`: "Unexpected missing quorum") → return empty and write nothing to the scan cache. The builds already made keep **all** their effects: object-cache inserts, members-cache state, rows written through the members facade, contributions. "No scan-cache write" is not "no effects".
   - `Err` → propagate (fail-stop, §2.6). Rows written synchronously by earlier builds outside an attempt are already durable; an attempt's rows are in its effect log, which is applied regardless of the result (§3.7.2).
6. **Result** = cached prefix (on continuation) ‖ materialized quorums. If non-empty and the cache is in use: **write** the first `min(len, keep_old_connections)` under `hash(store)` (overwriting). Return the first `min(len, count)`.

`scan_quorums_from` implements steps 3–6 given the lookup result and a materializer; the facade does steps 1–2, the lookup and the write. A satisfied hit (step 3, `len >= count`) and the continuation bail return without a single `get_quorum` call.

#### 3.8.2 Ancestry

Core reads the active chain's index filtered by mined height `<= from.h` and maps base heights with `GetAncestor`. Every Core call site passes an active-chain block or the block being connected/tested (whose ancestry is the active chain), so ancestry-resolved reads through `at` are equivalent at every site.

#### 3.8.3 The cache, Core-faithful (QM1 owns it; A-M1, A-n3)

- **Structure.** One cache per configured type, per `CQuorumManager` (process-wide): v23 `std::map<LLMQType, Uint256LruHashMap<vector<CQuorumCPtr>>>` filled at first use (quorumsman.h:89-91, quorumsman.cpp:233-244); v24 `PerLlmqTypeCache` initialized for every configured type (quorumsman.h:89, llmq/cache.h:25-102). Same sizes, same LRU. Never cleared, not even on reorg (keys are block hashes); reset only by a restart.
- **Capacity:** `max_cycles(keep) × (window_end − window_start)` with `max_cycles(n) = use_rotation ? n / N : n` (60_75: 2 × 8 = 16; 400_60: 5 × 8 = 40; 100_67: 25 × 8 = 200).
- **`unordered_lru_cache` semantics** (unordered_lru_cache.h v23:15-113, v24 identical plus an assert):
  - `get(key)` bumps the entry's access counter;
  - `emplace(key, v)` **overwrites** an existing value and bumps its counter, else inserts;
  - after each insert or overwrite, if the size exceeds `2 × capacity`, keep the `capacity` entries with the highest access counters and drop the rest.
- **What the cache holds.** Core caches `CQuorumCPtr`s; QM1 caches its quorum objects (`M::Quorum`), not commitments or base lists. A hit returns the retained objects with the members captured when each was built, even if the members cache has since been reset or would now compute differently. It makes no `GetQuorum` call, no quorum-object-cache access and no members access (v23 quorumsman.cpp:245-254 / v24 :215-224). On a continuation, the prefix objects are reused and only the new bases are materialized.
- **Facade order** (QM1): gate (empty, no cache touch) → store block (bail empty) → under the lock `get(hash(store))` → release → `scan_quorums_from` with the materializer (its `get_quorum` calls happen here, in base order) → under the lock `emplace` if `cache_write`. Core also releases the lock between lookup and write, so two concurrent scans can interleave; QM1 serialises scans of one type, which is one of Core's possible schedules. This is the only point where "same process history" must include thread scheduling; O15 records it as such.
- **The duplicate padding** (A-M1). With `count <= keep`, continuation happens exactly when fewer than `count` commitments are mined at or below `store`. For rotated types the fresh list is then exhaustive, and continuation from `back().base − 1` returns commitments already listed (any index whose oldest commitment predates `back()`'s cycle), appended as **duplicates**; the overwrite makes every later call at that store block see them. Non-rotated types are unaffected: no other commitment of the type is mined between a quorum's base and its mining height (`window_end < interval`).
- **Who reaches it** with rotated types: `SelectQuorumForSigning` (count `N`: IS and recsig verification), connection management (`keepOldConnections`) and key cleanup (`keepOldKeys − held`; rotated types have `keepOldKeys == keepOldConnections`), `quorum list` (default `N`), `node/interfaces` (`N`), `quorum memberof`. Today only young rotated chains have fewer than `count` commitments under a store block: regtest, devnet, and historically the first DIP24 cycles. **Selection** changes only for `N >= 3` (60_75, including on devnet), through the `signer > quorums.size()` guard (§3.9).
- **Rust.** QM1's `ScanCache` ports the above exactly; every QM1/SG1/LK1/IX/RP scan goes through the facade. Differential tests compare against a Core reference warmed with the same call sequence, and compare its event trace and written rows, not only the bases (§6.2, §6.6). r0's fresh-scan deviation (Q2) is withdrawn.

**The materializer (QM1's `QuorumMaterializer`, Core `GetQuorum`; rev3).** LQ1 fixes the contract the scan relies on; QM1 implements it (§8 Q7 for the consensus subset before QM1a).
- **Steps** (v23 quorumsman.cpp:391-414, `BuildQuorumFromCommitment` :66-112 / v24 :518-536, :58-104):
  1. **Mined check.** v23: `HasMinedCommitment(t, hash)`, an EvoDB existence test behind v23's `mapHasMinedCommitmentCache`. v24: the commitment's mined block is in the active chain, or in the given chain for the chain overload. LQ2's index answers it; not mined → `None`, no cache access.
  2. **Quorum-object cache.** Per type, an `unordered_lru_cache` of capacity **`keepOldKeys`**: v23 `InitQuorumsCache(mapQuorumsCache, …, /*limit_by_connections=*/false)`, quorumsman.cpp:42; v24 `mapQuorumsCache.Init(…, false)`, :42. Never cleared. `get(hash)` hit → return the retained object; its LRU bump counts.
  3. **Build on a miss.** `GetMinedCommitment` (none → `None`), then the members facade once (`members(t, base, false, effects)`, §3.7.4), then construction. If `populate_cache && size == 1`: insert into the object cache and return. Otherwise read the contributions, or build them through DK1 (only on a node with a DKG session manager, which then makes the second members access below) and write them; queue warming if `populate_cache` and a vvec exists; then **always** insert into the object cache.

     `populate_cache` therefore never changes the build's **initial** members access, its row writes or the object-cache insert (rev4 narrowing). It does decide, for size-1 types, whether the contribution path runs at all. On a node with a DKG session manager, that path can make a second members access (next bullet).
- **Chain overload** (v24 :539-555, used by v24 validation's asset-unlock scan, `ScanCache::Bypass`). The mined check runs on the given chain, and the same object cache is read and written. The scan cache is not used. QM1 binds such a materializer to that chain.
- **The DKG path's second members access (rev4, GPT r4 finding 2).**
  - *When Core makes it.* The build does not take the `populate_cache && size == 1` shortcut, no contribution record is stored, and the node has a DKG session manager. `BuildQuorumContributions` (v23 quorumsman.cpp:114-122 / v24 :106-114) then calls `GetVerifiedContributions`, which calls `GetAllQuorumMembers` again for the same base (v23 llmq/dkgsessionmgr.cpp:387-389 / v24 :145-147).
  - *Which nodes have a DKG session manager.* Masternodes, and `-watchquorums` observers (a DEBUG_ONLY option). Both pins create one in the active context (v23 active/context.cpp:39 / v24 :43) and in the observer context (v23 llmq/observer/context.cpp:23, wired at :32 / v24 llmq/observer.cpp:16, wired at llmq/net_dkg.cpp:462, :483), chosen in init.cpp v23:2193-2208 / v24:2166-2181.
  - *Contract.* The materializer **performs that access** through the members facade (`members(t, base, false, …)`) exactly when Core does. It is a `members_by_base` hit and emits a `MEMBERS` base-hit event. Under serialization its LRU bump changes no eviction order, but it is part of the event trace, so it is never omitted. For example, a cold size-1 quorum on such a node with no stored contributions makes one members access with `populate_cache = true` and two with `false` (GPT r4, both pins).
  - *Trace parity.* The §6.2 oracle context has no DKG session manager, so the LQ1 suites, CH7b's M4 runtime suite (a DKG-less node until QM1a) and any QM1a suite run in that configuration contain no second access. If QM1a's masternode/observer suite compares Core event traces, its oracle runs with the matching DKG configuration: a DKG session manager and the same stored-contribution state, so Core makes the same second access. Otherwise that suite is marked non-DKG for trace comparison, and the DKG access stream is tested separately against Core's build path: whether the second access happens, when, and its `MEMBERS` event. The event is never normalised away.
- **Effects outlive the scan.** Whatever `get_quorum` did stays done when a later base fails or the scan's tail is discarded (§3.8.1 step 5).

#### 3.8.4 Consensus callers (rev4: rotation is configuration-dependent)

- **ChainLock.** The ChainLock type is never rotated: `-llmqchainlocks` throws on a rotated type (chainparams.cpp v23:1226-1227 / v24:1270-1271). `CheckCbTxBestChainlock`'s selection is therefore non-rotated in every configuration.
- **Asset unlocks.** They scan the Platform type with `count = N + 1`, test membership, then fetch the payload's quorum with an explicit `GetQuorum` and verify against its commitment key (evo/assetlocktx.cpp v23:121-150 / v24:116-153).
  - In validation, v24 binds **both** the scan and the lookup to the validated chain, through the `chain` overloads (:165-173); the cached forms (:155-163) serve the mempool. v23 uses the cached forms everywhere (v23 :127-128).
  - QM1's facade takes `Bypass`, with a materializer bound to the validated chain for the scan and the lookup, under `V24_0` validation, and the cache otherwise. This matches each pin literally.
- **The Platform type is non-rotated only in the default configuration** (rev4, GPT r4 finding 1).
  - On devnet, `-llmqplatform` accepts any registered type and has no rotation guard at either pin (chainparams.cpp v23:1264-1285 / v24:1308-1329).
  - `-llmqplatform=llmq_devnet_dip0024` gives a rotated Platform type: interval 48, `N = 2`, keep 4 (llmq/params.h v23:296-315 / v24:301-320).
  - The asset-unlock scan is then a rotated scan of 3. A miss materializes the whole keep-sized range: with commitments at 96, 97, 48 and 49 it builds all four and returns `[96, 97, 48]`, and the repeat builds nothing (GPT r4, both pins). **So M4 validation reaches rotated member computation and row effects.**
  - Nothing in LQ1 relies on the Platform type being non-rotated. The facade, the materializer and the caches are Core-faithful for both cases, and CH7b's acceptance covers the override (§6.6, §8 Q7).
  - For the default, non-rotated Platform type, continuation equals a fresh scan, so the cached and bypass forms return equal results. Their members-cache and object-cache effects still differ, and each follows its own pin.
- **MNHF.** MNHF verification uses an explicit quorum hash. `GetQuorum` on the MNHF type (evo/mnhftx.cpp v23:142-143 / v24:145, :163, bound to the validated chain at :172) reaches the materializer and the quorum-object cache, but no scan. `-llmqmnhf` has no rotation guard either (devnet; chainparams.cpp v23:1286-1307 / v24:1330-1351), so a rotated MNHF override also reaches rotated member computation from validation.

#### 3.8.5 `ScanCommitments` (v24 only; quorum-proof RPCs)

v24 :294-409: same gate, store block and sizes, its own cache (`scanCommitmentsCache`, `std::map<type, Uint256LruHashMap<vector<CFinalCommitment>>>`, quorumsman.h:86-87); the continuation key is the last entry's quorum hash, resolved to the base block for rotated types and to the mined block for non-rotated ones (:361-373), then `.pprev`; fail-whole on a stored null commitment. `scan_commitments_from` ports it; QM1 owns its cache. v24 surface only (DKG-16).

### 3.9 `select_from_scan` (quirk 30)

v23 :693-753 / v24 :724-775 (and :800-847).

- **Start block** (`select_start_height`): `chain[sign_height − offset]` (`SIGN_HEIGHT_OFFSET = 8`, quorumsman.h:184 / :210), `-1` = tip, none if the result is `< 0` or `> tip`.
- **Rotated** (`is_quorum_rotation_enabled(p, start)`), over the facade's scan with `count = N`:
  - none if the scan is empty;
  - `n = signer_index_bits(N)` (§3.10); `b = LE64(selection_hash[24..32])`; `signer = ((1 << n) − 1) & (b >> (64 − n − 1))`: bits 58–62 for n = 5, bit 62 for n = 1 (30a);
  - **30b (corrected, A-m1):** `signer > quorums.size()` compares the signer with the **number of scanned quorums**, not the index range. When fewer than `N` quorums are scanned (early cycles, young chains) an index that is present can be rejected (fresh scan of 17 quorums containing index 20: signer 20 → none). With the duplicate padding of §3.8.3 the size grows and the same signer can then resolve. Ported literally;
  - otherwise the **first** scanned quorum with `quorum_index == signer` (30c). Because round 0 lists the newest commitment of every index first, an index that failed recently resolves to its latest mined quorum, however old. A never-mined index has no entry and resolves to none.
- **Non-rotated:** `argmin` over the scan of `SHA256d(u8 type ‖ quorum_hash ‖ selection_hash)` compared as `uint256` (memcmp, uint256.h:55-59 at both pins); ties by position (impossible for distinct hashes).

### 3.10 The `log2` site (DEC-29 D4)

#### 3.10.1 Domain

`int n = std::log2(llmq_params.signingActiveQuorumCount)` (v23 quorumsman.cpp:717; v24 :739, :811), inside `if (IsQuorumRotationEnabled(…))`, false unless `useRotation`. Rotated types at both pins: `TEST_DIP0024` (2), `DEVNET_DIP0024` (2), `60_75` (32). No chainparams code writes `useRotation` or `signingActiveQuorumCount`; the overrides touch size/threshold fields of non-rotated types only (v23 chainparams.cpp:740-747, :979-986 / v24 :770-777, :1019-1026) or re-point roles (`-llmqchainlocks` refuses a rotated type, v24 :1270). **Domain: {2, 32}** (confirmed by A).

#### 3.10.2 Call shape in every shipped build

18 sites in the 12 GPG-verified builds (v23: 1 per build; v24: 2), listings in `/work/scratch/lq1/fp/listings/` — **including the three win64 sites** (`23.1.8-win64-callsites.s`, `24.0.0-rc.3-win64-callsites.s`, from `find_win_log2.py`; A-m2):

| Platform | Callee | int → double | double → int |
|---|---|---|---|
| x86_64 Linux | glibc `log2@GLIBC_2.29` | `cvtsi2sd` (exact) | `cvttsd2si` |
| aarch64 Linux | glibc | `scvtf` | `fcvtzs` |
| riscv64 Linux | glibc | `fcvt.d.w` | `fcvt.w.d …, rtz` |
| win64 | static mingw-w64 `log2` at `0x140c5d780` (v23) / `0x140d91600` (v24), identical up to jump targets | `cvtsi2sd` | `cvttsd2si` |
| x86_64 macOS | Apple `_log2` | `cvtsi2sd` | `cvttsd2si` |
| arm64 macOS | Apple `_log2` | `sshll` + `scvtf` | `fcvtzs` |

The win64 routine computes `fyl2x(1.0, x)` for these inputs (`fyl2xp1` only when `|x − 1| <= 0.29`), then `fstp` to double. The x87 precision-control field does not affect `FYL2X` (Intel SDM vol. 1 §8.1.5.2).

#### 3.10.3 Results

Every measurement returns `log2(2) = 1.0`, `log2(32) = 5.0` exactly, and `(int)log2(n) == ⌊log₂ n⌋` for every n in 1..65,536:

| libm | Method |
|---|---|
| glibc 2.43 x86_64, FMA ifunc and generic (variant asserted by resolved address) | `log2probe`, with and without `GLIBC_TUNABLES` |
| glibc 2.39 x86_64 (first release with the FMA ifunc), both variants, asserted | ubuntu:24.04 container (`fp/glibc-2.39-ubuntu2404.txt`) |
| glibc 2.31, 2.36 x86_64 (generic only: no double-`log2` ifunc before 2.39) | ctypes probe in the CS2 containers |
| glibc 2.43 aarch64, riscv64 | qemu-user |
| mingw-w64 x87 `log2`, both win64 builds, FCW 0x37F and 0x27F | `winlog2.c`, run natively |

**glibc source bridge** (`fp/glibc-bridge/SUMMARY.md`; A-m2): `e_log2_data.c` is numerically identical in all 15 releases 2.29–2.43; `e_log2.c` is identical from 2.31 to 2.42, 2.29–2.30 differ only in alias macros, 2.43 only in `double_t` → `double` (no change where FLT_EVAL_METHOD is 0, as on x86_64, aarch64, riscv64). glibc builds with no `-ffp-contract=off`.

**Proof and gaps.**
- **Count 32 → 5.0 (mainnet, testnet).** 5.0 is interior to its binade, so a routine whose error at this input is **proven** below 1 ulp returns it exactly (both neighbours are exactly 1 ulp away).
  - **glibc ≥ 2.29:** proven by the authors' documented worst-case bound, 0.547 ulp (`e_log2.c` :136-137), and measured on 2.31, 2.36, 2.39 and 2.43.
  - **win64:** measured on both builds under both control words.
  - **Apple libm (x86_64 and arm64 macOS builds): open evidence gap; no parity claim for macOS hosts** (ruling 2; GPT M2). Gladman et al.'s 0.515 ulp (Aug 2026, Table 3) is the largest error found by testing, not a bound proven for every input, OS version and architecture, so it does not establish `log2(32.0) == 5.0`. The check that settles it is §3.10.5.
- **Count 2 → 1.0 (regtest `TEST_DIP0024`, devnet `DEVNET_DIP0024`).** 1.0 is a binade boundary (its lower neighbour is half an ulp away), so an error bound does not force exactness; only measurement does. Proven by measurement for: x86_64 generic glibc 2.31–2.43 (no FMA instruction exists in baseline x86-64, so code generation cannot contract, and the source is identical; endpoints measured), x86_64 FMA glibc 2.39 and 2.43, aarch64/riscv64 glibc 2.43, win64. **Open gaps:**
  1. **Apple libm** (x86_64 and arm64 macOS builds): not measured; the same §3.10.5 check covers it.
  2. **FMA-target glibc builds not measured:** aarch64 and riscv64 with glibc 2.29–2.42, x86_64 FMA ifunc 2.40–2.42. The source and data are identical, but contraction choices belong to the compiler that built each glibc, so a different rounding of the final sum is not excluded by argument.

  The count-2 gaps affect only regtest and devnet. The Apple count-32 gap concerns macOS-hosted dashd on mainnet and testnet. The Rust side is `ilog2` on every host either way.
- For non-powers of two the truncation is safe by margin: `⌈log₂ n⌉ − log₂ n >= log₂((n+1)/n) > 10⁻⁵` for n `<=` 65,536.

#### 3.10.4 Rust

```rust
/// Core `int n = std::log2(signingActiveQuorumCount)` (v24 llmq/quorumsman.cpp:739, :811; v23 :717).
/// LlmqCtx::new rejects a rotation type outside 1..=65536 (design §3.10).
pub fn signer_index_bits(count: u32) -> u32 { debug_assert!((1..=65536).contains(&count)); count.ilog2() }
```

Tests: `(2) == 1`, `(32) == 5`; every `use_rotation` type in BS5's tables for four networks × the override set passes the domain check; `ilog2` equals `⌊log₂ n⌋` for 1..=65536. The probes, listings and the glibc fetch script move into the LQ1a PR as `contrib/node/llmq-log2/` with a `reproduce.sh` (reusing CS2's download and GPG check), re-run at every re-pin with CS2's F2 checklist (DEC-35 D9).

#### 3.10.5 The macOS check (ruling 2)

One file, built and run on a Mac with Apple's toolchain against the system libm (the manager asks pasta to run it):

```c
/* log2check.c: does Apple's system libm return exact results at Core's two inputs?
 *   clang -O0 -fno-builtin -arch arm64  log2check.c -o log2check-arm64  && ./log2check-arm64
 *   clang -O0 -fno-builtin -arch x86_64 log2check.c -o log2check-x86_64 && ./log2check-x86_64
 *   sw_vers; uname -m; otool -L log2check-arm64 log2check-x86_64
 * x86_64 runs natively on an Intel Mac, or under Rosetta 2 on Apple silicon (record which).
 * Expected: (int)log2(2.0)=1 (int)log2(32.0)=5 log2(2.0)=0x1p+0 log2(32.0)=0x1.4p+2
 */
#include <math.h>
#include <stdio.h>
int main(void)
{
    volatile double two = 2.0, thirty_two = 32.0;   /* volatile: clang cannot evaluate log2 at build time */
    double a = log2(two), b = log2(thirty_two);
    printf("(int)log2(2.0)=%d (int)log2(32.0)=%d log2(2.0)=%a log2(32.0)=%a\n", (int)a, (int)b, a, b);
    return 0;
}
```

- **Why it is the right check.** Both release macOS builds call the system `_log2` through a stub and truncate with `cvttsd2si` (x86_64) or `fcvtzs` (arm64) (§3.10.2; listings `*-darwin-*.s`). The program calls the same library function and truncates the same way: C's `(int)` cast, like Core's `int n = std::log2(…)`, truncates toward zero. `-fno-builtin` and `volatile` keep clang from folding the call; `otool -L` shows the binary links `/usr/lib/libSystem.B.dylib`.
- **Scope.** Apple's libm ships with the OS, so a result holds for the macOS versions tested; `sw_vers`, the architecture and native-or-Rosetta are recorded with the output in `contrib/node/llmq-log2/` (§3.10.4), and any claim is limited to those versions.
- **If `(int)log2(32.0)` prints 4** (or `(int)log2(2.0)` prints 0):
  - *Core:* macOS dashd computes `n = 4` for LLMQ_60_75 and takes `signer = 15 & (b >> 59)` (bits 59–62, indices 0–15) where Linux and Windows dashd take bits 58–62 (indices 0–31); for `b = 20 << 58` macOS picks index 10 and Linux index 20. macOS dashd would select a different quorum, or none, for rotated selection (InstantSend locks, recovered signatures, its own signing), so mainnet nodes on macOS would diverge from Linux and Windows nodes in Core itself. `(int)log2(2.0) = 0` would make `signer` always 0 on regtest and devnet.
  - *LQ1:* a quirk row (Q-LQ1-4, "Apple libm `log2(32.0) < 5` on macOS versions …: macOS dashd uses `n = 4`") and a macOS-specific note in §3.10 and the release notes. rdashd keeps `ilog2`, the Linux and Windows behaviour, on every host including macOS, because it never calls libm here; no macOS-dashd emulation profile is proposed. The manager decides how and where the Core finding is recorded and reported.
- **If both print the expected values:** the Apple gap closes for the tested versions, and §3.10.3 is updated with the measurement.

### 3.11 Quirk register (R4 §1.15, refined)

| # | Behaviour | Where |
|---|---|---|
| 28a | `break` on a missing previous-cycle row; later quarters stay empty | §3.6.2 |
| 28b | `AddMN` exceptions swallowed in the used sets (H−C, H−2C, H−3C order per index) and in the fill (skips); judged on the entry's own state | §3.6.3 |
| 28c | skip-list encoding: the first non-zero skip index is the base (a skip at 0 keeps base 0); `size_t` wrap then `int`, negative entries legal; decoder mirrors it | §3.6.3 |
| 28d | `skipRemovedMNs` = `cycle_base+1 >= V19 || testnet`; before it removed masternodes stay used, can be re-selected, and make the later reconstruction differ from the original new quarter | §3.6.3 |
| 28e | no row when `enabled < size/4` (28e-1) or after a full fill pass without progress (28e-2) | §3.6.3 |
| 28f | decoder fills to `size/4` with wrap (duplicates possible); encoder may stop short | §3.6.2–3 |
| 28g | modes 2, 3, unknown → empty quarters | §3.6.2 |
| 28h | bitset sized to `total`, indexed over valid, confirmed entries in score order | §3.6.4 |
| 28i | the decoder's skip cursor never rewinds | §3.6.2 |
| 29a | non-rotated list gate `ActiveAfter(base, V20)` vs modifier gate `ActiveAfter(base−8, V20)` | §3.3 |
| 29b | the work block's coinbase CL through the shared predicate (profile row, §3.3.1); unavailable data → hash form; caller's BLS scheme | §3.3 |
| 29c | pre-V20 non-rotated modifier uses the base hash; every other hash form the work hash | §3.3 |
| 30a | rotated signer = bits `63−n … 62` of `GetUint64(3)` | §3.9 |
| 30b | `signer > quorums.size()` compares with the scanned count: rejects present indices on short scans; padding can flip it | §3.9 |
| 30c | first scanned quorum with the index wins; a recently failed index resolves to an older quorum | §3.9 |
| Q-LQ1-1 | `ScanQuorums` cache continuation and duplicate padding (modelled, QM1) | §3.8.3 |
| Q-LQ1-2 | a rotation type enabled before rotation is (non-aligned DIP24) → empty members | §3.2 |
| Q-LQ1-3 | Evo-only filter: Platform role, non-rotated, `ActiveAfter(base, V19)` | §3.4 |

### 3.12 Errors and reject strings

LQ1 rejects nothing; Core's selection functions return empty results. The strings stay with the callers (LQ2 `bad-qc-*`; EV4 `bad-cbtx-*`; EV1 `bad-assetunlock-*`, per EV1 D8), whose tests pin the empty-member and empty-scan cases of §3.5, §3.8 and §3.9. `LlmqError::Store` and `MemberError::Store` are fail-stop (§2.6). A materializer's `Ok(None)` is Core's `GetQuorum` `nullptr`, a value that empties the scan (§3.8.1), not an error.

### 3.13 Per-profile behaviour

| Item | v23.1.8 | v24.0.0-rc.3 | Keying |
|---|---|---|---|
| Coinbase-CL predicate (5 consumers) | type, payload, sig valid | + CbTx `nHeight == h`, `diff < h` | **profile, new O17 row** (§3.3.1; owner EV4) |
| CbTx v4 payload decode | v4 field unknown | known | EVO-06 `cbtx_v4_known` (via the predicate's decode) |
| `skipRemovedMNs` formulation | `ActiveAfter(cycle base, V19)` | height test | none (buried; equivalent) |
| Null `GetAncestor` for a previous cycle | dereference | `break` | none (unreachable) |
| Asset-unlock scan in validation | cached scan | cache-free `chain` overload | QM1 facade per profile (results identical; materialization effects are each pin's, §3.8.4) |
| Predictor; `ScanCommitments`/`SelectCommitmentForSigning` | absent | RPC | `rpc.surface` (DKG-15, DKG-16) |
| Scan cache container | `map<type, LRU>`, lazily filled | `PerLlmqTypeCache` | none (same semantics) |

Everything else in §3.1 is identical at both tags.

### 3.14 Errata for A1 and R4

- **A1 §2.5 `llmq::members`:** add 28e-2.
- **A1 §2.10.1 row 1:** "every connect attempt" → "every attempt that reaches the quorum `ProcessBlock`"; "develop's predicting mode" → "v24.0.0-rc.3's (and develop's)"; add `RollforwardBlock`; "never erased" becomes "overwritten unconditionally at each members-cache miss, never erased"; runtime writes are synchronous overwrites (§3.7.2).
- **A1 Appendix A `QuorumMemberSource`:** replaced by EV0's trait (§2.6).
- **A1 §4.4 `oracle_quorum_members`:** replaced by §6.2's entry points.
- **R4 quirk 28 / E§3.4:** 28c's precise rule, 28e-2, 28d's reconstruction effect. **R4 quirk 30:** 30b's rationale.

### 3.15 Deviations from A1 and from Core

- **D1.** The member-source trait takes an `EffectSink` and returns `Result` (EV0 owns it, §2.6).
- **D2.** A disk read error on a work block is a node error, not "no CL" (§3.7.3); Core falls back silently only under a hardware fault.
- **D3 (withdrawn in rev2).** rev1 let the pure layer write on every computation and justified it by "identical rewrites". rev2 writes at Core's write points, through the Core-faithful members cache, with Core's overwrite semantics (§3.7.2, §3.7.4), so no deviation remains. rev3 closes the two routes that still bypassed the cache: precompute (now `QuorumPrecompute` over the facade) and scan builds (now the materializer).
- **D4.** Store errors are fail-stop under both profiles: the manager's ruling (r1, B-M1). Core's precedent is v24's typed `EvoDbInconsistencyError` → `state.Error` (evo/specialtxman.cpp:989-992), which the credit-pool and MNHF stores throw. Other v24 store exceptions, including the DML's (`ApplyDiff`'s `runtime_error`, `BlockDataUnavailableError`), and every v23.1.8 store exception still reach the `std::exception` catch → `failed-procspectxsinblock` (v24 :993-996; `EV2-dml.md` §2.11 n5). Local faults are not consensus (A1 I5; `EV2-dml.md` §3.11 item 1). An absent snapshot row is not a store error (§2.3).

r0's Q2 deviation (fresh-scan semantics) is withdrawn: the cache is modelled.

---

## 4. Persistence, crash consistency and recovery

### 4.1 What LQ1 persists

Nothing itself. Its `SnapshotRows::store` calls become `Effect::StoreQuorumSnapshot` inside connect attempts and synchronous overwrites outside them (§3.7.2); CH7b persists both. Caches and memos are memory-only.

### 4.2 The contract LQ1 relies on (CH7b)

1. **Store.** CF `llmq_snapshot`, key `(type u8, cycle-base hash)` (DEC-11), value the snapshot's Core serialization as `CoreBytes`.
2. **Durability.** Applied immediately after the attempt, in emission order, regardless of the result, as a single-row `Commit::Buffered` batch in the same engine and WAL as the evo state (A1 §3.5.7). As in Core (raw write in the same LevelDB log as the later EvoDB commit), a row is durable no later than the next durable write; after a crash every cycle-base block at or below the recovered evo tip has its row unless 28e applies, and replay re-emits rows above the tip.
3. **Overwritten, never erased;** wiped with the evo state on `-reindex` and `-reindex-chainstate` (v23 node/chainstate.cpp:73 / v24 init.cpp:2041).
4. **Read path.** Within an attempt, reads see the attempt's own writes (the overlay); the next attempt's read view is taken after the previous attempt's effects are applied; runtime writes are synchronous overwrites (§3.7.2).

### 4.3 Import

`dash-coreimport` (CH6) copies Core's `llmq_S` rows, recomputing Core's key `SHA256d(type ‖ hash)` for each `(type, cycle base)` it knows. A row whose bitset is shorter than the masternodes its list scores is refused. A1 §2.10's fast-import report already requires `quorum rotationinfo` for every cycle base still needed.

---

## 5. Performance

### 5.1 Metrics affected

A1 §3.9: #1/#3/#12 IBD, reindex, CPU per block (members for every mined commitment and every cycle); #5 connect latency at cycle-base and QcTx blocks; #8 GBT at cycle-base heights (`TestBlockValidity` recomputes the rotation for every template hash); #15–17 recsig, IS, CL latency (selection); #9 startup (cold caches).

### 5.2 Cost model (mainnet n ≈ 3,500; measured by PF1, not claimed)

- **Non-rotated quorum:** `n` single-SHA-256 calls of 64 bytes (two compressions each), a full sort, a list reconstruction, one full block read for the CL (post-V20). ≈ 80k such computations from DIP3 to the tip.
- **Rotation cycle (60_75, every 288 blocks):** six full `CalculateQuorum`s (three reconstructions, the snapshot build, not-used and used), four list reconstructions, four block reads, ≈ 7k `AddMN` into scratch lists with ≈ 6 unique-property SHA256d each. Core repeats it for every distinct template hash and for the mined block (`mapIndexedQuorumMembers` is keyed by the cycle hash).

### 5.3 Levers (route to 2×), each exact with a twin

Each lever has a line-by-line reference twin under `cfg(any(test, feature = "xcheck"))`, compared on every call in xcheck builds (A1 §3.10 rule 1), and lands as its own commit with two reviewers (PLAN §0; LQ1b-G2). State lives in the runtime (B-m7); the pure layer sees it only through `SortedListMemo` or not at all.

| # | Lever | Home | Why exact | Expected gain (hypothesis until PF1) |
|---|---|---|---|---|
| L1 | **Sorted-list memo** (B-n2): `sorted(list(work), m)` (no Evo filter, no truncation) keyed by `(work block hash, m)` | `dashd-chainstate`, via `SortedListMemo` | the list behind a block hash is immutable; the sort is a pure function of (list, modifier) | the snapshot build of cycle C sorts `list(work_C)` with `m_C`; the next three cycles reconstruct exactly that sorted list: 3 of the 6 sorts per cycle become lookups |
| L2 | **Multi-buffer single SHA-256 of 64-byte messages** | `dash-sha256` (new kernel) | same function | needs a new SIMD kernel: `dash-sha256` is multi-way only for `sha256d64` (double SHA-256). Task "SH1b" (§9): scalar twin, SHA-NI and aarch64 tests via SH1's QEMU recipe; gain measured, not taken from DEC-28(a)'s D64 numbers (B-m6) |
| L3 | **Partial selection** for non-rotated: `select_nth_unstable` on `ScoreKey`, then sort the top `size` | pure (`score.rs`) | the order is total over distinct entries; the input is one list | `O(n + k log k)` instead of `O(n log n)` |
| L4 | **Rotation memo** (B-M2): key and proof below | `dashd-chainstate` | proof below | the template and the mined block at a cycle base share one computation; repeated templates cost lookups |
| L5 | **Coinbase-facts side table** (B-m5) | `dashd-chainstate` | stores the raw facts, decodes on read; misses fall back to the block | no full block read per modifier |
| L6 | **Single-pass indexed scan** via `newest_by_index` | LQ2 implements the trait method | round-based truncation equals one pass per index (v24 blockprocessor.cpp:796-815) | `O(N · cycles)` instead of `O(N · cycles²)` iterator steps |

**L4 key, reached inputs and exactness proof.** The key is built from exactly the inputs Core reaches, and it stops where Core stops (GPT m1). The **set** of reached inputs and the stop marker are Core's. The **order** of the reads is not an exact port of either pin, and it does not need to be (rev3, GPT r3 finding 3, ruling 3).
- **Reached inputs.**
  1. *Current cycle, always reached at both pins:* `work_0 = ancestor(cycle_base, cycle_base.h − 8)`, `list(work_0)`, `m_0 = hash_modifier(p, cycle_base)`.
  2. *For `k = 1, 2, 3`:* `c_k = ancestor(work_0, cycle_base.h − k·C)`; none → marker "no ancestor". Then `row_k = rows.get(t, c_k)`; `Ok(None)` → marker "absent". Only with the row present are `list(work_k)` and `m_k = hash_modifier(p, c_k)` reached.
  3. *Nothing past the marker.* An error in an unreached read therefore cannot fail a computation Core completes. In the first rotated cycle `c_1` has no row (or, below genesis, no ancestor), so no `m_1` is ever requested.
- **Core's orders** (traced on each tag's extracted functions, GPT r3 `read-trace-results.txt`), with the H−C row present and the H−2C row absent:
  - v23: `list_0 → row_1 → list_1 → m_1 → row_2 (absent) → m_0`. `m_0` is read inside `BuildNewQuorumQuarterMembers` (utils.cpp:354), before its `enabled < size/4` return (:356-358).
  - v24: `m_0 → list_0 → row_1 → list_1 → m_1 → row_2 (absent)`. `m_0` is read in `GetAllQuorumMembers` (:701-704), the list at :484.
  - Each prior cycle reads its row, then its list, then its modifier (v23 :476-487 with the helper's :199-200 / v24 :490-505 with :203-204).
  - Both pins read `m_0` a second time in `BuildQuorumSnapshot` (v23 :315, reached from :451; v24 :319, from :460), just before the store. The port reads it once. The two reads can differ only under a hardware fault (D2) or a prune landing inside one computation, which rdashd's serialization excludes (one of Core's schedules, §3.7.2).
- **Rust's order** is `work_0, list_0, m_0`, then per `k`: `c_k, row_k, list_k, m_k`. It matches neither pin, and no pin's order is ported.
- **Why the order is unobservable.** Under the fail-stop ruling (§2.6, D4) a read error aborts the node. Two read orders can then differ observably only through a side effect of a read that was reached before the abort and outlives it. Checked read by read:
  - *Ancestry* reads the block index; it writes nothing.
  - *`list(work_k)`:* EV2's `resolve_list` only offers lists to the in-memory `ListCache` (`EV2-dml.md` §2.10; O17 EVO-25 `none`). Core's `GetListForBlockInternal` likewise fills only in-memory caches (deterministicmns.cpp v23:778-870 / v24:826-939, no DB write; §6.3.6).
  - *`m_k`:* `coinbase_facts` reads the L5 table or the block. An L5 miss falls back to the block and **does not insert** (L5 rows come only from connected blocks; L5 rules below). Core's `ReadBlockFromDisk` caches nothing.
  - *`rows.get`:* CH7b's row cache and Core's 32-entry `quorumSnapshotCache` (filled on a DB read: snapshot.cpp v23:272-277 / v24:309-313) are in-memory and write-through, so they never disagree with the store.
  - *L1:* a `SortedListMemo::put` is an in-memory, value-exact memo.
  - *Writes:* within one rotated computation, the row store (v23 :453 / v24 :462) and the members-cache inserts (§3.7.4 steps 4–5) follow every read at both pins, so a read error prevents them whatever the order.

  In-memory state dies with the aborted process, and the restarted node starts cold as after any restart. **No read in the sequence has a side effect that survives the abort, so no pin's order is ported for any read.** A read that gains a persistent side effect (for example a persistent L5 fill-on-miss) must follow that pin's order, and this argument must be redone.
- **Two unreadable inputs.** If two reached inputs are both unreadable, the error may name a different input than a given pin would. The outcome is the same: a node error, no verdict, no row from this computation, no cache insert.
- **Tests** (§6.1, §7 G2) assert the **set** of reached inputs and the stop marker, never the sequence. The cases:
  - the first rotated cycle (stop before any `m_1`);
  - one reached row, then an absent row;
  - a missing ancestor;
  - an injected error in each reached input (→ `Store`, no row, no members-cache insert);
  - an injected error in each unreached input (no failure).

The memo maps `K = (type, hash(work_0), m_0, [(hash(c_k), m_k, digest(row_k)) for each reached k], stop marker)` to `(by_index members, snapshot row bytes or None)`, where `digest` is SHA-256 of the row's Core serialization.
- *Inputs of `compute_by_quarter_rotation` (§3.6):* (a) the parameters of `type` (constant per process); (b) `list(work_0)` and `list(work_k)` for each `k` that is reached; (c) `m_0` and `m_k`; (d) the rows read for `c_1 … c_k`, or the absence that stops the loop; (e) `skip_removed`, a function of `cycle_base.h` and the network; (f) nothing else: `BlsCtx`, `rules` and block-data availability enter only through the modifiers, and the cycle-base hash enters only as the effect key (v24 utils.cpp:468-470: "everything is derived from the cycle's work block").
- *The key determines every input:* `hash(work_0)` fixes the ancestry below it, hence `cycle_base.h = work_0.h + 8` (e), every `c_k` and `work_k`, and every list (lists are immutable per block hash, EV2-dml.md §2.10) (b). The modifiers (c) and row digests (d) are in the key. Which `k` are reached is fixed by the presence markers. So two lookups with equal keys compute from equal inputs, and the output is a pure function of its inputs (§3.6 uses no other state): the memo returns exactly what a fresh computation would.
- *Rules:* build the key from the reached inputs above, in Rust's order (cheap with L1 and L5); never store an `Err` (a `StoreError` is transient); on a hit, `rows.store` the memo's row bytes under the **current** cycle-base hash (an overwrite, exactly as the fresh computation would) and return the members. The memo sits below the Core-faithful members cache: it is consulted only on a members-cache miss, so it changes the cost of a write point, never whether or what is written. Under xcheck every hit is recomputed and compared.

**L5 rules** (B-m5). Rows only for work-block heights, `h ≡ 16 (mod 24)` (every configured interval is a multiple of 24, so every cycle base is `≡ 0` and every work block `≡ 16`). Store exactly `CoinbaseFacts::Coinbase { is_coinbase, n_type, extra_payload }` taken from connected blocks; decode on read with the caller's `rules` and `bls`. A table miss (imported datadirs, blocks never connected, rows older than the table) falls back to reading the block, never to `BlockDataUnavailable`, and **does not insert** (rev3: rows come only from connected blocks, so no read has a persistent side effect; §5.3 L4 relies on it). Availability is read from the block index's `HAVE_DATA` status at query time, so pruning (ST2/CH8's prune path clears `HAVE_DATA`) is reflected without a separate hook.

`UsedSet` (§2.2.4) makes `try_add` cheaper than Core's SHA256d-per-property `AddMN` (EV2's pre-image keys). Expectations, hypotheses until PF1: a rotation cycle ≥ 2× once L1, L2 and L4 land; non-rotated ≈ 1.5–3× from L2 and L3; GBT at a cycle base ≥ 2× from L4.

### 5.4 The never-slower check (B-M3 ruling)

- **The Core bench.** A DEC-16 local Dash Core branch `node/lq1-bench` (worktree `~/workspace/dash-wt/lq1-bench`), cut from the O2a-LQ1 follow-up's shim branches for both pins. **Owner: the LQ1 implementer, in LQ1a-A.** It is a perf-stat driver over the shim's `oracle_llmq_ctx_*` fixture loader (§6.2): it loads a fixture set and calls `GetAllQuorumMembers(…, reset_cache = true)` (members), `ScanQuorums` with a fresh `CQuorumManager` (scan) and `SelectQuorumForSigning` (select) in a loop, counting instructions and allocations with the same method as PF0. The shim's compile flags are checked against the release build's `-O2` (and recorded); built with B1's script.
- **Fixtures available now.** Lists from `protx listdiff` replay on the testnet replica (§6.3.6) or synthetic lists, written as EV2 canonical bytes, plus coinbase blocks, snapshot rows and a mined index. O8's dump replaces the replay later (it is not built yet).
- **Like with like.** Core reads a block per post-V20 modifier. Comparisons are reported per era: pre-V20 fixtures (no block reads on either side) and post-V20 fixtures, where Core's block reads (from a tmpfs blocksdir) are compared with Rust's L5 path and with Rust's block-read path, both labelled.
- **Where it runs.** Per PR, perfgate compares head with the previous head (`crit:llmq_*`). The Core comparison is a perfgate workload kind `core = { driver, fixture }` if PF0 accepts it (proposal to the PF0 owner), else a manual recorded comparison per phase, as CS0 N4 did.
- **When.** LQ1a-A: driver and fixtures, the first Core numbers for scan/select skeletons; **LQ1a-B: the first members PR is compared with Core** (`crit:llmq_members_nonrot` vs the driver); LQ1b-E: rotation (`crit:llmq_rotation_cycle`); LQ1b-G2: each lever before/after, and vs Core.
- **Dependencies:** PF0 (in review) for the per-PR gate; the O2a-LQ1 follow-up (§6.2, Q2) for the driver. LQ1a-A merges only when both are available; if the O2a-LQ1 follow-up is late, LQ1a-A is late (§8 Q2).

| Workload | Inputs | Counters |
|---|---|---|
| `crit:llmq_members_nonrot` | 200 bases (100_67, 400_60, 400_85, both eras) | instructions, allocations |
| `crit:llmq_rotation_cycle` | 12 consecutive 60_75 cycles (testnet now; mainnet later), rows preloaded | instructions, allocations |
| `crit:llmq_select` | 100k selections, both kinds, synthetic index | instructions |
| node level | `reindex-chainstate-mainnet`, `gbt-regtest` at cycle bases, B1 `bench_log` connect latency | as B1/PF0 |

---

## 6. Verification

### 6.1 L0 (Core vectors and constructed cases)

- **Transcribe** v24 `llmq_snapshot_tests.cpp` (13 cases) and `llmq_params_tests.cpp` (8 cases); every v23 case is contained in v24.
- Core has no unit test of `GetAllQuorumMembers`, rotation, the modifier or selection; L1 and L2 carry the proof.
- **Constructed vectors** (generated by §6.2's entry points, committed with provenance): every row of the quirk-29 table; Q-LQ1-2; 28a–28i each isolated (incl. 28d's reconstruction mismatch); default regtest's pre-DIP3 rotation-enabled cycles and base 432 (A-n1); quirk 30 bit patterns; **30b rejecting a present index on a short scan** and resolving it after padding (A-m1); the scan cache's duplicate padding for TEST_DIP0024 (count = keep = 4) and 60_75 (§3.8.3).
- **rev3 vectors:**
  - the reached-input sets and stop markers of §5.3, asserted as sets: first cycle, one row then absence, missing ancestor, an injected error in each reached and each unreached input (finding 3);
  - the TEST_DIP0024 materialization trace: a count-1 miss over four commitments materializes 48, 49, 24, 25 and returns `[48]`, and the repeat makes no `get_quorum` call (finding 2);
  - the precompute routing trace of GPT r3 `members-warm-and-undo-*`: after a warm cycle 240 and a changed input, `Connect` keeps row 1 and `Undo` writes row 2, after which `members` returns the new value (finding 1);
  - the digest known-answer vectors of §6.2 (finding 4).
- **rev2 vectors:** the L4 reached inputs in the first rotated cycle (stop before any `m_1`) and with an injected error in an unreached older read (must not fail) (GPT m1); an absent row (`Ok(None)` → break) against an unreadable row or missing required list (`MemberError::Store`, no verdict) (GPT m2); a **provenance fixture**: a rotated quorum whose members' operator keys and services change between an older work list and the current list, compared per position with the oracle's member state (GPT m3).

### 6.2 L1: oracle entry points (O2a-LQ1 follow-up; Q2)

```c
void* oracle_llmq_ctx_new(const char* chain, const char* const* args, size_t n_args);   /* regtest/devnet heights via args */
int   oracle_llmq_add_header(void*, const uint8_t hdr[80], int32_t height);
int   oracle_llmq_set_mn_list(void*, const uint8_t block[32], const uint8_t* entries, size_t len);
int   oracle_llmq_set_coinbase(void*, const uint8_t block[32], const uint8_t* coinbase_tx, size_t len, int have_data);
int   oracle_llmq_set_snapshot(void*, uint8_t type, const uint8_t cycle_base[32], const uint8_t* snap, size_t len);
int   oracle_llmq_set_mined(void*, uint8_t type, const uint8_t* commitment, size_t len, int32_t mined_height);
int   oracle_llmq_type_enabled(void*, uint8_t type, const uint8_t prev[32], int opt_dip24_active /* -1 none */,
                               int opt_have_dip24_quorums /* -1 none */, int* enabled);              /* B-m9 */
/* Every call below that can compute members, build quorums or write rows returns that call's event trace
   (format below) in trace_out/trace_len: per-call capture, buffer cleared at entry. */
int   oracle_llmq_members(void*, uint8_t type, const uint8_t base[32], int legacy_bls, int reset_cache,
                          uint8_t* members_out, size_t* members_len,      /* per member: proTxHash, operator key as stored, net-info bytes */
                          uint8_t* trace_out, size_t* trace_len);         /* Core GetAllQuorumMembers(type, base, reset_cache) */
int   oracle_llmq_precompute(void*, const uint8_t block[32], int legacy_bls, int reset_cache,
                             uint8_t* trace_out, size_t* trace_len);      /* Core's own PreComputeQuorumMembers (rev3) */
int   oracle_llmq_scan(void*, uint8_t type, const uint8_t start[32], uint32_t count, int use_chain_overload,
                       uint8_t* bases_out, size_t* n, uint8_t* trace_out, size_t* trace_len);
int   oracle_llmq_select(void*, uint8_t type, const uint8_t sel[32], int32_t sign_height, int32_t offset,
                         uint8_t out[32], int* found, uint8_t* trace_out, size_t* trace_len);
int   oracle_llmq_restart(void*);         /* a Core restart over the same DB: fresh managers (scan, object and snapshot
                                             caches), both members statics cleared; rows and the mined index kept */
int   oracle_llmq_rows_digest(void*, uint8_t out[32]);   /* the canonical digest below; reads only, emits no event */
void  oracle_llmq_ctx_free(void*);
```

`oracle_llmq_scan_reset_cache` (rev2) is replaced by `oracle_llmq_restart`. A fresh `CQuorumManager` alone, with warm members statics, is not a state Core reaches.

**Shared managers** (rev3 ruling 2). One context owns one `CDeterministicMNManager`, one `CQuorumSnapshotManager`, one `CQuorumBlockProcessor` and one `CQuorumManager` (no DKG session manager, as on a node that is neither a masternode nor a `-watchquorums` observer; so traces contain no DKG second access, §3.8.3. A DKG-configured oracle context, with a DKG session manager and seeded contribution state, is QM1a's addition if its suite compares Core traces). Every entry point above uses those objects and the process-global members statics: members, precompute, scan (`ScanQuorums` → `GetQuorum` → `BuildQuorumFromCommitment` → `GetAllQuorumMembers` → `StoreSnapshotForBlock`) and select. A scan therefore warms the same members cache and writes the same rows as a direct members call, as in a running node. `use_chain_overload` selects v24's `ScanQuorums(…, chain)` with the context's chain; v23.1.8 has no such overload, and the call fails there.

**Event trace** (DEC-16 record-only hooks on the O2a-LQ1 shim branches). The hooks sit in `GetAllQuorumMembers` (each return), both `GetQuorum` overloads (each return), `StoreSnapshotForBlock`, and `ScanQuorums` (each return). Each hook appends to a buffer and changes no control flow; the O2a-LQ1 review checks that. Each event is emitted when the operation it describes completes, so nested events precede the enclosing one (`ROW` → `MEMBERS` → `GET_QUORUM` → `SCAN`). The Rust facades emit the same events under the `reference` feature, and traces are compared byte for byte.
```
trace = CompactSize(n_events) || event*                         ; hashes are 32 bytes, internal byte order
ROW        0x01 || Core llmq_S key (39 B, below) || CompactSize(len) || snapshot bytes   ; every store, overwrites included
MEMBERS    0x02 || u8 type || base || u8 reset || u8 outcome || CompactSize(n_members)
                   outcome: 0 gate-empty, 1 base-hit, 2 index-out-of-range, 3 index-hit, 4 computed
GET_QUORUM 0x03 || u8 type || base || u8 populate_cache || u8 chain_overload || u8 outcome
                   outcome: 0 not-mined, 1 object-cache-hit, 2 built, 3 no-mined-commitment
SCAN       0x04 || u8 type || store || u8 cache (0 bypass, 1 miss, 2 hit-satisfied, 3 hit-continuation,
                   4 continuation-bail) || u8 scan_cache_written || CompactSize(n_returned)
```
Gate-level early returns of `ScanQuorums` (count 0, type not enabled, below genesis) emit no `SCAN` event, since they touch no cache. The rev2 output `written_snapshots` is the trace's `ROW` events.

**Canonical row digest** (rev3 ruling 4; `oracle_llmq_rows_digest` and the Rust twin over CH7b's store).
- **Namespace.** Every row in the `llmq_S` namespace and nothing else: seeded rows, rows overwritten since, rows written by failed attempts or `fJustCheck` templates (template hashes included), and rows written by runtime calls. No other EvoDB namespace is included.
- **Key.** Core's serialized DB key of `std::make_pair(DB_QUORUM_SNAPSHOT, snapshotHash)`: `CompactSize(6) ‖ "llmq_S" ‖ SHA256d(u8 type ‖ cycle-base hash)`, 39 bytes (snapshot.cpp v23:18, :283-291 / v24:24, :320-328; the `string_view` key serializes like a string). Rust maps its CF key `(type, hash)` (§4.2, DEC-11) to this form.
- **Row.** The snapshot's Core serialization (`CQuorumSnapshot::Serialize`, snapshot.h v23:41-89 / v24:42-90): `i32` LE mode ‖ `CompactSize(n)` ‖ fixed bitset `⌈n/8⌉` bytes, bit `p` in byte `p/8` ‖ `CompactSize(m)` ‖ `m × i32` LE. These are not the raw database value bytes.
- **Preimage.** `CompactSize(row_count) ‖ Σ over rows, sorted by key as unsigned bytes (memcmp order): CompactSize(len(key)) ‖ key ‖ CompactSize(len(row)) ‖ row`.
- **Digest.** `SHA-256(preimage)`, a single SHA-256. An empty namespace hashes the single byte `00`: `6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d`.
- **Read boundary.** The digest is taken between calls, at a point where no operation is in progress. On the Core side, the shim is single-threaded and raw writes are immediate. On the Rust side, no attempt is open, every attempt's effect log has been applied, and the rows are read from one consistent store snapshot.
- **No side effects.** Computing the digest reads the raw namespace with a DB iterator (Core) or a CF iterator (Rust). It never goes through `GetSnapshotForBlock` or `SnapshotRows::get`, so it fills no LRU, computes no members, creates no row and emits no event.
- **Known-answer vectors** (`/work/scratch/lq1/digest_kat.py`; keys and rows reproduced by Core v24's own serializers, `corekey_main.cpp` → `corekey.out`):

| Case | Rows (type, cycle-base hash in internal order, row bytes) | Digest |
|---|---|---|
| empty | — | `6e340b9c…17afa01d` (above) |
| A | `5`, `11`×32, `00000000094d0100` (mode 0, 9 bits `101100101`, no skips); key `066c6c6d715f5316b6e4…16a22c200` | `3fae486d7339fc149727a978309e4e6d5ba435dfb07d68380bde6b24385331d7` |
| A+B | A, and `103`, `000102…1f`, `01000000040b0202000000ffffffff` (mode 1, 4 bits `1101`, skips `[2, −1]`); B's key `066c6c6d715f533905e9…adbc140f6` | `8d633cea2b2d3a36fa0961257ee7e9bcd1e1d161d9db6018e425c8ec836ad66b` |
| A+B′ | A+B after B is overwritten with `03000000040000` (mode 3, 4 bits `0000`) | `ef532660182377caea3e420b965b09967087c1091f7464ae4841a25dd30608a9` |

The full keys and preimages are in `digest_kat.out`. Each implementation's unit tests pin these four values. The digest is for **same-history** comparisons (L1 contexts, `diff_chain`); L2 replicas with a different history compare per-cycle rows instead (§6.3.2 item 3).

**Shim constraints** (A-n2):
- `GetAllQuorumMembers`' members caches are function-level statics, so they are process-global. The shim passes the caller's `reset_cache`, so warm-cache cases keep the statics across calls. One context runs per process. `oracle_llmq_restart` clears the statics through a DEC-16 test hook.
- `oracle_llmq_set_coinbase` may be called again between calls, to change a work block's payload or data availability (the GPT M1 scenarios).
- `IsQuorumRotationEnabled` and `GetNonNullCoinbaseChainlock` read the global `Params()`, so there is one chain per process (a fuzz worker per network).
- `GetSnapshotForBlock` reads the 32-entry LRU before the DB, so rows are seeded through `StoreSnapshotForBlock` (which `oracle_llmq_set_snapshot` calls) or before a restart. Seeding is not part of any call's trace, but the seeded rows are part of the digest.
- Core's `PreComputeQuorumMembers` is a file-static. The O2a-LQ1 branch exposes it under its shim define with the body unchanged.
- The v23.1.8 shim reaches `ScanQuorums`/`SelectQuorumForSigning` through a `CChain` it builds over the headers.
- Compile flags are recorded (§5.4).

**Targets** (`node-fuzz`, through the CS0/O3 runner):

| Target | Generator | Compares | Budget |
|---|---|---|---|
| `diff_quorum_members` (non-rotated; LQ1a) | lists 0–2,000 entries; bans, unconfirmed, Evo mix, injected equal `cwp`; V19/V20/DIP0020 around the base; coinbase payloads valid, invalid, malleable, truncated, trailing, v4-shaped, `have_data` 0/1; both `legacy_bls`; both profiles | ordered proTxHashes | smoke + ≥ 24 CPU-h at LQ1a merge |
| `diff_quorum_members_rot` (LQ1b) | 4–12 cycles on `TEST_DIP0024`, `DEVNET_DIP0024`, `60_75` params; evolving lists with key/address reuse across eras (28b); DIP24/V19/V20 alignment; rows withheld (28a); small lists (28e) | per-index members **and** written rows' bytes | ≥ 24 CPU-h at merge; ≥ 72 CPU-h before M4 |
| `diff_scan_quorums`, `diff_select_quorum` (LQ1a) | mined indexes with failed indices, gaps, early cycles; `start` around the window; **call sequences** with repeated counts (exercising cache hits, continuation and padding, compared against the cached Core manager after the same sequence) | bases, chosen quorum, **and the call's event trace** (rev3) | ≥ 24 CPU-h |
| `diff_llmq_sequences` (rev3; LQ1a-C non-rotated, LQ1b-E rotated) | interleavings of `members` (reset 0/1), `precompute` (`Connect`/`Undo`), scans with both overloads, selections, `set_coinbase` availability changes, restarts and injected missing commitments, on one context | per call: the event trace (`ROW`, `MEMBERS`, `GET_QUORUM`, `SCAN`) and return values; after each call: the canonical row digest | ≥ 24 CPU-h at LQ1b-E merge; re-run by CH7b and QM1a against their production implementations |
| `diff_type_enabled` (LQ1a) | every type × prev heights around each activation × overrides | bool | smoke |

### 6.3 L2: history

#### 6.3.1 Inputs

LQ1 needs full lists at every work block: EV2's canonical dump (O8, `getcanonicalmnlist`) once it exists; until then `protx listdiff` replay with EV2a's `apply_diff`. Until O8 exists the replayed list is checked per block against the **stock** CbTx `merkleRootMNList` (B-m13), which covers every field LQ1 reads except the `try_add` inputs (collateral, owner key, platform node id); those are covered by L1 and by item 1's order gate. The mined index and commitments come from LQ2's fixtures or from block scans (`getblock` of QcTx-carrying blocks).

#### 6.3.2 The gates (B-M4 ruling)

1. **Member order: `quorum info` over all quorums, rotated included.** Our members equal Core's `quorum info <type> <hash>` `members[].proTxHash` in order, for **every** mined quorum the replica holds. This is the only order-exact L2 check and the member-order **merge gate** for LQ1a-B (non-rotated) and LQ1b-E/F (rotated): testnet over everything the replica holds (DIP24 at 769,700 through the replica's tip, so the first V20 cycles too); regtest FT chains (O4 and the FT-chain captures, §6.4); mainnet as the replica's sync allows (the mainnet oracle is at 1.02M, before DIP3), at the M4 milestone at the latest. **Entry state per position (GPT m3):** the returned entry's operator key and addresses also equal `quorum info`'s `pubKeyOperator` and `addresses`, which Core reads from the member entry itself (rpc/quorums.cpp v23:209-214 / v24:217-224), so the gate checks provenance as well as order. **Coverage is reported per network and era**; regtest runs never count as testnet or mainnet coverage (GPT r2).
2. **Signer-set certificate.** For every non-null historical commitment, `membersSig` verifies over the operator keys at signer positions of our members (BS11b/dash-pkc, the scheme Core used: legacy iff mined below V19). Core's secure aggregation sorts the keys first (`dashbls` `CoreMPL::VerifySecure`, schemes.cpp:240-269), so this certifies **only the multiset of signer keys**: not order, not non-signers. It is a cheap cross-check that needs no replica, never a substitute for item 1.
3. **Rotation rows per cycle.** Rows exist from the first rotation-enabled cycle base (testnet 769,824; mainnet 1,737,792). `quorum rotationinfo` at base `H` returns the rows at `H−C`, `H−2C`, `H−3C` and fails while any precedes the first row, so rows are read through requests from `first + 3C` on, each row seen up to three times and required to agree. Our bytes equal Core's; neither side has a row the other lacks; failing requests fail on both sides.
4. **Era coverage:** DIP24 activation and the first four cycles; DIP0024QuorumsHeight; V19 (`skipRemoved` flips; 28d's reconstruction effect); the first V20 cycle (mixed non-rotated row; first rotated CL modifiers); MN_RR; today.
5. **Selection.** `quorum selectquorum <type> <id>` for 10k random ids per type at a fixed replica tip, against `select_from_scan` over the facade with the replica's call history replayed where known (§3.8.3); each batch is bracketed by `getbestblockhash` and discarded if the tip moved (B-m10). Historical coverage of 30c (a recently failed index) comes only from L1 fuzzing and constructed vectors (B-m10).

#### 6.3.3 Non-vacuity (B-m10)

Every digest reports "compared N of expected M", with M from block scans of mined commitments (or `quorum listextended` on the replica), and fails on N < M except for documented exclusions.

#### 6.3.4 Profiles

Mainnet and testnet history are profile-independent for LQ1 (§3.3.1); the v23.1.8 reference (O7) supplies sampled `quorum listextended` equality at mainnet milestones (A1 §2.25 point 2).

#### 6.3.5 The CbTx-CL invariant (A-M2)

Over every mainnet and testnet block with a CbTx of version ≥ 3: `nHeight == h` and `bestCLHeightDiff < h`. Run once over the BS2 corpus (block files and `getblock` reads, pure), testnet now and mainnet once synced past V20. It proves from data that the two profiles' predicates agree on both histories.

#### 6.3.6 Where digests run (B-M5 ruling)

- **Writing or heavy calls run on a replica, never on the live oracles:** `quorum info`, `quorum list`, `quorum listextended`, `quorum selectquorum`, `quorum memberof` (all reach `ScanQuorums`/`GetQuorum`, which can rewrite `llmq_S` rows for rotated types), `quorum rotationinfo` and P2P `getqrinfo` on any node whose **tip** is post-V20 (the tip diff runs `BuildQuorumChainlockInfo` → `GetQuorum`, smldiff.cpp v23:199-200 / v24:221-226, and `BuildQuorumRotationInfo` holds `cs_main`), and `protx diff` / P2P `getmnlistdiff` with a post-V20 target (both call `BuildSimplifiedMNListDiff`: rpc/evo.cpp v23:1627 / v24:1887).
- **The replica** is built the O7 way (A1 §4.6 l.2361: sealed `blk` files hardlinked, tails copied, rebuilt with `-reindex-chainstate`; the oracle's files hashed unchanged before and after), with its own datadir, `-port`/`-rpcport`, `-listen=0`, `-connect=0`, no ZMQ. Never a `cp --reflink` of a running oracle (torn LevelDB state; A1 §2.9). Testnet first; mainnet once the oracle has the heights.
- **Only provably pure reads touch the live oracles**, throttled (concurrency 1, idle priority, outside B1 windows, through `oracle-cli`): `getblockhash`, `getblockheader`, `getblock` (verbosity ≤ 3), `getbestblockhash`, `getblockchaininfo`, and `protx listdiff` (`GetListForBlock` + `BuildDiff`, rpc/evo.cpp v23:1703-1706 / v24:1939-1942: list reconstruction touches only in-memory caches; `GetListForBlockInternal`, deterministicmns.cpp v23:778-870 / v24:826-939, has no DB write).
- **The r0 probe, reclassified.** r0's five `quorum rotationinfo` calls on the testnet oracle (cycle bases 769,824 … 770,976) were described as pure reads. They were not provably pure: the oracle's tip (1,148,082) is post-V20, so each successful request's tip diff could build rotated quorums and rewrite identical `llmq_S` rows, under `cs_main`. Any row written was byte-identical to the existing one (the node does the same after every restart); the calls were not repeated and nothing else ran. Under this rule they would have run on the replica.

### 6.4 Cross-check with dash-spv (B-M6 ruling: inverted)

- **LQ1 writes fixtures** (LQ1b-F, `test-vectors/lq1-spv/<capture>.json`): for each capture, per quorum `(type, quorum hash, index)` the expected members in order, and the **trace triggers** LQ1 observed on that quorum's path: mode 2 in a snapshot used, a pre-V19 cycle, any `try_add` rejection, `enabled < size/4`, and the SML adapter's reduced `try_add` (§2.2.4). The expected members are grounded in Core: `quorum info` from the chain the capture came from (the replica, or the FT node for regtest captures), plus the `lastCommitmentPerIndex` signer-set check.
- **dash-spv checks them in its own crate** (B-M6): a `#[cfg(test)]` module in `dash-spv/src/sml_engine/` uses `test_support` (sml_engine.rs:793) to feed the capture through `feed_qr_info` and compares members with the fixture. dash-spv depends only on the data files (A1 rule 8 holds). Owner: the LQ1b-F implementer (a small dash-spv test-only PR; nothing under `dash-spv/src/network/`).
- **Classification is mechanical** (B-M4 ruling): a disagreement may be recorded as a known dash-spv gap only when the fixture's trace shows that gap's trigger fired on that quorum (mode 2 present; a pre-V19 cycle; a `try_add` rejection on the path; `enabled < size/4`). "No collateral tie-break" can never apply to a real capture (equal scores do not occur), so a disagreement under that label fails. Otherwise the disagreement fails, and the arbiter is `quorum info`, not `membersSig`.
- **Non-vacuity:** each capture reports "dash-spv resolved N of M quorums"; the test fails below a set fraction.
- **Captures** (B-m11): the four O4 `qrinfo` (`core_wire/qrinfo_2240504.bin`, `qrinfo_2518986.bin`, `QR_INFO_0_2224359.hex`, `qrinfo_core231_paloma.hex`; all after V20) and the 32 `mnlistdiff`; **`dash/tests/data/core_v24_regtest_qrinfo.json`** (Core v24 regtest captures from `feature_llmq_rotation`, small lists exercising 28e); more regtest captures generated from the Core functional-test chains (no oracle needed); and one `qrinfo` per §6.3.2 era per network from the replica, which is **necessary**, since no existing capture covers DIP24 activation, pre-V19 or the first V20 cycle.

### 6.5 Model and non-differential tests

- **`rotation_skiplist`:** encoder → decoder round-trip over random fill traces (skips at 0, at wrap, after wrap, negative entries); arbitrary decoder input never panics and ends within `N·size/4 + skip_list.len()` steps (A-m5).
- **`rotation_model`:** members ⊆ work list ∪ previous quarters; quarter length `<= size/4`; `active.len() == total`; every twin (L1–L6, `UsedSet`) equals its optimised path.
- **Iteration order** (B-m3): every member computation and row build runs with three `for_each` orders (memcmp, EV2's `core_order()`, reversed) and must give identical members and row bytes.
- **`scan_cache_model`:** the QM1 cache against a direct port of `unordered_lru_cache` under random call sequences (get/emplace/truncate).

### 6.6 Stateful (`diff_chain`, O2b) and functional tests

`diff_chain` scenarios: a GBT template and a `proposal` at a cycle base (one row per template hash); cycle-base blocks failing after the quorum `ProcessBlock` (row kept) and before it (no row); disconnect/reconnect and `invalidateblock`/`reconsiderblock` across a cycle base; restart; VerifyDB 3 and 4; rotated selection call sequences on a young regtest chain (padding). After each step the verdicts and selection results equal Core's, and the **canonical `llmq_S` digest** (§6.2) equals Core's (rev2: digest comparison replaces in-process row assertions; rev3: one preimage).

**Overwrite scenarios (GPT M1, routed through the production facade per rev3 ruling 1).** A post-V20 rotated cycle is written while its work block's coinbase CL is available, and that block's data is then made unavailable. Then:
- (a) `QuorumPrecompute::precompute(block, Undo)`: the type's caches are reset, the row is overwritten, and the next `members` call returns the new members (GPT r3 `PRE 240 1`);
- (b) `precompute(block, Connect)` with a warm cache: no recomputation, no overwrite, the warm members stay (`PRE 240 0`);
- (c) a cold restart, then `members`: a recomputation and an overwrite;
- (d) the next cycle's reconstruction after each of (a)–(c) equals Core's.

The calls go to the **production** `QuorumPrecompute` and members facade (CH7b's), never to `precompute_uncached`. On the oracle side they go to `oracle_llmq_precompute`, `oracle_llmq_members` and `oracle_llmq_restart`, compared by event trace and canonical digest after each step. Until CH7b exists, LQ1b-E runs them against `reference::ModelMembersFacade` (§7). A test that passes the uncached route (`PURE 240`, which writes row 2 while the cache still returns revision 1) is a known-bad control: the suite must fail on it.

**Cross-cache scan sequences (rev3 ruling 2)**, on one context with real shared managers (§6.2). Each case compares the `GET_QUORUM`, `MEMBERS`, `ROW` and `SCAN` events and the canonical digest after each call:
1. **Cold scan:** a count-1 miss on TEST_DIP0024 and on 60_75 materializes the whole keep-sized range in base order, with `populate_cache` true exactly for the first `keep` bases, and writes rows for rotated builds that miss the members cache.
2. **Hit:** the same scan again: one `SCAN` event (hit-satisfied) and nothing else.
3. **Continuation:** a short cached list followed by a larger count. The prefix objects are reused; only the new bases emit `GET_QUORUM`, with `populate_cache` counting the prefix. On young rotated chains this includes the duplicate padding.
4. **Discarded tail:** after the cold scan, `members` for a base that was materialized but not returned is a `MEMBERS` base-hit with no `ROW`. In a pruned-work-block variant, the tail build's overwrite of the previous cycle's row is present on both sides.
5. **Later failure:** the k-th base's mined commitment is removed from the context. The scan returns empty with no scan-cache write, the builds of bases 1…k−1 keep their `MEMBERS`/`ROW` effects and object-cache inserts, and a repeat scan reuses them as object-cache hits.
6. **v24 chain overload:** the same materialization with no `SCAN` cache access or write, followed by a cached scan that misses the scan cache and hits the object cache.
7. **Rotated Platform override (rev4):** a devnet context with `-llmqplatform=llmq_devnet_dip0024`, running asset-unlock-shaped calls.
   - A cached scan of `N + 1 = 3`: commitments 96, 97, 48 and 49 all materialized, `[96, 97, 48]` returned, rotated `MEMBERS`/`ROW` events.
   - Its repeat: a hit, no `GET_QUORUM`.
   - On v24, the bound-chain bypass scan and the bound-chain explicit `GetQuorum` (no `SCAN` cache access or write), followed by the cached scan.
   - The same for a rotated `-llmqmnhf` explicit lookup.

**Conformance suites** (`conformance.rs`, rev3). `members_facade_suite<F: QuorumMemberSource + QuorumPrecompute>` runs the overwrite scenarios, the routing traces of §6.1 and a random-sequence model check (50k steps, as GPT r3's harness) against a supplied facade. `scan_suite<M: QuorumMaterializer, S: scan facade>` runs the seven cross-cache cases. LQ1b-E and LQ1a-C run them against the `reference` models. **CH7b's acceptance runs the members suite and the oracle fixtures against its production facade. It also runs the scan suite against its production scan facade, scan cache, materializer and quorum-object cache, for every configured type validation can select, including case 7 (the rotated Platform override with v24's bound-chain bypass and the cached scan after it) (rev4).** QM1a re-runs the scan suite on the same runtime caches it extends. Its suite for a node with a DKG session manager follows §3.8.3's trace-parity rule: a DKG-configured oracle if it compares Core traces, otherwise a non-DKG suite plus a separate DKG access-stream test. A production implementation that bypasses the facade cannot pass.

FT to port (with CH7b and QM1a): `feature_llmq_rotation.py`, `feature_new_quorum_type_activation.py`, `feature_llmq_evo.py`, `rpc_quorum.py`, `feature_llmq_signing.py`, `feature_llmq_simplepose.py`, `feature_llmq_is_*`, `feature_llmq_chainlocks.py`.

---

## 7. Implementation plan

Each phase is one reviewable PR (class LLMQ: two reviewers + `code-review` high). Worktree `~/workspace/rdc-wt/lq1-quorum-members`, branch `node/lq1-quorum-members`; Core bench on `node/lq1-bench` in `~/workspace/dash`.

| Phase | Content | Acceptance |
|---|---|---|
| **LQ1a-A** params, `log2`, Core bench | `llmq::params` (+ overrides), `eval_spork`, `signer_index_bits`, `LlmqCtx` domain check; `contrib/node/llmq-log2/`; the Core bench driver on `node/lq1-bench` over the O2a-LQ1 loader; first fixtures (synthetic + testnet replay) | unit tests per type × network × edge height; `diff_type_enabled` against `oracle_llmq_type_enabled` on four networks; `signer_index_bits` tests; the `log2` script passes; the driver produces recorded counts on both pins |
| **LQ1a-B** modifier, scores, non-rotated members | `modifier` (consuming EV4's predicate), `score` (+ twin, L3), non-rotated `members` (`members_gate`, `members_route`, `compute_quorum_members`), `reference::ModelMembersFacade` (Core's two caches and order; its rotated compute lands in LQ1b-E), `LlmqSource`/`MnListView` with a map-backed test source and the `MnList` adapter | quirk-29 vectors; `diff_quorum_members` smoke + ≥ 24 CPU-h; §6.3.2 item 1 for every non-rotated testnet quorum on the replica; **first Core comparison** (`crit:llmq_members_nonrot` vs the driver) |
| **LQ1a-C** scan + selection | `scan` (store block, `scan_quorums_from` with continuation and materialization, `scan_commitments_from`), `ScanEntry`, `QuorumMaterializer`, `selection`, an in-memory index with `newest_by_index`; `reference::ModelMaterializer` (Core's `GetQuorum` + object cache over the model members facade) and the `scan_suite` (§6.6) | constructed failed-index, early-cycle, padding and 30b vectors; the TEST_DIP0024 materialization trace; `diff_scan_quorums`, `diff_select_quorum` (with call sequences and event traces) ≥ 24 CPU-h; `scan_suite` cases 1–6 for non-rotated types against the oracle; `scan_cache_model`; §6.3.2 item 5 on the replica |
| **LQ1b-D** snapshot glue + reconstruction | `QuorumSnapshot` (BS8) glue, `skiplist`, `quarters_by_snapshot`, L1 hook | transcribed `llmq_snapshot_tests`; `rotation_skiplist` 24 CPU-h; reconstructed quarters equal the quarters embedded in the **later** quorums (`quorum info` on the replica), including pre-V19 cycles (28d) |
| **LQ1b-E** new quarters, snapshot build, effects | `new_quarters`, `build_quorum_snapshot`, `compute_by_quarter_rotation`, rotated `members_route`, `precompute_targets`, the `QuorumPrecompute` trait, emission, `UsedSet` (+ twin); `reference::{get_all_quorum_members_uncached, precompute_uncached, ModelMembersFacade}`; `members_facade_suite`; the rotated half of `scan_suite`; the runtime sink and members-facade contract for CH7b | 28a–28i vectors; `diff_quorum_members_rot` smoke + ≥ 24 CPU-h; `diff_llmq_sequences` ≥ 24 CPU-h; §6.3.2 items 1–3 on testnet (all cycles), including per-position entry state; the provenance fixture; the absent-row vs `Store` tests; the overwrite scenarios and cross-cache scan sequences (§6.6) against the oracle with the model facade and materializer, with traces and canonical digests; the known-bad `PURE 240` control fails the suite; the digest KATs; iteration-order test; Core comparison (`crit:llmq_rotation_cycle`). **LQ2 may start after this phase** (against the trait and the model facade). |
| **LQ1b-F** cross-checks | SML adapter, fixture writer, the dash-spv in-crate test PR, per-era captures from the replica, regtest/FT-chain captures; mainnet L2 as the replica allows; CbTx-CL invariant | §6.4 required equalities with non-vacuity; §6.3.5 invariant on testnet |
| **LQ1b-G1** budgets, predictor | `diff_quorum_members_rot` to ≥ 72 CPU-h; the v24 predictor (RPC surface) | 72 CPU-h clean; predictor equals `quorum dkginfo` `upcoming_dkgs` membership on regtest |
| **LQ1b-G2** levers | L1, L4, L5, L6 (one commit each, with twin and xcheck); L2 after SH1b | per-lever PF0 before/after and vs the Core driver; the L4 reached-input-set vectors (first cycle, one row then absence, missing ancestor, injected errors in reached and unreached inputs; sets and markers, not sequences); L5 never inserts on a read miss; `diff_chain` scenarios (§6.6) when O2b exists |

**Dependencies** (B-m4, ruling):
- **LQ1a:** EV2a (list types, `resolve_list`, `MemEvoStore`; PLAN M-9c), EV0 (the trait), BS5, CS0, CS4 (TESTDUMMY), BS6a + BS11a-t (CbTx decode inside the predicate), **i:EV4** for the coinbase-CL predicate (if EV4's `cbtx` module has not landed when LQ1a-B starts, LQ1a-B lands the predicate in `dash-evo::cbtx` with the EV4 owner as a reviewer), the O2a-LQ1 follow-up, PF0. No BS8 (the commitment is an associated type).
- **LQ1b:** LQ1a, **EV2a** (not EV2b: `try_add` is EV2a's; EV2b consumes LQ1's trait), **i:BS8**.
- **LQ2** merge-depends on LQ1b-E, not on LQ1b-F/G, keeping the levers and budgets off the CH7b critical path. LQ2 takes `&dyn QuorumPrecompute` and is tested against the model facade.
- **CH7b** (rev3, rev4): its acceptance adds the members-facade suite and the cold/warm/undo oracle fixtures against its production facade (§6.6). It also adds the scan suite against its production scan facade, scan cache, materializer and quorum-object cache, for every configured type validation can select, including the rotated Platform override (case 7) (Q7).
- **QM1a** (rev3, rev4): it extends CH7b's runtime caches and does not replace them. Its acceptance re-runs the scan suite on them and adds the DKG-path suite under §3.8.3's trace-parity rule.
- **O2a-LQ1** (rev3): the §6.2 entry points change. Added: `oracle_llmq_precompute`, `oracle_llmq_restart`, per-call traces with the DEC-16 hooks, the chain-overload scan and the canonical digest. Removed: `oracle_llmq_scan_reset_cache`.

---

## 8. Open questions for the manager

1. **The O17 row for the coinbase-CL predicate** (§3.3.1). Please assign the id and confirm the owner (EV4, with BS6a's decoder) and the five consumers, including which RPC task owns `getassetunlockstatuses`. Recommendation: class CON, `profile`, field `UngatedRules.cbtx_nonnull_cl_strict`.
2. **Schedule the O2a-LQ1 follow-up now.** *(Accepted after rev1; started.)* LQ1a-A (the Core bench, by ruling) and LQ1a-B's L1 gate both need its fixture loader; LQ1a is on the critical chain. Recommendation: a DEC-25(3)-style small task on both shim pins, started before LQ1a's implementation slot opens.
3. **Who builds the testnet replica, and when.** The order gate (§6.3.2 item 1) for LQ1a-B already needs it. Recommendation: O7's owner builds a testnet replica early (O7's mainnet reference later), shared by LQ1, EV2 and LQ3; until then LQ1a-B's gate runs on regtest FT chains only.
4. **`CacheHint::WorkBlock` from EV2** (§2.2.4). Recommendation: add it to `EV2-dml.md` §2.10 through the reconciler.
5. **SH1b (single SHA-256 of 64 bytes, multi-buffer)** (§5.3 L2). Recommendation: a small task under SH1's owner, after SH1 merges; L2 is optional for never-slower and only needed for the 2× route.
6. **perfgate `core = …` workload kind** (§5.4). Recommendation: ask PF0's owner to add it after PF0 merges; until then the Core comparison is recorded manually per phase.
7. **Who lands the scan runtime before QM1a (rev3; scope corrected in rev4).** M4 validation already reaches `ScanQuorums` and `GetQuorum`, and QM1a is an M7 task.
   - **Reached at M4:** CbTx best-CL selection on the CL type. Asset unlocks: a scan of `N + 1` and an explicit `GetQuorum`, both bound to the validated chain at v24. MNHF: an explicit `GetQuorum`.
   - **Rotation can be reached:** the Platform and MNHF types are non-rotated only by default. Devnet's `-llmqplatform`/`-llmqmnhf` accept rotated types (§3.8.4), so M4 can reach rotated member computation and row effects.
   - **Recommendation:** CH7b lands the complete M4 scan runtime in `dashd-quorums`, over the shared members facade, for every configured type validation can select:
     - the production scan facade and **scan cache**;
     - the `QuorumMaterializer`: the mined check, both `GetQuorum` routes, retained objects;
     - the **quorum-object cache**.

     It passes the scan suite for those types, including the rotated Platform override (case 7) with v24's bound-chain bypass and the cached scan after it.
   - **Deferred to M7:** contributions, warming and cleanup. Best-CL and asset-unlock checks verify against the commitment's `quorumPublicKey` (evo/specialtxman.cpp v23:83-87 / v24:200-204; evo/assetlocktx.cpp v23:140-150 / v24:138-148) and need no verification vector or secret share.
   - **QM1a** later adds them to the **same** runtime caches, with no second cache or facade, and adds the DKG-path second access with its trace-parity suite (§3.8.3).

## 9. PLAN row edits proposed

- **LQ1a:** dependencies `EV0, EV2a, BS5, CS0, CS4, BS6a, BS11a-t, i:EV4, O2a-LQ1, PF0`; acceptance adds `diff_type_enabled`, the first Core comparison in LQ1a-B, `quorum info` order gate (non-rotated); BG `crit:llmq_members_nonrot`, `crit:llmq_select` not worse than the previous head.
- **LQ1b:** dependencies `LQ1a, EV2a, i:BS8` (drop `i:EV2b`); acceptance: "dash-spv in-crate test over LQ1's fixtures (owner LQ1b-F) on the O4, regtest and per-era captures" replaces "cross-check vs dash-spv … on the 11 regenerated captures"; `quorum info` over all quorums as the order gate; BG `crit:llmq_rotation_cycle`.
- **LQ2:** merge dependency `LQ1b-E` instead of `LQ1b`; `process_block`/`undo_block` call the injected `QuorumPrecompute` (rev3).
- **CH7b (rev3, rev4):**
  - Implements the members facade and `QuorumPrecompute`.
  - Lands the M4 scan runtime in `dashd-quorums`, for every configured type validation can select: the production scan facade and scan cache, the `QuorumMaterializer` (both `GetQuorum` routes, retained objects) and the quorum-object cache (Q7).
  - Acceptance adds LQ1's `members_facade_suite`, the cold/warm/undo oracle fixtures, `scan_cache_model`, the padding vectors and LQ1's `scan_suite`, including the rotated `-llmqplatform` override with v24's bound-chain bypass and the following cached scan, all against the production implementations.
- **QM1a (rev4):** extends CH7b's scan runtime with contributions, warming and cleanup, keeping the same runtime caches. The DKG-path second members access is always performed. Acceptance re-runs the `scan_suite` and adds the DKG suite: against a DKG-configured oracle if it compares Core event traces, otherwise marked non-DKG with a separate DKG access-stream test (§3.8.3).
- **O2a-LQ1 (rev3):** the §6.2 entry-point list as revised (precompute, restart, traces with record-only DEC-16 hooks, chain-overload scan, canonical digest with its KATs).
- **EV4:** owns the coinbase-CL predicate and the new O17 row's consumers 2 and 4.
- **New small tasks:** O2a-LQ1 (shim entry points, §6.2), SH1b (kernel), the testnet replica (O7 owner).

---

## 10. Dispositions (rev1, rev2, rev3, rev4)

Review A (`reviews/LQ1-design-r1-opus-core.md`):

| Finding | Disposition |
|---|---|
| A-M1 `ScanQuorums` continuation reaches P2P | **Accepted (ruling).** Cache modelled: pure body with cache in/out (§2.5, §3.8.1), QM1's Core-faithful cache incl. padding (§3.8.3), reach corrected (§3.8.3, §3.9), Q2 withdrawn, DKG-08 precedent re-cited as O17's "model as state" policy |
| A-M2 coinbase-CL row mis-scoped | **Accepted (ruling).** One shared predicate owned by EV4/BS6a, five consumers, EVO-06/EVO-18 cross-refs, decode under `rules`/`bls` (§3.3.1); L2 invariant (§6.3.5) |
| A-m1 30b rationale, §3.9 contradiction | **Fixed** (§3.9, §3.11 30b/30c); constructed vector (§6.1) |
| A-m2 `log2` evidence gaps | **Fixed:** win64 call-site listings archived; glibc 2.29–2.43 source bridge and a 2.39 measurement added; remaining count-2 gaps listed (§3.10.3) |
| A-m3 pre-V19 reconstruction | **Fixed** (§3.6.3 28d; LQ1b-D acceptance compares against later quorums' embedded quarters) |
| A-m4 `select_start_height` | **Fixed:** `i32` inputs with Core's semantics (§2.5, §3.9) |
| A-m5 decoder bound | **Fixed:** `N·size/4 + skip_list.len()` (§3.6.2, §6.5) |
| A-n1 "not reached" | **Fixed:** default regtest pre-DIP3 cycles (+ vector), v24 dual chainstate out of scope (§3.7.1) |
| A-n2 shim constraints | **Fixed** (§6.2) |
| A-n3 cache mechanics | **Fixed** (§3.8.3) |

Review B (`reviews/LQ1-design-r1-opus-arch.md`):

| Finding | Disposition |
|---|---|
| B-M1 three `QuorumMemberSource` definitions | **Accepted (ruling).** One EV0 trait proposed (§2.6), `MemberError::Store` fail-stop, mapping table; deviation D4 |
| B-M2 L4 key incomplete | **Accepted (ruling).** Key on modifiers + row digests, proof, no `Err` caching, effect on every hit, runtime home (§5.3) |
| B-M3 never-slower not set up | **Accepted (ruling).** Core bench owned in LQ1a-A on `node/lq1-bench`, first comparison in LQ1a-B, PF0 and O2a-LQ1 dependencies, fixtures available now, per-era like-for-like (§5.4, §7, §8 Q2, Q6) |
| B-M4 certificate and arbitration | **Accepted (ruling).** Certificate = signer key multiset only; `quorum info` over all quorums is the order gate; mechanical classification; non-vacuity (§6.3.2, §6.4) |
| B-M5 Q4 premise, reflink copy | **Accepted (ruling).** Replica built the O7 way for writing/heavy calls; live oracles only for listed pure reads, throttled; r0's probe reclassified (§6.3.6) |
| B-M6 dash-spv not reachable | **Accepted (ruling).** Inverted: fixtures + in-crate dash-spv test, owner LQ1b-F (§6.4) |
| B-m1 scratch cost, cache hint | **Fixed:** `UsedSet` with `MnList::scratch()` twin (§2.2.4); `WorkBlock` hint requested (Q4) |
| B-m2 provenance, operator key | **Fixed:** provenance in the trait doc (§2.6); key/service consumers are concrete over `Arc<Dmn>` (§2.2.3) |
| B-m3 iteration-order test | **Fixed** (§6.5) |
| B-m4 dependencies | **Fixed:** LQ1a decoupled from BS8 (associated type), full LQ1a list, LQ1b on EV2a (§7, §9) |
| B-m5 L5 not exact | **Fixed:** raw facts, decode on read, miss → block read, `HAVE_DATA` at query time, work heights only (§5.3) |
| B-m6 L2 needs a new kernel | **Fixed:** SH1b task, own twin and QEMU tests, no borrowed numbers (§5.3, Q5) |
| B-m7 lever homes | **Fixed:** `newest_by_index` trait method; L1 via `SortedListMemo`; L4/L5 in `dashd-chainstate` (§2.5, §5.3) |
| B-m8 runtime sinks, overlay | rev1: write-if-absent runtime sink, overlay replaced by an assertion. **Superseded in rev2** (GPT M1, ruling 1): Core's unconditional overwrite at Core's write points, an attempt overlay for read-after-write, no row assertions (§3.7.2) |
| B-m9 `is_quorum_type_enabled` | **Fixed:** owned by LQ1a with overrides; `oracle_llmq_type_enabled` (§3.2, §6.2) |
| B-m10 non-vacuity, races, 30c coverage | **Fixed** (§6.3.2 item 5, §6.3.3, §6.4) |
| B-m11 captures | **Fixed:** per-era captures necessary; regtest capture and FT-chain captures added (§6.4) |
| B-m12 phasing | **Fixed:** G split into G1/G2; LQ2 depends on LQ1b-E (§7, §9) |
| B-m13 L2 list check | **Fixed:** stock `merkleRootMNList` until O8 (§6.3.1) |
| B-n1 EV2 names | **Fixed:** adapters re-targeted at `EV2-dml.md` (§2.2.4) |
| B-n2 memoize sorted lists | **Adopted** as L1 (§5.3) |
| B-n3 SML null collateral | **Fixed** (§2.2.4) |
| B-n4 domain check location | **Adopted:** `LlmqCtx::new` (§2.3) |

---

Review r2, GPT (`reviews/LQ1-design-r2-gpt.md`):

| Finding | Disposition |
|---|---|
| GPT M1 write-if-absent relies on an invariance the modifier contract breaks | **Accepted (ruling 1).** `SnapshotRows` with `CQuorumSnapshotManager` semantics (§2.3, §3.7.2); writes at Core's write points via a Core-faithful members cache (§3.7.4); identical-rewrite and cache-unobservability claims withdrawn (§3.7.1); attempt overlay restored; assertion removed; row-digest comparison and the cold/warm overwrite scenarios (§6.2, §6.6); D3 withdrawn. L4's derived-modifier key kept |
| GPT M2 Apple's largest known error is not a bound | **Accepted (ruling 2).** Apple count 32 is an open evidence gap with no parity claim for macOS hosts (§0, §3.10.3); the one-file check and the "if it prints 4" consequences (§3.10.5) |
| GPT m1 memo-key reads must stop where Core stops | **Fixed:** explicit read sequence, nothing past the marker (§5.3); first-cycle and unreached-read-error vectors (§6.1, §7 G2) |
| GPT m2 store-error rationale, expected absence | **Fixed:** ruling with Core precedent, EV2 cross-reference updated, `Ok(None)` vs `Store` distinguished and tested (§2.3, §2.6, §3.15 D4) |
| GPT m3 order gate misses entry-state provenance | **Fixed:** provenance fixture; per-position `pubKeyOperator`/`addresses` comparison; `oracle_llmq_members` returns member state (§6.1, §6.2, §6.3.2) |
| GPT note: regtest fallback is not testnet/mainnet coverage | **Fixed:** coverage reported per network and era (§6.3.2) |

---

Review r3, GPT (`reviews/LQ1-design-r3-gpt.md`, on rev2 at d44631bb):

| Finding | Disposition |
|---|---|
| 1 (major) precompute cannot use or reset the shared members cache | **Accepted (ruling 1).** CH7b-owned `QuorumPrecompute::precompute(block, Connect\|Undo, effects)`, defined in LQ1 and called by LQ2 (§2.5, §2.8). Every eligible type goes through the one process-wide facade with Core's gate, base-cache, route, index-guard and indexed-reset order (§3.7.4). Cache mutations are process state and survive failed or `fJustCheck` attempts. Undo resets only eligible types at cycle-base heights. The uncached composition is `reference`-only (`get_all_quorum_members_uncached`, `precompute_uncached`). The overwrite fixtures run against the production facade, with `PURE 240` as a known-bad control (§6.6); CH7b's acceptance carries the suite (§7, §9) |
| 2 (major) a commitment-only scan omits the quorum builds | **Accepted (ruling 2).** `scan_quorums_from` takes QM1's `QuorumMaterializer` and calls `get_quorum` for every newly scanned base, in order and beyond the returned prefix, with Core's `populate_cache` (§2.5, §3.8.1). The scan cache retains quorum objects, so a hit makes no build or member access, and the object-cache hits and the v24 chain overload are specified (§3.8.3). A later `None` empties the scan and keeps the earlier effects. The cross-cache sequences compare events and digests (cold scan, hit, continuation, discarded tail, later failure, chain overload) (§6.6). The oracle scan runs on the real shared managers (§6.2). The materializer core before QM1a is §8 Q7 |
| 3 (minor) the L4 order claim matches neither pin | **Accepted (ruling 3), qualified rather than ported.** §5.3 states both pins' orders and marks Rust's as an exact port of neither. Unobservability under fail-stop was checked read by read: EV2's list cache, the snapshot LRU, L5 (now explicitly non-inserting on a miss), L1 and the members cache are in-memory, and the computation's single write follows all its reads at both pins. No read has a surviving side effect, so no pin's order is ported. Tests assert sets and markers, not sequences (§6.1, §7 G2) |
| 4 (minor) row digest has no canonical preimage | **Accepted (ruling 4).** Core's serialized `llmq_S` keys, sorted as unsigned bytes, with CompactSize framing over the whole namespace; an empty store hashes `00`. Four known-answer vectors, with keys and rows reproduced by Core's own serializers. Defined: the read boundary, read without side effects, the event framing for written rows and traces, and per-call capture. `reset_cache` is a per-type members-cache reset behind Core's gates (§3.7.4, §6.2) |
| 5 (minor) EV2 describes expected absence as `Store` | **Accepted (ruling 5).** LQ1 only: §2.6 drops the dead commitment example and states that a scan's missing commitment is a `GetQuorum` value. The replacement text for EV2's duplicated comment and classification goes to the manager in the rev3 report; EV2 is not edited here |

---

Review r4, GPT (`reviews/LQ1-design-r4-gpt.md`, on rev3 at a0644ac6; all r3 findings closed):

| Finding | Disposition |
|---|---|
| 1 (minor) M4 subset must cover a rotated Platform override and own the scan runtime early | **Accepted.** "Non-rotated" is now qualified as the default configuration: `-llmqplatform` and `-llmqmnhf` accept rotated types on devnet with no guard, so M4 validation reaches rotated member computation and row effects (§2.5, §3.8.4). CH7b owns the production scan facade and **scan cache** with the materializer and object cache, for every configured type validation can select (§2.8, §8 Q7, §9). Scan-suite case 7 covers the override, v24's bound-chain bypass of the scan and the explicit lookup, and the cached scan after it, in CH7b's acceptance (§6.6, §7). QM1a extends the same caches (§2.8, §7, §9) |
| 2 (minor) optional masternode member hit contradicts the exact trace gate | **Accepted.** The permission to omit the access is removed: the materializer performs the DKG path's second members access exactly when Core does, on any node with a DKG session manager, i.e. masternodes and `-watchquorums` observers (§3.8.3). The `populate_cache` sentence is narrowed to the initial build access, the row writes and the object-cache insert. QM1a's DKG suite either uses a DKG-configured oracle for Core traces, or is marked non-DKG with a separate DKG access-stream test; the event is never normalised away (§3.8.3, §6.2, §6.6, §9) |

## Appendix: evidence index (`/work/scratch/lq1/`)

| Path | What |
|---|---|
| `src/v23`, `src/v24` | the Core files cited, extracted with `git show <tag>:src/<path>` |
| `fp/listings/*.s` | all 18 `log2` call sites (15 ELF/Mach-O listings + `*-win64-callsites.s`) and both win64 `log2` routines |
| `fp/tools/` | `log2probe.c`, `ifuncaddr.c`, `winlog2.c`, `find_win_log2.py`, `disasm_at.py` |
| `fp/glibc/`, `fp/glibc-bridge/` | glibc 2.31's `e_log2.c` from CS2's verified tarball; the 2.29–2.43 sources (`fetch.sh`) and `SUMMARY.md` |
| `fp/glibc-2.39-ubuntu2404.txt` | the glibc 2.39 measurement (both ifunc variants asserted) |
| `fp/binaries.sha256` | sha256 of the binaries the probes ran against |
| `rotationinfo-testnet-probe.txt` | r0's testnet probe (classified in §6.3.6) |
| `LQ1-quorum-members.{r0,rev1,rev2,rev3}.md` | the r0, rev1, rev2 and rev3 texts |
| `digest_kat.py`, `digest_kat.out` | the canonical row-digest definition and its four known-answer vectors (§6.2) |
| `corekey_main.cpp`, `corekey.out` | the KAT keys and rows recomputed with Core v24's `serialize.h`/`hash.h` (`SerializeHash(pair(LLMQType, uint256))`, `pair(string_view, uint256)`, `WriteFixedBitSet`); built against `corekey/src` (a `git archive` of v24.0.0-rc.3 `src/`) |
| `/work/scratch/lq1-r3-gpt/` | GPT r3's harnesses and results (members cache with precompute/undo, scan materialization trace, per-pin read traces, overlay model, digest ambiguity) |

## Erratum (EV0 API review, 2026-10-09)

**§3.8.2:** the claim that every call site passes an active-chain block is wrong for one site. Core's qrinfo builder checks the base block hashes against the active chain but not `blockRequestHash` (v24.0.0-rc.3 `llmq/snapshot.cpp:141`, v23.1.8 `:96`). For a stale request block it reads the **active chain's** commitment index under a height cap, not the stale branch. EV0 exposes that index-level read (EV0 M2), and LQ1a/LQ2's qrinfo path must use it for this site.

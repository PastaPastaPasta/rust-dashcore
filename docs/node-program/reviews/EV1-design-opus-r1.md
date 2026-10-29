# EV1 special-transaction design: Opus high review, round 1

Target: `node/ev1-design` at `d4c52e4cb`, `docs/node-program/designs/EV1-specialtx.md` (cited as "doc:line"). Reference: Dash Core `v24.0.0-rc.3` (`da567d7dba`), extracted read-only with `git archive`. Core paths are under `src/`. This is an independent design review, run after Sol r1 (CHANGES-REQUIRED) and Sol r2 (APPROVE-WITH-NITS). I did not re-check Sol's findings M1–M8 and m1–m2. Nothing was built or edited in the design worktree.

What I checked against the pin:
- **Dispatcher.** `specialtxman.cpp:217-282`.
- **ProTx contextual checks.** Every step of `CheckProRegTx`, `CheckProUpServTx`, `CheckProUpRegTx`, `CheckProUpRevTx`, `CheckProUpShareTx`, `CheckProUpSharedRegTx`, `CheckProDisTxForList`/`CheckProDisTx`, and both covenant functions (`specialtxman.cpp:1068-1849`).
- **Trivial validity.** Every `IsTriviallyValid` and list check (`providertx.cpp:20-589`), and the ProTx serializers (`providertx.h:162-560`).
- **Types 5–9.** `CheckCbTx`, `CheckLLMQCommitment`, `CheckMNHFTxImpl`, `IsValidMNActivation`, `CheckAssetLockTx` and `CheckAssetUnlockTxImpl`/`VerifyAssetUnlockSig`.
- **NetInfo.** `Validate`, `ValidateService`, `ValidateDomainPort`, `AddEntry`, `ProcessCandidate`, `NetInfoEntry` ordering/equality and the deserializer (`netinfo.cpp`, `netinfo.h`).
- **Callers.** `ProcessSpecialTxsInBlock` (`specialtxman.cpp:797-999`), `ConnectBlock` (`validation.cpp:2480-2716`), the mempool callers (`validation.cpp:802-825, 995-1018, 1097-1104, 1370-1404`), and `CBLSWrapper`/`CBLSLazyWrapper` (`bls/bls.h`).

Apart from the findings below, the order, class, reject string and condition matched the pin wherever I looked. That includes the forwarded migration reason's `TX_BAD_SPECIAL` class, the `ApplyProUpServTx`/`SetStateVersion` argument derivation in §3.4 and §3.5, the AU scan/window/get order, and the AU msgHash being a full serialization rather than the txid.

## Major

### M1. ProUpReg `key_changed` equality must be Core's `CBLSLazyWrapper::operator==`

**Doc:** §3.5 step 5a/5b, §2.6 EV2a row. **Core:** `specialtxman.cpp:1461-1478`, `bls/bls.h:514-535`, `bls/bls.h:77-80`.

The design must state that `key_changed` uses Core's lazy-wrapper equality as defined at those lines, and add vectors for it. The detailed scenario is held by the manager (PLAN §7 handling).

### M2. The block path does not send the coinbase through `CheckSpecialTxInner`; the design never states this and does not export `check_cbtx`

**Doc:**
- §2.4 entry points (doc:170-186): no `check_cbtx`.
- §2.6 CH7a row (doc:208).
- §3.2 steps 3.8–3.10 (doc:267-269).
- §6.2 "The oracle's mempool path is Core's block-path code for these types, so this covers both paths" (doc:822).

**Core:**
- `specialtxman.cpp:816-836`: the coinbase CbTx is checked **before** the loop, only when `fCheckCbTxMerkleRoots` and `vtx[0]->nType == TRANSACTION_COINBASE`. There is no DIP3 gate, and rejects are `BLOCK_CONSENSUS` directly.
- `specialtxman.cpp:865`: `if (i == 0 && block.vtx[i]->nType == TRANSACTION_COINBASE) continue;`. The loop never sends that coinbase to `CheckSpecialTxInner`.
- `validation.cpp:2663-2665`: `fCheckCbTxMerkleRoots = fScriptChecks`, which assumevalid controls.
- `validation.cpp:5197-5200`: `RollforwardBlock` passes `false`.

If CH7a feeds every transaction, coinbase included, to `check_special_tx_inner`, as the doc's dispatcher table implies:
- With `check_sigs = false`, it applies `CheckCbTx` (version and height rules) where Core applies none.
- Before DIP3 it returns `bad-tx-type-dip3-inactive` for a type-5 coinbase that Core accepts. Pre-DIP3 Core only runs `CheckCbTx` and the MN-list merkle root against the empty list (`:880-940`).
- The first reject also moves: Core checks the CbTx before tx 0's template-output check (`:816` vs `:855-862`).

The §6.2 claim is false for type 5. The oracle's dispatcher path cannot see this binding, and L2 history only replays valid coinbases.

**Fix:**
- Export `cbtx::check_cbtx`.
- Add to the CH7a row:
  - the coinbase rule (pre-loop, only under `check_sigs`, only when `vtx[0].nType == 5`);
  - the skip at `i == 0`;
  - that this path has no DIP3 gate.
- Narrow §6.2 to non-coinbase type-5 transactions.
- Add two A-1 / block-mode vectors:
  - a pre-DIP3 regtest block whose coinbase is a valid v1/v2 CbTx (accepted);
  - a block checked with `check_sigs = false` whose CbTx height is wrong (no CbTx reject).

### M3. The L1 generator mutates one field at a time, so it cannot detect misordered checks; one table already reads as two passes

**Doc:** §6.2 generator (doc:819). §6.5 `specialtx_check_diff` (doc:855). §3.14 `IsShareListTriviallyValid` (doc:598-614). §3.7 steps 8a/8b (doc:442-443).
**Core:** `providertx.cpp:77-117` runs steps 3a–3d **and** the script steps 4a–4d inside one per-share iteration. `specialtxman.cpp:1724-1737` interleaves payee and floor per share.

A first-reject order bug only shows when at least two checks fail at once. "Mutate a valid tx field by field" produces one fault per case, and coverage-guided fuzzing gains no new coverage from order mistakes. So the only order evidence is the §6.3 L3 pair matrix, and that covers types 7 and 9 alone.

The §3.14 table gives the share-loop checks top-level numbers 3 and 4, which reads like two loops. Core interleaves them. Example: share 0 has a P2PK refund script and share 1 has `amount < 100 DASH`. Core returns `bad-protx-shares-payee`. A two-pass port returns `bad-protx-shares-amount`.

This is the same class as Sol's NetInfo M6. Single-fault L1 would pass it; the fix is the same as for M6, an explicit per-element order.

**Fix:**
- Renumber §3.14 (and check §3.7) so the per-element scope is explicit.
- Add a pairwise first-reject requirement to L1:
  - for every table, generate cases that violate steps *i < j* together, across entities (share k with share k+1, purpose p with purpose p+1, payout k with payout k+1), and expect step *i*'s reject;
  - report pair coverage in the merge-gate report, next to census coverage;
  - make k ∈ {1, 2, 3} simultaneous mutations the generator default.

### M4. D6 maps every `UnknownProTxVersion` to `bad-protx-payload`; Core gives `bad-protx-version` when the payload is only the 2 version bytes

**Doc:** D6 (doc:904); §2.4 `PayloadSlot`/`is_complete` (doc:188).
**Core:**
- `providertx.h:167-171, 274-278, 338-342, 407-411`: an unknown version (0 or > 3) bails out after reading `nVersion`, without throwing.
- `evo/specialtx.h:24`: `GetTxPayload` succeeds when the stream is empty.
- So a 2-byte payload decodes. `GetValidatedPayload` then rejects with `bad-protx-version` (`specialtxman.cpp:1113-1116` for v ≥ 4). For v = 0, `IsTriviallyValid` rejects with `bad-protx-version` (`providertx.cpp:267-269, 414-416, 461-463, 488-490`).
- Only trailing bytes give `bad-protx-payload`.

base-sdk's decoder already models the bail-out (`SpecialPayload::UnknownProTxVersion`, consuming 2 bytes; BS6a). So the information exists, and D6 throws it away.

The verdict stays invalid and the class stays `TX_CONSENSUS`, but the reject string differs. A field-mutating generator never produces a bare 2-byte payload.

**Fix:**
- `UnknownProTxVersion` with `is_complete()` → `bad-protx-version` (C). Without it → `bad-protx-payload`.
- The covenant's template-output path is unaffected (`!IsShared()`).
- Add L0 vectors for versions 0, 4 and 0xffff, with and without trailing bytes, for types 1–4.

## Minor

### m1. On the mempool path, `rules` come from the tip, not from `prev`

**Doc:** §2.3 `SpecialTxRules` doc comment (doc:127); §2.6 MP1a row (doc:210).
**Core:** `validation.cpp:1103` passes `GetSpecialTxRules(tip)`. Inside, `check_at(chain[first_height])` (`:822`) keeps those rules, while `pindexPrev` (DIP3 gate, V19 `GetMax`, scan) is the earlier block.

If `rules` are derived from `prev` in the expired-AU retry, a v2 AU whose `requestedHeight` precedes V24 activation gets `bad-assetunlocktx-version-2` where Core accepts it.

**Fix:**
- Bind `rules = GetSpecialTxRules(tip)` explicitly in the MP1a row, and fix the doc comment.
- Also cite the second mempool call site, `validation.cpp:1370-1404` (fresh-instance AU replacement: tip, tip rules, `CoinsTip`, `check_sigs = true`).

### m2. BLS verification involves three schemes, and D10's "version-1 payloads" understates the legacy dependence

**Doc:** §2.5 (doc:197-198); D2 (doc:900); D10 (doc:908); §6.1 "Both BLS encodings" (doc:804).
**Core:**
- `bls/bls.h:183-201`: eager `CBLSSignature::Unserialize` tries the payload's scheme, then the other one. It calls `Reset()` (an invalid signature that still decodes) when only the other scheme is canonical. It throws only when neither is.
- `bls.cpp:296-312`: `VerifyInsecure` uses the global flag.

So a ProUpServ/ProUpRev signature is decoded under the payload version, the key under the stored lazy flag, and the verify under the global flag. The `SigVerifier` signature has one `scheme`.

Also, v2/v3 ProUpServ/ProUpRev signature bytes that are canonical only in legacy decode to an invalid signature. With `check_sigs = false` they are **accepted**, and bytes canonical in neither scheme are `bad-protx-payload`. So the legacy predicate decides validity at every version, not just v1. Gating is unchanged, since EV1-2 onward already wait on BS11a.

**Fix:**
- State that `BlsSigRef`/`BlsPublicKeyRef` carry their decode outcome or scheme, and that `scheme` is the verify scheme only.
- Reword D10.
- Add `check_sigs = false` vectors for v2/v3 signatures that are legacy-only-canonical and neither-canonical.

### m3. Signed preimages hash Core's re-encoding, but the preimage test never uses a non-canonical wire form

**Doc:** §3.18 (doc:748-751); §6.1 "Signed preimages" (doc:806).
**Core:**
- `SerializeHash(ptx)` (`specialtxman.cpp:1072, 1091`), `MakeSignString` (`providertx.cpp:375-395`) and `MakeSharedRegConsentHash` (`:356-357`) serialize the **decoded** object.
- `ExtNetInfo::Unserialize` builds `m_data` as a `std::map` (`netinfo.h:422-434`). A repeated purpose keeps the first occurrence, and the map re-encodes in purpose order.

A wire payload with a repeated or unsorted purpose can still be valid. Its signature covers the deduplicated, sorted re-encoding, not the wire bytes. "1,000 generated payloads per type" built from Rust structs are always canonical, so a port that hashes wire bytes would pass.

**Fix:** add signed-preimage vectors (ProUpServ sig hash, ProReg sign string, shared-reg consent hash) with repeated and out-of-order ExtNetInfo purposes, checked against `special_tx_sign_hash`.

### m4. Possible inconsistency with ruling D9: EV1 exports the converter that EV2b uses, so the equality test becomes tautological

**Doc:** §0 item 6 (doc:25); §2.4 (doc:180-185); §2.6 EV2b row (doc:212); §3.16.1 "EV2b equality" (doc:721).
The ruling says EV1 owns **only** a read-only failure predicate, equality-tested against EV2b. The design has EV1 own `convert_for_version`, which produces the converted `NetInfo`, and has EV2b's `migrate` call it. The equality test then compares EV1's predicate with EV2b wrapping EV1's own converter. It no longer checks two independent implementations; it checks only EV2b's argument wiring.

**Recommended:**
- EV1 exports `migration_reject` (plus `add_entry`, as a NetInfo primitive).
- EV2b implements its conversion over `add_entry` itself.
- The equality test then compares two implementations.
- Otherwise, record a manager confirmation that EV2b may consume `convert_for_version`, and name the independent evidence (L1 through ProUpServ/ProUpReg verdicts).

### m5. The seam contracts must carry Core-throw outcomes

**Doc:** §2.2 (doc:113-122); D5 (doc:903); D11 (doc:909); D1-f (doc:899).
**Core:** `specialtxman.cpp:267-271` catches any `std::exception` thrown during the switch. That includes exceptions from code EV1 reaches through seams: `CFinalCommitment::Verify` → `GetAllQuorumMembers`, `GetQuorum`/`BuildQuorumFromCommitment`, and `ScanQuorums`.

§2.2 only lists throw sites in EV1's own ported functions. The `CommitmentVerifier` and `QuorumLookup` contracts offer only `Ok` or `SeamError`. A deterministic Core throw inside a seam implementation would become a fail-stop fault where Core rejects with `failed-check-special-tx`.

**Fix:**
- Require each seam implementer (LQ1a, LQ2/SB8) to return a distinct "Core throws here" outcome for deterministic throws, which EV1 maps to `failed-check-special-tx`. Reserve `SeamError` for local I/O.
- Record as deliberate the deviation for storage errors: Core maps `dbwrapper_error` to `failed-check-special-tx`; D11 aborts.

### m6. Slice plan: some checkpoints depend on work the dependency column omits

**Doc:** §7 (doc:884-888); §6.1 D13 tests (doc:812); §6.3 (doc:829-839).
- **EV1-3.** The D13 coins-view tests are "MP1a integration tests … asserted against … pinned dashd (L3)", but MP1a is not a dependency. Make them L0 against a model mempool view in EV1-3, and move the L3 half to MP1a's acceptance.
- **EV1-5.** L2 ("replay … through CH7a with EV1 in place") and the L3 matrix ("EV1 in the node under test") need CH7a, MP1a and a runnable node. CH7a in turn consumes EV1-5's type-7/9 API, a possible cycle. Merge EV1-5 on L0 + L1 (A-1). Make L2 and L3 a joint certification row with CH7a/MP1a, or list them as dependencies and let CH7a land against stubbed type 7/9 checks.
- **EV1-1.** The narrowed `netinfo_validate` property compares tx verdicts through ProReg/ProUpServ wrappers. The Rust side must reproduce the earlier NetInfo trivial steps: §3.3 steps 9–11 / §3.4 steps 4–7, part of which (`trivial.rs`) lands in EV1-2. Say that EV1-1's harness carries those steps, or move the property to EV1-2.
- **EV1-1.** NetInfo needs a Core-exact `CNetAddr`/`CService` (numeric `Lookup`, `SetSpecial`, `MaybeFlipIPv6toCJDNS`, `IsRoutable`, `IsBadPort`, `ToStringAddr[Port]`). Name its provider as a dependency.
- **EV1-4.** L1 with `check_sigs = true` runs into pre-V19 (legacy global flag) contexts, which need A-4's legacy verify. Say whether those cases are excluded and counted, or block the gate, until A-4 resolves.

### m7. Nits

- **§3.15 step 6c (doc:650).** When `netInfo` is empty (ProReg `allow_empty`), the "core P2P entry's port" is 0. `MnNetInfo::GetPrimary` returns `CService{}` (`netinfo.cpp:336-342`), so `platformP2PPort == 0` gives `bad-protx-platform-dup-ports`. State this, and add a vector.
- **§3.14 (doc:598).** The `IsShareListTriviallyValid` signature also takes `keyIDVoting` (`providertx.cpp:63-64`).
- **§2.3 / §3.11 (doc:168).** `ancestor(prev, h)` must return "not equal" (not panic) when `h > prev.height` (`GetAncestor` → `nullptr`).
- **§6.5 `prodis_arith` (doc:859).** The zero divisor cannot be reached from consensus state (≥ 2 shares of ≥ 100 DASH, `providertx.cpp:67-81`). Keep the test, but describe it as defensive.

## Rulings check

- **D9.** Consistent except for m4.
- **Slices.** The gating matches (EV1-1 NetInfo + census; EV1-2/3 on BS11a; EV1-4 on BS12; EV1-5 on A-1).
- **DEC-82.** v24-only throughout. No release profile is modelled.

CHANGES-REQUIRED

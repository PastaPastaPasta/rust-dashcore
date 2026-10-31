# SB8 locks + commitment verification — Opus r1 (second reviewer)

**Verdict: ACCEPT WITH NITS**

Scope: SB8's own commits 2f5219fef, e042fcc5c, 82efaea43, 7b13febcc, 580cb95bb and 8b3eb8705 on `node/sb8-locks` (head 8b3eb8705, base 3d6f94444), read through `git diff 3d6f94444 82efaea43` and `git diff 592e09370 8b3eb8705`. Core reference: v24.0.0-rc.3 (0364ed626e), read with `git show 0364ed626e:<path>`. The local `~/workspace/dash` checkout is at 15db478750, but `llmq/commitment.cpp`, `llmq/utils.cpp`, `llmq/quorumsman.cpp`, `chainlock/` and `instantsend/net_instantsend.cpp` are the same as rc.3. I did not look into the private security item. Where the code meets it (D8, and D18's single-member mined path), I only checked that the code follows Core's control flow.

I found no safety defect: nothing accepts a lock or commitment that Core rejects. All the findings below are about evidence or documentation, plus one theoretical over-reject that can only cost liveness.

## Findings

### 1. Low (evidence/docs): `SmlScratch` has almost no test evidence, and D22's justification for it does not hold
- **Where:** `dash-evo/src/sml/view.rs:150-234`; D22 in `docs/node-program/designs/SB8-notes.md:38`; `node-l1/tests/l1_llmq.rs:1666`.
- **What:** D22 says the scratch semantics "are pinned by the comparison of `compute_quorum_members` under both views on every recorded quorum." They are not. `new_scratch` and `try_add` are only called by rotated selection (`dash-evo/src/llmq/rotation.rs:234-268`; Core `llmq/utils.cpp:375-435`). Every recorded quorum SB8 compares is non-rotated, because rotated ones are skipped (D1/D17), and non-rotated selection never builds a scratch. So no recorded quorum ever runs `SmlScratch`.
- **What tests exist:** dash-evo has no unit test for `SmlScratch` or `SmlMember` (`cargo test -p dash-evo --lib sml::view` matches 0 tests). The only evidence is three node-l1 cases: shared collateral, shared owner key and shared operator key.
- **Branches with no test:**
  - duplicate proTxHash;
  - legacy service uniqueness;
  - extended service and domain uniqueness;
  - `NIEntry::Invalid` rejection;
  - Evo platform-node-id uniqueness and its null exemption;
  - all-or-nothing rollback when a later key conflicts;
  - the same masternode holding a value again;
  - `for_each(only_valid)`.
- **Why it matters:** this scratch sits on SPV's main InstantSend path. Mainnet `llmqTypeDIP0024InstantSend` is rotated, so every SPV `isdlock` check depends on rotated members over the SML.
- **Safety is not at risk.** `membersSig` binds the signer-key multiset, so a scratch divergence can only make a quorum `Unverified` (liveness). That makes this an evidence gap, not a defect.
- **Fix:**
  - Add dash-evo unit tests for each branch above.
  - Rewrite D22 to say that the rotated SML-vs-DML member comparison is SB11's gate (EV addendum m3), not something SB8 has shown.
  - Add a doc line on `SmlScratch` saying it deliberately breaks `ScratchMnList`'s "exact acceptance rule is consensus" contract (`dash-evo/src/llmq/view.rs:85-87`). An `SmlList` must therefore never back a node or consensus path.

### 2. Low (correctness, liveness-only, unreachable in practice): `SmlScratch` registers legacy addresses Core does not, so D5's "can only under-reject" claim is not strictly true
- **Where:** `dash-evo/src/sml/view.rs:168-172`.
- **Rust:** it registers `LegacyService(addr, port)` for any address that is not all zeros.
- **Core:** `MnNetInfo::Unserialize` builds `NetInfoEntry{service}`, and that constructor leaves the entry empty unless `service.IsValid()` (`evo/netinfo.h` rc.3:212-217, :343-348). `GetEntries` returns nothing for an empty entry (`evo/netinfo.cpp` rc.3:331-339), so `AddMN` registers no property (`evo/deterministicmns.cpp` rc.3:459-477).
- **Divergence:** for a non-zero address that is not `IsValid()` (e.g. `::ffff:0.0.0.0`, `255.255.255.255`, `2001:db8::/32`), two masternodes holding the same one are both accepted by Core's scratch, while `SmlScratch` rejects the second. That is an over-reject.
- **Reachability:** ProRegTx and ProUpServTx validation should keep such services off the chain, so this is theoretical. It still contradicts D5's stated invariant.
- **Fix:** skip the key unless the address passes Core's `CNetAddr::IsValid`, using whatever base-sdk exposes (a helper may be needed). Otherwise, qualify D5.
- **Nit, same area:** a legacy service and the same service in an extended entry get different keys. Core keys both as `CService` through `GetUniquePropertyHash`, so this is a documented under-reject. Normalising IPv4-mapped legacy addresses into the extended key would remove it cheaply.

### 3. Low (evidence): the oracle differential covers signature mutations only; structural checks are tested by restatement
- **Where:** `node-l1/tests/l1_llmq.rs:1360-1484`.
- **Real differentials today:**
  - The four §5.1 `membersSig` mutations are judged against Core's `Verify(…, true)`.
  - Every recorded commitment is accepted. Core mined them with `fBLSChecks`, so that acceptance is also Core's verdict.
- **What lacks a Core comparison:**
  - The 13 structural checks, their order, and the trailing-bit loop are tested only by synthetic dash-evo unit tests (`every_structural_rejection_in_cores_order`, `bits_past_the_members_are_rejected`).
  - The gate compares accept/reject only, not the reason.
- **Context:** I checked the order line by line and it matches (see the checked list below), so this is not a defect.
- **Fix (cheap):** the oracle's `verify_commitment` already returns Core's LLMQ log line as `debug_message` (`dash-oracle-sys/src/context.rs:701-726`). Mutate each sampled commitment and compare both the verdict and Core's line with `CommitmentReject`'s `Display`. Mutations to cover:
  - nVersion ± 1;
  - quorumIndex;
  - signers / validMembers size;
  - clearing bits below `minSize`;
  - zeroed quorum key;
  - null vvec hash;
  - zeroed membersSig / quorumSig;
  - a member list shorter than a set bit;
  - a quorumSig from another commitment.

### 4. Low (evidence): Sol r1's ISLock finding — I agree, and Low is the right severity
- **Where:** `dash-evo/src/locks/tests.rs:140-184`.
- **The gap:** no `verify_islock` acceptance is backed by a Core verdict. Core's vector carries no quorum key. Its quorum (`…99334a2`) is not in `llmq-samples.json.gz`, which I checked. The regtest fixture's InstantSend type is rotated, and rotated is out of SB8 (SB11).
- **Why the gap is narrow:**
  - Each piece is pinned against Core: request id, `SignHash`, the lazy basic-scheme read, and two-pass composition (`islock_two_passes`).
  - `verify_with` / `verify_insecure` / `sign_hash` run on real data in the regtest ChainLock test.
  - What is untested is rotated selection plus sign height on a real scan, which is SB11's fixture work.
- **Fix:** as Sol suggests. Track it as an SB11 entry criterion: a captured regtest `isdlock` with its `qrinfo`.

### 5. Nit: two tips in Core's InstantSend selection, one tip in `QuorumScanView`
- **Where:** `dash-evo/src/locks/mod.rs:192, 485-491, 319-320`.
- **Core:** it takes `nSignHeight` from `m_is_manager.GetTipHeight()` (`instantsend/net_instantsend.cpp` rc.3:143-148). It takes the `-1` substitution and the `startBlockHeight > Height()` bound from `active_chain.Height()` (`llmq/quorumsman.cpp` rc.3:711-722).
- **Rust:** it uses `tip_height()` for both.
- **Effect:** the two agree whenever the IS manager's cached tip equals the active tip. They can differ only transiently in Core.
- **Fix:** say so in the `tip_height` doc, or split the method if the node needs Core's transient behaviour.

### 6. Nit (D20 wording; hand-off to LQ2)
- **Where:** D20, `SB8-notes.md:36`.
- **The wording:** D20 calls the CbTx shortcut's cached ChainLock "the same check". That is not exact. Both shortcuts compare only (height, signature), not the locked block's hash (`evo/specialtxman.cpp` rc.3:151-153, :177-178). Core's handler does verify every ChainLock it adopts (`chainlock/handler.cpp` rc.3:154-163).
- **SB8's conclusion holds:** always verifying can only reject where Core skipped, i.e. it only ever under-accepts. Confirmed.
- **Hand-off:** for LQ2's block path, an under-accept is a rejection of a block Core accepts. LQ2 should decide whether to model the (height, sig) cache.

## Checked and consistent with Core

**Item 1: `CFinalCommitment::Verify`** (`dash-evo/src/llmq/commitment.rs:264-327`; Core `llmq/commitment.cpp` rc.3:113-209)
- Every check runs in Core's order:
  1. type;
  2. parentless base;
  3. version against `GetVersion(IsQuorumRotationEnabled(base), V19 after base)`;
  4. quorumHash;
  5. `height % dkgInterval` against the `int16` index (0 when absent);
  6. signers size, then validMembers size;
  7. valid-members count, then signers count, both against `minSize`;
  8. key validity;
  9. vvec hash (skipped for single-member types);
  10. membersSig object, then quorumSig object;
  11. a single member read;
  12. the trailing-bit loop (validMembers before signers, per index);
  13. signatures.
- D23: an unconfigured type is the first rejection. This matches Core.
- **Scheme handling:**
  - BLS fields are decoded under the commitment version's scheme (`commitment.h` rc.3:95-114).
  - `BuildCommitmentHash` serialises the key under the caller's flag, with zeros when invalid (rc.3:312-322).
  - Both verifications use that same flag (D3).
- **Member views:**
  - `Dmn::operator_key` (lazy `Get()`) matches `CBLSLazyWrapper::Get` (`bls/bls.h` rc.3:492-510).
  - `SmlMember` decodes under the entry version's scheme, with no fallback and an invalid object on failure. This matches SML serialisation (`evo/simplifiedmns.h` rc.3:79-83) and copies the state's version.
  - `confirmedHashWithProRegTxHash` is a single SHA256, zero until confirmed. It is set only in `UpdateConfirmedHash` and never reset (`evo/dmnstate.h` rc.3:162-169; `specialtxman.cpp` :340).
- **D18 signature paths:**
  - `Sync` is `VerifySignatureAsync(nullptr)`.
  - `Mined` matches `ProcessBlock`, which ignores `VerifySignatureAsync`'s return value (`blockprocessor.cpp` rc.3:266-283) and judges only the queued checks.
  - `BlsCheck` aggregates for more than one key, checks directly for one, and returns false for none (`utils.cpp` rc.3:561-579).
  - Core's `VerifySecureAggregated` with no keys is false (`bls.cpp` rc.3:341-345).
  - The single-member Mined branches (empty members → Ok; member Invalid → Ok; member Unsupported → depends on the quorum signature) follow Core's flow exactly.
  - "A definite false decides over Unsupported" holds on both paths.
- No peer-reachable panic:
  - `members[0]` is guarded.
  - `dkg_interval` is `max(1)`.
  - The bitset hash runs after the size check.
  - The key collection is bounded by `params.size`.

**Item 2: ChainLocks** (`dash-evo/src/locks/mod.rs:379-418`; `chainlock/clsig.cpp` rc.3:248-260; `quorumsman.cpp` :711-798, :849)
- The request id is `SerializeHash(pair<string_view,int32>)`.
- The start block is `signHeight - 8`, with `-1` meaning the tip; a start that is negative or past the tip gives NoQuorum.
- Selection runs over the whole scan before any status is read.
- `SignHash` uses type, quorumHash, id and msgHash.
- The eager signature is read under the flag, and other-scheme bytes become the invalid object (`bls.h` rc.3:184-203, D13).
- D19's type binding is correct.
- D20 confirmed: SB8 only under-accepts (finding 6 is a wording note).

**Item 3: ISLocks** (`dash-evo/src/locks/mod.rs:434-568`; `net_instantsend.cpp` rc.3:54-170, :237-290, :330-354; `lock.cpp` :18-39)
- `MAX_INPUTS` is checked before the dedup set.
- The null cycle hash is unknown.
- The cycle must be on the interval.
- The lazy signature is validated before selection.
- Sign height is `cycle + interval - 1` only when `cycle + interval < tip`.
- Rotated quorums select; non-rotated ones use `GetQuorum(cycleHash)`, with D19's hash binding.
- **Two passes:** Core retries only the `badMessages` set (no quorum or invalid signature). Rust always tries pass 2 and accepts if either pass verifies. That is still sound: a valid pass 2 means Core accepts whatever pass 1 gave.
- `combine` claims Core's rejection only when both passes are Core verdicts.
- The `HasRecoveredSig` skip (D20) is under-accept only.

**Item 6: peer-reachable panics and unbounded work.** None found in SB8's new code. There are no `unwrap`/`expect` calls on peer data in `commitment.rs`, `locks/mod.rs` or `sml/view.rs`. All loops are bounded by `params.size`, `MAX_INPUTS` or `signingActiveQuorumCount`.
- `select_from_scan`'s panic on a zero rotated count is LQ1 code, and it is reachable only from local `ChainParams`.
- `verify_recovered_sig` takes `&ChainParams`, not an `LlmqCtx`, so `LlmqCtx::new`'s domain check does not guard it. That is config, not peer input.

## Validation (this round, worktree clean, nothing committed)
- `cargo test -p dash-evo --lib -- llmq::commitment locks:: sml::view`: 27 passed, 0 failed (the `sml::view` filter matches no tests; see finding 1).
- `cargo test -p node-l1 --features oracle --test l1_llmq` on the four SB8 tests: all four passed (`sampled_commitments_verify_under_both_views_and_mutations_match_core`, `regtest_commitments_verify_under_both_views`, `regtest_coinbase_chainlocks_verify_against_cores_scan`, `the_sml_scratch_under_rejects_what_the_sml_does_not_carry`), in 59 s. The known LQ1b failures were not run.

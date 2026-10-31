# LQ4 design review, round 1 (Opus-h)

**Under review:** `designs/LQ4-dkg.md` rev0 (593 lines, commit 4d95151ca), DEC-616, and the PLAN rows LQ4a/LQ4b (:596-597) and DK1 (:702).
**Reference:** Dash Core `v24.0.0-rc.4` (49caf372bb). rdc `node/integration` 0729966a0, bsdk fork pin `f27bc4f5`. Citations below use the design's prefixes (`a/` = `src/active/`, `l/` = `src/llmq/`).
**Verdict: CHANGES-REQUIRED.** That is APPROVE-WITH-CHANGES on REVIEW-COMMON's design scale: all three majors can be fixed in the document, and none needs a redesign.

## Summary

The state-machine transcription is very good. I re-read every rc.4 function the design ports, and the order of checks, the hook points, the scores and the relay rules match. Almost every file:line citation I checked is exact (list at the end). The pure boundary is right in shape: no clock, no threads, injected RNG and store, and batched verification is correctly placed in the session.

The gaps are in the contract with neighbouring rows and in the evidence:
1. The store trait lets DK1 drop a members access that LQ1 ruled must be kept (M1).
2. No Core-produced evidence goes beyond 12 members, though some Core paths only run at 32 or more (M2).
3. LQ4a-2's headline acceptance can't be met as written, because some replayed messages are random (M3).

The PLAN edits for DEC-616 #2/#4 are also incomplete (m4).

**Check 2, the batch-verification move (DEC-616 #7).** Accepting messages in the pure core is equivalent to Core's handler in every ordering if two things hold:
- DK1 hands over exactly Core's popped batch (FIFO, at most 8, own messages included);
- the contract pins the four properties in m6.

Under those conditions I found no ordering in which the accepted set differs.

**Check 4, SW-UPSTREAM.**
- `dkg::wire` + `dkg::bls` contain the field-privatization risk of base-sdk #89. The public API exposes BS9a and pkc *types* but not their fields, which is fine.
- I checked `ecb3905d` (not in `f27bc4f5`). Decode still rejects non-zero padding, as Core's `ReadFixedBitSet` does (`serialize.h:470-474`), so intake verdicts don't change.
- `encode` no longer masks padding, and `count_ones` counts it. So every bitset `wire` builds (validMembers, badMembers, complainForMembers, signers) must come from the validating constructor (`b5ffc07c`), never from raw bytes.
- Recommendation: after the rebase, rerun T1's decode/structure rows and T3 as the regression gate.

---

## Major

### M1. `DkgStore::verified_contributions` lets DK1 skip Core's members access, which LQ1 ruled must be kept

- **Where:** design §3.2 (`fn verified_contributions(&self, t, base, members: &[ProTxHash], valid) -> Option<…>`) and §2.8 step 4.
- **Core:** `GetVerifiedContributions` itself calls `utils::GetAllQuorumMembers(llmqType, {…, pQuorumBaseBlockIndex})` (`l/dkgsessionmgr.cpp:147`) on **every** call. On a member that means `SendCommitment` (`a/dkgsession.cpp:505`) and **every** received pcommit (`l/dkgsession.cpp:579`). Init calls it (`l/dkgsession.cpp:92`), and so does the relay-member build (`l/dkgsession.cpp:137`, `GetQuorumRelayMembers`, `utils.cpp:778`). Finalize's `fqc.Verify` calls it too (`a/dkgsession.cpp:672`; single-member :732).
- **Program rule:** LQ1 "Changes …, item 2 (finding 2)" says the DKG path's members accesses are kept and traced: "the permission to omit it is removed", and QM1a's DKG-configuration suite compares the access stream. Facade cache warmth decides recompute and row writes for rotated types (LQ1 §3.7.4).
- **Failure scenario:** DK1 implements `verified_contributions` from the `members` slice the session passes in, which the signature invites. The Rust node then makes one facade access per session where Core makes 2 + N(pcommits) + finalize. The DKG-configured QM1a trace diverges. If a facade miss occurs between accesses (eviction, the undo `reset_cache` path), Core recomputes and may write rotated snapshot rows where Rust does not.
- **Fix:**
  - Remove `members` from `verified_contributions`. State that DK1's implementation performs Core's `GetAllQuorumMembers` facade call per invocation, at `dkgsessionmgr.cpp:147`, before the cache lookup.
  - Add a short "members access points" table to §3.4: Init (DK1, before `DkgSession::init`), relay members (LQ5 input, members only), each `verified_contributions` call, `FinalVerifier` (finalize and single-member).
  - Add a T6 per-step comparison of the access count, which the oracle shim can report.

### M2. Core-produced evidence stops at 12 members, but some Core paths need at least 32

- **Where:** design §4.1 (largest Core capture is 10–12 members, DEC-616 #4); T5 member sets (`llmq_test`, `llmq_test_dip0024`, devnet 12/7/6); T6 runs "the same action scripts". T4 (3–400 members) is Rust against Rust only.
- **Core paths no planned parity test reaches:**
  - `ReceiveMessage(qcontrib)` runs `VerifyPendingContributions` **in the middle of a batch** when the 32nd pending index is queued (`l/dkgsession.cpp:259-263`). From that point it writes encrypted rows (`a/dkgsession.cpp:146`) and verified-sk rows (:177) during the Contribute phase rather than at Complain. It also changes which members are in each `VerifyContributionShares` call, so the later call at `VerifyAndComplain` (`a/dkgsession.cpp:191-194`) sees only the remainder.
  - A single `VerifyContributionShares` call spanning several 8-chunks, with the fallback in one chunk only (`bls_worker.cpp:520-566`). With at most 12 members, Core-produced data has at most two chunks per call, and never a full call of 32.
  - `ReceiveMessage(qpcommit)` / `FinalizeCommitments` with `memberIndexes` of mainnet shape (≥ 40 valid).
- **Failure scenario:** a port that verifies all pending contributions only at Complain, or regroups the calls differently, still passes T1–T4 with fewer than 32 members (verdicts equal per member) and T4's self-consistency at 50/400. But its dkgdb write order and time, and its per-call grouping, differ from Core's on every mainnet quorum type. Nothing Core-backed catches it.
- **Fix:**
  - Give T6 (`diff_dkg_session`) at least one script family with ≥ 33 injected members. `llmq_50_60` or a cut-down 40-member `llmq_test` override both work, since P-2's injected-members seam makes this cheap.
  - Include contribution orders that cross the 32 boundary inside one batch.
  - Add a T1 row asserting the mid-batch trigger and that store writes happen during Contribute.
  - Optionally, extend the DEC-616 #4 capture to ≥ 33 MNs if regtest can host it. If not, say so, and let T6 carry it.

### M3. LQ4a-2's "BS12 transcript reproduced byte-exact" can't pass as written

- **Where:** design §4.3 T3 step 4 ("all are deterministic: BLS signatures are unique and the bitsets are fixed"); §5 LQ4a-2 acceptance ("T3 complaints + justifications byte-exact on BS12"); LQ4b-1 ("T3 full sessions byte-exact (qpcommit, qfcommit)").
- **Core:** the erring member's own messages in the lie scenarios contain fresh randomness:
  - justify-lie replaces each share with `MakeNewKey()` (`a/dkgsession.cpp:399-402`);
  - commit-lie draws `lieType = GetRand<int>(5)` (:533), and lieType 0 draws a random quorum key (:537-540).

  T3 step 2 adopts only the member's **contribution** from Core's bytes (`ContributionSource::Recorded`). Nothing makes the session reproduce Core's justify-lie keys or commit-lie choice.
- **Failure scenario:** replaying BS12's "contribution-lie + justify-lie" session with `m` = the erring MN builds a qjustify with different random keys. The byte equality fails. The acceptance is unmeetable for that member, and the slice can't be closed honestly. The same happens in LQ4b-1 for the commit-lie session.
- **Fix:** pick one of these.
  - (a) Generalise "Recorded" to every lie output: adopt the captured qjustify / qpcommit as own for the erring member, as for the contribution, and assert only the honest members' outputs byte-exact.
  - (b) Drive the decisions through the typed RNG seam of m7 with values recovered from the capture (lieType from the bytes; justify keys from the captured qjustify).

  State which, and restate the acceptance as "byte-exact for every member whose own outputs are deterministic given the replay inputs; the erring member's lied fields come from the capture".

---

## Minor

### m1. §2.1: "Finalize starts at 5pb, which equals `dkgMiningWindowStart` for every shipped type" is false

- Rotated types: `llmq_test_dip0024` and `llmq_devnet_dip0024` have window 12 with 5pb = 10, and `llmq_60_75` has 42 with 5pb = 10 (`l/params.h:233`, :311, :390: "signingActiveQuorumCount + dkgPhaseBlocks * 5").
- It's harmless to the session, but MN0/CH7 readers may rely on it.
- **Fix:** "equals it for non-rotated types; rotated types add `signingActiveQuorumCount`."

### m2. Observer vs non-member semantics are mislabeled in `SessionInit`

- **Where:** §3.2, `my_pro_tx: Option<ProTxHash> // None = observer-mode session`, and `relay_members` taken unconditionally.
- **Core rc.4:**
  - Observers (no active MN) run no session at all. They ignore DKG messages (`l/net_dkg.cpp:511-518`) and start no phase threads (:739).
  - `myProTxHash` is set **only if the proTx is among the members** (`l/dkgsession.cpp:106-116`). So a "null" session is an active MN that isn't in this quorum.
  - `relayMembers` and `InitLocalSessionStatus` happen only for members (:135-146). A non-member therefore relays to qwatch peers only (`net_dkg.cpp:340-345`).
- **Failure scenario:** DK1 passes AM1's proTx and LQ5's relay set for a quorum the MN isn't in. The Rust non-member then relays invs to "relay members" that Core never computes for it, and may initialise debug status that Core doesn't.
- **Fix:**
  - Rename the field to the active MN's proTx.
  - `init` derives membership as Core does.
  - Require `relay_members` to be empty unless the MN is a member (or compute "member ⇒ use it" inside `init`).
  - Add a non-member replay to T3, from a capture with more MNs than the quorum size (the rotation capture has one).

### m3. §4.4 commit-lie mechanics name the wrong rejection points

- **Where:** §4.4 row commit-lie: "types 0–2 are rejected at receive where the vvec builds; types 3–4 fail the batch signature or the quorumSig check".
- **Core:**
  - Types 0/1 change a field before hashing, so `sig` is valid over `GetSignHash`. They are rejected in `ReceiveMessage(qpcommit)` on the key or vvecHash mismatch (`l/dkgsession.cpp:593-601`): no relay, no score.
  - **Type 2** changes the hash *after* `BuildCommitmentHash` (`a/dkgsession.cpp:547-552`). So `sig` does **not** verify over `GetSignHash` (`l/dkgmessages.h:181-184`), the message fails `BatchVerifyMessageSigs`, and the sending peer is scored 100 (`net_dkg.cpp:425-437`).
  - Types 3/4 perturb byte 5 of a compressed G2 encoding (`a/dkgsession.cpp:554-561`). That almost always yields an invalid point, which is rejected at **PreVerify** with a ban (`l/dkgsession.cpp:523-532`). Only if it decodes valid does it reach the batch check (type 3) or the quorumSig check (type 4, :609-612).
- **Failure scenario:** the per-lieType tests assert the wrong reason and score. Peer scoring of the liar is part of DK1's live parity, so getting it wrong here propagates.
- **Fix:** give the exact expected stage, reason text and score per lieType. Include the "perturbed bytes still valid" variant as a forced case for types 3/4.

### m4. The PLAN doesn't fully reflect DEC-616, and P-1/P-2 have no rows or deps

- PLAN:597 (LQ4b) still reads "LLMQ-100 corpus cross-check; `dkgsimerror` outcomes vs dashd on regtest (with DK1)". That contradicts DEC-616 #2 (the live comparison moved to DK1) and omits #4 (the 10–12-member capture). Only LQ4a and DK1 were edited in 4d95151ca.
- P-1 (base-sdk sign) and P-2 (O2a-DKG) appear in no PLAN row or dependency column.
- The design's slice table lists LQ4b-1 as depending on LQ4a-2 only. But its **production** code signs `quorumSig = skShare.Sign(commitmentHash, legacy)` (`a/dkgsession.cpp:552`) over `BlsSkValue`. That is P-1 itself; the typed-key bridge is test-only. So P-1 is a hard dependency of LQ4b-1, not a bridge.
- **Fix:** edit PLAN:597's acceptance. Add P-1 (BS11 thread) and P-2 rows, or BACKLOG items with owners. Set LQ4b-1's deps to "LQ4a-2, P-1" and LQ4b-2's to "LQ4b-1, P-2".

### m5. The critical-path claim contradicts the PLAN dependency

- §5 says "the critical path to DK1's start is LQ4a-1 → LQ4a-2 → LQ4b-1". PLAN:702 makes DK1 depend on **LQ4b**, the whole row including LQ4b-2, which waits on P-2 (12–16 h).
- **Fix:** either state DK1 → LQ4b-1 in the PLAN (LQ4b-2's differential then runs beside DK1), or put LQ4b-2 + P-2 on the path. The first matches the design's intent.

### m6. The batch contract needs four properties pinned (this answers check 2)

`process_batch` is equivalent to `ProcessPendingMessageBatch` (`net_dkg.cpp:392-445`) only if all of these hold:
- (a) **Sender identity is per connection, not per proTx.** `nodeIdsAllSame` is `adjacent_find` on `NodeId` over the preverified list (:289-292). The bad-sig set is per NodeId (:426-437). `Sender<P>` equality must be NodeId equality, with `Own` (Core's `-1`, :364) distinct and stable within a batch.
- (b) **Scores have different multiplicities.** Decode and PreVerify-ban scores are per **message** (:405-418), so the same peer can collect several. The bad-sig score is once per **node** (:426-431). `BatchOutcome.misbehaving` should keep that distinction, and say that entries for `Own` are dropped (Core's `PeerMisbehaving(-1, …)` is a no-op).
- (c) **PreVerify runs over the whole batch before any `ReceiveMessage`**, so its "≥ 2 already" checks (`l/dkgsession.cpp:202`, :306, :410, :543) read counts from before the batch. Three messages from one member in one batch all pass PreVerify. Then the preamble records the first, records the second with MarkBadMember and relays it, and drops the third with no relay (`l/dkgsession.cpp:720-751`).
- (d) The preamble uses **set** semantics on the session hash (`member_set.emplace(hash)`, :726). Re-delivering an equal session hash leaves the count at 1 and is processed again. (The same applies to pcommit's unconditional `emplace`, :571-572.)

**Fix:** state (a)–(d) in §3.3 and add T1 rows for (c) and (d).

### m7. The RNG seam should be typed, not a byte stream

- **Where:** §3.2, `rng: &mut dyn CryptoRngCore` for "coefficients, IES ephemeral+IV, MakeNewKey lies, sim draws". P-2 promises recorded randomness.
- **Core** draws through different primitives:
  - `MakeNewKey` (GetStrongRandBytes + validity loop);
  - `InitEncrypt`;
  - `GetRandBool` → `GetRand(10^8+1)` (`random.cpp:653-661`, a rejection-sampled range);
  - `GetRand<int>(5)`.
- **Failure scenario:** to replay a Core script through a byte stream, Rust must re-implement Core's byte consumption for each primitive exactly. Otherwise T6 diverges on the first sim draw. Also `FinalizeSingleCommitment` draws `sk1.MakeNewKey()` (`a/dkgsession.cpp:704-705`) whose value can't be seen but which still consumes randomness. That matters for any script-ordered replay.
- **Fix:**
  - Define `DkgRng` with `new_key()`, `ies_seed(n)`, `rand_bool(rate)` and `rand_range(n)`. Make the production implementation Core-faithful over a CSPRNG, and the replay implementation read P-2's recorded decisions.
  - State the draw order per start function: Contribute is t keys → CONTRIBUTION_OMIT → InitEncrypt → per-member CONTRIBUTION_LIE (and a key if it fires). Justify is per justified member JUSTIFY_LIE → JUSTIFY_OMIT. Commit is COMMIT_OMIT → COMMIT_LIE → lieType → key if lieType 0. Single-member is sk1.

### m8. The DEC-616 #4 capture parameters

- `-llmqtestparams=S:T` sets `minSize = threshold = dkgBadVotesThreshold = T` (`chainparams.cpp:1019-1027`). So "10:6" gives minSize 6, not a devnet-like 7.
- The harness's `LlmqParams` must use the effective overridden values, and PROVENANCE must record them. `SessionInit.params` from `dash_params` defaults would silently mismatch.
- "(or devnet `llmq_devnet` 12/7/6)" isn't available on regtest: it needs a devnet chain (`-llmqdevnetparams` is devnet-only, `chainparams.cpp:1373`, :1431).
- **Fix:** name the regtest override and record the effective params.

### m9. The simplepose capture and the `ConnSnapshot` contract

- Core logs only the **negative** outcomes of the §2.6 fold (`a/dkgsession.cpp:265-287`). The capture can't recover the fold inputs from debug.log, so "Core's connman facts logged" needs a capture-build log patch at `VerifyConnectionAndMinProtoVersions` entry.
- `connman.ForEachNode` visits only fully connected nodes (`net.h:1423-1426` → `FullyConnectedOnly`; `net.cpp:4852-4855`: `fSuccessfullyConnected && !fDisconnect`). The `ConnSnapshot` contract in §3.2 must say DK1 includes only those, in `m_nodes` order.
- **Failure scenario:** DK1 includes a connecting or disconnecting node whose `nVersion` is still 0 or stale. Its "first node seen" version then wins (§2.6, "version is the first node's"), and a good member is marked bad.
- **Fix:** add the filter to the contract, and the log patch to the capture recipe.

### m10. Note GETDATA phase windows and rc.4's null-session window for DK1

- §3.3's `get_*` accessors are served by Core only inside phase windows: contrib in [Initialized, Contribute], complaint in [Contribute, Complain], justification in [Complain, Justify], pcommit in [Justify, Commit] (`l/dkgsessionmgr.cpp:57-123`).
- rc.4 also nulls the current session before `Init` and leaves it null if Init fails (`a/dkgsessionhandler.cpp:94-105`). Old-session messages then stop being served.
- Both belong to DK1, but §3.3 is the API DK1 will freeze against.
- **Fix:** add one line each to §3.3 / §3.4.

### m11. Sessions where the base predates V19 but the process-wide flag is basic

- The design models the session flag (`m_use_legacy_bls`, `a/dkgsession.cpp:34`) and the process-wide flag separately, which is correct. But no test has them differ within one session: base before V19, tip after V19 (decode and hash basic, sign and `ToBytes` legacy).
- On regtest and devnet chains this happens at V19 activation. BS12's sessions are per scheme, not straddling.
- **Fix:** add a T4 row (and a T6 script) with a pre-V19 base and post-V19 tip, or state that it's outside the v24 profile and assert the precondition in `init`.

---

## Nit

1. §4.3 truth table: "including clearing an earlier bad flag". `badConnection` is set only inside `VerifyConnectionAndMinProtoVersions` (`a/dkgsession.cpp:265-286`; nowhere else in rc.4), which runs once per session. The row is unreachable unless `verify_and_complain` can be called twice, which it should not be. Drop the row, or make the method single-shot.
2. Truth table: add the boundary rows 70241 (bad) and 70242 (good). The design's 70240 is v23.1.8's `PROTOCOL_VERSION` (`v23.1.8:version.h:14`), not the boundary.
3. T4 lists "7 members", but no shipped type has size 7 (`llmq_devnet` is 12/7/6). Did you mean devnet with 5 dropouts?
4. The `DkgEvent` "Core's text" must strip the cxxtimer fields (`time=%d`, `time1..3`, `totalTime`), which are wall-clock values.
5. `SessionInit` carries both `use_legacy_bls` and `v19_after_base`, which are always negations of each other (`a/dkgsession.cpp:34`, :633). Keep one.
6. `DkgEnv.sim` is described as "a snapshot at this step", while §2.9 says the rate is read at each decision. Within one call these are equivalent; say so.
7. `phase_for_tip` should take the index > 0 rotation-at-tip gate (`a/dkgsessionhandler.cpp:49-51`) and the unvalidated-snapshot gate (:45) as inputs, so LQ4a-1's acceptance sweep covers them.
8. PLAN:702 (DK1, edited in this commit) still says "intake rules per profile" and "both pins", which DEC-82 made stale.
9. The `DkgStore` writes have no error path. State that DK1's implementation fail-stops on a write error, as Core's dbwrapper throws.
10. P-1 says "the sign of an invalid key (Core's behaviour) stays untested". No DKG path signs with an invalid key (`skShare.IsValid()` is checked first, `a/dkgsession.cpp:519`), so this is low stakes and can't block anything.

---

## DEC-616 rulings

None contradicts Core.
- #7 (batched verification is pure) matches Core's placement: `BatchVerifyMessageSigs` reads only session state.
- #6 (fail-stop) matches `assert(false)` (`a/dkgsession.cpp:734`).
- #1 matches DEC-82 and `MIN_MASTERNODE_PROTO_VERSION = 70242` (`version.h:23`).
- #2 and #4 aren't reflected in PLAN:597 (m4).
- #8 needs the non-member clarification (m2).
- #5 is plausible: my estimate for 400 × `VerifyContributionShares` at t = 240 is about 5 CPU-min, consistent with the design's 6–7.

## Citations spot-checked (rc.4), all correct unless listed above

- `l/net_dkg.cpp`: 186-211, 217-321 (235-252, 261-271, 288-298, 302-319), 323-354, 357-365, 370-389, 392-445, 492-666, 617-619, 772-785, 833-936.
- `l/dkgsessionhandler.cpp:29-56`.
- `a/dkgsessionhandler.cpp`: 43-70, 80-109, 111-147, 173-235.
- `a/dkgsession.cpp`: 34, 40-113, 120-183, 185-224, 226-290, 292-332, 334-422, 424-579, 581-688, 690-743, 745-749.
- `l/dkgsession.cpp`: 42-63, 158-211, 213-265, 268-316, 318-354, 357-420, 422-491, 494-553, 555-630, 678-685, 688-755.
- `l/dkgsessionmgr.cpp`: 41-55, 125-192.
- `l/options.cpp`: 26-47.
- `l/commitment.cpp`: 312-323.
- `l/blockprocessor.cpp:840`.
- `rpc/quorums.cpp`: 870-899.
- `random.cpp`: 653-661.
- `version.h:23`.
- `utils.cpp:778`.
- The rc.3 → rc.4 delta claim (header): confirmed by `git diff v24.0.0-rc.3 v24.0.0-rc.4 -- src/llmq src/active`. That diff touches only `a/dkgsessionhandler.{h,cpp}` and one line of `l/net_dkg.cpp`, plus `net_signing.cpp`, which isn't DKG.
- The rdc/bsdk citations I opened (`members.rs:222`, `commitment.rs:54/:67/:235-258`, `pool.rs:50`, `conn.rs:491-499/:531-534/:742-743`, `meta.rs:415/:422`, `worker.rs:28-194`, `threshold.rs:74`) match.

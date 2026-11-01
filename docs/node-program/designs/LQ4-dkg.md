# LQ4 — pure DKG in `dash-llmq` (rows LQ4a + LQ4b)

**Status:** APPROVED rev1.1, 2026-10-28 (Sol r2 APPROVE-WITH-NITS, `reviews/LQ4-design-r2-sol.md`; the manager folded m1–m3 and n1 and applied the §7 PLAN text). rev1 answered Opus-h r1 (`reviews/LQ4-design-r1-opus.md`, CHANGES-REQUIRED). Rulings: DEC-616 (Q1–Q10), DEC-619 (Q11–Q13).
**Rows:** PLAN.md:596 (LQ4a), :597 (LQ4b). **Requirements:** FR-LLMQ-03, FR-LLMQ-04. **Rulings applied:** DEC-82, DEC-616, DEC-619.
**Parity reference:** Dash Core `v24.0.0-rc.4` (49caf372bb). Unless a citation says otherwise it is `rc.4:<path>:<line>`; `a/` means `src/active/` and `l/` means `src/llmq/`.
- Core rc.3 → rc.4 changes only the session *handler* (`#7824`: `a/dkgsessionhandler.{h,cpp}` and one line of `l/net_dkg.cpp`). The session math and every message are the same at both tags, so BS12's rc.3 transcript (§4.1) is valid rc.4 evidence. Opus-h r1 confirmed this by diff.

**Code references:** rust-dashcore `node/integration` at 0729966a0 (`rdc:`), and the base-sdk fork pin `f27bc4f5` (`bsdk:`), which is the root `[patch]` rev (`Cargo.toml:58-64` on integration).

---

## Changes since rev0

Every Opus-h r1 finding is accepted. One part of m3 is refined, as noted in its row. Two items (X1, X2) are new; I found them while fixing M1 and m3.

| Finding | Fix in rev1 | Where |
|---|---|---|
| **M1** members access in `verified_contributions` | `members` removed from `DkgStore::verified_contributions`. DK1's implementation makes Core's `GetAllQuorumMembers` facade call on every invocation, before any row read (`l/dkgsessionmgr.cpp:147`). Relay members now come from a `RelayTopology` callback that `init` calls only for members, so that access also sits where Core's does (`l/dkgsession.cpp:137`). New "members access points" table. T6 compares the access count and position per step. | §3.2, §3.4.1, §4.3 T6 |
| **X1** (new, found while fixing M1) | Core's `Verify(…, checkSigs = true)` reads members **twice**: at `l/commitment.cpp:177`, then again in `VerifySignatureAsync` at :53 (called at :201). dash-evo's `verify_commitment(.., Some(SigPath::Sync))` reads once (SB8 D2, `rdc:dash-evo/src/llmq/commitment.rs:320`). Finalize goes through it, so the DKG access stream would be one access short per finalized group. New prerequisite **P-4**: `SigPath::Sync` makes the second read in Core's place. This also affects LQ2's `qfcommit` path (`l/blockprocessor.cpp:195`). | §1.2, §3.4.1, Q11 |
| **M2** no Core evidence ≥ 32 members | T6 has a required script family with ≥ 33 injected members (33, 40, 65) through P-2's members hook. It includes orders where the 32nd pending contribution falls at each position 1–8 of a batch, and a chunk fallback inside one 32-wide call. New T1 rows: the mid-batch trigger, store writes during Contribute, and the remainder at Complain. The Core capture **cannot** go to ≥ 33 MNs on Core's stock test framework, which caps a test at 20 nodes (`test/functional/test_framework/util.py:324`, asserted at `test_framework.py:951` and `util.py:362`). A capture patch that raises the cap is plausible but untried, so T6 carries M2. DEC-619 Q13: no capture spike. | §2.4, §4.1, §4.3 T1/T6 |
| **M3** LQ4a-2's byte-exact claim can't pass | Acceptance restated per message class (§4.3 T3, table T3-C). In short: everything deterministic is byte-exact; every member's own qcontrib is adopted from the capture. In the correct-justification scenario, the shares the erring member reveals come from the captured qjustify. The justify-lie keys are replayed through the typed `DkgRng` (m7), so that qjustify is byte-exact too. **Commit-lie needs no adoption**: Core's lied pcommit never leaves the liar (X2), so the check is equivalence over all five forced lieTypes against the observable capture. | §4.3 T3, §5 |
| **X2** (new, found while fixing m3) | For every lieType, the liar's **own** loopback rejects its lied pcommit (`EnqueueOwn` → `ProcessPendingMessageBatch`, `l/net_dkg.cpp:357-365`, :392-445). A score on sender `-1` is a no-op, nothing is relayed, and GETDATA serves only valid pcommits (`l/dkgsession.cpp:678-685`). The capture agrees: BS12's two commit-scenario sessions each hold only 2 pcommits. | §2.8, §4.4 |
| m1 mining window | Corrected: Finalize's start equals `dkgMiningWindowStart` for non-rotated types; rotated types add `signingActiveQuorumCount` (`l/params.h:233`, :311, :390). | §2.1 |
| m2 observer vs non-member | `my_pro_tx` → `active_pro_tx` (AM1's proTxHash, which may be null as in Core). `init` derives membership as Core does (`l/dkgsession.cpp:106-116`). Observers run no session (`l/net_dkg.cpp:511-518`, :739), so this API is never called for them. Relay members are requested only for members. A non-member replay from the rotation capture is added to T3. | §3.2, §4.3 T3 |
| m3 commit-lie stages | Per-lieType stage, Core log text and score are now given both at the liar's own loopback and at a receiver of a lie from a modified sender. Added a forced "perturbed bytes still decode" variant for lieTypes 3 and 4. **Refinement:** the reviewer's stages are right, but "the sending peer is scored 100" only happens when a *modified* sender transmits the lie. In Core's own `commit-lie` no peer receives it (X2). | §4.4 |
| m4 PLAN after DEC-616 | P-1 is a hard dependency of LQ4b-1 (production `quorumSig = skShare.Sign`, `a/dkgsession.cpp:552`), not a bridge. LQ4b-2 depends on P-2. Exact PLAN text for the manager is in §7. | §5, §7 |
| m5 critical path | DK1 should depend on **LQ4b-1**, so LQ4b-2's differential and fuzz run beside DK1. PLAN text in §7. | §5, §7 |
| m6 batch contract | Four properties pinned: per-connection sender identity with a distinct `Own`, score multiplicities, PreVerify over the whole batch before any Receive, and set semantics on the session hash. New T1 rows for the last two. | §2.3, §3.3 |
| m7 typed RNG | `rng: &mut dyn CryptoRngCore` → `rng: &mut dyn DkgRng` with `new_key`, `ies_seed`, `rand_bool`, `rand_range` and draw tags. The draw order per start function is stated, including the unobservable single-member `sk1`. | §2.9, §3.2 |
| m8 capture params | The 12-member capture is `-llmqtestparams=12:7` (effective size 12, `minSize = threshold = dkgBadVotesThreshold = 7`, `chainparams.cpp:1019-1027`). PROVENANCE records the effective values and the harness builds `LlmqParams` from them. Devnet types aren't used. | §4.1 |
| m9 ConnSnapshot | The contract includes only fully connected nodes (`fSuccessfullyConnected && !fDisconnect`, `net.h:1423-1426` → `net.cpp:4852-4855`), in `m_nodes` order. The simplepose capture adds a log patch at the entry of `VerifyConnectionAndMinProtoVersions`. | §2.6, §3.2, §4.1 |
| m10 GETDATA windows, null session | One line each in §3.3 and §3.4. | §3.3, §3.4 |
| m11 pre-V19 base, post-V19 tip | Added a T4 row and a T6 script (regtest `-testactivationheight=v19@h`, with h inside the session's phases). `env.bls` is passed per step. | §4.3 |
| Check 4 (SW-UPSTREAM) | Recorded: `wire` builds every bitset through the validating constructor (`b5ffc07c`), never from raw bytes. After the fork rebase, T1's decode/structure rows and T3 are the regression gate. | §1.3 |
| n1 | The start functions are single-shot and run in phase order; a second or out-of-order call returns `DkgFault::PhaseOrder`. The truth-table row that cleared a flag set earlier is dropped. | §3.3, §4.3 |
| n2 | Added boundary rows 70241 (bad) and 70242 (good). 70240 stays as the v23.1.8-shaped row. | §4.3 |
| n3 | "7 members" → `llmq_devnet` (12) with 5 dropouts, which leaves exactly `minSize` = 7. | §4.3 T4 |
| n4 | `DkgEvent` text drops the cxxtimer fields (`time=`, `time1..3=`, `totalTime=`). | §3.3 |
| n5 | `use_legacy_bls` dropped; derived as `!v19_after_base`. | §3.2 |
| n6 | `DkgEnv.sim`: one call is one decision batch, so a snapshot per call equals Core's read per decision; stated. | §3.2 |
| n7 | `phase_for_tip` takes the snapshot-unvalidated gate (`a/dkgsessionhandler.cpp:45`) and the rotation-at-tip gate for index > 0 (:49-51) as inputs. | §3.3 |
| n8 | DK1's "per profile" and "both pins": PLAN text in §7. | §7 |
| n9 | DK1's `DkgStore` fail-stops on a read or write error, as Core's dbwrapper throws. | §3.2 |
| n10 | The "invalid-key sign stays untested" sentence is dropped. | §1.2 |

---

## 0. Scope, non-goals and rulings this design assumes

**In scope.** This design covers a pure, sans-io port of the following, in a new crate `dash-llmq` (layer **Pn**, SB0 §2.3; A1:51, A1:572), module `dash_llmq::dkg`:
- Core's per-session DKG logic (`CDKGSession` + `ActiveDKGSession`);
- the per-batch message pipeline that decides which received messages a session accepts (`ProcessPendingMessageBatch` + `BatchVerifyMessageSigs`; DEC-616 #7);
- the phase and sleep arithmetic, as pure functions;
- the bad-connection fold;
- commitment building, finalization and the single-member path;
- the seven simulated-error hooks.

**Not in scope.** DK1 (PLAN:702) owns the following in `dashd-quorums`:
- the intake gates before the queue: spork 17, the MNAUTH gate, the size cap, the framing walk, the GETDATA-response gate and the per-proTx quota (`l/net_dkg.cpp:492-666`, `l/dkgsessionhandler.cpp:29-56`);
- the phase-handler thread with its waits, sleeps and aborts (`a/dkgsessionhandler.cpp:111-255`, `l/net_dkg.cpp:772-936`);
- relay to peers, and GETDATA serving with Core's phase windows;
- dkgdb storage, its 60 s contributions cache and its cleanup (`l/dkgsessionmgr.cpp:125-282`);
- the `quorum dkgstatus` / `dkginfo` / `dkgsimerror` RPCs.

Elsewhere:
- Probe and quorum connections belong to P7.
- Quorum topology (the relay-member function) belongs to LQ5 (A1:572); LQ4 calls it through a trait (§3.2).
- The mineable pool and `qfcommit` intake belong to CH7b-5 and LQ2 (CH7 D14; `rdc:dash-dml/src/blockprocessor/pool.rs:36`).

**Rulings in force:**
- **R-1, DEC-82 + DEC-616 #1 (v24 only).** Bad-connection criteria are v24's (`a/dkgsession.cpp:226-290`; `version.h:23`, `MIN_MASTERNODE_PROTO_VERSION = 70242`).
- **R-2, DEC-79 / SB0 L4.** `dash-llmq` makes only serial, pool-free BLS calls, has no clock, threads, globals, HashMap iteration or I/O, and carries `#![forbid(unsafe_code)]`. Parallelism is injected (§3.6).
- **R-3, DEC-616 #6.** Core's single-member `assert(false)` becomes `DkgFault`, and DK1 makes it a fail-stop.

---

## 1. Dependency check

### 1.1 Verdicts

| Dep | Where it landed | What LQ4 uses (file:line at the pin) | Verdict |
|---|---|---|---|
| **LQ1b** | Merged in train 14 (`c36a3d1b8`, fix `e3341f05e`) | `members_uncached(ctx, src, rows, llmq_type, base)` (`rdc:dash-evo/src/llmq/members.rs:222`): the rotated and non-rotated routes in Core's member order. `MnEntryView::pro_tx_hash` (`view.rs:16`). `OperatorKeyView` (`commitment.rs:37-44`). `is_quorum_rotation_enabled` (`params.rs:93`). The `QuorumMemberSource` trait that `verify_commitment` takes. Node paths reach members through CH7b's facade (LQ1 §3.7.4), which DK1 calls at Core's access points (§3.4.1) | **Met** |
| **BS9a** | Fork merge `a33623f` into `node/base-sdk`, carried at `f27bc4f5` | `bsdk:pkgs/p2p_core/src/msg/dkg.rs`: trait `DkgMessage` (:64); `dkg_inventory_hash` (:136); `DkgContribution` (:259, `sign_hash` :286); `DkgComplaint` (:353); `DkgRevealedShare` (:421); `DkgJustification` (:436); `DkgPrematureCommitment` (:510, `sign_hash` = `BuildCommitmentHash` :545); `dkg_quorum` (:587). The trait doc (:32-63) gives Core's six-step intake order, which DK1 uses | **Met.** The framing walk already enforces the type-level `CheckDKGMessageStructure` (`l/net_dkg.cpp:186-211`). LQ4 checks it again after decode, as Core does (§2.3 step 1) |
| **BS11b** | rdc `de1ef98cd → 4f02477a7`, fork `518b7896`, merged T38; `node/base-sdk` fast-forwarded to `f27bc4f5` (DEC-511) | `bsdk:pkgs/pkc/src/bls/core/`. **worker.rs:** `verify_vector` (:28), `verify_verification_vectors` (:38), `aggregate_*` (:72-82), `build_quorum_verification_vector` (:100), `build_pub_key_share` (:113), `verify_contribution_share` (:128), `verify_contribution_shares` (:149: Core's aggregated 8-chunks with the per-chunk fallback), `generate_contributions` (:194; a test cross-check only, §2.9). **ies.rs:** `encrypt_with(ephemeral_sk, iv_seed, recipients)` (:92), `decrypt_secret_key` (:137). **threshold.rs:** `secret_key_share` (:28), `public_key_share` (:51), `recover_signature` (:74). **aggregate.rs:** `aggregate_signatures_secure` (:99). **mod.rs:** `verify_insecure` (:135), `verify_insecure_aggregated` (:150), `public_key` (:317) | **Met, with one gap (P-1)**: no Core-style sign over `BlsSkValue` |
| **BS12** | Rode train 14 | Serial verify (DEC-79). Corpora: `bsdk:pkgs/pkc/corpus/bls_llmq_100.json5` (`PROVENANCE.md:153-179`: a 3-member, t = 2 `llmq_test` DKG per scheme, reproduced by Core rc.3 through the oracle; sha256 `72bec3e5…`). `bsdk:pkgs/pkc/corpus/bls_oracle/dkg.json5` (`bls_oracle/PROVENANCE.md:123-134`: `feature_llmq_dkgerrors.py` on rc.3 with `-capturemessages`; 8 type-100 sessions with members and operator keys, every relayed qcontrib/qcomplaint/qjustify/qpcommit, the final, per-recipient shares with `fits`, revealed keys with `fits`, and each member's aggregated share; sha256 `e5d2163f…`) | **Met** |
| **O17** | Done (DEC-41), then reduced to v24 by DEC-82 | Row DKG-01 (`O17-pin-delta.md:296`; §4 row 17, :171): the criteria apply to MN-flagged connections after the version test (#7755). The P2a part is in `dash-p2p`: `Peer::version_services` (`rdc:dash-p2p/src/conn.rs:491-494`), `version_relay_txs` (:496-499), `masternode_connection` (:531-534), stored at VERSION (:742-743), and the MNAUTH proTxHash slot (`conn.rs:276`). SP1's `outbound_failed_too_many_times` / `is_platform_banned` (`rdc:dashd-masternode/src/meta.rs:415`, :422) | **Met** |

**Other inputs that already exist:**
- dash-evo's `verify_commitment(ctx, members, qc, base, Some(SigPath::Sync), effects)` (`rdc:dash-evo/src/llmq/commitment.rs:258`), which Finalize calls. It makes one members access, but Core's path makes two (P-4).
- `build_commitment_hash` (:67) and `commitment_version` (:54).
- `LlmqCommitment = dash_primitives::Commitment` (`dash-evo/src/llmq/mod.rs:70`).
- `MineablePool::add` (`dash-dml/src/blockprocessor/pool.rs:50`).

### 1.2 Named prerequisites

- **P-1 (base-sdk, small; BS11 thread, DEC-616 #9): `pkc::bls::core::sign(sk: &BlsSkValue, hash: &[u8; 32], flag: BlsSchemeKind) -> BlsSigValue`**, Core's `CBLSSecretKey::Sign(hash, specificLegacyScheme)`, for both flags.
  - Production use: `quorumSig = skShare.Sign(commitmentHash, legacy)` (`a/dkgsession.cpp:552`). This makes P-1 a **hard dependency of LQ4b-1** (m4).
  - Test use: the in-process operator-key stand-in (for `m_mn_activeman.Sign` at :103, :322, :412, :551) and the single-member path (:724, :726).
  - LQ4a-1 and LQ4a-2 make no production `BlsSkValue` signature. Their tests may use `BlsSecretKey::<BlsScIetf>::sign_with` (`secret_ops.rs:165`) until P-1 lands.
  - Evidence: the `bls_oracle/sign.json5` rows under both flags.
- **P-2 (Core oracle shim, medium): O2a-DKG** (§4.2; named, not designed here). It builds on O2a-LQ1's context (headers, MN list, its record-only MEMBERS trace). It needs two test seams in the patched build:
  - a **members hook** at `utils::GetAllQuorumMembers` entry that returns injected members for (type, base) and counts every call (M1, M2);
  - **recorded decisions** at Core's four randomness points (m7).

  LQ4b-2's `diff_dkg_session` needs P-2. LQ4a and LQ4b-1 acceptance do not.
- **P-3: the `dkgsimerror` cycle, resolved by DEC-616 #2.** The live regtest comparison is DK1's acceptance.
- **P-4 (dash-evo, small; the SB8/LQ2 owner): Core's second members read in `Verify(…, checkSigs = true)`.**
  - Core: `Verify` reads at `l/commitment.cpp:177` (after the structural checks), then calls `VerifySignatureAsync` (:200-204). That function repeats the parentless-base check (:48-51) and reads members again (:53), with the single-member empty-members exit (:54-57), before any signature check.
  - Today: `verify_commitment` with `Some(SigPath::Sync)` reads once (`commitment.rs:320`) and passes that list to `verify_signatures` (:329-330).
  - Fix: on `SigPath::Sync`, make a second `members.members(..)` read in `VerifySignatureAsync`'s place, with its checks. `SigPath::Mined` is unchanged, since `queue_mined_signatures` already reads separately (LQ2 D23(6)).
  - Without P-4, the DKG access stream (§3.4.1) is one read short per finalized group, and LQ2's `qfcommit` path (`l/blockprocessor.cpp:195`) is short too. DEC-619 assigns it to the LQ4 implementer as BL-475, a pre-slice before LQ4b-1.

### 1.3 SW-UPSTREAM sync: APIs whose shape may change

1. **base-sdk #89** (`96ab5748`). It is on upstream `develop`, but not in `f27bc4f5` or in upstream rust-dashcore's pin `addedfa`. It privatizes the DKG message fields and `dash_primitives::Commitment` (getters, `impl_new!`). It also makes `DynBitset` fields private behind a validating constructor (`ecb3905d`, `b5ffc07c`).
   - **Containment:** all construction and field access lives in `dash_llmq::dkg::wire` (≈ 150 lines). Every bitset LQ4 builds (validMembers, badMembers, complainForMembers, signers) goes through the validating constructor, never raw bytes. After `ecb3905d`, `encode` no longer masks padding and `count_ones` counts it. Decode still rejects non-zero padding, as Core's `ReadFixedBitSet` does (`serialize.h:470-474`), so intake verdicts don't change.
   - **Regression gate after the rebase:** T1's decode and structure rows, plus T3.
2. **Fork-only modules** `pkc::bls::core::{worker, ies, threshold, aggregate, batch}` and `p2p_core::msg::dkg`. If they are upstreamed under kwvg's no-C++-names policy, they may be renamed. LQ4 names them only in `dkg::wire` and `dkg::bls`.
3. **rust-dashcore upstream `962a46219` / `70a125698`** move the root `[patch]` pin again. LQ4 uses no `dash/` crate type.
4. **Upstream `f35115b27`** (LLMQ params in `dash/`). LQ4 reads `dash_params::LlmqParams` (`bsdk:pkgs/params/src/llmq.rs:22`), so there is no impact.

---

## 2. Core's DKG as a state machine (rc.4)

### 2.1 Phases and height timing

`QuorumPhase` (`l/dkgsessionhandler.h:33-41`) runs Initialized = 1, Contribute, Complain, Justify, Commit, Finalize, Idle = 7. There is one handler per (type, quorumIndex); indices above 0 exist only for rotated types.

`CDKGSessionManager::UpdatedBlockTip` (`l/dkgsessionmgr.cpp:41-55`) skips IBD, pre-DIP3 tips and spork 17 off. Otherwise the handler (`a/dkgsessionhandler.cpp:43-70`) does this:

```
if snapshot active and unvalidated: return                         // :45
if quorumIndex > 0 and !IsQuorumRotationEnabled(params, tip): return  // :49-51
stage      = (h − quorumIndex) mod dkgInterval                       // C++ % on int
base       = ancestor(h − stage); quorumHash = base.hash               // updated on every tip
newPhase   = stage mod dkgPhaseBlocks == 0
phaseInt   = stage / dkgPhaseBlocks + 1
if newPhase && 1 ≤ phaseInt ≤ 7: phase = phaseInt                      // otherwise phase is unchanged
```

So, with phase blocks `pb`, the phases start at stage `0, pb, …, 6pb`. Finalize starts at `5pb`.
- For **non-rotated** types, `5pb` equals `dkgMiningWindowStart`.
- **Rotated** types set the window start to `signingActiveQuorumCount + 5pb` (`l/params.h:233`, :311, :390). For example, `llmq_test_dip0024` has window 12 against 5pb = 10, and `llmq_60_75` has 42 against 10.
- A stage beyond `6pb` leaves Idle in place.

**Round driver** (`l/net_dkg.cpp:833-936`, DK1):
1. Wait for Initialized. Clear every pending queue (:840).
2. `InitNewQuorum(base)` (`a/dkgsessionhandler.cpp:80-109`). It refuses while a snapshot is unvalidated or pre-DIP3. **rc.4 nulls the current session before `Init` and leaves it null if `Init` fails** (:94-105), so messages from the old session stop being served.
3. **Single-member types:** `FinalizeSingleCommitment` → `AddMineableCommitment` → relay (:855-865).
4. Ensure quorum connections; members also get probe connections (P7).
5. Four `HandlePhase`s with sleep factors 0.05, 0.05, 0.05 and 0.1. Each one:
   - sleeps, processing that phase's queue while waiting;
   - calls the phase's start function, whose own message goes through `EnqueueOwn` (:357-365);
   - `MarkPhaseAdvanced`;
   - waits for the next phase, draining that phase's queue in pops of up to 8 (:392-445).
6. At Finalize: `FinalizeCommitments` → `AddMineableCommitment` for each → relay (:929-935).

**Aborts.** An unexpected phase or a quorum-hash change throws (`a/dkgsessionhandler.cpp:111-147`, :224-228), and `MarkAborted` follows.

**Sleep** (`a/dkgsessionhandler.cpp:173-235`). It applies only to members, and not when `MineBlocksOnDemand` is set:

```
sleep = (pb − 1) · nPowTargetSpacing · 1000 / size · factor · myIdx   (ms)
```

The sleep ends early when blocks arrive faster than expected.

### 2.2 Session state (`l/dkgsession.h:39-60, 136-168`)

**Per member** (`CDKGMember`):
- `idx` and `id = CBLSId(proTxHash)`;
- message-hash sets: `contributions`, `complaints`, `justifications`, `prematureCommitments`, `badMemberVotes`, `complaintsFromOthers`;
- flags: `bad`, `badConnection`, `weComplain`, `someoneComplain`.

**Per session:**
- `members` in LQ1's order, and `membersMap`;
- `relayMembers`, members only;
- own `vvecContribution` and `m_sk_contributions`, which hold the **true** shares; a lie only replaces the copy being encrypted (`a/dkgsession.cpp:88-93`);
- `receivedVvecs`, `receivedSkContributions` and `vecEncryptedContributions`;
- `myProTxHash`, `myId` and `myIdx`, all set only when the active proTx is a member (`l/dkgsession.cpp:106-116`);
- the maps by message hash: `contributions`, `complaints`, `justifications` and `prematureCommitments` (`std::map<uint256,…>`, memcmp order);
- `pendingContributionVerifications`;
- `validCommitments`.

**Two properties that carry over to Rust:**
- **Order matters only where Core's containers fix it.** The hash-ordered `prematureCommitments` and Finalize's `std::map<std::vector<bool>, …>` (false < true) fix the group order and `first` (`a/dkgsession.cpp:589-611`). Every `Uint256HashSet` use is size, membership, erase, or `.begin()` on a set of size 1. Rust uses `BTreeMap<[u8; 32], _>` and `BTreeMap<Vec<bool>, _>`, and never iterates a `HashMap`.
- **A message's session hash is `::SerializeHash` of the eager-decoded object** (`l/dkgsession.cpp:564`, :725), not the queue's sha256d of the raw payload (`l/net_dkg.cpp:617-619`). The two differ when eager normalization zeroes bytes (BS9a doc, `dkg.rs:117-124`). Invs and GETDATA use the session hash; the queue's seen-set uses the raw hash.

### 2.3 Message validation order (every received message, own included)

Each pop of up to 8 messages from one phase's queue goes through these steps (`l/net_dkg.cpp:392-445`).

1. **Decode + structure** (`DeserializeCheckedDKGMessage`, :370-389). Decode the whole payload under the process-wide flag, require nothing left over, then run `CheckDKGMessageStructure` (:186-211). A failure scores the sender 100, **per message** (:405-408).
2. **`PreVerifyMessage`**, for **every** message in the batch, **before any `ReceiveMessage`**. False with `retBan` scores 100 per message; false without it drops silently (:410-418). Its "≥ 2 already" checks (`l/dkgsession.cpp:202`, :306, :410, :543) therefore read the counts from before the batch.
3. **`BatchVerifyMessageSigs`** (:217-321):
   - Skip any message with an unknown member or `!sig.IsValid()`, and mark its node bad.
   - A repeated sign hash forces per-message verification.
   - Otherwise run the aggregate `VerifyInsecureAggregated`. If it fails and every message came from one `NodeId` (`adjacent_find` on NodeId, :289-292), that node is bad. If it fails otherwise, verify each message singly.
   - Each bad `NodeId` scores 100 **once** (:426-431). **All** of that node's messages in the batch are then skipped (:435-437).
4. **`ReceiveMessage`** for each survivor in order. A returned `CInv` is relayed to the relay members and qwatch peers (`RelayInvToParticipants`, :323-354; a non-member has no relay members, so only qwatch peers).

**PreVerify, in Core's check order (each rejection also logs Core's text):**

| Msg | Checks in order (`l/dkgsession.cpp`) |
|---|---|
| qcontrib (:158-211) | wrong quorum → drop; non-member → ban; `!sig.IsValid()` → ban; `blobs.size() ≠ members.size()` → ban; `vvec.size() ≠ threshold` → ban; `!VerifyVerificationVector` → ban; ≥ 2 contributions → drop |
| qcomplaint (:268-316) | wrong quorum → drop; non-member → ban; invalid sig → ban; `badMembers.size() ≠ size` → ban; `complainForMembers.size() ≠ size` → ban; ≥ 2 → drop |
| qjustify (:357-420) | wrong quorum → drop; non-member → ban; invalid sig → ban; empty → ban; per entry: index ≥ members → ban, duplicate index → ban, `!sk.IsValid()` → ban; ≥ 2 → drop |
| qpcommit (:494-553) | wrong quorum → drop; non-member → ban; `validMembers.size() ≠ size` → ban; count < minSize → ban; `!sig.IsValid()` ("invalid membersSig") → ban; `!quorumSig.IsValid()` ("invalid quorumSig") → ban; any bit ≥ members.size() → ban; ≥ 2 → drop |

**`ReceiveMessagePreamble`** (contrib, complaint and justify; :688-755):
1. A non-member returns nothing.
2. If the member already holds ≥ 2 of this type, return nothing (no relay).
3. Otherwise `member_set.emplace(hash)` (**set semantics**: an equal session hash leaves the count unchanged, :726), record the message and set the debug bit.
4. If this is the member's **second** message, `MarkBadMember` and relay it without processing (:745-751). All other recorded messages are relayed.

Consequence of steps 2 and 3: three distinct messages from one member in one batch all pass PreVerify. Then the first is processed, the second is recorded, marks the member bad and is relayed, and the third is dropped with no relay. A re-delivered equal session hash (the raw bytes differ, so the queue lets it through) is processed again with the count unchanged. qpcommit has no preamble and emplaces unconditionally (:571-572).

### 2.4 Contribute (phase 2)

**`Contribute`** (`a/dkgsession.cpp:40-61`): `GenerateContributions(threshold, memberIds)`. Core first draws `threshold` coefficients with `MakeNewKey` (`bls/bls_worker.cpp:78-86`), then computes `vvec[j] = sk_j·G1` and each id's share as a polynomial evaluation (:87-113).

**`SendContributions`** (:63-113):
1. `CONTRIBUTION_OMIT` first (:71).
2. `InitEncrypt(n)` (:84).
3. For each member in index order: if `i ≠ myIdx` and `CONTRIBUTION_LIE`, encrypt `MakeNewKey()` instead of the share (:88-93). Encrypt with the recipient's operator key; a failure means no message.
4. Sign `GetSignHash` with the operator key, using the legacy scheme iff V19 isn't active after base (:34, :103).

**`ReceiveMessage(qcontrib)`** (`l/dkgsession.cpp:213-265`):
1. Preamble; then `receivedVvecs[i] = vvec`.
2. A non-member stops here and relays.
3. `WriteVerifiedVvecContribution` (:234).
4. Decrypt our blob. A failure → `weComplain`; **else if** `idx ≠ myIdx` and `COMPLAIN_LIE` → `weComplain`. If we complain, relay and return.
5. Otherwise store the share and the encrypted container, and **push `idx`** (own contributions included) onto `pendingContributionVerifications`.
6. **When the pending list reaches 32, run `VerifyPendingContributions` immediately, inside this `ReceiveMessage`** (:257-263). That is in the middle of the batch, before the next survivor's `ReceiveMessage`.

**`VerifyPendingContributions`** (`a/dkgsession.cpp:120-183`):
1. Skip members that are `bad || weComplain`. This includes members that became bad after their index was pushed, such as through a second contribution.
2. For each remaining member, write its **encrypted contributions row** (:146) before verifying.
3. If nothing remains, clear and return.
4. Call `VerifyContributionShares(myId, vvecs, shares)`: aggregated 8-chunks over **this call's members only**, with a per-chunk fallback (`bls/bls_worker.cpp:386-566`, :715-719; `bsdk:worker.rs:149`). A result-length mismatch returns **without clearing** (:161-164).
5. A false → `weComplain`; a true → `WriteVerifiedSkContribution` (:177). Then clear.

**Consequence for ≥ 32 members (M2):**
- Every member on a mainnet-sized type writes `qdkg_E` and `qdkg_S` rows **during Contribute**, in 32-entry groups that follow arrival order.
- `VerifyAndComplain`'s call (§2.5) sees only the remainder.
- The chunk boundaries, and so which chunk falls back, depend on that grouping.

The same verdicts reached in a different grouping are not parity: the dkgdb write order and timing would differ.

### 2.5 Complain (phase 3)

**`VerifyAndComplain`** (`a/dkgsession.cpp:185-224`), members only:
1. Drain the pending verifications (the remainder).
2. Every non-bad member with no contribution → `MarkBadMember` (:207-216).
3. Run the §2.6 fold.
4. `SendComplaint` (:292-332): `bad || badConnection` → badMembers, **else** `weComplain` → complainForMembers. If both are empty, nothing is sent.

**`ReceiveMessage(qcomplaint)`** (`l/dkgsession.cpp:318-354`): for each `i < members.size()`:
- a badMembers bit → `badMemberVotes += complainer`;
- a complainFor bit → `complaintsFromOthers += complainer`, `someoneComplain = true`.

### 2.6 Bad-connection criteria (DKG-01, v24 only)

`a/dkgsession.cpp:226-290`. The whole check is skipped unless `IsQuorumPoseEnabled(type)` (`l/options.cpp:44-47`: SPORK_23 = 0 → all types; = 1 → all but `LLMQ_100_67`/`400_60`/`400_85`). Otherwise:

1. Fold `connman.ForEachNode`, which visits **only fully connected nodes** (`fSuccessfullyConnected && !fDisconnect`, `net.h:1423-1426` → `net.cpp:4852-4855`), in `m_nodes` order, over nodes with a non-null MNAUTH-verified proTxHash:
   - the **first** node seen for a proTxHash supplies its `nVersion`;
   - nodes with `m_masternode_connection` set `has_mn_connection`, OR in `m_version_relay_txs`, and OR in `services & NODE_COMPACT_FILTERS`.
2. For each member other than me, the **else-if chain** (:264-280):
   - not in the fold → `badConnection = IsAllMembersConnectedEnabled(type)` (SPORK_21);
   - else `version < 70242` → bad;
   - else MN connection without tx relay → bad;
   - else MN connection without compact filters → bad.
3. Then, independently: `OutboundFailedTooManyTimes` → bad; `IsPlatformBanned` → bad (:281-288).

Core logs only the negative outcomes (:265-287), so the fold's inputs can't be recovered from debug.log (§4.1).

### 2.7 Justify (phase 4)

**`VerifyAndJustify`** (`a/dkgsession.cpp:334-374`). For each non-bad member:
- `badMemberVotes ≥ dkgBadVotesThreshold` → bad;
- no complaint → skip;
- more than one complaint → bad;
- otherwise, if the complaint flags `myIdx`, `justifyFor += complainer`.

**`SendJustification`** (:376-422), in index order over `justifyFor`:
1. Take the true share `m_sk_contributions[i]`. If `i ≠ myIdx` and `JUSTIFY_LIE`, replace it with `MakeNewKey()` (:399-402).
2. After the loop, `JUSTIFY_OMIT` (:407-410).
3. Sign.

**`ReceiveMessage(qjustify)`** (`l/dkgsession.cpp:422-491`):
1. Preamble.
2. A member already bad: relay only.
3. An entry for a member who didn't complain → bad, and relay.
4. Verify each share against `receivedVvecs[justifier]`:
   - a false → bad;
   - a true for my id → store it, clear `weComplain`, `WriteVerifiedSkContribution`;
   - every true → erase the complainer from `complaintsFromOthers`.
5. Relay (:490).

A lying justification is therefore still relayed, which is why it appears in the capture.

### 2.8 Commit (phase 5) and Finalize (phase 6)

**`VerifyAndCommit`** (`a/dkgsession.cpp:424-467`): every non-bad member with an open `complaintsFromOthers` → bad.

**`SendCommitment`** (:469-579):
1. `validMembers = !bad`. If the count is below minSize, send nothing.
2. `COMMIT_OMIT` (:494).
3. `GetVerifiedContributions` (`l/dkgsessionmgr.cpp:145-192`):
   - it **reads members itself** (:147);
   - a missing vvec row → false;
   - a missing sk row → the default (invalid) key.
4. Build the quorum vvec; `AggregateSecretKeys` must be valid.
5. `pk = vvec[0]`, `vvecHash = SerializeHash(vvec)`.
6. `COMMIT_LIE` → `lieType = GetRand<int>(5)`:

   | lieType | Change |
   |---|---|
   | 0 | `quorumPublicKey` = the pk of a fresh `MakeNewKey()` |
   | 1 | first vvecHash byte +1 |
   | 2 | first byte of the computed commitment hash +1, **after** `BuildCommitmentHash` and before both signatures |
   | 3 | byte 5 of `sig.ToBytes(legacy)` +1, then `SetBytes` |
   | 4 | byte 5 of `quorumSig` +1, then `SetBytes` |

   (:531-562)
7. `sig` = operator sign; `quorumSig = skShare.Sign(commitmentHash, legacy)`.

**`ReceiveMessage(qpcommit)`** (`l/dkgsession.cpp:555-630`), members and non-members alike. It has no preamble:
1. Record the message in both maps.
2. Call `GetVerifiedContributions(validMembers)` (**a members read**, :579).
3. If the quorum vvec can't be built, skip full verification (:583-586).
4. Otherwise all of these must hold, or return nothing with no relay and no score: `vvec[0] == pk` ("calculated quorum public key does not match", :592-595); `SerializeHash(vvec) == vvecHash` ("calculated quorum vvec hash does not match", :596-600); a valid pk share ("failed to calculate public key share", :602-606); `quorumSig.VerifyInsecure(pkShare, GetSignHash)` ("failed to verify quorumSig", :608-611).
5. Mark valid, relay.

**Own lied pcommit (X2).** The liar's own message re-enters through `EnqueueOwn` (sender `-1`). It is rejected at the loopback for every lieType:
- 0/1 fail step 4, because the liar can always build its own vvec: `SendCommitment` just did.
- 2 fails the batch signature check, since `sig` doesn't verify over the real `GetSignHash`.
- 3/4 almost always fail PreVerify as invalid points; if the bytes still decode, they fail the signature or `quorumSig` check.

`PeerMisbehaving(-1, …)` is a no-op, there is no relay, and GETDATA serves only valid pcommits (`l/dkgsession.cpp:678-685`). **No peer ever receives a `commit-lie` pcommit from a stock Core.**

**`FinalizeCommitments`** (`a/dkgsession.cpp:581-688`), members only:
1. Group the valid pcommits by `validMembers` (map order), each group in hash order. Skip groups smaller than minSize.
2. `first = cvec[0]`; version `GetVersion(rotation, V19)`; `quorumIndex = rotation ? idx : 0`.
3. Collect the entries whose key and vvecHash equal `first`'s.
4. `AggregateSecure`, then `Recover` (must succeed), then **`Verify(…, checkSigs = true)`**, which reads members at `l/commitment.cpp:177` and again at :53 (§3.4.1).

**`FinalizeSingleCommitment`** (`a/dkgsession.cpp:690-743`):
- `sk1.MakeNewKey()` (:704-705; the value is overwritten and never seen, but the draw happens);
- signers and valid = `{true}`;
- `quorumPublicKey` = the operator pk;
- vvecHash = 0;
- version `GetVersion(false, V19)`;
- `membersSig` = operator sign, and `quorumSig = membersSig`;
- `Verify(…, true)` must pass, else `assert(false)` (R-3).

### 2.9 Simulated errors and the order of random draws

| Piece | Core |
|---|---|
| RPC | `rpc/quorums.cpp:870-899`: integer rate 0–100 (else "invalid rate. Must be between 0 and 100"); an unknown type gives "invalid type. See DKGError class implementation"; it stores `rate/100` |
| Types | `complain-lie`, `commit-omit`, `commit-lie`, `contribution-omit`, `contribution-lie`, `justify-omit`, `justify-lie` (`l/dkgsession.h:62-84`) |
| Gate | `ShouldSimulateError` (`l/dkgsession.cpp:56-63`): `LLMQ_TEST` only (no draw for other types), then `GetRandBool(rate)` (`random.cpp:653-661`): rate 0 → false with no draw; else `GetRand(10^8 + 1) ≤ 10^8·rate`. At rate 1.0 the draw happens but always gives true. The rate is read at each decision |

**Draw order per start function** (only draws Core actually makes, with `&&` short-circuits applied):

| Step | Draws, in order |
|---|---|
| Contribute | `threshold` × `MakeNewKey` (coefficients) → `CONTRIBUTION_OMIT` → `InitEncrypt` (ephemeral key + IV seed) → for each member `i ≠ me` in index order: `CONTRIBUTION_LIE` → `MakeNewKey` if it fired |
| Receive qcontrib | after a successful decrypt and `idx ≠ me`: `COMPLAIN_LIE` |
| Justify | for each justified member `i ≠ me` in index order: `JUSTIFY_LIE` → `MakeNewKey` if it fired; then `JUSTIFY_OMIT` |
| Commit | `COMMIT_OMIT` (after the minSize check) → `COMMIT_LIE` (after the share build) → `GetRand<int>(5)` if it fired → `MakeNewKey` if lieType = 0 |
| Single member | `MakeNewKey` (sk1) |

Complain and Finalize draw nothing. Signing and IES encryption, once seeded, are deterministic.

---

## 3. The pure-core boundary

### 3.1 Crate and modules

`dash-llmq` is new. `layers.toml` gets `dash-llmq = "Pn"`, and `contrib/node/pure-deny.toml:2` already names it. Its normal dependencies are `dash-evo` (P), `dash-pkc` with `bls`, `dash-p2p-core` and `dash-params` (B). Modules:

| Module | Content |
|---|---|
| `dkg::wire` | the only module that touches BS9a or `LlmqCommitment` fields, and the only one that builds bitsets (validating constructor only) |
| `dkg::bls` | Core-named wrappers over `pkc::bls::core`: `GenerateContributions` from injected coefficients (`threshold::secret_key_share` + `core::public_key`), IES glue, `CBLSId` |
| `dkg::session` | §2.2–§2.8 |
| `dkg::batch` | §2.3 |
| `dkg::conn` | §2.6 |
| `dkg::phase` | §2.1 |
| `dkg::sim` | §2.9 |
| `dkg::rng` | `DkgRng`, its production implementation, and its replay implementation (behind a feature) |
| `dkg::reference` | the in-process multi-member harness (`cfg(any(test, feature = "test-util"))`, behind `required-features`) |

### 3.2 Inputs

```rust
/// One session = Core's CDKGSession/ActiveDKGSession for (type, base, quorumIndex), on an
/// active masternode. Observers run no session (net_dkg.cpp:511-518, :739).
pub struct SessionInit<'a, E: DkgMemberEntry> {
    pub params: &'a LlmqParams,          // effective params (regtest overrides applied, §4.1)
    pub base: QuorumBase,                // { hash, height, has_parent }
    pub quorum_index: i32,
    pub members: QuorumMembers<E>,       // DK1's access #1 (§3.4.1), Core's order
    pub active_pro_tx: Option<ProTxHash>,// AM1's proTxHash; None = Core's null (MN not ready)
    pub rotation_enabled: bool,          // IsQuorumRotationEnabled(params, base)
    pub v19_after_base: bool,            // session sign/ToBytes flag = legacy iff !this (:34)
}

/// LQ5's GetQuorumRelayMembers(params, base, me, onlyOutbound = true) over CH7b's facade:
/// access #2 (§3.4.1). `init` calls it once, only if `active_pro_tx` is a member, after the
/// minSize check and InitLocalSessionStatus (l/dkgsession.cpp:135-146).
pub trait RelayTopology {
    fn relay_members(&mut self, t: LlmqType, base: &QuorumHash, me: &ProTxHash) -> BTreeSet<ProTxHash>;
}

/// Core's four randomness points (m7). Production: Core-faithful over a CSPRNG. Replay
/// (feature `replay`): returns P-2's or a capture's recorded decisions, and fails on a tag
/// mismatch, so a divergent draw order shows at the first wrong draw.
pub trait DkgRng {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue;              // MakeNewKey (valid key)
    fn ies_seed(&mut self, recipients: usize) -> IesSeed;            // InitEncrypt: ephemeral sk + IV seed
    fn rand_bool(&mut self, rate: f64, why: SimError) -> bool;       // GetRandBool; rate 0 → false, no draw
    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64;         // GetRand<int>(n), [0, n)
}
pub enum KeyDraw { Coefficient(u32), ContributionLie(u32), JustifyLie(u32), CommitLieKey, SingleMemberSk1 }

/// Everything a step may touch outside the session; all injected.
pub struct DkgEnv<'a> {
    pub bls: BlsCtx,                     // process-wide read/hash scheme (tip's), per step (m11)
    pub rng: &'a mut dyn DkgRng,
    pub key: &'a dyn OperatorKey,        // AM1: Sign(hash, legacy), Decrypt(obj, idx), pk
    pub store: &'a mut dyn DkgStore,
    pub debug: &'a mut dyn DkgDebugSink,
    /// Rates as of this call. Each call is one uninterrupted run of Core decisions on the
    /// phase thread, and the RPC can't interleave inside it, so a snapshot per call equals
    /// Core's read at each decision; DK1 rereads between calls.
    pub sim: &'a SimErrorRates,
    pub runner: &'a dyn DkgBlsRunner,    // order-preserving map over independent BLS work
    pub verify: &'a dyn FinalVerifier,   // verify_commitment(.., Some(SigPath::Sync), ..) + P-4
}

/// dkgdb (DK1), row for row with l/dkgsessionmgr.cpp:125-192. Infallible by contract:
/// a backend error (Core's dbwrapper throws) fail-stops DK1's implementation. An absent
/// row is not an error: it is the `None` / invalid-key result below (:163-169, :180-183).
pub trait DkgStore {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]);
    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue);
    fn write_encrypted(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, c: &BlsIesMultiBlobs);
    /// GetVerifiedContributions. The implementation FIRST makes the members access through
    /// the facade (dkgsessionmgr.cpp:147; access #3/#4 in §3.4.1), then walks those members
    /// against `valid`: None when a vvec row is missing; a missing sk row is the invalid key.
    fn verified_contributions(&mut self, t: LlmqType, base: &QuorumHash, valid: &[bool])
        -> Option<VerifiedContributions>;
}
```

`ConnSnapshot` (§2.6) is a value DK1 builds per call:
- `Vec<ConnFacts { verified_protx, version, masternode_connection, relay_txs, services }>` with **only fully connected nodes** (`fSuccessfullyConnected && !fDisconnect`), in `m_nodes` order;
- per member, `{ outbound_failed_too_many, platform_banned }`;
- the two spork booleans for the type.

### 3.3 API and outputs

```rust
impl<E: DkgMemberEntry> DkgSession<E> {
    pub fn init(init: SessionInit<'_, E>, topo: &mut dyn RelayTopology,
                debug: &mut dyn DkgDebugSink) -> Result<Self, InitRefusal>;   // l/:90-155
    // Start functions: single-shot, in phase order; a repeat or out-of-order call is
    // Err(DkgFault::PhaseOrder), which DK1 treats as a fault (Core's handler can't do it).
    pub fn contribute(&mut self, env: &mut DkgEnv) -> Result<Option<DkgContribution>, DkgFault>;
    pub fn verify_and_complain(&mut self, conns: &ConnSnapshot, env: &mut DkgEnv)
        -> Result<Option<DkgComplaint>, DkgFault>;
    pub fn verify_and_justify(&mut self, env: &mut DkgEnv) -> Result<Option<DkgJustification>, DkgFault>;
    pub fn verify_and_commit(&mut self, env: &mut DkgEnv) -> Result<Option<DkgPrematureCommitment>, DkgFault>;
    pub fn finalize_commitments(&mut self, env: &mut DkgEnv) -> Result<Vec<LlmqCommitment>, DkgFault>;
    pub fn finalize_single_commitment(&mut self, env: &mut DkgEnv)
        -> Result<Option<LlmqCommitment>, DkgFault>;                  // SingleCommitmentUnverified (R-3)
    /// ProcessPendingMessageBatch over exactly one pop (≤ 8, FIFO, own messages included).
    pub fn process_batch<P: Copy + Ord>(&mut self, kind: DkgMsgKind,
        batch: &[(Sender<P>, &[u8])], env: &mut DkgEnv) -> BatchOutcome<P>;
    pub fn get_contribution(&self, h: &[u8; 32]) -> Option<&DkgContribution>; // + complaint,
    pub fn get_premature_commitment(&self, h: &[u8; 32]) -> Option<&DkgPrematureCommitment>; // justification
    pub fn my_index(&self) -> Option<usize>;
    pub fn relay_members(&self) -> &BTreeSet<ProTxHash>;              // empty for a non-member
}
pub enum Sender<P> { Peer(P /* NodeId */), Own /* Core's -1 */ }
pub struct BatchOutcome<P> {
    pub misbehaving: Vec<Misbehaviour<P>>, // Own entries never appear
    pub relay: Vec<DkgInv>,                 // (type, session hash), in Receive order
    pub events: Vec<DkgEvent>,              // Core's log text minus cxxtimer fields
}
pub enum Misbehaviour<P> {
    Decode { peer: P, reason: DkgReject },     // per message, 100
    PreVerifyBan { peer: P, reason: DkgReject },// per message, 100
    BadSignature { peer: P },                   // once per peer per batch, 100
}
/// Pure (n7): DK1 calls it on each tip that passes CDKGSessionManager's gates.
pub fn phase_for_tip(params: &LlmqParams, quorum_index: i32, tip_height: i32,
                     snapshot_unvalidated: bool, rotation_enabled_at_tip: bool,
                     current: QuorumPhase) -> Option<PhaseTick /* {base_height, phase} */>;
pub fn sleep_before_phase_ms(params: &LlmqParams, pow_spacing_s: i64, my_idx: Option<usize>,
                             factor: f64, mine_on_demand: bool) -> i64;
```

**Batch contract (m6).** `process_batch` equals `ProcessPendingMessageBatch` exactly when:
- **(a) Sender identity is per connection.** `Sender` equality is NodeId equality, which drives the all-same-node rule (:289-292) and the bad-node skip set. `Own` is distinct from every peer and stable within a batch.
- **(b) Score multiplicities match.** Decode and PreVerify bans are per message, so one peer can collect several. A bad signature is once per peer. `Own` scores are dropped, like Core's `PeerMisbehaving(-1, …)`.
- **(c) PreVerify runs over the whole batch before any Receive.** Its "≥ 2" counts are the counts from before the batch (§2.3).
- **(d) Set semantics on the session hash.** This holds in the preamble and in pcommit's unconditional emplace.

DK1 must hand over exactly Core's pop: FIFO, at most 8, one phase's queue.

**Outputs:**
- the outgoing message from each start function (DK1 serializes it and enqueues it as `Own`);
- scores and invs per batch;
- `DkgStore` writes;
- `DkgDebugSink` updates at every Core `Update*Status` / `Init*` / `MarkPhaseAdvanced` point;
- final commitments;
- events.

**GETDATA (m10).** Core serves the `get_*` results only inside phase windows: contrib in [Initialized, Contribute], complaint in [Contribute, Complain], justification in [Complain, Justify], pcommit in [Justify, Commit] (`l/dkgsessionmgr.cpp:57-123`). Only valid pcommits are served. The windows are DK1's filter.

**PoSe.** The DKG's PoSe result is the mined `validMembers` (CH7). Peer scores are DK1's to apply.

### 3.4 How DK1 plugs it in (SB0 §2.3)

`dashd-quorums` (layer N; A1:582), per (type, index):
- four `PendingQueue`s with Core's quota and seen-set;
- the §2.1 round driver on `dash-clock`;
- `phase_for_tip` on each tip;
- at Initialized: **null the current session first** (rc.4 :94-105), make access #1, then `DkgSession::init` (with LQ5's `RelayTopology` and AM1's key), and install the session only if `init` succeeds.

At each phase start, DK1 calls the start function, serializes the result and enqueues it as `Own`. While waiting it pops ≤ 8, calls `process_batch`, applies the scores through dashd-p2p, and relays the invs. At Finalize, each commitment goes to `MineablePool::add` (CH7b-5, `pool.rs:50`), and a returned inv is relayed as `qfcommit`. The edges are N → Pn and N → P, which L2 allows, and L6 keeps the crate out of the mobile closure.

#### 3.4.1 Members access points (M1, X1)

Each row is a Core `utils::GetAllQuorumMembers` call on the DKG path, and the place the Rust node makes the same access through CH7b's facade. On a facade miss, an access can recompute and write rotated snapshot rows (LQ1 §3.7.4), so **no access may be dropped, merged or moved** (the LQ1 "finding 2" rule).

| # | Core call site | When, and how often | Rust: who makes the access |
|---|---|---|---|
| 1 | `CDKGSession::Init` → `l/dkgsession.cpp:92` | every session, member or not; once | DK1, through the facade, immediately before `DkgSession::init`; the result is `SessionInit.members` |
| 2 | `GetQuorumRelayMembers` → `l/llmq/utils.cpp:781` (from `l/dkgsession.cpp:137`) | Init, **members only**, after the minSize check and `InitLocalSessionStatus`; 0 or 1 | `RelayTopology::relay_members` (LQ5 over the facade), called by `init` at that point |
| 3 | `GetVerifiedContributions` → `l/dkgsessionmgr.cpp:147` (from `a/dkgsession.cpp:505`) | `SendCommitment`, after the minSize and `COMMIT_OMIT` checks; 0 or 1 | `DkgStore::verified_contributions` (DK1), first action of the call |
| 4 | same → :147 (from `l/dkgsession.cpp:579`) | every `ReceiveMessage(qpcommit)` that survives the batch, own included, on members and non-members | same |
| 5 | `CFinalCommitment::Verify` → `l/commitment.cpp:177` | `FinalizeCommitments`, per group that reaches `Verify` (after `Recover` succeeds), only if the structural checks before :177 pass; single-member `Verify` the same | `FinalVerifier` → `verify_commitment` (`commitment.rs:320`) |
| 6 | `VerifySignatureAsync` → `l/commitment.cpp:53` (called at :201) | the same `Verify`, `checkSigs = true`, after the bit-range checks | `FinalVerifier` → `verify_commitment(Sync)` **with P-4** |

Outside LQ4, the same DK1 round also makes accesses for:
- `EnsureQuorumConnections` → `GetQuorumConnections` (`l/llmq/utils.cpp:758` or :781; P7);
- `AddQuorumProbeConnections` (`l/net_dkg.cpp:798`; P7);
- `GetEncryptedContributions` (`l/dkgsessionmgr.cpp:196`; QM1 data recovery).

QM1a's DKG-configured trace suite covers those; T6 covers rows 1–6.

### 3.5 How CH7 consumes the commitments

There's no direct LQ4 → CH7 edge; the path runs through the pool and blocks.
1. DK1 adds each final commitment to the pool, whose `qfcommit` relay goes through LQ2's verdict (`pool.rs:211`).
2. The miner (MN0-2a / MN1) puts it in a type-6 transaction in the mining window. For rotated types the window starts at `signingActiveQuorumCount + 5pb` (§2.1).
3. CH7a/CH7b validate it on connect through LQ2.
4. QM1a builds the quorum from the mined commitment plus the dkgdb rows.

LQ4's contract with CH7 is the byte-exact `LlmqCommitment` and the dkgdb row formats, which DK1 owns (`l/dkgsessionmgr.cpp:127-132`).

### 3.6 Parallelism and performance

**Hot spots for `llmq_400_60`** (n = 400, t = 240):

| Work | Per | Size |
|---|---|---|
| contribution verification | member | 12 mid-phase calls of 32 plus a remainder of 16 (§2.4), each in 8-chunks of 240-term G1 evaluations |
| pcommit checks | member | up to 400, each a `BuildPubKeyShare` plus a pairing |
| finalize | finalizer | an `AggregateSecure` over up to 400 signers, plus a `Recover` |

**Parallelism.** `DkgBlsRunner` is an order-preserving map that DK1 runs on its BLS pool. The default `SerialRunner` keeps the pure crate pool-free (DEC-79; same pattern as LQ2's `BlsCheckRunner`, DEC-325). The 32-groups are inherently sequential in Core, and the runner keeps them that way: it parallelizes only within a call.

**Route to 2× (DEC-616 #10: a BACKLOG item, not a dependency):**
- a serial Pippenger for the 240-term G1 evaluations (basic flag; the legacy flag keeps the naive path);
- a `BuildPubKeyShare` cache like `CBLSWorkerCache`;
- LQ5's MSM Lagrange for `Recover`.

**PF0 bench rows (instructions:u, fixed-seed harness transcripts):**
- `dkg-contribute-{50_60,400_60}`
- `dkg-verify-pending-{50_60,400_60}`
- `dkg-justify-verify-50_60`
- `dkg-commit-400_60`
- `dkg-pcommit-verify-400_60`
- `dkg-finalize-{50_60,400_60}`

The "≤ dashd" baseline is a one-off Core count per DEC-465 (D9).

---

## 4. Parity evidence plan

### 4.1 Core-produced fixtures

| Fixture | What it proves | Status |
|---|---|---|
| `bls_llmq_100.json5` | per scheme, for 3 members with t = 2: contribute, verify, commit and finalize values | exists; rerun through the session API (T2) |
| BS12 `dkg.json5` (rc.3 `feature_llmq_dkgerrors.py`, 4 nodes / 3 MNs) | 8 type-100 sessions: a clean one, then contribution-omit, contribution-lie with a correct justification, justify-omit, justify-lie, complain-lie, commit-omit and commit-lie (the FT carries some rates forward from one scenario to the next, rc.4 :37-90) | exists; T3 |
| **New: `dkg-ft-v24-rc4/`** (BS12's `dkg.py` generator on rc.4) | `feature_llmq_dkgerrors` rerun (adds a `scenario` label and the erring proTx to each session); `feature_llmq_singlenode`; `feature_llmq_rotation` (9 nodes / 8 MNs, `llmq_test_dip0024` size 4, which gives **non-member** sessions and quorumIndex ≥ 1); `feature_llmq_simplepose`, with a **capture-build log patch** at the entry of `VerifyConnectionAndMinProtoVersions` that dumps the fold inputs (each fully connected node's verified proTx, nVersion, MN flag, relay, services, plus both spork values), since Core logs only negative outcomes | LQ4a-2 (simplepose) and LQ4b-1 (the rest) |
| **New: 12-member capture** (DEC-616 #4) | `feature_llmq_dkgerrors`-style rounds with `set_dash_llmq_test_params(12, 7)`, i.e. `-llmqtestparams=12:7`. Effective `llmq_test` values are size 12, `minSize = threshold = dkgBadVotesThreshold = 7` (`chainparams.cpp:1019-1027`). PROVENANCE records them and the harness builds `LlmqParams` from them, not `dash_params` defaults. 12 MNs + 1 controller = 13 nodes, under the framework's cap of 20. Scenarios: honest; 5 omitted contributions (exactly minSize left); mixed lies. That gives batches beyond 8 and `minSize < size` | LQ4a-2 |
| **≥ 33-MN capture** (M2; skipped, DEC-619 Q13) | the mid-phase 32-trigger in a live Core round | **not possible on Core's stock framework**: `MAX_NODES = 20` (`test_framework/util.py:324`, asserted at `test_framework.py:951` and `util.py:362`). A capture-only patch raising it to ≥ 35 is plausible on agentbox but untried. **T6 carries M2.** No capture spike (DEC-619 Q13) |

### 4.2 Oracle-shim entry points (P-2, "O2a-DKG"; named, not built)

On the Core `node/oracle-shim` branch, with an ABI bump, built on O2a-LQ1's context (headers, MN list, record-only trace). Seams in the patched build:
- **Members hook.** At `utils::GetAllQuorumMembers` entry: if (type, base) has injected members, return them, and count every call with a MEMBERS trace event in O2a-LQ1's format. Because Core's `Init`, `GetVerifiedContributions` and `Verify` all go through this function, one hook covers access rows 1–6 and lifts the member count to anything up to `MAX_LLMQ_SIZE` = 400 (`l/params.h:17`).
- **Recorded decisions.** At `MakeNewKey` (DKG call sites only), `InitEncrypt`, `GetRandBool` from `ShouldSimulateError`, and `GetRand<int>(5)` in `SendCommitment`. Each takes the next tagged decision from the script, with the same tags as `DkgRng`.
- **dkgdb write trace** (row kind, proTx, bytes, in order) and a debug-status JSON per step.

Entry points:
- `oracle_dkg_preverify(type, params, base, members, counts, kind, payload) → {decoded, ok, ret_ban, reason}`
- `oracle_dkg_batch_sigs(members, [(node_id, kind, payload)]) → bad_node_ids`
- `oracle_dkg_session_open(script_header) / _step(op) / _close` → per step `{scores, invs, outgoing bytes, store writes, members accesses, debug JSON, finals}`. The ops are `start_phase(k)`, `batch([...])`, `set_sim(type, rate)`, `conn_snapshot(...)` and `set_tip_flag(scheme)`.
- `oracle_dkg_finalize(members+keys, base flags, pcommits) → finals`

### 4.3 Tests

**T1, unit (LQ4a-1/2, LQ4b-1).** Every branch of §2.3–§2.8 has a positive and a negative case, with Core's reason text and `retBan`. Groups:

- **Batch signatures:** all good; one bad node among several, where the bad node's valid messages are skipped too; all from one node; a duplicate sign hash; leading invalid signatures; an unknown member; two messages from the same proTx on two connections (different senders, m6 a).
- **m6 (c):** three distinct contributions from one member in one batch all pass PreVerify. Then the first is processed, the second is recorded, marks bad and is relayed, and the third is dropped with no relay.
- **m6 (d):** a re-delivered equal session hash with different raw bytes is processed again, count unchanged. A pcommit re-emplace.
- **Score multiplicity:** two decode failures from one peer give two entries; two bad signatures from one peer give one; `Own` gives none.
- **M2, mid-batch trigger:** a 33-member session where the 32nd push is message 3 of an 8-message batch. `VerifyPendingContributions` runs inside that `ReceiveMessage`, before message 4's. 32 `qdkg_E` rows (minus bad/weComplain) and the `qdkg_S` rows are written **in the Contribute phase**, before `verify_and_complain`. `verify_and_complain`'s call covers exactly the 1 remaining entry.
- **M2, the 32-count edges:** decrypt failures and `COMPLAIN_LIE` aren't pushed and so delay the trigger; a member made bad after its push is skipped with no `qdkg_E` row; the length-mismatch return leaves the list uncleared (injected runner).
- **§2.6 truth table:**
  - spork 23 off;
  - `EvalSpork` = 1 for `400_60`;
  - a member not connected with spork 21 on, and with it off;
  - version 70240 (v23.1.8's `PROTOCOL_VERSION`) bad; 70241 bad; 70242 good;
  - MN connection with relay 0; MN connection without filters; non-MN-only connection with relay 0 (passes);
  - first-node-version precedence;
  - a connecting or disconnecting node excluded by the snapshot contract;
  - outbound failures and platform ban stacking on any branch.
- **Single-shot start functions:** a second `verify_and_complain` → `DkgFault::PhaseOrder`.
- **Finalize:** group order and `first`.
- **lieType stages:** each lieType per the §4.4 stage table.

**T2, corpus.** `bls_llmq_100.json5` through the reference harness, with recorded vvecs, shares and operator keys. All verdicts, pcommit fields, recovered sigs and `membersSig` must be equal.

**T3, transcript replay (BS12, then the rc.4 captures).**

Per session and per replayed node `m`:
1. Build the session with Core's member order and the capture's effective params.
2. Adopt inputs per T3-C.
3. Feed every captured message in capture order, in pops of ≤ 8.
4. Compare per T3-C.

In addition:
- **Non-member replay (m2):** each rotation-capture session is replayed from an MN outside the quorum. Expected: no `relay_members` call, empty relay members, no `InitLocalSessionStatus`, invs only for qwatch, and verdicts equal to Core's.
- **Flag straddle (m11):** a T4 row and a T6 script cover a session whose base is before V19 while the tip is after it (regtest `-testactivationheight=v19@h`, with h inside the session's phases). Signing and `ToBytes` use legacy, decode and hashing use basic, and `env.bls` switches between steps where the tip crosses h.

**T3-C, acceptance per message class (M3).** "Deterministic given the replay inputs" means BLS signatures are unique, the bitsets are fixed, and every random draw is either adopted or replayed.

| Class | What is replayed or adopted | Assertion |
|---|---|---|
| Every member's own **qcontrib** (coefficients, IES seed, contribution-lie keys) | **adopted** from the capture (`ContributionSource::Recorded`). The true share for recipient j is the decrypted blob when it `fits`, else the share m reveals in its captured correct qjustify (contribution-lie + correct-justification scenario). Otherwise it is unknown and unused, since no justification for j is sent | none (input) |
| qcomplaint (all members, incl. complain-lie at rate 1.0) | nothing | **byte-exact** |
| qjustify, honest or correct justification | nothing beyond the adopted true shares | **byte-exact** |
| qjustify, **justify-lie** (erring m) | `DkgRng` replay: each `new_key(JustifyLie(i))` returns the captured key for index i, in index order; each `rand_bool` returns its recorded value | **byte-exact** (the signature is over the replayed keys); every replayed key has `fits = false` |
| justify-omit (erring m) | `rand_bool(JUSTIFY_OMIT)` replay = true | m sends no qjustify; the capture has none from m |
| qpcommit, honest; commit-omit (erring m) | nothing | **byte-exact** for every relayed pcommit; under omit, m sends none |
| qpcommit, **commit-lie** (erring m) | nothing to adopt: the lied pcommit is not in the capture (X2) | **equivalence**: for each forced lieType k ∈ 0..4 (`rand_range` replay = k; for k = 0 a fresh key), m relays no pcommit; m's own loopback rejects it at the §4.4 stage with Core's reason; m's `validCommitments` holds only the honest ones; m's qfcommit equals the captured final. The rc.3 commit-omit and commit-lie sessions look the same on the wire (2 pcommits each), so until the rc.4 `scenario` label exists, both sessions must pass under OMIT **and** under each of the five lieTypes |
| qfcommit (every finalizer) | nothing | **byte-exact** against the captured final |
| Per-member verdicts, `validMembers`, dkgdb rows | — | equal to the capture (`member_shares`, `fits`) |

**LQ4a-2 acceptance (restated):** T3-C rows for qcomplaint and qjustify pass on all 8 BS12 sessions, for every replayed member.
**LQ4b-1 acceptance:** the qpcommit and qfcommit rows pass on BS12 plus the rc.4 captures. LQ4b-1 may start on the rc.3 commit-omit/commit-lie equivalence, but it **merges only after the labelled rc.4 rerun agrees** (DEC-619 Q12).

**T4, in-process DKG.** Every finalizer's commitment must be byte-equal and pass `verify_commitment(.., Some(SigPath::Sync))`, with the right `quorumIndex` and version for rotated types.

| Case | Types / setup |
|---|---|
| honest, then drops and lies | 3, 4, 12 and 50 members (`llmq_test`, `llmq_test_dip0024`, `llmq_devnet`, `llmq_50_60`) |
| exactly `minSize` left | `llmq_devnet` (12) with 5 dropouts |
| several mid-phase 32-triggers | `llmq_100_67` (100 members) |
| flag straddle (m11) | pre-V19 base, post-V19 tip |
| LQ4b, 400 members | `llmq_400_60` and `llmq_400_85`, all members through commit; pcommit checks and finalize sampled on 8 members in the honest case (DEC-616 #5: tier 1, capped threads) |
| rotated | `llmq_60_75` and `llmq_test_dip0024` at index ≥ 1 |

**T5, fuzz `dkg_session`.** Honest material is precomputed per member set: `llmq_test`, `llmq_test_dip0024`, 12/7 and a **33-member** `llmq_test` override (needed so the fuzzer reaches the 32-trigger).

Fuzzer actions:
- drop, duplicate, reorder, or regroup into pops;
- flip bytes;
- equivocate;
- force each sim error and each lieType, including "perturbed bytes still decode" (§4.4);
- unverifiable groups (a finalizer missing a vvec row while a group's entries disagree);
- random `ConnSnapshot`s and spork pairs;
- a flag flip between steps.

Invariants: no panic; store writes only of rows Core would write; every honest finalizer that finalizes agrees byte for byte. Certify ≥ 24 CPU-h under DEC-184.

**T6, differential `diff_dkg_session` (LQ4b-2, needs P-2).** Action scripts run against `oracle_dkg_session_*` with recorded decisions and the members hook. Per step, these must be equal: scores (with multiplicity), invs, outgoing bytes, the **ordered** store writes, the **members-access count and each access's position** among the step's store reads and writes (§3.4.1 rows 1–6; M1), debug JSON and finals.

Script families:

| Family | Required? | Content |
|---|---|---|
| A | yes | `llmq_test` at 3 and 12/7 (overridden) |
| B (M2) | yes | **33, 40 and 65 injected members** on `llmq_test` overridden to `33:20`, `40:24` and `65:39`. Contribution orders put the 32nd push at each batch position 1–8 (a first pop of k = 1..8, then pops of 8). The own message lands at a varying position. 0–2 decrypt failures or `COMPLAIN_LIE` before the trigger. A member made bad after its push. An invalid share in exactly one 8-chunk of a 32-wide call, so the fallback runs in that chunk only. At 65, two triggers |
| C (M2) | if O2a-LQ1's context can select the network | `llmq_50_60` with mainnet params; otherwise B covers it at `50:30` |
| D | yes | rotated `llmq_test_dip0024` with non-member steps; the flag straddle (m11); single member |

At least 24 CPU-h with 0 divergences. `diff_dkg_preverify` and `diff_dkg_batch_sigs` are cheaper companions run at a higher rate.

### 4.4 The `dkgsimerror` matrix

**In-process (LQ4b-2).** Reproduce `feature_llmq_dkgerrors.py` (rc.4 :37-90; 3 MNs; the erring member is `mninfo[0]`; rates forced to 100 or 0, carried forward as the FT does):

| Scenario | Core expectation (FT) | Rust check |
|---|---|---|
| contribution-omit | contributions 2; member invalid | `validMembers[m] = 0` in every honest final |
| contribution-lie | 3, 2 complaints, 1 justification; valid | the justification verifies; `weComplain` cleared |
| + justify-omit | 3, 2, 0; invalid | open complaints → bad at commit |
| + justify-lie | 3, 2, 1; invalid | lying shares fail → bad |
| complain-lie | 3, 1 complaint, 2 justifications; valid | both justify the false complainFor |
| commit-omit | 3, 0, 0, 2 commitments; valid | 2 signers |
| commit-lie | 3, 0, 0, 2 commitments; valid | each lieType forced; the liar relays nothing (X2); 2 signers |

**Commit-lie stages per lieType (m3).** The "at the liar" column is Core's own `commit-lie`: the message arrives as sender `Own`, its score is dropped, and it is not relayed. The "at a receiver" column applies only when a modified sender transmits the lie (T1, T5, T6).

| lieType | At the liar (own loopback) | At a receiver: stage, Core log text, score |
|---|---|---|
| 0 (fresh key) | `ReceiveMessage`: "calculated quorum public key does not match" (`l/dkgsession.cpp:592-595`) | the same where the receiver can build the vvec (no relay, no score); if it can't, the pcommit is accepted unverified and relayed (:583-586) |
| 1 (vvecHash +1) | `ReceiveMessage`: "calculated quorum vvec hash does not match" (:596-600) | the same split as lieType 0 |
| 2 (hash +1 after build) | `BatchVerifyMessageSigs`: `sig` fails over `GetSignHash` (`l/dkgmessages.h:181-184`) | batch signature failure → **100 once per peer**, and all of that peer's batch skipped (`l/net_dkg.cpp:425-437`) |
| 3 (`sig` byte 5 +1) | PreVerify "invalid membersSig" (ban), in all but a negligible fraction of draws | PreVerify ban → **100 per message**. **Forced variant** (perturbed bytes still decode to a valid point): batch signature failure → 100 once per peer |
| 4 (`quorumSig` byte 5 +1) | PreVerify "invalid quorumSig" (ban) | PreVerify ban → 100 per message. **Forced variant:** `ReceiveMessage` "failed to verify quorumSig" (:608-611) where the vvec builds → no relay, no score; else accepted unverified |

The "perturbed bytes still decode" variant is forced in tests by replacing the perturbation with a valid point that doesn't verify. A real byte-5 change essentially never gives a valid G2 point that also passes the subgroup check.

**Decision mechanics:**
- a non-`llmq_test` type never draws;
- rate 0 never draws;
- rate 1.0 draws and always fires;
- the rate is read per call (§3.2);
- the draw order of §2.9 is replay-checked by the `DkgRng` tags.

**Transcript (LQ4b-2).** T3-C over the 8 BS12 sessions and the rc.4 rerun.

**Live regtest against dashd.** That is DK1 (DEC-616 #2).

---

## 5. Slicing and cost

| Slice | Content | Depends | Acceptance | BG rows | Tier |
|---|---|---|---|---|---|
| **P-1** (base-sdk, BS11 thread) | `pkc::bls::core::sign` over both flags | — | `sign.json5` rows under both flags; `just comb` per commit | — | impl O-m; rev Sol→O-h |
| **P-2** O2a-DKG (Core shim) | §4.2: members hook (inject + count), recorded decisions, store trace, entry points, ABI bump | O2a-LQ1 (merged) | shim selftests; `oracle_dkg_session` reproduces the 8 BS12 sessions from their recorded qcontribs and decisions; access counts for one 3-member round match §3.4.1 | — | impl O-m; rev Sol |
| **P-4** (dash-evo) | `SigPath::Sync` makes the second members read in Core's place (§1.2) | — | an LQ2/SB8 counting-source test: two reads per `Sync` verify that reaches :53, one when it stops between :177 and :53, zero before :177; existing twins unchanged | — | impl O-m by the LQ4 implementer, before LQ4b-1 (DEC-619, BL-475); rev O-h |
| **LQ4a-1** | crate scaffold, `wire`/`bls`/`rng` modules, `phase` helpers, `init` + `RelayTopology`, `process_batch` (decode, structure, 4× PreVerify, batch signatures, preamble), contribute, receive(contrib) incl. the 32-trigger, `VerifyPendingContributions`, the traits, reference harness | — (tests may use the typed-sign stand-in) | T1 for these paths, incl. the m6 and M2 rows; T2 contribute/verify; T3-C qcontrib-derived verdicts on BS12; T4 3–100 members through phase 2; `phase_for_tip` against Core's rule for every type × index × 3·dkgInterval heights × both gates | `dkg-contribute-*`, `dkg-verify-pending-*` | impl O-h; rev Sol→O-h |
| **LQ4a-2** | complain (§2.6 fold, SendComplaint, receive), justify (both sides), the contribute/complain/justify sim hooks, the 12/7 and simplepose captures, `dkg_session` fuzz v1 | LQ4a-1 | T1 truth table; **T3-C qcomplaint + qjustify rows on all 8 BS12 sessions**; simplepose complaints from the logged fold inputs; T4 through phase 4; fuzz smoke + 24 CPU-h certify | `dkg-justify-verify-50_60` | impl O-h; rev Sol→O-h |
| **LQ4b-1** | commit (both sides, commit hooks), finalize, single member, GET accessors, `FinalVerifier`, the singlenode/rotation captures | LQ4a-2, **P-1**, P-4 | T2 commit/finalize; T3-C qpcommit + qfcommit rows on BS12 + rc.4 captures, incl. non-member replays; T4 400-member + rotated + flag straddle; the §4.4 lieType stage table in T1 | `dkg-commit-400_60`, `dkg-pcommit-verify-400_60`, `dkg-finalize-*` | impl O-h; rev Sol→O-h |
| **LQ4b-2** | §4.4 in-process matrix, fuzz v2, `diff_dkg_session` families A–D + companions | LQ4b-1, **P-2** | the matrix with all lieTypes; T3-C replays of every scenario; `diff_dkg_*` ≥ 24 CPU-h with 0 divergences, incl. family B and the access-count comparison; fuzz certify | — | impl O-h; rev Sol→O-h |

**What parallelizes:**
- P-1, P-2, P-4 and LQ4a-1 start together.
- The rc.4 and 12/7 captures run beside LQ4a-1.
- DK1's design can start once LQ4a-1's API (§3.3) freezes.
- **DK1's implementation should depend on LQ4b-1** (m5), so LQ4b-2 runs beside it. The critical path to DK1's start is then LQ4a-1 → LQ4a-2 → LQ4b-1, plus P-1 and P-4 finishing in parallel.
- LQ4a-2 → LQ4b-1 → LQ4b-2 is serial.

**Cost (agent hours, rough, reviews included):**

| Slice | Agent hours | Other |
|---|---|---|
| P-1 | 5–8 | |
| P-2 | 14–18 | rev0 was 12–16; +2 for the members hook and store trace |
| P-4 | 3–5 | |
| LQ4a-1 | 20–26 | |
| LQ4a-2 | 16–20 | 24–30 CPU-h fuzz; incl. the two captures |
| LQ4b-1 | 18–22 | ≈ 1 CPU-h of 400-member runs |
| LQ4b-2 | 14–18 | 24–36 CPU-h diff + fuzz |
| **Total** | **≈ 90–117** | **≈ 50–70 CPU-h farm** |

The critical path to DK1 is about 55–68 agent hours.

**PF0.** Rows are added as NEW and anchored after merge.

---

## 6. Open questions for the manager

Q1–Q10 were answered by DEC-616 and Q11–Q13 by DEC-619. None are open.

11. **P-4 owner (X1).** Answered by DEC-619: BL-475, done by the LQ4 implementer as a dash-evo pre-slice before LQ4b-1 and reviewed Opus-h. The second members read stays observable.
12. **The rc.3 commit-omit/commit-lie ambiguity.** Answered by DEC-619: LQ4b-1 may start on equivalence, and it merges only after the labelled rc.4 rerun agrees (stated in §4.3).
13. **The ≥ 33-MN Core capture (M2).** Answered by DEC-619: no spike. T6 family B carries M2.

---

## 7. PLAN text for the manager (m4, m5, n8)

Replace the following cells exactly. Columns are Scope | Milestone | Crate | Deps | Size | Area | D | Tiers | FR | Acceptance.

**LQ4a (PLAN:596), Acceptance cell.** Replace
`in-process DKG with 3–50 members; `dkg_session` fuzz (drops and lies); BS12 transcript reproduced; v24 criteria tests plus a negative test for a 70240 peer · BG: crit contribution/verification time`
with
`in-process DKG with 3–100 members (incl. ≥ 32-member mid-phase verification); `dkg_session` fuzz (drops and lies); BS12 transcript replay: qcomplaint/qjustify byte-exact per design §4.3 T3-C (own qcontribs adopted, justify-lie keys replayed); 12/7 Core capture; v24 criteria tests incl. 70240/70241 bad and 70242 good · BG: crit contribution/verification time`

**LQ4b (PLAN:597).**
- **Deps cell:** replace `LQ4a` with `LQ4a, BS11c (P-1), P-4; LQ4b-2 also O2a-DKG`.
- **Acceptance cell:** replace
  `400-member in-process DKG; LLMQ-100 corpus cross-check; `dkgsimerror` outcomes vs dashd on regtest (with DK1) · BG: crit finalize time`
  with
  `400-member in-process DKG; `bls_llmq_100` corpus + BS12/rc.4 transcript replays (qpcommit/qfcommit per design §4.3 T3-C; commit-lie by equivalence over all lieTypes); in-process `dkgsimerror` matrix incl. every lieType (live comparison vs dashd is DK1's, DEC-616 #2); LQ4b-2: `diff_dkg_session` ≥ 24 CPU-h incl. ≥ 33-member scripts and the members-access count · BG: crit finalize time`

**DK1 (PLAN:702).**
- **Scope cell:**
  - replace `intake rules per profile` with `intake rules (v24, DEC-82)`;
  - replace `· O17: DKG intake (DKG-03), observer pushes (DKG-05) and the startup hook (DKG-06) per profile` with `· O17: DKG intake (DKG-03), observer pushes (DKG-05) and the startup hook (DKG-06), v24`;
  - append `· LQ4 design §3.4 contract: members accesses at Core's points (§3.4.1), null session before Init, GETDATA phase windows, fully connected nodes only in ConnSnapshot, dkgdb fail-stop`.
- **Deps cell:** replace `LQ4b, QM1a, P7` with `LQ4b-1, QM1a, P7`. LQ4b-2 then runs beside DK1 (m5).
- **Acceptance cell:** replace `(mixed B, alternating, all-Rust; both pins)` with `(mixed B, alternating, all-Rust; v24 pin)`.

**New row BS11c**, after BS11b (PLAN:525):
`| BS11c | dash-pkc Core-style `core::sign(sk: &BlsSkValue, hash, flag)` (CBLSSecretKey::Sign) for both flags (LQ4 P-1, DEC-616 #9) | M5 | base-sdk pkc | BS11b | S | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-CON-30 | `bls_oracle/sign.json5` rows under both flags; `just comb` per commit · BG: — |`

**New row O2a-DKG**, after O2a-LQ1 (PLAN:650):
`| O2a-DKG | Oracle follow-up for LQ4 (design §4.2): `GetAllQuorumMembers` members hook (inject + count, O2a-LQ1 MEMBERS trace format), recorded decisions at MakeNewKey/InitEncrypt/GetRandBool/GetRand<int>(5), dkgdb write trace; `oracle_dkg_preverify`, `oracle_dkg_batch_sigs`, `oracle_dkg_session_open/step/close`, `oracle_dkg_finalize`; ABI bump | M5 | Core oracle-shim branches; dash-oracle-sys | O2a-LQ1 | M | LLMQ | — | **impl O-m; rev Sol** | NFR-CEQ-04 | shim selftests; the 8 BS12 sessions reproduced from recorded decisions; access counts for a 3-member round equal LQ4 §3.4.1 · BG: — |`

**P-4.** BL-475 (DEC-619): a dash-evo pre-slice by the LQ4 implementer before LQ4b-1, reviewed Opus-h. It stays in LQ4b's Deps cell as above.

The DEC-616 #10 Pippenger item stays in BACKLOG; no PLAN change.

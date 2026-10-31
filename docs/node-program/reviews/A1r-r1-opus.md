# A1r design review, round 1 (Opus)

- **Reviewed:** `designs/A1-architecture.md` (reconciled, 2,188 lines, 2026-10-08 04:40) and `PLAN.md` (v1, 683 lines, 04:39), with `CHARTER.md`, `DECISIONS.md` (DEC-01…17), `PRD.md` (NFR sections), `research/R4-*` (main report and appendix E) as context.
- **Reviewer:** Opus 5.5, independent design-review sub-agent. I did not write any of the A1 inputs.
- **Verdict:** **APPROVE-WITH-CHANGES**: 1 blocker, 11 majors, 21 minors and 6 suggested cuts. The architecture stands. The blocker must be fixed in A1 before BS6, BS8 or BS11 are briefed. M-1 (embargo) is time-sensitive.

> Embargo: this review refers to the embargoed issue only as "EMBARGO-1" (its R4 section reference was replaced by the codename in A1-rev1) and does not restate it.

## What I did

1. **Read everything.** I read A1 and PLAN in full, then CHARTER, DECISIONS, PROCESS, STATUS and BACKLOG, R4 §0–§2 and R4-appendix E, and the PRD's NFR-CEQ, NFR-FP and NFR-PERF rows.
2. **Checked claims against code.** Three independent verification passes read the code directly:
   - Dash Core `develop` @ `15db478750`, plus tags `v23.1.8` and `v24.0.0-rc.3`: ConnectBlock, flush and replay, undo, BLS wrapper semantics, per-object hash sources, MNAUTH, DKG criteria, sporks, init soft-sets, InstantSend gates, ChainLock scheduling, the functional test framework.
   - base-sdk `develop` @ `33fcc59`.
   - rust-dashcore `dev`, `node/integration` and the `node/*` branches.

   I added my own spot checks with `git grep` on the tags. Every `file:line` below is Core develop unless marked otherwise.
3. **Recomputed the critical path** from PLAN §4's dependencies and sizes.
4. **Checked operations, read-only:** agentbox capacity and load, git remotes, and where the embargo material actually lives.

Nothing was modified or committed.

## What holds up (verified)

- **Single atomic `chainstate/` DB.** This is a real improvement on Core, where coins and evodb are separate LevelDBs committed one after the other without sync (`validation.cpp:3090-3095`, `evodb.h:236`). It removes the "Found EvoDB inconsistency, you must reindex" failure class (`validation.cpp:1924-1930`). M-4 lists the details that still need fixing.
- **ConnectBlock order is correct:**
  - `ProcessSpecialTxsInBlock` runs before the UTXO loop (`:2664` vs `:2687`);
  - BIP34 applies only before DIP3 (`:4577-4584`);
  - `bad-txns-oversize` is a consensus `>` from DIP0001 (`:382`);
  - `bad-cb-amount`/`bad-cb-payee` are `BLOCK_RESULT_UNSET`, yet still `FAILED_VALID` through `InvalidBlockFound` (`:2845-2862`, `:3432`).
- **Rotation snapshots are never erased on disconnect.** Confirmed: there is no `Erase` of `llmq_S` anywhere, and undo actually re-stores them (`blockprocessor.cpp:523`).
- **Datadir names are forced by the framework.** Verified: the cache keeps `chainstate blocks indexes evodb llmq` (`test_framework.py:1015`), and `copy_datadir` copies `blocks chainstate evodb llmq` (`util.py:491-502`).
- **The base-sdk divergences (A1 §1.4 rows 1–6, §0.2) are all real at `33fcc59`.** One nuance: 103 and 105 decode as `Unknown(n)` rather than failing.
- **Thread caps match Core:** `-par` at 15 (`validation.h:76-78`); `-parbls` at 33, with the BLS worker at `clamp(hw/2,1,4)`.
- **Legacy-scheme BLS is never batch-verified (D32).** This is sound.
- **ChainHelper (D21)** mirrors `CChainstateHelper` (`evo/chainhelper.h:43-89`).
- **Honest declared shortfalls:** ECDSA per core, disk at parity, RSS not ½, and protocol-bound DKG and CoinJoin wall clock.

## Findings

Ranked most-severe first. Each finding gives the location, the problem with evidence, and the recommended change.

### Blockers

#### B-1. BLS fields as "scheme-agnostic byte bags", plus BS11's wrapper semantics, diverge from Core's decode; a null quorum commitment can split consensus

**Location:**
- A1 §1.1 rule R4 (last sentence), §1.4 (Rule DDE and its table), §1.7 BS6/BS8/BS11(3), §2.5 `llmq::commitment`, §4.7 (QcTx operators)
- PLAN BS6, BS8, BS11, LQ2

**What Core does** (identical in v23.1.8 for these paths):

- **`CBLSWrapper::Unserialize`** (`bls/bls.h:184-203`) has four outcomes:
  - bytes canonical in the chosen scheme → a valid object;
  - all zeros → invalid;
  - bytes canonical **only in the opposite scheme** → `Reset()`. The object is invalid and **re-serializes as zeros** (`:133-139`);
  - bytes invalid in both schemes, **including the identity/infinity encoding** → **throws** "malleable BLS object". `SetBytes` resets the identity point (`:100-122`), so the malleability check fails in both schemes. R4-E §4.3 says the same.
- **The throw becomes a block rejection.** `GetTxPayload` (`evo/specialtx.h:17-28`) turns the throw into `nullopt`, which gives **`bad-qc-payload`** (`llmq/commitment.cpp:242-246`, `blockprocessor.cpp:574-578`).
- **Nullness is defined by validity.** `CFinalCommitment::IsNull` tests `IsValid()` on `quorumPublicKey`, `quorumSig` and `membersSig` (`commitment.h:118-131`), and `VerifyNull` is IsNull plus sizes (`commitment.cpp:211-224`). Null commitments are mandatory in every mining-window block.

**Why A1 splits from Core:**

- **A1 makes these fields lazy and too lenient.** It specifies BS11(3) "decoded infinity → invalid", not a decode error. BS6 turns the `FinalCommitment` fields into byte bags, which defers decoding.
- **Rust accepts what Core rejects.** Built as written, a null commitment whose `quorumPublicKey` holds the identity encoding is `bad-qc-payload` (block invalid) in Core. In Rust it is an invalid object, so `IsNull` holds and the commitment is a valid null. **A miner can split the network.**
- **Core accepts what a naive Rust check rejects.** The natural byte-bag test for null is "all bytes zero". That rejects commitments Core accepts, because Core turns opposite-scheme-canonical bytes into zeros and then treats the commitment as null.

**The same three-outcome decode applies to every non-lazy field:**

| Field | Scheme source |
|---|---|
| `FinalCommitment` × 3 | version-derived |
| CbTx `bestCLSignature` | global |
| AssetUnlock `quorumSig` | global (`assetlocktx.h:108-117`) |
| ProUpServ / ProUpRev `sig` | version-derived |
| `ChainLockSig` | global |
| MNAUTH `sig` | global |
| DKG vvec, `quorumPublicKey`, IES ephemeral key | global |

Lazy fields keep raw bytes and never fail to decode: ProReg/ProUpReg operator key, recsig, isdlock, sig shares, and the ProUpSharedReg key.

**Hashes also differ:**
- The CLSIG inv hash is `SerializeHash(clsig)` *after decode* (`net_processing.cpp:5792-5794`; v23.1.8 `:5669`).
- `qfcommit` and the mineable-commitment key are `SerializeHash(qc)` (`blockprocessor.cpp:115,452,504,842`).

Both hash Core's zeroed re-serialization. Re-emitting the wire bytes gives different inv hashes, so dedup and relay differ.

**P2P decoding depends on node state:** global-scheme fields decode under the tip's scheme, but BS4's `ProtoCtx` carries only the protocol version.

**Recommendation:**
1. Replace "byte bags" with two base-sdk types:
   - **`BlsLazy<G>`**: raw bytes plus scheme. It never fails decode and reproduces `CBLSLazyWrapper`, including zeros after a failed `Get()` (`bls.h:492-512`).
   - **`BlsEager<G>`**: decoded at payload or message decode with a `SchemeSource::{Version(v), Global(BlsCtx), AlwaysBasic}`. It has exactly Core's three outcomes: Valid, Invalid (re-encodes as zeros), or DecodeError.
2. Correct BS11(3): identity bytes, and bytes non-canonical in both schemes, are **decode errors**.
3. Add a per-object table to A1 §1.4 covering field, lazy/eager, scheme source, and hash source (wire bytes vs Core re-serialization). The verification table behind this review covers recsig, clsig, isdlock, sig shares, `FinalCommitment`, CbTx, MNAUTH, DSQ/DSTX, DKG, governance and ProTx.
4. Carry `BlsCtx` in `ProtoCtx` (M-3).
5. Add tests:
   - **L1** targets `diff_payload_{2,4,5,6,9}`, `diff_clsig` and `diff_p2p_qfcommit`, seeded per field with identity, opposite-scheme, off-curve-legacy and random encodings;
   - **L3** BLS-encoding operators for QcTx (null and non-null), AssetUnlock and ProUpServ/ProUpRev. §4.7 has these only for CbTx;
   - **Quirks:** "eager BLS decode outcomes" and "null commitment ⇔ !IsValid".

### Majors

#### M-1. The embargo procedure leaks: the codename describes the issue, and the mechanics are git-tracked and synced to GitHub through agent memory

**Location:** PLAN §7.1/7.3/7.4/7.6; A1 header, §0.3, §1.3, §1.4 row 8, §2.4; the STATUS banner; workspace memory.

**Problems:**
- **The codename is self-describing.** PLAN §7.1 requires it in "every document, commit message, review, brief, STATUS line, test name and code comment", but it describes the issue. Together with A1 §1.4 row 8 and the §2.4 note, a reader can reconstruct EMBARGO-1 without opening R4. The STATUS banner is more explicit still.
- **The mechanics are not confined where §7.1 says.** §7.1 says they live "nowhere except R4, its appendix repro and `~/private/embargo/`". In fact:
  - R4 is git-tracked on `node/integration`.
  - The rust-dashcore workspace memory holds a mechanism-level note: its MEMORY.md entry starts "EMBARGO:".
  - **That note is committed on `PastaPastaPasta/agent-config` `origin/main`.** Verified: `git show origin/main:memory/workspace-rust-dashcore/<note>` resolves, and `origin/main` was at `ca37fd5` at review time. agent-config pushes every 15 minutes.
  - The repo is private and pasta-owned, but this is content leaving the box. §7.4 forbids that, and §7 does not mention this channel at all.
- **Cross-provider review sends R4 off the box.** PROCESS makes GPT-6.1-Sol (another provider) the primary reviewer, and this A1r round gives it R4 including the EMBARGO-1 section. §7.6 only says reviewers must not quote mechanics.

**Recommendation (manager and pasta; I changed nothing):**
1. Move the EMBARGO-1 body to `~/private/embargo/` and leave a stub.
2. Switch to an opaque codename (e.g. "EMB-1"). Keep the code markers. Scrub A1 §0.3, §1.3, §1.4 row 8, §2.4 and the STATUS banner.
3. Reduce the memory note to an opaque pointer. Pasta decides whether to purge it from agent-config history. Add an embargo denylist to agent-config's pre-commit hook.
4. Extend PLAN §7 to cover:
   - agent memory and agent-config sync;
   - T3 thread titles and delegated prompts;
   - cross-provider reviewers: a brief template that excludes the EMBARGO-1 material, with the private path given only to the reviewers who need it;
   - B1 reports.

#### M-2. "Indistinguishable from the pinned release" is modelled only for the handshake; v23.1.8 differs from v24 in non-deployment-gated code, and A1 hard-codes v24

**Location:**
- A1 §0.10, §2.14 `ProtocolProfile` ("Consensus rules come from ChainParams deployments, never from the profile"), §2.6 (Complain), §2.10 step 13, §2.15 (sporks, MN mode, IS), §1.3 `Tx`, D42
- PLAN SW-V24, AM1, DK1, RL5

**Evidence** (v23.1.8 vs develop/rc.3):

- **DKG bad-connection criteria and the minimum MN protocol version.**
  - `MIN_MASTERNODE_PROTO_VERSION` is **70240 in v23.1.8** and 70242 in v24 (`version.h:23`).
  - The "no tx relay" and "no `NODE_COMPACT_FILTERS`" criteria exist only in v24 (`active/dkgsession.cpp:226-290`, added in `1552688094`).
  - A1 §2.6 hard-codes `version < 70242`. **A Rust MN on mainnet would mark every v23.1.8 member `badConnection` and complain about the whole quorum.**
- **InstantSend conflict gate.**
  - v23.1.8: `RejectConflictingBlocks()` = `mn_sync.IsBlockchainSynced() && SPORK_3` (`instantsend.cpp:818-828`). It is consulted by ConnectBlock (`validation.cpp:2607`), the miner (`miner.cpp:382`) and the CL signer (`signing.cpp:129`).
  - v24: `!IsInitialBlockDownload()` in ConnectBlock (`:2800-2817`), and the miner and signer use `IsInstantSendEnabled()`.
  - A1 implements only the v24 gate.
- **Sporks.** v23.1.8 also has SPORK_3 (10002) and SPORK_9 (10008); v24 removed both (`afcf3d148e`). A1 §2.15 lists v24's set.
- **MN init.** The v24-only InitErrors (blocksonly, peerblockfilters, maxuploadtarget, relay policy) are absent in v23.1.8.
- **AssetUnlock stable txid.**
  - In v24 it is **not** gated by activation (`primitives/transaction.cpp:93,117`) and it is absent in v23.1.8.
  - Both releases reject AU v2 on mainnet, but with different strings (`bad-assetunlocktx-version-2` vs `bad-assetunlocktx-version`, `assetlocktx.cpp:207-211`).
  - The txid, the merkle root, and therefore the failure class differ: MUTATED vs CONSENSUS.
- **RPC surface.** v23.1.8 has no `getdeploymentinfo`, `getcreditpoolinfo` or `getchainlockbyheight`.

**Recommendation:**
1. Generalise `ProtocolProfile` into a **`ReleaseProfile`** per network (DEC-05). Thread it into:
   - P2P;
   - ChainHelper's IS gate;
   - the miner's package test;
   - the CL and IS signers;
   - the DKG criteria and minimum protocol version;
   - the spork table;
   - MN init checks;
   - the AU txid rule;
   - reject strings that differ;
   - RPC availability (`dash-rpc-spec` is already per pin).
2. Add a Wave 1 task **PIN-DELTA** (S–M, Opus xhigh): diff v23.1.8 against v24.0.x over R4's consensus-relevant file list, and classify every delta as deployment-gated, profile-gated or irrelevant. Re-run it at every re-pin.
3. Tell pasta that a mainnet MN pilot needs either the v23.1.8 profile or mainnet's move to v24.

#### M-3. `BlsCtx` is defined "from the block being processed", but most consumers have no block

**Location:** A1 §1.1 R4, Appendix A (`BlsCtx`/`ConsensusCtx`), §2.12, §2.13, §2.15 MNAUTH.

**How Core decides the scheme:**
- The process flag is legacy until block V19Height−1 has been connected. It flips once `DeploymentActiveAfter(pindex, V19)` (`specialtxman.cpp:984-990`).
- At startup it is set from the tip (`node/chainstate.cpp:429-461`).
- It flips back on DisconnectTip and undo (`validation.cpp:3270`, `specialtxman.cpp:1010-1014`).

So everything processed outside ConnectBlock uses `!ActiveAfter(tip, V19)` *at processing time*:
- clsig;
- the default flag of lazy isdlock and recsig objects;
- sig shares;
- DKG messages and the IES ephemeral key;
- mempool `CheckSpecialTx` BLS signatures;
- the miner's CbTx;
- the operator-pubkey bytes inside the MNAUTH sign hash (`mnauth.cpp:36-45,93-98`; the MNAUTH sig itself is always basic).

A1 contradicts itself here: §1.1 lists MNAUTH as context-dependent, while §2.15 says "basic". Each is half right. This matters wherever V19 activates at low heights (regtest, devnet, `feature_dip3_v19.py`, DTF), i.e. in L4 and L5.

**Recommendation:**
1. Add `BlsCtx::for_tip(view)` and a per-call-site table:
   - block validation: per block;
   - mempool and GBT: `for_tip`;
   - P2P intake: `for_tip`, re-evaluated after each tip change;
   - DKG: from the quorum base (`dkgsession.cpp:34`);
   - MNAUTH: pubkey bytes `for_tip`, sig basic.
2. Add a reorg-across-V19 scenario *with MN traffic* to the M4 and M7 gates.

#### M-4. Split-batch crash recovery leaves evo state behind the coins; the fsync order is ambiguous; a process-kill harness can't detect either

**Location:** A1 §2.10 "Crash recovery", §2.11, §3.5; PLAN CH2 acceptance.

**Evo stays at the old tip after recovery.**
- §3.5 orders the sub-batches as: `head_blocks`, then the coin sub-batches, then evo/index/mined deltas together with `meta.tip`.
- A crash between them leaves coins partly at `new_tip` and evo at `old_tip`.
- Recovery replays only the coins ("evo is still consistent at old_tip"), so it ends with coins at `new_tip` and evo, mined commitments, rotation snapshots and the credit pool still at `old_tip`. Nothing in the text rolls them forward.
- Core runs `ProcessSpecialTxsInBlock(fJustCheck=false, fCheckCbTxMerkleRoots=false)` inside `RollforwardBlock` (`validation.cpp:5178-5214`), after `VerifyBestBlock` (`:5252-5257`).

**The fsync order contradicts itself.**
- §2.11 puts the barrier "before each chainstate flush". §3.5 commits the batch with `sync=false` and runs the barrier (fsync blk/rev, then WAL sync) afterwards.
- With `sync=false`, a background memtable flush can make the batch durable before blk/rev are fsynced. After a power loss, `block_index` (with `HAVE_DATA` recorded at pre-reserved offsets) can point at bytes that never reached disk.
- Core fsyncs the block files before it writes the index (`:3056-3063`).

**SIGKILL can't test either problem.** It never loses page-cache data.

**Recommendation:**
1. Write every non-coin delta into the **first** sub-batch together with `head_blocks`. Recovery then really is coins-only. If you keep the current order instead, port `RollforwardBlock` including special-tx processing.
2. State the order explicitly:
   1. fsync blk/rev up to every position the batch references;
   2. commit;
   3. WAL sync at durability points.
3. Make CH2's harness a true power-loss test (LazyFS or dm-log-writes prefix replay) on `regtest-40000`, and keep the kill test as a smoke test.
4. Require `llmq/`, `indexes/` and wallets to be idempotent under re-delivered block events after a rollback, each with its own locator, and add them to the harness.

#### M-5. Handing blocks and txs off the `PeerManager` actor breaks the sequential semantics the actor was chosen to keep; parallel ATMP ordering is underspecified

**Location:** A1 §2.14 ("Heavy work never runs on the actor"), §2.12, §3.7, §4.10; PLAN P2/P3/P5/MP1a/O13b.

**What Core does:**
- `msghand` runs ProcessNewBlock and ProcessNewBlockHeaders **synchronously**.
- It runs ATMP under `cs_main` + `mempool.cs` with inline, single-threaded scripts (`validation.cpp:1421-1422`, `:1178-1197`).
- Every later message, from any peer, sees the result.

**What the A1 actor does instead:** it moves on while validation and ATMP run elsewhere. That produces:
- **Stale answers to other peers.** `getheaders`, `getdata` or `mempool` from peer B right after A's block or tx can be answered from the old state. Two colluding probes make this a deterministic fingerprint.
- **Reordered replies to the same peer.** Its own `tx`, then `mempool`/`getdata`/`ping`, are answered out of order.
- **Child-before-parent races.** These change orphanage contents, relay, the `getrawmempool` sequence and the ZMQ `sequence` topic.
- **Wrong limit checks.** Limit and conflict checks are computed against a view that lacks earlier txs still in flight.

**Recommendation:**
1. Keep parallelism *inside* a message: decode and hash in reader tasks; scripts and BLS on pools inside validation and ATMP.
2. Have the actor **await** messages whose effects later messages can observe: the block family, headers, tx, and relay-changing overlay objects.
3. If PF1 shows the actor is a bottleneck, pipeline only provably independent messages. Commit in arrival order with a full policy re-check.
4. Add L6 cross-peer ordering probes, and a `mempool_model` property "parallel ≡ serial in arrival order".
5. Restate §3.9 row 7 to match.

#### M-6. Event-driven overlay processing is visible on the wire, and the LLMQ latency rows overstate the gain

**Location:** A1 §2.15 "Improvements (never fingerprint-visible)" for quorums and locks, §3.6(6), §3.9 rows 15–17; PLAN SG1b, LK1b/LK1c.

**Problems:**
- **Earlier relays are measurable.** Core verifies pending isdlocks in the `isman` worker (`instantsend/net_instantsend.cpp:424-440`, sleeping `WORK_THREAD_SLEEP_INTERVAL` when idle) and relays them only after verification, from `ProcessInstantSendLock` (`:358-419`). Sig shares are recovered in the sig-shares worker loop (`llmq/net_signing.cpp:294`, 100 ms), and the recovered sig is relayed afterwards (`:207`). Verifying and recovering on arrival relays isdlocks to **every** peer, and recsigs to MN peers, up to ~100 ms earlier. A probe can measure that distribution (NFR-FP-02/03). A1 preserves only the sig-share *announcement* cadence.
- **Core already enforces ChainLocks immediately.** `ProcessNewChainLock` schedules `EnforceBestChainLock` with a **0 s delay** when the block is known (`chainlock/handler.cpp:170-176`). The 5 s timer only matters when the header arrives after the clsig. So row 17's "plus 5 s enforcement polling" and §2.15's "no 5 s wait" are mostly not gains.

**Recommendation:**
1. Use **tick-aligned output**: do the work early, but release externally visible outputs on Core's worker-tick boundaries via `NodeClock`. These are the isdlock, recsig, clsig and qfcommit invs, and DKG messages.
2. Make `-llmqfastsend` benchmark-only.
3. Restate rows 15–17 as node-local CPU ≥ 2× with end-to-end ≈ 1.0×, a declared shortfall under NFR-FP.
4. Add isdlock, recsig and clsig relay latency to the §4.10 KS timer list.

#### M-7. Differential fuzzing stops at single functions; the hardest state has no coverage-guided differential target, and "the shim adds no logic" is unrealistic

**Location:** A1 §4.4 (C ABI v2, "The shim adds no logic"), §4.7; PLAN O2 (L), EV2b, LQ1b.

**Problem:** these entry points need chainstate-resident objects:
- `oracle_dml_apply`;
- `oracle_quorum_members` (with rotation and snapshots);
- `oracle_credit_pool_step`;
- `oracle_versionbits_state`.

The objects they need:
- `CBlockIndex` chains with versionbits caches for `DeploymentActiveAt`;
- `CDeterministicMNManager` over `CEvoDB`;
- `CQuorumSnapshotManager`, which does raw evodb writes (`snapshot.cpp:320-329`);
- the chain helper;
- the global `Params()`.

None of that is a thin ABI adapter. R4 §2.2 itself rates these "P2 … the replay covers them better". The charter asks for differential *fuzzing* of consensus code. For the XL areas (DML, PoSe, rotation and credit pool across blocks and reorgs), the plan has only replay (which proves acceptance only), about 40 hand-written L3 operators, and the single-step `diff_dml_apply`.

**Recommendation:**
1. Split O2 into:
   - **O2a** (stateless entry points, L);
   - **O2b**, a **`diff_chain`** harness (D, L, Opus xhigh). It links a Core regtest test chainstate (in-memory DBs, mocktime, low activation heights) into the shim, and feeds Core and `dashd-chainstate` (in-memory engine) the same fuzzer-generated block sequences:
     - ProTx churn and collateral spends;
     - QcTx with valid and invalid members;
     - asset lock and unlock;
     - MNHF;
     - invalid blocks and reorgs.

     It compares the per-block StateDigest, the verdict and the reject string.
2. Acknowledge in A1 that the shim contains reviewed adapter logic.
3. Note for `oracle_p2p_decode`: Core has no standalone decoder. Read semantics live inside each `ProcessMessage` branch (trailing bytes, optional `version` fields), so that adapter is logic too.

#### M-8. The mainnet golden and digest plan can't run on v23.1.8

**Location:** A1 §4.6 ("Mainnet reference" and the RPC golden table), D40; PLAN O7, O8, M0 exit 6.

**Problem:**
- v23.1.8 has no `getdeploymentinfo`, `getcreditpoolinfo` or `getchainlockbyheight`. I checked with `git grep` on the tag; all three exist in rc.3.
- v23.1.8 has no `evo/snapshot.{h,cpp}`, so neither `EvoSnapshot` nor `CanonicalMNListHash`. The `node/state-digest-v23.1` branch would need a backport of that whole codec.
- R4 §2.3.2 already says to extract mainnet goldens with a v24 binary, because mainnet consensus is identical while V24 and evo_shares are NEVER_ACTIVE.

**Recommendation:**
1. Take mainnet consensus goldens and `-statedigestlog` digests from the v24 oracle.
2. Use the v23.1.8 reference only for the real per-pin deltas (M-2: fingerprint baseline, RPC shapes, node-local gates, MN behaviour) and for sampled cross-checks.
3. Drop `state-digest-v23.1`.

#### M-9. PLAN §3's critical path doesn't match the task graph, and the real path overruns the milestone windows by about 40–60%

**Location:** PLAN §1 (dates), §3; A1 §5.

**Method:** I computed from PLAN §4's dependencies and sizes at midpoints (S=1, M=1.5, L=3 agent-weeks), with **no** design, review or fix latency.

**Findings:**
- **The diagram's edge `CH3b → EV1` doesn't exist.** EV1 depends only on BS6 and CS3.
- **The real M4 path** is BS3 → BS4 → BS6 → EV1 → EV2a → EV2b → LQ1a → LQ1b → LQ2 → LQ3 → CH7b = **26.5 weeks**. Every task at its lower bound still gives 17.5. Target: wk 20. Eight of these tasks carry D/D2 design steps and CON double review.
- **M2:** BS3 → BS4 → BS6 → BS8 → P2 = 11.5 weeks plus a D2 design. Target: wk 10.
- **M7:** LQ3 (23.5) → P6 (26.5) → AM1 (29.5) → P7 (31) → QM1 (34) → DK1 (37) / SG1b (38.5), then a 2-week devnet soak and a 30-day testnet soak ⇒ **≈ wk 45**. Target: wk 34. M8 lands around wk 48–50.
- **Headcount is not the constraint.** Tasks total ≈ 262 agent-weeks against ≈ 386 staffed implementer-weeks. The serial chain is the constraint.

**Recommendation:** re-baseline, or shorten the chain:
- (a) **Decouple the MN overlay from Done-1.** AM1, QM1, DK1 and SG1 need evo/LLMQ correct on regtest, not full mainnet replay. Gate them on EV2b/LQ3 plus regtest L4.
- (b) **Split P6** into P6a (NetHandler registry and spork/clsig/isdlock/govobj relay) and P6b (mnlistdiff/qrinfo/cfilters serving). AM1 needs only P6a.
- (c) **Start LQ1a earlier:** on EV2a's model with the in-memory `EvoStore`.
- (d) **Land BS6 types-first**, so EV1 can start.
- (e) **Fix the §3 diagram.**

#### M-10. Dependency errors make milestone exits unreachable as written

**Location:** PLAN §1 (M1, M4), §4 (LK1a, O15, P6, SP1, BS14, BS17); STATUS (V24a).

1. **M4 exit 3** (live ChainLock enforcement, 7-day shadow) needs:
   - `clsig` intake and the NetHandler registry, which are P6 at M5;
   - on testnet, SPORK_19 state, which is SP1 at M5. CL enforcement is gated by SPORK_19 (R4-E §5.2).

   LK1a depends on neither.
2. **The M4 shadow node is weaker than dashd at the tip.** It has neither IS (LK1b) nor governance (GV1b), so it follows weaker rules than dashd there. Either say so or pull the LK1b node role into M4.
3. **O15** compares RPC output (`getbestchainlock`, `quorum list`, `protx diff`) but doesn't depend on RP1/RP2.
4. **BS14 and BS17 don't belong in M1's exit.** Nothing needs them before M6. BS17 duplicates the running **V24a** task. Its stated purpose (R5 R-1, production SPV wallets) can't be served from a local branch:
   - the desktop pins `dashpay/rust-dashcore` from GitHub (`dashwallet-desktop/rust/Cargo.toml:33-37`);
   - DEC-09 keeps node work local.

   Move BS14/BS17 to M6 or SW-DESK. Suggest that pasta push the non-embargoed upstream PRs #1136–#1138, which is where the production fix actually lives.
5. **O1 is mis-ranked.** BS4's L1 acceptance and every other L1 gate need O1, which is #7 in Wave 1. Make it #2.

#### M-11. One shared 32-vCPU box can't host this verification and benchmark load and still meet NFR-PERF-02

**Location:** PLAN §2, §5 (SW-OPS, SW-PERF), §8; A1 §3.11, §4.6 (cost); PRD NFR-PERF-02(a).

**Current state:** agentbox has 2 × 16 cores, 157 GB of RAM and **one** 2 TB disk. At Phase 0 the load average is already ≈ 32, with four dashd processes using ≈ 12 GB RSS.

**What the plan adds on top:**
- a third mainnet dashd (the v23.1.8 reference);
- 16-core × 8 h nightly fuzzing;
- nightly testnet and weekly mainnet replays (8–16 vCPU each);
- nightly functional passlists (about 265 scripts × several mixes);
- a ≥ 2-week devnet soak with 20 MNs and 8 evonodes. Each evonode also runs Tenderdash and Drive containers;
- 6–10 implementers building at `CARGO_BUILD_JOBS=12`;
- per-PR bench guards at p < 0.05 with a 1% effect size.

NFR-PERF-02(a) says "nothing else running". NUMA pinning separates cores, but not the disk, memory bandwidth or page cache, and those dominate the IBD, reindex and flush metrics.

**Recommendation:**
1. Ask pasta for a dedicated benchmark host (add it as §8 Q4). It blocks the "never slower" evidence, not development.
2. Until then, per-PR guards use deterministic counters (`perf stat` instructions, allocation counts) and isolated criterion runs. Wall-clock scenarios run only in windows with everything else, including the soak, paused.
3. Add a capacity table to SW-OPS.
4. Consider a separate VM for the devnet soak.

### Minor

- **m-1. assumevalid skip set (A1 §2.10 step 6, Appendix A `EvoCtx`).**
  - Core skips CheckCbTx **entirely** (presence, version and height; `specialtxman.cpp:817,835`), the roots and best-CL, the credit-pool balance **and unlock limit** (`:884-890`), ProTx sigs (`:872`) and QC BLS sigs (`blockprocessor.cpp:267-283`).
  - Core **still verifies** the asset-unlock quorum sig (`assetlocktx.cpp:231`) and the MNHF sig (`mnhftx.cpp:152`).
  - `check_sigs` must not gate those two. Add a quirk test.
- **m-2. Replay perf target (A1 §4.6).** "≤ dashd `-reindex-chainstate -assumevalid=0`" compares unequal work: `-reindex-chainstate` skips `ContextualCheckBlock*` (`validation.cpp:4522-4527`; R4 §2.3.3). Compare with `-reindex`. CH5 must mirror that skip for parity.
- **m-3. Undo semantics (A1 §2.9).** Add a per-CF connect/disconnect table:
  - `mn_diff` kept on disconnect (`deterministicmns.cpp:784-817`);
  - `llmq_snapshot` kept, and also written by TestBlockValidity and VerifyDB in Core;
  - `llmq_mined` and `llmq_mined_h` erased;
  - `mnhf` keyed by hash;
  - credit pool derived.

  Also specify reorg handling for governance triggers, the IS mined index, archives and recsig truncation, and the DSTX confirmed height. None of these is described today.
- **m-4. ChainHelper threading.** ChainHelper back ends must read immutable snapshots and never call into validation (a deadlock risk with `ChainCommand` replies). Add this to §3.1's rules.
- **m-5. Regtest auto-import and func-compat (A1 §2.9, §4.8).**
  - Also move aside Core's `indexes/`, which `dynamically_initialize_datadir` copies (`test_framework.py:635-642`).
  - DTF never passes per-node binaries (`start_masternodes` `:1786`, `add_dynamically_node` `:598-599`, `create_simple_node`), so func-compat must patch them. It is not "only a runner flag".
  - BitcoinTestFramework builds the cache with one binary and copies it to all nodes (`:943-1022`). With mixed nodes, build it with dashd.
- **m-6. Copying a live RocksDB.** `DisableFileDeletions` doesn't make a copy of a running RocksDB consistent: MANIFEST and CURRENT can reference SSTs created after the copy listed the directory. On regtest, after `gettxoutsetinfo`, also pause background work. Test it nightly; it matters whenever rdashd is node 0.
- **m-7. MN mode (A1 §2.15).** Core soft-sets only `-disablewallet`, `-peerblockfilters` and `-blockfilterindex` (`init.cpp:1094-1105`). The rest are InitErrors, and the set depends on the pin (M-2). "Forces" is wrong, and the difference is observable (`feature_masternode_params*`).
- **m-8. The floating-point lint is too broad.** These Core-mirrored f64 sites must match exactly:
  - the governance rate check;
  - the superblock time estimate (·2.62·60);
  - the DKG sleep factor;
  - the fee estimator;
  - CoinJoin fees;
  - RPC difficulty and `verificationprogress`.

  Ban libm calls outside `fp`, and allowlist plain IEEE arithmetic at anchored sites. DIFF-RPC float text must follow UniValue's formatting, not serde_json's shortest representation (please verify the exact precision rule).
- **m-9. `no_std` conflicts with A1's own types.** `Tx` and `Dmn` use `OnceLock`, which is std-only, and are shared through `Arc` across threads. See X-1.
- **m-10. GBT template cache.** A 100 ms-debounced rebuild makes "template equals dashd's for the same mempool" ambiguous and adds CPU under load (metrics 7 and 12). Rebuild synchronously when `mempool_sequence` has changed. Say that "≥ 5× warm" holds only when idle.
- **m-11. Startup PoW check.** Core's index load checks PoW against the **stored** hash; it doesn't recompute X11. Do the same (metric 9).
- **m-12. Recorded decisions are vacuous.** Decisions recorded during the oracle's `-reindex-chainstate` are all IBD/NoCheck/no-CL, so `ReplayChainHelper` tests nothing. Record on the **live** oracles from O8 onward.
- **m-13. Decode errors from reader tasks** must reach the actor in message order. Otherwise misbehaviour and disconnect timing differ (behaviours 10–12).
- **m-14. txindex size.** "About 3× smaller" is likely ≈ 2× (Core's 33-byte key plus a 3-VARINT value, vs 8 + 12 bytes). Quote a range until B1 measures it.
- **m-15. `Tx` wrapper comment.** It omits the stable-txid preconditions (type 9 and special version; R4 §1.3) and the rule's per-pin status (M-2).
- **m-16. CJ0.** `is_collateral_valid` is an ATMP test-accept, which can't live in pure `dash-coinjoin`. Use a trait implemented in `dashd-coinjoin`.
- **m-17. BS4's divergence list comes from a single audit.** Rule DDE needs a systematic per-type sweep:
  - compact-size canonicality and `MAX_SIZE`;
  - vector caps and enum ranges;
  - trailing bytes: most P2P messages ignore them, but `GetTxPayload` requires `ds.empty()`;
  - `version` optional-field truncation.
- **m-18. Hash-before-decode bools.** DSQ's `fReady` is a bool hashed after re-serialization (`coinjoin.cpp:47`), so `CoreBool` must canonicalise to `0x01`. Covered by B-1's hash-source table.
- **m-19. STATUS and PLAN drift.** PLAN's "already running" omits V24a, and BS3, O1, O4 and ST1 already have branches. Reconcile them.
- **m-20. SPV-as-consumer is weaker evidence than A1 §4.11 implies.**
  - dash-spv doesn't verify mnlistdiff merkle proofs or CbTx roots (R4-E §11.4).
  - Since #1080 it marks the 24 stale v1 quorums `Skipped` rather than validating them (`dash/src/sml/llmq_type/network.rs:150`).
  - Treat it as a smoke test. EV4's byte-identity against `getmnlistd` is the real check.
- **m-21. #264's v2 transport (A1 §2.14).**
  - base-sdk already has V2 framing and short IDs (`p2p_core/src/bip324.rs`, `short_id.rs`). Reuse them in `dash-p2p-transport` instead of adding a third copy.
  - #264's `bip324` crate takes the network magic and the fixed `bitcoin_v2_shared_secret` prefix, which matches Core (`bip324.cpp:38`). The "vendor a patched copy" contingency can go.
  - Small correction: `conformance/` lives on `node/bs2-conformance-harness`, not yet on `node/integration`.

### Over-engineering to cut

- **X-1. Drop `no_std + alloc` for the pure crates.** No target needs it, it already conflicts with A1's own `OnceLock`-based types (m-9), and it costs a CI job. Enforce purity with cargo-deny bans and clippy's disallowed lists.
- **X-2. Make StateDigest digest-first.** The per-block `-statedigestlog` (`CanonicalMNListHash` and the rest) from the patched v24 oracle catches any DMN divergence. Extract per-block `protx listdiff` and `masternode payments` goldens (≈ 3M RPC calls) only around a first mismatch. Keep MuHash and `getblockstats` per block.
- **X-3. Drop the LevelDB arm of ST1.** dashd in B1 is already the LevelDB baseline. Keep RocksDB vs MDBX on the churn trace.
- **X-4. Shrink BS17.** Full typed v24 shapes in `dashcore` have no in-program consumer before M6 (M-10.4). Keep the unknown-type passthrough and the AU v2 stable txid if BS2 or dash-spv's testnet sync need them.
- **X-5. Merge small crates.** Fold `dashd-sporks` into `dashd-masternode`, and `dashd-zmq` into `dashd-rpc` behind the `zmq` feature. That takes the count from 32 to 30.
- **X-6. Keep one shadow comparator.** Keep the external comparator (O15). The in-node tripwire needs only CL conflicts and the digest log, not a second `-shadowcompare` RPC client.

## Plan: missing tasks and the Wave 1 order

**Missing tasks:**
- PIN-DELTA audit and `ReleaseProfile` (M-2).
- A BLS decode-semantics design note, written before BS6, BS8 and BS11 (B-1).
- Power-loss fault-injection infrastructure (M-4).
- `diff_chain`, i.e. O2b (M-7).
- Embargo hygiene, done by the manager rather than an implementer (M-1).
- A benchmark host and capacity plan (M-11).
- Design items to add to existing tasks:
  - tick-aligned overlay output in SG1/LK1 (M-6);
  - actor sequencing in P2's D2 (M-5);
  - reorg semantics for governance, IS and DSTX in GV1b and LK1b (m-3).

**Wave 1, reordered:**
1. BS3.
2. **O1.**
3. **BS4a**: type-shape changes only, no BLS decisions. **BS4b** (fuzz evidence) follows once O1 lands.
4. **PIN-DELTA** (S).
5. **B-1 design note** (S).
6. BS6, types first.
7. BS5.
8. CS1.
9. CS0, after BS4a.
10. CS2.

ST1 and O4 stay as they are. O7 runs with the corrected golden source (M-8).

## Verification sufficiency against the charter bar

| Layer | Adequate for the charter? | Gaps (finding) |
|---|---|---|
| L0 vectors | yes | — |
| L1 function-level differential | yes for parsers and pure functions | BLS eager/lazy targets (B-1); the shim is adapter logic, not "no logic" (M-7) |
| L1 stateful differential fuzz | **missing** | `diff_chain` (M-7) |
| L2 replay | yes (acceptance) | golden source (M-8); recorded decisions vacuous (m-12); compare with `-reindex` (m-2) |
| L3 mutation | partial | BLS-encoding operators for QcTx/AU/ProUpServ (B-1) |
| L4 functional tests | yes, with work | func-compat scope (m-5); live RocksDB copy (m-6); V19-reorg MN scenario (M-3) |
| L5 mixed networks | yes | v23.1.8-profile MN behaviour (M-2) before any mainnet pilot |
| L6 fingerprint | partial | cross-peer ordering probes (M-5); overlay relay-latency KS (M-6); per-pin baseline (M-2) |
| Crash safety (FR-STO-03) | **insufficient** | real power-loss testing (M-4) |
| BENCH | **not credible on a shared box** | M-11 |

## Verdict

**APPROVE-WITH-CHANGES.**

The reconciled architecture is sound and well verified against Core in most places: the type stack, crate layering, the single-DB chainstate, the verification layers and the honest shortfalls. It should not be redone. Conditions:

1. **Before BS6, BS8 or BS11 are briefed:** fix **B-1** in A1 §1.1, §1.4, §1.7 and §4.7.
2. **Now, at the manager and pasta level:** act on **M-1** (embargo).
3. **Before the tasks they touch are briefed:** resolve M-2 to M-11 in A1 and PLAN:
   - M-2 and M-8 before O7, O8, DK1 and AM1;
   - M-3 before CS0, EV1 and LQ2;
   - M-4 before CH2;
   - M-5 before P2;
   - M-6 before SG1 and LK1;
   - M-7 before O2;
   - M-9, M-10 and M-11 at the next PLAN revision.

   The minors can be folded into the relevant briefs.

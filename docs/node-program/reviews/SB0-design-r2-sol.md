# SB0 design review, round 2 — Sol (DEC-92)

Reviewed `docs/node-program/designs/SB0-shared-base.md` **rev1 at d07a7d92b3c589394902071015ca317c381cb0d9**, together with `docs/node-program/tools/cpm_sb0_rev1.py`, in the detached checkout `/work/scratch/sb0-review-r2-sol/`. Read `briefs/REVIEW-COMMON.md` first. SB0 line references below are to that revision. Core references are the code read with `git show v24.0.0-rc.3:src/<path>`.

Applied DEC-104's rulings: no SPV-to-full import; checkpoint selection with forward sync, without backfill; commitment-verified keys rather than exact SPV membership; and an extension of #902's facade. The findings below concern the correctness of those contracts' realization, not requests to restore r1's more elaborate alternatives. Current SPV validation coverage is outside this review: PLAN §7 handling (CANDIDATE-SPV).

**Result: four major findings, two minor findings, no blockers. REWORK. The Sol gate is not satisfied.**

## Closure of every r1 finding

“Closed” means the design now specifies the required behavior and implementation gate; it does not certify an implementation that has not been written.

| r1 finding | Status | Rev1 resolution and remaining issue |
|---|---|---|
| **M1 — v24 clock** | **CLOSED** | §4.2 `SB0-shared-base.md:461`, §7 `:838`, SB4 `:1183`: mockable local `NodeClock`, independent of peer offset, with offset/mocktime vectors. Matches Core `validation.cpp:4509–4510` and `dash-consensus/src/release.rs`. |
| **M2 — send admission** | **CLOSED** | §3.1 `:252`, `:265–284`, §3.4 `:377`, §12.1 P2a `:1116`, SB6 `:1185`: unconditional enqueue, retained-memory and partial-transport accounting, pause checks at Core's caller sites. Matches Core `net.cpp:4878–4890`. |
| **M3 — MN bootstrap history** | **OPEN (major)** | §4.3.1 `:508–542` fixes the original tip-near-checkpoint witness in the regular-DKG case. Its proposed maximum age is not a Core invariant: failed DKG rounds leave arbitrarily older quorums active. **F1**. |
| **M4 — cold branches and ancestry** | **PARTIALLY CLOSED (major)** | §4.1/§4.4 `:411–450`, `:554–589` and §9.5 `:975–979` now retain full branch records, stable ids and a separate active projection, with error/restart/deep-reorg gates. Those close the missing-record problem. The new context adapter cannot handle genesis fallback (**F2**), and the cold active lookup does not preserve a retained snapshot's branch (**F3**). |
| **M5 — SPV-to-full replay** | **CLOSED by DEC-104** | §2.4 `:226`, §4.7 `:611–615`, P10 `:821`, SB18 `:1197`: full mode never opens `spv/` and syncs normally from genesis. No importer or prefix-merge contract is needed. |
| **M6 — SQLite snapshot** | **CLOSED** | §9.4 `:957`, `:965–969`, P5 `:816`, SB10 `:1189`: a real read pins the view before return; an initialization failure latches and returns a poisoned snapshot. The two-connection probe confirms the pin and negative control. |
| **M7 — #902 facade** | **CLOSED by DEC-104** | §3.2.1 `:308–363`, §10.3 `:1056–1060`, SB16 `:1195`: bounded handles, peer backpressure, cancel, terminal owner-directed failures, request/attempt ids, and consumer/test migration replace the contradictory verbatim promise. |
| **M8 — membership inference** | **PARTIALLY CLOSED (major)** | §5.1 `:635–648`, §5.2 `:666–674`, SB8 `:1187`: exact membership is explicitly node-only; SPV uses authenticated, signature-verified commitment keys. That resolves r1's non-signer counterexample. The liveness-only argument still needs the canonical-selection invariant in **F4**, and its universal signer-position assertion needs **f1**. |
| **M9 — dependency graph and CPM** | **CLOSED; minor precision correction f2** | §13.1–§13.4 and the overlay now carry CS3→SB4, LQ1b→SB11, SB15→SB18 and SB15/SB16→SB17. SB13 precedes integration rather than depending on SB11. Stub coding and merge constraints are explicit. The claimed milestone dates and critical chain reproduce. |
| **m1 — layering/gate installation** | **CLOSED** | §2.3 `:166–197`, SB1/SB3 `:1180`, `:1182`: B→B, W→W/B/P, feature-separated waiver checks, a ratcheted baseline and positive/negative gate cases. |
| **m2 — thread/offload ownership** | **CLOSED** | L5 `:170`, §7 `:834–849`, SB13/SB17: runtime/pool/blocking-spawn/resolver entry points are banned in S; bounded `BlockingIo`/`Resolver` belong to the host, with exercised-path thread-count gates. |
| **m3 — EV/LQ store errors** | **CLOSED** | §5.1 `:649–658`, EV0/LQ1 addenda `:1123`, `:1126`: associated errors cover all shared views; SPV unavailability and fatal connected-node missing-list/storage failures are distinct. Node D4 remains intact. |
| **m4 — actual hashing path/mobile claims** | **CLOSED** | §8.1 `:862`, `:867`, §8.2 `:878–888`, SB2/SB17: `dash-pow` remains optimized until the switch; linked paths/C flags are measured; amended-profile, production-memory and combined/iOS measurements remain pending. |
| **m5 — feed mechanics** | **CLOSED** | §6.1–§6.2 `:728–804`, SB5 `:1184`: inactive registration, captured targets, generational ids, staging within capacity, producer-space/deregistration wakes, exact tip/status stream separation, and the DEC-96 handoff exception are specified and covered by acceptance cases. |

The two ancillary r1 clarifications are also addressed: the ABI baseline and additive size/version negotiation are explicit in §10.1 `:1030`, with a baseline-header caller gate; §9.5 `:986–995` now assigns one writer and commits each header step before publication and each subscriber's data with its own locator.

## Major findings

### F1 — M3 remains open: active quorum age has no bound in DKG intervals

**Severity:** major. **Section/file:** §4.3.1, `docs/node-program/designs/SB0-shared-base.md:510`, `:516–529`, `:540–542`; §5.2 `:674`; SB11 `:1190`.

The parameter arithmetic is correct: the formulas yield 1,506/1,506/276/156 for the default rotated IS roles, and the documented non-rotated values, including MNHF's 2,368. The unsupported step is “age of the newest active base: up to one interval plus the mining window.” Core counts **successfully mined commitments**, not elapsed DKG intervals. A null commitment returns before adding a mined-index row (`llmq/blockprocessor.cpp:369–378`). Non-rotated scanning takes the last A mined commitments (`:702–728`, `:827`); rotated scanning takes the last successful commitment for each index (`:731–793`, `:825`). Neither expires an old quorum by age. LQ1 §3.9 already states “however old” (`LQ1-quorum-members.md:845`).

**Concrete scenario:** mainnet anchor c=2,500,000, tip=2,501,506. One rotated index last succeeded at cycle base **2,499,840**, then failed in subsequent rounds; other indices can succeed normally. The advertised confirmation passes (`tip−c=1,506`), but reconstructing that still-active quorum needs its H−3C work block at **2,498,968**, a depth of **2,538**. Both its base and that work block are below the anchor. Core's qrinfo builder expressly adds historical snapshots/diffs for the actual last commitments (`llmq/snapshot.cpp:194–229`), beyond its usual H−C…H−4C bundle. With no backfill, the client cannot verify this active index; requesting the absent list cannot create an accepted below-anchor header. Longer outages can also exceed the assumed resident lookback.

**Fix:** retain DEC-104's earlier-checkpoint/forward-sync rule, but make startup confirmation cover the **actual required commitment bases, rotation work blocks and diff targets**, including failed-index history. Re-anchor farther back, ultimately to genesis, when necessary, before starting MN verification. The parameter-only values may remain a regular-DKG sizing estimate; do not call them a maximum or infer that `BelowFloor` is unreachable. Define the steady-state recovery for a deeper requirement and test repeated null rounds and a very old surviving index, plus the non-rotated A-successes-with-gaps case. No backfill is requested.

### F2 — The new 25-entry adapter cannot validate the genesis fallback

**Severity:** major. **Section/file:** §4.1.1, `docs/node-program/designs/SB0-shared-base.md:439–448`; §4.3 `:496–497`; §4.3.1 `:543`; SB4 `:1183`.

Every anchored **or windowed** SPV chain uses the partial path; even `Anchor::Genesis` has `complete() == None` when `window=Some(w)` (`:408`, `:425–426`). That path fetches at most 25 entries and promises those answer every header-rule read. The restriction to post-DGW **checkpoint anchors** does not remove BTC/KGW from a genesis sync.

**Concrete scenario:** a mainnet client falls back to genesis because no eligible checkpoint pack is available or its birth height precedes the first checkpoint. At next height **576**, BTC retargeting asks for ancestor **0** of parent 575 (`dash-consensus/src/pow/btc.rs:42–51`; Core `pow.cpp:162–167`, `GetNextWorkRequiredBTC`). The proposed window holds only 551…575. At the KGW era, the rule can read up to 4,032 entries; a uniform-spacing parent at 19,999 reads 19,974, outside 19,975…19,999. The design's prescribed response is a debug panic or an internal-error rejection of legitimate history. The detached Rust probe exercised both reads against the real consensus functions. A post-DGW 25-entry positive control succeeds.

**Fix:** use a complete ancestry implementation for genesis-origin history, or construct a fallible context with the actual selected rule's required history before calling the unchanged pure functions. Keep 25 as the post-DGW checkpoint pack size. Add genesis-to-BTC-retarget and KGW-boundary cases to SB4's gate, including the windowed genesis configuration and below-DGW fallback. This preserves the node's complete-ancestry contract.

### F3 — A retained SPV snapshot reads the current cold active branch

**Severity:** major. **Section/file:** §4.4, `docs/node-program/designs/SB0-shared-base.md:576–577`; §4.6 `:605`; §6.2 F8 `:801`; §9.5 `:979`. **Related r1 finding:** M4's lagging-reader/coherent-snapshot requirement.

The snapshot retains only the last w active chunks; below those chunks, the design sends height lookups to `HeaderStore::active_at`. That is the mutable `active` projection, rewritten by each reorg. No historical projection version or read view is bound to the snapshot. Stable header ids solve ancestry resolution but do not make a current active-height lookup a historical snapshot lookup.

**Concrete scenario:** with w=10,000, a subscriber retains snapshot `(tip=A20000, tip_seq=s)`, whose resident heights begin around 10,000. A valid competing branch forks at 8,000 and wins, a case §4.4 explicitly supports without a ChainLock. The writer changes `active[9000]` from A9000 to B9000. The retained snapshot's `at_height(9000)` now returns **B9000**, while its tip and resident chunks remain on A. Those facts never described one chain state. A catch-up driver can choose the wrong branch or trip a parent/feed-order invariant. A disk-backed projection probe reproduces the old-tip/new-prefix tuple. CH2's coherent `ChainSnapshot` contract (`CH2-coins-flush.md:1313–1314`, F8 `:1342`) disallows it.

**Fix:** resolve below-window snapshot heights through ancestry from **that snapshot's retained tip id**, using the immutable full branch records; or explicitly bind the snapshot to a versioned/pinned cold projection with an accounted lifetime/resource budget. Keep live `active_at` for live queries only. SB5's lagging-reader test must hold a snapshot across a >w reorg and query both resident and cold heights, asserting every result belongs to the snapshot tip's branch.

### F4 — The liveness-only claim needs selection before the verification-status gate

**Severity:** major. **Section/file:** §5.1, `docs/node-program/designs/SB0-shared-base.md:643–646`; §5.2 `:671`; SB8 `:1187`. **Related r1 finding:** M8.

The narrowed key-trust policy is sound, but “only Verified quorums are selectable (`select_from_scan`)” does not specify whether the verification status is checked **after selecting from the complete canonical scan**, or used to filter/materialize the scan first. These are different behaviors. Core selects non-rotated quorums by scoring the entire scan (`llmq/quorumsman.cpp:758–773`), and rotated quorums by the first matching index with scan-size quirks (`:732–756`; LQ1 §3.9). Dropping an entry changes that selection; Core also returns a blank scan on a missing materialized quorum rather than simply dropping it (`:253–268`).

**Concrete scenario:** the authentic CL active set contains Q0…Q3, and the Core scoring winner for request id r is Q0. SPV's reconstructed signer keys fail Q0's `membersSig`, so Q0 is locally `Unverified`; Q1…Q3 are `Verified`. A `QuorumScanView` built from Verified entries selects a different winner, Q1. A recovered signature under Q1's authentic key can then verify in SPV for r although Core selects Q0 and rejects that signature. This requires a signing quorum able to produce the signature, not a BLS forgery. The signer-reconstruction error has changed which quorum is authorized, so it is not merely liveness loss. A deterministic SHA256d scoring probe shows the selection change; a rotated scan can similarly substitute an older verified quorum for a newer unverified one at the same index.

This is a missing safety invariant in the proposed contract, not a claim that an unimplemented router already filters the scan.

**Fix:** explicitly preserve authenticated canonical active-set membership, ordering/count and the selection hash/height rules independently of local signature-verification status. Select the Core-defined commitment first; then require **that exact selected commitment** to be `Verified`, otherwise return unavailable/unverified and wait or fail. Never retry selection over a filtered set or refill it with older verified entries. Add non-rotated winning/non-winning Unverified cases and rotated newest-Unverified/older-Verified cases to SB8. With this rule, a genuine signing-key substitution can reduce liveness without authorizing another quorum, and DEC-104's key-only trust policy stays unchanged.

## Minor findings

### f1 — “A difference at a signer position fails” overstates what the signature proves

**Severity:** minor. **Section/file:** §5.1, `docs/node-program/designs/SB0-shared-base.md:646`; SB8 `:1187`.

Core's secure aggregate verifier sorts the serialized public keys before computing its weighted aggregate (`dashbls/src/schemes.cpp:231–253`). Swapping two signer entries `[A,B,C] → [B,A,C]` with bits `[1,1,0]` changes both signer positions but supplies the same sorted key multiset. `membersSig`, the commitment hash and `quorumSig` have identical verification inputs. This does not invalidate key authenticity or restore a claim of exact membership, but the universal assertion and its mutation gate are incorrect.

**Scenario/fix:** qualify the rejection claim and test to a genuine **change of the selected signing-key multiset**, under the BLS security assumptions. Add a signer-permutation case whose verdict is unchanged, beside the existing non-signer mutation and genuinely substituted-key cases. Do not infer signer identity/order from aggregate success.

### f2 — The CPM discussion mixes coding float with merge slack

**Severity:** minor. **Section/file:** §13.4, `docs/node-program/designs/SB0-shared-base.md:1292–1295`; overlay `docs/node-program/tools/cpm_sb0_rev1.py:25`.

The reported dates are correct, but CH1-node's **merge** slack against M2 is **1.5 weeks**, not 2.0: P2a finishes at 13.5 and requires CH1 + 0.5, so CH1's latest merge is 13.0; its EF is 11.5. P2a-node is on the reported critical chain and has zero merge slack; its 2.0-week gap refers to coding that completes before its SB6 merge constraint. CH1's 2.0 likewise corresponds to its unconstrained coding completion at 11.0, not its stated merge at 11.5.

**Scenario/fix:** dispatch based on “2.0 weeks of slack” can consume 0.5 week beyond CH1's merge allowance. Label coding float and merge slack separately, or state the CPM finish/slack values consistently: SB4/SB5 1.5, CH1-node 1.5, SB6/P2a-node 0. This does not change M2=18.5 or REL11=30.0.

## Verified claims and test evidence

- **Lookback arithmetic and mapping:** independently parsed Core `llmq/params.h` and its chainparams role assignments; all documented default values reproduce. `WORK_DIFF_DEPTH=8`, the previous three quarters, qrinfo's extra fourth share and `SIGN_HEIGHT_OFFSET=8` explain the nominal formulas when every necessary DKG succeeds. **F1** is the missing age bound, not an arithmetic error.
- **Skipped MNHF:** acceptable within this SPV scope. Its complete commitment remains in the authenticated quorum-root set, and no MNHF key is used by CL/IS/Platform/recovered-signature or FFI lookup. Skipping signature verification for an unused type does not weaken the keys actually consumed. A type shared with a used role must be verified, as the testnet CL/MNHF mapping already implies; exclusion is by configured use, not a blanket MNHF type-number exception. The node still owns MNHF state/transaction validation. The quoted 2,368 is itself a nominal regular-DKG estimate, subject to F1.
- **M8 trust boundary:** non-signer-only mutations need not change verification and now imply no membership claim. A genuine signer-key multiset substitution fails signature verification under the BLS assumptions. Retaining the canonical selection and then checking its exact key's status (**F4**) is necessary for the resulting loss to remain liveness-only.
- **CPM:** ran `python3 docs/node-program/tools/cpm_sb0_rev1.py` and independently recalculated every row's earliest finish. Also checked every SB1–SB19 dependency against §13.1, normalizing CH2a/CH2b for run B and the EV4-serve alias. Baseline/A/B/C M2 exits are **18.0 / 18.5 / 18.5 / 21.5**; REL11 is **30.0 / 30.0 / 30.0 / 31.0**. Run A's limiting chain is **P1(9.0) → SB6(13.0) → P2a-node(13.5) → P2b(16.0) → O13a(18.5)**. Interface edges impose X+0.5 merge constraints. The overlay openly excludes external #902/Q1 timing and other PLAN revisions; these are sensitivity results, not a fully refreshed program forecast.
- **Host probes:** seven Python checks passed: Core parameter arithmetic/roles; the failed-index history witness; selection over a filtered set; retained-snapshot/live-projection mismatch; SQLite pinning and negative control; signer-key permutation inputs; and independent CPM/edge verification. SQLite databases were disk-backed. These are narrow contract/input proofs, not device durability tests, BLS signature execution or a live-chain DKG reproduction.
- **Rust build/tests:** with `TMPDIR=/dev/shm/sb0-review-r2-sol-tmp` (mode 700) and `CARGO_BUILD_JOBS=12`, all completed successfully in this checkout:
  - `cargo test --locked -p dash-consensus --lib pow::` — **7 passed**.
  - `cargo test --locked -p dash-consensus --lib release::tests::` — **3 passed** (baseline release/context tests, not an implementation of the proposed offset/mocktime gate).
  - `cargo test --locked -p dash-consensus --test core_vectors pow_tests_get_next_work` — **1 passed**, covering six recorded Core next-work vectors.
  - `cargo test --locked -p dash-consensus --test sb0_review_probe` — **3 passed**. The disposable probe confirmed the real BTC ancestor-0 read, KGW's out-of-window read, and the post-DGW 25-entry positive control. Expected out-of-window panics were caught by the probe; this is a contract witness, not a claim that SB4 has been implemented.

## Verdict

**REWORK. Sol gate: NOT SATISFIED.** Close F1–F4 and correct f1–f2, then return the design to Sol. The sequential Opus-high pass should wait for this gate under DEC-92. The accepted DEC-104 simplifications remain in force.

The detached scratch checkout, its probes/build artifacts and `/dev/shm/sb0-review-r2-sol-tmp` were removed at handoff. No commit, push, GitHub comment or modification to the design/overlay under review was made.

# SB0 narrow preservation review, round 6 — Sol (DEC-92)

Reviewed `docs/node-program/designs/SB0-shared-base.md` **rev5 at 1f75dbb6**, against the Sol-approved rev4 at **53091a32**. All SB0 line references below name 1f75dbb6. Read `git diff 53091a32 1f75dbb6 -- docs/node-program/designs/SB0-shared-base.md`, §16.1, the prior Sol findings and closures, Opus r6, `briefs/REVIEW-COMMON.md`, and the relevant program context. DEC-104/105/107/109/112/113 remain binding and are not reopened. This is the requested preservation pass, not a new full review.

**Result: APPROVE-WITH-CHANGES. Sol gate: NOT SATISFIED. Three majors, one minor, no blockers or nits.** The new separation of Core's best header, bounded retention, authoritative wallet progress and thread-source controls are accepted. The three majors concern gaps in their specified operation, within those choices.

## Preservation of Sol-settled properties

| Property / edited section | Result |
|---|---|
| **NodeClock; unconditional push** (§3.4, §4.2, §7) | Preserved. The v24 future-time check still uses mockable local `NodeClock`, never peer-adjusted time (`SB0:495`). Push still charges retained memory and always appends; any admission budget remains in the SPV processor (`:264`–`:282`). Naming the receive overshoot does not change that contract. |
| **Node ancestry; header-rule envelope** (§4.1.1) | Preserved. Node `ChainAncestry` remains complete and infallible (`:448`); SPV context construction remains fallible with BTC 576, KGW 4,033 including the exit predecessor, and DGW 25 (`:462`–`:480`). Lazy MTP avoids inventing history below the pack without shortening the envelope. `CONTEXT` stays out of Core's status word. |
| **Anchor / coverage / no runtime re-anchor** (§4.3.1) | Preserved. Absolute work, accepted-vs-context distinction, actual active commitments' required history, headers-only Candidate fallback and Durable establishment remain. The added birth-height and MN-sync-off branches do not authorize a runtime fallback. Established anchors, applied locks and other subscribers' state are retained (`:597`–`:649`, `:1322`–`:1325`). **Enforcement of the retained CL floor has a separate regression in R6-M1.** |
| **Genesis clip / pre-quorum startup** (§4.3.1, §5.2.1) | Preserved. Nonexistent negative history is clipped; the authenticated domain remains `[max(tip − H_keep, A_q), tip − 8]`; empty-domain/empty-set coverage establishes subscribers before usable quorums. Regtest tip 20 needs no request; tip 450 targets 432 and 442. Early rotated commitments still require ordinary verification (`:616`–`:627`, `:846`–`:865`). |
| **Branch records / partial ancestry / own-tip snapshots** (§4.4, §4.6) | Preserved. Full branch records and never-reused stable IDs remain; cold snapshot heights follow that snapshot's own tip, never the live `active` projection (`:675`–`:708`, `:740`). u32 matches the pure interface. Eviction now waits for commit and follows the active tip, preserving cold-read availability. |
| **Canonical scan; select then require Verified; H_keep** (§5.2.1, §5.3) | Preserved for CL/IS selection. Scan membership/order remain independent of local verification status; no substitution or reselection is introduced. H_keep remains 320/320/80/56, with in-domain deduplicated fetches and outside-horizon unavailability. R ≥ H_keep retains rows needed by recent scans (`:874`–`:890`, `:953`). **The new hash-lookup fallback has R6-M3.** |
| **Feeds; other subscribers' data-plus-locator** (§4.6, §6.1, §9.5) | Preserved. F1 ordering, commit-before-publish, tip/status separation, F10/F11 and per-component WAL ordering remain. Removing the wallet locator correctly respects its separate host database. **The replacement wallet recovery rule has R6-M2.** |
| **SQLite snapshot pin** (§9.4) | Preserved. `BEGIN DEFERRED` is still followed by a real metadata read before return; failures still latch and poison the snapshot (`:1264`). Capping the read pool does not remove that read. The pinned-view and bare-BEGIN negative controls still reproduce. |
| **EV seams / node ownership / full-mode isolation** | Preserved. Per-client `LlmqConfig`, borrowed per-call `LlmqCtx`, infallible materialized views, fallible fetching views and node fatal-error mapping remain. Node V receives the ChainCommand; no locks-thread writer is added. Full mode still never opens the SPV store and synchronizes from genesis. DEC-113's attempt serialization is not changed by this diff. |
| **Plan edges / CPM** (§13.4) | Preserved in the actual model. SB12a is sized and feeds SB12; existing kernel/product merge gates remain. Run A/B M2 18.5, REL11 30.0 and M5 41.5 reproduce, including the new stress run. The literal “every run” sentence needs R6-m1. |

The §16.1 preservation map is accurate for these unchanged contracts, but its unqualified claim that CL-floor enforcement and wallet recovery are preserved is not established by the new rules below.

## Major findings

### R6-M1 — `fork_target` tests the candidate's flag, not its conflicting ancestry or the CL floor

**Severity:** major. **Refs:** §4.1 `docs/node-program/designs/SB0-shared-base.md:436`, `:442`; §4.5 `:729`; SB4 `:1510`. **Introduced by:** O1's replacement of the prior floor-compatible best-work predicate with a separate target and Core-exact marking.

The new target is the most-work accepted header that is itself neither failed nor `BLOCK_CONFLICT_CHAINLOCK`. The same section now explicitly limits ChainLock marking to sibling roots and disconnected blocks. An unflagged descendant of a stale conflicting root therefore remains eligible under the stated predicate. Calling it an analogue of `FindMostWorkChain` does not supply the missing condition: Core walks the candidate's ancestry and rejects a conflicting ancestor (`v24 validation.cpp:3533`–`:3575`). Its marking intentionally need not mark every descendant (`:4111`–`:4122`).

**Concrete failure:** three accepted branches share height 80. A ends at 100, B at 110 and C at 105, with unit header work; B is active and C is stale. A verified ChainLock locks A100. Core-style `EnforceBlock` marks B81 and C81; disconnecting B also flags B82…B110. C82…C105 remain unflagged. The written `fork_target` is **C105**, which has more work than A100 but crosses the newly established lock. `set_active_tip(fork_target)` then publishes the wrong branch, or stalls if an unstated downstream guard refuses it. No re-anchor or deleted lock is needed. The retained floor alone does not make this predicate safe.

**Suggested fix:** leave `best_header` and its Core update sites exactly as rev5 specifies. Make SPV target eligibility explicitly check failed/conflicting ancestry and compatibility with `cl_floor` (including previously accepted headers that predate the lock), before choosing by work. This can follow Core's candidate ancestry walk or test ancestry against the floor; it must also handle the case where the locked block was already active and Core marks nothing. Add the three-branch case, restart/cold-record variants and the no-marking case to SB4/SB5; assert that every published SPV tip respects the floor.

### R6-M2 — An unknown wallet hash does not prove the durable SPV tip is a safe rewind point

**Severity:** major. **Refs:** §9.5 `docs/node-program/designs/SB0-shared-base.md:1313`, `:1318`, `:1320`; SB13 `:1520`, SB14 `:1521`. **Introduced by:** O4's new unknown-hash recovery branch.

The min-height rule is valid for same-branch persistence skew. It does not establish a common branch after a Buffered reorg is lost. Commit-before-publish is also insufficient: it permits the wallet database to persist effects of an SPV transaction that was committed but not Durable.

**Concrete failure:** the SPV database's last Durable state is A100. A subsequent Buffered header step reorganizes at height 80 onto B110 and publishes it; the wallet truncates at 80, applies B81…B110 and durably stores checkpoint `(110, B110)` in its independent database. Power loss then removes the SPV Buffered suffix, recovering A100 with no B records. At open, B110 is unknown. Rev5 calls **`truncate_above(100)`** and resumes there. That retains wallet effects from B81…B100 although the retained SPV chain contains A81…A100; scanning above 100 cannot repair them. A payment in B90 remains falsely confirmed, or one in A90 is never discovered. This is an allowed WAL-prefix recovery, not a non-prefix storage failure.

**Suggested fix:** when the wallet tip is unknown, establish a common ancestor before using a height as the rewind point, or conservatively rewind once to a trusted common checkpoint/anchor covered by the wallet's scan contract. If needed, reacquire the lost wallet-branch ancestry before that single rewind. Do not infer same-branch continuity from the durable height. Keep the wallet checkpoint authoritative and keep the single `truncate_above` plus rescan. Add this independently persisted Buffered-reorg witness to SB13/SB14, alongside the simpler straight-extension suffix-loss control.

### R6-M3 — The on-demand target cannot retrieve a quorum replaced within the promised lookup grace

**Severity:** major. **Refs:** §5.3 `docs/node-program/designs/SB0-shared-base.md:959`, `:962`–`:966`; SB11 `:1517`, SB15 `:1522`. **Introduced by:** O3's bounded-retention fallback combined with its new lookup predicate.

The lookup permits a commitment replaced after `h − W_P`, including one already absent at h. After pruning, the fallback authenticates a `getmnlistd` **to h** and fetches member-verification inputs. Core's null-base diff contains the commitments **active at its target**, not every commitment ever mined before it (`v24 evo/smldiff.cpp:31`–`:64`; `llmq/blockprocessor.cpp:818`–`:829`). The work-block list is from before the queried commitment was mined, and cannot supply that commitment's missing root proof.

**Concrete failure:** mainnet parameters give W_P=38 and R=320. Let a non-rotated quorum Q be mined at 1000 and replaced at **1400**. At local tip **2000**, its row is correctly pruned (`1400 < 2000 − 320`). Request its key at **h=1437**. The new predicate permits Q because `1400 > 1437 − 38 = 1399`. A successful null-base diff to 1437 omits Q; the supplementary work-block diff also cannot authenticate it. With peers and all required headers above the anchor, the prescribed fetch still cannot produce the promised Found answer. Retained rows answer this query before pruning, so the behavior also depends on whether the row happens to remain in an LRU.

**Suggested fix:** keep bounded R, on-demand acquisition and the hash-lookup/selection distinction. Specify how the fallback obtains an authenticated historical target at which Q is present within the permitted interval, rather than always targeting h. In this witness a diff to **1399** supplies it. A bounded search/acquisition over the grace interval, with deduplication and timeout limits, can cover commitments mined and replaced inside that interval too. Require the same authorization predicate for cached and fetched results. Add h=`r + W_P − 1` and h=`r + W_P` tests **after pruning and restart**, using a non-rotated quorum with all verification inputs available; the first must be fetched and found, the second rejected. No indefinite retention or runtime re-anchor is requested.

## Minor finding

### R6-m1 — “Every run” gives run A's milestone dates to run C

**Severity:** minor. **Ref:** §13.4 `docs/node-program/designs/SB0-shared-base.md:1614`.

The new sentence says “M2 18.5, REL11 30.0 and M5 41.5 are unchanged in every run.” The supplied rev5 overlay returns **21.5 / 31.0 / 42.5** for run C. SB12a does not change those prior run-C results; the adjacent table correctly distinguishes C. A reader of the new sentence would nevertheless assign the stub-overlap dates to the no-overlap case.

**Suggested fix:** say that each run's prior milestone dates are unchanged, and quote 18.5/30.0/41.5 for A/B/stress and 21.5/31.0/42.5 for C. No graph or decision change is needed.

## Other new fixes and Q8

**O1's node half is accepted.** Acceptance updates, walk-back hooks, conflicting entries at load and V ownership agree with the cited Core sites. R6-M1 is confined to SPV target eligibility.

**O2 is accepted.** SB12a owns the bridge before SB12, preserves the public dashcore query shapes and gives moved APIs adapters with their old signatures. `Header::from_parts_hashed` takes a hash computed locally from the same canonical 80 bytes; it avoids a second production X11 call while retaining the explicitly permitted debug recomputation. The byte-round-trip, counting and mismatch gates are assigned. This proposed constructor and bridge are not implemented yet.

**O3's bounds are accepted.** R ≥ H_keep preserves recent scan commitments, and the spacing-derived arithmetic gives W_P=38 and R=320/320/80/76 for the documented configurations. Lists, active commitments and lookup rows have explicit retention/ownership. R6-M3 concerns obtaining the specified answer after pruning, not the decision to prune.

**O4's ownership is accepted.** The wallet's own checkpoint is the authority; no SPV wallet locator remains. Known stale-branch recovery uses the actual fork. R6-M2 addresses only the unknown-hash inference.

**O5 is accepted as an implementation contract.** The edits name the actual rayon entry points in V and key-wallet-manager, require `pool.install`, put blst `no-threads` at the FFI target boundary, exclude it from node graphs, and set both tokio thread limits explicitly. blst 0.3.17's `no-threads` pool executes jobs inline (`src/lib.rs:68`–`:91`). The old MN path is included at SB13 and later changed paths rerun the exercised-path test. Shared hygiene and host ownership remain. No production thread-count result is claimed by this design review.

**O6 is accepted.** Key-wallet at M5 and the WL2a-dependent SB18b at M6 avoid introducing a missing wallet dependency into M5.

**Q8 is a real open question.** `get_quorum_at_height` previously answered from any held/logged list at or before h. The new replacement cutoff changes what Platform can obtain, and its historical lookup assumptions belong to that consumer. Agreement on that semantic change and its supported grace window is separate from DEC-112's already settled bounded-retention choice; the stated before-SB11 deadline is meaningful. The 31-minute metadata-time check and 3-minute configuration support a nominal sizing policy, not a hard bound on Core-height lag: fresh Platform blocks can retain an old `last_committed_core_height` when no newer ChainLock is available (`Platform 6499c680c6 prepare_proposal.rs:84`–`:95`, `:126`–`:128`). Keeping on-demand acquisition is therefore necessary even with freshness enabled. Q8 does not excuse R6-M3's inconsistency under the currently recommended semantics.

## Verification and limits

- Read the cited Core v24 acceptance/marking/candidate-walk and quorum-diff code, wallet progress interfaces, current SPV query fallback, header hashing and rayon/FFI thread sources. Source paths cited by rev5 are unchanged between its additional-read baseline 447bc629 and 1f75dbb6. Also read Platform 6499c680c6's freshness check, proposal core-height assignment and SPV quorum consumer. No live node was queried or started.
- Ran `TMPDIR=/dev/shm/sb0-review-r6-sol-tmp PYTHONDONTWRITEBYTECODE=1 python3 /work/scratch/sb0-review-r6-sol/witnesses.py`: **5 tests passed**. They reproduce the three design counterexamples, horizon/domain arithmetic and the actual two-connection SQLite pinned-view/negative-control behavior. The wallet witness uses two independent disk databases and an explicitly saved allowed Durable prefix; it does not claim to simulate physical power loss. Fork and quorum witnesses are specification models checked against the cited Core control flow, not executed BLS or proposed SB4/SB11 implementations.
- Reran `/work/scratch/sb0-reconcile/rev5/cpm_sb0_rev5.py` against the committed CPM model. SB12a finishes at 11.5 and SB12 at 14.0 in A/B/C; SB14 remains 20.5/18.0/23.0. A/B milestones and the enlarged-SB13/SB15 stress numbers reproduce. Run C retains its distinct dates as noted in R6-m1.
- `git diff --check 53091a32 1f75dbb6 -- docs/node-program/designs/SB0-shared-base.md` passed. No source/build change is under review, so no broad build/test rerun was performed. Production fork, wallet crash/recovery, bridge, fetch and mobile gates remain implementation requirements.
- Followed PLAN §7 handling (CANDIDATE-SPV). No excluded/private issue material was investigated. No commit, push or GitHub action was taken. Only this requested report is written in the integration worktree, uncommitted; this review's scratch files/databases and mode-700 TMPDIR are removed at handoff. Other agents' files are left untouched.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate: NOT SATISFIED.** Resolve **R6-M1, R6-M2 and R6-M3** within the adopted design before the sequential Opus confirmation. The other Sol-settled properties and binding decisions remain accepted; R6-m1 is a prose correction.

# EV addendum to SB0 — design review, round 1 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md`, rev0 at **fa09d8d0**, `node/integration` |
| Reviewer | Sol, sequential first pass under DEC-92 |
| Inputs checked | SB0 rev3 at **d790b55e**; DEC-104/105/107 and subsequent DEC-109; EV2 rev4 (DEC-58); LQ1 rev4 (DEC-60); EV0 at **3a5cb631** and its r1/r2 API reviews; SB0 r1 m3 |
| Core | Exact `git show` reads at `v24.0.0-rc.3` and relevant comparisons at `v23.1.8` |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** Close M1–M5 before the Opus-high pass, unless the manager invokes DEC-92's explicit escalation alternative. |

## Evidence

I used detached worktrees under `/work/scratch/ev-add-r1-sol/`, without changing the reviewed branches. Test temporary files used `/dev/shm/ev-add-r1-sol-tmp`, mode 700.

- EV0 at 3a5cb631: `cargo test -p dash-evo --all-features`: **32 passed** (18 library, 8 consumers, 6 store).
- A two-crate `rustc` probe reproduced **E0117** for the proposed external `MnEntryView for Arc<Dmn>` implementation. A shared `Arc<T>` forwarding implementation plus a node-local `MnEntryView for Dmn` compiled; its test passed.
- A fallible-chain probe reproduced **E0277** when propagating an ancestry error from the retained boolean rotation predicate. Changing its result to `Result<bool, E>` propagated `BelowFloor`; its test passed.
- Scratch-only EV0 probes: **5 passed**, including two existing imported consumer tests and three probes. Separate per-call overlays lose a previous call's snapshot write; one attempt-owned overlay preserves it. A general lazy-key isolation probe shows that discarding effects does not isolate interior state shared through an `Arc`.
- I reran SB0's committed CPM overlay with the actual proposed EV0 edge change, `i:BS6a` → `BS6a`, plus `LQ1a + i:SB7` and `LQ2 + SB8`. Results appear in M4.

These are baseline and boundary checks. The reshaped crates and the LQ1/SB11 production implementations do not yet exist at this revision; their proposed acceptance suites have not been run. All reviewer-created worktrees, probes, build outputs and temporary files were removed after review. This review is uncommitted.

## Required changes

### M1 — The proposed node entry adapter violates Rust's orphan rules

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:204`; also `:165`, `:222`.

Commit 3 explicitly puts `impl MnEntryView for Arc<Dmn>` in `dash-dml`. After the split, `MnEntryView` belongs to `dash-evo`, and `Arc` belongs to `std`. `Arc` is not a fundamental type for coherence: nesting the local `Dmn` inside it does not make this implementation legal. The existing consumer copy works because its trait is local to that test crate. Moving the trait changes that fact.

**Failure scenario:** EV0 commit 3 fails with E0117, and neither `MnListView::Entry = Arc<Dmn>` nor `DmlMemberSource` can satisfy its entry bound as specified. Moving the illegal implementation into `dash-evo` would require a dependency on node-only `Dmn`, violating L2/L6.

**Fix:** land a forwarding `impl<T: MnEntryView> MnEntryView for Arc<T>` in shared `dash-evo`, and implement `MnEntryView for Dmn` in `dash-dml`. EV0's `Dmn` already implements `Clone`. This preserves `Arc<Dmn>` as the entry type and cheap clones at the algorithm boundary; the forwarding methods perform the same field reads. A node-owned entry newtype is another legal choice but causes more API churn. Put the chosen arrangement in commits 1/3 and exercise it with a real cross-crate consumer.

### M2 — The fallibility delta omits ancestry-reading predicates and iterators

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:157`; especially `:159` and `:232`.

The addendum changes `members_route` to `Result`, then says every other LQ1 §2.5 function keeps its shape. Approved LQ1 still declares:

- `is_quorum_rotation_enabled(...) -> bool` (`LQ1-quorum-members.md:327`);
- `enabled_quorum_params(...) -> Iterator<Item = &LlmqParams>` (`:328`);
- `precompute_targets(...) -> Iterator<Item = LlmqType>` (`:370`).

These paths read ancestry or call predicates that do. Core's rotation predicate explicitly reads the ancestor at `cycleBaseHeight - 1` (`v24 src/llmq/options.cpp:50`, particularly `:61`; LQ1 §3.2). The plain iterators have no channel for a failure while selecting their entries.

**Failure scenario:** a held base needs an ancestor outside the anchored view. Returning false, `None`, or skipping the affected type converts unavailable input into a Core-visible disabled/empty result; unwrapping instead panics. Making only `members_route` fallible leaves the public predicate and iterator contracts unable to propagate `View(E)`.

**Fix:** enumerate every ancestry-reading function and trait in LQ1 §2.5, including the materializer/precompute error seams. Make the relevant predicates fallible and give the iterators an explicit error channel, or move the reads to an explicitly fallible caller and pass fully resolved inputs to infallible helpers. Preserve Core's read/cache order. Add negative controls for an unavailable cycle predecessor and previous-cycle ancestor, as well as `prev` at a non-genesis floor: they must propagate `View(BelowFloor/NotHeld)`, never false/empty/break. Specify that `prev == Ok(None)` means actual genesis only and that missing retained ancestry is an error. Keep known before-genesis termination distinct.

### M3 — Fresh recomputation is not always the node's actual cached membership

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:235`; `:236`, `:259`.

`ShadowRows` can isolate writes, and bypassing the members facade avoids warming its caches. That does not establish that `members_uncached` supplies the authoritative roster that the node actually uses. Approved LQ1 explicitly says a hit can differ from a fresh computation after work-block data becomes unavailable, and limits its uncached comparator to misses (`LQ1-quorum-members.md:760`, §3.7.4).

**Failure scenario:** the node computes a non-rotated roster while the work block carries a non-null coinbase CL, then prunes that block while the roster remains cached. The facade returns that roster on a hit (`v24 src/llmq/utils.cpp:657`). Fresh computation uses the hash-form modifier after the disk read fails (`src/evo/cbtx.cpp:193`, `src/llmq/utils.cpp:89`), and can return different members. mnxcheck would compare against a roster consensus is not using. The unconditional equivalence implied by the new acceptance wording is therefore unsound even without rotation.

**Fix:** distinguish a comparison with current inputs from a comparison with actual node membership. For the latter, expose a read-only cache peek that returns the existing roster without an LRU bump or insert; on a miss, compute with private rows and `NoMemo` or a private memo. If mnxcheck intentionally compares fresh computations instead, name that limitation and stop calling its result the node's authoritative members. Add the warm-cache → block-data-unavailable case, and compare full cache state/access order and row bytes before/after. Identical-input cold captures remain a useful equivalence test.

### M4 — The CPM sensitivity omits the EV0 dependency change it claims to model

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:250`; `:262`, `:263`.

The proposed PLAN row changes EV0 from `CS0, i:BS6a` to `CS0, BS6a`. The cited `/work/scratch/ev-add/cpm_evadd.py` changes LQ1a and LQ2 edges and EV0 sizes but retains EV0's old interface edge throughout. Its zero-cost S → M result depends on that retained edge.

**Failure scenario:** using the proposed rows with the document's stated run-A model gives:

| Finish | SB0 run A / cited sensitivity | Actual proposed EV0 edge |
|---|---|---|
| EV0 | 7.0 | **8.5** |
| EV2a | 11.0 | **12.5** |
| LQ1a | 13.5 | **15.0** |
| SB7 / SB8 | 9.5 / 16.0 | **11.0 / 17.5** |
| SB11 / SB19 | 21.5 / 23.5 | **23.0 / 25.0** |
| REL11 | 30.0 | **31.5** |

Under those proposed edges EV0 S → M gives EV0 **9.0** and REL11 **32.0**, rather than being absorbed. Adding `i:SB7` to LQ1a and `SB8` to LQ2 alone does preserve 30.0 in the original model.

**Fix:** reconcile the PLAN row and the model. Keeping a historical overlap edge in the historical model while enforcing an already-satisfied real-world merge gate is reasonable, but label that distinction. Alternatively build a progress-aware remaining-work model that accounts for merged CS0/BS6a and already-implemented EV0, and size the reshape separately. Commit the sensitivity script and pin its inputs. These numbers expose a modeling mismatch; they do not predict that an already-merged dependency will delay real work. The present evidence cannot support “every finish is unchanged” for the proposed graph or the claimed residual +0.5-week allowance.

### M5 — Resolve the attempt-overlay contract before claiming LQ1a can start

**Severity: major (start readiness). Location:** `docs/node-program/designs/EV-addendum-SB0.md:207`; `:252`, `:277`.

Leaving EV0 m2 deferred was acceptable at its API review because it was explicitly a gate on LQ1a's start. This addendum's purpose is to release that start, yet it retains the gate without choosing a contract. The generic signatures still expose only a write-only `EffectSink` to `QuorumMemberSource::members`.

**Failure scenario:** the approved EV0 stub uses an `AttemptRows` overlay local to each call. A first call stores a row and appends its effect; a second call in the same attempt starts with an empty overlay and reads no row from the unchanged backing store. The scratch probe reproduced this. Core's unconditional raw write is visible to the next call (`v24 src/llmq/snapshot.cpp:320`), and normal row absence changes rotation. The current single-call acceptance does not cover this.

**Fix:** resolve m2 in this addendum, with CH7b/LQ1a ownership recorded. Use one attempt-tagged overlay, owned by CH7b's facade/attempt handle and shared by every members and precompute call in that attempt; keep `EffectSink` write-only. Bind that handle to the process-wide cache owner, so cache state still survives failed/proposal attempts. `SnapshotRows::store` updates the overlay and records the ordered effect; later reads see the last overwrite. Apply effects even on failure/`fJustCheck`, before the next attempt's read view. Runtime rows synchronously overwrite and propagate write failures through D4/abort. Add a two-call visibility test, an overwrite test, attempt isolation, and failure/fJustCheck persistence. This is a contract decision now; the production implementation can remain CH7b's deliverable. Otherwise retain the explicit LQ1a start gate and revise the readiness claim.

## Minor clarifications

### m1 — Review the final normative documents with the API delta

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:211`.

Commits 1–4 contain the executable delta, so they can remain the focus of EV0 r3. Commits 5/6 also record error semantics, effect ownership and deferred gates. A review confined literally to the earlier tree would miss a contradictory contract introduced there. Review the post-rebase code delta with move detection and check the final API/architecture documents against this approved addendum. Include any m2 resolution in that contract review. Reuse r2's approval for unchanged bodies.

### m2 — Specify the complete shadow-read isolation boundary

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:236`.

Require overlay-first reads, a backing row reader that does not mutate the production snapshot LRU, and `NoMemo` or a private memo. Also preserve shared lazy BLS fields during projection/comparison. `Arc<Dmn>` contains `Arc<MnCold>` with interior state: the scratch probe confirms that a shadow observer's `get()` changes a shared undecodable key's later bytes despite a discarding sink. This is an isolation guard for the synthetic/imported domain, not evidence that the unimplemented member algorithm performs that extra read. Clone lazy fields for observer-only serialization/comparison, as EV0's r2 `SameRecord` fix already does, and test later Core comparisons/serialization as well as row/cache digests. Compute observer digests from clones so the measurement itself cannot mask mutation.

### m3 — Make SB8's rotated acceptance phase explicit

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:235`; `:257`, `:274`.

The real rotated `members_uncached` body lands in LQ1b-E. SB8's verifier can land earlier against an injected member source, but SB0's SB8 acceptance includes LQ1 fixtures under both views and rotation cases. Identify which are verifier-only tests using supplied rosters and which require the later rotated composition. Put the latter behind LQ1b-E/SB11 explicitly; SB11 already has the LQ1b edge. If all those tests instead gate SB8 itself, add the necessary phase dependency and rerun CPM before relying on SB8's finish to gate LQ2. Do not use an empty/unimplemented rotated route as the final acceptance.

## Answers to the design checks

1. **Crate split:** the ownership map is sound after M1. DML/store/deltas/credit pool/MNHF/`imbl` stay in Pn; shared types, effect vocabulary and read traits need no node state. A complete-ancestry-only mined reader is a legitimate shared abstraction, with SPV deliberately constructing scans from its authenticated set. `UsedSet` belongs in `dash-dml`. The proposed ordinary dependencies respect L2/L3/L6; retain L4's purity/serial-BLS gates. No new codec, runtime/thread owner or command-handler duplication is proposed under L5/L8/L9; mobile size remains subject to L7 measurement.
2. **Errors:** `Missing` → node `Store(Inconsistent)`, `View(StoreError)` → the same store error, and `UnknownLlmqType` → the existing exception preserve D4 at the mapping boundary. The CH7b production test must prove latch/abort, no invalid block and no request. Fetching views need errors; materialized list/entry/scratch values do not. DEC-109's infallible `MnListView` choice is sound. SPV's request / below-floor unavailable / storage latch mapping agrees with DEC-104/105/107, provided all ancestry errors survive M2. Selection must continue over the full authenticated set before checking the selected commitment's status.
3. **Normal absence:** the rule is correct. Core's absent row breaks the previous-quarter loop (`v24 src/llmq/utils.cpp:496`, `:503`; v23 `:479`, `:485`); unavailable disk data selects the hash modifier. Unfetched inputs must never impersonate either. Complete the contract with M2's ancestry cases. Other genuine value paths remain: genesis/no parent, disabled or invalid base/index, known before-genesis previous cycle, insufficient enabled members/fill failure, and empty or mode-dependent quarters. Known pre-V20 and known coinbase/payload/signature predicate failures can legitimately give no CL; request a coinbase only when the Core path actually reads it. A missing mined commitment is the node scan/materializer's normal absence; SPV does not implement that reader and must not omit an authenticated entry because local data or verification is missing.
4. **EV0 reshape:** existing node behavior can remain unchanged with the legal adapter, full fallibility delta and D4 mapping. Rebase/pin changes also need validation. The six-commit sequence is reasonable; r3 should examine the executable delta plus final normative contract, per m1.
5. **EV2a/LQ1a:** moving `project_sml` to EV4-serve is correct and avoids an EV2a → SB7 type dependency. EV2a's temporary full `MnList` scratch is the approved twin; LQ1b's lean adapter must compare exact acceptance against it. Keep per-position older-state provenance. Public uncached composition is useful for SPV and observer work, while node consensus continues through the process-wide facade. Row isolation is achievable, but the comparison needs M3 and the read boundary in m2.
6. **PLAN/CPM:** the SB7 interface and SB8 verifier ownership changes are reasonable. The stated unchanged numerical schedule lacks the evidence claimed, per M4; account for phased rotated acceptance per m3. Keeping EV2a free of projection is a useful scope reduction, not proof that the added interface/test work has zero cost.
7. **EV0 m2:** resolve it here before releasing LQ1a's start, as M5 specifies. The production overlay stays node-side. DEC-109's five adopted recommendations do not settle this sixth contract issue.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** The shared/node split and generic error approach are worth retaining. M1–M5 require concrete revisions before the sequential Opus-high pass; LQ1a must retain its m2 start gate until that contract is settled.

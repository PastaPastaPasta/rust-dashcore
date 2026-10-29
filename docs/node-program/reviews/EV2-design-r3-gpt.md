# EV2 rev2 — closure review, round 3

**Verdict: CHANGES-REQUIRED.** No blocker remains in the three indexed bulk loops. Two major issues remain in the new payee fast-path specification: P1/P2 do not justify the equality-height queue run, and a block build cannot unconditionally certify P1 for a reader-accepted predecessor.

Read REVIEW-COMMON.md, the **Changes in rev2** section and round-2 dispositions first, then the round-2 report, relevant design sections, charter/process and DEC-50/DEC-54. This review retains the approved from-genesis online 64-bit ordering target. Reference commits are v23.1.8 `728f5055836c6d29806412fc7223ac8fe05af991` and v24.0.0-rc.3 `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

## Round-2 closure

| Finding | Assessment |
|---|---|
| R2-F1, indexed bulk collision history | **Closed for confirmations, decay and streak reset.** The specified source snapshots and per-bucket filtering reproduce Core. The blanket statement including QcTx needs qualification; see R3-m1. |
| R2-F2, payee/projection on reader-accepted lists | **The two original fixtures are fixed, but the general contract remains unfulfilled.** Literal first-scan accumulation is now correct. Additional accepted inputs still take an unjustified indexed path; see R3-F1/F2. |
| R2-m1, imbl API spelling | **Closed.** imbl 7.0.2's `HashMap<K,V>` alias supplies `std::collections::hash_map::RandomState` and `DefaultSharedPtr`, verified in the pinned crate. |
| R2-m2, equality projections | **The principal correction is closed.** Raw codecs exclude bucket history, raw diff application uses Core's content projection, and records require full equality. The explanatory counter formula still needs a minor correction; see R3-m3. |
| Order-only trailers and cache invalidation | **Closed at design level.** The dirty-bucket rule includes order-only sets/removes, and `apply_record` explicitly clears both caches. Implementation/reconstruction gates remain future work. |

## Major findings

### R3-F1 — major: P1/P2 do not put a continuation candidate in the equality-height run

**Locations:** `designs/EV2-dml.md:257`, `:659`, `:662`, `:664`.

Rev2 says that when P1 and P2 hold, the continuing MN lies in the queue run whose **pay height equals the list height**. Core's canonical reader does not enforce `nPoSeRevivedHeight <= list.height`, or the analogous registration-height bound. P2 supplies a lower bound for a continuation candidate's pay height; it does not supply equality.

The actual v24 canonical reader accepts this list, which the Core helper probes exercise at both pins:

```text
list height = 100; V19 on; MN_RR off; both MNs valid
R: regular, lastPaid = 1, registered = 10, revived = -1
E: Evo,     lastPaid = 100, registered = 20, revived = 101,
           consecutive = 1
```

P1 is true: E is the only valid MN with `lastPaid == 100`. P2 is true because the list height is positive. Nevertheless, the queue contains `(1,R)` and `(101,E)`, with no entry at height 100. Core's continuation scan selects E without checking its revival height. Applying the equality-height run specified at line 662 instead misses E and returns the queue minimum R. The corresponding five-entry projections are:

```text
Core:                    E E E R E
equality-run fast model:  R E E E E
```

The production scan fallback cannot repair this input, because `fast_payee_ok` is true. This is a failure of the advertised any-reader-accepted-list helper contract; it does not assert a change on a validated historical chain.

**Suggested fix:** explicitly retain an unbounded `pay_height >= list.height` candidate range, filtered by Core's exact `lastPaid == height`, Evo, validity and consecutive-payment predicates. P1 then guarantees uniqueness, and P2 keeps the candidate in that range. The probe verifies that this range repairs the example. Alternatively, add and check the temporal bounds required for an equality-height run and use the literal fallback when they fail. Do not tighten the canonical reader. Correct the top-run/equality proof in §2.4/§3.4 and add future-revival and height-0/future-registration fixtures to the differential corpus. The old disposition at line 1055 already mentions `>=`; reconcile that with the new operational text instead of leaving both rules in the design.

### R3-F2 — major: a successful build cannot always set `fast_payee_ok = true`

**Location:** `designs/EV2-dml.md:661`.

The new claim that even a predecessor violating P1 is repaired by one build omits a necessary temporal premise: **existing `lastPaid` values must be below the new block height**. The reader does not check that premise. A predecessor can even satisfy both P1 and P2 while carrying two future `lastPaid` values.

The actual v24 canonical reader and actual `CSpecialTxProcessor::RebuildListFromBlock` accept this fixture:

```text
previous list height = 100; next block height = 101
CHAMP order = A, B, C; all valid; no provider transactions or spends
A: Evo, lastPaid = 101, consecutive = 1, registered = 10
B: Evo, lastPaid = 101, consecutive = 2, registered = 20
C: regular, lastPaid = 1, registered = 30
```

The predecessor passes P1, because no MN has `lastPaid == 100`, and passes P2. Core chooses C, writes C's `lastPaid = 101`, preserves A's and B's existing values, and resets A's and B's streaks to zero. The successful result has **three** valid MNs with `lastPaid == 101`; P1 is false. The relevant assignments and reset loops agree at both pins (`specialtxman.cpp` v23:486–515 / v24:759–790).

Core's nine-entry projection of the result is:

```text
Core:                         A A A A A A A A B
single-candidate fast model:   A A A A B B B B C
```

The model shown chooses A as its sole candidate. Choosing B instead also fails to reproduce Core's accumulation of both candidates. Setting the cache to true after building suppresses the correctly designed P1 fallback, even though checking the actual result would select it. The regression is in the unconditional cache certification, independently of the queue-range issue in R3-F1.

**Suggested fix:** reserve the shortcut for a predecessor with a proven temporal/reachability invariant, including `lastPaid <= predecessor height`, and a positive new height. A cached P1/P2 conjunction alone is insufficient: this fixture's predecessor passes it. For decoded/imported or otherwise uncertified predecessors, leave the resulting cache unset or compute the predicate from the resulting list. State the builder's input-domain restriction if it relies on one; do not claim that canonical-reader acceptance proves it. Add this read → build → payee/projection fixture to ensure a build cannot bypass the fallback.

## Minor specification corrections

### R3-m1 — minor: the blanket CHAMP-phase rule includes a vector-ordered exception

**Locations:** `designs/EV2-dml.md:17`, `:353`, `:665`, `:1073`.

Core does not mutate a map it is traversing in any of the checked phases. However, **QcTx's update sequence is not the phase-start `core_order()` filtered to eligible MNs**. `HandleQuorumCommitment` walks `members[i]` in member-vector order, preserving each entry's association with `validMembers[i]` (`specialtxman.cpp` v23:159–174 / v24:283–298). The vector comes from quorum-member selection, not a snapshot of the live MN map. Payee bookkeeping is one update, not a CHAMP bulk scan.

For an initial collision bucket `4 3 2 1` and failed members presented as `1 2 3 4`, actual `PoSePunish` calls in member order leave `4 3 2 1`; reordering those calls to the live bucket's phase-start order leaves `1 2 3 4`. The source and both probes confirm the difference.

§3.3 already specifies the correct per-`i` loop, so this is a contradiction in the general rule rather than a demonstrated defect in that explicit branch. **Fix:** limit the CHAMP snapshot rule to confirmations, decay and streak reset. State that QcTx preserves member-vector order within every bucket, and that payee bookkeeping precedes the streak snapshot. Add a QcTx ordering fixture so the blanket optimization cannot later be applied to that branch.

### R3-m2 — minor: the module layout still gates the production fallback away

**Locations:** `designs/EV2-dml.md:106`, `:661`.

The layout puts `reference.rs` behind `cfg(any(test, feature = "xcheck"))`, while the new production fallback calls `reference::payee_scan` and `reference::projected_scan` in every build. Following both literally makes a normal non-xcheck build unable to resolve those calls. **Fix:** make the scans available unconditionally, placing only the twin behind the feature gate, and update the layout annotation. Verify a default-feature production build in the implementation gate.

### R3-m3 — minor: raw diff application retains the predecessor's registration counter

**Location:** `designs/EV2-dml.md:884`.

After a hidden register-and-spend, Core's raw counter is not generally `max(id)+1` over the surviving list. `ApplyDiff` starts with the predecessor's counter and bumps it only for additions; `AddMN` assigns `max(added internalId + 1, current counter)` (v23:483–485 / v24:513–515, with Core's stated u32 assignment behavior). A predecessor with counter 7 and no surviving MNs can register ID 7 and spend it in the next block: the empty raw diff leaves counter 7, while the online counter is 8. There is no surviving ID from which `max(id)+1` supplies 7. **Fix:** describe the actual retained-counter/addition-bump rule or refer to §2.8 rather than asserting the simplified formula. The separate record-level authoritative counter remains correct.

## Answers to the requested checks

1. **Snapshot-then-apply:** confirmations read `prevList` and write its clone; decay collects immutable MN pointers before updating; streak reset traverses the post-payment `newList2` copy. These are CHAMP-order snapshots at exactly the stages rev2 names. QcTx traverses its member vector while reading/updating the live list, and payee bookkeeping looks up and updates at most one surviving MN. No checked loop traverses the live map while mutating it.
2. **Cross-bucket commutation:** sound for the fixed-candidate confirmation/decay/reset phases. They neither change the key set nor move unique properties between MNs. `mnMap.set` path copies may change sharing, but not the trie's observable shape/order outside the target collision class. Applying different full-hash groups in a different order therefore preserves both states and full iteration order. This does not authorize reordering phases or moving QcTx calls across provider transactions.
3. **Per-bucket order:** filtering each phase-start bucket sequence is sufficient for the three indexed loops. Take a fresh snapshot after confirmations for decay, and after payment for streak reset; do not retain the block-start sequence throughout. For QcTx, preserve the member-vector subsequence instead. Same-value/hot-only replacements still move to the bucket front. The twin must preserve each respective source traversal.
4. **P1/P2 and literal fallback:** P1 prevents multiple first-scan appends on the optimized domain; P2 addresses the original height-0/negative-registration escape below the candidate range. The literal projection description now preserves every first-scan append, overwrites only the last-candidate bookkeeping, skips only that last MN, sorts after the last remaining count, and truncates to Core's weighted count. Both original fixtures have the required results, including `A A A A A A A B`. The equality-run proof and unconditional builder shortcut still need R3-F1/F2.
5. **New rev2 errors:** the checked fallback is the right approach, but its candidate-search proof and builder cache certification are not sufficient as written. The ordering fix is correct once its vector-ordered exception is made explicit. The trailer and full-record equality changes introduce no additional substantive problem found in this review.

## Validation evidence and limits

Scratch artifacts are in `/work/scratch/ev2-gpt3/`:

- `immer_probe.cpp`, `immer-v23.log`, `immer-v24.log`: independently compiled against `git archive` extracts of each exact pin. Persistent snapshots, same-value move-to-front, bulk reversal and same-bucket noncommutation passed. **1,000** mixed-eligibility schedules at each pin passed state and full-order comparisons with randomly scheduled full-hash groups, including distinct hashes sharing upper trie paths. Vendored immer has no diff between the pins.
- `core_probe.cpp`, `core-v23.1.log`, `core-v24.0.log`: compiled and linked against the respective existing Core archives. Relevant DML, special-tx, state and immer sources were compared with their exact tags; the v24 snapshot header also matches. Actual `DecreaseScores`, confirmation/decay sequencing with Core mutation methods, member-vector punishment order, and payment/streak sequencing passed. Both pins returned the original two-Evo projection and the same future-revival payee/projection. **Canonical-reader acceptance was executed on v24 only**; v23 has no canonical reader.
- `rebuild_probe.cpp`, `core-rebuild.log`: linked against the existing v24 O4 build using its real `TestingSetup` and **actual `RebuildListFromBlock`**. No simulated builder was used for this result. Reader acceptance, successful rebuilding, P1 becoming false, and the ordered projection mismatch all passed. Relevant Core sources in this build match v24.0.0-rc.3.
- v24 `evo_snapshot_tests`: **31 cases passed**. v24 field-bit migration, migration logic and canonical diff serialization: **3 cases / 53 assertions passed**. Logs are `snapshot-v24.log` and `dml-v24.log`.
- `build.py` and `command-*.txt` retain the probe build recipes. These are reference/source checks of a design; there is no completed EV2 implementation here to run its future xcheck, fuzz, record-reconstruction or performance gates.

All probes/tests used `TMPDIR=/dev/shm/ev2-gpt3-tmp`, mode 0700; it was removed after testing. No source branch, design or decision file was modified, no commit/push/GitHub message was made, and no dashd instance or existing chain datadir was accessed. The only program artifact added is this review.

Verdict: **CHANGES-REQUIRED**.

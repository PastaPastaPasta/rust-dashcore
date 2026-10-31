# CH3 design rev2: manager rulings on the r1 reviews

Inputs: Sol r1 /work/scratch/ch3-design-r1/CH3-design-r1-sol.md (CHANGES-REQUIRED: B1, M1–M3, m1–m2) and Opus r1
/work/scratch/ch3-design-r1/CH3-design-r1-opus.md (CHANGES-REQUIRED: M1–M3, m1–m14, 7 nits). Apply all of them as
below, record each as a D-row (D31+) naming the finding it closes, and add a "r1 closure" table (finding → section/D-row).

1. **Readiness guard (Sol B1 = Opus M1): REQUIRED, Sol's scope + Opus's G1/G2 form.**
   - Before CH7a: fail closed at the first block where an Evo rule is active, on **every** network (regtest/devnet
     included; NodeSut, L3 and evo-free FTs stay below DIP3 or raise -dip3params).
   - CH7a → SP1/GV1b: historical IBD/reindex only under Core's documented not-synced regime; refuse activation and
     serving at a live mainnet/testnet tip where superblock/payee decisions need missing state.
   - Applies to tip connect, TestBlockValidity/proposals, ABC, recovery and restart. It is a readiness/system
     failure, never an invalid-block verdict (no block marked invalid, no peer scored).
   - Tests: drive a block across the boundary, and restart at it. Removal only when the real helper inputs and
     their tests land (name the rows).
2. **Commit only after effects succeed (Sol M1).** Overlay and Evo transaction stay uncommitted through
   `finish_attempt`; keep Core's effect emission order and "effects survive invalid / just_check attempts".
   Fault-injection test at each effect-application boundary: old parent coins/evo best block, no published tip,
   recovery parity.
3. **MuHash (Sol M2 = Opus m9).** Per-PR L2 gate replays **from genesis** through 625,000 with per-block MuHash
   until a verified segment seed exists (segment seed = scanned start UTXO set or imported accumulator state, a
   later slice). If measured V time for that replay exceeds 45 min at gate load, switch the per-PR check to a
   suffix check that does not claim per-block MuHash parity, and record it as a D-row. Seed the cumulative counters
   from verified values. Test a segment against full replay at first/last heights when segments land.
4. **Recorded IS-conflict cursor (Sol M3 = Opus m2):** only `is_conflict` advances; removal acts on the returned
   conflict and updates the attempt-local removed view. Tests `[A,null]`, `[A,B,null]`, plus a failed attempt
   after a removal. Also Opus m1 (chained child locks) in the same removed-set design.
5. **VerifyDB levels 3–4 (Opus M2):** add the explicit view-target parameter to the a2 seam now
   (`ViewTarget::{Active, Scratch(..)}`; commit follows target, never origin; evo hooks take the same target).
   `ScratchView` itself is built by CH5 (with CH2b's owner), not CH3a. Drop §7.2's claim that CH3a runs VerifyDB
   3–4; make `verifychain 4` an explicit CH5 dependency in §11.
6. **FT ownership (Opus M3): adopt Opus's assignment.** CH3a acceptance + feature_block, feature_cltv,
   feature_dersig, feature_csv_activation, feature_assumevalid; CH3b + feature_abortnode (needs m11 + CH5 for the
   restart half); CH7a + feature_nulldummy, feature_bip68_sequence. Name the slice that wires them. Put the PLAN
   rows in §11.
7. **Slices (Sol m1 + Opus m4):** a7 depends on a4 and a8 (or split a7 into a serial partial digest slice and the
   full L2 gate); a3 → a8; a10 → a5.
8. **F01 test (Sol m2):** the pause hook must serve class-M commands (Flush) while paused, or make the index-only
   durable write on V before pausing; assert the durable record before kill -9.
9. **All other Opus minors (m3, m5–m8, m10–m14) and nits: apply.** CS6 API needs (m5 inline control below
   assumevalid, m7 probe the exec cache before copying spent scripts) go into §11 as CS6 deltas. m6: port CheckBlock's
   fChecked memo (or show PF0 doesn't need it). m8: make the assumevalid-on L2 run actually take the skip. m10/m11:
   undo read error texts and the ST2 handle cache vs feature_abortnode go into §11 as ST2 deltas if needed. m13: add
   an L2-scale disconnect differential and the CH3 fuzz targets. m14: state L3's dashd regime.

Then commit rev2 on node/ch3-design and report the head, the closure table and anything you disagreed with (with
reasons). A narrow Sol r2 + Opus r2 follow.

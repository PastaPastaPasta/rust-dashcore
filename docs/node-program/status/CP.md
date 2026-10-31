# CP status log (DEC-973)

Outcome lines only, newest last. Plan: /work/scratch/cp/PLAN.md.

- 2026-10-31 01:16Z PLAN written. Lanes:
  - A: EV1-5b-2 (EV1) → CH7a-4b → CH7a-5 (CP);
  - B: CH7b-1 → CH7b-2 (delegate cp-ch7b1, Opus xhigh), starting now;
  - C: LQ3 gap check → LQ3b (cp-lq3-gap), starting now;
  - D: BL-430 → BL-431 (cp-bl430), starting now;
  - CH7b-3 after B, C and D;
  - LK1a after CH7b-2, off CH7b-3's merge path (an empty ClState feed is correct).

  CP heavy units are serialized via /work/scratch/cp/heavy.lock. /work is at 163G, so builds and heavy starts wait for ≥ 200G.
- 2026-10-31 01:20Z BL-430/BL-431 BLOCKED (E4/E6 sent). dashpay/dash#7882 was closed unmerged. Its code exists only on origin/fix/keep-evo-block-data-when-pruning (3337a354042) and is in no tag, rc.4 included. Matching rc.4 means no evo prune lock and no startup check. Recommendation: withdraw both and drop them from CH7b-3's deps. Lane D is idle until the ruling.
- 2026-10-31 01:30Z LQ3 gap check done (/work/scratch/cp/lq3-gap/REPORT.md). The pure path from step 10 to the BLS verify is already at the tip (SB8/LQ3a). LQ3b is: LQ3b-1, a `mined` key constructor; LQ3b-2, the production cached ScanQuorums; LQ3b-3, a node-l1 test that every regtest CbTx CL verifies through the Rust scan. The node glue stays in CH7b-3. qrinfo and the rest of LQ3 are off the path (proposed LQ3c, for P6b).
- 2026-10-31 01:30Z OD-CP-1: CH7 §4.5.2 erratum. Step 10 selects with Core's cached `ScanQuorums` (SelectQuorumForSigning), not the chain-aware form. The chain-aware form stays for asset-unlock route C. The design text is fixed in CH7b-3's D-row.
- 2026-10-31 01:30Z OD-CP-2: LQ3b-1 adds a `mined` feature to dash-evo that only dashd-chainstate enables, for validation-accepted mined commitments. This widens DEC-412's gating, which reserved `reload` for dash-mnlist. Reason: Core verifies a lock against the mined commitment's key and never re-verifies it, assumevalid ranges included (quorumsman.cpp:795-796).
- 2026-10-31 01:30Z OD-CP-3: one scan implementation. LQ3b-2's `scan_quorums(.., Cached | ChainBound)` in dash-evo; CH7b-2 calls it and writes no second copy.

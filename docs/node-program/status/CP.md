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
- 2026-10-31 01:35Z DEC-974: BL-430 withdrawn; BL-431 retargeted to CH6b only; lane D closed. CH7b-3's merge dep is now "CE5-05 merged into node/integration" (follows rc.5 CE5-05), in place of BL-430/BL-431.
- 2026-10-31 01:56Z CH7b-1 plan in (/work/scratch/cp/ch7b-1/PLAN.md): 12 commits, 28 mutants. Provisional rulings on Q1–Q8 go to a fresh Sol plan review.
- 2026-10-31 01:56Z OD-CP-4: CH7 §4.2.5 erratum. `NodeLlmqSource::coinbase_facts` treats every failed work-block read as the absence value, as Core's `ReadBlockFromDisk` failure → `nullopt` (rc.4 cbtx.cpp:192-195) and as `FileBlocks` already does. No store-fault path.
- 2026-10-31 02:03Z CH7b-1 plan: Sol r1 CHANGES-REQUIRED 1/3/0 (/work/scratch/cp/reviews/CH7b-1-plan-sol.md). All accepted: F1 C10 schedule (B paused before `InlineRows::store`, after its previous-cycle reads); F2 checked in-crate visibility/overlap coverage; F3 M8 sync point, M18 split; F4 members-gate cache-order cases. OD-CP-5: Q7 `OnV` is class M at ABC's release point (`chain/abc.rs:963`), as Core releases cs_main between ABC iterations (validation.cpp:3768-3833). Q1, Q2 (with the construction contract), Q4, Q5, Q6 (rustdoc plus F2) and Q8 are as provisionally ruled. Implementation starts at C1–C4; narrow Sol r2 on the amended plan.
- 2026-10-31 02:12Z BL-547 MERGED in T56 (integration 6a3a3aa57, head 097d3b611, RDC-CI GREEN run 41155995964 on b7779ce14). Cleaned up: /work/scratch/bl547 and the rdc-wt/bl547 worktree (the branch is kept); tool scripts moved to /work/scratch/cp/tools-ch7a.
- 2026-10-31 02:25Z LQ3b head 89c6b40c4 (node/lq3b; 5 signed commits; mutants 10/10 killed; node-l1 l1_llmq 30/30 with the oracle; no deliberate difference from Core). Opus-h r1 and Sol r1 started; CI-S0 snapshot requested (label cp-lq3b).
- 2026-10-31 02:41Z DEC-976 noted: after EV1-5b-2 READY, EV1 plans EV1-6 and then EV1-J (each with a Sol plan review), both off the critical path. MN0 gets a ping when CH7a-4b merges if it asks.

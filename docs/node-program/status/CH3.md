# CH3 status log

CH3 owns BL-546 (fix round → landing), BL-554, BL-559, BL-407 and the CH3 rows (OWNERS.md, DEC-973). Branches are `node/ch3-<item>`, with worktrees under `~/workspace/rdc-wt/` and scratch in `/work/scratch/ch3-<item>/`; reviews go to `/work/scratch/ch3/reviews/`.

## Outcomes

- 2026-10-31 03:22Z BL-546 r1 (DEC-969) head b0ca84330 (node/ch3-bl546, on ae6d9b0a4): df0364cb0 M-1, aba9488c6 m-1, b33addf47 Sol's user-message assertion, b0ca84330 D221 r1 + n-1 + n-2 notes. Mutants 1–4 killed (/work/scratch/ch3-bl546/logs/mut-r1/summary). dashd-chainstate lib 928/928, dashd-node lib 233 + 3 ignored. Local gate-fast tier 1 GATE EXIT 0 on run 2 (run 1: a fixed-port case, BL-562). The `FlatFileError::Failed` sweep found nothing to file: a refused block save is `AcceptBlock`'s own state, as Core's is (D221).
- 2026-10-31 03:30Z filed BL-562 (ND1, test-only): fixed ports in `init_errors_match_dashd_v24` collide across concurrent gates.
- 2026-10-31 03:30Z BL-546 r2 narrow re-reviews started (Opus-h: M-1, m-1, n-1, n-2; Sol: the user-message gap). CI-S0 asked for the b0ca84330 snapshot.
- 2026-10-31 03:44Z BL-546 r2 narrow re-reviews: Sol ACCEPT (0/0/0; its coverage gap closed); Opus-h ACCEPT-WITH-NITS (0/0/2; r1 M-1, m-1, n-1, n-2 all closed). OD-CH3-2: both r2 nits are docs (the `GuardedFiles::mutate` doc line on the crash harness's callers; D221's n-1 note should name the disconnect's merge too) and go to the BL-554 branch, which rewrites D221 anyway; b0ca84330 stays the head to certify. READY waits on RDC-CI GREEN.
- 2026-10-31 03:46Z cleanup: /work/scratch/bl546-r1-opus and bl546-r1-sol (30G) removed; Sol's BL-559 probe is kept in /work/scratch/ch3/reviews/bl546-r1-sol-artifacts/.
- 2026-10-31 03:55Z BL-554 plan: Sol r1 REWORK (5 M, 2 m, all accepted). rev 1 moves the barrier into the latch: a trip made inside one of W's steps is "report pending" until that step has reported, and other threads' latch reads wait for it. Sol r2 is running.
- 2026-10-31 03:55Z OD-CH3-3 (supersedes OD-CH3-1; DEC-976 gives back CH3b b2b-2, b4b and F01): one heavy unit at a time, with plans and their Sol reviews done in between. The ETAs are mine and provisional:
  1. BL-546: READY on RDC-CI GREEN, 10-31 (b0ca84330); merges with the next train.
  2. BL-554: READY 11-01 12:00Z.
  3. F01 (M3 exit 2; node level, D39: V's stage hook makes the durable write itself, then `kill -9`, restart, B invalid): plan 10-31 while BL-554 builds; READY 11-02 18:00Z.
  4. BL-407 (shutdown flushes re-report after a D18 AbortNode; reproduced on pasta's laptop): READY 11-03 18:00Z.
  5. CH3b b4b (the rdashd-level `-dbcrashratio` reorg loop; Core's `feature_dbcrash` stays blocked on MP1a/RP3a, RP1a D5): READY 11-05 18:00Z.
  6. CH3b b2b-2 (FTs `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`; `p2p_invalid_block` also needs P2b's inbound block path, which I check first): the three RPC FTs READY 11-07; `p2p_invalid_block` has no ETA until that check.
  7. BL-559 (needs BL-554's ordering): READY 11-08.
- 2026-10-31 04:00Z M3 exit 1, CH3's part (for the CLERK's M3 exits block): my open CH3 items are CH3b b2b-2, b4b and F01, in OD-CH3-3's order with its ETAs; CH3a a10 is the L3 owner's (DEC-976). Next blocking item: BL-546's RDC-CI verdict (snapshot of b0ca84330 asked of CI-S0 at 03:28Z), then BL-554, then F01 (Opus-h is writing its plan). ETA for CH3's part: 11-07, when b2b-2's three RPC FTs are due; `p2p_invalid_block` has no ETA until the P2b check. Replay readiness: as I read the rows, none of these items is a replay prerequisite; CH4's open slices (CH4-2, -3, -5, -7) are on the CH5 owner's row. gfast-gate-b0ca84330 was not stuck: run 1 ended 02:38Z on the BL-562 port case, and run 2 was GATE EXIT 0 at 03:21Z.

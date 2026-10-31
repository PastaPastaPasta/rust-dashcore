# MN0 owner log (DEC-973/DEC-976)

Owner thread: `thread:delegated-task:command%3Amcp%3A62932ed6-797a-49f6-a13c-b659d8b3724d%3Adelegate-task%3Anode-mn0-plan-0540`. Scope: MN0-2b, MN0-3 (dashd-mining). Plan: /work/scratch/mn0/PLAN-MN0.md (§9 = MN0-2).

- 2026-10-31 02:42Z Took ownership (DEC-976). MN0-1 ca76c146f and MN0-2a 108cb0349 are merged (T45); CH7a-4a bda7f2fae is merged (T55). All three are ancestors of integration 40ad3b544.
- 2026-10-31 02:42Z PLAN-MN0 §9.3 checked against CH7a-4a. **The code dependencies are met:**
  - `ChainQuery::evo_view` gives `tip_list`, `credit_pool` and `has_mined_commitment` (CH7a-3);
  - `dashd_chainstate::evo::guard::g1q`, with `G1qArm`, is exported (CH7a-4a).
- 2026-10-31 02:42Z **The acceptance evidence is not reachable yet, so MN0-2b WAITS.** MN0-2b's evidence is a SUT byte comparison from 1 to N past DIP0003 and through the DKG windows, where mined and mineable commitments appear (the two fences MN0-2a left to MN0-2b). The SUT can't connect those blocks at the tip:
  - **Missing piece 1: CH7a-4b** (CP; it waits on EV1-5b-2). With `-testactivationheight=dip0024@H` moved, llmq_test needs a null QC from height 442, so a DIP3-active chain stops 10 blocks past DIP0003 (DEC-845). CH7a-4b delivers the connect past the first QC and the dashd differential there. This is the gate for MN0-2b's moved-dip0024 chains and DKG windows.
  - **Missing piece 2: CH7b-2** (CP). On default regtest, G1q arm (c) refuses block 432 and every 24th block after it, so the default-args comparison past 432 needs G1q's removal. With MN0-2b using G1q as its NotReady predicate, it would match the node's refusal but prove nothing new there.
  - Starting MN0-2b now could only re-test chains MN0-2a already byte-matches (they end before the first commitment), so I'm not starting it. I'll plan it as soon as CH7a-4b merges, and run the default-args leg after CH7b-2.
- 2026-10-31 02:42Z Asked CP for a ping when CH7a-4b merges, and again when CH7b-2 does.
- 2026-10-31 02:42Z Cleanup: the T45 merge cleanup already removed the large MN0-1/MN0-2a scratch (mut-wt, mut2a-wt, mut2a-target, pf-base, target-2a). Two small files remain, fixture-head.json and mn0-evo-regtest.rerun.json (3 MB); rm-if-idle.sh removes directories only, so the CLERK can take them by rule. /work at 237G.

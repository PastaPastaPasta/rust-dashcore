# Train 18 notes (driver SB1)

Merged into node/integration as 409311277 (node/train-18 31ba48d01). T18 was rebuilt under DEC-266 without CH1-node BL-206, with the former follow-on train (train-18b) folded in. The D-rows below, T18-D1..D10, are copied from the driver scratch at merge.


## T18-D1 (04:42Z 10-19) branch
node/train-18 cut from node/integration 217b0b486 (T17 a47bf2f61 + docs) in /work/scratch/train18/wt, target reflink-seeded from the T17 wt. Rules: GATE ≤30 min (DEC-223); PF0 merge tree = measured tree; attribute with measure --label; take riders as they arrive.

## T18-D2 (05:15Z 10-19) riders 1–2
- ND1 F1 node/nd1-nodesut bf5c3e29a (follow-merged a47bf2f61; owner gfast nd1-sut-t18 EXIT 0 1617 s; bench SAME 13/13). Merged 50d05bd52, clean.
- CJ0 BL-217 node/o2a-cj1 5750d4bea (FF to a47bf2f61 + 1 commit; gfast bl217-cj-t1 EXIT 0 after the BL-20 rerun; no re-pin). Merged de7ac5c7d, clean.
- Workspace clippy --all-features --all-targets on de7ac5c7d: exit 0.
- Expecting CH1 BL-202/206, ND1 rpc-races, O8 BL-200, then SB14, O2a-GV BL-211 (ABI 27 reserved, so the bundle takes ≥28), SB8, P2b, O2b.

## T18-D3 (05:27Z 10-19) rider 3: P2b BL-214
node/bl214-p4a-nits ff21032a2 (P4a Opus-h r1 nits 1/2/4: FileCapture warn-once with a 1024 cap, the inbound-ban-expiry dashd v24 fixture + replay killing M7a/M7b, doc/DEC comments, D26). Branched from a47bf2f61, so no follow-merge was needed; 4 signed commits. Merged 23090b8ba, clean. Green under DEC-243/245 per the manager: owner tier 1 bl214-p4a-nits-t1 EXIT 1. The only failure was dashd-net sim_dial::connect_only, the known P2a-net D33 race (fix e9ef589a9 on node/p2a-net-wire, not yet in integration), and BL-214 touches neither sim.rs nor anything sim_dial runs. A solo rerun of the gate's binary passed 6/8 (≥5); all other steps passed (oracle-diff 6130); bench SAME 13/13. Cite P4a D26.

## T18-D4 (05:43Z 10-19) riders 4–5
- O8 BL-200 node/bl200-o8-testnet-goldens 5675025ae: testnet StateDigest goldens (manifest, sample, provenance, C4 results, extractor mnl counter rule); signed follow-merge of a47bf2f61. Owner gfast bl200-t18 EXIT 0 (1280 s; BL-20 rerun 37/37); bench SAME 13/13. Merged 27ab8af35.
- ND1 BL-221/BL-188 node/nd1-rpc-races 161cc76e8 (rpc races, test-only; follow-merge of a47bf2f61). Owner tier 1 nd1-rpcr-t18 EXIT 1, connect_only only (P2a-net D33), solo 5/9, so green under DEC-243/245 (ND1-D25). Merged ad138ea38; Cargo.lock auto-merged.
- Workspace clippy --all-features --all-targets --locked on ad138ea38: exit 0; node-fuzz lock OK.
- Ruling for the T18 gate until node/p2a-net-sim-d33 lands: a connect_only-only failure is green under DEC-243/245 with ≥5 solo passes. Stack d33 first if it arrives before the gate starts.

## T18-D5 (05:49Z 10-19) rider 6: CH1-node BL-202 + BL-206; tier 1 started
node/ch1-node-bl206 b9dc58faa (clean merge of node/ch1-node-bl202 e9eed72d1; both follow-merged onto a47bf2f61). Owner gates: ch1-bl206-t17 EXIT 0 (1493 s); ch1-bl202-t17 EXIT 1, connect_only only, solo 8/10, green under DEC-243/245 (integration a47bf2f61 is also 8/10); create_wallet cleared by BL-20 (37/0). Bench SAME 13/13, D62a mutants 7/7, dashd-chainstate lib 553/553. The owner confirmed the T17-D11 N7b-2 flush.rs port. Merged 60211e293; clippy --all-features exit 0.
T18 set: ND1 F1, CJ0 BL-217, P2b BL-214, O8 BL-200, ND1 rpc-races, CH1 BL-202/206. sim-d33 has not arrived, so it goes to the follow-on gate. Tier 1 gfast-train18 on 60211e293e. PF0 waits for t17-anchor-merge (perfgate lock).
- 06:06Z 10-19: T17 post-merge anchor ANCHOR EXIT 0 at b1e308e15 (112 rows). Removed /work/scratch/train17/wt (announced). T18 PF0 started on 60211e293 vs base 84e1bcbfd.
- 06:22Z 10-19: T18 tier 1 gfast-train18 on 60211e293: GATE EXIT 0 (1681 s; 3 dashd_sync failures cleared by BL-20). No selftest required. PF0 running.

## T18-D6 (06:23Z 10-19) follow-on stack node/train-18b (wt-b) on the gated 60211e293
- 1) P2a-net sim-d33 node/p2a-net-sim-d33 7b05b4a4b (D33 cherry-pick e9ef589a9, d0715cd07's SimExec::close guard + tests, D35 remove_disconnected + sim_linger.rs; owner: connect_only 30/30 alone and under load; gfast p2a-net-sim-d33 EXIT 0 1657 s; bench SAME 13/13). Merged 695bb93f9 first, per the manager.
- 2) P3 S2 node/p3-s2 aad82de8c (follow-merge of a47bf2f61, 086c2cc95 + D89 notes; owner p3-s2-t3 EXIT 1 = BL-20 rescan + connect_only 16/20 alone, green under DEC-243/245; bench SAME 18/18). Merged 6b5a015c7. sim/mod.rs and PROVENANCE.md auto-merged; transcript.rs keeps both downloads_blocks and P4a's check.
- I first merged S2 alone (1aaa9217d), then reset my local train-18b to 60211e293 to put d33 first.
- Checks on 6b5a015c7: clippy --all-features exit 0; dashd-p2p tests all ok; dashd-net sim_dial 11/11; dashd-p2p sim_linger 1/1.
- Tier 1 of train-18b starts now (gfast-train18b on 6b5a015c7). It contains the T18 tip, and integration differs only in docs; its PF0 runs after T18 merges.

## T18-D7 (06:38Z 10-19) follow-on rider 3: SB8 follow-ups; follow-on gate restarted
node/sb8-lq-followups 5c9939747 (BL-209(3) + BL-218: f647bbff0 rotated-legacy lock capture lq3r-regtest-v24.0 and last-pass islock status, plus the 5c9939747 nit fold; Opus-h r1 ACCEPT WITH NITS, nits folded and manager-checked; owner: dash-evo locks 46, node-l1 oracle match_cores 8 at ABI 26, clippy/fmt clean; no ABI change, pin 7188798effbf). Merged into train-18b after P3 S2: 9c7cff060. Workspace clippy --all-features exit 0. bench_inputs 9c7cff060 vs 60211e293: no change in any of the 13 bench packages. gfast-train18b on 6b5a015c7 stopped at test-build (cheaper than finishing plus a rerun); relaunched as gfast-train18b2 on 9c7cff060.
- 07:31Z gfast-train18b2 on 9c7cff060: GATE EXIT 0. Six dashd_sync(dash-spv) failures under load all passed on the BL-20 rerun (37/0). No selftest required. Wall time was 2996 s from slot acquisition (3147 s with queueing), over the DEC-223 30-min budget: the load average was about 72 at that time, and oracle-diff/test made up most of it. Reported to the manager.

## T18-D8 (07:35Z 10-19) ND1 2b-1 deferred to the next train
node/nd1-2b1 2eeaf7421 (dashd-node only, 6 files; Sol r1 ACCEPT WITH NITS; owner GATE-FAST nd1-2b1-r1 exit 0; bench_inputs SAME x13) was offered as a late follow-on rider. The manager's rule: take it only if gfast-train18b2 hadn't started building yet. It had already finished (GATE EXIT 0 at 07:31Z), so 2b-1 is not merged into train-18b and goes on the next train. train-18b stays at 9c7cff060.

## T18-D9 (07:48Z 10-19) T18 PF0 FAIL: 3 regressions; per-rider attribution running
t18-pf0 on 60211e293 vs base 84e1bcbfd (measured tree a6ec102a17bf; report 20261019T060626Z-60211e293e4c-vs-84e1bcbfd114) gave PF0 EXIT 1. Regressions in instructions:
- connect-serial-empty-av-off +3.669% (121.515M to 125.973M), with a DRIFT twin; read_bytes improved by 3.705%.
- feed-subscribe-at +0.916%.
- status-publish-coalesce +1.140%.
Four node-* rows were REFUSED as usual. No merge, per the manager's rule. Unit t18-attr runs perfgate measure --label t18-attr on these 3 workloads at base 84e1bcbfd and at each first-parent merge point (ND1 F1, CJ0, P2b, O8, ND1 rpc, CH1).
- 08:06Z Attribution (perfgate measure --label t18-attr, instruction medians; per-row JSON/MD in attr-*.json/md):
  - ND1 F1, CJ0, P2b, O8 and ND1 rpc are all ±0.000% on all 3 rows.
  - The CH1 merge 60211e293 carries everything. Split inside CH1: BL-202 e9eed72d1 is -0.036% / 0 / 0, and BL-206 b9dc58faa carries all of it.
  - connect-serial-empty-av-off +3.706% (121.471M to 125.973M): NEW, not covered by DEC-261, which never listed this row.
  - status-publish-coalesce +1.140%: exactly the DEC-261 waived size.
  - feed-subscribe-at +0.411% on measure (the DEC-261 record also lists +0.411% as passing). The gate's +0.916% comes from its base side reading 139.748M against 140.132M on measure. That is a base-slot difference, not a rider effect.
  - Verdict: a real, un-waived regression in BL-206. NOT merged; reported to the manager with options.

## T18-D10 (08:14Z 10-19) T18 rebuilt per DEC-266: CH1 BL-206 out, follow-on folded in
- The old tip 60211e293 is kept as local ref node/train-18-old-60211e293 until the new train merges.
- node/train-18 was reset to ad138ea38. Signed --no-ff merges:
  - ae230368c: CH1-node BL-202 e9eed72d1
  - 4b83e04d3: P2a-net sim-d33 7b05b4a4b
  - 8fd011295: P3 S2 aad82de8c (sim/mod.rs and PROVENANCE.md auto-merged again)
  - a95e2def5: SB8 5c9939747
  - 31ba48d01: ND1 2b-1 2eeaf7421
  No conflicts.
- Checks:
  - diff(a95e2def5, 9c7cff060) is byte-identical to diff(e9eed72d1, b9dc58faa), so BL-206 is the only removal.
  - diff(a95e2def5, 31ba48d01) is byte-identical to diff(bf5c3e29a, 2eeaf7421), the 2b-1 delta.
  - Clippy --all-features --all-targets --locked: exit 0 (clippy-r7.log).
  - bench_inputs 31ba48d01 against 84e1bcbfd, a47bf2f61 and 9c7cff060: no change in any of the 13 packages.
- BL-228 had reported no green selftest before tier 1, so it goes to T19.
- node/train-18b is retired: 9c7cff060 is superseded and gets no further gates. Its worktree wt-b and the branch ref will be removed after the merge (announced).
- Tier 1: gfast-train18r on 31ba48d01.
- 08:14Z PF0 started as unit t18r-pf0 on 31ba48d01. Base is node/integration ba6eec01c, which differs from 84e1bcbfd in docs only (DEC-266). The old PF0 log was kept as pf0-60211e293.log.
- 08:45Z gfast-train18r on 31ba48d01: GATE EXIT 0, 1843 s, no failures and no selftest required. PF0 t18r-pf0 is still running.
- 09:37Z T18 MERGED: node/integration 409311277 (signed --no-ff of node/train-18 31ba48d01 onto ba4cd5742, which is ba6eec01c plus docs only). Non-docs tree == PF0 measured tree e8c9d7f0659d. PF0 PASS (0 regressions; improved: coins-encode-p2pkh -3.0%, coins-encode-long -2.4%, coins-add-10k -1.4%, coins-overlay-block-1k -0.6%). The bench inputs are unchanged, so the anchor is still valid and no re-anchor is needed.

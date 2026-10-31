# TRAINS status log (DEC-973)

TRAINS (SB1) cuts and merges trains into node/integration end to end (OWNERS.md). A rider is any READY head an owner sends (PROCESS.md READY bar). Each cut runs the two-parent merge-tree pre-check (every rider against the tip and every pair), a signed trial merge, has-trims, `cargo metadata --locked`, and tests for any test helper one rider changes and another changes or calls (DEC-929). A train merges when its head is RDC-CI GREEN as a whole (DEC-783). Consensus riders also need the DEC-32 receipts, taken on the train head in the gate's DEC-32 receipt step. Merges take `/work/scratch/mgr/np-commit.lock`. Order: CP heads, then SW-V24's pin-move train, then oldest READY first. Pre-check notes live in `/work/scratch/train<N>/PRECHECK.md`.

## Outcomes

- 2026-10-30 21:47Z T55 MERGED as bda7f2fae. Riders: BL-548 d3dfaf863, CH7a-4a 172a4abba, BL-528 dd3531df5. RDC-CI GREEN run 41129504661 on 95b663b64. Post-merge node-fuzz skipped for the disk floor; the next nightly covers it.
- 2026-10-31 01:19Z T56 CUT as b7779ce14 on integration bf0d54e70 and sent to CI-S0. Riders: BL-523 5b4a52160, BL-275 + BL-122 6e171c1bc, BL-547 097d3b611, node-gate v12 0b3629da1. Pre-check clean: no conflicts against the tip or between riders. Helper tests passed: dashd-chainstate lib 888, dashd-node lib 237. Receipts (DEC-32) come on the GREEN head before the merge.
- 2026-10-31 02:11Z T56 MERGED as 6a3a3aa57 (parents 3257e1f0f, b7779ce14). RDC-CI GREEN run 41155995964 on b7779ce14 (v12, 10 jobs). DEC-32 receipt step on b7779ce14: 4 receipt PASS lines, exit 0. Riders: BL-523 5b4a52160, BL-275 + BL-122 6e171c1bc (ND1); BL-547 097d3b611 (CP); node-gate v12 0b3629da1 (CI-S2).
- 2026-10-31 05:24Z T57 CUT as 1949851e1 on integration 4a9657105 and sent to CI-S0. Rider: BL-558 3a206f8b5 (ND1; adds rdashd/README.md only; head GREEN run 41170234747). Merge-tree clean; has-trims and `--locked` OK. Docs only, so no helper tests and no DEC-32 receipts.

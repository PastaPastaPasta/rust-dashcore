# CI-S0 status log

CI-S0 owns fork snapshots, RDC-CI lanes and verdicts, and CI triage (OWNERS.md, DEC-973). It takes snapshot requests straight from landing owners. Queue order: CP heads, then SW-V24's pin-move parts, then FIFO. Verdicts go to the requesting owner, and to TRAINS for train heads. Only E3 items go to the manager.

Lanes export node-gate v12 (node/ci-stage2 0b3629da1, fork master 867e9056e; DEC-954). v13 (node/ci-stage2-rc5 7207e31f8, rc.5 ST2_DASHD) is used only for rc.5 trees until the pin-move train merges; fork master and `lane.sh` move to it in the same step as that merge (DEC-971).

## Outcomes

- 2026-10-30 T55 95b663b64: RDC-CI GREEN, run 41129504661 (v11).
- 2026-10-30 node-gate v12 trial on integration f8226ebcb: RDC-CI GREEN, run 41129572281; thread-count passed (baseline 3, peak 9, bound 9).
- 2026-10-30 BL-547 2ece6d4ad: RDC-CI GREEN, run 41135624580 (v11; superseded by the DEC-953 fix round).
- 2026-10-30 SW-V24 S4 gate-env swap: node/sw-v24-rc5-gate-env 7f04e0447 (rc.5 release sha256 checked against SHA256SUMS and both `.asc` signatures; CACHE_GEN unchanged). Trial run 41142168731 RDC-CI RED, every red belongs to S13: dash-rpc-spec `dashd_help::help_matches_dashd`, rdash-cli `xcli::{xcli_dashd, xcli_deviations, xcli_offline}` (CORE_VERSION still rc.4), rdashd `dashd_v24::the_coins_setup_options_help_is_dashd_s` (`-assumevalid` help defaults, CE5-02). rpc_diff and p2p_silence passed (DEC-971).
- 2026-10-31 T56 riders on v12: BL-523 5b4a52160 GREEN run 41144773658; BL-275 6e171c1bc GREEN run 41146611197; BL-547 097d3b611 GREEN run 41149934647.
- 2026-10-31 BL-546 6f86fced5: dropped before export (fix round, DEC-969).
- 2026-10-31 DEC-973: CI-S0 runs as a landing owner. The queue is empty.

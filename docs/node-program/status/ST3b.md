# ST3b status log (DEC-973, DEC-976)

Outcome lines only, newest last. Scope: ST3b S6–S8 per PLAN-ST3b.md §7 (approved DEC-698), BL-541; BL-506/507/508 when CH7a/b, CH5 and CH8 land. Plans and reviews: /work/scratch/st3b/.

- 2026-10-31 02:42Z Owner thread registered in OWNERS.md. S0–S5 merged (T53, head 8bfee20c5). DEC-976 GO for S6–S8. Campaigns run only at /work ≥ 260G and 5-min load < 80, as bounded `systemd-run --user` units, yielding to CP and SW-V24; devices swept after every run.
- 2026-10-31 02:42Z **Slice order and ETAs** (estimates; campaign wall time depends on the box caps):
  1. **S6a dm-flakey** (FL-IBD, FL-FLUSH, FL-WAL; wrapper `flakey` verb; fail-stop, no `UpdateTip` after the abort, C0–C3 on the full-log image, C5 on restart): branch `node/st3b-s6`, implementation and short tests by 11-01 06:00Z, reviews (Sol → Opus-h, storage durability) and CI-S0 → READY by 11-01 18:00Z.
  2. **S6b disk full** (DF-PRE, DF-MID, DF-PL): on the same branch after S6a, READY together with S6a (one landing).
  3. **W-40k every-point campaign** (M3 exit 2, sharded ≤ 8 units; unblocked by BL-525 in T52): first window at ≥ 260G and load < 80 after S6's code is in review; ETA 11-02, wall time to be logged.
  4. **S7 VM harness** (Docker QEMU image, guest initramfs, V-IBD/REORG/FLUSH/RECOVERY, V-REINDEX stub → BL-507): branch `node/st3b-s7`, READY by 11-03 18:00Z, then its weekly target run (≥ 50 kills per scenario on ext4, ≥ 20 on xfs).
  5. **S8** (kill smoke, nightly/weekly units and ledger, runbook, notes; one nightly-device and one weekly-VM run green at the READY head): READY by 11-04 18:00Z.
  6. **BL-541** (4 KiB-unit T18 split campaign, 128 points per fs): a nightly slot after S8, no ETA.

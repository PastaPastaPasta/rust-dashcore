# CH5 status log (DEC-973, DEC-976)

Outcome lines only, newest last. Plan: /work/scratch/ch5/PLAN.md.

- 2026-10-31 02:36Z Owner thread registered in OWNERS.md; slice plan in progress.
- 2026-10-31 02:50Z PLAN rev0 written (/work/scratch/ch5/PLAN.md): slices CH5-1 verifychain 3–4 + startup check (BL-524/440/119/275/278b, abortnode restart, rpc_invalidateblock), CH5-2 -reindex-chainstate + -dbcrashratio, CH5-3 -reindex + -loadblock (BL-549/528/507/29), CH5-4 regtest Core-datadir auto-import (13 cache FTs), CH5-5 live-copy m-6, CH5-6 measurements (DEC-834/835). BL-508 → CH8, BL-431 → CH6b (out). Sol-h plan review r1 running.
- 2026-10-31 02:56Z ETAs (provisional until the plan review closes; CH5's share of each exit, PLAN §5):
  - exit 1: `verifychain 4 0` parity 11-06 (CH5-1); the `-reindex-chainstate` replay vehicle 11-11 (CH5-2); the replay itself is not CH5's.
  - exit 5: rpc_invalidateblock and feature_abortnode 11-06; feature_loadblock 11-17; the 13 chain-cache tests 11-20. feature_reindex waits on IX1 and feature_dbcrash on MP1a/b.
  - perf exit 6: rdashd half (DEC-834) 11-21, if the manager grants a mainnet datadir source by 11-11.
  - next blocking item: Sol-h plan review r1, then CH5-1.

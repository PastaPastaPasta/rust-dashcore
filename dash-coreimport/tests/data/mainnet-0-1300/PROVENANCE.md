# mainnet-0-1300

Dash mainnet blocks 0–1300 (`blk00000.dat`) and the undo records of blocks 1–1300
(`rev00000.dat`), copied byte for byte from the block files written by the reference oracle
(Dash Core v24.0.0-rc.3, `/work/chains/mainnet/data/blocks`, see `docs/node-program/ORACLE.md`)
on 2026-10-08.

- Generated with `dashd-store-bakeoff fixture --blocks <copy of the oracle's blocks/> --anchor
  000000aab01a883a7689a567b7a912399f09ec59e967d8eb84ad356c7a4dc51f --out <this dir>`.
- Blocks are re-ordered into height order and the undo records into one rev file; the record
  framing (`magic ‖ size ‖ payload [‖ checksum]`) is Core's. The undo record of height 2 appears
  once more before height 1's record, so the extractor has one non-matching record to skip.
- Expected values from the oracle (`dash-cli gettxoutsetinfo none 1300`): `txouts` = 1269,
  best block `000000aab01a883a7689a567b7a912399f09ec59e967d8eb84ad356c7a4dc51f`.

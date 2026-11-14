# CH5-1b pool and template answers: provenance

dashd's answers that `src/sut/tests/recorded.rs` compares rdashd's with (CH5-1b, BL-581, plan
§4 kinds 2 and 3): `quorum dkgstatus`'s `minableCommitments[]` and a `getblocktemplate`'s
commitment transactions and CbTx, at Leg B's and Leg C's observation tips, before any walk and
after each `verifychain` the test makes there.

- Recorded by `record.py` (this directory) on 2026-11-12 with dashd v24.0.0 from
  `contrib/setup-dashd.py`, archive checked against `SHA256SUMS.asc`:
  `python3 dashd-node/tests/data/ch5-1b-pool/record.py <dashd>`.
- Each file's `provenance` names dashd's version and its arguments. Each point's `history` is
  the node's starts, stops, submits (the height reached), `setmocktime` and walks up to it.
- At every tip the answers before and after each walk are equal; the recorder stops
  otherwise. They also equal the step-0 probe's answers, recorded the same day on separate
  nodes.
- Kept fields: the twelve commitment fields of each mineable commitment; the template's
  `previousblockhash`, `height`, type-6 transactions as bytes in order, and the
  `coinbase_payload` decoded to `version`, `height`, `merkleRootMNList`,
  `merkleRootQuorums` and, from version 3, `creditPoolBalance` (hashes as serialized).
  Dropped: `quorum dkgstatus`'s `quorumConnections`; the template's times, bits, target,
  long-poll id and other transactions; the CbTx's `bestCLHeightDiff` and `bestCLSignature`
  (the node's best ChainLock is runtime state the walk does not own).

## `legB.json`: R1's M (`dashd-chainstate/tests/data/v19-window/r1`)
R1's `chain_args` and a clock a minute after M's last block. No restart and no reorg.
- **249** (`395cac81…e58b`): cycle 240's window is open and nothing is mined. Nulls for types
  100, 104 and 106. Walks: `3 0`, `4 0`.
- **253** (`40c26a07…24aa`): 251 mined types 100 and 104, so only 106's null is left. Walks:
  `3 0`, `3 3` (which reaches 251), `4 0`.

## `legC.json`: `feature_llmq_chainlocks`' node0 (`test-vectors/statedigest/regtest-v24`)
The recording's `chain_args` with `-assumevalid=0`. Synced to 338, stopped and started, as Leg
C restarts it. Then each tip is submitted, with `setmocktime` a minute after it, so the
template is served outside IBD.
- **345** (`63adc39e…4d76`): cycle 336's window is open and nothing is mined. Nulls for types
  100 and 106. Walks: `3 0`, `3 8`, `4 0`.
- **352** (`5adf1317…6c80`): 347 and 348 mined types 100 and 103, so only 106's null is left.
  Walks: `3 0`, `3 8`, `4 0`.

# evo-deep-window chain recorder (BL-646)

The `evo-deep-window` growth guard (CH7 §7.1) needs a long regtest chain with non-null quorum
commitments every DKG cycle, and a side branch whose reorg leaves erased rows (tombstones) in the
mined-commitment index. `record.sh` records one from a Dash Core release. The chain is not committed.
Recordings mint fresh BLS keys every run, so a chain is kept with its sha256s, not regenerated.

The recorder lives here, beside `../evo-deep-window.sh` (the guard that consumes the chain), because
it is operator tooling: nothing under it is a test fixture.

## Chain

- **Nodes:** a controller (node0, the only miner) and 4 EvoNodes, so all three non-rotating types,
  `llmq_test`, `llmq_test_instantsend` and `llmq_test_platform`, form 3-member quorums (platform takes
  EvoNodes only).
- **Chain args:** `-dip3params=2:2`; `dip0024`, `v20` and `mn_rr` at 1,000,000; and
  `-vbparams=testdummy:-2:0` (never active). Every block from height 2 has a version 2 CbTx, and no
  rotation, credit pool or ChainLock field enters a block. TESTDUMMY would otherwise activate by
  signalling at 432 and enable `llmq_test_v17`, whose mining window then needs null commitments of a
  fourth type. V24 and EVO_SHARES stay inactive because no MNHF signal is mined. The run asserts all
  of this on every node at the start and the end.
- **Sporks:** 17 (DKG) on; 2 (InstantSend) and 19 (ChainLocks) off. A ChainLock on M would refuse S.
- **M (`chain.txt`):** after the setup blocks, every 24-block cycle has the same shape. The three DKGs
  run at the same base. The recorder waits for each phase on every member of every type, and for all
  three final commitments on node0, before it mines the commitment block. Null commitments go at base+10
  and the three non-null ones at base+11, then plain blocks follow to the next base. The recorder mines
  until the tip reaches `--blocks` (or `--cycles` cycles), and the file holds blocks 1..N.
- **S (`fork.txt`):** off M's block P, just below the first cycle base at or above N - `--fork-depth`.
  The recorder invalidates M's block P+1 on every node, then mines `--side-blocks` blocks. They hold as
  many full cycles as fit, then plain blocks that stay outside a mining window. S's quorums have S's
  own base blocks. node0 mines base+9..base+11 while isolated (`setnetworkactive false`), so no final
  commitment can reach it before base+12. S's commitments therefore sit at base+12. Neither index key,
  `q_mc` (type, quorum hash) or `q_mcih` (type, mined height), is shared with M, and a reorg back to M
  leaves S's rows erased. Reconsidering the block brings every node back to M, which has more work.
- **Asserts** (the run fails on any): every cycle block's CbTx version and commitments against the
  pattern; `quorum list` naming the new quorum for each type; no PoSe penalty or ban; the deferred heights
  on every node; S's quorum hashes and mined heights absent from M; every node back on M's tip.

## Files

`record.sh` writes `<work-dir>/out`, with the datadirs in `<work-dir>/ft`. The datadirs are deleted
after a passing run.

- `chain.txt.zst` and `fork.txt.zst`: one block's hex per line after three `#` header lines: the
  `dashd -version` line, `# dashd -regtest <chain args>`, and how the blocks were made. S's third line
  starts `fork off P <hash>:`, and its blocks follow at heights P+1 onward.
- `summary.json`: chain args, types, EvoNodes, sporks, and each M and S cycle (base, quorum hash, mined
  height, seconds).
- `verify.txt`: `verify_chain.py`'s report. It decodes every block itself, independent of the RPCs.
  It checks linkage by X11 hash, the CbTx version and height, the per-cycle commitment pattern, and the
  distinct S keys. It also gives counts per 1,000 blocks.
- `PROVENANCE`: recorder commit, Core tag and commit, dashd sha256, command, wall and CPU time, the
  sha256 of each file (before and after zstd), and `verify.txt`.

## Run

```
contrib/node/evo-deep-window-chain/record.sh ~/workspace/dash v24.0.0 \
    ~/.rust-dashcore-test/dashcore-24.0.0/bin <work-dir> [--blocks 20100] [--fork-depth 1150] [--side-blocks 100]
```

A pilot: `--cycles 20 --fork-depth 120 --side-blocks 48`. `LQ3A_PORTSEED` (default here 9173) and
`LQ3A_TIMEOUT_FACTOR` (8) pass through to `../lq3a-captures/run.sh`, which exports the tag's `test/`
tree, checks that `dashd -version` matches the tag, and builds a venv with `dash_hash`.
`BL646_KEEP_DATADIRS=1` keeps the datadirs. A work dir whose `out/` already holds a recording is refused
unless `BL646_OVERWRITE=1`.

## For a loader

- S's first blocks, base..base+10 of its first cycle, have the same coinbase txids as M's blocks at those
  heights. generate pays one address, and the CbTx roots match on both branches until M's commitment block.
  A txid-keyed store sees the same txid on both sides of the reorg.
- `chain.txt` stops at N. M's live tip during the recording was the end of the last cycle (N + 7 for the
  full run). Those blocks are not recorded.

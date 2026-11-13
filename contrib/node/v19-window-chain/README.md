# V19-window chain recorder (BL-648, BL-839)

The V19 basic-scheme switch needs regtest chains where V19 activates above height 1, with DIP0003
active and evo content on both sides of the switch. `record.sh` records three from a Dash Core release:
`dashd-chainstate/tests/data/v19-window/{r1,r2,r3}/` keeps them with their PROVENANCE. Recordings mint
fresh BLS keys every run, so a chain is kept with its sha256s, not regenerated.

## R1, `reorg`

- **Chain args:** `-dip3params=2:2`, V19 at 212 (N); DIP0024, V20 and MN_RR at 1,000,000;
  `-vbparams=testdummy:-2:0`. Every CbTx is version 2.
- **Sporks:** 17 on; 2 and 19 off. A ChainLock on M would refuse S.
- **Nodes:** a controller (node0, the only miner) and 4 masternodes with legacy operator keys
  (`-deprecatedrpc=legacy_mn`). `llmq_test` and `llmq_test_instantsend` run 3-member DKGs at 168, 192
  (legacy) and 216, 240 (basic). `llmq_test_platform` takes regular masternodes while V19 is not active
  after its base (`llmq/utils.cpp:294-295`), so it gets real commitments at 168 and 192 and null ones after.
- **M (`chain.txt`, 1-259):** a ProUpServTx v1 at 210; a fifth masternode's ProRegTx v2 at 212;
  a ProUpRegTx v2 moving a legacy key to basic at 213; a ProUpRevTx at 214 (v2: `protx revoke` writes
  v2 once V19 is active); the moved masternode's reviving ProUpServTx v2 at 215.
- **S (`fork.txt`, 209-214):** off 208. node0 invalidates 209 on every node and mines plain blocks with
  `generateblock` to 214, crossing N at S's own 212; then `reconsiderblock` returns every node to M.
  At S's 210 the run also asks dashd to check a block at 211 holding M's ProRegTx v2 (N1):
  `bad-protx-version`.
- **Probe:** a fresh dashd replays M to 215, `invalidateblock 209`, S, a restart on S's 214,
  `reconsiderblock 209` and the rest of M, recording the scheme lines of each step's `debug.log` window.

## R2, `v20-first`

- **Chain args:** `-dip3params=2:2`, V20 and MN_RR at 100, V19 at 236 (N2), DIP0024 at 1,000,000,
  testdummy as R1. Every CbTx from 100 is version 3.
- **Sporks:** 17 and 19 on, 2 off. A raw P2P observer on node0 keeps each `clsig` wire payload.
- **Chain (`chain.txt`, 1-290):** the same types' cycles at 168, 192, 216 (legacy) and 240, 264
  (basic). Core's miner keeps a ChainLock below the V19 height out of the CbTx
  (`node/miner.cpp:117-124`), so every CbTx from 100 to 236 carries a null ChainLock, and 237's carries
  the basic ChainLock on 236 at difference 0.
- **`switch-refused.txt`:** 236 with its CbTx rewritten to carry the ChainLock on 234 (its `clsig`'s
  legacy bytes) at difference 1, re-solved as `feature_llmq_chainlocks.py`'s `test_bestCLHeightDiff`
  shapes CbTxs. Core never mines it. This recording's 234 signature reads as no basic point, so dashd
  answers `bad-cbtx-cldiff` (a null signature at difference 1); one that reads as a basic point would be
  verified instead.
- **Probe:** (a) a fresh dashd takes the chain by `submitblock` alone; (b) a second one also takes each
  recorded ChainLock by `submitchainlock` at the tip where node0 first saw it. Each gets
  `switch-refused.txt` at tip 235: (a) holding no ChainLock, (b) holding the one on 234 as its best.

## R3, `v1-above` (BL-839)

- **Chain args:** R1's, with V19 at 150 (N). **Sporks:** 2, 17 and 19 off: no DKG, every commitment is
  null.
- **Core's rule:** a v1 ProTx stays valid above V19 (the version check is a maximum,
  `evo/specialtxman.cpp:1172-1176`). Its operator signature is read in the legacy byte encoding
  (`evo/providertx.h:307-310`, `:425-428`) but verified under the block's scheme, not the payload's version
  (`CheckHashSig`, `evo/specialtxman.cpp:1148-1155`, `:1482`, `:1617`; `bls/bls.cpp:323-326`). So above N a
  v1 payload needs a basic-scheme signature. dashd's RPC writes v1 only while V19 is not active after the
  tip, and signs it under legacy (`evo/providertx_service.cpp:351`).
- **Chain (`chain.txt`, 1-160):**
  - At 148, the RPC writes a ProUpServTx for masternode A (a new port) and a ProUpRevTx for masternode C
    (reason 2), v1 and unsent.
  - The recorder signs each payload again under the basic scheme with `py_ecc` (`record.sh` installs
    `py_ecc==8.0.0` in the run's venv) and writes the signature in the legacy encoding.
  - Those are mined at 150 and 151.
  - `testmempoolaccept` gives: legacy-signed allowed and basic-signed `bad-protx-sig` at 148; the other
    way round at 149 (B).
- **`serv-refused.txt`, `rev-refused.txt`:** 150 and 151 with the legacy-signed transaction in place of the
  basic-signed one, re-solved. dashd answers `bad-protx-sig`.
- **Probe:** a fresh dashd takes the chain. It runs the four `testmempoolaccept`s at 148 and 149, and C's at
  150. It gets `serv-refused.txt` at 149 and `rev-refused.txt` at 150, and runs `protx info` at 149, 150,
  151 and 160.

## Files

`record.sh` writes `<work-dir>/out`. The datadirs (`<work-dir>/ft`, `<work-dir>/probe/ft`) are deleted
after a passing run.

- `chain.txt`, `fork.txt`, `switch-refused.txt`, `serv-refused.txt`, `rev-refused.txt`: one block's hex per line after three `#` lines: the
  `dashd -version` line, `# dashd -regtest <chain args>`, and how the blocks were made.
- `clsigs.jsonl`: each ChainLock's height, block hash, wire hex, and arrival tip (the lowest of
  `getbestchainlock`'s first naming, the drain that captured its payload, and one below the first CbTx
  carrying it).
- `summary.json`: args, masternodes, cycles, events, state versions, each CbTx's ChainLock (R2), the ProTx
  variants and their mempool and `generateblock` verdicts (R3).
- `verify.txt`: `verify_chain.py`'s report. It decodes every block itself: linkage by X11 hash, CbTx
  version and height, the cycle pattern, ProTx versions, and each mode's shape.
- `probe.json`, `probe.txt`: the probe's verdicts and scheme lines. `verify_chain.py --probe` checks each
  verdict against the one the mode expects (R1: each step's scheme lines; R2: no refusal, every
  `submitchainlock` taken, both `switch-refused.txt` verdicts `bad-cbtx-cldiff`; R3: the mempool and block
  verdicts above, and `protx info` equal to the recorder's). It exits 1 on any other verdict, which fails
  `record.sh`.
- `PROVENANCE`: recorder commit and file sha256s, Core tag and commit, dashd sha256, command, times,
  output sha256s, `verify.txt` and `probe.txt`.

## Run

```
contrib/node/v19-window-chain/record.sh ~/workspace/dash v24.0.0 \
    ~/.rust-dashcore-test/dashcore-24.0.0/bin <work-dir> reorg|v20-first|v1-above
```

`LQ3A_PORTSEED` (default here 9481) and `LQ3A_TIMEOUT_FACTOR` (8) pass through to
`../lq3a-captures/run.sh`, which exports the tag's `test/` tree, checks that `dashd -version` matches
the tag, and builds a venv with `dash_hash`. `BL648_KEEP_DATADIRS=1` keeps the datadirs. A work dir
whose `out/` already holds a recording is refused unless `BL648_OVERWRITE=1`.

# dashd-chainstate test data

## `cs6_checkqueue_v24.json` (CS6)

What Dash Core's `ConnectBlock` reported for blocks with several invalid transactions, serially
and through script-check queues of several sizes, recorded by
`contrib/node/cs6-checkqueue-vectors.py` with the release binary on 2026-10-15 (`par1`, `par2`)
and again on 2026-10-26 with five modes (CH3a a9, DEC-323):

| File | Release | `dashd` sha256 |
|---|---|---|
| `cs6_checkqueue_v24.json` | v24.0.0-rc.3 (`~/.rust-dashcore-test/dashcore-24.0.0-rc.3`, from `contrib/setup-dashd.py`) | `e204e3163185…7ab5bb4` |

```bash
PYTHONPATH=<dir with the dash_hash extension> contrib/node/cs6-checkqueue-vectors.py \
    --dashd <dashd> --framework <git archive of test/functional at v24.0.0-rc.3 (0364ed626e)> \
    --datadir <empty dir> --port <p> --rpcport <q> --out cs6_checkqueue_v24.json
```

Five nodes, one per `-par`, share a regtest chain: `par1` (`-par=1`, no script-check threads),
`par2` (`-par=2`, one), `par16` (15, the cap), `par-40` (leaving 40 of the host's 48 online
processors free: 7) and `par-1000` (none). The file records each node's threads, from its
"Script verification uses N additional threads" line, and `num_cores` (Python's `os.cpu_count()`,
the online processors), so the replay checks Core's arithmetic without depending on the host. The
chain: 101 blocks to `raw(51)` and a funding block (height 102) whose transaction pays 240 P2PKH
outputs. Each of the ten cases is a block at height 103 that the script builds and solves (X11
through `dash_hash` 1.5.1), given to each node as `generateblock … false` (`TestBlockValidity`,
which stores in the caches) and then `submitblock` (block connection, which does not). The file
keeps both results, `ConnectBlock`'s `debug.log` lines, the funding transaction and
`getdeploymentinfo` (every buried script rule active from height 1 or 0). The clock is mocked and
signatures are RFC 6979, so two runs on a host with as many online processors write the same bytes
(sha256 `da82854a…00f17678`); the 2026-10-26 run's `par1` and `par2` results are the 2026-10-15
file's, byte for byte.

`tests/l3_checkqueue.rs` replays every case in every mode.

## `ch7a4b/` (CH7a-4b)

dashd's regtest chain with DIP0003 at 48 and DIP0024 moved out of the window, recorded by
`dashd-node/tests/data/ch7a4/record.sh` (CH7-notes D70's recorder, taking a block count and a
fork prefix since CH7a-4b) on 2026-10-31:

| File | Release | `dashd` sha256 |
|---|---|---|
| `ch7a4b/blocks.txt`, `fork-a.txt`, `fork-b.txt` | v24.0.0-rc.4 (`~/.rust-dashcore-test/dashcore-24.0.0-rc.4`, from `contrib/setup-dashd.py`) | `e1be2564c5f9…b4f823a7` |

```bash
dashd-node/tests/data/ch7a4/record.sh <empty dir> blocks.txt 200 fork
```

dashd runs `-regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000`.
`blocks.txt` is `generatetoaddress 200` to rpc_generate's P2PKH address. `fork-a.txt` is
`invalidateblock` of 62, 150 blocks from 61 to `raw(51)`, then `reconsiderblock`; `fork-b.txt` is
the same from 40 for 7 blocks. Each file starts with `#` lines naming the release, the arguments
and the commands; then one block's hex per line, at the heights after the block it names. The
clock is mocked: the same unit's 57-block rerun of the script reproduced CH7a-4a's
`dashd-node/tests/data/ch7a4/blocks.txt` blocks byte for byte (that file now carries the header).

## `ch7a5/` (CH7a-5a)

dashd's regtest chain with masternodes, on CH7a-4b's settings, recorded by
`dashd-node/tests/data/ch7a4/record.sh`'s `mn` mode on 2026-10-31:

| File | Release | `dashd` sha256 |
|---|---|---|
| `ch7a5/blocks.txt` | v24.0.0-rc.4 (`~/.rust-dashcore-test/dashcore-24.0.0-rc.4`, from `contrib/setup-dashd.py`) | `e1be2564c5f9…b4f823a7` |

```bash
dashd-node/tests/data/ch7a4/record.sh mn <empty dir> <out dir>
```

dashd runs `-regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000`.
A wallet mines all 300 blocks; `protx register_fund_evo` (an EvoNode, legacy platform ports) and
`protx register_fund` confirm at 116 and 117, `update_service_evo` at 141, `update_registrar` (a
new operator key) at 161, `revoke` (reason 2) of the regular node at 181, and a spend of its
collateral at 201. The header's `# HEIGHT KIND TXID` lines name each, then the operator keys;
then one block's hex per line. The wallet's keys are random, so a rerun records other bytes of
the same shape. No masternode runs: every commitment is null, and V20 (432) is past the chain.

## `v19-window/` (BL-648)

Two regtest chains with V19 above height 1 and DIP0003 active, recorded on 2026-11-10 by
`contrib/node/v19-window-chain/record.sh` at commit `2175152cecd976432d4ad94dacdf4694a60955ec` (clean
tree). Later edits to the recorder (8c6a714bd) change comments and the README only. Each runs the stock v24.0.0 `test/functional/` tree (Core `938fba502824`) through
`contrib/node/lq3a-captures/run.sh` against the release binary; the recorder's README gives each
chain's shape.

| Files | Release | `dashd` sha256 |
|---|---|---|
| `r1/`, `r2/` | v24.0.0 (`~/.rust-dashcore-test/dashcore-24.0.0`, from `contrib/setup-dashd.py`) | `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223` |

```bash
contrib/node/v19-window-chain/record.sh ~/workspace/dash v24.0.0 \
    ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r1 reorg       # LQ3A_PORTSEED=9481
contrib/node/v19-window-chain/record.sh ~/workspace/dash v24.0.0 \
    ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r2 v20-first   # LQ3A_PORTSEED=9617
```

- **R1** (`reorg`): `-dip3params=2:2 -testactivationheight=v20@1000000
  -testactivationheight=mn_rr@1000000 -testactivationheight=dip0024@1000000 -vbparams=testdummy:-2:0
  -testactivationheight=v19@212`. `chain.txt` is M, blocks 1-259 (tip `6896c7a9…6ce2d823`);
  `fork.txt` is S, 209-214 off 208 (`7b054b7c…e55414eb`). ProTxs: 210 ProUpServTx v1, 212 ProRegTx
  v2, 213 ProUpRegTx v2, 214 ProUpRevTx v2, 215 ProUpServTx v2. `summary.json` keeps each event's
  txid and hex, and each masternode's state version, `pubKeyOperator`, `PoSeBanHeight` and
  `revocationReason` at 211, 212, 214 and 259 (`protx info` at the block). At S's 210, dashd's
  `generateblock` check of a block holding 212's ProRegTx answered `bad-protx-version`.
- **R2** (`v20-first`): `-dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100
  -testactivationheight=dip0024@1000000 -vbparams=testdummy:-2:0 -testactivationheight=v19@236`,
  sporks 17 and 19 on. `chain.txt` is blocks 1-290 (tip `088bf335…c71c70a3a`). Every CbTx from 100 to
  236 carries a null ChainLock (`node/miner.cpp:117-124`); 237's carries the ChainLock on 236 at
  difference 0. `clsigs.jsonl` holds the 88 ChainLocks node0 took, each with its `clsig` wire hex
  and arrival tip. `switch-refused.txt` is 236 (`446160af…`) with its CbTx carrying the ChainLock on
  234 (`0f7c0945…`, its `clsig`'s legacy bytes) at difference 1, re-solved
  (`5cdbdcb2fa58…`); Core never mined it. Those bytes read as a legacy signature and as no basic
  point, so at 236 the CbTx holds a null signature at difference 1 (`evo/specialtxman.cpp:209-211`):
  `bad-cbtx-cldiff`. A legacy signature that also read as a basic point would be verified instead.

The probe, fresh dashd v24.0.0 nodes with the chain's args (R1 one, R2 two):

- R1: M to 215, `invalidateblock 209`, S, a restart on S's 214, `reconsiderblock 209`, M to 259. The
  scheme lines of each window: `ProcessSpecialTxsInBlock -- bls_legacy_scheme=0`; `entered
  bls_legacy_scheme=1`; `ProcessSpecialTxsInBlock -- bls_legacy_scheme=0`; the restart's `AppInitMain:
  bls_legacy_scheme=0`, then VerifyDB's 6 blocks at level 3 crossing B (`UndoSpecialTxsInBlock --
  bls_legacy_scheme=1`, `reverted bls_legacy_scheme=0`); `entered bls_legacy_scheme=1`,
  `ProcessSpecialTxsInBlock -- bls_legacy_scheme=0`; none.
- R2: both take the chain to 290 with no refusal, (b) also each recorded ChainLock by `submitchainlock`
  at its arrival tip (88, no error). `switch-refused.txt` at tip 235: `bad-cbtx-cldiff` from (a)
  holding no ChainLock and from (b) holding the one on 234.

| File | sha256 |
|---|---|
| `r1/chain.txt` | `4a4e42681c9c0f0d2ecc07acbe2af45886182f591192a0a7e035bbe9a9d7b705` |
| `r1/fork.txt` | `97ff1ea052ee29e2b17ffa7979884c13ea98a9f2907031c5cad359c992905914` |
| `r1/summary.json` | `0ec89010476ced2693409aee4e6b11f60c3d3f9d69672f8ae4f9d991c34192e7` |
| `r2/chain.txt` | `cd2ac7fc115a3ccaa3cfc8f29d782e3a08ec4f649acc38cb5c20fa7e9461eb61` |
| `r2/clsigs.jsonl` | `22c3d4101684f811e9cdb02cbfa0f55032ed47216f1f795fe7749f0e3a588004` |
| `r2/switch-refused.txt` | `077156f366a8e58f08251a2abfebe837f90563772b2ba3ebc9a06772f2096abb` |
| `r2/summary.json` | `38e86be9a5a59e7348c7272a69ba5e030f03cf47d0358ee6e43f7823aa405516` |

The tests: `src/chain/tests/v19_window.rs` (V over the node's connector),
`dashd-node/tests/v19_window_dashd.rs` (against dashd v24.0.0), `rdashd/tests/v19_reorg.rs` (both
nodes' logs).

### `v19-window/r3/` (BL-839)

A third chain, recorded on 2026-11-10 by the same `record.sh` at commit
`f4b30c6a39b075ea3f8092d59a74f60de5ff52a8` (clean tree; capture script sha256 `01c3f1af…ec689be78b`).
It runs against the same v24.0.0 binary (`dashd` sha256 `2ad05ccb…fcd732223`). The run's venv adds
`py_ecc==8.0.0`, the BLS signer below. A later recorder edit (4b8e1fe81) tightens `verify_chain.py`'s
check of the refused blocks and keeps `addresses` in R1/R2-mode summaries out. R3's capture path is
unchanged: a scratch re-run at 4b8e1fe81 recorded and probed R3 with every verdict as expected.

```bash
contrib/node/v19-window-chain/record.sh ~/workspace/dash v24.0.0 \
    ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r3 v1-above    # LQ3A_PORTSEED=9481
```

- **R3** (`v1-above`):
  - Args: `-dip3params=2:2 -testactivationheight=v20@1000000 -testactivationheight=mn_rr@1000000
    -testactivationheight=dip0024@1000000 -vbparams=testdummy:-2:0 -testactivationheight=v19@150`.
  - Sporks 2, 17 and 19 off: no DKG, so every commitment is null.
  - `chain.txt` is blocks 1-160 (tip `4932c7ce…dfae5194`), with 4 legacy masternodes from setup (to 135).
  - **Core's rule** (v24.0.0):
    - The version check is a maximum, so v1 stays valid above V19 (`evo/specialtxman.cpp:1172-1176`).
    - A v1 payload's operator signature is read in the legacy encoding (`evo/providertx.h:307-310`,
      `:425-428`).
    - It is verified under the block's scheme, not the payload version (`CheckHashSig`,
      `evo/specialtxman.cpp:1148-1155`, `:1482`, `:1617`; `bls/bls.cpp:323-326`).
  - **The ProTxs:**
    - At 148, dashd's RPC wrote a ProUpServTx for masternode A (`f459510f…`, a new port) and a
      ProUpRevTx for masternode C (`aa4f8d49…`, reason 2). Both are v1 and signed under legacy
      (`evo/providertx_service.cpp:351`), and were not sent.
    - The recorder signed each payload again under the basic scheme, in the legacy encoding.
    - Those are mined at 150 (`3cd32329…`) and 151 (`e9d7290d…`).
  - `summary.json` keeps four things:
    - the four variants' hex;
    - node0's `testmempoolaccept` at 148 (legacy allowed, basic `bad-protx-sig`) and at 149 (the
      other way round);
    - its `generateblock` checks, `bad-protx-sig` for each legacy one at 149 and 150;
    - each masternode's state version, `pubKeyOperator`, `PoSeBanHeight`, `revocationReason` and
      `addresses` at 149, 150, 151 and 160.
  - The states: A stays at version 1 with its legacy key. C is revoked at 151 (version 1, null key,
    reason 2).
  - **The refused blocks:**
    - `serv-refused.txt` is 150 with the legacy-signed ProUpServTx in place of the mined one, re-solved
      (`626a3b5c…`).
    - `rev-refused.txt` is 151 with the legacy-signed ProUpRevTx, re-solved (`022654d5…`).
    - Core never mined either.

The R3 probe ran a fresh dashd v24.0.0 with the chain's args. Each verdict is the one
`verify_chain.py --probe` expects:
- It takes the chain to 160 with no refusal.
- `testmempoolaccept` at 148: legacy-signed allowed, basic-signed `bad-protx-sig`.
- At 149: legacy-signed `bad-protx-sig`, basic-signed allowed. At 150, C's: the same.
- `serv-refused.txt` at tip 149 and `rev-refused.txt` at tip 150: `bad-protx-sig`.
- `protx info` at 149, 150, 151 and 160 equals the recorder's.

| File | sha256 |
|---|---|
| `r3/chain.txt` | `98908d2375a9f11ee20937e6b27142465c2b04492c7b368fbfd6ad587bf5f01f` |
| `r3/serv-refused.txt` | `57e7d22fb71fe8905239fd3981281218fe4c1e3fb200cefde7379ca2f2c14436` |
| `r3/rev-refused.txt` | `3b4c6a7e28cbc5a0c13b4e288860a0d6dc08c7956254cd089f9f58ea0f4f42e3` |
| `r3/summary.json` | `6391e097e99f596659bb25e7a09e6df49947c1b35378400ff2b73d25e309e52f` |

R3's tests: `r3_v1_protxs_above_v19_connect_with_cores_state` in `src/chain/tests/v19_window.rs`, and
`r3_v1_protxs_above_v19_agree_with_dashd` in `dashd-node/tests/v19_window_dashd.rs`.

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

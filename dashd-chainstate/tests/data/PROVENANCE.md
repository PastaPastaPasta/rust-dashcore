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

# Reference dashd oracles (agentbox)

Two long-running, fully indexed Dash Core nodes, one for mainnet and one for testnet. They are
the reference ("oracle") for differential tests: the conformance harness (BS2), later chain
replay, UTXO/MN-list comparisons, and RPC-shape checks.

**Do not use them for timing or benchmarks.** They run niced, with large caches and every
index on, while other agents query them. Benchmarks use B1's separate instances under
`/work/bench/`. Keep away from those datadirs and ports.

## Summary

| | mainnet | testnet |
|---|---|---|
| systemd user service | `dash-oracle-mainnet` | `dash-oracle-testnet` |
| datadir | `/work/chains/mainnet/data` | `/work/chains/testnet/data` (chain files in `testnet3/`) |
| config | `/work/chains/mainnet/dash.conf` | `/work/chains/testnet/dash.conf` (`[test]` section) |
| RPC | `http://127.0.0.1:27998` | `http://127.0.0.1:37998` |
| RPC cookie | `/work/chains/mainnet/rpc.cookie` | `/work/chains/testnet/rpc.cookie` |
| P2P (inbound from localhost only) | `127.0.0.1:27999` | `127.0.0.1:37999` |
| ZMQ `hashblock` | `tcp://127.0.0.1:27997` | `tcp://127.0.0.1:37997` |
| blk files | `/work/chains/mainnet/data/blocks/` | `/work/chains/testnet/data/testnet3/blocks/` |
| debug log | `/work/chains/mainnet/data/debug.log` | `/work/chains/testnet/data/testnet3/debug.log` |

- **Binary:** Dash Core **v24.0.0-rc.3** (`/work/chains/bin/dashcore-24.0.0-rc.3`, reached through
  the symlink `/work/chains/bin/current`). At setup time (2026-10-08) the latest *stable*
  release was v23.1.8; v24 was at rc.3, published 2026-10-07. The RC was chosen because it
  validates the v23 chain identically, it is what the program targets (protocol 70242), and
  a v23 node would stall if v24 activates on testnet. The tarball's SHA-256
  (`9327f000…69ff52`) matches the release's `SHA256SUMS.asc`; the GPG signature was not
  checked. To move to the final v24 release, extract it next to the RC, repoint `current`,
  and restart both services. The datadirs upgrade in place.
- **Indexes:** `txindex`, `addressindex`, `spentindex`, `timestampindex`,
  `blockfilterindex=basic`, `coinstatsindex`. No pruning, no wallet (`disablewallet=1`).
- **Caches:** `dbcache=12000` (mainnet) and `4000` (testnet); `maxmempool=300`.
- **RPC:** bound to 127.0.0.1 only, cookie auth, `rpcthreads=32`, `rpcworkqueue=1024`.
- **blk files are not obfuscated:** v24 has no `-blocksxor`, so there is no `xor.dat`.
- **P2P:** normal outbound sync from the internet. Inbound is bound to 127.0.0.1 with
  `whitelist=noban,download@127.0.0.1`, so a Rust node on this box can peer with the oracle.

## Querying

Use the wrapper in rust-dashcore. It reads the right config and cookie:

```bash
contrib/oracle-cli testnet getblockchaininfo
contrib/oracle-cli mainnet getblock "$(contrib/oracle-cli mainnet getblockhash 1000000)" 2
```

It is a thin exec of
`/work/chains/bin/current/bin/dash-cli -conf=/work/chains/<net>/dash.conf -datadir=/work/chains/<net>/data …`.

From code, use HTTP basic auth with the contents of the cookie file (`__cookie__:<random>`).
Re-read the cookie on every connection, because it changes on each restart. Example with curl:

```bash
curl -s --user "$(cat /work/chains/testnet/rpc.cookie)" \
  -d '{"method":"getblockcount","params":[]}' http://127.0.0.1:37998
```

dashd accepts JSON-RPC batches (a JSON array of requests), which the conformance harness uses
to stream blocks.

## Operating

```bash
systemctl --user status dash-oracle-mainnet dash-oracle-testnet
systemctl --user restart dash-oracle-testnet     # graceful; allows 15 min to flush the cache
systemctl --user stop dash-oracle-mainnet
journalctl --user -u dash-oracle-mainnet         # systemd view; dashd logs go to debug.log
```

- The unit files are `~/.config/systemd/user/dash-oracle-{mainnet,testnet}.service`.
- They are `Restart=on-failure` and `WantedBy=default.target`.
- Lingering is enabled for `agent`, so they start at boot without a login.
- ExecStart:
  `/work/chains/bin/current/bin/dashd -conf=/work/chains/<net>/dash.conf -datadir=/work/chains/<net>/data`,
  with `Nice=5` and `TimeoutStopSec=900`.
- **Never** `kill -9` an oracle: with a 12 GB dbcache, that means a long replay on restart.
  Use `systemctl --user stop` or `oracle-cli <net> stop`.

## Conformance refresh timer

- `dash-conformance-refresh.timer` (systemd user) runs `/work/conformance/bin/oracle-pass.sh`
  every 2 hours.
  - That script is a copy of `conformance/scripts/oracle-pass.sh`, run at idle CPU and I/O
    priority.
  - Each pass resumes the conformance harness (`/work/conformance/bin/dash-conformance`) over
    both oracles, in RPC and blk mode. Results go to `/work/conformance/<net>-{rpc,blk}/`, and
    each pass appends a line to `/work/conformance/passes.log`.
- Stop it with `systemctl --user disable --now dash-conformance-refresh.timer`.
- To use a newer harness build, copy it to `/work/conformance/bin/`. Start new output dirs if
  the report format changed.

## Sync status

Snapshot at 2026-10-08 06:19 UTC:

| Network | Blocks | Headers | Synced through |
|---|---|---|---|
| mainnet | 650,974 | 2,551,717 | 2017-04-10 |
| testnet | 647,722 | 1,568,398 | 2022-01-10 |

At that point:
- The rates were about 1.3K blocks/min (mainnet) and 1.7K/min (testnet).
- The `/work` disk was about 96% utilized by other workloads.
- The rough ETA was 24–36 h for mainnet and 9–14 h for testnet.

For the current state, run `contrib/oracle-cli <net> getblockchaininfo`.

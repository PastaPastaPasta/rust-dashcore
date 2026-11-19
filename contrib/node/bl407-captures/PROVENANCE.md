# BL-407 shutdown capture

Ground truth for Dash Core v24.0.0-rc.4's two shutdown flushes after an `AbortNode` that leaves
LevelDB healthy. `shutdown.txt` is dashd's `debug.log` from the `***` line to `Shutdown: done`,
with each line's timestamp stripped.

dashd-node's `node::tests::a_failed_block_read_abort_matches_dashd_v24` raises the same
`AbortNode` through the connector's abort path and compares rdashd's shutdown failure lines
with this capture's: the `***` line, the `Error:` line, and no failure line from either flush.

## How it was made

`capture.sh` runs a regtest dashd on its own datadir (mode 0700) and RPC port, and does not
listen:

1. `generatetoaddress 2 yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6` (rpc_generate.py's regtest address);
2. `getblock <h2> 0`, then `invalidateblock <h2>`: the block's data stays in `blk00000.dat`;
3. the low byte of that block's stored header time is flipped (the header is found once in the
   file);
4. `reconsiderblock <h2>`: `ConnectTip` reads the block from disk, `ReadBlockFromDisk` fails,
   and `AbortNode(state, "Failed to read block")` starts the shutdown
   (`validation.cpp:3415-3418`). The read logs one of two lines first, depending on whether
   the changed header still meets regtest's proof-of-work limit: `ReadBlockFromDisk: Errors in
   block header` if it does not, `GetHash() doesn't match index` if it does. The excerpt starts
   at the `***` line either way.

dashd then stops by itself, with exit status 1.

```
mkdir -m 0700 -p <scratch>
contrib/node/bl407-captures/capture.sh <scratch>/run1 contrib/node/bl407-captures/shutdown.txt
```

`<scratch>` is any scratch directory outside the repository. `capture.sh` creates its first
argument, which must not exist yet.

- `BL407_DASHD` names the dashd. The default is setup-dashd.py's v24.0.0-rc.4. The script refuses
  any other `-version`.
- `BL407_RPC_PORT` (default 31407) is the RPC port.

## The run

- **Date:** 2026-10-31, 14:36Z.
- **dashd:** `Dash Core version v24.0.0-rc.4`, the release build from setup-dashd.py's cache
  (`~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd`), sha256
  `e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7`.
- **Host:** x86_64 Linux, glibc 2.43.
- **Exit status:** 1. stderr was the one line
  `Error: A fatal internal error occurred, see debug.log for details`.

A second run gave the same `***` and `Error:` lines and the same two evodb timer pairs. The
thread-exit lines came in a different order, and the mempool dump's timings differed. Nothing
in `shutdown.txt` was edited by hand.

## What it shows

- **The first report:** `*** Failed to read block`, then Core's default user message
  `Error: A fatal internal error occurred, see debug.log for details`.
- **Each shutdown flush logs only the evodb timer pair:**
  - `FlushStateToDisk: write evodb cache to disk started`;
  - `FlushStateToDisk: write evodb cache to disk completed (0.00s)`.

  This comes from `validation.cpp:3101-3106`.
- **Neither flush logs a failure:** no second `***` line, no `System error while flushing`, no
  `ForceFlushStateToDisk: failed to flush state`, and no `-debug=leveldb` hint.

rdashd writes nothing after the connector's `AbortNode` (CH3 D52, DEC-309), so its flushes do
not log the two timer pairs. That difference is ruled by DEC-995 until BL-325.

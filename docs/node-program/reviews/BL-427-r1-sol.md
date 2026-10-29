# BL-427 review — ND1 BL-427 r1

Reviewed branch `node/nd1-netrpc` at `6bd9eded63411f88d84984b62cb01aeb0fc24adb`, against `497d3ca11`. The review was read-only; no branch files, live oracles, or implementer's target directory were modified. Reference behavior was Dash Core rc.4 at `375c64d2317`.

## Verdict

**ACCEPT**

Findings: **0 blocker, 0 major, 0 minor, 0 nit**.

## What I checked and found correct

- `disconnectnode` in `dashd-rpc/src/peers.rs:458-482` follows Core `src/rpc/net.cpp:442-487`: the connection-manager check is made before the handler's operation; address-only, empty-address-plus-nodeid, and named forms are dispatched correctly; both/neither are `-32602`; an unmatched peer is `-29`; and `get_int_i64` preserves Core's `-1` `JSON integer out of range` for fractional or out-of-range numeric IDs. The RPC spec's generic arity/type checks also cover the same positional and named argument errors as Core.
- `NetCore::disconnect_by_name` and `disconnect_by_id` in `dash-p2p/src/netcore.rs:734-758` match Core `src/net.cpp:4578-4613` and `src/net.h:1692-1731`: matching uses `m_addr_name` for address strings, scans table order, excludes peers already marked for disconnect, and marks the first remaining match while holding the connection-table read lock. The atomic mark and notification are nonblocking, and the lock is not held across any dial or processor call.
- `getaddednodeinfo` in `dashd-rpc/src/peers.rs:484-518` matches Core `src/rpc/net.cpp:489-559`: the manager-disabled path is `-31`, optional node lookup is by the complete added-node string with first-entry selection, unknown nodes are `-24`, and output ordering is `addednode`, `connected`, `addresses`, followed by `address`, `connected`. `addresses` is emitted even when empty.
- `NetCore::added_node_info` in `dash-p2p/src/dial.rs:406-450` matches Core `src/net.cpp:3308-3366`: numeric entries match the peer service; names match only a nonempty peer connection name; marked peers remain eligible; the last matching peer supplies inbound/outbound and the resolved service; `include_connected` is honored; and the added-connection loop reuses the same `false` matcher.
- `tick_addcon` in `dash-p2p/src/dial.rs:855-881` drops the dial mutex before invoking the helper that snapshots added nodes and peers, avoiding recursive locking. The snapshot/lock sequence matches Core's separate added-node and node-table critical sections. NUL truncation for actual connection attempts remains in `OpenRequest::cut_dest_at_nul`/`NetCore::open`, while the stored added-node and RPC matching strings retain the complete value, matching Core's `c_str()` versus stored-string behavior.
- `NetSlot::peer_info` in `dashd-node/src/rpc.rs:298-319` filters `disconnect_requested()` peers before constructing `getpeerinfo`, matching Core `src/net.cpp:4563-4576` and the `fDisconnect` handling used by `getpeerinfo`. `getnetworkinfo` retains the corresponding count filtering.
- The real-V regression test in `dashd-node/src/p2p/realv.rs:456-498` checks the peer both before marking and after marking, and verifies that the marked peer is absent from `peer_info` while still present in the connection table. This directly covers BL-394 n-3.

## Tests and evidence reviewed

Using the isolated review target under `/work/scratch/nd1-netrpc-review/` (and the implementer's recorded isolated logs), the focused suites passed:

- `dash-p2p` name/disconnect and added-node-info tests: 37 passed.
- `dashd-rpc` peer RPC tests: 18 passed (the full library run also passed).
- `dashd-node` real-V RPC-slot test: passed.
- `cargo test -p rdashd --test rpc_diff -- disconnectnode addnode`: 2 passed, including `disconnectnode_and_getaddednodeinfo_match_dashd_v24` against the rc.4 dashd oracle.
- The recorded format, clippy, and gate-fast checks passed for the slice.

The mutant evidence is consistent with the stated test strategy. m3–m7 (address-string matching, first/last winner behavior, marked-peer inclusion, name resolved address, and honoring `include_connected`) are killed by the `dash-p2p` unit tests. They survive the live diff because that scenario has no DNS-named outbound peer; the unit cases explicitly exercise those states. The RPC mutants and the n-3 node mutant are killed by their corresponding focused/live tests.

No correctness issue requiring a fix was found.

# P7 primitive vectors

`dashd-masternode/tests/mnconn.rs` ports the outbound symmetry, membership, null and equal-input
checks from v24.0.0's `src/test/llmq_utils_tests.cpp:394-479`. The fixed expected winners come
from the C++ selector itself, not from the Rust implementation:

```sh
export TMPDIR=/work/scratch/sw-v24-mn/p1/tmp
mkdir -p "$TMPDIR"
python3 contrib/node/p7-captures/outbound.py /work/scratch/sw-v24-mn/core-v24
```

The recorder extracts `DeterministicOutboundConnection` unchanged from `src/llmq/utils.cpp`,
compiles it against Core's `hash.h` and `crypto/sha256.cpp` with C++20 and
`DISABLE_OPTIMIZED_SHA256`, and prints input indexes and the winning index. It neither modifies
nor builds into the Core tree. Indexes 0 through 5 denote 32-byte arrays whose only nonzero
bytes are respectively: none, `[0]=1`, `[0]=2`, `[0]=3`, `[0]=42`, `[31]=1`.

| Input pair | Winner |
| --- | --- |
| 0, 1 | 0 |
| 0, 2 | 0 |
| 0, 3 | 3 |
| 0, 4 | 0 |
| 0, 5 | 5 |
| 1, 2 | 1 |
| 1, 3 | 3 |
| 1, 4 | 1 |
| 1, 5 | 5 |
| 2, 3 | 3 |
| 2, 4 | 2 |
| 2, 5 | 5 |
| 3, 4 | 4 |
| 3, 5 | 5 |
| 4, 5 | 5 |

Equal inputs select themselves. Tests also check every pair in reverse order. Core's
`uint256.h:55-59` compares stored bytes with `memcmp` for both sorting and comparing the
derived hashes. This differs from the P7 plan's reversed-byte integer comparison.

## Live mesh capture still needed

No `getpeerinfo` mesh vector was recorded. The existing `dash-spv` fixture starts with spork
21 disabled (`masternode_network.rs:214-244`), and its `mine_dkg_cycle_with_hook` assumes
two outbound connections per member under that setting (`:728-781`). A deterministic mesh
has different counts on each member. `contrib/setup-dashd.py` also explicitly documents that
the masternode DKG tests do not yet support v24. Adapting and validating that harness is beyond
the primitive-only change; the C++ vectors above do not claim to be live peer observations.

The deferred `mesh.py` capture needs these steps:

1. Use v24.0.0 and the `regtest-mn` data loaded by `MasternodeTestContext`. Export
   `DASHVERSION=24.0.0`, then run `eval $(python3 contrib/setup-dashd.py)` to load the binary
   and fixture. Copy the controller and every MN datadir into
   private directories with mode `0700`; use distinct P2P and RPC ports. Keep fixture private
   keys out of the capture output.
2. Preserve the fixture's MN activation, controller connections, service-address updates,
   chain synchronization and mock-time setup. Set `SPORK_21_QUORUM_ALL_CONNECTED` to `0`
   through `sporkupdate`, then verify `spork show` reports `0` on every MN.
3. Run a complete successful mesh DKG. Replace the harness's fixed two-outbound wait with
   quorum-member authentication and mesh completeness checks. Record the quorum hash, type,
   members and successful mined commitment via `quorum info`. Cover every pair from
   `network.json`; use additional member sets if one quorum does not contain all MNs.
4. After authentication settles, record `getpeerinfo` on every MN, keeping
   `verified_proregtx_hash`, `inbound`, and `masternode` (to identify outbound MN connection
   attempts). Retain both endpoints' raw observations. For each covered pair,
   require exactly one authenticated MN outbound leg and the matching inbound leg at the
   other endpoint. Retry transient duplicate/disconnecting states rather than deriving a
   winner from this implementation.
5. Save the binary version and SHA256, fixture version, block/quorum hashes, spork values and
   sanitized peer observations alongside `(a, b, outbound_protxhash)` for every pair. Reverse
   the RPC hex hashes to stored byte order only when loading them into the Rust test.

The spork evaluators use the existing `SporkManager::value` API, as Core does. The existing
`dash_chain::SporkView` exposes only ChainLocks, so it cannot supply these raw values.
`mnconn::sporks::eval_spork` re-exports the existing `dash_evo::llmq::eval_spork`.

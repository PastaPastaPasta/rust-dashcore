# ND1 BL-391 `getnetworkinfo` / `setnetworkactive`: Opus review, round 1

**Head reviewed:** cad3b4272 on `node/nd1-getnetworkinfo` (e5a150958 + cad3b4272, both signed `G`), base ac41eae46.
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…` in ~/workspace/dash).
**Scope:** `git diff ac41eae46 cad3b4272` (14 files, +764/−41), read only. The worktree
`~/workspace/rdc-wt/nd1-submitblock` has uncommitted edits to six files (connman.rs, node.rs, p2p/mod.rs, rpc.rs,
peers.rs, peers/tests.rs). They look like the next slice, so I reviewed and built only `git archive cad3b4272`.

**Verdict: CHANGES REQUIRED.** Narrow scope: one major (finding 1), a test gap. The behaviour behind it is correct:
my probe shows rdashd matching dashd in that state. The rest are three minors and four nits. The wire shape, the key
order, the D-rows and the `-networkactive` wiring all match rc.3.

## Method

I read:

- **Core rc.3:** `rpc/net.cpp:613-739` (`GetNetworksInfo`, `getnetworkinfo`) and `:917-935` (`setnetworkactive`).
- **Core `net.cpp`:**
  - `:1914` (`AcceptConnection` gate);
  - `:2075-2110` (`DisconnectNodes`);
  - `:2748-2756` (DNS-seed wait);
  - `:3599` (`OpenNetworkConnection`);
  - `:3890-3930` (`SetNetworkActive`, the `CConnman` constructor);
  - `:4520-4540` (`GetNodeCount`).
- **Core, other files:**
  - `init.cpp:697, 1688-1717`;
  - `compat/compat.h:99-112`, `util/sock.h:18-26`;
  - `clientversion.h:33-36`;
  - `logging.h:92` (`NET_NETCONN`);
  - `test/functional/rpc_net.py:192-220`, `feature_config_args.py:193-217`.
- **rdashd:**
  - the full diff;
  - `NetSlot` and node attach/detach order (`dashd-node/src/rpc.rs`, `node.rs:200-400`);
  - `Net::new` (`p2p/mod.rs:385-428`);
  - in dash-p2p: `NetCore::open` (`netcore.rs:911`), `disconnect_nodes` (`:1290`), `sweep` (`:1496`), `is_reachable` (`:852`), the dial DNS phases (`dial.rs:900-985`) and the inbound gate (`inbound.rs:359`);
  - `NetAddr` `Ord`/`Display` (`dash-netaddr`);
  - `Command::handle` arity and type checks (`dashd-rpc/src/server.rs:60-110`) and the `setnetworkactive` spec;
  - `support.rs` (`-listenonion` is inert-off; `-externalip`, `-onlynet`, `-proxy`, `-onion` and `-socketevents` are refused);
  - the logger's target rules (`dashd-node/src/logging/mod.rs:8-21`).
- **Implementer's material:**
  - plan.md (D1–D10);
  - the probe scripts and their captures (`probe/out/*.json`, `raw/*.raw`);
  - the bench_inputs verdict (`pf-inputs-ac41eae46-vs-cad3b4272.txt`: SAME on all 13 crates, EXIT 0).

**Runs.** Target reflinked from `/work/scratch/nd1-gni/target` into `/work/scratch/nd1-gni-review-r1`, with sources
touched and the incremental cache dropped, because the implementer was building concurrently. All of these pass at
cad3b4272:

- `dashd-rpc --lib peers`: 11/11.
- `dash-p2p --lib local_addresses`: 1/1.
- `dashd-node --lib rpc:: init::support`: 7/7.
- `rdashd --test rpc_diff getnetworkinfo_matches_dashd_v24 getpeerinfo_matches_dashd_v24_on_a_shared_peer`: 2/2, 63.6 s, with real dashd rc.3.

**Reviewer probe** (`probe_inbound.py`, now deleted with the scratch dir):

- **Setup:** dashd rc.3 and rdashd each run with `-bind=127.0.0.1 -listenonion=0 -discover=0 -blocksonly=0`. A third dashd `-connect`s to both.
- **One inbound peer each:** every non-identity key is equal, in the same order. Both report `localrelay:true` and `connections:1, connections_in:1, connections_out:0`.
- **After `setnetworkactive false`:** `connections` drops to 0 in 0.77 s on dashd and 0.20 s on rdashd.
- **Init line placement:** the init `SetNetworkActive: true` falls in the same place in both debug.logs: after "Recreating the banlist database", before the UTXO-cache line.

## Rc.3 conformance (check 1)

| Key | rdashd source | Verdict |
|---|---|---|
| order | `peers.rs:296-327` | Same as Core's push order (`networkactive` comes before `connections`, as in the body, not the help) |
| `version` | `lib.rs` `CLIENT_VERSION` | Core's formula (`10000·MAJOR + 100·MINOR + BUILD`); 0.45.0 gives 4500 (D1) |
| `buildversion`, `subversion` | `FULL_VERSION`, `identity.user_agent` | D1; `-uacomment` flows through the identity |
| `protocolversion` | `identity.protocol_version` | Diffed live |
| `localservices`, `localservicesnames` | `core.config().local_services`, `%016x`, shared `service_names` | Diffed live; Core's `nLocalServices` doesn't change at runtime |
| `localrelay` | `!shared.ignores_incoming_txs()` | Correct (probe), but no test sees `true` (finding 1) |
| `timeoffset` | `NetCore::time_offset` (median, 0 below five samples) | Correct; always 0 in tests (nit, finding 8) |
| `networkactive` | `core.network_active()` | Diffed live in all three states |
| `connections*` | `ConnectionCounts` over `!disconnect_requested()` | Matches `GetNodeCount`, including `Verified == VerifiedIn + VerifiedOut`. The inbound branch is untested (finding 1) |
| `socketevents` | cfg constant | Matches compat.h: Linux/Android → epoll; FreeBSD/Apple → kqueue; others → select |
| `networks` | `[Ipv4, Ipv6, Onion, I2p, Cjdns]`, `is_reachable`, proxy `None` | Order is Core's `NET_MAX` loop minus UNROUTABLE/INTERNAL; `limited = !reachable` |
| `relayfee`, `incrementalfee` | `DEFAULT_*` consts through `amount` | `ValueFromAmount` exact, including `i64::MIN` |
| `localaddresses` | `NetCore::local_addresses` (BTreeMap under Core's `CNetAddr` order) | Correct, but the NetSlot mapping is untested (finding 1) |
| `warnings` | `NodeWarnings::get_warnings(false)` = `.original` | D2 |

`setnetworkactive` gets Core's arity and type errors from the generated spec. Without connman it answers -31, as
`EnsureConnman` does. It returns `GetNetworkActive()`.

## Findings

### 1. major: four `getnetworkinfo` derivations have no test that fails on regression

**Evidence.** I applied these four mutants together to `dashd-node/src/rpc.rs` at cad3b4272:

- `:323-324`: dropped the `!disconnect_requested()` filter and replaced `connections.add(peer.is_inbound())` with `connections.add(false)`;
- `:336`: `local_relay: false`;
- `:351`: `.take(0)` on `local_addresses`.

With all four applied, `dashd-node rpc::` (2/2) and both live rpc_diff tests (2/2, 69 s) still pass. The reasons:

- rdashd never has an inbound peer in any test. `getpeerinfo_matches_dashd_v24_on_a_shared_peer` and the `networkactive0` config both use `-connect … -listen=0`, and nothing connects to the `listening` config.
- dashd always runs `-blocksonly=1` (DEC-322), so `localrelay` is false on both sides and the constant-false mutant matches.
- `-discover=0` (`rpc_diff.rs:839`) keeps `localaddresses` at `[]` in every live config.
- The unit tests in `peers/tests.rs` build `NetworkInfo` by hand, so they never call `ConnectionCounts::add` or `NetSlot::network_info`.

rc.3's `rpc_net.py:192-220` asserts `connections_in` 2 and then 1, which is exactly the uncovered branch. The plan's
"Not taken" note says the disconnect skip is shared with getpeerinfo's walk, but that walk is a separate filter, and
removing this one survives.

**Fix.** Add a fourth rpc_diff config, a "served" config:

- dashd and rdashd both run with `-bind=127.0.0.1 -listenonion=0 -discover=0 -blocksonly=0`;
- one dashd client runs `-connect=<core p2p> -connect=<ours p2p> -listen=0`;
- wait until each node reports `connections_in == 1`, then call `network_info_differences`.

My probe shows this passes today in under 15 s, and it kills the inbound and localrelay mutants. Two further
additions:

- **`localaddresses`:** `-externalip` is refused, so the mapping can't be diffed live. A dashd-node unit test on a real `NetCore` with one `add_local` (the NetSlot plumbing alone) would cover it. Failing that, a `-discover` config that compares addresses and scores and checks each port against that node's own `-port` (skip if the host has no routable interface).
- **Disconnect filter:** sample `connections` straight after `setnetworkactive false` and before the next sweep. Or mark the mutant as accepted in the plan, since its only visible effect is during the ≤100 ms linger.

### 2. minor: "Network not active, dropping peer" is logged under `net`, not Core's `NET_NETCONN`

**Evidence.** `dash-p2p/src/netcore.rs:1295` uses `tracing::debug!(target: "net", …)`. Core's `net.cpp:2103` uses
`LogPrint(BCLog::NET_NETCONN, …)` (`logging.h:92`: `NET | NETCONN`). dash-p2p already has the matching target, `NETCONN =
"net|netconn"` (`inbound.rs:18`), and uses it for the sibling `not accepting new connections` line (`inbound.rs:360`).
Under `-debug=netconn` alone, Core prints the line and rdashd doesn't. Under `-debug=net` the prefixes differ:
`[net]` against Core's `[net|netconn]`.

The line predates this branch, but before BL-391 it couldn't be reached: `-networkactive` was refused and
`setnetworkactive` wasn't served. This branch is the first to put it in a log.

**Fix.** Use the `NETCONN` target (`inbound.rs:18`; make the const `pub(crate)` or move it somewhere shared). Then assert the line in
`getnetworkinfo_matches_dashd_v24`'s `networkactive0` step, which already runs `setnetworkactive false` with a peer.
Start rdashd with `-debug=netconn` and expect `[net|netconn] Network not active, dropping peer=0`.

### 3. minor: the DNS-seed wait line still carries `[net:info]`, the same defect D8 fixed

**Evidence.** `dash-p2p/src/dial.rs:929` logs `tracing::info!(target: "net", "Waiting for network to be reactivated before
querying DNS seeds.")`. Core's `net.cpp:2750` uses `LogPrintf`, which has no prefix. The logger rule
(`logging/mod.rs:8-17`) makes this print as `[net:info] Waiting …`, the same mismatch D8 fixed for `SetNetworkActive`.
It's reachable only with `-networkactive=0` on a chain with DNS seeds (not regtest), so this branch is also the first
to expose it. The `Loading addresses from DNS seed` line in the same function (`:934`, `:969`) has the same defect.

The pattern is wider and predates the branch. dash-p2p, dashd-p2p and dashd-net have 41 `info!(target: "net")` sites,
and several are LogPrintf lines in Core. The probe's debug.log shows `[net:info] Creating peers.dat…`, `[net:info]
Recreating the banlist database`, and `[net:info] 0 block-relay-only anchors…`, which Core prints with no prefix.
`assert_debug_log` matches substrings, so no functional test catches it.

**Fix.** In this branch, drop the target on the `-networkactive` path's line (`dial.rs:929`). File a backlog row
(P2a-net) to audit the 41 sites against Core's LogPrintf/LogPrintLevel calls.

### 4. minor: the fixtures' provenance isn't reproducible from the repo

**Evidence.** Several doc comments in `dashd-rpc/src/peers/tests.rs` say "recorded live from `dashd` v24.0.0-rc.3":
`:306` for `DEFAULT`, `:355` for the proxy and onlynet fixtures, plus `:390` and `:438`. I checked the strings against
`/work/scratch/nd1-gni/raw/*.raw`, and they're byte-identical:

- `DEFAULT` = `default.raw`, `listen0.raw` and `connect0.raw`;
- the proxy fixtures = `proxy.raw` and `proxy_norand.raw`;
- the onlynet fixtures = `onlynet_onion.raw` and `onlynet_ipv4.raw`.

They're also credible against `init.cpp`: `-proxy` sets ipv4, ipv6, onion and cjdns but not i2p; cjdns stays
unreachable without `-cjdnsreachable`; onion is reachable only with a proxy. But the comments don't give the argv the
captures were made with (`-dnsseed=0 -fixedseeds=0 -disablewallet -bind=127.0.0.1 -listenonion=0`, fresh datadir).
The capture script, `probe-raw.py`, lives only in /work/scratch, which gets swept. Anyone refreshing the fixtures after
a Core pin bump would have to reverse-engineer the setup.

**Fix.** Either:

- give each fixture comment its exact argv and note that the capture was the raw HTTP body (`probe-raw.py` semantics); or
- commit the capture script, as other fixture generators are committed (`contrib/node/…`), and cite it.

### 5. nit: half of the D6 unit assert is tautological

**Evidence.** `dashd-node/src/rpc.rs:610-613` asserts `MemPoolOptions::default()` == `(DEFAULT_MIN_RELAY_TX_FEE,
DEFAULT_INCREMENTAL_RELAY_FEE)`, but `init/mempool.rs:55-56` builds the default from those same consts. Only `:614`
(`DEFAULT_MIN_RELAY_TX_FEE == 1000`) and the refusal loop can fail. `DEFAULT_INCREMENTAL_RELAY_FEE` isn't pinned in
the test. The live rpc_diff does pin both against dashd, so coverage is real; the unit test just overstates it.

**Fix.** Assert both consts equal 1000 (Core's `DEFAULT_MIN_RELAY_TX_FEE` and `DEFAULT_INCREMENTAL_RELAY_FEE`, cited
from policy.h/validation.h). Drop the `MemPoolOptions` comparison, or keep it for MP1a and say that it is the plumbing
check.

### 6. nit: the rpc_diff identity check re-derives `CLIENT_VERSION` with the implementation's formula

**Evidence.** `rdashd/tests/rpc_diff.rs:775-777` repeats `lib.rs`'s `10_000·major + 100·minor + patch`. If the formula
in `lib.rs` were wrong in the same way, both would agree. The formula is right; I checked it against
`clientversion.h:33-36`.

**Fix.** Optional: add a `dashd-node` unit assert with a literal (`CLIENT_VERSION == 4500` for 0.45.0, updated on
bump), or cite clientversion.h at the test's copy.

### 7. nit: the `-nonetworkactive` forms aren't exercised behind the ledger rows that cite them

**Evidence.** The ledger rows `feature_config_args.py:196/208` (`asserted_log_lines.txt:104-105`) stand for that
test's `test_networkactive`, which also starts with `-networkactive`, `-networkactive=1`, `-nonetworkactive` and
`-nonetworkactive=1` (`:200-217`). The cited proof runs only the default and `-networkactive=0`. Negation is
dash-args' generic Core port, so the risk is low.

**Fix.** Add `-nonetworkactive` to one config, or cover the four forms in a `Net::new` or args unit test.

### 8. nit: doc and coverage details

- **Proxy doc:** `dashd-rpc/src/peers.rs:91` documents the proxy as `ToStringAddrPort`. Core renders `Proxy::ToString()`, which is the unix-socket path when `m_is_unix_socket`. P2a-net should render that case.
- **`timeoffset`:** always 0 in tests (fewer than five samples), so `time_offset: core.time_offset()` versus a constant 0 isn't distinguished. That's acceptable; `NetCore::time_offset` has its own tests.
- **Arity error:** `peers/tests.rs:448` only checks that `setnetworkactive []` is an error. Asserting the help-text error (code -1, `RpcError::Runtime`) would pin Core's arity reply.

## Checks with no finding

- **D1 and D2 (identity, warnings):** compared by type and pinned to rdashd's own values. Unit fixtures reproduce Core's bytes. The `version_part` const panic fires at compile time.
- **D3–D6 (refused options):** `-proxy`, `-onion`, `-onlynet`, `-socketevents`, `-minrelaytxfee` and `-incrementalrelayfee` are pinned as refused (`rpc.rs:615-625`). `-listenonion` is inert-off, so no torcontrol onion proxy can appear: "no proxy" really is the only reachable state.
- **D7 (`-networkactive` wiring):**
  - `Net::new` applies it right after the core loads peers.dat and the banlist and before any dial or listen, matching `init.cpp:1715-1717` and `net.cpp:3929`. The log order matches dashd (probe).
  - Only the sweep is spawned at construction, so nothing races the flag at init.
  - At runtime, the inbound gate (`inbound.rs:359`), `open` (`netcore.rs:913`), the sweep's mark-all (`:1292`), the DNS wait and the stale-tip check all read the same atomic, as Core's `fNetworkActive` consumers do.
  - A connect in flight when the flag drops is attached and then marked on the next sweep, as in Core.
  - Core's early return on an unchanged state has no observable effect here, because `mn_sync` and the GUI notify are absent.
- **D8 (log format):** the log check (`rpc_diff.rs:879`) is strict about the prefix and the order: init's line, then each call's.
- **D9 (-31 without connman):** unreachable. The slot attaches at 7c, the warmup ends at 13, and shutdown stops the RPC server before `detach` (`node.rs:396-400`).
- **D10 (connection counts):** `_mn` = 0 until MNAUTH. The verified-hash mutant would be caught by the shared-peer diff.
- **RPC path safety:**
  - No panics: `amount`'s `expect` can't fail, and `slot()` recovers from poison.
  - The slot lock is released before peer iteration.
  - `get_bool` follows the spec's type check.
- **bench_inputs:** SAME everywhere.

## Verdict

**CHANGES REQUIRED** for finding 1: add the served/inbound/`-blocksonly=0` rpc_diff config and a test that reaches the
`localaddresses` mapping.

Findings 2 and 3 are one-line target fixes on the path this branch makes reachable for the first time. I recommend
taking them in the same round, and moving the wider `[net:info]` audit to a backlog row. Findings 4–8 can ride along
or wait.

# ND1 BL-391 `getnetworkinfo` r2 fix check + BL-394 `addnode` r1: Opus review

**Branch:** `node/nd1-getnetworkinfo`, head **74e89c470** (all four commits in cad3b4272..74e89c470 signed `G`).
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…` in ~/workspace/dash).
**Scope:**
- Part A: cad3b4272..2715d0d54 (aee663104 r1 fixes, 2715d0d54 NETCONN move).
- Part B: 2715d0d54..74e89c470 (1e83faae2 feature, 74e89c470 review r1).

**Not the worktree.** While I worked, the worktree `~/workspace/rdc-wt/nd1-submitblock` moved to 2715d0d54 and gained
uncommitted edits (realv.rs, rpc.rs, rpc_diff.rs). So I read and built only `git archive 74e89c470`, in
`/work/scratch/nd1-review-r2` with a reflinked target and `-j8`. The scratch dir is deleted.

| Part | Verdict |
|---|---|
| **A** (BL-391 r2) | **ACCEPT WITH NITS** |
| **B** (BL-394 r1) | **CHANGES REQUIRED**, narrow: finding B-1 only, a test gap. The code is right. |

## Runs (all at 74e89c470)

| Run | Result |
|---|---|
| `cargo test -p dash-p2p --lib` (no features) | 129/129 |
| `cargo test -p dash-p2p --all-targets` (no features) | all pass, EXIT 0 |
| `cargo test -p dash-p2p --all-features --all-targets` | lib 149/149, all targets pass, EXIT 0 |
| `cargo clippy -p dash-p2p --all-targets -- -D warnings`, without and with `--all-features` | clean |
| `cargo clippy -p rdashd -p dashd-rpc -p dashd-node -p dashd-net --all-targets -- -D warnings` | clean |
| `dashd-rpc --lib peers` | 14/14 |
| `dashd-node --lib rpc:: p2p::realv::the_rpc_slot` | 3/3 |
| `rdashd --test rpc_diff`: `getnetworkinfo_matches_dashd_v24`, `addnode_matches_dashd_v24`, `getpeerinfo_matches_dashd_v24_on_a_shared_peer` | 3/3, 68.3 s, real dashd rc.3 |

I also confirmed the 2715d0d54 claim. At aee663104, `netcore.rs:1297` names `crate::inbound::NETCONN`, and `lib.rs:42`
gates `mod inbound` behind `#[cfg(feature = "inbound")]`. So dash-p2p alone did not compile at that commit.

**Mutants.** One build with env-gated mutants (`RVMUT=<id>`), each run on its own against the test named.

| id | Mutant | Test | Result |
|---|---|---|---|
| A1 | `connections.add(false)` (every peer outbound) | getnetworkinfo diff | **killed**, but only by a 120 s timeout in the "each node serves the client" wait (127 s) |
| A2 | `local_relay: false` | getnetworkinfo diff | **killed** (served config diff) |
| A3 | `.take(0)` on `local_addresses` | `realv::the_rpc_slot_reads_the_network_it_is_attached_to` | **killed** (`realv.rs:448`) |
| A4 | `!disconnect_requested()` filter dropped | getnetworkinfo diff, getpeerinfo diff | survives (accepted; see A-1) |
| B1 | NetSlot `open_one_try` ignores `use_v2` and uses `we_advertise_v2()` | addnode diff | **survives** (50.4 s pass) |
| B2 | NetSlot `add_node` stores `we_advertise_v2()` instead of `use_v2` | addnode diff | **survives** (24.2 s pass) |
| B3 | `remove` also disconnects the peer by that name or address | addnode diff | **killed**: "after onetry, add and remove: rdashd has [], expected [manual/v2]" |
| B4 | `remove` skips the v2transport check | dashd-rpc unit + addnode diff | **killed** both (`peers/tests.rs:571`; diff -8 vs 200) |
| B8 | `onetry` returns before the connect (spawned thread) | addnode diff | **killed** at the listing check right after the call (`rpc_diff.rs:1041`) |
| none | env gates inert | addnode diff | pass |

The implementer's own five BL-394 mutants are recorded in `/work/scratch/nd1-gni/mut394/out-{2..5}.txt`. Those runs
were on an earlier tree: they fail at `rpc_diff.rs:1086`, where the head's final assert is at :1096. The four mutants
recorded there (name-only dedup, numeric remove, inactive check, `exists_service`) all fail on the checks that still
exist at head, and B8 above re-kills the onetry-no-op class.

**Live probe** (`nulprobe.py`): dashd rc.3 hub, then dashd and rdashd at `-listen=0`. Used for B-2.

---

## Part A: BL-391 r2

| r1 finding | Fix | Test that fails without it | Verdict |
|---|---|---|---|
| **F1** major | served config (`rpc_diff.rs:859-873`); realv slot test (`realv.rs:426-454`, test fn at :430) | A1, A2 and A3 killed; A4 accepted | **Fixed** |
| **F2** minor | `NETCONN` target (`netcore.rs:1297-1303`) | `rpc_diff.rs` networkactive0 requires `[net|netconn] Network not active, dropping peer=` under `-debug=netconn` | **Fixed** |
| **F3** minor | DNS-seed wait line untargeted (`dial.rs:929-932`) | none (only reachable with DNS seeds, never on regtest) | **Fixed**, untested; acceptable (A-3) |
| **F4** minor | `contrib/node/getnetworkinfo-fixtures.py` committed; fixture comments give its argv (`peers/tests.rs:336-340`, `:388-390`) | n/a. The implementer reports a byte-exact rerun on 10-26; I checked the argv against the comments | **Fixed** |
| F5 nit | both consts pinned to 1000; tautology dropped (`rpc.rs`) | yes | Fixed |
| F6 nit | clientversion.h cited (`rpc_diff.rs:774`) | n/a | Fixed; skipping a literal pin is reasonable |
| F7 nit | skipped as already covered | — | **Agree** (below) |
| F8 nits | proxy doc; arity help asserted as `RpcError::Runtime` (`peers/tests.rs:483-486`) | yes | Fixed |

**F7: I agree with the skip.** `dash-args/tests/functional_behaviours.rs:322-334` (`networkactive_forms`) runs all six
forms through the dashd ArgsManager: none, `-networkactive`, `=1`, `=0`, `-nonetworkactive` and `-nonetworkactive=1`.
`Net::new` (`dashd-node/src/p2p/mod.rs:414`) reads exactly `get_bool_arg("-networkactive", true)`. A further node-level
config would only re-test negation, which is shared code.

**dash-p2p builds and tests with and without features:** confirmed (Runs table).

### A-1. nit: accepting the disconnect-filter survivor is fine, but "≤100 ms" understates the window

**Evidence:**
- **Core:** `GetNodeCount` skips `fDisconnect` peers (`net.cpp:4520-4540`), and Dash's `DisconnectNodes` keeps a marked
  peer in `m_nodes` for at least 100 ms of linger (`net.cpp:2121-2131`). So Core has the same observable window, and the
  filter is what hides it.
- **rdashd, setnetworkactive path:** the mark is made inside the sweep (`netcore.rs:1293-1305`), so the window is the
  100 ms `LINGER` (`netcore.rs:61`).
- **rdashd, any other mark:** `Peer::mark_disconnect` wakes only the reader, not the sweep (`conn.rs:775-779`). The
  sweep sleeps up to `SWEEP_INTERVAL` = 500 ms (`netcore.rs:59`). So a processor disconnect or a `NetCore::disconnect`
  leaves the peer counted, under mutant A4, for up to about 600 ms.
- **Why it matters later:** that window becomes test-visible once `disconnectnode` is served (BL-394 D5). DTF's
  `disconnect_nodes` polls `getpeerinfo` right after it.

**Judgement:** acceptable for this slice, since Core's semantics are matched and the filter has the same shape as
getpeerinfo's. Correct the plan's rationale: "until the sweep's linger ends, up to ~600 ms after an off-sweep mark".

**Fix (optional now, required with `disconnectnode`):** a deterministic `realv_peers` test:
1. A scripted peer is dialled through `-connect` (that rig already does this).
2. Call `net.core().disconnect(id, …)`.
3. Read `NetSlot::network_info()` and `peer_info()` straight away: expect `connections == 0` and no peers.

This is not racy: removal needs at least one sweep pass plus 100 ms.

### A-2. nit: aee663104 leaves a bisect hole for `cargo test -p dash-p2p`

**Evidence:** at aee663104 dash-p2p does not compile without the `inbound` feature (E0433, as above); 2715d0d54 fixes
it. The workspace build is fine at both commits.

**Fix:** if the train keeps commits individually, squash 2715d0d54 into aee663104 when restacking. Otherwise, no action.

### A-3. nit: F3 and the fixture script

- **F3 has no test.** The line is only reachable with DNS seeds. A dash-p2p unit test could assert the event's target
  (`tick_dnsseed` with the network inactive and a capturing subscriber), but given the reachability I would not
  require it. The r1 recommendation stands: the ~41-site `info!(target: "net")` audit goes to a P2a-net backlog row.
- **Script usage path.** `contrib/node/getnetworkinfo-fixtures.py:15` gives `--out /work/scratch/<task>/raw` as the
  usage example. That is an agentbox path in a committed script. Use a neutral example such as `--out ./raw`.

### A-4. nit: mutant A1 dies by a 120 s timeout, not a diff

**Evidence:** the served config waits for `connections_in == "1"` on both nodes (`rpc_diff.rs:871-874`) before
diffing. A miscount on rdashd's side therefore turns into "timed out waiting for each node serves the client" after
`PATIENCE` (120 s). That message reads like a connectivity failure, not a wrong count.

**Fix:** wait on the client's side instead (the served-client dashd reports `connections_out == 2`), or on dashd's
`connections_in` alone. Then diff, so a miscount shows as a `connections_in` difference within seconds.

**Part A verdict: ACCEPT WITH NITS.** Every r1 finding is fixed. The fixes for the major (F1) and for F2 have tests that
fail without them; F3 is untested for a reason I accept. The one survivor is acceptable.

---

## Part B: BL-394 `addnode`

### Conformance with rc.3, check by check

**1. Error order, codes and messages.**

`addnode` (`dashd-rpc/src/peers.rs:392-427`) follows Core's handler (`rpc/net.cpp:338-376`) step by step:

| Step | rdashd | Core |
|---|---|---|
| 1 | `params[1].get_str()` | same |
| 2 | unknown command → help as `RpcError::Runtime` (-1) | same |
| 3 | `v2_transport()` `None` → -31 (Core's `EnsureConnman`) | `EnsureConnman` |
| 4 | `params[0].get_str()` | same, after `EnsureConnman` |
| 5 | v2 default = `GetLocalServices() & NODE_P2P_V2` | same |
| 6 | `use_v2 && !node_v2` → -8 with Core's text, before the command branch, so `remove` gets it too | same |
| 7 | `add` -23 / `remove` -24, exact texts | same |

Arity and type errors come from the shared spec check, which also runs before Core's lambda. All of this is diffed live
(`rpc_diff.rs:1032-1036`, the -8 loop at `:1071-1075`). The unit tests pin the command-before-connman order
(`peers/tests.rs:577-584`). No finding.

**2. `onetry`.**
- **The call:** `dest_request` (`connman.rs:305-315`) gives a MANUAL request with no grant and `count_failure: false`,
  which is Core's `OpenNetworkConnection(CAddress(), false, {}, node, MANUAL, use_v2)`.
- **No slot:** the v1 onetry at `-maxconnections=0` is diffed live.
- **Inactive or interrupted:** `NetCore::open` returns early (`netcore.rs:916-918`), as at `net.cpp:3599-3604`. The
  implementer's mutant 4 is killed.
- **Already connected by name:** `netcore.rs:939` matches `ExistsNode(std::string)`, including Core's `fDisconnect`
  skip (`net.h:1702-1710`).
- **Already connected by address:** `resolve_dest` checks `exists_service` (`netcore.rs:1022-1025`), as at
  `net.cpp:416-421`. Mutant 5 is killed.
- **IPv4-mapped spelling:** both nodes refuse the second connection (diffed).
- **Unseparated check:** the by-name check is not separable on loopback (B-4).

**3. `add`/`remove`.**
- **Shared list:** `NetCore::add_node` and `remove_added_node` (`dial.rs:346-370`) work on the list that `tick_addcon`
  reads (`dial.rs:812-870`, Core's `ThreadOpenAddedConnections`, `net.cpp:3368-3395`).
- **`add`:** dedups by name or by an equal `LookupNumeric`, so the mapped spelling gives -23.
- **`remove`:** matches names only, so the mapped spelling gives -24.
- **CJDNS flip:** rdashd's `lookup_numeric` flips to CJDNS and Core's `AddNode` doesn't. Both sides of the comparison
  start as IPv6 and the flip is injective there, so equality is unchanged.
- **Disconnects:** **`remove` does not disconnect**, which is right: Core's `RemoveAddedNode` (`net.cpp:4323-4333`)
  only erases the entry. This is pinned live; mutant B3 is killed.
- **New dials:** an `add` after reactivation is dialled within the 2 s tick (diffed).

**4. Concurrency.**

*D2, `onetry` blocking the RPC thread.* The blocking is safe:
- **Threads:** handlers run on plain std worker threads (`dashd-rpc/src/http/mod.rs:435`), and the network runtime is
  multi-thread (`p2p/mod.rs:394`). `Handle::block_on` (`connman.rs:260`) is valid there, and `attach`'s `spawn`s run
  inside its runtime context.
- **Slot lock:** the lock is released before the connect (`rpc.rs:389-392`).
- **Shutdown:** the order is `interrupt` → `rpc.stop()` (which joins the workers, `http/mod.rs:414-432`) → `detach` →
  `net.stop` (`node.rs:388-404`). So no `block_on` outlives the runtime. An in-flight connect delays shutdown by up to
  `-timeout`, as Core's `ConnectDirectly` does; `attach_inner` refuses the peer once the core is interrupted.
- **Duplicate onetries:** two concurrent onetries to the same name can both connect. Core has the same race: nothing
  holds a lock across `ExistsNode` → `ConnectNode` → `m_nodes.push_back`.

*D4, one-step wiring.*
- **When:** the slot attaches only after `net.start` succeeds (`node.rs:333-335`). A failed start returns before the
  attach, and `detach` is a `take()`.
- **Warmup:** `RpcServer` answers `RPC_IN_WARMUP` until step 13, so the 7c→12 move has no visible effect.
- **`Arc<ConnMan>`:** `Drop for ConnMan` (an interrupt) now runs only on the last Arc. `Net::stop` interrupts and joins
  through its own reference (`p2p/mod.rs:625-630`), and the slot's clone is gone by then.

No finding.

**5. Test strength.** One real gap: B-1.

**6. Code quality.** The code is clean and matches the surrounding style. Using match guards with side effects
(`peers.rs:413-424`) is taste; I agree with leaving it. Smaller divergences are B-2 to B-4.

### B-1. minor (blocking): an explicit `v2transport=false` on a v2 node is never exercised, and two mutants survive

**Evidence:**
- **`onetry` path:** the argument travels through
  1. `NetSlot::open_one_try` (`dashd-node/src/rpc.rs:389-392`);
  2. `ConnMan::open_one_try` → `dest_request(…, use_v2)` (`dashd-net/src/connman.rs:245-251, :305-315`);
  3. `NetCore::attach`'s transport choice.
- **`add` path:** through `NetSlot::add_node` → `AddedNode { use_v2 }` (`rpc.rs:396-403`), and then `tick_addcon`'s
  `use_v2: info.use_v2` (`dash-p2p/src/dial.rs:848`).
- **What the live test sends:** in the v2 pair (`rpc_diff.rs:1037-1063`), only the default (= true). In the v1 pair,
  only the default (= false) or an explicit `true` (-8). On every connection it opens, the argument equals the node's
  own default.
- **What the unit test covers:** `addnode_takes_v2_as_core` (`peers/tests.rs:549-574`) checks the dashd-rpc dispatch
  against a FakeNet. It never reaches the node plumbing.
- **Result:** B1 and B2 pass `addnode_matches_dashd_v24`.

**Failure scenario:** the use case is a v2 node told `addnode <v1-only peer> onetry false` (or `add … false`). Under B1
or B2, rdashd would open v2 where Core opens v1. Core's reconnect-as-v1 fallback could then hide or reshape the error.
`getpeerinfo`'s `transport_protocol_type` and `getaddednodeinfo` (once served) would differ from Core.

**Fix:** in the v2 pair, add a second hub (`hub2`):
1. `["<hub2>","onetry",false]`: both nodes then list `manual/v1` for hub2 next to `manual/v2` for hub1. This kills B1.
2. After the `remove` steps, `["<hub2 or hub3>","add",false]` against a peer that is not connected (cycle
   `setnetworkactive` or use a third hub). Wait for the added-node loop to list it as `manual/v1`. This kills B2.

That is two steps on the existing `Diff` helpers.

### B-2. nit: a node string with a NUL: Core connects to the truncated name, rdashd refuses

**Evidence:**
- **Core:** passes `node_arg.c_str()` to `OpenNetworkConnection` (`rpc/net.cpp:355-357`). So `ExistsNode(std::string(pszDest))`
  and `Lookup(pszDest, …)` see the name cut at the first NUL. The added-node loop does the same with
  `info.m_params.m_added_node.c_str()` (`net.cpp:3385`).
- **rdashd:** keeps the full string, and `pre_lookup` refuses any host containing `'\0'` (`dash-p2p/src/seeds.rs:58`).
- **Probe (rc.3 vs head), `addnode "127.0.0.1:<hub>\u0000junk" onetry`:**
  - dashd: null, then `getpeerinfo` = `[('127.0.0.1:<hub>', 'manual')]`;
  - rdashd: null, then `getpeerinfo` = `[]`.
- **Probe, `add` with the NUL:** both answer null. Core's `AddNode` keeps the full string; `LookupNumeric` is invalid
  because of `ContainsNoNUL`, so only names are compared.

**Fix:** reachable only by an authenticated RPC caller, so low priority. Either truncate at the first NUL where Core
takes `c_str()`, that is NetSlot's `open_one_try` and the added-node loop's `OpenRequest`, keeping the full string for
`AddNode`/`RemoveAddedNode`. Or add a D-row that records it as an accepted divergence.

### B-3. nit: `onetry`'s log lines differ from Core's

**Evidence:**
- **Prefix:** `netcore.rs:933` ("Failed to open new connection, already connected") and `:1023` ("Not opening a
  connection to {dest}, already connected to {}") log `info!(target: "net")`, which prints `[net:info] …`. Core's
  `net.cpp:384` and `:419` use `LogPrintf`, which has no prefix. The mapped-spelling `onetry` makes `:1023` reachable
  from RPC; `-connect` duplicates could already reach it.
- **Missing lines:** Core's three `NET_NETCONN` lines in `OpenNetworkConnection` (`net.cpp:3623, 3627, 3633`: "--
  connecting to", "-- ConnectNode failed for", "-- successfully connected to") have no rdashd counterpart. A
  repository grep at head finds none.

**Fix:** add both to the r1 F3 P2a-net `[net:info]` audit row; there is nothing to do in BL-394. If DTF-ported tests
start using `assert_debug_log` on the `onetry` path, take them then.

### B-4. nit: the by-name `ExistsNode` check in `open` has no separating test

**Evidence:** the plan accepts this (`plan-bl394.md`, "Not separable"). On loopback, `resolve_dest`'s address check
refuses the same connection. The check is separable when the name now resolves to a different address than the one the
existing connection used, as with multi-A or round-robin DNS. Then only `netcore.rs:939` stops Core's second
connection.

**Fix:** a dash-p2p unit test with the recording resolver (`names_tests.rs:40`):
1. Open by name `seed.test:p` resolving to A.
2. Switch the resolver to B.
3. Open the same name again: expect `AlreadyConnected` and no resolver call.

Optional.

**Part B verdict: CHANGES REQUIRED**, for B-1 only. The behaviour I read is correct, and each check in the brief
(order, codes, `onetry` slot and inactive rules, name and address detection, mapped spelling, `remove` not
disconnecting, D2, D4) matches rc.3 and has a killed mutant or a live diff, except the explicit `v2transport=false`
path. Adding the two hub2 steps from B-1 is enough for ACCEPT. B-2 to B-4 can ride along or go to the backlog.

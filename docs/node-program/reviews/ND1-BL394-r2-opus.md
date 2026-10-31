# ND1 BL-394 `addnode` r2 + BL-391 nits: narrow Opus re-check

**Branch:** `node/nd1-getnetworkinfo`, head **f3a4ed4d0**. All six commits in cad3b4272..f3a4ed4d0 are signed `G`.
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…` in ~/workspace/dash).
**Prior review:** reviews/ND1-BL391-r2-BL394-r1-opus.md. **Rulings:** DEC-462.

I read and built only `git archive f3a4ed4d0`, in `/work/scratch/nd1-review-r3` (0700). Its target was reflinked from
`/work/scratch/nd1-gni/target`, with every `.rs`/`Cargo.toml` touched so nothing came from the seed, and it ran with
`CARGO_BUILD_JOBS=8`. The scratch dir is deleted. I did not touch the worktree.

New series on top of cad3b4272:

| Commit | What |
|---|---|
| 8deada5b6 | BL-391 r1 fixes: the squash of aee663104 and 2715d0d54 |
| 8ba1ce110 | **new since r1:** a realv disconnect-filter test, and the `-[no]networkactive` forms in the getnetworkinfo diff |
| 8a5f77601 | BL-391 r2 nits A-3 and A-4 |
| f1dea067a | BL-394 feature (was 1e83faae2) |
| d6f50e98d | BL-394 review r1 (was 74e89c470) |
| f3a4ed4d0 | r2: B-1, B-2, B-4 |

## What I checked

1. **B-1.** Re-applied both surviving mutants and confirmed that each now fails `addnode_matches_dashd_v24` and that
   the unmutated run passes.
2. **B-2.**
   - The NUL cut in `NetCore::open` against rc.3's `OpenNetworkConnection` callers, `AddNode` and `RemoveAddedNode`.
   - Whether the live steps assert dashd's real answers.
   - Every caller of the cut path.
3. **B-4.** The new unit test with the by-name check removed.
4. **Squash and restack.** Tree and range-diffs.
5. **BL-391 nits.** The fixture script usage, the linger wording, and the realv test as the disconnect-filter test
   (with its mutant).
6. **New in f3a4ed4d0 and 8ba1ce110.** Anything new that r1 didn't see.

## Commands

| Run | Result |
|---|---|
| `git diff 2715d0d54 8deada5b6` | **empty**: the trees are identical |
| `git range-diff 2715d0d54~2..2715d0d54 8deada5b6~1..8deada5b6` | only the message changed (one added sentence on the NETCONN move); in the code, the NETCONN const and its import moved from inbound.rs to netcore.rs, as 2715d0d54 did |
| `git range-diff 2715d0d54..74e89c470 8a5f77601..d6f50e98d` | 1e83faae2 `=` f1dea067a; 74e89c470 → d6f50e98d adds only `slot.attach(…, net.connman().expect("started"))` in 8ba1ce110's realv test (plus the `[…; 8]` context line from 8ba1ce110) |
| Attach arity per commit | 8ba1ce110 and f1dea067a: 2-arg `attach` (+ `attach_connman`), tests call 2-arg. d6f50e98d: 3-arg, tests call 3-arg. Each commit is consistent |
| `cargo test -p dash-p2p --all-targets` | all pass, EXIT 0 (lib 131/131) |
| `cargo test -p dash-p2p --lib --no-default-features` | 131/131 |
| `cargo test -p dashd-rpc --lib` | 83/83 |
| `cargo test -p dashd-node --lib` | 152 passed, 3 ignored |
| `rdashd --test rpc_diff addnode_matches_dashd_v24` (pristine, dashd rc.3) | pass, 11.6 s |
| `rdashd --test rpc_diff getnetworkinfo_matches_dashd_v24` (pristine) | pass, 11.7 s |

**Mutants.** I made one build with env-gated mutants (`RVMUT=<id>`), and ran each on its own (rpc_diff one at a time):

| id | Mutant | Test | Result |
|---|---|---|---|
| none | gates inert | addnode diff | pass, 14 s |
| **b1** | `NetSlot::open_one_try` uses `we_advertise_v2()` and ignores `use_v2` | addnode diff | **killed**, 11 s: "when the v1 onetry has returned: rdashd has [manual/v2, manual/v2], expected [manual/v2, manual/v1]" (also "after onetry, add and remove" and "after the v1 add") |
| **b2** | `NetSlot::add_node` stores `we_advertise_v2()` | addnode diff | **killed**, 11 s: "after the v1 add: rdashd has [manual/v2, manual/v1, manual/v2], expected […, manual/v1]" |
| **n1** | no NUL cut in `NetCore::open` | `names_tests::open_cuts_a_name_at_its_first_nul` | **killed** (`names_tests.rs:539`) |
| n1 | same | addnode diff | **killed**, but only by the 120 s `wait_until("the added node connected on both")` timeout (128 s); see m-1 |
| **e1** | by-name `ExistsNode` check dropped (`netcore.rs:949`) | `names_tests::open_refuses_a_name_already_connected_before_resolving_it` | **killed** (`names_tests.rs:528`); also kills the NUL test |
| **a4** | `network_info` counts marked peers (`rpc.rs:335`) | `realv::the_rpc_slot_skips_a_peer_being_disconnected` | **killed** (`realv.rs:489`, "the marked peer was counted", outbound 1 vs 0) |
| none | gates inert | the dash-p2p and realv tests above | pass |

## Results by item

### 1. B-1: fixed

- **Hubs and steps:** `hub2` and `hub3` are added (`rpc_diff.rs:1044-1055`).
  - `["<hub2>","onetry",false]`, then `[manual/v2, manual/v1]` (`:1081-1086`).
  - `["<hub3>","add",false]`, then a wait for 3 settled peers with none `/detecting`, then `[v2, v1, v1]`
    (`:1104-1110`).
- **Mutants:** b1 and b2 each die within seconds, on a diff message that names the transport.
- **Expectations:** the expected lists are dashd's actual state too. `expect_peers` checks both nodes, and the
  pristine run passes.

### 2. B-2: correct and matches rc.3

- **Callers in Core.** Every name caller of `OpenNetworkConnection` in rc.3 passes `c_str()`:
  - `rpc/net.cpp:358` (`node_arg.c_str()`, in the `onetry` block at 355-360);
  - `net.cpp:2063` `AddConnection`;
  - `:2823` `ProcessAddrFetch`;
  - `:2933` `-connect`;
  - `:3385` the added-node thread;
  - `:4994` `PerformReconnections` (`item.destination.c_str()`).

  The two non-name callers (`:3290` `ThreadOpenConnections`, `:3670` `OpenMasternodeConnection`) pass `nullptr`.
  `OpenNetworkConnection` then uses `ExistsNode(std::string(pszDest))` (`:3619`), and `ConnectNode` uses the same
  `pszDest`. So cutting once, at the top of `NetCore::open` (`netcore.rs:919-926`), is exactly right.
- **`AddNode` and `RemoveAddedNode`** (`net.cpp:4309-4333`) compare the full `std::string`. `LookupNumeric` of a
  string with a NUL is invalid, so only names compare. rdashd's `add_node` and `remove_added_node` are unchanged and
  keep the full string. Correct.
- **Other callers of the cut path.** I checked each `Target::Dest` producer:
  - `NetCore::manual`, from `-connect`, `-seednode`/AddrFetch and the added-node loop (`dial.rs:443-453, 511, 548, 849`):
    Core cuts all three, so this is a faithful change. `-connect` and `-seednode` strings come from argv or config,
    so a NUL there is close to unreachable anyway.
  - v1-retry reconnection (`netcore.rs:1366`): uses `p.dest()`, which was already cut when it was attached. That
    matches Core's `m_dest`.
  - `ConnMan::open_one_try` and `add_connection` (`connman.rs:245-251, 291-297`): `AddConnection` passes `c_str()`
    in Core too.
- **The added-node loop's "connected" test.** `unconnected_added_nodes` (`dial.rs:389-408`) compares the full stored
  string to the peer's (cut) `addr_name`, so a NUL entry is never "connected". Each pass then calls `open`, gets
  `AlreadyConnected` and sets `tried`, so the next wait is 60 s. Core's `GetAddedNodeInfo` (`net.cpp:3337-3353`) has the
  same mismatch, and its `ExistsNode` refuses the open. **Same behaviour**, including the 60 s cadence.
- **Not production.** The test-only `OpenFilter` (`connman.rs:247`, `driver.rs:92`) sees the uncut name. Only
  dashd-net's loopback test sets one, and it never sends a NUL. `SimExec::connect` doesn't cut (n-2).
- **Live steps** (`rpc_diff.rs:1143-1168`, pair 3, `-v2transport=0`):
  - **dashd's codes are asserted:** `errors == [-23, null, -24, null, null]` (`:1166`) is read from dashd's bodies.
    `Diff::same` returns `theirs`, and rdashd must match byte for byte.
  - **Peers are asserted on both nodes:** `[manual/v1]` after `onetry "<hub2>\0junk"`, nothing new after
    `"<hub2>\0other"`, `[v1, v1]` after `add "<hub3>\0junk"` (waited for), and still `[v1, v1]` after the removes.
    They match my r1 `nulprobe.py` result and Core's code above, and the pristine run passes.
  - **The connection targets** are not checked by address, but the only way rdashd can connect from those strings is
    the cut name.

### 3. B-4: fixed

`open_refuses_a_name_already_connected_before_resolving_it` (`names_tests.rs:520-530`) attaches a peer named
`seed.test:9999` at 5.6.7.8. It then opens the name again and expects `AlreadyConnected` with **no resolver call**. With
the check removed (e1), the open resolves to 1.2.3.4, which no peer has, and fails with a different error. The test
fails as intended.

### 4. Squash and restack: confirmed

8deada5b6's tree is byte-identical to 2715d0d54's. Apart from the message (the NETCONN sentence is added), the patch is
aee663104 + 2715d0d54, with the const moved straight to `netcore.rs`. The A-2 bisect hole is gone. The BL-394 restack
adds only the `attach` arity change to the realv test that 8ba1ce110 introduced. The arity is consistent at each commit.

### 5. BL-391 nits: done

- **A-3 script usage:** `contrib/node/getnetworkinfo-fixtures.py:15` now reads `--out ./raw`.
- **A-1 linger wording:** `/work/scratch/nd1-gni/plan.md:84, :119` now say "up to about 600 ms" after an off-sweep mark
  (500 ms `SWEEP_INTERVAL` + 100 ms `LINGER`), with the in-sweep mark at 100 ms. That is the corrected rationale.
- **Disconnect-filter test:** `the_rpc_slot_skips_a_peer_being_disconnected` (`realv.rs:462-494`) is a good stand-in
  for `network_info`:
  - **No race:** "still listed after the read ⇒ listed during it" is sound, because removal is final. A sweep that
    wins the race only causes a retry.
  - **Mutant:** a4 dies.
  - **Gap:** it does not cover `peer_info`'s own filter (`rpc.rs:312`). plan.md:119 defers that to the
    `disconnectnode` follow-up, which is consistent with DEC-462.

### 6. New since r1

- **8ba1ce110: realv test and the networkactive forms.**
  - The realv test is reviewed above.
  - The four forms diff cleanly in the pristine run.
  - The `active` rule (`rpc_diff.rs`, `inactive = ["-networkactive=0","-nonetworkactive","-nonetworkactive=1"]`)
    matches ArgsManager negation, and dashd's own `networkactive` is asserted.
- **8a5f77601, A-4.** The served-client wait now waits until the client's `getpeerinfo` shows two `subver`s. A node
  sends its version only after attaching the inbound peer, so both counts are settled before the diff. Good.
- **f3a4ed4d0** brings nothing new beyond m-1 and the nits below.

## Findings

### Blocker

None.

### Major

None.

### Minor

**m-1. The n1 mutant dies only by the 120 s timeout in the live diff, and three waits share one label.**

- **Where:** `rpc_diff.rs:1109`, `:1137`, `:1152`.
- **What happens:** without the cut, `nul when onetry has returned` (`:1149`) records a diff failure. The test then
  blocks in `wait_until("the added node connected on both")` (`:1152`) until `PATIENCE` runs out. The panic message,
  "timed out waiting for the added node connected on both", hides the recorded failures. The same text is used at
  `:1109` and `:1137`, so you can't tell from the message which pair hung. This is the same pattern as r1's A-4.
- **Why it's minor:** not blocking. The dash-p2p unit test kills n1 in milliseconds with a precise assert.
- **Fix:**
  1. Give each wait its own label, for example "v2 pair: hub3 added", "v1 pair: hub added" and "nul pair: hub3
     added".
  2. Optionally, check `diff.failures.is_empty()` (or wait on dashd's side only) before the `:1152` wait, so a
     refused NUL onetry fails within seconds with its diff.

### Nits

**n-1. The cite is off by one.**

- **Where:** `netcore.rs:918` and `names_tests.rs:534` cite `rpc/net.cpp:355-357`.
- **Detail:** the `OpenNetworkConnection(… node_arg.c_str() …)` call is at line **358**, inside the `onetry` block at
  355-360. I made the same slip in r1.
- **Also:** the doc says "every caller passes `c_str()`", so the cite could list the other name callers too
  (`net.cpp:2063, 2823, 2933, 4994`).

**n-2. `SimExec::connect` does not cut at the NUL.**

- **Where:** `dashd-net/src/sim.rs:151`.
- **Detail:** it mirrors `NetCore::open`'s Core checks for the sim, but has no NUL cut. A sim script with a NUL name
  would give `Resolve` where the real `open` gives `AlreadyConnected` or a connection. No sim feeds such names today.
- **Fix:** add the same three lines, or a one-line note that it is out of scope.

**n-3. `peer_info`'s disconnect filter is still untested.**

- **Where:** `rpc.rs:312`.
- **Detail:** this was already deferred to the `disconnectnode` follow-up (plan.md:119). I'm only recording it so the
  follow-up isn't lost: the realv test could read `slot.peer_info()` in the same iteration at little cost.

### Not run

clippy was not run (not in this brief). The other rpc_diff tests (getpeerinfo and the rest) were not rerun:
f3a4ed4d0 touches only the addnode test and `NetCore::open`. That open is exercised by the dash-p2p, dashd-node and
both diff runs above.

**Verdict: ACCEPT WITH NITS.** B-1, B-2 and B-4 are fixed. Each has a test that fails without it: b1, b2, n1, e1 and
a4 are all killed. The squash and restack are clean, and the BL-391 nits are done. m-1 and n-1 to n-3 can ride along or
go to the backlog.

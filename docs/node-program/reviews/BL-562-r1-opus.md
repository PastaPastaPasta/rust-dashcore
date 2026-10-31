> Durable copy for node-program reviews/ (DEC-984). A few words may be reworded for the commit gate; the findings and the verdict are unchanged.

# BL-562 r1 review (Opus): correctness

- **Head:** `d7814ca0cc6afa575d98127a608a1ba0ad65bcdc` (`node/nd1-bl562`), two commits on `3807cb2e3`. Diff: `rdashd/tests/dashd_v24.rs` only.
- **Reviewer scratch:** `/work/scratch/bl562-r1-opus/`. I built and ran in a reflink copy of the worktree (`src/`) with `target/` seeded from the owner's target; both are now deleted. Logs: `run.log`, `oldnew.log`. Scripts: `run.sh`, `oldnew.sh`, `hold.py`.
- **Core:** `v24.0.0-rc.5:src/init.cpp`. rc.4→rc.5 changes only 4 lines of `init.cpp`, none in the code cited below. The oracle binary is rc.4 from `~/.rust-dashcore-test`.

## Verdict: APPROVE-WITH-NITS

No blockers, majors or minors. The fix does what BL-562 asks. Adding one free port to a case that sets only the other keeps every case's error and text, in both binaries; I checked this by running both. No case binds a fixed or default port any more.

## What I checked

### 1. Every case keeps its meaning (checked by running both binaries)

Core's order (`init.cpp`):
- `AppInitServers` (the RPC bind) runs at :1636, in step 4a. Step 6 starts at :1649.
- The spork checks come before the port loop.
- The port loop at :1760-1772 checks `-port`, then `-rpcport`. It errors only on a set option that is unparsable or 0.
- The socket-address checks follow (:1774-1824).
- Then `-uacomment`.

So a valid `-rpcport` added after a bad `-port` can't win, and a valid `-port` added beside `-rpcport=0` can't either.

For every case whose argv this change alters, I ran dashd rc.4 and rdashd with the old argv and with the new one (`oldnew.log`). The exit code and stderr were identical in all of them:

| Case | Added | Error, both binaries, old = new |
|---|---|---|
| `-regtest -server=0 -port=0` | `-rpcport` | Invalid port specified in -port: '0' |
| `-regtest -server=0 -port=x` | `-rpcport` | Invalid port specified in -port: 'x' |
| `-regtest -server=0 -port=65536` | `-rpcport` | Invalid port specified in -port: '65536' |
| `-regtest -sporkaddr=x -port=0` | `-rpcport` | Invalid spork address specified with -sporkaddr |
| `-regtest -sporkkey=x -port=0` | `-rpcport` | Unable to sign spork message, wrong key? |
| `-regtest -port=0 -uacomment=a/b` | `-rpcport` | Invalid port specified in -port: '0' |
| `-regtest -server=0 -port=22 -whitelist=1.2.3.4/33` | `-rpcport` | bad-port warning + Invalid netmask specified in -whitelist |
| `-regtest -rpcport=0` | `-port` | Invalid port specified in -rpcport: '0' |
| `-regtest -server=0 -rpcport=1 -noport` | nothing (as before) | Invalid port specified in -port: '0' |

- **The `-noport` clause is load-bearing.** As a counterfactual, I appended `-port=N` after `-noport`. Both binaries then no longer fail init (killed by `timeout` at 20 s, code 124). Without the `starts_with("-noport")` clause the case would silently stop testing anything. The clause is correct.
- **The `$PORT` case:** with fresh ports, both binaries still report `Unable to bind to 127.0.0.1:<PORT1> on this computer. … is probably already running.` + `Failed to listen on any port.` The port in the text is the same for both because `with_ports` runs once per case, at the caller (`dashd_v24.rs:276-280`), before both `run_case` calls.
- **`PORT1 ≠ PORT2` always.** `free_port` advances its per-process cursor after every pick (`node-test-ports/src/lib.rs:59-63`). It wouldn't matter anyway: with an `=onion` `-bind`, `-port` is never bound (`bind_on_any` is false).

### 2. No fixed or default port is bound any more

- **Which cases reach `CConnman::Start`:**
  - Only two cases get past the `-i2psam`/`-onlynet` check at :2736-2751 to `CConnman::Start` (:2755): `-bind=192.0.2.1:29989 -listenonion=0` and the `$PORT` onion case.
  - Both set a `-bind`, so neither adds the default onion bind. At :2686 Core pushes `DefaultOnionServiceTarget()` only when `onion_binds` and `vBinds` are both empty.
  - So `127.0.0.1:19896` is never bound.
  - 192.0.2.1 is not on the host, so 29989 fails with EADDRNOTAVAIL whoever holds it.
- **The other literals:**
  - 29992/29993 fail at `-whitelist=x` (:2716-2721), before Start.
  - The devnet cases fail in `AppInitParameterInteraction` (:1265-1269, or SelectParams/InitConfig), before `AppInitServers`.
  - The last devnet case sets `-listen=0 -server=0`.
- **The RPC server:** every case that reaches step 4a with `-server` on now carries a free `-rpcport`. Before the change, the three cases the owner found (spork ×2, `-port=0 -uacomment`) fell back to regtest's 19898.
- **Run with the ports held (`run.log`):**
  - `hold.py` listened on both 127.0.0.1 and ::1 at 21 ports:
    - regtest 19896-19899;
    - testnet 19996/19998/19999;
    - devnet 19796/19798/19799;
    - 29989-29999.
  - All 42 binds were taken by me (none were already held).
  - Results:
    - `init_errors_match_dashd_v24` alone: PASS (unheld run first, also PASS);
    - `an_unopenable_chainstate_fails_before_the_block_index`: PASS;
    - five copies of the main test at once: 5/5 PASS (34-53 s each at load ~55).

### 3. `with_ports`

The substitution is correct for the two names in use. `get_or_insert_with` makes each name one port for the whole case, and an arg that holds both names gets both replaced. See nits N1/N2 for future-proofing.

### 4. The devnet caller

`an_unopenable_chainstate_fails_before_the_block_index` (`dashd_v24.rs:330-352`) passes `-devnet=nd1 -listen=0 -server=0`, so `devnet` is true and neither port is added. That is the same as before: the old predicate also skipped any `devnet` arg. It passes.

### 5. The owner's evidence

`check2.log` prints `head eeb2c9e40`. The run was on the working tree with the second change staged, not on the commit. `staged2.diff` ends at blob `3c91c25f1`, the same blob `d7814ca0c` commits, and its hunks match `git diff eeb2c9e40 d7814ca0c` exactly. So the evidence covers the reviewed head. I reran it on the head's content myself (above).

## Findings

**N1 (nit): a mistyped placeholder passes silently.** `dashd_v24.rs:192-206`.
- **Problem:** a placeholder `with_ports` doesn't know (`$PORT3`, `$PORT_1`, a typo) is left literal. Both binaries then fail with `Invalid port specified in -rpcport: '$PORT3'` (or the `-bind` form), so the comparison passes. The case would quietly turn into a port-parse case.
- **Suggested fix:** after substitution, `assert!(!arg.contains("$PORT"), "{arg}: unknown port placeholder")`.
- **Same family:** `$PORT1` also matches the start of `$PORT10`. That is harmless with two names, and the same assert would flag it.

**N2 (nit): the `CASES` doc now contradicts itself.** `dashd_v24.rs:19-20`.
- **Problem:** it says the cases fail "before binding any port", then that "a case that binds one names it `$PORT1`…". Most cases also bind the RPC port (run_case_on's own `-rpcport`), and the `$PORT` case binds by design.
- **Suggested fix:** something like "…fail init. Ports a case binds are its own: …".

**N3 (nit, already there before, residual): a port can be taken between pick and bind.** `dashd_v24.rs:276-280`.
- **Problem:** `free_port` picks without reserving. The `$PORT` case can't retry the way `start_on_free_port` does, because a lost port changes its error. If another process listens on PORT1 after dashd exits and before rdashd starts, that one case fails.
- **Why it's acceptable:** this is far rarer than the fixed-port collision BL-562 fixes. Picks sit below the ephemeral range and walk from a per-process start, and five concurrent copies passed for the owner and for me.
- **Suggested fix:** none required. Optionally, mention it in the case's comment.

## Cleanup
- I stopped every process I started: the port holder was killed, and the counterfactual dashd/rdashd were ended by `timeout`. No strays (`pgrep`).
- I removed `/dev/shm/bl562-r1-opus-tmp`.
- I removed the reflink `src/` and `target/` with `rm-if-idle.sh`. Only the logs and scripts remain in my scratch.

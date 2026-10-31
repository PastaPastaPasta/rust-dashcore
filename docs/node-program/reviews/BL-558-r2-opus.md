> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-558 r2 narrow re-review (Opus)

- r1 head `6d0f98857`; fix commit `c43b019daeeeaee8d071d602380984ca46caef27` (head of `node/nd1-bl558`; the worktree is clean at that SHA). Delta: `rdashd/README.md` only, +31/−20.
- Scope: my r1 findings (B1, m1, n1–n3) and the lines the fix touches. I read the code and Core and did not rerun anything. The inbound recipe is the config I ran in r1 (regtest, my ports): rdashd bound only `0.0.0.0:<port>`, the onion target I gave it and RPC, and nothing on 19896. The owner's check covers the same config. No scratch files were created.

## r1 findings

| r1 | Status | Reason |
|---|---|---|
| **B1** (blocker): the main recipe collides on the default onion bind | **CLOSED** | The README now leads with `listen=0` (`:48-54`), which soft-sets `-listenonion=0` and opens no listener. The inbound variant (`:56-64`) sets `port=21999`, `bind=0.0.0.0` and `bind=127.0.0.1:21996=onion` inside `[test]`. `-bind` is NETWORK_ONLY, so it belongs there. Because a `-bind` is given, no default onion target is added (Core rc.5 `init.cpp:2686-2688`; rdashd `dashd-node/src/p2p/mod.rs:351-353`). Because the onion bind is non-wildcard, `-listenonion=1` passes both onion checks. A `-bind` without a port takes `-port`. 21996 isn't a default or bad port. The table (`:36-40`) gains the onion-target column (9996 / 19996 / 19896, matching r1), and `:42-43` explains why changing `port` alone collides. The CLI example (`:70`) uses `-listen=0`, so it needs no `-bind`. |
| **m1** (minor): wrong error line for an RPC collision | **CLOSED** | `:31-32` quotes the stderr line, `Error: Unable to start HTTP server. See debug log for details.`, and says the log has `Unable to bind any endpoint for RPC server`. Both match my r1 run and Core (`init.cpp:1636-1637`, `httpserver.cpp:499`). |
| **n1** (nit): the config error drops its `Error: ` prefix | **CLOSED** | `:25` now quotes `Error: Error reading configuration file: `, the exact line from r1. The trailing space survives in CommonMark because only one side of the code span has a space. |
| **n2** (nit): no Windows row in the data-directory table | **NOT CLOSED** (nit) | The owner's ruling is that rdashd is Unix-only. The tree says otherwise. `rdashd/src/main.rs:88` and `signals.rs:229` have `cfg(windows)` paths (UTF-8 console, `SetConsoleCtrlHandler`). `dash-args/src/paths.rs:419-438` gives `%APPDATA%\RDashCore`. CI builds `rdashd.exe` and runs the node-runtime group on `windows-latest` (`.github/workflows/build-and-test.yml:99`, `rust.yml:225-232`). DEC-84 makes Windows a supported platform. A Windows reader finds no default directory and no statement that Windows is unsupported. Fix: add a row reading `Windows \| %APPDATA%\RDashCore`. |
| **n3** (nit): "the same options … to both programs" is loose | **CLOSED** | `:66-67` now says `rdashd` takes the same options with a leading `-`, and `rdash-cli` needs only the network and the RPC port. That is right: `rdash-cli` reads `rpcport` from `[test]` when given `-testnet`, or from `-rpcport`. |

## New defects in the touched lines
None.
- `:42`, "Without a `bind` option, a listening node also binds the onion target": this is exact. A `-whitebind` alone still gets the default target, and the sentence still holds for it.
- `:33-34`: the P2P/onion error text matches the rdashd output from my r1 run (`Error: Unable to bind to 127.0.0.1:23513 on this computer. rdashd is probably already running.`).
- The code spans that wrap across source lines (`:31-32`, `:33-34`) render with a space, which is fine.

## Verdict
**APPROVE-WITH-NITS.** B1, m1, n1 and n3 are closed. n2 is still open as a nit, and the reason given for leaving it (rdashd is Unix-only) contradicts the code, the CI and DEC-84.

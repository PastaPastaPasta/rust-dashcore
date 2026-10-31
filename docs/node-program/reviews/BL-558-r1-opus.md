> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-558 r1 review (Opus, correctness)

- Head reviewed: `6d0f988570eabbf8cb69507dcd0d0482857b70b6` (`node/nd1-bl558`, one commit on `c471a6ddf`), new file `rdashd/README.md`.
- Core reference: `v24.0.0-rc.5` (`/work/home/agent/workspace/dash`); the dashd binary is `~/.rust-dashcore-test/dashcore-24.0.0-rc.5/bin/dashd`.
- rdashd binary used: a copy of `/work/scratch/nd1-bl560/target/debug/rdashd` (built 10-30 22:57). Its bind and init code paths are unchanged by this docs-only commit.
- Scratch: `/work/scratch/bl558-r1-opus/`. Only my own ports (23511–23522), datadirs under scratch, no defaults bound. Every process is stopped and the datadirs and binary copy are deleted. `onion-test.sh` and the `*.out` logs are kept as evidence.

## Findings

### B1 (blocker): the main side-by-side recipe still collides, on the default onion bind
`rdashd/README.md:39-55`

The README says that setting `port` and `rpcport` in the network section lets rdashd run beside dashd. It does not. When no `-bind` is given, rdashd, like Core v24, also binds the Tor onion target `127.0.0.1:<OnionServiceTargetPort>`. That port is fixed per network and does not follow `-port`: 9996 on mainnet, 19996 on testnet, 19896 on regtest. A failed onion bind is fatal.

- **Core rc.5:**
  - `init.cpp:2686-2688` adds `DefaultOnionServiceTarget()` whenever `onion_binds` and `vBinds` are empty, whatever `-listenonion` says.
  - `torcontrol.cpp:708-713`: the target is 127.0.0.1 on `BaseParams().OnionServiceTargetPort()`.
  - `net.cpp:3981-3984` binds onion targets with `BF_REPORT_ERROR` and `return false`.
- **rdashd:**
  - `dashd-node/src/p2p/mod.rs:351-353` adds the same default.
  - `dashd-net/src/binds.rs:182-184` binds it and `map_err(fatal)?`s on failure.
- **Run:** I held 127.0.0.1:23513 with a socket and started each node with `-regtest -port=23511 -rpcport=23512 -bind=127.0.0.1:23513=onion` (`onion-test.sh`). Both exit 1:
  - rdashd: `Error: Unable to bind to 127.0.0.1:23513 on this computer. rdashd is probably already running.` then `Error: Failed to listen on any port. Use -listen=0 if you want this.`
  - dashd rc.5: the same, with "Dash Core is probably already running."
  - I used an explicit onion bind so as not to bind a default port. The code above shows the default path takes the same fatal branch.

**Failure scenario:** a laptop runs dash-qt or dashd on mainnet. Both listen by default, so 127.0.0.1:9996 is held. The user copies the `[main] port=20999 rpcport=20998` block. rdashd now gets past the RPC bind and dies at step 12 with "Unable to bind to 127.0.0.1:9996 … rdashd is probably already running". The `[test]` block fails the same way against a testnet dashd, on 19996. Started in the other order, it is dashd that fails. Only the `listen=0` variant works. That is the one the owner's check exercised (`/work/scratch/nd1-bl558/h/.rdashcore/dash.conf`: `listen=0`), so the check did not cover the recipe the README leads with.

**Fix:** in each network section, replace the default binds with explicit ones that include a separate onion target. For example:

```ini
[main]
port=20999
rpcport=20998
bind=0.0.0.0
bind=127.0.0.1:20996=onion
```

- `-bind` without a port uses `-port`, per `default_bind_port`.
- `-bind` is NETWORK_ONLY (`dash-args/src/options/generated.rs`), so it belongs in the section.
- Alternatively, `bind=0.0.0.0` plus `listenonion=0`. A plain `bind=` with the default `listenonion=1` is rejected with "The automatic Tor onion service requires a dedicated onion bind".
- An explicit `-bind` drops the implicit `[::]` bind, so add `bind=[::]` if IPv6 inbound matters.
- I ran this config (regtest, my ports): rdashd bound `0.0.0.0:23511`, `127.0.0.1:23513` and RPC `23512`, and nothing on 19896.
- The ports table should gain an "onion target" column (9996 / 19996 / 19896) and a sentence on why it collides.
- The command-line example (`:51-55`) needs the same `-bind` options.
- Or lead with `listen=0`, which is what a laptop usually wants, and give the bind recipe second.

### m1 (minor): the error the user sees for an RPC collision is a different line
`rdashd/README.md:31-32`

"Unable to bind any endpoint for RPC server" is a log line. It reaches stdout only through printtoconsole, and it goes to `debug.log`. The error on stderr, which is also what `-daemon` users get, is `Error: Unable to start HTTP server. See debug log for details.`

- Run: hold `127.0.0.1:23522` and `[::1]:23522`, then `rdashd -regtest -listen=0 -rpcport=23522` exits 1. stdout has both lines; stderr has only the HTTP-server line.
- Core matches: `httpserver.cpp:499` LogPrintf, `init.cpp:1636-1637` InitError.
- Fix: quote both lines. A user who greps stderr won't find the one the README quotes.

### n1 (nit): the quoted config error drops its prefix
`rdashd/README.md:25`

What prints is `Error: Error reading configuration file: `. Both rdashd and dashd rc.5 print exactly that, verified with `HOME=<empty dir> … -version`, exit 1. The README says the program "exits with `Error reading configuration file: `". That is a substring of the real line, so it's fine for grep, but quoting the full line is clearer.

### n2 (nit): no Windows row in the data-directory table
`rdashd/README.md:18-21`

`default_data_dir` (`dash-args/src/paths.rs:421-441`) gives `%APPDATA%\RDashCore` on Windows. Either add the row or say Windows is not a supported run target.

### n3 (nit): "the same options … to both programs" is loose
`rdashd/README.md:51`

`-port` (and `-bind`, `-listen`) are rdashd-only; rdash-cli takes only `-rpcport`. The example is right; only the sentence overstates it.

## Claims verified correct
- **Default data directories:**
  - `Product::dir_names` gives `RDashCore` / `.rdashcore`.
  - macOS is `~/Library/Application Support/RDashCore`; other Unix is `~/.rdashcore` (`paths.rs:403-454`).
  - Both binaries set `Product::RDashCore` (`rdashd/src/main.rs:40`, `rdash-cli/src/cli.rs:837`).
- **The directory must exist, even for `-version`:**
  - Core rc.5: `bitcoind.cpp:274-276` runs `ParseArgs` (which runs `InitConfig`, `:122`) before `ProcessInitCommands`. `ReadConfigFiles` creates an empty conf with `ofstream(…, app)` and returns false with no error text (`util/system.cpp:1002-1008`). The datadir is created only after that (`common/init.cpp`, "Create datadir if it does not exist").
  - rdashd: `main.rs:50-57` and `140-150` follow the same order, and `args.rs:1385-1390` does the same create-or-fail.
  - Run: both binaries print `Error: Error reading configuration file: ` and exit 1, and create nothing.
- **Default ports:** they match Core:
  - P2P 9999 / 19999 / 19899 (`dash-network/src/lib.rs:117-123`; Core `nDefaultPort`).
  - RPC 9998 / 19998 / 19898 (`dash-args/src/chain.rs:45-49`; Core `chainparamsbase.cpp:42-48`).
- **Error string:** "Unable to bind any endpoint for RPC server" (`dashd-rpc/src/http/mod.rs:296`) matches Core `httpserver.cpp:499`, with the caveat in m1.
- **Ports are network-only:**
  - `-port`, `-rpcport` (rdashd and rdash-cli) and `-bind` are `ALLOW_ANY | NETWORK_ONLY`.
  - `use_default_section` (`args.rs:1050-1052`) applies top-level keys of network-only args only on `main`, which is Core's `UseDefaultSection`. So "keys before any section apply to mainnet only" is right.
  - `[main]` and `[test]` are the right section names (`chain::MAIN = "main"`).
- **rdash-cli reads the same `dash.conf`:** `app_init_rpc` sets `RDashCore` and calls `read_config_files` (`cli.rs:~875-886`). It takes `-rpcport` from it (`cli.rs:254`), so it reaches rdashd, and reads the cookie from `~/.rdashcore`.
- **`listen=0`:** opens no P2P listener (the owner's check). It soft-sets `-listenonion=0`, so it also sidesteps B1. Outbound connections are unaffected.
- **Flags in the examples:** `-testnet`, `-port`, `-rpcport`, `-listen`, `-datadir` and `-version` are all `Implemented` in `dashd-node/src/init/support.rs`. So are `-bind` and `-listenonion`, which the B1 fix uses.
- **Example ports:** 20998 / 20999 / 21998 / 21999 are valid, below 32768, outside every network's defaults (including devnet's 19798 / 19799 and the onion targets), and not on Core's bad-port list.
- **Build line:** the package and binary names `rdashd` and `rdash-cli` are workspace members, so `cargo build --release -p rdashd -p rdash-cli` resolves.

## Verdict
**CHANGES-REQUIRED.** B1: the README's main recipe (`port` + `rpcport`) still fails beside a listening dashd or dash-qt, because both nodes bind the fixed default onion target (127.0.0.1:9996 / 19996). Only the `listen=0` variant works as written.

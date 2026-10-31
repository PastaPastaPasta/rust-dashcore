# rdashd

The Rust Dash node daemon: Dash Core's `dashd` main over `dashd-node`. It takes `dashd`'s options and
reads a `dash.conf` the same way. `rdash-cli` is its `dash-cli`.

## Build

```sh
cargo build --release -p rdashd -p rdash-cli
```

The binaries are `target/release/rdashd` and `target/release/rdash-cli`.

## Data directory

`rdashd` and `rdash-cli` default to their own data directory, not Dash Core's:

| OS | Default |
|---|---|
| Linux and other Unix | `~/.rdashcore` |
| macOS | `~/Library/Application Support/RDashCore` |
| Windows | `%APPDATA%\RDashCore` |

As with Dash Core v24, the directory must exist before the first run, even for `rdashd -version`
or `rdashd -help`.
Init reads `dash.conf` first and creates an empty one if there is none; that fails when the directory
is missing, and the program exits with `Error: Error reading configuration file: `. Create it first, or pass
`-datadir=<an existing directory>`.

## Running beside dashd or dash-qt

Both programs default to the same ports, so a second node on the same machine can't bind them, and
`rdashd` exits as `dashd` does. A taken RPC port ends init with `Error: Unable to start HTTP server.
See debug log for details.` (the log has `Unable to bind any endpoint for RPC server`). A taken P2P or
onion port ends it with `Error: Unable to bind to <address> on this computer. rdashd is probably
already running.`

| Network | P2P port | RPC port | Onion target |
|---|---|---|---|
| mainnet | 9999 | 9998 | 127.0.0.1:9996 |
| testnet | 19999 | 19998 | 127.0.0.1:19996 |
| regtest | 19899 | 19898 | 127.0.0.1:19896 |

Without a `bind` option, a listening node also binds the onion target, whatever `port` says, so
changing `port` alone still collides. Give `rdashd` its own settings in its `dash.conf`, inside its
data directory. `rdash-cli` reads the same file, so it then talks to `rdashd`, not to `dashd`. Set
them in the network's section: `port`, `rpcport` and `bind` are per network, and before any section
they apply to mainnet only.

The simplest setup doesn't listen at all: `rdashd` still connects out to peers.

```ini
[test]
rpcport=21998
listen=0
```

To accept inbound peers too, bind every listening address yourself, including an onion one:

```ini
[test]
rpcport=21998
port=21999
bind=0.0.0.0
bind=127.0.0.1:21996=onion
```

On the command line, `rdashd` takes the same options with a leading `-`. `rdash-cli` needs only the
network and the RPC port:

```sh
rdashd -testnet -rpcport=21998 -listen=0
rdash-cli -testnet -rpcport=21998 getblockchaininfo
```

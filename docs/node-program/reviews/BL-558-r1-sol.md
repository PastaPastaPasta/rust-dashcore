> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-558 correctness review, round 1

Reviewed head `6d0f988570eabbf8cb69507dcd0d0482857b70b6`, parent `c471a6ddfba60616478c2c31c9db1acb17058c1d`. The change is exclusively the new `rdashd/README.md`. Followed the task brief and `briefs/REVIEW-COMMON.md`; the reviewed worktree and branch were left unchanged.

## Findings

1. **major — `rdashd/README.md:39` (config example at lines 44–50 and command at line 56): changing P2P and RPC ports still leaves a fatal onion-listener collision.**

   Concrete scenario: dashd or dash-qt is already running on testnet with its default listeners. Following the README's `rdashd -testnet -port=21999 -rpcport=21998` frees the advertised P2P and RPC ports, but rdashd still tries to bind `127.0.0.1:19996`. The existing Core node owns that socket, so rdashd exits with the P2P bind error and `Failed to listen on any port. Use -listen=0 if you want this.` The mainnet configuration has the same problem on `127.0.0.1:9996`. Thus both primary examples fail in the ordinary side-by-side scenario this task is meant to fix.

   Evidence: `dashd-node/src/p2p/mod.rs:322–323` derives the onion port independently of `-port`; lines 351–352 insert the default onion bind whenever no ordinary or onion `-bind` was supplied. `dash-args/src/chain.rs:46–49` supplies the fixed per-network onion ports. `dashd-net/src/binds.rs:182–183` propagates any onion bind failure as fatal, and `dashd-net/src/connman.rs:128–139` propagates that failure out of startup. Core v24.0.0-rc.5's `src/init.cpp:2686–2687` also inserts this default listener, and its `CConnman::InitBinds` in `src/net.cpp` treats an onion bind failure as fatal. This collision does not depend on having a running Tor daemon. Adding only `listenonion=0` also does not remove the default onion bind: insertion occurs before and outside the `listenonion` condition.

   Suggested fix: make the examples use `listen=0` with their distinct RPC port, or show a complete listening configuration that also avoids the onion target collision. For example, an explicit ordinary bind on the alternate P2P port plus `listenonion=0` suppresses the default onion bind; retaining automatic Tor service instead requires explicit ordinary and unique onion binds. Apply the chosen solution consistently to both config and command-line examples. The final paragraph's `listen=0` alternative is valid, but its optional presentation does not fix the primary examples.

## Verification

- Verified the default data directories against `Product::dir_names` and `default_data_dir`, and verified both binaries select `Product::RDashCore`.
- Verified config reading precedes rdashd's version/help handling, and missing-parent config creation yields the documented empty configuration error. Compared the startup ordering and config creation with Core v24.0.0-rc.5's `src/bitcoind.cpp` and `src/util/system.cpp` as directed. The client has its own earlier help/version handling; the README's explicit example is rdashd.
- Checked all listed RPC ports in `dash-args/src/chain.rs` and Core's `src/chainparamsbase.cpp`; checked P2P ports in the pinned base-sdk revision `1eda0100802ce5f4b8d3ef1c011c74c39e76ea0a` and Core's chainparams. The table is accurate. All example ports are valid, below 32768, and outside the network defaults.
- Verified RPC error text, flag declaration/support, rdash-cli's shared config loading, and network-only port handling. Unknown daemon-only config keys are ignored by the client. Verified `listen=0` skips listeners while outbound dial threads still start, and disables automatic onion service through parameter interaction.
- Ran five isolated tests against a reflink copy of the reviewed `dash-args` source: documented config for daemon/client on mainnet/testnet; unsectioned RPC ports applying only to mainnet; both documented command lines and `-listen=0`; regtest section with `listen=0`; distinct Linux default data directory. **5 passed, 0 failed.** Harness: `/work/scratch/bl558-r1-sol/claims.rs`. Compiled directly with rustc into its own `/work/scratch/bl558-r1-sol/target`, after confirming five-minute load 91.72 and 229G available. Test `TMPDIR` was `/dev/shm/bl558-r1-sol-tmp`, subsequently removed.
- `git diff --check` passed. No full daemon build or daemon runtime experiment was performed; the major finding follows the explicit bind construction and fatal-error propagation in the reviewed source. No processes were left running. No restricted notes, `/work/cache`, or `/work/chains` were read.

Verdict: **CHANGES-REQUIRED**

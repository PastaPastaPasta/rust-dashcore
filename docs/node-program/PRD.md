# PRD: Rust Dash node, masternode, CLI and desktop wallet

| | |
|---|---|
| Status | Draft 1 for manager and pasta review |
| Date | 2026-10-08 |
| Owner | Product (task P1), for the program manager |
| Binding input | [`CHARTER.md`](CHARTER.md) |
| Research inputs | [R1](research/R1-rust-dashcore-state.md) (rust-dashcore state), [R2](research/R2-dashcore-inventory.md) + [R2b](research/R2b-rpc-inventory.md) (Dash Core inventory, 289 RPCs), [R3](research/R3-base-sdk.md) (base-sdk), [R5](research/R5-wallet-user-stories.md) (wallet stories). [R4](research/R4-consensus-and-verification.md) + `research/R4-appendix/` (consensus surface and verification strategy; reconciled in §0.5) |
| Desktop inputs | dashwallet-desktop `CLAUDE.md` ("Product scope"), `docs/HANDOFF.md`, `docs/parity.md` (measured 2026-10-06, after M2), `README.md`, `docs/design/DESIGN.md` |
| Companion docs | `designs/A1-architecture.md` (how; in progress), `PLAN.md` (when; to be written from this PRD), `STATUS.md` (live status) |

> **EMBARGO-1.** This PRD refers to an unfixed Dash Core security issue only by its codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Like the rest of `node/integration` (STATUS.md), this file must not be published until pasta clears disclosure (Q13).

## 0. About this document

### 0.1 What this PRD is for

This PRD says **what** the program delivers and **how we prove it**. It does not choose crate layouts, storage engines or threading models; those belong to A1. It is the acceptance ledger for the charter's "Done when": every story here has a stable ID, named surfaces, a priority, its Dash Core counterpart, the current rust-dashcore status and testable acceptance criteria.

### 0.2 Baselines

| Component | Baseline used in this PRD |
|---|---|
| Dash Core | Surveyed at `~/workspace/dash` develop `15db478750` (2026-10-06): v24.1.0-pre, `PROTOCOL_VERSION` 70242, `MIN_PEER_PROTO_VERSION` 70221; `v24.0.0-rc.2` is an ancestor. R4 also ran live checks against the v24.0.0-rc.3 release binary and found that `origin/develop` (`2340e3a265`) and rc.3 add a second deployment, `evo_shares` (bit 14). **Reference releases are pinned per network** (Q5): mainnet runs v23.1.x today; testnet follows v24.0.0 rc → final; develop is watched weekly |
| rust-dashcore | `dev` `67b307c6` |
| base-sdk | `develop` `33fcc59`; rust-dashcore pins `e6402ced` |
| dashwallet-desktop | `main`, parity ledger as of 2026-10-06 (M2). HANDOFF says M3 (CoinJoin client, restyle) has merged since, so parity.md lags for CoinJoin rows |

### 0.3 Conventions

**IDs.** Stories are `US-<AREA>-NN`. Functional requirements are `FR-<SUBSYSTEM>-NN`. Non-functional requirements are `NFR-<CLASS>-NN`. Release criteria are `REL-N.M`. Open questions are `Q<N>`. IDs are stable: a removed item keeps its ID with the text "Withdrawn".

**Story IDs reuse R2's numbering.** Where R2 §D already had a story, it keeps R2's ID (for example `US-NODE-01`, `US-MN-13`). New stories are appended after R2's last number in that area. R5's IDs map onto PRD IDs in Appendix A.

**Surfaces.**

| Code | Surface |
|---|---|
| **N** | The Rust node daemon (dashd equivalent). Working name `dashd-rs` (Q3) |
| **C** | The Rust CLI (dash-cli equivalent, plus the dash-tx / dash-util / dash-wallet / import tools). Working name `dash-cli-rs` (Q3). "C" also implies that stock `dash-cli` works against N |
| **D** | dashwallet-desktop (DashPay-style, mobile-inspired, macOS / Windows / Linux) |
| **D: pending decision** | Masternode, ProTx and governance desktop UI. These are out of desktop scope today (dashwallet-desktop `CLAUDE.md`), and pasta has been asked (Q1). Until pasta decides, the story is delivered on N + C, and "Done" does not require D |

**Priorities.** All three priorities are required for "Done" (charter Done-when 4 covers every story in this PRD). Priority only orders the work.

| Priority | Meaning |
|---|---|
| **P0** | Needed to run on a public network as a validating node or masternode without harming it, or needed to prove that. Blocks the first public-network milestone |
| **P1** | Needed for user-story parity with Dash Core and the mobile wallets |
| **P2** | Niche, debugging or rarely used. Scheduled last. Moving a P2 story out of scope needs pasta's approval (Q10) |

**rust-dashcore status** uses R1's terms: **present**, **partial**, **missing**, **wrong** (exists but diverges from Core), with file or report references. "(lib)" means the capability exists only as a library, with no node, RPC or CLI surface. Desktop status uses parity.md's terms (done / partial / not started / parked) with the QT-/IOS- item IDs.

**Requirement keywords.** MUST and SHOULD as in RFC 2119. In this document every MUST is release-blocking.

### 0.4 Verification methods

Acceptance criteria use these tags. "Reference dashd" means the pinned reference release for the network under test (Q5), run as BS2's oracles (`ORACLE.md`, `/work/chains/{mainnet,testnet}`) or from release tarballs on regtest and devnet, plus test-only hooks where needed (NFR-CEQ-04/06).

| Tag | Meaning |
|---|---|
| **VEC** | A Dash Core test-data file (`src/test/data/*.json`) or an oracle-generated vector file passes 100% |
| **DIFF-RPC** | The same RPC sequence is issued to reference dashd and to N in the same scenario. Responses are equal after normalization by the **allowed-differences registry** (below). Equality covers field names, field order, number formatting, error codes and error messages |
| **DIFF-STATE** | Consensus state agrees with reference dashd (R4 §0.2): verdict, validation-result class and reject string; UTXO set (MuHash and full `dumptxoutset`); the full deterministic masternode list (every field incl. last-paid height, PoSe, confirmed hash, consecutive payments); mined-commitment index and rotation snapshots; credit pool; MNHF signals and versionbits states; BLS-scheme state; block-index status flags incl. ChainLock conflicts. Compared through R4's per-block **StateDigest v1** (NFR-CEQ-06) |
| **DIFF-P2P** | A scripted peer talks to both nodes; the message sequences, payload bytes, penalties and timing envelopes are compared (§4.2) |
| **DIFF-FUZZ** | A differential fuzz target that drives the Rust code and Dash Core's C++ with the same inputs and finds no divergence within its CPU budget (NFR-CEQ-04) |
| **FT** | Dash Core functional tests pass with N substituted for dashd. **FT-MAP** is the mapping from each of the 264 functional tests to story IDs; the functional-test port task produces it. "FT-MAP set" means every test mapped to the story |
| **MIX** | A mixed network of dashd and N nodes (regtest via `DashTestFramework`, or a devnet), §4.1 NFR-CEQ-09 |
| **XCLI** | Rust CLI and stock dash-cli, each run against dashd and against N, print byte-identical stdout/stderr and exit codes, modulo the registry |
| **SPV** | rust-dashcore's dash-spv integration suites (`dashd_sync`, `dashd_masternode`) pass with N as the only peer |
| **DESK** | dashwallet-desktop tests: view-model and runtime tests, Linux CrossUI AT-SPI flows, and the regtest suites (`regtest/harness`), run with N as Core where the story touches Core |
| **LIVE** | Running on public testnet (and mainnet where stated), with the duration given |
| **BENCH** | The side-by-side benchmark harness (task B1), measured by the rules in NFR-PERF-02 |

**Allowed-differences registry** (`compat/ALLOWED-DIFFS.md`, created by the first DIFF-RPC task). Each entry names the RPC/field or P2P item, the reason, an owner and the review date. Entries are allowed only for:

1. implementation identity (version strings, when Q4 picks honest identity);
2. host-dependent values (memory statistics, uptime, timing and thread counts);
3. values that are random by design (nonces, session IDs, random tie-breaks that Core also randomizes).

Anything else is a defect. The goal is an empty registry outside these three classes.

### 0.5 Reconciliation with R4

R4 arrived after the first pass of this PRD and has been folded in. What it changed:

- **Reference releases per network** (§0.2, Q5) instead of one baseline, and the new `evo_shares` deployment next to V24 (FR-CON-02, FR-CON-23, R-1).
- **The verification plan follows R4's seven layers L0–L6** (§4.1 maps each NFR-CEQ obligation to its layer): oracle shim instead of libdashconsensus for fuzzing, StateDigest v1 compared at every block, segment replays for PR gating, the mutation engine for rejection parity, functional-test tiers, staged mixed networks, and the fingerprint lab with a held-out AUC ≤ 0.55 target.
- **Consensus details** that sharpen FR-CON: the full assumevalid skip set (F2), unbounded old-budget windows and governance-free replay (F3), the tolerance-only early difficulty (F4), disabled LLMQ types still in `merkleRootQuorums` (F5), unvalidated early payments (F6), the `hash_serialized_2` quirk (F7), the 39 mainnet checkpoints, and R4's 34-item quirk register (FR-CON-34).
- **EMBARGO-1**, an unfixed Dash Core security issue: referenced by codename only (banner above, R-0, Q13).
- **Sizes** in §6 use R4's difficulty scale.

Stories and priorities in §2 did not change because of R4.

---

## 1. Vision, goals, non-goals, personas

### 1.1 Vision

A Rust implementation of Dash Core that is **consensus-identical**, **network-indistinguishable** and **faster**: a full node, masternode and evonode that serves the network as well as dashd does, with a CLI that operators already know, and that can take over an existing Dash Core installation. It ships next to a DashPay-style desktop wallet built on the same Rust libraries. Together, the node, the CLI and the desktop app cover every user story that dashd, dash-cli and dash-qt serve today, plus the mobile wallets' stories on the desktop.

It is a **parallel product**, not a replacement of Dash Core: it has its own datadir, it imports Dash Core's wallets and block data, and it is compatible with dash-cli, the RPC/ZMQ/REST interfaces and Dash Core's functional tests wherever practical.

### 1.2 Goals

| ID | Goal | Measured by |
|---|---|---|
| G1 | **Consensus identity.** Bit-for-bit the same validity decisions and state as Dash Core on mainnet, testnet, devnets and regtest, before and after V24 | REL-1, REL-5 (NFR-CEQ) |
| G2 | **Network indistinguishability.** With N presenting dashd's user agent (Q4), a peer that probes it cannot tell it from dashd in practice, as a regular node or as a masternode | REL-2.6 (NFR-FP) |
| G3 | **Full masternode and evonode service.** DKG, signing, InstantSend, ChainLocks, EHF signals, CoinJoin servicing, governance duties and Platform's Core interface, in quorums shared with dashd | REL-2 |
| G4 | **Drop-in for operators and integrators.** `dash.conf`, RPC (289 commands classified in §5), ZMQ, REST and dash-cli workflows behave as on dashd | REL-3, §5 |
| G5 | **Performance.** Never slower than dashd on any tracked metric; 2× where the node's own work is the bottleneck | REL-6 (NFR-PERF) |
| G6 | **User-story parity across surfaces.** Every story in §2 is supported on its named surfaces | REL-4 |
| G7 | **Migration in both directions.** Import a Dash Core datadir, its wallets and settings; export back to Dash Core | US-NODE-18, US-IMP-* |
| G8 | **Coexistence.** rust-dashcore's existing consumers (Platform, mobile FFI, Dash Evo Tool, the desktop app) keep working; SPV stays first-class | NFR-CMP |
| G9 | **Provable.** Every claim above is backed by an automated, reproducible suite | REL-5 |

### 1.3 Non-goals

| ID | Non-goal | Why |
|---|---|---|
| NG-1 | Changing consensus, or "fixing" Core quirks that affect validity (floating-point subsidy, DGW off-by-one, BIP39 checksum quirk on import) | G1 |
| NG-2 | Bitcoin Core features that Dash never adopted: segwit, taproot, RBF and `bumpfee`, `feefilter`, wtxid relay, headers presync, `xor.dat` block obfuscation, mempool.dat v2, JSON-RPC 2.0 | R2 §G.2; adopting them would break G1/G2 |
| NG-3 | Reproducing dash-qt. The desktop app follows the mobile wallets (dashwallet-desktop `CLAUDE.md`) | Product scope |
| NG-4 | Writing Dash Core's on-disk formats (LevelDB layouts, evodb byte order, BDB wallets). Core formats are **read** for import; N writes its own formats (Q11) | R2 §G.3 Q2; evodb HAMT order is impractical to reproduce |
| NG-5 | New P2P protocols or services that dashd does not offer (for example a signed SPV bootstrap bundle) before Done | G2; may be revisited after 1.0 |
| NG-6 | Assumeutxo loading. Dash Core has `dumptxoutset` but no `loadtxoutset` and no evo payload in snapshots | R2 §D.1 US-NODE-16, R5 §3.5 |
| NG-7 | Multiprocess builds (`echoipc`), USDT tracing, the `dash-chainstate` kernel tool, and the Qt GUI options | Build-specific Core features with no user story |
| NG-8 | Making Dash Core consume Rust (dash#7109) or base-sdk | R3 §4.2 phase 4; owned by Core developers |
| NG-9 | Mobile apps, Dash Platform (Drive, Tenderdash, DAPI) and dashmate themselves. We provide what they need from Core | Scope |
| NG-10 | Desktop UI for node operation, an RPC console against remote nodes, and debug tools | dashwallet-desktop scope. MN/governance desktop UI is a separate pending decision (Q1) |

### 1.4 Surfaces

| Surface | What it is | Relation to Dash Core |
|---|---|---|
| **N: Rust node daemon** | Full validating node; optional masternode/evonode mode; built-in wallet; serves light clients; RPC/ZMQ/REST | dashd equivalent. Own datadir (Q3), same ports by default (Q3), reads `dash.conf` semantics. Imports a dashd datadir, wallets and settings |
| **C: Rust CLI** | RPC client with dash-cli's options and output, plus tool subcommands: dash-tx, dash-util, dash-wallet equivalents, datadir/wallet import and export, differential mode | dash-cli (and dash-tx, dash-util, dash-wallet) equivalent. Works against N and against dashd |
| **D: dashwallet-desktop** | DashPay wallet for macOS, Windows and Linux: Rust engine on `platform-wallet` (rust-dashcore underneath), Swift view models, SwiftUI (macOS) and SwiftCrossUI (Linux/Windows) | Mobile-inspired, not dash-qt. SPV by default; can link to a local N for full-node data (US-WAL-19) |
| Shared libraries | rust-dashcore crates (`dashcore`, `key-wallet`, `key-wallet-manager`, `dash-spv`, new node crates per A1), base-sdk | Library code that N, C and D share. The node wallet and D share key-wallet (R5 R-4) |

### 1.5 Personas

| Persona | Who | Top jobs | Today | Story areas |
|---|---|---|---|---|
| **Node operator** | Runs a full node for itself, a business or the network | Sync and stay synced, configure, monitor, survive crashes, manage peers, prune, run over Tor | dashd + dash-cli | US-NODE |
| **Masternode / evonode operator** (owner and operator roles) | Holds 1000 DASH (regular) or 4000 DASH (evonode) collateral; runs the server | Register, run, keep PoSe score at 0, get paid, update service and keys, take part in quorums; evonodes also serve Platform | dashd + dash-cli (+ dash-qt wizard, mobile key tools) | US-MN, US-PLAT-01, US-CJS |
| **Shared-masternode participant** (V24) | Co-owns a masternode collateral with 1–7 others (≥100 DASH each) | Co-fund, verify terms, receive share rewards, change reward address, rotate keys jointly, dissolve | dash-cli and dash-qt v24 only | US-SHR |
| **Wallet user** | Holds and spends DASH; on the desktop also uses DashPay | Create/restore, receive, send, InstantSend, back up, secure, history, multiwallet; DashPay identity, username, contacts | Mobile wallets, dash-qt, DET | US-WAL, US-DPY |
| **Governance voter and proposal author** | MN voting-key holders; proposal authors | Browse proposals, vote with all keys, author and submit proposals, track funding | dash-qt governance tab, dash-cli `gobject` | US-GOV |
| **CoinJoin participant (client)** | Wallet user mixing coins | Start/stop mixing, set rounds/amount, spend only mixed coins, keep mixed coins across wallets | dash-qt, dash-cli, Android | US-CJ |
| **CoinJoin servicing masternode** | Every masternode, automatically | Run mixing sessions correctly, charge collateral fairly, resist spam, relay DSTX/DSQ | dashd (automatic) | US-MN-13, US-CJS |
| **Miner / pool** | Pool software and solo miners | Correct templates with MN, superblock and Platform payouts; submit blocks; signal deployments | dashd GBT | US-MINE |
| **RPC/ZMQ/REST integrator** | Exchanges, explorers, payment processors; Platform (Tenderdash, Drive, DAPI) | Look up anything, index addresses, accept on InstantSend/ChainLock, stream events; Platform: quorums, signing, credits | dashd RPC/ZMQ/REST | US-INT, US-PLAT |
| **Light-client server** | Any full node serving SPV wallets | Serve BIP157/158 (dash-spv: iOS, DET, desktop), BIP37 bloom (dashj Android), headers2, mnlistdiff/qrinfo, CL/IS relay | dashd | US-LCS |
| **Importer / migrator** | Anyone moving from Dash Core or a mobile wallet | Bring the chain, wallets (BDB and SQLite), dumps, seeds, settings; go back if needed | dash-qt/dashd tools | US-IMP, US-NODE-18 |
| **Regtest/devnet developer** | Core, Platform, wallet and test developers | Regtest masternode networks, devnets, mocktime, sporks, functional tests, differential checks | dashd regtest, DashTestFramework, dashmate | US-DEV |

---

## 2. User-story catalog

### 2.0 Counts

168 stories. P0 = 76, P1 = 71, P2 = 21. By surface: 146 involve N, 107 involve C, 49 involve D, and 21 more carry "D: pending decision" (Q1).

| Area | Persona | P0 | P1 | P2 | Total | D pending decision |
|---|---|---|---|---|---|---|
| US-NODE | Node operator | 9 | 11 | 2 | 22 | 0 |
| US-WAL | Wallet user (Core wallet; node and desktop) | 9 | 10 | 3 | 22 | 0 |
| US-DPY | Wallet user (DashPay on the desktop) | 0 | 9 | 3 | 12 | 0 |
| US-MN | Masternode / evonode operator | 15 | 5 | 1 | 21 | 7 |
| US-SHR | Shared-masternode participant (V24) | 0 | 6 | 0 | 6 | 6 |
| US-GOV | Governance voter and proposal author | 5 | 5 | 1 | 11 | 7 |
| US-CJ | CoinJoin participant (client) | 0 | 6 | 3 | 9 | 0 |
| US-CJS | CoinJoin servicing masternode | 5 | 1 | 0 | 6 | 0 |
| US-MINE | Miner / pool | 4 | 1 | 0 | 5 | 0 |
| US-INT | RPC/ZMQ/REST integrator (exchanges, explorers) | 8 | 7 | 0 | 15 | 0 |
| US-PLAT | Platform integrator (Tenderdash, Drive, DAPI, evonodes) | 6 | 3 | 0 | 9 | 1 |
| US-LCS | Light-client server | 6 | 1 | 0 | 7 | 0 |
| US-IMP | Importer / migrator | 5 | 3 | 5 | 13 | 0 |
| US-DEV | Regtest / devnet developer | 4 | 3 | 3 | 10 | 0 |
| **Total** | | **76** | **71** | **21** | **168** | **21** |

Shared-masternode *validation* is P0 even though the US-SHR user flows are P1: the rules are consensus once V24 activates (FR-CON-25, FR-CON-26).

Each table has the same columns:

- **Surf**: surfaces (§0.3).
- **Pri**: priority (§0.3).
- **Dash Core**: the RPCs, options and subsystems that implement the story in Dash Core (R2 §D, R2b).
- **rust-dashcore status**: present / partial / missing / wrong, with references. "D:" gives the desktop status from parity.md.
- **Acceptance**: testable criteria using the tags in §0.4.

### 2.1 Node operator (US-NODE)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-NODE-01 | As a node operator I run a full validating node on mainnet, testnet, a named devnet or regtest. It syncs headers and blocks from genesis, with or without assumevalid, and follows the most-work valid chain subject to ChainLocks. | N, C | P0 | dashd; `-chain`/`-testnet`/`-devnet`/`-regtest`; validation, net_processing; `getblockchaininfo`, `getblockcount` | **missing**: no script interpreter, tx/block validation, DGW, UTXO set or chainstate; dash-spv is a header/filter SPV client (R1 §0, §2.1, §3.2) | DIFF-STATE full replay of mainnet and testnet from genesis to the release tip, with default assumevalid and with `-assumevalid=0`: zero divergence (REL-1). Live sync from public peers reaches the same tip as reference dashd. FT-MAP set for block validation and sync passes (incl. `feature_block`) |
| US-NODE-02 | I can tell when the node and its Dash layers (blockchain, sporks, governance, masternode list) are fully synced. | N, C | P0 | `mnsync status`, `getblockchaininfo.initialblockdownload/verificationprogress`; CMasternodeSync BLOCKCHAIN → GOVERNANCE → FINISHED | **missing** (dash-spv has its own progress model; no mnsync) | DIFF-RPC: `mnsync status` and `getblockchaininfo` go through the same transitions as dashd's on regtest and testnet; asset switches occur within one 6 s tick of dashd's in the same scenario; FT `feature_sporks` and the governance-sync tests pass |
| US-NODE-03 | I configure the node with `dash.conf` (network sections, `includeconf`), `settings.json` and command-line flags, using Dash Core's option names, defaults and parse rules. | N, C | P0 | ArgsManager; `-conf`, `-datadir`, `-includeconf`, `-settings`; about 250 options (R2 §A.6 6.2) | **missing**: dash-spv has clap flags only, no config file (R1 §2.6) | A config corpus (every option in R2 §A.6 6.2, every `dash.conf` that dashmate generates, and malformed files) yields the same observable behaviour and the same accept/reject outcome and message class as dashd. NETWORK_ONLY options in the default section are ignored with a warning on non-main networks. FT-MAP set (config, includeconf, `feature_settings`) passes |
| US-NODE-04 | I run the node as a daemon, stop it cleanly, and it survives crashes and power loss without a reindex. I can force `-reindex`, `-reindex-chainstate`, `-loadblock` and verify the chain. | N, C | P0 | `-daemon`, `-daemonwait`, `-pid`, `stop`, `uptime`, `-dbcache`, `-reindex*`, `verifychain`, `-checkblocks`, `-checklevel` | **missing** | Port of FT `feature_dbcrash` passes. A kill-9 harness (≥1,000 random kill points during IBD replay and at tip) never needs a reindex and never diverges (DIFF-STATE after restart). Clean `stop` completes no slower than dashd with the same `-dbcache` (BENCH). FT `feature_reindex` passes |
| US-NODE-05 | I prune block storage to save disk, accepting that txindex, addressindex, spentindex and governance are unavailable. | N, C | P1 | `-prune`, `pruneblockchain`; NODE_NETWORK_LIMITED | **missing** | FT `feature_pruning`, `feature_index_prune`, `feature_remove_pruned_files_on_startup` pass. The option-incompatibility errors equal dashd's. A pruned node keeps ≥288 blocks and advertises NODE_NETWORK_LIMITED only |
| US-NODE-06 | I manage peers: add, remove and disconnect them, ban and unban, and see peer details including masternode/MNAUTH status and connection types. | N, C | P0 | `addnode`, `disconnectnode`, `getaddednodeinfo`, `getpeerinfo`, `setban`, `listbanned`, `clearbanned`, `getconnectioncount`, `getnodeaddresses`, `ping`, `setnetworkactive` | **partial**: outbound peer manager, DNS seeds, addrv2 and reputation in dash-spv (`dash-spv/src/network/`, R1 §2.3); no inbound listener, no RPC | DIFF-RPC on regtest topologies (manual, inbound, outbound, block-relay, masternode connections). FT-MAP set passes (incl. `rpc_setban`, `rpc_mnauth`) |
| US-NODE-07 | I run over Tor, I2P or CJDNS, publish an onion service and restrict networks. | N, C | P1 | `-proxy`, `-onion`, `-listenonion`, `-torcontrol`, `-i2psam`, `-i2pacceptincoming`, `-onlynet`, `-cjdnsreachable` | **missing** | FT-MAP set passes. Against tor and i2pd in Docker: the onion service is published with an ED25519-V3 key that persists across restarts; the I2P SAM session is accepted; connection behaviour per network equals dashd's (DIFF-P2P) |
| US-NODE-08 | I control reachability and permissions: NAT-PMP/UPnP, `-externalip`, `-discover`, `-bind`, `-whitebind`, `-whitelist` permissions. | N, C | P1 | mapport, net_permissions | **missing** | FT-MAP set passes (bind, permissions). Each permission flag (bloomfilter, noban, forcerelay, relay, mempool, download, addr) has the same effect as on dashd (DIFF-P2P) |
| US-NODE-09 | I limit resources: upload target, connection count, mempool size, validation threads, RPC threads, database cache. | N, C | P1 | `-maxuploadtarget`, `-maxconnections`, `-maxmempool`, `-par`, `-parbls`, `-rpcthreads`, `-dbcache` | **missing** | Each limit holds under load tests and is reported identically (DIFF-RPC `getnettotals.uploadtarget`, `getmempoolinfo`). Masternode hard requirements (`maxconnections` ≥ 125, `maxuploadtarget` = 0) fail init with dashd's errors |
| US-NODE-10 | I inspect network and traffic statistics. | N, C | P1 | `getnetworkinfo`, `getnettotals`, `getmemoryinfo`, `dash-cli -netinfo`, `-getinfo`, `-addrinfo` | **missing** | DIFF-RPC shapes. XCLI: `-netinfo 0…4`, `-getinfo` and `-addrinfo` output identical against both daemons |
| US-NODE-11 | I get hooks on events: new tip, new ChainLock, alerts, startup and shutdown. | N | P1 | `-blocknotify`, `-chainlocknotify`, `-alertnotify`, `-startupnotify`, `-shutdownnotify` | **missing** | In a scripted regtest run, each hook fires on the same events, in the same order, with the same substitutions as dashd (event logs compared) |
| US-NODE-12 | I change logging categories at runtime and read `debug.log` with the same categories and line format as dashd, including Dash's categories. | N, C | P1 | `logging`, `debug`, `-debug`, `-debugexclude`, `-loglevel`, `-logtimestamps`, `-logips`, `-logthreadnames`, `-logsourcelocations`, `-shrinkdebugfile`, `-printtoconsole` | **partial**: `tracing` logs in dash-spv; no Core categories | Every Core category name (incl. chainlocks, coinjoin, gobject, instantsend, llmq, llmq-dkg, llmq-sigs, mnpayments, mnsync, spork, netconn, ehf, creditpool, dash) is accepted. DIFF-RPC `logging`. Line prefix format equals dashd's, and the operator-relevant lines listed in FR-CFG-07 (UpdateTip, ChainLock, mnsync transitions, masternode state) use dashd's wording |
| US-NODE-13 | I see the sporks and their effect. | N, C | P0 | `spork show`, `spork active`; spork.cpp | **missing**: no spork messages (R1 §2.3) | DIFF-RPC `spork show`/`spork active` equal dashd's at all times in MIX and on testnet. Mainnet hard-coded values apply (FR-SPK-01). FT `feature_sporks` |
| US-NODE-14 | I see active and upcoming consensus deployments, buried and BIP9/EHF. | N, C | P0 | `getdeploymentinfo`, `getblockchaininfo` | **missing**: only `v20_activation_height()` (R1 §2.1) | DIFF-RPC `getdeploymentinfo` at every period boundary during mainnet/testnet replay and on regtest with `-vbparams`/`-testactivationheight`, incl. the V24 and `evo_shares` EHF paths. FT `feature_mnehf`, `feature_new_quorum_type_activation` |
| US-NODE-15 | I export metrics to statsd. | N | P2 | `-statshost`, `-statsport`, `-statsprefix`, `-statssuffix`, `-statsperiod`, `-statsbatchsize`, `-statsduration`; `src/stats` | **missing** | FT `feature_stats`. Metric names, types and units match R2 §A.6 7 |
| US-NODE-16 | I dump the UTXO set at a height. | N, C | P2 | `dumptxoutset` (hidden); evo snapshot | **missing** | `dumptxoutset` output is byte-identical to dashd's at the same height (also used by REL-1). FT `rpc_dumptxoutset` |
| US-NODE-17 | I recover from forks: invalidate, reconsider or prefer a block, and see all chain tips, including ChainLock-conflicting forks. | N, C | P1 | `invalidateblock`, `reconsiderblock`, `preciousblock`, `getchaintips` | **missing**: dash-spv "reorg" truncates storage and restarts (R1 §3.2) | DIFF-RPC `getchaintips` incl. `conflicting` status. ChainLock-conflicting forks are marked as dashd marks them (FT `feature_llmq_chainlocks`, `feature_llmq_is_cl_conflicts`). FT-MAP set for invalidate/reconsider/precious passes |
| US-NODE-18 | I migrate a complete Dash Core installation (chain, wallets, config, peers and bans) into N with one command, without touching the original. | N, C | P0 | R2 §E import matrix | **missing** | End-to-end test on copies of a mainnet and a testnet dashd datadir: N reaches the tip without re-downloading blocks (US-IMP-02); wallet balances and `listtransactions` equal dashd's (DIFF-RPC); config (US-IMP-07) and bans carry over; the source datadir hashes the same before and after |
| US-NODE-19 | I run N next to a dashd on the same host without either touching the other's files. | N, C | P0 | separate datadir, ports, `.lock` | n/a (new) | N and dashd run side by side (N on non-default ports) through a full regtest scenario. N refuses to open a datadir that contains a Dash Core layout for writing, and the error names the import command |
| US-NODE-20 | I upgrade N across releases without a resync, and a downgrade is refused safely. | N | P1 | Core's DB upgrade paths | n/a (new) | Each release opens the datadirs of the previous two releases and migrates in place (CI on a testnet datadir snapshot); opening a newer datadir fails with a clear error and no writes |
| US-NODE-21 | I install N from signed packages on supported platforms and run it under the OS service manager with dashd's signal semantics. | N, C | P1 | release packaging; signals (SIGTERM, SIGINT, SIGHUP reopens `debug.log`) | **missing** | Signed artifacts for the tier-1 platforms (Q8) pass an install smoke test; systemd units (`-daemonwait` or notify) and launchd plists work; signal behaviour equals dashd's |
| US-NODE-22 | I am alerted at once if my node disagrees with the ChainLocked chain, so a consensus bug cannot silently harm me or my quorums. | N | P1 | not in Core (safety net) | n/a (new) | Fault injection on regtest (a patched validation rule): N raises a critical warning (`getblockchaininfo.warnings`, `-alertnotify`), writes a reproduction bundle (block, undo, state digests), and in masternode mode stops signing until the operator acts. Peers see no other difference |

### 2.2 Wallet user (US-WAL)

The node wallet (N, C) and the desktop wallet (D) share key-wallet underneath (FR-WAL-01). "D" acceptance runs in DESK.

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-WAL-01 | I create, load, unload, list, back up and restore wallets, several at once. | N, C, D | P0 | `createwallet`, `loadwallet`, `unloadwallet`, `listwallets`, `listwalletdir`, `backupwallet`, `restorewallet`, `-wallet`, `-walletdir`, load-on-startup in `settings.json` | **partial** (lib): `WalletManager` multiwallet (`key-wallet-manager`); no persistence or encryption of its own (R1 §2.5). D: QT-101 done, QT-110 partial | N: DIFF-RPC over the wallet-lifecycle FT-MAP set (incl. `wallet_backup`); `createwallet`'s positional quirk (descriptors at position 5) reproduced. D: create/load/close/switch DESK flows |
| US-WAL-02 | I get an HD wallet from a BIP39 mnemonic (12–24 words) with an optional passphrase, on Core's paths, and can view and back up the mnemonic. Phrases that dashd accepts work here too. | N, C, D | P0 | `createwallet`, `upgradetohd`, `dumphdinfo`, `listdescriptors true`, `-mnemonic`, `-mnemonicpassphrase`, `-mnemonicbits`, `-usehd`, `sethdseed` | **partial**: mnemonic support in `key-wallet/src/mnemonic.rs`, but `Wallet::from_mnemonic` hard-codes an empty passphrase (R5 US-W-03). Core quirks (weak checksum, no NFKD, salt cut at 256 bytes) exist only in dashwallet-desktop `dw-compat::bip39core`. D: QT-103/104 done | VEC `bip39_vectors.json` plus the dw-compat quirk vectors. For every phrase+passphrase in a dashd-generated corpus, N and D accept it and derive the same first 1,000 external and internal addresses on BIP44 and DIP9 CoinJoin paths. FT `wallet_hd`, `wallet_mnemonicbits`, `wallet_upgradetohd` |
| US-WAL-03 | I receive: new addresses, labels, address info, keypool, payment requests. | N, C, D | P0 | `getnewaddress`, `getrawchangeaddress`, `setlabel`, `getaddressinfo`, `getaddressesbylabel`, `listlabels`, `keypoolrefill`, `newkeypool`; request records (`destdata rr`) | **present** (lib): address pools with gap limit and labels (`key-wallet/src/managed_account/address_pool.rs`); no RPC. D: QT-081…085 done | DIFF-RPC: from the same seed, N hands out the same address sequence as dashd for both wallet kinds (Q2), with keypool-1000 semantics; `getaddressinfo` fields equal |
| US-WAL-04 | I see balances (trusted, pending, immature, CoinJoin, InstantSend-locked) and full history with InstantSend/ChainLock status. | N, C, D | P0 | `getbalance`, `getbalances`, `getunconfirmedbalance`, `listtransactions`, `listsinceblock`, `gettransaction`, `listreceivedby*`, `getreceivedby*`, `listaddressbalances`, `listaddressgroupings` | **partial** (lib): balances and IS/CL-aware records (`key-wallet/src/wallet/balance.rs`); chain-locked records dropped unless `keep-finalized-transactions`; no reorg rollback (#854). D: QT-034, QT-087, QT-089 done; QT-086, QT-088 partial | DIFF-RPC on scripted regtest histories with IS-locked, chain-locked, conflicted, abandoned, immature-coinbase and CoinJoin transactions and reorgs: every balance and every field of `listtransactions`/`gettransaction` equal. D shows the same history as N for the same seed |
| US-WAL-05 | I send DASH with automatic fees, coin control, InstantSend, subtract-fee and send-all, to one or many recipients. | N, C, D | P0 | `sendtoaddress`, `sendmany`, `send`, `sendall`, `fundrawtransaction`, `settxfee`, `-paytxfee`, `-mintxfee`, `-fallbackfee`, `-maxtxfee`, `lockunspent`, `listunspent`, `listlockunspent` | **partial** (lib): `TransactionBuilder`, BnB/LargestFirst selection, OP_RETURN; P2PKH-only signing; `FeeRate::economy()` is 500 duff/kB, below Dash's 1000 minimum relay fee (R5 US-W-08). D: QT-052, 054–056, 059, 060, 062, 063 done; QT-053, 057, 058, 061, 067 partial; QT-064…066 not started | For the same UTXO set and request: N's transaction is valid and standard; fee rate equals dashd's for the same size; error codes and messages equal for every failure; the on-chain shape matches dashd's wallet (FR-WAL-05: version, locktime, sequence, BIP69 order). D: send flows in DESK against N |
| US-WAL-06 | I encrypt, lock and unlock my wallet, change the passphrase, and unlock for mixing only. | N, C, D | P0 | `encryptwallet`, `walletpassphrase` (mixingonly), `walletlock`, `walletpassphrasechange` | **missing** (lib): no encryption at rest; `Wallet::backup()` writes plaintext bincode with the mnemonic (R1 §2.5, R5 US-W-05). D: QT-111 done (dw-vault), QT-112 partial | DIFF-RPC lock-state transitions and errors (-13, -14, -15, -17); unlock timeout semantics equal. A disk scan after every step finds no plaintext secret. D: vault tests and DESK flows |
| US-WAL-07 | I import and export keys, watch-only addresses and pubkeys, descriptors and dump files. | N, C, D | P1 | `importprivkey`, `importpubkey`, `importaddress`, `importmulti`, `importdescriptors`, `listdescriptors`, `dumpprivkey`, `dumpwallet`, `importwallet`, `importprunedfunds`, `removeprunedfunds` | **missing**: no imported-key account, no descriptor engine (R5 G-3, US-PU-05). D: QT-107/108 partial (dw-compat) | DIFF-RPC for each import RPC on both wallet kinds, incl. rescan from a timestamp. `dumpwallet` output equals dashd's byte for byte, except timestamps. Dumps round-trip N → dashd → N |
| US-WAL-08 | I rescan, abort a rescan, abandon a stuck transaction and wipe transaction history. | N, C, D | P1 | `rescanblockchain`, `abortrescan`, `abandontransaction`, `wipewallettxes` | **partial**: abandon in key-wallet-manager; SPV rescan from birth height; no rescan-from-height API (R5 US-W-15). D: QT-117 done | DIFF-RPC; FT `rpc_wipewallettxes`; rescan of a ≥100k-tx wallet is no slower than dashd (BENCH) |
| US-WAL-09 | I use multisig, PSBT and external signers. | N, C, D | P1 | `addmultisigaddress`, `createmultisig`, the PSBT RPCs, `enumeratesigners`, `walletdisplayaddress`, `-signer` | **missing**: P2PKH-only signing, no PSBT (R1 §2.5). D: QT-076…079 partial (dw-psbt); QT-080 not started | PSBTs pass between N and dashd at every step (create, update, sign, combine, finalize) and the final transactions are identical. Multisig addresses and redeem scripts equal. FT-MAP PSBT and multisig sets |
| US-WAL-10 | I sign and verify messages. | N, C, D | P1 | `signmessage`, `verifymessage`, `signmessagewithprivkey` | **partial**: `dash/src/sign_message.rs` primitives, no wallet API (R5 US-W-13). D: QT-099/100 done | Signatures verify in both directions between N, D and dashd; VEC from Core's message-signing tests |
| US-WAL-11 | I upgrade or migrate legacy wallets and use an offline wallet tool. | N, C | P0 | `upgradewallet`, `migratewallet`, `upgradetohd`; `dash-wallet create/info/dump/createfromdump/salvage` | **missing** | C's dash-wallet equivalent prints the same `info` and writes the same dump files as `dash-wallet` for the same wallet. Legacy-to-descriptor migration gives the same descriptors and `next_index` as dashd's `migratewallet`. FT `tool_wallet`, `wallet_migration` |
| US-WAL-12 | I get notified about wallet transactions by command hook and ZMQ. | N | P1 | `-walletnotify`, `-instantsendnotify`, ZMQ | **missing** | In a scripted regtest run, hooks fire on the same events with the same substitutions (`%s %w %b %h`) as dashd; ZMQ as US-INT-05 |
| US-WAL-13 | I simulate a transaction's balance effect before signing. | N, C | P2 | `simulaterawtransaction` | **missing** | DIFF-RPC |
| US-WAL-14 | I set wallet flags such as avoid_reuse. | N, C | P2 | `setwalletflag` | **missing** | DIFF-RPC, incl. avoid_reuse's effect on coin selection and balances |
| US-WAL-15 | I sweep a paper wallet or import a WIF key and see its funds. | N, C, D | P1 | `importprivkey` + rescan; `scantxoutset` | **missing**: BIP38 removed (#1111); no imported-key account (R5 US-W-14). D: IOS-056 not started ("decide") | `scantxoutset` results equal dashd's. D: on regtest with N as the peer, sweeping a funded WIF moves all its coins to the wallet in one transaction |
| US-WAL-16 | On the desktop I protect the app with a passphrase, PIN, biometrics, auto-lock and spending confirmation, like the mobile apps. | D | P0 | not in Core (iOS) | n/a. D: IOS-012, 014, 015 done; IOS-010, 011, 013, 016, 017 partial | DESK: lockout policy, auto-lock timer, biometric quick-unlock and spending-limit tests pass; the vault has an independent security review (REL-4.4) |
| US-WAL-17 | On the desktop I see sync progress and peers, and choose the network and peers. | D | P0 | (SPV status) | **present** (lib): dash-spv progress, peer events, explicit peers (R5 US-W-21). D: QT-025/027 done, QT-024 and IOS-023 partial | DESK with N as the only peer and with public peers: progress phases complete, peer list correct |
| US-WAL-18 | On the desktop I keep transaction metadata (memos, tax categories) and export CSV and tax reports. | D | P1 | dash-qt CSV export; iOS tax features | n/a (app data). D: QT-093 done; IOS-036…040 not started | DESK: CSV columns equal dash-qt's (QT-093) and iOS's (IOS-039) |
| US-WAL-19 | On the desktop I link my local Rust node to get full-node data (smart fees, mempool and credit-pool statistics, masternode details) and to use it as a trusted peer. | D, N | P1 | dashwallet-desktop "NodeLink" (DESIGN-fable D8; DESIGN-opus §1.14) | n/a (new). D: not started | With a local N configured by cookie or rpcauth, D shows the full-node values and they equal N's RPC answers. Without the link, D shows "requires full-node data source" and never invents values. D syncs using N as its only peer |
| US-WAL-20 | I pay `dash:` URIs and QR codes (BIP21) and BIP70/72 payment requests. | D | P1 | dash-qt URI handling (QT-149/150); iOS BIP70 (IOS-049) | `Address::to_qr_uri` only (R5 US-W-06). D: QT-149 done, QT-150 partial, IOS-048 partial, IOS-049 not started | DESK: URI parsing vectors equal dash-qt's parser; BIP70 against a test merchant server |
| US-WAL-21 | On the desktop I use the mobile wallets' third-party services: fiat rates, buy/sell, Explore merchants and ATMs, gift cards, CrowdNode, swaps. | D | P2 | iOS integrations | n/a. D: IOS-087…103 not started. Credentials and the Explore database need DCG decisions (HANDOFF Q4/Q5, Q12) | DESK per IOS item |
| US-WAL-22 | On the desktop every item of the dash-qt and iOS parity checklists that is in scope works. | D | P1 | parity.md (QT-001…154, IOS-001…123) | D: QT 51 done / 63 partial / 22 not started / 18 parked; IOS 12 / 48 / 60 / 3 | Every non-parked parity.md item is "done" with a test reference (REL-4.3). Parked items follow Q1 |

### 2.3 DashPay desktop user (US-DPY)

These stories are the desktop's main purpose (dashwallet-desktop `CLAUDE.md`). Core-side needs are asset locks and unlocks, InstantSend/ChainLock proofs and the credit pool, all covered elsewhere. Detailed acceptance lives in the parity.md IOS items and the forthcoming `docs/design/DASHPAY.md` (task D3).

| ID | Story | Surf | Pri | Dash Core / Platform | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-DPY-01 | I create an identity and a DPNS username funded from my Core balance (asset lock with an InstantSend or ChainLock proof); the flow survives interruption; restoring my seed finds my identities. | D | P1 | AssetLockTx (type 8); Platform identity create and DPNS | **present** (lib): `asset_lock_builder.rs` (R5 US-PL-01); identity-auth keys are a path helper only (R5 US-PL-02). D: IOS-065…067, 069…071 not started | DESK on a local dashmate devnet: create, kill the app mid-flow, resume; restore from seed finds the identity. The asset lock is accepted by N and dashd (MIX) |
| US-DPY-02 | I follow a contested username through its voting period and see the request status. | D | P1 | Platform DPNS contests | D: IOS-068 not started | DESK on devnet |
| US-DPY-03 | I add contacts by username or QR, accept or ignore requests, hide contacts and keep an alias and note. | D | P1 | DIP-15 contact requests | **present** (lib): DIP-15 accounts (R5 US-PL-03). D: IOS-072…074 not started | DESK: two desktop instances establish a contact on devnet |
| US-DPY-04 | I pay a contact. | D | P1 | DIP-15 payment accounts | **present** (lib). D: IOS-050 not started | DESK: the payment arrives on the contact's DIP-15 account and both histories show the contact |
| US-DPY-05 | I edit my profile (display name, about, avatar). | D | P1 (avatar upload P2) | Platform profile documents | D: IOS-076 not started | DESK |
| US-DPY-06 | I get DashPay notifications (requests, accepts, payments). | D | P1 | — | D: IOS-075 not started | DESK |
| US-DPY-07 | I claim invitations; creating, sharing and reclaiming them depends on Q12. | D | P1 (create: P2) | Invitation funding (asset lock) | D: IOS-077 partial, IOS-078 not started ("decide") | DESK on devnet |
| US-DPY-08 | I top up identity credits from Core, withdraw credits to Core and transfer them to a Platform address. | D | P1 | AssetLock/AssetUnlock; credit pool | **partial**: AssetUnlock decode only; v2 missing. D: IOS-069/070 not started | DESK on devnet: the withdrawal is mined as an AssetUnlock that N and dashd both accept (DIFF-STATE) |
| US-DPY-09 | I use Platform payment addresses (DIP-17) and internal transfers between Core and Platform balances. | D | P1 | DIP-17 | **present** (lib): `PlatformPayment` accounts (R5 US-PL-04). D: IOS-062…064 not started | DESK |
| US-DPY-10 | I use the shielded pool. | D | P2 | Platform shielded pool | D: IOS-059…061 not started; timing is Q12 | DESK |
| US-DPY-11 | I use the username marketplace, DashConnect and tokens. | D | P2 | Platform | D: IOS-084…086 partial/not started | DESK |
| US-DPY-12 | As a masternode voting-key holder I vote on contested usernames (Platform DPNS votes, not Core governance). | D | P2 | Platform `MasternodeVoteTransition` | **present** (plat) (R5 US-GV-05). D: IOS-079 not started | DESK on devnet |

### 2.4 Masternode and evonode operator (US-MN)

Evonode registration and the Platform-facing duties are in US-PLAT-01 and US-PLAT-02…09. The CoinJoin server's protocol behaviour is in US-CJS.

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-MN-01 | I find wallet outputs that can serve as 1000-DASH (regular) or 4000-DASH (evonode) collateral. | N, C · D: pending decision | P0 | `masternode outputs`; CoinType ONLY_MASTERNODE_COLLATERAL | **missing** (no RPC); collateral auto-lock only on an unmerged branch (R5 US-MO-07) | DIFF-RPC on wallets holding locked and unlocked 1000/4000-DASH outputs |
| US-MN-02 | I generate operator BLS keys, or derive them from my seed. | N, C, D | P0 | `bls generate`, `bls fromsecret`; DIP-3 path `m/9'/c'/3'/3'` | **partial**: HD derivation (`key-wallet/src/account/bls_account.rs`); random keygen and `fromsecret` not exposed (R5 US-OP-01). D: IOS-083 partial (keychain viewer; in desktop scope) | `bls fromsecret` equals dashd's for 1,000 random secrets; keys from `bls generate` are accepted by dashd in ProRegTx/ProUpRegTx |
| US-MN-03 | I register a regular masternode with collateral in my wallet (`register`), funded in the same transaction (`register_fund`), or held externally or on a hardware wallet (`register_prepare` → sign the message → `register_submit`). | N, C · D: pending decision | P0 | `protx register`, `register_fund`, `register_prepare`, `register_submit`; `signmessage` | **partial**: payload types and the `inputs_hash` finalizer exist; no ProRegTx flow, no BLS signing helper (R1 §2.5, R5 US-MO-02/03). D: QT-123 parked | FT `feature_dip3_deterministicmns`, `wallet_dash_rpcs` pass. In MIX, ProRegTxs built by N are accepted by dashd and vice versa. The external-collateral flow works with the message signed outside N |
| US-MN-04 | I run the masternode: configure the operator key and external address, reach READY, authenticate to peers (MNAUTH) and serve the network. | N | P0 | `-masternodeblsprivkey`, `-externalip`; CActiveMasternodeManager; `masternode status` | **missing** (R1 §2.2, §8.5) | On regtest, devnet and testnet N reaches READY through the same state sequence as dashd (DIFF-RPC `masternode status`); dashd peers accept its MNAUTH; parameter interactions and hard-requirement errors equal dashd's (FT `feature_masternode_params`, `feature_masternode_params_devnet`) |
| US-MN-05 | As operator I update the service (IP/port; after V24, extended addresses), which also revives a PoSe-banned node. | N, C · D: pending decision | P0 | `protx update_service` (`update_service_evo` in US-PLAT-01) | **partial**: payload exists; BLS signing missing (TODO at `provider_update_service.rs:309`); v3 ExtNetInfo payload missing (R5 US-OP-02) | FT `rpc_netinfo`, `feature_protx_version`. In MIX, a ProUpServTx from N revives a banned MN exactly as one from dashd does |
| US-MN-06 | As owner I change the operator key, voting key or payout, including V24 multi-payouts. | N, C · D: pending decision | P0 | `protx update_registrar` | **partial**: payload; no builder (R5 US-MO-04) | FT `feature_masternode_payout_shares`. An operator-key change bans the MN until a ProUpServTx, as on dashd (DIFF-STATE) |
| US-MN-07 | As operator I revoke my operator role with a reason. | N, C · D: pending decision | P1 | `protx revoke` | **partial**: payload; BLS signing not wired (R5 US-MO-05) | FT revoke paths in `feature_dip3_deterministicmns` |
| US-MN-08 | I get paid: my masternode enters the payment queue and receives its reward, split with the operator and across payout addresses. | N | P0 | DML payee selection; `masternode payments`, `masternode winners`, `protx info` | **missing**: no deterministic MN list, only the simplified list (R1 §2.2) | DIFF-STATE: the expected coinbase outputs equal dashd's at every block of mainnet/testnet replay. DIFF-RPC `masternode winners`/`payments` |
| US-MN-09 | I monitor my masternode's status and PoSe score, and see bans and revivals. | N, C · D: pending decision | P0 | `protx info`, `protx list`, `masternode list`, `masternodelist`, `masternode count` | **missing**: SML has no PoSe score (R5 US-MO-06) | DIFF-RPC across replay snapshots; FT `feature_llmq_simplepose` |
| US-MN-10 | I inspect the masternode list at any height and diffs between heights. | N, C | P1 | `protx diff`, `protx listdiff`, `masternode list <mode>` | **partial**: SML engine inside dash-spv (`dash-spv/src/sml_engine.rs`, private) | DIFF-RPC for random height pairs across mainnet/testnet history |
| US-MN-11 | My masternode takes part in DKG for every LLMQ type it is selected for, and I can debug DKG. | N, C | P0 | DKG; `quorum dkgstatus`, `dkginfo`, `memberof`, `list`, `info` | **missing**: no DKG messages or sessions (R1 §2.2) | In MIX, Rust MNs complete DKGs with dashd MNs for every LLMQ type on regtest and devnet, rotation included. FT-MAP `feature_llmq_*` set passes (rotation, dkgerrors, dkg_intake, connections, simplepose, singlenode, evo, data_recovery). LIVE testnet: zero PoSe penalties attributable to N (REL-2.2) |
| US-MN-12 | My masternode serves quorum data recovery to peers and recovers its own shares. | N | P1 | `-llmq-data-recovery`, `-llmq-qvvec-sync`, `quorum getdata`; qgetdata/qdata | **missing** | FT `feature_llmq_data_recovery`, `p2p_quorum_data` |
| US-MN-13 | My masternode acts as a CoinJoin mixing server automatically, with no extra configuration. | N | P0 | CCoinJoinServer (masternode mode only) | **missing**: no CoinJoin messages beyond `senddsq` (R1 §2.3) | In MIX, a Rust MN completes mixing sessions with dashd clients and Rust clients. Protocol details: US-CJS |
| US-MN-14 | My masternode signs ChainLocks and InstantSend locks as a quorum member. | N | P0 | ChainLock and InstantSend signers; signature shares | **missing** (verification only, R1 §2.2) | In MIX, CLSIGs and ISDLOCKs recover from mixed quorums; shares from N are used by dashd recovery members and vice versa. Latency per NFR-PERF (BENCH) |
| US-MN-15 | My masternode carries out operator-key governance duties: creating, signing and voting superblock triggers. | N | P0 | GovernanceSigner | **missing** | Detailed in US-GOV-07; MIX acceptance there |
| US-MN-16 | I can still use legacy-BLS registrations (deprecated). | N, C | P2 | `protx *_legacy` with `-deprecatedrpc=legacy_mn` | n/a | FT `feature_protx_version` legacy paths pass; after V24 they are rejected with `bad-protx-version-disallowed`, as on dashd |
| US-MN-17 | As an owner I watch my masternodes from a wallet that is not the masternode (owner, voting, payout or collateral keys present). | N, C · D: pending decision | P1 | `protx list wallet`, `protx info` (wallet section) | **partial**: the wallet recognizes ProTxs that reference its keys (`key-wallet/src/transaction_checking/account_checker.rs:501-525`) | DIFF-RPC `protx list wallet` and the wallet section of `protx info` for wallets holding each key type. D: IOS-080…082 parked |
| US-MN-18 | My collateral is locked automatically so I cannot spend it by accident. | N, C, D | P0 | collateral auto-lock at startup | **partial**: unmerged branch `upstream/backport/key-wallet-collateral-lock-on-719de34b`; PR #1078 | `listlockunspent` equals dashd's for wallets owning collaterals; coin selection never spends a collateral unless explicitly unlocked |
| US-MN-19 | My masternode signs EHF (MNHF) signals as a quorum member, so hard forks such as V24 can activate. | N | P0 | `llmq/ehf_signals.cpp`; MnHfTx (type 7) | **missing** (payload parse only) | FT `feature_mnehf`. On regtest, V24 activates through signals from a Rust-majority quorum |
| US-MN-20 | I replace a running dashd masternode with N (same IP, same operator key) without losing payments or taking PoSe penalties, and I can switch back. | N, C | P0 | — | n/a (new) | LIVE testnet: a dashd MN swapped to N keeps PoSe score 0 across ≥3 DKG cycles of each LLMQ type it is selected for; swapping back works the same way |
| US-MN-21 | I derive and export my masternode keys (owner, voting, operator BLS, Platform Ed25519) from my wallet seed, compatible with the mobile key tools. | N, C, D | P1 | DIP-3 provider paths `m/9'/c'/3'/{1,2,3,4}'` | **present** (lib): `AccountType::Provider*` (R5 US-MO-01, US-EV-02). D: IOS-083 partial | For seeds from the Android/iOS key tools, N and D derive the same keys, addresses and node IDs (vectors per R5 §3.1) |

### 2.5 Shared-masternode participant, V24 (US-SHR)

The shared-masternode rules are consensus once V24 is active; validating them is covered by FR-CON-25/26 and is P0. These stories are the user flows.

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-SHR-01 | Several of us co-fund one masternode collateral (≥100 DASH per share), each with an immutable refund script, an owner key and an optional reward script. | N, C · D: pending decision | P1 | `protx shared_register_prepare`, `shared_sign`, `shared_combine`; `signrawtransactionwithwallet`, `sendrawtransaction` | **missing**: no ProRegTx v3, share table or consent hash; the decoder rejects types 10–12 (R5 US-SM-01, R1 §2.1) | FT `feature_masternode_shares` passes. A shared ProRegTx built by N is accepted by dashd; consent hashes equal dashd's for identical inputs (VEC from the oracle) |
| US-SHR-02 | Before signing my funding inputs I verify my share's terms and see warnings. | N, C · D: pending decision | P1 | `protx shared_sign` (`terms`, `warning`); `decoderawtransaction` (`proRegTx` JSON) | **missing** | DIFF-RPC for `shared_sign` terms and warnings and the decoded `proRegTx` JSON |
| US-SHR-03 | I receive my share of each reward at my reward script and can change that script. | N, C · D: pending decision | P1 | `protx shared_update_share` (ProUpShareTx, type 11); `SplitAmountByShares` | **missing** | DIFF-STATE: the reward split at every payout block equals dashd's; FT `feature_masternode_shares` share-update paths |
| US-SHR-04 | Together we change the operator or voting key. | N, C · D: pending decision | P1 | `protx shared_update_registrar_prepare` → `shared_sign` → `shared_combine` (ProUpSharedRegTx, type 12) | **missing** | FT paths pass; MIX acceptance by dashd |
| US-SHR-05 | I leave: a unanimous dissolution without penalty, or a unilateral one with the early-period penalty spread pro rata; I can pre-sign a standby dissolution and broadcast it later. | N, C · D: pending decision | P1 | `protx shared_dissolve_prepare`, `protx shared_dissolve` (ProDisTx, type 10; fee ≤ 1,000,000 duffs; early period ≤ 420480 blocks) | **missing** | FT dissolution paths incl. penalty floors and the fee cap; a pre-signed dissolution broadcast 1,000 blocks later is accepted by N and dashd |
| US-SHR-06 | I see the share table, terms and reward history of the shared masternodes I take part in. | N, C · D: pending decision | P1 | `protx info` (share table JSON), `protx list wallet` | **missing** | DIFF-RPC |

### 2.6 Governance voter and proposal author (US-GOV)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-GOV-01 | I browse proposals and triggers with their tallies, and see governance parameters and the next superblock's budget. | N, C · D: pending decision | P0 | `gobject list/diff/get/count/getcurrentvotes`, `getgovernanceinfo`, `getsuperblockbudget` | **missing** in rust-dashcore; base-sdk `dash_primitives::gov` has object/vote types (R3 §1.2); desktop parked branch `m3/r2-governance`. D: QT-026, QT-128…134 parked | After governance sync on mainnet and testnet, the object set, tallies and cached flags equal dashd's (DIFF-RPC). FT `feature_governance`, `feature_governance_objects` |
| US-GOV-02 | I write a proposal and check it before paying. | N, C · D: pending decision | P1 | `gobject check`, `gobject deserialize` | **missing** | VEC `proposals_valid.json`, `proposals_invalid.json`; error strings equal dashd's |
| US-GOV-03 | I pay the 1-DASH proposal collateral (OP_RETURN of the object hash), submit after 6 confirmations, and resume if interrupted. | N, C · D: pending decision | P0 | `gobject prepare`, `gobject list-prepared`, `gobject submit`; wallet record `g_object` | **missing** | FT `wallet_dash_rpcs` governance paths. In MIX, a proposal submitted via N appears in dashd's `gobject list` with the same hash; `list-prepared` survives a restart |
| US-GOV-04 | I vote (funding/valid/delete/endorsed × yes/no/abstain) with one or all of my masternodes' voting keys. | N, C · D: pending decision | P0 | `gobject vote-many`, `gobject vote-alias` | **missing** (R5 US-GV-02) | In MIX, votes cast via N are counted by dashd. Signature formats per network (mainnet signmessage; testnet hash-sign) match oracle vectors |
| US-GOV-05 | I vote with a signature produced elsewhere (hardware or offline voting key). | N, C | P1 | `voteraw` | **missing** | DIFF-RPC; a vote signed offline is accepted by N and dashd |
| US-GOV-06 | I get notified of new governance objects and votes. | N | P1 | ZMQ `pubhashgovernanceobject`, `pubhashgovernancevote`, `pubrawgovernanceobject`, `pubrawgovernancevote` | **missing** | ZMQ harness of US-INT-05 |
| US-GOV-07 | As a masternode my node creates, signs and votes superblock triggers, and as a miner my node includes superblock payments. | N | P0 | GovernanceSigner, SuperblockManager; GBT `superblock[]` | **missing** | MIX on regtest: Rust-only, dashd-only and mixed MN sets produce the same trigger JSON (same data hash) and the same superblock outputs. DIFF-STATE: superblock validity agrees at every superblock height of the replay |
| US-GOV-08 | I vote with an imported voting key (WIF) when the masternode's seed belongs to someone else. | N, C · D: pending decision | P1 | key import + `gobject vote-alias` | **missing**: no imported-key account (R5 US-GV-04) | A vote with an imported voting key is accepted by dashd (MIX) |
| US-GOV-09 | I follow my proposal through voting to payment. | N, C · D: pending decision | P1 | `gobject get`, `getsuperblockbudget`, `gobject list` | **missing** | DIFF-RPC across a regtest superblock cycle, up to the payment in the superblock |
| US-GOV-10 | My node syncs, validates and relays governance objects and votes exactly as dashd does, so that its superblock validation agrees with the network. | N | P0 | NetGovernance, CGovernanceManager; `govsync`, `govobj`, `govobjvote`, `ssc` | **missing**: no governance messages (R1 §2.3) | DIFF-P2P for govsync, ssc and inv behaviour incl. penalties. After syncing from mixed peers, the object and vote sets equal dashd's (DIFF-RPC). FT `p2p_govsync_bloom`, `feature_governance_cl` |
| US-GOV-11 | I am reminded of vote deadlines and the next superblock. | D: pending decision | P2 | dash-qt governance clock (QT-026) | n/a. D: QT-026 parked | DESK |

### 2.7 CoinJoin participant, client (US-CJ)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-CJ-01 | I start, stop and reset mixing in a wallet and see progress and sessions. | N, C, D | P1 | `coinjoin start/stop/reset/status`, `getcoinjoininfo` | **missing** in rust-dashcore (R5 US-CJ-01). D: CoinJoin client merged in desktop M3 per HANDOFF; parity.md (measured at M2) still shows QT-041…050 not started; the M3 CoinJoin regtest suite must be re-run | In MIX, N's wallet mixes to the configured rounds with dashd and Rust MNs. DIFF-RPC `getcoinjoininfo` and `coinjoin status` fields. FT `rpc_coinjoin`. D: CoinJoin DESK suite passes with Rust MNs as the servers |
| US-CJ-02 | I set rounds, target amount, sessions, multisession and denomination goals. | N, C, D | P1 | `setcoinjoinrounds`, `setcoinjoinamount`, `-coinjoin*` options | **missing** | Option clamping and errors equal dashd's (e.g. `-coinjoindenomshardcap` below the goal fails init) |
| US-CJ-03 | I spend only fully mixed coins and see my mixed balance. | N, C, D | P1 | coin type ONLY_FULLY_MIXED (`use_cj`), `getbalances.mine.coinjoin`, `listunspent` rounds | **partial** (lib): the CoinJoin account is excluded from default sends; no rounds computation or salt (R5 US-CJ-02/03) | For a corpus of dashd-mixed wallets, per-outpoint rounds and "fully mixed" (salt coin-flip included) equal dashd's (oracle VEC); fully-mixed sends have no change output |
| US-CJ-04 | I manage my wallet's CoinJoin salt. | N, C | P2 | `coinjoinsalt generate/get/set` | **missing** | DIFF-RPC incl. the guards (mixing in progress, CoinJoin balance, private keys disabled) |
| US-CJ-05 | My keypool lasts for mixing, and I can unlock the wallet for mixing only. | N, C, D | P1 | `walletpassphrase … mixingonly`, `-keypool`; automatic backups for legacy wallets | **missing** | DIFF-RPC; keys-left warnings and automatic backups for legacy-kind wallets behave as dashd's (CheckAutomaticBackup) |
| US-CJ-06 | Coins mixed by Dash Core or by a mobile wallet stay mixed and spendable after I move wallets. | N, C, D | P1 | `cj_salt`; Core mixes on the BIP44 external chain, mobile on DIP9 `m/9'/c'/4'` | **partial** (R5 US-CJ-06) | Importing a dashd-mixed wallet gives the same mixed balance and rounds as dashd. A mobile-mixed seed shows its DIP9 CoinJoin balance |
| US-CJ-07 | After V24 my wallet can promote or demote denominations (10:1 rebalance sessions). | N, C, D | P1 | `dsa` nFlags, rebalance sessions | **missing** | MIX on regtest with V24 active: rebalance sessions complete between Rust and dashd participants |
| US-CJ-08 | Mixing transactions are grouped and labelled in my history. | D | P2 | dash-qt transaction types (QT-086); iOS grouping (IOS-030) | **partial**: heuristic CoinJoin routing in key-wallet | DESK |
| US-CJ-09 | I stop mixing and merge or sweep my CoinJoin funds. | N, C, D | P2 | wallet sends from mixed coins | **present** (lib). D: IOS-057 not started | DESK |

### 2.8 CoinJoin servicing masternode (US-CJS)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-CJS-01 | My masternode runs mixing sessions with Core's exact protocol: queueing, entries, final transaction (BIP69, zero fee), signing, commit and timeouts. | N | P0 | `coinjoin/server.cpp`: `dsa`, `dsq`, `dsi`, `dsf`, `dss`, `dsc`, `dssu` | **missing** | DIFF-P2P: scripted clients drive N and dashd servers through every PoolMessage outcome (codes 0–22) and both timeouts; transcripts equal. FT `rpc_coinjoin` and `p2p_dstx` pass |
| US-CJS-02 | My masternode validates collateral and charges misbehaving clients the way dashd does. | N | P0 | `IsCollateralValid`, `ChargeFees`, `ChargeRandomFees` | **missing** | Same offender identification as dashd in scripted failures; over ≥1,000 sessions the charge frequencies are statistically equal to dashd's (about 34% and 10%) |
| US-CJS-03 | My masternode resists queue spam and respects the per-masternode mixing-transaction limit. | N | P0 | `IsMixingThresholdExceeded` (enabled MNs / 5), `MASTERNODE_MAX_MIXING_TXES` = 5 | **missing** | DIFF-P2P under DSQ floods: same drops and penalties (+10/+100) |
| US-CJS-04 | My masternode serves V24 rebalance sessions and keeps legacy participants out of them. | N | P0 | `COINJOIN_REBALANCE_VERSION` 70241; `dsa` nFlags | **missing** | MIX on regtest with V24 inactive, active, and across the activation block (PREMATURE scoring) |
| US-CJS-05 | Every node, masternode or not, relays DSTX and DSQ by dashd's rules. | N | P0 | `ValidateDSTX`, `CDSTXManager`, `senddsq` modes | **missing** | DIFF-P2P incl. announcing unbalanced DSTX as `MSG_TX` to peers below 70241. FT `p2p_dstx` |
| US-CJS-06 | I see my mixing server's state. | N, C | P1 | `getcoinjoininfo` (masternode form) | **missing** | DIFF-RPC during a session |

### 2.9 Miner and pool (US-MINE)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-MINE-01 | I get a block template with the right CbTx payload, masternode payments, superblock payments, quorum commitments, MNHF signals, asset unlocks and credit-pool balance. | N, C | P0 | `getblocktemplate` (`coinbase_payload`, `masternode[]`, `superblock[]`, `masternode_payments_started/enforced`, `superblocks_started/enabled`) | **missing** | For the same chain and mempool (regtest, and replay snapshots with injected mempools), templates from N and dashd agree in every consensus-relevant field and contain the same transactions; the order is the same unless the registry records a tie. Blocks mined from N's templates are accepted by dashd and vice versa |
| US-MINE-02 | I submit blocks and headers and propose blocks. | N, C | P0 | `submitblock`, `submitheader`, GBT proposal mode | **missing** | Result strings equal dashd's (`duplicate`, `inconclusive`, every reject reason) over the invalid-block corpus (NFR-CEQ-07) |
| US-MINE-03 | I see mining info and network hashrate, and prioritise transactions. | N, C | P1 | `getmininginfo`, `getnetworkhashps`, `prioritisetransaction`, `getprioritisedtransactions`, `-blockmaxsize`, `-blockmintxfee` | **missing** | DIFF-RPC |
| US-MINE-04 | My blocks signal version bits and EHF deployments correctly (for example bit 13 for V24 after the MNHF signal). | N, C | P0 | `ComputeBlockVersion`; GBT `rules`, `vbavailable` | **missing** | DIFF-RPC through every state transition of each deployment, incl. V24's dynamic threshold |
| US-MINE-05 | I mine on regtest for tests. | N, C | P0 | `generatetoaddress`, `generatetodescriptor`, `generateblock`, `dash-cli -generate` | **missing**; Dash Evo Tool's "Mine blocks" needs it (R5 §4) | Used throughout FT. XCLI `-generate` output identical |

### 2.10 RPC, ZMQ and REST integrator: exchanges, explorers (US-INT)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-INT-01 | I look up any transaction with its InstantSend and ChainLock status, also in batches. | N, C | P0 | `-txindex` (default on in Dash), `getrawtransaction`, `getrawtransactionmulti`, `gettxchainlocks`, `getislocks` | **missing** | DIFF-RPC on ≥100,000 random mainnet and testnet transactions at every verbosity; FT `feature_txindex` |
| US-INT-02 | I query by address: balance, UTXOs, deltas, txids, mempool. | N, C | P0 | `-addressindex`; `getaddressbalance`, `getaddressutxos`, `getaddressdeltas`, `getaddresstxids`, `getaddressmempool` | **missing** | DIFF-RPC on random and heavy addresses at several heights and with mempool activity; FT `feature_addressindex` |
| US-INT-03 | I look up spent outputs and blocks by timestamp. | N, C | P1 | `-spentindex` (`getspentinfo`), `-timestampindex` (`getblockhashes`) | **missing** | FT `feature_spentindex`, `feature_timestampindex`; DIFF-RPC |
| US-INT-04 | I credit deposits on an InstantSend lock and treat ChainLocked blocks as final. | N, C | P0 | `gettransaction.instantlock/chainlock`; ZMQ `hashtxlock`, `rawtxlock`, `rawtxlocksig`, `hashchainlock`, `rawchainlock`, `rawchainlocksig`; `-instantsendnotify`, `-chainlocknotify` | **partial**: IS and CL verification exist in dash-spv (R1 §2.2) | In MIX, a deposit-watcher script receives identical event sequences and contents from N and dashd |
| US-INT-05 | I stream events over ZMQ (all 19 topics). | N | P0 | `src/zmq`; `getzmqnotifications` | **missing** | ZMQ harness: identical topics, bodies and sequence numbers for scripted regtest scenarios; FT `interface_zmq_dash` and the Bitcoin-heritage ZMQ test pass |
| US-INT-06 | I read the chain over REST. | N | P1 | `-rest`; R2 §A.6 3 endpoints | **missing** | Every endpoint and format (bin, hex, json) returns bytes identical to dashd's; FT-MAP REST set |
| US-INT-07 | I get compact block filters (BIP157/158, Dash-extended). | N, C | P1 | `-blockfilterindex`, `-peerblockfilters`, `getblockfilter`, `scanblocks` | **partial**: filter construction exists for the client (`dash/src/bip158.rs`); no index or server (R1 §2.3) | Filters and filter headers equal dashd's for every mainnet and testnet block; VEC `blockfilters.json` |
| US-INT-08 | I get UTXO-set statistics and scan the UTXO set. | N, C | P1 | `gettxoutsetinfo` (+`-coinstatsindex`), `scantxoutset`, `gettxout` | **missing** | DIFF-RPC incl. the MuHash and `hash_serialized_2` values (Core's quirk reproduced, R4 F7); FT `feature_coinstatsindex`, `feature_utxo_set_hash` |
| US-INT-09 | I get fee estimates. | N, C | P1 | `estimatesmartfee`, `estimaterawfee`; `fee_estimates.dat` | **missing** | With the same block and mempool history replayed, estimates equal dashd's (DIFF-RPC) |
| US-INT-10 | I broadcast and test transactions. | N, C | P0 | `sendrawtransaction`, `testmempoolaccept`, `submitpackage` (regtest) | **missing**: wallet-only mempool (R1 §2.4) | Reject reasons equal dashd's over `tx_invalid.json` plus mutated special transactions of every type (NFR-CEQ-07); FT-MAP mempool set |
| US-INT-11 | I get and verify Merkle proofs for transactions. | N, C | P1 | `gettxoutproof`, `verifytxoutproof`, `getmerkleblocks` | **partial**: merkle-block types exist | Proofs are byte-identical to dashd's |
| US-INT-12 | I inspect special transactions in blocks. | N, C | P1 | `getspecialtxes`, `getblock` verbosity 2, `decoderawtransaction` | **partial**: payload types exist, with v24 gaps (R1 §2.1) | DIFF-RPC JSON for every special-tx type and version found in mainnet, testnet and regtest corpora |
| US-INT-13 | I secure RPC access: cookie, rpcauth, user/password, per-user whitelists, bind/allow rules, a separate queue for external users. | N, C | P0 | `-rpccookiefile`, `-rpcauth`, `-rpcuser`, `-rpcpassword`, `-rpcwhitelist`, `-rpcwhitelistdefault`, `-rpcallowip`, `-rpcbind`, `-rpcexternaluser`, `-rpcexternalworkqueue` | **missing** | FT `rpc_whitelist`, `rpc_external_queue` and the auth/bind FT-MAP set pass; HTTP status codes and the 250 ms delay on 401 equal dashd's |
| US-INT-14 | My existing client libraries and tools work unchanged: JSON-RPC 1.0-style replies, batches, named arguments, `/wallet/<name>` endpoints. | N, C | P0 | `httprpc.cpp`, `rpc/server.cpp` | **missing**; rust-dashcore's `rpc-integration-test` is dead (R1 §3.5) | rust-dashcore's revived `rpc-integration-test` passes against N; the functional-test framework's `AuthServiceProxy` works unchanged; a reference explorer stack (choice in PLAN) indexes the same data from N as from dashd |
| US-INT-15 | I query blocks, headers, chain tips, block statistics and mempool contents with dashd's JSON. | N, C | P0 | `getblock`, `getblockheader`, `getblockheaders`, `getblockhash`, `getchaintips`, `getblockstats`, `getchaintxstats`, `getdifficulty`, `getrawmempool`, `getmempool*` | **missing** | DIFF-RPC across replay snapshots; FT `rpc_getblockstats` (with its fixture) |

### 2.11 Platform integrator: Tenderdash, Drive, DAPI, evonodes (US-PLAT)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-PLAT-01 | I register and update an evonode (4000 DASH) with its platformNodeID and Platform ports or addresses. | N, C · D: pending decision | P0 | `protx register_evo`, `register_fund_evo`, `register_prepare_evo`, `update_service_evo` | **partial**: ProRegTx v2 evonode fields; no flow; no v3 ExtNetInfo payload (R5 US-EV-01) | FT `feature_llmq_evo`, `rpc_netinfo` |
| US-PLAT-02 | Platform (Tenderdash, Drive, DAPI) queries Core for quorums, masternode lists, ChainLocks, blocks and transactions over RPC and ZMQ. | N | P0 | the stable subset in §5.3 | **missing** (the client side exists in `rpc-client`) | Every call and topic in §5.3 is DIFF-RPC/ZMQ equal on recorded Platform traffic; golden fixtures frozen (§5.3); US-PLAT-09 end-to-end passes |
| US-PLAT-03 | Platform gets threshold signatures from Platform quorums and verifies signatures. | N | P0 | `quorum platformsign`, `quorum sign`, `quorum verify`, `quorum getrecsig`, `quorum hasrecsig`, `quorum isconflicting`, `quorum selectquorum` | **missing** | In MIX, sign requests sent to Rust and dashd members recover the same signature; DIFF-RPC |
| US-PLAT-04 | Users lock DASH into Platform credits (AssetLockTx) and Platform withdrawals unlock it (AssetUnlockTx signed by a Platform quorum), within credit-pool limits. | N, C | P0 | credit pool; asset lock/unlock validation; `getcreditpoolinfo`, `getassetunlockstatuses` | **missing**: decode only, no v2 (R1 §2.1) | DIFF-STATE: credit pool equal at every block of replay; FT `feature_asset_locks` |
| US-PLAT-05 | Platform and light clients get and verify quorum proof chains, rotation info, ChainLocks and InstantSend locks. | N, C | P1 | `getquorumproofchain`, `verifyquorumproofchain`, `quorum rotationinfo`, `verifychainlock`, `verifyislock`, `getchainlockbyheight` | **missing** | FT `feature_quorum_proof_chain`, `rpc_verifychainlock`, `rpc_verifyislock`; proofs byte-identical to dashd's |
| US-PLAT-06 | The Platform share of the masternode reward flows into the credit pool (MN_RR) and evonodes are paid accordingly. | N | P0 | payments, credit pool | **missing** | DIFF-STATE over the MN_RR era of the replay (coinbase outputs and pool balance) |
| US-PLAT-07 | Platform bans misbehaving evonodes with `platformban` messages that my node validates and relays. | N | P1 | `platformban` (inv 33, short ID 168) | **missing** | FT `p2p_platform_ban`; in MIX, the DKG bad-member effect is the same on N and dashd |
| US-PLAT-08 | I give Platform a restricted RPC user (whitelist, separate work queue). | N | P0 | `-rpcwhitelist` (composite names such as `quorumsign`), `-rpcwhitelistdefault`, `-rpcexternaluser` | **missing** | dashmate's generated whitelist works unchanged; calls outside it return 403 |
| US-PLAT-09 | I run a full evonode stack with dashmate using N as its Core service. | N | P1 | dashmate's Core service and generated `dash.conf` | n/a (new) | dashmate (local and devnet presets) runs with N's image in place of dashd and an unchanged `dash.conf`; Platform's end-to-end suite passes. LIVE testnet: an evonode with N as Core serves Platform for ≥14 days with no PoSe or Platform bans |

### 2.12 Light-client server (US-LCS)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-LCS-01 | My node serves compact block filters (BIP157/158, Dash-extended) to dash-spv clients: iOS, Dash Evo Tool, the desktop app. | N | P0 | `getcfilters`, `getcfheaders`, `getcfcheckpt`; NODE_COMPACT_FILTERS (forced on for masternodes) | **missing** (client only) | SPV: dash-spv's `dashd_sync` (35 tests) and `dashd_masternode` (10 tests) suites pass with N as the only peer; filters equal dashd's |
| US-LCS-02 | My node serves BIP37 bloom filtering, including Dash's special-transaction matching and the `isdlock` follow-ups, to dashj (Android). | N | P0 | NODE_BLOOM (on by default), `filterload`/`filteradd`/`filterclear`, `merkleblock`, special-tx matching | **missing** (client only) | DIFF-P2P for filtered sync. Android dash-wallet (dashj) on the agentbox `agentbox-api36` emulator syncs regtest and testnet from N alone and shows an InstantSend-locked incoming payment |
| US-LCS-03 | My node serves headers and compressed headers. | N | P0 | `getheaders2`/`headers2` (8000 / 2000 limits), `sendheaders2` | **missing** (client only) | DIFF-P2P: `headers2` encodings byte-identical for every chain range; FT `p2p_sendheaders_compressed` |
| US-LCS-04 | My node answers `getmnlistd` and `getqrinfo` byte-identically to dashd, including the legacy construction for old protocol versions. | N | P0 | `getmnlistd`/`mnlistdiff`, `getqrinfo`/`qrinfo` | **missing** (client parsers only) | Responses byte-identical to dashd's for random (base, block) pairs across mainnet and testnet history, for each protocol-version band 70225…70242 |
| US-LCS-05 | My node relays CLSIGs and ISDLOCKs, and answers `mempool` with transaction invs plus matching ISDLOCK invs and the best CLSIG inv. | N | P0 | `mempool` reply, CLSIG/ISDLOCK relay | **missing** | DIFF-P2P |
| US-LCS-06 | My node serves blocks and transactions (`getdata`, `notfound`) and limits itself correctly when pruned. | N | P0 | `ProcessGetData`; NODE_NETWORK_LIMITED (288 blocks) | **missing** | DIFF-P2P incl. `notfound` and pruned-node limits |
| US-LCS-07 | In practice, the real light clients sync from my node: dash-spv-based apps and dashj. | N | P1 | — | — | Dash Evo Tool's and dashwallet-desktop's regtest suites pass with N replacing dashd as their peer and miner (R5 §4). iOS: handed to pasta (no Mac on agentbox) |

### 2.13 Importer and migrator (US-IMP)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-IMP-01 | I build N's chain from Dash Core block files (`blk*.dat`), with `-reindex`, `-reindex-chainstate` or `-loadblock`. | N, C | P0 | `LoadExternalBlockFile`; `-loadblock`, `-reindex` | **missing** (R5 US-PU-10) | Importing a dashd `blocks/` directory, and `contrib/linearize` output, gives the same chain as dashd (DIFF-STATE); FT `feature_loadblock`, `feature_reindex` |
| US-IMP-02 | I import a dashd datadir (blocks, block index, chainstate with its XOR key, evodb) as a set that agrees on one tip, without re-validating, or with re-validation if I ask. | N, C | P0 | R2 §E.2–E.5 | **missing** | The imported state equals dashd's at that tip (DIFF-STATE). A set whose tips disagree is refused. The source is unchanged. The re-validating mode is no slower than IBD from local peers (BENCH) |
| US-IMP-03 | I import a legacy Berkeley DB `wallet.dat`: non-HD keys, Dash HD (`hdchain`), encrypted, CoinJoin-used, watch-only. | N, C, D | P0 | R2 §E.8; BDB 4.8 btree | **missing** in rust-dashcore. D: detected only; reader planned for desktop M6 (`dw-compat/src/walletdat.rs`) | For a corpus of BDB wallets made by Dash Core releases 0.16 through 23 (the `wallet_backwards_compatibility` releases plus 22.x and 23.x), N and D yield the same addresses, keys, labels, balances, locked coins, `cj_salt` and `g_object` records as dashd loading the same file (DIFF-RPC) |
| US-IMP-04 | I import a SQLite descriptor `wallet.dat`, with or without a stored mnemonic, encrypted or not, watch-only or with imported descriptors. | N, C, D | P0 | R2 §E.8 | **partial**: dashwallet-desktop `dw-compat` reads SQLite descriptor wallets that hold a mnemonic; other cases return `not_implemented` (desktop README) | As US-IMP-03, for wallets made by Dash Core v21 to v24 |
| US-IMP-05 | I import a `dumpwallet` file. | N, C, D | P1 | `importwallet`; dump format (R2 §A.6 1.8) | **partial**: dw-compat parses dumps and rebuilds HD from the header; loose keys are counted, not imported (desktop README) | HD rebuilt from the header, loose WIF keys imported; balances equal dashd's after `importwallet` |
| US-IMP-06 | I restore any Dash seed (mobile, Dash Evo Tool, Dash Core) and see all funds: BIP44, legacy `m/0'`, DIP9 CoinJoin, provider keys, with an optional passphrase and birth date. | N, C, D | P0 | R5 §3.1 path table | **present** (paths) / **partial** (passphrase, R5 US-W-03) | Seeds from Android, iOS and Dash Evo Tool test wallets restore the same balances in N and D as in the origin app; Core-originated seeds scan the BIP44 external chain with a lookahead ≥1,000 |
| US-IMP-07 | I bring over `dash.conf`, `settings.json`, peers and bans. | N, C | P2 | `peers.dat` (format 4), `banlist.json`, `settings.json` | **missing** | Effective settings carry over (US-NODE-03 corpus); `peers.dat` and bans are read |
| US-IMP-08 | I bring over Dash caches to speed up the first start: `governance.dat`, `mncache.dat`, `llmq/quorumdb`, `llmq/isdb`, `sporks.dat`. | N, C | P2 | R2 §E.6, §E.9 | **missing** | Governance sync after import is faster than a cold sync (BENCH) and the object set is equal; a migrated masternode needs no qgetdata recovery |
| US-IMP-09 | I can go back to Dash Core: export my wallets and blocks in forms dashd reads. | N, C, D | P1 | `importwallet`, `importdescriptors`, mnemonic restore, `-loadblock` | **partial**: D exports for dash-qt (QT-109) | dashd loads N's wallet exports with equal balances; dashd's `-loadblock` of N's block export reaches the same tip |
| US-IMP-10 | I import an Electrum-Dash wallet export. | N, C | P2 | `importelectrumwallet` | **missing** | DIFF-RPC on sample exports |
| US-IMP-11 | On the desktop I import an Android wallet backup file. | D | P2 | dashj backup format (R5 US-PU-08) | **missing** | DESK with sample backups |
| US-IMP-12 | On the desktop I import my dash-qt preferences. | D | P2 | QSettings and `settings.json` (QT-142) | D: not started | DESK |
| US-IMP-13 | I import from an xprv, xpub, raw `hdseed` or `listdescriptors` JSON. | N, C, D | P1 | `sethdseed`, `importdescriptors`, `listdescriptors` JSON | **partial**: `from_extended_key`, `from_xpub` (R5 US-PU-07). D: QT-108 partial | DIFF-RPC |

### 2.14 Regtest and devnet developer (US-DEV)

| ID | Story | Surf | Pri | Dash Core | rust-dashcore status | Acceptance |
|---|---|---|---|---|---|---|
| US-DEV-01 | I spin up regtest with instant blocks, masternodes, quorums, ChainLocks and InstantSend, using all of Core's regtest flags. | N, C | P0 | `-regtest`, `-llmqtestparams` and friends, `-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams`; DashTestFramework | **missing** | DashTestFramework's helpers (`mine_quorum`, `mine_cycle_quorum`, `dynamically_add_masternode` and the rest) work unmodified against N |
| US-DEV-02 | I launch a named devnet with custom subsidy, difficulty, LLMQ types and spork key. | N, C | P1 | `-devnet=<name>` and the devnet flags (R2 §C.7) | **partial**: `Network::Devnet` exists (`dash-network`), no parameters | FT `p2p_connect_to_devnet`; the MIX devnet of NFR-CEQ-09 runs on these flags |
| US-DEV-03 | I control time and the scheduler and mock peers and messages in tests. | N, C | P0 | `setmocktime`, `mockscheduler`, `sendmsgtopeer`, `addconnection`, `addpeeraddress`, `mnauth`, `setmnthreadactive`, `quorum dkgsimerror` | **missing** | The FTs that use them pass |
| US-DEV-04 | I flip sporks on devnet and regtest. | N, C | P0 | `sporkupdate`, `-sporkkey`, `-sporkaddr` | **missing** | FT `feature_sporks` |
| US-DEV-05 | I decode and construct raw objects. | N, C | P1 | `decoderawtransaction`, `decodescript`, `createrawtransaction`, `gobject deserialize`; `dash-tx`, `dash-util` | **partial**: codecs in the `dash` crate | DIFF-RPC; C's dash-tx equivalent passes Dash's `test/util/data/bitcoin-util-test.json` |
| US-DEV-06 | I inspect internal databases and index status and repair the masternode-list store. | N, C | P2 | `evodb verify/repair`, `getindexinfo`, `getrpcinfo`, `syncwithvalidationinterfacequeue`, `waitfor*` | **missing** | DIFF-RPC where the shape does not depend on the implementation |
| US-DEV-07 | I run Dash Core's functional test suite against N. | N | P0 | `test/functional`, `test_runner.py` | n/a (new) | One documented command runs the suite against N (compatibility runner, NFR-CEQ-05); tier 1 passes at the validating-node milestone, tier 2 (mixed) at the masternode milestone, all tiers at release (REL-5.3) |
| US-DEV-08 | I capture P2P messages for debugging. | N | P2 | `-capturemessages` | **missing** | Core's `message-capture-parser.py` reads N's files; FT `p2p_message_capture` |
| US-DEV-09 | rust-dashcore's own integration harness and the wallet apps' regtest harnesses can use N instead of dashd. | N | P1 | `contrib/setup-dashd.py`, `DashdTestContext` | n/a (new) | `cargo test -p dash-spv` (dashd tests) passes with `DASHD_PATH` pointing at N; dashwallet-desktop's regtest suites pass with N |
| US-DEV-10 | As a Core or Platform developer I run N next to dashd and get the first divergence in state or RPC answers. | N, C | P2 | — | n/a (new) | One command starts both on the same regtest or devnet and reports the first divergence (DIFF-STATE, DIFF-RPC) |

---

## 3. Functional requirements by subsystem

Every requirement is MUST unless marked SHOULD. "As Core" means the behaviour of the pinned reference release (Q5), with the R2 section cited as the working spec. R2's citations point at Core source lines; when R2 and Core's source disagree, Core's source wins and R2 gets a correction.

### 3.1 Consensus (FR-CON)

| ID | Requirement |
|---|---|
| FR-CON-01 | **One consensus layer.** Consensus rules are implemented once, as a pure function (chain view, block or tx) → (verdict, state delta) with no I/O and no wall-clock reads. The node-local inputs that dashd consults (best ChainLock, InstantSend locks, governance and sync state, local clock, assumevalid) are injected exactly where dashd consults them, with dashd's skip conditions (R4 §1.10). Validation, mempool, mining, RPC decoding and the fuzz/diff harnesses all call the same code |
| FR-CON-02 | **Rule set.** The rules of the per-network reference releases (Q5) for mainnet, testnet, devnets and regtest, **before and after V24 and `evo_shares`**. Matching current dashd is sufficient for history, because current dashd relaxes every historic rule (R4 §0.2). Consensus changes on Dash Core's release and develop branches after the pins are detected by a watcher (NFR-CEQ-11) and adopted through the normal task flow |
| FR-CON-03 | **Chain parameters** for all four networks as R2 §C.1–C.7: genesis blocks, magic, ports, address prefixes, BIP44 coin types, checkpoints (39 on mainnet, up to height 2487500), the hard-coded block-hash pins (`bad-sb-start` at 614820, `bad-dip3-enf-block` at 1047200), testnet's hard-coded LLMQ_25_67 height (847000), assumevalid, minimum chain work, the 15 buried deployment heights, BIP9/EHF parameters, PoW parameters, the 14 LLMQ types and per-network role assignment, superblock and budget parameters, CoinJoin limits, credit-pool period, spork addresses. The devnet genesis is built from the devnet name. Every regtest and devnet override flag (`-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams`, `-llmq*params`, `-highsubsidy*`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-llmqchainlocks` and the rest) is accepted with Core's parse rules and errors |
| FR-CON-04 | **Transaction serialization** (R2 §A.1 1): the 32-bit version/type field, inputs, outputs, lock time and payload are handled exactly as Core handles them in every deserialization, re-serialization, txid, merkle root and sighash; hashes, block storage and relay use Core's re-serialization (A1 D7); `vExtraPayload` present iff version ≥ 3 and type ≠ 0; payloads must be consumed exactly; the payload's length prefix is honoured, so unknown payload versions do not misalign blocks (fixes R1 §2.1 "wrong"); no witness serialization (the BIP144 path is removed) |
| FR-CON-05 | **Txid**: SHA256d of the serialization, except asset-unlock v2 (payload exactly 145 bytes and `payload[0] ≥ 2`), whose txid zeroes the last 132 bytes; the instance hash is always the full-serialization hash |
| FR-CON-06 | **CheckTransaction** exactly as R2 §A.1 1 (empty vin/vout allowances by type, 1 MB size, `MAX_TX_EXTRA_PAYLOAD` 10000, money range, duplicate inputs, coinbase scriptSig 2–100 or 1–100 for CbTx, null prevouts) |
| FR-CON-07 | **ContextualCheckTransaction**: type gates after DIP3 (`bad-txns-type`, `bad-cb-type`); `MAX_STANDARD_TX_SIZE` (100 kB) is consensus after DIP0001 |
| FR-CON-08 | **CheckTxInputs** incl. the asset-unlock fee path that skips input checks, coinbase maturity 100, money ranges |
| FR-CON-09 | **Lock times**: `IsFinalTx`, BIP68/112/113 gated by CSV, MTP cutoff |
| FR-CON-10 | **Sigops**: legacy counting (CHECKMULTISIG = 20), P2SH sigops added in ConnectBlock, CHECKDATASIG and CHECKDATASIGVERIFY not counted; limits by DIP0001 state |
| FR-CON-11 | **Script interpreter** (R2 §A.1 2): BASE sigversion only; flag bits 0–10, 14, 16 with Core's semantics; per-block flags (P2SH always; DERSIG, CLTV, CSV, NULLDUMMY by height); always-disabled opcodes fail even unexecuted; DIP0020 opcodes valid at every height with exact semantics (CAT, SPLIT, NUM2BIN, BIN2NUM, AND, OR, XOR, DIV, MOD); CHECKDATASIG(VERIFY) with single-SHA256 message hash and CheckSignatureEncoding's trailing-byte rule; limits 520 / 201 / 20 / 10000 / 1000 / 4-byte CScriptNum; script error codes identical to Core's `ScriptError`. Replaces the Bitcoin/Tapscript opcode table and the `bitcoinconsensus` verifier (R1 §2.1 "wrong") |
| FR-CON-12 | **Signatures**: legacy sighash including `vExtraPayload`, the SIGHASH_SINGLE bug (returns 1), FindAndDelete, OP_CODESEPARATOR; ECDSA verification with Core's lax DER parsing; LOW_S and NULLFAIL are policy only |
| FR-CON-13 | **X11 block hash, unconditionally** (removes the `core-block-hash-use-x11` feature footgun, R1 §7.1), identical to the SPH reference; `CheckProofOfWork` rules |
| FR-CON-14 | **Difficulty**: Bitcoin retarget before KGW, KGW, DGW v3 with its off-by-one; testnet/devnet minimum-difficulty rules; regtest no-retarget; devnet minimum-difficulty blocks; on mainnet up to height 68589 only the ±50% tolerance check with `ConvertBitsToDouble` (so BTC-era and KGW results are never exact-checked), DGW exact from 68590 and on every other network; `arith_uint256` wrap semantics (R4 §1.5, F4) |
| FR-CON-15 | **Floating point.** Every consensus computation that Core does in IEEE doubles gives Core's results bit-for-bit on every supported platform (binary64, no FMA; R4 §1.7). The subsidy matters most: mainnet only checks `coinbase ≤ subsidy + fees + limit`, so a subsidy that is too high would pass replay unnoticed. It is therefore compared against reference dashd's `getblockstats` subsidy at **every** height and fuzzed over the plausible input range (DIFF-FUZZ). KGW and the 68589 tolerance only feed the ±50% check (F4) |
| FR-CON-16 | **CheckBlock**: merkle root and the CVE-2012-2459 mutation check; for CbTx v4, `merkleRootAssetUnlocks` checked as MUTATED; relaxed 2 MB size and count limits; coinbase position; per-transaction checks; 40k sigops |
| FR-CON-17 | **ContextualCheckBlockHeader** (difficulty, checkpoints, `time > MTP`, `time ≤ now + 7200`, version minimums) and **ContextualCheckBlock** (size by DIP0001, finality, BIP34 until DIP3, CbTx type after DIP3) |
| FR-CON-18 | **ChainLock header rules**: `bad-prevblk-chainlock`, `bad-chainlock`, `BLOCK_CONFLICT_CHAINLOCK` |
| FR-CON-19 | **ConnectBlock order** exactly as R2 §A.1 1 (16 steps): evodb best-block check, BIP30 rule, superblock start hash, special-tx processing before UTXO updates, V24 shared-collateral spend check inside the UTXO loop, InstantSend conflict check only outside IBD with the ChainLock override, then block value and payee checks with the `check_superblock` mode and the do-not-punish classification |
| FR-CON-20 | **Subsidy**: `GetBlockSubsidyInner(prevBits, prevHeight, V20)` with all eras, the mainnet ≤ 4500 bug, the 1/14 yearly decline, devnet high-subsidy, treasury 10% then 20% |
| FR-CON-21 | **Masternode payments**: `GetMasternodePayment` schedule (no step at I+8P, BRR periods, 75% after V20, Platform share 37.5% computed on the subsidy only); `GetBlockTxOuts` split (operator reward, multi-payout remainder, shared-MN splits); set matching before V24 and strict multiset after; the no-payee leniency, incl. "accept anything" when every masternode is PoSe-banned while the credit pool is still credited; no payee validation before the DIP3 enforcement height (1047200 on mainnet) (R4 F6) |
| FR-CON-22 | **Block value and superblocks**: `IsBlockValueValid` incl. the superblock limit (the `nBits = 1` trick) and the old budget window; superblock payee validation (`IsValidSuperblock`, V24 duplicate rule) only when Core would run it (governance loaded, masternode-synced, block not ChainLocked). In the old 12.0 budget windows the coinbase value is unbounded when not checking (mainnet block 332320 overpays); during IBD and reindex only the value bounds apply (R4 F3) |
| FR-CON-23 | **Deployments**: 15 buried deployments with each call site's `ActiveAt` vs `ActiveAfter` choice reproduced (R2 §A.3 16); the BIP9/EHF state machine with dynamic thresholds (int32 arithmetic; overflow must not panic), per-deployment windows, `MinBIP9WarningHeight` as a consensus floor, EHF signal-height gating, signal expiry, acceptance and storage of unknown-bit and historic signals (bits 10, 11 and the abandoned 12) and Core's caching semantics; V24 (bit 13) and `evo_shares` (bit 14) per the reference release; `ComputeBlockVersion` (R4 §1.6) |
| FR-CON-24 | **assumevalid** skips exactly what dashd skips below the assumevalid block (R4 F2): scripts, and also the CbTx checks (MN-list, quorum and asset-unlock roots, best ChainLock, credit-pool balance), quorum-commitment signatures and ProTx signatures, while the masternode list and member computation are still built. Minimum chain work as Core. Correctness replays run with `-assumevalid=0` |
| FR-CON-25 | **Special-transaction validation** for every type and payload version (ProRegTx/ProUpServTx/ProUpRegTx/ProUpRevTx v1–3, CbTx v1–4, QcTx, MnHf, AssetLock v1–2, AssetUnlock v1–2, ProDisTx, ProUpShareTx, ProUpSharedRegTx), exact reject strings, payload-version gates (V19, V24) and signature schemes (message-signer compact recovery for collateral proofs, BLS `VerifyInsecure` for operator signatures, `VerifyHashCanonical` for shared types) as R2 §A.3 1–3, 6–8 |
| FR-CON-26 | **Shared-collateral covenant**: the template-output creation rule for every transaction incl. the coinbase, the spend rule, and pre-activation template outputs frozen |
| FR-CON-27 | **Credit pool**: construction per block, limit eras (windowed before WITHDRAWALS, 2000 DASH per block after it, the V24 rule using the balance at h−576), the cumulative index range set, the CbTx balance check, asset-unlock quorum recency and height window |
| FR-CON-28 | **Quorum commitments in blocks**: exact required count per enabled type inside mining windows, duplicate rules, null commitments (version unchecked), the `quorumIndex` rule for non-indexed versions, bitset padding rejection, the 66% PoSe penalty for invalid members |
| FR-CON-29 | **CbTx**: version gating (v2 required from DIP0008 height + 1, V20, V24), height, `merkleRootMNList` over all MNs incl. banned ones with memcmp ordering (leaf hash excludes the entry version; empty list gives zero), `merkleRootQuorums` over sorted hashes of **all configured LLMQ types, including disabled ones** (stale legacy LLMQ_50_60 commitments are still leaves, R4 F5), best-ChainLock rules, `creditPoolBalance`, `merkleRootAssetUnlocks` |
| FR-CON-30 | **BLS**: the legacy scheme (Chia; non-standard hash-to-G2 `ep2_map_legacy`; legacy encodings; no subgroup check on decode) and the basic scheme (IETF); the scheme is explicit per-block context (never a process global) that flips after V19 activation and reverts on undo; opposite-scheme bytes decode to an invalid object, all-zero bytes to null, garbage to an error; secure aggregation; threshold Lagrange with big-endian IDs mod r; BLS-IES. Conformance against dashbls vectors and oracle vectors with recorded provenance (dash-pkc's corpora have undocumented provenance, R3 §1.2) |
| FR-CON-31 | **Reject reasons and result classes** identical to Core's (strings; MUTATED vs CONSENSUS vs RESULT_UNSET), because they surface in `submitblock`, `testmempoolaccept`, logs and peer punishment |
| FR-CON-32 | **State-dependent validity**: N runs the governance, masternode-sync, ChainLock and InstantSend state machines so that state-dependent outcomes converge as dashd's do, incl. `FAILED_VALID` caching of `bad-cb-amount`/`bad-cb-payee` and reconsideration |
| FR-CON-33 | **Determinism**: no consensus outcome may depend on Cargo features, platform endianness, hash-map iteration order, thread scheduling or wall-clock time |
| FR-CON-34 | **Quirk register.** Every entry of R4's quirk register (§1.15, 34 items, with details in the R4 appendix) is replicated and has at least one test. Quirks found later by any verification layer are added to the register |

### 3.2 Chainstate (FR-CHS)

| ID | Requirement |
|---|---|
| FR-CHS-01 | Block index with Core's `nStatus` semantics: validity levels, HAVE_DATA/HAVE_UNDO, FAILED_VALID/FAILED_CHILD, CONFLICT_CHAINLOCK, ASSUMED_VALID; cached X11 hash |
| FR-CHS-02 | Headers-first sync; best-header vs active-tip tracking; most-work chain selection with failed and ChainLock-conflict propagation; `preciousblock`, `invalidateblock`, `reconsiderblock` |
| FR-CHS-03 | UTXO set with a write-back cache sized by `-dbcache`, batched atomic flushes and per-block undo data. DisconnectBlock reverses UTXOs, the masternode list, the quorum index, the credit pool, MNHF signals and the BLS scheme context |
| FR-CHS-04 | **Atomic consistency** across UTXO set, masternode-list state, LLMQ indexes and index best-block markers: a crash at any instant leaves a state that restarts without reindex (Core's evodb best-block check is an invariant here) |
| FR-CHS-05 | ChainLock enforcement (`EnforceBlock`, conflicting-chain marking); the best ChainLock is rebuilt at startup from coinbases and peers |
| FR-CHS-06 | IBD detection with Core's rules (maximum tip age 21600 s, minimum chain work) |
| FR-CHS-07 | No artificial reorg depth limit beyond Core's (ChainLocks are the limit) |
| FR-CHS-08 | `verifychain` levels 0–4, `-checkblocks`, `-checklevel`, `-checkblockindex` with equivalent checks |
| FR-CHS-09 | `-reindex` and `-reindex-chainstate` semantics, incl. wiping derived state as Core does |
| FR-CHS-10 | Parallel validation (scripts, ECDSA, BLS) is allowed only if results, including which reject reason is reported for a block with several invalid transactions, are identical to Core's |

### 3.3 Storage (FR-STO)

| ID | Requirement |
|---|---|
| FR-STO-01 | N uses its own on-disk formats in its own datadir; Dash Core's formats are read for import only (NG-4, Q11) |
| FR-STO-02 | Every store has a versioned schema; a release migrates the formats of the previous two releases in place and refuses newer formats without writing |
| FR-STO-03 | Crash safety: atomic commits, a documented fsync policy, and a power-loss test harness (US-NODE-04) |
| FR-STO-04 | Blocks are stored as their exact network bytes so they can be served, hashed and exported; random access by hash; undo data per block; pruning with Core's limits (≥ 288 blocks kept, 945 MiB minimum target) |
| FR-STO-05 | No bincode or other unversioned serde encoding for consensus state or node databases; explicit codecs with layout tests (R1 risk 9) |
| FR-STO-06 | A lock file and pid file; N refuses to write into a Dash Core datadir (US-NODE-19) |
| FR-STO-07 | Integrity checks on block and undo records and at DB level; corruption found at startup produces a clear error and a repair or reindex instruction, as Core does |
| FR-STO-08 | Historical masternode-list and credit-pool access at any height Core can answer for (Core keeps snapshots every 576 blocks plus diffs), for `protx diff`, `masternode list` at a height, and mnlistdiff/qrinfo serving |
| FR-STO-09 | Disk footprint per NFR-RES-02 |

### 3.4 P2P (FR-P2P)

Fingerprint obligations that cut across these requirements are in §4.2.

| ID | Requirement |
|---|---|
| FR-P2P-01 | **Messages**: all 74 commands (33 Bitcoin-heritage and 41 Dash) with exact payload codecs and protocol-version-dependent fields (R2 §A.2 2); `PROTOCOL_VERSION` 70242; disconnect peers below 70221. Today rust-dashcore covers 42 of 74 (R1 §2.3) |
| FR-P2P-02 | **Inventory types**: the 19 live types with Core's numbers (fixes `MSG_CMPCT_BLOCK` = 20, R1 §2.3 "wrong"); unknown types skipped; getdata-only types; re-request intervals (QSIGREC 15 s, CLSIG 5 s, ISDLOCK 10 s, others 60 s) |
| FR-P2P-03 | **v1 transport**: 24-byte header, 3 MiB message cap, bad checksum or command drops the message and keeps the connection, bad magic or oversize disconnects |
| FR-P2P-04 | **v2 transport (BIP324)**, on by default: HKDF salt with the Dash magic, short IDs 0–32 and 128–168, short-ID use gated on the peer's version (70235; 70240 for `platformban`), garbage length uniform in 0…4095, rekey, inbound v1 detection, outbound v2 only to peers advertising bit 12, v1 reconnect after early disconnect. PR #264 is the starting point |
| FR-P2P-05 | **Handshake**: `version` layout incl. the mnauth challenge and `fMasternode`; Core's service bits (0x1C05 node, 0x1C45 masternode, pruned variants); message order per R2 §A.2 6.1; desirable-service checks; devnet name check |
| FR-P2P-06 | **Connection manager**: Core's types and counts (125 total; 8 full-relay; 2 block-relay plus anchors; 8 addnode; feeler; extra peers), masternode connection flags and semaphore bypass, inbound acceptance and eviction incl. masternode protections, inactivity and ping timeouts, receive/send buffer limits, `-maxuploadtarget`, disconnect linger and `shutdown(SHUT_WR)` |
| FR-P2P-07 | **Addresses**: addrman with Core's bucket parameters; addr/addrv2 relay rules and rate limits; the `getaddr` cache (21–27 h, 23%, 1000); DNS seeds (`x1.` filter, timing); fixed seeds (BIP155 data equal to Core's `chainparamsseeds`); `-seednode`, `-addnode`, `-connect` semantics; anchors |
| FR-P2P-08 | **Block download**: headers2/headers sync; 1024-block window; 16 blocks in flight per peer; stalling and download timeouts per R2 §A.2 6.3; BIP152 compact blocks v1 (txid-based) with high/low-bandwidth semantics; announcements via headers2/headers/inv; `MAX_BLOCKS_TO_ANNOUNCE` |
| FR-P2P-09 | **Transaction and object relay**: the generalized object-request tracker (100,000 announcements per peer, 100 in flight before the overload delay), the non-preferred-peer delay (none in masternode mode), Poisson trickles (5 s inbound shared, 2 s outbound, 1 s to verified outbound masternodes, none masternode-to-masternode or noban), rate caps, the immediate path for non-transaction invs (incl. block-relay-only peers), ISDLOCK filter rules, orphans (10 MB, 20 min, 5-min sweep) |
| FR-P2P-10 | **Serving**: `getheaders`/`getheaders2`, `getblocks`, `getdata` for every object type, `mempool`, BIP157 (limits 1000/2000/1000), BIP37 with Dash special-tx matching, `merkleblock` followed by `tx` then `isdlock`, `getmnlistd`/`getqrinfo` (byte-identical, all version bands), sporks, governance, quorum data (masternodes), CoinJoin queues |
| FR-P2P-11 | **Misbehaviour**: Core's score table (R2 §A.2 4.5), threshold 100, the discouragement filter (50,000 entries), manual bans (`banlist.json` semantics, 24 h default), permissions, local-peer handling |
| FR-P2P-12 | **Privacy networks**: SOCKS5 proxy, Tor control and onion service (ED25519-V3, persisted key), I2P SAM 3.1 (persisted key), CJDNS, `-onlynet`, `-proxyrandomize` |
| FR-P2P-13 | **Background traffic**: `getsporks` once per relay peer, `mempool` to outbound peers after sync (`-syncmempool`), `govsync` and per-object vote sync with Core's cadence, disconnect after `full-sync`, sync restart after 1 h of inactivity (not on masternodes) |
| FR-P2P-14 | **Masternode networking**: the MNAUTH signing tuple and verification, probes, quorum connection sets (all-connected vs ring by spork 21), watch connections, `qsendrecsigs` and `qwatch`, the masternode-connection thread's cadence and backoff, refusing inbound until synced, 100 ms send cadence on masternode connections, dropping unverified masternode connections after 5 s, `DeterministicOutboundConnection` deduplication |
| FR-P2P-15 | **netfulfilled** request tracking with Core's names and expiry (1 h mainnet, 5 min elsewhere) |
| FR-P2P-16 | **Identity**: user agent and reported version configurable per Q4; `-uacomment` |
| FR-P2P-17 | SHOULD: Erlay (`-txreconciliation`), off by default as in Core |
| FR-P2P-18 | `-capturemessages` writes Core's capture format |
| FR-P2P-19 | Net state files (`peers.dat`, `anchors.dat`, `banlist.json`, onion and I2P keys) may use N's own formats but Core's are importable (US-IMP-07) |

### 3.5 Mempool (FR-MEM)

| ID | Requirement |
|---|---|
| FR-MEM-01 | **Acceptance** with Dash's PreChecks (R2 §A.1 5): `qc-not-allowed`; `IsStandardSpecialTx`; InstantSend-lock conflicts (`tx-txlock-conflict`) and eviction of conflicts when a lock is pending; asset-unlock duplicates, stale instances and refresh; V24 shared-collateral checks; the MNHF fee exemption; `CheckSpecialTxForMempool`; the six ProTx duplicate maps plus the cross-scheme operator-key check |
| FR-MEM-02 | **Policy defaults and options** identical to Core's (minimum relay fee 1000 duff/kB, dust, standard sizes and script types, datacarrier 83 bytes, bare multisig, `-acceptnonstdtxn` on test networks) |
| FR-MEM-03 | **No replacement**: any conflict is `txn-mempool-conflict` |
| FR-MEM-04 | Ancestor/descendant limits (25 / 101 kvB) with the CPFP carve-out; virtual size = max(size, sigops × 20) |
| FR-MEM-05 | `-maxmempool` eviction and rolling minimum fee, `-mempoolexpiry` (336 h), trimming |
| FR-MEM-06 | Mempool address and spent indexes when those indexes are on (for `getaddressmempool`, `getspentinfo`) |
| FR-MEM-07 | `-persistmempool`; Core's `mempool.dat` v1 is readable (SHOULD); N's own format allowed |
| FR-MEM-08 | **Fee estimation** equal to Core's `CBlockPolicyEstimator` (buckets 1000…1e7 at spacing 1.05, decays .962/.9952/.99931, horizons), persisted with Core's staleness rule (60 h) |
| FR-MEM-09 | InstantSend interactions: remove conflicts when a lock arrives; double-spend ZMQ notifications |
| FR-MEM-10 | `testmempoolaccept` and `sendrawtransaction` result shapes and reject strings identical; `prioritisetransaction` deltas |
| FR-MEM-11 | Mempool sequence numbers for ZMQ `sequence` and REST `mempool_sequence` |

### 3.6 Mining and block templates (FR-MIN)

| ID | Requirement |
|---|---|
| FR-MIN-01 | **Block assembly** as Core's `CreateNewBlock`: coinbase reservations; quorum commitments first (from the mineable pool, null commitments where needed); package selection by ancestor fee rate; credit-pool diff tracking; duplicate EHF bits; `TestPackageTransactions` incl. special-tx checks and the 10-minute `IsTxSafeForMining` rule when InstantSend is enabled |
| FR-MIN-02 | **Coinbase**: pre- and post-DIP3 forms, CbTx version selection, best-ChainLock selection, `creditPoolBalance`, `FillBlockPayments` order (masternode outputs, then superblock outputs) |
| FR-MIN-03 | **getblocktemplate**: every standard and Dash field (R2 §A.1 4); refusal conditions (no peers, IBD, masternode sync incomplete at a superblock height); long polling; proposal mode |
| FR-MIN-04 | `submitblock` and `submitheader` result strings identical |
| FR-MIN-05 | Regtest mining RPCs (`generate*`) and `-generate` in C |

### 3.7 Evo and the masternode list (FR-EVO)

| ID | Requirement |
|---|---|
| FR-EVO-01 | **Deterministic masternode list** with Core's data model (R2 §A.3 4.1): `internalId`, total registered count, state fields, state diffs with the field bitmask, and the unique-property map with reference counts and its hashing rules (ADDRv2 only for non-ADDRv1 addresses; scheme-dependent operator-key slots) |
| FR-EVO-02 | **Per-block transition order** exactly as R2 §A.3 4.2: confirm, decay PoSe, special transactions in block order, collateral spends, payee bump, consecutive-payment reset; the payee is computed from the previous list before mutation |
| FR-EVO-03 | PoSe penalties, decay, bans and revival (R2 §A.3 4.3) |
| FR-EVO-04 | Payee selection incl. the evonode four-in-a-row rule before MN_RR and memcmp tie-breaks |
| FR-EVO-05 | `SetStateVersion` semantics (V24 version lifting, payout migration, operator-key re-encoding, netinfo conversion and its failure) |
| FR-EVO-06 | Network info: `MnNetInfo` and `ExtNetInfo` encoding and validation, incl. domain rules and per-network port rules |
| FR-EVO-07 | Simplified list entries (versioned network form; hash excludes the version) and `merkleRootMNList` |
| FR-EVO-08 | `mnlistdiff` construction byte-identical to Core for every protocol band (incl. `quorumsCLSigs` after V20 and the extended comparison of payouts and shares) |
| FR-EVO-09 | MNHF manager: per-block signal maps, expiry, the legacy/new key distinction where it affects results |
| FR-EVO-10 | Masternode metadata (Core's `mncache.dat` content): DSQ counters, mixing-transaction counts, outbound attempt statistics, Platform ban flags, governance vote tracking; persisted (own format allowed) |
| FR-EVO-11 | Masternode-sync state machine shared with FR-P2P-13 |
| FR-EVO-12 | Import of Core's evodb, read-only, incl. the legacy `dmn_D3` diff format (US-IMP-02) |

### 3.8 Active masternode (FR-AMN)

| ID | Requirement |
|---|---|
| FR-AMN-01 | Masternode mode from `-masternodeblsprivkey`, with Core's parameter interactions (`disablewallet`, `peerblockfilters`, `blockfilterindex=basic`) and hard requirements (listen, txindex, peerbloomfilters, no prune, `maxconnections` ≥ 125, governance enabled, not blocksonly, `maxuploadtarget` = 0) and their errors |
| FR-AMN-02 | States WAITING_FOR_PROTX, POSE_BANNED, REMOVED, OPERATOR_KEY_CHANGED, PROTX_IP_CHANGED, READY, ERROR; local address detection; self-connect test; reactions to removal, key change and IP change |
| FR-AMN-03 | All masternode roles in masternode mode and none otherwise: DKG sessions, signature shares, ChainLock signer, InstantSend signer, governance signer, EHF signer, CoinJoin server |
| FR-AMN-04 | PoSe-relevant conduct: advertise NODE_COMPACT_FILTERS and relay transactions on masternode connections, protocol ≥ 70242, answer probes, so that no "badConnection" criterion is ever triggered by N |
| FR-AMN-05 | The operator key is never logged, is zeroized in memory and is read from config as Core does (SHOULD also accept a key file) |

### 3.9 LLMQ, ChainLocks and InstantSend (FR-LLMQ, FR-CL, FR-IS)

| ID | Requirement |
|---|---|
| FR-LLMQ-01 | The 14 LLMQ types with Core's parameters; enabled-type rules (`IsQuorumTypeEnabled`); the rotation-enable rule; spork evaluation (17, 21, 23); devnet and regtest overrides |
| FR-LLMQ-02 | **Member selection**: score = single SHA256 read little-endian; `CalculateQuorum` ordering and tie-break (collateral descending); modifier rules (pre/post V20, CL-signature based); the base−8 list rule for non-rotated quorums; DIP24 quarters from snapshots incl. skip-list quirks, `skipRemovedMNs` and the testnet exception; **snapshots created as a side effect of connecting cycle-base blocks, persisted and not reverted on reorg**; cache semantics |
| FR-LLMQ-03 | **DKG**: phase clock, round driver and per-member sleeps (none on regtest); the four message types and their sign hashes; intake checks and penalties; contribution, complaint, justification, premature and final commitment logic; single-member quorums; batched signature verification; contribution persistence and cleanup |
| FR-LLMQ-04 | **Final commitments**: versions 1–4, serialization, `BuildCommitmentHash` (public key under the scheme context), `Verify`/`VerifyNull`, the mineable pool and `qfcommit` relay rules incl. the stale re-propagation ban from 70239 |
| FR-LLMQ-05 | **Quorum manager**: `ScanQuorums` with mining-window snapping and indexed rotation scans; building quorums from commitments; verification-vector and secret-share storage; cleanup at Core's heights; `IsQuorumActive` |
| FR-LLMQ-06 | **Data recovery**: `qgetdata`/`qdata` formats, limits (64 per requester, 4096 total, 300 + 60 s), recovery start offset, `-llmq-data-recovery`, `-llmq-qvvec-sync` |
| FR-LLMQ-07 | **Connections**: relay ring or full mesh, deterministic initiator, watch connections, `keepOldConnections` |
| FR-LLMQ-08 | **Signing**: `SignHash`; length-prefixed request IDs (`clsig`, `inlock`, `islock`, `mnhf`, `plwdtx`, `PlatformPoSeBan`); quorum selection (non-rotated: memcmp minimum; rotated: `(GetUint64(3) >> (64 − n − 1)) & mask`); `VerifyRecoveredSig`; vote and conflict rules (different message hashes allowed for Platform and EHF); the recovered-signature store with `-maxrecsigsage` |
| FR-LLMQ-09 | **Signature shares**: `qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare` (spork 21), AUTOBITSET encoding, batch limits, recovery-member order and backoff, Lagrange recovery, batch-verification caps |
| FR-LLMQ-10 | Platform and EHF signing users (`quorum sign`, `quorum platformsign`, asset unlocks, EHF) |
| FR-LLMQ-11 | `qrinfo` construction byte-identical incl. the legacy (< 70236) path |
| FR-LLMQ-12 | Quorum proof chains (`getquorumproofchain`, DASHNC02) byte-identical |
| FR-CL-01 | `clsig` format and hash, request ID, `VerifyChainLock` against the quorum set at `chain[h − 8]` |
| FR-CL-02 | Signing conditions (synced; spork 19 value 0; transactions in the tip and previous 5 blocks InstantSend-locked or seen ≥ 10 minutes) and CLSIG creation from the recovered signature |
| FR-CL-03 | Processing: seen cache, height ordering, penalties (+10 invalid, +10 unrequested), enforcement and best-chain activation |
| FR-CL-04 | Validation effects in headers, ConnectBlock and block-template tests; best-ChainLock ingestion from coinbases; rebuild at startup |
| FR-CL-05 | `-chainlocknotify`, ZMQ ChainLock topics, `getbestchainlock`, `getchainlockbyheight`, `verifychainlock`, `submitchainlock` |
| FR-IS-01 | `isdlock` format, trivial validity, request IDs, the asset-unlock synthetic outpoint |
| FR-IS-02 | Signing flow: `CheckCanLock` (parents locked, ChainLocked or old enough), conflict abort, per-input signatures, quorum and `cycleHash` selection, retroactive signing on block connect |
| FR-IS-03 | Verification windows (offset 0, then `dkgInterval`; the `nSignHeight` rule), banning after both fail, the non-rotating regtest override |
| FR-IS-04 | Conflict resolution with blocks and ChainLocks, mempool removal, `tx-txlock-conflict` |
| FR-IS-05 | Lock persistence with Core's lifecycle (mined, archived, pruned after 100 blocks, removed on ChainLock) |
| FR-IS-06 | Gates (`IsInstantSendEnabled`: spork 2, not reindexing or importing), `-instantsendnotify`, ZMQ topics, `getislocks`, `verifyislock` |

### 3.10 Governance (FR-GOV)

| ID | Requirement |
|---|---|
| FR-GOV-01 | Object and vote wire formats and **all four hashes** with Core's quirks: object hash (data as a hex string, vin-like suffix, signature included, collateral hash excluded), object signature hash, data hash, vote hash (signal before outcome) and vote signature hash (outcome before signal) |
| FR-GOV-02 | Proposal validation in Core's order with Core's messages (512-byte limit, name and URL rules, epochs, amount, address incl. P2SH); the `ParsePaymentAmount` quirk for triggers |
| FR-GOV-03 | Collateral validation through the transaction index (1-DASH OP_RETURN of the object hash; 6 confirmations; objects with 1–5 confirmations postponed) |
| FR-GOV-04 | Vote validation: key selection by object type and signal (voting key for proposal funding votes, operator BLS key otherwise); per-network signature format (mainnet and devnet signmessage, testnet hash signing); 65/96-byte signature lengths; the 1 h future limit; tie-break on equal time; the 1 h rate limit; orphan votes with 10-minute expiry and parent fetch |
| FR-GOV-05 | Tallies and cached flags with evonode weight 4, funding threshold max(min quorum, W/10), delete threshold, vote invalidation after key changes or removal |
| FR-GOV-06 | Trigger rate limiting (5-entry buffer), creation-time window, additional relay; object lifecycle (10-minute deletion delay, erased-object retention) |
| FR-GOV-07 | Sync protocol: `govsync`, `govobj`, `govobjvote`, `ssc` semantics; netfulfilled limits; penalties; bloom size checks; per-network objects per tick; finish conditions |
| FR-GOV-08 | Masternode trigger creation: approved-proposal ordering, greedy budget fill, the ±2 h fudge window, the byte-exact trigger JSON template, deduplication by data hash, creator = first projected payee, funding votes yes/no |
| FR-GOV-09 | Superblock validation (`GetBestSuperblock` tie order; `IsValid` incl. the V24 duplicate rule) and trigger expiry |
| FR-GOV-10 | Persistence in N's own format (Core's `governance.dat` importable, US-IMP-08); `-disablegovernance` semantics (forced by prune, forbidden on masternodes) |
| FR-GOV-11 | Governance RPCs (§5) incl. the wallet-registered ones; ZMQ governance topics |

### 3.11 CoinJoin (FR-CJS server, FR-CJC client)

| ID | Requirement |
|---|---|
| FR-CJS-01 | Server state machine and messages (R2 §A.5 3.4): session creation and joining, readiness, entries, BIP69 final transaction, signature checks (P2SH \| STRICTENC), commit with the +0.1 DASH priority delta and the sane-fee mempool check, BLS-signed DSTX, timeouts (30 s, 15 s in signing), fee-charging probabilities |
| FR-CJS-02 | `IsValidInOuts` (STANDARD, PROMOTION, DEMOTION, FINAL_TX) and collateral validity |
| FR-CJS-03 | DSQ handling and spam control (enabled MNs / 5 threshold on the global DSQ counter; at most 5 mixing transactions per MN) |
| FR-CJS-04 | V24 rebalance sessions: `nFlags` only from 70241, declared shapes, no legacy participants, unbalanced DSTX structure rules, PREMATURE scoring at the activation boundary |
| FR-CJS-05 | DSTX relay validation on every node (structure, MN resolution up to 24 blocks back, BLS signature, priority, expiry) and per-version announcement rules |
| FR-CJC-01 | Client: scheduling, sessions, queue selection (skipping MNs about to be paid), denomination and collateral creation, final-transaction checks and signing (`SIGHASH_ALL\|ANYONECANPAY`), pending observations persisted, timeouts |
| FR-CJC-02 | Rounds computation and `IsFullyMixed` with `cj_salt` (memoized, Core's sentinels, the V24 reset rule); ONLY_FULLY_MIXED spends without change; coin types |
| FR-CJC-03 | Options and RPCs (`coinjoin`, `coinjoinsalt`, `setcoinjoin*`, `getcoinjoininfo`); automatic backups and keys-left warnings for legacy-kind wallets |
| FR-CJC-04 | SHOULD: one CoinJoin client implementation in rust-dashcore, used by both the node wallet and dashwallet-desktop. The desktop's M3 client is the starting point |

### 3.12 Sporks (FR-SPK)

| ID | Requirement |
|---|---|
| FR-SPK-01 | Spork IDs and defaults; on mainnet the spork table is ignored and the hard-coded values apply (all 0 except spork 21 = 1) |
| FR-SPK-02 | Message format and hash; signature schemes (testnet signs the hash; other networks sign the decimal string with the "DarkCoin Signed Message" magic); the +2 h time bound; newest wins; +100 for invalid; inv relay; `getsporks` replies |
| FR-SPK-03 | `-sporkaddr`, `-sporkkey`, `sporkupdate`, `spork show/active`; persistence (Core's `sporks.dat` importable) |

### 3.13 Indexes (FR-IDX)

| ID | Requirement |
|---|---|
| FR-IDX-01 | txindex **on by default**; indexes build asynchronously in the background; `getindexinfo` |
| FR-IDX-02 | addressindex, spentindex and timestampindex, with result ordering that matches Core's key layout (big-endian height and position) |
| FR-IDX-03 | blockfilterindex: Dash-extended basic filters (special-tx elements, spent prevouts, SipHash keys from the X11 block hash), filter-header chain; served over BIP157 and RPC |
| FR-IDX-04 | coinstatsindex (MuHash3072); `gettxoutsetinfo` hash types equal, incl. the `hash_serialized_2` VARINT-precedence quirk (R4 F7) and `unclaimed_rewards` going negative after asset unlocks |
| FR-IDX-05 | Prune and reindex incompatibilities as Core |
| FR-IDX-06 | Turning an index on for a synced, unpruned node builds it in the background without a reindex |

### 3.14 RPC server (FR-RPC)

| ID | Requirement |
|---|---|
| FR-RPC-01 | HTTP: POST only (405 otherwise); handlers at `/` and `/wallet/<name>`; batches; 8192-byte header limit; keep-alive; status codes per Core's mapping |
| FR-RPC-02 | Auth: cookie (`__cookie__:<hex>`, mode 0600, written via `.tmp` and rename, deleted at shutdown), `-rpcuser`/`-rpcpassword`, `-rpcauth` (HMAC-SHA256); 401 after a 250 ms delay |
| FR-RPC-03 | Bind and allow rules (loopback by default; `-rpcbind` honoured only with `-rpcallowip`) |
| FR-RPC-04 | Whitelists incl. composite names (`method` + `params[0]` without a space), `-rpcwhitelistdefault`; external users on a separate work queue |
| FR-RPC-05 | Dispatch: subcommand squashing (`method + " " + params[0]`), named arguments with aliases and `args`, Core's string-or-number quirks, `-deprecatedrpc` values (`service`, `legacy_mn`, `softforks`, `banscore`, `walletwarningfield`, `permissive_bool`), warmup error -28 |
| FR-RPC-06 | **Output identical**: field names, field order, number formatting (amounts with 8 decimals), hex case, hash byte order, error codes and messages. `help` output SHOULD be byte-identical |
| FR-RPC-07 | The 289 commands as classified in §5 |
| FR-RPC-08 | Threading and queues (`-rpcthreads`, `-rpcworkqueue`, `-rpcservertimeout`); `getrpcinfo` |
| FR-RPC-09 | Commands that Core does not have (for example the implementation-info RPC of Q4, or import tools) use names that cannot collide with Core's and never change Core commands' behaviour |
| FR-RPC-10 | No JSON-RPC 2.0, as Core (checked in `src/rpc/request.cpp` at the surveyed commit; R4 §2.5's "1.0/2.0" is loose wording) |

### 3.15 ZMQ and REST (FR-ZMQ, FR-REST)

| ID | Requirement |
|---|---|
| FR-ZMQ-01 | All 19 publishers; three-part messages (topic, body, little-endian per-notifier sequence); byte-reversed hashes; raw bodies in network serialization; `-zmqpub*hwm`; `getzmqnotifications` |
| FR-ZMQ-02 | Notifications are published in the same order as dashd's for the same event sequence |
| FR-REST-01 | Every REST endpoint and format of R2 §A.6 3, with Core's limits, deprecated forms and 503 during warmup |

### 3.16 CLI (FR-CLI)

| ID | Requirement |
|---|---|
| FR-CLI-01 | All dash-cli options (R2 §A.6 6.2): config-file reading, chain selection, cookie discovery, `-rpcconnect`, `-rpcport`, `-rpcuser`, `-rpcpassword`, `-rpccookiefile`, `-rpcwallet`, `-rpcwait`, `-rpcwaittimeout`, `-rpcclienttimeout` (900 s), `-stdin`, `-stdinrpcpass`, `-stdinwalletpassphrase`, `-named`, `-color` |
| FR-CLI-02 | stdout, stderr and exit codes byte-identical to dash-cli for identical responses (pretty printing, unquoted string results, Core's error format) |
| FR-CLI-03 | Local commands `-getinfo` (incl. `mn_in`, `mn_out`, `mn_total`, `coinjoin_balance`), `-netinfo [0-4\|help]`, `-addrinfo`, `-generate` |
| FR-CLI-04 | C works against dashd, and stock dash-cli works against N |
| FR-CLI-05 | Tool equivalents: dash-tx (all commands and registers), dash-util `grind`, dash-wallet (`info`, `create`, `dump`, `createfromdump`, `salvage`, `wipetxes`), import and export tools (US-IMP), and differential mode (US-DEV-10). Names per Q3 |

### 3.17 Node wallet (FR-WAL)

| ID | Requirement |
|---|---|
| FR-WAL-01 | Built by extending key-wallet and key-wallet-manager, not by replacing them. Features that both the node wallet and the desktop need (encryption, imported keys, descriptors, PSBT, ProTx builders, governance votes, CoinJoin, Core import) live in the shared library layer |
| FR-WAL-02 | Wallet kinds per Q2 (recommended: descriptor by default, plus legacy-HD semantics for imported legacy wallets and `descriptors=false`), with Core's flags and Core's per-kind errors for kind-specific RPCs |
| FR-WAL-03 | Derivation and key pools: BIP44 `m/44'/c'/0'` (coin 5 or 1); the inactive DIP9 CoinJoin descriptor; Core legacy mixing on the BIP44 external chain; keypool 1000; Core's BIP39 quirks on import; mnemonic passphrase everywhere (fixes R5 US-W-03) |
| FR-WAL-04 | Encryption at rest at least as strong as Core's (AES-256-CBC with an SHA-512-derived key, ≥ 25,000 calibrated iterations); mnemonic and seed encrypted; unlock timeouts; mixing-only unlock |
| FR-WAL-05 | Transaction building: Core's fee computation, coin types, ONLY_FULLY_MIXED without change, subtract-fee, send-all, OP_RETURN, `use_is`/`use_cj`. Transactions look like dashd's on chain: version, `nLockTime` (anti-fee-sniping as Core), `nSequence`, input/output ordering, change position, signature encoding |
| FR-WAL-06 | Signing for P2PKH, P2PK and P2SH multisig with all sighash types; PSBT (BIP174 v0, Dash's field set) interoperable with dashd and dw-psbt |
| FR-WAL-07 | InstantSend and ChainLock awareness (InstantSend-locked = trusted at depth 0; `addlocked`; chainlock flags), conflicts and abandonment, and reorg rollback (closes rust-dashcore #854) |
| FR-WAL-08 | Descriptor engine: `pk`, `pkh`, `sh`, `multi`, `sortedmulti`, `combo`, `addr`, `raw`; checksums; both hardening markers; `listdescriptors` with Dash's mnemonic fields |
| FR-WAL-09 | Imports of keys, scripts, descriptors and dumps, with rescans from a timestamp or height |
| FR-WAL-10 | Masternode features: collateral detection and auto-lock; ProTx builders for every type incl. shared types 10–12, with BLS, ECDSA (collateral message) and EdDSA signing; `masternode outputs`; `protx list wallet` |
| FR-WAL-11 | Governance features: prepare, submit and resume proposals (`g_object` records); `vote-many`, `vote-alias`; `voteraw` helpers |
| FR-WAL-12 | CoinJoin client per FR-CJC |
| FR-WAL-13 | Persistence in N's own encrypted, versioned format; automatic backups (`-createwalletbackups`, Core's naming); `backupwallet` output restorable with `restorewallet`; dashd-readable exports per US-IMP-09 |
| FR-WAL-14 | Wallet RPC shapes identical; `-walletnotify`, `-instantsendnotify` |
| FR-WAL-15 | Secrets are never logged and never written unencrypted (fixes the plaintext `Wallet::backup()`, R1 §2.5) |

### 3.18 Import and export (FR-IMP)

| ID | Requirement |
|---|---|
| FR-IMP-01 | Block import from `blk*.dat` (byte-wise magic scan, size bounds 80 B to 2 MB, out-of-order parents) and from linearize output |
| FR-IMP-02 | Datadir import, read-only: block index (with the stored X11 hash), chainstate with its obfuscation key, evodb keys incl. legacy formats, LLMQ databases, indexes optionally; tip-agreement check; no writes and no lock taken on the source (needs a LevelDB reader) |
| FR-IMP-03 | Wallet import: BDB 4.8 btree reader (leaf and overflow pages), SQLite, every record type in R2 §A.6 1.4 incl. Dash's (`hdchain`, `chdchain`, `hdpubkey`, `g_object`, `cj_salt`, `ps_salt`, `cj_pending_obs`, `platform_data`), Core's crypter, descriptor keys with mnemonics, the dump format. SHOULD: move dashwallet-desktop's `dw-compat` (BIP39 quirks, crypter, dump, SQLite reader, descriptors; about 2.3k lines) into rust-dashcore as the shared base |
| FR-IMP-04 | Flat files: `governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`, `peers.dat` (format 4), `anchors.dat`, `banlist.json`, `mempool.dat` v1, `fee_estimates.dat`, `settings.json`, `onion_v3_private_key` |
| FR-IMP-05 | Export: `dumpwallet` format, descriptor JSON, mnemonic; linearize-style block export; never write into a Core datadir |
| FR-IMP-06 | Every import is read-only on the source and ends with a verification report (counts, hashes, balances) |

### 3.19 Configuration and datadir (FR-CFG)

| ID | Requirement |
|---|---|
| FR-CFG-01 | Every dashd option in R2 §A.6 6.2 is recognized. Each is implemented, or, when it is build-specific (BDB tuning, Qt options), accepted and ignored with a warning. Unknown options are handled exactly as Core handles them, on the command line and in the config file (dashmate's template, for example, writes the dash-cli option `rpcwallet` into `dash.conf`) |
| FR-CFG-02 | Config semantics: network sections, `includeconf` rules, NETWORK_ONLY options, mutually exclusive chain selection, `settings.json` read and write, `-nosettings` |
| FR-CFG-03 | Datadir: N's own base directory (Q3); per-network subdirectories named as Core's (`""`, `testnet3`, `devnet-<name>`, `regtest`); `-blocksdir`, `-walletdir`; lock and pid files |
| FR-CFG-04 | Default ports per Q3, with a clear error when a port is busy |
| FR-CFG-05 | Dash-specific defaults (txindex on, bloom filters on, and so on) as Core |
| FR-CFG-06 | `-daemon`, `-daemonwait`, signals, `-pid`, notify hooks |
| FR-CFG-07 | Logging: Core's categories and line prefix; operator-relevant lines (UpdateTip, ChainLock, InstantSend, mnsync transitions, masternode state, DKG results) use dashd's wording so existing log monitors work; every `debug.log` line asserted by Dash's functional tests (63 tests, R4 §2.5) appears verbatim |

### 3.20 Desktop (FR-DSK)

| ID | Requirement |
|---|---|
| FR-DSK-01 | Scope as dashwallet-desktop `CLAUDE.md` (DashPay, mobile parity, selected Core conveniences). Masternode, ProTx and governance UI is pending Q1 |
| FR-DSK-02 | The desktop engine stays on `platform-wallet`/key-wallet; Core-wallet features come from the shared library layer (FR-WAL-01) |
| FR-DSK-03 | Optional node link (US-WAL-19): JSON-RPC to a local N (or dashd) with cookie or rpcauth; full-node-only data appears only when linked |
| FR-DSK-04 | Builds for macOS, Windows and Linux. Linux is tested on agentbox; macOS and Windows QA run on pasta's machines (HANDOFF "The macOS part") |
| FR-DSK-05 | `docs/parity.md` stays the acceptance ledger for desktop stories and is updated with every merge |
| FR-DSK-06 | The desktop's SPV engine works with N as its only peer (US-LCS-07) |

---

## 4. Non-functional requirements

### 4.1 Consensus-equivalence proof obligations (NFR-CEQ)

The charter requires consensus to be **proven** identical by differential tests and differential fuzzing, plus ported functional tests and mixed-network interop. R4 §2 designs this as seven layers; each obligation below names its layer and is a release gate (REL-5). Every consensus function gets three independent kinds of evidence: fixed vectors, differential comparison on generated inputs, and real-chain replay; P2P and masternode behaviour also get mixed-network operation and fingerprint probes (R4 §2.0).

| ID | Layer | Obligation | Pass condition |
|---|---|---|---|
| NFR-CEQ-01 | L0 | **Core's vector suites.** Every consensus, key or format file in Core's `src/test/data` (`script_tests.json`, `tx_valid.json`, `tx_invalid.json`, `sighash.json`, `trivially_valid.json`, `trivially_invalid.json`, `blockfilters.json`, `key_io_*`, `base58_encode_decode.json`, `bip39_vectors.json`, `proposals_valid.json`, `proposals_invalid.json`), run by a harness that ports Core's semantics (`ParseScript`, flag names, Dash's extra script-error names) without copying Core's two harness bugs (R4 §2.1); the vectors embedded in C++ (`pow_tests`, `subsidy_tests`, `block_reward_reallocation_tests`, `evo_simplifiedmns_tests`, `evo_islock_tests`, `quorum_proofs_tests`, DIP14, the versionbits threshold, `dip0020opcodes_tests`, `checkdatasig_tests`, dashbls's Chia and legacy vectors) transcribed to files; Dash's `bitcoin-util-test.json` for dash-tx. rust-dashcore's non-Core `mnlistdiff` fixtures are regenerated from dashd first (R4 F12). Core's release `test_dash` settles disputes | 100%; no `#[ignore]` (rust-dashcore uses none of these files today, R1 §3.1) |
| NFR-CEQ-02 | L0 | **Oracle vectors from chain data and dashd**, per pinned release: every header's expected `nBits`, `getblockstats` subsidy at every height, every special transaction's payload and JSON, every CbTx, `protx diff` at checkpoints; plus generated vectors for special-tx validity per type and version, masternode-list transitions, quorum members, commitment hashes, credit pool, governance hashes and BLS encodings in both schemes. Produced through BS2's oracles (`/work/chains/{mainnet,testnet}`, ORACLE.md) | Provenance recorded per file (Core release, generator); 100% |
| NFR-CEQ-03 | L1 | **Library-level differential tests.** Every mainnet and testnet block and transaction, decoded, re-encoded and hashed by N's codecs, equals reference dashd's `getblock <h> 0` bytes, txids, block hash and `getblock <h> 2` JSON (special payloads included) | Zero mismatches over full history; rerun per release |
| NFR-CEQ-04 | L1 | **Differential fuzzing** against Core's C++ through an **oracle shim**: a test-only static library built from the pinned Core commit in a Dash Core branch (`node/oracle-shim`), linked by a dev-only Rust crate, exposing Core's parse, check, script, sighash, sigop, PoW/DGW, subsidy/payment, BLS, commitment, SML, ProTx-payload, netinfo, credit-pool and versionbits functions (priorities in R4 §2.2). libdashconsensus is a week-1 stopgap only (accept/reject for five flags). Parser targets cover every wire and import format (tx, block, headers2, all 13 special payload types × versions, commitments, ISLOCK, CLSIG, governance, sporks, CoinJoin, DKG and sig-share messages, mnlistdiff, qrinfo, addr/ExtNetInfo, v1/v2 framing, bloom and BIP158 filters, PSBT, descriptors, Core's on-disk records). Seeds: Core's vectors, bitcoin-core/qa-assets, and every script and payload harvested during replay; `messages.py` as a third implementation for codecs | Per PR: 5-minute smoke per touched target. Nightly: 8 h on 16 cores. Per release: every target ran until coverage plateau (DEC-184; formerly ≥ 24 CPU-hours per parser target and ≥ 72 per consensus target). Zero open divergences; every divergence becomes a regression vector |
| NFR-CEQ-05 | L4 | **Ported functional tests.** Dash's 264 functional tests (209 RPC methods) drive N through a compatibility runner in a Dash Core branch (`node/func-compat`: per-node binary selection, a tracked expected-failures file with reason and owner). FT-MAP maps each test to stories. Tiers (R4 §2.5): **tier 1**, 35 tests and about 60 RPCs, gates the validating-node milestone; **tier 2**, 29 DashTestFramework tests run as mixed networks (dashd node 0 holds wallet, spork key and miner; masternodes force `-disablewallet`), gates the masternode milestone; **tier 3**, fingerprint and operations tests; **tier 4**, the 58 wallet tests | Required pass set at release: tiers 1–4 complete, i.e. all ~56 Dash-specific tests and every Bitcoin-heritage test except an exclusion list pasta approves (Q10; expected: Qt, USDT tracing, multiprocess, BDB internals); expected-failures file empty |
| NFR-CEQ-06 | L2 | **Full-chain replay with per-block state comparison.** Mainnet and testnet from genesis, imported from the oracle's block files with `-assumevalid=0` and no P2P (isolating consensus), plus a run with default assumevalid (performance parity) and a P2P sync run. N writes **StateDigest v1** for every block (R4 §2.3.2: header, UTXO MuHash and block info, block stats, expected MN payees, the full `protx listdiff` state delta, CbTx fields, credit pool, active quorum sets, members per mined quorum, rotation info per cycle, deployment states, coinbase ChainLocks, asset-unlock statuses, raw block bytes); the oracle's golden digests are extracted once per pinned release. A test-only dashd patch (`-statedigestlog`, using Core's `CanonicalMNListHash`) adds digests for derived state with no RPC, such as rotation snapshots. At the tip and chosen heights: `hash_serialized_2`, full `dumptxoutset` set-diff, `getchaintips`, `verifychain 4 0`. Segment replays from validated snapshots at every activation boundary (68589 … WITHDRAWALS, tip − 50k) gate PRs | Zero divergence at any height or field. Cadence: segment replays per consensus PR; testnet full replay nightly; mainnet full replay weekly and per release |
| NFR-CEQ-07 | L3 | **Rejection parity by mutation.** About 40 mutation operators (R4 §2.4) over real templates: coinbase, CbTx, quorum commitments, ProTx (re-signed on regtest), asset lock/unlock, MNHF, lock times, limits at the boundary, merkle mutation, header fields, transaction version/type. Oracles: `getblocktemplate` proposal mode (validity without PoW, works against live testnet and mainnet), `submitblock` and P2P submission on regtest (penalties), `testmempoolaccept`. Plus `feature_block`, `tx_invalid.json` and at least one case per `bad-*` string in Core's source | 100% identical verdict, reject string, result class and peer penalty |
| NFR-CEQ-08 | L4 | **Node-local and state-dependent scenarios**, never provable by replay: ChainLock conflicts; InstantSend conflicts inside and outside IBD; superblocks with governance synced and unsynced; the assumevalid boundary; reorgs across V19 (BLS scheme), V24 activation and DIP24 cycle bases (snapshot side effects) | DIFF-STATE equal after every scenario |
| NFR-CEQ-09 | L5 | **Mixed dashd/Rust masternode networks** (R4 §2.6). **Stage A, regtest:** the tier-2 tests with alternating binaries (e.g. 3 Rust + 2 dashd; 4/4 for rotation) and all-Rust. **Stage B, devnet soak on agentbox for ≥ 2 weeks:** 20 regular MNs (10/10), 8 evonodes (4/4), wallet nodes generating payments, InstantSend, CoinJoin, governance, asset locks/unlocks and an EHF deployment, ProTx churn, V24 transactions; chaos (kills, partitions, ±60 s clock skew, rolling upgrades, disk full); plus shorter runs at 1:3, 3:1 and all-Rust covering ≥ 4 DKG cycles of every type. **Stage C:** testnet (REL-2.3). **Stage D:** mainnet per Q6 | Every Rust member is in `validMembers` of every mined commitment; no PoSe penalty on a Rust MN; recovered signatures, ChainLocks and InstantSend locks form when the threshold needs Rust shares; `quorum dkgsimerror` outcomes equal dashd's; blocks from either implementation's templates are accepted by the other; StateDigest equal on all nodes at every block |
| NFR-CEQ-10 | L5 | **Testnet soak.** N as a non-masternode full node for ≥ 30 days next to the BS2 testnet oracle, with digest comparison; Rust masternodes per REL-2.3 | Zero divergence alarms (US-NODE-22) |
| NFR-CEQ-11 | — | **Change tracking.** Weekly diff of Dash Core's release branches and develop for consensus-relevant files (validation, script, evo, llmq, chainlock, instantsend, governance, coinjoin, chainparams, net_processing); each change becomes a task; golden digests and oracle-shim branches are rebased when a pin moves | No unreviewed consensus change in any pinned release at release time |
| NFR-CEQ-12 | — | **Review.** Consensus-critical changes get at least two independent reviewers plus differential evidence before merge; performance work on consensus code must pass L1–L3 unchanged (charter, R4 §2.0) | Recorded in `reviews/` |
| NFR-CEQ-13 | L2 | **One-off chain scans** through the oracle (R4 §2.3.5): unusual transaction version/type encodings, coinbases above the bound, odd CbTx encodings, MNHF signals by bit, commitments of disabled types, and the EMBARGO-1 scan (private). They show which quirks history exercises, so those get the heaviest tests | Scans recorded; every exercised quirk has a test |

### 4.2 P2P fingerprint equivalence (NFR-FP)

The charter: a peer that spoofs its user agent should find it very hard to tell N from dashd. "Spoofs" covers the case where N presents dashd's user agent (Q4 decides the default); the requirements below assume that.

| ID | Requirement |
|---|---|
| NFR-FP-01 | Each behaviour in the list below gives the same observable result as dashd in a scripted probe (DIFF-P2P). Any deterministic difference is a release blocker |
| NFR-FP-02 | **Classifier criterion.** An adversary with a peer connected for up to 24 hours, using any combination of the probes plus timing features, cannot tell N from dashd: a gradient-boosted classifier trained on probe results and passive traces (message order, timing, sizes, error responses) reaches a **held-out AUC ≤ 0.55** (R4 §2.7), measured over ≥ 200 sessions per implementation on regtest and testnet and re-run on every release of either implementation. The same holds from a masternode peer's point of view against a Rust masternode |
| NFR-FP-03 | **Timers**: protocol timers (trickle Poisson parameters, ping every 2 min, the 6 s sync tick, govsync cadence, `getsporks` timing, DNS-seed timing, feelers every 2 min, extra peers every 5 min, the 100 ms masternode send cadence) are statistically indistinguishable (two-sample KS test, p > 0.05 over ≥ 10,000 samples). Processing latency may be lower than dashd's as long as NFR-FP-02 holds |
| NFR-FP-04 | No listening port, service or response that dashd does not have is visible from the network by default (for example, any metrics endpoint binds to loopback and is off by default) |

**Observable behaviours** (from R2 §A.2 "Notes for a Rust reimplementation", §G.1 10, and the A.3–A.5 relay rules):

1. `version` layout and values: 137 bytes, with the 32-byte mnauth challenge and the `fMasternode` byte after `relay`; addrMe carries our services with an all-zero address; addrYou zeroed for unroutable, proxied or non-addrv1 peers; subversion `/Dash Core:<version>/` without an rc suffix (plus `devnet.<name>` on devnets); `start_height`; INIT_PROTO_VERSION framing.
2. Service bits: 0x1C05 for a node, 0x1C45 for a masternode, and the pruned variants.
3. Who speaks first: outbound sends `version` at once; inbound only after receiving one.
4. After the peer's `version`: `sendaddrv2` (≥ 70223), `sendtxrcncl` only if enabled, `verack`, then `getaddr` on outbound full-relay connections only.
5. After `verack`, in this order: `mnauth` (masternode, not on probes); `sendheaders2` instead of `sendheaders` for peers with bit 11; `sendcmpct(0,1)` only if `CanRelay()`; `senddsq(1)` unless rejecting transactions; `ping`; `getheaders2`/`getheaders`; `qwatch` when applicable (live rc.3 capture, R4 F11).
6. Messages before `version`, and non-allowed messages before `verack`, are ignored.
7. Immediate `ping`; `getheaders2`/`getheaders` policy; empty `headers`/`headers2` below minimum chain work.
8. `getsporks` once per address 0.5–2 s after the handshake; `govsync(0, empty bloom)` about 8 s later, then per-object vote syncs at Core's cadence; `mempool` to outbound peers after sync; disconnect after `full-sync`.
9. BIP324: salt, garbage length distribution, short-ID gating by peer version, accepted ID ranges (unknown slots ignored silently), empty version packet, rekey interval, inbound v1 detection, outbound v2 only to bit-12 peers, v1 reconnect after early disconnect.
10. v1 framing errors: drop-and-keep vs disconnect; the 3 MiB cap.
11. Unknown commands ignored without penalty; `feefilter`, `wtxidrelay`, `reject`, `alert` and `getaddrv2` never sent and treated as unknown.
12. Misbehaviour scores and the 100-point threshold; local and manual peers handled as Core does.
13. The blocks-only violation list (`tx` disconnects on block-relay-only connections; `clsig`, `spork`, `govobj`, `isdlock` do not).
14. Size limits and penalties for `inv`, `getdata`, `addr`, `headers`, `notfound`.
15. Address relay: the 0.1/s token bucket capped at 1000, one `getaddr` answer per connection from a 21–27 h cache, 23%/1000 caps, the 2 h time penalty, relaying only small fresh `addr` messages, ignoring `getaddr` on outbound connections.
16. Trickle timers, inventory batching and rate caps; the immediate path for non-transaction invs, incl. block-relay-only peers.
17. `mempool` replies: transaction invs, matching ISDLOCK invs, the best CLSIG inv; NODE_BLOOM required.
18. `merkleblock` followed by `tx` and then `isdlock` messages.
19. `filterload`/`filteradd`/`filterclear` limits and penalties; disconnect without NODE_BLOOM.
20. `getcfilters` family limits and service requirement.
21. `headers2` encoding (MRU version list, int16 time deltas, "bit set = field present"), 8000/2000 limits, announcements.
22. `getmnlistd`/`getqrinfo` answers byte-identical; +100 for unsolicited `mnlistdiff`/`qrinfo`; +1 or silence on request errors.
23. `clsig`: +10 when unrequested or invalid.
24. Block download: in-flight limits, stalling timeouts, compact-block handling and high-bandwidth peer counts.
25. Connection behaviour: outbound counts and types, feelers disconnecting after `version`, extra peers, anchors, ADDR_FETCH disconnect rule, DNS-seed timing, disconnect linger and half-close.
26. Inactivity timeouts: 60 s for the first message, 20 minutes thereafter.
27. Devnet: the subversion must contain `devnet.<name>`.
28. Masternodes: refusing inbound until synced; 1 s eviction protection while MNAUTH is pending; at most 3 verified inbound legs per ProTx; the MNAUTH tuple; probes disconnected after MNAUTH; unverified masternode connections dropped after 5 s; `qsendrecsigs` to relay members; no `sendcmpct` on non-IQR masternode connections; no trickle between masternodes; no non-preferred-peer delay in masternode mode; DKG messages only to relay members via inv/getdata; recovered signatures pushed to peers that asked.
29. Spork relay; governance relay batched every 5 s; DSQ as inv or full push by peer version; unbalanced DSTX announced as `MSG_TX` to peers below 70241.
30. `MSG_ASSET_UNLOCK` announced by instance hash to peers ≥ 70242.
31. `platformban` relay and penalties.

### 4.3 Performance (NFR-PERF)

| ID | Requirement |
|---|---|
| NFR-PERF-01 | **Never slower than dashd on any metric below**, on identical hardware, network and comparable configuration. A slowdown introduced by a PR blocks that PR's merge |
| NFR-PERF-02 | **Measurement rules.** (a) One reference box, spec recorded (CPU model and cores, RAM, disk model, filesystem, kernel), VM CPUs pinned, nothing else running. (b) Peers: local nodes serving a recorded chain over loopback or a 1 Gbit link; for "realistic" runs, a recorded netem profile. (c) Comparable config: same `-dbcache` budget, same validation thread counts, same indexes, same assumevalid, same `-maxmempool`. (d) dashd is the official release binary of the pinned reference release (Q5), or a Guix-equivalent build when testing develop. (e) Cold runs drop the page cache; warm runs are reported separately. (f) ≥ 5 runs per metric (≥ 4 for IBD and other long scenarios; DEC-23), median and p95 with 95% confidence intervals. (g) "Slower" means N's median exceeds dashd's with p < 0.05 (Mann-Whitney) and an effect ≥ 1%. A PR regression is the same test against the previous integration head. (h) Harness and baseline from task B1; results per merged PR in STATUS.md |
| NFR-PERF-03 | **2× target** where N's own work is the bottleneck: IBD from local peers, reindex, rescan, connect latency, validation throughput, mempool, GBT, startup, CPU per block, RPC; half the RSS; no more disk. For the LLMQ metrics, which are bound by block timing and the network, the 2× target applies to the node-local processing component and end-to-end latency must not be slower |
| NFR-PERF-04 | Correctness is never traded for speed; optimizations to consensus code get full multi-reviewer scrutiny (charter) |
| NFR-PERF-05 | Performance work must not create fingerprints (NFR-FP-02) |

**Tracked metrics** (the charter's list, defined):

| # | Metric | Definition and scenario | Unit |
|---|---|---|---|
| 1 | IBD to tip, default assumevalid | Start with empty datadir → `initialblockdownload=false` at the reference tip; mainnet and testnet; local peers | wall-clock; CPU-s; peak RSS |
| 2 | IBD to tip, `-assumevalid=0` | As 1, all scripts and signatures checked | same |
| 3 | Reindex and reindex-chainstate | `-reindex` and `-reindex-chainstate` on a full mainnet datadir | wall-clock |
| 4 | Wallet rescan | `rescanblockchain` of a wallet with ≥ 100,000 transactions over full mainnet history | wall-clock |
| 5 | Per-block connect latency | ConnectBlock + flush, p50/p95, over the last 50,000 mainnet blocks (replay) and live at the tip | ms |
| 6 | Validation throughput | ECDSA signature checks/s and BLS verifications/s (single and aggregated) on fixed corpora | ops/s |
| 7 | Mempool accept | Sustained acceptance rate and p50/p95 latency with a replayed mainnet transaction stream | tx/s; ms |
| 8 | GBT time | `getblocktemplate` p50/p95 with a typical and a full (300 MB) mempool | ms |
| 9 | Startup to ready | Process start → RPC out of warmup on a synced mainnet datadir, with and without txindex/addressindex | s |
| 10 | Memory | Peak RSS during IBD; steady RSS at the tip as node, masternode, and masternode with all indexes | MiB |
| 11 | Disk footprint | Blocks + undo, chainstate, masternode/LLMQ state, each index | GiB |
| 12 | CPU per block | User + system CPU per block at the tip, masternode mode | ms |
| 13 | RPC | Latency and throughput at concurrency 1/16/64 for a fixed mix (`getblock` 2, `getrawtransaction` verbose, `getaddressutxos` on a heavy address, `protx list`, `quorum info`, `masternodelist`, `getblocktemplate`, `sendrawtransaction`) | ms; req/s |
| 14 | DKG round time | Per-phase local processing time and session completion time (Init → final commitment) | ms; s |
| 15 | Recovered-signature latency | Sign request → recovered signature available locally | ms |
| 16 | InstantSend lock latency | Transaction first seen → ISDLOCK | ms |
| 17 | ChainLock latency | Block received → CLSIG | ms |
| 18 | CoinJoin servicing | `dsa` → `dsc` success latency; sessions per hour at saturation; CPU per session | s; sessions/h; ms |

### 4.4 Security (NFR-SEC)

| ID | Requirement |
|---|---|
| NFR-SEC-01 | Memory safety: no `unsafe` outside listed, reviewed modules; every FFI dependency (for example a C X11 or BLS backend) listed with its reason |
| NFR-SEC-02 | No panic on untrusted input (network, RPC, files); proven by fuzzing. Violated internal invariants in consensus or database state abort the process, as Core's assertions do |
| NFR-SEC-03 | DoS limits equal Core's (stricter or looser limits are both fingerprints). A weakness found in Core is reported to Dash Core's security contact through pasta (responsible disclosure) and fixed in both, not silently diverged |
| NFR-SEC-04 | Secrets: zeroized; never logged (sensitive RPC arguments redacted as Core does; rust-dashcore #1122 is the kind of bug to prevent); cookie and wallet files mode 0600 |
| NFR-SEC-05 | Wallet cryptography reviewed independently; key derivation parameters at least Core's |
| NFR-SEC-06 | Supply chain: `cargo-deny`, committed `Cargo.lock` for the binaries (rust-dashcore has none today), pinned toolchains, signed release artifacts, an SBOM; reproducible builds SHOULD |
| NFR-SEC-07 | RPC binds to loopback by default; REST and ZMQ off by default, as Core |
| NFR-SEC-08 | Divergence tripwire (US-NODE-22) |
| NFR-SEC-09 | Independent security review of P2P, consensus and wallet code before the mainnet pilot and before recommending N to third parties (Q6) |

### 4.5 Resource use (NFR-RES)

| ID | Requirement |
|---|---|
| NFR-RES-01 | Peak and steady RSS ≤ dashd's in every configuration of metric 10 (target ½); `-dbcache` caps cache memory as Core's does |
| NFR-RES-02 | Disk ≤ dashd's per component for the same features (metric 11) |
| NFR-RES-03 | Bandwidth equal to dashd's for the same peers and traffic (same protocol, same relay rules) |
| NFR-RES-04 | Runs a mainnet masternode and evonode within dashd's documented minimum hardware |
| NFR-RES-05 | File descriptors and threads bounded, with Core's fallback when descriptors are short |

### 4.6 Compatibility and coexistence (NFR-CMP)

| ID | Requirement |
|---|---|
| NFR-CMP-01 | rust-dashcore's consumers (Platform `rs-dpp`, mobile FFI, Dash Evo Tool, dashwallet-desktop) are not broken gratuitously: breaking changes are batched with migration notes; persisted bincode layouts are pinned by tests (R3 §4.1) |
| NFR-CMP-02 | New code follows rust-dashcore's patterns and crate structure and lives in the same workspace (charter); MSRV policy per A1 |
| NFR-CMP-03 | SPV and wallet use of the crates stays first-class; node-only dependencies stay out of the SPV build |
| NFR-CMP-04 | In-flight upstream work is coordinated rather than duplicated: #264 (BIP324), #902 (network rewrite), #803 (DGW), #799/#1077 (MN-list root), #1136–#1138 (v24 formats), #1110/#1142 (base-sdk), #1078 (collateral lock) |
| NFR-CMP-05 | Licensing per Q7 |

### 4.7 Operability (NFR-OPS)

| ID | Requirement |
|---|---|
| NFR-OPS-01 | Crash safety without reindex (US-NODE-04); clean shutdown no slower than dashd |
| NFR-OPS-02 | Logs compatible with existing monitoring (FR-CFG-07) |
| NFR-OPS-03 | Upgrades in place across two releases (US-NODE-20) |
| NFR-OPS-04 | Operator documentation: install, configure, migrate from dashd and back, run a masternode, the allowed-differences registry, known limitations |

### 4.8 Portability (NFR-PORT)

| ID | Requirement |
|---|---|
| NFR-PORT-01 | Platform tiers per Q8. Tier 1: built, fully tested and benchmarked in CI, release artifacts. Tier 2: built and smoke-tested |
| NFR-PORT-02 | Consensus results identical on every supported platform and CPU feature set (FR-CON-15, FR-CON-33) |

---

## 5. RPC compatibility matrix

### 5.1 Classification rules

- **MUST**: implemented before Done, with a DIFF-RPC test; output identical to dashd's (FR-RPC-06).
- **SHOULD**: needed for full parity; scheduled after the MUSTs. Releasing without one needs pasta's per-item approval (Q10). **Promotion rule:** a SHOULD that a required functional test calls becomes MUST.
- **WON'T**: not implemented. N answers exactly as a standard dashd release build answers.
- **Wallet kinds** (Q2): wallet commands that Core restricts to legacy or descriptor wallets behave as Core does for each kind, incl. Core's errors.
- **Gating**: hidden and test-only commands keep Core's gating (for example regtest-only).
- **Names**: two-word commands dispatch as `method + " " + params[0]`; whitelists also match `method` + `params[0]` without a space (FR-RPC-04/05). Commands registered twice in Core (`protx info`, `protx list`, `getcoinjoininfo`: node and wallet-aware) count once.
- **Platform column**: consumers found in dashpay/platform (§5.3 key). Empty means no Platform consumer was found; it does not lower the class.

### 5.2 Totals

| Category | Commands | MUST | SHOULD | WON'T |
|---|---|---|---|---|
| control | 7 | 7 | 0 | 0 |
| network | 13 | 13 | 0 | 0 |
| blockchain | 36 | 34 | 2 | 0 |
| rawtransactions | 21 | 19 | 2 | 0 |
| util | 8 | 8 | 0 | 0 |
| mining | 7 | 7 | 0 | 0 |
| addressindex | 5 | 5 | 0 | 0 |
| zmq | 1 | 1 | 0 | 0 |
| signer | 1 | 0 | 1 | 0 |
| wallet | 71 | 65 | 6 | 0 |
| dash | 38 | 37 | 1 | 0 |
| evo | 49 | 49 | 0 | 0 |
| hidden | 32 | 21 | 10 | 1 |
| **Total** | **289** | **266** | **22** | **1** |

The categories and command list are R2b's (289 distinct names from Core's `CRPCCommand` tables). A script checked that every name is classified exactly once.

### 5.3 The Platform stable subset

Platform's Core contract was checked in dashpay/platform at `6499c680c6` (2026-10-06): Drive's Core client, rs-dapi, legacy JS DAPI, rs-sdk, and dashmate's status, setup and `dash.conf` template. R2 and R2b listed more Platform-facing commands (for example `quorum sign`, `quorum selectquorum`, the recovered-signature ZMQ topics) than the code calls today; those stay MUST through their own stories but are not in the frozen subset.

**Consumer key.** **D** Drive ABCI, RPC user `drive_consensus` (`packages/rs-drive-abci/src/rpc/core.rs`). **A** rs-dapi, RPC user `dapi` (`packages/rs-dapi/src/clients/core_client.rs`). **T** Tenderdash, RPC user `tenderdash` (whitelist only; its code is not in this repo). **Q** the quorum_list service (whitelist `quorum`, `masternode`, `getblockcount`). **M** dashmate status, doctor and stop paths on production nodes. **J** legacy JS DAPI, still in the `dapi` whitelist. **S** dashmate setup paths (local networks and devnets).

| Tier | What | Commands and topics | Rule |
|---|---|---|---|
| P-1 | Production calls | `getbestchainlock`, `getblockchaininfo`, `getblockcount`, `getblockhash`, `getblockheader` (hex and verbose), `getblock` (verbosity 0 and 2), `getrawtransaction` (raw and verbose), `sendrawtransaction`, `getrawmempool`, `getspecialtxes` (`<hash> 5 1 0 1`), `getassetunlockstatuses`, `submitchainlock`, `verifychainlock`, `protx listdiff`, `protx diff`, `quorum listextended`, `quorum info`, `quorum verify`, `quorum platformsign`, `quorum dkginfo`, `quorum dkgstatus`, `masternode status`, `masternode list` (`json <protx>`), `masternode count`, `mnsync status`, `getnetworkinfo`, `estimatesmartfee`, `ping`, `getpeerinfo` | **Frozen**: golden request/response fixtures recorded from reference dashd (regtest with dashmate's config, and testnet) run in CI; any change is breaking and needs pasta's sign-off; re-recorded only when a reference pin moves (Q5) |
| P-2 | Allowed or legacy | `getbestblockhash`, `getblockheaders`, `getblockstats`, `getmerkleblocks`, `getrawtransactionmulti`, `verifyislock`, every `quorum` and `masternode` subcommand (quorum_list) | DIFF-RPC like all MUSTs |
| P-3 | dashmate setup (local/devnet) | `mnsync next`, `quorum list`, `masternodelist status`, `protx info`, `protx register`, `protx register_evo`, `masternode outputs`, `spork show`, `sporkupdate`, `createwallet`, `getnewaddress`, `dumpprivkey`, `importprivkey`, `generatetoaddress`, `getaddressbalance`, `getaddressutxos` | Covered by US-PLAT-09 (dashmate end-to-end) |
| ZMQ | Topics | Published by dashmate and used by rs-dapi: `hashblock`, `rawblock`, `rawtx`, `rawtxlocksig`, `rawchainlock`, `rawchainlocksig`. Published, used only by legacy JS DAPI: `rawtxlock`. Legacy JS DAPI only, not published by dashmate: `hashtx`, `hashtxlock` | Frozen like P-1 (topic, body, sequence) |
| Access | dashmate's RPC users and options | Users `dashmate` (no whitelist), `quorum_list`, `dapi`, `drive_consensus`, `drive_check_tx`, `tenderdash`; composite whitelist names (`protxdiff`, `protxlistdiff`, `mnsyncstatus`, `masternodestatus`, `quorumlistextended`, `quoruminfo`, `quorumverify`, `quorumplatformsign`); `rpcwhitelistdefault=0`; low-priority users in `rpcexternaluser`; `deprecatedrpc=service`; `rpcworkqueue=64`; `rpcthreads=16` (template `packages/dashmate/templates/core/dash.conf.dot`; whitelists `packages/dashmate/configs/defaults/getBaseConfigFactory.js:68-112`) | dashmate's generated `dash.conf` works unchanged (US-PLAT-08, US-NODE-03) |

**Behaviour Platform depends on beyond JSON shape:**

- `getbestchainlock` with no ChainLock returns error **-32603**; rs-dapi treats that as "no lock".
- Drive reads `getblockchaininfo.softforks["mn_rr"]`. Core lists `softforks` among its `-deprecatedrpc` values (R2 §A.6 2), and dashmate only sets `deprecatedrpc=service`, so N must reproduce the pinned release's exact behaviour here. Whether Platform's assumption holds on that release is for pasta to raise with Platform if it does not.
- Calls outside a user's whitelist return **403**. Today rs-dapi calls `estimatesmartfee` and `masternode list`, which dashmate's `dapi` whitelist does not allow; N reproduces the 403 rather than "fixing" it. (Worth passing to the Platform team through pasta.)

### 5.4 Matrix

#### `control` (7 commands: MUST 7)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `debug` | MUST | US-NODE-12 |  | Dash alias for changing log categories; used by operators |
| `getmemoryinfo` | MUST | US-NODE-10 |  | Shape identical; allocator statistics are registry class 2 |
| `getrpcinfo` | MUST | US-DEV-06 |  | Used by FT and monitoring; thread/timing values registry class 2 |
| `help` | MUST | US-INT-14 |  | dash-cli workflows rely on it; text SHOULD be byte-identical (FR-RPC-06) |
| `logging` | MUST | US-NODE-12 |  | Runtime log control |
| `stop` | MUST | US-NODE-04 |  | Clean shutdown |
| `uptime` | MUST | US-NODE-04 |  | Monitoring |

#### `network` (13 commands: MUST 13)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `addnode` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `clearbanned` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `disconnectnode` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `getaddednodeinfo` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `getconnectioncount` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `getnettotals` | MUST | US-NODE-10 |  | Network status; used by monitoring, dashmate and FT |
| `getnetworkinfo` | MUST | US-NODE-10 | A, T, M | Network status; used by monitoring, dashmate and FT |
| `getnodeaddresses` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `getpeerinfo` | MUST | US-NODE-06 | M | Peer management; used by operators, dashmate and FT |
| `listbanned` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `ping` | MUST | US-NODE-06 | T, M | Peer management; used by operators, dashmate and FT |
| `setban` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |
| `setnetworkactive` | MUST | US-NODE-06 |  | Peer management; used by operators, dashmate and FT |

#### `blockchain` (36 commands: MUST 34, SHOULD 2)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `getbestblockhash` | MUST | US-INT-15 | J, S | Core chain queries used by every integrator |
| `getbestchainlock` | MUST | US-INT-04, US-PLAT-02 | D, A, M | ChainLock finality for exchanges and Platform |
| `getblock` | MUST | US-INT-15 | A, J | Core chain queries used by every integrator |
| `getblockchaininfo` | MUST | US-INT-15 | D, A, M | Core chain queries used by every integrator |
| `getblockcount` | MUST | US-INT-15 | A, Q, S | Core chain queries used by every integrator |
| `getblockfilter` | MUST | US-INT-07 |  | BIP157 filters over RPC |
| `getblockfrompeer` | SHOULD | US-NODE-17 |  | Rare; fetches a specific block for fork analysis |
| `getblockhash` | MUST | US-INT-15 | D, A | Core chain queries used by every integrator |
| `getblockhashes` | MUST | US-INT-03 |  | timestampindex; explorers (Insight) |
| `getblockheader` | MUST | US-INT-15 | D, A | Core chain queries used by every integrator |
| `getblockheaders` | MUST | US-INT-15 | J | Core chain queries used by every integrator |
| `getblockstats` | MUST | US-INT-15 | J | Core chain queries used by every integrator |
| `getchaintips` | MUST | US-INT-15 |  | Core chain queries used by every integrator |
| `getchaintxstats` | MUST | US-INT-15 |  | Core chain queries used by every integrator |
| `getcreditpoolinfo` | MUST | US-PLAT-04 |  | Platform credits |
| `getdeploymentinfo` | MUST | US-NODE-14 |  | Deployment state |
| `getdifficulty` | MUST | US-INT-15 |  | Core chain queries used by every integrator |
| `getmempoolancestors` | MUST | US-INT-15 |  | Mempool inspection and persistence |
| `getmempooldescendants` | MUST | US-INT-15 |  | Mempool inspection and persistence |
| `getmempoolentry` | MUST | US-INT-15 |  | Mempool inspection and persistence |
| `getmempoolinfo` | MUST | US-INT-15 |  | Mempool inspection and persistence |
| `getmerkleblocks` | MUST | US-INT-11 | J | Dash SPV helper; merkleblocks for a bloom filter |
| `getrawmempool` | MUST | US-INT-15 | A, J | Mempool inspection and persistence |
| `getspecialtxes` | MUST | US-INT-12 | D | Special-tx inspection |
| `getspentinfo` | MUST | US-INT-03 |  | spentindex |
| `gettxout` | MUST | US-INT-08 |  | UTXO queries; `gettxoutsetinfo` also feeds REL-1 |
| `gettxoutproof` | MUST | US-INT-11 |  | Merkle proofs |
| `gettxoutsetinfo` | MUST | US-INT-08 |  | UTXO queries; `gettxoutsetinfo` also feeds REL-1 |
| `gettxspendingprevout` | MUST | US-INT-15 |  | Mempool spend lookup; cheap once the mempool exists |
| `preciousblock` | MUST | US-NODE-17 |  | Fork handling |
| `pruneblockchain` | MUST | US-NODE-05 |  | Manual pruning |
| `savemempool` | MUST | US-INT-15 |  | Mempool inspection and persistence |
| `scanblocks` | SHOULD | US-INT-07 |  | Experimental descriptor scan over block filters; low use |
| `scantxoutset` | MUST | US-INT-08 |  | UTXO queries; `gettxoutsetinfo` also feeds REL-1 |
| `verifychain` | MUST | US-NODE-04 |  | Database verification |
| `verifytxoutproof` | MUST | US-INT-11 |  | Merkle proofs |

#### `rawtransactions` (21 commands: MUST 19, SHOULD 2)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `analyzepsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `combinepsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `combinerawtransaction` | MUST | US-WAL-09 |  | Multisig signing workflows |
| `converttopsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `createpsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `createrawtransaction` | MUST | US-DEV-05 |  | Raw transaction tools |
| `decodepsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `decoderawtransaction` | MUST | US-DEV-05 |  | Raw transaction tools |
| `decodescript` | MUST | US-DEV-05 |  | Raw transaction tools |
| `finalizepsbt` | MUST | US-WAL-09 |  | PSBT workflows (desktop already has a PSBT subset) |
| `fundrawtransaction` | MUST | US-WAL-05 |  | Wallet funding of raw transactions |
| `getassetunlockstatuses` | MUST | US-PLAT-04 | D | Platform withdrawals |
| `getislocks` | MUST | US-INT-04 |  | Raw InstantSend locks |
| `getrawtransaction` | MUST | US-INT-01 | D, A, S | Transaction lookup |
| `getrawtransactionmulti` | MUST | US-INT-01 | J | Batch lookup |
| `gettxchainlocks` | MUST | US-INT-04 |  | ChainLock status per transaction |
| `joinpsbts` | SHOULD | US-WAL-09 |  | Rare PSBT helpers |
| `sendrawtransaction` | MUST | US-INT-10 | D, A, S | Broadcast |
| `signrawtransactionwithkey` | MUST | US-WAL-09 |  | Offline signing |
| `testmempoolaccept` | MUST | US-INT-10 |  | Policy testing |
| `utxoupdatepsbt` | SHOULD | US-WAL-09 |  | Rare PSBT helpers |

#### `util` (8 commands: MUST 8)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `createmultisig` | MUST | US-WAL-09 |  | Multisig |
| `deriveaddresses` | MUST | US-WAL-07 |  | Descriptor tools; cheap with the descriptor engine |
| `estimatesmartfee` | MUST | US-INT-09 | A (blocked by dashmate whitelist) | Fee estimation |
| `getdescriptorinfo` | MUST | US-WAL-07 |  | Descriptor tools; cheap with the descriptor engine |
| `getindexinfo` | MUST | US-DEV-06 |  | Index status; FT |
| `signmessagewithprivkey` | MUST | US-WAL-10 |  | Message signing |
| `validateaddress` | MUST | US-INT-14 |  | Address validation |
| `verifymessage` | MUST | US-WAL-10 |  | Message signing |

#### `mining` (7 commands: MUST 7)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `getblocktemplate` | MUST | US-MINE-01 |  | Pools |
| `getmininginfo` | MUST | US-MINE-03 |  | Mining info and prioritisation |
| `getnetworkhashps` | MUST | US-MINE-03 |  | Mining info and prioritisation |
| `getprioritisedtransactions` | MUST | US-MINE-03 |  | Mining info and prioritisation |
| `prioritisetransaction` | MUST | US-MINE-03 |  | Mining info and prioritisation |
| `submitblock` | MUST | US-MINE-02 |  | Block submission |
| `submitheader` | MUST | US-MINE-02 |  | Block submission |

#### `addressindex` (5 commands: MUST 5)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `getaddressbalance` | MUST | US-INT-02 | S | addressindex; explorers and Insight |
| `getaddressdeltas` | MUST | US-INT-02 |  | addressindex; explorers and Insight |
| `getaddressmempool` | MUST | US-INT-02 |  | addressindex; explorers and Insight |
| `getaddresstxids` | MUST | US-INT-02 |  | addressindex; explorers and Insight |
| `getaddressutxos` | MUST | US-INT-02 | S | addressindex; explorers and Insight |

#### `zmq` (1 commands: MUST 1)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `getzmqnotifications` | MUST | US-INT-05 |  | ZMQ configuration |

#### `signer` (1 commands: SHOULD 1)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `enumeratesigners` | SHOULD | US-WAL-09 |  | External signers (HWI protocol); Dash use is near zero |

#### `wallet` (71 commands: MUST 65, SHOULD 6)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `abandontransaction` | MUST | US-WAL-08 |  | Wallet repair |
| `abortrescan` | MUST | US-WAL-08 |  | Wallet repair |
| `addmultisigaddress` | MUST | US-WAL-09 |  | Legacy-kind wallets; Core's error on descriptor wallets (Q2) |
| `backupwallet` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `createwallet` | MUST | US-WAL-01 | S | Wallet lifecycle |
| `dumphdinfo` | MUST | US-WAL-02 |  | Dash HD; legacy-kind wallets (Q2) |
| `dumpprivkey` | MUST | US-WAL-07 | S | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `dumpwallet` | MUST | US-WAL-07 |  | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `encryptwallet` | MUST | US-WAL-06 |  | Encryption and locking (incl. mixing-only unlock) |
| `getaddressesbylabel` | MUST | US-WAL-03 |  | Receiving and labels |
| `getaddressinfo` | MUST | US-WAL-03 |  | Receiving and labels |
| `getbalance` | MUST | US-WAL-04 |  | Balances and history |
| `getbalances` | MUST | US-WAL-04 |  | Balances and history |
| `getnewaddress` | MUST | US-WAL-03 | S | Receiving and labels |
| `getrawchangeaddress` | MUST | US-WAL-03 |  | Receiving and labels |
| `getreceivedbyaddress` | MUST | US-WAL-04 |  | Balances and history |
| `getreceivedbylabel` | MUST | US-WAL-04 |  | Balances and history |
| `gettransaction` | MUST | US-WAL-04 |  | Balances and history |
| `getunconfirmedbalance` | MUST | US-WAL-04 |  | Deprecated in Core but still used; trivial |
| `getwalletinfo` | MUST | US-WAL-04 |  | Balances and history |
| `importaddress` | MUST | US-WAL-07 |  | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `importdescriptors` | MUST | US-WAL-07 |  | Descriptor import/export |
| `importelectrumwallet` | SHOULD | US-IMP-10 |  | Dash-specific; Electrum-Dash is little used |
| `importmulti` | MUST | US-WAL-07 |  | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `importprivkey` | MUST | US-WAL-07 | S | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `importprunedfunds` | MUST | US-WAL-07 |  | Pruned-node funds |
| `importpubkey` | MUST | US-WAL-07 |  | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `importwallet` | MUST | US-WAL-07 |  | Key import/export; legacy-kind semantics, Core's errors on descriptor wallets (Q2) |
| `keypoolrefill` | MUST | US-WAL-03 |  | Receiving and labels |
| `listaddressbalances` | MUST | US-WAL-04 |  | Balances and history |
| `listaddressgroupings` | MUST | US-WAL-04 |  | Balances and history |
| `listdescriptors` | MUST | US-WAL-07 |  | Descriptor import/export |
| `listlabels` | MUST | US-WAL-03 |  | Receiving and labels |
| `listlockunspent` | MUST | US-WAL-05 |  | Spending and coin control |
| `listreceivedbyaddress` | MUST | US-WAL-04 |  | Balances and history |
| `listreceivedbylabel` | MUST | US-WAL-04 |  | Balances and history |
| `listsinceblock` | MUST | US-WAL-04 |  | Balances and history |
| `listtransactions` | MUST | US-WAL-04 |  | Balances and history |
| `listunspent` | MUST | US-WAL-05 |  | Spending and coin control |
| `listwalletdir` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `listwallets` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `loadwallet` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `lockunspent` | MUST | US-WAL-05 |  | Spending and coin control |
| `migratewallet` | SHOULD | US-WAL-11 |  | Experimental in Core; legacy-kind → descriptor conversion (Q2) |
| `newkeypool` | MUST | US-WAL-03 |  | Receiving and labels |
| `removeprunedfunds` | MUST | US-WAL-07 |  | Pruned-node funds |
| `rescanblockchain` | MUST | US-WAL-08 |  | Wallet repair |
| `restorewallet` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `send` | MUST | US-WAL-05 |  | Spending and coin control |
| `sendall` | MUST | US-WAL-05 |  | Spending and coin control |
| `sendmany` | MUST | US-WAL-05 |  | Spending and coin control |
| `sendtoaddress` | MUST | US-WAL-05 |  | Spending and coin control |
| `setcoinjoinamount` | MUST | US-CJ-02 |  | CoinJoin settings |
| `setcoinjoinrounds` | MUST | US-CJ-02 |  | CoinJoin settings |
| `sethdseed` | MUST | US-WAL-02 |  | Dash HD; legacy-kind wallets (Q2) |
| `setlabel` | MUST | US-WAL-03 |  | Receiving and labels |
| `settxfee` | MUST | US-WAL-05 |  | Spending and coin control |
| `setwalletflag` | SHOULD | US-WAL-14 |  | Rarely used flags |
| `signmessage` | MUST | US-WAL-10 |  | Message signing |
| `signrawtransactionwithwallet` | MUST | US-WAL-09 |  | Wallet signing (also shared-MN flows) |
| `simulaterawtransaction` | SHOULD | US-WAL-13 |  | Rare |
| `unloadwallet` | MUST | US-WAL-01 |  | Wallet lifecycle |
| `upgradetohd` | MUST | US-WAL-02 |  | Dash HD; legacy-kind wallets (Q2) |
| `upgradewallet` | SHOULD | US-WAL-11 |  | Core wallet-version upgrades; little meaning for N's own format |
| `walletcreatefundedpsbt` | MUST | US-WAL-09 |  | PSBT |
| `walletdisplayaddress` | SHOULD | US-WAL-09 |  | External-signer builds only; near-zero Dash use |
| `walletlock` | MUST | US-WAL-06 |  | Encryption and locking (incl. mixing-only unlock) |
| `walletpassphrase` | MUST | US-WAL-06 |  | Encryption and locking (incl. mixing-only unlock) |
| `walletpassphrasechange` | MUST | US-WAL-06 |  | Encryption and locking (incl. mixing-only unlock) |
| `walletprocesspsbt` | MUST | US-WAL-09 |  | PSBT |
| `wipewallettxes` | MUST | US-WAL-08 |  | Wallet repair |

#### `dash` (38 commands: MUST 37, SHOULD 1)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `coinjoin` | MUST | US-CJ-01 |  | Mixing control |
| `coinjoin reset` | MUST | US-CJ-01 |  | Mixing control |
| `coinjoin start` | MUST | US-CJ-01 |  | Mixing control |
| `coinjoin status` | MUST | US-CJ-01 |  | Mixing control |
| `coinjoin stop` | MUST | US-CJ-01 |  | Mixing control |
| `coinjoinsalt` | MUST | US-CJ-04, US-CJ-06 |  | Salt management; needed to keep Core-mixed coins mixed |
| `coinjoinsalt generate` | MUST | US-CJ-04, US-CJ-06 |  | Salt management; needed to keep Core-mixed coins mixed |
| `coinjoinsalt get` | MUST | US-CJ-04, US-CJ-06 |  | Salt management; needed to keep Core-mixed coins mixed |
| `coinjoinsalt set` | MUST | US-CJ-04, US-CJ-06 |  | Salt management; needed to keep Core-mixed coins mixed |
| `getcoinjoininfo` | MUST | US-CJ-01, US-CJS-06 |  | Client and masternode forms (two registrations, one name) |
| `getgovernanceinfo` | MUST | US-GOV-01 |  | Governance browsing |
| `getsuperblockbudget` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject check` | MUST | US-GOV-02 |  | Proposal authoring |
| `gobject count` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject deserialize` | MUST | US-GOV-02 |  | Proposal authoring |
| `gobject diff` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject get` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject getcurrentvotes` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject list` | MUST | US-GOV-01 |  | Governance browsing |
| `gobject list-prepared` | MUST | US-GOV-03 |  | Proposal submission |
| `gobject prepare` | MUST | US-GOV-03 |  | Proposal submission |
| `gobject submit` | MUST | US-GOV-03 |  | Proposal submission |
| `gobject vote-alias` | MUST | US-GOV-04 |  | Voting |
| `gobject vote-many` | MUST | US-GOV-04 |  | Voting |
| `masternode` | MUST | US-MN-09 | Q (all subcommands) | Masternode list views |
| `masternode connect` | SHOULD | US-DEV-03 |  | Debug helper; promoted to MUST if a required FT uses it |
| `masternode count` | MUST | US-MN-09 | M | Masternode list views |
| `masternode list` | MUST | US-MN-09 | A, T | Masternode list views |
| `masternode outputs` | MUST | US-MN-01 | S | Collateral discovery |
| `masternode payments` | MUST | US-MN-08 |  | Payment schedule |
| `masternode status` | MUST | US-MN-04 | A, T, M | Local masternode state |
| `masternode winners` | MUST | US-MN-08 |  | Payment schedule |
| `masternodelist` | MUST | US-MN-09 | S | Masternode list views |
| `mnsync` | MUST | US-NODE-02 | D, A, M, S | Dash sync status |
| `spork` | MUST | US-NODE-13 | S | Spork state |
| `sporkupdate` | MUST | US-DEV-04 | S | Devnets and FT |
| `voteraw` | MUST | US-GOV-05 |  | External-signature voting |

#### `evo` (49 commands: MUST 49)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `bls` | MUST | US-MN-02 |  | Operator keys |
| `bls fromsecret` | MUST | US-MN-02 |  | Operator keys |
| `bls generate` | MUST | US-MN-02 |  | Operator keys |
| `getchainlockbyheight` | MUST | US-PLAT-05 |  | Historical ChainLocks |
| `getquorumproofchain` | MUST | US-PLAT-05 |  | DASHNC02 quorum proofs |
| `protx` | MUST | US-MN-09, US-MN-17 |  | Masternode details (node and wallet-aware registrations) |
| `protx diff` | MUST | US-MN-10, US-PLAT-02 | A, J | List diffs |
| `protx info` | MUST | US-MN-09, US-MN-17 | S | Masternode details (node and wallet-aware registrations) |
| `protx list` | MUST | US-MN-09, US-MN-17 | rs-sdk tests | Masternode details (node and wallet-aware registrations) |
| `protx listdiff` | MUST | US-MN-10, US-PLAT-02 | D | List diffs |
| `protx register` | MUST | US-MN-03 | S | Registration |
| `protx register_evo` | MUST | US-PLAT-01 | S | Evonode registration and service updates |
| `protx register_fund` | MUST | US-MN-03 |  | Registration |
| `protx register_fund_evo` | MUST | US-PLAT-01 |  | Evonode registration and service updates |
| `protx register_prepare` | MUST | US-MN-03 |  | Registration |
| `protx register_prepare_evo` | MUST | US-PLAT-01 |  | Evonode registration and service updates |
| `protx register_submit` | MUST | US-MN-03 |  | Registration |
| `protx revoke` | MUST | US-MN-07 |  | Operator revoke |
| `protx shared_combine` | MUST | US-SHR-01, US-SHR-02 |  | Shared masternode registration (V24) |
| `protx shared_dissolve` | MUST | US-SHR-05 |  | Dissolution (V24) |
| `protx shared_dissolve_prepare` | MUST | US-SHR-05 |  | Dissolution (V24) |
| `protx shared_register_prepare` | MUST | US-SHR-01, US-SHR-02 |  | Shared masternode registration (V24) |
| `protx shared_sign` | MUST | US-SHR-01, US-SHR-02 |  | Shared masternode registration (V24) |
| `protx shared_update_registrar_prepare` | MUST | US-SHR-04 |  | Joint key change (V24) |
| `protx shared_update_share` | MUST | US-SHR-03 |  | Share reward script (V24) |
| `protx update_registrar` | MUST | US-MN-06 |  | Owner updates |
| `protx update_service` | MUST | US-MN-05 |  | Operator updates |
| `protx update_service_evo` | MUST | US-PLAT-01 |  | Evonode registration and service updates |
| `quorum` | MUST | US-MN-11, US-PLAT-02 | Q (all subcommands) | Quorum inspection |
| `quorum dkginfo` | MUST | US-MN-11, US-PLAT-02 | M | Quorum inspection |
| `quorum dkgsimerror` | MUST | US-DEV-03 |  | Regtest-only error injection used by FT |
| `quorum dkgstatus` | MUST | US-MN-11, US-PLAT-02 | M | Quorum inspection |
| `quorum getdata` | MUST | US-MN-12 |  | Quorum data requests; used by FT `p2p_quorum_data` |
| `quorum getrecsig` | MUST | US-PLAT-03 |  | Threshold signing for Platform |
| `quorum hasrecsig` | MUST | US-PLAT-03 |  | Threshold signing for Platform |
| `quorum info` | MUST | US-MN-11, US-PLAT-02 | D, T, S | Quorum inspection |
| `quorum isconflicting` | MUST | US-PLAT-03 |  | Threshold signing for Platform |
| `quorum list` | MUST | US-MN-11, US-PLAT-02 | S | Quorum inspection |
| `quorum listextended` | MUST | US-MN-11, US-PLAT-02 | D, M | Quorum inspection |
| `quorum memberof` | MUST | US-MN-11, US-PLAT-02 |  | Quorum inspection |
| `quorum platformsign` | MUST | US-PLAT-03 | T | Threshold signing for Platform |
| `quorum rotationinfo` | MUST | US-PLAT-05 |  | Rotation info |
| `quorum selectquorum` | MUST | US-PLAT-03 |  | Threshold signing for Platform |
| `quorum sign` | MUST | US-PLAT-03 |  | Threshold signing for Platform |
| `quorum verify` | MUST | US-PLAT-03 | T | Threshold signing for Platform |
| `submitchainlock` | MUST | US-PLAT-05 | D | Platform submits ChainLocks |
| `verifychainlock` | MUST | US-PLAT-05 | D | Signature verification |
| `verifyislock` | MUST | US-PLAT-05 | D (no production caller) | Signature verification |
| `verifyquorumproofchain` | MUST | US-PLAT-05 |  | DASHNC02 quorum proofs |

#### `hidden` (32 commands: MUST 21, SHOULD 10, WON'T 1)

| Command | Class | Stories | Platform | Reason |
|---|---|---|---|---|
| `addconnection` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `addpeeraddress` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `cleardiscouraged` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `dumptxoutset` | MUST | US-NODE-16 |  | Also the full-UTXO comparison tool of REL-1 |
| `echo` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `echoipc` | WON'T | — |  | Multiprocess builds only (NG-7); N returns the same error as a standard dashd build |
| `echojson` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `estimaterawfee` | SHOULD | US-INT-09 |  | Unstable debug view of the estimator |
| `evodb repair` | SHOULD | US-DEV-06 |  | Checks for Core's evodb layout; N offers equivalent checks of its own store |
| `evodb verify` | SHOULD | US-DEV-06 |  | Checks for Core's evodb layout; N offers equivalent checks of its own store |
| `generate` | MUST | US-MINE-05 |  | Removed in Core; returns Core's identical "removed" error |
| `generateblock` | MUST | US-MINE-05 |  | Regtest mining |
| `generatetoaddress` | MUST | US-MINE-05 | S | Regtest mining |
| `generatetodescriptor` | MUST | US-MINE-05 |  | Regtest mining |
| `getaddrmaninfo` | SHOULD | US-NODE-06 |  | Experimental addrman views |
| `getrawaddrman` | SHOULD | US-NODE-06 |  | Experimental addrman views |
| `invalidateblock` | MUST | US-NODE-17 |  | Fork handling and FT |
| `mnauth` | MUST | US-DEV-03 |  | Dash test hooks used by FT |
| `mockscheduler` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `protx register_fund_legacy` | SHOULD | US-MN-16 |  | Deprecated legacy-BLS variants; consensus-invalid after V24; MUST if a required FT uses them |
| `protx register_legacy` | SHOULD | US-MN-16 |  | Deprecated legacy-BLS variants; consensus-invalid after V24; MUST if a required FT uses them |
| `protx register_prepare_legacy` | SHOULD | US-MN-16 |  | Deprecated legacy-BLS variants; consensus-invalid after V24; MUST if a required FT uses them |
| `protx update_registrar_legacy` | SHOULD | US-MN-16 |  | Deprecated legacy-BLS variants; consensus-invalid after V24; MUST if a required FT uses them |
| `reconsiderblock` | MUST | US-NODE-17 |  | Fork handling and FT |
| `sendmsgtopeer` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `setmnthreadactive` | MUST | US-DEV-03 |  | Dash test hooks used by FT |
| `setmocktime` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `submitpackage` | SHOULD | US-INT-10 |  | Regtest-only package submission |
| `syncwithvalidationinterfacequeue` | MUST | US-DEV-03 |  | Test-only hooks used by FT (regtest-gated where Core gates them) |
| `waitforblock` | MUST | US-DEV-06 |  | Used by FT and scripts |
| `waitforblockheight` | MUST | US-DEV-06 |  | Used by FT and scripts |
| `waitfornewblock` | MUST | US-DEV-06 |  | Used by FT and scripts |
---

## 6. Gap analysis

### 6.1 Summary by area

Size uses R4's scale (one strong implementer, tests included): **S** ≤ 1 week, **M** 1–3 weeks, **L** 1–2 months, **XL** more than 2 months or research-grade exactness work; XL items need a design phase (PROCESS.md). Sizes order the plan; they are not commitments, and A1 and PLAN refine them. R4 §1.14 has the per-function breakdown.

| # | Area | What exists (R1, R3, R5, desktop) | What is missing | Size |
|---|---|---|---|---|
| 1 | Type stack and base-sdk | BLS/EdDSA via `dash-pkc` in `dashcore-crypto`; #1110/#1142 in flight (R3 §2) | Re-pin; hash, Network/Devnet, params, primitives, P2P codec adoption; one codec instead of two (R3 §4 critical path ≈ 10–12 weeks, long tail 8–12 more) | L |
| 2 | V24 wire formats | Unknown-type passthrough, CbTx v4, ProUp* v3 in PRs #1136–#1138 | Types 10–12, ProRegTx v3 (DIP-26 payouts, shares, join sigs), AssetLock/Unlock v2 and the stable txid, payload-length handling (R1 §2.1). Also breaks production SPV wallets at V24 activation (R5 R-1) | M |
| 3 | Bitcoin leftovers | — | X11 `BlockHash` unconditional; opcode table; `bitcoinconsensus`; Bitcoin params; inventory numbers; size limits; protocol version; segwit/taproot removal (PR #400) (R1 §7) | M |
| 4 | Script interpreter, sighash, sigops | Script types, legacy sighash without FindAndDelete/CODESEPARATOR (R1 §2.1) | A Dash interpreter with DIP0020 and CHECKDATASIG, full sighash, sigop counting; all Core vectors | L |
| 5 | Transaction and block validation | Merkle helper (unused), PoW check against own bits | CheckTransaction/CheckBlock, contextual checks, ConnectBlock order, reject reasons | L |
| 6 | Difficulty | X11 (C FFI); DGW port only in draft PR #803 | BTC retarget, KGW (floating point), DGW, min-difficulty rules, 68589 tolerance | M |
| 7 | Subsidy, payments, block value | — | Subsidy eras (floating point), MN payment schedule, Platform share, superblock limits, payee validation | M |
| 8 | Deployments | `v20_activation_height()` only | 15 buried deployments, BIP9/EHF state machine with dynamic thresholds | M |
| 9 | Chainstate | SPV header chain; ChainWork code unused (R1 §3.2) | Block index, UTXO cache, undo, reorg, assumevalid, ChainLock enforcement, crash consistency | XL |
| 10 | Storage and block files | dash-spv flat segment store (R1 §2.4) | Node-grade store, block/undo storage, pruning, migrations | L |
| 11 | Deterministic masternode list | SML only (R1 §2.2) | DML state machine, unique properties, PoSe, payee selection, history access | XL |
| 12 | SML roots, mnlistdiff/qrinfo serving | Parse and verify (client side); the SML entry hash and MN/quorum roots are exact (R4: 15/15 Core vectors, 6/6 mainnet diffs), but unwired, and `from_diff` stores the wrong root (R1 §2.2) | CbTx root checks, diff and qrinfo builders byte-identical for every version band | L |
| 13 | Credit pool, asset lock/unlock | Payload decode (v1) | Validation, limits per era, pool state, v2 | M |
| 14 | MNHF/EHF | Payload parse | Signal manager, versionbits gating, MN signing | M |
| 15 | BLS | Both encodings, legacy hash-to-G2, secure aggregation, threshold and IES in `dash-pkc` | Fixes R4 found (unreduced secure-aggregation weight; IES key derived from the legacy encoding where Core uses basic), Dash wrapper semantics, the per-block scheme rule, conformance proof (vector provenance undocumented, R3 §1.2) | L |
| 16 | LLMQ: selection, DKG, commitments | LLMQ params; scoring and modifier; rotation reconstruction from peer-supplied snapshots in dash-spv, accurate post-V20 (R4 §1.9.3), with open issues #942/#954/#956/#957 | Snapshot building and skip lists, pre-V19/V20 rules, skip mode 2, swallowed `AddMN` exceptions, the collateral tie-break; DKG (all phases); commitment mining and validation; quorum manager | XL |
| 17 | Signing and signature shares | — | Sig-share protocol, recovery, recovered-signature store, Platform/EHF users | L |
| 18 | ChainLocks | Verification (dash-spv) | Signing, enforcement, conflict marking, CbTx best-CL | L |
| 19 | InstantSend | Verification (dash-spv) | Signing, conflict resolution, mempool integration, lock store | L |
| 20 | Governance | None in rust-dashcore; types in base-sdk `dash-primitives`; desktop parked branch `m3/r2-governance` | Objects, votes, sync, validation, triggers, superblocks, RPCs; not verifiable by replay (R4 F3) | XL |
| 21 | CoinJoin server | — | Server, DSTX/DSQ relay, V24 rebalance | L |
| 22 | CoinJoin client | Desktop M3 client in `dw-engine` (HANDOFF); key-wallet CoinJoin accounts | Shared library client, Core-compatible rounds and salt, RPCs | M |
| 23 | Sporks | — | Messages, signatures per network, hardened mainnet values | S |
| 24 | Masternode sync, metadata, netfulfilled | — | State machine and persistence | M |
| 25 | Active masternode | — | MNAUTH, masternode connections, quorum connections, roles, states | L |
| 26 | P2P | v1 transport; 42 of 74 messages; outbound SPV peer manager; BIP324 in PR #264; rewrite in draft #902 | Inbound, connection manager, addrman, relay, serving, compact blocks, misbehaviour, Tor/I2P/CJDNS | XL |
| 27 | Fingerprint equivalence | Very distinct today (R1 §2.3) | The 31 behaviours of §4.2 and the classifier gate | L |
| 28 | Mempool and policy | Wallet-only tracker (R1 §2.4) | Acceptance, policy, limits, eviction, orphans, persistence | L |
| 29 | Fee estimation | Static presets (500 duff/kB economy is below relay minimum) | Core's estimator | M |
| 30 | Mining and GBT | — | Assembly, coinbase, GBT, submit, regtest mining | M |
| 31 | Indexes | Client-side BIP158 construction | txindex, address, spent, timestamp, blockfilter (server), coinstats | L |
| 32 | RPC server | `rpc-json` (158 types, ~90 reusable as response schema); blocking client | HTTP server, auth, dispatch, 289 commands with identical output | XL |
| 33 | ZMQ | — | 19 publishers | S |
| 34 | REST | — | All endpoints | S |
| 35 | CLI | Blocking `rpc-client` | dash-cli equivalent and tool subcommands | M |
| 36 | Config and datadir | clap flags in dash-spv | About 250 options, `dash.conf` semantics, datadir layout | M |
| 37 | Node wallet | key-wallet: HD paths, pools, tx routing, builder, asset locks (R1 §2.5) | Encryption, wallet kinds, descriptors, imported keys, PSBT, multisig, ProTx and governance builders, CoinJoin, fee estimation, rescan and rollback, RPC layer | XL |
| 38 | Wallet import | Desktop `dw-compat` (BIP39 quirks, crypter, dump, SQLite with mnemonic, descriptors; ~2.3k lines) | BDB 4.8 reader, SQLite without mnemonic, all records, move into rust-dashcore | M |
| 39 | Datadir import | — | LevelDB reader, block index, chainstate (XOR), evodb incl. legacy formats | M |
| 40 | Verification infrastructure | dashd 23.1 integration harness and `masternode_network.rs` in dash-spv (R1 §4, R4 §2.6); 17 honggfuzz targets; BS2 oracles in progress | R4's first briefs V1–V9: vector crate (and regenerated SML fixtures, F12), oracle shim, StateDigest v1 extractor and comparator, state-digest dashd patch, chain scans, functional-test compatibility runner, mutation engine, mixed-network harness, fingerprint lab | XL |
| 41 | Benchmark harness | `dash-spv-bench` (SPV); B1 in progress | Side-by-side node harness for 18 metrics, per-PR tracking | M |
| 42 | Platform contract | Client methods in `rpc-client` | Frozen subset (§5.3), fixtures, dashmate integration | M |
| 43 | Packaging and release | — | Signed artifacts per tier, service files, docs | M |
| 44 | Desktop app | M0–M3 merged: engine, vault, both UIs, Core tools, CoinJoin client (HANDOFF) | DashPay (not started), remaining iOS parity (60 not started), Windows build, GUI QA against a real node, macOS verification (no Mac on agentbox) | XL |

### 6.2 Top risks

| # | Risk | Why it matters | Mitigation | Trigger to escalate |
|---|---|---|---|---|
| R-0 | **EMBARGO-1** (an unfixed Dash Core security issue; ~/private/embargo/EMBARGO-1.md) | N must reproduce Core's behaviour exactly; publishing details before a fix endangers the network | Replicate Core's behaviour with a test (FR-CON-34; PLAN §7 handling); details stay in the private file; nothing from `node/integration` goes public until pasta clears it (Q13) | Any sign the issue is public, or an upstream fix that changes the rule |
| R-1 | **V24 is a moving target and already live on testnet's schedule.** V24's start time on testnet is 2026-09-24; mainnet is `NEVER_ACTIVE`; rc.3 and develop move EvoNode shares and multi-payouts behind a new `evo_shares` deployment (bit 14), and the V24 credit-pool limit differs between v23.1.8, develop and rc.3 (R4 §1.12). rust-dashcore cannot decode types 10–12 or ProRegTx v3 (R1 §2.1, R5 R-1), which also endangers iOS, Dash Evo Tool and the desktop in production once V24 activates | Consensus divergence; broken SPV wallets | Land the v24 wire formats first (gap 2), once, in one place (R3 §4.1 principle 3); pin reference releases per network (Q5); the consensus-change watcher (NFR-CEQ-11) | V24 activation signal on testnet, or a new Core rc that touches consensus |
| R-2 | **Legacy BLS hash-to-G2** (`ep2_map_legacy`) is non-standard; no mainstream crate has it. `dash-pkc` claims bug-for-bug compatibility, but its vectors' provenance is undocumented (R3 §1.2) | Pre-V19 blocks, commitments and ChainLocks verify differently → chain split | Oracle vectors from dashbls for hash-to-G2 and every encoding path; differential fuzzing of decode/verify in both schemes; full replay with `-assumevalid=0` | Any vector mismatch |
| R-3 | **Floating-point consensus.** The subsidy is computed in doubles from the previous block's bits; mainnet only checks the upper bound, so a subsidy that is too high passes replay silently. KGW feeds only the ±50% tolerance (R4 F4) | Wrong subsidy → wrong templates and rejected blocks; platform-dependent results | Compare the subsidy at every height against reference dashd, fuzz the plausible range, run on every tier-1 target (FR-CON-15) | A mismatch at any height or on any platform |
| R-4 | **Rotation snapshots are side effects** of connecting cycle-base blocks, are not reverted on reorg, and depend on the enabled-MN count (R2 §A.4 12.5) | Wrong quorum members → failed DKGs, PoSe bans, rejected IS locks | Oracle vectors for members per cycle; replay comparison of `quorum listextended`; reorg-across-cycle scenarios (NFR-CEQ-08) | Member mismatch at any cycle |
| R-5 | **State-dependent block validity** (governance, masternode sync, ChainLocks, InstantSend outside IBD; R2 §G.1 7) | A node that is not a pure function of block data can disagree with the network while every unit test passes | Run the same state machines; mixed-network and testnet soak tests; the divergence tripwire (US-NODE-22) | Any `bad-cb-*` or `conflict-tx-lock` disagreement in MIX or LIVE |
| R-6 | **evodb byte compatibility** (immer HAMT iteration order; `WriteDerived` byte comparison) | Writing Core's evodb is impractical; reading it is required for datadir import | Import-only (NG-4, Q11), own format, plus a "rebuild from blocks" path; DIFF-STATE compares semantics, not bytes | — |
| R-7 | **base-sdk dual stack.** Two Dash type systems; base-sdk's protocol crates are unpublished 0.0.0 with one maintainer (kwvg) and lack Devnet, ChainLock, IS, QRInfo, V1 framing and v24 (R3) | Node work on the critical path of an external SDK; two codecs that must agree | A1 decides the stack and the bridge; the conformance harness (BS2, R3 §4.2 task 2.1) gates every swap; escalate base-sdk changes through pasta (PROCESS.md) | base-sdk review latency > 2 weeks on a critical-path PR |
| R-8 | **Fingerprint vs performance.** Being 2× faster can itself be a fingerprint; protocol timers must match | Masternode deanonymization; violates the charter | Timers equal to Core's; classifier gate (NFR-FP-02) decides; registry for any unavoidable difference | Classifier accuracy > 60% |
| R-9 | **Platform contract drift.** Platform's use of Core is wide and partly unverified (§5.3 "to confirm") | Evonodes running N break Platform | Contract audit and frozen fixtures; dashmate end-to-end (US-PLAT-09) before any evonode pilot | Platform e2e failure |
| R-10 | **Functional-test port effort.** 264 Python tests assume dashd's binary, flags, hidden RPCs and timings | The cheapest end-to-end differential harness slips | Make FT compatibility a design input (A1), port in waves by area, track the pass rate in STATUS.md | Pass rate flat for 4 weeks |
| R-11 | **Collisions with upstream work** (#902 network rewrite, #264 BIP324, #776 FFI removal) | Rework and merge conflicts | NFR-CMP-04; pasta coordinates with maintainers (Q9) | An upstream merge that touches a node task's files |
| R-12 | **Live-network risk.** Masternodes on testnet and especially mainnet carry PoSe penalties and real collateral | Lost payments; harm to quorums | Testnet first (REL-2); hot-swap back to dashd (US-MN-20); pilot only with pasta's approval (Q6); tripwire stops signing on divergence | Any PoSe penalty attributable to N |
| R-13 | **Desktop verification gaps.** No macOS on agentbox; Windows never built; GUI never exercised against a real node (HANDOFF) | Desktop stories cannot be closed on agentbox alone | Linux DESK on agentbox; macOS and Windows QA scheduled on pasta's machines; FR-DSK-04 | — |
| R-14 | **Scale of the RPC surface** (266 MUST commands with byte-identical output) | Long tail delays REL-3 and REL-4 | Generate shapes from `rpc-json` where possible; DIFF-RPC harness early; implement by story priority | — |
| R-15 | **agentbox contention** between the BS2 oracles, replays, fuzzing, devnet soaks and B1 benchmarks (R4 §2.9) | Noisy benchmarks; slow gates | Schedule correctness jobs away from B1's NUMA node and windows; record the schedule in STATUS.md | Benchmark variance above the NFR-PERF-02 noise bound |

---

## 7. Release criteria ("Done when", made measurable)

The program is done when all six groups hold on one release candidate. Every criterion is automated or produces a recorded artifact under `node-program/` (reports, logs, dashboards).

### REL-1. Syncs mainnet and testnet from genesis, agreeing with dashd on every block, UTXO set and masternode list

| ID | Criterion |
|---|---|
| REL-1.1 | NFR-CEQ-06 replay passes on mainnet and testnet against each network's pinned reference release (Q5), to a tip no older than 30 days at the release candidate: the import replay with `-assumevalid=0`, the import replay with default assumevalid, and a P2P sync. **StateDigest v1 equal at every block** (UTXO MuHash and block info, block stats, expected payees, full masternode-list delta, CbTx fields, credit pool, active quorums, members, rotation info, deployments, coinbase ChainLocks, asset-unlock statuses, raw bytes), the state-digest patch's derived-state hashes equal, and at the tip `hash_serialized_2`, full `dumptxoutset`, `getchaintips` and `verifychain 4 0` equal. Superblock payees are not checkable by replay (R4 F3); they are covered by REL-2.1/2.2 |
| REL-1.2 | Live: N syncs from public peers only (no local dashd) on mainnet and testnet, then follows the tip for ≥ 7 days on mainnet and ≥ 30 days on testnet with zero tripwire alarms (US-NODE-22) and the same tip hash as a reference dashd at every ChainLock |
| REL-1.3 | Metrics 1–3 not slower than dashd (REL-6) |

### REL-2. Runs a masternode and evonode that serves the network, indistinguishable from dashd in practice

| ID | Criterion |
|---|---|
| REL-2.1 | MIX regtest: every Dash-specific functional test passes in the three mixes of NFR-CEQ-09(a) |
| REL-2.2 | MIX devnet per NFR-CEQ-09(b) passes |
| REL-2.3 | LIVE testnet for ≥ 30 consecutive days with ≥ 4 regular Rust masternodes and ≥ 1 Rust evonode serving Platform through dashmate: zero PoSe penalties attributable to N; DKG valid-member rate not worse than dashd masternodes' over the same period; every Rust masternode completes ≥ 1 successful DKG for each LLMQ type it is selected for; its ChainLock and InstantSend shares are used in recovered signatures; ≥ 50 CoinJoin sessions served successfully, with both dashd and Rust clients |
| REL-2.4 | Hot swap dashd → N → dashd on testnet (US-MN-20) with PoSe score 0 throughout |
| REL-2.5 | Mainnet pilot per Q6, if pasta approves it: ≥ 1 regular masternode for ≥ 14 days, no PoSe penalty, paid every cycle |
| REL-2.6 | Indistinguishability: all 31 behaviours of §4.2 identical (NFR-FP-01); classifier held-out AUC ≤ 0.55 for node and masternode (NFR-FP-02); timers indistinguishable (NFR-FP-03) |
| REL-2.7 | Metrics 14–18 not slower than dashd (REL-6) |

### REL-3. The Rust CLI covers the dash-cli workflows operators actually use

| ID | Criterion |
|---|---|
| REL-3.1 | The operator-workflow list below (kept in `compat/OPERATOR-WORKFLOWS.md`; pasta may add to it) runs as scripts through C and through stock dash-cli against both N and dashd. XCLI output is identical modulo the registry. Every workflow runs on regtest; the read-only ones also run on testnet and mainnet |
| REL-3.2 | Every MUST RPC in §5 is implemented with a DIFF-RPC test. Each SHOULD RPC is implemented or has an approved deferral (Q10) |
| REL-3.3 | dash-tx, dash-util and dash-wallet equivalents pass their XCLI comparisons (Dash's `bitcoin-util-test.json`, `tool_wallet`) |

**Operator workflows** (OW). Each lists its commands; all run through `dash-cli`-style invocation, incl. two-word subcommands.

| ID | Workflow | Commands |
|---|---|---|
| OW-01 | Check sync | `-getinfo`, `getblockchaininfo`, `mnsync status`, `getbestchainlock` |
| OW-02 | Peers and bans | `-netinfo 4`, `getpeerinfo`, `addnode`, `disconnectnode`, `setban`, `listbanned` |
| OW-03 | Masternode status | `masternode status`, `protx info`, `masternodelist`, `masternode count`, `masternode winners`, `masternode payments` |
| OW-04 | Register a masternode | `masternode outputs`, `bls generate`, `getnewaddress` ×3, `protx register_prepare` → `signmessage` → `protx register_submit`; also `protx register_fund` |
| OW-05 | Evonode | `protx register_evo`, `protx update_service_evo` |
| OW-06 | Maintain a masternode | `protx update_service`, `protx update_registrar`, `protx revoke` |
| OW-07 | Shared masternode | `protx shared_register_prepare`, `shared_sign`, `shared_combine`, `shared_update_share`, `shared_update_registrar_prepare`, `shared_dissolve_prepare`, `shared_dissolve` |
| OW-08 | Governance | `gobject list`, `gobject get`, `getgovernanceinfo`, `gobject check`, `gobject prepare`, `gobject submit`, `gobject vote-many`, `gobject vote-alias`, `voteraw` |
| OW-09 | Quorums and DKG | `quorum list`, `quorum info`, `quorum dkgstatus`, `quorum memberof` |
| OW-10 | Sporks and deployments | `spork show`, `getdeploymentinfo` |
| OW-11 | Wallet basics | `getbalances`, `getnewaddress`, `sendtoaddress`, `listtransactions`, `listunspent`, `encryptwallet`, `walletpassphrase`, `backupwallet` |
| OW-12 | Key export | `dumphdinfo`, `listdescriptors true`, `dumpprivkey`, `dumpwallet` |
| OW-13 | CoinJoin | `coinjoin start`, `coinjoin status`, `coinjoin stop`, `getcoinjoininfo`, `setcoinjoinrounds` |
| OW-14 | Raw transactions | `createrawtransaction`, `signrawtransactionwithwallet`, `sendrawtransaction`, `decoderawtransaction`, `getrawtransaction` |
| OW-15 | Chain queries | `getblock`, `getblockhash`, `getblockheader`, `gettxout`, `getspecialtxes` |
| OW-16 | Node maintenance | `stop`, `verifychain`, `logging`, `invalidateblock`, `reconsiderblock`, `pruneblockchain` |
| OW-17 | Mining | `getblocktemplate`, `submitblock`, `getmininginfo` |
| OW-18 | Regtest development | `-generate`, `generatetoaddress`, `setmocktime`, `sporkupdate` |
| OW-19 | Index queries | `getaddressbalance`, `getaddressutxos`, `getaddresstxids`, `getspentinfo`, `getblockhashes` |
| OW-20 | Import and rescan | `importdescriptors`, `importprivkey`, `importwallet`, `rescanblockchain` |
| OW-21 | PSBT | `walletcreatefundedpsbt`, `walletprocesspsbt`, `finalizepsbt` |
| OW-22 | Platform checks | `quorum listextended`, `quorum sign`, `quorum verify`, `protx listdiff`, `getcreditpoolinfo`, `getassetunlockstatuses` |

### REL-4. Every PRD user story is supported across node, CLI and desktop

| ID | Criterion |
|---|---|
| REL-4.1 | A traceability matrix (`compat/STORIES.md`) maps each of the 168 stories to its tests per surface; all are green |
| REL-4.2 | The 21 stories marked "D: pending decision" are resolved per Q1: either D is added and passes, or pasta has decided they stay N + C |
| REL-4.3 | dashwallet-desktop `docs/parity.md`: every non-parked item is "done" with a test reference; parked items follow Q1 |
| REL-4.4 | The desktop vault and signing paths have had an independent security review; macOS and Windows QA runs on pasta's machines are recorded |

### REL-5. Backed by passing suites, including differential tests against Dash Core

| ID | Criterion |
|---|---|
| REL-5.1 | The `node/integration` gates are green: `cargo fmt --check`, `cargo clippy --all-features --all-targets -- -D warnings`, `cargo test --workspace`, and the dash-spv dashd integration tests against dashd and against N (SPV) |
| REL-5.2 | NFR-CEQ-01, -02 and -03 at 100% |
| REL-5.3 | Functional tests: tiers 1–4 (NFR-CEQ-05) at 100% of the required pass set, with the expected-failures file empty and < 1% flakes over 10 consecutive full runs |
| REL-5.4 | Differential fuzzing budgets met (NFR-CEQ-04) with zero open divergences |
| REL-5.5 | Replay (REL-1.1), mutation parity (NFR-CEQ-07), state scenarios (NFR-CEQ-08), mixed networks (NFR-CEQ-09) and the chain scans (NFR-CEQ-13) pass |
| REL-5.6 | Every MUST RPC has a DIFF-RPC test; the allowed-differences registry has been reviewed and holds no entry outside classes 1–3 (§0.4) |
| REL-5.7 | Light clients: SPV suites, Dash Evo Tool and desktop regtest suites, and dashj on the Android emulator (US-LCS-01, -02, -07) |
| REL-5.8 | Platform end-to-end through dashmate (US-PLAT-09) |

### REL-6. Benchmarks: no metric slower than dashd; results documented against the 2× target

| ID | Criterion |
|---|---|
| REL-6.1 | All 18 metrics of §4.3 measured by NFR-PERF-02 on the reference box, mainnet (and testnet for IBD), recorded in `bench/REPORT-<version>.md` with dashd's value, N's value, the ratio and confidence intervals |
| REL-6.2 | No metric slower than dashd (NFR-PERF-02 g) |
| REL-6.3 | For each metric where the 2× target applies and is not met: a written explanation (network-bound, protocol-timer-bound, disk-bound, or remaining work) and a tracked follow-up |
| REL-6.4 | Per-PR results recorded in STATUS.md without gaps since the harness landed |

---

## 8. Open product questions for pasta

Only product choices and decisions that are hard to reverse. Each has a recommended default; the program proceeds on the default until pasta answers.

| ID | Question | Why it matters | Options | Recommended default | Blocks |
|---|---|---|---|---|---|
| **Q1** | **Should the desktop app get masternode, ProTx and governance UI?** (already asked) | dashwallet-desktop `CLAUDE.md` (2026-10-07) leaves these to Dash Core; pasta's 2026-10-08 brief asks for voting, masternode creation and shared masternodes, "mobile-inspired" (STATUS.md). 21 stories carry "D: pending decision" | (a) N + C only. (b) Wallet-holder flows on the desktop: browse and vote (own and imported voting keys), create and resume proposals, "my masternodes" status, a registration wizard for collateral held in the wallet, shared-masternode participant flows; server-side operator work (running the node, update service, BLS operator actions) stays N + C. (c) dash-qt-style masternode and governance tabs | **(b)**, scheduled after the desktop's DashPay core, reviving the parked branches `m3/r2-governance` and `m3/r3-protx`. Until answered: (a) | 21 story rows; desktop roadmap (task D3) |
| **Q2** | **Which wallet kinds does the node wallet support?** | Sets the node wallet's data model and the behaviour of about 17 legacy-only RPCs (`dumpprivkey`, `dumpwallet`, `importprivkey`, `dumphdinfo`, `sethdseed`, `upgradetohd`, …). Hard to change once wallets exist | (a) Descriptor only: legacy wallets are converted on import, and legacy-only RPCs return Core's descriptor-wallet errors. (b) Both: descriptor by default, plus legacy-HD semantics for imported legacy wallets and `createwallet descriptors=false` | **(b)**: existing users keep their workflows; key-wallet's model is already close to Core's legacy HD chain | FR-WAL-02; wallet design |
| **Q3** | **Names, default datadir and default ports** | User-facing contracts; changing them later breaks scripts and service files | Binaries: `dashd-rs` / `dash-cli-rs`, or something else. Datadir: separate (decided) but which path. Ports: Core's defaults or offset ones | **`dashd-rs` and `dash-cli-rs`** (tools as subcommands); datadir `~/.dashcore-rs` (Linux), `~/Library/Application Support/DashCore-rs` (macOS), `%APPDATA%\DashCore-rs` (Windows), with `dash.conf` inside; **the same default ports as dashd** (mainnet masternodes must use 9999 anyway), with a clear error if a port is busy | FR-CFG-03/04, packaging |
| **Q4** | **How does N identify itself on the wire and over RPC?** | The charter asks for indistinguishability, which needs dashd's user agent; but network version statistics (which Core developers use to plan upgrades and deprecations) would count N as Dash Core | (a) Emulate dashd by default: P2P user agent `/Dash Core:<emulated version>/`, `getnetworkinfo.version/subversion` emulated; the true identity is in `-version`, the `debug.log` header and a new non-Core RPC; honest identity opt-in. (b) Honest by default, emulation opt-in | **(a)**, disclosed in release notes; the emulated version always equals the Core release whose behaviour N implements. pasta may want to tell Dash Core developers | FR-P2P-16, NFR-FP |
| **Q5** | **Which Dash Core releases are the references?** | Consensus, P2P behaviour and RPC output are compared against them. Mainnet runs v23.1.x; V24 is not final; rc.3 and develop add `evo_shares` (R4 §0.2, §1.12) | (a) One baseline for all networks. (b) A pinned release per network, tracked continuously | **(b)**: mainnet = the release mainnet runs (v23.1.x today; v24.0.x once mainnet moves); testnet = v24.0.0 rc → final; devnet/regtest = the testnet pin; develop watched weekly in a non-blocking lane. Release certification uses the pins current at code freeze. Current dashd validates all history, so older releases are never references | REL-1, REL-5, §5, Q4 (the emulated version follows the pin) |
| **Q6** | **Live-network pilots and security review** | Masternodes risk PoSe penalties and real collateral; third parties will trust N | Testnet only; or a mainnet pilot before Done; external audit or internal review | **Testnet masternodes early (REL-2.3). One mainnet regular-masternode pilot for ≥ 14 days after REL-2.1–2.4 pass, with pasta's collateral and a scripted swap back to dashd. A mainnet evonode only after US-PLAT-09. Internal multi-reviewer security review before the pilot; an external audit before recommending N to third parties (a budget decision)** | REL-2.5, NFR-SEC-09 |
| **Q7** | **Licensing of the node code** | rust-dashcore is CC0-1.0; Dash Core and base-sdk are MIT. Porting Core's consensus logic produces MIT-derived code whose notice must be kept. Hard to change once contributions accumulate | (a) New node crates MIT with Dash Core's notice where derived; existing crates stay CC0. (b) Relicense the workspace to MIT. (c) Keep CC0 and avoid deriving from Core's code (not realistic for bit-for-bit parity) | **(a)**, decided before the first ported-consensus PR merges | NFR-CMP-05 |
| **Q8** | **Supported platforms** | CI, benchmarks and packaging cost | Which OS/CPU pairs are tier 1 | **Node and CLI: tier 1 Linux x86_64 and aarch64; tier 2 macOS arm64 and Windows x86_64. Desktop: macOS, Windows and Linux as its own plan says. Benchmarks on Linux x86_64 (agentbox)** | NFR-PORT, US-NODE-21 |
| **Q9** | **Upstreaming and publication** | Working with dashpay/rust-dashcore, base-sdk and Dash Core maintainers is outward-facing; crate structure follows from the answer | Keep everything local until Done; or propose upstream in batches; or a separate repository | **Build in the rust-dashcore workspace on `node/*` branches, structured to upstream. Carry Dash Core test hooks (oracle shim, state-digest log, functional-test compatibility runner) as local Dash Core branches. pasta decides when to open upstream discussions and PRs (suggested: after the base-sdk milestone and the consensus core), whether to offer Core the hooks that would help it (a hidden evo-state-hash RPC, special-tx/BLS/LLMQ fuzz targets; R4 §2.9), and when to publish binaries** | NFR-CMP, NFR-CEQ-04/06, R-7, R-11 |
| **Q10** | **Scope approvals** | These items reduce parity | Approve or change | **Approve as written:** 1 WON'T RPC (`echoipc`); 22 SHOULD RPCs (shipping without any of them needs a per-item approval); non-goals NG-1…NG-10; the functional-test exclusion classes (Qt, USDT, multiprocess, BDB internals); and all stories, P2 included, block Done | §5, NFR-CEQ-05, REL-3.2 |
| **Q11** | **Storage independence and the way back to Dash Core** | Decides whether dashd can ever read N's datadir directly | (a) Own formats; Core formats import-only; export tools to go back. (b) Write Core-compatible `blk`/`rev` files so dashd can `-reindex` from N's blocks directory. (c) Full datadir compatibility (not feasible: evodb byte order) | **(a)** with linearize-style block export and dashd-loadable wallet exports (US-IMP-09) | FR-STO-01, A1 storage design |
| **Q12** | **Desktop product questions carried over from HANDOFF** | Product choices for the desktop roadmap | — | **Invitations:** include creating them (Android parity) after DashPay core. **Shielded pool:** after DashPay core, behind a build flag. **Integration credentials and the Explore database** (Coinbase, Uphold, CTX, Topper, ZenLedger, Imgur; Firebase): DCG decisions; ship without those integrations until credentials exist. **Trust model:** trusted quorum service for 1.0 (as iOS), SPV-derived quorum keys later; a linked N (US-WAL-19) can be the trusted source. **DashPay QR:** a plain `dashpay://user` link, as iOS | US-DPY-07, -10, US-WAL-21 |
| **Q13** | **Disclosure of EMBARGO-1** | Outward-facing and irreversible; until it is cleared nothing from `node/integration` (this PRD, R4, A1) can be published, which also gates Q9 | Private report under Dash Core's `SECURITY.md` now; or later | **pasta reports it privately to Dash Core now** (R4 V0). Meanwhile N replicates Core's behaviour, the details stay in the private file, and N follows upstream's fix in lockstep | Q9; any publication |

---

## Appendix A. R5 story IDs mapped to PRD IDs

| R5 | PRD | R5 | PRD | R5 | PRD |
|---|---|---|---|---|---|
| US-W-01 | US-WAL-01, -02 | US-CJ-01 | US-CJ-01, -02 | US-SM-01 | US-SHR-01 |
| US-W-02 | US-WAL-02, US-IMP-06 | US-CJ-02 | US-CJ-01, -03 | US-SM-02 | US-SHR-06 |
| US-W-03 | US-WAL-02 | US-CJ-03 | US-CJ-03 | US-SM-03 | US-SHR-03 |
| US-W-04 | US-WAL-02 | US-CJ-04 | US-CJ-08 | US-SM-04 | US-SHR-04 |
| US-W-05 | US-WAL-06, -16 | US-CJ-05 | US-CJ-09 | US-SM-05 | US-SHR-05 |
| US-W-06 | US-WAL-03, -20 | US-CJ-06 | US-CJ-06 | US-SM-06 | US-MN-05, -06 |
| US-W-07 | US-WAL-05 | US-CJ-07 | US-CJ-07 | US-GV-01 | US-GOV-01 |
| US-W-08 | US-WAL-05, -19, US-INT-09 | US-CJ-08 | US-MN-13, US-CJS-* | US-GV-02 | US-GOV-04 |
| US-W-09 | US-WAL-04, US-INT-04 | US-MO-01 | US-MN-21 | US-GV-03 | US-GOV-05 |
| US-W-10 | US-WAL-04, -18 | US-MO-02 | US-MN-03 | US-GV-04 | US-GOV-08 |
| US-W-11 | US-WAL-03, -22 | US-MO-03 | US-MN-03 | US-GV-05 | US-DPY-12 |
| US-W-12 | US-WAL-05 | US-MO-04 | US-MN-06 | US-GV-06 | US-GOV-06, -11 |
| US-W-13 | US-WAL-10 | US-MO-05 | US-MN-07 | US-PA-01 | US-GOV-02 |
| US-W-14 | US-WAL-15 | US-MO-06 | US-MN-09, -17 | US-PA-02 | US-GOV-03 |
| US-W-15 | US-WAL-08 | US-MO-07 | US-MN-18 | US-PA-03 | US-GOV-03 |
| US-W-16 | US-WAL-01 | US-MO-08 | US-MN-03, -06 | US-PA-04 | US-GOV-09 |
| US-W-17 | — (library capability; no Core or app story) | US-MO-09 | US-DPY-08 | US-EV-01 | US-PLAT-01 |
| US-W-18 | US-WAL-07, US-IMP-13 | US-OP-01 | US-MN-02 | US-EV-02 | US-MN-21 |
| US-W-19 | US-WAL-07, -22 | US-OP-02 | US-MN-05, US-PLAT-01 | US-EV-03 | US-DPY-08 |
| US-W-20 | US-WAL-20 | US-OP-03 | US-MN-04, -11, -13, -14, US-GOV-10 | US-EV-04 | US-PLAT-02…05 |
| US-W-21 | US-WAL-17 | US-OP-04 | US-MN-04, -09 | US-PL-01 | US-DPY-01, -08 |
| US-W-22 | US-WAL-01 | US-PU-01 | US-IMP-03 | US-PL-02 | US-DPY-01 |
| US-W-23 | US-WAL-09 | US-PU-02 | US-IMP-04 | US-PL-03 | US-DPY-03, -04 |
| US-W-24 | US-WAL-08 | US-PU-03 | US-IMP-05 | US-PL-04 | US-DPY-09 |
| US-W-25 | US-IMP-06 | US-PU-04 | US-WAL-07 | US-PL-05 | US-DPY-02, -10, -11 |
| US-W-26 | US-WAL-21 | US-PU-05 | US-WAL-07, US-IMP-13 | US-PU-09 | US-IMP-10 |
| | | US-PU-06 | US-IMP-06 | US-PU-10 | US-IMP-01, -02 |
| | | US-PU-07 | US-IMP-13 | US-PU-11 | US-DEV-05, US-INT-10 |
| | | US-PU-08 | US-IMP-11 | US-PU-12 | US-INT-14, REL-3, §5 |

## Appendix B. R2 story IDs

All R2 §D IDs keep their numbers and meaning: US-NODE-01…18, US-WAL-01…14, US-MN-01…17, US-SHR-01…05, US-GOV-01…07, US-CJ-01…05, US-MINE-01…05, US-INT-01…12, US-PLAT-01…07, US-DEV-01…06. Three were narrowed to avoid overlap with new areas:

- **US-MN-13** (CoinJoin server) is the operator-facing story; the protocol behaviour is in US-CJS-01…06.
- **US-MN-15** (governance) covers operator-key trigger duties only; voting-key voting is US-GOV-04.
- **US-NODE-18** (migrate a Dash Core datadir) is the end-to-end migration; its parts are US-IMP-01…08.

New areas: US-DPY, US-CJS, US-LCS, US-IMP. Each existing area also gained stories appended after R2's last number.

## Appendix C. Glossary

| Term | Meaning |
|---|---|
| CbTx | Coinbase special transaction (type 5) carrying the MN-list and quorum merkle roots, best ChainLock, credit-pool balance and asset-unlock root |
| DML / SML | Deterministic masternode list (full state, Core's `CDeterministicMNList`) / simplified masternode list (the subset light clients get in `mnlistdiff`) |
| LLMQ, DKG | Long-living masternode quorum; the distributed key generation that creates one |
| IS / ISDLOCK, CL / CLSIG | InstantSend lock; ChainLock signature |
| EHF / MNHF | Masternode hard-fork signal (DIP-23) that gates BIP9-style deployments such as V24 |
| PoSe | Proof of service: penalties and bans for masternodes that fail DKGs |
| Evonode | Masternode with 4000-DASH collateral that also serves Dash Platform |
| Superblock, trigger | Treasury payout block; the governance object that tells miners what it pays |
| DSTX, DSQ | CoinJoin final transaction broadcast; CoinJoin queue announcement |
| Credit pool | DASH locked for Platform credits, with per-block withdrawal limits |
| MuHash | The rolling hash of the UTXO set used by `gettxoutsetinfo` and coinstatsindex |
| Reference dashd, registry, FT-MAP | §0.4 |

> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

# A1: Foundational architecture of the Rust Dash node (Fable design)

- **Author:** Fable 5.1, independent design (the Opus design is written separately; the manager reconciles).
- **Date:** 2026-10-08.
- **Inputs read:** `briefs/A1-architecture.md`, `CHARTER.md`, `PROCESS.md`, `STATUS.md`, `research/R1-rust-dashcore-state.md`, `research/R2-dashcore-inventory.md` (+ `R2b-rpc-inventory.md`), `research/R3-base-sdk.md`, `research/R5-wallet-user-stories.md` (summary only), and `research/R4-consensus-and-verification.md` (landed while this was being written; its findings F1–F12 and its quirk register are folded in, see §4.11).
- **Repos inspected (read-only):** rust-dashcore `dev` @ `67b307c6` (+ `node/integration` @ `01304171`, `node/bs1-repin-base-sdk`, `node/bs2-conformance-harness`, `upstream/feat/bip324-v2-transport` = PR #264, `upstream/feat/fork-detection` = PR #803); base-sdk `develop` @ `33fcc59`; Dash Core `develop` @ `15db478750` (v24.1.0-pre, protocol 70242).
- **How to read:** §0 is the whole design in one page. §1–§5 answer the five brief questions in order. §6 is the decisions table, §7 the open questions for pasta. Appendix A gives the key Rust signatures so implementers can start from them. File paths are relative to the three repo roots; `rdc:` = rust-dashcore, `bsdk:` = base-sdk, `core:` = Dash Core.

---

## 0. Executive summary

1. **Type stack.** New node crates are written against **base-sdk types** (`dash-primitives`, `dash-script`, `dash-params`, `dash-p2p-core`, `dash-num`, `dash-pkc`, `dash-pow`), consumed from a **local base-sdk branch `node/base-sdk`** via path dependencies. `dashcore` (rust-dashcore's `dash/` crate) is **not** the node's type system; it stays the wallet/SPV/Platform type system and is converged onto base-sdk underneath (R3 Phase 3) as a parallel, non-critical-path workstream. The two codecs that exist during the program are tied together by the differential conformance harness (BS2) over all mainnet and testnet history, which is a merge gate.
2. **Base SDK milestone** is 18 PR-sized tasks (§1.4). Seven of them must land before node consensus work starts (re-pin, X11 unconditional, DIP-0020 opcode table + protocol constants, Devnet + LLMQ tables in `dash-params`, v24 payloads in `dash-primitives`, conformance harness, path-dep wiring). The P2P-typed-message work in `dash-p2p-core` (BS9) only gates the P2P crate, so consensus, chainstate and evo start in week 1.
3. **Crates.** Thirteen new workspace members under rust-dashcore, in strict layers: `dash-consensus` (pure, no I/O) → `dash-evo` (DML, quorum commitments, rotation, credit pool, EHF; pure state transitions over a store trait) → `dash-store` (KV engine) → `dash-chainstate` (block index, coins cache, undo, reorg, assumevalid) → `dash-mempool` → `dash-p2p` (transport, connman, addrman) → `dash-sync` (net_processing) → `dash-llmq`, `dash-governance`, `dash-coinjoin`, `dash-sporks`, `dash-indexes`, `dash-miner` → `dash-rpc` (+ REST, ZMQ) → `dash-node-wallet` → `dash-node` (daemon `dashd-rs`) and `dash-cli` (`dash-cli-rs`). Plus `dash-coreimport` (Core datadir/wallet import), `dash-testkit` (shared dashd harness, lifted out of dash-spv) and `dash-conformance` (vectors, oracle, replay, fuzz).
4. **Storage engine: RocksDB** behind a `KvStore` trait, **one database with column families** so chainstate, block index, evo and LLMQ state commit **atomically** in one `WriteBatch` (this removes Core's "evodb must agree with chainstate" failure class). Block and undo files keep **Core's exact `blk*.dat`/`rev*.dat` format** so a Core datadir imports by copy. Key and value encodings for coins, block index and evodb keys are Core's (import by translation, byte-diffable), except where Core's bytes depend on C++ container iteration order (DML full snapshots), where we define a sorted format and treat Core's as import-only.
5. **Concurrency.** Consensus and chainstate are **synchronous, deterministic code with no async**; one validation thread owns the chainstate write path; rayon pools do parallel script checks (CheckQueue equivalent) and batched BLS verification; tokio owns only the edges (sockets, RPC, ZMQ, timers). State is published to readers as immutable snapshots (`ArcSwap`), so RPC never blocks validation.
6. **Performance.** The 2x target comes from pipelining and caching that dashd lacks, not from a faster secp256k1: pre-validation of downloaded blocks ahead of the connect thread (merkle, X11, txids, CheckTransaction, sighash precomputation), parallel UTXO prefetch for all inputs of a block before connect, a compact `Coin` representation (about half of Core's per-entry memory), sorted batched flushes, a cached block template refreshed incrementally on mempool change, and a sigcache keyed so reorgs and template construction never re-verify. Per-metric expectations are in §3.8; some metrics (raw ECDSA throughput, X11) are parity, not 2x, and we say so.
7. **Verification.** Four layers, each natural in the crate layout: (a) Core's JSON vectors run directly against `dash-consensus`; (b) a **`libdashoracle`** C shared library built from Dash Core (local branch) exposing pure functions (script eval, sighash, DGW/KGW, subsidy, payload codecs, DML diff application, quorum member selection, BLS ops incl. legacy hash-to-G2) for **in-process differential fuzzing**; (c) **chain-replay comparison**: our node vs a dashd on the same blocks, comparing block status, UTXO MuHash, DML hash, quorum set and credit pool at every k-th height through RPC; (d) **Dash's `test/functional` suite** driven against the Rust binary through a thin shim in a Core branch, plus a mixed dashd/Rust masternode regtest harness lifted from `dash-spv/src/test_utils`.
8. **Sequencing.** Ten milestones (M0 foundations → M9 mainnet masternode). The critical path is BS → consensus → chainstate+store → evo (DML is needed for payee checks above mainnet 1047200 even under assumevalid) → mainnet IBD agreeing with dashd → P2P/sync → mempool/RPC → LLMQ (non-MN then MN) → masternode roles. Eight workstreams run in parallel from week 1 with 6–10 implementers; wallet parity and governance/CoinJoin are off the critical path until M7.

---

## 1. Type stack (Q1)

### 1.1 The decision

**Node crates build on base-sdk types. `dashcore` types are a compatibility layer for the wallet, SPV and Platform, converged onto base-sdk underneath, not the foundation of the node.**

Concretely:

| Shape | Owner (definition) | Node uses | Wallet/SPV/Platform use | Bridge |
|---|---|---|---|---|
| `Transaction`, `TxIn`, `TxOut`, `OutPoint`, `Block`, `BlockHeader`, `TxType`, special payloads | `bsdk: dash-primitives` | directly | `dashcore::{Transaction, Block, ...}` (today's structs; later thin wrappers/re-exports per R3 Phase 3.7) | `dash-bridge` (BS10): `From` both ways via re-encode, proven equal by BS2 |
| Hash newtypes (`TxHash`, `BlockHash`, `QuorumHash`, `MerkleRoot`, `InputsHash`, `ProTxHash` ...) | `bsdk: dash-num::make_hash!` + primitives | directly | `dashcore::{Txid, BlockHash, ...}` (bitcoin_hashes 0.14 newtypes with bincode) | byte-identical `From`; display direction documented |
| 256-bit arithmetic, compact target | `bsdk: dash-num::{Arith256, CompactTarget}` | directly | `dashcore::pow` (becomes a re-export in #1142) | none needed after #1142 |
| Script bytes, opcodes, sigops, address classification | `bsdk: dash-script` + `bitcoin-primitives` tagged `ScriptPubKeyBuf`/`ScriptSigBuf` | directly | `dashcore::ScriptBuf` (untagged) | `From` (bytes) |
| Script **interpreter**, sighash, tx/block **contextual** rules, PoW retarget, subsidy, payments, versionbits | `rdc: dash-consensus` (new) | directly | n/a (wallet signing uses `dashcore::sighash`, which is cross-checked by vectors) | n/a |
| Chain/consensus params, genesis, checkpoints, LLMQ tables, spork keys, Devnet | `bsdk: dash-params` (extended in BS6) | directly | `dash-network::Network` becomes a newtype over `dash_params::Network` (BS6b), keeping FFI discriminants | trivial |
| P2P message types, BIP324 short IDs, V1/V2 framing codec | `bsdk: dash-p2p-core` (extended in BS9) | directly | `dashcore::network::NetworkMessage` stays for dash-spv until Phase 3.9 | message-level `From` for the few messages both need (headers2, mnlistdiff, qrinfo, clsig, isdlock) |
| BLS/ECDSA/EdDSA | `bsdk: dash-pkc` | directly (scheme as a **type/context parameter**, never a global) | `dashcore-crypto` wrappers with runtime `BlsScheme` (keep, for bincode) | already exists |
| X11 | `bsdk: dash-pow` (default after BS3 benchmark) with `rs-x11-hash` C fallback feature | `dash-pow` | `dashcore_hashes::hash_x11`, X11 **unconditional** (BS3) | n/a |
| SML types, QRInfo, ChainLock, InstantLock | wire structs → `bsdk` (BS9); engines → `rdc: dash-evo` / `dash-llmq` | directly | `dashcore::sml`, `dashcore::ephemerealdata` (unchanged for SPV) | `From` for the wire structs |
| Wallet (BIP32/DIP9/DIP14/SLIP10, accounts, tx building, coin selection) | `rdc: key-wallet`, `key-wallet-manager` (extended) | via `dash-node-wallet` adapter | native | adapter converts blocks at the seam (§2.2.17) |
| RPC JSON shapes | `rdc: rpc-json` (extended to v24; becomes the server's response types too) | directly | native | n/a |

Rules that make this work:

- **R1 — one base-sdk source.** Every base-sdk crate in the rust-dashcore workspace resolves to the same source (today a git rev; during the program a path into `~/workspace/base-sdk` on branch `node/base-sdk`). A CI check (`contrib/check-base-sdk-pins.py`, BS15) fails the build if two sources appear.
- **R2 — node crates never import `dashcore` types.** `cargo deny` bans `dashcore` as a dependency of `dash-consensus`, `dash-evo`, `dash-chainstate`, `dash-mempool`, `dash-p2p`, `dash-sync`, `dash-llmq`, `dash-governance`, `dash-coinjoin`, `dash-indexes`, `dash-miner`. Only `dash-node-wallet`, `dash-rpc` (for `rpc-json`, which currently depends on `dashcore`), `dash-coreimport` and `dash-conformance` may depend on both, and they go through `dash-bridge`.
- **R3 — protocol shapes land in base-sdk first.** New wire formats (v24 and beyond) are implemented in `dash-primitives`/`dash-p2p-core` on `node/base-sdk`, with Core-derived corpora, before or together with the rust-dashcore copy. The conformance harness asserts equality.
- **R4 — the scheme is explicit.** No global BLS "legacy" flag anywhere in Rust. `dash-pkc`'s `BlsScChia`/`BlsScIetf` type parameter is used where the scheme is fixed by a version field; a `BlsCtx { legacy: bool }` value derived from the block being processed is passed where Core consults `bls_legacy_scheme` (CbTx `bestCLSignature`, `BuildCommitmentHash`, `quorumVvecHash`, MNAUTH, recovered sigs, IES ephemeral key, post-V20 modifier).

### 1.2 Alternatives considered

**A. Build the node on `dashcore` and migrate `dashcore` onto base-sdk later (R3 Phase 3 "for free").** This is the fastest start: the full P2P enum, SML types, CL/IS types and the wallet seam exist today. Rejected because consensus code must be written once against types whose definition is Core-vector-tested and shaped like Core's: `dashcore` has an eager payload enum that ignores the `vExtraPayload` length prefix (R1 §7.7), a Tapscript opcode table, a SHA256d-by-default `BlockHash`, segwit fields on `TxIn`, a `u16` version instead of Core's `int16 | u16 type` pair, and Bitcoin `consensus::Params`. Every one of those is fixable, but then Phase 3 renames fields under consensus code that has already been reviewed, and the review has to be repeated. The schedule gain (about 3 weeks) does not pay for that.

**B. Build the node on base-sdk and wait for upstream base-sdk to close its gaps.** Rejected: base-sdk has one maintainer, its incubating crates are 0.0.0, and the gaps (Devnet, LLMQ tables, v24, CL/IS/QRInfo/DKG/CoinJoin message types, V1 framing) are weeks of work with unknown review latency. The program is local and may change base-sdk (brief constraint), so we take a local branch and shape every commit so it can be upstreamed later (`namespace%type(scope): ...` commit format, 2-space style, no `unwrap`, nightly fmt). Upstreaming is pasta's call (§7).

**C. Define node types a third time in a `dash-node-types` crate.** Rejected: a third codec with no corpus.

**D. Build on `dashcore` but freeze it (no Phase 3).** Rejected: it leaves the Bitcoin leftovers in the consensus path and keeps two codecs forever.

Chosen: base-sdk types with a local branch (the "A-minus-churn, B-minus-waiting" option). The cost is the bridge at the wallet seam and the need to keep the two codecs equal, which BS2 does mechanically.

### 1.3 The specific sub-questions

**bincode.** base-sdk has no bincode by design, and the orphan rule prevents implementing `bincode::Encode` for base-sdk types in rust-dashcore. Decision: (a) `dashcore` newtypes stay the permanent owners of the persisted bincode formats (wallet blobs, SPV storage, Platform state), with pinned-layout tests (the `bincode_layout_matches_pubkey_hash` pattern from #1056); (b) **the node never persists with bincode.** Node on-disk values use Core's serialization where a Core format exists (coins, undo, block index, evodb values, mempool.dat, peers.dat, flat DBs) and a versioned `BaseCodec` struct otherwise. This keeps import/export and cross-version compatibility explicit and lets the differential harness byte-compare DB contents with dashd. `grovedb-bincode` (unmaintained fork, issue #344) therefore never enters the node's dependency set.

**Hash display order.** base-sdk displays every hash reversed (Core's `GetHex`); `dashcore` displays `ProTxHash`, `PubkeyHash`, `ScriptHash` and the `MerkleRoot*` types forward. Decision: the node and its RPC use Core's order everywhere (base-sdk's convention), because RPC JSON is fixed by Core and the functional tests compare strings. `rpc-json` gets serde adapters so the client crate accepts both. The wallet/FFI string forms are **not** flipped in this program (mobile stability); the bridge documents each type's direction, and R3 open question 5 stays open for the upstream owners.

**Network enum and Devnet.** `dash_params::Network` gains `Devnet` (BS6), and `ChainParams::devnet(name: &str, overrides: DevnetOverrides)` builds the devnet genesis (`CreateDevNetGenesisBlock`: prev = regtest-style genesis, coinbase scriptSig `1 <name>`, `OP_RETURN` output, bits of genesis) so `hashDevnetGenesisBlock` is derived, not stored. The devnet **name** lives in `ChainParams`, not in the enum (matches Core: four chains, name from args). `rdc: dash-network::Network` becomes `#[repr(u8)]` newtype/re-export preserving `FFINetwork` discriminants 0..3 and the lowercase serde names, plus `Network::Devnet` keeps its current `Display`. Devnet-only overrides (`-llmqchainlocks`, `-llmqinstantsenddip0024`, `-llmqplatform`, `-llmqmnhf`, `-llmqdevnetparams`, `-highsubsidyblocks/-factor`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-sporkaddr`) and regtest overrides (`-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams`, `-llmqtest*params`, `-llmqtestinstantsenddip0024`) are fields of `ConsensusParams`/`ChainParams` set by the node's args layer, with the same parse rules as `core: chainparams.cpp:1153-1408`, because Dash's functional tests use them.

**v24 payloads (upstream PRs #1136–#1138).** Both copies are needed: rust-dashcore's for SPV/wallet/Platform (ZocoLini's PRs are small and already written; merge them, plus the remaining #1131/#1132/#1134/#1135 items), and `dash-primitives` for the node (BS7: CbTx v4 `merkleRootAssetUnlocks`; ProRegTx v3 with `ExtNetInfo`, u8-counted payouts and shares, raw 65-byte join sigs, `nEarlyPeriodBlocks`/`nEarlyPenalty`; ProUpServ/ProUpReg/ProUpRev v3; types 10 `ProDisTx`, 11 `ProUpShareTx`, 12 `ProUpSharedRegTx`; AssetLock v2; AssetUnlock v2 with the **stable txid** that zeroes the trailing 132 bytes when the payload is exactly 145 bytes and `payload[0] >= 2`, plus `instance_hash()`). BS2 asserts both copies produce identical bytes, txids and instance hashes over the `trivially_valid.json`/`trivially_invalid.json` corpus and over synthetic v24 transactions generated by the Core oracle (§4.3). The node is tested on testnet where V24 starts 2026-09-24; mainnet is `NEVER_ACTIVE` (TODO in Core), so the node must implement **both** rule sets and read activation from params. R4 adds that `origin/develop` and the rc.3 backport split EvoNode shares and multi-payouts behind a second deployment, `evo_shares` (bit 14), and that the V24 credit-pool limit formula still changes between rc branches: `dash-params` carries both deployments, and the node pins the exact rule set to the Core tag chosen in §7 Q3 (mainnet v23.1.x for history, v24.0.0 final for testnet), re-verified by the replay at each re-pin.

**The `BlockHash` X11 feature footgun.** BS3 removes the `core-block-hash-use-x11` feature from `dashcore` and makes `BlockHash` X11 unconditionally (header hash = `dash_pow::hash(80 bytes)`, display reversed). This is a breaking correctness fix (R1 §9.1); Platform and mobile already enable the feature through dash-spv, so their behaviour is unchanged. The C `rs-x11-hash` stays behind a `x11-c` feature until the BS3 benchmark (header sync over the 2.5M mainnet headers, and `pow_hash` microbench) shows `dash-pow` is within 10 %; if not, `dash-pow` gets x86_64 AES-NI dispatch (it is compiled but not dispatched today) as a base-sdk perf PR. base-sdk `dash-primitives` is already X11-only, so the node is correct from day one regardless.

**One codec vs two.** Two codecs exist for the duration of the program: `dashcore::consensus::{Encodable, Decodable}` (streaming `io::Read/Write`) and base-sdk `BaseCodec` (slice cursor) + `bitcoin-consensus-encoding`. Decision: accept it, make it safe, and converge:
- Safe: BS2 decodes every mainnet and testnet block and transaction (from Core's `blk*.dat`, via `dash-coreimport`) through both codecs and asserts identical re-encoding, txid, instance hash, block hash, merkle root, payload fields, and (for P2P messages) the `message_capture` corpus from a dashd (`-capturemessages`). It runs in CI on the regtest datasets and nightly on the full chains.
- Converge: R3 Phase 3 (3.2 → 3.4 → 3.5 → 3.7 → 3.8 → 3.9) runs as the "convergence" workstream, one Platform-coordinated breaking bump per step. When 3.7 lands, `dashcore::Transaction` *is* `dash_primitives::Transaction` and the bridge becomes identity.
- The node does not wait for convergence at any milestone.

**Codec style and no_std.** Node crates read whole messages and whole blocks into memory before decoding (Core does too), so the slice-cursor `BaseCodec` is the right primitive. `dash-consensus` and `dash-evo` are written `#![no_std] + alloc` with an opt-in `std` feature (same rule as base-sdk) so they can move down into base-sdk later if the owners want them; everything from `dash-store` up is `std`.

**Edition and MSRV.** rust-dashcore is edition 2024 / MSRV 1.89; base-sdk is 2021 / MSRV 1.85. New rust-dashcore crates use 2024/1.89. Code destined for base-sdk is written 2021-compatible (no let-chains in those two crates).

**Dependency wiring (concrete).** In `rdc: Cargo.toml` `[workspace.dependencies]`, every base-sdk crate is declared with `path = "../base-sdk/pkgs/<name>"`. The program's worktree layout is `~/workspace/rdc-wt/<slug>`, so a symlink `~/workspace/rdc-wt/base-sdk -> ~/workspace/base-sdk` makes `../base-sdk` resolve from every worktree as well as from the main checkout (BS15 adds this to the worktree-creation step in PROCESS.md). CI and future upstreaming switch the same table to `git = ..., rev = ...`. `Cargo.lock` is committed on `node/integration` (R1: none committed today) so benchmarks are reproducible.

### 1.4 The Base SDK milestone (ordered, PR-sized)

Numbering continues the manager's `node/bs1-*` and `node/bs2-*` branches. "bsdk" in the Repo column means the PR is on base-sdk branch `node/base-sdk` and must follow base-sdk conventions (`namespace%type(scope): ...` commits, 2-space, no `unwrap`/`panic`/`unsafe`, Latin-1, `just test lint` green). Sizes are engineer-weeks for one implementer.

| # | Task | Repo | base-sdk change? | Gates | Size |
|---|---|---|---|---|---|
| BS1 | Re-pin base-sdk to `develop` @ `33fcc59` (absorb #53 tweak/negate API, #54 `Arith256`, #56 `Zeroizing`); fix `dash_pkc::__deps::ff` use with a public reduce-scalar API; drop the vestigial `dash-types` dep in `dash/` | rdc (+ tiny bsdk API) | small | everything | 0.5 (in flight) |
| BS2 | `dash-conformance` skeleton: dual-codec decode of every tx/block from Core `blk*.dat` (via `dash-coreimport::blockfiles`), assert equal re-encode, txid, instance hash, block hash, merkle root, payload fields; CI on `regtest-40000`/`regtest-mn`; nightly on mainnet+testnet bootstrap; P2P `message_capture` corpus replay | rdc | no | every later swap; node start | 1.5 (in flight) |
| BS3 | `BlockHash` X11 unconditional (remove `core-block-hash-use-x11`); `dashcore_hashes::hash_x11` backed by `dash-pow` (feature `x11-c` keeps `rs-x11-hash`); microbench + 2.5M-header sync bench; AES-NI dispatch PR to `dash-pow` if needed | rdc (+ bsdk perf) | maybe | node start | 1 |
| BS4 | Protocol constants and tables in `dashcore` (SPV side): `PROTOCOL_VERSION` 70242 and the version-gate constants (70221/70223/70225/70227/70228/70229/70230/70231/70234/70235/70236/70237/70238/70239/70240/70241/70242); `MSG_CMPCT_BLOCK = 20`, inventory 6/16/17/18/21/23–26/28/29/31/32/33/34; service flags {NETWORK, BLOOM, COMPACT_FILTERS, NETWORK_LIMITED, HEADERS_COMPRESSED, P2P_V2}, drop WITNESS/GETUTXO; `MAX_PROTOCOL_MESSAGE_LENGTH` 3 MiB, `MAX_SIZE` 32 MiB, compact size as u64; DIP-0020 opcode table (0xba = `OP_CHECKDATASIG`, 0xbb = `OP_CHECKDATASIGVERIFY`, 0x7e–0x81, 0x84–0x86, 0x96–0x97 enabled; disabled set = INVERT/2MUL/2DIV/MUL/LSHIFT/RSHIFT); verify `dash-script::Opcode` matches | rdc | no (audit bsdk) | node start | 1 |
| BS5 | Rebase and merge #400 (drop SegWit/Taproot: `Witness`, `Weight`, `taproot.rs`, bech32 payloads, segwit sighash) | rdc | no | convergence; smaller migrate surface | 1 |
| BS6 | `dash-params`: `Network::Devnet` + `ChainParams::devnet(name, overrides)` + derived devnet genesis; one `LlmqType` (merge the two copies; add 103 and 105); `LlmqParams` table (14 rows, fields as `core: llmq/params.h`); per-network roles (`llmq_type_chain_locks`, `llmq_type_dip0024_instant_send`, `llmq_type_platform`, `llmq_type_mnhf`) and enabled sets; spork address + pubkey id; `dip0003_enforcement_hash`, `dip0024_quorums_height`, `superblock_start_hash`, `credit_pool_period_blocks`, `pool_min/max_participants`, `fulfilled_request_expire_time`, `llmq_connection_retry_timeout`, `require_routable_external_ip`, `allow_multiple_addresses_from_group`, `instant_send_confirmations_required`, `high_subsidy_*`, `minimum_difficulty_blocks`; `RegtestOverrides`/`DevnetOverrides` structs with Core's parse rules; serde. **BS6b** (rdc): `dash-network::Network` becomes a newtype over `dash_params::Network` keeping `FFINetwork` discriminants | bsdk + rdc | **yes** | node start (evo), p2p | 2 |
| BS7 | `dash-primitives` v24: CbTx v4; ProRegTx v3 (ExtNetInfo, u8-counted `payouts[]`/`shares[]`, raw join sigs, early period/penalty); ProUpServ/ProUpReg/ProUpRev v3; `TxType` 10/11/12 with `ProDisTx`, `ProUpShareTx`, `ProUpSharedRegTx`; AssetLock v2 (P2SH credit outputs); AssetUnlock v2 + **stable txid** + `instance_hash()`; `Checkable` rules per `core: evo/providertx.cpp` `IsTriviallyValid`; corpus generated by the Core oracle (§4.3). Closes base-sdk#40 | bsdk | **yes** | node start (evo), testnet | 3 |
| BS8 | rust-dashcore v24 for SPV/wallet: merge #1136 (unknown types passthrough **and fix the ignored payload length**), #1137, #1138; implement #1131/#1132/#1134/#1135; BS2 equality against BS7 | rdc | no | SPV/wallet on testnet | 1.5 |
| BS9a | `dash-p2p-core` core messages: un-stub `block`/`tx` (use `dash-primitives`), `getblocks`, `notfound`, `sendtxrcncl`, `mnauth`, `spork`/`getsporks`, `senddsq`, `ssc`, `clsig`, `isdlock`, `getqrinfo`/`qrinfo` (+ `CQuorumSnapshot`), `getmnlistd`/`mnlistdiff` with SMLE v3 (`ExtNetInfo`, versioned entry, `quorumsCLSigs`), `platformban`; full `InvType` table; `ProtocolVersion`-dependent (de)serialization (`SMNLE_VERSIONED`, `MNLISTDIFF_VERSION_ORDER`, `MNLISTDIFF_CHAINLOCKS`) via a `WireCtx { version: u32 }` parameter | bsdk | **yes** | P2P crate | 2 |
| BS9b | `dash-p2p-core` LLMQ messages: `qfcommit`, `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit` (+ `CBLSIESMultiRecipientObjects`), `qwatch`, `qsendrecsigs`, `qsigsesann`/`qsigsinv`/`qgetsigs`/`qbsigs`/`qsigrec`/`qsigshare`, `qgetdata`/`qdata`; `DYNBITSET` (reject padding bits) and `AUTOBITSET` codecs; `MaxDKGMessageSize` framing walk | bsdk | **yes** | LLMQ crate | 1.5 |
| BS9c | `dash-p2p-core` CoinJoin + governance: `dsa` (with `nFlags` ≥ 70241), `dsi`, `dsf`, `dss`, `dsc`, `dssu`, `dstx`, `dsq`; `govsync` (with `CBloomFilter`), `govobj`, `govobjvote` (65/96-byte sig rule) — gov types exist, add the vote-length rule and hash helpers | bsdk | **yes** | CoinJoin, governance | 1 |
| BS9d | `dash-p2p-core` framing and filters: V1 24-byte header + checksum encode/decode (pure, no I/O); `filterload`/`filteradd`/`filterclear`, `merkleblock` (+ `CPartialMerkleTree`), `sendcmpct`/`cmpctblock`/`getblocktxn`/`blocktxn` (BIP152 v1, txid short ids), `getcfilters`/`cfilter`/`getcfheaders`/`cfheaders`/`getcfcheckpt`/`cfcheckpt`; `headers2` `CompressibleBlockHeader` moved from rdc (exists in both; keep one) | bsdk | **yes** | P2P crate | 1.5 |
| BS10 | `dash-bridge` crate: `From`/`TryFrom` between `dashcore` and base-sdk for hashes (byte-identical), `Transaction`/`Block` (re-encode), payloads, scripts, `Network`; a display-direction table with tests; `rpc-json` serde adapters for Core hash order | rdc | no | wallet seam, RPC, import | 1 |
| BS11 | ECDSA through `dash-pkc` (enable `ecdsa`; `crypto::key` over `EcdsaPkBytes`/`EcdsaSecretKey`; message signing via `EcdsaRecSignature`; add a lax-DER parse helper to pkc for `CPubKey::Verify` parity) | rdc (+ bsdk helper) | small | convergence only | 1.5 |
| BS12 | Land #1142 (`Arith256` for `U256`/`Work`/`Target`) and fold `dash-spv` `ChainWork` into it | rdc | no | convergence only | 0.5 |
| BS13 | Dedupe `EddsaPkHash`/`PlatformNodeId` onto `dash_pkc::eddsa::EddsaPkHash` with the bincode layout pinned | rdc | no | convergence only | 0.5 |
| BS14 | Audit `dash-pkc` BLS API parity for what `dash-llmq` needs: `secure_aggregate`, `fast_verify_aggregates`, `verify_aggregates` (distinct messages), threshold `split`/`derive_share`/`recover_shares` for sk/pk/sig, `BlsShareId` from `proTxHash` raw bytes (big-endian scalar), IES multi-recipient encrypt/decrypt, `to_scheme` conversion, hash-of-serialization helpers; add what is missing; **fix the four R4 E§11.1 findings: secure-aggregation weights reduced mod r, the IES symmetric key derived from the basic encoding of the ECDH point, opposite-scheme-canonical bytes decoding to an invalid object (not an error) with no curve/subgroup check for legacy, and the Dash wrapper null/default rules** | bsdk | **yes** | LLMQ crate; M3 (commitment signature checks) | 1.5 |
| BS15 | Workspace wiring: path deps for all base-sdk crates, `~/workspace/rdc-wt/base-sdk` symlink in the worktree step, `contrib/check-base-sdk-pins.py`, committed `Cargo.lock` on `node/integration`, `cargo deny` ban of `dashcore` in node crates (rule R2), new CI groups (§2.1), MSRV job for the no_std crates | rdc | no | node start | 0.5 |
| BS16 | BLS vector provenance: regenerate `dash-pkc` corpora from dashbls through the Core oracle; add legacy hash-to-G2 (`ep2_map_legacy`) vectors, G1/G2 legacy/basic encoding edge cases (infinity, sign bit on `y.c1`, opposite-scheme bytes decode to invalid-not-error), a full regtest DKG transcript, threshold recovery and IES vectors | bsdk + core oracle | **yes** (tests) | LLMQ crate | 1 |
| BS17 | `dash-primitives::Checkable` parity audit vs `core: consensus/tx_check.cpp` and `CheckBlock` (empty `vin` allowed for types 6/7/9, empty `vout` for 6/7, coinbase scriptSig 1..100 for type 5 else 2..100, payload ≤ 10000, size ≤ 1,000,000 for `CheckTransaction`, relaxed 2,000,000/2,000,000-tx and 40,000-sigop `CheckBlock`, CVE-2012-2459 mutation flag, v4 `merkleRootAssetUnlocks` as MUTATED); error `Display` = Core's `bad-*` strings; **txid, merkle leaves and sighash are computed over Core's re-serialization, never over raw wire bytes** | bsdk | **yes** | node start | 1 |
| BS18 | Release train: tag base-sdk snapshots `node-YYYY-MM-DD` at each milestone; rust-dashcore CI pins by tag; one re-pin PR per milestone | both | process | ongoing | 0.2 each |

**Node-start gate** (must be merged before `dash-consensus`/`dash-evo` code is written against the types): BS1, BS2, BS3, BS4, BS6, BS7, BS15, BS17. About 5 weeks with 3 implementers in parallel (BS6/BS7/BS9 are independent; BS3/BS4/BS5 are independent of them).

**What proceeds in parallel with the gate** (week 1, no dependency on unmerged BS work):
- `dash-consensus::script` (interpreter + sighash + `script_tests.json`/`sighash.json`/`tx_valid.json`/`tx_invalid.json` runners): needs only today's `dash-script::Opcode` and `dash-primitives::Transaction`.
- `dash-consensus::pow` (DGW v3, KGW, BTC retarget, `CheckProofOfWork`) and `subsidy`/`payments`: need only `dash-num`.
- `dash-consensus::deployments` (versionbits with dynamic threshold and EHF signal height): pure.
- `dash-store` engine binding and `KvStore` trait; `dash-chainstate` block index and block files (Core formats); `dash-coreimport::leveldb` reader.
- `dash-evo::dml` for ProTx v1/v2 (exists in primitives), with the LLMQ table shimmed in-crate until BS6 lands.
- `dash-testkit` extraction from `dash-spv/src/test_utils` and the Core `libdashoracle` branch (§4.3).
- `node-bench` dashd baseline (B1, in flight).

**P2P gate**: BS9a + BS9d. **LLMQ gate**: BS9b + BS14 + BS16. **CoinJoin/governance gate**: BS9c. Everything else (BS5, BS8, BS10–BS13, BS18) is convergence or hygiene and never blocks node milestones.

### 1.5 Definition of "Base SDK integrated" for this program

The R3 definition of done is adopted with two amendments: (a) "defined in base-sdk" means defined on `node/base-sdk`, which may be ahead of upstream; (b) the wallet, SPV engine, BIP158/bloom, sighash for signing and RPC stay in rust-dashcore by design. The program-level test is mechanical: BS2 green over full mainnet and testnet history, and `cargo deny` proving no node crate depends on `dashcore`.

---

## 2. Node crate architecture (Q2)

### 2.1 Workspace layout and dependency graph

All new crates live in the rust-dashcore workspace (brief constraint: same workspace, rust-dashcore patterns). Directory name = package name, edition 2024, `version.workspace = true`, `thiserror` errors, `tracing` logging, `tokio` only where I/O lives.

```
rust-dashcore/
  dash-consensus/     pure consensus rules (no_std + alloc; no I/O, no threads)
  dash-evo/           DML, SML, CbTx roots, credit pool, EHF, quorum commitments, member selection (no_std + alloc)
  dash-store/         KvStore trait + RocksDB engine + column families + Core key/value codecs
  dash-chainstate/    block index, block/undo files, coins views, connect/disconnect, best chain, assumevalid, flush, reindex
  dash-mempool/       CTxMemPool, policy, fee estimation, orphanage, AcceptToMemoryPool, mempool.dat
  dash-p2p/           sockets, V1/V2 transport, connman, addrman, banman, eviction, DoS scores, MNAUTH, netfulfilled
  dash-sync/          net_processing: headers/blocks sync, relay, compact blocks, serving, mnsync, sporks sync
  dash-sporks/        spork state, signatures, sporks.dat
  dash-llmq/          quorum manager, DKG, sig shares, recovered sigs, ChainLocks, InstantSend, EHF signer, data recovery
  dash-governance/    objects, votes, store, sync protocol, superblocks, triggers, MN signer
  dash-coinjoin/      common + server (MN); client lands later under the wallet track
  dash-indexes/       txindex, address/spent/timestamp indexes, Dash BIP158 block filter index, coinstats
  dash-miner/         block assembly, CbTx construction, GBT, regtest miner
  dash-rpc/           JSON-RPC server, dispatch, all handlers, REST; response types from rpc-json
  dash-zmq/           ZMQ notifier (optional feature of dash-node)
  dash-node-wallet/   node-hosted key-wallet(-manager), wallet RPCs, encryption, persistence, Core wallet import
  dash-node/          init/args/config/datadir/logging/scheduler/shutdown; bin: dashd-rs
  dash-cli/           bin: dash-cli-rs (on rpc-client + rpc-json)
  dash-bridge/        dashcore <-> base-sdk conversions (BS10)
  dash-coreimport/    Core datadir readers: leveldb (pure Rust), blk/rev, chainstate, blockindex, evodb, wallets, flat DBs
  dash-testkit/       dashd/regtest harness lifted out of dash-spv/src/test_utils (publish = false)
  dash-conformance/   vectors, oracle bindings, chain replay, functional-test runner glue, fuzz corpora (publish = false)
  node-bench/         side-by-side harness and results (B1; publish = false)
```

Dependency graph (arrows point at dependencies; base-sdk crates omitted, every node crate uses them):

```
                      dash-consensus
                            ^
                        dash-evo
                            ^
   dash-store  <----  dash-chainstate  ---->  dash-sporks
       ^                    ^                      ^
       |              dash-mempool                 |
       |                    ^                      |
       |               dash-p2p  <-----------------+
       |                    ^
       |               dash-sync  (uses chainstate, mempool, evo, indexes, sporks)
       |                    ^
  dash-indexes      dash-llmq  dash-governance  dash-coinjoin   dash-miner
       ^                    ^         ^               ^             ^
       +--------------------+---------+---------------+-------------+
                                      |
                 dash-rpc  dash-zmq  dash-node-wallet (key-wallet*, dash-bridge)
                                      ^
                                  dash-node  -->  bin dashd-rs
   dash-cli (rpc-client, rpc-json)          dash-coreimport (store, chainstate, evo, node-wallet)
   dash-testkit, dash-conformance, node-bench (everything; dev/CI only)
```

Rules: no cycles; "lower" crates expose **traits** that "upper" crates implement (e.g. `dash-chainstate` defines `ValidationSubscriber`; `dash-llmq` implements it). Cross-subsystem calls that Core makes through `CChainstateHelper` (ChainLock conflicts, IS conflicts, superblock validity) are modelled as **traits injected into chainstate** (`ChainLockOracle`, `InstantSendOracle`, `SuperblockOracle`), with `NoOp` implementations used for IBD/replay and tests. This is what makes the state-dependent parts of block validity (R2 §G.1 item 7) explicit and testable.

CI groups (`.github/ci-groups.yml`): `node-consensus` {dash-consensus, dash-evo, dash-store, dash-chainstate, dash-mempool, dash-miner, dash-indexes, dash-bridge}, `node-net` {dash-p2p, dash-sync, dash-sporks, dash-llmq, dash-governance, dash-coinjoin}, `node-app` {dash-rpc, dash-zmq, dash-node-wallet, dash-node, dash-cli, dash-coreimport}, `node-conformance` {dash-testkit, dash-conformance} (runs dashd). The existing `core/spv/wallet/ffi/rpc` groups are untouched.

### 2.2 Crate by crate

#### 2.2.1 `dash-consensus` (pure rules)

Scope: everything Core computes in `consensus/`, `script/`, `pow.cpp`, `validation.cpp` subsidy/`GetBlockScriptFlags`/`ContextualCheck*`, `masternode/payments.cpp` arithmetic, `versionbits.cpp`, `deploymentstatus`. No I/O, no threads, no global state, `#![no_std] + alloc`, `std` feature only for tests. Every function takes explicit inputs; nothing reaches into a DB.

Modules and key items:

```rust
// script
pub struct ScriptFlags(u32);              // P2SH, STRICTENC, DERSIG, LOW_S, NULLDUMMY, SIGPUSHONLY, MINIMALDATA,
                                          // DISCOURAGE_UPGRADABLE_NOPS, CLEANSTACK, CLTV, CSV, NULLFAIL, CONST_SCRIPTCODE
                                          // (bits exactly as core: script/interpreter.h; 15 is retired, DIP0020 always on)
pub enum ScriptError { .. }               // one variant per core ScriptError_t, Display = Core's string
pub trait SignatureChecker {
    fn check_ecdsa(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8]) -> bool;   // CHECKSIG path (sighash+cache)
    fn check_data_sig(&self, sig: &[u8], msg: &[u8], pubkey: &[u8]) -> bool;        // OP_CHECKDATASIG: single SHA256(msg)
    fn check_lock_time(&self, n: &ScriptNum) -> bool;
    fn check_sequence(&self, n: &ScriptNum) -> bool;
}
pub fn eval_script(stack: &mut Stack, script: &[u8], flags: ScriptFlags, checker: &impl SignatureChecker) -> Result<(), ScriptError>;
pub fn verify_script(script_sig: &[u8], script_pubkey: &[u8], flags: ScriptFlags, checker: &impl SignatureChecker) -> Result<(), ScriptError>;
pub fn signature_hash(tx: &Transaction, input: usize, script_code: &[u8], hash_type: u32) -> Hash256;  // legacy; includes
                                          // the 32-bit version|type word and vExtraPayload when version>=3 && type!=0;
                                          // SIGHASH_SINGLE with input >= outputs -> Hash256::ONE
pub struct TransactionSignatureChecker<'a> { tx: &'a Transaction, input: usize, amount: u64, cache: Option<&'a dyn SigCache> }
pub struct ScriptNum(i64);                // 4-byte default max, minimal encoding rules, OP_NUM2BIN getint() clamp quirk
pub fn legacy_sigop_count(script: &[u8]) -> usize;  // re-export dash-script; CHECKDATASIG not counted
pub fn p2sh_sigop_count(script_sig: &[u8], script_pubkey: &[u8]) -> usize;

// tx
pub fn check_transaction(tx: &Transaction) -> Result<(), TxError>;                   // = dash-primitives Checkable + Core quirks (BS17)
pub fn contextual_check_transaction(tx: &Transaction, next_height: u32, d: &dyn DeploymentView) -> Result<(), TxError>;
pub fn check_tx_inputs(tx: &Transaction, coins: &dyn CoinSource, spend_height: u32) -> Result<Fee, TxError>;  // asset unlock -> payload fee, no input checks
pub fn is_final_tx(tx: &Transaction, height: u32, time: i64) -> bool;
pub fn calculate_sequence_locks(tx: &Transaction, flags: LockTimeFlags, prev_heights: &[u32], block: &HeaderCtx) -> SequenceLocks;

// block
pub fn check_block_header(h: &BlockHeader, params: &ConsensusParams, devnet_genesis: Option<&BlockHash>) -> Result<(), BlockError>;
pub fn check_block(b: &Block, params: &ConsensusParams, check_pow: bool, check_merkle: bool) -> Result<(), BlockError>;   // relaxed limits
pub fn contextual_check_block_header(h: &BlockHeader, prev: &HeaderCtx, now: i64, params: &ConsensusParams, checkpoints: &dyn CheckpointView) -> Result<(), BlockError>;
pub fn contextual_check_block(b: &Block, prev: &HeaderCtx, d: &dyn DeploymentView) -> Result<(), BlockError>;
pub fn block_script_flags(d: &dyn DeploymentView, height: u32) -> ScriptFlags;  // P2SH | DERSIG@bip66 | CLTV@bip65 | CSV@csv | NULLDUMMY@bip147
pub fn max_block_size(dip0001: bool) -> usize;  pub fn max_block_sigops(dip0001: bool) -> usize;

// pow
pub fn get_next_work_required(last: &HeaderCtx, block: &BlockHeader, params: &ConsensusParams, chain: &dyn HeaderAncestry) -> CompactTarget;
pub fn dark_gravity_wave(last: &HeaderCtx, chain: &dyn HeaderAncestry, params: &ConsensusParams) -> CompactTarget;  // 24-block, actual spans 23 intervals
pub fn kimoto_gravity_well(..) -> CompactTarget;        // f64 + libm::pow; only ever checked under the ±50 % tolerance (R4 F4)
pub fn check_proof_of_work(hash: &BlockHash, bits: CompactTarget, params: &ConsensusParams) -> bool;
pub fn mainnet_pre_68589_tolerance(bits: CompactTarget, expected: CompactTarget) -> bool;   // |d(bits)-d(expected)| <= 0.5*d(bits), ConvertBitsToDouble

// subsidy & payments
pub fn block_subsidy_inner(prev_bits: CompactTarget, prev_height: u32, v20: bool, params: &ConsensusParams) -> Amount;  // f64 pow, truncation, <=4500 bug, /14 per 210240
pub fn block_subsidy(prev: &HeaderCtx, d: &dyn DeploymentView, params: &ConsensusParams) -> Amount;   // genesis special-cased
pub fn superblock_subsidy_inner(..) -> Amount;      // treasury part: /10 pre-V20, /5 from V20
pub fn masternode_payment(height: u32, block_value: Amount, era: MnRewardEra, params: &ConsensusParams) -> Amount;
pub fn platform_share(mn_reward_on_subsidy: Amount) -> Amount;   // *375/1000
pub fn split_amount_by_shares(total: Amount, shares: &[Share]) -> Vec<Amount>;  // u128 intermediates, last gets remainder

// deployments
pub enum BuriedDeployment { HeightInCb, DerSig, Cltv, Bip147, Csv, Dip0001, Dip0003, Dip0008, Dip0020, Dip0024, Brr, V19, V20, MnRr, Withdrawals }
pub trait DeploymentView {               // implemented by chainstate over the block index + evo EHF signals
    fn active_after(&self, prev: &HeaderCtx, d: Deployment) -> bool;   // prev.height + 1 >= H  (or BIP9 state at prev)
    fn active_at(&self, idx: &HeaderCtx, d: Deployment) -> bool;       // idx.height >= H
    fn dip0003_enforced(&self, prev: &HeaderCtx) -> bool;
}
pub fn versionbits_state(prev: &HeaderCtx, dep: &Bip9Deployment, chain: &dyn HeaderAncestry, ehf: &dyn EhfSignalView, cache: &mut VbCache) -> ThresholdState;
// the walk-back stops at MinBIP9WarningHeight (a consensus floor: no mainnet deployment leaves DEFINED before ~2203488); the dynamic
// threshold's attempt*attempt*period is an i32 product in Core and wraps at ~730 attempts: use wrapping_mul and never panic (R4 §1.6);
// historic MNHF signals for bits 10, 11 and the abandoned 12 are valid and stored (unknown bits are valid)
pub fn compute_block_version(prev: &HeaderCtx, ..) -> i32;   // 0x20000000 | STARTED/LOCKED_IN bits
```

Fidelity notes baked into the module docs (each is a test): `OP_CHECKDATASIG` uses `CheckSignatureEncoding` (DER + exactly one trailing hash-type byte under DERSIG; defined type under STRICTENC) and single-SHA256 of `msg`, with NULLFAIL; `OP_NUM2BIN` size via `getint()` then `u64`; `OP_BIN2NUM` 4-byte limit; `FindAndDelete` of the signature in BASE sigversion; `MAX_OPS_PER_SCRIPT` 201 counts CHECKDATASIG; disabled opcodes fail even in unexecuted branches; sigops ignore CHECKDATASIG; `MAX_STANDARD_TX_SIZE` 100,000 is consensus after DIP0001 (`bad-txns-oversize`); `CheckBlock` relaxed 2,000,000 bytes / 2,000,000 txs / 40,000 sigops, contextual = height-dependent; BIP34 height-in-coinbase only while BIP34 is active **and** DIP0003 is not; the mainnet `nPrevHeight <= 4500` subsidy bug; the mainnet `height <= 68589` 50 % difficulty tolerance (the only check applied to BTC-retarget and KGW blocks, R4 F4); ECDSA at consensus is lax-DER parse plus S normalisation (high-S verifies) with hybrid 06/07 keys accepted; C++ exceptions (scriptnum overflow or non-minimal encoding, empty pops) map to `UNKNOWN_ERROR`; a failing input is re-run without the non-mandatory flags to classify `mandatory-script-verify-flag-failed` versus `non-mandatory-script-verify-flag`; DGW needs `last.height >= 24` else powLimit; devnet `-minimumdifficultyblocks`; testnet/devnet min-difficulty rules (7200 s → powLimit, 600 s → ×10 clamp); regtest `fPowNoRetargeting`.

Floating point policy (refined by R4 F4): on mainnet, the BTC-retarget and KGW eras (h < 34140) and DGW up to 68589 are only checked by the ±50 % double tolerance, so those algorithms need no bit-exactness beyond the tolerance comparison; DGW is bit-exact from 68590 and is integer-only. The subsidy's `pow(x, 2.0)` is computed as `x*x` in IEEE binary64 with FMA disabled (what glibc returns for exponent 2), `ConvertBitsToDouble` is replicated literally, and because mainnet replay only enforces `coinbase <= subsidy + fees` (a too-high subsidy would pass unnoticed), **every mainnet and testnet height is differential-tested against `getblockstats h ["subsidy"]`** as part of the L3 replay (R4 C§1). An oracle-generated per-height subsidy table remains as an offline CI cross-check on non-x86 targets.

#### 2.2.2 `dash-evo` (block-derived masternode and quorum state)

Scope: `core: evo/*` (except `providertx_service.cpp` wallet builders and `snapshot.*` assumeutxo), `llmq/{commitment,blockprocessor,utils (member selection),snapshot,quorumsman::ScanQuorums+SelectQuorumForSigning}`, `masternode/payments.cpp` payee logic. This is the consensus-critical Dash state. `#![no_std] + alloc`; persistent data structures from `imbl` (HAMT) so per-block list copies are O(log n), as Core's `immer` gives it.

Key types:

```rust
pub struct DmnState { version: u16, registered_height: u32, last_paid_height: u32, consecutive_payments: u32,
    pose_penalty: i32, pose_revived_height: i32, pose_ban_height: i32, revocation_reason: u16,
    confirmed_hash: BlockHash, confirmed_hash_with_protx: Hash256 /* single SHA256(protx||confirmed) */,
    key_id_owner: Option<PubKeyHash>, pub_key_operator: BlsPkBytesVersioned, key_id_voting: PubKeyHash,
    net_info: NetInfo /* Mn (v<3) | Ext (v>=3) */, payouts: Vec<Payout>, shares: Vec<Share>,
    early_period_blocks: u32, early_penalty: i64, script_operator_payout: ScriptPubKeyBuf,
    platform_node_id: EddsaPkHash, platform_p2p_port: u16, platform_http_port: u16 }
pub struct Dmn { pro_tx_hash: ProTxHash, internal_id: u64, collateral: OutPoint, operator_reward: u16, state: Arc<DmnState>, mn_type: MnType }
pub struct DeterministicMnList { block_hash: BlockHash, height: u32, total_registered: u32,
    by_protx: imbl::HashMap<ProTxHash, Arc<Dmn>>, by_internal_id: imbl::HashMap<u64, ProTxHash>,
    unique_props: imbl::HashMap<Hash256 /* SerializeHash(value) */, (ProTxHash, u32 /* refcount */)> }
pub struct DmnListDiff { added: Vec<Dmn>, updated: Vec<(u64, DmnStateDiff)>, removed: Vec<u64> }   // Core dmn_D4 format
pub struct DmnStateDiff { fields: u64 /* bit map as Core */, .. }                                    // hana tuple order on the wire

pub fn build_new_list_from_block(prev: &DeterministicMnList, block: &Block, prev_hdr: &HeaderCtx, coins: &dyn CoinSource,
                                 quorum_members: &dyn QuorumMemberSource, d: &dyn DeploymentView, ctx: &EvoCtx)
    -> Result<(DeterministicMnList, Option<ProTxHash /* payee before mutation */>), EvoError>;
```

`build_new_list_from_block` encodes the consensus order exactly (R2 A.3 §4.2): payee computed from `prev` first; confirmation at `prev.height - registered >= masternode_minimum_confirmations` (mainnet 15, else 1) with `confirmed_hash = prev.hash`; `decrease_scores`; special txs in block order (ProReg replace-by-collateral, dup checks against the list as rebuilt so far, `BanIfNotBanned` on empty netInfo (since v23 invalid `CService` bytes normalise to empty and the MN registers banned where v18–v22 rejected the tx, R4 D§2), V24 operator-key-under-any-scheme; ProUpServ with pre/post-V24 version semantics and `Revive`; ProUpReg `ResetOperatorFields`; ProUpRev version drop to 1 pre-V24; types 11/12; QcTx PoSe `CalcPenalty(66)` for `!validMembers`); second pass collateral spends (shared only via type 10); payee `last_paid_height` bump and Evo `consecutive_payments` handling with the MN_RR switch. Payee selection: `compare_by_last_paid` with revived-height override, `registered_height` fallback, tie-break by `pro_tx_hash` raw-byte memcmp; the Evo "pay 4 in a row" rule between V19 and MN_RR.

Other modules: `sml` (`SimplifiedMnListEntry` with the version-dependent hash exclusions; `merkle_root_mn_list` over **all** MNs sorted by raw bytes; `build_mnlistdiff` incl. `quorumsCLSigs` grouping and the null-base echo rule); `cbtx` (`check_cbtx` version gates: DIP8 `active_at(prev)`, V20 `active_after(prev)`, V24&&V20 → v4; `merkle_root_quorums` with the rotation-indexed/sliding-window assembly and byte-sorted hashes **over all configured LLMQ types, enabled or not** (24 stale legacy LLMQ_50_60 commitments are still leaves on mainnet today, R4 F5); `check_cbtx_best_chainlock` via the injected `ChainLockVerifier`; `merkle_root_asset_unlocks` as MUTATED); `creditpool` (`CreditPool { locked, current_limit, lately_unlocked, indexes: RangeSet }`, the three limit eras, `cpm_S` every 576); `mnhf` (signals map, `IsValidMNActivation`, bit reuse expiry, `EhfSignalView` for versionbits); `specialtx` (every `CheckPro*Tx`, shared-collateral covenant creation/spend checks, asset lock/unlock checks with quorum selection, `process_special_txs_in_block` orchestration returning the new list, new commitments, credit pool, signals and the `bls_legacy` flip at V19); `llmq::commitment` (`FinalCommitment` versions 1–4, `verify` with the `checkSigs` split, `VerifyNull`, `build_commitment_hash` under `BlsCtx`); `llmq::blockprocessor` (required-commitment counts per enabled type at `prev`, mining window, dup rules, **null commitments of known-but-disabled types accepted at any height, `signers >= minSize` rather than threshold, padding bits a parse error**, `q_mc`/`q_mcih`/`q_mcihi` index, undo); `llmq::members` (score = single SHA256 → little-endian `Arith256`; sort descending by score then **collateral descending**; modifier rules pre/post-V20 with the V20 check at `base-8`; non-rotated list at `base-8` only if V20 active after `base`; rotated quarters with snapshots, `MODE_SKIPPING_ENTRIES` first-index quirk, `skipRemovedMNs` V19/testnet rule, no snapshot when enabled MNs < quarter size); `llmq::scan` (`ScanQuorums` window snapping, newest-first, rotation per-index); `llmq::selection` (`SelectQuorumForSigning` with `GetUint64(3) >> (64-n-1)` for rotated types and raw-byte `min` for non-rotated; `VerifyRecoveredSig`).

Store trait (implemented in-memory for tests and by `dash-store` for the node):

```rust
pub trait EvoStore {
    fn best_block(&self) -> Option<BlockHash>;
    fn dmn_snapshot(&self, h: &BlockHash) -> Option<DeterministicMnList>;      // "dmn_S3"
    fn dmn_diff(&self, h: &BlockHash) -> Option<DmnListDiff>;                   // "dmn_D4"
    fn credit_pool(&self, h: &BlockHash) -> Option<CreditPool>;                // "cpm_S"
    fn mnhf_signals(&self, h: &BlockHash) -> Option<Signals>;                  // "mnhf_s2"
    fn mined_commitment(&self, t: LlmqType, q: &QuorumHash) -> Option<(FinalCommitment, BlockHash)>;   // "q_mc"
    fn mined_commitments_until(&self, t: LlmqType, height: u32, n: usize, index: Option<i32>) -> Vec<..>; // "q_mcih(i)"
    fn quorum_snapshot(&self, t: LlmqType, cycle_base: &BlockHash) -> Option<QuorumSnapshot>;          // "llmq_S"
}
pub struct EvoWrite { .. }   // batched writes staged per block, committed by chainstate in the same KV batch as coins
```

In-memory caches mirror Core's: MN lists per height (LRU 1000), mini-snapshots every 32 blocks, member lists per (type, baseHash), quorum snapshots per cycle base.

Formats: keys are Core's (CompactSize-prefixed string tags, `htobe32(0xffffffff - h)` inverse heights); values are Core's **except** `dmn_S3`, where Core writes immer-HAMT iteration order and byte-compares on `WriteDerived`. We write entries sorted by `pro_tx_hash` and never byte-compare against Core; Core's evodb is **import-only** (§2.2.18). `internal_id` is assigned by our own counter (not consensus in Core either).

#### 2.2.3 `dash-store` (storage engine)

**Decision: RocksDB (via `rust-rocksdb`), one database per network datadir, column families per logical table, behind an engine-agnostic trait.** Flat files for blocks, undo and filters in Core's exact format.

Why RocksDB and not the alternatives:

| Option | Fit for the UTXO/index workload | Atomicity across tables | Ops/portability | Verdict |
|---|---|---|---|---|
| **RocksDB** | LSM: absorbs the random-key insert/delete storm of IBD; bloom filters + prefix seek for `txid||n` lookups and `'t'+txid` txindex; large sorted `WriteBatch` matches our flush pattern | one `WriteBatch` across all CFs is atomic | C++ build (sccache/ccache available, ~4 min cold); Linux/macOS/Windows; widely used by Rust chain software | **chosen** |
| LMDB (`heed`) / MDBX (`libmdbx`) | B+tree mmap: best read latency, but random writes into a 2.5M-txid key space cause page splits and high write amplification during IBD; map size must be managed; single writer | multi-table txn atomic | C build; mmap size and Windows quirks | fallback if the B1 harness shows a read-latency-bound phase |
| redb (pure Rust) | B-tree, ACID, single writer; younger and slower on write-heavy loads | multi-table txn atomic | no C deps; simplest build | kept as the **test/portable engine** behind the same trait |
| LevelDB (Core's) | what dashd uses; no CFs, separate DBs per store (hence Core's evodb/chainstate consistency checks) | none across DBs | pure-Rust reader exists (`rusty-leveldb`) | **import only** |
| custom flat files for UTXO | not justified; the win is in the cache, not the engine | — | — | no |

The 2x IBD target is not expected to come from the engine; it comes from the coins cache and pipelining (§3). Choosing RocksDB buys atomic multi-table commits (which deletes a whole class of Core failure modes: "evodb best block != chainstate tip", "H head blocks" partial-flush recovery, index/chainstate drift) and the engine the Rust ecosystem knows how to tune.

Trait and layout:

```rust
pub trait KvStore: Send + Sync {
    type Batch: KvBatch;
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Bytes>>;
    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Bytes>>>;     // parallel prefetch path
    fn iter_prefix(&self, cf: Cf, prefix: &[u8]) -> impl Iterator<Item = (Bytes, Bytes)>;
    fn write(&self, batch: Self::Batch, sync: bool) -> Result<()>;                 // atomic across CFs
    fn approximate_size(&self, cf: Cf) -> u64;
}
pub enum Cf { BlockIndex, Coins, Evo, Llmq, TxIndex, AddrIndex, SpentIndex, TsIndex, FilterIndex, CoinStats, Meta }
```

`dash-store::codec` holds the Core key/value codecs (`DiskBlockIndex` with the inline X11 hash and `nStatus` bits incl. `CONFLICT_CHAINLOCK = 128`; `Coin` with `VARINT(height*2+cb)` + `TxOutCompression`; `BlockFileInfo`; evodb string-tag keys; `BE32` inverse heights; txindex `DiskTxPos`; address/spent/timestamp fixed-width keys), shared by `dash-chainstate`, `dash-evo`, `dash-indexes` and `dash-coreimport`. **No XOR obfuscation** is applied in our DB (we are not LevelDB-compatible anyway); the importer handles Core's `\0obfuscate_key`.

RocksDB tuning defaults (overridable, reported in `getmemoryinfo`): `Coins` CF with 20-byte bloom (prefix = 32-byte txid), block cache sized from `-dbcache` (same semantic as Core: coins cache + DB caches share the budget; default 300 MiB to keep "comparable config"), `write_buffer_size` 64 MiB × 2, no compression on `Coins`/`BlockIndex` (hash keys don't compress), LZ4 on index CFs, `optimize_for_point_lookup` on `Coins`, periodic compaction off during IBD. `sync = true` only at checkpoints/shutdown (Core also relies on the OS for leveldb durability between flushes).

Flat files (`dash-store::flatfile`): `FlatFileSeq` with Core's naming (`blk%05u.dat`, `rev%05u.dat`, `fltr%05u.dat`), chunk pre-allocation (16 MiB / 1 MiB; `-tinyblk` 64 KiB; `-fastprune`), records `magic || u32 size || payload` and the undo checksum `SHA256d(prev_block_hash || undo)`. Positions point after the 8-byte header, as in `CDiskBlockIndex`. Pruning follows Core's `MIN_BLOCKS_TO_KEEP = 288` and the `prunedblockfiles` flag.

#### 2.2.4 `dash-chainstate` (block index, coins, connect, best chain)

Scope: `core: validation.{h,cpp}` minus mempool acceptance, `chain.h`, `coins.*`, `txdb.*`, `undo.h`, `node/blockstorage.*`, `node/chainstate.*`, `kernel/*`, `validationinterface.*`.

Block index: an arena `Vec<BlockIndexEntry>` with `u32` ids (`prev: Option<u32>`), `HashMap<BlockHash, u32>` lookup, `chain_work: Arith256`, `status: BlockStatus` (Core bits), `n_tx`, `n_chain_tx`, file/data/undo positions, and the 80-byte header. Persisted as Core's `'b'` records (so a Core `blocks/index` imports 1:1 and `verifychain`/`getchaintips` semantics are identical); loaded with PoW re-check like `LoadBlockIndexGuts`. The active chain is `Vec<u32>`; `HeaderCtx` (height, hash, time, bits, MTP) is the read-only view handed to `dash-consensus`.

Coins:

```rust
pub struct Coin { height_cb: u32 /* height<<1 | coinbase */, amount: u64, script: CompactScript }
pub enum CompactScript { P2pkh([u8;20]), P2sh([u8;20]), P2pkC(u8 /*02|03*/, [u8;32]), P2pkU(u8, [u8;32]), Other(Box<[u8]>) }  // = Core's ScriptCompression types 0..5
pub trait CoinsView { fn get_coin(&self, op: &OutPoint) -> Option<Coin>; fn have_coin(&self, op: &OutPoint) -> bool; fn best_block(&self) -> BlockHash; }
pub struct CoinsCache<'a> { base: &'a dyn CoinsView, map: HashMap<OutPoint, CacheEntry, FoldHash>, usage: usize /* memory accounting */ }
pub struct CacheEntry { coin: Option<Coin>, dirty: bool, fresh: bool }   // Core semantics incl. FRESH erase-on-spend
```

`CompactScript` is the on-disk compression used in memory too: a P2PKH coin is 4 + 8 + 21 bytes plus the 36-byte key, about 70 bytes per entry before hashmap overhead versus Core's `CTxOut` with a 28-byte-inline `prevector` plus `std::unordered_map` node overhead (about 130 bytes). This is the main RSS lever (§3.6). `CoinsCache::flush` writes sorted keys in one batch (chunked at `-dbbatchsize` 16 MiB with the `'H'` head-blocks marker protocol so a crash mid-flush is recoverable exactly as in Core).

Connect/disconnect:

```rust
pub struct ChainState { index: BlockIndex, coins_db: CoinsDb, cache: CoinsCache, evo: EvoState, params: ChainParams,
    oracles: Oracles /* ChainLockOracle, InstantSendOracle, SuperblockOracle, MnSyncOracle */, subscribers: ValidationBus }
impl ChainState {
    pub fn accept_block_header(&mut self, h: &BlockHeader, now: i64) -> Result<u32 /* id */, BlockError>;   // incl. CONFLICT_CHAINLOCK marking
    pub fn accept_block(&mut self, b: Arc<Block>, from_peer: bool, force: bool) -> Result<u32, BlockError>;  // CheckBlock + store to blk file
    pub fn activate_best_chain(&mut self, hint: Option<Arc<Block>>) -> Result<(), BlockError>;              // FindMostWorkChain (CONFLICT_CHAINLOCK = failed), reorg
    fn connect_block(&mut self, b: &Block, id: u32, view: &mut CoinsCache, evo: &mut EvoWrite, just_check: bool) -> Result<ConnectResult, BlockError>;
    fn disconnect_block(&mut self, b: &Block, id: u32, view: &mut CoinsCache, evo: &mut EvoWrite) -> Result<(), BlockError>;
    pub fn invalidate_block / reconsider_block / precious_block / enforce_block(chainlock) / mark_conflicting_block
    pub fn flush(&mut self, mode: FlushMode) -> Result<()>;     // coins + evo + index in ONE KV batch
    pub fn snapshot(&self) -> Arc<ChainSnapshot>;               // immutable view for RPC/P2P readers (ArcSwap-published)
}
```

`connect_block` reproduces Core's order (R2 A.1 §1 "ConnectBlock order"): CheckBlock (MUTATED → abort as Core does); conflicting ChainLock; evo best block == prev when DIP0003 active; genesis; assumevalid decision (`ancestor of assumevalid && ancestor of best header && best-header work >= minimum chain work && proof-equivalent time > 2 weeks`) sets `script_checks = false`, which also disables CbTx root checks, best-CL checks, ProTx signature checks and BLS commitment sig checks (`check_sigs`) while **still building the DML**; BIP30 as Core (always enforced unless the `BIP34Hash` block is an ancestor, hence always on devnet and regtest where `BIP34Hash` is null); `nSuperblockStartHash`; script flags; subsidy; `process_special_txs_in_block` **before** the UTXO loop; per-tx `check_tx_inputs`, V24 shared-collateral spend check, fee accumulation, sequence locks, sigops with the P2SH add, script checks queued to the parallel checker, `update_coins`; wait for the checker; IS conflict check only when not in IBD (via `InstantSendOracle`, with ChainLocked-block override); `is_block_value_valid` and `is_block_payee_valid` with `SuperBlockCheck::{NoCheck, AllowDuplicates, DisallowDuplicates}` chosen exactly as Core (best CL height ≥ h or MN sync incomplete → NoCheck); failures from the last two are `BLOCK_RESULT_UNSET` (not punished, still `FAILED_VALID`); write undo; `VALID_SCRIPTS`; set best block in coins and evo. Leniencies that must **not** be tightened (R4 F3/F6): no MN payee check below DIP3 enforcement; accept any coinbase when every MN is PoSe-banned (the credit pool is still credited); the 12.0 budget windows (`h % 16616 < 100` while `h < 614820`) are unbounded under `NoCheck` (mainnet block 332320 overpays about 1176 DASH); superblock payees are never checked during IBD or reindex, only the value bound.

The validation interface is a typed bus:

```rust
pub enum ValidationEvent { BlockConnected { block: Arc<Block>, idx: HeaderCtx, conflicting_txs: Vec<Txid> }, BlockDisconnected { .. },
    UpdatedBlockTip { new: HeaderCtx, fork: Option<HeaderCtx>, ibd: bool }, NewPoWValidBlock { .. }, ChainStateFlushed { locator: BlockLocator },
    TransactionAddedToMempool { tx: Arc<Transaction>, seq: u64 }, TransactionRemovedFromMempool { .. }, BlockChecked { .. },
    NotifyChainLock { .. }, NotifyTransactionLock { .. }, NotifyInstantSendDoubleSpend { .. }, NotifyRecoveredSig { .. },
    NotifyGovernanceObject { .. }, NotifyGovernanceVote { .. }, NotifyMasternodeListChanged { .. } }
pub trait ValidationSubscriber: Send + Sync { fn on_event(&self, ev: &ValidationEvent); }
```

Delivery is synchronous for subscribers that must observe the exact order (mempool, evo caches, llmq phase clock, indexes' queueing, wallet) and queued for the rest (ZMQ, notify scripts), mirroring Core's `CMainSignals` plus its background `SyncWithValidationInterfaceQueue` (the hidden RPC of the same name is supported).

Reindex and import: `-reindex` wipes the index/coins/evo CFs and replays `blk*.dat` through `load_external_block_file` (magic scan, size 80..`MaxBlockSize`, out-of-order parent map, `'R'` flag); `-reindex-chainstate` replays from existing files; `-loadblock=<file>`; `dash-coreimport` can also **translate** a Core `blocks/index` + `chainstate` + `evodb` set so a mainnet import is minutes rather than a day (§2.2.18).

#### 2.2.5 `dash-mempool` (pool, policy, acceptance)

Scope: `core: txmempool.*`, `policy/*`, `txorphanage.*`, `kernel/mempool_*`, and `MemPoolAccept` from `validation.cpp`.

Data structure: entries in a slab with multi-index maps (by txid, by ancestor-fee-rate score, by entry time, by descendant score for eviction), explicit ancestor/descendant sets with the 25 / 101 kvB limits and CPFP carve-out; no RBF (`txn-mempool-conflict`); Dash maps for conflicts (`protx` refs/addresses/pubkey ids/BLS pubkey hashes under both schemes/platform node ids/collaterals, asset-unlock instances/indexes/expiry/pending amount) and the mempool address and spent indexes (`mapAddress`, `mapSpent`) used by `getaddressmempool`.

```rust
pub struct MempoolAccept<'a> { chain: &'a ChainSnapshot, coins: &'a dyn CoinsView, evo: &'a EvoView, pool: &'a mut Mempool,
                               is_oracle: &'a dyn InstantSendOracle, sporks: &'a SporkView, policy: &'a Policy }
impl MempoolAccept<'_> {
    pub fn accept_single(&mut self, tx: Arc<Transaction>, test_accept: bool, bypass_limits: bool, special: SpecialTxCtx) -> AcceptResult;
    pub fn accept_package(&mut self, txs: Vec<Arc<Transaction>>, test_accept: bool) -> PackageResult;   // regtest submitpackage
}
```

`accept_single` follows `PreChecks` → `PolicyScriptChecks` → `ConsensusScriptChecks` → `Finalize` with the Dash insertions: `ContextualCheckTransaction` at the tip; `qc-not-allowed`; `IsStandardSpecialTx`; ISLOCK conflict (`tx-txlock-conflict`) or eviction of conflicting mempool txs when a lock is waiting; asset-unlock index/instance rules (`txn-already-known`, `assetunlock-stale-instance`, refresh/replace); V24 shared-collateral checks; MNHF txs exempt from min relay fee; `CheckSpecialTxForMempool`; `protx-dup` conflict maps; vsize = `max(size, sigops*20)`. Fee estimation is `CBlockPolicyEstimator` ported (buckets 1000..1e7 spacing 1.05, decays .962/.9952/.99931, 60 h staleness, `fee_estimates.dat` format 140100). Persistence is `mempool.dat` v1 (no XOR). The orphan pool is 10 MB / 20 min / 5 min sweep.

Policy constants are the Dash ones (min relay 1000 duff/kB, dust 3000/kB → 546 for P2PKH, `MAX_STANDARD_TX_SIZE` 100,000 strict less-than, `MIN_STANDARD_TX_SIZE` 65, `MAX_P2SH_SIGOPS` 15, `MAX_STANDARD_TX_SIGOPS` 4000, scriptSig ≤ 1650 push-only, versions 1..3, bare multisig ≤ 3 keys, datacarrier 83, shared-collateral template nonstandard except as a shared ProRegTx collateral output).

#### 2.2.6 `dash-p2p` (transport and connection management)

Scope: `core: net.*`, `protocol.*`, `netbase.*`, `netaddress.*`, `bip324.*`, `addrman*`, `addrdb.*`, `banman.*`, `net_permissions.*`, `node/eviction.*`, `netfulfilledman.*`, `evo/mnauth.*`, `masternode/utils.cpp`, plus Tor control, I2P SAM and asmap later (P1).

Design:
- **tokio** for sockets; one reader task and one writer task per peer, bounded channels; a single `ConnMan` actor owning the peer table, semaphores and timers (Core's `ThreadOpenConnections`, `ThreadOpenMasternodeConnections`, `ThreadSocketHandler` collapse into tokio tasks that keep Core's cadences: 1 s MN-connection tick (100 ms right after a connection), 2 min feelers, 5 min extra peers, 15 min peers.dat dump, 1 min MN maintenance).
- **Transport** trait with `V1Transport` and `V2Transport` (BIP324 via the `bip324` crate as in PR #264, with the Dash HKDF salt `"bitcoin_v2_shared_secret" || magic`, garbage 0..4095, the v1-fallback-on-early-disconnect queue, inbound auto-detection of the v1 `version` prefix, short IDs 0–32 and 128–168 from `dash-p2p-core`, and the **send-side version gate**: short form only when the peer's version ≥ 70235, ≥ 70240 for `platformban`, long form before `version` is received). Receive rules: IDs 5 and 29–32 decode to an empty type and are ignored; other IDs invalidate the message. V1: 24-byte header, 3 MiB limit, bad checksum/command drops the message but keeps the connection.
- **ConnMan** exposes `ConnectionType {Inbound, OutboundFullRelay, Manual, Feeler, BlockRelay, AddrFetch}` plus the Dash flags (`masternode_connection`, `masternode_probe`, `masternode_iqr`, `qwatch`, `verified_pro_reg_tx`), limits (125 total, 8 full-relay, 2 block-relay + anchors, 8 addnode, 1 feeler, ≤ 3 verified inbound MN legs per ProTx exempt from limits and eviction), eviction (`SelectNodeToEvict` upstream rules + the 1 s MNAUTH protection on masternodes + "refuse inbound until synced" for an active MN), the 100 ms linger + `shutdown(SHUT_WR)` disconnect, `InactivityCheck` timeouts, `-maxuploadtarget` accounting, permissions (`bloomfilter`, `forcerelay`, `relay`, `noban`, `mempool`, `download`, `addr`).
- **AddrMan** is a port of Core's (1024×64 new, 256×64 tried, same bucket math and `nKey`-salted hashing, asmap) with `peers.dat` V4_MULTIPORT compatibility byte 36 and `anchors.dat`. **BanMan**: `banlist.json` (`banned_nets`), discouragement rolling bloom 50000 @ 1e-6, 24 h default, dump every 15 min. **NetFulfilled**: `netfulfilled.dat` map with the 1 h / 5 min expiry.
- **Misbehavior** is Core's score model: points per offence table (R2 A.2 §4.5), threshold 100, `MaybeDiscourageAndDisconnect` exemptions for `noban`/manual/local.
- **MNAUTH**: challenge in `version`; after verack a masternode sends `mnauth` signed with the basic scheme over `SerializeHash((operator_pk, peer_challenge, is_inbound, our_version))`; receiver rules and penalties as Core; `DeterministicOutboundConnection` dedup; `qsendrecsigs` to quorum relay members; probes disconnect after MNAUTH.
- **Handler registration**: `dash-p2p` only routes. It exposes

```rust
pub trait NetHandler: Send + Sync {
    fn message_types(&self) -> &'static [CommandString];
    fn process_message(&self, peer: &PeerRef, msg: P2pMsg, cx: &mut HandlerCx) -> MessageProcessingResult;  // misbehavior, invs to relay, requests to erase
    fn already_have(&self, inv: &Inv) -> bool;
    fn process_get_data(&self, peer: &PeerRef, inv: &Inv, cx: &mut HandlerCx) -> Option<P2pMsg>;
    fn schedule(&self, s: &mut Scheduler);  fn start(&self);  fn stop(&self);
}
```

Handler order is Core's: the `dash-sync` core handler first, then CoinJoin wallet manager (wallet track), MNAUTH, quorum block processor, platform ban, CLSIG inline, then `NetInstantSend`, `NetSigning`, `NetQuorum`, `NetDKG`/stub, `CoinJoinServer` (MN), `NetGovernance`, `SyncManager`. Messages unknown to every handler are logged "Unknown command" and ignored with no penalty. "BRO-violating" types on block-relay-only, feeler or `-blocksonly` connections disconnect, with Core's list (`tx` yes; `clsig`, `spork`, `govobj`, `isdlock` no).

What is reused from `dash-spv`: the `Transport` trait and V2 code from PR #264 (rebased onto `dash-p2p-core` short-ID tables), `addrv2.rs`, `latency.rs`, the DNS discovery, the test port probe. PR #902 (SPV network-manager rewrite) is **not** a dependency and does not conflict: `dash-p2p` is a new crate; dash-spv keeps its own manager until a later "dash-spv on dash-p2p client mode" task.

#### 2.2.7 `dash-sync` (net_processing)

Scope: `core: net_processing.*`, `txrequest.*`, `blockencodings.*`, `node/sync_manager.*`, `masternode/sync.*`, serving of `getheaders(2)`, `getblocks`, `getdata` (tx/dstx/asset unlock/spork/qfcommit/qsigrec/clsig/isdlock/dsq/platformban), `mempool`, `getcfilters/getcfheaders/getcfcheckpt`, `getmnlistd`, `getqrinfo`, BIP37 filters with the Dash special-tx matching, compact blocks v1, headers2 (DIP-25) both directions, `merkleblock` followed by `isdlock`s.

Structure: one `PeerManager` task consuming an ordered stream of `(peer, message)` events from `dash-p2p` and a 100 ms `SendMessages` tick that walks peers in shuffled order (as `ThreadMessageHandler`), so the observable timing (Poisson trickles 5 s inbound shared / 2 s outbound / 1 s verified outbound MN / none MN-to-MN or noban; non-tx invs on every `SendMessages` call; `MAX_BLOCKS_TO_ANNOUNCE` 8; `getheaders2` for bit-11 peers; 2 min ping) matches dashd. Heavy work (block checks, BLS batch verify) is offloaded; the handler never blocks on it.

Per-peer state is Core's `Peer`/`CNodeState` union: `m_object_request` (generalised `TxRequestTracker` over every inv type, 100,000 announcements cap, 100 in flight, 2 s overload delay, no non-preferred delay on a masternode, `GetObjectInterval` 15 s/5 s/10 s/60 s), block download window 1024 / 16 in flight per peer, stalling and `CHAIN_SYNC_TIMEOUT` logic, headers sync without presync (`MAX_UNCONNECTING_HEADERS` 10, 2000/8000 header limits), compact-block state (3 in flight per block, HB peers), `m_relay_txs`, `m_wants_recsigs`, bloom filter, `qwatch`, addr token bucket (0.1/s, +1000 on getaddr), `getaddr` once per connection with the 21–27 h cache and 23 %/1000 limits.

The post-verack sequence is coded as a single function so it cannot drift: `[mnauth]`, `sendheaders2` **or** `sendheaders`, `sendcmpct(0,1)` if `CanRelay()`, `senddsq(true)` unless rejecting txs, `[qwatch]`; outbound additionally `sendaddrv2` (≥ 70223) → `verack` → `getaddr` before that. `version` carries our services + zero addrMe, `/Dash Core:24.1.0/(devnet.<name>; uacomments)`, start_height, relay flag, the 32-byte mnauth challenge and the masternode flag, written with version 209 framing. The default service bits are `0x1C05` (node) / `0x1C45` (masternode).

**Masternode sync** (`SyncManager`): assets BLOCKCHAIN → GOVERNANCE → FINISHED, 6 s tick, `getsporks` once per peer, `mempool` to outbound non-block-relay peers after reaching the tip (6 s with > 3 peers or 30 s), `govsync(0, empty bloom)` then per-object vote syncs (3 peers per hash on real networks, one object per tick on mainnet), disconnect after `full-sync`, 1 h restart when stuck (not on MNs). `mnsync` RPC drives it.

Fingerprint parity is a conformance deliverable (§4.8, Appendix B); every bullet of R2 A.2 "Notes for a Rust reimplementation" is a test.

#### 2.2.8 `dash-llmq` (quorum runtime, DKG, signing, ChainLocks, InstantSend)

Scope: `core: llmq/{quorums,quorumsman (runtime part),net_quorum,dkgsession*,net_dkg,signing,signing_shares,net_signing,ehf_signals,debug,quorumproofs}`, `active/dkgsession*`, `chainlock/*`, `instantsend/*`, `bls/bls_worker`, `bls/bls_batchverifier`, `bls/bls_ies` (via `dash-pkc` IES). Member selection, scanning and quorum selection live in `dash-evo` and are called from here.

Components (each a struct with its own task or tick, registered as `NetHandler`s and `ValidationSubscriber`s):
- `QuorumManager`: `Quorum { params, commitment, base_block, mined_block, members, vvec: Option<Vec<BlsPk>>, sk_share: Option<BlsSk> }`, built from `q_mc` + members; `quorumdb` keys `q_Qqvvec`/`q_Qsk` keyed by `SHA256d(type || quorumHash || member protxs...)`; cleanup every 576 at `h % 576 == 58` keeping `keep_old_keys`; background pubkey-share warm-up; data recovery (`qgetdata`/`qdata`, masks, 300 s + 60 s expiry, 64/4096 caps, `-llmq-data-recovery`, `-llmq-qvvec-sync`); quorum connections (`EnsureQuorumConnections` for the newest `keep_old_connections` quorums, relay ring `(i + 2^k) mod n`, full mesh under spork 21 with the deterministic initiator, watch connections).
- `DkgSessionManager` (MN only): handlers per `(type, quorum_index)`; phase clock from `UpdatedBlockTip` (`stage = (h - idx) % interval`, `phase = stage / phase_blocks + 1`, phases Init..Idle); per-phase sleep `my_idx * ((phase_blocks-1) * spacing * 1000 / size) * factor` (0.05, 0.1 for Commit; none on regtest); message intake rules and penalties (R2 A.4 §3 "Intake"), per-protx queue quota `size*2`, batched signature verification (`AggregateInsecure` + `VerifyInsecureAggregated`, fallback on duplicate sign hashes); `ActiveDkgSession` with Contribute (polynomial of `threshold` secrets, `vvec[j] = sk_j·G1`, shares at `id_m = BlsShareId(proTxHash raw bytes)`, IES to each operator key), Complain (badConnection criteria: not connected under spork 21, version < 70242, no tx relay, no `NODE_COMPACT_FILTERS`, > 5 failed outbound attempts, platform-banned), Justify, Commit (`validMembers`, `quorumVvec = Σ`, `quorumSig = skShare.sign(commitmentHash)`), Finalize (group by `validMembers`, `membersSig = AggregateSecure`, `quorumSig = recover`, version from rotation/V19 at base), single-member path; `dkgdb` keys `qdkg_V/S/E` (vvec always basic encoding), cleanup beyond `max_store_depth`; `quorum dkgstatus` debug state. Mineable commitments pool feeds `dash-miner`; `qfcommit` P2P intake with the ≥ 70239 stale-repropagation ban.
- `SigningManager` + `SigShareManager`: `CRecoveredSig` DB (`recsigdb` keys `rs_r/rs_t/rs_h/rs_s/rs_v/rs_vt` and the Platform-type `rs_p/rs_m/rs_u`, `-maxrecsigsage` 604800), conflict rule (different msgHash for same (type,id) on non-Platform types), vote recording (`rs_v`) with `allowDiffMsgHashSigning` for Platform/EHF, `AsyncSignIfMember` (quorum at `tip-8` or explicit), session announcements/invs/`qgetsigs`/`qbsigs`/`qsigshare` (spork 21 only), recovery member order `sort by H((proTxHash, id))`, backoff `min(2^n·2 s, 10 s)`, batch verification caps 1000/10000, `Recover` by Lagrange at `threshold`.
- `ChainLockHandler` + `ChainLockSigner`: `clsig` processing (seen cache, height ≤ best ignored, +10 on invalid, `UpdateBestChainlock`, enforcement via `ChainState::enforce_block` + `activate_best_chain`), `HasChainLock`/`HasConflictingChainLock` implementing the chainstate `ChainLockOracle`, rebuild from coinbase CLs at startup, `-chainlocknotify`; signer conditions (synced, spork 19 == 0, tip unsigned, best CL below tip, `TX_CONFIRM_THRESHOLD = 5` blocks of islocked-or-10-min-old txs when IS enabled), request id `H("clsig" || i32 h)`.
- `InstantSendManager` + `NetInstantSend` + `InstantSendSigner`: `isdb` keys (`is_v=1`, `is_i`, `is_tx`, `is_in`, `is_m`, `is_a1/a2`), `isdlock` verification (cycle hash at a cycle boundary, `nSignHeight` rule, offset 0 then `dkgInterval` retry, non-rotating regtest override), lock inputs incl. the `plwdtx` synthetic outpoint for v2 asset unlocks, conflict resolution (`ResolveBlockConflicts`, ChainLocked-block override, recsig truncation), mempool `tx-txlock-conflict` hooks (`InstantSendOracle`), archive pruning at 100 blocks; signer (`CheckCanLock`, per-input `inlock` ids, `islock` id over the inputs vector, retroactive signing on `BlockConnected`, `WAIT_FOR_ISLOCK_TIMEOUT` 10 min interplay with mining).
- `EhfSigner`: for `useEHF` deployments inside their window and not yet signalled, sign `H("mnhf" || i64 bit)` and submit the type-7 tx to the mempool.
- `PlatformBan` handling: validation (Evo target, `tip-576 ≤ h ≤ tip`, Platform quorum sig over `H("PlatformPoSeBan" || protx || i32 h)`), `mncache` state, relay, `ResetPlatformBan` on ProUpServ.
- `QuorumProofs` (DASHNC02 `getquorumproofchain`/`verifyquorumproofchain`) is P1, after M7.

BLS execution: a dedicated rayon pool ("bls worker", `-parbls`, default = cores, max 33) for contributions, vvec sums, share verification batches of 32, and all batch verifiers; results return through channels. `BlsCtx` is threaded explicitly (rule R4).

#### 2.2.9 `dash-governance`

Scope: `core: governance/*`, `rpc/governance.cpp` handlers (in `dash-rpc`), `flat-database.h` for `governance.dat`.

- **Types** (`Governance::Object`, `GovernanceVote`, `Superblock`, `GovernancePayment`, `Proposal` JSON validator) come from `dash-primitives::gov` (exists) plus the three distinct hashes implemented as tested helpers:
  - object hash = `SHA256d(hashParent || i32 revision || i64 time || ser_string(hex(vchData)) || COutPoint || u8 0 || u32 0xffffffff || ser_vec(vchSig))` (no `collateralHash`, includes the signature);
  - signature hash = `SerializeHash(fields 1..7)`;
  - data hash = `Hash(ser_string(hex(vchData)))`;
  - vote hash = `SHA256d(COutPoint || u8 0 || u32 0xffffffff || parent || i32 signal || i32 outcome || i64 time)` (signal before outcome, unlike the wire order).
- **Store**: `GovernanceStore` (objects with disk-only fields, erased map with the proposal-forever / trigger `creation + 2·cycle + 10 min` rule, orphan votes `CacheMultiMap` 1,000,000 / 10 min, per-MN `RateCheckBuffer` (5 timestamps, `rate >= 2·1.1/cycleSeconds`), `last_mn_list_for_voting_keys`). Persisted in Core's `CFlatDB` envelope (`magicGovernanceCache` + magic + `"CGovernanceManager-Version-16"` + body + SHA256d) so a Core `governance.dat` loads; a version-string mismatch rebuilds from the network, exactly like Core.
- **Processing**: `process_object` (known/postponed/erased check → rate check → `is_valid_locally(check_collateral)` → postpone on 1–5 confirmations → add; misbehavior 20 otherwise), collateral check through `dash-indexes::TxIndex` (P2PKH/unspendable outputs, `OP_RETURN <hash>` ≥ 1 DASH, 6 confirmations), `process_vote` order (known → MN missing (20) → signal checks → obsolete → equal-time lower-outcome loses → 1 h rate limit → signature (20) → store/meta/ZMQ), voting-key vs operator-key rule (proposal+FUNDING → ECDSA voting key with the `"DarkCoin Signed Message:\n"` message form on mainnet/devnet/regtest and raw-hash form on testnet; everything else BLS basic `VerifyInsecure`), vote invalidation on key changes and on MN removal, `check_and_remove` with the 10 min deletion delay, tallies with Evo weight 4 and `max(minQuorum, W/10)` / `2W/3` thresholds.
- **Superblocks**: `SuperblockManager` (`is_valid_block_height`, `get_payments_limit` = `superblock_subsidy_inner(nBits = powLimit compact on min-difficulty nets else 1, h-1, V20) × cycle`, trigger parsing with `ParsePaymentAmount` strictness, `get_best_superblock` with map-order tie-break, `is_valid` with the V24 `lastIdx+1` duplicate rule, trigger expiry rules). It implements the chainstate `SuperblockOracle` (`is_superblock_triggered`, `is_valid_superblock`, `payments_limit`) and the `MnSyncOracle` plumbing for `SuperBlockCheck::NoCheck`.
- **Signer** (MN only): `create_superblock_candidate` (approved proposals sorted absYes desc / hash desc, `now + (nextSB-h)·2.62·60` time estimate, greedy budget fill with the ±2 h fudge window, payments sorted by hash desc), `create_governance_trigger` (byte-exact JSON template `{"event_block_height": H, "payment_addresses": "a|b", "payment_amounts": "x.xxxxxxxx|...", "proposal_hashes": "h|h", "type":2}`, dedup by data hash, only if we are `GetProjectedMNPayees(tip).front()`), `vote_governance_triggers` (FUNDING-YES on ours, FUNDING-NO on other future triggers).
- **P2P** (`NetGovernance` handler): `govsync` semantics (full list vs object fetch vs vote sync with bloom, `ssc(10|11, n)`, `netfulfilled` `govsync`/`govsync-votes-<hash>` with +20 on repeat, bloom size check +100), `govobj`/`govobjvote` announced-or-requested gate, +100 on malformed vote length, relay batching every 5 s (1 s mockable), orphan parent requests every 5 min.
- `-disablegovernance` (forced by `-prune`, forbidden on MNs) and the `-txindex` requirement outside regtest are enforced in `dash-node` init.

#### 2.2.10 `dash-coinjoin`

Scope now: `core: coinjoin/{common,coinjoin,server,options,util (builder parts needed by server)}` and the DSTX relay policy in `net_processing`. Scope later (wallet track, M8): `client`, `walletman`, `wallet/coinjoin.cpp` rounds/salt/`IsFullyMixed`, the `coinjoin`/`coinjoinsalt` RPCs.

- `common`: denominations (`1<<0..1<<4` → 10.0001 / 1.00001 / 0.100001 / 0.0100001 / 0.00100001 DASH), collateral [10,000, 40,000] duffs, `COINJOIN_ENTRY_MAX_SIZE` 9 (10 in rebalance), max 180 inputs/outputs (200 per side post-V24 with the ×10 and `% 9` structure rules), promotion/demotion constants, `MixSideCounts::is_covered`, `is_valid_in_outs` (standard N:N, promotion 10:1, demotion 1:10, final composition rule, zero fee, unique P2PKH scripts, IS-locked mempool parents), `is_collateral_valid` (ATMP test-accept with the 0.1 DASH sane-fee cap), message structs from `dash-p2p-core` (BS9c) with the `dsq` hash-includes-sig / sighash-excludes-sig pair.
- `server` (MN only): session state machine IDLE → QUEUE → ACCEPTING_ENTRIES → SIGNING → (ERROR), `dsa` acceptance (`ERR_QUEUE_FULL`, `ERR_MN_LIST`, `ERR_RECENT` via `IsMixingThresholdExceeded = last_dsq + enabledMNs/5 > nDsqCount`), rebalance gating (peer ≥ 70241 and V24 at tip, no legacy participant mixing), `IsSessionReady` (sides covered and (collaterals ≥ max or 30 s timeout and ≥ min)), `dsi` entry rules, `CreateFinalTransaction` (BIP69 sort, `dsf` to each entry), `dss` signature application with `VerifyScript(P2SH|STRICTENC)`, `CommitFinalTransaction` (`PrioritiseTransaction(+0.1 DASH)`, ATMP, DSTX relay, `dsc MSG_SUCCESS`, `ChargeRandomFees` ≈10 %), timeouts (30 s / 15 s signing) with `ChargeFees` ≈34 %, `dsq` validation (+10/+100 rules, ±30 s), `DSTXManager` (24-block expiry, chainlock expiry).
- DSTX relay policy lives in `dash-sync` using `dash-coinjoin::common::validate_dstx` (structure incl. the one-block PREMATURE tolerance around V24, MN resolution up to 24 blocks back, `IsValidForMixingTxes` ≤ 5, BLS sig, priority delta, `CanAnnounceDstxTo` fallback to `MSG_TX` for peers < 70241).
- `mncache.dat` (`CMasternodeMetaMan-Version-5`: meta infos, `nDsqCount`, used masternodes) is owned by a small `dash-coinjoin::meta` module shared with `dash-llmq` (platform ban fields) and `dash-governance` (gov vote counts); its `CFlatDB` format is Core's.

#### 2.2.11 `dash-sporks`

~400 lines: `SporkId` table (10001 SPORK_2, 10016 SPORK_17, 10018 SPORK_19, 10020 SPORK_21, 10022 SPORK_23; 10023 deprecated), defaults `4070908800` (OFF), **mainnet hardening** (`GetSporkValue` ignores the table on non-test chains: all 0 except SPORK_21 = 1), `is_active(id) = value < adjusted_now`, `EvalSpork` for 21/23 (0 = all types, 1 = all but 100_67/400_60/400_85), message `{i32 id, i64 value, i64 time, vchSig ≤ 65}` with `GetHash` over all fields including the signature, signature forms (testnet: compact over `SHA256d(id||value||time)`; others: signmessage of the decimal string with the DarkCoin magic), `time ≤ now + 2 h`, +100 on invalid, newer replaces and relays, `sporks.dat` (`CSporkManager-Version-3`), `-sporkaddr`/`-sporkkey`, `sporkupdate`. Implements `SporkView` used by llmq/instantsend/chainlock/coinjoin/sync.

#### 2.2.12 `dash-indexes`

`BaseIndex` port: each index is a `ValidationSubscriber` with its own background syncer (`'B'` locator per index, rewind on reorg, prune locks), DB in the `Cf::*Index` column families (Core key formats, §2.2.3), `getindexinfo`. Indexes: **txindex** (default **on**; `'t'+txid → DiskTxPos`), **addressindex** (`'a'` 66-byte keys with BE height/position, `'u'` unspent keys; reads undo data; incompatible with prune), **spentindex**, **timestampindex**, **blockfilterindex** (`basic`; Dash-extended BIP158 elements from `evo/specialtx_filter.cpp`; `'V'` version 2; `fltr%05u.dat` records `hash || filter` with no magic; served through `dash-sync` when `-peerblockfilters`; auto-enabled on masternodes), **coinstatsindex** (MuHash3072, and `gettxoutsetinfo` `hash_serialized_2` **with Core's operator-precedence bug reproduced**: it writes `VARINT(1)` instead of the height/coinbase code, `kernel/coinstats.cpp:77`, R4 F7; `muhash` is the chain-replay comparison key). The legacy in-`blocks/index` layout (`'a'/'u'/'p'/'s'` + `'F'` flags) is handled by `dash-coreimport`.

The BIP158 builder in `dash/src/bip158.rs` lacks the special-tx elements and the X11 keys; the node's builder lives in `dash-indexes::filter` on base-sdk types and is vector-tested against Core's `blockfilters.json` (X11 hashes) plus oracle-generated filters for blocks with ProTx/asset locks. dash-spv can adopt it later.

#### 2.2.13 `dash-miner`

`BlockAssembler::create_new_block(script_pubkey, options)` as `core: node/miner.cpp`: reserve 1000 bytes/100 sigops; `nBlockMaxSize` clamp; `ComputeBlockVersion`; after DIP0003 add mineable quorum commitments first (fee 0, sigops 0; null commitments with the right version/index when nothing better is known), then `add_package_txs` with the credit-pool diff (subsidy 0), EHF signal stage, `TestPackageTransactions` (final, `CheckSpecialTx(check_sigs=false)`, V24 shared-collateral, `IsTxSafeForMining` 10-minute rule when IS enabled); coinbase (`OP_RETURN` scriptSig after DIP3, `nVersion 3 / nType 5`, CbTx version by V24&&V20 → 4 / V20 → 3 / DIP0008 → 2 / else 1, `CalcCbTxBestChainlock` with the "at least prev diff + 1" rule and the V19 floor, `creditPoolBalance`, `merkleRootMNList`/`Quorums`/`AssetUnlocks`); `FillBlockPayments` (MN outputs subtracted from `vout[0]`, superblock outputs added). GBT fields exactly as R2 A.1 §4 (`coinbase_payload`, `masternode[]`, `superblock[]`, flags, `masternode_payments_started/enforced`, `superblocks_started/enabled`, `rules` always containing `"csv"`), refusal conditions (no peers, IBD, MN sync incomplete at a superblock height), longpoll, `proposal` mode via `test_block_validity`. Regtest internal miner for `generatetoaddress`/`generatetodescriptor`/`generateblock` (X11 grinding). Template caching strategy in §3.7.

#### 2.2.14 `dash-rpc` (+ REST, `dash-zmq`)

- **HTTP**: `hyper` 1.x on tokio; POST only (405 otherwise); Basic auth with the 250 ms delay + `WWW-Authenticate` on failure; cookie (`__cookie__:<64 hex>` in `.cookie` via `.tmp` + rename, deleted at shutdown), `-rpcuser/-rpcpassword`, `-rpcauth=user:salt$hmac-sha256`; `-rpcbind` only with `-rpcallowip`; 127.0.0.0/8 and ::1 always allowed; header limit 8192; `-rpcthreads` 4 and `-rpcworkqueue` 16 semantics plus `-rpcexternaluser`/`-rpcexternalworkqueue` separate queue; `/` and `/wallet/<name>` routes; batch arrays; HTTP status mapping (-32600 → 400, -32601 → 404, else 500; 503 during warmup → -28 in body); `-rpcservertimeout` 30 s.
- **Dispatch**: `RpcTable` keyed by `"method"` and `"method sub"`; the Core squash rule (if `params[0]` is a string and `"method " + params[0]` is registered, shift params); named args with `|` aliases and the `args` key; `-rpcwhitelist` matched as `method + params[0]` with no space; `help [cmd] [sub]` generated from `RpcHelpMan`-style declarations (`RpcDef { name, args: Vec<RpcArg>, results, examples }`) so `help` output and arg validation (`-rpcdoccheck`) match Core's shapes; error codes are Bitcoin's (no Dash additions); `-deprecatedrpc` flags `service`, `legacy_mn`, `softforks`, `banscore`, `walletwarningfield`, `permissive_bool`.
- **JSON**: `serde_json` with `preserve_order` (Core's UniValue preserves insertion order; `dash-cli` output is diffed by humans and scripts). Amounts serialize as Core's `ValueFromAmount` (8 decimals, no exponent). Hash strings in Core display order.
- **Handlers** are grouped as Core's files (`blockchain`, `mempool`, `mining`, `net`, `node`, `rawtransaction`, `output_script`, `fees`, `signmessage`, `txoutproof`, `masternode`, `coinjoin`, `governance`, `evo`, `quorums`, `zmq`) and take `&NodeContext` (read-only snapshot handles + command channels to the owning tasks). Wallet handlers live in `dash-node-wallet` and register onto the same table only when a wallet is enabled, with the "wallet-aware override" trick for `protx list`/`protx info`/`getcoinjoininfo`.
- **Response types**: `rpc-json` structs are the single source of truth; the server serializes them, the client/CLI deserialize them, and the oracle tests deserialize dashd's responses into the same structs. `rpc-json` is extended to v24 by merging `upstream/integration/v4.2-core-v24` (+1.7K) early (BS task of the RPC stream).
- **REST** (`-rest`): the 12 endpoints of R2 A.6 §3 with `.bin/.hex/.json`, 503 in warmup, BIP64 `getutxos`.
- **`dash-zmq`** (feature `zmq`, `zmq` crate): 19 topics, 3-part messages `[topic][body][u32 LE seq]`, per-notifier HWM, hashes byte-reversed, `getzmqnotifications`.

#### 2.2.15 `dash-node` (daemon)

- **Args/config** (`dash_node::args`): a port of Core's `ArgsManager` semantics (`-foo`, `-nofoo`, `-foo=0`, repeated args, network sections `[main]/[test]/[devnet]/[regtest]`, NETWORK_ONLY options ignored with a warning outside main, `includeconf` only from the file and non-recursive, `-conf` only on the command line, exactly one chain selector, `settings.json` read/write, `-allowignoredconf`, `-help`/`-help-debug` with Core's categories), with **the full option table of R2 A.6 §6.2** registered up front (unknown options are errors, exactly as Core), so Dash's functional tests and existing `dash.conf` files work. Sensitive values are redacted from logs (R1 issue #1122 class).
- **Datadir** (§2.5), `.lock` flock, `dashd.pid`, `debug.log` with Core's categories (plus `-debug/-debugexclude/-loglevel/-logips/-logtimestamps/-logthreadnames/-logsourcelocations/-shrinkdebugfile`), `-daemon/-daemonwait` (fork via `daemonize`-style double fork on Unix), `-startupnotify/-shutdownnotify/-blocknotify/-chainlocknotify/-alertnotify` (detached `sh -c`).
- **Init order** mirrors `core: init.cpp` `AppInitMain`: params + overrides → datadir/lock → logging → store open (+ migrations) → block index load (+ reindex) → chainstate/evo load, `VerifyDB` (`-checkblocks/-checklevel`) → indexes → mempool load → sporks/mncache/netfulfilled/governance load → LLMQ/IS/CL managers → active masternode (`-masternodeblsprivkey`, forced settings: `-txindex=1`, `-peerbloomfilters=1`, `-peerblockfilters=1`, `-blockfilterindex=basic`, `-disablewallet` default, `maxconnections ≥ 125`, no prune, governance on, not blocksonly, `-maxuploadtarget=0`) → wallet(s) → P2P listen/connect → RPC out of warmup. Shutdown is the reverse with a final flush, `peers.dat`/`mempool.dat`/flat DB dumps.
- **Scheduler**: a tokio-based `Scheduler` with mockable time (`setmocktime`, `mockscheduler` on regtest) feeding every periodic job.
- **Active masternode manager**: states WAITING_FOR_PROTX / POSE_BANNED / REMOVED / OPERATOR_KEY_CHANGED / PROTX_IP_CHANGED / READY / ERROR; local address discovery (`GetLocal` toward 8.8.8.8, regtest/devnet fallbacks), MN lookup by operator key, primary netInfo match, self-connect test on routable networks, reset conditions on each tip.
- **Stats** (`-stats*`): statsd UDP client with Core's metric names (P2).
- Binary `dashd-rs` (name: §7 Q1). Version string `/Dash Core:24.1.0/` by default so the fingerprint matches; the real implementation name goes only in `getnetworkinfo.subversion`? No: Core puts `subversion` in both. Decision: the UA is a config knob (`-uaoverride`, hidden) defaulting to the dashd string for the fingerprint goal; `getnetworkinfo` adds an extra `implementation` field (additive JSON, harmless for Core clients).

#### 2.2.16 `dash-cli` (binary `dash-cli-rs`)

Built on `rpc-client` (blocking `jsonrpc`, fine for a CLI) and `rpc-json`. Supports Core's `dash-cli` surface: positional and `-named` args, `-rpcwallet` (→ `/wallet/<name>`), `-rpcwait/-rpcwaittimeout`, `-stdin/-stdinrpcpass/-stdinwalletpassphrase`, `-color`, `-rpcclienttimeout` 900, cookie/user/password/`-rpcconnect/-rpcport`, `-conf/-datadir` and chain selectors (reads the same `dash.conf`), `-getinfo` (batched `getnetworkinfo`/`getblockchaininfo`/`getwalletinfo`/`getbalances` + `mn_in/mn_out/mn_total` + `coinjoin_balance`), `-netinfo [0-4|help]`, `-addrinfo`, `-generate [n] [maxtries]`. Output formatting = Core's (UniValue pretty-print, 2-space indent). `dash-tx`/`dash-util`/`dash-wallet` equivalents are later subcommands (`dash-cli-rs tx ...`), P1.

#### 2.2.17 `dash-node-wallet` (wallet integration: extend, don't replace)

The node hosts `key-wallet-manager::WalletManager<ManagedWalletInfo>` (one per loaded wallet file) behind a `NodeWallet` adapter:

```rust
pub struct NodeWallet { name: String, manager: RwLock<WalletManager>, db: WalletDb /* SQLite */, crypter: Option<Crypter>, birth_height: u32 }
impl ValidationSubscriber for NodeWallet { /* BlockConnected → process_block_for_wallets; BlockDisconnected → disconnect_block (new);
    TransactionAddedToMempool → process_mempool_transaction; NotifyTransactionLock → process_instant_send_lock; NotifyChainLock → apply_chain_lock */ }
```

Required extensions to the existing crates (all additive, coordinated with the wallet track and R5):
- `WalletInterface::disconnect_block(&mut self, block: &Block, height)` and transaction-record rollback (issue #854), plus `rescan(from_height)` support from the node's block files (no filters needed: the node has every block).
- Encryption at rest (R5 US-W-05, R1 §9.8): `WalletDb` stores the seed/xprv as AES-256-CBC under a master key derived like Core (`BytesToKeySHA512AES`, ≥ 25000 rounds calibrated to 100 ms) so `encryptwallet/walletpassphrase[change]/walletlock` and `mixingonly` semantics match; the mnemonic passphrase is supported (`Wallet::from_mnemonic_with_passphrase`, the `""` hard-coding is removed).
- Persistence: SQLite file per wallet (`wallets/<name>/wallet.sqlite`), schema `main(key BLOB PRIMARY KEY, value BLOB)` **in Core's record format** for the records we also produce (`walletdescriptor*`, `tx`, `name`, `purpose`, `cj_salt`, `cj_pending_obs`, `g_object`, `lockedutxo`, `bestblock_nomerkle`, `flags`, `minversion`, `version`, `mkey`, `ckey`...) so that (a) a Core descriptor wallet opens directly, (b) our wallet can be opened by Core when it contains only Core-known records (open question §7 Q4), and (c) `dash-wallet dump` files round-trip. `application_id` = BE32 magic, `user_version` 0.
- Core wallet import (`dash-coreimport::wallet`): SQLite descriptor wallets directly; BDB 4.8 legacy wallets through a read-only btree page parser modelled on `core: test/functional/test_framework/bdb.py` (4096-byte pages, `main` subdatabase; overflow pages supported since `tx` records exceed a page), reproducing the Dash quirks: `hdchain`/`chdchain` (seed, mnemonic, passphrase, per-account counters), BIP39 checksum XOR bug and non-NFKD passphrase (accept both forms, prefer the one that derives known keys), legacy HD on BIP44 (CoinJoin on the same chain) vs descriptor DIP9 `4'` CoinJoin, keypool 1000 vs gap limit 30 (import sets the pool/gap from the counters), `cj_salt` (needed for `IsFullyMixed` parity), `ps_salt` migration, loose imported keys (new `AccountType::ImportedKeys`), watch-only scripts, labels/purposes, `lockedutxo`. `importwallet`/`dumpwallet` text format, WIF, `importdescriptors` (`pkh/sh/multi/sortedmulti/combo/addr/raw` only).
- Wallet RPCs (R2b `wallet` category, 71) implemented over the manager; ProTx builders with BLS signing for `protx register*/update_*/revoke/shared_*` (R5 US-MO/US-SM), `masternode outputs`, `gobject prepare/vote-many/vote-alias`, `signmessage`; PSBT (no witness fields) and P2SH/multisig signing are wallet-track items (M8).
- The desktop and mobile apps keep using `rs-platform-wallet`/FFI as today; `dash-node-wallet` is a third consumer of the same `key-wallet` crates, not a fork.

#### 2.2.18 `dash-coreimport`

Pure-Rust readers for a Core datadir, used by `-importcoredatadir=<path>` (one-shot), by `dash-conformance` and by BS2:
- `leveldb`: a read-only LevelDB reader (crate `rusty-leveldb` or an in-house reader of `MANIFEST`/`.ldb`/`.log` with CRC and the `\0obfuscate_key` XOR applied to values when present).
- `blockfiles`: iterate `blk*.dat`/`rev*.dat` (zero-magic stop, per-network magic), undo checksum verification with the previous block hash.
- `blockindex` → our `Cf::BlockIndex` (same bytes); `chainstate` → `Cf::Coins` (same bytes minus XOR); `evodb` → `Cf::Evo` (same keys; `dmn_S3` re-sorted; `dmn_D3` legacy diffs migrated; `mnhf_s` upgraded; `q_Q*` moved to `Cf::Llmq`); `llmq/*` DBs; `indexes/*` optional; legacy in-`blocks/index` address/spent/timestamp rows + `'F'` flags.
- Consistency rule: blocks, index, chainstate and evodb must agree on one tip (`b_b4` vs `'B'`), else refuse and suggest `-reindex-chainstate`.
- Flat DBs (`governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`), `peers.dat` (format 4, compat 36), `anchors.dat`, `banlist.json`, `mempool.dat` v1, `settings.json`, `onion_v3_private_key`, `i2p_private_key`.
- Wallets (§2.2.17).

#### 2.2.19 `dash-testkit` and `dash-conformance`

`dash-testkit` lifts `dash-spv/src/test_utils` (`DashdTestContext`, `DashCoreNode`, `MasternodeTestContext`, `contrib/setup-dashd.py` datasets `regtest-40000`/`regtest-200`/`regtest-mn`) into a shared crate so dash-spv, the node and the benchmarks use one harness; adds `RustNode` (spawns `dashd-rs` with a temp datadir and the same RPC driver), `MixedNetwork { dashd: Vec<_>, rust: Vec<_> }`, mock time control, and log retention (`DASHD_TEST_RETAIN_DIR`). `dash-conformance` is §4.

### 2.3 What is reused from dash-spv, what is replaced

| dash-spv piece | Node decision | How |
|---|---|---|
| Wire codecs (`dashcore::network`, headers2, mnlistdiff, qrinfo, CL/IS types) | **reuse as reference, replace with base-sdk types** | BS9 ports them to `dash-p2p-core`; BS2 proves equality; dash-spv keeps its copies until Phase 3.9 |
| BLS quorum-commitment / CL / IS verification (`dash/src/sml/*`, `dash-spv/src/validation/*`) | **reuse algorithms, re-implement on base-sdk types in `dash-evo`/`dash-llmq`; regenerate fixtures** | R4 F12 found that `mn_list_diff_0_2227096.bin` and the testnet `.bin` are not Core-produced (wrong IPv4 encoding, MN root mismatch), so every capture is regenerated from a dashd oracle before use; the `dashd_masternode` tests stay |
| `MasternodeListEngine` + rotated quorum construction (`dash-spv/src/sml_engine`) | **replace** (SPV reconstructs from diffs; the node derives from blocks) | member-selection code is cross-tested against it on the captures |
| Storage (`DiskStorageManager`, segments, migrator) | **replace** | RocksDB + Core flat files |
| Sync managers, `SyncCoordinator`, checkpoints, fork handling | **replace** | `dash-sync` + `dash-chainstate` |
| Mempool (wallet-filtered) | **replace** | `dash-mempool` |
| Network manager / `Peer` / handshake | **partially reuse** | PR #264 `Transport` + V2, `addrv2`, `latency`, discovery; handshake rewritten for fingerprint parity; PR #902 unaffected |
| `WalletInterface` seam | **reuse and extend** | `disconnect_block`, rescan, encryption |
| dashd test harness (`test_utils`) | **reuse, moved** | `dash-testkit` |
| `dash-spv` CLI/config | **replace** | Core-compatible args |
| `dash-spv-bench` scripts (docker dashd peers, netem, pprof) | **reuse** | folded into `node-bench` |

### 2.4 Coexistence with SPV, wallet and Platform consumers

- No existing crate changes its public API for the node except through the Base SDK milestone's correctness fixes (BS3 X11, BS4 constants/opcodes, BS5 segwit removal, BS6b Network newtype) and the additive wallet extensions (§2.2.17). Each of those is a normal upstream-style PR with a migration note; Platform's pin moves when lklimek chooses (R3 §4.3).
- `cargo deny` rule R2 guarantees node crates never create a second reason to change `dashcore`.
- dash-spv can later run in "client mode" on `dash-p2p` and use `dash-indexes::filter`, but that is not a program deliverable.
- FFI crates (`key-wallet-ffi`, `dash-spv-ffi`) are untouched; PR #776 (delete them) is orthogonal.

### 2.5 Datadir layout (parallel product; separate from Core's)

```
<datadir>/                      default ~/.dashcore-rs (Unix), see §7 Q2; dash.conf and settings.json as Core
  <net>/                        "" | testnet3 | devnet-<name> | regtest   (same as Core so configs port)
    blocks/blk%05u.dat rev%05u.dat          Core format, per-network magic, no XOR
    db/                                     RocksDB; CFs: blockindex, coins, evo, llmq, txindex, addrindex, spentindex, tsindex, filterindex, coinstats, meta
    indexes/blockfilter/basic/fltr%05u.dat  Core format
    wallets/<name>/wallet.sqlite  backups/
    peers.dat anchors.dat banlist.json mempool.dat fee_estimates.dat governance.dat mncache.dat netfulfilled.dat sporks.dat
    settings.json .cookie .lock debug.log dashd.pid onion_v3_private_key i2p_private_key
    message_capture/<addr>/msgs_{recv,sent}.dat   (-capturemessages; Core format; used by the fingerprint diff)
```

Core-format files are chosen wherever Core's format is simple and stable so that the import is a copy and the conformance tooling (and Core's own `contrib/` scripts) work on both datadirs. The `meta` CF holds our schema version and the import provenance (source datadir, Core version, tip) for `getblockchaininfo`-level reporting.

### 2.6 Chain selection, ChainLock enforcement, reorgs and pruning (chainstate details)

- **Candidate set**: `set_block_index_candidates` ordered by `(chain_work, sequence_id)` as Core's `CBlockIndexWorkComparator`; entries with `FAILED_MASK` or `CONFLICT_CHAINLOCK` are excluded, and both statuses propagate to descendants in `find_most_work_chain` (Core treats a CL-conflict exactly like a failed block for selection).
- **`activate_best_chain`**: loop {find most-work chain; compute fork point; disconnect tips (with undo) down to the fork, collecting disconnected txs into a `DisconnectedBlockTransactions` pool for mempool re-add; connect forward up to 32 blocks per step (Core's batch), flushing per Core's `FlushStateToDisk(IF_NEEDED)` policy; publish `UpdatedBlockTip`} until the tip is the best candidate; `NewPoWValidBlock` for compact-block pre-announcement when the block is valid up to TRANSACTIONS and extends the tip.
- **Invalid chain handling**: `invalid_block_found` marks `FAILED_VALID` and descendants `FAILED_CHILD`, removes them from candidates, `check_fork_warning_conditions`; `BLOCK_RESULT_UNSET` failures (payee/value) are **not** forwarded to peer punishment but still mark the index (Core semantics) and are cleared by `reconsiderblock`.
- **ChainLock enforcement** (`enforce_block(height, hash)`): mark every block at that height on another branch `CONFLICT_CHAINLOCK` (and descendants), disconnect the active chain if it is on a conflicting branch, add the locked block's branch back to candidates, then `activate_best_chain`. Header acceptance consults `ChainLockOracle::has_conflicting_chain_lock` and still inserts the header with `CONFLICT_CHAINLOCK` before rejecting (`bad-chainlock`), as Core does, so `getchaintips` shows `conflicting` tips.
- **`preciousblock`**: Core's `nSequenceId` trick (`n_block_reverse_sequence_id` decrement and re-insert).
- **Block index load**: parallel over `'b'` records (rayon), re-check PoW per entry (X11), rebuild `prev` links, `chain_work` and `n_chain_tx` in height order, derive candidates, verify `'l'`/`'f'` file info, `'R'` reindex flag, `'F'` prunedblockfiles; `-checkblockindex` consistency walk (on by default on regtest).
- **Pruning** (`-prune`): `MIN_BLOCKS_TO_KEEP` 288, `PRUNE_LOCK_BUFFER` 10, prune locks from indexes, `pruneblockchain` RPC, unlink files and `HAVE_DATA/HAVE_UNDO` clearing in the index batch; incompatible with txindex/addressindex/spentindex/governance (init error), forces `-txindex=0` soft-set. P1 (M9) but the index/file layer supports it from M2 so no format change is needed later.
- **assumeutxo**: not in scope (Core's is regtest-only and carries an evo snapshot in flux). The `BLOCK_ASSUMED_VALID` bit is reserved in the status codec so a future `loadtxoutset` does not need a format change.
- **VerifyDB** (`-checkblocks`/`-checklevel`, `verifychain`): levels 0–4 as Core (read, check, undo, disconnect/reconnect, full).

### 2.7 Bridge rules at the wallet seam

`dash-node-wallet` converts `Arc<dash_primitives::Block>` to `dashcore::Block` once per block via `dash-bridge` (encode with `BaseCodec`, decode with `consensus::Decodable`; about 1–3 ms per 1 MB block, off the validation thread) and caches the result alongside the block for the wallet, indexes that still use `dashcore` types (none planned) and ZMQ raw payloads (which use the original bytes, no conversion). Hash newtypes convert by `from_byte_array`/`to_byte_array` with no copies. When R3 Phase 3.7 lands, `dash-bridge`'s `From` impls become identity and the cache is removed; the adapter's API does not change.

---

## 3. Concurrency and performance architecture (Q3)

### 3.1 Threads vs async: the split

| Layer | Model | Why |
|---|---|---|
| `dash-consensus`, `dash-evo` | pure functions, no threads | determinism, differential testing, no_std |
| `dash-chainstate` write path (`accept_block`, `activate_best_chain`, flush) | **one validation thread** owning `ChainState` (an actor with a bounded command queue) | Core's `cs_main` discipline without a global lock: the single writer is the invariant, not a mutex |
| Script checks inside `connect_block` | **rayon** pool (`-par`, default = cores, max 15 as Core; `-par=0` → cores) via a `CheckQueue`-like scoped batch | CPU-bound, embarrassingly parallel; same parallelism knob as dashd for "comparable config" |
| BLS verification (commitments, sig shares, recovered sigs, DKG batches) | separate **rayon** pool (`-parbls`, max 33) | keeps BLS bursts (DKG at cycle boundaries) from starving script checks |
| Pre-validation of downloaded blocks | **rayon** `par_iter` over queued blocks | context-free work (§3.2) |
| Readers (RPC, P2P serving, indexes, wallet) | `Arc<ChainSnapshot>` published with `arc_swap` on every tip change + `CoinsView` read-through with a sharded `RwLock` on the cache | RPC never waits on validation; `getblockcount` during IBD is lock-free |
| Sockets, HTTP, ZMQ, timers | **tokio** multi-thread runtime | I/O concurrency; cancellation; the ecosystem crates |
| `dash-sync` `PeerManager` | one tokio task, sequential message handling, 100 ms `SendMessages` tick | fingerprint timing parity and Core-equivalent ordering guarantees; heavy work is offloaded |
| Mempool | `Mutex<Mempool>` (parking_lot) held briefly; `MempoolAccept` runs on the caller's thread with a `ChainSnapshot` | ATMP rate is bounded by script checks, not by the lock |
| Indexes | one background task per index, batched writes | as Core |
| LLMQ components | tokio tasks for protocol timers; rayon for crypto | DKG phases are block-driven; sleeps are mockable |

No `async` in consensus, evo, chainstate, mempool or store APIs. The chainstate actor exposes `ChainCommands` (a `tokio::sync::mpsc` of boxed closures returning oneshot results) so async callers can `await` a sync operation without blocking a runtime worker.

### 3.2 Block connect pipeline (IBD and steady state)

```
 peers ──(block bytes)──► [decode + stage A pre-validation, rayon, out of order]
                             ▲ bounded by the 1024-block download window
   stage A (per block, parallel, no chain state):
     - decode (BaseCodec), txids (SHA256d, SIMD sha2), X11 header hash, merkle root (+ CVE-2012-2459),
       check_transaction for every tx, legacy sigop counts, size checks,
       pre-serialize each tx once for the legacy sighash (prefix/suffix caches), payload decode + trivial checks,
       special-tx payload hashes (inputsHash, SerializeHash(payload)), BLS deserialization of commitment fields
   ─► ordered queue (by height in the active chain) ─►
   stage B (validation thread, sequential):
     - parallel UTXO prefetch: multi_get of every input not in the cache (RocksDB MultiGet, sorted keys)
     - connect_block: special-tx processing (evo), UTXO apply, queue script checks to rayon, wait, Dash checks
     - stage undo + evo writes into the pending KV batch; publish snapshot; emit ValidationEvents
   stage C (background): blk/rev file appends (sequential writer thread, fdatasync at flush points), index syncers
```

Core does stage A inside `ProcessNewBlock` on the message handler thread and prefetches UTXOs one at a time on cache miss. Moving stage A off the critical thread and batching the misses is the largest structural gain available; it does not change any rule.

### 3.3 Parallel script and signature validation; caches

- `CheckQueue` equivalent: `connect_block` collects `(tx_index, input_index, coin, flags)` work items and runs `rayon::scope` with chunking; the first failure cancels (atomic flag) and reports the Core error string. Deterministic failure selection is not needed by Core either (it reports the first failure it sees), but our tests pin the error of the **lowest** `(tx, input)` for reproducibility.
- **Signature cache** (`SigCache`): sharded (64 shards) cuckoo/robin-hood map keyed by `SHA256(salt || sighash || pubkey || sig)` truncated to 128 bits, entries pinned by `-maxsigcachesize` (32 MiB split 1:1 with the script cache as Core). Hits skip the secp256k1 verify; mempool acceptance populates it, so block connect of a mined tx costs no ECDSA (same as Core, but ours is lock-free per shard).
- **Script execution cache**: `SHA256(nonce || txid || flags)` → verified, consulted before queuing work (Core semantics incl. erase-after-block for mempool-only entries).
- **Sighash precomputation**: for `SIGHASH_ALL` inputs the per-tx serialization prefix/suffix is cached (stage A) so each input's sighash is one SHA256d over a reassembled buffer rather than a fresh serializer walk; for other hash types the full legacy path runs. The result is bit-identical by construction and vector-tested.
- **secp256k1**: the `secp256k1` crate (same libsecp256k1) with a static context; batch/parallel only (no "fast ECDSA" tricks). This is parity, not 2x (§3.8).
- **X11**: `dash-pow` or `rs-x11-hash` (BS3 benchmark); only ~2.5M header hashes on mainnet, negligible in IBD.
- **BLS**: `fast_verify_aggregates`/`verify_aggregates` batched per block for commitments and per message batch for shares; `blst` is the same backend as Core's planned migration (dash#7678), so parity plus batching.

### 3.4 Coins cache and DB write batching

- `CoinsCache` with `FoldHash`/`ahash` keyed by `OutPoint` (36 bytes), `CompactScript` values (§2.2.4), FRESH/DIRTY flags, memory accounting that counts actual allocations (hashbrown bucket size + boxed script), and `-dbcache` as the budget shared with RocksDB's block cache (Core splits `nTotalCache` into block index 2 MiB max, coins DB cache, coins cache; we mirror the split so "comparable config" holds).
- Flush policy: Core's (`FlushStateToDisk` IF_NEEDED when over budget or every 24 h / 1 h periodic; ALWAYS on shutdown and after `ActivateBestChain` when evodb cache ≥ 64 MiB equivalent). Writes are **sorted by key**, chunked at `-dbbatchsize`, with the `'H'` marker; coins, evo, block index and index locators go in the same batch so a crash leaves a consistent tip.
- **Parallel prefetch**: before `connect_block`, collect the inputs missing from the cache, `multi_get` them in one call (RocksDB does parallel IO internally), insert as clean entries. During IBD with a 300 MiB cache this turns thousands of random point reads per block into one vectored read.
- RocksDB memtable sized so an IBD flush (~16–64 MiB of coins deltas) lands in one write buffer; WAL enabled (durability on crash between flushes is still "replay from last flushed tip", as Core).
- Undo data is written to `rev*.dat` by the file writer thread as blocks connect; the index entry's `HAVE_UNDO` is set only after the write returns, as Core.

### 3.5 BLS work and LLMQ latencies

- DKG contributions and vvec aggregation on the BLS pool; shares verified in batches of 32 (Core's batch size) with early exit; IES decryption off the handler task.
- Sig-share sessions use lock-free per-session maps; recovery triggers immediately on the threshold-th share (Core has a worker thread with a sleep loop; our wake-up is event-driven, which is where the "recsig latency / IS lock / ChainLock creation latency" metrics should beat dashd).
- `quorum sign`/`platformsign` RPC calls go straight into the signing manager without the 100 ms handler cadence.

### 3.6 Memory (RSS) plan

| Component | Core (dashd, defaults) | This design | Lever |
|---|---|---|---|
| Coins cache per entry | ~130 B (CTxOut + prevector + unordered_map node) | ~70 B (`CompactScript`, hashbrown) | about 1.8x more coins per MiB at the same `-dbcache` |
| Block index | ~150 B/entry × 2.5M (+ map) | ~120 B/entry in an arena, `u32` links, `Arith256` chain work | 25 % smaller, no per-node allocation |
| Mempool | `CTxMemPoolEntry` + 5 indexes | slab + 4 indexes, `Arc<Transaction>` shared with relay | similar; accounting matches `-maxmempool` |
| Sig/script caches | 32 MiB fixed | 32 MiB fixed | parity |
| DML lists | immer HAMT per cached height | `imbl` HAMT with `Arc<DmnState>` sharing | similar; fewer copies of `netInfo` |
| RocksDB | n/a (leveldb 2 MiB + dbcache share) | block cache from the `-dbcache` share, bloom filters ~10 bits/key (≈ 8 MiB for 6M coins) | parity target; measured by B1 |
| Allocator | glibc malloc | `jemalloc` (as dash-spv-bench already does) | lower fragmentation on long runs |

Steady-state and peak RSS are B1 metrics; the target is "never higher than dashd at the same `-dbcache`/`-maxmempool`" with the coins-cache lever as the planned win.

### 3.7 Mempool throughput and GBT

- ATMP throughput: policy and script checks run on the caller thread (P2P handler offloads to a small `blocking` pool), the mempool lock is held only for the insert; sigcache hits make re-validation of relayed txs cheap. Target: 2x dashd's `AcceptToMemoryPool_ms` percentiles under the `mempool_stress` bench.
- GBT: Core rebuilds the template on every call (`CreateNewBlock` walks the mempool by ancestor fee rate). We keep a **template cache** keyed by `(tip, mempool_sequence, mn_list_hash, mineable_commitments_hash)`; it is rebuilt by a background task when the mempool sequence changes (debounced 100 ms) so a GBT call returns a ready template; `longpollid` semantics are preserved; a cold build uses the same algorithm as Core (so the template *contents* are identical for the same mempool, which the oracle test checks).
- Fee estimation reads are lock-free snapshots.

### 3.8 Where 2x comes from, metric by metric (honest expectations)

| Charter metric | Expected vs dashd | Source of the gain (or why parity) |
|---|---|---|
| IBD to tip (± assumevalid), reindex | **2x target credible** | stage-A pipelining (~35 % of connect time in Core is CheckBlock/txid/merkle/serialization), parallel UTXO prefetch (IO wait removal), compact coins (fewer flushes at the same dbcache), sorted batched flush, no evodb double-DB flush, jemalloc |
| Per-block connect latency (steady state) | 1.5–2x | prefetch + pipelining; bounded below by secp256k1 time ÷ cores |
| Script/sig validation throughput | **parity** on raw ECDSA; 1.2–1.5x per block | same libsecp256k1; gains only from sighash precompute, lock-free sigcache, better work distribution |
| Mempool accept throughput/latency | 2x | short critical section, no `cs_main`, cached special-tx conflict maps |
| GBT time | 10x+ (cached), 1.5x (cold) | template cache; cold path identical algorithm |
| Startup-to-ready | 2x | parallel block-index load/PoW recheck, lazy quorum warm-up |
| Peak/steady RSS | ≤ dashd, aim 0.7x | §3.6 |
| Disk footprint | parity (same block files) to 0.9x (RocksDB compression on index CFs) | formats are Core's |
| CPU per block | 1.5x fewer cycles | fewer allocations and copies; `Arc<Block>` sharing between P2P, validation, indexes, wallet |
| RPC latency/throughput | 2x+ | snapshot reads without `cs_main`; hyper vs libevent |
| DKG round time | **parity** (protocol-bound: block-driven phases, cooperative sleeps) | only the per-message processing is faster |
| recsig latency, IS lock latency, ChainLock creation latency | 1.5–2x on the node's share | event-driven recovery instead of sleep loops; batched verification without the 100 ms cadence |
| CoinJoin round servicing | parity (protocol timeouts dominate) | — |
| X11 hashing | parity or better only after `dash-pow` AES-NI dispatch | BS3 benchmark decides the backend |

Consensus-critical optimisations (sighash precompute, parallel prefetch ordering, template cache) each ship with a differential test that proves the output equals the unoptimised path on real chain data, per the charter's "never trade correctness for speed".

### 3.9 Benchmark contract with B1

`node-bench` runs both nodes on the same box with the same `dash.conf` subset (`-dbcache`, `-par`, `-maxmempool`, `-txindex`, `-blockfilterindex`), the same peers (docker dashd 23.1.x/24.0 peers with netem from `dash-spv-bench`), and records per PR: IBD wall time to assumevalid and to tip on mainnet and testnet; `ConnectBlock_ms` p50/p99 over the last 10,000 blocks; `CheckInputScripts_ms`; ATMP p50/p99 under `mempool_stress`; GBT p50/p99; startup-to-ready; peak/steady RSS; datadir bytes; CPU-seconds per 1,000 blocks; RPC p50/p99 for `getblock 2`, `getrawtransaction`, `protx list`, `quorum list`; DKG round time, recsig/IS/CL latencies and CoinJoin round time on the mixed regtest harness. dashd exports the same timings through its `-stats*` statsd client (R2 A.6 §7), which gives directly comparable series. Results go to `research/B1-dashd-baseline.md` and `STATUS.md` per merged PR; a regression on any metric blocks the merge (charter).

---

## 4. Verification architecture (Q4)

### 4.1 Layers of evidence and where each one lives

| Layer | What it proves | Where | When it runs |
|---|---|---|---|
| L0 Vectors | pure rules equal Core's reference data | `dash-conformance/vectors/` runners against `dash-consensus`, `dash-evo`, `dash-pkc` | every PR (seconds) |
| L1 Oracle unit differential | every pure function equals Core's implementation on arbitrary inputs | `libdashoracle` (Core branch) linked into `dash-conformance` tests and fuzz targets | every PR (proptest, 1k cases) + nightly fuzz |
| L2 Codec equality | both codecs and both Rust/C++ codecs agree byte-for-byte on all real data | BS2 harness + oracle round-trips | PR (regtest sets), nightly (full chains) |
| L3 Chain replay comparison | our chainstate, DML, quorums, credit pool, filters equal dashd's at every k-th height | `dash-conformance replay` against a synced dashd (RPC) and against imported Core DBs | nightly (testnet full, mainnet from checkpoints), release (mainnet full) |
| L4 Functional tests | behaviour parity with Dash Core at the RPC/P2P level | Dash `test/functional` run against `dashd-rs` via a Core-branch shim | PR (smoke subset), nightly (full list), gated % in STATUS |
| L5 Interop | Rust and dashd masternodes share quorums, CLs, IS locks, CoinJoin sessions, governance | `dash-testkit::MixedNetwork` on regtest and a long-running devnet | PR (small), nightly (devnet soak) |
| L6 Fingerprint | a UA-spoofing peer cannot distinguish | message/timing trace diff | PR |
| L7 Benchmarks | never slower, 2x where claimed | `node-bench` | per merged PR |

The crate layout makes L0–L2 cheap because the consensus surface is pure functions over base-sdk types; L3 is possible because our DB keys are Core's; L4 is possible because args/RPC/P2P are designed for drop-in parity. R4 numbers the same ideas L0–L6: its L2 is our L3 replay, its L3 (GBT-proposal mutation oracle) is folded into our L3, and its L4–L6 are our L4–L6 (§4.11).

### 4.2 Core JSON vectors (L0)

Copied from `core: src/test/data` and `test/functional/data` into `dash-conformance/vectors/<core-commit>/` with a provenance file; a script refreshes them from a Core checkout.

| Vector | Exercises | Runner |
|---|---|---|
| `script_tests.json` (271 KB, DIP0020 cases) | `eval_script`/`verify_script`, flags, errors | `dash-consensus` tests; **also** the libdashconsensus/oracle comparison |
| `sighash.json` (205 KB) | legacy `signature_hash` incl. payloads | `dash-consensus` |
| `tx_valid.json` / `tx_invalid.json` | `check_transaction` + script flags | `dash-consensus` |
| `trivially_valid.json` / `trivially_invalid.json` | special-tx trivial validation per type/variant | `dash-primitives::Checkable` (BS17) and `dash-evo::specialtx` |
| `blockfilters.json` (X11) | Dash BIP158 | `dash-indexes::filter` |
| `key_io_*.json`, `base58_encode_decode.json` | addresses/WIF | `dash-script::Recipient`, `dashcore-crypto` |
| `bip39_vectors.json` | mnemonic/seed (TREZOR) | `key-wallet` (plus the Core checksum-bug acceptance test) |
| `proposals_*.json` | governance proposal validator | `dash-governance` |
| `asmap.raw` | asmap | `dash-p2p` |
| `rpc_getblockstats.json`, `rpc_psbt.json`, `rpc_decodescript.json`, `rpc_bip67.json` | RPC shapes | `dash-rpc` |
| `blockheader_testnet3.hex` | header DoS tree | `dash-sync` |
| `quorum_proof.json` | DASHNC02 | `dash-llmq::quorumproofs` (P1) |
| `bitcoin-util-test.json` | dash-tx | `dash-cli tx` (P1) |
| Core unit-test literals (`pow_tests` 24 DGW pairs, `subsidy_tests` heights, `dip14_tests`, `evo_simplifiedmns_tests` root, `evo_islock_tests`, `bls_tests`, `llmq_*`) | extracted into JSON by a one-time script in the Core branch | respective crates |
| rust-dashcore fixtures (11 mainnet MnListDiff/QRInfo captures, `dashd_masternode` datasets) | SML/rotation | `dash-evo` |

### 4.3 The oracle: `libdashoracle` and in-process differential fuzzing (L1)

Dash Core still ships `libdashconsensus` (`core: src/script/bitcoinconsensus.{h,cpp}`), a deprecated C API around `VerifyScript` that accepts only five flags and reports no `ScriptError` (R4 A§6); it is a cheap accept/reject smoke oracle, not full coverage, which is why the program builds its own shim. The program adds, on Dash Core branch `node/oracle`, a `src/oracle/` library (built by the existing autotools as a shared library `libdashoracle`) exposing `extern "C"` entry points over **pure** Core functions. Nothing in Core's runtime changes; the branch is test hooks only, as the brief allows.

Entry points (all take/return raw bytes; errors as Core's reject strings):

```
oracle_eval_script(flags, scriptSig, scriptPubKey, tx, nIn, amount) -> (ok, script_error)
oracle_sighash(tx, nIn, scriptCode, hashType) -> 32 bytes
oracle_check_transaction(tx) -> (ok, reason)
oracle_tx_payload_roundtrip(tx) -> (txid, instance_hash, reencoded, payload_json)
oracle_block_roundtrip(block) -> (hash, merkle, mutated, reencoded)
oracle_next_work_required(chain_params, headers[]) -> nBits      // DGW/KGW/BTC/min-difficulty
oracle_block_subsidy(prevBits, prevHeight, v20, params) -> amount; oracle_superblock_subsidy(...)
oracle_masternode_payment(height, value, era, params) -> amount; oracle_split_shares(total, shares[])
oracle_versionbits_state(params, period_ends[], signals) -> state
oracle_dml_apply(prev_list, block, prev_header, coins[], quorum_members, deployment_flags) -> (new_list_sorted, payee, error)
oracle_sml_root(list) -> 32 bytes; oracle_quorums_root(commitments, params) -> 32 bytes
oracle_quorum_members(list_at_work_block, snapshots[], params, base_height, modifier_inputs) -> members[]
oracle_select_quorum(quorums[], selection_hash, rotated) -> quorum_hash
oracle_sign_hash(type, quorumHash, id, msgHash) -> 32 bytes; oracle_request_id(kind, payload) -> 32 bytes
oracle_credit_pool_step(prev_pool, block, params, era) -> pool
oracle_bls_{legacy,basic}_{serialize,deserialize,sign,verify,aggregate_secure,verify_secure,threshold_split,recover,ies_encrypt,ies_decrypt,hash_to_g2}
oracle_gov_hashes(object|vote) -> hashes; oracle_dsq_hashes(dsq) -> hashes
oracle_bip158_filter(block, undo_scripts[]) -> filter bytes
oracle_compress_amount / oracle_compress_script / oracle_varint / oracle_compactsize
oracle_bip39_check(mnemonic) -> ok; oracle_bip39_seed(mnemonic, passphrase) -> 64 bytes
```

The Rust side (`dash-conformance/oracle`) links it with `bindgen` (feature-gated; CI builds Core once and caches the `.so` under `~/.rust-dashcore-test/oracle/<commit>`). Three uses:

1. **Property tests** (`proptest`) on every PR: random transactions/scripts/headers/lists → both implementations → equal.
2. **Differential fuzzing** (`cargo-fuzz`/`honggfuzz`, nightly, plus a 60 s smoke on PR): targets `fuzz_script_eval`, `fuzz_sighash`, `fuzz_tx_decode_roundtrip`, `fuzz_block_decode_roundtrip`, `fuzz_special_payload` (one per type incl. 10–12), `fuzz_dml_apply`, `fuzz_quorum_members_rotation`, `fuzz_credit_pool`, `fuzz_versionbits`, `fuzz_bls_codec`, `fuzz_gov_object`, `fuzz_gov_vote`, `fuzz_isdlock`, `fuzz_clsig`, `fuzz_p2p_msg` (every command, both framings), `fuzz_headers2`, `fuzz_bip158`, `fuzz_compressor`, `fuzz_addrman_serialize`, `fuzz_diskblockindex`, `fuzz_txundo`. Each target's invariant is "same accept/reject and same bytes as the oracle". Corpora seed from the vectors and real chain data. This is the charter's "differential fuzzing of consensus code and parsers".
3. **Vector generation** for base-sdk corpora (BS7, BS16): synthetic v24 transactions, DKG transcripts, legacy BLS vectors with documented provenance.

For code paths that are not pure in Core (anything touching `CChainState`), the RPC oracle (§4.4) is used instead.

### 4.4 RPC oracle and chain-replay comparison hooks (L3)

`dash-conformance replay` drives two nodes over the same blocks and compares derived state through RPC, using `rpc-json` types on both sides:

| At every k-th height (k = 1 on regtest, 576 on testnet, 2016 on mainnet, always at deployment heights ± 2) | Compared via |
|---|---|
| block index status, chain work, `getchaintips` | `getblockheader`, `getchaintips` |
| UTXO set hash (`muhash`, `hash_serialized_3`), tx count, total amount | `gettxoutsetinfo` (coinstatsindex on both) |
| DML content hash: our full list vs `protx listdiff 0 <h>` / `masternodelist` at height, plus the CbTx `merkleRootMNList` | `protx listdiff`, `getblock 2` |
| quorum set per type (`quorum list`, `quorum listextended`, `quorum info` incl. `quorumPublicKey`, `membersSig`), rotation members (`quorum memberof`) | RPC |
| credit pool balance and limit | `getcreditpoolinfo` |
| EHF signals / versionbits | `getdeploymentinfo` |
| BIP158 filter hash/header | `getblockfilter` |
| mempool acceptance verdicts on a replayed tx stream (recorded from the network) | `testmempoolaccept` |
| block template contents for identical mempools (regtest) | `getblocktemplate` |
| per-height `getblockstats` (subsidy, fees, sigops, sizes), `masternode payments <h>`, `quorum rotationinfo`, `protx listdiff h-1 h` | RPC (R4's L2 digest set; the one-off extraction is about 13M calls, 1.5–4 h on this box, then hours per replay) |
| **block and tx mutation verdicts**: mutated blocks built on the tip are submitted to dashd with `getblocktemplate {"mode":"proposal"}`, which runs `TestBlockValidity(fCheckPOW=false)` and returns the reject reason with no side effects (R4 F9); the same mutations go through our `test_block_validity`; `testmempoolaccept` for txs | RPC, on regtest, testnet **and mainnet** |
| `decoderawtransaction` JSON for every special tx | RPC |

Two sources of truth: (a) a **live dashd** synced on the same box (its RPC is the oracle), and (b) **imported Core databases** (`dash-coreimport` reads a dashd datadir snapshot; our `Cf::Evo` and `Cf::Coins` contents are diffed key-by-key against the LevelDB contents, with the documented exceptions: `dmn_S3` ordering, obfuscation, `internalId`). (b) runs without a dashd process and catches persistence-format drift.

Hooks inside the node (`-replaycheck=<rpc-url>` dev option): after each `BlockConnected`, the node itself queries the oracle for that height and halts on the first divergence with a full state dump (`dumptxoutset`, our DML JSON, the oracle's), which turns "the chain diverged at height H" into a reproducible unit test in minutes.

### 4.5 State hashes we implement to make comparison cheap

- `MuHash3072` and `hash_serialized_3` (Core `kernel/coinstats`) so `gettxoutsetinfo` is comparable without a coinstats index rebuild on the oracle.
- Core already has a canonical full-state codec, `evo::EvoSnapshot` with `CanonicalMNListHash` (`evo/snapshot.h:501-520`, R4 F8), not yet wired to RPC. The Core test branch exposes it as a hidden RPC `evodb canonicalhash <height>`; the Rust node implements the same codec (sorted by proTxHash) so DML comparison is one 32-byte string per height, and the codec doubles as the future snapshot-sync interop format.
- `QuorumSetHash` per type likewise.

### 4.6 Running Dash's functional tests against the Rust node (L4)

Prerequisites designed in from day one: Core-compatible args (§2.2.15) including every regtest/devnet override, `-sporkkey`, `-mocktime`, `-dip3params`, `-llmqtest*params`; the RPC surface used by `test_framework` (`generate*`, `setmocktime`, `mockscheduler`, `mnsync`, `sporkupdate`, `protx`, `quorum`, `getbestchainlock`, `waitfor*`, `syncwithvalidationinterfacequeue`, `invalidateblock`, `reconsiderblock`, `addconnection`, `addpeeraddress`, `sendmsgtopeer`, `mnauth`, `setmnthreadactive`, `quorum dkgsimerror` (implemented as a debug feature), `submitpackage`); the P2P behaviour `p2p.py`/`v2_p2p.py` expect; `debug.log` lines some tests grep (`assert_debug_log` patterns are listed and matched verbatim).

The framework already supports per-node binaries (`add_nodes(binary=[...])`, R4 F10), so Core branch `node/functional-shim` only adds an environment override (`DASHD_RS_BINARY`, applied to all or to selected node indexes) and a `--rust-skiplist` file. Masternodes run with `-disablewallet` forced, so mixed dashd/Rust masternode tests need no Rust wallet (dashd node 0 keeps the wallet), and R4's census shows the 35 tier-1 tests need only about 60 RPCs. The runner is `dash-conformance functional --list <file>`, which maps each of the 264 tests to one of {pass, skip(reason), fail} and writes the matrix to `STATUS.md`. Ordering of adoption follows the milestones (§5): `feature_block`, `p2p_*` basic, `rpc_blockchain` first; `feature_llmq_*`/`feature_dip3_*`/`feature_governance*`/`p2p_dstx` by M7; `wallet_*` by M8. A test that cannot pass because of an intentional difference (e.g. Qt-only, BDB-write) is documented in the skip list with the reason.

### 4.7 Mixed regtest/devnet interop (L5)

`dash-testkit::MixedNetwork` starts `N` dashd masternodes (from the `regtest-mn` dataset, `DashdTestContext`) and `M` Rust masternodes on one regtest, registers them (ProRegTx through either node's wallet), mines quorums (`mine_quorum`-equivalent helpers in Rust mirroring `DashTestFramework`), and asserts: identical `quorum list` on all nodes, DKG success with mixed members (and `quorum dkgstatus` agreement), CL and IS locks produced by mixed quorums verified by all, recovered sigs relayed both ways, CoinJoin sessions between a dashd client and a Rust server (and vice versa), governance objects/votes and a superblock trigger agreed by all, `platformban` relay, data recovery across implementations. A standing **devnet** (`-devnet=rdc-<date>`) with mixed masternodes soaks continuously from M6; its logs feed the latency metrics.

### 4.8 Fingerprint parity (L6)

`dash-conformance fingerprint` uses a scripted peer (the Python `p2p.py` mininode or a Rust probe) that performs identical interaction scripts against dashd and `dashd-rs` and records both message streams in Core's `message_capture` format with timestamps. The diff is over: message sequence, field-level contents (with allowances for nonces/challenges/time), size of `addr` replies, trickle statistics (inter-arrival distributions compared with a two-sample test), penalty outcomes for each misbehaviour script, disconnect timing, BIP324 garbage-length distribution and short-ID usage per version, `headers2` encoding bytes, `getdata` responses and `notfound` lists. Appendix B is the checklist; every line is a test case. The deliverable is "the diff is empty except the allowed list", tracked per PR.

### 4.9 Fuzzing beyond the oracle

Non-differential targets (crash/hang/UB): every `BaseCodec` decoder on `dash-primitives`/`dash-p2p-core` (round-trip), the BIP324 transport state machine, the V1 framing, `ArgsManager`, config/`settings.json` parsing, `addrman` deserialization, `peers.dat`, `mempool.dat`, flat DBs, LevelDB reader, BDB reader, RPC JSON request parsing, REST paths, ZMQ formatting, headers2 compression/decompression, DKG message framing (`MaxDKGMessageSize` walk), `AUTOBITSET`/`DYNBITSET`. All node crates are `#![forbid(unsafe_code)]` except `dash-store` (FFI to RocksDB) and the oracle bindings; `cargo miri` runs on `dash-consensus`/`dash-evo`; ASAN/TSAN jobs (fixing the broken sanitizer path filters, R1 §4) run on the node crates.

### 4.10 Gates per milestone

Each milestone (§5) lists which layers must be green to close. Rules for `node/integration` (PROCESS.md) gain: L0 and L1 property tests on every PR; BS2 and the regtest subset of L3/L4/L5 on every PR touching consensus/evo/p2p/llmq; nightly full L2/L3/L4/L5; a consensus-critical PR needs the oracle differential test for its function and the `code-review` skill at high effort plus the GPT reviewer (PROCESS roles).

### 4.11 Reconciliation with R4

R4 landed while this design was being written. Everything below is adopted; nothing in R4 changes the crate boundaries or the type-stack decision.

| R4 item | Where it lands in this design |
|---|---|
| F1 EMBARGO-1 | BS17 (hash the re-serialization), BS2 (harness asserts txid = SHA256d of re-serialization), §7 Q11 (disclosure) |
| F2 assumevalid also skips CbTx roots, credit pool, commitment and ProTx signatures | §2.2.4 `script_checks` scope; replay runs with assumevalid off (§4.4) |
| F3 / F6 payee and value leniencies (old-budget windows unbounded, all-banned accept-anything, no payee check pre-enforcement) | §2.2.4 "do not tighten" list; `SuperblockOracle::NoCheck` path |
| F4 BTC/KGW eras tolerance-only; DGW exact from 68590 | §2.2.1 floating-point policy (tables dropped for KGW; `x*x`, per-height `getblockstats` subsidy diff) |
| F5 `merkleRootQuorums` covers disabled types | §2.2.2 `cbtx` |
| F7 `hash_serialized_2` precedence bug | §2.2.12 coinstats |
| F8 `EvoSnapshot` / `CanonicalMNListHash` | §4.5 (hidden RPC in the Core test branch; same codec in Rust) |
| F9 `getblocktemplate` proposal mode as a zero-PoW validity oracle | §4.4 mutation verdicts row (usable on mainnet) |
| F10 per-node binaries in the functional framework; masternodes force `-disablewallet` | §4.6 shim scope; §4.7 mixed networks without a Rust wallet |
| F11 handshake timings (`getsporks` +0.5–2 s, `govsync` +8 s, 137-byte `version`) | Appendix B item 4 |
| F12 rust-dashcore MnListDiff fixtures not Core-produced | §2.3 fixtures regenerated from the oracle |
| E§11.1 four `dash-pkc` fixes (aggregation weights mod r, IES key from basic encoding, invalid-not-error decoding, wrapper semantics) | BS14 (now a `yes` base-sdk change, gating M3 commitment-signature checks) |
| §1.6 `MinBIP9WarningHeight` floor; i32 threshold overflow; historic bits 10/11/12 | §2.2.1 `deployments` notes |
| §1.4 ECDSA consensus layer (lax DER, S normalisation, hybrid keys), `UNKNOWN_ERROR` mapping, mandatory/non-mandatory classification | §2.2.1 fidelity notes |
| B§4 checkpoint semantics (highest checkpoint present in the index) | `CheckpointView` signature |
| §1.10 validity inputs that are not chain data | the injected oracle traits (`ChainLockOracle`, `InstantSendOracle`, `SuperblockOracle`, `MnSyncOracle`, `Clock`) with `NoOp`/IBD defaults (§2.1, §2.2.4, Appendix A) |
| §1.15 quirk register (34 items) | each becomes a named test `dash-conformance/quirks/r4_<n>.rs` and is cited from the owning module's docs; the register is the review checklist for consensus PRs |
| §1.14 difficulty ratings (XL: DML, rotation, DKG, governance, P2P) | consistent with the staffing and xhigh review tiers in §5.3 |
| L2 digest set and cost estimate | §4.4 table row |
| Pinned references per network (v23.1.x mainnet, v24.0.0-rcN testnet, `develop` watch list; `evo_shares` bit 14) | §1.3 v24 paragraph, §7 Q3 |
| Chain scans (special txs `nVersion > 3`, the EMBARGO-1 scan, `bestCLHeightDiff >= height`) | T16 |

Where R4 leaves a choice open, this design decides: R4 "evaluate `dash-primitives` as the tx model" → committed (D1) with BS17; R4 "BLS: L in pure Rust, M via FFI to dashbls" → pure Rust via `dash-pkc` with BS14/BS16, dashbls FFI as the fallback (§7 Q6); R4 "V24 as a separate milestone pinned to the final v24.0.0 tag" → V24 rules are built in from BS7 (testnet needs them now) but mainnet validation is pinned to v23.1.x until pasta re-pins (§7 Q3).


---

## 5. Sequencing (Q5)

### 5.1 Milestones

Sizes assume 6–10 concurrent implementers (Opus high by default), one reviewer stream, and the manager merging continuously. Weeks are elapsed, not effort.

| M | Name | Exit criteria (all verified by tests in the repo) | Weeks |
|---|---|---|---|
| M0 | Foundations | Node-start gate of §1.4 merged (BS1–BS4, BS6, BS7, BS15, BS17); `dash-testkit` extracted; `libdashoracle` branch builds and `oracle_eval_script`/`oracle_sighash`/`oracle_tx_payload_roundtrip` work; B1 dashd baseline recorded; BS2 green on regtest datasets | 0–5 |
| M1 | Consensus core | `dash-consensus` complete: all L0 vectors pass; L1 property tests for script, sighash, tx/block checks, DGW/KGW/BTC retarget, subsidy, payments, versionbits; fuzz targets `fuzz_script_eval`/`fuzz_sighash`/`fuzz_tx_decode_roundtrip` running nightly with no divergences for 48 h | 1–7 |
| M2 | Chainstate + storage | `dash-store`, `dash-chainstate` (index, files, coins, undo, connect/disconnect, reorg, assumevalid, flush, reindex, loadblock, Core import of blocks/index/chainstate); regtest-40000 IBD from blk files; testnet and mainnet IBD **up to DIP0003 enforcement** (1047200) agreeing with dashd on UTXO MuHash at every 2016th height; crash-consistency test (`feature_dbcrash`-style) | 3–10 |
| M3 | Evo state | `dash-evo` complete; mainnet IBD from genesis to tip agreeing with dashd on block status, UTXO hash, DML hash, quorum sets, credit pool at every checkpoint (L3); evodb import comparison; L1 differential for DML apply, member selection (rotated incl. snapshots), credit pool, EHF; testnet IBD incl. V24 blocks (when activated there) | 6–14 |
| M4 | P2P + sync | `dash-p2p`, `dash-sync`, `dash-sporks`; inbound/outbound, V1/V2, addrman, banman, headers2 both ways, block download, compact blocks, tx relay, serving (getheaders/getdata/mempool/cfilters/getmnlistd/getqrinfo), misbehaviour scores, mnsync; IBD from live peers on testnet and mainnet; fingerprint diff empty for the handshake/sync/relay scripts; functional `p2p_*` basic set + `feature_block` + `p2p_sendheaders_compressed` + `p2p_v2_*` pass | 8–17 |
| M5 | Mempool, miner, RPC, CLI | `dash-mempool` (ATMP, policy, fees, orphans, persistence), `dash-miner` (CreateNewBlock/GBT/regtest generate), `dash-rpc` core categories (control/network/blockchain/rawtransactions/util/mining/addressindex read paths), `dash-indexes` txindex + blockfilterindex, `dash-cli`; functional `mempool_*`, `mining_*`, `rpc_blockchain/rawtransaction/net/misc/...`, `feature_txindex`, `rpc_getblockfilter` pass; template-content equality vs dashd on regtest; benchmarks for ATMP/GBT/RPC recorded | 11–20 |
| M6 | LLMQ (non-MN then MN) | `dash-llmq`: quorum manager, CL/IS verification + enforcement + DBs (node role) then DKG, sig shares, recovered sigs, EHF signer, data recovery (MN role); mixed regtest: Rust MN participates in DKGs with dashd, signs CLs/IS; functional `feature_llmq_signing/chainlocks/rotation/connections/data_recovery/dkgerrors/is_retroactive/is_cl_conflicts/evo/singlenode/simplepose`, `rpc_quorum`, `rpc_verifychainlock/islock`, `p2p_instantsend`, `p2p_quorum_data` pass; devnet soak starts | 14–26 |
| M7 | Masternode operation | active MN manager, MNAUTH + MN connections, PoSe, platformban, `dash-governance` (objects/votes/superblocks/triggers/signer), `dash-coinjoin` server, address/spent/timestamp indexes, ZMQ, REST, stats; functional `feature_dip3_*`, `feature_dip4_*`, `feature_governance*`, `feature_sporks`, `feature_mnehf`, `feature_asset_locks`, `feature_masternode_*`, `feature_protx_version`, `p2p_dstx`, `p2p_platform_ban`, `rpc_masternode`, `rpc_mnauth`, `interface_zmq_dash`, `feature_addressindex/spentindex/timestampindex`; testnet masternode runs for 2 weeks with no PoSe penalty | 20–32 |
| M8 | Wallet parity | `dash-node-wallet`: hosted key-wallet, encryption, SQLite persistence, Core wallet import (SQLite + BDB), wallet RPCs, ProTx builders, PSBT/multisig, CoinJoin client + mixing RPCs, fee estimation wiring, `dash-cli` wallet flows; functional `wallet_*`, `rpc_coinjoin`, `wallet_dash_rpcs`, `tool_wallet` (subset) pass; R5 US-W/US-CJ/US-MO stories covered | 24–36 |
| M9 | Hardening and mainnet | Full nightly L2–L5 green for 4 weeks; mainnet full node soak (CL/IS enforcement live); mainnet masternode (or evonode) runs ≥ 1 payment cycle with DKG participation; benchmark report vs 2x with explanations; import of a Core datadir end-to-end; docs and release packaging; pruning (P1), Tor/I2P (P1) | 32–40 |

Charter "done when" mapping: (1) M3 + M4; (2) M6 + M7 + M9; (3) M5 + M8; (4) M7 + M8 (+ desktop track); (5) M1–M9 gates; (6) M9 report.

### 5.2 Critical path

```
BS1/BS3/BS4/BS15 ─┐
BS6/BS7/BS17 ─────┼─► dash-consensus (script, tx/block, pow, subsidy) ─► dash-chainstate (+ dash-store) ─► dash-evo (DML, commitments, rotation)
BS2 (harness) ────┘                                                                                      │
                                                                                                          ▼
                                                   mainnet IBD agreeing with dashd (M3) ─► dash-sync on dash-p2p (M4, needs BS9a/d)
                                                                                                          │
                                                                              dash-mempool ─► dash-miner ─► dash-rpc core (M5)
                                                                                                          │
                                                                   dash-llmq node role ─► MN role (M6, needs BS9b/BS14/BS16)
                                                                                                          │
                                                       active MN + MNAUTH + governance + CoinJoin server (M7) ─► mainnet masternode (M9)
```

The DML is on the critical path earlier than one might expect: above mainnet 1047200 `IsBlockPayeeValid` needs the payee from the DML even under assumevalid, so M2's "IBD past DIP3 enforcement" exit requires at least `dash-evo::dml` + payee selection. M2 and M3 therefore overlap heavily; the evo implementer starts in week 3 at the latest.

Off the critical path: wallet (M8 can start at M2 against the chainstate events), governance and CoinJoin (start at M4 against `NetHandler`), indexes beyond txindex, ZMQ/REST/stats, convergence (BS5, BS8, BS10–BS13), pruning, Tor/I2P, quorum proofs, dash-tx/dash-wallet CLIs.

### 5.3 Parallel workstreams and staffing

| Stream | Owner profile | Starts | Crates | Notes |
|---|---|---|---|---|
| S1 Base SDK | Opus high (base-sdk style-strict) | wk 0 | base-sdk `node/base-sdk`, BS tasks | 2 implementers until M0, then 1 for BS9/BS14/BS16 and convergence |
| S2 Consensus | Opus xhigh | wk 0 | `dash-consensus` | script interpreter + sighash first; vectors from day one |
| S3 Chainstate/store | Opus high | wk 0 | `dash-store`, `dash-chainstate`, `dash-coreimport` (DB parts) | engine + formats first, connect pipeline second |
| S4 Evo | Opus xhigh | wk 2 | `dash-evo` | DML/payee first (M2 dependency), then commitments/rotation/credit pool/EHF |
| S5 Networking | Opus high | wk 3 (after BS9a/d design) | `dash-p2p`, `dash-sync`, `dash-sporks` | transport + connman first; net_processing second; fingerprint tests alongside |
| S6 Mempool/miner/RPC/CLI | Opus high (Sonnet for handler boilerplate) | wk 6 | `dash-mempool`, `dash-miner`, `dash-rpc`, `dash-cli`, `rpc-json` v24 | RPC handler tables are mechanical once types exist |
| S7 LLMQ | Opus xhigh | wk 10 | `dash-llmq` | node role (verification/enforcement) first |
| S8 Masternode roles | Opus high | wk 14 | `dash-governance`, `dash-coinjoin`, active MN, `dash-indexes` extras, `dash-zmq` | |
| S9 Wallet | Opus high | wk 6 | `dash-node-wallet`, `key-wallet*` extensions, BDB/SQLite import | coordinates with the desktop/wallet track |
| S10 Verification & bench (standing) | Opus high | wk 0 | `dash-conformance`, `dash-testkit`, Core branches `node/oracle`, `node/functional-shim`, `node-bench` | owns the nightly jobs and STATUS metrics |

Ten streams map to 6–10 implementers by merging S6/S8 and S3/S4 early on. Reviews: consensus/evo/llmq/p2p PRs get GPT xhigh + Opus + the `code-review` skill; others GPT high.

### 5.4 Task graph (first wave; `node/<slug>` branches)

| ID | Task (size) | Depends on | Stream |
|---|---|---|---|
| T01 | `dash-testkit` extraction + `RustNode` driver (M) | — | S10 |
| T02 | Core branch `node/oracle`: build system + script/sighash/tx/block entry points (M) | — | S10 |
| T03 | `dash-consensus::script` interpreter + sighash + vector runners (XL) | BS4 (opcodes) | S2 |
| T04 | `dash-consensus::{tx,block}` checks + flags + limits (L) | BS17 | S2 |
| T05 | `dash-consensus::pow` (DGW/KGW/BTC/min-diff/tolerance) + tables + oracle test (L) | T02 | S2 |
| T06 | `dash-consensus::{subsidy,payments,deployments}` + oracle tests (L) | T02 | S2 |
| T07 | `dash-store`: `KvStore` trait, RocksDB + redb engines, CFs, Core codecs, flatfile (L) | — | S3 |
| T08 | `dash-coreimport::leveldb` + blockfiles + blockindex/chainstate translators (L) | T07 | S3 |
| T09 | `dash-chainstate`: block index + headers acceptance + block files + reindex (L) | T07, T04 | S3 |
| T10 | `dash-chainstate`: coins views/cache + connect/disconnect + undo + flush + reorg (XL) | T09, T03–T06 | S3 |
| T11 | `dash-chainstate`: assumevalid/minchainwork/checkpoints + validation bus + snapshots (M) | T10 | S3 |
| T12 | `dash-evo::dml` state machine + payee + SML roots + unique props + diff formats (XL) | BS6, BS7 | S4 |
| T13 | `dash-evo::{cbtx,specialtx}` all CheckPro*/shared/asset lock-unlock/credit pool/EHF (XL) | T12 | S4 |
| T14 | `dash-evo::llmq` commitments, block processor, member selection (rotation, snapshots), scan, selection (XL) | T12 | S4 |
| T15 | `dash-evo` ↔ chainstate integration (`process_special_txs_in_block`, oracles, evo writes in the batch) + mainnet IBD agreement (L) | T10, T13, T14 | S3+S4 |
| T16 | BS2 full-chain nightly + `replay` tool + state hashes (MuHash incl. the `hash_serialized_2` quirk, canonical DML hash) + the R4 chain scans (special txs with `nVersion > 3`, the EMBARGO-1 scan, `bestCLHeightDiff ≥ height`) (L) | T08, T11 | S10 |
| T17 | `dash-p2p`: transport V1/V2 (rebase #264 onto p2p-core), ConnMan, eviction, permissions (XL) | BS9a, BS9d | S5 |
| T18 | `dash-p2p`: addrman/banman/netfulfilled/peers.dat/anchors/banlist (L) | T17 | S5 |
| T19 | `dash-sync`: handshake sequence, headers(2) sync, block download, serving, misbehaviour (XL) | T17, T11 | S5 |
| T20 | `dash-sync`: tx relay, object request tracker, compact blocks, bloom, cfilters, mnlistdiff/qrinfo serving, SyncManager (XL) | T19, T21 | S5 |
| T21 | `dash-mempool`: pool + policy + ATMP + fees + orphans + persistence (XL) | T11, T13 | S6 |
| T22 | `dash-sporks` (S) | BS9a | S5 |
| T23 | `dash-miner` + regtest miner (L) | T21, T14 | S6 |
| T24 | `dash-rpc` server + dispatch + help + auth + categories control/network/blockchain/rawtx/util/mining (XL) | T11, T21, T23 | S6 |
| T25 | `dash-cli` + `rpc-json` v24 merge (M) | T24 | S6 |
| T26 | `dash-indexes` txindex + Dash BIP158 filter index (L) | T11 | S6 |
| T27 | `dash-node` args/config/datadir/init/shutdown/logging/scheduler + `dashd-rs` (L) | T11, T19, T24 | S6 |
| T28 | Core branch `node/functional-shim` + runner + first matrix (M) | T27 | S10 |
| T29 | fingerprint harness + Appendix B tests (L) | T19 | S10 |
| T30 | `dash-llmq` node role: quorum manager, CL handler/oracle, IS manager/oracle + DBs (XL) | T14, T20, T21 | S7 |
| T31 | `dash-llmq` MN role: DKG, sig shares, recovered sigs, EHF signer, data recovery, connections (XL) | T30, BS9b, BS14, BS16 | S7 |
| T32 | active MN manager + MNAUTH + MN connections + platformban (L) | T18, T31 | S8 |
| T33 | `dash-governance` (XL) | T20, T26, BS9c | S8 |
| T34 | `dash-coinjoin` common + server + DSTX policy (L) | T21, T30, BS9c | S8 |
| T35 | `dash-indexes` address/spent/timestamp/coinstats + `dash-zmq` + REST + stats (L) | T26 | S8 |
| T36 | `dash-node-wallet` adapter + events + SQLite persistence + encryption (XL) | T11, T21 | S9 |
| T37 | Core wallet import: SQLite + BDB readers + quirks (L) | T36 | S9 |
| T38 | wallet RPCs + ProTx builders + PSBT/multisig (XL) | T36, T24 | S9 |
| T39 | CoinJoin client + mixing RPCs (XL) | T34, T36 | S9 |
| T40 | `node-bench` integration of node metrics + per-PR job (M) | T27 | S10 |
| T41 | mixed-network interop harness + devnet soak (L) | T01, T31 | S10 |

### 5.5 Risks and mitigations specific to this sequencing

| Risk | Mitigation |
|---|---|
| Base SDK gate slips (BS6/BS7 are base-sdk-style work) | S2/S3 start on existing types; BS6 can land as "rdc-side shim table" first and move to base-sdk second |
| Legacy BLS (`ep2_map_legacy`) in `dash-pkc` differs from dashbls on edge cases | BS16 vectors from the oracle **before** M3 relies on pre-V19 commitment verification; assumevalid 2487500 means mainnet IBD never verifies legacy sigs on the critical path, so a bug would surface only in L3 cross-checks on testnet/regtest, which run early |
| DIP24 rotation/snapshot side effects | T14 differential against both the oracle (`oracle_quorum_members`) and dash-spv's `rotated_quorum_construction` on the 11 mainnet captures |
| State-dependent validity (superblocks, IS conflicts, CL) | oracles injected into chainstate with `NoOp` defaults; replay runs twice (with and without governance) and asserts the same tip |
| Floating-point consensus | tables + CI cross-check on each platform (§2.2.1) |
| Functional-test harness fights the Rust node on log lines/timing | skip list with reasons; the matrix is a tracked metric, not a gate until M7 |
| RocksDB build time in CI | sccache/ccache, prebuilt `librocksdb-sys` cache keyed by version; `redb` engine for unit tests |
| Two codecs drift | BS2 is a merge gate; `cargo deny` keeps node crates on one codec |

### 5.6 Second-wave tasks (M8–M9 and convergence)

| ID | Task (size) | Depends on | Stream |
|---|---|---|---|
| T42 | Fee estimation (`CBlockPolicyEstimator` port, `fee_estimates.dat`) + `estimatesmartfee`/`estimaterawfee` (M) | T21 | S6 |
| T43 | `getblockstats`, `getchaintxstats`, `scantxoutset`, `scanblocks`, `gettxoutproof`/`verifytxoutproof`, `getmerkleblocks`, `getspecialtxes`, `getblockheaders` (M) | T24, T26 | S6 |
| T44 | `dumptxoutset` + `MuHash`/`hash_serialized_3` + coinstats index (M) | T26 | S10 |
| T45 | Tor control (`ADD_ONION ED25519-V3`), `-proxy/-onion/-onlynet`, I2P SAM 3.1, CJDNS, asmap, UPnP/NAT-PMP (L) | T18 | S5 |
| T46 | Pruning end-to-end + `feature_pruning`/`feature_index_prune`/`feature_fastprune` (M) | T26, T35 | S3 |
| T47 | `quorum platformsign/sign/verify/getrecsig/hasrecsig/isconflicting/selectquorum/rotationinfo`, `submitchainlock`, `getchainlockbyheight`, `getassetunlockstatuses`, `getcreditpoolinfo` (M) | T30 | S7 |
| T48 | Quorum proofs DASHNC02 (`getquorumproofchain`/`verifyquorumproofchain`) (L) | T30 | S7 |
| T49 | `protx shared_*` RPC builders + `feature_masternode_shares`/`payout_shares` (L) | T38 | S9 |
| T50 | `dash-cli tx` (dash-tx), `dash-cli util grind`, `dash-cli wallet info/create/dump/createfromdump` (M) | T25, T37 | S6/S9 |
| T51 | `-importcoredatadir` end-to-end (blocks copy/symlink, index/chainstate/evodb translate, wallets, flat files, peers/bans) + docs (L) | T08, T37 | S3/S9 |
| T52 | Convergence: BS5, BS8, BS10–BS13 landed; R3 Phase 3.2/3.3/3.4 (Network, LLMQ types, payload wrappers) with Platform bump notes (XL) | BS6, BS7 | S1 |
| T53 | Convergence: R3 Phase 3.5/3.7/3.8/3.9 (hash newtypes, tx/block, script/amount/address, P2P messages) (XL, after M7) | T52 | S1 |
| T54 | dash-spv client mode on `dash-p2p` + `dash-indexes::filter` (optional, after T53) (L) | T53 | S5 |
| T55 | Mainnet masternode runbook, config templates, systemd units, release packaging, reproducible build notes (M) | T27 | S10 |
| T56 | Benchmark report vs 2x per metric with explanations (M) | T40 | S10 |
| T57 | Security review pass (`security-review` skill) on P2P/RPC/wallet surfaces + fuzz corpus freeze (M) | M7 | S10 |

---

## 6. Decisions table

| # | Decision | Choice | Main alternative | Why |
|---|---|---|---|---|
| D1 | Node type stack | base-sdk types, local branch `node/base-sdk`, path deps | build on `dashcore` and migrate later | consensus code reviewed once against Core-shaped, corpus-tested types; no Bitcoin leftovers in the consensus path |
| D2 | base-sdk gaps | close them locally with upstream-shaped commits | wait for upstream | single maintainer; program is local by charter |
| D3 | Two codecs | accept during the program, BS2 equality as a merge gate, converge via R3 Phase 3 | big-bang migration of `dashcore` first | avoids putting 2,100 Platform uses on the critical path |
| D4 | bincode | never in the node; `dashcore` newtypes keep owning persisted bincode | add bincode to base-sdk | orphan rule + base-sdk policy; node formats are Core's or versioned `BaseCodec` |
| D5 | Hash display | Core order (reversed) in node and RPC; wallet/FFI unchanged | flip everything now | RPC/functional-test compatibility; mobile stability |
| D6 | Devnet | `Network::Devnet` in `dash-params`, name in `ChainParams`, derived devnet genesis | name in the enum | matches Core; no stored devnet hashes |
| D7 | v24 payloads | both in `dash-primitives` (node) and `dashcore` (SPV/wallet), harness-equal | one copy | both consumers need them now; equality is mechanical |
| D8 | X11 | unconditional in `dashcore`; `dash-pow` backend after benchmark | keep the feature | R1's top risk; feature unification can flip block identity |
| D9 | Script interpreter home | `dash-consensus` (rdc), no_std-capable | base-sdk `dash-script` | sighash needs `Transaction` (layering), base-sdk charter is stateless-only, speed of iteration |
| D10 | Storage engine | RocksDB, one DB, column families, `KvStore` trait; redb for tests | MDBX/LMDB; LevelDB; custom | atomic multi-table commits; LSM fits IBD writes; proven; engine is swappable if B1 says so |
| D11 | On-disk formats | Core's for block/undo/filter files and DB keys/values where stable; sorted `dmn_S3`; no XOR | own formats everywhere | import by copy/translation; byte-diff against dashd; Core tooling works |
| D12 | evodb compatibility | import-only (read Core's, write ours) | bit-compatible writes | Core's bytes depend on immer HAMT iteration order |
| D13 | Concurrency | sync consensus/chainstate with one validation thread; rayon for script/BLS; tokio at the edges; snapshot reads | async everywhere; cs_main-style global lock | determinism, testability, no lock contention for readers |
| D14 | Connect pipeline | stage-A pre-validation + parallel UTXO prefetch ahead of the validation thread | Core's single-thread flow | the main structural source of the 2x on IBD/connect |
| D15 | net_processing | one `PeerManager` task + 100 ms `SendMessages` tick, Core cadences | per-peer parallel handlers | fingerprint parity and ordering semantics |
| D16 | BLS scheme handling | explicit `BlsCtx`/type parameter; no global flag | global like Core | R2's #1 pitfall; thread safety |
| D17 | Floating-point consensus | f64+libm plus oracle-generated authoritative tables for the finite mainnet domains | pure f64 | bit-for-bit without depending on libm rounding per target |
| D18 | Mempool acceptance home | `dash-mempool` over chainstate traits | inside chainstate | separates policy from consensus; ATMP off the validation thread |
| D19 | RPC stack | hyper 1.x + serde_json(preserve_order) + `rpc-json` as the response contract | axum/jsonrpsee | exact HTTP semantics, batch, status mapping, shared types with the CLI and oracle tests |
| D20 | Wallet integration | node hosts `key-wallet-manager` via an adapter; SQLite file in Core's record format; Core import | fork or replace the wallet | charter: extend, don't replace; Core-compatible records make import/export two-way |
| D21 | CoinJoin client | wallet track (M8), server in M7 | both in M7 | charter scope: "CoinJoin servicing" is the node's; mixing is wallet parity |
| D22 | Verification oracle | `libdashoracle` C library from a Core branch + RPC oracle + imported-DB diff | subprocess RPC only | in-process differential fuzzing at native speed |
| D23 | Functional tests | run Dash's suite against the Rust binary via a shim | port tests to Rust | 264 tests for free; the cheapest end-to-end oracle |
| D24 | UA string | `/Dash Core:24.1.0/` default (hidden override), extra `implementation` field in `getnetworkinfo` | honest UA | charter's fingerprint goal |
| D25 | Binary names | `dashd-rs`, `dash-cli-rs` (feature to rename) | `dashd`/`dash-cli` | avoid clobbering Core installs; see §7 |
| D26 | Datadir | separate default dir, same per-network subdirs and file formats as Core | share Core's datadir | charter: parallel product, import |
| D27 | Crate granularity | 13 node crates + 5 support crates | 3–4 big crates | parallel ownership for 6–10 agents; enforceable layering with `cargo deny` |

## 7. Open questions for pasta (product or irreversible)

1. **Binary and crate names.** `dashd-rs` / `dash-cli-rs` with a cargo feature that builds them as `dashd` / `dash-cli` for drop-in use (and for Dash's functional tests, which take a path anyway). Alternatives: `rdashd`, `dash-node`. Also the user-facing product name for `getnetworkinfo.implementation`.
2. **Default datadir** for the parallel product: `~/.dashcore-rs` (proposed), `~/.dashd-rs`, or Core's `~/.dashcore` with a `-datadir` requirement. Changing this later breaks user setups.
3. **Dash Core baseline.** R4 recommends per-network pinned references: mainnet history and tip against **v23.1.x** (the running mainnet release), testnet and V24 rules against **v24.0.0-rcN then final**, with `develop` as a weekly-rebased watch list (it already adds `evo_shares`, bit 14). Proposed: adopt that, re-pin per milestone, and re-run the replay on every re-pin. Confirm.
4. **Wallet file compatibility direction.** Import-only (Core wallet → ours) is required. Should our `wallet.sqlite` stay openable by Dash Core (write only Core-known records, Core's `application_id`/`user_version`), which constrains our schema, or may we add our own records (which makes Core refuse the file)? Proposed: Core-openable while the wallet holds only Core-expressible state; a one-way "upgrade" flag otherwise.
5. **base-sdk upstreaming.** Keep `node/base-sdk` purely local until you ask (proposed), or open an RFC/discussion with kwvg now (R3 task 0.1) so the branch can land upstream in pieces? This also decides whether we pay base-sdk's lint/CodeQL costs from day one (proposed yes, to keep the option).
6. **Legacy BLS backend.** Port `ep2_map_legacy` semantics through `dash-pkc` (already implemented in `chia_h2c.rs`, provenance to be fixed by BS16) versus an FFI binding to vendored dashbls for pre-V19 verification. Proposed: `dash-pkc` only, with oracle vectors; dashbls FFI only if BS16 finds divergences.
7. **RocksDB (C++) in the dependency set.** Acceptable for the node binaries (proposed yes; it never enters the wallet/SPV/FFI crates). If a pure-Rust-only policy is wanted, `redb` becomes the default at a measured cost on IBD.
8. **Pruning, Tor/I2P, quorum proofs, assumeutxo** are scheduled as P1 after M7. Confirm none of them is required for the first mainnet masternode.
9. **Devnet for soak.** Running a persistent mixed devnet needs public IPs or a reachable network for masternodes; agentbox is network-restricted. Where should the soak run (Studio proxies are not enough)?
10. **Governance from day one.** The node runs governance always (required for superblock validation and for `-txindex` default parity). Confirm there is no appetite for a "governance-less" node mode beyond Core's `-disablegovernance`.
11. **Private disclosure of EMBARGO-1** (EMBARGO-1: see ~/private/embargo/EMBARGO-1.md): the Rust node must replicate Core's behaviour until upstream fixes it. Disclosure under Dash Core's `SECURITY.md` is your call; this document and R4 stay off public channels until then.

## Appendix A: key trait and type signatures

```rust
// dash-consensus
pub struct HeaderCtx { pub height: u32, pub hash: BlockHash, pub prev: Option<BlockHash>, pub time: u32, pub bits: CompactTarget, pub version: i32, pub mtp: i64, pub chain_work: Arith256 }
pub trait HeaderAncestry { fn ancestor(&self, of: &HeaderCtx, height: u32) -> Option<HeaderCtx>; fn prev(&self, of: &HeaderCtx) -> Option<HeaderCtx>; }
pub trait CoinSource { fn coin(&self, op: &OutPoint) -> Option<Coin>; }
pub trait CheckpointView { fn highest_checkpoint_in_index(&self) -> Option<u32>; }   // Core rejects forks only below the highest checkpoint present in the index; no per-height hash compare (R4 B§4)
pub trait EhfSignalView { fn signal_height(&self, prev: &HeaderCtx, bit: u8) -> Option<u32>; }
pub struct BlsCtx { pub legacy: bool }

// dash-evo
pub trait QuorumMemberSource { fn members(&self, t: LlmqType, base: &HeaderCtx) -> Vec<Arc<Dmn>>; }
pub trait ChainLockVerifier { fn verify_chain_lock(&self, height: u32, hash: &BlockHash, sig: &BlsSigBytes, ctx: &BlsCtx) -> bool; }
pub struct EvoCtx<'a> { pub params: &'a ConsensusParams, pub bls: BlsCtx, pub v24_active: bool, pub check_sigs: bool }
pub fn process_special_txs_in_block(block: &Block, prev: &HeaderCtx, coins: &dyn CoinSource, store: &dyn EvoStore, d: &dyn DeploymentView,
                                    cl: &dyn ChainLockVerifier, ctx: &EvoCtx, out: &mut EvoWrite) -> Result<EvoBlockResult, EvoError>;
pub struct EvoBlockResult { pub mn_list: DeterministicMnList, pub payee: Option<Arc<Dmn>>, pub commitments: Vec<(FinalCommitment, BlockHash)>,
                            pub credit_pool: CreditPool, pub signals: Signals, pub bls_legacy_after: bool }

// dash-chainstate
pub trait ChainLockOracle: Send + Sync { fn has_chain_lock(&self, h: u32, hash: &BlockHash) -> bool; fn has_conflicting_chain_lock(&self, h: u32, hash: &BlockHash) -> bool; fn best_height(&self) -> Option<u32>; }
pub trait InstantSendOracle: Send + Sync { fn conflicting_islock(&self, tx: &Transaction) -> Option<InstantSendLockHash>; fn is_waiting_for_tx(&self, txid: &TxHash) -> bool; fn remove_conflicting_lock(&self, h: &InstantSendLockHash); }
pub trait SuperblockOracle: Send + Sync { fn check_type(&self, height: u32) -> SuperBlockCheck; fn is_triggered(&self, height: u32) -> bool; fn is_valid(&self, cb: &Transaction, height: u32, reward: Amount, dup: DupPolicy) -> bool; fn payments_limit(&self, height: u32) -> Amount; }
pub struct NoOpOracles;  // IBD/replay/tests

// dash-mempool
pub struct Policy { pub min_relay_fee: FeeRate, pub dust_relay_fee: FeeRate, pub incremental_relay_fee: FeeRate, pub max_mempool: usize, pub expiry: Duration,
                    pub limit_ancestors: usize, pub limit_ancestor_size: usize, pub limit_descendants: usize, pub limit_descendant_size: usize,
                    pub datacarrier: bool, pub datacarrier_size: usize, pub permit_bare_multisig: bool, pub require_standard: bool, pub bytes_per_sigop: usize }

// dash-p2p
pub trait Transport: Send { fn poll_recv(&mut self, buf: &[u8]) -> TransportEvent; fn encode(&mut self, msg: &P2pMsg, peer_version: u32) -> Bytes; fn kind(&self) -> TransportKind; }
pub struct PeerRef { pub id: NodeId, pub addr: SocketAddr, pub conn_type: ConnectionType, pub services: ServiceFlags, pub version: u32,
                     pub masternode: MasternodeConn /* none | probe | verified{protx} | iqr */, pub permissions: Permissions }
pub struct MessageProcessingResult { pub misbehaving: Option<(u32, &'static str)>, pub relay: Vec<Inv>, pub erase_requests: Vec<Inv>, pub disconnect: bool }

// dash-sync / llmq / governance register through dash-p2p's NetHandler (§2.2.6) and chainstate's ValidationSubscriber (§2.2.4)

// dash-rpc
pub struct RpcDef { pub name: &'static str, pub category: &'static str, pub args: Vec<RpcArg>, pub results: RpcResults, pub examples: &'static str, pub hidden: bool }
pub trait RpcMethod: Send + Sync { fn def(&self) -> &RpcDef; fn call(&self, cx: &RpcCtx, req: RpcRequest) -> Result<serde_json::Value, RpcError>; }

// dash-node-wallet
pub trait WalletInterfaceExt: WalletInterface { fn disconnect_block(&mut self, block: &Block, height: u32); fn rescan_from(&mut self, height: u32, source: &dyn BlockSource); }
```

## Appendix B: fingerprint checklist (each line is a test)

1. `version`: field order and sizes; addrMe = our services + zero IPv6 + port 0; addrYou zeroed when unroutable/proxy/non-v1; `/Dash Core:24.1.0/` + `(devnet.<name>; <uacomment>)`; version-209 framing; start_height = best height; 32-byte challenge; masternode flag.
2. Services `0x1C05` node / `0x1C45` masternode; `NODE_NETWORK_LIMITED` with `NODE_NETWORK`; bloomfilter permission grants `NODE_BLOOM`.
3. Outbound order: `version` → (on peer version) `sendaddrv2`(≥70223) → `verack` → `getaddr`; feelers disconnect here.
4. Post-verack: `[mnauth]`, `sendheaders2` xor `sendheaders`, `sendcmpct(0,1)` iff `CanRelay()`, `senddsq(true)` unless rejecting txs, `[qwatch]`; then `ping`, `getheaders2`/`getheaders`, invs; `getsporks` follows 0.5–2 s later and `govsync` about 8 s later (live capture of rc.3, R4 F11). The `version` message is 137 bytes.
5. No `feefilter`, `wtxidrelay`, `reject`, `alert`; no `sendtxrcncl` by default.
6. `getsporks` to every relay peer within ~6 s; `mempool` to outbound after sync; `govsync(0, empty)` then per-object vote syncs; disconnect after `full-sync`.
7. BIP324: salt with magic; short IDs only at ≥70235 (≥70240 platformban); long before `version`; receive 0–32/128–168; 5 and 29–32 ignored; garbage uniform 0..4095; v2 only if bit 12; v1 fallback on early disconnect.
8. Penalty table (R2 A.2 §4.5): unsolicited `mnlistdiff`/`qrinfo` 100, unrequested `clsig` 10, oversized inv/getdata/addr/headers/notfound 20, bad filter 100, invalid spork 100, mnauth 100/10, govsync repeat 20, devnet mismatch 100/1, header invalid 10, non-continuous 20.
9. Bad checksum/command drops the message, keeps the connection; unknown commands silently ignored.
10. `mempool`/`filter*` without `NODE_BLOOM` → disconnect; BRO-violating list on block-relay-only/feeler/blocksonly (tx yes; clsig/spork/govobj/isdlock no).
11. Trickles: 5 s inbound shared, 2 s outbound, 1 s verified outbound MN, none MN-to-MN/noban; non-tx invs every `SendMessages` (100 ms) incl. to block-relay-only; `mempool` reply includes ISDLOCK invs + best CLSIG inv; `merkleblock` followed by `tx`s then `isdlock`s.
12. headers2 bit-exact (inverted "set = present", 7-entry MRU versions, int16 time delta); limits 8000/2000; empty headers below minimum chain work; `MAX_UNCONNECTING_HEADERS` 10.
13. Masternode: inbound refused until synced; 1 s MNAUTH protection; ≤3 verified legs per ProTx exempt from limits; MN connections bypass the outbound semaphore; probes disconnect after MNAUTH; MNAUTH tuple exact.
14. Disconnect: ~100 ms linger + `shutdown(SHUT_WR)`; addr cache 21–27 h, ≤1000 and ≤23 %; ADDR_FETCH disconnects after an `addr` with >1 entry.
15. Timeouts: `-peertimeout` 60 s, 20 min inactivity/ping, headers 15 min + 1 ms/header, block 150 s × (1 + 0.5·N), stalling 2→64 s, `CHAIN_SYNC_TIMEOUT` 20 min, `MINIMUM_CONNECT_TIME` 30 s.
16. `getdata` object intervals 15 s/5 s/10 s/60 s; 100 in flight; 100,000 announcements; no non-preferred delay on a masternode.
17. DSQ relay mode by version (INV ≥70234, else ALL); DSTX announced as `MSG_TX` to peers <70241 when unbalanced; `MSG_ASSET_UNLOCK` (34) by instance hash to ≥70242.
18. `getaddr` ignored on outbound; `sendaddrv2` after verack means disconnect; `inv` ISDLOCK on block-relay-only from >70223 means disconnect.

## Appendix C: glossary of Core terms used

DML deterministic masternode list (`CDeterministicMNList`); SML simplified MN list (`CSimplifiedMNListEntry`); CbTx coinbase special tx (type 5); QcTx quorum commitment tx (type 6); MNHF/EHF masternode hard-fork signal (type 7, DIP-23); CL ChainLock; IS/ISDLOCK deterministic InstantSend lock (DIP-22); DKG distributed key generation (LLMQ); PoSe proof of service; BRR block reward reallocation; MN_RR masternode reward reallocation (platform share); V19 basic BLS scheme switch; V20 credit pool/CbTx v3/EHF; V24 shared masternodes, ExtNetInfo, CbTx v4, asset lock/unlock v2; IBD initial block download; ATMP AcceptToMemoryPool; GBT getblocktemplate; BRO-violating "violates blocks-only" message list.

## Appendix D: Dash Core source → Rust crate/module map

Implementers port from the left column into the right one; the oracle entry points (§4.3) exist for every row marked ★.

| Core source (`src/`) | Rust home | Notes |
|---|---|---|
| `script/interpreter.{h,cpp}`, `script/script.{h,cpp}`, `script/script_error.h` ★ | `dash-consensus::script::{interpreter, num, error}` | DIP0020 opcodes unconditional; no SigVersion other than BASE |
| `script/sigcache.cpp`, `cuckoocache.h` | `dash-chainstate::cache::{sig_cache, script_cache}` | sharded, lock-free reads |
| `script/standard.{h,cpp}`, `script/descriptor.cpp` (subset) | `dash-mempool::policy::standard`, `dash-node-wallet::descriptors` | `pk/pkh/sh/multi/sortedmulti/combo/addr/raw` only |
| `consensus/tx_check.cpp`, `tx_verify.cpp`, `consensus.h`, `merkle.cpp` ★ | `dash-consensus::{tx, block::merkle}` | `check_transaction` delegates to `dash-primitives::Checkable` after BS17 |
| `pow.{h,cpp}`, `arith_uint256.*`, `hash_x11.h`, `crypto/x11/*` ★ | `dash-consensus::pow`, `dash-num`, `dash-pow` | tolerance-only for the BTC/KGW eras; DGW exact from 68590 |
| `validation.cpp` `GetBlockSubsidy*`, `ContextualCheck*`, `GetBlockScriptFlags`, `CheckBlock` ★ | `dash-consensus::{subsidy, tx::contextual, block}` | |
| `validation.cpp` `ConnectBlock`/`DisconnectBlock`/`ActivateBestChain*`/`AcceptBlock*`/`FlushStateToDisk`/`LoadExternalBlockFile`/`VerifyDB`/`EnforceBlock`/`MarkConflictingBlock`, `chain.{h,cpp}`, `node/chainstate.*`, `kernel/*` | `dash-chainstate::{chain, index, connect, flush, import, verify}` | single validation thread |
| `validation.cpp` `MemPoolAccept`, `txmempool.*`, `policy/*`, `txorphanage.*`, `kernel/mempool_*` | `dash-mempool::{accept, pool, policy, fees, orphanage, persist}` | |
| `coins.*`, `txdb.*`, `undo.h`, `compressor.*`, `dbwrapper.*` ★ | `dash-chainstate::coins`, `dash-store::{codec, rocks}` | Core key/value codecs in `dash-store::codec` |
| `node/blockstorage.*`, `flatfile.*` | `dash-store::flatfile`, `dash-chainstate::blockfiles` | |
| `versionbits.*`, `deploymentstatus.*`, `deploymentinfo.*` ★ | `dash-consensus::deployments` | EHF signal height via `EhfSignalView` |
| `chainparams*.cpp`, `chainparamsseeds.h`, `llmq/params.h` | `dash-params` (BS6) + `dash-node::args::chain_overrides` | |
| `evo/providertx.*`, `evo/providertx_util.cpp`, `evo/specialtx.*`, `evo/sharedcollateral.h`, `evo/types.h`, `evo/dmn_types.h` ★ | `dash-primitives::payload` (BS7) + `dash-evo::specialtx` | |
| `evo/specialtxman.*` ★ | `dash-evo::specialtx::block` (`process_special_txs_in_block`, `build_new_list_from_block`) | |
| `evo/deterministicmns.*`, `evo/dmnstate.*` ★ | `dash-evo::dml::{list, state, diff, payee, pose, manager}` | `imbl` HAMT |
| `evo/simplifiedmns.*`, `evo/smldiff.*` ★ | `dash-evo::sml` + `dash-sync::serve::mnlistdiff` | |
| `evo/cbtx.*` ★ | `dash-evo::cbtx` | |
| `evo/creditpool.*`, `evo/assetlocktx.*` ★ | `dash-evo::{creditpool, assetlock}` | |
| `evo/mnhftx.*` ★ | `dash-evo::mnhf` | |
| `evo/netinfo.*` ★ | `dash-primitives::types::netinfo` (exists) + `dash-evo::netinfo::validate` | domain rules, port rules |
| `evo/evodb.*` | `dash-store::evo_cf` + `dash-evo::store::EvoStore` | one KV batch with coins |
| `evo/mnauth.*` | `dash-p2p::mnauth` | |
| `evo/specialtx_filter.cpp`, `blockfilter.*`, `index/blockfilterindex.*` ★ | `dash-indexes::filter`, `dash-indexes::blockfilter` | X11 keys, Dash elements |
| `evo/providertx_service.cpp` | `dash-node-wallet::protx` | wallet builders |
| `evo/snapshot.*` | not ported (assumeutxo) | reserved |
| `llmq/commitment.*`, `llmq/blockprocessor.*` ★ | `dash-evo::llmq::{commitment, blockprocessor}` | consensus |
| `llmq/utils.cpp` (members, modifiers, connections), `llmq/snapshot.*` ★ | `dash-evo::llmq::members`, `dash-llmq::connections` | split: selection is consensus, connections are runtime |
| `llmq/quorumsman.*` (`ScanQuorums`, `SelectQuorumForSigning`, `VerifyRecoveredSig`) ★ | `dash-evo::llmq::{scan, selection}` | consensus-relevant |
| `llmq/quorumsman.*` (rest), `llmq/quorums.*`, `llmq/net_quorum.*` | `dash-llmq::quorum_manager`, `dash-llmq::net_quorum` | quorumdb, data recovery |
| `llmq/dkgmessages.h`, `dkgsession*`, `dkgsessionhandler*`, `dkgsessionmgr*`, `net_dkg.*`, `active/dkgsession*`, `llmq/debug.*` | `dash-p2p-core::msg::dkg` (BS9b) + `dash-llmq::dkg::{session, handler, manager, net, debug}` | |
| `llmq/signing.*`, `signing_shares.*`, `net_signing.*`, `signhash.*` ★(signhash) | `dash-llmq::signing::{recovered, shares, net}`, `dash-consensus::llmq::sign_hash` | |
| `llmq/ehf_signals.*` | `dash-llmq::ehf_signer` | |
| `llmq/quorumproofs*` | `dash-llmq::quorum_proofs` (T48) | |
| `chainlock/*` ★(clsig request id) | `dash-llmq::chainlock::{handler, signer, clsig}` | implements `ChainLockOracle` |
| `instantsend/*` ★(request ids) | `dash-llmq::instantsend::{manager, net, signer, db, lock}` | implements `InstantSendOracle` |
| `bls/bls.*`, `bls/bls_worker.*`, `bls/bls_batchverifier.h`, `bls/bls_ies.*`, `dashbls/` ★ | `dash-pkc` + `dash-llmq::bls_worker` | explicit scheme context |
| `masternode/payments.*` ★ | `dash-consensus::payments` + `dash-evo::payments` (payee outputs, validation) | |
| `masternode/meta.*`, `masternode/sync.*`, `masternode/utils.*`, `node/sync_manager.*`, `netfulfilledman.*` | `dash-coinjoin::meta`, `dash-sync::mnsync`, `dash-p2p::{mn_maintenance, netfulfilled}` | |
| `active/masternode.*`, `active/context.*` | `dash-node::active_masternode` | |
| `spork.*` ★(sig forms) | `dash-sporks` | |
| `governance/*` ★(hashes) | `dash-governance::{object, vote, store, manager, net, superblock, signer, rate}` | |
| `coinjoin/{common,coinjoin,server,options,util}.*` ★(hashes, in/outs) | `dash-coinjoin::{common, server, options}` | |
| `coinjoin/{client,walletman}.*`, `wallet/coinjoin.*` | `dash-coinjoin::client` + `key-wallet::coinjoin` (M8) | |
| `net.{h,cpp}`, `netbase.*`, `netaddress.*`, `net_permissions.*`, `node/eviction.*`, `node/connection_types.*` | `dash-p2p::{connman, transport, netaddress, permissions, eviction}` | |
| `bip324.*`, `crypto/{chacha20,poly1305,ellswift,hkdf}` | `dash-p2p::transport::v2` via `bip324` crate (PR #264) | Dash salt |
| `addrman*`, `addrdb.*`, `banman.*`, `netgroup.*`, `net_types.*` | `dash-p2p::{addrman, banman, netgroup}` | peers.dat v4 |
| `protocol.*`, `version.h` | `dash-p2p-core` (BS4/BS9) | |
| `net_processing.*`, `txrequest.*`, `blockencodings.*`, `merkleblock.*`, `common/bloom.*` | `dash-sync::{peerman, request, cmpct, bloom, serve}` | |
| `i2p.*`, `torcontrol.*`, `mapport.*` | `dash-p2p::{i2p, tor, mapport}` (T45) | |
| `index/{base,txindex,addressindex*,spentindex*,timestampindex*,coinstatsindex}.*` | `dash-indexes::*` | |
| `node/miner.*`, `rpc/mining.cpp` | `dash-miner`, `dash-rpc::mining` | |
| `rpc/server.*`, `rpc/request.*`, `rpc/util.*`, `httprpc.*`, `httpserver.*` | `dash-rpc::{server, dispatch, help, auth, http}` | |
| `rpc/{blockchain,mempool,net,node,rawtransaction,output_script,fees,signmessage,txoutproof,masternode,coinjoin,governance,evo,quorums}.cpp`, `core_write.cpp`, `evo/core_write.cpp`, `llmq/core_write.cpp`, `governance/common.cpp` `ToJson` | `dash-rpc::<category>` + `rpc-json` | JSON shapes |
| `wallet/rpc/*.cpp` | `dash-node-wallet::rpc` | |
| `rest.cpp` | `dash-rpc::rest` | |
| `zmq/*` | `dash-zmq` | |
| `init.cpp`, `init/*`, `util/system.*`, `common/*` (args, settings), `logging*`, `scheduler.*`, `shutdown.*`, `timedata.*`, `warnings.*` | `dash-node::{init, args, settings, logging, scheduler, shutdown, timedata}` | |
| `stats/*` | `dash-node::stats` | |
| `wallet/{wallet,scriptpubkeyman,walletdb,bip39,hdchain,crypter,db,bdb,sqlite,dump,coinselection,spend,receive,fees}.*` | `key-wallet*` (existing) + `dash-node-wallet::{db, crypter, import}` + `dash-coreimport::wallet` | import quirks |
| `bitcoin-cli.cpp`, `bitcoin-tx.cpp`, `bitcoin-util.cpp`, `bitcoin-wallet.cpp`, `wallet/wallettool.*` | `dash-cli` | |
| `test/functional/*`, `test/functional/test_framework/*` | run as-is via the shim; `dash-testkit` mirrors `DashTestFramework` helpers in Rust | |
| `src/test/data/*`, `test/functional/data/*` | `dash-conformance/vectors/` | |
| `bench/*` | `node-bench` + per-crate criterion benches | |

## Appendix E: on-disk layout, consolidated

| Store | Key | Value | Format owner |
|---|---|---|---|
| `Cf::BlockIndex` | `'b' ‖ hash32` | `DiskBlockIndex` (VARINT version, height, status, ntx, [file], [datapos], [undopos], **hash32**, header fields) | Core (`chain.h:391-413`) |
| | `'f' ‖ i32 file` | `BlockFileInfo` (7 VARINTs) | Core |
| | `'l'`, `'R'`, `'F' ‖ string` | last file; reindexing; flags (`prunedblockfiles`) | Core |
| `Cf::Coins` | `'C' ‖ txid32 ‖ VARINT(n)` | `VARINT(height*2+cb) ‖ AmountCompression ‖ ScriptCompression` | Core (`coins.h`, `compressor.*`), no XOR |
| | `'B'`, `'H'` | best block; head blocks during flush | Core |
| `Cf::Evo` | `"b_b4"`, `("dmn_S3",h)`, `("dmn_D4",h)`, `"dmn_R1"`, `("cpm_S",h)`, `("mnhf_s2",h)`, `("q_mc",(t,q))`, `("q_mcih",t,BE32(~h))`, `("q_mcihi",t,i32 idx,BE32(~h))`, `"q_bbu2"`, `("llmq_S",H((t,base)))` | Core values; `dmn_S3` entries sorted by proTxHash | Core keys (`evodb.h`, `deterministicmns.cpp`, `blockprocessor.cpp`, `snapshot.cpp`) |
| `Cf::Llmq` | `("q_Qqvvec",key)`, `("q_Qsk",key)`; `("is_i",h)`, `("is_tx",txid)`, `("is_in",op)`, `("is_m",BE32(~h),h)`, `("is_a1",..)`, `("is_a2",h)`, `"is_v"`; `("rs_r",t,id[,msg])`, `("rs_h",h)`, `("rs_s",sh)`, `("rs_t",BE32 t,t,id)`, `("rs_v",t,id)`, `("rs_vt",..)`, `("rs_p",h)`, `("rs_m",t,id,msg)`, `("rs_u",BE32 t,h)`; `("qdkg_V"|"qdkg_S"|"qdkg_E", t, base, protx)` | Core values (vvec always basic) | Core |
| `Cf::TxIndex` | `'t' ‖ txid` | `DiskTxPos` (VARINT file, pos, txoffset) | Core |
| `Cf::AddrIndex` | `'a' ‖ CAddressIndexKey(66)`; `'u' ‖ CAddressUnspentKey(57)` | i64 amount; `{amount, script, height}` | Core |
| `Cf::SpentIndex` | `'p' ‖ {txid, u32 n}` | `{txid, u32 vin, i32 height, i64 amount, u8 type, hash160}` | Core |
| `Cf::TsIndex` | `'s' ‖ {BE32 time, hash}` | `true` | Core |
| `Cf::FilterIndex` | `'t' ‖ BE32 h`; `'s' ‖ hash`; `'P'`; `'V'` (=2) | `{filter_hash, header, FlatFilePos}` | Core |
| `Cf::CoinStats` | `'t' ‖ BE32 h`; `'s' ‖ hash`; `'M'` | `DBVal`; MuHash3072 | Core |
| `Cf::Meta` | `"schema"`, `"import"`, each index's `'B'` locator | ours (versioned `BaseCodec`) | ours |
| `blocks/blk%05u.dat` | — | `magic ‖ u32 size ‖ block` | Core |
| `blocks/rev%05u.dat` | — | `magic ‖ u32 size ‖ undo ‖ SHA256d(prev_hash ‖ undo)` | Core |
| `indexes/blockfilter/basic/fltr%05u.dat` | — | `hash32 ‖ filter` | Core |
| `mempool.dat` | — | v1 (`u64 1`, count, {tx, time, delta}, deltas, unbroadcast), no XOR | Core |
| `fee_estimates.dat` | — | `140100 ‖ CLIENT_VERSION ‖ ...` | Core |
| `peers.dat`, `anchors.dat` | — | `magic ‖ data ‖ SHA256d`, addrman format 4 / compat 36 | Core |
| `banlist.json`, `settings.json` | — | JSON | Core |
| `governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat` | — | `CFlatDB` envelopes with Core version strings | Core |
| `wallets/<name>/wallet.sqlite` | `main(key BLOB, value BLOB)` | Core record formats (§2.2.17) | Core (+ ours behind an upgrade flag) |

## Appendix F: functional-test adoption matrix by milestone

| Milestone | Tests expected to pass (Dash-specific in bold) |
|---|---|
| M4 | `feature_block`, `feature_assumevalid`, `feature_minchainwork`, `feature_maxtipage`, `feature_dersig`, `feature_cltv`, `feature_csv_activation`, `feature_bip68_sequence`, `feature_nulldummy`, `feature_reindex`, `feature_loadblock`, `feature_blocksdir`, `feature_filelock`, `feature_abortnode`, `feature_dbcrash`, `feature_config_args`, `feature_includeconf`, `feature_settings`, `feature_init`, `feature_shutdown`, `feature_logging`, `feature_uacomment`, `feature_versionbits_warning`, `p2p_block_sync`, `p2p_initial_headers_sync`, **`p2p_sendheaders_compressed`**, `p2p_sendheaders`, `p2p_dos_header_tree`, `p2p_invalid_block`, `p2p_invalid_locator`, `p2p_invalid_messages`, `p2p_unrequested_blocks`, `p2p_mutated_blocks`, `p2p_ping`, `p2p_timeouts`, `p2p_leak`, `p2p_disconnect_ban`, `p2p_eviction`, `p2p_add_connections`, `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_addrfetch`, `p2p_dns_seeds`, `p2p_getaddr_caching`, `p2p_permissions`, `p2p_v2_transport`, `p2p_v2_encrypted`, `p2p_v2_misbehaving`, `p2p_ibd_stalling`, `p2p_node_network_limited`, `p2p_message_capture`, `p2p_fingerprint`, **`p2p_connect_to_devnet`**, `feature_anchors`, `feature_addrman`, `feature_asmap`, `rpc_net`, `rpc_setban`, `rpc_getpeerinfo_deprecation`, `rpc_bind`, **`feature_sporks`** |
| M5 | `mempool_*` (15), `mining_basic`, `mining_getblocktemplate_longpoll`, `mining_prioritisetransaction`, `rpc_blockchain`, `rpc_rawtransaction`, `rpc_decodescript`, `rpc_createmultisig`, `rpc_deriveaddresses`, `rpc_getdescriptorinfo`, `rpc_signmessagewithprivkey`, `rpc_signrawtransactionwithkey`, `rpc_txoutproof`, `rpc_getblockstats`, `rpc_getchaintips`, `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_misc`, `rpc_uptime`, `rpc_help`, `rpc_named_arguments`, `rpc_deprecated`, `rpc_users`, `rpc_whitelist`, `rpc_generate`, `rpc_packages`, `rpc_mempool_info`, `rpc_estimatefee`, `rpc_scanblocks`, `rpc_scantxoutset`, `rpc_getblockfilter`, `rpc_getblockfrompeer`, `rpc_getmerkleblocks`, `rpc_invalid_address_message`, `interface_rpc`, `interface_http`, `interface_bitcoin_cli`, `feature_txindex`, `feature_fee_estimation`, `p2p_filter`, `p2p_blockfilters`, `p2p_compactblocks*`, `p2p_blocksonly`, `p2p_tx_download`, `p2p_tx_privacy`, `p2p_leak_tx`, `p2p_getdata`, `p2p_nobloomfilter_messages`, `p2p_ibd_txrelay`, `p2p_segwit`-free equivalents, `feature_utxo_set_hash`, `feature_coinstatsindex`, `rpc_dumptxoutset`, `feature_notifications`, `feature_startupnotify` |
| M6 | **`feature_llmq_signing`**, **`feature_llmq_chainlocks`**, **`feature_llmq_chainlocks_automatic`**, **`feature_llmq_rotation`**, **`feature_llmq_connections`**, **`feature_llmq_data_recovery`**, **`feature_llmq_dkg_intake`**, **`feature_llmq_dkgerrors`**, **`feature_llmq_is_retroactive`**, **`feature_llmq_is_cl_conflicts`**, **`feature_llmq_evo`**, **`feature_llmq_singlenode`**, **`feature_llmq_simplepose`**, **`feature_new_quorum_type_activation`**, **`rpc_quorum`**, **`rpc_verifychainlock`**, **`rpc_verifyislock`**, **`p2p_instantsend`**, **`p2p_quorum_data`**, **`feature_quorum_proof_chain`** (T48) |
| M7 | **`feature_dip3_deterministicmns`**, **`feature_dip4_coinbasemerkleroots`**, **`feature_governance`**, **`feature_governance_cl`**, **`feature_governance_objects`**, **`feature_governance_txindex_devnet`**, **`feature_mnehf`**, **`feature_asset_locks`**, **`feature_masternode_params`**, **`feature_masternode_params_devnet`**, **`feature_masternode_shares`**, **`feature_masternode_payout_shares`**, **`feature_protx_version`**, **`p2p_dstx`**, **`p2p_platform_ban`**, **`p2p_govsync_bloom`**, **`rpc_masternode`**, **`rpc_mnauth`**, **`rpc_netinfo`**, **`rpc_external_queue`**, **`interface_zmq_dash`**, `interface_zmq`, `interface_rest`, **`feature_addressindex`**, **`feature_spentindex`**, **`feature_timestampindex`**, **`feature_stats`**, `feature_maxuploadtarget`, `feature_proxy`, `p2p_i2p_*` (T45) |
| M8 | `wallet_*` (58, minus BDB-write-only cases in the skip list), **`rpc_coinjoin`**, **`wallet_dash_rpcs`**, **`wallet_dust_protection`**, **`wallet_keypool_hd`**, **`wallet_mnemonicbits`**, **`wallet_upgradetohd`**, **`wallet_orphanedreward`**, **`rpc_wipewallettxes`**, `rpc_psbt`, `tool_wallet` (subset), `wallet_backwards_compatibility` (read side) |
| M9 | `feature_pruning`, `feature_index_prune`, `feature_fastprune`, `feature_remove_pruned_files_on_startup`, `feature_unsupported_utxo_db`, `feature_dirsymlinks`, `feature_posix_fs_permissions`, `feature_bind_*`, `feature_discover`, `interface_usdt_*` (skip: USDT), `feature_help` |

Known permanent skips (documented reasons): `interface_usdt_*` (eBPF tracepoints), tests that write BDB wallets through the node (`-legacy` create paths), Qt-only behaviours, `wallet_signer`/`rpc_signer` until an external-signer bridge exists.

## Appendix G: implementation conventions for the node crates

- **Errors**: every consensus/policy rejection is an enum whose `Display` is Core's exact reject string (`bad-txns-inputs-missingorspent`, `bad-cbtx-mnmerkleroot`, ...), with the Core `BlockValidationResult`/`TxValidationResult` class attached (`BLOCK_CONSENSUS`, `BLOCK_RESULT_UNSET`, `BLOCK_MUTATED`, `BLOCK_CHAINLOCK`, `TX_CONFLICT_LOCK`, `TX_BAD_SPECIAL`, ...). RPC and P2P penalties derive from the class, as Core.
- **Logging**: `tracing` targets equal Core's `-debug` categories (`net`, `mempool`, `validation`, `chainlocks`, `instantsend`, `llmq`, `llmq-dkg`, `llmq-sigs`, `mnpayments`, `mnsync`, `coinjoin`, `spork`, `gobject`, `netconn`, `creditpool`, `ehf`, `leveldb`→`db`, ...); `debug.log` line format matches Core's so `assert_debug_log` patterns in functional tests work for the messages we mirror.
- **Features**: `zmq`, `wallet`, `x11-c`, `redb-engine`, `oracle` (conformance), `miner` (regtest generate), `debug-rpcs` (hidden/test RPCs on by regtest default).
- **Unsafe**: `#![forbid(unsafe_code)]` everywhere except `dash-store` (RocksDB FFI) and `dash-conformance::oracle`.
- **Determinism**: no `HashMap` iteration order leaks into consensus outputs (BTreeMap or explicit sorts where Core sorts; `std::map`/`uint256 <` comparators reproduced as raw-byte memcmp).
- **Time**: all wall-clock reads go through `dash-node::timedata::Clock` (mockable; `-mocktime`, `setmocktime`, adjusted time with `-maxtimeadjustment`).
- **Tests**: unit tests next to code; vectors and differential tests in `dash-conformance`; each crate has a `criterion` bench for its hot path, wired into `node-bench`.
- **Docs**: each crate's `ARCHITECTURE.md` lists the Core files it mirrors (Appendix D rows) and the deviations; stale docs are a review finding (R1 §10).

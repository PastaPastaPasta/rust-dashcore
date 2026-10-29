> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

# A1: Foundational architecture of the Rust Dash node (Opus design)

- **Author:** Opus 5.5 (independent architect A1b). The Fable design (A1a) was written separately; the manager reconciles the two.
- **Date:** 2026-10-08
- **Inputs read:**
  - `CHARTER.md`, `PROCESS.md`, `STATUS.md`, `briefs/A1-architecture.md`
  - research R1 (rust-dashcore state), R2 + R2b (Dash Core inventory and RPC table), R3 (base-sdk)
  - R4 (consensus and verification surface, plus its appendices A–G). It landed while this design was being finalized and is integrated: §1.5 #8, BS15, §2.5, §2.10, §2.13, §4, D41–D43.
  - Code: rust-dashcore `dev` @ 67b307c6, base-sdk `develop` @ 33fcc59, Dash Core `develop` @ 15db478750 (24.1.0-pre, protocol 70242)
  - In-flight work: BS1 (base-sdk re-pin), BS2 (oracles plus conformance harness), B1 (bench harness)
- **Status:** design proposal. Read-only work; nothing committed.

Notation:
- `Core:file:line` points into `~/workspace/dash/src`.
- `RD:` is rust-dashcore and `BS:` is `base-sdk/pkgs`.
- "Core" means Dash Core.
- `M0`…`M8` are milestones (§5). `BS*` are Base SDK milestone tasks (§1.7).

---

## 0. Executive summary

The node is a new set of crates in the rust-dashcore workspace. They split into two layers:
1. **Pure "sans-io" consensus libraries:** `dash-consensus`, `dash-evo`, `dash-llmq`, `dash-governance`, `dash-coinjoin`.
2. **Node runtime crates** (`dashd-*`), assembled into one daemon binary, **`rdashd`**, and one CLI, **`rdash-cli`**.

The most consequential decisions:

1. **Type stack.**
   - The node's wire and data types are **base-sdk** types (`dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, `dash-num`).
   - `dashcore` stays the **SPV / wallet / FFI / Platform API surface**, unchanged except for additive bridges.
   - The two codecs coexist during the transition, proven byte-identical by the BS2 conformance harness.
   - Converging `dashcore` onto base-sdk is a **separate, non-blocking track**. The node does not wait for it.
2. **base-sdk is used only for types and codecs, never for rules.** Its `Checkable` impls are incomplete and in places diverge from Core: no block-size check, the merkle-mutation flag is ignored, and `bad-txns-oversize` is never checked. Every consensus rule lives in `dash-consensus` and friends, with Core's exact reject strings.
3. **Decode-domain equivalence is a hard requirement.**
   - For every type the node decodes, base-sdk must accept exactly the byte strings Core accepts.
   - Today it does not:
     - `TxOut` values above `MAX_MONEY` fail to decode instead of failing `CheckTransaction`.
     - Bools other than 0 and 1 are rejected.
     - Special payloads over 10,240 bytes fail to decode.
     - `LlmqType` 103 and 105 are missing.
   - Fixing these (**BS4**) is a base-sdk change and a gate for the node. The node also hashes, stores and relays **Core's re-serialization**; wire bytes are only a fast path, used when they provably equal it.
4. **base-sdk is carried as a local fork branch** (`node/base-sdk`), consumed through a workspace `[patch]` pointing at a `file://` git rev. Every change follows base-sdk's own conventions so it can be upstreamed if pasta chooses.
5. **The Base SDK milestone is 16 PR-sized tasks.** BS1 and BS2 are already running.
   - **Gates node consensus integration:** BS3–BS6, BS10–BS12 and BS15 (fork plumbing, decode domain, params with Devnet and overrides, v24 payloads, bridges, X11 BlockHash, Network mapping, dash-pkc parity fixes).
   - **Gates P2P transport:** BS8a (V1 framing, inventory and short-ID tables).
   - **Gates P2P serving and masternode work:** BS7 and BS8b (typed overlay messages, all 74 commands).
   - **Can start today in parallel:** script interpreter, storage, block files, RPC skeleton, oracle shim, test infrastructure.
6. **Storage engine: RocksDB.**
   - One DB with column families holds the chainstate: coins, block index, evo, mined LLMQ commitments, snapshots and meta. Updates are **atomic across all of them** in one `WriteBatch`, which removes Core's "evodb best block ≠ chainstate tip" failure class.
   - Separate RocksDB instances hold LLMQ runtime data and each optional index.
   - Block data stays in **byte-compatible Core `blk*/rev*.dat` flat files**, so Core datadirs import by hardlink and undo records can be diffed against dashd.
   - MDBX is the named fallback. An M0 bake-off on a real Dash UTXO churn trace confirms or overturns the choice before M3.
7. **Single-writer chainstate.**
   - One validation thread owns all mutable chain state.
   - Readers (P2P serving, RPC, mempool) see immutable `ChainView` snapshots published via `ArcSwap`. There is no global `cs_main`.
   - Core's state-dependent validity (ChainLocks, InstantSend, governance, MN sync) enters through an injected **`ChainHelper`** trait, like Core's `CChainstateHelper`.
8. **IBD is a parallel pipeline:**
   - parallel decode, hashing and context-free checks;
   - UTXO prefetch;
   - sequential contextual connect;
   - asynchronous layered cache flush.

   Together with O(log n) masternode payee selection and cached SML hashes, this is the main 2x lever. Under assumevalid, dashd is effectively single-threaded.
9. **Per-metric 2x targets are argued individually** (§3.9). Some cannot be 2x on identical hardware, and the shortfall is documented up front:
   - ECDSA throughput per core (same libsecp256k1);
   - disk footprint (block files dominate);
   - DKG wall-clock (bound by block height).
10. **P2P is a new `dashd-p2p` crate**, not dash-spv's network layer.
    - It has an inbound and outbound connection manager, a Core-faithful addrman and banman, and **#264's BIP324 transport lifted into it**, generalised for the responder role.
    - net_processing behaviour (message order, timers, limits, penalties) is ported for **fingerprint parity** and checked by a transcript-diff suite against dashd.
    - The node does not depend on, modify or block on #902 (dash-spv's manager rewrite).
11. **Masternode overlay** crates: `dashd-quorums` (quorum manager, signing, sig shares, DKG), `dashd-locks` (ChainLocks and InstantSend), `dashd-governance`, `dashd-coinjoin`, `dashd-masternode` (active MN, mnauth, MN connections).
    - They plug into P2P through a `NetHandler` trait mirroring Core's handler chain.
    - BLS uses `dash-pkc` (blst). It already implements both schemes, including Chia's legacy hash-to-G2, so nothing needs porting from dashbls. R4 found four parity fixes that are needed: the secure-aggregation weight reduction, the IES key encoding, Dash wrapper semantics, and the global-scheme rule (BS15).
12. **The wallet extends `key-wallet`/`key-wallet-manager`.**
    - A node-side `dashd-wallet` crate adds SQLite persistence, multiwallet, wallet RPCs, the CoinJoin client, ProTx and governance signing, and Core wallet import (SQLite plus a pure-Rust BDB 4.8 reader).
    - `WalletInterface` gains disconnect and mempool-removal hooks (#854).
13. **Verification is built into the architecture:**
    - Pure crates are directly fuzzable.
    - A **Dash Core oracle shim** (`libdash_oracle`, a C ABI built from our Dash Core branch) powers in-process differential fuzzing of every parser and rule.
    - A **per-block `StateDigest`** is emitted by both nodes (Core patch `-statedigestlog`, plus R4's RPC golden extraction), giving chain-replay equality over mainnet and testnet with `-assumevalid=0`. UTXO equality uses MuHash (coinstatsindex); MN-list equality uses Core's own `CanonicalMNListHash`.
    - **Mutation testing** checks rejection parity. GBT proposal mode and `testmempoolaccept` serve as Core-side oracles.
    - R4's 34-entry **quirk register** is normative: every quirk gets a named test.
    - **Dash Core's own functional tests run against `rdashd`**, unmodified where possible, with per-node binary selection for **mixed dashd/Rust masternode networks**.
    - Every optimised consensus algorithm keeps a naive Core-faithful twin, cross-checked in replay and fuzzing.
14. **Sequencing.** M0 foundations → M1 Base SDK milestone → M2 headers-first sync → M3 non-evo validation → M4 evo/LLMQ consensus plus full mainnet/testnet parity → M5 full-node services → M6 wallet → M7 masternode → M8 mainnet masternode and performance report.
    - Critical path: BS5/BS6 → chainstate core → DML → LLMQ commitments and snapshots → full-sync parity → P2P serving → mnauth and DKG → sigshares → CL/IS signing → mixed-quorum interop → testnet MN → mainnet MN.
    - Ten workstreams (W1–W10) run in parallel, with 7–10 implementers at a time (§5.4).
15. **Seven open questions for pasta** (§7): the default user agent, the license of the node crates, binary and datadir names, the base-sdk upstreaming posture, mainnet MN resources, the public parity claim (tagged Core releases), and the desktop-app ↔ node relationship.

---

## 1. Type stack

### 1.1 The problem

The node needs one authoritative definition of every Dash wire structure, and it must agree **bit for bit** with Core. Five forces pull in different directions:

| Force | Implication |
|---|---|
| Program instruction: "integrate Base SDK first" | base-sdk should be the home of protocol shapes, not a third parallel copy. |
| Consumers: about 2,100 `dashcore::` uses in Platform, about 630 in DET, mobile FFI, desktop | `dashcore`'s public API and bincode layouts must not churn gratuitously. |
| base-sdk maturity (R3) | Only `types`/`num`/`pkc` are released. `primitives`/`params`/`p2p_core` are incubating and missing Devnet, v24 payloads, CL/IS/QRInfo types, V1 framing, 55 typed P2P messages and LLMQ param tables. |
| Consensus parity | Codecs must reject exactly what Core rejects, at the same layer: decode error vs validation error matters for block-index failure marking and peer scoring. |
| Performance | Hashes computed once per object; no re-encode to hash; no per-hop deep copies. |

### 1.2 Options considered

| Option | Description | Verdict |
|---|---|---|
| **A. dashcore-native node** | Build node crates on `dashcore` types and migrate `dashcore` onto base-sdk underneath later (R3 Phase 3). | **Rejected.** `dashcore`'s shapes are wrong for a node: payloads are parsed eagerly, so unknown types fail the whole block; there is a vestigial `Witness`; there is a BIP144 decode path; txids are not cached; compact sizes are u32. Fixing these is as breaking for Platform as migrating, and Phase 3 would then churn the node a second time. |
| **B. base-sdk-native node, bridged to dashcore** | Node crates use base-sdk types directly. `dashcore` keeps its API and gains `From`/`TryFrom` bridges (byte round-trip). Convergence is a later, separate track. | **Chosen.** One source of truth for protocol shapes going forward, with no consumer breakage. The second codec only lives inside the SPV and wallet path and is guarded by BS2. |
| **C. Full migration first** | Finish R3 Phases 1–3 (dashcore wraps base-sdk everywhere), then start the node. | **Rejected.** About 20+ weeks on the critical path behind an unreleased, single-maintainer SDK. |
| **D. Node-private types** | The node defines its own primitive types and codec. | **Rejected.** That makes a third codec, contradicts the program instruction, and duplicates base-sdk work. |

### 1.3 Decision: what each layer owns

```
                ┌──────────────── consumers: Platform, DET, mobile FFI, desktop ───────────────┐
                │                                                                              │
   dashcore (API surface; bincode owner; FFI-stable Display)  ──bridge(bytes)──►  base-sdk types │
   key-wallet / key-wallet-manager / dash-spv (unchanged deps on dashcore)                      │
                                                                                                │
   dash-consensus, dash-evo, dash-llmq, dash-governance, dash-coinjoin  (rules; sans-io)  ◄─────┘
   dashd-* runtime crates (state, I/O)                     all on base-sdk types
```

- **base-sdk owns protocol shapes and codecs.** Blocks, transactions, special payloads (all 13 types, every version), P2P messages (all 74 commands, framing V1 and V2 contents), chain parameters (including Devnet and every regtest/devnet override), LLMQ parameter tables, hash blob types, 256-bit arithmetic, X11, BLS/ECDSA/EdDSA key and signature types. All of these must be stateless, no_std, and free of I/O, per base-sdk policy.
- **rust-dashcore owns rules, state and runtime.** The script interpreter, sighash, every validation rule, the DML state machine, LLMQ math, the chainstate, P2P runtime, RPC, wallet and binaries.
  - The interpreter and sighash are stateless and could move to base-sdk later. The pure crates are written `#![no_std]`-compatible (alloc only, no `std::io`, no globals) to keep that option open, but they start in rust-dashcore for velocity: our review process, edition 2024, no external maintainer queue.
- **`dashcore` keeps its public API.** It gains a `bridge` module (`From`/`TryFrom` to and from base-sdk) that converts through bytes, plus `debug_assert!` checks that the txid and hash are unchanged. Persisted bincode stays owned by `dashcore` newtypes (R3 recommendation 0.2(a)). The node never uses bincode.

**Why the second codec is acceptable.** Each codec is the source of truth for a disjoint set of consumers:
- the node only ever decodes with base-sdk;
- SPV, wallet and FFI only ever decode with `dashcore`.

The BS2 harness decodes every mainnet and testnet block and transaction through both and through dashd, and asserts byte-identical re-encoding, txid, instance hash, block hash and merkle root. The node's wallet boundary converts only wallet-relevant transactions (§2.19), so the second codec is never on the node's hot path. The codec disappears when the convergence track finishes (§1.9).

### 1.4 Node-level wrappers over base-sdk types (performance and safety)

base-sdk types recompute hashes on every call (`Hashable for Transaction` re-encodes into a fresh `Vec`), re-parse special payloads on every `decode_payload()`, and `Block::merkle()` re-encodes every transaction. The node therefore wraps them once, at decode time, in `dash-consensus::primitives`:

```rust
/// Immutable, shared, hash-cached transaction. Built only by `TxDecoder` (from bytes) or `Tx::new` (from parts).
pub struct Tx {
    raw: dash_primitives::Transaction,          // base-sdk shape (inputs/outputs/extra_payload bytes)
    txid: TxHash,                               // "stable" txid: AssetUnlock v2 zeroes trailing 132 payload bytes
    instance_hash: TxHash,                      // SHA256d of full serialization (== txid except AU v2)
    size: u32,                                  // serialized size
    payload: OnceLock<Result<SpecialPayload, PayloadError>>, // decoded at most once
    legacy_sigops: OnceLock<u32>,
}
pub type TxRef = Arc<Tx>;

pub struct Block {
    header: BlockHeader,        // base-sdk BlockHeader (Copy)
    hash: BlockHash,            // X11, computed once
    txs: Vec<TxRef>,
    size: u32,
    merkle: (MerkleRoot, bool), // (root, mutated) from the cached txids; CVE-2012-2459 flag preserved
}
```

- **`TxDecoder` / `BlockDecoder`** decode from a byte slice using base-sdk `BaseCodec::decode` directly, not the `VecDecoder` push path that copies twice. They track each transaction's byte range. One pass gives types plus all hashes. Peer tasks run this on the network threads, so hashing is parallel across peers for free.
- **Core hashes its re-serialization, not the wire bytes.**
  - Core's `GetHash()` is `SerializeHash(*this)`.
  - The decoder computes `txid = sha256d(&bytes[range])` (**zero re-encoding**) only on a fast path where the re-serialization provably equals the wire bytes.
  - Otherwise it hashes `dash_consensus::primitives::core_serialize(tx)`.
  - The same canonical bytes are what the node stores in `blk*.dat` and relays. For every transaction, re-serialization equality is asserted in debug builds and fuzzed (`diff_tx_decode`).
- **Payloads stay raw** (`extra_payload: Vec<u8>`) until a rule needs them. Unknown types and versions are preserved byte-exactly. This is the forward-compatibility property `dashcore` lacks (R1 §7.7).
- **BLS fields in wire types are byte bags** (`[u8; 48]`, `[u8; 96]`) with no scheme attached. They are interpreted only inside rules under an **explicit `BlsScheme` context**, never a global flag (R2 G.1 #1). base-sdk's `make_bytes!` bags fit this, but some base-sdk wire types currently bind a scheme in the type: `FinalCommitment` and the SML operator key are hard-coded to IETF/basic (R3 §3). BS6 and BS7 change those fields to scheme-agnostic bags, with the scheme chosen by payload version or by context, exactly as Core's version wrappers and global flag do.

### 1.5 Decode-domain equivalence (hard rule)

> **Rule DDE.** For every type `T` the node decodes from untrusted bytes, the set of byte strings that base-sdk decodes successfully (with all input consumed where Core requires it) equals the set that Core's `Unserialize` accepts. Re-encoding a decoded value reproduces **Core's re-serialization** of it. That equals the input bytes except where Core itself normalizes, e.g. #8 below. Range, semantic and policy checks belong in rules, never in decoders.

Why it matters:
- **Blocks.** Core deserializes a block whose transaction has a negative or over-`MAX_MONEY` value, then marks it `BLOCK_FAILED_VALID` with `bad-txns-vout-negative` / `bad-txns-vout-toolarge`. It also gives the peer 100 points. A decoder that rejects the bytes never creates that failed index entry, scores the peer differently, and returns a different `getchaintips` / `submitblock` result.
- **P2P.** Core accepts any non-zero byte as `true` (`Core:serialize.h:239`) in `version.relay`, `fMasternode`, `senddsq`, `qsendrecsigs` and others. A strict decoder disconnects or penalises peers that dashd tolerates, which is a fingerprint.

Known divergences found while writing this design (all in base-sdk at 33fcc59):

| # | Divergence | Core behaviour | base-sdk behaviour | Fix (BS4) |
|---|---|---|---|---|
| 1 | `TxOut.value` | int64, any value decodes; `CheckTransaction` rejects | `Amount::from_sat` rejects > MAX_MONEY at decode (`primitives/src/transaction.rs:103-114`) | store `value: i64` (Core `CAmount`). Provide `Amount` accessors that fail softly |
| 2 | `bool` | any byte, non-zero = true | only 0/1 (`types/src/codec.rs:279`) | `CoreBool` decode policy for P2P structs (or lenient `bool` behind a decode-policy type) |
| 3 | `vExtraPayload` length | any length up to `MAX_SIZE` (32 MiB); `CheckTransaction` enforces 10,000 | decode cap 10,240 (`primitives/src/codec.rs:10`) | decode cap = `MAX_SIZE`. The 10,000 check moves to rules |
| 4 | `LlmqType` 103 and 105 | valid u8 values | `Unknown` in primitives (two inconsistent enums) | a single `LlmqType` with all 14 types |
| 5 | Unknown P2P command | logged and ignored, no penalty | `P2pDecodeError::UnknownCommand` | `P2pMsg::Unknown { cmd, payload }` |
| 6 | Payload trailing bytes | `GetTxPayload` must consume all bytes; extra bytes are a *rule* failure (`bad-protx-payload`) | per-type | keep in rules; the payload decoder reports "consumed n of m" |
| 7 | Version-dependent encodings | SML entry, mnlistdiff, qrinfo and `dsa` encodings vary with the peer's protocol version | no context parameter in `BaseCodec` | `ProtoCtx` parameter (`decode_ctx(data, &ProtoCtx)`) for version-dependent P2P types (BS7) |

Enforcement: **differential decode fuzzing** of every node-decoded type against `libdash_oracle` (§4.4). A target is "green" after ≥24 CPU-hours with no divergence plus the full-history BS2 corpus.

### 1.6 Specific issues the brief asks about

**bincode.**
- The node uses none. On-disk formats are:
  - Core consensus serialization for chain data: blocks, undo, coins (Core `Coin` compression), block-index records;
  - explicit versioned records (implemented with `BaseCodec`) for node metadata.
- `dashcore` keeps bincode on its newtypes for wallet blobs, SPV storage and Platform, and the bridges never touch bincode.
- base-sdk does **not** get bincode back. This preserves kwvg's base-sdk#1 decision and avoids the orphan-rule problem.

**Hash display order.**
- Bytes are canonical: internal little-endian order, exactly as Core stores them.
- The node's RPC, CLI and ZMQ formatting follows Core's rule. All `uint256`/`uint160` blobs use `GetHex` (byte-reversed): txid, block hash, proTxHash, quorumHash, confirmedHash, vvecHash, `platformNodeID`. Byte strings such as scripts and BLS keys or signatures are forward hex.
- base-sdk's reversed `Display` matches Core, so the node uses it.
- `dashcore`'s per-type forward display (ProTxHash, PubkeyHash, MerkleRoot*) is an FFI-compat concern. It stays and is irrelevant to the node.

**Network enum and Devnet.**
- `dash_params` gains:
  - **`NetworkKind { Mainnet, Testnet, Devnet, Regtest }`**: `Copy`, with stable discriminants 0..3 that match `FFINetwork`.
  - **`ChainParams`**: an owned, non-`'static` structure built by `ChainParamsBuilder::new(kind).devnet_name(..).apply_overrides(&args)`.
- Every Core override goes through the builder: `-devnet` (genesis is mined from the name), `-highsubsidy*`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-llmq{chainlocks,instantsenddip0024,platform,mnhf,devnetparams}`, `-llmqtest*params`, `-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams` (Dash 9-field form) and `-sporkaddr`.
- The node threads `Arc<ChainParams>` explicitly through every crate. **No global `Params()`** — Core's global use is a known bug source (R2 A.4 `IsQuorumRotationEnabled` uses global `Params()`).
- `dash-network::Network` stays as is for SPV/FFI, with `From<Network> for NetworkKind` (BS12).

**v24 payloads (#1136–#1138).**
- The node's canonical v24 implementation lives in base-sdk `dash-primitives` (BS6): CbTx v4, AssetLock/AssetUnlock v2 including the stable txid, ProRegTx v3 with ExtNetInfo / DIP-26 payouts / shares / join sigs / early period, ProUpServ/ProUpReg/ProUpRev v3, and types 10–12.
- ZocoLini's upstream rust-dashcore PRs land in `dashcore` on upstream's schedule; we merge `dev` periodically. We neither block nor duplicate them in `dashcore`. When they land, BS2 proves `dashcore` ≡ base-sdk on testnet blocks that carry v24 payloads. V24's testnet start time is 2026-09-24, so real v24 transactions will appear in the corpus.
- The `dashcore` bridge handles both shapes in the meantime (unknown types go through #1136's raw passthrough).

**BlockHash X11 footgun.**
- The node is immune: base-sdk `BlockHeader` always hashes with X11.
- We still fix `dashcore` (BS11). X11 becomes unconditional and `core-block-hash-use-x11` becomes a deprecated no-op, so a bridged `dashcore::BlockHash` can never disagree with the node's.
- The C `rs-x11-hash` backend stays the default until `dash-pow` (pure Rust) benchmarks at parity. Today `dash-pow` has no runtime AES-NI dispatch (R3); BS14 adds it.

**One codec vs two.** Covered in §1.3. The end state is one codec, once `dashcore` types become thin wrappers over base-sdk (§1.9). The node is written so that end state requires no node changes.

### 1.7 The Base SDK milestone (ordered, PR-sized)

"base-sdk?" marks PRs that change base-sdk itself. These go to `~/workspace/base-sdk` branch `node/base-sdk`, follow base-sdk conventions (`namespace%type(scope): …` commits, 2-space style, `just test lint`, no `unwrap`/`panic`), and are structured to be upstreamable. Effort is in agent-weeks (aw).

| ID | Task | Repo | base-sdk? | Depends | aw | Gate / exit criterion |
|---|---|---|---|---|---|---|
| **BS1** *(running)* | Re-pin base-sdk to develop@33fcc59. Absorb #1110 and #1142 (`Arith256`). Fold SPV `ChainWork` into `Arith256`. Deduplicate `EddsaPkHash`. Drop the vestigial `dash-types` dependency in `dash`. Stop using `dash_pkc::__deps::ff` (R3 1.1–1.5). | RD | maybe (small pkc API) | — | 1.5 | workspace tests + dashd suites green |
| **BS2** *(running)* | Reference dashd oracles on `/work/chains` (24.0.0-rc.3 plus a develop build). Conformance harness: every mainnet and testnet block and transaction decoded by dashcore, base-sdk and dashd, with byte-identical re-encode, txid, instance hash, block hash and merkle root (R3 2.1). | RD | no | — | 2 | full-history run is green; becomes a CI job |
| **BS3** | **Fork plumbing.** Create the `node/base-sdk` branch. Add a workspace `[patch."https://github.com/dashpay/base-sdk"]` for all 8 crates pointing at `git = "file:///work/home/agent/workspace/base-sdk"`, `rev = …`. Add `contrib/node/bump-base-sdk.sh` (rebase onto upstream develop, re-pin, run gates). Add a check that only one base-sdk rev resolves. | RD + BS | infra only | BS1 | 0.5 | `cargo tree -d` shows one rev |
| **BS4** | **Decode-domain alignment** (§1.5 #1–#6): `TxOut.value: i64`, `CoreBool`, the payload decode cap, unified `LlmqType`, `P2pMsg::Unknown`, payload consumption reporting. Then differential decode fuzz targets against the oracle for `Transaction`, `Block`, `BlockHeader` and every payload. | BS | **yes** | BS3, O1 | 2 | §1.5 enforcement met |
| **BS5** | **`dash-params` completion** (§1.6):<br>• `NetworkKind` with Devnet; `ChainParamsBuilder` with every Core override.<br>• The full `LlmqParams` table: 14 types, all 15 fields, derived values. Per-network LLMQ roles.<br>• Spork address. DGW/KGW parameters. MN payment schedule (increase block/period, BRR periods table). Superblock/budget params. 40 mainnet checkpoints. assumevalid and minimum chain work. BIP155 fixed seeds. `CreditPoolPeriodBlocks`. CoinJoin participant limits. `fRequireRoutableExternalIP` and the other policy flags. BIP9 deployments as an indexable array with the Dash fields.<br>• **Oracle corpus**: a JSON dump of `CChainParams` from Core for the 4 networks plus 12 override combinations, with an equality test. | BS | **yes** | BS3 | 2.5 | equality with the Core dump |
| **BS6** | **`dash-primitives` v24**: CbTx v4; AssetLock v2; AssetUnlock v2 with `txid()` implementing the 145-byte / `payload[0]>=2` zeroing rule and `instance_hash()`; ProRegTx v3 (ExtNetInfo with ADDRv2 services and domain entries, u8-counted payouts, shares and join sigs, early period and penalty); ProUpServ/ProUpReg/ProUpRev v3; ProDisTx, ProUpShareTx, ProUpSharedRegTx. Per-payload `sig_hash_bytes()` that reproduces SER_GETHASH exclusions exactly (type 10 signs a separate digest; types 1/3/11 exclude `vchSig`; type 12 excludes its sig list). Corpora: Core `trivially_valid/invalid.json`, testnet v24 blocks, regtest vectors exported from `feature_masternode_shares.py` and `feature_protx_version.py`. | BS | **yes** | BS4 | 3 | BS2 green on the corpora; oracle diff-fuzz green |
| **BS7** | **Typed Dash overlay wire structures** in `dash-primitives`:<br>• ChainLockSig, InstantSendLock, CRecoveredSig.<br>• Sig-share family: CSigShare, CSigSesAnn, CSigSharesInv/qgetsigs (AUTOBITSET), CBatchedSigShares.<br>• DKG: qcontrib/qcomplaint/qjustify/qpcommit (DYNBITSET with padding-bit rejection, BLS-IES multi-recipient blobs); CFinalCommitment v1–4 and its tx payload.<br>• LLMQ data: CQuorumSnapshot, GetQRInfo/QRInfo, SML entry v1–3 (version-gated by protocol), MnListDiff (version-gated), qgetdata/qdata.<br>• Node messages: MNAUTH, CSporkMessage, PlatformBanMessage.<br>• CoinJoin: dsa (`nFlags` ≥70241), dsi, dsf, dss, dsc, dssu, dstx, dsq.<br>• `CompressibleBlockHeader` / headers2 stream codec (7-entry version MRU, int16 time deltas, inverted "bit set = field present" flags).<br>• Introduces `ProtoCtx`. | BS | **yes** | BS4 | 4 | oracle diff-fuzz per type; vectors from `messages.py` and Core captures |
| **BS8a** | **`dash-p2p-core` framing and tables** (needed by the M2 transport):<br>• V1 framing codec: 24-byte header encode/decode, checksum, Core's command-string validation (printable ASCII, NUL-padded), oversize detection. Pure functions only, no I/O.<br>• Full inventory table (types 1–34; unknown types preserved).<br>• Short-ID table (0–32 BIP324 order with 5 and 29–32 empty; 128–168 Dash) with the send-side gate `short_id(cmd, peer_version)`: ≥70235, and ≥70240 for `platformban`.<br>• A complete protocol-version constants table (R2 A.2 §1.1).<br>• `block`/`tx` un-stubbed onto `dash-primitives`. | BS | **yes** | BS4 | 1 | framing diff-fuzz against the oracle; `-capturemessages` headers round-trip |
| **BS8b** | **All 74 commands typed** using BS7. `decode(cmd, payload, &ProtoCtx)`; `Unknown { cmd, payload }` preserved. | BS | **yes** | BS7, BS8a | 1.5 | `-capturemessages` corpus round-trips; oracle diff-fuzz per command |
| **BS9** *(optional)* | `dash-script`: a Core-exact `GetOp` iterator (truncated-push semantics), `Solver`/`TxoutType`, accurate (P2SH) sigop counting. Only where trivially upstreamable; otherwise this stays in `dash-consensus`. | BS | yes | BS3 | 1 | Core script vectors |
| **BS10** | **dashcore ⇄ base-sdk bridge** (R3 1.7, extended): `dashcore::bridge` behind feature `base-sdk-bridge`. Covers hashes, OutPoint, TxIn/TxOut, Transaction, Header, Block, every special payload, ChainLock, InstantLock, MnListDiff, QRInfo. Conversions go through bytes with `debug_assert` on hash equality. Tests run over the BS2 corpus. | RD | no | BS6 | 1.5 | round-trip equality on the full history |
| **BS11** | **X11 `BlockHash` unconditional in dashcore.** `core-block-hash-use-x11` becomes a no-op. Add a `hash_x11` backend switch (C default; `dash-pow` behind a feature) with a differential test and a benchmark. Plus a base-sdk perf PR: runtime AES-NI and SSE dispatch in `dash-pow`. | RD (+BS) | perf part **yes** | BS1 | 1 | header sync bench: Rust X11 ≥ C before switching the default |
| **BS12** | **Network mapping.** `From`/`Into` between `dash_network::Network` and `NetworkKind` (`FFINetwork` discriminants and serde names unchanged). Deprecate `dashcore::consensus::Params` for new code. | RD | no | BS5 | 0.5 | FFI tests unchanged |
| **BS13** | **dashcore SPV correctness** (not node-blocking; parallel):<br>• inventory `CompactBlock = 20`;<br>• compact-size cap `MAX_SIZE`;<br>• `PROTOCOL_VERSION` 70242;<br>• `MAX_MSG_SIZE` 3 MiB;<br>• honour the payload length prefix (adopt #1136 when merged);<br>• remove the BIP144 decode path;<br>• fix DIP-0020 opcode naming (0xba = `OP_CHECKDATASIG`). | RD | no | — | 2 | dash-spv dashd suites green |
| **BS15** | **`dash-pkc` parity fixes** (R4 §1.9.4, E§11.1). Four changes:<br>• reduce secure-aggregation weights as dashbls does;<br>• derive the BLS-IES symmetric key from the **basic** encoding of the ECDH point;<br>• Dash wrapper semantics: all-zero bytes → null; a decoded infinity → invalid; bytes canonical only in the opposite scheme → *invalid object, not an error*; the malleability round-trip check;<br>• scheme selection as an explicit parameter, mapped to Core's global-flag rule by callers.<br>Vectors: dashbls `test.cpp` (Chia, Legacy, threshold, IES) plus `diff_bls_*`. | BS | **yes** | BS3, O1 | 1.5 | oracle diff-fuzz (legacy and basic), dashbls vectors, replay 1028160..1899071 with `-assumevalid=0` |
| **BS14** | **base-sdk performance helpers** used by the node: `encoded_len()` without allocation; streaming SHA256d `Hashable` (no temporary `Vec`); a `decode_with_ranges` hook so node decoders get transaction byte ranges without copying. | BS | **yes** | BS4 | 1 | microbenchmarks; no behaviour change |

**Ordering and critical path.** BS1 → BS3 → BS4 → {BS5 ∥ BS6 → BS10 ∥ BS8a ∥ BS15} → BS7 → BS8b.
- With 2 agents on base-sdk, BS4–BS6 plus BS8a take about 4 calendar weeks. BS8a unblocks the M2 transport (P1).
- BS7 and BS8b take about 3 more weeks and are needed by M5 (P2P serving) and M7 (masternode), not by M2–M4.

**"Base SDK milestone done"** means all of the following:
- BS1–BS8b, BS10–BS12 and BS15 are merged.
- BS2 runs green over full mainnet and testnet history across all three implementations.
- Every BS4/BS6/BS7/BS8a/BS8b differential fuzz target has ≥24 CPU-hours clean.
- `dashcore` builds Platform's usage patterns unchanged. There is a compile-only smoke test against a vendored copy of `rs-dpp`'s `dashcore` import list.

**What node work proceeds before the milestone is done** (from day one, M0):
- `dash-consensus::script`: interpreter, sighash and sigops. It needs only base-sdk `Transaction`, which exists today.
- DGW, KGW, subsidy and the floating-point module. They use temporary constants until BS5 lands, then switch to `ChainParams`.
- `dashd-store` and the RocksDB/MDBX bake-off.
- Block files (Core `blk`/`rev` format).
- `dash-args`, `dashd-rpc` skeleton, `dash-rpc-spec`.
- `libdash_oracle` shim (O1) and the differential-fuzz scaffolding.
- Functional-test runner plumbing.
- The bench harness (B1).

**What waits:**
- `dashd-chainstate` integration with real blocks waits for BS5 and BS6. V24 signalling on testnet opened on 2026-09-24, so v24 payloads can appear there any time after activation, and every block needs exact params.
- The `dash-p2p-transport` V1 path waits for BS8a. `dashd-p2p` overlay serving waits for BS8b.
- Overlay runtimes wait for BS7.

### 1.8 Fork mechanics and drift control

- **Branches.** Local base-sdk repo `~/workspace/base-sdk`: branch `node/base-sdk` on top of upstream `develop`. Each BS task gets its own branch `node/bs-<slug>` there, reviewed like any node task, and merged into `node/base-sdk` with `--no-ff`.
- **Weekly rebase** of `node/base-sdk` onto upstream `develop`:
  - The manager runs `bump-base-sdk.sh`, which runs base-sdk's `just test lint`, rust-dashcore's gates and BS2 on the testnet corpus.
  - A breaking upstream change (base-sdk ships several a month) becomes a single adaptation PR in rust-dashcore.
- **Commit hygiene.** Commits stay upstreamable: no node-specific hacks in base-sdk. Anything node-only, such as the `Tx`/`Block` wrappers and decode-with-ranges *consumers*, lives in rust-dashcore.
- **Tracking.** `docs/node-program/BASE-SDK-FORK.md` lists every fork commit, its upstream status, and whether upstreaming is intended (pending pasta's Q4, §7).

### 1.9 dashcore convergence track (post-M4; 1 agent; not on the critical path)

This is R3 Phase 3 (3.1–3.9), re-ordered so the node never waits on it:
1. Delete the Bitcoin vestiges (#400 equivalent).
2. Payloads become wrappers over `dash_primitives` (bincode kept on the wrappers).
3. Hash newtypes, with 0.14-API compatibility shims.
4. Tx/Block wrappers.
5. P2P messages.

Each step ships with a Platform-impact note. Nothing is pushed upstream without pasta's go-ahead. The end state: `dashcore` is a thin, bincode-owning façade over base-sdk, the bridge module becomes the identity, and one codec remains.

---
## 2. Node crate architecture

### 2.1 Principles

1. **Two layers, enforced by dependencies.**
   - **Pure crates** (`dash-consensus`, `dash-evo`, `dash-llmq`, `dash-governance`, `dash-coinjoin`) take no dependency on tokio, RocksDB, sockets, clocks, threads or globals. All context is passed in explicitly (`&ChainParams`, deployment states, `BlsScheme`, the current time as a value), and so is all state, as immutable inputs plus trait-object views.
   - They are `#![no_std]` + `alloc`, with an optional `std` feature, as in base-sdk. A CI job builds them with `--no-default-features`, which fails on any `std` use in our code. Cross-target builds are optional, since secp256k1 and blst would need C cross-compilers. This discipline is what makes them fuzzable and differentially testable (§4).
   - **Runtime crates** (`dashd-*`) own I/O, threads, persistence and policy.
2. **Port Core's structure; don't reinvent it.** Module and function names mirror Core (`connect_block` ↔ `ConnectBlock`, `build_new_list_from_block` ↔ `BuildNewListFromBlock`). Every ported function carries a `// Core: validation.cpp:2470 ConnectBlock` anchor comment, which lets reviewers diff logic side by side.
3. **One writer per state domain.** Chainstate, coins, evo state, block index and mined commitments are mutated only by the validation thread. Everyone else reads published immutable snapshots. There is no global `cs_main`.
4. **Explicit context instead of globals.** No `Params()`, no `bls_legacy_scheme` global, no `GetTime()`.
   - A `NodeClock` (real time plus mocktime, used for `setmocktime`/`mockscheduler`) is injected.
   - `ConsensusCtx { params, deployments_at_prev, bls_scheme, time }` is built per block.
5. **Core reject strings everywhere.** `ValidationError { kind: TxValidationResult | BlockValidationResult, reason: &'static str /* "bad-txns-vout-negative" */, debug: Option<String> }` is shared by RPC, P2P scoring and logs.
6. **Feature isolation from existing consumers.** None of the new crates is a dependency of `dashcore`, `key-wallet`, `dash-spv` or the FFI crates. The `[patch]` that points base-sdk at our fork is the only workspace-wide effect, and BS1/BS3 gate it.

### 2.2 Crate map

All crates live in the rust-dashcore workspace as top-level directories, matching the existing flat layout (`dash-spv/`, `key-wallet/`, …). All are `publish = false` until pasta decides otherwise (§7 Q2). A CI group `node` is added to `.github/ci-groups.yml`.

| Crate (dir) | Kind | Responsibilities | Core counterpart | Key deps |
|---|---|---|---|---|
| `dash-consensus` | pure | Node primitives over base-sdk (`Tx`, `Block`, decoders with cached hashes; §1.4). Script interpreter, flags, `ScriptError`, legacy sighash with `vExtraPayload`, DIP-0020 opcodes, sigop counting. `check_transaction`, `contextual_check_transaction`, `check_tx_inputs`, sequence locks and finality. `check_block_header`, `check_block` (merkle + mutation + v4 asset-unlock root), contextual header and block checks. PoW: BTC retarget, KGW, DGW v3, `check_proof_of_work`, chain work. **`fp`**: bit-exact `pow()` (§4.3). Subsidy, MN payment schedule, platform share, treasury part. Deployments: buried, Dash versionbits (dynamic threshold, EHF signal height). `Coin` and Core compression codecs, undo record codecs, `CoinsView` trait. Checkpoints. | `validation.cpp` (pure parts), `consensus/*`, `script/*`, `pow.cpp`, `versionbits.cpp`, `coins.h`, `compressor.*`, `undo.h` | base-sdk, `secp256k1` |
| `dash-evo` | pure | Contextual special-tx checks (ProReg/UpServ/UpReg/UpRev/ProDis/UpShare/UpSharedReg, AssetLock/Unlock, MNHF, CbTx). **Deterministic MN list**: `MnList` and its persistent structures (§2.5), `build_new_list_from_block`, PoSe, payee selection. Payments: `get_block_tx_outs`, `is_block_payee_valid`, `is_block_value_valid` taking a superblock oracle. SML entry, `merkleRootMNList`, `mnlistdiff` builder. Credit pool and limits. MNHF signal set. Shared-collateral covenants. NetInfo validation. Our persisted diff/snapshot codec plus a **Core evodb importer** (immer-order-agnostic reader). | `evo/*`, `masternode/payments.*` | dash-consensus, `imbl` |
| `dash-llmq` | pure | LLMQ param helpers. Member selection (non-rotated and DIP24 rotation, quarters, skip lists, snapshot construction), modifier. `CFinalCommitment::verify`/`verify_null`, commitment-in-block rules (required counts, mining window), `merkleRootQuorums`. `scan_quorums` over an index trait, `select_quorum_for_signing`, `SignHash`, request IDs. Verification of CL, IS and recovered sigs. **DKG session as a sans-io state machine.** BLS-IES. Threshold and Lagrange helpers over `dash-pkc`. Quorum connection topology. `qrinfo` builder. | `llmq/*`, `chainlock/clsig.*`, `instantsend/lock.*`, `bls/*` (logic) | dash-evo, `dash-pkc` |
| `dash-governance` | pure | Object and vote hashing (the non-serialization hashes), signature selection (voting ECDSA vs operator BLS), proposal validator, byte-exact trigger template, trigger parsing, `is_valid_superblock`, `get_payments_limit`, best-trigger selection, tallies and sentinel flags, rate-check buffer. | `governance/{common,object,vote,superblock}.*` (logic) | dash-evo |
| `dash-coinjoin` | pure | Denominations, collateral validity, `is_valid_in_outs` (STANDARD/PROMOTION/DEMOTION/FINAL), DSTX structure (pre/post-V24), queue validity, `is_mixing_threshold_exceeded`. Server and client session state machines (sans-io). Rounds and `is_fully_mixed` (salt). | `coinjoin/{common,coinjoin}.*`, `wallet/coinjoin.cpp` (logic) | dash-evo |
| `dashd-store` | runtime | RocksDB environment wrapper with typed tables (`Table<K: KeyCodec, V: ValueCodec>`), atomic `WriteBatch`, snapshots and iterators. Flat-file sequences (`blk`/`rev`, Core framing, preallocation). `FlatDb<T>` (Core CFlatDB envelope). Core LevelDB reader for import (pure Rust, honours `\0obfuscate_key`). Datadir lock. | `dbwrapper.*`, `flatfile.*`, `flat-database.h` | `rocksdb`, `rusty-leveldb` (import only) |
| `dashd-chainstate` | runtime | `ChainstateManager` actor on the **validation thread**. Block index arena, active chain, header acceptance, `accept_block`, `activate_best_chain`, connect/disconnect, reorg, invalidate/reconsider/precious, ChainLock enforcement entry points. Layered coins cache with async flush; undo I/O. Evo state manager (MN list cache, snapshot/diff persistence), quorum block processor (mined-commitment index), DIP24 snapshot store, credit-pool cache, MNHF store, versionbits cache. `ChainView` publication, `ChainEvent` bus. Flush, crash replay, `-reindex(-chainstate)`, `-loadblock`, Core datadir import, `test_block_validity`, `verifychain`. | `validation.cpp`, `node/blockstorage.*`, `txdb.*`, `evo/{evodb,deterministicmns,creditpool,mnhftx}` (state parts), `llmq/{blockprocessor,snapshot}` (state parts) | dash-evo, dash-llmq, dashd-store, `rayon`, `arc-swap`, `crossbeam` |
| `dashd-mempool` | runtime | `TxMemPool` with all Dash maps (ProTx conflicts, asset-unlock instances and indexes, mempool address/spent index), two-phase parallel ATMP, policy and standardness (Core strings), orphanage, IS-aware conflict and eviction handling, expiry, `mempool.dat` v1 persistence, fee estimator (`fee_estimates.dat`-compatible), ancestor-score package index for mining. | `txmempool.*`, `validation.cpp` MemPoolAccept, `policy/*`, `txorphanage.*`, `kernel/mempool_persist.*` | dashd-chainstate |
| `dashd-mining` | runtime | `BlockAssembler` (commitments first, packages, credit-pool diff, EHF stage, IS safety), CbTx field computation, `getblocktemplate`/`submitblock`/`generate*` back ends, regtest internal miner. | `node/miner.*`, `rpc/mining.cpp` (logic) | dashd-mempool, dashd-governance (payments) |
| `dash-p2p-transport` | runtime | V1 framing over base-sdk codecs. **BIP324 V2 lifted from #264**, generalised to initiator and responder, with Dash HKDF salt, short-ID version gating, inbound v1 detection and v1 reconnect fallback. Async `Transport` trait. Reusable by dash-spv later. | `net.cpp` transports, `bip324.*` | tokio, `bip324`, dash-p2p-core |
| `dashd-p2p` | runtime | Connection manager (listeners, outbound slot types, feelers, anchors, addnode, DNS and fixed seeds, MN connection API), Core-faithful addrman (peers.dat V4_MULTIPORT), banman (banlist.json, discouragement filter), **peer manager** (net_processing port), object request tracker, block download scheduler, headers/headers2 sync, compact blocks, serving, BIP37 (with Dash special-tx matching), BIP157 serving, misbehavior table, `NetHandler` registry, `-capturemessages`. | `net.*`, `net_processing.*`, `addrman*`, `banman.*`, `txrequest.*`, `blockencodings.*`, `node/eviction.*`, `common/bloom.*` | dash-p2p-transport, dashd-chainstate, dashd-mempool |
| `dashd-sporks` | runtime | Spork manager: store (`sporks.dat` Core format), signature verification (testnet hash vs signmessage forms), mainnet hard-coded values, relay, `sporkupdate` signing. | `spork.*` | dashd-p2p |
| `dashd-quorums` | runtime | Quorum manager (cache, building, vvec/sk-share store in `llmq/`), signing manager (recsig DB, conflict and vote tracking, listeners), sig-shares manager (MN), DKG session manager and per-(type, index) phase handlers (MN; `dkgdb`), data recovery (qgetdata/qdata), connection planner, EHF signal signer, net handlers (NetQuorum/NetSigning/NetDKG). | `llmq/{quorums,quorumsman,signing,signing_shares,net_*,dkgsessionmgr,ehf_signals}`, `active/dkg*` | dash-llmq, dashd-chainstate, dashd-p2p |
| `dashd-locks` | runtime | ChainLock handler and signer; InstantSend manager, DB and signer; `ChainHelper` back end for CL and IS. | `chainlock/*`, `instantsend/*` | dashd-quorums, dashd-mempool |
| `dashd-governance` | runtime | Governance manager (objects, votes, orphans, postponed objects, `govsync` handler, store), superblock trigger store (the `ChainHelper` superblock back end), governance signer (MN triggers and votes). | `governance/{governance,net_governance,signing}.*` | dash-governance, dashd-p2p |
| `dashd-coinjoin` | runtime | CoinJoin server (MN), DSTX manager, DSQ relay modes. | `coinjoin/server.*`, `coinjoin.cpp` runtime parts | dash-coinjoin, dashd-p2p, dashd-mempool |
| `dashd-masternode` | runtime | Active masternode manager (states, operator key, local address checks), MNAUTH, masternode sync plus `SyncManager` tick (`getsporks`/`mempool`/`govsync`), mncache meta, netfulfilled, platformban processing, MN connection maintenance. | `active/masternode.*`, `evo/mnauth.*`, `masternode/{sync,meta,utils}.*`, `node/sync_manager.*`, `netfulfilledman.*` | dashd-quorums, dashd-p2p |
| `dashd-indexes` | runtime | `BaseIndex` framework (async, own DB, locator), txindex (compact keys, §3.8), addressindex, spentindex, timestampindex, blockfilterindex (Dash-extended BIP158 + `fltr*.dat`), coinstatsindex (MuHash3072). | `index/*`, `blockfilter.*`, `evo/specialtx_filter.*` | dashd-chainstate |
| `dashd-wallet` | runtime | Node wallet service on key-wallet / key-wallet-manager: multiwallet, SQLite persistence, chain sync and rescans, wallet RPC back ends, CoinJoin client, ProTx/gobject/BLS signing ops, Core wallet import (SQLite, BDB 4.8 reader, dump formats). | `wallet/*`, `coinjoin/client.*` | key-wallet(-manager), dash-coinjoin, dashd-chainstate, dashd-mempool |
| `dash-rpc-spec` | shared | Declarative RPC table: names, Dash sub-commands, args with types/aliases/defaults, result docs, categories, Core help text. Also the dash-cli parameter-conversion table and whitelist naming. | `rpc/util.*` RPCHelpMan, `rpc/client.cpp` | serde_json |
| `dashd-rpc` | runtime | HTTP/1.1 server, auth (cookie, rpcauth, user/pass), `-rpcwhitelist`, work queues (incl. `-rpcexternaluser`), JSON-RPC 1.x dispatch with sub-command squashing, batches, `/wallet/<name>`, REST, handler modules per category, error codes, Core number formatting. | `httpserver.*`, `httprpc.*`, `rpc/*`, `rest.cpp` | hyper, dash-rpc-spec, all subsystems |
| `dashd-zmq` | runtime | All 19 topics with Core framing and sequence numbers. | `zmq/*` | `zmq` (libzmq) |
| `dash-args` | shared | ArgsManager port: `dash.conf` sections, `includeconf`, NETWORK_ONLY, negation, `settings.json`, chain selection, help generation. | `common/args.*`, `util/system.*` | — |
| `dashd-node` | runtime | Node assembly: `NodeContext`, init steps (Core `AppInitMain` order), shutdown, scheduler, `NodeClock`, logging (Core `debug.log` format and categories), notifications (`-blocknotify` …), statsd, pid and lock. | `init.cpp`, `node/*`, `logging.*`, `scheduler.*`, `stats/*` | everything |
| `rdashd` | bin | Daemon `main` (daemonize, signals). | `bitcoind.cpp` | dashd-node |
| `rdash-cli` | bin | dash-cli equivalent: `-getinfo`, `-netinfo`, `-addrinfo`, `-generate`, `-rpcwait`, `-named`, `-stdin*`, `-rpcwallet`. | `bitcoin-cli.cpp` | dash-args, dash-rpc-spec |
| `rdash-wallet` | bin | Offline wallet tool: `info`, `create`, `dump`, `createfromdump`, **`import-core`**. Later `rdash-tx` parity (P1). | `bitcoin-wallet.cpp` | dashd-wallet |
| `dashcore-oracle-sys` | test | FFI to `libdash_oracle` (§4.4). Feature `oracle`; never a runtime dependency. | — | — |
| `node-fuzz` | test | cargo-fuzz targets: single-implementation and differential. | `test/fuzz/*` | pure crates, dashcore-oracle-sys |
| `node-difftest` | test | Chain replay plus StateDigest compare, RPC JSON diff, P2P transcript diff, functional-test runner glue. | — | dashd-node, dashcore-rpc |

### 2.3 Dependency graph

```
 base-sdk:  dash-types ─ dash-num ─ dash-pkc ─ dash-pow ─ dash-script ─ dash-primitives ─ dash-params ─ dash-p2p-core
                                                   │
 pure:       dash-consensus ◄──────────────────────┘
                  ▲
             dash-evo ◄── dash-llmq
                  ▲  ▲         ▲
     dash-governance  dash-coinjoin
                  ▲
 runtime:    dashd-store ─► dashd-chainstate ─► dashd-mempool ─► dashd-mining
                                 ▲   ▲               ▲
                       dashd-indexes  │               │
          dash-p2p-transport ─► dashd-p2p ◄───────────┘
                                 ▲   ▲   ▲   ▲   ▲
               dashd-sporks ─────┘   │   │   │   └── dashd-coinjoin
               dashd-quorums ────────┘   │   └────── dashd-governance
               dashd-locks (→quorums,mempool)        dashd-masternode (→quorums, p2p)
               dashd-wallet (key-wallet, key-wallet-manager, dash-coinjoin, chainstate, mempool)
 services:   dash-rpc-spec ─► dashd-rpc ; dashd-zmq ; dash-args
 assembly:   dashd-node ─► rdashd ;  rdash-cli (dash-args, dash-rpc-spec) ;  rdash-wallet
 existing (untouched API): dashcore(+bridge feature) ─ key-wallet ─ key-wallet-manager ─ dash-spv ─ *-ffi
```

Cycle breakers:
- **Validation → ChainLocks / IS / governance.** Chainstate depends on the `ChainHelper` trait object, which `dashd-node` wires to `dashd-locks` and `dashd-governance`.
- **P2P → overlays.** Overlays implement `NetHandler`, and `dashd-node` registers them.
- **Chainstate → mempool.** `remove_for_block` is called through a `MempoolSink` trait implemented by `dashd-mempool`.

### 2.4 `dash-consensus` key interfaces

```rust
pub struct ConsensusCtx<'a> {
    pub params: &'a ChainParams,
    pub height: u32,                       // height of the block being validated
    pub deployments: &'a DeploymentStates, // buried + versionbits, "active after prev" and "active at" variants
    pub bls_scheme: BlsScheme,             // legacy iff !DeploymentActiveAt(block, V19); explicit, never global
    pub median_time_past: i64,
    pub adjusted_time: i64,                // from NodeClock; only for "time too new"
}

pub trait ChainAncestry {                  // what PoW/versionbits/MTP need (Core CBlockIndex walking)
    fn header_at(&self, id: BlockId) -> &HeaderInfo;          // height, time, bits, version, hash
    fn ancestor(&self, id: BlockId, height: u32) -> BlockId;  // skip-list O(log n)
    fn prev(&self, id: BlockId) -> Option<BlockId>;
}

pub trait SignatureChecker {               // Core BaseSignatureChecker
    fn check_ecdsa(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8], flags: ScriptFlags) -> bool;
    fn check_data_sig(&self, sig: &[u8], msg: &[u8], pubkey: &[u8], flags: ScriptFlags) -> bool; // single SHA256(msg)
    fn check_lock_time(&self, n: ScriptNum) -> bool;
    fn check_sequence(&self, n: ScriptNum) -> bool;
}
pub fn verify_script(script_sig: &[u8], script_pubkey: &[u8], flags: ScriptFlags,
                     checker: &dyn SignatureChecker) -> Result<(), ScriptError>;
pub fn signature_hash(script_code: &[u8], tx: &Tx, n_in: usize, hash_type: u32) -> Hash256; // legacy, with vExtraPayload, SINGLE bug

pub trait CoinsView { fn coin(&self, op: &OutPoint) -> Option<Coin>; }
pub fn check_transaction(tx: &Tx) -> Result<(), TxValidationError>;
pub fn check_tx_inputs(tx: &Tx, view: &dyn CoinsView, spend_height: u32) -> Result<Amount, TxValidationError>;
pub fn check_block(block: &Block, params: &ChainParams, check_pow: bool, check_merkle: bool) -> Result<(), BlockValidationError>;
pub fn get_next_work_required(chain: &dyn ChainAncestry, last: BlockId, header: &BlockHeader, p: &ChainParams) -> u32;
pub fn get_block_subsidy_inner(prev_bits: u32, prev_height: u32, v20: bool, p: &ChainParams) -> i64;
```

`ScriptError` variants and names mirror `script_error.h` exactly, so `script_tests.json` expectations map one-to-one.

### 2.5 `dash-evo`: deterministic MN list data structures

Core uses immer HAMTs (`CDeterministicMNList`). We use **`imbl`** persistent structures (O(1) clone per block, structural sharing) with **sorted maps**, so iteration order is deterministic and useful:

```rust
#[derive(Clone)]
pub struct MnList {
    pub block_hash: BlockHash,
    pub height: u32,
    pub total_registered: u32,                       // nTotalRegisteredCount
    by_pro_tx: imbl::OrdMap<ProTxKey, Arc<Dmn>>,    // ProTxKey: Ord = memcmp of LE bytes (Core uint256 order)
    by_internal_id: imbl::OrdMap<u64, ProTxKey>,
    unique: imbl::HashMap<UniqueKey, (ProTxKey, u32)>,  // SerializeHash(property) -> (owner, refcount)
    payee_queue: imbl::OrdSet<PayeeKey>,             // (effective_last_paid_height, ProTxKey) over valid MNs only
}
pub struct Dmn {                                     // immutable; replaced on change
    pub pro_tx_hash: ProTxHash, pub internal_id: u64, pub collateral: OutPoint,
    pub operator_reward: u16, pub mn_type: MnType, pub state: Arc<MnState>,
    sml_hash: OnceLock<Hash256>,                     // cached SML entry hash (valid because Dmn is immutable)
}
```

- **`payee_queue`** implements `GetMNPayee`'s `CompareByLastPaid` as an ordered set. Payee selection drops from O(n) to O(log n). The comparator is exactly Core's: `h = revived > last_paid ? revived : (last_paid == 0 ? registered : last_paid)`, tie-broken by proTxHash memcmp. The EvoNode 4-in-a-row rule is checked before the queue lookup, as Core does. A **reference implementation** (a linear scan ported line by line) runs in the cross-check mode (§3.10).
- **`merkleRootMNList`**:
  - Iterate `by_pro_tx`, which is already in Core's sort order.
  - Take each `Dmn::sml_hash`, computed once per immutable `Dmn`.
  - Compute the merkle root with SIMD SHA256D64.
  - The per-block cost becomes O(changed + n·SHA256D64 of 64 bytes), versus Core's full re-serialization and hash of every entry when uncached.
- **Persistence (our format, not Core's):**
  - Per block, write a `MnListDiff` record keyed by block hash: added MNs as full records, updated MNs as Core's field-bitmask state diff (same field set and bits; this lets us reuse Core's tested semantics), and removed internal IDs.
  - Every 576 blocks, write a full snapshot in **Core's canonical `EvoSnapshot` MN-list encoding** (`evo/snapshot.{h,cpp}`, sorted by proTxHash, version 3; R4 F8). That avoids Core's HAMT-order problem (R2 G.3 Q2). It also makes our snapshot bytes and Core's `CanonicalMNListHash` directly comparable (§4.6), and gives a ready interop format for future snapshot sync.
  - Lookups use an in-memory LRU of `Arc<MnList>` (mini-snapshots every 32 blocks, as Core does).
- **Core evodb import** reads `dmn_S3`/`dmn_D4` with an order-agnostic decoder, rebuilds `MnList`s, and verifies the tip list against the CbTx `merkleRootMNList`.

### 2.6 `dash-llmq` highlights

- **Member selection** is pure: `fn quorum_members(params, base: BlockId, ctx: &dyn LlmqContext) -> Vec<Arc<Dmn>>`.
  - `LlmqContext` supplies MN lists at heights, coinbase ChainLocks for the modifier, the snapshot store, and the deployment states at the two different V20 check points (R2 A.4 pitfall 6).
  - Snapshot creation is returned as an **output** (`QuorumMembersResult { members, new_snapshot: Option<QuorumSnapshot> }`). The chainstate persists it outside undo, matching Core's "not reverted on reorg" semantics (`llmq/snapshot.cpp:302-329`).
- **The DKG session** is a sans-io state machine:

  ```rust
  pub struct DkgSession { /* members, my index, contributions, complaints, justifications, premature commitments */ }
  pub enum DkgInput { Phase(QuorumPhase), Contribution(QContrib), Complaint(..), Justification(..), PrematureCommitment(..), SimError(..) }
  pub enum DkgOutput { Send(DkgMessage), MarkBad(ProTxHash), FinalCommitment(CFinalCommitment), StoreContribution(..), Log(..) }
  impl DkgSession { pub fn step(&mut self, input: DkgInput, rng: &mut dyn RngCore, bls: &dyn BlsBackend) -> Vec<DkgOutput>; }
  ```

  `dashd-quorums` drives it from block-height phase ticks and P2P messages. Unit tests can run a whole 400-member DKG in-process (dash-pkc has an LLMQ-100 DKG corpus to cross-check against). `quorum dkgsimerror` maps to `DkgInput::SimError`.
- **Reuse.** R4 verified several existing rust-dashcore algorithms against Core: SML entry hash and MN/quorum merkle roots (15/15 vectors, 6/6 mainnet diffs), quorum scoring and modifier, rotation reconstruction from snapshots, CL/IS request IDs and quorum selection.
  - These are **lifted** from `dashcore::sml` and dash-spv's `sml_engine` into `dash-evo`/`dash-llmq`, retargeted onto base-sdk types.
  - They then get the gaps R4 lists: snapshot building and skip-list generation, pre-V19/pre-V20 rules, mode 2, swallowed `AddMN` exceptions, the collateral tie-break, and `merkleRootQuorums` over *configured* (including disabled) types.
  - dash-spv's hard-coded basic scheme, which fails the 24 stale v1 commitments, is not carried over.
- **Signing math:**
  - `select_quorum_for_signing`: non-rotated uses min over memcmp; rotated uses the `(GetUint64(3) >> (64-n-1)) & mask` bit selection.
  - `sign_hash`, request IDs as CompactSize-prefixed strings.
  - `verify_recovered_sig`, CL verification (`chain[h-8]` scan point), IS verification with the two-window retry.
  - All of these are pure functions over a `QuorumSet` provider.

### 2.7 `dash-governance` and `dash-coinjoin`

- **`dash-governance`** is small and exact.
  - `GovObjectHash` (data as a hex string, vin-like suffix, signature included, collateral excluded).
  - `GovVoteHash` (signal before outcome).
  - The trigger formatter is byte-exact: `{"event_block_height": H, ...,"type":2}` spacing.
  - Proposal validator with Core error strings.
  - `is_valid_superblock` (pre/post-V24 duplicate rule) and `best_superblock(height)` (strict `>` on absYes, map order by hash).
  - Vectors: `proposals_{valid,invalid}.json` plus oracle diff-fuzz of the hashes.
- **`dash-coinjoin`** gives the server (`dashd-coinjoin`) and the client (`dashd-wallet`) **one shared implementation** of `is_valid_in_outs`, DSTX structure checks and the V24 rebalance rules, avoiding the client/server drift Core historically suffered.

### 2.8 Storage engine choice

**Decision: RocksDB** (`rust-rocksdb`, pinned C++ build, LZ4 on upper levels, ZSTD on the bottommost level).
- One RocksDB per state domain: `chainstate/` with CFs, `llmq/`, and one per index.
- Block and undo data stay in **Core-format flat files**.

**Why, against the performance targets:**

| Criterion | RocksDB | MDBX / LMDB (B+tree, mmap) | redb (pure Rust B-tree) | Custom flat/hash files |
|---|---|---|---|---|
| Write path for UTXO flushes (random 36-byte keys, millions per flush) | LSM: sequential L0 writes, amortized multi-threaded compaction. Same shape as Core's LevelDB, so **no regression is possible by construction**, and it is faster than LevelDB (parallel compaction, pipelined writes). | COW B-tree: a random-key flush rewrites most leaf pages plus the path to the root. A 2 GB tree can need ~1–2 GB written per large flush. | slower writes than LMDB in published benchmarks | fastest if done right, but a large, risky custom engine |
| Point-read latency on cache miss | 2–5 µs (bloom + block cache) | ~1 µs (mmap) — **best** | ~LMDB | depends |
| Disk footprint (metric: never above dashd) | ≈ LevelDB, minus 10–30% with ZSTD on values; prefix compression on address-index keys | 1.5–2× LevelDB for random-hash keys (page fill 50–70%) → **risk of losing the disk metric** | similar to MDBX | small |
| RSS accounting | block cache and memtables are explicit and budgeted from `-dbcache` | DB pages show up as file-backed RSS; the metric needs care | explicit | explicit |
| Atomic multi-table commit | `WriteBatch` across column families | yes (one txn) | yes | must be built |
| Precedent | Kaspa/rusty-kaspa (UTXO model, high throughput), electrs (address index), TiKV, Solana | reth, Erigon (account model, staged sync) | — | libbitcoin v4 |
| Build cost | C++ compile (~4 min cold; sccache shared on agentbox) | small C | none | none |

Two criteria decide it:
1. **Disk footprint is a "never worse than dashd" metric.** Random-hash keys make B-trees materially larger than LSM tables.
2. **The LSM write path is structurally the same as Core's.** The node can therefore not lose on write-heavy phases (reindex, IBD flushes) while it wins elsewhere through caching and parallelism.

MDBX would win on cache-miss reads. Our layered cache plus prefetch (§3.4) makes misses rare on the hot path.

**Validation gate (M0, task `ST1`).** `dashd-store` exposes a narrow engine trait for the coins table (`get_multi`, `apply_batch`, `iter_prefix`). The bake-off replays a **real Dash mainnet UTXO churn trace**: per-block created/spent sets extracted from the BS2 oracle, about 2.5M blocks. It runs the trace through RocksDB, MDBX and Core's LevelDB (via a tiny C++ driver in the `node/oracle-shim` branch), at `-dbcache` 300 MB and 4 GB, and measures:
- total time;
- p99 flush stall;
- cache-miss read latency;
- final on-disk size;
- RssAnon / RssFile.

If MDBX beats RocksDB by >25% on time without losing disk or RSS, switch before M3. The API is narrow, so the switch is cheap. Otherwise this decision stands.

**RocksDB configuration (chainstate DB):**
- One shared LRU block cache sized from the `-dbcache` split (§3.4), with `cache_index_and_filter_blocks=true` and L0 pinning.
- 10-bit bloom filters with whole-key filtering on `coins` and `block_index`.
- `level_compaction_dynamic_level_bytes`.
- `max_background_jobs = max(2, cores/4)`.
- WAL **on**, with `sync=false` per batch and an explicit `flush_wal(sync=true)` at durability points (§3.5).
- `two_write_queues` and `unordered_write=false`, so batch order is preserved.

### 2.9 Datadir layout and on-disk formats

Base directory default: `~/.rdashcore` (Linux), `~/Library/Application Support/RDashCore` (macOS), `%APPDATA%\RDashCore` (Windows). Names are pending pasta (§7 Q3).
- Per-network subdirectories are **identical to Core's**: none for mainnet, then `testnet3/`, `devnet-<name>/`, `regtest/`.
- The directory constraint comes from Core's functional-test framework:
  - The chain cache keeps only `blocks/`, `chainstate/`, `indexes/`, `evodb/` and `llmq/` and deletes everything else (`test_framework.py:1004`).
  - `copy_datadir` copies `blocks/`, `chainstate/`, `evodb/` and `llmq/` between nodes (`util.py:491-503`).
  - So **all consensus state lives under `blocks/` and `chainstate/`, all LLMQ runtime state under `llmq/`, and indexes under `indexes/`**. Flat files are rebuilt, as in Core.

```
<net>/
  blocks/blk00000.dat …      Core format, byte-identical (magic ‖ u32 size ‖ block); 128 MiB max, 16 MiB prealloc (1 MiB rev); -tinyblk honoured
  blocks/rev00000.dat …      Core format (magic ‖ size ‖ CBlockUndo ‖ SHA256d(prevhash ‖ undo))
  chainstate/                RocksDB "chainstate" (column families below) — the only consensus DB
  llmq/                      RocksDB "llmq": recsig_*, is_*, dkg_*, quorum_vvec, quorum_skshare (secret; file mode 0600)
  indexes/txindex/ indexes/addressindex/ indexes/spentindex/ indexes/timestampindex/ indexes/coinstats/   one RocksDB each
  indexes/blockfilter/basic/{db/, fltr00000.dat …}   Core-format filter files + RocksDB index
  wallets/<name>/wallet.sqlite3      node wallet (own schema, §2.19)
  peers.dat anchors.dat banlist.json mempool.dat fee_estimates.dat sporks.dat netfulfilled.dat   Core-compatible formats
  mncache.dat governance.dat         CFlatDB envelope, own version strings (Core treats a mismatch as "empty, rebuild")
  settings.json .cookie .lock rdashd.pid debug.log
```

Column families in `chainstate/`:

| CF | Key | Value | Written |
|---|---|---|---|
| `meta` | ASCII tags: `tip`, `flush_state`, `version`, `reindexing`, `pruned_files`, `last_file`, `node_build` | per tag | each flush |
| `coins` | `txid[32] ‖ VARINT(vout)` (Core's key without the `'C'`) | Core `Coin` bytes: VARINT(h·2+cb) ‖ CompressAmount ‖ ScriptCompression | flush |
| `block_index` | `hash[32]` | `BlockIndexRecord v1`: VARINT height, VARINT status (Core bits incl. 128 CONFLICT_CHAINLOCK), VARINT nTx, file/data/undo positions if present, the 80-byte header | flush (dirty entries) |
| `block_files` | `u32 BE file` | Core `CBlockFileInfo` (VARINT×7) | flush |
| `mn_diff` | `hash[32]` | `MnListDiffRecord` (§2.5) | per block (in batch) |
| `mn_snapshot` | `hash[32]` | `MnListSnapshot` (sorted by proTxHash) | every 576 blocks |
| `credit_pool` | `hash[32]` | `CreditPool` snapshot | every 576 blocks |
| `mnhf` | `hash[32]` | `map<u8 bit, u32 height>` | per block after V20 |
| `llmq_mined` | `u8 type ‖ quorumHash[32]` | `CFinalCommitment` bytes ‖ mined block hash | per block (in batch) |
| `llmq_mined_h` | `u8 type ‖ i16 qIdx ‖ BE32(!minedHeight)` | quorum base height ‖ base hash | per block |
| `llmq_snapshot` | `u8 type ‖ cycleBaseHash[32]` | `CQuorumSnapshot` bytes | at cycle bases; **never deleted on disconnect** (Core semantics) |

Keeping Core's encodings for coins, undo and block-file metadata buys three things:
1. **byte-level differential checks** (coins can be compared entry by entry with a deobfuscated dashd chainstate);
2. cheap import;
3. MuHash parity with Core's coinstatsindex.

### 2.10 `dashd-chainstate`

**Block index.**
- An append-only arena: `Vec<BlockIndexEntry>` in chunks of 64Ki, never moved.
- The hash lookup is a `hashbrown::HashTable<BlockId(u32)>` that hashes *through* the arena. Each block hash is stored once (in the entry) and the table costs about 5 B per entry.
- Immutable fields:
  - hash;
  - header without `hashPrevBlock` (derived from `parent`): version, merkle root, time, bits, nonce;
  - height, parent, skip;
  - chain work (`u128` with a debug overflow assert; Dash mainnet work is about 2^80).
- Mutable fields are atomics written only by the validation thread: status, `nTx`, `chain_tx`, file positions, sequence id, time-max.
- Readers on other threads therefore need no lock.
- About 147 B per block, versus about 220 B for Core's `CBlockIndex` plus its `std::unordered_map` node. That is about a third less: roughly 180–200 MB saved at Dash's 2.6M headers (§3.8).

**The validation thread** (`ChainstateManager::run`) consumes `ChainCommand`s from a bounded channel:

```rust
pub enum ChainCommand {
    ProcessHeaders { headers: Vec<BlockHeader>, from: Option<PeerId>, reply: oneshot::Sender<HeadersResult> },
    ProcessBlock   { block: Arc<PrevalidatedBlock>, force: bool, from: Option<PeerId>, reply: Option<oneshot::Sender<BlockResult>> },
    InvalidateBlock(BlockHash, Reply), ReconsiderBlock(BlockHash, Reply), PreciousBlock(BlockHash, Reply),
    EnforceChainLock(ChainLockSig),          // from dashd-locks: Core EnforceBlock + ActivateBestChain
    TestBlockValidity(Arc<Block>, Reply),     // GBT proposal / CreateNewBlock check (evo txn rolled back)
    Flush(FlushMode), LoadMempoolDone, Shutdown,
}
```

**Connect order.** This is a line-by-line port of Core `ConnectBlock`, R2 A.1 §1, steps 1–16:
1. BLS scheme context.
2. `check_block`.
3. Conflicting-CL check through `ChainHelper`.
4. Evo best-block equality. This is free here: one atomic DB, one in-memory overlay.
5. assumevalid evaluation. **Dash scope**: it also skips CbTx roots, best-CL, credit-pool, ProTx signatures and QC BLS signatures, exactly like Core's `fScriptChecks` fan-out.
6. BIP30 and `bad-sb-start`.
7. Script flags.
8. Subsidy.
9. `process_special_txs_in_block` **before** UTXO updates.
10. The per-tx loop: `check_tx_inputs` → shared-collateral spends (V24 `ActiveAt`) → fees → sequence locks → sigops → script checks queued → `update_coins`.
11. Wait for scripts.
12. IS conflicts (outside IBD) through `ChainHelper`.
13. Block value and payee via `dash-evo` with the superblock oracle.
14. Undo, status, best block.

**Quirk register.** R4 §1.15 (34 entries) is **normative**. Each entry has a named regression test (`quirk_NN_<slug>`) in the crate that implements it, and the PR description of every ★ task lists the quirks it touches. Examples affecting this section:
- old-budget windows accept unbounded coinbase values under `NoCheck` (F3);
- `merkleRootQuorums` iterates configured types including disabled ones (F5);
- MN payments are unvalidated before DIP3 enforcement, and an all-banned list accepts any payee while still crediting the credit pool (F6);
- CbTx v2 is required from DIP0008 **+1**.

During IBD and reindex `ChainHelper::is_mn_sync_finished()` is false, so superblock payees take Core's `NoCheck` path exactly as dashd does (R4 §1.10).

**Overlay-based atomicity.** Each block is connected into a `BlockOverlay`: a child coins map plus `EvoDelta` (MN diff, new commitments, snapshot outputs, credit pool, MNHF signals, BLS scheme flip). On success the overlay merges into the active layer. On failure it is dropped. This replaces Core's `CCoinsViewCache view(&CoinsTip())` plus `CEvoDBScopedCommitter` pair with one mechanism.

**Reorg.**
- `activate_best_chain` follows Core: `FindMostWorkChain` skips FAILED and CONFLICT_CHAINLOCK, at most 32 blocks are connected per step, and the `disconnectpool` re-adds transactions to the mempool.
- Disconnect order matches `DisconnectBlock`: `undo_special_txs_in_block` runs before the UTXO undo. The MN diff is read but not erased (Core); the credit pool is derived; the BLS scheme flips back.

**State-dependent validity: the `ChainHelper` trait** (Core `CChainstateHelper`):

```rust
pub trait ChainHelper: Send + Sync {
    fn has_conflicting_chainlock(&self, height: u32, hash: &BlockHash) -> bool;   // ArcSwap snapshot of best CL
    fn has_chainlock(&self, height: u32, hash: &BlockHash) -> bool;
    fn best_chainlock_height(&self) -> Option<u32>;
    fn conflicting_islock(&self, tx: &Tx) -> Option<Arc<InstantSendLock>>;     // only consulted outside IBD
    fn remove_conflicting_islock(&self, tx: &Tx);                               // chainlocked block overrides islock
    fn is_mn_sync_finished(&self) -> bool;                                       // superblock NoCheck path
    fn superblock(&self, height: u32) -> SuperblockState;                        // NotTriggered | Triggered(Arc<Superblock>) | GovernanceInvalid
}
```

- The node wires real implementations.
- Replay and differential tools use a `ReplayChainHelper` driven by **recorded** dashd decisions. CL events and superblock triggers per height are exported by the oracle patch, which makes state-dependent validity reproducible offline (§4.6).

**Events.** `ChainEvent` is delivered in order on a single dispatcher thread, like Core's validation-interface queue. It includes `BlockConnected { block, height, undo, mn_list }`, `BlockDisconnected`, `UpdatedBlockTip`, `NotifyHeaderTip`, `MnListChanged { old, new, diff }`, `ChainLockAccepted`, and mempool add/remove (forwarded from the mempool). Subscribers include indexes, wallet, ZMQ, notifications, quorums, locks, governance, masternode and P2P announcements. The `syncwithvalidationinterfacequeue` RPC waits for dispatcher idleness, as Core does.

**ChainView publication.** `ArcSwap<ChainView>` is updated after each tip change, and at most every 100 ms during IBD:

```rust
pub struct ChainView {
    pub tip: BlockId, pub best_header: BlockId, pub height: u32,
    pub active: imbl::Vector<BlockId>,           // O(1) clone; height → id
    pub index: Arc<BlockIndex>,                  // shared arena (lock-free reads)
    pub mn_list: Arc<MnList>,                    // at tip
    pub ibd: bool, pub mtp: i64,
}
```

**Crash recovery.** The DB always holds a consistent (coins, block index, evo) state at its `tip` marker, because one `WriteBatch` covers everything. At startup:
1. Load the index.
2. If any `HAVE_DATA` blocks above the DB tip are on the active chain, replay them (`RollforwardBlock`-equivalent with signature checks off, as Core does).
3. There is no "evodb mismatch → reindex" failure mode.

**Import from Core.** Two modes, both in `rdash-wallet`-style tooling plus `rdashd -importcoredatadir=<dir> -importmode=…`:
- `safe`: hardlink or copy `blk*/rev*`, convert `blocks/index` (LevelDB reader), then `-reindex-chainstate`.
- `fast`: also transcode `chainstate/` (deobfuscated) and `evodb/` (dmn_S3/D4, cpm_S, mnhf_s2, q_mc*, llmq_S), and optionally `llmq/quorumdb`, which keeps a migrating MN signing in its active quorums, plus `llmq/isdb`, `sporks.dat`, `peers.dat`, `banlist.json` and `mempool.dat`.
- Fast import is accepted only after these checks pass:
  - the UTXO MuHash equals `gettxoutsetinfo muhash`, if supplied, or is recomputed and logged;
  - the tip MN list SML root equals the tip CbTx `merkleRootMNList`;
  - the quorum root equals `merkleRootQuorums`.

### 2.11 Block files

- **`BlockStore`** keeps Core's `FlatFileSeq` semantics: the same file numbering, `nFile`/`nPos` meaning, `CBlockFileInfo` and prune behaviour.
- **Writes happen on the DB-writer thread.** `accept_block` enqueues the block's **canonical (Core re-serialized) bytes**. These are the peer's bytes on the fast path (§1.4), and freshly serialized otherwise, matching what dashd stores. It gets the position back synchronously (pre-reserved offsets), so the block index records `HAVE_DATA` immediately while the bytes land asynchronously. A durability barrier before each chainstate flush ensures nothing in the DB points at unwritten data.
- **Reads use `pread`** (no mmap, to keep RssFile honest) behind an LRU of recently connected `Arc<Block>`s, serving compact blocks, getdata and the wallet.
- **`-loadblock` and `-reindex`** use Core's scanner semantics: magic byte scan, `80 ≤ size ≤ MaxBlockSize`, and an orphans map.

### 2.12 `dashd-mempool`

- **Entry** `MempoolEntry { tx: TxRef, fee, vsize = max(size, sigops*20), time, height, sigops, ancestors/descendants (count, size, fees), lock_points, special: Option<SpecialRefs> }`. Indexes:
  - txid;
  - instance hash (AU v2);
  - `next_tx` (outpoint → spender);
  - ancestor-score set (mining);
  - descendant-score set (eviction);
  - time (expiry, skipping IS-locked transactions);
  - the Dash maps: `pro_tx_refs`, `pro_tx_addresses`, `pro_tx_pubkey_ids`, `pro_tx_bls_pubkey_hashes` (both schemes), `pro_tx_platform_node_ids`, `pro_tx_collaterals`, `asset_unlock_expiry`, `asset_unlock_instances`, `asset_unlock_indexes`, `pending_asset_unlock_amount`;
  - the mempool address and spent indexes (enabled with `-addressindex`/`-spentindex`).
- **Two-phase ATMP:**
  1. *pre-checks* (stateless and policy) on the caller's task;
  2. *coins fetch* under a read view (`CoinsReadGuard`, held for microseconds);
  3. *script and special-tx signature checks* on the script pool, **with no lock held**;
  4. *commit* under the mempool write lock, which re-validates the tip ID and conflicts and retries once on tip change.

  Reject reasons and their order match `MemPoolAccept::PreChecks` (R2 A.1 §5). There is no RBF (`txn-mempool-conflict`). IS-lock waiting evicts conflicts instead of rejecting.
- **Package index for mining.** An ancestor-feerate ordered index is maintained incrementally on insert and remove, so `getblocktemplate` does not re-sort the mempool (§3.7).

### 2.13 P2P: `dash-p2p-transport` + `dashd-p2p`

**Stance on existing work.**
- **#264 (BIP324):** its `Transport` trait, V1/V2 implementations, handshake and Dash short IDs are **lifted into `dash-p2p-transport`** and generalised: responder role, inbound v1 detection by the 16-byte `magic‖"version\0…"` prefix, short-ID send gating on `peer_version ≥ 70235` (≥70240 for `platformban`), long encoding before `version` arrives, and v1 reconnect on early disconnect.
  - The HKDF salt must be `"bitcoin_v2_shared_secret" ‖ Dash magic` (`Core:bip324.cpp:38`). An M2 interop test against dashd proves it. If the `bip324` crate cannot take a custom magic, vendor a patched copy.
  - dash-spv keeps its own copy until #264 merges upstream. Afterwards a follow-up can switch dash-spv to the shared crate.
- **#902 (dash-spv manager rewrite):** the node neither depends on it nor touches `dash-spv/src/network`. dash-spv's manager is outbound-only and SPV-shaped; a node needs Core's connection and relay semantics. Avoiding that directory also avoids merge conflicts with an active upstream author.

**Runtime shape.**
- **Per-connection tokio tasks.** The reader decodes frames through the transport and turns messages into `P2pMsg` with `ProtoCtx`. The writer drains a bounded send queue, with backpressure equivalent to `-maxsendbuffer`.
- **Per-peer processing state machine** (`PeerState`, the union of Core's `Peer` and `CNodeState`) runs in the peer's task. It handles handshake, ping, addr, inv bookkeeping, getdata serving from `ChainView`, block store and mempool snapshots, and dispatch to `NetHandler`s.
- **Global coordinators** are actors:
  - `BlockSync` (header sync peer selection, block download window, compact blocks, stalling);
  - `ObjectRequests` (Core `m_object_request` / TxRequestTracker generalised to all inv types);
  - `AddrMan`;
  - `BanMan`;
  - `ConnMan`.
- **`send_messages(peer)`** runs on a per-peer 100 ms tick, which matches Core's message-handler cadence. It emits in **Core's order**: ping → addr → getheaders/getheaders2 → block announcements (cmpctblock/headers2/headers/inv) → inv trickle → other invs (immediate) → getdata. Outgoing sequences therefore match dashd's.

**Connection management** (Core defaults, all configurable):
- `-maxconnections=125`; 8 full-relay outbound; 2 block-relay-only plus up to 2 anchors (`anchors.dat`); 1 feeler every 2 min (Poisson); 8 addnode on a separate semaphore.
- Extra peers on stale tip (150 s check, 450 s threshold) and extra block-relay peers every 5 min.
- DNS seeds (`x1.` prefix; 11 s when addrman has <1000 entries, else 5 min; 3 at a time), fixed seeds after 60 s.
- `-connect`/`-seednode`/`-addnode` semantics; distinct netgroups; v2 only if both sides advertise bit 12.
- **Inbound accept:** permissions → ban/discourage → `SelectNodeToEvict` (Core algorithm plus the masternode 1 s MNAUTH protection) → a masternode refuses inbound until synced.
- Disconnect linger: about 100 ms, then `shutdown(SHUT_WR)`.
- **Masternode connections:** `ConnMan::set_masternode_quorum_nodes`, `set_quorum_relay_members`, `add_pending_masternode`, `add_pending_probe`. A 1 s cadence (100 ms after a success) and a 60 s retry backoff. Connections are typed OUTBOUND_FULL_RELAY plus MN flags and bypass the outbound semaphore.

**Fingerprint parity: the explicit requirement list** (from R2 A.2 and the P2P survey). These are implemented as data, not scattered literals: one `fingerprint.rs` table.

| Surface | Required behaviour |
|---|---|
| `version` | written with init version 209; fields `nVersion` from the network's `ProtocolProfile` (70240 mainnet/v23.1.x, 70242 testnet/v24), services, unadjusted time, addr_recv (zeroed if non-routable/proxied/non-v1), addr_from = our services + `[::]:0`, SipHash nonce, UA, `start_height` = active-chain height, relay = `!RejectIncomingTxs`, **32-byte mnauth challenge on every connection**, **fMasternode byte**. Lenient parse; <70221 → disconnect without reject |
| services | 0x1C05 by default (NETWORK, BLOOM, NETWORK_LIMITED, HEADERS_COMPRESSED, P2P_V2); MN 0x1C45 (+COMPACT_FILTERS) |
| post-handshake order | inbound: `version`, `sendaddrv2` (≥70223), `verack`. On verack: [`mnauth`], **`sendheaders2` xor `sendheaders`** (by peer bit 11), `sendcmpct(0,1)` if CanRelay, `senddsq(1)` unless blocks-only, [`qwatch`], then ping. Outbound also sends `getaddr` (not block-relay) |
| absent messages | never send `feefilter`, `wtxidrelay`, `reject`, `sendtxrcncl` (default off); unknown commands are ignored without penalty |
| Dash sync chatter | `getsporks` once per peer address, observed +0.5–2 s after verack (tick every 6 s, while not synced); `govsync` about 8 s later (R4 F11); `mempool` to outbound non-block-relay peers when the BLOCKCHAIN asset completes; `govsync(0, empty bloom)` one peer per tick; vote syncs ≤3 peers per object; disconnect after `full-sync` |
| relay timing | tx trickle Poisson: inbound shared 5 s, outbound 2 s, 1 s to verified MN, none MN↔MN/noban. Non-tx invs on every pass; cap `140·MaxBlockSize/1e6 + (q/1000)·5` ≤ 1000; addr relay 0.1/s bucket of 1000; addr broadcast Poisson 30 s; local addr 24 h |
| limits | headers 2000 / headers2 8000 (≥70235), locator ≤101 → disconnect, inv/getdata ≤50000 (+20), addr ≤1000 (+20), message ≤3 MiB, getaddr once per inbound connection from a 21–27 h cache, 23% / 1000 cap; empty headers below min chain work (reply type follows the **service bit**, not the request type) |
| block download | window 1024, 16 per peer, stall 2→64 s (×0.85 decay), timeout 150 s·(1+0.5·N), headers sync timeout 15 min + 1 ms/header, 2 min getheaders spacing, unknown block inv → getheaders only |
| compact blocks | v1 only, txid short IDs (instance hash for AU v2), up to 3 HB peers, 3 in flight per block, depth ≤5 / ≤10 rules |
| penalties | additive scores, threshold 100. Table in `misbehavior.rs` copied from Core (100: invalid block/header, bad filter, unsolicited mnlistdiff/qrinfo, invalid spork, bad MNAUTH; 20: oversize messages, non-continuous headers, repeat govsync; 10: unrequested/invalid clsig, invalid DSTX, DKG from unverified peer; 1: getmnlistd/getqrinfo failures). Bad checksum or bad command → drop without disconnect. Blocks-only violations on BRO/feeler → disconnect |
| serving quirks | `merkleblock` followed by matched `tx` and **`isdlock`**; `mempool` reply adds isdlock invs and the best clsig inv; govsync replies' invs go to all peers (`RelayInv` quirk); DSTX downgraded to MSG_TX for peers <70241 when unbalanced |
| BIP324 | garbage uniform 0..4095, empty version packet, no decoys, short IDs as above |

**Verification:** the P2P transcript-diff suite (§4.9) runs every scenario against dashd and rdashd and diffs normalized transcripts. Any P2P PR must keep it green.

**Protocol profile per network.** "Indistinguishable from dashd" means indistinguishable from the **pinned dashd release that network actually runs** (R4 §0.2): today mainnet v23.1.x (protocol 70240) and testnet v24.0.0 (70242). `fingerprint.rs` is therefore keyed by a `ProtocolProfile { protocol_version, ua_version, feature_gates }` selected per network. It gates `MSG_ASSET_UNLOCK`, the COINJOIN_REBALANCE `dsa` field, platformban short ID 168 and the other version-gated behaviours. Consensus rules come from `ChainParams` deployments, not from the profile. Profiles move when the pinned release moves (D42).

**User agent.** Pending pasta (§7 Q1). The implementation supports both `/Dash Core:<ver>/` (indistinguishable) and an honest UA, plus `-uacomment` as in Core. `getnetworkinfo` and `getpeerinfo` report consistently.

**Serving data.**
- **Headers.** getheaders/getheaders2 are served from `ChainView` + `BlockIndex` (headers in memory), and from no other source.
- **Blocks.** Blocks come from the `BlockStore` LRU or `pread`. MSG_FILTERED_BLOCK uses the peer's bloom filter plus Dash special-tx element matching.
- **Overlay data.** mnlistdiff and qrinfo are built from `MnListProvider` with the `dash-evo`/`dash-llmq` builders, including legacy construction for peers below 70236. cfilters, cfheaders and cfcheckpt come from `dashd-indexes`.
- **`NetHandler`s** answer overlay getdata (spork, govobj and votes, qfcommit, qsigrec, clsig, isdlock, dsq, platformban, DKG messages).

**`NetHandler`** (mirrors Core `init.cpp:2208-2281`):

```rust
pub trait NetHandler: Send + Sync + 'static {
    fn commands(&self) -> &'static [Command];
    fn inv_types(&self) -> &'static [InvType];
    fn process_message(&self, peer: &PeerCtx, msg: &P2pMsg) -> MsgOutcome;   // never blocks; heavy work → own worker
    fn already_have(&self, inv: &Inv) -> bool;
    fn process_getdata(&self, peer: &PeerCtx, inv: &Inv) -> Option<P2pMsg>;
    fn on_connected(&self, _peer: &PeerCtx) {}
    fn on_disconnected(&self, _peer: PeerId) {}
    fn on_send_tick(&self, _peer: &PeerCtx, _out: &mut Outbox) {}            // Dash overlays that push per-peer data
}
pub struct MsgOutcome { pub misbehavior: Option<(u32, &'static str)>, pub relay: Vec<Inv>,
                        pub relay_filtered: Vec<(Inv, Txid)>, pub erase_requests: Vec<Inv>, pub push: Vec<(PeerId, P2pMsg)> }
```

### 2.14 Dash overlay runtimes

| Crate | State and threads | Core behaviour preserved | Node-level improvements |
|---|---|---|---|
| `dashd-sporks` | `sporks.dat`; reacts to `getsporks` and `spork`; `sporkupdate` signing with `-sporkkey` | mainnet ignores the spork table (hard-coded values; SPORK_21=1); testnet signs the hash, other networks signmessage; `nTimeSigned ≤ now+2h`; invalid → 100 | none needed |
| `dashd-masternode` | `ActiveMasternode` state machine (WAITING_FOR_PROTX … READY), MNAUTH, `MnSync` (BLOCKCHAIN → GOVERNANCE → FINISHED, 6 s tick, 30 s timeout, 900 s reset), `mncache.dat`, `netfulfilled.dat`, platformban validation and relay, MN connection maintenance (1 min) | MNAUTH sign-hash tuple (operatorPubKey, peer challenge, isInbound, our version); dedup via `DeterministicOutboundConnection`; ≤3 verified inbound legs per proTx; probe semantics; `qsendrecsigs` to relay members | none beyond the parity work |
| `dashd-quorums` | Quorum manager (Arc caches; warm pubkey shares on build, as Core does). Signing manager (recsig DB, conflict detection, listener fan-out). Sig-shares manager: per-session state, batching, recovery member ordering and backoff. DKG manager plus one async task per (type, quorumIndex) instead of Core's thread per handler. Data recovery. EHF signer | DKG phase clock `(tip-qIdx)%dkgInterval`, per-phase sleep `myIdx·((phaseBlocks-1)·spacing·1000/size)·factor`, intake rules and penalties, commitment construction, recsig DB key semantics and age-out, `allowDiffMsgHashSigning` for Platform | Event-driven verification (no 100 ms poll); **Pippenger MSM in G1/G2** for vvec evaluation and Lagrange recovery; batch verification on the BLS pool (`-parbls` default = cores/2, vs Core's `clamp(hw/2,1,4)` BLS workers). The **send cadence of sig-share announcements matches Core's 100 ms batching by default** (fingerprint); `-llmqfastsend` (off by default) is available for latency benchmarks |
| `dashd-locks` | `ChainLocks` (best CL in `ArcSwap`; coinbase-CL ingestion at startup); CL signer; `InstantSendManager` + `llmq/` IS tables + IS signer; implements the CL/IS parts of `ChainHelper` | Signing conditions: synced, SPORK_19 value 0, tip not signed, IS-safety of the last 5 blocks (or 10-minute first-seen); `EnforceBestChainLock` cadence 5 s; IS two-window verification; `CheckCanLock`; retroactive signing; conflict resolution with ChainLocks | CL enforcement also triggers immediately on a new CL (no 5 s wait), which is only faster |
| `dashd-governance` | Objects, votes and orphans (1M caps), postponed objects, rate buffers, `governance.dat`, trigger store, `govsync` handler, governance signer (MN) | Every validation rule and hash via `dash-governance`; fulfilled-request limits; `ssc` counts; relay flush every 5 s (1 s on mockable chains) | Vote signature verification batched on the BLS pool |
| `dashd-coinjoin` | Server session state machine (from `dash-coinjoin`), DSTX manager (24-block expiry, CL purge), DSQ modes (INV ≥70234 / ALL) | Timeouts 30 s / 15 s, BIP69 final ordering, `ChargeFees` and `ChargeRandomFees` probabilities, +0.1 DASH priority delta, ≤5 DSTX per MN counter | none needed |

### 2.15 `dashd-indexes`

- Each index is an async `BaseIndex` with its own RocksDB, a best-block locator, and a sync thread that catches up from block files and then follows `ChainEvent`s. This is Core semantics; toggling an index needs no reindex.
- **txindex** (default **on**, as in Dash).
  - Key: `txid[0..8]`. Value: a list of packed `(u32 file, u32 block_pos, u32 tx_offset)`.
  - Lookups read the transaction and compare the full txid; collisions are rare (≈1e-4 for any collision over 6e7 txids) and handled by the list.
  - About 3× smaller than Core's `txid[32] → CDiskTxPos` (§3.8). RPC behaviour is identical.
- **addressindex, spentindex, timestampindex.** Core key layouts: big-endian height and position, so iteration order and RPC result order match. RocksDB prefix compression shrinks the address-prefixed keys.
- **blockfilterindex.** Dash-extended BIP158 elements (`evo/specialtx_filter.cpp`), Core `fltr*.dat` file format, version `'V'=2` semantics, siphash keys from the X11 block hash. Served over BIP157 when `-peerblockfilters` is set (auto-on for masternodes).
- **coinstatsindex.** MuHash3072 per height, so `gettxoutsetinfo muhash <height>` gives **the** UTXO-equality oracle (§4.6). Core's `hash_serialized_2` (with its precedence bug and LevelDB-order dependence) is also implemented, for RPC parity.

### 2.16 RPC, REST, ZMQ

**`dash-rpc-spec`** is the single source for:
- method names, including Dash two-word sub-commands (`"protx register"`);
- argument specs (type, optional/default, `a|b` aliases, `skip_type_check` string-or-number quirks);
- categories, hidden flags and wallet gating;
- **Core's help text verbatim**, so `help` matches and `rpc_help.py`-style tests pass;
- the **dash-cli conversion table** (which positional and named params are JSON-parsed);
- whitelist names (`method + params[0]`, no space).

**`dashd-rpc`:**
- **HTTP.** hyper 1.x, POST only. `-rpcthreads` concurrent handlers (default 4) and `-rpcworkqueue` queue depth (default 16; overflow → HTTP 503 "Work queue depth exceeded"). A separate `-rpcexternaluser` queue. `-rpcservertimeout`. Max header 8192.
- **Auth.** Cookie `__cookie__:<64hex>` (temp file plus rename; deleted on shutdown), `-rpcauth` HMAC-SHA256, `-rpcuser`/`-rpcpassword`. 401 after a 250 ms sleep, with `WWW-Authenticate`.
- **Dispatch.**
  - JSON-RPC 1.x replies `{result, error, id}`; no 2.0.
  - Batches.
  - `/wallet/<urlencoded>` endpoints.
  - Sub-command squashing (`method + " " + params[0]`).
  - Named arguments including `args`.
  - Warmup returns `-28` until init completes.
  - Error codes from `rpc/protocol.h`.
  - HTTP status mapping: -32600 → 400, -32601 → 404, others → 500.
- **JSON output.**
  - `serde_json` with `preserve_order` and `arbitrary_precision`.
  - **Amounts are emitted as fixed 8-decimal numbers** (`1.00000000`), as Core's `ValueFromAmount` does.
  - Field order follows Core's construction order. Differential RPC tests compare parsed JSON exactly, numbers included.
- **Handlers** are organised by category modules (`blockchain`, `mempool`, `mining`, `net`, `node`, `rawtx`, `util`, `evo`, `quorums`, `governance`, `masternode`, `coinjoin`, `wallet` (in `dashd-wallet`), `zmq`, `hidden`).
  - All test-only and hidden RPCs the functional framework needs are included: `setmocktime`, `mockscheduler`, `syncwithvalidationinterfacequeue`, `generate*`, `invalidateblock`/`reconsiderblock`, `waitfor*`, `addconnection`, `addpeeraddress`, `sendmsgtopeer`, `setmnthreadactive`, `mnauth`, `echo*`, `quorum dkgsimerror`, `evodb verify`/`repair`, `dumptxoutset`, `getrawaddrman`.
  - `-rpcdoccheck` is accepted, and checks the spec against handler output shapes in debug builds.
- **REST** (`-rest`): every Core endpoint (`tx`, `block`, `block/notxdetails`, `blockfilter`, `blockfilterheaders`, `headers`, `chaininfo`, `deploymentinfo`, `mempool/{info,contents}`, `getutxos` with BIP64 binary, `blockhashbyheight`), with bin/hex/json formats and 503 during warmup.

**`dashd-zmq`:** the 19 topics with Core framing (`[topic][body][LE u32 seq]`), per-topic HWM and display-order hashes. It uses `libzmq` via the `zmq` crate for exact HWM and reconnect semantics with existing consumers (Insight, Tenderdash, Platform).

### 2.17 Config, datadir, init, clock, logging

- **`dash-args`** is a faithful ArgsManager port:
  - sections `[main] [test] [devnet] [regtest]`;
  - NETWORK_ONLY options ignored with a warning off-main;
  - non-recursive `includeconf` (config file only);
  - `-noX` negation and `DISALLOW_NEGATION`/`DISALLOW_ELISION` flags;
  - `settings.json` read/write (`-nosettings`);
  - one chain selector only (`-regtest`/`-testnet`/`-devnet=<name>`/`-chain=`).
- **Every Core option name is recognized.** Implemented options behave as in Core. Options we don't implement yet fail loudly at startup with "not supported by rdashd yet"; none is ever silently ignored. The functional framework passes many options (`-tinyblk`, `-unsafesqlitesync`, `-par=2`, `-maxsigcachesize=1`, `-parbls=2`, `-debugexclude=leveldb`, …), so the accepted-option list is part of the M5 gate.
- **Init** follows Core's `AppInitMain` step order:
  1. datadir lock (`.lock`), pid;
  2. logging;
  3. param interactions (masternode soft-sets: `-disablewallet`, `-peerblockfilters`, `-blockfilterindex=basic`, txindex required, maxconnections ≥125);
  4. load block index and verify DB (`-checkblocks=6`, `-checklevel=3`);
  5. replay;
  6. import (`-loadblock`, `-reindex`);
  7. load mempool;
  8. start P2P;
  9. start indexes;
  10. RPC leaves warmup.

  Shutdown order is the reverse. **Shutdown must leave stderr empty** (functional-test requirement).
- **`NodeClock`.** Every subsystem gets time through it. `setmocktime` and `-mocktime` freeze it; `mockscheduler(delta)` advances the scheduler. Poisson timers draw from a per-node RNG.
- **Logging.** `tracing` with a Core-format `debug.log` writer: ISO-8601 timestamps, `-logtimemicros`, `-logthreadnames`, `-logsourcelocations`, `-loglevel`, and categories with Core's names (`net`, `mempool`, …, `chainlocks`, `llmq`, `llmq-dkg`, `llmq-sigs`, `mnsync`, `gobject`, `coinjoin`, `spork`, `ehf`, `creditpool`).
  - **Log lines asserted by functional tests (`assert_debug_log`) are reproduced verbatim.** A script extracts every asserted fragment from `test/functional` into `dashd-node/tests/asserted_log_lines.txt`, and each fragment maps to its emitting call site.
- **Notifications and stats.** `-blocknotify`, `-chainlocknotify`, `-instantsendnotify`, `-walletnotify`, `-alertnotify`, `-startupnotify`, `-shutdownnotify`; statsd with Core metric names (P2).

### 2.18 Binaries

- **`rdashd`.** `-daemon`/`-daemonwait`, signal handling (SIGTERM/SIGINT shutdown; SIGHUP reopens the log), `-version`. The same `main` serves functional tests: point the framework's `DASHD=…/rdashd` at it.
- **`rdash-cli`.** Argument parsing identical to dash-cli, driven by the conversion table from `dash-rpc-spec`.
  - Supports `-getinfo` (batches `getnetworkinfo`, `getblockchaininfo`, `getwalletinfo`, `getbalances`; adds the `mn_*` connection counts and `coinjoin_balance`), `-netinfo [0-4]`, `-addrinfo`, `-generate`, `-rpcwait`/`-rpcwaittimeout`, `-stdin*`, `-named`, `-rpcwallet`, `-color`.
  - A **generic** JSON-RPC client (not the typed `dashcore-rpc` client), so every server method works on day one.
  - Works against both rdashd and dashd. This is checked in tests, and it makes it a drop-in dash-cli for operators.
- **`rdash-wallet`.**
  - `info`, `create`, `dump`/`createfromdump` (Core's `BITCOIN_CORE_WALLET_DUMP,1` format);
  - **`import-core`**: Core `wallet.dat` (BDB 4.8 or SQLite), `dumpwallet` text, or a `dash-wallet dump` file → node wallet;
  - `import-mnemonic` (with Core quirks: no NFKD, XOR checksum acceptance flag).

### 2.19 Wallet integration (extend, don't replace)

**What stays.**
- `key-wallet` remains the HD/account/derivation/coin-selection/tx-building engine.
- `key-wallet-manager::WalletManager` remains the multi-wallet container.
- Mobile, desktop and Platform keep consuming them unchanged.

**What `key-wallet` gains.** These are separate PRs, each useful to mobile and desktop too; R5 lists them as gaps G-2..G-9.
1. **BIP39 passphrase end to end** (G-4), plus a `MnemonicCompat::DashCore` mode (no NFKD, weakened checksum acceptance) for imports only.
2. **Imported keys, watch-only scripts and seedless wallets** (G-3) as new account types.
3. **Encryption at rest** (G-2). The node keeps secrets encrypted with argon2id + XChaCha20-Poly1305 under a master key; Core's crypter (SHA512-iterated KDF, AES-256-CBC) is implemented **read-only**, for import.
4. P2SH/multisig signing, PSBT (Dash: no witness fields), descriptor parse/eval for `pk`, `pkh`, `sh`, `multi`, `sortedmulti`, `combo`, `addr` and `raw` (G-8).
5. ProTx builders for all types including v3, shared types 10–12, BLS operator signing and EdDSA platform keys (G-5).
6. Governance object prepare and vote signing (G-6).
7. CoinJoin client via `dash-coinjoin` (G-7).
8. Message signing.
9. Fee estimation hook (the node supplies estimates).

**`WalletInterface` changes** in key-wallet-manager. These are additive and default-implemented, so dash-spv compiles unchanged:

```rust
async fn process_block_disconnected(&mut self, block: &Block, height: CoreBlockHeight) -> BlockDisconnectResult { Default::default() } // #854
async fn process_mempool_removal(&mut self, txid: &Txid, reason: MempoolRemovalReason) {}
fn relevance_filter(&self) -> RelevanceFilter { RelevanceFilter::from_scripts(self.monitored_script_pubkeys(), self.watched_outpoints()) }
```

**`dashd-wallet`** is the node's wallet service:
- It owns `LoadedWallet { name, manager: WalletManager<ManagedWalletInfo>, db: WalletDb, lock_state, keypool_target, cj_client: Option<CoinJoinClient> }`.
- It consumes `ChainEvent`s on its own task.
- **Relevance pre-filter.** Before calling `WalletInterface`, a hash-set test on raw output scripts, spent outpoints and special-tx elements (ProTx keys, proTxHash) runs over the base-sdk `Tx`. Only matching transactions are bridged to `dashcore::Transaction` (BS10) and handed to key-wallet. The bridge is therefore never on the IBD hot path.
- **Rescans** (`rescanblockchain`, wallet load behind tip) read block files directly. When `blockfilterindex` exists they use the Dash-extended BIP158 filters to skip blocks, bounded by birth height (lesson from the bait9 large-wallet profiling: unbounded BIP158 sweeps are not selective for 345K-script wallets).
- **Persistence: SQLite** (`rusqlite`, bundled), one file per wallet, with explicit tables:

  | Table | Contents |
  |---|---|
  | `meta` | version, network magic (Core-style `application_id` check), flags |
  | `seed` | encrypted mnemonic, passphrase, seed |
  | `accounts` | — |
  | `keys` | derived and imported, encrypted privkeys |
  | `scripts` | — |
  | `txs` | consensus bytes, block hash/height/position, time received, `from_me`, flags incl. `DS` |
  | `utxos` | — |
  | `labels`, `purposes`, `destdata` | — |
  | `locked_outpoints` | — |
  | `coinjoin` | `cj_salt`, pending observations |
  | `gov_objects` | — |
  | `settings` | — |

  Why SQLite: single-file wallets give Core's `backupwallet`/`restorewallet`/`loadwallet`/`unloadwallet` semantics for free, and it is the format Core's own default descriptor wallets use.
- **RPC surface:** every Core wallet RPC (71 in R2b) plus the wallet-gated Dash ones (`protx register*`/`update*`/`revoke`/`shared_*`, `gobject prepare`/`list-prepared`/`vote-many`/`vote-alias`, `coinjoin *`, `coinjoinsalt *`, `masternode outputs`, `setcoinjoinrounds`/`amount`).
  - Coin selection keeps key-wallet's BnB by default (policy, not consensus). `-coinselection=knapsack-compat` is available where exact Core change behaviour matters (functional tests that assert change outputs).
- **Core wallet import** (`rdash-wallet import-core`, also the `importwallet` RPC):
  - **SQLite:** read the `main` table directly.
  - **BDB 4.8:** a pure-Rust read-only btree page parser (leaf and overflow pages; modelled on `test_framework/bdb.py`).
  - **Record mapping:**
    - `hdchain`/`chdchain` → BIP44 account with chain counters (keypool lookahead = counters + 1000);
    - `walletdescriptor*` → accounts, with the mnemonic from the Dash `walletdescriptorkey` extension;
    - loose keys → imported-key account;
    - plus labels, `lockedutxo`, `cj_salt`, `cj_pending_obs` and `g_object`.
  - Encrypted wallets need the passphrase at import.
  - The import report lists anything unmapped. **One-way only** (no writing Core wallets) unless pasta asks.

### 2.20 dash-spv: reuse vs replace

| dash-spv component | Node decision | Reason |
|---|---|---|
| Network manager, peer, handshake, discovery, reputation (`src/network/`) | **Replaced** by `dashd-p2p`; not modified | outbound-only, SPV handshake (`relay=false`, services `HEADERS_COMPRESSED`), no serving, no protocol-violation scoring; #902 is rewriting it upstream |
| BIP324 transport (#264, unmerged) | **Reused**: lifted into `dash-p2p-transport` | the same Dash short IDs and handshake; becomes shared code |
| DNS discovery, addrv2 parsing | ideas reused; Core-faithful addrman replaces them | fingerprint (addrman behaviour is observable) |
| Storage (segments, `DiskStorageManager`) | **Replaced** by `dashd-store` + block files | SPV-shaped (height-keyed, single chain, wallet-matched blocks only) |
| Header/filter/block/masternode sync managers | **Replaced** by `dashd-chainstate` + `BlockSync` | full validation, reorgs with undo, CL constraints |
| `sml_engine` (MasternodeListEngine) and `dashcore::sml` | **Algorithms lifted** into `dash-evo`/`dash-llmq` (SML hashes and roots, scoring, modifier, rotation reconstruction, request IDs: verified by R4), with the gaps fixed (§2.6). The engine itself is not used for validation, since the node computes the full DML. **Reused as a differential consumer**: dash-spv syncs from rdashd, and its engine must validate our `mnlistdiff`/`qrinfo` | verified code is reused; the SPV engine is also an independent check of our serving output |
| Chain work, checkpoints, `ValidationMode` | **Replaced** by `dash-consensus` (DGW/KGW). dash-spv can later adopt `dash-consensus` for header difficulty (fixes #1116/#332) | — |
| Mempool manager | **Replaced** by `dashd-mempool` | wallet-only tracker |
| `test_utils` (`DashdTestContext`, `DashCoreNode`), `contrib/setup-dashd.py`, regtest datasets | **Reused** as the base of `node-difftest`; also extended to start rdashd | the existing dashd harness is the natural seed |
| dashd integration test suites (`dashd_sync`, `dashd_masternode`) | **Reused**: run with rdashd as the server (§4.10) | SPV-as-consumer conformance |
| `HashedBlockHeader`/`HashedBlock` | superseded in the node by `dash-consensus::Block` (cached hashes) | — |

---
## 3. Concurrency and performance architecture

### 3.1 Runtime model

| Execution context | Kind | Default size | Owns / runs | Core equivalent |
|---|---|---|---|---|
| `net` runtime | tokio multi-thread | `min(max(4, cores/2), 16)` | sockets, transports (decode, **block and tx hashing during decode**), per-peer state machines, `send_messages` ticks, `BlockSync`/`ObjectRequests`/`AddrMan`/`ConnMan` actors, RPC HTTP, REST, ZMQ, scheduler timers, overlay `NetHandler`s | `net`, `msghand`, `addcon`, `opencon`, `mncon`, `dnsseed`, `http`/`httpworker`, `scheduler` |
| `validation` | 1 dedicated OS thread | 1 | `ChainstateManager`: header/block acceptance, connect/disconnect, evo state, mempool `remove_for_block` (synchronous, as in Core `ConnectTip`) | the `cs_main` holder (msghand / loadblk) |
| `prevalidate` | rayon pool | `max(2, cores/4)` | context-free block checks, merkle, sigops, outpoint collection, **UTXO prefetch** (`MultiGet`), X11 for header batches | none (Core does this inline under `cs_main`) |
| `script` | rayon pool | `-par` (default `cores-1`; Core caps at 15, rdashd caps at 64) | block and mempool script checks, ECDSA | `scriptch.N` |
| `bls` | rayon pool | `-parbls` (default `max(2, cores/2)`) | BLS batch verification, MSM, DKG share verification, quorum warm-up | `bls-work-N`, `sigsh-work-N`, BLS `CCheckQueue` |
| `db-writer` | 1 OS thread | 1 | block/undo file appends, frozen-layer `WriteBatch` commits, WAL sync points | inline in `FlushStateToDisk` |
| `events` | 1 OS thread | 1 | ordered `ChainEvent` dispatch (validation-interface queue) with backpressure (`limit_queue`, as `LimitValidationInterfaceQueue` does at >10 pending) | `scheduler` callbacks |
| `index-<name>` | 1 OS thread per enabled index | n | `BaseIndex` sync and follow | index `ThreadSync` |
| wallet tasks | tokio tasks | per wallet | wallet event application, rescans (on the `prevalidate` pool) | wallet callbacks |
| overlay workers | tokio tasks | — | sig-share sessions, DKG round per (type, index), IS batch verification, CL enforcement | `sigsh-*`, `llmq-<t>-<i>`, `isman`, `cl-schdlr` |

Rules:
- Nothing on the `net` runtime blocks. CPU work above about 50 µs goes to rayon via `tokio::task::spawn_blocking`-free handoff (rayon `spawn` + oneshot).
- Chainstate is only touched on `validation`. Everyone else reads `ChainView` (ArcSwap) or the `BlockIndex` arena (lock-free reads).
- Lock order, enforced by a debug-build lock-order checker:
  1. mempool write lock;
  2. coins read guard;
  3. RocksDB internals.

  No lock is held across an `.await`.

### 3.2 The IBD / reindex pipeline

```
 peers ─(decode + txid/merkle-leaf hashing in peer tasks)─► BlockSync (window 1024, 16/peer, Core stall logic)
   └─► S1 prevalidate[rayon]: check_block (pow, merkle+mutation, sizes, coinbase, CheckTransaction, sigops)
          collect inputs; mark outpoints created inside the in-flight window
   └─► S2 prefetch[rayon]: MultiGet coins for remaining outpoints → PrefetchMap (bounded, LRU-evicted)
   └─► V  validation thread: accept_block (index, enqueue write) → connect_block (contextual, special txs, UTXO via overlay;
          scripts → script pool; wait) → merge overlay → events
   └─► W  db-writer: blk/rev appends; when the coins layer is frozen, one WriteBatch (coins + index + evo + meta tip)
   └─► E  events: indexes / wallet / ZMQ (bounded; backpressure into V like Core's queue limit)
```

- S1 and S2 run up to `prefetch_depth` (default 64) blocks ahead of the connect point. S2 is what turns cache misses into hits. With dbcache below the UTXO-set size, dashd stalls on synchronous LevelDB reads in `CCoinsViewCache::FetchCoin` on the `cs_main` thread; we take those reads off V.
- V is the only sequential stage. Under assumevalid, V's per-block work is the contextual checks plus special-tx processing plus hash-map updates.
- **Reindex and `-reindex-chainstate`** feed S1 from the block files: a parallel file scanner with Core's magic-scan semantics, then out-of-order arrival into the same pipeline.

### 3.3 Script and signature validation

- **Check queue.** Per block, a `Vec<ScriptCheck>` (input index, prevout coin, flags) is built during the per-tx loop, exactly where Core builds `vChecks`.
  - It is executed with `par_chunks(128)` on the script pool, with an `AtomicBool` early abort.
  - The first failing check (lowest tx index) determines the reported error. This matches Core's `CCheckQueue` result, `block-validation-failed` plus the script error, independent of scheduling.
- **Signature cache.** A salted (random per process) SHA256 of `sighash ‖ pubkey ‖ sig`, in a lock-free set-associative cache (Core uses CuckooCache).
  - Size comes from `-maxsigcachesize` (default 32 MiB, split ½/½ with the script cache).
  - Mempool acceptance inserts; block validation reads, and inserts only when `fJustCheck`, as in Core.
- **Script execution cache.** `SHA256(nonce ‖ txid ‖ flags)`. Transactions accepted to the mempool skip script checks in `connect_block`. This is the main connect-latency lever at the tip (§3.9).
- **ECDSA.** `secp256k1` 0.33 (libsecp256k1): `Signature::from_der_lax` plus `normalize_s` before verify, exactly `CPubKey::Verify`. CHECKDATASIG hashes with a single SHA256 of `msg` and applies `CheckSignatureEncoding` (DER plus one trailing hash-type byte).
- **BLS** (dash-pkc / blst):
  - Basic-scheme signatures in a block (QC `quorumSig`s, CbTx best-CL, asset-unlock, MNHF, ProUpServ/ProUpRev operator signatures) are **batch-verified per block** with random 128-bit scalars on the `bls` pool. A batch failure falls back to individual verification, which identifies the offender and yields the exact Core error.
  - **Legacy-scheme (pre-V19) signatures are always verified individually.** Legacy decoding skips subgroup checks, so the random-linear-combination soundness argument does not hold for them.
  - `membersSig` uses dash-pkc `verify_secure_aggregated` (Core `VerifySecureAggregated` semantics).

### 3.4 Caching strategy

**UTXO cache.** Layered and compact, with async flush.

```rust
struct CoinsCache {
    active: hashbrown::HashMap<OutPointKey, Entry, FxBuild>,   // mutated by validation thread only
    frozen: Option<Arc<FrozenLayer>>,                          // immutable; being written by db-writer
    prefetch: Arc<PrefetchMap>,                                // filled by S2
    db: CoinsTable,                                            // RocksDB CF `coins`
    budget_bytes: usize,                                       // from -dbcache split
}
#[repr(C)] struct OutPointKey([u8; 36]);                       // txid ‖ vout(LE); hashed with salted FxHash
struct Entry { coin: PackedCoin, flags: u8 /* DIRTY | FRESH | SPENT */ }
/// The Core `Coin` serialization itself (VARINT(h·2+cb) ‖ CompressAmount ‖ ScriptCompression), kept inline:
/// a typical P2PKH/P2SH coin is 27–31 bytes (4-byte height VARINT, ≤6-byte amount VARINT, 21-byte script).
/// Decoded on access (a few ns); identical bytes go to the DB.
enum PackedCoin {
    Inline { len: u8, bytes: [u8; 31] },                       // the vast majority of coins
    Heap(Box<[u8]>),                                           // P2PK, multisig, non-standard, rare long amount encodings
}
```

- **Read path:** block overlay → `active` → `frozen` → `prefetch` → DB.
- **FRESH semantics** are exactly Core's: a coin created and spent inside the cache never reaches disk.
- **Freeze-and-flush.** When `active` exceeds about 90% of budget, or on a periodic or forced flush, V atomically moves `active` to `frozen` (an O(1) pointer swap) and starts a new empty `active`. W then writes `frozen` in a background `WriteBatch`; V keeps connecting blocks. Only one frozen layer exists at a time. If a second freeze is needed while one is still being written, V waits. That is a pure backpressure point, and the only place IBD can stall on I/O.
- **Density.** A P2PKH entry is about 36 B key + 32 B coin + 1 B flags, or about 80 B with hashbrown control bytes and load factor. Core's pool-allocated `CCoinsMap` node is about 120–140 B: key, `Coin` with a 28-byte `CScript` prevector, the dirty-list linkage, and bucket and pool overhead. That is roughly **1.5× more coins per MiB**, so at equal `-dbcache` we take fewer misses and flushes. Storing Core's on-disk bytes also makes flush a memcpy, with no re-serialization.

**`-dbcache` split** (default 300 MiB, the same semantics as Core's single knob):

| Bucket | Share |
|---|---|
| RocksDB shared block cache | 8%, max 64 MiB |
| coins memtables | 2 × 16 MiB |
| prefetch map | 5%, max 64 MiB |
| coins cache | remainder |

Index DBs get their own caches, from Core's index cache rules.

**Other caches:**

| Cache | Contents | Bound |
|---|---|---|
| block LRU | `Arc<Block>`, recently connected and accepted | 64 blocks or 128 MiB |
| MN lists | `Arc<MnList>` | LRU of 64 lists plus mini-snapshots every 32 blocks; structural sharing makes these cheap |
| quorums | `Arc<Quorum>` with lazily computed pubkey shares | warmed on build |
| BLS verify cache | for recsig, islock, clsig | 30k entries |
| credit pool | — | LRU 1000 |
| versionbits state | per (deployment, period-end block) | in memory, rebuilt on startup |

### 3.5 DB write batching and durability

- **One `WriteBatch` per flush** carries the frozen coins layer, dirty `block_index` and `block_files` records, all `EvoDelta`s up to the freeze point (MN diffs and snapshots, credit pool, MNHF, mined commitments, snapshots) and `meta.tip`. It commits with `sync=false`.
- **Durability barrier.** W fsyncs the block and undo files written so far, then `flush_wal(sync=true)`. Only then is the frozen layer released. This guarantees every DB record points at durable file data.
- **Flush triggers** are Core's policy, made asynchronous:
  - cache LARGE (≥90%) → freeze;
  - CRITICAL → freeze and wait;
  - block index and files every 1 h;
  - full flush every 24 h;
  - at shutdown and at IBD exit;
  - `-dbbatchsize` caps a single batch (default 16 MiB, as in Core); larger frozen layers are split into sub-batches, with the tip marker written last.
- **Crash consistency** follows from atomicity (§2.10). A crash loses at most the blocks connected since the last freeze, and those replay quickly from block files.

### 3.6 Evo and LLMQ performance levers

1. **Payee selection** goes from O(n) to O(log n) through `payee_queue` (§2.5).
   - Core scans every valid MN per block (`GetMNPayee`): about 3.5k MNs × about 1.5M post-DIP3 blocks.
   - Only consensus-relevant when connecting. GBT and `masternode winners` also benefit.
2. **SML hash caching.** `merkleRootMNList` hashes only changed entries. The root itself is about n SHA256D64 compressions.
   - This only matters without assumevalid and at the tip (connect latency, GBT), since CbTx checks are skipped below assumevalid.
3. **MN list copies are O(1)** (`imbl`); diffs are computed structurally (`OrdMap::diff`).
4. **Quorum member computation** is memoized per (type, base hash), as Core does, and per (cycle hash, quorum index) for rotation. Snapshots are loaded once.
5. **BLS:**
   - Pippenger MSM for recovery: `Σ λ_i·sig_i` in G2, about 240 terms for LLMQ_400_60 CLs.
   - DKG share verification: polynomial evaluation in G1, with up to 340 coefficients.
   - Batch verification (§3.3).
   - The bls pool is sized `cores/2`, versus Core's `clamp(hw/2, 1, 4)` workers.
6. **Event-driven overlays.** IS and recsig verification and CL enforcement run on arrival rather than Core's 100 ms / 5 s polling loops. Send-side batching keeps Core cadence (§2.14).

### 3.7 Mempool and GBT

- **ATMP** is two-phase and parallel (§2.12). Several transactions are accepted concurrently, with scripts running off-lock. Core runs `MemPoolAccept` under `cs_main` plus `mempool.cs`, **including script verification**, so acceptance is effectively serialized.
- **GBT** (`dashd-mining::create_new_block`):
  - The ancestor-score package index is maintained incrementally, so selection is O(selected · log n).
  - The `BuildNewListFromBlock` result is memoized per (prev, special-tx set) and reused for the CbTx fields and the validity test.
  - `test_block_validity` reuses that list plus the script execution cache (every mempool transaction was already verified). It still runs the full connect logic in an overlay that is dropped, so correctness checks are not weakened.
  - The template is cached per (tip, mempool sequence) for longpoll and repeated calls.
  - Core recomputes everything per call, including a second `ProcessSpecialTxsInBlock` inside `TestBlockValidity`.

### 3.8 Memory (RSS) and disk plan

| Component | dashd (estimate; B1 measures) | rdashd plan |
|---|---|---|
| Block index, 2.6M entries | ≈ 560–580 MB (`CBlockIndex` ≈152 B + map node, hash and buckets ≈70 B) | ≈ 380 MB (arena entry ≈142 B + `HashTable<u32>` ≈5 B) |
| Coins cache | `-dbcache` share | same budget, about 1.5× the coins per MiB |
| MN lists | `MAX_CACHED_LISTS` 64 + diff cache 576×k (immer) | LRU 64 with `imbl` sharing; diffs stay on disk |
| Mempool | up to 300 MB | same `-maxmempool`, about 20% smaller entries (no prevector) |
| Allocator | glibc malloc (fragmentation under thread churn) | **mimalloc** (fewer arenas, less fragmentation) |

- **RSS measurement.** B1 should report `RssAnon` and `RssFile` separately. LevelDB in dashd mmaps SST files (up to 1000), so file-backed RSS inflates its number. rdashd avoids mmap for block reads and uses RocksDB's block cache (anonymous), so a like-for-like comparison needs both values.
- **Disk.**
  - Block and undo files are byte-identical to Core's, so the same size.
  - Chainstate: about the same as LevelDB, minus ZSTD savings on values.
  - txindex: about 3× smaller (8-byte prefix keys).
  - addressindex: prefix compression.
  - Net effect: **no larger than dashd**, plausibly 3–8% smaller in total. **2x is impossible**, because block files dominate and Core's format is kept for compatibility. This is documented as an expected shortfall.

### 3.9 How we beat dashd, metric by metric

"Expected" is a design target to be validated by B1 on identical hardware and config. Every row lists the lever and the honest limit.

| Metric | dashd bottleneck (where its own work dominates) | rdashd lever | Expected vs dashd | Limit / documented shortfall |
|---|---|---|---|---|
| IBD to tip, assumevalid (LAN peers) | single-threaded connect under `cs_main`: decode, hashing, merkle, synchronous UTXO misses, O(n) payee scan, synchronous flush stalls | parallel S1/S2, prefetch, async layered flush, O(log n) payee, denser cache | **2–3×** | becomes network-bound over the internet (16 blocks per peer is kept for fingerprint parity) |
| IBD without assumevalid | scripts on ≤15 threads + per-block SML rehash + individual BLS QC checks | all cores, cached SML hashes, BLS batching | **1.5–2.5×** | ECDSA verification cost is identical per signature (same libsecp256k1). On ≤16 cores, gains come only from removing overheads |
| Reindex / reindex-chainstate | as IBD plus a single-threaded file scan | parallel scan, same pipeline | **2–3×** | disk read bandwidth |
| Per-block connect latency at tip | read block, connect under `cs_main`, O(n) payee, SML root, synchronous notifications | compact-block reconstruction, script-cache hits, O(log n) payee, incremental SML, async events | **2×** | dominated by BLS checks when a block carries QCs (batching helps) |
| Script/sig validation throughput | libsecp256k1 + `CCheckQueue` | same library, lock-free sig cache, uncapped pool | **1.0–1.3× per core**; up to ~2× on >16-core hosts at default config | **2× per core is not achievable** with ECDSA (no batch verification) |
| Mempool accept throughput / latency | serialized ATMP including scripts under locks | parallel two-phase ATMP | **≥2× throughput** under load; latency ≈1.1–1.3× | single-tx latency is bounded by signature cost |
| GBT time | full rebuild per call plus a second special-tx pass and full `TestBlockValidity` | incremental package index, memoized list, cache hits, template cache | **≥2×** (≥5× for repeated calls) | first call after a new tip ≈2× |
| Startup-to-ready | sequential block-index load, verify 6 blocks, load mempool | parallel decode into the arena; same verification | **≈2×** | verification work is identical |
| Peak / steady RSS | block index, coins cache, immer caches, glibc fragmentation | compact index and coins, mimalloc | **≤0.75×** | **2× lower is unlikely**: the block index and the coins budget are config-bound |
| Disk footprint | LevelDB, full-key txindex | compact txindex, ZSTD | **≈0.92–0.97×** | block files dominate (identical format) |
| CPU per block | single-threaded overheads, copies | zero-copy hashing, no re-encode | **≈0.6×** CPU-seconds | — |
| RPC latency / throughput | `cs_main` contention, 4 worker threads | lock-free `ChainView` reads, parallel handlers | **≥2× throughput** for read RPCs; ≈1–1.5× single-call latency | wallet RPCs bounded by SQLite |
| DKG round time | phases are block-height-driven | faster contribution and verification work (MSM) | node-local CPU work **3–5×** | wall clock is **protocol-bound**: phases span 2–4 blocks of 2.6 min, so it cannot be 2× |
| Recsig latency / IS lock / CL creation | 100 ms worker polling, relic BLS, 4 BLS workers | event-driven processing, blst, MSM recovery | node-local **2×+**; end to end ≈1.2–1.5× | send cadence is kept at Core's for fingerprint parity; network RTT dominates |
| CoinJoin round servicing | timeouts are protocol-defined | same | ≈1× (CPU is negligible) | protocol-bound |

The status file tracks each metric per merged PR (charter). A regression on any metric blocks merging, measured with B1's harness at fixed config.

### 3.10 Safety rules for optimisations (consensus-critical)

1. **Dual path.** Every algorithmic replacement of a consensus computation keeps a line-by-line Core port as its reference twin, compiled under `cfg(any(test, feature = "xcheck"))`. Examples: payee queue vs linear scan, incremental vs full SML root, batched vs individual BLS, prefetch vs direct reads, MSM vs naive Lagrange.
2. **Cross-check mode.** `-consensusxcheck=1` (debug builds and CI replay) runs both paths and aborts on any difference. Chain replay (§4.6) runs with cross-check on for every milestone tag.
3. **Determinism lint.** In pure crates and `dashd-chainstate`, `HashMap` iteration is forbidden on consensus paths by a clippy `disallowed_methods` config, with a reviewed allowlist. Floating point is forbidden outside `dash_consensus::fp`.
4. **Review.** Optimisations get the program's consensus-review treatment: two independent reviewers, the code-review skill at high effort, and a green differential suite.

---
## 4. Verification architecture

### 4.1 Principles

1. **Dash Core is the spec.** Every consensus rule, parser and observable behaviour is justified by Core code or Core output, never by documentation alone.
2. **Differential by default.** Every layer has an oracle comparison. Hand-written expectations come only from Core-generated vectors.
3. **Architecture serves testability:**
   - pure crates take explicit inputs;
   - state-dependent validity is injected (`ChainHelper`);
   - all time comes from `NodeClock`;
   - optimised paths have reference twins.
4. **Ratchets, not targets.** Pass lists (functional tests, fuzz targets, replay ranges) only grow. A regression blocks merging.

### 4.2 Evidence layers

| # | Layer | What it proves | Tooling | Runs |
|---|---|---|---|---|
| L1 | Core JSON vectors and KATs | rule-level parity on curated cases | `dash-consensus/tests/core_vectors/*` | every PR |
| L2 | **In-process differential fuzzing** against `libdash_oracle` | parser and rule parity on adversarial inputs | `node-fuzz` (libFuzzer), `dashcore-oracle-sys` | per PR (60 s per target); continuous on agentbox (8 cores) |
| L3 | Codec domain equivalence on real data | decode/re-encode identity across all history | BS2 harness | nightly; per PR on a testnet sample |
| L3m | **Mutation testing (rejection parity)** | identical verdict, reject string and result class on invalid blocks and txs | `node-difftest mutate`: about 40 operators (R4 §2.4); Core oracles: `getblocktemplate` proposal mode (zero-PoW `TestBlockValidity`), `testmempoolaccept`, regtest `submitblock` and P2P submission | per PR: about 500 mutations; nightly long run |
| L4 | **Chain replay with `StateDigest` equality** | full consensus parity (UTXO, MN list, quorums, credit pool, deployments) on mainnet and testnet, **with `-assumevalid=0`** (R4 F2) | `node-difftest replay`, Core `-statedigestlog` plus R4's RPC golden extraction | per PR: windowed (§4.6). Nightly: full testnet. Weekly: full mainnet with `-consensusxcheck` |
| L5 | RPC differential | JSON shapes and values match dashd | `node-difftest rpcdiff` | per PR touching RPC; nightly corpus |
| L6 | **Core functional tests against rdashd** (single and mixed) | user-visible behaviour, MN protocols, mixed-quorum interop | Dash Core `test/functional` + our framework patch | per PR (passlist subset); nightly (full passlist) |
| L7 | P2P transcript diff | fingerprint parity | `node-difftest p2pdiff` (Python, Core `P2PInterface`) | per PR touching P2P; nightly |
| L8 | SPV as consumer | serving correctness (headers2, mnlistdiff, qrinfo, filters, CL/IS relay) | dash-spv `dashd_sync` and `dashd_masternode` suites against rdashd | nightly |
| L9 | Shadow mode | live agreement on mainnet and testnet | side-by-side rdashd + dashd plus a comparator | continuous from M4 |
| L10 | Unit and property tests | local invariants | `cargo test`, proptest | every PR |

### 4.3 Core vectors and KATs (L1)

`contrib/node/sync-core-vectors.sh` copies files from a pinned Dash Core commit into `dash-consensus/tests/data/` and records the commit hash. Updating them is a PR.

| Source | Consumer test | Notes |
|---|---|---|
| `script_tests.json` (1,282 tests incl. the DIP0020 section lines 832–1256) | `script_vectors.rs` | DIP0020 opcodes run without any flag; expected errors map 1:1 to `ScriptError` |
| `tx_valid.json` / `tx_invalid.json` | `tx_vectors.rs` | Core's flag names, `BADTX` → `check_transaction` failure |
| `sighash.json` (500) | `sighash_vectors.rs` | legacy sighash; *also* random special-tx sighash cases from L2, since the JSON lacks v3 transactions |
| `trivially_valid.json` / `trivially_invalid.json` | `dash-evo/tests/trivial.rs` | legacy and basic BLS variants; exact reject codes |
| `blockfilters.json` (Dash X11 re-hashes) | `dashd-indexes` | Dash-extended elements |
| `key_io_*`, `base58_*`, `bip39_vectors.json`, DIP14 KATs | key-wallet / dash-consensus | — |
| `proposals_{valid,invalid}.json` | `dash-governance` | — |
| `pow_tests` (DGW with 24 real mainnet pairs, devnet rules), `subsidy_tests` (real heights) | `dash-consensus` | plus **exhaustive historical sweeps** (below) |
| `evo_simplifiedmns_tests`, `evo_islock_tests`, `llmq_*_tests`, `quorum_proofs_tests`, `bls_tests` + dashbls `test.cpp` vectors | `dash-evo`, `dash-llmq`, dash-pkc | — |
| `test/util/data/bitcoin-util-test.json` | `rdash-tx` (P1) | dash-tx CLI parity |

**Floating point (`dash_consensus::fp`).**
- Subsidy before V20 and KGW use libm `pow` in IEEE doubles.
- We port **glibc's `pow`**: the ARM optimized-routines algorithm glibc has used since 2.28, non-FMA path, table-driven, with no platform libm calls. This makes results identical on Linux, macOS, Windows and ARM.
- Verification:
  - **exhaustive** over every historical input: each mainnet and testnet block before V20 (about 2M subsidy evaluations) and every KGW evaluation (mainnet 15200–34140), compared against dashd via the oracle;
  - plus the regtest and devnet domains (`bits=0x207fffff`, high-subsidy factors);
  - plus 10⁸ random inputs compared against glibc on x86-64 and aarch64.
- Any mismatch in random inputs that cannot occur historically is documented and decided case by case. The historical sweep must be 100% equal.
- **Why historical parity is what matters.**
  - Release dashd links the *host* glibc `pow`. Pre-2.28 hosts use IBM's correctly-rounded mathlib; later hosts use the <1-ULP ARM routines. So "Core's answer" is only uniform where the network accepted it, which is exactly the historical domain the sweep pins.
  - KGW only runs at mainnet heights 15200–34140. Those fall inside Core's ±50% difficulty-tolerance window (height ≤ 68589), so ULP-level `pow` differences cannot change a validation outcome there.
  - The exactness that matters is the pre-V20 subsidy truncation (`(int64)(2222222.0 / pow(…, 2.0))`) and its two earlier eras.

### 4.4 The Dash Core oracle shim (`libdash_oracle`) and differential fuzzing (L2)

**Where.**
- Dash Core branch `node/oracle-shim` (R4 V2) in `~/workspace/dash` adds `src/test/oracle/{oracle.h,oracle.cpp}` and a `Makefile.oracle.include` target. Built against `depends`, it produces a static `libdash_oracle.a` bundling libsecp256k1, dashbls, the X11 SPH code and univalue.
- It links Core's real implementation objects (`libbitcoin_consensus`, `libbitcoin_common`, evo, llmq, governance and coinjoin object files) and calls them directly. The shim adds **no logic**; it only adapts the ABI.
- It extends the existing `libdashconsensus` idea, which today exports only `dashconsensus_verify_script` with five flags and no `ScriptError`. That library is deprecated, but it is usable as a week-1 stopgap oracle for CS1, from the rc.3 release tarball (R4 §2.2). The functional framework's Python codecs (`messages.py`) act as a **third implementation** for codec disagreements.

**C ABI** (buffers in/out, deterministic, no globals beyond explicit `oracle_set_network(net, params_json)`):

```c
int  oracle_tx_decode(const uint8_t*, size_t, uint8_t* reencoded, size_t* n, uint8_t txid[32], uint8_t instance[32]);   // 0 ok, else Core exception class
int  oracle_block_decode(const uint8_t*, size_t, uint8_t hash[32], uint8_t merkle[32], int* mutated, ...);
int  oracle_check_transaction(const uint8_t*, size_t, char reason[64]);
int  oracle_check_block(const uint8_t*, size_t, int check_pow, int check_merkle, char reason[64]);
int  oracle_verify_script(const uint8_t* spk, size_t, const uint8_t* tx, size_t, uint32_t n_in, uint32_t flags, int* script_err);
int  oracle_signature_hash(const uint8_t* script, size_t, const uint8_t* tx, size_t, uint32_t n_in, int32_t hashtype, uint8_t out[32]);
int  oracle_payload_decode(uint16_t type, const uint8_t*, size_t, int* consumed_all, char trivial_reason[64], uint8_t* reenc, size_t*);
int  oracle_p2p_decode(const char* cmd, const uint8_t*, size_t, int32_t peer_version, uint8_t* reenc, size_t*);
int  oracle_headers2_decode(const uint8_t*, size_t, uint8_t* headers80xN, size_t* n);
int  oracle_next_work_required(const uint8_t* headers, size_t n_headers, const uint8_t new_header[80], uint32_t* bits);
int64_t oracle_block_subsidy_inner(uint32_t prev_bits, int32_t prev_height, int v20);
int64_t oracle_mn_payment(int32_t height, int64_t block_value, int era);
int  oracle_bls(int op, int legacy, const uint8_t* in, size_t, uint8_t* out, size_t*);   // decode/malleability, verify, aggregate(_secure), recover, IES
int  oracle_gov_hashes(const uint8_t* obj_or_vote, size_t, int kind, uint8_t hash[32], uint8_t sighash[32]);
int  oracle_validate_proposal(const char* hex, char err[512]);
int  oracle_sml_entry_hash(const uint8_t* dmn, size_t, uint8_t out[32]);
int  oracle_quorum_members(const uint8_t* mn_list, size_t, uint8_t llmq_type, const uint8_t modifier[32], uint8_t* out, size_t*);
int  oracle_coinjoin_valid_in_outs(...);
int  oracle_coin_codec(int op, const uint8_t*, size_t, uint8_t*, size_t*);   // Coin, CompressAmount, ScriptCompression, VARINT, CompactSize, undo
```

- The Rust side is `dashcore-oracle-sys`: bindgen, linking the `.a` built by `contrib/node/build-oracle.sh` (pinned dash commit, cached under `/work/cache/oracle/<commit>`). It sits behind feature `oracle`, so it is never in production builds.

**Differential fuzz targets** (`node-fuzz/fuzz_targets/diff_*.rs`; each compares accept/reject class, Core reason string, re-encoding and outputs):

| Target | Inputs | Compares |
|---|---|---|
| `diff_tx_decode`, `diff_block_decode`, `diff_header_decode` | raw bytes (corpus: all mainnet and testnet txs and blocks from BS2) | decode success, re-encoding, txid, instance hash, block hash, merkle + mutated |
| `diff_payload_<type>` (13 types × versions) | raw payload bytes + tx context | decode, consumed-all, `IsTriviallyValid` reason |
| `diff_check_transaction`, `diff_check_block` | tx/block bytes | reject reason |
| `diff_script_verify` | (scriptSig, scriptPubKey, tx, nIn, flags) | result and `ScriptError` |
| `diff_sighash` | (script, tx including special types, nIn, hashtype) | 32-byte hash |
| `diff_p2p_<cmd>` (74 commands × peer-version buckets) | payload bytes | decode and re-encode (domain equivalence, §1.5) |
| `diff_headers2` | compressed stream | header sequence |
| `diff_codec_primitives` | bytes | VARINT, CompactSize, Coin, CompressAmount, ScriptCompression, undo records |
| `diff_next_work` | synthetic header windows (random times and bits around KGW/DGW boundaries, min-difficulty rules) | nBits |
| `diff_subsidy_payments` | (prev_bits, prev_height, flags, era, fees) | subsidy, MN payment, platform share, treasury |
| `diff_bls_*` | random bytes and keys | legacy/basic decode (incl. "opposite scheme → invalid" rule), verify, secure aggregate, threshold recover, IES |
| `diff_gov` | objects, votes, proposal JSON | hashes, signature hashes, validation errors, trigger JSON bytes |
| `diff_sml` / `diff_quorum_members` | random MN lists (structured fuzz) | entry hashes, merkle root, member ordering (score and collateral tie-break) |
| `diff_coinjoin` | entries and final txs | `IsValidInOuts` classes and messages |

Plus model-based stateful targets, which need no oracle:
- `coins_cache_model`: layered cache plus freeze/flush vs a `BTreeMap` model, like Core's `coinscache_sim`;
- `mempool_model`;
- `mnlist_payee_queue`: payee queue vs linear scan;
- `dkg_session`: a 3–20 member in-process DKG with adversarial message drops and lies, checking that honest members agree.

**Corpora and triage.** Seeds come from BS2 (all historical txs and blocks), from `-capturemessages` captures taken from dashd peers on mainnet and testnet, and from functional-test traffic. Every crash or divergence becomes a minimized regression test plus an STATUS.md task automatically.

### 4.5 Codec domain equivalence (L3)

- **BS2** runs three-way over full history (dashcore, base-sdk, dashd):
  - every block's bytes decode through base-sdk and re-encode identically;
  - txids, instance hashes, block hash and merkle root match dashd's `getblock … 2`.
- **L2 `diff_*_decode`** covers the adversarial side.
- Together they gate BS4/BS6/BS7/BS8a/BS8b and every later base-sdk bump.

### 4.6 Chain replay and `StateDigest` (L4)

**Core patch** (branch `node/state-digest`, R4 V4): `-statedigestlog=<path>` makes `ConnectBlock` (after success, outside `fJustCheck`) append one line per block, and `DisconnectBlock` append a `D <hash>` line:

```
StateDigest v1 {
  height, block_hash,
  utxo_muhash      : MuHash3072 finalized digest of the full UTXO set after this block (incremental, coinstatsindex-identical),
  utxo_count, utxo_total_amount,
  undo_hash        : SHA256d(serialized CBlockUndo)          // byte-level undo parity
  mnlist_sml_root  : merkleRootMNList of the list after this block (computed even below assumevalid),
  mnlist_full_hash : Core's CanonicalMNListHash(list) (evo/snapshot.cpp:150-155; R4 F8), covering every DMN field,
  quorums_root     : merkleRootQuorums as computed (even pre-DIP8 / below assumevalid),
  credit_pool      : (locked, current_limit, lately_unlocked, SHA256d(indexes range set)),
  mnhf_signals     : SHA256d(map<bit, height>),
  versionbits      : [(deployment, state, since_height)],
  bls_legacy_after : bool,
  snapshot_written : Option<(llmq_type, cycle_hash, SHA256d(snapshot))>
}
```

- rdashd emits the identical record (`-statedigestlog`).
- **R4's RPC golden extraction** complements the patch and works with *stock* release binaries (R4 §2.3.2): `gettxoutsetinfo muhash <h>`, `getblockstats`, `masternode payments`, `protx listdiff h-1 h`, `getcreditpoolinfo`, `quorum listextended`, `quorum rotationinfo`, `getdeploymentinfo`, `getchainlockbyheight`. That is about 9M calls per mainnet extraction, run once per pinned release.
- **Replays run with `-assumevalid=0`** on both sides. Below assumevalid Core also skips CbTx, credit-pool, commitment and ProTx signature checks (R4 F2), so default-assumevalid runs are only perf benchmarks.
- Because the MuHash is incremental per block, replay cost is O(changes), not O(UTXO set).
- `undo_hash` pins byte-level undo parity.
- `mnlist_full_hash` covers state the SML omits: payouts, PoSe, last-paid, shares and more.

**Recorded state decisions.**
- Block validity depends on the best ChainLock, IS conflicts outside IBD, governance trigger state and MN-sync status (R2 G.1 #7).
- The Core patch adds `-recordstatedecisions=<path>`, which logs, per connected block:
  - `HasConflictingChainLock` inputs (best CL height and hash at connect time);
  - the IS conflicts encountered;
  - `check_superblock` mode;
  - the triggered superblock payment list.
- `ReplayChainHelper` replays these, so offline replay of historical blocks is **deterministic and decision-equivalent**.
- Live (non-replay) behaviour is covered by L6 and L9.

**Replay tool** (`node-difftest replay --network main --from H1 --to H2 --digest core.digest`):
1. Starts from a **fast-imported Core datadir snapshot** at H1, or from genesis.
2. Feeds blocks from the oracle node's block files through `dashd-chainstate`.
3. Streams the comparison.
4. At the first mismatch it dumps both sides' detail at that height (`protx list`, `quorum list`, UTXO delta) and the field that diverged.

**Snapshots for windowed replay.** The oracle node produces importable snapshots (Core datadir copies) at fixed heights:
- mainnet (R4 §2.3.4): 68589 (DGW exactness), 328008 (treasury), 614820 (superblocks), 1028159 (DIP3), 1047199 (DIP3 enforcement), 1088639 (DIP8), 1374911 (BRR), 1737791 (DIP24), 1899071 (V19), 1987775 (V20), 2128895 (MN_RR), 2201471 (WITHDRAWALS), and tip − 50k;
- testnet: the equivalents, plus around the V24 activation.

Each snapshot is validated against the oracle at its height (muhash, `protx list`, `quorum listextended`, `rotationinfo`, credit pool).

Per-PR gating replays **the windows touched by the change's domain**: 20k-block windows covering at least 3 DKG cycles of every LLMQ type. That takes minutes, not the hours of a full replay.

**One-off chain scans** (R4 §2.3.5) tell us which quirks history actually exercises, so those get tested hardest:
- `nVersion>3` special txs;
- pre-DIP3 non-zero type bytes;
- budget-window overpayments;
- odd CbTx encodings;
- MNHF signals by bit;
- disabled-type commitments.

**Box contention.** Replays must not overlap B1 benchmark runs (B1 pins NUMA node 0). Correctness jobs run on node 1 or in benchmark-free windows, scheduled through STATUS.md.

### 4.7 RPC differential (L5)

- Two nodes on the **same chain** (a regtest scenario script, or both at the same mainnet/testnet tip in shadow mode).
- A corpus generated from `dash-rpc-spec`: every read-only method × argument variants.
  - Examples: `getblock` across verbosities and across blocks containing every special-tx type, `getrawtransaction`, `decoderawtransaction`, `protx diff`/`listdiff`, `quorum info`/`list`/`listextended`/`rotationinfo`, `masternodelist` modes, `gobject list`, `getblocktemplate` (same mempool), `getdeploymentinfo`, `getchaintips`, `gettxoutsetinfo muhash`.
- Responses are compared after normalization: drop known nondeterministic fields (`timemillis`, peer IDs, `mempool` time) and keep everything else, **numbers compared textually** (`1.00000000`).
- Wallet RPCs run in regtest scenarios with identical seeds.

### 4.8 Dash Core functional tests against rdashd (L6)

**Mechanics:**
- `DASHD=/path/rdashd` plus a generated `config.ini` (`test_framework.py:272-287` reads it).
- The framework already supports per-node binaries (`add_nodes(binary=[...])`; R4 F10). Our Dash Core branch `node/func-compat` adds only a runner flag, `--rust-nodes=<mask>` / `RDASHD_NODES`, mapping node indices to rdashd, plus a tracked `rust_expected_failures.txt` (test, reason, owner).
- That is the whole patch. Tests themselves stay unmodified, except a tracked list of necessary adaptations (`node-difftest/functional-adaptations.md`), for example a test that asserts a Core-specific `debug.log` line we could not reasonably reproduce. That list is reviewed and kept short.

**What rdashd must satisfy** (from the test-framework survey):
- `regtest=1` in the config file;
- the option set passed by `TestNode` (`-logtimemicros`, `-debug`, `-debugexclude=libevent|leveldb|rand`, `-uacomment`, `-par=2`, `-maxsigcachesize=1`, `-parbls=2`, `-mocktime`, `-tinyblk`, `-unsafesqlitesync`, `-rpcdoccheck`, `-peertimeout`, `-v2transport`, `-bind=…=onion`);
- cookie auth at `<datadir>/regtest/.cookie`;
- `-28` warmup;
- `getmempoolinfo.loaded`;
- `stop` with exit code 0 and **empty stderr**;
- `assert_debug_log` lines;
- the datadir subdirectory layout (§2.9);
- the hidden RPC set (§2.16);
- `dash_hash` X11 for the Python side (already a framework dependency).

**Census (R4 §2.5).** 264 scripts use 209 RPC methods. 151 need no node wallet; 83 need it; the 34 `DashTestFramework` tests need a wallet only on node 0, because masternodes force `-disablewallet`. R4's tiers map onto the phases below: tier 1 (35 tests, about 60 RPCs) gates M3–M4, tier 2 (29 DTF tests, mixed) gates M7, tier 3 (fingerprint and ops) gates M5, and tier 4 (58 wallet tests) gates M6.

**Phased passlist:**

| Phase | Configuration | Tests |
|---|---|---|
| M3–M4 | all nodes rdashd, no wallet | `feature_block`, `p2p_invalid_*`, `feature_cltv`/`dersig`/`csv_activation`/`nulldummy`, `feature_assumevalid`, `feature_reindex`, `feature_loadblock`, `rpc_blockchain` (read parts), `p2p_sendheaders_compressed`, … |
| M5 | services | `mempool_*`, `p2p_*`, `rpc_*` (non-wallet), indexes (`feature_addressindex`/`spentindex`/`timestampindex`/`txindex`/`coinstatsindex`), `interface_zmq*`, `interface_rest`, `feature_governance*` (with dashd node0 wallet until M6) |
| **Mixed mode A (M5)** | node 0 and MNs = dashd; **non-masternode "simple" nodes = rdashd** | rdashd follows quorums, CL/IS, governance and sporks as a regular node inside every `DashTestFramework` test |
| **Mixed mode B (M7, independent of M6)** | **node 0 = dashd** (wallet funder, `sporkkey`, miner), **masternodes = rdashd** | `DashTestFramework` already creates MNs via node 0's wallet RPCs (`protx register_fund`) and copies node 0's datadir to the MN nodes, so this works once rdashd can start from a copied dashd regtest datadir. **rdashd auto-detects a Core datadir**: LevelDB `chainstate/` (with `\0obfuscate_key`), `blocks/index/` or `evodb/`. On regtest by default (elsewhere via `-importcoredatadir`), it moves those aside and reindexes from the byte-compatible `blk*.dat` files. That takes seconds for test chains. The same path covers the framework's chain cache when node 0 is dashd. D15's block-file compatibility is what makes this cheap. All 34 `DashTestFramework` tests become **mixed-quorum interop tests**. This is exactly the charter's "mixed regtest networks where Rust and dashd masternodes share quorums". |
| M6 | rdashd node 0 (wallet) | `wallet_*` plus all Dash wallet-dependent tests |
| M7 | MN parity | every `feature_llmq_*`, `feature_dip3_*`, `feature_masternode_*`, `p2p_instantsend`, `p2p_quorum_data`, `feature_mnehf`, `feature_asset_locks`, `rpc_quorum`, `rpc_mnauth` in three configurations: all-dashd baseline, all-rdashd, and alternating mixed |

The passlist (`node-difftest/functional-passlist.txt`) is a ratchet and part of the integration gate.

**Devnet interop (M7).** A docker-compose devnet with N dashd and M rdashd evonodes and MNs. A miner, a spork key and Platform-less runs, held for 2+ weeks with chaos (restarts, partitions). Assertions: DKG success rates equal, PoSe never penalizes rdashd, CL/IS latency distributions equal or better.

### 4.9 P2P transcript diff (L7)

- A Python harness built on Core's `test_framework.p2p.P2PInterface` and `v2_p2p.py`. It runs a scenario against a dashd and an rdashd that are in **identical regtest state** (same blocks, mocktime) and records every message both ways.
- Transcripts are normalized: nonces, timestamps, random challenges and Poisson-jittered inv batches are compared as sets within windows.
- **Scenarios:**
  - inbound and outbound handshakes for each connection type (incl. MN probe and fMasternode peers), v1 and v2;
  - getheaders/getheaders2 with edge locators (null, >101, unknown);
  - getaddr repeat;
  - mempool with and without bloom;
  - filterload limits;
  - every misbehavior trigger (score, then disconnect at 100);
  - unknown commands;
  - bad checksum;
  - oversized messages;
  - blocks-only violations;
  - unsolicited mnlistdiff, qrinfo and clsig;
  - govsync repeat;
  - senddsq modes by version;
  - short-ID usage by peer version.
- **Timing scenarios** under mocktime: ping interval and timeout, trickle intervals (statistical), stalling and timeouts, `getsporks`/`mempool`/`govsync` cadence.
- **"Spoofing peer" probes.** Feed both nodes an identical randomized message sequence from a peer claiming various UAs and versions; any behavioural difference is a finding.
- **Statistics and classifier** (R4 §2.7):
  - Kolmogorov–Smirnov tests on inter-message-gap distributions over ≥10⁴ samples.
  - Passive `-capturemessages` traces from both nodes side by side on testnet and mainnet, compared as message n-grams.
  - An adversarial gradient-boosted classifier over probe and trace features. **Acceptance target: held-out AUC ≤ 0.55**, re-run on every release of either implementation.

### 4.10 SPV as consumer (L8)

- dash-spv's `dashd_sync` (35 tests) and `dashd_masternode` (10 tests) suites gain a `DASHD_IMPL=rdashd` switch in `DashCoreNode`, and run against rdashd as the only peer.
- Their engine independently verifies our `mnlistdiff`/`qrinfo` (merkle proofs, quorum BLS), `headers2`, BIP157 filters (incl. Dash extras), ChainLock and ISLock relay.
- A small dashj-style BIP37 client test (Python) covers `filterload` → `merkleblock` + `tx` + `isdlock` ordering.

### 4.11 Shadow mode (L9)

- From M4, agentbox runs `rdashd` and `dashd` on mainnet and testnet. Each has its own peers; the two are also connected to each other.
- A comparator compares the two nodes after every new block:
  - tip (with a ChainLock lag window);
  - `getblockchaininfo`;
  - `gettxoutsetinfo muhash` (coinstatsindex on both);
  - `protx diff` (prev → tip);
  - `quorum list`;
  - `getbestchainlock`;
  - deployment info;
  - mempool size and conflicts (set similarity).
- Divergence pages the manager (STATUS.md entry plus task). This is Done-1's continuous proof and catches state-dependent behaviour that replay cannot.

### 4.12 Gates

| Gate | Applies to | Content |
|---|---|---|
| G0 (every PR) | all | fmt, clippy `-D warnings`, `cargo test --workspace`, dashd suites (`dash-spv`), L1, L2 smoke (60 s per touched target) |
| G1 (consensus PR) | pure crates, chainstate, base-sdk bumps | G0 + windowed L4 replay with `-consensusxcheck` + L3 testnet sample + 2 reviewers + code-review skill |
| G2 (P2P PR) | p2p, transport, overlays | G0 + L7 transcript suite + L8 subset |
| G3 (RPC/CLI PR) | rpc, cli, wallet RPC | G0 + L5 corpus subset + affected functional tests |
| Nightly | integration | full testnet replay, L6 passlist, L8, fuzz corpus re-run, B1 benchmarks (per-metric regression check) |
| Weekly / milestone | integration | full mainnet replay with xcheck, full L5/L7, devnet soak report |

---
## 5. Sequencing: from "Base SDK done" to "masternode on mainnet"

### 5.1 Milestones and exit criteria

Weeks are calendar weeks from program start (2026-10-08), assuming 8–10 concurrent agent implementers plus reviewers. They are estimates to plan against, not commitments.

| Milestone | Window | Content | Exit criteria (all must hold) |
|---|---|---|---|
| **M0 Foundations** | wk 0–4 | B1 harness + dashd baseline; BS1, BS2; oracle shim O1; chain scans O11; quirk scaffolding O12; vectors and fixtures O13; the pasta decision on EMBARGO-1 disclosure (manager escalation, R4 V0); storage bake-off ST1; block files ST2; script interpreter CS1; fp/pow/subsidy CS2; tx/block checks CS3; `dash-args` + RPC skeleton + `rdash-cli` AR1; fuzz scaffolding O9 | L1 vectors 100% (script, tx, sighash, pow, subsidy); `diff_script_verify`, `diff_sighash`, `diff_tx_decode` clean ≥24 CPU-h; bake-off decision recorded; `rdash-cli -getinfo/-netinfo` work against dashd |
| **M1 Base SDK milestone** | wk 0–7 | BS3–BS12 (BS7/BS8b may finish in M2–M3), BS13 in parallel | §1.7 "done" definition |
| **M2 Headers-first sync** | wk 4–9 | CH1 block index + header acceptance + ChainView; P1 transport; P2 outbound P2P core (handshake parity, ping, addrman, seeds, headers/headers2); ND1 node assembly; O3 digest patch; O4 functional binary selection; O7 v0 transcripts | rdashd syncs mainnet and testnet **headers** to tip; header chain and chainwork equal dashd; handshake transcripts identical (v1 and v2, inbound and outbound); restart reloads the index |
| **M3 Full validation (non-evo)** | wk 6–14 | CH2 coins + async flush; CH3 connect/disconnect/reorg/assumevalid; CH4 pipeline; CH5 reindex/loadblock/verifychain; CS4 versionbits; CS5 coin codecs; P3 block download + compact blocks (client); MN0 minimal regtest miner; O5 replay tool; RP1 core RPCs (subset) | replay mainnet 0 → 1,028,159 and testnet 0 → 6,999 with **per-block StateDigest equality** (UTXO MuHash, undo hash); regtest reorg and crash-recovery suites; first functional passlist (block, consensus tests) |
| **M4 Evo/LLMQ consensus: full parity** | wk 10–20 | EV1–EV4, LQ1–LQ3, CH7 evo integration with `ChainHelper` (CL/IS verification from P2P; governance in Core's not-synced `NoCheck` path); CH6 Core import; O8 shadow mode | **Charter Done-1**: full mainnet and testnet replay from genesis with complete StateDigest equality (UTXO, MN list full hash, quorums, credit pool, deployments) under `-consensusxcheck`; live sync from the network following ChainLocks; 7 days of shadow mode with no divergence |
| **M5 Full-node services** | wk 14–26 | P4 inbound/serving/misbehavior; P5 tx relay; P6 overlay relay (sporks, govsync, clsig/isdlock, dstx, mnlistdiff/qrinfo/cfilters serving, platformban); MP1 mempool; MN1 mining/GBT; IX1 indexes; RP1/RP2 RPC breadth; ZQ1 ZMQ/REST/notifications; GV1 governance runtime (full superblock validation); SP1 sporks + mnsync; O6 RPC diff; O7 full transcripts | rdashd is a drop-in non-masternode dashd: L7 full green; L8 (dash-spv suites) green; non-wallet functional passlist covers all applicable tests; GBT-mined blocks accepted by dashd; RPC diff corpus green; **mixed mode A green**: every `DashTestFramework` test passes with rdashd as the non-masternode nodes |
| **M6 Wallet** | wk 12–28 | WL1–WL6; wallet RPCs; Core wallet import; `rdash-wallet` | wallet functional tests green; Core BDB and SQLite fixture wallets (incl. encrypted, legacy hdchain, descriptor + mnemonic) import with identical balances and addresses; `DashTestFramework` runs with **rdashd as node 0** |
| **M7 Masternode** | wk 16–32 | AM1 active MN + mnauth + MN connections; QM1 quorum manager + recovery; LQ4 + DK1 DKG; SG1 signing + sigshares; LK1 CL/IS/EHF signers; GS1 governance signer; CJ1 CoinJoin server; PL1 Platform RPCs; P7 MN connection topology | All `feature_llmq_*`/`feature_dip3_*`/`feature_masternode_*`/`p2p_instantsend`/`p2p_quorum_data`/`feature_mnehf`/`feature_asset_locks`/`rpc_quorum`/`rpc_mnauth` green in mixed mode B (dashd node 0, rdashd MNs), in alternating dashd/rdashd MNs, and, once M6 lands, all-rdashd; devnet soak (≥50% rdashd MNs, 2 weeks) with equal DKG success, zero PoSe on rdashd; **testnet evonode** running rdashd for 2+ weeks with no PoSe, signing CL/IS and serving CoinJoin (**Done-2 on testnet**) |
| **M8 Mainnet MN and parity report** | wk 28–40 | mainnet masternode (resources from pasta, §7 Q5); remaining RPC/CLI parity (Done-3); PRD story closure across node, CLI and desktop (Done-4); full suites (Done-5); performance report vs the 2× targets with explanations (Done-6) | Charter Done-1..6 all satisfied and documented |

### 5.2 Task graph (PR-sized; ID → deps)

Size is the effort class: S ≤ 1 agent-week, M ≤ 2, L ≤ 4, XL > 4. Every L/XL item needs a design doc first (PROCESS.md). Consensus-critical items carry ★: they get two reviewers plus a windowed replay gate.

| ID | Task | Stream | Size | Depends on | MS |
|---|---|---|---|---|---|
| BS1–BS14 (incl. BS8a/b) | Base SDK milestone (§1.7) | W1 | — | — | M0–M1 |
| O1 | Oracle shim v1, `node/oracle-shim` (decode incl. Core re-serialization, checks, script, sighash, codec primitives; R4 V2 P0 set). libdashconsensus is the week-1 stopgap | W7 | M | — | M0 |
| O2 | Oracle shim v2 (payloads, P2P, headers2, BLS, governance, SML, quorum members, CoinJoin, pow/subsidy) | W7 | L | O1 | M1–M2 |
| O3 | `node/state-digest` Core patch (`-statedigestlog` with `CanonicalMNListHash`, `-recordstatedecisions`; R4 V4) + RPC golden extractor (R4 V3); digests and decisions for full mainnet and testnet per pinned release | W7 | M | O1 | M2 |
| O4 | `node/func-compat` runner flag over the framework's `add_nodes(binary=[...])` + passlist and expected-failures runner (R4 V6) | W7 | S | — | M2 |
| O5 | Replay tool + windowed snapshots | W7 | M | O3, CH3 | M3 |
| O6 | RPC diff harness | W7 | M | RP1 | M5 |
| O7 | P2P transcript harness (v0 handshake → full) | W7 | M→L | P2 | M2→M5 |
| O8 | Shadow-mode comparator | W7 | S | CH7 | M4 |
| O9 | node-fuzz scaffolding, CI wiring, continuous fuzz farm | W7 | M | O1 | M0→ |
| O10 | L3m mutation engine (about 40 operators) + GBT-proposal / `testmempoolaccept` differential harness (R4 V7) | W7 | M | O5, MN0 | M3–M4 |
| O11 | One-off chain scans via the oracle (R4 V5) | W7 | S | BS2 | M0 |
| O12 | Quirk-register scaffolding: one named test per R4 §1.15 entry (`quirk_NN_*`), initially `#[ignore]` with an owner; un-ignored by the implementing task | W7 | S | — | M0 |
| O13 | Regenerate rust-dashcore SML/QRInfo fixtures from dashd (the current `.bin` files are not Core-produced; R4 F12) and vendor Core vectors (`dash-consensus` test data; R4 V1) | W7 | S | BS2 | M0 |
| ST1 | `dashd-store` + engine bake-off | W3 | M | O1 (trace) | M0 |
| ST2 | Block files (FlatFileSeq, blk/rev, scanner) | W3 | S | — | M0 |
| CS1 ★ | Script interpreter, sighash, sigops, DIP0020, CHECKDATASIG | W2 | L | — | M0 |
| CS2 ★ | `fp` pow port, BTC/KGW/DGW, `check_proof_of_work`, subsidy, MN payment schedule | W2 | M | O1 | M0 |
| CS3 ★ | Context-free and contextual tx/block/header checks, Core reject strings | W2 | M | BS4 | M0–M1 |
| CS4 ★ | Deployments and versionbits with Dash extensions (dynamic threshold, EHF signal height) | W2 | M | BS5 | M1–M3 |
| CS5 ★ | Coin and undo codecs, `CoinsView`, `check_tx_inputs`, sequence locks, finality | W2 | S | CS3 | M1 |
| AR1 | `dash-args`, `dash-rpc-spec`, `dashd-rpc` core (auth, dispatch, queues, help), `rdash-cli` | W6 | L | — | M0–M1 |
| ND1 | `dashd-node`: init/shutdown, `NodeClock`/mocktime, scheduler, Core-format logging | W6 | M | AR1 | M2 |
| CH1 ★ | Block index arena, header acceptance, ChainView, event bus | W3 | L | CS2, CS3, BS5 | M2 |
| P1 | `dash-p2p-transport` (lift #264, responder, gating, v1 fallback) | W5 | M | BS8a | M2 |
| P2 | `dashd-p2p` outbound core: ConnMan outbound, handshake parity, ping, addrman port, seeds, headers/headers2 sync | W5 | L | P1, CH1 | M2 |
| CH2 ★ | Layered coins cache, coins table, freeze/flush, crash recovery | W3 | L | ST1, CS5 | M3 |
| CH3 ★ | connect/disconnect, ActivateBestChain, reorg, invalidate/reconsider/precious, undo, assumevalid (Dash scope) | W3 | XL | CH1, CH2, CS1 | M3 |
| CH4 | IBD pipeline (prevalidate, prefetch, async writer) | W3 | L | CH3 | M3 |
| CH5 | reindex, reindex-chainstate, loadblock, verifychain | W3 | M | CH3, ST2 | M3 |
| P3 | Block download scheduler, stalling, compact-block client | W5 | L | P2, CH3 | M3 |
| MN0 | Minimal regtest miner (CreateNewBlock with empty mempool, CbTx fields, `generatetoaddress`/`generateblock`) | W6 | M | CH3 (EV for CbTx later) | M3 |
| RP1 | Blockchain/control/util/rawtx/hidden RPC set (incl. `setmocktime`, `waitfor*`, `syncwithvalidationinterfacequeue`) | W6 | L | AR1, CH3 | M3–M5 |
| EV1 ★ | Special-tx contextual checks (ProTx v1–v3 incl. shared 10–12, AssetLock/Unlock, MNHF, CbTx) | W4 | L | BS6, CS3 | M2–M3 |
| EV2 ★ | `MnList` (imbl), `build_new_list_from_block`, payee queue, PoSe, unique props, persistence + reference twin | W4 | XL | EV1 | M3–M4 |
| EV3 ★ | Payments, value and superblock hooks, credit pool, MNHF signals, shared-collateral covenants | W4 | L | EV2 | M4 |
| EV4 ★ | SML entry, `merkleRootMNList`, mnlistdiff builder (version-gated), evodb importer | W4 | M | EV2, BS7 | M4 |
| LQ1 ★ | Member selection (non-rotated, DIP24 quarters, skip lists), modifier, snapshots as outputs | W4 | XL | EV2, BS5 | M4 |
| LQ2 ★ | Commitment verify/verify_null, in-block counts and windows, mined index, `merkleRootQuorums` | W4 | L | LQ1, BS7 | M4 |
| LQ3 ★ | SignHash/request IDs, quorum selection, recsig/CL/IS verification, qrinfo builder | W4 | L | LQ2 | M4 |
| CH7 ★ | Evo/LLMQ integration into chainstate (special-tx order, BLS scheme context, snapshot side effects, `ChainHelper` CL/IS back ends, CL enforcement) | W3/W4 | XL | CH3, EV3, LQ3 | M4 |
| CH6 | Core datadir import (safe and fast) | W3 | M | CH7, EV4 | M4 |
| P4 | Inbound, eviction, banman, misbehavior table, serving (headers, blocks, merkleblock, BIP37 with Dash matching) | W5 | L | P3 | M5 |
| P5 | Tx relay, object request tracker, trickle, orphanage interplay | W5 | M | P4, MP1 | M5 |
| P6 | Overlay relay and serving (sporks, govsync non-MN, clsig/isdlock, dstx, mnlistdiff/qrinfo, cfilters, platformban) | W5 | L | P4, EV4, LQ3, IX1 | M5 |
| MP1 ★ | Mempool, policy, two-phase ATMP, Dash conflict maps, persistence, fee estimator | W6 | XL | CH7 | M5 |
| MN1 ★ | `BlockAssembler`, GBT, submitblock, template cache | W6 | L | MP1, GV1 | M5 |
| IX1 | Indexes (txindex, address/spent/timestamp, blockfilter Dash-BIP158, coinstats MuHash) | W6 | L | CH7 | M5 |
| ZQ1 | ZMQ, REST, notifications, statsd | W6 | M | CH7, MP1 | M5 |
| RP2 | Evo, quorum, masternode, governance and spork RPCs (read side) | W6 | L | CH7, GV1, SP1 | M5 |
| GV1 ★ | Governance runtime (objects, votes, sync, store), superblock oracle for `ChainHelper` | W9 | L | P6 | M5 |
| SP1 | Sporks + mnsync + netfulfilled + SyncManager tick | W9 | M | P4 | M5 |
| WL1 | key-wallet extensions: passphrase, imported keys and watch-only, encryption, MnemonicCompat | W8 | L | — | M5–M6 |
| WL2 | `dashd-wallet` service, SQLite, chain sync and rescans, core wallet RPCs | W8 | XL | WL1, MP1 | M6 |
| WL3 | Core wallet import (SQLite, BDB 4.8 reader, dump formats), `rdash-wallet` | W8 | L | WL2 | M6 |
| WL4 | PSBT, multisig, descriptors, message signing | W8 | L | WL2 | M6 |
| WL5 | ProTx and governance wallet ops (all types incl. shared), BLS/EdDSA signing | W8 | L | WL2, EV1 | M6–M7 |
| WL6 | CoinJoin client (rounds/salt, sessions, V24 rebalance) | W8 | XL | WL2, CJ0 | M6–M7 |
| CJ0 ★ | `dash-coinjoin` pure crate (shared validation, session state machines) | W9 | M | BS7 | M5 |
| AM1 | Active MN, MNAUTH, MN connection maintenance, platformban processing | W9 | L | P6, SP1 | M7 |
| P7 | MN connection API, quorum topology, probes | W5 | M | AM1 | M7 |
| QM1 | Quorum manager runtime, quorum DB, data recovery (qgetdata/qdata) | W9 | L | LQ3, P7 | M7 |
| LQ4 ★ | DKG sans-io session (all phases, BLS-IES, complaints, justifications, finalize) + in-process DKG tests | W4 | XL | LQ1 | M5–M7 |
| DK1 ★ | DKG runtime: phase handlers, intake and penalties, `dkgsimerror`, `quorum dkgstatus/dkginfo` | W9 | L | LQ4, QM1 | M7 |
| SG1 ★ | Signing manager, recsig DB, sig-shares manager (sessions, batching, recovery members) | W9 | XL | QM1 | M7 |
| LK1 ★ | ChainLock signer + handler parity; IS manager, DB and signer; EHF signer | W9 | L | SG1, MP1 | M7 |
| GS1 | Governance signer (trigger creation and voting) | W9 | M | GV1, AM1 | M7 |
| CJ1 | CoinJoin server runtime, DSTX manager, DSQ relay modes | W9 | L | CJ0, AM1, MP1 | M7 |
| PL1 | Platform RPCs (`quorum sign/platformsign/verify/getrecsig/...`, `getassetunlockstatuses`, `getquorumproofchain`, `verifychainlock/islock`) | W6 | L | SG1, LK1 | M7 |
| PF1–PF4 | B1 harness/baselines; per-milestone profiling passes; IBD pipeline tuning; GBT/mempool tuning | W10 | standing | — | M0→ |

### 5.3 Critical path

```
BS1 → BS3 → BS4 → BS5 ─┐   (BS8a → P1 → P2 feeds the M2 sync in parallel)
                  BS6 ─┼→ CH1 → CH3 → EV2 → LQ1 → LQ2/LQ3 → CH7 → [Done-1 @ M4]
CS1, CS2, CS3 ─────────┘    ↑ CH2
CH7 → MP1 → P4/P5/P6 → AM1 → P7 → QM1 → SG1 → LK1 ─┐
                         LQ4 (from M5) → DK1 ────────┴→ mixed-quorum interop (L6 mixed + devnet) → testnet MN → mainnet MN
```

- **Longest pole:** the MN overlay (DKG, sig shares, CL/IS signing) behind full-parity chainstate. LQ4 (sans-io DKG) is deliberately started in M5 so DK1 and SG1 can begin as soon as QM1 lands.
- **Schedule-risk items on the path, given extra review capacity:** CH3 (XL), EV2 (XL), LQ1 (XL), CH7 (XL), SG1 (XL).
- **Off the critical path:** wallet (M6), indexes, ZMQ, REST, perf work, BS13, the dashcore convergence track.

### 5.4 Parallel workstreams and staffing (6–10 concurrent implementers)

| Stream | M0–M1 | M2–M3 | M4 | M5 | M6–M7 | M8 |
|---|---|---|---|---|---|---|
| W1 Base SDK | 2 | 1 | 0.5 | 0.5 | 0.5 | — |
| W2 Consensus (pure) | 2 | 1 | 0.5 | — | — | — |
| W3 Chainstate and storage | 1 | 2 | 2 | 1 | 0.5 | 0.5 |
| W4 Evo/LLMQ (pure + integration) | — | 1 | 2 | 1 (LQ4) | 1 | — |
| W5 P2P | — | 1.5 | 1 | 2 | 1 | 0.5 |
| W6 Services (RPC, CLI, mempool, mining, indexes, ZMQ) | 1 | 1 | 1 | 2 | 1 | 1 |
| W7 Verification infra | 1 | 1 | 1.5 | 1.5 | 1 | 1 |
| W8 Wallet | — | — | — | 1 | 2 | 0.5 |
| W9 Masternode overlay | — | — | — | 1 | 3 | 1 |
| W10 Performance | (B1) | 0.5 | 0.5 | 1 | 1 | 1 |
| **Total implementers** | **7** | **9** | **9** | **10** | **10** | **6** |

Reviewers are on top of these numbers, per PROCESS.md: GPT primary, plus Opus for ★ items. The desktop-wallet track (D-series) is separate and consumes WL1 outputs.

### 5.5 Top risks

| Risk | Impact | Mitigation in this architecture |
|---|---|---|
| base-sdk upstream churn or divergence from the fork | rebase pain; drift | weekly rebase via script; upstream-shaped commits; one adaptation PR per bump (§1.8) |
| Codec-domain mismatch hidden in base-sdk | consensus or fingerprint divergence | Rule DDE + BS4 + L2 decode targets + BS2 full history |
| DML/DIP24 subtleties (ordering, side-effect snapshots, `ActiveAt` vs `ActiveAfter`) | fork | pure crates with reference twins; L4 full-history `mnlist_full_hash` + snapshot digests; windows around every fork height |
| State-dependent validity (CL, IS, governance, sync) | divergence under live conditions | `ChainHelper` injection; recorded decisions in replay; shadow mode L9 |
| Floating-point subsidy/KGW | historical mismatch | glibc-pow port + exhaustive historical sweep |
| BLS legacy semantics | quorum/commitment mismatch pre-V19 | dash-pkc bug-for-bug legacy; no batching for legacy; `diff_bls_*` with legacy decoding rules |
| Fingerprint drift | network distinguishability | single `fingerprint.rs` table; L7 transcript diff on every P2P PR |
| RocksDB miss on a metric | perf regression | M0 bake-off with MDBX fallback behind a narrow engine API |
| MN-overlay complexity (DKG, sig shares) | schedule | sans-io DKG early (M5), in-process multi-member tests, mixed-mode functional tests long before testnet |
| Upstream Core bugs that must be replicated (EMBARGO-1 and any found by L2) | divergence, or accidental disclosure | replicate bug-for-bug; keep embargoed details out of upstreamable commits (PLAN §7); follow upstream's fix in lockstep |
| Develop moves under us (V24 still TODO on mainnet) | rework | pin a Core commit per milestone; re-run R2's extraction scripts as a diff at each re-pin (R2 G.3 Q1); BS2/L4 catch format and rule drift |

---
## 6. Decisions table

| # | Decision | Choice | Main alternative | Why |
|---|---|---|---|---|
| D1 | Node type stack | base-sdk types and codecs (`dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, `dash-num`), wrapped by hash-caching node types | build on `dashcore` types | `dashcore` shapes are wrong for a node (eager payloads, witness/BIP144 vestiges, uncached txids). base-sdk is the program-mandated home of protocol shapes. Avoids churning the node through R3 Phase 3 |
| D2 | `dashcore` during the program | unchanged public API + `bridge` feature (byte round-trip); convergence is a separate post-M4 track | migrate `dashcore` onto base-sdk first | protects Platform (~2.1k uses), mobile and desktop; removes 20+ weeks from the critical path |
| D3 | Where rules live | new pure crates in rust-dashcore (`dash-consensus`, `dash-evo`, `dash-llmq`, `dash-governance`, `dash-coinjoin`), no_std-compatible | base-sdk crates | velocity (our review loop, edition 2024); base-sdk's charter is stateless parsing. Moving pure code later stays possible |
| D4 | base-sdk `Checkable` | never used by the node | reuse `Block::check()`/`Transaction::check()` | incomplete and divergent (no block-size check, mutation flag ignored, `Oversize` never checked, fixed 40k sigops) |
| D5 | Decode domain | Rule DDE: decoders accept exactly Core's domain; BS4 fixes base-sdk | validate ranges in decoders | decode-vs-rule failures differ observably (index failure marks, peer scores, fingerprint) |
| D6 | base-sdk consumption | local fork branch `node/base-sdk`, workspace `[patch]` → `file://` git rev, weekly rebase, upstream-shaped commits | path deps; wait for upstream merges | reproducible from every worktree; no external queue; keeps the upstreaming option open |
| D7 | Base SDK gate for node work | chainstate integration waits for BS3–BS6, BS10–BS12; the transport waits for BS8a; P2P serving and overlays wait for BS7 and BS8b; pure-rule, storage, RPC and test work starts at M0 | finish all of R3 first | maximizes parallelism; the real blockers are params and v24 payloads |
| D8 | bincode | not used by the node; `dashcore` newtypes keep it | re-add bincode to base-sdk | preserves kwvg's decision; avoids the orphan-rule problem; node formats are Core encodings |
| D9 | Hash display | bytes canonical (internal LE); node RPC uses Core `GetHex` rules (base-sdk `Display`) | per-type dashcore display | RPC parity is fixed by Core; dashcore FFI display is irrelevant to the node |
| D10 | Network / chain params | `NetworkKind {Mainnet, Testnet, Devnet, Regtest}` + `ChainParamsBuilder` with every Core override; `Arc<ChainParams>` passed explicitly | dashcore `Network` + global params | Devnet and regtest overrides are needed for functional tests; globals are a known Core bug source |
| D11 | v24 payloads | canonical implementation in base-sdk (BS6); upstream dashcore PRs land independently and BS2 proves equivalence | implement in `dashcore` | one home for shapes; testnet V24 already started |
| D12 | BlockHash footgun | X11 unconditional in `dashcore` (BS11); node immune by construction | keep the feature | correctness; bridges can never disagree |
| D13 | Crate layering | sans-io pure `dash-*` + runtime `dashd-*` + `rdash*` binaries | monolithic node crate | fuzzability, differential testing, compile parallelism, clear ownership for 6–10 implementers |
| D14 | Storage engine | **RocksDB** (CFs, atomic WriteBatch), gated by an M0 bake-off against MDBX | MDBX | disk-footprint metric (random-hash keys favour LSM), LevelDB-equivalent write path (no regression by construction), precedent (Kaspa, electrs); MDBX is the fallback |
| D15 | Block storage | Core-format `blk`/`rev` flat files, byte-identical | blocks inside the DB; compressed files | hardlink import of Core datadirs, `-loadblock` compatibility, undo-record diffing; compression can't reach the 2× disk target anyway |
| D16 | Chainstate atomicity | one WriteBatch for coins + block index + evo + mined commitments + meta | Core's separate LevelDBs | removes the "evodb mismatch → reindex" failure class |
| D17 | Evo persistence | own format (per-block diffs, sorted snapshots every 576); Core evodb is import-only | byte-compatible evodb writes | Core snapshots depend on immer HAMT iteration order |
| D18 | Concurrency | one validation thread (single writer) + `ArcSwap` snapshots + rayon pools (prevalidate, script, BLS) + tokio for network and RPC | a global `cs_main` lock | removes lock contention and serialization; deterministic consensus path |
| D19 | UTXO cache | packed coins (Core compression in memory), layered freeze/flush, prefetch stage | Core-style cache with synchronous flush | about 1.5× denser; no IBD stalls on flush; misses moved off the critical thread; flush writes the cached bytes as-is |
| D20 | Payee selection | ordered payee queue O(log n) + reference twin + xcheck | linear scan per block | IBD and connect latency; equivalence proven continuously |
| D21 | BLS | `dash-pkc` (blst; legacy bug-for-bug), batch verification for basic scheme only, MSM for recovery and DKG | FFI to dashbls / relic | already integrated, faster, legacy semantics covered; batching is unsound for unchecked legacy points |
| D22 | Floating point | glibc `pow` port + exhaustive historical sweep | `f64::powf` / embedded tables | platform independence; regtest still needs live evaluation |
| D23 | P2P stack | new `dashd-p2p`; #264 transport lifted into `dash-p2p-transport`; independent of #902 | extend dash-spv's network layer | dash-spv is outbound-only and SPV-shaped; avoids conflicts with an active upstream rewrite |
| D24 | Fingerprint | Core-faithful behaviour as one data table + transcript-diff gate | best effort | charter requirement; drift is otherwise inevitable |
| D25 | Overlay integration | `NetHandler` trait registry mirroring Core's handler chain | logic inside net_processing | modularity; matches Core's structure for porting |
| D26 | RPC server | hyper + `dash-rpc-spec` registry; JSON-RPC 1.x; Core help verbatim; 8-decimal amounts | jsonrpsee / axum | Core semantics (sub-command squashing, whitelist, queues, warmup) don't fit JSON-RPC 2.0 frameworks |
| D27 | CLI | generic client driven by the spec's conversion table; works against dashd too | typed `dashcore-rpc` client | covers all 289 commands from day one; operators can adopt it early |
| D28 | Wallet | extend key-wallet / key-wallet-manager; `dashd-wallet` with SQLite; Core import one-way | a new wallet; KV store; two-way compatibility | brief mandate; single-file wallets give Core backup semantics; Core's default wallets are SQLite too |
| D29 | Datadir layout | Core per-network naming; consensus state under `blocks/` + `chainstate/`; `llmq/`; `indexes/` | free layout | the functional-test cache copies exactly these directories |
| D30 | Differential oracle | `libdash_oracle` C ABI from `node/oracle-shim` (real Core objects, no logic in the shim) via `dashcore-oracle-sys`; libdashconsensus as a week-1 stopgap | RPC-only oracles | in-process differential fuzzing needs speed and full domain coverage |
| D31 | Consensus parity proof | per-block `StateDigest` (`-statedigestlog` incl. `CanonicalMNListHash`, plus R4 RPC golden extraction) + recorded state decisions; replays with `-assumevalid=0`; windowed replays per PR; L3m mutation testing for rejection parity | spot checks with `gettxoutsetinfo` | catches the first divergent block and field, incl. MN-list state the SML omits; replay proves acceptance, mutation proves rejection |
| D32 | Functional tests | run Core's suite unmodified against rdashd; per-node binary selection for mixed networks (dashd node 0 until our wallet lands) | port tests to Rust | cheapest end-to-end parity proof; mixed-quorum interop comes for free |
| D33 | Consensus optimisations | dual path (reference twin) + `-consensusxcheck` in replay and fuzz | review only | never trade correctness for speed (charter) |
| D34 | Allocator | mimalloc for binaries | glibc malloc | RSS and fragmentation |
| D35 | Core baseline | **per-network pinned dashd release** as the parity reference (R4 §0.2): mainnet v23.1.x, testnet v24.0.0 (rc → final); develop watched weekly. Rules are implemented from develop and gated by deployments (V24, evo_shares) | one develop pin for everything | current dashd relaxes all historic rules; testnet V24 and develop deltas (e.g. the V24 unlock limit, evo_shares) differ by branch |
| D36 | Binaries | `rdashd`, `rdash-cli`, `rdash-wallet` (pending Q3) | `dashd` names | must coexist with Core installations |
| D37 | Reproducibility | commit `Cargo.lock` on `node/integration` | no lockfile (current) | binary product; deterministic benchmarks |
| D38 | Logging | `tracing` + Core-format `debug.log`; asserted log lines reproduced verbatim | free-form logs | functional tests `assert_debug_log`; operator familiarity |
| D39 | Light-client serving | BIP37 (with Dash special-tx matching), BIP157/158 (Dash-extended), headers2, mnlistdiff/qrinfo | serve a subset | mobile (dashj bloom) and dash-spv (filters) both need it (R5 §4) |
| D40 | txindex format | 8-byte txid prefix keys + verification read | Core full-key layout | about 3× smaller; identical RPC behaviour |
| D41 | Canonical tx bytes | txid, merkle leaves, block storage and relay use **Core's re-serialization**. Wire bytes are used only when provably identical (fast path) | hash wire bytes | Core hashes `SerializeHash(*this)` |
| D42 | Fingerprint target | `ProtocolProfile` per network, following the pinned release (mainnet 70240 `/Dash Core:23.1.x/`, testnet 70242) | one profile (develop) | indistinguishable means matching what that network actually runs |
| D43 | MN-list snapshot format | Core's canonical `EvoSnapshot` MN-list encoding + `CanonicalMNListHash` | custom sorted format | direct digest comparability; future snapshot-sync interop (R4 F8) |
| D44 | Quirks | R4 §1.15 quirk register is normative; one named test per quirk | case-by-case | historical blocks exercise them; "tightening" forks the node |

---

## 7. Open questions for pasta

Only product or hard-to-reverse decisions are listed. Everything else above is decided, with a default the program proceeds on.

1. **Default user agent.** Indistinguishability (charter) points at advertising **Core's exact UA** (`/Dash Core:24.x.y/`) plus Core's version numbers in `getnetworkinfo`. That is effectively impersonation, and it hides rdashd's share of the network from Core developers' telemetry.
   - (a) Core UA by default (recommended to satisfy the charter); `-uaoverride=rdash` available.
   - (b) An honest UA by default (trivially distinguishable).
   - (c) Core UA plus an identifying `-uacomment`.
   - Needed before any public-network connection (M2 mainnet header sync).
2. **License of the node crates.** The workspace is CC0-1.0, but the node crates are line-by-line ports of Dash Core (MIT), and derivative works carry MIT's notice requirement.
   - (a) MIT for `dash-consensus`/`dash-evo`/`dash-llmq`/`dash-governance`/`dash-coinjoin`/`dashd-*`/`rdash*`, keeping CC0 elsewhere (recommended).
   - (b) Relicense the workspace.
   - (c) CC0 with an attribution NOTICE.
   - This is hard to reverse once anything is published.
3. **Product names and datadir.** Binaries `rdashd`/`rdash-cli`/`rdash-wallet` and the default datadir `~/.rdashcore` (macOS `RDashCore`, Windows `%APPDATA%\RDashCore`). Confirm or rename before M5, when operators first see them.
4. **base-sdk posture.**
   - Is our base-sdk fork meant to be offered upstream to kwvg as PR-sized series (recommended; commits are already shaped for it)? Exception: anything under PLAN §7 handling (EMBARGO-1) stays local until pasta clears disclosure.
   - Or is it a permanent program fork?
   - Separately: should the `dashcore` convergence track (Platform-visible breaking changes) ever be proposed upstream?
   - Both involve upstream maintainers, so they are escalation items per PROCESS.md.
5. **Mainnet masternode resources (M8).**
   - Collateral (1000 DASH regular or 4000 DASH evonode), operator and owner key custody, hosting and a public IP for the mainnet MN.
   - Whether a mainnet *evonode* (Platform services) is in scope, or a regular MN suffices for Done-2.
   - Testnet can use the faucet (tDASH).
6. **Parity target and pins.** Parity is claimed against **pinned, tagged Core releases per network**, aligned with R4's open question 2:
   - mainnet v23.1.x today;
   - testnet v24.0.0 (rc → final);
   - develop watched weekly.

   Confirm the pins and when mainnet moves to v24. Also confirm that the public claim reads "rdashd X ≡ Dash Core vY on network Z".
7. **Desktop app ↔ node relationship.** Default: the desktop app stays SPV / platform-wallet (R5: DET is already SPV-only) and can optionally use a local rdashd as a trusted P2P peer. That needs no new API; the node serves BIP157/37, headers2 and mnlistdiff/qrinfo. The alternative is that the desktop app embeds or launches rdashd and uses `dashd-wallet` over RPC. That changes the wallet API surface, so a direction is needed before M6.

---

## Appendix A. Core → crate map (most-used files)

| Core | Rust |
|---|---|
| `validation.cpp` ConnectBlock/DisconnectBlock/ActivateBestChain/AcceptBlock(Header)/ProcessNewBlock | `dashd-chainstate::{connect,disconnect,activate,accept}` (+ pure rules in `dash-consensus::{tx,block,header}`) |
| `validation.cpp` MemPoolAccept | `dashd-mempool::accept` |
| `consensus/tx_check.cpp`, `tx_verify.cpp` | `dash-consensus::tx` |
| `script/interpreter.cpp`, `script.*`, `sigcache.*` | `dash-consensus::script`, `dashd-chainstate::sigcache` |
| `pow.cpp`, `chain.*` (work), `versionbits.*`, `deploymentstatus.*` | `dash-consensus::{pow, deployments}` |
| `coins.*`, `txdb.*`, `undo.h`, `compressor.*` | `dash-consensus::coins` (codecs), `dashd-chainstate::coins` (cache), `dashd-store` |
| `node/blockstorage.*`, `flatfile.*` | `dashd-store::flatfile`, `dashd-chainstate::blockstore` |
| `evo/specialtxman.cpp`, `providertx*`, `assetlocktx.*`, `mnhftx.*`, `cbtx.*`, `creditpool.*`, `deterministicmns.*`, `dmnstate.*`, `simplifiedmns.*`, `smldiff.*`, `netinfo.*`, `sharedcollateral.h` | `dash-evo::{special, mnlist, payments, cbtx, creditpool, mnhf, sml, netinfo, shared}` + `dashd-chainstate::evo` |
| `masternode/payments.*` | `dash-evo::payments` |
| `llmq/params.h` | `dash-params` (BS5) + `dash-llmq::params` |
| `llmq/utils.cpp`, `snapshot.*`, `commitment.*`, `blockprocessor.*` | `dash-llmq::{members, snapshot, commitment}` + `dashd-chainstate::quorum_blocks` |
| `llmq/quorums*`, `signing*`, `signing_shares*`, `net_*`, `dkgsession*`, `active/dkg*`, `ehf_signals*` | `dash-llmq::{signing, dkg}` + `dashd-quorums` |
| `chainlock/*`, `instantsend/*` | `dash-llmq::{clsig, islock}` + `dashd-locks` |
| `governance/*` | `dash-governance` + `dashd-governance` |
| `coinjoin/*`, `wallet/coinjoin.*` | `dash-coinjoin` + `dashd-coinjoin` + `dashd-wallet::coinjoin` |
| `spork.*` | `dashd-sporks` |
| `active/masternode.*`, `evo/mnauth.*`, `masternode/{sync,meta,utils}.*`, `node/sync_manager.*`, `netfulfilledman.*` | `dashd-masternode` |
| `net.*`, `net_processing.*`, `addrman*`, `banman.*`, `txrequest.*`, `blockencodings.*`, `node/eviction.*`, `bip324.*`, `protocol.*` | `dash-p2p-transport` + `dashd-p2p` (+ wire types in `dash-p2p-core`) |
| `txmempool.*`, `policy/*`, `txorphanage.*`, `kernel/mempool_persist.*` | `dashd-mempool` |
| `node/miner.*`, `rpc/mining.cpp` | `dashd-mining` + `dashd-rpc::mining` |
| `index/*`, `blockfilter.*`, `evo/specialtx_filter.*`, `kernel/coinstats.*` | `dashd-indexes` |
| `wallet/*` | key-wallet / key-wallet-manager (engine) + `dashd-wallet` (service) |
| `rpc/*`, `httprpc.*`, `httpserver.*`, `rest.cpp`, `rpc/client.cpp` | `dash-rpc-spec` + `dashd-rpc` |
| `zmq/*` | `dashd-zmq` |
| `init.cpp`, `common/args.*`, `logging.*`, `scheduler.*`, `stats/*` | `dashd-node` + `dash-args` |
| `bitcoin-cli.cpp`, `bitcoin-wallet.cpp` | `rdash-cli`, `rdash-wallet` |

## Appendix B. Deliberately deferred or out of scope (with landing point)

| Item | Status | Lands |
|---|---|---|
| Tor (incl. onion service publishing), I2P, CJDNS | the transport abstraction allows proxies; ADD_ONION control port; SAM 3.1 | M5+ (P1 priority per R2 US-NODE-07) |
| Pruning (`-prune`, `pruneblockchain`) | Core semantics incl. incompatibilities (txindex, addressindex, spentindex, governance) | M5 |
| UPnP / NAT-PMP | off by default in Core | M8 (P1) |
| assumeutxo / `loadtxoutset` | not usable in Dash today (no Dash payload; R5 §3.5) | follow Core |
| statsd metrics | Core metric names | M5–M8 (P2) |
| `rdash-tx`, `dash-util grind` | `bitcoin-util-test.json` vectors | M8 (P1/P2) |
| External signer (`-signer`) | HWI protocol | M8 (P2) |
| Qt GUI | not in scope; the desktop app is the UI | — |
| Erlay (`sendtxrcncl`) | off by default in Core; never sent | follow Core |

## Appendix C. Conventions carried into every task brief

- Every ported function has a `// Core: <file>:<line> <Function>` anchor, pinned to the milestone's Core commit.
- Every consensus PR (★) lists its Core vectors (L1), its differential targets (L2) and its replay windows (L4) in the PR description. It also states whether a reference twin exists.
- Reject reasons and RPC error strings are copied from Core verbatim and are never paraphrased.
- No `HashMap` iteration on consensus paths. No floating point outside `dash_consensus::fp`. No `std::time` outside `NodeClock`.
- base-sdk PRs follow base-sdk conventions (`namespace%type(scope):` commits, 2-space style, `just test lint`, no `unwrap`/`panic`/`unsafe` without a reasoned `#[expect]`).
- No changes under `dash-spv/src/network/` (avoids #902 conflicts) and no public-API breaks in `dashcore`, `key-wallet` or `key-wallet-manager` without a note on consumer impact.

# A1: Architecture of the Rust Dash node (reconciled, authoritative)

> **DEC-82 (2026-10-09): v24 only.** §2.25 is rewritten, and `ReleaseProfile`, `UngatedRules`, `Release::V23_1` and `-releaseprofile` no longer exist (RP-V24). Where the rest of this document names a per-release value, a profile group or a v23.1.8 behaviour, read the v24 behaviour; the O17 rows remain the record of the difference.

| | |
|---|---|
| Status | **Authoritative, revision 1** (A1-rev1, after design review A1r round 1: `reviews/A1r-r1-gpt.md` REWORK, `reviews/A1r-r1-opus.md` APPROVE-WITH-CHANGES). Every finding's disposition is in `designs/A1-rev1-dispositions.md`. Supersedes `A1-architecture-fable.md` (Fable 5.1) and `A1-architecture-opus.md` (Opus 5.5); both are kept for history |
| Date | 2026-10-08 (rev1) |
| Author | A1 reconcile and rev1 (Opus 5.5, design sub-agent) |
| Binding inputs | `CHARTER.md`; `DECISIONS.md` (DEC-01…DEC-17, these override any open question in the two designs); `PRD.md` (story `US-*`, requirement `FR-*`/`NFR-*`, release `REL-*` IDs); research R1–R5, with **R4 and `R4-appendix/*` as the consensus ground truth** |
| Code verified at | rust-dashcore `dev` `67b307c6` and `node/integration` `64c28628`, plus `node/st1-store-bakeoff` `73f709da` (the `dashd-store` API) and the task worktrees; base-sdk `develop` `33fcc59`; Dash Core `develop` `15db478750` and tags `v23.1.8`, `v24.0.0-rc.3`. Rev1 re-verified every disputed claim against these trees; Core BLS decode outcomes were probed by compiling Core's wrapper against the oracle branch's dashbls/relic |
| Companion | `PLAN.md`: milestones, the task graph, Wave 1/1b, standing workstreams, and the upstream-sync and embargo procedures |

> **EMBARGO-1.** This document refers to an unfixed Dash Core security issue only by its codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. Per DEC-09 and DEC-14, nothing from `node/integration`, `node/base-sdk` or the Dash Core `node/*` branches is pushed, and no upstream PR or public artifact may reveal the issue. PLAN §7 defines who may read the private file and how affected code is marked.

**How to read.** §0 is the design on one page. §1 covers the type stack, the decode domain (including BLS, §1.4.1) and the Base SDK milestone. §2 covers crates, interfaces, on-disk layout, the effect channel (§2.10.1) and per-network release pinning (§2.25). §3 covers concurrency, the durability and recovery protocol (§3.5) and performance, with the declared shortfalls. §4 is verification. §5 summarizes the sequencing (PLAN.md has the details). §6 is the decisions table, with the choice, the rejected alternative, whose position that was, and why. §7 lists the PRD errata this design implies. §8 holds the open questions for pasta. The appendices hold key signatures, the Core→crate map, the consolidated on-disk layout, the functional-test adoption order and conventions.

**Notation.** `rdc:` rust-dashcore, `bsdk:` base-sdk (`pkgs/`), `core:` Dash Core (`src/`). **F** = Fable design, **O** = Opus design. `★` marks consensus-critical code: two reviewers, the oracle differential test and replay gates (PROCESS.md, NFR-CEQ-12). "rev1" marks text changed by this revision; review finding IDs are GPT `F01`–`F20` and Opus `B-1`, `M-1`…`M-11`, `m-1`…`m-21`, `X-1`…`X-6`.

---

## 0. The design on one page

1. **Type stack.**
   - Node crates use **base-sdk** types: `dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, `dash-num`, `dash-pow`, `dash-script` and `dash-types`. They come from a local fork branch, **`node/base-sdk`**, in `~/workspace/base-sdk` (DEC-09), consumed through a workspace `[patch]` that points at a pinned `file://` git revision.
   - `dashcore` stays the type system of SPV, the wallet, FFI and Platform. Its public API is unchanged; it gains a feature-gated bridge module (BS14, M6).
   - The **BS2 conformance harness** (dashcore ⇄ base-sdk ⇄ dashd, full mainnet and testnet history) proves the two codecs equal.
   - Converging `dashcore` onto base-sdk is a separate, non-blocking track (§1.8).
2. **base-sdk supplies shapes and codecs, never rules.** Its `Checkable` impls are incomplete and divergent: the block check ignores the merkle-mutation flag, and `TxInvalid::Oversize` is never raised (both verified). Every rule lives in our pure crates, with Core's reject strings.
3. **Decode-domain equivalence (Rule DDE, §1.4).** For every type the node decodes from untrusted bytes, base-sdk must accept exactly what Core's handler accepts. The decode domain is **per command and per state**, not per type (§2.14, F14).
   - **Divergences verified in base-sdk 33fcc59:**
     - `TxOut` values above `MAX_MONEY` are rejected at decode;
     - `bool` accepts only 0 and 1;
     - payloads are capped at 10,240 bytes;
     - `LlmqType` lacks 103 and 105;
     - unknown P2P commands are an error;
     - `version` makes Core's optional trailing fields mandatory;
     - empty-payload messages reject any payload;
     - **every BLS field** is a byte bag where Core decodes it eagerly with a three-outcome rule (§1.4.1, B-1).
   - **Fix:** BS4a (shapes), BS11a-t/BS11a (BLS decode) and BS6a (BLS field typing) fix these in the fork.
   - **Hashing and storage:** hashes, merkle leaves, sighash, block storage and relay always use **Core's re-serialization** of a decoded object. For consensus objects, `dash_consensus::core_serialize`, and for an LLMQ final commitment `write_commitment`/`commitment_hash` (DEC-356), are the only encoders (§1.3). Anything under PLAN §7 handling lives in `dash-consensus`, never in base-sdk.
4. **The Base SDK milestone** (§1.7).
   - BS1 is merged. BS2 is nearly done. BS3 is running. V24a is the running `dashcore` v24 task; it replaces v1's BS17.
   - Gates, as in §1.7:
     - **node consensus:** BS3, BS4a, BS5, BS6a and BS11a-t;
     - **P2P transport:** BS7; **P2P serving:** BS8 + BS10;
     - **LLMQ verification (M4):** BS8 + BS11a + BS11b + BS12; **masternode overlay (M7):** BS9a + BS11b + BS12;
     - **wallet seam (M6):** BS14, which needs BS15 and V24a.
   - BS13 (performance), BS15 and BS16 must be merged for the M1 exit, but none of them gates a later milestone's work. BS18 (SPV correctness) never gates a node milestone.
5. **Crates (§2).** There are 30 crates: 29 workspace members plus the workspace-excluded `node-fuzz` (Opus X-5 merged `dashd-sporks` into `dashd-masternode` and `dashd-zmq` into `dashd-rpc`).
   - **Five pure, sans-io crates:** `dash-consensus`, `dash-evo`, `dash-llmq`, `dash-governance` and `dash-coinjoin`. `dash-evo` holds the DML, special txs, CbTx, credit pool, MNHF, payments, SML and LLMQ *consensus* (member selection, commitments, scan/select, signature verification); `dash-llmq` holds the DKG and sig-share protocol math.
     - These crates have no I/O, clock, thread or global dependencies, enforced by `cargo deny` and clippy lists.
     - They are **not** `no_std` (X-1): no consumer needs it, and it conflicted with A1's own `OnceLock`-based types.
   - **Fifteen daemon runtime crates:** `dashd-store`, `dashd-chainstate`, `dashd-mempool`, `dashd-mining`, `dashd-p2p`, `dashd-masternode` (with sporks), `dashd-quorums`, `dashd-locks`, `dashd-governance`, `dashd-coinjoin`, `dashd-indexes`, `dashd-wallet`, `dashd-rpc` (with REST and, behind a feature, ZMQ) and `dashd-node`, plus the reusable `dash-p2p-transport`.
   - **Shared libraries:** `dash-args`, `dash-rpc-spec` and `dash-coreimport`.
   - **Binaries (DEC-03):** `rdashd`, `rdash-cli` and `rdash-wallet`.
   - **Test and tooling:** `dash-conformance` (exists, from BS2), `dash-oracle-sys`, `dash-testkit`, `node-fuzz`, and B1's `node-bench` (Python).
   - New crates that port Core logic are **MIT**; existing crates stay CC0 (DEC-07).
6. **Storage (§2.8–2.9, §3.5).**
   - **Engine.** RocksDB sits behind ST1's narrow engine trait (`write(batch, Commit::{Buffered, Durable})`, `sync_wal`, `snapshot`).
   - **One consensus database.** A single `chainstate/` database with column families holds all consensus state: coins, block index, MN-list diffs and snapshots, mined commitments, rotation snapshots, credit pool, MNHF and meta.
   - **Durability (rev1, §3.5).**
     - Files are made durable first (`fdatasync` of every touched `blk`/`rev`, plus a directory `fsync`).
     - Then one Durable batch carries the block index, **all evo state** and the `head_blocks` marker.
     - Then coin sub-batches, ending in a Durable tip batch.
     - Recovery is a coins-only `ReplayBlocks`. Every block above the recovered tip is connected through **ordinary validation**.
     - Power-loss behaviour is tested with fault-injecting engines, `dm-log-writes` prefix replay and VM crashes (PLAN ST3a/ST3b), not only process kills.
   - **Other directories.** LLMQ runtime state lives in `llmq/` and each index in `indexes/<name>/`; these names match what Core's functional framework copies and caches.
   - **Formats.** Block data uses **Core-identical `blk*.dat`/`rev*.dat`**; everything else is our own versioned format (DEC-11). Core's chainstate, evodb, flat files and wallets are **import-only**.
7. **Concurrency (§3).**
   - **Validation:** one thread owns all mutable chain state. Readers see coherent views (§2.10.2): the `ChainView` tip summary, seqlocked index entries, and ancestry-pinned `EvoReadView`s.
   - **CPU work:** rayon pools for prevalidation and prefetch, script checks and BLS, under one compute budget (§3.1).
   - **I/O:** tokio for sockets, RPC, ZMQ and timers. Per-connection tasks do transport work only (rev1, F14).
   - **Peer management:** one **`PeerManager` actor** reproduces Core's `ThreadMessageHandler` (rev1):
     - a shuffled, wake-driven loop, with a 100 ms throttle on masternode connections only (F13);
     - decoding in the actor after Core's state gates (F14);
     - the actor **awaits** every message whose effects later messages can observe (M-5).
     Parallelism lives inside a message and in Core's own asynchronous edges, never between messages.
   - **State-dependent validity:** ChainLocks, InstantSend, governance, MN sync and the clock enter through an injected **`ChainHelper`**, as in Core's `CChainstateHelper`.
   - **Non-transactional effects** (rotation snapshots, IS-lock removal, mineable-pool erasure, platform-ban resets) travel on an **effect channel** that survives `Err` and proposal mode (rev1, §2.10.1, F04).
8. **Performance (§3.9, rev1).**
   - **Sources of the 2× target:** pipelining, prefetch with an epoch fence, a coins cache under one memory budget, O(log n) payee selection, cached SML hashes, basic-scheme BLS batching and lock-free reads.
   - **Already in Core (F11), so counted only as parity:** the script-execution cache, the ancestor-score index, the GBT template cache, immediate ChainLock enforcement, persistent MN-list maps and parallel QC signature checks.
   - **Targets are hypotheses until PF1 measures them,** under an Amdahl budget.
   - **Safety:** every optimisation of consensus code keeps a line-by-line **reference twin** and a `-consensusxcheck` mode.
   - **Evidence (M-11):**
     - per-PR evidence on the shared box is **deterministic counters**: instructions retired, allocations, syscalls;
     - wall-clock side-by-side runs are for milestone reports, in reserved windows or on a dedicated host.
   - **Declared shortfalls:**
     - ECDSA throughput per core: parity, since both use the same libsecp256k1.
     - Disk footprint: at most parity, since block files dominate and stay Core-identical.
     - DKG, recovered-signature, InstantSend, ChainLock and CoinJoin wall clock: bound by the protocol and the network, with externally visible outputs kept on Core's ticks (M-6). End to end ≈ 1×; only node-local CPU can reach 2×.
     - RSS: ½ is unlikely, because the budget is set by configuration.
     - IBD over the internet: network-bound, and 16 blocks in flight per peer are kept for fingerprint parity.
     - X11: parity until `dash-pow` gains AES-NI dispatch.
9. **Verification (§4).** We use the PRD/R4 layers **L0–L6**:
   - **L0:** Core vectors.
   - **L1:** BS2 codec equality, plus in-process differential fuzzing against **`libdash_oracle`**, a C ABI over Core's real objects with **reviewed adapter logic** (M-7):
     - **O2a** covers stateless and contextual entry points, with a coverage matrix (F15);
     - **O2b `diff_chain`** links a Core regtest chainstate into the shim and fuzzes block sequences, failures and reorgs against `dashd-chainstate`, comparing verdicts, state and **surviving effects** after every step.
   - **L2:** full-chain replay with a per-block **StateDigest v1**. Mainnet and testnet goldens and `-statedigestlog` digests come from the **v24 oracle**; there is no v23.1.8 reference (DEC-82, §2.25).
   - **L3:** mutation testing for rejection parity, through GBT proposal mode and `testmempoolaccept`.
   - **L4:** Dash's 266 functional tests (distinct script stems at v24.0.0-rc.3; R4 counted 264 before it) against `rdashd`, per pinned release.
   - **L5:** mixed dashd/Rust masternode networks (regtest, a devnet soak, testnet), plus shadow mode.
   - **L6:** the fingerprint lab (31 behaviours, KS timer tests, classifier AUC ≤ 0.55).
   - **Cross-cutting:** DIFF-RPC, XCLI, SPV-as-consumer (a smoke test), BENCH and crash safety (ST3a/b). R4's quirk register (34 entries, quirk 1 being the private one) plus rev1's quirks 35–38 is normative: one named test per quirk, 38 in all.
10. **P2P identity and release pinning (DEC-04, DEC-82, §2.25).** Every network targets v24.0.0-rc.3 alone. Rev1's per-network **`ReleaseProfile`** (DEC-05) is gone; O17 (`O17-pin-delta.md`, `O17-release-profile.md`) stays as history of the v23.1.8 differences.
    - mainnet: v23.1.8, protocol 70240, `/Dash Core:23.1.8/`;
    - testnet, devnets and regtest: v24.0.x, protocol 70242, `/Dash Core:24.0.0/`. Regtest can select the v23.1.8 profile for tests.
    - `-honestua` and `-uacomment` are available. The real identity is reported by `-version`, the `debug.log` header and a hidden non-Core RPC; `getnetworkinfo` gets no extra field.
11. **Wallet (DEC-02, DEC-11).**
    - `dashd-wallet` hosts `key-wallet-manager` with SQLite in our own schema.
    - Descriptor wallets are the default; legacy-HD semantics are kept for imported legacy wallets.
    - Encryption is argon2id + XChaCha20-Poly1305.
    - Core's crypter and BDB/SQLite readers are import-only, and export tools provide the way back.
12. **Sequencing (§5, PLAN.md; rev1 re-baselined with design and review latency).**
    - **Milestones:** M0 Foundations (≈ wk 7) → M1 Base SDK (≈ wk 13) → M2 Headers-first sync (≈ wk 18) → M3 Full validation, non-evo (≈ wk 23) → M4 Evo/LLMQ consensus with full parity (**Done-1 by replay**, ≈ wk 30–35) → M5 Full-node services (≈ wk 44) → M6 Wallet and migration (≈ wk 46) → M7 Masternode on testnet (**Done-2 on testnet**, ≈ wk 52) → M8 Release (≈ wk 58).
    - **Decoupled masternode track (M-9a):** the overlay tasks gate on regtest-correct evo/LLMQ (CH7b plus regtest L4 and `diff_chain`), not on the full mainnet replay campaign.
    - **Critical path (computed):** BS3 → BS4a → BS6a → EV2a → LQ1a → LQ1b → LQ2 → LQ3 → CH7b. From there it forks into the REL-1.1 replay (Done-1) and two **co-critical** masternode branches, QM1a → SG1a → SG1b and MP1a → LK1b → LK1c. These join at the mixed-quorum harness (O12b), followed by the devnet soak, the 30-day testnet soak and the mainnet pilot.
    - **Near-critical** (≤ 1.5 weeks of slack): BS11a-t, CS0, EV0, EV1, EV2b, EV3, CS3, CH7a, and M5's P5 → O13b.

---

## 1. Type stack

### 1.1 Decision and layer ownership

Both designs reached the same core decision: **node crates build on base-sdk types; `dashcore` is a compatibility layer for the wallet, SPV and Platform, kept equal by BS2 and converged later.**

| Shape | Defined in (owner) | Node uses | Wallet / SPV / Platform use | Bridge |
|---|---|---|---|---|
| `Transaction`, `TxIn`, `TxOut`, `OutPoint`, `Block`, `BlockHeader`, `TxType`, special payloads (13 types, all versions) | `bsdk: dash-primitives` (fork `node/base-sdk`) | through the hash-caching wrappers `dash_consensus::{Tx, Block}` (§1.3) | `dashcore::{Transaction, Block, …}` (today's structs; later thin wrappers, CV3/CV4) | `dashcore::bridge` (BS14): byte round trip, proven by BS2 |
| Hash newtypes (`TxHash`, `BlockHash`, `QuorumHash`, `MerkleRoot`, `ProTxHash`, …) | `bsdk: dash-num::make_hash!` + primitives | directly | `dashcore::{Txid, BlockHash, …}` (`bitcoin_hashes` 0.14 newtypes with bincode) | byte-identical `From` in `dashcore::bridge`; display direction documented per type |
| 256-bit arithmetic, compact target | `bsdk: dash-num::{Arith256, CompactTarget}` | directly (chain work is `Arith256`) | `dashcore::pow::{Work, Target}` (backed by `Arith256` since BS1) | none needed |
| Script bytes, opcodes, address classification | `bsdk: dash-script` + tagged `ScriptPubKeyBuf`/`ScriptSigBuf` | bytes and opcode table only | `dashcore::ScriptBuf` | `From` (bytes) |
| Script **interpreter**, sighash, sigops, tx/block/header rules, PoW, subsidy, payments, versionbits | `rdc: dash-consensus` (new, MIT) | directly | wallet signing keeps `dashcore::sighash`, cross-checked by the same vectors | n/a |
| Chain/consensus params, genesis, devnet genesis, checkpoints, LLMQ tables, spork keys, overrides | `bsdk: dash-params` (BS5) | `Arc<ChainParams>` threaded explicitly | `dash-network::Network` maps to `NetworkKind` (BS16; `FFINetwork` discriminants kept) | trivial |
| P2P message types, inventory, short IDs, V1 framing, `ProtoCtx` | `bsdk: dash-p2p-core` (BS7–BS10) | directly | `dashcore::network::NetworkMessage` stays for dash-spv until CV4 | message-level `From` only where both sides need it |
| BLS / ECDSA / EdDSA | `bsdk: dash-pkc` (BS11a/b parity fixes) | the scheme is an **explicit parameter**, never global | `dashcore-crypto` wrappers (runtime `BlsScheme`, bincode) | exists |
| X11 | `bsdk: dash-pow` (C `rs-x11-hash` behind a feature until benchmarked) | base-sdk `BlockHeader` hashes with X11 (node immune to the footgun) | `dashcore_hashes::hash_x11`; `BlockHash` X11 unconditionally (BS15) | n/a |
| DML, SML, CbTx, credit pool, MNHF, LLMQ consensus (members, commitments, scan/select, verification) | `rdc: dash-evo` (new, MIT) | directly | dash-spv keeps `dashcore::sml` and its engine | `From` for wire structs only |
| DKG / sig-share protocol math, BLS-IES use, quorum topology | `rdc: dash-llmq` (new, MIT) | directly | n/a | n/a |
| Wallet engine (BIP32/39/44, DIP9/14, SLIP10, accounts, tx building, coin selection) | `rdc: key-wallet`, `key-wallet-manager` (extended, CC0) | through `dashd-wallet` | native | the relevance pre-filter bridges only matching txs (§1.9) |
| RPC JSON shapes | handlers in `dashd-rpc` build Core-ordered JSON from node types; `rpc-json` (`dashcore-rpc-json`) stays the typed *client* model | server: ordered `serde_json` | clients, Platform, conformance diffs | rpc-json parses both dashd's and rdashd's responses in DIFF-RPC tests |

**Rules.**

- **R1: one base-sdk source.** Every base-sdk crate in the workspace resolves to the same pinned `node/base-sdk` revision through one `[patch."https://github.com/dashpay/base-sdk"]` table. `contrib/node/check-base-sdk-pins.sh` fails CI if `cargo tree` shows two sources (BS3). BS2's crate-local pin in `conformance/Cargo.toml` folds into the workspace pin.
- **R2: node crates never depend on `dashcore`.** `cargo deny` bans `dashcore` (and `dash-spv`) as a dependency of the pure crates and of every `dashd-*` crate except `dashd-wallet`. The only crates allowed to depend on both stacks are `dashd-wallet`, `rdash-wallet`, `dash-coreimport` (only its `wallet` feature), `dash-conformance` and `dash-testkit`.
- **R3: protocol shapes land in base-sdk first.** New wire formats (v24 and later) are implemented in the fork, with Core-derived corpora, before or together with any `dashcore` copy. BS2 asserts equality. **Exception:** anything under PLAN §7 handling (EMBARGO-1) lives only in `dash-consensus`, so the fork stays free of embargoed content and upstream-shaped.
- **R4: the BLS scheme is explicit, and the decode domain is Core's** (rev1, B-1, M-3).
  - No global "legacy" flag exists anywhere in Rust.
  - Every BLS field is typed by **how Core decodes it**: `BlsEager` (decoded with the enclosing object, with Core's VALID / INVALID-re-encodes-as-zeros / error outcomes) or `BlsLazy` (raw bytes plus the scheme captured at construction).
  - Every field also has a **scheme source**: the field's version, the context, or always basic.
  - Where Core consults the process-wide flag, a `BlsCtx` is passed, built per call site by the rules of §1.4.1: `for_block(h)` inside block processing, `for_tip(view)` elsewhere, the base block for local DKG signing.
  - v1's "scheme-agnostic byte bags" are withdrawn: they accept what Core rejects (identity bytes in a null commitment) and re-encode what Core zeroes.
- **R5: base-sdk `Checkable` is never called by node code.** Upstream-shaped fixes to it are welcome on the convergence track, but never gate the node.

### 1.2 Alternatives considered

| Option | Verdict |
|---|---|
| **A.** Build on `dashcore`; migrate it onto base-sdk underneath later (R3 Phase 3) | Rejected by both designs. `dashcore`'s shapes are wrong for a node: eager payload parsing that ignores the `vExtraPayload` length (unknown types fail the whole block), segwit fields and a BIP144 decode path, a Tapscript opcode table, a SHA256d-by-default `BlockHash`, Bitcoin `consensus::Params`, uncached txids and u32 compact sizes. Phase 3 would then rename fields under already-reviewed consensus code, forcing re-review. The ~3 weeks gained do not pay for that |
| **B.** Build on base-sdk and wait for upstream to close its gaps | Rejected: base-sdk has one maintainer, incubating 0.0.0 crates, and weeks of gaps (Devnet, LLMQ tables, v24, CL/IS/QRInfo/DKG/CoinJoin types, V1 framing). The program may change base-sdk locally (brief, DEC-09) |
| **C.** Finish R3 Phases 1–3 first, then start the node | Rejected: about 20 weeks on the critical path behind an unreleased SDK |
| **D.** Node-private types (a third codec) | Rejected: a third codec with no corpus, against the program instruction |
| **Chosen: base-sdk types on a local fork + a bridge + BS2 equality** | The cost is the bridge at the wallet seam and keeping two codecs equal, which BS2 does mechanically |

### 1.3 Node primitive wrappers (decode once, hash once)

base-sdk types recompute hashes on every call (`Hashable` re-encodes into a fresh `Vec`), re-parse payloads on every `decode_payload()`, and `Block::merkle()` re-encodes every transaction. Decoded objects are therefore wrapped once, in `dash_consensus::primitives`:

```rust
/// Immutable, shared, hash-cached transaction. Built only by `TxDecoder` (from bytes) or `Tx::new` (from parts).
pub struct Tx {
    raw: dash_primitives::Transaction,            // base-sdk shape; extra_payload kept as raw bytes
    txid: TxHash,                                 // UngatedRules.au_stable_txid (v24: yes; v23.1.8: no, §2.25 row 5):
                                                  //   nVersion >= 3 && nType == 9 (AssetUnlock) && payload.len() == 145 && payload[0] >= 2
                                                  //   ⇒ SHA256d over the serialization with the trailing 132 payload bytes zeroed; else = instance_hash
    instance_hash: TxHash,                        // SHA256d of the full canonical serialization
    size: u32,                                    // canonical serialized size
    payload: OnceLock<Result<SpecialPayload, PayloadError>>, // decoded at most once, by the first rule that needs it
    legacy_sigops: OnceLock<u32>,
}
pub type TxRef = Arc<Tx>;

pub struct Block {
    header: BlockHeader,          // base-sdk header (Copy)
    hash: BlockHash,              // X11, computed once
    txs: Vec<TxRef>,
    size: u32,
    merkle: (MerkleRoot, bool),   // (root, mutated) from cached txids; CVE-2012-2459 flag preserved
}
```

- `TxDecoder`/`BlockDecoder` decode from a byte slice with base-sdk `BaseCodec::decode` (not the double-copying `VecDecoder` path), learning each transaction's byte range from the decode cursor (CS0's own range tracking; BS13's `decode_with_ranges` replaces it later as a pure performance change, so CS0 does not depend on BS13). One pass yields the types and all hashes.
  - The P2P actor calls it after its gates (§2.14).
  - Reader tasks may pre-hash payload bytes speculatively, which is unobservable.
  - Reindex and import feed it from block files in parallel.
- **Canonical bytes.** Core hashes its re-serialization of the decoded object (`SerializeHash(*this)`), stores that serialization in `blk*.dat` (`core: node/blockstorage.cpp:748` serializes the decoded `CBlock`), and relays it. The node does the same:
  - txid, merkle leaves, sighash input, block storage and relay all use `dash_consensus::primitives::core_serialize`;
  - **`core_serialize` (and, for an LLMQ final commitment, `write_commitment`/`commitment_hash`, DEC-356) is the only encoder for consensus objects.** base-sdk's encoders are never used for consensus hashing, so an upstream base-sdk change can never move a txid;
  - the decoder hashes `bytes[range]` directly only when a fast-path predicate proves that the canonical re-serialization equals the wire bytes; otherwise it hashes `core_serialize(tx)`. CS0's design specifies the predicate (PLAN §7 handling);
  - Re-serialization equality is asserted in debug builds and fuzzed (`diff_tx_decode`).
- **Payloads stay raw** until a rule needs them. Unknown types and versions are preserved byte-exactly, which is the forward-compatibility property `dashcore` lacks (R1 §7.7).
- **Errors** carry Core's class and string: `ValidationError { class: TxValidationResult | BlockValidationResult, reason: &'static str /* "bad-txns-vout-negative" */, debug: Option<String> }`. RPC, P2P scoring and logs share it (FR-CON-31).

### 1.4 Rule DDE: decode-domain equivalence

> **Rule DDE.** For every type `T` the node decodes from untrusted bytes, base-sdk decodes exactly the byte strings Core's handler for that object accepts, in the same handler state, with the same treatment of optional, trailing and empty input. For P2P messages and payloads, re-encoding a decoded value reproduces Core's re-serialization of it, eager BLS fields included (§1.4.1). For transactions and blocks, `dash_consensus::core_serialize` (§1.3) is the canonical re-serialization. Range, semantic and policy checks belong in rules, never in decoders. The decode domain is defined **per command and per state** (§2.14 policy table), not by `Unserialize(T)` alone (GPT F14).

Why it matters:
- **Blocks.** Core deserializes a block whose transaction carries an out-of-range value, then marks it `BLOCK_FAILED_VALID` (`bad-txns-vout-toolarge`) and gives the peer 100 points. A decoder that rejects the bytes creates no failed index entry, scores the peer differently, and returns a different `submitblock` and `getchaintips` result.
- **P2P.** Core reads any non-zero byte as `true` (`core: serialize.h:239`) in `version.relay`, `fMasternode`, `senddsq`, `qsendrecsigs` and others. A strict decoder penalises peers that dashd tolerates, which is a fingerprint.

| # | Divergence (base-sdk 33fcc59) | Core | base-sdk today (verified) | Fix |
|---|---|---|---|---|
| 1 | `TxOut.value` | int64, any value decodes; `CheckTransaction` rejects | `Amount::from_sat` rejects > `MAX_MONEY` at decode (`primitives/src/transaction.rs:105-110`) | BS4a: `value: i64` (Core `CAmount`) with soft `Amount` accessors |
| 2 | `bool` | any byte; non-zero = true | 0/1 only (`types/src/codec.rs:279`) | BS4a: `CoreBool` decode policy for the P2P structs that carry one |
| 3 | `vExtraPayload` length | any length up to `MAX_SIZE` (32 MiB); `CheckTransaction` enforces 10,000 | `MAX_SPTX_PAYLOAD_SIZE = 10_240` at decode (`primitives/src/codec.rs:10`) | BS4a: decode cap = `MAX_SIZE`; the 10,000 rule moves to `dash-consensus` (CS3) |
| 4 | `LlmqType` 103 (`llmq_test_dip0024`), 105 (`llmq_devnet_dip0024`) | valid | missing (`primitives/src/support.rs:19-44`), with two inconsistent enums across crates | BS4a/BS5: one `LlmqType` with all 14 types |
| 5 | Unknown P2P command | logged and ignored, no penalty | `P2pDecodeError::UnknownCommand` | BS4a: `P2pMsg::Unknown { cmd, payload }` |
| 6 | Payload trailing bytes | `GetTxPayload` must consume all bytes; leftovers are a *rule* failure (`bad-protx-payload`) | per type | BS4a: decoders report "consumed n of m"; the rule stays in `dash-evo` |
| 7 | Version-dependent encodings (SML entry, mnlistdiff, qrinfo, `dsa`) | depend on the peer's protocol version | no context parameter | BS4a introduces `ProtoCtx { version, bls: BlsCtx, … }`; BS8–BS10 use it |
| 8 | `version` message (rev1, F14) | `nVersion`, services, time and addrMe are mandatory; every later field is read only `if (!vRecv.empty())` | all 11 fields mandatory (`p2p_core/src/msg/version.rs:209-221`) | BS4a: optional suffix fields in Core's order |
| 9 | Empty-payload messages (`verack`, `getaddr`, …) and trailing bytes (rev1) | payload ignored; trailing bytes ignored for P2P messages | any payload rejected (`macros.rs:36-44`); top-level decode rejects trailing bytes (`entity.rs:131-139`) | BS4a: per-command policy (§2.14) |
| 10 | BLS fields (rev1, B-1) | eager three-outcome decode or lazy raw bytes, per field and scheme source | unvalidated byte bags tagged IETF; `dash-pkc` returns one `Err` and diverges on most classes | BS11a + BS6a, §1.4.1 |

**A systematic sweep, not one audit** (Opus m-17). BS4a also sweeps every node-decoded type for:
- compact-size canonicality and `MAX_SIZE`;
- vector caps and enum ranges;
- trailing-byte policy;
- optional fields.

The sweep is recorded per type in `docs/node-program/DDE-SWEEP.md`.

Enforcement: differential decode fuzzing of every node-decoded type, at handler level for P2P (§2.14), against `libdash_oracle` (§4.4). A target is green after ≥ 24 CPU-hours with no divergence, plus the full-history BS2 corpus. BS4b's `diff_tx_decode` compares decode verdicts and field values; the re-encoding comparison for transactions and blocks is CS0's `diff_tx_decode`/`diff_block_decode`, through `core_serialize`.

### 1.4.1 The BLS decode domain (rev1; Opus B-1, M-3; GPT F06)

v1 carried BLS fields as "scheme-agnostic byte bags" and specified "decoded infinity → invalid object". Both diverge from Core in ways that split consensus. One example: a null quorum commitment whose `quorumPublicKey` holds the identity encoding is `bad-qc-payload` in Core (the block is invalid), while v1 would read an invalid object, so `IsNull` holds and the commitment counts as a valid null. Everything below was verified for this revision against `core: bls/bls.h:100-216` (identical in v23.1.8 and develop) and against dashbls/relic. Core's wrapper was compiled against the oracle branch's libdashbls and probed class by class.

#### Core's algorithm (normative)

**Eager decode.** `CBLSWrapper<T>::Unserialize(bytes, F)`, where F is the legacy or basic scheme flag:
1. `SetBytes(bytes, F)` resets the object to **invalid** in three cases: the bytes are all zero; `FromBytes(F)` throws; or the decoded point is the identity.
2. If the object's F-serialization equals the input, the result is the object (VALID, or INVALID for all-zero input).
3. Otherwise `SetBytes(bytes, !F)` runs. If the !F-serialization of that object equals the input, the object is reset to INVALID and **no error is raised**: the bytes are canonical in the other scheme.
4. Otherwise it throws `"malleable BLS object"`.

An INVALID object re-serializes as zeros.

**`FromBytes` per scheme.**
- **Legacy** (dashbls → relic) performs **no on-curve check and no subgroup check**. Relic's errors are swallowed (`CheckRelicErrors(false)`), and an off-curve x keeps whatever point relic computes.
- **Basic** checks that the point is on the curve and in the subgroup.

**Encoding flags.**
- G1 legacy: bit 0x80 is the sign of y.
- G1 basic: 0x80 is the compression flag, 0x20 the sign, 0x40 infinity.
- G2: the same flags on its first byte; relic's legacy G2 sign falls back to `y.c0` when `y.c1 = 0`.
- On serialization relic's `ep2_pck` takes the G2 sign from `y.c1` only, so a G2 point with `y.c1 = 0` and the sign bit set is not canonical in either scheme (BS11at-r1 M4c).

**Secret keys.** The flag is reused as `modOrder`, and serialization does not depend on the scheme.

**Lazy decode** (`CBLSLazyWrapper`, `bls.h:409-559`):
- `Unserialize` stores the raw bytes. It sets `bufValid` to "any byte non-zero" and `bufLegacyScheme` to the scheme it was given (by default the global flag at construction). It never fails on content.
- `Get()` has **no** opposite-scheme fallback. The object is valid only if the bytes decode as valid under the stored scheme and round-trip. On failure, `bufValid` becomes false and the result is an invalid object.
- Serializing before any `Get()` writes the raw bytes; after a failed `Get()` it writes zeros, and `GetHash` hashes the zeros.
- Serializing under a scheme other than the buffered one before a successful `Get()` writes zeros and overwrites the buffer (`:453-464`).

#### Resulting outcome classes

This table is verified and serves as a design aid. The normative definition is the algorithm above together with relic's decompression, and the arbiter is `diff_bls_decode` against the oracle (L1).

| Input (top bits b7 b6 b5 of byte 0; coordinate) | Under the legacy flag | Under the basic flag |
|---|---|---|
| all zero | INVALID | INVALID |
| identity `c0 00…00`, and non-canonical infinity (`c0…01`, `e0…`) | **error** (THROW) | **error** |
| any coordinate ≥ p | error | error |
| stray 0x20/0x40 bits (`001`, `010`, `011`, `11x`; `101` that is not a point) | error | error |
| G1 `100`, a curve point in the subgroup | VALID (P) | VALID (−P): canonical in **both** schemes (Core notes this at `governance/vote.cpp:33-36`) |
| G1 `000`, on curve | VALID | INVALID (canonical in legacy only) |
| G1 `101`, a canonical basic point | INVALID (canonical in basic only) | VALID |
| G1 `000`/`100`, off curve or outside the subgroup | **VALID** (no checks) | INVALID |
| G2, legacy-canonical | VALID | INVALID |
| G2, basic-canonical with sign 0 | VALID as an unrelated non-subgroup point when the swapped-coordinate x is on the curve (about half the time), else INVALID | VALID |
| G2 `000`, random | VALID (about 48% off curve) | INVALID |
| G2 `100`, random | VALID if on curve, else error | INVALID if on curve, else error |
| G2 `100` (legacy sign set), on the curve with `y.c1 = 0` | error | error |
| secret key = r | INVALID | VALID |
| secret key > r | error | error |

#### base-sdk today (33fcc59): divergent on almost every row

- **Error model.** `dash-pkc` returns a single `Err` for every failure. It has no INVALID-vs-error distinction, no opposite-scheme fallback and no round-trip check.
- **Legacy G1:**
  - it rejects all-zero bytes and any `0xc0` prefix;
  - it strips stray 0x20/0x40 bits where Core raises an error;
  - it decompresses with blst, which checks the curve but not the subgroup, so off-curve points that Core treats as VALID are rejected.
- **Legacy G2** takes the sign from `y.c1` only.
- **Basic secret keys:** `sk == r` is rejected, where Core's basic decode accepts it.
- **`dash-primitives`** types every BLS field as an unvalidated `BlsPkBytes`/`BlsSigBytes<BlsScIetf>`, whatever the payload version. The IETF tag is wrong for v1/v2 commitments, v1 ProTx and v1 SML entries. Decoding therefore never fails, and re-encoding reproduces the wire bytes where Core reproduces zeros.

#### Rust types (BS11a-t types and BS11a legacy decode in `dash-pkc`; BS6a in `dash-primitives`)

```rust
pub enum SchemeSource { Version /* the field's own version mapping */, Global /* BlsCtx */, AlwaysBasic }
pub enum PointRepr { Usable(/* blst point: on curve, in the subgroup */), Exotic /* legacy only: off curve or outside the subgroup */ }
pub enum BlsValue<const N: usize> { Valid { canonical: [u8; N], point: PointRepr }, Invalid /* re-encodes as zeros */ }
/// Eager: decoded when the enclosing payload or message is decoded; Core's THROW is a decode error of the enclosing object.
pub struct BlsEager<G, const N: usize> { value: BlsValue<N>, scheme: Scheme }
/// Lazy: raw bytes plus the scheme captured at construction (CBLSLazyWrapper); never fails to decode.
pub struct BlsLazy<G, const N: usize> { raw: [u8; N], scheme: Scheme, state: OnceCell<LazyState> }
```

Implemented names (BS11a-t, BS11at-r1 M4d): `BlsSchemeKind` for `Scheme`, `BlsSchemeSource` for `SchemeSource`, `BlsPoint` for `PointRepr` (BS11a classifies `Exotic` lazily, BS11a D2), `BlsWire`, and the per-kind aliases.

- **`BlsEager::decode(bytes, scheme)`** implements the four-step algorithm exactly.
  - A THROW becomes an error of the enclosing decoder. For special-transaction payloads, the rule layer then reports Core's `bad-*-payload` (Core's `GetTxPayload` catches the exception and returns `nullopt`). For P2P messages, the handler gets the exception outcome from §2.14's policy table.
  - Re-encoding writes `canonical`, or zeros for `Invalid`.
- **`BlsLazy`** reproduces `Get()` and the re-serialization rules, the explicit-scheme trap included.
- **Legacy decode is blst uncompress plus the BS11a-t predicate** (BS11at-r1 §2, BS11a D1); there is no relic decode port. The canonical bytes fix the point relic holds, off-curve points included, so `canonical` and the VALID/INVALID decision match bit for bit. Basic decoding uses blst plus the wrapper logic.
- **`Exotic` points** cannot be loaded into blst. What Core computes with one depends on the call (BS11a §2, D16):
  - under the **basic** verify flag, the three direct verify entry points (`VerifyInsecure`, `VerifyInsecureAggregated`, `VerifySecureAggregated`) return `false` with no arithmetic on the Exotic operand beyond the exact validity test (`CoreMPL` checks `IsValid()`, or re-decodes, before using it; `VerifySecure` may already have computed with earlier keys, BS11a D31);
  - aggregation (`AggregateInsecure`) runs relic on its inputs under either flag with no validity gate; the span form uses an invalid input's `impl`, which is the default (infinity) element only for a fresh, reset or failed-decode wrapper (BS11a F11). Where Core aggregates before verifying (the batch verifiers), the basic gate tests only the aggregate (BS11a F7, D26);
  - under the **legacy** verify flag, and for aggregation and the IES DH key under either flag, Core runs relic's arithmetic on it. The Rust node reproduces that with BS11a's transliteration of relic. `diff_bls_arith` and `diff_bls_verify` compare it with Core; scan (h) (O6) is evidence only.
  - The case *is* reachable live on mainnet through version-derived legacy fields: ProUpServTx/ProUpRevTx v1 signatures and v1 operator keys. Core decodes them, then verifies under the global (basic) scheme and rejects with `bad-protx-sig`. The Rust node must report that string, not a payload error.

#### Per-field decode table

develop is v24; v23.1.8 lacks the v24 types.

| Object · field | Eager/lazy | Scheme source | Hash computed over | Notes |
|---|---|---|---|---|
| `CFinalCommitment` quorumPublicKey, quorumSig, membersSig | eager | version: legacy iff `nVersion ∈ {1, 2}`; any other value, unknown included, is basic | `SerializeHash(qc)`, i.e. the re-serialization with zeros for INVALID, for the `qfcommit` inv, the mineable pool and the mined key (`blockprocessor.cpp:115,452,504,842`) | `IsNull` ⇔ no signer bits, no valid-member bits, a null vvecHash, and all three fields `!IsValid()`. A THROW is `bad-qc-payload` |
| `CCbTx` bestCLSignature | eager | global | txid over the raw `vExtraPayload` | a THROW is `bad-cbtx-payload` |
| `CAssetUnlockPayload` quorumSig | eager | global | txid (raw payload; zeroed by the v24 stable txid, §2.25 row 5) | |
| `CProUpServTx` sig, `CProUpRevTx` sig | eager | version (v1 is legacy) | txid | verified under the **global** scheme (quirk 21) |
| `CProRegTx`/`CProUpRegTx` pubKeyOperator; DMN state; SML entry | lazy | version | SML entry hash over the raw bytes | |
| `CProUpSharedRegTx` pubKeyOperator | lazy | always basic | txid | v24 only |
| `ChainLockSig` sig (`clsig`) | eager | global, i.e. the tip at processing time | `SerializeHash(clsig)`, the CLSIG inv hash (zeros for INVALID) | |
| `CMNAuth` sig | eager | global | — | sign hash = `SerializeHash(tuple(CBLSPublicKey operatorKey, challenge, fInbound, nVersion))`, where the key is **non-lazy**, serialized under the global flag; verified with **basic** |
| `CRecoveredSig`, `CInstantSendLock`, `CSigShare` | lazy | global at construction (decode time) | the **wire bytes**: the recsig `UpdateHash` and the ISDLOCK hash are taken before any `Get()` | an object queued across a V19 flip keeps its construction scheme |
| DKG messages: contribution (vvec, IES blobs, sig), complaint, justification (sig, secret keys), premature commitment | eager | global when receiving; local signing and the final commitment's version use `!ActiveAfter(base, V19)` (`active/dkgsession.cpp:34`) | per message type | `BuildCommitmentHash` serializes the pubkey under the global flag; `CFinalCommitment::Verify` takes `nVersion` from the base but verifies under the global flag |
| IES ephemeral pubkey | eager | global | — | the symmetric key is derived from the **basic** encoding of the ECDH point (BS11a fix (2)) |
| Governance object/vote `vchSig`; CoinJoin `dsq`/`dstx` `vchSig` | raw bytes | `SetBytes(basic)` at check time: no round trip, no fallback, never throws | the object/message hash includes the raw signature | |
| `PlatformBanMessage` sig | eager | always basic | | |
| `CProDisTx`, `CProUpShareTx`, sporks | ECDSA only | — | | |

#### `BlsCtx` per call site (Opus M-3)

Core's process-wide flag works out to two rules:
- **inside block processing:** block h is legacy iff `h < V19Height`;
- **everywhere else:** legacy iff `!ActiveAfter(tip, V19)` at the moment of the call.

Startup sets the flag from the tip (`node/chainstate.cpp:429-461`), and DisconnectTip and undo flip it back. develop and v24 commit the flag only after a full connect (`ScopedBLSLegacyScheme`, `validation.cpp:137-178`), so failed connects and `fJustCheck` revert it. v23.1.8 does not: a failed connect at the V19 boundary leaves the flag switched (§2.25 row 26).

Rust has no global flag. Every call site takes an explicit `BlsCtx`:

| Call site | `BlsCtx` |
|---|---|
| `connect_block` / `test_block_validity` / `disconnect_block` of block h | `BlsCtx::for_block(h)`: legacy iff `h < V19Height` |
| Mempool `CheckSpecialTx`; miner CbTx serialization; GBT | `BlsCtx::for_tip(view)`, taken at the call |
| P2P intake, decoded in the actor (§2.14) | `for_tip` at processing time, re-evaluated after every tip change; lazy objects keep the scheme captured at decode |
| DKG receive / local sign | `for_tip` / `!ActiveAfter(quorum_base, V19)` |
| MNAUTH | the sig decodes `for_tip`; the operator key in the sign hash serializes `for_tip`; verification is basic |
| Queued background work (recsig and isdlock verification, sig-share recovery, DKG sessions) | the scheme captured with the object. Each job carries the tip epoch and is fenced: on a tip change across V19, it is re-checked or discarded (§2.10.2) |

#### Tests (Opus B-1 item 5)

- **L1:**
  - `diff_bls_decode`: G1/G2 × pk/sig/sk × legacy/basic, seeded with every class in the outcome table plus random bytes;
  - `diff_bls_verify` with exotic seeds;
  - `diff_payload_{2,4,5,6,9}`, `diff_clsig`, `diff_p2p_qfcommit` and `diff_mnauth`, seeded per field.
- **L3 operators:** BLS encodings for QcTx (null and non-null), AssetUnlock, ProUpServ/ProUpRev (v1 and v2) and CbTx.
- **New quirks:**
  - 35: eager BLS decode outcomes;
  - 36: null commitment ⇔ `!IsValid`;
  - 37: lazy re-serialization after a failed `Get()`;
  - 38: CLSIG/qfcommit hashes over the re-serialization, isdlock/recsig hashes over the wire bytes.
- **Scenario:** a reorg across V19 with P2P traffic decoding across the switch. At **M4** this covers `clsig` traffic, the only overlay input an M4 node processes. At **M7** it covers full masternode traffic: `isdlock`, recsig, sig shares and DKG messages.

### 1.5 The specific sub-questions

**bincode.** The node uses none (FR-STO-05). On-disk values use explicit, versioned codecs (§2.9). `dashcore` newtypes keep owning the persisted bincode formats of wallet blobs, SPV storage and Platform state, with pinned-layout tests (the `bincode_layout_matches_pubkey_hash` pattern). base-sdk does not get bincode back, which keeps kwvg's base-sdk#1 decision and avoids the orphan-rule problem. `grovedb-bincode` never enters the node's dependency set.

**Hash display order.** Bytes are canonical: internal little-endian, exactly as Core stores them. The node's RPC, CLI and ZMQ follow Core's rule:
- every `uint256`/`uint160` blob uses `GetHex` (byte-reversed): txid, block hash, proTxHash, quorumHash, confirmedHash, vvecHash, `platformNodeID`;
- byte strings (scripts, BLS keys and signatures) are forward hex.

base-sdk's reversed `Display` matches Core, so the node uses it. `dashcore`'s forward display of `ProTxHash`, `PubkeyHash`, `ScriptHash` and `MerkleRoot*` is an FFI-stability concern: it stays, and is documented in the bridge's direction table. R3's open question 5 remains with the upstream owners.

**Network enum and Devnet.** `dash_params` gains:
- `NetworkKind { Mainnet, Testnet, Devnet, Regtest }`, `#[repr(u8)]` with discriminants 0..3, equal to `FFINetwork`;
- `ChainParams`: an owned structure built by `ChainParamsBuilder::new(kind).devnet_name(..).apply_overrides(&args)`.

The devnet **name** lives in `ChainParams`, not in the enum, as in Core (four chains; the name comes from args). The devnet genesis is **derived** (`CreateDevNetGenesisBlock`: prev = regtest-style genesis, coinbase scriptSig `1 <name>`, an `OP_RETURN` output, bits of the genesis), never stored.

Every override flag goes through the builder with `core: chainparams.cpp` parse rules and errors (FR-CON-03), because Dash's functional tests use them:
- devnet: `-llmqchainlocks`, `-llmqinstantsenddip0024`, `-llmqplatform`, `-llmqmnhf`, `-llmqdevnetparams`, `-highsubsidyblocks`/`-highsubsidyfactor`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-sporkaddr`;
- regtest: `-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams` (Dash's 9-field form), `-llmqtestparams`, `-llmqtestinstantsendparams`, `-llmqtestinstantsenddip0024`.

`Arc<ChainParams>` is passed explicitly through every crate. There is **no global `Params()`**; Core's global use is a known bug source (`IsQuorumRotationEnabled`, R2 A.4).

**v24 payloads (upstream #1136–#1138, all still open).** Both copies are needed:
- **`dash-primitives` (BS6a/b), for the node:** CbTx v4 `merkleRootAssetUnlocks`; ProRegTx v3 (`ExtNetInfo` with ADDRv2 services and domain entries, u8-counted payouts, shares and join sigs, `nEarlyPeriodBlocks`/`nEarlyPenalty`); ProUpServ/ProUpReg/ProUpRev v3; types 10 `ProDisTx`, 11 `ProUpShareTx` and 12 `ProUpSharedRegTx`; AssetLock v2; AssetUnlock v2 with the stable txid and `instance_hash()`; and per-payload `sig_hash_bytes()` reproducing `SER_GETHASH` exclusions (type 10 signs a separate digest; types 1/3/11 exclude `vchSig`; type 12 excludes its signature list).
- **`dashcore` (V24a, running; v1's BS17), for SPV, the wallet, the desktop and Platform:** #1136 (unknown-type passthrough plus honouring the payload length) and #1138 landed, #1137 ported, and the remaining shapes implemented with Core-produced vectors. The production fix for shipped SPV wallets still requires the upstream PRs, so pushing them is pasta's call.

BS2 asserts both copies produce identical bytes, txids and instance hashes over `trivially_valid.json`/`trivially_invalid.json`, testnet v24 blocks and oracle-generated synthetic v24 transactions.

The rule sets follow the pins (DEC-05):
- mainnet `V24` is `NEVER_ACTIVE` in every branch (`core: chainparams.cpp:213`);
- testnet's start time is 2026-09-24 (`:413`);
- rc.3 and develop add `evo_shares` (bit 14).

`dash-params` carries both deployments, and the node pins the exact rule set to each network's reference release (mainnet v23.1.8, testnet v24.0.x), re-verified by replay at every re-pin.

**The `BlockHash` X11 footgun.** The node is immune: base-sdk `BlockHeader` always hashes with X11. `dashcore` is still fixed (BS15): `core-block-hash-use-x11` becomes a deprecated no-op and `BlockHash` is X11 unconditionally, so a bridged hash can never disagree. The C `rs-x11-hash` stays the default until `dash-pow` (pure Rust, which has AES-NI code compiled but not dispatched) benchmarks at parity on B1's `microbench` X11 row and the `spv-headers-*` header-sync scenario.

**One codec vs two.** Two codecs exist during the program, each the source of truth for a disjoint set of consumers: the node decodes only with base-sdk; SPV, the wallet and FFI decode only with `dashcore`.
- **Kept safe:** BS2 decodes every mainnet and testnet block and transaction from Core's block files through both codecs and dashd, and asserts identical re-encoding, txid, instance hash, block hash, merkle root and payload JSON. It also round-trips a `-capturemessages` corpus. It runs per PR on the regtest sets and nightly on full history.
- **Off the hot path:** the node's wallet seam bridges only wallet-relevant transactions (§1.9).
- **Converged later:** CV1–CV4 (§1.8) make `dashcore` a bincode-owning façade, and the bridge becomes the identity.

**Codec style, purity, edition, MSRV.**
- Node crates read whole messages and blocks before decoding, as Core does, so the slice-cursor `BaseCodec` is the right primitive.
- The five pure crates are ordinary `std` crates with **no I/O, threads, clocks or globals**. `cargo deny` bans tokio, RocksDB and socket crates; clippy's `disallowed_methods`/`disallowed_types` ban `std::time`, `std::fs`, `std::net` and `std::thread`. v1's `no_std + alloc` requirement and its CI job are dropped (Opus X-1, GPT F20): no target needs it, and it conflicted with the `OnceLock`-based `Tx`/`Dmn` types.
- rust-dashcore crates are edition 2024 / MSRV 1.89. base-sdk changes are edition 2021 / MSRV 1.85: no let-chains, 2-space style, no `unwrap`/`panic`/`unsafe` without a reasoned `#[expect]`, Latin-1, `just test lint` green.

### 1.6 base-sdk fork mechanics (DEC-09)

- **Branches.** In `~/workspace/base-sdk`, branch **`node/base-sdk`** sits on top of upstream `develop`. Each BS task that changes base-sdk gets its own branch `node/bs<N>-<slug>` there and a worktree under `~/workspace/bsdk-wt/<slug>`. It is reviewed like any node task and merged into `node/base-sdk` with `--no-ff`. The existing empty `node/integration` branch in base-sdk (at `develop`, no extra commits) is retired by BS3 to avoid two "integration" branches.
- **Consumption.** rust-dashcore's root `Cargo.toml` keeps the upstream `git = "https://github.com/dashpay/base-sdk"` declarations and adds `[patch."https://github.com/dashpay/base-sdk"]` entries for all 8 crates: `git = "file:///work/home/agent/workspace/base-sdk", rev = "<sha>"`. This is reproducible from every worktree, independent of anyone's uncommitted edits, and recorded in the committed `Cargo.lock`.
  - While developing a base-sdk change, an implementer may use a local, never-committed `[patch]` with a `path`.
  - Path dependencies through a `~/workspace/rdc-wt/base-sdk` symlink (F) were rejected: they make every build depend on whatever happens to be checked out in the base-sdk working tree.
- **Pin moves.** `contrib/node/bump-base-sdk.sh` re-pins, runs base-sdk `just test lint`, the rust-dashcore gates and BS2 on the testnet corpus, then tags the pinned revision `node-base-sdk-YYYY-MM-DD` so it stays reachable after later rebases.
- **Weekly upstream rebase** of `node/base-sdk` onto upstream `develop` (PLAN §6). Each breaking upstream change becomes one adaptation PR in rust-dashcore.
- **Commit hygiene.** Commits stay upstream-shaped (`namespace%type(scope): …`, base-sdk style). Nothing node-specific and nothing embargoed goes into base-sdk. Node-only wrappers and consumers live in rust-dashcore. `docs/node-program/BASE-SDK-FORK.md` lists every fork commit, its upstream status and whether upstreaming is intended. Whether and when to upstream is pasta's decision (DEC-09).

### 1.7 The Base SDK milestone (BS1–BS18, rev1 splits)

BS1 is merged (9494ed8e). BS2 is nearly done. BS3 and V24a are running. The tasks below are numbered in dependency order. "bsdk" means a PR on `node/base-sdk` that follows base-sdk conventions. Sizes: S ≤ 1, M 1–2, L 2–4 agent-weeks. The **Origin** column maps each task to Fable's and Opus's numbering and to R3's task IDs.

Rev1 changes to the table:
- splits BS4, BS6, BS9 and BS11 into PR-sized halves (GPT F18, Opus M-9d);
- folds BS17 into the running V24a task (manager instruction; Opus M-10.4, X-4);
- moves BS14 to M6 (M-10.4);
- moves the headers2 stream codec and the small control messages from BS8 to BS7, because headers sync and the handshake (P2a) need them and nothing else in BS8 (critical-path lever);
- re-types BLS fields (B-1).

| ID | Task | Repo | Changes base-sdk? | Depends | Size | Gates | Origin |
|---|---|---|---|---|---|---|---|
| **BS1** *(merged)* | Re-pin base-sdk to develop @ 33fcc59 (#53, #54 `Arith256`, #56); land #1110/#1142; `Work`/`Target`/SPV `ChainWork` → `Arith256`; dedupe `EddsaPkHash`; drop the vestigial `dash-types` dep; stop using `dash_pkc::__deps::ff` | rdc | small | — | — | everything | R3 1.1–1.5; F BS1/BS12/BS13; O BS1 |
| **BS2** *(nearly done)* | Reference dashd oracles (`/work/chains`, `ORACLE.md`, `contrib/oracle-cli`) + `dash-conformance`: dashcore ⇄ base-sdk ⇄ dashd over full history | rdc | no | — | — | every swap; node start | R3 2.1; F BS2; O BS2 |
| **BS3** *(running)* | **Fork plumbing and workspace wiring**: the `node/base-sdk` branch; `[patch]` → `file://` rev for all 8 crates; fold BS2's private pin; `bump-base-sdk.sh` and `check-base-sdk-pins.sh`; a committed `Cargo.lock` on `node/integration`; `cargo deny` rule R2 skeleton plus the pure-crate bans (§1.5); CI groups `node-pure`, `node-runtime`, `node-app`, `node-conformance`; base-sdk worktree layout in PROCESS.md. (v1's `no_std` job is dropped: X-1) | rdc + bsdk (infra) | infra only | BS1 | S | node start | F BS15 + O BS3 |
| **BS4a** | **Decode-domain alignment, shapes** (§1.4 #1–#9 and the m-17 sweep): `TxOut.value: i64`; `CoreBool`; payload cap `MAX_SIZE`; one 14-type `LlmqType`; `P2pMsg::Unknown`; empty-payload leniency; optional `version` suffix fields; consumption reporting; `ProtoCtx { version, bls, … }`; `DDE-SWEEP.md`. No BLS decisions (those are BS11a) | bsdk | **yes** | BS3 | M | node consensus | O BS4 (F had none); Opus Wave-1 "BS4a" |
| **BS4b** | **Decode-domain evidence**: differential decode targets against the oracle for `Transaction`, `Block`, `BlockHeader` and every payload (decode verdicts and field values), each ≥ 24 CPU-h; BS2 full history | bsdk + node-fuzz | tests | BS4a, O1, O3 | S | node consensus | — |
| **BS5** | **`dash-params` completion**: `NetworkKind` + Devnet + derived devnet genesis; `ChainParamsBuilder` with every override and Core's parse errors; `LlmqParams` (14 types × all fields, derived values) and per-network roles (ChainLocks, DIP24 InstantSend, Platform, MNHF) and enabled sets; spork address and key ID; DGW/KGW params; MN payment schedule (increase block/period, BRR table); superblock/budget params; the 39 mainnet checkpoints; assumevalid; minimum chain work; BIP9 deployments as an indexable array with Dash fields (V24 bit 13, `evo_shares` bit 14 per pin); `CreditPoolPeriodBlocks`; CoinJoin participant limits; `fRequireRoutableExternalIP` and the other policy flags; BIP155 fixed seeds; hard-coded pins (`bad-sb-start` 614820, `bad-dip3-enf-block` 1047200, testnet LLMQ_25_67 at 847000). **Oracle corpus:** a JSON dump of `CChainParams` from both pinned releases for the 4 networks plus 12 override combinations, with an equality test | bsdk | **yes** | BS3, BS4a, O1 | L | node consensus | F BS6 + O BS5; R3 2.2 |
| **BS6a** | **`dash-primitives` v24 shapes, types first** (§1.5): every v24 payload decoded; **BLS fields re-typed** as `BlsEager`/`BlsLazy` with their scheme source (§1.4.1 table), replacing the IETF-tagged byte bags; corpus round trips (`trivially_valid/invalid.json`, testnet v24 blocks, regtest vectors from `feature_masternode_shares.py` and `feature_protx_version.py`). Closes base-sdk#40 | bsdk | **yes** | BS4a, BS11a-t | M | node consensus; testnet | F BS7 + O BS6; R3 2.3 |
| **BS6b** | Per-payload `sig_hash_bytes()` (SER_GETHASH exclusions; type 10's separate digest); `diff_payload_<type>` (13 types × versions) ≥ 24 CPU-h; AU v2 stable-txid vectors per profile | bsdk | **yes** | BS6a, O2a | M | node consensus | — |
| **BS7** | **`dash-p2p-core` framing and tables**: V1 24-byte header encode/decode, checksum, command-string validation (printable ASCII, NUL-padded), oversize detection (pure); full inventory table (types 1–34; `MSG_CMPCT_BLOCK = 20`; unknown types preserved); short-ID table (0–32 in BIP324 order with 5 and 29–32 empty, 128–168 Dash) with the send gate `short_id(cmd, peer_version)` (≥ 70235; ≥ 70240 for `platformban`); the protocol-version constants table (70221…70242); `block`/`tx` un-stubbed onto `dash-primitives`; BIP37 (`filterload`/`filteradd`/`filterclear`, `merkleblock` + `CPartialMerkleTree`), BIP152 v1 (`sendcmpct`/`cmpctblock`/`getblocktxn`/`blocktxn`, txid short IDs), the BIP157 message families, and (rev1) the headers2 `CompressibleBlockHeader` stream codec (7-entry version MRU, int16 time deltas, inverted "bit set = field present" flags; moved from rdc, one copy) and the small control messages headers sync and the handshake need (`sendheaders2`, `getheaders2`, `senddsq`, `qsendrecsigs`, `qwatch`, `ssc`, `getmnlistd`; also moved from BS8). base-sdk's existing V2 framing and short IDs (`p2p_core/src/bip324.rs`, `short_id.rs`) are the ones `dash-p2p-transport` uses (m-21) | bsdk | **yes** | BS4a | M | P2P transport | F BS9d + O BS8a; R3 2.5 |
| **BS8** | **Overlay wire types I (chain-facing)**: `ChainLockSig`, `InstantSendLock` (isdlock), `CRecoveredSig`; `FinalCommitment` v1–4 + QcTx payload; `CQuorumSnapshot`; `GetQRInfo`/`QRInfo`; SML entry v1–3 (protocol-gated `SMNLE_VERSIONED`, `ExtNetInfo`); `MnListDiff` (`MNLISTDIFF_VERSION_ORDER`, `MNLISTDIFF_CHAINLOCKS`, `quorumsCLSigs`); MNAUTH; `CSporkMessage`; `PlatformBanMessage`; `qgetdata`/`qdata`. Every BLS field uses BS11a's types | bsdk | **yes** | BS6a, BS7, BS11a-t | L | P2P serving; LLMQ verify | F BS9a + O BS7 (part); R3 2.4 |
| **BS9a** | **Overlay wire types II, masternode protocols**: DKG messages `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit` (`DYNBITSET` with padding-bit rejection, BLS-IES multi-recipient blobs), `qfcommit`, and the `MaxDKGMessageSize` framing walk; the sig-share family (`qsigsesann`, `qsigsinv`, `qgetsigs` with `AUTOBITSET`, `qbsigs`, `qsigrec`, `qsigshare`) | bsdk | **yes** | BS8 | M | DKG | F BS9b + O BS7 (part) |
| **BS9b** | **Overlay wire types III, CoinJoin and governance**: `dsa` (`nFlags` ≥ 70241), `dsi`, `dsf`, `dss`, `dsc`, `dssu`, `dstx`, `dsq` (hash includes the sig, sighash excludes it; `CoreBool` `fReady` canonicalised to 0x01 for hashing, m-18); `govsync` (with `CBloomFilter`), `govobj`, `govobjvote` (65/96-byte vote-signature rule) | bsdk | **yes** | BS8 | M | CoinJoin, governance | F BS9c |
| **BS10** | **All 74 commands typed**: `P2pMsg::decode_with_policy(cmd, payload, &ProtoCtx)`; `Unknown` preserved; a `-capturemessages` corpus (mainnet and testnet dashd peers, every peer-version bucket) round-trips byte-identically | bsdk | **yes** | BS9a, BS9b | M | P2P serving (M5) | O BS8b |
| **BS11a-t** | **BLS decode types** (§1.4.1): `BlsEager`/`BlsLazy`/`SchemeSource`/`BlsValue`; the four-step wrapper algorithm for the **basic** scheme; the lazy `Get()`/re-serialization rules; the scheme as an explicit parameter everywhere | bsdk | **yes** | BS3 | S | node consensus (types) | Opus B-1 rec 1 |
| **BS11a** | **BLS decode domain, legacy**: decode closure on the BS11a-t predicate + blst (no relic decode port); `Exotic` points and the relic arithmetic Core runs on them (a transliteration); secret-key `modOrder` rules; the IES key from the basic encoding (fix (2)); `diff_bls_decode`/`diff_bls_arith`/`diff_bls_verify` certified per DEC-184/186 (designs/BS11a-legacy-bls.md) | bsdk | **yes** | BS11a-t, O2a (acceptance) | L | LLMQ verify (M4); CH7b merge | F BS14 + O BS15 (part) |
| **BS11b** | **`dash-pkc` API completion** on BS11a's `BlsElem` (secure-aggregation weights, `HashPubKeys` and the three verifies are BS11a's, BS11a D10): `secure_aggregate`, `fast_verify_aggregates`, `verify_aggregates` (distinct messages), threshold `split`/`derive_share`/`recover` for sk/pk/sig with `BlsShareId` from proTxHash bytes (big-endian mod r), multi-recipient IES, `to_scheme`, hash-of-serialization helpers, random-scalar batch verification (basic scheme only) | bsdk | **yes** | BS11a | M | LLMQ verify (M4) | F BS14 + O BS15 (part) |
| **BS12** | **BLS vector corpus with provenance**: regenerate `dash-pkc` corpora from dashbls through the oracle; legacy hash-to-G2 (`ep2_map_legacy`) vectors; every class of §1.4.1's outcome table for G1/G2 legacy and basic; a full regtest DKG transcript; threshold recovery; IES. A `PROVENANCE.md` per file | bsdk + Core oracle | **yes** (tests) | O2a | M | LLMQ verify; DKG | F BS16 |
| **BS13** | **base-sdk performance helpers**: allocation-free `encoded_len()`; streaming SHA256d `Hashable` (no temporary `Vec`); `decode_with_ranges`. Risk class CON (it changes hashing helpers, F18) | bsdk | **yes** | BS4a | S | perf only | O BS14 |
| **BS14** | **`dashcore` ⇄ base-sdk bridge** (`dashcore::bridge`, feature `base-sdk-bridge`): hashes, `OutPoint`, `TxIn`/`TxOut`, `Transaction`, header, `Block`, every payload, ChainLock, InstantLock, MnListDiff, QRInfo; conversion through bytes with `debug_assert` on hash equality; the display-direction table with tests; round trip over the BS2 full-history corpus; a compile-only smoke test against Platform's `dashcore` import list. **M6**, not M1 (M-10.4) | rdc | no | BS6b, BS15, V24a | M | wallet seam (M6), import tools | F BS10 + O BS10; R3 1.7 |
| **BS15** | **X11 `BlockHash` unconditional in `dashcore`** (`core-block-hash-use-x11` → no-op); a `hash_x11` backend switch (C default, `dash-pow` behind a feature) with a differential test over all mainnet headers; runtime AES-NI/SSE dispatch in `dash-pow` (bsdk perf PR); switch the default only when B1's X11 microbench and `spv-headers-*` show Rust ≥ C | rdc (+ bsdk perf) | perf part **yes** | BS1 | S | SPV correctness; bridge | F BS3 + O BS11; R3 1.8, 2.7 |
| **BS16** | **Network mapping**: `From`/`Into` between `dash_network::Network` and `NetworkKind`; `FFINetwork` discriminants and serde names unchanged; deprecate `dashcore::consensus::Params` for new code | rdc | no | BS5 | S | — | F BS6b + O BS12; R3 3.2 (part) |
| *(BS17)* | **Folded into V24a** (running): full v24 wire format in `dashcore` (#1136/#1138 landed, #1137 ported, types 10–12, ProRegTx v3, AssetLock/Unlock v2, the payload length prefix) with Core-produced vectors. The production fix for mobile/desktop SPV wallets is the upstream PRs #1136–#1138; pushing them is pasta's call (DEC-09; PLAN §10 Q5) | rdc | no | — | — | wallet (M6); desktop | F BS8 |
| **BS18** | **`dashcore` SPV protocol correctness**: `PROTOCOL_VERSION` 70242 and the version-gate constants; inventory 20 and the inventory table; service flags (`NETWORK`, `BLOOM`, `COMPACT_FILTERS`, `NETWORK_LIMITED`, `HEADERS_COMPRESSED`, `P2P_V2`; drop `WITNESS`/`GETUTXO`); `MAX_PROTOCOL_MESSAGE_LENGTH` 3 MiB, `MAX_SIZE` 32 MiB, compact size as u64; remove the BIP144 decode path; DIP-0020 opcode naming (0xba `OP_CHECKDATASIG`, 0xbb `OP_CHECKDATASIGVERIFY`). Changes stay in `dash/`, not `dash-spv/src/network/` (#902) | rdc | no | — | M | SPV only | F BS4 + O BS13 |

**Gates.**
- **Node consensus gate** (chainstate on real blocks): BS3, BS4a, BS5, BS6a, BS11a-t.
- **P2P transport:** BS7. **P2P serving:** BS8 + BS10.
- **LLMQ verification (M4):** BS8 + BS11a + BS11b + BS12. **Masternode overlay (M7):** BS9a + BS11b + BS12.
- **Wallet seam (M6):** BS14 (which needs BS15 and V24a).
- **Never gate a node milestone:** BS13, BS16, BS18.

**Dropped from both lists, with reasons:**
- **F BS17** (fix base-sdk `Checkable`, including PLAN §7 content): replaced by R5 + CS3 + §1.3, to keep embargoed content out of the fork.
- **O BS9** (an optional `dash-script` `GetOp` in base-sdk): the interpreter's `GetOp` lives in `dash-consensus`.
- **F BS5** (#400 segwit removal) and **F BS11** (ECDSA through pkc): moved to the convergence track (CV1, CV2).
- **F BS18** (release train): a standing procedure (PLAN §6).

**"Base SDK integrated" (the M1 exit)** uses R3's definition with three amendments:
1. "Defined in base-sdk" means defined on `node/base-sdk`, which may be ahead of upstream.
2. The wallet, SPV engine, BIP158/bloom, wallet sighash and RPC stay in rust-dashcore by design.
3. The test is mechanical (rev1 wording, F17):
   - the **base-sdk** items BS3 (bsdk part), BS4a/b, BS5, BS6a/b, BS7, BS8, BS11a-t, BS11a, BS11b, BS12 and BS13 are merged into `node/base-sdk` and pinned;
   - the **rust-dashcore** items BS3 (rdc part), BS15 and BS16 are merged into `node/integration`;
   - BS9a/b and BS10 finish by M5, BS14 at M6, BS18 off the critical path;
   - BS2 is green over full mainnet and testnet history across all three implementations;
   - every BS4b/BS6b/BS7/BS8 differential fuzz target has ≥ 24 CPU-hours clean, and `diff_bls_*` ≥ 72;
   - `cargo deny` proves no node crate depends on `dashcore`;
   - `dashcore` compiles Platform's usage patterns unchanged.

**Node work that starts before the gate:**
- `dash-consensus::script` (interpreter, sighash, sigops) on today's base-sdk `Transaction`;
- PoW, `fp`, subsidy and payment arithmetic (temporary constants until BS5);
- `dashd-store` and the bake-off; block files;
- `dash-args`, `dash-rpc-spec`, the RPC skeleton and `rdash-cli` (which works against dashd from day one);
- the oracle shim and the differential-fuzz scaffolding; vectors and the quirk scaffolding; chain scans; the golden-digest extractor;
- B1.

### 1.8 Convergence track (non-blocking; one implementer after M4)

R3 Phase 3, re-ordered so the node never waits on it. Each step carries a Platform-impact note. Nothing is proposed upstream without pasta (DEC-09).

| ID | Step | R3 |
|---|---|---|
| CV1 | Delete Bitcoin vestiges (#400: `Witness`, taproot, `Weight`, `Wtxid`, segwit/bech32 payloads) | 3.1 |
| CV2 | Route ECDSA through `dash-pkc` (with a lax-DER parse helper for `CPubKey::Verify` parity) | 1.6 |
| CV3 | `Network` and LLMQ types onto `dash-params`/`dash-primitives`; payloads become wrappers that own bincode | 3.2–3.4 |
| CV4 | Hash newtypes (0.14 API shims); Tx/Block wrappers; Script/Amount/Address; dash-spv P2P onto `dash_p2p_core::P2pMsg` (#264 rebased) | 3.5–3.9 |
| CV5 | Optional: dash-spv "client mode" on `dash-p2p-transport` and the Dash-extended BIP158 builder from `dashd-indexes::filter` (also fixes #1116/#332 by adopting `dash-consensus` difficulty) | — |

End state: `dashcore` is a thin, bincode-owning façade over base-sdk; `dashcore::bridge` becomes the identity; one codec remains.

### 1.9 Bridge rules at the wallet seam

- `dashd-wallet` consumes `ChainEvent`s carrying `Arc<dash_consensus::Block>`. Before calling key-wallet, a **relevance pre-filter** runs on the base-sdk transactions. It is a hash-set test on raw output scripts, spent outpoints and special-tx elements (ProTx keys, proTxHash, collateral). Only matching transactions are bridged (`dashcore::bridge`, a byte round trip of ~µs per tx) and handed to `WalletInterface`. The bridge is never on the IBD hot path; (F's per-block full-block conversion, about 1–3 ms per MB, is rejected).
- Hash newtypes convert by `from_byte_array`/`to_byte_array`, with no copies.
- ZMQ `raw*` topics publish the original canonical bytes; no conversion.
- Once CV4 lands, the bridge is the identity and the adapter API does not change.

---
## 2. Crate architecture

### 2.1 Principles

1. **Two layers, enforced by dependencies.**
   - **Pure crates** (`dash-consensus`, `dash-evo`, `dash-llmq`, `dash-governance`, `dash-coinjoin`) take no dependency on tokio, RocksDB, sockets, clocks, threads or globals. All context is passed in explicitly as values or trait-object views: `&ChainParams`, a `DeploymentView`, the `BlsCtx`, the time. These crates are `#![forbid(unsafe_code)]`, banned from I/O, threads, clocks and globals by `cargo deny` and clippy lists (not `no_std`: X-1), directly fuzzable and differentially testable.
   - **Runtime crates** (`dashd-*`) own I/O, threads, persistence and policy.
2. **Naming.**
   - `dash-*` is a reusable library: the pure crates, plus `dash-p2p-transport`, `dash-args`, `dash-rpc-spec` and `dash-coreimport`.
   - `dashd-*` is internal to the daemon.
   - `rdash*` is a binary (DEC-03).
   - None of these names collides with base-sdk's eight crate names (`dash-types`, `dash-num`, `dash-pkc`, `dash-pow`, `dash-script`, `dash-primitives`, `dash-params`, `dash-p2p-core`) or with existing rust-dashcore packages.
   - Directories are top-level, matching the existing flat layout (`dash-spv/`, `key-wallet/`). BS2's directory `conformance/` keeps its name (package `dash-conformance`).
3. **Port Core's structure; don't reinvent it.**
   - Module and function names mirror Core (`connect_block` ↔ `ConnectBlock`, `build_new_list_from_block` ↔ `BuildNewListFromBlock`).
   - Every ported function carries an anchor comment, `// Core: validation.cpp:2470 ConnectBlock`, pinned to the milestone's Core commit, so reviewers can diff the logic side by side.
   - Each crate's `ARCHITECTURE.md` lists the Core files it mirrors and every deviation (Appendix B).
4. **One writer per state domain.** Chainstate, coins, evo state, the block index and mined commitments are mutated only by the validation thread. Everyone else reads published immutable snapshots. There is no global `cs_main`.
5. **Explicit context instead of globals.** There is no `Params()`, no `bls_legacy_scheme` and no `GetTime()`.
   - `NodeClock` (real time plus mocktime, used by `setmocktime`/`mockscheduler`) is injected everywhere.
   - `ConsensusCtx` is built per block (Appendix A).
6. **Core reject strings and result classes everywhere** (FR-CON-31). Every rejection is an enum whose `Display` is Core's exact string, tagged with Core's `BlockValidationResult` or `TxValidationResult` class. P2P penalties and RPC errors derive from the class.
7. **Determinism (FR-CON-33).**
   - No `HashMap` iteration on consensus paths: a clippy `disallowed_methods` configuration plus a reviewed allowlist enforce this in the pure crates and in `dashd-chainstate`.
   - No floating point outside `dash_consensus::fp`. No `std::time` outside `NodeClock`.
   - Core's `std::map`/`uint256 <` orderings are reproduced as raw-byte `memcmp`.
8. **Isolation from existing consumers.**
   - None of the new crates is a dependency of `dashcore`, `key-wallet`, `key-wallet-manager`, `dash-spv` or the FFI crates (NFR-CMP-03).
   - The only workspace-wide effects are the base-sdk `[patch]` (BS3) and the committed `Cargo.lock`.
   - Nothing is changed under `dash-spv/src/network/`, which avoids conflicts with #902 (NFR-CMP-04).
9. **Licensing (DEC-07).**
   - Every new crate that contains code or data derived from Dash Core is **MIT** with Dash Core's copyright notice. That covers all new crates, including `dash-conformance` once Core vectors land in it.
   - Existing crates stay CC0. A CC0 crate that needs Core-derived logic (for example key-wallet needing CoinJoin rounds or `IsFullyMixed`) depends on the MIT crate (`dash-coinjoin`) instead of absorbing the code.

### 2.2 Crate map

| Crate (dir) | Kind | Responsibilities | Core counterpart | Key deps |
|---|---|---|---|---|
| `dash-consensus` | pure ★ | Node primitives (`Tx`, `Block`, decoders, `core_serialize`; §1.3). Script interpreter, flags, `ScriptError`, legacy sighash with `vExtraPayload`, DIP-0020, CHECKDATASIG, sigops, the ECDSA consensus layer. `check_transaction`, `contextual_check_transaction`, `check_tx_inputs`, sequence locks and finality. Header/block checks (merkle + mutation, v4 asset-unlock root), contextual header/block checks. PoW (BTC retarget, KGW, DGW v3, `check_proof_of_work`, chain work). **`fp`** (bit-exact `pow`, `ConvertBitsToDouble`; §4.5). Subsidy, MN payment schedule, platform share, treasury part. Buried deployments and Dash versionbits (dynamic threshold, EHF signal height). `Coin`/undo codecs, `CoinsView`, MuHash3072 and `hash_serialized_2` helpers. Checkpoint view | `consensus/*`, `script/*`, `pow.cpp`, `versionbits.cpp`, `deploymentstatus.*`, `coins.h`, `compressor.*`, `undo.h`, `kernel/coinstats.cpp`, pure parts of `validation.cpp` | base-sdk, `secp256k1` |
| `dash-evo` | pure ★ | Special-tx contextual checks (all 13 types × versions), the shared-collateral covenant, NetInfo validation. **Deterministic MN list** (`MnList`, persistent structures, `build_new_list_from_block`, PoSe, payee queue). Payments (`get_block_tx_outs`, `is_block_payee_valid`, `is_block_value_valid` with a superblock view). CbTx. Credit pool and limit eras. MNHF signals. SML entry, `merkleRootMNList`, `mnlistdiff` builder. **`llmq` module (consensus):** param helpers, member selection (non-rotated + DIP24 quarters, skip lists, snapshots emitted as effects, §2.10.1), modifier, commitments (`verify`/`verify_null`, `build_commitment_hash`), in-block rules (required counts, mining windows, null commitments), `merkleRootQuorums`, `scan_quorums`, `select_quorum_for_signing`, sign hash and request IDs, recovered-sig/CL/IS verification, `qrinfo` builder. The per-block orchestrator `process_special_txs_in_block`. Our MN-list diff/snapshot codecs, and the `EvoStore` trait | `evo/*`, `masternode/payments.*`, `llmq/{commitment,blockprocessor,utils (selection),snapshot,quorumsman (ScanQuorums, SelectQuorumForSigning, VerifyRecoveredSig),signhash}`, `chainlock/clsig.*`, `instantsend/lock.*` | dash-consensus, `imbl` |
| `dash-llmq` | pure | DKG session as a sans-io state machine (contribute, complain, justify, commit, finalize, sim errors), BLS-IES use, threshold/Lagrange helpers (Pippenger MSM) over `dash-pkc`, sig-share session and recovery math, quorum connection topology (relay ring `(i + 2^k) mod n`, full mesh under spork 21 with the deterministic initiator) | `llmq/dkgsession*` (logic), `llmq/signing_shares` (logic), `llmq/utils.cpp` (connections), `bls/bls_ies.*` | dash-evo, `dash-pkc` |
| `dash-governance` | pure ★ | The four hashes (object, signature, data, vote) with Core's quirks; signature-key selection (ECDSA voting key vs BLS operator key); the proposal validator with Core's messages; the byte-exact trigger template; `ParsePaymentAmount`; `is_valid_superblock` (V24 duplicate rule); `best_superblock`; `get_payments_limit`; tallies with Evo weight 4; the rate-check buffer | `governance/{common,object,vote,superblock,classes,validators}.*` (logic) | dash-evo |
| `dash-coinjoin` | pure | Denominations; collateral validity; `is_valid_in_outs` (STANDARD, PROMOTION, DEMOTION, FINAL); DSTX structure (pre/post V24); queue validity; `is_mixing_threshold_exceeded`; server and client session state machines; rounds and `is_fully_mixed` with salt. One implementation shared by server and client (FR-CJC-04) | `coinjoin/{common,coinjoin,util}.*`, `wallet/coinjoin.cpp` (logic) | dash-evo |
| `dashd-store` | runtime | `KvEngine` trait (ST1: `write(batch, Commit::{Buffered, Durable})`, `sync_wal`, `flush_memtables`, `snapshot`) with a RocksDB engine and an in-memory engine (unit tests and model fuzzing); ST3's `FaultyEngine`/`FaultyFlatFiles` (power-loss model, error injection); typed tables `Table<K: KeyCodec, V: ValueCodec>`; `FlatFileSeq` (blk/rev, Core framing, preallocation, `fdatasync`, directory `fsync`, the link-count rule of §2.11); our `FlatDb<T>` envelope; datadir lock | `dbwrapper.*`, `flatfile.*`, `flat-database.h` | `rocksdb` |
| `dashd-chainstate` | runtime ★ | `ChainstateManager` actor on the validation thread: block-index arena, active chain, header acceptance, `accept_block`, `activate_best_chain`, connect/disconnect, reorg, invalidate/reconsider/precious, ChainLock enforcement entry points. Layered coins cache with freeze-and-flush and the epoch-fenced prefetch (§3.4); undo I/O; the durability and recovery protocol (§3.5); the effect channel (§2.10.1). Evo state store (MN-list cache, diffs/snapshots), mined-commitment index, rotation-snapshot store, credit pool, MNHF, versionbits cache. `ChainView` publication and the `ChainEvent` bus. Sigcache, script-exec cache, check queue. Flush, crash replay, `-reindex(-chainstate)`, `-loadblock`, `test_block_validity`, `verifychain`, pruning | `validation.cpp`, `node/blockstorage.*`, `node/chainstate.*`, `txdb.*`, `chain.*`, `kernel/*`, `validationinterface.*`, `script/sigcache.*`, `checkqueue.h`, state parts of `evo/{evodb,deterministicmns,creditpool,mnhftx}` and `llmq/{blockprocessor,snapshot}` | dash-evo, dashd-store, `rayon`, `arc-swap`, `crossbeam` |
| `dashd-mempool` | runtime ★ | `TxMemPool` with every Dash map, two-phase ATMP, policy and standardness, orphanage, IS-aware conflict and eviction handling, expiry, persistence, the fee estimator, the ancestor-score package index for mining, mempool address/spent indexes | `txmempool.*`, `MemPoolAccept`, `policy/*`, `txorphanage.*`, `kernel/mempool_*` | dashd-chainstate |
| `dashd-mining` | runtime ★ | `BlockAssembler` (commitments first, packages, credit-pool diff, EHF stage, IS safety), CbTx field computation, `FillBlockPayments`, GBT/`submitblock`/`generate*` back ends, template cache, regtest internal miner (X11 grinding) | `node/miner.*`, `rpc/mining.cpp` (logic) | dashd-mempool, dashd-governance (payments view) |
| `dash-p2p-transport` | runtime lib | V1 framing over base-sdk codecs; V2 framing and short IDs from base-sdk (`p2p_core/src/bip324.rs`, `short_id.rs`; m-21); **the BIP324 handshake and state machine lifted from #264** (its `bip324` crate already takes the network magic and Core's `bitcoin_v2_shared_secret` salt prefix, `core: bip324.cpp:38`) and generalised to both initiator and responder, with short-ID version gating, inbound v1 detection and v1 reconnect fallback; an async `Transport` trait; **transport work only, no payload decoding** (§2.14). Reusable by dash-spv later (CV5) | `net.cpp` transports, `bip324.*`, `crypto/{chacha20,poly1305,ellswift,hkdf}` | tokio, `bip324`, `dash-p2p-core` |
| `dashd-p2p` | runtime | **net:** `ConnMan` (listeners, outbound slot types, feelers, anchors, addnode, DNS and fixed seeds, MN connection API), Core-faithful addrman (with a format that also imports Core's `peers.dat` V4_MULTIPORT), banman (`banlist.json`, discouragement filter), eviction, permissions, `-maxuploadtarget`, `-capturemessages`. **processing:** the `PeerManager` actor (`net_processing` port, Core's wake-driven loop), both object-request trackers (v23's legacy `ObjectDownloadState` and v24's `TxRequestTracker`, selected by `ReleaseProfile`), the per-command decode policy, block-download scheduler, headers/headers2 sync, compact blocks, serving, BIP37 with Dash special-tx matching, BIP157 serving, the misbehaviour table, the `NetHandler` registry, the `fingerprint.rs` table and the P2P part of `ReleaseProfile` (§2.25). Privacy networks (SOCKS5, Tor control, I2P SAM 3.1, CJDNS, asmap, NAT-PMP/UPnP) | `net.*`, `net_processing.*`, `netbase.*`, `netaddress.*`, `addrman*`, `addrdb.*`, `banman.*`, `net_permissions.*`, `node/eviction.*`, `txrequest.*`, `blockencodings.*`, `merkleblock.*`, `common/bloom.*`, `torcontrol.*`, `i2p.*`, `mapport.*` | dash-p2p-transport, dashd-chainstate, dashd-mempool |
| `dashd-masternode` | runtime | Active masternode manager (WAITING_FOR_PROTX…READY…ERROR), MNAUTH (all nodes verify; MNs sign), masternode sync (`SyncManager` 6 s tick, BLOCKCHAIN → GOVERNANCE → FINISHED; runs on every node), mncache metadata (incl. the platform-ban-reset effect), netfulfilled, platformban processing, MN connection maintenance; **sporks** (module `sporks`, folded in from v1's `dashd-sporks`, X-5): store (our format; Core `sporks.dat` importable), the spork table and signer model per `ReleaseProfile`, mainnet hard-coded values, relay, `sporkupdate` signing, `SporkView` | `active/masternode.*`, `active/context.*`, `evo/mnauth.*`, `masternode/{sync,meta,utils}.*`, `node/sync_manager.*`, `netfulfilledman.*`, `spork.*` | dashd-p2p, dashd-quorums |
| `dashd-quorums` | runtime | Quorum manager (cache, building, vvec/sk-share store in `llmq/`), signing manager (recsig DB, conflict and vote tracking, listeners), sig-shares manager, DKG session manager plus per-(type, quorumIndex) phase handlers, data recovery (`qgetdata`/`qdata`), connection planner, EHF signal signer, NetQuorum/NetSigning/NetDKG handlers | `llmq/{quorums,quorumsman,signing,signing_shares,net_*,dkgsessionmgr,dkgsessionhandler,ehf_signals,debug}.*`, `active/dkgsession*`, `bls/bls_worker.*`, `bls/bls_batchverifier.h` | dash-llmq, dashd-chainstate, dashd-p2p |
| `dashd-locks` | runtime ★ | ChainLock handler and signer; InstantSend manager, DB and signer; the CL/IS back ends of `ChainHelper` | `chainlock/*`, `instantsend/*` | dashd-quorums, dashd-mempool |
| `dashd-governance` | runtime | Governance manager (objects, votes, orphans, postponed objects, rate buffers, store), the `govsync` handler, the trigger store (the superblock back end of `ChainHelper`), the governance signer (MN triggers and votes) | `governance/{governance,net_governance,signing}.*` | dash-governance, dashd-p2p, dashd-indexes (txindex for collateral) |
| `dashd-coinjoin` | runtime | CoinJoin server (MN), DSTX manager (24-block expiry, CL purge), DSQ relay modes, DSTX relay policy hook used by `dashd-p2p` | `coinjoin/server.*`, runtime parts of `coinjoin.cpp` | dash-coinjoin, dashd-p2p, dashd-mempool |
| `dashd-indexes` | runtime | `BaseIndex` framework (async, own DB, locator, prune locks): txindex (compact keys), addressindex, spentindex, timestampindex, blockfilterindex (Dash-extended BIP158), coinstatsindex (MuHash3072) | `index/*`, `blockfilter.*`, `evo/specialtx_filter.*` | dashd-chainstate |
| `dashd-wallet` | runtime | Node wallet service on key-wallet and key-wallet-manager: multiwallet, SQLite persistence, chain sync and rescans, wallet RPC back ends, CoinJoin client, ProTx/gobject/BLS/EdDSA signing ops | `wallet/*`, `coinjoin/{client,walletman}.*` | key-wallet(-manager), dash-coinjoin, dashd-chainstate, dashd-mempool, `dashcore` (bridge) |
| `dash-rpc-spec` | shared | Declarative RPC table: names and Dash two-word sub-commands, args (types, defaults, `a\|b` aliases, `skip_type_check`), categories, hidden and wallet gating, **Core's help text verbatim**, the dash-cli conversion table, whitelist naming | `rpc/util.*` (RPCHelpMan), `rpc/client.cpp` | serde_json |
| `dashd-rpc` | runtime | HTTP/1.1 server, auth, `-rpcwhitelist`, work queues incl. `-rpcexternaluser`, JSON-RPC 1.x dispatch with sub-command squashing, batches, `/wallet/<name>`, REST, per-category handlers, error codes, Core number formatting; **ZMQ** behind feature `zmq` (19 topics with Core framing and sequence numbers; folded in from v1's `dashd-zmq`, X-5) | `httpserver.*`, `httprpc.*`, `rpc/*`, `rest.cpp`, `zmq/*`, `core_write.cpp`, `evo/core_write.cpp`, `llmq/core_write.cpp` | hyper, dash-rpc-spec, `zmq` (feature), all subsystems |
| `dash-args` | shared | ArgsManager port: `dash.conf` sections, `includeconf`, NETWORK_ONLY options, negation, `settings.json`, chain selection, help generation, Core's full option table | `common/args.*`, `common/settings.*`, `util/system.*` | — |
| `dashd-node` | runtime | Node assembly: `NodeContext`, init steps in `AppInitMain` order, shutdown, scheduler, `NodeClock`, Core-format logging, notifications, statsd, pid and lock, divergence tripwire (ChainLock conflicts and the digest log only; X-6) | `init.cpp`, `node/*`, `logging.*`, `scheduler.*`, `shutdown.*`, `timedata.*`, `warnings.*`, `stats/*` | everything |
| `dash-coreimport` | shared | Readers for a Core datadir (all read-only): LevelDB (pure Rust, applies `\0obfuscate_key`), `blk`/`rev` iteration and undo checksum, block index, chainstate, evodb (order-agnostic `dmn_S3`, legacy `dmn_D3`, `mnhf_s`/`mnhf_s2`, `cpm_S`, `q_mc*`, `llmq_S`), `llmq/*` DBs, legacy in-index address/spent/timestamp rows, Core flat files (`governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`, `peers.dat` format 4 / compat 36, `anchors.dat`, `banlist.json`, `mempool.dat` v1, `fee_estimates.dat`, `settings.json`, onion/I2P keys). Feature `wallet`: SQLite wallet, a BDB 4.8 btree reader (leaf and overflow pages), dump formats, Core's crypter (decrypt only) | `txdb.*`, `node/blockstorage.*`, `evo/evodb.*`, `flat-database.h`, `addrdb.*`, `wallet/{bdb,sqlite,walletdb,crypter,dump}.*` | `rusty-leveldb` (a C++ `leveldb` read-only binding if it proves unsuitable; no in-house LevelDB parser, F20), dashd-store codecs, key-wallet (wallet feature) |
| `rdashd` | bin | Daemon `main` (`-daemon`/`-daemonwait`, signals, `-version`) | `bitcoind.cpp` | dashd-node |
| `rdash-cli` | bin | The dash-cli equivalent: a generic JSON-RPC client driven by the spec's conversion table; `-getinfo`, `-netinfo [0-4\|help]`, `-addrinfo`, `-generate`, `-rpcwait`, `-named`, `-stdin*`, `-rpcwallet`, `-color`; later tool subcommands `tx`, `util grind` | `bitcoin-cli.cpp`, `bitcoin-tx.cpp`, `bitcoin-util.cpp` | dash-args, dash-rpc-spec |
| `rdash-wallet` | bin | Offline wallet tool: `info`, `create`, `dump`/`createfromdump` (Core's `BITCOIN_CORE_WALLET_DUMP,1` format), **`import-core`** (BDB/SQLite `wallet.dat`, `dumpwallet` text, dash-wallet dumps), `import-mnemonic` (Core quirks: no NFKD, XOR-checksum acceptance), block export (linearize-style) | `bitcoin-wallet.cpp`, `wallet/wallettool.*` | dashd-wallet, dash-coreimport |
| `dash-conformance` | test (exists) | BS2 codec equality; vectors loader; StateDigest extractor and comparator; `replay`; `mutate`; `rpcdiff`; `p2pdiff` glue; `functional` runner glue; `scan` (chain scans); `shadow` comparator | — | dashcore, base-sdk, dash-oracle-sys (feature), dashd-chainstate (replay feature) |
| `dash-oracle-sys` | test | bindgen FFI to `libdash_oracle` (§4.4), behind feature `oracle`; never a runtime dependency | — | — |
| `dash-testkit` | test | Process harness. It wraps dash-spv's existing `test-utils` feature (`DashdTestContext`, `DashCoreNode`, `masternode_network.rs` with `mine_dkg_cycle`, `wait_for_chainlocked_block`) **without moving code**, and adds `RustNode` (spawns `rdashd` on a temp datadir), `MixedNetwork { dashd, rust }`, mocktime control, log retention (`DASHD_TEST_RETAIN_DIR`) and `DASHD_IMPL=rdashd` for the dash-spv suites (SPV-as-consumer) | `test_framework/*` helpers | dash-spv (`test-utils`), rpc-client |
| `node-fuzz` | test (workspace-excluded) | cargo-fuzz / libFuzzer targets, both single-implementation and differential (§4.4) | `test/fuzz/*` | pure crates, dash-oracle-sys |
| `node-bench` | test (B1, Python) | Side-by-side harness `node-bench/nodebench` with scenarios `ibd-*`, `reindex-*`, `rescan-mainnet`, `startup-*`, `mempool-accept`, `gbt-regtest`, `rpc-*`, `microbench`, `spv-headers-*`, `mn-{dkg,recsig,islock,chainlock,coinjoin}-regtest`; baselines dashd **23.1.8** (mainnet workloads) **and 24.0.x** (testnet/regtest/devnet workloads; NFR-PERF-02(d), F11); plus PF0's deterministic-counter mode (§3.11) | — | — |

### 2.3 Dependency graph, cycle breakers and rules

```
 base-sdk (fork node/base-sdk):  dash-types ─ dash-num ─ dash-pkc ─ dash-pow ─ dash-script ─ dash-primitives ─ dash-params ─ dash-p2p-core
                                                    │
 pure:        dash-consensus ◄─────────────────────┘
                   ▲
              dash-evo  (DML, special txs, CbTx, credit pool, MNHF, payments, SML, llmq-consensus)
               ▲   ▲   ▲
       dash-llmq  dash-governance  dash-coinjoin
                   ▲
 runtime:    dashd-store ─► dashd-chainstate ─► dashd-mempool ─► dashd-mining
                               ▲   ▲   ▲            ▲
                 dashd-indexes ┘   │   └── dashd-locks (→ quorums, mempool)
   dash-p2p-transport ─► dashd-p2p ◄┴──────────────────┐
                           ▲   ▲   ▲   ▲   ▲           │
                           │   │   │   └── dashd-coinjoin
           dashd-masternode (+ sporks) ┘   └────── dashd-governance (→ indexes)
           dashd-quorums (→ dash-llmq, chainstate) ┘
           dashd-wallet (key-wallet, key-wallet-manager, dash-coinjoin, chainstate, mempool, dashcore::bridge)
 services:   dash-rpc-spec ─► dashd-rpc (+ REST, feature zmq) ;  dash-args ;  dash-coreimport
 assembly:   dashd-node ─► rdashd ;  rdash-cli (dash-args, dash-rpc-spec) ;  rdash-wallet (dashd-wallet, dash-coreimport)
 tests:      dash-conformance, dash-oracle-sys, dash-testkit, node-fuzz (dev/CI only)
 existing (public API untouched): dashcore (+ bridge feature) ─ key-wallet ─ key-wallet-manager ─ dash-spv ─ *-ffi ─ rpc-json ─ rpc-client
```

**Cycle breakers.** Lower crates define traits; higher crates implement them; `dashd-node` wires them.
- **Validation → ChainLocks, InstantSend, governance, MN sync.** `dashd-chainstate` holds an `Arc<dyn ChainHelper>` (Appendix A), with `NoOpChainHelper` for tests and `ReplayChainHelper` for replay. `dashd-node` wires it to `dashd-locks`, `dashd-governance` and `dashd-masternode`.
- **P2P → overlays.** Overlays implement `NetHandler`; `dashd-node` registers them in Core's order.
- **Chainstate → mempool.** `remove_for_block` and the disconnect pool go through a `MempoolSink` trait implemented by `dashd-mempool`.
- **`dash-evo` → LLMQ consensus.** Both live in `dash-evo` (the `llmq` module), so there is no cycle. **O** put member selection and commitments in a separate pure `dash-llmq` above `dash-evo`, which made the CbTx check and `process_special_txs_in_block` straddle two crates. **F** put them in `dash-evo`. F's split is chosen; `dash-llmq` keeps only DKG and sig-share protocol math.

**Enforced rules.**
- `cargo deny` bans: `dashcore`/`dash-spv` in node crates (R2), `tokio`/`rocksdb`/`std::net` users in pure crates, and `bincode` in node crates.
- `#![forbid(unsafe_code)]` everywhere except `dashd-store` (RocksDB FFI, `fdatasync`) and `dash-oracle-sys`.
- A clippy `disallowed_methods`/`disallowed_types` configuration covers `HashMap` iteration, `f64` outside `fp`, and `SystemTime` outside `NodeClock` (determinism).

**CI groups** (`.github/ci-groups.yml`; run locally by the integration gate):
- `node-pure`: the five pure crates and their purity bans.
- `node-runtime`: store, chainstate, mempool, mining, indexes, p2p, transport, masternode (with sporks), quorums, locks, governance, coinjoin.
- `node-app`: rpc (with REST and the `zmq` feature), rpc-spec, args, node, wallet, coreimport, binaries.
- `node-conformance`: conformance, oracle-sys, testkit, node-fuzz smoke. It needs the oracles and dashd.

The existing `core/spv/wallet/ffi/rpc` groups are untouched.

### 2.4 `dash-consensus`

Scope: everything Core computes in `consensus/`, `script/`, `pow.cpp`, the subsidy, `GetBlockScriptFlags` and `ContextualCheck*` parts of `validation.cpp`, the payment arithmetic of `masternode/payments.cpp`, `versionbits.cpp` and `deploymentstatus`. Signatures are in Appendix A. Notable modules:

- **`script`**:
  - `ScriptFlags(u32)`, with bits exactly as in `core: script/interpreter.h`. Bit 15 is retired, DIP0020 is always on, and only P2SH, DERSIG, CLTV, CSV and NULLDUMMY are consensus.
  - `ScriptError`: one variant per Core `ScriptError_t`, with `Display` equal to Core's string.
  - `SignatureChecker` trait; `eval_script`, `verify_script`, legacy `signature_hash`.
  - `ScriptNum`: 4-byte default, minimal encoding, and the `OP_NUM2BIN` `getint()` clamp quirk.
  - Sigop counting: legacy and P2SH; `CHECKDATASIG` is not counted.
- **Fidelity notes** (each is a test; R4 quirks 2–10):
  - `OP_CHECKDATASIG` uses `CheckSignatureEncoding` (DER plus exactly one trailing hash-type byte under DERSIG; a defined type under STRICTENC), a single SHA256 of `msg`, NULLFAIL, lax verify over the full sig, zero sigops and no sigcache.
  - `OP_NUM2BIN` size is computed via `getint()` and then u64; the `OP_BIN2NUM` limit is 4 bytes.
  - `FindAndDelete` of the signature in BASE sigversion, including `OP_0` removal for empty sigs.
  - `MAX_OPS_PER_SCRIPT` (201) counts `CHECKDATASIG`. Disabled opcodes, `OP_VERIF` and `OP_VERNOTIF` fail even in unexecuted branches.
  - `MAX_STANDARD_TX_SIZE` (100,000) is consensus `>` after DIP0001 (`bad-txns-oversize`); policy uses `>=`.
  - `CheckBlock` uses the relaxed limits: 2,000,000 bytes, 2,000,000 txs, 40,000 sigops. Contextual limits depend on height.
  - BIP34 is a byte-prefix compare of `CScript() << h`, applied only while BIP34 is active **and** DIP0003 is not.
  - ECDSA at consensus is lax-DER parsing plus S normalisation (high-S verifies), and hybrid 06/07 keys are accepted.
  - C++ exceptions (ScriptNum overflow or non-minimal encoding, empty pops) map to `UNKNOWN_ERROR`.
  - A failing input is re-run without the non-mandatory flags, to classify `mandatory-script-verify-flag-failed` versus `non-mandatory-script-verify-flag`.
- **`pow`**:
  - BTC retarget, KGW (`f64` and `fp::pow`) and DGW v3 (the 24-block window spans 23 intervals, with per-step truncation and wrapping U256). DGW needs `last.height >= 24`, otherwise it returns powLimit.
  - Testnet/devnet minimum-difficulty rules (7200 s → powLimit; 600 s → ×10 clamp with the testnet wrap), devnet `-minimumdifficultyblocks`, regtest `fPowNoRetargeting`.
  - `check_proof_of_work`.
  - `mainnet_pre_68589_tolerance`: `|d(bits) - d(expected)| <= 0.5·d(bits)` via `ConvertBitsToDouble`. This is the only check applied to BTC-retarget and KGW blocks; DGW is exact from 68590 and on every other network (R4 F4, quirks 8–9).
- **`subsidy` / `payments`**:
  - `block_subsidy_inner(prev_bits, prev_height, v20, params)`: three eras computed in doubles; truncation before clamping; the mainnet `nPrevHeight <= 4500` dDiff bug; the iterative 1/14 decline per 210240 blocks; devnet high subsidy; a fixed base of 5 from V20 (and on devnets from `nHighSubsidyBlocks`).
  - The treasury part is /10 before V20 and /5 from V20, using a strict `>` against `nBudgetPaymentsStartBlock`.
  - `masternode_payment` schedule: separately truncated increments, period 8 skipped, the reallocation start rounded to the superblock cycle, the BRR table, and 75% after V20.
  - `platform_share` = MN reward on subsidy × 375/1000.
  - `split_amount_by_shares` uses u128 intermediates, and the last share gets the remainder.
  - Quirks 11, 12, 15.
- **`deployments`**:
  - `BuriedDeployment { HeightInCb, DerSig, Cltv, Bip147, Csv, Dip0001, Dip0003, Dip0008, Dip0020, Dip0024, Brr, V19, V20, MnRr, Withdrawals }`.
  - `DeploymentView::{active_after, active_at}` reproduces each call site's choice (R2 A.3 §16).
  - `versionbits_state`:
    - the walk-back stops at `MinBIP9WarningHeight` (a consensus floor);
    - the dynamic threshold's `attempt*attempt*period` is an i32 product in Core and wraps at about 730 attempts, so we use `wrapping_mul` and never panic;
    - historic MNHF signals for bits 10, 11 and the abandoned 12 are valid and stored.
  - `compute_block_version` = `0x20000000 | STARTED/LOCKED_IN bits`. Quirks 31–32.
- **`coins`**:
  - `Coin` = `VARINT(height*2+cb) ‖ CompressAmount ‖ ScriptCompression`, the undo-record codecs, and the `CoinsView` trait.
  - `MuHash3072` and `hash_serialized_2` reproduce Core's operator-precedence bug: `VARINT(1)` is written instead of the height/coinbase code (`core: kernel/coinstats.cpp:77`, quirk 33).

### 2.5 `dash-evo`

This crate is the consensus-critical Dash state. It is pure (no I/O, clocks or globals), with persistent data structures from `imbl` (O(1) clone per block, structural sharing).

**Data model** (O's sorted maps, so that iteration is in Core's order and deterministic):

```rust
#[derive(Clone)]
pub struct MnList {
    pub block_hash: BlockHash, pub height: u32,
    pub total_registered: u32,                           // nTotalRegisteredCount
    by_pro_tx: imbl::OrdMap<ProTxKey, Arc<Dmn>>,        // ProTxKey: Ord = memcmp of LE bytes (Core uint256 order)
    by_internal_id: imbl::OrdMap<u64, ProTxKey>,
    unique: imbl::HashMap<UniqueKey, (ProTxKey, u32)>,  // SerializeHash(property) -> (owner, refcount); lookups only, never iterated
    payee_queue: imbl::OrdSet<PayeeKey>,                 // (effective_last_paid_height, ProTxKey) over valid MNs
}
pub struct Dmn {                                         // immutable; replaced on change
    pub pro_tx_hash: ProTxHash, pub internal_id: u64, pub collateral: OutPoint,
    pub operator_reward: u16, pub mn_type: MnType, pub state: Arc<MnState>,
    sml_hash: OnceLock<Hash256>,                         // cached SML entry hash
}
pub struct MnState { version: u16, registered_height: u32, last_paid_height: u32, consecutive_payments: u32,
    pose_penalty: i32, pose_revived_height: i32, pose_ban_height: i32, revocation_reason: u16,
    confirmed_hash: BlockHash, confirmed_hash_with_protx: Hash256 /* single SHA256(protx||confirmed) */,
    key_id_owner: Option<PubKeyHash>, pub_key_operator: BlsPkBytesVersioned, key_id_voting: PubKeyHash,
    net_info: NetInfo /* Mn (v<3) | Ext (v>=3) */, payouts: Vec<Payout>, shares: Vec<Share>,
    early_period_blocks: u32, early_penalty: i64, script_operator_payout: ScriptPubKeyBuf,
    platform_node_id: EddsaPkHash, platform_p2p_port: u16, platform_http_port: u16 }
pub struct MnListDiff { added: Vec<Dmn>, updated: Vec<(u64, MnStateDiff)>, removed: Vec<u64> }
pub struct MnStateDiff { fields: u64 /* Core's field bitmask */, .. }
```

**`build_new_list_from_block` encodes Core's exact order** (R2 A.3 §4.2, R4 quirk 25):
1. The payee is computed from `prev` first.
2. Confirmation: at `prev.height - registered >= masternode_minimum_confirmations` (mainnet 15, otherwise 1), `confirmed_hash = prev.hash`.
3. `decrease_scores` (decay PoSe).
4. Special txs in block order:
   - ProReg replaces an existing MN with the same collateral; duplicate checks run against the list *as rebuilt so far*;
   - `BanIfNotBanned` on empty netInfo (since v23, invalid `CService` bytes normalise to empty and the MN registers banned);
   - the V24 operator key is accepted under either scheme;
   - ProUpServ with pre/post-V24 version semantics and `Revive`;
   - ProUpReg with `ResetOperatorFields`;
   - ProUpRev, whose version drops to 1 before V24;
   - types 11 and 12;
   - QcTx PoSe `CalcPenalty(66)` for `!validMembers`, interleaved in order. The penalty is added even to already-banned MNs, and `max(100, totalMNs)` is evaluated on the partly rebuilt list (quirk 24).
5. A second pass over collateral spends (shared collateral only via type 10).
6. Payee bookkeeping: bump `last_paid_height`; Evo `consecutive_payments` with the MN_RR switch; the streak reset.

**Payee selection.**
- `compare_by_last_paid`: the revived height overrides; fall back to `registered_height`; tie-break by proTxHash memcmp. Unconfirmed MNs are payable (quirk 13).
- The Evo "pay 4 in a row" rule between V19 and MN_RR is checked before the queue lookup.
- **Payee queue:** O(log n) via `payee_queue`, with a line-by-line **reference twin** (a linear scan) under `cfg(any(test, feature = "xcheck"))`.
- External collateral must be in the `pprev` UTXO set. Special txs never see outputs from the same block (quirk 26).
- `MakeSignString` depends on the network (base58 prefixes) and uses the "DarkCoin Signed Message" magic (quirk 27).

**Other modules:**
- **`sml`:** `SimplifiedMnListEntry` with version-dependent hash exclusions (the leaf hash excludes `nVersion`, quirk 17). `merkle_root_mn_list` runs over **all** MNs (banned included) in memcmp order, using the cached `Dmn::sml_hash` and SIMD SHA256D64; an empty list gives zero. `build_mnlistdiff` covers every protocol band (`quorumsCLSigs` grouping, the null-base echo rule, legacy construction below 70236).
- **`cbtx`:**
  - Version gates: v2 required from DIP0008 height **+ 1** (quirk 16); V20 `active_after(prev)`; V24 && V20 → v4.
  - Height and `merkleRootMNList`.
  - `merkleRootQuorums`: the rotation-indexed, sliding-window assembly with byte-sorted hashes **over all configured LLMQ types, enabled or not**. Twenty-four stale legacy LLMQ_50_60 commitments are still leaves on mainnet (R4 F5, quirk 18).
  - Best CL: the "at least prev diff + 1" rule and the V19 floor, checked through the injected `ChainLockVerifier`.
  - `creditPoolBalance`; `merkleRootAssetUnlocks`, whose mismatch is MUTATED.
- **`creditpool`:** `CreditPool { locked, current_limit, lately_unlocked, indexes: RangeSet }` and three limit eras: windowed before WITHDRAWALS; 2000 DASH per block after it; the V24 rule using the balance at h−576, per the pinned release. Asset-unlock quorum recency and the height window.
- **`mnhf`:** the signals map, `IsValidMNActivation`, bit-reuse expiry, `EhfSignalView` for versionbits. Any mined 400_85 quorum may sign; the bit in the request ID is an int64 (quirk 31).
- **`specialtx`** (lives in `dash-dml::specialtx` per SB0 rev6.1; EV1 design): every `CheckPro*Tx`, the shared-collateral covenant (template-output creation for every tx including the coinbase; the spend rule; pre-activation templates frozen), asset lock/unlock checks with quorum selection, and **`process_special_txs_in_block`**. It returns the new list, new commitments, the credit pool, signals and the `bls_legacy` flip at V19; non-transactional work (rotation snapshots, mineable-pool erasure, platform-ban resets) goes to its `EffectSink` parameter, so it survives an `Err` (§2.10.1).
- **`llmq::commitment`:** `FinalCommitment` versions 1–4. The version is fixed by the quorum base block; the BLS fields decode under the version-derived scheme (legacy iff nVersion ∈ {1, 2}), while signatures verify and the commitment-hash pubkey bytes serialize under the call's `BlsCtx` (quirk 20, §1.4.1). Also `verify` (with the `check_sigs` split), `verify_null`, and `build_commitment_hash`.
- **`llmq::blockprocessor`:** required commitments per enabled type at `prev`, the mining window, duplicate rules. Null commitments of known-but-disabled types are accepted at any height; `signers >= minSize` rather than the threshold; padding bits are a parse error (quirk 19). The `quorumIndex` rule for non-indexed versions.
- **`llmq::members`:**
  - Score = single SHA256 read as a little-endian `Arith256`; sort by score descending, then **collateral descending**.
  - Modifier rules before and after V20, with the V20 check at `base-8`; the first V20 cycle uses the `base-8` list with the old modifier, and the modifier reads the work block's coinbase CL (quirk 29).
  - Rotated quarters with snapshots: the `MODE_SKIPPING_ENTRIES` first-index quirk, `skipRemovedMNs` from V19 (or on testnet), `break` on a missing snapshot, swallowed `AddMN` exceptions (quirk 28), and no snapshot when the enabled MNs are fewer than a quarter.
  - Snapshots are **effects**, not transactional outputs (rev1, F04): member computation emits `Effect::StoreQuorumSnapshot` as Core's `PreComputeQuorumMembers` does, before any commitment check and in every connect mode, and the chainstate persists it immediately and never erases it (`core: llmq/snapshot.cpp:302-329`; §2.10.1).
- **`llmq::scan` / `selection`:** `ScanQuorums` (window snapping, newest first, rotation per index); `SelectQuorumForSigning`, which uses bits 58–62 of `GetUint64(3)` for rotated types (quirk 30) and raw-byte min for non-rotated ones; `VerifyRecoveredSig`; CL verification against `chain[h-8]`; IS verification with the two-window retry.

**Reuse (R4 G).** Several algorithms R4 verified against Core are lifted from `dashcore::sml` and dash-spv's `sml_engine`, retargeted onto base-sdk types, and given R4's gap fixes:
- verified: SML entry hash and MN/quorum roots (15/15 vectors, 6/6 mainnet diffs), quorum scoring and the modifier, rotation reconstruction, CL/IS request IDs and quorum selection;
- gaps fixed: snapshot building and skip lists, pre-V19/V20 rules, skip mode 2, swallowed exceptions, the collateral tie-break, and quorum roots over configured types.

dash-spv's hard-coded basic scheme is **not** carried over; it fails the 24 stale v1 commitments.

**Store trait** (in-memory for tests; `dashd-chainstate` implements it over `chainstate/`):

```rust
pub trait EvoStore {
    fn best_block(&self) -> Option<BlockHash>;                        // == meta.evo_tip (§3.5)
    fn mn_list_snapshot(&self, at: &BlockHash) -> Option<MnList>;        // every 576 blocks
    fn mn_list_diff(&self, at: &BlockHash) -> Option<MnListDiff>;        // every block
    fn credit_pool(&self, at: &BlockHash) -> Option<CreditPool>;
    fn mnhf_signals(&self, at: &BlockHash) -> Option<Signals>;
    fn mined_commitment(&self, t: LlmqType, q: &QuorumHash) -> Option<(FinalCommitment, BlockHash)>;
    // rev1 (F06): resolved through `at`'s ancestry, as Core's GetMinedCommitmentsUntilBlock(pindex); never "latest"
    fn mined_commitments_until(&self, at: BlockId, t: LlmqType, n: usize, index: Option<i16>) -> Vec<(FinalCommitment, BlockHash)>;
    fn quorum_snapshot(&self, t: LlmqType, cycle_base: &BlockHash) -> Option<QuorumSnapshot>;
}
pub struct EvoDelta { /* MN diff, list snapshot (if any), mined commitments, credit pool, MNHF, BLS flip; transactional only */ }
// Rotation snapshots and Core's other non-transactional effects travel as `Effect`s through an `EffectSink` (§2.10.1).
```

In-memory caches mirror Core's: MN lists (LRU of 64 `Arc<MnList>`, with mini-snapshots every 32 blocks), member lists per `(type, baseHash)`, rotation members per `(cycle hash, quorum index)`, quorum snapshots per cycle base.

### 2.6 `dash-llmq` (pure protocol math)

```rust
pub struct DkgSession { /* members, my index, contributions, complaints, justifications, premature commitments */ }
pub enum DkgInput  { Phase(QuorumPhase), Contribution(QContrib), Complaint(QComplaint), Justification(QJustify),
                     PrematureCommitment(QPCommit), SimError(DkgSimError) }
pub enum DkgOutput { Send(DkgMessage), MarkBad(ProTxHash), FinalCommitment(FinalCommitment), StoreContribution(..), Log(..) }
impl DkgSession { pub fn step(&mut self, input: DkgInput, rng: &mut dyn RngCore, bls: &dyn BlsBackend) -> Vec<DkgOutput>; }
```

- **Contribute.** A polynomial of `threshold` secrets, with `vvec[j] = sk_j·G1`. Shares are evaluated at `id_m = BlsShareId(proTxHash raw bytes)` and sent by IES to each operator key.
- **Complain.** `badConnection` criteria come from the release profile (`LlmqRules.dkg_bad_connection`, §2.25 row 17), all under SPORK_23 PoSe. **v23.1.8:** not connected under spork 21, version < 70240, outbound failed too often, platform-banned. **v24:** those, with version < 70242, plus no tx relay on its MN connections and no `NODE_COMPACT_FILTERS`. v1 hard-coded the v24 set, which would mark every v23.1.8 member of a mainnet quorum bad (M-2).
- **Justify.**
- **Commit.** `validMembers`, `quorumVvec = Σ`, `quorumSig = skShare.sign(commitmentHash)`.
- **Finalize.** Group by `validMembers`; `membersSig = AggregateSecure`; `quorumSig = recover`; the version comes from rotation and V19 at the base. There is a single-member path.
- **Tests.** Whole 3–400-member DKGs run in-process, cross-checked against dash-pkc's LLMQ-100 corpus and the BS12 transcript. `quorum dkgsimerror` maps to `DkgInput::SimError`.
- **Sig-share math.** The recovery member order sorts by `H((proTxHash, id))`; backoff is `min(2^n·2 s, 10 s)`; recovery is Lagrange at `threshold` via Pippenger MSM in G2, with a naive twin.

### 2.7 `dash-governance` and `dash-coinjoin`

**`dash-governance`** (small, exact):
- **Object hash** = `SHA256d(hashParent ‖ i32 revision ‖ i64 time ‖ ser_string(hex(vchData)) ‖ COutPoint ‖ u8 0 ‖ u32 0xffffffff ‖ ser_vec(vchSig))`. It has no `collateralHash`, includes the signature, and serializes the data as a hex string.
- **Signature hash** = `SerializeHash(fields 1..7)`. **Data hash** = `Hash(ser_string(hex(vchData)))`.
- **Vote hash** = `SHA256d(COutPoint ‖ u8 0 ‖ u32 0xffffffff ‖ parent ‖ i32 signal ‖ i32 outcome ‖ i64 time)`; signal comes before outcome, unlike the wire order.
- **Key selection.** Proposal + FUNDING votes use the ECDSA voting key: the `"DarkCoin Signed Message:\n"` form on mainnet, devnet and regtest, and the raw-hash form on testnet. Everything else uses BLS basic `VerifyInsecure`.
- **Proposal validator:** Core's messages and order (512-byte limit, name and URL rules).
- **Trigger formatter** is byte-exact: `{"event_block_height": H, "payment_addresses": "a|b", "payment_amounts": "x.xxxxxxxx|...", "proposal_hashes": "h|h", "type":2}`, including the spacing. `ParsePaymentAmount` keeps its strictness.
- **Superblock limit** = `superblock_subsidy_inner(nBits = 1 on mainnet, powLimit compact elsewhere, h-1, V20) × cycle` (quirk 15).
- **Best superblock:** strict `>` on absYes, then map order by hash. `is_valid` applies the V24 `lastIdx+1` duplicate rule.
- **Tallies:** Evo weight 4; thresholds `max(minQuorum, W/10)` and `2W/3`.
- **Rate-check buffer:** 5 timestamps; `rate >= 2·1.1/cycleSeconds`.
- **Vectors:** `proposals_{valid,invalid}.json`, plus oracle differential fuzzing of the hashes.

**`dash-coinjoin`:**
- **Denominations:** bits `1<<0..1<<4` → 10.0001 / 1.00001 / 0.100001 / 0.0100001 / 0.00100001 DASH.
- **Collateral:** within [10,000, 40,000] duffs.
- **Entry limits:** `COINJOIN_ENTRY_MAX_SIZE` is 9 (10 when rebalancing). At most 180 inputs/outputs; post-V24 this becomes 200 per side, with the ×10 and `% 9` structure rules.
- **`is_valid_in_outs`:** standard N:N, promotion 10:1, demotion 1:10, the final-composition rule, zero fee, unique P2PKH scripts, IS-locked mempool parents.
- **`is_collateral_valid`:** an ATMP test-accept with the 0.1 DASH sane-fee cap.
- **Message hashes:** for `dsq`, the hash includes the signature and the sighash excludes it.
- **Session state machines:**
  - **Server:** IDLE → QUEUE → ACCEPTING_ENTRIES → SIGNING → ERROR.
  - **Client:** scheduling, queue selection (skipping MNs about to be paid), the denomination goal.
- **Rounds** and `IsFullyMixed` with `cj_salt`: memoized, with Core's sentinels and the V24 handling.

### 2.8 `dashd-store`: storage engine

**Decision: RocksDB** (`rust-rocksdb`, pinned C++ build) behind a narrow engine trait, confirmed by the ST1 bake-off before the coins cache lands. Block and undo data stay in Core-identical flat files.

| Criterion | RocksDB | MDBX / LMDB (B+tree, mmap) | redb (pure Rust) | LevelDB (Core's) |
|---|---|---|---|---|
| UTXO flush writes (random 36-byte keys, millions per flush) | LSM: sequential L0 writes, multi-threaded compaction; the same write shape as Core's LevelDB. That does **not** guarantee parity on stalls, write amplification, CPU, RSS or disk; the bake-off and the bench guards decide (GPT F12) | COW B-tree: a random-key flush rewrites most leaf pages; ~1–2 GB written per large flush of a 2 GB tree | slower writes than LMDB | baseline |
| Point read on a cache miss | 2–5 µs (bloom + block cache) | ~1 µs (mmap): **best** | ~LMDB | ~RocksDB |
| Disk footprint (a "never above dashd" metric, per component: NFR-RES-02) | ≈ LevelDB; compression helps the evo CFs | 1.5–2× for random-hash keys (50–70% page fill): **risk to the disk metric** | similar to MDBX | baseline |
| RSS accounting | block cache and memtables are explicit, anonymous and budgeted from `-dbcache` | pages show up as file-backed RSS | explicit | mmap'ed SSTs inflate RssFile |
| Atomic multi-table commit | `WriteBatch` across CFs | one txn | one txn | none across DBs (hence Core's evodb/chainstate checks) |
| Precedent | rusty-kaspa (UTXO), electrs, TiKV | reth, Erigon | — | dashd |
| Build | C++ (~4 min cold; sccache shared) | small C | none | — |

**Bake-off (task ST1, running).**
- **Trace.** The real mainnet UTXO churn from the oracle's `blk`+`rev` files: per-block created coins with bytes and spent outpoints, for ~2.55M blocks.
- **Runs.** RocksDB and MDBX, at `-dbcache` 300 MiB and 4 GiB. A LevelDB arm is optional (Opus X-3): dashd in B1 is already the LevelDB baseline.
- **Measurements:** total time, p99 flush stall, miss-read latency, final size, RssAnon/RssFile, plus ST1 §6a's durability costs (Buffered vs Durable commit, `sync_wal`, flat-file `fdatasync`) and the disk-full behaviour on a loop device.
- **Representative workload (GPT F12).** CH2 adds a second trace before it merges: multi-CF flushes (coins plus evo and index CFs), reorg bursts, concurrent snapshot readers and background compaction, measuring per-component peak and final disk.
- **Decision rule.** RocksDB stays unless MDBX beats it by more than 25% on time without losing on disk or RSS. That threshold only picks between engines. Whichever engine is chosen must still pass every tracked metric against dashd within the charter's no-regression rule. If it does not, it is tuned or replaced; a 25% margin over the other engine is not a pass.
- The result is recorded in `designs/ST1-bakeoff.md`.

**Engine trait.** This is the API ST1 shipped (`dashd-store/src/engine.rs`); v1's sketch is superseded:

```rust
pub enum Commit { Buffered /* visible on return; survives a process crash */, Durable /* durable on return, with every earlier batch */ }
pub trait KvRead {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>>;
    fn iter_prefix<'a>(&'a self, cf: Cf, prefix: &[u8]) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a;
}
pub trait KvEngine: KvRead + Send + Sync {
    type Batch: KvBatch; type Snapshot<'a>: KvRead where Self: 'a;
    fn batch(&self) -> Self::Batch;
    fn write(&self, batch: Self::Batch, commit: Commit) -> Result<()>;   // atomic across the CFs of one DB
    fn sync_wal(&self) -> Result<()>;                                    // durability barrier for all earlier writes
    fn flush_memtables(&self) -> Result<()>;                             // bounds WAL replay; not needed for durability
    fn snapshot(&self) -> Self::Snapshot<'_>;                            // point-in-time reads
    fn approximate_size(&self, cf: Cf) -> Result<u64>;
}
```

- Errors carry `ErrorKind::{NoSpace, Io, Corruption, ReadOnly, Other}`. A failed `write`/`sync_wal` leaves the call's outcome and the durability of unsynced batches unknown, so callers abort (§3.5.6).
- The trait deliberately encodes **no ordering** with anything else. The flush protocol (§3.5) owns ordering.
- The **in-memory engine** (a `BTreeMap` per CF) backs unit tests and the `kv_model` fuzz target. ST3 adds `FaultyEngine`, which models unsynced loss and injects errors (§3.5.8). F's redb test engine is dropped: a second real engine doubles the test surface for no gain.

**RocksDB configuration** (chainstate DB; overridable and reported in `getmemoryinfo`):
- One shared LRU block cache sized from the `-dbcache` split (§3.4), with `cache_index_and_filter_blocks = true` and L0 pinning.
- 10-bit whole-key bloom filters on `coins` and `block_index`; point-lookup tuning on `coins`.
- `level_compaction_dynamic_level_bytes`; `max_background_jobs = max(2, cores/4)`, inside the compute budget (§3.1).
- Compression: none on `coins` and `block_index` (hash keys don't compress), LZ4 on upper levels and ZSTD on the bottommost level of the evo and index CFs.
- **WAL on**, `atomic_flush = true`, `unordered_write = false`, `manual_wal_flush = false`, `paranoid_checks = true`, `wal_recovery_mode = kPointInTimeRecovery`. Durability points are `Commit::Durable` batches (§3.5). `two_write_queues` is not exposed by rust-rocksdb and is not needed.
- **Regtest only:** `disable_auto_compactions = true` and memtables flushed only at explicit points, for live-copy safety (§2.9).
- Periodic compaction is relaxed during IBD.

### 2.9 Datadir layout and on-disk formats (DEC-03, DEC-11)

- **Base directory:** `~/.rdashcore` on Linux, `~/Library/Application Support/RDashCore` on macOS, `%APPDATA%\RDashCore` on Windows. `dash.conf` and `settings.json` live inside.
- **Ports:** the same defaults as dashd (DEC-03): 9999 / 19999 / 19899 and RPC 9998 / 19998 / 19898. A clear error is raised when a port is busy (FR-CFG-04).
- **Per-network subdirectories** are named as in Core: none for mainnet, `testnet3/`, `devnet-<name>/`, `regtest/`.
- **Directory names are constrained** by Core's functional framework, verified in `core: test/functional/test_framework/test_framework.py:638-642, 1015` and `util.py:491-503`:
  - the chain cache keeps only `blocks/`, `chainstate/`, `indexes/`, `evodb/` and `llmq/`, and deletes all other entries;
  - `dynamically_initialize_datadir` copies node 0's datadir, deletes `wallets/` and `llmq/`, and keeps `chainstate/`, `blocks/`, `indexes/` and `evodb/`;
  - `copy_datadir` copies `blocks/`, `chainstate/`, `evodb/` and `llmq/`, ignoring failures.
  - Therefore **all consensus state lives under `blocks/` and `chainstate/`, all LLMQ runtime state under `llmq/`, and indexes under `indexes/`**. Flat files are rebuildable, as in Core. (F's single `db/` directory would be deleted by the cache step.)

```
<datadir>/<net>/
  blocks/blk00000.dat …     Core-identical: magic ‖ u32 size ‖ block (Core's re-serialization); 128 MiB max; 16 MiB prealloc; -tinyblk 64 KiB; -fastprune
  blocks/rev00000.dat …     Core-identical: magic ‖ u32 size ‖ CBlockUndo ‖ SHA256d(prev_hash ‖ undo); 1 MiB prealloc
  chainstate/               RocksDB, the only consensus DB (column families below)
  llmq/                     RocksDB: recsigs (rs_*), IS locks (is_*), DKG (qdkg_*), quorum vvec, quorum sk-share (secret; mode 0600)
  indexes/txindex/ indexes/addressindex/ indexes/spentindex/ indexes/timestampindex/ indexes/coinstats/   one RocksDB each
  indexes/blockfilter/basic/{db/, fltr00000.dat …}    filter records (hash32 ‖ filter) + RocksDB index
  wallets/<name>/wallet.sqlite3   backups/
  peers.dat anchors.dat mempool.dat fee_estimates.dat sporks.dat netfulfilled.dat mncache.dat governance.dat   our versioned formats
  banlist.json settings.json      Core JSON (interfaces, not storage)
  .cookie .lock rdashd.pid debug.log onion_v3_private_key i2p_private_key
  message_capture/<addr>/msgs_{recv,sent}.dat   (-capturemessages; Core format, FR-P2P-18)
```

**Column families in `chainstate/`** (our schema v1; FR-STO-02 versioned; no XOR obfuscation):

| CF | Key | Value | Written |
|---|---|---|---|
| `meta` | ASCII tags: `schema`, `tip` (absent during a transition), `head_blocks` (`[new, old]`, §3.5), `evo_tip`, `reindexing`, `pruned_files`, `last_file`, `import_provenance` (source datadir, Core version, tip), `node_build` | per tag | every flush |
| `coins` | `txid[32] ‖ VARINT(vout)` (Core's key order, without the `'C'` prefix) | Core's `Coin` bytes: `VARINT(h·2+cb) ‖ CompressAmount ‖ ScriptCompression`. **Our choice**: dense, a flush is a memcpy, and it is byte-diffable with a deobfuscated dashd chainstate. It is not a compatibility promise | flush |
| `block_index` | `hash[32]` | `BlockIndexRecord v1`: VARINT height, VARINT status (Core's bits incl. `CONFLICT_CHAINLOCK = 128`, `ASSUMED_VALID` reserved), VARINT nTx, file/data/undo positions if present, the 80-byte header | flush (dirty entries) |
| `block_files` | `u32 BE file` | `BlockFileInfo` (7 VARINTs, Core's semantics) | flush |
| `mn_diff` | `hash[32]` | `MnListDiffRecord` (added MNs in full; updates as Core's field-bitmask state diff; removed internal IDs) | per block (in batch) |
| `mn_snapshot` | `hash[32]` | the MN list in **Core's canonical `EvoSnapshot` MN-list encoding** (sorted by proTxHash; `CanonicalMNListHash`-comparable, R4 F8) | every 576 blocks |
| `credit_pool` | `hash[32]` | `CreditPool` snapshot | every 576 blocks |
| `mnhf` | `hash[32]` | `map<u8 bit, u32 height>` | per block after V20 |
| `llmq_mined` | `u8 type ‖ quorumHash[32]` | `FinalCommitment` bytes ‖ mined block hash | per block (in batch) |
| `llmq_mined_h` | `u8 type ‖ i16 qIdx ‖ BE32(!minedHeight)` | quorum base height ‖ base hash | per block |
| `llmq_snapshot` | `u8 type ‖ cycleBaseHash[32]` | `CQuorumSnapshot` bytes | as an **effect**, at every member computation for a cycle base (incl. failed connects, templates, proposals, VerifyDB and undo; §2.10.1); **never deleted** (Core semantics) |

**Connect/disconnect semantics per CF** (Opus m-3; verified against `core: evo/deterministicmns.cpp:784-817`, `llmq/blockprocessor.cpp:523,556`, `evo/creditpool.cpp`, `evo/mnhftx.cpp`):

| CF / state | On connect | On disconnect | Notes |
|---|---|---|---|
| `coins` | spend inputs, add outputs | undo record restores spent coins, erases created outputs | Core |
| `mn_diff` | write the block's diff | **kept** (read, not erased) | Core keeps diffs for disconnected blocks |
| `mn_snapshot` | every 576 blocks | kept | keyed by block hash |
| `llmq_mined`, `llmq_mined_h` | put | **erased** | the mineable pool gets the commitment back (effect) |
| `llmq_snapshot` | effect (any attempt) | kept; undo re-writes it | §2.10.1 |
| `credit_pool` | derived; snapshot every 576 | derived from the parent | Core derives it |
| `mnhf` | keyed by block hash | kept (keyed by hash) | signals of the active chain are found through ancestry |
| Governance triggers, IS mined index and archive, recsig truncation, DSTX confirmed height | runtime stores follow `BlockConnected` | follow `BlockDisconnected` | specified with their owning tasks (GV1b, LK1b, CJ1), each idempotent under re-delivery (§3.5.7) |

**Formats.**
- **Rules (DEC-11):**
  - `blk`/`rev` files are Core-identical. Sealed files import by hardlink (never a file either side may still write; §2.11, F09), undo records diff against dashd byte by byte, and Core's `-loadblock` and `contrib/` tools work.
  - Everything else is our own versioned format.
  - Core's binary formats are **read** by `dash-coreimport`, and our loaders also accept them on detection. Functional tests that drop Core-format files into a datadir (`feature_addrman`, `mempool_persist`) work that way (FR-P2P-19, FR-MEM-07).
  - JSON configuration files keep Core's format.
- **Flat DBs** use an envelope of `magic ‖ our version string ‖ body ‖ SHA256d`. Core and rdashd therefore never misread each other's `governance.dat`/`mncache.dat`; a mismatch means "rebuild from the network", as in Core.
- **Evo:** our per-block diffs plus snapshots every 576 blocks (Core's `DISK_SNAPSHOT_PERIOD`). F's alternative (Core's evodb keys with `dmn_S3` re-sorted) is rejected under DEC-11. Core's evodb bytes depend on immer HAMT iteration order and are import-only (R2 G.3 Q2, R-6).

**Datadir safety.**
- `.lock` (flock) and `rdashd.pid` are always written.
- **Core datadir detection.** `chainstate/` is ours if it holds RocksDB `OPTIONS-*` files and our `meta.schema`. It is Core's if it holds LevelDB files and a `\0obfuscate_key`.
  - On **regtest**, rdashd detects a Core datadir by default, moves Core's `chainstate/`, `blocks/index/`, `evodb/`, `llmq/` and `indexes/` (which `dynamically_initialize_datadir` copies too, m-5) aside to `<net>/core-import-<ts>/`, keeps the byte-identical `blk`/`rev` files, and rebuilds with `-reindex`. This is how functional-test caches and `copy_datadir` from a dashd node 0 work.
  - On other networks it refuses to start unless `-importcoredatadir` is given (FR-STO-06, US-NODE-19).
- **Live-copy safety for the functional framework.** The framework copies a *running* node's datadir right after `gettxoutsetinfo` (`dynamically_initialize_datadir`).
  - v1 relied on `DisableFileDeletions`. That is not enough (Opus m-6): MANIFEST and CURRENT can reference SSTs created after the copier listed the directory.
  - Rev1, regtest only: every regtest RocksDB runs with `disable_auto_compactions = true` and memtables large enough never to flush on their own. `gettxoutsetinfo`, `stop` and the other copy points the framework uses perform an explicit flush, memtable flush and `sync_wal` before replying.
  - After that, no background job creates or deletes files until the next explicit flush point. A copy taken after the reply sees a stable MANIFEST, and a WAL torn mid-append recovers as a prefix (§3.5.1). Regtest DBs are tiny.
  - A nightly test copies a busy regtest datadir during writes and opens the copy.

### 2.10 `dashd-chainstate`

**Block index.**
- An append-only arena: `Vec<BlockIndexEntry>` in chunks of 64 Ki entries, never moved, addressed by `BlockId(u32)`.
- Hash lookup is a `hashbrown::HashTable<BlockId>` that hashes *through* the arena, so each hash is stored once (≈ 5 B per entry for the table).
- **Immutable fields:** hash; the header fields except `hashPrevBlock` (derived from `parent`); height; parent; skip pointer; `chain_work: Arith256` (F, and BS1's direction — O's u128 is rejected because FR-CHS-01/FR-CON-14 want `arith_uint256` semantics).
- **Mutable fields** (status, `nTx`, `chain_tx`, file positions, sequence ID, time-max) are written only by the validation thread and published per entry under a seqlock, so a reader always sees one coherent version (§2.10.2, F06).
- Readers on other threads need no lock.
- Memory estimates differ: **O** ~147 B per entry vs ~220 B for Core's `CBlockIndex` + map node; **F** ~120 B vs ~150 B. PF1 measures before the design claims a number.
- **Load:** parallel decode of `block_index` records (rayon); PoW is checked against the **stored** hash, as Core's `LoadBlockIndexGuts` does, with no X11 recomputation (m-11); rebuild parent links, `chain_work` and `n_chain_tx` in height order; derive candidates; verify file info and the `reindexing`/`pruned_files` flags.
- `-checkblockindex` (the consistency walk) is on by default on regtest.

**Validation thread** (`ChainstateManager::run`) consumes commands from a bounded channel:

```rust
pub enum ChainCommand {
    ProcessHeaders { headers: Vec<BlockHeader>, from: Option<PeerId>, reply: oneshot::Sender<HeadersResult> },
    ProcessBlock   { block: Arc<PrevalidatedBlock>, force: bool, from: Option<PeerId>, reply: Option<oneshot::Sender<BlockResult>> },
    InvalidateBlock(BlockHash, Reply), ReconsiderBlock(BlockHash, Reply), PreciousBlock(BlockHash, Reply),
    EnforceChainLock(ChainLockSig),            // from dashd-locks: Core EnforceBlock + ActivateBestChain
    TestBlockValidity(Arc<Block>, Reply),       // GBT proposal / CreateNewBlock check (overlay dropped; effects applied, §2.10.1)
    Flush(FlushMode), PruneTo(u32, Reply), LoadMempoolDone, Shutdown,
}
```

Async callers `await` the oneshot replies, so no tokio worker blocks on validation.

**`connect_block` order** is a line-by-line port of Core `ConnectBlock` (R2 A.1 §1; FR-CON-19):
1. `BlsCtx::for_block(h)`: legacy iff h < V19Height (§1.4.1).
2. `check_block` (MUTATED → abort as Core does).
3. Conflicting-CL check via `ChainHelper`.
4. Evo best block == prev when DIP0003 is active. This is free here: one atomic DB and one overlay.
5. Genesis special case.
6. **assumevalid** decision: an ancestor of assumevalid, an ancestor of the best header, best-header work ≥ minimum chain work, and proof-equivalent time > 2 weeks. When it holds, `script_checks = false`, which is passed as `fCheckCbTxMerkleRoots` (`core: validation.cpp:2543-2570, 2664`). That skips, verified rev1 (m-1):
   - `CheckCbTx` **entirely**: its presence, version and height checks (`specialtxman.cpp:817-839`). Because the CbTx is then never parsed, the credit-pool balance **and unlock limit** (`:884-890`), the MN-list and quorum merkle roots and the best-CL check (`:930-971`) are skipped too.
   - ProTx signatures (`check_sigs`).
   - QC BLS signatures (`blockprocessor.cpp:267-283`).

   It **still verifies**:
   - the QC structure, including `IsValid()` of its BLS fields;
   - the **asset-unlock quorum signature** (`assetlocktx.cpp:231`);
   - the **MNHF signature** (`mnhftx.cpp:152`);
   - the eager payload decoding, which can throw.

   It also still builds the DML and member lists (R4 F2, FR-CON-24). `EvoCtx.check_sigs` must not gate the asset-unlock and MNHF signatures (quirk test).
7. BIP30 (always enforced unless the `BIP34Hash` block is an ancestor, so always on devnet and regtest); `nSuperblockStartHash` (`bad-sb-start`).
8. Script flags.
9. Subsidy.
10. `process_special_txs_in_block` **before** the UTXO loop.
11. Per tx: `check_tx_inputs`, the V24 shared-collateral spend check (`active_at`), fees, sequence locks, sigops (adding P2SH), script checks queued to the check queue, `update_coins`.
12. Wait for the check queue.
13. IS conflicts, behind `UngatedRules.is_gate` (§2.25 row 6): v24 checks them only outside IBD, through `instantsend::HasLockInputs`, which covers asset-unlock virtual inputs; v23.1.8 checks them only when `IsBlockchainSynced() && SPORK_3`. Both go through `ChainHelper`. For a ChainLocked block, conflicting IS locks are **removed**, and that removal is an effect that survives a later failure (§2.10.1).
14. `is_block_value_valid` and `is_block_payee_valid`, with `SuperBlockCheck::{NoCheck, AllowDuplicates, DisallowDuplicates}` chosen exactly as Core does (NoCheck when the best CL height ≥ h or MN sync is incomplete). Failures from these two are `BLOCK_RESULT_UNSET`: not punished, still `FAILED_VALID`, and cleared by `reconsiderblock` (FR-CON-32).
15. Write undo.
16. Set `VALID_SCRIPTS` and the best block.

**Leniencies that must not be tightened** (R4 F3, F6; FR-CON-21/22):
- no MN payee check below DIP3 enforcement (mainnet 1047200);
- any coinbase is accepted when every MN is PoSe-banned, and the credit pool is still credited;
- the 12.0 budget windows (`h % 16616 < 100` while `h < 614820`) are unbounded under `NoCheck`; mainnet block 332320 overpays about 1176 DASH;
- superblock payees are never checked during IBD or reindex, only the value bound.

**Overlay atomicity, for transactional state only.** Each block connects into a `BlockOverlay`, a child coins map plus an `EvoDelta`. On success the overlay merges into the active layer; on failure, or in proposal mode, it is dropped. That one mechanism replaces Core's `CCoinsViewCache view(&CoinsTip())` and `CEvoDBScopedCommitter` for the state Core itself rolls back. It does **not** cover Core's non-transactional effects, which §2.10.1 handles. v1's claim that proposal mode is side-effect free was wrong (GPT F04).

#### 2.10.1 The effect channel (rev1; GPT F04, F15; Opus M-6, M-7)

Several Core actions run inline during a connect attempt and persist even when the attempt later fails, runs with `fJustCheck`, is a GBT proposal, or is a VerifyDB pass. The Rust design models them as **effects**, kept separate from the transactional delta:

```rust
// as in rs:dash-evo/src/effect.rs:34 (CH7 erratum, DEC-531)
pub enum Effect {
    StoreQuorumSnapshot { llmq_type: LlmqType, cycle_base: BlockHash, snapshot: QuorumSnapshot },
    RemoveConflictingIsLock { lock_hash: Hash256 },          // ChainLocked block overrides an IS lock
    EraseMineableCommitment { llmq_type: LlmqType, quorum_hash: QuorumHash, commitment_hash: [u8; 32] }, // LQ2 D7, D8
    ReinsertMineableCommitment { commitment: Arc<LlmqCommitment> },
    ResetPlatformBan { pro_tx_hash: ProTxHash, height: i32 }, // ProUpServ revive; SetPlatformBan refuses a stale height
    NotifyMnListChanged { at: BlockHash, undo: bool },       // VerifyDB levels 3 (undo) and 4
    // (DEC-82: no BlsSchemeLeak; v24 commits the BLS scheme flag only after a full connect, §2.25)
}
pub trait EffectSink { fn emit(&mut self, e: Effect); }
```

- **Emission.** Pure functions that can produce effects take `&mut dyn EffectSink` as a parameter: `process_special_txs_in_block`, the member computation, commitment verification, and the IS-conflict step of `connect_block`. An effect is therefore emitted even when the function returns `Err`, because it never travels inside the `Result`.
- **Application.** The chainstate actor applies the log **in emission order and regardless of the result**, before acting on the result. Core performs these effects inline, so their order relative to the failure is Core's. Each effect is persisted exactly as Core persists it, no more durably and no less.
- **No durable tip (DEC-202).** A kill before the first flush makes a tip durable can leave effect rows with no chainstate tip. No durable tip is a logically empty store: recovery keeps those rows, as it keeps effect rows above a durable tip, and the connects from height 1 rewrite them (CH2 §4.3 step 5). Core's rotation snapshot, a raw write, survives such a kill too.
- **`ReplayChainHelper` and recorded decisions** cover failed and proposal attempts as well as successful connects (GPT F15).

The effect inventory below was verified against develop and v23.1.8. Caches whose contents are pure functions of deterministic keys (MN-list, member, versionbits, credit-pool and the static best-CL cache) are deliberately left unmodelled, because none of them is observable.

| Effect | Core site | When it happens | Survives `Err` / `fJustCheck`? | Rust persistence | Observable through |
|---|---|---|---|---|---|
| Rotation snapshot | `PreComputeQuorumMembers` at the start of the quorum `ProcessBlock`, before any commitment check, whatever `fJustCheck` is (`blockprocessor.cpp:223`; v23.1.8 `:176`) → `StoreSnapshotForBlock` → raw DB write outside the EvoDB transaction (`snapshot.cpp:320-328`). Also reached from `CFinalCommitment::Verify` via `CheckLLMQCommitment` and `qfcommit` intake. Undo (`reset_cache=true`, `:523`) re-writes it. develop's RPC-only "predicting" mode skips the write; v23.1.8 always writes | every connect attempt, including every `CreateNewBlock` template (via `TestBlockValidity`), GBT proposals, VerifyDB and `qfcommit` | yes; never erased | an immediate `Commit::Buffered` single-row batch into `llmq_snapshot` (§3.5.7); never erased | contents are deterministic per (type, base hash), so the visible effects are DB growth (one row per distinct template hash at a cycle-base height) and `quorum rotationinfo` / `qrinfo` for known blocks |
| Conflicting IS-lock removal | `connect_block` for a ChainLocked block outside IBD, **after** script checks and **before** the value/payee checks and the `fJustCheck` return (`validation.cpp:2800-2817`); writes the IS DB directly | real and `fJustCheck` attempts | yes | `dashd-locks` deletes from `llmq/` immediately | `getrawtransaction.instantlock`, IS RPCs, mempool conflict handling, ZMQ |
| Mineable-commitment erase | quorum `ProcessCommitment` with `fJustCheck = false` (`blockprocessor.cpp:439-453`) | real connect attempts | yes; not restored when the block later fails | `dashd-quorums` in-memory pool | GBT: the miner emits a **null** commitment for that quorum |
| Mineable-commitment re-insert | undo and VerifyDB level 3 (`blockprocessor.cpp:556`) | disconnects, VerifyDB | n/a | in-memory pool | GBT |
| Platform-ban reset | `CDeterministicMNManager::ProcessBlock` with `fJustCheck = false`, for ProUpServTx revives (`deterministicmns.cpp:741-748`) | real connect attempts | yes; persisted through `mncache.dat` | the `dashd-masternode` meta store | local DKG bad-connection decisions (`active/dkgsession.cpp:285`) |
| MN-list-changed notification | VerifyDB level 4 runs `ConnectBlock(fJustCheck=false)` (`validation.cpp:2916-2919, 5158`) | `-checklevel=4` | n/a | event | listeners (DKG, MN connections) |
| BLS-flag leak | v23.1.8 only: a failed connect or a failed `TestBlockValidity` at the V19 boundary leaves the global flag switched; v24 guards it | v23.1.8 profile only | v23: yes; v24: no | `BlsCtx` state | the decode scheme of later P2P and mempool objects (regtest and devnet only) |

**Tests.** O2b `diff_chain` (§4.4) generates **failure sequences** and compares Core's and Rust's surviving effects after **every** step, rejected steps included:
- connect failures: a ChainLocked block with an IS conflict that then fails its payee check; a block that fails after `ProcessCommitment`;
- proposals: a GBT `proposal` at a cycle-base height; templates built at a cycle base;
- follow-ups after a failure: member queries, GBT, restart, reconnect and reorg.

What is compared: snapshot rows, the IS DB, GBT commitments, mncache, and the verdict, class and string of each step. The rejection and proposal experiments run against **disposable** oracle datadirs, never against the BS2 oracles.

#### 2.10.2 Views and coherence (rev1; GPT F06; Opus m-4)

- **`ChainView`** (published through `ArcSwap`) holds only tip-level facts. Validation (V) computes them together at a block boundary: `tip`, `best_header`, `height`, the active-chain vector, `Arc<MnList>` at the tip, the best CL, `ibd`, `mtp`, `bls: BlsCtx::for_tip`, and a monotonically increasing `epoch`.
- **Block-index entries.**
  - Immutable fields are plain data.
  - The mutable group (status, `nTx`, `chain_tx`, file positions, sequence id) is published per entry under a **seqlock**: the writer bumps an odd sequence number, writes, then bumps it even, and readers retry until they read a stable even value. A reader therefore always sees one coherent version of an entry.
  - Queries that need cross-entry consistency (`getchaintips`, `getblockheader` confirmations, locators) run as `ChainCommand::Query` on V, as Core holds `cs_main` for them.
- **Historical and cross-domain state is never "latest".** Every API that reads mined commitments, rotation snapshots, the credit pool, MNHF signals or MN lists takes an explicit `at: BlockId` and resolves through that block's ancestry, as Core's `GetMinedCommitmentsUntilBlock(pindex)` does (`blockprocessor.cpp:687-723`).
  - Appendix A's `EvoStore::mined_commitments_until` gains `at`.
  - V hands out an `EvoReadView { at, O(1) clones of the active and frozen layers' merged indexes, a DB snapshot }` (CH7 D22). That view stays valid across reorgs because all evo data is keyed by block hash and never mutated in place.
- **Coins.** Point reads (`gettxout`) run on V, as Core takes `cs_main`. `gettxoutsetinfo`, `scantxoutset` and `dumptxoutset` first force a flush, then iterate a DB snapshot, as Core does.
- **`ChainHelper` back ends** (CL, IS, governance, MN sync) answer from immutable snapshots published before the connect starts, and **never call into validation** (m-4; that rule also prevents a deadlock with `ChainCommand` replies). State that Core changes only under `cs_main`, such as CL enforcement, reaches the chainstate as a `ChainCommand`, so it is serialized with connects exactly as Core serializes it.
- **Background work is fenced.** Every job (recsig, isdlock and CL verification, DKG sessions, sig-share recovery, prefetch, §3.4) carries `(epoch, at, bls)`. On completion its owner checks that `at` is still the block it was computed for and that the BLS context is unchanged; otherwise the result is discarded or recomputed. L4 scenarios exercise delayed jobs across connect, failed connect, disconnect and V19.


**Chain selection, ChainLock enforcement and reorgs.**
- **Candidate set** ordered by `(chain_work, sequence_id)` (Core's `CBlockIndexWorkComparator`). `FAILED_MASK` and `CONFLICT_CHAINLOCK` entries are excluded, and both statuses propagate to descendants in `find_most_work_chain`.
- **`activate_best_chain`:**
  - Loop: find the most-work chain; compute the fork point; disconnect to it, collecting disconnected txs into the `DisconnectedBlockTransactions` pool for mempool re-add; connect forward at most 32 blocks per step; flush per Core's `FlushStateToDisk(IF_NEEDED)` policy; publish `UpdatedBlockTip`. Repeat until the tip is the best candidate.
  - `NewPoWValidBlock` pre-announces compact blocks.
  - `DisconnectBlock` order: `undo_special_txs_in_block` runs before the UTXO undo. The MN diff is read but not erased (Core), the credit pool is derived, and the BLS scheme flips back.
- **`enforce_block(height, hash)`:** mark every block at that height on other branches `CONFLICT_CHAINLOCK` (with descendants), disconnect a conflicting active chain, re-add the locked branch to the candidates, then `activate_best_chain`. Header acceptance consults `ChainHelper::has_conflicting_chainlock` and still inserts the header with `CONFLICT_CHAINLOCK` before rejecting (`bad-chainlock`), so `getchaintips` shows `conflicting` tips (FR-CON-18, FR-CHS-05).
- **`invalid_block_found`:** marks `FAILED_VALID` and descendants `FAILED_CHILD`, removes them from the candidates, and runs `check_fork_warning_conditions`.
- **`preciousblock`:** Core's `nSequenceId` trick.
- **Reorg depth:** no artificial limit beyond Core's; ChainLocks are the limit (FR-CHS-07).
- **Pruning (`-prune`):** `MIN_BLOCKS_TO_KEEP` 288, a 945 MiB minimum target, `PRUNE_LOCK_BUFFER` 10, prune locks from indexes, `pruneblockchain`, and `HAVE_DATA`/`HAVE_UNDO` clearing in the index batch. It is incompatible with txindex, addressindex, spentindex and governance (an init error), and soft-sets `-txindex=0`. The file and index layer supports pruning from M3, so no format change is needed later.
- **assumeutxo:** not in scope (NG-6). `ASSUMED_VALID` is reserved in the status codec.
- **VerifyDB** (`-checkblocks`, `-checklevel`, `verifychain`): levels 0–4 as Core.

**State-dependent validity: `ChainHelper`** (Core `CChainstateHelper`; FR-CON-01/32; R4 §1.10). This is O's single trait, chosen over F's four `*Oracle` traits because it means one wiring point and one replay implementation; narrow test fakes come from `NoOpChainHelper` overrides. Signature in Appendix A.
- **IBD/reindex:** `is_mn_sync_finished()` is false, so superblock payees take Core's `NoCheck` path exactly as dashd does.
- **Replay:** `ReplayChainHelper` serves **recorded** dashd decisions per height (§4.6), so offline replay of state-dependent validity is deterministic.

**Events.** `ChainEvent` is delivered in order on one dispatcher thread, with backpressure into validation when more than 10 events are pending (Core's `LimitValidationInterfaceQueue`).
- Variants: `BlockConnected { block, height, undo, mn_list }`, `BlockDisconnected`, `UpdatedBlockTip { new, fork, ibd }`, `NotifyHeaderTip`, `NewPoWValidBlock`, `ChainStateFlushed { locator }`, `MnListChanged { old, new, diff }`, `ChainLockAccepted`, `NotifyTransactionLock`, `NotifyInstantSendDoubleSpend`, `NotifyRecoveredSig`, `NotifyGovernanceObject`, `NotifyGovernanceVote`, plus mempool add and remove (forwarded).
- Subscribers that need exact ordering (mempool, evo caches, the LLMQ phase clock, index queueing, wallet) are called synchronously in dispatcher order; the rest (ZMQ, notify scripts) are queued. The `syncwithvalidationinterfacequeue` RPC waits for dispatcher idleness.

**`ChainView` publication.** `ArcSwap<ChainView>` is updated after each tip change, and at most every 100 ms during IBD. It is a tip-level summary only. Cross-domain and historical reads use the pinned views of §2.10.2:

```rust
pub struct ChainView { pub tip: BlockId, pub best_header: BlockId, pub height: u32,
    pub active: imbl::Vector<BlockId>, pub index: Arc<BlockIndex>, pub mn_list: Arc<MnList>,
    pub best_chainlock: Option<Arc<ChainLockSig>>, pub ibd: bool, pub mtp: i64, pub bls: BlsCtx, pub epoch: u64 }
```

**Crash recovery (FR-CHS-04, FR-STO-03)** follows the durability and recovery protocol of §3.5: Core's file → index → coins/evo order, the `head_blocks` marker with all non-coin state committed atomically in the first sub-batch, a coins-only `ReplayBlocks`, and **ordinary validation for every block above the recovered tip** (§3.5.3 I3). v1's step 3, which replayed `HAVE_DATA` blocks with signatures off, is withdrawn (GPT F01): `HAVE_DATA` only records that block data was accepted and stored, not that the block was ever connected.

**Reindex and import.**
- `-reindex` wipes `chainstate/` and replays `blk*.dat` through `load_external_block_file`: a magic scan, size 80 B to 2 MB, an out-of-order parent map, and the `reindexing` flag.
- `-reindex-chainstate` replays from existing files. `-loadblock=<file>`.
- **Core datadir import** (`-importcoredatadir=<dir> -importmode=safe|fast`; CH6a safe, CH6b fast; rev1, GPT F09/F10):
  - **The source must be stopped and locked, or be a filesystem snapshot.**
    - The importer takes Core's datadir `.lock` with `flock`. If it cannot, it refuses, unless `-importsource=snapshot` asserts that the directory is an LVM, btrfs or XFS-reflink snapshot.
    - Agreement on a tip is **not** an atomic multi-directory snapshot.
  - **Staging and finalisation.** Everything is built in `<datadir>/import-staging/` and moved into place only after a Durable final batch and directory `fsync`s. An interrupted import resumes or restarts from staging and never leaves a half-imported datadir.
  - **Files.**
    - Only **sealed** files are hardlinked: every `blk`/`rev` file except the highest-numbered of each series.
    - The writable tails and every `rev` file the destination might rewrite are copied or reflinked (`cp --reflink=auto`; agentbox's XFS has `reflink=1`).
    - New blocks always go to new destination-only files numbered after the last imported one.
    - `FlatFileSeq` never opens for writing a file whose link count is above 1 (§2.11).
    - The source's files are hashed before and after import, and after a destination mine, reorg and restart, and must be unchanged.
  - **`safe`** (CH6a, the first delivery): import the files as above, translate `blocks/index`, then `-reindex-chainstate`.
  - **`fast`** (CH6b, **provisional** until the evidence below passes) also transcodes `chainstate/` (deobfuscated) and `evodb/`. Optionally `llmq/quorumdb` (which keeps a migrating MN signing in its active quorums), `llmq/isdb`, `sporks.dat`, `peers.dat`, `banlist.json` and `mempool.dat`.
  - **Fast-import evidence (GPT F10).** v1's tip roots do not certify a state transcoder: the SML omits `nLastPaidHeight` and `nConsecutivePayments`, which choose the next payee, and the tip roots do not cover rotation snapshots, MNHF, credit-pool index ranges or mined-height mappings. Fast import therefore **requires** an independently obtained **source report**, taken from the running source before it is stopped. The report holds:
    - `gettxoutsetinfo muhash`, which is mandatory (with no report, fast import refuses; it never just recomputes and logs);
    - `protx list registered true` at the tip;
    - `quorum listextended` and `quorum rotationinfo` for every cycle base still needed by member selection;
    - deployment state, the credit pool and asset-unlock statuses;
    - `getblockstats` for the last 576 blocks.

    The destination must match it on **complete canonical state**: full DML state, including payment heights and PoSe; UTXO MuHash; mined commitments and their height index; rotation snapshots for the cycles member selection reads; MNHF; credit pool. It must also hold the history needed for disconnects of the last 576 blocks (undo and MN diffs).

    Fast import stays provisional until two things pass: corrupt- and missing-record fixtures are rejected, and bounded connect/disconnect/restart differential runs after import equal a `safe` import's digests. Blocks, index, chainstate and evodb must agree on one tip; otherwise it refuses and suggests `safe`.
  - Every import ends with a verification report (FR-IMP-06).

### 2.11 Block files

- `BlockStore` keeps Core's `FlatFileSeq` semantics: file numbering, `nFile`/`nPos` meaning, `BlockFileInfo` and prune behaviour. Positions point after the 8-byte header.
- **Writes happen on the `db-writer` thread.** `accept_block` enqueues the block's **canonical bytes**: the peer's bytes on the fast path, or a fresh Core re-serialization when §1.3 requires it.
  - The position comes back synchronously (pre-reserved offsets), so the **in-memory** index records `HAVE_DATA` immediately while the bytes land asynchronously.
  - No **durable** record references those bytes until the flush has made them durable: `fdatasync` of every touched file, plus a directory `fsync` for created or truncated files. That is invariant I1 of §3.5, which replaces v1's contradictory barrier wording (F03).
  - `HAVE_UNDO` is set only after the undo write returns.
- **Link-count rule (rev1, F09).** `FlatFileSeq` never opens for writing (append, preallocate, finalize/truncate) a file whose `st_nlink > 1`. It first replaces the file with a private copy: copy or reflink, `fsync`, `rename`, then `fsync` the directory. Hardlinked files from an import or an oracle datadir can therefore never be modified through the link, in either direction. The same rule governs oracle datadir construction (PLAN O7).
- **Reads use `pread`** (no mmap, which keeps RssFile honest) behind an LRU of recently connected `Arc<Block>`s, which also serves compact blocks, `getdata` and the wallet.
- `-loadblock`/`-reindex` use Core's scanner semantics: magic-byte scan, `80 ≤ size ≤ MaxBlockSize`, an orphans map.

### 2.12 `dashd-mempool`

- **Entry:** `MempoolEntry { tx: TxRef, fee, vsize = max(size, sigops*20), time, height, sigops, ancestors/descendants (count, size, fees), lock_points, special: Option<SpecialRefs> }`.
- **Indexes:** txid; instance hash (AU v2); `next_tx` (outpoint → spender); ancestor-score (mining); descendant-score (eviction); time (expiry, skipping IS-locked txs).
- **Dash maps:** `pro_tx_refs`, `pro_tx_addresses`, `pro_tx_pubkey_ids`, `pro_tx_bls_pubkey_hashes` (both schemes), `pro_tx_platform_node_ids`, `pro_tx_collaterals`, `asset_unlock_expiry`, `asset_unlock_instances`, `asset_unlock_indexes` and `pending_asset_unlock_amount`. Plus the mempool address and spent indexes when those indexes are on (FR-MEM-06).
- **ATMP is serial-equivalent in arrival order** (rev1, GPT F07, Opus M-5). Core holds `cs_main` and `mempool.cs` across pre-checks, scripts and finalization (`core: validation.cpp:1418-1445`, lock contracts `:751-784`). Rust keeps that **observable** contract and parallelises only work whose result cannot depend on concurrent admissions:
  1. **Stage A (parallel, no locks).** Stateless pre-checks. Then, against a pinned `(chain epoch, mempool generation, lock generation)` view, the coins fetch plus script and special-tx signature verification.
     - Results are cached under `(txid, flags, hash of the spent coins)`. That is Core's script-execution-cache key extended with the coins, because legacy sighash depends on the spent scripts.
  2. **Stage B (a sequencer, strictly in arrival order).** Under the mempool write lock, stage B re-runs **every** inexpensive mutable check exactly as `PreChecks` / `PolicyScriptChecks` / `Finalize` would against the current state:
     - conflicts and IS locks;
     - mempool-parent availability;
     - ancestor/descendant limits with the carve-out;
     - the rolling minimum fee;
     - the six ProTx uniqueness maps and the cross-scheme operator-key check;
     - asset-unlock pending totals and stale-instance rules;
     - special-tx contextual checks at the tip.

     Stage A's signature verdicts are reused only if the spent coins and flags are unchanged; otherwise stage B re-verifies inline.
  3. **Order.** Transaction k enters stage B only after every earlier arrival has committed or been rejected. On a tip change, stage B re-runs everything against the new tip. Because stage B checks in Core's order, reject precedence is Core's; a signature failure is reported only when every earlier check passes.
  4. **Where the parallelism is.** The P2P actor awaits each `tx` (§2.14). Parallelism therefore comes from multi-input script checks on the pool, concurrent RPC submissions (`sendrawtransaction`, `testmempoolaccept`, `submitpackage`) and orphan cascades, not from interleaving peers.
  - **Property test** `mempool_model`: parallel ≡ serial in arrival order. It covers concurrent admissions, parent eviction and removal, fee-floor changes, ProTx collisions and AU refresh/amount races at a fixed tip.
- **Order of checks.** Reject reasons and their order match `MemPoolAccept::PreChecks` (R2 A.1 §5) with the Dash insertions:
  - contextual check at the tip; `qc-not-allowed`; `IsStandardSpecialTx`;
  - ISLOCK conflict (`tx-txlock-conflict`), or eviction of mempool conflicts when a lock is pending;
  - asset-unlock duplicate, stale-instance and refresh rules (`txn-already-known`, `assetunlock-stale-instance`);
  - V24 shared-collateral checks; MNHF txs exempt from the minimum relay fee; `CheckSpecialTxForMempool`;
  - `protx-dup` and the six ProTx duplicate maps, plus the cross-scheme operator-key check.
- **No RBF** (`txn-mempool-conflict`; FR-MEM-03).
- **Limits:** 25 / 101 kvB ancestor/descendant limits with the CPFP carve-out; `-maxmempool` eviction and the rolling minimum fee; `-mempoolexpiry` 336 h.
- **Policy constants** are Dash's (FR-MEM-02), with the release-dependent ones read from the release profile (§2.25 rows 19–20): `MANDATORY_SCRIPT_VERIFY_FLAGS` (`UngatedRules.mandatory_script_flags`; v23.1.8: P2SH only; v24: P2SH, DERSIG, NULLDUMMY, CLTV, CSV); `MIN_STANDARD_TX_SIZE` (`PolicyRules.min_standard_tx_size`; v23.1.8: 83; v24: 65); testnet's standardness default (v24 requires standard txs; no field, since only v24-pinned networks differ, O17 MPIX-03). The rest: minimum relay 1000 duff/kB; dust 3000/kB (546 for P2PKH); `MAX_STANDARD_TX_SIZE` 100,000 (strict `<`); `MAX_P2SH_SIGOPS` 15; `MAX_STANDARD_TX_SIGOPS` 4000; scriptSig ≤ 1650 bytes and push-only; versions 1..3; bare multisig ≤ 3 keys; datacarrier 83; the shared-collateral template is non-standard except as a shared ProRegTx collateral output; `-acceptnonstdtxn` on test networks.
- **Fee estimator:** a port of `CBlockPolicyEstimator` (buckets 1000…1e7 at spacing 1.05, decays .962/.9952/.99931, the 60 h staleness rule), persisted in our format with Core's `fee_estimates.dat` importable.
- **Orphans:** 10 MB, 20 min, 5-minute sweep. **Persistence:** our versioned `mempool.dat`; Core's v1 is readable (FR-MEM-07).
- **Package index for mining.** An ancestor-feerate ordered index is maintained incrementally on insert and remove. Core already has one (`txmempool.h:257-263`, walked by `miner.cpp:522`), so this is parity, not a gain (F11).

### 2.13 `dashd-mining`

`BlockAssembler::create_new_block(script_pubkey, options)` follows `core: node/miner.cpp` (FR-MIN-01/02):
- Reserve 1000 bytes and 100 sigops; clamp to `nBlockMaxSize`; `compute_block_version`.
- After DIP0003, **add mineable quorum commitments first** (fee 0, sigops 0), with null commitments of the right version and index where nothing better is known.
- Then `add_package_txs`, with:
  - the credit-pool diff (subsidy 0) and the EHF signal stage (no duplicate bits);
  - `TestPackageTransactions`: final, `CheckSpecialTx(check_sigs = false)`, V24 shared collateral, and `IsTxSafeForMining` (the 10-minute rule), gated per `UngatedRules.is_gate`: v23.1.8 skips the IS test unless `RejectConflictingBlocks()`, v24 uses `IsInstantSendEnabled()` (§2.25 row 6).
- **Coinbase:** `OP_RETURN` scriptSig after DIP3, `nVersion 3`/`nType 5`; CbTx version (V24 && V20 → 4, V20 → 3, DIP0008 → 2, else 1); `CalcCbTxBestChainlock` (the "at least prev diff + 1" rule and the V19 floor); `creditPoolBalance`; `merkleRootMNList`, `merkleRootQuorums` and `merkleRootAssetUnlocks`.
- **`FillBlockPayments`:** MN outputs are subtracted from `vout[0]`, then superblock outputs are added.
- **GBT fields** exactly as R2 A.1 §4: `coinbase_payload`, `masternode[]`, `superblock[]`, flags, `masternode_payments_started/enforced`, `superblocks_started/enabled`, and `rules` always containing `"csv"`.
- **GBT behaviour:** refusal conditions (no peers, IBD, MN sync incomplete at a superblock height); longpoll; `proposal` mode via `test_block_validity`.
- **Template cache = Core's policy, exactly** (rev1, GPT F08, Opus m-10). Core rebuilds only on a new tip, or when the mempool update counter changed **and** more than 5 s have passed since the last build. A new ChainLock, IS lock, governance change or MN-list change does not invalidate it (`rpc/mining.cpp:843-866`; v23.1.8 `:795-818`, same logic). The node does the same.
  - Per-call fields (`curtime`/`UpdateTime`, refusal checks, longpoll) are recomputed on every call.
  - v1's background 100 ms rebuild is **cut**.
- **DML memo.** The `BuildNewListFromBlock` result is memoized per `(prev hash, ordered txid list of the candidate block)`, i.e. per exact block. It is reused only between `create_new_block`'s CbTx computation and its own `test_block_validity` of the same block (Core runs `ProcessSpecialTxsInBlock` twice there).
  - v1's `(prev, special-tx set)` key was unsound: an ordinary transaction that spends a collateral removes an MN (`specialtxman.cpp:732-757`), and the order of ProTx and QcTx transitions matters.
- **Inputs Core's miner reads** are read the same way:
  - first-seen age for `IsTxSafeForMining` (`chainlock/handler.cpp:257-268`);
  - the local best ChainLock for `bestCLHeightDiff`/`bestCLSignature`;
  - the credit pool and the asset-unlock merkle root;
  - governance triggers for `FillBlockPayments` (`miner.cpp:299-329`);
  - the mineable-commitment pool, including its effect-driven erasures (§2.10.1).

  Cache-hit, cold-build and oracle templates are compared after each same-tip dependency change: CL arrival, IS lock, governance, a mempool change within 5 s, reordered special txs and alternative ordinary collateral spends.
- **Regtest internal miner** for `generatetoaddress`/`generatetodescriptor`/`generateblock` (X11 grinding).

### 2.14 P2P: `dash-p2p-transport` and `dashd-p2p`

**Stance on upstream work** (NFR-CMP-04).

- **#264 (BIP324, open).** Its `Transport` trait, V1/V2 implementations, handshake and Dash short IDs are **lifted into `dash-p2p-transport`** and generalised:
  - **Responder role:** inbound v1 detection by the 16-byte `magic ‖ "version\0…"` prefix.
  - **Short-ID send gating:** short IDs only when `peer_version ≥ 70235` (≥ 70240 for `platformban`); the long encoding before `version` arrives.
  - **Fallback:** a v1 reconnect after an early disconnect.
  - **HKDF salt:** `"bitcoin_v2_shared_secret" ‖ Dash magic` (`core: bip324.cpp:38`). An M2 interop test against dashd proves it. #264's `bip324` crate already takes the network magic and Core's salt prefix (m-21).
  - **Garbage length:** uniform in 0..4095. **Rekey:** at Core's interval. **No decoys:** the version packet is empty.
  - dash-spv keeps its own copy until #264 merges upstream.
- **#902 (dash-spv network rewrite, draft).** The node neither depends on it nor touches `dash-spv/src/network/`. dash-spv's manager is outbound-only and SPV-shaped (`relay=false`, services `HEADERS_COMPRESSED`, no serving, no misbehaviour scoring).

**Runtime shape: one `PeerManager` actor with Core's sequential, wake-driven semantics; parallelism only where it cannot be observed** (§6 D19; rev1: GPT F13, F14; Opus M-5, m-13).

- **Per connection:** a tokio reader task and a writer task.
  - The reader does **transport work only**. That covers V1 header, checksum, command-string and size checks, or V2 decryption. It then queues a `RawMsg { cmd, payload: Bytes, recv_time, wire_size }` on the peer's receive queue in arrival order and wakes the actor (Core's `WakeMessageHandler`, `core: net.cpp:2616,2671`). Transport errors behave as in Core's socket layer: a bad checksum or command drops the message, while bad magic or oversize disconnects.
  - The reader does **not** decode payloads. As an unobservable optimisation it may pre-hash whole payloads that need no parsing: SHA256d of a `tx` payload (its txid on the fast path) and the X11 hash of the 80-byte headers that start `block`/`cmpctblock` payloads. The results go into a side cache the actor consumes later. Per-transaction hashing inside a block needs transaction boundaries, so it happens when the actor decodes, on the `prevalidate` pool. That is a pure function of the bytes: it never fails, never scores and never reorders anything.
  - The writer runs a bounded send queue with backpressure equal to `-maxsendbuffer` (Core's `fPauseSend`).
- **One `PeerManager` actor** owns all `net_processing` state, exactly like Core's single `msghand` thread:
  - the per-peer `Peer`/`CNodeState` union;
  - the object-request tracker (per `ReleaseProfile`: v23's legacy `ObjectDownloadState` or v24's `TxRequestTracker`, below);
  - block-download scheduling, headers sync and compact-block state;
  - address relay, BIP37 filters and `qwatch`.
- **Message loop** = Core's `ThreadMessageHandler` (`core: net.cpp:3676-3721`; v23.1.8 `:3682-3727`, the same logic):

  ```
  loop {
      more_work = false
      for peer in shuffled(peers):                        // a fresh shuffle on every pass
          if peer.disconnect { continue }
          more_work |= process_messages(peer)            // pending getdata first, then ≤ 1 orphan, else ≤ 1 polled message
          if !peer.masternode_connection || mn_send_due { send_messages(peer) }   // one global 100 ms timer, MN connections only
      if mn_send_due { reset the 100 ms timer }
      if !more_work { wait_until(now + 100 ms, woken) }   // interrupted by message arrival and UpdatedBlockTip
  }
  ```

  - There is **no** fixed tick for ordinary peers. The 100 ms throttle applies only to masternode connections: outbound MN connections, and inbound peers whose `version` set `fMasternode` (`net_processing.cpp:4000`).
  - Within `send_messages`, emission follows **Core's order**: ping → addr → getheaders/getheaders2 → block announcements (cmpctblock/headers2/headers/inv) → inv trickle → other invs → getdata.
- **Decoding happens in the actor, after the gates.** `process_messages` applies Core's state and command gates first:
  - a duplicate `version` is ignored before it is parsed;
  - anything except `version` before `version` is ignored;
  - between `version` and `verack`, only `verack`, `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2` and `sendtxrcncl` are processed; anything else is logged "Unsupported message prior to verack" and gets no penalty (`net_processing.cpp:3932-3935, 4174-4177, 4181-4362`).

  Only then does the actor decode, with the **per-command decode policy** below. Decode errors therefore surface in message order, in the actor, with Core's outcome for that command (m-13).
- **Sequential, observable semantics (M-5).** Core runs `ProcessNewBlock`, `ProcessNewBlockHeaders` and ATMP synchronously inside `msghand`, so every later message from any peer sees the result. The actor therefore **awaits** the result of every message whose effects a later message could observe before it takes the next message:
  - `block`, `cmpctblock`/`blocktxn` and `headers`/`headers2`: awaits `ChainCommand` replies, including `activate_best_chain`;
  - `tx`: awaits ATMP and orphan resolution;
  - the overlay messages Core processes synchronously: `spork`, `clsig` (verification and best-CL update), `govobj`/`govobjvote`, `mnauth`, `qfcommit` intake, `dsq`/`dsa`/`dsi`/`dss`, `qgetdata`/`qdata`, `platformban`.

  While the actor waits, other peers wait too, exactly as behind `msghand`. Parallelism lives **inside** a message: script checks and BLS batches run on their pools during `connect_block` and ATMP. Hashing is speculative in the readers.
- **Core's own asynchronous edges are reproduced, and only those** (verified, `core: validationinterface.cpp:193-240`, `instantsend/net_instantsend.cpp:237-297`, `llmq/net_signing.cpp:159-168`, `llmq/net_dkg.cpp:748`, `chainlock/handler.cpp:133-181`):
  - the queued validation-interface callbacks (`UpdatedBlockTip`, `TransactionAddedToMempool`, `BlockConnected`);
  - `isdlock` verification on the `isman` worker;
  - sig-share workers and the recovered-sig thread;
  - per-phase DKG threads;
  - ChainLock enforcement on the `cl-schdlr` scheduler.

  Their externally visible outputs keep Core's cadence (§2.15, "tick-aligned output").
- **Why not O's per-peer state machines?** Core's semantics are globally sequential (request-tracker decisions, download scheduling, relay ordering across peers). Splitting them across tasks introduces orderings dashd never produces, which are fingerprint and behaviour risks.
- **Fallback.** If PF1 shows the actor is the bottleneck, only messages proved independent may be pipelined. They still commit in arrival order with a full policy re-check, and only after the L6 cross-peer ordering probes show no difference.
- **Global helpers** run as small actors: `AddrMan`, `BanMan` and `ConnMan`.

**Per-command decode policy** (GPT F14, Opus m-17). `Unserialize(T)` alone is not the decode domain; each handler's reads are. `dash-p2p-core` (BS4a/BS10) exposes `decode_with_policy(cmd, payload, &ProtoCtx)`, and a table in `dashd-p2p::policy` pins, per command:

| Aspect | Core behaviour (verified) | Rust rule |
|---|---|---|
| Trailing bytes | ignored for P2P messages (nothing checks `vRecv.empty()`); `GetTxPayload` requires full consumption; `qgetdata` above its size is +100 (v24 only); DKG messages have size and structure checks | the table says, per command, whether trailing bytes are ignored or are an error, and the penalty |
| `version` | mandatory: `nVersion`, services, time (negative → 0), the skipped addrMe; then **each** of addrFrom+nonce, subver (`LIMITED_STRING` 256), start height, relay, mnauth challenge and `fMasternode` is read only `if (!vRecv.empty())`; a truncated optional field throws, is caught and logged, and leaves the peer resending (`net_processing.cpp:3931-4015`; v23.1.8 `:3906-3990`) | BS4 makes these fields `Option`al with Core's order; the actor reproduces the catch-and-log outcome |
| Empty-payload messages (`verack`, `getaddr`, `mempool`, `sendheaders`, …) | the payload is ignored | base-sdk rejects any payload (`macros.rs:36-44`); BS4 fixes that |
| `bool` | any non-zero byte is true (`serialize.h:239`) | `CoreBool` (BS4a) |
| Unknown command | logged, ignored, no penalty | `P2pMsg::Unknown` (BS4a) |
| Version-dependent layouts | per the negotiated version (SML entry, mnlistdiff, qrinfo, `dsa` flags ≥ 70241, `MSG_ASSET_UNLOCK` ≥ 70242) | `ProtoCtx { version, bls: BlsCtx, … }`, built by the actor from the peer's negotiated state and the tip at processing time (§1.4.1) |
| Limits | `LIMITED_STRING`, vector caps, `MAX_SIZE`, compact-size canonicality | per type, swept systematically in BS4a (m-17) |

`diff_p2p_<cmd>` targets compare the whole handler-level outcome (accepted, ignored, logged-and-dropped, score, disconnect) rather than codec equality alone. Sequence fuzzing (O13b) adds state changes: pre-version, pre-verack, renegotiation and tip changes across V19.

**Connection manager** (Core defaults, all configurable; FR-P2P-06):
- **Outbound:** `-maxconnections=125`; 8 full-relay outbound; 2 block-relay-only plus up to 2 anchors (`anchors.dat`); 1 feeler every 2 min (Poisson); 8 addnode on a separate semaphore.
- **Extra peers:** on a stale tip (150 s check, 450 s threshold); extra block-relay peers every 5 min.
- **Seeds and connect options:**
  - DNS seeds: the `x1.` service-bit prefix; after 11 s when addrman has fewer than 1000 entries, otherwise after 5 min; 3 at a time.
  - Fixed seeds after 60 s, with BIP155 data equal to Core's `chainparamsseeds`.
  - `-connect`, `-seednode` and `-addnode` semantics.
  - Distinct netgroups. v2 only if both sides advertise bit 12.
- **Inbound accept:** permissions → ban/discourage → `SelectNodeToEvict` (Core's algorithm, plus the 1 s MNAUTH protection on masternodes) → a masternode refuses inbound until synced.
- **Disconnect:** about 100 ms of linger, then `shutdown(SHUT_WR)`. `InactivityCheck` timeouts; `-maxuploadtarget` accounting.
- **Permissions:** `bloomfilter`, `forcerelay`, `relay`, `noban`, `mempool`, `download`, `addr`.
- **Masternode connections:** `set_masternode_quorum_nodes`, `set_quorum_relay_members`, `add_pending_masternode`, `add_pending_probe`. A 1 s cadence (100 ms after a success) and a 60 s retry backoff. They are typed `OUTBOUND_FULL_RELAY` plus MN flags (`masternode_connection`, `masternode_probe`, `masternode_iqr`, `qwatch`, `verified_pro_reg_tx`) and bypass the outbound semaphore. At most 3 verified inbound legs per ProTx, exempt from limits and eviction. `DeterministicOutboundConnection` deduplication.
- **AddrMan:** a port of Core's (1024×64 new buckets, 256×64 tried, the same bucket math and `nKey`-salted hashing, asmap).
- **BanMan:** `banlist.json`; a discouragement rolling bloom of 50,000 entries at 1e-6; 24 h default; dumped every 15 min.
- **NetFulfilled:** Core's names and expiry (1 h mainnet, 5 min elsewhere).

**`ReleaseProfile` and the fingerprint table** (DEC-04, DEC-05; FR-P2P-16). "Indistinguishable" means indistinguishable from the **pinned release that network actually runs**. v1 modelled this only for the handshake (`ProtocolProfile`). Rev1 generalises it to `ReleaseProfile`, which pins every behavioural difference between the pinned releases that is *not* deployment-gated (Opus M-2, §2.25). `fingerprint.rs` is the P2P part of that profile:

| Network | Reference | Protocol | MN min proto | UA (default) |
|---|---|---|---|---|
| every network (DEC-82) | v24.0.x | 70242 | 70242 | per DEC-83 (honest identity; rev1's `/Dash Core:24.0.0/` emulation, with no rc suffix and a `devnet.<name>` comment on devnets, is superseded) |

DEC-82 removed rev1's mainnet row (v23.1.8, 70240, `/Dash Core:23.1.8/`) and the hidden regtest `-releaseprofile=23.1`.

- Profiles move when the pins move (PLAN §6, re-pin procedure). **Deployment-gated consensus rules come from `ChainParams`; every other release difference comes from `ReleaseProfile`.** Both are pinned per network.
- `-honestua` switches to an honest UA. `-uacomment` works as in Core.
- The real identity appears in `-version`, the `debug.log` header and a hidden non-Core RPC (`getimplementationinfo`, FR-RPC-09). F's extra `implementation` field in `getnetworkinfo` is rejected, because it would break DIFF-RPC equality.
- Release notes disclose the emulation (DEC-04).

**Handshake** (FR-P2P-05; live rc.3 capture, R4 F11):
- **`version`:**
  - written with init version 209 framing. The default mainnet/testnet fixture is 137 bytes; the size changes with the UA, `-uacomment` and the devnet name, so 137 is a fixture, not a rule (F14);
  - fields: `nVersion` from the profile; services; unadjusted time; `addr_recv` (zeroed if unroutable, proxied or non-v1); `addr_from` = our services + `[::]:0`; a SipHash nonce; the UA; `start_height` = the active-chain height; `relay = !RejectIncomingTxs`;
  - a **32-byte mnauth challenge on every connection** and the **`fMasternode` byte**.
  - Parsing is lenient (`CoreBool`). Peers below 70221 are disconnected without a reject.
- **Default services:** `0x1C05` (NETWORK, BLOOM, NETWORK_LIMITED, HEADERS_COMPRESSED, P2P_V2); masternodes `0x1C45` (+ COMPACT_FILTERS); pruned variants.
- **Order after the peer's `version`:** `sendaddrv2` (≥ 70223), `sendtxrcncl` only if enabled, `verack`; then `getaddr` on outbound full-relay connections only.
- **Order after `verack`:**
  1. `mnauth` (masternode, not on probes);
  2. `sendheaders2` **xor** `sendheaders` (by the peer's bit 11);
  3. `sendcmpct(0,1)` iff `CanRelay()`;
  4. `senddsq(1)` unless rejecting txs;
  5. `ping`;
  6. `getheaders2`/`getheaders`;
  7. `qwatch` when applicable.
- **Later:** `getsporks` once per address 0.5–2 s later; `govsync` about 8 s later.
- **Never sent:** `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2`; `sendtxrcncl` stays off by default.

The full checklist of 31 behaviours is in §4.10.

**Misbehaviour** (FR-P2P-11) uses Core's additive score model with threshold 100 and `MaybeDiscourageAndDisconnect` exemptions (`noban`, manual, local). The table is in `misbehavior.rs`, copied from Core (R2 A.2 §4.5):

| Points | Offences |
|---|---|
| 100 | invalid block or header; bad filter; unsolicited `mnlistdiff`/`qrinfo`; invalid spork; bad MNAUTH; devnet mismatch |
| 20 | oversized `inv`/`getdata`/`addr`/`headers`; `notfound` above the profile's limit (v23.1.8: > 116; v24: > 50,000); non-continuous headers; repeated `govsync` |
| 10 | unrequested or invalid `clsig`; invalid DSTX; DKG from an unverified peer; invalid header |
| 1 | `getmnlistd`/`getqrinfo` failures |

The table is per `ReleaseProfile` (§2.25). v24 adds scoring for unsolicited getdata-only objects (CLSIG, QFCOMMIT, DKG messages) and +100 for an oversized `qgetdata`; v23.1.8 has neither.

- A bad checksum or bad command drops the message and keeps the connection; bad magic or oversize disconnects.
- Unknown commands are ignored without penalty.
- Blocks-only violations on block-relay-only, feeler or `-blocksonly` connections disconnect: `tx` yes; `clsig`, `spork`, `govobj`, `isdlock` no.

**Serving** (FR-P2P-10):
- **Headers:** from `ChainView` and the arena only.
- **Blocks:** from the block LRU or `pread`. `MSG_FILTERED_BLOCK` applies the peer's bloom filter plus Dash special-tx element matching, then sends `merkleblock`, the matched `tx`s, and then the **`isdlock`s**.
- **`mempool` reply:** tx invs, matching ISDLOCK invs and the best CLSIG inv; it requires `NODE_BLOOM`.
- **Overlay data:**
  - `mnlistdiff`/`qrinfo` come from the `dash-evo` builders over the MN-list provider, including the legacy construction for peers below 70236.
  - `cfilters`/`cfheaders`/`cfcheckpt` come from `dashd-indexes` (limits 1000/2000/1000).
  - Every other overlay `getdata` goes to the `NetHandler`s: spork, govobj and votes, qfcommit, qsigrec, clsig, isdlock, dsq, platformban, DKG messages.
- **Compact blocks:** v1 only, txid short IDs (the instance hash for AU v2), up to 3 high-bandwidth peers, 3 in flight per block, the depth ≤ 5 / ≤ 10 rules.
- **Block download:** a 1024-block window; 16 in flight per peer; stall timeout 2 → 64 s (×0.85 decay); block timeout 150 s·(1 + 0.5·N); headers-sync timeout 15 min + 1 ms per header; 2-minute `getheaders` spacing; an unknown block inv triggers `getheaders` only.
- **Limits:**
  - headers 2000 and headers2 8000 (≥ 70235); a locator above 101 entries disconnects;
  - `inv`/`getdata` ≤ 50,000 (+20 if exceeded); `addr` ≤ 1000 (+20);
  - message ≤ 3 MiB;
  - one `getaddr` per inbound connection, from a 21–27 h cache, capped at 23% and 1000 entries;
  - empty headers below minimum chain work (the reply type follows the **service bit**, not the request type);
  - `MAX_UNCONNECTING_HEADERS` 10.
- **Object requests: two implementations, selected by `ReleaseProfile`** (§2.25 row 9, rev1).
  - **v24:** a generalised `TxRequestTracker` over every inv type: preferred peers; a 2 s non-preferred delay for `MSG_TX` on non-masternodes; 100 in flight before the 2 s overload delay; an announcement cap of ≥ 5000 (100,000 per peer for tx); no non-preferred delay in masternode mode.
  - **v23.1.8:** Core's legacy `ObjectDownloadState` and `g_already_asked_for` (`net_processing.cpp` v23:572, 1117).
  - Both use `GetObjectInterval`: 15 s for QSIGREC, 5 s for CLSIG, 10 s for ISDLOCK, 60 s otherwise.
  - The orphan work set goes to the peer that supplied the parent (v23) or to the orphan's original sender (v24).
- **Trickle:** Poisson with means of 5 s (inbound, shared), 2 s (outbound), 1 s (verified outbound MN), none MN↔MN or noban. Non-tx invs go out on every `send_messages` pass, including to block-relay-only peers. The cap is `140·MaxBlockSize/1e6 + (q/1000)·5` ≤ 1000 (`INVENTORY_BROADCAST_PER_SECOND` 7).
- **Addr relay:** a 0.1/s token bucket with 1000 burst (+1000 on getaddr); broadcast Poisson ~30 s; local address every 24 h; ADDR_FETCH disconnects after an `addr` with more than one entry.
- **Ping and timeouts:** ping every 2 min; `-peertimeout` 60 s; 20 min inactivity; `CHAIN_SYNC_TIMEOUT` 20 min; `MINIMUM_CONNECT_TIME` 30 s.

**`NetHandler`** (mirrors `core: init.cpp:2208-2281`):

```rust
pub trait NetHandler: Send + Sync + 'static {
    fn commands(&self) -> &'static [Command];
    fn inv_types(&self) -> &'static [InvType];
    fn process_message(&self, peer: &PeerCtx, msg: &P2pMsg) -> MsgOutcome;     // never blocks; heavy work → own worker
    fn already_have(&self, inv: &Inv) -> bool;
    fn process_getdata(&self, peer: &PeerCtx, inv: &Inv) -> Option<P2pMsg>;
    fn on_connected(&self, _peer: &PeerCtx) {}
    fn on_disconnected(&self, _peer: PeerId) {}
    fn on_send_tick(&self, _peer: &PeerCtx, _out: &mut Outbox) {}              // per-peer pushes (sig shares, govsync)
}
pub struct MsgOutcome { pub misbehavior: Option<(u32, &'static str)>, pub relay: Vec<Inv>,
                        pub relay_filtered: Vec<(Inv, Txid)>, pub erase_requests: Vec<Inv>, pub push: Vec<(PeerId, P2pMsg)>, pub disconnect: bool }
```

Handlers are registered in Core's order: the core `net_processing` handler, the CoinJoin wallet manager (when the wallet is on), MNAUTH, the quorum block processor, platform ban, CLSIG, `NetInstantSend`, `NetSigning`, `NetQuorum`, `NetDKG`, `CoinJoinServer` (MN), `NetGovernance`, `SyncManager`. Messages unknown to every handler are logged "Unknown command" and ignored.

**Masternode sync** (`dashd-masternode`, on every node; FR-P2P-13, FR-EVO-11):
- Assets go BLOCKCHAIN → GOVERNANCE → FINISHED on a 6 s tick.
- `getsporks` is sent once per peer.
- `mempool` is sent to outbound non-block-relay peers after reaching the tip (6 s with more than 3 peers, otherwise 30 s).
- `govsync(0, empty bloom)` then per-object vote syncs: 3 peers per hash on real networks, one object per tick on mainnet.
- Peers are disconnected after `full-sync`. A stuck sync restarts after 1 h, except on masternodes. The `mnsync` RPC drives it.

### 2.15 Overlay runtimes

| Crate | State and threads | Core behaviour preserved | Improvements (node-local only; visible outputs on Core's ticks) |
|---|---|---|---|
| `dashd-masternode::sporks` (v1: `dashd-sporks`) | Spork store; reacts to `getsporks`/`spork`; `sporkupdate` signs with `-sporkkey` | Spork IDs 10001 (SPORK_2), 10016 (17), 10018 (19), 10020 (21), 10022 (23); 10023 deprecated. Default `4070908800` (OFF). **Mainnet ignores the table**: all 0 except SPORK_21 = 1. `is_active(id) = value < adjusted_now`; `EvalSpork` for 21/23 (0 = all types, 1 = all but 100_67/400_60/400_85). **Per `ReleaseProfile`** (§2.25 rows 7–8): v23.1.8 also has SPORK_3 (10002, IS block filtering) and SPORK_9 (10008, superblocks), with multi-signer sporks (`-minsporkkeys`) and an always-answered `getsporks`; v24 drops 3 and 9, uses a single key, stops caching true values, and ignores a repeated `getsporks` while the active set is unchanged. Message `{i32 id, i64 value, i64 time, vchSig ≤ 65}`, with a `GetHash` covering every field including the signature. Testnet signs a compact sig over `SHA256d(id ‖ value ‖ time)`; other networks signmessage the decimal string with the DarkCoin magic. `time ≤ now + 2 h`; +100 if invalid; a newer message replaces and relays | none needed |
| `dashd-masternode` | `ActiveMasternode` (WAITING_FOR_PROTX, POSE_BANNED, REMOVED, OPERATOR_KEY_CHANGED, PROTX_IP_CHANGED, READY, ERROR); MNAUTH; MnSync; mncache metadata; netfulfilled; platformban; MN connection maintenance (1 min) | MNAUTH: challenge in `version`; after verack an MN sends `mnauth` signed with the basic scheme over `SerializeHash((operator_pk, peer_challenge, is_inbound, our_version))`; Core's receiver rules and penalties. Probes disconnect after MNAUTH; unverified MN connections are dropped after 5 s; `qsendrecsigs` to relay members. Local address discovery (`GetLocal` toward 8.8.8.8, regtest/devnet fallbacks); MN lookup by operator key; primary netInfo match; a self-connect test on routable networks; reset conditions each tip. Platformban: Evo target, `tip-576 ≤ h ≤ tip`, Platform quorum sig over `H("PlatformPoSeBan" ‖ protx ‖ i32 h)`, reset on ProUpServ. **Masternode mode** (FR-AMN-01; rev1, Opus m-7). With `-masternodeblsprivkey` set, Core **soft-sets** only `-disablewallet=1`, `-peerblockfilters=1` and `-blockfilterindex=basic`; everything else is an **InitError**, and the set depends on the release (`InitRules.mn_init`, §2.25 row 21). **Both releases:** `-listen=0` (when a routable IP is required), txindex off, peerbloomfilters off, `prune > 0`, `maxconnections` below the default, `-disablegovernance`. **v24 adds:** blocksonly, `peerblockfilters=0`, `maxuploadtarget ≠ 0`, a stricter-than-default relay policy outside regtest, and, for non-MNs, governance without txindex outside regtest. The `feature_masternode_params*` tests observe the difference. The operator key is never logged and is zeroized (FR-AMN-05) | none |
| `dashd-quorums` | Quorum manager (Arc caches; pubkey shares warmed on build); `llmq/` DB: `q_Qqvvec`/`q_Qsk`-equivalent keyed by `SHA256d(type ‖ quorumHash ‖ member protxs…)`, cleanup every 576 at `h % 576 == 58`, keeping `keep_old_keys`. Signing manager: recsig DB (`rs_r/rs_t/rs_h/rs_s/rs_v/rs_vt` plus the Platform `rs_p/rs_m/rs_u` equivalents), `-maxrecsigsage` 604800, the conflict rule (a different msgHash for the same (type, id) on non-Platform types), vote recording, `allowDiffMsgHashSigning` for Platform/EHF, `AsyncSignIfMember` (quorum at `tip-8` or explicit). Sig-shares manager: announcements/invs/`qgetsigs`/`qbsigs`/`qsigshare` (spork 21 only), batch verify caps 1000/10000. DKG manager: one async task per (type, quorumIndex) instead of Core's thread per handler. Data recovery (`qgetdata`/`qdata`, masks, 300 + 60 s expiry, 64/4096 caps, `-llmq-data-recovery`, `-llmq-qvvec-sync`). EHF signer | **DKG phase clock:** `stage = (h - idx) % interval`, `phase = stage / phase_blocks + 1`, phases Init..Idle. Per-phase sleep `my_idx · ((phase_blocks-1) · spacing · 1000 / size) · factor` (0.05; 0.1 for Commit; none on regtest). Intake rules and penalties; per-protx queue quota `size·2`; batched signature verification (`AggregateInsecure` + `VerifyInsecureAggregated`, falling back on duplicate sign hashes). `dkgdb` keys `qdkg_V/S/E` (vvec always basic encoding), cleanup beyond `max_store_depth`. `quorum dkgstatus` state; mineable-commitment pool; `qfcommit` intake with the ≥ 70239 stale-repropagation ban. Quorum connections: `EnsureQuorumConnections` for the newest `keep_old_connections` quorums | **Node-local CPU only; externally visible outputs keep Core's ticks** (rev1, Opus M-6). Pippenger MSM for vvec evaluation and recovery. Batch verification on the BLS pool (`-parbls` default `max(2, cores/2)`; Core caps at 33 and defaults to `clamp(hw/2,1,4)` workers). Work may run on arrival, but every externally visible output is released on Core's worker-tick boundaries through `NodeClock`, so relay timing is indistinguishable (NFR-FP-02/03): the recovered-sig relay (verify loop: 100 ms idle sleep), sig-share sends (send/cleanup loop: 100 ms; share-verification dispatcher: 10 ms, batches of ≤ 32, ≤ 4 in flight; `core: llmq/net_signing.cpp:283-388`), `qfcommit` relay and DKG messages (per-phase threads and sleeps). Self-recovered sigs are verified on every 100th recovery, as Core (`signing_shares.cpp:734-744`). `-llmqfastsend` is benchmark-only: refused on mainnet and testnet |
| `dashd-locks` | `ChainLocks`: best CL in `ArcSwap`; coinbase-CL ingestion at startup. CL signer. `InstantSendManager` + IS tables in `llmq/` (`is_v=1`, `is_i`, `is_tx`, `is_in`, `is_m`, `is_a1/a2` equivalents) + IS signer. CL/IS parts of `ChainHelper` | **CL:** `clsig` processing (seen cache; height ≤ best ignored; +10 if invalid or unrequested); `UpdateBestChainlock`; enforcement through `ChainCommand::EnforceChainLock`; `HasChainLock`/`HasConflictingChainLock`; `-chainlocknotify`. Signer conditions: synced, spork 19 value 0, tip unsigned, best CL below tip, `TX_CONFIRM_THRESHOLD = 5` blocks of IS-locked or 10-minute-old txs when IS is enabled; request ID `H("clsig" ‖ i32 h)`. **IS:** `isdlock` verification (cycle hash at a cycle boundary, the `nSignHeight` rule, offset 0 then `dkgInterval` retry, the non-rotating regtest override); lock inputs including the `plwdtx` synthetic outpoint for v2 asset unlocks; conflict resolution (`ResolveBlockConflicts`, ChainLocked-block override, recsig truncation); mempool `tx-txlock-conflict` hooks; archive pruning at 100 blocks. Signer: `CheckCanLock`, per-input `inlock` IDs, `islock` ID over the inputs vector, retroactive signing on `BlockConnected`, the `WAIT_FOR_ISLOCK_TIMEOUT` 10 min interplay with mining. Gates: spork 2, not reindexing or importing (FR-IS-06) | **None that are visible** (rev1, F11/M-6). Core already schedules `EnforceBestChainLock` with a 0 s delay when the block is known (`chainlock/handler.cpp:168-176`; v23.1.8 `:141-146`); the 5 s timer is a periodic `CheckActiveState` + enforce + cleanup, reproduced. `isdlock`s are verified on the `isman` worker cadence (100 ms idle sleep, batches of ≤ 32, `instantsend/net_instantsend.cpp:37, 424-443`) and relayed only after batch verification, as Core. The node-local verification can be faster, but the relay is released on the worker tick. v24's processing of coinbase-embedded CLs (§2.25 row 16) is per profile |
| `dashd-governance` | Objects, votes and orphans (`CacheMultiMap` 1,000,000 / 10 min), postponed objects, per-MN rate buffers, `last_mn_list_for_voting_keys`, our `governance.dat` (Core's importable), the trigger store, `govsync` handler, governance signer | **`process_object`:** known/postponed/erased check → rate check → `is_valid_locally(check_collateral)` → postponed on 1–5 confirmations → add; otherwise misbehaviour 20. **Collateral check** through txindex: P2PKH or unspendable outputs, `OP_RETURN <hash>` ≥ 1 DASH, 6 confirmations. **`process_vote`:** known → MN missing (20) → signal checks → obsolete → equal-time lower-outcome loses → 1 h rate limit → signature (20) → store/meta/ZMQ. Vote invalidation on key changes and MN removal; `check_and_remove` with the 10-minute deletion delay; erased-map rules (proposals forever, triggers `creation + 2·cycle + 10 min`). **`govsync` semantics:** full list vs object fetch vs vote sync with bloom; `ssc(10\|11, n)`; netfulfilled `govsync`/`govsync-votes-<hash>` with +20 on repeat; +100 on a bad bloom size; +100 on a malformed vote length; relay batching every 5 s (1 s mockable); orphan parent requests every 5 min. **Signer (MN):** `create_superblock_candidate` (approved proposals sorted absYes desc / hash desc; `now + (nextSB-h)·2.62·60` time estimate; greedy budget fill with the ±2 h fudge; payments sorted by hash desc); `create_governance_trigger` (dedup by data hash, only if we are `GetProjectedMNPayees(tip).front()`); `vote_governance_triggers` (FUNDING-YES on ours, FUNDING-NO on other future triggers). `-disablegovernance` (forced by `-prune`, forbidden on MNs) and the `-txindex` requirement outside regtest are enforced at init | Vote signature verification batched on the BLS pool |
| `dashd-coinjoin` | Server session state machine (from `dash-coinjoin`), DSTX manager (24-block expiry, CL purge), DSQ relay modes (INV ≥ 70234, else ALL), `mncache` DSQ counters (shared with `dashd-masternode`) | **`dsa` acceptance:** `ERR_QUEUE_FULL`, `ERR_MN_LIST`, `ERR_RECENT` via `IsMixingThresholdExceeded = last_dsq + enabledMNs/5 > nDsqCount`. **Rebalance gating:** peer ≥ 70241 and V24 at the tip; no legacy participants. `IsSessionReady` (sides covered, and collaterals ≥ max, or a 30 s timeout with ≥ min). `dsi` entry rules. `CreateFinalTransaction` (BIP69 sort, `dsf` to each entry). `dss` signature application with `VerifyScript(P2SH \| STRICTENC)`. `CommitFinalTransaction` (`PrioritiseTransaction(+0.1 DASH)`, ATMP, DSTX relay, `dsc MSG_SUCCESS`, `ChargeRandomFees` ≈ 10%). Timeouts 30 s / 15 s (signing) with `ChargeFees` ≈ 34%. **`dsq` validation:** +10/+100 rules, ±30 s. **DSTX relay policy (every node):** structure incl. the one-block PREMATURE tolerance around V24, MN resolution up to 24 blocks back, `IsValidForMixingTxes` ≤ 5, BLS signature, priority delta, `CanAnnounceDstxTo` fallback to `MSG_TX` for peers below 70241 when unbalanced | none needed |

### 2.16 `dashd-indexes`

- **`BaseIndex`.** Each index is its own RocksDB with a best-block locator and a sync thread that catches up from block files and then follows `ChainEvent`s (async, as in Core). Indexes can be toggled without a reindex (FR-IDX-06). They rewind on reorg, set prune locks, and report through `getindexinfo`.
- **txindex** (default **on**, as in Dash; FR-IDX-01):
  - Key: `txid[0..8]`. Value: a list of packed `(u32 file, u32 block_pos, u32 tx_offset)`. Lookups read the tx and compare the full txid; collisions (about 1e-4 for any collision over 6e7 txids) are handled by the list.
  - About 1.5–2.5× smaller than Core's `'t' ‖ txid[32] → CDiskTxPos` (a 33-byte key plus 3 VARINTs, against our 8 + 12 bytes; B1 measures it), with identical RPC behaviour (O; F's Core-layout keys are rejected under DEC-11 because of the disk metric; m-14).
- **addressindex, spentindex, timestampindex:** Core key layouts (big-endian height and position), so iteration order and RPC result order match (FR-IDX-02). Prefix compression shrinks the address-prefixed keys.
- **blockfilterindex:**
  - Dash-extended BIP158 elements (`evo/specialtx_filter.cpp`), spent prevouts, SipHash keys from the X11 block hash.
  - Filter records (`hash ‖ filter`) in `fltr*.dat` files plus a RocksDB index, with `'V' = 2` versioning semantics.
  - Served over BIP157 when `-peerblockfilters` is set (automatically on masternodes).
  - The builder in `rdc: dash/src/bip158.rs` lacks the special-tx elements and X11 keys. The node's builder lives in `dashd-indexes::filter` on base-sdk types, vector-tested against Core's `blockfilters.json` and oracle filters for blocks with ProTx and asset locks.
- **coinstatsindex:** MuHash3072 per height, so `gettxoutsetinfo muhash <h>` is **the** UTXO-equality oracle (§4.6). `hash_serialized_2` is also implemented with Core's quirk, for RPC parity; `unclaimed_rewards` can go negative because of asset unlocks (quirk 33).
- **Legacy layouts:** Core's in-`blocks/index` `'a'/'u'/'p'/'s'` rows and `'F'` flags are handled by `dash-coreimport`.

### 2.17 RPC, REST and ZMQ

**`dash-rpc-spec`** is the single source for:
- method names, including Dash two-word sub-commands (`"protx register"`);
- argument specs (type, optional/default, `a|b` aliases, the `skip_type_check` string-or-number quirks);
- categories, hidden flags and wallet gating;
- **Core's help text verbatim** (`rpc_help.py`-style tests pass);
- the dash-cli conversion table (which positional and named params are JSON-parsed);
- whitelist names (`method + params[0]` without a space).

It is generated by a script from Core's `RPCHelpMan` declarations at the pinned commits and checked against `help <cmd>` output from both pinned dashd releases. The PRD's 289 commands are classified 266 MUST / 22 SHOULD / 1 WON'T (§5).

**`dashd-rpc`:**
- **HTTP:**
  - hyper 1.x; POST only (405 otherwise); handlers at `/` and `/wallet/<urlencoded>`; header limit 8192;
  - `-rpcthreads` (4) concurrent handlers and `-rpcworkqueue` (16) depth (overflow → HTTP 503 "Work queue depth exceeded"), plus a separate `-rpcexternaluser`/`-rpcexternalworkqueue` queue;
  - `-rpcservertimeout` 30 s; HTTP 503 during warmup, with `-28` in the body.
- **Auth:**
  - cookie `__cookie__:<64 hex>` at mode 0600, written via `.tmp` + rename and deleted at shutdown;
  - `-rpcauth=user:salt$hmac-sha256`; `-rpcuser`/`-rpcpassword`;
  - a failure gets 401 after a 250 ms sleep, with `WWW-Authenticate`;
  - `-rpcbind` is honoured only with `-rpcallowip`; 127.0.0.0/8 and ::1 are always allowed (FR-RPC-02/03);
  - `-rpcwhitelist`/`-rpcwhitelistdefault` with composite names (FR-RPC-04).
- **Dispatch:**
  - JSON-RPC 1.x replies `{result, error, id}`; **no 2.0** (FR-RPC-10); batches;
  - sub-command squashing (if `params[0]` is a string and `"method " + params[0]` is registered, shift the params);
  - named args with aliases and the `args` key;
  - error codes from `rpc/protocol.h`; HTTP status mapping (-32600 → 400, -32601 → 404, else 500);
  - `-deprecatedrpc` (`service`, `legacy_mn`, `softforks`, `banscore`, `walletwarningfield`, `permissive_bool`). Platform's reading of `softforks["mn_rr"]` reproduces the pinned release exactly (PRD §5.3).
- **JSON output:**
  - `serde_json` with `preserve_order` and `arbitrary_precision`;
  - **amounts as fixed 8-decimal numbers** (`ValueFromAmount`);
  - field order follows Core's `*ToJSON` construction order (handlers build ordered maps directly from node types);
  - hashes in Core display order.
  - `rpc-json` is *not* the server's response model (F's choice is rejected): it depends on `dashcore` (R2) and would put the bridge on every response. It remains the typed model for clients, Platform and DIFF-RPC parsing.
- **Handlers** are organised as Core's files: `blockchain`, `mempool`, `mining`, `net`, `node`, `rawtransaction`, `output_script`, `fees`, `signmessage`, `txoutproof`, `masternode`, `coinjoin`, `governance`, `evo`, `quorums`, `zmq`, `hidden`. Wallet handlers live in `dashd-wallet` and register only when a wallet is enabled, with the "wallet-aware override" trick for `protx list`/`protx info`/`getcoinjoininfo`.
  - **Test and hidden RPCs** the functional framework needs: `setmocktime`, `mockscheduler`, `syncwithvalidationinterfacequeue`, `generate*`, `invalidateblock`/`reconsiderblock`, `waitfor*`, `addconnection`, `addpeeraddress`, `sendmsgtopeer`, `setmnthreadactive`, `mnauth`, `echo*`, `quorum dkgsimerror`, `evodb verify`/`repair`, `dumptxoutset`, `getrawaddrman`, `submitpackage`.
  - `-rpcdoccheck` checks the spec against handler output shapes in debug builds.
- **REST** (`-rest`, off by default): every endpoint (`tx`, `block`, `block/notxdetails`, `blockfilter`, `blockfilterheaders`, `headers`, `chaininfo`, `deploymentinfo`, `mempool/{info,contents}`, `getutxos` with BIP64 binary, `blockhashbyheight`) in bin/hex/json, with Core's limits, deprecated forms, and 503 during warmup (FR-REST-01).

**ZMQ** (`dashd-rpc` feature `zmq`, folded in from v1's `dashd-zmq` by X-5; libzmq through the `zmq` crate for exact HWM and reconnect semantics): all 19 topics, 3-part messages `[topic][body][u32 LE sequence]` with per-notifier sequences and HWM, hashes byte-reversed, and the same publication order as dashd for the same event sequence (FR-ZMQ-01/02); `getzmqnotifications`. Platform's frozen topics are `hashblock`, `rawblock`, `rawtx`, `rawtxlocksig`, `rawchainlock`, `rawchainlocksig` and `rawtxlock` (PRD §5.3).

### 2.18 Config, init, clock and logging

**`dash-args`** is a faithful port of `ArgsManager` (FR-CFG-01/02):
- sections `[main] [test] [devnet] [regtest]`; NETWORK_ONLY options ignored with a warning outside main;
- `includeconf` only from the config file and non-recursive; `-conf` only on the command line;
- `-noX` negation and the `DISALLOW_NEGATION`/`DISALLOW_ELISION` flags;
- `settings.json` read and write (`-nosettings`, `-allowignoredconf`);
- exactly one chain selector (`-regtest`/`-testnet`/`-devnet=<name>`/`-chain=`);
- `-help`/`-help-debug` with Core's categories.

**Every Core option name is recognized** (R2 A.6 §6.2, about 250 options). The option table is per release (`dash_args::options(Release)`, generated from each pinned tag; O17): `-minsporkkeys` and `-rescan` exist only in v23.1, `-allowignoredconf`, `-acceptstalefeeestimates` and `-dustprotectionthreshold` only in v24, and init behaviour that differs is `InitRules` (O17-release-profile.md §4.2). Implemented options behave as in Core. Options not yet implemented fail loudly at startup ("not supported by rdashd yet"); none is ever silently ignored. The option set the functional framework passes (`-logtimemicros`, `-debug`, `-debugexclude=libevent|leveldb|rand`, `-uacomment`, `-par=2`, `-maxsigcachesize=1`, `-parbls=2`, `-mocktime`, `-tinyblk`, `-unsafesqlitesync`, `-rpcdoccheck`, `-peertimeout`, `-v2transport`, `-bind=…=onion`) is part of the M2–M5 gates. Sensitive values are redacted from logs (the R1 issue #1122 class; NFR-SEC-04).

**`dashd-node` init** follows Core's `AppInitMain` order:
1. params and overrides;
2. datadir, `.lock` and pid;
3. logging;
4. parameter interactions: the masternode soft-sets and InitErrors per `InitRules.mn_init` (§2.15, §2.25 row 21);
5. open stores and migrations (FR-STO-02: migrate the previous two schema versions in place; refuse newer ones without writing);
6. load the block index, replay, and `VerifyDB` (`-checkblocks=6`, `-checklevel=3`);
7. indexes;
8. mempool load;
9. sporks, mncache, netfulfilled, governance;
10. LLMQ/IS/CL managers;
11. the active masternode;
12. wallets;
13. P2P listen and connect;
14. RPC leaves warmup.

Shutdown is the reverse, with a final flush and the flat-file dumps. **Shutdown leaves stderr empty** (a functional-test requirement).

**Clock, scheduler and logging:**
- **`NodeClock`:** all wall-clock reads go through it. `-mocktime` and `setmocktime` freeze it; `mockscheduler(delta)` advances the tokio `Scheduler`; adjusted time follows `-maxtimeadjustment`. Poisson timers draw from a per-node RNG.
- **Logging:** `tracing` with a Core-format `debug.log` writer.
  - Line format: ISO-8601 timestamps, `-logtimemicros`, `-logthreadnames`, `-logsourcelocations`, `-loglevel`, `-logips`, `-shrinkdebugfile`.
  - Categories use Core's names (`net`, `mempool`, `validation`, `chainlocks`, `instantsend`, `llmq`, `llmq-dkg`, `llmq-sigs`, `mnpayments`, `mnsync`, `coinjoin`, `spork`, `gobject`, `netconn`, `creditpool`, `ehf`, `leveldb` → `db`, …).
  - **Log lines asserted by functional tests are reproduced verbatim:** a script extracts every `assert_debug_log` fragment (63 tests) into `dashd-node/tests/asserted_log_lines.txt`, and each fragment maps to its emitting call site.
  - Operator-relevant lines (UpdateTip, ChainLock, …) follow FR-CFG-07.
- **Notifications:** `-blocknotify`, `-chainlocknotify`, `-instantsendnotify`, `-walletnotify`, `-alertnotify`, `-startupnotify`, `-shutdownnotify` (a detached `sh -c`). statsd uses Core's metric names (P2 priority).
- **Divergence tripwire** (US-NODE-22, NFR-SEC-08): if the node's active chain conflicts with a ChainLock it verified, the node raises an alert (log, `-alertnotify`, `getblockchaininfo.warnings`) and stops signing as a masternode. Digest comparison against dashd is the external O15 comparator's job: v1's in-node `-shadowcompare` RPC client is cut (Opus X-6). The node's `-statedigestlog` output is what O15 and operators compare.

### 2.19 Binaries

- **`rdashd`:** `-daemon`/`-daemonwait` (Unix double fork), signals (SIGTERM/SIGINT shut down; SIGHUP reopens the log), `-version`. The same binary serves functional tests (`DASHD=…/rdashd`).
- **`rdash-cli`:** argument parsing identical to dash-cli, driven by the spec's conversion table.
  - `-getinfo` batches `getnetworkinfo`, `getblockchaininfo`, `getwalletinfo` and `getbalances`, and adds `mn_in`/`mn_out`/`mn_total` and `coinjoin_balance`.
  - Also `-netinfo [0-4|help]`, `-addrinfo`, `-generate [n] [maxtries]`, `-rpcwait`/`-rpcwaittimeout`, `-stdin`/`-stdinrpcpass`/`-stdinwalletpassphrase`, `-named`, `-rpcwallet` (→ `/wallet/<name>`), `-color`, `-rpcclienttimeout` 900, cookie/user/password and `-rpcconnect`/`-rpcport`, and `-conf`/`-datadir` plus chain selectors (it reads the same `dash.conf`).
  - Output: UniValue pretty-print with 2-space indent; stdout, stderr and exit codes are byte-identical to dash-cli (FR-CLI-02).
  - It is a **generic** client, not the typed `dashcore-rpc`, so every server method works on day one. It works against both rdashd and dashd (FR-CLI-04).
  - Tool subcommands (`tx`, `util grind`) are P1/P2 (FR-CLI-05).
- **`rdash-wallet`:** see §2.2. Its `import-core` covers US-IMP-03…06 and 13.

### 2.20 Wallet integration (extend, don't replace; DEC-02, FR-WAL-01)

**What stays.** `key-wallet` remains the engine for HD, accounts, derivation, coin selection and tx building. `key-wallet-manager::WalletManager` remains the multi-wallet container. Mobile, desktop and Platform keep consuming them unchanged.

**What `key-wallet`/`key-wallet-manager` gain.** These are additive PRs, each useful to the desktop too (R5 G-2…G-9):
1. A BIP39 passphrase end to end, and `MnemonicCompat::DashCore` for imports only: no NFKD, plus acceptance of Core's XOR checksum bug (accept both forms, prefer the one that derives known keys).
2. Imported-key, watch-only and seedless account types (`AccountType::ImportedKeys`).
3. Encryption at rest (FR-WAL-04, "at least as strong as Core's"): **argon2id + XChaCha20-Poly1305** under a master key, with unlock timeouts and mixing-only unlock (O). Core's crypter (`BytesToKeySHA512AES`, AES-256-CBC) is implemented **decrypt-only** in `dash-coreimport` for imports. F's choice of re-implementing Core's crypter for writing is rejected because the wallet file is not Core-compatible anyway (DEC-11).
4. P2SH/multisig signing, PSBT (BIP174 v0, no witness fields), and descriptor parse/eval for `pk`, `pkh`, `sh`, `multi`, `sortedmulti`, `combo`, `addr`, `raw`.
5. ProTx builders for every type, including v3 and shared types 10–12, with BLS operator signing and EdDSA Platform keys; collateral auto-lock (#1078).
6. Governance object prepare and vote signing.
7. A CoinJoin client via `dash-coinjoin`.
8. Message signing.
9. A fee-estimation hook (the node supplies estimates).

**`WalletInterface` additions** (default-implemented, so dash-spv compiles unchanged):

```rust
async fn process_block_disconnected(&mut self, block: &Block, height: CoreBlockHeight) -> BlockDisconnectResult { Default::default() } // #854
async fn process_mempool_removal(&mut self, txid: &Txid, reason: MempoolRemovalReason) {}
fn relevance_filter(&self) -> RelevanceFilter { RelevanceFilter::from_scripts(self.monitored_script_pubkeys(), self.watched_outpoints()) }
```

**`dashd-wallet`:**
- **Wallet state:** each `LoadedWallet { name, manager: WalletManager<ManagedWalletInfo>, db: WalletDb, lock_state, keypool_target, cj_client: Option<CoinJoinClient> }` consumes `ChainEvent`s on its own task, through the relevance pre-filter and the bridge (§1.9).
- **Rescans** (`rescanblockchain`, or loading a wallet behind the tip) read block files directly. When `blockfilterindex` exists they use the Dash-extended filters to skip blocks, **bounded by birth height**: the bait9 profiling showed unbounded BIP158 sweeps are not selective for 345K-script wallets.
- **Persistence:** SQLite (`rusqlite`, bundled), one file per wallet, in our schema with tables `meta` (version, network magic, flags), `seed`, `accounts`, `keys`, `scripts`, `txs` (canonical bytes, block hash, height and position, time received, `from_me`, flags incl. `DS`), `utxos`, `labels`, `purposes`, `destdata`, `locked_outpoints`, `coinjoin` (`cj_salt`, pending observations), `gov_objects` and `settings`.
  - Single-file wallets give Core's `backupwallet`/`restorewallet`/`loadwallet`/`unloadwallet` semantics.
  - Automatic backups follow `-createwalletbackups` with Core's naming (FR-WAL-13).
  - F's alternative (Core's record format inside SQLite, making our file openable by Core) is rejected under DEC-11, which makes wallet import one-way, with export tools for the way back.
- **Wallet kinds (DEC-02):** descriptor wallets by default. Legacy-HD semantics are kept for imported legacy wallets (`hdchain` counters; keypool lookahead = counters + 1000; legacy HD on BIP44 with CoinJoin on the same chain vs descriptor DIP9 `4'`), plus `createwallet descriptors=false`. The ~17 legacy-only RPCs behave as Core does for each kind, including Core's errors.
- **RPC surface:** all 71 Core wallet RPCs (R2b), plus the wallet-gated Dash ones: `protx register*`/`update*`/`revoke`/`shared_*`, `gobject prepare`/`list-prepared`/`vote-many`/`vote-alias`, `coinjoin *`, `coinjoinsalt *`, `masternode outputs`, `setcoinjoinrounds`/`amount`.
  - Coin selection keeps key-wallet's BnB by default (policy, not consensus). `-coinselection=knapsack-compat` is available where tests assert Core's exact change behaviour.
- **Core wallet import** (`rdash-wallet import-core`, the `importwallet` RPC):
  - **SQLite:** read the `main` table directly. **BDB 4.8:** a pure-Rust read-only btree parser (4096-byte pages, the `main` subdatabase, overflow pages), modelled on `test_framework/bdb.py`.
  - **Record mapping:**
    - `hdchain`/`chdchain` (seed, mnemonic, passphrase, per-account counters) → a BIP44 account with counters;
    - `walletdescriptor*` → accounts, with the mnemonic from Dash's `walletdescriptorkey` extension;
    - loose keys → the imported-key account;
    - plus watch-only scripts, labels and purposes, `lockedutxo`, `cj_salt` (needed for `IsFullyMixed` parity), the `ps_salt` migration, `cj_pending_obs` and `g_object`.
  - Encrypted wallets need the passphrase at import. The import report lists anything unmapped.
  - Logic is ported from dashwallet-desktop `dw-compat` (BIP39 quirks, crypter, dump, SQLite with mnemonic, descriptors; ~2.3k lines) into `dash-coreimport::wallet`.
- **Export (US-IMP-09, FR-IMP-05):** the `dumpwallet` format, descriptor JSON, the mnemonic, and linearize-style block export. All are dashd-loadable.

**Desktop relationship (DEC-01, PRD FR-DSK-03).** The desktop stays on `platform-wallet` (SPV by default). It can link to a local rdashd over JSON-RPC (US-WAL-19) and use it as a trusted SPV peer, which the node serves through BIP157/37, headers2 and mnlistdiff/qrinfo. No new API is needed. `dashd-wallet` is a third consumer of the same `key-wallet` crates, not a fork.

### 2.21 `dash-coreimport`

These pure-Rust, read-only readers for a Core datadir are used by `-importcoredatadir`, regtest auto-import (§2.9), `rdash-wallet import-core`, `dash-conformance` (replacing BS2's private `blkfile.rs` over time), and the ST1 churn-trace extractor. Coverage is in the §2.2 row.

**Consistency rule:** blocks, index, chainstate and evodb must agree on one tip (`b_b4` vs `'B'`); otherwise the import refuses and suggests `safe` mode. The source must be stopped and locked, or be a filesystem snapshot. The destination is built in a staging directory, and only sealed files are hardlinked (§2.10, F09). Every import ends with a verification report (FR-IMP-06) that includes the before/after hashes of the source.

### 2.22 Test and tooling crates

- **`dash-conformance`** (exists; BS2) grows these subcommands:
  - `vectors`: the Core vector loader, `test-vectors/core/<commit>/` with `PROVENANCE.md`;
  - `digest extract|compare`: StateDigest v1 (§4.6);
  - `replay`, `mutate` (§4.7), `rpcdiff` (§4.11), `p2pdiff` (fingerprint glue), `functional` (the passlist runner glue), `scan` (chain scans), `shadow` (the live comparator).
  It is relicensed to MIT when Core vectors land (DEC-07).
- **`dash-oracle-sys`:** bindgen over `libdash_oracle.a`, built by `contrib/node/build-oracle.sh` from the pinned Core commits and cached under `/work/cache/oracle/<commit>`. It sits behind feature `oracle`.
- **`dash-testkit`:** see §2.2. **No code moves out of dash-spv**: the `test-utils` feature already exposes `test_utils`, which avoids upstream merge conflicts.
- **`node-fuzz`** (excluded from the workspace, like cargo-fuzz crates): §4.4. The existing honggfuzz `fuzz/` (`dash-fuzz`) is untouched.
- **`node-bench`** (B1): see §3.11.
- **Vectors:** under `test-vectors/core/<commit>/` (JSON from `src/test/data`, `test/functional/data` and transcribed C++ vectors). Each pure crate runs them in `tests/core_vectors.rs`. There is no separate vectors crate, which avoids dev-dependency cycles.

### 2.23 dash-spv: reuse vs replace

| dash-spv component | Node decision | Reason |
|---|---|---|
| Network manager, peer, handshake, discovery, reputation (`src/network/`) | **Replaced** by `dashd-p2p`; not modified | outbound-only, SPV handshake, no serving, no scoring; #902 is rewriting it upstream |
| BIP324 transport (#264, unmerged) | **Reused**, lifted into `dash-p2p-transport` | the same Dash short IDs and handshake; becomes shared code (CV5) |
| DNS discovery, addrv2 parsing, latency, port probe | ideas reused; a Core-faithful addrman replaces them | addrman behaviour is observable (fingerprint) |
| Storage (segments, `DiskStorageManager`, migrator) | **Replaced** by `dashd-store` + block files | SPV-shaped (height-keyed, single chain, wallet-matched blocks only) |
| Header/filter/block/masternode sync managers, `SyncCoordinator`, checkpoints, fork handling | **Replaced** by `dashd-chainstate` + the `PeerManager` | full validation, reorgs with undo, CL constraints |
| `sml_engine` and `dashcore::sml` | **Algorithms lifted** into `dash-evo` (§2.5) with R4's gap fixes; the engine is not used for validation. **Reused as a differential consumer**: dash-spv syncs from rdashd, and its engine must validate our `mnlistdiff`/`qrinfo` (SPV-as-consumer) | verified code is reused; an independent check of our serving output |
| BLS quorum-commitment / CL / IS verification | algorithms reused in `dash-evo::llmq`, on base-sdk types; **fixtures regenerated** from the oracle (R4 F12: `mn_list_diff_0_2227096.bin` and the testnet `.bin` are not Core-produced) | correctness |
| Chain work, checkpoints, `ValidationMode` | **Replaced** by `dash-consensus` (DGW/KGW); dash-spv can adopt it later (fixes #1116/#332; CV5) | — |
| Mempool manager | **Replaced** by `dashd-mempool` | wallet-only tracker |
| `test_utils` (`DashdTestContext`, `DashCoreNode`, `masternode_network.rs`), `contrib/setup-dashd.py`, datasets `regtest-40000`/`regtest-200`/`regtest-mn` | **Reused in place** through `dash-testkit` | the natural seed for the harness |
| `dashd_sync` (35 tests) and `dashd_masternode` (10 tests) suites | **Reused**: run with rdashd as the only peer (SPV-as-consumer) | serving conformance |
| `HashedBlockHeader`/`HashedBlock` | superseded in the node by `dash_consensus::Block` | — |
| `dash-spv-bench` (docker dashd peers, netem, pprof) | folded into B1 where useful | — |

### 2.24 Coexistence with SPV, wallet and Platform consumers (NFR-CMP)

- The only existing-crate changes the node needs:
  - the Base SDK milestone's correctness fixes (BS15 X11, BS16 Network mapping, V24a v24 payloads, BS18 protocol constants), each with a migration note, batched as Platform-impacting changes (NFR-CMP-01);
  - the additive wallet extensions (§2.20);
  - the feature-gated bridge (BS14).
- `cargo deny` rule R2 guarantees node crates never create a reason to change `dashcore`.
- The FFI crates are untouched; #776 (deleting them) is orthogonal.
- Persisted bincode layouts stay pinned by tests (R3 §4.1).

### 2.25 Release pinning: v24 only (DEC-82; replaces rev1's `ReleaseProfile`)

Every network targets the v24 line, pinned to v24.0.0-rc.3. There is no `ReleaseProfile`, no `UngatedRules`, no `-releaseprofile` option and no per-release code in the node's crates. The only `Release` left is `Release::V24_0`, the value `dash-params`' chain-parameter builder takes (the base-sdk fork keeps its `V23_1` variant; removing it is a BSX/DEC-98 follow-up). RP-V24 removed the rest, and `contrib/node/pin-delta/check-release-refs.sh` refuses any other name.

Rev1 listed 26 differences between v23.1.8 and v24.0.x (rows 1–26) and read each at its call site through the profile. Each now has the v24 behaviour, unconditionally. In particular:
- The AU v2 stable txid is always used (`Transaction::txid()` is `stable_txid`, else the instance hash), and the block's transaction merkle root is computed once, over those txids.
- Script policy uses `MANDATORY_V24_0` (P2SH, DERSIG, NULLDUMMY, CLTV, CSV), the clock is `NodeClock`, and the IS conflict gate, sporks, object download, DKG criteria, MN init and the RPC surface are v24's.
- The BLS scheme flag commits only after a full connect (`ScopedBLSLegacyScheme`), so `Effect::BlsSchemeLeak` and the `bls_scheme_scope` rule do not exist.
- The `-fastprune` and rev-file finalize behaviour in `dashd-store` is v24's.

**What the audit is now.** `O17-pin-delta.md` and `O17-release-profile.md` stay as history: the 217 rows say which v23.1.8 behaviour each v24 behaviour replaced, and which v24 changes touch only data that is invalid under both. Where a task text cites a row, read it as "the v24 side of that row".

**How parity is proven.** Consensus goldens and `-statedigestlog` digests come from the v24 oracle (a live, synced mainnet and testnet node). DEC-82 still requires correct historical sync and validation of the existing chain (v24 validates history itself); the L2 full-chain replay covers it. There is no v23.1.8 reference node (O7 is dropped), no v23.1.8 proposal-mode rejection check, no `-releaseprofile` regtest run and no v23.1.8 fingerprint baseline.
---
## 3. Concurrency and performance

### 3.1 Runtime model

| Context | Kind | Default size | Owns / runs | Core equivalent |
|---|---|---|---|---|
| `net` | tokio multi-thread | `min(max(4, cores/2), 16)` | Sockets; transports; per-connection reader tasks (**transport framing only**, plus speculative, unobservable payload hashing) and writer tasks; RPC HTTP; REST; ZMQ; scheduler timers; the `ConnMan`/`AddrMan`/`BanMan` actors | `net`, `addcon`, `opencon`, `mncon`, `dnsseed`, `http`/`httpworker`, `scheduler` |
| `peerman` | 1 dedicated thread (single-threaded actor) | 1 | `PeerManager`: all `net_processing` state; Core's shuffled, wake-driven `ThreadMessageHandler` loop (the 100 ms throttle on MN connections only); decoding after the gates; awaits observable work (§2.14) | `msghand` |
| `validation` | 1 dedicated OS thread | 1 | `ChainstateManager`: header/block acceptance, connect/disconnect, evo state, mempool `remove_for_block` (synchronous, as in `ConnectTip`) | the `cs_main` holder (`msghand`/`loadblk`) |
| `prevalidate` | rayon pool | from the compute budget (below) | Parallel parts of a block's accept-time checks (S1, inside the awaited `ProcessBlock`), sighash prefix/suffix precompute, outpoint collection, **UTXO prefetch** (S2, `MultiGet`, epoch-fenced), X11 over header batches | none (Core does this inline under `cs_main`) |
| `script` | rayon pool | `-par` (0 = auto = cores−1; negative leaves cores free; Core caps at 15, rdashd at 64) | Block and mempool script checks, ECDSA | `scriptch.N` |
| `bls` | rayon pool | `-parbls` (default `max(2, cores/2)`; max 33 as Core) | BLS batch verification, MSM, DKG share verification, quorum warm-up | `bls-work-N`, `sigsh-work-N`, BLS `CCheckQueue` |
| `db-writer` | 1 OS thread | 1 | Block and undo appends, frozen-layer `WriteBatch` commits, fsync and WAL-sync durability points | inline in `FlushStateToDisk` |
| `events` | 1 OS thread | 1 | Ordered `ChainEvent` dispatch with backpressure (> 10 pending blocks validation, like `LimitValidationInterfaceQueue`) | `CScheduler` callbacks |
| `index-<name>` | 1 OS thread per enabled index | n | `BaseIndex` sync and follow | index `ThreadSync` |
| wallet | tokio tasks | one per wallet | Event application; rescans (on the `prevalidate` pool) | wallet callbacks |
| overlay workers | tokio tasks | — | Sig-share sessions, DKG round per (type, index), IS batch verification, CL enforcement, governance relay | `sigsh-*`, `llmq-<t>-<i>`, `isman`, `cl-schdlr` |

**Compute budget (rev1, GPT F12).** One `ComputeBudget` with priorities, not independent pools that can oversubscribe the machine:
- `-par` and `-parbls` keep Core's meaning, and the gating benchmarks pin them equal on both sides (NFR-PERF-02(c)).
- `prevalidate`, RocksDB background jobs, index threads and the wallet's rescan share what remains: `max(1, cores − par − parbls_workers)`.
- On machines with ≤ 4 cores, `prevalidate` runs on the `script` pool, and RocksDB gets 2 background jobs.
- PF5 runs the bench guards on a 4-vCPU / 8 GB profile as well as on agentbox.

**Rules.**
- Nothing on the `net` runtime blocks. The `peerman` actor blocks only where Core's `msghand` does: it awaits block, headers, tx and the synchronous overlay messages (§2.14). Other CPU work above about 50 µs goes to rayon through a `spawn` + oneshot handoff.
- Chainstate is touched only on `validation`. Everyone else reads the coherent views of §2.10.2: `ChainView`, seqlocked index entries, and `EvoReadView`s pinned to a block.
- **`ChainHelper` back ends** read immutable snapshots published before the connect starts and never call into validation, which would deadlock against `ChainCommand` replies (Opus m-4).
- No `async` exists in consensus, evo, chainstate, mempool or store APIs. Async callers use `ChainCommand` + oneshot.
- **Lock order** (checked by a debug-build lock-order checker): mempool write lock → coins read guard → RocksDB internals. No lock is held across an `.await`.

### 3.2 IBD and reindex pipeline

```
 peers ─(framing; speculative whole-payload/header hashing in reader tasks)─► PeerManager/BlockSync (window 1024, 16/peer, Core stall logic; awaits ProcessBlock)
   └─► S1 prevalidate[rayon, within the awaited accept of one block]: check_block (pow, merkle+mutation, sizes, coinbase, CheckTransaction, sigops),
          payload decode + trivial checks, special-tx payload hashes (inputsHash, SerializeHash(payload)),
          BLS deserialization of commitment fields, sighash prefix/suffix caches, outpoint collection,
          mark outpoints created inside the in-flight window
   └─► S2 prefetch[rayon, ahead on accepted-but-unconnected blocks]: MultiGet coins (DB snapshot, fetch_gen) → V admits if fetch_gen == released_gen (§3.4)
   └─► V  validation thread (sequential, in active-chain order): accept_block (index; enqueue the write) → connect_block
          (contextual checks, special txs, UTXO via the overlay; scripts → script pool; wait) → merge overlay → publish → events
   └─► W  db-writer: blk/rev appends; when the coins layer is frozen, the §3.5 flush: files durable → B1 (index + evo + marker) → coin sub-batches → Bk (tip)
   └─► E  events: indexes / wallet / ZMQ (bounded; backpressure into V like Core's queue limit)
```

- **S1** runs inside the accept of the block being processed: Core runs `CheckBlock`/`ContextualCheckBlock` synchronously in `ProcessNewBlock`, so S1 parallelises *within* a block and never reorders accepts. During reindex, which has no peers, S1 may run ahead across blocks; accept checks are pure, so that cannot be observed.
- **S2** runs up to `prefetch_depth` (64 by default) accepted blocks ahead of the connect point, and turns cache misses into hits. With `-dbcache` below the UTXO-set size, dashd stalls on synchronous LevelDB reads in `CCoinsViewCache::FetchCoin` on the `cs_main` thread; we take those reads off V.
- V is the only sequential stage. Under assumevalid, V's per-block work is the contextual checks, special-tx processing and hash-map updates.
- **Reindex and `-reindex-chainstate`** feed S1 from block files: a parallel scanner with Core's magic-scan semantics, then out-of-order arrival into the same pipeline.
- F's estimate that "~35% of Core's connect time is CheckBlock, txid, merkle and serialization" is **a hypothesis**. PF1 measures it on dashd 23.1.8 and 24.0.x with B1's `-debug=bench` log parse before it is used to size work. Even if it holds, removing all of that work leaves 65%: about 1.54× by Amdahl (GPT F11). Any 2× claim for connect must therefore come from measured savings across several stages, not from this one.

### 3.3 Script and signature validation

- **Check queue.** A per-block `Vec<ScriptCheck { tx_index, input_index, coin, flags }>` is built during the per-tx loop, exactly where Core builds `vChecks`. It runs as `par_chunks(128)` on the script pool with an `AtomicBool` early abort.
  - The reported result is **Core's observable result for each mode** (rev1, GPT F20; verified `core: validation.cpp:2481, 2751-2757, 2777-2780`):
    - **Parallel script checks** (`-par` > 1; the functional framework passes `-par=2`): `state.Invalid(BLOCK_CONSENSUS, "block-validation-failed")` with an empty debug message. Non-script errors in later transactions are found before the queue is waited on, so they take precedence over an earlier script failure, as in Core.
    - **Serial** (`-par=1`): the first failing input in block order is reported, with its `mandatory-script-verify-flag-failed (…)` reason.
  - v1's "lowest failing `(tx, input)` in every mode" invented an order Core does not expose, and is dropped.
- **Signature cache.** A salted (per-process random) SHA256 of `sighash ‖ pubkey ‖ sig`, truncated to 128 bits, in a 64-shard lock-free set-associative cache (Core uses CuckooCache).
  - Size comes from `-maxsigcachesize` (32 MiB by default, split ½/½ with the script cache).
  - Mempool acceptance inserts; block validation reads, and inserts per Core's semantics.
- **Script-execution cache.** `SHA256(nonce ‖ txid ‖ flags)` → verified; transactions accepted to the mempool skip script checks in `connect_block`, with Core's erase-after-block semantics for mempool-only entries. **Core already has it** (`validation.cpp:2166-2176, 2228-2231`), so it is parity, not a lever over dashd (F11).
- **Sighash precompute.** For `SIGHASH_ALL` inputs, the per-tx serialization prefix/suffix is cached (S1), so each input's sighash is one SHA256d over a reassembled buffer. Other hash types take the full legacy path. The result is bit-identical by construction and vector-tested; the reference twin is the plain serializer.
- **ECDSA.** `secp256k1` 0.33 (libsecp256k1) with a static context: `from_der_lax` plus `normalize_s` before verify, exactly as `CPubKey::Verify`. CHECKDATASIG hashes the message with a single SHA256. There is no "fast ECDSA" trick, so this is parity per signature.
- **BLS (dash-pkc / blst):**
  - **Basic-scheme signatures in a block** (QC `quorumSig`s, CbTx best-CL, asset unlocks, MNHF, ProUpServ/ProUpRev operator signatures) are **batch-verified per block** with random 128-bit scalars on the `bls` pool. A batch failure falls back to individual verification, which identifies the offender and yields the exact Core error.
  - **Legacy-scheme (pre-V19) signatures are always verified individually** (O). Legacy decoding skips subgroup checks, so the random-linear-combination soundness argument does not hold. F's design batched them; that is rejected.
  - `membersSig` uses `verify_secure_aggregated` (Core's `VerifySecureAggregated` semantics).
- **X11.** About 2.6M header hashes on mainnet, which is negligible in IBD. The backend is decided by BS15.

### 3.4 UTXO cache

```rust
struct CoinsCache {
    active: hashbrown::HashMap<OutPointKey, Entry, SaltedFxBuild>, // mutated by the validation thread only
    frozen: Option<Arc<FrozenLayer>>,                               // immutable; being written by db-writer (generation g)
    prefetch: PrefetchMap,                                          // owned by V; filled from S2 results that pass the epoch fence
    db: CoinsTable,                                                 // CF `coins`
    released_gen: u64,                                              // generation of the last frozen layer W finished and V released
    budget: CoinsBudget,                                            // one budget for active + frozen + prefetch + batch buffers (below)
}
#[repr(C)] struct OutPointKey([u8; 36]);                            // txid ‖ vout (LE)
struct Entry { coin: PackedCoin, flags: u8 /* DIRTY | FRESH | SPENT */ }
/// The Core `Coin` serialization itself, kept inline when short: a typical P2PKH/P2SH coin is 27–31 bytes
/// (≤4-byte height VARINT, ≤6-byte amount VARINT, 21-byte compressed script). Decoded on access; the same bytes go to the DB.
enum PackedCoin { Inline { len: u8, bytes: [u8; 31] }, Heap(Box<[u8]>) }  // Heap: P2PK, multisig, non-standard, long amounts
```

- **Read path:** block overlay → `active` → `frozen` → `prefetch` → DB. Tombstones (SPENT entries) in `active` and `frozen` are authoritative.
- **FRESH semantics** are exactly Core's: a coin created and spent inside the cache never reaches disk.
- **Freeze-and-flush:**
  - V moves `active` to `frozen` (an O(1) pointer swap) and starts an empty `active`. W writes `frozen` with the §3.5 protocol while V keeps connecting.
  - Triggers: `active` reaches the freeze threshold (below); a periodic or forced flush (Core's triggers, §3.5); or leaving IBD.
  - Only one frozen layer exists. If a second freeze is needed while one is still being written, V waits. This is the only point where IBD can stall on I/O.

**Prefetch correctness (GPT F05).** S2 issues `MultiGet`s against a DB snapshot ahead of V. A stale result could resurrect a spent coin: V spends X, W flushes the tombstone, F is released, and an older prefetch result for X is then consulted before the DB. The fence:
1. Each S2 result carries `fetch_gen`, the value of `released_gen` when its DB snapshot was taken (`KvEngine::snapshot()`).
2. V accepts a result into `prefetch` only if `fetch_gen == released_gen` at insertion. When V releases a frozen layer it increments `released_gen` and clears `prefetch`. Results that land later with the old generation are dropped. A dropped result only costs a DB read.
3. Prefetch caches **positive results only**. "Absent" is never cached, so a reorg that re-creates a coin cannot meet a stale absence. Core does not cache absence either.
4. Any `disconnect_block` clears `prefetch`.
5. `-consensusxcheck` compares every coins lookup with a direct read of the reference view (overlay → active → frozen → DB snapshot), not only the final digests. The `coins_cache_model` fuzz generates interleavings of spend, create, flush completion, prefetch completion, eviction and reorg.

**One memory budget (GPT F12).** The `-dbcache` coins share B bounds everything the cache holds at once:

`size(active) + size(frozen) + size(prefetch) + batch_buffer ≤ B`

- `batch_buffer` is the serialized sub-batch in flight, at most `-dbbatchsize`.
- The freeze threshold is `f·B`, so the new `active` may only grow to `B − size(frozen) − size(prefetch) − batch_buffer`. When it reaches that, V waits (Core's CRITICAL state).
- `f` defaults to 0.5. PF3 tunes it on the churn trace within the invariant. v1 froze at 90% and let a new active layer reach 90% again, about 180% of B, which is the overrun F12 found.
- Sizes are **measured allocations**, not estimates. A counting allocator in the bench build checks them, and a layout test asserts `size_of::<Entry>()` and the hashbrown load factor. v1's "about 32 B per coin" is withdrawn until PF1 measures it.

**`-dbcache` split** (300 MiB by default; one knob, as Core). All buckets come out of `-dbcache`, so the sum is the user's number:

| Bucket | Share |
|---|---|
| RocksDB shared block cache | 8%, max 64 MiB |
| `chainstate/` memtables, all CFs | 2 × 16 MiB write buffers per CF, capped at 64 MiB total by `db_write_buffer_size` |
| Coins cache B (active + frozen + prefetch + batch buffer) | the remainder |
| Block index | as Core: the arena is not counted against `-dbcache` |

- Index DBs get their own caches under Core's index-cache rules.
- `getmemoryinfo` and B1's RSS split report each bucket. PF5 adds an aggregate RSS budget covering the in-flight block window, the block LRU, MN-list caches and allocator overhead, so the total is accounted for, not only the coins share.

**Density.** A packed entry is meant to be denser than Core's pool-allocated `CCoinsMap` node: key, `Coin` with a 28-byte `CScript` prevector, dirty linkage, bucket and pool overhead. The number is **not** claimed until PF1 measures both sides. If it holds, more coins fit per MiB at equal `-dbcache`, so there are fewer misses and flushes. F's `CompactScript` needed re-serialization at flush, so O's `PackedCoin` is kept.

**Other caches:**

| Cache | Contents | Bound |
|---|---|---|
| Block LRU | `Arc<Block>`, recently accepted and connected | 64 blocks or 128 MiB |
| MN lists | `Arc<MnList>` (structural sharing) | LRU of 64, plus mini-snapshots every 32 blocks |
| Quorums | `Arc<Quorum>` with lazily computed pubkey shares | warmed on build |
| BLS verify cache | for recsig, islock, clsig | 30k entries |
| Credit pool | — | LRU 1000 |
| Versionbits state | per (deployment, period-end block) | in memory, rebuilt on startup; never compared as state (§4.6) |

### 3.5 Durability and recovery protocol (rev1)

This section replaces v1's sketch, which had three defects: startup replay of `HAVE_DATA` blocks with signatures off, a split-flush marker that could name a block whose index entry was not yet on disk, and a database commit that could become durable before the file bytes it referenced (GPT F01–F03, Opus M-4). It is written against the primitives `dashd-store` already exposes (task ST1, `dashd-store/src/engine.rs`): `KvEngine::write(batch, Commit::{Buffered, Durable})`, `sync_wal()`, `flush_memtables()`, `snapshot()` and `ErrorKind::NoSpace`. The flat-file side (`FlatFileSeq`, with `fdatasync` and directory sync) is task ST2.

#### 3.5.1 Three guarantees, kept apart

| Guarantee | Failure it survives | What it relies on |
|---|---|---|
| **Atomic visibility** | none (another thread, or a reader after restart) | one `write(batch, _)` is all-or-nothing across the column families of one RocksDB |
| **Process-crash recovery** | SIGKILL, panic, OOM kill | the OS page cache survives, so every returned `write` (Buffered or Durable) and every completed `write(2)` to a flat file survives |
| **Power-loss durability** | power cut, VM kill, kernel panic | only bytes covered by a completed `fdatasync` (file data), `fsync` of the directory (file names, sizes after truncation), or a `Commit::Durable` write / `sync_wal()` (database) |

Persistence assumptions, which ST3 tests rather than trusts:
- The device honours flush/FUA; the filesystem is ext4, XFS or btrfs with ordered metadata. Nothing relies on `rename` being atomic without a directory `fsync`.
- After power loss, unsynced flat-file pages can survive or vanish in any combination, and file sizes can revert.
- The RocksDB WAL is one ordered log, so recovery (`kPointInTimeRecovery`) yields a **prefix** of the commit order. `chainstate/` runs with the WAL on and `atomic_flush = true`, so a memtable flush can never leave one column family ahead of the recovered WAL prefix. (rust-rocksdb exposes `set_atomic_flush`; ST1's engine does not set it yet, see PLAN §8.1 (ST1 delta).)

#### 3.5.2 Stores and markers

- **Flat files:** `blocks/blk*.dat`, `blocks/rev*.dat` (Core-identical).
- **`chainstate/`**, one RocksDB: `meta`, `block_index`, `block_files`, `coins` and the evo column families (§2.9).
- **`meta` keys** that matter here:
  - `tip`: the durable coins-and-evo tip. It is **absent** while a transition is in flight.
  - `head_blocks = [new, old]`: present only during a transition. `old` is the previous durable tip, or null on the first flush.
  - `evo_tip`: the block the evo column families describe.
- **Other stores** (`llmq/`, `indexes/*`, wallets) are separate databases that follow `ChainEvent`s with their own locators (§3.5.7).

#### 3.5.3 Invariants

- **I1 — data before reference.** No durable record that references flat-file bytes (a `block_index` entry with `HAVE_DATA`/`HAVE_UNDO` and its positions, a `block_files` size) is committed before those bytes are covered by `fdatasync` and, for files created or truncated since the last directory sync, by an `fsync` of `blocks/`.
  - The directory's `fsync` is Core's `DirectoryCommit` (DEC-212, BL-124): one attempt in each file's `Flush`, never retried. A failure is logged as a warning (Core logs nothing) and the flush goes on; `blocks/` stays unsynced until the next barrier's `Flush` syncs it. After a failed directory `fsync`, a power loss can lose a new file's entry that a durable record references, as it can in Core; ST3a's R1 and D1(a) entitle exactly that. The same holds for `blocks/`'s own entry in the datadir. Core never syncs the datadir; the node syncs it at each barrier until a sync of both succeeds, per sequence (extra durability, not a parity change), attempted whatever `blocks/`'s sync returned, and a failure is logged and passed over the same way. The file's bytes are still covered by `fdatasync`, whose failure aborts (I5).
- **I2 — every prefix recovers.** Every prefix of a flush's commit sequence is a state that §3.5.5 recovers to a consistent (coins, evo, index, meta) state without a reindex.
- **I3 — no validation bypass.** Startup connects every block above the recovered tip through the ordinary `connect_block`, with the normal assumevalid decision. No status bit (`HAVE_DATA`, `VALID_TRANSACTIONS`, or even `VALID_SCRIPTS` from an index-only write) authorises skipping a check. The only signature-free path is the coins replay of the single transition named by `meta.head_blocks`, whose blocks were fully connected before the flush began. This is exactly Core's split: `ReplayBlocks`/`RollforwardBlock` touch only the blocks the head-blocks marker names, and everything else goes through `ActivateBestChain` → `ConnectBlock` (`core: validation.cpp:5218-5298`, `node/blockstorage.cpp:947-953`).
- **I4 — idempotent recovery.** Recovery can be interrupted at any point, including by a second power loss, and re-run to the same result.
- **I5 — fail-stop.** After any error from a flat-file write, an `fdatasync`, a `write` or a `sync_wal`, nothing more is committed to `chainstate/`. The node aborts, and the durable state is what the last completed durability barrier covered.

#### 3.5.4 The flush

The `db-writer` thread W runs this for the frozen layer F (tip `new`, previous durable tip `old`). The order is Core's: files, then the block index, then coins and evo (`core: validation.cpp:3048-3095`). The deliberate deviations are marked †; each is visible only after a crash or a disk failure.

1. **Space.** `CheckDiskSpace(blocks/, 50 MiB)` and `CheckDiskSpace(datadir, 48·2·2·cache)`, as Core. A failure aborts with Core's text, `"Disk space is too low!"`, before anything is written.
2. **Files.**
   - Finish every queued `blk`/`rev` append for the blocks in F.
   - `fdatasync` **every** `blk`/`rev` file written since the last flush. † Core syncs only the current pair (`FlushBlockFile` at `:3056`), so undo written into an older, lagging `rev` file is never synced even though the synced index records `HAVE_UNDO` for it.
   - Apply Core's finalize/truncate rules when a file rolls over.
   - `fsync(blocks/)` if any file was created or truncated since the last directory sync.
3. **B1, `Commit::Durable`.** This is the index write Core makes with `sync=true` (`txdb.cpp:285`), extended with every non-coin delta of F:
   - every dirty `block_index` and `block_files` record;
   - all evo deltas of F: MN diffs and snapshots, credit pool, MNHF signals, `llmq_mined` puts and erases, `llmq_mined_h` (the frozen window's merged index, the same as the deltas in block order with later rows winning; CH7 D22). The BLS scheme is not persisted: it is derived from the deployment state at the block (CH7 D9);
   - `meta.evo_tip = new`, `meta.head_blocks = [new, old]`, and the erase of `meta.tip`.

   If F's coin entries fit within `-dbbatchsize` (16 MiB, as Core), they also go into B1, together with `meta.tip = new` and the erase of `head_blocks`. The whole flush is then one atomic Durable batch, and steps 4–5 are skipped.
4. **B2…Bk−1, `Commit::Buffered`.** Coin sub-batches of at most `-dbbatchsize` each, holding Core's `BatchWrite` entries: puts for dirty coins, erases for spent ones, nothing for fresh coins that were also spent.
5. **Bk, `Commit::Durable`.** The last coins, `meta.tip = new`, and the erase of `meta.head_blocks`.
6. **Release.** Release F (the prefetch epoch advances, §3.4) and emit `ChainStateFlushed { locator(new) }`.
7. **Prune.** Only now, unlink the files whose `HAVE_DATA` B1 cleared, then `fsync(blocks/)`. † Core unlinks after the index write and before the coins write (`:3068-3071`). After a crash there, its on-disk coins tip can need blocks it has already deleted.
8. **Periodic index-only write.** Core's hourly `fPeriodicWrite` is step 2 followed by one Durable batch of dirty `block_index`/`block_files` records.

**Why all non-coin state goes into B1** (Opus M-4, option 1). The transition `old → new` is fully known in memory before W starts. Committing evo at `new` atomically with the marker makes recovery **coins-only**. That rules out the state v1 could reach, with coins at `new` and evo at `old` (F02, M-4). It also removes the need to port `RollforwardBlock`'s special-tx re-execution, and Core's own window between the coins flush and the EvoDB commit (`:3090`→`:3095`), which ends in "Found EvoDB inconsistency, you must reindex" (`:1924-1930`), cannot occur.

**Why two Durable commits.** B1 must be durable because it carries the block index, as Core's synced index write does. If it were lost, blocks received since the previous index write would sit in `blk` files with no index entry. Bk must be durable before step 7. A Buffered Bk would also be crash-safe, since every prefix recovers, but pruning must not run before the new tip is durable. The cost is two WAL fsyncs per flush, and flushes are minutes apart.

**Prefixes (I2).** Every prefix of the sequence recovers:
- none of B1: the old state, consistent;
- B1 alone, or B1…Bj: a marker is present, so recovery runs;
- the full sequence: the new state.

#### 3.5.5 Startup recovery (Core `ReplayBlocks`)

1. **Load the block index.** As Core, PoW is checked against the **stored** hash; X11 is not recomputed (m-11).
2. **No marker.** If `meta.head_blocks` is absent, `meta.tip` must be present and equal `meta.evo_tip`. Otherwise refuse with Core's "Found EvoDB inconsistency, you must reindex". This is unreachable by construction and is kept as a corruption check. Continue at step 5.
3. **Marker `[new, old]`.**
   - `new` must be in the index with `HAVE_DATA`, which I1 guarantees.
   - `old` must be null or in the index.
   - `meta.evo_tip` must equal `new`.

   Any failure ends with Core's message: "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate."
4. **Coins-only replay.** Let `fork = LCA(old, new)`.
   - For each block b from `old` down to `fork+1`: apply b's undo record to `coins` tolerantly (re-add the coins it spent, erase the outputs it created), as Core's `DISCONNECT_UNCLEAN` path does.
   - For each block b from `fork+1` up to `new`: read b, erase its inputs (missing is fine) and put its outputs (overwrite).
   - No script, signature, value or special-tx check runs, and evo is not touched; it is already at `new`.
   - Write coin sub-batches (Buffered) with the marker still in place, then finish with one Durable batch: `meta.tip = new` and erase `meta.head_blocks`.
   - An interruption leaves the marker in place, so the next start runs step 3 again (I4). Every operation is an overwrite or an erase of a key the transition touches, so re-running converges.
5. **Load the tip.** `LoadChainTip`, derive the candidate set, run `VerifyDB` (`-checkblocks=6`, `-checklevel=3`).
6. **Activate.** `activate_best_chain` through the ordinary `connect_block` (I3). Any `HAVE_DATA` block above the tip is validated as if it had just arrived. A block that passed `accept_block` but has invalid scripts is marked `FAILED_VALID` exactly as on its first connection (the GPT F01 test: PLAN CH3b, M3 exit 2).

#### 3.5.6 Errors and disk full (I5)

- **Flat files.** ENOSPC or EIO from a write or `fdatasync` aborts; a failed directory `fsync` is logged and passed over (I1, DEC-212, BL-124). The node logs Core's strings: `"Disk space is too low!"` and `"Flushing block file to disk failed. This is likely the result of an I/O error."`. It sets the warning, raises `AbortNode`, which starts shutdown with exit status 1, and commits nothing further.
  - † Core keeps writing the index, coins and EvoDB in the same flush after a failed block-file fsync (`blockstorage.cpp:600` returns `void`).
  - A failed `fdatasync` is never retried: the kernel may already have dropped the dirty pages.
- **Database.** A `write` or `sync_wal` error, including RocksDB's `ErrorKind::NoSpace` (after which RocksDB may go read-only), logs `"System error while flushing: …"` and aborts the same way.
- **Shutdown.** After an abort, the shutdown flush is skipped. On the next start, with space freed, recovery runs from the last durable state.
- **Test hook.** `-dbcrashratio`, Core's crash hook for `feature_dbcrash`, calls `_exit` at random sub-batch boundaries.

#### 3.5.7 Effects, readers and subscribers

- **Non-transactional effects** (§2.10.1). Rotation snapshots and Core's other immediate writes to `chainstate/` are committed as their own small Buffered batches at the moment Core makes them. The WAL is one ordered log, so any later durable flush implies they are durable too. That is the same prefix argument Core's single EvoDB log gives its raw snapshot writes.
- **Readers** never read `meta.tip`; they read `ChainView`. A snapshot reader (§2.10.2) consults the frozen layer before the DB snapshot, so it never sees a partially written transition.
- **Subscribers** (`llmq/`, every index, wallets) persist their best-block locator only after the `ChainStateFlushed` of a flush that covers it. This is Core's `BaseIndex::Commit` on `ChainStateFlushed`. After a crash they rewind to that locator and re-apply `BlockConnected`. Re-delivery is idempotent, and a `BlockDisconnected` for a block above the locator is handled (M-4 recommendation 4). The `llmq/` DB keeps Core's semantics: IS "mined" marks and recsig writes happen as events arrive, so they can be ahead of the chainstate after a crash, exactly as in Core.

#### 3.5.8 What tests this (PLAN ST3a, ST3b, CH2, CH7a)

- **Model level.** `FaultyEngine` and `FaultyFlatFiles` wrap the in-memory engine and record every write, sync and commit. At any point they can "lose power": keep the synced prefix, an adversarial subset of unsynced file pages and a prefix of the unsynced DB batches. They then reopen and compare against a reference model. They can also inject ENOSPC and EIO at every call site. `coins_cache_model` and the chainstate model fuzz drive them.
- **Block-device level.** `dm-log-writes` records rdashd's I/O on regtest through flushes, reorgs, a prune and split flushes (tiny `-dbcache`, small `-dbbatchsize`). Every prefix at each flush or FUA mark, plus random points between, is replayed onto a fresh device. rdashd must recover without a reindex, and its StateDigest at the recovered tip must equal the oracle's.
- **Double crash.** The same replay is run over the recovery's own I/O.
- **VM level.** rdashd runs in a KVM guest with `cache=none`. The guest is powered off hard (`kill -9` on QEMU) at random points in IBD, reindex, reorgs and flushes, then restarted, and digests are compared.
- **Process kill.** `SIGKILL` tests stay, but as smoke tests only: a process kill never loses the page cache (F03).
- **Cases covered:** the first flush (`old` null); a reorg inside the flush window; the V19 and V24 boundaries; DIP24 cycle bases (snapshot effects); pruning; disk full (a filled loop device); `fsync` failure (`dm-flakey` with `error_writes`); and an accepted block with invalid scripts persisted before its connection (F01).

### 3.6 Evo and LLMQ performance levers (rev1: only what Core does not already do)

Core already has these, so they are **parity features, not levers** (GPT F11, all verified):
- persistent `immer::map` MN lists (`evo/deterministicmns.h:142-144`, also in v23.1.8);
- member caches;
- parallel QC signature checks through `CCheckQueueControl<BlsCheck>` (v23.1.8 `llmq/blockprocessor.cpp:208-225`; any failure is the generic `bad-qc-invalid`);
- immediate ChainLock enforcement (0 s scheduling, §2.15).

What remains:
1. **Payee selection:** O(n) → O(log n) through `payee_queue`, with a reference twin and xcheck. Core scans every valid MN per block in `GetMNPayee`: about 3.5k MNs × about 1.5M post-DIP3 blocks. This also benefits GBT and `masternode winners`. PF1 measures its share of connect time before the twin's complexity is accepted.
2. **SML hash caching.** `merkleRootMNList` hashes only changed entries (`Dmn::sml_hash` is cached because `Dmn` is immutable); the root itself is about n SHA256D64 compressions. This matters without assumevalid and at the tip.
3. **Structural MN-list diffs** (`OrdMap::diff`) instead of a full comparison. Cheap copies exist on both sides.
4. **BLS:**
   - Pippenger MSM for recovery (`Σ λ_i·sig_i` in G2, about 240 terms for LLMQ_400_60 CLs) and for DKG share verification (polynomial evaluation in G1, up to 340 coefficients).
   - Random-scalar batch verification across a block's basic-scheme signatures (§3.3), where Core checks them individually in parallel.
   - blst vs relic per operation, measured by B1's microbench.
5. **Overlay work is node-local only.** Verification and recovery can run on arrival. Every externally visible output (isdlock and recsig relay, sig-share sends, qfcommit, DKG messages) is released on Core's worker ticks (§2.15; Opus M-6), so the end-to-end latency matches dashd's. Only node-local CPU improves.

### 3.7 Mempool and GBT (rev1)

- **ATMP** is serial-equivalent in arrival order (§2.12).
  - Throughput gains come from multi-input script checks on the pool, from concurrent RPC submissions, and from cheaper bookkeeping.
  - They do **not** come from interleaving P2P transactions, which the actor processes one at a time, exactly as Core does (M-5).
  - Core runs `MemPoolAccept` under `cs_main` plus `mempool.cs`, *including* script verification, on one thread.
- **GBT:**
  - Selection walks the ancestor-score package index, as Core's does (`miner.cpp:522`).
  - The `BuildNewListFromBlock` memo is keyed by the exact block (§2.13) and saves the second `ProcessSpecialTxsInBlock` inside `CreateNewBlock`'s own `TestBlockValidity`.
  - `test_block_validity` runs the full connect logic in a dropped overlay, applies its effects (§2.10.1), and hits the script-execution cache, as Core does.
  - The **template cache follows Core's policy** (new tip, or a mempool change after more than 5 s). A warm call therefore costs the same as Core's warm call; the gain is in cold builds. v1's claim that "Core rebuilds per call" was wrong.
- Fee-estimation reads are lock-free snapshots.

### 3.8 Memory and disk plan

| Component | dashd (estimate; B1 measures) | rdashd plan |
|---|---|---|
| Block index, 2.6M entries | **O:** ≈ 560–580 MB (`CBlockIndex` ≈ 152 B + map node, hash and buckets ≈ 70 B); **F:** ≈ 150 B/entry | arena ≈ 120–147 B/entry + `HashTable<u32>` ≈ 5 B; PF1 measures before any claim |
| Coins cache | the `-dbcache` share | the same budget, holding active + frozen + prefetch + batch buffer (§3.4); density claimed only after PF1 |
| MN lists | `MAX_CACHED_LISTS` 64 + a diff cache of 576×k (immer) | LRU 64 with `imbl` sharing; diffs stay on disk |
| Mempool | up to 300 MB | the same `-maxmempool` accounting; smaller entries (no prevector) to be measured; `Arc<Tx>` shared with relay |
| Sig/script caches | 32 MiB | 32 MiB (parity) |
| RocksDB | n/a (LevelDB: 2 MiB + dbcache share; mmaps up to 1000 SSTs) | block cache and memtables from the `-dbcache` share; bloom ≈ 10 bits/key (≈ 8 MiB for 6M coins) |
| Allocator | glibc malloc | **jemalloc** by default (`tikv-jemallocator`, already in the workspace through dash-spv-bench; heap profiling for the RSS work), with mimalloc as the candidate. PF2 picks by measurement before M5 |

- **RSS measurement:** B1 reports `RssAnon` and `RssFile` separately. LevelDB's mmapped SSTs inflate dashd's file-backed RSS; rdashd avoids mmap for block reads and uses an anonymous block cache. A like-for-like comparison needs both values, plus the aggregate budget of §3.4.
- **Disk** (per component, as NFR-RES-02 requires, not only in total):
  - block and undo files are byte-identical to Core's, so the same size;
  - chainstate is about LevelDB's size, minus compression on the evo CFs; the per-CF peak during compaction is measured too (F12);
  - txindex is **≈ 1.5–2.5× smaller**, not v1's "3×" (Opus m-14): Core stores a 33-byte key and a 3-VARINT value, we store 8 + 12 bytes. B1 quotes the measured number;
  - addressindex benefits from prefix compression.
  - **Net:** no larger than dashd, plausibly a few percent smaller. **2× is impossible** while the block files stay Core-identical (DEC-11). The PRD requires "no more disk" (NFR-PERF-03, NFR-RES-02).

### 3.9 Metric by metric: expectations and declared shortfalls (rev1)

The rows are the PRD's 18 tracked metrics (NFR-PERF), with B1's `node-bench` scenario and metric names.
- "Expected" is a **hypothesis until PF1 measures it** (GPT F11). Each row names the dashd cost that is actually *removable*, i.e. something Core does and we do not, and the Amdahl ceiling that follows from it.
- Baselines are dashd 23.1.8 for mainnet workloads and 24.0.x for testnet, regtest and devnet workloads (NFR-PERF-02(d)).
- **Gating comparisons** pin equal `-par`, `-parbls` and `-dbcache` on both sides (NFR-PERF-02(c)).
- **Default-config runs** (rdashd's higher `-par` cap) are reported separately and never gate.

| # | Metric (PRD) | node-bench scenario · metric | Removable dashd cost (verified absent in Core) | rdashd lever | Expected vs dashd | Declared limit / shortfall |
|---|---|---|---|---|---|---|
| 1 | IBD to tip, default assumevalid | `ibd-{mainnet,testnet}-{seed,net}` · `ibd_time_s`, `blocks_per_s`, `header_sync_s` | synchronous UTXO misses on the `cs_main` thread; O(n) payee; synchronous flush stalls; single-threaded accept-time checks | in-block parallel accept checks, epoch-fenced prefetch, asynchronous flush, O(log n) payee | **1.3–2×** on `-seed`, to be confirmed by PF1 | `-net` variants are **network-bound** (16 blocks in flight per peer kept for fingerprint): parity is the honest expectation |
| 2 | IBD, `-assumevalid=0` | `ibd-*-noav` · same | per-block SML rehash; individual BLS QC checks; the same as #1 | cached SML hashes, basic-scheme BLS batching, sighash precompute | **1.2–1.6×** at equal `-par` | **2× at equal threads is unlikely**: per-signature ECDSA cost is identical |
| 3 | Reindex, reindex-chainstate | `reindex-{mainnet,testnet}`, `reindex-chainstate-mainnet(-noav)`, `reindex-chainstate-testnet-noav` · `reindex_time_s`, `blocks_per_s`, `cpu_ms_per_block` | as #1, plus a single-threaded file scan | a parallel scan (no peers, so cross-block S1 is allowed), the same pipeline | **1.5–2×** (default AV); as #2 for `-noav` | disk read bandwidth |
| 4 | Wallet rescan | `rescan-mainnet` · `rescan_time_s` | sequential block reads and per-tx wallet matching | direct block-file reads, a hash-set relevance pre-filter, filter skipping bounded by birth height | **≈ 2×** | disk read bandwidth for full-history rescans |
| 5 | Per-block connect latency | B1 `bench_log` ConnectBlock timings (p50/p95, last 50k blocks and live) | O(n) payee, the full SML root, synchronous notifications | O(log n) payee, incremental SML, asynchronous events. Script-exec cache hits are **parity** (Core has them) | **≈ 1.2–1.5×** until PF1 shows otherwise (v1's 2× counted the script cache) | dominated by BLS when a block carries QCs |
| 6 | Validation throughput | `microbench` · `micro_*_ns` (script/sig, BLS, X11, block) vs criterion equivalents | relic BLS | the same libsecp256k1; lock-free sigcache; blst; batching | ECDSA **1.0–1.3× per core**; BLS aggregate verify ≥ 1.5×; X11 parity | **ECDSA 2× per core is not achievable** (no batch verify for ECDSA); X11 parity until AES-NI dispatch (BS15) |
| 7 | Mempool accept | `mempool-accept` · `tx_accept_tps`, `tx_accept_latency_ms`, `_p99_ms` | single-threaded script verification inside ATMP | multi-input scripts on the pool, concurrent RPC submissions, cheaper bookkeeping; serial-equivalent order (§2.12) | **1.3–2×** throughput for multi-input load; latency ≈ 1–1.3× | single-tx latency is bounded by signature cost (**2× latency declared not achievable**); P2P tx arrival stays serial (M-5) |
| 8 | GBT time | `gbt-regtest` · `gbt_ms_mempool_<N>` | the second `ProcessSpecialTxsInBlock` in `TestBlockValidity` | the exact-block DML memo, O(log n) payee | **≈ 1.2–1.5× cold; ≈ 1× warm** (both sides cache, with the same policy; v1's "≥ 5× warm" is withdrawn) | the template cache is Core's |
| 9 | Startup to ready | `startup-{regtest,mainnet}` · `startup_s`, `startup_cold_s`, `shutdown_s` | sequential block-index load | parallel decode into the arena; PoW against the stored hash (as Core) | **≈ 1.5–2×** | the `-checkblocks` verification work is identical |
| 10 | Memory | `peak_rss_mib`, `steady_rss_mib`, `idle_rss_mib` (IBD, startup, mempool scenarios) | glibc fragmentation; map-node overheads | compact index and coins under one budget (§3.4), jemalloc/mimalloc | **≤ 0.85×**, to be measured | **½ (NFR-PERF-03) is unlikely**: the coins budget is set by `-dbcache` |
| 11 | Disk | `disk_*_gib`, `disk_total_gib` | full-key txindex | compact txindex, evo-CF compression | **≈ 0.95×** | **2× is impossible**: block files dominate and are Core-identical (DEC-11); the PRD requires "no more", per component |
| 12 | CPU per block | `cpu_ms_per_block`, `cpu_s` | copies, re-serialization, O(n) payee | zero-copy hashing, `Arc` sharing; offset by pool handoffs and decode costs, which are counted | **≈ 0.8×** CPU-seconds, to be measured | PF0's instruction counters are the per-PR guard |
| 13 | RPC | `rpc-{regtest,mainnet}` · `rpc_<call>_ms`, `_p99_ms`, `_ops` | `cs_main` contention for read RPCs that need only the tip summary | lock-free `ChainView` reads; queries needing cross-entry coherence still serialize on V, as Core does (§2.10.2) | **≥ 1.5×** throughput for tip-summary reads; ≈ 1× for queries Core runs under `cs_main` | wallet RPCs are bound by SQLite |
| 14 | DKG round | `mn-dkg-regtest` · `dkg_round_s`, `dkg_phase_<n>_s` | — (phases are driven by block height) | faster contribution and verification work (MSM, batching) | node-local CPU work **2–5×**; wall clock ≈ 1× | **wall clock is protocol-bound** (phases span blocks): 2× impossible; "not slower" applies |
| 15 | Recovered-signature latency | `mn-recsig-regtest` · `recsig_ms` | relic per-operation cost | blst, MSM recovery; outputs on Core's ticks (M-6) | node-local CPU **≥ 2×**; **end to end ≈ 1.0×** | a declared shortfall under NFR-FP: releasing earlier than Core's ticks would be a fingerprint |
| 16 | InstantSend lock latency | `mn-islock-regtest` · `islock_ms` | as #15 | as #15 | as #15 | as #15 |
| 17 | ChainLock latency | `mn-chainlock-regtest` · `chainlock_ms` | as #15. Enforcement is already immediate in Core (0 s scheduling); v1's "5 s polling" premise was wrong | as #15 | as #15 | as #15 |
| 18 | CoinJoin servicing | `mn-coinjoin-regtest` · `coinjoin_round_s` | — (timeouts are protocol-defined) | — | ≈ 1× | **protocol-bound**: 2× impossible |
| — | SPV regression guard (not a charter metric) | `spv-headers-*` · `header_sync_s`, `peak_rss_mib` | — | — | not slower after the BS changes | — |

Every shortfall above is recorded with its explanation in `bench/REPORT-<version>.md` (REL-6.3). PF1 runs **before** the costly optimisations (CH4's pipeline, CS6's caches, the payee queue) are committed to. It produces the serial-fraction and resource budget that the "Expected" column must respect.

### 3.10 Safety rules for optimisations (charter, NFR-PERF-04, NFR-CEQ-12)

1. **Dual path.** Every algorithmic replacement of a consensus computation keeps a line-by-line Core port as its reference twin, compiled under `cfg(any(test, feature = "xcheck"))`:
   - payee queue vs linear scan;
   - incremental vs full SML root;
   - batched vs individual BLS;
   - prefetch vs direct reads, at every lookup (§3.4);
   - the exact-block DML memo vs a fresh `BuildNewListFromBlock`;
   - MSM vs naive Lagrange;
   - sighash precompute vs plain serializer;
   - template cache vs cold build, after each same-tip dependency change (§2.13).
2. **Cross-check mode.** `-consensusxcheck=1` (debug builds and CI replay) runs both paths and aborts on any difference. Replay runs with it on at every milestone tag.
3. **Determinism lint** (§2.3).
4. **Review.** Optimisations of consensus code get two independent reviewers, the `code-review` skill at high effort and a green differential suite (L1–L3 unchanged). An optimisation must also not create a fingerprint (NFR-PERF-05): any change to send-side timing needs the L6 timer suite.

### 3.11 Benchmark contract with B1 (rev1; Opus M-11, GPT F11)

**The problem.** agentbox is a shared 32-vCPU VM with one disk. Its load average is already around 27–32 from oracles, implementers, fuzzing and soaks. NFR-PERF-02(a) assumes nothing else is running, and wall-clock gates at p < 0.05 with a 1% effect size are not credible there. Measured during rev1, three identical runs of one workload varied by about 30% in wall time and by **±0.04% in instructions retired**.

The evidence is therefore split into two kinds.

**1. Per-PR gate: deterministic counters** (PF0, agentbox, robust to shared load).
- **Counters**, all from the process under test:
  - user-space instructions retired (`perf stat -e instructions:u`; the PMU works in this VM once `kernel.perf_event_paranoid ≤ 2` is set);
  - heap allocation count and bytes (a counting global allocator in the bench build);
  - syscall count (`perf stat -e raw_syscalls:sys_enter` or `strace -c`);
  - read/write bytes and syscalls from `/proc/<pid>/io`;
  - page faults;
  - peak RSS for a fixed input.
- **Fallback.** Where PMU counters are unavailable, `valgrind --tool=cachegrind` (via `iai-callgrind` for microbenchmarks) supplies instruction counts. It is slower, so the workloads are sized for it.
- **Workloads:** fixed inputs, single-threaded where thread scheduling would perturb the counts (`-par=1`, a fixed `-dbcache`):
  - criterion and `iai` microbenchmarks per crate;
  - `reindex-chainstate` over fixed 2,000-block windows from local block files: pre-DIP3, DIP3–V19, post-V20, and mainnet tip − 50k;
  - a fixed mempool-accept stream;
  - GBT on a fixed mempool snapshot;
  - a fixed RPC batch;
  - a header-sync replay.
- **Rule.** Compare with the previous `node/integration` head on the same box. The noise floor of each counter and workload is measured from 5 repeated runs and stored with the baseline. A regression beyond max(noise floor, 0.5%) **blocks the merge** unless the manager approves an explicit justification recorded in STATUS (for example a correctness fix). Improvements are recorded too.
- **Against dashd.** The same counters are recorded for dashd 23.1.8 and 24.0.x on the same workloads at each milestone. Instructions are not time, but a ratio above 1 on a CPU-bound metric is a red flag that must be explained.
- Until rdashd runs end to end (≤ M2), only the microbenchmarks and `spv-headers-*` apply.

**2. Milestone and release evidence: wall-clock, side by side** (B1 `node-bench`).
- **Harness.** `node-bench` (Python; `node-bench/run`) runs dashd and rdashd with the same `NodeConfig`: `-dbcache`, `-par`, indexes, assumevalid, `-maxmempool`. The baselines are dashd **23.1.8** and **24.0.x**.
  - rdashd plugs in via `--rust-bin` and must accept the dashd-compatible flag subset documented in node-bench's README. `RustImpl.translate` handles anything mapped differently, and that list must stay empty.
- **Where the runs happen:** only in **reserved windows**. Every other agentbox job (oracle syncs, replays, fuzzing, soaks, builds) is paused and verified idle. NUMA node 0 is pinned. Or on a **dedicated benchmark host** once pasta provides one (§8 Q4; not blocking development).
- **Statistics:** NFR-PERF-02(g) (Mann-Whitney, p < 0.05, effect ≥ 1%) applies to these runs. Milestone exits and REL-6 cite them.
- Nightly and weekly long scenarios (`ibd-*`, `reindex-*`, `rescan-mainnet`) run in the windows. Results go to STATUS.md (REL-6.4).

---
## 4. Verification architecture

### 4.1 Principles

1. **Dash Core is the spec.** Every consensus rule, parser and observable behaviour is justified by Core code or Core output, never by documentation alone. The reference is the **pinned release per network** (DEC-05): mainnet v23.1.8, testnet/regtest/devnet v24.0.x. Deployment-gated rules come from `ChainParams`; every other release difference comes from `ReleaseProfile` (§2.25), and each is tested against the release it emulates. `develop` is watched weekly in a non-blocking lane.
2. **Differential by default.** Every layer has an oracle comparison. Hand-written expectations come only from Core-generated vectors. Each consensus function gets three independent kinds of evidence: fixed vectors, differential comparison on generated inputs, and real-chain replay (R4 §2.0). P2P and masternode behaviour also get mixed-network operation and fingerprint probes.
3. **The architecture serves testability.** Pure crates take explicit inputs. State-dependent validity is injected (`ChainHelper`). All time comes from `NodeClock`. Optimised paths have reference twins. Our DB holds the same *semantic* state as Core's (byte-diffable coins and undo; CanonicalMNListHash-comparable MN lists).
4. **Ratchets, not targets.** Pass lists (functional tests, fuzz targets, replay ranges, DIFF-RPC corpus) only grow. A regression blocks merging.
5. **The quirk register is normative.** Each of R4 §1.15's 34 entries, plus rev1's quirks 35–38 (§1.4.1), is a named test `quirk_NN_<slug>` in the crate that implements it. Quirk 1 is named `quirk_01_embargo_1` and loads private vectors (PLAN §7). Tests start as `#[ignore]` with an owner task and are un-ignored by that task. Every ★ PR lists the quirks it touches. Quirks found later by any layer are appended (FR-CON-34).

### 4.2 Layers (PRD/R4 numbering)

F and O each invented their own layer numbering (F L0–L7, O L1–L10). Both are dropped for the PRD's, which NFR-CEQ already cites. Their extra layers become the PRD's cross-cutting verification tags.

| Layer | Proves | Tooling | Runs | PRD |
|---|---|---|---|---|
| **L0** Vectors | pure rules equal Core's reference data | `test-vectors/core/<commit>/` + runners in each pure crate; oracle-generated vectors with `PROVENANCE.md` | every PR (seconds) | NFR-CEQ-01, -02 |
| **L1** Function-level differential | (a) both codecs and dashd agree on all real data; (b) every pure function equals Core's on arbitrary inputs | (a) BS2 `dash-conformance`; (b) `libdash_oracle` + `dash-oracle-sys` + `node-fuzz`; (c) **stateful**: O2b `diff_chain` (fuzzer-generated block sequences, failures and reorgs against a linked Core regtest chainstate, comparing verdicts, state and surviving effects after every step) | (a) per PR on regtest sets and a testnet sample, nightly full history; (b) per PR a 5-min smoke per touched target, nightly 8 h × 16 cores, release ≥ 24 CPU-h per parser target and ≥ 72 per consensus target | NFR-CEQ-03, -04 |
| **L2** Chain replay | full consensus parity at every block | `dash-conformance replay` + StateDigest v1 (`-statedigestlog` digests and RPC goldens from the **v24** oracle for both networks, sampled v23.1.8 cross-checks; §4.6) + decisions recorded on **live** oracles, `-assumevalid=0`, `-consensusxcheck` | per consensus PR: segment windows; nightly: full testnet; weekly and per release: full mainnet | NFR-CEQ-06, -13 |
| **L3** Rejection parity | identical verdict, reject string, result class and peer penalty for invalid inputs | `dash-conformance mutate` (~45 operators); GBT `proposal`, `testmempoolaccept`, regtest `submitblock` + P2P | per PR ≈ 500 mutations; nightly long run | NFR-CEQ-07 |
| **L4** Functional tests and node-local scenarios | user-visible behaviour, state-dependent validity | Dash's `test/functional` through `node/func-compat-v24.0` and `-v23.1` (the latter with `-releaseprofile=23.1`, regtest-only); NFR-CEQ-08 scenario scripts | per PR: the passlist subset for touched areas; nightly: the full passlist | NFR-CEQ-05, -08 |
| **L5** Mixed networks and soaks | Rust and dashd MNs share quorums, CLs, IS, CoinJoin, governance; live agreement | `dash-testkit::MixedNetwork`, DTF with per-node binaries, devnet compose, testnet, shadow mode | per PR small mixed set (from M5); nightly tier 2 mixed; standing devnet soak (M7); shadow from M4 | NFR-CEQ-09, -10; US-NODE-22 |
| **L6** Fingerprint | a probing peer cannot tell rdashd from dashd | `dash-conformance p2pdiff` + Python probes on `P2PInterface`/`v2_p2p.py`, KS timer tests, capture n-grams, a classifier | per P2P PR: the transcript diff; nightly: probes; release: the classifier | NFR-FP-01…04 |
| DIFF-RPC | JSON shapes and values equal dashd's | `dash-conformance rpcdiff`, corpus from `dash-rpc-spec`, `compat/ALLOWED-DIFFS.md` | per RPC PR: a subset; nightly: the corpus | FR-RPC-06, REL-5.6 |
| XCLI | CLI parity | rdash-cli and dash-cli × dashd and rdashd, byte-equal output | per CLI PR | FR-CLI-02, REL-3 |
| SPV | serving correctness for light clients | dash-spv `dashd_sync` + `dashd_masternode` with `DASHD_IMPL=rdashd`; a dashj-style BIP37 test | nightly (from M5) | REL-5.7 |
| BENCH | never slower; 2× where claimed | PF0 deterministic counters per PR; `node-bench` wall clock in reserved windows or on a dedicated host (§3.11) | per merged PR (counters); milestones and releases (wall clock) | NFR-PERF, REL-6 |
| Crash safety | process-crash and power-loss recovery without reindex (FR-STO-03) | ST3: `FaultyEngine`/`FaultyFlatFiles`, `dm-log-writes` prefix replay, `dm-flakey`, VM hard power-off, double crash; kill tests as smoke (§3.5.8) | per STATE PR: the model level; nightly: device level; weekly: VM | FR-STO-03, NFR-OPS-01 |
| Unit / property / model fuzz | local invariants | `cargo test`, proptest, `coins_cache_model`, `kv_model`, `mempool_model`, `mnlist_payee_queue`, `dkg_session` | every PR | — |

Why the crate layout makes this cheap:
- L0–L1 are cheap because the consensus surface is pure functions over base-sdk types.
- L2 works because our state is semantically Core's, and the digests are per block.
- L4 works because args, RPC, P2P and datadir layout are designed for drop-in parity.

### 4.3 L0: Core vectors

`contrib/node/sync-core-vectors.sh` copies files from a pinned Core commit into `test-vectors/core/<commit>/`, with a `PROVENANCE.md`; updating them is a PR. The harness ports Core's semantics (`ParseScript`, flag names, Dash's extra script-error names) **without copying Core's two harness bugs** (R4 §2.1). Core's release `test_dash` settles disputes.

| Source | Exercises | Runner |
|---|---|---|
| `script_tests.json` (1,282 tests, incl. the DIP0020 section) | `eval_script`/`verify_script`, flags, errors | `dash-consensus` |
| `sighash.json` (500) | legacy `signature_hash` (+ oracle-generated special-tx sighash cases, since the JSON lacks v3 txs) | `dash-consensus` |
| `tx_valid.json` / `tx_invalid.json` | `check_transaction` + script flags (`BADTX` → `check_transaction` failure) | `dash-consensus` |
| `trivially_valid.json` / `trivially_invalid.json` | special-tx trivial validity per type and variant, legacy and basic BLS, exact codes | `dash-evo` |
| `blockfilters.json` (X11 re-hashes) | Dash BIP158 | `dashd-indexes::filter` |
| `key_io_*.json`, `base58_encode_decode.json` | addresses and WIF | key-wallet, `dash-script` |
| `bip39_vectors.json` | mnemonic and seed (+ the Core checksum-bug acceptance test) | key-wallet |
| `proposals_{valid,invalid}.json` | proposal validator | `dash-governance` |
| `asmap.raw` | asmap | `dashd-p2p` |
| `rpc_getblockstats.json`, `rpc_psbt.json`, `rpc_decodescript.json`, `rpc_bip67.json` | RPC shapes | `dashd-rpc` |
| `blockheader_testnet3.hex` | header DoS tree (real testnet X11/DGW) | `dashd-p2p` |
| `quorum_proof.json` | DASHNC02 | `dashd-quorums` (P1) |
| `test/util/data/bitcoin-util-test.json` | dash-tx | `rdash-cli tx` |
| Embedded C++ vectors, transcribed to JSON by a one-time script in the Core branch: `pow_tests` (24 real DGW pairs, devnet rules), `subsidy_tests`, `block_reward_reallocation_tests`, `evo_simplifiedmns_tests`, `evo_islock_tests`, `quorum_proofs_tests`, `dip14_tests`, the versionbits threshold, `dip0020opcodes_tests`, `checkdatasig_tests`, `bls_tests` and dashbls `test.cpp` (Chia, legacy, threshold, IES) | respective rules | respective crates |
| rust-dashcore mnlistdiff/qrinfo fixtures (11 mainnet captures, `dashd_masternode` datasets) | SML, rotation | `dash-evo` — **after regeneration from the oracle** (R4 F12) |

Oracle-generated vectors (NFR-CEQ-02), per pinned release:
- every header's expected `nBits`;
- the `getblockstats` subsidy at every height;
- every special tx's payload and JSON; every CbTx; `protx diff` at checkpoints;
- generated vectors for special-tx validity per type and version, MN-list transitions, quorum members, commitment hashes, the credit pool, governance hashes and BLS encodings in both schemes.

### 4.4 L1: the oracle shim and differential fuzzing

- **Where.** Dash Core branch **`node/oracle-shim`** (one head per pinned release: `node/oracle-shim-v23.1`, `node/oracle-shim-v24.0`) in `~/workspace/dash`.
  - It adds `src/test/oracle/{oracle.h,oracle.cpp}` and a build target producing a **static `libdash_oracle.a`**, built against `depends` and bundling libsecp256k1, dashbls, the X11 SPH code and univalue.
  - Dash Core develop has both `CMakeLists.txt` and `configure.ac`; the build uses whichever the pinned release builds with.
  - It links Core's real implementation objects (`libbitcoin_consensus`, `libbitcoin_common`, evo, llmq, governance and coinjoin) and calls them directly. Nothing in Core's runtime changes.
  - **The shim contains reviewed adapter logic** (rev1, Opus M-7; v1 said "no logic").
    - Entry points such as `oracle_dml_apply`, `oracle_quorum_members`, `oracle_credit_pool_step`, `oracle_versionbits_state` and the contextual checks need chainstate-resident objects: `CBlockIndex` chains with versionbits caches, `CDeterministicMNManager` over `CEvoDB`, `CQuorumSnapshotManager` (which writes raw evodb), the chain helper, and the global `Params()`. The shim builds those from synthetic inputs.
    - Core has no standalone P2P decoder: each `ProcessMessage` branch is its own reader. `oracle_p2p_decode` therefore reproduces each handler's reads, with optional `version` fields and trailing bytes.
    - All of this is CON-reviewed code with its own tests. Every adapter is checked against the matching RPC or functional-test behaviour of the stock binary on sampled inputs.
  - `libdashconsensus` (deprecated: `verify_script` with five flags, no `ScriptError`) is only a week-1 stopgap from the rc.3 tarball.
  - The functional framework's `messages.py` is a third implementation for codec disputes.
- **C ABI** (buffers in and out, deterministic, no globals beyond an explicit `oracle_set_network(net, params_json)`). O1 is v1 and O2a is v2:

```c
/* v1 (O1): codecs, checks, script, sighash, coins, PoW, subsidy */
int  oracle_tx_decode(const uint8_t*, size_t, uint8_t* reenc, size_t* n, uint8_t txid[32], uint8_t instance[32]);  // 0 ok, else Core exception class
int  oracle_block_decode(const uint8_t*, size_t, uint8_t hash[32], uint8_t merkle[32], int* mutated, uint8_t* reenc, size_t*);
int  oracle_header_decode(const uint8_t*, size_t, uint8_t hash[32]);
int  oracle_check_transaction(const uint8_t*, size_t, char reason[64]);
int  oracle_check_block(const uint8_t*, size_t, int check_pow, int check_merkle, char reason[64]);
int  oracle_verify_script(const uint8_t* spk, size_t, const uint8_t* tx, size_t, uint32_t n_in, int64_t amount, uint32_t flags, int* script_err);
int  oracle_signature_hash(const uint8_t* script, size_t, const uint8_t* tx, size_t, uint32_t n_in, int32_t hashtype, uint8_t out[32]);
int  oracle_coin_codec(int op, const uint8_t*, size_t, uint8_t*, size_t*);      // VARINT, CompactSize, Coin, CompressAmount, ScriptCompression, undo
int  oracle_next_work_required(const uint8_t* headers, size_t n_headers, const uint8_t new_header[80], uint32_t* bits);  // DGW/KGW/BTC/min-diff
int64_t oracle_block_subsidy_inner(uint32_t prev_bits, int32_t prev_height, int v20);
int64_t oracle_superblock_subsidy_inner(uint32_t prev_bits, int32_t prev_height, int v20);
int64_t oracle_mn_payment(int32_t height, int64_t block_value, int era);
int  oracle_chainparams_json(char* out, size_t* n);                              // BS5 equality corpus
/* v2 (O2a): Dash layers; rev1 additions are marked (F15, B-1) */
int  oracle_payload_decode(uint16_t type, const uint8_t*, size_t, int* consumed_all, char trivial_reason[64], uint8_t* reenc, size_t*);
int  oracle_p2p_decode(const char* cmd, const uint8_t*, size_t, int32_t peer_version, uint8_t* reenc, size_t*);
int  oracle_headers2_decode(const uint8_t*, size_t, uint8_t* headers80xN, size_t* n);
int  oracle_bls(int op, int legacy, const uint8_t* in, size_t, uint8_t* out, size_t*);  // (de)serialize incl. malleability, sign, verify, aggregate(_secure), threshold split/recover, IES, hash-to-G2
int  oracle_gov_hashes(const uint8_t* obj_or_vote, size_t, int kind, uint8_t hash[32], uint8_t sighash[32]);
int  oracle_validate_proposal(const char* hex, char err[512]);
int  oracle_sml_entry_hash(const uint8_t* dmn, size_t, uint8_t out[32]);
int  oracle_quorums_root(const uint8_t* commitments, size_t, uint8_t out[32]);
int  oracle_quorum_members(const uint8_t* mn_list, size_t, uint8_t llmq_type, const uint8_t* modifier_inputs, size_t, uint8_t* out, size_t*);  // incl. rotation + snapshots
int  oracle_select_quorum(const uint8_t* quorums, size_t, const uint8_t sel[32], int rotated, uint8_t out[32]);
int  oracle_sign_hash(uint8_t llmq_type, const uint8_t quorum[32], const uint8_t id[32], const uint8_t msg[32], uint8_t out[32]);
int  oracle_request_id(int kind, const uint8_t* payload, size_t, uint8_t out[32]);
int  oracle_credit_pool_step(const uint8_t* prev, size_t, const uint8_t* block, size_t, int era, uint8_t* out, size_t*);
int  oracle_versionbits_state(const uint8_t* params, size_t, const uint8_t* period_ends, size_t, const uint8_t* signals, size_t, int* state);
int  oracle_dml_apply(const uint8_t* prev_list, size_t, const uint8_t* block, size_t, const uint8_t* prev_header, const uint8_t* coins, size_t,
                      const uint8_t* deployment_flags, size_t, uint8_t* new_list_sorted, size_t*, uint8_t payee[32], char err[64]); // BuildNewListFromBlock over a synthetic CCoinsView
int  oracle_coinjoin_valid_in_outs(const uint8_t* entries, size_t, int kind, char msg[64]);
int  oracle_bip158_filter(const uint8_t* block, size_t, const uint8_t* undo_scripts, size_t, uint8_t* out, size_t*);
int  oracle_bip39(int op, const char* mnemonic, const char* passphrase, uint8_t* out, size_t*);
/* rev1 additions (O2a): contextual entry points over explicit synthetic contexts, with full result classes */
typedef struct { int32_t cls; /* TxValidationResult | BlockValidationResult */ char reason[80]; char debug[256]; int32_t dos; } oracle_result;
int  oracle_bls_decode(int group, int kind /* pk|sig|sk */, int legacy, const uint8_t*, size_t, int* outcome /* VALID|INVALID|THROW */, uint8_t reser[96]); // §1.4.1
int  oracle_bls_verify(int legacy, const uint8_t* pk, const uint8_t* sig, const uint8_t msg[32], int* ok);
int  oracle_check_special_tx(const uint8_t* tx, size_t, const uint8_t* ctx, size_t /* list, coins, ancestry, deployments, BlsCtx, check_sigs */, oracle_result*); // CheckSpecialTxInner
int  oracle_verify_commitment(const uint8_t* qc, size_t, const uint8_t* ctx, size_t, oracle_result*);
int  oracle_check_tx_inputs(const uint8_t* tx, size_t, const uint8_t* coins, size_t, int32_t spend_height, int64_t* fee, oracle_result*);
int  oracle_contextual_check_tx(const uint8_t* tx, size_t, const uint8_t* ctx, size_t, oracle_result*);
int  oracle_contextual_check_header(const uint8_t hdr[80], const uint8_t* ancestry, size_t, int64_t adjusted_time, oracle_result*);
int  oracle_contextual_check_block(const uint8_t* blk, size_t, const uint8_t* ctx, size_t, oracle_result*);
int  oracle_sequence_locks(const uint8_t* tx, size_t, const uint8_t* prev_heights_mtps, size_t, int flags, int64_t* min_height, int64_t* min_time);
int  oracle_sigops(const uint8_t* tx, size_t, const uint8_t* coins, size_t, int flags, uint32_t* legacy, uint32_t* p2sh);
int  oracle_block_value_payee(const uint8_t* cb, size_t, const uint8_t* ctx, size_t /* list, superblock state, check mode */, oracle_result*);
/* O2b: diff_chain, a stateful oracle */
void* oracle_chain_new(const char* params_json);                                       // in-memory DBs, mocktime, low activation heights
int  oracle_chain_step(void*, int op /* connect|test_validity|disconnect|invalidate|reconsider|restart|set_cl|set_islock */, const uint8_t*, size_t, oracle_result*, uint8_t* digest_jsonl, size_t*);
void oracle_chain_free(void*);
```

**Coverage matrix** (rev1, GPT F15). Before any implementation gate depends on the shim, O2a publishes `node-program/compat/COVERAGE.md`. It is a table mapping each Rust consensus function or state transition to its pinned Core entry point, vector source, generator/fuzzer, replay window and scenario. A row may say "none" only with a reason the manager accepts. EV1's "special-tx checks vs oracle" is backed by `oracle_check_special_tx`, not by `oracle_dml_apply`, which only covers `BuildNewListFromBlock`. Every entry point returns the full result class (`oracle_result`), not only a string.

**`diff_chain`** (O2b; Opus M-7, GPT F15, F04). This is the stateful differential fuzzer for the XL areas: the DML, PoSe, rotation, credit pool, MNHF, IS and CL interplay, and reorgs.
- **Core side:** a Core regtest chainstate linked into the shim, with in-memory DBs, mocktime and compressed activation heights.
- **Rust side:** `dashd-chainstate` on the in-memory engine.
- **Input:** both are fed the same fuzzer-generated operation sequences. These cover ProTx churn and collateral spends, QcTx with valid and invalid members, asset lock and unlock, MNHF, ChainLocks and IS locks, invalid blocks, proposals, `invalidate`/`reconsider`, restarts and reorgs.
- **Compared after every step, rejected steps included:**
  - the verdict, its class and string;
  - the per-block StateDigest;
  - the surviving effects: snapshot rows, IS DB, mineable pool and mncache (§2.10.1).
- **Budget:** ≥ 72 CPU-h per release.
- **Status:** the model-based targets below remain an *additional* layer, not a substitute.

**Rust side and uses.** `dash-oracle-sys` builds the `.a` with `contrib/node/build-oracle.sh` (pinned dash commit; cached under `/work/cache/oracle/<commit>`). It has three uses:
1. **Property tests** (proptest) on every PR: random transactions, scripts, headers and lists through both implementations, which must be equal.
2. **Differential fuzzing** (libFuzzer in `node-fuzz`). Each target compares the accept/reject class, Core's reason string, the re-encoding and the outputs:

| Target(s) | Inputs | Compares |
|---|---|---|
| `diff_tx_decode`, `diff_block_decode`, `diff_header_decode` | raw bytes; corpus = all mainnet and testnet txs and blocks (BS2) | decode success, re-encoding, txid, instance hash, block hash, merkle + mutated |
| `diff_payload_<type>` (13 types × versions, incl. 10–12) | payload bytes + tx context | decode, consumed-all, `IsTriviallyValid` reason |
| `diff_check_transaction`, `diff_check_block` | tx / block bytes | reject reason |
| `diff_script_verify`, `diff_sighash` | (scriptSig, scriptPubKey, tx, nIn, flags) / (script, tx incl. special types, nIn, hashtype) | result and `ScriptError` / 32-byte hash |
| `diff_p2p_<cmd>` (74 commands × peer-version buckets), `diff_headers2`, `diff_v1_framing` | payload bytes | decode and re-encode (Rule DDE) |
| `diff_codec_primitives` | bytes | VARINT, CompactSize, Coin, CompressAmount, ScriptCompression, undo records, `DiskBlockIndex` (import), `txundo` |
| `diff_next_work` | synthetic header windows around the KGW/DGW boundaries and the min-difficulty rules | nBits |
| `diff_subsidy_payments` | (prev_bits, prev_height, flags, era, fees) | subsidy, MN payment, platform share, treasury |
| `diff_versionbits`, `diff_credit_pool`, `diff_dml_apply`, `diff_sml`, `diff_quorum_members` (incl. rotation), `diff_quorums_root`, `diff_select_quorum` | structured inputs | outputs and errors |
| `diff_bls_decode`, `diff_bls_verify`, `diff_bls_*` (legacy and basic) | random bytes and keys, seeded with every class of §1.4.1's outcome table | the decode **outcome** (VALID / INVALID / error) and re-serialized bytes, `Exotic` verification, secure aggregation, threshold recovery, IES, hash-to-G2 |
| `diff_mnauth`, `diff_p2p_qfcommit` | MNAUTH and `qfcommit` payloads with per-field BLS seeds | the handler outcome, the sign hash, the inv hash over the re-serialization |
| `diff_chain` (O2b) | fuzzer-generated operation sequences (connect, proposal, disconnect, invalidate, reconsider, restart, CL/IS arrival) | per-step verdict and class, StateDigest, surviving effects |
| `diff_gov`, `diff_gov_vote`, `diff_isdlock`, `diff_clsig`, `diff_dsq`, `diff_coinjoin` | objects, votes, locks, CoinJoin entries | hashes, sighashes, validation errors, trigger JSON bytes, `IsValidInOuts` classes |
| `diff_bip158`, `diff_bip39` | blocks + undo scripts / mnemonics | filter bytes / seed, checksum |

Model-based stateful targets need no oracle: `coins_cache_model` (layered cache + freeze/flush + crash points vs a `BTreeMap`, like Core's `coinscache_sim`), `kv_model`, `mempool_model`, `mnlist_payee_queue` (queue vs linear scan), `dkg_session` (3–20 members with adversarial drops and lies; honest members agree).

3. **Vector generation** for base-sdk corpora (BS5's chainparams dump, BS6a/b's synthetic v24 txs, BS12's DKG transcripts and legacy BLS vectors), each with documented provenance.

**Corpora and triage.** Seeds come from BS2 (all historical txs and blocks), from `-capturemessages` captures of dashd peers on mainnet and testnet, from functional-test traffic, from Core's vectors and from `bitcoin-core/qa-assets`. Every divergence becomes a minimized regression vector plus a STATUS.md task. Corpus entries under PLAN §7 handling live outside git and are never exported.

**Where the shim is not enough.** Behaviour that needs a running node (P2P scheduling, timers, MN networking) uses the RPC and process oracles (§4.6–§4.10). With O2b, `CChainState` behaviour is fuzzed in-process, not only replayed.

### 4.5 Floating point (FR-CON-15, NFR-PORT-02; rev1, GPT F19, Opus m-8)

**Domain.** Core's *consensus* code computes in IEEE binary64 in exactly two places:
- **The pre-V20 subsidy.**
  - The formulas are `1111/((dDiff+1)^2)`, `11111/(((dDiff+51)/6)^2)` and `2222222/(((dDiff+2600)/9)^2)`, computed via `pow(x, 2.0)`, then truncation of `nSubsidyBase·COIN`.
  - From V20, and on devnets from `nHighSubsidyBlocks`, the base is a fixed 5 (`core: validation.cpp:1823-1849`).
  - On mainnet and testnet, **no future block evaluates `pow`**. The live domain is the finite pre-V20 history, plus regtest and devnet chains before V20, where the clamps make most results exact.
- **KGW and the `ConvertBitsToDouble` tolerance.** KGW uses `pow(x, -1.228)`. Both apply only on mainnet heights ≤ 68589, under the ±50% check (R4 F4). KGW and BTC retargets are never exact-checked on any network.

**Step 1: decide the reference from the binaries, not from glibc** (rev1).
- CS2's `fp` design starts by disassembling `GetBlockSubsidyInner` and the KGW code in the **official release binaries** of v23.1.8 and v24.0.0-rc.3 for x86_64 and aarch64. That shows whether the compiler folded `pow(x, 2.0)` into `x*x`; GCC does this at `-O2` because the result is exact.
- If the release binaries compute `x*x`, the subsidy reference **is** `x*x`, a single correctly rounded multiplication with no libm dependency. That is then the implementation, and v1's port of the glibc `pow` routine is not needed for the subsidy.
- A libm port is kept only for call sites the binaries actually route to libm: KGW's `pow(x, -1.228)`. It stays under the ±50% tolerance, so only results right at the tolerance boundary could differ.

**Step 2: the admissible domain decides, not history alone.**
- The gate compares the implementation with the reference on the **admissible domain**: every compact target and configuration a valid supported chain can reach. That covers mainnet and testnet history, plus pre-V20 regtest and devnet chains under every override (`-highsubsidyblocks`, `-highsubsidyfactor`, `-powtargetspacing`, `-minimumdifficultyblocks`).
- A mismatch on any admissible input **fails the gate**, even if no recorded chain contains it.
- Only inputs proven unreachable or invalid may be excluded, and each exclusion is written up with its proof. v1's "decided case by case" is withdrawn.

**Verification:**
- **Exhaustive historical sweep:** the subsidy equals the oracle's `getblockstats subsidy` at **every** mainnet and testnet height; it matters because mainnet replay only checks the upper bound, so a subsidy that is too high would pass unnoticed. Also every KGW evaluation (mainnet 15200–34140) and every header's `nBits`.
- The admissible regtest and devnet domain, enumerated: `bits = 0x207fffff` and every reachable `dDiff` under the overrides.
- For any call site that keeps a libm port: 10^8 random inputs against glibc on x86_64 (FMA and non-FMA ifuncs) and aarch64, and against the release binary's own routine.

**Lint scope (m-8).**
- `f64` is banned in the consensus crates outside `dash_consensus::fp`, and **libm calls are banned everywhere outside `fp`**.
- Plain IEEE arithmetic is allowlisted at anchored, Core-mirrored sites that must match exactly:
  - the governance rate check;
  - the superblock time estimate (`·2.62·60`);
  - the DKG sleep factor;
  - the fee estimator;
  - CoinJoin fees;
  - RPC `difficulty` and `verificationprogress`.
- DIFF-RPC float text follows UniValue's formatting, not serde_json's shortest representation. O14 confirms the exact rule from the pinned `univalue` source and pins it with a test.

### 4.6 L2: chain replay and StateDigest v1

**Inputs and mechanics.**
- Blocks come from the BS2 oracle's `blk*.dat` (read-only), or from a fork-free `bootstrap.dat` made with `contrib/linearize`.
- rdashd imports them through the `-reindex`-equivalent path with **`-assumevalid=0`** (R4 F2: below assumevalid Core also skips CbTx, merkle-root, credit-pool, commitment and ProTx-signature checks), checkpoints on, `-par` at maximum and no P2P. This isolates consensus from networking.
- **The chain checks itself:** from DIP3 the node recomputes `merkleRootMNList` every block; from 1088641 `merkleRootQuorums`; from V20 `bestCL` and `creditPoolBalance`; from V24 `merkleRootAssetUnlocks`.
- Non-SML state (`nLastPaidHeight`, PoSe counters, `nConsecutivePayments`, …) is caught only indirectly that way, possibly much later. Hence the explicit digests.

**StateDigest v1** is one JSONL record per block, written by rdashd's `-statedigestlog` and compared with golden files extracted from the oracle.

*RPC golden extraction* (stock binaries; R4 §2.3.2). Rev1 (Opus M-8, X-2): goldens for **both networks come from the v24 oracle**, because v23.1.8 lacks `getdeploymentinfo`, `getcreditpoolinfo`, `getchainlockbyheight` and `CanonicalMNListHash`. §2.25 explains why that is valid for mainnet and how v23.1.8 parity is then proven. The extraction is **digest-first**: the per-block patch digests below catch any DMN divergence, so the expensive per-block DMN RPCs are taken sparsely and expanded only around a first mismatch:

| Field(s) | dashd RPC | From (mainnet) | Cadence |
|---|---|---|---|
| hash, prev, height, header fields | `getblockheader` | 0 | every block |
| UTXO `muhash`, `txouts`, `total_amount`, `total_unspendable_amount`, `block_info{…}` | `gettxoutsetinfo muhash <h>` (coinstatsindex) | 0 | every block |
| subsidy, totalfee, txs, ins, outs, utxo_increase, utxo_size_inc | `getblockstats <h>` | 1 | every block |
| expected MN payee outputs and amounts | `masternode payments <hash>` | 1028160 | every 576 + on demand around a mismatch (X-2) |
| full DMN state delta | `protx listdiff <h-1> <h>` | 1028160 | every 576 + on demand around a mismatch (X-2) |
| full DMN list | `protx list registered true <h>` | 1028160 | every 576 |
| CbTx fields | `getblock <hash> 1` → `cbTx` | 1028160 | every block |
| credit pool | `getcreditpoolinfo <h>` | 1987776 | every block |
| active quorum sets | `quorum listextended <h>` | 1028160 | every block inside mining windows, else every 24 |
| quorum members and valid flags | `quorum info <type> <hash>` | 1028160 | per mined quorum |
| rotation snapshots and quarter members | `quorum rotationinfo <hash>` | 1737792 | per 288-block cycle base |
| deployment states and EHF signal heights | `getdeploymentinfo <hash>` | 2203488 for V24/`evo_shares` | every 576 + window boundaries (deployment **answers**, never cache contents; F16) |
| coinbase CL | `getchainlockbyheight <h>` | 1987776 | every block |
| asset-unlock statuses | `getassetunlockstatuses [idx…] <h>` | 2128896 | per block with unlocks |
| raw block bytes | `getblock <hash> 0` | 0 | every block |

*Test-only Core patch* (branch **`node/state-digest-v24.0` only**; v1's `-v23.1` backport of `evo/snapshot.*` is dropped, M-8; R4 V4; about 150 LOC behind hidden flags):
- **Field classes** (rev1, GPT F16). Every digest field is one of three kinds, and the comparator treats each differently:
  - **semantic state:** must be equal, record for record;
  - **history and effect observations** (snapshot rows written, effects applied): must be equal for the same operation sequence;
  - **schedule-dependent** (cache occupancy): never compared.

  v1's "versionbits cache states" is replaced by deployment **answers** at fixed heights.
- **`-statedigestlog=<file>`** writes, after each successful `ConnectBlock` (outside `fJustCheck`), the derived state that has no RPC (semantic unless marked):
  - `CanonicalMNListHash(list)` (`core: evo/snapshot.cpp:150-155`, every DMN field);
  - the canonical hash of mined-commitment index entries;
  - `llmq_S` snapshot hashes;
  - the EHF signals map; credit-pool state (locked, current_limit, lately_unlocked, SHA256d of the index range set);
  - deployment answers for every deployment at this block; the BLS legacy flag; the coinstats MuHash;
  - **`undo_hash` = SHA256d(serialized `CBlockUndo`)**, for byte-level undo parity;
  - *(observation)* the effects of this connect: snapshot rows written, IS locks removed, mineable commitments erased, platform bans reset (§2.10.1).

  `DisconnectBlock` appends `D <hash>`. Every **failed or `fJustCheck` attempt** appends `R <hash> <class> <reason>` with the effects it left behind, so failed attempts are compared too (F04, F15). rdashd emits the identical record, so the comparison is `cmp`, and the patched oracle produces full-state digests during its own `-reindex-chainstate` with no RPC cost.
- **`-recordstatedecisions=<file>`** logs, per connected **or attempted** block: the best CL (height and hash) at connect time; the IS conflicts encountered and locks removed; the `check_superblock` mode; and the triggered superblock payment list. `ReplayChainHelper` replays these.
  - **Recording happens on the live oracles** (rev1, Opus m-12). Decisions recorded during an oracle's own `-reindex-chainstate` are all IBD / NoCheck / no-CL and test nothing; O8 therefore records on the synced, live v24 oracles from the day it lands.
  - Live behaviour is covered by L4/L5 and shadow mode.

At the tip, or at heights chosen with `-stopatheight`, the replay also compares: `gettxoutsetinfo hash_serialized_2` (reproducing the quirk), a full `dumptxoutset` set-diff, `getchaintips`, and `verifychain 4 0`.

**Replay tool** (`dash-conformance replay --network main --from H1 --to H2 --digest <golden>`):
1. Start from a validated snapshot at H1, or from genesis.
2. Feed blocks from the oracle's files through `dashd-chainstate`.
3. Stream the comparison.
4. At the first mismatch, dump both sides' detail at that height (`protx list`, `quorum list`, the UTXO delta) and the diverging field.
5. `-consensusxcheck` is on for milestone tags.

v1's in-node `-shadowcompare=<rpc-url>` hook is cut (Opus X-6). The external comparator (O15) consumes rdashd's `-statedigestlog` and the oracle's RPC, and gives the same first-divergence dump.

**Segment snapshots** (fast PR gating; R4 §2.3.4). Rust state is snapshotted once at:
- mainnet 68589 (DGW exactness), 328008 (treasury), 614820 (superblocks), 1028159 (DIP3), 1047199 (DIP3 enforcement), 1088639 (DIP8), 1374911 (BRR), 1737791 (DIP24), 1899071 (V19), 1987775 (V20), 2128895 (MN_RR), 2201471 (WITHDRAWALS), and tip − 50k;
- testnet: the equivalents, plus around V24 activation.

Each snapshot is validated against the oracle at its height (muhash, `protx list`, `quorum listextended`, `rotationinfo`, credit pool). Each consensus PR then replays the windows its crates touch: 20k blocks per boundary window, covering at least 3 DKG cycles of every LLMQ type. Minutes, not hours.

**Chain scans** (once per pin; R4 §2.3.5; NFR-CEQ-13). These confirm which quirks history actually exercises, so they get tested hardest:
- (a) txs with `nVersion > 3 && nType ≠ 0`;
- (b) pre-DIP3 txs with non-zero type bytes;
- (c) coinbases above the bound (the budget-window blocks);
- (d) `bestCLHeightDiff ≥ height` or odd CbTx encodings;
- (e) MNHF signals by bit;
- (f) commitments of disabled types after their disable height;
- (g) the EMBARGO-1 scan (criteria and results private; PLAN §7);
- (h) historical blocks whose version-derived legacy BLS fields decode as `Exotic` (§1.4.1). Evidence only: parity rests on BS11a's engine, not on the count (BS11a D21).

**Cost on agentbox** (32 vCPU, 160 GB):
- Mainnet golden extraction is about 9M RPC calls, ≈ 1–4 h with batched JSON-RPC over 32 `rpcthreads`, ≈ 1 GB of zstd JSONL. Testnet takes ≈ 0.5–1.5 h.
- A Rust testnet full replay takes ≈ 1–3 h.
- **Mainnet replay target (m-2):** ≤ dashd's **`-reindex -assumevalid=0`**, not `-reindex-chainstate`. `-reindex-chainstate` skips `ContextualCheckBlock*` (`validation.cpp:4522-4527`; R4 §2.3.3), so comparing against it would compare unequal work. CH5 mirrors that skip exactly in rdashd's own `-reindex-chainstate`, for parity.
- Oracle ≈ 12 GB dbcache, 100–150 GB disk; replay ≈ 8–16 vCPU, 16–24 GB RAM, 20–40 GB disk.
- **Never overlap with wall-clock benchmark windows** (§3.11; SW-OPS capacity table, PLAN §5).

**Mainnet reference** (rev1, M-8, F09). Both BS2 oracles run **v24.0.0-rc.3**, including mainnet, and produce the consensus goldens for both networks. O7 still provisions a **v23.1.8 mainnet reference**, a second synced datadir with every index, for everything that must come from the release itself:
- the sampled cross-checks of §2.25;
- L3 proposal-mode rejection parity for the profile-keyed rows;
- the mainnet P2P fingerprint baseline (O13a–c);
- the O15 shadow peer;
- the v23.1.8 RPC shapes for DIFF-RPC.

It is built from the oracle's `blk` files under the link-count rule (§2.11): sealed files are hardlinked, the writable tails are copied or reflinked, and the v23.1.8 dashd writes only to its own new files. It is then rebuilt with `-reindex-chainstate`. The BS2 oracle's files are hashed before and after, and must be unchanged. v1's plain hardlinking would have let the reference's dashd truncate and append to the oracle's own `blk`/`rev` tails. B1 baselines dashd 23.1.8 and 24.0.x.

### 4.7 L3: rejection parity by mutation

Replay proves acceptance only. **Oracles:**
- `getblocktemplate {"mode":"proposal","data":<hex>}`: `TestBlockValidity(fCheckPOW=false)` on any block built on the tip; it returns `null` or the BIP22 reject string (R4 F9, `core: rpc/mining.cpp:731-759`).
  - It is **not** side-effect free (rev1, GPT F04): at a rotation cycle-base height it writes an `llmq_S` row keyed by the proposal's hash, outside the EvoDB transaction (§2.10.1). The values are deterministic, so against the **live** BS2 oracles that is a harmless DB leak, and L3 uses them for non-cycle-base heights.
  - Operators at cycle-base heights, and every failure-sequence experiment, run on **disposable** oracle datadirs.
  - Rows 3–5 and 24 of §2.25 also run against the **live v23.1.8 mainnet reference**, for mainnet parity.
- On regtest (trivial PoW): `submitblock`, plus P2P submission through `P2PInterface`, comparing misbehaviour and disconnect effects.
- `testmempoolaccept` and `sendrawtransaction` reject strings.
- Headless alternative: the experimental `dash-chainstate` binary.

**Engine.** About 45 operators (rev1 adds the BLS-encoding and per-profile operators) over real templates (`getblocktemplate`, or `generateblock submit=false` on regtest):

| Target | Mutations |
|---|---|
| Coinbase | value ±1 duff; drop, move or alter the MN payee, operator or platform OP_RETURN output |
| CbTx | each field and version; `bestCLHeightDiff ±1`; CL sig bit flips, all-zero, and opposite-scheme encodings |
| QcTx | insert, remove or duplicate; bitset flips and padding bits; version vs base; null vs real outside the window; **BLS encodings** of `quorumPublicKey`/`quorumSig`/`membersSig` in null and non-null commitments: identity, opposite-scheme, top-bit classes, off-curve legacy (§1.4.1, B-1) |
| ProTx (regtest, re-signed with known keys) | every field; uniqueness collisions inside one block; same-block collateral |
| Asset lock/unlock | fee, index reuse, height-window edges, output limits, credit-pool balance; `quorumSig` BLS encodings; AU v2 under each profile (stable txid, reject string and class; §2.25 row 5) |
| ProUpServ / ProUpRev | `sig` BLS encodings for v1 (legacy, `Exotic` cases → `bad-protx-sig`) and v2+ |
| MNHF | duplicate bit, unknown bit, wrong quorum |
| Locktime | `nLockTime`/`nSequence` at the MTP and height boundaries |
| Limits | sigops at the limit and limit+1; sizes of exactly 100,000, 1,000,000 and 2,000,000 bytes |
| Merkle | the duplicate-tx mutation (CVE-2012-2459) |
| Header | nBits ±1; time = MTP and MTP+1; now+7200 and +7201 (regtest mocktime) |
| Tx version/type | version/type combinations |
| Private | one operator under PLAN §7 handling (corpus outside git) |

**Workload and pass criterion.**
- On regtest, compress the activation heights (`-testactivationheight`, `-dip3params`, `-vbparams`); run MiniWallet and DTF-style ProTx/QC workloads; **mine alternately with dashd and rdashd** (each GBT block submitted to the other) and compare StateDigest per block over 10^5 blocks. `regtest-mn` (4 MNs, 8 DKG cycles) and `regtest-40000` are the starting chains.
- Every `bad-*` string in Core's source has at least one case (a census script). Multiple-failure blocks (two independent defects) pin Core's precedence, including script failures under `-par=1` vs parallel (§3.3).
- **Pass:** 100% identical verdict, reject string, result class and peer penalty, plus identical surviving effects for failure sequences (O2b's `diff_chain` covers the generated cases; L3 covers real templates).

### 4.8 L4: Dash's functional tests against rdashd

**Mechanics.**
- `DASHD=/path/rdashd` plus the framework's `config.ini`. The framework already supports per-node binaries (`add_nodes(binary=[...])`, R4 F10).
- Dash Core branches **`node/func-compat-v24.0`** and **`node/func-compat-v23.1`** (one per pinned release; the v23.1 tree runs against rdashd with `-releaseprofile=23.1` on regtest, §2.25) add only:
  - a runner flag `--rust-nodes=<mask>` / `RDASHD_NODES` that maps node indexes to rdashd. This is **not only a runner flag** (Opus m-5):
    - DTF never passes per-node binaries (`start_masternodes` `test_framework.py:1786`, `add_dynamically_node` `:598-599`, `create_simple_node`), so those helpers are patched too;
    - BitcoinTestFramework builds its chain cache with one binary and copies it to all nodes (`:943-1022`), so with mixed nodes the cache is built with dashd, and rdashd auto-imports it (§2.9);
  - a tracked `rust_expected_failures.txt` (test, reason, owner);
  - the passlist ratchet `node-program/compat/functional-passlist.txt`;
  - FT-MAP generation (test → story IDs; PRD §0.4).
- Tests stay unmodified. The exceptions are a short, reviewed list of necessary adaptations (`compat/functional-adaptations.md`), for example a Core-specific `debug.log` line we could not reasonably reproduce.

**What rdashd must satisfy:**
- `regtest=1` in the config file; the option set `TestNode` passes (§2.18);
- cookie auth at `<datadir>/regtest/.cookie`; `-28` during warmup; `getmempoolinfo.loaded`; `pong` bytes ≥ 29 in `getpeerinfo.bytesrecv_per_msg`;
- `stop` with exit code 0 and **empty stderr**;
- the `assert_debug_log` lines (§2.18);
- the datadir layout, Core-datadir auto-import and live-copy safety (§2.9);
- the hidden and test RPC set (§2.17);
- `dash_hash` X11 on the Python side (already a framework dependency).

**Census (R4 §2.5).** 264 scripts use 209 RPC methods. 151 need no node wallet (many use MiniWallet); 83 need it; the 34 `DashTestFramework` (DTF) tests need a wallet only on node 0, because masternodes force `-disablewallet`.

**Tiers:**
- **Tier 1** (35 tests, about 60 RPCs) gates the validating-node milestones M3/M4.
- **Tier 2** (29 DTF tests, run as mixed networks) gates M7.
- **Tier 3** (fingerprint and operations) gates M5.
- **Tier 4** (58 wallet tests) gates M6.
- The release requires tiers 1–4 complete, i.e. all ~56 Dash-specific tests and every Bitcoin-heritage test except pasta's exclusion list (Qt, USDT, multiprocess, BDB internals; DEC-10). The expected-failures file must be empty (NFR-CEQ-05).

**Phased passlist** (Appendix D lists the tests):

| Phase | Configuration | Content |
|---|---|---|
| M3–M4 | all nodes rdashd, no wallet | tier 1 |
| M5 | services | `mempool_*`, `p2p_*`, `rpc_*` (non-wallet), indexes, `interface_zmq*`, `interface_rest`, tier 3 |
| **Mixed mode A** (M5) | node 0 and MNs = dashd; **non-MN nodes = rdashd** | rdashd follows quorums, CL/IS, governance and sporks as a regular node inside every DTF test |
| M6 | rdashd node 0 (wallet) | `wallet_*` and the wallet-dependent Dash tests |
| **Mixed mode B** (M7) | **node 0 = dashd** (wallet funder, `sporkkey`, miner); **masternodes = rdashd** | DTF creates MNs through node 0's wallet RPCs and copies node 0's datadir to the MN nodes. rdashd's regtest Core-datadir auto-import (§2.9) handles that cheaply thanks to the byte-identical block files. All 34 DTF tests become **mixed-quorum interop tests**, which is exactly the charter's "mixed regtest networks where Rust and dashd masternodes share quorums" |
| M7 | MN parity | every `feature_llmq_*`, `feature_dip3_*`, `feature_masternode_*`, `p2p_instantsend`, `p2p_quorum_data`, `feature_mnehf`, `feature_asset_locks`, `rpc_quorum`, `rpc_mnauth` in three configurations: all-dashd baseline, all-rdashd, and alternating mixed |

**Node-local scenarios (NFR-CEQ-08)**, never provable by replay, are scripted on regtest and compared by DIFF-STATE:
- ChainLock conflicts;
- IS conflicts inside and outside IBD;
- superblocks with governance synced and unsynced;
- the assumevalid boundary;
- reorgs across V19 (the BLS scheme) **with masternode traffic**: clsig, isdlock, recsig and DKG messages decoding across the switch (§1.4.1, Opus M-3); V24 activation; DIP24 cycle bases (snapshot effects);
- failure sequences: a ChainLocked block whose IS-conflict removal survives a later payee failure; a GBT proposal at a cycle base; a block that fails after `ProcessCommitment` (the miner's next template then carries a null commitment) (§2.10.1);
- the v23.1.8 profile's IS gate (SPORK_3 and blockchain-synced) against v24's `!IBD` gate.

### 4.9 L5: mixed networks, soaks and shadow mode

- **Stage A, regtest** (DTF + `dash-testkit::MixedNetwork`; R4 V8). Alternating binaries: 5 MNs → 3 Rust + 2 dashd; rotation (8 MNs) → 4/4; also all-Rust. Pass criteria beyond the tests' own asserts:
  1. every Rust member is in `validMembers` of every mined commitment for every type;
  2. no PoSe penalty ever lands on a Rust MN;
  3. recovered sigs, CLs and IS locks form when the threshold needs Rust shares (force this by stopping dashd MNs);
  4. `quorum dkgsimerror` on Rust MNs gives dashd's complaint and justification outcome;
  5. blocks from either implementation's GBT are accepted by the other;
  6. StateDigest is equal on all nodes at every block.
- **Stage B, devnet soak on agentbox** (≥ 2 weeks; docker compose; NFR-CEQ-09b):
  - **Topology:** 1 dashd seed and miner; 20 regular MNs (10/10); 8 evonodes (4/4); 4 dashd wallet nodes; 1 Rust non-MN node.
  - **Devnet LLMQs:** LLMQ_DEVNET 12/7/6 for CL and MNHF (`-llmqchainlocks`, `-llmqmnhf`); LLMQ_DEVNET_DIP0024 8/6/4 for IS; LLMQ_DEVNET_PLATFORM 12/9/8.
  - **Timing:** `-powtargetspacing=30..60` and `-highsubsidyblocks`.
  - **Workload:** payments and IS; CoinJoin with both coordinators; governance → superblocks with triggers from both implementations; asset locks and unlocks via `quorum platformsign`; an EHF test deployment; ProTx churn; V24 and `evo_shares` txs.
  - **Chaos:** kills, netem partitions, ±60 s skew, rolling upgrades, disk full.
  - **Shorter runs:** 1:3, 3:1 and all-Rust, covering ≥ 4 DKG cycles of every type.
  - **Metrics:** DKG success and the Rust share of `validMembers`, CL latency and coverage, IS latency, PoSe events by implementation, CoinJoin rounds per coordinator, superblock payouts, StateDigest or best-hash divergence (which pages the manager).
- **Stage C, testnet** (REL-2.3): ≥ 4 regular Rust MNs + ≥ 1 Rust evonode through dashmate for ≥ 30 days, next to the BS2 testnet oracle; collateral via the `dash-faucet` skill. This needs inbound reachability (§8 Q2).
- **Stage D, mainnet** (REL-2.5): a pilot with pasta's collateral (DEC-06, §8 Q1).
- **Shadow mode** (from M4; NFR-CEQ-10, US-NODE-22). rdashd and dashd run side by side, each with its own peers and also connected to each other. On mainnet the dashd side is the **v23.1.8** reference, on testnet the v24 oracle (§2.25).
  - At M4 the rdashd side follows blocks and ChainLocks but has no IS or governance runtime, so it is weaker than dashd at the tip. That run is a 7-day **smoke**, and the REL-1.2 clock starts at M5, when LK1b and GV1b land (Opus M-10.2).
  - The external comparator (O15) checks after every block: the tip (with a CL-lag window), `getblockchaininfo`, `gettxoutsetinfo muhash`, `protx diff` (prev → tip), `quorum list`, `getbestchainlock`, deployment info, and the mempool (set similarity). Any divergence pages the manager (a STATUS.md entry plus a task).

### 4.10 L6: P2P fingerprint checklist

The 31 behaviours of PRD §4.2 are the authority (NFR-FP-01). The columns add the owner module and the concrete values from both designs and R4 §2.7. Each behaviour is a scripted probe run against both binaries under identical regtest state and mocktime. Transcripts are normalized (nonces, timestamps, challenges; Poisson-jittered batches compared as sets within windows) and diffed. The allowed list holds only randomness.

| # | Behaviour | Owner | Values |
|---|---|---|---|
| 1 | `version` layout and values | `dashd-p2p::handshake` | 137 bytes for the default fixture (varies with UA, comments, devnet name); init version 209 framing; `mnauthChallenge` (32) + `fMasternode` (1) after `relay`; addrMe = our services + zero address; addrYou zeroed for unroutable/proxied/non-v1; UA from `ReleaseProfile`, no rc suffix, `devnet.<name>` on devnets; `start_height` |
| 2 | Service bits | handshake | 0x1C05 node, 0x1C45 MN, pruned variants; `NODE_NETWORK_LIMITED` with `NODE_NETWORK`; the `bloomfilter` permission grants `NODE_BLOOM`. Under the v23.1 profile an MN with an explicit `peerblockfilters=0` may lack `NODE_COMPACT_FILTERS` (O17 P2P-35) |
| 3 | Who speaks first | handshake | outbound sends `version` at once; inbound only after receiving one |
| 4 | After the peer's `version` | handshake | `sendaddrv2` (≥ 70223), `sendtxrcncl` only if enabled, `verack`, then `getaddr` on outbound full-relay only; feelers disconnect here |
| 5 | After `verack`, in order | handshake | `mnauth` (MN, not on probes) → `sendheaders2` xor `sendheaders` → `sendcmpct(0,1)` iff `CanRelay()` → `senddsq(1)` unless rejecting txs → `ping` → `getheaders2`/`getheaders` → `qwatch` when applicable |
| 6 | Pre-handshake messages | peerman | messages before `version`, and non-allowed messages before `verack`, are ignored |
| 7 | Ping, headers policy | peerman | immediate `ping`, every 2 min; `getheaders2`/`getheaders` policy; empty `headers`/`headers2` below minimum chain work (type follows the service bit) |
| 8 | Dash sync chatter | `dashd-masternode::sync` | `getsporks` once per address 0.5–2 s after the handshake; `govsync(0, empty bloom)` ≈ 8 s later; per-object vote syncs at Core's cadence; `mempool` to outbound after sync; disconnect after `full-sync` |
| 9 | BIP324 | `dash-p2p-transport` | Dash-magic salt; garbage uniform 0..4095; short IDs only at ≥ 70235 (≥ 70240 `platformban`), long before `version`; accepted ranges 0–32 and 128–168 (5 and 29–32 empty and ignored silently; others invalidate); empty version packet; rekey; inbound v1 detection; outbound v2 only to bit-12 peers; v1 reconnect after early disconnect |
| 10 | v1 framing errors | transport | bad checksum or command → drop the message, keep the connection; bad magic or oversize → disconnect; 3 MiB cap |
| 11 | Unknown and absent messages | peerman | unknown commands ignored without penalty; `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2` never sent and treated as unknown |
| 12 | Misbehaviour | `misbehavior.rs` | additive, threshold 100, the table in §2.14; local/manual/noban exemptions |
| 13 | Blocks-only violations | peerman | `tx` disconnects on block-relay-only, feeler and `-blocksonly`; `clsig`, `spork`, `govobj`, `isdlock` do not; `inv` ISDLOCK on block-relay-only from > 70223 disconnects |
| 14 | Size limits and penalties | peerman | `inv`/`getdata` 50,000, `addr` 1000, `headers` 2000 / 8000, `notfound` limit (+20 each; 116 under v23.1, 50,000 under v24, O17 P2P-10); locator > 101 disconnects |
| 15 | Address relay | peerman, addrman | 0.1/s token bucket capped at 1000; one `getaddr` answer per connection from a 21–27 h cache; 23%/1000 caps; the 2 h time penalty; relay only small fresh `addr`; `getaddr` ignored on outbound; `sendaddrv2` after verack disconnects |
| 16 | Trickle and batching | peerman | Poisson 5 s inbound shared / 2 s outbound / 1 s verified MN outbound / none MN↔MN or noban; `INVENTORY_BROADCAST_PER_SECOND` 7; the immediate path for non-tx invs incl. block-relay-only |
| 17 | `mempool` replies | serving | tx invs + matching ISDLOCK invs + the best CLSIG inv; `NODE_BLOOM` required, else disconnect |
| 18 | Filtered blocks | serving | `merkleblock`, then `tx`s, then `isdlock`s |
| 19 | BIP37 filters | serving | `filterload`/`filteradd`/`filterclear` limits and penalties (+100 bad filter); disconnect without NODE_BLOOM |
| 20 | BIP157 | serving | `getcfilters` family limits (1000/2000/1000) and the service requirement |
| 21 | headers2 | serving, sync | bit-exact encoding (MRU version list, int16 time deltas, "bit set = field present"); 8000/2000 limits; announcements |
| 22 | `getmnlistd`/`getqrinfo` | serving | byte-identical answers in every version band; +100 for unsolicited `mnlistdiff`/`qrinfo`; +1 or silence on request errors (missing block data: +1 under v23.1, silence under v24, O17 DKG-13) |
| 23 | `clsig` | `dashd-locks` handler | +10 when invalid; +10 when unrequested only under v24 (O17 P2P-12) |
| 24 | Block download | `BlockSync` | 16 in flight per peer; window 1024; stall 2→64 s (×0.85 decay); block timeout 150 s·(1+0.5·N); headers timeout 15 min + 1 ms/header; compact-block HB peer counts (≤ 3) and 3 in flight per block |
| 25 | Connection behaviour | `ConnMan` | 8 full-relay + 2 block-relay + anchors; feelers every 2 min that disconnect after `version`; extra peers (stale tip 150/450 s; extra block-relay every 5 min); ADDR_FETCH disconnect rule; DNS-seed timing (11 s / 5 min, 3 at a time); ~100 ms linger + `shutdown(SHUT_WR)` |
| 26 | Inactivity timeouts | `ConnMan` | 60 s for the first message (`-peertimeout`), then 20 min; ping timeout; `CHAIN_SYNC_TIMEOUT` 20 min; `MINIMUM_CONNECT_TIME` 30 s |
| 27 | Devnet | handshake | subversion contains `devnet.<name>`; a devnet-name mismatch is penalised (100/1) |
| 28 | Masternodes | `dashd-masternode`, `ConnMan` | inbound refused until synced; 1 s eviction protection while MNAUTH is pending; ≤ 3 verified inbound legs per ProTx (v24 only, O17 P2P-18); the MNAUTH tuple; probes disconnected after MNAUTH; unverified MN connections dropped after 5 s; `qsendrecsigs` to relay members; no `sendcmpct` on non-IQR MN connections; no trickle between MNs; no non-preferred delay in MN mode; DKG messages only to relay members via inv/getdata; recovered sigs pushed to peers that asked; the 100 ms send cadence on MN connections |
| 29 | Overlay relay | sporks, governance, coinjoin | spork relay; governance relay batched every 5 s; DSQ as inv (≥ 70234) or full push by peer version; unbalanced DSTX announced as `MSG_TX` to peers < 70241 |
| 30 | `MSG_ASSET_UNLOCK` | serving | announced by instance hash to peers ≥ 70242 (testnet profile only) |
| 31 | `platformban` | `dashd-masternode` | relay and penalties per Core |
| +A | Object-request timing (O17 P2P-05..-11) | request tracker (per `P2pRules.object_download`) | v24: `TxRequestTracker` with `getdata` intervals 15 s (QSIGREC) / 5 s (CLSIG) / 10 s (ISDLOCK) / 60 s, 100 in flight, 100,000 announcements, no non-preferred delay on an MN; v23.1.8: legacy `ObjectDownloadState` (§2.25 row 9) |
| +C | Cross-peer ordering (rev1, Opus M-5) | peerman | peer A sends a block or tx and peer B immediately sends `getheaders`/`getdata`/`mempool`; B's answer reflects A's message exactly as dashd's does. Same-peer reply order after a `tx`, `ping` and `getdata` matches |
| +D | Scheduling under load (rev1, GPT F13) | peerman | multi-peer, backpressured (`fPauseSend`), delayed-worker and flood-plus-probe transcripts; response-latency distributions compared by KS; the 100 ms throttle on MN connections only |
| +E | Per-profile differences (rev1, M-2) | all | every P2P- and MN-observable `profile` row of O17 §5 probed against v23.1.8 dashd (regtest `-releaseprofile=23.1`) and v24 dashd |
| +B | Version-gated serving | serving | DSTX/DSQ modes by version; legacy `mnlistdiff` construction below 70236; SML entry version by peer version |

**Statistics and the classifier.**
- **Timers:** NFR-FP-03 two-sample KS tests, p > 0.05 over ≥ 10^4 samples (ping, trickles, 6 s sync tick, `govsync` and `getsporks` cadence, DNS-seed timing, feelers, extra peers, the 100 ms MN send cadence), plus, rev1 (Opus M-6), **isdlock, recsig and clsig relay latency** after the triggering input, and the sig-share send cadence.
- **Passive traces:** dashd and rdashd side by side on testnet and mainnet with identical configs, `-capturemessages`, parsed with `contrib/message-capture/message-capture-parser.py`, and compared as message n-grams and timing histograms.
- **Classifier:** a gradient-boosted model trained on probe results plus traces, over ≥ 200 sessions per implementation on regtest and testnet, for both the node and the MN viewpoint. **Acceptance: held-out AUC ≤ 0.55** (NFR-FP-02). It is re-run on every release of either implementation.
- **NFR-FP-04:** no listening port, service or response that dashd does not have is visible by default (metrics endpoints bind to loopback and are off by default).

### 4.11 DIFF-RPC, XCLI, SPV-as-consumer

- **DIFF-RPC.** Two nodes on the **same chain**: a regtest scenario script, or both at the same mainnet/testnet tip in shadow mode.
  - The corpus is generated from `dash-rpc-spec`: every read-only method × argument variants. Examples: `getblock` across verbosities and blocks with every special-tx type, `decoderawtransaction`, `protx diff`/`listdiff`, `quorum info`/`list`/`listextended`/`rotationinfo`, `masternodelist` modes, `gobject list`, GBT with the same mempool, `getdeploymentinfo`, `gettxoutsetinfo muhash`. Wallet RPCs run in regtest scenarios with identical seeds.
  - Normalization drops only entries in `compat/ALLOWED-DIFFS.md`, which is limited to the PRD's three classes: implementation identity, host-dependent values, values random by design. **Numbers are compared textually** (`1.00000000`).
  - The **Platform P-1 subset is frozen** (PRD §5.3): golden request/response fixtures from reference dashd (regtest with dashmate's config, and testnet) run in CI, including behaviour beyond JSON shape (`getbestchainlock` -32603 with no CL; whitelist 403s; `softforks["mn_rr"]`).
- **XCLI:** rdash-cli and stock dash-cli, each against dashd and rdashd, produce byte-identical stdout, stderr and exit codes modulo the registry. The operator workflows OW-01…OW-22 (PRD REL-3) are scripts.
- **SPV:** dash-spv's `dashd_sync` (35) and `dashd_masternode` (10) suites gain `DASHD_IMPL=rdashd` in `DashCoreNode` and run against rdashd as the only peer. This is a **smoke test**, weaker evidence than v1 implied (Opus m-20): dash-spv does not verify mnlistdiff merkle proofs or CbTx roots (R4-E §11.4), and since #1080 it marks the 24 stale v1 quorums `Skipped`. The real serving check is EV4's byte identity against the oracle's `getmnlistd`/`qrinfo` in every version band. A dashj-style Python BIP37 client test covers `filterload` → `merkleblock` + `tx` + `isdlock` ordering. dashj on the Android emulator closes REL-5.7.

### 4.12 Non-differential fuzzing and safety

- **Crash, hang and UB targets:**
  - every `BaseCodec` decoder in `dash-primitives`/`dash-p2p-core` (round trip);
  - the BIP324 state machine and V1 framing;
  - `ArgsManager`, `dash.conf` and `settings.json` parsing;
  - addrman deserialization and every flat file;
  - the LevelDB, BDB and SQLite readers;
  - RPC JSON request parsing, REST paths, ZMQ formatting;
  - headers2 (de)compression; DKG framing (`MaxDKGMessageSize`); `AUTOBITSET`/`DYNBITSET`.
- **No panics on untrusted input** (NFR-SEC-02), proven by fuzzing. Violated internal invariants in consensus or DB state abort the process, as Core's assertions do.
- **Tooling:** `cargo miri` on the pure crates; ASAN/TSAN jobs on node crates (fixing the broken sanitizer path filters, R1 §4); the `security-review` skill on P2P, RPC and wallet surfaces before the mainnet pilot (NFR-SEC-09).

### 4.13 Gates by risk class

| Gate | Applies to | Content |
|---|---|---|
| G0 (every PR) | all | `cargo fmt --check`, `cargo clippy --all-features --all-targets -- -D warnings`, `cargo test --workspace`, the dash-spv dashd suites, L0, L1 smoke (5 min per touched target), BS2 regtest sets, `cargo deny` (purity bans), the PF0 counter guard for touched crates (§3.11), the private EMBARGO-1 tests (PLAN §7) |
| G-consensus / G-state | pure crates, chainstate, mempool, mining, base-sdk bumps; **any change, in any crate, to hashing/decoding helpers, state mutation, the clock, init/shutdown, block submission or chain invalidation** (GPT F18: the class follows the riskiest changed behaviour, not the crate prefix) | G0 + segment replays (`-consensusxcheck`) for touched windows + the BS2 testnet sample + an L3 subset + `diff_chain` smoke + ST3 model-level crash tests (STATE) + **two independent reviewers (GPT-6.1-Sol xhigh + Opus) + the `code-review` skill at high effort** + the quirks touched |
| G-LLMQ | evo llmq module, dash-llmq, quorums, locks | G-consensus + mixed regtest (from M7) + the BLS differential targets |
| G-P2P | transport, p2p, overlays, masternode | G0 + the L6 transcript suite + the SPV subset (from M5) + two reviewers + `code-review` |
| G-SEC (rev1) | key handling, wallet encryption and signing, untrusted-file parsers (BDB/SQLite/LevelDB import), the oracle trust boundary | the gate of the task's base class (G-other, G-state, G-P2P, …) + an independent security reviewer (Opus xhigh, `security-review` skill) before the code counts as complete (GPT F18) |
| G-other | rpc, cli, args, indexes, tooling | G0 + affected functional tests + DIFF-RPC/XCLI subset + one reviewer (GPT-6.1-Sol high) |
| Nightly | `node/integration` | full testnet replay; L1 fuzz 8 h on 16 cores incl. `diff_chain`; L3 long run; the L4 passlists for both pins (incl. tier 2 mixed); L6 probes; SPV; DIFF-RPC corpus; ST3 device-level crash runs; PF0 counter workloads |
| Weekly / milestone | `node/integration` | full mainnet replay with xcheck; v23.1.8 sampled cross-checks; full L6; the devnet soak report; ST3 VM crash runs; B1 wall-clock scenarios in a reserved window; rebase of the oracle-shim and state-digest branches; golden refresh if a pin moved; PIN-DELTA re-run if a pin moved |
| Release | RC | REL-1…6 (PLAN §1) |

### 4.14 How R4's findings land in this design

| R4 item | Where |
|---|---|
| F1 (EMBARGO-1) | PLAN §7 handling: CS0, CS1, quirk 1, scan (g), the private L3 operator |
| F2 assumevalid also skips CbTx, credit pool, commitment and ProTx sigs | §2.10 step 6; replays use `-assumevalid=0` |
| F3, F6 payee and value leniencies | §2.10 "must not be tightened"; `ChainHelper` NoCheck path; recorded decisions |
| F4 BTC/KGW tolerance-only; DGW exact from 68590 | §2.4 pow, §4.5 |
| F5 `merkleRootQuorums` covers disabled types | §2.5 cbtx, quirk 18 |
| F7 `hash_serialized_2` precedence bug | §2.4 coins, §2.16 coinstats, quirk 33 |
| F8 `EvoSnapshot`/`CanonicalMNListHash` | §2.9 `mn_snapshot` CF; §4.6 patch digest |
| F9 GBT proposal mode as a zero-PoW oracle | §4.7 |
| F10 per-node binaries; MNs force `-disablewallet` | §4.8 mixed modes A and B |
| F11 handshake timings | §4.10 behaviours 5 and 8 |
| F12 non-Core fixtures | §2.23; task O4 |
| E§11.1 four dash-pkc fixes | BS11a/b |
| §1.6 `MinBIP9WarningHeight` floor; i32 overflow; historic bits | §2.4 deployments |
| §1.10 validity inputs that are not chain data | `ChainHelper` (§2.10), `NodeClock` |
| E§4.3 BLS wrapper semantics; E§11.1 dash-pkc fixes | §1.4.1 (rev1, verified by probing Core's wrapper), BS11a/b |
| §1.12 v23.1.8 vs v24 (rev1) | §2.25 `ReleaseProfile`, PLAN O17 (PIN-DELTA) |
| §1.12 V24/`evo_shares` moving target | DEC-05 pins; `ReleaseProfile` (§2.25); PLAN standing workstream SW-V24 and O17 |
| §1.15 quirk register | §4.1 principle 5; task O5 |
| §2.3 StateDigest, §2.3.4 snapshots, §2.3.5 scans | §4.6 |
| §2.4 mutation | §4.7 |
| §2.5 tiers | §4.8 |
| §2.6 stages A–D | §4.9 |
| §2.7 fingerprint | §4.10 |
| §2.8 V0–V9 first briefs | PLAN tasks: V0 → DEC-14 (done); V1 → O4; V2 → O1/O2a/O2b; V3 → O7; V4 → O8; V5 → O6; V6 → O11; V7 → O10; V8 → O12; V9 → O13 |
| §1.14 difficulty ratings (XL: DML, rotation, DKG, governance, P2P) | PLAN sizes and splits; design steps; xhigh assignments |

---
## 5. Sequencing (summary; PLAN.md is authoritative)

Rev1 re-baselines the schedule (Opus M-9, M-10; GPT F17).
- **The model.** The windows come from one explicit task graph (PLAN §3–§4), using S/M/L midpoints of 1/1.5/3 weeks. Each task also carries its design step (D +1 week, D2 +2 weeks, with designs started early) and its review-and-fix latency (+1 week for CON/STATE/LLMQ/P2P, +0.5 for OTH), and interface-first dependencies are modelled explicitly. About 15% contingency is added from M4 on.
- **Why v1's windows were wrong.** They left out design and review latency, and their diagram had edges the task graph did not.
- **The masternode track is decoupled from Done-1.** It gates on regtest-correct evo/LLMQ (CH7b plus regtest L4 and `diff_chain`), not on the full mainnet replay campaign.

| M | Name | Window (from 2026-10-08) | Exit in one line | Charter / PRD |
|---|---|---|---|---|
| M0 | Foundations | wk 0–7 | BS1 merged, BS2/B1/BS3 merged; the oracle shim builds for both pins; O3 fuzz farm; vectors and quirk scaffolding; chain scans (a)–(h); the v23.1.8 mainnet reference (link-count rule); the StateDigest spec; ST1 decided; ST2; ST3a model-level crash harness; O17 PIN-DELTA report; PF0 counter gate live; script/sighash L0 = 100% | — |
| M1 | Base SDK milestone | wk 0–13 | "Base SDK integrated" (§1.7, rev1 wording); `dash-consensus` core rules done with L0 + L1; O2a with the coverage matrix | charter "Base SDK first" |
| M2 | Headers-first sync | wk 6–18 | rdashd syncs mainnet and testnet headers; outbound **and** minimal inbound handshakes identical per profile (P2a, P2b); RPC/CLI skeleton; first functional tests | US-NODE-01 (part) |
| M3 | Full validation (non-evo) | wk 10–23 | replay mainnet 0→1,028,159 and testnet pre-DIP3 with StateDigest equality; ST3 device-level power-loss tests; `diff_chain` (non-evo); reorg suites; tier-1 non-evo FTs | REL-1.1 (part) |
| M4 | Evo/LLMQ consensus: full parity | wk 14–35 (replay ≈ wk 30; live CL following and the 7-day shadow smoke ≈ wk 35) | full mainnet and testnet replay equal at every block with `-assumevalid=0` and xcheck; `diff_chain` with evo; live sync following CLs (P5a, SP0, LK1a); safe import | **Done-1** (REL-1.1, REL-1.3) |
| M5 | Full-node services | wk 20–44 | non-MN drop-in: mempool, GBT, RPC, indexes, ZMQ/REST, serving, governance runtime, IS node role; tiers 1 + 3 per pin; mixed mode A; L6 for the node; SPV smoke; **the REL-1.2 clock starts** | REL-1.2; REL-2.6 (node half) |
| M6 | Wallet and migration | wk 24–46 | node wallet, Core wallet and datadir import, exports, CoinJoin client; tier 4 | REL-4 (wallet stories) |
| M7 | Masternode on testnet | wk 26–52 (devnet soak ≈ wk 43; the 30-day testnet soak ends ≈ wk 50) | MN roles; mixed mode B; devnet soak; ≥ 30 days on testnet with ≥ 4 MNs + 1 evonode | **Done-2 on testnet** (REL-2.1–2.4, 2.6, 2.7) |
| M8 | Release | wk 44–58 | mainnet pilot (needs §8 Q1 and either the v23.1.8 MN profile or mainnet on v24); all MUST RPCs; all stories; all suites; benchmark report | Done-3…6 (REL-2.5, REL-3…6) |

**Critical path (computed with `tools/cpm.py`; slack measured by delaying each task):** BS3 → BS4a → BS6a → EV2a → LQ1a → LQ1b → LQ2 → LQ3 → CH7b, which then forks:
- **Done-1:** the REL-1.1 replay campaign;
- **masternodes, two co-critical branches:** QM1a → SG1a → SG1b, and MP1a → LK1b → LK1c. Both join at O12b (the mixed-quorum harness), followed by the devnet soak, the 30-day testnet soak (RL4) and the mainnet pilot (RL5).

**Near-critical** (≤ 1.5 weeks of slack):
- BS11a-t, CS0 and EV0 (0.5 week);
- CS3, EV1, EV2b, EV3 and CH7a (1 week);
- M5's P5 → O13b (1.5 weeks).

The CH1 → CH3a → P3 → AM1 chain has more than 2 weeks of slack.

---

## 6. Decisions table

Each row gives the choice, the rejected alternative (and whose it was: F = Fable, O = Opus, PRD, R3, R4) and the reason. "Both" means F and O agreed.

| # | Decision | Choice | Rejected alternative | Why |
|---|---|---|---|---|
| D1 | Node type stack | base-sdk types on the local fork; `dashcore` bridged | build on `dashcore` and migrate later (both rejected it) | consensus code is reviewed once against Core-shaped, corpus-tested types; no Bitcoin leftovers in the consensus path |
| D2 | base-sdk gaps | close them locally on `node/base-sdk` with upstream-shaped commits (DEC-09) | wait for upstream | single maintainer; the program is local |
| D3 | base-sdk consumption | workspace `[patch]` → `file://` git rev, tagged `node-base-sdk-YYYY-MM-DD` (O) | path deps via a symlink (F) | reproducible from every worktree; independent of uncommitted base-sdk edits; recorded in `Cargo.lock` |
| D4 | Where rules live | pure crates in rust-dashcore; base-sdk `Checkable` never used (O) | fix and reuse `Checkable` (F BS17) | verified incomplete and divergent (mutation flag ignored, `Oversize` never raised); keeps the fork free of rules |
| D5 | Decoder domain | Rule DDE; BS4a fixes base-sdk (O) | range checks in decoders (base-sdk status quo; F had no rule) | decode-vs-rule failures differ observably (index marks, scores, `submitblock`); verified divergences |
| D6 | PLAN §7 content (EMBARGO-1) | node-side in `dash-consensus` only, marked `EMBARGO-1` (O) | in base-sdk `dash-primitives` (F BS17) | the fork stays upstreamable with no embargoed content (DEC-09, DEC-14) |
| D7 | Canonical bytes | txid, merkle, sighash, storage and relay use Core's re-serialization; wire bytes only on a proven-equal fast path (O) | hash and store the wire bytes (PRD FR-STO-04 wording) | Core hashes `SerializeHash(*this)` and stores and relays its re-serialization (`blockstorage.cpp:748`); eager BLS fields and `CoreBool` make the two forms differ for P2P objects (§1.4.1) |
| D8 | Codec count | two during the program; BS2 is a merge gate; convergence later (both) | big-bang migration of `dashcore` first | keeps ~2,100 Platform uses off the critical path |
| D9 | bincode | none in node crates; `dashcore` newtypes keep it (both) | add bincode to base-sdk | the orphan rule; kwvg's base-sdk#1 decision; FR-STO-05 |
| D10 | Hash display | Core order in node, RPC and ZMQ; `dashcore` FFI display unchanged (both) | flip everything now | RPC parity; mobile stability |
| D11 | Network/Devnet | `NetworkKind` + `ChainParamsBuilder`, devnet name in params, derived devnet genesis, no global `Params()` (O names, both semantics) | name in the enum; global params | matches Core; functional tests need every override; globals are a known Core bug source |
| D12 | v24 payloads | both copies: `dash-primitives` (BS6a/b) and `dashcore` (the running V24a task, v1's BS17), BS2-equal (F) | leave `dashcore` to upstream's schedule (O) | testnet V24 is live and the wallet seam needs them; the production fix for shipped SPV wallets is the upstream PRs #1136–#1138 (pushing them is pasta's call, §8 Q5) |
| D13 | X11 footgun | X11 unconditional in `dashcore`; `dash-pow` becomes default only after the benchmark (both) | keep the feature | Cargo feature unification can flip block identity |
| D14 | Bridge location | `dashcore::bridge` behind feature `base-sdk-bridge` (O) | a separate `dash-bridge` crate (F) | the orphan rule forbids `From` between two foreign types in a third crate |
| D15 | Base SDK task list | BS1–BS18 as renumbered in v1; rev1 splits BS4, BS6, BS9 and BS11 (BS4a/b, BS6a/b, BS9a/b, BS11a-t/a/b) and folds BS17 into the running V24a (§1.7) | F's 18 / O's 16 with clashing numbers | merges both; dropped items moved to CV or standing procedures |
| D16 | Crate naming | `dash-*` libraries (pure ones sans-io, **not** `no_std`: X-1), `dashd-*` daemon runtime, `rdash*` binaries (O + DEC-03) | everything `dash-*`, binaries `dashd-rs` (F) | the sans-io boundary is visible in the name; DEC-03 |
| D17 | Crate granularity | 5 pure + 15 runtime + 3 shared + 3 binaries + 4 test = 30 (rev1 folds `dashd-sporks` into `dashd-masternode` and `dashd-zmq` into `dashd-rpc`, X-5) | F 13+5; O ~30; v1's 32 | parallel ownership for 6–10 agents, with `cargo deny` layering, without boilerplate crates |
| D18 | LLMQ consensus home | inside `dash-evo` (`llmq` module); `dash-llmq` = DKG/sig-share protocol math (F's split) | a pure `dash-llmq` above `dash-evo` with selection and commitments (O) | the CbTx check and `process_special_txs_in_block` need both; no cross-crate straddle |
| D19 | `net_processing` runtime | one `PeerManager` actor reproducing Core's shuffled, wake-driven `ThreadMessageHandler`; the 100 ms throttle on MN connections only; decode after the state gates; the actor **awaits** every message whose effects later messages observe; parallelism inside a message and on Core's own asynchronous edges only (rev1: F13, F14, M-5) | per-peer state machines (O); v1's 100 ms tick for all peers, eager decode in readers, and asynchronous handoffs | Core's semantics are globally sequential and observable (cross-peer staleness, same-peer reply order, request decisions) |
| D20 | P2P crates | `dash-p2p-transport` + `dashd-p2p` (net + processing modules) (O) | `dash-p2p` + `dash-sync` (F) | fewer crates; module ownership is enough |
| D21 | State-dependent validity | one `ChainHelper` trait with NoOp and Replay implementations (O) | four `*Oracle` traits (F) | one wiring point and one replay implementation |
| D22 | Storage engine | RocksDB behind ST1's narrow trait (`Commit::{Buffered, Durable}`, `sync_wal`, `snapshot`), WAL on, `atomic_flush`; confirmed by the ST1 bake-off on the real UTXO-churn trace plus CH2's multi-CF workload; the chosen engine must itself pass every tracked metric (rev1, F12) | no bake-off with MDBX as a reactive fallback (F); MDBX default; v1's "cannot regress by construction" | disk metric (LSM vs random-hash B-tree), atomic CFs; LSM shape alone guarantees nothing, so the measurements decide |
| D23 | DB layout | `chainstate/` (all consensus state, CFs), `llmq/`, `indexes/<name>/` (O) | one `db/` with every CF (F) | the functional framework only caches and copies `blocks/ chainstate/ evodb/ indexes/ llmq/` (verified); indexes toggle independently |
| D24 | On-disk formats | own versioned formats except Core-identical `blk`/`rev` (DEC-11); Core `Coin` bytes reused as our coins value; evo = our diffs + Core-canonical `EvoSnapshot` snapshots (O) | Core keys and values everywhere with re-sorted `dmn_S3` (F) | DEC-11; Core's evodb depends on immer order; the canonical snapshot stays comparable with `CanonicalMNListHash` |
| D25 | Flat files | own versioned envelope; loaders also accept Core's formats; JSON configs kept as Core's | write Core's flat formats (F) | DEC-11; functional tests that drop Core files still pass |
| D26 | txindex keys | 8-byte txid prefix + verification read (O) | Core's full-key layout (F) | ≈ 1.5–2.5× smaller (disk metric; m-14), identical RPC behaviour |
| D27 | Coins cache and durability | `PackedCoin` + freeze-and-flush under one memory budget (active + frozen + prefetch + batch ≤ B), epoch-fenced prefetch, and the §3.5 protocol: files durable → one Durable batch with index + **all evo** + `head_blocks` → coin sub-batches → a Durable tip batch; coins-only `ReplayBlocks` (rev1: F02, F03, F05, F12, M-4) | v1: evo and index in the *last* sub-batch, a barrier after the DB commit, a 90% + 90% freeze; porting `RollforwardBlock` with special txs (M-4's option 2); one giant batch | every commit prefix recovers; recovery is coins-only; data before reference; memory bounded |
| D28 | Chain work type | `Arith256` (F; BS1 direction) | `u128` with an overflow assert (O) | `arith_uint256` semantics (FR-CHS-01, FR-CON-14); 16 B/entry is not worth a semantic deviation |
| D29 | Test engine | in-memory `KvEngine` | redb as a second real engine (F) | one real engine; the model fuzz needs only a `BTreeMap` |
| D30 | Concurrency model | one validation thread + `ArcSwap` snapshots + rayon pools + tokio edges (both) | a global `cs_main`; async everywhere | determinism; no reader contention |
| D31 | IBD pipeline | S1 parallel within the awaited accept of a block (cross-block only in reindex), S2 prefetch ahead on accepted blocks, V sequential, W, E (rev1) | v1's out-of-order S1 across in-flight P2P blocks | Core accepts blocks synchronously in `msghand`; parallelism must not reorder observable accepts |
| D32 | BLS batching | batch the basic scheme per block; legacy always individually (O) | batch everything (F) | legacy decoding skips subgroup checks, so the random-linear-combination soundness argument fails |
| D33 | Floating point (rev1, F19) | the **release binaries decide**: if `pow(x, 2.0)` is folded to `x*x`, that is the reference and the implementation; a libm port is kept only for call sites the binary routes to libm (KGW). An **admissible-domain gate** decides acceptance, and a mismatch on any reachable chain or configuration fails | v1's glibc-`pow` port as the primary implementation, with "history is the arbiter"; platform libm | portability (NFR-PORT-02); the reference is what the shipped binaries compute; history alone does not cover regtest/devnet domains |
| D34 | Optimisation safety | reference twins + `-consensusxcheck` (O) | review only | "never trade correctness for speed" |
| D35 | Allocator | jemalloc by default; PF2 measures vs mimalloc before M5 | mimalloc (O) without measurement | already in the workspace with heap profiling; decided by B1 numbers |
| D36 | `-par` | Core semantics, cap raised to 64; gating benchmarks at equal `-par` | Core's cap of 15 | NFR-PERF-02(c) comparable config; default-config gains reported separately |
| D37 | Verification layers | PRD/R4 L0–L6 + DIFF-RPC/XCLI/SPV/BENCH tags | F L0–L7; O L1–L10 | one numbering, which NFR-CEQ already cites |
| D38 | Oracle | static `libdash_oracle.a` C ABI from `node/oracle-shim` per pinned release, with **reviewed adapter logic** (synthetic chainstate contexts, per-handler P2P reads), full result classes, the coverage matrix (O2a), and `diff_chain` (O2b) (rev1: M-7, F15) | "the shim adds no logic" (v1); an autotools shared library (F); subprocess RPC only | in-process differential fuzzing needs speed and full domain coverage, including state transitions |
| D39 | Replay evidence | StateDigest v1 with semantic, observation and schedule-dependent field classes; digest-first (per-block patch digests and MuHash; DMN RPC goldens sparse and on demand); failed-attempt records; decisions recorded on live oracles (rev1: F16, X-2, m-12) | spot checks with `gettxoutsetinfo`; per-block `protx listdiff` goldens; recording during `-reindex-chainstate` | catches the first divergent block and field, including state the SML omits, at a fraction of the RPC cost |
| D40 | Mainnet reference | consensus goldens for both networks from the **v24** oracle; a v23.1.8 mainnet reference (built under the link-count rule) for sampled cross-checks, proposal-mode rejection, fingerprints, shadow and RPC shapes (rev1, M-8) | v23.1.8 goldens through a `node/state-digest-v23.1` backport of `evo/snapshot.*` (v1) | v23.1.8 lacks the RPCs and the canonical hash; both releases accept exactly the mainnet history (§2.25) |
| D41 | Functional tests | run Core's suite unmodified with per-node binaries; mixed modes A and B; regtest Core-datadir auto-import (both + O) | port the tests to Rust | 264 tests for free; mixed-quorum interop comes for free |
| D42 | P2P identity and release pinning | a per-network `ReleaseProfile` covering every non-deployment-gated release difference (P2P, IS gate, sporks, DKG criteria, MN min proto, MN init, policy, the AU stable txid, reject strings, RPC), selectable on regtest (rev1, M-2) | `ProtocolProfile` for the handshake only (v1); one `/Dash Core:24.1.0/` UA everywhere (F) | indistinguishable means matching what each network runs, beyond the handshake |
| D43 | Real identity | `-version`, the log header and a hidden `getimplementationinfo` RPC | an extra `implementation` field in `getnetworkinfo` (F) | keeps DIFF-RPC equality |
| D44 | RPC server | hyper + `dash-rpc-spec`; handlers build Core-ordered JSON; `rpc-json` stays client-side (O) | `rpc-json` structs as response types (F); jsonrpsee/axum | Core semantics (squashing, whitelists, queues, warmup); R2 (no `dashcore` in node crates) |
| D45 | CLI | a generic client driven by the spec's conversion table (O) | the typed `dashcore-rpc` client | covers all 289 commands from day one; works against dashd |
| D46 | Wallet persistence and crypto | SQLite in our schema; argon2id + XChaCha20-Poly1305; Core's crypter decrypt-only (O; DEC-11) | Core's record format in SQLite + Core's crypter for writing (F) | DEC-11 (one-way import + exports); stronger KDF (FR-WAL-04 "at least as strong") |
| D47 | Wallet seam | a relevance pre-filter, then bridge only matching txs (O) | convert every block to `dashcore` (F) | keeps the bridge off the IBD hot path |
| D48 | CoinJoin client | wallet track (M6–M7), sharing `dash-coinjoin` with the server (both) | the client inside the server crate | charter scope split; FR-CJC-04 |
| D49 | Test harness | `dash-testkit` wraps dash-spv's `test-utils` in place | move `test_utils` out of dash-spv (F) | avoids upstream merge conflicts |
| D50 | Licensing | MIT for new Core-derived crates (incl. `dash-conformance` once vectors land); CC0 crates depend on MIT crates rather than absorbing Core logic (DEC-07) | relicense the workspace | MIT notice requirement; consumers unchanged |
| D51 | Milestones | M0–M8 (O's numbering; DEC-06 refers to "M7") | F's M0–M9 | one numbering that matches DECISIONS.md |
| D52 | Names, datadir, ports | `rdashd`/`rdash-cli`/`rdash-wallet`, `~/.rdashcore`, dashd's ports (DEC-03) | `dashd-rs`, `~/.dashcore-rs` (F, PRD Q3 default) | DEC-03 |
| D53 | Governance | always on (Core's `-disablegovernance` only) | a governance-less mode (F Q10) | superblock validity and txindex default parity |
| D54 | P1 features | pruning (M5), Tor/I2P/CJDNS (M5–M8), quorum proofs (M7), UPnP/NAT-PMP (M8); assumeutxo out (NG-6) | required before the first MN (F Q8) | PRD priorities; none is needed for an MN |
| D55 | Desktop ↔ node | desktop SPV + optional RPC link to a local rdashd (DEC-01, FR-DSK-03) | the desktop embeds or launches rdashd (O Q7 alternative) | PRD scope; no new API |
| D56 | Node primitives | hash-caching `Tx`/`Block` wrappers over base-sdk types (O) | raw base-sdk types | base-sdk recomputes hashes and re-parses payloads per call |
| D57 | evodb compatibility | import-only (read Core's, write ours) (both) | bit-compatible writes | immer HAMT iteration order (R2 G.3 Q2) |
| D58 | BLS decode domain | Core's wrapper algorithm exactly: eager (VALID / INVALID-as-zeros / error) or lazy per field; scheme source per field; legacy via blst uncompress + the BS11a-t predicate (BS11a D1); `Exotic` points: `false` at the direct verify entry points under the basic verify flag, relic's arithmetic (BS11a's transliteration) under the legacy flag and in IES (BS11a D16); `BlsCtx` per call site (rev1: B-1, M-3) | scheme-agnostic byte bags (v1); blst-only decoding | Core's decode outcomes decide block validity (`bad-qc-payload`) and hash inputs (CLSIG, qfcommit); verified by probing Core's wrapper |
| D59 | Non-transactional effects | an effect channel that survives `Err`, proposal and VerifyDB paths, persisted exactly as Core persists each effect (rev1: F04) | success-only overlays (v1); making all evo state non-transactional | Core writes snapshots, removes IS locks, erases mineable commitments and resets platform bans before later failures; all are observable |
| D60 | Startup validation | ordinary `connect_block` for every block above the recovered tip; signature-free replay only for the `head_blocks` transition (rev1: F01) | v1's signature-free replay of `HAVE_DATA` blocks; a separate validation manifest (GPT's alternative) | `HAVE_DATA` is not validity; Core's split is exactly this, and the head-blocks transition already names the validated blocks |
| D61 | Mempool acceptance | serial-equivalent in arrival order: parallel signature work, then a sequencer that re-runs every cheap mutable check (rev1: F07, M-5) | an optimistic commit that re-checks only the tip and conflicts (v1) | limits, fee floors, ProTx maps and AU totals are shared mutable state that Core serialises under `cs_main` + `mempool.cs` |
| D62 | Mining caches | Core's template-cache policy; a DML memo per exact block (rev1: F08, m-10) | background 100 ms rebuilds and a `(prev, special-tx set)` memo (v1) | an ordinary collateral spend changes the DML; Core's cache semantics are observable |
| D63 | Core datadir import | a stopped or snapshotted source, staging, sealed-file hardlinks, the link-count rule; fast import provisional and backed by a source report checked against complete canonical state (rev1: F09, F10) | hardlinking all `blk`/`rev`, tip-root checks only (v1) | hardlinks share mutable bytes; tip roots don't certify payee heights, snapshots or credit-pool ranges |
| D64 | Benchmark evidence | per PR: deterministic counters (instructions, allocations, syscalls, I/O) against the previous head; wall clock side by side only in reserved windows or on a dedicated host (rev1: M-11) | per-PR wall-clock gates on the shared box (v1) | shared-load noise is about 30% on wall time and ±0.04% on instructions (measured) |
| D65 | Pure-crate `no_std` | dropped; purity enforced by `cargo deny` and clippy lists (X-1, F20) | `no_std + alloc` with a CI job (v1) | no consumer; conflicted with the `OnceLock` types |
| D66 | Shadow comparison | one external comparator (O15); the in-node tripwire only for CL conflicts and the digest log (X-6) | an in-node `-shadowcompare` RPC client in addition (v1) | one mechanism, one review |

---

## 7. PRD errata and alignment notes (for the PRD owner)

1. **FR-STO-04** says blocks are stored as "their exact network bytes". It should say: "the bytes dashd stores and relays, i.e. Core's re-serialization of the decoded block". This is D7; it is what Core-identical `blk` files require (DEC-11).
2. **§0.3 and §1.4 surface names** (`dashd-rs`, `dash-cli-rs`) and **Q3's datadir default** (`~/.dashcore-rs`) are superseded by DEC-03: `rdashd`, `rdash-cli`, `rdash-wallet`, `~/.rdashcore`, dashd's ports.
3. **Q1–Q13 are resolved by DEC-01…DEC-15.**
   - The 21 "D: pending decision" stories follow DEC-01: wallet-holder MN and governance flows come to the desktop after the DashPay core; operator and server tasks stay on N + C. REL-4.2 then reads "delivered per DEC-01".
   - FR-P2P-16 follows DEC-04 (`-honestua`).
   - NFR-CMP-05 follows DEC-07.
   - FR-WAL-02 follows DEC-02.
4. **§0.2/§0.4 "reference dashd"** (rev1): the BS2 oracles run v24.0.0-rc.3 for both networks. They produce the consensus goldens for both, which is valid for mainnet because both releases accept exactly its history (§2.25). DEC-05's v23.1.8 mainnet pin is enforced through `ReleaseProfile`, sampled cross-checks against the O7 v23.1.8 reference, proposal-mode rejection, per-profile functional tests and shadow mode.
5. **FR-RPC-09:** name the identity RPC `getimplementationinfo` (hidden).
6. **FR-WAL-04** is met by argon2id + XChaCha20-Poly1305, which is stronger than Core's `BytesToKeySHA512AES` + AES-256-CBC. Core's crypter is decrypt-only, for import.
7. **NFR-PERF-03:** A1 declares in advance that "half the RSS" is unlikely and that disk can only reach parity (§3.9); REL-6.3 explanations will cite this.
8. **FR-P2P-19 / FR-MEM-07:** our loaders also accept Core's binary formats on detection, which goes beyond "importable" so that the functional tests that drop Core files pass.
9. **NFR-PERF-02(a)** ("nothing else running") cannot hold on agentbox (rev1, M-11). Per-PR evidence uses deterministic counters (§3.11). Wall-clock gates need reserved windows or a dedicated host (§8 Q4).
10. **FR-STO-03 / NFR-OPS-01**: power-loss safety is shown by fault-injection, block-device prefix replay and VM crash tests (§3.5.8). A process kill is only a smoke test (F03).
11. **NFR-PERF 2× targets** for GBT, per-block connect and the LLMQ latencies (rows 5, 8, 15–17) are hypotheses until PF1 measures them, because Core already has several of the levers v1 counted (F11). End to end, the CL/IS/recsig latencies are ≈ 1× by NFR-FP (M-6). REL-6.3 cites §3.9.
12. **DEC-05 / PRD Q5**: "mainnet = v23.1.x" implies the full `ReleaseProfile` (§2.25), not only the handshake. A mainnet MN pilot (REL-2.5) needs the v23.1.8 MN rows of the profile, or mainnet's move to v24 first.
13. **FR-CON-04, NFR-CEQ-13, R-0, Q13** and the banner were rewritten by the EMBARGO-1 scrub (A1-rev1); the PRD now names the issue only by codename.

---

## 8. Open questions for pasta (true blockers only)

Product and irreversible choices are settled in DECISIONS.md (DEC-01…DEC-17) and §6. The program proceeds on the defaults below; each item blocks only the step named. Rev1 adds Q4 and Q5 and amends Q1.

| # | Question | Blocks | Needed by | Default we proceed on |
|---|---|---|---|---|
| Q1 | **Mainnet masternode pilot resources** (DEC-06): 1000 DASH collateral (a regular MN; an evonode with 4000 DASH only after US-PLAT-09 passes), custody of the owner and voting keys, and a host with a public IPv4 and port 9999 reachable. agentbox cannot accept inbound connections. Also approval of the scripted dashd ↔ rdashd swap-back | REL-2.5 (mainnet pilot) | M7 exit (≈ wk 52, rev1), raised then per DEC-06. **Rev1 addition:** the pilot also needs either the v23.1.8 `ReleaseProfile` complete for the MN rows (§2.25: IS gate, sporks, object download, DKG criteria, MNAUTH cap, MN init), which is planned (O17 + per-task profile work), or mainnet's move to v24 first. Pasta is told now so the choice can follow mainnet's upgrade timing | a regular MN; pasta holds the collateral and owner keys; we hold only the operator BLS key on a small VPS outside the home network; until then testnet only |
| Q2 | **Inbound reachability for the testnet masternode fleet** (REL-2.3): ≥ 4 regular MNs and ≥ 1 evonode (dashmate) need a reachable IP:port each (unique IP:port; testnet allows non-default ports). Otherwise they are PoSe-banned. agentbox's network is closed to inbound traffic from the internet, and we must not touch the router. Also, if the faucet's limits cannot supply about 8,000 tDASH of collateral, a testnet top-up | REL-2.3, REL-2.4 (testnet soak, hot swap) | ≈ wk 44, the start of the 30-day testnet soak (rev1 schedule) | port forwards to agentbox for 5 ports, or one small VPS; meanwhile Stage B (the devnet soak) runs on agentbox with no inbound need |
| Q3 | **Tier-2 platform verification** (DEC-08): macOS arm64 and Windows x86_64 builds and smoke tests need hardware or CI we don't have, and node work cannot go to GitHub Actions (DEC-09) | the tier-2 half of NFR-PORT-01 at M8 | M8 (≈ wk 56) | Linux tier 1 only in release candidates; a scripted smoke pack for pasta to run on the Mac Studio and a Windows box when convenient |
| Q4 | **A dedicated benchmark host** (rev1, Opus M-11; a DEC-06-style escalation): an idle machine of agentbox's class (32 cores, NVMe, ≥ 128 GB RAM) running only the side-by-side benchmarks. agentbox is shared by oracles, implementers, fuzzing and soaks, so wall-clock comparisons there are not credible evidence for "never slower than dashd" | the credibility of the NFR-PERF wall-clock evidence (REL-6); **development is not blocked** | before the M3 exit (the first wall-clock bench gate), ≈ wk 23 | per-PR deterministic-counter gates on agentbox (§3.11); wall-clock milestone runs in reserved agentbox windows with all other jobs paused |
| Q5 | **Push the non-embargoed upstream PRs #1136–#1138** (rev1, Opus M-10.4). The production fix for mobile and desktop SPV wallets on V24 activation lives there, and a local branch cannot ship to wallets (DEC-09 keeps node work local) | wallet safety on testnet V24 for shipped SPV wallets (not the node) | now (testnet V24 signalling started 2026-09-24) | V24a lands the same changes on `node/integration`; nothing is pushed until pasta decides |

---

## Appendix A: key types and traits

```rust
// ---- dash-consensus ----
pub struct HeaderCtx { pub height: u32, pub hash: BlockHash, pub time: u32, pub bits: CompactTarget, pub version: i32, pub mtp: i64, pub chain_work: Arith256 }
pub trait ChainAncestry {                       // Core CBlockIndex walking; implemented by the block-index arena
    fn header(&self, id: BlockId) -> &HeaderCtx;
    fn ancestor(&self, id: BlockId, height: u32) -> BlockId;     // skip list, O(log n)
    fn prev(&self, id: BlockId) -> Option<BlockId>;
}
pub trait DeploymentView {                      // per call site: ActiveAfter(prev) vs ActiveAt(block)
    fn active_after(&self, prev: BlockId, d: Deployment) -> bool;
    fn active_at(&self, block: BlockId, d: Deployment) -> bool;
    fn dip0003_enforced(&self, prev: BlockId) -> bool;
}
pub trait CheckpointView { fn highest_checkpoint_in_index(&self) -> Option<u32>; }   // forks rejected only below it (R4 B§4)
pub trait EhfSignalView { fn signal_height(&self, prev: BlockId, bit: u8) -> Option<u32>; }
pub struct BlsCtx { pub legacy: bool }          // rev1: built by for_block(h) / for_tip(view) / from a DKG base (§1.4.1); never global
pub struct ConsensusCtx<'a> {
    pub params: &'a ChainParams, pub height: u32, pub prev: Option<BlockId>,
    pub chain: &'a dyn ChainAncestry, pub deployments: &'a dyn DeploymentView, pub checkpoints: &'a dyn CheckpointView,
    pub bls: BlsCtx, pub median_time_past: i64, pub adjusted_time: i64 /* time-too-new; clock per UngatedRules.time_source */,
    pub rules: &'static UngatedRules,          // rev1 + O17: non-deployment-gated validity rules (§2.25; O17-release-profile.md §4.1)
}
pub trait SignatureChecker {                     // Core BaseSignatureChecker
    fn check_ecdsa(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8], flags: ScriptFlags) -> bool;   // sighash + sigcache
    fn check_data_sig(&self, sig: &[u8], msg: &[u8], pubkey: &[u8], flags: ScriptFlags) -> bool;        // single SHA256(msg), no sigcache
    fn check_lock_time(&self, n: &ScriptNum) -> bool;
    fn check_sequence(&self, n: &ScriptNum) -> bool;
}
pub fn verify_script(script_sig: &[u8], script_pubkey: &[u8], flags: ScriptFlags, checker: &dyn SignatureChecker) -> Result<(), ScriptError>;
pub fn signature_hash(script_code: &[u8], tx: &Tx, n_in: usize, hash_type: u32) -> Hash256;   // legacy; vExtraPayload; SINGLE bug → ONE
pub trait CoinsView { fn coin(&self, op: &OutPoint) -> Option<Coin>; }
pub fn check_transaction(tx: &Tx) -> Result<(), ValidationError>;
pub fn contextual_check_transaction(tx: &Tx, ctx: &ConsensusCtx) -> Result<(), ValidationError>;
pub fn check_tx_inputs(tx: &Tx, view: &dyn CoinsView, spend_height: u32) -> Result<Amount, ValidationError>;  // asset unlock → payload fee
pub fn calculate_sequence_locks(tx: &Tx, flags: LockTimeFlags, prev_heights: &[u32], ctx: &ConsensusCtx) -> SequenceLocks;
pub fn check_block_header(h: &BlockHeader, params: &ChainParams) -> Result<(), ValidationError>;
pub fn check_block(b: &Block, params: &ChainParams, check_pow: bool, check_merkle: bool) -> Result<(), ValidationError>;
pub fn contextual_check_block_header(h: &BlockHeader, ctx: &ConsensusCtx) -> Result<(), ValidationError>;
pub fn contextual_check_block(b: &Block, ctx: &ConsensusCtx) -> Result<(), ValidationError>;
pub fn block_script_flags(ctx: &ConsensusCtx) -> ScriptFlags;     // P2SH | DERSIG@bip66 | CLTV@bip65 | CSV@csv | NULLDUMMY@bip147
pub fn get_next_work_required(chain: &dyn ChainAncestry, last: BlockId, header: &BlockHeader, p: &ChainParams) -> CompactTarget;
pub fn block_subsidy_inner(prev_bits: CompactTarget, prev_height: u32, v20: bool, p: &ChainParams) -> Amount;
pub fn masternode_payment(height: u32, block_value: Amount, era: MnRewardEra, p: &ChainParams) -> Amount;
pub fn versionbits_state(prev: BlockId, dep: &Bip9Deployment, chain: &dyn ChainAncestry, ehf: &dyn EhfSignalView, cache: &mut VbCache) -> ThresholdState;

// ---- dash-evo ----
pub trait ChainLockVerifier { fn verify_chain_lock(&self, height: u32, hash: &BlockHash, sig: &[u8; 96], ctx: &BlsCtx) -> bool; }
pub trait QuorumMemberSource { fn members(&self, t: LlmqType, base: BlockId) -> QuorumMembersResult; }
pub struct QuorumMembersResult { pub members: Vec<Arc<Dmn>> }   // rev1: a new snapshot is emitted as Effect::StoreQuorumSnapshot (§2.10.1)
pub struct EvoCtx<'a> { pub cons: &'a ConsensusCtx<'a>, pub check_sigs: bool /* false below assumevalid; never gates AU quorum or MNHF sigs (m-1) */ }
pub fn process_special_txs_in_block(block: &Block, coins: &dyn CoinsView, store: &dyn EvoStore, cl: &dyn ChainLockVerifier,
                                    ctx: &EvoCtx, out: &mut EvoDelta, effects: &mut dyn EffectSink   // rev1: survives Err
                                    ) -> Result<EvoBlockResult, ValidationError>;
pub struct EvoBlockResult { pub mn_list: Arc<MnList>, pub payee: Option<Arc<Dmn>>, pub commitments: Vec<(FinalCommitment, BlockHash)>,
                            pub credit_pool: CreditPool, pub signals: Signals, pub bls_legacy_after: bool }
pub fn is_block_value_valid(block: &Block, height: u32, reward: Amount, sb: &SuperblockState, ctx: &ConsensusCtx) -> Result<(), ValidationError>;
pub fn is_block_payee_valid(cb: &Tx, list: &MnList, height: u32, reward: Amount, check: SuperBlockCheck, sb: &SuperblockState, ctx: &ConsensusCtx) -> Result<(), ValidationError>;

// ---- dashd-chainstate ----
pub trait ChainHelper: Send + Sync {                // Core CChainstateHelper
    fn has_conflicting_chainlock(&self, height: u32, hash: &BlockHash) -> bool;
    fn has_chainlock(&self, height: u32, hash: &BlockHash) -> bool;
    fn best_chainlock_height(&self) -> Option<u32>;
    fn conflicting_islock(&self, tx: &Tx) -> Option<Arc<InstantSendLock>>;   // consulted only outside IBD
    fn remove_conflicting_islock(&self, tx: &Tx);                             // a ChainLocked block overrides an islock
    fn is_mn_sync_finished(&self) -> bool;                                     // superblock NoCheck path
    fn superblock(&self, height: u32) -> SuperblockState;                      // NotTriggered | Triggered(Arc<Superblock>) | GovernanceInvalid
    fn payments_limit(&self, height: u32) -> Amount;
}
pub struct NoOpChainHelper; pub struct ReplayChainHelper { /* recorded dashd decisions per height */ }
pub trait MempoolSink: Send + Sync { fn remove_for_block(&self, block: &Block, height: u32); fn readd_disconnected(&self, txs: Vec<TxRef>); }

// ---- dashd-mempool ----
pub struct Policy { pub min_relay_fee: FeeRate, pub dust_relay_fee: FeeRate, pub incremental_relay_fee: FeeRate, pub max_mempool: usize,
                    pub expiry: Duration, pub limit_ancestors: usize, pub limit_ancestor_size: usize, pub limit_descendants: usize,
                    pub limit_descendant_size: usize, pub datacarrier: bool, pub datacarrier_size: usize, pub permit_bare_multisig: bool,
                    pub require_standard: bool, pub bytes_per_sigop: usize }

// ---- dash-p2p-transport / dashd-p2p ----
pub trait Transport: Send { fn poll_recv(&mut self, buf: &[u8]) -> TransportEvent; fn encode(&mut self, msg: &P2pMsg, ctx: &ProtoCtx) -> Bytes; fn kind(&self) -> TransportKind; }
// rev1 + O17: v1's ProtocolProfile is generalised to the layered release profile (§2.25; O17-release-profile.md §3–§4).
// dashd-p2p owns Identity (protocol/UA/client versions, MN minimum) and P2pRules; dashd_node::ReleaseProfile aggregates every group.
pub struct ProtoCtx { pub version: u32, pub bls: BlsCtx /* + the plain P2P rule values a codec needs (e.g. a QRINFO cap, msg_asset_unlock), passed in by dashd-p2p; negotiated peer features. base-sdk cannot name `Release` */ }
// Effect and EffectSink: §2.10.1. BlsEager/BlsLazy/SchemeSource: §1.4.1. Views (ChainView, EvoReadView): §2.10.2.
pub struct PeerCtx { pub id: PeerId, pub addr: SocketAddr, pub conn_type: ConnectionType, pub services: ServiceFlags, pub version: u32,
                     pub masternode: MasternodeConn /* none | probe | verified{protx} | iqr */, pub permissions: Permissions }
// NetHandler and MsgOutcome: §2.14

// ---- dashd-rpc ----
pub trait RpcMethod: Send + Sync { fn spec(&self) -> &'static RpcSpec; fn call(&self, cx: &RpcCtx, req: RpcRequest) -> Result<OrderedJson, RpcError>; }

// ---- dashd-wallet / key-wallet-manager ----
// WalletInterface additions: §2.20
```

## Appendix B: Dash Core source → Rust crate/module map

`★` marks rows with oracle entry points (§4.4).

| Core source (`src/`) | Rust home |
|---|---|
| `script/interpreter.*`, `script/script.*`, `script/script_error.h` ★ | `dash-consensus::script::{interpreter, num, error}` |
| `script/sigcache.*`, `cuckoocache.h`, `checkqueue.h` | `dashd-chainstate::{sigcache, checkqueue}` |
| `script/standard.*`, `script/descriptor.cpp` (subset) | `dashd-mempool::policy::standard`; key-wallet descriptors |
| `consensus/tx_check.cpp`, `tx_verify.cpp`, `consensus.h`, `merkle.cpp` ★ | `dash-consensus::{tx, block::merkle}` |
| `pow.*`, `arith_uint256.*`, `hash_x11.h`, `crypto/x11/*` ★ | `dash-consensus::{pow, fp}`, `dash-num`, `dash-pow` |
| `validation.cpp`: `GetBlockSubsidy*`, `ContextualCheck*`, `GetBlockScriptFlags`, `CheckBlock` ★ | `dash-consensus::{subsidy, tx::contextual, block}` |
| `validation.cpp`: `ConnectBlock`, `DisconnectBlock`, `ActivateBestChain*`, `AcceptBlock*`, `FlushStateToDisk`, `LoadExternalBlockFile`, `VerifyDB`, `EnforceBlock`, `MarkConflictingBlock`; `chain.*`, `node/chainstate.*`, `kernel/*` | `dashd-chainstate::{connect, disconnect, activate, accept, flush, import, verify, chainlock}` |
| `validation.cpp` `MemPoolAccept`, `txmempool.*`, `policy/*`, `txorphanage.*`, `kernel/mempool_*` | `dashd-mempool::{accept, pool, policy, fees, orphanage, persist}` |
| `coins.*`, `txdb.*`, `undo.h`, `compressor.*`, `dbwrapper.*` ★ | `dash-consensus::coins` (codecs), `dashd-chainstate::coins` (cache), `dashd-store` |
| `node/blockstorage.*`, `flatfile.*` | `dashd-store::flatfile`, `dashd-chainstate::blockstore` |
| `versionbits.*`, `deploymentstatus.*`, `deploymentinfo.*` ★ | `dash-consensus::deployments` |
| `chainparams*.cpp`, `chainparamsseeds.h`, `llmq/params.h` | `dash-params` (BS5) + `dash-args` (overrides) |
| `evo/providertx.*`, `evo/providertx_util.cpp`, `evo/specialtx.*`, `evo/sharedcollateral.h`, `evo/types.h`, `evo/dmn_types.h` ★ | `dash-primitives::payload` (BS6a/b) + `dash-evo::specialtx` |
| `evo/specialtxman.*` ★ | `dash-dml::specialtx::block` (`process_special_txs_in_block`, `build_new_list_from_block`) |
| `evo/deterministicmns.*`, `evo/dmnstate.*` ★ | `dash-evo::dml::{list, state, diff, payee, pose}` + `dashd-chainstate::evo` (store, caches) |
| `evo/simplifiedmns.*`, `evo/smldiff.*` ★ | `dash-evo::sml` (+ serving in `dashd-p2p`) |
| `evo/cbtx.*` ★ | `dash-evo::cbtx` |
| `evo/creditpool.*`, `evo/assetlocktx.*` ★ | `dash-evo::{creditpool, assetlock}` |
| `evo/mnhftx.*` ★ | `dash-evo::mnhf` |
| `evo/netinfo.*` ★ | `dash-primitives` types + `dash-evo::netinfo` validation |
| `evo/evodb.*` | `dashd-chainstate::evo` over the `chainstate/` CFs; import in `dash-coreimport::evodb` |
| `evo/mnauth.*` | `dashd-masternode::mnauth` |
| `evo/specialtx_filter.cpp`, `blockfilter.*`, `index/blockfilterindex.*` ★ | `dashd-indexes::{filter, blockfilter}` |
| `evo/providertx_service.cpp` | key-wallet ProTx builders + `dashd-wallet::protx` |
| `evo/snapshot.*` | the canonical MN-list encoding in `dash-evo::dml::snapshot` (assumeutxo not ported) |
| `llmq/commitment.*`, `llmq/blockprocessor.*` ★ | `dash-evo::llmq::{commitment, blockprocessor}` |
| `llmq/utils.cpp` (members, modifiers), `llmq/snapshot.*` ★ | `dash-evo::llmq::members` |
| `llmq/utils.cpp` (connections) | `dash-llmq::topology` |
| `llmq/quorumsman.*` (`ScanQuorums`, `SelectQuorumForSigning`, `VerifyRecoveredSig`), `llmq/signhash.*` ★ | `dash-evo::llmq::{scan, selection, signhash}` |
| `llmq/quorumsman.*` (rest), `llmq/quorums.*`, `llmq/net_quorum.*` | `dashd-quorums::{manager, net_quorum, recovery}` |
| `llmq/dkgmessages.h`, `dkgsession*`, `dkgsessionhandler*`, `dkgsessionmgr*`, `net_dkg.*`, `active/dkgsession*`, `llmq/debug.*` | `dash-p2p-core` (BS9a) + `dash-llmq::dkg` + `dashd-quorums::dkg::{handler, manager, net, debug}` |
| `llmq/signing.*`, `signing_shares.*`, `net_signing.*` | `dash-llmq::sigshares` + `dashd-quorums::signing::{recovered, shares, net}` |
| `llmq/ehf_signals.*` | `dashd-quorums::ehf_signer` |
| `llmq/quorumproofs*` | `dashd-quorums::proofs` |
| `chainlock/*` ★ (`clsig` request ID) | `dash-evo::llmq` (verify) + `dashd-locks::chainlock::{handler, signer}` |
| `instantsend/*` ★ (request IDs) | `dash-evo::llmq` (verify) + `dashd-locks::instantsend::{manager, net, signer, db}` |
| `bls/bls.*`, `bls/bls_worker.*`, `bls/bls_batchverifier.h`, `bls/bls_ies.*`, `dashbls/` ★ | `dash-pkc` (BS11a/b) + `dashd-quorums::bls_worker` |
| `masternode/payments.*` ★ | `dash-consensus::payments` (arithmetic) + `dash-evo::payments` |
| `masternode/meta.*`, `masternode/sync.*`, `masternode/utils.*`, `node/sync_manager.*`, `netfulfilledman.*`, `active/masternode.*`, `active/context.*` | `dashd-masternode` |
| `spork.*` ★ (signature forms) | `dashd-masternode::sporks` |
| `governance/*` ★ (hashes) | `dash-governance` + `dashd-governance` |
| `coinjoin/{common,coinjoin,server,options,util}.*` ★ (hashes, in/outs) | `dash-coinjoin` + `dashd-coinjoin` |
| `coinjoin/{client,walletman}.*`, `wallet/coinjoin.*` | `dash-coinjoin::client` + `dashd-wallet::coinjoin` |
| `net.*`, `netbase.*`, `netaddress.*`, `net_permissions.*`, `node/eviction.*`, `node/connection_types.*` | `dashd-p2p::net::{connman, netaddress, permissions, eviction}` |
| `bip324.*`, `crypto/{chacha20,poly1305,ellswift,hkdf}` | `dash-p2p-transport::v2` (`bip324` crate, Dash salt) |
| `addrman*`, `addrdb.*`, `banman.*`, `netgroup.*` | `dashd-p2p::net::{addrman, banman, netgroup}` |
| `protocol.*`, `version.h` | `dash-p2p-core` (BS7–BS10) + `ReleaseProfile` |
| `net_processing.*`, `txrequest.*`, `blockencodings.*`, `merkleblock.*`, `common/bloom.*` | `dashd-p2p::processing::{peerman, request, cmpct, bloom, serve, blocksync}` |
| `i2p.*`, `torcontrol.*`, `mapport.*` | `dashd-p2p::net::{i2p, tor, mapport}` |
| `index/{base,txindex,addressindex*,spentindex*,timestampindex*,coinstatsindex}.*` | `dashd-indexes::*` |
| `node/miner.*`, `rpc/mining.cpp` | `dashd-mining` + `dashd-rpc::mining` |
| `rpc/server.*`, `rpc/request.*`, `rpc/util.*`, `httprpc.*`, `httpserver.*`, `rpc/client.cpp` | `dash-rpc-spec` + `dashd-rpc::{server, dispatch, help, auth, http}` |
| `rpc/{blockchain,mempool,net,node,rawtransaction,output_script,fees,signmessage,txoutproof,masternode,coinjoin,governance,evo,quorums}.cpp`, `core_write.cpp`, `evo/core_write.cpp`, `llmq/core_write.cpp`, `governance/common.cpp` | `dashd-rpc::<category>` |
| `wallet/rpc/*.cpp` | `dashd-wallet::rpc` |
| `rest.cpp`, `zmq/*` | `dashd-rpc::rest`, `dashd-rpc::zmq` (feature `zmq`) |
| `init.cpp`, `init/*`, `common/args.*`, `common/settings.*`, `logging*`, `scheduler.*`, `shutdown.*`, `timedata.*`, `warnings.*`, `stats/*` | `dash-args` + `dashd-node::{init, settings, logging, scheduler, shutdown, clock, stats}` |
| `wallet/{wallet,scriptpubkeyman,walletdb,bip39,hdchain,crypter,db,bdb,sqlite,dump,coinselection,spend,receive,fees}.*` | key-wallet(-manager) (extended) + `dashd-wallet::{db, crypto}` + `dash-coreimport::wallet` |
| `bitcoin-cli.cpp`, `bitcoin-tx.cpp`, `bitcoin-util.cpp`, `bitcoin-wallet.cpp`, `wallet/wallettool.*` | `rdash-cli` (+ `tx`, `util` subcommands), `rdash-wallet` |
| `test/functional/*` | run as-is through `node/func-compat`; `dash-testkit` mirrors DTF helpers |
| `src/test/data/*`, `test/functional/data/*` | `test-vectors/core/<commit>/` |
| `bench/*` | B1 `microbench` (dashd) + per-crate criterion benches |

## Appendix C: consolidated on-disk layout

| Store | Key | Value | Format owner |
|---|---|---|---|
| `blocks/blk%05u.dat` | — | `magic ‖ u32 size ‖ block` (Core's re-serialization) | **Core-identical** |
| `blocks/rev%05u.dat` | — | `magic ‖ u32 size ‖ CBlockUndo ‖ SHA256d(prev_hash ‖ undo)` | **Core-identical** |
| `chainstate/` CFs | §2.9 table | §2.9 table | ours (schema v1; Coin bytes and the canonical MN-list encoding reused) |
| `llmq/` | recsig: `(t, id[, msg])`, `h`, `sh`, `(BE32 t, t, id)`, votes, Platform-type entries; IS: lock by hash, by txid, by input, mined `(BE32 ~h, h)`, archived; DKG: `(qdkg_V\|S\|E, t, base, protx)`; quorum vvec / sk-share by `SHA256d(type ‖ quorumHash ‖ members)` | Core's value encodings (vvec always basic) inside our versioned keys | ours (Core's `llmq/` importable) |
| `indexes/txindex/` | `txid[0..8]` | `[(u32 file, u32 block_pos, u32 tx_offset)]` | ours |
| `indexes/addressindex/`, `spentindex/`, `timestampindex/` | Core's key layouts (`CAddressIndexKey`, `CAddressUnspentKey`, `CSpentIndexKey`, `{BE32 time, hash}`) | Core's values | Core layout (for RPC order) |
| `indexes/blockfilter/basic/` | `db/`: height → `{filter_hash, header, pos}`, hash → same, `'V' = 2`; `fltr%05u.dat`: `hash32 ‖ filter` | — | ours (Core-like records) |
| `indexes/coinstats/` | height and hash → `{muhash, totals, block_info}` | MuHash3072 state | ours |
| `wallets/<name>/wallet.sqlite3` | tables of §2.20 | encrypted secrets (argon2id + XChaCha20-Poly1305) | ours |
| `peers.dat`, `anchors.dat`, `mempool.dat`, `fee_estimates.dat`, `sporks.dat`, `netfulfilled.dat`, `mncache.dat`, `governance.dat` | — | `magic ‖ our version string ‖ body ‖ SHA256d`; Core's formats accepted on load | ours |
| `banlist.json`, `settings.json`, `dash.conf` | — | Core JSON / conf | Core |
| `message_capture/` | — | Core capture format | Core |

## Appendix D: functional-test adoption by milestone

Bold marks Dash-specific tests. The lists are the starting passlists; FT-MAP (task O11) maps each test to story IDs.

| Milestone | Tests expected to pass |
|---|---|
| M2 | `feature_filelock`, `feature_shutdown`, `feature_logging`, `feature_config_args`, `feature_includeconf`, `feature_settings`, `interface_rpc`, `interface_http`, `rpc_users`, `rpc_whitelist`, `rpc_named_arguments`, `rpc_help`, `rpc_uptime`, `rpc_bind` (partial) |
| M3 (tier 1, non-evo) | `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_nulldummy`, `feature_bip68_sequence`, `feature_assumevalid`, `feature_minchainwork`, `feature_maxtipage`, `feature_reindex`, `feature_loadblock`, `feature_blocksdir`, `feature_abortnode`, `feature_dbcrash`, `p2p_invalid_block`, `p2p_unrequested_blocks`, `p2p_mutated_blocks`, `p2p_dos_header_tree`, `p2p_sendheaders`, **`p2p_sendheaders_compressed`**, `p2p_block_sync`, `p2p_initial_headers_sync`, `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `rpc_blockchain` (read), `rpc_generate`, `mining_basic` (regtest) |
| M4 (tier 1, evo) | **`feature_new_quorum_type_activation`**, `feature_utxo_set_hash`, `feature_coinstatsindex`, `rpc_getblockstats`, `rpc_dumptxoutset`, `feature_versionbits_warning`, **`feature_dip4_coinbasemerkleroots`** (mixed A: rdashd non-MN) |
| M5 (services, tier 3, mixed A) | `mempool_*` (15), `mining_*`, `rpc_rawtransaction`, `rpc_decodescript`, `rpc_createmultisig`, `rpc_deriveaddresses`, `rpc_getdescriptorinfo`, `rpc_signmessagewithprivkey`, `rpc_signrawtransactionwithkey`, `rpc_txoutproof`, `rpc_misc`, `rpc_deprecated`, `rpc_packages`, `rpc_mempool_info`, `rpc_estimatefee`, `rpc_scanblocks`, `rpc_scantxoutset`, `rpc_getblockfilter`, `rpc_getblockfrompeer`, `rpc_getmerkleblocks`, `rpc_invalid_address_message`, `interface_bitcoin_cli`, `interface_zmq`, **`interface_zmq_dash`**, `interface_rest`, `feature_txindex`, **`feature_addressindex`**, **`feature_spentindex`**, **`feature_timestampindex`**, `feature_fee_estimation`, `feature_notifications`, `feature_startupnotify`, `feature_pruning`, `feature_index_prune`, `feature_fastprune`, `p2p_filter`, `p2p_blockfilters`, `p2p_compactblocks*`, `p2p_blocksonly`, `p2p_tx_download`, `p2p_tx_privacy`, `p2p_leak_tx`, `p2p_getdata`, `p2p_nobloomfilter_messages`, `p2p_ibd_txrelay`, `p2p_ibd_stalling`, `p2p_node_network_limited`, `p2p_invalid_messages`, `p2p_invalid_locator`, `p2p_invalid_tx`, `p2p_leak`, `p2p_fingerprint`, `p2p_ping`, `p2p_timeouts`, `p2p_disconnect_ban`, `p2p_eviction`, `p2p_add_connections`, `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_addrfetch`, `p2p_dns_seeds`, `p2p_getaddr_caching`, `p2p_permissions`, `p2p_v2_transport`, `p2p_v2_encrypted`, `p2p_v2_misbehaving`, `p2p_message_capture`, **`p2p_connect_to_devnet`**, `feature_anchors`, `feature_addrman`, `feature_asmap`, `rpc_net`, `rpc_setban`, `rpc_getpeerinfo_deprecation`, **`feature_sporks`**, **`feature_governance_objects`**, **`p2p_govsync_bloom`**, **`p2p_dstx`** (relay), `feature_proxy`, plus every DTF test in mixed mode A |
| M6 (tier 4) | `wallet_*` (58, minus the BDB-write-only cases in the skip list), **`rpc_coinjoin`**, **`wallet_dash_rpcs`**, **`wallet_dust_protection`**, **`wallet_keypool_hd`**, **`wallet_mnemonicbits`**, **`wallet_upgradetohd`**, **`wallet_orphanedreward`**, **`rpc_wipewallettxes`**, `rpc_psbt`, `tool_wallet` (subset), `wallet_backwards_compatibility` (read side) |
| M7 (tier 2, mixed B, all-Rust) | **`feature_dip3_deterministicmns`**, **`feature_dip3_v19`**, **`feature_dip4_coinbasemerkleroots`**, **`feature_llmq_signing`**, **`feature_llmq_chainlocks`**, **`feature_llmq_chainlocks_automatic`**, **`feature_llmq_rotation`**, **`feature_llmq_connections`**, **`feature_llmq_data_recovery`**, **`feature_llmq_dkg_intake`**, **`feature_llmq_dkgerrors`**, **`feature_llmq_is_retroactive`**, **`feature_llmq_is_cl_conflicts`**, **`feature_llmq_evo`**, **`feature_llmq_singlenode`**, **`feature_llmq_simplepose`**, **`feature_mnehf`**, **`feature_asset_locks`**, **`feature_governance`**, **`feature_governance_cl`**, **`feature_governance_txindex_devnet`**, **`feature_masternode_params`**, **`feature_masternode_params_devnet`**, **`feature_masternode_shares`**, **`feature_masternode_payout_shares`**, **`feature_protx_version`**, **`p2p_instantsend`**, **`p2p_quorum_data`**, **`p2p_platform_ban`**, **`rpc_quorum`**, **`rpc_verifychainlock`**, **`rpc_verifyislock`**, **`rpc_masternode`**, **`rpc_mnauth`**, **`rpc_netinfo`**, **`rpc_external_queue`**, **`feature_quorum_proof_chain`**, **`feature_stats`**, `feature_maxuploadtarget` |
| M8 | `feature_remove_pruned_files_on_startup`, `feature_unsupported_utxo_db`, `feature_dirsymlinks`, `feature_posix_fs_permissions`, `feature_bind_*`, `feature_discover`, `feature_help`, `p2p_i2p_*`, the remaining tier 1–4 tests |

**Permanent skips** (pasta-approved classes, DEC-10): `interface_usdt_*` (eBPF), multiprocess (`echoipc`), Qt-only behaviour, tests that write BDB wallets through the node, and `wallet_signer`/`rpc_signer` until an external-signer bridge exists (SHOULD).

## Appendix E: implementation conventions (carried into every brief)

- **Port anchors.** Every ported function has a `// Core: <file>:<line> <Function>` anchor pinned to the milestone's Core commit.
- **★ PR descriptions** list:
  - the L0 vectors, L1 targets, L2 windows and L3 operators the change touches;
  - the quirks it touches;
  - whether a reference twin exists;
  - the PRD IDs it implements.
- **Strings.** Reject reasons, RPC error strings and help text are copied from Core verbatim, never paraphrased.
- **Determinism.** No `HashMap` iteration on consensus paths; no floating point outside `dash_consensus::fp`; no `std::time` outside `NodeClock`.
- **Errors and logging.** Errors are `thiserror` enums with Core's class and string. Logging uses `tracing` targets equal to Core's `-debug` categories.
- **Features.** `zmq` (on `dashd-rpc`), `wallet`, `x11-c`, `oracle`, `xcheck`, `debug-rpcs` (hidden/test RPCs, on by default on regtest).
- **Unsafe.** `#![forbid(unsafe_code)]` except `dashd-store` and `dash-oracle-sys`.
- **Rev1 conventions.**
  - A function that can produce one of Core's non-transactional effects takes `&mut dyn EffectSink` and never returns effects inside its `Result` (§2.10.1).
  - Every BLS field uses `BlsEager`/`BlsLazy` with its scheme source, and every call site takes an explicit `BlsCtx` (§1.4.1).
  - Every behaviour that differs between the pinned releases reads `ReleaseProfile`, never a hard-coded constant (§2.25).
  - Durable writes follow §3.5's order. No code outside `dashd-chainstate::flush` commits a batch that references flat-file positions.
- **Tests and benches.** Unit tests sit next to code; vectors and differential tests live in `dash-conformance`, the pure crates' `tests/`, and `node-fuzz`. Each crate has a criterion bench for its hot path, wired into B1's microbench comparison.
- **base-sdk PRs** follow base-sdk conventions (`namespace%type(scope):` commits, 2-space style, `just test lint`, no `unwrap`/`panic`/`unsafe` without a reasoned `#[expect]`).
- **Upstream boundaries.**
  - No changes under `dash-spv/src/network/`.
  - No public-API breaks in `dashcore`, `key-wallet` or `key-wallet-manager` without a consumer-impact note.
  - Code under PLAN §7 handling carries the `EMBARGO-1` marker and follows PLAN §7.
- **Docs.** Each crate's `ARCHITECTURE.md` lists the Core files it mirrors and its deviations; stale docs are a review finding (R1 §10).

## Errata (O2a review, 2026-10-08)

Notes from the O2a (oracle shim v2) review and the coverage matrix's contradictions table (`compat/COVERAGE.md` §21.3, on the O2a branch). The body text above is unchanged; where it disagrees with an entry here, the entry wins.

- **§4.4 `oracle_result.dos` (C5).** There is no `dos` field. Neither Core pin carries a DoS score: the shim returns `cls` (the `TxValidationResult` or `BlockValidationResult` class), `reason` and `debug`, and the penalty derives from the class in net_processing, so L3's P2P leg is the only penalty check. `oracle_verify_commitment` returns a bool plus a log line, because Core's `Verify` is a bool. Its debug message is pin-specific log text: compare it per pin, never across pins.
- **"13 types" (C6).** Wherever the text says 13 special-tx types (§1.1's type-stack table, the `dash-evo` row in §2, §1.7's BS6b row, §4.4's `diff_payload_<type>` row), read **12 types (1–12; 10–12 v24 only)**. Core v24 has 12 special types and v23.1.8 has 9 (types 1–9).
- **§4.4 v2 entry-point list (C8).** O2a ships only part of the list placed in its v2 ABI. It adds `oracle_payload_decode` (not in the original v2 list; BS4b and BS6b need it at M1). The rest is owned elsewhere:
  - `oracle_dml_block`, the list entry points, `oracle_sml_entry_hash` and `oracle_special_tx_sign_hash`: O2a-EV.
  - `oracle_quorums_root`, `oracle_sign_hash` and `oracle_request_id`, plus the block-path variants of `oracle_check_special_tx` (MNHF and asset unlock): O2a-LQ1.
  - `oracle_block_value_payee` and `oracle_credit_pool_step`: O2a-GV.
  - `oracle_bls` (multi-op): BS12.
  - `oracle_versionbits_state`: CS4.
  - `oracle_quorum_members` and `oracle_select_quorum` are replaced by LQ1's `oracle_llmq_*` context API (O2a-LQ1).
  - `oracle_check_special_tx` runs the mempool path (no chain, no asset-unlock index set); see PLAN EV1.
- **Appendix D, mixed mode A placement (C10).** The M4 row lists `feature_dip4_coinbasemerkleroots` as "(mixed A: rdashd non-MN)", but §4.8 schedules mixed mode A at M5 (the M5 row is headed "mixed A"). Until the M4 entry is re-homed, read it as running in mixed A from M5; the all-Rust run stays in the M7 row.

## Errata (EV0 API review, 2026-10-09)

- **§2.10.1, list-changed notification:** besides VerifyDB level 4, **level 3** notifies a masternode-list change with `undo = true` through `DisconnectBlock` (v24.0.0-rc.3 `validation.cpp:2368`, v23.1.8 `:2157`). The listener uses it to disconnect masternode-authenticated peers, which is visible through `verifychain` and at every startup. The list-changed `Effect` therefore carries an `undo` flag (EV0 M3).

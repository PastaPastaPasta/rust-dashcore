# R3: Base SDK, what it is and how far rust-dashcore has adopted it

**Date:** 2026-10-08
**Snapshots used:**
- base-sdk `develop` @ `33fcc59` (2026-10-04)
- rust-dashcore `dev` @ `67b307c6` (2026-10-07)
- platform `v5.0-dev` @ `7b61d6f3de`
- dash-evo-tool `v1.0-dev` @ `e4850fca`
- dash `develop` @ `15db478750`

**Method:** read-only review of both repos (every package under `base-sdk/pkgs/`, the docs, `maint/` and `contrib/`), GitHub PR and issue history for base-sdk, rust-dashcore, platform, dash-evo-tool, dash and bls-signatures, and downstream greps. Nothing was modified.

**Not checked:** the item lists on the dashpay project boards #13 ("base-sdk v0.1") and #14 ("base-sdk v0.2"). This box's `gh` token lacks `read:project`, and the boards render client-side. I used milestones and PR labels instead.

---

## 0. TL;DR

- **What base-sdk is.** A `no_std` + `alloc`, MIT, edition-2021 (MSRV 1.85) family of 9 crates for **parsing and stateless verification** of Dash L1 data. It is designed and built essentially single-handedly by **kwvg** (Kittywhiskers Van Gogh, a Dash Core developer).
  - Its docs say plainly that it is *not a full node*, and point to rust-dashcore or Dash Core for "batteries-included" use.
  - It builds on the **new rust-bitcoin 1.x ecosystem**: `bitcoin_hashes` 1.2, `bitcoin-primitives` 0.103, `bitcoin-consensus-encoding` 1.3 and `bitcoin-units` 0.5. rust-dashcore is still on the 0.14-era `bitcoin_hashes`.
- **Maturity is split in two.**
  - **Released** (0.1.0-beta on crates.io, 2026-09-15/17): `dash-types`, `dash-types-marker`, `dash-num`, `dash-pkc`.
  - **Incubating** (0.0.0, unpublished, no stability promise): `dash-pow`, `dash-script`, `dash-primitives`, `dash-params`, `dash-p2p-core`.
- **Integration status in rust-dashcore** is roughly *"crypto backends done, everything else not started"*.
  - **Done** (kwvg, Sep–Oct 2026):
    - #1036: BLS moved to `dash-pkc` in a new `dashcore-crypto` crate.
    - #1056: EdDSA.
    - #1108, #1115: workspace consolidation, re-exporting unchanged rust-bitcoin code, `hex-conservative` 1.x.
  - **In flight:**
    - #1110 (open): drop `dashcore_hashes::hex`.
    - #1142 (draft): `dash_num::Arith256` replaces the private `U256` in `pow.rs`. It needs a base-sdk re-pin past base-sdk#54.
  - **Not started:** hashes, Amount, Script, Address, Network/chain params, Tx/Block, special payloads, consensus codec, P2P messages.
  - **Footprint:** only **2 source files** (`crypto/src/bls.rs` and `crypto/src/eddsa.rs`) import base-sdk today.
- **base-sdk is behind rust-dashcore in protocol coverage.** No ChainLock, InstantLock, QRInfo or SML engine; no BIP158 or bloom types; no sighash; no Devnet; no V1 P2P framing; most P2P messages, including `block` and `tx`, are raw-byte stubs; no signature verification inside primitives.
- **Both libraries are behind Dash Core v24:** CbTx v4, asset-unlock v2 txid, special-tx types 10–12, the full ProRegTx v3 with DIP-26 payouts and shares, and AssetLock v2.
- **There is no C ABI or cxx bridge in base-sdk.** It has no code or written plan for Dash Core to consume it. The only Dash Core groundwork is kwvg's **dash#7109**, a draft for Rust-in-depends with `cxx`, which needs a rebase and has changes requested. A competing C++ path (knst's dash#7678–7680, moving Core's BLS to `blst`) is open.
- **Downstream exposure.**
  - **Platform** pins rust-dashcore by git rev. Its v5.0-dev line is *before* #1036. Its v5.1 stack (#5307, lklimek) already adopted `dash-pkc` at the same base-sdk rev, but does not yet include #1108 or #1115.
  - **Platform** uses `dashcore::` about 2,100 times across 628 files. **dash-evo-tool** uses it about 630 times, through `dash_sdk::dpp::dashcore`.
  - **bincode persistence** (wallets, SPV storage, Platform) is the hardest compatibility constraint, because base-sdk deliberately has no bincode.
- **Recommendation.** Finish integration in **tiers**. Tier 1 is weeks of low-risk work and should be done now. Tiers 2–3 need base-sdk feature work and a cross-team architecture agreement first. A literal "replace every dashcore type with a base-sdk type" before starting node work would put the node program on the critical path of an unreleased SDK with one maintainer. §4 proposes an ordered, PR-sized plan and §5 lists the decisions needed from the program owner.

---

## 1. What base-sdk is

### 1.1 Identity, philosophy and process

| Aspect | Detail |
|---|---|
| Repo | `github.com/dashpay/base-sdk`. Default branch `develop`, 728 commits, a single branch. PRs #1–#56 are all by kwvg and all merged. CodeRabbit is the only other reviewer of note. |
| Stated purpose | "A parsing and stateless verification SDK for Dash's layer 1 blockchain." It began as an embeddable library for the Hyphen block explorer. It is described as "closer to `libbitcoinkernel` than `libbitcoin_server`" (rust-dashcore#1036 body). |
| Scope rule | `AGENTS.md`: every crate is `#![no_std]` + `alloc`; `std` is an opt-in feature; **"No networking code in any crate"**; encoding, decoding and data types only. `docs/README.md` covers only stateless or context-independent consensus and says outright that the SDK "is not a full node". |
| Layering | Foundation packages (`types`, `num`, `pow`): unopinionated Bitcoin-compatible data handling. Base packages (`script`, `pkc`): algorithms. Protocol packages (`primitives`, `params`, `p2p_core`): Dash protocol. |
| Dependency graph | `types → num, script`; `num → pkc, primitives`; `script → primitives`; `pow → primitives`; `pkc → p2p_core`; `primitives → params`; `params → p2p_core`. |
| Upstream alignment | Builds on rust-bitcoin's new crates (`bitcoin-primitives` 0.103, `bitcoin-consensus-encoding` 1.3, `bitcoin_hashes` 1.2, `bitcoin-units` 0.5, `bitcoin-internals` 0.7, `base58ck` 0.5, `hex-conservative` 1.3) instead of forking them. |
| Toolchain | Edition 2021, resolver 3, **MSRV 1.85.0**, which is capped at Debian stable's rustc and enforced in CI by `build_msrv.yml`. The dev toolchain is pinned to `nightly-2026-02-01`. Nightly is needed only for `dash-pow/simd`, the `full` feature set, the WASM solver sample and the rustfmt options. |
| Style | `docs/dev/guide_rust.md` and `AGENTS.md`: 2-space indent, width 120 and comment width 80; newtypes, and invalid states made unrepresentable; **no `unwrap`, `expect`, `panic`, `todo`, `unreachable` or `unsafe`** (denied at workspace level); error variants mirror Core's `bad-*` reject strings; Latin-1-only sources; a fixed 5-line MIT header; fixed import order; `rstest`, `divan` and JSON5 corpora. |
| Commit convention | `namespace%type[(scope)][!]: description`, enforced by `maint/lint/lint_commit.py` on PR titles and on every commit. Namespaces: `num`, `p2p_core`, `params`, `pkc`, `pow`, `primitives`, `script`, `sdk`. Types: `build`, `chore`, `ci`, `doc`, `feat`, `fix`, `lint`, `misc`, `perf`, `refac`, `test`. Maximum 73 columns and 5 lines. Examples: `sdk%lint: …`, `pkc%fix(ecdsa): …`, `num%fix(compact)!: …`. At most 100 commits per PR, and every commit must pass `just test lint` (`just comb`). |
| Lint and QA | CodeQL custom queries (required derives, declaration order, import grouping, **BLS/ECDSA API parity**, **secret dataflow** for zeroize, subtle and redaction), about 25 semgrep rules, cargo-deny (license allowlist, `hex-literal` banned), taplo, ruff, markdownlint, nix fmt, Codecov and nextest. Merge gate: `just test lint`. |
| Release | Per-crate Keep-a-Changelog files. crates.io releases are tagged `dash-<crate>-<ver>`; repo snapshots are tagged `YYYY-MM-DD`. Milestones "0.1" (47 PRs, closed) and "0.2" (6 PRs so far). Org project boards #13 and #14 are kept in sync with the PR topic labels Build/CI, Codec, Crypto, Documentation, **FFI**, P2P and Primitives. FFI is a label with no code behind it yet. |
| Open issues | #40 "Known Issues". `dash-primitives` cannot parse Asset Lock v2. Compact-filter P2P types are stubbed (a regression from base-sdk#39, which dropped the unreleased `bitcoin-p2p-messages`). One unresolved review comment (`ct_eq`). |
| C/C++ interop | **None.** There is no `extern "C"`, `#[no_mangle]`, cbindgen, cxx, staticlib or uniffi. The only `cdylib` targets are two `wasm-bindgen` doc demos (`docs/samples/{parser,solver}`). The only FFI is outbound to `blst`, isolated in `pkgs/pkc/src/bls/blst_ffi.rs`. No document describes Dash Core consuming base-sdk. The related Core work lives in dash#7109 (see §2.4). |

### 1.2 Crate-by-crate API surface

Paths are relative to `base-sdk/pkgs/`.

#### `dash-types` + `dash-types-marker` (0.1.0-beta, published): shared newtype and codec infrastructure

- **Features:** `codec` (on by default; pulls in `bitcoin-consensus-encoding`), `serde`, `bitcoin-primitives` (`ScriptBuf` adapters), `std`, `full`.
- **Codec:** `codec::BaseCodec<E>` (`types/src/codec.rs:187`), a synchronous slice-cursor codec.
  - `decode(&mut &[u8])` and `encode(&self, &mut impl EncodeBuf)`.
  - Integers are little-endian. CompactSize must be minimal. Bools are strictly 0 or 1. `Vec<T>` and `String` are length-prefixed.
  - Supporting types: `DecodeError<E>`, `EncodeBuf`, `SerBound`, `CompactSize`.
- **Bridge to rust-bitcoin:** `impl_type!` implements rust-bitcoin's `bitcoin_consensus_encoding::{Encode, Decode}` through the buffer-and-replay `VecEncoder`/`VecDecoder` (maximum 32 MiB). The secret-material versions are `ArrEncoder`/`ArrDecoder`.
- **Traits:**
  - `Checkable` returns `Option<Error>` for structural invariants.
  - `Hashable` returns the canonical hash.
  - `Numeric` handles little/big-endian byte images.
  - `TypeId` is a name-stable `u32`: xxh32 of the type name, derived by the marker crate, with a build-time collision check.
  - `Codec` is a hidden aggregate trait, and `Unencodable` is its opposite marker.
- **Macros:**
  - `make_bytes!` builds fixed byte newtypes with `fwd` or `rev` hex display, optional codec, and optional scheme generics.
  - `make_sbytes!` is the secret version: zeroize on drop, constant-time equality, redacted `Debug`, no `Copy` or serde.
  - Also `derive_bytes!`, `impl_bytes!`, `impl_type!`, `dlgt_codec!` and `dlgt_scodec!` (which delegate an operational type's codec to its byte form), `make_num!`, `impl_num!`, `enum_map!` (open enums with an `Unknown(n)` catch-all), and `type_cvrt!` (`From`/`TryFrom` pairs from one closure).
- **serde (base-sdk#51, merged after the crates.io release):** hex in human-readable formats; **raw storage-order bytes in binary formats**. This matches Core's `uint256` serialization.

#### `dash-num` (0.1.0-beta, published): hash blobs and 256-bit arithmetic

- `HashBlob<N>` (`num/src/hash.rs:26`) stores bytes little-endian and displays them **reversed** (big-endian hex), like Core's `GetHex`. `Hash160` and `Hash256` are aliases.
- `make_hash!` declares a hash newtype over `HashBlob` with codec, serde, `Numeric`, `from_hex` and `from_hex_lossy`. It is used for `BlockHash`, `TxHash`, `MerkleRoot`, `QuorumHash` and others in primitives.
- `Arith256` (`arith256.rs:42`) wraps `bitcoin_internals::u256::U256`. It has wrapping operators, `u64`/`u32` operand overloads matching Core, `to_f64` with Core's rounding, `block_proof`, and `from_hex_lossy` (Core's `SetHexDeprecated`). It has **no wire codec**; it converts to and from `Hash256`.
- `CompactTarget(u32)` (`compact.rs:21`) with `expand() -> DecodedTarget {value, negative, overflow}` and `block_proof()`.
- **Corpus:** `arith256.json5`, generated from **Dash Core v24.0.0-rc.1**. The C++ generator is embedded in the file.
- **Unreleased since 0.1.0-beta:** base-sdk#54 (`Arith256` behaviour fixes and delegation to bitcoin-internals). rust-dashcore #1142 depends on it.

#### `dash-pkc` (0.1.0-beta, published): BLS, ECDSA and EdDSA

- **Two-layer design.**
  - Always-compiled **byte bags**: `*Bytes`, `*PkHash`, `BlsShareId`. Parsing therefore needs no crypto backend.
  - Feature-gated **operational types**: `bls` (blst 0.3 + ff/group 0.14), `ecdsa` (secp256k1 **0.33.1**), `eddsa` (ed25519-dalek 3).
- **BLS** (`pkc/src/bls/`):
  - The scheme is a type parameter: `BlsScChia` (legacy) or `BlsScIetf`. The `BlsScheme` trait is sealed and doc-hidden.
  - Types: `BlsPkBytes<S>` (48 B), `BlsSigBytes<S>` (96 B), `BlsSkBytes<S>` (32 B, secret), `BlsPkHash<S>`, `BlsDhBytes<S>`, `BlsShareId`, `BlsSigId`, `BlsError`.
  - `BlsPublicKey<S>`: `verify`, `aggregate`, `secure_aggregate`, `to_scheme` (converts legacy and IETF), add/mul tweaks, `negate`, `derive_share` (Core's `BuildPubKeyShare`), `recover_shares`, `ies_encrypt`, and IETF PoP.
  - `BlsSecretKey<S>`: `sign`, `split` (threshold), `derive_share`, `dh_exchange`, `ies_decrypt`, and tweaks.
  - `BlsSignature<S>`: `aggregate`, `fast_verify_aggregates`, `verify_aggregates`, `secure_*`, `recover_shares`, `sub_insecure`.
  - Shares: `Bls{Sk,Pk,Sig}Share<S>`.
  - **BLS-IES:** `BlsIesBlob` and `BlsIesMulti` (AES-256-CBC, sha256d IV chain, at most 2048 recipients).
  - Thin curve wrappers `G1/G2/Fr` expose blst types publicly.
  - The legacy scheme is "bug-for-bug" compatible with dashbls: it uses Chia's in-crate SvdW hash-to-curve and **deliberately skips subgroup checks**.
  - Corpora cover keygen, sign, aggregate, secure_aggregate, threshold, IES, DH, hash, serialize, and a complete LLMQ-100 DKG. **Their provenance is not documented**; they are probably dashbls or Core, but that is unverified.
- **ECDSA** (`pkc/src/ecdsa/`):
  - Byte types: `EcdsaPkBytes` (SEC1, including hybrid), `EcdsaPkHash` (Hash160, re-exported as `dash_script::PubKeyHash`), `EcdsaSkBytes` (WIF in and out, zeroizing), `EcdsaSigBytes` (64-byte compact), `EcdsaRecSigBytes` (65-byte message-signing format).
  - Operational types:
    - `EcdsaPublicKey`: `verify` rejects high-S; `recover`.
    - `EcdsaSecretKey`: RFC6979 signing with **low-R grinding**. Its wire form is Core's DER `CPrivKey`.
    - `EcdsaSignature`: **strict** DER only, not Core's lax DER.
    - `EcdsaRecSignature`.
- **EdDSA** (`pkc/src/eddsa/`): `EddsaPkBytes`, `EddsaSkBytes`, `EddsaSigBytes` and `EddsaPkHash`. `EddsaPkHash` is the Platform node ID, with byte order pinned to rust-dashcore#887. Operational: `EddsaPublicKey` (strict verify; rejects weak keys), `EddsaSecretKey`, `EddsaSignature`. The byte types are `nocodec`.
- **Secrets:** zeroized on drop, constant-time equality, redacted `Debug`/`Display`, enforced by CodeQL dataflow queries (base-sdk#56).
- **Unreleased since 0.1.0-beta:** #53 (tweak and `negate` harmonisation, breaking) and #56 (`BlsScheme::sk_to_bytes` returns `Zeroizing`, breaking, plus several zeroization fixes).

#### `dash-pow` (0.0.0, unpublished): X11

- **Pure-Rust X11**, all 11 algorithms in the crate, **zero runtime dependencies**.
- API: `dash_pow::hash(&[u8]) -> [u8; 32]`. The per-algorithm `hash512` functions are hidden.
- `simd` needs nightly `portable_simd`. `aes_hw` works on aarch64; on x86_64 the AES-NI module is compiled but **never dispatched**.
- Tests: NIST SHA-3 known-answer tests for each algorithm, plus 2 X11 vectors. Primitives' block corpus and bspcheck exercise it further.

#### `dash-script` (0.0.0): opcodes and address classification

- `Opcode` is an open enum that includes the DIP-0020 opcodes (`OP_CAT`, `OP_SPLIT`, `OP_CHECKDATASIG`, …). Also `legacy_sigop_count`.
- `Recipient {PubKey, PubKeyHash, ScriptHash, Unspendable}` with `from_script`, `to_script`, `to_base58c` and `from_base58c`. `AddrParams` holds the prefixes.
- **There is no Script, ScriptBuf or Builder type.** It uses `bitcoin_primitives::script::{ScriptPubKeyBuf, ScriptSigBuf}` or raw bytes. No `Address` type, no interpreter, no sighash.

#### `dash-primitives` (0.0.0): blocks, transactions, payloads, governance

- **Block types:** `BlockHeader` (`bits: u32`; `Hashable` is X11), `Block {header, transactions}` (`Checkable`), `BlockHash`, `MerkleRoot`.
- **Transaction types:**
  - `Transaction {version: i16, tx_type: TxType, inputs, outputs, lock_time, extra_payload: Vec<u8>}` decodes payloads **lazily** through `decode_payload()`.
  - `TxIn {prevout, script_sig: ScriptSigBuf, sequence}` has **no witness**.
  - `TxOut {value: bitcoin_units::Amount, script_pubkey: ScriptPubKeyBuf}`.
  - `OutPoint {hash: TxHash, index}` and `TxHash`.
- **Special payloads:**
  - `SpecialPayload` has these variants: `ProRegTx`, `ProUpServTx`, `ProUpRegTx`, `ProUpRevTx`, `CoinbaseCommitment` (CbTx v1–3), `FinalCommitment`/`Commitment` (QcTx, `DynBitset`), `MnHardFork`, `AssetLock`, `AssetUnlock`, and `Unknown{tx_type, data}`.
  - `TxType` is an open enum covering 0–9 **only**. `MnType` and `PlatformNodeId` are also defined.
  - **Implemented:** ProRegTx and ProUpServTx v3 ExtAddr (`NetInfoV2`).
  - **Not implemented:** DIP-26 multi-party payouts and shares. The ProRegTx v3 vectors are synthetic, from `rpc_netinfo.py`.
- **Governance:** `GovObject` and `GovVote` with Core-compatible hashes, `Proposal`, `Trigger`, `VoteOutcome`, `VoteSignal`. rust-dashcore has none of these.
- **Network address types:** `AddrV1`, `ServiceV1`, `AddrV2`, `ServiceV2`, `NetInfo{Legacy, Extended}`, `NIEntry`, `NIPurpose`.
- `LlmqType` is a wire `u8` enum. **It is missing types 103 and 105.**
- **Stateless verification** covers:
  - `Block::check()`: PoW against nBits, merkle root, coinbase rules, per-tx checks, legacy sigops.
  - `Transaction::check()`: Core's `CheckTransaction` rules and structural payload checks.
  - **No signature verification at all.** `dash-pkc` is pulled in with `codec` only, so BLS and ECDSA fields are opaque bytes.
  - No contextual checks.
- **Corpus:**
  - 34 mainnet blocks around every activation height, from genesis to 2,201,472.
  - About 20 mainnet transactions for each payload type.
  - Governance objects and votes.
  - bspcheck (in `pkgs/dev`) replays a linearized `bootstrap.dat` through decode, re-encode, serde round-trip and `check()`.

#### `dash-params` (0.0.0): chain parameters

- `Network {Main, Testnet3, Regtest}`. **There is no Devnet.**
- `ChainParams`: magic, ports (P2P, RPC and platform), DNS seeds, `AddrParams`, checkpoints (37 on mainnet, 19 on testnet), CoinJoin limits, credit-pool period.
- `ConsensusParams`: every buried deployment height, the BIP9 `v24` deployment, `pow_limit: Arith256`, DGW, minimum chain work, assume-valid, LLMQ selections for ChainLocks, IS, platform and MNHF.
- `genesis()`.
- **It defines its own second `LlmqType`**, which has no discriminants. There are no LLMQ size, threshold or rotation tables, no spork keys and no serde.

#### `dash-p2p-core` (0.0.0): P2P message types

- `P2pMsg` is generated from a table. Framing is **BIP324 v2 short-ID framing only**: `encode_v2` and `decode_v2`. **There is no V1 24-byte header or checksum**, and no cipher or handshake (there's no networking by policy).
- **Parsed messages:** `version`, `verack`, `ping`, `pong`, `addr`, `addrv2`, `inv`, `getdata`, `notfound`, `getheaders`, `headers`, `getheaders2`, `headers2` (DIP-25 compressed), `govsync`, `govobj`, `govobjvote`, `getmnlistd`, `mnlistdiff` (with `SimplifiedMnListEntry`).
- **Raw-byte stubs:** `block`, `tx`, all BIP157/158 messages, bloom messages, `merkleblock`, compact blocks, `clsig`, `isdlock`, `qrinfo`, `getqrinfo`, `mnauth`, `spork`, all CoinJoin messages, and all LLMQ DKG and signing messages.
- `InvType` does not name the isdlock or clsig inventory types.

#### `dash-dev` (`publish = false`): test helpers and tooling

- Corpus loader (`Corpus`), check helpers (`check_tx`, `check_wire`, `check_sptx`), and serde round-trip asserts.
- The `bsdk-util bspcheck` binary.
- A build-time `TypeId` collision table.

---

## 2. Integration status in rust-dashcore

### 2.1 How rust-dashcore consumes base-sdk today

- The root `Cargo.toml:15-17` pins `dash-pkc` and `dash-types` by **git rev `e6402ced`**. That is base-sdk#52, 2026-09-27, with the comment "MUST share same version ACROSS crates".
- `develop` is now 51 commits ahead. The gap holds base-sdk #53, #54, #55 and #56, two of which are breaking for pkc and num.
- `secp256k1 = 0.33.1` is pinned in lockstep with `dash-pkc` and `bitcoin-crypto`. `bitcoin-crypto` is itself a **git rev** of rust-bitcoin, because its crates.io release depends on a yanked secp.
- **Only `crypto/src/bls.rs` and `crypto/src/eddsa.rs` import base-sdk crates.**
  - `dash/Cargo.toml:47` declares `dash-types` but no `dash/src` file uses it, so that dependency is vestigial.
  - `crypto/src/bls.rs:10` reaches into `dash_pkc::__deps::ff` (a hidden, unstable path).
- `dashcore-crypto` (`crypto/`, created by #1036) is the integration seam.
  - It wraps `dash-pkc` (BLS, EdDSA), `secp256k1`, `bitcoin-crypto` (sighash and taproot types) and `base58ck`.
  - BLS uses **unscheme'd** `make_bytes!` byte bags plus a **runtime** `BlsScheme {Legacy, Modern}`. Every operation is a `match` that dispatches to `dash_pkc`'s compile-time `BlsScChia` or `BlsScIetf`. The runtime choice is deliberate: Dash data picks the scheme from the version.
  - The byte bags carry **hand-written bincode impls** so persisted formats survive.

### 2.2 Status by type and module

| Area | rust-dashcore today | base-sdk target | Status | Driver / PR |
|---|---|---|---|---|
| BLS backend (sign, verify, aggregate, threshold) | `dashcore-crypto::bls` over `dash_pkc::bls` | `dash-pkc` | **Done** (merged 2026-09-23) | kwvg #1036 (with #1038, #1048) |
| BLS byte types (`BLSPublicKey`, `BLSSignature`, `BlsSkBytes`) | Own `make_bytes!` types in `crypto/bls.rs`, unscheme'd, with bincode | `BlsPkBytes<S>` etc. (scheme-typed) | **Partial, by design**: the local wrappers stay for runtime scheme selection and bincode | #1036 |
| EdDSA keys and signatures | Re-export of `dash_pkc::eddsa` | `dash-pkc` | **Done** (2026-09-28) | kwvg #1056 |
| `EddsaPkHash` / `PlatformNodeId` | **Own** `make_bytes!{EddsaPkHash,20,rev}` (`crypto/eddsa.rs:17`), kept for its bincode layout | `dash_pkc::eddsa::EddsaPkHash` | **Duplicated**, follow-up needed | none |
| `make_bytes!`/`make_sbytes!` helpers | Used in `crypto/` | `dash-types` | Done (in `crypto` only) | #1036 |
| Workspace pins and rust-bitcoin re-exports (sighash types, taproot, `base58ck`, `bitcoin_hashes` 0.14 pass-through) | `hashes/` is now a pass-through; `crypto/sighash.rs` re-exports `bitcoin-crypto` | n/a (this is the "re-export upstream" strategy) | **Done** (2026-10-07) | kwvg #1108 |
| Hex | `hex-conservative` 1.x everywhere | Same | **Done** (#1115, 2026-10-07). #1110 (open, ready for review) removes `dashcore_hashes::hex` and bans other hex crates | kwvg #1115, #1110 |
| BIP38 | Removed | n/a | Done | kwvg #1111 |
| `U256`, `Work`, `Target`, difficulty | Private `U256` in `dash/src/pow.rs` (about 1,000 lines) + SPV `ChainWork` (a third 256-bit implementation) | `dash_num::Arith256`, `CompactTarget` | **In flight**: #1142 (draft, -1393/+326). Needs a re-pin past base-sdk#54 | kwvg #1142 |
| ECDSA keys (`PublicKey`, `PrivateKey`, WIF, message signing) | `crypto/key.rs`, rust-bitcoin shaped, directly on secp256k1; `dash-pkc/ecdsa` **not enabled** | `dash_pkc::ecdsa::*` | **Not started** | none |
| X11 | `dashcore_hashes::hash_x11` over C `rs-x11-hash` (git tag) | `dash-pow` (pure Rust, unpublished) | Not started | none |
| Hash newtypes (`Txid`, `BlockHash`, `QuorumHash`, `ProTxHash`, …) | `hash_newtype!` on `bitcoin_hashes` 0.14 (+bincode, per-type display direction) | `dash_num::make_hash!` (`TxHash`, `BlockHash`, …), bitcoin_hashes 1.x | Not started | none |
| `Amount` | Fork of rust-bitcoin `Amount` (`dash/src/amount.rs`), `TxOut.value: u64` | `bitcoin_units::Amount` | Not started | none |
| Script, opcodes, classification | `blockdata/script/*` (rust-bitcoin fork) | `bitcoin_primitives` `ScriptBuf<T>` + `dash_script::{Opcode, Recipient}` | Not started | none |
| Address | `dash/src/address.rs` (typestate, plus segwit and taproot leftovers) | `dash_script::Recipient` + `AddrParams` | Not started | none |
| Network enum | `dash-network` (`Mainnet/Testnet/Devnet/Regtest`, `FFINetwork`, bincode) | `dash_params::Network` (**no Devnet**) | Not started | none |
| Chain and consensus params, genesis, checkpoints | `dash/src/consensus/params.rs`, `blockdata/constants.rs`, `dash-spv/src/chain/checkpoints.rs` | `dash-params` | Not started | none |
| Tx, TxIn, TxOut, OutPoint, Block, Header | `blockdata/*` (rust-bitcoin fork, with `Witness`) | `dash-primitives` | Not started | none |
| Special payloads (DIP2) | `blockdata/transaction/special_transaction/*` (eager, bincode) | `dash_primitives::payload::*` (lazy) | Not started. **Parallel work:** ZocoLini #1136 (unknown types as raw), #1137 (CbTx v4), #1138 (ProUpServ/ProUpReg v3) are open in rust-dashcore. base-sdk lacks CbTx v4 and types 10–12 | ZocoLini (rust-dashcore side) |
| LLMQ types and params | `dash/src/sml/llmq_type` (full params, rotation) | Two partial copies (`primitives::LlmqType`, `params::LlmqType`), no param tables | Not started; base-sdk needs work first | none |
| SML engine, QRInfo, quorum validation | `dash/src/sml/*`, `dash-spv` | None (MnListDiff only) | n/a: stays in rust-dashcore | none |
| ChainLock, InstantLock | `ephemerealdata/*` | None (P2P stubs) | n/a: base-sdk gap | none |
| Consensus codec | `consensus::{Encodable, Decodable}` over `std::io` (streaming) | `BaseCodec` (slice cursor) + `bitcoin-consensus-encoding` | Not started | none |
| P2P messages | `dash/src/network/message*.rs` (about 47 typed variants) | `dash-p2p-core` (most are stubs) | Not started; base-sdk gap | none |
| BIP158, bloom, MerkleBlock, BIP152, sighash, PSBT-ish signing, BIP32/DIP9/DIP14/SLIP10 | rust-dashcore | None | n/a: stays in rust-dashcore unless the owner decides otherwise | none |
| Governance objects and votes | **None** | `dash_primitives::gov`, `dash_p2p_core` gov messages | Not started (purely additive) | none |

### 2.3 Who is driving

- **kwvg** is the base-sdk author and the sole driver of the rust-dashcore integration. Work arrives as a "reduce" PR series (branches `reduce_p1`, `reduce_p2` and `reduce_p3`, i.e. #1108, #1110 and #1142).
  - The stated long-term goal (#1108): re-export unchanged rust-bitcoin code and end with "a dependency graph where no more than one version of a rust-bitcoin crate appears at a time". rust-dashcore currently "sits in between 0.13 and 0.15 of rust-bitcoin".
- **rust-dashcore approvers**: QuantumExplorer, ZocoLini and xdustinface (PR-hygiene bot). Reviews have been substantive and have caught real compatibility risks:
  - **#1056:** ZocoLini flagged a bincode byte-order change for `PlatformNodeId`, and a serde switch to always-hex for binary formats. kwvg's response: "We cannot *change* the encoding format without breaking backwards compatibility". He kept bincode and changed serde-binary to raw little-endian bytes to match Core.
  - **#1108:** ZocoLini found a sighash-SINGLE regression for 7 non-standard flag values from switching to upstream `EcdsaSighashType`. It was fixed before merge.
  - **Workspace-level dependency policy:** ZocoLini pushed back on declaring single-crate dependencies at workspace level. kwvg kept the base-sdk crates there for version lockstep.
- **Platform side:** lklimek drives the adaptation (#5307 stack on v5.1-dev).

### 2.4 Downstream consumers and the Dash Core path

| Consumer | How it depends | State relative to base-sdk |
|---|---|---|
| **platform** v5.0-dev | rust-dashcore git rev `40268cc` (8 crates) | **Before** #1036, #1056 and #1108: still `blsful` and secp 0.30. 16 `dashcore::blsful` uses, including the public `ProtocolError::BlsError`. |
| **platform** v5.1 stack (`chore/rust-dashcore-1112-v5.1`; #5307 open, #5320 merged 2026-10-07) | rust-dashcore `314f106`; **`rs-dpp` depends on `dash-pkc` directly at the same rev `e6402ced`** | Has #1036 and #1056 but **not #1108 or #1115**. #5320 replaced blsful with DPP-owned `dpp::bls::*` over `dash-pkc`, with frozen compatibility vectors. |
| Platform usage depth | 628 files, about 2,100 `dashcore::` paths | Top paths: `Network` (about 520 including variants), `hashes::Hash` (252), `secp` (200), `OutPoint` (106), `Txid` (85), `Transaction` (65), consensus encode (69), special payloads (42), ChainLock and IS (34), `sml` (11). bincode is heavily used (shared `grovedb-bincode` traits). |
| **dash-evo-tool** | Only through platform (`dash_sdk::dpp::dashcore`, 532 paths); lock at rust-dashcore `e4208c9` (before #1036) | Will absorb everything in one platform bump. Relies on the identity of the bincode traits for on-disk envelopes. |
| Mobile (Swift/Kotlin) | `key-wallet-ffi`, `dash-spv-ffi` (cbindgen; `FFINetwork` 0..3; hex strings; opaque bincode wallet blobs) | Sensitive to Network discriminants, hex display direction and bincode layout. |
| dash-wallet (Android), tenderdash (Go) | dashj and Go BLS bindings | Not affected directly. |
| **Dash Core** | No Rust in its build today, apart from the vendored `src/dashbls` Rust bindings | **dash#7109** (kwvg, draft, Jan 2026, "needs rebase", changes requested by UdjinM6 and knst) adds Rust-in-depends via `cxx`, musl, vendoring and Guix. It is a prerequisite for Core ever linking base-sdk. **Competing direction:** knst's dash#7678, #7679 and #7680 move Core's BLS to C++ `blst` and drop relic and GMP, which would reduce the need for `dash-pkc` in Core. PastaPastaPasta's platform-GUI cxx PRs (#7623, #7670) are another Rust-in-Core path. |

---

## 3. Duplication map

**Difficulty scale:**
- **L:** mechanical.
- **M:** API or semantic gaps; adapters needed.
- **H:** changes representation, persisted format or ABI, or needs base-sdk features first.

**Blast radius** = the number of `.rs` files referencing the type (generic names over-count).

| rust-dashcore type (location) | base-sdk equivalent | Key differences | Blast radius | Difficulty |
|---|---|---|---|---|
| X11 `hash_x11` (`hashes/src/hash_x11.rs`, C `rs-x11-hash`) | `dash_pow::hash` | Pure Rust; removes a C dependency; performance unbenchmarked; crate unpublished | 5 | **L** |
| Private `U256`, `Work`, `Target` (`dash/src/pow.rs`) | `dash_num::Arith256`, `CompactTarget`, `DecodedTarget` | Core-exact rounding and lossy parsing; no codec on `Arith256` | 1 / 6 / 19 | **L** (#1142) |
| SPV `ChainWork` (`dash-spv/src/chain/chain_work.rs`) | `Arith256` | A third 256-bit implementation | spv only | L |
| `CompactTarget` (`pow.rs:270`) | `dash_num::CompactTarget` | `Header.bits` is a raw `u32` in base-sdk; bincode derive | 21 | M |
| `EddsaPkHash` / `PlatformNodeId` (`crypto/eddsa.rs:17`) | `dash_pkc::eddsa::EddsaPkHash` | Pure duplication; bincode layout must be preserved (an existing test pins it) | 12 | **L-M** |
| BLS byte bags (`crypto/bls.rs`) | `BlsPkBytes<S>`, `BlsSigBytes<S>`, `BlsSkBytes<S>` | Scheme chosen at runtime vs in the type; bincode | 24 / 39 / 4 | H (keep wrappers) |
| `PublicKey`, `PrivateKey`, WIF (`crypto/key.rs`) | `EcdsaPkBytes`/`EcdsaPublicKey`, `EcdsaSkBytes`/`EcdsaSecretKey` | Different shape; key-wallet BIP32 uses secp directly; WIF prefix vs `Network` | 41 / 15 | M-H |
| ECDSA `Signature` + sighash byte (`crypto/ecdsa.rs`), `sign_message` | `EcdsaSignature`, `EcdsaRecSignature` | base-sdk is strict-DER only and has no sighash pairing | 16 | M |
| sha256d / hash160 (bitcoin_hashes 0.14) | bitcoin_hashes 1.2, `dash_num::Hash160`/`Hash256` | 1.x drops the generic `Hash` trait (`all_zeros`, engine). **Both majors already coexist in the build** through `bitcoin-crypto` | 29 / 29 | M |
| `hash_newtype!` types | `dash_num::make_hash!` | No bincode; `Numeric` API instead of `to_byte_array`/`from_byte_array`/`all_zeros` (≈390 / 155 / 83 call hits) | ~93 files | **H** |
| `Txid` | `TxHash` | Rename and API change | 78 | H |
| `BlockHash` (sha256d **by default**, X11 only with `core-block-hash-use-x11`) | `BlockHash` (always X11) | Footgun: `dashcore` with default features computes the wrong block hash. base-sdk removes it | 103 | H |
| `QuorumHash` (= `BlockHash` alias), `CycleHash` | Distinct `QuorumHash` | The alias becomes a distinct type, which forces conversions | 29 | M |
| `ProTxHash` (forward display) | `TxHash` (reversed) | Display direction flips, which changes RPC and FFI strings | 14 | M |
| `PubkeyHash` / `ScriptHash` (forward) | `EcdsaPkHash` (reversed), `ScriptHash` (`make_bytes … rev`, TODO) | Display direction flips | 31 / 12 | H |
| `FilterHash`, `FilterHeader`, `ChainLockHash`, `InstantSendLockHash`, `Wtxid` | None | Wtxid is a Bitcoin vestige | small | n/a |
| `Amount`, `SignedAmount`, `Denomination` (fork) | `bitcoin_units::Amount` | Display "BTC"; `rpc-json` uses `as_btc` (21×); `TxOut.value` u64 vs Amount | 27 (+117 `.value`) | M |
| `Script`, `ScriptBuf` (untagged) | `bitcoin_primitives` `ScriptSigBuf`/`ScriptPubKeyBuf` (tagged) | The tag split ripples through every scriptPubKey use (263 hits) | 78 | H |
| Opcodes (`blockdata/opcodes.rs`) | `dash_script::Opcode` | base-sdk includes the DIP-0020 opcodes | 18 | L-M |
| `Builder`, `Instruction`, push-bytes | None | n/a | ~26 | n/a |
| Script classification (`is_p2pkh` …) | `Recipient::from_script` | Subset (no multisig) | 8 | L |
| `Address<V>`, `Payload`, `AddressType` | `Recipient` + `AddrParams` | No typestate; wallet and FFI string forms | 112 | H |
| `Network` (`dash-network`) | `dash_params::Network` | **No Devnet**; no serde, bincode or FFI; variant names differ (`Mainnet`/`Main`, `Testnet`/`Testnet3`) | **199** | **H** |
| Consensus params, genesis, magic, ports, seeds, checkpoints | `dash_params::{ChainParams, ConsensusParams}`, `genesis()` | base-sdk is richer for params; SPV checkpoints carry extra metadata | ~20 | M |
| `LLMQType` + `LLMQParams` | `primitives::LlmqType` + `params::LlmqType` | base-sdk has two copies, no 103/105 in primitives, and no param tables | 30 | M-H |
| `Transaction` | `dash_primitives::Transaction` | `version: u16` vs `i16` + `tx_type`; eager `Option<TransactionPayload>` vs lazy `extra_payload`; field names | 125 (188 literal constructions) | **H** |
| `TxIn` (`previous_output`, `witness`) | `TxIn {prevout, …}` (no witness) | Field rename; drop `Witness` | 36 | H |
| `TxOut` (`value: u64`) | `TxOut {value: Amount, …}` | Amount type | 44 | H |
| `OutPoint {txid, vout}` | `OutPoint {hash, index}` | Field rename; serde string form | 68 | H |
| `Witness`, taproot, `Weight` | None | Bitcoin vestiges; can be deleted | 38+ | L-M |
| `Header`, `Block` (`txdata`) | `BlockHeader`, `Block {transactions}` | Field names; `bits` type | 57 / 78 | H |
| Merkle root | `Block::merkle() -> (root, mutated)` | Equivalent | 28 | L |
| `TransactionPayload` / `TransactionType` | `SpecialPayload` / `TxType` | Unknown-type handling (#1136 vs `Unknown{..}`) | 27 | H |
| ProRegTx, ProUpServTx | `ProRegTx`, `ProUpServTx` (NetInfo v1/v2) | rust-dashcore lacks NetInfo v3 in ProRegTx (#1138 adds ProUpServ/ProUpReg v3). **Neither has DIP-26 payouts or shares** | 7 | M-H |
| ProUpRegTx, ProUpRevTx, MnHF | `ProUpRegTx`, `ProUpRevTx`, `MnHardFork` | Close | 7 / 2 | M / L |
| `CoinbasePayload` | `CoinbaseCommitment` | **base-sdk rejects v4**; #1137 adds v4 to rust-dashcore | 6 | M |
| `QuorumEntry`, `QuorumCommitmentPayload` | `Commitment`, `FinalCommitment` | `Vec<bool>` vs `DynBitset`; BLS scheme hard-coded to IETF in base-sdk | 12 | H |
| `AssetLockPayload`, `AssetUnlock*` | `AssetLock` (v1 only), `AssetUnlock` | v2 support missing in both; v24 asset-unlock txid rule missing in both | 11 / 5 | M |
| `MnListDiff`, `GetMnListDiff`, `MasternodeListEntry`, `MasternodeNetInfo` | `dash_p2p_core::{MnListDiff, MnListDiffPayload, SimplifiedMnListEntry}`, `NetInfo` | base-sdk SML entry is ServiceV1-only and its operator key is typed IETF | 26 | H |
| `QRInfo`, `ChainLock`, `InstantLock` | Stubs only | base-sdk gap | 21 / 40 / 30 | n/a |
| `consensus::{Encodable, Decodable}`, `VarInt` | `BaseCodec`, `CompactSize`, `bitcoin_consensus_encoding::{Encode, Decode}` | Streaming `io` vs slice cursor (463 / 414 call hits) | 58 / 53 | **H** |
| `NetworkMessage`, `RawNetworkMessage` | `P2pMsg` | V1 framing absent; most SPV-relevant messages are stubbed | 36 | **H** |
| `VersionMessage`, `ServiceFlags`, `AddrV2`, headers2 | `Version`, `ServiceFlags`, `AddrV2Msg`, `Headers2` | Close | 14–18 | M / L |
| BIP158, bloom | None | n/a | 12 / 10 | n/a |
| Governance | (none in rust-dashcore) | `GovObject`, `GovVote`, … | 0 | Additive |
| bincode (45 derive sites in `dash`; all hash newtypes; key-wallet; SPV storage) | **None** (dropped in base-sdk#1) | Orphan rule: rust-dashcore cannot implement `bincode::Encode` for base-sdk types | all | **H** (policy decision) |

**Cross-cutting constraints:**
- **no_std.** `dashcore` dropped no_std in #521. Consuming base-sdk is fine; pushing rust-dashcore code down into base-sdk (sighash, BIP158, SML) requires removing `std::io`.
- **Editions.** rust-dashcore defaults to 2024 and base-sdk to 2021. Code moving into base-sdk must downgrade.
- **Publishing.** Both repos depend on git revs (`bitcoin-crypto`, `rs-x11-hash`, base-sdk), so rust-dashcore cannot publish to crates.io. All of its crates are consumed by git rev only.
- **Coordinated bumps.** Every secp256k1 bump must move rust-dashcore, base-sdk and `bitcoin-crypto` together.
- **Display order.** rust-dashcore displays hash160-family types and `ProTxHash` forward, while base-sdk displays everything reversed. Changing this alters RPC JSON (fixed by Core) and FFI strings.

---

## 4. Recommended integration plan

### 4.1 Strategy

The aim is "rust-dashcore fully uses base-sdk", and base-sdk then becomes the shared base layer for the node. Four principles follow from the evidence above.

1. **Tier the work by base-sdk maturity.** The released crates (`types`, `num`, `pkc`) can be adopted fully now. The incubating crates (`primitives`, `params`, `script`, `p2p_core`, `pow`) have gaps that **must be closed in base-sdk first**, otherwise rust-dashcore would regress on Devnet, ChainLock, InstantLock, QRInfo, P2P and v24. Gaps should be closed upstream, never by forking base-sdk.
2. **Converge by adapters, not a big bang.**
   - Add `From`/`TryFrom` bridges between dashcore and base-sdk types, plus a **differential conformance harness**.
   - Then move the *definition* of each type into base-sdk, leaving rust-dashcore a re-export or a thin newtype (needed for bincode, the orphan rule and FFI stability).
   - kwvg already uses this pattern for BLS and EdDSA.
3. **One home for protocol shapes going forward.** New wire formats (v24: CbTx v4, types 10–12, ProRegTx v3 with DIP-26, AssetLock/Unlock v2) should land in `dash-primitives` first, and rust-dashcore should consume them. Today ZocoLini's #1136/#1137/#1138 implement them in rust-dashcore in parallel, which deepens the duplication.
4. **Freeze persisted and ABI formats explicitly.** Every type swap carries layout-pinning tests for bincode and serde, and FFI string-format tests. #1056's `bincode_layout_matches_pubkey_hash` is the template.

**Definition of done for "rust-dashcore fully uses base-sdk":**
- (a) All cryptography goes through `dash-pkc`.
- (b) All hash blobs and 256-bit arithmetic come from `dash-num`.
- (c) Every consensus-serialised primitive (tx, block, header, payloads, SML entry, MnListDiff, governance) is **defined** in `dash-primitives` or `dash-p2p-core`. rust-dashcore re-exports it or wraps it thinly.
- (d) Chain and consensus parameters come from `dash-params`.
- (e) X11 comes from `dash-pow`.
- (f) A differential harness proves byte-for-byte equivalence over mainnet and testnet history.

Things that stay in rust-dashcore by design: wallet (BIP32/DIP9/14, SLIP10), SPV, SML engine and quorum validation, BIP158, bloom, sighash, signing, and RPC.

### 4.2 Ordered tasks

Each task is roughly one PR. Effort is a rough engineer-week estimate.

"Coord" means coordination is needed:
- **K** = kwvg / base-sdk maintainer.
- **M** = rust-dashcore approvers (QuantumExplorer, ZocoLini, xdustinface).
- **P** = Platform (lklimek).
- **C** = Dash Core developers.

"base-sdk change?" says whether the PR is in base-sdk itself; those must follow `namespace%type:` commits, the nightly toolchain, CodeQL and semgrep, and 2-space style.

#### Phase 0: agreement before code (week 0–1)

| # | Task | Scope | Crates | Risk | Coord | base-sdk change? | Effort |
|---|---|---|---|---|---|---|---|
| 0.1 | **Architecture RFC: ownership map** | One GitHub discussion or issue in each repo. It fixes the ownership map (which crate defines each shape), the definition of done above, the bincode policy (see 0.2), the display-order policy, Network/Devnet ownership, the `LlmqType` home, the version-pin policy (git rev vs crates.io; a common release train), and where v24 payloads land | docs | Low | **K, M, P** | Discussion only | 0.5 |
| 0.2 | **bincode decision** | Options: (a) keep rust-dashcore newtypes that own bincode (the status quo pattern); (b) add an optional `bincode` feature to base-sdk, which kwvg removed in base-sdk#1, so it needs buy-in; (c) serde-bridged bincode, which changes layout and needs migrations. **Recommendation: (a) for persisted types, plus pinned-layout tests.** | n/a | Medium | **K, M, P** | Maybe | n/a |
| 0.3 | **Get project-board access** | `gh auth refresh -s read:project` so boards #13 and #14 can be read and the v0.2 roadmap confirmed | n/a | None | n/a | No | n/a |

#### Phase 1: finish released-crate integration (rust-dashcore only; weeks 1–4; low risk)

| # | Task | Scope | Crates | Risk | Coord | base-sdk change? | Effort |
|---|---|---|---|---|---|---|---|
| 1.1 | **Re-pin base-sdk to `develop`** | Absorb #53 (tweak and `negate` API), #54 (`Arith256`), #56 (`Zeroizing` returns). Add a CI check that all base-sdk deps share one rev. Prefer pinning to a tagged snapshot. **Platform's `rs-dpp` pins `dash-pkc` too, so its pin must move in lockstep.** | root `Cargo.toml`, `crypto` | Low-Med | M, **P** | No | 0.5 |
| 1.2 | **Land #1110 and #1142** (kwvg's) | Review and help them merge. #1142 depends on 1.1 | `dash`, `hashes`, `internals`, … | Low | K, M | No | review |
| 1.3 | **Fold SPV `ChainWork` into `Arith256`** | Remove the third 256-bit implementation. Keep the on-disk format (pinned test) | `dash-spv` | Low | M | No | 0.5 |
| 1.4 | **Deduplicate `EddsaPkHash`** | Make `PlatformNodeId` a newtype over or a re-export of `dash_pkc::eddsa::EddsaPkHash`, keeping the bincode layout and serde form | `crypto`, `dash`, `key-wallet` | Low | M, P | No | 0.5 |
| 1.5 | **Remove or use the vestigial `dash-types` dep in `dash`; stop using `dash_pkc::__deps::ff`** | Ask kwvg for a public reduce-scalar API, or use `dash_pkc`'s `Fr` directly | `dash`, `crypto` | Low | K | Maybe (small pkc API) | 0.5 |
| 1.6 | **Route ECDSA through `dash-pkc`** | Enable `dash-pkc/ecdsa`. Re-base the internals of `crypto::key::{PublicKey, PrivateKey}` and WIF on `EcdsaPkBytes`, `EcdsaSecretKey` and `EcdsaSkBytes`, keeping the rust-bitcoin-shaped public API. Message signing goes through `EcdsaRecSignature`. Check **lax-DER** handling: base-sdk is strict and Core verification is lax. key-wallet BIP32 keeps raw secp (same version) | `crypto`, `dash`, `key-wallet` | Med | M, K | Possibly (lax-DER parse helper) | 1.5 |
| 1.7 | **Hash bridge layer** | `From`/`Into` between dashcore hash newtypes and the `dash_num`/`dash_primitives` hash types (`Txid↔TxHash`, `BlockHash↔BlockHash`, `QuorumHash`, `PubkeyHash↔EcdsaPkHash`, …), with tests asserting the same bytes and documented display differences. This enables everything in Phase 3 incrementally | `dash` (behind a feature at first) | Low | M | No | 1 |
| 1.8 | **Fix the BlockHash footgun** | Make X11 `BlockHash` unconditional (remove the sha256d default of `core-block-hash-use-x11`). Breaking, but a correctness fix and a prerequisite for aligning with base-sdk | `dash`, `hashes` | Med (breaking) | M, P | No | 0.5 |

#### Phase 2: close base-sdk gaps (base-sdk PRs; weeks 2–10; can run in parallel with Phase 1)

| # | Task | Scope | Crates | Risk | Coord | base-sdk change? | Effort |
|---|---|---|---|---|---|---|---|
| 2.1 | **Differential conformance harness** | A dev-only crate (in rust-dashcore, or in base-sdk's `dash-dev`/bspcheck) that decodes every mainnet and testnet block and transaction through **both** `dashcore` and `dash_primitives`. It asserts identical re-encoding, txid, block hash, merkle root and payload fields, and optionally cross-checks Dash Core RPC `getblock 2`. **This gates every later swap and is reusable for the node.** | new `rust-dashcore/conformance` (publish=false) | Low | M, K | No (or yes, if placed in bspcheck) | 1.5 |
| 2.2 | **`dash-params`: Devnet + LLMQ** | Add Devnet (parameterised name and genesis); one `LlmqType` (merge the two copies, add 103/105) with the full `LLMQParams` table (size, threshold, dkg interval, rotation, signing active count), ported from `dash/src/sml/llmq_type`; optional serde. Possibly also the spork address | params, primitives | Med | **K** | **Yes** | 2 |
| 2.3 | **`dash-primitives` v24** | CbTx v4 (`merkleRootAssetUnlocks`), AssetUnlock v2 (stable txid with zeroed tail plus instance hash), ProDisTx, ProUpShareTx and ProUpSharedRegTx (types 10–12), full ProRegTx v3 (DIP-26 payouts and shares, join sigs, early period), AssetLock v2. Fixes base-sdk#40. **Coordinate with ZocoLini's #1136/#1137/#1138 so it is implemented once**, preferably here, with rust-dashcore consuming it | primitives | Med | **K, M** | **Yes** | 3 |
| 2.4 | **ChainLock, InstantLock, QRInfo types** | Typed `clsig`, `isdlock` (+ InvType 29/31), `qrinfo`/`getqrinfo`; un-stub `block` and `tx` messages to use `dash_primitives::{Block, Transaction}`; SML entry with NetInfo v3. Port the wire structs only, not the engine | primitives, p2p_core | Med | K | **Yes** | 2 |
| 2.5 | **P2P completeness** | V1 24-byte framing + checksum (pure encode/decode, no I/O, so it stays within the "no networking" rule); BIP157/158 messages (re-add after #39); bloom and MerkleBlock; `sendcmpct`/`cmpctblock` | p2p_core | Med | K | **Yes** | 2 |
| 2.6 | **Release the incubating crates** | Publish `dash-pow`, `dash-script`, `dash-primitives`, `dash-params`, `dash-p2p-core` as 0.1.0-beta (and pkc/num/types 0.2) so rust-dashcore can pin versions, not revs | all | Low | **K** | Yes (release) | 0.5 |
| 2.7 | **X11 swap** | Replace the C `rs-x11-hash` with `dash_pow::hash` behind `dashcore_hashes::hash_x11` (same engine API). Benchmark header sync against the C version first; consider wiring x86_64 AES-NI dispatch in dash-pow (currently compiled but unused) | `hashes` (+ pow perf PR) | Low-Med | M, K | Maybe (perf) | 1 |

#### Phase 3: move protocol shapes onto base-sdk (rust-dashcore, breaking; weeks 6–16; one Platform bump per step)

Order: leaves first; tx and block last; each step gated by 2.1.

| # | Task | Scope | Crates | Risk | Coord | base-sdk change? | Effort |
|---|---|---|---|---|---|---|---|
| 3.1 | **Delete Bitcoin vestiges first** | `Witness`, taproot, `Weight`, `Wtxid`, segwit and bech32 address payloads. This shrinks the surface to migrate | `dash`, `key-wallet`, ffi | Med (breaking, noisy) | M, P | No | 1.5 |
| 3.2 | **Network and chain params** | `dash-network` becomes a thin newtype or re-export over `dash_params::Network` (keep `FFINetwork` discriminants and serde lowercase names); genesis, magic, ports, seeds and consensus params sourced from `dash-params`; SPV checkpoints keep their metadata but take hashes and heights from params | `dash-network`, `dash`, `dash-spv` | Med-High (199 files; FFI) | M, P | Needs 2.2 | 2 |
| 3.3 | **LLMQ types** | Replace `dash/src/sml/llmq_type` definitions with `dash_params`/`dash_primitives` ones, keeping rotation logic | `dash`, `dash-spv` | Med | M | Needs 2.2 | 1 |
| 3.4 | **Special payloads, one PR per payload** | `dashcore` payload structs become thin wrappers over `dash_primitives::payload::*` (owning bincode). Keep the eager `TransactionPayload` enum API for wallet code. Order: MnHF, AssetLock/Unlock, ProUpRev, ProUpReg, ProUpServ, ProReg, CbTx, QcTx | `dash`, `key-wallet`, `key-wallet-ffi` | Med | M, P | Needs 2.3 | 3 |
| 3.5 | **Hash newtypes** | Retarget `Txid`, `BlockHash` and the rest onto `make_hash!` types. Compatibility shims for the 0.14 `Hash`-trait methods (`to_byte_array`, `from_byte_array`, `all_zeros`) to keep about 600 call sites compiling, then migrate them gradually. Decide display direction per type in 0.1 (ProTxHash and PubkeyHash flip). Consider bundling `bitcoin_hashes` 0.14 → 1.x here | `hashes`, `dash`, everyone | **High** | M, **P** | Possibly (bincode) | 3 |
| 3.6 | **Consensus codec adapter** | Implement `consensus::{Encodable, Decodable}` for base-sdk types through `BaseCodec` (buffer to slice); keep the `io` streaming API for SPV. In the long run, consider adopting `bitcoin_consensus_encoding` as the outer trait like base-sdk | `dash` | Med | M | No | 1.5 |
| 3.7 | **Tx, TxIn, TxOut, OutPoint, Block, Header** | Wrap or re-export `dash_primitives`. Field renames (`previous_output→prevout`, `vout→index`, `txdata→transactions`), `Amount` type, tagged `ScriptBuf` and `version`/`tx_type` split. Probably via a compatibility layer (accessor methods) to cap the churn: 188 literal constructions, 241 `.input` hits | `dash`, `key-wallet*`, `dash-spv*`, `rpc-*` | **High** | M, **P** | Maybe | 4 |
| 3.8 | **Script, Amount, Address** | `ScriptBuf` → `bitcoin_primitives` tagged scripts; `Amount` → `bitcoin_units::Amount` (with Dash display/`as_dash` serde adapters for rpc-json); `Address` stays a rust-dashcore wallet type built on `dash_script::Recipient` + `AddrParams` | `dash`, `rpc-json`, `key-wallet` | High | M, P | Maybe (Dash denomination display) | 3 |
| 3.9 | **P2P messages** | `dash-spv` network layer moves onto `dash_p2p_core::P2pMsg` (needs 2.4/2.5). Rebase BIP324 #264 onto it | `dash`, `dash-spv` | High | M | Needs 2.4, 2.5 | 3 |

#### Phase 4: Dash Core sharing (optional, long-horizon, owned by Core developers)

| # | Task | Notes |
|---|---|---|
| 4.1 | Revive **dash#7109** (Rust-in-depends via cxx) | It is kwvg's draft, with changes requested. Base-sdk inside Core is only plausible once this lands. |
| 4.2 | Decide on Core's BLS direction | `dash-pkc` via cxx, or knst's C++ `blst` route (#7678–7680). Not a rust-dashcore blocker. |
| 4.3 | Add a C/cxx surface to base-sdk | Behind an `ffi` feature or in a separate `dash-ffi` crate. The board has an "FFI" topic, but no code exists. |

**Critical path to "integration complete enough to start node work":** 0.1 → 1.1 → 1.7 → 2.1 → 2.2/2.3/2.4 → 3.2/3.4. That is roughly **10–12 weeks**, if kwvg can review base-sdk PRs promptly.

Phases 3.5, 3.7, 3.8 and 3.9 are the long tail, a further 8–12 weeks. They can overlap with early node work if the node's new crates are written against `dash-primitives`/`dash-p2p-core` types from day one, using the bridge layer (1.7) where they touch rust-dashcore.

### 4.3 Risk register

| Risk | Mitigation |
|---|---|
| **Single-maintainer bottleneck.** kwvg authored 100% of base-sdk and all integration PRs. | Agree on review SLAs and co-maintainership in 0.1. Follow base-sdk conventions exactly (lint gate, commit format) to minimise friction. |
| **Silent consensus drift.** Examples: lax vs strict DER, BLS subgroup checks, BlockHash default, display direction. | The differential harness (2.1), Core-generated corpora (as in `arith256.json5`), and documented BLS vector provenance. |
| **Persisted data breakage** (wallet blobs, SPV storage, Platform state). | Pinned-layout tests; newtype ownership of bincode; storage version bumps (rust-dashcore#1084 already added storage versioning). |
| **Platform churn.** It is pinned per rev, and the v5.0 and v5.1 lines diverge. | Batch breaking steps, publish migration notes per step, and coordinate each bump with lklimek. |
| **Parallel v24 implementations** (rust-dashcore #1136/#1137/#1138 vs base-sdk). | Decide the home in 0.1. If rust-dashcore lands first, port with the harness proving equivalence. |
| **base-sdk API churn** (0.x, several breaking changes per month). | Pin to tagged snapshots; one re-pin PR per base-sdk milestone. |

---

## 5. Open questions for the program owner

1. **Scope of "fully uses base-sdk".**
   - Is Tiers 1–2 (crypto, num, params and payload shapes defined in base-sdk, with rust-dashcore wrappers) sufficient before node work begins?
   - Or must every public `dashcore` type be a base-sdk type, including Tx/Block/Script/Address/P2P (Phase 3 in full, about 20+ weeks)?
   - **Recommendation:** the former, with node crates built directly on base-sdk types.
2. **Should the node's new crates target `dash-primitives` and `dash-p2p-core` directly** rather than `dashcore` types? This decouples the node from the long tail of Phase 3.
3. **Who owns base-sdk changes?**
   - Will kwvg accept a large volume of external PRs (Devnet, LLMQ tables, v24, ChainLock/IS/QRInfo, V1 framing, P2P un-stubbing)?
   - What review turnaround can we expect, and should co-maintainers be added?
   - Does any of this conflict with the "base-sdk v0.2" board plan, which we could not read?
4. **bincode policy** (0.2): accept rust-dashcore newtypes as the permanent owners of persisted formats, or ask base-sdk to re-add bincode?
5. **Display-order policy.** base-sdk shows every hash reversed, while rust-dashcore shows `ProTxHash`, `PubkeyHash`, `ScriptHash` and the MerkleRoot* types forward. Do we accept changed hex strings in FFI and Swift/Kotlin, or keep per-type display overrides?
6. **v24 payloads.** Should ZocoLini's #1136/#1137/#1138 be paused and redirected into `dash-primitives`, or land in rust-dashcore now and be ported later?
7. **Versioning.** Push base-sdk to publish the 0.0.0 crates, or stay on git revs, which also keeps rust-dashcore unpublishable?
8. **Platform coordination.** Platform v5.0-dev is three integration steps behind, and v5.1 (#5307) is mid-adaptation. Who signs off on the cadence of breaking rust-dashcore bumps?
9. **Dash Core.**
   - Is "Dash Core consumes base-sdk" a hard program goal? If so, base-sdk needs a C/cxx surface, and dash#7109 needs a champion.
   - knst's C++ `blst` migration (dash#7678–7680) competes with putting `dash-pkc` in Core. Which direction does the program back?
10. **X11.** Is replacing the C `rs-x11-hash` with pure-Rust `dash-pow` acceptable if it is slower on x86_64? AES-NI isn't dispatched today.
11. **rust-bitcoin alignment.** Should the `bitcoin_hashes` 0.14 → 1.x migration (task 3.5) be part of this program? Both versions are already in the build through `bitcoin-crypto`.
12. **Consensus-critical gaps outside base-sdk's charter.** A node needs a script interpreter, sighash, contextual validation and UTXO state, none of which either library provides. Do they belong in base-sdk (no_std, stateless-only charter) or in new rust-dashcore crates?

---

## Appendix A: key references

| Item | Value |
|---|---|
| base-sdk released crates | `dash-types` 0.1.0-beta and `dash-types-marker` 0.1.0-beta (2026-09-15), `dash-num` 0.1.0-beta (2026-09-15), `dash-pkc` 0.1.0-beta (2026-09-17). Tags `dash-{types,num,pkc}-0.1.0-beta` |
| base-sdk milestones and boards | 0.1 (#1–#50, closed, 47 PRs), 0.2 (#51–#56); org projects #13 "base-sdk v0.1" and #14 "base-sdk v0.2" |
| base-sdk issues | #40 Known Issues (open), #37 (closed, CodeQL arm64) |
| rust-dashcore integration PRs (kwvg) | #1036, #1038, #1042, #1048, #1053, #1056, #1108, #1111, #1115 merged; **#1110 open**, **#1142 draft** |
| rust-dashcore related open PRs (ZocoLini) | #1136 unknown special-tx types as raw, #1137 CbTx v4, #1138 ProUpServ/ProUpReg v3; issues #1047 (Core v24 wire formats), #1130, #1131, #1135 |
| Platform | v5.0-dev pin `40268cc` (pre-#1036); v5.1 stack #5307 (open) / #5320 (merged, dpp::bls over dash-pkc) pin `314f106` + `dash-pkc@e6402ced` |
| Dash Core | dash#7109 (Rust build-system support, draft); dash#7678/7679/7680 (C++ blst BLS, knst); dash#7623/7670 (platform cxx GUI) |
| Key files | base-sdk: `AGENTS.md`, `docs/dev/guide_rust.md`, `maint/lint/lint_commit.py`, `pkgs/types/src/{codec,entity,secret}.rs`, `pkgs/num/src/{hash,arith256,compact}.rs`, `pkgs/pkc/src/{bls,ecdsa,eddsa}/`, `pkgs/primitives/src/{block,transaction}.rs`, `pkgs/primitives/src/payload/`, `pkgs/params/src/`, `pkgs/p2p_core/src/msg/mod.rs`. rust-dashcore: `Cargo.toml:11-23`, `crypto/src/{bls,eddsa,key}.rs`, `dash/src/hash_types.rs`, `dash/src/pow.rs`, `dash-network/src/lib.rs`, `dash/src/blockdata/`, `dash/src/network/` |

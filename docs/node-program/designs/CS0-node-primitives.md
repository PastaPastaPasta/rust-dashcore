# CS0: node primitives (the `dash-consensus` skeleton)

| | |
|---|---|
| Status | **Revision r2, for the manager's check** (PLAN §4.2 row CS0, design step D). Round 1: GPT-6.1-Sol REWORK, Opus APPROVE-WITH-CHANGES. Round 2: GPT-6.1-Sol REWORK (no blocker, two majors), Opus APPROVE-WITH-CHANGES. Dispositions in §16; decisions in DEC-30 |
| Date | 2026-10-08 |
| Author | CS0 implementer (Opus 5.5 xhigh), design phase |
| Task | `dash-consensus` skeleton: `Tx`/`Header`/`Block` wrappers, range decoders, `core_serialize` as the sole consensus encoder, `ValidationError`, `ConsensusCtx` (with `BlsCtx` and the release rules), plus the private EMBARGO-1 deliverables of PLAN §7.2 |
| Binding inputs | A1 §1.3 (canonical bytes), §1.4 (Rule DDE), §2.1, §2.3, §2.4, §2.25, Appendix A and E; PLAN §0, §4.2 (CS0, CS3, CS5a/b), §4.4 (EV0), §7; DEC-05, DEC-07, DEC-16, DEC-23, DEC-24, DEC-25, DEC-26, DEC-28, DEC-29; PRD FR-CON-01, -04, -05, -31, -33 |
| Code verified at | r0: `node/integration` 01098f56; BS3 d74f94a6; base-sdk `node/base-sdk` = develop 33fcc59; Dash Core `v23.1.8` and `v24.0.0-rc.3`; O1 89f33820; BS2 c66335fd; O4 (uncommitted tree); B1 288c2a78. **r1 re-checks:** `node/integration` 88ed5a76 (O4 merged as `dash-core-vectors`); BS3 36a97b35 (`check-no-std.sh` gone; `contrib/node/check-pure.sh` with `pure-clippy.toml`); O1 `node/o1-oracle-sys` c4657bc0 (both pins in one process, `Pin::ALL`); `bitcoin_hashes` 1.2.0 |
| Worktree | `~/workspace/rdc-wt/cs0-node-primitives`, branch `node/cs0-node-primitives` (from `node/integration`) |

> **EMBARGO-1.** This document refers to an unfixed Dash Core security issue only by its codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. The parts of CS0 under PLAN §7 handling are specified in a private companion note in the same directory, readable per PLAN §7.2. This document describes their interfaces, locations and procedures only.

**Changed in r1:** §0, §1, §2, §3, §4.2–§4.5, §5, §6, §7, §8.1, §8.3, §8.4, §9 (rewritten), §10, §11, §12, §13, §14, §15; §16 is new. §4.1, §6.4 and §8.2 are unchanged.

**Changed in r2:** §2 (lints, dev-dependencies), §3 (layout), §4.2 (`CoreSerialize` sealed; `CompareWriter` past the end), §4.3 (marker placement), §4.5 (`CoreBytes` constructors), §6.1 (`Decoded` fields private), §9.1 (recorded rows against live rows; property output rules), §9.3 (runner invocation and argument check), §9.4 (empty corpora, receipt ids, `pending`/`required`), §9.5, §9.6 (`block_txs_decode`), §10, §11, §13 (DEC-30), §14, §15, §16 (round-2 dispositions).

---

## 0. Summary

CS0 creates the pure crate `dash-consensus` and fills in the layer every later consensus task builds on:

1. **`core_serialize`**, the only encoder node code uses for transactions, headers and blocks. It writes every field itself and never calls base-sdk's `encode`. Transaction fields are written by the marked module `primitives::canonical`, which the sighash serializer (CS1) also uses. Bytes that leave the node or reach disk carry the type `CoreBytes` (§4.5).
2. **`Tx`, `Header` and `Block`**, immutable wrappers around base-sdk's `Transaction` and `BlockHeader`. Every cached hash is a function of the object alone: the full serialization hash, the stable asset-unlock hash when the transaction has that shape, the X11 header hash and the merkle roots. The two values that depend on the release, the txid and the block's merkle root, are **selected by accessors that take the release rules** (§5.1). No object carries release-dependent state.
3. **The decoders**, free functions over a byte slice. One pass gives the objects and every hash. Whether the received bytes equal Core's serialization (`wire_is_canonical`) is decided **exactly**, by comparing Core's serialization against the wire range while it is produced (§6.2). The hashes are taken over the wire range when the two are equal, and over `core_serialize` otherwise.
4. **`ValidationError`**, generic over Core's two result enums, with Core's exact reject reason and debug message, and Core's two different tx-to-block conversions (§8.1).
5. **`ConsensusCtx`** and the interfaces it references: `BlsCtx`, `UngatedRules`, `BlockId`, `HeaderCtx`, `ChainAncestry`, `DeploymentView` and `CheckpointView`. These are types only; CS3, CS4 and CH1 own the semantics.
6. **Evidence.** Unit tests, Core vectors and an explicit boundary table run on every PR. The L1 targets `diff_tx_decode`, `diff_block_decode` and `diff_header_decode` run against O1's oracle per pin, only through one runner that keeps every artefact private and fails closed (§9.3). The BS2 corpus runs through the wrappers. Criterion benches are compared with matching Core workloads.
7. **The private PLAN §7 deliverables:** the `quirk_01_embargo_1` test over the private corpus, quirk 1's vectors, the corpus layout, scan (g) and the private L3 operator definition. Their content lives only in `~/private/embargo/` (§10).

---

## 1. Scope

**In scope.**
- The new crate `dash-consensus`: its manifest, licence, lints, CI group and `ARCHITECTURE.md`.
- `primitives`: `Tx`, `TxRef`, `Header`, `Block`, the decoders, `core_serialize`, `CoreBytes`, `canonical`.
- `merkle`: Core's `ComputeMerkleRoot` and `BlockMerkleRoot`.
- `validation`: `ValidationError`, `TxValidationResult`, `BlockValidationResult`.
- `release`: `Release`, `UngatedRules` (the consensus slice of A1 §2.25).
- `ctx`: `ConsensusCtx`, `BlsCtx` (or its re-export, §8.3), `bls_ctx_for_block`, `BlockId`, `HeaderCtx`, the three view traits, `Deployment`.
- The L1 targets, their runner and the private sink (§9.3), and the BS2 leg (§9.2).
- The PLAN §7.2 deliverables (§10).

**Not in scope.**
- Any consensus *rule*: `check_transaction`, `check_block`, the contextual checks, sigop limits and the cached sigop count (CS3; CS1 counts sigops, DEC-25).
- Script and sighash (CS1). CS0 provides the field writers CS1 must use.
- Deployments and versionbits semantics, including `DeploymentDIP0003Enforced` (CS4). Coins and undo codecs (CS5a).
- `BlsCtx::for_tip(view)` (dashd-chainstate). The full `ReleaseProfile` aggregate (DEC-24 D3).
- Payload typing and BLS decode (BS6a, BS11a-t): `Tx::payload` takes a `BlsCtx` now, so it can adopt them without an API change.
- Fixing base-sdk's decode domain (BS4a). CS0 inherits it (§6.4).
- The SHA-256 backend (SH1, DEC-28). CS0 provides the seam it plugs into (§7).
- The fuzz farm and triage automation (O3). CS0 writes the targets, the runner and the sink (§9.3).
- Block-file I/O, storage and relay (ST2, CH*, P*). CS0 gives them the bytes to store and relay, as `CoreBytes`.

---

## 2. Crate setup

| Item | Choice |
|---|---|
| Directory, package | `dash-consensus/`, package `dash-consensus`, lib name `dash_consensus` |
| Edition, MSRV | workspace (2024, 1.89) |
| Licence | **MIT**, with Dash Core's copyright notice in `dash-consensus/LICENSE` (DEC-07; it ports Core code) |
| Dependencies | `dash-primitives`, `dash-types`, `dash-num`, `dash-params`, `dash-pow` from base-sdk, through `workspace = true` and the BS3 `[patch]` table (BS3's guide: add each new package to both tables, then `cargo metadata` and `check-base-sdk-pins.sh`). `bitcoin_hashes` 1.x, the version base-sdk already pulls in, **with its `std` feature named explicitly**, so its runtime CPU dispatch is on (§7) |
| Dev-dependencies | `dash-core-vectors` (O4, merged) for Core's data files, `node-sink` (§9.3; the private sink, used by the property tests and the quirk driver), `proptest`, `criterion`, `serde`/`serde_json`, `hex-conservative` (the quirk driver's manifest digests use `bitcoin_hashes`' SHA-256) |
| Unsafe | `#![forbid(unsafe_code)]` |
| `std` | an ordinary `std` crate (A1 §1.5, X-1): `Tx` uses `OnceLock` |
| CI group | `node-pure` in `.github/ci-groups.yml`. BS3 dropped the `no_std` job (DEC-24 D1; no `check-no-std.sh` at 36a97b35) |
| Rule R2 | nothing to add: `dash-consensus` is in no `wrappers` list of `deny.toml`, so it cannot depend on the `dashcore` stack, and `check-r2.sh` covers the graph |
| CS2's modules | CS2's phases A and B live in their own `dash-consensus` submodules on a provisional `lib.rs` (DEC-29). Commit 1 reconciles that `lib.rs` with this layout |

**Lints.**
- **Purity** comes from BS3's mechanism: `contrib/node/check-pure.sh` runs clippy on every `node-pure` crate with `contrib/node/pure-clippy.toml` (I/O, threads, clocks) and `pure-deny.toml`. CS0 adds no copy of those bans.
- **Determinism** (A1 §2.1 #7): CS0 adds `std::collections::HashMap` and `HashSet` to `disallowed-types` in the shared `pure-clippy.toml`, because the rule holds for every pure crate (consensus code uses `BTreeMap` or a sorted `Vec`). §13 N6.
- **Sole encoder, node-wide part** (Opus m2-2): the root `clippy.toml` gets `disallowed-methods` `dash_primitives::Block::merkle`, base-sdk's **inherent** merkle helper, which hashes the leaves through base-sdk's own encoder. Every workspace crate without its own `clippy.toml` reads the root file, and `check-pure.sh` prepends it to the pure bans. Crates that do not depend on base-sdk are unaffected: clippy 0.1.99 ignores a configured path whose crate is not a dependency (checked). base-sdk's transaction and header hashes are not inherent (they are `Hashable` implementations, `block.rs:70`, `transaction.rs:325`), so they fall under the next item.
- **Sole encoder, crate-local part** (A1 §1.3): `dash-consensus/clippy.toml` repeats the root file's contents (clippy uses the nearest file and does not merge) and adds `disallowed-methods` `dash_types::codec::BaseCodec::encode` and `dash_types::Hashable::hash`. `dash-consensus` never needs either: hashes come from the wrappers, bytes from `core_serialize`.
- **Why the trait bans stay crate-local** (Opus M-6). base-sdk implements `Hashable` for about twenty types that are not transactions, headers or blocks (simplified MN-list entries, the CbTx and ProUpServTx payloads, public keys, P2P fields), and clippy resolves a trait-method call to the trait item, so it can ban a trait method only for every implementation at once. A node-wide ban would forbid legitimate uses in `dash-evo` and the runtime crates. The rule crosses crate boundaries as a type instead: everything that stores, relays or prints consensus bytes takes `CoreBytes`, which only CS0's encoder or an exact wire comparison can produce (§4.5), and `ARCHITECTURE.md` states the rule.

---

## 3. Module layout

```
dash-consensus/
  Cargo.toml  LICENSE  clippy.toml  ARCHITECTURE.md
  src/lib.rs                  #![forbid(unsafe_code)]; module docs; re-exports
  src/hash.rs                 sha256d, sha256d_64_many, x11: the one place SH1 swaps backends
  src/primitives/mod.rs       re-exports base-sdk shapes; Tx, TxRef, Header, Block, decoders, CoreSerialize, CoreBytes
  src/primitives/serialize.rs CoreWrite sinks (Vec, HashWriter, SizeWriter, CompareWriter); CoreSerialize; CoreBytes;
                              compact-size and integer writers; header and block
  src/primitives/canonical.rs every transaction field's encoding; every Core predicate on CTransaction (marked, PLAN §7)
  src/primitives/tx.rs        Tx
  src/primitives/block.rs     Header, Block
  src/primitives/decode.rs    decode_tx, decode_tx_exact, decode_block, decode_header, Decoded, DecodeError
  src/merkle.rs               compute_merkle_root
  src/validation.rs           ValidationError<C>, TxValidationResult, BlockValidationResult
  src/release.rs              Release, UngatedRules, HeaderTimeSource
  src/ctx.rs                  ConsensusCtx, BlsCtx, bls_ctx_for_block, BlockId, HeaderCtx, traits, Deployment
  tests/core_vectors.rs       tx_valid/tx_invalid decode and re-encode; block813851
  tests/recorded.rs           the §9.1 rows with recorded expectations: merkle, error strings, txid display
  tests/properties.rs         the §9.1 properties, under the §9.5 output rules
  tests/quirk_01_embargo_1.rs PLAN §7 (marked); schema-opaque driver over $DASH_EMBARGO_DIR/quirk1/
  benches/decode.rs           block_decode_hash, block_txs_decode, merkle_root_9001

node-sink/                    workspace member, publish = false, std only, no dependencies
  src/lib.rs                  the private sink (marked): run directories, modes, divergence records, receipts,
                              and the fuzz targets' init-hook check (§9.3)

node-fuzz/                    (D5: created by CS0 if O3 has not; workspace-excluded)
  run.sh                      the only way to run a target (§9.3)
  fuzz_targets/diff_{tx,block,header}_decode.rs
  .gitignore                  artifacts/, corpus/, crash-*, leak-*, timeout-*, oom-*, fuzz-*.log

dash-conformance/             (BS2's crate; CS0 adds, behind feature `oracle`)
  src/l1/mod.rs               compare_tx/compare_block/compare_header, Divergence
  tests/l1_boundaries.rs      the §9.1 codec rows, compared live against O1
  tests/l1_twins.rs           proptest twins of the three targets
  tests/private_seed_replay.rs   (marked) replay of the private seeds of every L1 target
```

`merkle` sits at the crate root rather than under CS3's `block` module. `Block` needs it at construction, and `dash-evo` uses the same `ComputeMerkleRoot` for `merkleRootMNList`, `merkleRootQuorums` and the v4 asset-unlock root.

Every ported function carries an anchor, `// Core: <file>:<line> <Function>`, pinned to v24.0.0-rc.3. For code that differs between the pins, the anchor also gives the v23.1.8 line (A1 Appendix E).

---

## 4. Canonical bytes

### 4.1 The rule (A1 §1.3, FR-CON-04)

Core hashes its own re-serialization of a decoded object (`SerializeHash(*this)`), stores that serialization in `blk*.dat` and relays it. The node does the same. Txids, instance hashes, merkle leaves, the sighash preimage (CS1), block storage (ST2/CH) and relay (P*) all take their bytes from `core_serialize`. base-sdk's encoders never produce consensus bytes, so no base-sdk change can move a txid.

### 4.2 API

```rust
/// A byte sink for Core's serialization.
pub trait CoreWrite { fn write(&mut self, bytes: &[u8]); }
impl CoreWrite for Vec<u8> { .. }
pub struct HashWriter { /* sha256 engine */ }    // finish() -> SHA256d; hashes without buffering
pub struct SizeWriter(usize);                     // GetSerializeSize
/// Compares the bytes written against `wire`, stopping comparing at the first difference, and
/// counts them. No allocation.
pub struct CompareWriter<'a> { wire: &'a [u8], written: usize, equal: bool }
impl<'a> CompareWriter<'a> {
    pub fn new(wire: &'a [u8]) -> Self;
    pub fn written(&self) -> usize;               // = GetSerializeSize of what was written
    pub fn matches(&self) -> bool;                // every byte equal and written == wire.len()
}

/// Core's serialization of a consensus object. Sealed: implemented only for the five types below,
/// so no type outside this crate can supply its own bytes (§4.5).
pub trait CoreSerialize: sealed::Sealed {
    fn core_write(&self, w: &mut impl CoreWrite);
    fn core_size(&self) -> usize { /* SizeWriter */ }
    fn core_serialize(&self) -> Vec<u8> { /* with_capacity(core_size) + core_write */ }
}
impl CoreSerialize for dash_primitives::Transaction { .. }   // delegates to canonical::write_tx
impl CoreSerialize for Tx { .. }
impl CoreSerialize for dash_primitives::BlockHeader { .. }   // 80 bytes, Core's CBlockHeader field order
impl CoreSerialize for Header { .. }
impl CoreSerialize for Block { .. }                          // header ‖ CompactSize(n) ‖ txs

/// The A1 name for the sole consensus encoder.
pub fn core_serialize<T: CoreSerialize + ?Sized>(value: &T) -> Vec<u8>;
```

- `serialize.rs` holds only generic encoders and sinks: little-endian integers, `WriteCompactSize`, byte vectors. Everything that knows a transaction's layout is in `canonical`.
- **`CompareWriter` past the end** (Opus m2-1): once `written` would exceed `wire.len()`, it stops comparing (the result is already "different") and keeps counting, so it never indexes past the end of `wire`; `matches()` also requires `written == wire.len()`, so a shorter re-serialization never matches either. A unit test covers both, and the fuzz targets reach both.
- **`CoreWrite` stays open.** Other crates (CS5a's coin and undo codecs, CS1's sighash) write through it with their own code; only the trait that can mint `CoreBytes` is sealed.

### 4.3 `primitives::canonical` (marked module, PLAN §7 handling)

This module owns Core's serialization of **every** transaction field, in `CTransaction::Serialize` order, and **every** Core predicate on a transaction. Its public surface:

```rust
/// One writer per field of CTransaction::Serialize, in Core's order. Each takes the field's
/// value rather than the transaction, so CS1's CTransactionSignatureSerializer can pass the
/// modified inputs and outputs Core's sighash uses.
pub fn write_version_and_type(tx: &Transaction, w: &mut impl CoreWrite);
pub fn write_prevout(prevout: &OutPoint, w: &mut impl CoreWrite);                  // DEC-25
pub fn write_sequence(sequence: u32, w: &mut impl CoreWrite);                       // DEC-25
pub fn write_input(prevout: &OutPoint, script_sig: &[u8], sequence: u32, w: &mut impl CoreWrite);
    // = write_prevout ‖ CompactSize(script_sig.len()) ‖ script_sig ‖ write_sequence
pub fn write_output(value: i64, script_pubkey: &[u8], w: &mut impl CoreWrite);
pub fn write_lock_time(lock_time: u32, w: &mut impl CoreWrite);
pub fn write_extra_payload(tx: &Transaction, w: &mut impl CoreWrite);   // writes nothing when the tx has no payload field
pub fn write_tx(tx: &Transaction, w: &mut impl CoreWrite);              // the whole CTransaction::Serialize

// Core's predicates on CTransaction (transaction.h:285-338; v23.1.8 :258-304) and the
// asset-unlock predicates (transaction.h:383-395, v24.0.0-rc.3 only; the v23.1.8 rules never
// consult them).
pub fn is_null(tx: &Transaction) -> bool;                      // vin.empty() && vout.empty()
pub fn is_coinbase(tx: &Transaction) -> bool;                  // vin.size() == 1 && vin[0].prevout.IsNull()
pub fn is_special_tx_version(tx: &Transaction) -> bool;
pub fn has_extra_payload_field(tx: &Transaction) -> bool;
pub fn is_platform_transfer(tx: &Transaction) -> bool;
pub fn is_asset_unlock_payload(tx: &Transaction) -> bool;
pub fn is_asset_unlock_with_stable_txid(tx: &Transaction) -> bool;
```

- `write_output` takes `i64` (Core's `CAmount`), not base-sdk's current `Amount`. It already matches BS4a's i64 `TxOut.value` and the `CTxOut()` null output (value −1) that CS1's `SIGHASH_SINGLE` path writes.
- **Type comparisons are numeric** (Opus M-5). base-sdk's `TxType` has an `Unknown(u16)` catch-all with a derived `PartialEq`, so `TxType::Unknown(0) != TxType::Spend` although both are `nType == 0`. Every writer and predicate here compares `tx_type.to_base()` as Core compares `nType`; none uses enum equality. `Tx::new` also canonicalises the field (§5).
- The module contains only Core's own expressions, each behind a `// Core:` anchor. There is no fast-path predicate: whether the wire bytes equal Core's serialization is measured, not predicted (§6.2).
- **Marker placement (PLAN §7.1, §7.3).**
  - The module carries the bare comment `EMBARGO-1` once, at its top. Because the module owns every field and every predicate, the marker points at no particular one.
  - Outside it, the marker goes on each hunk that depends on it: the hunk in `decode.rs` that calls the comparison (§6.2), `quirk_01_embargo_1`, the private-seed replay, and the private sink and its loaders (§9.3). The generic `CompareWriter` in `serialize.rs` carries none.
  - Commit messages, comments and docs never say more.
- **Reviews (PLAN §7.4).** The two Claude reviewers with access review `canonical.rs`. Cross-provider reviewers get `git diff node/integration...HEAD -- . ':(exclude)dash-consensus/src/primitives/canonical.rs'`.

### 4.4 Header and block encoding

- **Header:** `version` (i32), `prev_hash`, `merkle_root`, `time`, `bits`, `nonce`, 80 bytes.
- **Block:** header ‖ `CompactSize(txs.len())` ‖ each `canonical::write_tx`.
- **Header hash:** `X11(core_serialize(header))` through `hash::x11`, which calls `dash_pow::hash` (A1 §1.1 X11 row; BS15 owns the backend switch). base-sdk's `BlockHeader::hash()` is not used (sole-encoder rule). It is computed once, when a `Header` is made (§6.3).
- **Transaction offsets in stored bytes** are `80 + CompactSize(n)` plus the prefix sums of `Tx::size()`, the sizes of Core's serialization. Wire ranges are internal to the decoder and are not offsets into what is stored.

### 4.5 `CoreBytes`: consensus bytes as a type (Opus M-6)

```rust
/// Bytes equal to Core's serialization of a transaction, header or block; cheap to clone for
/// relay fan-out. The field is private and there are exactly two ways to make one (below).
#[derive(Clone, PartialEq, Eq)]
pub struct CoreBytes(Arc<[u8]>);
impl CoreBytes { pub fn as_slice(&self) -> &[u8]; pub fn len(&self) -> usize; }
impl AsRef<[u8]> for CoreBytes { .. }
impl fmt::Debug for CoreBytes { /* "CoreBytes(<len> bytes)": never the bytes (§9.5) */ }

/// Constructor 1: CS0's encoder. `T` is one of the five sealed `CoreSerialize` types.
pub fn core_bytes<T: CoreSerialize + ?Sized>(value: &T) -> CoreBytes;
/// Constructor 2: `Decoded::core_bytes()` (§6.1): the received range when the decoder's exact
/// comparison found it equal to Core's serialization, constructor 1 otherwise.
```

- **The constructors, exhaustively** (GPT R2-M1, Opus m2-3): `core_bytes(&value)` and `Decoded::core_bytes()`. There is no `From`/`TryFrom` conversion, no `from_vec`, `from_stored` or unchecked constructor, no `Default`, and no `Deserialize`.
  - `core_bytes` reaches only CS0's encoder, because `CoreSerialize` is sealed: a downstream type cannot implement it and supply its own bytes. Any value of the five types, including a hand-built `Transaction`, has a Core serialization, so the guarantee holds for all of them.
  - `Decoded::core_bytes()` reaches the received range only through the decoder's own comparison, because `Decoded`'s fields are private and it has no public constructor or setter (§6.1).
- **API coverage:** `compile_fail` doctests, with their error codes, show that a downstream crate cannot implement `CoreSerialize`, construct or convert into `CoreBytes`, or assign `Decoded`'s fields.
- **Reading stored blocks back** (ST2, for relay and `getblock`): `decode_block(stored)?.core_bytes()`. That is Core's own path: v24's `ReadBlockFromDisk` decodes the stored block, and relay and RPC re-serialize the object (there is no raw-read path in `net_processing.cpp`). Stored bytes are Core's serialization, so the comparison matches and the received range is reused without re-encoding.
- **The interface rule** (in `ARCHITECTURE.md` and in the ST2, CH, P* and RP briefs, §11): transaction, header and block bytes that are stored (block files), relayed (`tx`, `block`, `headers`, `cmpctblock`, `blocktxn`) or printed (RPC hex) are `CoreBytes`, and never come from base-sdk's encoders.
- Storage and relay APIs that take `CoreBytes` make the rule a compile-time property across crates, which a lint cannot. P2a's outbound message variants for those commands carry `CoreBytes` rather than base-sdk message structs (DEC-30 N3).

---

## 5. `Tx`

```rust
/// Immutable, shared, hash-cached transaction (A1 §1.3). Built only by the decoders (from
/// bytes) or Tx::new (from parts). No field depends on the release.
pub struct Tx {
    raw: Transaction,              // base-sdk shape; tx_type canonical; extra_payload kept as raw bytes
    instance_hash: TxHash,         // SHA256d(core_serialize(tx)): v24's GetInstanceHash, v23.1.8's GetHash
    stable_txid: Option<TxHash>,   // Some exactly when canonical::is_asset_unlock_with_stable_txid(raw)
    size: u32,                     // GetTotalSize() = core_size()
    payload: [OnceLock<Box<Result<SpecialPayload, PayloadError>>>; 2],   // one slot per BLS scheme
}
pub type TxRef = Arc<Tx>;

impl Tx {
    pub fn new(raw: Transaction) -> Tx;                // CMutableTransaction -> CTransaction; canonicalises tx_type
    pub fn raw(&self) -> &Transaction;
    pub fn to_mutable(&self) -> Transaction;           // CMutableTransaction(const CTransaction&)
    pub fn into_raw(self) -> Transaction;
    pub fn txid(&self, rules: &UngatedRules) -> TxHash;    // GetHash() under `rules` (§5.1)
    pub fn instance_hash(&self) -> TxHash;
    pub fn size(&self) -> u32;
    // Core's predicates, delegating to canonical:
    pub fn is_null(&self) -> bool;  pub fn is_coinbase(&self) -> bool;
    pub fn is_special_tx_version(&self) -> bool;  pub fn has_extra_payload_field(&self) -> bool;
    pub fn is_platform_transfer(&self) -> bool;
    pub fn is_asset_unlock_payload(&self) -> bool;  pub fn is_asset_unlock_with_stable_txid(&self) -> bool;
    pub fn payload(&self, bls: BlsCtx) -> Option<Result<&SpecialPayload, &PayloadError>>;
    // plus read-only accessors delegating to `raw`: inputs(), outputs(), lock_time(), tx_type(), extra_payload(), …
}
```

**Hashes (FR-CON-05, A1 §2.25 row 5).**
- `instance_hash = SHA256d(core_serialize(tx))`. This is the same value on both pins: v24's `GetInstanceHash()` and v23.1.8's `GetHash()` for every transaction.
- `stable_txid` holds v24's `ComputeStableUnlockHash`: SHA256d of `core_serialize(tx)` with its trailing 132 bytes (`ASSET_UNLOCK_QUORUM_INFO_SIZE`) zeroed (`core: primitives/transaction.cpp:70-125`, v24.0.0-rc.3). It is computed whenever the transaction has the shape `IsAssetUnlockWithStableTxid` tests (version ≥ 3, type 9, a 145-byte payload, `payload[0] >= 2`), whatever the release.
- `txid(rules)` is `stable_txid` when `rules.au_stable_txid` holds and the shape matches, and `instance_hash` otherwise. `UngatedRules::V23_1_8.au_stable_txid` is false.

### 5.1 Release provenance (GPT B1)

r0 cached a release-dependent txid in `Tx::new(raw, rules)` and then dropped the release, so a `TxRef` built under the v24 rules could feed a v23.1.8 `Block` or context, with a wrong merkle leaf and no way to detect it. r1 removes the release-dependent state altogether:

- **What is cached is release-independent:** `instance_hash` and `stable_txid` above, and in `Block` the merkle root over instance hashes plus, only when some transaction has a stable txid, the root over the v24 leaves (§6.3). All are pure functions of the bytes.
- **What depends on the release is selected, never cached:** `Tx::txid(&self, rules)` and `Block::merkle(&self, rules)`. The compiler makes every consumer name its rules. A `TxRef` decoded in one call can go into a `Block` assembled anywhere, and one `Block` can be checked under both releases; each consumer gets its own release's value, recomputed from nothing.
- **Decoders and constructors take no rules.**

**Why this is the simplest sound option.**
- *A type parameter, `Tx<R: ReleaseRules>`:* the release is chosen at run time (mainnet v23.1.8, testnet v24, regtest `-releaseprofile`), so the parameter would have to be threaded through EV0, CS3, CS4, CH*, the mempool and storage types, and the node monomorphised per release. The `UngatedRules` tables would turn into associated consts. Much more code for the same guarantee.
- *A stored `Release` tag checked on use:* every boundary (block assembly, each context, each cache) needs a check that someone can forget, and a failed check has no remedy but re-wrapping. With rules-indexed accessors there is nothing to check.
- *Cost:* one extra SHA256d for each asset-unlock-shaped transaction and one extra merkle pass for a block that contains one; nothing for any other transaction. v24 Core also hashes such a transaction twice (`GetInstanceHash()` re-serializes it). `stable_txid` (33 bytes) replaces r0's separate `txid` field (32 bytes).

**For later tasks.**
- CS3's `check_block` takes the rules, because Core's `CheckBlock` compares the header against `BlockMerkleRoot`, whose leaves are `GetHash()` (deviation 8).
- Runtime caches keyed by txid (mempool, coins view, indexes) belong to one network and take its rules when they are built. That is a line in their briefs (§11).
- Tests (§9.1, §9.2): the same objects under both rule sets against O1 on each pin; blocks assembled from separately decoded `TxRef`s; and over all mainnet history, both rule sets give equal txids and roots.

**Choices.**
- **`is_coinbase` follows Core**, not base-sdk. base-sdk's `Transaction::is_coinbase` accepts any input count. Core requires exactly one input.
- **`Tx::new` canonicalises `tx_type`** (`TxType::new(tx_type.to_base())`), so callers that build a `Transaction` directly (the miner, tests, CS1) get the decoder's representation. The writers and predicates do not rely on it (§4.3).
- **Payloads stay raw** until a rule asks (A1 §1.3). `payload(bls)` decodes at most once **per BLS scheme**:
  - the decoded result of a CbTx or an asset unlock depends on the global scheme (A1 §1.4.1 per-field table: eager BLS fields with scheme source "global"), so one cached result is not enough;
  - other types decode the same either way, and both slots stay empty until asked;
  - `Box` keeps `Tx` small when the slots are empty, which is the case for nearly every transaction;
  - today the call ignores `bls` and uses base-sdk's `decode_payload`. BS6a/BS11a-t make the scheme real without an API change.
- **No `PartialEq`/`Hash`/`Clone` on `Tx`.** Core's `operator==` compares txids, which is wrong for re-signed asset unlocks. Callers compare `txid(rules)` or `instance_hash()` explicitly, and share through `TxRef`.
- **The cached sigop count** is CS3's (DEC-25: CS1 counts sigops, CS3 owns the limits and the `OnceLock<u32>` slot on `Tx`). The field is private, so adding it is not an API change.
- `Tx: Send + Sync`.

---

## 6. Decoders

### 6.1 API

```rust
/// A decoded object bound to the bytes it was decoded from. Made only by the decoders below; the
/// fields are private and there are no setters, so the binding cannot be changed (GPT R2-M1).
pub struct Decoded<'a, T> {
    value: T,
    wire: &'a [u8],                  // the bytes read from the input
    wire_is_canonical: bool,         // wire == core_serialize(value), decided exactly (§6.2)
}
impl<'a, T> Decoded<'a, T> {
    pub fn value(&self) -> &T;
    pub fn wire(&self) -> &'a [u8];               // wire().len() is the consumed length
    pub fn wire_is_canonical(&self) -> bool;
    pub fn into_value(self) -> T;                 // consumes the binding
}
impl<'a, T: CoreSerialize> Decoded<'a, T> {
    /// The received range when it equals Core's serialization, core_bytes(value) otherwise.
    pub fn core_bytes(&self) -> CoreBytes;
}

/// Reads one transaction and leaves the rest (P2P `tx`, …).
pub fn decode_tx<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Tx>, DecodeError>;
/// Core's DecodeTx: trailing bytes are an error (`sendrawtransaction`, `decoderawtransaction`, …).
pub fn decode_tx_exact(bytes: &[u8]) -> Result<Decoded<'_, Tx>, DecodeError>;
/// Reads one block and leaves trailing bytes. Core's DecodeHexBlk, P2P `block` and block-file
/// reads all accept trailing bytes; the caller applies its own policy.
pub fn decode_block<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Block>, DecodeError>;
/// One 80-byte header and its X11 hash (CH1, `headers`).
pub fn decode_header<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Header>, DecodeError>;

pub struct DecodeError { pub offset: usize, pub tx_index: Option<usize>, pub kind: DecodeErrorKind }
pub enum DecodeErrorKind { Codec(dash_types::codec::DecodeError), TrailingBytes { remaining: usize } }
```

- **The trailing-byte policy belongs to the caller,** because Core's call sites differ (`core: core_read.cpp:105-159`): `DecodeTx` rejects leftovers, while `DecodeHexBlk` and `DecodeHexBlockHeader` ignore them. P2P call sites follow the A1 §2.14 policy table. `wire().len()` (the consumed length) lets every caller apply its own rule.
- **On failure the cursor does not move.** Each decoder works on a copy of the slice and writes it back only on success, so a caller can retry or report from the original position.
- **`DecodeError.offset`** is where the failing item starts, relative to the slice passed to the call: the transaction for `decode_tx`, and for `decode_block` the header, the count or the failing transaction (whose index is `tx_index`).
- `DecodeError` keeps base-sdk's error. Rule DDE (§1.4) and every L1 target compare decode *verdicts*. Core's exception text appears only in `debug.log`; if a later task needs that text for log parity, it maps the error there.
- r0's `TxDecoder`/`BlockDecoder` structs carried only the rules, which decoding no longer needs (§5.1), so they become free functions.

### 6.2 One pass (A1 §1.3)

```
decode_tx(input):
    cur = *input
    raw = <Transaction as BaseCodec>::decode(&mut cur)?       // base-sdk's slice cursor, no VecDecoder copy
    raw.tx_type = TxType::new(raw.tx_type.to_base())          // already canonical from the decoder; cheap
    wire = &input[..input.len() - cur.len()]
    cmp = CompareWriter::new(wire); canonical::write_tx(&raw, &mut cmp)    // EMBARGO-1
    if cmp.matches(): bytes = wire                            // hash the received range: no re-encoding
    else:             bytes = core_serialize(&raw)
    instance_hash = sha256d(bytes)
    stable_txid   = if is_asset_unlock_with_stable_txid(&raw) { sha256d(bytes with the trailing 132 bytes zeroed, in a scratch copy) }
    *input = cur
    Decoded { value: Tx { raw, instance_hash, stable_txid, size: cmp.written(), payload: empty }, wire, wire_is_canonical: cmp.matches() }
```

- **Exact by construction** (Opus M-3). The comparison runs over exactly the bytes `core_write` produces, so `wire_is_canonical` is true if and only if the received range equals `core_serialize(value)`, and the hashes are always over Core's serialization. No sufficiency argument and no assumption about base-sdk's decode domain is involved, and the decoder needs no debug assertion.
- **`size`** is the number of bytes `core_write` produced, which is `GetSerializeSize` by definition.
- **The stable-txid scratch copy** takes the bytes being hashed (the received range on the common path, `core_serialize` otherwise) and never relies on the two being equal.
- **Cost, measured** in a scratch prototype on `block813851.raw` (999 transactions, 749 KB; base-sdk 33fcc59, `bitcoin_hashes` 1.2.0, release profile, instruction counts from `perf stat`, which are stable under load where cycles were not at a load average near 130):

  | Per-transaction work over the block | Instructions, relative |
  |---|---|
  | decode only | 4.7 % |
  | decode + SHA256d of the wire range (r0's fast path) | 100 % |
  | **decode + exact comparison + SHA256d of the wire range (r1)** | **101.8 %** |
  | decode + one traversal that both stream-hashes and compares | 104.6 % |
  | decode + stream-hash of `core_write` only (Core's `SerializeHash`) | 103.1 % |

  The comparison costs 1.8 % of the decode-and-hash work (about 830 instructions per transaction, 1.1 per byte) before X11 and the merkle root, which make the share smaller still. SH1's faster SHA-256 raises the share a little, to roughly 2.5 % at Core's instruction rate. `block_decode_hash` re-measures it (§9.6).
- **`decode_block`:**
  - it decodes the header, then `CompactSize` with Core's `MAX_SIZE` (0x02000000) bound, then calls `decode_tx` per transaction on the same cursor, so ranges come free (no BS13 dependency, A1 §1.3; BS13's `decode_with_ranges` can replace this later as a pure performance change);
  - capacity is reserved in batches capped by `remaining / 10`. Ten bytes is the smallest serialized transaction, so a hostile count cannot force a large allocation;
  - a block with zero transactions decodes, like Core: CS3's `CheckBlock` rejects it;
  - the block's `wire_is_canonical` compares the header and count bytes against their re-serialization and is the conjunction of that and every transaction's flag. ST2/CH store `Decoded::core_bytes()`.
- **No panics on any input.** The fuzz targets check this. No allocation is sized from an unchecked count.
- **Threads.** The pure crate has none (A1 §2.1). CH4 decodes blocks in parallel across blocks. Each decode is self-contained, so that needs nothing from CS0.

### 6.3 `Header` and `Block` (GPT M1)

```rust
/// An immutable block header bound to its X11 hash. Copying it copies the pair.
#[derive(Clone, Copy, Debug)]
pub struct Header { raw: BlockHeader, hash: BlockHash }
impl Header {
    pub fn new(raw: BlockHeader) -> Header;        // computes X11; the only way to pair a header with a hash
    pub fn raw(&self) -> &BlockHeader;  pub fn hash(&self) -> BlockHash;
    // read-only field accessors: version(), prev_hash(), merkle_root(), time(), bits(), nonce()
}

pub struct Block {
    header: Header,
    txs: Vec<TxRef>,
    size: u32,                                     // core_size()
    merkle_full: (MerkleRoot, bool),               // over instance hashes: the v23.1.8 root, and the v24 root when no tx has a stable txid
    merkle_stable: Option<(MerkleRoot, bool)>,     // over the v24 leaves; Some only when some tx has a stable txid
}
impl Block {
    pub fn new(header: Header, txs: Vec<TxRef>) -> Block;   // miner, tests
    pub fn header(&self) -> &Header;  pub fn hash(&self) -> BlockHash;  pub fn txs(&self) -> &[TxRef];
    pub fn size(&self) -> u32;
    /// Core BlockMerkleRoot(block, &mutated) under `rules`: (root, mutated).
    pub fn merkle(&self, rules: &UngatedRules) -> (MerkleRoot, bool);
}
```

- **One X11 per header.** There is no public constructor that takes a hash, so a header cannot be paired with someone else's hash, and no consumer needs to hash again. CH1's `ProcessHeaders` command carries `Vec<Header>` and CS3's header checks take `&Header` (deviation 7). A unit test passes 2,000 decoded headers through stubs with those signatures, reads every hash, and asserts that a test-only X11 call counter in `hash.rs` reads 2,000.
- **Merkle roots** are computed eagerly, at decode or in `new`, for one deterministic cost. The leaves are `GetHash()` (`core: consensus/merkle.cpp:65-73`), so the v23.1.8 root commits to instance hashes and the v24 root to stable txids where they exist (A1 §2.25 row 5). `merkle(rules)` picks `merkle_stable` when `rules.au_stable_txid` holds and it exists, and `merkle_full` otherwise.

### 6.4 Decode domain

The decode domain is base-sdk's, as BS4a fixes it (Rule DDE, A1 §1.4). CS0 adds nothing and removes nothing.

Until BS4a merges, `diff_tx_decode` will report base-sdk's known divergences:
- DDE #1: `TxOut` values above `MAX_MONEY` fail to decode;
- DDE #3: payloads are capped at 10,240 bytes.

These sit on an explicit allowlist in the target, with one line per DDE row. Each line is removed when BS4a lands. CS0 merges after BS4a (`i:BS4a`), so the merged targets carry no allowlist.

---

## 7. Merkle

```rust
/// Core ComputeMerkleRoot (consensus/merkle.cpp:45-63): returns (root, mutated); an empty input
/// gives the zero hash.
pub fn compute_merkle_root(leaves: Vec<[u8; 32]>) -> ([u8; 32], bool);
```

- **"Mutated", exactly as Core computes it** (GPT M4, Opus m-2): on each level, *before* an odd level's last hash is duplicated, the hashes at positions (0, 1), (2, 3), … are compared, and any equal pair sets the flag. Overlapping pairs (1, 2), … are not compared, and neither is the synthetic duplicate. So `[a, b, b]` and `[A, B, B, C]` are not mutated, `[a, a, b]` and `[A, B, C, C]` are, and `[a, b, c, d, e, f]` and `[a, b, c, d, e, f, e, f]` have the same root with different flags (the second is caught one level up). CVE-2012-2459.
- **Cost:** each level hashes ⌈len/2⌉ pairs, which is n − 1 pair hashes for a power of two and at most n − 1 + ⌈log₂ n⌉ otherwise (3 leaves: 3; 5 leaves: 6).
- **Buffers:** each level's hashes are contiguous `[u8; 32]`s, so `as_flattened().as_chunks::<64>()` (stable since 1.88) gives the 64-byte pairs without copying; an odd level pushes its duplicate first. `hash::sha256d_64_many(out, pairs)` writes the next level into a second buffer, and the two are swapped. Both are allocated once per root. No `unsafe`.
- **Backend (GPT M6, DEC-28).** `bitcoin_hashes` 1.2.0's `hash_64_many` already dispatches at run time to its SHA-NI two-way, AVX2 eight-way and SSE4.1 four-way transforms, with `std` or `cpufeatures`, so the merkle root is not scalar on agentbox. Its single-stream engine, which hashes transactions, has no SSE4 path and runs scalar without SHA-NI (DEC-28: about 51.7 instructions per byte against Core's 36.8). SH1 delivers a Core-class backend, and `hash.rs` is where it plugs in. No caller changes.

---

## 8. Types for later tasks

### 8.1 `ValidationError` (FR-CON-31)

```rust
#[repr(i32)] pub enum TxValidationResult { Unset = 0, Consensus, RecentConsensusChange, InputsNotStandard,
    NotStandard, MissingInputs, PrematureSpend, BadSpecial, Conflict, ConflictLock, MempoolPolicy, NoMempool }
#[repr(i32)] pub enum BlockValidationResult { Unset = 0, Consensus, RecentConsensusChange, CachedInvalid,
    InvalidHeader, Mutated, MissingPrev, InvalidPrev, TimeFuture, Checkpoint, ChainLock }

pub trait ResultClass: Copy + Eq + fmt::Debug { fn as_i32(self) -> i32; }

pub struct ValidationError<C: ResultClass> {
    result: C,
    reason: Cow<'static, str>,   // Core's GetRejectReason(), verbatim
    debug: String,               // Core's GetDebugMessage(); empty = none, as in Core
}
pub type TxValidationError = ValidationError<TxValidationResult>;
pub type BlockValidationError = ValidationError<BlockValidationResult>;

impl<C: ResultClass> ValidationError<C> {
    pub fn new(result: C, reason: impl Into<Cow<'static, str>>) -> Self;
    pub fn with_debug(self, debug: impl Into<String>) -> Self;     // replaces the debug message
    pub fn result(&self) -> C;  pub fn reject_reason(&self) -> &str;  pub fn debug_message(&self) -> &str;
}
impl<C: ResultClass> fmt::Display for ValidationError<C> { /* Core ToString() of an invalid state: reason, or "reason, debug" */ }
impl<C: ResultClass> std::error::Error for ValidationError<C> {}
impl TxValidationError {
    /// ConnectBlock (validation.cpp:2701-2702 CheckTxInputs, :2754-2755 CheckInputScripts):
    /// state.Invalid(result, tx_state.GetRejectReason(), tx_state.GetDebugMessage()). Reason and
    /// debug message unchanged.
    pub fn into_block(self, result: BlockValidationResult) -> BlockValidationError;
    /// CheckBlock (validation.cpp:4411-4412) and ContextualCheckBlock (:4567-4568): the tx's reason,
    /// and the debug message strprintf("Transaction check failed (tx hash %s) %s", txid, debug).
    /// `txid` is the tx's GetHash() under the block's rules, printed as Core's uint256::ToString()
    /// (byte-reversed hex). An empty tx debug message still leaves the trailing space.
    pub fn into_block_wrapped(self, result: BlockValidationResult, txid: &TxHash) -> BlockValidationError;
}
```

- **Discriminants equal Core's** (`core: consensus/validation.h`, identical in both pins). O1/O2a's `Rejection.result` integers compare directly.
- **Two types, like Core's `TxValidationState` and `BlockValidationState`,** where A1 Appendix A has one. The compiler then forces an explicit tx-to-block conversion. A wrong class changes P2P scoring, so this is worth the extra type.
- **Two conversions, because Core has two** (GPT M2, Opus m-1). `ConnectBlock` passes the debug message through; `CheckBlock` and `ContextualCheckBlock` wrap it. Neither adds the prefix implicitly. Tests compare both against Core-formatted strings, including the empty-debug case, with the txid's display pinned by a Core-formatted vector.
- **The reason is a `Cow<'static, str>`, not `&'static str`** (A1 §1.3). Core formats some reasons: `bad-version(0x%08x)` (`validation.cpp:4517`), and `mandatory-script-verify-flag-failed (%s)` / `non-mandatory-script-verify-flag (%s)` (`:2212`, `:2223`). Every other reason is a borrowed literal with no allocation.
- **`Display` covers the invalid state only.** Core's `ToString()` of a valid state, and `ValidationState::Error()` (`M_ERROR`, system failures such as disk errors), belong to runtime crates, as their own error type that wraps `ValidationError` for the invalid case.

### 8.2 Release rules (A1 §2.25)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release { V23_1_8, V24_0 }
impl Release {
    /// DEC-05: mainnet pins v23.1.8; testnet, devnets and regtest pin v24.0.x
    /// (regtest may select v23.1.8 with -releaseprofile).
    pub const fn default_for(network: dash_params::Network) -> Release;
}

/// The consensus rules that differ between the pinned releases without a deployment: A1 §2.25's
/// `UngatedRules`, i.e. `ReleaseProfile.consensus`. One static value per release.
#[non_exhaustive]
pub struct UngatedRules {
    pub release: Release,
    pub au_stable_txid: bool,                  // row 5; read by Tx::txid and Block::merkle (CS0)
    pub header_time_source: HeaderTimeSource,  // row 4; read by CS3 and CH1
    // Rows 3 (MNHF valid bits; CS4/EV3), 24 (fJustCheck duplicate rotated commitment; LQ2) and the
    // AU v2 reject string (EV3) are added by those tasks, with values from O17's table.
}
pub enum HeaderTimeSource { AdjustedTime /* v23.1.8 */, NodeClock /* v24 */ }
impl UngatedRules {
    pub const V23_1_8: UngatedRules;  pub const V24_0: UngatedRules;
    pub fn of(release: Release) -> &'static UngatedRules;
}
```

### 8.3 `BlsCtx`

DEC-24 D2: `BlsCtx` lives in base-sdk's `dash-pkc`, next to BS11a-t's `Scheme`, and BS4a's `ProtoCtx` names it. CS0 re-exports it, and until BS11a-t lands it defines a provisional copy of the same shape:

```rust
pub struct BlsCtx { pub legacy: bool }            // A1 Appendix A; replaced by `pub use dash_pkc::BlsCtx`
/// A1 §1.4.1: inside block processing, block h is legacy iff h < V19Height.
pub fn bls_ctx_for_block(params: &ChainParams, height: u32) -> BlsCtx;
```

`for_tip(view)` needs `ChainView`, so it lives in `dashd-chainstate`.

### 8.4 `ConsensusCtx` and the views

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);                      // index into CH1's block-index arena

#[derive(Clone, Copy, Debug)]
pub struct HeaderCtx { pub height: u32, pub hash: BlockHash, pub time: u32, pub bits: CompactTarget,
                       pub version: i32, pub mtp: i64, pub chain_work: Arith256 }

/// CBlockIndex walking; implemented by CH1's arena. Every BlockId passed in must have been issued
/// by this arena, and stays valid for the arena's lifetime; implementations may panic otherwise.
pub trait ChainAncestry: Sync {
    fn header(&self, id: BlockId) -> HeaderCtx;                        // by value: works over a locked or epoch-managed arena
    /// Core GetAncestor (chain.cpp:94-98): None when `height` is above `id`'s height.
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;  // skip list, O(log n)
    fn prev(&self, id: BlockId) -> Option<BlockId>;
}
pub enum Deployment { Buried(dash_params::BuriedDeployment), Versionbits(DeploymentPos) }
pub enum DeploymentPos { TestDummy, V24, EvoShares }   // Core DeploymentPos (v24.0.0-rc.3 superset; v23.1.8 lacks EvoShares)
pub trait DeploymentView: Sync {                  // CS4 implements; per call site, as in Core
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool;   // DeploymentActiveAfter(pindexPrev)
    fn active_at(&self, block: BlockId, d: Deployment) -> bool;             // DeploymentActiveAt(index)
}
/// None when the index holds no checkpoint, and also when checkpoints are disabled.
pub trait CheckpointView: Sync { fn highest_checkpoint_in_index(&self) -> Option<u32>; }

/// One chain position under one release profile: `prev`, `height` (= prev's height + 1),
/// `median_time_past` (prev's), the views, `bls` and `rules` all describe the same chain and
/// profile. CH1 builds it in one place.
#[derive(Clone, Copy)]
pub struct ConsensusCtx<'a> {
    pub params: &'a ChainParams,
    pub height: u32,
    pub prev: Option<BlockId>,
    pub chain: &'a dyn ChainAncestry,
    pub deployments: &'a dyn DeploymentView,
    pub checkpoints: &'a dyn CheckpointView,
    pub bls: BlsCtx,
    pub median_time_past: i64,
    pub header_check_time: i64,      // the time `time-too-new` compares against; the caller picks it per rules.header_time_source
    pub rules: &'a UngatedRules,     // A1: `release: &ReleaseProfile` (deviation 3)
}
```

- **These are interface skeletons for EV0, CS3, CS4 and CH1.** They take A1 Appendix A's signatures, with these changes (§12):
  - `prev` is an `Option`, because Core's `DeploymentActiveAfter(nullptr, …)` is defined for genesis;
  - the traits are `Sync`, so CH4's parallel stages can share a context;
  - `ancestor` returns `Option`, as Core's `GetAncestor` returns null, which DGW and versionbits callers rely on; `header` returns `HeaderCtx` by value (Opus m-4, GPT minor);
  - `dip0003_enforced` is not a view method: Core's `DeploymentDIP0003Enforced(nHeight, params)` is a `constexpr` height comparison (`deploymentstatus.h:66`; v23.1.8 `:53`), and each call site passes its own height. CS4 ports it as a free function of `(height, params)` (Opus m-3);
  - `adjusted_time` is renamed `header_check_time`: in v24 it is not the adjusted time but the profile's chosen clock.
- Their owners review them before EV0 starts (PLAN: EV0's acceptance is an API review by the EV1/EV2a/LQ1a owners). After that, changes go through the same review.
- `EhfSignalView` and the versionbits cache stay with CS4.
- The full `ReleaseProfile` is assembled in a runtime crate, with `consensus: &'static UngatedRules` as one field (DEC-24 D3).

---

## 9. Verification

### 9.1 Every PR

**Where expectations come from** (Opus P2-B1). Committed expected values are allowed only where no transaction is decoded or encoded from generated inputs: the merkle cases (symbolic leaves), the `ValidationError` strings, the txid display, the header hand-off, and Core's own public data files. **Every codec row of the boundary table takes its expectations live from O1 at test time; they are never committed.** No row is dropped for this: the table's coverage is unchanged from r1.

**`cargo test -p dash-consensus`:**
- **Unit tests per module:**
  - writers against Core's byte layout, on hand-built standard transactions; `write_input` equals `write_prevout ‖ script ‖ write_sequence`;
  - `Tx::new` and the decoders give the same hashes; `Tx::new` canonicalises `tx_type`;
  - `is_coinbase` and the other predicates follow Core's definitions, with types compared numerically (a constructed `Unknown(n)` that shadows a named variant behaves as the variant);
  - `CompareWriter` with output longer and shorter than the wire range (§4.2);
  - the header hand-off: one X11 per header (§6.3);
  - the `compile_fail` doctests of §4.5.
- **Recorded rows** (`tests/recorded.rs`):
  - merkle: empty; one leaf; `[a,b,c]` and `[a,b,b]` not mutated; `[a,a,b]` mutated; `[A,B,B,C]` not mutated, `[A,B,C,C]` mutated; `[a,b,c,d,e,f]` and `[a,b,c,d,e,f,e,f]` with equal roots and different flags; 3 and 5 leaves for the pair count. Expected roots and flags come from O1's `merkle_root`, recorded once with provenance;
  - `ValidationError`: `Display` equals Core's `ToString()`; `into_block` and `into_block_wrapped` equal Core's two formats, including the empty-debug case; the txid display against a Core-formatted value.
- **Core vectors** through O4's `dash-core-vectors`:
  - every transaction in `tx_valid.json` and `tx_invalid.json` decodes, and `core_serialize` reproduces its bytes;
  - `src/bench/data/block813851.raw`, imported through O4's `sync-core-vectors.sh` with a provenance entry, decodes to Core's hash, merkle root and transaction count.
- **Properties** (`tests/properties.rs`, `proptest`; Opus M-4a). Generators cover every integer field over its full range and build `TxType` through `TxType::from_base`:
  - **idempotence:** `core_serialize(decode(core_serialize(x)).value()) == core_serialize(x)`, and that decode is `wire_is_canonical`, for every generated `x`;
  - **identity where canonical:** for a decoded value `d` with `wire_is_canonical`, `decode(core_serialize(d.value())).value() == d.value()`;
  - `wire_is_canonical == (core_serialize(value) == wire)` and `size == core_size(value)` for every decode of arbitrary bytes;
  - the decoders never panic on arbitrary bytes.

  **Output rules for the properties** (Opus M2-1), the same as the twins': the generated seed sits in a newtype whose `Debug` prints a fixed placeholder; `failure_persistence: None` (no `proptest-regressions/` in the worktree); no shrinking; comparisons are `prop_assert!(a == b, "<field>")`, never `assert_eq!`, `assert_ne!`, `prop_assert_eq!` or `prop_assert_ne!` on values derived from inputs; the §9.5 lint set and reviewer grep. A failure reports the property and the field. When `DASH_EMBARGO_DIR` is set, the seed goes to the private sink first (`node-sink`), so a designated reader can reproduce it.
- **`quirk_01_embargo_1`** (§9.4, §10).

**`dash-conformance`, feature `oracle`, every PR, beside the twins: the boundary table** (`tests/l1_boundaries.rs`, GPT M4). Each row's inputs are built deterministically in code and passed to the shared `compare_tx`/`compare_block`/`compare_header` (§9.3), which take Core's answer from O1 on both pins **at test time**. O1's codec entry points are streaming, so `decode_tx_exact` is checked as "stream decode succeeds and consumes everything", which is Core's `DecodeTx`. The rows run under the §9.5 output rules: a failure reports the area, the row's index and the field, and the reproducer goes to the sink when the variable is set.

| Area | Cases |
|---|---|
| CompactSize | minimal encodings at 252/253, 0xFFFF/0x10000 and 0xFFFFFFFF/0x100000000; non-minimal forms; values at and above `MAX_SIZE`; for the input and output counts, both script lengths, the payload length and the block's transaction count |
| Integer fields | every integer field at its minimum and maximum and at Core's named boundaries, alone and combined; output values −1, `i64::MIN`, 0, `MAX_MONEY`, `MAX_MONEY + 1`, `i64::MAX` decode wherever Core decodes them (rules reject later, CS3) |
| Shapes | empty inputs and empty outputs; every transaction type 0–12, 13 and the largest value, with and without a payload; payload lengths 0, 1, 10,000, 10,240, 10,241 and near Core's decoder bound |
| Truncation and suffixes | every prefix of one representative transaction of each shape, of a header and of a small regtest block, and every transaction boundary ±1 of `block813851`; a complete transaction, header and block followed by a suffix; two transactions back to back. Asserted: the cursor, `wire().len()`, an unmoved cursor on failure, `decode_tx_exact`'s `TrailingBytes { remaining }`, and that the streaming decoders leave the suffix |
| Stable txid (Opus M-4b) | type 9 with payload lengths 144, 145, 146; `payload[0]` 1, 2, 3, 255; tx versions 2, 3, 4; type ≠ 9 with a 145-byte payload; changes to the trailing 132 bytes against changes to the first 13 payload bytes. Each under both rule sets, against O1 on the matching pin: Core's serialization, txid, instance hash, and the root of a block that contains it. The cases of Core's `src/test/evo_assetlocks_tests.cpp:525-545` (txid against instance hash, re-signed instances) are ported as inputs. All of them also seed `diff_tx_decode` |
| Release provenance (GPT B1) | one `Block` with such a transaction checked under both rule sets against O1's roots on each pin; the same `TxRef`s assembled into a `Block` by `Block::new` from separate decodes |

Both hashing paths of §6.2, the received range and `core_serialize`, are exercised against these independent expectations.

### 9.2 BS2 full history through the wrappers

BS2's `dash-conformance` gets a third leg, the `dash_consensus` wrappers. For every best-chain and stale block of mainnet and testnet, under the network's rules:
- **RPC mode:** the block hash, every txid and the merkle root equal the oracle's `getblock 1`.
- **blk mode:** the hash chain links as BS2 checks it, and **`core_serialize(block)` equals the stored bytes**. Core stores its own re-serialization, so this checks the encoder against all of history.
- **Both modes:**
  - **every block and every transaction is `wire_is_canonical`**: stored and RPC bytes are Core's serialization, and the comparison is exact (§6.2), so any other result is a bug in the encoder or the comparison;
  - the block is not mutated;
  - `size` equals the stored length.
- **Both rule sets on mainnet** (Opus m-8): every txid and merkle root is equal under `V23_1_8` and `V24_0`. This turns A1 §2.25's argument that row 5 never fires on mainnet history into evidence, which the mainnet comparison against the v24 oracle rests on. On testnet the leg reports how many transactions have a stable txid.

It runs on BS2's incremental oracle timer, read-only and at idle priority, like BS2's own passes. The oracles were still syncing at the time of writing (mainnet ≈ 650K of 2.55M blocks). **CS0's merge evidence is partial history**, labelled as such: the networks, rule sets, height ranges, stale-block counts and the pinned revisions (rust-dashcore, base-sdk, oracle binaries) it covered, plus the v24 regtest chains of `conformance/tests/data/`. PLAN's full-history criterion is met only when the oracles reach the tip (an M1 exit item, not CS0's merge).

### 9.3 L1 differential targets (A1 §4.4)

**Targets.**
- **`diff_tx_decode`**, per pin, `Pin::V23_1` with `UngatedRules::V23_1_8` and `Pin::V24_0` with `UngatedRules::V24_0`. Compared: the decode verdict and `wire().len()`; `core_serialize` against Core's re-serialization; txid and instance hash.
- **`diff_block_decode`:** the same, plus the block hash, merkle root and `mutated`.
- **`diff_header_decode`:** decode, hash and re-encoding. It is cheap and covers `decode_header`.
- Each comparison is one function in `dash_conformance::l1` (`compare_tx(bytes, pin) -> Result<(), Divergence>`, `compare_block`, `compare_header`), called by both the libFuzzer target and its proptest twin, so the two forms cannot drift (Opus m-12). Every L1 target, including later tasks' (CS1, CS3, …), registers its comparison by name in `dash_conformance::l1::TARGETS`; the private-seed replay finds targets there (§9.4).
- **Seeds:** all mainnet and testnet transactions and blocks (BS2); O4's Core vectors; the stable-txid cases of §9.1; the regtest v24 chains; mutated blocks (duplicated transaction tails, swapped pairs); and the private seeds in `$DASH_EMBARGO_DIR/fuzz/<target>/seeds/`.
- **Where.** The targets go in `node-fuzz` (workspace-excluded, libFuzzer, O1's `fuzz` oracle flavour). If O3 has not created `node-fuzz` when CS0 implements, CS0 adds the minimal crate (manifest, the three targets, the runner, a README) and O3 builds the farm around it (DEC-24 D5).

**Containment (Opus M-2, GPT M3).** libFuzzer prints crash inputs of up to 256 bytes in Base64, writes `crash-*` files to its artifact prefix (by default the working directory), saves every coverage-increasing input to its corpus directory, and prints file paths when it replays them. All of that goes to one private sink. The rule is the same for **every L1 target in `node-fuzz`**, so tracked code classifies none of them:
- **The runner is the only way to run a target.** `node-fuzz/run.sh <target> --pin <v23.1|v24.0> --cpu-hours H [--jobs J]`:
  1. refuses to start unless `DASH_EMBARGO_DIR` is set and its `fuzz/` directory exists with mode 0700; it never sets the variable;
  2. creates `fuzz/<target>/{corpus,seeds}/` if missing and a run directory `fuzz/<target>/runs/<run-id>/{divergences,crashes,logs}/`, with modes 0700/0600 set explicitly (agentbox's umask is 002);
  3. builds the target (`cargo fuzz build`) and **invokes the built binary directly**, never `cargo fuzz run`, which adds its own `-artifact_prefix` (Opus m2-5). It runs from inside `logs/` (with `-jobs`, libFuzzer writes `fuzz-N.log` into the working directory), with `TMPDIR` pointing at `<run>/tmp/`, a fixed argument list (`-artifact_prefix=<run>/crashes/`, the time and job limits, and the corpus directories in the order `fuzz/<target>/corpus`, the only one libFuzzer writes to, then `fuzz/<target>/seeds`, then the public seeds), and its stdout and stderr redirected into `logs/`. It passes the pin and run id in `NODE_FUZZ_PIN` and `NODE_FUZZ_RUN`. Merging and crash minimisation are runner modes (`--merge`, `--minimize <crash>`) that set those flags' output paths inside the run directory;
  4. afterwards writes `<run>/verdict.json`: target, pin, the rust-dashcore and base-sdk revisions, the oracle build's pin and hash, toolchain, CPU-hours (the children's user and system time), executions, divergences, crashes, sink write failures and status;
  5. prints **one public summary line** with those fields and no paths, and exits non-zero unless the status is `pass`: no divergence, no crash, no sink failure, and the CPU-hour budget reached.
- **The target refuses to run outside the runner.** A shared helper, `node_sink::fuzz_init(target)`, runs in libFuzzer's initialisation hook (`fuzz_target!(init: …)`), before any input executes. It checks the variable, the run id and the run directory's mode, and then the whole argument list (Opus m2-5):
  - only the flags the runner uses are accepted, each at most once, because libFuzzer takes the last occurrence of a repeated flag;
  - every path-valued flag (`-artifact_prefix`, `-exact_artifact_path`, `-merge_control_file`, `-features_dir` and the like) and every positional corpus directory lies inside the run directory or `fuzz/<target>/`;
  - the first positional directory, where libFuzzer writes new inputs, is `fuzz/<target>/corpus`, or for a smoke's fuzzing run (not a merge) the run's own sample of it, `<run>/sample` (BL-333), so that the smoke never writes the working corpus;
  - `-fork`, `-merge` and `-minimize_crash` are accepted only in the shapes the runner's own modes produce.

  Otherwise it writes one fixed line and exits. Exiting there never dumps an input, which exiting inside the fuzz callback would.
- **Divergences never panic.** On a mismatch the target writes the input to `divergences/<sha256>` (0600) with a sidecar naming the target, pin and field, logs one line (target, the first 12 hex digits of the sha256, the field) to the redirected stderr, and continues. A sink write failure stops the run, and the runner reports it as a sink failure. Genuine crashes abort as usual, into the run directory.
- **Gate policy.** A leg with any divergence, crash or sink failure fails. After the fix the leg starts a fresh budget (the working corpus is kept; the CPU-hours count from zero). Only the summary lines leave the private tree; the manager releases non-embargo reproducers to git after checking them (PLAN §7.5).
- **Runner self-test** (feature `selftest` of `node-fuzz`): a target variant whose comparison reports a mismatch on a fixed public input. The self-test points the variable at a fresh empty directory it creates under `/work/scratch/` (never the private tree), runs the runner, and asserts status `fail`, exactly one divergence file, and no input bytes on stdout. A second run with a read-only sink asserts `fail` with the fixed diagnostic. Part of CS0's merge evidence.
- **Proptest twins** (`dash-conformance/tests/l1_twins.rs`, feature `oracle`, every PR), so every PR gets a smoke run without libFuzzer and without private data:
  - inputs are built from a seed in a newtype whose `Debug` prints a fixed placeholder; `failure_persistence: None`; no shrinking;
  - on a divergence: if `DASH_EMBARGO_DIR` is set, the reproducer and seed go to the sink first; then the test panics with the target, the field and the sink id (or "not recorded" when the variable is unset). It never uses `assert_eq!`.
- **Budget** (GPT M3, Opus m-7): ≥ 24 CPU-h **per pin** for each of `diff_tx_decode` and `diff_block_decode` (four legs), and ≥ 1 CPU-h per pin for `diff_header_decode` (a fixed 80-byte input), each leg recorded separately. They run after BS4a has merged and the allowlist of §6.4 is gone (NFR-CEQ-04).

### 9.4 Private tests in the gate (DEC-24 D6)

- **No box-wide `[env]`, and no file in the rust-dashcore tree points `DASH_EMBARGO_DIR` at the private tree** (no `.cargo/config.toml`, `Justfile`, CI group or script). Ordinary task runs never load private data.
- **Unset:** `quirk_01_embargo_1` and `private_seed_replay` pass after writing one fixed, codename-only line with `std::io::stderr().write_all`. libtest captures `eprintln!` from passing tests (verified by the Opus reviewer), so that would not be seen.
- **Set: the quirk corpus must be complete and non-empty** (GPT R2-M2). `quirk1/MANIFEST` starts with `cases <N>` and `min <M>`, where `M ≥ 1` is a floor that only rises as cases are added. The test **fails** if the manifest is missing, if `N < M` (so an empty corpus always fails), if any listed case file is missing or has a different sha256, if an unlisted case file is present, or if it checked any number of cases other than `N`.
- **Set: the private-seed replay** (`dash-conformance/tests/private_seed_replay.rs`). `fuzz/MANIFEST` has `min <M>` (`M ≥ 1`) and one entry per target, each with a state (Opus M2-2):
  - `required`: the target must exist in the tree under test, and every listed seed must be present, unaltered and pass. A missing `required` target fails;
  - `pending`: seeds written for a target that has not merged yet. They are replayed when the target exists in the tree under test (it is registered in `dash_conformance::l1::TARGETS`, §9.3) and skipped otherwise;
  - a seed directory with no manifest entry fails.

  The replay fails if the `required` entries hold fewer than `M` seeds or if it executed no seed at all. The CS0 owner writes new entries as `pending`, and **the manager flips an entry to `required` when its task merges, as a step of that task's DEC-30 merge checklist.** A seed written on one task's branch therefore never fails another task's gate, and once the task merges its seeds are mandatory.
- **Receipts carry the gate's run id** (Opus m2-4). The gate passes `DASH_EMBARGO_RECEIPT_ID`. On success each test writes `$DASH_EMBARGO_DIR/.receipts/<run id>/<test>` (mode 0600; one directory per run, so gates running at once do not overwrite each other's receipts, BL-49) with that id and the number it executed (for the replay, also the number of seeds applicable to the tree under test), and prints one fixed line with the count. A receipt from another gate's run cannot satisfy this one. The replay's public output is **one aggregate line** (pass or fail, total count, no per-target numbers).
- **The manager's gate** sets the variable and the receipt id for its test processes only. After `cargo test --workspace` (and `dash-conformance` with `oracle`) it asserts, for each receipt: its own run id; a **positive** count; for the quirk test, a count equal to the manifest's `N`; for the replay, an executed count equal to the applicable count and at least the `required` seeds. It reports one line for each. A Cargo exit status of 0 is not enough.
- **Self-tests on synthetic corpora** (public, no private data). The quirk driver and the replay are functions of a directory, and their own tests run them over synthetic trees under `CARGO_TARGET_TMPDIR`: an empty manifest (`cases 0`), `N < M`, a missing file, an altered file, an unlisted file, a `required` entry without its target, and a `pending` entry without its target. Each fails with its fixed, sanitized message and writes no receipt, except the `pending` case, which is skipped while a `required` entry beside it runs.

### 9.5 No leaks through test output

Cross-provider reviewers and implementers without access run `cargo test --workspace` and see its output (PLAN §7.4: "reviewers see the results, never the inputs"). Therefore:
- `quirk_01_embargo_1` reports the case number and the mismatching field only, never inputs or expected values; private case IDs and file names are opaque (`case-001`, sha256), because paths appear in output;
- the property tests, the boundary rows and the twins print seeds only as a fixed placeholder and never persist failures in the worktree (§9.1, §9.3);
- fuzz output stays in the private run directory (§9.3);
- `CoreBytes`'s `Debug` prints its length only (§4.5);
- **mechanical discipline** in the quirk driver, the private-seed replay, `node-sink`, the twins, the boundary rows (`l1_boundaries.rs`) and the properties (`properties.rs`): `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::use_debug, clippy::dbg_macro, clippy::print_stdout)]`, and no `assert_eq!`, `assert_ne!`, `debug_assert_eq!`, `prop_assert_eq!` or `prop_assert_ne!` on any value derived from an input (they print both operands' `Debug`, and base-sdk's `Transaction` derives `Debug`). The reviewers grep those files for the macros.

### 9.6 Benchmarks (BG: crit `block_decode_hash` vs B1 `microbench`) (GPT M6)

**Rust** (`benches/decode.rs`, criterion), each iteration decoding and dropping:
- `block_decode_hash`: `decode_block(block813851.raw)`: the per-transaction comparison and hashes, the header's X11 and the merkle root;
- `block_txs_decode` (benchmark-only code, GPT R2-m1): the work of Core's `DeserializeBlockTest` with the same boundaries: decode the 80-byte header with base-sdk's decoder (no X11), decode the transaction count, `decode_tx` each transaction into a `Vec<TxRef>` (an `Arc` per transaction, as Core's `CTransactionRef`), keep the vector until the end of the iteration and drop it there;
- `merkle_root_9001`: Core's `MerkleRoot` bench shape, 9,001 random leaves (input preparation outside the timed loop).

**Core** (`bench_dash`, 24.0.0-rc.3, built with B1's `build-dashd-bench.sh`):
- `DeserializeBlockTest` (`src/bench/checkblock.cpp:21-33`), which matches `block_txs_decode`: it decodes the header and count, builds every `CTransaction` (and so its hash) into the block's vector of shared references, destroys the block in each iteration, and computes neither X11 nor the merkle root. The remaining differences are recorded with the numbers: Core rewinds its stream inside the timed loop, and Core hashes by re-serializing where the Rust row compares and hashes the received range, which is the difference being measured;
- `MerkleRoot`, which matches `merkle_root_9001`;
- a new `DeserializeBlockHashMerkle`, which deserializes, calls `GetHash()` and `BlockMerkleRoot(block, &mutated)`, and constructs and destroys the `CBlock` in each iteration. It matches `block_decode_hash` and is a test-only patch on a local Core `node/*` branch (DEC-16; §13 N4);
- `DeserializeAndCheckBlockTest` is reported as a diagnostic only, since it also runs checks.

**Recorded with every number:** the Rust hash backend and its features (`bitcoin_hashes` 1.2.0 with runtime dispatch; SH1's once it merges), Core's SHA-256 autodetect string, the pins and rules, the compiler and profile, and whether input preparation and pair packing are timed.

**Where.** Wall-clock comparisons run only in a manager-reserved quiet window (DEC-23(b)). Per PR, PF0 counts instructions on the same rows once it exists; until then CS0 reports numbers and does not gate (there is no previous head to regress against).

**Expectations.** SHA-256 is about 95 % of the instructions of decoding and hashing `block813851` (§6.2 prototype). So these rows are decided by the hash backend (DEC-28, SH1), not by the decode structure, and the comparison saves Core's re-serialization work but none of its cryptography. CS0 makes **no 2× claim**: the rows record parity or a shortfall, and any 2× evaluation belongs to the A1 pipeline as a whole.

---

## 10. PLAN §7.2 deliverables (interfaces only; content in the private note)

| Deliverable | Where | Runs |
|---|---|---|
| The marked module | `dash-consensus/src/primitives/canonical.rs` (tracked) | every build |
| `quirk_01_embargo_1` | `dash-consensus/tests/quirk_01_embargo_1.rs` (tracked, marked), a schema-opaque driver; data in `$DASH_EMBARGO_DIR/quirk1/` | every `cargo test`; skips loudly when the variable is unset; the gate runs it and checks its receipt (§9.4) |
| Quirk 1 vectors | `~/private/embargo/corpus/quirk1/`: `MANIFEST` (`cases <N>`, `min <M>`, one sha256 per case), `PROVENANCE.md`, opaque case files | read by the test |
| Private corpus layout | `~/private/embargo/corpus/` (`= $DASH_EMBARGO_DIR`): `README.md`, `quirk1/`, `fuzz/MANIFEST` (entries `pending`/`required`), `fuzz/<target>/{seeds,corpus,runs}/`, `l3/`, `.receipts/`; private regtest run directories in `~/private/embargo/runs/` | — |
| Scan (g) | tool in `~/private/embargo/tools/`, results in `~/private/embargo/scan-g/` | over the oracles' block files (read-only, idle priority, as BS2's blk mode), partial now and complete when they are synced; STATUS records only "scan (g) recorded" |
| Private L3 operator | definition and dashd baseline verdicts (both pins) in `$DASH_EMBARGO_DIR/l3/` | O10's engine loads private operators from there; rdashd's side runs once the node exists |

- **The tracked driver knows one neutral case shape** (Opus B-1): an input (transaction or block bytes), the pin, and Core's outputs for the fields `diff_tx_decode` and `diff_block_decode` compare (decode verdict, consumed length, serialization, txids, instance hashes, block hash, merkle root, mutation flag). It deserializes cases with `#[serde(deny_unknown_fields)]`, so the private files cannot grow fields that tracked code would have to name, and it reads files only through `MANIFEST`.
- **Later tasks' functions** (replaces r0's D7). Expectations for functions other tasks own are checked through **private seeds of those tasks' own public L1 targets**: the oracle supplies the expectation on both pins, the gate's private-seed replay runs them (§9.4), and **nothing tracked changes when those tasks merge**. A designated reader writes those seeds in the private tree as a `pending` manifest entry once each target exists, before that task merges, and the manager flips the entry to `required` at the merge (DEC-30 N2; §9.4). Which targets have private seeds is recorded only in the private tree.
- **Vector provenance.** Every expected value comes from Dash Core:
  - O1's oracle on both pins;
  - private regtest runs of dashd v24.0.0-rc.3 (`/work/chains/bin/dashcore-24.0.0-rc.3`) and v23.1.8, on their own datadirs and ports, stopped when done. Their datadirs and logs live in the private tree (`~/private/embargo/runs/`, mode 0700), never in `/work/scratch` or `/tmp`.

  The v23.1.8 binary comes from O7's build or the official release tarball (checksum verified). The inputs fed to Core are constructed by a private script, and the script, its run log and the binaries' hashes are part of the provenance.
- **Briefs for later tasks.** The private note lists, for the manager, the neutral instructions that later briefs need under PLAN §7.2.

---

## 11. Dependencies and order of work

| Dependency | Kind | Effect on CS0 |
|---|---|---|
| BS3 (fork wiring) | merge dep | CS0 starts on BS3's tree and adds `dash-primitives`, `dash-params` and `dash-pow` to the base-sdk tables. Implementation waits for BS3's merge. The `no_std` job is gone (36a97b35) |
| BS4a (decode domain) | `i:` dep | CS0 develops against 33fcc59 with the §6.4 allowlist. It merges after BS4a, rebased onto its pin, with no allowlist |
| BS11a-t (BLS types) | soft | `BlsCtx` becomes a re-export (§8.3). If BS11a-t lands after CS0, the switch is a one-line follow-up |
| SH1 (SHA-256 backend) | soft | plugs into `hash.rs` (DEC-28); CS0's bench numbers are recorded with whichever backend is current |
| CS2 (phases A, B) | merge order | CS0 reconciles CS2's provisional `lib.rs` (DEC-29) |
| O1 (`dash-oracle-sys`) | acceptance | needed for §9.1's recorded expectations, §9.3 and the quirk-1 vectors |
| BS2 (`dash-conformance`) | acceptance | §9.2, §9.3's comparisons, sink and twins, and the private-seed replay are changes to that crate after it merges |
| O3 (`node-fuzz`, farm) | acceptance | CS0 creates `node-fuzz` if it is missing (DEC-24 D5); O3 adopts the runner. The budget runs on the farm or on CS0's own cores |
| O4 (`dash-core-vectors`) | merged | CS0 imports `block813851.raw` through O4's sync script |
| O17 (PIN-DELTA) | soft | its table supplies the later `UngatedRules` fields. CS0 needs only rows 4 and 5, which A1 §2.25 already settles |

**Consumers this unblocks, and the lines their briefs need:**
- **EV0:** `Tx`, `Header`, `Block`, `TxRef`, `ConsensusCtx`, `BlsCtx`, the error types, `BlockId`.
- **CS3:** everything above plus `merkle` and the predicates. Brief lines: `check_block` takes the rules and reads `Block::merkle(rules)`; header checks take `&Header`; the CheckBlock/ContextualCheckBlock conversion is `into_block_wrapped` with `tx.txid(rules)`; CS3 adds the cached sigop count slot on `Tx` (DEC-25).
- **Every consensus task** (one general line, Opus M-5): "port every condition verbatim: call `Tx`'s predicates exactly where Core calls the C++ ones and compare raw fields exactly where Core does; compare `tx_type.to_base()` numerically, as Core compares `nType`, never `TxType` values".
- **CS1:** the `canonical` field writers including `write_prevout` and `write_sequence`, `HashWriter`, and `decode_tx` in place of r0's `TxDecoder`.
- **CS4:** `Deployment`, `DeploymentView`; `dip0003_enforced(height, params)` as its own free function.
- **CS5a:** `CoreWrite`, compact size.
- **CH1:** `decode_header`, `Header`, `HeaderCtx`, `ChainAncestry`; `ProcessHeaders` carries `Vec<Header>`.
- **ST2, CH*, P*, RP:** "stored, relayed and RPC-hex transaction, header and block bytes are `CoreBytes`; stored blocks are read back with `decode_block(..)?.core_bytes()`; there is no other way to make `CoreBytes`" (§4.5).
- **Every task that adds an L1 target:** "register its comparison function in `dash_conformance::l1::TARGETS`; run it only through `node-fuzz/run.sh`" (§9.3).
- **Mempool, coins view, indexes:** "anything keyed by txid takes the network's `UngatedRules` when it is built, and gets txids from `Tx::txid(rules)`".

---

## 12. Deviations from A1

| # | A1 | CS0 | Why |
|---|---|---|---|
| 1 | `ValidationError { class, reason: &'static str, debug: Option<String> }` (§1.3, App. A) | `ValidationError<C>` with `TxValidationError`/`BlockValidationError`, `reason: Cow<'static, str>`, `debug: String`, two tx-to-block conversions | Core formats some reasons and has two conversions (§8.1). Two types mirror Core's two state classes. An empty debug string means none, as in Core's `ToString()` |
| 2 | `BlsCtx` defined in `dash-consensus` (App. A) | re-exported from `dash-pkc`, provisional local copy until then | BS4a's `ProtoCtx` in base-sdk must name it (DEC-24 D2) |
| 3 | `ConsensusCtx.release: &ReleaseProfile` | `ConsensusCtx.rules: &UngatedRules` | the pure crate cannot hold P2P, spork, DKG, policy or RPC data (DEC-24 D3) |
| 4 | `DeploymentView::active_after(prev: BlockId, …)` | `prev: Option<BlockId>` | Core's `DeploymentActiveAfter` accepts `nullptr` (genesis) |
| 5 | `Tx { …, payload: OnceLock<…>, legacy_sigops: OnceLock<u32> }` | one payload slot per BLS scheme; `legacy_sigops` added by CS3; no cached txid | a payload's decoded value depends on the global scheme for CbTx and asset unlocks (A1 §1.4.1); DEC-25; §5.1 |
| 6 | merkle under `block::merkle` (App. B) | `dash_consensus::merkle` | used at `Block` construction and by `dash-evo`'s roots |
| 7 | CH1 `ProcessHeaders { headers: Vec<BlockHeader> }`; `check_block_header(h: &BlockHeader, …)`, `contextual_check_block_header(h: &BlockHeader, …)` | `Vec<Header>`; `&Header` | one X11 per header, and no header paired with a foreign hash (§6.3) |
| 8 | `check_block(b, params, check_pow, check_merkle)` | adds `rules: &UngatedRules` | the merkle root depends on the release (§5.1) |
| 9 | `DeploymentView::dip0003_enforced(prev)` | CS4's free function `dip0003_enforced(height, params)` | Core's is a height comparison and each call site picks its height |
| 10 | `ChainAncestry::ancestor(..) -> BlockId`, `header(..) -> &HeaderCtx` | `-> Option<BlockId>`, `-> HeaderCtx` | Core's `GetAncestor` returns null; by-value access works over any arena |
| 11 | A1 §1.3: a "fast-path predicate" decides when wire bytes are hashed | an exact comparison while serializing (§6.2) | exact by construction; costs 1.8 % (measured) |

---

## 13. Decisions

**Resolved by DEC-24:** D1 (BS3 dropped the job; verified), D2 (§8.3), D3 (§8.4), D4 (the shape is final after this review; r1 adds `into_block_wrapped`), D5 (§9.3), D6 (implemented in §9.4). **D7 is replaced** by the mechanism in §10, as the Opus review requires (N2).

**Approved by DEC-30** (GPT R2-m2; these were r1's requests N1–N6):
- **N1.** Release-dependent hashes are selected by accessors that take the rules (§5.1); CS3's `check_block` takes the rules (deviation 8); txid-keyed runtime caches take the rules when built (§11).
- **N2.** D7 is replaced by private seeds of later tasks' own public L1 targets (§10). The CS0 implementer writes them as `pending` entries before CS1, CS3 and CS5b merge. **Merge-checklist step** (for each of those tasks' DEC-30 checklist items): the manager confirms the task's entries exist and flips them from `pending` to `required` in `fuzz/MANIFEST` (a private-tree step, §9.4). CS1's design still describes r0's D7 and drops that text in its next revision.
- **N3.** `CoreBytes` is the only type for stored, relayed and RPC-hex consensus bytes (§4.5); ST2 adopts it; P2a's outbound `tx`/`block`/`headers`/`cmpctblock`/`blocktxn` variants carry it.
- **N4.** CS0 writes the matching Core bench `DeserializeBlockHashMerkle` (§9.6) as a DEC-16 test-only patch on a local Core branch, built with B1's script.
- **N5.** Every `node-fuzz` L1 target runs only through the runner and the private sink (§9.3); O3 adopts the runner. Implementers without access fuzz locally through the proptest twins, and the manager releases non-embargo reproducers (PLAN §7.5).
- **N6.** The determinism bans (`HashMap`, `HashSet`) go into BS3's shared `pure-clippy.toml` (§2).

**Still for the manager:** the root `clippy.toml` entry for base-sdk's `Block::merkle` (§2, Opus m2-2) touches a workspace-wide file; it is part of CS0's commit 1 unless the manager prefers BS3 to carry it.
- **For O17** (Opus m-9, not a CS0 change): `ContextualCheckTransaction` accepts special types 0–9 in v23.1.8 and 0–12 in v24.0.0-rc.3. On mainnet a type-10 transaction is rejected under both, with different reasons and classes (`bad-txns-type` against `bad-prodis-too-early` after payload checks). It is not in A1 §2.25's table.
- **Design review routing.**
  - This document has no embargoed content, so any reviewer may read it.
  - The private note is for the manager and the two Claude reviewers of the marked module (PLAN §7.2, items 1 and 3). It also holds recommendations for the manager that only readers may see.

---

## 14. Implementation plan

Commits, in order. Each is one logical change, with a conventional message that says nothing beyond the codename.

1. `feat(consensus): add the dash-consensus crate` (manifest, MIT licence, `clippy.toml`, `node-pure`, base-sdk tables, the determinism bans in `pure-clippy.toml`, the root `clippy.toml` entry for base-sdk's `Block::merkle`, `ARCHITECTURE.md` with the sole-encoder and `CoreBytes` rule; reconcile CS2's provisional `lib.rs`)
2. `feat(consensus): add ValidationError and Core's result classes`
3. `feat(consensus): add release rules, BlsCtx and ConsensusCtx interfaces`
4. `feat(consensus): add core_serialize, CoreBytes and the canonical transaction encoding`
5. `feat(consensus): add merkle root computation`
6. `feat(consensus): add Tx, Header and Block with range decoders`
7. `test(node-sink): add the private sink, the fuzz init-hook check and their self-tests`
8. `test(consensus): decode Core's vectors and block813851; add the recorded rows, the properties and decode benches`
9. `test(consensus): add quirk_01_embargo_1 over the private corpus`
10. `test(node-fuzz): add the runner, its self-test and the three decode targets`
11. `test(conformance): add the L1 comparisons, the boundary table, the proptest twins and the private-seed replay`
12. `test(conformance): run the dash-consensus wrappers over the BS2 corpus`
13. `docs(consensus): document the crate's Core mapping and deviations`

**Size:** M (PLAN), about 2 agent-weeks plus the fuzz budget (2 × 24 CPU-h for each of two targets, 2 × 1 CPU-h for the header target). The private vectors, scan (g), the L3 definition and the private seeds run in parallel with commits 2–8.

**Definition of done:**
- PLAN row CS0's acceptance:
  - L1 `diff_tx_decode`/`diff_block_decode` clean for ≥ 24 CPU-h **per pin**, each leg's runner summary `pass`;
  - BS2 history through the wrappers over the synced range, reported as partial history (§9.2);
  - ranges from the decode cursor;
  - quirk 1 passing against the private corpus, with the gate's receipt;
  - the bench numbers recorded with their configuration (§9.6);
- the runner self-test, the synthetic-corpus self-tests (§9.4) and the `compile_fail` doctests (§4.5) passing;
- G0 (including the dash-spv dashd suites, since CS0 touches the root manifest and `Cargo.lock`);
- a self-review of the full diff;
- the code-simplifier and code-review-validator passes;
- the two Claude reviews of the marked module and the cross-provider review of the rest (PLAN §7.4).

---

## 15. Risks

| Risk | Mitigation |
|---|---|
| The SHA-256 backend is slower than Core's on agentbox, which has no SHA-NI, so `block_decode_hash` trails Core | SH1 (DEC-28) through the `hash.rs` seam; the bench records the backend; no 2× claim (§9.6) |
| A runtime cache mixes txids of two releases | CS0's objects hold no release-dependent value; the brief line for txid-keyed caches (§11) |
| BS4a changes `Transaction` (i64 values, payload cap) under CS0 | CS0 touches base-sdk fields only in `canonical` and the accessors; `write_output` already takes `i64` |
| BS6a re-types payloads and changes `SpecialPayload::decode` | `Tx::payload(bls)` already has the final shape; only its body changes |
| The oracles are not synced at merge time | merge on the synced range, labelled partial history; the M1 exit (not CS0's merge) needs full history |
| A private input leaks through test, fuzz or log output | §9.3 runner and sink, §9.5 output rules and lints, opaque names, and review of exactly those code paths by the Claude reviewers |
| A later task merges before its private seeds exist, or a seed written early breaks other gates | `pending`/`required` manifest states; the DEC-30 merge-checklist step flips the task's entries to `required` and confirms they exist (§9.4, §13 N2) |
| A downstream crate forges `CoreBytes` | sealed `CoreSerialize`, private `Decoded` fields, two named constructors, `compile_fail` doctests (§4.5) |

---

## 16. Review dispositions

### Round 1

Reviews: `reviews/CS0-design-r1-gpt.md` (GPT-6.1-Sol, REWORK) and `reviews/CS0-design-r1-opus.md` (Opus, APPROVE-WITH-CHANGES). The Opus review's private findings (P-B1, P-M1 to P-M6, P-m1 to P-m8) are disposed of in the private note.

**Counts (public findings):** 30 findings; 29 accepted (one of them fixed differently from the suggestion, GPT B1), 1 accepted in part (Opus M-6), 0 rejected. Including the 15 private findings, all accepted: 45 findings, 44 accepted, 1 in part, 0 rejected.

#### GPT-6.1-Sol

| ID | Severity | Disposition | Where |
|---|---|---|---|
| B1 | Blocker | **Accepted, fixed differently.** The review asked for a release tag checked at boundaries. r1 removes release-dependent state instead: caches are release-independent and the txid and merkle root are selected by accessors that take the rules, so there is nothing to mismatch and no check to forget. Tests mix objects across both rule sets against O1 | §5, §5.1, §6.3, §9.1, §9.2, deviation 8 |
| M1 | Major | Accepted: `Header` binds the header to its X11 hash, has no hash-taking constructor, and flows through CH1 and CS3; a test counts one X11 per header | §6.3, deviation 7 |
| M2 | Major | Accepted: `into_block` (ConnectBlock, pass-through) and `into_block_wrapped` (CheckBlock and ContextualCheckBlock, Core's prefix with the txid, empty-debug case) | §8.1 |
| M3 | Major | Accepted: runner with a run-scoped verdict, fail-closed exit, fresh budget after a fix, self-test of the failure and sink-failure paths, private capture of all libFuzzer output from launch, sanitized twins, one aggregate private-replay line, budget per target per pin | §9.3, §9.4 |
| M4 | Major | Accepted: the boundary table (CompactSize, integer fields, shapes, truncation, suffixes and cursors, stable txid, merkle with Core's exact mutation definition); evidence labelled partial history | §7, §9.1, §9.2 |
| M5 | Major | Accepted: DEC-24 D6 implemented (loud skip, fail on a missing or partial corpus, receipts the gate checks) | §9.4, §13 |
| M6 | Major | Accepted: matching Core workload `DeserializeBlockHashMerkle` plus a Rust row matching `DeserializeBlockTest`; the SHA assessment corrected (multi-way merkle already dispatched; the single stream is the gap, DEC-28/SH1); configuration recorded; quiet windows; merkle cost corrected; buffers reused; no 2× claim | §7, §9.6, §13 N4 |
| minor: ancestry | Minor | Accepted: `ancestor -> Option<BlockId>`; valid-id and lifetime contract | §8.4 |
| minor: DEC-25 writers | Minor | Accepted: `write_prevout`, `write_sequence`; `write_input` is their composition | §4.3 |
| minor: failure cursors | Minor | Accepted: the cursor does not move on failure; `offset` defined | §6.1 |
| minor: contexts | Minor | Accepted: disabled checkpoints are `None`; one chain and profile per context; `header_check_time` | §8.4 |

#### Opus (public part)

| ID | Severity | Disposition | Where |
|---|---|---|---|
| B-1 | Blocker | Accepted: the tracked quirk driver knows only the neutral case shape the public targets compare, with `deny_unknown_fields` and a manifest; D7 is replaced by private seeds of later tasks' public targets, so nothing tracked changes at their merges | §9.4, §10, §13 N2 |
| M-1 | Major | Accepted: raw-stderr skip line, failure on a missing or partial corpus, count receipts, no tracked file pointing at the private tree, targets refuse to run without the variable | §9.3, §9.4 |
| M-2 | Major | Accepted, applied to every `node-fuzz` target: corpus, crashes and logs in the private sink; the runner as the only entry; an init-time refusal that cannot dump an input; `.gitignore`; opaque names; explicit modes; one shared sink; output lints and macro rules | §3, §9.3, §9.5, §13 N5 |
| M-3 | Major | Accepted, the preferred form: an allocation-free `CompareWriter` decides the fast path exactly. Measured at +1.8 % instructions on `block813851`. §9.2 now asserts that every stored transaction and block is canonical | §4.2, §6.2, §9.2 |
| M-4 | Major | Accepted: (a) idempotence for all values and identity only where canonical, `TxType` through `from_base`; (b) stable-txid cases generated with O1 on both pins and Core's `evo_assetlocks_tests` cases, all seeding `diff_tx_decode` | §9.1 |
| M-5 | Major | Accepted: numeric type comparisons in `canonical`, `Tx::new` canonicalises, a general brief line for every consensus task | §4.3, §5, §11 |
| M-6 | Major | **Accepted in part.** Accepted: the interface rule in `ARCHITECTURE.md` and the ST2/CH/P*/RP briefs, made a type (`CoreBytes`), with P2a's outbound variants carrying it. Rejected: a node-wide `Hashable::hash` lint, because base-sdk implements `Hashable` for about twenty non-consensus types (MN-list entries, payloads, keys, P2P fields) and clippy cannot ban a trait method for some implementations only; the ban stays in `dash-consensus`, and `CoreBytes` carries the rule across crates | §2, §4.5, §11, §13 N3 |
| m-1 | Minor | Accepted (with GPT M2): `into_block_wrapped`; `with_debug` replaces | §8.1 |
| m-2 | Minor | Accepted: exact mutation definition and the listed cases | §7, §9.1 |
| m-3 | Minor | Accepted: `dip0003_enforced` moves to CS4 as a free function | §8.4, deviation 9 |
| m-4 | Minor | Accepted: `Option<BlockId>`, `HeaderCtx: Copy` returned by value; for the EV0/CH1 owners' API review | §8.4, deviation 10 |
| m-5 | Minor | Accepted: DEC-25's writers; every Core predicate on `CTransaction` in `canonical`, including `is_null`, `is_coinbase` and the asset-unlock predicates | §4.3 |
| m-6 | Minor | Accepted: the two passages moved to the private note | §6.2, §10 |
| m-7 | Minor | Accepted: ≥ 24 CPU-h per target per pin | §9.3 |
| m-8 | Minor | Accepted: both rule sets over mainnet history, txids and roots equal | §9.2 |
| m-9 | Minor | Accepted and forwarded to O17 (not a CS0 change) | §13 |
| m-10 | Minor | Accepted: `Tx::to_mutable(&self)` | §5 |
| m-11 | Minor | Accepted: the scratch copy takes the bytes being hashed | §6.2 |
| m-12 | Minor | Accepted: one comparison function per target, shared by the libFuzzer target and its twin | §9.3 |

### Round 2

Reviews: `reviews/CS0-design-r2-gpt.md` (GPT-6.1-Sol, REWORK with no blocker and two majors) and `reviews/CS0-design-r2-opus.md` (Opus, APPROVE-WITH-CHANGES). The Opus review's private findings (P2-M1, P2-M2, P2-m1 to P2-m3) are disposed of in the private note; P2-B1 is listed here with its public requirement.

**Counts (public findings):** 13 findings, all accepted, 0 rejected.

| ID | Severity | Disposition | Where |
|---|---|---|---|
| GPT R2-M1 | Major | Accepted: `Decoded`'s fields are private, with read-only accessors and a consuming `into_value`, and no constructor or setter; `CoreSerialize` is sealed to the five consensus types; `CoreBytes` has exactly two named constructors and no conversion, `Default` or `Deserialize`; `compile_fail` doctests show a downstream crate cannot implement the trait, build or convert into `CoreBytes`, or assign `Decoded`'s fields | §4.2, §4.5, §6.1, §9.1 |
| GPT R2-M2 | Major | Accepted: the quirk manifest declares `cases <N>` and `min <M ≥ 1>` and an empty corpus fails; the seed manifest has its own floor and a replay that executes nothing fails; the gate asserts a positive count; synthetic-corpus self-tests show sanitized failures and no receipt | §9.4, §10 |
| GPT R2-m1 | Minor | Accepted, by making the row match rather than relabelling it: `block_txs_decode` now decodes the header and count, collects `Vec<TxRef>` and drops it per iteration, as `DeserializeBlockTest` does; the remaining differences (stream rewind, hashing strategy) are recorded with the numbers | §9.6 |
| GPT R2-m2 | Minor | Accepted: §13 records N1–N6 as approved by DEC-30, including CS0's ownership of the Core bench patch | §13 |
| Opus P2-B1 | Blocker | Accepted: every codec row of the boundary table takes its expectations live from O1 at test time (`dash-conformance/tests/l1_boundaries.rs`, feature `oracle`, every PR, through the shared comparison functions) and never commits them; only the merkle, error-string and txid-display rows keep recorded values; no row is removed | §3, §9.1 |
| Opus M2-1 | Major | Accepted: the properties follow the twins' output rules (placeholder seed `Debug`, `failure_persistence: None`, no shrinking, `prop_assert!` with a static field name instead of the `*_eq!` macros, the lint set and reviewer grep), and write their seed to the sink when the variable is set; the sink is a small `node-sink` crate so `dash-consensus` can dev-depend on it | §3, §9.1, §9.5 |
| Opus M2-2 | Major | Accepted: seed-manifest entries are `pending` or `required`; `pending` entries replay only when their target exists in the tree under test; the manager flips them at the task's merge as a DEC-30 merge-checklist step | §9.4, §10, §13 N2, §15 |
| Opus m2-1 | Minor | Accepted: `CompareWriter` stops comparing and keeps counting past the end of the wire range, never indexes past it, and `matches()` requires equal lengths; unit-tested both ways | §4.2, §9.1 |
| Opus m2-2 | Minor | Accepted: base-sdk's inherent `Block::merkle` is banned node-wide in the root `clippy.toml` (clippy ignores the path in crates without base-sdk; checked). It is the only inherent consensus-hash helper on `Transaction`, `BlockHeader` and `Block`; their hashes are `Hashable` implementations and stay under the crate-local trait ban | §2, §13 |
| Opus m2-3 | Minor | Accepted: the constructors are named exhaustively and ST2 reads blocks back through `decode_block(..)?.core_bytes()`, which is Core v24's own path (`ReadBlockFromDisk` decodes; there is no raw-read path in `net_processing.cpp`) | §4.5, §11 |
| Opus m2-4 | Minor | Accepted: receipts carry the gate's `DASH_EMBARGO_RECEIPT_ID`, which the gate checks | §9.4 |
| Opus m2-5 | Minor | Accepted: the runner invokes the built binary with a fixed argument list and `TMPDIR` inside the run directory; the init hook accepts only that shape (flag allowlist, each flag once, every path-valued flag and positional directory inside the private tree, the first positional being the working corpus); merging and minimisation are runner modes | §9.3 |
| Opus m2-6 | Minor | Accepted: the PLAN items the private review lists were rewritten in general terms on the manager's instruction; CS1's design text is covered by DEC-30 N2 | PLAN §7.2, PLAN §4.2 |

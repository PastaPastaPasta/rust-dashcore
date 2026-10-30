# dash-consensus

Dash Core's consensus layer for the Rust Dash node (node program A1 §2.4). The design is
`docs/node-program/designs/CS0-node-primitives.md`; this file lists what other crates must know
to use it correctly.

## Purity

An ordinary `std` crate with no I/O, clocks, threads or globals (A1 §1.5, §2.1). It is in the
`node-pure` group, so `contrib/node/check-pure.sh` enforces the bans. Consensus code uses
`BTreeMap`/`BTreeSet` or a sorted `Vec`, never `HashMap`/`HashSet`. Every ported function
carries a `// Core: <file>:<line>` anchor to v24.0.0-rc.3, (many anchors also carry the v23.1.8 line they had
before DEC-82; those are history).

## Canonical bytes: the sole encoder (A1 §1.3)

Core hashes its own re-serialization of a decoded object, stores that serialization and relays
it. So does the node:

- **Bytes.** Transaction, header and block bytes come from `core_serialize`, never from
  base-sdk's encoders (`BaseCodec::encode`, `Hashable::hash`, `Block::merkle`).
- **`CoreBytes`.** Bytes that are stored (block files), relayed (`tx`, `block`, `headers`,
  `cmpctblock`, `blocktxn`) or printed (RPC hex) have the type `CoreBytes`. There are exactly two
  ways to make one: `core_bytes(&value)` and `Decoded::core_bytes()`. Storage, relay and RPC APIs
  take `CoreBytes`, so the rule is checked by the compiler across crates.
- **Reading stored blocks back** (for relay and `getblock`): `decode_block(stored)?.core_bytes()`,
  which is Core's own path (`ReadBlockFromDisk` decodes the block). Stored bytes are Core's
  serialization, so the received range is reused without re-encoding. `dash-conformance`'s
  `dash_consensus` leg checks this, with the hashes and merkle roots, over chain history
  (CS0 §9.2).
- **Transaction offsets inside stored bytes** are `80 + CompactSize(n)` plus the prefix sums of
  `Tx::size()`.
- **Field writers.** Code that serializes transaction fields itself (CS1's sighash) uses
  `primitives::canonical`'s writers, never its own encoding.

## Hashes

- `Tx`, `Header` and `Block` compute their hashes once. `Header::new` is the only way to pair a
  header with its X11 hash; pass `&Header`/`Vec<Header>` along rather than the base-sdk header.
- **The txid is v24's stable asset-unlock txid** (DEC-82: v24 only): `Tx::txid()` is the stable
  txid for a version 2+ asset unlock and the instance hash otherwise, and `Block::merkle()` is
  computed over those txids. Anything keyed by txid (mempool, coins view, indexes) uses `Tx::txid()`.
- `Tx::instance_hash()` is Core's `GetInstanceHash()`: SHA256d of the serialization.
- All hashing goes through `dash_consensus::hash`, where faster backends plug in.

## Porting rules for consensus code

- Port every condition verbatim: call `Tx`'s predicates (`is_null`, `is_coinbase`,
  `is_special_tx_version`, `has_extra_payload_field`, `is_platform_transfer`, the asset-unlock
  predicates) exactly where Core calls the C++ ones, and compare raw fields exactly where Core
  does.
- Compare `tx_type.to_base()` numerically, as Core compares `nType`; never compare `TxType`
  values (base-sdk's `Unknown(n)` catch-all can shadow a named variant).
- Port integer expressions verbatim, including signedness and casts.

## Validation results

`TxValidationError` and `BlockValidationError` carry Core's result class (Core's integers), the
reject reason verbatim and the debug message. Core converts a transaction rejection to a block
rejection in two ways, and so do we:
- `into_block`: `ConnectBlock`, reason and debug message unchanged;
- `into_block_wrapped(result, &tx.txid())`: `CheckBlock` and `ContextualCheckBlock`, whose
  debug message is `Transaction check failed (tx hash <txid>) <debug>`.

## Decoders

`decode_tx`, `decode_block` and `decode_header` read one object and leave the rest of the input;
`decode_tx_exact` is Core's `DecodeTx` and rejects trailing bytes. The caller applies its own
trailing-byte policy. A failed decode leaves the cursor where it was. The decode domain is
base-sdk's (Rule DDE, A1 §1.4).

## Coins and undo records (`coins`)

Core's `Coin` (database value), `TxInUndoFormatter`, `CTxUndo` and `CBlockUndo` serializations,
identical at v24.

- **Bytes come from Core's in-memory coin.** Core reads a coin or undo entry into a `Coin` and
  writes that `Coin` again; an amount outside the money range, an off-curve key of
  `ScriptCompression` codes 4 and 5 (read as an empty script) and a raw script over
  `MAX_SCRIPT_SIZE` (read as `OP_RETURN`) do not survive. So never move a coin between the
  database and an undo record by copying or splicing bytes: decode a `CoinRef` and use
  `CoinRef::write` / `CoinRef::write_undo`.
- **New coins**: `coin_bytes(&TxOut, height, coinbase, w)` writes the database value;
  `CoinRef::from_output(&TxOut, height, coinbase)` is the same coin borrowed (the mempool view's
  `Coin(tx.vout[n], MEMPOOL_HEIGHT, false)`). Outputs with `is_unspendable` scripts never become
  coins. `ArrayWriter<N>` encodes into a fixed inline buffer and reports overflow.
- **Reading**: `CoinRef::decode` borrows the bytes and does not allocate; the script stays
  compressed until `ScriptRef::expand`.
- **`CoinsView` is fallible** (DEC-77): reads return `Result<_, V::Error>`, where the error is a
  storage read failure, never a missing coin. Consensus code propagates it and never turns it
  into a verdict (Core aborts the process on one). Views without storage use `Infallible`. Core's
  database read treats a stored value that fails to decode as no coin (`CDBWrapper::Read`), and
  `CCoinsViewDB::HaveCoin` is a key lookup; a view that reports an undecodable value as an
  error declares that deviation.
- **Undo records**: `BlockUndoRef::decode` reads a record as `CoinRef`s without copying, and a
  `BlockUndoRef` built from the coins the cache holds writes `CoreUndoBytes` without decoding
  them; `BlockUndo::decode` is the owned read; `decode_block_undo` binds the bytes to the record
  (its `CoreUndoBytes`). Rev files store `CoreUndoBytes`, from
  `BlockUndo::core_bytes`. `CoinRef::decode_undo` reads one input's entry; a zero height is an
  entry without its metadata, which the decoder reports and the caller handles (Core's
  `ApplyTxInUndo` uses `access_by_txid`; CH2's recovery treats it as corruption).
- **Writes are blind**: `CoinWrite::{Put, Erase}` overwrite or erase without reading, as Core's
  `CCoinsViewDB::BatchWrite`; startup replay and subscriber rewind depend on that.

## Input checks, sequence locks, sigops and UTXO hashes (CS5b)

The rules that read the coins a transaction spends, v24's (DEC-82), for `ConnectBlock` (CH3a) and
the mempool.

- **Order is the caller's.** Core's `ConnectBlock` runs the special-transaction checks
  (`ProcessSpecialTxsInBlock`) before its per-transaction loop; they reject an asset unlock with
  any input (`bad-assetunlocktx-have-input`). `check_tx_inputs` accepts an asset unlock on its
  payload's fee without reading its inputs, as Core does, so a block's caller runs those checks
  first. The mempool's `PreChecks` runs its special-transaction checks last, after rejecting any
  transaction with an input the view lacks (`bad-txns-inputs-missingorspent`).
  Its other preconditions are `CheckTransaction`'s (outputs and their sum in the money range) and
  not a coinbase; it panics where Core asserts or throws.
- **`check_tx_inputs(tx, view, spend_height)`** returns the fee; the block's accumulated-fee
  check stays the caller's. It reads every input's coin before judging any (`HaveInputs`), then
  checks maturity, the money range of each value and of the running sum, and value in against
  value out, with Core's reasons, classes and debug messages.
- **Failures are `ViewOr`.** `ViewOr::View` is the view's read error: fatal, propagated, never a
  rejection (Core aborts on one). `ViewOr::Reject` is the verdict. The sigop counts
  (`p2sh_sigop_count`, `transaction_sigop_count`) return `ViewOr::Reject(MissingCoin)` for a coin
  the view lacks, where Core asserts; with the order above it never happens, and it never panics.
- **Sequence locks.** `calculate_sequence_locks(tx, flags, prev_heights, ctx)` takes each input's
  coin height and, as Core, sets the heights of inputs with a disabled lock to 0 when BIP68 is
  enforced; the mempool keeps the mutated heights for its lock points. `ctx` is the block being
  connected (the mempool's: the tip's child): a time lock runs from the median time past of the
  block before the coin's, read through the complete ancestry (`ChainAncestry`, never a windowed
  context), and `evaluate_sequence_locks` compares against the parent's median time past.
  `block_lock_time_flags(csv_active)` gives `ConnectBlock`'s flags.
- **UTXO hashes.** `MuHash3072` is the coinstats index's accumulator (`insert_coin`,
  `remove_coin`, `finalize`, which divides and leaves the state at the quotient as Core's does,
  and `to_bytes`/`from_bytes`); compare finalized hashes, never states. `HashSerialized2` takes
  coins in the coins database's order (ascending txid bytes, a transaction's outputs together)
  and panics on a txid below the previous one; quirk 33 writes `1` as the header of every
  transaction a UTXO set can hold (`0` only for a non-coinbase coin at height 0).
- **Coverage.** L1 `diff_check_tx_inputs` (with the coin sigops) and `diff_sequence_locks`
  against the oracle; Core-recorded BIP68/112/113 verdicts, UTXO-set hashes on a regtest chain,
  and MuHash boundary arithmetic (`tests/data/PROVENANCE.md`).

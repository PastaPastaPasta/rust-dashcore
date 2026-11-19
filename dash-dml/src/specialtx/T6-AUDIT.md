# T6 operator-key audit (OK-1, F5 with §11.3 and §11.5)

Taken at this slice's head on `node/integration` 6f44f250 plus OK-1. "Non-test" excludes
`#[cfg(test)]` modules, `tests/`, `benches/`, `examples/`, `fuzz/` and `contrib/`. The legacy
`dashcore` crates (`dash/`, `key-wallet*`, `rpc-*`, `dash-filters/`) have their own provider
payload types of the same names and are out of scope. In OK-1 `CorePayload` delegates to the
fork payload's fields, so every site below reads the same object as before the slice.

## 1. `==`, `!=`, `Hash` and collection keys over the payload types

Types: `ProRegTx`, `ProUpRegTx`, `ProUpServTx`, `ProUpRevTx`, `ProUpSharedRegTx`,
`SpecialPayload`, `PayloadDecode`, and the new `CorePayload`, which derives no `PartialEq`. No
production caller compares a `PayloadDecode` or a `tx.payload()` result (grep `payload() ==` and
`== …payload()`: no hit).

| Site | What | Handling |
|---|---|---|
| `node-l1/src/specialtx.rs:611` | `#[derive(PartialEq)]` on the generator's `Payload` enum over the five types; used at `:946` (`x.payload == y.payload`) | Tooling (T layer), not consensus. Unchanged in OK-1 (fork derive). OK-2 moves it to a side-car comparison. |

No `Hash` derive, `HashMap`, `HashSet`, `BTreeMap` or `BTreeSet` key over these types was found.
Hits checked and found unrelated: `dash-dml/src/trace.rs:17` and `dash-evo/src/effect.rs:34`
(the types only appear in doc comments; the fields are hashes), `dash-coinjoin/src/queue.rs:201`
(a `TxHash` key), `conformance/src/verbose.rs:1295, :1326` (a local `PayloadDecode` enum of its
own), `conformance/src/report.rs:44` (a comment), and `node-l1/src/bsdk_decode.rs:56` (a local
`CorePayload` struct of node-l1's own, consumed flag and bytes). DML collection keys are
`UniqueKey` values (`dash-dml/src/unique.rs:263`), built from the lazy key object.

Every other production `==`/`!=` near a payload compares a field: version, mode, type, voting
or owner key id, collateral index (`provider.rs:260, :390, :442`, `trivial.rs:397, :414, :478`,
`build/mod.rs:777`, `build/state.rs:119`, `covenant.rs:75`,
`dash-consensus/src/primitives/protx.rs:64, :111`,
`dashd-mempool/src/policy/standard.rs:73`).

## 2. Sites that resolve a cached operator key (`get()` and the like)

Consensus (dash-dml):
- `specialtx/trivial.rs:421, :481, :549`: `operator_key.get().is_valid()`, the trivial checks, run
  from `validated_payload` (`specialtx/payload.rs:156-176`) on the side-car's key, before any other
  read in the check.
- `specialtx/provider.rs:251, :335`, `specialtx/shared.rs:242`, `build/mod.rs:652`
  (`operator_key_held`): `clone().get()`, the `HasOperatorKeyUnderAnyScheme` probe, on a clone.
- `specialtx/provider.rs:332`, `build/mod.rs:851, :976`: `CBLSLazyWrapper::operator==`/`!=`
  against the state key (may decode both).
- `specialtx/sig.rs:90` (`operator_key_bytes`): the sign-hash preimages and the shared consent
  hash, `clone().get()` on a clone (the fork's `lazy_bytes`); the consent hash takes the
  side-car's key (`provider.rs:282`). The `SignedPayload` impls for ProRegTx and
  ProUpSharedRegTx (`sig.rs:103, :134`) pass the payload's own key, the same object in OK-1;
  OK-3 rewrites these two lines.
- `build/state.rs:74`: `set_state_version` re-sets a state key (the `MnCold` copy, not the payload).

The checks read the side-car through `Validated::operator_key`/`operator_sig`
(`specialtx/payload.rs:28-38`) and the build through the key matched with the payload
(`build/mod.rs:602`); a provider payload without its key or signature, which the decoder never
produces, is `bad-protx-payload` there, never a panic.

Non-consensus:
- `dashd-rpc/src/core_write/evo.rs:423` (`lazy_key`, from `:458, :503, :551`): `get()` on the
  cached key of `tx.payload()` (`dashd-rpc/src/core_write.rs:430`), in place.
- `conformance/src/verbose.rs:868` (from `:897, :928, :1030`), `node-l1/src/bls.rs:200`: `get()`
  in place, on payloads those tools decode themselves.
- `node-l1/src/bsdk_decode.rs:333`, `node-l1/src/payload.rs:46`, `node-l1/src/protx_mirror.rs:564`:
  `clone().get()`, on clones.
- `dash-governance/src/object.rs:185, :193`: a masternode-list state key, not a payload key.

## 3. Why the consensus H consumers only see keys the trivial check has resolved

H consumers: `UniqueKey::operator_key` (`provider.rs:237, :385`, `build/mod.rs:715`), `operator==`
(`provider.rs:332`, `build/mod.rs:851, :976`), the scheme (`build/mod.rs:866, :978`), and the
clones into `MnCold` (`build/mod.rs:725` → `build/state.rs:27`, `:868, :980`).

- Every check path reaches its key through `validated_payload`, which runs the trivial check on
  the same side-car object first (`payload.rs:156-176`). A key that fails `get()` is refused there
  with `bad-protx-key-null`, so no later consumer of that call sees a failed key. The D3 test
  (`specialtx/tests/t6.rs`) pins this order for `check_pro_reg_tx`, `check_pro_up_reg_tx` and
  `check_pro_up_shared_reg_tx`.
- Block connect checks every transaction (`dashd-chainstate/src/evo/node.rs:1124`, step 4, through
  `ev1.rs:284` over the block's `BlockPayloads`) before the list build (`node.rs:1207`, step 7),
  over the same cached `Tx` objects. A failing check rejects the block before the build. The
  mining build (`dashd-mining/src/evo.rs:313`) takes mempool transactions, which passed the same
  check on the same `Arc<Tx>`.
- `get()` is idempotent once it succeeds. The only earlier non-consensus resolver of a cached key,
  RPC's `lazy_key`, therefore leaves exactly the state the trivial check's own `get()` leaves.
  For a key that does not resolve, the trivial check refuses the transaction whatever ran first.

## 4. Operator-key change, held and uniqueness decisions (§11.5)

Each decision uses the lazy key object and dash-dml's shared helpers, never `OperatorKey` byte
equality:
- change: `BlsLazyPk` `operator==`/`!=` (Core's `CBLSLazyWrapper`), `provider.rs:332`,
  `build/mod.rs:851, :976`;
- held: `has_operator_key_under_any_scheme` (`list.rs:433`, `build/mod.rs:340`) of the key's
  `get()` value, `provider.rs:251, :335`, `shared.rs:242`, `build/mod.rs:652` (from `:861, :990`);
- uniqueness: `UniqueKey::operator_key(&BlsLazyPk)` (`unique.rs:92`), `provider.rs:237, :385`,
  `build/mod.rs:715`;
- the diff record's state equality: `same_operator_key` (`record.rs:66`), on `MnCold` keys.

## 5. Payload re-encoding and signature verification (§3.5, §11.4)

No non-test `.encode(` of the five payloads in dash-consensus and dash-dml. The sign-hash
preimages the node builds (ProRegTx, ProUpSharedRegTx, the shared consent stream) are
byte writers in `dash-consensus/src/primitives/protx.rs` (outside `CoreSerialize`'s sealed
set, so `core_bytes` cannot take them), each tested equal to the
fork helper it replaces over the corpus (`sig/tests.rs` `t6_builders_equal_the_fork_helpers`);
the other sign hashes are base-sdk's `Signable::payload`. `node-l1/src/payload.rs:72-82`
(`core_bytes`, tooling) re-encodes them and is OK-2's. Transaction ids and relay use
`tx.raw()`.

Verify functions are unchanged per payload type and version:
- ProRegTx, external collateral: `verify_hash`. It was `check_string_sig` → `verify_message` →
  `verify_hash(message_hash(s))`; it is now `check_hash_sig_ecdsa` → `verify_hash` of
  `pro_reg_message_hash` (`provider.rs:272-273`), the same hash, tested over the corpus on every
  network.
- ProRegTx, shared consent: `verify_hash_canonical` (`provider.rs:284`).
- ProUpRegTx: `verify_hash` (`provider.rs:397`).
- ProUpShareTx and ProUpSharedRegTx: `verify_hash_canonical` (`shared.rs:207, :253`).
- ProUpServTx and ProUpRevTx: the BLS `SigVerifier` under `VerifyScheme` (`provider.rs:493, :524`),
  with the side-car signature.

# T6 OK-3 D1: the operator key and signature differential

The probe checks that pinning the base-sdk rc and building the payload side-car
(`dash_consensus::primitives::CorePayload`) changes nothing the node reads from a provider
payload's operator key or operator signature. Each row is decoded twice:

- **old** (`old/`): base-sdk's fork at 29e01c83, the pin before OK-3. Read the way the node read
  it then: the fork decoder's verdict and its payload's own BLS fields, its sign-hash helpers, and
  node-l1's Core image (with `patch_lazy_key`).
- **new** (`new/`): the node at OK-3's head on the rc pin. That means the side-car's verdict,
  key and signature, dash-dml's sign-hash builders, and node-l1's Core image (`core_bytes`, copied
  here: it is private to node-l1's oracle module).

Both print the same JSON per row:

- the decode verdict (`PayloadError` text, or `consumed` and `is_complete`);
- whether the carrying transaction's payload is accepted;
- the key probes of `common/bls.rs`: fresh bytes, `get()` bytes under both schemes, `to_bytes_as`
  fresh and after `get()`, `is_default`, `==` against masternode state keys of both schemes, and
  the printed form;
- the signature's validity and bytes under both schemes;
- every sign hash: the payload hash, and for a ProRegTx the sign string and message hash on
  mainnet, testnet and regtest plus the shared-registration consent hash;
- the Core image.

`compare` joins the two outputs by row id. Pass means it exits 0, which it does only when:

- no row differs, no id is on the old side only, and no id occurs twice on either side;
- both sides have one row per `rows.tsv` line;
- there are 2 item 4 rows and both sides refuse both;
- there are at least 84 crafted rows and item 5 has no failure.

Otherwise it exits 1, and the summary's `failed` names the checks that failed.

The probe is two workspaces because of the base-sdk pin rules (`contrib/node/base_sdk_pins.py`):

- `old/` pins the fork revision directly;
- `new/` repeats the root's `[patch]`, as `../dde-probe/bs7` does.

## Rows

- **Inputs:** every type 1, 2, 3, 4 and 12 transaction found in these sources:
  - base-sdk's corpora at the rc (`proregtx`, `v24`, `payload_core`, `sighash` and
    `decode_domain`);
  - rdc fixtures: dash-oracle-sys, dash-dml, node-l1 and conformance test data, and the
    statedigest testnet set;
  - the statedigest regtest chains and the `setup-dashd.py` regtest chains (block files);
  - dash-dml's T6 sign-hash corpus (`sig/tests.rs` `t6_probe_rows`);
  - node-l1's BS4b rows (`bsdk_recorded.rs` `t6_probe_rows`).
- **Mutations:** each input is also written with its operator key (types 1, 3 and 12) or
  operator signature (types 2 and 4) replaced by six values: zeros, 0xff, canonical legacy only,
  canonical basic only, canonical in both, and a sixth: for a key, a valid point's non-canonical
  encoding (`exotic`); for a signature, a value Core's payload read refuses (`refused`).
  Keys also take four values from dash-pkc's tests at the rc: `ZZ_X1` and `ZZ_X2`
  (`pkgs/pkc/src/bls/elem.rs`) and `G1_OFF_SUBGROUP_CHIA` and `G1_OFF_SUBGROUP_IETF`
  (`bls/tests.rs`).
  No key fills the key `exotic` class, and that is structural. At the rc, a G1 encoding that
  either scheme reads as a valid key is that scheme's canonical encoding. So the class is empty by
  construction, not for want of candidates.
  Types 1 to 4 run each of these as written and at versions 1, 2 and 3. `values.json` records the
  values chosen, any class no candidate filled, and the inputs with no field to replace (their
  payload fails to decode, or is a provider payload of an unknown version).
- **Item 4 (OK3-CARRY):** one ProUpServTx row and one ProUpRevTx row (`item4:*`) whose signature
  Core's payload read refuses. Each is built from a corpus row by changing the signature only;
  `compare` checks that both sides refuse it with Core's error. `values.json`'s
  `item4_sig` names the signature's source: the signature mutation value it equals, or its hex.
- **Item 5 (OK3-CARRY):** every row of base-sdk's crafted corpus `payload_core.json5`, unmutated
  (`crafted:*`), checked under both Core flags (basic and legacy):
  - Core's outcome is either nothing or one payload object, never an RPC error, as in the
    deleted fork test;
  - the node's verdict equals Core v24.0.0-rc.3's (an object printed or not) and the old pin's;
  - where Core accepted, the operator key the node prints equals Core's `pubKeyOperator`.

  Core's JSON prints no operator signature for ProUpServTx or ProUpRevTx, so those rows are
  checked on the verdict only. For a ProUpRegTx of an unknown version, Core reads the version alone
  and prints its default key, all zeros; the node holds no key there. As in the deleted fork test,
  such a row passes when Core's key is the default.

## Running

```
contrib/node/t6-probe/run.sh <out-dir>
```

The script:

1. builds both sides (`--locked --offline`);
2. dumps the two row sets with their ignored tests;
3. writes the input rows (`rows.tsv`, `values.json`);
4. runs both sides (`old.jsonl`, `new.jsonl`);
5. writes `d1.jsonl`: one line per row with both sides' values and `same`, then a summary line
   with the row and difference counts and the item 4 and item 5 results.

It fails if it finds no `setup-dashd.py` regtest chain or no statedigest chain, and it exits with
`compare`'s status.

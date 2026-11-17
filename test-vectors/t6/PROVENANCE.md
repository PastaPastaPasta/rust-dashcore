# Provenance of `test-vectors/t6/`

Unlike the rest of `test-vectors/`, these files are written by this repository's own code:
they pin what the node computes on the base-sdk pin `29e01c834d3eb5d647db7eb51578350a77593847` (`node/base-sdk`), so the
operator-key re-pin (T6) can show that it changes none of it. Each is written by
`T6_GOLDENS_RECORD=1 cargo test -p dash-dml --lib t6_` and asserted by the same tests without
the variable.

| File | Test | Records |
|---|---|---|
| `sign-strings.txt` | `specialtx::sig::tests::t6_sign_strings_are_pinned` | `make_sign_string` of every ProRegTx in the sign-hash corpus (`dash-dml/tests/data/core_signed_protx.txt`, Core's `trivially_valid.json` and `trivially_invalid.json`, the blocks of `dash-dml/tests/data/ev2b-quirks.txt`, built version-3 registrations: Regular and Evo, each paying a P2PKH, a P2SH and a non-standard script, and one each with an extended net info holding a domain and an invalid entry and with an extended net info of an unknown version) on mainnet, testnet and regtest, and of every ProRegTx in the two mainnet build windows (below) on mainnet: `<txid> <network> <string>` |
| `dml-lists.txt` | `t6_goldens::tests::t6_list_goldens` | For each block with a ProRegTx, ProUpRegTx or ProUpSharedRegTx: the built list's merkleRootMNList, then each masternode such a transaction touches, with its class (legacy, basic, migrated, shared, evo), its `CDeterministicMNState` codec bytes, its SML entry bytes and its unique-property keys |

`dml-lists.txt` inputs, all in dash-dml's fixtures (the `setup-dashd.py` regtest chain is not
reachable from unit tests):
- the accepted blocks of `dash-dml/tests/data/ev2b-quirks.txt` (regtest blocks over Core-recorded
  lists): the basic and shared masternodes;
- the first four such blocks of each mainnet build window
  (`dash-dml/benches/data/mainnet-dml-1028160-1030159.bin`, `...-1371028-1373027.bin`): legacy
  registrations;
- each window's last block again with one more transaction, a version-2 ProUpRegTx that moves the
  first legacy, non-shared regular masternode of the list to a basic key (`testkit::operator_key(0x71, Basic)`):
  the migrated masternodes. The transaction is built by the test, not mined, and is applied
  without `check_pro_up_reg_tx` at heights before V19, where Core refuses a version-2 ProUpRegTx.
  The masternode state in these rows is one the chain cannot reach: they pin the codec, SML and
  unique-key bytes of a migrated state only, not a state the node persists.
- each window's last block again, under V19 and V24 rules, with one more transaction built by
  the test:
  - a version-2 ProUpRegTx that moves the second such masternode to its own key's point in the
    basic encoding (the same key, so the state's key object is kept and re-set as basic): the
    "same-key migration" rows;
  - a version-3 Evo ProRegTx with an internal collateral: the "evo" rows.
- the first EV2b quirk block whose list holds a shared masternode, replayed with one more
  transaction built by the test: a ProUpSharedRegTx that gives that masternode a new basic
  operator key and a new voting key (the "shared operator change" row).

None of these test-built transactions passed the special-transaction checks (their signatures
are not valid), and the window heights predate V19 and V24 on mainnet: the rows pin what the list
build makes of them, not states the chain reached.

# TxToUniv fixtures (RP1a-2c)

`decoderawtransaction-v24.0.0-rc.4.jsonl.gz`: one JSON object per line, with these fields:

- `net`: `main`, `testnet` or `regtest`.
- `service`: whether dashd ran with `-deprecatedrpc=service`.
- `bucket`: the shape the transaction was picked for. This is `t<type>/pv<payload version>[/mn<masternode type>]` (a quorum commitment adds `/qv<commitment version>/null` or `/full`, `null` where its members signature is all zeros; a version-3 ProRegTx or ProUpServTx adds `/domain` for a domain network entry and `/invalid` for an invalid one; a ProRegTx adds `/ownreward` for a collateral share whose reward script is set and differs from its refund script; a ProUpShareTx adds `/noreward` for an empty reward script), or `plain/v<version>/in<inputs, at most 3>`.
- `source`: the blk file the transaction was read from.
- `dashd`: the subversion of the dashd that decoded it.
- `hex`: the transaction.
- `reply`: dashd's JSON-RPC reply to `decoderawtransaction <hex>`, byte for byte.

Core's `decoderawtransaction` is `TxToUniv(tx, uint256(), entry, /*include_hex=*/false)` (rpc/rawtransaction.cpp:956). Its output therefore checks rdashd's `tx_to_univ` and the payload objects it prints, for the same bytes and parameters, without a chain.

Nothing in the file was written by hand. It was made in three steps on 2026-10-29, and remade with the `/domain`, `/invalid`, `/ownreward` and `/noreward` dimensions the same day (RP1a-2c r1, m-2):

1. **Read.** `extract.py` read Dash Core's own blk files read-only. Those were:
   - the five regtest chains Core's functional tests built under v24.0.0-rc.3: `feature_asset_locks`, `feature_masternode_payout_shares`, `feature_masternode_shares`, `feature_protx_version` and `rpc_netinfo`, kept in `/work/conformance/regtest-v24/`;
   - every testnet blk file of the reference oracle (`/work/chains/testnet`);
   - the last two mainnet blk files of the mainnet oracle.

   It kept up to three transactions per network and bucket, and at most four plain transactions per file. A coinbase without a payload (type 0) was skipped.
2. **Decode.** `decode.py` ran dashd v24.0.0-rc.4 offline on fresh data directories:
   - The binary was `~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd`, as `contrib/setup-dashd.py` caches it. `dashd -version` reports v24.0.0-rc.4, and its sha256 is `e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7`.
   - It started one node for regtest and one for mainnet, each first without and then with `-deprecatedrpc=service`.
   - Core prints three BLS fields under its process-wide scheme flag: a CbTx's `bestCLSignature`, an MnHF signal's `sig` and an asset unlock's `quorumSig`. The flag stays legacy until the tip passes V19. The regtest node therefore mines one block (`generatetodescriptor 1 raw(51)`; V19 is active from height 1) and so prints those fields as a synced node does.
   - The regtest node also decodes the testnet rows, since testnet has regtest's address prefixes and Platform HRP. `dashd` names the node that decoded each row.
   - Every reply was a result; none was an error.
3. **Trim.** `trim.py` cut the rows down by these rules:
   - per network, bucket and `service`, the two shortest transactions are kept;
   - the `service` rows are kept only for ProRegTx and ProUpServTx (types 1 and 2), the only payloads that flag changes;
   - no mainnet row is kept whose output depends on the BLS flag (CbTx version 3 and above, MnHF signals, asset unlocks). The offline mainnet node cannot leave the legacy flag, so it printed those fields as zeros; the testnet and regtest rows cover those payloads.
   - the result was compressed with `gzip -9 -n`, leaving 221 rows.

Coverage: every special type (1 to 12) at every payload version the chains carry, including:

- legacy and basic BLS ProTxs;
- Evo ProRegTxs and ProUpServTxs, with legacy platform ports and with extended network info;
- payout lists and shares, including a share with its own reward script;
- a domain `platform_https` entry (regtest `rpc_netinfo`);
- CbTx versions 1 to 4;
- LLMQ commitments at versions 1 to 4, null and full (mainnet carries only versions 3 and 4; a full version 2 commitment comes from testnet only);
- MnHF signals;
- asset locks and unlocks at versions 1 and 2.

Testnet and regtest have the same address prefixes and Platform HRP, so the test decodes both under one set of parameters.

sha256 of the `.gz`: `2ec5d706a4063d369972be25b0d0a622bf60b719af380c7b337bdd2aa6204dc3`.

## Crafted transactions

`decoderawtransaction-crafted-v24.0.0-rc.4.jsonl.gz` holds **crafted** transactions: shapes no chain carries. Each one is a chain transaction from the file above with one payload field edited, and is not valid on any chain. It has the same fields; `bucket` starts with `crafted/`, and `source` names the edit, the bucket and txid of the transaction it was made from, and that transaction's blk file. `decoderawtransaction` checks no validity, so dashd prints the edited bytes as it would print them in a stored block.

`craft.py` made three transactions from the `/domain` extract on 2026-10-29:

- `crafted/t2/pv3/mn1/invalid`: an Evo ProUpServTx v3 with one more `platform_https` entry, of type `0xff`. Core reads that entry as Invalid and prints `[invalid entry]`.
- `crafted/t11/pv1/noreward`: a ProUpShareTx with an empty reward script. Core prints no `rewardAddress`.
- `crafted/t1/pv2/mn0/opreward7`: a ProRegTx v2 with `nOperatorReward = 7`. Core prints `0.07000000000000001` (`%.16g`).

`decode.py` decoded them with the same dashd and nodes as above, and `trim.py` applied the same rules to the bucket after `crafted/`. The ProUpShareTx loses its `service` row, leaving 5 rows. Every reply was a result.

sha256 of the `.gz`: `d7b94fad5385593bd17fae4ea76d707e96feaa938f88b75d83f4fe6c5d338259`.

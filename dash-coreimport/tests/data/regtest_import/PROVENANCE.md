# cache-chainstate-000006.log

Our own regtest data: the chainstate WAL of node0 of the Dash Core functional-test framework's
regtest cache (the framework's cache recipe, `test_framework.py` `_initialize_chain`, 199 blocks),
built on 2026-11-26 by the dashd v24.0.0 release binary that `contrib/node/func-compat` pins for
v24.0 (`Dash Core version v24.0.0`, `bin/dashd` sha256
2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223), and copied into
`rpc_txoutproof`'s node0 by the func-compat runner (`--pin v24.0 --phase M5`, mask `all`, branch
`node/func-compat-v24.0-v2400` at 64cad32b) at 07:48Z that day.
The other three files of that chainstate (`CURRENT`, `MANIFEST-000004` and the table
`000005.ldb`, 198 bytes) are literal arrays in `tests/regtest_import.rs`; its `LOCK` is empty.
The obfuscation key `5c9b2f71f00366e3` is in the table only.

SHA-256:
- `000006.log` ca95fb110ec6a94cef1a0459a028c4f362f8a23002c82411d85bb2c16d52e040
- `000005.ldb` 2ef631fab7c4431d4ab316d30a7d863bd2a8f32cc86df584d99112de4dfa5cba
- `MANIFEST-000004` 93f08f7d8fecb2692b68e087c22a00cdbe9cd76ce9c900889e23f1bac572fd9b
- `CURRENT` 0861415cada612ea5834d56e2cf1055d3e63979b69eb71d32ae9ae394d8306cd

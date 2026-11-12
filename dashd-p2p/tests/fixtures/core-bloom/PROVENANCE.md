# Provenance of the Core bloom vectors

Every `*.hex` here is a transaction or block from Dash Core's own unit test
`src/test/bloom_tests.cpp` at the release tag v24.0.0-rc.3 (commit `0364ed626e`), extracted by
`contrib/node/p4b-bloom-vectors.py` (its docstring names each file):

    git -C <dash> show v24.0.0-rc.3:src/test/bloom_tests.cpp > bloom_tests.cpp
    python3 -I contrib/node/p4b-bloom-vectors.py bloom_tests.cpp dashd-p2p/tests/fixtures/core-bloom

`dashd_p2p::processing::bloom` and `::merkle` replay the Core test cases that use them
(`bloom_match`, `dip2_bloom_match`, `dip2_bloom_update`, `merkle_block_2`,
`merkle_block_2_with_update_none`, `merkle_block_3_and_serialize`, `merkle_block_4`,
`merkle_block_4_test_p2pubkey_only`, `merkle_block_4_test_update_none`) with Core's expected
values (P4b D8).

The `v24-*.hex` files are transactions dashd v24.0.0-rc.3 made in the regtest chains of Dash
Core's own functional tests (`feature_masternode_payout_shares.py`, `feature_masternode_shares.py`,
`feature_asset_locks.py`), taken from its `getblock <hash> 2` replies in `conformance/tests/data`
(recorded by `conformance/scripts/regtest-v24.sh`) by `contrib/node/p4b-special-tx-vectors.py`,
which names each source block and checks the txids it pins:

    python3 -I contrib/node/p4b-special-tx-vectors.py conformance/tests/data dashd-p2p/tests/fixtures/core-bloom

`dashd_p2p::processing::bloom::tests::v24` uses them for the special-transaction arms of
`CheckSpecialTransactionMatchesAndUpdate` (P4b D17).

# Provenance of `test-vectors/evo-driver/`

Fixed vectors for the BL-875 chain driver's offline tests (`contrib/node/evo-images/tests/test_driver.py`).
Nothing here is hand-written: two blocks are copied from Dash Core's sources, the third was recorded from a Dash
Core v24.0.0 regtest node by a script in `contrib/node/`.

| File | sha256 |
|---|---|
| `core-v24.json` | `df56e9f6aa20aede8f6e638ee1f3939c5d2e4f360fa7bf1ca3029ba2167565dd` |

## `dashbls_legacy_threshold`

A verbatim copy of the threshold example in dashpay/dash v24.0.0 (`938fba502824b9d850b7c710601456dc3d71603e`),
`src/dashbls/src/test.cpp:1253-1322` (`TEST_CASE("Schemes")`, `SECTION("Legacy scheme")`): `vecIdsHex` as `ids`,
`vecSecretKeySharesHex` as `secret_key_shares`, `strThresholdPublicKey` as `threshold_public_key_legacy`.

- Core's test reverses each id before use (`std::reverse(vecIds.back()...)`), so the ids are written in RPC
  (`uint256::GetHex`) order, the order `quorum info` prints proTxHashes in.
- The public key is in the legacy serialization. `G1Element::SerializeToArray` (`src/dashbls/src/elements.cpp:178-198`)
  sets 0x80 for the sign of y in legacy form, and 0x80 (compressed) plus 0x20 (sign) in the basic form; the test turns
  one into the other.
- Core's test recovers the threshold signature from the matching signature shares and verifies it with this public
  key. The driver's test instead recovers the threshold secret key from the secret key shares and checks that its
  public key is this one, which pins the share-id byte order and the position of the G1 sign flag.

## `dashbls_basic_keygen`

A verbatim copy of `sk1` and `pk1` from the same file, `src/dashbls/src/test.cpp:336-365`
(`TEST_CASE("Chia test vectors")`, `SECTION("Chia test vectors 1 (Basic)")`): `KeyGen` of 32 zero bytes, both as
`Serialize()` prints them. Its y is the smaller root and odd, so it tells the sign rule Core uses (y > (p−1)/2) apart
from a parity rule, which the threshold key above (larger root, odd) cannot.

## `mnhf_signal_tx`

Network: **regtest**. The bit-13 MNHF signal tx the masternodes of a Dash Core v24.0.0 regtest network created
(dashd sha256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`). It was recorded by
`contrib/node/evo-images/driver_standalone.py` (run r9 of BL875-Q, 2026-11-14, through `driver-standalone.sh`) from
node0's mempool (`getrawtransaction`), and mined in that run's block 650. `quorum_hash` is the block's
`mnhfTx.signal.quorumHash`. The test rebuilds the tx byte for byte from (bit, quorum hash, sig) and checks its txid,
which pins the serialization the driver hashes for the masternodes' MNHF msgHash.

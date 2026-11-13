# Operator-key vectors

`operator.json` copies the first public-key pair from `dashpay/base-sdk`, commit
`ed39410cee114d0e569b533ac300831c4122b6e9`,
`pkgs/pkc/corpus/bls_serialize.json5`: `chia.pk[0]` (legacy) and `ietf.pk[0]`
(basic). The two encodings represent the same point. These are the paired
serialization vectors used by `pkgs/pkc/src/bls/tests.rs`.

The expected hashes were calculated independently with Python 3:

```python
first = hashlib.sha256(bytes.fromhex(pubkey)).hexdigest()
hashlib.sha256(bytes.fromhex(first)).hexdigest()
```

Hashes are in internal byte order, matching `uint256`'s storage, rather than RPC
hex. Core v24.0.0 `bls/bls.h:545-560` writes the raw key bytes to `CHashWriter`;
`hash.h:117-121` applies SHA256 twice. Entry version 1 selects legacy encoding;
version 2 selects basic (`evo/dmnstate.h:107`).

The `dashd-p2p/tests/fixtures/dashd-v24/inbound-evict-mn.txt` fixture records B's
proRegTx and RPC-form operator hash, but omits B's public key. Its
`PROVENANCE.md:129-140` confirms that omission, so that fixture cannot independently
pair a key with its recorded hash. The pinned `dash-pkc` vector is used instead.

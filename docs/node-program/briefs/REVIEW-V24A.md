# Review brief: V24a (Dash Core v24 wire format in `dashcore`)

Program: rust-dashcore is becoming a full Dash node with bit-for-bit parity with Dash Core. This task is in the **consensus/parser** risk class, so it gets multiple independent reviewers. Read /work/home/agent/workspace/node-program/PROCESS.md.

Branch: `node/v24a-dashcore-wire`, worktree /work/home/agent/workspace/rdc-wt/v24a-dashcore-wire. Diff: `git diff node/integration...HEAD`, 10 commits from `4391a292`. It cherry-picks upstream PRs #1137, #1136 and #1138 (view them via `gh api repos/dashpay/rust-dashcore/pulls/<N>`; use REST, not GraphQL), then adds: the v24 transaction layer (payload length prefix honoured; a payload must parse exactly, otherwise it is kept raw as `UnknownPayloadType`; i16 version; payload only from version 3; no BIP144 framing on special txs); types 10/11/12; ProRegTx v3 (extended addresses, DIP-0026 payouts, shares, join sigs, early-exit terms); ProUpServ/ProUpReg/ProUpRev v3; CbTx v4; asset unlock v2 stable txid plus `instance_hash()`; inventory type 34; protocol constants; the getqrinfo cap; size/weight fixes; key-wallet v3 payout matching; Core-produced vectors (v24.0.0-rc.3 regtest) and regeneration tooling under dash/contrib/core-vectors.

Dash Core reference: /work/home/agent/workspace/dash (develop and tag v24.0.0-rc.3). Key files: src/primitives/transaction.{h,cpp}, src/evo/providertx.{h,cpp}, src/evo/cbtx.h, src/evo/assetlocktx.h, src/evo/netinfo.{h,cpp}, src/evo/dmn_types.h, src/protocol.h, src/version.h.

Check adversarially:
1. Byte-level parity of every encoder/decoder with Core's SERIALIZE_METHODS, including version-gated fields, compact sizes, NetInfo v3 address types (IPv4/IPv6/Tor/I2P/domain) and their validation at decode, share and payout list encodings, and sig fields.
2. The "payload must parse exactly, else raw" rule: is it what Core does? (Core fails deserialization of a block whose special payload doesn't parse, or treats it as invalid; which one, and where?) An SPV library that silently accepts what Core rejects, or rejects what Core accepts, matters. Determine precisely what Core does with a malformed payload of a known type, and whether this design is safe for an SPV/wallet library and for the future node (which must reject exactly what Core rejects; the node will use base-sdk types, but note the divergence).
3. Asset unlock v2 stable txid: exact conditions vs Core's `GetHash`/`ComputeHash` (version>=3, type==9, payload size, payload[0]>=2), and the `instance_hash` semantics. Also check the downstream impact on Platform's use of `txid()`.
4. The i16 version handling: the parse must match Core's struct model exactly. Don't discuss anything beyond that.
5. Bincode/serde backward compatibility for persisted wallet data (enum variant order, new fields) and the FFI ABI.
6. Vector provenance (truly Core-produced?) and test quality; missing vector coverage (e.g. qrinfo).
7. The 4 key-wallet performance test failures under load: flaky timing tests. Recommend whether to keep them.
Re-run at least `cargo test -p dashcore` and the vector tests.

Write the review to the path given in your task: findings with severity (blocker/major/minor/nit), file:line, the concrete failure scenario, and a fix. End with a verdict: APPROVE, APPROVE-WITH-NITS, or CHANGES-REQUIRED. Don't modify the branch and don't commit.

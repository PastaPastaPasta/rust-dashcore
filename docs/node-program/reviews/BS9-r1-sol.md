# BS9a and BS9b independent review (Core v24.0 d4c16888b45a)

I reviewed the named base-sdk and rust-dashcore revisions against the pinned Core source. “Exact” below means the node composes the documented handler checks with `P2pMsg::decode_with_policy`, at the correct chain parameters, peer version, and process-wide BLS scheme. The stateless decoder alone deliberately has a broader read domain for DKG, `dsi`, `dss`, and `govsync`. The reported 1,686-case differential and green gates are supporting evidence; I also inspected the code paths and mutated one bound in each slice.

## BS9a — overlay wire types II

### Read domain and write-back

| Messages | Core read and result |
| --- | --- |
| `qcontrib` | Core `src/llmq/net_dkg.cpp:43-75,99-120` caps the payload, requires exactly `threshold` vector keys and `minSize..size` encrypted blobs, and walks all bytes. base-sdk `dkg.rs:218-251` does the same. Its raw BLS fields require the node’s eager read under `bls_legacy_scheme`; with that step, the acceptance and refusal sets match. |
| `qcomplaint` | Core `src/llmq/net_dkg.cpp:122-134` bounds both bitsets by quorum size, requires equal bit counts, rejects padding bits, and consumes the payload. base-sdk `dkg.rs:290-310` matches, with the same eager signature step. |
| `qjustify` | Core `src/llmq/net_dkg.cpp:136-148` bounds revealed shares by quorum size and reads each `u32 + 32-byte` share. base-sdk `dkg.rs:312-381` matches after the node eagerly reads each secret-key scalar and the signature. |
| `qpcommit` | Core `src/llmq/net_dkg.cpp:150-159` bounds the valid-members bitset and reads the fixed key, hash, and two signatures. base-sdk `dkg.rs:384-451` matches after eager BLS reads. |
| `qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare` | Core `src/llmq/net_signing.cpp:27-44,66-148`, `src/llmq/signing_shares.h:46-47,94-157`, and `src/serialize.h:546-575` impose outer limits 100/200/200/400/32, an inner inventory or batch limit of 400, and a total `qbsigs` share limit of 400. base-sdk `sigshare.rs:29-41,201-221,255-271,325-405` matches those counts, varints, bitsets, and lazy signature bytes. Handler gates still precede the read. |
| `qsigrec` | This is the existing `RecoveredSig` path, not a new BS9a change. Core `src/llmq/signing.h:74-105` reads its signature lazily; base-sdk `llmq.rs:23-50` preserves those bytes. Its chain-specific type gate remains with the handler (`src/llmq/net_signing.cpp:49-62`). Its earlier `core_overlay.json5` corpus covers this path; the BS9 judge does not add a new case. |

The DKG worker’s typed read is whole-payload (`src/llmq/net_dkg.cpp:370-385`, `dkg.rs:95-101`). Core eagerly decodes DKG BLS fields (`src/bls/bls.h:183-209`) and may serialize an accepted field as canonical bytes or zero. The BS9 predictor applies `BlsEagerPk/Sig/Sk` with the process scheme before comparing write-back and sign hashes (`rdc contrib/node/dde-probe/bs9/src/predict.rs:65-77,155-205`). It handles the `qjustify` share explicitly. Signature-share fields, including `qsigrec`, are Core lazy bytes (`src/llmq/signing_shares.h:49-87`, `src/llmq/signing.h:74-101`); they retain their bytes. `AUTOBITSET` may write back the shorter form, and base-sdk makes the same choice (`src/serialize.h:546-575`, `sigshare.rs:157-180`). No unexplained write-back difference was found in the tested reads.

Core keys DKG inventory by the *received* payload before the worker read (`src/llmq/net_dkg.cpp:616-619`); it computes DKG sign hashes from the reserialized object with a zero signature (`src/llmq/dkgmessages.h:63-69,99-104,130-135,181-184`). The predictor compares both in that order. The signature-share request hash is type, quorum, id, and message hash (`src/llmq/signhash.h`; `sigshare.rs:43-52`). CodeQL’s `policy.qll:74-79` exemptions cover published signature shares and `DkgRevealedShare`, whose scalar is put on the wire by Core (`src/llmq/dkgmessages.h:107-127`); I found no exempted still-private key type.

### Findings

- **Minor — `pkgs/p2p_core/src/msg/dkg.rs:18-21,89-92`: preserve the raw inventory hash in the eventual node adapter.** For accepted corpus case `qcomplaint_481`, Core’s inventory hash of the received bytes begins `38fd36d1`, while hashing the eagerly normalized write-back begins `13fce9bc`. A node that normalizes first and then calls the message’s `Hashable::hash()` would announce or look up the wrong inventory key, even though its signature hash is correct. Core hashes before normalization at `src/llmq/net_dkg.cpp:616-619`; the BS9 predictor already gets this right. Expose the raw hash alongside `decode_checked` or document this ordering at the public DKG API and enforce it in the node adapter.

- **Minor — `pkgs/p2p_core/src/msg/dkg.rs:10-16` and `pkgs/p2p_core/src/msg/mod.rs:168-179`: clarify DKG gate order for the node port.** The docs say handler gates come before decoding, but the unsolicited-GETDATA gate is *after* the cap, framing walk, and raw hash (`src/llmq/net_dkg.cpp:590-637`). For an unsolicited malformed DKG payload, checking GETDATA first would score 10 instead of Core’s framing score 100 (`src/msg_result.h:21`). Spell out this exception to the “gates first” rule where handler authors will read it.

### Test coverage

`core_overlay_ii.rs:107-148,156-209` tests the check stage, read verdict, consumed bytes, hashes, and ordinary write-back; `overlay_ii.rs:99-206,331-363` tests cap and count boundaries. The corpus intentionally delegates eager BLS normalization/write-back to the Core judge. In a detached scratch worktree, changing `dkg.rs`’s slack from 1024 to 1023 made `dkg_size_caps_are_cores` fail both sizes and the serde corpus fail on `qcomplaint_166`: Core classified it as framing, the mutant as oversized. Thus the bound and refusal stage are caught.

## BS9b — overlay wire types III

### Read domain and write-back

| Messages | Core read and result |
| --- | --- |
| `dsa` | Core `src/coinjoin/coinjoin.h:137-166` reads the flags byte at version 70241 or later and omits it earlier. base-sdk `coinjoin.rs:64-103` does the same when given the negotiated version. Decoding and then encoding preserves the bytes Core consumed. |
| `dsi`, `dss` | Core `src/coinjoin/coinjoin.h:202-220` bounds each `dsi` list by `GetMaxPoolInputOutputCount()`; `src/coinjoin/server.cpp:254-263` checks a `dss` count before any input and replies `ERR_MAXIMUM`. base-sdk `coinjoin.rs:32-41,124-134,171-189` uses chain parameters, with `dsi` checked after its read and `dss` before. Their composed accept/refuse results and `dss` reply stage match. The raw decoder alone accepts over-pool lists. |
| `dsf`, `dsc`, `dssu` | Core `src/coinjoin/client.cpp:105-142` reads a session id plus transaction or raw message code; `src/coinjoin/coinjoin.h:113-135` serializes four status `i32`s without the memory-only count. base-sdk `coinjoin.rs:136-231` matches the bytes and preserves unknown enum values. Core passes one stream through client sessions in turn; the node must do likewise. |
| `dsq`, `dstx` | Core layouts and hash contexts are `src/coinjoin/coinjoin.h:245-349`, `src/coinjoin/coinjoin.cpp:43-47,77-79`. base-sdk `coinjoin.rs:233-318` matches. Any nonzero `dsq.ready` byte reads true and writes `01`, including for the signature hash; the inventory hash includes the signature. `dstx`’s key is its transaction hash, and its signature hash includes `sigTime` but excludes signature bytes. |
| `govobj`, `govobjvote` | Core `src/governance/common.h:70-84` and `src/governance/vote.h:190-206` match base-sdk’s field order. A network vote read permits only signature lengths 65 or 96 and refuses a larger count before allocation (`gov.rs:44-71`); the base codec remains unbounded for non-network uses. The new corpus compares identity and signature hashes, including Core’s vote-hash padding (`src/governance/vote.cpp:145-160`). |
| `govsync` | Core reads its hash separately, then the whole `CBloomFilter`, with a 36,000-byte data cap and a post-read hash-function cap (`src/governance/net_governance.cpp:92-110`, `src/common/bloom.h:76-88`). base-sdk `gov.rs:20-42` and `bloom.rs:18-64` now read all fields. With `is_within_size_constraints` at the handler, accepted/refused bytes and the log-vs-score split match; the stateless read alone allows an over-limit hash-function count. |

Aside from `dsq`’s Core boolean normalization, the accepted bytes written back match Core’s consumed prefix. Core ignores trailing bytes in these handlers; base-sdk reports the consumed length. The governance object identity hash includes its signature (`src/governance/common.cpp:23-38`), whereas its signature hash excludes it. A vote’s signature hash is serialized fields without its signature (`src/governance/vote.h:183-205`); on mainnet Core checks an ECDSA vote over `GetSignatureString()` instead (`src/governance/vote.h:166-172`, `src/governance/vote.cpp:164-185`). `GovVote::signature_hash` documents this; `signature_string()` is still a governance-port task, not a wire-read method. The node-owned state gates and per-session CoinJoin stream reads are documented at `msg/mod.rs:190-209`, `coinjoin.rs:136-141`, and `gov.rs:35-39`.

### Finding

- **Nit — `pkgs/p2p_core/src/msg/coinjoin.rs:71-76`: make old-peer outbound `dsa` encoding explicit.** A newly constructed `DsAccept { flags: Some(1), .. }` encoded for a 70240 peer contains a flags byte, whereas Core’s versioned writer omits it (`src/coinjoin/coinjoin.h:160-165`). Decode-then-encode of a 70240 message is correct because decoding sets `None`; the risk is outbound construction. Add a version-aware send helper or an explicit call-site rule that sets `flags: None` for older peers.

### Test coverage

`core_overlay_iii.rs:107-184` checks Core verdict, consumed bytes, write-back, and hashes; `overlay_iii.rs:69-277` covers version 70241, pool bounds, the 65/96 rule, the complete bloom filter, and `dsq` hashes. In the detached scratch worktree, changing `COINJOIN_ENTRY_MAX_SIZE` from 9 to 8 made `dsi_and_dss_counts_are_the_pools` fail and the serde corpus fail at `dsi_1554`, which Core accepted. The Core judge and corpus therefore catch this reverted pool bound. After restoring both mutations, the four targeted test binaries passed: 1 + 1 corpus tests and 35 + 16 unit tests, with zero failures.

BS9a VERDICT: APPROVE-WITH-NITS
BS9b VERDICT: APPROVE-WITH-NITS

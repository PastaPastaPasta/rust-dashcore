# V24a review, round 1 (Opus)

- **Branch:** `node/v24a-dashcore-wire` at `a22c7138` (worktree `~/workspace/rdc-wt/v24a-dashcore-wire`).
- **Diff:** `git diff node/integration...HEAD`, 10 commits from `4391a292`, merge-base `c3cda6c5`.
- **Reference:** Dash Core `v24.0.0-rc.3`. The `develop` branch (`15db4787`) was checked where it matters; it has no wire changes in the reviewed files.
- **Scope:** I wrote nothing to the branch or the worktree. The upstream PR refs I fetched for the cherry-pick check have been deleted again.

**Summary:**
- The core transaction layer, the asset unlock txid and instance hash, CbTx v4, types 10–12 and every v3 field layout match Core byte for byte. The vectors really come from Core, and the tests are strong.
- The cherry-picks of #1136, #1137 and #1138 are hunk-identical to the upstream PR heads (`6d485302`, `095cfc65`, `257334b3`).
- One parity gap remains, and Core confirms it (M1): the v3 `netInfo` decoder is stricter than Core's. As a result, ProRegTx and ProUpServTx payloads that Core accepts, and that can be mined, are kept as raw bytes, or decoded with a different signing hash.
- One FFI contract break (M2) is now reachable from the network.

**Verdict: CHANGES-REQUIRED** (details at the end).

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dashcore --all-features` | 612 unit + 5 `core_v24_vectors` + 12 + 1 + 35 doc tests pass |
| `cargo test -p dashcore` (default features) | 573 unit + 5 vectors pass |
| `cargo test -p key-wallet-ffi` | all pass (241 + integration) |
| `cargo test -p dash-spv --lib` | 621 pass |
| `cargo test -p key-wallet` | 669 pass; **2 timing failures under load** (see item 7) |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p dashcore -p key-wallet -p key-wallet-ffi -p dash-spv --all-features --all-targets -- -D warnings` | clean |
| **Provenance:** all 69 fixture txs run through `decoderawtransaction` on the local v24.0.0-rc.3 oracle (`/Dash Core:24.0.0/`) | txid, size, version, type, locktime, `extraPayloadSize` and `extraPayload` hex identical for **69/69**. The only differences are 5 BLS-signature renderings, caused by the still-syncing oracle's scheme state, not by the fixture. Script: `/tmp/v24a-r1-opus/check_vectors.py` |
| **History, testnet:** every block 0–713,499 from the testnet oracle decoded with the branch | 0 decode, re-encode, merkle or size failures. **0 payloads fell back to raw** across 869,683 typed special txs (ProRegTx/ProUpServ/ProUpReg/ProUpRev v1, CbTx v1/v2, QcTx v1) |
| **History, mainnet:** every block 0–704,799 from the mainnet oracle | 0 failures (pre-DIP3 range only) |
| Two parity subagents (Opus), split ProRegTx+NetInfo / everything else | Found M1 independently of each other. Agent A confirmed it end to end with `testmempoolaccept` on a private v24 regtest dashd; agent B confirmed it with `decoderawtransaction` on the oracle |
| `code-review` skill, high, on the branch diff | Used as one input (disposition at the end) |

## Findings

### M1 — MAJOR: v3 `netInfo` decoding is stricter than Core; Core-valid ProRegTx/ProUpServTx v3 become raw, or decode with the wrong signing hash

**Rust code:**
- `dash/src/sml/masternode_list_entry/net_info.rs:118-155` rejects any entry type other than 1/2/0xff, unknown BIP155 ids, address-length mismatches and non-UTF-8 domains.
- `net_info.rs:224-250` keeps every purpose pair in wire order, duplicates included.
- Any rejection sends the whole payload to `UnknownPayloadType` at `special_transaction/mod.rs:551`.

**What Core does (rc.3):**
- `ExtNetInfo::m_data` is a `std::map` (`evo/netinfo.h:401`), filled by `UnserializeMap` (`serialize.h:1231-1243`). Keys are sorted and **a repeated purpose is silently dropped** (the first one wins).
- `NetInfoEntry::Unserialize` (`netinfo.h:243-263`) turns an unknown type code into `Invalid` after consuming 1 byte.
- It catches `CService` decode failures and marks the entry `Invalid`. This leaves the stream wherever the throw happened (for example after the net id and the compact size), and parsing continues from there.
- Domains are `LIMITED_STRING(…, 253)` (`netinfo.h:178`), and validation (`netinfo.cpp:588+`) only sees the de-duplicated map.
- The payload signature and the collateral sign string hash the **re-serialized object** (`::SerializeHash(*this)`, `providertx.cpp:392`; `specialtxman.cpp:1089-1095` for ProUpServTx), not the wire bytes. The txid still covers the raw `vExtraPayload`.

**Concrete failure (agent A, Core `testmempoolaccept` = allowed; Rust = `UnknownPayloadType(1, …)`):**
- v3 ProRegTx netInfo `01 02 | 00 01 01 01 04 7f000001 4e1f | 00 01 00`: CORE_P2P appears twice and the second copy holds one entry of type 0x00.
- The result is the same when the second copy holds BIP155 id 7, a Tor v2 entry, a non-UTF-8 domain, or `01 01 00` (IPv4 with length 0).
- External-collateral registrations are affected too.
- Agent B got the same outcome for ProUpServTx v3 through the oracle.

**Second failure mode, typed but wrong:**
- With two *valid* copies of a purpose, or purposes out of order, Rust decodes the payload as typed.
- But `base_payload_hash()` / `payload_collateral_string()` hash the wire form, while Core signs its canonical form. Agent A reproduced a Core-accepted external-collateral ProRegTx whose Rust sign string ends `…|ca7ee1…272d` instead of Core's `…|b30aba…7631`.
- `purposes` also exposes the duplicate that Core ignores.

**Impact:**
- For the wallet: the payouts and shares of a valid, mineable v3 registration or service update are invisible to `key-wallet` (`provider_payout_scripts` only looks at typed payloads), and any Rust-side signature check disagrees with Core.
- Only the transaction's creator can build such bytes. Even so, this is exactly the "rejects what Core accepts" class the brief calls out.
- For the node: this decoder must not be reused as-is.

**Fix:**
- Decode entries exactly the way Core does, consuming the same bytes:
  - unknown type → 1 byte, `Invalid`;
  - unknown BIP155 id → compact size (≤ 512), skip the address and read the port;
  - a known id with the wrong length, an oversize length or a non-canonical compact size → stop right after the bytes Core consumed before it threw;
  - domain → raw bytes with the 253 cap (see m2).
- Keep each entry's consumed bytes, e.g. a `NetInfoEntry::Malformed(Vec<u8>)` variant, so the payload still re-encodes to identical bytes.
- Expose Core's view (sorted, first copy wins, malformed entries as `Invalid`), and compute `base_payload_hash` and the collateral sign string from that canonical re-serialization. In that form an invalid entry serializes as a single `0xff`, and duplicates are gone.
- Add agent A's Core-accepted hex (`/tmp/v24a-parity-a/signed_args.txt`, `ext_args.txt`) as regression vectors.

### M2 — MAJOR: the FFI `FFISpecialTransactionPayload` contract ("pointer non-null iff `payload_type == 1…4`") is broken for raw fallbacks

**Code:** `key-wallet-ffi/src/special_payload.rs:337-362` sets `payload_type` from `payload.get_type().to_u16()`. For `UnknownPayloadType(1, raw)` that gives `payload_type = 1` with `provider_registration = NULL`. The struct docs (`:315-333`) promise "Non-null iff `payload_type == 1`".

**Scenario:**
- A peer relays a mempool tx: version 3, type 1, a payload that doesn't decode exactly, and an output paying a wallet address.
- Before this branch the tx failed to decode. Now it decodes. The router sends `Unknown(1)` to the fund-bearing accounts (`transaction_router/mod.rs:58-61`), the wallet records it, and a C consumer that follows the documented contract dereferences NULL.
- No consumer reads these pointers yet (checked Platform `6499c680c6` and `~/workspace`), so the bug is latent, but it is reachable from the network and sits on the FFI boundary.

**Fix:**
- At minimum, document that the typed pointer is NULL when the payload couldn't be decoded, and add a unit test for the raw case.
- Since this release breaks the ABI anyway, better to add an explicit `decoded`/`raw` flag, or a raw byte view for raw payloads.

### Minor

**m1 — Rust decodes payloads as typed whose BLS signatures Core refuses to parse** (agent B, oracle-confirmed)
- **Core:** `bls/bls.h:184-203` throws "malleable BLS object" unless the bytes are all zero or a canonical encoding in either scheme.
- **Fields affected:** the ProUpServTx sig, the ProUpRevTx sig, the CbTx `bestCLSignature` and the asset unlock `quorumSig`.
- **Rust:** reads any 96 bytes (`provider_update_service.rs:258`, `provider_update_revocation.rs:125`, `coinbase.rs:205`, `qualified_asset_unlock.rs:141`).
- **Example:** a payload with `0xff`×96, `80‖00…` or `c0‖00…` is `bad-*-payload` in Core and typed in Rust.
- **Impact:** no harm for SPV, which doesn't validate. The node must apply the check.
- **Fix:** document this, or check canonicity under the `bls` feature.

**m2 — no 253-byte limit on domains** (`net_info.rs:139`, `String::consensus_decode`)
- **Core:** reads the domain with `LIMITED_STRING(253)`, which throws, so the whole payload fails (`bad-protx-payload`; agent A confirmed on dashd).
- **Rust:** decodes a 254-byte domain as typed.
- **Fix:** read the compact size first and reject anything over 253.

**m3 — CbTx `bestCLHeightDiff` has no `MAX_SIZE` limit** (`coinbase.rs:200`, `read_compact_size` accepts up to `u32::MAX`)
- **Core:** `ReadCompactSize` throws above `0x02000000` (`serialize.h:318-320`).
- **Bytes:** `fe 01 00 00 02` fails in Core and is typed in Rust.
- **Fix:** add the range check.

**m4 — BIP152 short IDs still use `txid()`** (`dash/src/bip152.rs:226`)
- **Core v24:** `blockencodings.cpp:33` uses `GetShortID(tx.GetInstanceHash())`, and the mempool's `vTxHashes` hold instance hashes (`txmempool.cpp:475`).
- **Impact:** for a block containing a version 2 asset unlock, `HeaderAndShortIds::from_block` now produces a short ID that Core can't match against its mempool.
- **Fix:** use `tx.instance_hash()` for compact-block version 1. The branch changed the meaning of `txid()` without auditing callers that need per-instance identity; this is the only such caller I found inside the crate.

**m5 — downstream (Platform): the withdrawal vote extension must switch to `instance_hash()` before Platform emits version 2 unlocks**
- **Platform today:** `packages/rs-drive-abci/src/platform_types/withdrawal/unsigned_withdrawal_txs/v0/mod.rs:164` sets `extension = tx.txid()`, and that extension is the message the quorum signs.
- **Core v24:** verifies the unlock signature over `::SerializeHash(tx_copy)` with only the sig zeroed (`evo/assetlocktx.cpp:222-229`; the comment there spells this out). That is `instance_hash()` of the unsigned tx, **not** the stable txid.
- **Why nothing breaks yet:** Platform still builds version 1 payloads (`rs-dpp/src/withdrawal/document_try_into_asset_unlock_base_transaction_info/v1/mod.rs:78`), and for those `txid() == instance_hash()`.
- **When it breaks:** the moment Platform moves to version 2 on this crate, every withdrawal is rejected with `bad-assetunlock-not-verified`.
- **Side effect on stored data:** Platform's persistence integrity check (`rs-platform-wallet-ffi/src/persistence.rs:5131`, `tx.txid() == rec.txid`) will drop any stored version 2 unlock whose key used the old full hash. That only matters on devnets or regtest before V24.
- **Fix:** the CHANGELOG sentence "code that signs over or announces a specific instance must use it" is correct but easy to miss. Name this exact Platform call site in the Platform-impact migration note (NFR-CMP-01 batch).

**m6 — the FFI views of v3 ProTxs are silently incomplete** (`key-wallet-ffi/src/special_payload.rs:138-163` and the ProUpServ/ProUpReg conversions)
- **Symptom:** for v3, `service_address` reads `"0.0.0.0:0"` and `service_ip` is all zeros. `script_payout` is empty, and payouts, `net_info` and shares aren't exposed.
- **Fix:** expose them, or at least document that v3 fields are absent.

**m7 — constructed states fail silently or panic**
- **Payload dropped without an error:**
  - **Where:** `transaction/mod.rs:252` / `:268`.
  - **What happens:** a `Transaction` with `version < 3` and a typed payload now serializes **without** the payload, while `tx_type()` still reports the type. It decodes back as `ClassicalWithNonStandardVersionTypeBytes`.
  - **Why it's minor:** this matches Core's `HasExtraPayloadField`, and decoding never produces this state. In-tree builders and Platform all use version 3.
  - **The problem:** before this branch the payload was emitted, and now nothing tells the caller it is gone.
- **Panics:**
  - **Where:** `txid()` (`transaction/mod.rs:209`), `instance_hash()` and `base_payload_hash()` (`provider_registration.rs:489`, and the ProUpReg/ProUpServ copies) all `expect()`.
  - **What triggers them:** the v3 encoders now return errors (`net_info`/`payouts` unset, a join-signature count mismatch, more than 255 list entries), so these calls panic. A CbTx v4 with `merkle_root_asset_unlocks: None` triggers it too.
  - **Why it's minor:** decoded values always re-encode, so only constructed values hit this.
- **Fix:** a `debug_assert!`, a fallible `try_txid`, or validate in constructors. At least document the panic.

**m8 — the key-wallet v3 matching covers payout scripts only**
- **Owner keys:** `check_provider_owner_key_in_transaction_for_match` (`account_checker.rs:999`, `:1458`) never looks at the share `owner_key_hash`es of a shared ProRegTx. Core's JSON shows the owner per share, and the top-level `ownerAddress` is absent for shared MNs.
- **Voting and operator keys:** never matched for ProUpSharedRegTx (type 12).
- **Routing:** types 10–12 go only to fund-bearing accounts (`transaction_router/mod.rs:58-61`).
- **Result:** a wallet that holds those keys never records these transactions in `ProviderOwnerKeys`, `ProviderVotingKeys` or `ProviderOperatorKeys`.
- **Status:** fine as a follow-up, but it should be tracked.

**m9 — the transaction builder's fee estimate uses pre-v3 layouts** (`key-wallet/.../transaction_builder.rs:448-520`)
- **Problem:** it assumes ip+port, a single script, no shares or join sigs, no CbTx v4 root and a flat 100 bytes for types 10–12. A v3 ProRegTx with 8 shares is underestimated by more than 1 KB.
- **Fix:** `TransactionPayload::len()` is now exact; use it for typed payloads.

**m10 — gaps in the vectors** (the vectors themselves are good; see item 6)
- **No Core captures** of `qrinfo`/`getqrinfo` or inventory type 34. `p2p_capture.py` only does `getmnlistd`.
- **Address types:**
  - CJDNS isn't covered anywhere.
  - In ProRegTx vectors only IPv4 and IPv6 appear. Tor v3, I2P and domains appear only in the ProUpServTx vectors (`rpc_netinfo@269/270`).
  - The mnlistdiff fixture only has IPv4/IPv6, although `core_v24_vectors.rs:505` says it has Tor/I2P/domain.
- **Asset unlock root:** no block has two or more version 2 unlocks, or mixes version 1 and 2. So the multi-leaf `merkleRootAssetUnlocks` and the version 1 exclusion are checked only by reading the code. Agent B verified them against `evo/cbtx.cpp:157-171` that way.
- **Missing real bytes:** ProUpRevTx v1 has none; CbTx v1 is hand-built only.
- **Adversarial vectors:** none are Core-validated. M1's bytes should become the first.
- **Coverage test:** `core_v24_coverage` only checks (type, version) pairs, not address types or shapes.

### Nits

- **n1:** `CoinbasePayload::CURRENT_VERSION = 3` (`coinbase.rs:50`), and `new()` leaves the v4 root unset; Core requires CbTx v4 after V24. `AssetLockPayload::CURRENT_VERSION = 1` (`asset_lock.rs:50`), while Core has 2.
- **n2:** documentation accuracy.
  - The CHANGELOG says ProTx payloads of version 0 or above 3 "fail to decode, as in Dash Core". In fact Core *parses* a version-only 2-byte payload (`providertx.h:166-171`, `275`, `339`, `408`) and rejects it at validation (`bad-protx-version`). The outcome is the same; the wording isn't.
  - `provider_registration.rs:215` still says "The current version is 0".
  - The `size()` unit test uses version 0, which the decoder rejects.
- **n3:** for version 0 or ≥ 4, the ProUp* encoders write every field, while Core writes only the version. This only affects constructed payloads.
- **n4:** decoding re-encodes every typed payload into a fresh `Vec` to compare it, and `encode_special_payload` buffers again on every serialize/txid. That is fine for correctness, and testnet's 714K blocks decoded quickly. Writing `VarInt(payload.size())` directly would save the second buffer.
- **n5:** the BIP144 compatibility path for type-0, zero-input transactions (`transaction/mod.rs:667`) still has no counterpart in Core. Core deserializes such a tx and rejects it at validation (`bad-txns-vin-empty`); Rust misparses it. This predates the branch and is harmless for SPV, but the node must not inherit it.
- **n6:** the four hand-written bincode impls (CbTx, ProRegTx, ProUpReg, ProUpServ) have to stay in sync with their structs. ProRegTx and ProUpReg have legacy-layout tests; add the same for CbTx v3 and ProUpServ v2.

## Answers to the brief

**1. Byte parity.**
- **Verified field by field** against `SERIALIZE_METHODS`, by me and the two agents, with `size()` checked against encodings across 47 shapes plus a 400K-mutation fuzz with 0 decode-then-re-encode mismatches:
  - the 32-bit `nVersion|nType` header and the "payload iff special version and type ≠ 0" rule (`primitives/transaction.h:356-364`);
  - the length-prefixed `vExtraPayload`;
  - ProRegTx v1–3 (MnNetInfo vs ExtNetInfo selected by version; v3 payouts as a u8 count plus script and u16; shares as a u8 count plus i64, script, script and key20; that many raw 65-byte join sigs; u32/i64 early-exit terms; Evo `platformNodeID` with ports only below v3);
  - ProUpServ/ProUpReg/ProUpRev v1–3;
  - ProDis/ProUpShare/ProUpSharedReg;
  - CbTx v1–4;
  - asset lock and unlock;
  - inventory 34;
  - the 70241/70242 constants;
  - the `getqrinfo` 4096 cap (read-only, as in Core).
- `qrinfo`, `mnlistdiff`, SML entries and `CFinalCommitment` are unchanged between v23 and rc.3.
- **Divergences:** M1, m1, m2 and m3.

**2. "Parse exactly, else raw" vs Core.**
- **What Core does:**
  - It never interprets `vExtraPayload` while deserializing (`primitives/transaction.h:356-364`). A malformed payload never fails `tx`/`block` decoding.
  - Rejection comes later, during validation:
    - **Unknown type on a special version:** `ContextualCheckTransaction` → `bad-txns-type` (`validation.cpp:351-385`). This runs from `ContextualCheckBlock` (`:4563`) during `AcceptBlock` and in the mempool (`:850`).
    - **Known type whose payload fails `GetTxPayload`:** `GetTxPayload` must consume every byte (`evo/specialtx.h:17-28`, `ds.empty()`). The failure is caught in `ConnectBlock` → `ProcessSpecialTxsInBlock` (`:2663`) → `CheckSpecialTxInner` (`evo/specialtxman.cpp:217-273`), and in `RebuildListFromBlock` (`:361` etc.). The block is marked invalid as `BLOCK_CONSENSUS` with `bad-protx-payload`, `bad-cbtx-payload`, `bad-assetunlocktx-payload` and so on, and the mempool rejects the same way (`validation.cpp:1400`).
  - The size cap `MAX_TX_EXTRA_PAYLOAD = 10000` is a `CheckTransaction` rule (`consensus/tx_check.cpp:34`), not a decode rule.
- **The branch therefore matches Core at the decode layer.** A well-framed payload never fails decoding, and keeping it raw preserves the bytes, txid and merkle root. That is safe, and strictly better than before for an SPV/wallet library: previously a malformed or newer payload either failed the block or knocked the stream out of alignment.
- **Divergences that remain** concern only *which* payloads end up typed:
  - (a) the Rust criterion "consumes every byte **and re-encodes identically**" is stricter than `GetTxPayload` in principle. In practice the only real case is M1. Testnet history produced zero raw fallbacks.
  - (b) Rust types a few payloads that Core's `GetTxPayload` fails on (m1, m2, m3).
- **For the future node** (base-sdk types): decoding must stay infallible for well-framed payloads, exactly as here.
  - Validation must reproduce `GetTxPayload` acceptance exactly: Core's lenient NetInfo plus map semantics, BLS malleability, `LIMITED_STRING`, compact-size `MAX_SIZE`, and no re-encode criterion.
  - Rejection must happen at Core's stage with Core's reason: `ContextualCheckBlock` for unknown types, `ConnectBlock` for malformed known types.
  - Rejecting at decode would leave the block unmarked, apply the wrong misbehaviour handling and diverge on re-requests.

**3. Asset unlock v2 stable txid.**
- **`has_stable_txid` (`transaction/mod.rs:228-243`) matches `IsAssetUnlockWithStableTxid`:**
  - special version (signed) and type 9;
  - payload size exactly 145;
  - `payload[0] >= 2`.
- **The rule is context-free, like Core's.** It is judged from the bytes, also for raw type-9 payloads.
- **`txid()` zeroes the last 132 serialized bytes,** exactly as `ComputeStableUnlockHash` does (`transaction.cpp:81-88`).
- **`instance_hash()` equals `GetInstanceHash()`** (`:121-125`).
- **`compute_asset_unlocks_merkle_root` equals `CalcCbTxMerkleRootAssetUnlocks`** (`evo/cbtx.cpp:157-171`): instance hashes of stable unlocks in block order, zero when there are none.
- **`Block::compute_merkle_root` uses the stable txid, as Core's does.**
- **Downstream:** m4 (BIP152) and m5 (Platform's vote extension, plus persisted keys).

**4. i16 version.** The parse matches Core's struct model:
- `nVersion = (int16_t)(n32 & 0xffff)` and `nType = n32 >> 16` (`primitives/transaction.h:356-360`) correspond to `transaction/mod.rs:648-650`.
- The payload is read iff `nVersion >= 3 && nType != 0` (`:362-364`), which corresponds to `is_special_version` (`:705`, signed comparison) and the `Classic` check.
- The Rust field stays `u16` with the same bit pattern, and every special-version predicate in the crate reads it as `i16`. There are no unsigned comparisons of `tx.version` elsewhere in dashcore, key-wallet or dash-spv.

**5. Compatibility of persisted data (bincode/serde) and the FFI ABI.**
- **Bincode:**
  - The new variants of `TransactionPayload` and `TransactionType` are appended, so the old variant indices are unchanged.
  - The hand-written bincode impls keep the derived layout for older payload versions and add the new fields only for v3 or CbTx v4.
  - `serde`: the new `Option` fields default to `None` when missing (JSON), and the ProRegTx share fields carry `#[serde(default)]`.
  - Old persisted bytes therefore keep decoding. Two caveats: legacy tests are missing for CbTx and ProUpServ (n6), and `txid()` has changed for any persisted v2 unlock (m5).
- **FFI:**
  - No layout changes; the views were not extended (m6).
  - Semantic break: M2.
  - `payload_type` can now be 10–12, or any raw type.

**6. Provenance and tests.**
- **The vectors are genuinely Core-produced:**
  - The tooling runs Core's own functional tests against dashd rc.3 and extracts the results via RPC and P2P.
  - All 69 tx hexes decode on an independent rc.3 node to the recorded txid and payload.
  - A private regtest dashd (agent A) agrees.
- **Test quality is high:** each test re-encodes the bytes, checks the txid, size and instance hash, compares the payload fields with Core's JSON, checks the block hash and both merkle roots, and recovers the payload signatures to the signers Core checked. That last check shows the signing hashes match Core for well-formed payloads.
- **Gaps:** m10.

**7. key-wallet performance tests.**
- **Observed:** `test_wallet_recovery_performance` (< 70 ms; measured 83–111 ms) and `test_wallet_serialization_performance` (< 50 ms) failed while the box's load average was 49–65 on 32 vCPUs. The 8 perf tests then passed 2 of 3 times in a row, and with `--test-threads=1`.
- **They are unrelated to this branch:** `performance_tests.rs` is unchanged.
- **Recommendation:**
  - Don't block V24a on them.
  - Keep the measurements, but stop asserting wall-clock time in the default `cargo test` gate. Mark them `#[ignore]` (run with `--ignored` on an idle machine or in a perf job), or put the assertions behind an env var.
  - Longer term, move them to criterion benches. Don't delete them.

## Disposition of the `code-review` skill findings

- **Accepted:**
  - the FFI null pointer → M2;
  - the incomplete FFI v3 view → m6;
  - `expect()` panics → m7;
  - ProUpSharedRegTx / share owner keys → m8;
  - the builder estimate → m9;
  - version < 3 dropping the payload → m7;
  - silent downgrade to `Unknown` → covered by the item 2 analysis, with one correction: its example claims Core ignores padding bits in a commitment bitset. In fact Core throws ("Out-of-range bits set", `serialize.h:470-475`), so the raw fallback is consistent there;
  - double encoding → n4;
  - hand-written bincode → n6.
- **Not discussed here:** one item about the version field falls outside the scope of brief item 4.

## Verdict: CHANGES-REQUIRED

**Must fix in this round:**
- **M1:** match Core's NetInfo decoding and canonical signing hash, and add the Core-accepted regression vectors.
- **M2:** fix the FFI contract or its documentation, and add a test.

**Cheap to fold into the same round:**
- m1 (at least document it), m2, m3 and m4.

**Fine as tracked follow-ups:**
- m5: put it in the Platform migration note now.
- m6 to m10, and the nits.

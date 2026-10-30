# V24a review, round 2 (Opus, focused)

- **Branch:** `node/v24a-dashcore-wire` at `ea2dbf49` (worktree `~/workspace/rdc-wt/v24a-dashcore-wire`), on `node/integration` `a7b32c83`.
- **Range-diff against `backup/v24a-r0` (`a22c7138`):**
  - 7 of the 10 round-1 commits are identical.
  - Commits 1, 2 and 4 differ only by rebase fallout: `Vec::from_hex`/`internals::hex` became `hex_conservative`, plus one moved test helper.
  - 23 commits are new.
- **Reference:** Dash Core `v24.0.0-rc.3`, release binary `dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz` (sha256 `9327f000…ff52`, matches `SHA256SUMS.asc`), unpacked into my scratch.
- **Scope:** I wrote nothing to the branch or the worktree; `git status` stays clean. The probes ran on a separate clone with its own target directory, under `/work/scratch/v24a-r2-opus/`. My regtest dashd ran on port 31790 with its own datadir, and it has been stopped.

**Summary:**
- Every round-1 Opus finding is resolved, documented as the agreed minimum, or (for follow-ups) left open for good reason.
- An independent dashd differential on **1,280 crafted payloads** found **0 divergences**, in either the decode verdict or the signing hash. Run against the round-1 code, the same harness flags **412 of 640**, including 80 signing-hash disagreements.
- A separate live check confirmed the legacy-address hash fix (823cdcc3) on mineable v2 registrations.
- The `CURRENT_VERSION` decision (n1) is sound. Its new doc comment misstates Core's rule (nit N1).

**Verdict: APPROVE-WITH-NITS.**

## What I ran

| Check | Result |
|---|---|
| `cargo test --locked -p dashcore --all-features` | 597 unit (17 ignored, all pre-existing), **8/8 `core_v24_vectors`** (incl. new `core_v24_netinfo`, `core_v24_netinfo_entries`, `core_v24_qrinfo`), 5 + 7 + 12 + 1 integration, 35 doc |
| `cargo test --locked -p dashcore` (default features) | 550 unit + 5 + 8 vector tests pass |
| `cargo test --locked -p key-wallet-ffi --all-features` | 277 pass, incl. the two new raw-payload tests |
| `cargo test --locked -p key-wallet --all-features` | 700 pass, 1 ignored. The timing tests are now gated by `RUN_PERF_ASSERTIONS` (merged on integration, f41300ad), so brief item 7 is closed |
| `cargo test --locked -p dash-spv --lib` | 625 pass |
| `cargo fmt --all -- --check` (on the worktree) | clean |
| `cargo clippy -p dashcore -p key-wallet -p key-wallet-ffi -p dash-spv --all-features --all-targets -- -D warnings` | clean |
| **NetInfo / signing-hash differential vs dashd rc.3**, 2 seeds × 640 variants | **1,280 variants, 0 divergences** (details below) |
| Control: same harness, seed 2, branch at round 1 (`a22c7138`) | **412 / 640 flagged**: 259 parse verdicts, 131 canonical maps, 86 "Core accepts what Rust keeps raw", 80 signing hashes |
| Legacy (v2) ProRegTx with addresses Core holds as empty | 6/6 accepted when signed over the branch's sign string; 6/6 `bad-protx-sig` when signed over the wire-form hash (the old behaviour) |

## The differential

**Setup** (`/work/scratch/v24a-r2-opus/work/{setup,diff,legacy}.py`; the Rust side is `dash/examples/netinfo_probe.rs` in my clone):
- A copy of the round-0 `feature_masternode_payout_shares` node0 chain, with V24 active.
- dashd built and signed four version 3 bases:
  - ProRegTx regular and Evo, external collateral, via `register_prepare[_evo]`, left unsubmitted;
  - ProUpServTx regular and Evo, via `update_service[_evo]` with `submit=false`.
- Only the `netInfo` bytes are rewritten. The probe uses the branch's own types: `deserialize::<Transaction>`, `ExtNetInfo::canonical()`, `base_payload_hash()`, `payload_collateral_string()`, and a BLS verify through `dashcore_crypto`. I wrote the generator and the comparison independently of `craft_netinfo.py`, reusing only its node-start helper.

**Variant families** (randomised; seeds 2 and 3):
- **`clean`:** valid first lists, purposes shuffled, repeated purposes after their first list (junk or valid entries, or empty lists), and sometimes a trailing over-long unknown-network claim.
  - For ProRegTx, 60% of variants also carry *new* valid first lists mixing IPv4, IPv6, Tor v3, I2P (port 0), CJDNS and domains, so Core's hash changes too.
- **`junk_first`:** invalid entries inside the first list. The junk covers:
  - each `CService::IsValid` rejection;
  - BIP155 id 3 and other unknown ids;
  - known ids with the wrong length;
  - non-canonical lengths, and lengths above 512;
  - each `DomainPort::Validate` failure, including non-ASCII, NUL, a 64-character label and an empty domain;
  - the 253-byte domain.
- **`map`:**
  - unknown purposes, an empty map, empty first lists, more than 4 entries;
  - unknown ExtNetInfo versions (0, 2, 255);
  - a non-canonical purpose count, an entry count, or both; a purpose count beyond or short of the lists; a count over `MAX_SIZE`;
  - a domain of 254 bytes, or a domain that claims more bytes than remain.
- **`mutate`:** 1 to 3 random byte overwrites, insertions or deletions in the NetInfo bytes.
- **`sigbreak`:** ProUpServTx only. The first valid entry is changed while Core's BLS signature is kept.

**Compared, per variant:**
1. **Parse verdict:** Core's `decoderawtransaction` has `proRegTx`/`proUpServTx` ⇔ the branch decodes the payload typed. Otherwise it must be `UnknownPayloadType(type, bytes)`. The txid and a byte-identical re-encode are checked too.
2. **Canonical map:** Core's `addresses` JSON equals the branch's `canonical()`, rendered with Core's `ToStringAddrPort`: RFC 5952 IPv6, Tor v3 checksum, `.b32.i2p`, and `[invalid entry]`.
3. **Field alignment:** `inputsHash` and `pubKeyOperator`/`proTxHash` match, so both sides read the bytes after NetInfo the same way.
4. **Signing hash:**
   - ProRegTx variants are signed over the branch's `payload_collateral_string()`. Core answering `bad-protx-sig` would mean the payload hash differs.
   - ProUpServTx variants keep Core's original BLS signature. The branch's verify over its own `base_payload_hash()` must agree with Core's verdict: allowed ⇒ verifies; `bad-protx-sig` ⇒ doesn't.

**Results (1,280 variants, 0 divergences):**

| Core verdict | Count | Branch |
|---|---|---|
| `bad-protx-payload` (Core's `GetTxPayload` fails) | 322 | raw in every case |
| typed, refused at validation (`bad-protx-netinfo-{bad,empty,entry,maxlimit,port}`, `bad-protx-dup-netinfo-entry`) | 418 | typed; canonical map identical to Core's JSON |
| typed, **signature check reached** | **540** | — |
| ↳ accepted | 468 | typed; 279 of them are ProRegTx signed over the branch's own sign string (exact hash equality) |
| ↳ `bad-protx-sig` (ProUpServTx with a changed canonical form) | 72 | BLS verify over the branch's hash also fails, in all 72 |

**What the coverage shows:**
- 830 typed variants have a canonical form that differs from their wire form.
- 463 of them reached the signature check, and 417 were accepted. So "hash the map Core holds, not the bytes" is exercised heavily, not incidentally.
- The `CoreStream` re-read path (a claim longer than the bytes left, after which Core goes on parsing from just after the compact size) appears in about 30% of the `clean` variants.

**Legacy addresses (823cdcc3):**
- **Why this path matters:** a ProRegTx v2 (BasicBLS) is still allowed after V24, and `CheckProviderNetworkFields` allows an empty `MnNetInfo` for registrations. So an address Core holds as empty (`0.0.0.0`, `255.255.255.255`, `::`, `2001:db8::/32`, Tor v2-in-IPv6, internal-in-IPv6) makes a valid, mineable registration, and its collateral signature covers `CService{}`.
- **Result:** dashd accepted each of these six signed over the branch's string, and rejected each one signed over the wire-form hash with `bad-protx-sig`.
- **What that means:** before this round (and before this branch), dashcore produced and checked the wrong sign string for such registrations. The fix is necessary and correct.

## Round-1 Opus findings: status

| # | Finding | Status | Evidence |
|---|---|---|---|
| M1 | NetInfo stricter than Core; signing hash over the wire form | **Resolved** | 18417686, 19fc6a1d, fdf4e136. Decoding mirrors `NetInfoEntry::Unserialize` / `UnserializeV2Stream` / `SetNetFromBIP155Network` / `DataStream` (read and ignore consume nothing on failure); `Malformed(bytes)` and `UnknownType(u8)` re-encode identically; `canonical()` = `std::map` first-wins and sorted, with invalid entries as `0xff`. I compared it line by line with rc.3 `netinfo.{h,cpp}`, `netaddress.{h,cpp}`, `streams.h`, and confirmed it with the differential above. Agent A's round-1 bytes are in `core_v24_netinfo.json` / `core_v24_netinfo_entries` |
| M2 | FFI tag says provider type but pointer NULL for raw payloads | **Resolved** | 7520d3ec: `FFI_SPECIAL_PAYLOAD_TYPE_RAW = 0xFFFF` for `UnknownPayloadType` and `ClassicalWithNonStandardVersionTypeBytes`, contract docs updated, tests via the converter and through `FFITransactionRecord`. The CHANGELOG marks it **Breaking** |
| m1 | BLS canonicity not checked | **Documented** (the agreed minimum) | 86b3e427: `TransactionType::consensus_decode` docs and CHANGELOG |
| m2 | no 253-byte domain cap | **Resolved** | `decode_domain` rejects a length over 253 before allocating; dashd agrees (`map` family) |
| m3 | `bestCLHeightDiff` over `MAX_SIZE` | **Resolved** | bf3184af, with a boundary test (0x02000000 ok, +1 fails) |
| m4 | BIP152 short IDs from `txid()` | **Resolved** | 9044d662: `instance_hash()` for version 1 compact blocks, with a v2-unlock test |
| m5 | Platform vote extension must move to `instance_hash()` | **Resolved** | CHANGELOG "Platform impact" names the call site; `instance_hash()` docs say signers must use it |
| m6 | FFI v3 views incomplete | **Documented** | struct docs say v3 addresses, payouts and shares aren't exposed. Exposing them remains a follow-up, not tracked (N3) |
| m7 | constructed states: silent payload drop, `expect` panics | **Documented** | `special_transaction_payload` docs (`HasExtraPayloadField`), `# Panics` on `txid`, `instance_hash`, `base_payload_hash`. Acceptable |
| m8 | key-wallet ignores share owner keys, ProUpSharedRegTx voting/operator keys | **Open (follow-up)** | not tracked in `BACKLOG.md` (N3) |
| m9 | builder fee estimate uses pre-v3 layouts | **Resolved** | 5523fa79, 798fec98: `TransactionPayload::size()` plus placeholder signature allowances; `size()` is arithmetic, so there's no panic path; tests check the estimate is identical before and after signing |
| m10 | vector gaps | **Mostly resolved** | e9a25280 requires every layout and address kind (CJDNS included) plus v3 SML IPv6/Tor/I2P/domain; 7b3e8602 adds 4 Core `qrinfo` captures. **Still missing:** a block with ≥2 v2 unlocks or mixed v1/v2 (only blocks 969 and 970 have one v2 unlock each), and a Core capture of inventory 34 (N3) |
| n1 | `CURRENT_VERSION` 3 (CbTx) / 1 (asset lock) | **Rejection accepted; doc wrong** | see below and N1 |
| n2 | doc accuracy | **Resolved** | CHANGELOG now says Core extracts version-only payloads and refuses them at validation; `provider_registration.rs` doc fixed; the `size()` test uses version 1 |
| n3 | ProUp* encoders for version 0/≥4 write all fields | unchanged (nit, constructed payloads only) | — |
| n4 | double buffering on decode and serialize | unchanged (nit) | — |
| n5 | BIP144 for type-0 zero-input transactions | unchanged (pre-existing; the node must not inherit it) | — |
| n6 | legacy bincode tests for CbTx v3 / ProUpServ v2 | **Resolved** | 2035ed79. The appended `NetInfoEntry::{Malformed,UnknownType}` and `NetInfoPurpose::Unknown` keep the existing bincode variant indices, so persisted SML data still decodes |

The GPT round-1 items I touched on along the way are also fixed:
- size and weight with witnesses on special transactions (75217d6d; it now matches the encoder's rule);
- regeneration failing open (e94d7798);
- address normalisation and the domain cap (part of M1/m2).

## n1: the `CURRENT_VERSION` decision

**What the branch did:** it keeps `CoinbasePayload::CURRENT_VERSION = 3` and `AssetLockPayload::CURRENT_VERSION = 1`, and rewrites their docs. CbTx: "The version `new` builds, which every network accepts. Dash Core v24 builds version 4 … once V24 is active." Asset lock: "… which every network accepts. Dash Core v24 also accepts version 2 once V24 is active."

**What Core does (rc.3):**
- **Asset lock:** `CheckAssetLockTx` (`assetlocktx.cpp:56-61`) accepts 1..`CURRENT_VERSION` (2), and allows 2 only with V24. v23.1.8 accepts only 1. So version 1 really is accepted everywhere, and the doc is correct.
- **CbTx:** `CheckCbTx` (`cbtx.cpp:35-51`) requires **exactly one** version per deployment state:
  - ≥ 2 after DIP0008;
  - `== 3` iff V20 is active (`!isV20 && nVersion >= 3` is refused);
  - `>= 4` iff V20 and V24 are both active (`!requires_unlock_root && nVersion >= 4` is refused).
- **Deployment state today:** V24 is `NEVER_ACTIVE` on mainnet in rc.3, and starts on testnet from 2026-09-24 (`chainparams.cpp:422`). The testnet oracle, still syncing at height 1,107,974, reports V24 `defined`.

**Assessment:**
- Not bumping is the right call. No constant fits every network, and 4 would be refused on mainnet today.
- But "which every network accepts" is false for CbTx: version 3 is refused on any chain before V20 (fresh regtest or devnets) and on any chain once V24 activates, which testnet is about to do. See N1.

## Findings (round 2)

### N1 — nit: `CoinbasePayload::CURRENT_VERSION` doc misstates Core's rule

- **Where:** `dash/src/blockdata/transaction/special_transaction/coinbase.rs:51-53`.
- **Scenario:** a caller building a regtest or devnet coinbase before V20, or a testnet or mainnet coinbase after V24, trusts "every network accepts" and uses `new()`. Core refuses it with `bad-cbtx-version`.
- **Fix:** document the rule instead. Something like:
  > "Dash Core requires exactly one CbTx version per deployment state: 2 before V20, 3 while V20 is active and V24 isn't (mainnet, and testnet until V24), 4 with the asset unlock root once both are active. `new` builds 3; set `version` and `merkle_root_asset_unlocks` for other chains."

### N2 — nit: the regeneration version check matches a substring

- **Where:** `dash/contrib/core-vectors/run-functional-tests.sh:19-23` (`case "$version" in *"${tag#v}"*`).
- **Scenario:** with tag `v24.0.0` and an rc.3 binary, `"Dash Core version v24.0.0-rc.3"` contains `24.0.0`, so the run proceeds and the label is wrong. (`extract.py` records the real `dashd -version`, so the fixture itself stays truthful.)
- **Fix:** compare `"Dash Core version $tag"` exactly against the first line.

### N3 — minor (process): accepted follow-ups aren't tracked

- **What:** m8 (key-wallet share owner keys; ProUpSharedRegTx voting and operator keys; routing of types 10–12), m6 (FFI v3 views) and the m10 residuals (multi-leaf or mixed-version `merkleRootAssetUnlocks` block vectors, a Core capture of inventory 34) appear in no `BACKLOG.md` row.
- **Why it matters:** round 1 accepted them as follow-ups on the condition that they are tracked.
- **Fix:** add `BACKLOG.md` rows (manager action; nothing to change in the branch).

### Out of scope, pre-existing (no action for V24a)

- **`MasternodeNetInfo::primary_service_address`** (`sml/masternode_list_entry/mod.rs:104-125`, from #797):
  - **What it does:** returns the first IPv4/IPv6 entry of the CORE_P2P list.
  - **What Core does:** `ExtNetInfo::GetPrimary` returns `entries[0]` whatever its network.
  - **Where they differ:** for a valid v3 masternode that lists a Tor or I2P entry first, Rust returns the later IPv4 address, while its doc says those yield `None`. This isn't consensus. Worth a doc fix whenever that code is next touched.
- **Allocation:** each junk service entry allocates a small `Vec` (`Malformed`), and every service read allocates a temporary buffer. The memory amplification per wire byte is in the same range as Core's `NetInfoEntry` objects, and payloads are bounded by the transaction size, so I don't consider it a finding.

## Answers to the round-2 asks

1. **Round-1 Opus findings:**
   - M1, M2, m2, m3, m4, m5, m9 and n6 are resolved.
   - m1, m6 and m7 are documented, which was the agreed minimum.
   - m8 and the m10 residuals remain follow-ups, still to be tracked (N3).
   - n3, n4 and n5 are unchanged nits.
2. **NetInfo decode and signing hash vs a v24 regtest dashd:**
   - 1,280 crafted payloads (≥ 500 required), on all four ProTx kinds that carry ExtNetInfo, with **0 divergences** in parse verdict, canonical map, field alignment or signing hash.
   - 540 of them reached Core's signature check, and 279 ProRegTx were accepted with a signature over the branch's own sign string.
   - The same harness flags 412/640 against the round-1 code, so it has teeth.
   - The legacy-address canonicalisation was also confirmed live (6/6, plus 6/6 controls).
3. **Rejection of the `CURRENT_VERSION` bump:** correct to keep 3 and 1. The asset lock doc is accurate; the CbTx doc's "every network accepts" isn't (N1, nit).

## Verdict: APPROVE-WITH-NITS

No blockers and no majors. N1 and N2 are small doc and tooling fixes. N3 is a backlog action for the manager.

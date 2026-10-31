# SB7 Opus-high review (DEC-92, after Sol r1–r4 APPROVE)

Reviewed `node/sb7-sml-core` at `692f91c291a5bbaaa1f26b606f99821858c32d43`, range
`8eae79676..692f91c29` (merge-base with `node/integration`), in a detached worktree
`/work/scratch/sb7-opus-high/wt`. Core reference: `~/workspace/dash` at `v24.0.0-rc.3`
(0364ed626e, the oracles' `/Dash Core:24.0.0/`). base-sdk at the pinned `dbe245f7e`. The
implementer's worktree and branch were not touched.

## Findings

No Blocker or Major findings. The runtime code matches Core v24 for everything SB7 computes.
The four Minor findings are about test evidence and one program rule. None of them shows a
consensus divergence.

### Minor

**m1: the new bench builds dash-evo with `test-utils` on (PROCESS.md Gates, "Benches measure production only").**
`dash-evo/Cargo.toml` already had the self dev-dependency `dash-evo = { path = ".", features = ["test-utils"] }`. SB7 adds the crate's first bench (`benches/sml_root.rs`) and five PF0 rows. I verified this with `cargo bench -p dash-evo --bench sml_root --no-run --message-format=json`: the `dash_evo` lib artifact linked into the bench has `features = ["test-utils"]`. Today the feature only adds `testkit`, so the measured SML code is unchanged. Still, the rule (2026-10-11) forbids this shape, and PF0 is about to anchor these rows. Fix:
- drop the self dev-dep;
- give `tests/consumers.rs` (the only `testkit` user) `required-features = ["test-utils"]`, with an explicit gate line `cargo test -p dash-evo --features test-utils --test consumers`;
- add the bench canary `#[cfg(all(feature = "test-utils", not(debug_assertions)))] compile_error!(…)`.

**m2: no regular test pins proTxHash memcmp order.**
Mutant M04 swaps `merkle_root_mn_list`'s sort to numeric order (reversed bytes) instead of memcmp. It survived every non-ignored test in dash-evo and dashcore.
- Core's `simplifiedmns_merkleroots` vector uses proTxHashes with only byte 0 set, where memcmp and numeric order agree.
- The unit tests use `[x; 32]`, which has the same property.
- The dashcore adapters pass leaves that dashcore has already sorted.

`SmlList`'s own order (the derived `Ord` of `ProTxHash` over base-sdk `Hash256`) is likewise pinned only by the ignored history acceptance. I confirmed that the acceptance does kill it: on the 6-block mainnet control it fails with "full vs cached at 2089995". Add a tracked vector where the two orders disagree. Either a small real list with its CbTx root (one testnet height from the dumps, about 500 rows), or two or three entries whose proTxHashes order differently by first and last byte, checked against `compute_merkle_root` over hand-ordered leaves.

**m3: the v2 boundary of `check_quorums_root` is untested.**
M14 (`version < 2` → `version <= 2`) survived. `cbtx_root_checks` covers v1 (skipped) and v3 (computed) but not v2. The v2 fixture (testnet 900,000) is already in `cbtx_testnet.json`, so add the v2 case.

**m4: the `merkleRootQuorums` assembly has no Core-derived vector.**
The newest-first pop of the oldest commitment, rotated-by-index replacement, the indexed→plain merge and the excess count are checked only by the synthetic `quorums_root_assembly`. M08, M09 and M10 are killed there and nowhere else. The MN root, by contrast, is pinned by Core vectors and roughly 2.6M real CbTx roots. The code does match `CalcCbTxMerkleRootQuorums` (`evo/cbtx.cpp:65-160` at the pin) line for line; I re-derived it below. Either add one vector (a regtest or testnet block's `GetQcHashes` input plus its QCs, against the CbTx `merkleRootQuorums`), or record in STATUS that SB11/EV4's end-to-end connect is where this gets real-data coverage.

### Nits

- **n1:** v3 (ExtAddr) entry hashing has no oracle vector in dash-evo itself. M01 (Evo platform port written at v3) and M18 (domain port little-endian) are killed only by dashcore's `core_v24_vectors`, through the adapter. Port a few v24 regtest v3 entries (Evo, domain, IPv6/Tor/I2P) into dash-evo's tests so the coverage survives the adapters' retirement.
- **n2:** `write_ext_net_info` writes `NIEntry::Service` as given. Core's `NetInfoEntry::Serialize` writes `Invalid` for a service or domain that is not `IsValid()`. The documented contract is that `SmlEntry.net_info` is already canonical (base-sdk decoders and the dashcore adapter make it so), but `SmlEntry` is public. Consider canonicalizing here, or at least a `debug_assert`, so EV4-serve's projection can't hash a form Core never writes.
- **n3:** CbTx v4 (`MERKLE_ROOT_ASSETUNLOCKS`, `merkleRootAssetUnlocks`) exists at the v24 pin. base-sdk decodes it and `nonnull_coinbase_chainlock` treats v4 like v3 (correct). `CalcCbTxMerkleRootAssetUnlocks` is outside SB7; make sure a PLAN row owns it.
- **n4:** dashcore's root adapters still give `None` for an empty quorum set, where Core's root is zero. So `has_valid_llmq_list_root` fails for a v2+ CbTx committing an empty set. This is pre-existing and deliberately kept; record it so the #1117 work doesn't inherit it unknowingly.
- **n5:** the commitment hash is a bare `[u8; 32]` caller input. A newtype for "SerializeHash of the CFinalCommitment" would stop a caller passing the txid or the sign hash.

### Simplification opportunities (not applied)

- `SmlList::from_entries`: a stable sort plus `dedup_by` that keeps the last entry would replace the `BTreeMap` round trip.
- `MasternodeList::scores_for_quorum_for_masternodes` still returns a score-keyed map that drops ties. No member-selection caller uses it now; deprecate it.
- dashcore's `to_sml_entry` round-trips net info through bytes (dashcore encode → base-sdk decode) on every `calculate_entry_hash`. That's fine for an adapter, but it's per-hash work SPV now does on every diff.
- `tests/cbtx_root_history.rs` is 1,178 lines. Moving the `strictness` module and the `Chain` binding into `tests/cbtx_root_history/` submodules would make the acceptance path easier to audit.

## Bit-identity with Core v24 (checked against the rc.3 sources)

- **Entry and hash** (`evo/simplifiedmns.h:72-96`, `simplifiedmns.cpp:46`):
  - field order, with `nVersion` excluded under `SER_GETHASH`;
  - `nType` as u16 LE from version 2;
  - Evo `platformHTTPPort` LE only below v3, then `platformNodeID`;
  - legacy `CService` port big-endian;
  - ExtNetInfo version byte, with nothing after it for version 0 or above 1 (`netinfo.h:411-418`);
  - ADDRv2 entries and the domain port big-endian.

  The operator key is hashed as the entry's bytes, written in the entry's scheme (`CBLSLazyPublicKeyVersionWrapper`). Core's `simplifiedmns_merkleroots` vector passes. M01, M02, M03 and M18 are killed by Core data.
- **List root:** Core sorts by `uint256::Compare` (memcmp, `simplifiedmns.cpp:73-79`), then `ComputeMerkleRoot`. dash-evo does the same; `ProTxHash` documents memcmp `Ord`. Mutation is checked only on real sibling pairs, before odd duplication (CVE-2012-2459), as in Core. The empty list gives the zero root. Core's process-wide `cached_sml` short-circuit (`simplifiedmns.cpp:110-121`) returns the same root and reason class for an equal list, so not modelling it is correct.
- **CHAMP order:** nothing SB7 computes depends on iteration order. The MN root sorts by memcmp and the quorum root sorts its final hashes. `CalculateQuorum`'s comparator is a strict total order over distinct masternodes (score, then a unique collateral), so Core's unstable `std::sort` over `ForEachMNShared`'s CHAMP order gives one result. CHAMP order matters for mnlistdiff bytes (EV4-serve), not here.
- **Quorum root** (`evo/cbtx.cpp:65-160`):
  - block order from `vtx[1]`;
  - bad payload, then null skip, then unknown type, in that order;
  - rotated types set by quorum index;
  - non-rotated types pop the last (oldest) entry when full, then push;
  - indexed hashes merged after the plain ones;
  - the per-type excess check;
  - byte sort, then `mutated` rejection.

  The `u8` map key mirrors `std::map<LLMQType>` ordering, and only the order of the excess error depends on it.
- **Scores** (`llmq/utils.cpp:72-186`): single SHA-256 of `confirmedHashWithProRegTxHash‖modifier`, compared as `arith_uint256` (last byte most significant). Filters are banned / unconfirmed / Evo-only. The sort is descending (score, collateral) via `std::sort(rbegin, rend)` with an ascending comparator, which dash-evo reproduces with `Reverse((score, (txid bytes, n)))`. Truncation keeps that order.
- **Partial merkle tree** (`merkleblock.cpp:112-198`): the checks are `nTransactions == 0`, more than `MaxBlockSize() / 60` (= 2,000,000 / 60, DIP1 default), more hashes than transactions, and fewer bits than hashes. The traversal, the identical-subtrees check (with an implicit odd sibling still allowed) and the whole-byte and whole-hash consumption checks are identical. `None` stands in for Core's zero root. `verify_cbtx_proof`'s requirement of exactly one match `(coinbase txid, 0)` is stricter than `ExtractMatches`, as Core's own `BuildSimplifiedMNListDiff` tree matches only the coinbase.
- **CbTx gating:** `check_quorums_root` runs only from version 2 and only after the MN check (`specialtxman.cpp:928-952`). `nonnull_coinbase_chainlock` (`cbtx.cpp:181-210`) applies:
  - V20 active at the block;
  - coinbase with `TRANSACTION_COINBASE` type;
  - a complete payload (`GetTxPayload` requires an empty stream);
  - version ≥ 3, with height equality;
  - `IsValid()` under the global scheme flag;
  - `diff < height`.

  The `COMPACTSIZE` range check matches base-sdk's `decode_size`.

## Implementer's deviations

| Deviation | Verdict |
|---|---|
| BLS `quorum_entry` validation and `verify_message` stay in dashcore until SB8 | Accept. This agrees with DEC-109's single shared commitment verifier. |
| Commitment `SerializeHash` is a caller input | Accept (see n5). |
| Coinbase-CL predicate without `rules`, `CoinbaseFacts` = `TxRef` or `NoTransactions` | Accept. It is v24-only and keeps every fact Core reads; `BlockDataUnavailable` covers `ReadBlockFromDisk` failure. |
| `QcHashes` keyed by `u8` | Accept. `LLMQType` is a `uint8_t` enum and `std::map` orders it numerically. |
| Quorum-score tie-break | **Unreachable, not a divergence.** Since bfd282914 `calculate_quorum` follows Core's (score, collateral) descending order exactly when a collateral is given. The SML has no collateral, so tied entries keep input order. A tie needs `sha256(c_a‖m) = sha256(c_b‖m)` with `c_a ≠ c_b` (where `c = sha256(proTx‖confirmed)` and proTxHashes are unique in a list), which is a SHA-256 collision. M16 is killed. |
| dashcore root adapters give `None` for a mutated tree | Accept. It matches Core's rejection, and the only callers (`builder.rs:54-55`) store it as "no valid root". |
| Unknown active quorum type → `excess-quorums` where Core asserts | Accept. This input is impossible on a connected chain (Sol r1 also notes an empty unknown bucket passes). Callers must not treat it as a peer fault. |
| dashcore always depends on dash-evo and dash-primitives | Accept. There is one `secp256k1` (0.33.1) in the lock and dashcore enables no dash-evo features. Sol r1 accepted the +61,368 B Android delta. |

## Incremental rehash

**Argument.** After a root is taken, `levels[l]` is Core's level `l` without padding, and `equal_pairs` holds exactly the real equal sibling pairs.
- An insert or remove at `i` moves only leaves `≥ i`. A replace changes only `i`. So with `s = min(first move, first replace)`, every leaf `< s` is unchanged in value and position, and `s ≤` the old length.
- A parent `p < ⌊s/2⌋` has both children `< s`. Each child is real in the old tree (`2p+1 ≤ s−1 ≤ L_old−1`), so the parent's value and its equality flag are unchanged. The first possibly stale parent is `⌊s/2⌋`, and the same holds level by level.
- `rebuild_from` recomputes exactly from there. It drops level-`l` flags at or after that pair, and drops flags above a lowered top. The replace-only path keeps the tree's shape and recomputes exactly the changed leaves' ancestors and their pair flags.

**Exhaustive check** (scratch-only test in `root.rs`, never committed). It compares against an independent port of Core's `ComputeMerkleRoot`, both the root and the `mutated` flag:
- every leaf vector over a 2-letter alphabet up to 9 leaves (so equal siblings are common), with every single insert, remove and replace;
- up to 6 leaves, every ordered pair of operations, both with and without a root between them.

That is 251,172 cases. A further 3,000 random sequences run to about 300 leaves with 1–6 batched edits between roots. Everything passed. All four cache mutants (M05, M06, M07, M19) are killed by the branch's own tests and by this one.

## Mutation campaign (scratch copy, restored after)

The suites were the dash-evo lib tests plus `sml_core_vectors`, `cbtx` and the non-ignored `cbtx_root_history`, and, for entry or score mutants, dashcore `--lib sml::` plus `core_v24_vectors` and `core_mnlistdiff_ground_truth`.

| # | Mutant | Result |
|---|---|---|
| M01 | Evo platform port also written at v3 | killed (dashcore `core_v24_mnlistdiffs` only) |
| M02 | legacy service port little-endian | killed (Core vector, dashcore lib + ground truth) |
| M03 | `nType` written from v1 | killed (Core vector, dashcore) |
| M04 | full-root sort numeric, not memcmp | **survived** (non-ignored); killed by real-data acceptance (m2) |
| M05 | `rebuild_from` keeps one stale parent | killed |
| M06 | replace path never clears an equal pair | killed |
| M07 | stale flags above a lowered top kept | killed |
| M08 | non-rotated pop when `len > count` | killed (`quorums_root_assembly` only) |
| M09 | pop the newest instead of the oldest | killed (`quorums_root_assembly` only) |
| M10 | excess check off by one | killed (`quorums_root_assembly` only) |
| M11 | no identical-subtree rejection | killed (`pmt_malleability`) |
| M12 | whole unused flag byte accepted | killed (`coinbase_proof_failures`) |
| M13 | `diff > height` | killed (`nonnull_coinbase_chainlock_cases`) |
| M14 | quorum root gated `<= 2` | **survived** (m3) |
| M15 | trailing payload bytes accepted | killed |
| M16 | ties by ascending collateral | killed (`ties_go_by_collateral_as_core_sorts_them`) |
| M17 | score ordered by memcmp | killed (`score_orders_as_arith_uint256`) |
| M18 | domain port little-endian | killed (dashcore `core_v24_mnlistdiffs` only) |
| M19 | first equal pair of a rebuilt level not rechecked | killed |
| M20 | Evo-only filter ignored | killed (`quorum_filters_and_cuts_like_core`) |

That is 18 of 20 killed. The two survivors are m2 and m3.

## Do the tests prove what they claim?

- **History acceptance:** the expected root is the `merkleRootMNList` of a coinbase that the run proves:
  - with a partial merkle tree, to the header's merkle root;
  - with that header's X11 hash, equal to the record's hash, linked to the previous record across files;
  - against Core's checkpoints (read from dash-params) and the manifest at every anchor, with END itself an anchor;
  - with the proof stream read to EOF.

  The expected values are not taken from the code under test. The one in-loop use of tested code is `verify_cbtx_proof`, which is pinned independently: Core's `pmt_tests` ports use their own tree builder and `compute_merkle_root`, and Core's `gettxoutproof` fixtures are included. I re-queried all four manifest anchors with read-only `getblockhash` on the oracles; they match.
- **Independent replay at 692f91c29, my build:** strict mainnet 1,028,160..=2,090,000 gave 1,061,841 roots, 213 full recomputes and 0 mismatches (429 s). The 6-block mainnet control at 2,089,995..=2,090,000 passes. Sol r1 replayed testnet 7,000..=1,569,775 independently; I did not repeat it.
- **`sml_core_vectors`:** verbatim from Core's `simplifiedmns_merkleroots` at rc.3 (15 hashes and the root). `cbtx_testnet.json` is from the oracle. The dashcore ground-truth tests use Core-computed goldens.

## Production code free of test-only features

dashcore's normal dependency on dash-evo enables no features, and dash-dml forwards `test-utils` only through its own feature. The only breach is m1 (bench builds).

## Validation

Builds used `RUSTC_WRAPPER=`, `nice -n 10`, `CARGO_BUILD_JOBS=12`, target and `TMPDIR` under `/work/scratch/sb7-opus-high` (seeded by reflink from the implementer's release cache), and `--locked`.

| Run | Result |
|---|---|
| dash-evo lib | 15 passed |
| `sml_core_vectors` | 1 passed |
| `cbtx` | 6 passed |
| `cbtx_root_history` | 31 passed, 1 ignored |
| dashcore `sml::` | 52 passed |
| `core_v24_vectors` | 5 passed |
| `core_mnlistdiff_ground_truth` | 8 passed |

Logs and mutant outputs are in `/work/scratch/sb7-opus-high/mut/`. Mainnet RPC was read-only and capped at 2,090,000. Nothing under `/work/chains` or `/work/bench` was written. There was no commit or push. This report is uncommitted.

**Verdict:** APPROVE-WITH-NITS

The four Minors (bench rule, the memcmp-order vector, the v2 gate case, and a quorum-root vector or a recorded deferral) should land before PF0 anchors the new rows and before the merge. None of them blocks correctness.

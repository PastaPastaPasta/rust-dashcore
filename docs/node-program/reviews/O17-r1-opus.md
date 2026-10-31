# O17 PIN-DELTA: second-Opus review

| | |
|---|---|
| Reviewer | independent second Opus (PLAN O17 acceptance: "reviewed by a second Opus") |
| Date | 2026-10-08 |
| Under review | `designs/O17-pin-delta.md`, `designs/O17-release-profile.md`, `designs/O17-pin-delta/*.md` (nine packets), `designs/O17-pin-delta/decisions.json` |
| Context read | A1 §2.2, §2.3, §2.25; CS0 §5.1, §8.2, §8.4; DECISIONS DEC-02, DEC-05, DEC-24; PLAN O17 row and the task rows of every profile consumer |
| Source | `~/workspace/dash`, read-only, `git show/grep/diff/log` at `v23.1.8` (728f505583) and `v24.0.0-rc.3` (0364ed626e) |

## Verdict: APPROVE-WITH-CHANGES

The audit is strong. Of the 36 rows I checked against tag-to-tag source, every code-level claim holds; only some framing does not (below). The index counts, the home counts in §0, the duplicate merges and the generated §10 table are internally consistent (checked mechanically: 282 packet rows = 217 index rows + 65 duplicates, none lost; class × keying totals and home totals equal §0). The A1 refutations (row 3, row 26, rows 14/15, row 9 corrections, inv type 34, notfound 116/50 000) are correct.

No blocker. Four major findings need fixing before the manager approves §2 and before task briefs quote §10:
1. the SPORK_3 instruction is wrong, and following it would split consensus on mainnet;
2. the keying policies do not cover about a third of the `v24-all` rows, and two `v24-all` rows break the binding interop rule;
3. the per-task table breaks the DEC-24 D3 layering for about 30 (task, field) pairs;
4. the declared-deviation rows do not name the verification list that must record them.

Counts: blocker 0 · major 4 · minor 11 · nit 10.

## Findings

### Major

**M1. "Pin mainnet's live SPORK_3 value" is wrong. Both pins hard-code every mainnet spork.**
- Evidence: `CSporkManager::GetSporkValue` in v23 `spork.cpp:265-275` and v24 `spork.cpp:171-181`, "Harden all sporks on Mainnet": `!IsTestChain()` returns 0 for every spork and 1 for SPORK_21. So under v23.1.8 on mainnet SPORK_3 is always active (`0 < GetAdjustedTime()`, v23 `spork.cpp:256`), and `RejectConflictingBlocks()` (v23 `instantsend.cpp:819-828`) reduces to `IsBlockchainSynced()`.
- The mn-overlay packet states this correctly (MNO-01/02), and PLAN SP0 already says "mainnet hard-coded values". The wrong version appears in four places:
  - report §3 row 6 ("SP0 must pin mainnet's live SPORK_3 value");
  - report §6.2 SP0 bullet ("which SIG-12/-13 depend on; the compiled default is off");
  - spec §11 item 2;
  - llmq-sig Notes ("If SPORK_3 is off on mainnet, v23 never rejects IS-conflicting blocks" cannot happen).
- Risk: an SP0 or CH3a implementer who "pins" a live or compiled value of off would make a v23-profile rdashd connect blocks that v23.1.8 rejects with `conflict-tx-lock` (BLOCK_CHAINLOCK). That is a mainnet consensus split.
- Fix: replace those passages with "SP0 implements Core's mainnet hard-coding (all sporks 0, SPORK_21 = 1) under both profiles. The SPORK_3 value matters only off mainnet: the v23.1 DTF sets it to 0, and `feature_mnehf.py:91` disables it." Note in the `IsBlockGate::SyncedAndSpork3` doc comment that on mainnet it equals "blockchain-synced". SIG-13's CL-signer half is then unobservable on mainnet (`TrySignChainTip` already returns when not synced, v23 `chainlock/signing.cpp:83`); it is needed only for O11.

**M2. The §2 policies do not cover the `v24-all` set, and two `v24-all` rows violate the binding interop rule.**
- (a) Policy H is defined as "only reachable with input no honest peer or client produces". Many `v24-all` rows, including some §2 lists under H, are reachable by honest input. Their real rationale is an unstated fifth policy: "v24 fixes a v23 defect, race or crash; v24's outcome is safer or a possible v23 outcome, and no honest party can depend on v23's".
  - **RPC-10** (listed under H): v23 checks a whitelisted user's batch against the envelope (`whitelisted(jreq)`, v23 `httprpc.cpp:256`, empty method), so every batch from a whitelisted user gets 403. v24 checks each element. Honest dashmate/Platform users see the difference. v23 fails closed, so emulating it would not re-create a weakness.
  - **WAL-08** (listed under H): its packet says "an honest v23 client can trigger it".
  - **MPIX-16 / SIG-06**: honest v23 peers at a window edge trigger it (both packets).
  - Also: SIG-14, DKG-05, DKG-06, DKG-13, MPIX-21, WAL-31, WAL-35, WAL-36, CONS-11, MNO-08, MNO-12, MNO-21, P2P-31, RPC-08, RPC-09, RPC-17.
  - The boundary is also inconsistent: SIG-17 (`profile`) has the same shape as SIG-14 and DKG-06 (`v24-all`): a benign change honest MNs can observe, with no scoring involved.
  - Approving §2 as written approves only H and R, not these rows.
- (b) The interop rule ("mis-score, disconnect, mark bad or **reject** something an honest v23.1.8 node sends") is broken by two rows:
  - **MNO-07.** v23 caches a vote for an unknown parent with a WARNING and no penalty (v23 `governance.cpp:812-821`). v24 answers PERMANENT_ERROR +20 when `IsValidForUnknownParent` fails. The report admits that honest peers trigger this "in a key or MN-list race". That is exactly the DKG-01 model case, on a smaller scale.
  - **MPIX-12.** v24 rejects (`protx-dup`) a ProRegTx/ProUp* pair that honest v23 nodes accept and relay. MPIX-13 and MPIX-15 have the same shape and are `profile`. MPIX-12's stated reason, that v23 stalls an honest miner, does not hold for rdashd, because MPIX-21 (`v24-all`) already makes the template skip a failing package.
- Fix:
  - add a Policy B (defect fix) with explicit criteria and its row list, for sign-off;
  - move RPC-10, WAL-08 and MPIX-16 out of H. Better still, key RPC-10 `profile`: emulation is one bool and Policy S says RPC behaviour is profile;
  - key MPIX-12 `profile` (`PolicyRules.protx_collateral_vs_pending`);
  - for MNO-07, either key it `profile` or record an explicit exception to the interop rule (+20 is below discouragement; frequency of the race).

**M3. The §10 per-task view breaks the D3 layering it is meant to implement.**
- §10 is generated from the §5 Consumers column, which lists every task that touches a behaviour, not every task that reads the field. Under A1 §2.2/§2.3 the following tasks sit in crates that cannot depend on the field's home. §10 says "Task briefs quote their line", so these briefs would ask for impossible reads.

  | Task (crate) | Fields it is told to read (home crate) |
  |---|---|
  | CS4 (`dash-consensus`) | `LlmqRules.ehf_sign_window` (dashd-quorums); `RpcRules.bip9_stats_window` (dashd-rpc) |
  | LQ4a, LQ5 (`dash-llmq`, pure) | `Identity.min_mn_proto_version` (dashd-p2p); `LlmqRules.dkg_bad_connection`, `sigshare_recovery_skip_unreachable` |
  | BS7 (base-sdk `dash-p2p-core`) | `P2pRules.msg_asset_unlock` |
  | MP1b (`dashd-mempool`; the orphanage lives there per A1 §2.2) | `P2pRules.orphan_processing` |
  | CH2, CH5, CH8 (`dashd-chainstate`) | `PolicyRules.blocksonly_max_mempool_mb`; `InitRules.verifydb_require_full`, `RpcRules.verifychain_strict`; `WalletRules.importwallet_pruned` |
  | MN1 (`dashd-mining`), DK1 (`dashd-quorums`), IX2 (`dashd-indexes`) | `RpcRules.networkhashps_strict`, `dkgstatus_mineable_height`, `index_rpc_errors` |
  | P2a, P8 (`dashd-p2p`), AM1 (`dashd-masternode`), GV1b (`dashd-governance`), RP1a (`dashd-rpc`) | `InitRules.*` (dashd-node) |
  | GV1b | `SyncRules.gov_vote_sync_peer_bound` (dashd-masternode) |
  | P6a, P5a (`dashd-p2p`) | `GovernanceRules.invalid_vote_cache`; `SporkRules.getsporks_dedup` |
  | WL1 (key-wallet), WL2b, WL4, WL5 (`dashd-wallet`) | `WalletRules.empty_hd_chain_counts_as_one`; `RpcRules.address_decoding`, `raw_tx_arg_order`, `utxoupdatepsbt_txindex`, `composite_args` |
  | WL6 (`dashd-wallet`, which does not depend on dashd-p2p) | `Identity.protocol_version` |
  | AR1 (`dash-args`), AR2 (`dash-rpc-spec`), AR3 (`rdash-cli`) | `SporkRules`/`WalletRules`/`InitRules`/`RpcRules` fields |
  | IM1a, IM1b, IM2 (`dash-coreimport`) | `PolicyRules.fee_estimates`, `InitRules.settings_file`, `SporkRules.dat_format`, `GovernanceRules.invalid_vote_cache`, `WalletRules.*` |

- The principle in spec §3 is right: a pure crate defines its own parameter and its runtime caller fills it. The table does not apply it, and §2 rule 6 has no clause for runtime crates that sit below a group's home.
- Fix:
  - split §10 into "reads `Group.field`" and "receives parameter `T` from X", and generate it from a new per-row `readers` field instead of Consumers;
  - define enum types in the lowest crate that needs them and re-export them from the group (`BadConnectionCriteria` in dash-llmq; `OrphanProcessing` in dashd-mempool, or move P2P-13 to `PolicyRules`);
  - AR1/AR2/AR3 and IM* consume only `options(Release)`, `surface(Release)` or their own release-keyed tables.

**M4. The `v24-all` rows do not name the verification list that must record them, so "consumers listed per row" is not met for the declared deviations.**
- 59 of the 65 `v24-all` rows name none of O10, O13a-c, O14 or O15 in Consumers (for example DKG-03 "DK1, BS9a, O3"; DKG-09 "QM1b, BS8"; MNO-07 "GV1b, P6a"; RPC-07 "ZQ1").
- Spec §8 assigns them only with "e.g." lists, and 27 `v24-all` rows appear in no §8 list. These include observable ones: DKG-06, MPIX-12, MPIX-21, MNO-08, MNO-12, MNO-21, WAL-25, WAL-31, CONS-25, CONS-30, P2P-31, P2P-39.
- Report §6.1 item 1 ("the list for O13b/O14/O10 is the `v24-all` rows of §5") cannot be applied mechanically.
- Fix: give every `v24-all` row a generated "deviation list" value (O10 / O13b / O14 / O15 / none-internal), add that task to its Consumers, and make spec §8 the complete partition.

### Minor

**m1. SIG-19 is not "MN-observable on mainnet today"** (§0 finding 1; evo EVO-31). Bit-12 MNHF signals are already mined and live on mainnet (message of 1be3ac299a; CONS-03 shows they never expire). v23's `UpdatedBlockTip` signs only when `ehfSignals.find(bit) == end` (v23 `llmq/ehf_signals.cpp:45`), so v23 MNs sign nothing on mainnet, and neither do v24 MNs. Keep `profile` (O11 `feature_mnehf.py` needs the v23 window on regtest), but fix §0, EVO-31 and the mainnet-pilot count in §6.1 item 4.

**m2. MPIX-01 is not an interop-rule case** (§2 "EVO-15 and MPIX-01 are new ones"; §0 finding 5). The mempool packet itself says an honest v23.1.8 peer cannot trigger the scoring split, because it applies the same STANDARD flags before relaying. Keep `profile` (the value is shared with CONS-07; probe parity); fix the rationale.

**m3. EVO-17 is `profile` but unobservable.** Mainnet DIP3 is at 1,028,160 (v23 `chainparams.cpp:185`), below the last checkpoint at 2,487,500 (v23 `chainparams.cpp:336`). That is the same argument the report uses to make CONS-09 `v24-all`. ATMP and GBT always run with DIP3 active, and no v23.1.8 functional test asserts `bad-tx-type`. Return EVO-17 to `v24-all` (the evo auditor's keying) and drop `RejectStrings.special_tx_before_dip3`, or state a reason.

**m4. MNO-04 contradicts Policy F.** `sporks.dat` is not a wallet file, each pin loads the other's file as empty, and the store refills from peers. Under Policy F the importer reads both formats and rdashd keeps its own, so `SporkRules.dat_format` is unnecessary. Key it `v24-all`/importer-only, or amend F.

**m5. RPC-46's "gated, never on mainnet" is wrong for the decode RPCs.** v24 `TxToUniv` emits `instanceHash` for any AU-v2-shaped transaction and `proDisTx`/`proUpShareTx`/`proUpSharedRegTx` by `nType` alone (v24 `core_write.cpp:323-338`); EVO-14's v3 ProTx layout also changes the decoded JSON. `decoderawtransaction` is ungated, and any client can feed it a testnet transaction on a mainnet node. Key the decode-JSON part to follow `au_stable_txid`/`special_tx_max_type` (or to `RpcSurface`), or declare it for O14.

**m6. One delta is missing from coverage: `ValidateDSTX` order.** v24 checks "already have" before `IsValidStructure`; v23 checks structure first (compare `ValidateDSTX` at both tags). A known DSTX hash resent with a null masternode identity therefore gets +10 on v23 and 0 on v24. The mn-overlay Notes handed this to P2P, but it was never rowed. Add a row: `v24-all` under H, deviation list O13b.

**m7. MPIX-10 belongs in §0 finding 8.** `-prune` together with `-addressindex` or `-spentindex` is a valid v23.1.8 mainnet configuration that fails to start on rdashd: v24 `init.cpp:1249-1254` InitErrors on it. P2P-24/-33..-38 are in the same class and are `profile` for exactly that reason. Keep `v24-all` for the IX2 design reason, but list it in finding 8 and in the RL2 migration notes.

**m8. MNO-06 (`profile`) combined with MNO-07 (`v24-all`) gives a behaviour neither pin has.** Under V23_1_8 rdashd keeps `cmapInvalidVotes` but runs the v24 orphan gate. The spec must say whether an orphan-gate PERMANENT_ERROR/20 enters the cache. v23 inserts only after `ProcessVote` on a known parent (v23 `governance.cpp:835-838`), so the answer should be "no".

**m9. Spec drift.**
- WAL-29's home is `InitRules.log_categories` in report §5, spec §6 and spec §10, but spec §4.2 defines `log_scan_category: bool`.
- The `superblocks_gate` row cites a syncing refusal "with RpcRules", but no such RpcRules field exists. The test-chain scoping follows from `Spork9`/`Always` (v23 `rpc/mining.cpp:737-740` vs v24 `:741-754`).
- Spec §3 says A1's `rpc: &'static RpcSurface` is dropped, yet the §4.3 aggregate carries `rpc_surface`. Say it is dropped below `dashd-node` only.

**m10. Stale planning text is missing from the §6.2/§6.3 edits.**
- PLAN CS4 row: "MNHF valid bits per profile".
- CS0 §8.2 comment: "Rows 3 (MNHF valid bits; CS4/EV3)".
- CS0 §8.2 lets only regtest select v23.1.8, but spec §5 also allows devnets. A devnet run under V23_1_8 meets MPIX-03's devnet default difference (v23 devnet `fRequireStandard=false`), which was made `v24-all` on the premise that "only v24-pinned networks differ". Either restrict `-releaseprofile=23.1.8` to regtest or note the deviation.

**m11. Duplicate merges resolved three keying disagreements silently.** EVO-09 → CONS-06, EVO-27 → MPIX-14 and MNO-10 → P2P-21 each went from `v24-all` to `profile`. `decisions.json` `keying` records only EVO-06, EVO-17 and RPC-39. Record these three with reasons, so the re-pin audit replays them.

### Nits

- **n1. MNO-11's "1 → 3 peers" framing is imprecise** (§0 finding 5, §4.1; P2P-19's "one peer per hour"). Within one round v23 already asks up to `nPeersPerHashMax` (3) peers: the loop breaks at `mapAskedRecently[hash].size() >= nPeersPerHashMax`, which is identical at both tags. Only `FetchGovernanceObjectVotes`' cap differs (1 vs 3; v23 `governance.cpp:950`), which governs re-asking in later rounds. The field `VoteSyncBound` is right; fix the prose.
- **n2. DKG-07 detail.** The v24 pre-pass keys on the claimed `(llmqType, qc.quorumHash)` of non-null commitments (v24 `blockprocessor.cpp:255-264`). With the same claimed hash and a different `quorumIndex`, v23's second copy fails `bad-qc-block` instead (and the first copy's mineable entries may already have been erased); v24 gives `bad-qc-dup`. Add this to the §4.2 multi-fault row. Also, "only a miner can produce such a block" is too narrow: any GBT `proposal` client reaches the `fJustCheck` path.
- **n3. Scope numbers.** `git log --no-merges v23.1.8..v24.0.0-rc.3` gives 1,662 commits. The 1,757 counts per-merge `p1..p2` ranges (inventory.py:186), which include v23.1.x commits already in v23.1.8. The 1,633 files are counted with `--no-renames` (1,621 with rename detection). State the method.
- **n4. `Release::V23_1_8` names a tag**, while spec §9 says variants name lines and DEC-05 pins "latest v23.1.x". A v23.1.9 re-pin would leave the variant misnamed. Consider `V23_1`.
- **n5. The spec §2 rule-2 grep** (`Release::` only in `release.rs`) would flag ND1's init-time `Release::default_for`/`parse`. Put the selection in `dashd_node::release` or exempt it.
- **n6. WAL-01's guard is not data.** The `load_on_startup` guard differs (v23: only when `descriptors=true`, v23 `wallet/rpc/wallet.cpp:634-640`; v24: whenever `descriptors` is given, `:665-669`). State that it follows `default_kind`, or add a field. This matters if DEC-02 changes the V23_1_8 default.
- **n7. Merged rows lose PR provenance.** WAL-46 lacks CONS-16's #7356 (bitcoin#28587); P2P-28 = WAL-07 ∪ WAL-17. Union the PR lists in merge.py.
- **n8. Spec §3 item 1 should state the "shared value" criterion.** `UngatedRules` also holds non-validity rows (SIG-13's miner and CL-signer gates, MPIX-19, P2P-16) because they share a value with a validity row. Conversely, SIG-15 (coinbase CL adoption feeds chain selection) lives in `LockRules`.
- **n9. Naming.** CS0 uses `dash_params::Network`; the spec uses `NetworkKind`. Align.
- **n10. CS0 alignment list.** Spec §3's CS0 alignment should also rename the CS0 §8.4 comment `rules.header_time_source` and drop the A1 row-3 comment (see m10).

## Design assessment (DEC-24 D3)

- **Layering.** The two layers are sound: `Release` and `UngatedRules` in `dash-consensus`, which every node crate can read; runtime groups in owning crates; the aggregate in `dashd-node`. The groups' homes are right for their primary readers (P2pRules → p2p, PolicyRules → mempool, LlmqRules → quorums, and so on). The defect is the consumer mapping (M3), not the homes, with one exception: P2P-13 is read by the orphanage, which A1 places in `dashd-mempool`.
- **`UngatedRules` contents.** Nothing consensus-relevant is left out:
  - SIG-15, MNO-06 and DKG-01 change node-local inputs (CL state, governance tallies, commitment contents), and their runtime crates own those; pure code sees the results through `ChainHelper` and the views.
  - CONS-10, EVO-14 and EVO-18 change consensus code but are declared `v24-all` and handed to O10.
  - The only items in `UngatedRules` that are not about validity are there because they share a value (n8).
  - `IsGate` correctly folds the gate and the per-transaction filter (`!vin.empty()` vs `HasLockInputs`, v24 `instantsend/lock.cpp:59-62`) into one enum. M1 must be fixed in its documentation.
- **Data, not code.** The rules hold. `ObjectDownload` as one enum (rather than separate booleans) is the right granularity: v23 `net_processing.cpp:1574-1712, 6707-6760` and v24's `TxRequestTracker` differ in five coupled ways.

## Rows verified against source

Legend: ✔ confirmed · ✔* code confirmed, but the report's framing or keying needs the noted fix · ✘ claim wrong.

| Row | Result | Evidence (tag:path:line) |
|---|---|---|
| CONS-04 | ✔ | v23 validation.cpp:4143-4144 (`nAdjustedTime`), 4282, 4570 (`GetAdjustedTime()`); v24 :4509-4510 (`NodeClock::now()`); v23 timedata.cpp (≥5 samples; `DEFAULT_MAX_TIME_ADJUSTMENT` = 4200 s, timedata.h:16) |
| CONS-06 | ✔ | v23 validation.cpp:324-336 (types 0-9); v24 :358-372 (+10-12); v24 specialtxman.cpp:1573, 1630, 1799 (`*-too-early`, TX_CONSENSUS) |
| CONS-07 | ✔ | v23 standard.h:57 (P2SH only); v24 :47-51; v23 validation.cpp:1912-1934 (retry without STANDARD_NOT_MANDATORY), 2544 |
| EVO-02 | ✔ | v23 assetlocktx.cpp:71-72, 179-180; v24 :56-61, 207-212 |
| EVO-06 | ✔ | v24 validation.cpp:4347-4359 (BLOCK_MUTATED `bad-cbtx-assetunlockmerkleroot`); v23 cbtx.h:33-60 (no v4); v24 cbtx.h:37, 61-66 |
| EVO-15 | ✔ | v24 providertx.cpp:420-422, specialtxman.cpp:1344-1350; v23 specialtxman.cpp:1055-1138 (no type check), 339-344 (mismatch before range) |
| EVO-17 | ✔* (unobservable on mainnet, m3) | v23 specialtxman.cpp:111-113; v24 :227-229 |
| DKG-07 | ✔* (n2) | v23 blockprocessor.cpp:165-264; ProcessCommitment returns on `fJustCheck` before the EvoDB write, erasure at :370-375; v24 :255-264 pre-pass; v23 :446-451 non-rotated dup |
| SIG-01 | ✔ | v24 primitives/transaction.h:42-56, 383-395 (`nVersion ≥ 3`, type 9, 145 bytes, `payload[0] ≥ 2`), transaction.cpp:81-124; v23 transaction.cpp:73-75, 96-98 |
| SIG-12 | ✔* (M1) | v23 validation.cpp:2607-2612, instantsend.cpp:819-828; v24 validation.cpp:2801-2805, lock.cpp:59-62 |
| SIG-13 | ✔* (CL-signer half mainnet-unobservable, M1) | v23 miner.cpp:382, chainlock/signing.cpp:83, 129; v24 miner.cpp:417, signing.cpp:129 |
| MPIX-01 | ✔* (m2) | as CONS-07; v24 MaybePunishNodeForTx: TX_CONSENSUS +100, TX_NOT_STANDARD 0 |
| MPIX-19 | ✔ | v24 miner.cpp:51, 215 (`NodeClock`); v23 :53, 222 (`GetAdjustedTime`); rpc/mining.cpp `curtime` = block time at both tags |
| MNO-02 | ✔ | v23 governance.cpp:1505-1508, payments.cpp:243-252, 311, 343, rpc/mining.cpp:737-740, 966; v24 rpc/mining.cpp:741-754, 980 (`true`) |
| P2P-16 | ✔ | v23 net_processing.cpp:1407, 6225, 6246, 6627; v24 :1430, 6370, 6391, 6785 |
| CONS-03 | ✔ (A1 row 3 refuted) | `IsValidMNActivation` identical at both tags (chainparams.cpp:109-130; only the preceding log line differs); mainnet V24 bit 12 → 13 and EVO_SHARES 14, all NEVER_ACTIVE (v23 :212-219, v24 :212-228) |
| CONS-09 | ✔ (A1 row 26 → `v24-all`) | v23 validation.cpp:4548-4585 (restored only on success); `ScopedBLSLegacyScheme` absent at v23, 12 uses at v24; mainnet V19 1,899,072 < checkpoint 2,487,500 (v23 chainparams.cpp:193, 336) |
| DKG-04 | ✔ (A1 row 15) | v23 llmq/dkgsessionhandler.cpp:29-58 (per NodeId, charged before the dup check); v24 :29-56 (per proTxHash, dup check first) |
| DKG-09 | ✔ (A1 row 14) | v23 llmq/quorums.h:108-121 (optional `nError`); v24 llmq/net_quorum.cpp:85-94 (+100 oversized) |
| DKG-10 | ✔ | v23 quorumsman.cpp:449-495 (reply, +25 only on repeat); v24 net_quorum.cpp:121-129 (+100) |
| DKG-11 | ✔ | v23 quorumsman.cpp: tracked in `mapQuorumDataRequests` before any validation; the requester only logs QDATA errors (v23 :533-536), so the error-code change is harmless |
| DKG-12 | ✔ | v23 llmq/snapshot.h:100 (unbounded); v24 :100-111 (`LIMITED_VECTOR`, 4096); v24 net_processing.cpp:5664-5668 (+100 malformed) |
| DKG-01 | ✔ | v24 active/dkgsession.cpp:269-279 (`has_mn_connection && !relays_txs / !serves_compact_filters`); #7755 (1552688094) is an ancestor of rc.3, not of v23.1.8 |
| P2P-04 | ✔ (A1 row 18 corrected) | v23 protocol.h: last type 33; v24 protocol.h MSG_ASSET_UNLOCK=34, `IsGenTxMsg` includes it, protocol.cpp:305 GetCommand=`tx`; INV loop gates only `IsKnownType` (v24 :4523); the version gate is send-only (v24 :3625) |
| P2P-05 | ✔ (A1 row 9 corrected) | v23 net_processing.cpp:89-96 (`MAX_PEER_OBJECT_ANNOUNCEMENTS = 2*MAX_INV_SZ`; 2 s `INBOUND_PEER_TX_DELAY` applied with `!fPreferredDownload`); v24 :85-115 |
| P2P-06 | ✔ | v23 :6692-6705 (expiry 10× interval), :6709 (100 in flight); v24 tracker |
| P2P-07 | ✔ | v23 :1574-1587 (`g_erased_object_requests` stamp), :6722-6727 ("GETDATA skipping") |
| P2P-10 | ✔ (A1 row 11) | v23 :5602 (> 100+16); v24 :5752 (> min(50 000, 100 016)); +20 at both |
| P2P-13 | ✔ | v23 txorphanage.cpp:155 `AddChildrenToWorkSet(tx, peer)`; v24 :155-172 per `fromPeer` |
| P2P-15 | ✔ | v24 net_processing.cpp:1240-1245 (drop MSG_TX/DSTX while `m_next_inv_send_time == 0`) |
| P2P-18 | ✔ | v24 net.h:118 `MAX_VERIFIED_INBOUND_PER_PROTX{3}` |
| SIG-16 | ✔ | v24 llmq/signing.cpp:31-38, 136-170 (`rs_m`/`rs_p`/`rs_u`, Platform type only), 594-596, 666-679 (`IsConflicting` false) |
| SIG-17 | ✔ | v23 signing_shares.cpp:1036-1049; v24 :752-774, 1009-1069 (skip unreachable, `sendCount` backoff) |
| SIG-19 | ✔* (m1) | v23 ehf_signals.cpp:36-48 (`:45` skips bits with a live signal); v24 :36-63 (`IsSignalWindowOpen`, skips EVO_SHARES) |
| MPIX-02 | ✔ | policy.h:29: 83 → 65 |
| MPIX-15 | ✔ | v23 specialtxman.cpp:152-157 (`std::nullopt` indexes in mempool); v24 validation.cpp:921-957 (`txn-already-known`, `assetunlock-stale-instance`, TX_CONFLICT) |
| MNO-06 | ✔ | v23 governance.cpp:804-810 ("Old invalid vote" +20), 835-838 (insert), governance.h:187, 203, 223 (persisted) |
| MNO-07 | ✔* (M2b) | v23 governance.cpp:812-822 (orphan cached, WARNING, no penalty) |
| MNO-11 | ✔* (n1) | sync_manager.cpp: only the `FetchGovernanceObjectVotes` cap argument differs; v23 governance.cpp:950 |
| RPC-02 | ✔ | v23 rpc/blockchain.cpp:1636 (always); v24 :1624-1627 (`IsDeprecatedRPCEnabled("softforks")`) |
| RPC-10 | ✔* (M2a) | v23 httprpc.cpp:247-262 (`whitelisted(jreq)` on the envelope) |
| WAL-01 | ✔ (+n6) | v23 wallet/rpc/wallet.cpp:592 (`Default{false}`), 634-640; v24 :613 (`Default{true}`), 665-669 |
| WAL-21 | ✔ | v24 script/descriptor.cpp:412-422 (marker preserved), 1273-1279 (`DescriptorID` = SHA256(`ToString()`)); walletutil.h `DeserializeDescriptor` recomputes the id at both tags; v23 bip32.cpp always writes `'` |
| RPC-46 | ✘ (gating claim, m5) | v24 core_write.cpp:323-338 |
| Scope | ✔* (n3) | 423 first-parent PRs ✔; 1,633 files with `--no-renames` ✔; 1,662 non-merge commits (not 1,757) |

Not independently verified (packet evidence accepted after reading): DKG-03 and DKG-13 internals, P2P-12's grace and LRU, P2P-24..-38 InitError texts, and the RPC-surface rows other than RPC-02 and RPC-10.

Coverage spot-check, five files with large diffs:
- `net_processing.cpp`: every hunk context; all scoring and constant changes.
- `validation.cpp`: every reject-string, AbortNode and ChainLock-related change.
- `evo/specialtxman.cpp`: all added and removed reject strings.
- `llmq/signing_shares.cpp`: scoring, constants and recsig lookups.
- `init.cpp`: every InitError, soft-set and arg change.

Everything behaviour-changing maps to an existing row except m6 (`ValidateDSTX` order). The moved strings (`bad-protx-collateral-reuse` → `providertx.cpp:152`) and the moved checks (`HasRecoveredSigForSession`) are refactors.

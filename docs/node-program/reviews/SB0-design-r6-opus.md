# SB0 design review, round 6 — Opus high (DEC-92 sequential pass after Sol)

Reviewed `docs/node-program/designs/SB0-shared-base.md` **rev4 at 53091a32**, in the detached checkout `/work/scratch/sb0-opus-final/wt`. Line numbers below (`SB0:N`) are that file's. Also read: `briefs/REVIEW-COMMON.md`; Sol r1–r5 (`reviews/SB0-design-r{1..5}-sol.md`); DEC-81, 84, 85, 87, 89, 92, 104, 105, 107 and 109; the EV addendum `designs/EV-addendum-SB0.md` at fa09d8d0 (it is under its own review, so it is read here only as SB0's companion); PLAN rows CV1–CV5, CH1, CH2b, P2a and WL1–WL8; and P2a's CH1 requirements R1–R8.

Code read: integration's dash-spv, dash-spv-ffi, key-wallet-manager, dash (sml, bip158), crypto, dash-consensus and `contrib/node/pure-clippy.toml`; #902 at a0e64364 (`/work/scratch/sb0-reconcile/pr902.diff`; still draft and conflicting, last updated 2026-09-22); Platform @6499c680c6 (`rs-platform-wallet`, `rs-platform-wallet-ffi`, `rs-dapi`); dashwallet-desktop @c19e441 (`dw-engine`, `dw-ffi`, `dw-p2p`); Dash Core v24.0.0-rc.3 (`validation.cpp`, `node/blockstorage.cpp`, `chainparams.cpp`, `net.h`, `chainlock/handler.cpp`).

Probes: `cargo tree -p dash-spv-ffi --target aarch64-linux-android -e normal` (offline, no build). No tests were relevant to run on a design-only change; the dash-consensus rule tests Sol r1 ran still apply.

Settled rulings (DEC-104/105/107/109) are not re-argued. Every finding below is either outside them or a correctness gap they do not cover.

> PLAN §7: this review says nothing about current SPV validation coverage beyond "PLAN §7 handling (CANDIDATE-SPV)".

---

## Summary

The architecture is sound and buildable in this codebase. It has the right split: one network core with two processors, node-only authoritative DML, SQLite for SPV only, and V and N as siblings. Sol's five rounds closed the consensus and coherence questions inside the design's own frame.

The gaps are at the frame's edges, where SB0 meets existing code and consumers:
- one shared field breaks node Core-exactness (`best_header`);
- dash-spv must bridge two type stacks, and no row owns that bridge;
- the MN state's retention, which Platform's quorum lookup depends on, is unspecified;
- the wallet's real progress store is outside the SPV database;
- the mobile thread claims are not enforced where the threads actually come from.

None of these needs a redesign. Each has a contained fix.

**Verdict: APPROVE-WITH-CHANGES.** There are 0 blockers, 6 majors (O1–O6), 10 minors and 5 nits.

---

## Major findings

### O1 — The shared `best_header` is not Core's `m_best_header`, so the node diverges (Core-exactness)

**Severity:** major. **Where:** §4.1 `SB0:392` ("max chain work among VALID_TREE headers not conflicting with cl_floor (R5)"), `:402` (`apply_chainlock` "may move best_header"); §12.1 CH1 `SB0:1338`; SB4 `SB0:1404`.

**Defect.** In v24, `m_best_header` is path-dependent. It is not a max-over-a-predicate:
- `AddToBlockIndex` updates it only for headers with `BLOCK_VALID_MASK` status, and only on strictly greater work, so the first-seen header wins a tie (`node/blockstorage.cpp:111-115`).
- It walks back by `pprev` only when the marked or disconnected block **is** `m_best_header` (`validation.cpp:3951-3953`, `:3969-3971`, `:4089-4090`, `:4107-4108`).
- `InvalidChainFound` sets it to the tip (`:2043-2044`).
- At load it is recomputed over every `IsValid(BLOCK_VALID_TREE)` entry with `CBlockIndexWorkComparator` (`:5359-5360`). That set **includes** headers stored with `BLOCK_CONFLICT_CHAINLOCK`, because `AcceptBlockHeader` adds them with that status, `RaiseValidity(TREE)` sets VALID_TREE, and they are not failed (`:4695`; `blockstorage.cpp:117-118`).
- `EnforceBlock` marks only the sibling roots of the ChainLocked path (`:4047-4077`). If the ChainLocked block is already active it marks nothing. Descendants of a marked sibling are not marked ("this may not be all descendants", `:4111`).

"R5" is P2a's requirement that `best_header` is never null after load (`P2a-outbound.md:449`), not a definition. SB0 introduced the predicate itself.

**Failure scenarios (node, rdashd full mode).**
1. *No restart.* Active chain A has tip a1000. Header-only branch B forks at a990 and reaches b1005 with more work. Core sets `m_best_header = b1005`. A ChainLock for a1000 arrives. a1000 is active, so `EnforceBlock`'s loop does not run and Core keeps b1005. SB0's `apply_chainlock` moves `best_header` to a1000. `getblockchaininfo.headers` reads 1005 in Core and 1000 here. The P2a header-sync start and its `getheaders` locator, both built from `best_header` (`P2a-outbound.md:869-878`, `:919`), differ on the wire.
2. *Restart.* A peer sends a higher-work header that conflicts with the best ChainLock. Core stores it with `BLOCK_CONFLICT_CHAINLOCK`. After a restart, Core's load picks it as `m_best_header`, while SB0's predicate excludes it.

Both are observable. The O13a/O16 transcripts and DIFF-RPC would catch them late. This is an SPV-motivated fork-choice rule pushed onto the node's shared field.

**Fix.**
- `HeaderChain::best_header` follows Core's update sites exactly:
  - strict-greater on `AddToBlockIndex` for valid headers;
  - the `pprev` walk-backs as hooks that V calls at Core's `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` sites;
  - the load-time recompute with Core's comparator, including the sequence-id tie-break.
- SPV's fork-choice target ("most work among non-failed, non-conflicting headers", the header-only analogue of Core's `FindMostWorkChain` / `setBlockIndexCandidates` filter) becomes a separate value. Only dash-spv's header task reads it, to call `set_active_tip`.
- SB4 adds the two scenarios above as node vectors: `getblockchaininfo.headers` and the locator's first entry, each compared with the oracle.

### O2 — dash-spv must bridge dashcore and base-sdk types, and no row owns that bridge (a hidden migration)

**Severity:** major. **Where:** §2.2 `SB0:136`, `:138` ("dashcore re-exports the old paths"); §3.2.1 `SB0:312-321` (`NetworkMessage`); §5.5 `SB0:934-936`; §12.2 `SB0:1361-1362`; §13.2 CV4 `SB0:1427`; SB7, SB9, SB12–SB16.

**Defect.**
- L2 forbids S and P from depending on W (`SB0:167`). So `dash-chain`, `dash-mnlist`, `dash-evo` and `dash-filters` take base-sdk and `dash-consensus` types: `dash_consensus::Header`, `BlockHash`, `BlockId(u32)`, and base-sdk's mnlistdiff, qrinfo, clsig and isdlock types.
- dash-spv, its managers, key-wallet-manager (`WalletInterface`), the FFI and every Rust consumer speak dashcore types. #902's facade, which SB16 keeps and extends, carries `dashcore::network::message::NetworkMessage` (`SB0:313`, `:318`, `:321`).
- So from SB12 on, every header, block, cfheader, cfilter, mnlistdiff, qrinfo, CL and ISLock crosses a conversion. The conversions are re-encode/decode or field-by-field.
- PLAN places exactly this bridge in **CV3 (post-M4)** and **CV4 (post-M7)**, which say "the bridge becomes the identity" (PLAN rows CV3/CV4). SB0 needs it at M3–M4. SB0 marks CV4's P2pMsg part as "subsumed by SB16" (`:1427`), but SB16's facade still names `NetworkMessage`.

Concrete consequences:
1. **Re-exports are impossible as written.** `dash/src/bip158.rs` and `dash/src/sml/{merkle_roots,quorum_entry/validation,…}.rs` take `dashcore::Block`, `dashcore::BlockHash` and dashcore SML types. Once that code moves to P crates on base-sdk types, dashcore cannot `pub use` it. It needs adapter functions with the old signatures, which Platform calls through `dashcore::sml` (`rs-platform-wallet/src/masternode/list.rs:18-19`, `spv/runtime.rs:11`).
2. **Hash-twice cost.** `dash_consensus::Header::new` always runs X11 (`dash-consensus/src/primitives/block.rs:26-27`, DEC-51a). dash-spv has already hashed the dashcore header, so converting hashes every SPV header twice (more for compressed `headers2`). That is about +1 s for a 50,000-header anchor sync and tens of seconds for a genesis-origin sync on a phone. It works against SB12/SB14's BG gate "`spv-headers-*` not slower" (`SB0:1412`, `:1414`).
3. **Platform-facing queries keep dashcore shapes** (§10.1 `SB0:1254`). `DashSpvClient::latest_masternode_list() -> dashcore::sml::masternode_list::MasternodeList` (`dash-spv/src/client/queries.rs:53-56`; Platform `spv/runtime.rs:507-513`) and `get_quorum_at_height -> QualifiedQuorumEntry` (`queries.rs:73`) must then be filled from `dash-mnlist`'s base-sdk state on every call.

**Fix.**
- Add an explicit bridge to the plan:
  - a `dash-spv::bridge` module (V, so W is allowed) owning every dashcore ↔ base-sdk conversion;
  - **hash once**: either decode at the network edge with base-sdk and derive the dashcore value from it, or have `dash-consensus` provide a crate-sanctioned `Header::from_parts_hashed(raw, hash)`, with a debug assertion, for hashes computed locally from the same bytes. DEC-51a needs a one-line note either way;
  - the adapter functions dashcore keeps for the moved bip158 and sml code.
- Size it: SB12 M → L, or a new S row SB12a before SB12. Rerun `cpm_sb0_rev1.py`. These rows have slack against M5 (`SB0:1533`), so the milestones should not move, but the work must exist.
- Fix §13.2's CV3/CV4 text: either CV3's wrappers move before SB12, or SB0's bridge is what CV3/CV4 later collapse into the identity.

### O3 — The MN state's retention is unspecified, and Platform's quorum lookup depends on it

**Severity:** major. **Where:** §5.3 `SB0:893-900` (`lists`, `quorums`, `recent`); §5.2.1 rule 6 `SB0:863-864`; §5.5 `SB0:936`; §8.2 `SB0:1115`; §9.5 `SB0:1199`, `:1224`.

**Defect.**
- `lists: SmlLists` is "sparse by BlockId: diff targets and qrinfo heights". dash-spv diffs to every new tip once synced (`sync/masternodes/sync_manager.rs:271-316`), and SB0 relies on that for H_keep (`:849`). `quorums` grows with every mined commitment. Neither has an eviction rule, in memory or in `mn_lists`/`mn_quorums`.
- Verification needs lists at many work blocks. Mainnet's Platform type `llmq_100_67` has C = 24 and A = 24, so verifying it needs a list per 24 blocks, plus four quarters per rotated cycle.
- At ≈ 0.8 MB per distinct list (`:1115`), even partial structural sharing does not bound this.
- Today's engine has `apply_and_prune` / `prune_after_extending` (`dash-spv/src/sml_engine.rs:701-710`). SB0 replaces it and states no successor.

**Platform depends on old quorums.**
- `ffi_dash_spv_get_quorum_public_key(type, hash, core_chain_locked_height)` (`dash-spv-ffi/src/platform_integration.rs:42-91`) and `rs-platform-wallet`'s `get_quorum_public_key` (`spv/runtime.rs:340-359`) call `get_quorum_at_height(height, type, hash)`.
- That function today answers "the quorum was in a list at or before height", falling back to the **stored message log** (`queries.rs:73-105`). §9.5 drops the message log (`:1224`).
- §5.2.1 says the horizon does not apply to lookups by hash (`:864`), but nothing says the quorum is still held. Platform verifies proofs at its `core_chain_locked_height`, which lags the tip and can name a quorum that has since left the active set.
- Under SB0 as written, either the engine keeps everything (an unbounded mobile memory window) or Platform's proof verification fails for any quorum that has rotated out.

**Fix.** Specify retention in §5.3 and test it in SB11/SB15:
- `quorums`: keep each used-type commitment's `{key, status, mined height, replaced-at height}` persisted indefinitely in `mn_quorums`. That is about 60 rows a day on mainnet, which is small. Keep in memory only the commitments active within the authenticated domain, plus an LRU.
- `lists`: drop a list once no unverified commitment, pending fetch or `recent` set needs it. Keep the tip list for `latest_masternode_list()`.
- Define `get_quorum_at_height`'s new semantics over that state: `Verified`, mined at or before `height`, and not replaced at or before `height − X` if Platform needs that. Write the definition with Platform's owner (through pasta) and add a consumer note.
- Gate: Platform's lookup for a quorum rotated out ≥ 1 day earlier, after a restart, with no network. Bound the measured RSS in SB17.

### O4 — The wallet's progress lives in the host's database, so "each subscriber commits data with its own locator" does not hold for the wallet

**Severity:** major. **Where:** §6.1 `SB0:997`; §9.5 `SB0:1200`, `:1212`, `:1214-1219`; §4.6 `SB0:710`; SB13/SB14.

**Defect.**
- §9.5 puts a "wallet locator" in the SPV store's `locators` table and says recovery resumes every subscriber from its own locator, blindly.
- The wallet's real progress is `WalletInterface::{synced_height, wallet_synced_height, update_wallet_synced_height, last_processed_height}` (`key-wallet-manager/src/wallet_interface.rs:131-171`). It is per wallet, forward-only and height-keyed, and the host persists it with the wallet: Platform's `platform-wallet-storage` SQLite, or dashwallet-desktop's `dw-appdb`.
- Those are different databases from the SPV store, with no atomic commit.

**Failure scenarios.**
- **SPV locator ahead of wallet.** The wallet's DB loses a Buffered suffix on power loss (wallet at H1) while the SPV store's wallet locator is at H2 > H1. The SPV store resumes the wallet subscriber at H2, and transactions in (H1, H2] are never matched. Funds stay invisible until a manual rescan.
- **Wallet ahead after an offline reorg.** The wallet stores heights, not hashes, so at restart neither side can detect that blocks the wallet applied were orphaned. The `truncate_above` that #1104 adds needs a trigger.

**Fix.**
- The wallet's own synced heights are authoritative. The SPV store keeps **no** wallet locator. The SPV store's wallet-feed resume point is `min(wallet.synced_height, durable SPV tip)`.
- Add a `WalletInterface` method for a hash at the synced height, as an additive #854-style method. At open, a mismatch with the SPV store's chain calls `truncate_above(fork)` once.
- State the ordering. The wallet may advance `synced_height` past a block only after that block's header step has committed (the existing commit-before-publish rule already gives this). Buffered-vs-Durable skew between the two databases is tolerated by the min rule.
- Test in SB13/SB14 with crash points in each database independently (the SPV `Sut` plus a faulty wallet store).

### O5 — The mobile thread claims are not enforced where the threads come from

**Severity:** major (asserted, not planned). **Where:** §7 `SB0:1051-1053`, `:1062-1067`; L5 `SB0:170`; SB13 `SB0:1413`, SB17 `SB0:1417`.

**Defect.** §7 asserts "one injected rayon pool, default min(2, cores)" and "blst's own pool never starts". L5 enforces this only for S crates. The threads come from elsewhere (confirmed with `cargo tree -p dash-spv-ffi --target aarch64-linux-android -e normal`: `rayon`, `rayon-core` and `blst` are all in the closure):
1. **rayon's global pool**, num_cpus threads: `key-wallet-manager/src/matching.rs:71` (`into_par_iter`, enabled by dash-spv's `parallel-filters`, `dash-spv/Cargo.toml:17`), and `dash-spv/src/validation/{header.rs:22, filter.rs:75}`.
2. **blst's pool**: dashcore-crypto's `verify_secure_aggregate` → `dash_pkc::bls::BlsSignature::secure_verify_aggregates` (`crypto/src/bls.rs:405-442`), which is on DEC-79's banned list (`contrib/node/pure-clippy.toml:120`). dashcore's `quorum_validation` and `message_verification` features keep this reachable from dash-spv until SB15, and from dashcore's public API afterwards.
3. **tokio defaults**: the FFI sets `worker_threads` only when it is non-zero (`dash-spv-ffi/src/client.rs:48-51`), and `max_blocking_threads` is tokio's default of 512.

SB13's exercised-path thread test includes "the MN pipeline" (`:1064`). At SB13 that pipeline is still dashcore's verifier (SB15 migrates it), so the test fails or gets waived. No row owns any of the three fixes.

**Fix.**
- Give dash-spv (V) the blst pooled-call bans through a V clippy config, and run DEC-79's audit over the dashcore-crypto / key-wallet paths reachable from dash-spv-ffi.
- dash-spv drops dashcore's `quorum_validation` / `message_verification` features once SB15 lands.
- Run key-wallet-manager matching inside the injected pool (`pool.install`) or turn `parallel-filters` off on mobile.
- dash-spv-ffi sets `max_blocking_threads` (and a worker default) explicitly.
- Run the thread test's MN leg in SB15, not SB13. Assign the work to SB13 (pool and runtime) and SB15 (blst).

### O6 — Q4's recommended wallet does not exist at M5; SB18 is an M5 exit row

**Severity:** major (plan realism). **Where:** §15 Q4 `SB0:1593`; SB18 `SB0:1418`; §13.2 M5 `SB0:1444`.

**Defect.** Q4 recommends `dashd-wallet` for `-spv`'s RPC parity. `dashd-wallet` is **WL2a, at M6** (PLAN WL2a row), and SB18's deps do not include it. If pasta takes the recommendation, either M5_exit (now on SB18) slips behind WL2a, or SB18 ships with key-wallet and is redone.

**Fix.** Recommend key-wallet (`WalletInterface` via key-wallet-manager) for SB18 at M5, with `dashd-wallet` as an M6 follow-up row (the adapter in `rdashd`, A layer, honouring L2/L3's `allow_w`). Alternatively, keep `dashd-wallet` and move SB18 and M5's SPV smoke to M6 with an explicit CPM note. Either way, Q4 needs an answer before SB18's brief, and the current recommendation should change now.

---

## Minor findings

**m1 — `BlockId` width disagrees with `dash-consensus`.**
- **Where:** §4.4 `SB0:652` (`BlockId(u64)`); `dash-consensus/src/ctx.rs:14` (`BlockId(pub u32)`).
- **Problem:** `RuleContext`, `AnchoredAncestry` and EV's `LlmqChain` all pass ids into `ChainAncestry`'s u32 `BlockId`.
- **Fix:** use u32 everywhere. 4.29 × 10⁹ never-reused SPV ids is unreachable. Better still, make the SPV id equal to an arena slot in a sparse chunk table, so a released chunk leaves a hole. That removes the id→slot map and unifies both products' `BlockId` meaning (see S2).

**m2 — Eviction can precede the step's commit.**
- **Where:** §4.4 `SB0:656-658`.
- **Problem:** Eviction keys on `best_header` inside `accept`, but the batch's records commit later in the header step's `WriteSet` (`:1205-1211`). With w below the batch size (SB4/SB5 test a w below KGW's reach), a just-accepted entry is evicted before its record exists. A cold read then sees a "missing record for an issued id", which trips the latch on a healthy store.
- **Fix:** evict only after the header step commits (in `set_active_tip`), or require w ≥ the maximum batch (2,000) and assert it.

**m3 — The anchor rule ignores two existing config paths.**
- **Where:** §4.3.1 step 1 `SB0:572-577`.
- **Problem:** `ClientConfig::enable_masternodes = false` (`without_masternodes()`, `client/config.rs:188`; FFI `dash_spv_ffi_config_set_masternode_sync_enabled`) leaves the coverage step with no MN pipeline. `start_from_height`, which always wins (`client/lifecycle.rs:100-113`), and the network's `hd_wallet_sync_floor` are not named in the candidate rule.
- **Fix:** with MN sync off, coverage is vacuous, the store becomes `Established` at the candidate, and CL/IS stay `NoQuorum`. Treat `start_from_height` as the birth height, honouring the sync floor.

**m4 — The consumer list is incomplete.**
- **Where:** §10.1 Rust API `SB0:1254`.
- **Missing consumers:**
  - **dashwallet-desktop** (`dw-engine`, `dw-ffi`, `dw-p2p`: `ClientConfig`, `DevnetConfig`, `SyncProgress`, `BlockHeadersProgress`, `FilterHeadersProgress`, `MasternodesProgress`, `EventHandler`, `NetworkEvent`);
  - **rs-dapi** (`dash_spv::Hash`).
- **API surfaces Platform uses directly:** `DiskStorageManager`, `BlockHeaderStorage::{get_tip, get_header}` (`spv/runtime.rs:194-197`, `:648-682`), `PeerNetworkManager`, `CheckpointManager`.
- **Changes needing consumer notes:**
  - retiring the devnet `OnceLock`s changes `DevnetConfig` (`SB0:913`);
  - the `Candidate`/coverage phases change `SyncProgress`.
- **Fix:** list all of these, and include a note for each.

**m5 — The memory window understates two items.**
- **Peer queues** (§3.4 `SB0:378`, §8.2 `:1112`). A message up to `MAX_PROTOCOL_MESSAGE_LENGTH` = 3 MiB (`net.h:84`) is admitted before `fPauseRecv`, so the per-peer bound is ≈ 1 MiB + 3 MiB + the send budget. That is ≈ 34 MiB worst case across 8 peers, not 10.4.
- **SQLite snapshots** (§9.4 `SB0:1174`). "If the pool is empty, `snapshot()` opens one more connection rather than wait" is unbounded, and each connection carries a 4 MiB page cache (`cache_size=-4096`).
- **Fix:** state the max-message term, and cap the snapshot connections (see S3).

**m6 — SQLite on iOS and real flash are asserted, not planned.**
- **Where:** §8.1 `SB0:1079`; §9.3(f) `SB0:1162`; §11.3 `SB0:1321`.
- **Problems:**
  - "0 bytes on iOS" holds only if no crate in the iOS graph enables rusqlite `bundled`. Platform's `rs-platform-wallet-storage` enables it, and `links = "sqlite3"` unifies the graph. Per-target features are needed: `bundled` on Android and desktop, system SQLite on iOS.
  - The iOS data-protection class for the data dir is not addressed. With `NSFileProtectionComplete`, a sync while the device is locked returns `SQLITE_IOERR`/`CANTOPEN`, and §9.4 treats that as a fatal latch. Document `CompleteUntilFirstUserAuthentication` as a host requirement.
  - "SB17 measures [write rate] on the emulator" does not measure flash.
- **Fix:** specify `dash-kv-sqlite`'s per-target features; add the data-protection host note; put a real-device write-rate cell under Q6/pasta, or drop the claim.

**m7 — #1104 is not on integration and SB14 depends on it.**
- **Where:** §4.6 `SB0:710`; SB14 `SB0:1414`; §10.1 `SB0:1251`.
- **Problem:** #1104 (wallet `truncate_above`, FFI `on_chain_truncated`) is open upstream and absent from integration (`grep` finds only the storage traits' `truncate_above`). SB14 has no edge or contingency for it.
- **Fix:** add `#1104 or carry` to SB14, as SB16 does for #902.

**m8 — #902 hits stage B, not just SB16; Q1's deadline moves earlier.**
- **Where:** §10.3 `SB0:1275`; Q1 `SB0:1590`.
- **Problem:** #902 changes 26 files under `dash-spv/src/sync` (per its diff), which are the managers SB14 and SB15 rewrite into feed subscribers. If #902 merges upstream after SB14, integration must absorb it into rewritten managers. That is unsized, about +M on the SPV track. The SPV track has slack, so no milestone moves, but SB14's base must be decided before it starts.
- **Fix:** Q1 must be answered before SB14 (≈ week 16 in run A), not SB16. If #902 lands first, rebase integration onto it before SB14.

**m9 — The node-side ChainLock writer is unstated.**
- **Where:** §4.1 `SB0:402`; §2.4 `SB0:211`.
- **Problem:** `HeaderChain` is owned by V (`SB0:1049`), so `apply_chainlock` in the node must be a `ChainCommand` from `dashd-locks` to V, not a call from the locks thread.
- **Fix:** state it in §2.4 and in CH1-node's split.

**m10 — `HeaderCtx.mtp` for the bottom of the anchor pack.**
- **Where:** §4.1.1 `SB0:451-456`.
- **Problem:** A `RuleContext` entry's `HeaderCtx` (`ctx.rs:17-34`) carries `mtp` and `chain_work`. For the lowest ten pack entries, `mtp` needs headers below the pack. No rule in the envelope reads them (MTP reads only the parent's), but an eager context builder will fail or fabricate them.
- **Fix:** say the context fills `mtp` lazily, or only where the envelope reads it, and add a debug assertion.

---

## Nits

- **n1** `SB0:462`: `CONTEXT` must not take a bit in the shared status word that the node's `BlockIndexRecord` codec persists with Core's `nStatus` layout (`BLOCK_CONFLICT_CHAINLOCK` = 128, `chain.h:118`). Keep it SPV-record-only.
- **n2** `SB0:472`: the node's `CheckpointView` must be Core's chainparams list (39 mainnet entries; `-checkpoints`) and never dash-spv's 51 anchor checkpoints. Say so; the shared rule is `bad-fork-prior-to-checkpoint` (`validation.cpp:4492-4500`).
- **n3** `SB0:1147`: §9.2's evidence #4 ("one SQLite transaction holds header rows, MN rows and locators") is not how §9.5 commits: each subscriber has its own `WriteSet`. Reword it as "one database, per-component atomic WriteSets, WAL-ordered".
- **n4** `SB0:1078-1079`: "0 inside Platform's Android SDK and dashwallet-desktop" also depends on the rusqlite major staying in lockstep. (f) says so; cross-reference it.
- **n5**: the base-sdk fork `[patch]` (`Cargo.toml:56-62`, file:// URLs) reaches dash-spv's consumers once SB12 makes dash-spv depend on `dash-consensus`. Add it to Q2's upstream-shape question, since BSX must upstream the fork before stages A–C can upstream.

---

## Simplicity

- **S1 — Cut `legacy-net`** (`SB0:1267`, `:1416`). Under DEC-87 nothing ships upstream during the program, so "one release" spans nothing. Keeping two network stacks compiled and tested through M5 costs CI time and test duplication. Keep #902's old path only in git history; SB16's `dashd_sync` suites on the new router are the gate.
- **S2 — One `BlockId` meaning** (with m1). Use a sparse chunk-table arena in which id = slot for both products. This removes SPV's id→slot map and the u64/u32 split, and the node's never-taken cold branch becomes a null-chunk check.
- **S3 — The minimal SQLite snapshot.** Name the SPV callers of `KvEngine::snapshot()`. SPV's chain snapshots are in memory (§4.4), and each subscriber reads its own tables. If only the contract suite and ST3a call it, implement one dedicated read connection that waits for a free slot (the contract's `snapshot()` may block). Drop the growable pool and keep poisoning.
- **S4 — Keep** mnxcheck (cheap, default-off on mainnet), the segmented header download (existing code) and SB-ST as contingent. Nothing else is over-engineered for its purpose. The DEC-107 anchor machinery is the irreducible core.

---

## Checks requested in the brief

1. **Buildability and plan realism.** The crate split, layer rules and stage order fit the codebase. dash-spv's `sync/` is 22,712 lines (filters 6,725, mempool 3,907, masternodes 2,654), so SB14 at L and SB13 at M are tight but plausible. The hidden migrations are O2 (type bridge), O3 (MN retention and Platform semantics), O4 (wallet locator), m7 (#1104) and m8 (#902 absorption). The FFI ABI rules (§10.1) are sound: 43 functions confirmed, with `start_from_height` and `masternode_sync_enabled` semantics noted in m3. The v2 → v3 migrator is fine: v2 anchors are checkpoints (`client/lifecycle.rs:331-341`), which carry packs. All the extra work lands on rows with 14+ weeks of slack against M5, so M2 18.5 and REL11 30.0 stand. SB6 still has zero slack.
2. **Node Core-exactness.** The processor split keeps PeerManager scheduling Core's, and nothing SPV-shaped enters the actor. `NetCore::push` matches `PushMessage` (DEC-104 M2). V, B1/P10 and CH2 F1–F11/P1–P10 are unaffected. **Exception: O1** (`best_header`). Nits n1/n2 guard the shared record codec and the checkpoint list.
3. **Mobile reality.**
   - Asserted, not planned: the thread rules (O5), iOS SQLite linkage and data protection, real-flash write rate (m6), and peer-queue and snapshot memory (m5).
   - Properly labelled pending: the amended mobile profile, combined-SDK size and steady-state memory.
   - Durability (WAL, NORMAL/FULL, fullfsync) is sound inside the SPV store. Across the wallet database it is not (O4).
4. **Simplicity.** S1–S3.
5. **Q1–Q7.** See the next section.

---

## Which open questions block which rows

| Q | Blocks | Urgency |
|---|---|---|
| **Q3** (the reading of DEC-81's "one network loop") | **SB6** and **P2a-node** start at approval and have zero merge slack on M2. A "no" means a different SB6 | **Now.** Answer it with SB0's approval |
| **Q2** (upstream shape; SQLite vs SB-ST) | **SB12**: the first dash-spv dependency on node-program crates, and the base-sdk fork (n5). **SB10**'s SQLite half and **SB13 → SB14/15/17/18**, which fork to SB-ST if SQLite is refused | **Soon:** before SB10 starts (when ST3a and CH2a merge) and SB12 (≈ week 9–10) |
| **Q1** (#902's path) | **SB16** (hard edge) and so SB17/SB18. Per m8, also **SB14**'s base | Before SB14 (≈ week 16), earlier than §15 implies |
| **Q4** (`-spv` scope and wallet) | **SB18**: its deps and M5_exit (O6) | Not urgent for starting work, but the recommendation must change now, because it changes M5 if adopted |
| **Q5** (mobile ABIs) | **SB1**'s L6 target list and **SB2**'s profile and size baseline (per ABI) | Cheap: settle before SB1 lands. Arm64-only needs no change |
| **Q6** (Mac/iOS checks) | **SB17**'s final cells only (iOS build, size, fullfsync), plus m6's device cells | Not blocking a start |
| **Q7** (hybrid rdashd) | Nothing | — |

---

## Verdict

**APPROVE-WITH-CHANGES.** No blockers. Fix O1–O6 in a rev5 of SB0. Each is a local edit: O1 §4.1 and SB4; O2 §2.2, §5.5, §12.2, §13 and a CPM rerun; O3 §5.3, §5.5, SB11 and SB15; O4 §9.5 and SB13/SB14; O5 §7, L5, SB13, SB15 and SB17; O6 §15 Q4 and SB18. Fold in the minors where they touch the same text. A narrow confirm of O1–O6 by either reviewer is sufficient; a full round is not needed. SB1, SB2, SB3, SB4's design step and SB6 can start on approval once Q3 is answered. O1 must be in SB4's brief.

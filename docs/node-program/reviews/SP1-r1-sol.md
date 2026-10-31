REWORK

## Major

### 1. Flatfile CompactSize values are not range checked like Core

- **Rust:** `dashd-masternode/src/flatdb.rs:85-87`
- **Core:** `src/serialize.h:38-42,292-320`
- **Problem:** `read_compact_size` accepts every canonical value that fits in `usize` by passing `usize::MAX` to `CompactSize::into_len`. Core's `ReadCompactSize` rejects any value above `MAX_SIZE = 0x02000000`. The 1024 element allocation cap in `capacity` does not fix this: the netfulfilled and mncache readers still iterate the original count, and string lengths also bypass Core's limit.
- **Failing scenario:** A checksum-valid local `netfulfilled.dat` or `mncache.dat` with a canonical count or string length above 33,554,432 is accepted far enough to read/iterate that value. A sufficiently populated crafted file can make startup spend unbounded time parsing it; a Core node instead throws `ReadCompactSize(): size too large`, clears the object as `Format`, and recreates it. The same mismatch is present for an over-limit version string.
- **Fix:** Make the shared reader enforce Core's `MAX_SIZE` (0x02000000) before converting to `usize`, returning `None` so `decode_into` takes the `Format`/clear/recreate path. Add over-limit vectors for string, outer-map, inner-map, governance-vote, and used-masternode counts.

### 2. The production command table does not expose `mnsync`

- **Rust:** `dashd-node/src/rpc.rs:484-510`
- **Core:** `src/rpc/node.cpp:95-149,1255`
- **Problem:** `command_table` registers control, blockchain, mining, and peer commands, but never registers `mnsync`. The branch has the six sync accessors and transcript harness checks, but no server command reaches them. This leaves the PLAN line 695 acceptance (`mnsync status` DIFF-RPC) unmet. D16 says the RPC is deferred to RP2b, which is an honest design note but conflicts with the stated SP1 acceptance.
- **Failing scenario:** With an otherwise running rdashd, `rdash-cli mnsync status` (and the existing `rpc-client` `mnsync_status` helper) receives method-not-found. `next` and `reset` are likewise unavailable, so the six exact fields cannot be checked through the node's RPC surface.
- **Fix:** Either move this acceptance explicitly to RP2b and remove it from SP1's gate, or route the `MasternodeSync` handle into the RPC table and register Core's command. Implement `status` with exactly `AssetID`, `AssetName`, `AssetStartTime`, `Attempt`, `IsBlockchainSynced`, and `IsSynced`, plus Core's `next`/ `reset` return strings, and add a live RPC differential test.

## Minor

### 3. Sync construction is after chainstate load, unlike Core

- **Rust:** `dashd-node/src/node.rs:317-330`
- **Core:** `src/init.cpp:2062-2098`
- **Problem:** Core constructs `CMasternodeSync` before `LoadChainstate`; rdashd constructs `MasternodeSync` only after the load completes. Its constructor records `AssetStartTime` and `last_bump` from the clock.
- **Failing scenario:** On an unmocked node whose chainstate load takes a long time, a post-start status reports an `AssetStartTime` at the end of loading in rdashd but at the beginning in Core. The elapsed time in the first `SwitchToNextAsset` log also differs. The fixed-mocktime transcript fixtures cannot see this.
- **Fix:** Construct the sync object at the pre-load step (it does not need the loaded chain), then attach its listener after chainstate initialization; add a startup test with a clock advanced during the load.

## Checked and correct

- `SyncManager` matches rc.4 for the 6-second gate, strict timeout boundaries, the >3-peer timeout choice, the one-hour reset and its mockable-chain/active-MN exemptions, full-sync disconnects, asset order, mempool eligibility, governance one-peer-per-tick requests, asked-recently 3600-second expiry, MAX_INV_SZ guard, and vote-stop timing.
- Wire behavior matches the rc.4 captures: exact empty-bloom `govsync(0)`, `mempool` only for eligible outbound peers, and no tick `getsporks` after Core's a56ed47c98 change. The transcript comparison rejects an unexpected tick `getsporks`; the separate SP0 fixtures cover incoming `getsporks`.
- Netfulfilled expiry/maintenance and cache load/clear/store ordering match Core, including D4's stored-tip lag and storing the valid netfulfilled cache after a failed mncache load. Meta serialization covers all v24 fields, the used queue, and platform-ban height rules; `ResetPlatformBan` is present as the declared CH7a seam.
- D4, D6, D9, and D12 are documented with tests for their implemented behavior. Governance's `GovernanceSync`/`EmptyGovernance` seam is minimal and sufficient for GV1b; D16 exposes the accessors and mutations needed by RP2b, subject to finding 2.
- The mutant table claims and the checked suite cover all 16 listed timing/order mutants. The listed mutants do not exercise the CompactSize limit, production RPC registration, or pre-load construction timestamp.

## Commands run

- `git -C ~/workspace/rdc-wt/sp1 diff 497d3ca11..795edba87`
- `git -C ~/workspace/rdc-wt/sp1 log --first-parent --oneline 497d3ca11..795edba87`
- `CARGO_TARGET_DIR=/work/scratch/sp1-r1-sol/target CARGO_BUILD_JOBS=8 cargo test -p dashd-masternode --all-features --lib --test sync_transcripts --test core_flatfiles` (39 unit tests, 2 flatfile tests, 7 transcript tests passed)
- `CARGO_TARGET_DIR=/work/scratch/sp1-r1-sol/target CARGO_BUILD_JOBS=8 cargo test -p dashd-node --all-features --lib` (182 passed, 2 ignored)
- `cargo fmt --all -- --check`
- `git -C ~/workspace/rdc-wt/sp1 diff --check 497d3ca11..795edba87` (only captured transcript lines with intentional empty-payload trailing spaces)
- Read the rc.4 implementations and call sites in `src/masternode/sync.{h,cpp}`, `src/netfulfilledman.{h,cpp}`, `src/masternode/meta.{h,cpp}`, `src/flat-database.h`, `src/init.cpp`, `src/net_processing.cpp`, and `src/rpc/node.cpp`.

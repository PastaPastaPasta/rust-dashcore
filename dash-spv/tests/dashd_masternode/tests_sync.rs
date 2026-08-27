//! Masternode list sync tests using dashd.
//!
//! These tests verify SPV masternode list synchronization against a pre-generated
//! regtest masternode network (1 controller + 4 masternodes with DKG cycles).

use std::sync::Arc;
use std::time::Duration;

use tokio::time;

use dash_spv::sync::{ProgressPercentage, SyncState};
use dashcore::sml::llmq_entry_verification::LLMQEntryVerificationStatus;
use dashcore::sml::llmq_type::LLMQType;

use super::helpers::{
    assert_all_rotated_quorums_verified, wait_for_chainlock_height_at_least,
    wait_for_masternode_sync, wait_for_mn_state_event, wait_for_mn_state_event_above,
    wait_for_mn_state_with_stored_cycle_above,
};
use super::setup::{
    create_and_start_client, create_client, create_dummy_wallet, create_mn_test_config,
    TestContext, SYNC_TIMEOUT,
};

/// Sync masternode list against a pre-generated regtest controller node.
///
/// Verifies that the SPV client can complete masternode list sync (QRInfo + MnListDiff)
/// and that the MasternodeStateUpdated event fires.
#[tokio::test]
async fn test_masternode_list_sync() {
    let Some(ctx) = TestContext::new(true).await else {
        return;
    };

    let expected_masternodes = ctx.mn_ctx.metadata.masternodes.len();
    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;

    // Wait for the MasternodeStateUpdated event
    let mn_height =
        wait_for_mn_state_event(&mut client_handle.sync_event_receiver, SYNC_TIMEOUT).await;
    assert!(mn_height > 0, "Masternode state height should be positive");

    // Wait for full masternode sync
    let mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;

    assert_eq!(mn_progress.state(), SyncState::Synced, "Masternode sync should reach Synced state");
    assert!(mn_progress.current_height() > 0, "Masternode sync height should be positive");
    tracing::info!(
        "Masternode sync verified: state={:?}, height={}, diffs={}",
        mn_progress.state(),
        mn_progress.current_height(),
        mn_progress.diffs_processed()
    );

    // The engine must hold a masternode list whose entry count matches the
    // pre-generated network metadata. A successful sync that ends with an
    // empty engine indicates the MnListDiff/QRInfo plumbing dropped data.
    {
        let engine = client_handle.engine.read().await;
        let latest_list = engine.latest_masternode_list().expect("Should have a masternode list");
        assert!(
            !latest_list.masternodes.is_empty(),
            "Engine should have at least one masternode after sync"
        );
        assert_eq!(
            latest_list.masternodes.len(),
            expected_masternodes,
            "Engine masternode count {} should match pre-generated metadata count {}",
            latest_list.masternodes.len(),
            expected_masternodes,
        );
    }

    client_handle.stop().await;

    let final_progress = client_handle.client.sync_progress().await;

    // Headers should also be synced
    let header_height = final_progress.headers().unwrap().current_height();
    assert!(
        header_height >= ctx.mn_ctx.expected_height,
        "Headers should sync to at least expected height: got {}, expected {}",
        header_height,
        ctx.mn_ctx.expected_height
    );
}

/// Sync masternode list, stop, restart with same storage, verify incremental sync.
#[tokio::test]
async fn test_masternode_list_sync_with_restart() {
    let Some(ctx) = TestContext::new(true).await else {
        return;
    };

    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    // First sync
    tracing::info!("=== Starting first masternode sync ===");
    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;
    let first_mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    let first_height = first_mn_progress.current_height();
    client_handle.stop().await;
    drop(client_handle);

    // Restart with same storage
    tracing::info!("=== Restarting with same storage ===");
    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;
    let second_mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    let second_height = second_mn_progress.current_height();

    assert_eq!(
        second_height, first_height,
        "Masternode sync height should be identical after restart"
    );
    assert_eq!(
        second_mn_progress.state(),
        SyncState::Synced,
        "Should reach Synced state after restart"
    );

    tracing::info!(
        "Restart verified: first_height={}, second_height={}",
        first_height,
        second_height
    );

    client_handle.stop().await;
}

/// List the immediate children of `dir`, sorted, marking directories with a
/// trailing `/`. Used to show what a shutdown actually left on disk.
fn list_storage_entries(dir: &std::path::Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to read storage dir {}: {}", dir.display(), e))
        .map(|entry| {
            let entry = entry.expect("Failed to read storage dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                format!("{}/", name)
            } else {
                name
            }
        })
        .collect();
    entries.sort();
    entries
}

/// REGRESSION (dash-spv #954): masternode-list state is never written to disk, so
/// every restart re-syncs the list from scratch.
///
/// `MasternodeStateStorage::{store,load}_masternode_state`
/// (`dash-spv/src/storage/masternode.rs`, writing
/// `masternodestate/masternodestate.json`) is implemented by `DiskStorageManager`
/// but has **zero callers outside the storage module** — it is dead code. The
/// legacy sync engine used to call it. PR #411 (commit 12abdc2f) rewrote sync
/// without wiring persistence into the new `MasternodesManager`, which holds only
/// `header_storage` and therefore has no way to persist; PR #414 (commit 245a330d)
/// then deleted the legacy module along with the last remaining callers.
/// `DashSpvClient::new` compounds this by always building a fresh
/// `MasternodeListEngine::default_for_network(..)` (`client/lifecycle.rs`) instead
/// of restoring one.
///
/// User-facing consequence, observed in DashWallet iOS on dash-spv 0.45.0: headers
/// and filters restore instantly from disk while the masternode list restarts at
/// zero, so every launch logs `Requesting QRInfo for tip at height X with 0 base
/// hash(es)` and `QRInfo processed: stored_cycle_height=None`. Should QRInfo then
/// go unanswered for all `MAX_RETRY_ATTEMPTS`, `sync/masternodes/sync_manager.rs`
/// logs "QRInfo failed on every attempt, skipping masternode sync" and the client
/// finishes the phase with no masternode list at all — despite having fully synced
/// it on a previous run.
///
/// The existing `test_masternode_list_sync_with_restart` cannot catch this: it only
/// compares post-restart sync *progress*, which a full network re-sync satisfies.
///
/// This test asserts what *should* happen (state restored from disk) and therefore
/// FAILS on current `dev`.
#[tokio::test]
async fn test_masternode_restart_loses_state() {
    let Some(ctx) = TestContext::new(true).await else {
        return;
    };

    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    // === First run: sync the masternode list to completion ===
    tracing::info!("=== First run: syncing masternode list ===");
    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;
    let first_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(
        first_progress.state(),
        SyncState::Synced,
        "First run must reach Synced before the restart can prove anything"
    );

    let (first_mn_count, first_list_heights, first_rotated_cycles) = {
        let engine = client_handle.engine.read().await;
        let latest =
            engine.latest_masternode_list().expect("First run should have a masternode list");
        assert!(
            !latest.masternodes.is_empty(),
            "First run masternode list must be non-empty for this test to mean anything"
        );
        (
            latest.masternodes.len(),
            engine.masternode_lists.keys().copied().collect::<Vec<u32>>(),
            engine.rotated_quorums_per_cycle.len(),
        )
    };
    tracing::info!(
        "First run synced: {} masternodes, mn_list heights {:?}, {} rotated cycles, \
         diffs_processed={}",
        first_mn_count,
        first_list_heights,
        first_rotated_cycles,
        first_progress.diffs_processed(),
    );

    // Clean shutdown: everything that is going to be persisted is persisted here.
    client_handle.stop().await;
    let first_header_height =
        client_handle.client.sync_progress().await.headers().unwrap().current_height();
    drop(client_handle);

    // === Evidence A: nothing on disk holds the masternode state ===
    let storage_dir = ctx.storage_path();
    let storage_entries = list_storage_entries(storage_dir);
    let mn_state_file = storage_dir.join("masternodestate").join("masternodestate.json");
    let mn_state_persisted = mn_state_file.exists();
    tracing::info!(
        "Storage dir {} after clean shutdown contains {:?}; masternodestate.json exists = {}",
        storage_dir.display(),
        storage_entries,
        mn_state_persisted,
    );

    // === Evidence B: a fresh client on the same storage restores nothing ===
    // Constructed but deliberately NOT run, so the observation is made strictly
    // before any network traffic. Header state, by contrast, is read from storage
    // during construction and acts as the control: it proves the shutdown really
    // did persist and that this directory really is the same one.
    tracing::info!("=== Second run: constructing a client on the same storage, no network ===");
    let mut second_handle = create_client(&config, Arc::clone(&wallet)).await;
    let restored_header_height =
        second_handle.client.sync_progress().await.headers().unwrap().current_height();
    let (restored_mn_count, restored_list_heights) = {
        let engine = second_handle.engine.read().await;
        (
            engine.latest_masternode_list().map(|l| l.masternodes.len()).unwrap_or(0),
            engine.masternode_lists.keys().copied().collect::<Vec<u32>>(),
        )
    };
    second_handle.stop().await;
    drop(second_handle);

    tracing::info!(
        "Second run before any network work: header height {} (first run: {}), \
         masternodes {} (first run: {}), mn_list heights {:?}",
        restored_header_height,
        first_header_height,
        restored_mn_count,
        first_mn_count,
        restored_list_heights,
    );

    // Control: headers *do* survive. If this fails the test setup is wrong (wrong
    // storage dir, shutdown never flushed) and the masternode assertions below
    // would be meaningless.
    assert!(
        restored_header_height > 0 && restored_header_height == first_header_height,
        "Control failed: header state should restore from the same storage dir \
         (first run {}, second run {}). Without this the masternode findings prove nothing.",
        first_header_height,
        restored_header_height,
    );

    // The defect. Asserted together so one run reports both facts.
    assert!(
        mn_state_persisted && restored_mn_count == first_mn_count,
        "Masternode list state is not persisted across restart (dash-spv #954).\n\
         \n\
         After a clean shutdown of a fully-synced client ({} masternodes at heights {:?}, \
         {} rotated cycles):\n\
         - {} exists: {} (expected true)\n\
         - storage dir contains: {:?}\n\
         - a fresh client on the same dir restores {} masternodes at heights {:?} \
           (expected {})\n\
         - headers, by contrast, restored to height {} from that same dir\n\
         \n\
         `store_masternode_state`/`load_masternode_state` are never called: PR #411 \
         (12abdc2f) rewrote sync without wiring persistence into `MasternodesManager`, \
         PR #414 (245a330d) removed the legacy callers, and `DashSpvClient::new` always \
         builds a fresh engine. The second run must therefore re-request the whole list \
         from the network (`Requesting QRInfo for tip at height {} with 0 base hash(es)`), \
         and gets nothing at all if those requests go unanswered.",
        first_mn_count,
        first_list_heights,
        first_rotated_cycles,
        mn_state_file.display(),
        mn_state_persisted,
        storage_entries,
        restored_mn_count,
        restored_list_heights,
        first_mn_count,
        restored_header_height,
        restored_header_height,
    );
}

/// The user-facing outcome of the missing persistence: a client that fully synced
/// its masternode list on a previous run comes back up with **no list at all** when
/// the network is unavailable, even though headers restore from disk untouched.
///
/// Nothing about the masternode list is recoverable offline, so this is not a
/// transient network problem — the state was simply never written. See
/// `test_masternode_restart_loses_state` for the mechanism and the #411/#414 history.
///
/// Deliberately does *not* try to reach the "QRInfo failed on every attempt,
/// skipping masternode sync" branch: that needs the full retry ladder
/// (`QRINFO_TIMEOUT_SCHEDULE_SECS = [10, 30, 60]`, so ~100s of wall clock) to
/// elapse. What matters here is only that the list is absent while the peer is
/// unreachable, which is observable immediately.
///
/// FAILS on current `dev`.
#[tokio::test]
async fn test_masternode_state_unrecoverable_offline_after_restart() {
    let Some(ctx) = TestContext::new(true).await else {
        return;
    };

    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    // First run: sync fully against the live controller.
    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;
    let first_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(first_progress.state(), SyncState::Synced);
    let first_mn_count = {
        let engine = client_handle.engine.read().await;
        engine.latest_masternode_list().map(|l| l.masternodes.len()).unwrap_or(0)
    };
    assert!(first_mn_count > 0, "First run must sync a non-empty masternode list");
    client_handle.stop().await;
    drop(client_handle);
    tracing::info!("First run synced {} masternodes, now taking the peer offline", first_mn_count);

    // Take the peer's p2p away, then restart. Any masternode list the second run
    // ends up with can only have come from disk.
    ctx.mn_ctx.controller.set_network_active(false);

    let mut second_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;
    // Give the run loop room to restore state and to fail at reaching the peer.
    time::sleep(Duration::from_secs(10)).await;
    let (offline_mn_count, offline_header_height, offline_mn_progress) = {
        let engine = second_handle.engine.read().await;
        let count = engine.latest_masternode_list().map(|l| l.masternodes.len()).unwrap_or(0);
        let progress = second_handle.client.sync_progress().await;
        let height = progress.headers().unwrap().current_height();
        (count, height, progress.masternodes().unwrap().clone())
    };
    second_handle.stop().await;
    // Restore p2p before the assertions below, which are expected to panic.
    ctx.mn_ctx.controller.set_network_active(true);

    tracing::info!(
        "Offline restart: {} masternodes, header height {}, masternode progress {:?}",
        offline_mn_count,
        offline_header_height,
        offline_mn_progress,
    );

    assert!(
        offline_header_height > 0,
        "Control failed: headers should restore from disk with no network available"
    );
    // Control: the peer really was unreachable. Without this, a `set_network_active(false)`
    // that failed to block the *new* client's outbound connection would let the second run
    // re-sync the list over the network, and the assertion below would pass for entirely
    // the wrong reason — masking the very bug this test exists to prove.
    assert_ne!(
        offline_mn_progress.state(),
        SyncState::Synced,
        "Control failed: masternode sync reached Synced with the peer supposedly offline, \
         so the second run re-fetched the list over the network instead of restoring it. \
         set_network_active(false) did not block the connection; this test proves nothing \
         in that state. Progress: {:?}",
        offline_mn_progress,
    );
    assert_eq!(
        offline_mn_count, first_mn_count,
        "A client that already synced {} masternodes came back up with {} after restart \
         with the peer offline, while headers restored to height {} from the same storage \
         dir. Masternode state is never persisted (dash-spv #954), so it is unrecoverable \
         without the network.",
        first_mn_count, offline_mn_count, offline_header_height,
    );
}

/// Sync to pre-generated height, generate new blocks, verify incremental update.
///
/// Exercises the SPV's incremental masternode-list update path when new headers
/// arrive. Only needs the controller as a peer since this path does not depend
/// on live masternodes, ChainLocks, or IS signing.
#[tokio::test]
async fn test_masternode_list_sync_with_new_blocks() {
    let Some(ctx) = TestContext::new(true).await else {
        return;
    };

    let initial_height = ctx.mn_ctx.expected_height;
    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;

    // Wait for initial masternode sync
    let mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(mn_progress.state(), SyncState::Synced);
    tracing::info!(
        "Initial sync complete at height {}, generating new blocks...",
        mn_progress.current_height()
    );

    // Generate new blocks on the controller
    let blocks_to_generate = 10;
    let addr = ctx.mn_ctx.controller.get_new_address();
    ctx.mn_ctx.controller.generate_blocks(blocks_to_generate, &addr);

    let expected_new_height = initial_height + blocks_to_generate as u32;
    tracing::info!(
        "Generated {} blocks, waiting for SPV update to height {}",
        blocks_to_generate,
        expected_new_height
    );

    // Wait for the SPV client to sync to the expected height
    let updated_height = wait_for_mn_state_event_above(
        &mut client_handle.sync_event_receiver,
        expected_new_height - 1,
        SYNC_TIMEOUT,
    )
    .await;

    assert!(
        updated_height >= expected_new_height,
        "Updated height {} should be >= expected {}",
        updated_height,
        expected_new_height
    );

    tracing::info!(
        "Incremental update verified: initial={}, updated={}",
        initial_height,
        updated_height
    );

    client_handle.stop().await;
}

/// Mine multiple DKG cycles while the SPV client is connected and verify it keeps up.
///
/// Starts the full masternode network, syncs to the pre-generated height, then
/// orchestrates 3 complete DKG cycles (6 phases + commitment each). After each
/// cycle, verifies the SPV client receives a MasternodeStateUpdated event at
/// the new height.
#[tokio::test]
async fn test_masternode_list_sync_with_quorum_rotation() {
    let Some(mut ctx) = TestContext::new(false).await else {
        return;
    };

    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;

    // Wait for initial masternode sync
    let mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(mn_progress.state(), SyncState::Synced);
    let mut last_height = mn_progress.current_height();
    tracing::info!("Initial sync complete at height {}", last_height);

    // Mine 3 DKG cycles and verify the SPV client keeps up after each
    let num_cycles = 3;
    let mut prev_stored_cycles: usize = {
        let engine = client_handle.engine.read().await;
        engine.rotated_quorums_per_cycle.len()
    };
    for cycle in 1..=num_cycles {
        tracing::info!("Starting DKG cycle {}/{}...", cycle, num_cycles);

        // Snapshot the highest stored-cycle boundary height before mining.
        // The new DKG cycle's stored_cycle_height is always pre_dkg_max_cycle +
        // dkg_interval, so waiting for stored_cycle_height > pre_dkg_max_cycle
        // synchronizes precisely on the new cycle rather than on an early
        // Incremental event that fires before the QRInfo window opens.
        let pre_dkg_max_cycle: u32 = {
            let engine = client_handle.engine.read().await;
            engine
                .rotated_quorums_per_cycle
                .keys()
                .filter_map(|h| engine.block_container.get_height(h))
                .max()
                .unwrap_or(0)
        };

        let quorum_hash =
            ctx.mn_ctx.mine_dkg_cycle().unwrap_or_else(|| panic!("DKG cycle {} failed", cycle));

        // Wait for the SPV client to sync the new masternode state.
        // Using wait_for_mn_state_with_stored_cycle_above instead of
        // wait_for_mn_state_event_above to avoid returning on Incremental events
        // that fire before the QRInfo window for the new cycle opens.
        let updated_height = wait_for_mn_state_with_stored_cycle_above(
            &mut client_handle.sync_event_receiver,
            pre_dkg_max_cycle,
            SYNC_TIMEOUT,
        )
        .await;

        assert!(
            updated_height > last_height,
            "Cycle {}: updated height {} should be greater than previous {}",
            cycle,
            updated_height,
            last_height
        );

        // After each successful DKG cycle, the stored rotation cycle count
        // must not shrink and must reach at least `cycle` distinct entries.
        // Then verify every stored rotated quorum is `Verified`.
        let stored_cycles = {
            let engine = client_handle.engine.read().await;
            let stored = engine.rotated_quorums_per_cycle.len();
            assert!(
                stored >= prev_stored_cycles,
                "Cycle {}: rotated_quorums_per_cycle shrank from {} to {}",
                cycle,
                prev_stored_cycles,
                stored
            );
            assert!(
                stored >= cycle as usize,
                "Cycle {}: expected at least {} stored rotation cycles, got {}",
                cycle,
                cycle,
                stored
            );
            assert_all_rotated_quorums_verified(&engine);
            stored
        };
        prev_stored_cycles = stored_cycles;

        tracing::info!(
            "Cycle {}/{} verified: height {} -> {}, quorum={}, stored_cycles={}",
            cycle,
            num_cycles,
            last_height,
            updated_height,
            quorum_hash,
            stored_cycles
        );
        last_height = updated_height;
    }

    client_handle.stop().await;
}

/// Starting the SPV client *after* a freshly-mined DIP0024 cycle forces the
/// initial QRInfo to carry `tip_diff_has_rotating_quorums=true`, and a fresh
/// engine has no prior entries in `rotated_quorums_per_cycle`. Under those
/// conditions `feed_qr_info` exercises both storage paths against cycles
/// never before stored:
///
/// - Current-cycle path stores the tip cycle (the freshly-mined one).
/// - `validate_and_store_previous_cycle_quorums` stores the cycle whose
///   quorums live on `masternode_lists[h]` (post-fix) or on
///   `masternode_lists[h-c]` (pre-fix).
///
/// Regtest DIP0024 ships mn_lists at `[h-4c, h-3c, h-2c, h-c, h]` in the
/// QRInfo. Post-fix the previous-cycle target's reconstruction lands inside
/// that window and succeeds. Pre-fix it lands one cycle deeper than shipped
/// (`h-5c`), `find_rotated_masternodes_for_quorums` returns
/// `RequiredMasternodeListNotPresent`, and `from_validation_error` maps it to
/// `Skipped` — the outer match at `mod.rs:596` silently returns `Ok(())`
/// without storing anything. `rotated_quorums_per_cycle.len()` ends at 1
/// (tip cycle only), not 2.
///
/// This test catches the work-block-pick regression that
/// `test_masternode_list_sync_with_quorum_rotation` can't see because its
/// initial sync runs with tip *before* a mining window (current-cycle path
/// stores the most recent cycle, previous-cycle path lands on the same
/// already-stored cycle → both fix variants converge to `len == 1`).
#[tokio::test]
async fn test_rotated_quorums_stored_when_sync_starts_post_dkg() {
    let Some(mut ctx) = TestContext::new(false).await else {
        return;
    };

    // Mine one full DIP0024 cycle before the SPV client starts. The initial
    // QRInfo will then have tip past the mining window but inside the same
    // DKG cycle, so `mn_list_diff_tip` carries the freshly-mined cycle's
    // rotating commitments.
    let fresh_cycle_quorum_hash = ctx.mn_ctx.mine_dkg_cycle().expect("DKG cycle should succeed");
    tracing::info!(
        "Pre-SPV DKG cycle mined, quorum_hash={}, starting SPV client…",
        fresh_cycle_quorum_hash
    );

    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);
    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;

    let mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(mn_progress.state(), SyncState::Synced);

    {
        let engine = client_handle.engine.read().await;
        let stored = engine.rotated_quorums_per_cycle.len();
        assert!(
            stored >= 2,
            "Initial QRInfo should store both the freshly-mined tip cycle and \
             the previous cycle from `mn_list[h]`; got {} entries. Pre-fix this \
             silently skips the previous cycle because it targets `mn_list[h-c]` \
             whose cycle needs a quarter mn_list deeper than the QRInfo ships.",
            stored
        );
        assert_all_rotated_quorums_verified(&engine);
        let mut heights: Vec<u32> = engine
            .rotated_quorums_per_cycle
            .keys()
            .filter_map(|h| engine.block_container.get_height(h))
            .collect();
        heights.sort_unstable();
        heights.dedup();
        assert!(
            heights.len() >= 2,
            "Stored cycles should map to at least 2 distinct block heights, got {:?}",
            heights
        );
    }

    client_handle.stop().await;
}

/// End-to-end masternode sync test: initial sync, DKG cycle, and ChainLock.
///
/// Starts the full masternode network with rotated quorum verification enabled.
/// After initial sync, validates the masternode list against the pre-generated
/// metadata, mines a new DKG cycle, verifies the SPV client picks up the update,
/// then mines blocks and waits for a ChainLock to propagate.
#[tokio::test]
async fn test_masternode_list_sync_end_to_end() {
    let Some(mut ctx) = TestContext::new(false).await else {
        return;
    };

    let expected_masternodes = ctx.mn_ctx.metadata.masternodes.len();
    let wallet = create_dummy_wallet();
    let config =
        create_mn_test_config(ctx.storage_path().to_path_buf(), ctx.mn_ctx.controller_addr);

    let mut client_handle = create_and_start_client(&config, Arc::clone(&wallet)).await;

    // Wait for initial masternode sync
    let mn_progress =
        wait_for_masternode_sync(&mut client_handle.progress_receiver, SYNC_TIMEOUT).await;
    assert_eq!(mn_progress.state(), SyncState::Synced);
    let initial_height = mn_progress.current_height();
    tracing::info!("Initial sync complete at height {}", initial_height);

    // Validate MN list matches pre-generated metadata
    {
        let engine = client_handle.engine.read().await;

        let latest_list = engine.latest_masternode_list().expect("Should have a masternode list");
        assert_eq!(
            latest_list.masternodes.len(),
            expected_masternodes,
            "Should have {} masternodes, got {}",
            expected_masternodes,
            latest_list.masternodes.len()
        );

        // Verify each pro_tx_hash from metadata is present
        for mn_info in &ctx.mn_ctx.metadata.masternodes {
            let pro_tx_hash: dashcore::ProTxHash =
                mn_info.pro_tx_hash.parse().unwrap_or_else(|e| {
                    panic!("Failed to parse pro_tx_hash {}: {}", mn_info.pro_tx_hash, e)
                });
            assert!(
                latest_list.masternodes.contains_key(&pro_tx_hash),
                "Masternode {} not found in engine's list",
                mn_info.pro_tx_hash
            );
        }

        // Non-rotating quorums (llmq_test, type 100)
        let non_rotating_quorums =
            latest_list.quorums.get(&LLMQType::LlmqtypeTest).map(|q| q.len()).unwrap_or(0);
        assert!(non_rotating_quorums > 0, "Should have llmq_test (type 100) quorums");

        let rotated_quorum_cycles = engine.rotated_quorums_per_cycle.len();
        assert!(rotated_quorum_cycles > 0, "Should have rotated quorum cycles from initial QRInfo");

        // Every quorum in `rotated_quorums_per_cycle` must be Verified.
        // That structure is the authoritative map of validated rotating
        // quorums used for IS lock verification.
        assert_all_rotated_quorums_verified(&engine);
        tracing::info!(
            "All rotated quorums across {} cycles verified",
            engine.rotated_quorums_per_cycle.len()
        );

        // Non-rotating quorums in the latest MN list must be Verified.
        // Older historical quorums (from previous cycles) may remain
        // Unknown in `quorum_statuses` because validation only runs on
        // the latest MN list; that's fine — they're no longer active.
        if let Some(latest_quorums) = latest_list.quorums.get(&LLMQType::LlmqtypeTest) {
            for (quorum_hash, entry) in latest_quorums {
                assert!(
                    matches!(entry.verified, LLMQEntryVerificationStatus::Verified),
                    "Non-rotating quorum {} in latest MN list should be Verified, got {}",
                    quorum_hash,
                    entry.verified
                );
            }
        }

        tracing::info!(
            "Validated: {} masternodes, {} non-rotating quorums, {} rotated quorum cycles",
            latest_list.masternodes.len(),
            non_rotating_quorums,
            rotated_quorum_cycles,
        );
    }

    // Snapshot rotated_quorums_per_cycle before the DKG cycle so we can
    // assert the count grew once the SPV has fully validated the new cycle.
    let prev_stored_cycles = {
        let engine = client_handle.engine.read().await;
        engine.rotated_quorums_per_cycle.len()
    };

    // Mine a DKG cycle and verify the SPV client picks up the update
    tracing::info!("Mining DKG cycle...");
    let _quorum_hash = ctx.mn_ctx.mine_dkg_cycle().expect("DKG cycle should succeed");

    // Wait for the newly mined DKG cycle to be fully stored and verified.
    // The QRInfo for this cycle emits stored_cycle_height == initial_height (408 in
    // regtest), so we gate on >= initial_height by passing initial_height - 1 as the
    // lower bound. Using wait_for_mn_state_event_above here would consume this event
    // before the stored-cycle check could see it, so we combine both waits into one.
    // Bump mocktime to nudge the tick handler in case a catch-up QRInfo is pending.
    ctx.mn_ctx.bump_mocktime(30);
    let updated_height = wait_for_mn_state_with_stored_cycle_above(
        &mut client_handle.sync_event_receiver,
        initial_height.saturating_sub(1),
        SYNC_TIMEOUT,
    )
    .await;
    assert!(
        updated_height > initial_height,
        "Post-DKG height {} should be greater than initial {}",
        updated_height,
        initial_height
    );

    // Verify engine has masternode list at the new height with new cycle stored
    {
        let engine = client_handle.engine.read().await;
        let latest_list = engine.latest_masternode_list().expect("Should have a masternode list");
        assert_eq!(
            latest_list.masternodes.len(),
            expected_masternodes,
            "MN count should remain {} after DKG",
            expected_masternodes
        );
        let stored = engine.rotated_quorums_per_cycle.len();
        assert!(
            stored > prev_stored_cycles,
            "Expected rotated_quorums_per_cycle to grow by at least 1 after DKG: \
             prev={}, got={}",
            prev_stored_cycles,
            stored
        );
        tracing::info!(
            "Post-DKG rotated quorum cycles: prev={}, now={}",
            prev_stored_cycles,
            stored
        );
    }
    tracing::info!("Post-DKG verified at height {}", updated_height);

    // Mine blocks and wait for ChainLock — required, not optional.
    // After a completed DKG cycle, the llmq_test quorum should be signing ChainLocks.
    tracing::info!("Mining blocks and waiting for ChainLock...");
    let cl_height = ctx
        .mn_ctx
        .mine_blocks_and_wait_for_chainlock(3, 60)
        .expect("ChainLock should be produced after DKG cycle completion");

    tracing::info!("ChainLock received at height {}", cl_height);

    // Wait for the SPV ChainLock manager to validate the new ChainLock.
    let cl_sync_height = wait_for_chainlock_height_at_least(
        &mut client_handle.progress_receiver,
        cl_height,
        SYNC_TIMEOUT,
    )
    .await;
    assert!(
        cl_sync_height >= cl_height,
        "SPV should sync to at least ChainLock height {}, got {}",
        cl_height,
        cl_sync_height
    );
    tracing::info!("SPV synced to ChainLocked height {}", cl_sync_height);

    client_handle.stop().await;
}

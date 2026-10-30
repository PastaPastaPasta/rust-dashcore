//! The masternode sync, fulfilled-request and meta-cache entries (ABI 30, BL-451): the shim's
//! refusals (`oracle.h` "masternode sync") and Core's behaviour where its sources fix the values:
//! the asset order and names (`masternode/sync.cpp`), regtest's 5-minute
//! `nFulfilledRequestExpireTime` (`chainparams.cpp`), the mixing threshold
//! (`masternode/meta.cpp`) and `CFlatDB`'s read results (`flat-database.h`). rdashd's side of each
//! is node-l1's `l1_mnsync`.

use dash_oracle_sys::addrman::{NetAddr, Service};
use dash_oracle_sys::{
    Chain, ErrorKind, FlatFile, FulfilledOp, MetaOp, MetaOutput, Oracle, Pin, ReadResult,
    SyncBlock, SyncOp,
};

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

fn regtest() -> Chain {
    Chain::new("regtest", &[])
}

#[test]
fn sync_walks_the_assets() {
    let states = oracle()
        .mnsync_step(
            &regtest(),
            1000,
            &[
                SyncOp::Time(1010),
                SyncOp::SwitchToNextAsset,
                SyncOp::BumpAttempt,
                SyncOp::Time(1020),
                SyncOp::SwitchToNextAsset,
                SyncOp::Reset {
                    force: false,
                    notify: true,
                },
            ],
        )
        .unwrap();
    let assets: Vec<_> = states.iter().map(|s| (s.asset, s.asset_name.as_str())).collect();
    assert_eq!(
        assets,
        [
            (1, "MASTERNODE_SYNC_BLOCKCHAIN"),
            (4, "MASTERNODE_SYNC_GOVERNANCE"),
            (4, "MASTERNODE_SYNC_GOVERNANCE"),
            (999, "MASTERNODE_SYNC_FINISHED"),
            (1, "MASTERNODE_SYNC_BLOCKCHAIN"),
        ]
    );
    assert_eq!((states[0].asset_start_time, states[0].last_bump), (1000, 1000));
    assert_eq!(
        (states[1].asset_start_time, states[1].last_bump, states[1].attempt),
        (1010, 1010, 0)
    );
    assert_eq!(states[2].attempt, 1);
    assert!(states[3].synced && states[3].blockchain_synced);
    assert_eq!((states[3].finishes, states[3].resets), (1, 0));
    // No tip update yet (nTimeLastUpdateBlockTip 0): an unforced reset goes ahead.
    assert_eq!((states[4].resets, states[4].asset_start_time), (1, 1020));
}

#[test]
fn sync_tip_updates_track_the_best_header() {
    let a = SyncBlock {
        height: 5,
        hash: [1; 32],
    };
    let b = SyncBlock {
        height: 6,
        hash: [2; 32],
    };
    let tip = |tip, new| SyncOp::UpdatedBlockTip {
        tip: Some(tip),
        new,
        initial_download: false,
    };
    let states = oracle()
        .mnsync_step(
            &regtest(),
            1000,
            &[
                tip(a, a),
                tip(b, a),
                SyncOp::NotifyHeaderTip {
                    block: None,
                    initial_download: true,
                },
            ],
        )
        .unwrap();
    assert!(states[1].reached_best_header);
    // Reached before and not now: Reset(true) (sync.cpp UpdatedBlockTip).
    assert!(!states[2].reached_best_header);
    assert_eq!(states[2].resets, 1);
    assert_eq!(states[3].code, 3);
}

#[test]
fn scripts_are_refused_whole() {
    let o = oracle();
    let chain = regtest();
    for script in [
        &[4u8, 2, 0][..], // RESET with a flag byte 2
        &[8],             // an unknown op
        &[0, 1, 2],       // a truncated TIME
    ] {
        let err = o.mnsync_step_raw(&chain, 1000, script).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{script:?}");
    }
    let err = o.mnsync_step(&chain, 0, &[]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument);
    let err = o.mnsync_step(&chain, 1000, &[SyncOp::Time(1 << 34)]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument);
    let err = o.mncache_step(&chain, &[MetaOp::RemoveUsed(u64::MAX)]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument);
    let err =
        o.mncache_step(&chain, &[MetaOp::SetPlatformBan([0; 32], vec![1, 2, 3])]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument);
}

#[test]
fn fulfilled_requests_expire_after_five_minutes_on_regtest() {
    let peer = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899);
    let has = || FulfilledOp::Has(peer.clone(), "full-sync".into());
    let run = oracle()
        .netfulfilled_step(
            &regtest(),
            1000,
            &[
                FulfilledOp::Add(peer.clone(), "full-sync".into()),
                FulfilledOp::Time(1299),
                has(),
                FulfilledOp::Time(1300),
                has(),
                FulfilledOp::CheckAndRemove,
            ],
        )
        .unwrap();
    // Expiry 1300: held while expiry > now; CheckAndRemove keeps it until now > expiry.
    assert_eq!(run.has, [true, false]);
    assert_ne!(run.store, [0]);
    let run = oracle()
        .netfulfilled_step(
            &regtest(),
            1000,
            &[
                FulfilledOp::Add(peer.clone(), "full-sync".into()),
                FulfilledOp::Time(1301),
                FulfilledOp::DoMaintenance,
            ],
        )
        .unwrap();
    assert_eq!(run.store, [0]);
}

#[test]
fn mixing_threshold_is_a_fifth_of_the_list() {
    let a = [7; 32];
    let run = oracle()
        .mncache_step(
            &regtest(),
            &[
                MetaOp::MixingThreshold(a, 5),
                MetaOp::AllowMixing(a),
                MetaOp::MixingThreshold(a, 5),
                MetaOp::MixingThreshold(a, 4),
                MetaOp::AddUsed(a),
                MetaOp::AddUsed(a),
                MetaOp::UsedCount,
                MetaOp::GetPlatformBan([9; 32]),
            ],
        )
        .unwrap();
    // last_dsq 0: never exceeded; then last_dsq 1, nDsqCount 1: 1 + 5/5 > 1, 1 + 4/5 > 1 no.
    assert_eq!(
        run.outputs,
        [
            MetaOutput::Flag(false),
            MetaOutput::Flag(true),
            MetaOutput::Flag(false),
            MetaOutput::Count(1),
            MetaOutput::PlatformBan(None),
        ]
    );
    let version = b"CMasternodeMetaMan-Version-5";
    assert_eq!(&run.store[1..1 + version.len()], version);
}

#[test]
fn flat_files_load_as_cflatdb_reads_them() {
    let o = oracle();
    let chain = regtest();
    let missing = o.flatdb_load(&chain, FlatFile::NetFulfilled, 1000, None).unwrap();
    assert_eq!((missing.read, missing.loaded), (ReadResult::FileError, true));
    // The destructor stored the empty manager.
    let written = missing.file_after.unwrap();
    let again = o.flatdb_load(&chain, FlatFile::NetFulfilled, 1000, Some(&written)).unwrap();
    assert_eq!((again.read, again.loaded), (ReadResult::Ok, true));
    assert_eq!(again.file_after.as_deref(), Some(&written[..]));
    let short = o.flatdb_load(&chain, FlatFile::MnCache, 1000, Some(&[1, 2, 3])).unwrap();
    assert_eq!((short.read, short.loaded), (ReadResult::HashReadError, false));
    assert_eq!(short.file_after.as_deref(), Some(&[1u8, 2, 3][..]));
    // The netfulfilled file read as mncache: right hash, wrong magic message.
    let other = o.flatdb_load(&chain, FlatFile::MnCache, 1000, Some(&written)).unwrap();
    assert_eq!((other.read, other.loaded), (ReadResult::IncorrectMagicMessage, false));
    let mut corrupt = written.clone();
    corrupt[0] ^= 1;
    let corrupt = o.flatdb_load(&chain, FlatFile::NetFulfilled, 1000, Some(&corrupt)).unwrap();
    assert_eq!(corrupt.read, ReadResult::IncorrectHash);
    let err = o.flatdb_load(&chain, FlatFile::NetFulfilled, 0, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument);
}

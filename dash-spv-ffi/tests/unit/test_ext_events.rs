#[cfg(test)]
mod tests {
    use crate::client::EXT_MIN_SIZE;
    use crate::events::FfiEventSink;
    use crate::*;
    use dash_network::ffi::FFINetwork;
    use dash_spv::network::NetworkEvent;
    use dash_spv::{EventHandler, SyncEvent, SyncProgress};
    use key_wallet_manager::WalletEvent;
    use std::ffi::{c_char, c_void, CString};
    use std::ptr::{self, addr_of_mut};
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    const GARBAGE: u8 = 0xAB;
    const SUCCESS: i32 = FFIErrorCode::Success as i32;
    const INVALID: i32 = FFIErrorCode::InvalidArgument as i32;

    #[derive(Default)]
    struct Recorder {
        resynced: AtomicU32,
        reorgs: AtomicU32,
        last_reorg: [AtomicU32; 3],
        headers_stored: AtomicU32,
        peers_updated: AtomicU32,
        progress: AtomicU32,
        sync_height_advanced: AtomicU32,
        errors: AtomicU32,
    }

    impl Recorder {
        fn as_user_data(&self) -> *mut c_void {
            self as *const Self as *mut c_void
        }
    }

    fn recorder<'a>(user_data: *mut c_void) -> &'a Recorder {
        unsafe { &*(user_data as *const Recorder) }
    }

    extern "C" fn on_resynced(_bus: FFIEventBus, _missed: u64, user_data: *mut c_void) {
        recorder(user_data).resynced.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_reorg(fork: u32, disconnected: u32, connected: u32, user_data: *mut c_void) {
        let r = recorder(user_data);
        for (slot, value) in r.last_reorg.iter().zip([fork, disconnected, connected]) {
            slot.store(value, Ordering::SeqCst);
        }
        r.reorgs.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_headers_stored(_tip: u32, user_data: *mut c_void) {
        recorder(user_data).headers_stored.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_peers_updated(_count: u32, _best: u32, user_data: *mut c_void) {
        recorder(user_data).peers_updated.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_progress(_progress: *const FFISyncProgress, user_data: *mut c_void) {
        recorder(user_data).progress.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_sync_height_advanced(
        _wallet_id: *const c_char,
        _height: u32,
        user_data: *mut c_void,
    ) {
        recorder(user_data).sync_height_advanced.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_error(_error: *const c_char, user_data: *mut c_void) {
        recorder(user_data).errors.fetch_add(1, Ordering::SeqCst);
    }

    struct Client {
        ptr: *mut FFIDashSpvClient,
        _dir: TempDir,
    }

    impl Client {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            unsafe {
                let config = dash_spv_ffi_config_new(FFINetwork::Regtest);
                let path = CString::new(dir.path().to_str().unwrap()).unwrap();
                assert_eq!(dash_spv_ffi_config_set_data_dir(config, path.as_ptr()), 0);
                let ptr = dash_spv_ffi_client_new(config, FFIEventCallbacks::default());
                dash_spv_ffi_config_destroy(config);
                assert!(!ptr.is_null());
                Self {
                    ptr,
                    _dir: dir,
                }
            }
        }

        fn sink(&self) -> &FfiEventSink {
            unsafe { &(*self.ptr).sink }
        }

        fn set(&self, table: *const FFIExtEventCallbacks) -> i32 {
            unsafe { dash_spv_ffi_client_set_ext_event_callbacks(self.ptr, table) }
        }

        fn set_table(&self, table: &FFIExtEventCallbacks) -> i32 {
            self.set(table)
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            unsafe { dash_spv_ffi_client_destroy(self.ptr) };
        }
    }

    /// A caller's table as raw bytes: `len` bytes of garbage with the v1 fields written over
    /// the start, so a field the library must not read is a non-null pointer.
    struct RawTable {
        words: Vec<usize>,
    }

    impl RawTable {
        fn new(len: usize, fields: FFIExtEventCallbacks) -> Self {
            let bytes = len.max(size_of::<FFIExtEventCallbacks>());
            let mut words = vec![0; bytes.div_ceil(size_of::<usize>())];
            let table = words.as_mut_ptr() as *mut FFIExtEventCallbacks;
            unsafe {
                ptr::write_bytes(table as *mut u8, GARBAGE, bytes);
                ptr::copy_nonoverlapping(
                    &fields as *const FFIExtEventCallbacks as *const u8,
                    table as *mut u8,
                    len.min(size_of::<FFIExtEventCallbacks>()),
                );
                addr_of_mut!((*table).size).write(len);
            }
            Self {
                words,
            }
        }

        fn as_ptr(&self) -> *const FFIExtEventCallbacks {
            self.words.as_ptr() as *const FFIExtEventCallbacks
        }
    }

    fn full_table(recorder: &Recorder) -> FFIExtEventCallbacks {
        FFIExtEventCallbacks {
            user_data: recorder.as_user_data(),
            on_events_resynced: Some(on_resynced),
            on_chain_reorganized: Some(on_reorg),
            ..FFIExtEventCallbacks::default()
        }
    }

    fn no_callback_set(table: &FFIExtEventCallbacks) -> bool {
        table.on_events_resynced.is_none()
            && table.on_chain_reorganized.is_none()
            && table.on_fail_stop.is_none()
            && table.on_subscriber_stalled.is_none()
            && table.on_pinned_peer_failing.is_none()
    }

    #[test]
    fn ext_size_min_accepted() {
        let client = Client::new();
        let rec = Recorder::default();
        let raw = RawTable::new(EXT_MIN_SIZE, full_table(&rec));
        assert_eq!(
            unsafe { dash_spv_ffi_client_set_ext_event_callbacks(ptr::null_mut(), raw.as_ptr()) },
            FFIErrorCode::NullPointer as i32
        );

        assert_eq!(client.set(raw.as_ptr()), SUCCESS);
        let tables = client.sink().ext();
        let ext = &tables.ext;
        assert_eq!(ext.size, size_of::<FFIExtEventCallbacks>());
        assert_eq!(ext.user_data, rec.as_user_data());
        assert!(ext.on_events_resynced.is_some());
        assert!(ext.on_chain_reorganized.is_none());
        assert!(ext.on_fail_stop.is_none());
        assert!(ext.on_subscriber_stalled.is_none());
        assert!(ext.on_pinned_peer_failing.is_none());

        client.sink().chain_reorganized(10, 1, 2);
        assert_eq!(rec.reorgs.load(Ordering::SeqCst), 0);
    }

    /// The client holds the only other reference to its stored sink: the handler it dispatches
    /// through.
    #[test]
    fn stored_sink_is_the_dispatched_handler() {
        let client = Client::new();
        assert_eq!(Arc::strong_count(unsafe { &(*client.ptr).sink }), 2);
    }

    #[test]
    fn ext_size_min_minus_one_rejected() {
        let client = Client::new();
        let rec = Recorder::default();
        for size in [EXT_MIN_SIZE - 1, EXT_MIN_SIZE - size_of::<usize>()] {
            let raw = RawTable::new(size, full_table(&rec));
            assert_eq!(client.set(raw.as_ptr()), INVALID, "size {size}");
            assert!(no_callback_set(&client.sink().ext().ext));
        }
    }

    #[test]
    fn ext_size_partial_field_rejected() {
        let client = Client::new();
        let active = Recorder::default();
        assert_eq!(client.set_table(&full_table(&active)), SUCCESS);

        let other = Recorder::default();
        let raw = RawTable::new(EXT_MIN_SIZE + 4, full_table(&other));
        assert_eq!(client.set(raw.as_ptr()), INVALID);

        client.sink().chain_reorganized(10, 1, 2);
        assert_eq!(active.reorgs.load(Ordering::SeqCst), 1);
        assert_eq!(active.last_reorg.each_ref().map(|v| v.load(Ordering::SeqCst)), [10, 1, 2]);
        assert_eq!(active.resynced.load(Ordering::SeqCst), 0);
        assert_eq!(other.reorgs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ext_version_zero_rejected() {
        let client = Client::new();
        let rec = Recorder::default();
        let table = FFIExtEventCallbacks {
            version: 0,
            ..full_table(&rec)
        };
        assert_eq!(client.set_table(&table), INVALID);
        assert!(no_callback_set(&client.sink().ext().ext));
    }

    #[test]
    fn ext_version_two_larger_size_reads_v1_fields() {
        let client = Client::new();
        let rec = Recorder::default();
        let table = FFIExtEventCallbacks {
            version: 2,
            ..full_table(&rec)
        };
        let raw = RawTable::new(size_of::<FFIExtEventCallbacks>() + 16, table);
        assert_eq!(client.set(raw.as_ptr()), SUCCESS);

        let tables = client.sink().ext();
        assert_eq!(tables.ext.size, size_of::<FFIExtEventCallbacks>());
        assert!(tables.ext.on_events_resynced.is_some());
        assert!(tables.ext.on_fail_stop.is_none());
        client.sink().chain_reorganized(7, 2, 3);
        assert_eq!(rec.reorgs.load(Ordering::SeqCst), 1);
        assert_eq!(rec.last_reorg.each_ref().map(|v| v.load(Ordering::SeqCst)), [7, 2, 3]);
    }

    #[test]
    fn ext_null_table_clears() {
        let client = Client::new();
        let rec = Recorder::default();
        assert_eq!(client.set_table(&full_table(&rec)), SUCCESS);
        assert_eq!(client.set(ptr::null()), SUCCESS);
        assert!(no_callback_set(&client.sink().ext().ext));
        client.sink().chain_reorganized(10, 1, 2);
        assert_eq!(rec.reorgs.load(Ordering::SeqCst), 0);
    }

    struct Reentry {
        client: *mut FFIDashSpvClient,
        next: FFIExtEventCallbacks,
        setter_result: AtomicI32,
    }

    extern "C" fn reorg_sets_next_table(_f: u32, _d: u32, _c: u32, user_data: *mut c_void) {
        let reentry = unsafe { &*(user_data as *const Reentry) };
        let result =
            unsafe { dash_spv_ffi_client_set_ext_event_callbacks(reentry.client, &reentry.next) };
        reentry.setter_result.store(result, Ordering::SeqCst);
    }

    #[test]
    fn ext_setter_from_inside_callback() {
        let client = Client::new();
        let next = Box::leak(Box::new(Recorder::default()));
        let reentry = Box::leak(Box::new(Reentry {
            client: client.ptr,
            next: full_table(next),
            setter_result: AtomicI32::new(-1),
        }));
        let table = FFIExtEventCallbacks {
            user_data: reentry as *mut Reentry as *mut c_void,
            on_chain_reorganized: Some(reorg_sets_next_table),
            ..FFIExtEventCallbacks::default()
        };
        assert_eq!(client.set_table(&table), SUCCESS);

        let (done, returned) = mpsc::channel();
        let sink = client.sink() as *const FfiEventSink as usize;
        thread::spawn(move || {
            let sink = unsafe { &*(sink as *const FfiEventSink) };
            sink.chain_reorganized(10, 1, 2);
            done.send(()).unwrap();
        });
        returned
            .recv_timeout(Duration::from_secs(10))
            .expect("a callback that sets the table must return");
        assert_eq!(reentry.setter_result.load(Ordering::SeqCst), SUCCESS);

        client.sink().chain_reorganized(11, 1, 3);
        assert_eq!(next.reorgs.load(Ordering::SeqCst), 1);
        assert_eq!(next.last_reorg.each_ref().map(|v| v.load(Ordering::SeqCst)), [11, 1, 3]);
    }

    #[test]
    fn ext_null_callbacks_ignored() {
        let client = Client::new();
        let rec = Recorder::default();
        let table = FFIExtEventCallbacks {
            user_data: rec.as_user_data(),
            ..FFIExtEventCallbacks::default()
        };
        assert_eq!(client.set_table(&table), SUCCESS);
        client.sink().chain_reorganized(10, 1, 2);
        client.sink().on_sync_event(&SyncEvent::BlockHeadersStored {
            tip_height: 5,
        });
        assert_eq!(rec.reorgs.load(Ordering::SeqCst), 0);
        assert_eq!(rec.resynced.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn each_event_delivered_once() {
        let rec = Recorder::default();
        let user_data = rec.as_user_data();
        let mut frozen = FFIEventCallbacks::default();
        frozen.sync.on_block_headers_stored = Some(on_headers_stored);
        frozen.sync.user_data = user_data;
        frozen.network.on_peers_updated = Some(on_peers_updated);
        frozen.network.user_data = user_data;
        frozen.progress.on_progress = Some(on_progress);
        frozen.progress.user_data = user_data;
        frozen.wallet.on_sync_height_advanced = Some(on_sync_height_advanced);
        frozen.wallet.user_data = user_data;
        frozen.error.on_error = Some(on_error);
        frozen.error.user_data = user_data;
        let sink = FfiEventSink::new(frozen);
        sink.set_ext(full_table(&rec));

        sink.on_sync_event(&SyncEvent::BlockHeadersStored {
            tip_height: 5,
        });
        sink.on_network_event(&NetworkEvent::PeersUpdated {
            addresses: Vec::new(),
            best_height: 5,
        });
        sink.on_progress(&SyncProgress::default());
        sink.on_wallet_event(&WalletEvent::SyncHeightAdvanced {
            wallet_id: [1; 32],
            height: 5,
        });
        sink.on_error("boom");
        sink.chain_reorganized(4, 1, 2);

        for (name, count) in [
            ("headers_stored", &rec.headers_stored),
            ("peers_updated", &rec.peers_updated),
            ("progress", &rec.progress),
            ("sync_height_advanced", &rec.sync_height_advanced),
            ("errors", &rec.errors),
            ("reorgs", &rec.reorgs),
        ] {
            assert_eq!(count.load(Ordering::SeqCst), 1, "{name}");
        }
        assert_eq!(rec.resynced.load(Ordering::SeqCst), 0);
    }
}

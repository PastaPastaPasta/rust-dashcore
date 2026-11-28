#[cfg(test)]
mod tests {
    use crate::client::MASTERNODE_MIN_SIZE;
    use crate::events::FfiEventSink;
    use crate::*;
    use dash_network::ffi::FFINetwork;
    use dash_spv::{EventHandler, InitialSyncPhase, MnDiagnostic, SyncEvent};
    use dash_types::Numeric;
    use std::ffi::{c_void, CString};
    use std::ptr::{self, addr_of_mut};
    use std::sync::Mutex;
    use tempfile::TempDir;

    const GARBAGE: u8 = 0xAB;
    const SUCCESS: i32 = FFIErrorCode::Success as i32;
    const INVALID: i32 = FFIErrorCode::InvalidArgument as i32;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Miss {
            llmq_type: u8,
            quorum_hash: [u8; 32],
            anchor: u32,
            has_missing: bool,
            missing: u32,
        },
        Phase(FFIInitialSyncPhase),
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<Call>>,
    }

    impl Recorder {
        fn as_user_data(&self) -> *mut c_void {
            self as *const Self as *mut c_void
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn record(user_data: *mut c_void, call: Call) {
        let recorder = unsafe { &*(user_data as *const Recorder) };
        recorder.calls.lock().unwrap().push(call);
    }

    extern "C" fn on_miss(
        llmq_type: u8,
        quorum_hash: *const [u8; 32],
        anchor: u32,
        has_missing: bool,
        missing: u32,
        user_data: *mut c_void,
    ) {
        let quorum_hash = unsafe { *quorum_hash };
        record(
            user_data,
            Call::Miss {
                llmq_type,
                quorum_hash,
                anchor,
                has_missing,
                missing,
            },
        );
    }

    extern "C" fn on_phase(phase: FFIInitialSyncPhase, user_data: *mut c_void) {
        record(user_data, Call::Phase(phase));
    }

    fn full_table(recorder: &Recorder) -> FFIMasternodeEventCallbacks {
        FFIMasternodeEventCallbacks {
            user_data: recorder.as_user_data(),
            on_coverage_miss: Some(on_miss),
            on_initial_sync_phase: Some(on_phase),
            ..FFIMasternodeEventCallbacks::default()
        }
    }

    fn miss(n: u8, missing: Option<u32>) -> SyncEvent {
        SyncEvent::MasternodeDiagnostic(MnDiagnostic::CoverageMiss {
            llmq_type: Numeric::from_base(106),
            quorum_hash: [n; 32].into(),
            anchor: 1000 + u32::from(n),
            missing,
        })
    }

    fn phase(phase: InitialSyncPhase) -> SyncEvent {
        SyncEvent::MasternodeDiagnostic(MnDiagnostic::InitialSyncPhase {
            phase,
        })
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

        fn set(&self, table: *const FFIMasternodeEventCallbacks) -> i32 {
            unsafe { dash_spv_ffi_client_set_masternode_event_callbacks(self.ptr, table) }
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            unsafe { dash_spv_ffi_client_destroy(self.ptr) };
        }
    }

    /// A caller's table as raw bytes: garbage with the v1 fields written over the first `len`
    /// bytes, so a field the library must not read is a non-null pointer.
    struct RawTable {
        words: Vec<usize>,
    }

    impl RawTable {
        fn new(len: usize, fields: FFIMasternodeEventCallbacks) -> Self {
            let bytes = len.max(size_of::<FFIMasternodeEventCallbacks>());
            let mut words = vec![0; bytes.div_ceil(size_of::<usize>())];
            let table = words.as_mut_ptr() as *mut FFIMasternodeEventCallbacks;
            unsafe {
                ptr::write_bytes(table as *mut u8, GARBAGE, bytes);
                ptr::copy_nonoverlapping(
                    &fields as *const FFIMasternodeEventCallbacks as *const u8,
                    table as *mut u8,
                    len.min(size_of::<FFIMasternodeEventCallbacks>()),
                );
                addr_of_mut!((*table).size).write(len);
            }
            Self {
                words,
            }
        }

        fn as_ptr(&self) -> *const FFIMasternodeEventCallbacks {
            self.words.as_ptr() as *const FFIMasternodeEventCallbacks
        }
    }

    #[test]
    fn smaller_table_reads_only_coverage_miss() {
        let client = Client::new();
        let rec = Recorder::default();
        let raw = RawTable::new(MASTERNODE_MIN_SIZE, full_table(&rec));
        assert_eq!(
            unsafe {
                dash_spv_ffi_client_set_masternode_event_callbacks(ptr::null_mut(), raw.as_ptr())
            },
            FFIErrorCode::NullPointer as i32
        );

        assert_eq!(client.set(raw.as_ptr()), SUCCESS);
        let tables = client.sink().ext();
        assert_eq!(tables.masternode.size, size_of::<FFIMasternodeEventCallbacks>());
        assert_eq!(tables.masternode.user_data, rec.as_user_data());
        assert!(tables.masternode.on_coverage_miss.is_some());
        assert!(tables.masternode.on_initial_sync_phase.is_none());

        client.sink().on_sync_event(&phase(InitialSyncPhase::Established));
        client.sink().on_sync_event(&miss(1, Some(5)));
        assert_eq!(
            rec.calls(),
            [Call::Miss {
                llmq_type: 106,
                quorum_hash: [1; 32],
                anchor: 1001,
                has_missing: true,
                missing: 5,
            }]
        );
    }

    #[test]
    fn rejected_tables_keep_the_old_one() {
        let client = Client::new();
        let active = Recorder::default();
        assert_eq!(client.set(&full_table(&active)), SUCCESS);

        let other = Recorder::default();
        for size in [
            MASTERNODE_MIN_SIZE - 1,
            MASTERNODE_MIN_SIZE - size_of::<usize>(),
            MASTERNODE_MIN_SIZE + size_of::<usize>() / 2,
        ] {
            let raw = RawTable::new(size, full_table(&other));
            assert_eq!(client.set(raw.as_ptr()), INVALID, "size {size}");
        }
        let version_zero = FFIMasternodeEventCallbacks {
            version: 0,
            ..full_table(&other)
        };
        assert_eq!(client.set(&version_zero), INVALID);

        client.sink().on_sync_event(&phase(InitialSyncPhase::Coverage));
        assert_eq!(active.calls(), [Call::Phase(FFIInitialSyncPhase::Coverage)]);
        assert!(other.calls().is_empty());
    }

    #[test]
    fn null_table_resets() {
        let client = Client::new();
        let rec = Recorder::default();
        assert_eq!(client.set(&full_table(&rec)), SUCCESS);
        assert_eq!(client.set(ptr::null()), SUCCESS);
        let tables = client.sink().ext();
        assert!(tables.masternode.on_coverage_miss.is_none());
        assert!(tables.masternode.on_initial_sync_phase.is_none());
        assert!(tables.masternode.user_data.is_null());
        client.sink().on_sync_event(&miss(1, None));
        assert!(rec.calls().is_empty());
    }

    #[test]
    fn null_callbacks_call_nothing() {
        let client = Client::new();
        let rec = Recorder::default();
        let table = FFIMasternodeEventCallbacks {
            user_data: rec.as_user_data(),
            ..FFIMasternodeEventCallbacks::default()
        };
        assert_eq!(client.set(&table), SUCCESS);
        client.sink().on_sync_event(&miss(1, Some(2)));
        client.sink().on_sync_event(&miss(2, None));
        client.sink().on_sync_event(&phase(InitialSyncPhase::Failed));
        assert!(rec.calls().is_empty());

        let sink = FfiEventSink::new(FFIEventCallbacks::default());
        sink.on_sync_event(&miss(3, None));
        sink.on_sync_event(&phase(InitialSyncPhase::Unopened));
    }

    #[test]
    fn each_setter_keeps_the_other_table() {
        let rec = Recorder::default();
        let ext_data = ptr::dangling_mut::<c_void>();
        let sink = FfiEventSink::new(FFIEventCallbacks::default());

        sink.set_masternode(full_table(&rec));
        sink.set_ext(FFIExtEventCallbacks {
            user_data: ext_data,
            ..FFIExtEventCallbacks::default()
        });
        assert_eq!(sink.ext().ext.user_data, ext_data);
        sink.on_sync_event(&miss(1, None));
        assert_eq!(rec.calls().len(), 1);

        sink.set_masternode(FFIMasternodeEventCallbacks::default());
        assert_eq!(sink.ext().ext.user_data, ext_data);
        sink.set_masternode(full_table(&rec));
        assert_eq!(sink.ext().ext.user_data, ext_data);
        sink.on_sync_event(&phase(InitialSyncPhase::Candidate));
        assert_eq!(rec.calls().len(), 2);
    }

    #[test]
    fn one_delivery_per_event() {
        let rec = Recorder::default();
        let sink = FfiEventSink::new(FFIEventCallbacks::default());
        sink.set_masternode(full_table(&rec));

        let phases = [
            (InitialSyncPhase::Unopened, FFIInitialSyncPhase::Unopened),
            (InitialSyncPhase::Candidate, FFIInitialSyncPhase::Candidate),
            (InitialSyncPhase::Coverage, FFIInitialSyncPhase::Coverage),
            (InitialSyncPhase::Establishing, FFIInitialSyncPhase::Establishing),
            (InitialSyncPhase::Established, FFIInitialSyncPhase::Established),
            (InitialSyncPhase::Failed, FFIInitialSyncPhase::Failed),
        ];
        let mut expected = Vec::new();
        for (n, missing) in [(1, Some(10)), (2, None), (3, Some(0))] {
            sink.on_sync_event(&miss(n, missing));
            expected.push(Call::Miss {
                llmq_type: 106,
                quorum_hash: [n; 32],
                anchor: 1000 + u32::from(n),
                has_missing: missing.is_some(),
                missing: missing.unwrap_or(0),
            });
        }
        for (input, output) in phases {
            sink.on_sync_event(&phase(input));
            expected.push(Call::Phase(output));
        }
        sink.on_sync_event(&SyncEvent::BlockHeadersStored {
            tip_height: 5,
        });

        assert_eq!(rec.calls(), expected);
    }
}

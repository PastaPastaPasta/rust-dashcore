#[cfg(test)]
mod tests {
    use crate::*;
    use dash_network::ffi::FFINetwork;
    use serial_test::serial;
    use std::ffi::CStr;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Reads the calling thread's last error as an owned string.
    fn last_error() -> Option<String> {
        let ptr = dash_spv_ffi_get_last_error();
        // SAFETY: a non-null pointer from `dash_spv_ffi_get_last_error` is a
        // NUL-terminated string that stays valid until this thread sets or
        // clears its error again.
        (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
    }

    #[test]
    fn test_concurrent_error_handling() {
        // Every thread sets its error, then waits until all of them have, so each
        // read happens after the other nine writes. Each thread still reads its
        // own. The threads only record what they read (a panic would leave the
        // others waiting at the barrier); the asserts run after the join.
        const THREADS: usize = 10;
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    set_last_error(&format!("Error from thread {}", i));
                    barrier.wait();
                    let after_all_set = last_error();
                    barrier.wait();
                    clear_last_error();
                    barrier.wait();
                    (after_all_set, last_error())
                })
            })
            .collect();

        for (i, handle) in handles.into_iter().enumerate() {
            let (after_all_set, after_clear) = handle.join().unwrap();
            assert_eq!(after_all_set, Some(format!("Error from thread {}", i)));
            assert_eq!(after_clear, None, "thread {}", i);
        }
    }

    #[test]
    fn test_another_threads_error_leaves_this_threads_pointer_valid() {
        // The caller holds the pointer while another thread sets and clears its own
        // errors; the pointer must still read this thread's message afterwards.
        set_last_error("Error from the caller");
        let ptr = dash_spv_ffi_get_last_error();
        assert!(!ptr.is_null());

        thread::spawn(|| {
            assert_eq!(last_error(), None);
            for i in 0..100 {
                set_last_error(&format!("Error from another thread {}", i));
            }
            clear_last_error();
        })
        .join()
        .unwrap();

        assert_eq!(dash_spv_ffi_get_last_error(), ptr);
        // SAFETY: this thread has not set or cleared its error since `ptr` was read.
        let message = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
        assert_eq!(message, "Error from the caller");
        clear_last_error();
    }

    #[test]
    fn test_error_calls_during_thread_teardown_do_not_abort() {
        // A thread-local whose destructor calls the error functions. It is
        // registered before the thread's error slot, so (destructors running in
        // reverse order) it runs after the slot is gone; on a platform that runs
        // them the other way it runs before, and the sequence gives null too.
        struct CallsOnDrop(Option<std::sync::mpsc::Sender<bool>>);
        impl Drop for CallsOnDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    set_last_error("Error from thread teardown");
                    clear_last_error();
                    let _ = sender.send(dash_spv_ffi_get_last_error().is_null());
                }
            }
        }
        thread_local! {
            static CALLS_ON_DROP: std::cell::RefCell<CallsOnDrop> =
                const { std::cell::RefCell::new(CallsOnDrop(None)) };
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            CALLS_ON_DROP.with(|guard| guard.borrow_mut().0 = Some(sender));
            set_last_error("Error before thread exit");
        })
        .join()
        .unwrap();
        // A bound, not `recv()`: a target that never runs thread-local
        // destructors fails here instead of hanging.
        assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(60)), Ok(true));
    }

    #[test]
    #[serial]
    fn test_error_message_truncation() {
        // Test very long error message
        let long_error = "X".repeat(10000);
        set_last_error(&long_error);

        let error_ptr = dash_spv_ffi_get_last_error();
        assert!(!error_ptr.is_null());

        unsafe {
            let error_str = CStr::from_ptr(error_ptr).to_str().unwrap();
            // Should handle long strings without truncation
            assert_eq!(error_str.len(), 10000);
            assert!(error_str.chars().all(|c| c == 'X'));
        }
    }

    #[test]
    fn test_all_error_code_mappings() {
        // Test all error codes have correct values
        assert_eq!(FFIErrorCode::Success as i32, 0);
        assert_eq!(FFIErrorCode::NullPointer as i32, 1);
        assert_eq!(FFIErrorCode::InvalidArgument as i32, 2);
        assert_eq!(FFIErrorCode::NetworkError as i32, 3);
        assert_eq!(FFIErrorCode::StorageError as i32, 4);
        assert_eq!(FFIErrorCode::ValidationError as i32, 5);
        assert_eq!(FFIErrorCode::SyncError as i32, 6);
        assert_eq!(FFIErrorCode::ConfigError as i32, 7);
        assert_eq!(FFIErrorCode::RuntimeError as i32, 8);

        // Test conversions from SpvError
        use dash_spv::{NetworkError, SpvError, StorageError};

        let net_err = SpvError::Network(NetworkError::ConnectionFailed("test".to_string()));
        assert_eq!(FFIErrorCode::from(net_err) as i32, FFIErrorCode::NetworkError as i32);

        let storage_err = SpvError::Storage(StorageError::NotFound("test".to_string()));
        assert_eq!(FFIErrorCode::from(storage_err) as i32, FFIErrorCode::StorageError as i32);

        let config_err = SpvError::Config("test".to_string());
        assert_eq!(FFIErrorCode::from(config_err) as i32, FFIErrorCode::ConfigError as i32);
    }

    #[test]
    #[serial]
    fn test_error_clearing_between_operations() {
        // Set an error
        set_last_error("First error");
        assert!(!dash_spv_ffi_get_last_error().is_null());

        // Clear it
        clear_last_error();
        assert!(dash_spv_ffi_get_last_error().is_null());

        // Set another error
        set_last_error("Second error");
        let error_ptr = dash_spv_ffi_get_last_error();
        assert!(!error_ptr.is_null());

        unsafe {
            let error_str = CStr::from_ptr(error_ptr).to_str().unwrap();
            assert_eq!(error_str, "Second error");
        }

        // Clear using public API
        clear_last_error();
        assert!(dash_spv_ffi_get_last_error().is_null());
    }

    #[test]
    #[serial]
    fn test_null_pointer_error_handling() {
        // Test null_check! macro behavior
        unsafe {
            // Test with config functions
            let result = dash_spv_ffi_config_set_data_dir(std::ptr::null_mut(), std::ptr::null());
            assert_eq!(result, FFIErrorCode::NullPointer as i32);

            // Check error was set
            let error_ptr = dash_spv_ffi_get_last_error();
            assert!(!error_ptr.is_null());
            let error_str = CStr::from_ptr(error_ptr).to_str().unwrap();
            assert_eq!(error_str, "Null pointer provided");
        }
    }

    #[test]
    fn test_invalid_enum_handling() {
        // Use a valid enum value to avoid UB in Rust tests. If invalid raw inputs
        // need to be tested, do so from a C test or add a raw-int FFI entrypoint.
        unsafe {
            let config = dash_spv_ffi_config_new(FFINetwork::Mainnet);
            assert!(!config.is_null());
            dash_spv_ffi_config_destroy(config);
        }
    }

    #[test]
    #[serial]
    fn test_handle_error_helper() {
        // Test Ok case
        let ok_result: Result<i32, String> = Ok(42);
        let handled = handle_error(ok_result);
        assert_eq!(handled, Some(42));
        assert!(dash_spv_ffi_get_last_error().is_null());

        // Test Err case
        let err_result: Result<i32, String> = Err("Test error".to_string());
        let handled = handle_error(err_result);
        assert!(handled.is_none());

        let error_ptr = dash_spv_ffi_get_last_error();
        assert!(!error_ptr.is_null());
        unsafe {
            let error_str = CStr::from_ptr(error_ptr).to_str().unwrap();
            assert_eq!(error_str, "Test error");
        }
    }
}

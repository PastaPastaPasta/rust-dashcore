use dash_spv::error::SpvError;
use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;

thread_local! {
    /// The calling thread's last error, as `errno` is. Each FFI call sets or
    /// clears it on the thread that made the call, so a caller always reads the
    /// error of its own last call, and the string a returned pointer refers to
    /// is only replaced by that same thread, or freed when it exits.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FFIErrorCode {
    Success = 0,
    NullPointer = 1,
    InvalidArgument = 2,
    NetworkError = 3,
    StorageError = 4,
    ValidationError = 5,
    SyncError = 6,
    ConfigError = 7,
    RuntimeError = 8,
}

/// Sets the calling thread's last error.
///
/// During thread teardown, once the slot is destroyed, the error is dropped:
/// `with` would panic there, and the panic would abort the process at the FFI
/// boundary.
pub fn set_last_error(err: &str) {
    let c_err = CString::new(err).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
    let _ = LAST_ERROR.try_with(|slot| *slot.borrow_mut() = Some(c_err));
}

/// Clears the calling thread's last error (a no-op during thread teardown).
pub fn clear_last_error() {
    let _ = LAST_ERROR.try_with(|slot| *slot.borrow_mut() = None);
}

/// Returns the error message of the last failed call made on the calling thread,
/// or null if there is none.
///
/// The error is per thread: a call made on another thread never changes it.
/// The string is owned by the library and must not be freed. It is valid only
/// on the calling thread, until that thread makes another call that sets or
/// clears its error, or exits. Copy it before calling into the library again or
/// passing it to another thread. Returns null during thread teardown, once the
/// thread's error slot is destroyed.
#[no_mangle]
pub extern "C" fn dash_spv_ffi_get_last_error() -> *const c_char {
    LAST_ERROR
        .try_with(|slot| slot.borrow().as_ref().map_or(std::ptr::null(), |err| err.as_ptr()))
        .unwrap_or(std::ptr::null())
}

impl From<SpvError> for FFIErrorCode {
    fn from(err: SpvError) -> Self {
        match err {
            SpvError::ChannelFailure(_, _) => FFIErrorCode::RuntimeError,
            SpvError::Network(_) => FFIErrorCode::NetworkError,
            SpvError::Storage(_) => FFIErrorCode::StorageError,
            SpvError::Sync(_) => FFIErrorCode::SyncError,
            SpvError::Config(_) => FFIErrorCode::ConfigError,
            SpvError::QuorumLookupError(_) => FFIErrorCode::ValidationError,
        }
    }
}

pub fn handle_error<T, E: std::fmt::Display>(result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => {
            clear_last_error();
            Some(value)
        }
        Err(e) => {
            set_last_error(&e.to_string());
            None
        }
    }
}

pub fn handle_error_code<E: std::fmt::Display + Into<FFIErrorCode>>(
    result: Result<(), E>,
) -> FFIErrorCode {
    match result {
        Ok(()) => {
            clear_last_error();
            FFIErrorCode::Success
        }
        Err(e) => {
            set_last_error(&e.to_string());
            e.into()
        }
    }
}

#[macro_export]
macro_rules! ffi_result {
    ($expr:expr) => {
        match $expr {
            Ok(val) => {
                $crate::error::clear_last_error();
                val
            }
            Err(e) => {
                $crate::error::set_last_error(&e.to_string());
                return $crate::error::FFIErrorCode::from(e) as i32;
            }
        }
    };
}

#[macro_export]
macro_rules! null_check {
    ($ptr:expr) => {
        if $ptr.is_null() {
            $crate::error::set_last_error("Null pointer provided");
            return $crate::error::FFIErrorCode::NullPointer as i32;
        }
    };
    ($ptr:expr, $ret:expr) => {
        if $ptr.is_null() {
            $crate::error::set_last_error("Null pointer provided");
            return $ret;
        }
    };
}

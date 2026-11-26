pub use dash_spv_ffi::dash_spv_ffi_version;

#[no_mangle]
pub extern "C" fn combined_sdk_probe_sqlite_version() -> i32 {
    rusqlite::version_number()
}

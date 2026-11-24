//! FNV-1a, 64 bits: a stable digest with no dependency, shared by the build script (`#[path]`)
//! and the crate.

/// `bytes`' digest, as 16 hex digits.
pub fn fnv64(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{h:016x}")
}

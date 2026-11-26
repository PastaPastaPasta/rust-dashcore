use dash_consensus::primitives::OutPoint;

use crate::is::IsLockHash;

pub(super) const LOCK: &[u8] = b"\x04is_i";
pub(super) const TX: &[u8] = b"\x05is_tx";
pub(super) const INPUT: &[u8] = b"\x05is_in";
pub(super) const MINED: &[u8] = b"\x04is_m";
pub(super) const ARCHIVE_HEIGHT: &[u8] = b"\x05is_a1";
pub(super) const ARCHIVE_HASH: &[u8] = b"\x05is_a2";
pub(super) const VERSION: &[u8] = b"\x04is_v";

pub(super) fn hash(prefix: &[u8], hash: &[u8; 32]) -> Vec<u8> {
    [prefix, hash].concat()
}

pub(super) fn input(outpoint: &OutPoint) -> Vec<u8> {
    [INPUT, outpoint.txid().as_bytes(), &outpoint.index().to_le_bytes()].concat()
}

pub(super) fn height(prefix: &[u8], height: i32, hash: &IsLockHash) -> Vec<u8> {
    [prefix, &u32::MAX.wrapping_sub(height as u32).to_be_bytes(), hash].concat()
}

pub(super) fn read_hash(bytes: &[u8]) -> Option<IsLockHash> {
    bytes.get(..32)?.try_into().ok()
}

pub(super) fn read_height(prefix: &[u8], key: &[u8]) -> Option<(u32, IsLockHash)> {
    let bytes = key.strip_prefix(prefix)?;
    let inverse = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?);
    Some((u32::MAX - inverse, read_hash(bytes.get(4..)?)?))
}

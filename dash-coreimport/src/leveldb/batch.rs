use std::path::Path;

use super::{Cursor, MAX_KEY, MAX_SEQUENCE, Value, newest};
use crate::Result;

pub(super) fn lookup(
    data: &[u8],
    path: &Path,
    offset: u64,
    key: &[u8],
    found: &mut Option<Value>,
) -> Result<(u64, u64)> {
    let mut cursor = Cursor::new(data, path, offset);
    let sequence =
        u64::from_le_bytes(cursor.take(8)?.try_into().map_err(|_| cursor.error("batch sequence"))?);
    let count =
        u32::from_le_bytes(cursor.take(4)?.try_into().map_err(|_| cursor.error("batch count"))?);
    if sequence > MAX_SEQUENCE
        || u64::from(count).checked_sub(1).is_some_and(|n| sequence > MAX_SEQUENCE - n)
    {
        return Err(cursor.error("batch sequence overflow"));
    }
    for n in 0..count {
        let kind = cursor.take(1)?[0];
        if kind > 1 {
            return Err(cursor.error("unknown write batch type"));
        }
        let user_key = cursor.slice()?;
        if user_key.len() > MAX_KEY - 8 {
            return Err(cursor.error("batch internal key exceeds 64 KiB"));
        }
        let bytes = if kind == 1 {
            Some(cursor.slice()?)
        } else {
            None
        };
        if user_key == key {
            newest(
                found,
                Value {
                    sequence: sequence + u64::from(n),
                    bytes: bytes.map(<[u8]>::to_vec),
                },
                path,
            )?;
        }
    }
    if !cursor.done() {
        return Err(cursor.error("write batch count mismatch"));
    }
    let last = sequence
        .checked_add(u64::from(count))
        .and_then(|n| n.checked_sub(1))
        .ok_or_else(|| cursor.error("batch sequence range underflow"))?;
    Ok((sequence, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(sequence: u64, count: u32, data: &[u8]) -> Vec<u8> {
        let mut bytes = sequence.to_le_bytes().to_vec();
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn later_sequences_and_deletions_win() {
        let path = Path::new("batch");
        let mut found = None;
        lookup(&batch(10, 2, b"\x01\x01k\x01v\x00\x01k"), path, 0, b"k", &mut found).unwrap();
        assert_eq!(
            found,
            Some(Value {
                sequence: 11,
                bytes: None
            })
        );
        lookup(&batch(1, 1, b"\x01\x01k\x01x"), path, 0, b"k", &mut found).unwrap();
        assert_eq!(found.as_ref().unwrap().sequence, 11);
        lookup(&batch(20, 1, b"\x01\x01k\x01y"), path, 0, b"k", &mut found).unwrap();
        assert_eq!(
            found,
            Some(Value {
                sequence: 20,
                bytes: Some(b"y".to_vec())
            })
        );
    }

    #[test]
    fn malformed_batch_never_returns_a_partial_value() {
        let path = Path::new("batch");
        for bytes in [
            batch(1, 2, b"\x01\x01k\x01v"),
            batch(1, 0, b"\x01\x01k\x01v"),
            batch(MAX_SEQUENCE, 2, b"\x00\x00\x00\x00"),
            batch(1, 1, b"\x02\x00"),
            batch(1, 1, b"\x01\xff\xff\xff\xff\x0f"),
        ] {
            assert!(lookup(&bytes, path, 0, b"k", &mut None).is_err());
        }
    }
}

//! The committed tip's slots (SB14 D2): 64 bytes each, two in `block_headers/tip` (A at offset 0,
//! B at 65,536) and one in SQLite's `meta.hdr_marker`.
//!
//! `magic "SB14" ‖ format 1 ‖ flags ‖ 2 pad ‖ seq u64 ‖ start u32 ‖ tip u32 ‖ tip hash ‖ crc32 ‖ 4
//! pad`, little-endian, the crc over bytes 0..56. Flags: bit 0 `linked` (D12), bit 1 `empty` (no
//! active records; `start`, `tip` and the hash are then zero).

/// A slot's size.
pub const SLOT_LEN: usize = 64;
/// The file slots' offsets in `tip`: 64 KiB apart, above every target's page and block size, so a
/// page writeback of one never rewrites the other (r3 n1).
pub const FILE_SLOT_OFFSETS: [u64; 2] = [0, 65_536];

const MAGIC: &[u8; 4] = b"SB14";
const FORMAT: u8 = 1;
const LINKED: u8 = 1;
const EMPTY: u8 = 2;

/// What a slot commits: the active records' heights, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Committed {
    /// No active records.
    Empty,
    /// The active records are `[start, tip]`, the record at `tip` hashing to `tip_hash`.
    Range {
        start: u32,
        tip: u32,
        tip_hash: [u8; 32],
        /// Every record in `[start, tip]` links to the one below it (D12).
        linked: bool,
    },
}

impl Committed {
    pub fn tip(&self) -> Option<u32> {
        match self {
            Committed::Empty => None,
            Committed::Range {
                tip,
                ..
            } => Some(*tip),
        }
    }
}

/// A decoded slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub seq: u64,
    pub committed: Committed,
}

/// The CRC-32 (IEEE) of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

impl Slot {
    pub fn encode(&self) -> [u8; SLOT_LEN] {
        let mut m = [0u8; SLOT_LEN];
        m[0..4].copy_from_slice(MAGIC);
        m[4] = FORMAT;
        m[8..16].copy_from_slice(&self.seq.to_le_bytes());
        match self.committed {
            Committed::Empty => m[5] = EMPTY,
            Committed::Range {
                start,
                tip,
                tip_hash,
                linked,
            } => {
                m[5] = if linked {
                    LINKED
                } else {
                    0
                };
                m[16..20].copy_from_slice(&start.to_le_bytes());
                m[20..24].copy_from_slice(&tip.to_le_bytes());
                m[24..56].copy_from_slice(&tip_hash);
            }
        }
        let crc = crc32(&m[..56]);
        m[56..60].copy_from_slice(&crc.to_le_bytes());
        m
    }

    /// The slot in `m`, if it is one: the magic, the format and the crc check, and no unknown flag.
    pub fn decode(m: &[u8]) -> Option<Slot> {
        let m: &[u8; SLOT_LEN] = m.get(..SLOT_LEN)?.try_into().ok()?;
        if &m[0..4] != MAGIC || m[4] != FORMAT {
            return None;
        }
        if crc32(&m[..56]) != u32::from_le_bytes(m[56..60].try_into().ok()?) {
            return None;
        }
        let flags = m[5];
        if flags & !(LINKED | EMPTY) != 0 {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes(m[o..o + 4].try_into().expect("4 bytes"));
        let seq = u64::from_le_bytes(m[8..16].try_into().expect("8 bytes"));
        let committed = match flags & EMPTY {
            0 => Committed::Range {
                start: u32_at(16),
                tip: u32_at(20),
                tip_hash: m[24..56].try_into().expect("32 bytes"),
                linked: flags & LINKED != 0,
            },
            _ => Committed::Empty,
        };
        Some(Slot {
            seq,
            committed,
        })
    }
}

/// Where a slot was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotAt {
    /// File slot A (0) or B (1).
    File(usize),
    Sqlite,
}

/// The winner among the valid slots read (`seq` rules, r3 n2): the highest `seq`; two valid
/// slots with one `seq` and different contents are corruption.
pub fn winner(slots: &[(SlotAt, Slot)]) -> Result<Option<(SlotAt, Slot)>, String> {
    let mut best: Option<(SlotAt, Slot)> = None;
    for &(at, slot) in slots {
        match best {
            Some((_, b)) if b.seq == slot.seq => {
                if b.committed != slot.committed {
                    return Err(format!(
                        "two slots with seq {} commit different tips ({b:?}, {slot:?})",
                        slot.seq
                    ));
                }
            }
            Some((_, b)) if b.seq > slot.seq => {}
            _ => best = Some((at, slot)),
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_is_ieee() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn slots_round_trip_and_reject_damage() {
        for committed in [
            Committed::Empty,
            Committed::Range {
                start: 7,
                tip: 260_000,
                tip_hash: [9; 32],
                linked: true,
            },
            Committed::Range {
                start: 0,
                tip: 0,
                tip_hash: [1; 32],
                linked: false,
            },
        ] {
            let s = Slot {
                seq: 42,
                committed,
            };
            let m = s.encode();
            assert_eq!(Slot::decode(&m), Some(s));
            for i in 0..60 {
                let mut bad = m;
                bad[i] ^= 0x10;
                assert_eq!(Slot::decode(&bad), None, "byte {i}");
            }
            assert_eq!(Slot::decode(&m[..63]), None, "short");
        }
        assert_eq!(Slot::decode(&[0; 64]), None, "a hole reads as zeros");
    }

    #[test]
    fn the_winner_is_the_highest_seq_and_a_tie_must_agree() {
        let s = |seq, tip| Slot {
            seq,
            committed: Committed::Range {
                start: 0,
                tip,
                tip_hash: [0; 32],
                linked: true,
            },
        };
        let w = winner(&[(SlotAt::File(0), s(3, 5)), (SlotAt::Sqlite, s(4, 2))]).unwrap();
        assert_eq!(w, Some((SlotAt::Sqlite, s(4, 2))));
        assert!(winner(&[(SlotAt::File(0), s(4, 5)), (SlotAt::Sqlite, s(4, 2))]).is_err());
        assert!(winner(&[(SlotAt::File(0), s(4, 5)), (SlotAt::Sqlite, s(4, 5))]).is_ok());
        assert_eq!(winner(&[]).unwrap(), None);
    }
}

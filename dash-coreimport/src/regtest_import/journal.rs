//! The regtest import journal: one whole-state record, replaced atomically.
//!
//! Layout (all integers little-endian), exactly this long and nothing after it:
//!
//! | Offset | Bytes | Field |
//! | --- | --- | --- |
//! | 0 | 6 | magic `CH5RJ1` |
//! | 6 | 1 | version, 1 |
//! | 7 | 1 | phase: 0 `Intent`, 1 `Moved`, 2 `Complete` |
//! | 8 | 1 | reindex intent, 1 |
//! | 9 | 8 | `at_ms`: the intent's wall clock, ms since the epoch |
//! | 17 | 1 | backup name length `n`, 1..=[`MAX_BACKUP_NAME`] |
//! | 18 | `n` | backup name: ASCII `core-import-` and then `[0-9A-Za-z-]` |
//! | 18+n | 1 | member count, 5 |
//! | 19+n | 5 × 17 | per member in [`MEMBERS`] order: present (0 or 1), `st_dev` u64, `st_ino` u64 (both 0 when absent); `chainstate` is present |
//! | 104+n | 4 | masked crc32c of bytes `0..104+n` |
use super::{BACKUP_PREFIX, MEMBERS, Phase};
use crate::leveldb::crc32c;

pub(crate) const MAGIC: &[u8; 6] = b"CH5RJ1";
pub(crate) const VERSION: u8 = 1;
pub(crate) const MAX_BACKUP_NAME: usize = 64;
const MEMBER_BYTES: usize = 17;
const FIXED: usize = 6 + 1 + 1 + 1 + 8 + 1 + 1 + MEMBERS.len() * MEMBER_BYTES + 4;

/// A member's identity when the intent was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemberId {
    pub present: bool,
    pub dev: u64,
    pub ino: u64,
}

/// The journal's one record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalState {
    pub phase: Phase,
    pub at_ms: u64,
    pub backup: String,
    pub members: [MemberId; MEMBERS.len()],
}

pub(super) fn valid_backup_name(name: &str) -> bool {
    name.len() <= MAX_BACKUP_NAME
        && name.len() > BACKUP_PREFIX.len()
        && name.starts_with(BACKUP_PREFIX)
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl JournalState {
    pub(crate) fn encode(&self) -> Vec<u8> {
        assert!(valid_backup_name(&self.backup), "backup name {:?}", self.backup);
        let mut out = Vec::with_capacity(FIXED + self.backup.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.phase as u8);
        out.push(1);
        out.extend_from_slice(&self.at_ms.to_le_bytes());
        out.push(self.backup.len() as u8);
        out.extend_from_slice(self.backup.as_bytes());
        out.push(MEMBERS.len() as u8);
        for m in &self.members {
            out.push(u8::from(m.present));
            out.extend_from_slice(&m.dev.to_le_bytes());
            out.extend_from_slice(&m.ino.to_le_bytes());
        }
        let crc = crc32c::mask(crc32c::extend(0, &out));
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// The record in `raw`, or what is wrong with it.
    pub(crate) fn decode(raw: &[u8]) -> Result<Self, String> {
        if raw.len() < FIXED {
            return Err(format!("{} bytes, shorter than any record", raw.len()));
        }
        let (body, crc) = raw.split_at(raw.len() - 4);
        let crc = u32::from_le_bytes(crc.try_into().expect("four bytes"));
        if crc != crc32c::mask(crc32c::extend(0, body)) {
            return Err("checksum mismatch".into());
        }
        if &body[..6] != MAGIC {
            return Err("bad magic".into());
        }
        if body[6] != VERSION {
            return Err(format!("version {}", body[6]));
        }
        let phase = match body[7] {
            0 => Phase::Intent,
            1 => Phase::Moved,
            2 => Phase::Complete,
            p => return Err(format!("phase {p}")),
        };
        if body[8] != 1 {
            return Err(format!("reindex intent {}", body[8]));
        }
        let at_ms = u64::from_le_bytes(body[9..17].try_into().expect("eight bytes"));
        let n = usize::from(body[17]);
        if raw.len() != FIXED + n {
            return Err(format!("{} bytes for a {n}-byte backup name", raw.len()));
        }
        let backup = std::str::from_utf8(&body[18..18 + n])
            .ok()
            .filter(|name| valid_backup_name(name))
            .ok_or("bad backup name")?
            .to_owned();
        if usize::from(body[18 + n]) != MEMBERS.len() {
            return Err(format!("{} members", body[18 + n]));
        }
        let mut members = [MemberId::default(); MEMBERS.len()];
        for (i, m) in members.iter_mut().enumerate() {
            let at = 19 + n + i * MEMBER_BYTES;
            let field = |k: usize| u64::from_le_bytes(body[k..k + 8].try_into().expect("eight"));
            *m = match body[at] {
                0 if body[at + 1..at + MEMBER_BYTES].iter().all(|&b| b == 0) => MemberId::default(),
                1 => MemberId {
                    present: true,
                    dev: field(at + 1),
                    ino: field(at + 9),
                },
                _ => return Err(format!("member {}", MEMBERS[i])),
            };
        }
        if !members[0].present {
            return Err(format!("no {}", MEMBERS[0]));
        }
        Ok(Self {
            phase,
            at_ms,
            backup,
            members,
        })
    }
}

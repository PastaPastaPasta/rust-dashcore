//! Power-loss images from a recorded log (plan §6), and the prefix check.
//!
//! A crash after the first `at` events leaves, for each file, its bytes as of its last sync before
//! `at`, plus any of the later operations: each write in 512-byte sector pieces, where a sector
//! keeps a temporal prefix of the pieces written to it (ST3a's `FaultyFlatFiles` adversary), and
//! each truncate or delete kept or lost. No image has a `-shm` file.
//!
//! The model assumes powersafe overwrite (SQLite's default `SQLITE_POWERSAFE_OVERWRITE=1`): a
//! crash during a write to a sector leaves the sector's other bytes as they were, so a write
//! never damages data outside the bytes it writes.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use crate::vfs::Event;

pub const SECTOR: u64 = 512;

/// Files by name in the database's directory.
pub type Files = BTreeMap<String, Vec<u8>>;

/// A database's content: every entry, by column family index and key.
pub type State = BTreeMap<(usize, Vec<u8>), Vec<u8>>;

/// An operation after its file's last sync, which a crash may keep or lose.
enum Pending {
    /// The part of write event `event` that falls in sector `sector`; the `ord`-th piece written
    /// to that sector since the sync.
    Piece {
        event: usize,
        file: String,
        sector: u64,
        offset: u64,
        data: Vec<u8>,
        ord: usize,
    },
    Truncate {
        event: usize,
        file: String,
        size: u64,
    },
    Delete {
        event: usize,
        file: String,
    },
}

/// What a crash keeps: per sector, how many of its pieces (a temporal prefix); per truncate or
/// delete, whether it happened.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Choice {
    pieces: Vec<usize>,
    toggles: Vec<bool>,
}

/// A crash point: the durable files and what is pending.
pub struct Point {
    durable: Files,
    pending: Vec<Pending>,
    /// Pieces per sector key, in key order.
    sectors: Vec<usize>,
    sector_index: HashMap<(String, u64), usize>,
    toggles: usize,
}

fn write(files: &mut Files, file: &str, offset: u64, data: &[u8]) {
    let bytes = files.entry(file.to_owned()).or_default();
    let end = offset as usize + data.len();
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[offset as usize..end].copy_from_slice(data);
}

fn apply(files: &mut Files, event: &Event) {
    match event {
        Event::Write {
            file,
            offset,
            data,
        } => write(files, file, *offset, data),
        Event::Truncate {
            file,
            size,
        } => files.entry(file.clone()).or_default().resize(*size as usize, 0),
        Event::Delete {
            file,
        } => {
            files.remove(file);
        }
        Event::Sync {
            ..
        }
        | Event::Acked {
            ..
        } => {}
    }
}

impl Point {
    /// The crash after the first `at` events of `log`, recorded from `base`.
    pub fn new(base: &Files, log: &[Event], at: usize) -> Self {
        let mut last_sync = HashMap::new();
        for (i, event) in log[..at].iter().enumerate() {
            if let Event::Sync {
                file,
            } = event
            {
                last_sync.insert(file.as_str(), i);
            }
        }
        let mut durable = base.clone();
        let mut pending = Vec::new();
        let mut sector_index = HashMap::new();
        let mut sectors = Vec::new();
        let mut toggles = 0;
        for (i, event) in log[..at].iter().enumerate() {
            let file = match event {
                Event::Write {
                    file,
                    ..
                }
                | Event::Truncate {
                    file,
                    ..
                }
                | Event::Delete {
                    file,
                } => file,
                _ => continue,
            };
            if last_sync.get(file.as_str()).is_some_and(|&s| i < s) {
                apply(&mut durable, event);
                continue;
            }
            match event {
                Event::Write {
                    offset,
                    data,
                    ..
                } => {
                    let mut at = *offset;
                    let end = offset + data.len() as u64;
                    while at < end {
                        let sector = at / SECTOR;
                        let piece_end = end.min((sector + 1) * SECTOR);
                        let key = sector_index.entry((file.clone(), sector)).or_insert_with(|| {
                            sectors.push(0);
                            sectors.len() - 1
                        });
                        let ord = sectors[*key];
                        sectors[*key] += 1;
                        pending.push(Pending::Piece {
                            event: i,
                            file: file.clone(),
                            sector,
                            offset: at,
                            data: data[(at - offset) as usize..(piece_end - offset) as usize]
                                .to_vec(),
                            ord,
                        });
                        at = piece_end;
                    }
                }
                Event::Truncate {
                    size,
                    ..
                } => {
                    toggles += 1;
                    pending.push(Pending::Truncate {
                        event: i,
                        file: file.clone(),
                        size: *size,
                    });
                }
                _ => {
                    toggles += 1;
                    pending.push(Pending::Delete {
                        event: i,
                        file: file.clone(),
                    });
                }
            }
        }
        Self {
            durable,
            pending,
            sectors,
            sector_index,
            toggles,
        }
    }

    /// The number of distinct choices, saturating.
    pub fn choices(&self) -> u64 {
        let pieces = self.sectors.iter().fold(1u64, |n, &k| n.saturating_mul(k as u64 + 1));
        pieces.saturating_mul(1u64.checked_shl(self.toggles as u32).unwrap_or(u64::MAX))
    }

    /// Every choice, in a fixed order.
    pub fn all(&self) -> Vec<Choice> {
        let mut out = vec![Choice {
            pieces: Vec::new(),
            toggles: Vec::new(),
        }];
        for &n in &self.sectors {
            out = out
                .into_iter()
                .flat_map(|c| {
                    (0..=n).map(move |k| {
                        let mut c = c.clone();
                        c.pieces.push(k);
                        c
                    })
                })
                .collect();
        }
        for _ in 0..self.toggles {
            out = out
                .into_iter()
                .flat_map(|c| {
                    [false, true].map(|t| {
                        let mut c = c.clone();
                        c.toggles.push(t);
                        c
                    })
                })
                .collect();
        }
        out
    }

    /// The choice that keeps the pending operations for which `keep(event, sector)` holds, cut
    /// to a temporal prefix per sector. `sector` is `None` for a truncate or delete.
    pub fn choice_where(&self, mut keep: impl FnMut(usize, Option<u64>) -> bool) -> Choice {
        let mut pieces = vec![0; self.sectors.len()];
        let mut broken = vec![false; self.sectors.len()];
        let mut toggles = Vec::new();
        for p in &self.pending {
            match p {
                Pending::Piece {
                    event,
                    file,
                    sector,
                    ..
                } => {
                    let key = self.sector_index[&(file.clone(), *sector)];
                    if !broken[key] && keep(*event, Some(*sector)) {
                        pieces[key] += 1;
                    } else {
                        broken[key] = true;
                    }
                }
                Pending::Truncate {
                    event,
                    ..
                }
                | Pending::Delete {
                    event,
                    ..
                } => toggles.push(keep(*event, None)),
            }
        }
        Choice {
            pieces,
            toggles,
        }
    }

    /// The temporal prefixes (the first `m` pending operations, for `m` evenly spread up to all),
    /// then `random` choices drawn from `seed`.
    pub fn sampled(&self, prefixes: usize, random: usize, seed: u64) -> Vec<Choice> {
        let n = self.pending.len();
        let mut out: Vec<Choice> = (0..=prefixes.min(n))
            .map(|i| {
                let m = if prefixes >= n {
                    i
                } else {
                    i * n / prefixes
                };
                let mut seen = 0;
                self.choice_where(|_, _| {
                    seen += 1;
                    seen <= m
                })
            })
            .collect();
        let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..random {
            out.push(Choice {
                pieces: self.sectors.iter().map(|&k| (next() % (k as u64 + 1)) as usize).collect(),
                toggles: (0..self.toggles).map(|_| next() % 2 == 0).collect(),
            });
        }
        out
    }

    /// The files a crash with `choice` leaves.
    pub fn image(&self, choice: &Choice) -> Files {
        let mut files = self.durable.clone();
        let mut toggle = choice.toggles.iter();
        for p in &self.pending {
            match p {
                Pending::Piece {
                    file,
                    sector,
                    offset,
                    data,
                    ord,
                    ..
                } => {
                    if *ord < choice.pieces[self.sector_index[&(file.clone(), *sector)]] {
                        write(&mut files, file, *offset, data);
                    }
                }
                Pending::Truncate {
                    file,
                    size,
                    ..
                } => {
                    if *toggle.next().expect("a toggle per truncate") {
                        files.entry(file.clone()).or_default().resize(*size as usize, 0);
                    }
                }
                Pending::Delete {
                    file,
                    ..
                } => {
                    if *toggle.next().expect("a toggle per delete") {
                        files.remove(file);
                    }
                }
            }
        }
        files
    }
}

pub fn digest(files: &Files) -> u64 {
    let mut h = DefaultHasher::new();
    files.hash(&mut h);
    h.finish()
}

/// The commit whose state `got` is: one at or after `required` (every commit acknowledged durable
/// before the crash). `states[j]` is the state after the first `j` commits. When states repeat
/// (a commit that changes nothing, or one that undoes another), the latest match is taken: the
/// content is all a crash can be judged by.
pub fn prefix_of(states: &[State], required: usize, got: &State) -> Result<usize, String> {
    match states.iter().rposition(|s| s == got) {
        Some(j) if j >= required => Ok(j),
        Some(j) => {
            Err(format!("recovered the state after {j} commits, but {required} were durable"))
        }
        None => {
            let close = states
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.iter().filter(|e| !got.contains_key(e.0)).count())
                .map_or(0, |(j, _)| j);
            Err(format!(
                "recovered a state that is no commit prefix (nearest: {close} commits): {got:?}"
            ))
        }
    }
}

/// The commit a crash after the first `at` events must keep: the last one acknowledged durable.
pub fn required(log: &[Event], at: usize) -> usize {
    log[..at]
        .iter()
        .filter_map(|e| match e {
            Event::Acked {
                commit,
                durable: true,
            } => Some(*commit),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

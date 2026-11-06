//! The record a measured pass's child writes (F8): its identity, conditions, completion and
//! counts, as one JSON object of at most [`MAX_RECORD`] bytes.

use serde::{Deserialize, Serialize};

use crate::account::{Completion, WindowCount};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use crate::fixture::{self, Fixture};
use crate::{Result, refuse};

/// A record's bound (rev6: the supervisor drains the pipe and refuses anything longer).
pub const MAX_RECORD: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Leg {
    /// dashd-node with real V (S6).
    Node,
    /// Dash Core through the oracle's bench session.
    Core,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pass {
    /// Through P2P with the listener (or `PeerManager`) registered.
    A,
    /// Straight to the chain with it unregistered.
    B,
}

/// What a record was measured on: equal across a leg's two passes, and across the two legs but
/// for the [`Identity::core`] half (S1 and S6 are compared against one Core record).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The rust-dashcore revision (`git rev-parse HEAD`, `-dirty` with tracked changes).
    pub revision: String,
    /// `release` or `debug` (debug assertions).
    pub profile: String,
    /// `uname -r`.
    pub kernel: String,
    /// The CPU's model, microcode and the instruction-set flags code dispatches on at run time
    /// ([`crate::sys::cpu`]).
    pub cpu: String,
    /// What else shapes the build: the compile-time flags and environment, and a digest of the
    /// Cargo configuration files ([`crate::sys::build_env`]).
    pub build: String,
    /// The fixture file's sha256 ([`crate::fixture`]).
    pub fixture_sha256: String,
    pub prefix_blocks: u32,
    /// sha256 of the prefix's blocks as the fixture encodes them.
    pub prefix_sha256: String,
    pub workload_blocks: u32,
    pub workload_txs: u64,
    /// The last workload block's time and both legs' mock time.
    pub t0: i64,
    /// Core's pin, ABI and archive (Core leg only).
    pub core: Option<CoreBuild>,
}

impl Identity {
    /// A pass's identity on `f` (the first `f.workload.len()` rows of the checked-in fixture).
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub fn of(f: &Fixture, revision: String, core: Option<CoreBuild>) -> Identity {
        Identity {
            revision,
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
            kernel: crate::sys::kernel_release(),
            cpu: crate::sys::cpu(),
            build: crate::sys::build_env(),
            fixture_sha256: fixture::SHA256.into(),
            prefix_blocks: f.prefix.len() as u32,
            prefix_sha256: f.prefix_sha256(),
            workload_blocks: f.workload.len() as u32,
            workload_txs: f.workload_txs(),
            t0: fixture::T0,
            core,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreBuild {
    pub pin: String,
    /// The shim commit `pins.txt` names.
    pub commit: String,
    pub abi: i32,
    /// sha256 of the archive linked.
    pub archive_sha256: String,
    /// `pins.txt`'s `sha256(plain)` for the pin: the comparison refuses any other archive.
    pub pinned_sha256: String,
    /// `dash_oracle_sys::ARCHIVES_PINNED`: every linked archive is the one `pins.txt` records.
    pub pinned: bool,
    /// The linked archive's flavor (`plain`, `fuzz`, `ubsan`).
    pub flavor: String,
}

/// The run conditions a record states, checked by the comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conditions {
    /// Initial block download at the window's start (must be false).
    pub ibd_at_start: bool,
    /// H requested high-bandwidth compact blocks at the window's start (pass A).
    pub hb_at_start: Option<bool>,
    /// The node's options, or the Core session's chain arguments.
    pub options: Vec<String>,
    /// Each block-index check as resolved, by name (all must be off).
    pub check_block_index: Vec<(String, bool)>,
    /// The resolved debug-category mask (must be 0).
    pub log_categories: u64,
    /// Core only: the session is a bench session, recording no draw, call or log line.
    pub bench: Option<bool>,
    /// The script-check workers (`d-scriptch.<n>`) running when the conditions are taken, just
    /// before the window: the node's must be none (`-par=1`), Core's its pinned test setup's
    /// [`crate::compare::CORE_SCRIPT_CHECK_THREADS`].
    pub script_check_threads: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub leg: Leg,
    pub pass: Pass,
    /// The child's pid: the supervisor checks one process per pass.
    pub pid: u32,
    pub identity: Identity,
    pub conditions: Conditions,
    pub completion: Completion,
    /// `None` for a window opened without counters (no PMU): never a number.
    pub counts: Option<WindowCount>,
}

impl Record {
    /// This process's record of a pass.
    pub fn new(
        leg: Leg,
        pass: Pass,
        identity: Identity,
        conditions: Conditions,
        completion: Completion,
        counts: Option<WindowCount>,
    ) -> Record {
        Record {
            leg,
            pass,
            pid: std::process::id(),
            identity,
            conditions,
            completion,
            counts,
        }
    }

    /// The record's bytes; refuses beyond [`MAX_RECORD`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).expect("a record serializes");
        if bytes.len() > MAX_RECORD {
            refuse!("a {}-byte record exceeds {MAX_RECORD}", bytes.len());
        }
        Ok(bytes)
    }

    /// A child's record; refuses a long or malformed one.
    pub fn decode(bytes: &[u8]) -> Result<Record> {
        if bytes.len() > MAX_RECORD {
            refuse!("a {}-byte record exceeds {MAX_RECORD}", bytes.len());
        }
        match serde_json::from_slice(bytes) {
            Ok(r) => Ok(r),
            Err(e) => refuse!("a malformed record ({e}): {:?}", String::from_utf8_lossy(bytes)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn record(leg: Leg, pass: Pass, total: u64) -> Record {
        let a = pass == Pass::A;
        Record {
            leg,
            pass,
            pid: if a {
                100
            } else {
                101
            },
            identity: Identity {
                revision: "abc".into(),
                profile: "release".into(),
                kernel: "7.0.0-38-generic".into(),
                cpu: "cpu".into(),
                build: "build".into(),
                fixture_sha256: "f".repeat(64),
                prefix_blocks: 100,
                prefix_sha256: "e".repeat(64),
                workload_blocks: 201,
                workload_txs: 5704,
                t0: 1_761_955_200,
                core: (leg == Leg::Core).then(|| CoreBuild {
                    pin: "v24.0".into(),
                    commit: "08700f95eac5b63e1467625cab26a09ff93c38d3".into(),
                    abi: 34,
                    archive_sha256: "d".repeat(64),
                    pinned_sha256: "d".repeat(64),
                    pinned: true,
                    flavor: "plain".into(),
                }),
            },
            conditions: Conditions {
                ibd_at_start: false,
                hb_at_start: a.then_some(true),
                options: vec!["-checkblockindex=0".into(), "-par=1".into()],
                check_block_index: vec![("v".into(), false)],
                log_categories: 0,
                bench: (leg == Leg::Core).then_some(true),
                script_check_threads: if leg == Leg::Core {
                    2
                } else {
                    0
                },
            },
            completion: Completion {
                tip_is_last: true,
                signals_barrier: true,
                actor_barrier: a.then_some(true),
                hb_last_cmpct: a.then_some(true),
                quiesced: a.then_some(true),
            },
            counts: Some(WindowCount {
                total,
                calls: if leg == Leg::Core && a {
                    3
                } else {
                    0
                },
                calibration_pairs: if leg == Leg::Core && a {
                    64
                } else {
                    0
                },
                threads_counted: 4,
                threads_excluded: u32::from(leg == Leg::Node),
                threads_idle: u32::from(leg == Leg::Node),
                ..WindowCount::default()
            }),
        }
    }

    #[test]
    fn a_record_round_trips_within_its_bound() {
        let r = record(Leg::Core, Pass::A, 7);
        let bytes = r.encode().unwrap();
        assert!(bytes.len() <= MAX_RECORD);
        assert_eq!(Record::decode(&bytes).unwrap(), r);
        assert!(Record::decode(&bytes[..bytes.len() - 1]).unwrap_err().0.contains("malformed"));
        let mut long = r;
        long.conditions.options = vec!["x".repeat(MAX_RECORD)];
        assert!(long.encode().unwrap_err().0.contains("exceeds"));
        assert!(Record::decode(&[b' '; MAX_RECORD + 1]).unwrap_err().0.contains("exceeds"));
    }
}

//! The model-target registry (DEC-59 Q1): fuzz targets that check node code against a reference
//! model instead of Dash Core's oracle. They run through `run.sh` like the L1 targets; the
//! runner's pin is the Core release the node targets (v24.0). Later tasks add theirs here, with
//! a target file and a `targets.toml` entry.

mod block_download_model;
// BS11a-3 D18: dash-pkc's Group and engine BLS representations against each other.
mod bls_engine_model;
mod chainstate_crash;
mod cmpct_reconstruct;
mod coins_cache_model;
mod coins_overlay_diff;
mod coins_replay_blind;
mod connect_disconnect_roundtrip;
mod connman_sched;
mod coreimport_external_scan;
mod coreimport_flat;
mod coreimport_netstate;
mod disconnect_pool;
mod dml_codec_roundtrip;
mod kv_model;
mod netinfo;
// DEC-178: the Core-derived acceptance matrix `p2p_decode_model` judges decode verdicts by.
mod p2p_decode_matrix;
mod p2p_decode_model;
mod p2p_gate_model;
mod p2p_transport_model;
mod packed_coin_roundtrip;
mod prodis_arith;
mod specialtx_trivial;
mod undo_decode;

pub use p2p_transport_model::CountingAlloc;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use dash_p2p::CommandString;
use dash_p2p::policy::ALL_NET_MESSAGE_TYPES;
use dashd_chainstate::crash::ChainSut;
use dashd_store::faulty::{InvariantId, Scenario, Seeded, sweep};
use node_l1::Divergence;

thread_local! {
    /// The command the current input's check is on (empty: none), for the message models.
    static COMMAND: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The name a message model files a command under: Core's own name for one of its 74, else
/// `unknown`, so the commands a mutated input can spell (12 raw bytes) are one cluster key.
pub(crate) fn command_class(cmd: &CommandString) -> &str {
    if ALL_NET_MESSAGE_TYPES.contains(cmd) {
        cmd.as_str()
    } else {
        "unknown"
    }
}

/// Notes the command the check of the current input is on; a divergence it then returns is
/// recorded with it (`cmd` in the sidecar). The last call before the divergence counts.
pub(crate) fn note_command(cmd: &CommandString) {
    COMMAND.with(|c| {
        let mut c = c.borrow_mut();
        c.clear();
        c.push_str(command_class(cmd));
    });
}

/// Forgets the noted command: called before each input.
pub(crate) fn clear_command() {
    COMMAND.with(|c| c.borrow_mut().clear());
}

/// The command noted since [`clear_command`], if any.
pub(crate) fn noted_command() -> Option<String> {
    COMMAND.with(|c| Some(c.borrow().clone()).filter(|c| !c.is_empty()))
}

/// A model target: its name (the `node-fuzz` target and its directory in the private tree) and
/// its check, which reports only the name of what disagreed.
pub struct ModelTarget {
    pub name: &'static str,
    pub check: fn(&[u8]) -> Result<(), Divergence>,
    /// What the target sets up once per process before its first input (libFuzzer's
    /// initialisation hook), so that no input's coverage includes it.
    pub start: Option<fn()>,
}

/// Every model target.
pub const MODELS: &[ModelTarget] = &[
    ModelTarget {
        name: "kv_model",
        check: kv_model::check,
        start: None,
    },
    ModelTarget {
        name: "coreimport_external_scan",
        check: coreimport_external_scan::check,
        start: None,
    },
    ModelTarget {
        name: "coreimport_peers_dat",
        check: coreimport_netstate::peers_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_anchors_dat",
        check: coreimport_netstate::anchors_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_banlist_json",
        check: coreimport_netstate::banlist_json,
        start: None,
    },
    ModelTarget {
        name: "coreimport_mempool_dat",
        check: coreimport_netstate::mempool_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_fee_estimates_dat",
        check: coreimport_netstate::fee_estimates_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_settings_json",
        check: coreimport_netstate::settings_json,
        start: None,
    },
    ModelTarget {
        name: "coreimport_governance_dat",
        check: coreimport_flat::governance_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_mncache_dat",
        check: coreimport_flat::mncache_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_netfulfilled_dat",
        check: coreimport_flat::netfulfilled_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_sporks_dat",
        check: coreimport_flat::sporks_dat,
        start: None,
    },
    ModelTarget {
        name: "coreimport_dash_conf",
        check: coreimport_flat::dash_conf,
        start: None,
    },
    ModelTarget {
        name: "coreimport_onion_key",
        check: coreimport_flat::onion_key,
        start: None,
    },
    ModelTarget {
        name: "coreimport_i2p_key",
        check: coreimport_flat::i2p_key,
        start: None,
    },
    ModelTarget {
        name: "packed_coin_roundtrip",
        check: packed_coin_roundtrip::check,
        start: None,
    },
    ModelTarget {
        name: "disconnect_pool",
        check: disconnect_pool::check,
        start: None,
    },
    ModelTarget {
        name: "bls_engine_model",
        check: bls_engine_model::check,
        start: None,
    },
    ModelTarget {
        name: "coins_cache_model",
        check: coins_cache_model::check,
        start: None,
    },
    ModelTarget {
        name: "coins_overlay_diff",
        check: coins_overlay_diff::check,
        start: None,
    },
    ModelTarget {
        name: "chainstate_crash",
        check: chainstate_crash::check,
        start: None,
    },
    ModelTarget {
        name: "coins_replay_blind",
        check: coins_replay_blind::check,
        start: None,
    },
    ModelTarget {
        name: "undo_decode",
        check: undo_decode::check,
        start: None,
    },
    ModelTarget {
        name: "connect_disconnect_roundtrip",
        check: connect_disconnect_roundtrip::check,
        start: None,
    },
    ModelTarget {
        name: "dml_codec_roundtrip",
        check: dml_codec_roundtrip::check,
        start: None,
    },
    ModelTarget {
        name: "netinfo_validate",
        check: netinfo::validate_check,
        start: Some(netinfo::validate_start),
    },
    ModelTarget {
        name: "netinfo_add_entry",
        check: netinfo::add_entry_check,
        start: Some(netinfo::add_entry_start),
    },
    ModelTarget {
        name: "prodis_arith",
        check: prodis_arith::check,
        start: None,
    },
    ModelTarget {
        name: "specialtx_trivial",
        check: specialtx_trivial::check,
        start: None,
    },
    ModelTarget {
        name: "p2p_transport_model",
        check: p2p_transport_model::check,
        start: None,
    },
    ModelTarget {
        name: "p2p_decode_model",
        check: p2p_decode_model::check,
        start: None,
    },
    ModelTarget {
        name: "p2p_gate_model",
        check: p2p_gate_model::check,
        start: None,
    },
    ModelTarget {
        name: "connman_sched",
        check: connman_sched::check,
        start: None,
    },
    ModelTarget {
        name: "cmpct_reconstruct",
        check: cmpct_reconstruct::check,
        start: None,
    },
    ModelTarget {
        name: "block_download_model",
        check: block_download_model::check,
        start: None,
    },
];

/// One CRASH sweep case (CH2 §6.5) from `generate`, seeded by the input's first 8 bytes
/// (little-endian, zero-padded): the first finding's invariant, if any.
fn crash_case(
    data: &[u8],
    generate: &dyn Fn(&mut Seeded) -> Scenario<ChainSut>,
) -> Result<(), Divergence> {
    let mut seed = [0; 8];
    let n = data.len().min(8);
    seed[..n].copy_from_slice(&data[..n]);
    let report = sweep::<ChainSut>(1, u64::from_le_bytes(seed), generate);
    match report.findings.first() {
        Some(f) => Err(Divergence {
            field: invariant_name(f.violation.invariant),
        }),
        None => Ok(()),
    }
}

/// An invariant's name, interned (`Divergence` names a field by a static string).
fn invariant_name(id: InvariantId) -> &'static str {
    static NAMES: OnceLock<Mutex<BTreeMap<InvariantId, &'static str>>> = OnceLock::new();
    let mut names =
        NAMES.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner);
    names.entry(id).or_insert_with(|| id.name().leak())
}

/// The registered model target named `name`.
pub fn model(name: &str) -> Option<&'static ModelTarget> {
    MODELS.iter().find(|m| m.name == name)
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn a_command_is_filed_under_core_s_name_or_unknown() {
        for known in ALL_NET_MESSAGE_TYPES.iter() {
            assert_eq!(command_class(known), known.as_str());
        }
        // 12 raw bytes a mutated input can spell, however many, are one key.
        for raw in [*b"barfoo\0\0\0\0\0\0", *b"VERSION\0\0\0\0\0", [0xff; 12], [0; 12]] {
            assert_eq!(command_class(&CommandString::from_bytes(raw)), "unknown");
        }
        assert_eq!(command_class(&CommandString::from_static("")), "unknown");
        // Every key a sidecar may carry.
        for known in ALL_NET_MESSAGE_TYPES.iter() {
            assert!(node_sink::sink::is_command_name(command_class(known)), "{}", known.as_str());
        }
    }

    #[test]
    fn the_noted_command_is_per_input() {
        clear_command();
        assert_eq!(noted_command(), None);
        note_command(&ALL_NET_MESSAGE_TYPES[3]);
        assert_eq!(noted_command().as_deref(), Some(ALL_NET_MESSAGE_TYPES[3].as_str()));
        note_command(&CommandString::from_bytes([0xee; 12]));
        assert_eq!(noted_command().as_deref(), Some("unknown"));
        clear_command();
        assert_eq!(noted_command(), None);
    }
}

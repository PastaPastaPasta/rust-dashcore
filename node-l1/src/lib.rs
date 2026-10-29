//! L1 differential comparisons against Dash Core's in-process oracle (A1 §4.4, CS0 §9.3).
//!
//! Each L1 target is one comparison function here, called by its libFuzzer target
//! (`node-fuzz`), its proptest twin, the boundary rows and the private-seed replay, so the forms
//! cannot drift. Every target, including later tasks', registers in [`TARGETS`].
//!
//! A comparison reports only the name of the field that differs ([`Divergence`]), never the
//! input or either side's value; callers record the input in the private sink (`node-sink`).

#![cfg(feature = "oracle")]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

use std::fmt;

use dash_consensus::primitives::core_serialize;
use dash_consensus::{decode_block, decode_header, decode_tx, decode_tx_exact};
use dash_oracle_sys::{ErrorKind, Oracle, Pin};

pub mod bsdk_decode;
// The CS3 input formats and the node's side of their checks, shared with dash-consensus's tests.
pub mod bls;
pub mod bls_arith;
pub mod bls_wrapper;
pub mod chain;
#[allow(clippy::expect_used)]
#[path = "../../dash-consensus/tests/support/checks.rs"]
pub mod check_inputs;
pub mod checks;
pub mod coinjoin;
pub mod coins;
pub mod coins_ops;
pub mod core_usage;
pub mod credit_pool;
pub mod dml;
pub mod dml_apply;
pub mod dml_block;
pub mod dml_checks;
pub mod dml_payee;
pub mod gov;
pub mod gov_superblock;
pub mod inputs;
pub mod llmq;
pub mod mnhf;
pub mod mnsync;
pub mod overlay;
pub mod p2p;
pub mod p2p_addr;
pub mod p2p_node;
pub mod p2p_single;
pub mod payload;
pub mod payments;
pub mod versionbits;

pub use checks::{
    compare_check_block, compare_check_transaction, compare_contextual_check_block,
    compare_contextual_check_block_header, compare_contextual_check_tx,
};
pub use coins::{compare_block_undo, compare_codec_primitives, compare_coin_decode};
pub use inputs::{compare_check_tx_inputs, compare_sequence_locks};
pub use llmq::{
    compare_quorum_members, compare_quorum_members_rot, compare_scan_quorums,
    compare_select_quorum, compare_type_enabled,
};
pub use p2p::{
    compare_addrman, compare_p2p_multi_handshake, compare_p2p_multi_headers,
    compare_p2p_multi_relay,
};
pub use p2p_addr::{
    compare_p2p_addr, compare_p2p_addrv2, compare_p2p_getaddr, compare_p2p_selfaddr,
};
pub use p2p_single::{
    compare_p2p_getdata, compare_p2p_headers, compare_p2p_headers2, compare_p2p_inv,
    compare_p2p_notfound,
};
pub use versionbits::compare_versionbits;

/// The field in which the node and Core disagree.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    pub field: &'static str,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.field)
    }
}

/// The field name only (CS0 §9.5).
impl fmt::Debug for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Divergence({})", self.field)
    }
}

/// An agreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Both decode, and every compared field is equal.
    BothAccept,
    /// Both reject the bytes, or (for a check) both reject the object alike: the same result
    /// class, reject reason and debug message.
    BothReject,
    /// Nothing was compared: the oracle refuses the input as outside its domain
    /// (`ORACLE_E_CONTEXT`, `ORACLE_E_ARG`), the input is too short for the target's format, or
    /// the target has no comparison at this pin. It is never an agreement: a private seed that
    /// gets it on each of its target's pins fails the gate's replay, and the CS3 census caps its
    /// share.
    OutOfDomain,
    /// Core reads a flag byte other than 0 or 1 as `true`, which the node refuses, and the input
    /// is otherwise read alike: the declared strictness of BSX A-12 (DEC-98), counted apart.
    StrictFlag,
    /// Nothing is scored and the input must not enter the corpus: `diff_chain`'s tie discard
    /// (O2b D30). node-fuzz returns `Corpus::Reject` for it.
    Discarded,
}

/// A comparison's result.
pub type Compared = Result<Outcome, Divergence>;

const fn divergence(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

fn check(equal: bool, field: &'static str) -> Result<(), Divergence> {
    if equal {
        Ok(())
    } else {
        Err(divergence(field))
    }
}

/// Core's verdict: `Ok` when it decodes, `Err(true)` when its deserializer rejects the bytes,
/// `Err(false)` for any other oracle failure.
fn core_verdict<T>(result: dash_oracle_sys::Result<T>) -> Result<T, bool> {
    result.map_err(|e| e.kind == ErrorKind::Decode)
}

fn mismatched_verdicts<C, O, E>(core: Result<C, bool>, ours: Result<O, E>) -> Compared {
    match (core, ours) {
        (Err(false), _) => Err(divergence("oracle error")),
        (Err(_), Err(_)) => Ok(Outcome::BothReject),
        (Ok(_), Err(_)) | (Err(_), Ok(_)) | (Ok(_), Ok(_)) => Err(divergence("decode verdict")),
    }
}

/// `diff_tx_decode`: one transaction read from the front of `bytes` (P2P semantics).
/// Compared: the verdict, the consumed length, Core's serialization, txid, instance hash, size, and
/// `wire_is_canonical` against a byte comparison.
pub fn compare_tx(bytes: &[u8], pin: Pin) -> Compared {
    let core = core_verdict(Oracle::new(pin).tx_decode(bytes));
    let mut cursor = bytes;
    let ours = decode_tx(&mut cursor);
    let (core, ours) = match (core, ours) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    let tx = ours.value();
    check(ours.wire().len() == core.consumed, "consumed")?;
    check(core_serialize(tx) == core.reencoded, "serialization")?;
    check(tx.txid().as_bytes() == &core.txid, "txid")?;
    check(tx.instance_hash().as_bytes() == &core.instance_hash, "instance hash")?;
    check(tx.size() as usize == core.reencoded.len(), "size")?;
    check(ours.wire_is_canonical() == (core.reencoded == ours.wire()), "wire_is_canonical")?;
    check(ours.core_bytes().as_slice() == core.reencoded, "core bytes")?;
    Ok(Outcome::BothAccept)
}

/// Core's `DecodeTx` (RPC semantics): exactly one transaction. The oracle's entry point streams,
/// so Core's verdict is "decodes and consumes everything".
pub fn compare_tx_exact(bytes: &[u8], pin: Pin) -> Compared {
    let core = core_verdict(Oracle::new(pin).tx_decode(bytes)).and_then(|core| {
        if core.consumed == bytes.len() {
            Ok(core)
        } else {
            Err(true)
        }
    });
    let ours = decode_tx_exact(bytes);
    match (core, ours) {
        (Ok(core), Ok(ours)) => {
            check(core_serialize(ours.value()) == core.reencoded, "serialization")?;
            Ok(Outcome::BothAccept)
        }
        (core, ours) => mismatched_verdicts(core, ours),
    }
}

/// `diff_block_decode`: one block read from the front of `bytes`. Compared: as for
/// transactions, plus the block hash, the merkle root and the mutation flag under the pin's
/// rules.
pub fn compare_block(bytes: &[u8], pin: Pin) -> Compared {
    let core = core_verdict(Oracle::new(pin).block_decode(bytes));
    let mut cursor = bytes;
    let ours = decode_block(&mut cursor);
    let (core, ours) = match (core, ours) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    let block = ours.value();
    let (root, mutated) = block.merkle();
    check(ours.wire().len() == core.consumed, "consumed")?;
    check(core_serialize(block) == core.reencoded, "serialization")?;
    check(block.hash().as_bytes() == &core.hash, "block hash")?;
    check(root.as_bytes() == &core.merkle_root, "merkle root")?;
    check(mutated == core.mutated, "mutated")?;
    check(block.size() as usize == core.reencoded.len(), "size")?;
    check(ours.wire_is_canonical() == (core.reencoded == ours.wire()), "wire_is_canonical")?;
    check(ours.core_bytes().as_slice() == core.reencoded, "core bytes")?;
    Ok(Outcome::BothAccept)
}

/// `diff_header_decode`: one 80-byte header. Compared: the verdict, consumed length, Core's
/// serialization and the X11 hash.
pub fn compare_header(bytes: &[u8], pin: Pin) -> Compared {
    let core = core_verdict(Oracle::new(pin).header_decode(bytes));
    let mut cursor = bytes;
    let ours = decode_header(&mut cursor);
    let (core, ours) = match (core, ours) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    check(ours.wire().len() == core.consumed, "consumed")?;
    check(core_serialize(ours.value()) == core.reencoded, "serialization")?;
    check(ours.value().hash().as_bytes() == &core.hash, "header hash")?;
    check(ours.wire_is_canonical() == (core.reencoded[..] == *ours.wire()), "wire_is_canonical")?;
    Ok(Outcome::BothAccept)
}

/// Core's decode of a transaction, block or header as the judges take it: `None` when its
/// deserializer rejects the bytes.
fn bsdk_core_verdict(
    result: dash_oracle_sys::Result<(usize, Vec<u8>)>,
) -> Result<Option<bsdk_decode::CoreDecode>, Divergence> {
    match core_verdict(result) {
        Ok((consumed, reencoded)) => Ok(Some(bsdk_decode::CoreDecode {
            consumed,
            reencoded,
        })),
        Err(true) => Ok(None),
        Err(false) => Err(divergence("oracle error")),
    }
}

fn bsdk_outcome(judged: bsdk_decode::Judged) -> Compared {
    match judged {
        Ok(bsdk_decode::Agreement::BothAccept) => Ok(Outcome::BothAccept),
        Ok(bsdk_decode::Agreement::BothReject) => Ok(Outcome::BothReject),
        Err(field) => Err(divergence(field)),
    }
}

/// `diff_bsdk_tx_decode` (BS4b): base-sdk's `Transaction` against Core's `CTransaction`, read
/// from the front of `bytes`. Compared: the verdict, the length read and every decoded field.
pub fn compare_bsdk_tx(bytes: &[u8], pin: Pin) -> Compared {
    let core =
        bsdk_core_verdict(Oracle::new(pin).tx_decode(bytes).map(|d| (d.consumed, d.reencoded)))?;
    bsdk_outcome(bsdk_decode::judge_tx(bytes, core.as_ref()))
}

/// `diff_bsdk_block_decode` (BS4b): base-sdk's `Block` against Core's `CBlock`.
pub fn compare_bsdk_block(bytes: &[u8], pin: Pin) -> Compared {
    let core =
        bsdk_core_verdict(Oracle::new(pin).block_decode(bytes).map(|d| (d.consumed, d.reencoded)))?;
    bsdk_outcome(bsdk_decode::judge_block(bytes, core.as_ref()))
}

/// `diff_bsdk_header_decode` (BS4b): base-sdk's `BlockHeader` against Core's `CBlockHeader`.
pub fn compare_bsdk_header(bytes: &[u8], pin: Pin) -> Compared {
    let core = bsdk_core_verdict(
        Oracle::new(pin).header_decode(bytes).map(|d| (d.consumed, d.reencoded.to_vec())),
    )?;
    bsdk_outcome(bsdk_decode::judge_header(bytes, core.as_ref()))
}

/// Core's `GetTxPayload` deserializer for `tx_type` under each BLS scheme flag: `[basic, legacy]`.
pub fn core_payload(
    oracle: &Oracle,
    tx_type: u16,
    payload: &[u8],
) -> Result<[Option<bsdk_decode::CorePayload>; 2], Divergence> {
    let mut out = [None, None];
    for (slot, legacy) in out.iter_mut().zip([false, true]) {
        *slot = match core_verdict(oracle.payload_decode(tx_type, legacy, payload)) {
            Ok(d) => Some(bsdk_decode::CorePayload {
                consumed_all: d.consumed_all,
                reencoded: d.reencoded,
            }),
            Err(true) => None,
            Err(false) => return Err(divergence("oracle error")),
        };
    }
    Ok(out)
}

/// `diff_payload_decode` (BS4b): a special-transaction payload, `[selector] ++ payload` (the
/// selector picks the type, 1 to 12: v24 only, the pin that has them all), decoded by base-sdk and by `GetTxPayload`, under
/// both BLS scheme flags. Compared per flag: the verdict, whether every byte was read, and the
/// decoded field values.
pub fn compare_payload(bytes: &[u8], pin: Pin) -> Compared {
    let Some((tx_type, payload)) =
        bsdk_decode::split_payload_input(bytes, bsdk_decode::PAYLOAD_TYPES)
    else {
        return Ok(Outcome::BothReject);
    };
    let core = core_payload(&Oracle::new(pin), tx_type, payload)?;
    let mut accepted = false;
    for (core, legacy) in core.iter().zip([false, true]) {
        match bsdk_decode::judge_payload(tx_type, legacy, payload, core.as_ref()) {
            Ok(bsdk_decode::Agreement::BothAccept) => accepted = true,
            Ok(bsdk_decode::Agreement::BothReject) => {}
            Err(field) => return Err(divergence(field)),
        }
    }
    Ok(if accepted {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    })
}

/// An L1 target: its name (the `node-fuzz` target and the private seed directory), its
/// comparison, the pins it compares on, and its start.
pub struct Target {
    pub name: &'static str,
    pub compare: fn(&[u8], Pin) -> Compared,
    pub pins: &'static [Pin],
    /// What the target sets up once per process before its first input (libFuzzer's
    /// initialisation hook), so that no input's coverage includes it.
    pub start: Option<fn(Pin)>,
}

/// Every linked pin.
const ALL: &[Pin] = &Pin::ALL;
/// The overlay messages and BS4b's decode targets are read at v24 only (DEC-82).
const V24: &[Pin] = &[Pin::V24_0];

/// Every L1 target. Later tasks add theirs here.
pub const TARGETS: &[Target] = &[
    Target {
        name: "diff_tx_decode",
        compare: compare_tx,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_block_decode",
        compare: compare_block,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_header_decode",
        compare: compare_header,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_codec_primitives",
        compare: compare_codec_primitives,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_coin_decode",
        compare: compare_coin_decode,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_undo_decode",
        compare: compare_block_undo,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_check_transaction",
        compare: compare_check_transaction,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_check_block",
        compare: compare_check_block,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_contextual_check_tx",
        compare: compare_contextual_check_tx,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_contextual_check_block_header",
        compare: compare_contextual_check_block_header,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_contextual_check_block",
        compare: compare_contextual_check_block,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_check_tx_inputs",
        compare: compare_check_tx_inputs,
        pins: ALL,
        start: None,
    },
    Target {
        name: "diff_sequence_locks",
        compare: compare_sequence_locks,
        pins: ALL,
        start: None,
    },
    // CS4: versionbits and the EHF stage filter (`versionbits`).
    Target {
        name: "diff_versionbits",
        compare: compare_versionbits,
        pins: V24,
        start: None,
    },
    // O2c: BS8's overlay messages (`overlay`).
    Target {
        name: "diff_p2p_clsig",
        compare: overlay::compare::clsig,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_isdlock",
        compare: overlay::compare::isdlock,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qsigrec",
        compare: overlay::compare::qsigrec,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qfcommit",
        compare: overlay::compare::qfcommit,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_mnauth",
        compare: overlay::compare::mnauth,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_spork",
        compare: overlay::compare::spork,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_platformban",
        compare: overlay::compare::platformban,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qgetdata",
        compare: overlay::compare::qgetdata,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qdata",
        compare: overlay::compare::qdata,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_getqrinfo",
        compare: overlay::compare::getqrinfo,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qrinfo",
        compare: overlay::compare::qrinfo,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_mnlistdiff",
        compare: overlay::compare::mnlistdiff,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_smlentry",
        compare: overlay::compare::smlentry,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_qsnapshot",
        compare: overlay::compare::qsnapshot,
        pins: V24,
        start: None,
    },
    // EV2a's masternode list (`dml`): the O2a-EV entry points are in the v24 shim only.
    Target {
        name: "diff_mnlist_codec",
        compare: dml::compare_codec,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_mnlist_diff",
        compare: dml::compare_diff,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_mnlist_order",
        compare: dml::compare_order,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_unique_hash",
        compare: dml::compare_unique_hash,
        pins: V24,
        start: None,
    },
    // EV2b's block build: Core's `RebuildListFromBlock` (O2a-EV).
    Target {
        name: "diff_dml_apply",
        compare: dml_apply::compare_apply,
        pins: V24,
        start: None,
    },
    // EV2b's validated block: Core's check loop, build and ProcessBlock (ABI 26, D34/D40).
    Target {
        name: "diff_dml_block",
        compare: dml_block::compare_block,
        pins: V24,
        start: None,
    },
    Target {
        name: "mnlist_payee_queue",
        compare: dml_payee::compare_payee_queue,
        pins: V24,
        start: None,
    },
    // LQ1a: quorum member selection (`llmq`).
    Target {
        name: "diff_quorum_members",
        compare: compare_quorum_members,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_type_enabled",
        compare: compare_type_enabled,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_scan_quorums",
        compare: compare_scan_quorums,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_select_quorum",
        compare: compare_select_quorum,
        pins: V24,
        start: None,
    },
    // BS11a-1: `dash-pkc`'s BLS decode (`bls`), against the v24 shim.
    Target {
        name: "diff_bls_decode",
        compare: bls::compare_bls_decode,
        pins: V24,
        start: None,
    },
    // BS11a-3: `dash-pkc`'s BLS element arithmetic and IES key (`bls_arith`), against the
    // v24 shim's ABI 26 ops.
    Target {
        name: "diff_bls_arith",
        compare: bls_arith::compare_bls_arith,
        pins: V24,
        start: None,
    },
    // BS11b: `dash-pkc`'s remaining BLS wrapper operations (`bls_wrapper`): aggregates and
    // the secure aggregate, threshold shares and recovery, DH and multi-recipient BLS-IES.
    Target {
        name: "diff_bls_aggregate",
        compare: bls_wrapper::compare_bls_aggregate,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_bls_threshold",
        compare: bls_wrapper::compare_bls_threshold,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_bls_ies",
        compare: bls_wrapper::compare_bls_ies,
        pins: V24,
        start: None,
    },
    // BS11a-4 D10: `dash-pkc`'s three verifications on every operand class (`bls_wrapper`).
    Target {
        name: "diff_bls_verify",
        compare: bls_wrapper::compare_bls_verify,
        pins: V24,
        start: None,
    },
    // LQ1b: DIP0024 rotation (`llmq`).
    Target {
        name: "diff_quorum_members_rot",
        compare: compare_quorum_members_rot,
        pins: V24,
        start: None,
    },
    // O1c: Core's AddrMan and peer sessions (`p2p`); SB6 takes them over.
    Target {
        name: "diff_addrman",
        compare: compare_addrman,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_multi_handshake",
        compare: compare_p2p_multi_handshake,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_multi_headers",
        compare: compare_p2p_multi_headers,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_multi_relay",
        compare: compare_p2p_multi_relay,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_headers",
        compare: compare_p2p_headers,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_headers2",
        compare: compare_p2p_headers2,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_inv",
        compare: compare_p2p_inv,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_getdata",
        compare: compare_p2p_getdata,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_notfound",
        compare: compare_p2p_notfound,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_bsdk_tx_decode",
        compare: compare_bsdk_tx,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_bsdk_block_decode",
        compare: compare_bsdk_block,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_bsdk_header_decode",
        compare: compare_bsdk_header,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_decode",
        compare: compare_payload,
        pins: V24,
        start: None,
    },
    // P2a-net: address relay (`p2p_addr`).
    Target {
        name: "diff_p2p_addr",
        compare: compare_p2p_addr,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_addrv2",
        compare: compare_p2p_addrv2,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_p2p_getaddr",
        compare: compare_p2p_getaddr,
        pins: V24,
        start: None,
    },
    // P2a-net BL-203: self-announcement, out of IBD.
    Target {
        name: "diff_p2p_selfaddr",
        compare: compare_p2p_selfaddr,
        pins: V24,
        start: None,
    },
    // O2b-3a: Core alone until O2b-3b adds NodeSut.
    Target {
        name: "diff_chain",
        compare: chain::fuzz_one,
        pins: V24,
        start: Some(chain::run::start),
    },
    Target {
        name: "diff_payload_1",
        compare: |bytes, pin| payload::compare_payload(1, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_2",
        compare: |bytes, pin| payload::compare_payload(2, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_3",
        compare: |bytes, pin| payload::compare_payload(3, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_4",
        compare: |bytes, pin| payload::compare_payload(4, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_5",
        compare: |bytes, pin| payload::compare_payload(5, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_6",
        compare: |bytes, pin| payload::compare_payload(6, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_7",
        compare: |bytes, pin| payload::compare_payload(7, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_8",
        compare: |bytes, pin| payload::compare_payload(8, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_9",
        compare: |bytes, pin| payload::compare_payload(9, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_10",
        compare: |bytes, pin| payload::compare_payload(10, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_11",
        compare: |bytes, pin| payload::compare_payload(11, bytes, pin),
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_payload_12",
        compare: |bytes, pin| payload::compare_payload(12, bytes, pin),
        pins: V24,
        start: None,
    },
    // O2a-CJ1: dash-coinjoin's session-free rules (`coinjoin`).
    Target {
        name: "diff_coinjoin",
        compare: coinjoin::compare_coinjoin,
        pins: V24,
        start: None,
    },
    // GV1a: dash-governance's objects, proposals, triggers, votes and tallies (`gov`).
    Target {
        name: "diff_gov",
        compare: gov::compare_gov,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_gov_vote",
        compare: gov::compare_gov_vote,
        pins: V24,
        start: None,
    },
    // EV3: the coinbase value and payee checks (`payments`).
    Target {
        name: "diff_block_value_payee",
        compare: payments::compare_block_value_payee,
        pins: V24,
        start: Some(payments::start),
    },
    // EV3-2: the credit pool (`credit_pool`).
    Target {
        name: "diff_credit_pool",
        compare: credit_pool::compare_credit_pool,
        pins: V24,
        start: Some(credit_pool::start),
    },
    // EV3-3: the MNHF signals (`mnhf`).
    Target {
        name: "diff_mnhf_step",
        compare: mnhf::compare_mnhf_step,
        pins: V24,
        start: Some(mnhf::start),
    },
    // CH2c: CH2's coins cache and block overlay against Core's coins views, in the aligned and
    // hidden modes, with Core's DynamicMemoryUsage per op (`coins_ops`).
    Target {
        name: "diff_coins_ops",
        compare: coins_ops::compare_coins_ops_target,
        pins: V24,
        start: None,
    },
    // BL-451b: SP1's sync, fulfilled requests, meta cache and their flat files against Core's
    // (`mnsync`, D24).
    Target {
        name: "diff_mnsync",
        compare: mnsync::compare_mnsync,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_netfulfilled",
        compare: mnsync::compare_netfulfilled,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_mncache",
        compare: mnsync::compare_mncache,
        pins: V24,
        start: None,
    },
    Target {
        name: "diff_flatdb_load",
        compare: mnsync::compare_flatdb,
        pins: V24,
        start: None,
    },
];

/// The registered target named `name`.
pub fn target(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}

/// The pin named as in `pins.txt` (`v24.0`).
pub fn pin_named(name: &str) -> Option<Pin> {
    Pin::ALL.into_iter().find(|pin| pin.name() == name)
}

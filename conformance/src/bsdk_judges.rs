//! The BS4b leg: the decode judges of `node-l1` (`bsdk_decode`) over chain data. Each block is
//! judged as a header, as a block and as its transactions, and each special-transaction payload
//! (types 1 to 12) under both BLS scheme flags, against Core's side, which is the bytes the
//! chain holds (Dash Core serialized them, and the oracle served them) and, for a payload, the
//! oracle's `GetTxPayload` deserializer. The judges are the ones the `diff_bsdk_*` fuzz targets
//! and the census run, so this is the same comparison over the whole history.
//!
//! Every record is `bsdk.*` against base-sdk and names a field, never a value. Built with the
//! `bsdk-judges` feature (it links the oracle shim).

use std::sync::OnceLock;

use dash_oracle_sys::{Oracle, Pin};
use node_l1::bsdk_decode::{
    Agreement, CoreDecode, CorePayload, PAYLOAD_TYPES, judge_block, judge_header, judge_payload,
    judge_tx,
};

use crate::check::{Sink, TxRef};
use crate::report::{Side, Stats};
use crate::split::TxSpan;

const SIDE: Side = Side::BaseSdk;

/// The header's length.
const HEADER_LEN: usize = 80;

fn oracle() -> &'static Oracle {
    static ORACLE: OnceLock<Oracle> = OnceLock::new();
    ORACLE.get_or_init(|| Oracle::new(Pin::V24_0))
}

/// Core's side of an object it serialized: it read every byte and writes the same bytes.
fn chain_side(bytes: &[u8]) -> CoreDecode {
    CoreDecode {
        consumed: bytes.len(),
        reencoded: bytes.to_vec(),
    }
}

/// Runs the leg on one block (at least a header's length). `txs` are the block's transactions as
/// the other legs report them.
pub(crate) fn check(
    sink: &mut Sink<'_>,
    bytes: &[u8],
    spans: Option<&[TxSpan]>,
    txs: &[TxRef<'_>],
    stats: &mut Stats,
) {
    stats.bsdk_blocks += 1;
    let header = &bytes[..HEADER_LEN.min(bytes.len())];
    if let Err(field) = judge_header(header, Some(&chain_side(header))) {
        sink.fail("bsdk.header", SIDE, None, field);
    }
    if let Err(field) = judge_block(bytes, Some(&chain_side(bytes))) {
        sink.fail("bsdk.block", SIDE, None, field);
    }
    let Some(spans) = spans else {
        return;
    };
    for (i, span) in spans.iter().enumerate() {
        let txref = txs.get(i).copied();
        let raw = &bytes[span.range.clone()];
        stats.bsdk_txs += 1;
        if let Err(field) = judge_tx(raw, Some(&chain_side(raw))) {
            sink.fail("bsdk.tx", SIDE, txref, field);
        }
        let Some(range) = span.payload.clone() else {
            continue;
        };
        if !(1..=PAYLOAD_TYPES).contains(&span.tx_type) {
            continue;
        }
        stats.bsdk_payloads += 1;
        let payload = &bytes[range];
        let core = match node_l1::core_payload(oracle(), span.tx_type, payload) {
            Ok(core) => core,
            Err(e) => {
                sink.fail("bsdk.payload", Side::Harness, txref, format!("the oracle: {e:?}"));
                continue;
            }
        };
        for (core, legacy) in core.iter().zip([false, true]) {
            let core: Option<&CorePayload> = core.as_ref();
            let flag = if legacy {
                "legacy"
            } else {
                "basic"
            };
            match judge_payload(span.tx_type, legacy, payload, core) {
                // A payload of Core's own best chain is accepted under either flag, and read
                // to its end: both sides rejecting it, or neither reading it all, is not
                // agreement. The oracle refusing chain bytes is the oracle's (or the shim's)
                // fault, as the other oracle failures are.
                Ok(Agreement::BothAccept) if core.is_some_and(|c| c.consumed_all) => {}
                Ok(_) => sink.fail(
                    "bsdk.payload",
                    Side::Harness,
                    txref,
                    format!("chain payload not accepted in full (type {} {flag})", span.tx_type),
                ),
                Err(field) => sink.fail(
                    "bsdk.payload",
                    SIDE,
                    txref,
                    format!("type {} {flag}: {field}", span.tx_type),
                ),
            }
        }
    }
}

//! The `dash_consensus` leg (CS0 §9.2): the node's own decoder, serializer and hashes, checked
//! against the same references as the two libraries, with no other library involved.
//!
//! Per block:
//! - `decode_block` consumes exactly the block's bytes, and the block, so every transaction in
//!   it, is `wire_is_canonical`. Stored and RPC bytes are Core's serialization and the
//!   comparison is exact, so any other result is a bug in the serializer or the comparison; the
//!   leg then decodes each transaction alone to name the one that differs;
//! - `core_serialize` and `Decoded::core_bytes` give back the bytes, and `size` is their length;
//! - the block hash equals the reference (the oracle's, or the next block's link);
//! - every txid equals the reference txid (`getblock 1`, else Core's rule over the raw bytes,
//!   [`crate::check::core_txid`]), and the merkle root equals the reference root, unmutated
//!   (v24, the release of the oracles and of the blk files' dashd);
//!
//! Every `consensus.*` record is against the leg. Where the reference is the harness's own (blk
//! mode's txid rule and header root), the libraries' records for the same block show whether the
//! reference itself is suspect: all three disagreeing with it alike points at the harness.

use dash_consensus::{core_serialize, decode_block, decode_tx_exact};

use crate::check::{H256, Sink, TxRef, rev_hex};
use crate::report::{Side, Stats};
use crate::split::TxSpan;

const SIDE: Side = Side::DashConsensus;

/// The references one block is checked against, as the other legs use them.
pub(crate) struct Refs<'a> {
    /// The block hash: the oracle's, or the next block's `prev` link.
    pub hash: Option<&'a H256>,
    /// The oracle's merkle root, else the header's.
    pub merkle_root: &'a H256,
    /// Per transaction: the oracle's txid, else Core's txid rule over the raw bytes.
    pub txids: &'a [Option<H256>],
    /// Per transaction, for reports.
    pub txs: &'a [TxRef<'a>],
    /// The reference split, to decode transactions one by one.
    pub spans: Option<&'a [TxSpan]>,
}

/// Runs the leg on one block (at least a header's length).
pub(crate) fn check(sink: &mut Sink<'_>, bytes: &[u8], refs: &Refs<'_>, stats: &mut Stats) {
    stats.consensus_blocks += 1;
    let mut cursor = bytes;
    let decoded = match decode_block(&mut cursor) {
        Ok(decoded) => decoded,
        Err(e) => {
            // The decoder's own transaction index may not be the split's (if it misread an
            // earlier length), so the record names none; the split decode below names it.
            sink.fail("consensus.decode", SIDE, None, e.to_string());
            check_txs_alone(sink, bytes, refs);
            return;
        }
    };
    if !cursor.is_empty() {
        let detail = format!("{} trailing bytes not consumed", cursor.len());
        sink.fail("consensus.decode", SIDE, None, detail);
    }
    if !decoded.wire_is_canonical() {
        sink.fail("consensus.canonical", SIDE, None, "the bytes are not Core's serialization");
        check_txs_alone(sink, bytes, refs);
    }
    let block = decoded.value();
    if core_serialize(block) != bytes {
        sink.fail("consensus.serialize", SIDE, None, "core_serialize differs from the bytes");
    }
    if decoded.core_bytes().as_slice() != bytes {
        sink.fail("consensus.core_bytes", SIDE, None, "core_bytes differs from the bytes");
    }
    if block.size() as usize != bytes.len() {
        let (e, a) = (Some(bytes.len().to_string()), Some(block.size().to_string()));
        sink.push("consensus.size", SIDE, None, None, e, a, "");
    }
    if let Some(reference) = refs.hash
        && block.hash().as_bytes() != reference
    {
        let (e, a) = (Some(rev_hex(reference)), Some(rev_hex(block.hash().as_bytes())));
        sink.push("consensus.hash", SIDE, None, None, e, a, "");
    }

    let txs = block.txs();
    if txs.len() != refs.txids.len() {
        let (e, a) = (Some(refs.txids.len().to_string()), Some(txs.len().to_string()));
        sink.push("consensus.tx_count", SIDE, None, None, e, a, "");
    }
    for (i, tx) in txs.iter().enumerate() {
        let txref = refs.txs.get(i).copied();
        let txid: H256 = *tx.txid().as_bytes();
        if let Some(Some(reference)) = refs.txids.get(i)
            && txid != *reference
        {
            let (e, a) = (Some(rev_hex(reference)), Some(rev_hex(&txid)));
            sink.push("consensus.txid", SIDE, txref, None, e, a, "");
        }
    }

    let (root, mutated) = block.merkle();
    if root.as_bytes() != refs.merkle_root {
        let (e, a) = (Some(rev_hex(refs.merkle_root)), Some(rev_hex(root.as_bytes())));
        sink.push("consensus.merkle_root", SIDE, None, None, e, a, "");
    }
    if mutated {
        sink.fail("consensus.mutated", SIDE, None, "");
    }
}

/// Decodes each transaction of the reference split on its own, to name the one that does not
/// decode or is not canonical.
fn check_txs_alone(sink: &mut Sink<'_>, bytes: &[u8], refs: &Refs<'_>) {
    let Some(spans) = refs.spans else {
        return;
    };
    for (i, span) in spans.iter().enumerate() {
        let txref = refs.txs.get(i).copied();
        match decode_tx_exact(&bytes[span.range.clone()]) {
            Err(e) => sink.fail("consensus.tx_decode", SIDE, txref, e.to_string()),
            Ok(tx) if !tx.wire_is_canonical() => {
                sink.fail("consensus.tx_canonical", SIDE, txref, "not Core's serialization")
            }
            Ok(_) => {}
        }
    }
}

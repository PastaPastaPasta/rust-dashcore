//! `DigestTap`: rdashd's StateDigest log (spec O8-statedigest-v1; node program task O9a-2), as a
//! [`BlockConnector`] decorator. Each `connect_block` call writes one `C` or `R` and each
//! `disconnect_block` call one `D`, numbered by one `seq` (§4.2–§4.4, §8.1), after an `H` written
//! when the tap opens (§4.1).
//!
//! What it records is what V hands a connector:
//! - a `C` holds the header keys of §5.1 only (from the index entry and the block); the derived
//!   and golden keys (§5.2, §5.3) need CH3a's `ConnectBlock`, so they stay absent, uncovered in a
//!   comparison rather than claimed;
//! - `fx` stays absent: no effect is observed here (§6);
//! - every record has `org:"tip"`, and a `TestBlockValidity` call is passed through unlogged:
//!   the tap does not read [`ConnectCx::origin`] yet (CH3 §2.15, slice a7);
//! - `D.res` is the seam's [`DisconnectResult`]: `ok`, `unclean` or `failed` (§4.4).
//!
//! A failed write is an `AbortNode` (§8.3): a log with gaps is worse than none.

use std::io::Write;

use dash_chain::EntryView;
use dash_consensus::{Block, BlockId, BlockValidationResult, BlsCtx};
use dash_statedigest::emit::{self, HeaderFields, RejectResult};
use dash_statedigest::{Hash256, Header, Ident, Origin, Outcome, Record};

use super::actor::VContext;
use super::seams::{
    AttemptOrigin, BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult,
    Fatal, ProposalChecks, ViewTarget,
};

/// A connector that logs each attempt of `inner` to `out`.
pub struct DigestTap<C, W> {
    inner: C,
    out: W,
    seq: i64,
}

impl<C: BlockConnector, W: Write + Send> DigestTap<C, W> {
    /// A tap around `inner` that has written `header` to `out`.
    pub fn new(inner: C, mut out: W, header: Header) -> Result<DigestTap<C, W>, Fatal> {
        write_line(&mut out, &Record::Header(header).to_line())?;
        Ok(DigestTap {
            inner,
            out,
            seq: 0,
        })
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    fn write(
        &mut self,
        record: Result<Outcome, dash_statedigest::FieldError>,
    ) -> Result<(), Fatal> {
        let record = record.map_err(|e| Fatal::new(format!("statedigest: {e}")))?;
        write_line(&mut self.out, &Record::Outcome(record).to_line())
    }
}

pub(crate) fn write_line(out: &mut impl Write, line: &[u8]) -> Result<(), Fatal> {
    out.write_all(line)
        .and_then(|()| out.flush())
        .map_err(|e| Fatal::new(format!("statedigest: writing the digest log: {e}")))
}

/// A hash's display-order hex (`GetHex()`), as the record keeps it.
pub(crate) fn display(hex: impl std::fmt::Display) -> Result<Hash256, Fatal> {
    let text = format!("{hex:0>64}");
    Hash256::from_hex(text.as_bytes())
        .ok_or_else(|| Fatal::new(format!("statedigest: {text} is not a 256-bit hex")))
}

/// `D.res` (§4.4). A `Fatal` is an exception escaping `DisconnectBlock`: `failed`.
pub(crate) fn disconnect_res(done: &Result<DisconnectResult, Fatal>) -> &'static str {
    match done {
        Ok(DisconnectResult::Ok) => "ok",
        Ok(DisconnectResult::Unclean) => "unclean",
        Ok(DisconnectResult::Failed) | Err(_) => "failed",
    }
}

/// The record's `org` for an attempt's origin (§4.2, CH3 D17).
pub(crate) fn origin(origin: AttemptOrigin) -> Origin {
    match origin {
        AttemptOrigin::Tip => Origin::Tip,
        AttemptOrigin::Proposal => Origin::Proposal,
        AttemptOrigin::VerifyDb => Origin::Verifydb,
        AttemptOrigin::Replay => Origin::Replay,
    }
}

/// `BlockValidationResult`'s name without `BLOCK_`, in lower case (§4.3).
pub(crate) fn class_name(result: BlockValidationResult) -> &'static str {
    match result {
        BlockValidationResult::Unset => "result_unset",
        BlockValidationResult::Consensus => "consensus",
        BlockValidationResult::RecentConsensusChange => "recent_consensus_change",
        BlockValidationResult::CachedInvalid => "cached_invalid",
        BlockValidationResult::InvalidHeader => "invalid_header",
        BlockValidationResult::Mutated => "mutated",
        BlockValidationResult::MissingPrev => "missing_prev",
        BlockValidationResult::InvalidPrev => "invalid_prev",
        BlockValidationResult::TimeFuture => "time_future",
        BlockValidationResult::Checkpoint => "checkpoint",
        BlockValidationResult::ChainLock => "chainlock",
    }
}

pub(crate) fn entry(cx: &ConnectCx<'_>, id: BlockId) -> Result<EntryView, Fatal> {
    cx.reader
        .entry(id)
        .ok_or_else(|| Fatal::new(format!("statedigest: node entry {} is not resident", id.0)))
}

/// The record's identity: the entry's height, the block's hash, `tip`.
fn ident(entry: &EntryView) -> Result<Ident, Fatal> {
    Ok(Ident {
        h: entry.height.into(),
        b: display(entry.hash)?,
        org: Origin::Tip,
    })
}

/// The §5.1 keys of `block`, whose entry is `entry`.
pub(crate) fn header_fields(
    cx: &ConnectCx<'_>,
    block: &Block,
    entry: &EntryView,
) -> Result<HeaderFields, Fatal> {
    let prev = match entry.prev {
        None => None,
        Some(p) => Some(display(self::entry(cx, p)?.hash)?),
    };
    let h = block.header();
    Ok(HeaderFields {
        prev,
        ver: h.version(),
        mrkl: display(h.merkle_root())?,
        time: h.time(),
        bits: h.bits(),
        nonce: h.nonce(),
        work: display(entry.chain_work)?,
        ntx: block.txs().len() as u64,
    })
}

impl<C: BlockConnector, W: Write + Send> BlockConnector for DigestTap<C, W> {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let ident = ident(&entry(cx, id)?)?;
        let outcome = self.inner.connect_block(cx, v, target, block, checked, id, st);
        let seq = self.next_seq();
        let record = match &outcome {
            // The connect may have raised the entry's status, so it is read again.
            Ok(ConnectOutcome::Connected) => {
                emit::connect(ident, &header_fields(cx, block, &entry(cx, id)?)?, seq)
            }
            Ok(ConnectOutcome::Invalid(e)) => {
                let result = RejectResult::Invalid {
                    cls: class_name(e.result()),
                    why: e.reject_reason(),
                };
                let dbg = Some(e.debug_message()).filter(|d| !d.is_empty());
                emit::reject(ident, result, dbg, seq)
            }
            Ok(ConnectOutcome::SystemError(why))
            | Err(Fatal {
                0: why,
                ..
            }) => {
                let why = if why.is_empty() {
                    "unset"
                } else {
                    why
                };
                emit::reject(
                    ident,
                    RejectResult::Error {
                        why,
                    },
                    None,
                    seq,
                )
            }
        };
        self.write(record)?;
        outcome
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        let ident = ident(&entry(cx, id)?)?;
        let done = self.inner.disconnect_block(cx, v, target, block, id);
        let seq = self.next_seq();
        self.write(emit::disconnect(ident, disconnect_res(&done), seq))?;
        done
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        self.inner.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.inner.published_bls()
    }
}

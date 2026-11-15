//! One block of Core's MNHF signal processing (ABI 26, the context's tip-cache seed ABI 28;
//! `src/test/oracle/oracle.h`, "MNHF signals").
//!
//! [`Oracle::mnhf_step`] runs `CMNHFManager::ProcessBlock()` for a block on top of the chain
//! context's tip, as `ProcessSpecialTxsInBlock()` calls it, over Core's own manager reading the
//! signals each header's block holds ([`SignalChange`], as for
//! [`Oracle::versionbits_state`]). A connected block also gets the next block's stage and
//! `UndoBlock()`.

use super::addrman::{take_flag, take_le, take_list, take_u8};
use super::*;

/// What an MNHF step reads besides the block and the chain context (oracle.h "MNHF context").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MnhfContext {
    /// `ProcessBlock()`'s `fJustCheck`: false as `ConnectBlock()` connects a block, true as
    /// `TestBlockValidity()` checks a template or proposal.
    pub just_check: bool,
    /// The signals each header's block holds (`CMNHFManager::GetForBlock()`'s rows).
    pub signals: Vec<SignalChange>,
    /// Put a change at the tip, where V20 is not active, in the tip's entry of Core's manager
    /// cache (`CMNHFManager::AddToCache()`), as the node's `ProcessBlock()` of the tip left it:
    /// at `V20Height - 1` the next block's stage reads it (O2a-MNHF D13, BL-211). The entry holds
    /// the tip's own bits, so each seeded signal must be at the tip's height. No effect at a tip
    /// where V20 is active, whose row the cache would hold.
    pub seed_tip_cache: bool,
}

impl MnhfContext {
    /// The shim's MNHF-context bytes: format 1, or format 2 (ABI 28) when seeding the tip's
    /// cache entry.
    pub fn serialize(&self) -> Vec<u8> {
        let format = if self.seed_tip_cache {
            2
        } else {
            1
        };
        let mut out = vec![format, u8::from(self.just_check)];
        versionbits::write_changes(&mut out, &self.signals);
        if self.seed_tip_cache {
            out.push(1);
        }
        out
    }
}

/// A `CMNHFManager::Signals` map: `(version bit, height of the block that mined its signal)` in
/// bit order.
pub type Signals = Vec<(u8, i32)>;

/// What follows a connected block (`just_check` false, accepted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MnhfConnected {
    /// `GetSignalsStage(block)`: the signals the next block is checked against.
    pub stage_after: Signals,
    /// `UndoBlock(block)`, as `DisconnectBlock()` calls it.
    pub undo_ok: bool,
    /// `GetSignalsStage(tip)` after the undo.
    pub stage_undone: Signals,
}

/// Result of [`Oracle::mnhf_step`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MnhfStep {
    /// `ProcessBlock()`'s verdict.
    pub verdict: Verdict,
    /// `GetSignalsStage(tip)`: the signals the block is checked against.
    pub stage_before: Signals,
    /// `ProcessBlock()`'s result when it accepted: the block's row (`stage_before` plus its new
    /// signals at its height), or with `just_check` `stage_before` alone.
    pub signals: Option<Signals>,
    /// When the block was connected.
    pub connected: Option<MnhfConnected>,
}

/// A `std::map<uint8_t, int>`: bits strictly ascending.
fn take_signals(r: &mut &[u8]) -> Option<Signals> {
    let signals: Signals = take_list(r, |r| Some((take_u8(r)?, take_le(r)?)))?;
    signals.windows(2).all(|w| w[0].0 < w[1].0).then_some(signals)
}

fn parse_mnhf_step(mut r: &[u8], verdict: Verdict) -> Option<MnhfStep> {
    let r = &mut r;
    let stage_before = take_signals(r)?;
    let has_signals = take_flag(r)?;
    let signals = take_signals(r)?;
    let has_after = take_flag(r)?;
    let stage_after = take_signals(r)?;
    let undo_ok = take_flag(r)?;
    let stage_undone = take_signals(r)?;
    // The shim writes empty maps and false flags for the parts that did not run, has a result
    // exactly for an accepted block, and connects only an accepted block.
    let consistent = has_signals == verdict.is_accept()
        && (has_signals || signals.is_empty())
        && (!has_after || has_signals)
        && (has_after || (stage_after.is_empty() && !undo_ok && stage_undone.is_empty()));
    (r.is_empty() && consistent).then(|| MnhfStep {
        verdict,
        stage_before,
        signals: has_signals.then_some(signals),
        connected: has_after.then_some(MnhfConnected {
            stage_after,
            undo_ok,
            stage_undone,
        }),
    })
}

impl Oracle {
    /// `CMNHFManager::ProcessBlock()` for `block` on top of the context's tip; for a connected
    /// block also the next block's stage and `UndoBlock()`. [`ErrorKind::Context`] when Core
    /// reads a median time past whose blocks are below the headers; [`ErrorKind::Argument`] for
    /// a block that does not extend the tip or does not start with a coinbase, a signal bit at
    /// or above `VERSIONBITS_NUM_BITS`, malformed signal changes, or a non-empty change at a
    /// header where V20 is not active other than one at the tip with
    /// [`MnhfContext::seed_tip_cache`] (O2a-MNHF D14), or a seeded signal whose height is not the
    /// tip's.
    pub fn mnhf_step(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        mnhf: &MnhfContext,
    ) -> Result<MnhfStep> {
        self.mnhf_step_raw(chain, block, ctx, &mnhf.serialize())
    }

    /// [`Oracle::mnhf_step`] with the MNHF context's bytes as given, for contexts
    /// [`MnhfContext::serialize`] never writes.
    pub fn mnhf_step_raw(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        mnhf: &[u8],
    ) -> Result<MnhfStep> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let record = self.with_buffer(1024, |out, len| {
                let status = unsafe {
                    (self.api.mnhf_step)(
                        block.as_ptr(),
                        block.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        mnhf.as_ptr(),
                        mnhf.len(),
                        out,
                        len,
                        &mut state,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            parse_mnhf_step(&record, block_verdict(accepted, &state))
                .ok_or_else(|| payments::malformed("oracle_mnhf_step"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versionbits::write_signals;

    /// `stage_before`, then `(has_signals, signals)`, `(has_after, stage_after)` and
    /// `(undo_ok, stage_undone)`.
    fn record(before: &[(u8, i32)], rest: [(u8, &[(u8, i32)]); 3]) -> Vec<u8> {
        let mut out = Vec::new();
        write_signals(&mut out, before);
        for (flag, list) in rest {
            out.push(flag);
            write_signals(&mut out, list);
        }
        out
    }

    fn rejected() -> Verdict {
        Verdict::Reject(Rejection {
            result: ResultCode::Block(1),
            reason: "bad-mnhf-duplicate".into(),
            debug_message: String::new(),
            reason_truncated: false,
            debug_truncated: false,
        })
    }

    #[test]
    fn contexts_serialize_as_formats_1_and_2() {
        let change = SignalChange {
            height: 7,
            signals: vec![(13, 7)],
        };
        let mut ctx = MnhfContext {
            just_check: true,
            signals: vec![change],
            seed_tip_cache: false,
        };
        // format, just_check, one change (height LE, one signal: bit, height LE).
        let format1 = [1, 1, 1, 7, 0, 0, 0, 1, 13, 7, 0, 0, 0];
        assert_eq!(ctx.serialize(), format1);
        ctx.seed_tip_cache = true;
        let mut format2 = format1.to_vec();
        format2[0] = 2;
        format2.push(1);
        assert_eq!(ctx.serialize(), format2);
    }

    #[test]
    fn records_parse_exactly() {
        let connected =
            record(&[(28, 5)], [(1, &[(3, 9), (28, 5)]), (1, &[(3, 9)]), (1, &[(28, 5)])]);
        let parsed = parse_mnhf_step(&connected, Verdict::Accept).unwrap();
        assert_eq!(parsed.stage_before, vec![(28, 5)]);
        assert_eq!(parsed.signals, Some(vec![(3, 9), (28, 5)]));
        assert_eq!(
            parsed.connected,
            Some(MnhfConnected {
                stage_after: vec![(3, 9)],
                undo_ok: true,
                stage_undone: vec![(28, 5)],
            })
        );
        assert!(parse_mnhf_step(&connected[..connected.len() - 1], Verdict::Accept).is_none());
        assert!(parse_mnhf_step(&[connected.as_slice(), &[0]].concat(), Verdict::Accept).is_none());

        let rejection = record(&[(28, 5)], [(0, &[]), (0, &[]), (0, &[])]);
        let parsed = parse_mnhf_step(&rejection, rejected()).unwrap();
        assert_eq!((parsed.signals, parsed.connected), (None, None));

        // Records the shim never writes: a result that does not match the verdict, signals after
        // a reject, a connect after a reject, undo results without a connect, a flag above 1,
        // and a map out of bit order.
        for (bad, verdict) in [
            (record(&[], [(0, &[]), (0, &[]), (0, &[])]), Verdict::Accept),
            (record(&[], [(1, &[]), (0, &[]), (0, &[])]), rejected()),
            (record(&[], [(0, &[(1, 1)]), (0, &[]), (0, &[])]), rejected()),
            (record(&[], [(0, &[]), (1, &[]), (1, &[])]), rejected()),
            (record(&[], [(1, &[]), (0, &[]), (1, &[])]), Verdict::Accept),
            (record(&[], [(1, &[]), (0, &[(1, 1)]), (0, &[])]), Verdict::Accept),
            (record(&[], [(2, &[]), (0, &[]), (0, &[])]), Verdict::Accept),
            (record(&[(5, 1), (3, 1)], [(1, &[]), (0, &[]), (0, &[])]), Verdict::Accept),
        ] {
            assert!(parse_mnhf_step(&bad, verdict).is_none(), "{bad:?}");
        }
    }
}

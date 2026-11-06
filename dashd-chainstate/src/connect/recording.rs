//! [`RecordingHelper`]: a [`ChainHelper`] that forwards every call to the helper it wraps and
//! records the attempt's queries as O8's `A` (§7.1), and [`TapedLog`], the [`DigestSink`] that
//! writes each `C` or `R` with its `A` after it, as O8's patch's `Finish` does
//! (`statedigest.cpp:121-126`). O2b's records and attempts planes read the pair (CH3a a10 Q7).
//!
//! - **The attempt.** It opens at [`ChainHelper::begin_attempt`] (CH3a D125) with `t`, the
//!   adjusted time then (`statedigest.cpp:189`), and closes at [`ChainHelper::end_attempt`],
//!   before the connector's record of it. An attempt that asked nothing has an empty `q`; a `D`
//!   has no `A` (`:125`); a proposal rejected before `ConnectBlock` opens none, so has neither.
//! - **The queries**, `[name, args…, answer]` in call order, the answer the wrapped helper gave.
//!   `remove_conflicting_is_lock` answers none and G2's `interim_refusal` is not an O8 query:
//!   neither is recorded.
//!
//! Not [`ReplayChainHelper`](super::ReplayChainHelper): nothing is answered from a recording.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use dash_clock::Clock;
use dash_consensus::Tx;
use dash_evo::EffectSink;
use dash_primitives::{BlockHash, TxOut};
use dash_statedigest::{
    Attempt, BestChainLock, Hash256, Ident, IsConflictPair, Query, Record, SbPayment, Value,
};

use super::digest::{DigestAttempt, DigestSink};
use super::digest_log::DigestLog;
use super::evo::BlockAt;
use super::helper::{ChainHelper, IsLockRef};
use crate::chain::digest_tap::{display, origin, write_line};
use crate::chain::{ChainLockSig, ConnectCx, Fatal};

/// The closed attempt's `A`, from its [`RecordingHelper`] to its [`TapedLog`]. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct DecisionTap(Arc<Mutex<Option<Attempt>>>);

impl DecisionTap {
    pub fn new() -> DecisionTap {
        DecisionTap::default()
    }

    fn put(&self, attempt: Attempt) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(attempt);
    }

    fn take(&self) -> Option<Attempt> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// A helper, its queries recorded.
pub struct RecordingHelper {
    inner: Box<dyn ChainHelper>,
    tap: DecisionTap,
    clock: Arc<dyn Clock>,
    open: Option<Attempt>,
}

impl RecordingHelper {
    /// `inner`, each closed attempt's `A` put in `tap`, its `t` from `clock` (`GetAdjustedTime()`
    /// with no peer offset).
    pub fn new(
        inner: Box<dyn ChainHelper>,
        tap: DecisionTap,
        clock: Arc<dyn Clock>,
    ) -> RecordingHelper {
        RecordingHelper {
            inner,
            tap,
            clock,
            open: None,
        }
    }

    /// `answer`, with `query` of it recorded in the open attempt.
    fn record<T>(
        &mut self,
        answer: Result<T, Fatal>,
        query: impl FnOnce(&T) -> Result<Query, Fatal>,
    ) -> Result<T, Fatal> {
        let answer = answer?;
        if let Some(open) = &mut self.open {
            open.q.push(query(&answer)?);
        }
        Ok(answer)
    }
}

fn height(h: u32) -> i64 {
    h.into()
}

// Core: chainlock/clsig.h ChainLockSig::IsNull
fn best(sig: Option<&Arc<ChainLockSig>>) -> Result<Option<BestChainLock>, Fatal> {
    let Some(sig) = sig.filter(|s| ***s != ChainLockSig::null()) else {
        return Ok(None);
    };
    Ok(Some(BestChainLock {
        height: sig.height.into(),
        hash: display(sig.block_hash)?,
        sig: sig.sig,
    }))
}

impl ChainHelper for RecordingHelper {
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        let answer = self.inner.cl_conflict(cx, h, hash);
        self.record(answer, |&answer| {
            Ok(Query::ClConflict {
                height: height(h),
                hash: display(hash)?,
                answer,
            })
        })
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        let answer = self.inner.ibd(cx);
        self.record(answer, |&answer| Ok(Query::Ibd(answer)))
    }

    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        let answer = self.inner.cl_has(cx, h, hash);
        self.record(answer, |&answer| {
            Ok(Query::ClHas {
                height: height(h),
                hash: display(hash)?,
                answer,
            })
        })
    }

    fn cl_best(&mut self, cx: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        let answer = self.inner.cl_best(cx);
        self.record(answer, |answer| Ok(Query::ClBest(best(answer.as_ref())?)))
    }

    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        let answer = self.inner.is_conflict(tx);
        self.record(answer, |answer| {
            let conflict = answer.map(|lock| IsConflictPair {
                islock: Hash256::from_wire_bytes(*lock.hash.as_bytes()),
                conflicting_txid: Hash256::from_wire_bytes(*lock.txid.as_bytes()),
            });
            Ok(Query::IsConflict {
                txid: display(tx.txid())?,
                conflict,
            })
        })
    }

    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        self.inner.remove_conflicting_is_lock(lock, sink);
    }

    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        self.inner.begin_attempt(cx, at)?;
        if self.open.is_some() {
            return Err(Fatal::new("RecordingHelper: an attempt began inside another"));
        }
        let id = Ident {
            h: height(at.height),
            b: display(at.hash)?,
            org: origin(cx.origin),
        };
        self.open = Some(Attempt::new(id, self.clock.node_now().secs()));
        Ok(())
    }

    fn end_attempt(&mut self) {
        self.inner.end_attempt();
        if let Some(attempt) = self.open.take() {
            self.tap.put(attempt);
        }
    }

    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        let answer = self.inner.sb_required(cx, at);
        self.record(answer, |&answer| {
            Ok(Query::SbRequired {
                height: height(at.height),
                answer,
            })
        })
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        let answer = self.inner.gov_valid();
        self.record(answer, |&answer| Ok(Query::GovValid(answer)))
    }

    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
        let answer = self.inner.sb_triggered(h);
        self.record(answer, |&answer| {
            Ok(Query::SbTriggered {
                height: height(h),
                answer,
            })
        })
    }

    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        let answer = self.inner.sb_payments(h);
        self.record(answer, |answer| {
            Ok(Query::SbPayments {
                height: height(h),
                payments: answer.as_ref().map(|list| {
                    list.iter()
                        .map(|out| SbPayment {
                            amount: out.value,
                            script: out.script_pubkey.to_vec(),
                        })
                        .collect()
                }),
            })
        })
    }

    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        self.inner.interim_refusal(cx, h)
    }
}

/// A [`DigestLog`] whose `C`s and `R`s are each followed by their attempt's `A` from a
/// [`DecisionTap`], with the record's `seq`, on `out`: the log's own writer in O8's memory mode,
/// where both streams are one, or a decisions file. A `C` or `R` without an `A`, or a `D` after
/// one, fails the record as a failed write does.
pub struct TapedLog<W> {
    log: DigestLog<W>,
    out: W,
    tap: DecisionTap,
    seq: i64,
    failed: bool,
}

impl<W: Write + Send> TapedLog<W> {
    /// `log`, with its `A`s from `tap` written to `out`.
    pub fn new(log: DigestLog<W>, out: W, tap: DecisionTap) -> TapedLog<W> {
        TapedLog {
            log,
            out,
            tap,
            seq: 0,
            failed: false,
        }
    }

    fn write(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        self.seq += 1;
        self.log.record(cx, attempt)?;
        match (attempt, self.tap.take()) {
            (DigestAttempt::Connect(_), Some(mut a)) => {
                a.x_mut()
                    .set("seq", Value::Int(self.seq))
                    .map_err(|e| Fatal::new(format!("statedigest: {e}")))?;
                write_line(&mut self.out, &Record::Attempt(a).to_line())
            }
            (DigestAttempt::Connect(_), None) => {
                Err(Fatal::new("statedigest: a connect attempt without its decisions"))
            }
            (DigestAttempt::Disconnect(_), None) => Ok(()),
            (DigestAttempt::Disconnect(_), Some(_)) => {
                Err(Fatal::new("statedigest: decisions without their connect attempt"))
            }
        }
    }
}

impl<W: Write + Send> DigestSink for TapedLog<W> {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        if self.failed {
            return Ok(());
        }
        let recorded = self.write(cx, attempt);
        self.failed = recorded.is_err();
        recorded
    }
}

#[cfg(test)]
mod tests {
    use dash_clock::MockClock;
    use dash_statedigest::Origin;

    use super::*;

    /// Governance's answers, fixed; nothing else is asked here.
    struct Gov;

    impl ChainHelper for Gov {
        fn cl_conflict(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
            unreachable!()
        }

        fn ibd(&mut self, _: &ConnectCx<'_>) -> Result<bool, Fatal> {
            unreachable!()
        }

        fn cl_has(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
            unreachable!()
        }

        fn cl_best(&mut self, _: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
            unreachable!()
        }

        fn is_conflict(&mut self, _: &Tx) -> Result<Option<IsLockRef>, Fatal> {
            unreachable!()
        }

        fn remove_conflicting_is_lock(&mut self, _: &IsLockRef, _: &mut dyn EffectSink) {}

        fn sb_required(&mut self, _: &ConnectCx<'_>, _: &BlockAt) -> Result<bool, Fatal> {
            unreachable!()
        }

        fn gov_valid(&mut self) -> Result<bool, Fatal> {
            Ok(true)
        }

        fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
            Ok(h == 20)
        }

        fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
            Ok((h == 20).then(|| {
                vec![TxOut {
                    value: 7,
                    script_pubkey: vec![0x51, 0xab].into(),
                }]
            }))
        }
    }

    /// O8 §7.1's encodings of the governance queries, each with its answer, in call order, only
    /// inside an attempt; the closed attempt goes to the tap.
    #[test]
    fn the_governance_queries_are_recorded_as_core_encodes_them() {
        let tap = DecisionTap::new();
        let mut helper =
            RecordingHelper::new(Box::new(Gov), tap.clone(), Arc::new(MockClock::new()));
        assert_eq!(helper.gov_valid(), Ok(true), "outside an attempt");
        let id = Ident {
            h: 20,
            b: Hash256::from_wire_bytes([3; 32]),
            org: Origin::Tip,
        };
        helper.open = Some(Attempt::new(id, 9));
        assert_eq!(helper.gov_valid(), Ok(true));
        assert_eq!(helper.sb_triggered(20), Ok(true));
        assert_eq!(helper.sb_triggered(21), Ok(false));
        assert!(helper.sb_payments(20).is_ok_and(|p| p.is_some()));
        assert_eq!(helper.sb_payments(21), Ok(None));
        helper.end_attempt();
        assert!(helper.open.is_none());
        let mut a = tap.take().expect("the closed attempt");
        a.x_mut().set("seq", Value::Int(4)).expect("seq");
        let line = String::from_utf8(Record::Attempt(a).to_line()).expect("utf-8");
        let b = "03".repeat(32);
        assert_eq!(
            line,
            format!(
                "{{\"k\":\"A\",\"h\":20,\"b\":\"{b}\",\"org\":\"tip\",\"t\":9,\"q\":[[\"gov_valid\",true],\
                 [\"sb_triggered\",20,true],[\"sb_triggered\",21,false],[\"sb_payments\",20,[[7,\"51ab\"]]],\
                 [\"sb_payments\",21,null]],\"x\":{{\"seq\":4}}}}\n"
            )
        );
        assert!(tap.take().is_none(), "taken once");
    }

    /// `cl_best`'s `null` is Core's null sig (`ChainLockSig::IsNull`), whichever way V gives it.
    #[test]
    fn a_null_best_chainlock_is_recorded_null() {
        assert_eq!(best(None), Ok(None));
        assert_eq!(best(Some(&ChainLockSig::null_arc())), Ok(None));
        let sig = Arc::new(ChainLockSig {
            height: 5,
            block_hash: BlockHash::from([1; 32]),
            sig: [2; 96],
        });
        assert_eq!(
            best(Some(&sig)),
            Ok(Some(BestChainLock {
                height: 5,
                hash: display(sig.block_hash).expect("a hash"),
                sig: [2; 96],
            }))
        );
    }
}

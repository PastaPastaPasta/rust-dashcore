//! [`RecordedHelper`]: the [`ChainHelper`] of replays of O8's recordings (CH3 §3.3; O8 §7.2): it
//! answers `ConnectBlock`'s chain-helper queries from O9a's [`RecordedDecisions`], so a replay sees
//! the ChainLock, InstantSend and governance state Core saw, and it never guesses.
//!
//! - **The attempt.** Its scope opens with `ConnectBlock` ([`ChainHelper::begin_attempt`], CH3a
//!   D125) and closes at [`ChainHelper::end_attempt`]. Its recording is the one `(b, org)` names,
//!   picked per [`AttemptChoice`].
//! - **The cursor** (CH3 D35). Each query is asked of the attempt's [`AttemptCursor`] under the
//!   caller's [`Policy`]. O8 records one `is_conflict` answer per loop condition
//!   (`evo/chainhelper.cpp:79-85`), so `is_conflict` is the only query of the IS loop that takes
//!   an entry; [`ChainHelper::remove_conflicting_is_lock`] takes none and emits the effect for the
//!   lock `is_conflict` returned.
//! - **Misses** (CH3a D139). A decision miss (no such attempt, an unrecorded or out-of-order query,
//!   or under strict an unasked tail at `end_attempt`) goes into the replay's [`MissSlot`]: the
//!   query that missed and every helper call after it fail with a [`Fatal`] carrying its text, and
//!   the driver checks the slot after each block, which also catches a tail found at
//!   `end_attempt`, where no call returns.
//! - G2's `interim_refusal` stays `None`: the recording holds Core's answers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use dash_consensus::Tx;
use dash_evo::{Effect, EffectSink};
use dash_primitives::{BlockHash, TxHash, TxOut};
use dash_statedigest::decisions::{
    Answer, Ask, AttemptCursor, AttemptKey, Policy, RecordedDecisions,
};
use dash_statedigest::{Hash256, Origin};

use super::evo::BlockAt;
use super::helper::{ChainHelper, IsLockRef};
use crate::chain::digest_tap::{display, origin};
use crate::chain::{ConnectCx, Fatal};

/// Which recorded attempt answers each replayed attempt (CH3a D140).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptChoice {
    /// A tip connect takes the attempt behind the block's last tip `C`, the one the golden merges
    /// ([`RecordedDecisions::final_connect`]): a replay of the final chain connects each block
    /// once, while Core may have tried it several times. Other origins, and tip attempts of a
    /// block the recording never connected at the tip (a rejected one), go in order, as below.
    FinalConnect,
    /// The replay's n-th attempt for `(b, org)` takes the recording's n-th
    /// ([`ReplaySession::begin`](dash_statedigest::decisions::ReplaySession::begin)).
    InOrder,
}

/// A replay's first decision miss, as text (CH3a D139): set by its [`RecordedHelper`], checked by
/// its driver. Clones share the slot.
#[derive(Clone, Debug, Default)]
pub struct MissSlot(Arc<Mutex<Option<String>>>);

impl MissSlot {
    pub fn new() -> MissSlot {
        MissSlot::default()
    }

    /// The first miss, if any.
    pub fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Keeps `text` unless a miss is there already, and returns the kept one.
    fn set(&self, text: String) -> String {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(text).clone()
    }
}

/// A [`ChainHelper`] over a node's recorded decisions.
pub struct RecordedHelper {
    decisions: Arc<RecordedDecisions>,
    policy: Policy,
    choice: AttemptChoice,
    /// How many attempts per `(b, org)` the replay has begun, for [`AttemptChoice::InOrder`].
    seen: BTreeMap<(Hash256, Origin), u32>,
    /// The open attempt's cursor.
    current: Option<AttemptCursor<'static>>,
    miss: MissSlot,
}

impl RecordedHelper {
    /// A helper answering from `decisions` under `policy`, which reports its misses to `miss`.
    pub fn new(
        decisions: Arc<RecordedDecisions>,
        policy: Policy,
        choice: AttemptChoice,
        miss: MissSlot,
    ) -> RecordedHelper {
        RecordedHelper {
            decisions,
            policy,
            choice,
            seen: BTreeMap::new(),
            current: None,
            miss,
        }
    }

    /// A miss: kept in the slot (the first one wins) and returned as the call's [`Fatal`].
    fn missed(&self, text: impl std::fmt::Display) -> Fatal {
        Fatal::new(self.miss.set(format!("decision miss: {text}")))
    }

    /// After a miss, every call fails with it: the replay has stopped.
    fn stopped(&self) -> Result<(), Fatal> {
        self.miss.get().map_or(Ok(()), |text| Err(Fatal::new(text)))
    }

    fn key(&mut self, b: Hash256, org: Origin) -> Result<AttemptKey, Fatal> {
        if self.choice == AttemptChoice::FinalConnect
            && org == Origin::Tip
            && self.decisions.has_tip_connect(&b)
        {
            return self.decisions.final_connect(&b).ok_or_else(|| {
                self.missed(format!("block {b}: its last tip C is in a run without decisions"))
            });
        }
        let n = self.seen.entry((b, org)).or_insert(0);
        let key = AttemptKey {
            b,
            org,
            n: *n,
        };
        *n += 1;
        Ok(key)
    }

    /// The recorded answer to `ask`, from the open attempt.
    fn ask(&mut self, ask: Ask) -> Result<Answer, Fatal> {
        self.stopped()?;
        let answer = match &mut self.current {
            Some(cursor) => cursor.ask(&ask).map_err(|m| m.to_string()),
            None => Err(format!("{ask:?} asked outside an attempt")),
        };
        answer.map_err(|text| self.missed(text))
    }

    fn ask_bool(&mut self, ask: Ask) -> Result<bool, Fatal> {
        match self.ask(ask)? {
            Answer::Bool(answer) => Ok(answer),
            other => Err(self.missed(format!("{other:?} where a bool was recorded"))),
        }
    }
}

fn height(h: u32) -> i64 {
    h.into()
}

impl ChainHelper for RecordedHelper {
    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        self.stopped()?;
        if self.current.is_some() {
            return Err(Fatal::new("RecordedHelper: an attempt began inside another"));
        }
        let key = self.key(display(at.hash)?, origin(cx.origin))?;
        let cursor = self.decisions.session(self.policy).begin_at(key);
        self.current = Some(cursor.map_err(|m| self.missed(m))?.into_owned());
        Ok(())
    }

    fn end_attempt(&mut self) {
        let Some(cursor) = self.current.take() else {
            return;
        };
        // Keyed leaves recorded queries a replay may skip (a pure query asked once); strict
        // requires every one.
        if self.miss.get().is_none()
            && let Err(m) = cursor.finish()
        {
            self.missed(m);
        }
    }

    fn cl_conflict(&mut self, _: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        let hash = display(hash)?;
        self.ask_bool(Ask::ClConflict {
            height: height(h),
            hash,
        })
    }

    fn ibd(&mut self, _: &ConnectCx<'_>) -> Result<bool, Fatal> {
        self.ask_bool(Ask::Ibd)
    }

    fn cl_has(&mut self, _: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        let hash = display(hash)?;
        self.ask_bool(Ask::ClHas {
            height: height(h),
            hash,
        })
    }

    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        let txid = display(tx.txid())?;
        match self.ask(Ask::IsConflict {
            txid,
        })? {
            Answer::IsConflict(pair) => Ok(pair.map(|p| IsLockRef {
                hash: dash_num::Hash256::from(p.islock.to_wire_bytes()),
                txid: TxHash::from(p.conflicting_txid.to_wire_bytes()),
            })),
            other => Err(self.missed(format!("{other:?} where an is_conflict was recorded"))),
        }
    }

    /// Consumes nothing (CH3 D35): the recording's next `is_conflict` answer already excludes
    /// the removed lock and its chained children.
    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        sink.emit(Effect::RemoveConflictingIsLock {
            lock_hash: lock.hash,
        });
    }

    fn sb_required(&mut self, _: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        self.ask_bool(Ask::SbRequired {
            height: height(at.height),
        })
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.ask_bool(Ask::GovValid)
    }

    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
        self.ask_bool(Ask::SbTriggered {
            height: height(h),
        })
    }

    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        match self.ask(Ask::SbPayments {
            height: height(h),
        })? {
            Answer::SbPayments(payments) => Ok(payments.map(|list| {
                list.into_iter()
                    .map(|p| TxOut {
                        value: p.amount,
                        script_pubkey: p.script.into(),
                    })
                    .collect()
            })),
            other => Err(self.missed(format!("{other:?} where sb_payments was recorded"))),
        }
    }
}

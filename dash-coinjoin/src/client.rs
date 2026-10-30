//! The mixing client's session transitions (Core's `CCoinJoinClientSession`,
//! `src/coinjoin/client.cpp`): status updates, the final transaction's checks before signing,
//! completion and timeouts. Coin selection, keys, signing and the queue strategy are the wallet's
//! (CJ1/WL6; key-wallet and the desktop wallet, U-06); each answer here says what the wallet
//! does next.

use std::collections::BTreeSet;

use dash_p2p_core::{DsComplete, DsStatusUpdate};
use dash_primitives::{OutPoint, Transaction, TxIn, TxOut};

use crate::denom::{self, MixShape};
use crate::inouts::{CoinView, InOutsKind, is_bip69_sorted, is_valid_in_outs};
use crate::pool::{PoolMessage, PoolState, PoolStatusUpdate, state_timeout};

/// Extra seconds a client gives the masternode before it times a session out.
pub const CLIENT_LAG_TIME: i64 = 10;
/// Seconds a client stays in [`PoolState::Error`] before it resets.
pub const ERROR_RESET_TIME: i64 = 10;

/// An entry this client sent (Core's client-side `CCoinJoinEntry`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientEntry {
    /// The inputs (`CTxDSIn`s as plain inputs: outpoint, empty script, sequence).
    pub inputs: Vec<TxIn>,
    /// The outputs.
    pub outputs: Vec<TxOut>,
}

impl ClientEntry {
    /// `GetMixShape()`.
    pub fn mix_shape(&self) -> MixShape {
        MixShape::of_entry(self.inputs.len(), self.outputs.len())
    }
}

/// What the wallet does after [`ClientSession::process_status_update`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusOutcome {
    /// Nothing: the session is idle or failed, or the update is out of range.
    Ignored,
    /// Rejected: the session is in [`PoolState::Error`]; unlock the coins and return the keys.
    Rejected(PoolMessage),
    /// Accepted (the session id may have been set).
    Accepted(PoolMessage),
}

/// Why the client refuses to sign a final transaction. Each resets the session; the wallet
/// unlocks its coins and returns its keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalTxRefusal {
    /// Not in BIP69 order.
    NotBip69,
    /// [`is_valid_in_outs`] failed.
    InvalidInOuts(PoolMessage),
    /// One of our outputs is missing.
    MissingOutput,
    /// One of our inputs is missing.
    MissingInput,
    /// Post-V24: nobody else has coins at the session denomination on a side we occupy.
    NoCover,
}

/// What the wallet does with a `dsf`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalTxOutcome {
    /// Not this session's: ignore.
    WrongSession,
    /// Refused; the session was reset.
    Refused(FinalTxRefusal),
    /// Sign these inputs of the (unchanged) final transaction with `SIGHASH_ALL |
    /// SIGHASH_ANYONECANPAY`, send the signed ones in `dss` and call
    /// [`ClientSession::signed`]; on a signing failure or nothing signed, call
    /// [`ClientSession::reset`].
    Sign {
        /// Indices of our inputs in the final transaction.
        inputs: Vec<usize>,
    },
}

/// What the wallet does after a `dsc`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompleteOutcome {
    /// Out of range or not this session's: ignore.
    Ignored,
    /// The session is over and reset. On [`PoolMessage::MsgSuccess`] keep the keys and hold the
    /// locked coins among `mixed_inputs` until a transaction spending them is seen (pending
    /// observation); unlock the rest. Otherwise return the keys and unlock everything.
    Completed {
        /// The outcome.
        message: PoolMessage,
        /// Our inputs in the final transaction.
        mixed_inputs: BTreeSet<OutPoint>,
    },
}

/// What the wallet does after [`ClientSession::check_timeout`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeoutOutcome {
    /// Nothing.
    None,
    /// Reset after [`ERROR_RESET_TIME`] in [`PoolState::Error`].
    Reset,
    /// Timed out: now in [`PoolState::Error`]; unlock the coins and return the keys.
    TimedOut,
}

/// A client's mixing session, the part Core keeps in `CCoinJoinBaseSession`.
#[derive(Clone, Debug, Default)]
pub struct ClientSession {
    /// `nState`.
    pub state: PoolState,
    /// `nSessionID`, 0 until the masternode assigns one.
    pub session_id: i32,
    /// `nSessionDenom`.
    pub session_denom: i32,
    /// `vecEntries`: what this client sent.
    pub entries: Vec<ClientEntry>,
    /// `finalMutableTransaction`, once received and sorted.
    pub final_tx: Option<Transaction>,
    /// `nTimeLastSuccessfulStep`.
    pub time_last_successful_step: i64,
    /// `strLastMessage`.
    pub last_message: Option<PoolMessage>,
}

impl ClientSession {
    // Core: coinjoin/coinjoin.cpp:157 CCoinJoinBaseSession::SetNull
    /// `SetNull()`: idle, no session, at `now`. The wallet clears its own session data (the
    /// masternode, the pending `dsa`, the rebalance inputs).
    pub fn reset(&mut self, now: i64) {
        self.state = PoolState::Idle;
        self.session_id = 0;
        self.session_denom = 0;
        self.entries.clear();
        self.final_tx = None;
        self.time_last_successful_step = now;
    }

    // Core: coinjoin/client.cpp:1453 JoinExistingQueue, :1543 and :1626 StartNewQueue
    /// A queue at `session_denom` was joined, or a new one asked for: [`PoolState::Queue`] at
    /// `now`. The wallet keeps the masternode, the pending `dsa` and the rebalance side.
    ///
    /// `session_denom` must be a standard denomination, as Core's callers pass (`client.cpp:1444`,
    /// `:1535`, `:1617`): no session has denomination 0 (O2a-CJ1 D6).
    pub fn enter_queue(&mut self, session_denom: i32, now: i64) {
        debug_assert!(denom::is_valid_denomination(session_denom), "{session_denom}");
        self.session_denom = session_denom;
        self.state = PoolState::Queue;
        self.time_last_successful_step = now;
    }

    // Core: coinjoin/client.cpp:1672 CCoinJoinClientSession::ProcessPendingDsaRequest
    /// The pending `dsa` was sent to the masternode at `now`.
    pub fn dsa_sent(&mut self, now: i64) {
        self.time_last_successful_step = now;
    }

    // Core: coinjoin/client.cpp:378-408 CCoinJoinClientSession::SendDenominate
    /// `entry` was sent (`dsi`): [`PoolState::AcceptingEntries`], the entry kept, at `now`.
    /// `entry` is what went on the wire: Core drops a rebalance pairing's null inputs and empty
    /// outputs first.
    pub fn entry_sent(&mut self, entry: ClientEntry, now: i64) {
        self.state = PoolState::AcceptingEntries;
        self.last_message = None;
        self.entries.push(entry);
        self.time_last_successful_step = now;
    }

    // Core: coinjoin/client.cpp:414 CCoinJoinClientSession::ProcessPoolStateUpdate
    /// A `dssu` from the mixing masternode.
    pub fn process_status_update(&mut self, update: &DsStatusUpdate, now: i64) -> StatusOutcome {
        if matches!(self.state, PoolState::Idle | PoolState::Error) {
            return StatusOutcome::Ignored;
        }
        let Some(state) = PoolState::from_i32(update.state) else {
            return StatusOutcome::Ignored;
        };
        let Some(message) = PoolMessage::from_i32(update.message_id) else {
            return StatusOutcome::Ignored;
        };
        if update.status_update == PoolStatusUpdate::Rejected as i32 {
            self.state = PoolState::Error;
            self.time_last_successful_step = now;
            self.last_message = Some(message);
            StatusOutcome::Rejected(message)
        } else if update.status_update == PoolStatusUpdate::Accepted as i32 {
            // A session id is set only while queued.
            if self.state == state
                && state == PoolState::Queue
                && self.session_id == 0
                && update.session_id != 0
            {
                self.session_id = update.session_id;
                self.time_last_successful_step = now;
            }
            StatusOutcome::Accepted(message)
        } else {
            StatusOutcome::Ignored
        }
    }

    // Core: coinjoin/client.cpp:464 CCoinJoinClientSession::SignFinalTransaction
    /// A `dsf` from the mixing masternode: check the final transaction before signing it.
    /// `rebalance_possible` is `IsPromotionDemotionActive(fNextBlock=true)`.
    pub fn process_final_tx<V: CoinView + ?Sized>(
        &mut self,
        view: &V,
        session_id: i32,
        tx: Transaction,
        rebalance_possible: bool,
        now: i64,
    ) -> FinalTxOutcome {
        if session_id != self.session_id {
            return FinalTxOutcome::WrongSession;
        }
        match self.check_final_tx(view, &tx, rebalance_possible) {
            Ok(inputs) => {
                self.final_tx = Some(tx);
                FinalTxOutcome::Sign {
                    inputs,
                }
            }
            Err(refusal) => {
                self.reset(now);
                FinalTxOutcome::Refused(refusal)
            }
        }
    }

    fn check_final_tx<V: CoinView + ?Sized>(
        &self,
        view: &V,
        tx: &Transaction,
        rebalance_possible: bool,
    ) -> Result<Vec<usize>, FinalTxRefusal> {
        // Core sorts a copy and compares hashes; a stable sort changes nothing exactly when the
        // transaction is already in order. Only a repeated prevout compares equal with different
        // bytes, and IsValidInOuts refuses that as a duplicate script either way (D10).
        if !is_bip69_sorted(tx) {
            return Err(FinalTxRefusal::NotBip69);
        }
        let counts = is_valid_in_outs(
            view,
            &tx.inputs,
            &tx.outputs,
            self.session_denom,
            rebalance_possible,
            InOutsKind::FinalTx,
        )
        .map_err(|r| FinalTxRefusal::InvalidInOuts(r.message))?;

        // Our inputs and outputs are all there.
        let mut mine = Vec::new();
        for entry in &self.entries {
            if !entry.outputs.iter().all(|o| tx.outputs.contains(o)) {
                return Err(FinalTxRefusal::MissingOutput);
            }
            for input in &entry.inputs {
                let index = tx
                    .inputs
                    .iter()
                    .position(|i| i == input)
                    .ok_or(FinalTxRefusal::MissingInput)?;
                mine.push(index);
            }
        }

        // Post-V24: somebody else must hold session-denomination coins on each side we do, or a
        // masternode could pad the transaction with larger coins and expose our rebalance.
        if rebalance_possible {
            let session_amount = denom::denomination_to_amount(self.session_denom);
            let own_inputs: usize = self
                .entries
                .iter()
                .filter(|e| e.mix_shape() != MixShape::Demotion)
                .map(|e| e.inputs.len())
                .sum();
            let own_outputs = self
                .entries
                .iter()
                .flat_map(|e| &e.outputs)
                .filter(|o| o.value == session_amount)
                .count();
            let inputs_covered = own_inputs == 0 || counts.inputs > own_inputs;
            let outputs_covered = own_outputs == 0 || counts.outputs > own_outputs;
            if !inputs_covered || !outputs_covered {
                return Err(FinalTxRefusal::NoCover);
            }
        }

        // Core collects its inputs by outpoint (a map), so each is signed once, in the final
        // transaction's order.
        mine.sort_unstable();
        mine.dedup();
        Ok(mine)
    }

    /// The signed inputs were sent: [`PoolState::Signing`] at `now`.
    pub fn signed(&mut self, now: i64) {
        self.state = PoolState::Signing;
        self.time_last_successful_step = now;
    }

    // Core: coinjoin/client.cpp:127 DSCOMPLETE
    /// A `dsc` from the mixing masternode (Core's handler, then `CompletedTransaction`).
    pub fn process_complete(&mut self, msg: &DsComplete, now: i64) -> CompleteOutcome {
        let Some(message) = PoolMessage::from_i32(msg.message) else {
            return CompleteOutcome::Ignored;
        };
        if msg.session_id != self.session_id {
            return CompleteOutcome::Ignored;
        }
        // Core: coinjoin/client.cpp:634 CCoinJoinClientSession::CompletedTransaction
        let mixed_inputs = if message == PoolMessage::MsgSuccess {
            self.entries.iter().flat_map(|e| e.inputs.iter().map(|i| i.prevout)).collect()
        } else {
            BTreeSet::new()
        };
        self.reset(now);
        self.last_message = Some(message);
        CompleteOutcome::Completed {
            message,
            mixed_inputs,
        }
    }

    // Core: coinjoin/client.cpp:304 CCoinJoinClientSession::CheckTimeout
    /// The periodic timeout check at `now`.
    pub fn check_timeout(&mut self, now: i64) -> TimeoutOutcome {
        match self.state {
            PoolState::Idle => TimeoutOutcome::None,
            PoolState::Error => {
                if now.saturating_sub(self.time_last_successful_step) >= ERROR_RESET_TIME {
                    self.reset(now);
                    TimeoutOutcome::Reset
                } else {
                    TimeoutOutcome::None
                }
            }
            state => {
                let timeout = state_timeout(state) + CLIENT_LAG_TIME;
                if now.saturating_sub(self.time_last_successful_step) < timeout {
                    return TimeoutOutcome::None;
                }
                self.state = PoolState::Error;
                self.time_last_successful_step = now;
                self.last_message = Some(PoolMessage::ErrSession);
                TimeoutOutcome::TimedOut
            }
        }
    }
}

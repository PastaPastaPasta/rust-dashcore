//! The session vocabulary both sides share: Core's pool messages, states and status updates, the
//! timeouts, and the `dsa` rebalance flags (`src/coinjoin/coinjoin.h`).

use dash_p2p_core::DsAccept;

use crate::denom::MixShape;

/// `COINJOIN_AUTO_TIMEOUT_MIN`, seconds.
pub const AUTO_TIMEOUT_MIN: i64 = 5;
/// `COINJOIN_AUTO_TIMEOUT_MAX`, seconds.
pub const AUTO_TIMEOUT_MAX: i64 = 15;
/// `COINJOIN_QUEUE_TIMEOUT`, seconds: a queue's time bound and the session timeout outside
/// signing.
pub const QUEUE_TIMEOUT: i64 = 30;
/// `COINJOIN_SIGNING_TIMEOUT`, seconds.
pub const SIGNING_TIMEOUT: i64 = 15;

/// The session timeout in `state`: [`SIGNING_TIMEOUT`] while signing, [`QUEUE_TIMEOUT`]
/// otherwise (both sides' `CheckTimeout` and the server's `HasTimedOut`).
pub const fn state_timeout(state: PoolState) -> i64 {
    match state {
        PoolState::Signing => SIGNING_TIMEOUT,
        _ => QUEUE_TIMEOUT,
    }
}

/// `COINJOIN_PENDING_OBSERVATION_TIMEOUT`, seconds: how long a mixed input stays locked waiting
/// for its final transaction.
pub const PENDING_OBSERVATION_TIMEOUT: i64 = 60 * 60;
/// `COINJOIN_ENTRY_MAX_SIZE`: the most inputs or outputs of a pre-V24 entry.
pub const ENTRY_MAX_SIZE: usize = dash_p2p_core::COINJOIN_ENTRY_MAX_SIZE;

// Core: coinjoin/coinjoin.h:65 PoolMessage
/// Core's `PoolMessage`, a server's answer (`dssu`, `dsc`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(i32)]
pub enum PoolMessage {
    ErrAlreadyHave = 0,
    ErrDenom,
    ErrEntriesFull,
    ErrExistingTx,
    ErrFees,
    ErrInvalidCollateral,
    ErrInvalidInput,
    ErrInvalidScript,
    ErrInvalidTx,
    ErrMaximum,
    ErrMnList,
    ErrMode,
    /// Not used.
    ErrNonStandardPubkey,
    /// Not used.
    ErrNotAMn,
    ErrQueueFull,
    ErrRecent,
    ErrSession,
    ErrMissingTx,
    ErrVersion,
    MsgNoErr,
    MsgSuccess,
    MsgEntriesAdded,
    ErrSizeMismatch,
}

impl PoolMessage {
    const ALL: [PoolMessage; 23] = [
        Self::ErrAlreadyHave,
        Self::ErrDenom,
        Self::ErrEntriesFull,
        Self::ErrExistingTx,
        Self::ErrFees,
        Self::ErrInvalidCollateral,
        Self::ErrInvalidInput,
        Self::ErrInvalidScript,
        Self::ErrInvalidTx,
        Self::ErrMaximum,
        Self::ErrMnList,
        Self::ErrMode,
        Self::ErrNonStandardPubkey,
        Self::ErrNotAMn,
        Self::ErrQueueFull,
        Self::ErrRecent,
        Self::ErrSession,
        Self::ErrMissingTx,
        Self::ErrVersion,
        Self::MsgNoErr,
        Self::MsgSuccess,
        Self::MsgEntriesAdded,
        Self::ErrSizeMismatch,
    ];

    /// The message of a wire value in `MSG_POOL_MIN..=MSG_POOL_MAX`, the range Core's client
    /// accepts.
    pub fn from_i32(value: i32) -> Option<Self> {
        usize::try_from(value).ok().and_then(|i| Self::ALL.get(i).copied())
    }

    // Core: coinjoin/coinjoin.cpp:505 CoinJoin::GetMessageByID
    /// `GetMessageByID(..).original`.
    pub fn text(self) -> &'static str {
        match self {
            Self::ErrAlreadyHave => "Already have that input.",
            Self::ErrDenom => "No matching denominations found for mixing.",
            Self::ErrEntriesFull => "Entries are full.",
            Self::ErrExistingTx => "Not compatible with existing transactions.",
            Self::ErrFees => "Transaction fees are too high.",
            Self::ErrInvalidCollateral => "Collateral not valid.",
            Self::ErrInvalidInput => "Input is not valid.",
            Self::ErrInvalidScript => "Invalid script detected.",
            Self::ErrInvalidTx => "Transaction not valid.",
            Self::ErrMaximum => "Entry exceeds maximum size.",
            Self::ErrMnList => "Not in the Masternode list.",
            Self::ErrMode => "Incompatible mode.",
            Self::ErrQueueFull => "Masternode queue is full.",
            Self::ErrRecent => "Last queue was created too recently.",
            Self::ErrSession => "Session not complete!",
            Self::ErrMissingTx => "Missing input transaction information.",
            Self::ErrVersion => "Incompatible version.",
            Self::MsgNoErr => "No errors detected.",
            Self::MsgSuccess => "Transaction created successfully.",
            Self::MsgEntriesAdded => "Your entries added successfully.",
            Self::ErrSizeMismatch => "Inputs vs outputs size mismatch.",
            Self::ErrNonStandardPubkey | Self::ErrNotAMn => "Unknown response.",
        }
    }
}

// Core: coinjoin/coinjoin.h:95 PoolState
/// Core's `PoolState`, a session's state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(i32)]
pub enum PoolState {
    #[default]
    Idle = 0,
    Queue,
    AcceptingEntries,
    Signing,
    Error,
}

impl PoolState {
    /// The state of a wire value in `POOL_STATE_MIN..=POOL_STATE_MAX`.
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Idle),
            1 => Some(Self::Queue),
            2 => Some(Self::AcceptingEntries),
            3 => Some(Self::Signing),
            4 => Some(Self::Error),
            _ => None,
        }
    }

    // Core: coinjoin/coinjoin.cpp:237 CCoinJoinBaseSession::GetStateString
    /// `GetStateString()`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Queue => "QUEUE",
            Self::AcceptingEntries => "ACCEPTING_ENTRIES",
            Self::Signing => "SIGNING",
            Self::Error => "ERROR",
        }
    }
}

// Core: coinjoin/coinjoin.h:107 PoolStatusUpdate
/// Core's `PoolStatusUpdate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(i32)]
pub enum PoolStatusUpdate {
    Rejected = 0,
    Accepted = 1,
}

/// `CCoinJoinAccept::FLAG_PROMOTION`.
pub const FLAG_PROMOTION: u8 = 1 << 0;
/// `CCoinJoinAccept::FLAG_DEMOTION`.
pub const FLAG_DEMOTION: u8 = 1 << 1;

/// Core's `CCoinJoinAccept` flag predicates over BS9b's [`DsAccept`]. A request read without
/// flags (from a peer below `COINJOIN_REBALANCE`) has flags 0.
pub trait AcceptFlags {
    /// The flags byte, 0 when absent.
    fn flag_bits(&self) -> u8;

    /// `IsPromotion()`.
    fn is_promotion(&self) -> bool {
        self.flag_bits() & FLAG_PROMOTION != 0
    }

    /// `IsDemotion()`.
    fn is_demotion(&self) -> bool {
        self.flag_bits() & FLAG_DEMOTION != 0
    }

    /// `IsRebalance()`.
    fn is_rebalance(&self) -> bool {
        self.is_promotion() || self.is_demotion()
    }

    /// `HasValidFlags()`: not both directions.
    fn has_valid_flags(&self) -> bool {
        !(self.is_promotion() && self.is_demotion())
    }

    // Core: coinjoin/server.cpp:952 DeclaredShape
    /// The side(s) the participant declares: promotion, demotion, or standard (both).
    fn declared_shape(&self) -> MixShape {
        if self.is_promotion() {
            MixShape::Promotion
        } else if self.is_demotion() {
            MixShape::Demotion
        } else {
            MixShape::Standard
        }
    }
}

impl AcceptFlags for DsAccept {
    fn flag_bits(&self) -> u8 {
        self.flags.unwrap_or(0)
    }
}

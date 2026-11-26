//! Header batches a peer is scored for (PLAN-r1 §2.2.3, rule 1): Core's checks before
//! `ProcessNewBlockHeaders` and Core's weights. Until SB16 S4 adds scores, a weight is logged
//! and the peer kept.

use std::collections::HashMap;

use dash_chain::{HeaderStop, PeerId};
use dash_consensus::BlockValidationResult;
use dash_primitives::BlockHash;

/// `MAX_BLOCKS_TO_ANNOUNCE` (`net_processing.cpp:174`): an unconnecting batch of at most this
/// many headers may be a block announcement.
pub(super) const MAX_BLOCKS_TO_ANNOUNCE: usize = 8;

/// `MAX_UNCONNECTING_HEADERS` (`net_processing.cpp:176`).
const MAX_UNCONNECTING_HEADERS: u32 = 10;

/// Why a batch was not handed to `accept` (`net_processing.cpp:3341-3360`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// At most `MAX_BLOCKS_TO_ANNOUNCE` headers whose first parent is unknown
    /// (`HandleFewUnconnectingHeaders`, `:3143-3170`): the peer is to be sent `getheaders` from
    /// `locator`, the best header's, unless one is still in flight to it (the headers manager's
    /// `MaybeSendGetHeaders`).
    Unconnecting {
        locator: Vec<BlockHash>,
    },
    /// More headers than that whose first parent is unknown.
    UnconnectingBatch,
    /// Headers that connect but do not chain to each other (`CheckHeadersAreContinuous`).
    NonContinuous,
}

/// Core's `MaybePunishNodeForBlock` (`net_processing.cpp:1974-2018`) for a rejected header from
/// an outbound peer (dash-spv makes no other), not via a compact block.
pub(super) fn reject_weight(result: BlockValidationResult) -> u32 {
    use BlockValidationResult as R;
    match result {
        R::Consensus | R::Mutated | R::CachedInvalid => 100,
        R::InvalidHeader | R::Checkpoint | R::InvalidPrev => 100,
        R::MissingPrev | R::ChainLock => 10,
        R::Unset | R::RecentConsensusChange | R::TimeFuture => 0,
    }
}

/// Core's `Misbehaving` for one batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderMisbehaviour {
    /// The result of the header `accept` rejected, if it rejected one.
    pub result: Option<BlockValidationResult>,
    /// Core's message, or the rejected header's reason.
    pub reason: String,
    pub weight: u32,
}

impl HeaderMisbehaviour {
    fn new(reason: impl Into<String>, weight: u32) -> Option<Self> {
        Some(Self {
            result: None,
            reason: reason.into(),
            weight,
        })
    }
}

/// Each peer's count of unconnecting batches in a row (Core's `nUnconnectingHeaders`).
#[derive(Default)]
pub(super) struct Unconnecting(HashMap<PeerId, u32>);

impl Unconnecting {
    /// `peer`'s misbehaviour for a non-empty batch that was `refused`, or stopped at `stop`.
    /// The count resets only after a batch is accepted (`UpdatePeerStateForReceivedHeaders`,
    /// `:3272-3280`, run at `:3385`), not after one that connects but is rejected (`:3367-3371`).
    pub(super) fn score(
        &mut self,
        peer: PeerId,
        refused: Option<&Refused>,
        stop: Option<&HeaderStop>,
    ) -> Option<HeaderMisbehaviour> {
        match (refused, stop) {
            (
                Some(Refused::Unconnecting {
                    ..
                }),
                _,
            ) => {
                let n = self.0.entry(peer).or_default();
                *n += 1;
                if n.is_multiple_of(MAX_UNCONNECTING_HEADERS) {
                    return HeaderMisbehaviour::new(format!("{n} non-connecting headers"), 20);
                }
                None
            }
            (Some(Refused::UnconnectingBatch), _) => {
                HeaderMisbehaviour::new("invalid header received", 10)
            }
            (Some(Refused::NonContinuous), _) => {
                HeaderMisbehaviour::new("non-continuous headers sequence", 20)
            }
            (
                None,
                Some(HeaderStop::Invalid {
                    reject,
                    ..
                }),
            ) => Some(HeaderMisbehaviour {
                result: Some(reject.result()),
                reason: reject.reject_reason().into(),
                weight: reject_weight(reject.result()),
            }),
            (None, _) => {
                self.0.remove(&peer);
                None
            }
        }
    }
}

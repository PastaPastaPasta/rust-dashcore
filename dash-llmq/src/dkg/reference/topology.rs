//! Core's relay topology test adapter over the production member-list topology.

use std::collections::BTreeSet;

use dash_evo::types::ProTxHash;

use crate::dkg::session::RelayTopology;
use crate::dkg::wire::{LlmqType, QuorumHash};
pub use crate::topology::quorum_relay_members;

/// A [`RelayTopology`] over one quorum's member list, outbound only as `Init` asks, that counts
/// its calls.
#[derive(Clone, Debug, Default)]
pub struct CoreRelayTopology {
    /// The members, in Core's order.
    pub members: Vec<ProTxHash>,
    /// The calls made.
    pub calls: usize,
}

impl RelayTopology for CoreRelayTopology {
    fn relay_members(
        &mut self,
        _t: LlmqType,
        _base: &QuorumHash,
        me: &ProTxHash,
    ) -> BTreeSet<ProTxHash> {
        self.calls += 1;
        quorum_relay_members(&self.members, me, true)
    }
}

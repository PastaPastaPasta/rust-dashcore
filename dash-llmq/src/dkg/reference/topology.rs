//! Core's relay topology for tests: `GetQuorumRelayMembers` (`l/utils.cpp:778-831`) over a
//! fixed member list (DEC-616 #8). The node's is LQ5's, over CH7b's facade.

use std::collections::BTreeSet;

use dash_evo::types::ProTxHash;

use crate::dkg::session::RelayTopology;
use crate::dkg::wire::{LlmqType, QuorumHash};

/// `GetQuorumRelayMembers(params, members, forMember, onlyOutbound)`: the ring neighbours at
/// `(i + 2^k) mod n` of `for_member`, and with `only_outbound` off also the members that relay
/// to it.
pub fn quorum_relay_members(
    members: &[ProTxHash],
    for_member: &ProTxHash,
    only_outbound: bool,
) -> BTreeSet<ProTxHash> {
    let n = members.len();
    let calc_outbound = |i: usize, pro_tx: &ProTxHash| {
        let mut r = BTreeSet::new();
        if n == 1 {
            return r;
        }
        let mut gap = 1usize;
        let mut gap_max = n - 1;
        let mut k = 0;
        loop {
            gap_max >>= 1;
            if gap_max == 0 && k > 1 {
                break;
            }
            let idx = (i + gap) % n;
            gap <<= 1;
            k += 1;
            if members[idx] != *pro_tx {
                r.insert(members[idx]);
            }
        }
        r
    };
    let mut result = BTreeSet::new();
    for (i, pro_tx) in members.iter().enumerate() {
        if pro_tx == for_member {
            result.extend(calc_outbound(i, pro_tx));
        } else if !only_outbound && calc_outbound(i, pro_tx).contains(for_member) {
            result.insert(*pro_tx);
        }
    }
    result
}

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

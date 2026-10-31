//! P2P-12: GETDATA-only objects must answer a `getdata` we sent (P4a D7).
//!
//! CLSIG, QFCOMMITMENT and the four DKG messages travel only `inv` → `getdata` → object, so one
//! we never asked the peer for was pushed at us. Their handlers (LK1a, LQ2, DK1) call
//! [`consume_getdata_response`]; P5's `getdata` sender calls [`RecentObjectRequests::record`].
//! An unrequested object costs the peer [`UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE`] and is dropped
//! before any work on its behalf.

use std::collections::HashMap;

use dash_p2p_core::{InvType, Inventory};
use dash_types::Numeric;

/// Core's `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE`.
// Core: msg_result.h:21
pub const UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE: i32 = 10;

/// Core's `RECENT_OBJECT_REQUEST_TTL_INTERVALS`: an answer this many request intervals after
/// the `getdata` is late, not unsolicited.
// Core: net_processing.cpp:96-104
pub const RECENT_OBJECT_REQUEST_TTL_INTERVALS: i64 = 2;

/// Core's `MAX_RECENT_OBJECT_REQUESTS`: the record's `unordered_lru_cache` size.
// Core: net_processing.cpp:105-111
pub const MAX_RECENT_OBJECT_REQUESTS: usize = 256;

/// Core's `GETDATA_TX_INTERVAL`, in µs.
// Core: net_processing.cpp:113
const GETDATA_TX_INTERVAL_US: i64 = 60_000_000;

/// Core's `IsGetDataOnlyObject`.
// Core: net_processing.cpp:1611-1631
pub fn is_getdata_only(inv_type: InvType) -> bool {
    matches!(
        inv_type,
        InvType::ClSig
            | InvType::QuorumFinalCommitment
            | InvType::QuorumContrib
            | InvType::QuorumComplaint
            | InvType::QuorumJustification
            | InvType::QuorumPrematureCommitment
    )
}

/// Core's `GetObjectInterval`, in µs: how long a request waits before another peer is asked.
// Core: net_processing.cpp:1593-1609
pub fn object_interval_us(inv_type: InvType) -> i64 {
    match inv_type {
        InvType::QuorumRecoveredSig => 15_000_000,
        InvType::ClSig => 5_000_000,
        InvType::IsdLock => 10_000_000,
        _ => GETDATA_TX_INTERVAL_US,
    }
}

/// Core's `GetDataResponse`.
// Core: net_processing.h GetDataResponse
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GetDataResponse {
    /// The tracker had the request in flight.
    Requested,
    /// We asked this peer for it, within the grace.
    Late,
    /// Never asked, asked for another type, or answered too late.
    Unrequested,
}

/// Core's `RequestedObject`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RequestedObject {
    inv_type: InvType,
    time_us: i64,
}

/// Core's `CNodeState::m_recent_object_requests`: an `unordered_lru_cache<uint256,
/// RequestedObject, …, 256>` of the GETDATA-only objects we asked this peer for. As Core's cache
/// it grows to twice its size, then keeps the 256 most recently used.
// Core: unordered_lru_cache.h
#[derive(Clone, Debug, Default)]
pub struct RecentObjectRequests {
    map: HashMap<[u8; 32], (RequestedObject, i64)>,
    access: i64,
}

impl RecentObjectRequests {
    /// What `SendMessages` section 13 records after queueing a `getdata` entry: only
    /// GETDATA-only types are kept.
    // Core: net_processing.cpp:6876-6881
    pub fn record(&mut self, inv: &Inventory, now_us: i64) {
        if !is_getdata_only(inv.inv_type) {
            return;
        }
        let value = RequestedObject {
            inv_type: inv.inv_type,
            time_us: now_us,
        };
        self.map.insert(inv.hash.to_lendian(), (value, self.access));
        self.access += 1;
        self.truncate_if_needed();
    }

    // Core: unordered_lru_cache.h truncate_if_needed (threshold 2 × size)
    fn truncate_if_needed(&mut self) {
        if self.map.len() <= 2 * MAX_RECENT_OBJECT_REQUESTS {
            return;
        }
        let mut access: Vec<i64> = self.map.values().map(|(_, a)| *a).collect();
        access.sort_unstable_by(|a, b| b.cmp(a));
        let oldest_kept = access[MAX_RECENT_OBJECT_REQUESTS - 1];
        self.map.retain(|_, (_, a)| *a >= oldest_kept);
    }
}

/// Core's `PeerConsumeGetDataResponse` for `inv` from this peer. `requested` is the tracker's
/// `ReceivedRequestedResponse(peer, inv)` (P5's; the M2 node requests nothing, so `false`).
/// The record entry is spent whatever the outcome, and the type and the window come from the
/// request, never from the answer.
// Core: net_processing.cpp:6925-6957
pub fn consume_getdata_response(
    recent: &mut RecentObjectRequests,
    requested: bool,
    inv: &Inventory,
    now_us: i64,
) -> GetDataResponse {
    let key = inv.hash.to_lendian();
    let asked = recent.map.remove(&key);
    if requested {
        return GetDataResponse::Requested;
    }
    match asked {
        Some((r, _))
            if r.inv_type == inv.inv_type
                && now_us - r.time_us
                    <= object_interval_us(r.inv_type) * RECENT_OBJECT_REQUEST_TTL_INTERVALS =>
        {
            GetDataResponse::Late
        }
        _ => GetDataResponse::Unrequested,
    }
}

#[cfg(test)]
mod tests {
    use dash_num::Hash256;

    use super::*;

    fn inv(inv_type: InvType, n: u8) -> Inventory {
        Inventory {
            inv_type,
            hash: Hash256::from_lendian([n; 32]),
        }
    }

    #[test]
    fn getdata_only_types_are_cores() {
        let only: Vec<u32> =
            (0..64u32).filter(|t| is_getdata_only(InvType::from_base(*t))).collect();
        assert_eq!(only, [21, 23, 24, 25, 26, 29]);
    }

    #[test]
    fn grace_is_two_intervals_of_the_requested_type_and_spent_once() {
        let mut r = RecentObjectRequests::default();
        let clsig = inv(InvType::ClSig, 1);
        r.record(&clsig, 0);
        // CLSIG's interval is 5 s: the grace ends at 10 s.
        assert_eq!(
            consume_getdata_response(&mut r, false, &clsig, 10_000_000),
            GetDataResponse::Late
        );
        // Spent: a replay is unsolicited.
        assert_eq!(
            consume_getdata_response(&mut r, false, &clsig, 10_000_000),
            GetDataResponse::Unrequested
        );
        r.record(&clsig, 0);
        assert_eq!(
            consume_getdata_response(&mut r, false, &clsig, 10_000_001),
            GetDataResponse::Unrequested
        );
        // A commitment's interval is 60 s.
        let qc = inv(InvType::QuorumFinalCommitment, 2);
        r.record(&qc, 0);
        assert_eq!(
            consume_getdata_response(&mut r, false, &qc, 120_000_000),
            GetDataResponse::Late
        );
    }

    #[test]
    fn the_answer_cannot_name_another_type() {
        let mut r = RecentObjectRequests::default();
        r.record(&inv(InvType::QuorumContrib, 3), 0);
        let as_clsig = inv(InvType::ClSig, 3);
        assert_eq!(
            consume_getdata_response(&mut r, false, &as_clsig, 1),
            GetDataResponse::Unrequested
        );
        // The mismatched answer still spent the entry.
        assert!(r.map.is_empty());
    }

    #[test]
    fn tracker_answer_wins_and_spends_the_grace() {
        let mut r = RecentObjectRequests::default();
        let qc = inv(InvType::QuorumFinalCommitment, 4);
        r.record(&qc, 0);
        assert_eq!(consume_getdata_response(&mut r, true, &qc, 1), GetDataResponse::Requested);
        assert_eq!(consume_getdata_response(&mut r, false, &qc, 1), GetDataResponse::Unrequested);
        assert_eq!(
            consume_getdata_response(&mut r, true, &inv(InvType::ClSig, 9), 1),
            GetDataResponse::Requested
        );
    }

    #[test]
    fn only_getdata_only_types_are_recorded() {
        let mut r = RecentObjectRequests::default();
        r.record(&inv(InvType::Tx, 5), 0);
        r.record(&inv(InvType::IsdLock, 6), 0);
        r.record(&inv(InvType::QuorumRecoveredSig, 7), 0);
        assert!(r.map.is_empty());
    }

    #[test]
    fn the_cache_truncates_to_the_most_recent_256_past_512() {
        let mut r = RecentObjectRequests::default();
        let key = |n: u32| Inventory {
            inv_type: InvType::QuorumContrib,
            hash: Hash256::from_lendian({
                let mut h = [0; 32];
                h[..4].copy_from_slice(&n.to_le_bytes());
                h
            }),
        };
        for n in 0..512 {
            r.record(&key(n), 0);
        }
        assert_eq!(r.map.len(), 512);
        r.record(&key(512), 0);
        assert_eq!(r.map.len(), MAX_RECENT_OBJECT_REQUESTS);
        // The newest 256 survive.
        assert_eq!(consume_getdata_response(&mut r, false, &key(512), 0), GetDataResponse::Late);
        assert_eq!(consume_getdata_response(&mut r, false, &key(257), 0), GetDataResponse::Late);
        assert_eq!(
            consume_getdata_response(&mut r, false, &key(256), 0),
            GetDataResponse::Unrequested
        );
        // Re-recording refreshes an entry's access, as `insert` on an existing key does.
        let mut r = RecentObjectRequests::default();
        for n in 0..512 {
            r.record(&key(n), 0);
        }
        r.record(&key(0), 0);
        r.record(&key(600), 0);
        assert_eq!(consume_getdata_response(&mut r, false, &key(0), 0), GetDataResponse::Late);
    }
}

//! Core v24.0.0's quorum connection topology and recovery member order.

use std::collections::BTreeSet;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

/// Returns the proTxHash that initiates the connection, with hashes in Core's stored byte order.
// Core: v24.0.0 src/llmq/utils.cpp:732-752 DeterministicOutboundConnection
pub fn deterministic_outbound(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    // Core: v24.0.0 src/uint256.h:55-59 operator< compares stored bytes with memcmp.
    let (lo, hi) = if a < b {
        (a, b)
    } else {
        (b, a)
    };
    let mut data = [0; 96];
    data[..32].copy_from_slice(&lo);
    data[32..64].copy_from_slice(&hi);
    data[64..].copy_from_slice(&a);
    let h1 = sha256d(&data);
    data[64..].copy_from_slice(&b);
    let h2 = sha256d(&data);
    if h1 < h2 {
        a
    } else {
        b
    }
}

/// `GetQuorumRelayMembers(params, members, forMember, onlyOutbound)`: the ring neighbours at
/// `(i + 2^k) mod n` of `for_member`, and with `only_outbound` off also the members that relay
/// to it.
// Core: v24.0.0 src/llmq/utils.cpp:778-831 GetQuorumRelayMembers
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

/// Core's mesh or relay ring, with `IsAllMembersConnectedEnabled` supplied by the caller.
/// Mesh outbound peers are those selected by `DeterministicOutboundConnection`.
// Core: v24.0.0 src/llmq/utils.cpp:754-776 GetQuorumConnections
pub fn quorum_connections(
    members: &[ProTxHash],
    me: &ProTxHash,
    only_outbound: bool,
    all_members_connected: bool,
) -> BTreeSet<ProTxHash> {
    if !all_members_connected {
        return quorum_relay_members(members, me, only_outbound);
    }
    members
        .iter()
        .copied()
        .filter(|peer| {
            peer != me
                && (!only_outbound
                    || deterministic_outbound(*me.as_bytes(), *peer.as_bytes()) == *peer.as_bytes())
        })
        .collect()
}

/// Core's watch connection indexes, with its process seed supplied by the caller.
/// Each draw advances the hash; repeated indexes reduce the resulting set's size.
/// `n` must be nonzero when `count` is nonzero, as Core's modulo requires.
// Core: v24.0.0 src/llmq/utils.cpp:833-853 CalcDeterministicWatchConnections
pub fn watch_connections(
    t: LlmqType,
    base: &BlockHash,
    n: usize,
    count: usize,
    seed: &[u8; 32],
) -> BTreeSet<usize> {
    let mut rnd = *seed;
    let mut result = BTreeSet::new();
    for _ in 0..count {
        rnd = sha256d(&[&rnd[..], &[t.to_base()], base.as_bytes()].concat());
        let word =
            u64::from_le_bytes([rnd[0], rnd[1], rnd[2], rnd[3], rnd[4], rnd[5], rnd[6], rnd[7]]);
        result.insert((word % n as u64) as usize);
    }
    result
}

/// Core's member indexes sorted by `SHA256d(proTxHash || id)` in stored byte order.
// Core: v24.0.0 src/llmq/signing_shares.cpp:764-780 CSigSharesManager::GetRecoveryMemberOrder
pub fn recovery_member_order(members: &[ProTxHash], id: &[u8; 32]) -> Vec<usize> {
    let mut hashed: Vec<_> = members
        .iter()
        .enumerate()
        .map(|(i, member)| (sha256d(&[&member.as_bytes()[..], &id[..]].concat()), i))
        .collect();
    hashed.sort_unstable();
    hashed.into_iter().map(|(_, i)| i).collect()
}

/// Core's recovery attempt modulo member count; `None` for an out-of-range attempt or no members.
// Core: v24.0.0 src/llmq/signing_shares.cpp:782-788 CSigSharesManager::SelectMemberForRecovery
pub fn select_member_for_recovery(
    members: &[ProTxHash],
    id: &[u8; 32],
    attempt: u32,
    recovery_members: u32,
) -> Option<usize> {
    if attempt >= recovery_members || members.is_empty() {
        return None;
    }
    let order = recovery_member_order(members, id);
    Some(order[attempt as usize % order.len()])
}

#[cfg(test)]
mod tests;

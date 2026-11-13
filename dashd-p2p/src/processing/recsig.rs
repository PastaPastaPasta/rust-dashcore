//! Pure recovered-signature relay decisions for Dash Core v24.

use dash_p2p::PeerId;

/// How to relay one recovered signature to a subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecsigPush {
    /// Send the signature in a `qsigrec` message.
    QsigRec,
    /// Queue an `MSG_QUORUM_RECOVERED_SIG` inventory entry with the signature's hash.
    Inv,
}

/// Select the relay action for each connected peer with processor state.
///
/// Each input is `(peer_id, wants_recsigs)`. Every subscriber receives the same action,
/// regardless of whether either end of the connection is a masternode. The caller supplies
/// the recovered signature for a push, or its hash for the inventory queue.
// Core: v24.0.0 src/net_processing.cpp:2668-2689 PeerManagerImpl::RelayRecoveredSig
pub fn recsig_relay(
    peers: impl IntoIterator<Item = (PeerId, bool)>,
    proactive: bool,
) -> Vec<(PeerId, RecsigPush)> {
    let push = if proactive {
        RecsigPush::QsigRec
    } else {
        RecsigPush::Inv
    };
    peers.into_iter().filter_map(|(id, wants)| wants.then_some((id, push))).collect()
}

/// Whether a peer can reconstruct an InstantSend lock from a recovered signature.
///
/// Only subscribers with a nonzero verified masternode registration hash run the signing
/// flow needed to reconstruct the lock. Other subscribers still need the ISDLOCK inventory.
// Core: v24.0.0 src/net_processing.cpp:1218-1226 PeerReconstructsISLockFromRecsig
pub fn peer_reconstructs_islock_from_recsig(
    wants_recsigs: bool,
    verified_pro_reg_tx_hash: &[u8; 32],
) -> bool {
    wants_recsigs && *verified_pro_reg_tx_hash != [0; 32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_modes_include_every_subscriber() {
        // The verified MN and non-MN subscriber get the same relay action.
        let peers =
            [(1, true, [1; 32]), (2, true, [0; 32]), (3, false, [2; 32]), (4, false, [0; 32])];
        for (proactive, push) in [(true, RecsigPush::QsigRec), (false, RecsigPush::Inv)] {
            let mut got =
                recsig_relay(peers.iter().rev().map(|&(id, wants, _)| (id, wants)), proactive);
            got.sort_unstable_by_key(|&(id, _)| id);
            assert_eq!(got, [(1, push), (2, push)]);
        }
        assert!(peer_reconstructs_islock_from_recsig(peers[0].1, &peers[0].2));
        assert!(!peer_reconstructs_islock_from_recsig(peers[1].1, &peers[1].2));
    }

    #[test]
    fn relay_without_subscribers_is_empty() {
        for proactive in [false, true] {
            assert!(recsig_relay([], proactive).is_empty());
            assert!(recsig_relay([(1, false), (2, false)], proactive).is_empty());
        }
    }

    #[test]
    fn reconstructs_islock_requires_subscription_and_verified_hash() {
        for wants in [false, true] {
            assert!(!peer_reconstructs_islock_from_recsig(wants, &[0; 32]));
            for byte in 0..32 {
                let mut hash = [0; 32];
                hash[byte] = 1;
                assert_eq!(peer_reconstructs_islock_from_recsig(wants, &hash), wants);
            }
        }
    }
}

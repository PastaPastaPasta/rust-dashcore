//! Quorum selection for signing (LQ1 §3.9, quirk 30): the start block and the choice among the
//! scanned quorums. QM1's `select_quorum_for_signing` scans through its facade with
//! `count = signingActiveQuorumCount` between the two.

use dash_consensus::hash::sha256d;
use dash_params::LlmqParams;
use dash_types::Numeric;

use super::params::signer_index_bits;
use super::scan::ScanEntry;

/// Core `llmq::SIGN_HEIGHT_OFFSET`: signing reads the quorums of the block this far below the
/// signing height.
// Core: v24 llmq/quorumsman.h:210 SIGN_HEIGHT_OFFSET
pub const SIGN_HEIGHT_OFFSET: u32 = 8;

/// Core `SelectQuorumForSigningStartBlock(active_chain, signHeight, signOffset)` as a height:
/// `signHeight` -1 is the tip; `None` when `signHeight - signOffset` is below 0 or above the tip.
/// The subtraction is exact (Core's `int` arithmetic, in the domain it does not overflow).
// Core: v24 llmq/quorumsman.cpp:738 SelectQuorumForSigningStartBlock
pub fn select_start_height(tip_height: i32, sign_height: i32, sign_offset: i32) -> Option<u32> {
    let sign_height = if sign_height == -1 {
        tip_height
    } else {
        sign_height
    };
    let start = i64::from(sign_height) - i64::from(sign_offset);
    if start > i64::from(tip_height) {
        return None;
    }
    u32::try_from(start).ok()
}

/// The choice in Core's `SelectQuorumForSigning` among `scanned`, the scan of
/// `signingActiveQuorumCount` quorums from the start block. `rotation_enabled` is
/// `is_quorum_rotation_enabled` at the start block, resolved by the caller.
///
/// Rotated: the signer index from bits of the selection hash's last 64-bit word; none when it
/// exceeds the number of scanned quorums (quirk 30b), else the first scanned quorum with that
/// index (30c). Not rotated: the least `SHA256d(type, quorumHash, selectionHash)`.
///
/// # Panics
///
/// If `rotation_enabled` and `signingActiveQuorumCount` is 0 (a negative count reads as its
/// magnitude): outside the domain [`LlmqCtx::new`](super::LlmqCtx::new) checks.
// Core: v24 llmq/quorumsman.cpp:751 SelectQuorumForSigning
pub fn select_from_scan<'q, Q: ScanEntry>(
    params: &LlmqParams,
    rotation_enabled: bool,
    scanned: &'q [Q],
    selection_hash: &[u8; 32],
) -> Option<&'q Q> {
    if scanned.is_empty() {
        return None;
    }
    if rotation_enabled {
        let n = signer_index_bits(params.signing_active_quorum_count.unsigned_abs());
        let word: [u8; 8] = selection_hash[24..].try_into().expect("8 bytes");
        let b = u64::from_le_bytes(word);
        let signer = ((1u64 << n) - 1) & (b >> (64 - n - 1));
        if signer > scanned.len() as u64 {
            return None;
        }
        // `uint64_t(qc->quorumIndex)` sign-extends the `int16_t`.
        return scanned.iter().find(|q| i64::from(q.quorum_index()) as u64 == signer);
    }
    let mut data = [0u8; 1 + 32 + 32];
    data[0] = params.llmq_type.to_base();
    data[33..].copy_from_slice(selection_hash);
    scanned
        .iter()
        .enumerate()
        .map(|(i, q)| {
            data[1..33].copy_from_slice(q.quorum_hash().as_bytes());
            // `uint256` orders by memcmp of its bytes; ties by position.
            (sha256d(&data), i, q)
        })
        .min_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
        .map(|(_, _, q)| q)
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
    use dash_primitives::{LlmqType, QuorumHash};

    use super::*;
    use dash_consensus::BlockId;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Q {
        hash: u8,
        index: i16,
    }

    impl ScanEntry for Q {
        fn base(&self) -> BlockId {
            BlockId(u32::from(self.hash))
        }
        fn quorum_hash(&self) -> QuorumHash {
            QuorumHash::from([self.hash; 32])
        }
        fn quorum_index(&self) -> i16 {
            self.index
        }
    }

    fn llmq(t: LlmqType) -> LlmqParams {
        let params: ChainParams =
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
        *params.consensus.llmq(t).expect("configured")
    }

    /// A selection hash whose last 64-bit word (little endian) is `word`.
    fn selection(word: u64) -> [u8; 32] {
        let mut h = [0x5a; 32];
        h[24..].copy_from_slice(&word.to_le_bytes());
        h
    }

    #[test]
    fn the_start_height_is_exact_and_bounded_by_the_tip() {
        assert_eq!(select_start_height(100, -1, 8), Some(92));
        assert_eq!(select_start_height(100, 50, 8), Some(42));
        assert_eq!(select_start_height(100, 108, 8), Some(100));
        assert_eq!(select_start_height(100, 109, 8), None);
        assert_eq!(select_start_height(100, 7, 8), None);
        assert_eq!(select_start_height(100, 8, 8), Some(0));
        // No wrap at the ends of `int`.
        assert_eq!(select_start_height(100, i32::MIN, i32::MAX), None);
        assert_eq!(select_start_height(100, i32::MAX, i32::MIN), None);
        assert_eq!(select_start_height(i32::MAX, -1, -1), None);
    }

    #[test]
    fn not_rotated_takes_the_least_hash_in_any_scan_order() {
        let params = llmq(LlmqType::LlmqTest);
        let sel = selection(7);
        let scanned: Vec<Q> = (1..=5)
            .map(|hash| Q {
                hash,
                index: 0,
            })
            .collect();
        let score = |q: &Q| {
            let mut data = vec![LlmqType::LlmqTest.to_base()];
            data.extend_from_slice(q.quorum_hash().as_bytes());
            data.extend_from_slice(&sel);
            sha256d(&data)
        };
        let least = scanned.iter().min_by_key(|q| score(q)).copied();
        assert_eq!(select_from_scan(&params, false, &scanned, &sel).copied(), least);
        let reversed: Vec<Q> = scanned.iter().rev().copied().collect();
        assert_eq!(select_from_scan(&params, false, &reversed, &sel).copied(), least);
        // A repeated quorum: the first of equals.
        let twice = [scanned[0], scanned[0]];
        assert!(std::ptr::eq(select_from_scan(&params, false, &twice, &sel).unwrap(), &twice[0]));
        assert_eq!(select_from_scan::<Q>(&params, false, &[], &sel), None);
    }

    #[test]
    fn rotated_signer_bits_and_the_index_guard() {
        // Quirk 30: two signing quorums use one bit, bit 62 of the last word.
        let params = llmq(LlmqType::LlmqTestDip0024);
        let scanned = [
            Q {
                hash: 1,
                index: 1,
            },
            Q {
                hash: 2,
                index: 0,
            },
            Q {
                hash: 3,
                index: 1,
            },
        ];
        let pick = |word, scanned: &[Q]| {
            select_from_scan(&params, true, scanned, &selection(word)).copied()
        };
        assert_eq!(pick(0, &scanned), Some(scanned[1]));
        assert_eq!(pick(1 << 62, &scanned), Some(scanned[0]), "the first with the index");
        assert_eq!(pick(1 << 63 | 1 << 61, &scanned), Some(scanned[1]), "other bits unread");
        // No quorum with the signer's index.
        assert_eq!(pick(1 << 62, &scanned[1..2]), None);

        // Four signing quorums use two bits (61, 62). The guard is `signer > size`, so a signer
        // equal to the scan's length passes it and finds nothing.
        let mut four = params;
        four.signing_active_quorum_count = 4;
        let two = [
            Q {
                hash: 1,
                index: 0,
            },
            Q {
                hash: 2,
                index: 2,
            },
        ];
        let pick = |word| select_from_scan(&four, true, &two, &selection(word)).copied();
        assert_eq!(pick(0b10 << 61), Some(two[1]), "signer 2 = size 2 passes the guard");
        assert_eq!(pick(0b11 << 61), None, "signer 3 > size 2");
        assert_eq!(pick(0b01 << 61), None, "no index 1");
        // A negative index sign-extends and never equals a signer.
        let negative = [Q {
            hash: 1,
            index: -1,
        }];
        for word in [0, 1 << 61, 1 << 62, 0b11 << 61] {
            assert_eq!(select_from_scan(&four, true, &negative, &selection(word)), None);
        }
    }
}

//! The `-debug=llmq` lines of Core's rotated member computation (LQ2 D28), as typed events.
//!
//! dash-evo logs nothing itself: the rotation sends each line to a [`MemberTrace`], and the node
//! formats it with [`MemberEvent::core_line`]. Every event borrows what the computation already
//! holds, so a trace makes no read and cannot change a result.

use std::fmt::Write as _;

use dash_params::LlmqType;
use dash_types::Numeric as _;

use super::rotation::PREVIOUS_CYCLES;
use super::view::MnEntryView;

/// One line of Core's rotated member computation (v24 `llmq/utils.cpp`).
#[derive(Debug)]
pub enum MemberEvent<'a, X> {
    /// `ComputeQuorumMembersByQuarterRotation` after the work block's list read (v24:485):
    /// `enabled` is the list's `GetCounts().enabled()`.
    Rotation {
        llmq_type: LlmqType,
        cycle_base_height: u32,
        enabled: usize,
    },
    /// `GetQuorumQuarterMembersBySnapshot` for the earlier cycle at `cycle_height`, read for
    /// the cycle at `from_height` (v24:226): the combined list, unused non-banned entries then
    /// used ones.
    QuartersBySnapshot {
        cycle_height: u32,
        from_height: u32,
        combined: &'a [X],
    },
    /// `BuildNewQuorumQuarterMembers` (v24:412): not used sorted, then used sorted.
    NewQuarters {
        cycle_base_height: u32,
        combined: &'a [X],
    },
    /// `BuildQuorumSnapshot` (v24:322): `total` is the work list's `GetCounts().total()`.
    Snapshot {
        cycle_base_height: u32,
        total: usize,
    },
    /// `QuarterComposition` for quorum index `index` (v24:525): `previous` newest first (H−C,
    /// H−2C, H−3C), as [`PREVIOUS_CYCLES`] orders them.
    QuarterComposition {
        cycle_base_height: u32,
        index: usize,
        previous: [&'a [X]; PREVIOUS_CYCLES],
        new: &'a [X],
    },
    /// `QuorumComposition` for quorum index `index` (v24:538).
    QuorumComposition {
        cycle_base_height: u32,
        index: usize,
        members: &'a [X],
    },
}

/// Core's `ToString(QuorumMembers)` (v24:35): each member's first four characters of
/// `proTxHash.ToString()`, then `|`.
fn members_string<X: MnEntryView>(members: &[X]) -> String {
    let mut s = String::with_capacity(members.len() * 5);
    for mn in members {
        let hash = mn.pro_tx_hash().to_string();
        s.push_str(hash.get(..4).unwrap_or(&hash));
        s.push('|');
    }
    s
}

impl<X: MnEntryView> MemberEvent<'_, X> {
    /// Core's text, after the category prefix.
    pub fn core_line(&self) -> String {
        match *self {
            MemberEvent::Rotation {
                llmq_type,
                cycle_base_height,
                enabled,
            } => format!(
                "ComputeQuorumMembersByQuarterRotation llmqType[{}] nHeight[{cycle_base_height}] \
                 allMns[{enabled}]",
                llmq_type.to_base()
            ),
            MemberEvent::QuartersBySnapshot {
                cycle_height,
                from_height,
                combined,
            } => format!(
                "GetQuorumQuarterMembersBySnapshot h[{cycle_height}] from[{from_height}] \
                 sortedCombinedMns[{}]",
                members_string(combined)
            ),
            MemberEvent::NewQuarters {
                cycle_base_height,
                combined,
            } => format!(
                "BuildNewQuorumQuarterMembers h[{cycle_base_height}] sortedCombinedMns[{}]",
                members_string(combined)
            ),
            MemberEvent::Snapshot {
                cycle_base_height,
                total,
            } => format!("BuildQuorumSnapshot h[{cycle_base_height}] numMns[{total}]"),
            MemberEvent::QuarterComposition {
                cycle_base_height,
                index,
                previous,
                new,
            } => {
                let mut s = format!("QuarterComposition h[{cycle_base_height}] i[{index}]:");
                for (cycle, quarter) in previous.iter().enumerate().rev() {
                    let _ = write!(s, " {cycle}Cmns[{}]", members_string(quarter));
                }
                let _ = write!(s, " new[{}]", members_string(new));
                s
            }
            MemberEvent::QuorumComposition {
                cycle_base_height,
                index,
                members,
            } => format!(
                "QuorumComposition h[{cycle_base_height}] i[{index}]: [{}]",
                members_string(members)
            ),
        }
    }
}

/// Where the rotated member computation reports its lines (LQ2 D28). Never affects results.
pub trait MemberTrace<X> {
    /// Records one line.
    fn event(&self, event: MemberEvent<'_, X>);

    /// Whether this trace takes lines. The computation sends no event, and makes no read for
    /// one, to a trace that takes none.
    fn takes_lines(&self) -> bool {
        true
    }
}

/// Discards every line.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMemberTrace;

impl<X> MemberTrace<X> for NoMemberTrace {
    fn event(&self, _: MemberEvent<'_, X>) {}

    fn takes_lines(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use dash_primitives::{OutPoint, TxHash};

    use super::*;
    use crate::types::ProTxHash;

    /// A masternode whose `proTxHash` prints as `prefix` followed by zeros.
    #[derive(Clone)]
    struct Mn(ProTxHash, OutPoint);

    impl MnEntryView for Mn {
        fn pro_tx_hash(&self) -> &ProTxHash {
            &self.0
        }
        fn collateral(&self) -> &OutPoint {
            &self.1
        }
        fn is_evo(&self) -> bool {
            false
        }
        fn is_banned(&self) -> bool {
            false
        }
        fn confirmed_hash_is_null(&self) -> bool {
            false
        }
        fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
            &[0; 32]
        }
    }

    /// `uint256::ToString` prints the bytes last first, so the first four characters are the
    /// last two bytes.
    fn mn(prefix: u16) -> Mn {
        let mut bytes = [0u8; 32];
        bytes[30..].copy_from_slice(&prefix.to_le_bytes());
        Mn(ProTxHash::from(bytes), OutPoint::new(TxHash::from([0; 32]), 0))
    }

    // Cycle 216 of `mn0-qc` as dashd v24.0.0 logs it with `-debug=llmq`.
    #[test]
    fn each_line_is_dashds_text() {
        let (a, b, c, d) = (mn(0x3ad2), mn(0x225f), mn(0xc915), mn(0x3dde));
        let line = |event: MemberEvent<'_, Mn>| event.core_line();
        assert_eq!(
            line(MemberEvent::Rotation {
                llmq_type: LlmqType::LlmqTestDip0024,
                cycle_base_height: 216,
                enabled: 4,
            }),
            "ComputeQuorumMembersByQuarterRotation llmqType[103] nHeight[216] allMns[4]"
        );
        assert_eq!(
            line(MemberEvent::QuartersBySnapshot {
                cycle_height: 192,
                from_height: 216,
                combined: &[a.clone(), b.clone(), c.clone(), d.clone()],
            }),
            "GetQuorumQuarterMembersBySnapshot h[192] from[216] sortedCombinedMns[3ad2|225f|c915|3dde|]"
        );
        assert_eq!(
            line(MemberEvent::NewQuarters {
                cycle_base_height: 216,
                combined: &[c.clone(), d.clone(), b.clone(), a.clone()],
            }),
            "BuildNewQuorumQuarterMembers h[216] sortedCombinedMns[c915|3dde|225f|3ad2|]"
        );
        assert_eq!(
            line(MemberEvent::Snapshot {
                cycle_base_height: 216,
                total: 4,
            }),
            "BuildQuorumSnapshot h[216] numMns[4]"
        );
        // `previous` is newest first: H−C is printed last, as `0Cmns`.
        assert_eq!(
            line(MemberEvent::QuarterComposition {
                cycle_base_height: 216,
                index: 0,
                previous: [
                    std::slice::from_ref(&c),
                    std::slice::from_ref(&a),
                    std::slice::from_ref(&b)
                ],
                new: std::slice::from_ref(&d),
            }),
            "QuarterComposition h[216] i[0]: 2Cmns[225f|] 1Cmns[3ad2|] 0Cmns[c915|] new[3dde|]"
        );
        assert_eq!(
            line(MemberEvent::QuorumComposition {
                cycle_base_height: 216,
                index: 0,
                members: &[b, a, c, d],
            }),
            "QuorumComposition h[216] i[0]: [225f|3ad2|c915|3dde|]"
        );
    }

    #[test]
    fn empty_lists_print_nothing_between_the_brackets() {
        let line = |event: MemberEvent<'_, Mn>| event.core_line();
        assert_eq!(
            line(MemberEvent::QuarterComposition {
                cycle_base_height: 24,
                index: 1,
                previous: [&[], &[], &[]],
                new: &[],
            }),
            "QuarterComposition h[24] i[1]: 2Cmns[] 1Cmns[] 0Cmns[] new[]"
        );
        assert_eq!(
            line(MemberEvent::QuorumComposition {
                cycle_base_height: 24,
                index: 0,
                members: &[],
            }),
            "QuorumComposition h[24] i[0]: []"
        );
        assert_eq!(
            line(MemberEvent::NewQuarters {
                cycle_base_height: 144,
                combined: &[mn(0x00ab)],
            }),
            "BuildNewQuorumQuarterMembers h[144] sortedCombinedMns[00ab|]"
        );
        assert_eq!(
            line(MemberEvent::QuartersBySnapshot {
                cycle_height: 144,
                from_height: 168,
                combined: &[],
            }),
            "GetQuorumQuarterMembersBySnapshot h[144] from[168] sortedCombinedMns[]"
        );
    }
}

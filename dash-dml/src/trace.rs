//! Core's `debugLogs` lines from the block build (EV2 §3.9): `ProcessSpecialTxsInBlock` builds
//! with `debugLogs = true`, so Core logs every registration, update, revocation, removal, PoSe
//! punishment and ban with fixed texts. They never affect results.

use std::sync::Arc;

use dash_evo::llmq::LlmqCommitment;
use dash_num::Hash256;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{DynBitset, OutPoint, QuorumHash};
use dash_types::Numeric;

use crate::blockprocessor::quorum_index;
use crate::{Dmn, ProTxHash};

/// One `debugLogs` line of `RebuildListFromBlock` or `PoSePunish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmlEvent {
    /// A ProRegTx reused a registered collateral (`specialtxman.cpp` v24:395-399).
    Replaced {
        pro_tx: ProTxHash,
        collateral: OutPoint,
        height: i32,
        total: u64,
    },
    /// A ProRegTx added a masternode (v24:450-452).
    Added {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpServTx revived a banned masternode (v24:500-502).
    Revived {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpServTx or ProUpRegTx updated a masternode (v24:518-520, 583-585).
    Updated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpRevTx revoked the operator key (v24:615-617).
    Revoked {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpShareTx updated a share (v24:645-647).
    ShareUpdated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpSharedRegTx updated a shared registrar (v24:698-700).
    SharedRegistrarUpdated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A transaction spent a collateral (v24:749-753).
    CollateralSpent {
        pro_tx: ProTxHash,
        collateral: OutPoint,
        height: i32,
        total: u64,
    },
    /// `PoSePunish` on a masternode that was not banned (`deterministicmns.cpp` v24:298-302).
    Punished {
        pro_tx: ProTxHash,
        height: i32,
        banned: bool,
        from: i32,
        to: i32,
        max: i32,
    },
    /// An Evo payee's streak grew (v24:769-771; category `mnpayments`).
    StreakBumped {
        pro_tx: ProTxHash,
        to: i32,
    },
    /// An unpaid Evo's streak was reset (v24:783-785; category `mnpayments`).
    StreakReset {
        pro_tx: ProTxHash,
        from: i32,
    },
}

impl DmlEvent {
    /// Core's log text, without the `__func__` prefix and, for `Added`, `Updated`, `Revoked`,
    /// `ShareUpdated` and `SharedRegistrarUpdated`, without the `": " + proTx.ToString()` suffix
    /// Core appends (`specialtxman.cpp` v24:451, 519, 584, 616, 646, 699; EV2b D19).
    pub fn core_message(&self) -> String {
        match self {
            DmlEvent::Replaced {
                pro_tx,
                collateral,
                height,
                total,
            } => format!(
                "MN {pro_tx} removed from list because collateral was used for a new ProRegTx. \
                 collateralOutpoint={}, nHeight={height}, mapCurMNs.allMNsCount={total}",
                short(collateral)
            ),
            DmlEvent::Added {
                pro_tx,
                height,
            } => format!("MN {pro_tx} added at height {height}"),
            DmlEvent::Revived {
                pro_tx,
                height,
            } => format!("MN {pro_tx} revived at height {height}"),
            DmlEvent::Updated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated at height {height}"),
            DmlEvent::Revoked {
                pro_tx,
                height,
            } => format!("MN {pro_tx} revoked operator key at height {height}"),
            DmlEvent::ShareUpdated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated share at height {height}"),
            DmlEvent::SharedRegistrarUpdated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated shared registrar at height {height}"),
            DmlEvent::CollateralSpent {
                pro_tx,
                collateral,
                height,
                total,
            } => format!(
                "MN {pro_tx} removed from list because collateral was spent. \
                 collateralOutpoint={}, nHeight={height}, mapCurMNs.allMNsCount={total}",
                short(collateral)
            ),
            DmlEvent::Punished {
                pro_tx,
                height,
                banned,
                from,
                to,
                max,
            } => format!(
                "{} MN {pro_tx} at height {height}, penalty {from}->{to} (max={max})",
                if *banned {
                    "banned"
                } else {
                    "punished"
                }
            ),
            DmlEvent::StreakBumped {
                pro_tx,
                to,
            } => format!("MN {pro_tx} is an EvoNode, bumping nConsecutivePayments to {to}"),
            DmlEvent::StreakReset {
                pro_tx,
                from,
            } => format!("MN {pro_tx}, reset nConsecutivePayments {from}->0"),
        }
    }
}

/// `COutPoint::ToStringShort()`: `txid-n`.
fn short(op: &OutPoint) -> String {
    format!("{}-{}", op.txid(), op.index())
}

/// Where the build reports its `debugLogs` lines (EV2 §2.11). Never affects results.
pub trait DmlTrace {
    /// Records one line.
    fn event(&mut self, event: DmlEvent);
}

/// Discards every line.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTrace;

impl DmlTrace for NoTrace {
    fn event(&mut self, _: DmlEvent) {}
}

/// Collects every line, in order.
impl DmlTrace for Vec<DmlEvent> {
    fn event(&mut self, event: DmlEvent) {
        self.push(event);
    }
}

/// One line Core logs on the mined-commitment path of a block when its checks pass (LQ2 D26):
/// `CheckLLMQCommitment`, `ProcessBlock` and `ProcessCommitment`, with the `Verify` and
/// `VerifySignatureAsync` dumps they reach. All are `-debug=llmq` lines except
/// [`QcEvent::CommitmentsRequired`], Core's `LogPrintf` ([`QcEvent::is_unconditional`]).
#[derive(Clone, Copy, Debug)]
pub enum QcEvent<'a> {
    /// `CheckLLMQCommitment`'s `validMembers` dump, for a commitment of a configured type of
    /// `size` members (`llmq/commitment.cpp` v24:257-267).
    CheckDump {
        qc: &'a LlmqCommitment,
        size: u64,
    },
    /// `Verify`'s dump: `members` is the member count it read (v24:178-186).
    VerifyDump {
        qc: &'a LlmqCommitment,
        members: usize,
        size: u64,
    },
    /// `Verify` returns true (v24:206).
    ValidQuorum {
        qc: &'a LlmqCommitment,
    },
    /// `CheckLLMQCommitment` passes a non-null commitment checked on top of `height` (v24:307).
    CheckValid {
        height: u32,
    },
    /// `ProcessBlock`'s count line for a type rotation is enabled for at the block of `height`
    /// (`llmq/blockprocessor.cpp` v24:250-252).
    CommitmentsRequired {
        height: u32,
        required: usize,
        in_block: usize,
    },
    /// `VerifySignatureAsync`'s members dump, per non-null commitment whose checks are queued
    /// (`llmq/commitment.cpp` v24:61-68). `scheme` is the block's.
    SignatureAsync {
        qc: &'a LlmqCommitment,
        members: &'a [Arc<Dmn>],
        scheme: BlsSchemeKind,
        commitment_hash: &'a [u8; 32],
    },
    /// `ProcessCommitment`'s first line, for the block of `height`; `quorum_hash` is
    /// `GetQuorumBlockHash`'s, `None` for its null hash (`llmq/blockprocessor.cpp` v24:340-345).
    Processing {
        qc: &'a LlmqCommitment,
        height: u32,
        quorum_hash: Option<QuorumHash>,
        scheme: BlsSchemeKind,
        just_check: bool,
    },
    /// A rotated quorum's commitment of the block of `height`, based at `base_height`, is about
    /// to be written (v24:423-426).
    Built {
        qc: &'a LlmqCommitment,
        height: u32,
        base_height: u32,
    },
    /// The commitment's rows are written (v24:455-456).
    Processed {
        qc: &'a LlmqCommitment,
    },
}

impl QcEvent<'_> {
    /// Whether Core logs the line whatever the categories (`LogPrintf`), not under `llmq`.
    pub fn is_unconditional(&self) -> bool {
        matches!(self, QcEvent::CommitmentsRequired { .. })
    }

    /// Core's text, after the category prefix.
    pub fn core_line(&self) -> String {
        match *self {
            QcEvent::CheckDump {
                qc,
                size,
            } => format!(
                "CFinalCommitment -- CheckLLMQCommitment llmqType[{}] validMembers[{}] signers[]",
                qc.llmq_type().to_base(),
                bits('v', qc.valid_members(), size.min(qc.valid_members().num_bits()))
            ),
            QcEvent::VerifyDump {
                qc,
                members,
                size,
            } => format!(
                "CFinalCommitment::Verify mns[{members}] validMembers[{}] signers[{}]",
                bits('v', qc.valid_members(), size),
                bits('s', qc.signers(), size)
            ),
            QcEvent::ValidQuorum {
                qc,
            } => format!("CFinalCommitment -- q[{}] VALID QUORUM", qc.quorum_hash()),
            QcEvent::CheckValid {
                height,
            } => format!("CFinalCommitment -- h[{height}] CheckLLMQCommitment VALID"),
            QcEvent::CommitmentsRequired {
                height,
                required,
                in_block,
            } => format!(
                "[ProcessBlock] h[{height}] numCommitmentsRequired[{required}] \
                 numCommitmentsInNewBlock[{in_block}]"
            ),
            QcEvent::SignatureAsync {
                qc,
                members,
                scheme,
                commitment_hash,
            } => {
                let mut list = String::new();
                for mn in members {
                    let hash = mn.pro_tx_hash.to_string();
                    list.push_str(&hash[..4]);
                    list.push_str(" | ");
                }
                format!(
                    "CFinalCommitment::VerifySignatureAsync members[{list}] quorumPublicKey[{}] \
                     commitmentHash[{}]",
                    hex(&qc.quorum_public_key().to_bytes(scheme)),
                    Hash256::from(*commitment_hash)
                )
            }
            QcEvent::Processing {
                qc,
                height,
                quorum_hash,
                scheme,
                just_check,
            } => format!(
                "ProcessCommitment -- processing commitment for block height={height}, type={}, \
                 quorumIndex={}, quorumHash={}, signers={}, validMembers={}, quorumPublicKey={} \
                 fJustCheck[{}] processing commitment from block.",
                qc.llmq_type().to_base(),
                quorum_index(qc),
                quorum_hash.unwrap_or_default(),
                qc.signers().count_ones(),
                qc.valid_members().count_ones(),
                hex(&qc.quorum_public_key().to_bytes(scheme)),
                u8::from(just_check)
            ),
            QcEvent::Built {
                qc,
                height,
                base_height,
            } => format!(
                "ProcessCommitment -- height[{height}] pQuorumBaseBlockIndex[{base_height}] \
                 quorumIndex[{}] qversion[{}] Built",
                quorum_index(qc),
                qc.version()
            ),
            QcEvent::Processed {
                qc,
            } => format!(
                "ProcessCommitment -- processed commitment from block. type={}, quorumIndex={}, \
                 quorumHash={}",
                qc.llmq_type().to_base(),
                quorum_index(qc),
                qc.quorum_hash()
            ),
        }
    }
}

/// Core's bitset dump: `v[0]=1v[1]=0…` for the first `n` bits, a bit past the bitset's bytes 0.
fn bits(name: char, set: &DynBitset, n: u64) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for i in 0..n {
        let _ = write!(s, "{name}[{i}]={}", u8::from(set.get(i) == Some(true)));
    }
    s
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Where the quorum block processor and `CheckLLMQCommitment` report their lines (LQ2 D26).
/// Never affects results.
pub trait QcTrace {
    /// Records one line.
    fn event(&self, event: QcEvent<'_>);

    /// Whether this trace takes lines at all. A line that needs a read only to decide whether it
    /// is logged makes that read only when this is true, so a discarding trace adds none.
    fn takes_lines(&self) -> bool {
        true
    }
}

/// Discards every line.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoQcTrace;

impl QcTrace for NoQcTrace {
    fn event(&self, _: QcEvent<'_>) {}

    fn takes_lines(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use dash_params::LlmqType;
    use dash_pkc::bls::core::BlsSigValue;
    use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeSource};
    use dash_primitives::{Commitment, QuorumVvecHash};

    use super::*;
    use crate::MnType;
    use crate::testkit::dmn_with;

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex"))
            .collect()
    }

    fn bitset(n: u64, set: &[u64]) -> DynBitset {
        let mut data = vec![0u8; usize::try_from(n.div_ceil(8)).expect("small")];
        for &i in set {
            data[(i / 8) as usize] |= 1 << (i % 8);
        }
        DynBitset::new(n, data).expect("a bitset")
    }

    // Core's regtest log (`tools/core-lines.out`): LLMQ_TEST's commitment mined at 179.
    const Q: &str = "0c2375cd2a5e820737bf1126779a8636419813dc54ffd94eb7d22d44bdf346c4";
    const PK: &str = "8612b6f0633063d1387bead476db263c79fb2ef069f2fb17f44d484eee5dae81\
                      6076c2a1a1468865a66504c277063aef";

    fn commitment(version: u16, index: Option<i16>, valid: DynBitset, pk: &str) -> Commitment {
        let key = if pk.is_empty() {
            BlsEagerPk::new(dash_pkc::bls::core::BlsPkValue::invalid(), BlsSchemeKind::Basic)
        } else {
            BlsEagerPk::decode(
                &<[u8; 48]>::try_from(bytes(pk)).expect("48 bytes"),
                BlsSchemeSource::Version(BlsSchemeKind::Basic),
            )
            .expect("Core's key")
        };
        Commitment::new(
            version,
            LlmqType::LlmqTest,
            QuorumHash::from_hex(Q).expect("hex"),
            index,
            valid.clone(),
            valid,
            key,
            QuorumVvecHash::from([0x11; 32]),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        )
    }

    /// T1: every line's text against Core's own output (`core-lines.out`), `Built` against
    /// `blockprocessor.cpp` v24:423-426.
    #[test]
    fn qc_lines_are_cores_text() {
        let qc = commitment(3, None, bitset(3, &[0, 1]), PK);
        let line = |e: QcEvent<'_>| e.core_line();
        assert_eq!(
            line(QcEvent::CheckDump {
                qc: &qc,
                size: 3,
            }),
            "CFinalCommitment -- CheckLLMQCommitment llmqType[100] \
             validMembers[v[0]=1v[1]=1v[2]=0] signers[]"
        );
        // A bitset shorter than the type's size prints only its own bits (v24:261).
        let short = commitment(3, None, bitset(2, &[0, 1]), PK);
        assert_eq!(
            line(QcEvent::CheckDump {
                qc: &short,
                size: 3,
            }),
            "CFinalCommitment -- CheckLLMQCommitment llmqType[100] validMembers[v[0]=1v[1]=1] \
             signers[]"
        );
        assert_eq!(
            line(QcEvent::VerifyDump {
                qc: &qc,
                members: 2,
                size: 3,
            }),
            "CFinalCommitment::Verify mns[2] validMembers[v[0]=1v[1]=1v[2]=0] \
             signers[s[0]=1s[1]=1s[2]=0]"
        );
        assert_eq!(
            line(QcEvent::ValidQuorum {
                qc: &qc,
            }),
            format!("CFinalCommitment -- q[{Q}] VALID QUORUM")
        );
        assert_eq!(
            line(QcEvent::CheckValid {
                height: 178,
            }),
            "CFinalCommitment -- h[178] CheckLLMQCommitment VALID"
        );
        assert_eq!(
            line(QcEvent::CommitmentsRequired {
                height: 179,
                required: 0,
                in_block: 0,
            }),
            "[ProcessBlock] h[179] numCommitmentsRequired[0] numCommitmentsInNewBlock[0]"
        );
        let mn = |hash: &str| {
            dmn_with(1, MnType::Regular, |dmn, _| {
                dmn.pro_tx_hash = ProTxHash::from(Hash256::from_hex(hash).expect("hex"));
            })
        };
        let members = [
            mn("d1b5000000000000000000000000000000000000000000000000000000000001"),
            mn("b03e000000000000000000000000000000000000000000000000000000000002"),
        ];
        let hash =
            *Hash256::from_hex("b9643efa444788c08da94ca4d0a38b67d1eb6e4c9bfc74cd4abcd7b75a0f19c2")
                .expect("hex")
                .as_bytes();
        assert_eq!(
            line(QcEvent::SignatureAsync {
                qc: &qc,
                members: &members,
                scheme: BlsSchemeKind::Basic,
                commitment_hash: &hash,
            }),
            format!(
                "CFinalCommitment::VerifySignatureAsync members[d1b5 | b03e | ] \
                 quorumPublicKey[{PK}] \
                 commitmentHash[b9643efa444788c08da94ca4d0a38b67d1eb6e4c9bfc74cd4abcd7b75a0f19c2]"
            )
        );
        assert_eq!(
            line(QcEvent::Processing {
                qc: &qc,
                height: 179,
                quorum_hash: Some(qc.quorum_hash()),
                scheme: BlsSchemeKind::Basic,
                just_check: true,
            }),
            format!(
                "ProcessCommitment -- processing commitment for block height=179, type=100, \
                 quorumIndex=0, quorumHash={Q}, signers=2, validMembers=2, quorumPublicKey={PK} \
                 fJustCheck[1] processing commitment from block."
            )
        );
        // A null commitment: zero counts and the invalid key's bytes; Core's null quorum hash
        // where `GetQuorumBlockHash` has no block (crash replay).
        let null = commitment(3, None, bitset(3, &[]), "");
        let zeros_pk = "0".repeat(96);
        let zeros_hash = "0".repeat(64);
        assert_eq!(
            line(QcEvent::Processing {
                qc: &null,
                height: 10,
                quorum_hash: None,
                scheme: BlsSchemeKind::Basic,
                just_check: false,
            }),
            format!(
                "ProcessCommitment -- processing commitment for block height=10, type=100, \
                 quorumIndex=0, quorumHash={zeros_hash}, signers=0, validMembers=0, \
                 quorumPublicKey={zeros_pk} fJustCheck[0] processing commitment from block."
            )
        );
        let rotated = commitment(4, Some(1), bitset(3, &[0, 1]), PK);
        assert_eq!(
            line(QcEvent::Built {
                qc: &rotated,
                height: 60,
                base_height: 49,
            }),
            "ProcessCommitment -- height[60] pQuorumBaseBlockIndex[49] quorumIndex[1] \
             qversion[4] Built"
        );
        assert_eq!(
            line(QcEvent::Processed {
                qc: &qc,
            }),
            format!(
                "ProcessCommitment -- processed commitment from block. type=100, quorumIndex=0, \
                 quorumHash={Q}"
            )
        );
    }

    /// Only the count line is Core's `LogPrintf`.
    #[test]
    fn only_the_count_line_is_unconditional() {
        let qc = commitment(3, None, bitset(3, &[0, 1]), PK);
        assert!(
            QcEvent::CommitmentsRequired {
                height: 1,
                required: 0,
                in_block: 0,
            }
            .is_unconditional()
        );
        assert!(
            !QcEvent::ValidQuorum {
                qc: &qc,
            }
            .is_unconditional()
        );
        assert!(
            !QcEvent::CheckValid {
                height: 1,
            }
            .is_unconditional()
        );
    }
}

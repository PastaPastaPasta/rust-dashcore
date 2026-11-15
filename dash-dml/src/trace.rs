//! Core's `debugLogs` lines from the block build (EV2 §3.9): `ProcessSpecialTxsInBlock` builds
//! with `debugLogs = true`, so Core logs every registration, update, revocation, removal, PoSe
//! punishment and ban with fixed texts. They never affect results.

use std::sync::Arc;

use dash_evo::llmq::{CommitmentReject, LlmqCommitment};
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
    /// `ProcessBlock` connected the DIP3 enforcement height (v24:779).
    Dip3Enforced {
        height: i32,
    },
    /// `UndoBlock` disconnected the DIP3 enforcement height (v24:812-814).
    Dip3NotEnforced {
        height: i32,
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
            DmlEvent::Dip3Enforced {
                height,
            } => format!("DIP3 is enforced now. nHeight={height}"),
            DmlEvent::Dip3NotEnforced {
                height,
            } => format!("DIP3 is not enforced anymore. nHeight={height}"),
        }
    }

    /// Core's whole line for the `CDeterministicMNManager` lines the node logs, `LogPrintf`
    /// both; `None` for the build's lines, whose text lacks the suffix Core appends (EV2b
    /// D19), so the node does not log them.
    pub fn core_line(&self) -> Option<String> {
        let func = match self {
            DmlEvent::Dip3Enforced {
                ..
            } => "ProcessBlock",
            DmlEvent::Dip3NotEnforced {
                ..
            } => "UndoBlock",
            _ => return None,
        };
        Some(format!("CDeterministicMNManager::{func} -- {}", self.core_message()))
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

/// Why `CheckLLMQCommitment` refuses a commitment, with the value its line prints
/// (`llmq/commitment.cpp` v24:244-302, LQ2 D27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckReject {
    /// The payload does not decode (`bad-qc-payload`).
    Payload,
    /// The type is not configured (`bad-qc-commitment-type`): `llmqType`.
    Type(u8),
    /// The payload version (`bad-qc-version`): `qcTx.nVersion`.
    Version(u16),
    /// The payload height (`bad-qc-height`): `qcTx.nHeight`.
    Height(u32),
    /// A null commitment fails `VerifyNull` (`bad-qc-invalid-null`): its quorum hash.
    Null(QuorumHash),
    /// A commitment fails `Verify` (`bad-qc-invalid`): its quorum hash.
    Verify(QuorumHash),
}

/// Which of `ProcessCommitment`'s logged rejects (`llmq/blockprocessor.cpp` v24:353-413).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessReject {
    /// The quorum hash is null (`bad-qc-block`, :353-357).
    NullHash,
    /// The quorum hash differs from the commitment's (`bad-qc-block`, :361-365).
    NotEqual,
    /// A null commitment fails `VerifyNull` (`bad-qc-invalid-null`, :371-375).
    VerifyNull,
    /// The commitment fails `Verify` (`bad-qc-invalid`, :409-413).
    Verify,
}

/// One line Core logs on the mined-commitment path of a block (LQ2 D26 when its checks pass, D27
/// when one fails): `CheckLLMQCommitment`, `ProcessBlock` and `ProcessCommitment`, with the
/// `Verify` and `VerifySignatureAsync` dumps they reach. All are `-debug=llmq` lines except
/// [`QcEvent::CommitmentsRequired`] and [`QcEvent::Failed`], Core's `LogPrintf`
/// ([`QcEvent::is_unconditional`]).
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
    /// `Verify` (or `VerifyNull`'s `VerifySizes`) refuses the commitment
    /// (`llmq/commitment.cpp` v24:117-194, :229-233).
    VerifyReject {
        qc: &'a LlmqCommitment,
        reject: CommitmentReject,
    },
    /// `CheckLLMQCommitment` refuses a commitment checked on top of `height` (v24:244-302).
    CheckReject {
        height: u32,
        reject: CheckReject,
    },
    /// `ProcessCommitment` refuses a commitment of the block of `height`; `quorum_hash` is the
    /// one it compares, `None` for the null hash (`llmq/blockprocessor.cpp` v24:353-413).
    ProcessReject {
        qc: &'a LlmqCommitment,
        height: u32,
        quorum_hash: Option<QuorumHash>,
        scheme: BlsSchemeKind,
        reject: ProcessReject,
    },
    /// The block index has no entry for a commitment's quorum hash: `ProcessBlock`'s, for the
    /// block of `height`, before its signature checks (v24:273), else `ProcessCommitment`'s
    /// (v24:402).
    UnknownBase {
        qc: &'a LlmqCommitment,
        height: Option<u32>,
    },
    /// `ProcessCommitment` on a type that is not configured (v24:333).
    InvalidType {
        qc: &'a LlmqCommitment,
    },
    /// `ProcessCommitment` failed for a commitment of the block of `height` (v24:287).
    Failed {
        qc: &'a LlmqCommitment,
        height: u32,
    },
    /// `GetQuorumBlockHash` finds no active-chain block at `start`, the cycle start of `height`
    /// plus `quorum_index` (v24:637).
    QuorumBlockHashEmpty {
        llmq_type: u8,
        height: u32,
        quorum_index: i64,
        start: i64,
    },
}

impl QcEvent<'_> {
    /// Whether Core logs the line whatever the categories (`LogPrintf`), not under `llmq`.
    pub fn is_unconditional(&self) -> bool {
        matches!(self, QcEvent::CommitmentsRequired { .. } | QcEvent::Failed { .. })
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
            QcEvent::VerifyReject {
                qc,
                reject,
            } => {
                let text = match reject {
                    CommitmentReject::LlmqType => {
                        format!("invalid llmqType={}", qc.llmq_type().to_base())
                    }
                    CommitmentReject::QuorumIndex => {
                        format!("invalid quorumIndex={}", quorum_index(qc))
                    }
                    CommitmentReject::SignersSize => {
                        format!("invalid signers.size={}", qc.signers().num_bits())
                    }
                    CommitmentReject::ValidMembersSize => {
                        format!("invalid validMembers.size={}", qc.valid_members().num_bits())
                    }
                    CommitmentReject::ValidMembersCount => format!(
                        "invalid validMembers count. validMembersCount={}",
                        qc.valid_members().count_ones()
                    ),
                    CommitmentReject::SignersCount => {
                        format!("invalid signers count. signersCount={}", qc.signers().count_ones())
                    }
                    other => other.to_string(),
                };
                format!("CFinalCommitment -- q[{}] {text}", qc.quorum_hash())
            }
            QcEvent::CheckReject {
                height,
                reject,
            } => {
                let text = match reject {
                    CheckReject::Payload => "GetTxPayload LLMQCommitment failed".to_owned(),
                    CheckReject::Type(t) => format!("GetLLMQ failed for llmqType[{t}]"),
                    CheckReject::Version(v) => format!("invalid qcTx.nVersion[{v}]"),
                    CheckReject::Height(h) => format!("invalid qcTx.nHeight[{h}]"),
                    CheckReject::Null(q) => {
                        format!("invalid qcTx.commitment[{q}] VerifyNull failed")
                    }
                    CheckReject::Verify(q) => format!("invalid qcTx.commitment[{q}] Verify failed"),
                };
                format!("CFinalCommitment -- h[{height}] {text}")
            }
            QcEvent::ProcessReject {
                qc,
                height,
                quorum_hash,
                scheme,
                reject,
            } => {
                let signers = qc.signers().count_ones();
                let valid = qc.valid_members().count_ones();
                let key = || hex(&qc.quorum_public_key().to_bytes(scheme));
                let tail = match reject {
                    ProcessReject::NullHash => format!(
                        "signers={signers}, validMembers={valid}, quorumPublicKey={} quorumHash \
                         is null.",
                        key()
                    ),
                    ProcessReject::NotEqual => format!(
                        "qc.quorumHash={} signers={signers}, validMembers={valid}, \
                         quorumPublicKey={} non equal quorumHash.",
                        qc.quorum_hash(),
                        key()
                    ),
                    // Core's text has no space before `qc` (v24:372).
                    ProcessReject::VerifyNull => {
                        format!("signers={signers}, validMembers={valid}qc verifynull failed.")
                    }
                    ProcessReject::Verify => format!(
                        "signers={signers}, validMembers={valid}, quorumPublicKey={} qc verify \
                         failed.",
                        key()
                    ),
                };
                format!(
                    "ProcessCommitment -- height={height}, type={}, quorumIndex={}, \
                     quorumHash={}, {tail}",
                    qc.llmq_type().to_base(),
                    quorum_index(qc),
                    quorum_hash.unwrap_or_default()
                )
            }
            QcEvent::UnknownBase {
                qc,
                height,
            } => {
                let prefix = match height {
                    Some(h) => format!("[ProcessBlock] h[{h}]"),
                    None => "ProcessCommitment --".to_owned(),
                };
                format!(
                    "{prefix} unexpectedly failed due to no known pindex for hash[{}]",
                    qc.quorum_hash()
                )
            }
            QcEvent::InvalidType {
                qc,
            } => {
                format!("ProcessCommitment -- invalid commitment type {}", qc.llmq_type().to_base())
            }
            QcEvent::Failed {
                qc,
                height,
            } => format!(
                "[ProcessBlock] failed h[{height}] llmqType[{}] version[{}] quorumIndex[{}] \
                 quorumHash[{}]",
                qc.llmq_type().to_base(),
                qc.version(),
                quorum_index(qc),
                qc.quorum_hash()
            ),
            QcEvent::QuorumBlockHashEmpty {
                llmq_type,
                height,
                quorum_index,
                start,
            } => format!(
                "[GetQuorumBlockHash] llmqType[{llmq_type}] h[{height}] qi[{quorum_index}] \
                 quorumStartHeight[{start}] quorumHash[EMPTY]"
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

    /// The DIP3 enforcement lines the node logs, as Core's `LogPrintf` formats them; the build's
    /// own lines have no node text.
    // Core: evo/deterministicmns.cpp:779, :813 (v24.0.0)
    #[test]
    fn the_dmn_manager_lines_are_cores_text() {
        let lines = [
            DmlEvent::Dip3Enforced {
                height: 5,
            },
            DmlEvent::Dip3NotEnforced {
                height: 2,
            },
        ]
        .map(|e| e.core_line());
        assert_eq!(
            lines,
            [
                "CDeterministicMNManager::ProcessBlock -- DIP3 is enforced now. nHeight=5",
                "CDeterministicMNManager::UndoBlock -- DIP3 is not enforced anymore. nHeight=2",
            ]
            .map(|l| Some(l.to_owned()))
        );
        let added = DmlEvent::Added {
            pro_tx: ProTxHash::from(Hash256::from_hex(&"11".repeat(32)).expect("hex")),
            height: 3,
        };
        assert_eq!(added.core_line(), None, "the build's lines are not the node's");
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

    /// The count line and `[ProcessBlock] failed` are Core's `LogPrintf` (`blockprocessor.cpp`
    /// v24:251, :287); every other line is `LogPrint(BCLog::LLMQ, …)`.
    #[test]
    fn only_the_count_and_failed_lines_are_unconditional() {
        let qc = commitment(3, None, bitset(3, &[0, 1]), PK);
        let q = qc.quorum_hash();
        let unconditional = [
            QcEvent::CommitmentsRequired {
                height: 1,
                required: 0,
                in_block: 0,
            },
            QcEvent::Failed {
                qc: &qc,
                height: 1,
            },
        ];
        assert!(unconditional.iter().all(QcEvent::is_unconditional));
        let members = [];
        let hash = [0; 32];
        let llmq = [
            QcEvent::CheckDump {
                qc: &qc,
                size: 3,
            },
            QcEvent::VerifyDump {
                qc: &qc,
                members: 0,
                size: 3,
            },
            QcEvent::ValidQuorum {
                qc: &qc,
            },
            QcEvent::CheckValid {
                height: 1,
            },
            QcEvent::SignatureAsync {
                qc: &qc,
                members: &members,
                scheme: BlsSchemeKind::Basic,
                commitment_hash: &hash,
            },
            QcEvent::Processing {
                qc: &qc,
                height: 1,
                quorum_hash: None,
                scheme: BlsSchemeKind::Basic,
                just_check: false,
            },
            QcEvent::Built {
                qc: &qc,
                height: 1,
                base_height: 0,
            },
            QcEvent::Processed {
                qc: &qc,
            },
            QcEvent::VerifyReject {
                qc: &qc,
                reject: CommitmentReject::QuorumHash,
            },
            QcEvent::CheckReject {
                height: 1,
                reject: CheckReject::Verify(q),
            },
            QcEvent::ProcessReject {
                qc: &qc,
                height: 1,
                quorum_hash: Some(q),
                scheme: BlsSchemeKind::Basic,
                reject: ProcessReject::NotEqual,
            },
            QcEvent::UnknownBase {
                qc: &qc,
                height: Some(1),
            },
            QcEvent::UnknownBase {
                qc: &qc,
                height: None,
            },
            QcEvent::InvalidType {
                qc: &qc,
            },
            QcEvent::QuorumBlockHashEmpty {
                llmq_type: 100,
                height: 1,
                quorum_index: 0,
                start: 0,
            },
        ];
        assert!(llmq.iter().all(|e| !e.is_unconditional()));
    }

    /// A commitment of `llmq_type` on quorum `q` with every field the reject lines print.
    fn qc_of(
        llmq_type: LlmqType,
        version: u16,
        q: &str,
        index: Option<i16>,
        signers: DynBitset,
        valid: DynBitset,
        pk: &str,
    ) -> Commitment {
        let key = BlsEagerPk::decode(
            &<[u8; 48]>::try_from(bytes(pk)).expect("48 bytes"),
            BlsSchemeSource::Version(BlsSchemeKind::Basic),
        )
        .expect("Core's key");
        Commitment::new(
            version,
            llmq_type,
            QuorumHash::from_hex(q).expect("hex"),
            index,
            signers,
            valid,
            key,
            QuorumVvecHash::from([0x11; 32]),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        )
    }

    // dashd v24.0.0 on the `mn0-qc` recording (BL-851 `tools/reject-lines.out`).
    const Q216: &str = "7e5c0dcaa81be8fbbdba60989ffcd17b85b982cde6b9b9cf22f4de2a35ae10af";
    const Q240: &str = "5a97caa3a6c2e1bdbb42611e3b6d7b579049c3c0a65909611b28608a3767b9ce";
    const Q217: &str = "676c3cdf542bf89cd8f4141a59426d40bb8741ca0ead23f133c2d12b6aff4f93";
    const Q241: &str = "7257f5f216f51c931c2e85faead2135cad8ec4291ddac94b12d1460862ebbd26";
    const Q24: &str = "0347e6f34f5711cbfdb69bcbed89b2c6f9919b51916061880372011508df9cc0";
    const GENESIS: &str = "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e";
    const Q215: &str = "3dddb819a161d4eb5ceeeed843b08cabd77cbb87bc8fca479facea12c02e6945";
    const PK100: &str = "b2fcde5e6392e84703f453580974c16b5d9859937cb039dfa1797374a024d70e\
                         7b9c75a10f7346acc5f17cf9bc25ecae";
    const PK103_1: &str = "94a8d94abd58b634fc923fb5eb9a28d07c4931fc992dada2b9227bd0f5c4be2d\
                           a94f5ea7f42e5cdbf88a59a50dafaa4a";

    /// BL-851 T1 (LQ2 D27): each reject line's text against dashd v24.0.0's own output where a
    /// block reaches it, against the source (`commitment.cpp` v24:117-140,
    /// `blockprocessor.cpp` v24:273-413, :637) where none does.
    #[test]
    fn reject_lines_are_cores_text() {
        let line = |e: QcEvent<'_>| e.core_line();
        let all3 = || bitset(3, &[0, 1, 2]);
        let q216 = qc_of(LlmqType::LlmqTest, 3, Q216, None, all3(), all3(), PK100);
        let reject = |qc: &Commitment, reject| {
            line(QcEvent::VerifyReject {
                qc,
                reject,
            })
        };

        // `Verify` (commitment.cpp v24:117-194).
        let on = |q: &str, text: &str| format!("CFinalCommitment -- q[{q}] {text}");
        let unknown = qc_of(LlmqType::Unknown(99), 3, Q216, None, all3(), all3(), PK100);
        assert_eq!(reject(&unknown, CommitmentReject::LlmqType), on(Q216, "invalid llmqType=99"));
        let genesis = qc_of(LlmqType::LlmqTest, 3, GENESIS, None, all3(), all3(), PK100);
        assert_eq!(
            reject(&genesis, CommitmentReject::ParentlessBase),
            on(GENESIS, "parentless quorum base block")
        );
        let v1 = qc_of(LlmqType::LlmqTest, 1, Q216, None, all3(), all3(), PK100);
        let version = CommitmentReject::Version {
            found: 1,
            expected: 3,
        };
        assert_eq!(reject(&v1, version), on(Q216, "invalid nVersion=1 expected=3"));
        assert_eq!(reject(&q216, CommitmentReject::QuorumHash), on(Q216, "invalid quorumHash"));
        // A version without an index prints Core's default 0; a negative index its sign.
        let q215 = qc_of(LlmqType::LlmqTest, 3, Q215, None, all3(), all3(), PK100);
        assert_eq!(reject(&q215, CommitmentReject::QuorumIndex), on(Q215, "invalid quorumIndex=0"));
        let all4 = || bitset(4, &[0, 1, 2, 3]);
        let first = qc_of(LlmqType::LlmqTestDip0024, 4, Q216, Some(1), all4(), all4(), PK103_1);
        assert_eq!(
            reject(&first, CommitmentReject::QuorumIndex),
            on(Q216, "invalid quorumIndex=1")
        );
        let second = qc_of(LlmqType::LlmqTestDip0024, 4, Q217, Some(-1), all4(), all4(), PK103_1);
        assert_eq!(
            reject(&second, CommitmentReject::QuorumIndex),
            on(Q217, "invalid quorumIndex=-1")
        );
        let signers4 =
            qc_of(LlmqType::LlmqTest, 3, Q216, None, bitset(4, &[0, 1, 2]), all3(), PK100);
        assert_eq!(
            reject(&signers4, CommitmentReject::SignersSize),
            on(Q216, "invalid signers.size=4")
        );
        let valid4 = qc_of(LlmqType::LlmqTest, 3, Q216, None, all3(), bitset(4, &[0, 1, 2]), PK100);
        assert_eq!(
            reject(&valid4, CommitmentReject::ValidMembersSize),
            on(Q216, "invalid validMembers.size=4")
        );
        let valid1 = qc_of(LlmqType::LlmqTest, 3, Q216, None, all3(), bitset(3, &[0]), PK100);
        assert_eq!(
            reject(&valid1, CommitmentReject::ValidMembersCount),
            on(Q216, "invalid validMembers count. validMembersCount=1")
        );
        let signers1 = qc_of(LlmqType::LlmqTest, 3, Q216, None, bitset(3, &[0]), all3(), PK100);
        assert_eq!(
            reject(&signers1, CommitmentReject::SignersCount),
            on(Q216, "invalid signers count. signersCount=1")
        );
        for (r, text) in [
            (CommitmentReject::QuorumPublicKey, "invalid quorumPublicKey"),
            (CommitmentReject::VvecHash, "invalid quorumVvecHash"),
            (CommitmentReject::MembersSigObject, "invalid membersSig"),
            (CommitmentReject::QuorumSigObject, "invalid vvecSig"),
        ] {
            assert_eq!(reject(&q216, r), on(Q216, text));
        }
        let q24 = qc_of(LlmqType::LlmqTest, 3, Q24, None, all3(), all3(), PK100);
        assert_eq!(
            line(QcEvent::VerifyDump {
                qc: &q24,
                members: 0,
                size: 3,
            }),
            "CFinalCommitment::Verify mns[0] validMembers[v[0]=1v[1]=1v[2]=1] \
             signers[s[0]=1s[1]=1s[2]=1]"
        );
        assert_eq!(
            reject(&q24, CommitmentReject::ValidMemberBit(0)),
            on(Q24, "invalid validMembers bitset. bit 0 should not be set")
        );
        assert_eq!(
            reject(&q24, CommitmentReject::SignerBit(0)),
            on(Q24, "invalid signers bitset. bit 0 should not be set")
        );

        // `CheckLLMQCommitment` (commitment.cpp v24:244-302): the parent's height.
        let check = |height, reject| {
            line(QcEvent::CheckReject {
                height,
                reject,
            })
        };
        let q = |hex: &str| QuorumHash::from_hex(hex).expect("hex");
        assert_eq!(
            check(226, CheckReject::Payload),
            "CFinalCommitment -- h[226] GetTxPayload LLMQCommitment failed"
        );
        assert_eq!(
            check(226, CheckReject::Type(99)),
            "CFinalCommitment -- h[226] GetLLMQ failed for llmqType[99]"
        );
        assert_eq!(
            check(226, CheckReject::Version(2)),
            "CFinalCommitment -- h[226] invalid qcTx.nVersion[2]"
        );
        assert_eq!(
            check(226, CheckReject::Version(0)),
            "CFinalCommitment -- h[226] invalid qcTx.nVersion[0]"
        );
        assert_eq!(
            check(226, CheckReject::Height(229)),
            "CFinalCommitment -- h[226] invalid qcTx.nHeight[229]"
        );
        assert_eq!(
            check(226, CheckReject::Null(q(Q216))),
            format!("CFinalCommitment -- h[226] invalid qcTx.commitment[{Q216}] VerifyNull failed")
        );
        assert_eq!(
            check(33, CheckReject::Verify(q(Q24))),
            format!("CFinalCommitment -- h[33] invalid qcTx.commitment[{Q24}] Verify failed")
        );

        // `ProcessCommitment` (blockprocessor.cpp v24:333-413): the hash it compares.
        let process = |qc: &Commitment, height, quorum_hash, reject| {
            line(QcEvent::ProcessReject {
                qc,
                height,
                quorum_hash,
                scheme: BlsSchemeKind::Basic,
                reject,
            })
        };
        assert_eq!(
            process(&q216, 251, Some(q(Q240)), ProcessReject::NotEqual),
            format!(
                "ProcessCommitment -- height=251, type=100, quorumIndex=0, quorumHash={Q240}, \
                 qc.quorumHash={Q216} signers=3, validMembers=3, quorumPublicKey={PK100} non \
                 equal quorumHash."
            )
        );
        let old1 = qc_of(LlmqType::LlmqTestDip0024, 4, Q217, Some(1), all4(), all4(), PK103_1);
        assert_eq!(
            process(&old1, 252, Some(q(Q241)), ProcessReject::NotEqual),
            format!(
                "ProcessCommitment -- height=252, type=103, quorumIndex=1, quorumHash={Q241}, \
                 qc.quorumHash={Q217} signers=4, validMembers=4, quorumPublicKey={PK103_1} non \
                 equal quorumHash."
            )
        );
        let zeros = "0".repeat(64);
        assert_eq!(
            process(&q216, 251, None, ProcessReject::NullHash),
            format!(
                "ProcessCommitment -- height=251, type=100, quorumIndex=0, quorumHash={zeros}, \
                 signers=3, validMembers=3, quorumPublicKey={PK100} quorumHash is null."
            )
        );
        // No space before `qc`, and no key (v24:371-375).
        assert_eq!(
            process(&q216, 251, Some(q(Q216)), ProcessReject::VerifyNull),
            format!(
                "ProcessCommitment -- height=251, type=100, quorumIndex=0, quorumHash={Q216}, \
                 signers=3, validMembers=3qc verifynull failed."
            )
        );
        assert_eq!(
            process(&old1, 252, Some(q(Q217)), ProcessReject::Verify),
            format!(
                "ProcessCommitment -- height=252, type=103, quorumIndex=1, quorumHash={Q217}, \
                 signers=4, validMembers=4, quorumPublicKey={PK103_1} qc verify failed."
            )
        );
        assert_eq!(
            line(QcEvent::UnknownBase {
                qc: &q216,
                height: None,
            }),
            format!(
                "ProcessCommitment -- unexpectedly failed due to no known pindex for \
                 hash[{Q216}]"
            )
        );
        assert_eq!(
            line(QcEvent::UnknownBase {
                qc: &q216,
                height: Some(251),
            }),
            format!(
                "[ProcessBlock] h[251] unexpectedly failed due to no known pindex for \
                 hash[{Q216}]"
            )
        );
        assert_eq!(
            line(QcEvent::InvalidType {
                qc: &unknown,
            }),
            "ProcessCommitment -- invalid commitment type 99"
        );

        // `ProcessBlock`'s `failed`: the commitment's own hash, version and index (v24:287).
        let failed = |qc: &Commitment, height| {
            line(QcEvent::Failed {
                qc,
                height,
            })
        };
        assert_eq!(
            failed(&q216, 251),
            format!(
                "[ProcessBlock] failed h[251] llmqType[100] version[3] quorumIndex[0] \
                 quorumHash[{Q216}]"
            )
        );
        let old0 = qc_of(LlmqType::LlmqTestDip0024, 4, Q216, Some(0), all4(), all4(), PK103_1);
        assert_eq!(
            failed(&old0, 252),
            format!(
                "[ProcessBlock] failed h[252] llmqType[103] version[4] quorumIndex[0] \
                 quorumHash[{Q216}]"
            )
        );
        assert_eq!(
            failed(&old1, 252),
            format!(
                "[ProcessBlock] failed h[252] llmqType[103] version[4] quorumIndex[1] \
                 quorumHash[{Q217}]"
            )
        );

        // `GetQuorumBlockHash` (v24:637).
        assert_eq!(
            line(QcEvent::QuorumBlockHashEmpty {
                llmq_type: 103,
                height: 60,
                quorum_index: 1,
                start: 49,
            }),
            "[GetQuorumBlockHash] llmqType[103] h[60] qi[1] quorumStartHeight[49] \
             quorumHash[EMPTY]"
        );
    }
}

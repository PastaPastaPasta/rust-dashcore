//! Core's `NetDKG` in observer mode (`m_active == nullptr`): a node with no DKG worker scores
//! `qwatch`, logs and drops the four pushed DKG messages, and reports the four DKG inventory
//! types as already had so it never requests them.

use crate::dkg::wire::InvType;

/// Core's `NetMsgType::QCONTRIB`.
pub const QCONTRIB: &str = "qcontrib";
/// Core's `NetMsgType::QCOMPLAINT`.
pub const QCOMPLAINT: &str = "qcomplaint";
/// Core's `NetMsgType::QJUSTIFICATION`.
pub const QJUSTIFICATION: &str = "qjustify";
/// Core's `NetMsgType::QPCOMMITMENT`.
pub const QPCOMMITMENT: &str = "qpcommit";
/// Core's `NetMsgType::QWATCH`.
pub const QWATCH: &str = "qwatch";

/// The misbehavior score an observer gives a peer that sends `qwatch`.
pub const QWATCH_SCORE: i32 = 10;

/// What the observer's `NetDKG::ProcessMessage` asks the runtime to do. Both None means the
/// message is dropped silently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObserverOutcome {
    /// Add the score to the sending peer's misbehavior with the reason. Core's `qwatch` call is
    /// `PeerMisbehaving(id, 10)` with the default empty message, so the reason is `""`.
    pub score: Option<(i32, &'static str)>,
    /// A `LogPrint(BCLog::LLMQ_DKG, …)` line, without its trailing newline.
    pub log: Option<String>,
}

/// Handles a message `cmd` on an observer, with spork 17 (`SPORK_17_QUORUM_DKG_ENABLED`) on or
/// off.
// Core: v24.0.0 src/llmq/net_dkg.cpp:505-531 NetDKG::ProcessMessage
pub fn observer_dkg(spork17_on: bool, cmd: &str) -> ObserverOutcome {
    if !spork17_on {
        return ObserverOutcome::default();
    }
    match cmd {
        QWATCH => ObserverOutcome {
            score: Some((QWATCH_SCORE, "")),
            log: None,
        },
        QCONTRIB | QCOMPLAINT | QJUSTIFICATION | QPCOMMITMENT => ObserverOutcome {
            score: None,
            log: Some(format!("NetDKG -- ignoring {cmd} in observer mode")),
        },
        _ => ObserverOutcome::default(),
    }
}

/// Whether an observer already has an inventory item of type `inv`, so does not request it.
/// The four DKG types are true before any spork check; every other type is false.
// Core: v24.0.0 src/llmq/net_dkg.cpp:681-704 NetDKG::AlreadyHave
pub fn observer_already_have(inv: InvType) -> bool {
    matches!(
        inv,
        InvType::QuorumContrib
            | InvType::QuorumComplaint
            | InvType::QuorumJustification
            | InvType::QuorumPrematureCommitment
    )
}

#[cfg(test)]
mod tests {
    use dash_types::Numeric;

    use super::*;

    const PUSHES: [&str; 4] = [QCONTRIB, QCOMPLAINT, QJUSTIFICATION, QPCOMMITMENT];
    const UNRELATED: [&str; 7] =
        ["qgetdata", "qfcommit", "inv", "QWATCH", "qcontribx", "qcontri", ""];

    fn nothing() -> ObserverOutcome {
        ObserverOutcome {
            score: None,
            log: None,
        }
    }

    #[test]
    fn observer_dkg_table() {
        let mut rows: Vec<(bool, &str, ObserverOutcome)> = Vec::new();
        for cmd in PUSHES.iter().chain([QWATCH].iter()).chain(UNRELATED.iter()) {
            rows.push((false, cmd, nothing()));
        }
        rows.push((
            true,
            QWATCH,
            ObserverOutcome {
                score: Some((10, "")),
                log: None,
            },
        ));
        for (cmd, line) in PUSHES.iter().zip([
            "NetDKG -- ignoring qcontrib in observer mode",
            "NetDKG -- ignoring qcomplaint in observer mode",
            "NetDKG -- ignoring qjustify in observer mode",
            "NetDKG -- ignoring qpcommit in observer mode",
        ]) {
            rows.push((
                true,
                cmd,
                ObserverOutcome {
                    score: None,
                    log: Some(line.to_owned()),
                },
            ));
        }
        for cmd in UNRELATED {
            rows.push((true, cmd, nothing()));
        }
        for (spork, cmd, want) in rows {
            assert_eq!(observer_dkg(spork, cmd), want, "spork17_on={spork} cmd={cmd}");
        }
    }

    #[test]
    fn observer_already_have_table() {
        let rows = [
            (InvType::QuorumContrib, true),
            (InvType::QuorumComplaint, true),
            (InvType::QuorumJustification, true),
            (InvType::QuorumPrematureCommitment, true),
            (InvType::QuorumFinalCommitment, false),
            (InvType::QuorumRecoveredSig, false),
            (InvType::Tx, false),
            (InvType::Block, false),
        ];
        for (inv, want) in rows {
            assert_eq!(observer_already_have(inv), want, "{inv:?}");
        }
    }

    #[test]
    fn observer_already_have_sweeps_every_wire_value() {
        let had: Vec<u32> =
            (0..64u32).filter(|t| observer_already_have(InvType::from_base(*t))).collect();
        assert_eq!(had, [23, 24, 25, 26]);
    }
}

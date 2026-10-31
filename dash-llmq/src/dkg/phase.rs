//! The DKG's phase and sleep arithmetic (LQ4 §2.1): which phase a tip starts, and how long a
//! member waits before a phase's start function. DK1 owns the clock and the round driver.

use dash_params::LlmqParams;

/// `QuorumPhase` (`l/dkgsessionhandler.h:33-41`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuorumPhase {
    /// A new session is initialized.
    Initialized = 1,
    /// Contributions are sent and verified.
    Contribute,
    /// Complaints are sent.
    Complain,
    /// Justifications are sent.
    Justify,
    /// Premature commitments are sent.
    Commit,
    /// Final commitments are built.
    Finalize,
    /// Nothing happens until the next interval; the handler starts here.
    Idle,
}

impl QuorumPhase {
    /// The phase with Core's number, `Initialized` (1) to `Idle` (7).
    pub const fn from_int(n: i32) -> Option<Self> {
        Some(match n {
            1 => Self::Initialized,
            2 => Self::Contribute,
            3 => Self::Complain,
            4 => Self::Justify,
            5 => Self::Commit,
            6 => Self::Finalize,
            7 => Self::Idle,
            _ => return None,
        })
    }
}

/// What a tip sets in a handler: the base of the current interval and the phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseTick {
    /// `pQuorumBaseBlockIndex->nHeight`: the quorum hash is that ancestor's hash.
    pub base_height: i32,
    /// The phase after the tip: a new one at a phase boundary, else `current`.
    pub phase: QuorumPhase,
}

/// `ActiveDKGSessionHandler::UpdatedBlockTip` (`a/dkgsessionhandler.cpp:43-70`) for the handler
/// of `quorum_index`, given its `current` phase. `None` where Core returns without a change:
/// a snapshot active and unvalidated, or an index above 0 while rotation is off at the tip (and,
/// as no Core parameters do, a zero interval or phase length). DK1 calls it on each tip that
/// passes `CDKGSessionManager::UpdatedBlockTip`'s gates (`l/dkgsessionmgr.cpp:41-55`).
pub fn phase_for_tip(
    params: &LlmqParams,
    quorum_index: i32,
    tip_height: i32,
    snapshot_unvalidated: bool,
    rotation_enabled_at_tip: bool,
    current: QuorumPhase,
) -> Option<PhaseTick> {
    if snapshot_unvalidated || (quorum_index > 0 && !rotation_enabled_at_tip) {
        return None;
    }
    // C++ `%` and `/` on int: truncation toward zero, as Rust's.
    let stage = (tip_height - quorum_index).checked_rem(params.dkg_interval)?;
    let new_phase = stage.checked_rem(params.dkg_phase_blocks)? == 0;
    let phase_int = stage.checked_div(params.dkg_phase_blocks)? + 1;
    let phase = match QuorumPhase::from_int(phase_int) {
        Some(phase) if new_phase => phase,
        _ => current,
    };
    Some(PhaseTick {
        base_height: tip_height - stage,
        phase,
    })
}

/// `SleepBeforePhase`'s wait in milliseconds (`a/dkgsessionhandler.cpp:173-200`): none for a
/// non-member or where blocks are mined on demand, else
/// `(pb − 1) · nPowTargetSpacing · 1000 / size · factor · myIdx`, in Core's double arithmetic,
/// truncated. The round driver passes `factor` 0.05 for Contribute, Complain and Justify and
/// 0.1 for Commit (`l/net_dkg.cpp:833-936`). The wait ends early when blocks come faster; that
/// is DK1's.
#[allow(
    clippy::float_arithmetic,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "Core's double arithmetic and its static_cast<int64_t>, bit-exact under IEEE"
)]
pub fn sleep_before_phase_ms(
    params: &LlmqParams,
    pow_target_spacing_s: i64,
    my_idx: Option<usize>,
    factor: f64,
    mine_on_demand: bool,
) -> i64 {
    let Some(my_idx) = my_idx else {
        return 0;
    };
    if mine_on_demand {
        return 0;
    }
    let phase_sleep = (i64::from(params.dkg_phase_blocks) - 1) * pow_target_spacing_s * 1000;
    let per_member = phase_sleep as f64 / f64::from(params.size);
    let adjusted = per_member * factor;
    (adjusted * my_idx as f64) as i64
}

#[cfg(test)]
mod tests;

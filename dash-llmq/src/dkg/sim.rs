//! Core's simulated DKG errors (`DKGError`, `ShouldSimulateError`; LQ4 §2.9): the
//! `quorumdkgsimerror` rates and the gate each decision passes.

use std::fmt;

use super::rng::DkgRng;
use super::wire::LlmqType;

/// `DKGError::type` (`l/dkgsession.h:62-84`), in Core's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimError {
    /// `complain-lie`: complain about a contribution that decrypted.
    ComplainLie,
    /// `commit-omit`: send no premature commitment.
    CommitOmit,
    /// `commit-lie`: send a wrong premature commitment.
    CommitLie,
    /// `contribution-omit`: send no contribution.
    ContributionOmit,
    /// `contribution-lie`: encrypt a fresh key instead of a member's share.
    ContributionLie,
    /// `justify-omit`: send no justification.
    JustifyOmit,
    /// `justify-lie`: reveal a fresh key instead of a member's share.
    JustifyLie,
}

impl SimError {
    /// Every type, in Core's order.
    pub const ALL: [Self; 7] = [
        Self::ComplainLie,
        Self::CommitOmit,
        Self::CommitLie,
        Self::ContributionOmit,
        Self::ContributionLie,
        Self::JustifyOmit,
        Self::JustifyLie,
    ];

    /// `DKGError::from_string`: the RPC's name for the type.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The RPC's name for the type.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ComplainLie => "complain-lie",
            Self::CommitOmit => "commit-omit",
            Self::CommitLie => "commit-lie",
            Self::ContributionOmit => "contribution-omit",
            Self::ContributionLie => "contribution-lie",
            Self::JustifyOmit => "justify-omit",
            Self::JustifyLie => "justify-lie",
        }
    }
}

/// A rate the RPC refuses: "invalid rate. Must be between 0 and 100" (`rpc/quorums.cpp:886-888`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRate(pub i32);

impl fmt::Display for InvalidRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid rate. Must be between 0 and 100")
    }
}

/// The simulated error rates, as `quorumdkgsimerror` sets them: an integer percent per type.
/// Core stores `rate / 100` as a double (`rpc/quorums.cpp:894`); [`get_rand_bool`] makes the
/// decision Core makes from that double (LQ4a-1 D-3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimErrorRates {
    percent: [u8; 7],
}

impl SimErrorRates {
    /// `SetSimulatedDKGErrorRate(type, rate / 100)` for an RPC rate.
    ///
    /// # Errors
    ///
    /// [`InvalidRate`] outside `0..=100`, as the RPC refuses it.
    pub fn set(&mut self, kind: SimError, rate: i32) -> Result<(), InvalidRate> {
        let percent = u8::try_from(rate).ok().filter(|p| *p <= 100).ok_or(InvalidRate(rate))?;
        self.percent[kind as usize] = percent;
        Ok(())
    }

    /// The rate of `kind`, in percent.
    pub const fn percent(&self, kind: SimError) -> u8 {
        self.percent[kind as usize]
    }
}

/// The range of the draw [`get_rand_bool`] compares: `GetRand(v + 1)` with `v = 10^8`.
pub const RAND_BOOL_RANGE: u64 = 100_000_001;

/// `GetRandBool(percent / 100.0)`'s decision for the draw `r` it made (`random.cpp:653-661`):
/// `r <= v * rate`. A rate of 0 is false; Core makes no draw for it.
///
/// The product is Core's double: `percent / 100.0` is not exact, and `1e8 · rate` falls just
/// below `percent · 10^6` for 29, 57 and 58 (so the draw `percent · 10^6` is false there). One
/// IEEE division and one multiplication, no libm, so every target rounds them as Core's
/// x86_64 build does.
#[allow(
    clippy::float_arithmetic,
    clippy::cast_precision_loss,
    reason = "Core's double comparison, bit-exact under IEEE (CS2 §2.2); r <= 10^8 is exact"
)]
pub fn get_rand_bool(r: u64, percent: u8) -> bool {
    if percent == 0 {
        return false;
    }
    let rate = f64::from(percent) / 100.0;
    (r as f64) <= 100_000_000.0 * rate
}

/// `CDKGSession::ShouldSimulateError(type)` (`l/dkgsession.cpp:56-63`): only `LLMQ_TEST`
/// simulates, with no draw for any other type, then `GetRandBool(rate)`, which makes no draw
/// at rate 0.
pub fn should_simulate(
    llmq_type: LlmqType,
    kind: SimError,
    rates: &SimErrorRates,
    rng: &mut dyn DkgRng,
) -> bool {
    if llmq_type != LlmqType::LlmqTest {
        return false;
    }
    match rates.percent(kind) {
        0 => false,
        percent => rng.rand_bool(percent, kind),
    }
}

#[cfg(test)]
mod tests;

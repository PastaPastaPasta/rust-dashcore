//! Core's randomness points on the DKG path (LQ4 §2.9, m7): every draw a session makes goes
//! through [`DkgRng`], tagged with why it is made, so a replay can tell a divergent draw order
//! at the first wrong draw.

use rand_core::CryptoRng;

use super::bls::{BlsSkValue, IV_SEED_LEN, IesSeed, make_new_key};
use super::sim::{RAND_BOOL_RANGE, SimError, get_rand_bool};

/// Why a session draws a secret key (`CBLSSecretKey::MakeNewKey`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDraw {
    /// Coefficient `j` of the session's polynomial (`GenerateContributions`,
    /// `bls/bls_worker.cpp:86`).
    Coefficient(u32),
    /// The fresh key `CONTRIBUTION_LIE` encrypts for member `i` (`a/dkgsession.cpp:88-93`).
    ContributionLie(u32),
    /// The fresh key `JUSTIFY_LIE` reveals for member `i` (`a/dkgsession.cpp:399-402`).
    JustifyLie(u32),
    /// The fresh key of `COMMIT_LIE`'s lie type 0 (`a/dkgsession.cpp:531-562`).
    CommitLieKey,
    /// `FinalizeSingleCommitment`'s `sk1` (`a/dkgsession.cpp:704-705`).
    SingleMemberSk1,
}

/// Why a session draws from a range (`GetRand<int>(n)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeDraw {
    /// `COMMIT_LIE`'s lie type, `GetRand<int>(5)` (`a/dkgsession.cpp:531`).
    CommitLieType,
}

/// Core's four randomness points on the DKG path. The production implementation is
/// [`CoreRng`]; a replay returns recorded decisions.
pub trait DkgRng {
    /// `CBLSSecretKey::MakeNewKey`: a valid key.
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue;
    /// `InitEncrypt`'s ephemeral key and IV seed, for `recipients` blobs
    /// (`bls/bls_ies.cpp:59-71`).
    fn ies_seed(&mut self, recipients: usize) -> IesSeed;
    /// `GetRandBool(percent / 100.0)` at a rate of 1 to 100 percent; a rate of 0 draws nothing
    /// and never reaches the RNG ([`super::sim::should_simulate`]).
    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool;
    /// `GetRand<int>(n)`: a value in `[0, n)`.
    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64;
}

/// The production [`DkgRng`]: Core's draws over a cryptographic RNG.
#[derive(Debug)]
pub struct CoreRng<R>(pub R);

impl<R: CryptoRng> CoreRng<R> {
    /// A uniform value below `n` (`n > 0`), by rejection, as `FastRandomContext::randrange`.
    fn below(&mut self, n: u64) -> u64 {
        let zone = u64::MAX - u64::MAX % n;
        loop {
            let v = self.0.next_u64();
            if v < zone {
                return v % n;
            }
        }
    }
}

impl<R: CryptoRng> DkgRng for CoreRng<R> {
    fn new_key(&mut self, _why: KeyDraw) -> BlsSkValue {
        make_new_key(&mut self.0)
    }

    fn ies_seed(&mut self, _recipients: usize) -> IesSeed {
        let ephemeral_sk = make_new_key(&mut self.0);
        let mut iv_seed = [0u8; IV_SEED_LEN];
        self.0.fill_bytes(&mut iv_seed);
        IesSeed {
            ephemeral_sk,
            iv_seed,
        }
    }

    fn rand_bool(&mut self, percent: u8, _why: SimError) -> bool {
        let r = self.below(RAND_BOOL_RANGE);
        get_rand_bool(r, percent)
    }

    fn rand_range(&mut self, n: u64, _why: RangeDraw) -> u64 {
        self.below(n)
    }
}

/// One recorded draw, with its tag.
#[cfg(any(test, feature = "reference"))]
#[derive(Clone, Debug)]
pub enum Draw {
    /// A [`DkgRng::new_key`] result.
    Key(KeyDraw, BlsSkValue),
    /// A [`DkgRng::ies_seed`] result, with the recipient count it was drawn for.
    Ies(usize, IesSeed),
    /// A [`DkgRng::rand_bool`] decision, with the rate in percent it was drawn at.
    Bool(SimError, u8, bool),
    /// A [`DkgRng::rand_range`] result.
    Range(RangeDraw, u64),
}

/// The first draw a [`ReplayRng`] could not answer.
#[cfg(any(test, feature = "reference"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayMismatch {
    /// The index of the draw in the session's stream.
    pub at: usize,
    /// What the session asked for.
    pub asked: String,
    /// What the recording held there, or `None` past its end.
    pub recorded: Option<String>,
}

/// A [`DkgRng`] that returns recorded draws (LQ4 §3.2). A draw whose tag differs from the
/// recording's, or one past its end, is a mismatch: [`ReplayRng::finish`] reports the first,
/// and every later draw is answered with a placeholder so the session runs on.
#[cfg(any(test, feature = "reference"))]
#[derive(Debug)]
pub struct ReplayRng {
    draws: std::collections::VecDeque<Draw>,
    taken: usize,
    mismatch: Option<ReplayMismatch>,
}

#[cfg(any(test, feature = "reference"))]
impl ReplayRng {
    /// A replay of `draws`, in order.
    pub fn new(draws: impl IntoIterator<Item = Draw>) -> Self {
        Self {
            draws: draws.into_iter().collect(),
            taken: 0,
            mismatch: None,
        }
    }

    /// `Ok` when every draw matched and the recording is used up; otherwise the first
    /// mismatch, or the first unused draw.
    ///
    /// # Errors
    ///
    /// The first [`ReplayMismatch`].
    pub fn finish(self) -> Result<(), ReplayMismatch> {
        if let Some(mismatch) = self.mismatch {
            return Err(mismatch);
        }
        match self.draws.front() {
            None => Ok(()),
            Some(left) => Err(ReplayMismatch {
                at: self.taken,
                asked: "nothing".into(),
                recorded: Some(format!("{left:?}")),
            }),
        }
    }

    fn take<T>(&mut self, asked: String, pick: impl FnOnce(&Draw) -> Option<T>) -> Option<T> {
        let at = self.taken;
        self.taken += 1;
        if self.mismatch.is_some() {
            return None;
        }
        let picked = self.draws.front().and_then(pick);
        if picked.is_some() {
            self.draws.pop_front();
        } else {
            self.mismatch = Some(ReplayMismatch {
                at,
                asked,
                recorded: self.draws.front().map(|d| format!("{d:?}")),
            });
        }
        picked
    }
}

#[cfg(any(test, feature = "reference"))]
impl DkgRng for ReplayRng {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        self.take(format!("Key({why:?})"), |d| match d {
            Draw::Key(tag, key) if *tag == why => Some(key.clone()),
            _ => None,
        })
        .unwrap_or_else(BlsSkValue::invalid)
    }

    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        self.take(format!("Ies({recipients})"), |d| match d {
            Draw::Ies(n, seed) if *n == recipients => Some(seed.clone()),
            _ => None,
        })
        .unwrap_or_else(|| IesSeed {
            ephemeral_sk: BlsSkValue::invalid(),
            iv_seed: [0; IV_SEED_LEN],
        })
    }

    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool {
        self.take(format!("Bool({why:?}, {percent}%)"), |d| match d {
            Draw::Bool(tag, rate, value) if *tag == why && *rate == percent => Some(*value),
            _ => None,
        })
        .unwrap_or(false)
    }

    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64 {
        self.take(format!("Range({why:?}, {n})"), |d| match d {
            Draw::Range(tag, value) if *tag == why && *value < n => Some(*value),
            _ => None,
        })
        .unwrap_or(0)
    }
}

/// A deterministic, seeded RNG (SplitMix64) for tests, harnesses and benches. It is not
/// cryptographically secure.
#[cfg(any(test, feature = "reference"))]
#[derive(Clone, Debug)]
pub struct SeededRng(u64);

#[cfg(any(test, feature = "reference"))]
impl SeededRng {
    /// An RNG whose stream is fixed by `seed`.
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }
}

#[cfg(any(test, feature = "reference"))]
impl rand_core::TryRng for SeededRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok((self.try_next_u64()? >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Ok(z ^ (z >> 31))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        for chunk in dst.chunks_mut(8) {
            let word = self.try_next_u64()?.to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "reference"))]
impl rand_core::TryCryptoRng for SeededRng {}

#[cfg(test)]
mod tests;

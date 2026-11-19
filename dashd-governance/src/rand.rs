//! Governance randomness: the vote-sync bloom tweak, the orphan-parent request sample and the
//! object-request shuffles, from Core's draws in production and from a recording in replay.

use std::collections::VecDeque;
use std::sync::Arc;

use dash_p2p::rng::{FastRandom, RandSource, get_rand, process_source};

pub trait GovRng {
    /// The vote bloom filter's tweak.
    // Core: governance/governance.cpp:947-964
    fn bloom_tweak(&mut self) -> u32;

    /// Keeps a random `k` of `v` when it holds more.
    // Core: governance/governance.cpp:1171-1175
    fn sample(&mut self, v: &mut Vec<[u8; 32]>, k: usize);

    // Core: node/sync_manager.cpp:94-95
    fn shuffle(&mut self, v: &mut [[u8; 32]]);
}

/// Core's draws: each site takes a fresh `FastRandomContext` over the process source, as
/// `GetRand` and `Shuffle(…, FastRandomContext())` do.
pub struct CoreRng(Arc<dyn RandSource>);

impl CoreRng {
    pub fn new(source: Arc<dyn RandSource>) -> Self {
        Self(source)
    }
}

impl Default for CoreRng {
    fn default() -> Self {
        Self::new(process_source())
    }
}

impl GovRng for CoreRng {
    fn bloom_tweak(&mut self) -> u32 {
        // `GetRand<int>(999999)`, below `i32::MAX`.
        get_rand(&self.0, 999_999) as u32
    }

    fn sample(&mut self, v: &mut Vec<[u8; 32]>, k: usize) {
        if v.len() > k {
            FastRandom::lazy(Arc::clone(&self.0)).shuffle(v);
            v.truncate(k);
        }
    }

    fn shuffle(&mut self, v: &mut [[u8; 32]]) {
        FastRandom::lazy(Arc::clone(&self.0)).shuffle(v);
    }
}

/// One recorded draw: the value it gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Draw {
    BloomTweak(u32),
    Sample(Vec<[u8; 32]>),
    Shuffle(Vec<[u8; 32]>),
}

/// Answers each draw from a recording, in order. Panics when the recording runs out, when the
/// next draw is of another kind, or when its value cannot come from the input.
#[derive(Debug, Default)]
pub struct ReplayRng(VecDeque<Draw>);

impl ReplayRng {
    pub fn new(draws: impl IntoIterator<Item = Draw>) -> Self {
        Self(draws.into_iter().collect())
    }

    /// Draws not taken yet.
    pub fn remaining(&self) -> usize {
        self.0.len()
    }

    fn next(&mut self, kind: &str) -> Draw {
        self.0.pop_front().unwrap_or_else(|| panic!("replay recording exhausted at a {kind} draw"))
    }
}

fn sorted(v: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut v = v.to_vec();
    v.sort_unstable();
    v
}

impl GovRng for ReplayRng {
    fn bloom_tweak(&mut self) -> u32 {
        match self.next("bloom") {
            Draw::BloomTweak(tweak) => tweak,
            other => panic!("replay mismatch: bloom draw, recorded {other:?}"),
        }
    }

    /// Draws only when `v` holds more than `k`, as Core shuffles only then.
    fn sample(&mut self, v: &mut Vec<[u8; 32]>, k: usize) {
        if v.len() <= k {
            return;
        }
        let Draw::Sample(recorded) = self.next("sample") else {
            panic!("replay mismatch: sample draw, recorded another kind");
        };
        let mut pool = sorted(v);
        let fits = recorded.len() == k
            && recorded.iter().all(|h| pool.binary_search(h).map(|i| pool.remove(i)).is_ok());
        assert!(fits, "replay mismatch: recorded sample is not {k} of the input");
        *v = recorded;
    }

    fn shuffle(&mut self, v: &mut [[u8; 32]]) {
        let Draw::Shuffle(recorded) = self.next("shuffle") else {
            panic!("replay mismatch: shuffle draw, recorded another kind");
        };
        assert!(
            sorted(&recorded) == sorted(v),
            "replay mismatch: recorded order is not the input's"
        );
        v.copy_from_slice(&recorded);
    }
}

//! Deciders: depth-first enumeration of every decision sequence (the exhaustive search of §4.5),
//! fixed decisions (repros, §4.8) and seeded random ones (sweeps, §4.6).
//!
//! The depth-first search is stateless: every branch reruns the system under test from the
//! booted image, replaying the decisions of the branch prefix and choosing 0 for every new
//! variable. Determinism (§4.9) makes the k-th decision of a branch the same variable as in the
//! previous branch; a mismatch is a model self-check failure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::faulty::world::{Decider, Var};

#[derive(Debug, Default)]
struct DfsState {
    /// The choices to replay at the start of the branch.
    prefix: Vec<u32>,
    /// The decisions of the current branch, in order.
    taken: Vec<(Var, u32)>,
    /// The variable each replayed position decided in the previous branch.
    expected: Vec<String>,
    /// A determinism violation (a different variable at a replayed position).
    mismatch: Option<String>,
    /// The positions `advance` may change: from `lo` (a pinned prefix stays fixed) up to `hi`
    /// (later positions stay 0), `None` for no upper limit.
    lo: usize,
    hi: Option<usize>,
}

/// Depth-first enumeration of decision sequences. Clone it to share the state with a booted
/// world's decider.
#[derive(Clone, Default)]
pub struct Dfs(Arc<Mutex<DfsState>>);

impl Dfs {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DfsState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A decider for one branch.
    pub fn decider(&self) -> Box<dyn Decider> {
        self.lock().taken.clear();
        Box::new(self.clone())
    }

    /// The decisions of the branch just run.
    pub fn decisions(&self) -> Vec<(Var, u32)> {
        self.lock().taken.clone()
    }

    /// A determinism violation seen in this branch, if any.
    pub fn mismatch(&self) -> Option<String> {
        self.lock().mismatch.clone()
    }

    /// Moves to the next branch; `false` when every branch has been run.
    pub fn advance(&self) -> bool {
        let mut s = self.lock();
        let hi = s.hi.unwrap_or(usize::MAX).min(s.taken.len());
        let lo = s.lo.min(hi);
        let Some(i) = s.taken[lo..hi].iter().rposition(|(v, c)| c + 1 < v.domain).map(|i| i + lo)
        else {
            return false;
        };
        let mut prefix: Vec<u32> = s.taken[..i].iter().map(|(_, c)| *c).collect();
        prefix.push(s.taken[i].1 + 1);
        s.expected = s.taken[..=i].iter().map(|(v, _)| v.name.clone()).collect();
        s.prefix = prefix;
        true
    }

    /// Fixes the first decisions of every later branch (nested witnesses fix the outer
    /// decisions, §4.5).
    pub fn with_prefix(prefix: Vec<u32>) -> Self {
        let d = Self::default();
        d.lock().prefix = prefix;
        d
    }

    /// Every branch whose first decisions are `prefix`: the prefix is replayed and never
    /// advanced (one shard of a sharded search, DEC-185 (2)).
    pub fn pinned(prefix: Vec<u32>) -> Self {
        let d = Self::default();
        {
            let mut s = d.lock();
            s.lo = prefix.len();
            s.prefix = prefix;
        }
        d
    }

    /// One branch per distinct choice of the first `depth` decisions, later ones left at 0 (the
    /// shards' prefixes).
    pub fn limited(depth: usize) -> Self {
        let d = Self::default();
        d.lock().hi = Some(depth);
        d
    }
}

impl Decider for Dfs {
    fn choose(&mut self, var: &Var) -> u32 {
        let mut s = self.lock();
        let pos = s.taken.len();
        let c = match s.prefix.get(pos) {
            Some(&c) => {
                if let Some(name) = s.expected.get(pos)
                    && *name != var.name
                    && s.mismatch.is_none()
                {
                    s.mismatch = Some(format!(
                        "decision {pos} was `{name}` and is now `{}`: the run is not deterministic",
                        var.name
                    ));
                }
                c.min(var.domain - 1)
            }
            None => 0,
        };
        s.taken.push((var.clone(), c));
        c
    }
}

/// Decisions by variable name; every other variable is 0. Records what it decided.
#[derive(Clone, Default)]
pub struct Fixed {
    pub choices: BTreeMap<String, u32>,
    taken: Arc<Mutex<Vec<(Var, u32)>>>,
}

impl Fixed {
    pub fn new(choices: BTreeMap<String, u32>) -> Self {
        Self {
            choices,
            taken: Arc::default(),
        }
    }

    pub fn decisions(&self) -> Vec<(Var, u32)> {
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Decider for Fixed {
    fn choose(&mut self, var: &Var) -> u32 {
        let c = self.choices.get(&var.name).copied().unwrap_or(0).min(var.domain - 1);
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).push((var.clone(), c));
        c
    }
}

/// A deterministic pseudo-random generator (xorshift64*), the only randomness the harness uses.
#[derive(Debug, Clone)]
pub struct Seeded(u64);

impl Seeded {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// True with probability `per_mille / 1000`.
    pub fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

/// Random decisions from a seed. Records what it decided.
#[derive(Clone)]
pub struct SeededDecider {
    rng: Arc<Mutex<Seeded>>,
    taken: Arc<Mutex<Vec<(Var, u32)>>>,
}

impl SeededDecider {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Arc::new(Mutex::new(Seeded::new(seed))),
            taken: Arc::default(),
        }
    }

    pub fn decisions(&self) -> Vec<(Var, u32)> {
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Decider for SeededDecider {
    fn choose(&mut self, var: &Var) -> u32 {
        let c =
            self.rng.lock().unwrap_or_else(|e| e.into_inner()).below(u64::from(var.domain)) as u32;
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).push((var.clone(), c));
        c
    }
}

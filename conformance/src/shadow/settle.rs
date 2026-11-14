use dash_statedigest::compare::Slot;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLock {
    pub h: i64,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settle {
    Cl {
        h: i64,
        hash: String,
    },
    Depth {
        h: i64,
    },
}

impl Settle {
    pub fn height(&self) -> i64 {
        match self {
            Self::Cl {
                h,
                ..
            }
            | Self::Depth {
                h,
            } => *h,
        }
    }
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Cl {
                ..
            } => "cl",
            Self::Depth {
                ..
            } => "depth",
        }
    }
}

pub fn choose(cl: Option<&ChainLock>, age: u64, left: &[Slot], right: &[Slot]) -> Option<Settle> {
    if let Some(cl) = cl.filter(|_| age <= 30 * 60) {
        let i = left.binary_search_by_key(&cl.h, |s| s.h).ok()?;
        return (left[i].b.to_hex() == cl.hash).then(|| Settle::Cl {
            h: cl.h,
            hash: cl.hash.clone(),
        });
    }
    let h = left.last()?.h.min(right.last()?.h) - 6;
    (h >= 0).then_some(Settle::Depth {
        h,
    })
}

//! Compile-only consumers (PLAN EV0 acceptance): the shapes EV1, EV2a, EV2b, LQ1a and CH7a will
//! write against EV0's API, each with a small runnable check. They are not ports; the owners'
//! tasks port Core's logic. `API.md` maps every requirement to the item these use.

#![allow(dead_code, reason = "compile-only consumers: the signatures are the point, not every use")]

#[path = "consumers/ch7a.rs"]
mod ch7a;
#[path = "consumers/ev1.rs"]
mod ev1;
#[path = "consumers/ev2a.rs"]
mod ev2a;
#[path = "consumers/ev2b.rs"]
mod ev2b;
#[path = "consumers/lq1a.rs"]
mod lq1a;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::EvoDelta;
use dash_dml::testkit::MemChain;

/// The delta of a block that wrote nothing but moved the evo tip.
pub fn empty_delta(chain: &MemChain, block: BlockId) -> EvoDelta {
    EvoDelta {
        block: chain.hash(block),
        height: chain.header(block).height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

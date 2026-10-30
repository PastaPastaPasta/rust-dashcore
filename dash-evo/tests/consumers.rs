//! Compile-only consumers (PLAN EV0 acceptance): the shapes the shared crate's users write against
//! its API, each with a small runnable check. They are not ports. The node's consumers (EV1,
//! EV2a/b, LQ1a, CH7a) are `dash-dml`'s; this crate's are the ones that never name `dash-dml`.

#![allow(dead_code, reason = "compile-only consumers: the signatures are the point, not every use")]

#[path = "consumers/spv.rs"]
mod spv;

//! Segment replays (node program O9b; R4 §2.3.4, A1 §4.6): [`bundle`] the snapshots a window
//! starts from, checked against the golden; [`select`] which windows a change runs. The window
//! itself is `digest l2 --start-utxo` over a bundle (`conformance/scripts/segment-run.sh`).

pub mod bundle;
pub mod select;

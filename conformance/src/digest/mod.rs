//! StateDigest v1 tooling (node program tasks O8 and O9a). `digest extract` is the golden
//! extractor (O8); `digest compare` the comparator with its first-mismatch dump (O9a); `digest
//! replay` a recorded node's blocks through V (O9a-2), and [`replay_core`] its CoreConnector leg
//! (CH3a a7-1); `digest l2` the L2 driver over that leg ([`l2`], CH3a a7-2); `digest
//! rev-compare` a run's undo records against Core's ([`revfiles`], CH3a a7-3, D101), and
//! [`headerfile`] a header chain for `digest l2 --headers` (D162). The spec is
//! `docs/node-program/designs/O8-statedigest-v1.md`.

pub mod amount;
pub mod blkdir;
pub mod compare;
pub mod dump;
pub mod extract;
pub mod frames;
pub mod headerfile;
pub mod json;
pub mod l2;
pub mod replay;
pub mod replay_core;
pub mod revfiles;
pub mod store;
pub mod utxo;

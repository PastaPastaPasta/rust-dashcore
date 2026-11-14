//! `diff_cmpct_initdata` (P3 §6.4, E4): `dashd-p2p`'s `PartiallyDownloadedBlock` against Core's
//! `InitData` and `FillBlock` (`oracle_partially_downloaded_block`). Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_cmpct_initdata");

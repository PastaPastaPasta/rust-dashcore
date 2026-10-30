//! `diff_mnhf_step` (EV3-3): dash-dml's MNHF manager (`CMNHFManager::ProcessBlock`,
//! `GetSignalsStage`, `UndoBlock`) against Core's `oracle_mnhf_step`. Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_mnhf_step");

//! `diff_credit_pool` (EV3-2): dash-dml's credit pool (`CheckCreditPoolDiffForBlock`,
//! `GetCreditPoolDiffForBlock`, `ConstructCreditPool`) against Core's
//! `oracle_credit_pool_step`. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_credit_pool");

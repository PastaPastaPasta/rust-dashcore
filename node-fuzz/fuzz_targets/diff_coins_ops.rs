//! `diff_coins_ops` (CH2c): CH2's coins cache, block overlay and flush points against Core's coins
//! views (`oracle_coins_ops`), with Core's DynamicMemoryUsage per op. Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_coins_ops");

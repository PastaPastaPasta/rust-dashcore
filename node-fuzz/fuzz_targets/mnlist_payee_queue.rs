//! `mnlist_payee_queue` (EV2b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("mnlist_payee_queue");

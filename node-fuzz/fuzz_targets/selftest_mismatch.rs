//! The runner's self-test (CS0 §9.3): a comparison that reports a divergence on the fixed public
//! input `node-fuzz selftest`, crashes on `node-fuzz crash` (the minimisation case), and agrees on
//! everything else.
#![no_main]

use dash_oracle_sys::Pin;
use node_l1::{Compared, Divergence, Outcome};

fn compare(data: &[u8], _pin: Pin) -> Compared {
    if data == b"node-fuzz crash" {
        panic!("selftest_mismatch: the self-test's crash");
    }
    if data == b"node-fuzz selftest" {
        Err(Divergence {
            field: "planted",
        })
    } else {
        Ok(Outcome::BothReject)
    }
}

node_fuzz::l1_target!("selftest_mismatch", compare);

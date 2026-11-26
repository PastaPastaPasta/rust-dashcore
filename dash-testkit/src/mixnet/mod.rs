//! The mixed-network harness (PLAN-O12b, US-DEV-09): rdashd masternodes beside dashd, on regtest
//! and on a devnet, judged from one observation log.
//!
//! [`contract`] is pure: no I/O, clock, process, environment or network.

pub mod contract;

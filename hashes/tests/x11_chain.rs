//! Every header of a chain dumped from a reference oracle hashes to the oracle's hash, with every
//! X11 backend in the build.
//!
//! Ignored by default: set `DASH_X11_CHAIN` to a directory written by
//! `contrib/dump-oracle-headers.py` (see `oracle_chain/mod.rs`) and run
//! `cargo test --release -p dashcore_hashes --features x11 -- --ignored`, then with
//! `--features x11-c`.
//!
//! Compared per header: `hash_x11::Hash` (the backend the features select, which is what
//! `dashcore::BlockHash` uses: `rs-x11-hash` under `x11-c`), `dash_x11::x11` (the CPU's best
//! implementation) and the portable `dash-x11` code.
#![cfg(feature = "x11")]

mod oracle_chain;

use dashcore_hashes::{hash_x11, Hash};

#[test]
#[ignore = "needs DASH_X11_CHAIN, a dump from contrib/dump-oracle-headers.py"]
fn every_oracle_header() {
    let chain = oracle_chain::Chain::load();
    let bad = chain.mismatches(|_, raw, want| {
        let mut bad = Vec::new();
        if hash_x11::Hash::hash(raw).as_byte_array() != want {
            bad.push("hash_x11::Hash");
        }
        if &dash_x11::x11(raw) != want {
            bad.push("dash_x11::x11");
        }
        if &dash_x11::Implementation::standard().x11(raw) != want {
            bad.push("dash_x11 portable");
        }
        bad
    });
    assert!(bad.is_empty(), "{} mismatches, first {:?}", bad.len(), &bad[..bad.len().min(5)]);
    eprintln!("every_oracle_header: {} headers match with {}", chain.len(), hash_x11::backend());
}

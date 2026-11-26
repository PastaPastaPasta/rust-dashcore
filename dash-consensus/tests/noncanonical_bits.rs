//! Non-canonical `nBits` (CS2 §3.2, §6.1; GPT M2). `0x1e010000`, `0x1f000100` and `0x20000001`
//! all decode to 2^232, and `CheckProofOfWork` accepts each of them. Core never re-encodes header
//! bits, so the port must not either: the recorded cases are Core's answers on both pins
//! (`tests/data/oracle/noncanonical.json`).

mod common;

use common::{VecChain, chain_params, data, flag, pow_params, uint32};
use dash_consensus::pow::{check_proof_of_work, get_next_work_required, nbits_acceptable};
use dash_core_vectors::hex::parse_hex;
use dash_num::CompactTarget;
use dash_types::Numeric;

#[test]
fn check_proof_of_work_takes_the_raw_word() {
    let cases = data("oracle/noncanonical.json");
    for case in cases["check_proof_of_work"].as_array().unwrap() {
        let p = pow_params(&chain_params("v24.0", case["chain"].as_str().unwrap()));
        let hash: [u8; 32] = parse_hex(case["hash"].as_str().unwrap()).try_into().unwrap();
        let ok = check_proof_of_work(&hash, CompactTarget::new(uint32(&case["bits"])), &p);
        assert_eq!(ok, flag(&case["ok"]), "{case}");
    }
}

#[test]
fn next_work_returns_raw_words() {
    let cases = data("oracle/noncanonical.json");
    let mut raw_returned = 0;
    for case in cases["next_work"].as_array().unwrap() {
        let p = pow_params(&chain_params("v24.0", case["chain"].as_str().unwrap()));
        let chain = VecChain::from_case(case);
        let core = uint32(&case["bits"]);
        let got = get_next_work_required(&chain, chain.tip(), uint32(&case["new_time"]), &p);
        assert_eq!(got.to_base(), core, "{case}");
        // Core hands the stored word back, not its canonical re-encoding
        let canonical = got.expand().value.compact(false).to_base();
        raw_returned += usize::from(canonical != core);
    }
    assert_eq!(raw_returned, 4, "the BTC and min-difficulty cases of both non-canonical words");
}

/// From 68590 on, mainnet's rule compares the raw words (`block.nBits != GetNextWorkRequired`),
/// so a non-canonical header is rejected even when its target is the expected one. The end-to-end
/// verdict from Core's own ContextualCheckBlockHeader comes with CS3's L1 target (O2a).
#[test]
fn the_exact_rule_compares_raw_words() {
    let p = pow_params(&chain_params("v24.0", "main"));
    let canonical = CompactTarget::new(0x1e01_0000);
    for raw in [0x1f00_0100, 0x2000_0001].map(CompactTarget::new) {
        assert_eq!(raw.expand().value, canonical.expand().value);
        assert!(!nbits_acceptable(68_590, raw, canonical, &p));
        assert!(nbits_acceptable(68_590, raw, raw, &p));
    }
}

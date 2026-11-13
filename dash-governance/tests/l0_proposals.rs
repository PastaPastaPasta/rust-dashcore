//! L0: every `governance::ValidateProposal()` call of O2a-GOV's `proposals.txt` (Core's
//! `proposals_valid.json` / `proposals_invalid.json` driven as `governance_validators_tests.cpp`
//! drives them, answered by the v24.0 oracle), checked for the verdict and the full error string
//! (notes D15).

use dash_governance::GovParams;
use dash_governance::proposal::validate_proposal;
use dash_params::{ChainParamsBuilder, Network, Release};

const VECTORS: &str = include_str!("../../dash-oracle-sys/tests/data/gov/proposals.txt");

fn field<'a>(part: &'a str, key: &str) -> &'a str {
    part.strip_prefix(key)
        .and_then(|v| v.strip_prefix(' '))
        .unwrap_or_else(|| panic!("{key} in {part:?}"))
}

#[test]
fn proposals_match_core() {
    let chain = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet");
    let p = GovParams::from_chain_params(&chain).expect("mainnet");
    let (mut valid, mut invalid) = (0, 0);
    for line in VECTORS.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let parts: Vec<&str> = line.split(" | ").collect();
        assert_eq!(parts.len(), 7, "{line}");
        let name = parts[0];
        let expiration = field(parts[1], "expiration") == "1";
        let script = field(parts[2], "script") == "1";
        let time: i64 = field(parts[3], "time").parse().expect("time");
        let expected = match (parts[5], parts[6]) {
            ("valid", "-") => Ok(()),
            ("invalid", errors) => Err(errors.to_owned()),
            other => panic!("{name}: {other:?}"),
        };
        let got = validate_proposal(parts[4].as_bytes(), expiration, script, time, &p);
        assert_eq!(got, expected, "{name}");
        if got.is_ok() {
            valid += 1;
        } else {
            invalid += 1;
        }
    }
    // O2a-GOV's 76 calls over Core's two files plus `extreme_end_epoch_is_not_expired`.
    assert_eq!(valid + invalid, 76, "vector count");
    assert!(valid > 0 && invalid > 0);
}

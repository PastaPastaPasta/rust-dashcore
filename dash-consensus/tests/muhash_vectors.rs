//! MuHash3072's arithmetic against Dash Core's on boundary values
//! (`data/coins/muhash-boundary-v24.0.json`, written by `contrib/node/muhash-vectors/`; see
//! `data/PROVENANCE.md`). The values (0, 1, 2, p − 2, p − 1, p, p + 1, 2^3072 − 1, dense and
//! single-bit patterns) reach the branches random data never does: a product above p after the
//! reductions, an overflow at exactly p, and the carries of the second reduction. A state read
//! back from the coinstats index can hold any of them.
//!
//! For every pair (a, b), `{a, 1} *= {b, 1}` must give Core's numerator; for the quotients Core
//! recorded (every denominator, and the numerators at or above p − 1), `{a, b}`'s finalization
//! must give Core's hash and numerator (a / b). Each leaves a denominator of 1.

use dash_consensus::coins::MuHash3072;
use dash_core_vectors::hex::try_parse_hex;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Fixture {
    values: BTreeMap<String, String>,
    products: Vec<Product>,
    quotients: Vec<Quotient>,
}

#[derive(Deserialize)]
struct Product {
    a: String,
    b: String,
    numerator: String,
}

#[derive(Deserialize)]
struct Quotient {
    a: String,
    b: String,
    hash: String,
    numerator: String,
}

const BYTES: usize = MuHash3072::SERIALIZED_SIZE / 2;

fn unhex(text: &str) -> Vec<u8> {
    try_parse_hex(text).expect("hex")
}

/// The state `numerator / denominator`, as Core serializes it.
fn state(numerator: &[u8], denominator: &[u8]) -> MuHash3072 {
    let bytes: Vec<u8> = numerator.iter().chain(denominator).copied().collect();
    MuHash3072::from_bytes(&bytes.try_into().expect("two numbers"))
}

/// `numerator`'s serialized state over a denominator of 1.
fn over_one(numerator: &[u8]) -> Vec<u8> {
    let mut one = vec![0; BYTES];
    one[0] = 1;
    numerator.iter().chain(&one).copied().collect()
}

#[test]
fn muhash_arithmetic_reproduces_cores_on_boundary_values() {
    let path =
        format!("{}/tests/data/coins/muhash-boundary-v24.0.json", env!("CARGO_MANIFEST_DIR"));
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    let values: BTreeMap<&str, Vec<u8>> =
        fixture.values.iter().map(|(name, hex)| (name.as_str(), unhex(hex))).collect();
    assert_eq!(fixture.products.len(), values.len() * values.len());
    assert!(fixture.quotients.len() > values.len());
    let one = &values["1"];

    let mut differing = Vec::new();
    for product in &fixture.products {
        let mut mul = state(&values[product.a.as_str()], one);
        mul *= &state(&values[product.b.as_str()], one);
        if mul.to_bytes().to_vec() != over_one(&unhex(&product.numerator)) {
            differing.push(format!("{} * {}", product.a, product.b));
        }
    }
    for quotient in &fixture.quotients {
        let mut finalized = state(&values[quotient.a.as_str()], &values[quotient.b.as_str()]);
        if finalized.finalize().to_vec() != unhex(&quotient.hash)
            || finalized.to_bytes().to_vec() != over_one(&unhex(&quotient.numerator))
        {
            differing.push(format!("{} / {}", quotient.a, quotient.b));
        }
    }
    assert!(differing.is_empty(), "the node differs from Core on {differing:?}");
}

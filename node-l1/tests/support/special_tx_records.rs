//! BS6b's Core-signed provider transactions (`dash-oracle-sys`' signature records), for the
//! EV1-2 tests.

#![allow(dead_code)]

use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_primitives::SpecialPayload;
use dash_types::codec::BaseCodec;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Records {
    records: Vec<SignedTx>,
}

#[derive(Deserialize)]
pub struct SignedTx {
    pub txid: String,
    pub network: String,
    #[serde(rename = "type")]
    pub tx_type: u16,
    pub raw: String,
    pub signer: Value,
}

impl SignedTx {
    /// The record's transaction.
    pub fn tx(&self) -> Tx {
        decode_tx_exact(&unhex(&self.raw)).expect("Core's transaction").into_value()
    }
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

pub fn records() -> Vec<SignedTx> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dash-oracle-sys/tests/data/special-tx-signatures.json"
    );
    let text = std::fs::read_to_string(path).expect(path);
    serde_json::from_str::<Records>(&text).expect(path).records
}

/// The payload's encoding (base-sdk's codec, as Core serializes it).
pub fn encode_payload(p: &SpecialPayload) -> Vec<u8> {
    let mut out = Vec::new();
    match p {
        SpecialPayload::ProviderRegister(p) => p.encode(&mut out),
        SpecialPayload::ProviderUpdateService(p) => p.encode(&mut out),
        SpecialPayload::ProviderUpdateRegistrar(p) => p.encode(&mut out),
        SpecialPayload::ProviderUpdateRevoke(p) => p.encode(&mut out),
        SpecialPayload::ProviderDissolve(p) => p.encode(&mut out),
        SpecialPayload::ProviderUpdateShare(p) => p.encode(&mut out),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => p.encode(&mut out),
        other => panic!("not a provider payload: {other:?}"),
    }
    out
}

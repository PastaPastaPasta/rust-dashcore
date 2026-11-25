//! Helpers shared by the smoke tests.

#![allow(dead_code)]

pub mod gov_vectors;
pub mod mnhf_vectors;
pub mod quorum;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::DeserializeOwned;

pub fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An RPC hash (display order) in Core's internal byte order.
pub fn rpc_hash(s: &str) -> [u8; 32] {
    let mut h: [u8; 32] = hex(s).try_into().expect("32-byte hash");
    h.reverse();
    h
}

/// A hash in Core's internal byte order as RPC prints it (display order).
pub fn rpc_hex(hash: &[u8; 32]) -> String {
    let mut h = *hash;
    h.reverse();
    to_hex(&h)
}

/// Headers from their concatenated hex.
pub fn headers(hex_str: &str) -> Vec<[u8; 80]> {
    hex(hex_str).chunks(80).map(|h| h.try_into().expect("80-byte header")).collect()
}

/// Context coins from a JSON array of {txid (RPC order), vout, height, coinbase, amount, script}.
pub fn coins(list: &serde_json::Value) -> Vec<dash_oracle_sys::ContextCoin> {
    list.as_array()
        .expect("coins")
        .iter()
        .map(|c| dash_oracle_sys::ContextCoin {
            txid: rpc_hash(c["txid"].as_str().unwrap()),
            vout: c["vout"].as_u64().unwrap() as u32,
            height: c["height"].as_u64().unwrap() as u32,
            coinbase: c["coinbase"].as_bool().unwrap(),
            amount: c["amount"].as_i64().unwrap(),
            script_pubkey: hex(c["script"].as_str().unwrap()),
        })
        .collect()
}

/// A file under tests/data.
pub fn load_data<T: DeserializeOwned>(file: &str) -> T {
    let path = format!("{}/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
}

/// A file of `records` (tests/data/core-tests and dash-core-vectors' `transcribed`).
#[derive(Deserialize)]
struct Records<T> {
    records: Vec<T>,
}

/// The records of a file under tests/data.
pub fn load_records<T: DeserializeOwned>(file: &str) -> Vec<T> {
    load_data::<Records<T>>(file).records
}

/// The records of one of dash-core-vectors' `transcribed/*.json` files.
pub fn transcribed<T: DeserializeOwned>(file: &str) -> Vec<T> {
    let text = dash_core_vectors::read_core_text(&format!("transcribed/{file}"));
    serde_json::from_str::<Records<T>>(&text).expect(file).records
}

#[derive(Deserialize)]
pub struct HeaderRange {
    pub first_height: i32,
    pub hex: String,
}

#[derive(Deserialize)]
pub struct Subsidy {
    pub height: i32,
    pub subsidy: i64,
    pub prev_bits: u32,
}

#[derive(Deserialize)]
pub struct Superblock {
    pub height: i32,
    pub budget: i64,
}

/// The empty list `CDeterministicMNList()` (no masternode registered), as evodb stores it.
pub fn empty_list() -> dash_oracle_sys::MnList {
    dash_oracle_sys::MnList::Supplied(hex(&format!("{}ffffffff0000000000", "00".repeat(32))))
}

/// An 80-byte header with only the fields next-work reads set.
pub fn header(time: u32, bits: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[68..72].copy_from_slice(&time.to_le_bytes());
    h[72..76].copy_from_slice(&bits.to_le_bytes());
    h
}

/// Regtest's genesis header (chainparams.cpp `CreateGenesisBlock(1417713337, 1096447, 0x207fffff, 1, ...)`).
pub fn regtest_genesis() -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&1i32.to_le_bytes());
    let mut merkle = hex("e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7");
    merkle.reverse();
    h[36..68].copy_from_slice(&merkle);
    h[68..72].copy_from_slice(&1_417_713_337u32.to_le_bytes());
    h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    h[76..80].copy_from_slice(&1_096_447u32.to_le_bytes());
    h
}

/// A regtest chain context of the genesis block alone, with the empty masternode list.
pub fn genesis_context(oracle: &dash_oracle_sys::Oracle) -> dash_oracle_sys::Context {
    let genesis = regtest_genesis();
    assert_eq!(
        oracle.header_decode(&genesis).unwrap().hash,
        rpc_hash("000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e")
    );
    dash_oracle_sys::Context::new(
        0,
        vec![genesis],
        false,
        vec![],
        1_417_713_337 + 86_400,
        dash_oracle_sys::Checkpoints::Disabled,
        dash_oracle_sys::CoinsView::Unchecked(vec![]),
        empty_list(),
        vec![],
    )
}

pub fn header_time(h: &[u8]) -> u32 {
    u32::from_le_bytes(h[68..72].try_into().unwrap())
}

pub fn header_bits(h: &[u8]) -> u32 {
    u32::from_le_bytes(h[72..76].try_into().unwrap())
}

/// Counts of the cases a test reached, by name; `require` fails if any is below its minimum,
/// so trimmed fixtures cannot silently drop a case the test is meant to cover.
#[derive(Default, Debug)]
pub struct Coverage(BTreeMap<String, usize>);

impl Coverage {
    pub fn hit(&mut self, case: impl Into<String>) {
        *self.0.entry(case.into()).or_default() += 1;
    }

    pub fn require(&self, minimums: &[(&str, usize)]) {
        for (case, min) in minimums {
            let n = self.0.get(*case).copied().unwrap_or(0);
            assert!(
                n >= *min,
                "coverage: {case} reached {n} times, expected at least {min}; all: {:?}",
                self.0
            );
        }
    }

    /// Like `require`, for a count taken over every linked pin: each minimum is what one pin
    /// reaches, scaled by the number of pins the test ran on.
    pub fn require_per_pin(&self, minimums: &[(&str, usize)]) {
        let pins = dash_oracle_sys::Pin::ALL.len();
        for (case, per_pin) in minimums {
            let n = self.0.get(*case).copied().unwrap_or(0);
            assert!(
                n >= per_pin * pins,
                "coverage: {case} reached {n} times over {pins} pin(s), expected at least \
                 {per_pin} per pin; all: {:?}",
                self.0
            );
        }
    }
}

/// The extra payload of a special transaction (`vExtraPayload`, after the lock time).
pub fn extra_payload(tx: &[u8]) -> Vec<u8> {
    fn compact(b: &mut &[u8]) -> usize {
        let (first, rest) = b.split_first().unwrap();
        let (n, width) = match first {
            0xfd => (u64::from(u16::from_le_bytes(rest[..2].try_into().unwrap())), 2),
            0xfe => (u64::from(u32::from_le_bytes(rest[..4].try_into().unwrap())), 4),
            0xff => (u64::from_le_bytes(rest[..8].try_into().unwrap()), 8),
            n => (u64::from(*n), 0),
        };
        *b = &rest[width..];
        n as usize
    }
    let mut b = &tx[4..];
    for _ in 0..compact(&mut b) {
        b = &b[36..];
        let script = compact(&mut b);
        b = &b[script + 4..];
    }
    for _ in 0..compact(&mut b) {
        b = &b[8..];
        let script = compact(&mut b);
        b = &b[script..];
    }
    b = &b[4..];
    let len = compact(&mut b);
    assert_eq!(b.len(), len, "payload length");
    b.to_vec()
}

/// Core's evo_trivialvalidation vectors: `[hash, type, "basic" | "legacy", tx hex, expected error]`.
pub fn trivial_vectors(file: &str) -> Vec<Vec<String>> {
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    let rows: Vec<Vec<String>> = serde_json::from_str(&text).unwrap();
    rows.into_iter().skip(1).collect()
}

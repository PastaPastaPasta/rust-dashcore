//! MuHash3072 and `coins::stats::hash_serialized_2` (over a whole set) against Dash Core's
//! (`data/coins/regtest-utxo-hash-v24.0.json`, written by `contrib/node/utxo-hash-vectors.py`; see
//! `data/PROVENANCE.md`): at each sampled height, Core's `dumptxoutset` snapshot (its coins
//! database in cursor order) must hash to the `gettxoutsetinfo` values Core reported for the same
//! tip. Every coin in the set is above height 0, so every transaction's header is quirk 33's `1`.
//! MuHash is also carried from one sample to the next by removing and inserting the coins that
//! differ. The last sample holds a coin of every special script type Core's coins database
//! compresses (0 to 5) and others stored as scripts.

use std::collections::BTreeMap;

use dash_consensus::coins::compress::compress_script;
use dash_consensus::coins::stats::hash_serialized_2;
use dash_consensus::coins::{Coin, MuHash3072};
use dash_core_vectors::hex::{try_parse_hex, uint256_to_hex};
use dash_primitives::{BlockHash, OutPoint, TxHash};
use dash_types::Numeric as _;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    samples: Vec<Sample>,
}

#[derive(Deserialize)]
struct Sample {
    height: u32,
    best_block: String,
    coins: u64,
    hash_serialized_2: String,
    muhash: String,
    snapshot: String,
}

fn unhex(text: &str) -> Vec<u8> {
    try_parse_hex(text).expect("hex")
}

fn array32(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("32 bytes")
}

/// A snapshot (`SnapshotMetadata`, then each `COutPoint` and `Coin` in cursor order): the base
/// block and the coins in that order.
fn read_snapshot(bytes: &[u8]) -> (BlockHash, Vec<(OutPoint, Coin)>) {
    let base = BlockHash::from_lendian(array32(&bytes[..32]));
    let count = u64::from_le_bytes(bytes[32..40].try_into().expect("8 bytes"));
    let mut rest = &bytes[40..];
    let mut coins = Vec::new();
    for _ in 0..count {
        let outpoint = OutPoint {
            hash: TxHash::from_lendian(array32(&rest[..32])),
            index: u32::from_le_bytes(rest[32..36].try_into().expect("4 bytes")),
        };
        rest = &rest[36..];
        coins.push((outpoint, Coin::decode(&mut rest).expect("Core's coin decodes")));
    }
    assert!(rest.is_empty(), "the snapshot holds exactly its coins");
    (base, coins)
}

#[test]
fn utxo_hashes_reproduce_cores() {
    let path =
        format!("{}/tests/data/coins/regtest-utxo-hash-v24.0.json", env!("CARGO_MANIFEST_DIR"));
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    assert!(fixture.samples.len() >= 5);

    let mut previous: Option<(MuHash3072, BTreeMap<OutPoint, Coin>)> = None;
    for sample in &fixture.samples {
        let (base, coins) = read_snapshot(&unhex(&sample.snapshot));
        assert_eq!(uint256_to_hex(base.as_bytes()), sample.best_block, "height {}", sample.height);
        assert_eq!(coins.len() as u64, sample.coins, "height {}", sample.height);

        let mut muhash = MuHash3072::new();
        for (outpoint, coin) in &coins {
            muhash.insert_coin(outpoint, &coin.as_ref());
        }
        let whole = hash_serialized_2(&base, coins.iter().map(|(op, coin)| (op, coin.as_ref())));
        let set: BTreeMap<OutPoint, Coin> = coins.into_iter().collect();
        assert_eq!(uint256_to_hex(&whole), sample.hash_serialized_2, "height {}", sample.height);
        assert_eq!(
            uint256_to_hex(&muhash.clone().finalize()),
            sample.muhash,
            "height {}",
            sample.height
        );

        if let Some((mut carried, before)) = previous.take() {
            for (outpoint, coin) in before.iter().filter(|(op, _)| !set.contains_key(op)) {
                carried.remove_coin(outpoint, &coin.as_ref());
            }
            for (outpoint, coin) in set.iter().filter(|(op, _)| !before.contains_key(op)) {
                carried.insert_coin(outpoint, &coin.as_ref());
            }
            assert_eq!(
                uint256_to_hex(&carried.finalize()),
                sample.muhash,
                "carried to {}",
                sample.height
            );
        }
        previous = Some((muhash, set));
    }

    let (_, last) = previous.expect("samples");
    let mut types = std::collections::BTreeSet::new();
    for coin in last.values() {
        let script = coin.as_ref().script_pubkey().to_script();
        types.insert(compress_script(script.as_slice()).map(|(kind, _)| kind));
    }
    let expected = [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];
    assert_eq!(types, expected.into_iter().collect(), "script types in the last sample");
}

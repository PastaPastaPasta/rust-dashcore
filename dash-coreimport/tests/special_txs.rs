//! DIP2 special transactions from Dash Core regtest blocks (see tests/data/PROVENANCE-regtest.md):
//! payload parsing, txids and merkle roots, including the stable txid of a version 2 asset unlock.

use dash_coreimport::hash_to_hex;
use dash_coreimport::tx::{block_hash, read_block};

fn block(name: &str) -> Vec<u8> {
    let hex = std::fs::read_to_string(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap();
    let hex = hex.trim();
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

fn txids(bytes: &[u8]) -> (String, Vec<(u16, String)>) {
    let b = read_block(bytes).unwrap();
    // The header Core mined commits to these txids, the stable txid of an asset unlock included.
    assert!(b.merkle_root_matches(), "merkle root");
    let txs = b.txs.iter().map(|t| (t.tx_type, hash_to_hex(&t.txid))).collect();
    (hash_to_hex(&block_hash(b.header)), txs)
}

#[test]
fn coinbase_and_asset_locks() {
    let (hash, txs) = txids(&block("regtest-asset-locks-750.hex"));
    assert_eq!(hash, "2e77a3cdfded0b9806185b518a80a60ce51c691c75ef171f6a0adb99133941ec");
    assert_eq!(
        txs,
        [
            (5, "f505b861432bf770603c9f0302969023d407c46583eb3b4904262129940ed0f3".into()),
            (8, "fa2b5f2cb1d99b6ef55e10a46ec85a3b1621b3ed854ea6720ad6f51ef4505325".into()),
            (8, "a80496095c71e1409ffd3fe138e8c9b79a44eb0029ee6de1ab8341fc2d8cff25".into()),
            (8, "0d88508724d5ec566e1b8e9d848394f4d1703ab5e58ea2720d158a6d3389fea5".into()),
        ]
    );
}

#[test]
fn asset_unlock_v2_has_a_stable_txid() {
    let bytes = block("regtest-asset-unlock-v2-969.hex");
    let (hash, txs) = txids(&bytes);
    assert_eq!(hash, "3fec332f23b2072d8d6e6710b769f3fd8731825dcf6fa1bb843a0d41ddeda7e9");
    assert_eq!(
        txs,
        [
            (5, "86b61772d6ef5f3a65dc5fd413d2401063ac53eaebbb9e88bbb0f85255615cab".into()),
            (9, "ed6d78c04f2e9ba2f1c81536cde6885725448ae7083b7580b21031cf565590b8".into()),
        ]
    );
    let unlock = &read_block(&bytes).unwrap().txs[1];
    assert_eq!(unlock.payload.map(|p| (p.len(), p[0])), Some((145, 2)));
}

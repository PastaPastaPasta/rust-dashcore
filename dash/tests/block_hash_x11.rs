//! `BlockHash` is the X11 hash of the header, with default features (BS15).
//!
//! The hashes and headers are Dash Core's: `dash-x11/tests/data/known-hashes.json` holds the
//! genesis hashes `chainparams.cpp` asserts and every checkpoint, with the headers the reference
//! oracles hold for them (its `PROVENANCE.md`). `every_oracle_header` replays a whole chain
//! dumped from an oracle by `hashes/contrib/dump-oracle-headers.py`; it is ignored unless run
//! with `--ignored` and `DASH_X11_CHAIN` naming the dump directory of one network.

use std::path::Path;
use std::str::FromStr;

use dashcore::blockdata::constants::genesis_block;
use dashcore::consensus::{deserialize, serialize};
use dashcore::hashes::{Hash, sha256d};
use dashcore::{BlockHash, Network, block};

#[path = "../../hashes/tests/oracle_chain/mod.rs"]
mod oracle_chain;

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn core_known_hashes() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../dash-x11/tests/data/known-hashes.json");
    let json: serde_json::Value = serde_json::from_slice(&read(&path)).unwrap();
    let entries: Vec<_> = json["genesis"]
        .as_array()
        .unwrap()
        .iter()
        .chain(json["checkpoints"].as_array().unwrap())
        .collect();
    assert_eq!(entries.len(), 3 + 58);
    for e in entries {
        let raw = hex_conservative::decode_to_vec(e["header"].as_str().unwrap()).unwrap();
        let header: block::Header = deserialize(&raw).unwrap();
        let want = e["hash"].as_str().unwrap();
        assert_eq!(header.block_hash().to_string(), want, "{e}");
        assert_eq!(header.block_hash(), BlockHash::from_str(want).unwrap(), "{e}");
        // The hash is of the serialized header, not sha256d as the pre-BS15 default was.
        assert_eq!(serialize(&header), raw);
        assert_ne!(header.block_hash().to_byte_array(), sha256d::Hash::hash(&raw).to_byte_array());
    }
}

#[test]
fn genesis_blocks() {
    for (network, hash) in [
        (Network::Mainnet, "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6"),
        (Network::Testnet, "00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c"),
        (Network::Regtest, "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"),
    ] {
        let block = genesis_block(network);
        assert_eq!(block.block_hash().to_string(), hash, "{network:?}");
        assert_eq!(block.header.block_hash().to_string(), hash, "{network:?}");
    }
}

#[test]
#[ignore = "needs DASH_X11_CHAIN, a dump from hashes/contrib/dump-oracle-headers.py"]
fn every_oracle_header() {
    let chain = oracle_chain::Chain::load();
    let bad = chain.mismatches(|_, raw, want| {
        let header: block::Header = deserialize(raw).unwrap();
        if header.block_hash().as_byte_array() == want {
            vec![]
        } else {
            vec!["Header::block_hash"]
        }
    });
    assert!(
        bad.is_empty(),
        "{} headers differ from the oracle, first {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
    eprintln!("every_oracle_header: {} headers match the oracle", chain.len());
}

//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! `dash-chainparams` decodes `-sporkaddr` with its own Base58Check reader; it
//! must read P2PKH addresses as [`Recipient::from_base58c`] does.

#![expect(clippy::unwrap_used, reason = "test code")]

use dash_chainparams::{
    ChainOption, ChainParamsBuilder, ChainSetup, Network, ParamsError, Release,
};
use dash_script::{PubKeyHash, Recipient};
use dash_types::Numeric;
use test_case::test_case;

/// The spork key ID `dash-params` reads from `address`, or `None` if it
/// refuses the address.
fn spork_key_id(network: Network, address: &str) -> Option<[u8; 20]> {
    let built = ChainParamsBuilder::new(network, Release::V24_0)
        .arg(ChainOption::SporkAddr, address)
        .build();
    match built {
        Ok(setup) => Some(*setup.spork_signers.key_ids[0].as_bytes()),
        Err(err) => {
            assert_eq!(err, ParamsError::SporkAddress, "{address}");
            None
        }
    }
}

/// The P2PKH key ID `dash-script` reads from `address`.
fn recipient_key_id(setup: &ChainSetup, address: &str) -> Option<[u8; 20]> {
    match Recipient::from_base58c(address, &setup.base_sdk_addr_params())? {
        Recipient::PubKeyHash(hash) => Some(*hash.as_bytes()),
        _ => None,
    }
}

/// 20-byte hashes: the extremes and a fixed pseudo-random spread.
fn hashes() -> Vec<[u8; 20]> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut hashes = vec![[0; 20], [0xff; 20]];
    for _ in 0..32 {
        hashes.push(core::array::from_fn(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        }));
    }
    hashes
}

#[test_case(Network::Main; "mainnet")]
#[test_case(Network::Testnet3; "testnet")]
#[test_case(Network::Devnet; "devnet")]
#[test_case(Network::Regtest; "regtest")]
fn spork_addresses_decode_as_recipients(network: Network) {
    let setup = ChainParamsBuilder::new(network, Release::V24_0).build().unwrap();
    let addr = setup.addr_params;
    let mut accepted = 0;
    for hash in hashes() {
        for version in [addr.pubkey_addr, addr.script_addr, addr.secret_key, 0, 0xff] {
            let address = PubKeyHash::from_lendian(hash).to_base58c(version);
            let ours = spork_key_id(network, &address);
            assert_eq!(ours, recipient_key_id(&setup, &address), "{address}");
            assert_eq!(ours.is_some(), version == addr.pubkey_addr, "{address}");
            accepted += usize::from(ours.is_some());
            // A corrupted checksum is refused by both.
            let mut corrupted = address.clone();
            let last = corrupted.pop().unwrap();
            corrupted.push(if last == '1' {
                '2'
            } else {
                '1'
            });
            assert_eq!(spork_key_id(network, &corrupted), None, "{corrupted}");
            assert_eq!(recipient_key_id(&setup, &corrupted), None, "{corrupted}");
        }
    }
    assert_eq!(accepted, hashes().len());
    for other in ["", "1", "not an address", "11111111111111111111111111111111111111"] {
        assert_eq!(spork_key_id(network, other), None, "{other}");
        assert_eq!(recipient_key_id(&setup, other), None, "{other}");
    }
}

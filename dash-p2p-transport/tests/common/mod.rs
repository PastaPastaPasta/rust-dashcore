//! Network message starts for the tests, from base-sdk's chain parameters.
#![allow(dead_code)] // each test binary uses its own subset

use dash_p2p_transport::Magic;
use dash_params::{ChainParamsBuilder, Network, Release};

fn magic(network: Network) -> Magic {
    let params = ChainParamsBuilder::new(network, Release::V24_0).build().unwrap();
    Magic::from_bytes(params.message_start)
}

/// Regtest's: the network Core's transport tests and our dashd run on.
pub fn regtest() -> Magic {
    magic(Network::Regtest)
}

/// Mainnet's: Core's BIP324 vectors, and "another network" for the regtest transports.
pub fn mainnet() -> Magic {
    magic(Network::Main)
}

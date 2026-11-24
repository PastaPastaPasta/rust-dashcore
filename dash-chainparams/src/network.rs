//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The networks a node runs: base-sdk's three and Core's devnet.

use crate::{AddrParams, ChainParams, ConsensusParams, devnet};

/// Dash network identifier.
///
/// The discriminants are stable and equal to `FFINetwork`'s. base-sdk's `Network` has no
/// devnet and other discriminants, so the two convert by variant, never by value
/// ([`TryFrom`]).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Network {
    /// Production network.
    Main,
    /// Public testnet.
    Testnet3,
    /// Development network, here the one `-devnet=` names with an empty name. A named devnet
    /// has its own devnet genesis block, so it is built with
    /// [`ChainParamsBuilder`](crate::ChainParamsBuilder).
    Devnet,
    /// Local regression test network.
    Regtest,
}

impl Network {
    /// Every network, in discriminant order.
    pub const ALL: [Self; 4] = [Self::Main, Self::Testnet3, Self::Devnet, Self::Regtest];

    /// Core's `NetworkIDString()`: `main`, `test`, `devnet` or `regtest`.
    pub const fn network_id(self) -> &'static str {
        self.chain().network_id
    }

    /// The network whose [`network_id`](Self::network_id) is `id`, exactly.
    pub fn from_network_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|network| network.network_id() == id)
    }

    /// Full chain parameters for this network: base-sdk's table, or this crate's for the
    /// devnet.
    ///
    /// The devnet-only consensus values are not [`ConsensusParams`] fields upstream; a
    /// [`ChainSetup`](crate::ChainSetup) holds them in its
    /// [`ConsensusSetup`](crate::ConsensusSetup).
    pub const fn chain(self) -> &'static ChainParams {
        match self {
            Self::Main => bsdk_params::Network::Main.chain(),
            Self::Testnet3 => bsdk_params::Network::Testnet3.chain(),
            Self::Devnet => &devnet::PARAMS,
            Self::Regtest => bsdk_params::Network::Regtest.chain(),
        }
    }

    /// Address encoding parameters for this network.
    pub const fn addr(self) -> &'static AddrParams {
        &self.chain().addr_params
    }

    /// Consensus parameters for this network.
    pub const fn consensus(self) -> &'static ConsensusParams {
        &self.chain().consensus
    }
}

/// base-sdk's network of the same name; the devnet has none, and is returned as the error.
impl TryFrom<Network> for bsdk_params::Network {
    type Error = Network;

    fn try_from(network: Network) -> Result<Self, Network> {
        match network {
            Network::Main => Ok(Self::Main),
            Network::Testnet3 => Ok(Self::Testnet3),
            Network::Regtest => Ok(Self::Regtest),
            Network::Devnet => Err(network),
        }
    }
}

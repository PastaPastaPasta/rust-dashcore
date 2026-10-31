//! Conversions between [`Network`] and base-sdk's [`NetworkKind`] (BS16).
//!
//! Both enums name Core's four chains, `Mainnet`, `Testnet`, `Devnet` and
//! `Regtest`, with the same discriminants (`NetworkKind` says so; `Network` and
//! `FFINetwork` have always counted this way), so the conversions are total and
//! lossless in both directions: they are plain `From` impls, not `TryFrom`.
//!
//! - **Devnet names.** Neither side carries one. `NetworkKind` leaves a
//!   devnet's name to its chain parameters, and `Network::Devnet` is the kind
//!   alone. Converting to `NetworkKind` and back therefore loses nothing, but a
//!   `Network::Devnet` cannot say which devnet it is: a caller that needs
//!   `ChainParams` for one passes the name (`-devnet=<name>`) to the
//!   `ChainParamsBuilder` itself.
//! - **Helper values.** The conversion maps the kind only. `Network`'s own
//!   helpers are dashcore-stack values and can differ from the parameters of
//!   the same kind (`v20_activation_height` is 0 for devnet and regtest here,
//!   while Core's parameters activate V20 at 2 and 432), so do not read one as
//!   the other.
//! - **Unknown variants.** `NetworkKind` is not `#[non_exhaustive]`, so there
//!   are none, and both matches below are exhaustive with no wildcard arm. A
//!   fifth variant on either side fails to compile here instead of being
//!   folded into another network.

use dash_params::NetworkKind;

use crate::Network;

impl Network {
    /// The base-sdk [`NetworkKind`] of this network.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use dash_network::Network;
    /// use dash_params::NetworkKind;
    ///
    /// assert_eq!(Network::Testnet.network_kind(), NetworkKind::Testnet);
    /// ```
    pub const fn network_kind(self) -> NetworkKind {
        match self {
            Network::Mainnet => NetworkKind::Mainnet,
            Network::Testnet => NetworkKind::Testnet,
            Network::Devnet => NetworkKind::Devnet,
            Network::Regtest => NetworkKind::Regtest,
        }
    }

    /// The network of a base-sdk [`NetworkKind`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use dash_network::Network;
    /// use dash_params::NetworkKind;
    ///
    /// assert_eq!(Network::from_network_kind(NetworkKind::Devnet), Network::Devnet);
    /// ```
    pub const fn from_network_kind(kind: NetworkKind) -> Network {
        match kind {
            NetworkKind::Mainnet => Network::Mainnet,
            NetworkKind::Testnet => Network::Testnet,
            NetworkKind::Devnet => Network::Devnet,
            NetworkKind::Regtest => Network::Regtest,
        }
    }
}

impl From<Network> for NetworkKind {
    fn from(network: Network) -> Self {
        network.network_kind()
    }
}

impl From<NetworkKind> for Network {
    fn from(kind: NetworkKind) -> Self {
        Network::from_network_kind(kind)
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, Release};

    use super::*;

    const ALL: [Network; 4] =
        [Network::Mainnet, Network::Testnet, Network::Devnet, Network::Regtest];

    #[test]
    fn network_to_kind_and_back_is_identity() {
        for network in ALL {
            let kind = NetworkKind::from(network);
            assert_eq!(Network::from(kind), network, "{network}");
        }
    }

    #[test]
    fn kind_to_network_and_back_is_identity() {
        for kind in NetworkKind::ALL {
            let network = Network::from(kind);
            assert_eq!(NetworkKind::from(network), kind, "{kind:?}");
        }
    }

    #[test]
    fn both_enums_list_the_same_chains() {
        assert_eq!(ALL.len(), NetworkKind::ALL.len());
        assert_eq!(ALL.map(Network::network_kind), NetworkKind::ALL);
    }

    /// The pair names the same chain: the wire magic and the default port of `Network` are
    /// the ones of the parameters built for its kind.
    #[test]
    fn the_pair_names_the_same_chain() {
        for network in ALL {
            let mut builder = ChainParamsBuilder::new(network.network_kind(), Release::V24_0);
            if network == Network::Devnet {
                builder = builder.devnet_name("bs16");
            }
            let chain = builder.build().unwrap();
            assert_eq!(chain.network, network.network_kind(), "{network}");
            assert_eq!(chain.message_start, network.magic().to_le_bytes(), "{network}");
            assert_eq!(chain.default_port, network.default_p2p_port(), "{network}");
        }
    }

    #[test]
    fn variants_pair_by_name() {
        assert_eq!(NetworkKind::from(Network::Mainnet), NetworkKind::Mainnet);
        assert_eq!(NetworkKind::from(Network::Testnet), NetworkKind::Testnet);
        assert_eq!(NetworkKind::from(Network::Devnet), NetworkKind::Devnet);
        assert_eq!(NetworkKind::from(Network::Regtest), NetworkKind::Regtest);
    }

    /// `Network` and `NetworkKind` count the chains alike (`FFINetwork` is checked in `ffi`).
    #[test]
    fn discriminants_agree() {
        for network in ALL {
            assert_eq!(network.network_kind() as u8, network as u8, "{network}");
        }
    }

    /// `Display` is the lowercase chain name; Core's `NetworkIDString()` is the
    /// kind's own and differs for main and test, so the two are not interchangeable.
    #[test]
    fn names_are_kept_apart() {
        for network in ALL {
            let id = network.network_kind().network_id();
            assert_eq!(NetworkKind::from_network_id(id), Some(network.network_kind()));
        }
        assert_eq!(Network::Mainnet.to_string(), "mainnet");
        assert_eq!(Network::Mainnet.network_kind().network_id(), "main");
        assert_eq!(Network::Testnet.to_string(), "testnet");
        assert_eq!(Network::Testnet.network_kind().network_id(), "test");
        assert_eq!(Network::Devnet.network_kind().network_id(), "devnet");
        assert_eq!(Network::Regtest.network_kind().network_id(), "regtest");
    }

    #[test]
    fn conversions_are_const() {
        const KIND: NetworkKind = Network::Regtest.network_kind();
        const NETWORK: Network = Network::from_network_kind(NetworkKind::Testnet);
        assert_eq!(KIND, NetworkKind::Regtest);
        assert_eq!(NETWORK, Network::Testnet);
    }
}

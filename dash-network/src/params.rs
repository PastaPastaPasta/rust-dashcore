//! Conversions between [`Network`] and base-sdk's [`CoreNetwork`] (BS16).
//!
//! Both enums name Core's four chains (`Mainnet`/`Main`, `Testnet`/`Testnet3`,
//! `Devnet` and `Regtest`) with the same discriminants (`CoreNetwork` counts
//! them so; `Network` and `FFINetwork` have always counted this way), so the
//! conversions are total and lossless in both directions: they are plain
//! `From` impls, not `TryFrom`.
//!
//! - **Devnet names.** Neither side carries one. `CoreNetwork` leaves a
//!   devnet's name to its chain setup, and `Network::Devnet` is the kind
//!   alone. Converting to `CoreNetwork` and back therefore loses nothing, but a
//!   `Network::Devnet` cannot say which devnet it is: a caller that needs the
//!   `ChainSetup` for one passes the name (`-devnet=<name>`) to the
//!   `ChainParamsBuilder` itself, and [`Network::chain_params`] has none for it.
//! - **Helper values.** The conversion maps the kind only. `Network`'s own
//!   helpers are dashcore-stack values and can differ from the parameters of
//!   the same kind (`v20_activation_height` is 0 for devnet and regtest here,
//!   while Core's parameters activate V20 at 2 and 432), so do not read one as
//!   the other.
//! - **Unknown variants.** `CoreNetwork` is not `#[non_exhaustive]`, so there
//!   are none, and both matches below are exhaustive with no wildcard arm. A
//!   fifth variant on either side fails to compile here instead of being
//!   folded into another network.

use dash_params::Network as CoreNetwork;

use crate::Network;

impl Network {
    /// The base-sdk [`CoreNetwork`] of this network.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use dash_network::Network;
    /// use dash_params::Network as CoreNetwork;
    ///
    /// assert_eq!(Network::Testnet.core_network(), CoreNetwork::Testnet3);
    /// ```
    pub const fn core_network(self) -> CoreNetwork {
        match self {
            Network::Mainnet => CoreNetwork::Main,
            Network::Testnet => CoreNetwork::Testnet3,
            Network::Devnet => CoreNetwork::Devnet,
            Network::Regtest => CoreNetwork::Regtest,
        }
    }

    /// The network of a base-sdk [`CoreNetwork`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use dash_network::Network;
    /// use dash_params::Network as CoreNetwork;
    ///
    /// assert_eq!(Network::from_core_network(CoreNetwork::Devnet), Network::Devnet);
    /// ```
    pub const fn from_core_network(network: CoreNetwork) -> Network {
        match network {
            CoreNetwork::Main => Network::Mainnet,
            CoreNetwork::Testnet3 => Network::Testnet,
            CoreNetwork::Devnet => Network::Devnet,
            CoreNetwork::Regtest => Network::Regtest,
        }
    }
}

impl From<Network> for CoreNetwork {
    fn from(network: Network) -> Self {
        network.core_network()
    }
}

impl From<CoreNetwork> for Network {
    fn from(network: CoreNetwork) -> Self {
        Network::from_core_network(network)
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, Release};

    use super::*;

    #[test]
    fn network_to_core_network_and_back_is_identity() {
        for network in Network::ALL {
            let core = CoreNetwork::from(network);
            assert_eq!(Network::from(core), network, "{network}");
        }
    }

    #[test]
    fn core_network_to_network_and_back_is_identity() {
        for core in CoreNetwork::ALL {
            let network = Network::from(core);
            assert_eq!(CoreNetwork::from(network), core, "{core:?}");
        }
    }

    #[test]
    fn both_enums_list_the_same_chains() {
        assert_eq!(Network::ALL.map(Network::core_network), CoreNetwork::ALL);
    }

    /// The pair names the same chain: the wire magic and the default port of `Network` are
    /// the ones of the setup built for its core network.
    #[test]
    fn the_pair_names_the_same_chain() {
        for network in Network::ALL {
            let mut builder = ChainParamsBuilder::new(network.core_network(), Release::V24_0);
            if network == Network::Devnet {
                builder = builder.devnet_name("bs16");
            }
            let setup = builder.build().unwrap();
            assert_eq!(setup.network, network.core_network(), "{network}");
            assert_eq!(setup.message_start, network.magic().to_le_bytes(), "{network}");
            assert_eq!(setup.default_port, network.default_p2p_port(), "{network}");
        }
    }

    #[test]
    fn variants_pair_by_name() {
        assert_eq!(CoreNetwork::from(Network::Mainnet), CoreNetwork::Main);
        assert_eq!(CoreNetwork::from(Network::Testnet), CoreNetwork::Testnet3);
        assert_eq!(CoreNetwork::from(Network::Devnet), CoreNetwork::Devnet);
        assert_eq!(CoreNetwork::from(Network::Regtest), CoreNetwork::Regtest);
    }

    /// `Network` and `CoreNetwork` count the chains alike (`FFINetwork` is checked in `ffi`).
    #[test]
    fn discriminants_agree() {
        for network in Network::ALL {
            assert_eq!(network.core_network() as u8, network as u8, "{network}");
        }
    }

    /// `Display` is the lowercase chain name; Core's `NetworkIDString()` is the
    /// core network's own and differs for main and test, so the two are not interchangeable.
    #[test]
    fn names_are_kept_apart() {
        for network in Network::ALL {
            let id = network.core_network().network_id();
            assert_eq!(CoreNetwork::from_network_id(id), Some(network.core_network()));
        }
        assert_eq!(Network::Mainnet.to_string(), "mainnet");
        assert_eq!(Network::Mainnet.core_network().network_id(), "main");
        assert_eq!(Network::Testnet.to_string(), "testnet");
        assert_eq!(Network::Testnet.core_network().network_id(), "test");
        assert_eq!(Network::Devnet.core_network().network_id(), "devnet");
        assert_eq!(Network::Regtest.core_network().network_id(), "regtest");
    }

    #[test]
    fn conversions_are_const() {
        const CORE: CoreNetwork = Network::Regtest.core_network();
        const NETWORK: Network = Network::from_core_network(CoreNetwork::Testnet3);
        assert_eq!(CORE, CoreNetwork::Regtest);
        assert_eq!(NETWORK, Network::Testnet);
    }
}

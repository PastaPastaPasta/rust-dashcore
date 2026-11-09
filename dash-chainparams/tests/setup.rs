//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The chain setup a node reads, against the network tables.

#![expect(clippy::unwrap_used, reason = "test code")]
#![expect(
    clippy::disallowed_methods,
    reason = "compares the setup with the static network tables on purpose"
)]

use bitcoin_units::BlockHeight;
use dash_chainparams::{
    AddrSetup, Bip9Deployment, ChainParams, ChainParamsBuilder, ChainSetup, DeploymentPos,
    LlmqType, Network, Release,
};
use test_case::test_case;

fn setup(network: Network, release: Release) -> ChainSetup {
    ChainParamsBuilder::new(network, release).build().unwrap()
}

/// The [`ChainParams`] copy that `Deref` and coercion reach.
fn chain(setup: &ChainSetup) -> &ChainParams {
    setup
}

#[test]
fn network_discriminants_are_ffi_networks() {
    let discriminants: Vec<(Network, u8)> =
        Network::ALL.into_iter().map(|n| (n, n as u8)).collect();
    assert_eq!(
        discriminants,
        [(Network::Main, 0), (Network::Testnet3, 1), (Network::Devnet, 2), (Network::Regtest, 3)]
    );
}

#[test_case(Network::Main, "main"; "mainnet")]
#[test_case(Network::Testnet3, "test"; "testnet")]
#[test_case(Network::Devnet, "devnet"; "devnet")]
#[test_case(Network::Regtest, "regtest"; "regtest")]
fn network_ids_are_the_tables(network: Network, id: &str) {
    assert_eq!(network.network_id(), id);
    assert_eq!(network.chain().network_id, id);
    assert_eq!(Network::from_network_id(id), Some(network));
}

#[test_case(""; "empty")]
#[test_case("Main"; "capitalized")]
#[test_case("testnet3"; "testnet3")]
#[test_case("devnet-x"; "devnet_named")]
fn other_network_ids_name_no_network(id: &str) {
    assert_eq!(Network::from_network_id(id), None);
}

/// With no options at v24.0, every view a node reads equals the network's
/// table.
#[test_case(Network::Main; "mainnet")]
#[test_case(Network::Testnet3; "testnet")]
#[test_case(Network::Devnet; "devnet")]
#[test_case(Network::Regtest; "regtest")]
fn v24_setup_views_are_the_tables(network: Network) {
    let setup = setup(network, Release::V24_0);
    assert_eq!(*setup.consensus, *network.consensus());
    assert_eq!(chain(&setup), network.chain());
    let table = network.consensus().deployments;
    let deployments: Vec<_> =
        setup.consensus.deployments.iter().map(|(pos, d)| (pos, *d)).collect();
    assert_eq!(
        deployments,
        [
            (DeploymentPos::TestDummy, table.test_dummy),
            (DeploymentPos::V24, table.v24),
            (DeploymentPos::EvoShares, table.evo_shares),
        ]
    );
    let addr = network.addr();
    assert_eq!(setup.addr_params, AddrSetup::from(*addr));
    assert_eq!(setup.platform_hrp, Some(addr.bech32_hrp));
    assert_eq!(setup.base_sdk_addr_params(), *addr);
    assert_eq!(setup.checkpoints, network.chain().checkpoints);
    let unknown = [LlmqType::LlmqNone, LlmqType::Unknown(0xfe)];
    for llmq_type in LlmqType::variants().iter().copied().chain(unknown) {
        assert_eq!(
            setup.consensus.llmq(llmq_type).copied(),
            network.consensus().llmq(llmq_type),
            "{llmq_type}"
        );
    }
    assert!(setup.consensus.llmq_params().all(|(t, _)| network.consensus().llmqs.contains(&t)));
}

#[test_case(Network::Main; "mainnet")]
#[test_case(Network::Testnet3; "testnet")]
#[test_case(Network::Devnet; "devnet")]
#[test_case(Network::Regtest; "regtest")]
fn v24_does_not_require_standard(network: Network) {
    assert_eq!(setup(network, Release::V24_0).require_standard, None);
}

/// At v24.0.0, 2304 (four days) but 288 on regtest (chainparams.cpp:247,
/// :454, :639, :885).
#[test_case(Network::Main, 2304; "mainnet")]
#[test_case(Network::Testnet3, 2304; "testnet")]
#[test_case(Network::Devnet, 2304; "devnet")]
#[test_case(Network::Regtest, 288; "regtest")]
fn min_blocks_to_keep_is_cores(network: Network, v24: u32) {
    assert_eq!(setup(network, Release::V24_0).min_blocks_to_keep, v24);
}

#[test_case(Network::Main, ""; "mainnet")]
#[test_case(Network::Testnet3, "testnet3"; "testnet")]
#[test_case(Network::Devnet, "devnet"; "devnet")]
#[test_case(Network::Regtest, "regtest"; "regtest")]
fn data_dirs_are_cores(network: Network, dir: &str) {
    assert_eq!(setup(network, Release::V24_0).data_dir(), dir);
}

#[test]
fn devnet_checkpoints_hold_its_devnet_genesis_block() {
    let setup = setup(Network::Devnet, Release::V24_0);
    assert_eq!(setup.devnet_name.as_deref(), Some("devnet"));
    let devnet_genesis = setup.devnet_genesis.as_ref().unwrap();
    assert_eq!(devnet_genesis.name, "devnet");
    assert_eq!(devnet_genesis.hash, setup.consensus.hash_devnet_genesis_block);
    let consensus = &setup.consensus;
    assert_eq!(
        setup.checkpoints,
        [
            (BlockHeight::from_u32(0), consensus.hash_genesis_block),
            (BlockHeight::from_u32(1), consensus.hash_devnet_genesis_block),
        ]
    );
}

/// A caller outside the crate can change a deployment the release has, as
/// the node's MNHF tests do, and the release's view reads the change.
#[test]
fn deployments_change_through_the_release_view() {
    let mut v24 = setup(Network::Regtest, Release::V24_0);
    let start = v24.deployment(DeploymentPos::EvoShares).unwrap().start_time;
    let shares = v24.consensus.deployments.deployment_mut(DeploymentPos::EvoShares).unwrap();
    (shares.bit, shares.start_time, shares.timeout, shares.use_ehf) =
        (9, start + 30, start + 40, false);
    let want = Bip9Deployment {
        bit: 9,
        start_time: start + 30,
        timeout: start + 40,
        use_ehf: false,
        ..Network::Regtest.consensus().deployments.evo_shares
    };
    assert_eq!(v24.deployment(DeploymentPos::EvoShares), Some(&want));
    let positions: Vec<DeploymentPos> = v24.deployments().map(|(pos, _)| pos).collect();
    assert_eq!(positions, [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares]);
    assert_eq!(v24.deployments().last(), Some((DeploymentPos::EvoShares, &want)));
}

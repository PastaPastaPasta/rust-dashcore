//! O2b D8: each schedule's chain parameters are the same on both sides, Core's from
//! `oracle_chainparams_json` and rdashd's from its init over the same arguments.

use std::path::Path;
use std::sync::Arc;

use dash_chain::ChainPolicy;
use dash_oracle_sys::{Chain, Oracle, Pin};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use node_l1::chain::core::{Schedule, T_RECENT};
use node_l1::chain::params::{
    check, check_args, core_params, core_shape, differences, network_params, node_params,
};
use serde_json::Value;

fn node(dir: &Path, args: &[&str]) -> NodeSut {
    let mut argv = vec!["-regtest", "-printtoconsole=0"];
    argv.extend_from_slice(args);
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    NodeSut::new(dir, &argv, T_RECENT, ui).unwrap_or_else(|e| panic!("{args:?} refused: {e:?}"))
}

#[test]
fn each_schedule_has_one_set_of_chain_parameters() {
    for schedule in [Schedule::S0, Schedule::S1] {
        let dir = tempfile::tempdir().unwrap();
        let sut = node(dir.path(), &schedule.args());
        check(Pin::V24_0, schedule, sut.params()).unwrap();
    }
}

/// Every leaf Core prints has an rdashd counterpart: no key is compared on one side only.
#[test]
fn the_shapes_have_the_same_leaves() {
    let core = core_params(Pin::V24_0, Schedule::S0).unwrap();
    let rust = core_shape(&node_params(&Schedule::S0.chain_args()).unwrap());
    assert!(core.len() > 100, "{} leaves", core.len());
    assert_eq!(core.keys().collect::<Vec<_>>(), rust.keys().collect::<Vec<_>>());
}

/// The comparison sees the schedule's options: rdashd's parameters without `-vbparams` differ
/// from Core's with it at the deployment that option sets, and nowhere else. (`check`'s first
/// step is this `differences` and a test for none.)
#[test]
fn a_chain_option_one_side_lacks_is_a_difference() {
    let without: Vec<&str> =
        Schedule::S0.chain_args().into_iter().filter(|a| !a.starts_with("-vbparams=")).collect();
    let core = core_params(Pin::V24_0, Schedule::S0).unwrap();
    let diffs = differences(&core, &core_shape(&node_params(&without).unwrap()));
    assert!(diffs.iter().any(|(k, ..)| k == "consensus.vDeployments.0.nWindowSize"), "{diffs:?}");
    for (key, core, rust) in &diffs {
        assert!(key.starts_with("consensus.vDeployments.0."), "{key}: {core:?} vs {rust:?}");
    }
}

/// The node's own parameters may differ from the compared ones only where `-fastprune` acts:
/// a node that runs with another chain option fails the check.
#[test]
fn the_running_parameters_are_the_compared_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = Schedule::S0.args();
    args.push("-testactivationheight=dip0024@1500");
    let sut = node(dir.path(), &args);
    let message = check(Pin::V24_0, Schedule::S0, sut.params()).unwrap_err();
    assert!(message.contains("consensus.DIP0024Height"), "{message}");
}

/// The `-fastprune` allowance is exact: a node that runs without it fails the check.
#[test]
fn a_node_without_fastprune_fails_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let args: Vec<&str> = Schedule::S0.args().into_iter().filter(|a| *a != "-fastprune").collect();
    let sut = node(dir.path(), &args);
    let message = check(Pin::V24_0, Schedule::S0, sut.params()).unwrap_err();
    assert!(message.contains("pruneAfterHeight"), "{message}");
}

/// Any regtest argument list has its check (`check_args`), as L3's legs run it: here the D49
/// deployments with low boundary heights and no `-fastprune`. A node on fewer of them fails it.
#[test]
fn any_argument_list_has_one_set_of_chain_parameters() {
    let args = [
        "-dip3params=2000:2000",
        "-testactivationheight=v20@2000",
        "-testactivationheight=mn_rr@2000",
        "-testactivationheight=dip0001@120",
        "-testactivationheight=bip147@160",
    ];
    let dir = tempfile::tempdir().unwrap();
    let sut = node(dir.path(), &args);
    check_args(Pin::V24_0, "l3", &args, false, sut.params()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fewer = node(dir.path(), &args[..3]);
    let e = check_args(Pin::V24_0, "l3", &args, false, fewer.params()).unwrap_err();
    assert!(e.starts_with("l3: the node's parameters vs its chain options'"), "{e}");
}

/// `minBlocksToKeep` (oracle ABI 35) on every network: Core's value, the `ChainSetup` rdashd's
/// init builds, and the `ChainPolicy` the node prunes with, `-fastprune` or not. Core:
/// `chainparams.cpp:247, 454, 639` (2304) and `:885` (288) at v24.0.0.
#[test]
fn every_network_has_cores_min_blocks_to_keep() {
    let networks: [(Chain, &[&str], u32); 4] = [
        (Chain::main(), &[], 2304),
        (Chain::test(), &["-testnet"], 2304),
        (Chain::devnet("s7"), &["-devnet=s7"], 2304),
        (Chain::regtest(), &["-regtest"], 288),
    ];
    let oracle = Oracle::new(Pin::V24_0);
    for (chain, args, keep) in networks {
        let json = oracle.chainparams_json(&chain).unwrap();
        let core: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(core["minBlocksToKeep"], Value::from(keep), "Core, {args:?}");
        let network = core["networkID"].as_str().unwrap();
        let rust = network_params(network, args).unwrap();
        assert_eq!(rust.policy.min_blocks_to_keep, keep, "rdashd, {args:?}");
        for fast_prune in [false, true] {
            let policy = ChainPolicy::for_setup(&rust, fast_prune);
            assert_eq!(policy.min_blocks_to_keep, keep, "rdashd policy, {args:?} {fast_prune}");
        }
    }
}

/// `consensus.IPv4OptionalHeight` (oracle ABI 36) on every network: Core's value and the one
/// rdashd's parameters render. Core: `chainparams.cpp:197, 409, 594, 839` at v24.0.0 (not
/// scheduled on main and regtest, 0 on testnet and devnets), and regtest's
/// `-testactivationheight=ipv4_optional@<h>` (`:1089-1090`).
#[test]
fn every_network_has_cores_ipv4_optional_height() {
    const MOVED: &str = "-testactivationheight=ipv4_optional@500";
    let networks: [(Chain, &[&str], i32); 5] = [
        (Chain::main(), &[], i32::MAX),
        (Chain::test(), &["-testnet"], 0),
        (Chain::devnet("s1"), &["-devnet=s1"], 0),
        (Chain::regtest(), &["-regtest"], i32::MAX),
        (Chain::new("regtest", &[MOVED]), &["-regtest", MOVED], 500),
    ];
    let oracle = Oracle::new(Pin::V24_0);
    for (chain, args, height) in networks {
        let json = oracle.chainparams_json(&chain).unwrap();
        let core: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(core["consensus"]["IPv4OptionalHeight"], Value::from(height), "Core, {args:?}");
        let network = core["networkID"].as_str().unwrap();
        let rust = core_shape(&network_params(network, args).unwrap());
        assert_eq!(rust["consensus.IPv4OptionalHeight"], Value::from(height), "rdashd, {args:?}");
    }
}

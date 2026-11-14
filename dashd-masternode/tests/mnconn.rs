use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dash_clock::{AdjustedTime, NodeSeconds};
use dash_evo::spork::SporkId;
use dash_params::{ChainParamsBuilder, LlmqType, LlmqTypeExt, Network, Release};
use dashd_masternode::mnconn::deterministic_outbound;
use dashd_masternode::mnconn::sporks::{all_members_connected, eval_spork, quorum_pose_enabled};
use dashd_masternode::sporks::SporkManager;

fn hash(n: u8) -> [u8; 32] {
    let mut hash = [0; 32];
    hash[0] = n;
    hash
}

// Core: test/llmq_utils_tests.cpp:394-479
#[test]
fn core_deterministic_outbound_properties() {
    let inputs = [hash(0), hash(1), hash(2), hash(3), hash(42)];
    for a in inputs {
        assert_eq!(deterministic_outbound(a, a), a);
        for b in inputs {
            let winner = deterministic_outbound(a, b);
            assert!(winner == a || winner == b);
            assert_eq!(winner, deterministic_outbound(a, b));
            assert_eq!(winner, deterministic_outbound(b, a));
        }
    }
}

#[test]
fn core_outbound_vectors() {
    // Core: llmq/utils.cpp:732-752; recorded by contrib/node/p7-captures/outbound.py.
    let mut high = [0; 32];
    high[31] = 1;
    let inputs = [hash(0), hash(1), hash(2), hash(3), hash(42), high];
    let vectors = [
        (0, 1, 0),
        (0, 2, 0),
        (0, 3, 3),
        (0, 4, 0),
        (0, 5, 5),
        (1, 2, 1),
        (1, 3, 3),
        (1, 4, 1),
        (1, 5, 5),
        (2, 3, 3),
        (2, 4, 2),
        (2, 5, 5),
        (3, 4, 4),
        (3, 5, 5),
        (4, 5, 5),
    ];
    for (a, b, winner) in vectors {
        assert_eq!(deterministic_outbound(inputs[a], inputs[b]), inputs[winner], "{a}, {b}");
        assert_eq!(deterministic_outbound(inputs[b], inputs[a]), inputs[winner], "{b}, {a}");
    }
}

#[test]
fn spork_values_for_every_llmq_type() {
    let types = [
        (LlmqType::Llmq50_60, true),
        (LlmqType::Llmq400_60, false),
        (LlmqType::Llmq400_85, false),
        (LlmqType::Llmq100_67, false),
        (LlmqType::Llmq60_75, true),
        (LlmqType::Llmq25_67, true),
        (LlmqType::LlmqTest, true),
        (LlmqType::LlmqDevnet, true),
        (LlmqType::LlmqTestV17, true),
        (LlmqType::LlmqTestDip0024, true),
        (LlmqType::LlmqTestInstantsend, true),
        (LlmqType::LlmqDevnetDip0024, true),
        (LlmqType::LlmqTestPlatform, true),
        (LlmqType::LlmqDevnetPlatform, true),
        (LlmqType::LlmqNone, true),
        (LlmqType::Unknown(0), true),
        (LlmqType::Unknown(254), true),
    ];
    for llmq_type in LlmqType::AVAILABLE {
        assert!(types.iter().any(|(t, _)| *t == llmq_type));
    }
    for (llmq_type, one_enabled) in types {
        for (value, expected) in [
            (0, true),
            (1, one_enabled),
            (-1, false),
            (2, false),
            (4_070_908_800, false),
            (i64::MIN, false),
            (i64::MAX, false),
        ] {
            assert_eq!(eval_spork(llmq_type, value), expected, "{llmq_type:?}, {value}");
        }
    }
}

struct Time(AtomicI64);

impl AdjustedTime for Time {
    fn now(&self) -> NodeSeconds {
        NodeSeconds(self.0.load(Ordering::SeqCst))
    }
}

#[test]
fn quorum_sporks_read_the_managers_values() {
    let time = Arc::new(Time(AtomicI64::new(1000)));
    for network in [Network::Main, Network::Regtest] {
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().unwrap();
        let sporks = SporkManager::new(&params, time.clone());
        for llmq_type in LlmqType::AVAILABLE {
            assert_eq!(
                all_members_connected(llmq_type, &sporks),
                network == Network::Main && eval_spork(llmq_type, 1)
            );
            assert_eq!(quorum_pose_enabled(llmq_type, &sporks), network == Network::Main);
        }
        if network == Network::Main {
            continue;
        }
        sporks.set_priv_key("cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK").unwrap();
        for (connections, pose) in [(0, 2), (1, 0), (2, 1), (4_070_908_800, -1)] {
            time.0.fetch_add(1, Ordering::SeqCst);
            sporks.update_spork(SporkId::SPORK_21_QUORUM_ALL_CONNECTED, connections).unwrap();
            sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, pose).unwrap();
            for llmq_type in LlmqType::AVAILABLE {
                assert_eq!(
                    all_members_connected(llmq_type, &sporks),
                    eval_spork(llmq_type, connections)
                );
                assert_eq!(quorum_pose_enabled(llmq_type, &sporks), eval_spork(llmq_type, pose));
            }
        }
    }
}

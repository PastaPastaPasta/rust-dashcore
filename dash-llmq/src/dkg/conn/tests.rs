//! T1 for the bad-connection inputs: `EvalSpork`'s table and each branch of
//! `VerifyConnectionAndMinProtoVersions` (`a/dkgsession.cpp:226-290`).

use super::*;

fn pro_tx(n: u8) -> ProTxHash {
    ProTxHash::from([n; 32])
}

/// A fully connected node of `who`.
fn node(who: ProTxHash, version: i32, mn: bool, relay: bool, filters: bool) -> ConnFacts {
    ConnFacts {
        verified_pro_tx: who,
        version,
        masternode_connection: mn,
        relay_txs: relay,
        services: if filters {
            NODE_COMPACT_FILTERS | 1
        } else {
            1 | 1 << 10
        },
    }
}

fn snapshot(nodes: Vec<ConnFacts>) -> ConnSnapshot {
    ConnSnapshot {
        nodes,
        ..ConnSnapshot::connected([])
    }
}

const V: i32 = MIN_MASTERNODE_PROTO_VERSION;

#[test]
fn eval_spork_is_cores_table() {
    use LlmqType::*;
    let excluded = [Llmq100_67, Llmq400_60, Llmq400_85];
    let others = [LlmqTest, Llmq50_60, Llmq60_75, Llmq25_67, LlmqTestDip0024, LlmqTestV17];
    for t in excluded.into_iter().chain(others) {
        assert!(eval_spork(t, 0), "{t:?}: 0 enables every type");
        assert_eq!(eval_spork(t, 1), !excluded.contains(&t), "{t:?}: 1");
        for value in [2, -1, 4_070_908_800, i64::MAX] {
            assert!(!eval_spork(t, value), "{t:?}: {value}");
        }
    }
}

#[test]
fn a_missing_member_is_a_bad_connection_only_when_all_must_be_connected() {
    let mut conn = snapshot(vec![node(pro_tx(2), V, true, true, true)]);
    assert_eq!(conn.faults(&pro_tx(1)), [ConnFault::NotConnected]);
    assert_eq!(conn.faults(&pro_tx(2)), []);
    conn.all_members_connected = false;
    assert_eq!(conn.faults(&pro_tx(1)), []);
}

#[test]
fn a_node_without_a_verified_pro_tx_counts_for_nobody() {
    // Kills a fold that keeps unverified nodes: the null hash would then be "connected".
    let null = ProTxHash::from([0; 32]);
    let conn = snapshot(vec![node(null, V, true, true, true)]);
    assert_eq!(conn.faults(&null), [ConnFault::NotConnected]);
}

#[test]
fn the_first_nodes_version_decides() {
    let who = pro_tx(1);
    let low_first =
        snapshot(vec![node(who, V - 1, true, true, true), node(who, V, true, true, true)]);
    assert_eq!(low_first.faults(&who), [ConnFault::MinProtoVersion(V - 1)]);
    let low_last =
        snapshot(vec![node(who, V, true, true, true), node(who, V - 1, true, true, true)]);
    assert_eq!(low_last.faults(&who), []);
    // The minimum itself passes.
    assert_eq!(snapshot(vec![node(who, V, false, false, false)]).faults(&who), []);
}

#[test]
fn only_masternode_connections_are_judged_for_relay_and_filters() {
    let who = pro_tx(1);
    // No masternode connection: a block-relay-only peer advertises relay=0 and is fine.
    assert_eq!(snapshot(vec![node(who, V, false, false, false)]).faults(&who), []);
    // A non-masternode connection's relay and filters do not count.
    let conn = snapshot(vec![node(who, V, false, true, true), node(who, V, true, false, false)]);
    assert_eq!(conn.faults(&who), [ConnFault::NoTxRelay]);
    let conn = snapshot(vec![node(who, V, false, true, true), node(who, V, true, true, false)]);
    assert_eq!(conn.faults(&who), [ConnFault::NoCompactFilters]);
    // One masternode connection per service is enough.
    let conn = snapshot(vec![node(who, V, true, true, false), node(who, V, true, false, true)]);
    assert_eq!(conn.faults(&who), []);
}

#[test]
fn the_fold_checks_are_an_else_if_chain_and_the_metadata_checks_independent() {
    let who = pro_tx(1);
    let mut conn = snapshot(vec![node(who, V - 1, true, false, false)]);
    assert_eq!(conn.faults(&who), [ConnFault::MinProtoVersion(V - 1)]);
    conn.nodes = vec![node(who, V, true, false, false)];
    assert_eq!(conn.faults(&who), [ConnFault::NoTxRelay]);
    conn.outbound_failed_too_many.insert(who);
    conn.platform_banned.insert(who);
    assert_eq!(
        conn.faults(&who),
        [ConnFault::NoTxRelay, ConnFault::OutboundFailed, ConnFault::PlatformBanned]
    );
    conn.nodes.clear();
    assert_eq!(
        conn.faults(&who),
        [ConnFault::NotConnected, ConnFault::OutboundFailed, ConnFault::PlatformBanned]
    );
    conn.all_members_connected = false;
    assert_eq!(conn.faults(&who), [ConnFault::OutboundFailed, ConnFault::PlatformBanned]);
    assert_eq!(conn.faults(&pro_tx(2)), []);
}

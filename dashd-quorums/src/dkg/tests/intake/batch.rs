//! `ProcessPendingMessageBatch` and `RelayInvToParticipants` (`l/net_dkg.cpp:336-458`,
//! v24.0.0) over a reference network's sessions: the scores (none for our own messages), the
//! relays' peer choice, and the order of the lines, scores and pushes.

use dash_evo::llmq::MnEntryView;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::reference::{CoreRelayTopology, Network, RecordingDebug};
use dash_llmq::dkg::session::{DkgMsgKind, DkgSession, Sender, SessionInit};
use dash_llmq::qdata::PeerId;

use super::super::fakes::{logged, params};
use super::fixture::{FakeHost, TEST, network, pro_tx};
use crate::dkg::intake::process_pending_batch;
use crate::dkg::log::uint256_hex;
use crate::dkg::seams::{RelayFacts, RelayPeer};

fn peer(id: PeerId, verified: Option<ProTxHash>, qwatch: bool) -> RelayPeer {
    RelayPeer {
        id,
        addr_name: format!("10.0.0.{id}:9999"),
        verified_pro_tx: verified,
        qwatch,
    }
}

fn short(p: &ProTxHash) -> String {
    p.to_string()[..4].to_string()
}

/// [`network`] whose member 0 has Core's relay members: the reference mesh gives none.
fn network_with_relays(seed: u64) -> Network {
    let mut net = network(seed);
    let members = net.members.clone();
    let mut topo = CoreRelayTopology {
        members: members.iter().map(|m| m.pro_tx_hash).collect(),
        calls: 0,
    };
    let params = params(TEST);
    let init = SessionInit {
        llmq_type: TEST,
        params: &params,
        base: net.setup.base,
        quorum_index: net.setup.quorum_index,
        members: members.clone().into(),
        active_pro_tx: Some(members[0].pro_tx_hash),
        rotation_enabled: net.setup.rotation_enabled,
        v19_after_base: net.setup.v19_after_base,
    };
    let mut debug = RecordingDebug::default();
    let mut session = DkgSession::init(init, &mut topo, &mut debug).expect("enough members");
    session.take_events();
    net.nodes[0].session = session;
    net
}

#[test]
fn each_received_message_is_relayed_to_watchers_and_relay_members_only() {
    let mut net = network_with_relays(0x2001);
    let c1 = net.contribute(1).expect("a contribution");
    let c2 = net.contribute(2).expect("a contribution");
    let bls = net.bls;
    let node = &mut net.nodes[0];
    let relay: Vec<ProTxHash> = node.session.relay_members().iter().copied().collect();
    assert!(!relay.is_empty());
    let me = *node.session.members()[0].pro_tx_hash();
    let outsider = pro_tx(0xee);
    let host = FakeHost::default();
    *host.facts.lock().unwrap() = RelayFacts {
        node_count: 9,
        network_active: true,
        has_quorum_nodes: false,
        peers: vec![
            peer(1, Some(relay[0]), false),
            peer(2, None, true),
            peer(3, None, false),
            peer(4, Some(outsider), false),
            peer(5, Some(outsider), true),
            peer(6, Some(ProTxHash::default()), false),
        ],
    };
    let batch = vec![
        (Sender::Peer(11), c1.payload(bls.scheme())),
        (Sender::Peer(12), c2.payload(bls.scheme())),
    ];
    let ((), lines) = logged(|| {
        node.step(bls, |s, env| {
            process_pending_batch(&host, s, env, DkgMsgKind::Contribution, &batch);
        })
    });
    let pushes = ["push 1 Contribution", "push 2 Contribution", "push 5 Contribution"];
    let one: Vec<String> = ["facts 100"].into_iter().chain(pushes).map(String::from).collect();
    assert_eq!(host.take(), [one.clone(), one].concat());

    let records: Vec<&str> = lines
        .iter()
        .filter(|l| l.target == "llmq-dkg" && l.text.starts_with("    RelayInvToParticipants"))
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(records.len(), 2);
    let members: String = relay.iter().map(|r| format!("{} | ", short(r))).collect();
    let base = net.setup.base.hash;
    let want = format!(
        "    RelayInvToParticipants inv[qcontrib {}] relayMembers[{}] GetNodeCount[9] \
         GetNetworkActive[1] HasMasternodeQuorumNodes[0] for quorumHash[{}] forMember[{}] \
         relayMembers[{members}]\n    node[2:10.0.0.2:9999] not mn\n    node[3:10.0.0.3:9999] not mn\n    \
         node[6:10.0.0.6:9999] not mn\n    forMember[{}] NOTrelayMembers[{} | {} | ]",
        uint256_hex(c1.session_hash()),
        relay.len(),
        uint256_hex(base.as_bytes()),
        short(&me),
        short(&me),
        short(&outsider),
        short(&outsider),
    );
    assert_eq!(records[0], want);
    assert!(records[1].contains(&uint256_hex(c2.session_hash())));
}

#[test]
fn scores_are_100_per_bad_peer_message_and_none_for_our_own() {
    let mut net = network(0x2002);
    let c1 = net.contribute(1).expect("a contribution");
    let c2 = net.contribute(2).expect("a contribution");
    let bls = net.bls;
    let mut forged = c2.payload(bls.scheme());
    let sig_at = forged.len() - 96;
    forged[sig_at..].copy_from_slice(&c1.payload(bls.scheme())[sig_at..]);
    let host = FakeHost::default();
    let batch = vec![
        (Sender::Peer(9), vec![1, 2, 3]),
        (Sender::Own, vec![4, 5]),
        (Sender::Peer(11), c1.payload(bls.scheme())),
        (Sender::Peer(12), forged),
    ];
    let ((), lines) = logged(|| {
        net.nodes[0].step(bls, |s, env| {
            process_pending_batch(&host, s, env, DkgMsgKind::Contribution, &batch);
        })
    });
    let calls = host.take();
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("score")).collect::<Vec<_>>(),
        ["score 9 100", "score 12 100"]
    );
    assert_eq!(calls.iter().filter(|c| c.starts_with("facts")).count(), 1, "c1 alone is relayed");

    let order: Vec<&str> = lines
        .iter()
        .filter_map(|l| {
            let t = l.text.as_str();
            [
                ("deserialize, peer=9", t.contains("failed to deserialize message, peer=9")),
                ("score 9", t == "score 9 100"),
                ("deserialize, peer=-1", t.contains("failed to deserialize message, peer=-1")),
                ("signature, peer=12", t.contains("failed to verify signature, peer=12")),
                ("score 12", t == "score 12 100"),
                ("received", t.contains("received and relayed contribution")),
                ("facts", t == "facts 100"),
                ("relay", t.starts_with("    RelayInvToParticipants")),
            ]
            .into_iter()
            .find_map(|(name, hit)| hit.then_some(name))
        })
        .collect();
    assert_eq!(
        order,
        [
            "deserialize, peer=9",
            "score 9",
            "deserialize, peer=-1",
            "signature, peer=12",
            "score 12",
            "received",
            "facts",
            "relay"
        ]
    );
}

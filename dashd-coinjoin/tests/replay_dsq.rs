#![cfg(feature = "testing")]
//! Replays of the non-masternode DSQ fixtures through the Sim and [`DsqRuntime`]. The
//! masternode DSQ fixtures (`dsq-modes`, `dsq-server-intake`) run through the server runtime in
//! `replay_server.rs`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use common::replay::{
    Scores, Source, StepRule, Streams, check_mask, compare_streams, disconnected, dsq_projection,
    enabled_count, inventory_projection, is_dsq_inv, is_dsq_inv_payload, masternodes,
    pin_queue_hashes, queue_aliases, recorded_streams, step_drivers, steps, wire_hex,
};
use common::transcript::{Direction, Record};
use dash_coinjoin::queue::{MixingCounters, check_queue_signature};
use dash_p2p::conn::Permissions;
use dash_pkc::bls::core::BlsVerdict;
use dashd_coinjoin::dsq::DsqRuntime;
use dashd_coinjoin::testing::{FakeChainView, FakeMnLists};

fn tracked(command: &str, payload: &[u8]) -> bool {
    matches!(command, "dsq" | "notfound") || (command == "inv" && is_dsq_inv_payload(payload))
}

/// Replays a non-masternode wallet fixture record by record: each peer connects with its
/// recorded `version` and `senddsq`, every message goes through the Sim's gates and processor,
/// and each step's scores and closed connections are compared with Core's before the next step.
/// At the end every peer's `dsq`/`inv`/`notfound` stream is compared with Core's, both ways,
/// each message in the step whose span holds Core's (see [`StepRule::Direct`]).
fn replay_wallet(name: &str, blocks_only: bool) {
    let fixture = common::transcript::fixture(name);
    check_mask(&fixture);
    pin_queue_hashes(&fixture);
    assert_eq!(fixture.facts["setup"]["blockchain_synced"].as_bool(), Some(!fixture.ibd));
    let list = masternodes(&fixture);
    let mut mns = FakeMnLists {
        enabled: enabled_count(&fixture),
        ..FakeMnLists::default()
    };
    for mn in &list {
        mns.by_collateral.insert(mn.collateral, mn.clone());
        mns.valid.insert(mn.pro_tx_hash, mn.clone());
    }
    let runtime = DsqRuntime::new(
        !blocks_only,
        Arc::new(FakeChainView {
            synced: !fixture.ibd,
            ..FakeChainView::default()
        }),
        Arc::new(mns),
        Arc::new(Mutex::new(MixingCounters::default())),
    );
    let mut sim = common::replay::sim(&fixture, runtime.clone(), false, blocks_only);
    let mut peers = BTreeMap::new();
    let mut streams = Streams::default();
    let mut scores = Scores::default();
    let mut closed = BTreeSet::new();
    let steps = steps(&fixture);
    for (index, step) in steps.iter().enumerate() {
        let at = format!("{name}:{}", step.line);
        match step.record {
            Record::Accept {
                peer,
                ..
            } => {
                let accepted = sim.accept(u8::try_from(*peer).unwrap(), Permissions::NONE);
                peers.insert(*peer, accepted.unwrap_or_else(|| panic!("{at}: accept")));
            }
            Record::Message(message) => {
                assert_eq!(message.direction, Direction::ToNode);
                let peer = &peers[&message.peer];
                // A pong echoes the node's ping nonce; dashd closed connections read nothing more.
                if !message.echo && !peer.handle.disconnect_requested() {
                    sim.deliver(peer, &[(message.command.as_str(), message.payload.clone())]);
                }
            }
            other => panic!("{at}: no wallet fixture drives {other:?}"),
        }
        sim.pass();
        streams.take_sim(&sim, &peers, index, Source::Sim, tracked);
        assert_eq!(scores.deltas(&sim, &peers), step.score_deltas(), "{at}: score changes");
        closed.extend(step.closed());
        assert_eq!(disconnected(&peers), closed, "{at}: closed peers");
    }

    // A non-masternode never relays a queue: it announces none and pushes none unasked.
    for (peer, outs) in &streams.0 {
        assert!(
            outs.iter().all(|out| out.command != "inv"),
            "{name}: the wallet announced a queue to peer {peer}"
        );
    }
    let aliases = queue_aliases(&fixture);
    let alias = |inv: &dash_p2p_core::Inventory| aliases.get(&wire_hex(inv)).cloned();
    let verify = |queue: &dash_p2p_core::DsQueue| {
        list.iter().find(|mn| mn.pro_tx_hash == queue.pro_tx_hash()).is_some_and(|mn| {
            matches!(check_queue_signature(queue, &mn.operator_key), BlsVerdict::Valid)
        })
    };
    let project = |command: &str, payload: &[u8]| match command {
        "dsq" => dsq_projection(payload, alias, verify),
        _ => inventory_projection(payload, alias),
    };
    let recorded = recorded_streams(&fixture, |message| {
        matches!(message.command.as_str(), "dsq" | "notfound") || is_dsq_inv(message)
    });
    compare_streams(
        name,
        &recorded,
        &streams,
        StepRule::Direct(&step_drivers(&steps)),
        |entry, out| {
            entry.command == out.command && project(&out.command, &out.payload) == entry.value
        },
        |entry, out, context| {
            assert_eq!(project(&out.command, &out.payload), entry.value, "{context}");
        },
    );
}

#[test]
fn wallet_intake_replay_differential() {
    replay_wallet("dsq-wallet-intake", false);
}

#[test]
fn wallet_threshold_replay_differential() {
    replay_wallet("dsq-wallet-threshold", false);
}

#[test]
fn blocksonly_replay_differential() {
    replay_wallet("dsq-blocksonly", true);
}

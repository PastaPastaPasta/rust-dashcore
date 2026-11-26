//! `ProcessPendingMessageBatch` around [`DkgSession::process_batch`], and
//! `RelayInvToParticipants`.

use dash_evo::llmq::OperatorKeyView;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::session::{DkgEnv, DkgInv, DkgMsgKind, DkgSession, Misbehaviour, Sender};
use dash_llmq::qdata::PeerId;

use super::message::command;
use crate::dkg::log::{log_record, uint256_hex};
use crate::dkg::round::log_events;
use crate::dkg::seams::IntakeHost;

/// `ProcessPendingMessageBatch` over one pop, `batch`, on the phase thread: the session's
/// read, pre-verification, signature check and receive; each score is `PeerMisbehaving(peer,
/// 100)` (none for [`Sender::Own`]), each inv a [`relay_inv_to_participants`], and the
/// session's lines are logged with both where Core runs them.
///
/// The sessions stop before V at shutdown (L3's order); the batch reads no members beyond the
/// session's own list and relay set.
// Core: v24.0.0 src/llmq/net_dkg.cpp:404-458 ProcessPendingMessageBatch
pub fn process_pending_batch<E: OperatorKeyView>(
    host: &dyn IntakeHost,
    session: &mut DkgSession<E>,
    env: &mut DkgEnv<'_>,
    kind: DkgMsgKind,
    batch: &[(Sender<PeerId>, Vec<u8>)],
) {
    let refs: Vec<(Sender<PeerId>, &[u8])> =
        batch.iter().map(|(s, p)| (*s, p.as_slice())).collect();
    let outcome = session.process_batch(kind, &refs, env);
    let mut scores = outcome.misbehaving.iter().peekable();
    let mut relays = outcome.relay.iter().peekable();
    let mut done = 0;
    loop {
        while let Some(scored) = scores.next_if(|s| s.at <= done) {
            let (Misbehaviour::Decode {
                peer,
                ..
            }
            | Misbehaviour::PreVerifyBan {
                peer,
                ..
            }
            | Misbehaviour::BadSignature {
                peer,
            }) = scored.misbehaviour;
            host.misbehaving(peer, 100);
        }
        while let Some(relayed) = relays.next_if(|r| r.at <= done) {
            relay_inv_to_participants(host, session, relayed.inv);
        }
        if done >= outcome.events.len() {
            break;
        }
        let next = scores.peek().map(|s| s.at).into_iter().chain(relays.peek().map(|r| r.at)).min();
        let end = next.unwrap_or(usize::MAX).min(outcome.events.len());
        log_events(&outcome.events[done..end]);
        done = end;
    }
}

/// The first four characters of `uint256::ToString()`.
fn short(pro_tx: &ProTxHash) -> String {
    pro_tx.to_string().chars().take(4).collect()
}

/// `RelayInvToParticipants`: pushes `inv` to each fully connected peer that watches quorums or
/// whose verified proTxHash is one of the session's relay members, and logs one record.
// Core: v24.0.0 src/llmq/net_dkg.cpp:336-368 RelayInvToParticipants
pub fn relay_inv_to_participants<E: OperatorKeyView>(
    host: &dyn IntakeHost,
    session: &DkgSession<E>,
    inv: DkgInv,
) {
    let relay = session.relay_members();
    let me =
        session.my_index().map_or_else(ProTxHash::default, |i| *session.members()[i].pro_tx_hash());
    let base = session.base().hash;
    let facts = host.relay_facts(session.llmq_type(), &base);
    let members: String = relay.iter().map(|r| format!("{} | ", short(r))).collect();
    let mut lines = vec![format!(
        "RelayInvToParticipants inv[{} {}] relayMembers[{}] GetNodeCount[{}] GetNetworkActive[{}] \
         HasMasternodeQuorumNodes[{}] for quorumHash[{}] forMember[{}] relayMembers[{members}]",
        command(inv.kind),
        uint256_hex(&inv.hash),
        relay.len(),
        facts.node_count,
        u8::from(facts.network_active),
        u8::from(facts.has_quorum_nodes),
        uint256_hex(base.as_bytes()),
        short(&me),
    )];
    let mut not_relay = String::new();
    for peer in &facts.peers {
        let verified = peer.verified_pro_tx.filter(|p| !p.is_null());
        if peer.qwatch || verified.is_some_and(|p| relay.contains(&p)) {
            host.push_inventory(peer.id, inv);
        }
        match verified {
            None => lines.push(format!("node[{}:{}] not mn", peer.id, peer.addr_name)),
            Some(p) if !relay.contains(&p) => not_relay.push_str(&format!("{} | ", short(&p))),
            Some(_) => {}
        }
    }
    lines.push(format!("forMember[{}] NOTrelayMembers[{not_relay}]", short(&me)));
    log_record(lines);
}

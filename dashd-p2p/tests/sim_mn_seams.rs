//! The processor's masternode seams in `sim` (BL-702 §5, A1–A6): `mnauth` after our `verack` on a
//! masternode (np:4214-4216, evo/mnauth.cpp:19-49), `qwatch` inside the `!RejectIncomingTxs` block
//! (np:4243-4250), and the list seam every handler context carries for the `mnauth` intake
//! (np:5801). Order checks are relative: other messages may come between.

use std::sync::{Arc, Mutex};

use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, MnAuth};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{
    MasternodeNet, MasternodeSeams, MnAuthChallenge, MnAuthSigner, MnListView, OperatorKeyRef,
    TipFacts,
};
use dashd_p2p::sim::masternode::{FakeMnList, FakeMnNet, FakeSigner, sample_mnauth};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams};

const MNAUTH_CMD: [CommandString; 1] = [CommandString::from_static("mnauth")];
/// A `-pushversion` that differs from `PROTOCOL_VERSION` (70242).
const PUSH_VERSION: i32 = 70240;
/// The challenge our scripted peers send in their `version`.
const CHALLENGE: [u8; 32] = [0x5a; 32];
/// The masternode the lists know.
const PRO: [u8; 32] = [0xc1; 32];
const MSG_TX: u32 = 1;

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn key(n: u8) -> OperatorKeyRef {
    OperatorKeyRef {
        pubkey: [n; 48],
        pubkey_hash: [n; 32],
    }
}

fn list(n: u8) -> Arc<dyn MnListView> {
    Arc::new(FakeMnList::new().with(PRO, key(n)))
}

struct Node {
    sim: Sim,
    net: Arc<FakeMnNet>,
    signer: Option<Arc<FakeSigner>>,
}

/// A synced seam with list 1 published.
fn synced_net() -> Arc<FakeMnNet> {
    Arc::new(FakeMnNet::new(true, list(1)))
}

/// A node at `-pushversion=70240` over `net`, with the signer when `signer`, and `registry`. The
/// identity says masternode when `caller_mn`, which `Shared::new` overrides.
fn node(net: Arc<FakeMnNet>, signer: bool, caller_mn: bool, registry: Registry) -> Node {
    let p = params();
    let opts = IdentityOptions {
        push_version: Some(PUSH_VERSION),
        ..IdentityOptions::new()
    };
    let mut id = local_identity(&p, &opts).unwrap();
    id.masternode = caller_mn;
    let signer = signer.then(|| Arc::new(FakeSigner::new(Some(sample_mnauth(7)))));
    let mn = MasternodeSeams {
        net: Arc::clone(&net) as Arc<dyn MasternodeNet>,
        signer: signer.clone().map(|s| s as Arc<dyn MnAuthSigner>),
    };
    let sim = Sim::with_masternode(
        p,
        id,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        mn,
        |_| {},
    );
    Node {
        sim,
        net,
        signer,
    }
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn encode<T: BaseCodec>(m: &T) -> Vec<u8> {
    let mut out = Vec::new();
    m.encode(&mut out);
    out
}

fn with_challenge(challenge: [u8; 32]) -> PeerVersionMsg {
    PeerVersionMsg {
        mnauth_challenge: challenge,
        ..PeerVersionMsg::v24(SIM_START)
    }
}

type Sent = Vec<(CommandString, Vec<u8>)>;

/// Runs the handshake from `p` with `v`; returns the commands the node sent, and the messages.
fn handshake_peer(sim: &mut Sim, p: &SimPeer, v: PeerVersionMsg) -> (Vec<CommandString>, Sent) {
    let mut sent = sim.handshake(p, v);
    settle(sim);
    sent.extend(sim.sent(p));
    (sent.iter().map(|(c, _)| *c).collect(), sent)
}

/// Connects peer `n` as `conn_type` and runs [`handshake_peer`].
fn handshake(
    sim: &mut Sim,
    n: u8,
    conn_type: ConnectionType,
    v: PeerVersionMsg,
) -> (SimPeer, Vec<CommandString>, Sent) {
    let p = sim.connect(n, conn_type, Permissions::NONE);
    let (cmds, sent) = handshake_peer(sim, &p, v);
    (p, cmds, sent)
}

fn position(cmds: &[CommandString], cmd: &str) -> Option<usize> {
    cmds.iter().position(|c| c.as_str() == cmd)
}

fn has(cmds: &[CommandString], cmd: &str) -> bool {
    position(cmds, cmd).is_some()
}

/// Whether `cx` carries `net` as its masternode seam.
fn carries(cx: &HandlerCx<'_>, net: &Arc<FakeMnNet>) -> bool {
    std::ptr::addr_eq(cx.mn_net as *const dyn MasternodeNet, Arc::as_ptr(net))
}

/// A1: a masternode pushes the signer's `mnauth` after our `verack` and before `sendheaders`, and
/// the signer sees the peer's challenge, our side of the connection and `-pushversion`.
#[test]
fn a1_signer_mnauth_follows_verack_before_sendheaders() {
    let mut node = node(synced_net(), true, false, Registry::new());
    assert!(node.sim.shared.identity().masternode, "a signer makes the node a masternode");
    let (_p, cmds, sent) =
        handshake(&mut node.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
    let at = position(&cmds, "mnauth").expect("an mnauth");
    let verack = position(&cmds, "verack").unwrap();
    let headers = position(&cmds, "sendheaders2")
        .or_else(|| position(&cmds, "sendheaders"))
        .expect("a sendheaders");
    assert!(verack < at && at < headers, "{cmds:?}");
    let got = MnAuth::decode(&mut &sent[at].1[..]).unwrap();
    assert_eq!(got, sample_mnauth(7));
    let (_q, cmds, _) =
        handshake(&mut node.sim, 2, ConnectionType::OutboundFullRelay, with_challenge(CHALLENGE));
    assert!(has(&cmds, "mnauth"), "{cmds:?}");
    assert_eq!(
        node.signer.as_ref().unwrap().challenges(),
        [
            MnAuthChallenge {
                received: CHALLENGE,
                inbound: true,
                our_version: PUSH_VERSION,
            },
            MnAuthChallenge {
                received: CHALLENGE,
                inbound: false,
                our_version: PUSH_VERSION,
            },
        ]
    );
}

/// A2: no `mnauth` to a masternode probe, none for an all-zero challenge (mnauth.cpp:26-29), and
/// none without a signer, whatever the caller's identity said; the signer is never asked.
#[test]
fn a2_no_mnauth_to_probes_null_challenges_or_without_signer() {
    let mut mn = node(synced_net(), true, false, Registry::new());
    let signer = mn.signer.clone().unwrap();
    let probe = mn.sim.connect_probe(1);
    let (cmds, _) = handshake_peer(&mut mn.sim, &probe, with_challenge(CHALLENGE));
    assert!(has(&cmds, "verack"), "{cmds:?}");
    assert!(!has(&cmds, "mnauth"), "probe: {cmds:?}");
    assert_eq!(signer.calls(), 0, "probe");

    let (_p, cmds, _) =
        handshake(&mut mn.sim, 2, ConnectionType::Inbound, PeerVersionMsg::v24(SIM_START));
    assert!(has(&cmds, "verack"));
    assert!(!has(&cmds, "mnauth"), "zero challenge: {cmds:?}");
    assert_eq!(signer.calls(), 0, "zero challenge");

    let mut plain = node(synced_net(), false, true, Registry::new());
    assert!(!plain.sim.shared.identity().masternode, "no signer, no masternode");
    let (_p, cmds, _) =
        handshake(&mut plain.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
    assert!(has(&cmds, "verack"));
    assert!(!has(&cmds, "mnauth"), "no signer: {cmds:?}");
}

/// BL-705 test 10: a masternode probe (the sim's `connect_probe`, with the masternode and probe
/// flags the loop's opens carry), whose first message after `verack` is not `mnauth`, is
/// disconnected (net_processing.cpp:4386-4396), and an outbound masternode connection that is
/// not a probe keeps the same message. The MNAUTH-first case needs AM1's `mnauth` handler.
#[test]
fn bl705_a_probe_whose_first_message_is_not_mnauth_is_disconnected() {
    let mut mn = node(synced_net(), false, false, Registry::new());
    let ping = || ("ping", 7u64.to_le_bytes().to_vec());
    let probe = mn.sim.connect_probe(1);
    assert!(probe.handle.masternode_probe());
    let (cmds, _) = handshake_peer(&mut mn.sim, &probe, with_challenge(CHALLENGE));
    assert!(has(&cmds, "verack"), "{cmds:?}");
    assert!(!probe.handle.disconnect_requested(), "the handshake is not the first message");
    assert_eq!(probe.handle.first_message_time(), 0);
    mn.sim.deliver(&probe, &[ping()]);
    settle(&mut mn.sim);
    assert!(probe.handle.disconnect_requested(), "a probe's first message must be mnauth");
    assert_ne!(probe.handle.first_message_time(), 0);
    assert!(!probe.handle.first_message_is_mnauth());

    let service = dash_p2p::Service::new(dash_p2p::NetAddr::ipv4([10, 0, 0, 2]), 19899);
    let plain = mn.sim.attach_at(
        service,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        dash_p2p::ServiceFlags::NODE_NETWORK,
        true,
        false,
    );
    assert!(plain.handle.masternode_connection() && !plain.handle.masternode_probe());
    let (cmds, _) = handshake_peer(&mut mn.sim, &plain, with_challenge(CHALLENGE));
    assert!(has(&cmds, "verack"), "{cmds:?}");
    mn.sim.deliver(&plain, &[ping()]);
    settle(&mut mn.sim);
    assert!(!plain.handle.disconnect_requested(), "not a probe: kept");
    assert_ne!(plain.handle.first_message_time(), 0);
}

/// A3: `qwatch` comes after `senddsq` when the seam wants it, with or without a signer
/// (`-watchquorums`); a block-relay peer gets neither and the seam is not asked.
#[test]
fn a3_qwatch_after_senddsq_inside_the_reject_block() {
    for signer in [false, true] {
        let mut node = node(synced_net(), signer, false, Registry::new());
        node.net.set_wants_qwatch(true);
        let (_p, cmds, _) =
            handshake(&mut node.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
        let dsq = position(&cmds, "senddsq").expect("a senddsq");
        let qwatch = position(&cmds, "qwatch").expect("a qwatch");
        assert!(dsq < qwatch, "signer {signer}: {cmds:?}");
        assert_eq!(node.net.wants_qwatch_calls(), 1);

        let (_b, cmds, _) =
            handshake(&mut node.sim, 2, ConnectionType::BlockRelay, with_challenge(CHALLENGE));
        assert!(has(&cmds, "verack"), "{cmds:?}");
        assert!(!has(&cmds, "senddsq") && !has(&cmds, "qwatch"), "block relay: {cmds:?}");
        assert_eq!(node.net.wants_qwatch_calls(), 1, "block relay: not asked");

        node.net.set_wants_qwatch(false);
        let (_q, cmds, _) =
            handshake(&mut node.sim, 3, ConnectionType::Inbound, with_challenge(CHALLENGE));
        assert!(has(&cmds, "senddsq") && !has(&cmds, "qwatch"), "not wanted: {cmds:?}");
    }
}

/// Records, for each call, whether the context carried the installed seam.
struct SeamProbe {
    net: Arc<FakeMnNet>,
    messages: Arc<Mutex<Vec<bool>>>,
    getdata: Arc<Mutex<Vec<bool>>>,
}

impl NetHandler for SeamProbe {
    fn commands(&self) -> &'static [CommandString] {
        &MNAUTH_CMD
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        self.messages.lock().unwrap().push(carries(cx, &self.net));
        // The intake's one read per message.
        let _ = cx.mn_net.tip_list();
        Ok(())
    }

    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, _inv: &Inventory) -> bool {
        self.getdata.lock().unwrap().push(carries(cx, &self.net));
        true
    }
}

type Carried = Arc<Mutex<Vec<bool>>>;

/// A node without a signer whose registry has a [`SeamProbe`] on `mnauth`.
fn probe_node() -> (Node, Carried, Carried) {
    let messages = Carried::default();
    let getdata = Carried::default();
    let net = synced_net();
    let mut reg = Registry::new();
    reg.register(Box::new(SeamProbe {
        net: Arc::clone(&net),
        messages: Arc::clone(&messages),
        getdata: Arc::clone(&getdata),
    }));
    (node(net, false, false, reg), messages, getdata)
}

fn tx_inv(n: u8) -> Inventory {
    Inventory::new(InvType::new(MSG_TX), Hash256::from_lendian([n; 32]))
}

/// A4: both handler-context builders a registry handler runs in pass the installed seam: the
/// message dispatch and `ProcessGetData`'s, once per entry. (The headers continuation's context
/// sets it too, but runs no handler.)
#[test]
fn a4_dispatch_and_getdata_contexts_carry_the_installed_seam() {
    let (mut node, messages, getdata) = probe_node();
    let (p, _, _) = handshake(&mut node.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
    node.sim.deliver(&p, &[("mnauth", encode(&sample_mnauth(3)))]);
    settle(&mut node.sim);
    node.sim.deliver(&p, &[("getdata", encode(&GetData::new(vec![tx_inv(1), tx_inv(2)])))]);
    settle(&mut node.sim);
    assert_eq!(*messages.lock().unwrap(), [true]);
    assert_eq!(*getdata.lock().unwrap(), [true, true]);
}

/// What [`ListProbe`] saw for [`PRO`] in one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reads {
    /// From the list the message read first.
    first: Option<OperatorKeyRef>,
    /// From a second, fresh read.
    fresh: Option<OperatorKeyRef>,
    /// From the first list again, after the fresh read.
    kept: Option<OperatorKeyRef>,
}

/// Reads the list once per `mnauth` and asks it for [`PRO`], then reads it again, then asks the
/// kept `Arc` again.
struct ListProbe {
    seen: Arc<Mutex<Vec<Reads>>>,
    snaps: Arc<Mutex<Vec<TipFacts>>>,
}

impl NetHandler for ListProbe {
    fn commands(&self) -> &'static [CommandString] {
        &MNAUTH_CMD
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        let kept = cx.mn_net.tip_list();
        let first = kept.operator(&PRO);
        let fresh = cx.mn_net.tip_list().operator(&PRO);
        self.seen.lock().unwrap().push(Reads {
            first,
            fresh,
            kept: kept.operator(&PRO),
        });
        self.snaps.lock().unwrap().push(cx.snap.tip);
        Ok(())
    }
}

/// A5: a list published between two messages is the second message's, while its snapshot is
/// unchanged; a list published during a message leaves the `Arc` it kept on the older one.
#[test]
fn a5_tip_list_is_live_and_a_kept_arc_stays_on_its_list() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let snaps = Arc::new(Mutex::new(Vec::new()));
    let mut reg = Registry::new();
    reg.register(Box::new(ListProbe {
        seen: Arc::clone(&seen),
        snaps: Arc::clone(&snaps),
    }));
    let mut node = node(synced_net(), false, false, reg);
    let (p, _, _) = handshake(&mut node.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
    let mnauth = encode(&sample_mnauth(3));

    node.sim.deliver(&p, &[("mnauth", mnauth.clone())]);
    settle(&mut node.sim);
    node.net.publish(list(2));
    node.sim.deliver(&p, &[("mnauth", mnauth.clone())]);
    settle(&mut node.sim);
    // Right after the third message's first read.
    node.net.on_read(node.net.tip_list_reads() + 1, list(3));
    node.sim.deliver(&p, &[("mnauth", mnauth)]);
    settle(&mut node.sim);

    let reads = |first, fresh, kept| Reads {
        first: Some(key(first)),
        fresh: Some(key(fresh)),
        kept: Some(key(kept)),
    };
    assert_eq!(*seen.lock().unwrap(), [reads(1, 1, 1), reads(2, 2, 2), reads(2, 3, 2)]);
    // The chain stays put, so the snapshot is the same for all three messages.
    let snaps = snaps.lock().unwrap();
    assert_eq!(snaps.len(), 3);
    assert!(snaps.iter().all(|s| *s == snaps[0]), "{snaps:?}");
}

/// A6: on a node without a signer, every `mnauth` reaches the registered handler with the
/// installed seam, and the processor reads the list on no command of its own: the reads are the
/// handler's one per `mnauth`. Our `verack` sends no `mnauth`.
#[test]
fn a6_non_masternode_dispatches_every_mnauth_and_reads_no_list_itself() {
    let (mut node, messages, getdata) = probe_node();
    assert!(!node.sim.shared.identity().masternode);
    let (p, cmds, _) =
        handshake(&mut node.sim, 1, ConnectionType::Inbound, with_challenge(CHALLENGE));
    assert!(has(&cmds, "verack"));
    assert!(!has(&cmds, "mnauth"), "{cmds:?}");
    let mnauth = encode(&sample_mnauth(3));
    node.sim.deliver(
        &p,
        &[
            ("mnauth", mnauth.clone()),
            ("sendheaders", vec![]),
            ("ping", 7u64.to_le_bytes().to_vec()),
            ("mnauth", mnauth.clone()),
            ("inv", encode(&Inv::new(vec![]))),
            ("senddsq", vec![1]),
            ("getdata", encode(&GetData::new(vec![tx_inv(1)]))),
            ("mnauth", mnauth),
        ],
    );
    settle(&mut node.sim);
    assert!(!p.handle.disconnect_requested());
    assert_eq!(*messages.lock().unwrap(), [true, true, true]);
    assert_eq!(*getdata.lock().unwrap(), [true]);
    assert_eq!(node.net.tip_list_reads(), 3, "one read per mnauth, none by the processor");
}

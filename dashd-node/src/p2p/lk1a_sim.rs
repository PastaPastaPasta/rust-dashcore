//! P13's receive half (LK1a F): the `clsig` receive path has no IBD gate. Locks requested out of
//! IBD arrive while the node reports IBD, and Core's `ChainlockHandler` still processes them: the
//! verified one is recorded and relayed, and the one that fails verification costs the sender 10.
//! The node's own IBD state latches once left, so the simulated chain, which does not latch,
//! reports IBD here.
// Core: net_processing.cpp:5812-5833; chainlock/handler.cpp:133-183 at v24.0.0

use std::sync::{Arc, Mutex, PoisonError};

use dash_clock::MockClock;
use dash_evo::bls::BlsCtx;
use dash_evo::locks::{ChainLockSig, ClsigHashSchemes, clsig_hash};
use dash_num::Hash256;
use dash_p2p::{CommandString, ConnectionType, Permissions};
use dash_p2p_core::{ClSig, GetData, Inv, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_locks::chainlock::{
    ChainLockHandler, ClChain, ClSigsSnapshot, ClVerdict, ClVerify, Read, Stopped,
};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::MasternodeSeams;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

use crate::init::params::RELEASE;

/// What the handler recorded: each lock's height and sender.
type Recorded = Arc<Mutex<Vec<(i32, i64)>>>;

/// The handler's chain: V's best ChainLock height and its records, nothing else.
struct Chain(Recorded);

impl ClChain for Chain {
    fn intake(&self) -> Result<i32, Stopped> {
        let recorded = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(recorded.last().map_or(-1, |r| r.0))
    }

    fn record(&self, sig: Arc<ChainLockSig>, _: &[u8; 32], from: i64) -> Result<bool, Stopped> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((sig.height, from));
        Ok(true)
    }

    fn reads(&self) -> ClSigsSnapshot {
        ClSigsSnapshot::default()
    }

    fn abort_node(&self, msg: String) {
        panic!("AbortNode: {msg}");
    }
}

const VALID: i32 = 10;

/// Verifies the lock at [`VALID`], rejects every other.
struct Verify;

impl ClVerify for Verify {
    fn verify(&self, read: &Read<ClSig>) -> ClVerdict {
        if read.msg.height() == VALID {
            ClVerdict::Valid
        } else {
            ClVerdict::Invalid
        }
    }
}

fn lock(height: i32) -> (Vec<u8>, Inventory) {
    let clsig = ClSig::new(height, [0xab; 32].into(), [0; 96].into());
    let hash = clsig_hash(&clsig, ClsigHashSchemes::both(BlsCtx::BASIC)).expect("decodable");
    (encode(&clsig), Inventory::new(InvType::ClSig, Hash256::from_lendian(hash)))
}

fn encode(m: &impl BaseCodec) -> Vec<u8> {
    let mut bytes = Vec::new();
    m.encode(&mut bytes);
    bytes
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn peer(sim: &mut Sim, n: u8, connection: ConnectionType) -> SimPeer {
    let peer = sim.connect(n, connection, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&peer);
    peer
}

/// The inventory in `frames`' `getdata` or `inv` frames.
fn inventory(frames: Vec<(CommandString, Vec<u8>)>, command: CommandString) -> Vec<Inventory> {
    frames
        .into_iter()
        .filter(|(cmd, _)| *cmd == command)
        .flat_map(|(_, payload)| match command {
            CommandString::GETDATA => {
                GetData::decode(&mut &payload[..]).unwrap().inventory().clone()
            }
            _ => Inv::decode(&mut &payload[..]).unwrap().inventory().clone(),
        })
        .collect()
}

#[test]
fn a_solicited_clsig_is_processed_while_the_node_reports_ibd() {
    let recorded = Recorded::default();
    let handler = ChainLockHandler::new(
        Chain(Arc::clone(&recorded)),
        Verify,
        Arc::new(MockClock::new()),
        || true,
    );
    let params = Arc::new(ChainParamsBuilder::new(Network::Regtest, RELEASE).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with_masternode(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            chainlocks: Box::new(handler),
            ..Seams::default()
        },
        MasternodeSeams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    let source = peer(&mut sim, 1, ConnectionType::Inbound);
    let other = peer(&mut sim, 2, ConnectionType::OutboundFullRelay);
    let ((valid, valid_inv), (invalid, invalid_inv)) = (lock(VALID), lock(VALID + 1));
    sim.deliver(&source, &[("inv", encode(&Inv::new(vec![valid_inv, invalid_inv])))]);
    settle(&mut sim);
    let asked = inventory(sim.sent(&source), CommandString::GETDATA);
    assert_eq!(asked, [valid_inv, invalid_inv]);

    sim.chain.set_ibd(true);
    sim.deliver(&source, &[("clsig", valid)]);
    settle(&mut sim);
    let id = source.handle.id();
    assert_eq!(*recorded.lock().unwrap(), [(VALID, id)], "recorded");
    assert_eq!(sim.shared.misbehavior(id), Some((0, false)), "a verified lock costs nothing");
    sim.deliver(&source, &[("clsig", invalid)]);
    settle(&mut sim);
    assert_eq!(sim.shared.misbehavior(id), Some((10, false)), "a rejected lock costs 10");
    assert_eq!(recorded.lock().unwrap().len(), 1, "the rejected lock is not recorded");
    let relayed = inventory(sim.sent_at_least(&other, 1), CommandString::INV);
    assert_eq!(relayed, [valid_inv], "the recorded lock is relayed");
}

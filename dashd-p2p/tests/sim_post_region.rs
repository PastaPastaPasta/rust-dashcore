//! Registry continuations release the peer region while they wait and reopen it for their tail.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;

use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType, PeerHandle, PeerId, RawMsg};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peerman::Shared;
use dashd_p2p::processing::seams::Interrupted;
use dashd_p2p::sim::{Sim, SimPeer};
use dashd_p2p::{HandlerCx, HandlerResult, NetHandler, PostRegion, Registry, Seams};

const SPORK: [CommandString; 1] = [CommandString::from_static("spork")];

#[derive(Debug, Default)]
struct Gate {
    open: Mutex<bool>,
    waker: Mutex<Option<Waker>>,
    started: AtomicBool,
}

impl Gate {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

#[derive(Clone)]
struct Owner {
    gate: Arc<Gate>,
    shared: Arc<Mutex<Option<Arc<Shared>>>>,
    other: Arc<Mutex<Option<PeerHandle>>>,
    tail: Arc<AtomicUsize>,
    stop: bool,
}

impl NetHandler for Owner {
    fn commands(&self) -> &'static [CommandString] {
        &SPORK
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }

    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        Ok(HandlerResult::Continue(Box::new(Gated {
            gate: Arc::clone(&self.gate),
            shared: Arc::clone(&self.shared),
            other: Arc::clone(&self.other),
            peer: cx.id(),
            tail: Arc::clone(&self.tail),
            stop: self.stop,
        })))
    }
}

struct Gated {
    gate: Arc<Gate>,
    shared: Arc<Mutex<Option<Arc<Shared>>>>,
    other: Arc<Mutex<Option<PeerHandle>>>,
    peer: PeerId,
    tail: Arc<AtomicUsize>,
    stop: bool,
}

impl std::fmt::Debug for Gated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gated").field("peer", &self.peer).finish_non_exhaustive()
    }
}

impl PostRegion for Gated {
    fn poll(
        &mut self,
        logic: &mut dashd_p2p::PeerLogic,
        peer: &PeerHandle,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Interrupted>> {
        assert_eq!(self.peer, peer.id());
        assert!(
            self.shared
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|shared| shared.with_state(peer.id(), |_| ()))
                .is_some()
        );
        if !self.gate.started.swap(true, Ordering::SeqCst) {
            let other = self.other.lock().unwrap().clone().unwrap();
            logic
                .process_admitted(
                    &other,
                    &RawMsg {
                        cmd: CommandString::from_static("sendheaders"),
                        raw_size: 24,
                        payload: Vec::new(),
                        recv_us: logic.core().clock().node_now_micros(),
                    },
                )
                .unwrap();
        }
        if self.stop {
            logic.core().disconnect(peer.id(), dash_p2p::Why::Processor("post-region test"));
            return Poll::Ready(Ok(()));
        }
        if *self.gate.open.lock().unwrap() {
            return Poll::Ready(Ok(()));
        }
        *self.gate.waker.lock().unwrap() = Some(cx.waker().clone());
        Poll::Pending
    }

    fn tail(self: Box<Self>, cx: &mut HandlerCx<'_>) -> Result<(), Throw> {
        cx.misbehaving(3, "post-region tail");
        self.tail.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn sim(owner: Owner) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mut registry = Registry::new();
    registry.register(Box::new(owner));
    Sim::with(p, id, ProcessorOptions::default(), registry, Seams::default(), |_| {})
}

fn inbound(sim: &Sim, id: u8) -> SimPeer {
    sim.connect(id, ConnectionType::Inbound, Permissions::NONE)
}

#[test]
fn gated_registry_continuation_reopens_and_scores_after_wait() {
    let gate = Arc::new(Gate::default());
    let tail = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(Mutex::new(None));
    let other = Arc::new(Mutex::new(None));
    let owner = Owner {
        gate: Arc::clone(&gate),
        shared: Arc::clone(&shared),
        other: Arc::clone(&other),
        tail: Arc::clone(&tail),
        stop: false,
    };
    let mut sim = sim(owner);
    *shared.lock().unwrap() = Some(Arc::clone(&sim.shared));
    let p = inbound(&sim, 1);
    let q = inbound(&sim, 2);
    *other.lock().unwrap() = Some(q.handle.clone());
    let gate_thread = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            while !gate.started.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            gate.release();
        })
    };
    sim.admitted(&p, "spork", vec![1]).unwrap();
    gate_thread.join().unwrap();
    assert_eq!(tail.load(Ordering::SeqCst), 1);
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 3);
    assert!(sim.shared.with_state(q.handle.id(), |state| state.prefer_headers).unwrap());
}

#[test]
fn disconnected_registry_continuation_drops_its_tail() {
    let gate = Arc::new(Gate::default());
    let tail = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(Mutex::new(None));
    let other = Arc::new(Mutex::new(None));
    let p = {
        let owner = Owner {
            gate: Arc::clone(&gate),
            shared: Arc::clone(&shared),
            other: Arc::clone(&other),
            tail: Arc::clone(&tail),
            stop: true,
        };
        let mut sim = sim(owner);
        *shared.lock().unwrap() = Some(Arc::clone(&sim.shared));
        let p = inbound(&sim, 1);
        let q = inbound(&sim, 2);
        *other.lock().unwrap() = Some(q.handle.clone());
        sim.admitted(&p, "spork", vec![1]).unwrap();
        assert_eq!(tail.load(Ordering::SeqCst), 0);
        p
    };
    assert!(p.handle.disconnect_requested());
}

#[test]
fn external_disconnect_wakes_pending_registry_continuation() {
    let gate = Arc::new(Gate::default());
    let tail = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(Mutex::new(None));
    let other = Arc::new(Mutex::new(None));
    let owner = Owner {
        gate: Arc::clone(&gate),
        shared: Arc::clone(&shared),
        other: Arc::clone(&other),
        tail: Arc::clone(&tail),
        stop: false,
    };
    let mut sim = sim(owner);
    *shared.lock().unwrap() = Some(Arc::clone(&sim.shared));
    let p = inbound(&sim, 1);
    let q = inbound(&sim, 2);
    *other.lock().unwrap() = Some(q.handle.clone());
    let core = Arc::clone(&sim.core);
    let id = p.handle.id();
    let closer = thread::spawn(move || {
        while !gate.started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        core.disconnect(id, dash_p2p::Why::Processor("external post-region close"));
    });
    sim.admitted(&p, "spork", vec![1]).unwrap();
    closer.join().unwrap();
    assert_eq!(tail.load(Ordering::SeqCst), 0);
    assert!(p.handle.disconnect_requested());
}

#[test]
fn disconnect_by_id_wakes_pending_registry_continuation() {
    let gate = Arc::new(Gate::default());
    let tail = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(Mutex::new(None));
    let other = Arc::new(Mutex::new(None));
    let owner = Owner {
        gate: Arc::clone(&gate),
        shared: Arc::clone(&shared),
        other: Arc::clone(&other),
        tail: Arc::clone(&tail),
        stop: false,
    };
    let mut sim = sim(owner);
    *shared.lock().unwrap() = Some(Arc::clone(&sim.shared));
    let p = inbound(&sim, 1);
    let q = inbound(&sim, 2);
    *other.lock().unwrap() = Some(q.handle.clone());
    let core = Arc::clone(&sim.core);
    let id = p.handle.id();
    let closer = thread::spawn(move || {
        while !gate.started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        assert!(core.disconnect_by_id(id));
    });
    sim.admitted(&p, "spork", vec![1]).unwrap();
    closer.join().unwrap();
    assert_eq!(tail.load(Ordering::SeqCst), 0);
    assert!(p.handle.disconnect_requested());
}

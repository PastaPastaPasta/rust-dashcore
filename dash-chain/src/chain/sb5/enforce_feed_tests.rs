//! SB4 A-40 and A-41, their feed halves (SB5 §4.7; SB4 H4, H5, H7): a deep `enforce_block`
//! walk on the real slots, its subscribers on their own threads. CH2b runs the full rows with
//! the real W and its CRITICAL waits; here V's only waits are the per-step drains.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use super::hold_tests::{WAIT, await_event, elsewhere, locked_off_tip_n, subscribe};
use crate::BlockId;
use crate::HeaderChain;
use crate::error::ChainError;
use crate::feed::{
    ChainEvent, ControlKind, ControlMsg, FeedSource, Handoff, MAX_CONTROL_KINDS, Needs, Next,
    Publish, SubscribeError,
};
use crate::host::TipHost;
use crate::notify::{Notify, Parker};
use crate::reader::ChainReader;
use crate::residency::Permanent;
use crate::snapshot::ChainReadSnapshot;
use crate::store::NoStore;

/// The losing branch's length: every block is one disconnect inside the hold.
const N: u64 = 200;

/// V in `enforce_block`, with its subscribers on other threads: each `DisconnectTip` steps,
/// runs `on_step`, then drains, parking on the producer wake while a subscriber is full (§4.7).
struct ThreadedV {
    wake: Arc<Parker>,
    pre: Arc<ChainReadSnapshot>,
    reader: ChainReader,
    steps: Vec<ChainEvent>,
    on_step: Box<dyn FnMut(usize)>,
}

impl TipHost<NoStore, Permanent> for ThreadedV {
    type Error = ChainError;

    fn tip(&self, chain: &HeaderChain) -> BlockId {
        chain.private_tip().expect("a tip").0.id
    }

    fn contains(&self, chain: &HeaderChain, id: BlockId) -> Result<bool, ChainError> {
        Ok(chain.active_contains_private(id))
    }

    fn disconnect_tip(&mut self, chain: &mut HeaderChain) -> Result<(), ChainError> {
        let step = chain.disconnect_tip()?;
        assert!(
            Arc::ptr_eq(&self.reader.snapshot(), &self.pre),
            "readers keep the pre-walk snapshot"
        );
        self.steps.push(step);
        (self.on_step)(self.steps.len());
        while chain.retry_pending() == Publish::Blocked {
            assert!(self.wake.park_timeout(WAIT), "a subscriber wake");
        }
        Ok(())
    }

    fn conflicting_marked(
        &mut self,
        _: &mut HeaderChain,
        _: BlockId,
        _: bool,
    ) -> Result<(), ChainError> {
        Ok(())
    }
}

/// A-40's and A-41's feed halves. A deep walk at Q = 1:
/// - M, live on its own thread, gets all `N` disconnects in `seq` order while V holds, so V's
///   drains are served by M's pops alone;
/// - S holds the first disconnect and the second is pending for it; its hub deregisters from
///   another thread, which returns at once (H4), and V's drain drops what S was owed (H7);
/// - X, registered before the walk, runs `subscribe_at` during the hold: `HoldActive` at once
///   (a hub call never blocks on the hold), then `Moved` after it;
/// - readers keep the pre-walk snapshot, and the release stores one with the last `seq`.
#[test]
fn enforce_deep_walk_feed_half() {
    let (mut chain, _, b) = locked_off_tip_n(1, N, 1_000);
    let feeds = chain.feeds();
    let wake = Parker::new();
    chain.set_producer_wake(Notify::Thread(wake.clone()));
    let (_, mut m) = subscribe(&feeds);
    let (s, s_reader) = subscribe(&feeds);
    let x = feeds.register(Needs::default()).expect("registers");
    let old_tip = FeedSource::<()>::snapshot(&feeds).tip();
    let pre = chain.snapshot();
    let want: Vec<_> = (pre.tip_seq + 1..=pre.tip_seq + N).collect();
    let reader = chain.reader();

    let got = std::thread::scope(|scope| {
        let m_thread = scope.spawn(move || {
            let p = Parker::new();
            m.set_waker(Notify::Thread(p.clone()));
            let mut got = Vec::new();
            while got.len() < N as usize {
                await_event(&mut *m, &p, "M");
                got.push(m.pop().expect("selected").seq());
            }
            got
        });
        let hub = feeds.clone();
        let mut v = ThreadedV {
            wake,
            pre: pre.clone(),
            reader,
            steps: Vec::new(),
            on_step: Box::new(move |k| {
                if k == 2 {
                    let (h, t) = (hub.clone(), old_tip);
                    let r = elsewhere(move || h.subscribe_at(x, &t, &mut || Ok::<(), ()>(())));
                    assert!(matches!(r, Err(SubscribeError::HoldActive)), "{:?}", r.err());
                    let h = hub.clone();
                    elsewhere(move || h.deregister(s));
                }
            }),
        };
        let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
        assert_eq!(r.disconnected, b.iter().rev().copied().collect::<Vec<_>>());
        let seqs: Vec<_> = v.steps.iter().map(|e| e.seq()).collect();
        assert_eq!(seqs, want);
        m_thread.join().expect("M")
    });
    assert_eq!(got, want, "M got every step");
    let snap = chain.snapshot();
    assert_eq!((snap.epoch, snap.tip_seq), (pre.epoch + 1, pre.tip_seq + N), "one store");
    let reg = feeds.registry();
    assert!(reg.feeds.iter().all(|e| e.id != s), "S's entry is reclaimed");
    assert!(s_reader.is_closed());
    let r = feeds.subscribe_at(x, &old_tip, &mut || Ok::<(), ()>(()));
    assert!(matches!(r, Ok(Handoff::Moved)), "the tip moved under X");
}

/// `peak_enforcement_mirror_backlog` (§6.1, A-F12): the node's `enforce_block` over `d_h`
/// disconnects, a model hub sending every control kind twice at each one (a freeze and its
/// mirror updates), and a subscriber paused for the whole walk (Q > d_h, so V never waits on
/// it). Its slot holds at most one message per kind throughout; afterwards it takes the
/// `d_h` events and exactly one control per kind, carrying the walk's last payload. Returns the
/// peak held count.
fn mirror_backlog(d_h: u64) -> usize {
    let (mut chain, _, b) = locked_off_tip_n(64, d_h, 2_000 + d_h);
    let feeds = chain.feeds();
    let (p_id, mut p) = subscribe(&feeds);
    let hub = feeds.clone();
    let peak = Rc::new(Cell::new(0));
    let peak_v = Rc::clone(&peak);
    let mut v = ThreadedV {
        wake: Parker::new(),
        pre: chain.snapshot(),
        reader: chain.reader(),
        steps: Vec::new(),
        on_step: Box::new(move |k| {
            for round in 0..2u32 {
                for kind in 0..MAX_CONTROL_KINDS as u8 {
                    hub.send_control(p_id, ControlMsg::new(ControlKind::new(kind), (k, round)));
                    let held = hub.control_outstanding(p_id).count_ones() as usize;
                    peak_v.set(peak_v.get().max(held));
                }
            }
        }),
    };
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    assert_eq!(r.disconnected.len(), b.len());
    let (mut events, mut kinds, mut payloads) = (0, 0u16, Vec::new());
    loop {
        match p.next() {
            Next::Event(_) => {
                p.pop().expect("selected");
                events += 1;
            }
            Next::Control(c) => {
                kinds |= 1 << c.kind().get();
                payloads.push(c.downcast::<(usize, u32)>().expect("the model's payload"));
            }
            Next::Empty => break,
        }
    }
    assert_eq!(events, d_h as usize);
    assert_eq!(payloads.len(), MAX_CONTROL_KINDS, "one per kind");
    assert_eq!(kinds, (1 << MAX_CONTROL_KINDS) - 1, "every kind");
    assert!(payloads.iter().all(|&x| x == (d_h as usize, 1)), "the newest payloads");
    assert_eq!(feeds.control_outstanding(p_id), 0);
    peak.get()
}

#[test]
fn peak_enforcement_mirror_backlog() {
    let peak = mirror_backlog(50);
    assert_eq!(peak, MAX_CONTROL_KINDS, "held ≤ the kinds");
    assert_eq!(mirror_backlog(5), peak, "independent of d_h");
}

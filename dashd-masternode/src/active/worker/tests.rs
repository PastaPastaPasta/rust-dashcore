use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use dashd_chainstate::chain::{NoWaitGuard, no_wait_marked};
use dashd_chainstate::chain::{Queued, RecordingAbort, ValidationInterface, ValidationSignals};

use super::*;

#[derive(Clone)]
struct Counting(Arc<AtomicUsize>);

impl ListForBlock for Counting {
    fn list_for_block(&self, _: BlockId) -> Result<Arc<MnList>, ListError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(MnList::default_pre_dip3()))
    }
}

fn id(n: u32) -> BlockId {
    BlockId(n)
}

// Core: active/masternode.cpp:184-194; validationinterface.cpp:193 at v24.0.0
#[test]
fn core_list_lookup_refuses_the_signals_thread_marker() {
    let count = Arc::new(AtomicUsize::new(0));
    let source = CheckedListForBlock::new(Counting(Arc::clone(&count)), no_wait_marked);
    let _guard = NoWaitGuard::set();
    assert!(std::panic::catch_unwind(|| source.list_for_block(id(1))).is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

struct Listener {
    posted: TipListener,
    count: Arc<AtomicUsize>,
    done: mpsc::Sender<()>,
}

impl ValidationInterface for Listener {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        assert!(no_wait_marked());
        self.posted.updated_block_tip(new, fork, ibd).unwrap();
        assert_eq!(self.count.load(Ordering::SeqCst), 0);
        self.done.send(()).unwrap();
    }
}

// Core: active/masternode.cpp:175-228; validationinterface.cpp:193 at v24.0.0
#[test]
fn core_signals_listener_posts_and_unmarked_worker_keeps_order() {
    let count = Arc::new(AtomicUsize::new(0));
    let source = CheckedListForBlock::new(Counting(Arc::clone(&count)), no_wait_marked);
    let (release, gate) = mpsc::channel();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let worker_seen = Arc::clone(&seen);
    let (posted, worker) = {
        let _guard = NoWaitGuard::set();
        let mut first = true;
        spawn_tip_worker(move |event| {
            assert!(!no_wait_marked());
            assert_eq!(std::thread::current().name(), Some("am1-tip"));
            if first {
                gate.recv_timeout(Duration::from_secs(60)).unwrap();
                first = false;
            }
            source.list_for_block(event.new).unwrap();
            worker_seen.lock().unwrap().push(event);
        })
        .unwrap()
    };
    let signals = ValidationSignals::default();
    let (done, received) = mpsc::channel();
    let listener: Arc<dyn ValidationInterface> = Arc::new(Listener {
        posted: posted.clone(),
        count: Arc::clone(&count),
        done,
    });
    signals.register(listener.clone());
    let abort = Arc::new(RecordingAbort::default());
    let running = signals.start(abort.clone()).unwrap();
    for n in 1..=3 {
        signals.enqueue(Queued::UpdatedBlockTip {
            new: id(n),
            fork: Some(id(n - 1)),
            ibd: n == 1,
        });
        received.recv_timeout(Duration::from_secs(60)).unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    running.stop().unwrap().unwrap();
    assert!(abort.reports().is_empty());
    signals.unregister(&listener);
    drop(listener);
    drop(posted);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert_eq!(
        *seen.lock().unwrap(),
        (1..=3)
            .map(|n| TipEvent {
                new: id(n),
                fork: Some(id(n - 1)),
                ibd: n == 1
            })
            .collect::<Vec<_>>()
    );
}

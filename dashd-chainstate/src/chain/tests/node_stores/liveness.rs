//! BL-819 liveness (CH3b D244, Opus m-4): every automatic prune is waited for inside its call, so
//! V waits for a release whose step 6 waits for a Synced mandatory subscriber's ack. Each row runs
//! that subscriber on its own thread over the rig's own feed table, and asserts that step 6
//! captured it and that the call returned with its ack in.

use std::sync::OnceLock;

use dash_chain::SubscribeError;
use dash_chain::{ChainReader, ChainSnapshot, ControlMsg, IndexAccess, Refused, SendControl};

use super::*;
use crate::store::RecordingSink;
use crate::subscriber::{
    ChainFeedTable, DriverConfig, FeedTable, NoBlocks, SubStep, SubscriberDriver, SubscriberError,
    SubscriberId, SyncProgress, TOY_DB, ToyIndex,
};

/// The rig's feed table, which the hub takes before the rig's chain exists: set once the rig is
/// built. Before that it has no subscriber. Each synced read is recorded: the set step 5 reads,
/// whose mandatory members step 6 captures.
#[derive(Default)]
struct LateFeeds {
    table: OnceLock<ChainFeedTable>,
    hook: Mutex<Option<Notify>>,
    synced: Mutex<Vec<Vec<SubscriberId>>>,
}

impl LateFeeds {
    fn table(&self) -> &ChainFeedTable {
        self.table.get().expect("the rig's feeds")
    }

    fn fill(&self, table: ChainFeedTable) {
        if let Some(hook) = self.hook.lock().expect("hook").take() {
            table.set_close_hook(hook);
        }
        assert!(self.table.set(table).is_ok(), "filled once");
    }
}

impl FeedTable for LateFeeds {
    fn register(&self, id: SubscriberId, needs: Needs) -> Result<(), Refused> {
        self.table().register(id, needs)
    }

    fn deregister(&self, id: SubscriberId) {
        self.table().deregister(id);
    }

    fn on_release(&self, w: Notify) {
        self.table().on_release(w);
    }

    fn send_control(&self, id: SubscriberId, c: ControlMsg) -> SendControl {
        self.table().send_control(id, c)
    }

    fn is_closing(&self, id: SubscriberId) -> bool {
        self.table.get().is_none_or(|t| t.is_closing(id))
    }

    fn synced(&self) -> Vec<SubscriberId> {
        let synced = self.table.get().map(ChainFeedTable::synced).unwrap_or_default();
        self.synced.lock().expect("synced").push(synced.clone());
        synced
    }

    fn set_close_hook(&self, w: Notify) {
        match self.table.get() {
            Some(t) => t.set_close_hook(w),
            None => *self.hook.lock().expect("hook") = Some(w),
        }
    }

    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_> {
        self.table().snapshot()
    }

    fn subscribe_at(
        &self,
        id: SubscriberId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), SubscriberError>,
    ) -> Result<Handoff, SubscribeError<SubscriberError>> {
        self.table().subscribe_at(id, p, finish)
    }
}

/// The rig's lock-free reader, for the hub's ancestry, set once the rig is built.
#[derive(Default)]
struct LateIndex(OnceLock<ChainReader>);

impl IndexAccess for LateIndex {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.0.get()?.lookup(h)
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.0.get()?.hash(id)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.get()?.ancestor(id, height)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        self.0.get()?.lca(a, b)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.0.get()?.height(id)
    }

    fn have_data_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        self.0.get()?.have_data_pos(id)
    }

    fn undo_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        self.0.get()?.undo_pos(id)
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        self.0.get()?.status(id)
    }
}

/// A toy mandatory subscriber on its own thread: catch-up until Synced, then its events until
/// stopped.
struct Subscriber {
    id: SubscriberId,
    live: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    parker: Arc<Parker>,
    thread: Option<crate::chain::reply::NodeJoinHandle<Result<(), String>>>,
}

impl Subscriber {
    fn start(hub: Arc<SubscriberHub>) -> Subscriber {
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::new(&TOY_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let cfg = DriverConfig {
            journal_budget: 1 << 20,
            unbounded_delete_span: 64,
            d_max: None,
            cadence: 1000,
        };
        let mut d = SubscriberDriver::open(ToyIndex::mandatory(), db, hub, Arc::new(NoBlocks), cfg)
            .expect("the subscriber opens");
        let id = d.id();
        let (live, stop, parker) = (Arc::default(), Arc::default(), Parker::new());
        let (is_live, stopped, wake): (Arc<AtomicBool>, Arc<AtomicBool>, _) =
            (Arc::clone(&live), Arc::clone(&stop), Arc::clone(&parker));
        let thread = crate::chain::reply::spawn_thread("subscriber", move || {
            d.set_waker(Notify::from(Arc::clone(&wake)));
            while !stopped.load(Ordering::SeqCst) {
                match d.sync_step().map_err(|e| format!("catching up: {e}"))? {
                    SyncProgress::Progress => {}
                    SyncProgress::Live => break,
                    SyncProgress::Blocked => wake.park(),
                }
            }
            is_live.store(true, Ordering::SeqCst);
            while !stopped.load(Ordering::SeqCst) {
                while d.step().map_err(|e| format!("events: {e}"))? == SubStep::Progress {}
                wake.park();
            }
            d.stop().map_err(|e| format!("stop: {e}"))
        })
        .expect("the subscriber's thread");
        Subscriber {
            id,
            live,
            stop,
            parker,
            thread: Some(thread),
        }
    }

    fn wait_live(&self) {
        assert!(until(|| self.live.load(Ordering::SeqCst)), "the subscriber is Synced");
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        Notify::from(Arc::clone(&self.parker)).notify();
        let thread = self.thread.take().expect("running");
        let ran = thread.join().expect("unmarked joiner").expect("the subscriber thread");
        ran.expect("the subscriber");
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        Notify::from(Arc::clone(&self.parker)).notify();
    }
}

/// A pruned node whose hub runs over the rig's own feed table and index
/// (`Stores::pruned_with_subscriber`), mined `n` padded blocks with the host off, then a Synced
/// mandatory subscriber on its own thread, and an ALWAYS that brings its durable locator to the
/// tip.
struct Live {
    st: Stores,
    rig: Rig,
    chain: Vec<Mined>,
    obs: Arc<RecordingObserver>,
    on: Arc<AtomicBool>,
    feeds: Arc<LateFeeds>,
    sub: Subscriber,
}

impl Stores {
    /// [`Stores::pruned`] with a hub over feeds the rig fills once built.
    fn pruned_with_subscriber() -> (Stores, Arc<LateFeeds>, Arc<LateIndex>) {
        let mut st = Stores::pruned();
        let (feeds, idx) = (Arc::new(LateFeeds::default()), Arc::new(LateIndex::default()));
        let table: Arc<dyn FeedTable> = feeds.clone();
        st.hub = Arc::new(SubscriberHub::with_feeds(table, idx.clone()));
        (st, feeds, idx)
    }
}

impl Live {
    fn new(n: u32) -> Live {
        let (st, feeds, idx) = Stores::pruned_with_subscriber();
        let on = Arc::new(AtomicBool::new(false));
        let (rig, chain, obs) = pruned_node(&st, n, 1, &on);
        Live::attach(st, rig, chain, obs, on, feeds, &idx)
    }

    fn attach(
        st: Stores,
        rig: Rig,
        chain: Vec<Mined>,
        obs: Arc<RecordingObserver>,
        on: Arc<AtomicBool>,
        feeds: Arc<LateFeeds>,
        idx: &LateIndex,
    ) -> Live {
        let chain_feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
        feeds.fill(ChainFeedTable::new(chain_feeds));
        assert!(idx.0.set(rig.v.reader().clone()).is_ok(), "set once");
        let sub = Subscriber::start(st.hub.clone());
        sub.wait_live();
        assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        let tip = chain.last().expect("a tip");
        let at_tip = || st.hub.durable_locator(sub.id).is_some_and(|l| l.hash == tip.hash());
        assert!(until(at_tip), "the subscriber's locator at the tip");
        Live {
            st,
            rig,
            chain,
            obs,
            on,
            feeds,
            sub,
        }
    }

    /// The prune's flush: its id and the block it froze at.
    fn pruned(&self) -> (u64, Vec<u32>, BlockHash) {
        let marks = self.obs.marks();
        let (flush, files) = marks
            .iter()
            .find_map(|m| match m {
                ObsMark::PruneSelected {
                    flush,
                    files,
                    ..
                } if !files.is_empty() => Some((*flush, files.clone())),
                _ => None,
            })
            .expect("a selection");
        let new = marks.iter().find_map(|m| match m {
            ObsMark::FlushBegin {
                flush: f,
                new,
                ..
            } if *f == flush => Some(*new),
            _ => None,
        });
        (flush, files, new.expect("the prune's flush"))
    }

    /// Step 6 captured the subscriber after the ALWAYS above: some synced read since names it.
    fn captured(&self, reads_before: usize) -> bool {
        self.feeds.synced.lock().expect("synced")[reads_before..]
            .iter()
            .any(|r| r.contains(&self.sub.id))
    }

    fn reads(&self) -> usize {
        self.feeds.synced.lock().expect("synced").len()
    }

    fn finish(self) {
        let Live {
            st,
            mut rig,
            sub,
            ..
        } = self;
        rig.assert_no_abort();
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        drop(exit);
        sub.finish();
        assert!(st.latch.tripped().is_none());
    }
}

/// `got`, or every thread's stack (gdb's `thread apply all bt` on this process) and a failure
/// naming `what`: a call that never returns is a wait no one ends.
fn or_dump<T>(got: Option<T>, what: &str) -> T {
    if let Some(t) = got {
        return t;
    }
    let pid = std::process::id().to_string();
    let stacks = std::process::Command::new("gdb")
        .args(["-p", &pid, "-batch", "-ex", "thread apply all bt"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    panic!("{what} timed out; every thread's stack:\n{}", stacks.unwrap_or_else(|e| e.to_string()));
}

/// Asserts the prune's call returned with the subscriber's ack in: its durable locator is the
/// prune's frozen block, the selection's entries have no data, and step 6 captured it.
fn assert_acked(live: &Live, reads_before: usize, selected_through: u32) {
    let (_, files, new) = live.pruned();
    assert!(files.contains(&0), "{files:?}");
    let locator = live.st.hub.durable_locator(live.sub.id).expect("a locator");
    assert_eq!(locator.hash, new, "the ack is in when the call returns");
    assert!(live.captured(reads_before), "step 6 captured the subscriber");
    let pruned = live.chain.iter().filter(|m| m.height >= 1 && m.height <= selected_through);
    for m in pruned {
        assert!(!live.rig.status(m).has(Status::HAVE_DATA), "{} cleared", m.height);
    }
}

/// BL-819 (Opus m-4): an automatic prune selected at a `ConnectTip` IF_NEEDED site of a plain
/// extension, with W held at its unlink: `new` is unpublished while V waits. The call returns,
/// with the selection made, its entries cleared and the subscriber's durable locator at `new`.
#[test]
fn an_automatic_prune_at_a_connect_site_returns_with_a_synced_subscriber() {
    let live = Live::new(400);
    let tip = live.chain.last().expect("a tip").clone();
    let next = padded_branch(&live.rig.p, &tip, 1).remove(0);
    let heights = heights(&live.st);
    let reads = live.reads();
    // AcceptBlock's NONE site, then the ConnectTip site: the host turns on at the second.
    live.st.countdown.store(2, Ordering::SeqCst);
    live.st.fs.unlink.arm();
    let held = ReleaseOnDrop(&live.st.fs.unlink);
    let connected = live.rig.v.process_block(next.block.clone(), true, None, true);
    or_dump(live.st.fs.unlink.wait_held().then_some(()), "W at the prune's unlink");
    assert_eq!(live.rig.v.reader().snapshot().tip.tip, live.rig.id(&tip), "new unpublished");
    live.st.fs.unlink.release();
    drop(held);
    assert!(or_dump(connected.recv_timeout(PROMPT), "the call").expect("V").accepted);
    let (_, files, new) = live.pruned();
    assert_eq!(new, next.hash(), "the freeze at new");
    assert_acked(&live, reads, last_height(&heights, &files));
    live.finish();
}

/// BL-819 (Opus m-4, FF8's path end to end): the same at a `DisconnectTip` site with the
/// selection's highest at the bound (FF2's fixture), by a one-block `invalidateblock`.
#[test]
fn an_automatic_prune_at_a_disconnect_site_returns_with_a_synced_subscriber() {
    let (st, feeds, idx) = Stores::pruned_with_subscriber();
    let on = Arc::new(AtomicBool::new(false));
    let (rig, chain, obs, b, _) = boundary_node(&st, &on, false);
    let live = Live::attach(st, rig, chain, obs, on, feeds, &idx);
    let tip = live.chain.last().expect("a tip").clone();
    let parent = live.chain[live.chain.len() - 2].clone();
    let reads = live.reads();
    live.on.store(true, Ordering::SeqCst);
    let rewind = live.rig.v.invalidate_block(tip.hash());
    assert_eq!(or_dump(rewind.recv_timeout(PROMPT), "the call"), Ok(Ok(())), "invalidateblock");
    live.rig.v.settle().recv().expect("V settles");
    let (_, files, new) = live.pruned();
    assert_eq!((files, new), (vec![0], parent.hash()), "file 0, frozen after the step");
    assert_acked(&live, reads, b);
    live.finish();
}

/// BL-819 (Opus m-4): an automatic prune inside a reorg's hold of several steps. With T the tip,
/// a sibling s1 at T's height (equal work: stored, not connected), then s2 on s1, whose arrival
/// makes one hold: disconnect T, connect s1, connect s2, then the release. The host turns on at
/// the fourth Core-placed decision from s2's arrival: s2's accept (NONE), T's disconnect site,
/// s1's connect site, s2's connect site, which decides at s1 and freezes at s2. The published tip
/// is still T while V waits, and the subscriber has followed a disconnect and a connect that
/// aren't published.
#[test]
fn an_automatic_prune_inside_a_reorgs_hold_returns_with_a_synced_subscriber() {
    let live = Live::new(400);
    let tip = live.chain.last().expect("a tip").clone();
    let parent = &live.chain[live.chain.len() - 2];
    let s1 = branch(&live.rig.p, parent, 1, 7).remove(0);
    let s2 = branch(&live.rig.p, &s1, 1, 7).remove(0);
    live.rig.accept(&s1);
    assert_eq!(super::super::failure::tip(&live.rig), live.rig.id(&tip), "s1 not connected");
    let heights = heights(&live.st);
    let reads = live.reads();
    live.st.countdown.store(4, Ordering::SeqCst);
    live.st.fs.unlink.arm();
    let held = ReleaseOnDrop(&live.st.fs.unlink);
    let connected = live.rig.v.process_block(s2.block.clone(), true, None, true);
    or_dump(live.st.fs.unlink.wait_held().then_some(()), "W at the prune's unlink");
    assert_eq!(live.rig.v.reader().snapshot().tip.tip, live.rig.id(&tip), "T still published");
    live.st.fs.unlink.release();
    drop(held);
    assert!(or_dump(connected.recv_timeout(PROMPT), "the call").expect("V").accepted);
    let decided = live.obs.marks().into_iter().find_map(|m| match m {
        ObsMark::PruneDecision {
            manual: None,
            tip,
            files,
            ..
        } if !files.is_empty() => Some(tip),
        _ => None,
    });
    assert_eq!(decided, Some(s1.hash()), "decided at s1");
    let (_, files, new) = live.pruned();
    assert_eq!(new, s2.hash(), "frozen at s2");
    assert_acked(&live, reads, last_height(&heights, &files));
    assert_eq!(super::super::failure::tip(&live.rig), live.rig.id(&s2));
    live.finish();
}

//! Scripted seams, an injected clock and a log capture for the DKG runtime's tests.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};
use dash_primitives::{BlockHash, QuorumHash};

use crate::dkg::handler::Handler;
use crate::dkg::seams::{
    BaseFacts, ContributionsCache, DkgChain, PhaseMarks, Sporks, TipFacts, TipRef,
};
use crate::dkg::wait::{PhaseClock, PhaseCtx, TICK};
use crate::dkg::{DkgSessions, QuorumPhase, SessionKey, SessionSeams};

/// `t`'s parameters as the first chain that runs it configures them (regtest, then testnet,
/// mainnet and devnet).
pub(super) fn params(t: LlmqType) -> LlmqParams {
    [Network::Regtest, Network::Testnet3, Network::Main, Network::Devnet]
        .into_iter()
        .find_map(|network| {
            let setup = ChainParamsBuilder::new(network, Release::V24_0).build().ok()?;
            setup.consensus.llmq(t).copied()
        })
        .expect("a chain runs the type")
}

/// The calls the fakes saw, in order.
#[derive(Default)]
pub(super) struct Trace(Mutex<Vec<String>>);

impl Trace {
    pub(super) fn push(&self, what: impl Into<String>) {
        self.0.lock().expect("trace").push(what.into());
    }

    pub(super) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("trace"))
    }
}

/// The block at `height`, by its hash.
pub(super) fn block_hash(height: i32) -> BlockHash {
    let mut bytes = [0xbb; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(bytes)
}

/// The quorum hash of a base at `height`.
pub(super) fn quorum_hash(height: i32) -> QuorumHash {
    QuorumHash::from(*block_hash(height).as_bytes())
}

pub(super) fn tip(height: i32) -> TipFacts {
    TipRef {
        height,
        hash: block_hash(height),
    }
}

/// A chain whose blocks are [`block_hash`] of their height.
pub(super) struct FakeChain {
    pub(super) snapshot: AtomicBool,
    pub(super) rotation: AtomicBool,
    pub(super) dip3_from: i32,
    pub(super) trace: Arc<Trace>,
}

impl FakeChain {
    pub(super) fn new(trace: Arc<Trace>) -> Self {
        Self {
            snapshot: AtomicBool::new(false),
            rotation: AtomicBool::new(false),
            dip3_from: 0,
            trace,
        }
    }
}

impl DkgChain for FakeChain {
    fn snapshot_unvalidated(&self) -> bool {
        self.trace.push("snapshot");
        self.snapshot.load(Ordering::SeqCst)
    }
    fn dip3_enforced(&self, height: i32) -> bool {
        self.trace.push(format!("dip3({height})"));
        height >= self.dip3_from
    }
    fn lookup(&self, _: &QuorumHash) -> Option<BaseFacts> {
        None
    }
    fn ancestor_hash(&self, t: &TipFacts, height: i32) -> BlockHash {
        assert!((0..=t.height).contains(&height), "an ancestor of the tip");
        block_hash(height)
    }
    fn rotation_enabled(&self, _: &LlmqParams, _: &TipRef) -> bool {
        self.trace.push("rotation");
        self.rotation.load(Ordering::SeqCst)
    }
    fn v19_after(&self, _: &TipRef) -> bool {
        true
    }
    fn current_tip(&self) -> Option<TipFacts> {
        None
    }
}

type TickHook = Box<dyn FnMut(usize) + Send>;

/// A clock whose tick advances it by [`TICK`] and then runs the scripted hook with the tick's
/// number (from 1).
pub(super) struct FakeClock {
    now_ms: AtomicU64,
    ticks: AtomicUsize,
    pub(super) spacing: i64,
    pub(super) on_demand: bool,
    hook: Mutex<Option<TickHook>>,
    trace: Arc<Trace>,
}

impl FakeClock {
    pub(super) fn new(spacing: i64, trace: Arc<Trace>) -> Self {
        Self {
            now_ms: AtomicU64::new(1_000_000),
            ticks: AtomicUsize::new(0),
            spacing,
            on_demand: false,
            hook: Mutex::new(None),
            trace,
        }
    }

    pub(super) fn on_tick(&self, hook: impl FnMut(usize) + Send + 'static) {
        *self.hook.lock().expect("hook") = Some(Box::new(hook));
    }

    pub(super) fn ticks(&self) -> usize {
        self.ticks.load(Ordering::SeqCst)
    }
}

impl PhaseClock for FakeClock {
    fn steady_now(&self) -> Duration {
        Duration::from_millis(self.now_ms.load(Ordering::SeqCst))
    }
    fn sleep_tick(&self) {
        let millis = u64::try_from(TICK.as_millis()).expect("small");
        self.now_ms.fetch_add(millis, Ordering::SeqCst);
        let n = self.ticks.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(n < 100_000, "a wait that never ends");
        self.trace.push("tick");
        if let Some(hook) = self.hook.lock().expect("hook").as_mut() {
            hook(n);
        }
    }
    fn pow_spacing_s(&self) -> i64 {
        self.spacing
    }
    fn mine_on_demand(&self) -> bool {
        self.on_demand
    }
}

/// A debug manager that records its calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Reset(LlmqType, i32),
    Advanced(LlmqType, i32, QuorumPhase),
}

pub(super) struct FakeMarks {
    pub(super) marks: Mutex<Vec<Mark>>,
    trace: Arc<Trace>,
}

impl FakeMarks {
    pub(super) fn new(trace: Arc<Trace>) -> Self {
        Self {
            marks: Mutex::new(Vec::new()),
            trace,
        }
    }

    pub(super) fn take(&self) -> Vec<Mark> {
        std::mem::take(&mut *self.marks.lock().expect("marks"))
    }
}

impl PhaseMarks for FakeMarks {
    fn reset_local_session_status(&self, t: LlmqType, quorum_index: i32) {
        self.trace.push("reset");
        self.marks.lock().expect("marks").push(Mark::Reset(t, quorum_index));
    }
    fn mark_phase_advanced(&self, t: LlmqType, quorum_index: i32, phase: QuorumPhase) {
        self.trace.push(format!("mark:{}", phase as i32));
        self.marks.lock().expect("marks").push(Mark::Advanced(t, quorum_index, phase));
    }
}

pub(super) struct FakeSporks {
    pub(super) spork17: AtomicBool,
    trace: Arc<Trace>,
}

impl Sporks for FakeSporks {
    fn quorum_dkg_enabled(&self) -> bool {
        self.trace.push("spork17");
        self.spork17.load(Ordering::SeqCst)
    }
    fn spork21_value(&self) -> i64 {
        0
    }
    fn spork23_value(&self) -> i64 {
        0
    }
}

pub(super) struct FakeCache {
    trace: Arc<Trace>,
}

impl ContributionsCache for FakeCache {
    fn cleanup_cache(&self) {
        self.trace.push("cleanup");
    }
}

/// One handler with its seams, a clock of `spacing` seconds per block and a shared trace.
pub(super) struct Rig {
    pub(super) handler: Arc<Handler>,
    pub(super) chain: Arc<FakeChain>,
    pub(super) marks: FakeMarks,
    pub(super) clock: FakeClock,
    pub(super) trace: Arc<Trace>,
}

impl Rig {
    pub(super) fn new(t: LlmqType, params: LlmqParams, quorum_index: i32, spacing: i64) -> Self {
        let trace = Arc::new(Trace::default());
        let key = SessionKey {
            llmq_type: t,
            quorum_index,
        };
        Self {
            handler: Arc::new(Handler::new(key, params)),
            chain: Arc::new(FakeChain::new(trace.clone())),
            marks: FakeMarks::new(trace.clone()),
            clock: FakeClock::new(spacing, trace.clone()),
            trace,
        }
    }

    /// `llmq_test`, index 0, 2.5-minute blocks.
    pub(super) fn llmq_test() -> Self {
        Self::new(LlmqType::LlmqTest, params(LlmqType::LlmqTest), 0, 150)
    }

    pub(super) fn ctx(&self) -> PhaseCtx<'_> {
        PhaseCtx {
            handler: &self.handler,
            chain: self.chain.as_ref(),
            marks: &self.marks,
        }
    }

    /// The handler sees the tip at `height`.
    pub(super) fn tip(&self, height: i32) {
        self.handler.updated_block_tip(&tip(height), self.chain.as_ref());
    }

    /// From the next tick, tick `n` (from 1) delivers the tips `script(n)` gives.
    pub(super) fn script(&self, mut script: impl FnMut(usize) -> Vec<i32> + Send + 'static) {
        let handler = self.handler.clone();
        let chain = self.chain.clone();
        self.clock.on_tick(move |n| {
            for h in script(n) {
                handler.updated_block_tip(&tip(h), chain.as_ref());
            }
        });
    }
}

/// [`SessionSeams`] over fakes sharing `trace`, spork 17 on, with the chain to script.
pub(super) fn seams(trace: &Arc<Trace>) -> (SessionSeams, Arc<FakeChain>, Arc<FakeSporks>) {
    let chain = Arc::new(FakeChain::new(trace.clone()));
    let sporks = Arc::new(FakeSporks {
        spork17: AtomicBool::new(true),
        trace: trace.clone(),
    });
    let seams = SessionSeams {
        chain: chain.clone(),
        sporks: sporks.clone(),
        cache: Arc::new(FakeCache {
            trace: trace.clone(),
        }),
    };
    (seams, chain, sporks)
}

/// `llmq_test`'s [`DkgSessions`] over [`seams`], with the sporks to script.
pub(super) fn sessions(trace: &Arc<Trace>) -> (DkgSessions, Arc<FakeSporks>) {
    let (seams, _, sporks) = seams(trace);
    (DkgSessions::new([(LlmqType::LlmqTest, params(LlmqType::LlmqTest))], seams), sporks)
}

/// A line a call logged: its level, target and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Line {
    pub(super) level: tracing::Level,
    pub(super) target: String,
    pub(super) text: String,
}

struct Lines {
    lines: Mutex<Vec<Line>>,
    hold_tip_line: Duration,
}

impl tracing::Subscriber for Lines {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                if f.name() == "message" {
                    self.0.push_str(&format!("{v:?}"));
                } else {
                    self.0.push_str(&format!(" {}={v:?}", f.name()));
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        if m.0.starts_with("ActiveDKGSessionHandler::UpdatedBlockTip") {
            std::thread::sleep(self.hold_tip_line);
        }
        self.lines.lock().expect("lines").push(Line {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            text: m.0,
        });
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// The lines `f` logs on this thread.
pub(super) fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<Line>) {
    logged_across(Duration::ZERO, |_| f())
}

/// The lines `f` logs on this thread and on any thread it gives the dispatch to, in the order
/// they were recorded. Each `UpdatedBlockTip` line is held for `hold_tip_line` before it is
/// recorded.
pub(super) fn logged_across<T>(
    hold_tip_line: Duration,
    f: impl FnOnce(&tracing::Dispatch) -> T,
) -> (T, Vec<Line>) {
    // BL-479: never the only registered dispatcher.
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines {
        lines: Mutex::new(Vec::new()),
        hold_tip_line,
    });
    let dispatch = tracing::Dispatch::new(Arc::clone(&lines));
    let out = tracing::dispatcher::with_default(&dispatch, || f(&dispatch));
    let lines = std::mem::take(&mut *lines.lines.lock().expect("lines"));
    (out, lines)
}

/// The `llmq-dkg` debug lines' texts, every line checked to be one.
pub(super) fn dkg_lines(lines: &[Line]) -> Vec<String> {
    for line in lines {
        assert_eq!(line.level, tracing::Level::DEBUG, "{line:?}");
        assert_eq!(line.target, "llmq-dkg", "{line:?}");
    }
    lines.iter().map(|l| l.text.clone()).collect()
}

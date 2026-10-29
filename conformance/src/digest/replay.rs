//! `digest replay` (node program task O9a-2): a recorded node's `blocks.dat` through V, logged by
//! [`DigestTap`] around a [`NullConnector`].
//!
//! `blocks.dat` holds the node's active chain in height order, in Core's `-loadblock` framing
//! (O8 §13.1), so the replay builds the chain the node ended on and its log projects onto the
//! golden's final chain. The replay runs in linear terms only: the file carries no side
//! branches, invalid blocks or proposals, and V's connector learns no origin, so the log holds
//! `tip` connects of the header keys (§5.1) and nothing else (D-rows O9a-2).
//!
//! - **Corpus:** the stored files must be the ones `extract.json` hashed ([`Node::verify`]), and
//!   `blocks.dat` must hold genesis first and nowhere else: an empty file, a missing genesis and
//!   a second genesis are refused before V starts. The check trusts `extract.json`: for the
//!   git-tracked vectors that binds the replay to the corpus; for any other directory it proves
//!   only that the files agree with the `extract.json` beside them (D21).
//! - **Input** (CH3a D148): `blocks.dat` is streamed, one block in memory at a time. A pre-pass
//!   ([`frames::prepass`]) hashes it and checks its shape before V starts; the replay pass streams
//!   it again and must end on the same bytes. V's block files are on disk, in a new directory
//!   under `TMPDIR` that the replay removes, so the replay's memory does not grow with the chain
//!   either (O9a-2 kept every block in memory).
//! - **Params:** regtest at Core v24, with the chain options `extract.json` lists
//!   (`chain_args`). A node that ran more than one parameter set is refused.
//! - **Clock:** before each block, the mock clock is set to `A.t` of the attempt that put the
//!   block on the recorded final chain ([`RecordedDecisions::final_connect`]); a block without
//!   one keeps the clock where it was.
//! - **Checks:** CS3's [`CoreChecks`], as V runs them; `ConnectBlock` is the null connector's.
//!   SPORK_19 (ChainLocks) is off: the replay feeds no ChainLock.
//! - **Result:** every block is accepted, V stops without an `AbortNode` or a tripped fail-stop
//!   latch, V's tip is the file's last block at the height a linear chain gives it, that height
//!   is the tip of `extract.json`'s summary and that block is the golden's last. Otherwise the
//!   output is removed; if removing it fails, the error says so.

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_chain::{ChainConfig, FailStop, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::Block;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_statedigest::decisions::RecordedDecisions;
use dash_statedigest::{Chain, FileKind, Hash256, Header, Writer};
use dashd_chainstate::chain::digest_tap::DigestTap;
use dashd_chainstate::chain::{
    AbortPath, CoreChecks, GuardScope, LocalShutdown, NoFlush, NoInstantSend, NoUi, NullConnector,
    NullMempool, RecordingAbort, VParts, ValidationSignals, load_genesis_block, spawn,
};
use dashd_chainstate::store::GlobalFailStop;

use super::frames::{self, FileFrames, Shape};
use super::replay_core::{check_tip, disk_block_files};
use super::store::{Node, read};

/// What a replay did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
    /// Blocks handed to V (genesis, which V connects at start, is not one).
    pub blocks: usize,
    /// V's tip at the end: height and hash.
    pub tip: (u32, Hash256),
}

pub(crate) fn extract_json(node: &Node) -> Result<serde_json::Value> {
    let path = node.dir.join("extract.json");
    serde_json::from_slice(&read(&path)?).with_context(|| format!("parsing {}", path.display()))
}

/// The params `node` ran with: regtest at v24 with its `extract.json` `chain_args`.
pub fn params(node: &Node) -> Result<Arc<ChainParams>> {
    chain_params(node, &extract_json(node)?)
}

pub(crate) fn chain_params(node: &Node, json: &serde_json::Value) -> Result<Arc<ChainParams>> {
    ensure!(
        json["other_params_runs"].as_array().is_some_and(Vec::is_empty),
        "{}: the node ran more than one parameter set",
        node.label()
    );
    let args = json["chain_args"]
        .as_array()
        .with_context(|| format!("{}: extract.json has no chain_args", node.label()))?;
    let mut builder = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0);
    for arg in args {
        let arg = arg.as_str().with_context(|| format!("chain_args entry {arg}"))?;
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        let option = ChainOption::from_name(name)
            .with_context(|| format!("{}: unknown chain option {name}", node.label()))?;
        builder = builder.arg(option, value);
    }
    let params = builder.build().map_err(|e| anyhow!("{}: {e:?}", node.label()))?;
    Ok(Arc::new(params))
}

/// A hash in the records' display form.
pub(crate) fn display(hash: impl std::fmt::Display) -> Hash256 {
    Hash256::from_hex(hash.to_string().as_bytes()).expect("a block hash is 256-bit hex")
}

/// V's fail-stop latch: it keeps the first reason, which fails the replay.
#[derive(Default)]
pub(crate) struct Latch(Mutex<Option<String>>);

impl Latch {
    pub(crate) fn reason(&self) -> Option<String> {
        self.0.lock().expect("latch").clone()
    }
}

impl FailStop for Latch {
    fn read_failed(&self, what: &str) {
        self.0.lock().expect("latch").get_or_insert_with(|| what.to_owned());
    }

    fn is_tripped(&self) -> bool {
        self.0.lock().expect("latch").is_some()
    }
}

/// Creates `path`, which must not exist.
pub(crate) fn create_new(path: &Path) -> Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))
}

/// `e`, the error of a replay that wrote the new file `out`, after removing `out`; if that fails,
/// `e` says `out` is left.
pub(crate) fn remove_partial(out: &Path, e: anyhow::Error) -> anyhow::Error {
    match std::fs::remove_file(out) {
        Ok(()) => e,
        Err(rm) if rm.kind() == std::io::ErrorKind::NotFound => e,
        Err(rm) => {
            anyhow!("{e:#}; the partial output {} is left: removing it failed: {rm}", out.display())
        }
    }
}

/// What a replay of a stored node runs from, read from its hashed corpus.
pub(crate) struct Corpus {
    pub(crate) params: Arc<ChainParams>,
    pub(crate) decisions: Arc<RecordedDecisions>,
    /// Where the replay must end: the summary's tip height and the golden's last block.
    pub(crate) want: (i64, Hash256),
    /// The mock clock at start: init connects genesis (block 0) at the time of its recorded
    /// attempt, else at that of the first block that has one.
    pub(crate) start: i64,
    /// `blocks.dat`, as the streaming pre-pass found it.
    pub(crate) shape: Shape,
}

/// `node`'s corpus: the stored files must be the ones `extract.json` hashed, and `blocks.dat` must
/// pass the streaming pre-pass.
pub(crate) fn load_corpus(node: &Node) -> Result<Corpus> {
    // One parse of `extract.json` for the hashes, the params and the summary.
    let json = extract_json(node)?;
    node.verify_records_against(&json)?;
    let params = chain_params(node, &json)?;
    // Where the replay must end, from the hashed corpus.
    let height = json["summary"]["tip"]
        .as_i64()
        .with_context(|| format!("{}: extract.json has no summary tip", node.label()))?;
    let golden = super::compare::project(&node.golden())?;
    let Some(last) = golden.chain.last() else {
        bail!("{}: the golden has no block", node.label());
    };
    let want = (height, last.b);
    let decisions = RecordedDecisions::load(&read(&node.digest())?, &read(&node.decisions())?)
        .map_err(|e| anyhow!("{}: loading the decisions: {e}", node.label()))?;
    let decisions = Arc::new(decisions);
    let genesis = dashd_chainstate::chain::genesis_block(&params);
    let mut start = None;
    let shape = frames::prepass(node, &json, params.message_start, genesis.hash(), |b| {
        if start.is_none() {
            start = replay_time(&decisions, b);
        }
    })?;
    let start = start.unwrap_or(i64::from(genesis.header().time()));
    Ok(Corpus {
        params,
        decisions,
        want,
        start,
        shape,
    })
}

/// Replays `node`'s `blocks.dat` through V and writes the tap's digest to `out`, a new file that
/// is removed again if the replay fails.
pub fn replay(node: &Node, out: &Path) -> Result<Replayed> {
    let corpus = load_corpus(node)?;
    let file = create_new(out)?;
    let input = frames::replay_pass(node, corpus.params.message_start)?;
    run(node, &corpus, input, file).map_err(|e| remove_partial(out, e))
}

/// The mock time of the attempt that put `block` on the recorded final chain, if any.
pub(crate) fn replay_time(decisions: &RecordedDecisions, block: &Block) -> Option<i64> {
    let key = decisions.final_connect(&display(block.hash()))?;
    decisions.get(&key).map(|a| a.t)
}

fn run(node: &Node, c: &Corpus, mut input: FileFrames, file: File) -> Result<Replayed> {
    let p = &c.params;
    // V's block files, under the stores' latch and the abort sink, as the node's.
    let work = tempfile::Builder::new()
        .prefix("dash-conformance-replay-")
        .tempdir()
        .context("creating the replay's block directory")?;
    let stores = GlobalFailStop::new();
    let abort = Arc::new(RecordingAbort::default());
    let path = AbortPath::new(abort.clone(), stores.clone());
    let (_, mut files) = disk_block_files(p, work.path(), &stores, &path)?;

    let clock = Arc::new(MockClock::at(c.start));
    let latch = Arc::new(Latch::default());
    let svc = Services {
        clock: clock.clone(),
        sporks: Arc::new(StaticSporks::for_network(p, Some(false))),
        fail: latch.clone(),
    };
    let (chain, idx) = load_genesis_block(p.clone(), ChainConfig::node(p), svc, &mut files)
        .map_err(|e| anyhow!("loading genesis: {e:?}"))?;

    let header = Header {
        file: FileKind::Digest,
        src: Writer::Rdashd,
        ver: format!("dash-conformance {} replay", env!("CARGO_PKG_VERSION")).into_bytes(),
        chain: Chain::Regtest,
        run: format!("replay-{}", node.label()).into_bytes(),
    };
    let tap = DigestTap::new(NullConnector, file, header).map_err(|e| anyhow!("{e:?}"))?;

    let signals = ValidationSignals::default();
    let signals_thread = signals.start(abort.clone()).context("starting the signals thread")?;
    let (v, join) = spawn(VParts {
        params: p.clone(),
        chain,
        idx,
        checks: Box::new(CoreChecks::new(p.clone()).map_err(|e| anyhow!("{e:?}"))?),
        files: Box::new(files),
        connector: Box::new(tap),
        mempool: Box::new(NullMempool),
        flush: Box::new(NoFlush),
        is: Box::new(NoInstantSend),
        ui: Arc::new(NoUi),
        abort: abort.clone(),
        shutdown: Arc::new(LocalShutdown::default()),
        signals,
        clock: clock.clone(),
        check_block_index: true,
        have_pruned: false,
        stop_at_height: 0,
        guard: GuardScope::of(p),
    })
    .context("starting V")?;

    let mut read = 0;
    let mut feed = || -> Result<usize> {
        v.activate_best_chain().recv().map_err(|e| anyhow!("init's ABC: {e:?}"))?;
        let mut handed = 0;
        // Block 0 is genesis (the pre-pass), which V connected at start.
        for (i, block) in input.by_ref().enumerate() {
            let block = block?;
            read += 1;
            if i == 0 {
                continue;
            }
            #[cfg(test)]
            tests::before_block(i, &latch);
            if let Some(t) = replay_time(&c.decisions, &block) {
                clock.set_mocktime(t);
            }
            let r = v
                .process_block(block.clone(), true, None, true)
                .recv()
                .map_err(|e| anyhow!("block {i}: {e:?}"))?;
            ensure!(
                r.accepted && r.state.is_valid(),
                "blocks.dat block {i} ({}) was not accepted: {:?}",
                block.hash(),
                r.state
            );
            handed += 1;
        }
        Ok(handed)
    };
    let handed = feed();
    let view = v.view();
    v.shutdown();
    let exit =
        join.join().map_err(|e| anyhow!("joining V: {e:?}"))?.map_err(|_| anyhow!("V panicked"))?;
    let _ = signals_thread.stop();
    // An AbortNode first: it is why V stopped answering, which `handed` reports as a lost reply.
    if let Some(fatal) = exit.fatal {
        bail!("V stopped on an AbortNode: {}", fatal.0);
    }
    ensure!(abort.messages().is_empty(), "AbortNode: {:?}", abort.messages());
    if let Some(what) = latch.reason() {
        bail!("V's fail-stop latch tripped: {what}");
    }
    if let Some(failure) = stores.tripped() {
        bail!("the stores' latch tripped: {failure:?}");
    }
    let handed = handed?;
    frames::finish_pass(&node.blocks(), input, &c.shape, read)?;
    work.close().context("removing the replay's block directory")?;

    // Acceptance does not say the block was connected: V's tip must be the last block, at the
    // height of a chain of every block handed to it.
    let tip = check_tip(&view, c.shape.last, handed)?;
    let (want_height, want_hash) = c.want;
    ensure!(
        i64::from(tip.0) == want_height,
        "V's tip height {} is not extract.json's summary tip {want_height}",
        tip.0
    );
    ensure!(tip.1 == want_hash, "V's tip {} is not the golden's last block {want_hash}", tip.1);
    Ok(Replayed {
        blocks: handed,
        tip,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::digest::store::Vectors;

    type Hook = Box<dyn FnMut(usize, &Latch)>;

    thread_local! {
        /// Runs before the replay hands block `i` to V (on the replaying thread).
        static BEFORE_BLOCK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn before_block(i: usize, latch: &Latch) {
        BEFORE_BLOCK.with_borrow_mut(|hook| {
            if let Some(hook) = hook {
                hook(i, latch);
            }
        });
    }

    /// Replays `p2p_invalid_block` node0 into `out` with `hook` before each block.
    fn replay_with(out: &Path, hook: impl FnMut(usize, &Latch) + 'static) -> Result<Replayed> {
        let nodes = Vectors::regtest_v24().nodes("p2p_invalid_block").unwrap();
        let node = nodes.into_iter().find(|n| n.name == "node0").expect("node0");
        BEFORE_BLOCK.set(Some(Box::new(hook)));
        let replayed = replay(&node, out);
        BEFORE_BLOCK.set(None);
        replayed
    }

    fn tempdir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        chmod(dir.path(), 0o700);
        dir
    }

    /// V trips the latch on a failed cold read, which `MemBlockFiles` never has: the hook trips
    /// it mid-replay instead.
    #[test]
    fn a_latch_tripped_during_the_replay_fails_it_with_the_first_reason() {
        let dir = tempdir();
        let out = dir.path().join("replay.jsonl");
        let err = replay_with(&out, |i, latch| {
            if i == 3 {
                latch.read_failed("planted read failure");
                latch.read_failed("a later one");
            }
        })
        .unwrap_err();
        assert_eq!(format!("{err:#}"), "V's fail-stop latch tripped: planted read failure");
        assert!(!out.exists());
    }

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A failed removal is reported with the replay's error, and the output is left. The hook
    /// makes the directory read-only mid-replay; where that does not bind (root), the test skips.
    #[test]
    fn a_failed_removal_is_reported_with_the_replay_error() {
        let dir = tempdir();
        let sub = dir.path().join("out");
        std::fs::create_dir(&sub).unwrap();
        chmod(&sub, 0o500);
        let binds = std::fs::File::create(sub.join("probe")).is_err();
        chmod(&sub, 0o700);
        if !binds {
            eprintln!("skipped: a read-only directory is writable here");
            return;
        }
        let out = sub.join("replay.jsonl");
        let ro = sub.clone();
        let err = replay_with(&out, move |i, latch| {
            if i == 3 {
                chmod(&ro, 0o500);
                latch.read_failed("planted read failure");
            }
        })
        .unwrap_err();
        let exists = out.exists();
        chmod(&sub, 0o700);
        let err = format!("{err:#}");
        assert!(err.starts_with("V's fail-stop latch tripped: planted read failure; "), "{err}");
        assert!(
            err.contains(&format!(
                "the partial output {} is left: removing it failed",
                out.display()
            )),
            "{err}"
        );
        assert!(exists);
    }

    /// An output that is already gone is not reported as left.
    #[test]
    fn an_output_already_gone_is_not_reported() {
        let dir = tempdir();
        let out = dir.path().join("replay.jsonl");
        let gone = out.clone();
        let err = replay_with(&out, move |i, latch| {
            if i == 3 {
                std::fs::remove_file(&gone).unwrap();
                latch.read_failed("planted read failure");
            }
        })
        .unwrap_err();
        assert_eq!(format!("{err:#}"), "V's fail-stop latch tripped: planted read failure");
        assert!(!out.exists());
    }

    /// No replay over in-memory block files trips the latch, so its contract is pinned here.
    #[test]
    fn the_latch_keeps_the_first_reason() {
        let latch = Latch::default();
        assert!(!latch.is_tripped());
        assert_eq!(latch.reason(), None);
        latch.read_failed("first");
        latch.read_failed("second");
        assert!(latch.is_tripped());
        assert_eq!(latch.reason().as_deref(), Some("first"));
    }
}

//! CH5-5's live-copy seams for the process tests, built only with `test-hooks` (§2.2, §6.2) and
//! active only under `RDASHD_TEST_HOOK=live-copy` ([`enabled`], OD-CH5-27 (b)):
//! - `gettxoutsetinfo`'s `livecopy_counters` field, from the copy point's counter snapshot;
//! - the read-only `livecopycounters` command: the same counters live, with the super versions
//!   read now and the hooks' events;
//! - with `RDASHD_TEST_HOOK_DIR=<dir>` (required with the seam: init panics without it): file
//!   requests. The test writes `<dir>/<req>` (whole, by a rename), the node removes it, acts, and
//!   writes `<dir>/<req>.done` the same way. The test removes the `.done` file before it asks
//!   again.
//!   - `flush`: a scripted flush through V's `FlushStateToDisk`; `.done` once W's job has run its
//!     last step, with `steps=` the job's steps and `verdicts=` the barrier verdicts it added;
//!   - `quiesce`: the engine's wait for no flush, compaction or purge, with no flush;
//!   - `spork-update` holding `<name> <value>`: Core's `UpdateSpork` (needs `-sporkkey`);
//!   - `cl-floor` holding `<hash> <height>`: `RecordChainLock` for a lock with a zero signature,
//!     as after LK1a's verification;
//!   - `verdicts`: every barrier verdict since start, one column family a line.
//!
//! A failed request writes `error=<text>` to its `.done` file.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use dash_num::Hash256;
use dash_primitives::BlockHash;
use dash_rpc_spec::{Class, Gating, RpcCommand};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ChainHandle, ChainLockSig};
use dashd_chainstate::store::GuardedEngine;
use dashd_masternode::sporks::SporkManager;
use dashd_rpc::UniValue;
use dashd_rpc::protocol::{RPC_MISC_ERROR, RpcError};
use dashd_rpc::server::{Command, RpcServer};
use dashd_store::livecopy::{COUNTERS, CopyPointSnapshot, Tripwire};
use dashd_store::{KvEngine as _, LiveVerdict};

use crate::chainstate::Engine;
use crate::logging::Logger;
use crate::shutdown::ShutdownState;

/// How long a `flush` request waits for its job.
const FLUSH_PATIENCE: Duration = Duration::from_secs(60);

/// The loaded chainstate's engine, for `livecopycounters` and `quiesce`; weak, so the seams
/// never keep the database open past shutdown.
static ENGINE: Mutex<Option<Weak<GuardedEngine<Engine>>>> = Mutex::new(None);

/// Records the loaded chainstate's engine.
pub(crate) fn attach(engine: &Arc<GuardedEngine<Engine>>) {
    *ENGINE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(engine));
}

fn engine() -> Option<Arc<GuardedEngine<Engine>>> {
    ENGINE.lock().unwrap_or_else(PoisonError::into_inner).as_ref()?.upgrade()
}

/// Whether this process runs the live-copy seam (`RDASHD_TEST_HOOK=live-copy`). Without it a
/// hooks build answers as the production build: no `livecopy_counters` field and no
/// `livecopycounters` command (OD-CH5-27 (b)), so the Core-diff targets stay comparable when a
/// unified feature selection compiles `test-hooks` in.
pub fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RDASHD_TEST_HOOK").as_deref() == Ok("live-copy"))
}

/// The counters as the reply and the command show them.
fn counters(tripwire: &Tripwire, super_versions: &[(&str, u64)]) -> UniValue {
    let mut o = UniValue::new_object();
    o.push_kv_end("activations", tripwire.activations.into());
    o.push_kv_end("disconnects", tripwire.disconnects.into());
    o.push_kv_end("flatfile_mutations", tripwire.flatfile_mutations.into());
    o.push_kv_end("undo_writes", tripwire.undo_writes.into());
    let mut versions = UniValue::new_object();
    for (cf, n) in super_versions {
        versions.push_kv_end(*cf, (*n).into());
    }
    o.push_kv_end("super_versions", versions);
    o
}

/// `gettxoutsetinfo`'s `livecopy_counters`: what the copy point's barrier snapshotted.
pub fn reply_field(snapshot: &CopyPointSnapshot) -> UniValue {
    counters(&snapshot.tripwire, &snapshot.super_versions)
}

/// `livecopycounters`' spec: hidden, so out of `help`'s overview; registered only here.
static SPEC: RpcCommand = RpcCommand {
    name: "livecopycounters",
    category: "hidden",
    whitelist_name: "livecopycounters",
    gating: Gating::Always,
    class: Class::Wont,
    args: &[],
    help: "livecopycounters\n\nTest-hooks builds under RDASHD_TEST_HOOK=live-copy only (CH5-5): \
           the live-copy counters now, with the chainstate's super versions and the test hooks' \
           events.\n",
};

/// Registers `livecopycounters` when the seam is [`enabled`].
pub fn register(table: &RpcServer) {
    if !enabled() {
        return;
    }
    table.append_command(Command::with_spec(&SPEC, |_| livecopycounters()));
}

/// The counters live: the tripwire, each column family's super version read now, and the
/// events under `events`.
fn livecopycounters() -> Result<UniValue, RpcError> {
    let misc = |m: String| RpcError::new(RPC_MISC_ERROR, m);
    let engine = engine().ok_or_else(|| misc("no chainstate is loaded".into()))?;
    let versions = engine.engine().rocks().super_versions().map_err(|e| misc(e.to_string()))?;
    let mut o = counters(&COUNTERS.tripwire(), &versions);
    let e = COUNTERS.events();
    let mut events = UniValue::new_object();
    events.push_kv_end("cl_turned_on_reset", e.cl_turned_on_reset.into());
    events.push_kv_end("cl_enforced", e.cl_enforced.into());
    let floor = e.cl_enforced_floor.map(|f| BlockHash::from(f).to_string());
    events.push_kv_end("cl_enforced_floor", floor.map_or(UniValue::Null, UniValue::from));
    events.push_kv_end("flush_jobs_done", e.flush_jobs_done.into());
    events.push_kv_end("verdicts", (COUNTERS.verdicts().len() as u64).into());
    o.push_kv_end("events", events);
    Ok(o)
}

/// What the request hooks act through.
pub(crate) struct Hooks {
    pub dir: PathBuf,
    pub chain: ChainHandle,
    pub sporks: Arc<SporkManager>,
    pub logger: Arc<Logger>,
    pub shutdown: Arc<ShutdownState>,
}

/// The requests, in the order each poll looks for them.
const REQUESTS: [&str; 5] = ["flush", "quiesce", "spork-update", "cl-floor", "verdicts"];

impl Hooks {
    /// Serves the requests on a thread of their own until shutdown is requested.
    pub(crate) fn spawn(self) {
        self.logger.log_printf(&format!("test-hooks: live-copy hooks in {}\n", self.dir.display()));
        let spawned = std::thread::Builder::new().name("livecopy-hooks".into()).spawn(move || {
            while !self.shutdown.shutdown_requested() {
                for req in REQUESTS {
                    self.serve(req);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        spawned.expect("the live-copy hooks' thread starts");
    }

    fn serve(&self, req: &str) {
        let path = self.dir.join(req);
        let Ok(body) = std::fs::read_to_string(&path) else {
            return;
        };
        if let Err(e) = std::fs::remove_file(&path) {
            self.logger.log_printf(&format!("test-hooks: {req}: {e}\n"));
            return;
        }
        let done = match req {
            "flush" => self.flush(),
            "quiesce" => quiesce(),
            "spork-update" => self.spork_update(&body),
            "cl-floor" => self.cl_floor(&body),
            "verdicts" => Ok(verdict_lines(&COUNTERS.verdicts())),
            _ => unreachable!("one of REQUESTS"),
        };
        let text = done.unwrap_or_else(|e| format!("error={e}\n"));
        if let Err(e) = write_done(&self.dir, req, &text) {
            self.logger.log_printf(&format!("test-hooks: {req}.done: {e}\n"));
        }
    }

    /// A scripted flush (§6.2, OD-CH5-22 (a)): [`ChainHandle::scripted_flush`], answered once
    /// W's `JobDone` of that job is seen, with the barrier verdicts added meanwhile.
    fn flush(&self) -> Result<String, String> {
        let verdicts = COUNTERS.verdicts().len();
        let job = self.chain.scripted_flush(FLUSH_PATIENCE)?;
        let added = COUNTERS.verdicts().split_off(verdicts);
        Ok(format!(
            "steps={}\nverdicts={}\n{}",
            job.steps.join(","),
            added.len(),
            verdict_lines(&added)
        ))
    }

    /// Core's `sporkupdate` without its relay: `<name> <value>`.
    fn spork_update(&self, body: &str) -> Result<String, String> {
        let mut words = body.split_whitespace();
        let (Some(name), Some(value), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!("expected `<name> <value>`, got {body:?}"));
        };
        let id = dash_evo::spork::id_by_name(name).ok_or_else(|| format!("no spork {name}"))?;
        let value = value.parse::<i64>().map_err(|e| format!("value {value:?}: {e}"))?;
        let hash = self.sporks.update_spork(id, value).ok_or("no -sporkkey signer")?;
        let hex: String = hash.iter().rev().map(|b| format!("{b:02x}")).collect();
        Ok(format!("hash={hex}\n"))
    }

    /// `RecordChainLock` for `<hash> <height>`, as after a verified CLSIG (§6.2).
    fn cl_floor(&self, body: &str) -> Result<String, String> {
        let mut words = body.split_whitespace();
        let (Some(hash), Some(height), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!("expected `<hash> <height>`, got {body:?}"));
        };
        let block_hash = hash.parse::<BlockHash>().map_err(|e| format!("hash {hash:?}: {e:?}"))?;
        let height = height.parse::<i32>().map_err(|e| format!("height {height:?}: {e}"))?;
        // Any value distinct per lock: V keys its known locks by it (D41).
        let mut key = *block_hash.as_bytes();
        key[..4].iter_mut().zip(height.to_le_bytes()).for_each(|(k, h)| *k ^= h);
        let sig = Arc::new(ChainLockSig {
            height,
            block_hash,
            sig: ChainLockSig::null().sig,
        });
        let reply = self
            .chain
            .record_chainlock(sig, Hash256::from_lendian(key), None)
            .recv()
            .map_err(|_| "the chainstate stopped".to_owned())?;
        Ok(format!("relay={}\n", reply.relay))
    }
}

/// `quiesce`: the engine's idle wait, with no flush (§6.2).
fn quiesce() -> Result<String, String> {
    let engine = engine().ok_or("no chainstate is loaded")?;
    engine.wait_idle_purged().map_err(|e| e.to_string())?;
    Ok("ok\n".into())
}

/// One line a column family: `<verdict index> <cf> hwm= bytes= over_b= valid= sv=
/// invalidation=<from>-><to>|none`.
fn verdict_lines(verdicts: &[LiveVerdict]) -> String {
    let mut out = String::new();
    for (i, v) in verdicts.iter().enumerate() {
        for cf in &v.cfs {
            let invalidation = cf.invalidation.map_or_else(
                || "none".to_owned(),
                |dashd_store::Invalidation::SuperVersionChanged {
                     from,
                     to,
                 }| format!("{from}->{to}"),
            );
            out.push_str(&format!(
                "{i} {} hwm={} bytes={} over_b={} valid={} sv={} invalidation={invalidation}\n",
                cf.cf, cf.hwm, cf.bytes, cf.over_b, cf.valid, cf.super_version
            ));
        }
    }
    out
}

/// Writes `<req>.done` whole: a temporary file, synced, then renamed.
fn write_done(dir: &Path, req: &str, text: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!("{req}.done.tmp"));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, dir.join(format!("{req}.done")))
}

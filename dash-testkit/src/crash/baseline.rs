//! The baseline (S3's gate before any crash run): rdashd on plain disk syncs a fixture's schedule
//! to its tip, with C2 (the coin statistics at the tip equal the oracle row of that tip) at every
//! acknowledgement and every tip on the trajectory in order. A fixture and an rdashd that disagree
//! with nothing crashed would make every crash check meaningless.
//!
//! An acknowledgement is a `gettxoutsetinfo`: rdashd flushes before it answers (as dashd does),
//! so its answer is also the durable state a later crash must not lose.

use std::fs::File;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::checks::{C2_HASHES, c2_call};
use super::fixture::{F01, Fixture};
use crate::{NodeRpc, RpcEndpoint, RustNode, RustNodeConfig, find_available_port};

/// How long a run waits for a node to reach a tip; generous for a loaded box.
const PATIENCE: Duration = Duration::from_secs(1800);

/// The dashd that serves a fixture's P2P part, on a reflinked copy of its datadir.
pub struct Peer {
    child: Child,
    rpc: RpcEndpoint,
    pub p2p: SocketAddr,
}

impl Peer {
    pub fn start(dashd: &Path, fixture: &Fixture, work: &Path) -> io::Result<Peer> {
        let datadir = work.join("peer");
        let copied = Command::new("cp")
            .args(["-a", "--reflink=always"])
            .arg(&fixture.p2p_datadir)
            .arg(&datadir)
            .status()?;
        if !copied.success() {
            return Err(io::Error::other(format!("copying {}", fixture.p2p_datadir.display())));
        }
        let (port, rpc_port) = (find_available_port(), find_available_port());
        let rpc = RpcEndpoint::regtest(&datadir, rpc_port);
        let mut command = Command::new(dashd);
        // A harness killed outright must not leave the peer holding its port (ST3b r1 n-6).
        crate::rust_node::kill_with_parent(&mut command);
        let child = command
            .arg("-regtest")
            .arg(format!("-datadir={}", datadir.display()))
            .args(["-daemon=0", "-printtoconsole=0", "-disablewallet", "-dnsseed=0", "-connect=0"])
            .args(["-listen=1", "-bind=127.0.0.1", "-listenonion=0", "-whitelist=127.0.0.1"])
            .arg(format!("-port={port}"))
            .arg(format!("-mocktime={}", fixture.mocktime))
            .args(rpc.args())
            .args(&fixture.chain_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(work.join("peer.stderr"))?)
            .spawn()?;
        let mut peer = Peer {
            child,
            rpc,
            p2p: SocketAddr::from(([127, 0, 0, 1], port)),
        };
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = peer.child.try_wait()? {
                return Err(io::Error::other(format!("the peer dashd exited: {status}")));
            }
            if let Ok(best) = peer.call("getbestblockhash", &[]) {
                if best == fixture.p2p_tip.as_str() {
                    return Ok(peer);
                }
                return Err(io::Error::other(format!("the peer's tip is {best}")));
            }
            if Instant::now() > deadline {
                return Err(io::Error::other("the peer dashd did not answer RPC"));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    pub fn call(&self, method: &str, params: &[Value]) -> io::Result<Value> {
        let client = self.rpc.client().map_err(io::Error::other)?;
        client.call(method, params).map_err(io::Error::other)
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.call("stop", &[])?;
        let status = self.child.wait()?;
        if !status.success() {
            return Err(io::Error::other(format!("the peer dashd stopped with {status}")));
        }
        Ok(())
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// What a baseline run saw.
#[derive(Debug, Default)]
pub struct Report {
    /// The trajectory position each acknowledgement saw, in order: ack `a<k>` is `acks[k]`.
    pub acks: Vec<usize>,
    pub tip: String,
    /// rdashd's "Writing partial batch" lines in this run: a split coins flush (ST3b-D9).
    pub partial_batches: usize,
    pub failures: Vec<String>,
}

/// Called with `a<k>` right after acknowledgement `k` returns (a recording marks the log with it).
pub type Marker<'a> = &'a mut dyn FnMut(&str) -> io::Result<()>;

/// The rdashd arguments of a fixture run, beyond the harness's own: the fixture's chain,
/// `-debug=coindb` (so a split flush shows its partial batches), then `extra`.
pub fn node_args(fixture: &Fixture, extra: &[String]) -> Vec<String> {
    let mut args = fixture.chain_args.clone();
    args.push("-debug=coindb".to_owned());
    args.extend(extra.iter().cloned());
    args
}

/// A fault run's schedule (S6 plan §1, §3.4): it runs until the node dies.
pub struct UntilDeath<'a> {
    /// Set once the fault may be in effect. From then on an acknowledgement is one
    /// `gettxoutsetinfo`, a refused `submitblock` is no failure, and the first RPC error ends the
    /// schedule.
    pub armed: &'a AtomicBool,
    /// Called with `k` right after the stored blocks' `k`-th acknowledgement (from 0) returns: the
    /// fault arms from one. The P2P part's acknowledgements, which come every 2 s, have no index a
    /// seed could name.
    pub hook: &'a mut dyn FnMut(usize) -> io::Result<()>,
}

/// One run of the schedule against a node.
struct Drive<'a> {
    rpc: &'a dyn NodeRpc,
    fixture: &'a Fixture,
    /// The node's tip's position on the trajectory.
    pos: usize,
    report: Report,
    mark: Marker<'a>,
    until_death: Option<UntilDeath<'a>>,
    /// The stored blocks' acknowledgements so far, once that part has begun.
    stored_acks: Option<usize>,
    /// C's status after F01's pair, as [`Drive::c_expected`] found it when C was submitted.
    c_expected: Option<Value>,
}

impl Drive<'_> {
    /// An acknowledgement: C2 at the node's tip, which must be on the trajectory at or after
    /// `pos`. Returns the tip. Once armed, the first of C2's calls alone; a pair whose first call
    /// succeeded and second failed (the fault landed between them) still counts the first call's
    /// tip (its flush is durable) before the error returns (S6 plan §3.4).
    fn ack(&mut self) -> io::Result<String> {
        let calls = if self.armed() {
            1
        } else {
            C2_HASHES.len()
        };
        let (mut best, mut failed) = (None, None);
        for hash in &C2_HASHES[..calls] {
            match c2_call(self.rpc, self.fixture, *hash, &mut self.report.failures) {
                Ok(tip) => best = Some(tip),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        let Some(best) = best else {
            return Err(io::Error::other(failed.expect("a call failed")));
        };
        match self.fixture.on_trajectory(&best, self.pos) {
            Some(at) => self.pos = at,
            None => self.fail(format!("tip {best} is not on the trajectory after {}", self.pos)),
        }
        (self.mark)(&format!("a{}", self.report.acks.len()))?;
        self.report.acks.push(self.pos);
        if let Some(e) = failed {
            return Err(io::Error::other(e));
        }
        if let Some(k) = self.stored_acks {
            if let Some(until) = self.until_death.as_mut() {
                (until.hook)(k)?;
            }
            self.stored_acks = Some(k + 1);
        }
        Ok(best)
    }

    fn armed(&self) -> bool {
        self.until_death.as_ref().is_some_and(|u| u.armed.load(Ordering::SeqCst))
    }

    fn fail(&mut self, why: String) {
        self.report.failures.push(why);
    }

    fn call(&self, method: &str, params: &[Value]) -> io::Result<Value> {
        self.rpc.call(method, params).map_err(io::Error::other)
    }

    fn known(&self, hash: &str) -> bool {
        self.call("getblockheader", &[json!(hash)]).is_ok()
    }

    /// The first stored block the node lacks. A flush writes every new index entry at once, so
    /// the blocks it has are a prefix of the list; one after the gap is a lost entry.
    fn resume_at(&mut self) -> usize {
        let submit = &self.fixture.submit;
        let start = submit.iter().position(|h| !self.known(h)).unwrap_or(submit.len());
        if let Some(later) = submit[start..].iter().find(|h| self.known(h)) {
            let why =
                format!("the node has stored block {later} but not the earlier {}", submit[start]);
            self.fail(why);
        }
        start
    }

    /// `submitblock` of a block the fixture's chain holds: anything but a null reply (connected)
    /// or `inconclusive` (stored on a side branch) fails.
    fn submit(&mut self, hash: &str) -> io::Result<()> {
        let reply = self.call("submitblock", &[json!(self.fixture.blocks[hash])])?;
        // A dying node may refuse a block it could not store.
        if !(reply.is_null() || reply == "inconclusive" || self.armed()) {
            self.fail(format!("submitblock {hash}: {reply}"));
        }
        Ok(())
    }

    /// A block's `getchaintips` status (null if it is no tip).
    fn tip_status(&self, hash: &str) -> io::Result<Value> {
        let tips = self.call("getchaintips", &[])?;
        Ok(tips
            .as_array()
            .and_then(|t| t.iter().find(|x| x["hash"] == hash))
            .map_or(Value::Null, |x| x["status"].clone()))
    }

    /// What dashd answers for C, and C's status after: the fixture's, or, when the node already
    /// holds F as invalid, Core's for a child of a failed block (refused, never stored:
    /// `validation.cpp:4638-4640`, rc.3). A recovery can fail F before C comes: F has the tip's
    /// work, and Core breaks that tie among blocks loaded from disk by pointer address
    /// (`node/blockstorage.cpp:42-43`), so it may try F first.
    fn c_expected(&self, f: &F01) -> io::Result<(Value, Value)> {
        Ok(if self.tip_status(&f.f_hash)? == "invalid" {
            (json!("bad-prevblk"), Value::Null)
        } else {
            (f.dashd_submit_c.clone(), f.dashd_c_status.clone())
        })
    }

    /// F01's pair (C only if the node has F): F is an equal-work sibling, stored, not connected.
    /// A flush, then its child makes it the most work, and connecting it fails. dashd's replies
    /// are the reference.
    fn f01(&mut self, f: &F01, have_f: bool) -> io::Result<()> {
        if !have_f {
            let reply_f = self.call("submitblock", &[json!(f.f_hex)])?;
            if reply_f != f.dashd_submit_f {
                self.fail(format!("F01: submit F {reply_f}, dashd's {}", f.dashd_submit_f));
            }
            self.ack()?;
        }
        let (want_reply, want_status) = self.c_expected(f)?;
        self.c_expected = Some(want_status.clone());
        let reply_c = self.call("submitblock", &[json!(f.c_hex)])?;
        let status = self.tip_status(&f.c_hash)?;
        if (&reply_c, &status) != (&want_reply, &want_status) {
            self.fail(format!(
                "F01: (submit C, C's status) ({reply_c}, {status}), dashd's ({want_reply}, \
                 {want_status})"
            ));
        }
        self.ack()?;
        Ok(())
    }

    /// The P2P part (an acknowledgement every 2 s of it), then the stored blocks from the first
    /// one the node lacks (F01's pair after its parent), an acknowledgement every `every`, and a
    /// last one at the final tip.
    async fn schedule(&mut self, every: usize) -> io::Result<()> {
        let fixture = self.fixture;
        let p2p_pos = fixture.on_trajectory(&fixture.p2p_tip, 0).ok_or_else(|| {
            io::Error::other(format!("the P2P tip {} is not on the trajectory", fixture.p2p_tip))
        })?;
        // Stalled: no new tip for PATIENCE (regtest's index checks make a long sync slow).
        let (mut deadline, mut last) = (Instant::now() + PATIENCE, None);
        loop {
            let best = self.ack()?;
            if self.pos >= p2p_pos {
                break;
            }
            if last.as_ref() != Some(&best) {
                deadline = Instant::now() + PATIENCE;
            } else if Instant::now() > deadline {
                self.fail(format!("P2P sync stalled at {best}"));
                return Ok(());
            }
            last = Some(best);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let start = self.resume_at();
        self.stored_acks = Some(0);
        if let Some(f) = &fixture.f01 {
            // The crash kept F01's parent but lost the pair, or C only.
            let parent_before = start > 0 && fixture.submit[start - 1] == f.after;
            if parent_before && !self.known(&f.c_hash) {
                self.f01(f, self.known(&f.f_hash))?;
            }
        }
        for (i, hash) in fixture.submit.iter().enumerate().skip(start) {
            self.submit(hash)?;
            if let Some(f) = fixture.f01.as_ref().filter(|f| &f.after == hash) {
                self.f01(f, false)?;
            }
            if (i + 1) % every == 0 {
                self.ack()?;
            }
        }
        self.report.tip = self.ack()?;
        if self.report.tip != fixture.final_tip {
            self.fail(format!("ended at {}, not the fixture's tip", self.report.tip));
        }
        if let Some(f) = &fixture.f01 {
            let status = self.tip_status(&f.c_hash)?;
            let want = self.c_expected.clone().unwrap_or_else(|| f.dashd_c_status.clone());
            if status != want {
                self.fail(format!("F01: C ends {status}, dashd's {want}"));
            }
        }
        Ok(())
    }
}

/// Runs the schedule on the node in `datadir`, whose tip is at trajectory position `from` (0 for
/// a new node; C5 continues a recovered one), then stops it cleanly. `mark` sees every
/// acknowledgement. With `plant`, `rdashd` is a plant build that must arm that planted bug.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    rdashd: &Path,
    dashd: &Path,
    fixture: &Fixture,
    work: &Path,
    datadir: &Path,
    from: usize,
    extra: &[String],
    plant: Option<&str>,
    every: usize,
    mark: Marker<'_>,
) -> io::Result<Report> {
    let peer = Peer::start(dashd, fixture, work)?;
    let config = RustNodeConfig::with_binary(rdashd, datadir)
        .with_mocktime(Some(crate::MockTime::at(fixture.mocktime)))
        .with_connect(peer.p2p)
        .with_rpc(find_available_port())
        .with_args(node_args(fixture, extra))
        .with_plant(plant);
    let node = RustNode::start(config).await.map_err(io::Error::other)?;
    // A binary built without the feature ignores the variable, and its run would pass as if the
    // planted bug had been missed.
    if let Some(bug) = plant
        && !node.log().contains(&format!("ST3b: planted bug {bug} armed"))
    {
        return Err(io::Error::other(format!(
            "{} did not arm planted bug {bug}: not a plant build (--features planted-bugs)",
            rdashd.display()
        )));
    }
    let rpc = node.rpc_with_timeout(super::checks::RPC_PATIENCE).map_err(io::Error::other)?;
    let mut drive = Drive {
        rpc: &rpc,
        fixture,
        pos: from,
        report: Report::default(),
        mark,
        until_death: None,
        stored_acks: None,
        c_expected: None,
    };
    drive.schedule(every).await?;
    let mut report = drive.report;
    report.partial_batches = node.log().matches("Writing partial batch").count();
    let stopped = node.stop().await.map_err(io::Error::other)?;
    if !stopped.is_clean() {
        report.failures.push(format!("rdashd did not stop cleanly: {stopped:?}"));
    }
    peer.stop()?;
    Ok(report)
}

/// A fault run's schedule on a fresh node at `rpc` (S6 plan §1): [`run`]'s, with [`UntilDeath`].
/// Returns what it saw and the error that ended it, if one did; the caller judges whether the node
/// was armed then.
pub async fn until_death<'a>(
    rpc: &'a dyn NodeRpc,
    fixture: &'a Fixture,
    every: usize,
    mark: Marker<'a>,
    until: UntilDeath<'a>,
) -> (Report, Option<io::Error>) {
    let mut drive = Drive {
        rpc,
        fixture,
        pos: 0,
        report: Report::default(),
        mark,
        until_death: Some(until),
        stored_acks: None,
        c_expected: None,
    };
    let ended = drive.schedule(every).await.err();
    (drive.report, ended)
}

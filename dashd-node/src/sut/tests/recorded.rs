//! CH7b-2's recorded regtest chains: the O8 recorder's dashd v24 runs kept under
//! `test-vectors/statedigest/regtest-v24`, each node0's blocks connected through the node on the
//! run's settings. C14: a datadir synced past V20 under `-assumevalid` restarts. DEC-1028: the
//! EHF deployments follow dashd's after every block, across a reorg and a restart. C15 (§7.2 L1,
//! T-RD): runs with non-null commitments, rotation, MNHF signals and asset unlocks connect as
//! dashd connected them, and each mined quorum's roster is dashd's `quorum info`. CH7b-3 C7:
//! the runs with CbTx ChainLocks connect whole, step 10 verifying each.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;

use dash_consensus::ctx::DeploymentView as _;
use dash_consensus::deployments::ThresholdState;
use dash_params::DeploymentPos;
use dash_primitives::QuorumHash;
use dashd_chainstate::evo::cl::census_under;
use dashd_chainstate::evo::gates::counts_under;

use dashd_chainstate::connect::{
    BlockAt, DigestSink, INTERIM_REFUSAL, MuHashAt, g1_capability, g2_refusal_text, refusal_text,
};

use super::state_hash::{DigestRun, SharedSink, linear_clean, projected, read_records};
use super::*;

/// A recorded run's node0: its settings and its blocks, genesis first.
struct Recording {
    /// `extract.json`'s `chain_args`.
    args: Vec<String>,
    /// The network magic of the blk records.
    magic: [u8; 4],
    blocks: Vec<Arc<dash_consensus::Block>>,
    dir: PathBuf,
}

impl Recording {
    /// `test`'s node0. Its `blocks.dat` holds blk records: the network's magic, the size, the
    /// block.
    fn open(test: &str) -> Recording {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-vectors/statedigest/regtest-v24")
            .join(test)
            .join("node0");
        let extract: serde_json::Value =
            serde_json::from_reader(std::fs::File::open(dir.join("extract.json")).unwrap())
                .unwrap();
        let args = extract["chain_args"].as_array().expect("chain_args");
        let args = args.iter().map(|a| a.as_str().expect("an argument").to_owned()).collect();
        let raw = zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).unwrap());
        let raw = raw.unwrap();
        let (mut rest, mut magic, mut blocks) = (raw.as_slice(), None, Vec::new());
        while let Some((record, tail)) = rest.split_at_checked(8) {
            let at = <[u8; 4]>::try_from(&record[..4]).unwrap();
            assert_eq!(*magic.get_or_insert(at), at, "one network");
            let size = u32::from_le_bytes(record[4..].try_into().unwrap());
            let (mut bytes, tail) = tail.split_at(usize::try_from(size).unwrap());
            let block = dash_consensus::primitives::decode_block(&mut bytes).expect("a block");
            blocks.push(Arc::new(block.into_value()));
            rest = tail;
        }
        Recording {
            args,
            magic: magic.expect("a block"),
            blocks,
            dir,
        }
    }

    /// `golden.jsonl.zst`'s path.
    fn golden_path(&self) -> PathBuf {
        self.dir.join("golden.jsonl.zst")
    }

    /// `golden.jsonl`'s records.
    fn golden(&self) -> Vec<serde_json::Value> {
        let raw = zstd::decode_all(std::fs::File::open(self.dir.join("golden.jsonl.zst")).unwrap());
        let text = String::from_utf8(raw.unwrap()).unwrap();
        text.lines().map(|l| serde_json::from_str(l).expect("a record")).collect()
    }

    /// The blocks dashd connected (the golden's `C` records): height and hash.
    fn connected(&self) -> Vec<(u64, String)> {
        let golden = self.golden().into_iter().filter(|r| r["k"] == "C");
        golden.map(|r| (r["h"].as_u64().unwrap(), r["b"].as_str().unwrap().to_owned())).collect()
    }

    /// dashd's `quorum info` for each commitment, at the block that mined it.
    fn quorum_infos(&self) -> Vec<QuorumInfo> {
        let golden = self.golden().into_iter().filter(|r| r["rpc"] == "quorum_info");
        golden
            .map(|r| {
                let res = &r["res"];
                let hash = |v: &serde_json::Value| {
                    let hex = v.as_str().unwrap();
                    let mut run = self.blocks.iter().map(|b| b.hash());
                    run.find(|h| h.to_string() == hex).expect("a block of the run")
                };
                let llmq_type = u8::try_from(r["args"][0].as_u64().unwrap()).unwrap();
                let members = res["members"].as_array().unwrap().iter();
                QuorumInfo {
                    llmq_type: dash_params::LlmqType::new(llmq_type),
                    base: hash(&res["quorumHash"]),
                    index: i16::try_from(res["quorumIndex"].as_i64().unwrap()).unwrap(),
                    mined: hash(&res["minedBlock"]),
                    members: members.map(|m| m["proTxHash"].as_str().unwrap().to_owned()).collect(),
                }
            })
            .collect()
    }

    /// dashd's [`EhfState`] after each block it connected, by height (the golden's `C` records:
    /// `ehf`, null where V20 is not active at the block, and `dep`).
    fn ehf_states(&self) -> Vec<EhfState> {
        let golden = self.golden().into_iter().filter(|r| r["k"] == "C");
        let states: Vec<_> = (0_u64..)
            .zip(golden)
            .map(|(h, r)| {
                assert_eq!(r["h"].as_u64(), Some(h), "in turn");
                let s = &r["s"];
                let int = |v: &serde_json::Value| i32::try_from(v.as_i64().unwrap()).unwrap();
                let map = s["ehf"].as_array().map_or(&[][..], Vec::as_slice).iter();
                let map = map.map(|e| (u8::try_from(e[0].as_u64().unwrap()).unwrap(), int(&e[1])));
                let dep = s["dep"].as_array().unwrap().iter().map(|d| {
                    let name = d[0].as_str().unwrap().to_owned();
                    (name, d[1].as_str().unwrap().to_owned(), int(&d[2]))
                });
                EhfState {
                    map: map.collect(),
                    dep: dep.collect(),
                }
            })
            .collect();
        assert_eq!(states.len(), self.blocks.len(), "a record per block");
        states
    }

    /// A node in `dir` on the run's settings and `more`, on the run's network.
    fn node(&self, dir: &Path, more: &[&str], ui: Arc<Ui>) -> NodeSut {
        self.node_with(dir, more, ui, None)
    }

    /// [`node`](Self::node), its connector writing to `sink` from genesis when there is one.
    fn node_with(
        &self,
        dir: &Path,
        more: &[&str],
        ui: Arc<Ui>,
        sink: Option<Box<dyn DigestSink>>,
    ) -> NodeSut {
        self.node_on(dir, &self.args, more, ui, sink)
    }

    /// [`node_with`](Self::node_with) on the consensus arguments `args` (a [`Segment`]'s).
    fn node_on(
        &self,
        dir: &Path,
        args: &[String],
        more: &[&str],
        ui: Arc<Ui>,
        sink: Option<Box<dyn DigestSink>>,
    ) -> NodeSut {
        let argv: Vec<&str> = args.iter().map(String::as_str).chain(more.iter().copied()).collect();
        let sut = try_sut_at(dir, &argv, ui, self.clock(), sink).expect("accepted");
        assert_eq!(self.magic, sut.params.chain.message_start, "the network's magic");
        let genesis = dashd_chainstate::chain::genesis_block(&sut.params.chain);
        assert_eq!(self.blocks[0].hash(), genesis.hash(), "genesis first");
        sut
    }

    /// The node's clock: [`MOCKTIME`], or a day past the run's latest block when the run is
    /// later (`feature_dip3_deterministicmns` ran on the wall clock, without `-mocktime`), so no
    /// block is `time-too-new`. The state hashes do not move with it: no key reads the node's
    /// clock, and versionbits reads the blocks' median time past (OD-CP-44).
    fn clock(&self) -> i64 {
        let latest = self.blocks.iter().map(|b| i64::from(b.header().time())).max();
        latest.map_or(MOCKTIME, |t| MOCKTIME.max(t + 86_400))
    }

    /// The blocks after genesis.
    fn after_genesis(&self) -> &[Arc<dash_consensus::Block>] {
        &self.blocks[1..]
    }

    /// A node in `dir` that connects the run under `-assumevalid` at the last of 8,065 headers
    /// ground past its tip: two weeks of proof-equivalent time past every block, so none runs
    /// its script checks (`validation.cpp:2548-2577`) or with them the CbTx's steps 8 to 10 and
    /// the commitments' signature checks (`evo/specialtxman.cpp:876`, `:955`, `:984-1029`).
    fn assumed(&self, dir: &Path, log: Arc<Ui>) -> NodeSut {
        let one = [Segment {
            from: 0,
            args: self.args.clone(),
        }];
        self.assumed_on(dir, log, &one, None).0
    }

    /// [`assumed`](Self::assumed) over `segments` (OD-CP-44): each segment's blocks under its own
    /// consensus arguments, the node reopened at each boundary with `shared` re-armed, the ground
    /// headers sent again after each open. The node, and the argument sets it applied.
    fn assumed_on(
        &self,
        dir: &Path,
        log: Arc<Ui>,
        segments: &[Segment],
        shared: Option<&SharedSink>,
    ) -> (NodeSut, BTreeSet<Vec<String>>) {
        const AHEAD: u32 = 8_065;
        let probe = tempfile::tempdir().unwrap();
        let p = self.node(probe.path(), &[], ui()).params.chain.clone();
        let tip = self.blocks.last().unwrap();
        let mut headers: Vec<_> = self.after_genesis().iter().map(|b| *b.header()).collect();
        let t0 = tip.header().time();
        for i in 1..=AHEAD {
            let prev = headers.last().unwrap().hash();
            headers.push(grind(&p, regtest_header(prev, t0 + i)));
        }
        let at_best = format!("-assumevalid={}", headers.last().unwrap().hash());
        let (first, rest) = segments.split_first().expect("a segment");
        assert_eq!(first.from, 0, "the first segment starts at genesis");
        let mut applied = BTreeSet::from([first.args.clone()]);
        let mut sut = self.node_on(dir, &first.args, &[&at_best], log, shared.map(SharedSink::arm));
        imported(&mut sut);
        send_headers(&sut, &headers);
        for (h, b) in (1..).zip(self.after_genesis()) {
            if let Some(seg) = rest.iter().find(|seg| seg.from == h) {
                let sink = shared.map(|shared| shared.restart(view_tip(&sut)));
                sut = self.reopen(sut, dir, &seg.args, &[&at_best], sink);
                applied.insert(seg.args.clone());
                send_headers(&sut, &headers);
            }
            submit(&sut, b);
        }
        assert_eq!(view_tip(&sut), tip.hash(), "connected under -assumevalid");
        (sut, applied)
    }

    /// `sut` closed cleanly and its datadir and coins reopened on `args` and `more`, as the
    /// recorded node restarted with new arguments (OD-CP-44), its connector writing to `sink`.
    fn reopen(
        &self,
        mut sut: NodeSut,
        dir: &Path,
        args: &[String],
        more: &[&str],
        sink: Option<Box<dyn DigestSink>>,
    ) -> NodeSut {
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let mut argv = vec!["-regtest", "-printtoconsole=0"];
        argv.extend(args.iter().map(String::as_str));
        argv.extend_from_slice(more);
        let mut sut = NodeSut::unopened(dir, &argv, self.clock(), ui(), coins).expect("accepted");
        sut.pending_digest = sink;
        sut.open();
        loaded(&sut);
        imported(&mut sut);
        sut
    }

    /// Each recorded start's consensus arguments ([`consensus_args`]), from `chainargs.json`; the
    /// last start's are `extract.json`'s `chain_args`, as the recorder derived them.
    fn start_args(&self) -> Vec<Vec<String>> {
        let file = std::fs::File::open(self.dir.join("chainargs.json")).unwrap();
        let json: serde_json::Value = serde_json::from_reader(file).unwrap();
        let starts = json["starts"].as_array().expect("starts");
        let args: Vec<Vec<String>> =
            starts.iter().map(|s| consensus_args(s).unwrap_or_else(|e| panic!("{e}"))).collect();
        assert_eq!(args.last(), Some(&self.args), "the last start's are chain_args");
        args
    }

    /// For each block of the run, the start that connected it: in dashd's own state-hash log
    /// (an `H` per start), the start whose `tip` `C` its final chain keeps.
    fn start_at(&self) -> Vec<usize> {
        let records = read_records(&self.dir.join("digest.jsonl.zst"));
        let mut starts = Vec::with_capacity(records.len());
        let mut n = 0;
        for r in &records {
            if matches!(r, dash_statedigest::Record::Header(_)) {
                n += 1;
            }
            starts.push(n);
        }
        let chain = projected("dashd", &records);
        let hashes = chain.iter().map(|(_, o)| o.id.b.to_string());
        let blocks = self.blocks.iter().map(|b| b.hash().to_string());
        assert!(hashes.eq(blocks), "dashd's final chain is the run's blocks");
        chain.iter().map(|(at, _)| starts[*at] - 1).collect()
    }

    /// The run's segments (OD-CP-44): the heights each consensus-argument set connected.
    fn segments(&self) -> Vec<Segment> {
        segments_of(&self.start_args(), &self.start_at())
    }

    /// The distinct consensus-argument sets the recording connected blocks `0..end` under.
    fn recorded_sets(&self, end: usize) -> BTreeSet<Vec<String>> {
        let args = self.start_args();
        self.start_at()[..end].iter().map(|&s| args[s].clone()).collect()
    }

    /// The heights whose CbTx carries a non-null best ChainLock. A CbTx before V20 is below
    /// version 3, which carries none.
    fn chainlocked(&self) -> Vec<u32> {
        use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};

        (1..)
            .zip(self.after_genesis())
            .filter(|&(height, b)| {
                let facts = CoinbaseFacts::First(b.txs()[0].clone());
                let bls = dash_consensus::BlsCtx::BASIC;
                nonnull_coinbase_chainlock(&facts, height, true, bls).is_some()
            })
            .map(|(height, _)| height)
            .collect()
    }

    /// The CbTxs from 1 to `to` as step 10 reads them, decoded from their payload's bytes apart
    /// from the node: the heights whose CbTx is version 3 or later, each of which asks for the
    /// best ChainLock (`evo/specialtxman.cpp:143-151`), and `(h, h - diff - 1)` for each whose
    /// `bestCLSignature` is not null, the check step 10 makes when the node holds no best
    /// ChainLock (`:202`).
    fn cbtx_chainlocks(&self, to: u32) -> (Vec<u32>, Vec<(u32, u32)>) {
        let (mut v3, mut locks) = (Vec::new(), Vec::new());
        for (h, b) in (1..=to).zip(self.after_genesis()) {
            let payload = b.txs()[0].to_mutable().extra_payload().clone();
            if payload.len() < 2 || u16::from_le_bytes([payload[0], payload[1]]) < 3 {
                continue;
            }
            v3.push(h);
            let (diff, sig) = (payload[70], &payload[71..167]);
            assert!(diff < 253, "{h}: a one-byte difference");
            if sig != [0; 96] {
                locks.push((h, h - 1 - u32::from(diff)));
            }
        }
        (v3, locks)
    }
}

/// What dashd's `quorum info` gave for a commitment: its base block, index, mining block and
/// members' ProTx hashes, in order.
#[derive(Debug, PartialEq, Eq)]
struct QuorumInfo {
    llmq_type: dash_params::LlmqType,
    base: BlockHash,
    index: i16,
    mined: BlockHash,
    members: Vec<String>,
}

impl QuorumInfo {
    /// Route B's `GetQuorum(type, hash)` for this commitment at `sut`'s tip, as dashd would
    /// answer `quorum info` from it.
    fn at(&self, sut: &NodeSut) -> QuorumInfo {
        use dashd_chainstate::chain::OnV;

        let (llmq_type, hash) = (self.llmq_type, QuorumHash::from(*self.base.as_bytes()));
        let reply = sut.chain().unwrap().on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
            q.bind(o).quorum(llmq_type, &hash).map_err(|e| e.to_string()).map(|got| {
                got.map(|q| QuorumInfo {
                    llmq_type,
                    base: q.base_hash,
                    index: q.commitment.quorum_index().unwrap_or(0),
                    mined: q.mined_block,
                    members: q.members.iter().map(|m| m.pro_tx_hash.to_string()).collect(),
                })
            })
        });
        let got = reply.recv().expect("served").expect("no fault");
        got.unwrap_or_else(|| panic!("{self:?}: not mined on V's chain"))
    }
}

/// `feature_llmq_chainlocks.py`'s run: V20 at 200, CbTx ChainLocks from 309.
const CHAINLOCKS: &str = "feature_llmq_chainlocks";

/// CH7b-3 C5 (W-1), CH7b-2 C14's control turned around: with `-assumevalid=0` every block runs
/// its script checks (`validation.cpp:2548`), which `ProcessSpecialTxsInBlock` takes as
/// `fCheckCbTxMerkleRoots` (`:2669-2671`), so the CbTx's steps 8 to 10 run
/// (`evo/specialtxman.cpp:876`, `:984-1029`). The node holds no best ChainLock, so step 10
/// verifies the run's first CbTx ChainLock and the next; both connect, where the stub refused
/// the first with D2's text, the readiness refusal (AD-CH3-4).
#[test]
fn the_node_wires_the_attempt_verifier() {
    let run = Recording::open(CHAINLOCKS);
    let dir = tempfile::tempdir().unwrap();
    let (ui, lines) = logged_ui();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui);
    let locked = run.chainlocked();
    let first = usize::try_from(locked[0]).unwrap();
    assert_eq!(locked[1], locked[0] + 1, "{locked:?}: two CbTx ChainLocks in a row");
    imported(&mut sut);
    for b in &run.blocks[1..=first + 1] {
        submit(&sut, b);
    }
    assert_eq!(view_tip(&sut), run.blocks[first + 1].hash(), "both ChainLocks' blocks connected");
    assert!(!sut.shutdown.aborted(), "no AbortNode");
    let logged = lines.lock().unwrap().clone();
    assert!(!logs_a_readiness_refusal(&logged), "{logged:?}");
}

/// Whether `lines` hold a readiness refusal (`refusal_text`), whatever registry row it names.
fn logs_a_readiness_refusal(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains("rdashd cannot validate block"))
}

/// BL-699: the predicate sees G1's and G2's refusals, each naming AD-CH3-4, and no other line.
#[test]
fn the_refusal_predicate_sees_each_readiness_form() {
    use dash_params::{ChainParamsBuilder, Network, Release};
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    let at = BlockAt {
        hash: BlockHash::from([0x18; 32]),
        height: 1,
        prev: None,
        id: None,
    };
    for text in [
        refusal_text(&p, &at, &g1_capability("DIP0003")),
        g2_refusal_text(&p, &at, INTERIM_REFUSAL),
    ] {
        assert!(text.contains("(ALLOWED-DIFFS AD-CH3-4)"), "{text}");
        assert!(logs_a_readiness_refusal(&[format!("*** {text}")]), "{text}");
    }
    let ordinary = [
        format!("UpdateTip: new best={} height=1 version=0x20000000", at.hash),
        "Error: Unable to bind to 127.0.0.1:18444 on this computer.".to_owned(),
    ];
    assert!(!logs_a_readiness_refusal(&ordinary), "{ordinary:?}");
}

/// CH7b-2 C14 (CH7 §7.2 D3, the startup row): the run connects under `-assumevalid`
/// ([`Recording::assumed`]), so H4 still builds each mined quorum's members, and the blocks whose
/// CbTx carries a non-null ChainLock connect without step 10's verify (no script checks).
/// Reopened with default flags, the node loads at that tip, past V20, with no refusal.
#[test]
fn a_datadir_synced_past_v20_under_assumevalid_restarts() {
    let run = Recording::open(CHAINLOCKS);
    let tip = run.blocks.last().unwrap();
    let locked = run.chainlocked();
    assert!(locked.len() > 1, "{locked:?}: CbTx ChainLocks to connect past");
    let dir = tempfile::tempdir().unwrap();
    let (ui, lines) = logged_ui();
    let mut sut = run.assumed(dir.path(), Arc::clone(&ui));

    sut.steps.shutdown();
    let coins = Arc::clone(&sut.coins);
    drop(sut);
    coins.restart();
    let defaults = ["-regtest", "-printtoconsole=0"].into_iter().map(str::to_owned);
    let argv: Vec<String> = defaults.chain(run.args.iter().cloned()).collect();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
    loaded(&sut);
    imported(&mut sut);
    assert_eq!(view_tip(&sut), tip.hash(), "the tip, past V20");
    assert!(!sut.shutdown.aborted());
    let logged = lines.lock().unwrap().clone();
    assert!(!logged.iter().any(|l| l.contains("cannot validate")), "{logged:?}");
}

/// The EHF deployments after a block: the MNHF map the next block's stage reads, and
/// `[name, state, since]` of each versionbits deployment for the next block, as dashd's
/// `getdeploymentinfo` gives `status_next`.
#[derive(Debug, PartialEq, Eq)]
struct EhfState {
    map: Vec<(u8, i32)>,
    dep: Vec<(String, String, i32)>,
}

/// The node's [`EhfState`] after `hash`: `GetSignalsStage(block)` (dashd's stored map where no
/// signal has expired, as in these runs) and V's versionbits answers.
fn ehf_state(sut: &NodeSut, hash: BlockHash) -> EhfState {
    let state = sut.chain().unwrap().query(move |q| {
        let id = q.chain.reader().lookup(&hash).expect("indexed");
        let map = q.ehf_signals_stage(id).expect("GetForBlock answers");
        let positions = [
            (DeploymentPos::TestDummy, "testdummy"),
            (DeploymentPos::V24, "v24"),
            (DeploymentPos::EvoShares, "evo_shares"),
        ];
        let dep = positions.into_iter().map(|(pos, name)| {
            let answer = q.deployments.versionbits(Some(id), pos).expect("enabled");
            let state = match answer.state {
                ThresholdState::Defined => "defined",
                ThresholdState::Started => "started",
                ThresholdState::LockedIn => "locked_in",
                ThresholdState::Active => "active",
                ThresholdState::Failed => "failed",
            };
            (name.to_owned(), state.to_owned(), answer.since)
        });
        EhfState {
            map: map.iter().collect(),
            dep: dep.collect(),
        }
    });
    state.recv().expect("V serves")
}

/// Connects `run`'s block at height `h` under the node's checks: accepted, valid and the new
/// tip.
fn connect(sut: &NodeSut, run: &Recording, h: usize) {
    let b = &run.blocks[h];
    let r = sut.chain().unwrap().process_block(Arc::clone(b), true, None, true).recv();
    let r = r.expect("V replies");
    let connected = r.accepted && r.state.is_valid() && view_tip(sut) == b.hash();
    assert!(connected, "{h}: accepted {}, {}, tip {}", r.accepted, r.state, view_tip(sut));
}

/// [`connect`]s `run`'s blocks at `heights` in turn, the node's [`EhfState`] after each
/// `dashd`'s.
fn follow(sut: &NodeSut, run: &Recording, dashd: &[EhfState], heights: Range<usize>) {
    for h in heights {
        connect(sut, run, h);
        assert_eq!(ehf_state(sut, run.blocks[h].hash()), dashd[h], "after {h}");
    }
}

/// `feature_asset_locks.py`'s run: V24's MNHF signal at 188, asset unlocks from 189, V24 active
/// from 750, whose CbTx is the first of version 4.
const ASSET_LOCKS: &str = "feature_asset_locks";

/// DEC-1028. With no EHF source (`NoEhfSignals`) V24 stayed `DEFINED`, so block 750's version 4
/// CbTx got `bad-cbtx-version` (`evo/cbtx.cpp:45-51`) and the node stopped at 749. With the
/// node's MNHF maps, the run connects block by block under `-assumevalid=0`, and after each block
/// the node's map and its TESTDUMMY, V24 and EVO_SHARES states are dashd's: the signal at 188
/// starts V24 at the first period end at or past it, 249 (`versionbits.cpp:69`, `:95`, with the
/// signal height from `:238-251`), so it is `STARTED` from 250, `LOCKED_IN` from 500 and active
/// from 750.
#[test]
fn feature_asset_locks_ehf_deployments_are_dashds_after_every_block() {
    let run = Recording::open(ASSET_LOCKS);
    let dashd = run.ehf_states();
    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    assert_eq!(ehf_state(&sut, run.blocks[0].hash()), dashd[0], "at genesis");
    follow(&sut, &run, &dashd, 1..run.blocks.len());
}

/// DEC-1028, a reorg: `invalidateblock` of the signal's block 188 takes the tip back to 187,
/// whose stage holds no signal and where V24 is `DEFINED`, as dashd's after 187;
/// `reconsiderblock` reconnects to 751, 750's version 4 CbTx again, and V24 is active as before.
/// The maps stay per block and the versionbits cache is keyed by block (Core's
/// `ThresholdConditionCache`), so neither the undo nor the reconnect leaves a stale answer.
#[test]
fn invalidating_the_mnhf_signals_block_takes_v24_back_until_it_is_reconsidered() {
    let run = Recording::open(ASSET_LOCKS);
    let dashd = run.ehf_states();
    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    follow(&sut, &run, &dashd, 1..752);
    let v = sut.chain().unwrap();
    let signal = run.blocks[188].hash();
    let _ = v.invalidate_block(signal).recv();
    wait_for("188 undone", || view_tip(&sut) == run.blocks[187].hash());
    assert_eq!(ehf_state(&sut, run.blocks[187].hash()), dashd[187], "after 187");
    let _ = v.reconsider_block(signal, false).recv();
    wait_for("751 reconnected", || view_tip(&sut) == run.blocks[751].hash());
    for h in [249, 499, 749, 750, 751] {
        assert_eq!(ehf_state(&sut, run.blocks[h].hash()), dashd[h], "after {h}");
    }
    assert!(!sut.shutdown.aborted());
}

/// DEC-1028, a restart: a node stopped at 749, V24 active for 750, reopens with V's versionbits
/// cache empty and the MNHF manager's too; its source answers from the stored maps, so its
/// states after 749 are dashd's, and 750 to the run's tip connect, each with dashd's after it.
#[test]
fn a_node_reopened_at_749_reads_its_mnhf_maps_and_connects_750() {
    let run = Recording::open(ASSET_LOCKS);
    let dashd = run.ehf_states();
    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    follow(&sut, &run, &dashd, 1..750);

    sut.steps.shutdown();
    let coins = Arc::clone(&sut.coins);
    drop(sut);
    coins.restart();
    let flags = ["-regtest", "-printtoconsole=0", "-assumevalid=0"].into_iter().map(str::to_owned);
    let argv: Vec<String> = flags.chain(run.args.iter().cloned()).collect();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
    loaded(&sut);
    imported(&mut sut);
    assert_eq!(view_tip(&sut), run.blocks[749].hash(), "reopened at 749");
    assert_eq!(ehf_state(&sut, run.blocks[749].hash()), dashd[749], "after 749");
    follow(&sut, &run, &dashd, 750..run.blocks.len());
}

/// Where step 10's census and the sequence decoded from the CbTxs, each check valid, first part:
/// the lower attempt height of the first two entries that differ, or of the first entry one
/// holds past the other's end; `None` where they are equal.
fn census_mismatch(census: &[(u32, u32, Option<bool>)], decoded: &[(u32, u32)]) -> Option<u32> {
    let decoded: Vec<_> = decoded.iter().map(|&(h, cl)| (h, cl, Some(true))).collect();
    (0..census.len().max(decoded.len())).find_map(|i| {
        let (got, want) = (census.get(i), decoded.get(i));
        (got != want).then(|| got.into_iter().chain(want).map(|e| e.0).min().expect("an entry"))
    })
}

/// The census of the node whose data directory is `dir`: its step-10 verifier's checks.
fn census(dir: &Path) -> Vec<(u32, u32, Option<bool>)> {
    census_under(dir).expect("the node's census").checks.lock().clone()
}

/// Step 10's observations over a replay's starts, in replay order: each open registers fresh
/// cells and a fresh census ([`counts_under`], [`census_under`]), so each start's are taken
/// while its node is still open.
#[derive(Default)]
struct StepTenSeen {
    asks: Vec<(u32, Option<i32>)>,
    shortcuts: Vec<(u32, dashd_chainstate::evo::cl::ClPass)>,
    checks: Vec<(u32, u32, Option<bool>)>,
}

impl StepTenSeen {
    /// Adds the observations of the open node whose data directory is `dir`.
    fn take(&mut self, dir: &Path) {
        let roots = counts_under(dir).expect("the node's cells").root_checks();
        self.asks.extend(roots.cl_asks);
        self.shortcuts.extend(roots.cl_shortcuts);
        self.checks.extend(census(dir));
    }
}

/// Step 10 over every start of a replay (`seen`), connected to `to`, against `run`'s CbTxs
/// decoded: holding no best ChainLock, the node asks for one at each CbTx of version 3 or later
/// and gets Core's null lock, takes neither shortcut, and verifies each non-null
/// `bestCLSignature`, valid, in order. Returns the verifies.
fn step_ten_checks(run: &Recording, seen: &StepTenSeen, to: u32) -> usize {
    let (v3, locks) = run.cbtx_chainlocks(to);
    let nulls: Vec<_> = v3.iter().map(|&h| (h, None)).collect();
    assert_eq!(seen.asks, nulls, "an ask at each v3 CbTx, Core's null lock each time");
    assert!(seen.shortcuts.is_empty(), "{:?}", seen.shortcuts);
    assert_eq!(census_mismatch(&seen.checks, &locks), None, "{:?}", seen.checks);
    locks.len()
}

/// C15's replay of `test` (CH7 §7.2 L1, T-RD), whole since CH7b-3 C7. dashd connected each
/// recorded block in turn (the golden's `C` records). With `-assumevalid=0` the node connects
/// each in turn too, to the run's tip: every CbTx's MN-list and quorum roots checked (steps 8 and
/// 9 refuse a root that differs), its best ChainLock (step 10, [`step_ten_checks`]), each
/// commitment's members and signatures, and the MNHF signals' and asset unlocks' quorum
/// signatures through route C. Each commitment's roster, read through route B at the node's tip,
/// is dashd's `quorum info`. A run with a CbTx ChainLock connects on a node under
/// `-assumevalid` too ([`Recording::assumed_on`]), which skips step 10 with the script checks, and
/// each roster is dashd's there as well. Both nodes write state-hash logs from genesis with
/// `muhash` at every block (CH7a-5b C3), each equal to dashd's golden under I4 ([`linear_clean`])
/// over the whole run, genesis's `C` its only one at height 0 (I3). Returns the commitments and
/// step 10's verifies.
///
/// CH7b-3 C13 (PLAN-5b I7): all thirteen of I7's chains replay through here, each whole on the
/// checked node, `feature_llmq_is_cl_conflicts` among them. Every replay writes its log through
/// [`DigestRun::shared`]'s wrapper. Eleven meet I3's lifecycle through it: one load takes the
/// sink, on a fresh datadir, with no restart. The recorded nodes of
/// `feature_evo_shares_activation` and `feature_mnehf` restarted with other consensus arguments,
/// which that lifecycle cannot replay whole (a restart drops its sink, and a later segment does
/// not start on a fresh datadir), so those two use OD-CP-44's restart extension
/// ([`SharedSink::restart`]): one log across their starts.
fn replay(test: &str) -> (usize, usize) {
    let run = Recording::open(test);
    let order = (0_u64..).zip(&run.blocks).map(|(h, b)| (h, b.hash().to_string()));
    assert_eq!(run.connected(), order.collect::<Vec<_>>(), "dashd connected each in turn");
    let quorums = run.quorum_infos();
    let all = run.blocks.len();
    let segments = run.segments();
    let last = |n: usize| i64::try_from(n).unwrap() - 1;

    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let (log, shared) = DigestRun::shared(logs.path(), MuHashAt::Every);
    let checked_args = ["-assumevalid=0"];
    let (first, rest) = segments.split_first().expect("a segment");
    let mut applied = BTreeSet::from([first.args.clone()]);
    let mut sut = run.node_on(dir.path(), &first.args, &checked_args, ui(), Some(shared.arm()));
    imported(&mut sut);
    let mut seen = StepTenSeen::default();
    for h in 1..all {
        if let Some(seg) = rest.iter().find(|seg| seg.from == h) {
            seen.take(dir.path());
            let sink = shared.restart(view_tip(&sut));
            sut = run.reopen(sut, dir.path(), &seg.args, &checked_args, Some(sink));
            applied.insert(seg.args.clone());
        }
        connect(&sut, &run, h);
    }
    for q in &quorums {
        assert_eq!(q.at(&sut), *q);
    }
    linear_clean(&log, &run.golden_path(), 0, last(all));
    check_applied(&run.recorded_sets(all), &applied).unwrap_or_else(|e| panic!("{e}"));
    seen.take(dir.path());
    let tip = u32::try_from(all - 1).unwrap();
    let verifies = step_ten_checks(&run, &seen, tip);
    if verifies > 0 {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let (log, shared) = DigestRun::shared(logs.path(), MuHashAt::Every);
        let (sut, applied) = run.assumed_on(dir.path(), ui(), &segments, Some(&shared));
        for q in &quorums {
            assert_eq!(q.at(&sut), *q);
        }
        linear_clean(&log, &run.golden_path(), 0, last(all));
        check_applied(&run.recorded_sets(all), &applied).unwrap_or_else(|e| panic!("{e}"));
    }
    (quorums.len(), verifies)
}

/// The keys of a recorded start's argv that set consensus parameters (OD-CP-44): a restart with
/// other values changes what the next blocks connect under.
const CONSENSUS_KEYS: [&str; 8] = [
    "budgetparams",
    "dip3params",
    "llmqtestinstantsenddip0024",
    "llmqtestinstantsendparams",
    "llmqtestparams",
    "llmqtestplatformparams",
    "testactivationheight",
    "vbparams",
];

/// The other keys the recorder's starts carry: logging, RPC, P2P, wallet and index settings.
const OTHER_KEYS: [&str; 42] = [
    "acceptnonstdtxn",
    "bind",
    "connect",
    "createwalletbackups",
    "datadir",
    "debug",
    "debugexclude",
    "discover",
    "dnsseed",
    "fallbackfee",
    "fixedseeds",
    "keypool",
    "listenonion",
    "loglevel",
    "logsourcelocations",
    "logthreadnames",
    "logtimemicros",
    "maxsigcachesize",
    "natpmp",
    "par",
    "parbls",
    "peertimeout",
    "persistmempool",
    "port",
    "printtoconsole",
    "recordstatedecisions",
    "reindex",
    "rpcdoccheck",
    "rpcport",
    "rpcservertimeout",
    "rpcthreads",
    "server",
    "shrinkdebugfile",
    "spentindex",
    "sporkkey",
    "statedigestlog",
    "tinyblk",
    "uacomment",
    "unsafesqlitesync",
    "upnp",
    "v2transport",
    "whitelist",
];

/// A recorded start's consensus arguments as `-key=value`, its command line's first and then its
/// config's network section, the order the recorder's `chain_args` lists them in. A key in
/// neither [`CONSENSUS_KEYS`] nor [`OTHER_KEYS`] is an error: the rig never drops an argument it
/// cannot classify.
fn consensus_args(start: &serde_json::Value) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut add = |key: &str, values: &serde_json::Value| -> Result<(), String> {
        if CONSENSUS_KEYS.contains(&key) {
            for v in values.as_array().ok_or("a list of values")? {
                args.push(format!("-{key}={}", v.as_str().ok_or("a string value")?));
            }
            Ok(())
        } else if OTHER_KEYS.contains(&key) {
            Ok(())
        } else {
            Err(format!("an unclassified argument: {key}"))
        }
    };
    for (key, values) in start["cmdline"].as_object().ok_or("a cmdline")? {
        add(key, values)?;
    }
    if let Some(config) = start.get("config") {
        for (key, values) in config.as_object().ok_or("a config")? {
            match key.split_once('.') {
                Some(("regtest", key)) => add(key, values)?,
                None if key == "regtest" => {}
                _ => return Err(format!("a config key outside regtest's section: {key}")),
            }
        }
    }
    Ok(args)
}

/// The heights from `from` on (to the next segment) that one consensus-argument set connected.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    from: usize,
    args: Vec<String>,
}

/// The segments of a run whose block `h` start `start_at[h]` connected, each start's consensus
/// arguments `args[start]`: consecutive heights under equal arguments are one segment.
fn segments_of(args: &[Vec<String>], start_at: &[usize]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for (h, &start) in start_at.iter().enumerate() {
        if segments.last().is_none_or(|seg| seg.args != args[start]) {
            segments.push(Segment {
                from: h,
                args: args[start].clone(),
            });
        }
    }
    segments
}

/// OD-CP-44's fail-closed check: every consensus-argument set the recording connected blocks
/// under was applied by the replay.
fn check_applied(
    recorded: &BTreeSet<Vec<String>>,
    applied: &BTreeSet<Vec<String>>,
) -> Result<(), String> {
    let missing: Vec<_> = recorded.difference(applied).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the replay applied {} of the recording's {} consensus-argument sets; missing {missing:?}",
            applied.intersection(recorded).count(),
            recorded.len()
        ))
    }
}

/// OD-CP-44's fail-closed check: a replay that applied fewer consensus-argument sets than the
/// recording connected under is an error, the missing set named; all of them, or more, pass.
#[test]
fn a_replay_missing_a_recorded_argument_set_fails() {
    let set = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let early = set(&["-vbparams=mn_rr:0:999999999999:0:20:16:12:5:1"]);
    let late = set(&[]);
    let recorded = BTreeSet::from([early.clone(), late.clone()]);
    let finals_only = BTreeSet::from([late.clone()]);
    let err = check_applied(&recorded, &finals_only).unwrap_err();
    assert!(err.contains("1 of the recording's 2"), "{err}");
    assert!(err.contains("mn_rr"), "{err}");
    assert_eq!(check_applied(&recorded, &recorded), Ok(()));
    let more = BTreeSet::from([early, late, set(&["-dip3params=2:2"])]);
    assert_eq!(check_applied(&recorded, &more), Ok(()));
}

/// Consecutive blocks under equal arguments are one segment, whichever start connected them; a
/// start whose arguments differ from the last one's opens a new segment.
#[test]
fn segments_split_where_the_arguments_change() {
    let a = vec!["-vbparams=x".to_owned()];
    let b: Vec<String> = Vec::new();
    let args = [a.clone(), a.clone(), b.clone(), a.clone()];
    let seg = |from, args: &Vec<String>| Segment {
        from,
        args: args.clone(),
    };
    assert_eq!(
        segments_of(&args, &[0, 0, 1, 1, 2, 2, 3]),
        vec![seg(0, &a), seg(4, &b), seg(6, &a)]
    );
    assert_eq!(segments_of(&args, &[0]), vec![seg(0, &a)]);
}

/// A start's consensus arguments: the command line's, then the config's regtest section, other
/// keys dropped; an unclassified key, or a config key outside regtest's section, is an error.
#[test]
fn consensus_args_keep_the_consensus_keys_and_refuse_unknown_ones() {
    let start = serde_json::json!({
        "cmdline": {"datadir": ["/d"], "vbparams": ["a:1", "b:2"], "debug": ["all"]},
        "config": {"regtest": ["1"], "regtest.port": ["1"], "regtest.dip3params": ["2:2"]},
    });
    assert_eq!(
        consensus_args(&start),
        Ok(vec!["-vbparams=a:1".into(), "-vbparams=b:2".into(), "-dip3params=2:2".into()])
    );
    let unknown = serde_json::json!({"cmdline": {"newconsensusknob": ["1"]}});
    assert!(consensus_args(&unknown).unwrap_err().contains("newconsensusknob"));
    let outside = serde_json::json!({"cmdline": {}, "config": {"main.vbparams": ["x"]}});
    assert!(consensus_args(&outside).unwrap_err().contains("main.vbparams"));
}

/// `feature_dip4_coinbasemerkleroots.py`'s run: DIP0008 at 432, no V20 and so no CbTx
/// ChainLock; six commitments.
#[test]
fn feature_dip4_coinbasemerkleroots_connects_as_dashd_did() {
    assert_eq!(replay("feature_dip4_coinbasemerkleroots"), (6, 0));
}

/// `feature_llmq_rotation.py`'s run: eleven commitments, the rotated ones at 396, 420 and 444
/// among them, and a CbTx ChainLock in each block from 300, 153 verifies.
#[test]
fn feature_llmq_rotation_connects_as_dashd_did() {
    assert_eq!(replay("feature_llmq_rotation"), (11, 153));
}

/// `feature_mnehf.py`'s run: one-member quorums (`-llmqtestparams=1:1`), MNHF signals at 187
/// and 247, CbTxs of version 3 from 100 with no ChainLock; two commitments.
#[test]
fn feature_mnehf_connects_as_dashd_did() {
    assert_eq!(replay("feature_mnehf"), (2, 0));
}

/// `feature_asset_locks.py`'s run: asset locks, unlocks from 189, V24 active from 750 on its
/// MNHF signal (DEC-1028), CbTxs of version 3 from 100 with no ChainLock; 23 commitments.
#[test]
fn feature_asset_locks_connects_as_dashd_did() {
    assert_eq!(replay(ASSET_LOCKS), (23, 0));
}

/// `feature_llmq_evo.py`'s run: ten commitments, the rotated ones at 228 and 252 among them,
/// and CbTx ChainLocks from 212, 160 verifies.
#[test]
fn feature_llmq_evo_connects_as_dashd_did() {
    assert_eq!(replay("feature_llmq_evo"), (10, 160));
}

/// `feature_llmq_chainlocks.py`'s run: six commitments, and CbTx ChainLocks from 309, 44
/// verifies.
#[test]
fn feature_llmq_chainlocks_connects_as_dashd_did() {
    assert_eq!(replay(CHAINLOCKS), (6, 44));
}

/// `feature_governance_cl.py`'s run: one commitment, and CbTx ChainLocks from 164, 59 verifies.
/// The node's helper has no masternode sync (D18's regime), so it never requires the superblocks'
/// validation, and 200 connects. That is Core's lenient path for a node that has not synced:
/// `IsSuperblockValidationRequired`'s `!mn_sync.IsSynced()` branch (`evo/chainhelper.cpp:47-49`)
/// gives `SuperBlockCheckType::NoCheck` (`validation.cpp:2852-2854`). Past the superblock max
/// bounds (`masternode/payments.cpp:348-353`), `IsBlockValueValid` then returns valid (`:362`),
/// and `IsBlockPayeeValid` does too after the masternode payment (`:421`).
#[test]
fn feature_governance_cl_connects_as_dashd_did() {
    assert_eq!(replay("feature_governance_cl"), (1, 59));
}

/// `feature_llmq_is_cl_conflicts.py`'s run, which node0 connected in turn: no V20 and so no CbTx
/// ChainLock; three commitments.
#[test]
fn feature_llmq_is_cl_conflicts_connects_as_dashd_did() {
    assert_eq!(replay("feature_llmq_is_cl_conflicts"), (3, 0));
}

/// C7's census controls (planted-bugs): the verifier skips the check of the run's first CbTx
/// ChainLock, or makes it twice, once; the check answers valid either way, so the node connects
/// past it, and the comparator finds the census parting from the decoded sequence at that block.
#[cfg(feature = "planted-bugs")]
fn the_census_parts_at_the_first_cbtx_chainlock(bug: dashd_chainstate::Planted) {
    let run = Recording::open(CHAINLOCKS);
    let first = run.chainlocked()[0];
    let to = first + 1;
    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    sut.planted = Some(bug);
    assert_eq!(sut.restart(Teardown::Clean), 2);
    loaded(&sut);
    imported(&mut sut);
    for h in 1..=usize::try_from(to).unwrap() {
        connect(&sut, &run, h);
    }
    let (_, locks) = run.cbtx_chainlocks(to);
    assert_eq!(locks.first().map(|l| l.0), Some(first), "{locks:?}");
    let census = census(dir.path());
    assert_eq!(census_mismatch(&census, &locks), Some(first), "{bug}: {census:?}");
}

#[cfg(feature = "planted-bugs")]
#[test]
fn the_census_rejects_a_suppressed_verify() {
    the_census_parts_at_the_first_cbtx_chainlock(dashd_chainstate::Planted::ClCensusSuppressOnce);
}

#[cfg(feature = "planted-bugs")]
#[test]
fn the_census_rejects_a_duplicated_verify() {
    the_census_parts_at_the_first_cbtx_chainlock(dashd_chainstate::Planted::ClCensusDuplicateOnce);
}

/// §8 answer 9 (OD-CP-45): a plant makes H11's credit-pool read Core's `GetCreditPool` exception
/// at `feature_asset_locks`' block 500 (V20), the staged delta unchanged. The record fails, not
/// the attempt (CH3a D137): the failure reaches V's abort path and trips the node latch, and the
/// connect's outcome goes on unchanged; V stops on the latch after it.
/// - (a) one `AbortNode`, with Core's `GetCreditPool` text;
/// - (b) V checks block 500 `Valid`, once;
/// - (c) the log's last line is 499's `C`;
/// - (d1) V's published tip stays 499: the stop comes before the tip moves;
/// - (d2) a clean restart reopens at the last tip flushed before the plant (genesis when none
///   was);
/// - (d3) the store takes no write after the latch: its tip and evo tip are the same at the
///   abort and after the close, whose shutdown flush the latch refuses.
///
/// (d1) to (d3) hold because the record's `AbortNode` trips the node latch (`AbortPath`), which
/// V's stop, the shutdown flushes and the stores each read: with the trip removed, V publishes
/// 500 and the shutdown flush stores it.
#[cfg(feature = "test-support")]
#[test]
fn a_planted_pool_error_fails_the_record_and_keeps_the_connect() {
    const AT: usize = 500;
    let run = Recording::open(ASSET_LOCKS);
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let (log, sink) = DigestRun::new(logs.path(), MuHashAt::Every);
    let (ui, lines) = logged_ui();
    let mut argv = vec!["-regtest", "-printtoconsole=0", "-assumevalid=0"];
    argv.extend(run.args.iter().map(String::as_str));
    let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    let mut sut = NodeSut::unopened(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
    sut.pending_digest = Some(sink);
    sut.h11_pool_error_at = Some(u32::try_from(AT).unwrap());
    sut.open();
    imported(&mut sut);
    for h in 1..AT {
        connect(&sut, &run, h);
    }
    assert!(!sut.shutdown.aborted());
    // The last tip flushed before the plant: none stored, the load starts at genesis.
    let flushed = stored_tip(&sut);
    let flushed_at = flushed.unwrap_or_else(|| run.blocks[0].hash());
    let checked = Arc::new(Checked::default());
    assert!(sut.register_listener(Arc::clone(&checked) as _));
    let b = &run.blocks[AT];
    // The record's AbortNode starts the shutdown, which may end V before it answers, as in
    // `consensusxcheck_stops_the_node_on_a_planted_memo`.
    let _ = sut.chain().unwrap().process_block(Arc::clone(b), true, None, true).recv();
    wait_for("AbortNode ran", || sut.shutdown.aborted());
    let at_abort = (stored_tip(&sut), stored_evo_tip(&sut));

    // (a)
    let logged = lines.lock().unwrap().clone();
    let aborts: Vec<_> = logged.iter().filter(|l| l.contains("*** ")).collect();
    assert_eq!(aborts.len(), 1, "one AbortNode: {aborts:?}");
    assert!(aborts[0].contains("GetCreditPool"), "{aborts:?}");
    // (b)
    sut.sync_signals();
    let states = checked.of(&b.hash());
    assert_eq!(states.len(), 1, "block 500 checked once: {states:?}");
    assert!(states[0].is_valid(), "block 500 checked: {}", states[0]);
    // (c)
    let records = log.records();
    let last = records.last().expect("a record");
    let dash_statedigest::Record::Outcome(last) = last else {
        panic!("{last:?}")
    };
    assert_eq!((last.kind(), last.id.h), (dash_statedigest::Kind::Connect, 499), "the last line");
    // (d1)
    let published = sut.chain().unwrap().tip_watch().get();
    assert_eq!(published, Some(run.blocks[AT - 1].hash()), "V's published tip");
    // (d3)
    sut.steps.shutdown();
    let at_close = (stored_tip(&sut), stored_evo_tip(&sut));
    assert_eq!(at_close, at_abort, "a write after the latch");
    // (d2)
    sut.coins.restart();
    sut.open();
    loaded(&sut);
    assert_eq!(view_tip(&sut), flushed_at, "reopened at the last flush before the plant");
    // Not the claim, a fact about this rig's flush cadence: no flush had stored a tip by block
    // 499, so the reopen is at genesis.
    assert_eq!(flushed, None, "this rig's flush cadence");
}

/// `feature_dip3_deterministicmns.py`'s run (C3, never through `NodeSut` before): DIP0003's
/// registrations, updates and collateral spends, no commitment. It ran on the wall clock, so the
/// node's clock follows it ([`Recording::clock`]).
#[test]
fn feature_dip3_deterministicmns_state_hashes_are_dashds() {
    assert_eq!(replay("feature_dip3_deterministicmns"), (0, 0));
}

/// `feature_evo_shares_activation.py`'s run: one commitment at 154, and CbTx ChainLocks from 176,
/// 66 verifies.
#[test]
fn feature_evo_shares_activation_state_hashes_are_dashds() {
    assert_eq!(replay("feature_evo_shares_activation"), (1, 66));
}

/// `feature_governance.py`'s run: V20 from 160, no commitment.
#[test]
fn feature_governance_state_hashes_are_dashds() {
    assert_eq!(replay("feature_governance"), (0, 0));
}

/// `feature_llmq_simplepose.py`'s run: no V20, 34 commitments.
#[test]
fn feature_llmq_simplepose_state_hashes_are_dashds() {
    assert_eq!(replay("feature_llmq_simplepose"), (34, 0));
}

/// `feature_masternode_shares.py`'s run: 2,028 blocks, V20 from 100, no commitment.
#[test]
fn feature_masternode_shares_state_hashes_are_dashds() {
    assert_eq!(replay("feature_masternode_shares"), (0, 0));
}

/// Every block V checked (`BlockChecked`), with its state.
#[derive(Default)]
struct Checked(std::sync::Mutex<Vec<(BlockHash, dashd_chainstate::chain::BlockState)>>);

impl Checked {
    /// The states `hash` was checked with, in order.
    fn of(&self, hash: &BlockHash) -> Vec<dashd_chainstate::chain::BlockState> {
        let seen = self.0.lock().unwrap();
        seen.iter().filter(|(h, _)| h == hash).map(|(_, s)| s.clone()).collect()
    }
}

impl ValidationInterface for Checked {
    fn block_checked(
        &self,
        block: &dash_consensus::Block,
        state: &dashd_chainstate::chain::BlockState,
        _: bool,
    ) {
        self.0.lock().unwrap().push((block.hash(), state.clone()));
    }
}

/// RP1a-5: `getcreditpoolinfo`'s reads (`ChainQuery::credit_pool_info`; the RPC's height
/// resolution around it is the DIFF's, `rdashd/tests/rpc_diff.rs`) at
/// every block of `feature_asset_locks.py`'s run, connected block by block, are dashd's recorded
/// answers (the golden's `creditpool` records, from V20 at 100): the pool after the block, the
/// window start (100 blocks below, genesis at 100) and its balance, the unlocks in the window, and
/// V24's limit from 750, relative to the window start's balance. Below V20 the pool is empty.
#[test]
fn feature_asset_locks_credit_pools_are_dashds() {
    const COIN: i64 = 100_000_000;
    let run = Recording::open(ASSET_LOCKS);
    let sat = |v: &serde_json::Value| (v.as_f64().unwrap() * COIN as f64).round() as i64;
    // `(balance, currentlimit, window.unlocked, window.height, window.balance)` by height.
    let golden: std::collections::HashMap<u64, (i64, i64, i64, i64, i64)> = run
        .golden()
        .into_iter()
        .filter(|r| r["k"] == "S" && r["rpc"] == "creditpool")
        .map(|r| {
            let (res, w) = (&r["res"], &r["res"]["window"]);
            let row = (
                sat(&res["balance"]),
                sat(&res["currentlimit"]),
                sat(&w["unlocked"]),
                w["height"].as_i64().unwrap(),
                sat(&w["balance"]),
            );
            (r["h"].as_u64().unwrap(), row)
        })
        .collect();
    assert_eq!(golden.len(), run.blocks.len() - 100, "a record per V20 block");
    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    for h in 1..run.blocks.len() {
        connect(&sut, &run, h);
    }
    for (h, block) in (0_u64..).zip(&run.blocks) {
        let hash = block.hash();
        let got = sut.chain().unwrap().query(move |q| {
            let id = q.chain.reader().lookup(&hash).expect("indexed");
            q.credit_pool_info(id).expect("the store reads").expect("a pool")
        });
        let info = got.recv().expect("V serves");
        assert_eq!(info.window_blocks, 100, "{h}");
        let (start, start_balance) =
            info.window_start.map_or((-1, 0), |(height, b)| (i64::from(height), b));
        let pool = &info.pool;
        let ours = (pool.locked, pool.current_limit, pool.lately_unlocked, start, start_balance);
        // Below V20 (no record): the empty pool, and no balance at the window start.
        let dashd = golden.get(&h).copied().unwrap_or((0, 0, 0, start, 0));
        assert_eq!(ours, dashd, "at {h}");
    }
    let limit = |h| golden[&h].1;
    assert_eq!((limit(749), limit(750)), (2_000 * COIN, 210_754_994_300), "V24's rule at 750");
    assert!(golden.values().any(|g| g.2 > 0), "unlocks in a window");
}

/// A stored recording whose node0 chain carries a type 7 or 9 tx, as EV1-J's L2-R1 census pins
/// it: the heights of its type 7 txs, of its type 9 txs by payload version (1, then 2), and the
/// CbTx ChainLock of each type 7 block, `(h, the locked height)`, where its CbTx carries one.
struct Carrying {
    test: &'static str,
    type7: &'static [u32],
    type9_v1: &'static [u32],
    type9_v2: &'static [u32],
    type7_locks: &'static [(u32, u32)],
}

/// EV1-J L2-R1's census (the J2 delta's F1, OD-CP-73): every stored recording that carries a type
/// 7 or 9 tx, 9 type 7 and 19 type 9 (14 of version 1, 5 of version 2).
const CARRYING: [Carrying; 7] = [
    Carrying {
        test: ASSET_LOCKS,
        type7: &[188],
        type9_v1: &[189, 212, 238, 381, 381, 381, 381, 383, 484, 645, 646, 812, 814, 816],
        type9_v2: &[969, 970, 971, 972, 976],
        type7_locks: &[],
    },
    Carrying {
        test: "feature_evo_shares_activation",
        type7: &[176],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[(176, 174)],
    },
    Carrying {
        test: "feature_governance_cl",
        type7: &[181],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[(181, 180)],
    },
    Carrying {
        test: CHAINLOCKS,
        type7: &[332],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[(332, 329)],
    },
    Carrying {
        test: "feature_llmq_evo",
        type7: &[221],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[(221, 220)],
    },
    Carrying {
        test: "feature_llmq_rotation",
        type7: &[301],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[(301, 300)],
    },
    Carrying {
        test: "feature_mnehf",
        type7: &[187, 187, 247],
        type9_v1: &[],
        type9_v2: &[],
        type7_locks: &[],
    },
];

/// EV1-J L2-R1's census, regtest only: the O8 recorder's (v24.0.0-rc.3-8-g0a36fab32cb5) 18
/// stored node0 chains decoded, exactly the seven of [`CARRYING`] carry a type 7 or 9 tx, each
/// at the pinned heights and payload versions, and each type 7 block's CbTx carries the pinned
/// ChainLock as step 10 reads it ([`Recording::cbtx_chainlocks`]), so [`step_ten_checks`] holds
/// the replay's verify of it. The heights are positions in the chain the replay connects.
///
/// That the replay connects each of these blocks is the replay's own asserts, per recording (the
/// tests by name: `feature_asset_locks_connects_as_dashd_did`, `feature_mnehf_connects_as_dashd_did`,
/// `feature_llmq_chainlocks_connects_as_dashd_did`, `feature_llmq_rotation_connects_as_dashd_did`,
/// `feature_llmq_evo_connects_as_dashd_did`, `feature_governance_cl_connects_as_dashd_did` and
/// `feature_evo_shares_activation_state_hashes_are_dashds`): the checked node [`connect`]s every
/// block to the run's tip, and [`linear_clean`] from 0 to the tip fails on a height the node did
/// not log (a gap, in `report.mismatches`) and requires a pair at every height (`report.pairs ==
/// to - from + 1`, no short range); for the five runs with a CbTx ChainLock the node under
/// `-assumevalid` is compared the same way, and [`Recording::assumed_on`] asserts its tip.
#[test]
fn l2_census_pins_every_type_7_and_9_tx_of_the_recordings() {
    use dash_primitives::TxType;

    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/statedigest/regtest-v24");
    let mut all: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| root.join(name).join("node0/blocks.dat.zst").exists())
        .collect();
    all.sort();
    assert_eq!(all.len(), 18, "{all:?}: the stored node0 chains");
    let (mut sevens, mut nines) = (0, 0);
    let mut found = Vec::new();
    for name in &all {
        let run = Recording::open(name);
        let (mut type7, mut type9_v1, mut type9_v2) = (Vec::new(), Vec::new(), Vec::new());
        for (h, b) in (0_u32..).zip(&run.blocks) {
            for tx in b.txs() {
                if tx.tx_type() == TxType::new(7) {
                    type7.push(h);
                } else if tx.tx_type() == TxType::new(9) {
                    match tx.extra_payload().first() {
                        Some(1) => type9_v1.push(h),
                        Some(2) => type9_v2.push(h),
                        v => panic!("{name} {h}: an asset unlock of payload version {v:?}"),
                    }
                }
            }
        }
        if type7.is_empty() && type9_v1.is_empty() && type9_v2.is_empty() {
            continue;
        }
        found.push(name.as_str());
        let pin = CARRYING.iter().find(|c| c.test == name);
        let pin = pin.unwrap_or_else(|| panic!("{name} carries type 7 or 9, not in the census"));
        assert_eq!(type7, pin.type7, "{name}: type 7 heights");
        assert_eq!(type9_v1, pin.type9_v1, "{name}: type 9 version 1 heights");
        assert_eq!(type9_v2, pin.type9_v2, "{name}: type 9 version 2 heights");
        let tip = u32::try_from(run.blocks.len() - 1).unwrap();
        let (_, locks) = run.cbtx_chainlocks(tip);
        let at_type7: Vec<_> = locks.into_iter().filter(|(h, _)| type7.contains(h)).collect();
        assert_eq!(at_type7, pin.type7_locks, "{name}: the type 7 blocks' CbTx ChainLocks");
        sevens += type7.len();
        nines += type9_v1.len() + type9_v2.len();
    }
    let mut pinned: Vec<&str> = CARRYING.iter().map(|c| c.test).collect();
    pinned.sort_unstable();
    assert_eq!(found, pinned, "the recordings that carry a type 7 or 9 tx");
    assert_eq!((sevens, nines), (9, 19), "the census");
}

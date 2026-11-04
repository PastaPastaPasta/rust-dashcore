//! CH7b-2's recorded regtest chains: the O8 recorder's dashd v24 runs kept under
//! `test-vectors/statedigest/regtest-v24`, each node0's blocks connected through the node on the
//! run's settings. C14: a datadir synced past V20 under `-assumevalid` restarts. DEC-1028: the
//! EHF deployments follow dashd's after every block, across a reorg and a restart. C15 (§7.2 L1,
//! T-RD): runs with non-null commitments, rotation, MNHF signals and asset unlocks connect as
//! dashd connected them, and each mined quorum's roster is dashd's `quorum info`. CH7b-3 C7:
//! the runs with CbTx ChainLocks connect whole, step 10 verifying each.

use std::ops::Range;
use std::path::PathBuf;

use dash_consensus::ctx::DeploymentView as _;
use dash_consensus::deployments::ThresholdState;
use dash_params::DeploymentPos;
use dash_primitives::QuorumHash;
use dashd_chainstate::evo::cl::census_under;
use dashd_chainstate::evo::gates::counts_under;

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
        let argv: Vec<&str> =
            self.args.iter().map(String::as_str).chain(more.iter().copied()).collect();
        let sut = try_sut(dir, &argv, ui).expect("accepted");
        assert_eq!(self.magic, sut.params.chain.message_start, "the network's magic");
        let genesis = dashd_chainstate::chain::genesis_block(&sut.params.chain);
        assert_eq!(self.blocks[0].hash(), genesis.hash(), "genesis first");
        sut
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
        let mut sut = self.node(dir, &[&at_best], log);
        imported(&mut sut);
        send_headers(&sut, &headers);
        for b in self.after_genesis() {
            submit(&sut, b);
        }
        assert_eq!(view_tip(&sut), tip.hash(), "connected under -assumevalid");
        sut
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
            let payload = b.txs()[0].to_mutable().extra_payload;
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
                    index: q.commitment.quorum_index.unwrap_or(0),
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
/// the first with D2's text (ALLOWED-DIFFS D18).
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
    assert!(!logged.iter().any(|l| l.contains("ALLOWED-DIFFS D18")), "{logged:?}");
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

/// Step 10 on the node in `dir`, connected to `to`, against `run`'s CbTxs decoded: holding no
/// best ChainLock, the node asks for one at each CbTx of version 3 or later and gets Core's null
/// lock, takes neither shortcut, and verifies each non-null `bestCLSignature`, valid, in order.
/// Returns the verifies.
fn step_ten_checks(run: &Recording, dir: &Path, to: u32) -> usize {
    let (v3, locks) = run.cbtx_chainlocks(to);
    let roots = counts_under(dir).expect("the node's cells").root_checks();
    let nulls: Vec<_> = v3.iter().map(|&h| (h, None)).collect();
    assert_eq!(roots.cl_asks, nulls, "an ask at each v3 CbTx, Core's null lock each time");
    assert!(roots.cl_shortcuts.is_empty(), "{:?}", roots.cl_shortcuts);
    let census = census(dir);
    assert_eq!(census_mismatch(&census, &locks), None, "{census:?}");
    locks.len()
}

/// C15's replay of `test` (CH7 §7.2 L1, T-RD), whole since CH7b-3 C7. dashd connected each
/// recorded block in turn (the golden's `C` records). With `-assumevalid=0` the node connects
/// each in turn too, to the run's tip: every CbTx's MN-list and quorum roots checked (steps 8 and
/// 9 refuse a root that differs), its best ChainLock (step 10, [`step_ten_checks`]), each
/// commitment's members and signatures, and the MNHF signals' and asset unlocks' quorum
/// signatures through route C. Each commitment's roster, read through route B at the node's tip,
/// is dashd's `quorum info`. A run with a CbTx ChainLock connects on a node under
/// `-assumevalid` too ([`Recording::assumed`]), which skips step 10 with the script checks, and
/// each roster is dashd's there as well. Returns the commitments and step 10's verifies.
fn replay(test: &str) -> (usize, usize) {
    let run = Recording::open(test);
    let order = (0_u64..).zip(&run.blocks).map(|(h, b)| (h, b.hash().to_string()));
    assert_eq!(run.connected(), order.collect::<Vec<_>>(), "dashd connected each in turn");
    let quorums = run.quorum_infos();

    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    for h in 1..run.blocks.len() {
        connect(&sut, &run, h);
    }
    for q in &quorums {
        assert_eq!(q.at(&sut), *q);
    }
    let tip = u32::try_from(run.blocks.len() - 1).unwrap();
    let verifies = step_ten_checks(&run, dir.path(), tip);
    if verifies > 0 {
        let dir = tempfile::tempdir().unwrap();
        let sut = run.assumed(dir.path(), ui());
        for q in &quorums {
            assert_eq!(q.at(&sut), *q);
        }
    }
    (quorums.len(), verifies)
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

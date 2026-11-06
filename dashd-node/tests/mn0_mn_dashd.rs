//! MN0-3's E-mn differential (OD-CP-124 Q3, OD-CP-125 R4): on setup-dashd's masternode network
//! (406 recorded blocks, four masternodes, `llmq_test` and rotated `llmq_test_dip0024` quorums,
//! CbTx ChainLocks from 237), at every tip from DIP0003, dashd v24.0.0-rc.5's
//! `getblocktemplate` and the SUT's `create_new_block` give the same CbTx, masternode payments,
//! quorum commitments, version and bits. The recorded blocks go to both nodes with
//! `submitblock`; regtest's `getblocktemplate` answers without peers or a synced masternode
//! state (`rpc/mining.cpp:741-754`).
//!
//! The ChainLock state is a harness condition, asserted before each sample on both nodes, in one
//! of two modes:
//! - null-best: neither node knows a ChainLock (dashd unsynced takes none from coinbases,
//!   `chainlock/handler.cpp:104-130`), so each CbTx's ChainLock is the parent's carried forward,
//!   or null;
//! - fed: both take the ChainLock the next recorded CbTx carries before the sample.
//!
//! E-shares (OD-CP-124 Q4): the same comparison at every tip of the O8 recorder's
//! `feature_masternode_shares` node0 chain (2027 blocks, `evo_shares` activated by `-vbparams`),
//! whose evonodes are paid their share once it is active.
//!
//! Needs dashd v24.0.0-rc.5 (see `v24`).

mod v24;

use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::primitives::core_serialize;
use dashd_mining::{BlockTemplateSource, ChainTemplateSource, TemplateError, TemplateTxs};
use serde_json::{Value, json};

use v24::{Dashd, blocks, dashd_binary, hex};

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406).
const QC_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 3] =
    ["-dip3params=2:2", "-testactivationheight=v20@100", "-testactivationheight=mn_rr@100"];
/// The coinbase script both templates pay the miner's part to (excluded from the comparison).
const SCRIPT: [u8; 1] = [0x51];

/// What the comparison reads of a template: the CbTx payload, the payments after the miner's
/// output (script, amount), the other transactions, the version and the bits.
#[derive(Debug, PartialEq, Eq)]
struct Sample {
    cbtx: String,
    payments: Vec<(String, i64)>,
    txs: Vec<String>,
    version: i64,
    bits: String,
    /// `coinbasevalue`, the coinbase's `GetValueOut()` (`rpc/mining.cpp:937`).
    coinbase_value: i64,
}

fn ours(block: &Block) -> Sample {
    let coinbase = &block.txs()[0];
    Sample {
        cbtx: hex(coinbase.extra_payload()),
        payments: coinbase.outputs()[1..]
            .iter()
            .map(|o| (hex(o.script_pubkey.as_bytes()), o.value))
            .collect(),
        txs: block.txs()[1..].iter().map(|tx| hex(&core_serialize(&**tx))).collect(),
        version: i64::from(block.header().version()),
        bits: format!("{:08x}", block.header().bits()),
        coinbase_value: coinbase.outputs().iter().map(|o| o.value).sum(),
    }
}

fn dashds(gbt: &Value) -> Sample {
    let text = |v: &Value| v.as_str().expect("a string").to_owned();
    Sample {
        cbtx: text(&gbt["coinbase_payload"]),
        payments: gbt["masternode"]
            .as_array()
            .expect("masternode")
            .iter()
            .map(|p| (text(&p["script"]), p["amount"].as_i64().expect("amount")))
            .collect(),
        txs: gbt["transactions"]
            .as_array()
            .expect("transactions")
            .iter()
            .map(|t| text(&t["data"]))
            .collect(),
        version: gbt["version"].as_i64().expect("version"),
        bits: text(&gbt["bits"]),
        coinbase_value: gbt["coinbasevalue"].as_i64().expect("coinbasevalue"),
    }
}

/// Null-best mode: neither node knows a ChainLock.
#[test]
fn every_tip_templates_as_dashd_without_a_best_chainlock() {
    every_tip(false);
}

/// Fed mode: before each sample both nodes take the ChainLock the next recorded block's CbTx
/// carries (dashd `submitchainlock`, V `ClIntake` and `RecordChainLock`), so each template's
/// CbTx is `CalcCbTxBestChainlock` over a best ChainLock.
#[test]
fn every_tip_templates_as_dashd_fed_the_recorded_chainlocks() {
    every_tip(true);
}

/// The ChainLock `block`'s CbTx carries: its height, block hash and signature.
fn carried(chain: &[Arc<Block>], at: usize) -> Option<(i32, dash_primitives::BlockHash, [u8; 96])> {
    use dash_primitives::SpecialPayload;
    let block = chain.get(at)?;
    let p = block.txs()[0].payload()?.ok()?;
    let SpecialPayload::CoinbaseCommitment(cbtx) = &p.payload else {
        return None;
    };
    let sig = cbtx.best_cl_signature.as_ref()?.to_bytes();
    if sig == [0; 96] {
        return None;
    }
    // The block at index `at` is at height `at + 1`; its ChainLock is `diff + 1` below it.
    let height = at as i64 - i64::try_from(cbtx.best_cl_height_diff?).ok()?;
    let locked = usize::try_from(height - 1).ok()?;
    Some((height as i32, chain[locked].hash(), sig))
}

fn every_tip(fed: bool) {
    let Some(classes) = every_tip_of(&blocks(QC_CHAIN), &QC_ARGS, fed) else {
        return;
    };
    assert!(classes.superblocks.is_empty(), "{classes:?}");
    assert!(classes.chainlocks > 0 && classes.paid > 0 && classes.commitments > 0, "{classes:?}");
    // Zero classes on this network (MN0-3 notes 3-D9): every commitment has every member valid,
    // so none PoSe-punishes (evo_dashd's crafted case is that class), and no masternode has an
    // operator reward, so each payment is the owner's one output.
    assert_eq!(classes.pose, 0, "{classes:?}");
    assert!(classes.outputs.keys().all(|&n| n == 1), "{classes:?}");
}

/// The recorder's `feature_masternode_shares` node0 chain, null-best.
#[test]
fn every_shares_tip_templates_as_dashd() {
    let Some(classes) = every_tip_of(&shares_chain(), &SHARES_ARGS, false) else {
        return;
    };
    assert!(classes.paid > 0 && classes.platform_share > 0, "{classes:?}");
    // Regtest's superblocks: from 1500, every 20 (`chainparams.cpp:809-811`).
    let superblocks: Vec<usize> = (1500..=2028).step_by(20).collect();
    assert_eq!(classes.superblocks, superblocks, "the heights refused for GV1b");
    assert!(classes.outputs.keys().any(|&n| n > 2), "a split owner reward: {classes:?}");
}

/// E-pool (OD-CP-124, OD-CP-125): at each block of the masternode network that mines a non-null
/// commitment, both nodes invalidate it, so its undo returns the commitment to the mineable pool
/// (`llmq/blockprocessor.cpp`'s `UndoBlock` re-adding it, V's `ReinsertMineableCommitment`), and
/// the templates on its parent agree. Each mines the block's commitments, whose quorums root
/// and list after the block are the recorded block's. Both nodes then reconsider it.
#[test]
fn templates_mine_a_disconnected_blocks_commitments_as_dashd() {
    use dash_primitives::SpecialPayload;
    let chain = blocks(QC_CHAIN);
    let Some(nodes) = Nodes::start(&chain, &QC_ARGS) else {
        return;
    };
    let commitments = |block: &Block| -> Vec<String> {
        block.txs()[1..]
            .iter()
            .filter(|tx| {
                matches!(tx.payload(), Some(Ok(p))
                    if matches!(&p.payload, SpecialPayload::QuorumCommitment(qc) if !qc.commitment.is_null()))
            })
            .map(|tx| hex(&core_serialize(&**tx)))
            .collect()
    };
    let roots = |block: &Block| match block.txs()[0].payload() {
        Some(Ok(p)) => match &p.payload {
            SpecialPayload::CoinbaseCommitment(cbtx) => {
                (cbtx.merkle_root_mn_list, cbtx.merkle_root_quorums)
            }
            _ => panic!("a CbTx"),
        },
        _ => panic!("a CbTx"),
    };
    let mut met = 0;
    let mut mined_total = 0;
    for (i, block) in chain.iter().enumerate() {
        let height = i + 1;
        nodes.submit(height, block);
        let mined = commitments(block);
        // 3-D9's PoSe class: no recorded commitment leaves a member out.
        for tx in &block.txs()[1..] {
            if let Some(Ok(p)) = tx.payload()
                && let SpecialPayload::QuorumCommitment(qc) = &p.payload
                && !qc.commitment.is_null()
            {
                let bits = &qc.commitment.valid_members;
                assert!((0..).map_while(|i| bits.get(i)).all(|valid| valid), "PoSe at {height}");
                mined_total += 1;
            }
        }
        if !mined.is_empty() {
            let hash = block.hash();
            nodes.dashd.call("invalidateblock", json!([hash.to_string()]));
            nodes.v.invalidate_block(hash).recv().expect("V").expect("in the index");
            let (gbt, template) = nodes.templates(height);
            assert_eq!(dashds(&gbt), ours(&template), "the templates at {height}");
            assert_eq!(commitments(&template), mined, "the commitments mined at {height}");
            assert_eq!(roots(&template), roots(block), "the roots at {height}");
            nodes.dashd.call("reconsiderblock", json!([hash.to_string()]));
            nodes.v.reconsider_block(hash, false).recv().expect("V").expect("in the index");
            met += 1;
        }
    }
    eprintln!("{met} blocks with {mined_total} non-null commitments, none leaving a member out");
    assert!(met > 0);
}

/// E-rot (ii) (a) (OD-CP-134 m1): each rotated cycle's snapshot row V stored as it connected the
/// recorded chain (and the template validity passes it ran never changed) is dashd's, as
/// `quorum rotationinfo` answers it at each tip. Each snapshot comes beside its cycle's work-block
/// diff, the block `WORK_DIFF_DEPTH` (8) below the cycle base it is keyed by.
// Core: v24.0.0-rc.5 rpc/quorums.cpp:960-995, llmq/snapshot.cpp:35-65, llmq/core_write.cpp:42-100
#[test]
fn every_rotated_snapshot_is_dashds() {
    const WORK_DIFF_DEPTH: usize = 8;
    let chain = blocks(QC_CHAIN);
    let Some(nodes) = Nodes::start(&chain, &QC_ARGS) else {
        return;
    };
    let height_of: std::collections::BTreeMap<String, usize> =
        chain.iter().zip(1..).map(|(b, h)| (b.hash().to_string(), h)).collect();
    let mut theirs = std::collections::BTreeMap::new();
    for (i, block) in chain.iter().enumerate() {
        let height = i + 1;
        nodes.submit(height, block);
        // The template's validity pass stores a cycle base's row keyed by the template.
        let _ = nodes.try_templates(height + 1);
        let Some(info) =
            nodes.dashd.try_call("quorum", json!(["rotationinfo", block.hash().to_string(), true]))
        else {
            continue;
        };
        for k in ["C", "2C", "3C", "4C"] {
            let (diff, snapshot) = (
                &info[format!("mnListDiffAtHMinus{k}")],
                &info[format!("quorumSnapshotAtHMinus{k}")],
            );
            let Some(work) = diff["blockHash"].as_str() else {
                continue;
            };
            let base = height_of[work] + WORK_DIFF_DEPTH;
            let old = theirs.insert(base, snapshot.clone());
            assert!(old.is_none_or(|old| old == *snapshot), "dashd's snapshot at {base} changed");
        }
    }
    assert!(theirs.len() >= 4, "rotated cycles: {:?}", theirs.keys());
    for (&base, theirs) in &theirs {
        let key = dashd_chainstate::evo::codec::quorum_snapshot_key(
            dash_params::LlmqType::LlmqTestDip0024,
            &chain[base - 1].hash(),
        );
        let row = nodes
            .sut
            .stored_row("llmq_snapshot", &key)
            .unwrap_or_else(|| panic!("V's row at {base}"));
        let ours = dashd_chainstate::evo::codec::decode_quorum_snapshot(&row).expect("decodes");
        let ours = json!({
            "activeQuorumMembers": ours.active_quorum_members,
            "mnSkipListMode": ours.skip_list_mode,
            "mnSkipList": ours.skip_list,
        });
        assert_eq!(&ours, theirs, "the snapshot at {base}");
    }
    eprintln!("rotated snapshots compared at {:?}", theirs.keys());
}

/// `feature_masternode_shares`'s chain arguments (its `extract.json`).
const SHARES_ARGS: [&str; 5] = [
    "-vbparams=v24:0:999999999999:250:10:8:6:5:0",
    "-vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
];

/// The recorder's node0 `blocks.dat` of `feature_masternode_shares`, genesis dropped.
fn shares_chain() -> Vec<Arc<Block>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-vectors/statedigest/regtest-v24/feature_masternode_shares/node0/blocks.dat.zst"
    );
    let raw = zstd::decode_all(std::fs::File::open(path).expect("the recording")).expect("zstd");
    let (mut rest, mut chain) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().expect("a size"));
        let (mut bytes, tail) = tail.split_at(usize::try_from(size).expect("a size"));
        chain.push(Arc::new(
            dash_consensus::primitives::decode_block(&mut bytes).expect("a block").into_value(),
        ));
        rest = tail;
    }
    chain.split_off(1)
}

/// dashd and the SUT side by side on `args`, their clocks after `chain`'s last block.
struct Nodes {
    dashd: Dashd,
    sut: dashd_node::sut::NodeSut,
    v: dashd_chainstate::chain::ChainHandle,
    source: ChainTemplateSource,
    _dir: tempfile::TempDir,
}

impl Nodes {
    /// `None` when dashd tests are skipped.
    fn start(chain: &[Arc<Block>], args: &[&str]) -> Option<Nodes> {
        let binary = dashd_binary()?;
        let now = chain.iter().map(|b| i64::from(b.header().time())).max().expect("blocks") + 60;
        let dir = tempfile::tempdir().unwrap();
        let dashd = Dashd::start_at(&binary, &dir.path().join("dashd"), now, args);
        let sut_dir = dir.path().join("rdashd");
        std::fs::create_dir_all(&sut_dir).unwrap();
        let ui = Arc::new(dashd_node::ui::Ui::with_stderr(
            Arc::new(dashd_node::logging::Logger::new()),
            Box::new(|_| {}),
        ));
        let argv: Vec<&str> =
            ["-regtest", "-printtoconsole=0"].into_iter().chain(args.iter().copied()).collect();
        let sut = dashd_node::sut::NodeSut::new(&sut_dir, &argv, now, ui).expect("the SUT starts");
        let v = sut.chain().expect("V runs").clone();
        let source = ChainTemplateSource::new(
            v.clone(),
            Arc::new(sut.params().clone()),
            sut.clock().clone() as _,
            None,
        );
        Some(Nodes {
            dashd,
            sut,
            v,
            source,
            _dir: dir,
        })
    }

    /// `block`, at `height`, to both.
    fn submit(&self, height: usize, block: &Arc<Block>) {
        let answer = self.dashd.call("submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "dashd at {height}");
        let r = self.v.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {height}: {r:?}");
    }

    /// dashd's template and the SUT's, at `height`.
    fn templates(&self, height: usize) -> (Value, Block) {
        let (gbt, template) = self.try_templates(height);
        (gbt, template.unwrap_or_else(|e| panic!("the SUT's template at {height}: {e:?}")))
    }

    /// [`Self::templates`], the SUT's refusal kept.
    fn try_templates(&self, height: usize) -> (Value, Result<Block, TemplateError>) {
        let gbt = self.dashd.call("getblocktemplate", json!([]));
        assert_eq!(gbt["height"], json!(height));
        let template = self.source.create_new_block(&SCRIPT, TemplateTxs::None);
        (gbt, template.map(|t| t.block))
    }
}

/// Every tip of `chain` (heights 1 up) compared, both nodes on `args`; the classes met.
fn every_tip_of(chain: &[Arc<Block>], args: &[&str], fed: bool) -> Option<Classes> {
    let mut nodes = Nodes::start(chain, args)?;
    let mut compared = 0;
    let mut fed_best = 0;
    let mut classes = Classes::default();
    let mut differ = Vec::new();
    for (i, block) in chain.iter().enumerate() {
        let height = i + 1;
        nodes.submit(height, block);
        if fed && let Some((h, hash, sig)) = carried(chain, height) {
            nodes.dashd.call("submitchainlock", json!([hash.to_string(), hex(&sig), h]));
            nodes.sut.cl_record(h, hash, sig).expect("V takes it");
        }
        // R4: both nodes at the same best ChainLock (none when not fed).
        let core_best = nodes
            .dashd
            .try_call("getbestchainlock", json!([]))
            .map_or(-1, |cl| cl["height"].as_i64().expect("height"));
        let best = nodes.v.query(|q| q.cl.best_height()).recv().expect("V serves");
        assert_eq!(i64::from(best), core_best, "the best ChainLock at {height}");
        if !fed {
            assert_eq!(best, -1, "a best ChainLock at {height}");
        }
        fed_best += usize::from(best >= 0);

        let (gbt, template) = nodes.try_templates(height + 1);
        let template = match template {
            Ok(template) => template,
            // Until GV1b (OD-CP-134 M1): refused at every superblock height of a chain with
            // masternodes, where this dashd, without triggers, pays none.
            Err(TemplateError::NotReady {
                rule: "superblock",
                ..
            }) => {
                assert_eq!(gbt["superblock"], json!([]), "dashd's superblock at {}", height + 1);
                classes.superblocks.push(height + 1);
                continue;
            }
            Err(e) => panic!("the SUT's template at {}: {e:?}", height + 1),
        };
        let (core, sut) = (dashds(&gbt), ours(&template));
        classes.count(&template);
        if core != sut {
            differ.push((height + 1, core, sut));
        }
        compared += 1;
    }
    for (h, core, sut) in differ.iter().take(5) {
        eprintln!("h{h}\n dashd {core:?}\n ours  {sut:?}");
    }
    assert!(differ.is_empty(), "{} of {compared} templates differ", differ.len());
    eprintln!("fed {fed}: compared {compared}, {fed_best} with a best ChainLock: {classes:?}");
    assert_eq!(fed_best > 0, fed, "tips with a best ChainLock");
    Some(classes)
}

/// The template classes the comparison met (OD-CP-124 Q3's counts).
#[derive(Debug, Default)]
struct Classes {
    /// CbTxs with a non-null ChainLock.
    chainlocks: usize,
    /// Templates paying a masternode (not only the credit pool's `OP_RETURN` share).
    paid: usize,
    /// Templates paying the credit pool's share.
    platform_share: usize,
    /// Templates with any commitment, and with a non-null one.
    commitments: usize,
    real_commitments: usize,
    /// Paying templates by their owner and operator outputs (the credit pool's `OP_RETURN`
    /// share left out): more than two is a split owner reward (`masternode/payments.cpp:171-191`).
    outputs: std::collections::BTreeMap<usize, usize>,
    /// The heights refused as a superblock's (GV1b).
    superblocks: Vec<usize>,
    /// Non-null commitments that leave a member out (`validMembers`), which PoSe-punish it.
    pose: usize,
}

impl Classes {
    fn count(&mut self, block: &Block) {
        use dash_primitives::SpecialPayload;
        let coinbase = &block.txs()[0];
        if let Some(Ok(p)) = coinbase.payload()
            && let SpecialPayload::CoinbaseCommitment(cbtx) = &p.payload
            && cbtx.best_cl_signature.as_ref().is_some_and(|s| s.to_bytes() != [0; 96])
        {
            self.chainlocks += 1;
        }
        let payments = &coinbase.outputs()[1..];
        let to_payees =
            payments.iter().filter(|o| o.script_pubkey.as_bytes().first() != Some(&0x6a)).count();
        self.platform_share += usize::from(to_payees < payments.len());
        if to_payees > 0 {
            self.paid += 1;
            *self.outputs.entry(to_payees).or_default() += 1;
        }
        let mut any = false;
        for tx in &block.txs()[1..] {
            if let Some(Ok(p)) = tx.payload()
                && let SpecialPayload::QuorumCommitment(qc) = &p.payload
            {
                any = true;
                self.real_commitments += usize::from(!qc.commitment.is_null());
                let bits = &qc.commitment.valid_members;
                self.pose += usize::from(
                    !qc.commitment.is_null()
                        && (0..).map_while(|i| bits.get(i)).any(|valid| !valid),
                );
            }
        }
        self.commitments += usize::from(any);
    }
}

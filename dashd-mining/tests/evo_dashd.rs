//! MN0-2a's offline differential: [`dashd_mining::evo::block`] against the blocks dashd v24.0.0-rc.4
//! mined with `generateblock <address> []` on no-masternode regtest chains (the default
//! parameters, moved DIP0003/DIP0008/V20/MN_RR heights, and V24 by `-vbparams`), from the first
//! DIP0003 block to the tip. Each block is assembled over the chain of dashd's headers below it,
//! an empty masternode list, an empty mineable pool, no mined commitments and the credit pool our
//! previous block left; its transactions, behind dashd's header, must be dashd's raw block.
//!
//! `tests/data/mn0-evo-regtest.json` is `contrib/node/mn0-captures/capture_mn0_evo.py`'s output
//! (README there).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::ctx::{BlockId, ChainAncestry, Deployment, DeploymentView as _, HeaderCtx};
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::payments::PaymentParams;
use dash_consensus::primitives::{core_serialize, decode_block, decode_header, write_compact_size};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::blockprocessor::{BlockLookup, QcCtx, mined_record};
use dash_dml::creditpool::CreditPool;
use dash_dml::store::{EvoDelta, MemEvoStore, StoreError};
use dash_dml::testkit::{dmn, replay};
use dash_dml::{Dmn, MnList, MnType};
use dash_evo::bls::BlsCtx;
use dash_evo::effect::EffectSink;
use dash_evo::llmq::{
    CompleteChain, LlmqCtx, MemberError, PrecomputeMode, QuorumMemberSource, QuorumMembers,
    QuorumPrecompute, SerialBlsChecks,
};
use dash_num::{Arith256, CompactTarget};
use dash_params::LlmqType;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, Release,
};
use dash_primitives::{BlockHash, SpecialPayload};
use dash_types::Numeric as _;
use dashd_mining::evo::{self, EvoChain};
use dashd_mining::{TemplateError, TipFacts};
use serde_json::Value;

const FIXTURE: &str = include_str!("data/mn0-evo-regtest.json");

#[test]
fn every_block_from_dip3_is_dashds() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture JSON");
    let chains = fixture["chains"].as_array().expect("chains");
    assert_eq!(chains.len(), 7);
    // Per chain: its CbTx versions with their block counts, and the quorum commitments mined.
    let seen: Vec<_> = chains.iter().map(replay).collect();
    assert_eq!(
        seen,
        [
            // 432..=500, null commitments in the mining windows 442..=452, 466..=476, 490..=500.
            ("default".to_owned(), BTreeMap::from([(3, 69)]), 135),
            // DIP0003 alone 2..=49, DIP0008 50..=149, V20 150..=199, MN_RR 200..=270; eleven
            // windows from 10..=20 to 250..=260.
            ("moved".to_owned(), BTreeMap::from([(1, 48), (2, 100), (3, 121)]), 387),
            // V24 active from 300, so the first CbTx is version 4; windows 442..=452, 466..=476.
            ("v24".to_owned(), BTreeMap::from([(4, 49)]), 90),
            // MN_RR at 432 below BRR (1000): the platform reward steps at 440 (MN0-2a r1 m1).
            ("brr-late".to_owned(), BTreeMap::from([(3, 29)]), 45),
            // MN_RR at 10 below the reallocation start (20): 20 % of the block value to 19.
            ("mnrr-early".to_owned(), BTreeMap::from([(2, 8), (3, 51)]), 71),
            // V19 at 100: legacy-scheme null commitments (versions 1 and 2) below it.
            ("legacy-bls".to_owned(), BTreeMap::from([(2, 148), (3, 51)]), 279),
            // V24 from 300 without V20 keeps CbTx v2; v4 from V20 at 350.
            ("v24-before-v20".to_owned(), BTreeMap::from([(2, 348), (4, 21)]), 533),
        ]
    );
}

fn replay(chain: &Value) -> (String, BTreeMap<u16, u32>, u64) {
    let name = chain["name"].as_str().expect("name");
    let args: Vec<&str> =
        chain["args"].as_array().expect("args").iter().map(|a| a.as_str().expect("arg")).collect();
    let params = regtest(&args);
    let headers = Headers::new(chain["headers"].as_array().expect("headers"));
    let dip3 = chain["dip3_height"].as_u64().expect("dip3_height") as u32;
    let script = unhex(chain["script_pubkey"].as_str().expect("script"));
    let blocks: Vec<Vec<u8>> = chain["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .map(|b| unhex(b.as_str().expect("hex")))
        .collect();
    assert_eq!(
        blocks.len() as u32,
        headers.len() - dip3,
        "{name}: one block per height from DIP0003"
    );

    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &headers, &NoEhfSignals, &cache);
    let complete = CompleteChain::<StoreError>::new(&headers);
    let llmq = LlmqCtx::new(&params, &complete, &deployments, BlsCtx::BASIC).expect("in domain");
    let store = MemEvoStore::new();
    let mined = store.view(&headers);
    let qc = QcCtx {
        llmq,
        index: &headers,
        mined: &mined,
        members: &NoMembers,
        precompute: &NoMembers,
        bls_checks: &SerialBlsChecks,
    };
    let pool = MineablePool::new();
    let list = MnList::scratch();
    let payments = PaymentParams::from_chain_params(&params).expect("payment parameters");
    let subsidy = SubsidyParams::from_chain_params(
        &params,
        genesis_reward(&dash_consensus::params::genesis_block(&params)).expect("genesis"),
    )
    .expect("subsidy parameters");

    let mut credit_pool = CreditPool::default();
    let mut versions = BTreeMap::new();
    let mut commitments = 0;
    for (raw, height) in blocks.iter().zip(dip3..) {
        let prev = BlockId(height - 1);
        let core = decode_block(&mut &raw[..]).expect("dashd's block").into_value();
        let facts = facts_at(height, &headers, &deployments, &subsidy);
        assert_eq!(
            deployments.compute_block_version(Some(prev)),
            facts.version,
            "{name} h{height}: ComputeBlockVersion"
        );
        let chain = EvoChain {
            deployments: &deployments,
            qc: &qc,
            pool: &pool,
            list: &list,
            chainlock: false,
            superblock_triggered: false,
            credit_pool: &credit_pool,
            payments: &payments,
        };
        let ours = evo::block(&facts, prev, &script, &chain).expect("assembled");
        assert_eq!(ours.stats.num_txs, core.txs().len() as u64 - 1, "{name} h{height}: nBlockTx");
        commitments += ours.stats.num_txs;
        // `nBlockSize`: the coinbase's reserved 1000 plus each commitment's serialized size.
        let size: usize = core.txs()[1..].iter().map(|tx| core_serialize(&**tx).len()).sum();
        assert_eq!(ours.stats.size, 1000 + size as u64, "{name} h{height}: nBlockSize");

        let mut bytes = core_serialize(core.header());
        write_compact_size(ours.block.txs().len() as u64, &mut bytes);
        for tx in ours.block.txs() {
            bytes.extend(core_serialize(&**tx));
        }
        assert!(
            bytes == *raw,
            "{name} h{height}: ours\n{}\ndashd's\n{}",
            replay::hex(&bytes),
            replay::hex(raw)
        );

        let cbtx = evo::Eras::after(&deployments, prev);
        *versions.entry(cbtx.cbtx_version()).or_insert(0u32) += 1;
        if cbtx.v20 {
            credit_pool = CreditPool {
                locked: credit_balance(&ours.block),
                ..CreditPool::default()
            };
        }
    }

    // The fences: a registered masternode, a ChainLock to carry forward (from V20) and a
    // superblock are other slices'; below DIP0003 is `assemble::block`'s.
    let mut registered = MnList::scratch();
    registered.insert_for_tests(dmn(1, MnType::Regular));
    let empty_pool = CreditPool::default();
    let assemble = |height: u32, list: &MnList, chainlock, superblock_triggered| {
        let chain = EvoChain {
            deployments: &deployments,
            qc: &qc,
            pool: &pool,
            list,
            chainlock,
            superblock_triggered,
            credit_pool: &empty_pool,
            payments: &payments,
        };
        let facts = facts_at(height, &headers, &deployments, &subsidy);
        evo::block(&facts, BlockId(height - 1), &script, &chain).err()
    };
    let not_ready = |height, rule, owner| {
        Some(TemplateError::NotReady {
            height,
            rule,
            owner,
        })
    };
    let tip = headers.len() - 1;
    assert_eq!(assemble(dip3, &registered, false, false), not_ready(dip3, "DIP0003", "MN0-3"));
    assert_eq!(assemble(tip, &list, true, false), not_ready(tip, "V20", "MN0-3"), "{name}");
    assert_eq!(assemble(dip3, &list, false, true), not_ready(dip3, "superblock", "GV1b"));
    // Before V20 the CbTx has no ChainLock to carry.
    let v20 = deployments
        .active_after(Some(BlockId(dip3 - 1)), Deployment::Buried(BuriedDeployment::V20));
    assert_eq!(assemble(dip3, &list, true, false).is_none(), !v20, "{name}");
    assert!(
        matches!(assemble(dip3 - 1, &list, false, false), Some(TemplateError::Failed(_))),
        "{name}"
    );

    // A real commitment, from a quorum signed before the list emptied, is MN0-2b's to compare:
    // dashd's first null commitment with one signer.
    let (window, first) = blocks
        .iter()
        .zip(dip3..)
        .find_map(|(raw, height)| {
            let block = decode_block(&mut &raw[..]).expect("dashd's block").into_value();
            let tx = block.txs().get(1)?.clone();
            match &tx.payload()?.ok()?.payload {
                SpecialPayload::QuorumCommitment(p) => Some((height, p.commitment.clone())),
                _ => None,
            }
        })
        .expect("a mining window");
    let mut real = first;
    real.signers.data[0] |= 1;
    assert!(!real.is_null());
    let fence = |height, qc: &QcCtx<'_>, pool: &MineablePool| {
        let chain = EvoChain {
            deployments: &deployments,
            qc,
            pool,
            list: &list,
            chainlock: false,
            superblock_triggered: false,
            credit_pool: &empty_pool,
            payments: &payments,
        };
        let facts = facts_at(height, &headers, &deployments, &subsidy);
        evo::block(&facts, BlockId(height - 1), &script, &chain).err()
    };
    // In the pool: GetMineableCommitmentsTx takes it into the block.
    let mut holding = MineablePool::new();
    holding.add(&real);
    assert_eq!(
        fence(window, &qc, &holding),
        not_ready(window, "mineable commitments", "MN0-2b"),
        "{name}"
    );
    // Mined at the window: it stays active, so the next block's quorum root is not zero.
    let params_of = params.consensus.llmq(real.llmq_type).expect("configured");
    let (base, base_height) =
        headers.lookup(&BlockHash::from(*real.quorum_hash.as_bytes())).expect("a known base");
    let record = mined_record(&qc.llmq, params_of, &real, base, base_height).expect("a record");
    let mut history = MemEvoStore::new();
    history
        .apply(&EvoDelta {
            block: headers.header(BlockId(window)).hash,
            height: window,
            dml: None,
            mined_commitments: vec![record],
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: None,
        })
        .expect("a consistent delta");
    let history_view = history.view(&headers);
    let mined_qc = QcCtx {
        mined: &history_view,
        ..qc
    };
    assert_eq!(
        fence(window + 1, &mined_qc, &pool),
        not_ready(window + 1, "mined commitments", "MN0-2b"),
        "{name}"
    );
    (name.to_owned(), versions, commitments)
}

/// The facts of the block at `height` on `headers`, dashd's header fields with our subsidy.
fn facts_at(
    height: u32,
    headers: &Headers,
    deployments: &Deployments<'_>,
    subsidy: &SubsidyParams,
) -> TipFacts {
    let prev = BlockId(height - 1);
    let tip = headers.header(prev);
    let header = headers.header(BlockId(height));
    let v20 = deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::V20));
    TipFacts {
        prev: tip.hash,
        height,
        version: header.version,
        time: header.time,
        bits: header.bits.to_base(),
        subsidy: block_subsidy_inner(tip.bits.to_base(), tip.height as i32, v20, subsidy)
            .expect("subsidy"),
    }
}

/// v24 regtest with Core's arguments.
fn regtest(args: &[&str]) -> ChainSetup {
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args {
        let (name, value) = arg.split_once('=').expect("name=value");
        builder = builder.arg(ChainOption::from_name(name).expect("a chain option"), value);
    }
    builder.build().expect("Core's arguments build")
}

/// The CbTx's `creditPoolBalance`.
fn credit_balance(block: &dash_consensus::Block) -> i64 {
    use dash_primitives::SpecialPayload;
    let coinbase = &block.txs()[0];
    match &coinbase.payload().expect("a payload").expect("decodes").payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.credit_pool_balance.expect("V20's CbTx"),
        other => panic!("{other:?}"),
    }
}

/// dashd's headers with Core's median time past; `BlockId(h)` is height `h`.
struct Headers(Vec<HeaderCtx>);

impl Headers {
    fn new(raw: &[Value]) -> Headers {
        let mut out: Vec<HeaderCtx> = Vec::with_capacity(raw.len());
        for (height, hex) in raw.iter().enumerate() {
            let bytes = unhex(hex.as_str().expect("hex"));
            let header = decode_header(&mut &bytes[..]).expect("header");
            let header = header.value();
            if let Some(last) = out.last() {
                assert_eq!(header.raw().prev_hash, last.hash, "h{height} links");
            }
            let time = header.raw().time;
            // `GetMedianTimePast`: the median of the block's and up to ten ancestors' times.
            let mut times: Vec<i64> = out.iter().rev().take(10).map(|h| h.time.into()).collect();
            times.push(time.into());
            times.sort_unstable();
            out.push(HeaderCtx {
                height: height as u32,
                hash: header.hash(),
                time,
                bits: CompactTarget::new(header.raw().bits),
                version: header.version(),
                mtp: times[times.len() / 2],
                chain_work: Arith256::default(),
            });
        }
        Headers(out)
    }

    fn len(&self) -> u32 {
        self.0.len() as u32
    }
}

impl ChainAncestry for Headers {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= self.0[id.0 as usize].height).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

impl BlockLookup for Headers {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        self.0.iter().find(|h| h.hash == *hash).map(|h| (BlockId(h.height), h.height))
    }
}

/// No masternode is ever registered: nothing computes members.
struct NoMembers;

impl QuorumMemberSource for NoMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        unreachable!("members of {llmq_type:?} at {base:?} on a chain without masternodes")
    }
}

impl QuorumPrecompute for NoMembers {
    type Error = StoreError;

    fn precompute(
        &self,
        block: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        unreachable!("precompute at {block:?}: assembling computes no members")
    }
}

fn unhex(s: &str) -> Vec<u8> {
    replay::unhex(s).expect("hex")
}

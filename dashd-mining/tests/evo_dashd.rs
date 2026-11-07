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
use dash_dml::testkit::{dmn, dmn_with, replay};
use dash_dml::{DML_PREV_LIST_MISMATCH, DmlParams, Dmn, MnList, MnType, NoCoins};
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
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{BlockHash, MerkleRoot, SpecialPayload};
use dash_types::Numeric as _;
use dashd_chainstate::evo::node::node_net_info;
use dashd_mining::evo::{self, BlockError, EvoChain};
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
    let dml = DmlParams::new(&params);
    let v19_height = params.consensus.deployment_height(BuriedDeployment::V19).to_u32() as i32;
    let no_parent_cl = || Ok(None);
    let net_info = node_net_info(&params, false).expect("regtest's NetInfo rules");
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
            deployments.compute_block_version(Some(prev)).ok(),
            Some(facts.version),
            "{name} h{height}: ComputeBlockVersion"
        );
        let chain = EvoChain {
            deployments: &deployments,
            qc: &qc,
            pool: &pool,
            list: &list,
            coins: &NoCoins,
            ancestry: &headers,
            dml: &dml,
            net_info: &net_info,
            best_chainlock: None,
            parent_chainlock: &no_parent_cl,
            v19_height,
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

        let cbtx = evo::Eras::after(&deployments, prev).expect("the run's rows read");
        *versions.entry(cbtx.cbtx_version()).or_insert(0u32) += 1;
        if cbtx.v20 {
            credit_pool = CreditPool {
                locked: credit_balance(&ours.block),
                ..CreditPool::default()
            };
        }
    }

    // The fence: a superblock is GV1b's; below DIP0003 is `assemble::block`'s. A registered
    // masternode and a ChainLock to carry (MN0-3) are assembled.
    let parent = headers.header(BlockId(dip3 - 1)).hash;
    let mut registered = MnList::scratch();
    let first = dmn(1, MnType::Regular);
    registered.insert_for_tests(Arc::clone(&first));
    let registered = registered.with_block_hash(parent);
    // A list whose masternodes are all gone.
    let mut emptied = registered.clone();
    emptied.remove(&first.pro_tx_hash).expect("registered");
    assert_eq!(emptied.counts().total(), 0);
    assert_ne!(emptied.total_registered(), 0);
    let empty_pool = CreditPool::default();
    let assemble = |height: u32,
                    list: &MnList,
                    best_chainlock: Option<(i32, BlsGlobalSig)>,
                    superblock_triggered| {
        let chain = EvoChain {
            deployments: &deployments,
            qc: &qc,
            pool: &pool,
            list,
            coins: &NoCoins,
            ancestry: &headers,
            dml: &dml,
            net_info: &net_info,
            best_chainlock,
            parent_chainlock: &no_parent_cl,
            v19_height,
            superblock_triggered,
            credit_pool: &empty_pool,
            payments: &payments,
        };
        let facts = facts_at(height, &headers, &deployments, &subsidy);
        evo::block(&facts, BlockId(height - 1), &script, &chain).err()
    };
    let not_ready = |height, rule, owner| {
        Some(BlockError::Refused(TemplateError::NotReady {
            height,
            rule,
            owner,
        }))
    };
    let tip = headers.len() - 1;
    let best = Some((tip as i32 - 1, null_sig()));
    assert_eq!(assemble(dip3, &registered, None, false), None, "{name}");
    assert_eq!(assemble(dip3, &emptied, None, false), None, "{name}");
    assert_eq!(assemble(tip, &list, best.clone(), false), None, "{name}");
    assert_eq!(assemble(dip3, &list, None, true), not_ready(dip3, "superblock", "GV1b"));
    // Until GV1b, a superblock height of a chain that has had masternodes may trigger one; a
    // height off the cycle, or a chain that never had one, does not.
    let (start, cycle) =
        (payments.config().superblocks.start(), payments.config().superblocks.cycle());
    let first_superblock = u32::try_from((start + cycle - 1) / cycle * cycle).expect("a height");
    let may = |list: &MnList, height| evo::superblock_may_trigger(&payments, list, height);
    assert!(may(&registered, first_superblock) && may(&emptied, first_superblock));
    assert!(!may(&registered, first_superblock + 1) && !may(&registered, first_superblock - 1));
    assert!(!may(&list, first_superblock), "{name}: a chain without masternodes");
    assert_eq!(
        assemble(dip3, &registered, None, may(&registered, first_superblock)),
        not_ready(dip3, "superblock", "GV1b")
    );
    assert_eq!(assemble(dip3, &registered, None, may(&registered, first_superblock + 1)), None);
    assert!(
        matches!(
            assemble(dip3 - 1, &list, None, false),
            Some(BlockError::Refused(TemplateError::Failed(_)))
        ),
        "{name}"
    );
    // The builder's precondition, a list at the tip, is a refusal and no read's fault.
    let elsewhere = registered.clone().with_block_hash(headers.header(BlockId(dip3 - 2)).hash);
    let refused = assemble(dip3, &elsewhere, None, false);
    assert!(
        matches!(&refused, Some(BlockError::Refused(TemplateError::Failed(e))) if e.contains(DML_PREV_LIST_MISMATCH)),
        "{name}: {refused:?}"
    );

    // A real commitment, from a quorum signed before the list emptied: dashd's first null
    // commitment with one signer. Its quorum has no members on a list without masternodes.
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
    let no_members = QcCtx {
        members: &EmptyMembers,
        ..qc
    };
    let assemble_with = |height, qc: &QcCtx<'_>, pool: &MineablePool| {
        let chain = EvoChain {
            deployments: &deployments,
            qc,
            pool,
            list: &list,
            coins: &NoCoins,
            ancestry: &headers,
            dml: &dml,
            net_info: &net_info,
            best_chainlock: None,
            parent_chainlock: &no_parent_cl,
            v19_height,
            superblock_triggered: false,
            credit_pool: &empty_pool,
            payments: &payments,
        };
        let facts = facts_at(height, &headers, &deployments, &subsidy);
        evo::block(&facts, BlockId(height - 1), &script, &chain)
    };
    // In the pool: GetMineableCommitmentsTx takes it into the block, and the quorums' root holds
    // it.
    let mut holding = MineablePool::new();
    holding.add(&real);
    let block = assemble_with(window, &no_members, &holding).expect("assembled").block;
    let mined: Vec<_> = block.txs()[1..]
        .iter()
        .filter_map(|tx| match &tx.payload()?.ok()?.payload {
            SpecialPayload::QuorumCommitment(p) => Some(p.commitment.clone()),
            _ => None,
        })
        .collect();
    assert!(mined.contains(&real), "{name}: the pool's commitment in the block");
    let dip8 = |h: u32| evo::Eras::after(&deployments, BlockId(h - 1)).expect("reads").dip8;
    // Below DIP0008 the CbTx has no quorums' root.
    let zero = Some(MerkleRoot::default());
    assert_eq!(quorums_root(&block).is_some(), dip8(window), "{name}");
    assert_ne!(quorums_root(&block), zero, "{name}");
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
        ..no_members
    };
    let next = assemble_with(window + 1, &mined_qc, &pool).expect("assembled").block;
    assert_eq!(quorums_root(&next).is_some(), dip8(window + 1), "{name}");
    assert_ne!(quorums_root(&next), zero, "{name}");

    // OD-CP-134 m3 (b): a pool commitment whose `validMembers` clears a listed member's bit
    // PoSe-punishes it in the template's list (HandleQuorumCommitment). Its 40 points, less the
    // build's `DecreaseScores`, plus 66% of 100 reach the maximum: the template's list root is
    // the root built over a tip list holding the member banned, and not the unpunished one.
    assert!(!real.valid_members.get(0).expect("a member bit"), "member 0 left out");
    let mn = dmn_with(2, MnType::Regular, |d, _| d.hot.pose_penalty = 40);
    let on_tip = |dmn: Arc<Dmn>| {
        let mut list = MnList::scratch();
        list.insert_for_tests(dmn);
        list.with_block_hash(headers.header(BlockId(window - 1)).hash)
    };
    let banned = on_tip(dmn_with(2, MnType::Regular, |d, _| {
        d.hot.pose_penalty = 100;
        d.hot.ban_if_not_banned(i32::try_from(window).expect("a height") - 1);
    }));
    let punishing = QcCtx {
        members: &ListMembers(vec![Arc::clone(&mn)]),
        ..qc
    };
    let list_root = |qc: &QcCtx<'_>, list: &MnList| {
        let chain = EvoChain {
            deployments: &deployments,
            qc,
            pool: &holding,
            list,
            coins: &NoCoins,
            ancestry: &headers,
            dml: &dml,
            net_info: &net_info,
            best_chainlock: None,
            parent_chainlock: &no_parent_cl,
            v19_height,
            superblock_triggered: false,
            credit_pool: &empty_pool,
            payments: &payments,
        };
        let facts = facts_at(window, &headers, &deployments, &subsidy);
        mn_root(&evo::block(&facts, BlockId(window - 1), &script, &chain).expect("assembled").block)
    };
    let punished = list_root(&punishing, &on_tip(Arc::clone(&mn)));
    assert_eq!(punished, list_root(&no_members, &banned), "{name}: the member banned");
    assert_ne!(punished, list_root(&no_members, &on_tip(mn)), "{name}: unpunished");

    // OD-CP-134 M2: the list build's store errors as connect's `store_failure` splits them. A
    // member read that fails with no capability is V's fault; one whose capability is not built
    // is the readiness refusal.
    let fault = StoreError::inconsistent("a member read");
    let faulted = QcCtx {
        members: &FaultMembers(fault.clone()),
        ..qc
    };
    assert_eq!(
        assemble_with(window, &faulted, &holding).err(),
        Some(BlockError::Fault(format!("BuildNewListFromBlock: {fault}"))),
        "{name}"
    );
    let not_built = QcCtx {
        members: &FaultMembers(StoreError::NotBuilt("a member read")),
        ..qc
    };
    assert!(
        matches!(
            assemble_with(window, &not_built, &holding),
            Err(BlockError::Refused(TemplateError::NotReady { .. }))
        ),
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

/// The CbTx's `merkleRootQuorums`.
fn quorums_root(block: &dash_consensus::Block) -> Option<MerkleRoot> {
    match &block.txs()[0].payload().expect("a payload").expect("decodes").payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.merkle_root_quorums,
        other => panic!("{other:?}"),
    }
}

/// The CbTx's `merkleRootMNList`.
fn mn_root(block: &dash_consensus::Block) -> MerkleRoot {
    match &block.txs()[0].payload().expect("a payload").expect("decodes").payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.merkle_root_mn_list,
        other => panic!("{other:?}"),
    }
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

/// A quorum's members: these masternodes.
struct ListMembers(Vec<Arc<Dmn>>);

impl QuorumMemberSource for ListMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Ok(self.0.iter().cloned().collect())
    }
}

/// Every member read failing with its error.
struct FaultMembers(StoreError);

impl QuorumMemberSource for FaultMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Err(MemberError::View(self.0.clone()))
    }
}

/// The members of a quorum on a list without masternodes: none.
struct EmptyMembers;

impl QuorumMemberSource for EmptyMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Ok(Arc::from([]))
    }
}

fn unhex(s: &str) -> Vec<u8> {
    replay::unhex(s).expect("hex")
}

/// Core's default `CBLSSignature`, zeros under either scheme.
fn null_sig() -> BlsGlobalSig {
    BlsGlobalSig::decode(&[0; 96]).expect("zeros decode")
}

use std::cell::Cell;
use std::sync::Arc;

use dash_consensus::coins::CoinRef;
use dash_consensus::ctx::{Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{Header, OutPoint, Transaction, TxIn, TxOut, decode_tx_exact};
use dash_consensus::{BlockId, BlockValidationResult};
use dash_core_vectors::cases::{BlsScheme, load_trivial};
use dash_dml::creditpool::RangesSet;
use dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT;
use dash_dml::specialtx::reasons::{
    BAD_ASSETLOCKTX_NO_RETURN, BAD_ASSETUNLOCK_DUPLICATED_INDEX, BAD_CBTX_INVALID,
    BAD_MNHF_PAYLOAD, BAD_PROTX_PAYLOAD, BAD_PROTX_SIG, BAD_SHARED_COLLATERAL_CREATE,
    BAD_SHARED_COLLATERAL_SPEND,
};
use dash_dml::specialtx::sig::{inputs_hash_of, payload_sig_hash};
use dash_dml::testkit::{MemChain, dmn_with, operator_key, operator_sig, pro_tx};
use dash_dml::{MnType, OperatorKey};
use dash_evo::bls::BlsSchemeKind;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader, InputsHash, SpecialPayload, TxHash};
use dash_types::codec::BaseCodec;
use dashd_store::ErrorKind;

use super::*;
use crate::connect::{NoCheckpoints, pre_dip3_special_txs};
use crate::evo::node::node_net_info;
use crate::evo::testkit::cbtx_bytes;
use crate::store::{CHAINSTATE_SCHEMA, GuardedEngine, GuardedRead, ProbeEngine, RecordingSink};

fn tx(version: i16, tx_type: u16, coinbase: bool, n: u8) -> Arc<Tx> {
    tx_paying(version, tx_type, coinbase, n, &[0x51])
}

/// A type-7 transaction whose payload decodes and passes the checks before the quorum's: version 1,
/// a version bit no deployment uses, and a null signature.
fn mnhf_signal(n: u8) -> Arc<Tx> {
    let mut raw = tx(3, 7, false, n).raw().clone();
    raw.extra_payload = [&[1u8, 27][..], &[n; 32], &[0; 96]].concat();
    Arc::new(Tx::new(raw))
}

/// A type-9 transaction that passes the checks before the credit pool's: no inputs, one
/// withdrawal, payload version 1 with `index`.
fn asset_unlock(index: u64) -> Arc<Tx> {
    let mut raw = tx(3, 9, false, 9).raw().clone();
    raw.inputs.clear();
    raw.extra_payload =
        [&[1u8][..], &index.to_le_bytes(), &[0; 4], &[0; 4], &[9; 32], &[0; 96]].concat();
    Arc::new(Tx::new(raw))
}

fn tx_paying(version: i16, tx_type: u16, coinbase: bool, n: u8, script: &[u8]) -> Arc<Tx> {
    let prevout = if coinbase {
        OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        }
    } else {
        OutPoint {
            hash: TxHash::from([n; 32]),
            index: 0,
        }
    };
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout,
            script_sig: vec![1, n].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: script.to_vec().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// The blocks whose verdicts differ below DIP0003: classical, a raw-type-5 coinbase at versions
/// 1 and 2, a raw-type-5 `vtx[0]` that is no coinbase, a special transaction after the coinbase,
/// and a version-3 coinbase of types 1 and 8. Below DIP0003 Core admits those to `ConnectBlock`
/// (`ContextualCheckTransaction`'s type rules need DIP0003, `v:358-381`), and step 4 skips only a
/// raw type 5 (`stm:948-949`), so `CheckSpecialTxInner` rejects them (`stm:251-253`).
fn blocks() -> Vec<Block> {
    let header = || Header::new(BlockHeader::default());
    let cb = tx(1, 0, true, 0);
    [
        vec![cb.clone(), tx(2, 0, false, 1)],
        vec![tx(1, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, false, 2)],
        vec![cb.clone(), tx(3, 1, false, 3)],
        vec![cb, tx(2, 1, false, 4), tx(3, 8, false, 5)],
        vec![tx(3, 1, true, 0)],
        vec![tx(3, 8, true, 0)],
    ]
    .into_iter()
    .map(|txs| Block::new(header(), txs))
    .collect()
}

/// A view with no coin: the pre-DIP3 checks read none.
struct NoCoins;

impl CoinsView for NoCoins {
    type Error = CoinsReadError;

    fn coin(
        &self,
        _: &dash_consensus::primitives::OutPoint,
    ) -> Result<Option<dash_consensus::coins::CoinRef<'_>>, CoinsReadError> {
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// The consensus context of the block after `prev`.
fn cons<'a>(
    params: &'a ChainParams,
    chain: &'a MemChain,
    deployments: &'a dyn DeploymentView,
    prev: u32,
) -> ConsensusCtx<'a> {
    ConsensusCtx {
        params,
        height: prev + 1,
        prev: Some(BlockId(prev)),
        chain,
        deployments,
        checkpoints: &NoCheckpoints,
        bls: bls_ctx_for_block(params, prev + 1),
        median_time_past: 0,
        header_check_time: 0,
    }
}

fn cx<'a>(cons: &'a ConsensusCtx<'a>, list: &'a PrevList<'a>) -> SpecialTxCtx<'a> {
    SpecialTxCtx {
        cons,
        rules: SpecialTxRules::default(),
        coins: &NoCoins,
        indexes: None,
        check_sigs: true,
        list,
        net_info: node_net_info(cons.params, false).expect("net info rules"),
    }
}

/// Regtest, its chain to one block past DIP0003, and DIP0003's height.
fn regtest() -> (ChainParams, MemChain, u32) {
    let params =
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
    let dip3 = params.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    assert!(dip3 > 1, "regtest's DIP0003 is above genesis");
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), dip3 + 1);
    (params, chain, dip3)
}

/// A parent list that counts its resolutions: Core's empty list, or `fail`'s error.
struct Lists {
    calls: Cell<u32>,
    fail: bool,
}

impl Lists {
    fn new(fail: bool) -> Lists {
        Lists {
            calls: Cell::new(0),
            fail,
        }
    }

    fn resolve(&self) -> Result<Arc<MnList>, StoreError> {
        self.calls.set(self.calls.get() + 1);
        if self.fail {
            return Err(StoreError::inconsistent("no mn_diff row"));
        }
        Ok(Arc::new(MnList::default_pre_dip3()))
    }
}

/// H4's order over the seam: step 1 with `fCheckCbTxMerkleRoots` and a raw-type-5 `vtx[0]`,
/// then step 4's body for every transaction.
fn run<C: Ev1Checks>(
    checks: &C,
    block: &Block,
    check_cbtx_merkle_roots: bool,
    cx: SpecialTxCtx<'_>,
) -> Result<Result<(), SpecialTxFailure>, Fatal> {
    let payloads = checks.payloads(block);
    let raw_cbtx =
        block.txs().first().is_some_and(|tx| tx.tx_type().to_base() == TRANSACTION_COINBASE);
    if check_cbtx_merkle_roots
        && raw_cbtx
        && let Err(err) = checks.check_cbtx(block, &payloads, cx)?
    {
        return Ok(Err(err));
    }
    for i in 0..block.txs().len() {
        if let Err(err) = checks.check_tx(block, &payloads, i, cx)? {
            return Ok(Err(err));
        }
    }
    Ok(Ok(()))
}

/// D44 (DEC-581): on a chain below DIP0003 both bindings give `NoEvo`'s verdict for every block,
/// with and without the CbTx checks, and read no list; at DIP0003 the stand-in refuses.
#[test]
fn both_bindings_are_no_evo_below_dip3_and_the_stand_in_refuses_from_it() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let blocks = blocks();
    let lists = Lists::new(true);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);

    for prev in [0, 1, dip3 / 2, dip3 - 2] {
        let cons = cons(&params, &chain, &deployments, prev);
        let cx = cx(&cons, &list);
        for (n, block) in blocks.iter().enumerate() {
            for check in [false, true] {
                let want = Ok(pre_dip3_special_txs(block, check));
                for got in [run(&Ev1NotBuilt, block, check, cx), run(&DmlChecks, block, check, cx)]
                {
                    assert!(
                        !matches!(got, Ok(Err(SpecialTxFailure::NotReady(_)))),
                        "prev {prev} block {n}"
                    );
                    assert_eq!(got, want, "prev {prev} block {n} check {check}");
                }
            }
        }
    }
    assert_eq!(lists.calls.get(), 0, "no list below DIP0003");
    // The special-version coinbases: rejected at `vtx[0]`, which only a raw type 5 skips.
    let cons_1 = cons(&params, &chain, &deployments, 1);
    for block in &blocks[blocks.len() - 2..] {
        let got = run(&DmlChecks, block, true, cx(&cons_1, &list));
        let cb = &block.txs()[0];
        assert_rejected(&got, "Special Transaction check failed", cb, "bad-tx-type-dip3-inactive");
    }

    let cons_dip3 = cons(&params, &chain, &deployments, dip3 - 1);
    let at_dip3 = cx(&cons_dip3, &list);
    let refused = SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS);
    for block in &blocks {
        assert_eq!(Ev1NotBuilt.check_cbtx(block, &(), at_dip3), Ok(Err(refused.clone())));
        assert_eq!(Ev1NotBuilt.check_tx(block, &(), 0, at_dip3), Ok(Err(refused.clone())));
    }
    // V24 needs EV1's template check, at any height.
    let v24 = SpecialTxCtx {
        rules: SpecialTxRules {
            v24: true,
            evo_shares: false,
        },
        ..cx(&cons_1, &list)
    };
    assert_eq!(Ev1NotBuilt.check_tx(&blocks[0], &(), 0, v24), Ok(Err(refused)));
}

/// Step 4's body for each transaction of `block`.
fn step4(block: &Block, cx: SpecialTxCtx<'_>) -> Vec<Result<Result<(), SpecialTxFailure>, Fatal>> {
    let payloads = DmlChecks.payloads(block);
    (0..block.txs().len()).map(|i| DmlChecks.check_tx(block, &payloads, i, cx)).collect()
}

/// Asserts step 4 rejected `tx` for `reason`, with Core's `what` prefix (`stm:941-945`,
/// `:955-958`).
fn assert_rejected(
    got: &Result<Result<(), SpecialTxFailure>, Fatal>,
    what: &str,
    tx: &Tx,
    reason: &str,
) {
    let Ok(Err(SpecialTxFailure::Invalid(err))) = got else {
        panic!("{got:?}");
    };
    assert_eq!((err.result(), err.reject_reason()), (BlockValidationResult::Consensus, reason));
    let prefix = format!("{what} (tx hash {}) ", tx.txid());
    assert!(err.debug_message().starts_with(&prefix), "{}", err.debug_message());
}

/// From DIP0003 step 4 is EV1-3's dispatcher: a type it ports gives its reject under Core's
/// prefix, a type it does not, or type 6 without its seams (DEC-830 Q1), is a readiness refusal
/// (DEC-663), a raw-type-5 `vtx[0]` is skipped, and the parent's list is resolved once, at the
/// first transaction with a payload.
#[test]
fn from_dip3_step_4_is_the_dispatcher() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let cons = cons(&params, &chain, &deployments, dip3 - 1);
    let lists = Lists::new(false);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let cx = cx(&cons, &list);

    let classical = Block::new(
        Header::new(BlockHeader::default()),
        vec![tx(1, 0, true, 0), tx(2, 0, false, 1)],
    );
    assert_eq!(step4(&classical, cx), [Ok(Ok(())), Ok(Ok(()))]);
    assert_eq!(lists.calls.get(), 0, "no payload, no list");

    let pro_reg = tx(3, 1, false, 2);
    let block = Block::new(
        Header::new(BlockHeader::default()),
        vec![
            tx(3, TRANSACTION_COINBASE, true, 0),
            pro_reg.clone(),
            tx(3, 8, false, 3),
            tx(3, TRANSACTION_COINBASE, false, 4),
            tx(3, 4, false, 5),
            tx(3, 6, false, 6),
            tx(3, 7, false, 7),
            mnhf_signal(8),
        ],
    );
    let got = step4(&block, cx);
    assert_eq!(got[0], Ok(Ok(())), "vtx[0] is step 1's");
    assert_rejected(&got[1], "Special Transaction check failed", &pro_reg, BAD_PROTX_PAYLOAD);
    let failed = "Special Transaction check failed";
    assert_rejected(&got[2], failed, &block.txs()[2], BAD_ASSETLOCKTX_NO_RETURN);
    assert_rejected(&got[3], failed, &block.txs()[3], BAD_CBTX_INVALID);
    assert_rejected(
        &got[4],
        "Special Transaction check failed",
        &block.txs()[4],
        BAD_PROTX_PAYLOAD,
    );
    assert_eq!(got[5], Ok(Err(SpecialTxFailure::NotReady("the quorum commitment seams (CH7b)"))));
    assert_rejected(&got[6], failed, &block.txs()[6], BAD_MNHF_PAYLOAD);
    assert_eq!(got[7], Ok(Err(SpecialTxFailure::NotReady("the quorum manager (QM1a)"))));
    assert_eq!(lists.calls.get(), 1, "resolved once for the block");

    // A list that does not resolve is the store's fault, not a verdict.
    let failing = Lists::new(true);
    let resolve = || failing.resolve();
    let broken = PrevList::new(&resolve);
    let got = step4(
        &block,
        SpecialTxCtx {
            list: &broken,
            ..cx
        },
    );
    assert_eq!(got[0], Ok(Ok(())));
    assert!(
        matches!(&got[1], Err(fatal) if fatal.0.starts_with("GetListForBlock: ")),
        "{:?}",
        got[1]
    );
}

/// Step 4 gives type 9 the credit pool's indexes at `pindexPrev` (`stm:956`): an index in the
/// pool is `bad-assetunlock-duplicated-index`, before the quorum manager the check reads next; an
/// index not in it, or any without the pool (below V20), reaches the quorum manager.
#[test]
fn step_4_passes_the_pools_unlock_indexes() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let cons = cons(&params, &chain, &deployments, dip3 - 1);
    let lists = Lists::new(false);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let cx = cx(&cons, &list);
    let mut indexes = RangesSet::default();
    indexes.add(7);
    let pool = CreditPool {
        locked: 0,
        current_limit: 0,
        lately_unlocked: 0,
        indexes,
    };
    let block = Block::new(
        Header::new(BlockHeader::default()),
        vec![tx(3, TRANSACTION_COINBASE, true, 0), asset_unlock(7), asset_unlock(8)],
    );
    let unbound = Ok(Err(SpecialTxFailure::NotReady("the quorum manager (QM1a)")));
    let got = step4(
        &block,
        SpecialTxCtx {
            indexes: Some(&pool),
            ..cx
        },
    );
    let failed = "Special Transaction check failed";
    assert_rejected(&got[1], failed, &block.txs()[1], BAD_ASSETUNLOCK_DUPLICATED_INDEX);
    assert_eq!(got[2], unbound);
    assert_eq!(step4(&block, cx)[1], unbound);
}

/// Under V24 every transaction's outputs are checked for the template first, the coinbase and
/// classical transactions included, below DIP0003 too (`stm:940-949`).
#[test]
fn under_v24_the_template_check_comes_first() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let lists = Lists::new(false);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let coinbase = tx_paying(3, TRANSACTION_COINBASE, true, 0, &SHARED_COLLATERAL_SCRIPT);
    let classical = tx_paying(2, 0, false, 1, &SHARED_COLLATERAL_SCRIPT);
    let block =
        Block::new(Header::new(BlockHeader::default()), vec![coinbase.clone(), classical.clone()]);
    for prev in [1, dip3 - 1] {
        let cons = cons(&params, &chain, &deployments, prev);
        let off = cx(&cons, &list);
        let on = SpecialTxCtx {
            rules: SpecialTxRules {
                v24: true,
                evo_shares: false,
            },
            ..off
        };
        assert_eq!(step4(&block, off), [Ok(Ok(())), Ok(Ok(()))], "prev {prev}");
        let got = step4(&block, on);
        for (got, tx) in got.iter().zip([&coinbase, &classical]) {
            assert_rejected(
                got,
                "Shared collateral check failed",
                tx,
                BAD_SHARED_COLLATERAL_CREATE,
            );
        }
    }
    assert_eq!(lists.calls.get(), 0);
}

/// A view with one coin paying `out`.
struct OneCoin(TxOut);

impl CoinsView for OneCoin {
    type Error = CoinsReadError;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        Ok(Some(CoinRef::from_output(&self.0, 1, false)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// A view whose every read fails, as the guarded engine's do once its store has.
struct FailedReads(GuardedEngine<ProbeEngine>);

impl CoinsView for FailedReads {
    type Error = CoinsReadError;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        self.0.read(CHAINSTATE_SCHEMA.cf("coins").expect("coins"), b"k")?;
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// H8 is EV1-3's covenant: a template output spent by anything but a ProDisTx is a reject, and a
/// failed read is the store's fault.
#[test]
fn h8_is_the_covenant() {
    let spend = tx(2, 0, false, 1);
    let template = OneCoin(TxOut {
        value: 1,
        script_pubkey: SHARED_COLLATERAL_SCRIPT.to_vec().into(),
    });
    let plain = OneCoin(TxOut {
        value: 1,
        script_pubkey: vec![0x51].into(),
    });
    assert_eq!(DmlChecks.shared_collateral_spends(&spend, &plain), Ok(Ok(())));
    assert_eq!(DmlChecks.shared_collateral_spends(&spend, &NoCoins), Ok(Ok(())));
    let got = DmlChecks.shared_collateral_spends(&spend, &template).expect("a verdict");
    assert_eq!(got.expect_err("a reject").reject_reason(), BAD_SHARED_COLLATERAL_SPEND);

    let db = GuardedEngine::probe(Arc::new(RecordingSink::default()));
    db.inner().fail_reads(Some(ErrorKind::Io));
    let fatal = DmlChecks.shared_collateral_spends(&spend, &FailedReads(db)).expect_err("a fault");
    assert!(fatal.0.starts_with("coins read: "), "{fatal:?}");
}

/// The dispatcher reads the attempt's rules, `fCheckCbTxMerkleRoots`, NetInfo rules and context.
#[test]
fn the_dispatcher_context_is_the_attempts() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let cons = cons(&params, &chain, &deployments, dip3 - 1);
    let lists = Lists::new(false);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let mainnet =
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("main");
    let parent = MnList::default_pre_dip3();
    for (v24, evo_shares, check_sigs) in [(true, false, true), (false, true, false)] {
        let cx = SpecialTxCtx {
            rules: SpecialTxRules {
                v24,
                evo_shares,
            },
            check_sigs,
            net_info: node_net_info(&mainnet, true).expect("net info rules"),
            ..cx(&cons, &list)
        };
        let ctx = dml_ctx(cx, &parent);
        assert_eq!(
            ctx.rules,
            specialtx::SpecialTxRules {
                v24,
                evo_shares
            }
        );
        assert_eq!(ctx.evo.check_sigs, check_sigs);
        assert_eq!(ctx.net_info, cx.net_info);
        assert!(std::ptr::eq(ctx.evo.cons, &cons) && std::ptr::eq(ctx.list, &parent));
    }
}

/// Core's first legacy-scheme registration in `trivially_valid.json`: its collateral is an
/// outpoint, read from the view.
fn core_pro_reg() -> Arc<Tx> {
    let file = "trivially_valid.json";
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    let case = load_trivial(file, &text)
        .expect(file)
        .into_iter()
        .find(|c| c.tx_type == "proregtx" && c.scheme == BlsScheme::Legacy)
        .expect("a legacy ProRegTx row");
    Arc::new(decode_tx_exact(&case.tx).expect("decodes").into_value())
}

/// Step 4's coins fault is the store's, never a verdict: a registration whose collateral read
/// fails stops the attempt.
#[test]
fn a_failed_collateral_read_in_step_4_is_fatal() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let cons = cons(&params, &chain, &deployments, dip3 - 1);
    let lists = Lists::new(false);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    // Core's vector registers a mainnet address, so its network fields need mainnet's rules.
    let mainnet =
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("main");
    let net_info = node_net_info(&mainnet, false).expect("net info rules");
    let pro_reg = core_pro_reg();
    let block = Block::new(
        Header::new(BlockHeader::default()),
        vec![tx(3, TRANSACTION_COINBASE, true, 0), pro_reg],
    );
    // Over no coin the registration reads its collateral as missing.
    let got = step4(
        &block,
        SpecialTxCtx {
            net_info,
            ..cx(&cons, &list)
        },
    );
    assert_rejected(
        &got[1],
        "Special Transaction check failed",
        &block.txs()[1],
        "bad-protx-collateral",
    );

    let db = GuardedEngine::probe(Arc::new(RecordingSink::default()));
    db.inner().fail_reads(Some(ErrorKind::Io));
    let failed = FailedReads(db);
    let got = step4(
        &block,
        SpecialTxCtx {
            coins: &failed,
            net_info,
            ..cx(&cons, &list)
        },
    );
    assert!(matches!(&got[1], Err(fatal) if fatal.0.starts_with("coins read: ")), "{:?}", got[1]);
}

/// A coinbase of `version` with raw type 5 and `payload`, which only version 3 and up carry.
fn cbtx_coinbase(version: i16, payload: Vec<u8>) -> Arc<Tx> {
    let mut cb = tx(version, TRANSACTION_COINBASE, true, 0).to_mutable();
    cb.extra_payload = payload;
    Arc::new(Tx::new(cb))
}

/// The CbTx of `version` at `height` with zero roots, decoded.
fn cbtx_of(version: u16, height: u32) -> CoinbaseCommitment {
    CoinbaseCommitment::decode(&mut cbtx_bytes(version, height, 0, 0).as_slice()).expect("a CbTx")
}

/// Step 1 for the block whose only transaction is `cb`, as H4 calls it.
fn step1(
    cb: Arc<Tx>,
    cx: SpecialTxCtx<'_>,
) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
    let block = Block::new(Header::new(BlockHeader::default()), vec![cb]);
    DmlChecks.check_cbtx(&block, &BlockPayloads::new(&block), cx)
}

/// Step 1's reason, asserting Core's class and debug message: none for `cbtx_payload`'s rejects
/// (`stm:902-904`, `:914-916`), the transaction's prefix for `CheckCbTx`'s (`:910-912`).
fn step1_reason(
    got: Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal>,
    tx: &Tx,
) -> String {
    let Ok(Err(SpecialTxFailure::Invalid(err))) = got else {
        panic!("{got:?}");
    };
    assert_eq!(err.result(), BlockValidationResult::Consensus);
    let reason = err.reject_reason().to_owned();
    let debug = match reason.as_str() {
        "bad-cbtx-invalid" | "bad-cbtx-payload" => String::new(),
        _ => format!("Special Transaction check failed (tx hash {}) ", tx.txid()),
    };
    assert_eq!(err.debug_message(), debug, "{reason}");
    reason
}

/// CH7 D56: step 1 is EV1-5a's `cbtx_payload` then `check_cbtx` for a `vtx[0]` with type bits 5,
/// below DIP0003 too (D18). Version-1 and -2 coinbases carry no payload field, so their slot is
/// `bad-cbtx-payload`, as Core's `GetTxPayload` fails on it (Opus EV1-5a m-1); a payload not read
/// to its last byte is too; a `vtx[0]` that is no coinbase is `bad-cbtx-invalid`. Then
/// `CheckCbTx`: regtest's DIP0008 is active at block 1, so version 1 passes only in block 2's
/// parent, block 1 (quirk 16), and version 3 needs V20.
#[test]
fn step_1_is_ev1_5a_on_the_type_bits() {
    let (params, chain, _) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let lists = Lists::new(true);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let (cons_1, cons_2) =
        (cons(&params, &chain, &deployments, 0), cons(&params, &chain, &deployments, 1));
    let (at_1, at_2) = (cx(&cons_1, &list), cx(&cons_2, &list));

    for (h, cx) in [(1, at_1), (2, at_2)] {
        assert_eq!(
            step1(cbtx_coinbase(3, cbtx_bytes(2, h, 0, 0)), cx),
            Ok(Ok(cbtx_of(2, h))),
            "{h}"
        );
    }
    assert_eq!(step1(cbtx_coinbase(3, cbtx_bytes(1, 1, 0, 0)), at_1), Ok(Ok(cbtx_of(1, 1))));

    let good = cbtx_bytes(2, 2, 0, 0);
    let mut long = good.clone();
    long.push(0);
    let short = good[..good.len() - 1].to_vec();
    let mut not_coinbase = tx(3, TRANSACTION_COINBASE, false, 7).to_mutable();
    not_coinbase.extra_payload = good.clone();
    for (cb, want) in [
        (cbtx_coinbase(1, Vec::new()), "bad-cbtx-payload"),
        (cbtx_coinbase(2, Vec::new()), "bad-cbtx-payload"),
        (cbtx_coinbase(3, long), "bad-cbtx-payload"),
        (cbtx_coinbase(3, short), "bad-cbtx-payload"),
        (cbtx_coinbase(3, Vec::new()), "bad-cbtx-payload"),
        (Arc::new(Tx::new(not_coinbase)), "bad-cbtx-invalid"),
        (cbtx_coinbase(3, cbtx_bytes(2, 3, 0, 0)), "bad-cbtx-height"),
        (cbtx_coinbase(3, cbtx_bytes(1, 2, 0, 0)), "bad-cbtx-version"),
        (cbtx_coinbase(3, cbtx_bytes(3, 2, 0, 0)), "bad-cbtx-version"),
        (cbtx_coinbase(3, cbtx_bytes(0, 2, 0, 0)), "bad-cbtx-version"),
    ] {
        let got = step1(cb.clone(), at_2);
        assert_eq!(step1_reason(got, &cb), want, "{:?}", cb.version());
    }
    assert_eq!(lists.calls.get(), 0, "step 1 reads no list");
}

/// Step 1 passes the attempt's V24 rule to `CheckCbTx` (`stm:907`): with V20 active, a version-4
/// CbTx needs V24 and a version-3 one needs it off.
#[test]
fn step_1_reads_the_attempts_v24_rule() {
    let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@3")
        .build()
        .expect("regtest");
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), 5);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let lists = Lists::new(true);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    let cons = cons(&params, &chain, &deployments, 4);
    let off = cx(&cons, &list);
    let on = SpecialTxCtx {
        rules: SpecialTxRules {
            v24: true,
            evo_shares: false,
        },
        ..off
    };
    let (v3, v4) = (cbtx_of(3, 5), cbtx_of(4, 5));
    let (cb3, cb4) =
        (cbtx_coinbase(3, cbtx_bytes(3, 5, 0, 0)), cbtx_coinbase(3, cbtx_bytes(4, 5, 0, 0)));
    assert_eq!(step1(cb3.clone(), off), Ok(Ok(v3)));
    assert_eq!(step1(cb4.clone(), on), Ok(Ok(v4)));
    assert_eq!(step1_reason(step1(cb4.clone(), off), &cb4), "bad-cbtx-version");
    assert_eq!(step1_reason(step1(cb3.clone(), on), &cb3), "bad-cbtx-version");
}

/// Buried deployments at `params`' heights for the block after `prev_height`, as
/// `DeploymentActiveAt(*pindexPrev)` and `DeploymentActiveAfter(pindexPrev)` read them
/// (`deploymentstatus.h:13-27`).
struct AfterHeight<'p> {
    params: &'p ChainParams,
    prev_height: u32,
}

impl DeploymentView for AfterHeight<'_> {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        self.first(d) <= prev.map_or(0, |_| self.prev_height + 1)
    }

    fn active_at(&self, _: BlockId, d: Deployment) -> bool {
        self.first(d) <= self.prev_height
    }
}

impl AfterHeight<'_> {
    fn first(&self, d: Deployment) -> u32 {
        match d {
            Deployment::Buried(dep) => self.params.consensus.deployment_height(dep).to_u32(),
            _ => u32::MAX,
        }
    }
}

/// Quirk 16 (`cbtx.cpp:35-38`): CbTx version 2 is required from DIP0008's height plus one, as
/// `CheckCbTx` asks DIP0008 of `pindexPrev`. Through `DmlChecks`' step 1, on each network: at
/// DIP0008's own height version 1 passes, one block later it is `bad-cbtx-version` and version 2
/// passes. Core's verdicts on regtest, where DIP0008 is at 1, are `ch7a_cbtx_dashd`'s (dashd's
/// `submitblock` accepts version 1 in block 1 and refuses it in block 2).
#[test]
fn quirk_16_cbtx_v2_from_dip0008_plus_one() {
    let chain = MemChain::new();
    let lists = Lists::new(true);
    let resolve = || lists.resolve();
    let list = PrevList::new(&resolve);
    for kind in [NetworkKind::Mainnet, NetworkKind::Testnet, NetworkKind::Regtest] {
        let params = ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's params");
        let dip8 = params.consensus.deployment_height(BuriedDeployment::Dip0008).to_u32();
        let verdict = |height: u32, version: u16| {
            let deployments = AfterHeight {
                params: &params,
                prev_height: height - 1,
            };
            let cons = ConsensusCtx {
                params: &params,
                height,
                prev: Some(BlockId(height - 1)),
                chain: &chain,
                deployments: &deployments,
                checkpoints: &NoCheckpoints,
                bls: bls_ctx_for_block(&params, height),
                median_time_past: 0,
                header_check_time: 0,
            };
            let cb = cbtx_coinbase(3, cbtx_bytes(version, height, 0, 0));
            match step1(cb.clone(), cx(&cons, &list)) {
                Ok(Ok(cbtx)) => {
                    assert_eq!(cbtx, cbtx_of(version, height));
                    "ok".to_owned()
                }
                got => step1_reason(got, &cb),
            }
        };
        assert_eq!(verdict(dip8, 1), "ok", "{kind:?}");
        assert_eq!(verdict(dip8 + 1, 1), "bad-cbtx-version", "{kind:?}");
        assert_eq!(verdict(dip8 + 1, 2), "ok", "{kind:?}");
    }
}

/// The masternode [`revocation`] revokes.
const OPERATOR: u8 = 0x31;

/// Core's trivially valid basic-scheme ProUpRevTx, naming [`OPERATOR`] over one input and signed
/// by the operator key of seed `signer`.
fn revocation(signer: u8) -> Arc<Tx> {
    let file = "trivially_valid.json";
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    let case = load_trivial(file, &text)
        .expect(file)
        .into_iter()
        .find(|c| c.tx_type == "prouprevtx" && c.scheme == BlsScheme::Basic)
        .expect("a basic ProUpRevTx row");
    let core: Tx = decode_tx_exact(&case.tx).expect("decodes").into_value();
    let Some(Ok(decoded)) = core.payload() else {
        panic!("Core's payload decodes");
    };
    let SpecialPayload::ProviderUpdateRevoke(mut p) = decoded.payload.clone() else {
        panic!("a ProUpRevTx");
    };
    let inputs = vec![TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0x35; 32]),
            index: 0,
        },
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }];
    p.pro_tx_hash = TxHash::from(*pro_tx(OPERATOR).as_bytes());
    p.inputs_hash = InputsHash::from_lendian(inputs_hash_of(&inputs));
    p.sig = operator_sig(signer, &payload_sig_hash(&p).expect("a preimage"));
    let mut extra_payload = Vec::new();
    #[allow(clippy::disallowed_methods, reason = "a test payload encoded into a transaction")]
    p.encode(&mut extra_payload);
    Arc::new(Tx::new(Transaction {
        version: 3,
        tx_type: TxType::ProviderUpdateRevoke,
        inputs,
        outputs: Vec::new(),
        lock_time: 0,
        extra_payload,
    }))
}

/// Step 4 binds the node's BLS verifier (EV1-4, DEC-830): with `check_sigs`
/// (`fCheckCbTxMerkleRoots`), a revocation its operator signed is accepted and one another key
/// signed is `bad-protx-sig`; without it the signature is not read (`specialtxman.cpp`
/// rc.4:1615-1618).
#[test]
fn step_4_verifies_operator_signatures() {
    let (params, mut chain, dip3) = regtest();
    let v19 = params.consensus.deployment_height(BuriedDeployment::V19).to_u32();
    let prev = dip3.max(v19);
    let tip = BlockId(dip3 + 1);
    chain.extend(tip, prev.saturating_sub(dip3));
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let cons = cons(&params, &chain, &deployments, prev);
    let operator: OperatorKey = operator_key(OPERATOR, BlsSchemeKind::Basic);
    let mut parent = MnList::new(chain.hash(BlockId(prev)), prev as i32, 0);
    parent
        .try_add(
            dmn_with(OPERATOR, MnType::Regular, |_, c| {
                c.version = 2;
                c.pub_key_operator = operator;
            }),
            true,
        )
        .expect("a unique masternode");
    let parent = Arc::new(parent);
    let resolve = || Ok(parent.clone());
    let list = PrevList::new(&resolve);
    let cb = tx(3, TRANSACTION_COINBASE, true, 0);
    for (signer, check_sigs, expected) in [
        (OPERATOR, true, None),
        (OPERATOR + 1, true, Some(BAD_PROTX_SIG)),
        (OPERATOR + 1, false, None),
    ] {
        let rev = revocation(signer);
        let block = Block::new(Header::new(BlockHeader::default()), vec![cb.clone(), rev.clone()]);
        let got = step4(
            &block,
            SpecialTxCtx {
                check_sigs,
                ..cx(&cons, &list)
            },
        );
        let what = format!("signer {signer:#x}, check_sigs {check_sigs}");
        match expected {
            None => assert_eq!(got[1], Ok(Ok(())), "{what}"),
            Some(reason) => {
                assert_rejected(&got[1], "Special Transaction check failed", &rev, reason)
            }
        }
    }
}

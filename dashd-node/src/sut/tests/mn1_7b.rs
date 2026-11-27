//! MN1-7b: `ChainGbt`'s fields turn over a real chain at its tip (MN1-6 r1 N2), and a template
//! from the node's pool that selects a ProRegTx commits the list after it in its CbTx (PLAN-MN1
//! §12.4).
// Core: v24.0.2 src/node/miner.cpp:49-61, :301-303; src/rpc/mining.cpp:891, :933, :940

use dash_consensus::ctx::DeploymentPos;
use dash_consensus::deployments::ThresholdState;
use dash_consensus::primitives::{Block, TxRef};
use dash_pkc::bls::{BlsPkBytes, BlsScIetf, BlsSecretKey};
use dash_primitives::SpecialPayload;
use dashd_mining::gbt::{ChainGbt, FieldsAt, FieldsTurn, GbtChain as _, TipFields};
use dashd_mining::{
    BlockTemplate, BlockTemplateSource as _, ChainTemplateSource, Grind, TemplateTxs, grind,
};

use super::mp1a9::{COIN, FEE, assert_valid, live, mature};
use super::*;
use crate::mining_adapters::MiningPool;

const OP_TRUE: [u8; 1] = [0x51];
/// Regtest's `powLimit` as compact bits.
const POW_LIMIT_BITS: u32 = 0x207f_ffff;

/// `ChainGbt`'s turn at `tip`, whose parent's time is `time`, at clock `now`.
fn fields(gbt: &ChainGbt, tip: &Block, time: u32, now: i64) -> TipFields {
    let at = FieldsAt {
        prev: tip.hash(),
        time,
        now,
    };
    match gbt.fields_turn(at).expect("V answers") {
        FieldsTurn::At(fields) => fields,
        FieldsTurn::Moved(to) => panic!("V's tip moved to {to}"),
    }
}

/// N2: at tip 143 of regtest blocks one second apart, the turn reads the tip, not its parent:
/// MTP is block 138's time (the parent's would be 137's), `UpdateTime` lifts `nTime` to MTP + 1
/// or the clock, the minimum-difficulty rule answers the PoW limit, `testdummy` (144-block
/// periods from time 0) is STARTED for block 144 (DEFINED after 142), and DIP0001 at 144 is
/// active after the tip (not after 142).
#[test]
fn chain_gbt_reads_the_fields_at_the_tip() {
    let dir = tempfile::tempdir().unwrap();
    let sut = sut(dir.path(), &["-testactivationheight=dip0001@144"]);
    loaded(&sut);
    let blocks = mature(&sut, 143);
    let tip = blocks.last().unwrap();
    let chain = sut.chain().unwrap().clone();
    let gbt = ChainGbt::new(chain.clone(), sut.params()).expect("regtest's PoW");
    let t0 = i64::from(dashd_chainstate::chain::genesis_block(sut.params()).header().time());

    let early = fields(&gbt, tip, 0, 0);
    assert_eq!(early.mtp, t0 + 138);
    assert_eq!(early.mtp, chain.view().mtp, "V's MTP of its tip");
    assert_eq!(i64::from(early.time), early.mtp + 1, "UpdateTime: MTP + 1 over an earlier clock");
    assert_eq!(early.min_difficulty_bits, Some(POW_LIMIT_BITS));
    assert!(early.versionbits.contains(&(DeploymentPos::TestDummy, ThresholdState::Started)));
    assert!(early.dip0001, "DIP0001 is active after the tip");

    let late = t0 + 143 + 2 * 60 * 60 + 1;
    let after = fields(&gbt, tip, 0, late);
    assert_eq!(i64::from(after.time), late, "UpdateTime: the clock");
    assert_eq!(after.min_difficulty_bits, Some(POW_LIMIT_BITS), "past 2 h: the PoW limit");
    let kept = fields(&gbt, tip, u32::MAX, late);
    assert_eq!(kept.time, u32::MAX, "UpdateTime never moves nTime back");
}

/// A basic-scheme operator key (DEC-1655: the payload's plain 48 bytes).
fn operator_key(n: u8) -> BlsPkBytes {
    BlsSecretKey::<BlsScIetf>::from_ikm(&[n; 32])
        .expect("32 bytes of key material")
        .public_key()
        .into()
}

fn p2pkh(hash: u8) -> Vec<u8> {
    [&[0x76, 0xa9, 0x14][..], &[hash; 20], &[0x88, 0xac]].concat()
}

/// A version-2 (basic BLS) Regular ProRegTx spending `inputs`: output 0 is its own 1000 DASH
/// collateral to a fresh P2PKH, output 1 the change less [`FEE`]; no service, no signature.
// Core: v24.0.2 src/evo/providertx.h:62-126, src/evo/specialtxman.cpp CheckProRegTx
fn pro_reg_tx(inputs: Vec<TxIn>, value_in: i64) -> TxRef {
    let mut p = Vec::new();
    p.extend(2u16.to_le_bytes());
    p.extend(0u16.to_le_bytes());
    p.extend(0u16.to_le_bytes());
    p.extend([0; 32]);
    p.extend(0u32.to_le_bytes());
    p.extend([0; 18]);
    p.extend([0x11; 20]);
    p.extend(operator_key(7).as_bytes());
    p.extend([0x22; 20]);
    p.extend(0u16.to_le_bytes());
    let payout = p2pkh(0x33);
    p.push(payout.len() as u8);
    p.extend(payout);
    p.extend(dash_dml::specialtx::sig::inputs_hash_of(&inputs));
    p.push(0);
    let collateral = 1000 * COIN;
    let outputs = vec![
        TxOut::from_duffs(collateral, p2pkh(0x44).into()),
        TxOut::from_duffs(value_in - collateral - FEE, OP_TRUE.to_vec().into()),
    ];
    let tx = Tx::new(Transaction::new(3, TxType::ProviderRegister, inputs, outputs, 0, p));
    assert!(matches!(tx.payload(), Some(Ok(d)) if d.is_complete()), "a complete ProRegTx");
    Arc::new(tx)
}

fn merkle_root_mn_list(template: &BlockTemplate) -> [u8; 32] {
    let coinbase = &template.block.txs()[0];
    match coinbase.payload().expect("a payload").expect("decodes").payload() {
        SpecialPayload::CoinbaseCommitment(cbtx) => *cbtx.merkle_root_mn_list().as_bytes(),
        other => panic!("{other:?}"),
    }
}

/// §12.4: past DIP0003, a ProRegTx in the node's pool is selected into the template, and the
/// CbTx's `merkleRootMNList` is the root of the list after it: the empty template's root
/// differs, and the block connects (V checks the root against its own build) with the new
/// masternode in V's list at the new tip.
// Core: v24.0.2 src/node/miner.cpp:219-247, :301-303
#[test]
fn a_selected_pro_reg_tx_is_in_the_cbtx_list_root() {
    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = live(dir.path(), &["-dip3params=2:2"]);
    let mempool = Arc::clone(sut.steps.mempool().expect("step 7 built it"));
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    let params = Arc::new(sut.params().clone());
    let source = ChainTemplateSource::new(chain.clone(), params, clock, None)
        .with_mempool(Arc::new(MiningPool::new(Arc::clone(&mempool))));
    let pow = *source.pow().expect("regtest's PoW");
    let connect = |template: BlockTemplate| {
        let mut block = template.block;
        let mut tries = u64::MAX;
        assert_eq!(grind(&mut block, &pow, &mut tries, &|| false), Grind::Found);
        let block = Arc::new(block);
        submit(&sut, &block);
        block
    };
    let mut blocks = Vec::new();
    for _ in 0..110 {
        let template = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("a template");
        blocks.push(connect(template));
    }

    let mut inputs = Vec::new();
    let mut value_in = 0;
    for block in &blocks {
        let coinbase = &block.txs()[0];
        inputs.push(TxIn::new(OutPoint::new(coinbase.txid(), 0), Vec::new().into(), u32::MAX));
        value_in += coinbase.outputs()[0].value();
        if value_in > 1001 * COIN {
            break;
        }
    }
    let pro_reg = pro_reg_tx(inputs, value_in);
    assert_valid(&mempool.handle().accept(Arc::clone(&pro_reg), false).expect("V replies"));

    let empty = source.create_new_block(&OP_TRUE, TemplateTxs::None).expect("no transactions");
    let template = source.create_new_block(&OP_TRUE, TemplateTxs::Mempool).expect("TBV passes");
    let txs = template.block.txs()[1..].iter();
    let selected: Vec<_> =
        txs.filter(|tx| tx.tx_type() != TxType::QuorumCommitment).map(|tx| tx.txid()).collect();
    assert_eq!(selected, [pro_reg.txid()], "the ProRegTx is selected, after the commitments");
    assert_ne!(merkle_root_mn_list(&template), merkle_root_mn_list(&empty));
    let root = merkle_root_mn_list(&template);
    let block = connect(template);
    let view = chain.view();
    assert_eq!(view.snap.tip.tip_hash, block.hash(), "the block connected");
    let list = &view.mn_list;
    let pro_tx_hash = dash_dml::ProTxHash::from(*pro_reg.txid().as_bytes());
    assert!(list.get(&pro_tx_hash).is_some(), "the new masternode is listed");
    let tip_root = dash_dml::merkle_root_mn_list_full(list).expect("a root");
    assert_eq!(*tip_root.as_bytes(), root, "the CbTx commits the list after the ProRegTx");
    assert_eq!(sut.down(), None);
}

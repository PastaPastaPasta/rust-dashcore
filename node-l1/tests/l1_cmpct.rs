//! `diff_cmpct_initdata` (P3 §6.4, E4): `dashd-p2p`'s `PartiallyDownloadedBlock` against Core's
//! `InitData` and `FillBlock` on fixed inputs of each form, with the paths each one takes.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::primitives::core_serialize;
use dash_consensus::{Block, Header, TxRef, decode_tx};
use dash_oracle_sys::{CompactFill, ErrorKind, Oracle, Pin, ReadStatus};
use dash_p2p_core::CmpctBlock;
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric as _;
use dashd_p2p::processing::compact::CompactBlock;
use node_l1::cmpct::{
    self, Agreed, Arg, FILL_MAX, FIXTURE_PAIR, fixture_block, fixture_compact, raw_coinbase,
    raw_spend, raw_spend64, transition_sizes,
};
use node_l1::{Outcome, target};

const OK: ReadStatus = ReadStatus::Ok;
const INVALID: ReadStatus = ReadStatus::Invalid;
const FAILED: ReadStatus = ReadStatus::Failed;

fn agreed(bytes: &[u8]) -> Agreed {
    cmpct::compare_with(&Oracle::new(Pin::V24_0), bytes)
        .unwrap_or_else(|d| panic!("{bytes:02x?}: diverges at {}", d.field))
}

/// Core's (and so the node's) `InitData` and `FillBlock` statuses.
fn statuses(bytes: &[u8]) -> (ReadStatus, Option<ReadStatus>) {
    let fill = agreed(bytes).fill.expect("both decode the inputs");
    (fill.init, fill.fill)
}

/// A built input over a synthetic block with a coinbase first and spends of the usual size:
/// `txs` are (seed, flags, mempool key), `decoys` the same for spends outside the block.
fn built(txs: &[(u32, u8, u8)], decoys: &[(u32, u8, u8)], nonce: u64, perturb: u8) -> Vec<u8> {
    built_shaped(0, txs, decoys, nonce, perturb)
}

/// [`built`] with its shape byte: 1 drops the coinbase (the first transaction is a spend), 2
/// makes the spends 64 bytes long.
fn built_shaped(
    shape: u8,
    txs: &[(u32, u8, u8)],
    decoys: &[(u32, u8, u8)],
    nonce: u64,
    perturb: u8,
) -> Vec<u8> {
    let mut b = vec![0, 0, (txs.len() - 1) as u8, shape];
    txs.iter().for_each(|t| b.extend(t.0.to_le_bytes()));
    b.push(decoys.len() as u8);
    decoys.iter().for_each(|d| b.extend(d.0.to_le_bytes()));
    b.extend(nonce.to_le_bytes());
    txs.iter().chain(decoys).for_each(|t| b.extend([t.1, t.2, 0]));
    b.extend([perturb, 0]);
    b
}

/// A built input over the fixture block: (flags, mempool key, extra key) of its three
/// transactions and of the colliding pair.
fn fixture(txs: [(u8, u8, u8); 3], pair: [(u8, u8, u8); 2]) -> Vec<u8> {
    let mut b = vec![0, 1];
    txs.iter().chain(&pair).for_each(|t| b.extend([t.0, t.1, t.2]));
    b.extend([0, 0]);
    b
}

/// A transition input: `n` short IDs, 12 or 13 of them in one bucket.
fn transition(n: usize, thirteen: bool, placement: u8, prefill: bool, fill: bool) -> Vec<u8> {
    let index = transition_sizes().iter().position(|&s| s == n).expect("a transition size");
    let flags = u8::from(thirteen) | placement << 1 | u8::from(prefill) << 3 | u8::from(fill) << 4;
    let mut b = vec![1];
    b.extend((index as u16).to_le_bytes());
    b.push(flags);
    b.extend(0x1234_5678u32.to_le_bytes());
    b.extend(3u16.to_le_bytes());
    b
}

fn chunk(bytes: &[u8]) -> Vec<u8> {
    [&(bytes.len() as u16).to_le_bytes()[..], bytes].concat()
}

/// A raw input.
fn raw(cmpct: &[u8], pool: &[Vec<u8>], extra: &[Vec<u8>], blocktxn: Option<&[u8]>) -> Vec<u8> {
    let mut b = vec![2];
    b.extend(chunk(cmpct));
    b.push(pool.len() as u8);
    pool.iter().for_each(|t| b.extend(chunk(t)));
    b.push(extra.len() as u8);
    extra.iter().for_each(|t| b.extend(chunk(t)));
    b.push(u8::from(blocktxn.is_some()));
    b.extend(chunk(blocktxn.unwrap_or_default()));
    b
}

fn fixture_cmpct() -> Vec<u8> {
    fixture_compact().expect("the fixture").wire_bytes()
}

fn fixture_bytes() -> Vec<u8> {
    core_serialize(&fixture_block().expect("the fixture"))
}

/// The fixture's spends' Core serializations.
fn fixture_spends() -> [Vec<u8>; 2] {
    cmpct::FIXTURE_SEEDS.1.map(raw_spend)
}

#[test]
fn registered_at_v24() {
    let t = target("diff_cmpct_initdata").expect("registered");
    assert_eq!(t.pins, [Pin::V24_0]);
    assert!(t.start.is_some());
}

/// `InitData`'s three statuses: `Ok`; `Invalid` for no transactions at all, a null header and a
/// prefilled index past the block; `Failed` for a repeated short ID and for 13 in one bucket.
#[test]
fn init_data_statuses_agree() {
    let five: Vec<(u32, u8, u8)> = (10..15).map(|s| (s, 0, 0)).collect();
    assert_eq!(statuses(&built(&five, &[], 1, 0)), (OK, Some(OK)));

    let mut empty = fixture_compact().expect("the fixture");
    empty.short_ids.clear();
    empty.prefilled.clear();
    assert_eq!(statuses(&raw(&empty.wire_bytes(), &[], &[], None)), (INVALID, None));

    let mut null = fixture_cmpct();
    null[72..76].fill(0);
    assert_eq!(statuses(&raw(&null, &[], &[], None)), (INVALID, None));

    let mut past = fixture_compact().expect("the fixture");
    past.prefilled[0].index = 3;
    assert_eq!(statuses(&raw(&past.wire_bytes(), &[], &[], None)), (INVALID, None));

    // Two slots carry the one decoy's short ID.
    let aliased = [(10, 0, 0), (11, 8, 0), (12, 8, 0)];
    assert_eq!(statuses(&built(&aliased, &[(99, 0, 0)], 1, 0)), (FAILED, None));

    assert_eq!(statuses(&transition(13, true, 0, true, true)), (FAILED, None));
}

/// `FillBlock`'s statuses through each `blocktxn` perturbation: the answer as asked (`Ok`), one
/// dropped or one added (`Invalid`, before and after the header is cleared), two swapped, one
/// replaced, and a header whose merkle root is wrong (`Failed`).
#[test]
fn each_blocktxn_perturbation_agrees() {
    let six: Vec<(u32, u8, u8)> = (20..26).map(|s| (s, 0, 0)).collect();
    for (perturb, want) in [OK, INVALID, INVALID, FAILED, FAILED, FAILED].into_iter().enumerate() {
        let bytes = built(&six, &[], 2, perturb as u8);
        assert_eq!(statuses(&bytes), (OK, Some(want)), "perturbation {perturb}");
        let fill = agreed(&bytes).fill.expect("decoded");
        assert_eq!(fill.block.is_some(), want == OK, "perturbation {perturb}");
    }
}

/// P3 D22's 12-against-13 case, through Core's real map: 12 IDs in one bucket reconstruct, 13
/// fail at the 13th insertion, wherever the crowd sits among the others.
#[test]
fn twelve_in_one_bucket_reconstruct_and_thirteen_fail() {
    let n = *transition_sizes().iter().find(|&&n| n >= 100).expect("a size");
    for placement in 0..3 {
        let twelve = transition(n, false, placement, true, true);
        assert_eq!(statuses(&twelve), (OK, Some(OK)), "n = {n}, placement {placement}");
        assert_eq!(agreed(&twelve).outcome, Outcome::BothAccept);
        let thirteen = transition(n, true, placement, true, true);
        assert_eq!(statuses(&thirteen), (FAILED, None), "n = {n}, placement {placement}");
    }
}

/// Every transition set of P3 D22 (n−1, n and n+1 at each change of the bucket count, up to
/// 65,535 short IDs), with 12 and with 13 in one bucket, agrees with Core, and Core fails exactly
/// the crowded ones. Past 65,535 transactions Core's decoder refuses the `cmpctblock`, and so does
/// the node's.
#[test]
fn every_bucket_count_transition_agrees() {
    let sizes = transition_sizes();
    assert!(sizes.contains(&13) && sizes.contains(&14) && sizes.len() > 300, "{}", sizes.len());
    for (i, &n) in sizes.iter().enumerate() {
        for thirteen in [false, true] {
            let prefill = i % 2 == 0;
            let bytes = transition(n, thirteen, (i % 3) as u8, prefill, true);
            let got = agreed(&bytes);
            let Some(fill) = got.fill else {
                assert!(n + usize::from(prefill) > 65_535, "n = {n}: both refused the decoding");
                continue;
            };
            let want = if thirteen && n >= 13 {
                (FAILED, None)
            } else if n == 0 && !prefill {
                (INVALID, None)
            } else if n <= FILL_MAX {
                (OK, Some(OK))
            } else {
                (OK, Some(INVALID))
            };
            assert_eq!((fill.init, fill.fill), want, "n = {n}, thirteen {thirteen}");
        }
    }
}

/// The mempool walk's order decides: with the pair's short ID in slot 1 and the other spend in
/// the mempool, the walk that meets one of the pair, then the spend, stops with every slot
/// filled and reconstructs a block with the decoy (`Failed`); the walk that meets both of the
/// pair first clears the slot, so the `blocktxn` answers it and the block reconstructs.
#[test]
fn the_mempool_order_decides_between_two_colliding_transactions() {
    let keys = fixture_compact().expect("the fixture");
    let [x, y] =
        [FIXTURE_PAIR.0, FIXTURE_PAIR.1].map(|s| cmpct::spend(s).expect("a spend").instance_hash());
    assert_ne!(x, y);
    assert_eq!(keys.short_id(&x), keys.short_id(&y), "FIXTURE_PAIR collides");

    // Coinbase prefilled; slot 1 has the pair's short ID; slot 2 (key 1 or 2) is in the mempool.
    let order = |spend_key: u8, x_key: u8, y_key: u8| {
        fixture([(1, 0, 0), (8, 0, 0), (2, spend_key, 0)], [(2, x_key, 0), (2, y_key, 0)])
    };
    assert_eq!(statuses(&order(1, 0, 2)), (OK, Some(FAILED)), "X, spend, Y");
    assert_eq!(statuses(&order(1, 2, 0)), (OK, Some(FAILED)), "Y, spend, X");
    let both_first = agreed(&order(2, 0, 1));
    assert_eq!(both_first.outcome, Outcome::BothAccept, "X, Y, spend");
    assert_eq!(both_first.fill.and_then(|f| f.block), Some(fixture_bytes()));
}

/// The extra list's slot order decides as the mempool walk's does: the walk that meets one of
/// the pair, then the spend, stops with every slot filled (`Failed`); the walk that meets both of
/// the pair first clears the slot and the `blocktxn` answers it.
#[test]
fn the_extra_list_order_decides_between_two_colliding_transactions() {
    let order = |spend_key: u8, x_key: u8, y_key: u8| {
        fixture([(1, 0, 0), (8, 0, 0), (4, 0, spend_key)], [(4, 0, x_key), (4, 0, y_key)])
    };
    assert_eq!(statuses(&order(1, 0, 2)), (OK, Some(FAILED)), "X, spend, Y");
    assert_eq!(statuses(&order(1, 2, 0)), (OK, Some(FAILED)), "Y, spend, X");
    let both_first = agreed(&order(2, 0, 1));
    assert_eq!(both_first.outcome, Outcome::BothAccept, "X, Y, spend");
    assert_eq!(both_first.fill.and_then(|f| f.block), Some(fixture_bytes()));
}

/// `InitData` refuses a prefilled transaction with no inputs and no outputs (`IsNull`).
#[test]
fn a_null_prefilled_transaction_is_invalid() {
    let empty = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let null = Arc::new(decode_tx(&mut &empty[..]).expect("decodes").into_value());
    assert!(null.is_null());
    let mut cmpct = fixture_compact().expect("the fixture");
    cmpct.prefilled[0].tx = null;
    let [a, c] = fixture_spends();
    assert_eq!(statuses(&raw(&cmpct.wire_bytes(), &[a, c], &[], None)), (INVALID, None));
}

/// `IsBlockMutated`'s 64-byte rule: a block with no coinbase first and a 64-byte transaction is
/// `Failed` behind a correct merkle root; without the 64-byte transaction, or with a coinbase
/// first, it reconstructs.
#[test]
fn a_64_byte_transaction_without_a_coinbase_fails() {
    assert_eq!(raw_spend64(1).len(), 64);
    let spends: Vec<(u32, u8, u8)> = (40..44).map(|s| (s, 0, 0)).collect();
    assert_eq!(statuses(&built_shaped(3, &spends, &[], 4, 0)), (OK, Some(FAILED)));
    assert_eq!(statuses(&built_shaped(1, &spends, &[], 4, 0)), (OK, Some(OK)));
    assert_eq!(statuses(&built_shaped(2, &spends, &[], 4, 0)), (OK, Some(OK)));
}

/// The merkle tree's duplicated trailing pair (CVE-2012-2459): the block with its last
/// transaction twice has the root of the block without it, and `FillBlock` refuses it.
#[test]
fn a_duplicated_trailing_pair_fails() {
    let doubled = [(50, 0, 0), (51, 0, 0), (52, 1, 0), (52, 1, 0)];
    assert_eq!(statuses(&built(&doubled, &[], 5, 0)), (OK, Some(FAILED)));
    let distinct = [(50, 0, 0), (51, 0, 0), (52, 1, 0), (53, 1, 0)];
    assert_eq!(statuses(&built(&distinct, &[], 5, 0)), (OK, Some(OK)));
}

/// The extra list fills what the mempool does not, and a second extra match of the same txid
/// keeps the slot.
#[test]
fn the_extra_list_fills_a_slot() {
    let [a, c] = fixture_spends();
    let cmpct = fixture_cmpct();
    let filled = agreed(&raw(&cmpct, std::slice::from_ref(&a), std::slice::from_ref(&c), None));
    assert_eq!(filled.fill.and_then(|f| f.block), Some(fixture_bytes()));
    assert_eq!(statuses(&raw(&cmpct, std::slice::from_ref(&a), &[], None)), (OK, Some(INVALID)));
    assert_eq!(statuses(&raw(&cmpct, &[], &[a.clone(), c.clone()], None)), (OK, Some(OK)));
    assert_eq!(
        statuses(&raw(&cmpct, std::slice::from_ref(&a), &[c.clone(), c.clone()], None)),
        (OK, Some(OK))
    );
    assert_eq!(statuses(&raw(&cmpct, &[a.clone(), c.clone()], &[c], None)), (OK, Some(OK)));
    // A built block whose missing transactions all come from the extra list.
    let in_extra: Vec<(u32, u8, u8)> = (30..34).map(|s| (s, 4, 0)).collect();
    assert_eq!(statuses(&built(&in_extra, &[], 3, 0)), (OK, Some(OK)));
}

/// Raw inputs' decoding: a trailing byte, a cut `cmpctblock`, an undecodable mempool
/// transaction, and an undecodable `blocktxn` whatever `InitData` returns, are refused by both;
/// an empty `blocktxn` payload is no `blocktxn`.
#[test]
fn raw_decoding_verdicts_agree() {
    let cmpct = fixture_cmpct();
    let [a, c] = fixture_spends();
    let refused = |bytes: Vec<u8>, arg: Arg| {
        let got = agreed(&bytes);
        assert_eq!((got.outcome, got.fill, got.refused), (Outcome::BothReject, None, Some(arg)));
    };
    refused(raw(&[&cmpct[..], &[0]].concat(), &[], &[], None), Arg::Cmpct);
    refused(raw(&cmpct[..40], &[], &[], None), Arg::Cmpct);
    refused(raw(&cmpct, &[vec![0xff; 3]], &[], None), Arg::Pool);
    refused(raw(&cmpct, &[], &[[&a[..], &[0]].concat()], None), Arg::Extra);
    refused(raw(&cmpct, &[], &[], Some(&[1; 40])), Arg::Blocktxn);
    let mut empty = fixture_compact().expect("the fixture");
    empty.short_ids.clear();
    empty.prefilled.clear();
    refused(raw(&empty.wire_bytes(), &[], &[], Some(&[1; 40])), Arg::Blocktxn);
    // Core stops at the first argument it cannot decode, and so does the comparison.
    refused(raw(&cmpct, &[vec![0xff; 3]], &[], Some(&[1; 40])), Arg::Pool);
    refused(
        raw(&cmpct[..40], std::slice::from_ref(&a), &[vec![0xff; 3]], Some(&[1; 40])),
        Arg::Cmpct,
    );

    let pool = [a.clone(), c.clone()];
    assert_eq!(statuses(&raw(&cmpct, &pool, &[], Some(&[]))), (OK, Some(OK)));
    let block = fixture_block().expect("the fixture");
    let mut blocktxn = block.header().hash().as_bytes().to_vec();
    blocktxn.push(1);
    blocktxn.extend(&c);
    let answered = agreed(&raw(&cmpct, std::slice::from_ref(&a), &[], Some(&blocktxn)));
    assert_eq!(answered.fill.and_then(|f| f.block), Some(fixture_bytes()));
    // A coinbase in the `blocktxn` instead: the merkle root does not match.
    let mut wrong = block.header().hash().as_bytes().to_vec();
    wrong.push(1);
    wrong.extend(raw_coinbase(5));
    assert_eq!(statuses(&raw(&cmpct, &[a], &[], Some(&wrong))), (OK, Some(FAILED)));
}

/// A version 2 asset unlock: no inputs, one output, and a payload whose signature holds
/// `sig_seed`. Seeds give one txid and different instance hashes.
fn raw_asset_unlock(sig_seed: u32) -> Vec<u8> {
    let mut raw = vec![3, 0, 9, 0, 0, 1];
    raw.extend_from_slice(&100_000_000u64.to_le_bytes());
    raw.extend_from_slice(&[1, 0x51]);
    raw.extend_from_slice(&[0; 4]);
    // Version, index, fee, then the quorum info: requested height, quorum hash, signature.
    raw.extend_from_slice(&[145, 2]);
    raw.extend_from_slice(&7u64.to_le_bytes());
    raw.extend_from_slice(&1_000u32.to_le_bytes());
    raw.extend_from_slice(&100u32.to_le_bytes());
    raw.extend_from_slice(&[0xab; 32]);
    let mut sig = [0; 96];
    sig[..4].copy_from_slice(&sig_seed.to_le_bytes());
    raw.extend_from_slice(&sig);
    raw
}

/// Signature seeds of two asset unlocks with one short ID under [`collision_compact`]'s keys
/// (`dashd-p2p`'s `COLLIDING_UNLOCK_SEEDS`).
const UNLOCK_PAIR: (u32, u32) = (11_363_086, 14_128_518);

fn decoded(raw: &[u8]) -> TxRef {
    Arc::new(decode_tx(&mut &raw[..]).expect("a valid transaction").into_value())
}

fn unlock_header(merkle_root: MerkleRoot) -> BlockHeader {
    BlockHeader::new(42, BlockHash::from_lendian([7; 32]), merkle_root, 1, 0x207f_ffff, 0)
}

/// `dashd-p2p`'s collision compact block: `short_ids` under a zero-root header and nonce 7.
fn collision_compact(short_ids: Vec<u64>) -> CompactBlock {
    let mut c = CompactBlock::from_msg(CmpctBlock::new(
        unlock_header(MerkleRoot::ZERO),
        7,
        Vec::new(),
        Vec::new(),
    ));
    c.short_ids = short_ids;
    c
}

/// A `blocktxn` answering `c` with `txs`.
fn blocktxn(c: &CompactBlock, txs: &[&[u8]]) -> Vec<u8> {
    let mut b = c.header().hash().as_bytes().to_vec();
    b.push(txs.len() as u8);
    txs.iter().for_each(|t| b.extend_from_slice(t));
    b
}

/// Which hash keys which lookup, with asset unlocks whose instance hash is not their txid:
/// short IDs, the mempool walk and the extra list use the instance hash; mempool uniqueness and
/// the extra list's second-match compare use the txid.
#[test]
fn instance_hashes_key_short_ids_and_txids_key_uniqueness() {
    let (x, y) = (raw_asset_unlock(1), raw_asset_unlock(2));
    let (tx_x, tx_y) = (decoded(&x), decoded(&y));
    assert!(tx_x.is_asset_unlock_with_stable_txid());
    assert_eq!(tx_x.txid(), tx_y.txid());
    assert_ne!(tx_x.instance_hash(), tx_y.instance_hash());

    let txs = vec![decoded(&raw_coinbase(5)), Arc::clone(&tx_x)];
    let root = Block::new(Header::new(unlock_header(MerkleRoot::ZERO)), txs.clone()).merkle().0;
    let block = Block::new(Header::new(unlock_header(root)), txs);
    let full = core_serialize(&block);
    let c = CompactBlock::from_block(&block, 9);
    assert_eq!(c.short_ids, [c.short_id(&tx_x.instance_hash())]);
    let mut by_txid = c.clone();
    by_txid.short_ids = vec![c.short_id(&tx_x.txid())];
    let (c, by_txid) = (c.wire_bytes(), by_txid.wire_bytes());
    let block_of = |bytes: Vec<u8>| agreed(&bytes).fill.and_then(|f| f.block);

    // The short ID and the mempool walk: the instance hash.
    assert_eq!(block_of(raw(&c, std::slice::from_ref(&x), &[], None)), Some(full.clone()));
    assert_eq!(statuses(&raw(&c, std::slice::from_ref(&y), &[], None)), (OK, Some(INVALID)));
    assert_eq!(statuses(&raw(&by_txid, std::slice::from_ref(&x), &[], None)), (OK, Some(INVALID)));
    // The extra list: the instance hash.
    assert_eq!(block_of(raw(&c, &[], std::slice::from_ref(&x), None)), Some(full.clone()));
    assert_eq!(statuses(&raw(&c, &[], std::slice::from_ref(&y), None)), (OK, Some(INVALID)));
    assert_eq!(statuses(&raw(&by_txid, &[], std::slice::from_ref(&x), None)), (OK, Some(INVALID)));

    // Mempool uniqueness: the txid. Core's mempool refuses the second instance, and both sides
    // are given the first.
    let core = Oracle::new(Pin::V24_0);
    let both = core.partially_downloaded_block(&c, &[x.clone(), y.clone()], &[], None);
    assert_eq!(both.map_err(|e| e.kind).err(), Some(ErrorKind::Argument));
    let other = raw_spend(9);
    assert!(core.partially_downloaded_block(&c, &[x.clone(), other], &[], None).is_ok());
    assert_eq!(block_of(raw(&c, &[x.clone(), y.clone()], &[], None)), Some(full));
    assert_eq!(statuses(&raw(&c, &[y.clone(), x.clone()], &[], None)), (OK, Some(INVALID)));

    // The extra list's second match: the txid. Two instances under one short ID leave the
    // mempool's in place, so the `blocktxn` fills the last slot and the zero root fails.
    let (u, v) = (raw_asset_unlock(UNLOCK_PAIR.0), raw_asset_unlock(UNLOCK_PAIR.1));
    let (tx_u, tx_v) = (decoded(&u), decoded(&v));
    assert_eq!(tx_u.txid(), tx_v.txid());
    let probe = collision_compact(Vec::new());
    let id = probe.short_id(&tx_u.instance_hash());
    assert_eq!(id, probe.short_id(&tx_v.instance_hash()));
    let z = collision_compact(vec![id, id ^ 1]);
    let answer = blocktxn(&z, &[&raw_spend(9)]);
    let z_bytes = z.wire_bytes();
    let second = raw(&z_bytes, std::slice::from_ref(&u), std::slice::from_ref(&v), Some(&answer));
    assert_eq!(statuses(&second), (OK, Some(FAILED)));
    assert_eq!(statuses(&raw(&z_bytes, &[], &[], Some(&answer))), (OK, Some(INVALID)));
}

/// A transaction twice in the mempool reaches both sides once (Core refuses two of one txid).
#[test]
fn a_repeated_mempool_transaction_is_given_once() {
    let [a, c] = fixture_spends();
    let pool = [a.clone(), a, c];
    assert_eq!(statuses(&raw(&fixture_cmpct(), &pool, &[], None)), (OK, Some(OK)));
}

/// Each compared field is its own divergence.
#[test]
fn judge_names_each_field() {
    let core = CompactFill {
        init: OK,
        fill: Some(OK),
        block: Some(fixture_bytes()),
    };
    assert_eq!(cmpct::judge(&core, &core), Ok(Outcome::BothAccept));
    let field = |node: CompactFill| cmpct::judge(&core, &node).map_err(|d| d.field);
    assert_eq!(
        field(CompactFill {
            init: FAILED,
            fill: None,
            block: None,
        }),
        Err("InitData status")
    );
    assert_eq!(
        field(CompactFill {
            fill: Some(FAILED),
            block: None,
            ..core.clone()
        }),
        Err("FillBlock status")
    );
    let mut other = fixture_bytes();
    other[4] ^= 1;
    assert_eq!(
        field(CompactFill {
            block: Some(other),
            ..core.clone()
        }),
        Err("block bytes")
    );
}

/// A deterministic sweep over the three forms through the registered target; `node-fuzz` runs
/// the real search.
#[test]
fn a_sweep_agrees() {
    let t = target("diff_cmpct_initdata").expect("registered");
    (t.start.expect("a start"))(Pin::V24_0);
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut counts: HashMap<(u8, String), usize> = HashMap::new();
    for i in 0..900usize {
        let len = 1 + i % 300;
        let mut data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        data[0] = (i % 3) as u8;
        match (t.compare)(&data, Pin::V24_0) {
            Ok(outcome) => *counts.entry((data[0], format!("{outcome:?}"))).or_default() += 1,
            Err(d) => panic!("{data:02x?}: diverges at {}", d.field),
        }
    }
    eprintln!("{counts:?}");
    for mode in 0..3 {
        assert!(counts.contains_key(&(mode, "BothReject".into())), "form {mode}: {counts:?}");
    }
    assert!(counts.contains_key(&(0, "BothAccept".into())), "{counts:?}");
}

/// For `FIXTURE_PAIR`: the first spend seed from 1,000 whose instance hash shares a short ID
/// with an earlier one under the fixture `cmpctblock`'s keys (about 2^24 tries, 15 million
/// spends; minutes in a release build). Run it with
/// `cargo test --release -p node-l1 --features oracle --test l1_cmpct grind -- --ignored --nocapture`.
#[test]
#[ignore = "a one-off search; its result is FIXTURE_PAIR"]
fn grind_fixture_pair() {
    let keys = fixture_compact().expect("the fixture");
    let mut seen: HashMap<u64, u32> = HashMap::new();
    for seed in 1_000u32.. {
        let hash = cmpct::spend(seed).expect("a spend").instance_hash();
        if let Some(other) = seen.insert(keys.short_id(&hash), seed) {
            println!("FIXTURE_PAIR = ({other}, {seed})");
            return;
        }
    }
}

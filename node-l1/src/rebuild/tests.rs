//! Each plain constructor takes its arguments in the order [`Rebuild::rebuild`] passes them: a
//! value built from distinct field values reads every one back through its getter, so a swap of
//! two same-typed parameters at a later re-pin fails here.

use dash_pkc::bls::core::{BlsSkValue, public_key, sign};
use dash_pkc::bls::{BlsSchemeKind, BlsValue};

use super::*;

fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("a secret key")
}

/// Two different signatures, each eager and global.
fn eager_sig(n: u8) -> BlsEagerSig {
    BlsEagerSig::new(sign(&sk(n), &[n; 32], BlsSchemeKind::Basic), BlsSchemeKind::Basic)
}

fn global_sig(n: u8) -> BlsGlobalSig {
    BlsGlobalSig::decode(&eager_sig(n).to_bytes(BlsSchemeKind::Basic)).expect("a signature")
}

fn script(n: u8) -> Vec<u8> {
    vec![n, n.wrapping_add(1)]
}

fn txout(n: u8) -> TxOut {
    TxOut::from_duffs(i64::from(n), script(n).into())
}

fn txin(n: u8) -> TxIn {
    TxIn::new(
        OutPoint::new(TxHash::from([n; 32]), u32::from(n)),
        script(n).into(),
        u32::from(n) + 1,
    )
}

/// `T::rebuild(f).fields() == f`.
fn round_trip<T: Rebuild>(f: T::Fields)
where
    T::Fields: Clone + PartialEq + std::fmt::Debug,
{
    assert_eq!(T::rebuild(f.clone()).fields(), f, "{}", std::any::type_name::<T>());
}

#[test]
fn every_plain_constructor_takes_its_fields_in_order() {
    assert_ne!(eager_sig(1), eager_sig(2), "distinct signatures");
    assert_ne!(global_sig(1), global_sig(2), "distinct signatures");
    round_trip::<TxOut>(TxOutFields {
        value: 1,
        script_pubkey: script(2).into(),
    });
    round_trip::<OutPoint>(OutPointFields {
        txid: TxHash::from([3; 32]),
        index: 4,
    });
    round_trip::<TxIn>(TxInFields {
        prevout: OutPoint::new(TxHash::from([5; 32]), 6),
        script_sig: script(7).into(),
        sequence: 8,
    });
    round_trip::<PayoutShare>(PayoutShareFields {
        script_payout: script(9).into(),
        reward: 10,
    });
    round_trip::<CollateralShare>(CollateralShareFields {
        amount: 11,
        script_refund: script(12).into(),
        script_reward: script(13).into(),
        key_id_owner: PubKeyHash::from([14; 20]),
    });
    round_trip::<Transaction>(TransactionFields {
        version: 3,
        tx_type: TxType::AssetLock,
        inputs: vec![txin(15), txin(16)],
        outputs: vec![txout(17), txout(18)],
        lock_time: 19,
        extra_payload: script(20),
    });
    round_trip::<CoinbaseCommitment>(CoinbaseCommitmentFields {
        version: CbCommitmentVersion::new(3),
        height: BlockHeight::from_u32(21),
        merkle_root_mn_list: MerkleRoot::from([22; 32]),
        merkle_root_quorums: Some(MerkleRoot::from([23; 32])),
        best_cl_height_diff: Some(24),
        best_cl_signature: Some(global_sig(25)),
        credit_pool_balance: Some(26),
        merkle_root_asset_unlocks: Some(MerkleRoot::from([27; 32])),
    });
    round_trip::<AssetLock>(AssetLockFields {
        version: 28,
        credit_outputs: vec![txout(29), txout(30)],
    });
    round_trip::<MnHardFork>(MnHardForkFields {
        version: 31,
        version_bit: 32,
        quorum_hash: QuorumHash::from([33; 32]),
        sig: eager_sig(34),
    });
    round_trip::<AssetUnlock>(AssetUnlockFields {
        version: 35,
        index: 36,
        fee: 37,
        requested_height: 38,
        quorum_hash: QuorumHash::from([39; 32]),
        quorum_sig: global_sig(40),
    });
    round_trip::<Commitment>(CommitmentFields {
        version: 41,
        llmq_type: LlmqType::LlmqTest,
        quorum_hash: QuorumHash::from([42; 32]),
        quorum_index: Some(43),
        signers: DynBitset::new(9, vec![0b0000_0101, 0b1]).expect("a bitset"),
        valid_members: DynBitset::new(9, vec![0b0000_1010, 0]).expect("a bitset"),
        quorum_public_key: BlsEagerPk::new(public_key(&sk(44)), BlsSchemeKind::Basic),
        quorum_vvec_hash: QuorumVvecHash::from([45; 32]),
        quorum_sig: eager_sig(46),
        members_sig: eager_sig(47),
    });
}

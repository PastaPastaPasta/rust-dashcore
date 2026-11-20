use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use dash_consensus::primitives::decode_tx_exact;
use dash_consensus::{Tx, TxRef};
use dash_core_vectors::cases::{BlsScheme, load_trivial};
use dash_core_vectors::read_core_text;
use dash_dml::testkit::{dmn_with, operator_key};
use dash_dml::{MnList, MnType, ProTxHash};
use dash_pkc::bls::{BlsGlobalSig, BlsSchemeKind};
use dash_primitives::{AssetUnlock, OutPoint, QuorumHash, Transaction, TxHash, TxOut, TxType};
use dash_types::codec::BaseCodec;

use super::super::{EntryMetadata, PendingEvent, PoolState};
use super::payload;
use super::protx::key_hashes;

fn fixture(kind: &str, scheme: BlsScheme) -> TxRef {
    fixture_n(kind, scheme, 0)
}

fn fixture_n(kind: &str, scheme: BlsScheme, ordinal: usize) -> TxRef {
    let rows =
        load_trivial("trivially_valid.json", &read_core_text("src/test/data/trivially_valid.json"))
            .expect("the pinned Core vectors load");
    let row = rows
        .into_iter()
        .filter(|row| row.tx_type == kind && row.scheme == scheme)
        .nth(ordinal)
        .expect("a valid fixture");
    Arc::new(decode_tx_exact(&row.tx).expect("fixture decodes").into_value())
}

fn add(pool: &mut PoolState, tx: TxRef, tip_list: &MnList) {
    pool.add_unchecked_at(pool.new_entry(tx, EntryMetadata::default()), tip_list);
}

fn tip_for(pro: TxHash, key: dash_pkc::bls::BlsLazyPk, collateral: OutPoint) -> MnList {
    let mut list = MnList::scratch();
    let dmn = dmn_with(1, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = ProTxHash::from(*pro.as_bytes());
        dmn.collateral = collateral;
        cold.version = 2;
        cold.pub_key_operator = key;
    });
    list.insert_for_tests(dmn);
    list
}

fn registrar_for(template: &Tx, pro: TxHash) -> TxRef {
    let Some(Ok(decoded)) = template.payload() else {
        panic!("registrar fixture has a payload");
    };
    let dash_primitives::SpecialPayload::ProviderUpdateRegistrar(p) = decoded.payload() else {
        panic!("registrar fixture has the wrong payload");
    };
    let p = dash_primitives::ProUpRegTx::new(
        p.version(),
        pro,
        p.mode(),
        p.pub_key_operator().clone(),
        p.key_id_voting(),
        p.script_payout().clone(),
        p.payouts().clone(),
        p.inputs_hash(),
    );
    let mut payload = Vec::new();
    p.encode(&mut payload);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateRegistrar,
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        1,
        payload,
    )))
}

fn unlock_with_version(
    index: u64,
    requested_height: u32,
    value: i64,
    version: u8,
    sig: u8,
) -> TxRef {
    let signature = BlsGlobalSig::decode(&[sig; 96]).expect("fixture BLS signature decodes");
    let payload = AssetUnlock::new(
        version,
        index,
        7,
        requested_height,
        QuorumHash::from([index as u8; 32]),
        signature,
    );
    let mut extra_payload = Vec::new();
    payload.encode(&mut extra_payload);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(value, vec![0x51].into())],
        requested_height,
        extra_payload,
    )))
}

fn unlock(index: u64, requested_height: u32, value: i64) -> TxRef {
    unlock_with_version(index, requested_height, value, 1, 0)
}

#[test]
fn dash_map_invariants_hold_after_mixed_insertions_and_removals() {
    let registration = fixture("proregtx", BlsScheme::Basic);
    let service = fixture("proupservtx", BlsScheme::Basic);
    let registrar = fixture("proupregtx", BlsScheme::Basic);
    let revoke = fixture("prouprevtx", BlsScheme::Basic);
    let unlock = unlock(77, 100, 123);
    let list = MnList::default_pre_dip3();
    let mut pool = PoolState::default();

    for tx in [registration, service, registrar, revoke, unlock] {
        add(&mut pool, tx, &list);
        pool.check();
    }
    assert!(!pool.dash.protx_refs.is_empty());
    assert!(!pool.dash.addresses.is_empty());
    assert!(!pool.dash.owner_ids.is_empty());
    assert!(!pool.dash.bls_keys.is_empty());
    assert!(!pool.dash.collaterals.is_empty());
    assert!(!pool.dash.asset_unlock_indexes.is_empty());
    assert!(!pool.dash.asset_unlock_expiry.is_empty());

    let ids: Vec<_> = pool.entries.keys().copied().collect();
    for id in ids {
        let tx = pool.get(&id).expect("entry is resident");
        pool.remove_recursive(&tx, crate::types::MemPoolRemovalReason::Manual);
        pool.check();
    }
    assert!(pool.dash.protx_refs.is_empty());
    assert!(pool.dash.addresses.is_empty());
    assert!(pool.dash.owner_ids.is_empty());
    assert!(pool.dash.bls_keys.is_empty());
    assert!(pool.dash.collaterals.is_empty());
    assert!(pool.dash.asset_unlock_indexes.is_empty());
    assert!(pool.dash.asset_unlock_expiry.is_empty());
    assert_eq!(pool.pending_asset_unlock_amount(), 0);
}

#[test]
fn check_rejects_a_dash_map_key_attached_to_the_wrong_resident_transaction() {
    let first = fixture_n("proregtx", BlsScheme::Basic, 0);
    let second = fixture_n("proregtx", BlsScheme::Basic, 1);
    let mut pool = PoolState::default();
    add(&mut pool, first.clone(), &MnList::default_pre_dip3());
    add(&mut pool, second.clone(), &MnList::default_pre_dip3());
    let address = pool
        .dash
        .addresses
        .iter()
        .find_map(|(address, id)| (*id == first.txid()).then(|| address.clone()))
        .expect("an address keyed to the first registration");
    pool.check();
    pool.dash.addresses.insert(address, second.txid());
    assert!(catch_unwind(AssertUnwindSafe(|| pool.check())).is_err());
}

#[test]
fn asset_unlock_instance_replacement_rekeys_the_instance_map_and_hash_index() {
    let old = unlock_with_version(9, 100, 10, 2, 0);
    let fresh = unlock_with_version(9, 100, 10, 2, 1);
    assert_eq!(old.txid(), fresh.txid());
    assert_ne!(old.instance_hash(), fresh.instance_hash());
    let mut pool = PoolState::default();
    add(&mut pool, old.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.asset_unlock_by_instance_hash(old.instance_hash()), Some(old.txid()));
    pool.replace_asset_unlock_instance(old.txid(), fresh.clone());
    assert_eq!(pool.asset_unlock_by_instance_hash(old.instance_hash()), None);
    assert_eq!(pool.asset_unlock_by_instance_hash(fresh.instance_hash()), Some(old.txid()));
    assert_eq!(pool.asset_unlock_txids_by_index(9), [old.txid()]);
    pool.check();
}

#[test]
fn bls_operator_keys_are_indexed_for_legacy_and_basic_schemes() {
    let basic = fixture("proregtx", BlsScheme::Basic);
    let legacy = fixture("proregtx", BlsScheme::Legacy);
    let mut pool = PoolState::default();
    add(&mut pool, basic.clone(), &MnList::default_pre_dip3());
    add(&mut pool, legacy.clone(), &MnList::default_pre_dip3());
    for tx in [&basic, &legacy] {
        let hashes = key_hashes(tx).expect("valid fixture operator key");
        assert!(
            hashes
                .into_iter()
                .flatten()
                .any(|hash| pool.dash.bls_keys.get(&hash) == Some(&tx.txid()))
        );
    }
    let raw = legacy.raw().clone();
    let candidate = Arc::new(Tx::new(Transaction::new(
        raw.version(),
        raw.tx_type(),
        raw.inputs().clone(),
        raw.outputs().clone(),
        raw.lock_time() + 1,
        raw.extra_payload().clone(),
    )));
    assert!(pool.exists_provider_tx_cross_scheme_conflict(&candidate, &MnList::default_pre_dip3()));
    pool.check();
}

#[test]
fn key_change_ordering_tracks_service_updates_before_the_change() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let service_pro = match payload(&service).expect("service payload") {
        dash_primitives::SpecialPayload::ProviderUpdateService(p) => p.pro_tx_hash(),
        _ => panic!("service fixture payload"),
    };
    let registrar = registrar_for(&fixture("proupregtx", BlsScheme::Basic), service_pro);
    let list = tip_for(
        service_pro,
        operator_key(9, BlsSchemeKind::Basic),
        OutPoint::new(TxHash::from([9; 32]), 0),
    );
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &list);
    add(&mut pool, registrar.clone(), &list);
    assert_eq!(pool.service_updates_before_key_change(registrar.txid(), &list), [service.txid()]);
    assert!(pool.is_unorderable_service_update(&service, &[registrar.txid()], &list));
    pool.check();
}

#[test]
fn platform_node_id_is_indexed_for_evo_registration_and_service_conflicts() {
    let registration = fixture_n("proregtx", BlsScheme::Basic, 1);
    let platform = super::platform_id(&registration).expect("Core's Evo registration vector");
    let mut pool = PoolState::default();
    add(&mut pool, registration.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.dash.platform_ids.get(&platform), Some(&registration.txid()));
    let raw = registration.raw();
    let candidate = Arc::new(Tx::new(Transaction::new(
        raw.version(),
        raw.tx_type(),
        raw.inputs().clone(),
        raw.outputs().clone(),
        raw.lock_time() + 1,
        raw.extra_payload().clone(),
    )));
    assert!(pool.exists_provider_tx_conflict(&candidate, &MnList::default_pre_dip3()));
    pool.check();
}

#[test]
fn mpix12_mined_registration_evicts_pending_update_on_collateral() {
    let registration = fixture("proregtx", BlsScheme::Basic);
    let service = fixture("proupservtx", BlsScheme::Basic);
    let service_pro = service
        .payload()
        .and_then(Result::ok)
        .and_then(|p| match p.payload() {
            dash_primitives::SpecialPayload::ProviderUpdateService(p) => Some(p.pro_tx_hash()),
            _ => None,
        })
        .expect("service target");
    let registration_payload =
        registration.payload().and_then(Result::ok).expect("registration payload");
    let dash_primitives::SpecialPayload::ProviderRegister(p) = registration_payload.payload()
    else {
        panic!("registration fixture has the wrong payload");
    };
    let collateral = OutPoint::new(p.collateral_hash(), p.collateral_index());
    let list = tip_for(service_pro, operator_key(9, BlsSchemeKind::Basic), collateral);
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &list);
    assert!(pool.exists(&service.txid()));
    pool.remove_pro_tx_conflicts(&registration, &list);
    assert!(!pool.exists(&service.txid()));
    pool.check();
    assert!(
        matches!(pool.take_pending_events().as_slice(), [PendingEvent::TransactionRemovedFromMempool { tx, .. }] if tx.txid() == service.txid())
    );
}

#[test]
fn same_scheme_operator_key_is_also_a_cross_scheme_probe_hit() {
    let resident = fixture("proregtx", BlsScheme::Basic);
    let candidate = {
        let raw = resident.raw().clone();
        Arc::new(Tx::new(Transaction::new(
            raw.version(),
            raw.tx_type(),
            raw.inputs().clone(),
            raw.outputs().clone(),
            raw.lock_time() + 1,
            raw.extra_payload().clone(),
        )))
    };
    let mut pool = PoolState::default();
    add(&mut pool, resident, &MnList::default_pre_dip3());
    assert!(pool.exists_provider_tx_cross_scheme_conflict(&candidate, &MnList::default_pre_dip3()));
    pool.check();
}

#[test]
fn mpix14_only_an_operator_key_change_evicts_service_update() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let registrar_template = fixture("proupregtx", BlsScheme::Basic);
    let service_pro = service
        .payload()
        .and_then(Result::ok)
        .and_then(|p| match p.payload() {
            dash_primitives::SpecialPayload::ProviderUpdateService(p) => Some(p.pro_tx_hash()),
            _ => None,
        })
        .expect("service target");
    let registrar = registrar_for(&registrar_template, service_pro);
    let Some(Ok(decoded)) = registrar.payload() else {
        panic!("registrar payload");
    };
    let dash_primitives::SpecialPayload::ProviderUpdateRegistrar(registrar_payload) =
        decoded.payload()
    else {
        panic!("registrar payload type");
    };
    let changed_list = tip_for(
        service_pro,
        operator_key(9, BlsSchemeKind::Basic),
        OutPoint::new(TxHash::from([9; 32]), 0),
    );
    let same_list = tip_for(
        service_pro,
        registrar_payload.pub_key_operator().clone(),
        OutPoint::new(TxHash::from([9; 32]), 0),
    );
    let mut changed_pool = PoolState::default();
    add(&mut changed_pool, service.clone(), &changed_list);
    changed_pool.remove_pro_tx_conflicts(&registrar, &changed_list);
    assert!(!changed_pool.exists(&service.txid()));
    changed_pool.check();

    let mut same_pool = PoolState::default();
    add(&mut same_pool, service.clone(), &same_list);
    same_pool.remove_pro_tx_conflicts(&registrar, &same_list);
    assert!(same_pool.exists(&service.txid()));
    same_pool.check();
}

#[test]
fn mpix15_asset_unlock_index_claim_tracks_stale_and_fresher_entries() {
    let stale = unlock(42, 100, 2_100_000_000_000_000);
    let fresh = unlock(42, 200, 2_100_000_000_000_000);
    let mut pool = PoolState::default();
    add(&mut pool, stale.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.asset_unlock_txids_by_index(42), [stale.txid()]);
    pool.remove_asset_unlock_conflicts(&fresh);
    assert!(pool.asset_unlock_txids_by_index(42).is_empty());
    add(&mut pool, fresh.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.asset_unlock_txids_by_index(42), [fresh.txid()]);
    assert_eq!(pool.pending_asset_unlock_amount(), 2_100_000_000_000_007i64);
    pool.check();
}

#[test]
fn asset_unlock_removals_preserve_claim_order_and_notify_with_the_removed_tx() {
    let first = unlock(7, 100, 10);
    let second = unlock(7, 200, 20);
    let claimant = unlock(7, 300, 30);
    let mut pool = PoolState::default();
    for tx in [&first, &second] {
        add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    }
    pool.remove_asset_unlock_conflicts(&claimant);
    let events = pool.take_pending_events();
    assert_eq!(
        events
            .iter()
            .map(|event| match event {
                PendingEvent::TransactionRemovedFromMempool {
                    tx,
                    ..
                } => tx.txid(),
            })
            .collect::<Vec<_>>(),
        [first.txid(), second.txid()]
    );
    assert!(pool.asset_unlock_txids_by_index(7).is_empty());
    pool.check();
}

#[test]
fn pending_asset_unlock_total_caps_then_returns_to_exact_value() {
    let mut pool = PoolState::default();
    let txs: Vec<_> = (0..9_000).map(|index| unlock(index, 100, 2_100_000_000_000_000)).collect();
    for tx in &txs {
        add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    }
    assert_eq!(pool.pending_asset_unlock_amount(), i64::MAX);
    for tx in txs.iter().take(5_001) {
        pool.remove_recursive(tx, crate::types::MemPoolRemovalReason::Manual);
    }
    assert_eq!(pool.pending_asset_unlock_amount(), 8_397_900_000_000_027_993i64);
    pool.check();
}
// ---- Core regression cases ----
fn list_with(pro: TxHash, key: dash_pkc::bls::BlsLazyPk, version: i32) -> MnList {
    let mut list = MnList::scratch();
    let dmn = dmn_with(1, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = ProTxHash::from(*pro.as_bytes());
        dmn.collateral = OutPoint::new(TxHash::from([9; 32]), 0);
        cold.version = version;
        cold.pub_key_operator = key;
    });
    list.insert_for_tests(dmn);
    list
}

fn registrar_with_key(pro: TxHash, key: dash_pkc::bls::BlsLazyPk, lock: u32) -> TxRef {
    let template = fixture("proupregtx", BlsScheme::Basic);
    let Some(Ok(decoded)) = template.payload() else {
        panic!()
    };
    let dash_primitives::SpecialPayload::ProviderUpdateRegistrar(p) = decoded.payload() else {
        panic!()
    };
    let p = dash_primitives::ProUpRegTx::new(
        p.version(),
        pro,
        p.mode(),
        key,
        p.key_id_voting(),
        p.script_payout().clone(),
        p.payouts().clone(),
        p.inputs_hash(),
    );
    let mut payload = Vec::new();
    p.encode(&mut payload);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateRegistrar,
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        lock,
        payload,
    )))
}

fn share_update(pro: TxHash, reward_key: [u8; 20]) -> TxRef {
    let mut script = vec![0x76, 0xa9, 20];
    script.extend_from_slice(&reward_key);
    script.extend_from_slice(&[0x88, 0xac]);
    share_update_paying(pro, &script)
}

fn share_update_paying(pro: TxHash, script: &[u8]) -> TxRef {
    let mut b = Vec::new();
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(pro.as_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.push(script.len() as u8);
    b.extend_from_slice(script);
    b.extend_from_slice(&[0u8; 32]);
    b.push(65);
    b.extend_from_slice(&[1u8; 65]);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateShare,
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        b,
    )))
}

fn shared_registrar(pro: TxHash, key: &dash_pkc::bls::BlsLazyPk, voting: [u8; 20]) -> TxRef {
    let mut b = Vec::new();
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(pro.as_bytes());
    b.extend_from_slice(&key.to_bytes_as(BlsSchemeKind::Basic));
    b.extend_from_slice(&voting);
    b.extend_from_slice(&[0u8; 32]);
    b.push(0);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateSharedRegistrar,
        vec![],
        vec![TxOut::from_duffs(2, vec![0x51].into())],
        0,
        b,
    )))
}

#[test]
fn probe_voting_payee_exists_core_says_conflict() {
    let pro = TxHash::from([0x42; 32]);
    let key = operator_key(9, BlsSchemeKind::Basic);
    let list = list_with(pro, key.clone(), 2);
    let v = [0x77u8; 20];
    let share = share_update(pro, v);
    assert!(payload(&share).is_some(), "share decodes");
    let reg = shared_registrar(pro, &key, v);
    assert!(payload(&reg).is_some(), "shared reg decodes");
    let mut pool = PoolState::default();
    add(&mut pool, share.clone(), &list);
    // Core tm:1825-1838: pending ProUpShare paying PKHash(keyIDVoting) => conflict
    assert!(
        pool.exists_provider_tx_conflict(&reg, &list),
        "Core: SharedReg conflicts with pending Share paying its voting key"
    );
}

#[test]
fn probe_voting_payee_exists_mirror_core_says_conflict() {
    let pro = TxHash::from([0x42; 32]);
    let key = operator_key(9, BlsSchemeKind::Basic);
    let list = list_with(pro, key.clone(), 2);
    let v = [0x77u8; 20];
    let mut pool = PoolState::default();
    add(&mut pool, shared_registrar(pro, &key, v), &list);
    // Core tm:1855-1866
    assert!(
        pool.exists_provider_tx_conflict(&share_update(pro, v), &list),
        "Core: Share conflicts with pending SharedReg voting key"
    );
}

#[test]
fn probe_voting_payee_block_removal_core_evicts_share() {
    let pro = TxHash::from([0x42; 32]);
    let key = operator_key(9, BlsSchemeKind::Basic);
    let list = list_with(pro, key.clone(), 2);
    let v = [0x77u8; 20];
    let share = share_update(pro, v);
    let mut pool = PoolState::default();
    add(&mut pool, share.clone(), &list);
    pool.remove_pro_tx_conflicts(&shared_registrar(pro, &key, v), &list);
    // Core tm:1298 removeProTxVotingPayeeConflicts(..., TRANSACTION_PROVIDER_UPDATE_SHARE)
    assert!(!pool.exists(&share.txid()), "Core evicts the pending ProUpShare");
}

#[test]
fn probe_asset_unlock_expiry_is_requested_plus_48() {
    let tx = unlock(5, 100, 10);
    let mut pool = PoolState::default();
    add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    pool.remove_expired_asset_unlock(101);
    // Core: expiry = getHeightToExpiry() = 148; removed only when 148 < height
    assert!(pool.exists(&tx.txid()), "Core keeps the unlock at height 101");
}

#[test]
fn probe_normal_v3_tx_is_not_a_protx_conflict() {
    let tx = Arc::new(Tx::new(Transaction::new(
        3,
        TxType::Spend,
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        vec![],
    )));
    let pool = PoolState::default();
    assert!(
        !pool.exists_provider_tx_conflict(&tx, &MnList::default_pre_dip3()),
        "Core v:1116 returns false for a normal tx"
    );
}

#[test]
fn probe_normal_tx_is_not_a_cross_scheme_conflict() {
    let tx = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        vec![],
    )));
    let pool = PoolState::default();
    assert!(
        !pool.exists_provider_tx_cross_scheme_conflict(&tx, &MnList::default_pre_dip3()),
        "Core v:1134 returns false for a normal tx"
    );
}

#[test]
fn probe_revoked_mn_registrar_is_a_key_change() {
    let pro = TxHash::from([0x42; 32]);
    let zero = dash_pkc::bls::BlsLazyPk::new(BlsSchemeKind::Basic);
    let list = list_with(pro, zero, 2);
    let first = registrar_with_key(pro, operator_key(1, BlsSchemeKind::Basic), 1);
    let second = registrar_with_key(pro, operator_key(2, BlsSchemeKind::Basic), 2);
    let mut pool = PoolState::default();
    add(&mut pool, first.clone(), &list);
    // Core tm:665-669: isKeyChangeProTx = (zero != new) = true; second key change => protx-dup
    assert!(pool.exists_provider_tx_conflict(&second, &list), "Core: one key change per MN");
}

#[test]
fn probe_same_key_scheme_migration_is_not_a_key_change() {
    let pro = TxHash::from([0x42; 32]);
    let list = list_with(pro, operator_key(9, BlsSchemeKind::Legacy), 1);
    let migrate = registrar_with_key(pro, operator_key(9, BlsSchemeKind::Basic), 1);
    let change = registrar_with_key(pro, operator_key(3, BlsSchemeKind::Basic), 2);
    assert!(
        PoolState::default().key_change_target(&migrate, &list).is_none(),
        "lazy == is point equality"
    );
    let mut pool = PoolState::default();
    add(&mut pool, migrate.clone(), &list);
    // Core: isKeyChangeProTx false for a same-point migration, so a later change is admitted
    assert!(!pool.exists_provider_tx_conflict(&change, &list), "Core admits the later key change");
}

fn reg_payload(tx: &Tx) -> &dash_primitives::ProRegTx {
    match payload(tx) {
        Some(dash_primitives::SpecialPayload::ProviderRegister(p)) => p,
        _ => panic!("registration payload"),
    }
}

#[derive(Default)]
struct RegEdit<'a> {
    /// Supplies the addresses, keys, collateral and payout; the template keeps its type.
    donor: Option<&'a Tx>,
    net_from: Option<&'a Tx>,
    key: Option<dash_pkc::bls::BlsLazyPk>,
    owner: Option<[u8; 20]>,
    platform: Option<[u8; 20]>,
    collateral: Option<OutPoint>,
    lock_time: u32,
}

fn registration_from(template: &Tx, edit: RegEdit<'_>) -> TxRef {
    let base = reg_payload(template);
    let p = reg_payload(edit.donor.unwrap_or(template));
    let collateral =
        edit.collateral.unwrap_or(OutPoint::new(p.collateral_hash(), p.collateral_index()));
    let p = dash_primitives::ProRegTx::new(
        base.version(),
        base.mn_type(),
        base.mode(),
        collateral.txid(),
        collateral.index(),
        reg_payload(edit.net_from.unwrap_or(edit.donor.unwrap_or(template))).net_info().clone(),
        edit.owner.map(Into::into).unwrap_or(p.key_id_owner()),
        edit.key.unwrap_or_else(|| p.pub_key_operator().clone()),
        p.key_id_voting(),
        p.operator_reward(),
        p.script_payout().clone(),
        p.payouts().clone(),
        p.shares().clone(),
        p.join_sigs().clone(),
        p.early_period_blocks(),
        p.early_penalty(),
        p.inputs_hash(),
        edit.platform.map(Into::into).or(base.platform_node_id()),
        base.platform_p2p_port(),
        base.platform_http_port(),
    );
    let lock_time = edit.lock_time;
    let mut payload = Vec::new();
    p.encode(&mut payload);
    let raw = template.raw();
    Arc::new(Tx::new(Transaction::new(
        raw.version(),
        raw.tx_type(),
        raw.inputs().clone(),
        raw.outputs().clone(),
        lock_time,
        payload,
    )))
}

fn spend(prevout: OutPoint, lock_time: u32) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        vec![dash_primitives::TxIn::new(prevout, vec![].into(), u32::MAX)],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        lock_time,
        vec![],
    )))
}

/// Owner keys and platform node IDs share one namespace; when both maps hold a claimant
/// for the id, Core (tm:991-1031) evicts each of them.
fn owner_and_platform_claimants() -> (PoolState, TxRef, TxRef) {
    let owner = fixture_n("proregtx", BlsScheme::Basic, 0);
    let owner_id = *reg_payload(&owner).key_id_owner().as_bytes();
    let platform = registration_from(
        &fixture_n("proregtx", BlsScheme::Basic, 1),
        RegEdit {
            platform: Some(owner_id),
            ..RegEdit::default()
        },
    );
    assert_eq!(super::platform_id(&platform), Some(owner_id));
    let mut pool = PoolState::default();
    add(&mut pool, owner.clone(), &MnList::default_pre_dip3());
    add(&mut pool, platform.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.dash.owner_ids.get(&owner_id), Some(&owner.txid()));
    assert_eq!(pool.dash.platform_ids.get(&owner_id), Some(&platform.txid()));
    pool.check();
    (pool, owner, platform)
}

#[test]
fn mined_owner_key_evicts_both_owner_and_platform_claimants() {
    let (mut pool, owner, platform) = owner_and_platform_claimants();
    let owner_id = *reg_payload(&owner).key_id_owner().as_bytes();
    let mined = registration_from(
        &fixture_n("proregtx", BlsScheme::Legacy, 0),
        RegEdit {
            owner: Some(owner_id),
            ..RegEdit::default()
        },
    );
    assert!(!pool.exists_provider_tx_conflict(
        &registration_from(
            &mined,
            RegEdit {
                owner: Some([0xee; 20]),
                ..RegEdit::default()
            }
        ),
        &MnList::default_pre_dip3()
    ));
    pool.remove_pro_tx_conflicts(&mined, &MnList::default_pre_dip3());
    assert!(!pool.exists(&owner.txid()));
    assert!(!pool.exists(&platform.txid()), "Core also evicts the platform-ID claimant");
    pool.check();
}

#[test]
fn mined_platform_id_evicts_both_platform_and_owner_claimants() {
    let (mut pool, owner, platform) = owner_and_platform_claimants();
    let platform_id = super::platform_id(&platform).expect("Evo platform id");
    let donor = fixture_n("proregtx", BlsScheme::Legacy, 0);
    let mined = registration_from(
        &fixture_n("proregtx", BlsScheme::Basic, 1),
        RegEdit {
            donor: Some(&donor),
            platform: Some(platform_id),
            ..RegEdit::default()
        },
    );
    assert!(!pool.exists_provider_tx_conflict(
        &registration_from(
            &mined,
            RegEdit {
                platform: Some([0xee; 20]),
                ..RegEdit::default()
            }
        ),
        &MnList::default_pre_dip3()
    ));
    pool.remove_pro_tx_conflicts(&mined, &MnList::default_pre_dip3());
    assert!(!pool.exists(&platform.txid()));
    assert!(!pool.exists(&owner.txid()), "Core also evicts the owner-key claimant");
    pool.check();
}

#[test]
fn conflicting_spend_of_a_registration_evicts_its_collateral_ref_and_clears_deltas() {
    let funding = spend(OutPoint::new(TxHash::from([0xf1; 32]), 0), 0);
    let template = fixture("proregtx", BlsScheme::Basic);
    let registration = registration_from(
        &template,
        RegEdit {
            collateral: Some(OutPoint::new(funding.txid(), 0)),
            ..RegEdit::default()
        },
    );
    assert!(!registration.inputs().is_empty(), "the fixture registration spends an input");
    let registrar = registrar_for(&fixture("proupregtx", BlsScheme::Basic), registration.txid());
    let mut pool = PoolState::default();
    for tx in [&funding, &registration, &registrar] {
        add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    }
    for tx in [&funding, &registration, &registrar] {
        pool.prioritise_transaction(tx.txid(), 5);
    }
    pool.check();
    _ = pool.take_pending_events();

    let mined = spend(registration.inputs()[0].prevout(), 1);
    pool.remove_conflicts(&mined);
    for tx in [&funding, &registration, &registrar] {
        assert!(!pool.exists(&tx.txid()));
        assert!(!pool.deltas().contains_key(&tx.txid()), "Core clears every removed ref's delta");
    }
    assert!(!pool.dash.protx_refs.contains_key(&registration.txid()));
    pool.check();
    let removed: Vec<_> = pool
        .take_pending_events()
        .into_iter()
        .map(|event| match event {
            PendingEvent::TransactionRemovedFromMempool {
                tx,
                ..
            } => tx.txid(),
        })
        .collect();
    assert_eq!(removed.len(), 3);
}

/// One resident entry in every Dash map: an Evo registration with external collateral, a
/// key-changing registrar update for it, and a v1 (expiring) and a v2 (stable txid) asset unlock.
fn pool_with_every_dash_map() -> PoolState {
    let registration = fixture_n("proregtx", BlsScheme::Basic, 1);
    let pro = registration.txid();
    let list = list_with(pro, operator_key(1, BlsSchemeKind::Basic), 2);
    let mut pool = PoolState::default();
    for tx in [
        registration,
        registrar_with_key(pro, operator_key(2, BlsSchemeKind::Basic), 1),
        unlock(3, 100, 10),
        unlock_with_version(4, 100, 10, 2, 0),
    ] {
        add(&mut pool, tx, &list);
    }
    pool.check();
    pool
}

fn drop_first<K: Clone + std::hash::Hash + Eq, V>(map: &mut std::collections::HashMap<K, V>) {
    let key = map.keys().next().cloned().expect("the map has an entry");
    map.remove(&key);
}

fn drop_first_ordered<K: Clone + Ord, V>(map: &mut std::collections::BTreeMap<K, V>) {
    let key = map.keys().next().cloned().expect("the map has an entry");
    map.remove(&key);
}

#[test]
fn check_rejects_a_deleted_row_in_every_dash_map() {
    type Mutation = fn(&mut super::DashMaps);
    let mutations: [(&str, Mutation); 15] = [
        ("protx_refs", |m| drop_first_ordered(&mut m.protx_refs)),
        ("protx_refs row", |m| {
            let ids = m.protx_refs.values_mut().find(|ids| ids.len() > 1).expect("a shared ref");
            ids.pop();
        }),
        ("addresses", |m| drop_first(&mut m.addresses)),
        ("owner_ids", |m| drop_first(&mut m.owner_ids)),
        ("bls_keys", |m| drop_first(&mut m.bls_keys)),
        ("platform_ids", |m| drop_first(&mut m.platform_ids)),
        ("collaterals", |m| drop_first(&mut m.collaterals)),
        ("asset_unlock_instances", |m| drop_first_ordered(&mut m.asset_unlock_instances)),
        ("asset_unlock_indexes", |m| drop_first_ordered(&mut m.asset_unlock_indexes)),
        ("asset_unlock_indexes duplicate", |m| {
            let ids = m.asset_unlock_indexes.values_mut().next().expect("an index");
            ids.push(ids[0]);
        }),
        ("asset_unlock_expiry", |m| drop_first_ordered(&mut m.asset_unlock_expiry)),
        ("pending_asset_unlock_amount", |m| m.pending_asset_unlock_amount -= 1),
        ("valid_for_protx_key", |m| drop_first_ordered(&mut m.valid_for_protx_key)),
        ("key_change_protx", |m| {
            let id = *m.key_change_protx.iter().next().expect("a key change");
            m.key_change_protx.remove(&id);
        }),
        ("protx_index_context with its rows", |m| {
            let id = *m.key_change_protx.iter().next().expect("a key change");
            m.key_change_protx.remove(&id);
            m.valid_for_protx_key.remove(&id).expect("its valid-for key");
            m.protx_index_context.remove(&id).expect("its context");
        }),
    ];
    for (name, mutate) in mutations {
        let mut pool = pool_with_every_dash_map();
        mutate(&mut pool.dash);
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.check())).is_err(),
            "check() missed a mutated {name}"
        );
    }
}

#[test]
fn check_compares_asset_unlock_claims_per_index_as_a_multiset() {
    let mut unlocks = [unlock(7, 100, 10), unlock(7, 200, 20)];
    unlocks.sort_by_key(|tx| std::cmp::Reverse(tx.txid()));
    let mut pool = PoolState::default();
    for tx in &unlocks {
        add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    }
    assert_eq!(pool.asset_unlock_txids_by_index(7), unlocks.map(|tx| tx.txid()));
    pool.check();
}

fn removed_txids(pool: &mut PoolState) -> Vec<TxHash> {
    pool.take_pending_events()
        .into_iter()
        .map(|event| match event {
            PendingEvent::TransactionRemovedFromMempool {
                tx,
                ..
            } => tx.txid(),
        })
        .collect()
}

struct VotingCase {
    pro: TxHash,
    key: dash_pkc::bls::BlsLazyPk,
    list: MnList,
}

fn voting_case() -> VotingCase {
    let pro = TxHash::from([0x42; 32]);
    let key = operator_key(9, BlsSchemeKind::Basic);
    let list = list_with(pro, key.clone(), 2);
    VotingCase {
        pro,
        key,
        list,
    }
}

#[test]
fn voting_payee_conflicts_pair_only_the_other_update_type() {
    let c = voting_case();
    let voting = [0x77; 20];
    let share = share_update(c.pro, voting);
    let shared_reg = shared_registrar(c.pro, &c.key, voting);
    let mut pool = PoolState::default();
    add(&mut pool, share.clone(), &c.list);
    assert!(pool.exists_provider_tx_conflict(&shared_reg, &c.list));
    assert!(
        !pool.exists_provider_tx_conflict(&shared_registrar(c.pro, &c.key, [0x78; 20]), &c.list)
    );
    assert!(!pool.exists_provider_tx_conflict(&share_update(c.pro, voting), &c.list));

    let mut pool = PoolState::default();
    add(&mut pool, shared_reg.clone(), &c.list);
    assert!(pool.exists_provider_tx_conflict(&share, &c.list));
    assert!(!pool.exists_provider_tx_conflict(&share_update(c.pro, [0x78; 20]), &c.list));
    pool.check();
}

#[test]
fn share_update_for_a_masternode_missing_from_the_tip_list_is_a_conflict() {
    let c = voting_case();
    let share = share_update(TxHash::from([0x43; 32]), [0x77; 20]);
    assert!(payload(&share).is_some());
    assert!(PoolState::default().exists_provider_tx_conflict(&share, &c.list));
    assert!(
        !PoolState::default()
            .exists_provider_tx_conflict(&share_update(c.pro, [0x77; 20]), &c.list)
    );
}

#[test]
fn mined_voting_payee_updates_evict_the_other_type_and_notify() {
    let c = voting_case();
    let voting = [0x77; 20];
    let share = share_update(c.pro, voting);
    let shared_reg = shared_registrar(c.pro, &c.key, voting);

    let mut pool = PoolState::default();
    add(&mut pool, share.clone(), &c.list);
    pool.remove_for_block(&[shared_registrar(c.pro, &c.key, [0x78; 20])], &c.list);
    assert!(pool.exists(&share.txid()), "a different voting key leaves the share update");
    pool.remove_for_block(std::slice::from_ref(&shared_reg), &c.list);
    assert!(!pool.exists(&share.txid()));
    assert_eq!(removed_txids(&mut pool), [share.txid()]);
    pool.check();

    let mut pool = PoolState::default();
    add(&mut pool, shared_reg.clone(), &c.list);
    pool.remove_for_block(&[share_update(c.pro, [0x78; 20])], &c.list);
    assert!(pool.exists(&shared_reg.txid()), "a different reward key leaves the registrar update");
    pool.remove_for_block(std::slice::from_ref(&share), &c.list);
    assert!(!pool.exists(&shared_reg.txid()));
    assert_eq!(removed_txids(&mut pool), [shared_reg.txid()]);
    pool.check();
}

/// Core matches `ExtractDestination(scriptReward)`, which maps a pay-to-pubkey script to the
/// key's `PKHash` as well (script/standard.cpp:42-53, :167-174).
#[test]
fn pay_to_pubkey_reward_matches_the_voting_key_id() {
    let c = voting_case();
    for pubkey in [[0x02; 33].to_vec(), [0x04; 65].to_vec()] {
        let voting = dash_consensus::hash::hash160(&pubkey);
        let mut script = vec![pubkey.len() as u8];
        script.extend_from_slice(&pubkey);
        script.push(0xac);
        let share = share_update_paying(c.pro, &script);
        assert!(payload(&share).is_some());
        let shared_reg = shared_registrar(c.pro, &c.key, voting);

        let mut pool = PoolState::default();
        add(&mut pool, share.clone(), &c.list);
        assert!(pool.exists_provider_tx_conflict(&shared_reg, &c.list));
        pool.remove_for_block(std::slice::from_ref(&shared_reg), &c.list);
        assert!(!pool.exists(&share.txid()));

        let mut pool = PoolState::default();
        add(&mut pool, shared_reg.clone(), &c.list);
        assert!(pool.exists_provider_tx_conflict(&share, &c.list));

        let mut invalid = script.clone();
        invalid[1] = 0x05;
        assert!(!pool.exists_provider_tx_conflict(&share_update_paying(c.pro, &invalid), &c.list));
    }
}

fn tip_list_of(dmns: &[(TxHash, OutPoint)]) -> MnList {
    let mut list = MnList::scratch();
    for (n, (pro, collateral)) in dmns.iter().enumerate() {
        list.insert_for_tests(dmn_with(n as u8 + 1, MnType::Regular, |dmn, cold| {
            dmn.pro_tx_hash = ProTxHash::from(*pro.as_bytes());
            dmn.collateral = *collateral;
            cold.version = 2;
            cold.pub_key_operator = operator_key(n as u8 + 1, BlsSchemeKind::Basic);
        }));
    }
    list
}

fn service_pro(service: &Tx) -> TxHash {
    match payload(service) {
        Some(dash_primitives::SpecialPayload::ProviderUpdateService(p)) => p.pro_tx_hash(),
        _ => panic!("service payload"),
    }
}

/// MPIX-13: the resident key is indexed under its own scheme only, so a candidate that
/// encodes the same point under the other scheme is found by the cross-scheme probe alone.
#[test]
fn mpix13_same_point_under_the_other_scheme_is_a_cross_scheme_duplicate() {
    let list = MnList::default_pre_dip3();
    for (resident, other) in [
        (fixture_n("proregtx", BlsScheme::Legacy, 0), fixture_n("proregtx", BlsScheme::Basic, 0)),
        (fixture_n("proregtx", BlsScheme::Basic, 0), fixture_n("proregtx", BlsScheme::Legacy, 0)),
    ] {
        let candidate = registration_from(
            &other,
            RegEdit {
                key: Some(reg_payload(&resident).pub_key_operator().clone()),
                ..RegEdit::default()
            },
        );
        let mut pool = PoolState::default();
        add(&mut pool, resident.clone(), &list);
        let candidate_key = super::key_hash(&candidate).expect("candidate key");
        assert_ne!(Some(candidate_key), super::key_hash(&resident));
        assert!(!pool.dash.bls_keys.contains_key(&candidate_key));
        assert!(!pool.exists_provider_tx_conflict(&candidate, &list));
        assert!(pool.exists_provider_tx_cross_scheme_conflict(&candidate, &list));
        assert!(!pool.exists_provider_tx_cross_scheme_conflict(&other, &list));
        pool.check();
    }
}

#[test]
fn registration_collateral_conflicts_cover_each_admission_branch() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let collateral = OutPoint::new(TxHash::from([0xc0; 32]), 1);
    let candidate = registration_from(
        &fixture("proregtx", BlsScheme::Basic),
        RegEdit {
            collateral: Some(collateral),
            ..RegEdit::default()
        },
    );
    let free_list =
        tip_list_of(&[(service_pro(&service), OutPoint::new(TxHash::from([9; 32]), 0))]);
    let used_list = tip_list_of(&[(service_pro(&service), collateral)]);

    // MPIX-12: the collateral of a listed masternode with a pending update.
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &free_list);
    assert!(!pool.exists_provider_tx_conflict(&candidate, &free_list));
    assert!(pool.exists_provider_tx_conflict(&candidate, &used_list));
    let mut pool = PoolState::default();
    assert!(!pool.exists_provider_tx_conflict(&candidate, &used_list));

    // A pending registration already claims the collateral.
    let claimant = registration_from(
        &fixture("proregtx", BlsScheme::Legacy),
        RegEdit {
            collateral: Some(collateral),
            ..RegEdit::default()
        },
    );
    add(&mut pool, claimant, &free_list);
    assert!(pool.exists_provider_tx_conflict(&candidate, &free_list));

    // A pending transaction spends the collateral.
    let mut pool = PoolState::default();
    add(&mut pool, spend(collateral, 0), &free_list);
    assert!(pool.exists_provider_tx_conflict(&candidate, &free_list));
    pool.check();
}

#[test]
fn update_of_a_masternode_whose_collateral_a_pending_registration_reuses_is_a_conflict() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let collateral = OutPoint::new(TxHash::from([0xc0; 32]), 1);
    let list = tip_list_of(&[(service_pro(&service), collateral)]);
    let mut pool = PoolState::default();
    assert!(!pool.exists_provider_tx_conflict(&service, &list));
    let reuse = registration_from(
        &fixture("proregtx", BlsScheme::Basic),
        RegEdit {
            collateral: Some(collateral),
            ..RegEdit::default()
        },
    );
    add(&mut pool, reuse, &list);
    assert!(pool.exists_provider_tx_conflict(&service, &list));
    pool.check();
}

#[test]
fn registration_address_and_owner_conflicts_are_independent() {
    let list = MnList::default_pre_dip3();
    let resident = fixture_n("proregtx", BlsScheme::Basic, 1);
    let other = fixture_n("proregtx", BlsScheme::Legacy, 0);
    let mut pool = PoolState::default();
    add(&mut pool, resident.clone(), &list);
    assert!(!pool.exists_provider_tx_conflict(&other, &list));
    let same_address = registration_from(
        &other,
        RegEdit {
            net_from: Some(&resident),
            ..RegEdit::default()
        },
    );
    assert!(pool.exists_provider_tx_conflict(&same_address, &list));
    let same_owner = registration_from(
        &other,
        RegEdit {
            owner: Some(*reg_payload(&resident).key_id_owner().as_bytes()),
            ..RegEdit::default()
        },
    );
    assert!(pool.exists_provider_tx_conflict(&same_owner, &list));
    let owner_is_platform_id = registration_from(
        &other,
        RegEdit {
            owner: super::platform_id(&resident),
            ..RegEdit::default()
        },
    );
    assert!(pool.exists_provider_tx_conflict(&owner_is_platform_id, &list));
    pool.check();
}

fn removals(pool: &mut PoolState) -> Vec<(TxHash, crate::types::MemPoolRemovalReason)> {
    pool.take_pending_events()
        .into_iter()
        .map(|event| match event {
            PendingEvent::TransactionRemovedFromMempool {
                tx,
                reason,
                ..
            } => (tx.txid(), reason),
        })
        .collect()
}

/// MPIX-12, block side (tm:1244-1252): a mined ProRegTx spending a listed masternode's
/// collateral evicts that masternode's pending updates.
#[test]
fn remove_for_block_mined_registration_evicts_updates_of_the_replaced_masternode() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let collateral = OutPoint::new(TxHash::from([0xc0; 32]), 1);
    let list = tip_list_of(&[(service_pro(&service), collateral)]);
    let elsewhere =
        tip_list_of(&[(service_pro(&service), OutPoint::new(TxHash::from([9; 32]), 0))]);
    let mined = registration_from(
        &fixture("proregtx", BlsScheme::Basic),
        RegEdit {
            collateral: Some(collateral),
            ..RegEdit::default()
        },
    );
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &list);
    pool.remove_for_block(std::slice::from_ref(&mined), &elsewhere);
    assert!(pool.exists(&service.txid()));
    pool.remove_for_block(&[mined], &list);
    assert!(!pool.exists(&service.txid()));
    assert_eq!(
        removals(&mut pool),
        [(service.txid(), crate::types::MemPoolRemovalReason::Conflict)]
    );
    pool.check();
}

/// MPIX-14, block side: only a mined operator-key change evicts a pending service update.
#[test]
fn remove_for_block_evicts_service_updates_only_on_an_operator_key_change() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let pro = service_pro(&service);
    let old_key = operator_key(1, BlsSchemeKind::Basic);
    let list = list_with(pro, old_key.clone(), 2);
    for (mined, evicts) in [
        (registrar_with_key(pro, old_key.clone(), 1), false),
        (registrar_with_key(pro, operator_key(2, BlsSchemeKind::Basic), 2), true),
    ] {
        let mut pool = PoolState::default();
        add(&mut pool, service.clone(), &list);
        pool.remove_for_block(&[mined], &list);
        assert_eq!(pool.exists(&service.txid()), !evicts);
        pool.check();
    }
}

#[test]
fn remove_for_block_mined_parent_leaves_child_with_its_own_ancestor_stats() {
    let parent = spend(OutPoint::new(TxHash::from([0xaa; 32]), 0), 0);
    let child = spend(OutPoint::new(parent.txid(), 0), 0);
    let mut pool = PoolState::default();
    add(&mut pool, parent.clone(), &MnList::default_pre_dip3());
    add(&mut pool, child.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.entries[&child.txid()].ancestors.count, 2);
    pool.remove_for_block(std::slice::from_ref(&parent), &MnList::default_pre_dip3());
    assert!(!pool.exists(&parent.txid()));
    let entry = &pool.entries[&child.txid()];
    assert_eq!(entry.ancestors.count, 1);
    assert_eq!(entry.ancestors.size, entry.vsize);
    assert!(entry.parents.is_empty());
    assert!(removals(&mut pool).is_empty(), "Core does not notify BLOCK removals");
    pool.check();
}

#[test]
fn remove_for_block_clears_the_deltas_of_mined_txids() {
    let resident = spend(OutPoint::new(TxHash::from([0xab; 32]), 0), 0);
    let absent = spend(OutPoint::new(TxHash::from([0xac; 32]), 0), 0);
    let kept = spend(OutPoint::new(TxHash::from([0xad; 32]), 0), 0);
    let mut pool = PoolState::default();
    add(&mut pool, resident.clone(), &MnList::default_pre_dip3());
    for tx in [&resident, &absent, &kept] {
        pool.prioritise_transaction(tx.txid(), 7);
    }
    pool.remove_for_block(&[resident.clone(), absent.clone()], &MnList::default_pre_dip3());
    assert!(!pool.deltas().contains_key(&resident.txid()));
    assert!(!pool.deltas().contains_key(&absent.txid()));
    assert_eq!(pool.deltas().get(&kept.txid()), Some(&7));
    pool.check();
}

/// Core stores `getHeightToExpiry()` = requestedHeight + 48 as an `int` and evicts when it is
/// strictly below the new height (evo/assetlocktx.h:163-167, tm:1355-1369).
#[test]
fn asset_unlock_expires_strictly_after_requested_height_plus_48() {
    let tx = unlock(5, 100, 10);
    let stable = unlock_with_version(6, 100, 10, 2, 0);
    let mut pool = PoolState::default();
    add(&mut pool, tx.clone(), &MnList::default_pre_dip3());
    add(&mut pool, stable.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.dash.asset_unlock_expiry.get(&tx.txid()), Some(&148));
    pool.remove_expired_asset_unlock(148);
    assert!(pool.exists(&tx.txid()), "kept at its expiry height");
    assert!(removals(&mut pool).is_empty());
    pool.remove_expired_asset_unlock(149);
    assert!(!pool.exists(&tx.txid()), "evicted at expiry + 1");
    assert_eq!(removals(&mut pool), [(tx.txid(), crate::types::MemPoolRemovalReason::Expiry)]);
    assert!(pool.exists(&stable.txid()), "a stable-txid unlock has no expiry");
    pool.check();
}

#[test]
fn asset_unlock_expiry_wraps_like_cores_int() {
    let near_max = unlock(5, u32::MAX - 10, 10);
    let past_int_max = unlock(6, i32::MAX as u32 - 10, 10);
    let mut pool = PoolState::default();
    add(&mut pool, near_max.clone(), &MnList::default_pre_dip3());
    add(&mut pool, past_int_max.clone(), &MnList::default_pre_dip3());
    assert_eq!(pool.dash.asset_unlock_expiry.get(&near_max.txid()), Some(&37));
    assert!(pool.dash.asset_unlock_expiry[&past_int_max.txid()] < 0);
    pool.remove_expired_asset_unlock(0);
    assert!(pool.exists(&near_max.txid()));
    assert!(!pool.exists(&past_int_max.txid()));
    pool.remove_expired_asset_unlock(37);
    assert!(pool.exists(&near_max.txid()));
    pool.remove_expired_asset_unlock(38);
    assert!(!pool.exists(&near_max.txid()));
    pool.check();
}

fn fixture_pro(tx: &Tx) -> Option<TxHash> {
    match payload(tx)? {
        dash_primitives::SpecialPayload::ProviderUpdateService(p) => Some(p.pro_tx_hash()),
        dash_primitives::SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pro_tx_hash()),
        dash_primitives::SpecialPayload::ProviderUpdateRevoke(p) => Some(p.pro_tx_hash()),
        _ => None,
    }
}

/// Transactions of every Dash map family, and a tip list holding each masternode they update.
/// The first listed masternode's collateral is a registration fixture's, so the MPIX-12
/// paths run too.
fn property_universe() -> (Vec<TxRef>, MnList) {
    let mut txs = Vec::new();
    for kind in ["proregtx", "proupservtx", "proupregtx", "prouprevtx"] {
        let basic = if kind == "proregtx" {
            2
        } else {
            1
        };
        for (scheme, count) in [(BlsScheme::Basic, basic), (BlsScheme::Legacy, 5)] {
            txs.extend((0..count).map(|n| fixture_n(kind, scheme, n)));
        }
    }
    let shared = TxHash::from([0x42; 32]);
    let key = operator_key(40, BlsSchemeKind::Basic);
    txs.extend([
        share_update(shared, [0x77; 20]),
        share_update(shared, [0x78; 20]),
        shared_registrar(shared, &key, [0x77; 20]),
        shared_registrar(shared, &operator_key(41, BlsSchemeKind::Basic), [0x79; 20]),
        unlock(1, 100, 10),
        unlock(1, 200, 20),
        unlock(2, 150, 30),
        unlock_with_version(3, 120, 40, 2, 0),
    ]);
    let parent = spend(OutPoint::new(TxHash::from([0xaa; 32]), 0), 0);
    txs.push(spend(OutPoint::new(parent.txid(), 0), 0));
    txs.push(parent);

    let mut pros: Vec<TxHash> = txs.iter().filter_map(|tx| fixture_pro(tx)).collect();
    pros.sort_unstable();
    pros.dedup();
    let reused = reg_payload(&fixture("proregtx", BlsScheme::Basic)).collateral_hash();
    let mut dmns: Vec<_> = pros
        .iter()
        .enumerate()
        .map(|(n, pro)| (*pro, OutPoint::new(TxHash::from([n as u8 + 1; 32]), 7)))
        .collect();
    if let Some(first) = dmns.first_mut() {
        let index = reg_payload(&fixture("proregtx", BlsScheme::Basic)).collateral_index();
        first.1 = OutPoint::new(reused, index);
    }
    let mut list = tip_list_of(&dmns);
    list.insert_for_tests(dmn_with(200, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = ProTxHash::from(*shared.as_bytes());
        dmn.collateral = OutPoint::new(TxHash::from([0xee; 32]), 0);
        cold.version = 2;
        cold.pub_key_operator = key.clone();
    }));
    (txs, list)
}

/// Core's admission gates that the conflict maps answer, plus `add_unchecked`'s precondition
/// that no resident child already spends the transaction.
fn admissible(pool: &PoolState, tx: &Tx, list: &MnList) -> bool {
    !pool.exists(&tx.txid())
        && tx.inputs().iter().all(|input| pool.get_conflict_tx(&input.prevout()).is_none())
        && (0..tx.outputs().len())
            .all(|n| pool.get_conflict_tx(&OutPoint::new(tx.txid(), n as u32)).is_none())
        && !pool.exists_provider_tx_conflict(tx, list)
        && !pool.exists_provider_tx_cross_scheme_conflict(tx, list)
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Any admission-gated sequence of adds and removals leaves every Dash map equal to the
    /// maps rebuilt from the resident entries, and removing everything empties them. A mined
    /// tx leaves no resident copy, input conflict or delta (tm:1335-1361), and expiry leaves
    /// no unlock with `expiry < height` (tm:1369-1383).
    #[test]
    fn dash_maps_match_a_rebuild_after_any_add_and_remove_sequence(
        ops in proptest::collection::vec((0u8..4, 0usize..64, 0u32..300), 1..60)
    ) {
        let (txs, list) = property_universe();
        let mut pool = PoolState::default();
        for (op, index, height) in ops {
            let tx = &txs[index % txs.len()];
            match op {
                0 => {
                    let admissible = admissible(&pool, tx, &list);
                    if admissible {
                        add(&mut pool, tx.clone(), &list);
                    }
                }
                1 => {
                    if pool.exists(&tx.txid()) {
                        pool.remove_recursive(tx, crate::types::MemPoolRemovalReason::Manual);
                    }
                }
                2 => {
                    pool.remove_for_block(std::slice::from_ref(tx), &list);
                    proptest::prop_assert!(!pool.exists(&tx.txid()));
                    proptest::prop_assert!(
                        tx.inputs().iter().all(|input| pool.get_conflict_tx(&input.prevout()).is_none())
                    );
                    proptest::prop_assert!(!pool.deltas().contains_key(&tx.txid()));
                }
                _ => {
                    pool.remove_expired_asset_unlock(height);
                    proptest::prop_assert!(
                        pool.dash.asset_unlock_expiry.values().all(|expiry| *expiry >= height as i32)
                    );
                }
            }
            pool.check();
        }
        let ids: Vec<_> = pool.entries.keys().copied().collect();
        for id in ids {
            if let Some(tx) = pool.get(&id) {
                pool.remove_recursive(&tx, crate::types::MemPoolRemovalReason::Manual);
            }
        }
        pool.check();
        let empty = super::DashMaps::default();
        proptest::prop_assert!(pool.dash.protx_refs.is_empty() && pool.dash.addresses.is_empty());
        proptest::prop_assert!(pool.dash.owner_ids.is_empty() && pool.dash.bls_keys.is_empty());
        proptest::prop_assert!(pool.dash.platform_ids.is_empty() && pool.dash.collaterals.is_empty());
        proptest::prop_assert!(pool.dash.asset_unlock_indexes.is_empty());
        proptest::prop_assert!(pool.dash.asset_unlock_instances.is_empty());
        proptest::prop_assert!(pool.dash.asset_unlock_expiry.is_empty());
        proptest::prop_assert_eq!(pool.dash.pending_asset_unlock_amount, empty.pending_asset_unlock_amount);
        proptest::prop_assert!(pool.dash.valid_for_protx_key.is_empty() && pool.dash.key_change_protx.is_empty());
        proptest::prop_assert!(pool.dash.protx_index_context.is_empty());
    }
}

#[test]
fn property_universe_reaches_every_dash_map() {
    let (txs, list) = property_universe();
    let mut pool = PoolState::default();
    for tx in &txs {
        if admissible(&pool, tx, &list) {
            add(&mut pool, tx.clone(), &list);
        }
    }
    pool.check();
    let d = &pool.dash;
    for (name, filled) in [
        ("protx_refs", !d.protx_refs.is_empty()),
        ("addresses", !d.addresses.is_empty()),
        ("owner_ids", !d.owner_ids.is_empty()),
        ("bls_keys", !d.bls_keys.is_empty()),
        ("platform_ids", !d.platform_ids.is_empty()),
        ("collaterals", !d.collaterals.is_empty()),
        ("asset_unlock_instances", !d.asset_unlock_instances.is_empty()),
        ("asset_unlock_indexes", !d.asset_unlock_indexes.is_empty()),
        ("asset_unlock_expiry", !d.asset_unlock_expiry.is_empty()),
        ("valid_for_protx_key", !d.valid_for_protx_key.is_empty()),
        ("key_change_protx", !d.key_change_protx.is_empty()),
    ] {
        assert!(filled, "the property universe never fills {name}");
    }
}

/// removeProTxReferences (tm:1046-1064) leaves mapDeltas alone; only removeConflicts clears.
#[test]
fn spent_collateral_eviction_keeps_the_update_delta() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let collateral = OutPoint::new(TxHash::from([0xc0; 32]), 1);
    let list = tip_list_of(&[(service_pro(&service), collateral)]);
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &list);
    pool.prioritise_transaction(service.txid(), 5);
    pool.remove_for_block(&[spend(collateral, 0)], &list);
    assert!(!pool.exists(&service.txid()));
    assert_eq!(pool.deltas().get(&service.txid()), Some(&5));
    pool.check();
}

#[test]
fn mined_registration_reusing_the_collateral_keeps_the_update_delta() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let collateral = OutPoint::new(TxHash::from([0xc0; 32]), 1);
    let list = tip_list_of(&[(service_pro(&service), collateral)]);
    let mined = registration_from(
        &fixture("proregtx", BlsScheme::Basic),
        RegEdit {
            collateral: Some(collateral),
            ..RegEdit::default()
        },
    );
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &list);
    pool.prioritise_transaction(service.txid(), 5);
    pool.remove_for_block(&[mined], &list);
    assert!(!pool.exists(&service.txid()));
    assert_eq!(pool.deltas().get(&service.txid()), Some(&5));
    pool.check();
}

/// existsProviderTxConflict (tm:1655-1877) has no ProDis branch.
#[test]
fn undecodable_dissolve_is_not_a_conflict() {
    let bad = |ty| {
        Arc::new(Tx::new(Transaction::new(
            3,
            ty,
            vec![],
            vec![TxOut::from_duffs(1, vec![0x51].into())],
            0,
            vec![0xff],
        )))
    };
    let pool = PoolState::default();
    let list = MnList::default_pre_dip3();
    assert!(payload(&bad(TxType::ProviderDissolve)).is_none());
    assert!(pool.exists_provider_tx_conflict(&bad(TxType::ProviderUpdateService), &list));
    assert!(!pool.exists_provider_tx_conflict(&bad(TxType::ProviderDissolve), &list));
}

/// A ProUpServ indexed while its MN is missing has no validForProTxKey; any mined key
/// change for that MN evicts it (tm:643-646).
#[test]
fn service_update_without_a_key_record_is_evicted_by_a_mined_key_change() {
    let service = fixture("proupservtx", BlsScheme::Basic);
    let pro = service_pro(&service);
    let registrar = registrar_for(&fixture("proupregtx", BlsScheme::Basic), pro);
    let mut pool = PoolState::default();
    add(&mut pool, service.clone(), &MnList::default_pre_dip3());
    assert!(!pool.dash.valid_for_protx_key.contains_key(&service.txid()));
    let list = tip_for(
        pro,
        operator_key(9, BlsSchemeKind::Basic),
        OutPoint::new(TxHash::from([9; 32]), 0),
    );
    pool.remove_for_block(&[registrar], &list);
    assert!(!pool.exists(&service.txid()));
    pool.check();
}

//! The oracle half of EV1 D17: `key_changed` (`CheckProUpRegTx` step 5a, `!(ptx.pubKeyOperator ==
//! state.pubKeyOperator)`) observed through Core's `CheckSpecialTx` and ours on the same inputs.
//!
//! The observable is the V24 address migration: masternode 1's state holds a legacy address on
//! port 22, which ExtNetInfo refuses, and the ProUpRegTx raises it to ExtAddr. An unchanged key
//! runs the migration (`bad-protx-netinfo-version`); a changed one skips it and passes.
//!
//! The constructible cells are the ptx key read and decoded (`IsTriviallyValid`'s `Get()`,
//! `providertx.cpp` rc.4:458, rejects a key that does not decode before step 5a) against a state
//! key as the list's disk bytes leave it, raw and never read, under the state version's scheme.
//! A state key that is default, decoded by `Set`, or read before step 5a has no disk form, and
//! the same object cannot be both keys; the L0 `eq` matrix
//! (`dash-dml/src/specialtx/tests.rs`, `operator_key_equality_is_cores_d17`) covers those.
//!
//! Each cell also runs with masternode 2 holding a key, for step 5a's probe
//! (`specialtxman.cpp` rc.4:1515-1527, `bad-protx-dup-key`), which runs when the key changed or
//! the state migrates out of the legacy scheme.

use dash_consensus::primitives::Tx;
use dash_consensus::subsidy::COIN;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_dml::codec::core_disk;
use dash_dml::specialtx::SpecialTxRules;
use dash_dml::specialtx::reasons::{BAD_PROTX_DUP_KEY, BAD_PROTX_NETINFO_VERSION};
use dash_dml::specialtx::sig::inputs_hash_of;
use dash_dml::testkit::replay::{Coins, Headers, ReplayCoin};
use dash_dml::testkit::{dmn_with, list_disk_bytes, operator_key, pro_tx};
use dash_dml::{MnList, MnType, OperatorKey};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, DEPLOYMENT_V24, MnList as CoreList,
    Oracle, ResultCode, ThresholdState, Verdict,
};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{
    AddrV1, BlockHash, InputsHash, NetInfo, NetInfoV1, OutPoint, OwnerSig, PayoutShare,
    ProTxVersion, ProUpRegTx, ServiceV1, Transaction, TxHash, TxIn, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use node_l1::dml_checks::CheckCtx;

const V19: &str = "v19@100";
const DIP3: &str = "2:2";
/// The context's tip: the check is of block 101, past V19 and DIP3's enforcement.
const TIP: usize = 100;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, V19)
        .arg(ChainOption::Dip3Params, DIP3)
        .build()
        .expect("regtest parameters")
}

/// A regtest chain of `n` headers from height 0 (only linkage matters to the context).
fn regtest_headers(oracle: &Oracle, n: usize) -> Vec<[u8; 80]> {
    let mut out = Vec::new();
    let mut prev = [0u8; 32];
    for i in 0..n {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&4i32.to_le_bytes());
        h[4..36].copy_from_slice(&prev);
        h[68..72].copy_from_slice(&(1_417_713_337 + 600 * i as u32).to_le_bytes());
        h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
        prev = oracle.header_decode(&h).expect("a header").hash;
        out.push(h);
    }
    out
}

fn p2pkh(hash: [u8; 20]) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend(hash);
    script.extend([0x88, 0xac]);
    script
}

const COLLATERAL: [u8; 32] = [0x43; 32];

/// The ptx's operator key, basic.
fn ptx_key() -> OperatorKey {
    operator_key(7, BlsSchemeKind::Basic)
}

/// The D17 corpus (dash-dml's L0 data), for its class inputs.
const D17: &str = include_str!("../../dash-dml/src/specialtx/data/d17-lazy-eq.txt");

/// The input of the corpus class `name`.
fn corpus_input(name: &str) -> [u8; 48] {
    let prefix = format!("class {name} ");
    let line = D17.lines().find_map(|l| l.strip_prefix(prefix.as_str()));
    let hex = line.unwrap_or_else(|| panic!("no class {name}")).split(' ').next().expect("hex");
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect();
    bytes.try_into().expect("48 bytes")
}

/// `key`'s bytes under `scheme`, from its decoded value (a raw key would discard its bytes).
fn encoded(key: &OperatorKey, scheme: BlsSchemeKind) -> [u8; 48] {
    BlsLazyPk::from_value(key.get(), scheme).to_bytes()
}

/// A key as a list's disk bytes leave it: raw under `scheme`, never read.
fn raw(bytes: [u8; 48], scheme: BlsSchemeKind) -> OperatorKey {
    BlsLazyPk::from_bytes(&bytes, BlsSchemeSource::Version(scheme))
}

/// Another masternode's operator key: its state version and the key under that version's scheme.
type Holder = Option<(i32, OperatorKey)>;

/// Masternode 1 at state `version` with `key` and a legacy address on port 22, its collateral
/// `(COLLATERAL, 0)`, and masternode 2 if it `holder`s a key, as the disk bytes of the list at
/// `tip`.
fn list_bytes(tip: BlockHash, version: i32, key: OperatorKey, holder: Holder) -> Vec<u8> {
    let mut ip = [0u8; 16];
    ip[10..12].copy_from_slice(&[0xff, 0xff]);
    ip[12..].copy_from_slice(&[1, 2, 3, 4]);
    let dmn = dmn_with(1, MnType::Regular, |d, c| {
        d.collateral = OutPoint {
            hash: TxHash::from(COLLATERAL),
            index: 0,
        };
        c.version = version;
        c.pub_key_operator = key;
        c.net_info = NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(ip),
            port: 22,
        }))
        .into();
    });
    let mut list = MnList::new(tip, TIP as i32, 2);
    list.try_add(dmn, true).expect("a masternode");
    if let Some((version, key)) = holder {
        let other = dmn_with(2, MnType::Regular, |d, c| {
            d.collateral = OutPoint {
                hash: TxHash::from([0x45; 32]),
                index: 0,
            };
            c.version = version;
            c.pub_key_operator = key;
        });
        list.try_add(other, true).expect("masternode 2");
    }
    list_disk_bytes(&list)
}

/// The ProUpRegTx raising masternode 1 to ExtAddr with operator key `key`.
fn up_reg(key: OperatorKey) -> Tx {
    let inputs = vec![TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0x34; 32]),
            index: 0,
        },
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }];
    let p = ProUpRegTx {
        version: ProTxVersion::ExtAddr,
        pro_tx_hash: TxHash::from(*pro_tx(1).as_bytes()),
        mode: 0,
        pub_key_operator: key,
        key_id_voting: [0x41; 20].into(),
        script_payout: Vec::new().into(),
        payouts: vec![PayoutShare {
            script_payout: p2pkh([0x42; 20]).into(),
            reward: 10_000,
        }],
        inputs_hash: InputsHash::from_lendian(inputs_hash_of(&inputs)),
        vch_sig: OwnerSig::decode(&mut &[[65].as_slice(), &[0; 65]].concat()[..])
            .expect("a 65-byte signature"),
    };
    let mut extra_payload = Vec::new();
    p.encode(&mut extra_payload);
    Tx::new(Transaction {
        version: 3,
        tx_type: TxType::ProviderUpdateRegistrar,
        inputs,
        outputs: Vec::new(),
        lock_time: 0,
        extra_payload,
    })
}

/// Masternode 1's collateral, as the oracle context and the replay view take it.
fn collateral() -> (ContextCoin, ReplayCoin) {
    let ours = ReplayCoin {
        txid: COLLATERAL,
        vout: 0,
        height: 50,
        coinbase: false,
        amount: 1000 * COIN,
        script_pubkey: p2pkh([0x44; 20]),
    };
    let core = ContextCoin {
        txid: ours.txid,
        vout: ours.vout,
        height: ours.height,
        coinbase: ours.coinbase,
        amount: ours.amount,
        script_pubkey: ours.script_pubkey.clone(),
    };
    (core, ours)
}

const BAD_SPECIAL: i32 = TxValidationResult::BadSpecial as i32;

/// A rejection's class and reason.
type Reject = (i32, String);

fn verdict(e: &TxValidationError) -> Reject {
    (e.result() as i32, e.reject_reason().to_string())
}

/// One cell: the ptx key, masternode 1's state version and key bytes, masternode 2's key.
struct Cell {
    ptx: OperatorKey,
    version: i32,
    state: [u8; 48],
    holder: Holder,
}

/// Core's verdict and ours on [`up_reg`] for `cell`.
fn verdicts(
    oracle: &Oracle,
    chain: &Chain,
    headers: &[[u8; 80]],
    cell: &Cell,
) -> (Option<Reject>, Option<Reject>) {
    let tx = up_reg(cell.ptx.clone());
    let mut raw_tx = Vec::new();
    tx.raw().encode(&mut raw_tx);
    let tip = BlockHash::from(oracle.header_decode(&headers[TIP]).expect("a header").hash);
    let key = raw(cell.state, scheme_of(cell.version));
    let list = list_bytes(tip, cell.version, key, cell.holder.clone());
    let (core_coin, our_coin) = collateral();
    let ctx = Context::new(
        0,
        headers.to_vec(),
        false,
        vec![(DEPLOYMENT_V24, ThresholdState::Active)],
        1_500_000_000,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![core_coin]),
        CoreList::Supplied(list.clone()),
        vec![],
    );
    let core = match oracle.check_special_tx(chain, &raw_tx, &ctx, false).expect("a verdict") {
        Verdict::Accept => None,
        Verdict::Reject(r) => {
            let ResultCode::Tx(class) = r.result else {
                panic!("a transaction result: {r:?}");
            };
            Some((class, r.reason))
        }
    };

    let params = regtest();
    let our_headers = Headers::new(0, headers).expect("our headers");
    let coins = Coins::new(&[our_coin], None);
    let our_list = core_disk::decode_list_s3(&mut &list[..]).expect("our list");
    let rules = SpecialTxRules {
        v24: true,
        evo_shares: false,
    };
    let cx = CheckCtx::new(&params, &our_headers, &coins, rules, &our_list);
    assert_eq!(cx.height, TIP as i32 + 1);
    let ours = cx.check_special_tx(&tx).err().map(|e| verdict(&e));
    (core, ours)
}

/// The scheme of a state at `version`'s operator key.
fn scheme_of(version: i32) -> BlsSchemeKind {
    if version == 1 {
        BlsSchemeKind::Legacy
    } else {
        BlsSchemeKind::Basic
    }
}

/// D17's oracle half: per constructible cell, Core's verdict, ours, and the one Core's
/// `operator==` (`bls.h:514-535`) and step 5a's probe give.
#[test]
fn key_changed_through_pro_up_reg_matches_core_d17() {
    use BlsSchemeKind::{Basic, Legacy};
    let key = ptx_key();
    let basic = key.to_bytes();
    let legacy = encoded(&key, Legacy);
    assert_ne!(basic, legacy);
    let other_key = operator_key(8, Basic);
    let other = other_key.to_bytes();
    let other_legacy = encoded(&other_key, Legacy);
    let undecodable = [0xff; 48];
    assert!(!raw(undecodable, Basic).get().is_valid());
    assert!(!raw(undecodable, Legacy).get().is_valid());
    // Bytes canonical under both schemes, at two points (a key that cannot fall back to
    // "undecodable").
    let both = corpus_input("sub_100");
    let (as_legacy, as_basic) = (raw(both, Legacy).get(), raw(both, Basic).get());
    assert!(as_legacy.is_valid() && as_basic.is_valid());
    assert_ne!(as_legacy.to_bytes(Basic), as_basic.to_bytes(Basic));
    let holder = |version: i32, bytes| Some((version, raw(bytes, scheme_of(version))));

    let migration = Some(BAD_PROTX_NETINFO_VERSION);
    let dup_key = Some(BAD_PROTX_DUP_KEY);
    // (ptx key, state version, state key bytes, masternode 2, expected reason, cell)
    let cells = [
        (key.clone(), 2, basic, None, migration, "same bytes, same scheme: the buffers compare"),
        (key.clone(), 1, legacy, None, migration, "same point, legacy state: Get() == Get()"),
        (key.clone(), 2, other, None, None, "another key"),
        (key.clone(), 1, other_legacy, None, None, "another key under the other scheme"),
        (
            key.clone(),
            1,
            basic,
            None,
            None,
            "the basic bytes read as legacy: another point or none",
        ),
        (
            key.clone(),
            2,
            undecodable,
            None,
            None,
            "undecodable basic: the failed Get() is not the ptx key",
        ),
        (key.clone(), 1, undecodable, None, None, "undecodable legacy"),
        (key.clone(), 2, [0; 48], None, None, "zero bytes, basic"),
        (key.clone(), 1, [0; 48], None, None, "zero bytes, legacy"),
        (raw(both, Basic), 1, both, None, None, "bytes canonical in both schemes, at two points"),
        (raw(both, Basic), 2, both, None, migration, "those bytes, same scheme"),
        (
            key.clone(),
            1,
            legacy,
            holder(2, basic),
            dup_key,
            "migrating, masternode 2 holds the key as basic",
        ),
        (
            key.clone(),
            2,
            basic,
            holder(1, legacy),
            migration,
            "neither changed nor migrating: no probe",
        ),
        (
            key.clone(),
            2,
            other,
            holder(1, legacy),
            dup_key,
            "changed, masternode 2 holds it as legacy",
        ),
        (
            key.clone(),
            2,
            other,
            holder(2, basic),
            dup_key,
            "changed, masternode 2 holds it as basic",
        ),
    ];
    for oracle in Oracle::all() {
        let chain = Chain::new(
            "regtest",
            &[&format!("-testactivationheight={V19}"), &format!("-dip3params={DIP3}")],
        );
        let headers = regtest_headers(&oracle, TIP + 1);
        for (ptx, version, state, holder, reason, cell) in cells.clone() {
            let input = Cell {
                ptx,
                version,
                state,
                holder,
            };
            let (core, ours) = verdicts(&oracle, &chain, &headers, &input);
            assert_eq!(ours, core, "{cell}");
            let expected = reason.map(|r| (BAD_SPECIAL, r.to_owned()));
            assert_eq!(core, expected, "{cell}");
        }
    }
}

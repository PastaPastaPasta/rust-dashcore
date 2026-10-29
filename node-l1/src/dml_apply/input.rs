//! `diff_dml_apply`'s input: a crafted case of O2a-EV (`dml-crafted-v24.0.json`) and a block of
//! special transactions generated against its list.
//!
//! Layout: byte 0 picks the case; byte 1 bit 0 makes the coins view's best block null (external
//! collateral then goes unchecked, so a ProRegTx can register on a fresh outpoint). The rest is
//! up to [`MAX_TXS`] operations, each a kind byte and its arguments (missing bytes read as 0):
//!
//! | kind mod 10 | transaction |
//! |---|---|
//! | 0 | ProUpServTx from a template: target, version (one the chain's checks accept), type, service, platform fields |
//! | 1 | ProUpRevTx: target, version, reason |
//! | 2 | ProUpRegTx: target, version, operator key, voting key, payout |
//! | 3 | ProRegTx from a template: collateral (a listed one, the tx's own, or fresh), keys, service, type |
//! | 4, 5 | a spend of a listed collateral, as an ordinary transaction (4) or a ProDisTx type (5) |
//! | 6 | QcTx from a template, naming a header of the window, with members the context supplies (but one in 64) |
//! | 7 | raw: a provider, share or commitment type and up to 255 payload bytes |
//! | 8 | ProUpShareTx: target, share index, reward script |
//! | 9 | ProUpSharedRegTx: target, operator key, voting key |
//!
//! Targets, keys, services and platform ids come from the case's list (so duplicates and
//! replacements are reachable), a few fresh values, and the transactions earlier in the block.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use dash_consensus::primitives::{TxRef, decode_block, decode_header, decode_tx};
use dash_dml::codec::{ContextFreeNetInfo, core_disk, decode_canonical_list};
use dash_dml::testkit::replay::testnet;
use dash_dml::testkit::{dmn_bytes, list_disk_bytes, operator_key};
use dash_dml::{Dmn, MnList, OperatorKey};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, MnList as CoreList, QuorumMembers,
    ThresholdState,
};
use dash_params::{ChainParams, ChainParamsBuilder, LlmqParams, NetworkKind, Release};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{
    COMPACT_SIG_LEN, CollateralShare, CompactSig, FinalCommitment, MnType, NetInfo, OutPoint,
    PlatformNodeId, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
    QuorumHash, RevocationReason, SpecialPayload, Transaction, TxHash, TxIn, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde::Deserialize;

use super::DEPLOYMENT_V24;
use crate::dml::{extended, legacy, payout, service, spread, twenty, unhex};

/// The crafted fixture (`dash-oracle-sys/tests/data/PROVENANCE.md`).
const CRAFTED: &str = include_str!("../../../dash-oracle-sys/tests/data/dml-crafted-v24.0.json");

/// A block of one shared ProReg on a crafted case and the list Core builds from it
/// (`examples/dml_shared_case.rs`, EV2b D23).
const SHARED_CASE: &str = include_str!("shared-case.json");
/// [`testnet_case`]'s fixture.
const TESTNET_7300: &str = include_str!("../../tests/data/ev2b-testnet-7300.json");

/// The most transactions one input adds after the coinbase.
pub const MAX_TXS: usize = 16;

#[derive(Deserialize)]
struct Fixture {
    chains: BTreeMap<String, String>,
    cases: Vec<FixtureCase>,
}

#[derive(Deserialize)]
struct FixtureCase {
    name: String,
    v24: bool,
    context: FixtureContext,
    block: String,
}

#[derive(Deserialize)]
struct FixtureContext {
    tip: usize,
    list: String,
    coins: Vec<FixtureCoin>,
    deployments: Vec<(u8, u8)>,
    adjusted_time: i64,
    bls_legacy: bool,
}

#[derive(Deserialize)]
struct SharedCaseFixture {
    /// The crafted case the block extends, with V24 forced on.
    base: String,
    block: String,
    /// Core's list after the block (`oracle_dml_apply`, the evodb encoding).
    list: String,
}

#[derive(Deserialize)]
struct FixtureCoin {
    /// Big-endian hex.
    txid: String,
    vout: u32,
    height: u32,
    coinbase: bool,
    amount: i64,
    script: String,
}

/// A fixture's hex; a malformed fixture panics.
#[allow(clippy::expect_used)] // a fixture compiled into the crate
fn hex(s: &str) -> Vec<u8> {
    unhex(s).expect("fixture hex")
}

#[allow(clippy::expect_used)] // a fixture compiled into the crate
fn context_coin(c: &FixtureCoin) -> ContextCoin {
    let mut txid: [u8; 32] = hex(&c.txid).try_into().expect("a 32-byte txid");
    txid.reverse();
    ContextCoin {
        txid,
        vout: c.vout,
        height: c.height,
        coinbase: c.coinbase,
        amount: c.amount,
        script_pubkey: hex(&c.script),
    }
}

/// The payloads of one chain's crafted blocks, to mutate.
#[derive(Default)]
pub(super) struct Templates {
    pro_reg: Vec<ProRegTx>,
    pro_up_serv: Vec<ProUpServTx>,
    pro_up_rev: Vec<ProUpRevTx>,
    pub(super) qc: Vec<FinalCommitment>,
}

/// A crafted case, prepared once.
pub struct Case {
    pub name: String,
    /// Whether the chain runs with V24 always active (`-vbparams=v24:-1:999999999999`).
    pub v24: bool,
    pub params: ChainParams,
    pub ctx: Context,
    /// The case's own block.
    pub block: Vec<u8>,
    /// For a case derived from an earlier block (EV2b D23): that block and its context.
    pub prior: Option<Prior>,
    pub(super) list: MnList,
    /// The header hashes of the context, from its first height.
    pub(super) hashes: Vec<[u8; 32]>,
}

impl Case {
    /// The oracle chain of the case.
    pub fn chain(&self) -> Chain {
        chain_for(self.v24)
    }
}

/// The block a derived case's list was built from, and the context Core built it in.
pub struct Prior {
    pub ctx: Context,
    pub block: Vec<u8>,
}

/// The regtest oracle chain, with V24 always active (`-vbparams=v24:-1:999999999999`) or not.
pub fn chain_for(v24: bool) -> Chain {
    if v24 {
        Chain::new("regtest", &["-vbparams=v24:-1:999999999999"])
    } else {
        Chain::regtest()
    }
}

/// Input flags (byte 1) bit: the context's coins are `Unchecked` instead of at the tip.
pub const FLAG_UNCHECKED_COINS: u8 = 1;
/// Input flags (byte 1) bit: V24 active on a pre-V24 case (EV2b D22).
pub const FLAG_FORCE_V24: u8 = 2;

/// One generated input: the case it starts from, whether V24 is active (the case's, or forced on
/// a pre-V24 case), Core's context and the block.
pub struct Input {
    pub case: &'static Case,
    pub v24: bool,
    pub ctx: Context,
    pub block: Vec<u8>,
}

impl Input {
    /// The oracle chain of the input.
    pub fn chain(&self) -> Chain {
        chain_for(self.v24)
    }

    /// The block's transactions after the coinbase.
    pub fn txs(&self) -> Vec<TxRef> {
        decode_block(&mut &self.block[..])
            .map(|b| b.into_value().txs()[1..].to_vec())
            .unwrap_or_default()
    }
}

pub(super) struct Crafted {
    cases: Vec<Case>,
    /// Templates of the default chain (0) and of the V24 chain (1).
    pub(super) templates: [Templates; 2],
}

/// Adds the payloads of a crafted block's transactions (the coinbase aside) to `t`.
#[allow(clippy::expect_used)] // a fixture compiled into the crate
fn collect_templates(block: &[u8], t: &mut Templates) {
    let decoded = decode_block(&mut &block[..]).expect("a crafted block").into_value();
    for tx in decoded.txs().iter().skip(1) {
        if let Some(Ok(d)) = tx.payload()
            && d.is_complete()
        {
            match &d.payload {
                SpecialPayload::ProviderRegister(p) => t.pro_reg.push(p.clone()),
                SpecialPayload::ProviderUpdateService(p) => t.pro_up_serv.push(p.clone()),
                SpecialPayload::ProviderUpdateRevoke(p) => t.pro_up_rev.push(p.clone()),
                SpecialPayload::QuorumCommitment(p) => t.qc.push(p.clone()),
                _ => {}
            }
        }
    }
}

/// The case after `block` on `base` with V24 forced on, whose list is Core's `list` after
/// it (EV2b D23) with the block's hash set, as `ProcessBlock` stores it: its context is
/// `base`'s with the block's header as the new tip, and its own block is the coinbase under a
/// header on that tip (the shim takes only `hashPrevBlock` from it, the build nothing).
#[allow(clippy::expect_used)] // a fixture compiled into the crate
fn derive(base: &Case, block: Vec<u8>, mut list_bytes: Vec<u8>) -> Case {
    let mut prior_ctx = base.ctx.clone();
    force_v24(&mut prior_ctx);
    let (header, txs) = split_block(&block).expect("the shared case's block");
    let header: [u8; 80] = header.try_into().expect("an 80-byte header");
    let hash = *decode_header(&mut &header[..]).expect("header").value().hash().as_bytes();
    let mut next = header;
    next[4..36].copy_from_slice(&hash);
    let own = assemble(&next, &txs[..1]);
    list_bytes[..32].copy_from_slice(&hash);

    let mut ctx = prior_ctx.clone();
    ctx.headers.push(header);
    ctx.mn_list = CoreList::Supplied(list_bytes.clone());
    let mut hashes = base.hashes.clone();
    hashes.push(hash);
    Case {
        name: format!("{} + shared ProReg", base.name),
        v24: true,
        params: base.params.clone(),
        ctx,
        block: own,
        prior: Some(Prior {
            ctx: prior_ctx,
            block,
        }),
        list: core_disk::decode_list_s3(&mut &list_bytes[..]).expect("Core's list"),
        hashes,
    }
}

#[allow(clippy::expect_used)] // a fixture compiled into the crate
pub(super) fn crafted() -> &'static Crafted {
    static CRAFTED_CASES: OnceLock<Crafted> = OnceLock::new();
    CRAFTED_CASES.get_or_init(|| {
        let fixture: Fixture = serde_json::from_str(CRAFTED).expect("the crafted fixture");
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .build()
            .expect("Core's parameters");
        let mut templates = [Templates::default(), Templates::default()];
        let cases = fixture
            .cases
            .into_iter()
            .map(|c| {
                let headers: Vec<[u8; 80]> = hex(&fixture.chains[if c.v24 {
                    "v24"
                } else {
                    "default"
                }])
                .chunks(80)
                .take(c.context.tip + 1)
                .map(|h| h.try_into().expect("80-byte header"))
                .collect();
                let hashes = headers
                    .iter()
                    .map(|h| *decode_header(&mut &h[..]).expect("header").value().hash().as_bytes())
                    .collect();
                let list_bytes = hex(&c.context.list);
                let list = core_disk::decode_list_s3(&mut &list_bytes[..]).expect("a Core list");
                let block = hex(&c.block);
                collect_templates(&block, &mut templates[usize::from(c.v24)]);
                let ctx = Context::new(
                    0,
                    headers,
                    c.context.bls_legacy,
                    c.context
                        .deployments
                        .iter()
                        .filter_map(|&(d, s)| Some((d, ThresholdState::try_from(s).ok()?)))
                        .collect(),
                    c.context.adjusted_time,
                    Checkpoints::Disabled,
                    CoinsView::AtTip(c.context.coins.iter().map(context_coin).collect()),
                    CoreList::Supplied(list_bytes),
                    vec![],
                );
                Case {
                    name: c.name,
                    v24: c.v24,
                    params: params.clone(),
                    ctx,
                    block,
                    prior: None,
                    list,
                    hashes,
                }
            })
            .collect::<Vec<_>>();
        // An empty fixture (`"base": ""`) while `dml_shared_case` regenerates it from the others.
        let shared: SharedCaseFixture =
            serde_json::from_str(SHARED_CASE).expect("shared-case.json");
        let mut cases = cases;
        if !shared.base.is_empty() {
            let base =
                cases.iter().find(|c| c.name == shared.base).expect("the shared case's base");
            cases.push(derive(base, hex(&shared.block), hex(&shared.list)));
        }
        Crafted {
            cases,
            templates,
        }
    })
}

/// The crafted cases, for the L0 replay.
pub fn cases() -> &'static [Case] {
    &crafted().cases
}

/// Testnet's context before its DIP3 enforcement block (`tests/data/ev2b-testnet-7300.json`,
/// from EV2b's O8 replica), with that block: any other block there is `ProcessBlock`'s
/// `bad-dip3-enf-block` (`deterministicmns.cpp` v24:777).
#[allow(clippy::expect_used)] // a fixture compiled into the crate
pub fn testnet_case() -> &'static Case {
    static TESTNET_CASE: OnceLock<Case> = OnceLock::new();
    TESTNET_CASE.get_or_init(|| {
        let data: serde_json::Value =
            serde_json::from_str(TESTNET_7300).expect("the testnet fixture");
        let hex = |v: &serde_json::Value| self::hex(v.as_str().expect("a hex string"));
        let headers: Vec<[u8; 80]> = data["headers"]
            .as_array()
            .expect("headers")
            .iter()
            .map(|h| hex(h).try_into().expect("an 80-byte header"))
            .collect();
        let hashes = headers
            .iter()
            .map(|h| *decode_header(&mut &h[..]).expect("header").value().hash().as_bytes())
            .collect();
        let canonical = hex(&data["list_canonical"]);
        let list = decode_canonical_list(&mut &canonical[..], &ContextFreeNetInfo)
            .expect("testnet's list");
        let ctx = Context::new(
            i32::try_from(data["first_height"].as_i64().expect("first height")).expect("a height"),
            headers,
            true,
            vec![],
            data["adjusted_time"].as_i64().expect("adjusted time"),
            Checkpoints::Disabled,
            CoinsView::AtTip(vec![]),
            CoreList::Supplied(list_disk_bytes(&list)),
            vec![],
        );
        Case {
            name: String::from("testnet DIP3 enforcement block"),
            v24: false,
            params: testnet().clone(),
            ctx,
            block: hex(&data["block"]),
            prior: None,
            list,
            hashes,
        }
    })
}

/// The input's bytes, read front to back; missing bytes read as 0.
pub(super) struct Bytes<'a>(pub(super) &'a [u8]);

impl Bytes<'_> {
    pub(super) fn next(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    pub(super) fn take(&mut self, n: usize) -> Vec<u8> {
        let (taken, rest) = self.0.split_at(n.min(self.0.len()));
        self.0 = rest;
        taken.to_vec()
    }

    pub(super) fn pick<'t, T>(&mut self, items: &'t [T]) -> Option<&'t T> {
        let b = usize::from(self.next());
        items.get(b % items.len().max(1))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Pay-to-pubkey-hash of `key_id`.
pub(super) fn p2pkh(key_id: [u8; 20]) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 20];
    script.extend(key_id);
    script.extend([0x88, 0xac]);
    script
}

/// Values a transaction draws from: the case's list, fresh ones, and earlier transactions.
struct Pools {
    targets: Vec<[u8; 32]>,
    collaterals: Vec<OutPoint>,
    keys: Vec<OperatorKey>,
    key_ids: Vec<[u8; 20]>,
    net_infos: Vec<NetInfo>,
    platform_ids: Vec<[u8; 20]>,
    /// Shared masternodes of the list or registered earlier in the block, which share updates
    /// prefer.
    shared: Vec<[u8; 32]>,
}

impl Pools {
    fn new(list: &MnList) -> Pools {
        let mns: Vec<&Arc<Dmn>> = list.iter().collect();
        let mut pools = Pools {
            targets: mns.iter().map(|d| *d.pro_tx_hash.as_bytes()).collect(),
            collaterals: mns.iter().map(|d| d.collateral).collect(),
            keys: mns.iter().map(|d| d.cold.pub_key_operator.clone()).collect(),
            key_ids: mns
                .iter()
                .flat_map(|d| [*d.cold.key_id_owner.as_bytes(), *d.cold.key_id_voting.as_bytes()])
                .collect(),
            net_infos: mns.iter().map(|d| (*d.cold.net_info).clone()).collect(),
            platform_ids: mns.iter().map(|d| *d.cold.platform_node_id.as_bytes()).collect(),
            shared: mns
                .iter()
                .filter(|d| d.cold.is_shared())
                .map(|d| *d.pro_tx_hash.as_bytes())
                .collect(),
        };
        for n in 1..=3u8 {
            pools.targets.push(spread(n, 0, 40));
            pools.keys.push(operator_key(n + 10, BlsSchemeKind::Basic));
            pools.key_ids.push(twenty(spread(n, 1, 41)));
            pools.net_infos.push(legacy(&service(n, 0x50)));
            pools.platform_ids.push(twenty(spread(n, 2, 42)));
        }
        pools.keys.push(OperatorKey::new(BlsSchemeKind::Basic));
        pools
    }

    /// A network info for a payload of `version`: as drawn, or (from ExtAddr) the extended kind.
    fn net_info(&self, r: &mut Bytes<'_>, version: u16) -> NetInfo {
        let b = r.next();
        let drawn = r.pick(&self.net_infos).cloned().unwrap_or_else(|| legacy(&service(b, 0x51)));
        match (version >= 3, &drawn) {
            (true, NetInfo::Legacy(_)) => extended(service(b, 0x52)),
            (false, NetInfo::Extended(_)) => legacy(&service(b, 0x53)),
            _ => drawn,
        }
    }

    fn key_id(&self, r: &mut Bytes<'_>) -> [u8; 20] {
        r.pick(&self.key_ids).copied().unwrap_or_default()
    }

    /// A share update's target: a shared masternode of the block three times in four, when
    /// there is one, else any target.
    fn share_target(&self, r: &mut Bytes<'_>) -> Option<[u8; 32]> {
        let shared = !self.shared.is_empty() && !r.next().is_multiple_of(4);
        r.pick(if shared {
            &self.shared
        } else {
            &self.targets
        })
        .copied()
    }
}

/// A payload version: the template's, or one Core's checks accept on the chain: 2, and 3 with V24
/// (`IsVersionValid`: V19 is active on both chains, and ExtAddr needs V24). Raw payloads
/// (kind 7) reach the others.
fn version(r: &mut Bytes<'_>, template: u16, v24: bool) -> u16 {
    match (r.next() % 3, v24) {
        (0, _) => template,
        (1, true) => 3,
        _ => 2,
    }
}

fn mn_type(b: u8) -> MnType {
    match b % 8 {
        0..=3 => MnType::Regular,
        4..=6 => MnType::Evo,
        _ => MnType::from_base(u16::from(b)),
    }
}

/// A special transaction: version 3, `inputs`, no outputs, the payload as `extra_payload`.
pub(super) fn special(tx_type: TxType, inputs: Vec<TxIn>, payload: Vec<u8>) -> Vec<u8> {
    encoded(&Transaction {
        version: 3,
        tx_type,
        inputs,
        outputs: vec![],
        lock_time: 0,
        extra_payload: payload,
    })
}

pub(super) fn encoded(payload: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    payload.encode(&mut out);
    out
}

/// A special transaction of `tx_type` carrying `payload`, no inputs.
fn payload_tx(tx_type: TxType, payload: &impl BaseCodec) -> Vec<u8> {
    special(tx_type, vec![], encoded(payload))
}

/// A spend of `outpoint`.
pub(super) fn spend_of(outpoint: OutPoint) -> TxIn {
    TxIn {
        prevout: outpoint,
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }
}

/// The node id and the two ports of a payload of `mn_type` and `version`: an Evo payload has a
/// node id, and ports before ExtAddr.
fn platform_fields(
    r: &mut Bytes<'_>,
    pools: &Pools,
    mn_type: MnType,
    version: u16,
) -> (Option<PlatformNodeId>, Option<u16>, Option<u16>) {
    let evo = mn_type == MnType::Evo;
    let node_id =
        evo.then(|| PlatformNodeId::from(*r.pick(&pools.platform_ids).unwrap_or(&[0; 20])));
    let ports = evo && version < 3;
    let p2p = ports.then(|| u16::from(r.next()) + 26_000);
    let http = ports.then(|| u16::from(r.next()) + 1_400);
    (node_id, p2p, http)
}

fn pro_up_serv(r: &mut Bytes<'_>, t: &Templates, pools: &Pools, v24: bool) -> Option<Vec<u8>> {
    let mut p = r.pick(&t.pro_up_serv)?.clone();
    p.pro_tx_hash = TxHash::from_lendian(*r.pick(&pools.targets)?);
    p.version = version(r, p.version, v24);
    p.mn_type = mn_type(r.next());
    p.net_info = pools.net_info(r, p.version);
    (p.platform_node_id, p.platform_p2p_port, p.platform_http_port) =
        platform_fields(r, pools, p.mn_type, p.version);
    Some(payload_tx(TxType::ProviderUpdateService, &p))
}

fn pro_up_rev(r: &mut Bytes<'_>, t: &Templates, pools: &Pools, v24: bool) -> Option<Vec<u8>> {
    let mut p = r.pick(&t.pro_up_rev)?.clone();
    p.pro_tx_hash = TxHash::from_lendian(*r.pick(&pools.targets)?);
    p.version = version(r, p.version, v24);
    p.reason = RevocationReason::from_base(u16::from(r.next() % 5));
    Some(payload_tx(TxType::ProviderUpdateRevoke, &p))
}

/// A ProUpRegTx, read from its serialization at version 1 or 2 and then edited (no crafted block
/// holds one).
fn pro_up_reg(r: &mut Bytes<'_>, pools: &Pools, v24: bool) -> Option<Vec<u8>> {
    let target = *r.pick(&pools.targets)?;
    let version = version(r, 2, v24);
    let mut bytes = Vec::new();
    bytes.extend(version.min(2).to_le_bytes());
    bytes.extend(target);
    bytes.extend(0u16.to_le_bytes());
    bytes.extend([0u8; 48]);
    bytes.extend(pools.key_id(r));
    let script = p2pkh(pools.key_id(r));
    bytes.push(script.len() as u8);
    bytes.extend(&script);
    bytes.extend([0u8; 32]);
    bytes.push(0);
    let mut p = ProUpRegTx::decode(&mut &bytes[..]).ok()?;
    p.pub_key_operator = r.pick(&pools.keys)?.clone();
    if version >= 3 {
        p.version = version;
        p.payouts = payout(&script).into_iter().collect();
    }
    Some(payload_tx(TxType::ProviderUpdateRegistrar, &p))
}

fn pro_reg(r: &mut Bytes<'_>, t: &Templates, pools: &Pools, v24: bool) -> Option<Vec<u8>> {
    let mut p = r.pick(&t.pro_reg)?.clone();
    let template_version = p.version;
    p.version = version(r, p.version, v24);
    match r.next() % 3 {
        0 => {
            let c = *r.pick(&pools.collaterals)?;
            p.collateral_hash = c.hash;
            p.collateral_index = c.index;
        }
        1 => {
            p.collateral_hash = TxHash::from_lendian([0; 32]);
            p.collateral_index = u32::from(r.next() % 2);
        }
        _ => {
            p.collateral_hash = TxHash::from_lendian(spread(r.next(), r.next(), 43));
            p.collateral_index = 0;
        }
    }
    p.mn_type = mn_type(r.next());
    p.key_id_owner = From::from(pools.key_id(r));
    p.key_id_voting = From::from(pools.key_id(r));
    p.pub_key_operator = r.pick(&pools.keys)?.clone();
    p.net_info = pools.net_info(r, p.version);
    (p.platform_node_id, p.platform_p2p_port, p.platform_http_port) =
        platform_fields(r, pools, p.mn_type, p.version);
    let script = p2pkh(pools.key_id(r));
    match (template_version >= 3, p.version >= 3) {
        (false, true) => {
            p.payouts = payout(&script).into_iter().collect();
            p.script_payout = Vec::new().into();
        }
        (true, false) => {
            p.payouts = vec![];
            p.script_payout = script.into();
        }
        _ => {}
    }
    if p.version >= 3 {
        shares(r, pools, &mut p)?;
    }
    Some(payload_tx(TxType::ProviderRegister, &p))
}

/// Makes an ExtAddr ProRegTx a shared registration, unless `r` says not (one in two): one to
/// three shares whose scripts and owner keys come from the pools, so a share script can pay a
/// voting key (`bad-pro*-payee-reuse`) and an owner key can be taken (`bad-protx-dup-key`).
fn shares(r: &mut Bytes<'_>, pools: &Pools, p: &mut ProRegTx) -> Option<()> {
    if r.next().is_multiple_of(2) {
        return Some(());
    }
    let count = 1 + usize::from(r.next() % 3);
    p.shares = (0..count)
        .map(|_| CollateralShare {
            amount: i64::from(r.next()) * 100_000_000,
            script_refund: p2pkh(pools.key_id(r)).into(),
            script_reward: if r.next().is_multiple_of(2) {
                Vec::new().into()
            } else {
                p2pkh(pools.key_id(r)).into()
            },
            key_id_owner: From::from(pools.key_id(r)),
        })
        .collect();
    let sig = CompactSig::decode(&mut &[0; COMPACT_SIG_LEN][..]).ok()?;
    p.join_sigs = vec![sig; count];
    p.early_period_blocks = u32::from(r.next());
    p.early_penalty = i64::from(r.next());
    Some(())
}

/// The special types the raw operation draws: the provider types the build reads (ProDisTx is a
/// spend), the commitment and the two share types.
const RAW_TYPES: [u16; 7] = [1, 2, 3, 4, 6, 11, 12];

/// A ProUpShareTx (version 1, as Core writes it), read from its serialization and then edited.
/// No crafted list holds a shared masternode; it reaches one registered earlier in the block
/// ([`shares`], EV2b D13).
fn pro_up_share(r: &mut Bytes<'_>, pools: &Pools) -> Option<Vec<u8>> {
    let target = pools.share_target(r)?;
    let script = p2pkh(pools.key_id(r));
    let mut bytes = Vec::new();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(target);
    bytes.extend(u16::from(r.next() % 4).to_le_bytes());
    bytes.push(script.len() as u8);
    bytes.extend(&script);
    bytes.extend([0u8; 32]);
    bytes.push(0);
    let p = ProUpShareTx::decode(&mut &bytes[..]).ok()?;
    Some(payload_tx(TxType::ProviderUpdateShare, &p))
}

/// A ProUpSharedRegTx (version 1), as [`pro_up_share`].
fn pro_up_shared_reg(r: &mut Bytes<'_>, pools: &Pools) -> Option<Vec<u8>> {
    let target = pools.share_target(r)?;
    let mut bytes = Vec::new();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(target);
    bytes.extend([0u8; 48]);
    bytes.extend(pools.key_id(r));
    bytes.extend([0u8; 32]);
    bytes.push(0);
    let mut p = ProUpSharedRegTx::decode(&mut &bytes[..]).ok()?;
    p.pub_key_operator = r.pick(&pools.keys)?.clone();
    Some(payload_tx(TxType::ProviderUpdateSharedRegistrar, &p))
}

/// Adds the quorum `hash` of `llmq` to the context's quorums with members drawn from the case's
/// list, unless `r` says not (one in 64) or the quorum is already there.
fn supply_members(
    r: &mut Bytes<'_>,
    case: &Case,
    llmq: &LlmqParams,
    hash: [u8; 32],
    quorums: &mut Vec<QuorumMembers>,
) -> Option<()> {
    if r.next().is_multiple_of(64) {
        return Some(());
    }
    let mns: Vec<&Arc<Dmn>> = case.list.iter().collect();
    let start = usize::from(r.next());
    let count = usize::try_from(llmq.size).ok()?.min(mns.len());
    let members = (0..count).map(|i| dmn_bytes(mns[(start + i) % mns.len()])).collect();
    let llmq_type = llmq.llmq_type.to_base();
    if !quorums.iter().any(|q| q.llmq_type == llmq_type && q.quorum_hash == hash) {
        quorums.push(QuorumMembers {
            llmq_type,
            quorum_hash: hash,
            members,
        });
    }
    Some(())
}

/// A QcTx naming a quorum base in the window, with its members supplied unless `r` says not.
fn qc(
    r: &mut Bytes<'_>,
    t: &Templates,
    case: &Case,
    quorums: &mut Vec<QuorumMembers>,
) -> Option<Vec<u8>> {
    let mut p = r.pick(&t.qc)?.clone();
    let llmq = r.pick(&case.params.consensus.llmqs)?;
    let interval = u32::try_from(llmq.dkg_interval).ok()?.max(1);
    let tip = u32::try_from(case.hashes.len()).ok()?.checked_sub(1)?;
    let back = u32::from(r.next() % 4);
    let base = (tip / interval).checked_sub(back)? * interval;
    let index = if llmq.use_rotation {
        u32::from(r.next()) % 4
    } else {
        0
    };
    let height = base + u32::from(r.next()) % interval;
    let quorum_base = usize::try_from(base + index).ok()?;
    let hash = if r.next().is_multiple_of(8) {
        spread(r.next(), 0, 44)
    } else {
        *case.hashes.get(quorum_base)?
    };
    let c = &mut p.commitment;
    c.llmq_type = llmq.llmq_type;
    c.quorum_hash = QuorumHash::from_lendian(hash);
    c.quorum_index = matches!(c.version, 2 | 4).then_some(index as i16);
    let size = u64::try_from(llmq.size).ok()?;
    let bytes = usize::try_from(size.div_ceil(8)).ok()?;
    c.valid_members.num_bits = size;
    c.valid_members.data = r.take(bytes);
    c.signers.num_bits = size;
    c.signers.data = vec![0xff; bytes];
    supply_members(r, case, llmq, hash, quorums)?;
    // The payload's `nHeight` follows its version (`FinalCommitment::encode`).
    let mut payload = encoded(&p);
    payload[2..6].copy_from_slice(&height.to_le_bytes());
    Some(special(TxType::QuorumCommitment, vec![], payload))
}

/// `ctx` with V24 active for the block after its tip (EV2b D22), as `-vbparams=v24:-1:…` makes
/// it on the V24 chain.
pub(super) fn force_v24(ctx: &mut Context) {
    ctx.deployments.retain(|&(d, _)| d != DEPLOYMENT_V24);
    ctx.deployments.push((DEPLOYMENT_V24, ThresholdState::Active));
}

/// The case, context and block an input describes; `None` for an input too short to pick one.
pub fn generate(input: &[u8]) -> Option<Input> {
    let crafted = crafted();
    let mut r = Bytes(input);
    if r.is_empty() {
        return None;
    }
    let case = r.pick(&crafted.cases)?;
    let t = &crafted.templates[usize::from(case.v24)];
    let mut ctx = case.ctx.clone();
    let flags = r.next();
    if flags & FLAG_UNCHECKED_COINS != 0 {
        let CoinsView::AtTip(coins) = &ctx.coins else {
            return None;
        };
        ctx.coins = CoinsView::Unchecked(coins.clone());
    }
    // V24 on a pre-V24 case: its v1/v2 states reach `SetStateVersion`'s migrations, and V24
    // payloads (ExtAddr, shared registrations) reach a populated list (EV2b D22).
    let forced = !case.v24 && flags & FLAG_FORCE_V24 != 0;
    if forced {
        force_v24(&mut ctx);
    }
    let v24 = case.v24 || forced;
    let mut pools = Pools::new(&case.list);
    let mut txs: Vec<Vec<u8>> = Vec::new();
    while !r.is_empty() && txs.len() < MAX_TXS {
        let tx = match r.next() % 10 {
            0 => pro_up_serv(&mut r, t, &pools, v24),
            1 => pro_up_rev(&mut r, t, &pools, v24),
            2 => pro_up_reg(&mut r, &pools, v24),
            3 => pro_reg(&mut r, t, &pools, v24),
            k @ (4 | 5) => {
                let Some(&spent) = r.pick(&pools.collaterals) else {
                    continue;
                };
                let tx_type = if k == 4 {
                    TxType::Spend
                } else {
                    TxType::ProviderDissolve
                };
                let len = usize::from(r.next() % 8);
                Some(special(tx_type, vec![spend_of(spent)], r.take(len)))
            }
            6 => qc(&mut r, t, case, &mut ctx.quorums),
            8 => pro_up_share(&mut r, &pools),
            9 => pro_up_shared_reg(&mut r, &pools),
            _ => {
                let tx_type = TxType::from_base(*r.pick(&RAW_TYPES)?);
                let len = usize::from(r.next());
                Some(special(tx_type, vec![], r.take(len)))
            }
        };
        let Some(tx) = tx else {
            continue;
        };
        // A later provider update can name this transaction, and a later spend or update can
        // reach a masternode it registers.
        let decoded = decode_tx(&mut &tx[..]).ok()?.into_value();
        let txid = decoded.txid();
        pools.targets.push(*txid.as_bytes());
        if let Some(Ok(d)) = decoded.payload()
            && let SpecialPayload::ProviderRegister(p) = &d.payload
        {
            let hash = if p.collateral_hash.is_null() {
                txid
            } else {
                p.collateral_hash
            };
            pools.collaterals.push(OutPoint {
                hash,
                index: p.collateral_index,
            });
            pools.key_ids.push(*p.key_id_voting.as_bytes());
            if !p.shares.is_empty() {
                pools.shared.push(*txid.as_bytes());
            }
        }
        txs.push(tx);
    }
    let (header, block_txs) = split_block(&case.block)?;
    let mut parts = vec![*block_txs.first()?];
    parts.extend(txs.iter().map(Vec::as_slice));
    Some(Input {
        case,
        v24,
        ctx,
        block: assemble(header, &parts),
    })
}

/// A block's 80-byte header and its transactions' bytes: the header is followed by the
/// transaction count as one byte (below 253, a CompactSize of one byte) and the transactions.
pub fn split_block(block: &[u8]) -> Option<(&[u8], Vec<&[u8]>)> {
    let header = block.get(..80)?;
    let count = *block.get(80)?;
    if count >= 253 {
        return None;
    }
    let mut rest = block.get(81..)?;
    let mut txs = Vec::new();
    for _ in 0..count {
        let start = rest;
        decode_tx(&mut rest).ok()?;
        txs.push(&start[..start.len() - rest.len()]);
    }
    Some((header, txs))
}

/// The block with only its coinbase and the transaction at `keep`.
pub fn with_one_tx(block: &[u8], keep: usize) -> Option<Vec<u8>> {
    let (header, txs) = split_block(block)?;
    Some(assemble(header, &[*txs.first()?, *txs.get(keep)?]))
}

/// The block of `header` and `txs` ([`split_block`]'s layout); fewer than 253 transactions.
pub fn assemble(header: &[u8], txs: &[&[u8]]) -> Vec<u8> {
    let mut out = header.to_vec();
    out.push(txs.len() as u8);
    out.extend(txs.iter().copied().flatten());
    out
}

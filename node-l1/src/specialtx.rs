//! EV1-3's L1 target (EV1 §6.2): `diff_check_special_tx`, Core's `CheckSpecialTx` (the oracle's
//! transaction entry, `check_sigs` off) against dash-dml's `check_special_tx_inner` through
//! [`CheckCtx`], on generated registrations, registrar updates and shared-masternode
//! transactions (types 1, 3 and 10 to 12) over a synthetic regtest tip.
//!
//! The world: regtest with V19 at height 100 and DIP3 enforced from 2, the tip at 100. Its list
//! holds four masternodes, each with its collateral in the coins view: [`REG`] (regular, version
//! 2), [`EVO`] (Evo, version 2), [`OLD`] (version 1, a legacy key and a legacy address on port
//! 22, which ExtNetInfo refuses) and [`SHARED`] (version 3, shared, registered at 90). The view
//! also holds an external collateral per type for registrations. The flags byte turns on V24 and
//! EVO_SHARES, puts the shared masternode in its early period and gives it three shares instead
//! of two.
//!
//! A case starts from a seed, a transaction of one type both sides accept, applies up to three
//! mutations from the type's [`catalogue`], each labelled with the EV1 §3 step it violates
//! (D19), and then varies the inputs and outputs no check of the type reads (the EV1-2 Opus-h
//! n-1 nit). Compared: Core's verdict and ours, the class and the reason.
//!
//! Step labels are `table:step` with the step numbers of EV1 §3: `1t` and `1c` for
//! `CProRegTx::IsTriviallyValid` and `CheckProRegTx` (§3.3), `3t` and `3c` (§3.5), `10t`, `10c`
//! and `10l` for ProDisTx's `IsTriviallyValid`, `CheckProDisTx` and `CheckProDisTxForList`
//! (§3.7), `11t`/`11c` (§3.8), `12t`/`12c` (§3.9), `pl` and `sl` for the payout and share lists
//! (§3.14), `nt` for `IsNetInfoTriviallyValid` (numbered by its three rejects) and `nf` for
//! `CheckProviderNetworkFields` (§3.15). A step that loops over a list takes an entity, the
//! payout or share index. Steps no wire transaction reaches here are not in the catalogue;
//! `contrib/node/ev1-pairs.toml` lists them with Core's reason.
//!
//! The context holds the last two headers only: the checks read the tip's height and hash.
//!
//! Input layout: byte 0 the flags, byte 1 the type, byte 2 the seed, byte 3 the variation,
//! byte 4 the mutation count (mod 4), then a (pick, argument) byte pair per mutation.

use std::sync::{Arc, OnceLock};

use dash_consensus::decode_header;
use dash_consensus::primitives::Tx;
use dash_consensus::subsidy::COIN;
use dash_dml::codec::core_disk;
use dash_dml::specialtx::SpecialTxRules;
use dash_dml::specialtx::reasons::*;
use dash_dml::specialtx::sig::inputs_hash_of;
use dash_dml::testkit::replay::{Coins, Headers, ReplayCoin};
use dash_dml::testkit::{dmn_with, fresh, list_disk_bytes, operator_key, pro_tx};
use dash_dml::{Dmn, MnCold, MnList, OperatorKey};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, DEPLOYMENT_EVO_SHARES, DEPLOYMENT_V24,
    MnList as CoreList, Oracle, Pin, ResultCode, ThresholdState, Verdict,
};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{
    AddrV1, AddrV2, BlockHash, COMPACT_SIG_LEN, CollateralShare, CompactSig, InputsHash, MnType,
    NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, OutPoint, PayoutShare, PlatformNodeId,
    ProDisTx, ProRegTx, ProUpRegTx, ProUpShareTx, ProUpSharedRegTx, ServiceV1, ServiceV2,
    Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use crate::dml::legacy;
use crate::dml_checks::CheckCtx;
use crate::inputs::oracle_failure;
use crate::{Compared, Divergence, Outcome, check, divergence};

/// `-testactivationheight` and `-dip3params` of the world's chain.
const V19: &str = "v19@100";
const DIP3: &str = "2:2";
/// The context's tip: the checks are of block 101.
pub const TIP: usize = 100;

/// The world's masternodes, by number ([`pro_tx`]`(n)`, internal id `n`).
pub const REG: u8 = 1;
pub const EVO: u8 = 2;
pub const OLD: u8 = 3;
pub const SHARED: u8 = 4;
/// The number of the identities a seed's transaction brings.
const FRESH: u8 = 5;

/// The shared masternode's registration height and early period when the flags put block 101
/// in it, and when not.
const SHARED_REGISTERED: i32 = 90;
const EARLY_PERIOD: u32 = 100;
const LATE_PERIOD: u32 = 5;
/// Its early penalty: odd, so two equal non-actor shares leave a sub-duff remainder.
const PENALTY: i64 = 10 * COIN + 1;
/// The fee a seed dissolution leaves.
const DIS_FEE: i64 = 1_000;
/// `CProDisTx::MAX_FEE`.
const MAX_FEE: i64 = 1_000_000;

/// Flag bits (byte 0).
pub const FLAG_V24: u8 = 1;
pub const FLAG_EVO_SHARES: u8 = 2;
pub const FLAG_EARLY: u8 = 4;
pub const FLAG_THREE_SHARES: u8 = 8;

/// The types the target generates, in the order byte 1 picks them.
pub const TYPES: [TxType; 5] = [
    TxType::ProviderRegister,
    TxType::ProviderUpdateRegistrar,
    TxType::ProviderDissolve,
    TxType::ProviderUpdateShare,
    TxType::ProviderUpdateSharedRegistrar,
];

/// The most mutations a case applies.
pub const MAX_MUTATIONS: usize = 3;

/// An identity of masternode or role `n`: key ids and payout keys `[n << 4 | role; 20]`.
fn id(n: u8, role: u8) -> [u8; 20] {
    [n << 4 | role; 20]
}

/// The identities of share `i` of the shared masternode (`base` 0xc0) or of a shared
/// registration (`base` 0xd0): role 0 the refund key, 1 the reward key, 2 the owner key.
fn share_id(base: u8, i: usize, role: u8) -> [u8; 20] {
    [base | (i as u8) << 2 | role; 20]
}

/// Pay-to-pubkey-hash of `key_id`.
pub fn p2pkh(key_id: [u8; 20]) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 20];
    script.extend(key_id);
    script.extend([0x88, 0xac]);
    script
}

/// Pay-to-script-hash of `hash`.
fn p2sh(hash: [u8; 20]) -> Vec<u8> {
    let mut script = vec![0xa9, 20];
    script.extend(hash);
    script.push(0x87);
    script
}

/// A script with no destination (`OP_TRUE`).
const NO_DEST: [u8; 1] = [0x51];

/// `SharedCollateralScript()`.
const TEMPLATE: [u8; 7] = dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT;

/// The regtest P2P port, which no list address takes here.
const PORT: u16 = 19_899;

/// Routable service `n` on `port`.
fn service(n: u8, port: u16) -> ServiceV2 {
    ServiceV2 {
        addr: AddrV2::Ipv4([1, 2, 3, n]),
        port,
    }
}

/// An ExtNetInfo of `entries`.
fn ext(entries: Vec<(NIPurpose, Vec<NIEntry>)>) -> NetInfo {
    NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries,
    })
}

/// The network info of a payload of `version` and type `evo` at service `n`: legacy below 3;
/// from 3 ExtNetInfo, with the platform entries for Evo.
fn net_info(n: u8, version: u16, evo: bool) -> NetInfo {
    if version < 3 {
        return legacy(&service(n, PORT));
    }
    let mut entries = vec![(NIPurpose::CoreP2p, vec![NIEntry::Service(service(n, PORT))])];
    if evo {
        entries.push((NIPurpose::PlatformP2p, vec![NIEntry::Service(service(n, 26_656))]));
        entries.push((NIPurpose::PlatformHttps, vec![NIEntry::Service(service(n, 1_443))]));
    }
    ext(entries)
}

/// The operator key of masternode `n` (1 to 4), basic but for [`OLD`]'s.
fn member_key(n: u8) -> OperatorKey {
    static KEYS: OnceLock<Vec<OperatorKey>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| {
        (REG..=SHARED)
            .map(|n| {
                let scheme = if n == OLD {
                    BlsSchemeKind::Legacy
                } else {
                    BlsSchemeKind::Basic
                };
                operator_key(0x80 + n, scheme)
            })
            .collect()
    });
    keys[usize::from(n - REG)].clone()
}

/// Key `n` (1 to 3) a seed brings: never a list member's.
fn fresh_key(n: u8) -> OperatorKey {
    static KEYS: OnceLock<Vec<OperatorKey>> = OnceLock::new();
    let keys = KEYS
        .get_or_init(|| (1..=3).map(|n| operator_key(0x90 + n, BlsSchemeKind::Basic)).collect());
    keys[usize::from(n - 1)].clone()
}

/// `key`'s point under `scheme`, from its decoded value.
fn rekeyed(key: &OperatorKey, scheme: BlsSchemeKind) -> OperatorKey {
    BlsLazyPk::from_value(key.clone().get(), scheme)
}

/// The scheme a key of an object at `version` is read in.
fn scheme_at(version: u16) -> BlsSchemeKind {
    if version < 2 {
        BlsSchemeKind::Legacy
    } else {
        BlsSchemeKind::Basic
    }
}

/// A key whose bytes decode to no point.
fn invalid_key() -> OperatorKey {
    BlsLazyPk::from_bytes(&[0xff; 48], BlsSchemeSource::Version(BlsSchemeKind::Basic))
}

fn compact_sig() -> Option<CompactSig> {
    CompactSig::decode(&mut &[0; COMPACT_SIG_LEN][..]).ok()
}

fn outpoint(hash: [u8; 32], index: u32) -> OutPoint {
    OutPoint {
        hash: TxHash::from(hash),
        index,
    }
}

fn spend(prevout: OutPoint) -> TxIn {
    TxIn {
        prevout,
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }
}

fn out(value: i64, script: Vec<u8>) -> TxOut {
    TxOut {
        value,
        script_pubkey: script.into(),
    }
}

fn collateral_amount(evo: bool) -> i64 {
    if evo {
        4000 * COIN
    } else {
        1000 * COIN
    }
}

/// Masternode `n`'s collateral outpoint.
fn member_collateral(n: u8) -> OutPoint {
    outpoint([0x30 | n; 32], u32::from(n))
}

/// The external collateral of a registration of type `evo`.
fn external_collateral(evo: bool) -> OutPoint {
    if evo {
        outpoint([0x62; 32], 2)
    } else {
        outpoint([0x61; 32], 1)
    }
}

/// The key the external collateral of type `evo` pays.
fn external_key_of(evo: bool) -> [u8; 20] {
    id(
        6,
        if evo {
            4
        } else {
            3
        },
    )
}

/// The key the regular registrations' external collateral pays.
pub fn external_key() -> [u8; 20] {
    external_key_of(false)
}

fn coin(at: OutPoint, amount: i64, script: Vec<u8>) -> ReplayCoin {
    ReplayCoin {
        txid: *at.hash.as_bytes(),
        vout: at.index,
        height: 50,
        coinbase: false,
        amount,
        script_pubkey: script,
    }
}

/// The shared masternode's share amounts, in duffs.
fn member_share_amounts(three: bool) -> Vec<i64> {
    if three {
        vec![400 * COIN, 300 * COIN, 300 * COIN]
    } else {
        vec![600 * COIN, 400 * COIN]
    }
}

/// Shares of `amounts` with identities from `base` ([`share_id`]); odd shares name a reward
/// script, even ones leave it to the refund script.
fn shares(base: u8, amounts: &[i64]) -> Vec<CollateralShare> {
    amounts
        .iter()
        .enumerate()
        .map(|(i, &amount)| CollateralShare {
            amount,
            script_refund: p2pkh(share_id(base, i, 0)).into(),
            script_reward: if i % 2 == 1 {
                p2pkh(share_id(base, i, 1)).into()
            } else {
                Vec::new().into()
            },
            key_id_owner: share_id(base, i, 2).into(),
        })
        .collect()
}

/// What the checks read beside the transaction: the flags, the tip's list and the coins.
#[derive(Clone)]
pub struct World {
    pub v24: bool,
    pub evo_shares: bool,
    pub early: bool,
    pub three_shares: bool,
    members: Vec<Arc<Dmn>>,
    coins: Vec<ReplayCoin>,
}

impl World {
    /// The world of the flags byte.
    pub fn new(flags: u8) -> World {
        let v24 = flags & FLAG_V24 != 0;
        let three_shares = flags & FLAG_THREE_SHARES != 0;
        let early = flags & FLAG_EARLY != 0;
        let member = |n: u8, mn_type: dash_dml::MnType, f: &dyn Fn(&mut MnCold)| {
            dmn_with(n, mn_type, |d, c| {
                d.collateral = member_collateral(n);
                c.registered_height = 50;
                c.pub_key_operator = member_key(n);
                c.key_id_owner = id(n, 1).into();
                c.key_id_voting = id(n, 2).into();
                c.script_payout = p2pkh(id(n, 4));
                c.version = 2;
                c.net_info = Arc::new(legacy(&service(n, PORT)));
                f(c);
            })
        };
        let reg = member(REG, dash_dml::MnType::Regular, &|_| {});
        let evo = member(EVO, dash_dml::MnType::Evo, &|c| {
            c.platform_node_id = PlatformNodeId::from(id(EVO, 7));
            c.platform_p2p_port = 26_656;
            c.platform_http_port = 1_443;
        });
        let old = member(OLD, dash_dml::MnType::Regular, &|c| {
            c.version = 1;
            let mut ip = [0u8; 16];
            ip[10..12].copy_from_slice(&[0xff, 0xff]);
            ip[12..].copy_from_slice(&[1, 2, 3, OLD]);
            c.net_info = Arc::new(NetInfo::Legacy(NetInfoV1(ServiceV1 {
                addr: AddrV1(ip),
                port: 22,
            })));
        });
        let amounts = member_share_amounts(three_shares);
        let shared = member(SHARED, dash_dml::MnType::Regular, &|c| {
            c.version = 3;
            c.registered_height = SHARED_REGISTERED;
            c.key_id_owner = [0; 20].into();
            c.script_payout = Vec::new();
            c.net_info = Arc::new(net_info(SHARED, 3, false));
            c.shares = shares(0xc0, &amounts);
            c.early_period_blocks = if early {
                EARLY_PERIOD
            } else {
                LATE_PERIOD
            };
            c.early_penalty = PENALTY;
        });
        let coins = vec![
            coin(member_collateral(REG), collateral_amount(false), p2pkh(id(REG, 3))),
            coin(member_collateral(EVO), collateral_amount(true), p2pkh(id(EVO, 3))),
            coin(member_collateral(OLD), collateral_amount(false), p2pkh(id(OLD, 3))),
            coin(member_collateral(SHARED), collateral_amount(false), TEMPLATE.to_vec()),
            coin(
                external_collateral(false),
                collateral_amount(false),
                p2pkh(external_key_of(false)),
            ),
            coin(external_collateral(true), collateral_amount(true), p2pkh(external_key_of(true))),
        ];
        World {
            v24,
            evo_shares: v24 && flags & FLAG_EVO_SHARES != 0,
            early,
            three_shares,
            members: vec![reg, evo, old, shared],
            coins,
        }
    }

    /// Masternode `n`'s state.
    pub fn member(&self, n: u8) -> Option<&Dmn> {
        self.members.iter().find(|d| d.pro_tx_hash == pro_tx(n)).map(|d| &**d)
    }

    /// Edits masternode `n`'s state.
    fn edit_member(&mut self, n: u8, f: impl FnOnce(&mut MnCold)) -> Option<()> {
        let slot = self.members.iter_mut().find(|d| d.pro_tx_hash == pro_tx(n))?;
        let mut dmn = (**slot).clone();
        let mut cold = (*dmn.cold).clone();
        f(&mut cold);
        dmn.cold = Arc::new(fresh(cold));
        *slot = Arc::new(dmn);
        Some(())
    }

    /// The coin at `at`.
    fn coin_mut(&mut self, at: &OutPoint) -> Option<&mut ReplayCoin> {
        self.coins.iter_mut().find(|c| c.txid == *at.hash.as_bytes() && c.vout == at.index)
    }

    fn remove_coin(&mut self, at: &OutPoint) -> Option<()> {
        let before = self.coins.len();
        self.coins.retain(|c| !(c.txid == *at.hash.as_bytes() && c.vout == at.index));
        (self.coins.len() < before).then_some(())
    }

    /// The tip's list as Core's disk bytes.
    fn list_bytes(&self, tip: BlockHash) -> Option<Vec<u8>> {
        let mut list = MnList::new(tip, TIP as i32, 1);
        for dmn in &self.members {
            list.try_add(dmn.clone(), true).ok()?;
        }
        Some(list_disk_bytes(&list))
    }

    fn deployments(&self) -> Vec<(u8, ThresholdState)> {
        let state = |on: bool| {
            if on {
                ThresholdState::Active
            } else {
                ThresholdState::Defined
            }
        };
        vec![(DEPLOYMENT_V24, state(self.v24)), (DEPLOYMENT_EVO_SHARES, state(self.evo_shares))]
    }
}

/// A generated transaction's payload.
#[derive(Clone)]
pub enum Payload {
    Reg(ProRegTx),
    UpReg(ProUpRegTx),
    Dis(ProDisTx),
    Share(ProUpShareTx),
    SharedReg(ProUpSharedRegTx),
}

/// A mutation applied to a case: its step and, for a step in a list loop, the entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Applied {
    pub step: &'static str,
    pub entity: Option<u8>,
}

/// One generated case.
#[derive(Clone)]
pub struct Case {
    pub world: World,
    pub payload: Payload,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    /// The payload's inputs hash stands as a mutation set it, instead of the inputs'.
    keep_inputs_hash: bool,
    /// The payload is its version alone (the two bytes Core's reader stops after, when the
    /// version is out of range).
    bare: bool,
    /// A byte after the payload, which Core's decode refuses.
    trailing: Option<u8>,
    pub applied: Vec<Applied>,
}

impl Case {
    fn new(world: World, payload: Payload, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Case {
        Case {
            world,
            payload,
            inputs,
            outputs,
            keep_inputs_hash: false,
            bare: false,
            trailing: None,
            applied: Vec::new(),
        }
    }

    pub fn tx_type(&self) -> TxType {
        match self.payload {
            Payload::Reg(_) => TxType::ProviderRegister,
            Payload::UpReg(_) => TxType::ProviderUpdateRegistrar,
            Payload::Dis(_) => TxType::ProviderDissolve,
            Payload::Share(_) => TxType::ProviderUpdateShare,
            Payload::SharedReg(_) => TxType::ProviderUpdateSharedRegistrar,
        }
    }

    /// What Core reads of the case: the transaction, the deployments, the list and the coins.
    pub fn fingerprint(&self) -> Option<Vec<u8>> {
        let w = &self.world;
        let mut bytes = vec![u8::from(w.v24), u8::from(w.evo_shares), u8::from(w.early)];
        self.tx().raw().encode(&mut bytes);
        bytes.extend(w.list_bytes(headers()?.1)?);
        bytes.extend(format!("{:?}", w.coins).into_bytes());
        Some(bytes)
    }

    fn reg(&mut self) -> Option<&mut ProRegTx> {
        match &mut self.payload {
            Payload::Reg(p) => Some(p),
            _ => None,
        }
    }

    fn up_reg(&mut self) -> Option<&mut ProUpRegTx> {
        match &mut self.payload {
            Payload::UpReg(p) => Some(p),
            _ => None,
        }
    }

    fn dis(&mut self) -> Option<&mut ProDisTx> {
        match &mut self.payload {
            Payload::Dis(p) => Some(p),
            _ => None,
        }
    }

    fn share(&mut self) -> Option<&mut ProUpShareTx> {
        match &mut self.payload {
            Payload::Share(p) => Some(p),
            _ => None,
        }
    }

    fn shared_reg(&mut self) -> Option<&mut ProUpSharedRegTx> {
        match &mut self.payload {
            Payload::SharedReg(p) => Some(p),
            _ => None,
        }
    }

    /// The masternode a type 3, 10, 11 or 12 payload names.
    fn target(&mut self) -> Option<&mut TxHash> {
        match &mut self.payload {
            Payload::Reg(_) => None,
            Payload::UpReg(p) => Some(&mut p.pro_tx_hash),
            Payload::Dis(p) => Some(&mut p.pro_tx_hash),
            Payload::Share(p) => Some(&mut p.pro_tx_hash),
            Payload::SharedReg(p) => Some(&mut p.pro_tx_hash),
        }
    }

    /// The payload's inputs hash, for the types that carry one.
    fn inputs_hash(&mut self) -> Option<&mut InputsHash> {
        match &mut self.payload {
            Payload::Reg(p) => Some(&mut p.inputs_hash),
            Payload::UpReg(p) => Some(&mut p.inputs_hash),
            Payload::Dis(_) => None,
            Payload::Share(p) => Some(&mut p.inputs_hash),
            Payload::SharedReg(p) => Some(&mut p.inputs_hash),
        }
    }

    /// An external registration's collateral moved to output 0 of `funding` (a transaction of
    /// the same block), its old coin dropped from the view.
    pub fn fund_collateral_from(&mut self, funding: &Tx) {
        let Some(p) = self.reg() else {
            return;
        };
        let old = OutPoint {
            hash: p.collateral_hash,
            index: p.collateral_index,
        };
        p.collateral_hash = TxHash::from(*funding.txid().as_bytes());
        p.collateral_index = 0;
        self.world.remove_coin(&old);
    }

    /// The transaction: the payload's inputs hash set from the inputs (unless a mutation set
    /// it), then encoded as the extra payload.
    pub fn tx(&self) -> Tx {
        let mut case = self.clone();
        if !case.keep_inputs_hash {
            let hash = InputsHash::from_lendian(inputs_hash_of(&case.inputs));
            if let Some(h) = case.inputs_hash() {
                *h = hash;
            }
        }
        let mut extra_payload = Vec::new();
        match &case.payload {
            Payload::Reg(p) => p.encode(&mut extra_payload),
            Payload::UpReg(p) => p.encode(&mut extra_payload),
            Payload::Dis(p) => p.encode(&mut extra_payload),
            Payload::Share(p) => p.encode(&mut extra_payload),
            Payload::SharedReg(p) => p.encode(&mut extra_payload),
        }
        if self.bare {
            extra_payload.truncate(2);
        }
        extra_payload.extend(self.trailing);
        Tx::new(Transaction {
            version: 3,
            tx_type: self.tx_type(),
            inputs: case.inputs,
            outputs: case.outputs,
            lock_time: 0,
            extra_payload,
        })
    }
}

/// The ProRegTx a seed starts from: fresh keys, a version-3 payout list or a version-2 payout
/// script, the network info of its version, no collateral yet.
fn pro_reg_base(version: u16, evo: bool) -> ProRegTx {
    let payout = p2pkh(id(FRESH, 4));
    ProRegTx {
        version,
        mn_type: if evo {
            MnType::Evo
        } else {
            MnType::Regular
        },
        mode: 0,
        collateral_hash: TxHash::from([0; 32]),
        collateral_index: 0,
        net_info: net_info(FRESH, version, evo),
        key_id_owner: id(FRESH, 1).into(),
        pub_key_operator: fresh_key(1),
        key_id_voting: id(FRESH, 2).into(),
        operator_reward: 0,
        script_payout: if version < 3 {
            payout.clone()
        } else {
            Vec::new()
        }
        .into(),
        payouts: if version < 3 {
            Vec::new()
        } else {
            vec![PayoutShare {
                script_payout: payout.into(),
                reward: 10_000,
            }]
        },
        shares: Vec::new(),
        join_sigs: Vec::new(),
        early_period_blocks: 0,
        early_penalty: 0,
        inputs_hash: InputsHash::from_lendian([0; 32]),
        platform_node_id: evo.then(|| PlatformNodeId::from(id(FRESH, 7))),
        platform_p2p_port: (evo && version < 3).then_some(26_657),
        platform_http_port: (evo && version < 3).then_some(1_444),
        vch_sig: Vec::new(),
    }
}

/// Fresh payouts of `rewards` basis points each (at most three).
fn fresh_payouts(rewards: &[u16]) -> Vec<PayoutShare> {
    rewards
        .iter()
        .zip([4, 5, 10])
        .map(|(&reward, n)| PayoutShare {
            script_payout: p2pkh(id(FRESH, n)).into(),
            reward,
        })
        .collect()
}

/// Two fresh payouts, the first of `first` basis points.
fn two_payouts(first: u16) -> Vec<PayoutShare> {
    fresh_payouts(&[first, 10_000 - first])
}

/// A seed's split payouts: two, or three in a world with three shares (so that a step on a
/// payout can have one on the next after it, D19).
fn split_payouts(three: bool) -> Vec<PayoutShare> {
    if three {
        fresh_payouts(&[4_000, 3_000, 3_000])
    } else {
        two_payouts(6_000)
    }
}

/// The funding inputs of a seed: `n` spends of outpoints nothing else names.
fn funding(n: usize) -> Vec<TxIn> {
    (0..n).map(|j| spend(outpoint([0x70 + j as u8; 32], j as u32))).collect()
}

/// A ProRegTx seed. `variant`: bits 0–2 the kind (internal or external collateral, regular or
/// Evo, or shared), bit 3 version 2 instead of 3 under V24 (and version 1 for a regular one
/// before it), bit 4 a second payout (version 3, regular), bit 5 an Evo shared registration
/// (with EVO_SHARES).
fn seed_pro_reg(world: World, variant: u8) -> Option<Case> {
    let kind = variant & 7;
    let shared = kind >= 4 && world.v24;
    let evo = if shared {
        variant & 0x20 != 0 && world.evo_shares
    } else {
        kind & 1 == 1
    };
    let external = !shared && kind & 2 == 2;
    let low = variant & 8 != 0;
    let version = match (world.v24, shared, low, evo) {
        (_, true, ..) => 3,
        (true, false, false, _) => 3,
        (true, false, true, _) => 2,
        (false, _, false, _) | (false, _, true, true) => 2,
        (false, _, true, false) => 1,
    };
    let mut p = pro_reg_base(version, evo);
    if version == 1 {
        p.pub_key_operator = rekeyed(&p.pub_key_operator, BlsSchemeKind::Legacy);
    }
    if version >= 3 && !evo && variant & 0x10 != 0 {
        p.payouts = split_payouts(world.three_shares);
    }
    let amount = collateral_amount(evo);
    let mut outputs = Vec::new();
    if shared {
        let amounts: Vec<i64> = match (evo, world.three_shares) {
            (true, false) => vec![2000 * COIN, 2000 * COIN],
            (true, true) => vec![2000 * COIN, 1000 * COIN, 1000 * COIN],
            (false, three) => member_share_amounts(three),
        };
        p.shares = shares(0xd0, &amounts);
        p.join_sigs = vec![compact_sig()?; amounts.len()];
        p.key_id_owner = [0; 20].into();
        p.payouts = Vec::new();
        p.early_period_blocks = 100;
        p.early_penalty = 50 * COIN;
        outputs.push(out(amount, TEMPLATE.to_vec()));
    } else if external {
        let at = external_collateral(evo);
        p.collateral_hash = at.hash;
        p.collateral_index = at.index;
    } else {
        outputs.push(out(amount, p2pkh(id(FRESH, 3))));
    }
    Some(Case::new(world, Payload::Reg(p), funding(1), outputs))
}

/// A ProUpRegTx seed. `variant`: bit 0 [`EVO`] instead of [`REG`], bit 1 version 2 instead of 3
/// under V24, bit 2 the state's key kept (a version-3 update of [`REG`] then migrates its
/// address), bit 3 two payouts (version 3, [`REG`]).
fn seed_pro_up_reg(world: World, variant: u8) -> Option<Case> {
    if variant & 0x10 != 0 {
        // OLD at version 1, with a fresh key or its own (both legacy).
        let key = if variant & 1 == 1 {
            world.member(OLD)?.cold.pub_key_operator.clone()
        } else {
            rekeyed(&fresh_key(2), BlsSchemeKind::Legacy)
        };
        let p = pro_up_reg(OLD, 1, key);
        return Some(Case::new(world, Payload::UpReg(p), funding(1), Vec::new()));
    }
    let n = if variant & 1 == 1 {
        EVO
    } else {
        REG
    };
    let kept = variant & 4 != 0;
    // An Evo state's addresses do not migrate (its platform ports have no ExtAddr form): a
    // version-3 update keeping its key is refused.
    let version = if world.v24 && variant & 2 == 0 && !(kept && n == EVO) {
        3
    } else {
        2
    };
    let key = if kept {
        world.member(n)?.cold.pub_key_operator.clone()
    } else {
        fresh_key(2)
    };
    let mut p = pro_up_reg(n, version, key);
    if variant & 8 != 0 && version >= 3 && n == REG {
        p.payouts = split_payouts(world.three_shares);
    }
    Some(Case::new(world, Payload::UpReg(p), funding(1), Vec::new()))
}

/// A ProUpRegTx at `version` for masternode `n` with operator key `key`, a fresh voting key and
/// one payout.
fn pro_up_reg(n: u8, version: u16, key: OperatorKey) -> ProUpRegTx {
    let payout = p2pkh(id(FRESH, 4));
    ProUpRegTx {
        version,
        pro_tx_hash: TxHash::from(*pro_tx(n).as_bytes()),
        mode: 0,
        pub_key_operator: key,
        key_id_voting: id(FRESH, 2).into(),
        script_payout: if version < 3 {
            payout.clone()
        } else {
            Vec::new()
        }
        .into(),
        payouts: if version < 3 {
            Vec::new()
        } else {
            vec![PayoutShare {
                script_payout: payout.into(),
                reward: 10_000,
            }]
        },
        inputs_hash: InputsHash::from_lendian([0; 32]),
        vch_sig: vec![0; 65],
    }
}

/// `netinfo_add_entry`'s ProUpReg leg (EV1 §6.5, D29): under V24, masternode REG (or EVO, with
/// `evo`) at `state_version` holding `net_info`, and a ProUpRegTx at `version` keeping its
/// operator key (in the scheme each version reads), so that Core runs the state migration
/// (§3.5 5b) from `state_version` to `max(state_version, version)` and every other check passes.
/// `None` when no list holds that state (the object's kind does not follow the version, an
/// EvoNode below version 2, versions outside 1-3, an update below the state) or the list refuses
/// it (an address another masternode holds).
pub fn migration_case(
    evo: bool,
    state_version: u16,
    net_info: NetInfo,
    version: u16,
) -> Option<Case> {
    let extended = matches!(net_info, NetInfo::Extended(_));
    if !(1..=3).contains(&state_version)
        || !(state_version..=3).contains(&version)
        || extended != (state_version >= 3)
        || (evo && state_version < 2)
    {
        return None;
    }
    let n = if evo {
        EVO
    } else {
        REG
    };
    let mut world = World::new(FLAG_V24);
    world.edit_member(n, |c| {
        c.version = i32::from(state_version);
        c.net_info = Arc::new(net_info);
        c.pub_key_operator = rekeyed(&member_key(n), scheme_at(state_version));
        if state_version >= 3 {
            c.platform_p2p_port = 0;
            c.platform_http_port = 0;
        }
    })?;
    world.list_bytes(headers()?.1)?;
    let key = rekeyed(&member_key(n), scheme_at(version));
    Some(Case::new(world, Payload::UpReg(pro_up_reg(n, version, key)), funding(1), Vec::new()))
}

/// The outputs of a dissolution of the shared masternode by `actor`: each non-actor share
/// refunded with its pro-rata bonus (the floors' remainder on the first), the actor's refund less
/// the bonuses and [`DIS_FEE`].
fn dissolution_outputs(world: &World, actor: usize, unanimous: bool) -> Option<Vec<TxOut>> {
    let state = &world.member(SHARED)?.cold;
    let early = 101 - i64::from(state.registered_height) < i64::from(state.early_period_blocks);
    let required = if unanimous || !early {
        0
    } else {
        state.early_penalty
    };
    let non_actor: Vec<&CollateralShare> =
        state.shares.iter().enumerate().filter(|&(i, _)| i != actor).map(|(_, s)| s).collect();
    let total: i64 = non_actor.iter().map(|s| s.amount).sum();
    let floors: Vec<i64> = non_actor
        .iter()
        .map(|s| dash_dml::specialtx::shared::pro_rata_floor(required, s.amount, total))
        .collect::<Option<_>>()?;
    let remainder = required - floors.iter().sum::<i64>();
    let mut outputs: Vec<TxOut> = non_actor
        .iter()
        .zip(&floors)
        .enumerate()
        .map(|(i, (s, floor))| {
            let bonus = floor
                + if i == 0 {
                    remainder
                } else {
                    0
                };
            out(s.amount + bonus, s.script_refund.as_bytes().to_vec())
        })
        .collect();
    let actor_share = state.shares.get(actor)?;
    outputs.push(out(
        actor_share.amount - required - DIS_FEE,
        actor_share.script_refund.as_bytes().to_vec(),
    ));
    Some(outputs)
}

/// A ProDisTx seed of the shared masternode. `variant`: bit 0 unanimous, the rest the actor.
fn seed_pro_dis(world: World, variant: u8) -> Option<Case> {
    let count = world.member(SHARED)?.cold.shares.len();
    let unanimous = variant & 1 == 1;
    let actor = usize::from(variant >> 1) % count;
    let p = ProDisTx {
        version: 1,
        pro_tx_hash: TxHash::from(*pro_tx(SHARED).as_bytes()),
        actor_index: actor as u16,
        sigs: vec![
            compact_sig()?;
            if unanimous {
                count
            } else {
                1
            }
        ],
    };
    let outputs = dissolution_outputs(&world, actor, unanimous)?;
    let inputs = vec![spend(member_collateral(SHARED))];
    Some(Case::new(world, Payload::Dis(p), inputs, outputs))
}

/// A ProUpShareTx seed: `variant` picks the share.
fn seed_pro_up_share(world: World, variant: u8) -> Option<Case> {
    let count = world.member(SHARED)?.cold.shares.len();
    let p = ProUpShareTx {
        version: 1,
        pro_tx_hash: TxHash::from(*pro_tx(SHARED).as_bytes()),
        share_index: (usize::from(variant) % count) as u16,
        script_reward: p2pkh(id(FRESH, 6)).into(),
        inputs_hash: InputsHash::from_lendian([0; 32]),
        vch_sig: vec![0; COMPACT_SIG_LEN],
    };
    Some(Case::new(world, Payload::Share(p), funding(1), Vec::new()))
}

/// A ProUpSharedRegTx seed: a fresh operator and voting key, one signature per share.
fn seed_pro_up_shared_reg(world: World, _variant: u8) -> Option<Case> {
    let count = world.member(SHARED)?.cold.shares.len();
    let p = ProUpSharedRegTx {
        version: 1,
        pro_tx_hash: TxHash::from(*pro_tx(SHARED).as_bytes()),
        pub_key_operator: fresh_key(3),
        key_id_voting: id(FRESH, 2).into(),
        inputs_hash: InputsHash::from_lendian([0; 32]),
        sigs: vec![compact_sig()?; count],
    };
    Some(Case::new(world, Payload::SharedReg(p), funding(1), Vec::new()))
}

/// The seed variants of `tx_type` that differ (the rest repeat them).
pub fn variants(tx_type: TxType) -> std::ops::Range<u8> {
    match tx_type {
        TxType::ProviderRegister => 0..0x40,
        TxType::ProviderUpdateRegistrar => 0..0x12,
        TxType::ProviderDissolve => 0..6,
        TxType::ProviderUpdateShare => 0..3,
        _ => 0..1,
    }
}

/// The seed of `tx_type` (one of [`TYPES`]) in `world`.
pub fn seed(world: World, tx_type: TxType, variant: u8) -> Option<Case> {
    match tx_type {
        TxType::ProviderRegister => seed_pro_reg(world, variant),
        TxType::ProviderUpdateRegistrar => seed_pro_up_reg(world, variant),
        TxType::ProviderDissolve => seed_pro_dis(world, variant),
        TxType::ProviderUpdateShare => seed_pro_up_share(world, variant),
        TxType::ProviderUpdateSharedRegistrar => seed_pro_up_shared_reg(world, variant),
        _ => None,
    }
}

/// A violation of one EV1 §3 step.
pub struct Mutation {
    /// `table:step` (the module's labels).
    pub step: &'static str,
    /// The reasons the step rejects with.
    pub reasons: &'static [&'static str],
    /// Applies the violation, `arg` choosing among its forms and its entity; `None` when the
    /// case cannot carry it. `Some(entity)` for a step in a list loop.
    apply: fn(&mut Case, u8) -> Option<Option<u8>>,
}

const fn m(
    step: &'static str,
    reasons: &'static [&'static str],
    apply: fn(&mut Case, u8) -> Option<Option<u8>>,
) -> Mutation {
    Mutation {
        step,
        reasons,
        apply,
    }
}

/// The defect of a step, checked on a case, for the steps the pair check cannot judge by
/// re-applying them: those whose mutation rewrites more than the field its check reads (a
/// payload rebuilt at another version, a target moved with its key, outputs or rewards set
/// whole), those that cannot apply to their own result (a coin removed, a deployment turned off,
/// a value stepped), and those a bare payload carries by its defaults (null keys). Every other
/// step is idempotent.
fn own_defect(step: &str) -> Option<fn(&mut Case) -> Option<bool>> {
    Some(match step {
        "1p:version" => |c| {
            let max = if c.world.v24 {
                3
            } else {
                2
            };
            Some(c.reg()?.version > max)
        },
        "3p:version" => |c| {
            let max = if c.world.v24 {
                3
            } else {
                2
            };
            Some(c.up_reg()?.version > max)
        },
        "1t:2" => |c| {
            let bare = c.bare;
            let p = c.reg()?;
            Some(!bare && p.mn_type == MnType::Evo && p.version < 2)
        },
        "1c:6e1" => |c| {
            let evo = is_evo(c)?;
            let p = c.reg()?;
            if p.collateral_hash.is_null() {
                return Some(false);
            }
            let at = OutPoint {
                hash: p.collateral_hash,
                index: p.collateral_index,
            };
            let coin =
                c.world.coins.iter().find(|x| x.txid == *at.hash.as_bytes() && x.vout == at.index);
            Some(coin.is_none_or(|x| x.amount != collateral_amount(evo)))
        },
        "1c:6i2" => |c| {
            let evo = is_evo(c)?;
            Some(collateral_output(c)?.value != collateral_amount(evo))
        },
        "1c:6i1" => |c| {
            let outputs = c.outputs.len() as u32;
            let p = c.reg()?;
            Some(p.collateral_hash.is_null() && p.collateral_index >= outputs)
        },
        "1c:3a" => |c| {
            let evo_shares = c.world.evo_shares;
            let p = c.reg()?;
            Some(!evo_shares && p.mn_type == MnType::Evo && !p.shares.is_empty())
        },
        "1c:2" => |c| {
            let v24 = c.world.v24;
            let p = c.reg()?;
            Some(v24 && p.version == 1 && p.mn_type != MnType::Evo && p.shares.is_empty())
        },
        "sl:4" => |c| {
            let required = collateral_amount(is_evo(c)?);
            Some(reg_shares(c)?.iter().map(|s| s.amount).sum::<i64>() != required)
        },
        "1t:5e" => |c| {
            let bare = c.bare;
            let p = c.reg()?;
            Some(bare || (p.shares.is_empty() && p.key_id_owner == [0; 20].into()))
        },
        "1t:6" => |c| {
            let bare = c.bare;
            let p = c.reg()?;
            Some(bare || p.pub_key_operator == invalid_key() || p.key_id_voting == [0; 20].into())
        },
        "3t:3" => |c| {
            let bare = c.bare;
            let p = c.up_reg()?;
            Some(bare || p.pub_key_operator == invalid_key() || p.key_id_voting == [0; 20].into())
        },
        "10c:2" => |c| {
            c.dis()?;
            Some(!c.world.v24)
        },
        "11c:2" => |c| {
            c.share()?;
            Some(!c.world.v24)
        },
        "12c:2" => |c| {
            c.shared_reg()?;
            Some(!c.world.v24)
        },
        "3c:8" => |c| {
            let at = member_collateral(up_reg_member(c)?);
            Some(!c.world.coins.iter().any(|x| x.txid == *at.hash.as_bytes() && x.vout == at.index))
        },
        "pl:3" => |c| {
            let sum: u32 = match payouts(c)? {
                Payouts::List(list) => list.iter().map(|p| u32::from(p.reward)).sum(),
                Payouts::Script(_) => return Some(false),
            };
            Some(sum != 10_000)
        },
        "3c:4" => |c| {
            let v24 = c.world.v24;
            let (version, state) = (c.up_reg()?.version, up_reg_state(c)?.version);
            Some(v24 && version == 1 && state >= 2)
        },
        "3c:5b" => |c| {
            // A state below ExtAddr whose addresses ExtAddr refuses (OLD's port 22, any Evo's
            // platform ports), raised to ExtAddr with its key kept.
            let v24 = c.world.v24;
            let n = up_reg_member(c)?;
            let state = up_reg_state(c)?;
            let (state_version, state_key) = (state.version, state.pub_key_operator.clone());
            let p = c.up_reg()?;
            let kept = p.pub_key_operator == rekeyed(&state_key, p.pub_key_operator.scheme());
            Some(v24 && (n == OLD || n == EVO) && state_version < 3 && p.version >= 3 && kept)
        },
        "3c:7" => |c| {
            let evo_shares = c.world.evo_shares;
            let p = c.up_reg()?;
            Some(
                !evo_shares
                    && p.pro_tx_hash == TxHash::from(*pro_tx(EVO).as_bytes())
                    && p.payouts.len() > 1,
            )
        },
        "10l:10" => |c| {
            let (floors, required) = dis_floors(c)?;
            let bonus: i64 =
                c.outputs.iter().zip(&floors).map(|(o, (amount, _))| o.value - amount).sum();
            Some(bonus < required)
        },
        "10l:12" => |c| {
            let (floors, _) = dis_floors(c)?;
            let (_, _, unanimous) = dis_parts(c)?;
            let bonus: i64 =
                c.outputs.iter().zip(&floors).map(|(o, (amount, _))| o.value - amount).sum();
            Some(!unanimous && bonus > target_state(c)?.early_penalty)
        },
        "10l:11" => |c| {
            c.dis()?;
            let total: i64 = target_shares(c)?.iter().map(|s| s.amount).sum();
            let out: i64 = c.outputs.iter().map(|o| o.value).sum();
            Some(total - out > MAX_FEE)
        },
        _ => return None,
    })
}

impl Mutation {
    /// Whether the step checks its own defect on a case ([`Mutation::holds`]).
    pub fn checks_itself(&self) -> bool {
        own_defect(self.step).is_some()
    }

    /// Whether `case` still carries this step's defect, for a step that checks it itself;
    /// `None` for the others.
    pub fn holds(&self, case: &Case) -> Option<bool> {
        let defect = own_defect(self.step)?;
        Some(defect(&mut case.clone()).unwrap_or(false))
    }

    /// Applies the mutation and records it; `false` when the case cannot carry it.
    pub fn apply(&self, case: &mut Case, arg: u8) -> bool {
        match (self.apply)(case, arg) {
            Some(entity) => {
                case.applied.push(Applied {
                    step: self.step,
                    entity,
                });
                true
            }
            None => false,
        }
    }
}

/// Entity `arg % len`; `None` for an empty list.
fn index(arg: u8, len: usize) -> Option<usize> {
    (len > 0).then(|| usize::from(arg) % len)
}

/// Done, with no entity.
const DONE: Option<Option<u8>> = Some(None);

fn entity(i: usize) -> Option<Option<u8>> {
    Some(Some(i as u8))
}

/// An invalid payload version: 0 or past the maximum.
/// A ProRegTx or ProUpRegTx payload Core's decode refuses: a byte after it, or (all fields
/// written) a version its reader stops at.
fn bad_payload(c: &mut Case, arg: u8) -> Option<Option<u8>> {
    if arg & 1 == 0 {
        c.trailing = Some(arg >> 1);
        return DONE;
    }
    let version = bad_version(arg >> 1, 3);
    match &mut c.payload {
        Payload::Reg(p) => p.version = version,
        Payload::UpReg(p) => p.version = version,
        _ => return None,
    }
    DONE
}

fn bad_version(arg: u8, max: u16) -> u16 {
    if arg & 1 == 0 {
        0
    } else {
        max + 1 + u16::from(arg >> 1)
    }
}

/// The payout list of a ProRegTx or ProUpRegTx: the payouts from version 3, else the payload
/// whose payout script stands for the one entry.
enum Payouts<'a> {
    List(&'a mut Vec<PayoutShare>),
    Script(ScriptSlot<'a>),
}

/// The payload holding a payout script.
enum ScriptSlot<'a> {
    Reg(&'a mut ProRegTx),
    UpReg(&'a mut ProUpRegTx),
}

impl ScriptSlot<'_> {
    fn set(&mut self, script: Vec<u8>) {
        match self {
            ScriptSlot::Reg(p) => p.script_payout = script.into(),
            ScriptSlot::UpReg(p) => p.script_payout = script.into(),
        }
    }
}

fn payouts(case: &mut Case) -> Option<Payouts<'_>> {
    Some(match &mut case.payload {
        Payload::Reg(p) if p.shares.is_empty() => {
            if p.version >= 3 {
                Payouts::List(&mut p.payouts)
            } else {
                Payouts::Script(ScriptSlot::Reg(p))
            }
        }
        Payload::UpReg(p) => {
            if p.version >= 3 {
                Payouts::List(&mut p.payouts)
            } else {
                Payouts::Script(ScriptSlot::UpReg(p))
            }
        }
        _ => return None,
    })
}

/// Payout `arg % len`'s script set to `script(i)`; the payout script for an old version.
fn set_payout_script(
    case: &mut Case,
    arg: u8,
    script: fn(usize, &[PayoutShare]) -> Option<Vec<u8>>,
) -> Option<Option<u8>> {
    match payouts(case)? {
        Payouts::List(list) => {
            let i = index(arg, list.len())?;
            let s = script(i, list)?;
            list.get_mut(i)?.script_payout = s.into();
            entity(i)
        }
        Payouts::Script(mut s) => {
            s.set(script(0, &[])?);
            entity(0)
        }
    }
}

/// The voting key of the payload, which a payout may not pay.
fn voting(case: &Case) -> Option<[u8; 20]> {
    Some(match &case.payload {
        Payload::Reg(p) => *p.key_id_voting.as_bytes(),
        Payload::UpReg(p) => *p.key_id_voting.as_bytes(),
        Payload::SharedReg(p) => *p.key_id_voting.as_bytes(),
        _ => return None,
    })
}

/// The share list of a shared ProRegTx.
fn reg_shares(case: &mut Case) -> Option<&mut Vec<CollateralShare>> {
    let p = case.reg()?;
    (!p.shares.is_empty()).then_some(&mut p.shares)
}

/// The share `arg % len` of a shared ProRegTx, and the list.
fn share_at(case: &mut Case, arg: u8) -> Option<(usize, &mut Vec<CollateralShare>)> {
    let shares = reg_shares(case)?;
    Some((index(arg, shares.len())?, shares))
}

/// The ProRegTx's network info, as an ExtNetInfo's entries.
fn ext_entries(case: &mut Case) -> Option<&mut Vec<(NIPurpose, Vec<NIEntry>)>> {
    match &mut case.reg()?.net_info {
        NetInfo::Extended(n) => Some(&mut n.entries),
        _ => None,
    }
}

/// The core service of the ProRegTx's network info.
fn core_service(case: &mut Case) -> Option<&mut ServiceV2> {
    let entries = ext_entries(case)?;
    let (_, list) = entries.iter_mut().find(|(p, _)| *p == NIPurpose::CoreP2p)?;
    match list.first_mut()? {
        NIEntry::Service(s) => Some(s),
        _ => None,
    }
}

/// The entries of the ExtNetInfo's purpose `arg % len`, and its index.
fn purpose_entries(case: &mut Case, arg: u8) -> Option<(usize, &mut Vec<NIEntry>)> {
    let entries = ext_entries(case)?;
    let p = index(arg, entries.len())?;
    Some((p, &mut entries.get_mut(p)?.1))
}

/// A legacy network info's service port and address, for an old-version ProRegTx.
fn legacy_service(case: &mut Case) -> Option<&mut ServiceV1> {
    match &mut case.reg()?.net_info {
        NetInfo::Legacy(NetInfoV1(s)) => Some(s),
        _ => None,
    }
}

fn is_evo(case: &mut Case) -> Option<bool> {
    Some(case.reg()?.mn_type == MnType::Evo)
}

/// The ProRegTx's internal collateral output.
fn collateral_output(case: &mut Case) -> Option<&mut TxOut> {
    let p = case.reg()?;
    if !p.collateral_hash.is_null() {
        return None;
    }
    let i = p.collateral_index as usize;
    case.outputs.get_mut(i)
}

/// The ProRegTx's external collateral coin.
fn collateral_coin(case: &mut Case) -> Option<&mut ReplayCoin> {
    let p = case.reg()?;
    if p.collateral_hash.is_null() {
        return None;
    }
    let at = OutPoint {
        hash: p.collateral_hash,
        index: p.collateral_index,
    };
    case.world.coin_mut(&at)
}

/// The key the ProRegTx's collateral pays, if it has a destination.
fn collateral_key(case: &mut Case) -> Option<[u8; 20]> {
    let script = match collateral_coin(case) {
        Some(c) => c.script_pubkey.clone(),
        None => collateral_output(case)?.script_pubkey.as_bytes().to_vec(),
    };
    match script.as_slice() {
        [0x76, 0xa9, 20, key @ .., 0x88, 0xac] => key.try_into().ok(),
        _ => None,
    }
}

/// The steps of `IsPayoutListTriviallyValid` (§3.14) on a ProRegTx's or ProUpRegTx's payouts.
const PAYOUT_LIST: &[Mutation] = &[
    m("pl:1", &[BAD_PROTX_PAYOUTS_COUNT], |c, arg| match payouts(c)? {
        Payouts::List(list) => {
            if arg & 1 == 0 {
                list.clear();
            } else {
                if list.len() > 8 {
                    return DONE;
                }
                let first = list.first()?.clone();
                list.extend((0..9).map(|i| PayoutShare {
                    script_payout: p2pkh(id(9, i)).into(),
                    reward: first.reward,
                }));
            }
            DONE
        }
        Payouts::Script(_) => None,
    }),
    m("pl:2a", &[BAD_PROTX_PAYOUT_REWARD], |c, arg| match payouts(c)? {
        Payouts::List(list) => {
            let i = index(arg >> 1, list.len())?;
            list.get_mut(i)?.reward = if arg & 1 == 0 {
                99
            } else {
                10_001
            };
            entity(i)
        }
        Payouts::Script(_) => None,
    }),
    m("pl:2b", &[BAD_PROTX_PAYEE], |c, arg| {
        set_payout_script(c, arg, |_, _| Some(NO_DEST.to_vec()))
    }),
    m("pl:2c", &[BAD_PROTX_PAYEE_DUP], |c, arg| match payouts(c)? {
        Payouts::List(list) if list.len() >= 2 => {
            let i = 1 + usize::from(arg) % (list.len() - 1);
            let earlier = list.get(i - 1)?.script_payout.clone();
            list.get_mut(i)?.script_payout = earlier;
            entity(i)
        }
        _ => None,
    }),
    m("pl:2e", &[BAD_PROTX_PAYEE_REUSE], |c, arg| {
        let key = match (&c.payload, arg & 1) {
            (Payload::Reg(p), 0) => *p.key_id_owner.as_bytes(),
            _ => voting(c)?,
        };
        let i = match payouts(c)? {
            Payouts::List(list) => {
                let i = index(arg >> 1, list.len())?;
                list.get_mut(i)?.script_payout = p2pkh(key).into();
                i
            }
            Payouts::Script(mut s) => {
                s.set(p2pkh(key));
                0
            }
        };
        entity(i)
    }),
    m("pl:3", &[BAD_PROTX_PAYOUT_REWARD_SUM], |c, arg| match payouts(c)? {
        Payouts::List(list) => {
            // One basis point off, every reward within its bounds.
            if list.iter().any(|p| !(100..=10_000).contains(&p.reward)) {
                return None;
            }
            let i = index(arg, list.len())?;
            let p = list.get_mut(i)?;
            p.reward = if p.reward > 100 {
                p.reward - 1
            } else {
                p.reward + 1
            };
            DONE
        }
        Payouts::Script(_) => None,
    }),
];

/// The steps of `IsShareListTriviallyValid` (§3.14) on a shared ProRegTx's shares.
const SHARE_LIST: &[Mutation] = &[
    m("sl:1", &[BAD_PROTX_SHARES_COUNT], |c, arg| {
        let shares = reg_shares(c)?;
        if arg & 1 == 0 {
            shares.truncate(1);
        } else {
            let first = shares.first()?.clone();
            let more = (shares.len()..9).map(|i| CollateralShare {
                key_id_owner: share_id(0xe0, i, 2).into(),
                script_refund: p2pkh(share_id(0xe0, i, 0)).into(),
                ..first.clone()
            });
            let more: Vec<_> = more.collect();
            shares.extend(more);
        }
        let count = shares.len();
        let sig = compact_sig()?;
        c.reg()?.join_sigs = vec![sig; count];
        DONE
    }),
    m("sl:2", &[BAD_PROTX_SHARES_EARLY_PERIOD], |c, arg| {
        reg_shares(c)?;
        c.reg()?.early_period_blocks = 420_481 + u32::from(arg);
        DONE
    }),
    m("sl:3a", &[BAD_PROTX_SHARES_AMOUNT], |c, arg| {
        let required = collateral_amount(is_evo(c)?);
        let (i, shares) = share_at(c, arg >> 1)?;
        shares.get_mut(i)?.amount = if arg & 1 == 0 {
            100 * COIN - 1
        } else {
            required + 1
        };
        entity(i)
    }),
    m("sl:3b", &[BAD_PROTX_SHARES_KEY_NULL], |c, arg| {
        let (i, shares) = share_at(c, arg)?;
        shares.get_mut(i)?.key_id_owner = [0; 20].into();
        entity(i)
    }),
    m("sl:3c", &[BAD_PROTX_SHARES_DUP_KEY], |c, arg| {
        let (i, shares) = share_at(c, arg)?;
        let i = i.max(1);
        shares.get_mut(i)?.key_id_owner = shares.get(i - 1)?.key_id_owner;
        entity(i)
    }),
    m("sl:3d", &[BAD_PROTX_SHARES_DUP_REFUND], |c, arg| {
        let (i, shares) = share_at(c, arg)?;
        let i = i.max(1);
        shares.get_mut(i)?.script_refund = shares.get(i - 1)?.script_refund.clone();
        entity(i)
    }),
    m("sl:3e.i", &[BAD_PROTX_SHARES_PAYEE], |c, arg| {
        let (i, shares) = share_at(c, arg >> 1)?;
        let share = shares.get_mut(i)?;
        if arg & 1 == 0 {
            share.script_refund = NO_DEST.to_vec().into();
        } else {
            share.script_reward = NO_DEST.to_vec().into();
        }
        entity(i)
    }),
    m("sl:3e.iii", &[BAD_PROTX_SHARES_PAYEE_REUSE], |c, arg| {
        let voting = voting(c)?;
        let (i, shares) = share_at(c, arg >> 1)?;
        let share = shares.get_mut(i)?;
        if arg & 1 == 0 {
            share.script_refund = p2pkh(voting).into();
        } else {
            share.script_reward = p2pkh(voting).into();
        }
        entity(i)
    }),
    m("sl:3e.iv", &[BAD_PROTX_SHARES_PAYEE_REUSE], |c, arg| {
        let (i, shares) = share_at(c, arg >> 2)?;
        // The owner of this share or of another, before or after it.
        let owner =
            *shares.get(usize::from(arg >> 1 & 1) * (shares.len() - 1))?.key_id_owner.as_bytes();
        let share = shares.get_mut(i)?;
        if arg & 1 == 0 {
            share.script_refund = p2pkh(owner).into();
        } else {
            share.script_reward = p2pkh(owner).into();
        }
        entity(i)
    }),
    m("sl:4", &[BAD_PROTX_SHARES_AMOUNT_SUM], |c, arg| {
        let shares = reg_shares(c)?;
        let first = shares.first_mut()?;
        first.amount += if arg & 1 == 0 {
            -1
        } else {
            1
        };
        DONE
    }),
    m("sl:5", &[BAD_PROTX_SHARES_PENALTY], |c, arg| {
        let min = reg_shares(c)?.iter().map(|s| s.amount).min()?;
        c.reg()?.early_penalty = if arg & 1 == 0 {
            -1
        } else {
            min
        };
        DONE
    }),
    m("sl:6", &[BAD_PROTX_SHARES_PENALTY], |c, _| {
        reg_shares(c)?;
        let p = c.reg()?;
        p.early_period_blocks = 0;
        (p.early_penalty != 0).then_some(None)
    }),
];

/// `IsNetInfoTriviallyValid` (§3.15) and the `CheckNetInfo` statuses `CheckProviderNetworkFields`
/// maps (its step 2b, labelled `ns:` by status) that regtest reaches, on an ExtNetInfo. Regtest's
/// address rules admit a zero port, an unroutable address and a Tor core address, so `BadPort`,
/// `NotRoutable` and `BadType` are left to EV1-1's NetInfo targets, which read mainnet's.
const NET_INFO: &[Mutation] = &[
    m("nt:1", &[BAD_PROTX_NETINFO_EMPTY], |c, _| {
        let entries = ext_entries(c)?;
        entries.retain(|(p, _)| *p != NIPurpose::CoreP2p);
        if entries.is_empty() {
            entries.push((NIPurpose::PlatformP2p, vec![NIEntry::Service(service(FRESH, 26_656))]));
        }
        DONE
    }),
    m("nt:2", &[BAD_PROTX_NETINFO_BAD], |c, _| {
        if is_evo(c)? {
            return None;
        }
        let entries = ext_entries(c)?;
        if !entries.iter().any(|(p, _)| *p == NIPurpose::PlatformHttps) {
            entries.push((NIPurpose::PlatformHttps, vec![NIEntry::Service(service(FRESH, 1_443))]));
        }
        DONE
    }),
    m("nt:3", &[BAD_PROTX_NETINFO_EMPTY], |c, arg| {
        if !is_evo(c)? {
            return None;
        }
        let drop = if arg & 1 == 0 {
            NIPurpose::PlatformHttps
        } else {
            NIPurpose::PlatformP2p
        };
        ext_entries(c)?.retain(|(p, _)| *p != drop);
        DONE
    }),
    m("ns:malformed", &[BAD_PROTX_NETINFO_BAD], |c, arg| {
        // A second entry of a purpose, so that a step on the first one stays.
        let (p, list) = purpose_entries(c, arg)?;
        let port = match list.first()? {
            NIEntry::Service(s) => s.port,
            NIEntry::Domain {
                port,
                ..
            } => *port,
            NIEntry::Invalid => return None,
        };
        let zero = NIEntry::Service(ServiceV2 {
            addr: AddrV2::Ipv4([0; 4]),
            port,
        });
        if !list.contains(&zero) {
            list.push(zero);
        }
        entity(p)
    }),
    m("ns:dup", &[BAD_PROTX_DUP_NETINFO_ENTRY], |c, arg| {
        let (p, list) = purpose_entries(c, arg)?;
        let first = list.first()?.clone();
        if !list[1..].contains(&first) {
            list.push(first);
        }
        entity(p)
    }),
    m("ns:maxlimit", &[BAD_PROTX_NETINFO_MAXLIMIT], |c, arg| {
        let (p, list) = purpose_entries(c, arg)?;
        let NIEntry::Service(first) = list.first()?.clone() else {
            return None;
        };
        // Fresh addresses: copies of a malformed entry would be duplicates of each other.
        if list.len() > 4 {
            return entity(p);
        }
        list.extend((1..=8).map(|i| {
            NIEntry::Service(ServiceV2 {
                addr: AddrV2::Ipv4([1, 2, 4, i]),
                port: first.port,
            })
        }));
        entity(p)
    }),
    m("ns:input", &[BAD_PROTX_NETINFO_ENTRY], |c, arg| {
        // A domain where a purpose takes addresses only (all but platform HTTPS).
        let (p, list) = purpose_entries(c, arg)?;
        let first = list.first()?.clone();
        if ext_entries(c)?.get(p)?.0 == NIPurpose::PlatformHttps {
            return None;
        }
        let first = match first {
            NIEntry::Service(first) => first,
            NIEntry::Domain {
                name,
                ..
            } if name == b"example.com" => return entity(p),
            _ => return None,
        };
        *purpose_entries(c, arg)?.1.first_mut()? = NIEntry::Domain {
            name: b"example.com".to_vec(),
            port: first.port,
        };
        entity(p)
    }),
];

/// `CheckProviderNetworkFields`' platform steps (§3.15 4–6), on an Evo ProRegTx.
const PLATFORM_FIELDS: &[Mutation] = &[
    m("nf:4", &[BAD_PROTX_PLATFORM_NODEID], |c, _| {
        if !is_evo(c)? {
            return None;
        }
        c.reg()?.platform_node_id = Some(PlatformNodeId::from([0; 20]));
        DONE
    }),
    m("nf:6b", &[BAD_PROTX_PLATFORM_P2P_PORT, BAD_PROTX_PLATFORM_HTTP_PORT], |c, arg| {
        let p = c.reg()?;
        if p.mn_type != MnType::Evo || p.version >= 3 {
            return None;
        }
        if arg & 1 == 0 {
            p.platform_p2p_port = Some(9_999);
        } else {
            p.platform_http_port = Some(9_999);
        }
        DONE
    }),
    m("nf:6c", &[BAD_PROTX_PLATFORM_DUP_PORTS], |c, arg| {
        let p = c.reg()?;
        if p.mn_type != MnType::Evo || p.version >= 3 {
            return None;
        }
        match arg % 3 {
            0 => p.platform_http_port = p.platform_p2p_port,
            1 => p.platform_p2p_port = Some(PORT),
            _ => p.platform_http_port = Some(PORT),
        }
        DONE
    }),
];

/// `CProRegTx::IsTriviallyValid` and `CheckProRegTx` (§3.3), in Core's order, the list and
/// network steps where Core calls them.
const PRO_REG: &[Mutation] = &[
    m("1p:payload", &[BAD_PROTX_PAYLOAD], |c, arg| {
        c.reg()?;
        bad_payload(c, arg)
    }),
    m("1p:version", &[BAD_PROTX_VERSION], |c, arg| {
        if arg & 1 != 0 {
            c.reg()?.version = 4 + u16::from(arg >> 1);
            c.bare = true;
            return DONE;
        }
        if c.world.v24 {
            return None;
        }
        let p = c.reg()?;
        *p = ProRegTx {
            collateral_hash: p.collateral_hash,
            collateral_index: p.collateral_index,
            ..pro_reg_base(3, p.mn_type == MnType::Evo)
        };
        DONE
    }),
    m("1t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.reg()?.version = 0;
        c.bare = true;
        DONE
    }),
    m("1t:2", &[BAD_PROTX_EVO_VERSION], |c, _| {
        let p = c.reg()?;
        if p.mn_type != MnType::Evo || !p.shares.is_empty() {
            return None;
        }
        *p = ProRegTx {
            collateral_hash: p.collateral_hash,
            collateral_index: p.collateral_index,
            ..pro_reg_base(1, true)
        };
        p.pub_key_operator = rekeyed(&p.pub_key_operator, BlsSchemeKind::Legacy);
        DONE
    }),
    m("1t:3", &[BAD_PROTX_TYPE], |c, arg| {
        let p = c.reg()?;
        p.mn_type = MnType::from_base(2 + u16::from(arg));
        p.platform_node_id = None;
        p.platform_p2p_port = None;
        p.platform_http_port = None;
        DONE
    }),
    m("1t:4", &[BAD_PROTX_MODE], |c, arg| {
        c.reg()?.mode = 1 + u16::from(arg);
        DONE
    }),
    m("1t:5a", &[BAD_PROTX_SHARES_EXTERNAL], |c, _| {
        reg_shares(c)?;
        c.reg()?.collateral_hash = TxHash::from([0x66; 32]);
        DONE
    }),
    m("1t:5b", &[BAD_PROTX_SHARES_OWNER_KEY], |c, _| {
        reg_shares(c)?;
        c.reg()?.key_id_owner = id(FRESH, 1).into();
        DONE
    }),
    m("1t:5c", &[BAD_PROTX_SHARES_PAYOUTS], |c, _| {
        reg_shares(c)?;
        c.reg()?.payouts = vec![PayoutShare {
            script_payout: p2pkh(id(FRESH, 4)).into(),
            reward: 10_000,
        }];
        DONE
    }),
    m("1t:5d", &[BAD_PROTX_SHARES_EMPTY_FIELDS], |c, arg| {
        let p = c.reg()?;
        if !p.shares.is_empty() || p.version < 3 {
            return None;
        }
        if arg & 1 == 0 {
            p.early_period_blocks = 1;
        } else {
            p.early_penalty = 1;
        }
        DONE
    }),
    m("1t:5e", &[BAD_PROTX_KEY_NULL], |c, _| {
        let p = c.reg()?;
        if !p.shares.is_empty() {
            return None;
        }
        p.key_id_owner = [0; 20].into();
        DONE
    }),
    m("1t:6", &[BAD_PROTX_KEY_NULL], |c, arg| {
        let p = c.reg()?;
        if arg & 1 == 0 {
            p.pub_key_operator = invalid_key();
        } else {
            p.key_id_voting = [0; 20].into();
        }
        DONE
    }),
    m("1t:12", &[BAD_PROTX_OPERATOR_REWARD], |c, arg| {
        c.reg()?.operator_reward = 10_001 + u16::from(arg);
        DONE
    }),
    m("1c:2", &[BAD_PROTX_VERSION_DISALLOWED], |c, _| {
        if !c.world.v24 {
            return None;
        }
        let p = c.reg()?;
        if p.mn_type == MnType::Evo || !p.shares.is_empty() {
            return None;
        }
        *p = ProRegTx {
            collateral_hash: p.collateral_hash,
            collateral_index: p.collateral_index,
            ..pro_reg_base(1, false)
        };
        p.pub_key_operator = rekeyed(&p.pub_key_operator, BlsSchemeKind::Legacy);
        DONE
    }),
    m("1c:3a", &[BAD_PROTX_SHARES_EVO], |c, _| {
        if c.world.evo_shares || is_evo(c)? {
            return None;
        }
        let shares = reg_shares(c)?;
        for s in shares.iter_mut() {
            s.amount *= 4;
        }
        let p = c.reg()?;
        p.mn_type = MnType::Evo;
        p.platform_node_id = Some(PlatformNodeId::from(id(FRESH, 7)));
        p.net_info = net_info(FRESH, 3, true);
        collateral_output(c)?.value = collateral_amount(true);
        DONE
    }),
    m("1c:3b", &[BAD_PROTX_PAYOUTS_EVO], |c, _| {
        if c.world.evo_shares {
            return None;
        }
        let p = c.reg()?;
        if p.mn_type != MnType::Evo || p.version < 3 || !p.shares.is_empty() {
            return None;
        }
        p.payouts = two_payouts(5_000);
        DONE
    }),
    m("1c:6e1", &[BAD_PROTX_COLLATERAL], |c, arg| {
        if arg & 1 == 0 {
            let p = c.reg()?;
            if p.collateral_hash.is_null() {
                return None;
            }
            let at = OutPoint {
                hash: p.collateral_hash,
                index: p.collateral_index,
            };
            c.world.remove_coin(&at)?;
        } else {
            collateral_coin(c)?.amount -= 1;
        }
        DONE
    }),
    m("1c:6e2", &[BAD_PROTX_COLLATERAL_DEST], |c, _| {
        collateral_coin(c)?.script_pubkey = NO_DEST.to_vec();
        DONE
    }),
    m("1c:6e3", &[BAD_PROTX_COLLATERAL_PKH], |c, _| {
        collateral_coin(c)?.script_pubkey = p2sh(id(6, 5));
        DONE
    }),
    m("1c:6i1", &[BAD_PROTX_COLLATERAL_INDEX], |c, arg| {
        collateral_output(c)?;
        let len = c.outputs.len() as u32;
        c.reg()?.collateral_index = len + u32::from(arg);
        DONE
    }),
    m("1c:6i2", &[BAD_PROTX_COLLATERAL], |c, arg| {
        let o = collateral_output(c)?;
        o.value += if arg & 1 == 0 {
            -1
        } else {
            1
        };
        DONE
    }),
    m("1c:6i3", &[BAD_PROTX_SHARES_COLLATERAL_SCRIPT], |c, _| {
        reg_shares(c)?;
        collateral_output(c)?.script_pubkey = p2pkh(id(FRESH, 3)).into();
        DONE
    }),
    m("1c:6i4", &[BAD_PROTX_COLLATERAL_DEST], |c, arg| {
        if reg_shares(c).is_some() {
            return None;
        }
        let script = if arg & 1 == 0 {
            NO_DEST.to_vec()
        } else {
            TEMPLATE.to_vec()
        };
        collateral_output(c)?.script_pubkey = script.into();
        DONE
    }),
    m("1c:7", &[BAD_PROTX_COLLATERAL_REUSE, BAD_PROTX_PAYEE_REUSE], |c, arg| {
        if reg_shares(c).is_some() {
            return None;
        }
        if arg % 4 == 3 {
            // A payout to the collateral's own script, P2PKH or P2SH.
            let script = match collateral_coin(c) {
                Some(coin) => coin.script_pubkey.clone(),
                None => collateral_output(c)?.script_pubkey.as_bytes().to_vec(),
            };
            if !matches!(script.as_slice(), [0x76, 0xa9, 20, .., 0x88, 0xac] | [0xa9, 20, .., 0x87])
                || script.len()
                    != if script[0] == 0xa9 {
                        23
                    } else {
                        25
                    }
            {
                return None;
            }
            let p = c.reg()?;
            if p.version < 3 {
                return None;
            }
            p.payouts.first_mut()?.script_payout = script.into();
            return DONE;
        }
        let key = collateral_key(c)?;
        let p = c.reg()?;
        match arg % 4 {
            0 => p.key_id_owner = key.into(),
            1 => p.key_id_voting = key.into(),
            _ => {
                if p.version < 3 {
                    return None;
                }
                p.payouts.first_mut()?.script_payout = p2pkh(key).into();
            }
        }
        DONE
    }),
    m("1c:8a", &[BAD_PROTX_DUP_NETINFO_ENTRY], |c, _| {
        // REG's address.
        if let Some(s) = legacy_service(c) {
            s.addr.0[15] = REG;
            return DONE;
        }
        core_service(c)?.addr = AddrV2::Ipv4([1, 2, 3, REG]);
        DONE
    }),
    m("1c:9a", &[BAD_PROTX_DUP_KEY], |c, arg| {
        let (i, shares) = share_at(c, arg)?;
        shares.get_mut(i)?.key_id_owner = id(REG, 1).into();
        entity(i)
    }),
    m("1c:9b", &[BAD_PROTX_DUP_KEY], |c, _| {
        reg_shares(c)?;
        c.reg()?.pub_key_operator = member_key(REG);
        DONE
    }),
    m("1c:9c", &[BAD_PROTX_DUP_KEY], |c, arg| {
        let p = c.reg()?;
        if !p.shares.is_empty() {
            return None;
        }
        if arg & 1 == 0 {
            p.key_id_owner = id(REG, 1).into();
        } else {
            let scheme = p.pub_key_operator.scheme();
            p.pub_key_operator = rekeyed(&member_key(REG), scheme);
        }
        DONE
    }),
    m("1c:10", &[BAD_PROTX_DUP_KEY], |c, _| {
        // OLD's point, which OLD's state holds under the legacy scheme.
        if !c.world.v24 {
            return None;
        }
        // A version-1 payload (after 1c:2) carries the key under the legacy scheme.
        let p = c.reg()?;
        p.pub_key_operator = rekeyed(&member_key(OLD), scheme_at(p.version));
        DONE
    }),
    m("1c:11", &[BAD_PROTX_DUP_PLATFORMNODEID], |c, _| {
        let p = c.reg()?;
        if p.mn_type != MnType::Evo {
            return None;
        }
        p.platform_node_id = Some(PlatformNodeId::from(id(EVO, 7)));
        DONE
    }),
    m("1c:13", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.reg()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
    m("1c:14i", &[BAD_PROTX_SIG], |c, _| {
        let p = c.reg()?;
        if !p.collateral_hash.is_null() {
            return None;
        }
        p.vch_sig = vec![0; 65];
        DONE
    }),
];

/// `CProUpRegTx::IsTriviallyValid` and `CheckProUpRegTx` (§3.5).
const PRO_UP_REG: &[Mutation] = &[
    m("3p:payload", &[BAD_PROTX_PAYLOAD], |c, arg| {
        c.up_reg()?;
        bad_payload(c, arg)
    }),
    m("3p:version", &[BAD_PROTX_VERSION], |c, arg| {
        if arg & 1 != 0 {
            c.up_reg()?.version = 4 + u16::from(arg >> 1);
            c.bare = true;
            return DONE;
        }
        if c.world.v24 {
            return None;
        }
        let p = c.up_reg()?;
        if p.version >= 3 {
            return None;
        }
        p.version = 3;
        p.payouts = vec![PayoutShare {
            script_payout: std::mem::take(&mut p.script_payout),
            reward: 10_000,
        }];
        DONE
    }),
    m("3t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.up_reg()?.version = 0;
        c.bare = true;
        DONE
    }),
    m("3t:2", &[BAD_PROTX_MODE], |c, arg| {
        c.up_reg()?.mode = 1 + u16::from(arg);
        DONE
    }),
    m("3t:3", &[BAD_PROTX_KEY_NULL], |c, arg| {
        let p = c.up_reg()?;
        if arg & 1 == 0 {
            p.pub_key_operator = invalid_key();
        } else {
            p.key_id_voting = [0; 20].into();
        }
        DONE
    }),
    m("3c:2", &[BAD_PROTX_HASH], |c, _| {
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("3c:3", &[BAD_PROTX_SHARED_MN], |c, _| {
        c.up_reg()?;
        *c.target()? = TxHash::from(*pro_tx(SHARED).as_bytes());
        DONE
    }),
    m("3c:4", &[BAD_PROTX_VERSION_DOWNGRADE], |c, _| {
        if !c.world.v24 {
            return None;
        }
        // A state of version 2 or more (OLD's is 1).
        let target = c.up_reg()?.pro_tx_hash;
        let state =
            c.world.members.iter().find(|d| *d.pro_tx_hash.as_bytes() == *target.as_bytes());
        if state.is_none_or(|d| d.cold.version < 2) {
            return None;
        }
        let p = c.up_reg()?;
        let payout = p.payouts.first().map(|o| o.script_payout.clone());
        p.version = 1;
        p.pub_key_operator = rekeyed(&p.pub_key_operator, BlsSchemeKind::Legacy);
        if let Some(script) = payout {
            p.script_payout = script;
            p.payouts = Vec::new();
        }
        DONE
    }),
    m("3c:5a", &[BAD_PROTX_DUP_KEY], |c, arg| {
        if !c.world.v24 {
            return None;
        }
        let n = if arg & 1 == 0 {
            OLD
        } else {
            EVO
        };
        let p = c.up_reg()?;
        if p.pro_tx_hash == TxHash::from(*pro_tx(n).as_bytes()) {
            return None;
        }
        let scheme = p.pub_key_operator.scheme();
        p.pub_key_operator = rekeyed(&member_key(n), scheme);
        DONE
    }),
    m("3c:5b", &[BAD_PROTX_NETINFO_VERSION], |c, arg| {
        // OLD's legacy address on port 22, or EVO's platform ports, raised to ExtAddr with the
        // state's key.
        if !c.world.v24 {
            return None;
        }
        let n = if arg & 1 == 0 {
            OLD
        } else {
            EVO
        };
        let p = c.up_reg()?;
        p.pro_tx_hash = TxHash::from(*pro_tx(n).as_bytes());
        p.pub_key_operator = rekeyed(&member_key(n), BlsSchemeKind::Basic);
        if p.version < 3 {
            p.version = 3;
            p.payouts = vec![PayoutShare {
                script_payout: p.script_payout.clone(),
                reward: 10_000,
            }];
            p.script_payout = Vec::new().into();
        }
        DONE
    }),
    m("3c:6", &[BAD_PROTX_PAYEE_REUSE], |c, arg| {
        // The state's owner key (a shared state has none, and Core skips the check then).
        let key = *up_reg_state(c)?.key_id_owner.as_bytes();
        if key == [0; 20] {
            return None;
        }
        match payouts(c)? {
            Payouts::List(list) => {
                let i = index(arg, list.len())?;
                list.get_mut(i)?.script_payout = p2pkh(key).into();
                entity(i)
            }
            Payouts::Script(mut s) => {
                s.set(p2pkh(key));
                entity(0)
            }
        }
    }),
    m("3c:7", &[BAD_PROTX_PAYOUTS_EVO], |c, _| {
        if c.world.evo_shares {
            return None;
        }
        let p = c.up_reg()?;
        if p.version < 3 || p.pro_tx_hash != TxHash::from(*pro_tx(EVO).as_bytes()) {
            return None;
        }
        p.payouts = two_payouts(5_000);
        DONE
    }),
    m("3c:8", &[BAD_PROTX_COLLATERAL], |c, _| {
        let n = up_reg_member(c)?;
        c.world.remove_coin(&member_collateral(n))?;
        DONE
    }),
    m("3c:9", &[BAD_PROTX_COLLATERAL_DEST], |c, _| {
        let n = up_reg_member(c)?;
        c.world.coin_mut(&member_collateral(n))?.script_pubkey = NO_DEST.to_vec();
        DONE
    }),
    m("3c:10", &[BAD_PROTX_COLLATERAL_REUSE, BAD_PROTX_PAYEE_REUSE], |c, arg| {
        // The key the collateral pays (a shared collateral's template script pays none).
        let n = up_reg_member(c)?;
        let key = match c.world.coin_mut(&member_collateral(n))?.script_pubkey.as_slice() {
            [0x76, 0xa9, 20, key @ .., 0x88, 0xac] => <[u8; 20]>::try_from(key).ok()?,
            _ => return None,
        };
        let p = c.up_reg()?;
        if arg & 1 == 0 {
            p.key_id_voting = key.into();
        } else {
            p.payouts.first_mut()?.script_payout = p2pkh(key).into();
        }
        DONE
    }),
    m("3c:11", &[BAD_PROTX_DUP_KEY], |c, _| {
        // Another masternode's key in this payload's scheme: OLD's legacy one at version 1 (the
        // unique property is the serialized key).
        let p = c.up_reg()?;
        let n = if p.version < 2 {
            OLD
        } else {
            EVO
        };
        if p.pro_tx_hash == TxHash::from(*pro_tx(n).as_bytes()) {
            return None;
        }
        let scheme = p.pub_key_operator.scheme();
        p.pub_key_operator = rekeyed(&member_key(n), scheme);
        DONE
    }),
    m("3c:13", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.up_reg()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
];

/// The member a ProUpRegTx updates, if one of [`REG`] and [`EVO`].
fn up_reg_member(c: &mut Case) -> Option<u8> {
    let h = c.up_reg()?.pro_tx_hash;
    [REG, EVO, OLD, SHARED].into_iter().find(|&n| h == TxHash::from(*pro_tx(n).as_bytes()))
}

/// The state of the masternode a ProUpRegTx updates.
fn up_reg_state(c: &mut Case) -> Option<&MnCold> {
    c.up_reg()?;
    target_state(c)
}

/// The world's masternode the payload names.
fn up_member(c: &mut Case) -> Option<u8> {
    let h = *c.target()?;
    [REG, EVO, OLD, SHARED].into_iter().find(|&n| h == TxHash::from(*pro_tx(n).as_bytes()))
}

/// The state of the masternode the payload names.
fn target_state(c: &mut Case) -> Option<&MnCold> {
    let h = *c.target()?;
    let d = c.world.members.iter().find(|d| *d.pro_tx_hash.as_bytes() == *h.as_bytes())?;
    Some(&d.cold)
}

/// The shares of the masternode the payload names (none for one that is not shared).
fn target_shares(c: &mut Case) -> Option<Vec<CollateralShare>> {
    Some(target_state(c)?.shares.clone())
}

/// The dissolution's output for non-actor share `k`, and the actor's (the last).
fn dis_parts(c: &mut Case) -> Option<(usize, usize, bool)> {
    let p = c.dis()?;
    let (actor, sigs) = (usize::from(p.actor_index), p.sigs.len());
    let count = target_shares(c)?.len();
    if count == 0 {
        return None;
    }
    Some((actor, count, sigs == count))
}

/// [`dis_parts`] for a dissolution whose actor is one of the shares: the steps on the non-actor
/// outputs, the actor's output and the bonuses are defined around it.
fn dis_actor(c: &mut Case) -> Option<(usize, usize, bool)> {
    dis_parts(c).filter(|&(actor, count, _)| actor < count)
}

/// A dissolution with an actor output: an actor among the shares, and an output for each share
/// (Core reads the last as the actor's only then).
fn actor_output(c: &mut Case) -> Option<()> {
    let (_, count, _) = dis_actor(c)?;
    (c.outputs.len() == count).then_some(())
}

/// The non-actor shares of the dissolution's actor, in share order: each amount and its
/// pro-rata floor of the penalty the dissolution owes (unilateral and early), and that penalty.
fn dis_floors(c: &mut Case) -> Option<(Vec<(i64, i64)>, i64)> {
    let (actor, _, unanimous) = dis_actor(c)?;
    let state = target_state(c)?;
    let early = 101 - i64::from(state.registered_height) < i64::from(state.early_period_blocks);
    let required = if unanimous || !early {
        0
    } else {
        state.early_penalty
    };
    let non_actor: Vec<i64> = state
        .shares
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != actor)
        .map(|(_, s)| s.amount)
        .collect();
    let total = non_actor.iter().sum();
    let floors = non_actor
        .iter()
        .map(|&a| Some((a, dash_dml::specialtx::shared::pro_rata_floor(required, a, total)?)))
        .collect::<Option<_>>()?;
    Some((floors, required))
}

/// `CProDisTx::IsTriviallyValid`, `CheckProDisTx` and `CheckProDisTxForList` (§3.7).
const PRO_DIS: &[Mutation] = &[
    m("10t:1", &[BAD_PRODIS_VERSION, BAD_PROTX_PAYLOAD], |c, arg| {
        c.dis()?.version = bad_version(arg, 1);
        DONE
    }),
    m("10t:2", &[BAD_PRODIS_SIG_COUNT], |c, arg| {
        let p = c.dis()?;
        let sig = compact_sig()?;
        p.sigs = vec![
            sig;
            if arg & 1 == 0 {
                0
            } else {
                9
            }
        ];
        DONE
    }),
    m("10c:2", &[BAD_PRODIS_TOO_EARLY], |c, _| {
        c.dis()?;
        too_early(c)
    }),
    m("10c:3", &[BAD_PRODIS_HASH], |c, _| {
        c.dis()?;
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("10l:2", &[BAD_PRODIS_NOT_SHARED], |c, _| {
        c.dis()?;
        *c.target()? = TxHash::from(*pro_tx(REG).as_bytes());
        DONE
    }),
    m("10l:3", &[BAD_PRODIS_INPUT], |c, arg| {
        c.dis()?;
        match arg % 3 {
            0 if c.inputs.len() < 2 => c.inputs.push(spend(outpoint([0x7f; 32], 0))),
            0 => {}
            1 => {
                // Another masternode's collateral than the one the payload names.
                let n = if up_member(c)? == REG {
                    EVO
                } else {
                    REG
                };
                c.inputs[0].prevout = member_collateral(n);
            }
            _ => c.inputs[0].script_sig = vec![0x51].into(),
        }
        DONE
    }),
    m("10l:4", &[BAD_PRODIS_ACTOR], |c, arg| {
        let (_, count, _) = dis_parts(c)?;
        c.dis()?.actor_index = (count + usize::from(arg)) as u16;
        DONE
    }),
    m("10l:5", &[BAD_PRODIS_SIG_COUNT], |c, _| {
        let (_, count, _) = dis_parts(c)?;
        if count < 3 {
            return None;
        }
        let sig = compact_sig()?;
        c.dis()?.sigs = vec![sig; 2];
        DONE
    }),
    m("10l:7", &[BAD_PRODIS_PAYEE_COUNT], |c, arg| {
        // Two outputs fewer than the non-actor shares, or one more than all the shares.
        let (_, count, _) = dis_parts(c)?;
        if arg & 1 == 0 {
            c.outputs.truncate(count.saturating_sub(2));
        } else {
            c.outputs.resize(count + 1, out(1, p2pkh(id(FRESH, 8))));
        }
        DONE
    }),
    m("10l:8a", &[BAD_PRODIS_PAYEE], |c, arg| {
        let (_, count, _) = dis_actor(c)?;
        let k = usize::from(arg) % (count - 1);
        c.outputs.get_mut(k)?.script_pubkey = p2pkh(id(FRESH, 9)).into();
        entity(k)
    }),
    m("10l:8b", &[BAD_PRODIS_PENALTY_FLOOR], |c, arg| {
        let (floors, _) = dis_floors(c)?;
        let k = usize::from(arg) % floors.len();
        let (amount, floor) = *floors.get(k)?;
        if floor == 0 {
            return None;
        }
        c.outputs.get_mut(k)?.value = amount + floor - 1;
        entity(k)
    }),
    m("10l:9a", &[BAD_PRODIS_PAYEE], |c, _| {
        actor_output(c)?;
        c.outputs.last_mut()?.script_pubkey = p2pkh(id(FRESH, 9)).into();
        DONE
    }),
    m("10l:9b", &[BAD_PRODIS_ACTOR_OUTPUT_ZERO], |c, _| {
        actor_output(c)?;
        c.outputs.last_mut()?.value = 0;
        DONE
    }),
    m("10l:10", &[BAD_PRODIS_PENALTY_SUM], |c, _| {
        // Every bonus at its floor, which leaves the floors' remainder unpaid.
        let (floors, required) = dis_floors(c)?;
        if floors.iter().map(|(_, f)| f).sum::<i64>() == required {
            return None;
        }
        for (o, (amount, floor)) in c.outputs.iter_mut().zip(&floors) {
            o.value = amount + floor;
        }
        DONE
    }),
    m("10l:11", &[BAD_PRODIS_FEE], |c, _| {
        c.dis()?;
        c.outputs.last_mut()?.value -= MAX_FEE - DIS_FEE + 1;
        DONE
    }),
    m("10l:12", &[BAD_PRODIS_BONUS], |c, _| {
        let (_, _, unanimous) = dis_actor(c)?;
        if unanimous || c.outputs.last()?.value <= PENALTY + 1 {
            return None;
        }
        c.outputs.first_mut()?.value += PENALTY + 1;
        c.outputs.last_mut()?.value -= PENALTY + 1;
        DONE
    }),
];

/// V24 off: the shared types are too early.
fn too_early(c: &mut Case) -> Option<Option<u8>> {
    if !c.world.v24 {
        return None;
    }
    c.world.v24 = false;
    c.world.evo_shares = false;
    DONE
}

/// `CProUpShareTx::IsTriviallyValid` and `CheckProUpShareTx` (§3.8).
const PRO_UP_SHARE: &[Mutation] = &[
    m("11t:1", &[BAD_PROUPSHARE_VERSION, BAD_PROTX_PAYLOAD], |c, arg| {
        c.share()?.version = bad_version(arg, 1);
        DONE
    }),
    m("11t:2", &[BAD_PROUPSHARE_SIG_SIZE], |c, arg| {
        c.share()?.vch_sig = vec![
            0;
            if arg & 1 == 0 {
                64
            } else {
                66
            }
        ];
        DONE
    }),
    m("11t:3", &[BAD_PROUPSHARE_PAYEE], |c, _| {
        c.share()?.script_reward = NO_DEST.to_vec().into();
        DONE
    }),
    m("11c:2", &[BAD_PROUPSHARE_TOO_EARLY], |c, _| {
        c.share()?;
        too_early(c)
    }),
    m("11c:3", &[BAD_PROUPSHARE_HASH], |c, _| {
        c.share()?;
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("11c:4", &[BAD_PROUPSHARE_NOT_SHARED], |c, _| {
        c.share()?;
        *c.target()? = TxHash::from(*pro_tx(REG).as_bytes());
        DONE
    }),
    m("11c:5", &[BAD_PROUPSHARE_INDEX], |c, arg| {
        c.share()?;
        let count = target_shares(c)?.len();
        if count == 0 {
            return None;
        }
        c.share()?.share_index = (count + usize::from(arg)) as u16;
        DONE
    }),
    m("11c:7a", &[BAD_PROUPSHARE_PAYEE_REUSE], |c, _| {
        // The voting key of the masternode the payload names.
        c.share()?;
        let voting = *target_state(c)?.key_id_voting.as_bytes();
        c.share()?.script_reward = p2pkh(voting).into();
        DONE
    }),
    m("11c:7b", &[BAD_PROUPSHARE_PAYEE_REUSE], |c, arg| {
        c.share()?;
        let shares = target_shares(c)?;
        let k = index(arg, shares.len())?;
        let owner = *shares.get(k)?.key_id_owner.as_bytes();
        c.share()?.script_reward = p2pkh(owner).into();
        entity(k)
    }),
    m("11c:8", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.share()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
];

/// `CProUpSharedRegTx::IsTriviallyValid` and `CheckProUpSharedRegTx` (§3.9).
const PRO_UP_SHARED_REG: &[Mutation] = &[
    m("12t:1", &[BAD_PROUPSHAREDREG_VERSION, BAD_PROTX_PAYLOAD], |c, arg| {
        c.shared_reg()?.version = bad_version(arg, 1);
        DONE
    }),
    m("12t:2", &[BAD_PROTX_KEY_NULL], |c, arg| {
        let p = c.shared_reg()?;
        if arg & 1 == 0 {
            p.pub_key_operator = invalid_key();
        } else {
            p.key_id_voting = [0; 20].into();
        }
        DONE
    }),
    m("12t:4", &[BAD_PROUPSHAREDREG_SIG_COUNT], |c, arg| {
        let sig = compact_sig()?;
        c.shared_reg()?.sigs = vec![
            sig;
            if arg & 1 == 0 {
                0
            } else {
                9
            }
        ];
        DONE
    }),
    m("12c:2", &[BAD_PROUPSHAREDREG_TOO_EARLY], |c, _| {
        c.shared_reg()?;
        too_early(c)
    }),
    m("12c:3", &[BAD_PROUPSHAREDREG_HASH], |c, _| {
        c.shared_reg()?;
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("12c:4", &[BAD_PROUPSHAREDREG_NOT_SHARED], |c, _| {
        c.shared_reg()?;
        *c.target()? = TxHash::from(*pro_tx(REG).as_bytes());
        DONE
    }),
    m("12c:5", &[BAD_PROUPSHAREDREG_SIG_COUNT], |c, arg| {
        c.shared_reg()?;
        let count = target_shares(c)?.len();
        if count == 0 {
            return None;
        }
        let sig = compact_sig()?;
        c.shared_reg()?.sigs = vec![
            sig;
            if arg & 1 == 0 {
                count - 1
            } else {
                count + 1
            }
        ];
        DONE
    }),
    m("12c:6", &[BAD_PROTX_DUP_KEY], |c, arg| {
        // Another masternode's key than the one the payload names.
        c.shared_reg()?;
        let n = match (arg & 1, up_member(c)) {
            (0, Some(REG)) => EVO,
            (0, _) => REG,
            _ => OLD,
        };
        c.shared_reg()?.pub_key_operator = rekeyed(&member_key(n), BlsSchemeKind::Basic);
        DONE
    }),
    m("12c:7", &[BAD_PROUPSHAREDREG_PAYEE_REUSE], |c, arg| {
        c.shared_reg()?;
        let shares = target_shares(c)?;
        let k = index(arg >> 1, shares.len())?;
        // Share k's refund key, or its reward key (where it has one).
        let share = shares.get(k)?;
        let script = if arg & 1 == 1 && !share.script_reward.is_empty() {
            &share.script_reward
        } else {
            &share.script_refund
        };
        let key: [u8; 20] = match script.as_bytes() {
            [0x76, 0xa9, 20, key @ .., 0x88, 0xac] => key.try_into().ok()?,
            _ => return None,
        };
        c.shared_reg()?.key_id_voting = key.into();
        entity(k)
    }),
    m("12c:8", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.shared_reg()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
];

/// The catalogue of `tx_type`, in Core's order: the type's steps, then the list and network
/// steps it reaches (ProRegTx and ProUpRegTx).
pub fn catalogue(tx_type: TxType) -> Vec<&'static Mutation> {
    match tx_type {
        TxType::ProviderRegister => PRO_REG
            .iter()
            .chain(PAYOUT_LIST)
            .chain(SHARE_LIST)
            .chain(NET_INFO)
            .chain(PLATFORM_FIELDS)
            .collect(),
        TxType::ProviderUpdateRegistrar => PRO_UP_REG.iter().chain(PAYOUT_LIST).collect(),
        TxType::ProviderDissolve => PRO_DIS.iter().collect(),
        TxType::ProviderUpdateShare => PRO_UP_SHARE.iter().collect(),
        TxType::ProviderUpdateSharedRegistrar => PRO_UP_SHARED_REG.iter().collect(),
        _ => Vec::new(),
    }
}

/// Inputs and outputs no check of the type reads (the n-1 nit): `variation` adds up to two
/// funding inputs and up to two outputs, before or after a registration's collateral output.
/// A dissolution reads all of both, so it is left alone.
pub fn vary(case: &mut Case, variation: u8) {
    if matches!(case.payload, Payload::Dis(_)) {
        return;
    }
    let extra_inputs = usize::from(variation % 3);
    let start = case.inputs.len();
    case.inputs.extend(funding(start + extra_inputs).into_iter().skip(start));
    let extra_outputs = usize::from(variation / 3 % 3);
    let before = variation & 0x80 != 0;
    let count = case.outputs.len() as u32;
    for j in 0..extra_outputs {
        let o = out(COIN + j as i64, p2pkh(id(7, j as u8)));
        if before {
            case.outputs.insert(0, o);
        } else {
            case.outputs.push(o);
        }
    }
    // An internal collateral's index stays on its output, or past the last one; an external
    // one's names a coin, not an output.
    if let Some(p) = case.reg()
        && p.collateral_hash.is_null()
        && (before || p.collateral_index >= count)
    {
        p.collateral_index = p.collateral_index.saturating_add(extra_outputs as u32);
    }
}

/// The input reader: missing bytes read as 0.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn next(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }
}

/// The case `input` describes; `None` for an empty input.
pub fn generate(input: &[u8]) -> Option<Case> {
    if input.is_empty() {
        return None;
    }
    let mut r = Reader(input);
    let world = World::new(r.next());
    let tx_type = TYPES[usize::from(r.next()) % TYPES.len()];
    let mut case = seed(world, tx_type, r.next())?;
    let variation = r.next();
    let count = usize::from(r.next()) % (MAX_MUTATIONS + 1);
    let catalogue = catalogue(tx_type);
    for _ in 0..count {
        let pick = usize::from(r.next());
        let arg = r.next();
        if let Some(mutation) = catalogue.get(pick % catalogue.len().max(1)) {
            mutation.apply(&mut case, arg);
        }
    }
    vary(&mut case, variation);
    Some(case)
}

/// The world's chain parameters, as dash-params builds them.
pub fn params() -> Option<&'static ChainParams> {
    static PARAMS: OnceLock<Option<ChainParams>> = OnceLock::new();
    PARAMS
        .get_or_init(|| {
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
                .arg(ChainOption::TestActivationHeight, V19)
                .arg(ChainOption::Dip3Params, DIP3)
                .build()
                .ok()
        })
        .as_ref()
}

/// The world's chain, as the oracle takes it.
pub fn chain() -> Chain {
    Chain::new(
        "regtest",
        &[&format!("-testactivationheight={V19}"), &format!("-dip3params={DIP3}")],
    )
}

/// The context's first height: a window of two headers ends at the tip (the checks read the
/// tip's height and hash; every input hashes the context's headers, on both sides).
const FIRST: usize = TIP - 1;

/// The context's headers, linked regtest headers from [`FIRST`] to the tip, and the tip's hash.
fn headers() -> Option<&'static ([[u8; 80]; 2], BlockHash)> {
    static HEADERS: OnceLock<Option<([[u8; 80]; 2], BlockHash)>> = OnceLock::new();
    HEADERS
        .get_or_init(|| {
            let mut out = [[0u8; 80]; 2];
            let mut prev = [0x11u8; 32];
            for (i, h) in out.iter_mut().enumerate() {
                h[0..4].copy_from_slice(&4i32.to_le_bytes());
                h[4..36].copy_from_slice(&prev);
                h[68..72]
                    .copy_from_slice(&(1_417_713_337 + 600 * (FIRST + i) as u32).to_le_bytes());
                h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
                prev = *decode_header(&mut &h[..]).ok()?.value().hash().as_bytes();
            }
            Some((out, BlockHash::from(prev)))
        })
        .as_ref()
}

/// Our view of [`headers`].
fn our_headers() -> Option<&'static Headers> {
    static OURS: OnceLock<Option<Headers>> = OnceLock::new();
    OURS.get_or_init(|| Headers::new(FIRST as i32, &headers()?.0)).as_ref()
}

/// A rejection's class (`TxValidationResult`, Core's numbering) and reason.
pub type Reject = (i32, String);

/// Core's verdict and ours on one case; `None` for an acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdicts {
    pub core: Option<Reject>,
    pub ours: Option<Reject>,
}

fn context_coin(c: &ReplayCoin) -> ContextCoin {
    ContextCoin {
        txid: c.txid,
        vout: c.vout,
        height: c.height,
        coinbase: c.coinbase,
        amount: c.amount,
        script_pubkey: c.script_pubkey.clone(),
    }
}

/// The oracle's context of `world`: the headers, the deployments, the coins unchecked, the list.
pub fn context(world: &World) -> Result<Context, Divergence> {
    let (headers, tip) = headers().ok_or(divergence("headers"))?;
    let list = world.list_bytes(*tip).ok_or(divergence("world list"))?;
    Ok(Context::new(
        FIRST as i32,
        headers.to_vec(),
        false,
        world.deployments(),
        1_500_000_000,
        Checkpoints::Disabled,
        CoinsView::Unchecked(world.coins.iter().map(context_coin).collect()),
        CoreList::Supplied(list),
        vec![],
    ))
}

/// Core's verdict and ours on `case`; `Ok(None)` when the oracle refuses the context.
pub fn verdicts(oracle: &Oracle, case: &Case) -> Result<Option<Verdicts>, Divergence> {
    let tx = case.tx();
    let mut raw = Vec::new();
    tx.raw().encode(&mut raw);
    let ctx = context(&case.world)?;
    let core = match oracle.check_special_tx(&chain(), &raw, &ctx, false) {
        Ok(Verdict::Accept) => None,
        Ok(Verdict::Reject(r)) => {
            let ResultCode::Tx(class) = r.result else {
                return Err(divergence("core result class"));
            };
            Some((class, r.reason))
        }
        Err(e) => {
            oracle_failure(&e)?;
            return Ok(None);
        }
    };
    let params = params().ok_or(divergence("params"))?;
    let (_, tip) = headers().ok_or(divergence("headers"))?;
    let headers = our_headers().ok_or(divergence("our headers"))?;
    let our_coins = Coins::new(&case.world.coins, None);
    let list_bytes = case.world.list_bytes(*tip).ok_or(divergence("world list"))?;
    let list =
        core_disk::decode_list_s3(&mut &list_bytes[..]).map_err(|_| divergence("our list"))?;
    let rules = SpecialTxRules {
        v24: case.world.v24,
        evo_shares: case.world.evo_shares,
    };
    let cx = CheckCtx::new(params, headers, &our_coins, rules, &list);
    check(cx.height == TIP as i32 + 1, "height")?;
    let ours =
        cx.check_special_tx(&tx).err().map(|e| (e.result() as i32, e.reject_reason().to_owned()));
    Ok(Some(Verdicts {
        core,
        ours,
    }))
}

/// `diff_check_special_tx`: the generated case through Core's `CheckSpecialTx` and ours.
/// Compared: acceptance, then the class and the reason.
pub fn compare(bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some(case) = generate(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Some(v) = verdicts(&Oracle::new(pin), &case)? else {
        return Ok(Outcome::OutOfDomain);
    };
    match (&v.core, &v.ours) {
        (None, None) => Ok(Outcome::BothAccept),
        (Some(core), Some(ours)) => {
            check(core.0 == ours.0, "result class")?;
            check(core.1 == ours.1, "reason")?;
            Ok(Outcome::BothReject)
        }
        _ => Err(divergence("verdict")),
    }
}

/// The inputs of `diff_check_special_tx`'s public seeds: every seed of five worlds (none, V24,
/// V24 with EVO_SHARES, V24 early with three shares, early with three shares), and each mutation
/// of the catalogue with four arguments, on the first seed of a V24 world that carries it.
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let worlds = [
        0,
        FLAG_V24,
        FLAG_V24 | FLAG_EVO_SHARES,
        FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES,
        FLAG_EARLY | FLAG_THREE_SHARES,
    ];
    let mut out = Vec::new();
    for (t, &tx_type) in TYPES.iter().enumerate() {
        for flags in worlds {
            for variant in variants(tx_type) {
                out.push(vec![flags, t as u8, variant, 0, 0]);
            }
        }
        for (pick, mutation) in catalogue(tx_type).iter().enumerate() {
            for arg in 0..4u8 {
                let carrier = variants(tx_type).find(|&variant| {
                    seed(World::new(FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES), tx_type, variant)
                        .is_some_and(|mut c| mutation.apply(&mut c, arg))
                });
                if let Some(variant) = carrier {
                    let flags = FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES;
                    out.push(vec![flags, t as u8, variant, 0, 1, pick as u8, arg]);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A block at height 101 on the world's tip: a crafted case's coinbase with its cbTx height
/// moved to 101 (Core's check loop compares it with the block's), then `txs`.
pub fn block(txs: &[Tx]) -> Option<Vec<u8>> {
    let crafted = crate::dml_apply::cases().first()?;
    let (header, crafted_txs) = crate::dml_apply::split_block(&crafted.block)?;
    let mut coinbase = Transaction::decode(&mut &crafted_txs.first()?[..]).ok()?;
    coinbase.extra_payload.get_mut(2..6)?.copy_from_slice(&(TIP as i32 + 1).to_le_bytes());
    let mut header = header.to_vec();
    header.get_mut(4..36)?.copy_from_slice(headers()?.1.as_bytes());
    let mut encoded = vec![Vec::new()];
    coinbase.encode(&mut encoded[0]);
    for tx in txs {
        let mut raw = Vec::new();
        tx.raw().encode(&mut raw);
        encoded.push(raw);
    }
    let refs: Vec<&[u8]> = encoded.iter().map(Vec::as_slice).collect();
    Some(crate::dml_apply::assemble(&header, &refs))
}

/// `txs` in a [`block`] over `world`, through `diff_dml_block`'s comparison (Core's check loop,
/// build and `ProcessBlock` against ours).
pub fn compare_in_block(
    oracle: &Oracle,
    world: &World,
    txs: &[Tx],
) -> Result<crate::dml_block::Detailed, Divergence> {
    let block = block(txs).ok_or(divergence("block"))?;
    let params = params().ok_or(divergence("params"))?;
    crate::dml_block::compare_dml_block_detailed(oracle, &chain(), params, &context(world)?, &block)
}

/// A plain transaction spending a funding outpoint and paying `outputs`.
pub fn plain_tx(outputs: Vec<TxOut>) -> Tx {
    Tx::new(Transaction {
        version: 2,
        tx_type: TxType::Spend,
        inputs: vec![spend(outpoint([0x6f; 32], 0))],
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    })
}

//! EV1-3's L1 target (EV1 §6.2): `diff_check_special_tx`, Core's `CheckSpecialTx` (the oracle's
//! transaction entry) against dash-dml's `check_special_tx_inner` through [`CheckCtx`], on
//! generated registrations, service, registrar and revocation updates, shared-masternode
//! transactions, coinbase commitments and asset locks (types 1 to 5, 8 and 10 to 12, EV1-4 adding 2
//! and 4, EV1-5b-1 5 and 8) over a synthetic regtest tip.
//!
//! The world: regtest with V19 at height 100 and DIP3 enforced from 2, the tip at 100. Its list
//! holds four masternodes, each with its collateral in the coins view: [`REG`] (regular, version
//! 2), [`EVO`] (Evo, version 2), [`OLD`] (version 1, a legacy key and a legacy address on port
//! 22, which ExtNetInfo refuses) and [`SHARED`] (version 3, shared, registered at 90). The view
//! also holds an external collateral per type for registrations. The flags byte turns on V24 and
//! EVO_SHARES, puts the shared masternode in its early period and gives it three shares instead
//! of two; it turns on `check_sigs` (each case then signed by the keys Core reads, which the
//! world's secrets give) and moves V19 past the tip (the legacy global flag, with V24 off). A
//! world before V19 has seeds of version 1 only: its other members' states keep their versions.
//! Its cases that check signatures are counted apart (D30, [`Case::legacy_flag`]). Regtest's
//! DIP0008 is active from height 1 and V20 from 432, so a coinbase commitment of block 101 takes
//! version 2 in every world; an asset lock takes version 2 under V24 only.
//!
//! A case starts from a seed, a transaction of one type both sides accept, applies up to three
//! mutations from the type's [`catalogue`], each labelled with the EV1 §3 step it violates
//! (D19), and then varies the inputs and outputs no check of the type reads (the EV1-2 Opus-h
//! n-1 nit). Compared: Core's verdict and ours, the class and the reason.
//!
//! Step labels are `table:step` with the step numbers of EV1 §3: `1t` and `1c` for
//! `CProRegTx::IsTriviallyValid` and `CheckProRegTx` (§3.3), `3t` and `3c` (§3.5), `10t`, `10c`
//! and `10l` for ProDisTx's `IsTriviallyValid`, `CheckProDisTx` and `CheckProDisTxForList`
//! (§3.7), `11t`/`11c` (§3.8), `12t`/`12c` (§3.9), `2p`, `2t` and `2c` for ProUpServTx (§3.4),
//! `4p`, `4t` and `4c` for ProUpRevTx (§3.6), `5c` for the dispatcher's type-5 arm (`coinbase`
//! and `payload`, §3.2 3.8 and 3.9) and `CheckCbTx` (§3.10), `8c` for `CheckAssetLockTx`
//! (§3.12), `pl` and `sl` for the payout and share lists (§3.14), `nt` for
//! `IsNetInfoTriviallyValid` (numbered by its three rejects) and `nf` for
//! `CheckProviderNetworkFields` (§3.15). A step that loops over a list takes an entity: the
//! payout or share index, an asset lock's output index (Core's loop over `vout`) or credit
//! output index. Steps no wire transaction reaches here are not in the catalogue;
//! `contrib/node/ev1-pairs.toml` lists them with Core's reason.
//!
//! The context holds the last two headers only: the checks read the tip's height and hash.
//!
//! Input layout: byte 0 the flags, byte 1 the type, byte 2 the seed, byte 3 the variation,
//! byte 4 the mutation count (mod 4), then a (pick, argument) byte pair per mutation.

use std::sync::{Arc, OnceLock};

use dash_consensus::primitives::{Tx, decode_block};
use dash_consensus::subsidy::COIN;
use dash_consensus::{MAX_MONEY, decode_header};
use dash_dml::codec::core_disk;
use dash_dml::specialtx::SpecialTxRules;
use dash_dml::specialtx::reasons::*;
use dash_dml::specialtx::sig::{
    inputs_hash_of, make_sign_string, payload_sig_hash, pro_dis_sign_hash, shared_reg_consent_hash,
};
use dash_dml::testkit::replay::{Coins, Headers, ReplayCoin};
use dash_dml::testkit::{dmn_with, fresh, list_disk_bytes, operator_key, pro_tx};
use dash_dml::{Dmn, MnCold, MnList, OperatorKey};
use dash_evo::spork::{key_id_of, message_hash, sign_compact};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, DEPLOYMENT_EVO_SHARES, DEPLOYMENT_V24,
    MnList as CoreList, Oracle, Pin, ResultCode, ThresholdState, Verdict,
};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::{
    BlsEagerSig, BlsLazyPk, BlsScChia, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey,
    BlsSigId,
};
use dash_primitives::{
    AddrV1, AddrV2, AssetLock, BlockHash, COMPACT_SIG_LEN, CoinbaseCommitment, CollateralShare,
    CollateralSig, CompactSig, InputsHash, MerkleRoot, MnType, NIEntry, NIPurpose, NetInfo,
    NetInfoV1, NetInfoV2, OutPoint, PayoutShare, PlatformNodeId, ProDisTx, ProRegTx, ProTxVersion,
    ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx, RevocationReason,
    ServiceV1, ServiceV2, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use secp256k1::{PublicKey, SecretKey};

use crate::dml::legacy;
use crate::dml_checks::CheckCtx;
use crate::inputs::oracle_failure;
use crate::{Compared, Divergence, Outcome, check, divergence};

/// `-testactivationheight` and `-dip3params` of the world's chain.
const V19: &str = "v19@100";
/// V19 past the tip, for a world of the legacy global flag.
const V19_LATE: &str = "v19@200";
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
pub const FLAG_SIGS: u8 = 16;
pub const FLAG_LEGACY: u8 = 32;

/// The types the target generates, in the order byte 1 picks them.
pub const TYPES: [TxType; 9] = [
    TxType::ProviderRegister,
    TxType::ProviderUpdateService,
    TxType::ProviderUpdateRegistrar,
    TxType::ProviderUpdateRevoke,
    TxType::ProviderDissolve,
    TxType::ProviderUpdateShare,
    TxType::ProviderUpdateSharedRegistrar,
    TxType::CoinbaseCommitment,
    TxType::AssetLock,
];

/// The most mutations a case applies.
pub const MAX_MUTATIONS: usize = 3;

/// The world's ECDSA identities: identity `b` (1 to 0xff) is the secret of 31 bytes `b` after a
/// zero byte, and its compressed key id.
fn identities() -> &'static [(SecretKey, [u8; 20])] {
    static IDS: OnceLock<Vec<(SecretKey, [u8; 20])>> = OnceLock::new();
    IDS.get_or_init(|| {
        (1..=0xff)
            .map(|b| {
                let mut secret = [b; 32];
                secret[0] = 0;
                #[allow(clippy::expect_used)]
                let key = SecretKey::from_secret_bytes(secret).expect("a scalar below the order");
                (key, key_id_of(&PublicKey::from_secret_key(&key), true))
            })
            .collect()
    })
}

/// The key id of identity `b`.
fn identity(b: u8) -> [u8; 20] {
    identities()[usize::from(b) - 1].1
}

/// The secret behind `key_id`, or [`forger`]'s for a key id no identity has.
fn secret_of(key_id: &[u8; 20]) -> SecretKey {
    identities().iter().find(|(_, id)| id == key_id).map_or_else(forger, |(key, _)| *key)
}

/// A secret behind no key id of the world (its first byte is not zero): it signs a forged
/// signature.
fn forger() -> SecretKey {
    #[allow(clippy::expect_used)]
    SecretKey::from_secret_bytes([0xfe; 32]).expect("a scalar below the order")
}

/// An identity of masternode or role `n`: key ids and payout keys, identity `n << 4 | role`.
fn id(n: u8, role: u8) -> [u8; 20] {
    identity(n << 4 | role)
}

/// The identities of share `i` of the shared masternode (`base` 0xc0) or of a shared
/// registration (`base` 0xd0): role 0 the refund key, 1 the reward key, 2 the owner key.
fn share_id(base: u8, i: usize, role: u8) -> [u8; 20] {
    identity(base | (i as u8) << 2 | role)
}

/// The compact signature of `hash` by `key`.
fn ecdsa_sig(hash: &[u8; 32], key: &SecretKey) -> Option<CompactSig> {
    CompactSig::decode(&mut &sign_compact(hash, key, true)[..]).ok()
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

/// The key material of the operator keys: [`member_key`]`(n)`'s, then [`fresh_key`]`(n)`'s.
const OPERATOR_IKM: [u8; 7] = [0x81, 0x82, 0x83, 0x84, 0x91, 0x92, 0x93];

/// The signature of `hash` under the global flag (`legacy`) by the operator behind `key`, one of
/// the world's operator keys; `forged`, or a key no operator of the world has, signs with a
/// fresh key instead.
fn operator_sig(
    key: &OperatorKey,
    hash: &[u8; 32],
    legacy: bool,
    forged: bool,
) -> Option<BlsEagerSig> {
    let point = key.clone().get();
    let ikm = OPERATOR_IKM
        .into_iter()
        .find(|&ikm| !forged && operator_key(ikm, BlsSchemeKind::Basic).get() == point)
        .unwrap_or(0x91);
    let basic = BlsSecretKey::<BlsScIetf>::from_ikm(&[ikm; 32]).ok()?;
    let (bytes, scheme) = if legacy {
        let chia = BlsSecretKey::<BlsScChia>::from_bytes(&basic.to_bytes()).ok()?;
        (chia.sign_digest(hash).to_bytes(), BlsSchemeKind::Legacy)
    } else {
        (basic.sign_with(hash, BlsSigId::Basic).to_bytes(), BlsSchemeKind::Basic)
    };
    BlsEagerSig::decode(&bytes, BlsSchemeSource::Version(scheme)).ok()
}

/// The operator signature an unsigned case carries: a valid point, signing nothing it is
/// checked against.
fn unchecked_operator_sig() -> BlsEagerSig {
    static SIG: OnceLock<BlsEagerSig> = OnceLock::new();
    SIG.get_or_init(|| {
        #[allow(clippy::expect_used)]
        operator_sig(&fresh_key(1), &[0; 32], false, true).expect("a signature of the fresh key")
    })
    .clone()
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
    /// V19 active at the block (the basic global flag); off in a [`FLAG_LEGACY`] world.
    pub v19: bool,
    /// `check_sigs`.
    pub sigs: bool,
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
        let v19 = flags & FLAG_LEGACY == 0;
        let v24 = v19 && flags & FLAG_V24 != 0;
        let three_shares = flags & FLAG_THREE_SHARES != 0;
        let early = flags & FLAG_EARLY != 0;
        let member = |n: u8, mn_type: dash_dml::MnType, f: &dyn Fn(&mut MnCold)| {
            dmn_with(n, mn_type, |d, c| {
                d.collateral = member_collateral(n);
                // REG's operator reward lets a service update set an operator payee.
                d.operator_reward = if n == REG {
                    1_000
                } else {
                    0
                };
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
            v19,
            sigs: flags & FLAG_SIGS != 0,
            v24,
            evo_shares: v24 && flags & FLAG_EVO_SHARES != 0,
            early,
            three_shares,
            members: vec![reg, evo, old, shared],
            coins,
        }
    }

    /// The flags [`Case::fingerprint`] records.
    fn recorded_flags(&self) -> [bool; 5] {
        [self.v24, self.evo_shares, self.early, self.v19, self.sigs]
    }

    /// Whether `other` is this world for [`Case::sign`] and [`Case::fingerprint`]: every flag,
    /// the same member states (shared, not only equal) and the same coins.
    fn identical(&self, other: &World) -> bool {
        self.recorded_flags() == other.recorded_flags()
            && self.three_shares == other.three_shares
            && self.members.len() == other.members.len()
            && self.members.iter().zip(&other.members).all(|(a, b)| Arc::ptr_eq(a, b))
            && format!("{:?}", self.coins) == format!("{:?}", other.coins)
    }

    /// `GetMaxFromDeployment`'s ProTx version: 1 before V19, 3 with V24, else 2.
    pub fn max_version(&self) -> u16 {
        match (self.v19, self.v24) {
            (false, _) => 1,
            (true, false) => 2,
            (true, true) => 3,
        }
    }

    /// The masternode with `pro_tx_hash`.
    fn named(&self, pro_tx_hash: &TxHash) -> Option<&Dmn> {
        self.members
            .iter()
            .find(|d| d.pro_tx_hash.as_bytes() == pro_tx_hash.as_bytes())
            .map(|d| &**d)
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
#[derive(Clone, PartialEq)]
pub enum Payload {
    Reg(ProRegTx),
    UpServ(ProUpServTx),
    UpReg(ProUpRegTx),
    UpRev(ProUpRevTx),
    Dis(ProDisTx),
    Share(ProUpShareTx),
    SharedReg(ProUpSharedRegTx),
    CbTx(CoinbaseCommitment),
    AssetLock(AssetLock),
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
    /// The payload's last byte dropped, which leaves a CbTx or asset-lock payload short.
    cut: bool,
    /// A version no `ProTxVersion` names (0, or past 3), written over the payload's first two
    /// bytes; the payload keeps the fields of the nearest defined version. A mutation that sets
    /// the payload's version clears it.
    raw_version: Option<u16>,
    /// Signed with a key the check does not read (in a world that checks signatures).
    forged: bool,
    /// The operator signs under the scheme the global flag does not name (quirk 21).
    pub other_scheme: bool,
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
            cut: false,
            raw_version: None,
            forged: false,
            other_scheme: false,
            applied: Vec::new(),
        }
    }

    /// The version of a type 1 to 4 payload as encoded (1 for the other types).
    fn version(&self) -> u16 {
        let typed = match &self.payload {
            Payload::Reg(p) => p.version,
            Payload::UpServ(p) => p.version,
            Payload::UpReg(p) => p.version,
            Payload::UpRev(p) => p.version,
            Payload::Dis(_)
            | Payload::Share(_)
            | Payload::SharedReg(_)
            | Payload::CbTx(_)
            | Payload::AssetLock(_) => return 1,
        };
        self.raw_version.unwrap_or(u16::from(typed))
    }

    /// A type 1 to 4 payload at `version`, which no `ProTxVersion` names: the fields of
    /// `LegacyBls` for 0 and of `ExtAddr` past it, which the encoder lays out as they were (it
    /// branches on versions 2 and 3). `None` for the other types.
    fn set_bad_version(&mut self, version: u16) -> Option<()> {
        let layout = if version == 0 {
            ProTxVersion::LegacyBls
        } else {
            ProTxVersion::ExtAddr
        };
        match &mut self.payload {
            Payload::Reg(p) => p.version = layout,
            Payload::UpServ(p) => p.version = layout,
            Payload::UpReg(p) => p.version = layout,
            Payload::UpRev(p) => p.version = layout,
            _ => return None,
        }
        self.raw_version = Some(version);
        Some(())
    }

    pub fn tx_type(&self) -> TxType {
        match self.payload {
            Payload::Reg(_) => TxType::ProviderRegister,
            Payload::UpServ(_) => TxType::ProviderUpdateService,
            Payload::UpReg(_) => TxType::ProviderUpdateRegistrar,
            Payload::UpRev(_) => TxType::ProviderUpdateRevoke,
            Payload::Dis(_) => TxType::ProviderDissolve,
            Payload::Share(_) => TxType::ProviderUpdateShare,
            Payload::SharedReg(_) => TxType::ProviderUpdateSharedRegistrar,
            Payload::CbTx(_) => TxType::CoinbaseCommitment,
            Payload::AssetLock(_) => TxType::AssetLock,
        }
    }

    /// A case that checks signatures under the legacy global flag (D30): counted apart.
    pub fn legacy_flag(&self) -> bool {
        self.world.sigs && !self.world.v19
    }

    /// What Core reads of the case: the transaction, the deployments, the global flag,
    /// `check_sigs`, the list and the coins.
    pub fn fingerprint(&self) -> Option<Vec<u8>> {
        self.fingerprint_of(&self.tx())
    }

    /// [`Case::fingerprint`] with the case's transaction, `tx`, already built.
    fn fingerprint_of(&self, tx: &Tx) -> Option<Vec<u8>> {
        let w = &self.world;
        let mut bytes = vec![
            u8::from(w.v24),
            u8::from(w.evo_shares),
            u8::from(w.early),
            u8::from(w.v19),
            u8::from(w.sigs),
        ];
        tx.raw().encode(&mut bytes);
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

    fn up_serv(&mut self) -> Option<&mut ProUpServTx> {
        match &mut self.payload {
            Payload::UpServ(p) => Some(p),
            _ => None,
        }
    }

    fn up_rev(&mut self) -> Option<&mut ProUpRevTx> {
        match &mut self.payload {
            Payload::UpRev(p) => Some(p),
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

    fn cbtx(&mut self) -> Option<&mut CoinbaseCommitment> {
        match &mut self.payload {
            Payload::CbTx(p) => Some(p),
            _ => None,
        }
    }

    fn lock(&mut self) -> Option<&mut AssetLock> {
        match &mut self.payload {
            Payload::AssetLock(p) => Some(p),
            _ => None,
        }
    }

    /// The masternode a type 2, 3, 4, 10, 11 or 12 payload names.
    fn target(&mut self) -> Option<&mut TxHash> {
        match &mut self.payload {
            Payload::Reg(_) | Payload::CbTx(_) | Payload::AssetLock(_) => None,
            Payload::UpServ(p) => Some(&mut p.pro_tx_hash),
            Payload::UpReg(p) => Some(&mut p.pro_tx_hash),
            Payload::UpRev(p) => Some(&mut p.pro_tx_hash),
            Payload::Dis(p) => Some(&mut p.pro_tx_hash),
            Payload::Share(p) => Some(&mut p.pro_tx_hash),
            Payload::SharedReg(p) => Some(&mut p.pro_tx_hash),
        }
    }

    /// The payload's inputs hash, for the types that carry one.
    fn inputs_hash(&mut self) -> Option<&mut InputsHash> {
        match &mut self.payload {
            Payload::Reg(p) => Some(&mut p.inputs_hash),
            Payload::UpServ(p) => Some(&mut p.inputs_hash),
            Payload::UpReg(p) => Some(&mut p.inputs_hash),
            Payload::UpRev(p) => Some(&mut p.inputs_hash),
            Payload::Dis(_) | Payload::CbTx(_) | Payload::AssetLock(_) => None,
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
    /// it), signed in a world that checks signatures, then encoded as the extra payload. A bare
    /// payload is not signed: it keeps its first two bytes, the version, which every payload
    /// encodes before its signature fields, so signing would change none of its bytes (BL-548).
    pub fn tx(&self) -> Tx {
        let mut case = self.unsigned();
        if case.world.sigs && !case.bare {
            case.sign();
        }
        case.encoded()
    }

    /// The case [`Case::tx`] signs: the payload's inputs hash set from the inputs, unless a
    /// mutation set it.
    fn unsigned(&self) -> Case {
        let mut case = self.clone();
        if !case.keep_inputs_hash {
            let hash = InputsHash::from_lendian(inputs_hash_of(&case.inputs));
            if let Some(h) = case.inputs_hash() {
                *h = hash;
            }
        }
        case
    }

    /// Whether `other` gives the same transaction ([`Case::tx`]) and the same
    /// [`Case::fingerprint`]: [`Case::same_signed`]'s answer, signing only when it turns on the
    /// signatures (BL-548). In order:
    ///
    /// 1. the flags the fingerprints record differ: not the same;
    /// 2. two payloads of one type and signature count, neither of which writes or cuts bytes
    ///    after encoding (`bare`, `cut`, `raw_version`), where this case with `other`'s signature
    ///    fields encodes unlike `other`: not the same, in any two worlds. [`Case::sign`] writes
    ///    only signature fields, whatever the world, and the encoding is a sequence of
    ///    self-delimiting fields whose presence no signature field sets, so the two signed
    ///    transactions differ outside their signatures too;
    /// 3. any other payloads, or two worlds that are not [`World::identical`]:
    ///    [`Case::same_signed`];
    /// 4. no signatures checked: the unsigned transactions, which are the ones [`Case::tx`]
    ///    encodes;
    /// 5. the same payload, inputs, outputs, trailing byte, `forged` and `other_scheme`, in an
    ///    identical world: the same. [`Case::sign`] is a function of exactly those and the
    ///    world (its `v19` flag, members and coins). Its ECDSA signer, `sign_compact`, takes
    ///    RFC 6979's nonce with no extra data (secp256k1 0.33's
    ///    `sign_ecdsa_recoverable`, `secp256k1_nonce_function_rfc6979`). The crate re-randomizes
    ///    its context before each signature with a seed that mixes in `rand::random()`, but
    ///    libsecp256k1 spends that seed only on blinding the base-point multiplication
    ///    (`secp256k1_context_randomize` in `secp256k1.h`), which leaves the product and so the
    ///    signature unchanged. BLS signing (`operator_sig`, dash-pkc's `sign_digest`) is the
    ///    secret scalar times the message's hash on the curve: deterministic. The l1_specialtx
    ///    test `same_as_agrees_with_the_signed_comparison` signs cases twice and compares;
    /// 6. otherwise (signature fields differ and might be overwritten alike):
    ///    [`Case::same_signed`].
    pub fn same_as(&self, other: &Case) -> bool {
        let (x, y) = (self.unsigned(), other.unsigned());
        if x.world.recorded_flags() != y.world.recorded_flags() {
            return false;
        }
        let plain = |c: &Case| !c.bare && !c.cut && c.raw_version.is_none();
        let resigned = (plain(&x) && plain(&y)).then(|| x.with_sigs_of(&y)).flatten();
        let bytes = |c: &Case| {
            let mut bytes = Vec::new();
            c.encoded().raw().encode(&mut bytes);
            bytes
        };
        if resigned.as_ref().is_some_and(|r| bytes(r) != bytes(&y)) {
            return false;
        }
        if resigned.is_none() || !x.world.identical(&y.world) {
            return self.same_signed(other);
        }
        if !x.world.sigs {
            return bytes(&x) == bytes(&y);
        }
        let alike = x.payload == y.payload
            && x.inputs == y.inputs
            && x.outputs == y.outputs
            && x.trailing == y.trailing
            && x.forged == y.forged
            && x.other_scheme == y.other_scheme;
        if alike && bytes(&x) == bytes(&y) {
            return true;
        }
        self.same_signed(other)
    }

    /// [`Case::same_as`]'s reference: both transactions signed and encoded, then both
    /// fingerprints (of the same transactions: signing is deterministic, step 5 there).
    pub fn same_signed(&self, other: &Case) -> bool {
        let (x, y) = (self.tx(), other.tx());
        let bytes = |tx: &Tx| {
            let mut bytes = Vec::new();
            tx.raw().encode(&mut bytes);
            bytes
        };
        bytes(&x) == bytes(&y) && self.fingerprint_of(&x) == other.fingerprint_of(&y)
    }

    /// A copy of this case with `other`'s signature fields, when the payloads have one type and
    /// the same signature counts.
    fn with_sigs_of(&self, other: &Case) -> Option<Case> {
        let mut case = self.clone();
        match (&mut case.payload, &other.payload) {
            (Payload::Reg(p), Payload::Reg(q)) if p.join_sigs.len() == q.join_sigs.len() => {
                p.join_sigs.clone_from(&q.join_sigs);
                p.vch_sig = q.vch_sig.clone();
            }
            (Payload::UpServ(p), Payload::UpServ(q)) => p.sig = q.sig.clone(),
            (Payload::UpReg(p), Payload::UpReg(q)) => p.vch_sig = q.vch_sig.clone(),
            (Payload::UpRev(p), Payload::UpRev(q)) => p.sig = q.sig.clone(),
            (Payload::Dis(p), Payload::Dis(q)) if p.sigs.len() == q.sigs.len() => {
                p.sigs.clone_from(&q.sigs);
            }
            (Payload::Share(p), Payload::Share(q)) => p.vch_sig.clone_from(&q.vch_sig),
            (Payload::SharedReg(p), Payload::SharedReg(q)) if p.sigs.len() == q.sigs.len() => {
                p.sigs.clone_from(&q.sigs);
            }
            (Payload::CbTx(_), Payload::CbTx(_))
            | (Payload::AssetLock(_), Payload::AssetLock(_)) => {}
            _ => return None,
        }
        Some(case)
    }

    fn encoded(&self) -> Tx {
        let mut extra_payload = Vec::new();
        match &self.payload {
            Payload::Reg(p) => p.encode(&mut extra_payload),
            Payload::UpServ(p) => p.encode(&mut extra_payload),
            Payload::UpReg(p) => p.encode(&mut extra_payload),
            Payload::UpRev(p) => p.encode(&mut extra_payload),
            Payload::Dis(p) => p.encode(&mut extra_payload),
            Payload::Share(p) => p.encode(&mut extra_payload),
            Payload::SharedReg(p) => p.encode(&mut extra_payload),
            Payload::CbTx(p) => p.encode(&mut extra_payload),
            Payload::AssetLock(p) => p.encode(&mut extra_payload),
        }
        if let Some(version) = self.raw_version {
            extra_payload[..2].copy_from_slice(&version.to_le_bytes());
        }
        if self.bare {
            extra_payload.truncate(2);
        }
        if self.cut {
            extra_payload.pop();
        }
        extra_payload.extend(self.trailing);
        Tx::new(Transaction {
            version: 3,
            tx_type: self.tx_type(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            lock_time: 0,
            extra_payload,
        })
    }

    /// The signatures Core checks, by the keys it reads them against (`forged`: one by a key it
    /// does not): an external collateral's key over the sign string, each share owner over the
    /// consent hash, the owner key, the dissolution's signers (every owner, or the actor), a
    /// share's owner, every share owner, and the operator under the global flag (under the other
    /// scheme with `other_scheme`). A payload whose signature fields an earlier step refuses keeps
    /// them.
    fn sign(&mut self) {
        let unsigned = self.encoded();
        let (forged, legacy) = (self.forged, self.world.v19 == self.other_scheme);
        let world = self.world.clone();
        let params = params_of(&world);
        let state = |h: &TxHash| world.named(h).map(|d| d.cold.clone());
        let owner = |share: Option<&CollateralShare>, forge: bool| {
            share.filter(|_| !forge).map_or_else(forger, |s| secret_of(s.key_id_owner.as_bytes()))
        };
        let collateral = collateral_key(self);
        match &mut self.payload {
            Payload::Reg(p) if !p.shares.is_empty() => {
                let Some(consent) = shared_reg_consent_hash(p, &unsigned) else {
                    return;
                };
                let shares = p.shares.clone();
                for (i, sig) in p.join_sigs.iter_mut().enumerate() {
                    let key = owner(shares.get(i), forged && i == 0);
                    if let Some(s) = ecdsa_sig(&consent, &key) {
                        *sig = s;
                    }
                }
            }
            Payload::Reg(p) if !p.collateral_hash.is_null() => {
                let Some(message) = params.and_then(|params| make_sign_string(p, params)) else {
                    return;
                };
                let key = match collateral.filter(|_| !forged) {
                    Some(key_id) => secret_of(&key_id),
                    None => forger(),
                };
                p.vch_sig = sig_of(&sign_compact(&message_hash(&message), &key, true));
            }
            Payload::Reg(_) => {}
            Payload::UpServ(p) => {
                let (Some(state), Some(hash)) = (state(&p.pro_tx_hash), payload_sig_hash(p)) else {
                    return;
                };
                if let Some(sig) = operator_sig(&state.pub_key_operator, &hash, legacy, forged) {
                    p.sig = sig;
                }
            }
            Payload::UpReg(p) => {
                let Some(hash) = payload_sig_hash(p) else {
                    return;
                };
                let key = match state(&p.pro_tx_hash).filter(|_| !forged) {
                    Some(s) => secret_of(s.key_id_owner.as_bytes()),
                    None => forger(),
                };
                p.vch_sig = sig_of(&sign_compact(&hash, &key, true));
            }
            Payload::UpRev(p) => {
                let (Some(state), Some(hash)) = (state(&p.pro_tx_hash), payload_sig_hash(p)) else {
                    return;
                };
                if let Some(sig) = operator_sig(&state.pub_key_operator, &hash, legacy, forged) {
                    p.sig = sig;
                }
            }
            Payload::Dis(p) => {
                let shares = state(&p.pro_tx_hash).map(|s| s.shares.clone()).unwrap_or_default();
                let count = p.sigs.len();
                let hash = pro_dis_sign_hash(p, &unsigned, count as u8);
                let actor = usize::from(p.actor_index);
                for (i, sig) in p.sigs.iter_mut().enumerate() {
                    let signer = if count == 1 {
                        actor
                    } else {
                        i
                    };
                    let key = owner(shares.get(signer), forged && i == 0);
                    if let Some(s) = ecdsa_sig(&hash, &key) {
                        *sig = s;
                    }
                }
            }
            Payload::Share(p) => {
                let shares = state(&p.pro_tx_hash).map(|s| s.shares.clone()).unwrap_or_default();
                let Some(hash) = payload_sig_hash(p).filter(|_| p.vch_sig.len() == COMPACT_SIG_LEN)
                else {
                    return;
                };
                let key = owner(shares.get(usize::from(p.share_index)), forged);
                p.vch_sig = sig_of(&sign_compact(&hash, &key, true));
            }
            Payload::SharedReg(p) => {
                let shares = state(&p.pro_tx_hash).map(|s| s.shares.clone()).unwrap_or_default();
                let Some(hash) = payload_sig_hash(p) else {
                    return;
                };
                for (i, sig) in p.sigs.iter_mut().enumerate() {
                    let key = owner(shares.get(i), forged && i == 0);
                    if let Some(s) = ecdsa_sig(&hash, &key) {
                        *sig = s;
                    }
                }
            }
            Payload::CbTx(_) | Payload::AssetLock(_) => {}
        }
    }
}

/// The ProRegTx a seed starts from: fresh keys, a version-3 payout list or a version-2 payout
/// script, the network info of its version, no collateral yet.
fn pro_reg_base(version: u16, evo: bool) -> ProRegTx {
    let payout = p2pkh(id(FRESH, 4));
    ProRegTx {
        version: pro_tx_version(version),
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
        vch_sig: CollateralSig::default(),
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
        version: pro_tx_version(version),
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
        vch_sig: sig_of(&[0; 65]),
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

/// The masternodes a ProUpServTx or ProUpRevTx seed names, by its variant's low two bits.
const UPDATED: [u8; 4] = [REG, EVO, OLD, SHARED];

/// The version of a type 2 or 4 seed naming `n`: the world's maximum, or with `other` 2 under
/// V24 ([`OLD`]: 1, or with `other` 2 from V19, its scheme migration). `None` for a member other
/// than [`OLD`] before V19, whose states are above the world's maximum.
fn update_version(world: &World, n: u8, other: bool) -> Option<u16> {
    let max = world.max_version();
    Some(match n {
        OLD if other => max.min(2),
        OLD => 1,
        _ if !world.v19 => return None,
        EVO => max,
        _ if other && max == 3 => 2,
        _ => max,
    })
}

/// The member numbered by `pro_tx_hash`.
fn member_of(pro_tx_hash: &TxHash) -> Option<u8> {
    [REG, EVO, OLD, SHARED]
        .into_iter()
        .find(|&n| *pro_tx_hash == TxHash::from(*pro_tx(n).as_bytes()))
}

/// A ProUpServTx at `version` naming masternode `n`, at its address `n` (Evo with `evo`, with
/// platform node id `n`), with an operator payee when `payee`.
fn pro_up_serv(n: u8, version: u16, evo: bool, payee: bool) -> ProUpServTx {
    ProUpServTx {
        version: pro_tx_version(version),
        mn_type: if evo {
            MnType::Evo
        } else {
            MnType::Regular
        },
        pro_tx_hash: TxHash::from(*pro_tx(n).as_bytes()),
        net_info: net_info(n, version, evo),
        script_operator_payout: if payee {
            p2pkh(id(FRESH, 11))
        } else {
            Vec::new()
        }
        .into(),
        inputs_hash: InputsHash::from_lendian([0; 32]),
        platform_node_id: evo.then(|| PlatformNodeId::from(id(n, 7))),
        platform_p2p_port: (evo && version < 3).then_some(26_656),
        platform_http_port: (evo && version < 3).then_some(1_443),
        sig: unchecked_operator_sig(),
    }
}

/// `p` rebuilt at `version` (and type `evo`, regular at version 1), keeping its masternode and
/// operator payee: the fields of that version and type at the masternode's address.
fn reversioned(p: &ProUpServTx, version: u16, evo: bool) -> ProUpServTx {
    let n = member_of(&p.pro_tx_hash).unwrap_or(FRESH);
    let evo = evo && version >= 2;
    ProUpServTx {
        pro_tx_hash: p.pro_tx_hash,
        script_operator_payout: p.script_operator_payout.clone(),
        ..pro_up_serv(n, version, evo, false)
    }
}

/// A ProUpServTx seed. `variant`: bits 0–1 the masternode ([`UPDATED`]), bit 2 the other
/// version ([`update_version`]), bit 3 an operator payee (for [`REG`], whose state has an
/// operator reward).
fn seed_pro_up_serv(world: World, variant: u8) -> Option<Case> {
    let n = UPDATED[usize::from(variant & 3)];
    let version = update_version(&world, n, variant & 4 != 0)?;
    let p = pro_up_serv(n, version, n == EVO, variant & 8 != 0 && n == REG);
    Some(Case::new(world, Payload::UpServ(p), funding(1), Vec::new()))
}

/// A ProUpRevTx seed. `variant`: bits 0–1 the masternode ([`UPDATED`]), bit 2 the other
/// version ([`update_version`]), bits 3–4 the reason.
fn seed_pro_up_rev(world: World, variant: u8) -> Option<Case> {
    let n = UPDATED[usize::from(variant & 3)];
    let p = ProUpRevTx {
        version: pro_tx_version(update_version(&world, n, variant & 4 != 0)?),
        pro_tx_hash: TxHash::from(*pro_tx(n).as_bytes()),
        reason: RevocationReason::from_base(u16::from(variant >> 3 & 3)),
        inputs_hash: InputsHash::from_lendian([0; 32]),
        sig: unchecked_operator_sig(),
    };
    Some(Case::new(world, Payload::UpRev(p), funding(1), Vec::new()))
}

/// 2c:10 on the oracle (EV1-4, Opus r1 n-4; the world's one Evo cannot reach it, so
/// `ev1-pairs.toml` lists it elsewhere): under V24, REG made an Evo holding a platform node id,
/// and EVO's service update naming that id (`held`) or keeping its own.
pub fn serv_platform_node_id_case(held: bool) -> Option<Case> {
    let mut world = World::new(FLAG_V24);
    let node_id = PlatformNodeId::from(id(FRESH, 9));
    let slot = world.members.iter_mut().find(|d| d.pro_tx_hash == pro_tx(REG))?;
    let reg = slot.clone();
    *slot = dmn_with(REG, dash_dml::MnType::Evo, |d, c| {
        let mn_type = d.mn_type;
        *d = (*reg).clone();
        d.mn_type = mn_type;
        *c = (*reg.cold).clone();
        c.platform_node_id = node_id;
        c.platform_p2p_port = 26_656;
        c.platform_http_port = 1_443;
    });
    // Variant 1 names EVO at the world's version.
    let mut case = seed_pro_up_serv(world, 1)?;
    if held {
        case.up_serv()?.platform_node_id = Some(node_id);
    }
    Some(case)
}

/// `netinfo_add_entry`'s ProUpServ leg (EV1 §6.5, D29; EV1-4): under V24, masternode REG (or EVO,
/// with `evo`) at `state_version`, named by a ProUpServTx at `version` carrying `payload_net_info`,
/// so that Core runs `ApplyProUpServTx`'s migration of the payload's network info from `version`
/// to `max(state_version, version)` (§3.4 step 7). `None` when no list holds that state (as
/// [`migration_case`]), when the network info is not of `version`'s kind, or when an earlier step
/// refuses the versions (a downgrade to 1, an Evo below ExtAddr).
pub fn serv_migration_case(
    evo: bool,
    state_version: u16,
    payload_net_info: NetInfo,
    version: u16,
) -> Option<Case> {
    let extended = matches!(payload_net_info, NetInfo::Extended(_));
    if !(1..=3).contains(&state_version)
        || !(1..=3).contains(&version)
        || extended != (version >= 3)
        || (state_version >= 2 && version == 1)
        || (evo && (state_version < 2 || version < 3))
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
        c.net_info = Arc::new(net_info(n, state_version, evo));
        c.pub_key_operator = rekeyed(&member_key(n), scheme_at(state_version));
        if state_version >= 3 {
            // `SetStateVersion`'s ExtAddr state: the payout script as a one-entry list.
            c.payouts = vec![PayoutShare {
                script_payout: std::mem::take(&mut c.script_payout).into(),
                reward: 10_000,
            }];
            c.platform_p2p_port = 0;
            c.platform_http_port = 0;
        }
    })?;
    world.list_bytes(headers()?.1)?;
    let mut p = pro_up_serv(n, version, evo, false);
    p.net_info = payload_net_info;
    Some(Case::new(world, Payload::UpServ(p), funding(1), Vec::new()))
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

/// `CCbTx::Version::MERKLE_ROOT_QUORUMS`, the one version the world's block 101 takes: DIP0008
/// is active at the tip and V20 is not after it (`cbtx.cpp:35-43`).
const CBTX_VERSION: u16 = 2;

/// A CbTx seed: a coinbase (one input, spending the null outpoint) whose payload is at
/// [`CBTX_VERSION`] and names block 101. `variant`: bit 0 merkle roots that are not null, bit 1
/// an empty coinbase script instead of the height's push.
fn seed_cbtx(world: World, variant: u8) -> Option<Case> {
    let root = |b: u8| {
        MerkleRoot::from(if variant & 1 == 1 {
            [b; 32]
        } else {
            [0; 32]
        })
    };
    let p = CoinbaseCommitment {
        version: CBTX_VERSION,
        height: TIP as i32 + 1,
        merkle_root_mn_list: root(0x5a),
        merkle_root_quorums: Some(root(0x5b)),
        best_cl_height_diff: None,
        best_cl_signature: None,
        credit_pool_balance: None,
        merkle_root_asset_unlocks: None,
    };
    let mut input = spend(outpoint([0; 32], u32::MAX));
    if variant & 2 == 0 {
        input.script_sig = vec![0x01, TIP as u8 + 1].into();
    }
    let outputs = vec![out(500 * COIN, p2pkh(id(FRESH, 12)))];
    Some(Case::new(world, Payload::CbTx(p), vec![input], outputs))
}

/// `OP_RETURN OP_0`: the script of an asset lock's return output.
const EMPTY_RETURN: [u8; 2] = [0x6a, 0x00];

/// Whether `script` starts with `OP_RETURN`: the outputs Core's asset-lock loop reads.
fn is_return(script: &[u8]) -> bool {
    script.first() == Some(&0x6a)
}

/// An AssetLock seed: credit outputs of 1 DASH and a duff more each, the return output paying
/// their sum, and two change outputs. `variant`: bit 0 version 2 (under V24 only), bits 1–2 the
/// credit outputs less one, bit 3 P2SH credit outputs at the even indexes (version 2 only), bits
/// 4–5 the return output's index (0 to 2).
fn seed_asset_lock(world: World, variant: u8) -> Option<Case> {
    let v2 = variant & 1 == 1;
    let p2sh_credits = variant & 8 != 0;
    let at = usize::from(variant >> 4);
    if (v2 && !world.v24) || (p2sh_credits && !v2) || at > 2 {
        return None;
    }
    let count = 1 + usize::from(variant >> 1 & 3);
    let credit_outputs: Vec<TxOut> = (0..count)
        .map(|i| {
            let key = id(8, i as u8);
            let script = if p2sh_credits && i % 2 == 0 {
                p2sh(key)
            } else {
                p2pkh(key)
            };
            out(COIN + i as i64, script)
        })
        .collect();
    let total = credit_outputs.iter().map(|o| o.value).sum();
    let mut outputs: Vec<TxOut> =
        (0..2u8).map(|j| out(2 * COIN + i64::from(j), p2pkh(id(7, 8 + j)))).collect();
    outputs.insert(at, out(total, EMPTY_RETURN.to_vec()));
    let p = AssetLock {
        version: 1 + u8::from(v2),
        credit_outputs,
    };
    Some(Case::new(world, Payload::AssetLock(p), funding(1), outputs))
}

/// The seed variants of `tx_type` that differ (the rest repeat them).
pub fn variants(tx_type: TxType) -> std::ops::Range<u8> {
    match tx_type {
        TxType::ProviderRegister => 0..0x40,
        TxType::ProviderUpdateService => 0..0x10,
        TxType::ProviderUpdateRegistrar => 0..0x12,
        TxType::ProviderUpdateRevoke => 0..0x20,
        TxType::ProviderDissolve => 0..6,
        TxType::ProviderUpdateShare => 0..3,
        TxType::CoinbaseCommitment => 0..4,
        TxType::AssetLock => 0..0x30,
        _ => 0..1,
    }
}

/// The seed of `tx_type` (one of [`TYPES`]) in `world`; `None` for a ProTx whose version is past
/// the world's maximum (a world before V19 takes version 1 only), and for a variant the world
/// does not take.
pub fn seed(world: World, tx_type: TxType, variant: u8) -> Option<Case> {
    match tx_type {
        TxType::CoinbaseCommitment => return seed_cbtx(world, variant),
        TxType::AssetLock => return seed_asset_lock(world, variant),
        _ => {}
    }
    let max = world.max_version();
    let case = match tx_type {
        TxType::ProviderRegister => seed_pro_reg(world, variant),
        TxType::ProviderUpdateService => seed_pro_up_serv(world, variant),
        TxType::ProviderUpdateRegistrar => seed_pro_up_reg(world, variant),
        TxType::ProviderUpdateRevoke => seed_pro_up_rev(world, variant),
        TxType::ProviderDissolve => seed_pro_dis(world, variant),
        TxType::ProviderUpdateShare => seed_pro_up_share(world, variant),
        TxType::ProviderUpdateSharedRegistrar => seed_pro_up_shared_reg(world, variant),
        _ => None,
    }?;
    (case.version() <= max).then_some(case)
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
            let max = c.world.max_version();
            c.reg()?;
            Some(c.version() > max)
        },
        "2p:version" => |c| {
            let max = c.world.max_version();
            c.up_serv()?;
            Some(c.version() > max)
        },
        "3p:version" => |c| {
            let max = c.world.max_version();
            c.up_reg()?;
            Some(c.version() > max)
        },
        "4p:version" => |c| {
            let max = c.world.max_version();
            c.up_rev()?;
            Some(c.version() > max)
        },
        "2c:4" => |c| {
            let p = c.up_serv()?.clone();
            let state = c.world.named(&p.pro_tx_hash)?;
            Some(
                matches!(p.mn_type, MnType::Regular | MnType::Evo)
                    && p.mn_type.to_base() != state.mn_type.0,
            )
        },
        "2c:5" => |c| {
            let v24 = c.world.v24;
            let p = c.up_serv()?.clone();
            let legacy = c.version() == 1;
            Some(v24 && legacy && c.world.named(&p.pro_tx_hash)?.cold.version >= 2)
        },
        "2c:6" => |c| {
            let v24 = c.world.v24;
            let p = c.up_serv()?;
            Some(v24 && p.mn_type == MnType::Evo && p.version < ProTxVersion::ExtAddr)
        },
        "1t:2" => |c| {
            let bare = c.bare;
            let p = c.reg()?;
            Some(!bare && p.mn_type == MnType::Evo && p.version < ProTxVersion::BasicBls)
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
            let legacy = c.version() == 1;
            let p = c.reg()?;
            Some(v24 && legacy && p.mn_type != MnType::Evo && p.shares.is_empty())
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
            c.up_reg()?;
            let (version, state) = (c.version(), up_reg_state(c)?.version);
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
            Some(
                v24 && (n == OLD || n == EVO)
                    && state_version < 3
                    && p.version >= ProTxVersion::ExtAddr
                    && kept,
            )
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
        "8c:9" => |c| {
            // The return amount is the first return output's value, 0 with none.
            let credits: i128 = c.lock()?.credit_outputs.iter().map(|o| i128::from(o.value)).sum();
            let returned = c.outputs.iter().find(|o| is_return(o.script_pubkey.as_bytes()));
            Some(credits != i128::from(returned.map_or(0, |o| o.value)))
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
/// A ProRegTx, ProUpServTx, ProUpRegTx or ProUpRevTx payload Core's decode refuses: a byte after
/// it, or (all fields written) a version its reader stops at.
fn bad_payload(c: &mut Case, arg: u8) -> Option<Option<u8>> {
    if arg & 1 == 0 {
        c.trailing = Some(arg >> 1);
        return DONE;
    }
    c.set_bad_version(bad_version(arg >> 1, 3))?;
    DONE
}

/// The payload version `version`, one of 1 to 3.
fn pro_tx_version(version: u16) -> ProTxVersion {
    #[allow(clippy::expect_used)] // the builders' versions are 1 to 3
    ProTxVersion::try_from(version).expect("a defined version")
}

/// A provider signature (`CollateralSig`, `OwnerSig`) holding `bytes`, as the decoder reads it.
fn sig_of<T: BaseCodec>(bytes: &[u8]) -> T {
    let mut encoded = Vec::new();
    bytes.to_vec().encode(&mut encoded);
    #[allow(clippy::expect_used)] // the decoder reads a length-prefixed vector
    T::decode(&mut encoded.as_slice()).expect("a signature of any length decodes")
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
            if p.version >= ProTxVersion::ExtAddr {
                Payouts::List(&mut p.payouts)
            } else {
                Payouts::Script(ScriptSlot::Reg(p))
            }
        }
        Payload::UpReg(p) => {
            if p.version >= ProTxVersion::ExtAddr {
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

/// The network fields of a ProRegTx or ProUpServTx, which `CheckProviderNetworkFields` reads.
struct NetFields<'a> {
    mn_type: MnType,
    version: u16,
    net_info: &'a mut NetInfo,
    node_id: &'a mut Option<PlatformNodeId>,
    p2p_port: &'a mut Option<u16>,
    http_port: &'a mut Option<u16>,
}

fn net_fields(case: &mut Case) -> Option<NetFields<'_>> {
    Some(match &mut case.payload {
        Payload::Reg(p) => NetFields {
            mn_type: p.mn_type,
            version: u16::from(p.version),
            net_info: &mut p.net_info,
            node_id: &mut p.platform_node_id,
            p2p_port: &mut p.platform_p2p_port,
            http_port: &mut p.platform_http_port,
        },
        Payload::UpServ(p) => NetFields {
            mn_type: p.mn_type,
            version: u16::from(p.version),
            net_info: &mut p.net_info,
            node_id: &mut p.platform_node_id,
            p2p_port: &mut p.platform_p2p_port,
            http_port: &mut p.platform_http_port,
        },
        _ => return None,
    })
}

/// The ProRegTx's or ProUpServTx's network info, as an ExtNetInfo's entries.
fn ext_entries(case: &mut Case) -> Option<&mut Vec<(NIPurpose, Vec<NIEntry>)>> {
    match net_fields(case)?.net_info {
        NetInfo::Extended(n) => Some(&mut n.entries),
        _ => None,
    }
}

/// The core service of the ProRegTx's or ProUpServTx's network info.
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

/// A legacy network info's service port and address, for an old-version ProRegTx or
/// ProUpServTx.
fn legacy_service(case: &mut Case) -> Option<&mut ServiceV1> {
    match net_fields(case)?.net_info {
        NetInfo::Legacy(NetInfoV1(s)) => Some(s),
        _ => None,
    }
}

fn is_evo(case: &mut Case) -> Option<bool> {
    Some(net_fields(case)?.mn_type == MnType::Evo)
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

/// `CheckProviderNetworkFields`' platform steps (§3.15 4–6), on an Evo ProRegTx or ProUpServTx.
const PLATFORM_FIELDS: &[Mutation] = &[
    m("nf:4", &[BAD_PROTX_PLATFORM_NODEID], |c, _| {
        let f = net_fields(c)?;
        if f.mn_type != MnType::Evo {
            return None;
        }
        *f.node_id = Some(PlatformNodeId::from([0; 20]));
        DONE
    }),
    m("nf:6b", &[BAD_PROTX_PLATFORM_P2P_PORT, BAD_PROTX_PLATFORM_HTTP_PORT], |c, arg| {
        let f = net_fields(c)?;
        if f.mn_type != MnType::Evo || f.version >= 3 {
            return None;
        }
        if arg & 1 == 0 {
            *f.p2p_port = Some(9_999);
        } else {
            *f.http_port = Some(9_999);
        }
        DONE
    }),
    m("nf:6c", &[BAD_PROTX_PLATFORM_DUP_PORTS], |c, arg| {
        let f = net_fields(c)?;
        if f.mn_type != MnType::Evo || f.version >= 3 {
            return None;
        }
        match arg % 3 {
            0 => *f.http_port = *f.p2p_port,
            1 => *f.p2p_port = Some(PORT),
            _ => *f.http_port = Some(PORT),
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
            c.reg()?;
            c.set_bad_version(4 + u16::from(arg >> 1))?;
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
        c.raw_version = None;
        DONE
    }),
    m("1t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.reg()?;
        c.set_bad_version(0)?;
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
        if !p.shares.is_empty() || p.version < ProTxVersion::ExtAddr {
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
        if p.mn_type != MnType::Evo || p.version < ProTxVersion::ExtAddr || !p.shares.is_empty() {
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
            if p.version < ProTxVersion::ExtAddr {
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
                if p.version < ProTxVersion::ExtAddr {
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
            // A key held in this payload's scheme: OLD's legacy one at version 1 (the unique
            // property is the serialized key).
            let n = if p.version < ProTxVersion::BasicBls {
                OLD
            } else {
                REG
            };
            let scheme = p.pub_key_operator.scheme();
            p.pub_key_operator = rekeyed(&member_key(n), scheme);
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
        p.pub_key_operator = rekeyed(&member_key(OLD), scheme_at(u16::from(p.version)));
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
        p.vch_sig = sig_of(&[0; 65]);
        DONE
    }),
    m("1c:14e", &[BAD_PROTX_SIG], |c, _| {
        let p = c.reg()?;
        if p.collateral_hash.is_null() || !p.shares.is_empty() {
            return None;
        }
        forge(c)
    }),
    m("1c:14s", &[BAD_PROTX_SHARES_SIG], |c, _| {
        reg_shares(c)?;
        forge(c)
    }),
];

/// A signature by a key the check does not read, in a world that checks signatures.
fn forge(c: &mut Case) -> Option<Option<u8>> {
    if !c.world.sigs {
        return None;
    }
    c.forged = true;
    DONE
}

/// `CProUpRegTx::IsTriviallyValid` and `CheckProUpRegTx` (§3.5).
const PRO_UP_REG: &[Mutation] = &[
    m("3p:payload", &[BAD_PROTX_PAYLOAD], |c, arg| {
        c.up_reg()?;
        bad_payload(c, arg)
    }),
    m("3p:version", &[BAD_PROTX_VERSION], |c, arg| {
        if arg & 1 != 0 {
            c.up_reg()?;
            c.set_bad_version(4 + u16::from(arg >> 1))?;
            c.bare = true;
            return DONE;
        }
        if c.world.v24 {
            return None;
        }
        let p = c.up_reg()?;
        if p.version >= ProTxVersion::ExtAddr {
            return None;
        }
        p.version = ProTxVersion::ExtAddr;
        p.payouts = vec![PayoutShare {
            script_payout: std::mem::take(&mut p.script_payout),
            reward: 10_000,
        }];
        c.raw_version = None;
        DONE
    }),
    m("3t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.up_reg()?;
        c.set_bad_version(0)?;
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
        p.version = ProTxVersion::LegacyBls;
        p.pub_key_operator = rekeyed(&p.pub_key_operator, BlsSchemeKind::Legacy);
        if let Some(script) = payout {
            p.script_payout = script;
            p.payouts = Vec::new();
        }
        c.raw_version = None;
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
        if p.version < ProTxVersion::ExtAddr {
            p.version = ProTxVersion::ExtAddr;
            p.payouts = vec![PayoutShare {
                script_payout: p.script_payout.clone(),
                reward: 10_000,
            }];
            p.script_payout = Vec::new().into();
            c.raw_version = None;
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
        if p.version < ProTxVersion::ExtAddr
            || p.pro_tx_hash != TxHash::from(*pro_tx(EVO).as_bytes())
        {
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
        let n = if p.version < ProTxVersion::BasicBls {
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
    m("3c:14", &[BAD_PROTX_SIG], |c, _| {
        c.up_reg()?;
        forge(c)
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
    m("10l:13", &[BAD_PRODIS_SIG], |c, _| {
        c.dis()?;
        forge(c)
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
    m("11c:9", &[BAD_PROUPSHARE_SIG], |c, _| {
        c.share()?;
        forge(c)
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
    m("12c:9", &[BAD_PROUPSHAREDREG_SIG], |c, _| {
        c.shared_reg()?;
        forge(c)
    }),
];

/// `CProUpServTx::IsTriviallyValid` and `CheckProUpServTx` (§3.4), the network steps after them.
const PRO_UP_SERV: &[Mutation] = &[
    m("2p:payload", &[BAD_PROTX_PAYLOAD], |c, arg| {
        c.up_serv()?;
        bad_payload(c, arg)
    }),
    m("2p:version", &[BAD_PROTX_VERSION], |c, arg| {
        if arg & 1 != 0 {
            c.up_serv()?;
            c.set_bad_version(4 + u16::from(arg >> 1))?;
            c.bare = true;
            return DONE;
        }
        // Every field of the version past the world's maximum.
        let max = c.world.max_version();
        if max >= 3 {
            return None;
        }
        let p = c.up_serv()?;
        *p = reversioned(p, max + 1, p.mn_type == MnType::Evo);
        c.raw_version = None;
        DONE
    }),
    m("2t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.up_serv()?;
        c.set_bad_version(0)?;
        c.bare = true;
        DONE
    }),
    m("2t:3", &[BAD_PROTX_TYPE], |c, arg| {
        let p = c.up_serv()?;
        if p.version < ProTxVersion::BasicBls {
            return None;
        }
        p.mn_type = MnType::from_base(2 + u16::from(arg));
        p.platform_node_id = None;
        p.platform_p2p_port = None;
        p.platform_http_port = None;
        DONE
    }),
    m("2t:5", &[BAD_PROTX_NETINFO_EMPTY], |c, _| {
        match &mut c.up_serv()?.net_info {
            NetInfo::Legacy(NetInfoV1(s)) => {
                *s = ServiceV1 {
                    addr: AddrV1([0; 16]),
                    port: 0,
                };
            }
            NetInfo::Extended(n) => n.entries.clear(),
        }
        DONE
    }),
    m("2c:3", &[BAD_PROTX_HASH], |c, _| {
        c.up_serv()?;
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("2c:4", &[BAD_PROTX_TYPE_MISMATCH], |c, _| {
        // The type the masternode is not, with that type's fields.
        let target = c.up_serv()?.pro_tx_hash;
        let evo = c.world.named(&target)?.mn_type.0 != MnType::Evo.to_base();
        let p = c.up_serv()?;
        if p.version < ProTxVersion::BasicBls || !matches!(p.mn_type, MnType::Regular | MnType::Evo)
        {
            return None;
        }
        *p = reversioned(p, u16::from(p.version), evo);
        DONE
    }),
    m("2c:5", &[BAD_PROTX_VERSION_DOWNGRADE], |c, _| {
        if !c.world.v24 {
            return None;
        }
        // An Evo masternode only once the payload is regular (2c:4): Core compares the types first.
        let p = c.up_serv()?.clone();
        let state = c.world.named(&p.pro_tx_hash)?;
        if state.cold.version < 2
            || (state.mn_type.0 == MnType::Evo.to_base() && p.mn_type != MnType::Regular)
        {
            return None;
        }
        let p = c.up_serv()?;
        *p = reversioned(p, 1, false);
        DONE
    }),
    m("2c:6", &[BAD_PROTX_VERSION_DISALLOWED], |c, _| {
        if !c.world.v24 {
            return None;
        }
        let p = c.up_serv()?;
        if p.mn_type != MnType::Evo || p.version < ProTxVersion::BasicBls {
            return None;
        }
        *p = reversioned(p, 2, true);
        DONE
    }),
    m("2c:7", &[BAD_PROTX_NETINFO_VERSION], |c, _| {
        // A legacy address ExtNetInfo refuses (port 22), which the state's version (ExtAddr)
        // raises to ExtNetInfo.
        if !c.world.v24 {
            return None;
        }
        let target = c.up_serv()?.pro_tx_hash;
        if c.world.named(&target)?.cold.version < 3 {
            return None;
        }
        legacy_service(c)?.port = 22;
        DONE
    }),
    m("2c:8", &[BAD_PROTX_DUP_KEY], |c, _| {
        // OLD raised from the legacy scheme while REG holds its key under the basic one.
        if !c.world.v24 {
            return None;
        }
        let p = c.up_serv()?;
        if member_of(&p.pro_tx_hash) != Some(OLD) || p.version < ProTxVersion::BasicBls {
            return None;
        }
        let key = rekeyed(&member_key(OLD), BlsSchemeKind::Basic);
        c.world.edit_member(REG, |s| s.pub_key_operator = key)?;
        DONE
    }),
    m("2c:9a", &[BAD_PROTX_DUP_NETINFO_ENTRY], |c, _| {
        // Another masternode's address: REG's, or EVO's for REG.
        let n = match member_of(&c.up_serv()?.pro_tx_hash) {
            Some(REG) => EVO,
            _ => REG,
        };
        if let Some(s) = legacy_service(c) {
            s.addr.0[15] = n;
            return DONE;
        }
        core_service(c)?.addr = AddrV2::Ipv4([1, 2, 3, n]);
        DONE
    }),
    m("2c:11a", &[BAD_PROTX_OPERATOR_PAYEE], |c, _| {
        let target = c.up_serv()?.pro_tx_hash;
        if c.world.named(&target)?.operator_reward != 0 {
            return None;
        }
        c.up_serv()?.script_operator_payout = p2pkh(id(FRESH, 11)).into();
        DONE
    }),
    m("2c:11b", &[BAD_PROTX_OPERATOR_PAYEE], |c, _| {
        let target = c.up_serv()?.pro_tx_hash;
        if c.world.named(&target)?.operator_reward == 0 {
            return None;
        }
        c.up_serv()?.script_operator_payout = NO_DEST.to_vec().into();
        DONE
    }),
    m("2c:12", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.up_serv()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
    m("2c:13", &[BAD_PROTX_SIG], |c, _| {
        c.up_serv()?;
        forge(c)
    }),
];

/// `CProUpRevTx::IsTriviallyValid` and `CheckProUpRevTx` (§3.6).
const PRO_UP_REV: &[Mutation] = &[
    m("4p:payload", &[BAD_PROTX_PAYLOAD], |c, arg| {
        c.up_rev()?;
        bad_payload(c, arg)
    }),
    m("4p:version", &[BAD_PROTX_VERSION], |c, arg| {
        if arg & 1 != 0 {
            c.up_rev()?;
            c.set_bad_version(4 + u16::from(arg >> 1))?;
            c.bare = true;
            return DONE;
        }
        let max = c.world.max_version();
        if max >= 3 {
            return None;
        }
        c.up_rev()?.version = pro_tx_version(max + 1);
        c.raw_version = None;
        DONE
    }),
    m("4t:1", &[BAD_PROTX_VERSION], |c, _| {
        c.up_rev()?;
        c.set_bad_version(0)?;
        c.bare = true;
        DONE
    }),
    m("4t:2", &[BAD_PROTX_REASON], |c, arg| {
        c.up_rev()?.reason = RevocationReason::from_base(4 + u16::from(arg));
        DONE
    }),
    m("4c:2", &[BAD_PROTX_HASH], |c, _| {
        c.up_rev()?;
        *c.target()? = TxHash::from([0x5e; 32]);
        DONE
    }),
    m("4c:3", &[BAD_PROTX_VERSION_DOWNGRADE], |c, _| {
        if !c.world.v24 {
            return None;
        }
        let target = c.up_rev()?.pro_tx_hash;
        if c.world.named(&target)?.cold.version < 2 {
            return None;
        }
        c.up_rev()?.version = ProTxVersion::LegacyBls;
        c.raw_version = None;
        DONE
    }),
    m("4c:4", &[BAD_PROTX_INPUTS_HASH], |c, _| {
        c.up_rev()?.inputs_hash = InputsHash::from_lendian([0x99; 32]);
        c.keep_inputs_hash = true;
        DONE
    }),
    m("4c:5", &[BAD_PROTX_SIG], |c, _| {
        c.up_rev()?;
        forge(c)
    }),
];

/// A CbTx or asset-lock payload Core's decode refuses: a byte after it, or its last byte dropped.
fn bad_bytes(c: &mut Case, arg: u8) -> Option<Option<u8>> {
    if arg & 1 == 0 {
        c.trailing = Some(arg >> 1);
    } else {
        c.cut = true;
    }
    DONE
}

/// The dispatcher's type-5 arm (§3.2 3.8, 3.9) and `CheckCbTx` (§3.10), in Core's order. Step
/// 5 (V24 with V20) is not here: the world's V20 is not active, so step 4 refuses a version of
/// 4 or more first.
// Core: evo/specialtxman.cpp:247-256; evo/cbtx.cpp:24-55
const CB_TX: &[Mutation] = &[
    m("5c:coinbase", &[BAD_CBTX_INVALID], |c, arg| {
        c.cbtx()?;
        if arg & 1 == 0 {
            c.inputs.first_mut()?.prevout = outpoint([0x7e; 32], 0);
        } else if c.inputs.len() < 2 {
            c.inputs.push(spend(outpoint([0x7f; 32], 0)));
        }
        DONE
    }),
    m("5c:payload", &[BAD_CBTX_PAYLOAD], |c, arg| {
        c.cbtx()?;
        bad_bytes(c, arg)
    }),
    m("5c:1", &[BAD_CBTX_VERSION], |c, arg| {
        c.cbtx()?.version = if arg & 1 == 0 {
            0
        } else {
            5 + u16::from(arg >> 1)
        };
        DONE
    }),
    m("5c:2", &[BAD_CBTX_HEIGHT], |c, arg| {
        c.cbtx()?.height = if arg & 1 == 0 {
            TIP as i32
        } else {
            TIP as i32 + 2 + i32::from(arg >> 1)
        };
        DONE
    }),
    m("5c:3", &[BAD_CBTX_VERSION], |c, _| {
        c.cbtx()?.version = 1;
        DONE
    }),
    m("5c:4", &[BAD_CBTX_VERSION], |c, arg| {
        c.cbtx()?.version = 3 + u16::from(arg & 1);
        DONE
    }),
];

/// Output `arg % len` of an asset lock: an element of Core's loop over `vout`.
fn lock_output(c: &mut Case, arg: u8) -> Option<(usize, &mut TxOut)> {
    c.lock()?;
    let i = index(arg, c.outputs.len())?;
    Some((i, c.outputs.get_mut(i)?))
}

/// Credit output `arg % len` of an asset lock: an element of Core's loop over the credit
/// outputs.
fn credit_output(c: &mut Case, arg: u8) -> Option<(usize, &mut TxOut)> {
    let credits = &mut c.lock()?.credit_outputs;
    let k = index(arg, credits.len())?;
    Some((k, credits.get_mut(k)?))
}

/// `CheckAssetLockTx` (§3.12) past its type check, in Core's order. The output steps (2a–2c)
/// set any output, so that two of them can fall on adjacent outputs.
// Core: evo/assetlocktx.cpp:29-89
const ASSET_LOCK: &[Mutation] = &[
    m("8c:2a", &[BAD_ASSETLOCKTX_NON_EMPTY_RETURN], |c, arg| {
        let (i, o) = lock_output(c, arg >> 1)?;
        o.script_pubkey = if arg & 1 == 0 {
            vec![0x6a]
        } else {
            vec![0x6a, 0x01, 0x00]
        }
        .into();
        entity(i)
    }),
    m("8c:2b", &[BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE], |c, arg| {
        let (i, o) = lock_output(c, arg >> 1)?;
        o.script_pubkey = EMPTY_RETURN.to_vec().into();
        o.value = if arg & 1 == 0 {
            0
        } else {
            MAX_MONEY + 1
        };
        entity(i)
    }),
    m("8c:2c", &[BAD_ASSETLOCKTX_MULTIPLE_RETURN], |c, arg| {
        // An output that is not a return output made a second one, of 1 duff.
        let (i, o) = lock_output(c, arg)?;
        let second = out(1, EMPTY_RETURN.to_vec());
        if is_return(o.script_pubkey.as_bytes()) && *o != second {
            return None;
        }
        *o = second;
        entity(i)
    }),
    m("8c:3", &[BAD_ASSETLOCKTX_NO_RETURN], |c, _| {
        c.lock()?;
        for o in c.outputs.iter_mut().filter(|o| is_return(o.script_pubkey.as_bytes())) {
            o.script_pubkey = p2pkh(id(7, 15)).into();
        }
        DONE
    }),
    m("8c:4", &[BAD_ASSETLOCKTX_PAYLOAD], |c, arg| {
        c.lock()?;
        bad_bytes(c, arg)
    }),
    m("8c:5", &[BAD_ASSETLOCKTX_VERSION], |c, arg| {
        c.lock()?.version = if arg & 1 == 0 {
            0
        } else {
            3 + (arg >> 1)
        };
        DONE
    }),
    m("8c:6", &[BAD_ASSETLOCKTX_VERSION_2], |c, _| {
        c.lock()?.version = 2;
        c.world.v24 = false;
        c.world.evo_shares = false;
        DONE
    }),
    m("8c:7", &[BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS], |c, _| {
        c.lock()?.credit_outputs.clear();
        DONE
    }),
    m("8c:8a.value", &[BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE], |c, arg| {
        let (k, o) = credit_output(c, arg >> 1)?;
        o.value = if arg & 1 == 0 {
            0
        } else {
            MAX_MONEY + 1
        };
        entity(k)
    }),
    m("8c:8a.sum", &[BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE], |c, arg| {
        // A value in range that takes the running sum one duff past MAX_MONEY.
        let credits = &mut c.lock()?.credit_outputs;
        if credits.len() < 2 {
            return None;
        }
        let k = 1 + usize::from(arg) % (credits.len() - 1);
        let before: i64 = credits[..k].iter().map(|o| o.value).sum();
        if !(1..=MAX_MONEY).contains(&before) {
            return None;
        }
        credits.get_mut(k)?.value = MAX_MONEY - before + 1;
        entity(k)
    }),
    m("8c:8b", &[BAD_ASSETLOCKTX_SCRIPT_PUBKEY], |c, arg| {
        if c.lock()?.version < 2 {
            return None;
        }
        let (k, o) = credit_output(c, arg >> 1)?;
        o.script_pubkey = if arg & 1 == 0 {
            NO_DEST.to_vec()
        } else {
            EMPTY_RETURN.to_vec()
        }
        .into();
        entity(k)
    }),
    m("8c:8c", &[BAD_ASSETLOCKTX_PUBKEYHASH], |c, arg| {
        if c.lock()?.version >= 2 {
            return None;
        }
        let (k, o) = credit_output(c, arg >> 1)?;
        o.script_pubkey = if arg & 1 == 0 {
            p2sh(id(8, 9))
        } else {
            NO_DEST.to_vec()
        }
        .into();
        entity(k)
    }),
    m("8c:9", &[BAD_ASSETLOCKTX_CREDITAMOUNT], |c, arg| {
        c.lock()?;
        let o = c.outputs.iter_mut().find(|o| is_return(o.script_pubkey.as_bytes()))?;
        o.value += if arg & 1 == 0 {
            -1
        } else {
            1
        };
        DONE
    }),
];

/// The catalogue of `tx_type`, in Core's order: the type's steps, then the list and network
/// steps it reaches (ProRegTx, ProUpServTx and ProUpRegTx).
pub fn catalogue(tx_type: TxType) -> Vec<&'static Mutation> {
    match tx_type {
        TxType::ProviderRegister => PRO_REG
            .iter()
            .chain(PAYOUT_LIST)
            .chain(SHARE_LIST)
            .chain(NET_INFO)
            .chain(PLATFORM_FIELDS)
            .collect(),
        TxType::ProviderUpdateService => {
            PRO_UP_SERV.iter().chain(NET_INFO).chain(PLATFORM_FIELDS).collect()
        }
        TxType::ProviderUpdateRegistrar => PRO_UP_REG.iter().chain(PAYOUT_LIST).collect(),
        TxType::ProviderUpdateRevoke => PRO_UP_REV.iter().collect(),
        TxType::ProviderDissolve => PRO_DIS.iter().collect(),
        TxType::ProviderUpdateShare => PRO_UP_SHARE.iter().collect(),
        TxType::ProviderUpdateSharedRegistrar => PRO_UP_SHARED_REG.iter().collect(),
        TxType::CoinbaseCommitment => CB_TX.iter().collect(),
        TxType::AssetLock => ASSET_LOCK.iter().collect(),
        _ => Vec::new(),
    }
}

/// Inputs and outputs no check of the type reads (the n-1 nit): `variation` adds up to two
/// funding inputs and up to two outputs, before or after a registration's collateral output (or
/// an asset lock's return output). A dissolution reads all of both, so it is left alone, and a
/// coinbase has one input, so it gains outputs only.
pub fn vary(case: &mut Case, variation: u8) {
    if matches!(case.payload, Payload::Dis(_)) {
        return;
    }
    let extra_inputs = if matches!(case.payload, Payload::CbTx(_)) {
        0
    } else {
        usize::from(variation % 3)
    };
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

/// The chain parameters of a world with V19 at the block, as dash-params builds them.
pub fn params() -> Option<&'static ChainParams> {
    static PARAMS: OnceLock<Option<ChainParams>> = OnceLock::new();
    PARAMS.get_or_init(|| build_params(V19)).as_ref()
}

/// The chain parameters of `world`.
pub fn params_of(world: &World) -> Option<&'static ChainParams> {
    static LATE: OnceLock<Option<ChainParams>> = OnceLock::new();
    if world.v19 {
        params()
    } else {
        LATE.get_or_init(|| build_params(V19_LATE)).as_ref()
    }
}

fn build_params(v19: &str) -> Option<ChainParams> {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, v19)
        .arg(ChainOption::Dip3Params, DIP3)
        .build()
        .ok()
}

/// The chain of a world with V19 at the block, as the oracle takes it.
pub fn chain() -> Chain {
    build_chain(V19)
}

/// The chain of `world`.
pub fn chain_of(world: &World) -> Chain {
    build_chain(if world.v19 {
        V19
    } else {
        V19_LATE
    })
}

fn build_chain(v19: &str) -> Chain {
    Chain::new(
        "regtest",
        &[&format!("-testactivationheight={v19}"), &format!("-dip3params={DIP3}")],
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

/// The oracle's context of `world`: the headers, the global flag, the deployments, the coins
/// unchecked, the list.
pub fn context(world: &World) -> Result<Context, Divergence> {
    let (headers, tip) = headers().ok_or(divergence("headers"))?;
    let list = world.list_bytes(*tip).ok_or(divergence("world list"))?;
    Ok(Context::new(
        FIRST as i32,
        headers.to_vec(),
        !world.v19,
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
    let core = match oracle.check_special_tx(&chain_of(&case.world), &raw, &ctx, case.world.sigs) {
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
    let ours = with_our_ctx(case, |cx| {
        cx.check_special_tx(&tx).err().map(|e| (e.result() as i32, e.reject_reason().to_owned()))
    })?;
    Ok(Some(Verdicts {
        core,
        ours,
    }))
}

/// `f` over our check context for `case`'s world, with the world's `check_sigs`.
fn with_our_ctx<T>(case: &Case, f: impl FnOnce(&CheckCtx) -> T) -> Result<T, Divergence> {
    let params = params_of(&case.world).ok_or(divergence("params"))?;
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
    let mut cx = CheckCtx::new(params, headers, &our_coins, rules, &list);
    cx.check_sigs = case.world.sigs;
    check(cx.height == TIP as i32 + 1, "height")?;
    check(cx.v19 == case.world.v19, "V19")?;
    Ok(f(&cx))
}

/// Our check loop's walk of `case`'s transaction in a [`block`] (`dml_checks::check_block`, whose
/// step tables `diff_dml_block` reads), with the world's `check_sigs`: the step and reason of our
/// reject, `None` for an acceptance. A reason the step's row lacks is a divergence.
pub fn walked(case: &Case) -> Result<Option<(&'static str, &'static str)>, Divergence> {
    let bytes = block(&[case.tx()]).ok_or(divergence("block"))?;
    let ours =
        decode_block(&mut &bytes[..]).map_err(|_| divergence("our block read"))?.into_value();
    let checks = with_our_ctx(case, |cx| crate::dml_checks::check_block(cx, &ours))??;
    // The coinbase's check, then the transaction's.
    let tx = checks.get(1).ok_or(divergence("the transaction's check"))?;
    Ok(tx.reject.as_ref().map(|(at, reason)| (at.step.id, *reason)))
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

/// The inputs of `diff_check_special_tx`'s public seeds: every seed of eight worlds (none, V24,
/// V24 with EVO_SHARES, V24 early with three shares, early with three shares, the last two
/// checking signatures, and before V19 with and without them), and each mutation of the
/// catalogue with four arguments, on the first seed of a V24 world checking signatures that
/// carries it. Before V19 with signatures, the operator signature steps (2c:13, 4c:5) and 2c:3
/// (refused before the signature) run on every seed that carries them, so that the seeds hold
/// legacy-flag BLS rejects (D30).
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let worlds = [
        0,
        FLAG_V24,
        FLAG_V24 | FLAG_EVO_SHARES,
        FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES,
        FLAG_EARLY | FLAG_THREE_SHARES,
        FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES | FLAG_SIGS,
        FLAG_EARLY | FLAG_THREE_SHARES | FLAG_SIGS,
        FLAG_LEGACY,
        FLAG_LEGACY | FLAG_SIGS,
    ];
    let mut out = Vec::new();
    for (t, &tx_type) in TYPES.iter().enumerate() {
        for flags in worlds {
            for variant in variants(tx_type) {
                if seed(World::new(flags), tx_type, variant).is_some() {
                    out.push(vec![flags, t as u8, variant, 0, 0]);
                }
            }
        }
        for (pick, mutation) in catalogue(tx_type).iter().enumerate() {
            for arg in 0..4u8 {
                let flags = FLAG_V24 | FLAG_EARLY | FLAG_THREE_SHARES | FLAG_SIGS;
                let carrier = variants(tx_type).find(|&variant| {
                    seed(World::new(flags), tx_type, variant)
                        .is_some_and(|mut c| mutation.apply(&mut c, arg))
                });
                if let Some(variant) = carrier {
                    out.push(vec![flags, t as u8, variant, 0, 1, pick as u8, arg]);
                }
            }
            if ["2c:3", "2c:13", "4c:5"].contains(&mutation.step) {
                let flags = FLAG_LEGACY | FLAG_SIGS;
                for variant in variants(tx_type) {
                    let carries = seed(World::new(flags), tx_type, variant)
                        .is_some_and(|mut c| mutation.apply(&mut c, 0));
                    if carries {
                        out.push(vec![flags, t as u8, variant, 0, 1, pick as u8, 0]);
                    }
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
    let params = params_of(world).ok_or(divergence("params"))?;
    crate::dml_block::compare_dml_block_detailed(
        oracle,
        &chain_of(world),
        params,
        &context(world)?,
        &block,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Case::tx`] leaves a bare payload unsigned (BL-548): signed, every bare case of every
    /// seed, step and argument below 4 encodes the same bytes.
    #[test]
    fn signing_changes_no_byte_of_a_bare_payload() {
        let bytes = |tx: &Tx| {
            let mut bytes = Vec::new();
            tx.raw().encode(&mut bytes);
            bytes
        };
        let mut checked = 0;
        for flags in [FLAG_SIGS, FLAG_SIGS | FLAG_LEGACY, FLAG_SIGS | FLAG_V24 | FLAG_EVO_SHARES] {
            for tx_type in TYPES {
                for v in variants(tx_type) {
                    let Some(s) = seed(World::new(flags), tx_type, v) else {
                        continue;
                    };
                    for m in catalogue(tx_type) {
                        for arg in 0..4 {
                            let mut c = s.clone();
                            if !(m.apply(&mut c, arg) && c.bare) {
                                continue;
                            }
                            let mut signed = c.unsigned();
                            signed.sign();
                            assert_eq!(bytes(&signed.encoded()), bytes(&c.tx()), "{}", m.step);
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 0, "no step makes a bare payload");
    }
}

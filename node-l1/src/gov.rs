//! GV1a's targets `diff_gov` and `diff_gov_vote`: dash-governance against Dash Core v24's
//! src/governance through O2a-GOV's oracle entries (ABI 26, `oracle.h` "governance"), on the
//! Core-produced regtest network of `dash-oracle-sys/tests/data/gov-regtest-v24.0.*` (O2a-GOV
//! D11: four masternodes and two evonodes, four proposals, a superblock trigger and node 0's
//! votes; provenance in that directory's `PROVENANCE.md`). GV1a notes D16, D17 and D22–D25.
//!
//! Byte 0 selects a case (modulo 2); the rest is the case's input. A short input is
//! [`Outcome::OutOfDomain`].
//!
//! `diff_gov`:
//!
//! | case | input | compared |
//! |---|---|---|
//! | 0 proposal | flags (bit 0 check expiration, bit 1 allow script, bits 2-3 network: main, test, the fixture's regtest, a named devnet), time, mode (bit 0: the rest is data the harness hex-encodes, else the hex text itself; bit 1: wrapped as the legacy `[["proposal", …]]`), the rest | `ValidateProposal`'s verdict and whole error string |
//! | 1 object | flags (bit 1: the re-keyed list), clock (i8, hours from the fixture's time), collaterals (2 bits each: active, off the active chain, absent), object, signer | every field of `oracle_gov_object`'s record |
//!
//! `diff_gov_vote`:
//!
//! | case | input | compared |
//! |---|---|---|
//! | 0 vote | flags (bit 0: the re-keyed list), clock (i8, 10 minutes each), vote | every field of `oracle_gov_vote`'s record |
//! | 1 script | flags (bit 0 re-keyed, bit 1 synced, bit 2 node 0's votes first), object mask, collaterals (as case 1 object), op count (u8 % 17), ops | each op's output of `oracle_gov_ops`, then every object's state and the next superblock |
//!
//! Primitives:
//! - time (`t`): even, [`TIMES`]`[t / 2 % len]`; odd, an i64 LE follows.
//! - int (`t`): even, [`INTS`]`[t / 2 % len]`; odd, an i32 LE follows.
//! - object: a source byte below 0xf0 is fixture object `s % 5` (proposals, then the trigger),
//!   edited by a count (u8 % 8) of edits (u8 % 9: parent hash, revision, time, collateral (below
//!   0x80 a fixture collateral, else 32 bytes), data (u8 length and bytes), data appended, type,
//!   masternode outpoint (below 0x80 a list member, else 36 bytes), signature (u8 length and
//!   bytes, the first [`object::MAX_SIGNATURE_SIZE`] kept)), then a signer byte (0xff none, else
//!   the re-keyed operator key `b % 8` signs the result); 0xf0 and above: the rest is the message.
//! - vote: the same with fixture vote `s % n` and edits (u8 % 7: outpoint, parent (below 0x80 a
//!   fixture object's hash), outcome (int), signal (int), time (u8 `d`: `d % 4` = 0, the fixture
//!   time plus `(d as i8 >> 2)` × 10 minutes; 1, an i64; 2, the fixture time plus an hour plus
//!   `(d >> 2) % 5 - 2` seconds; 3, a [`TIMES`] pick), signature (u8 length and bytes), sign (u8
//!   `m`: `m % 4` = 0 the voting key over the message, 1 the voting key over the hash, 2 the
//!   operator key, 3 nothing; key `(m >> 2) % 8`)).
//! - op: u8 % 5: 0 TIME (i16 × 16 s from the fixture's time), 1 and 2 VOTE (a vote), 3 BLOCK
//!   ([`HEIGHTS`]`[u8 % len]`), 4 OBJECT (an object).
//!
//! The node's side of a script is a model of `CGovernanceManager`'s `ProcessObject` and
//! `ProcessVote` (`governance.cpp:227-275`, `:802-854`) over dash-governance, as GV1b will run
//! them: seen objects, the trigger rate check, local validity, postponement, `AddTrigger`, the
//! known-vote map, unknown parents (the orphan cache's insert refuses every vote after the first
//! for a parent, because `CGovernanceVote`'s `operator<` is false for every pair,
//! `vote.cpp:263-291`), the orphan vote replayed when its parent is added (`CheckOrphanVotes`),
//! and expired or deleted parents.
//!
//! Not compared (outside the domain): times the oracle refuses (`ORACLE_E_ARG`: 0, or above
//! `INT64_MAX / 1e9 - 7200`), proposal hex text that is not UTF-8 (the oracle's entry takes a
//! `&str`), a record string that is not UTF-8 (the oracle's record reader refuses it; the node's
//! text is lossy there), and a trigger whose event height overflows `int` in `AddTrigger`
//! (undefined behaviour in Core, refused by the oracle). Any other oracle error diverges.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::sync::{Arc, OnceLock};

use dash_consensus::primitives::write_compact_size;
use dash_consensus::{Tx, decode_tx};
use dash_dml::codec::core_disk;
use dash_dml::testkit::{fresh, list_disk_bytes};
use dash_dml::{MnCold, MnList};
use dash_evo::spork::{key_id_of, message_hash, sign_compact};
use dash_governance::GovParams;
use dash_governance::exception::{ExceptionType, GovException};
use dash_governance::object::{
    self, Check, FoundCollateral, TRIGGER, Validity, is_valid_locally, min_collateral_fee,
    txid_bytes,
};
use dash_governance::proposal::validate_proposal;
use dash_governance::rate::{LastObjectRecord, masternode_rate_check};
use dash_governance::strings::{hash_hex, hex_str};
use dash_governance::superblocks::SuperblockSet;
use dash_governance::tally::{ObjectState, OutpointKey, outpoint_key};
use dash_governance::trigger::{Status, Superblock, is_valid_block_height, payments_limit};
use dash_governance::vote;
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Collateral, Context, DEPLOYMENT_V24, ErrorKind, Expiry,
    GovContext, GovExceptionType, GovObjectCheck, GovOp, GovOutput, GovVoteCheck,
    MnList as CoreList, ObjectOutcome, Oracle, Pin, SuperblockOutcome, ThresholdState,
    TxOut as CoreTxOut, VoteOutcome,
};
use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey, BlsSigId};
use dash_primitives::{GovObject, GovVote};
use dash_types::codec::BaseCodec;
use secp256k1::{PublicKey, SecretKey};
use serde_json::Value;

use crate::dml::unhex;
use crate::{Compared, Divergence, Outcome, check, divergence};

/// `diff_gov`'s cases (byte 0 modulo 2).
pub const CASE_PROPOSAL: u8 = 0;
pub const CASE_OBJECT: u8 = 1;
/// `diff_gov_vote`'s cases.
pub const CASE_VOTE: u8 = 0;
pub const CASE_SCRIPT: u8 = 1;

/// The largest time the oracle takes (`oracle.h`: `INT64_MAX / 1e9 - 7200`).
pub const MAX_TIME: i64 = i64::MAX / 1_000_000_000 - 7200;

/// The times an even tag picks: the oracle's bounds, the fixture's clock, Core's vectors' epochs.
pub const TIMES: [i64; 12] = [
    1,
    1_417_715_856,
    1_417_715_841,
    1_417_802_241,
    1_474_261_086,
    1_491_368_400,
    1_760_659_200,
    MAX_TIME,
    MAX_TIME + 1,
    0,
    -1,
    i64::MAX,
];

/// The ints an even tag picks: types, signals and outcomes around their ranges, and the extremes.
pub const INTS: [i32; 12] = [0, 1, 2, 3, 4, 5, 6, -1, 7, 255, i32::MAX, i32::MIN];

/// BLOCK heights: the fixture's next and last superblocks, their neighbours and the extremes.
pub const HEIGHTS: [i32; 8] = [180, 160, 200, 179, 181, 0, -20, i32::MAX];

/// The fixture's gzipped JSON and node 0's `governance.dat`.
const FIXTURE_GZ: &[u8] =
    include_bytes!("../../dash-oracle-sys/tests/data/gov-regtest-v24.0.json.gz");
const GOVERNANCE_DAT: &[u8] =
    include_bytes!("../../dash-oracle-sys/tests/data/gov-regtest-v24.0.governance.dat");

/// The node's regtest arguments (the framework's `dash.conf` and `gov_capture.py`'s).
const REGTEST_ARGS: [&str; 4] = [
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-budgetparams=10:10:10",
];

/// A `CGovernanceObject`'s network fields, edited as fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawObject {
    pub parent: [u8; 32],
    pub revision: i32,
    pub time: i64,
    pub collateral: [u8; 32],
    pub data: Vec<u8>,
    pub obj_type: i32,
    pub outpoint: OutpointKey,
    pub sig: Vec<u8>,
}

/// A `CompactSize`-prefixed byte string.
fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    write_compact_size(b.len() as u64, out);
    out.extend_from_slice(b);
}

impl RawObject {
    /// `CGovernanceObject` in network serialization (`governance/common.h:70-84`).
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.parent);
        out.extend(self.revision.to_le_bytes());
        out.extend(self.time.to_le_bytes());
        out.extend(self.collateral);
        put_bytes(&mut out, &self.data);
        out.extend(self.obj_type.to_le_bytes());
        out.extend(self.outpoint.0);
        out.extend(self.outpoint.1.to_le_bytes());
        put_bytes(&mut out, &self.sig);
        out
    }

    fn decoded(&self) -> Option<GovObject> {
        decode_whole(&self.bytes())
    }

    pub fn hash(&self) -> Option<[u8; 32]> {
        self.decoded().map(|o| object::hash(&o))
    }

    /// Signed by operator key `key` over `GetSignatureHash()`.
    fn sign(&mut self, key: u8) {
        if let Some(obj) = self.decoded() {
            self.sig = bls_sign(key, &object::signature_hash(&obj));
        }
    }
}

/// A `CGovernanceVote`'s network fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawVote {
    pub outpoint: OutpointKey,
    pub parent: [u8; 32],
    pub outcome: i32,
    pub signal: i32,
    pub time: i64,
    pub sig: Vec<u8>,
}

impl RawVote {
    /// `CGovernanceVote` in network serialization (`governance/vote.h:190-206`).
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.outpoint.0);
        out.extend(self.outpoint.1.to_le_bytes());
        out.extend(self.parent);
        out.extend(self.outcome.to_le_bytes());
        out.extend(self.signal.to_le_bytes());
        out.extend(self.time.to_le_bytes());
        put_bytes(&mut out, &self.sig);
        out
    }

    /// The vote with any signature, for its hashes and message.
    fn unsigned(&self) -> Option<GovVote> {
        let mut bytes = self.bytes();
        bytes.truncate(bytes.len() - self.sig.len() - 1);
        bytes.push(0);
        decode_whole(&bytes)
    }

    /// Signed as `mode` says (see the module docs) with key `key`.
    fn sign(&mut self, mode: u8, key: u8) {
        let Some(v) = self.unsigned() else {
            return;
        };
        self.sig = match mode % 4 {
            0 => sign_compact(&message_hash(&vote::signature_string(&v)), voting_key(key), true),
            1 => sign_compact(&vote::signature_hash(&v), voting_key(key), true),
            2 => bls_sign(key, &vote::signature_hash(&v)),
            _ => return,
        };
    }
}

/// The message decoded as a whole, as the oracle's object and vote entries read it.
fn decode_whole<T: BaseCodec>(bytes: &[u8]) -> Option<T> {
    let mut cursor = bytes;
    T::decode(&mut cursor).ok().filter(|_| cursor.is_empty())
}

/// The harness's voting key `i` (0..8): the secp256k1 secret 32 bytes `0x31 + i`.
pub fn voting_key(i: u8) -> &'static SecretKey {
    static KEYS: [OnceLock<SecretKey>; 8] = [const { OnceLock::new() }; 8];
    let i = i % 8;
    KEYS[usize::from(i)].get_or_init(|| {
        #[allow(clippy::expect_used)]
        SecretKey::from_secret_bytes([0x31 + i; 32]).expect("a fixed key")
    })
}

/// The harness's operator key `i` (0..8): the basic-scheme secret key from 32 bytes `0xc1 + i`.
pub fn operator_key(i: u8) -> &'static BlsSecretKey<BlsScIetf> {
    static KEYS: [OnceLock<BlsSecretKey<BlsScIetf>>; 8] = [const { OnceLock::new() }; 8];
    let i = i % 8;
    KEYS[usize::from(i)].get_or_init(|| {
        #[allow(clippy::expect_used)]
        BlsSecretKey::from_ikm(&[0xc1 + i; 32]).expect("a fixed key")
    })
}

fn bls_sign(key: u8, hash: &[u8; 32]) -> Vec<u8> {
    operator_key(key).sign_with(hash, BlsSigId::Basic).to_bytes().to_vec()
}

/// A fee transaction of the fixture, mined on the active chain.
pub struct FixtureCollateral {
    pub txid: [u8; 32],
    pub tx_bytes: Vec<u8>,
    pub tx: Tx,
    /// The block holding it, internal order.
    pub block_hash: [u8; 32],
    pub height: i32,
}

/// The fixture's network seen through one masternode list: the captured one, or the same list
/// re-keyed (each member's voting key id and operator key replaced by the harness's keys, by
/// list position) with the trigger and every vote signed again by those keys.
pub struct Variant {
    pub list: MnList,
    pub list_bytes: Vec<u8>,
    /// node 0's objects: the proposals, then the trigger.
    pub objects: Vec<RawObject>,
    /// node 0's votes, in vote-file order.
    pub votes: Vec<RawVote>,
}

/// The fixture.
pub struct Fixture {
    pub params: GovParams,
    pub chain: Chain,
    pub headers: Vec<[u8; 80]>,
    pub tip: i32,
    /// node 0's mock time at the tip.
    pub now: i64,
    pub collaterals: Vec<FixtureCollateral>,
    /// The list members' collateral outpoints, in list order (the keys' index).
    pub members: Vec<OutpointKey>,
    pub variants: [Variant; 2],
}

/// An RPC hash (display order) in internal order.
fn rpc_hash(s: &str) -> Option<[u8; 32]> {
    let mut h: [u8; 32] = unhex(s)?.try_into().ok()?;
    h.reverse();
    Some(h)
}

/// GV1a's parameters for the fixture's chain.
pub fn regtest_params() -> GovParams {
    let b = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@100")
        .arg(ChainOption::BudgetParams, "10:10:10");
    #[allow(clippy::expect_used)]
    GovParams::from_chain_params(&b.build().expect("the fixture's chain")).expect("its params")
}

/// What `diff_gov`'s proposal selector (flag bits 2-3) picks: Core's four chains, regtest being
/// the fixture's.
pub const NETWORKS: [NetworkKind; 4] =
    [NetworkKind::Mainnet, NetworkKind::Testnet, NetworkKind::Regtest, NetworkKind::Devnet];

/// The devnet's `-devnet` name.
const DEVNET_NAME: &str = "gv1a";

/// The oracle's chain and GV1a's parameters for a selector (its low two bits).
pub fn params_of(network: u8) -> &'static (Chain, GovParams) {
    static PARAMS: [OnceLock<(Chain, GovParams)>; 4] = [const { OnceLock::new() }; 4];
    let i = usize::from(network & 3);
    PARAMS[i].get_or_init(|| {
        let (chain, builder) = match NETWORKS[i] {
            NetworkKind::Regtest => {
                return (Chain::new("regtest", &REGTEST_ARGS), regtest_params());
            }
            NetworkKind::Devnet => (
                Chain::devnet(DEVNET_NAME),
                ChainParamsBuilder::new(NetworkKind::Devnet, Release::V24_0)
                    .arg(ChainOption::Devnet, DEVNET_NAME),
            ),
            kind => (
                if kind == NetworkKind::Mainnet {
                    Chain::main()
                } else {
                    Chain::test()
                },
                ChainParamsBuilder::new(kind, Release::V24_0),
            ),
        };
        #[allow(clippy::expect_used)]
        let p = GovParams::from_chain_params(&builder.build().expect("params")).expect("params");
        (chain, p)
    })
}

/// The fixture, read once. `None` when it does not read, which the tests notice.
pub fn fixture() -> Option<&'static Fixture> {
    static FIXTURE: OnceLock<Option<Fixture>> = OnceLock::new();
    FIXTURE.get_or_init(read_fixture).as_ref()
}

/// The fixture with every collateral mined `confirmations` blocks deep: its block is the
/// context's header at `tip - confirmations + 1`. node 0's collaterals are 15 deep, so they never
/// reach the window Core postpones in (1 to 5, `object.cpp:853-873`). The oracle's `FindTx` seam
/// serves the transaction from that block (O2a-GOV D4), and the active chain counts the
/// confirmations from it.
pub fn fixture_confirmed(confirmations: i32) -> Option<Fixture> {
    let mut fx = read_fixture()?;
    let height = fx.tip.checked_sub(confirmations)?.checked_add(1)?;
    let block_hash = crate::mnhf::header_hash(fx.headers.get(usize::try_from(height).ok()?)?);
    for c in &mut fx.collaterals {
        c.height = height;
        c.block_hash = block_hash;
    }
    Some(fx)
}

fn read_fixture() -> Option<Fixture> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(FIXTURE_GZ).read_to_string(&mut text).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    let loaded = dash_coreimport::netstate::governance::parse(
        GOVERNANCE_DAT,
        dash_network::Network::Regtest,
    )
    .ok()?;
    let dat = loaded.value;
    let headers = json["headers"]
        .as_array()?
        .iter()
        .map(|h| unhex(h.as_str()?)?.try_into().ok())
        .collect::<Option<Vec<[u8; 80]>>>()?;
    let list_bytes = unhex(json["tip_list"].as_str()?)?;
    let list = core_disk::decode_list_s3(&mut list_bytes.as_slice()).ok()?;
    let mut collaterals = Vec::new();
    for (txid, c) in json["collaterals"].as_object()? {
        let tx_bytes = unhex(c["hex"].as_str()?)?;
        let tx = decode_tx(&mut tx_bytes.as_slice()).ok()?.into_value();
        collaterals.push(FixtureCollateral {
            txid: rpc_hash(txid)?,
            tx_bytes,
            tx,
            block_hash: rpc_hash(c["blockhash"].as_str()?)?,
            height: i32::try_from(c["height"].as_i64()?).ok()?,
        });
    }
    collaterals.sort_by(|a, b| a.tx_bytes.cmp(&b.tx_bytes));
    let mut objects: Vec<RawObject> = dat
        .objects
        .values()
        .map(|o| RawObject {
            parent: o.parent_hash,
            revision: o.revision,
            time: o.time,
            collateral: o.collateral_hash,
            data: o.data.clone(),
            obj_type: o.object_type,
            outpoint: (o.masternode_outpoint.hash, o.masternode_outpoint.n),
            sig: o.signature.clone(),
        })
        .collect();
    objects.sort_by_key(|o| o.obj_type);
    let votes: Vec<RawVote> = objects_votes(&dat);
    let members: Vec<OutpointKey> = list.iter().map(|d| outpoint_key(&d.collateral)).collect();
    let rekeyed = rekey(&list, &objects, &votes, &members)?;
    Some(Fixture {
        params: regtest_params(),
        chain: Chain::new("regtest", &REGTEST_ARGS),
        headers,
        tip: i32::try_from(json["tip"].as_i64()?).ok()?,
        now: json["mocktime_at_tip"].as_i64()?,
        collaterals,
        members,
        variants: [
            Variant {
                list,
                list_bytes,
                objects,
                votes,
            },
            rekeyed,
        ],
    })
}

/// node 0's votes, every object's vote file in object order.
fn objects_votes(dat: &dash_coreimport::netstate::governance::GovernanceDat) -> Vec<RawVote> {
    let mut objects: Vec<_> = dat.objects.values().collect();
    objects.sort_by_key(|o| o.object_type);
    objects
        .iter()
        .flat_map(|o| o.vote_file.votes.iter())
        .map(|v| RawVote {
            outpoint: (v.masternode_outpoint.hash, v.masternode_outpoint.n),
            parent: v.parent_hash,
            outcome: v.outcome,
            signal: v.signal,
            time: v.time,
            sig: v.signature.clone(),
        })
        .collect()
}

/// The re-keyed variant (see [`Variant`]).
fn rekey(
    list: &MnList,
    objects: &[RawObject],
    votes: &[RawVote],
    members: &[OutpointKey],
) -> Option<Variant> {
    let mut rekeyed = list.clone();
    for (i, dmn) in list.iter().enumerate() {
        let i = u8::try_from(i).ok()?;
        let mut cold: MnCold = (*dmn.cold).clone();
        let pk = PublicKey::from_secret_key(voting_key(i));
        cold.key_id_voting = key_id_of(&pk, true).into();
        cold.pub_key_operator = dash_dml::OperatorKey::from_bytes(
            &operator_key(i).public_key().to_bytes(),
            BlsSchemeSource::Version(BlsSchemeKind::Basic),
        );
        rekeyed.update(&dmn.pro_tx_hash, dmn.hot, Arc::new(fresh(cold))).ok()?;
    }
    let key_of = |op: &OutpointKey| members.iter().position(|m| m == op).map(|i| i as u8);
    let mut renamed = BTreeMap::new();
    let mut new_objects = objects.to_vec();
    for o in &mut new_objects {
        if o.obj_type == TRIGGER {
            let old = o.hash()?;
            o.sign(key_of(&o.outpoint).unwrap_or(7));
            renamed.insert(old, o.hash()?);
        }
    }
    let proposals: BTreeSet<[u8; 32]> =
        new_objects.iter().filter(|o| o.obj_type != TRIGGER).filter_map(RawObject::hash).collect();
    let new_votes = votes
        .iter()
        .map(|v| {
            let mut v = v.clone();
            v.parent = renamed.get(&v.parent).copied().unwrap_or(v.parent);
            let key = key_of(&v.outpoint).unwrap_or(7);
            let funding = v.signal == vote::SIGNAL_FUNDING && proposals.contains(&v.parent);
            let mode = if funding {
                0
            } else {
                2
            };
            v.sign(mode, key);
            v
        })
        .collect();
    let list_bytes = list_disk_bytes(&rekeyed);
    Some(Variant {
        list: rekeyed,
        list_bytes,
        objects: new_objects,
        votes: new_votes,
    })
}

impl Fixture {
    /// The chain context at the tip with `variant`'s list and adjusted time `now`.
    pub fn context(&self, variant: &Variant, now: i64) -> Context {
        Context::new(
            0,
            self.headers.clone(),
            false,
            vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
            now,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            CoreList::Supplied(variant.list_bytes.clone()),
            vec![],
        )
    }

    /// The governance context with each collateral as `modes` (2 bits each) says.
    pub fn gov_context(&self, synced: bool, modes: u8) -> GovContext {
        GovContext {
            synced,
            collaterals: self
                .collaterals
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match collateral_mode(modes, i) {
                    CollateralMode::Active => Some(Collateral {
                        tx: c.tx_bytes.clone(),
                        block_hash: c.block_hash,
                        active: true,
                    }),
                    CollateralMode::Inactive => Some(Collateral {
                        tx: c.tx_bytes.clone(),
                        block_hash: OFF_CHAIN_BLOCK,
                        active: false,
                    }),
                    CollateralMode::Absent => None,
                })
                .collect(),
        }
    }

    /// `g_txindex->FindTx` and the confirmations `IsCollateralValid` counts, as `modes` says.
    fn find_collateral(&self, hash: &[u8; 32], modes: u8) -> Option<FoundCollateral<'_>> {
        let (i, c) = self.collaterals.iter().enumerate().find(|(_, c)| &c.txid == hash)?;
        let confirmations = match collateral_mode(modes, i) {
            CollateralMode::Active => self.tip - c.height + 1,
            CollateralMode::Inactive => 0,
            CollateralMode::Absent => return None,
        };
        Some(FoundCollateral {
            tx: &c.tx,
            mined: true,
            confirmations,
        })
    }
}

/// A block hash off the context's chain.
const OFF_CHAIN_BLOCK: [u8; 32] = [0x55; 32];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CollateralMode {
    Active,
    Inactive,
    Absent,
}

fn collateral_mode(modes: u8, i: usize) -> CollateralMode {
    match (u32::from(modes) >> (2 * (i % 4))) & 3 {
        0 => CollateralMode::Active,
        1 => CollateralMode::Inactive,
        _ => CollateralMode::Absent,
    }
}

/// The input reader.
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = usize::from(self.u8()?).min(self.0.len());
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(head.to_vec())
    }

    fn time(&mut self) -> Option<i64> {
        let t = self.u8()?;
        if t & 1 == 0 {
            Some(TIMES[usize::from(t >> 1) % TIMES.len()])
        } else {
            self.array().map(i64::from_le_bytes)
        }
    }

    fn int(&mut self) -> Option<i32> {
        let t = self.u8()?;
        if t & 1 == 0 {
            Some(INTS[usize::from(t >> 1) % INTS.len()])
        } else {
            self.array().map(i32::from_le_bytes)
        }
    }

    fn outpoint(&mut self, members: &[OutpointKey]) -> Option<OutpointKey> {
        let b = self.u8()?;
        if b < 0x80 {
            members.get(usize::from(b) % members.len().max(1)).copied()
        } else {
            Some((self.array()?, u32::from_le_bytes(self.array()?)))
        }
    }

    /// An object message (see the module docs).
    fn object(&mut self, fx: &Fixture, v: &Variant) -> Option<Vec<u8>> {
        let source = self.u8()?;
        if source >= 0xf0 || v.objects.is_empty() {
            return Some(std::mem::take(&mut self.0).to_vec());
        }
        let mut o = v.objects[usize::from(source) % v.objects.len()].clone();
        for _ in 0..self.u8()? % 8 {
            match self.u8()? % 9 {
                0 => o.parent = self.array()?,
                1 => o.revision = self.int()?,
                2 => o.time = self.time()?,
                3 => {
                    let b = self.u8()?;
                    o.collateral = match fx.collaterals.get(usize::from(b)) {
                        Some(c) if b < 0x80 => c.txid,
                        _ if b < 0x80 => fx.collaterals.first()?.txid,
                        _ => self.array()?,
                    };
                }
                4 => o.data = self.bytes()?,
                5 => o.data.extend(self.bytes()?),
                6 => o.obj_type = self.int()?,
                7 => o.outpoint = self.outpoint(&fx.members)?,
                _ => {
                    o.sig = self.bytes()?;
                    o.sig.truncate(object::MAX_SIGNATURE_SIZE);
                }
            }
        }
        let signer = self.u8()?;
        if signer != 0xff {
            o.sign(signer);
        }
        Some(o.bytes())
    }

    /// A vote message (see the module docs).
    fn vote(&mut self, fx: &Fixture, v: &Variant) -> Option<Vec<u8>> {
        let source = self.u8()?;
        if source >= 0xf0 || v.votes.is_empty() {
            return Some(std::mem::take(&mut self.0).to_vec());
        }
        let mut vote = v.votes[usize::from(source) % v.votes.len()].clone();
        for _ in 0..self.u8()? % 8 {
            match self.u8()? % 7 {
                0 => vote.outpoint = self.outpoint(&fx.members)?,
                1 => {
                    let b = self.u8()?;
                    vote.parent = if b < 0x80 {
                        v.objects.get(usize::from(b) % v.objects.len())?.hash()?
                    } else {
                        self.array()?
                    };
                }
                2 => vote.outcome = self.int()?,
                3 => vote.signal = self.int()?,
                4 => {
                    let d = self.u8()?;
                    vote.time = match d % 4 {
                        0 => fx.now + i64::from((d as i8) >> 2) * 600,
                        1 => i64::from_le_bytes(self.array()?),
                        2 => fx.now + vote::MAX_TIME_AHEAD + i64::from((d >> 2) % 5) - 2,
                        _ => TIMES[usize::from(d >> 2) % TIMES.len()],
                    };
                }
                5 => vote.sig = self.bytes()?,
                _ => {
                    let m = self.u8()?;
                    vote.sign(m, (m >> 2) % 8);
                }
            }
        }
        Some(vote.bytes())
    }
}

/// Core's answer, `None` when the failure is one `expected` accepts; any other failure diverges.
fn answer<T>(
    result: dash_oracle_sys::Result<T>,
    expected: impl FnOnce(&dash_oracle_sys::Error) -> bool,
) -> Result<Option<T>, Divergence> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e) if expected(&e) => Ok(None),
        Err(_) => Err(divergence("oracle error")),
    }
}

/// The oracle's record reader refused a string that is not UTF-8.
fn record_not_utf8(e: &dash_oracle_sys::Error) -> bool {
    e.kind == ErrorKind::Exception && e.message.ends_with("output record is malformed")
}

/// Text the node made lossy (a byte string that is not UTF-8 in Core's).
fn lossy(s: &str) -> bool {
    s.contains('\u{fffd}')
}

/// The shared front of `diff_gov` and `diff_gov_vote`: the case byte, the fixture and the
/// oracle, then `run` on the case (modulo 2) and the rest of the input.
fn dispatch(
    bytes: &[u8],
    pin: Pin,
    run: impl FnOnce(u8, &Oracle, &Fixture, &mut Reader<'_>) -> Option<Compared>,
) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some((&case, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let Some(fx) = fixture() else {
        return Err(divergence("fixture"));
    };
    let oracle = Oracle::new(pin);
    run(case % 2, &oracle, fx, &mut Reader(rest)).unwrap_or(Ok(Outcome::OutOfDomain))
}

/// `diff_gov`.
pub fn compare_gov(bytes: &[u8], pin: Pin) -> Compared {
    dispatch(bytes, pin, |case, oracle, fx, r| match case {
        CASE_PROPOSAL => proposal(oracle, r),
        _ => one_object(oracle, fx, r),
    })
}

/// `diff_gov_vote`.
pub fn compare_gov_vote(bytes: &[u8], pin: Pin) -> Compared {
    dispatch(bytes, pin, |case, oracle, fx, r| match case {
        CASE_VOTE => one_vote(oracle, fx, r),
        _ => script(oracle, fx, r),
    })
}

fn verdict(accepted: bool) -> Outcome {
    if accepted {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    }
}

fn proposal(oracle: &Oracle, r: &mut Reader<'_>) -> Option<Compared> {
    let flags = r.u8()?;
    let time = r.time()?;
    let mode = r.u8()?;
    let rest = std::mem::take(&mut r.0);
    let hex = if mode & 1 == 0 {
        // Text that is not UTF-8 is outside the oracle entry's domain.
        std::str::from_utf8(rest).ok()?.to_owned()
    } else if mode & 2 == 0 {
        hex_str(rest)
    } else {
        let mut legacy = b"[[\"proposal\",".to_vec();
        legacy.extend(rest);
        legacy.extend(b"]]");
        hex_str(&legacy)
    };
    let (chain, p) = params_of((flags >> 2) & 3);
    let (expiration, script) = (flags & 1 != 0, flags & 2 != 0);
    Some(compare_proposal(oracle, chain, p, &hex, expiration, script, time))
}

fn compare_proposal(
    oracle: &Oracle,
    chain: &Chain,
    p: &GovParams,
    hex: &str,
    expiration: bool,
    script: bool,
    time: i64,
) -> Compared {
    // The entry refuses only the times outside its domain (D24); any other refusal diverges.
    let core = answer(oracle.gov_proposal_validate(chain, hex, expiration, script, time), |e| {
        e.kind == ErrorKind::Argument && !(1..=MAX_TIME).contains(&time)
    })?;
    let Some((valid, errors)) = core else {
        return Ok(Outcome::OutOfDomain);
    };
    let ours = validate_proposal(hex.as_bytes(), expiration, script, time, p);
    check(ours.is_ok() == valid, "proposal verdict")?;
    check(ours.err().unwrap_or_default() == errors, "proposal errors")?;
    Ok(verdict(valid))
}

fn one_object(oracle: &Oracle, fx: &Fixture, r: &mut Reader<'_>) -> Option<Compared> {
    let flags = r.u8()?;
    let clock = r.u8()? as i8;
    let modes = r.u8()?;
    let variant = &fx.variants[usize::from(flags >> 1 & 1)];
    let message = r.object(fx, variant)?;
    let now = fx.now + i64::from(clock) * 3600;
    Some(compare_object(oracle, fx, variant, &message, now, flags & 1 != 0, modes))
}

/// One object's record (`oracle_gov_object`) against the node's.
pub fn compare_object(
    oracle: &Oracle,
    fx: &Fixture,
    variant: &Variant,
    message: &[u8],
    now: i64,
    synced: bool,
    modes: u8,
) -> Compared {
    let ctx = fx.context(variant, now);
    let gov = fx.gov_context(synced, modes);
    let mut cursor = message;
    let ours = object::decode_network(&mut cursor).filter(|_| cursor.is_empty());
    let core = match oracle.gov_object(&fx.chain, message, &ctx, &gov) {
        Ok(core) => core,
        Err(e) if matches!(e.kind, ErrorKind::Decode | ErrorKind::Trailing) => {
            check(ours.is_none(), "decode verdict")?;
            return Ok(Outcome::BothReject);
        }
        Err(e) if record_not_utf8(&e) => {
            let ours = ours.ok_or(divergence("decode verdict"))?;
            let ours = object_record(fx, variant, &ours, modes);
            let text = [&ours.local_error, &ours.error];
            let trigger = ours.trigger.as_ref().err();
            return if text.iter().any(|t| lossy(t)) || trigger.is_some_and(|t| lossy(t)) {
                Ok(Outcome::OutOfDomain)
            } else {
                Err(divergence("oracle error"))
            };
        }
        Err(_) => return Err(divergence("oracle error")),
    };
    let ours = ours.ok_or(divergence("decode verdict"))?;
    let ours = object_record(fx, variant, &ours, modes);
    check(ours.hash == core.hash, "object hash")?;
    check(ours.data_hash == core.data_hash, "data hash")?;
    check(ours.signature_hash == core.signature_hash, "object signature hash")?;
    check(ours.object_type == core.object_type, "object type")?;
    check(ours.min_fee == core.min_fee, "min fee")?;
    check(ours.local_valid == core.local_valid, "local validity")?;
    check(ours.local_error == core.local_error, "local error")?;
    check(ours.valid == core.valid, "validity")?;
    check(ours.error == core.error, "error")?;
    check(ours.missing_confirmations == core.missing_confirmations, "missing confirmations")?;
    check(ours.trigger == core.trigger, "trigger")?;
    Ok(verdict(core.valid))
}

/// The node's [`GovObjectCheck`] of `obj`.
fn object_record(fx: &Fixture, variant: &Variant, obj: &GovObject, modes: u8) -> GovObjectCheck {
    let p = &fx.params;
    let obj_type = object::object_type(obj);
    let local = is_valid_locally(obj, false, Check::NoCollateral, p);
    let found = fx.find_collateral(&txid_bytes(&obj.collateral_hash), modes);
    let full = is_valid_locally(
        obj,
        false,
        Check::Collateral {
            list: &variant.list,
            found,
        },
        p,
    );
    GovObjectCheck {
        hash: object::hash(obj),
        data_hash: object::data_hash(obj),
        signature_hash: object::signature_hash(obj),
        object_type: obj_type,
        min_fee: min_collateral_fee(obj_type),
        local_valid: local.valid,
        local_error: local.error,
        valid: full.valid,
        error: full.error,
        missing_confirmations: full.missing_confirmations,
        trigger: Superblock::parse(obj, p).map(|sb| parsed_trigger(&sb, fx.tip, p)),
    }
}

fn expiry(sb: &Superblock, status: Status, height: i32, p: &GovParams) -> Expiry {
    let sb = Superblock {
        status,
        ..sb.clone()
    };
    match sb.is_expired(height, p) {
        None => Expiry::Overflow,
        Some(true) => Expiry::Expired,
        Some(false) => Expiry::Live,
    }
}

fn parsed_trigger(sb: &Superblock, tip: i32, p: &GovParams) -> dash_oracle_sys::ParsedTrigger {
    dash_oracle_sys::ParsedTrigger {
        height: sb.height,
        payments: sb
            .payments
            .iter()
            .map(|pay| dash_oracle_sys::GovPayment {
                script: pay.dest.script(),
                amount: pay.amount,
                proposal_hash: pay.proposal_hash,
            })
            .collect(),
        valid_height: is_valid_block_height(sb.height, p),
        payments_limit: payments_limit(sb.height, p).ok(),
        expired_valid: expiry(sb, Status::Valid, tip, p),
        expired_executed: expiry(sb, Status::Executed, tip, p),
        data: sb.hex_str_data(p),
    }
}

fn one_vote(oracle: &Oracle, fx: &Fixture, r: &mut Reader<'_>) -> Option<Compared> {
    let flags = r.u8()?;
    let clock = r.u8()? as i8;
    let variant = &fx.variants[usize::from(flags & 1)];
    let message = r.vote(fx, variant)?;
    Some(compare_vote(oracle, fx, variant, &message, fx.now + i64::from(clock) * 600))
}

/// One vote's record (`oracle_gov_vote`) against the node's.
pub fn compare_vote(
    oracle: &Oracle,
    fx: &Fixture,
    variant: &Variant,
    message: &[u8],
    now: i64,
) -> Compared {
    let ctx = fx.context(variant, now);
    let ours = decode_whole::<GovVote>(message)
        .filter(|v| matches!(v.sig.len(), vote::COMPACT_SIG_SIZE | vote::BLS_SIG_SIZE));
    let core = match oracle.gov_vote(&fx.chain, message, &ctx) {
        Ok(core) => core,
        Err(e) if matches!(e.kind, ErrorKind::Decode | ErrorKind::Trailing) => {
            check(ours.is_none(), "decode verdict")?;
            return Ok(Outcome::BothReject);
        }
        Err(_) => return Err(divergence("oracle error")),
    };
    let v = ours.ok_or(divergence("decode verdict"))?;
    let ours = vote_record(&v, &variant.list, now, &fx.params);
    check(ours.hash == core.hash, "vote hash")?;
    check(ours.signature_hash == core.signature_hash, "vote signature hash")?;
    check(ours.signature_string == core.signature_string, "signature string")?;
    check(ours.reads_list == core.reads_list, "reads list")?;
    check(ours.mn_found == core.mn_found, "masternode found")?;
    check(ours.valid_voting == core.valid_voting, "valid with the voting key")?;
    check(ours.valid_operator == core.valid_operator, "valid with the operator key")?;
    check(ours.valid_unknown_parent == core.valid_unknown_parent, "valid for an unknown parent")?;
    Ok(verdict(core.valid_voting || core.valid_operator))
}

fn vote_record(v: &GovVote, list: &MnList, now: i64, p: &GovParams) -> GovVoteCheck {
    let reads_list = vote::passes_static_checks(v, now);
    GovVoteCheck {
        hash: vote::hash(v),
        signature_hash: vote::signature_hash(v),
        signature_string: vote::signature_string(v),
        reads_list,
        mn_found: reads_list && list.get_by_collateral(&v.masternode_outpoint).is_some(),
        valid_voting: vote::is_valid(v, list, true, now, p),
        valid_operator: vote::is_valid(v, list, false, now, p),
        valid_unknown_parent: vote::is_valid_for_unknown_parent(v, list, now, p),
    }
}

/// `GOVERNANCE_ORPHAN_EXPIRATION_TIME`, ten minutes (`governance.cpp:35`).
const ORPHAN_EXPIRATION: i64 = 600;

/// The node's model of the oracle's `CGovernanceManager` (see the module docs).
struct Manager<'a> {
    fx: &'a Fixture,
    list: &'a MnList,
    modes: u8,
    synced: bool,
    now: i64,
    objects: BTreeMap<[u8; 32], ObjectState>,
    postponed: BTreeSet<[u8; 32]>,
    superblocks: SuperblockSet,
    accepted_votes: BTreeSet<[u8; 32]>,
    /// `cmmapOrphanVotes`: at most one vote per parent (see the module docs), with its expiry.
    orphans: BTreeMap<[u8; 32], (GovVote, i64)>,
    last_object: BTreeMap<OutpointKey, LastObjectRecord>,
}

/// An op the oracle refuses: the whole script is outside the domain.
struct Refused;

impl Manager<'_> {
    /// `ProcessObject` (`governance.cpp:227-275`) and `AddGovernanceObjectInternal` (rc.4 `:306-376`).
    fn object(&mut self, message: &[u8]) -> Result<Option<ObjectOutcome>, Refused> {
        let Some(obj) = object::decode_network(&mut &message[..]) else {
            return Ok(None);
        };
        let p = &self.fx.params;
        let hash = object::hash(&obj);
        let outcome = |ok, held, postponed| {
            Ok(Some(ObjectOutcome {
                hash,
                ok,
                held,
                postponed,
            }))
        };
        if self.objects.contains_key(&hash) || self.postponed.contains(&hash) {
            return outcome(true, self.objects.contains_key(&hash), self.postponed.contains(&hash));
        }
        let obj_type = object::object_type(&obj);
        let gated = self.synced && obj_type == TRIGGER;
        let key = outpoint_key(&obj.masternode_outpoint);
        let mut bypassed = false;
        if gated {
            let rc = masternode_rate_check(
                obj.time,
                self.now,
                self.last_object.get_mut(&key),
                false,
                true,
                p,
            );
            if !rc.ok {
                return outcome(true, false, false);
            }
            bypassed = rc.bypassed;
        }
        let found = self.fx.find_collateral(&txid_bytes(&obj.collateral_hash), self.modes);
        let check = Check::Collateral {
            list: self.list,
            found,
        };
        let Validity {
            valid,
            missing_confirmations,
            ..
        } = is_valid_locally(&obj, false, check, p);
        if bypassed
            && valid
            && !masternode_rate_check(
                obj.time,
                self.now,
                self.last_object.get_mut(&key),
                true,
                true,
                p,
            )
            .ok
        {
            return outcome(true, false, false);
        }
        if !valid {
            if missing_confirmations {
                self.postponed.insert(hash);
                return outcome(true, false, true);
            }
            return outcome(false, false, false);
        }
        let mut state = ObjectState::new(obj_type, hash);
        state.update_sentinel(self.list, self.now, p);
        if obj_type == TRIGGER {
            // `MasternodeRateUpdate` counts the attempt before `AddTrigger` can reject it
            // (rc.4, dash#7521).
            let record = self.last_object.entry(key).or_default();
            record.buffer.add(obj.time);
            record.status_ok = true;
            match self.superblocks.add_trigger(&obj, self.fx.tip, p) {
                Ok(true) => {}
                // `PrepareDeletion` and return (`governance.cpp:353-357`).
                Ok(false) => {
                    state.mark_for_deletion(self.now);
                    self.objects.insert(hash, state);
                    return outcome(true, true, false);
                }
                Err(_) => return Err(Refused),
            }
        }
        // `CheckOrphanVotes` (`governance.cpp:277-304`): the parent's orphan vote, rate checks
        // off, kept until it expires or is accepted; not added to the known-vote map.
        if let Some((v, expiry)) = self.orphans.get(&hash)
            && (*expiry < self.now || state.process_vote(v, self.list, false, self.now, p).is_ok())
        {
            self.orphans.remove(&hash);
        }
        self.objects.insert(hash, state);
        outcome(true, true, false)
    }

    /// `CGovernanceManager::ProcessVote` (`governance.cpp:802-854`).
    fn vote(&mut self, message: &[u8]) -> Option<VoteOutcome> {
        let v = vote::decode_network(message)?;
        let p = &self.fx.params;
        let hash = vote::hash(&v);
        let parent = txid_bytes(&v.parent_hash);
        let outcome = |ok, exception: Option<GovException>, to_request| {
            let e = exception.unwrap_or_else(|| GovException::new(ExceptionType::None, "", 0));
            VoteOutcome {
                hash,
                ok,
                exception: exception_type(e.kind()),
                message: e.message().to_owned(),
                penalty: e.penalty(),
                to_request,
            }
        };
        if self.accepted_votes.contains(&hash) {
            return Some(outcome(false, None, None));
        }
        let Some(state) = self.objects.get_mut(&parent) else {
            const F: &str = "CGovernanceManager::ProcessVote";
            let short = object::outpoint_short(&v.masternode_outpoint);
            if !vote::is_valid_for_unknown_parent(&v, self.list, self.now, p) {
                let msg = format!(
                    "{F} -- Invalid vote for unknown parent object {}, MN outpoint = {short}, vote hash = {}",
                    hash_hex(&parent),
                    hash_hex(&hash)
                );
                return Some(outcome(
                    false,
                    Some(GovException::new(ExceptionType::PermanentError, &msg, 20)),
                    None,
                ));
            }
            let msg = format!(
                "{F} -- Unknown parent object {}, MN outpoint = {short}",
                hash_hex(&parent)
            );
            // `Now() + GOVERNANCE_ORPHAN_EXPIRATION_TIME` (`governance.cpp:35, 834`).
            let to_request = match self.orphans.entry(parent) {
                std::collections::btree_map::Entry::Occupied(_) => None,
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert((v, self.now + ORPHAN_EXPIRATION));
                    Some(parent)
                }
            };
            return Some(outcome(
                false,
                Some(GovException::new(ExceptionType::Warning, &msg, 0)),
                to_request,
            ));
        };
        if state.cached_delete || state.expired {
            return Some(outcome(false, None, None));
        }
        Some(match state.process_vote(&v, self.list, true, self.now, p) {
            Ok(_) => {
                self.accepted_votes.insert(hash);
                outcome(true, None, None)
            }
            Err(e) => outcome(false, Some(e), None),
        })
    }

    /// STATE: the object's state over the tip's list.
    fn state(&self, hash: &[u8; 32]) -> Option<dash_oracle_sys::ObjectState> {
        let s = self.objects.get(hash)?;
        let tally = |signal| {
            let unique = s.unique_voters(self.list, signal);
            dash_oracle_sys::Tally {
                yes: s.yes(self.list, signal),
                no: s.no(self.list, signal),
                abstain: s.abstain(self.list, signal),
                unique_regular: unique.regular as u16,
                unique_evo: unique.evo as u16,
            }
        };
        Some(dash_oracle_sys::ObjectState {
            object_type: s.obj_type,
            cached_funding: s.cached_funding,
            cached_valid: s.cached_valid,
            cached_delete: s.cached_delete,
            cached_endorsed: s.cached_endorsed,
            dirty: s.dirty,
            expired: s.expired,
            deletion_time: s.deletion_time,
            tallies: [1, 2, 3, 4].map(tally),
            current_votes: s
                .current
                .iter()
                .map(|(op, instances)| {
                    let instances = instances
                        .iter()
                        .map(|(&signal, i)| dash_oracle_sys::VoteInstance {
                            signal,
                            outcome: i.outcome,
                            last_update: i.last_update,
                            creation_time: i.creation_time,
                        })
                        .collect();
                    (*op, instances)
                })
                .collect(),
            vote_count: s.file.len() as u32,
        })
    }

    /// BLOCK: `IsSuperblockTriggered`, `GetBestSuperblock` and `GetSuperblockPayments`.
    fn block(&mut self, height: i32) -> SuperblockOutcome {
        let p = &self.fx.params;
        let triggered =
            self.superblocks.is_triggered(self.list, height, &mut self.objects, self.now, p);
        let best = self.superblocks.best(self.list, height, &self.objects, p);
        let payments = self
            .superblocks
            .payments(self.list, height, &self.objects, p)
            .unwrap_or_default()
            .into_iter()
            .map(|o| CoreTxOut {
                value: o.value,
                script_pubkey: o.script_pubkey.as_bytes().to_vec(),
            })
            .collect();
        SuperblockOutcome {
            triggered,
            best,
            payments,
        }
    }
}

fn exception_type(kind: ExceptionType) -> GovExceptionType {
    match kind {
        ExceptionType::None => GovExceptionType::None,
        ExceptionType::Warning => GovExceptionType::Warning,
        ExceptionType::PermanentError => GovExceptionType::PermanentError,
        ExceptionType::TemporaryError => GovExceptionType::TemporaryError,
        ExceptionType::InternalError => GovExceptionType::InternalError,
    }
}

fn script(oracle: &Oracle, fx: &Fixture, r: &mut Reader<'_>) -> Option<Compared> {
    let flags = r.u8()?;
    let mask = r.u8()?;
    let modes = r.u8()?;
    let variant = &fx.variants[usize::from(flags & 1)];
    let mut ops: Vec<GovOp> = variant
        .objects
        .iter()
        .enumerate()
        .filter(|(i, _)| mask >> (i % 8) & 1 != 0)
        .map(|(_, o)| GovOp::Object(o.bytes()))
        .collect();
    if flags & 4 != 0 {
        ops.extend(variant.votes.iter().map(|v| GovOp::Vote(v.bytes())));
    }
    for _ in 0..r.u8()? % 17 {
        ops.push(match r.u8()? % 5 {
            0 => GovOp::Time(fx.now + i64::from(i16::from_le_bytes(r.array()?)) * 16),
            1 | 2 => GovOp::Vote(r.vote(fx, variant)?),
            3 => GovOp::Superblock(HEIGHTS[usize::from(r.u8()?) % HEIGHTS.len()]),
            _ => GovOp::Object(r.object(fx, variant)?),
        });
    }
    ops.extend(variant.objects.iter().filter_map(RawObject::hash).map(GovOp::State));
    ops.push(GovOp::Superblock(HEIGHTS[0]));
    Some(compare_script(oracle, fx, variant, flags & 2 != 0, modes, &ops))
}

/// An op script (`oracle_gov_ops`) against the node's model of the manager.
pub fn compare_script(
    oracle: &Oracle,
    fx: &Fixture,
    variant: &Variant,
    synced: bool,
    modes: u8,
    ops: &[GovOp],
) -> Compared {
    let ctx = fx.context(variant, fx.now);
    let gov = fx.gov_context(synced, modes);
    let core =
        answer(oracle.gov_ops(&fx.chain, &ctx, &gov, ops), |e| e.kind == ErrorKind::Argument)?;
    let mut m = Manager {
        fx,
        list: &variant.list,
        modes,
        synced,
        now: fx.now,
        objects: BTreeMap::new(),
        postponed: BTreeSet::new(),
        superblocks: SuperblockSet::new(),
        accepted_votes: BTreeSet::new(),
        orphans: BTreeMap::new(),
        last_object: BTreeMap::new(),
    };
    m.superblocks.set_loaded(true);
    let mut ours = Vec::new();
    for op in ops {
        match op {
            GovOp::Time(t) => {
                if !(1..=MAX_TIME).contains(t) {
                    return check(core.is_none(), "time refusal").map(|()| Outcome::OutOfDomain);
                }
                m.now = *t;
            }
            GovOp::Object(bytes) => match m.object(bytes) {
                Ok(o) => ours.push(GovOutput::Object(o)),
                Err(Refused) => {
                    return check(core.is_none(), "trigger refusal").map(|()| Outcome::OutOfDomain);
                }
            },
            GovOp::Vote(bytes) => ours.push(GovOutput::Vote(m.vote(bytes))),
            GovOp::State(hash) => ours.push(GovOutput::State(m.state(hash).map(Box::new))),
            GovOp::Superblock(height) => ours.push(GovOutput::Superblock(m.block(*height))),
        }
    }
    let Some(core) = core else {
        return Err(divergence("script refusal"));
    };
    check(core.len() == ours.len(), "output count")?;
    for (c, o) in core.iter().zip(&ours) {
        compare_output(c, o)?;
    }
    Ok(Outcome::BothAccept)
}

fn compare_output(core: &GovOutput, ours: &GovOutput) -> Result<(), Divergence> {
    match (core, ours) {
        (GovOutput::Object(c), GovOutput::Object(o)) => {
            check(c.is_some() == o.is_some(), "object decode")?;
            let (Some(c), Some(o)) = (c, o) else {
                return Ok(());
            };
            check(c.hash == o.hash, "object op hash")?;
            check(c.ok == o.ok, "object op verdict")?;
            check(c.held == o.held, "object held")?;
            check(c.postponed == o.postponed, "object postponed")
        }
        (GovOutput::Vote(c), GovOutput::Vote(o)) => {
            check(c.is_some() == o.is_some(), "vote decode")?;
            let (Some(c), Some(o)) = (c, o) else {
                return Ok(());
            };
            check(c.hash == o.hash, "vote op hash")?;
            check(c.ok == o.ok, "vote op verdict")?;
            check(c.exception == o.exception, "vote exception type")?;
            check(c.message == o.message, "vote exception message")?;
            check(c.penalty == o.penalty, "vote penalty")?;
            check(c.to_request == o.to_request, "parent to request")
        }
        (GovOutput::State(c), GovOutput::State(o)) => {
            check(c.is_some() == o.is_some(), "object found")?;
            let (Some(c), Some(o)) = (c, o) else {
                return Ok(());
            };
            check(c.object_type == o.object_type, "state type")?;
            check(
                (c.cached_funding, c.cached_valid, c.cached_delete, c.cached_endorsed)
                    == (o.cached_funding, o.cached_valid, o.cached_delete, o.cached_endorsed),
                "cached flags",
            )?;
            check(c.dirty == o.dirty, "dirty")?;
            check(c.expired == o.expired, "expired")?;
            check(c.deletion_time == o.deletion_time, "deletion time")?;
            for (ct, ot) in c.tallies.iter().zip(&o.tallies) {
                check((ct.yes, ct.no, ct.abstain) == (ot.yes, ot.no, ot.abstain), "tallies")?;
                check(
                    (ct.unique_regular, ct.unique_evo) == (ot.unique_regular, ot.unique_evo),
                    "unique voters",
                )?;
            }
            check(c.current_votes == o.current_votes, "current votes")?;
            check(c.vote_count == o.vote_count, "vote count")
        }
        (GovOutput::Superblock(c), GovOutput::Superblock(o)) => {
            check(c.triggered == o.triggered, "superblock triggered")?;
            check(c.best == o.best, "best superblock")?;
            check(c.payments == o.payments, "superblock payments")
        }
        _ => Err(divergence("output kind")),
    }
}

/// Writes inputs.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn case(case: u8) -> Self {
        Writer(vec![case])
    }

    pub fn u8(mut self, b: u8) -> Self {
        self.0.push(b);
        self
    }

    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }

    /// An explicit time.
    pub fn time(self, t: i64) -> Self {
        self.u8(1).bytes(&t.to_le_bytes())
    }
}

/// The Core vectors' proposal calls (`proposals.txt`): (expiration, script, time, hex).
pub fn core_proposals() -> Vec<(bool, bool, i64, String)> {
    include_str!("../../dash-oracle-sys/tests/data/gov/proposals.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter_map(|l| {
            let f: Vec<&str> = l.split(" | ").collect();
            let num = |i: usize, key: &str| f.get(i)?.strip_prefix(key)?.trim().parse::<i64>().ok();
            Some((
                num(1, "expiration")? == 1,
                num(2, "script")? == 1,
                num(3, "time")?,
                (*f.get(4)?).to_owned(),
            ))
        })
        .collect()
}

/// `diff_gov`'s seeds: every Core vector on every network, as hex text and as data; every
/// fixture object unedited, with each collateral mode and both lists; edited types, data,
/// signers and outpoints.
pub fn gov_seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for (expiration, script, time, hex) in core_proposals() {
        for net in 0..NETWORKS.len() as u8 {
            let flags = u8::from(expiration) | u8::from(script) << 1 | net << 2;
            seeds.push(
                Writer::case(CASE_PROPOSAL).u8(flags).time(time).u8(0).bytes(hex.as_bytes()).0,
            );
        }
    }
    let Some(fx) = fixture() else {
        return seeds;
    };
    for (vi, v) in fx.variants.iter().enumerate() {
        for i in 0..v.objects.len() as u8 {
            for modes in [0x00, 0x55, 0xaa, 0x01, 0x04] {
                seeds.push(
                    Writer::case(CASE_OBJECT)
                        .u8(1 | (vi as u8) << 1)
                        .u8(0)
                        .u8(modes)
                        .u8(i)
                        .u8(0)
                        .u8(0xff)
                        .0,
                );
            }
            // Each edit kind once, with the trigger re-signed.
            for kind in 0..9u8 {
                seeds.push(
                    Writer::case(CASE_OBJECT)
                        .u8(1 | (vi as u8) << 1)
                        .u8(0)
                        .u8(0)
                        .u8(i)
                        .u8(1)
                        .u8(kind)
                        .bytes(&[0x02; 40])
                        .u8(i % 6)
                        .0,
                );
            }
        }
        // Proposal data from Core's vectors, as the object's data.
        for (_, _, _, hex) in core_proposals().iter().take(16) {
            if let Some(data) = unhex(hex) {
                seeds.push(
                    Writer::case(CASE_OBJECT)
                        .u8((vi as u8) << 1)
                        .u8(0)
                        .u8(0)
                        .u8(0)
                        .u8(1)
                        .u8(4)
                        .u8(data.len().min(255) as u8)
                        .bytes(&data)
                        .u8(0xff)
                        .0,
                );
            }
        }
    }
    seeds
}

/// `diff_gov_vote`'s seeds: every fixture vote on both lists, edited in each field and signed
/// each way, at clocks around the hour bound; scripts with every object, node 0's votes, and
/// votes and blocks after them.
pub fn gov_vote_seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let Some(fx) = fixture() else {
        return seeds;
    };
    for (vi, v) in fx.variants.iter().enumerate() {
        let vi = vi as u8;
        for i in 0..v.votes.len().min(0xf0) as u8 {
            seeds.push(Writer::case(CASE_VOTE).u8(vi).u8(0).u8(i).u8(0).0);
        }
        for kind in 0..7u8 {
            for arg in [0x00u8, 0x02, 0x05, 0x09, 0x0a, 0x40] {
                seeds.push(
                    Writer::case(CASE_VOTE)
                        .u8(vi)
                        .u8(0)
                        .u8(kind)
                        .u8(2)
                        .u8(kind)
                        .u8(arg)
                        .bytes(&[0x81; 80])
                        .u8(6)
                        .u8(arg)
                        .0,
                );
            }
        }
        for clock in [0u8, 6, 7, 0xfa] {
            seeds.push(Writer::case(CASE_VOTE).u8(vi).u8(clock).u8(1).u8(0).0);
        }
        for flags in [vi, vi | 2, vi | 4, vi | 6] {
            seeds.push(Writer::case(CASE_SCRIPT).u8(flags).u8(0x1f).u8(0).u8(0).0);
            seeds.push(Writer::case(CASE_SCRIPT).u8(flags).u8(0x0f).u8(0).u8(0).0);
            // Votes again, a vote on a missing parent twice, blocks, a later time.
            seeds.push(
                Writer::case(CASE_SCRIPT)
                    .u8(flags)
                    .u8(0x1f)
                    .u8(0)
                    .u8(7)
                    .u8(1)
                    .u8(0)
                    .u8(0)
                    .u8(1)
                    .u8(3)
                    .u8(1)
                    .u8(1)
                    .u8(0x80)
                    .bytes(&[0x13; 32])
                    .u8(2)
                    .u8(3)
                    .u8(1)
                    .u8(1)
                    .u8(0x80)
                    .bytes(&[0x13; 32])
                    .u8(3)
                    .u8(0)
                    .u8(0)
                    .bytes(&900i16.to_le_bytes())
                    .u8(1)
                    .u8(5)
                    .u8(0)
                    .u8(3)
                    .u8(1)
                    .0,
            );
        }
    }
    // Generated objects on the re-keyed list, synced: the trigger with unparsable data, then
    // re-timed copies of it (several at one height), signed by its masternode's operator key
    // and, once, by another (a signature failure); and the proposals under each collateral mode.
    let retime = |dt: i64| [&[2u8, 1][..], &(fx.now + dt).to_le_bytes()].concat();
    let own = fx.variants[1]
        .objects
        .get(4)
        .and_then(|t| fx.members.iter().position(|m| *m == t.outpoint))
        .unwrap_or(0) as u8;
    for signer in [own, (own + 1) % 6] {
        let trigger = |w: Writer, edits: &[Vec<u8>]| {
            let w = w.u8(4).u8(4).u8(edits.len() as u8);
            edits.iter().fold(w, |w, e| w.bytes(e)).u8(signer)
        };
        // Four unparsable triggers, each counted by `MasternodeRateUpdate` before `AddTrigger`
        // rejects it (rc.4 `governance.cpp:348-357`), then valid ones that meet that record in the
        // rate check.
        let mut w = Writer::case(CASE_SCRIPT).u8(3).u8(0).u8(0).u8(7);
        for (i, data) in [&b"{}"[..], b"[]", b"1", b"x"].into_iter().enumerate() {
            let edit = [&[4u8, data.len() as u8][..], data].concat();
            w = trigger(w, &[edit, retime(-60 + i as i64)]);
        }
        let w = trigger(w, &[retime(-50)]);
        let w = trigger(w, &[retime(-120)]);
        seeds.push(trigger(w, &[]).0);
    }
    // Orphan votes, then their parents (`CheckOrphanVotes` replays one per parent), and the
    // same after the ten-minute expiry.
    for (vi, v) in fx.variants.iter().enumerate() {
        let votes = v.votes.len().min(8) as u8;
        let objects = v.objects.len().min(5) as u8;
        for later in [None, Some(44i16)] {
            let count = votes + objects + u8::from(later.is_some());
            let mut w = Writer::case(CASE_SCRIPT).u8(vi as u8).u8(0).u8(0).u8(count);
            for i in 0..votes {
                w = w.u8(1).u8(i).u8(0);
            }
            if let Some(dt) = later {
                w = w.u8(0).bytes(&dt.to_le_bytes());
            }
            for source in 0..objects {
                w = w.u8(4).u8(source).u8(0).u8(0xff);
            }
            seeds.push(w.0);
        }
    }
    for modes in [0x55u8, 0xaa, 0x01] {
        let mut w = Writer::case(CASE_SCRIPT).u8(1).u8(0).u8(modes).u8(4);
        for source in 0..4u8 {
            w = w.u8(4).u8(source).u8(0).u8(0xff);
        }
        seeds.push(w.0);
    }
    seeds
}

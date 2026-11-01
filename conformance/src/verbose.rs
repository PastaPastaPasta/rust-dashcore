//! Cross-check against dashd's `getblock <hash> 2` JSON.
//!
//! Every scalar leaf of the oracle's transaction JSON is assigned, by its normalized path (array
//! indices stripped, e.g. `vout[].scriptPubKey.hex`), to exactly one class of [`CLASSES`]:
//!
//! - [`Class::Lib`] (A): derivable from the transaction bytes and modelled field by field. Each
//!   library's decoded value is rendered into dashd's exact JSON format (see `dc_flat`/`bs_flat`
//!   and the independent renderers in [`crate::render`] and [`crate::script`]) and compared with
//!   the oracle (`verbose.field`, attributed to the library). An oracle value a library did not
//!   emit is `verbose.missing` against that library, unless that library's decode of the
//!   transaction or of its payload already failed (reported by `tx.decode`/`payload.*`).
//! - [`Class::Render`] (B): a rendering of class-A fields of the same object (`asm`, `desc`,
//!   `address`, `type`, decimal `value`, payout/share addresses, `instanceHash`). The harness
//!   renders the oracle's own source fields and compares the result with the oracle's rendering
//!   both ways (`verbose.render`, against the harness, since the source fields themselves are
//!   compared with the libraries as class A).
//! - [`Class::Raw`]: checked against the raw transaction bytes by a dedicated check here.
//! - [`Class::Chain`] (C): not a function of the transaction bytes (index, undo or chain data).
//!   Exact paths only; there are no prefix or subtree exemptions.
//! - Anything else (D) is a `harness-gap` finding: the harness neither compares nor exempts
//!   it. So is any value the harness cannot render.
//!
//! Canonical forms for class A: integers in decimal; hashes in display (reversed) hex; scripts,
//! payload bytes, BLS values and LLMQ member bitsets in forward hex; amounts in duffs; addresses
//! and endpoints exactly as dashd prints them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{AddrV2, NIEntry, NetInfo as BsNetInfo, SpecialPayload, Transaction as BsTx};
use dash_types::{Hashable, Numeric};
use dashcore::Transaction as DcTx;
use dashcore::hashes::{Hash, sha256d};
use dashcore::sml::masternode_list_entry::MasternodeNetInfo;
use dashcore::sml::masternode_list_entry::net_info::{Bip155Network, ExtNetInfo, NetInfoEntry};
use dashcore::transaction::special_transaction::TransactionPayload as Dc;
use dashcore::transaction::special_transaction::provider_registration::{
    CollateralShare, MasternodePayoutShare,
};
use dashcore::transaction::special_transaction::provider_update_service::ProTxVersion;
use dashcore::transaction::special_transaction::quorum_commitment::QuorumEntry;
use hex_conservative::DisplayHex;
use serde_json::Value;

use crate::check::{ORACLE_BLS, Sink, TxRef, parse_rev_hex};
use crate::payload::{bs_bytes, bs_global, dc_bytes, protx_scheme, qc_scheme};
use crate::render::{
    self, Entry, LegacyPorts, NetAddr, NetInfo, Params, Rendered, Service, netinfo_fields,
};
use crate::report::Side;
use crate::script;
use crate::split::TxSpan;

/// How a class-A value is canonicalized for comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    /// Lowercase hex.
    Hex,
    /// Compared exactly (addresses, endpoints).
    Text,
    /// A `ValueFromAmount` number, compared in duffs.
    Amount,
    /// `operatorReward`: basis points printed as a percentage.
    Percent,
}

/// The class of an oracle path; see the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Lib(Kind),
    Render,
    Raw(&'static str),
    Chain(&'static str),
}

use Class::{Chain, Lib, Raw, Render};
use Kind::{Amount, Hex, Int, Percent, Text};

const SPENT_INDEX: &str = "spent index (-spentindex): data of the spent output";
const UNDO: &str = "undo data of the spent output (verbosity 3)";

/// The classification of every oracle path the harness knows, by normalized path.
pub const CLASSES: &[(&str, Class)] = &[
    // Transaction (TxToUniv, src/core_write.cpp).
    ("txid", Lib(Hex)),
    ("version", Lib(Int)),
    ("type", Lib(Int)),
    ("size", Raw("verbose.size compares it with the raw transaction's length")),
    ("locktime", Lib(Int)),
    ("vin[].coinbase", Lib(Hex)),
    ("vin[].txid", Lib(Hex)),
    ("vin[].vout", Lib(Int)),
    ("vin[].scriptSig.asm", Render),
    ("vin[].scriptSig.hex", Lib(Hex)),
    ("vin[].sequence", Lib(Int)),
    ("vin[].value", Chain(SPENT_INDEX)),
    ("vin[].valueSat", Chain(SPENT_INDEX)),
    ("vin[].address", Chain(SPENT_INDEX)),
    ("vin[].prevout.generated", Chain(UNDO)),
    ("vin[].prevout.height", Chain(UNDO)),
    ("vin[].prevout.value", Chain(UNDO)),
    ("vin[].prevout.scriptPubKey.asm", Chain(UNDO)),
    ("vin[].prevout.scriptPubKey.desc", Chain(UNDO)),
    ("vin[].prevout.scriptPubKey.hex", Chain(UNDO)),
    ("vin[].prevout.scriptPubKey.address", Chain(UNDO)),
    ("vin[].prevout.scriptPubKey.type", Chain(UNDO)),
    ("vout[].value", Render),
    ("vout[].valueSat", Lib(Int)),
    ("vout[].n", Lib(Int)),
    ("vout[].scriptPubKey.asm", Render),
    ("vout[].scriptPubKey.desc", Render),
    ("vout[].scriptPubKey.hex", Lib(Hex)),
    ("vout[].scriptPubKey.address", Render),
    ("vout[].scriptPubKey.type", Render),
    ("vout[].spentTxId", Chain(SPENT_INDEX)),
    ("vout[].spentIndex", Chain(SPENT_INDEX)),
    ("vout[].spentHeight", Chain(SPENT_INDEX)),
    ("extraPayloadSize", Lib(Int)),
    ("extraPayload", Lib(Hex)),
    // For a special-version asset unlock dashd prints the payload fee instead (checked by
    // `check_unlock_fee`).
    ("fee", Chain("undo data: the spent outputs' amounts")),
    ("blockhash", Chain("chain position")),
    ("hex", Raw("verbose.hex compares it with the raw transaction")),
    ("instanceHash", Render),
    ("instantlock", Chain("InstantSend/ChainLock state")),
    ("instantlock_internal", Chain("InstantSend state")),
    ("chainlock", Chain("ChainLock state")),
    // CProRegTx::ToJson (src/evo/core_write.cpp).
    ("proRegTx.version", Lib(Int)),
    ("proRegTx.type", Lib(Int)),
    ("proRegTx.collateralHash", Lib(Hex)),
    ("proRegTx.collateralIndex", Lib(Int)),
    ("proRegTx.service", Lib(Text)),
    ("proRegTx.addresses.core_p2p[]", Lib(Text)),
    ("proRegTx.addresses.platform_p2p[]", Lib(Text)),
    ("proRegTx.addresses.platform_https[]", Lib(Text)),
    ("proRegTx.ownerAddress", Lib(Text)),
    ("proRegTx.votingAddress", Lib(Text)),
    ("proRegTx.shares[].amount", Lib(Int)),
    ("proRegTx.shares[].refundAddress", Render),
    ("proRegTx.shares[].refundScript", Lib(Hex)),
    ("proRegTx.shares[].rewardAddress", Render),
    ("proRegTx.shares[].rewardScript", Lib(Hex)),
    ("proRegTx.shares[].ownerAddress", Lib(Text)),
    ("proRegTx.earlyPeriodBlocks", Lib(Int)),
    ("proRegTx.earlyPenalty", Lib(Int)),
    ("proRegTx.payouts[].address", Render),
    ("proRegTx.payouts[].script", Lib(Hex)),
    ("proRegTx.payouts[].reward", Lib(Int)),
    ("proRegTx.payoutAddress", Lib(Text)),
    ("proRegTx.pubKeyOperator", Lib(Hex)),
    ("proRegTx.operatorReward", Lib(Percent)),
    ("proRegTx.platformNodeID", Lib(Hex)),
    ("proRegTx.platformP2PPort", Lib(Int)),
    ("proRegTx.platformHTTPPort", Lib(Int)),
    ("proRegTx.inputsHash", Lib(Hex)),
    // CProUpServTx::ToJson
    ("proUpServTx.version", Lib(Int)),
    ("proUpServTx.type", Lib(Int)),
    ("proUpServTx.proTxHash", Lib(Hex)),
    ("proUpServTx.service", Lib(Text)),
    ("proUpServTx.addresses.core_p2p[]", Lib(Text)),
    ("proUpServTx.addresses.platform_p2p[]", Lib(Text)),
    ("proUpServTx.addresses.platform_https[]", Lib(Text)),
    ("proUpServTx.operatorPayoutAddress", Lib(Text)),
    ("proUpServTx.platformNodeID", Lib(Hex)),
    ("proUpServTx.platformP2PPort", Lib(Int)),
    ("proUpServTx.platformHTTPPort", Lib(Int)),
    ("proUpServTx.inputsHash", Lib(Hex)),
    // CProUpRegTx::ToJson
    ("proUpRegTx.version", Lib(Int)),
    ("proUpRegTx.proTxHash", Lib(Hex)),
    ("proUpRegTx.votingAddress", Lib(Text)),
    ("proUpRegTx.payouts[].address", Render),
    ("proUpRegTx.payouts[].script", Lib(Hex)),
    ("proUpRegTx.payouts[].reward", Lib(Int)),
    ("proUpRegTx.payoutAddress", Lib(Text)),
    ("proUpRegTx.pubKeyOperator", Lib(Hex)),
    ("proUpRegTx.inputsHash", Lib(Hex)),
    // CProUpRevTx::ToJson
    ("proUpRevTx.version", Lib(Int)),
    ("proUpRevTx.proTxHash", Lib(Hex)),
    ("proUpRevTx.reason", Lib(Int)),
    ("proUpRevTx.inputsHash", Lib(Hex)),
    // CProDisTx::ToJson (v24)
    ("proDisTx.version", Lib(Int)),
    ("proDisTx.proTxHash", Lib(Hex)),
    ("proDisTx.actorIndex", Lib(Int)),
    ("proDisTx.sigCount", Lib(Int)),
    // CProUpShareTx::ToJson (v24)
    ("proUpShareTx.version", Lib(Int)),
    ("proUpShareTx.proTxHash", Lib(Hex)),
    ("proUpShareTx.shareIndex", Lib(Int)),
    ("proUpShareTx.rewardAddress", Lib(Text)),
    ("proUpShareTx.inputsHash", Lib(Hex)),
    // CProUpSharedRegTx::ToJson (v24)
    ("proUpSharedRegTx.version", Lib(Int)),
    ("proUpSharedRegTx.proTxHash", Lib(Hex)),
    ("proUpSharedRegTx.votingAddress", Lib(Text)),
    ("proUpSharedRegTx.pubKeyOperator", Lib(Hex)),
    ("proUpSharedRegTx.inputsHash", Lib(Hex)),
    ("proUpSharedRegTx.sigCount", Lib(Int)),
    // CCbTx::ToJson
    ("cbTx.version", Lib(Int)),
    ("cbTx.height", Lib(Int)),
    ("cbTx.merkleRootMNList", Lib(Hex)),
    ("cbTx.merkleRootQuorums", Lib(Hex)),
    ("cbTx.bestCLHeightDiff", Lib(Int)),
    ("cbTx.bestCLSignature", Lib(Hex)),
    ("cbTx.creditPoolBalance", Lib(Amount)),
    ("cbTx.merkleRootAssetUnlocks", Lib(Hex)),
    // CFinalCommitmentTxPayload::ToJson (src/llmq/core_write.cpp)
    ("qcTx.version", Lib(Int)),
    ("qcTx.height", Lib(Int)),
    ("qcTx.commitment.version", Lib(Int)),
    ("qcTx.commitment.llmqType", Lib(Int)),
    ("qcTx.commitment.quorumHash", Lib(Hex)),
    ("qcTx.commitment.quorumIndex", Lib(Int)),
    ("qcTx.commitment.signersCount", Lib(Int)),
    ("qcTx.commitment.signers", Lib(Hex)),
    ("qcTx.commitment.validMembersCount", Lib(Int)),
    ("qcTx.commitment.validMembers", Lib(Hex)),
    ("qcTx.commitment.quorumPublicKey", Lib(Hex)),
    ("qcTx.commitment.quorumVvecHash", Lib(Hex)),
    ("qcTx.commitment.quorumSig", Lib(Hex)),
    ("qcTx.commitment.membersSig", Lib(Hex)),
    // MNHFTxPayload::ToJson
    ("mnhfTx.version", Lib(Int)),
    ("mnhfTx.signal.versionBit", Lib(Int)),
    ("mnhfTx.signal.quorumHash", Lib(Hex)),
    ("mnhfTx.signal.sig", Lib(Hex)),
    // CAssetLockPayload::ToJson
    ("assetLockTx.version", Lib(Int)),
    ("assetLockTx.creditOutputs[].value", Render),
    ("assetLockTx.creditOutputs[].valueSat", Lib(Int)),
    ("assetLockTx.creditOutputs[].scriptPubKey.asm", Render),
    ("assetLockTx.creditOutputs[].scriptPubKey.hex", Lib(Hex)),
    ("assetLockTx.creditOutputs[].scriptPubKey.type", Render),
    ("assetLockTx.creditOutputs[].address", Render),
    // CAssetUnlockPayload::ToJson
    ("assetUnlockTx.version", Lib(Int)),
    ("assetUnlockTx.index", Lib(Int)),
    ("assetUnlockTx.fee", Lib(Int)),
    ("assetUnlockTx.requestedHeight", Lib(Int)),
    ("assetUnlockTx.quorumHash", Lib(Hex)),
    ("assetUnlockTx.quorumSig", Lib(Hex)),
];

/// Each class-B path with the oracle fields (class A, or `hex`) the harness renders it from.
pub const RENDERINGS: &[(&str, &[&str])] = &[
    ("vin[].scriptSig.asm", &["vin[].scriptSig.hex"]),
    ("vout[].value", &["vout[].valueSat"]),
    ("vout[].scriptPubKey.asm", &["vout[].scriptPubKey.hex"]),
    ("vout[].scriptPubKey.desc", &["vout[].scriptPubKey.hex"]),
    ("vout[].scriptPubKey.address", &["vout[].scriptPubKey.hex"]),
    ("vout[].scriptPubKey.type", &["vout[].scriptPubKey.hex"]),
    ("instanceHash", &["hex"]),
    ("proRegTx.shares[].refundAddress", &["proRegTx.shares[].refundScript"]),
    ("proRegTx.shares[].rewardAddress", &["proRegTx.shares[].rewardScript"]),
    ("proRegTx.payouts[].address", &["proRegTx.payouts[].script"]),
    ("proUpRegTx.payouts[].address", &["proUpRegTx.payouts[].script"]),
    ("assetLockTx.creditOutputs[].value", &["assetLockTx.creditOutputs[].valueSat"]),
    (
        "assetLockTx.creditOutputs[].scriptPubKey.asm",
        &["assetLockTx.creditOutputs[].scriptPubKey.hex"],
    ),
    (
        "assetLockTx.creditOutputs[].scriptPubKey.type",
        &["assetLockTx.creditOutputs[].scriptPubKey.hex"],
    ),
    (
        "assetLockTx.creditOutputs[].address",
        &["assetLockTx.creditOutputs[].scriptPubKey.hex", "assetLockTx.version"],
    ),
];

/// The class-A paths no flattener emits, because neither library models them yet. The
/// oracle's values are `verbose.missing` against a library whose payload decode succeeded.
/// Empty since dashcore models the v24 formats (share/payout lists, CbTx v4, types 10–12);
/// base-sdk's flattener leaves them out, but base-sdk fails those payload decodes.
pub const NOT_FLATTENED: &[&str] = &[];

/// The top-level keys of special-transaction payload objects. A library whose payload decode
/// failed is not also reported as missing their fields.
const PAYLOAD_OBJECTS: &[&str] = &[
    "proRegTx",
    "proUpServTx",
    "proUpRegTx",
    "proUpRevTx",
    "proDisTx",
    "proUpShareTx",
    "proUpSharedRegTx",
    "cbTx",
    "qcTx",
    "mnhfTx",
    "assetLockTx",
    "assetUnlockTx",
];

static CLASS_MAP: LazyLock<HashMap<&'static str, Class>> =
    LazyLock::new(|| CLASSES.iter().copied().collect());

/// A path with its array indices removed: `vout[3].scriptPubKey.hex` -> `vout[].scriptPubKey.hex`.
pub fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_index = false;
    for c in path.chars() {
        match c {
            '[' => {
                in_index = true;
                out.push('[');
            }
            ']' => {
                in_index = false;
                out.push(']');
            }
            _ if in_index => {}
            c => out.push(c),
        }
    }
    out
}

/// The class of a concrete or normalized path, if the harness knows it.
pub fn class_of(path: &str) -> Option<Class> {
    CLASS_MAP.get(normalize(path).as_str()).copied()
}

/// The class of an oracle leaf (see [`leaves`]). An empty object or array whose children would
/// be classified (`vin` of a quorum commitment, `addresses` of an empty netinfo) is covered:
/// its emptiness is checked by the class-A paths that would fall under it.
pub fn classify_leaf(path: &str, v: &Value) -> Option<Class> {
    let empty =
        v.as_object().is_some_and(|m| m.is_empty()) || v.as_array().is_some_and(|a| a.is_empty());
    class_of(path).or_else(|| {
        (empty && is_known_container(&normalize(path))).then_some(Chain("empty container"))
    })
}

/// Whether a normalized path is a container of some classified path (`proRegTx.addresses`).
fn is_known_container(norm: &str) -> bool {
    CLASSES.iter().any(|(p, _)| {
        p.strip_prefix(norm).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    })
}

type Flat = Vec<(String, Rendered)>;

fn rev(bytes: &[u8]) -> String {
    let mut b = bytes.to_vec();
    b.reverse();
    b.to_lower_hex_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.to_lower_hex_string()
}

/// Core's `BitsVectorToHexStr`: bit `i` goes to byte `i / 8`, bit `i % 8`.
fn bits_hex(bits: &[bool]) -> String {
    let mut bytes = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        bytes[i / 8] |= u8::from(b) << (i % 8);
    }
    hex(&bytes)
}

/// One library's flattened transaction, built with the chain parameters for addresses.
struct Flattener {
    out: Flat,
    params: Result<Params, String>,
}

impl Flattener {
    fn new(params: &Result<Params, String>) -> Self {
        Self {
            out: Flat::new(),
            params: params.clone(),
        }
    }

    /// Records a rendering. Flatteners may only emit class-A paths: anything else would be
    /// compared without being covered by the reverse check, so it becomes a harness gap.
    fn put(&mut self, path: String, v: Rendered) {
        let v = match class_of(&path) {
            Some(Lib(_)) => v,
            other => Err(format!("flattener emitted a path of class {other:?}, not class A")),
        };
        self.out.push((path, v));
    }

    fn kv(&mut self, path: impl Into<String>, v: impl ToString) {
        self.put(path.into(), Ok(v.to_string()));
    }

    /// `EncodeDestination(PKHash(key_id))`.
    fn key_address(&mut self, path: String, key_id: &[u8]) {
        let v = self.params.clone().and_then(|p| render::key_id_address(key_id, p));
        self.put(path, v);
    }

    /// `if (ExtractDestination(script, dest)) pushKV(path, EncodeDestination(dest))`.
    fn script_address(&mut self, path: String, script: &[u8]) {
        if let Some(d) = script::extract_destination(script) {
            let v = self.params.clone().map(|p| render::encode_destination(&d, p));
            self.put(path, v);
        }
    }

    /// `payoutAddress`: printed for payload versions below ExtAddr (3), which print `payouts`
    /// (or `shares`) instead, and only when ExtractDestination succeeds.
    fn payout_address(&mut self, obj: &str, version: u16, script: &[u8]) {
        if version < 3 {
            self.script_address(format!("{obj}.payoutAddress"), script);
        }
    }

    /// `PayoutListToJson` (src/evo/providertx_util.cpp); `address` is a rendering.
    fn payouts(&mut self, obj: &str, payouts: &[(&[u8], u16)]) {
        for (i, (script, reward)) in payouts.iter().enumerate() {
            self.kv(format!("{obj}.payouts[{i}].script"), hex(script));
            self.kv(format!("{obj}.payouts[{i}].reward"), reward);
        }
    }

    /// `addresses`, plus `service` and the platform ports of `-deprecatedrpc=service`.
    fn net_info(&mut self, obj: &str, ni: &NetInfo, evo: bool, ports: Option<LegacyPorts>) {
        for (k, v) in netinfo_fields(ni, evo, ports) {
            self.put(format!("{obj}.{k}"), v);
        }
    }

    /// `vin[i]`; dashd renders a coinbase input as its scriptSig alone.
    fn input(&mut self, i: usize, coinbase: bool, txid: String, vout: u32, sig: &[u8], seq: u32) {
        if coinbase {
            self.kv(format!("vin[{i}].coinbase"), hex(sig));
        } else {
            self.kv(format!("vin[{i}].txid"), txid);
            self.kv(format!("vin[{i}].vout"), vout);
            self.kv(format!("vin[{i}].scriptSig.hex"), hex(sig));
        }
        self.kv(format!("vin[{i}].sequence"), seq);
    }

    /// An output (`vout[i]` or an asset-lock credit output).
    fn output(&mut self, prefix: String, value: impl ToString, script_pubkey: &[u8]) {
        self.kv(format!("{prefix}.valueSat"), value);
        self.kv(format!("{prefix}.scriptPubKey.hex"), hex(script_pubkey));
    }

    /// `platformNodeID` (consensus-encoded, displayed reversed).
    fn node_id(&mut self, obj: &str, node_id: Option<Vec<u8>>) {
        if let Some(id) = node_id {
            self.kv(format!("{obj}.platformNodeID"), rev(&id));
        }
    }
}

/// A legacy (ADDRv1) service from dashcore's `SocketAddr` (IPv4 is stored IPv4-mapped).
fn dc_service(s: &std::net::SocketAddr) -> Service {
    let addr = match s.ip() {
        std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        std::net::IpAddr::V6(v6) => v6.octets(),
    };
    Service::from_legacy(addr, s.port())
}

/// dashcore's service address: v3 (ExtAddr) carries `ExtNetInfo`, whose platform entries
/// replace the ports; earlier versions one legacy service.
fn dc_net_info(service_address: &MasternodeNetInfo) -> NetInfo {
    match service_address {
        MasternodeNetInfo::Extended(ext) => dc_ext_net_info(ext),
        MasternodeNetInfo::Legacy(addr) => NetInfo::Legacy(Entry::service(dc_service(addr))),
    }
}

fn legacy_ports(p2p: Option<u16>, http: Option<u16>) -> Option<LegacyPorts> {
    Some(LegacyPorts {
        p2p: p2p?,
        http: http?,
    })
}

/// A v3 (ExtAddr) entry from dashcore, with dashcore's own verdict on its validity: the
/// harness does not re-validate it, so an address dashcore wrongly accepts is rendered as an
/// address, where Core prints `[invalid entry]`.
fn dc_entry(e: &NetInfoEntry) -> Entry {
    if !e.is_valid() {
        return Entry::Invalid;
    }
    match e {
        NetInfoEntry::Service {
            network,
            addr,
            port,
        } => {
            let b = addr.as_slice();
            let addr = match network {
                Bip155Network::Ipv4 => b.try_into().map(NetAddr::Ipv4),
                Bip155Network::Ipv6 => b.try_into().map(NetAddr::Ipv6),
                Bip155Network::TorV3 => b.try_into().map(NetAddr::Onion),
                Bip155Network::I2p => b.try_into().map(NetAddr::I2p),
                Bip155Network::Cjdns => b.try_into().map(NetAddr::Cjdns),
            };
            // `is_valid` checks the length, so the conversion cannot fail.
            addr.map_or(Entry::Invalid, |addr| {
                Entry::Service(Service {
                    addr,
                    port: *port,
                })
            })
        }
        NetInfoEntry::Domain {
            host,
            port,
        } => Entry::Domain {
            name: host.as_bytes().to_vec(),
            port: *port,
        },
        NetInfoEntry::Invalid | NetInfoEntry::Malformed(_) | NetInfoEntry::UnknownType(_) => {
            Entry::Invalid
        }
    }
}

/// dashcore's v3 `ExtNetInfo`, purpose lists in wire order.
pub(crate) fn dc_ext_net_info(n: &ExtNetInfo) -> NetInfo {
    NetInfo::Extended {
        version: n.version,
        groups: n
            .purposes
            .iter()
            .map(|(p, es)| (p.to_u8(), es.iter().map(dc_entry).collect()))
            .collect(),
    }
}

/// The ProTx version with extended addresses and DIP-0026 payouts.
const EXT_ADDR: u16 = ProTxVersion::ExtAddr as u16;

fn dc_payouts(payouts: Option<&[MasternodePayoutShare]>) -> Vec<(&[u8], u16)> {
    payouts.unwrap_or_default().iter().map(|p| (p.script_payout.as_bytes(), p.reward)).collect()
}

/// `ShareListToJson` (src/evo/providertx_util.cpp) and the early-dissolution terms; the
/// share addresses are renderings.
fn dc_shares(f: &mut Flattener, shares: &[CollateralShare], period: u32, penalty: i64) {
    for (i, s) in shares.iter().enumerate() {
        let o = format!("proRegTx.shares[{i}]");
        f.kv(format!("{o}.amount"), s.amount);
        f.kv(format!("{o}.refundScript"), hex(s.script_refund.as_bytes()));
        // An empty reward script pays the refund script.
        let reward = if s.script_reward.is_empty() {
            &s.script_refund
        } else {
            &s.script_reward
        };
        f.kv(format!("{o}.rewardScript"), hex(reward.as_bytes()));
        f.key_address(format!("{o}.ownerAddress"), &dc_bytes(&s.owner_key_hash));
    }
    f.kv("proRegTx.earlyPeriodBlocks", period);
    f.kv("proRegTx.earlyPenalty", penalty);
}

fn dc_flat(tx: &DcTx, params: &Result<Params, String>) -> Flat {
    let mut f = Flattener::new(params);
    let ty = tx.special_transaction_payload.as_ref().map_or(0, |p| p.get_type().to_u16());
    f.kv("txid", rev(&tx.txid().to_byte_array()));
    f.kv("version", tx.version);
    f.kv("type", ty);
    f.kv("locktime", tx.lock_time);
    let coinbase = tx.is_coin_base();
    for (i, x) in tx.input.iter().enumerate() {
        let (txid, vout) = (rev(&x.previous_output.txid.to_byte_array()), x.previous_output.vout);
        f.input(i, coinbase, txid, vout, x.script_sig.as_bytes(), x.sequence);
    }
    for (i, x) in tx.output.iter().enumerate() {
        f.kv(format!("vout[{i}].n"), i);
        f.output(format!("vout[{i}]"), x.value, x.script_pubkey.as_bytes());
    }
    if let Some(p) = &tx.special_transaction_payload
        && !matches!(p, Dc::ClassicalWithNonStandardVersionTypeBytesPayloadType(_))
    {
        let raw = dc_bytes(p);
        f.kv("extraPayloadSize", raw.len());
        f.kv("extraPayload", hex(&raw));
        dc_payload_flat(p, &mut f);
    }
    f.out
}

fn dc_qc_entry(e: &QuorumEntry, f: &mut Flattener) {
    let p = "qcTx.commitment";
    f.kv(format!("{p}.version"), e.version);
    f.kv(format!("{p}.llmqType"), u8::from(e.llmq_type));
    f.kv(format!("{p}.quorumHash"), rev(&dc_bytes(&e.quorum_hash)));
    f.kv(format!("{p}.quorumIndex"), e.quorum_index.unwrap_or(0));
    f.kv(format!("{p}.signersCount"), e.signers.iter().filter(|b| **b).count());
    f.kv(format!("{p}.signers"), bits_hex(&e.signers));
    f.kv(format!("{p}.validMembersCount"), e.valid_members.iter().filter(|b| **b).count());
    f.kv(format!("{p}.validMembers"), bits_hex(&e.valid_members));
    f.kv(format!("{p}.quorumVvecHash"), rev(&dc_bytes(&e.quorum_vvec_hash)));
    f.kv(format!("{p}.quorumPublicKey"), hex(&dc_bytes(&e.quorum_public_key)));
    f.kv(format!("{p}.quorumSig"), hex(&dc_bytes(&e.threshold_sig)));
    f.kv(format!("{p}.membersSig"), hex(&dc_bytes(&e.all_commitment_aggregated_signature)));
}

fn dc_payload_flat(p: &Dc, f: &mut Flattener) {
    match p {
        Dc::ProviderRegistrationPayloadType(a) => {
            let o = "proRegTx";
            let evo = a.masternode_type as u16 == 1;
            f.kv(format!("{o}.version"), a.version);
            f.kv(format!("{o}.type"), a.masternode_type as u16);
            let op = dc_bytes(&a.collateral_outpoint);
            f.kv(format!("{o}.collateralHash"), rev(&op[..32]));
            f.kv(
                format!("{o}.collateralIndex"),
                u32::from_le_bytes([op[32], op[33], op[34], op[35]]),
            );
            // v3 (ExtAddr) carries ExtNetInfo, whose platform entries replace the ports.
            let ni = dc_net_info(&a.service_address);
            f.net_info(o, &ni, evo, legacy_ports(a.platform_p2p_port, a.platform_http_port));
            // A shared masternode has one owner key per share and no top-level owner.
            let terms = a.shared_collateral.as_ref().filter(|terms| !terms.shares.is_empty());
            let shared = terms.is_some();
            if !shared {
                f.key_address(format!("{o}.ownerAddress"), &dc_bytes(&a.owner_key_hash));
            }
            f.key_address(format!("{o}.votingAddress"), &dc_bytes(&a.voting_key_hash));
            if let Some(terms) = terms {
                dc_shares(f, &terms.shares, terms.early_period_blocks, terms.early_penalty);
            } else if a.version >= EXT_ADDR {
                f.payouts(o, &dc_payouts(a.payouts.as_deref()));
            } else {
                f.payout_address(o, a.version, a.script_payout.as_bytes());
            }
            f.kv(format!("{o}.pubKeyOperator"), hex(&dc_bytes(&a.operator_public_key)));
            f.kv(format!("{o}.operatorReward"), a.operator_reward);
            f.node_id(o, a.platform_node_id.as_ref().map(dc_bytes));
            f.kv(format!("{o}.inputsHash"), rev(&dc_bytes(&a.inputs_hash)));
        }
        Dc::ProviderUpdateServicePayloadType(a) => {
            let o = "proUpServTx";
            let ty = a.mn_type.unwrap_or(0);
            f.kv(format!("{o}.version"), a.version);
            f.kv(format!("{o}.type"), ty);
            f.kv(format!("{o}.proTxHash"), rev(&dc_bytes(&a.pro_tx_hash)));
            let ni = dc_net_info(&a.service_address);
            f.net_info(o, &ni, ty == 1, legacy_ports(a.platform_p2p_port, a.platform_http_port));
            f.script_address(format!("{o}.operatorPayoutAddress"), a.script_payout.as_bytes());
            f.node_id(o, a.platform_node_id.as_ref().map(dc_bytes));
            f.kv(format!("{o}.inputsHash"), rev(&dc_bytes(&a.inputs_hash)));
        }
        Dc::ProviderUpdateRegistrarPayloadType(a) => {
            let o = "proUpRegTx";
            f.kv(format!("{o}.version"), a.version);
            f.kv(format!("{o}.proTxHash"), rev(&dc_bytes(&a.pro_tx_hash)));
            f.key_address(format!("{o}.votingAddress"), &dc_bytes(&a.voting_key_hash));
            if a.version >= EXT_ADDR {
                f.payouts(o, &dc_payouts(a.payouts.as_deref()));
            } else {
                f.payout_address(o, a.version, a.script_payout.as_bytes());
            }
            f.kv(format!("{o}.pubKeyOperator"), hex(&dc_bytes(&a.operator_public_key)));
            f.kv(format!("{o}.inputsHash"), rev(&dc_bytes(&a.inputs_hash)));
        }
        Dc::ProviderUpdateRevocationPayloadType(a) => {
            f.kv("proUpRevTx.version", a.version);
            f.kv("proUpRevTx.proTxHash", rev(&dc_bytes(&a.pro_tx_hash)));
            f.kv("proUpRevTx.reason", a.reason);
            f.kv("proUpRevTx.inputsHash", rev(&dc_bytes(&a.inputs_hash)));
        }
        Dc::CoinbasePayloadType(a) => {
            f.kv("cbTx.version", a.version);
            f.kv("cbTx.height", a.height);
            f.kv("cbTx.merkleRootMNList", rev(&dc_bytes(&a.merkle_root_masternode_list)));
            if a.version >= 2 {
                f.kv("cbTx.merkleRootQuorums", rev(&dc_bytes(&a.merkle_root_quorums)));
            }
            if let Some(d) = a.best_cl_height {
                f.kv("cbTx.bestCLHeightDiff", d);
            }
            if let Some(s) = &a.best_cl_signature {
                f.kv("cbTx.bestCLSignature", hex(&dc_bytes(s)));
            }
            if let Some(b) = a.asset_locked_amount {
                f.kv("cbTx.creditPoolBalance", b);
            }
            if let Some(r) = &a.merkle_root_asset_unlocks {
                f.kv("cbTx.merkleRootAssetUnlocks", rev(&dc_bytes(r)));
            }
        }
        Dc::QuorumCommitmentPayloadType(a) => {
            let raw = dc_bytes(a);
            if raw.len() >= 6 {
                f.kv("qcTx.version", u16::from_le_bytes([raw[0], raw[1]]));
                f.kv("qcTx.height", u32::from_le_bytes([raw[2], raw[3], raw[4], raw[5]]));
                if let Ok(e) = dashcore::consensus::deserialize::<QuorumEntry>(&raw[6..]) {
                    dc_qc_entry(&e, f);
                }
            }
        }
        Dc::MnhfSignalPayloadType(a) => {
            f.kv("mnhfTx.version", a.version);
            f.kv("mnhfTx.signal.versionBit", a.version_bit);
            f.kv("mnhfTx.signal.quorumHash", rev(&dc_bytes(&a.quorum_hash)));
            f.kv("mnhfTx.signal.sig", hex(&dc_bytes(&a.sig)));
        }
        Dc::AssetLockPayloadType(a) => {
            f.kv("assetLockTx.version", a.version);
            for (i, o) in a.credit_outputs.iter().enumerate() {
                let prefix = format!("assetLockTx.creditOutputs[{i}]");
                f.output(prefix, o.value, o.script_pubkey.as_bytes());
            }
        }
        Dc::AssetUnlockPayloadType(a) => {
            f.kv("assetUnlockTx.version", a.base.version);
            f.kv("assetUnlockTx.index", a.base.index);
            f.kv("assetUnlockTx.fee", a.base.fee);
            f.kv("assetUnlockTx.requestedHeight", a.request_info.request_height);
            f.kv("assetUnlockTx.quorumHash", rev(&dc_bytes(&a.request_info.quorum_hash)));
            f.kv("assetUnlockTx.quorumSig", hex(&dc_bytes(&a.quorum_sig)));
        }
        // CProDisTx::ToJson
        Dc::ProviderDissolvePayloadType(a) => {
            f.kv("proDisTx.version", a.version);
            f.kv("proDisTx.proTxHash", rev(&dc_bytes(&a.pro_tx_hash)));
            f.kv("proDisTx.actorIndex", a.actor_index);
            f.kv("proDisTx.sigCount", a.signatures.len());
        }
        // CProUpShareTx::ToJson: `rewardAddress` only when ExtractDestination succeeds.
        Dc::ProviderUpdateSharePayloadType(a) => {
            let o = "proUpShareTx";
            f.kv(format!("{o}.version"), a.version);
            f.kv(format!("{o}.proTxHash"), rev(&dc_bytes(&a.pro_tx_hash)));
            f.kv(format!("{o}.shareIndex"), a.share_index);
            f.script_address(format!("{o}.rewardAddress"), a.script_reward.as_bytes());
            f.kv(format!("{o}.inputsHash"), rev(&dc_bytes(&a.inputs_hash)));
        }
        // CProUpSharedRegTx::ToJson. The operator key is always in the basic scheme on the
        // wire, and Core prints it in the scheme it was read with.
        Dc::ProviderUpdateSharedRegistrarPayloadType(a) => {
            let o = "proUpSharedRegTx";
            f.kv(format!("{o}.version"), a.version);
            f.kv(format!("{o}.proTxHash"), rev(&dc_bytes(&a.pro_tx_hash)));
            f.key_address(format!("{o}.votingAddress"), &dc_bytes(&a.voting_key_hash));
            f.kv(format!("{o}.pubKeyOperator"), hex(&dc_bytes(&a.operator_public_key)));
            f.kv(format!("{o}.inputsHash"), rev(&dc_bytes(&a.inputs_hash)));
            f.kv(format!("{o}.sigCount"), a.signatures.len());
        }
        // TxToUniv prints no payload object for a type it does not know, nor for a known type
        // whose payload GetTxPayload cannot read: only `extraPayloadSize` and `extraPayload`,
        // which `dc_flat` emits from the raw bytes.
        Dc::ClassicalWithNonStandardVersionTypeBytesPayloadType(_) | Dc::RawPayloadType(..) => {}
    }
}

/// base-sdk's network entry as it decoded it. The entry is built without the harness's own
/// validation, so an entry base-sdk wrongly keeps reaches the comparison.
fn bs_entry(e: &NIEntry) -> Entry {
    match e {
        NIEntry::Service(s) => {
            let addr = match &s.addr {
                AddrV2::Ipv4(b) => NetAddr::Ipv4(*b),
                AddrV2::Ipv6(b) => NetAddr::Ipv6(*b),
                AddrV2::TorV3(b) => NetAddr::Onion(*b),
                AddrV2::I2p(b) => NetAddr::I2p(*b),
                AddrV2::Cjdns(b) => NetAddr::Cjdns(*b),
                AddrV2::Unknown {
                    ..
                } => return Entry::Invalid,
            };
            Entry::Service(Service {
                addr,
                port: s.port,
            })
        }
        NIEntry::Domain {
            name,
            port,
        } => Entry::Domain {
            name: name.clone(),
            port: *port,
        },
        NIEntry::Invalid => Entry::Invalid,
    }
}

pub(crate) fn bs_net_info(n: &BsNetInfo) -> NetInfo {
    match n {
        // base-sdk holds a service Core reads as empty as the all-zero service.
        BsNetInfo::Legacy(v1) if v1.0.addr.0 == [0; 16] && v1.0.port == 0 => {
            NetInfo::Legacy(Entry::Invalid)
        }
        BsNetInfo::Legacy(v1) => {
            NetInfo::Legacy(Entry::Service(Service::from_legacy(v1.0.addr.0, v1.0.port)))
        }
        BsNetInfo::Extended(v2) => NetInfo::Extended {
            version: v2.version,
            groups: v2
                .entries
                .iter()
                .map(|(p, es)| (p.to_base(), es.iter().map(bs_entry).collect()))
                .collect(),
        },
    }
}

fn bs_flat(tx: &BsTx, params: &Result<Params, String>) -> Flat {
    let mut f = Flattener::new(params);
    f.kv("txid", rev(&tx.hash().to_lendian()));
    f.kv("version", tx.version as u16);
    f.kv("type", tx.tx_type.to_base());
    f.kv("locktime", tx.lock_time);
    let coinbase = tx.inputs.len() == 1 && tx.inputs[0].prevout.is_null();
    for (i, x) in tx.inputs.iter().enumerate() {
        let (txid, vout) = (rev(&bs_bytes(&x.prevout.hash)), x.prevout.index);
        f.input(i, coinbase, txid, vout, x.script_sig.as_bytes(), x.sequence);
    }
    for (i, x) in tx.outputs.iter().enumerate() {
        f.kv(format!("vout[{i}].n"), i);
        f.output(format!("vout[{i}]"), x.value, x.script_pubkey.as_bytes());
    }
    if !tx.extra_payload.is_empty() {
        f.kv("extraPayloadSize", tx.extra_payload.len());
        f.kv("extraPayload", hex(&tx.extra_payload));
    }
    if let Some(Ok(d)) = tx.decode_payload()
        && d.is_complete()
    {
        bs_payload_flat(&d.payload, &mut f);
    }
    f.out
}

/// `ShareListToJson` for base-sdk's shares; see [`dc_shares`].
fn bs_shares(f: &mut Flattener, b: &dash_primitives::ProRegTx) {
    for (i, s) in b.shares.iter().enumerate() {
        let o = format!("proRegTx.shares[{i}]");
        let reward = if s.script_reward.is_empty() {
            &s.script_refund
        } else {
            &s.script_reward
        };
        f.kv(format!("{o}.amount"), s.amount);
        f.kv(format!("{o}.refundScript"), hex(s.script_refund.as_bytes()));
        f.kv(format!("{o}.rewardScript"), hex(reward.as_bytes()));
        f.key_address(format!("{o}.ownerAddress"), &bs_bytes(&s.key_id_owner));
    }
    f.kv("proRegTx.earlyPeriodBlocks", b.early_period_blocks);
    f.kv("proRegTx.earlyPenalty", b.early_penalty);
}

fn bs_payouts(payouts: &[dash_primitives::PayoutShare]) -> Vec<(&[u8], u16)> {
    payouts.iter().map(|p| (p.script_payout.as_bytes(), p.reward)).collect()
}

/// Core prints a lazy operator key through `Get()`, under the scheme it was read with, which
/// the caller gives by Core's rule.
fn lazy_key(key: &dash_pkc::bls::BlsLazyPk, scheme: BlsSchemeKind) -> String {
    hex(&key.get().to_bytes(scheme))
}

fn bs_payload_flat(p: &SpecialPayload, f: &mut Flattener) {
    match p {
        SpecialPayload::ProviderRegister(b) => {
            let o = "proRegTx";
            let evo = b.mn_type.to_base() == 1;
            f.kv(format!("{o}.version"), u16::from(b.version));
            f.kv(format!("{o}.type"), b.mn_type.to_base());
            f.kv(format!("{o}.collateralHash"), rev(&bs_bytes(&b.collateral_hash)));
            f.kv(format!("{o}.collateralIndex"), b.collateral_index);
            let ports = legacy_ports(b.platform_p2p_port, b.platform_http_port);
            f.net_info(o, &bs_net_info(&b.net_info), evo, ports);
            // A shared masternode has one owner key per share and no top-level owner.
            let shared = !b.shares.is_empty();
            if !shared {
                f.key_address(format!("{o}.ownerAddress"), &bs_bytes(&b.key_id_owner));
            }
            f.key_address(format!("{o}.votingAddress"), &bs_bytes(&b.key_id_voting));
            if shared {
                bs_shares(f, b);
            } else if u16::from(b.version) >= EXT_ADDR {
                f.payouts(o, &bs_payouts(&b.payouts));
            } else {
                f.payout_address(o, u16::from(b.version), b.script_payout.as_bytes());
            }
            f.kv(
                format!("{o}.pubKeyOperator"),
                lazy_key(&b.pub_key_operator, protx_scheme(u16::from(b.version))),
            );
            f.kv(format!("{o}.operatorReward"), b.operator_reward);
            f.node_id(o, b.platform_node_id.as_ref().map(bs_bytes));
            f.kv(format!("{o}.inputsHash"), rev(&bs_bytes(&b.inputs_hash)));
        }
        SpecialPayload::ProviderUpdateService(b) => {
            let o = "proUpServTx";
            let evo = b.mn_type.to_base() == 1;
            f.kv(format!("{o}.version"), u16::from(b.version));
            f.kv(format!("{o}.type"), b.mn_type.to_base());
            f.kv(format!("{o}.proTxHash"), rev(&bs_bytes(&b.pro_tx_hash)));
            let ports = legacy_ports(b.platform_p2p_port, b.platform_http_port);
            f.net_info(o, &bs_net_info(&b.net_info), evo, ports);
            let payout = b.script_operator_payout.as_bytes();
            f.script_address(format!("{o}.operatorPayoutAddress"), payout);
            f.node_id(o, b.platform_node_id.as_ref().map(bs_bytes));
            f.kv(format!("{o}.inputsHash"), rev(&bs_bytes(&b.inputs_hash)));
        }
        SpecialPayload::ProviderUpdateRegistrar(b) => {
            let o = "proUpRegTx";
            f.kv(format!("{o}.version"), u16::from(b.version));
            f.kv(format!("{o}.proTxHash"), rev(&bs_bytes(&b.pro_tx_hash)));
            f.key_address(format!("{o}.votingAddress"), &bs_bytes(&b.key_id_voting));
            if u16::from(b.version) >= EXT_ADDR {
                f.payouts(o, &bs_payouts(&b.payouts));
            } else {
                f.payout_address(o, u16::from(b.version), b.script_payout.as_bytes());
            }
            f.kv(
                format!("{o}.pubKeyOperator"),
                lazy_key(&b.pub_key_operator, protx_scheme(u16::from(b.version))),
            );
            f.kv(format!("{o}.inputsHash"), rev(&bs_bytes(&b.inputs_hash)));
        }
        SpecialPayload::ProviderUpdateRevoke(b) => {
            f.kv("proUpRevTx.version", u16::from(b.version));
            f.kv("proUpRevTx.proTxHash", rev(&bs_bytes(&b.pro_tx_hash)));
            f.kv("proUpRevTx.reason", b.reason.to_base());
            f.kv("proUpRevTx.inputsHash", rev(&bs_bytes(&b.inputs_hash)));
        }
        SpecialPayload::CoinbaseCommitment(b) => {
            f.kv("cbTx.version", b.version);
            f.kv("cbTx.height", b.height);
            f.kv("cbTx.merkleRootMNList", rev(&bs_bytes(&b.merkle_root_mn_list)));
            if let Some(r) = &b.merkle_root_quorums {
                f.kv("cbTx.merkleRootQuorums", rev(&bs_bytes(r)));
            }
            if let Some(d) = b.best_cl_height_diff {
                f.kv("cbTx.bestCLHeightDiff", d);
            }
            // Printed under Core's flag, as it was decoded.
            if let Some(s) = &b.best_cl_signature {
                f.kv("cbTx.bestCLSignature", hex(&bs_global(s)));
            }
            if let Some(v) = b.credit_pool_balance {
                f.kv("cbTx.creditPoolBalance", v);
            }
            if let Some(r) = &b.merkle_root_asset_unlocks {
                f.kv("cbTx.merkleRootAssetUnlocks", rev(&bs_bytes(r)));
            }
        }
        SpecialPayload::QuorumCommitment(b) => {
            let q = &b.commitment;
            let p = "qcTx.commitment";
            f.kv("qcTx.version", b.version);
            f.kv("qcTx.height", b.height.to_u32());
            f.kv(format!("{p}.version"), q.version);
            f.kv(format!("{p}.llmqType"), q.llmq_type.to_base());
            f.kv(format!("{p}.quorumHash"), rev(&bs_bytes(&q.quorum_hash)));
            f.kv(format!("{p}.quorumIndex"), q.quorum_index.unwrap_or(0));
            let bitset = |b: &dash_primitives::DynBitset| {
                let bits: Vec<bool> = (0..b.num_bits).map(|i| b.get(i) == Some(true)).collect();
                bits_hex(&bits)
            };
            f.kv(format!("{p}.signersCount"), q.signers.count_ones());
            f.kv(format!("{p}.signers"), bitset(&q.signers));
            f.kv(format!("{p}.validMembersCount"), q.valid_members.count_ones());
            f.kv(format!("{p}.validMembers"), bitset(&q.valid_members));
            f.kv(format!("{p}.quorumVvecHash"), rev(&bs_bytes(&q.quorum_vvec_hash)));
            // Printed under the version's scheme.
            let scheme = qc_scheme(q.version);
            f.kv(format!("{p}.quorumPublicKey"), hex(&q.quorum_public_key.to_bytes(scheme)));
            f.kv(format!("{p}.quorumSig"), hex(&q.quorum_sig.to_bytes(scheme)));
            f.kv(format!("{p}.membersSig"), hex(&q.members_sig.to_bytes(scheme)));
        }
        SpecialPayload::MnhfSignal(b) => {
            f.kv("mnhfTx.version", b.version);
            f.kv("mnhfTx.signal.versionBit", b.version_bit);
            f.kv("mnhfTx.signal.quorumHash", rev(&bs_bytes(&b.quorum_hash)));
            // Decoded under the basic scheme, printed under Core's flag.
            f.kv("mnhfTx.signal.sig", hex(&b.sig.to_bytes(ORACLE_BLS.scheme())));
        }
        SpecialPayload::AssetLock(b) => {
            f.kv("assetLockTx.version", b.version);
            for (i, o) in b.credit_outputs.iter().enumerate() {
                let prefix = format!("assetLockTx.creditOutputs[{i}]");
                f.output(prefix, o.value, o.script_pubkey.as_bytes());
            }
        }
        SpecialPayload::AssetUnlock(b) => {
            f.kv("assetUnlockTx.version", b.version);
            f.kv("assetUnlockTx.index", b.index);
            f.kv("assetUnlockTx.fee", b.fee);
            f.kv("assetUnlockTx.requestedHeight", b.requested_height);
            f.kv("assetUnlockTx.quorumHash", rev(&bs_bytes(&b.quorum_hash)));
            // Printed under Core's flag, as it was decoded.
            f.kv("assetUnlockTx.quorumSig", hex(&bs_global(&b.quorum_sig)));
        }
        // CProDisTx::ToJson
        SpecialPayload::ProviderDissolve(b) => {
            f.kv("proDisTx.version", b.version);
            f.kv("proDisTx.proTxHash", rev(&bs_bytes(&b.pro_tx_hash)));
            f.kv("proDisTx.actorIndex", b.actor_index);
            f.kv("proDisTx.sigCount", b.sigs.len());
        }
        // CProUpShareTx::ToJson: `rewardAddress` only when ExtractDestination succeeds.
        SpecialPayload::ProviderUpdateShare(b) => {
            let o = "proUpShareTx";
            f.kv(format!("{o}.version"), b.version);
            f.kv(format!("{o}.proTxHash"), rev(&bs_bytes(&b.pro_tx_hash)));
            f.kv(format!("{o}.shareIndex"), b.share_index);
            f.script_address(format!("{o}.rewardAddress"), b.script_reward.as_bytes());
            f.kv(format!("{o}.inputsHash"), rev(&bs_bytes(&b.inputs_hash)));
        }
        // CProUpSharedRegTx::ToJson; the operator key is always in the basic scheme.
        SpecialPayload::ProviderUpdateSharedRegistrar(b) => {
            let o = "proUpSharedRegTx";
            f.kv(format!("{o}.version"), b.version);
            f.kv(format!("{o}.proTxHash"), rev(&bs_bytes(&b.pro_tx_hash)));
            f.key_address(format!("{o}.votingAddress"), &bs_bytes(&b.key_id_voting));
            f.kv(
                format!("{o}.pubKeyOperator"),
                lazy_key(&b.pub_key_operator, BlsSchemeKind::Basic),
            );
            f.kv(format!("{o}.inputsHash"), rev(&bs_bytes(&b.inputs_hash)));
            f.kv(format!("{o}.sigCount"), b.sigs.len());
        }
        // No valid chain carries a ProTx of unknown version; Core would print its version only.
        SpecialPayload::UnknownProTxVersion {
            ..
        }
        | SpecialPayload::Unknown {
            ..
        } => {}
    }
}

/// The class-B renderings of the oracle's own source fields (see [`RENDERINGS`]), by concrete
/// path. A rendering whose source is absent or unreadable is an `Err`.
fn render_oracle(
    otx: &Value,
    raw: &[u8],
    params: &Result<Params, String>,
) -> BTreeMap<String, Rendered> {
    let mut out = BTreeMap::new();
    let script_of = |v: &Value, key: &str| -> Result<Vec<u8>, String> {
        let s = v
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("source field {key} absent"))?;
        hex_conservative::decode_to_vec(s).map_err(|e| format!("source field {key}: {e}"))
    };
    let amount = |o: &Value, prefix: &str, out: &mut BTreeMap<String, Rendered>| {
        let v = o
            .get("valueSat")
            .and_then(Value::as_i64)
            .map(render::value_from_amount)
            .ok_or_else(|| "source field valueSat absent".to_string());
        out.insert(format!("{prefix}.value"), v);
    };
    // Core omits the key when ExtractDestination fails; PayoutListToJson/ShareListToJson
    // print EncodeDestination(CNoDestination), the empty string.
    let address_or_empty = |script: &Result<Vec<u8>, String>| -> Rendered {
        let script = script.clone()?;
        Ok(match script::extract_destination(&script) {
            Some(d) => render::encode_destination(&d, params.clone()?),
            None => String::new(),
        })
    };

    let spk_hex = |o: &Value| script_of(o.get("scriptPubKey").unwrap_or(&Value::Null), "hex");
    fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
        v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
    }

    for (i, vin) in arr(otx, "vin").iter().enumerate() {
        if let Some(sig) = vin.get("scriptSig") {
            let asm = script_of(sig, "hex").map(|s| script::asm(&s, true));
            out.insert(format!("vin[{i}].scriptSig.asm"), asm);
        }
    }
    for (i, vout) in arr(otx, "vout").iter().enumerate() {
        let prefix = format!("vout[{i}]");
        amount(vout, &prefix, &mut out);
        let fields =
            spk_hex(vout).and_then(|s| Ok(script::script_to_univ(&s, true, params.clone()?)));
        insert_script_fields(&mut out, &prefix, fields, &["asm", "desc", "address", "type"]);
    }
    if otx.get("instanceHash").is_some() || stable_txid_unlock(otx) {
        let h = sha256d::Hash::hash(raw).to_byte_array();
        out.insert("instanceHash".into(), Ok(rev(&h)));
    }
    if let Some(al) = otx.get("assetLockTx") {
        let version = al.get("version").and_then(Value::as_u64);
        for (i, o) in arr(al, "creditOutputs").iter().enumerate() {
            let prefix = format!("assetLockTx.creditOutputs[{i}]");
            amount(o, &prefix, &mut out);
            let script = spk_hex(o);
            // Without addresses the parameters are unused.
            let fields = script
                .as_ref()
                .map(|s| script::script_to_univ(s, false, Params::Main))
                .map_err(String::clone);
            insert_script_fields(&mut out, &prefix, fields, &["asm", "type"]);
            // `address` (DIP-18) only for version >= 2 and a P2PKH/P2SH credit script.
            let address = match (version, &script, params) {
                (None, _, _) => Some(Err("source field assetLockTx.version absent".to_string())),
                (_, Err(e), _) | (_, _, Err(e)) => Some(Err(e.clone())),
                (Some(v), Ok(s), Ok(p)) => (v >= 2)
                    .then(|| script::extract_destination(s))
                    .flatten()
                    .map(|d| Ok(render::platform_address(&d, *p))),
            };
            if let Some(a) = address {
                out.insert(format!("{prefix}.address"), a);
            }
        }
    }
    for obj in ["proRegTx", "proUpRegTx"] {
        let Some(o) = otx.get(obj) else {
            continue;
        };
        for (i, p) in arr(o, "payouts").iter().enumerate() {
            out.insert(
                format!("{obj}.payouts[{i}].address"),
                address_or_empty(&script_of(p, "script")),
            );
        }
        for (i, s) in arr(o, "shares").iter().enumerate() {
            for (key, source) in
                [("refundAddress", "refundScript"), ("rewardAddress", "rewardScript")]
            {
                let v = address_or_empty(&script_of(s, source));
                out.insert(format!("{obj}.shares[{i}].{key}"), v);
            }
        }
    }
    out
}

/// For a special-version asset unlock (`IsPlatformTransfer`), dashd's `fee` is the payload fee
/// rather than undo data: check it against the oracle's own `assetUnlockTx.fee`.
fn check_unlock_fee(sink: &mut Sink<'_>, txref: TxRef<'_>, otx: &Value) {
    let special = otx.get("version").and_then(Value::as_u64).is_some_and(|v| v >= 3);
    let (Some(fee), true, Some(9)) =
        (otx.get("fee"), special, otx.get("type").and_then(Value::as_u64))
    else {
        return;
    };
    match otx.pointer("/assetUnlockTx/fee").and_then(Value::as_i64) {
        Some(f) => {
            let r = render::value_from_amount(f);
            if !rendering_matches(fee, &r) {
                let detail = "an asset unlock's fee is its payload fee";
                render_mismatch(sink, txref, "fee", show(fee), r, detail);
            }
        }
        None => {
            let detail =
                "an asset unlock's fee is its payload fee, but assetUnlockTx.fee is absent";
            harness_gap(sink, Some(txref), "fee", Some(show(fee)), detail.into());
        }
    }
}

/// `IsAssetUnlockWithStableTxid` (src/primitives/transaction.h), from the oracle's class-A
/// `version`, `type` and `extraPayload`: a special-version asset unlock with a 145-byte payload
/// of version 2 or later. dashd then prints `instanceHash`.
fn stable_txid_unlock(otx: &Value) -> bool {
    let payload = otx.get("extraPayload").and_then(Value::as_str).unwrap_or_default();
    otx.get("version").and_then(Value::as_u64).is_some_and(|v| v >= 3)
        && otx.get("type").and_then(Value::as_u64) == Some(9)
        && payload.len() == 2 * 145
        && u8::from_str_radix(&payload[..2], 16).is_ok_and(|v| v >= 2)
}

/// Inserts the `ScriptToUniv` `fields` of the script at `{prefix}.scriptPubKey`, or, when the
/// script is unreadable, the error under each of the `keys` dashd would print.
fn insert_script_fields(
    out: &mut BTreeMap<String, Rendered>,
    prefix: &str,
    fields: Result<Vec<(&'static str, String)>, String>,
    keys: &[&str],
) {
    match fields {
        Ok(fields) => {
            for (k, v) in fields {
                out.insert(format!("{prefix}.scriptPubKey.{k}"), Ok(v));
            }
        }
        Err(e) => {
            for k in keys {
                out.insert(format!("{prefix}.scriptPubKey.{k}"), Err(e.clone()));
            }
        }
    }
}

/// Looks up a dotted path with `[i]` indices, e.g. `vin[0].scriptSig.hex`.
fn lookup<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        let (name, idx) = match part.find('[') {
            Some(p) => (&part[..p], part[p + 1..part.len() - 1].parse::<usize>().ok()),
            None => (part, None),
        };
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        if let Some(i) = idx {
            cur = cur.get(i)?;
        }
    }
    Some(cur)
}

/// Converts an oracle JSON value to the canonical form of a class-A `kind`.
fn canonical(kind: Kind, v: &Value) -> String {
    // A decimal number as the integer it scales to.
    let scaled = |n: &serde_json::Number, scale: f64| {
        n.as_f64()
            .map(|f| ((f * scale).round() as i64).to_string())
            .unwrap_or_else(|| n.to_string())
    };
    match (kind, v) {
        (Amount, Value::Number(n)) => scaled(n, 1e8),
        (Percent, Value::Number(n)) => scaled(n, 100.0),
        (Hex, Value::String(s)) => s.to_lowercase(),
        (_, Value::String(s)) => s.clone(),
        (_, other) => other.to_string(),
    }
}

/// An oracle value as text, for reports.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether the oracle's rendering `v` equals the harness rendering `r`. Decimal amounts are
/// numbers in JSON, so they are compared as numbers.
fn rendering_matches(v: &Value, r: &str) -> bool {
    match v {
        Value::String(s) => s == r,
        Value::Number(n) => r.parse::<f64>().ok() == n.as_f64(),
        _ => false,
    }
}

/// `service`, `platformP2PPort` and `platformHTTPPort` exist only with
/// `-deprecatedrpc=service`, which also prints `service` for every ProRegTx/ProUpServTx; when
/// the oracle's object has neither the field nor `service`, the oracle runs without it, and a
/// library value for the field has nothing to be compared with.
fn deprecated_rpc_off(oracle: &Value, path: &str) -> bool {
    let Some((obj, leaf)) = path.rsplit_once('.') else {
        return false;
    };
    ["service", "platformP2PPort", "platformHTTPPort"].contains(&leaf)
        && lookup(oracle, obj).is_some_and(|o| o.get("service").is_none() && o.get(leaf).is_none())
}

/// Scalar leaves of a JSON value as `(path, value)` pairs; empty objects and arrays are leaves
/// too, so that nothing in the oracle's JSON can go unclassified.
fn leaves<'a>(v: &'a Value, prefix: String, out: &mut Vec<(String, &'a Value)>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, v) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaves(v, p, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, v) in a.iter().enumerate() {
                leaves(v, format!("{prefix}[{i}]"), out);
            }
        }
        _ => out.push((prefix, v)),
    }
}

/// How one library's decode of a transaction's payload went, as already reported by check.rs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PayloadDecode {
    /// Decoded (or there is no payload); missing fields are `verbose.missing`.
    #[default]
    Ok,
    /// Reported as `payload.decode`, `payload.unsupported` or `payload.missing`.
    Failed,
    /// Reported as a `payload.variant` mismatch; the library decoded this variant (named as in
    /// `payload::dc_variant`/`bs_variant`, e.g. `ProRegTx`).
    Variant(&'static str),
}

impl PayloadDecode {
    /// Whether a failure already reported for this library covers the oracle's payload object
    /// `obj`: a decode failure always does; a variant mismatch does unless the library's
    /// variant is the one dashd printed (then the other library is the wrong one).
    fn reported_for(self, obj: &str) -> bool {
        match self {
            Self::Ok => false,
            Self::Failed => true,
            Self::Variant(v) => {
                let mut c = v.chars();
                let key: String =
                    c.next().map(|f| f.to_ascii_lowercase()).into_iter().chain(c).collect();
                key != obj
            }
        }
    }
}

/// Each library's [`PayloadDecode`] for one transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PayloadStatus {
    pub dashcore: PayloadDecode,
    pub base_sdk: PayloadDecode,
}

/// One library's view of a transaction: its flattening (`None` when the transaction itself
/// failed to decode) and how its payload decode went.
struct LibView {
    side: Side,
    flat: Option<BTreeMap<String, Rendered>>,
    payload: PayloadDecode,
}

fn harness_gap(
    sink: &mut Sink<'_>,
    txref: Option<TxRef<'_>>,
    path: &str,
    expected: Option<String>,
    detail: String,
) {
    sink.push("harness-gap", Side::Harness, txref, Some(path.to_string()), expected, None, detail);
}

fn compare_tx(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    oracle: &Value,
    raw: &[u8],
    params: &Result<Params, String>,
    libs: &[LibView; 2],
) {
    // Library values against the oracle (class A).
    let paths: BTreeSet<&str> = libs
        .iter()
        .filter_map(|l| l.flat.as_ref())
        .flat_map(|m| m.keys().map(String::as_str))
        .collect();
    for path in paths {
        if deprecated_rpc_off(oracle, path) {
            continue;
        }
        let kind = match class_of(path) {
            Some(Lib(k)) => k,
            _ => Text,
        };
        let expected =
            lookup(oracle, path).map_or_else(|| "<absent>".to_string(), |v| canonical(kind, v));
        let value = |l: &LibView| l.flat.as_ref().and_then(|m| m.get(path)).cloned();
        let (dc, bs) = (value(&libs[0]), value(&libs[1]));
        let gaps: BTreeSet<&String> =
            [&dc, &bs].into_iter().filter_map(|v| v.as_ref()?.as_ref().err()).collect();
        for g in gaps {
            harness_gap(
                sink,
                Some(txref),
                path,
                Some(expected.clone()),
                format!("cannot render the library value: {g}"),
            );
        }
        let (dc, bs) = (dc.and_then(Result::ok), bs.and_then(Result::ok));
        sink.judge(
            "verbose.field",
            Some(txref),
            Some(path),
            dc.as_ref(),
            bs.as_ref(),
            Some(&expected),
            String::clone,
        );
    }

    // Every oracle leaf, by class.
    let rendered = render_oracle(oracle, raw, params);
    let mut oracle_leaves = Vec::new();
    leaves(oracle, String::new(), &mut oracle_leaves);
    let mut seen_renderings = BTreeSet::new();
    for (path, v) in &oracle_leaves {
        let class = classify_leaf(path, v);
        match class {
            None => harness_gap(
                sink,
                Some(txref),
                path,
                Some(show(v)),
                "oracle field is unclassified: the harness neither compares nor exempts it".into(),
            ),
            Some(Lib(kind)) => {
                let obj = path.split(['.', '[']).next().unwrap_or(path);
                let payload = PAYLOAD_OBJECTS.contains(&obj);
                for lib in libs {
                    let Some(m) = &lib.flat else {
                        continue;
                    };
                    if !m.contains_key(path.as_str()) && !(payload && lib.payload.reported_for(obj))
                    {
                        let e = Some(canonical(kind, v));
                        let detail = if NOT_FLATTENED.contains(&normalize(path).as_str()) {
                            "no flattener renders this field: a library that now models it \
                             needs the harness flattener extended"
                        } else {
                            ""
                        };
                        sink.push(
                            "verbose.missing",
                            lib.side,
                            Some(txref),
                            Some(path.clone()),
                            e,
                            None,
                            detail,
                        );
                    }
                }
            }
            Some(Render) => {
                seen_renderings.insert(path.as_str());
                match rendered.get(path.as_str()) {
                    Some(Ok(r)) if rendering_matches(v, r) => {}
                    Some(Ok(r)) => render_mismatch(
                        sink,
                        txref,
                        path,
                        show(v),
                        r.clone(),
                        "harness rendering of the oracle's own source fields differs",
                    ),
                    Some(Err(e)) => harness_gap(
                        sink,
                        Some(txref),
                        path,
                        Some(show(v)),
                        format!("cannot render: {e}"),
                    ),
                    None => render_mismatch(
                        sink,
                        txref,
                        path,
                        show(v),
                        "<absent>".into(),
                        "the harness renders no such field from the oracle's source fields",
                    ),
                }
            }
            Some(Raw(_) | Chain(_)) => {}
        }
    }
    for (path, r) in &rendered {
        if let Ok(r) = r
            && !seen_renderings.contains(path.as_str())
        {
            render_mismatch(
                sink,
                txref,
                path,
                "<absent>".into(),
                r.clone(),
                "the harness renders a field the oracle does not print",
            );
        }
    }
}

/// A `verbose.render` finding: the oracle's rendering `expected` differs from the harness's
/// rendering `actual` of the oracle's own source fields.
fn render_mismatch(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    path: &str,
    expected: String,
    actual: String,
    detail: &str,
) {
    sink.push(
        "verbose.render",
        Side::Harness,
        Some(txref),
        Some(path.to_string()),
        Some(expected),
        Some(actual),
        detail,
    );
}

/// The block-level keys of `getblock <hash> 2` (`blockheaderToJSON`/`blockToJSON` in
/// src/rpc/blockchain.cpp) other than `tx`, by class. `Raw` keys are checked against the block
/// bytes by [`check_block_fields`].
const BLOCK_CLASSES: &[(&str, Class)] = &[
    ("hash", Raw("the block hash the other checks established")),
    ("height", Chain("chain position")),
    ("confirmations", Chain("chain position")),
    ("version", Raw("header bytes")),
    ("versionHex", Raw("header bytes")),
    ("merkleroot", Raw("header bytes")),
    ("time", Raw("header bytes")),
    ("mediantime", Chain("median time of the previous 11 blocks")),
    ("nonce", Raw("header bytes")),
    ("bits", Raw("header bytes")),
    ("difficulty", Chain("a floating-point rendering of nBits; bits is compared")),
    ("chainwork", Chain("cumulative chain work")),
    ("nTx", Raw("the transaction count of the block bytes")),
    ("previousblockhash", Raw("header bytes")),
    ("nextblockhash", Chain("chain position")),
    ("chainlock", Chain("ChainLock state")),
    ("size", Raw("the length of the block bytes")),
];

/// Checks the block-level fields of `getblock <hash> 2` against the block bytes (the libraries
/// are compared with `getblock <hash> 1` by the other checks): every key must be known, the
/// header-derived ones must match, and the block-level `cbTx` must equal `tx[0].cbTx`.
fn check_block_fields(sink: &mut Sink<'_>, verbose: &Value, block: &[u8], n_txs: usize) {
    let Some(obj) = verbose.as_object() else {
        return;
    };
    let h = &block[..crate::split::HEADER_LEN];
    let le32 = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().expect("4 bytes"));
    let prev = &h[4..36];
    let mut expected: BTreeMap<&str, String> = BTreeMap::from([
        ("version", (le32(0) as i32).to_string()),
        ("versionHex", format!("{:08x}", le32(0))),
        ("merkleroot", rev(&h[36..68])),
        ("time", le32(68).to_string()),
        ("bits", format!("{:08x}", le32(72))),
        ("nonce", le32(76).to_string()),
        ("nTx", n_txs.to_string()),
        ("size", block.len().to_string()),
    ]);
    if prev.iter().any(|b| *b != 0) {
        expected.insert("previousblockhash", rev(prev));
    }
    if let Some(hash) = sink.block_hash() {
        expected.insert("hash", hash.to_string());
    }
    for (key, v) in obj {
        if key == "tx" {
            continue;
        }
        if key == "cbTx" {
            if verbose.pointer("/tx/0/cbTx") != Some(v) {
                let detail = "block-level cbTx differs from tx[0].cbTx";
                let e = Some(show(v));
                let a = verbose.pointer("/tx/0/cbTx").map(show);
                sink.push("verbose.block", Side::Harness, None, Some("cbTx".into()), e, a, detail);
            }
            continue;
        }
        match BLOCK_CLASSES.iter().find(|(k, _)| k == key).map(|(_, c)| c) {
            None => harness_gap(
                sink,
                None,
                key,
                Some(show(v)),
                "block-level oracle field is unclassified: the harness neither compares nor \
                 exempts it"
                    .into(),
            ),
            Some(Raw(_)) => match expected.remove(key.as_str()) {
                Some(e) if show(v) == e => {}
                e => {
                    let detail = "oracle block field differs from the block bytes";
                    let (o, e) = (Some(show(v)), e.or_else(|| Some("<absent>".into())));
                    sink.push(
                        "verbose.block",
                        Side::Harness,
                        None,
                        Some(key.clone()),
                        o,
                        e,
                        detail,
                    );
                }
            },
            Some(_) => {}
        }
    }
    // Fields dashd prints that the oracle lacks.
    for (key, e) in expected {
        let detail = "oracle lacks a block field";
        let (o, e) = (Some("<absent>".to_string()), Some(e));
        sink.push("verbose.block", Side::Harness, None, Some(key.to_string()), o, e, detail);
    }
    if obj.get("cbTx").is_none() && verbose.pointer("/tx/0/cbTx").is_some() {
        let detail = "block-level cbTx absent but tx[0] has one";
        sink.push("verbose.block", Side::Harness, None, Some("cbTx".into()), None, None, detail);
    }
}

/// Compares one block's transactions with `getblock <hash> 2`. `payload[i]` says how each
/// library's decode of transaction `i`'s payload went.
pub fn check(
    sink: &mut Sink<'_>,
    verbose: &Value,
    block: &[u8],
    spans: &[TxSpan],
    dc_txs: &[Option<&DcTx>],
    bs_txs: &[Option<&BsTx>],
    payload: &[PayloadStatus],
) {
    let Some(txs) = verbose.get("tx").and_then(Value::as_array) else {
        sink.fail("verbose.shape", Side::Harness, None, "no tx array");
        return;
    };
    check_block_fields(sink, verbose, block, spans.len());
    if txs.len() != spans.len() {
        let (e, a) = (Some(txs.len().to_string()), Some(spans.len().to_string()));
        sink.push("verbose.tx_count", Side::Harness, None, None, e, a, "");
        return;
    }
    let params = Params::from_net(sink.net());
    for (i, (otx, span)) in txs.iter().zip(spans).enumerate() {
        let raw = &block[span.range.clone()];
        let txid = otx.get("txid").and_then(Value::as_str).and_then(parse_rev_hex);
        let txref = TxRef {
            index: i,
            txid: txid.as_ref(),
            raw: Some(raw),
        };
        // Harness/data sanity: dashd's own view of the transaction bytes and size.
        match otx.get("hex") {
            Some(Value::String(h)) if *h == hex(raw) => {}
            None => {}
            Some(_) => sink.fail("verbose.hex", Side::Harness, Some(txref), "oracle hex != raw"),
        }
        if otx.get("size").and_then(Value::as_u64) != Some(raw.len() as u64) {
            let e = otx.get("size").map(Value::to_string);
            let a = Some(raw.len().to_string());
            sink.push("verbose.size", Side::Harness, Some(txref), None, e, a, "");
        }
        check_unlock_fee(sink, txref, otx);
        let status = payload.get(i).copied().unwrap_or_default();
        let to_map = |f: Flat| f.into_iter().collect::<BTreeMap<_, _>>();
        let libs = [
            LibView {
                side: Side::Dashcore,
                flat: dc_txs.get(i).copied().flatten().map(|t| to_map(dc_flat(t, &params))),
                payload: status.dashcore,
            },
            LibView {
                side: Side::BaseSdk,
                flat: bs_txs.get(i).copied().flatten().map(|t| to_map(bs_flat(t, &params))),
                payload: status.base_sdk,
            },
        ];
        compare_tx(sink, txref, otx, raw, &params, &libs);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn lookup_and_canonical() {
        let v = json!({"vin": [{"scriptSig": {"hex": "AB"}}], "cbTx": {"creditPoolBalance": 1.5},
                       "proRegTx": {"operatorReward": 12.34, "ownerAddress": "yAbC"}});
        let get = |p: &str| lookup(&v, p).unwrap();
        assert_eq!(canonical(Hex, get("vin[0].scriptSig.hex")), "ab");
        assert_eq!(canonical(Amount, get("cbTx.creditPoolBalance")), "150000000");
        assert_eq!(canonical(Percent, get("proRegTx.operatorReward")), "1234");
        // Addresses are case-sensitive.
        assert_eq!(canonical(Text, get("proRegTx.ownerAddress")), "yAbC");
        assert!(lookup(&v, "vin[1].txid").is_none());
    }

    #[test]
    fn normalize_strips_indices() {
        assert_eq!(normalize("vout[12].scriptPubKey.hex"), "vout[].scriptPubKey.hex");
        assert_eq!(normalize("proRegTx.addresses.core_p2p[0]"), "proRegTx.addresses.core_p2p[]");
        assert_eq!(class_of("vout[3].n"), Some(Lib(Int)));
        assert_eq!(class_of("newPayload.height"), None);
    }

    #[test]
    fn classes_are_disjoint() {
        let mut seen = BTreeSet::new();
        for (p, _) in CLASSES {
            assert!(seen.insert(*p), "{p} is classified twice");
            assert_eq!(normalize(p), *p, "{p} is not normalized");
            // No classified path is also a container of another (no subtree classes).
            assert!(!is_known_container(p), "{p} is both a leaf and a container");
        }
    }

    #[test]
    fn renderings_are_listed_with_class_a_sources() {
        let listed: BTreeSet<&str> = RENDERINGS.iter().map(|(p, _)| *p).collect();
        let render: BTreeSet<&str> =
            CLASSES.iter().filter(|(_, c)| *c == Render).map(|(p, _)| *p).collect();
        assert_eq!(listed, render);
        for (p, sources) in RENDERINGS {
            for s in *sources {
                assert!(
                    matches!(class_of(s), Some(Lib(_) | Raw(_))),
                    "{p}: source {s} is not compared by the harness"
                );
            }
        }
    }

    /// A transaction with one input and no outputs carrying `payload` as special type `ty`.
    fn special_tx(ty: u16, payload: &[u8]) -> Vec<u8> {
        let mut raw = 3u16.to_le_bytes().to_vec();
        raw.extend_from_slice(&ty.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&[0x01; 32]); // prevout hash
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]); // index, scriptSig, nSequence
        raw.extend_from_slice(&[0, 0, 0, 0, 0]); // no outputs, locktime 0
        assert!(payload.len() < 0xfd);
        raw.push(payload.len() as u8);
        raw.extend_from_slice(payload);
        raw
    }

    fn flat_paths(raw: &[u8], params: &Result<Params, String>) -> Vec<(String, Rendered)> {
        let mut out = Vec::new();
        if let Ok(t) = dashcore::consensus::deserialize::<DcTx>(raw) {
            out.extend(dc_flat(&t, params));
        }
        let mut cur = raw;
        if let Ok(t) = <BsTx as dash_types::codec::BaseCodec>::decode(&mut cur) {
            out.extend(bs_flat(&t, params));
        }
        out
    }

    #[test]
    fn flatteners_emit_only_class_a_paths() {
        let params = Ok(Params::Test);
        let mut emitted = Vec::new();
        // Every transaction of every committed fixture block.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
        for entry in std::fs::read_dir(dir).unwrap() {
            let v: Value =
                serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
            for tx in v["getblock2"]["tx"].as_array().unwrap() {
                let raw = hex_conservative::decode_to_vec(tx["hex"].as_str().unwrap()).unwrap();
                emitted.extend(flat_paths(&raw, &params));
            }
        }
        // Payload types the fixtures lack (dashcore's own test vectors, and zero-filled bytes
        // that only need to decode).
        let mnhf = [&[1u8, 0][..], &[0; 32], &[0; 96]].concat();
        let rev_payload = [&[1u8, 0][..], &[0; 32], &[0, 0], &[0; 32], &[0; 96]].concat();
        let p2pkh = [&[25u8, 0x76, 0xa9, 20][..], &[0x33; 20], &[0x88, 0xac]].concat();
        let up_reg =
            [&[1u8, 0][..], &[0; 32], &[0, 0], &[0; 48], &[0x11; 20], &p2pkh, &[0; 32], &[0]]
                .concat();
        // A v2 (BasicBLS) Evo ProUpServTx: platform node ID and ports follow inputsHash.
        let mut up_serv = [&[2u8, 0, 1, 0][..], &[0x22; 32]].concat();
        up_serv
            .extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4, 0x4e, 0x1f]);
        up_serv.extend_from_slice(&p2pkh);
        up_serv.extend_from_slice(&[0; 32]);
        up_serv.extend_from_slice(&[0x44; 20]);
        up_serv.extend_from_slice(&[0x67, 0x69, 0x01, 0xbb]);
        up_serv.extend_from_slice(&[0; 96]);
        for (ty, payload, obj, per_lib) in [
            (7u16, mnhf.as_slice(), "mnhfTx", 4),
            (4, &rev_payload, "proUpRevTx", 4),
            (3, &up_reg, "proUpRegTx", 6),
            (2, &up_serv, "proUpServTx", 12),
        ] {
            let paths = flat_paths(&special_tx(ty, payload), &params);
            let n = paths.iter().filter(|(p, _)| p.starts_with(obj)).count();
            assert_eq!(n, 2 * per_lib, "type {ty}: {paths:?}");
            emitted.extend(paths);
        }
        for (path, v) in &emitted {
            assert!(matches!(class_of(path), Some(Lib(_))), "{path} is not class A");
            assert!(v.is_ok(), "{path}: {v:?}");
        }
        // Every class-A path is emitted by a flattener, or listed as not flattened.
        let covered: BTreeSet<String> = emitted.iter().map(|(p, _)| normalize(p)).collect();
        let lib_paths: BTreeSet<String> = CLASSES
            .iter()
            .filter(|(_, c)| matches!(c, Lib(_)))
            .map(|(p, _)| p.to_string())
            .collect();
        let not_flattened: BTreeSet<String> = NOT_FLATTENED.iter().map(|p| p.to_string()).collect();
        assert!(covered.is_disjoint(&not_flattened), "{:?}", covered.intersection(&not_flattened));
        let all: BTreeSet<String> = covered.union(&not_flattened).cloned().collect();
        assert_eq!(all, lib_paths, "missing: {:?}", lib_paths.difference(&all));
    }

    #[test]
    fn variant_mismatch_suppresses_only_the_wrong_library() {
        assert!(!PayloadDecode::Ok.reported_for("proRegTx"));
        assert!(PayloadDecode::Failed.reported_for("proRegTx"));
        assert!(!PayloadDecode::Variant("ProRegTx").reported_for("proRegTx"));
        assert!(PayloadDecode::Variant("ProUpServTx").reported_for("proRegTx"));
        assert!(!PayloadDecode::Variant("CbTx").reported_for("cbTx"));
    }

    #[test]
    fn oracle_renderings() {
        let otx = json!({
            "vin": [{"scriptSig": {"hex": "0101"}}],
            "vout": [{"valueSat": 150_000_000, "scriptPubKey": {"hex": "76a914a2d5dbbd4dc8d0c22e48e4a0a1b3fd47c6c4ab4f88ac"}}],
            "assetLockTx": {"version": 2, "creditOutputs": [{"valueSat": 1, "scriptPubKey": {"hex": "6a"}}]},
            "proRegTx": {"payouts": [{"script": "6a"}]},
        });
        let r = render_oracle(&otx, &[], &Ok(Params::Test));
        let get = |p: &str| r.get(p).cloned().unwrap().unwrap();
        assert_eq!(get("vin[0].scriptSig.asm"), "1");
        assert_eq!(get("vout[0].value"), "1.50000000");
        assert_eq!(get("vout[0].scriptPubKey.type"), "pubkeyhash");
        assert!(get("vout[0].scriptPubKey.address").starts_with('y'));
        // A non-P2PKH/P2SH credit output has no platform address; a payout gets "".
        assert!(!r.contains_key("assetLockTx.creditOutputs[0].address"));
        assert_eq!(get("assetLockTx.creditOutputs[0].scriptPubKey.asm"), "OP_RETURN");
        assert_eq!(get("proRegTx.payouts[0].address"), "");
        // A missing source is a gap, not a silent skip.
        let r = render_oracle(&json!({"vout": [{"scriptPubKey": {}}]}), &[], &Ok(Params::Test));
        assert!(r["vout[0].scriptPubKey.asm"].is_err());
        assert!(r["vout[0].value"].is_err());
    }

    /// dashcore's Core-produced v24 vectors (`dash/tests/data`, made by dashd v24.0.0-rc.3
    /// on regtest; see `dash/contrib/core-vectors/README.md`): each transaction's raw bytes
    /// and Core's `TxToUniv` JSON for them.
    fn core_v24_vectors(file: &str, list: &str) -> Vec<(String, Vec<u8>, Value)> {
        let path = format!("{}/../dash/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
        let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        v[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let name = t.get("source").or(t.get("name")).unwrap().as_str().unwrap();
                let raw = hex_conservative::decode_to_vec(t["hex"].as_str().unwrap()).unwrap();
                (name.to_string(), raw, t["core"].clone())
            })
            .collect()
    }

    /// dashcore's flattening of `raw` against Core's JSON; see [`assert_flat_matches`].
    fn assert_dc_flat_matches(name: &str, raw: &[u8], core: &Value) -> BTreeSet<String> {
        let tx: DcTx = dashcore::consensus::deserialize(raw).unwrap();
        assert_flat_matches("dashcore", name, dc_flat(&tx, &Ok(Params::Test)), core)
    }

    /// base-sdk's flattening of `raw` against Core's JSON; see [`assert_flat_matches`].
    fn assert_bs_flat_matches(name: &str, raw: &[u8], core: &Value) -> BTreeSet<String> {
        use dash_types::codec::BaseCodec;
        let tx = BsTx::decode(&mut &raw[..]).unwrap();
        assert_flat_matches("base-sdk", name, bs_flat(&tx, &Ok(Params::Test)), core)
    }

    /// A library's flattening against Core's JSON, both ways: every value it emits equals
    /// Core's, and every class-A leaf Core prints is emitted. `vin` and `vout` are left out:
    /// the vector files keep them in a reduced form (`slim.py`). Returns the paths.
    fn assert_flat_matches(lib: &str, name: &str, flat: Flat, core: &Value) -> BTreeSet<String> {
        let io = |p: &str| p.starts_with("vin[") || p.starts_with("vout[");
        let flat: BTreeMap<String, Rendered> = flat.into_iter().filter(|(p, _)| !io(p)).collect();
        for (path, v) in &flat {
            if deprecated_rpc_off(core, path) {
                continue;
            }
            // Known harness gaps (reported as `harness-gap` in a real run): TORv3 addresses
            // and netinfo purposes Core does not know, which it prints under the key "".
            if let Err(e) = v {
                let known = e.contains("TORv3") || path.contains(".<purpose ");
                assert!(known, "{lib} {name}: {path}: {e}");
                continue;
            }
            let Some(Lib(kind)) = class_of(path) else {
                panic!("{lib} {name}: {path} is not class A");
            };
            let expected = lookup(core, path).map(|o| canonical(kind, o));
            assert_eq!(v.as_ref().ok(), expected.as_ref(), "{lib} {name}: {path}");
        }
        let mut core_leaves = Vec::new();
        leaves(core, String::new(), &mut core_leaves);
        for (path, v) in core_leaves {
            if io(&path) {
                continue;
            }
            // Core prints the entries of a netinfo purpose it does not know under the key ""
            // (a `harness-gap` in a real run; Core rejects such a payload, so no chain has one).
            if path.contains(".addresses.[") {
                continue;
            }
            match classify_leaf(&path, v) {
                Some(Lib(_)) => {
                    assert!(flat.contains_key(&path), "{lib} {name}: does not emit {path}")
                }
                Some(_) => {}
                // The vector files also keep `getrawtransaction`'s chain fields (`blocktime`,
                // `confirmations`, ...), which `getblock 2` does not print.
                None if !PAYLOAD_OBJECTS.iter().any(|o| path.starts_with(&format!("{o}."))) => {}
                None => panic!("{name}: Core prints {path}, which the harness does not classify"),
            }
        }
        flat.into_keys().collect()
    }

    /// Every transaction of the v24 vectors, including the shared-masternode types 10–12,
    /// flattens to exactly what Core prints, through either library.
    #[test]
    fn dashcore_flattening_matches_core_v24_vectors() {
        let mut objects = BTreeMap::<&str, usize>::new();
        for (name, raw, core) in core_v24_vectors("core_v24_regtest.json", "transactions") {
            let paths = assert_dc_flat_matches(&name, &raw, &core);
            assert_eq!(assert_bs_flat_matches(&name, &raw, &core), paths, "{name}");
            for obj in PAYLOAD_OBJECTS {
                if paths.iter().any(|p| p.starts_with(&format!("{obj}."))) {
                    *objects.entry(obj).or_default() += 1;
                }
            }
        }
        for (obj, n) in [("proDisTx", 5), ("proUpShareTx", 2), ("proUpSharedRegTx", 3)] {
            assert_eq!(objects.get(obj), Some(&n), "{objects:?}");
        }
    }

    /// Core prints no payload object for a payload `GetTxPayload` cannot read (nor for a type
    /// it does not know): only `extraPayloadSize` and `extraPayload`. dashcore keeps such a
    /// payload as `RawPayloadType` and base-sdk fails or does not finish its decode; both
    /// flatten the same way. The extended-address vectors include all three cases Core
    /// produced (accepted, rejected, unreadable), among them the payloads whose purposes are
    /// repeated, out of order or unknown, which rc.4 cannot read (CE4-01).
    #[test]
    fn unreadable_payloads_flatten_like_core() {
        let mut unreadable = 0;
        for (name, raw, core) in core_v24_vectors("core_v24_netinfo.json", "cases") {
            let paths = assert_dc_flat_matches(&name, &raw, &core);
            assert_eq!(assert_bs_flat_matches(&name, &raw, &core), paths, "{name}");
            let tx: DcTx = dashcore::consensus::deserialize(&raw).unwrap();
            if let Some(Dc::RawPayloadType(ty, _)) = &tx.special_transaction_payload {
                unreadable += 1;
                assert!(core["type"].as_u64() == Some(u64::from(*ty)), "{name}");
                assert!(PAYLOAD_OBJECTS.iter().all(|o| core.get(o).is_none()), "{name}");
                let raw_fields = ["extraPayload", "extraPayloadSize"];
                assert!(raw_fields.iter().all(|p| paths.contains(*p)), "{name}: {paths:?}");
                let payload_path = |p: &String| PAYLOAD_OBJECTS.iter().any(|o| p.starts_with(o));
                assert!(!paths.iter().any(payload_path), "{name}: {paths:?}");
            }
        }
        assert_eq!(unreadable, 8);
    }
}

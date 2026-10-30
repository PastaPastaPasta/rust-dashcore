//! The special payloads' `ToJson` (`evo/core_write.cpp`, `llmq/core_write.cpp`) and what they
//! stand on: `PayoutListToJson`, `ShareListToJson` and the network info's `ToJson` with
//! `GetNetInfoWithLegacyFields`.
//!
//! A BLS field Core prints under its process-wide scheme flag (a CbTx's `bestCLSignature`, an
//! asset unlock's `quorumSig`, an MnHF signal's `sig`) prints its stored bytes (D41).

use dash_netaddr::Service;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{
    AssetLock, AssetUnlock, CoinbaseCommitment, CollateralShare, DynBitset, FinalCommitment,
    MnHardFork, NIEntry, NIPurpose, NetInfo, PayoutShare, ProDisTx, ProRegTx, ProUpRegTx,
    ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx, SpecialPayload,
};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;

use super::{TxJsonParams, script_to_univ};
use crate::UniValue;
use crate::descriptor::hex;
use crate::key_io::{
    Destination, PlatformDestination, encode_destination, encode_platform_destination,
    extract_destination,
};
use crate::peers::amount;

/// `ProTxVersion::ExtAddr`.
// Core: evo/types.h:19
const PROTX_EXT_ADDR: u16 = 3;
/// `MnType::Evo`.
// Core: evo/dmn_types.h
const MN_TYPE_EVO: u16 = 1;

/// The payload object `TxToUniv` prints under its key, or `None` where it prints none: Core
/// prints only a payload `GetTxPayload` reads whole, of a type it knows, and of a ProTx version
/// it knows (D41).
// Core: core_write.cpp:291-341
pub(crate) fn payload_to_json(
    payload: &SpecialPayload,
    p: &TxJsonParams,
) -> Option<(&'static str, UniValue)> {
    Some(match payload {
        SpecialPayload::ProviderRegister(b) => ("proRegTx", pro_reg_tx(b, p)),
        SpecialPayload::ProviderUpdateService(b) => ("proUpServTx", pro_up_serv_tx(b, p)),
        SpecialPayload::ProviderUpdateRegistrar(b) => ("proUpRegTx", pro_up_reg_tx(b, p)),
        SpecialPayload::ProviderUpdateRevoke(b) => ("proUpRevTx", pro_up_rev_tx(b)),
        SpecialPayload::CoinbaseCommitment(b) => ("cbTx", cbtx_to_json(b)),
        SpecialPayload::QuorumCommitment(b) => ("qcTx", qc_tx(b)),
        SpecialPayload::MnhfSignal(b) => ("mnhfTx", mnhf_tx(b)),
        SpecialPayload::AssetLock(b) => ("assetLockTx", asset_lock_tx(b, p)),
        SpecialPayload::AssetUnlock(b) => ("assetUnlockTx", asset_unlock_tx(b)),
        SpecialPayload::ProviderDissolve(b) => ("proDisTx", pro_dis_tx(b)),
        SpecialPayload::ProviderUpdateShare(b) => ("proUpShareTx", pro_up_share_tx(b, p)),
        SpecialPayload::ProviderUpdateSharedRegistrar(b) => {
            ("proUpSharedRegTx", pro_up_shared_reg_tx(b, p))
        }
        SpecialPayload::UnknownProTxVersion {
            ..
        }
        | SpecialPayload::Unknown {
            ..
        } => return None,
    })
}

/// A field's wire bytes.
fn wire<T: BaseCodec>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(&mut out);
    out
}

/// `uint256::ToString()` / `uint160::ToString()`: the wire bytes reversed, in hex.
fn uint_hex<T: BaseCodec>(v: &T) -> UniValue {
    let mut bytes = wire(v);
    bytes.reverse();
    hex(&bytes).into()
}

/// `EncodeDestination(PKHash(key_id))`.
fn key_address<T: BaseCodec>(key_id: &T, p: &TxJsonParams) -> UniValue {
    let hash: [u8; 20] = wire(key_id).try_into().expect("a key id is 20 bytes");
    encode_destination(&Destination::PkHash(hash), &p.key_io).into()
}

/// `ExtractDestination(script)`, encoded.
fn script_address(script: &[u8], p: &TxJsonParams) -> Option<UniValue> {
    extract_destination(script).map(|d| encode_destination(&d, &p.key_io).into())
}

/// `PayoutListToJson`. Every payout script is P2PKH or P2SH (`IsPayoutListTriviallyValid`), so
/// Core prints the address it extracts; for any other script it prints `CNoDestination`'s "".
// Core: evo/providertx_util.cpp:36-50
fn payout_list(payouts: &[PayoutShare], p: &TxJsonParams) -> UniValue {
    let mut out = UniValue::new_array();
    for payout in payouts {
        let script = payout.script_payout.as_bytes();
        let mut o = UniValue::new_object();
        o.push_kv_end("address", script_address(script, p).unwrap_or_else(|| "".into()));
        o.push_kv_end("script", hex(script).into());
        o.push_kv_end("reward", i64::from(payout.reward).into());
        out.push_back(o);
    }
    out
}

/// `ShareListToJson`: the reward script is `RewardScript()`, the refund script when none is set.
// Core: evo/providertx_util.cpp:66-85
fn share_list(shares: &[CollateralShare], p: &TxJsonParams) -> UniValue {
    let mut out = UniValue::new_array();
    for share in shares {
        let refund = share.script_refund.as_bytes();
        let reward = match share.script_reward.as_bytes() {
            [] => refund,
            reward => reward,
        };
        let mut o = UniValue::new_object();
        o.push_kv_end("amount", share.amount.into());
        o.push_kv_end("refundAddress", script_address(refund, p).unwrap_or_else(|| "".into()));
        o.push_kv_end("refundScript", hex(refund).into());
        o.push_kv_end("rewardAddress", script_address(reward, p).unwrap_or_else(|| "".into()));
        o.push_kv_end("rewardScript", hex(reward).into());
        o.push_kv_end("ownerAddress", key_address(&share.key_id_owner, p));
        out.push_back(o);
    }
    out
}

/// A ProTx's network info as Core holds it after reading (`MnNetInfo` or `ExtNetInfo`).
/// base-sdk marks an extended entry Core reads as invalid [`NIEntry::Invalid`], and keeps the
/// purpose groups known, ascending and unique, as Core's reader requires.
struct NetInfoView<'a>(&'a NetInfo);

/// `NetInfoEntry::ToStringAddrPort()` of an invalid entry.
// Core: evo/netinfo.cpp:264-275
const INVALID_ENTRY: &str = "[invalid entry]";

/// `ExtNetInfo::CURRENT_VERSION`.
// Core: evo/netinfo.h:378
const EXT_NET_INFO_VERSION: u8 = 1;

impl NetInfoView<'_> {
    /// `MnNetInfo`'s entry: the legacy service when valid (`NetInfoEntry(CService)`).
    // Core: evo/netinfo.h:212-217, :343-349
    fn legacy_service(&self) -> Option<Service> {
        match self.0 {
            NetInfo::Legacy(v1) => {
                Some(Service::from_v1(&v1.0)).filter(|service| service.addr.is_valid())
            }
            NetInfo::Extended(_) => None,
        }
    }

    /// The purpose groups Core's map holds: none for an unknown `ExtNetInfo` version.
    ///
    /// The version guard here and in `is_empty` is defensive: base-sdk's decoder already leaves
    /// an unknown version without entries, as Core's `Unserialize` does, so only a hand-built
    /// `NetInfoV2` can tell the guard from its absence.
    // Core: evo/netinfo.h:419-424
    fn groups(&self) -> &[(NIPurpose, Vec<NIEntry>)] {
        match self.0 {
            NetInfo::Extended(v2) if v2.version == EXT_NET_INFO_VERSION => &v2.entries,
            _ => &[],
        }
    }

    /// `CanStorePlatform()`.
    fn can_store_platform(&self) -> bool {
        matches!(self.0, NetInfo::Extended(_))
    }

    /// `IsEmpty()`.
    // Core: evo/netinfo.h:355, :442
    fn is_empty(&self) -> bool {
        match self.0 {
            NetInfo::Legacy(_) => self.legacy_service().is_none(),
            NetInfo::Extended(v2) => v2.version == EXT_NET_INFO_VERSION && v2.entries.is_empty(),
        }
    }

    /// The first entry of `purpose`'s group.
    fn first(&self, purpose: NIPurpose) -> Option<&NIEntry> {
        self.groups().iter().find(|(p, _)| *p == purpose).and_then(|(_, g)| g.first())
    }

    /// `GetPrimary()`: the first `CORE_P2P` service, or `CService{}` (`[::]:0`).
    // Core: evo/netinfo.cpp:343-349, :566-579
    fn primary(&self) -> Service {
        let service = match self.0 {
            NetInfo::Legacy(_) => self.legacy_service(),
            NetInfo::Extended(_) => match self.first(NIPurpose::CoreP2p) {
                Some(NIEntry::Service(s)) => Some(service_v2(s)),
                _ => None,
            },
        };
        service.unwrap_or_default()
    }

    /// `ToJson()`: each purpose's entries by `PurposeToString`, as `ToStringAddrPort()`.
    // Core: evo/netinfo.cpp:359-374, :640-656
    fn to_json(&self) -> UniValue {
        let mut ret = UniValue::new_object();
        if let Some(service) = self.legacy_service() {
            ret.push_kv_end("core_p2p", array_of(service.to_string()));
        }
        for (purpose, entries) in self.groups() {
            let mut arr = UniValue::new_array();
            for entry in entries {
                arr.push_back(entry_addr_port(entry).into());
            }
            ret.push_kv_end(purpose_to_string(*purpose), arr);
        }
        ret
    }
}

fn service_v2(s: &dash_primitives::ServiceV2) -> Service {
    Service::new(dash_netaddr::NetAddr::from_addrv2(&s.addr), s.port)
}

fn array_of(s: String) -> UniValue {
    let mut arr = UniValue::new_array();
    arr.push_back(s.into());
    arr
}

/// `NetInfoEntry::ToStringAddrPort()`.
// Core: evo/netinfo.cpp:264-275
fn entry_addr_port(entry: &NIEntry) -> String {
    match entry {
        NIEntry::Service(s) => service_v2(s).to_string(),
        NIEntry::Domain {
            name,
            port,
        } => format!("{}:{port}", String::from_utf8_lossy(name)),
        NIEntry::Invalid => INVALID_ENTRY.into(),
    }
}

/// `NetInfoEntry::GetPort()`: 0 for an invalid entry.
fn entry_port(entry: &NIEntry) -> u16 {
    match entry {
        NIEntry::Service(s) => s.port,
        NIEntry::Domain {
            port,
            ..
        } => *port,
        NIEntry::Invalid => 0,
    }
}

/// `PurposeToString`: "" for a purpose it does not name, which base-sdk never decodes.
// Core: evo/netinfo.h:92-103
fn purpose_to_string(purpose: NIPurpose) -> &'static str {
    match purpose {
        NIPurpose::CoreP2p => "core_p2p",
        NIPurpose::PlatformP2p => "platform_p2p",
        NIPurpose::PlatformHttps => "platform_https",
        NIPurpose::Unknown(_) => "",
    }
}

/// The fields a ProRegTx and a ProUpServTx share around their network info.
struct ProTxNet<'a> {
    net_info: NetInfoView<'a>,
    evo: bool,
    /// `platformP2PPort`, kept only beside a legacy network info.
    p2p_port: Option<u16>,
    /// `platformHTTPPort`, likewise.
    http_port: Option<u16>,
}

impl ProTxNet<'_> {
    /// `GetDeprecatedServiceField`.
    // Core: evo/core_write.cpp:30-34
    fn service(&self) -> UniValue {
        self.net_info.primary().to_string().into()
    }

    /// `GetNetInfoWithLegacyFields`: an Evo node's legacy network info also lists its platform
    /// endpoints, the primary address at the two stored ports.
    // Core: evo/core_write.cpp:37-57
    fn addresses(&self) -> UniValue {
        let mut ret = self.net_info.to_json();
        if !self.evo || self.net_info.is_empty() || self.net_info.can_store_platform() {
            return ret;
        }
        let addr = self.net_info.primary().addr;
        let at =
            |port: Option<u16>| array_of(Service::new(addr.clone(), port.unwrap_or(0)).to_string());
        ret.push_kv_end("platform_https", at(self.http_port));
        ret.push_kv_end("platform_p2p", at(self.p2p_port));
        ret
    }

    /// `GetPlatformPort<is_p2p>`: the stored port beside a legacy network info, else the first
    /// entry's port of the platform purpose, or −1 for an empty one. A non-empty extended info
    /// without the purpose fails Core's `CHECK_NONFATAL`; it cannot pass `Validate` for an Evo
    /// node, and prints −1 here (D41).
    // Core: evo/core_write.cpp:60-93
    fn platform_port(&self, p2p: bool) -> UniValue {
        let (stored, purpose) = if p2p {
            (self.p2p_port, NIPurpose::PlatformP2p)
        } else {
            (self.http_port, NIPurpose::PlatformHttps)
        };
        if !self.net_info.can_store_platform() {
            return i64::from(stored.unwrap_or(0)).into();
        }
        if self.net_info.is_empty() {
            return (-1i64).into();
        }
        self.net_info.first(purpose).map_or(-1, |e| i64::from(entry_port(e))).into()
    }

    /// `platformNodeID` and, under `-deprecatedrpc=service`, the two platform ports: an Evo
    /// node's fields.
    fn push_evo_fields(
        &self,
        node_id: Option<&dash_primitives::PlatformNodeId>,
        ret: &mut UniValue,
        p: &TxJsonParams,
    ) {
        if !self.evo {
            return;
        }
        ret.push_kv_end("platformNodeID", node_id.map_or_else(|| hex(&[0; 20]).into(), uint_hex));
        if p.service {
            ret.push_kv_end("platformP2PPort", self.platform_port(true));
            ret.push_kv_end("platformHTTPPort", self.platform_port(false));
        }
    }
}

/// A lazy operator key's `ToString()`: `Get()` under the scheme it was read with, which is the
/// stored key when valid and zeros otherwise.
// Core: bls/bls.h:571-574
fn lazy_key(key: &dash_pkc::bls::BlsLazyPk) -> UniValue {
    hex(&key.get().to_bytes(key.scheme())).into()
}

/// `CProRegTx::ToJson`.
// Core: evo/core_write.cpp:198-233
fn pro_reg_tx(b: &ProRegTx, p: &TxJsonParams) -> UniValue {
    let net = ProTxNet {
        net_info: NetInfoView(&b.net_info),
        evo: b.mn_type.to_base() == MN_TYPE_EVO,
        p2p_port: b.platform_p2p_port,
        http_port: b.platform_http_port,
    };
    let shared = !b.shares.is_empty();
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("type", i64::from(b.mn_type.to_base()).into());
    ret.push_kv_end("collateralHash", uint_hex(&b.collateral_hash));
    ret.push_kv_end("collateralIndex", i64::from(b.collateral_index).into());
    if p.service {
        ret.push_kv_end("service", net.service());
    }
    ret.push_kv_end("addresses", net.addresses());
    if !shared {
        ret.push_kv_end("ownerAddress", key_address(&b.key_id_owner, p));
    }
    ret.push_kv_end("votingAddress", key_address(&b.key_id_voting, p));
    if shared {
        ret.push_kv_end("shares", share_list(&b.shares, p));
        ret.push_kv_end("earlyPeriodBlocks", i64::from(b.early_period_blocks).into());
        ret.push_kv_end("earlyPenalty", b.early_penalty.into());
    } else if b.version >= PROTX_EXT_ADDR {
        ret.push_kv_end("payouts", payout_list(&b.payouts, p));
    } else if let Some(address) = script_address(b.script_payout.as_bytes(), p) {
        ret.push_kv_end("payoutAddress", address);
    }
    ret.push_kv_end("pubKeyOperator", lazy_key(&b.pub_key_operator));
    let reward = f64::from(b.operator_reward) / 100.0;
    ret.push_kv_end("operatorReward", UniValue::from_f64(reward).expect("a finite reward"));
    net.push_evo_fields(b.platform_node_id.as_ref(), &mut ret, p);
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret
}

/// `CProUpServTx::ToJson`.
// Core: evo/core_write.cpp:296-318
fn pro_up_serv_tx(b: &ProUpServTx, p: &TxJsonParams) -> UniValue {
    let net = ProTxNet {
        net_info: NetInfoView(&b.net_info),
        evo: b.mn_type.to_base() == MN_TYPE_EVO,
        p2p_port: b.platform_p2p_port,
        http_port: b.platform_http_port,
    };
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("type", i64::from(b.mn_type.to_base()).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    if p.service {
        ret.push_kv_end("service", net.service());
    }
    ret.push_kv_end("addresses", net.addresses());
    if let Some(address) = script_address(b.script_operator_payout.as_bytes(), p) {
        ret.push_kv_end("operatorPayoutAddress", address);
    }
    net.push_evo_fields(b.platform_node_id.as_ref(), &mut ret, p);
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret
}

/// `CProUpRegTx::ToJson`.
// Core: evo/core_write.cpp:235-249
fn pro_up_reg_tx(b: &ProUpRegTx, p: &TxJsonParams) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    ret.push_kv_end("votingAddress", key_address(&b.key_id_voting, p));
    if b.version >= PROTX_EXT_ADDR {
        ret.push_kv_end("payouts", payout_list(&b.payouts, p));
    } else if let Some(address) = script_address(b.script_payout.as_bytes(), p) {
        ret.push_kv_end("payoutAddress", address);
    }
    ret.push_kv_end("pubKeyOperator", lazy_key(&b.pub_key_operator));
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret
}

/// `CProUpRevTx::ToJson`.
// Core: evo/core_write.cpp:251-259
fn pro_up_rev_tx(b: &ProUpRevTx) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    ret.push_kv_end("reason", i64::from(b.reason.to_base()).into());
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret
}

/// `CProDisTx::ToJson`.
// Core: evo/core_write.cpp:261-269
fn pro_dis_tx(b: &ProDisTx) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    ret.push_kv_end("actorIndex", i64::from(b.actor_index).into());
    ret.push_kv_end("sigCount", (b.sigs.len() as u64).into());
    ret
}

/// `CProUpShareTx::ToJson`.
// Core: evo/core_write.cpp:271-282
fn pro_up_share_tx(b: &ProUpShareTx, p: &TxJsonParams) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    ret.push_kv_end("shareIndex", i64::from(b.share_index).into());
    if let Some(address) = script_address(b.script_reward.as_bytes(), p) {
        ret.push_kv_end("rewardAddress", address);
    }
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret
}

/// `CProUpSharedRegTx::ToJson`.
// Core: evo/core_write.cpp:284-294
fn pro_up_shared_reg_tx(b: &ProUpSharedRegTx, p: &TxJsonParams) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("proTxHash", uint_hex(&b.pro_tx_hash));
    ret.push_kv_end("votingAddress", key_address(&b.key_id_voting, p));
    ret.push_kv_end("pubKeyOperator", lazy_key(&b.pub_key_operator));
    ret.push_kv_end("inputsHash", uint_hex(&b.inputs_hash));
    ret.push_kv_end("sigCount", (b.sigs.len() as u64).into());
    ret
}

/// `CCbTx::ToJson`: each version adds its fields, which base-sdk decodes exactly for it.
// Core: evo/core_write.cpp:134-152
pub(crate) fn cbtx_to_json(b: &CoinbaseCommitment) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("height", i64::from(b.height).into());
    ret.push_kv_end("merkleRootMNList", uint_hex(&b.merkle_root_mn_list));
    if let Some(root) = &b.merkle_root_quorums {
        ret.push_kv_end("merkleRootQuorums", uint_hex(root));
    }
    if let (Some(diff), Some(sig), Some(balance)) =
        (b.best_cl_height_diff, &b.best_cl_signature, b.credit_pool_balance)
    {
        ret.push_kv_end("bestCLHeightDiff", diff.into());
        ret.push_kv_end("bestCLSignature", hex(&sig.to_bytes()).into());
        ret.push_kv_end("creditPoolBalance", amount(balance));
    }
    if let Some(root) = &b.merkle_root_asset_unlocks {
        ret.push_kv_end("merkleRootAssetUnlocks", uint_hex(root));
    }
    ret
}

/// `CFinalCommitment::BitsVectorToHexStr`: the bits packed LSB first.
// Core: llmq/commitment.h:137-144
fn bits_hex(bits: &DynBitset) -> UniValue {
    let mut bytes = vec![0u8; bits.num_bits.div_ceil(8) as usize];
    for i in 0..bits.num_bits {
        if bits.get(i) == Some(true) {
            bytes[(i / 8) as usize] |= 1 << (i % 8);
        }
    }
    hex(&bytes).into()
}

/// The set bits among the first `num_bits` (`CountSigners`, `CountValidMembers`).
fn count_bits(bits: &DynBitset) -> UniValue {
    ((0..bits.num_bits).filter(|&i| bits.get(i) == Some(true)).count() as u64).into()
}

/// `CFinalCommitmentTxPayload::ToJson` and `CFinalCommitment::ToJson`: the BLS fields under the
/// legacy scheme for versions 1 and 2, as Core prints them.
// Core: llmq/core_write.cpp:15-40
fn qc_tx(b: &FinalCommitment) -> UniValue {
    let q = &b.commitment;
    let scheme = BlsSchemeKind::from_legacy(q.version == 1 || q.version == 2);
    let mut c = UniValue::new_object();
    c.push_kv_end("version", i64::from(q.version).into());
    c.push_kv_end("llmqType", i64::from(q.llmq_type.to_base()).into());
    c.push_kv_end("quorumHash", uint_hex(&q.quorum_hash));
    c.push_kv_end("quorumIndex", i64::from(q.quorum_index.unwrap_or(0)).into());
    c.push_kv_end("signersCount", count_bits(&q.signers));
    c.push_kv_end("signers", bits_hex(&q.signers));
    c.push_kv_end("validMembersCount", count_bits(&q.valid_members));
    c.push_kv_end("validMembers", bits_hex(&q.valid_members));
    c.push_kv_end("quorumPublicKey", hex(&q.quorum_public_key.to_bytes(scheme)).into());
    c.push_kv_end("quorumVvecHash", uint_hex(&q.quorum_vvec_hash));
    c.push_kv_end("quorumSig", hex(&q.quorum_sig.to_bytes(scheme)).into());
    c.push_kv_end("membersSig", hex(&q.members_sig.to_bytes(scheme)).into());
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("height", i64::from(b.height.to_u32()).into());
    ret.push_kv_end("commitment", c);
    ret
}

/// `MNHFTxPayload::ToJson` and `MNHFTx::ToJson`.
// Core: evo/core_write.cpp:421-436
fn mnhf_tx(b: &MnHardFork) -> UniValue {
    let mut signal = UniValue::new_object();
    signal.push_kv_end("versionBit", i64::from(b.version_bit).into());
    signal.push_kv_end("quorumHash", uint_hex(&b.quorum_hash));
    signal.push_kv_end("sig", hex(&b.sig.to_bytes(b.sig.scheme())).into());
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("signal", signal);
    ret
}

/// `CAssetLockPayload::ToJson`: version 2 adds each credit output's DIP-18 Platform address.
// Core: evo/core_write.cpp:97-120
fn asset_lock_tx(b: &AssetLock, p: &TxJsonParams) -> UniValue {
    let mut outputs = UniValue::new_array();
    for out in &b.credit_outputs {
        let script = out.script_pubkey.as_bytes();
        let mut o = UniValue::new_object();
        o.push_kv_end("value", amount(out.value));
        o.push_kv_end("valueSat", out.value.into());
        o.push_kv_end("scriptPubKey", script_to_univ(script, true, false, &p.key_io));
        if b.version >= 2
            && p.key_io.platform_hrp.is_some()
            && let Some(dest) = platform_destination_from_script(script)
        {
            o.push_kv_end("address", encode_platform_destination(&dest, &p.key_io).into());
        }
        outputs.push_back(o);
    }
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("creditOutputs", outputs);
    ret
}

/// `PlatformDestinationFromScript`: the Platform destination of a script `ExtractDestination`
/// reads as a key hash (P2PK included) or a script hash.
// Core: key_io.cpp:319-330
fn platform_destination_from_script(script: &[u8]) -> Option<PlatformDestination> {
    Some(match extract_destination(script)? {
        Destination::PkHash(h) => PlatformDestination::P2pkh(h),
        Destination::ScriptHash(h) => PlatformDestination::P2sh(h),
    })
}

/// `CAssetUnlockPayload::ToJson`.
// Core: evo/core_write.cpp:122-132
fn asset_unlock_tx(b: &AssetUnlock) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("version", i64::from(b.version).into());
    ret.push_kv_end("index", b.index.into());
    ret.push_kv_end("fee", i64::from(b.fee).into());
    ret.push_kv_end("requestedHeight", i64::from(b.requested_height).into());
    ret.push_kv_end("quorumHash", uint_hex(&b.quorum_hash));
    ret.push_kv_end("quorumSig", hex(&b.quorum_sig.to_bytes()).into());
    ret
}

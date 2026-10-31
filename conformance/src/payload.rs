//! Field-by-field comparison of special-transaction payloads decoded by `dashcore` and by
//! `dash_primitives`.
//!
//! The two libraries model the same wire fields with different Rust types (e.g. `OutPoint`
//! vs `collateral_hash` + `collateral_index`, `SocketAddr` vs `ServiceV1`), so each mapped
//! field is compared through its consensus encoding, or as an integer where both sides are
//! integers. Every difference is reported with the field path and both values in hex.

use dash_pkc::bls::{BlsGlobal, BlsLazy, BlsSchemeKind, BlsWirePoint};
use dash_primitives::{
    AssetLock, AssetUnlock, CoinbaseCommitment, CollateralShare, CompactSig, FinalCommitment,
    MnHardFork, NetInfo, PayoutShare, ProDisTx, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx,
    ProUpShareTx, ProUpSharedRegTx, SpecialPayload,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashcore::consensus::{Encodable, deserialize, serialize};
use dashcore::crypto::ecdsa::EcdsaRecSignature;
use dashcore::sml::masternode_list_entry::MasternodeNetInfo;
use dashcore::transaction::special_transaction::TransactionPayload as Dc;
use dashcore::transaction::special_transaction::asset_lock::AssetLockPayload;
use dashcore::transaction::special_transaction::asset_unlock::qualified_asset_unlock::AssetUnlockPayload;
use dashcore::transaction::special_transaction::coinbase::CoinbasePayload;
use dashcore::transaction::special_transaction::mnhf_signal::MnhfSignalPayload;
use dashcore::transaction::special_transaction::provider_dissolve::ProviderDissolvePayload;
use dashcore::transaction::special_transaction::provider_registration::{
    CollateralShare as DcShare, MasternodePayoutShare, ProviderRegistrationPayload,
};
use dashcore::transaction::special_transaction::provider_update_registrar::ProviderUpdateRegistrarPayload;
use dashcore::transaction::special_transaction::provider_update_revocation::ProviderUpdateRevocationPayload;
use dashcore::transaction::special_transaction::provider_update_service::ProviderUpdateServicePayload;
use dashcore::transaction::special_transaction::provider_update_share::ProviderUpdateSharePayload;
use dashcore::transaction::special_transaction::provider_update_shared_registrar::ProviderUpdateSharedRegistrarPayload;
use dashcore::transaction::special_transaction::quorum_commitment::{
    QuorumCommitmentPayload, QuorumEntry,
};
use hex_conservative::DisplayHex;

/// One field whose values differ between the libraries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    pub dashcore: String,
    pub base_sdk: String,
}

/// Consensus encoding through `dashcore`.
pub fn dc_bytes<T: Encodable + ?Sized>(v: &T) -> Vec<u8> {
    serialize(v)
}

/// Consensus encoding through `dash_primitives`/`dash_types`.
pub fn bs_bytes<T: BaseCodec>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(&mut out);
    out
}

/// A BLS field under Core's process-wide flag as the oracle holds it: the object Core reads
/// under [`ORACLE_BLS`](crate::check::ORACLE_BLS), re-serialized under that flag.
pub fn bs_global<K: BlsWirePoint<N>, const N: usize>(f: &BlsGlobal<K, N>) -> Vec<u8> {
    let oracle = crate::check::ORACLE_BLS;
    f.get(oracle).to_bytes(oracle.scheme()).to_vec()
}

/// A lazy BLS field's wire image: the raw bytes it holds before any `Get()`.
pub fn bs_lazy<K: BlsWirePoint<N>, const N: usize>(f: &BlsLazy<K, N>) -> Vec<u8> {
    f.to_bytes().to_vec()
}

/// Collects differences for one payload.
pub struct Cmp {
    ty: &'static str,
    diffs: Vec<FieldDiff>,
}

impl Cmp {
    pub fn new(ty: &'static str) -> Self {
        Self {
            ty,
            diffs: Vec::new(),
        }
    }

    fn diff(&mut self, field: &str, dashcore: String, base_sdk: String) {
        self.diffs.push(FieldDiff {
            field: format!("{}.{}", self.ty, field),
            dashcore,
            base_sdk,
        });
    }

    pub fn bytes(&mut self, field: &str, dashcore: &[u8], base_sdk: &[u8]) {
        if dashcore != base_sdk {
            self.diff(field, dashcore.to_lower_hex_string(), base_sdk.to_lower_hex_string());
        }
    }

    fn text(&mut self, field: &str, dashcore: String, base_sdk: String) {
        if dashcore != base_sdk {
            self.diff(field, dashcore, base_sdk);
        }
    }

    pub fn int(&mut self, field: &str, dashcore: i128, base_sdk: i128) {
        if dashcore != base_sdk {
            self.diff(field, dashcore.to_string(), base_sdk.to_string());
        }
    }

    /// Optional fields: presence must agree, and present values must be byte-identical.
    pub fn opt_bytes(&mut self, field: &str, dashcore: Option<Vec<u8>>, base_sdk: Option<Vec<u8>>) {
        let show = |v: &Option<Vec<u8>>| match v {
            Some(b) => b.to_lower_hex_string(),
            None => "<absent>".to_string(),
        };
        if dashcore != base_sdk {
            self.diff(field, show(&dashcore), show(&base_sdk));
        }
    }

    pub fn opt_int(&mut self, field: &str, dashcore: Option<i128>, base_sdk: Option<i128>) {
        let show = |v: Option<i128>| v.map_or_else(|| "<absent>".to_string(), |v| v.to_string());
        if dashcore != base_sdk {
            self.diff(field, show(dashcore), show(base_sdk));
        }
    }

    pub fn finish(self) -> Vec<FieldDiff> {
        self.diffs
    }
}

/// The `TxType` name of a base-sdk payload, for reporting.
pub fn bs_variant(p: &SpecialPayload) -> &'static str {
    match p {
        SpecialPayload::ProviderRegister(_) => "ProRegTx",
        SpecialPayload::ProviderUpdateService(_) => "ProUpServTx",
        SpecialPayload::ProviderUpdateRegistrar(_) => "ProUpRegTx",
        SpecialPayload::ProviderUpdateRevoke(_) => "ProUpRevTx",
        SpecialPayload::CoinbaseCommitment(_) => "CbTx",
        SpecialPayload::QuorumCommitment(_) => "QcTx",
        SpecialPayload::MnhfSignal(_) => "MnhfTx",
        SpecialPayload::AssetLock(_) => "AssetLockTx",
        SpecialPayload::AssetUnlock(_) => "AssetUnlockTx",
        SpecialPayload::ProviderDissolve(_) => "ProDisTx",
        SpecialPayload::ProviderUpdateShare(_) => "ProUpShareTx",
        SpecialPayload::ProviderUpdateSharedRegistrar(_) => "ProUpSharedRegTx",
        // Core reads only the version of a ProTx of unknown version; no valid chain has one.
        SpecialPayload::UnknownProTxVersion {
            ..
        } => "UnknownProTxVersion",
        SpecialPayload::Unknown {
            ..
        } => "Unknown",
    }
}

/// The equivalent name of a dashcore payload.
pub fn dc_variant(p: &Dc) -> &'static str {
    match p {
        Dc::ProviderRegistrationPayloadType(_) => "ProRegTx",
        Dc::ProviderUpdateServicePayloadType(_) => "ProUpServTx",
        Dc::ProviderUpdateRegistrarPayloadType(_) => "ProUpRegTx",
        Dc::ProviderUpdateRevocationPayloadType(_) => "ProUpRevTx",
        Dc::CoinbasePayloadType(_) => "CbTx",
        Dc::QuorumCommitmentPayloadType(_) => "QcTx",
        Dc::MnhfSignalPayloadType(_) => "MnhfTx",
        Dc::AssetLockPayloadType(_) => "AssetLockTx",
        Dc::AssetUnlockPayloadType(_) => "AssetUnlockTx",
        Dc::ClassicalWithNonStandardVersionTypeBytesPayloadType(_) => "ClassicNonStandardType",
        // The v24 shared-masternode types (10–12), named after Core's classes and JSON keys
        // like the others.
        Dc::ProviderDissolvePayloadType(_) => "ProDisTx",
        Dc::ProviderUpdateSharePayloadType(_) => "ProUpShareTx",
        Dc::ProviderUpdateSharedRegistrarPayloadType(_) => "ProUpSharedRegTx",
        // A payload dashcore keeps as raw bytes; base-sdk's `SpecialPayload::Unknown`.
        Dc::RawPayloadType(..) => "Unknown",
    }
}

/// Compares two decoded payloads field by field. Returns `None` when the variants differ
/// (the caller reports that as a variant mismatch).
pub fn compare(dc: &Dc, bs: &SpecialPayload) -> Option<Vec<FieldDiff>> {
    Some(match (dc, bs) {
        (Dc::ProviderRegistrationPayloadType(a), SpecialPayload::ProviderRegister(b)) => {
            pro_reg(a, b)
        }
        (Dc::ProviderUpdateServicePayloadType(a), SpecialPayload::ProviderUpdateService(b)) => {
            pro_up_serv(a, b)
        }
        (Dc::ProviderUpdateRegistrarPayloadType(a), SpecialPayload::ProviderUpdateRegistrar(b)) => {
            pro_up_reg(a, b)
        }
        (Dc::ProviderUpdateRevocationPayloadType(a), SpecialPayload::ProviderUpdateRevoke(b)) => {
            pro_up_rev(a, b)
        }
        (Dc::CoinbasePayloadType(a), SpecialPayload::CoinbaseCommitment(b)) => cbtx(a, b),
        (Dc::QuorumCommitmentPayloadType(a), SpecialPayload::QuorumCommitment(b)) => qc(a, b),
        (Dc::MnhfSignalPayloadType(a), SpecialPayload::MnhfSignal(b)) => mnhf(a, b),
        (Dc::AssetLockPayloadType(a), SpecialPayload::AssetLock(b)) => asset_lock(a, b),
        (Dc::AssetUnlockPayloadType(a), SpecialPayload::AssetUnlock(b)) => asset_unlock(a, b),
        (Dc::ProviderDissolvePayloadType(a), SpecialPayload::ProviderDissolve(b)) => pro_dis(a, b),
        (Dc::ProviderUpdateSharePayloadType(a), SpecialPayload::ProviderUpdateShare(b)) => {
            pro_up_share(a, b)
        }
        (
            Dc::ProviderUpdateSharedRegistrarPayloadType(a),
            SpecialPayload::ProviderUpdateSharedRegistrar(b),
        ) => pro_up_shared_reg(a, b),
        _ => return None,
    })
}

/// The optional platform fields of ProRegTx and ProUpServTx: consensus-encoded node ID, P2P
/// port and HTTP port.
type Platform = (Option<Vec<u8>>, Option<u16>, Option<u16>);

fn platform(c: &mut Cmp, (dc_id, dc_p2p, dc_http): Platform, (bs_id, bs_p2p, bs_http): Platform) {
    c.opt_bytes("platform_node_id", dc_id, bs_id);
    c.opt_int("platform_p2p_port", dc_p2p.map(Into::into), bs_p2p.map(Into::into));
    c.opt_int("platform_http_port", dc_http.map(Into::into), bs_http.map(Into::into));
}

/// Compares the service addresses as Core holds them. A v3 (ExtAddr) payload's `ExtNetInfo`
/// is compared through dashcore's canonical form (purposes in code order, the first list of
/// each, unreadable or invalid entries invalid), since base-sdk decodes it that way; a legacy
/// one through its 18-byte wire image, all zeros where Core reads the service as empty.
fn net_info(c: &mut Cmp, a: &MasternodeNetInfo, b: &NetInfo) {
    match a {
        MasternodeNetInfo::Extended(ext) => c.text(
            "net_info",
            format!("{:?}", crate::verbose::dc_ext_net_info(&ext.canonical())),
            format!("{:?}", crate::verbose::bs_net_info(b)),
        ),
        MasternodeNetInfo::Legacy(addr) => {
            c.bytes("net_info", &legacy_as_core_holds(&dc_bytes(addr)), &net_info_bytes(b))
        }
    }
}

/// Core's `MnNetInfo` keeps a legacy service only when `CService::IsValid`; it serializes an
/// empty one as `CService{}`, 18 zero bytes.
fn legacy_as_core_holds(wire: &[u8]) -> Vec<u8> {
    let (Ok(addr), Ok(port)) =
        (<[u8; 16]>::try_from(&wire[..16]), <[u8; 2]>::try_from(&wire[16..]))
    else {
        return wire.to_vec();
    };
    let service = crate::render::Service::from_legacy(addr, u16::from_be_bytes(port));
    if service.addr.is_valid() {
        wire.to_vec()
    } else {
        vec![0; 18]
    }
}

/// The scheme Core serializes a ProTx BLS field under: legacy for version 1 only
/// (`nVersion == ProTxVersion::LegacyBLS`). The harness applies Core's rule itself rather
/// than the scheme base-sdk recorded.
pub fn protx_scheme(version: u16) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(version == 1)
}

/// The scheme Core serializes a final commitment's BLS fields under: legacy for versions 1
/// and 2.
pub fn qc_scheme(version: u16) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(version == 1 || version == 2)
}

fn net_info_bytes(n: &NetInfo) -> Vec<u8> {
    match n {
        NetInfo::Legacy(v1) => bs_bytes(v1),
        NetInfo::Extended(v2) => bs_bytes(v2),
    }
}

fn pro_reg(a: &ProviderRegistrationPayload, b: &ProRegTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProRegTx");
    c.int("version", a.version.into(), u16::from(b.version).into());
    c.int("mn_type", a.masternode_type as i128, b.mn_type.to_base().into());
    c.int("mode", a.masternode_mode.into(), b.mode.into());
    let mut collateral = bs_bytes(&b.collateral_hash);
    collateral.extend_from_slice(&b.collateral_index.to_le_bytes());
    c.bytes("collateral_outpoint", &dc_bytes(&a.collateral_outpoint), &collateral);
    net_info(&mut c, &a.service_address, &b.net_info);
    c.bytes("key_id_owner", &dc_bytes(&a.owner_key_hash), &bs_bytes(&b.key_id_owner));
    c.bytes("pub_key_operator", &dc_bytes(&a.operator_public_key), &bs_lazy(&b.pub_key_operator));
    c.bytes("key_id_voting", &dc_bytes(&a.voting_key_hash), &bs_bytes(&b.key_id_voting));
    c.int("operator_reward", a.operator_reward.into(), b.operator_reward.into());
    c.bytes("script_payout", a.script_payout.as_bytes(), b.script_payout.as_bytes());
    payouts(&mut c, a.payouts.as_deref().unwrap_or_default(), &b.payouts);
    // dashcore keeps the version 3 sharing terms together, base-sdk flat (empty below version 3).
    let terms = a.shared_collateral.as_ref();
    shares(&mut c, terms.map_or(&[][..], |t| &t.shares), &b.shares);
    c.text("join_sigs", sigs(terms.map_or(&[][..], |t| &t.join_sigs)), bs_sigs(&b.join_sigs));
    let (period, penalty) = terms.map_or((0, 0), |t| (t.early_period_blocks, t.early_penalty));
    c.int("early_period_blocks", period.into(), b.early_period_blocks.into());
    c.int("early_penalty", penalty.into(), b.early_penalty.into());
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    platform(
        &mut c,
        (a.platform_node_id.as_ref().map(dc_bytes), a.platform_p2p_port, a.platform_http_port),
        (b.platform_node_id.as_ref().map(bs_bytes), b.platform_p2p_port, b.platform_http_port),
    );
    c.bytes("sig", &a.signature, b.vch_sig.as_slice());
    c.finish()
}

fn pro_up_serv(a: &ProviderUpdateServicePayload, b: &ProUpServTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProUpServTx");
    c.int("version", a.version.into(), u16::from(b.version).into());
    // nType is on the wire from version 2 on; base-sdk defaults it to Regular below that.
    c.opt_int(
        "mn_type",
        a.mn_type.map(Into::into),
        (u16::from(b.version) >= 2).then(|| b.mn_type.to_base().into()),
    );
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    net_info(&mut c, &a.service_address, &b.net_info);
    c.bytes("script_payout", a.script_payout.as_bytes(), b.script_operator_payout.as_bytes());
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    platform(
        &mut c,
        (a.platform_node_id.as_ref().map(dc_bytes), a.platform_p2p_port, a.platform_http_port),
        (b.platform_node_id.as_ref().map(bs_bytes), b.platform_p2p_port, b.platform_http_port),
    );
    c.bytes("sig", &dc_bytes(&a.payload_sig), &b.sig.to_bytes(protx_scheme(u16::from(b.version))));
    c.finish()
}

fn pro_up_reg(a: &ProviderUpdateRegistrarPayload, b: &ProUpRegTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProUpRegTx");
    c.int("version", a.version.into(), u16::from(b.version).into());
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    c.int("mode", a.provider_mode.into(), b.mode.into());
    c.bytes("pub_key_operator", &dc_bytes(&a.operator_public_key), &bs_lazy(&b.pub_key_operator));
    c.bytes("key_id_voting", &dc_bytes(&a.voting_key_hash), &bs_bytes(&b.key_id_voting));
    c.bytes("script_payout", a.script_payout.as_bytes(), b.script_payout.as_bytes());
    payouts(&mut c, a.payouts.as_deref().unwrap_or_default(), &b.payouts);
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    c.bytes("sig", &a.payload_sig, b.vch_sig.as_slice());
    c.finish()
}

fn pro_up_rev(a: &ProviderUpdateRevocationPayload, b: &ProUpRevTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProUpRevTx");
    c.int("version", a.version.into(), u16::from(b.version).into());
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    c.int("reason", a.reason.into(), b.reason.to_base().into());
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    c.bytes("sig", &dc_bytes(&a.payload_sig), &b.sig.to_bytes(protx_scheme(u16::from(b.version))));
    c.finish()
}

fn cbtx(a: &CoinbasePayload, b: &CoinbaseCommitment) -> Vec<FieldDiff> {
    let mut c = Cmp::new("CbTx");
    c.int("version", a.version.into(), b.version.into());
    c.int("height", a.height.into(), b.height.into());
    c.bytes(
        "merkle_root_mn_list",
        &dc_bytes(&a.merkle_root_masternode_list),
        &bs_bytes(&b.merkle_root_mn_list),
    );
    // dashcore has no "absent" state for merkleRootQuorums (v1 decodes to all zeros); only
    // compare it when the payload version carries the field.
    c.opt_bytes(
        "merkle_root_quorums",
        (a.version >= 2).then(|| dc_bytes(&a.merkle_root_quorums)),
        b.merkle_root_quorums.as_ref().map(bs_bytes),
    );
    c.opt_int(
        "best_cl_height_diff",
        a.best_cl_height.map(Into::into),
        b.best_cl_height_diff.map(Into::into),
    );
    c.opt_bytes(
        "best_cl_signature",
        a.best_cl_signature.as_ref().map(dc_bytes),
        b.best_cl_signature.as_ref().map(bs_global),
    );
    c.opt_int(
        "credit_pool_balance",
        a.asset_locked_amount.map(Into::into),
        b.credit_pool_balance.map(Into::into),
    );
    c.opt_bytes(
        "merkle_root_asset_unlocks",
        a.merkle_root_asset_unlocks.as_ref().map(dc_bytes),
        b.merkle_root_asset_unlocks.as_ref().map(bs_bytes),
    );
    c.finish()
}

fn bits(v: &[bool]) -> String {
    v.iter()
        .map(|b| {
            if *b {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

fn bs_bits(b: &dash_primitives::DynBitset) -> String {
    (0..b.num_bits)
        .map(|i| {
            let byte = b.data.get((i / 8) as usize).copied().unwrap_or(0);
            if byte >> (i % 8) & 1 == 1 {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

fn qc(a: &QuorumCommitmentPayload, b: &FinalCommitment) -> Vec<FieldDiff> {
    let mut c = Cmp::new("QcTx");
    // `QuorumCommitmentPayload` keeps its fields private; take `version`/`height` from its
    // own encoding and re-read the commitment with dashcore's `QuorumEntry` decoder.
    let raw = dc_bytes(a);
    let entry: Option<QuorumEntry> = raw.get(6..).and_then(|r| deserialize(r).ok());
    let (Some(version), Some(height), Some(e)) = (
        raw.get(0..2).map(|v| u16::from_le_bytes([v[0], v[1]])),
        raw.get(2..6).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]])),
        entry,
    ) else {
        c.diff("encoding", raw.to_lower_hex_string(), "<dashcore re-read failed>".into());
        return c.finish();
    };
    let q = &b.commitment;
    let scheme = qc_scheme(q.version);
    c.int("version", version.into(), b.version.into());
    c.int("height", height.into(), b.height.to_u32().into());
    c.int("commitment.version", e.version.into(), q.version.into());
    c.int("commitment.llmq_type", u8::from(e.llmq_type).into(), q.llmq_type.to_base().into());
    c.bytes("commitment.quorum_hash", &dc_bytes(&e.quorum_hash), &bs_bytes(&q.quorum_hash));
    c.opt_int(
        "commitment.quorum_index",
        e.quorum_index.map(Into::into),
        q.quorum_index.map(Into::into),
    );
    c.text("commitment.signers", bits(&e.signers), bs_bits(&q.signers));
    c.text("commitment.valid_members", bits(&e.valid_members), bs_bits(&q.valid_members));
    c.bytes(
        "commitment.quorum_public_key",
        &dc_bytes(&e.quorum_public_key),
        &q.quorum_public_key.to_bytes(scheme),
    );
    c.bytes(
        "commitment.quorum_vvec_hash",
        &dc_bytes(&e.quorum_vvec_hash),
        &bs_bytes(&q.quorum_vvec_hash),
    );
    c.bytes("commitment.quorum_sig", &dc_bytes(&e.threshold_sig), &q.quorum_sig.to_bytes(scheme));
    c.bytes(
        "commitment.members_sig",
        &dc_bytes(&e.all_commitment_aggregated_signature),
        &q.members_sig.to_bytes(scheme),
    );
    c.finish()
}

fn mnhf(a: &MnhfSignalPayload, b: &MnHardFork) -> Vec<FieldDiff> {
    let mut c = Cmp::new("MnhfTx");
    c.int("version", a.version.into(), b.version.into());
    c.int("version_bit", a.version_bit.into(), b.version_bit.into());
    c.bytes("quorum_hash", &dc_bytes(&a.quorum_hash), &bs_bytes(&b.quorum_hash));
    c.bytes("sig", &dc_bytes(&a.sig), &b.sig.to_bytes(BlsSchemeKind::Basic));
    c.finish()
}

fn asset_lock(a: &AssetLockPayload, b: &AssetLock) -> Vec<FieldDiff> {
    let mut c = Cmp::new("AssetLockTx");
    c.int("version", a.version.into(), b.version.into());
    c.int("credit_outputs.len", a.credit_outputs.len() as i128, b.credit_outputs.len() as i128);
    for (i, (x, y)) in a.credit_outputs.iter().zip(&b.credit_outputs).enumerate() {
        c.int(&format!("credit_outputs[{i}].value"), x.value.into(), y.value.into());
        c.bytes(
            &format!("credit_outputs[{i}].script_pubkey"),
            x.script_pubkey.as_bytes(),
            y.script_pubkey.as_bytes(),
        );
    }
    c.finish()
}

fn asset_unlock(a: &AssetUnlockPayload, b: &AssetUnlock) -> Vec<FieldDiff> {
    let mut c = Cmp::new("AssetUnlockTx");
    c.int("version", a.base.version.into(), b.version.into());
    c.int("index", a.base.index.into(), b.index.into());
    c.int("fee", a.base.fee.into(), b.fee.into());
    c.int("requested_height", a.request_info.request_height.into(), b.requested_height.into());
    c.bytes("quorum_hash", &dc_bytes(&a.request_info.quorum_hash), &bs_bytes(&b.quorum_hash));
    c.bytes("quorum_sig", &dc_bytes(&a.quorum_sig), &bs_global(&b.quorum_sig));
    c.finish()
}

/// DIP-0026 owner payouts (v3).
fn payouts(c: &mut Cmp, a: &[MasternodePayoutShare], b: &[PayoutShare]) {
    c.int("payouts.len", a.len() as i128, b.len() as i128);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        c.bytes(
            &format!("payouts[{i}].script"),
            x.script_payout.as_bytes(),
            y.script_payout.as_bytes(),
        );
        c.int(&format!("payouts[{i}].reward"), x.reward.into(), y.reward.into());
    }
}

/// Shared-masternode collateral shares (v3).
fn shares(c: &mut Cmp, a: &[DcShare], b: &[CollateralShare]) {
    c.int("shares.len", a.len() as i128, b.len() as i128);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        c.int(&format!("shares[{i}].amount"), x.amount.into(), y.amount.into());
        c.bytes(
            &format!("shares[{i}].script_refund"),
            x.script_refund.as_bytes(),
            y.script_refund.as_bytes(),
        );
        c.bytes(
            &format!("shares[{i}].script_reward"),
            x.script_reward.as_bytes(),
            y.script_reward.as_bytes(),
        );
        c.bytes(
            &format!("shares[{i}].key_id_owner"),
            &dc_bytes(&x.owner_key_hash),
            &bs_bytes(&y.key_id_owner),
        );
    }
}

/// Compact signatures as one hex string per signature.
fn sigs(v: &[EcdsaRecSignature]) -> String {
    v.iter().map(|s| s.as_bytes().to_lower_hex_string()).collect::<Vec<_>>().join(",")
}

fn bs_sigs(v: &[CompactSig]) -> String {
    v.iter().map(|s| s.as_bytes().to_lower_hex_string()).collect::<Vec<_>>().join(",")
}

fn pro_dis(a: &ProviderDissolvePayload, b: &ProDisTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProDisTx");
    c.int("version", a.version.into(), b.version.into());
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    c.int("actor_index", a.actor_index.into(), b.actor_index.into());
    c.text("sigs", sigs(&a.signatures), bs_sigs(&b.sigs));
    c.finish()
}

fn pro_up_share(a: &ProviderUpdateSharePayload, b: &ProUpShareTx) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProUpShareTx");
    c.int("version", a.version.into(), b.version.into());
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    c.int("share_index", a.share_index.into(), b.share_index.into());
    c.bytes("script_reward", a.script_reward.as_bytes(), b.script_reward.as_bytes());
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    c.bytes("sig", &a.payload_sig, &b.vch_sig);
    c.finish()
}

fn pro_up_shared_reg(
    a: &ProviderUpdateSharedRegistrarPayload,
    b: &ProUpSharedRegTx,
) -> Vec<FieldDiff> {
    let mut c = Cmp::new("ProUpSharedRegTx");
    c.int("version", a.version.into(), b.version.into());
    c.bytes("pro_tx_hash", &dc_bytes(&a.pro_tx_hash), &bs_bytes(&b.pro_tx_hash));
    c.bytes("pub_key_operator", &dc_bytes(&a.operator_public_key), &bs_lazy(&b.pub_key_operator));
    c.bytes("key_id_voting", &dc_bytes(&a.voting_key_hash), &bs_bytes(&b.key_id_voting));
    c.bytes("inputs_hash", &dc_bytes(&a.inputs_hash), &bs_bytes(&b.inputs_hash));
    c.text("sigs", sigs(&a.signatures), bs_sigs(&b.sigs));
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitset_rendering_matches() {
        let b = dash_primitives::DynBitset {
            num_bits: 10,
            data: vec![0b0000_0101, 0b0000_0010],
        };
        assert_eq!(bs_bits(&b), "1010000001");
        assert_eq!(
            bits(&[true, false, true, false, false, false, false, false, false, true]),
            "1010000001"
        );
    }

    #[test]
    fn cmp_reports_only_differences() {
        let mut c = Cmp::new("T");
        c.int("same", 1, 1);
        c.int("differs", 1, 2);
        c.opt_bytes("absent", None, Some(vec![0xab]));
        c.bytes("bytes", &[1], &[1]);
        let d = c.finish();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].field, "T.differs");
        assert_eq!(d[1].base_sdk, "ab");
        assert_eq!(d[1].dashcore, "<absent>");
    }

    /// The harness applies Core's scheme rules itself; base-sdk's published ones must agree
    /// for every version.
    #[test]
    fn scheme_rules_agree_with_base_sdk() {
        for version in 0..=u16::MAX {
            if let Ok(protx_version) = dash_primitives::ProTxVersion::try_from(version) {
                assert_eq!(
                    protx_scheme(version),
                    dash_primitives::protx_bls_scheme(protx_version),
                    "{version}"
                );
            }
            assert_eq!(
                qc_scheme(version),
                dash_primitives::commitment_bls_scheme(version),
                "{version}"
            );
        }
    }
}

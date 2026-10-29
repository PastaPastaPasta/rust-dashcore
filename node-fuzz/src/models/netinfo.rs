//! `netinfo_validate` and `netinfo_add_entry` (EV1 §6.5, D29): `dash_dml::netinfo` on arbitrary
//! network info and inputs, on every network. Until the NetInfo oracle entries (A-3) back an L1
//! comparison, the property is the narrowed one: each function is total (a panic is libFuzzer's
//! crash) and deterministic, and the results agree with each other the way Core's do:
//! - `netinfo_validate`: `Validate()`, `CoreNetInfoValidity`, `CheckNetInfo` and
//!   `CheckProviderNetworkFields`; and, from EV1-2 (D29), the transaction verdict: the object in a
//!   Core-signed ProRegTx or ProUpServTx through `GetValidatedPayload` and
//!   `CheckProviderNetworkFields`, against Core's `CheckSpecialTx` (see [`tx_verdict`]);
//! - `netinfo_add_entry`: `AddEntry` leaves the object unchanged when it fails; when it succeeds
//!   on an empty or valid object, the result validates and has an entry for the purpose; and
//!   `migration_reject` over the object before and after.
//!
//! Input: byte 0 picks the network (bits 0-1), the object's kind (bit 2: legacy or extended),
//! nothing with bit 3 (`-allowprivatenet` until rc.4, CE4-02; kept so corpora keep their
//! meaning), CJDNS reachability (bit 4), the masternode type (bit 5) and
//! `allow_empty` (bit 6). `netinfo_validate`: byte 1 is the ProTx version (bits 0-6) and, with
//! byte 0 bit 7, the platform node id (byte 1 bit 7 set: absent, Core's null pointer; else byte 0
//! bit 7 set: the null id; else a non-null id); bytes 2-5 are the two platform ports, the rest
//! the object's wire form. `netinfo_add_entry`: byte 1 is the purpose, byte 2 the two versions of the
//! migration (low and high nibble, mod 4, plus 1), byte 3 the length of the object's wire form,
//! which follows; the rest is AddEntry's input string.

use dash_consensus::deployments::bip9_enabled;
use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_dml::codec::NetInfoValidity;
use dash_dml::netinfo::{
    CoreNetInfoValidity, NetInfoConfig, NetInfoRules, NetInfoStatus, add_entry_bytes, has_entries,
    is_empty, migration_reject, validate,
};
use dash_dml::specialtx::reasons::{
    BAD_PROTX_COLLATERAL, BAD_PROTX_HASH, BAD_PROTX_VERSION_DISALLOWED,
};
use dash_dml::specialtx::validated_payload;
use dash_dml::specialtx::{ProviderNetworkFields, check_net_info, check_provider_network_fields};
use dash_dml::{DmlParams, MnType, RawMnType};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, MnList, Oracle, Pin, ResultCode,
    ThresholdState, Verdict,
};
use dash_params::{ChainParams, ChainParamsBuilder, DeploymentPos, NetworkKind, Release};
use dash_primitives::{
    AddrV1, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId, ProRegTx, ProUpServTx,
    ServiceV1, SpecialPayload, Transaction, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use node_l1::Divergence;
use node_l1::check_inputs::Network;
use node_l1::specialtx::{migration_case, verdicts};
use std::sync::OnceLock;

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(Divergence {
            field,
        })
    }
}

/// Core's parameters for every network, built once; mainnet's is `MainParams()`.
#[allow(clippy::expect_used)] // Core's parameters, constants, always build
fn params() -> &'static [ChainParams; 4] {
    static PARAMS: OnceLock<[ChainParams; 4]> = OnceLock::new();
    PARAMS.get_or_init(|| {
        NetworkKind::ALL
            .map(|k| ChainParamsBuilder::new(k, Release::V24_0).build().expect("Core's parameters"))
    })
}

/// The settings byte 0 selects.
struct Setup {
    rules: NetInfoRules,
    extended: bool,
    mn_type: MnType,
    allow_empty: bool,
}

fn setup(b: u8) -> Setup {
    let all = params();
    let config = NetInfoConfig {
        cjdns_reachable: b & 16 != 0,
    };
    Setup {
        rules: NetInfoRules::new(&all[usize::from(b & 3)], &all[0], config),
        extended: b & 4 != 0,
        mn_type: if b & 32 != 0 {
            MnType::Evo
        } else {
            MnType::Regular
        },
        allow_empty: b & 64 != 0,
    }
}

/// The object's wire form as a ProTx payload carries it, or `None` when it does not decode whole
/// (rc.4's purpose rule included, CE4-01).
fn decode(extended: bool, wire: &[u8]) -> Option<NetInfo> {
    let mut rest = wire;
    let net_info = if extended {
        NetInfo::Extended(NetInfoV2::decode(&mut rest).ok()?)
    } else {
        NetInfo::Legacy(NetInfoV1::decode(&mut rest).ok()?)
    };
    rest.is_empty().then_some(net_info)
}

pub(super) fn validate_check(data: &[u8]) -> Result<(), Divergence> {
    let [b0, b1, p0, p1, h0, h1, wire @ ..] = data else {
        return Ok(());
    };
    let s = setup(*b0);
    let Some(net_info) = decode(s.extended, wire) else {
        return Ok(());
    };
    let status = validate(&net_info, &s.rules);
    ensure(validate(&net_info, &s.rules) == status, "Validate() determinism")?;
    let validity = CoreNetInfoValidity {
        rules: s.rules,
    };
    ensure(validity.is_valid(&net_info) == (status == NetInfoStatus::Success), "is_valid")?;
    ensure(
        check_net_info(&net_info, &s.rules).is_ok() == (status == NetInfoStatus::Success),
        "CheckNetInfo",
    )?;
    // Validate() never returns BadAddress (census: providertx.cpp:191 is unreachable).
    ensure(status != NetInfoStatus::BadAddress, "Validate() BadAddress")?;

    // Byte 1 bit 7: no node id (Core's null pointer); byte 0 bit 7: the null id.
    let node_id = PlatformNodeId::from([u8::from(b0 & 0x80 == 0); 20]);
    let fields = ProviderNetworkFields {
        net_info: &net_info,
        mn_type: s.mn_type,
        version: u16::from(b1 & 0x7f),
        platform_node_id: (b1 & 0x80 == 0).then_some(&node_id),
        platform_p2p_port: u16::from_be_bytes([*p0, *p1]),
        platform_http_port: u16::from_be_bytes([*h0, *h1]),
    };
    let first = check_provider_network_fields(&fields, s.allow_empty, &s.rules);
    let again = check_provider_network_fields(&fields, s.allow_empty, &s.rules);
    ensure(first == again, "CheckProviderNetworkFields determinism")?;
    // A non-empty object it accepts is one Validate() accepts.
    ensure(
        first.is_err() || is_empty(&net_info) || status == NetInfoStatus::Success,
        "accepted invalid object",
    )?;
    tx_verdict(*b0, *b1, [*p0, *p1, *h0, *h1], net_info).map(drop)
}

/// BS6b's Core-signed provider transactions (`dash-oracle-sys`' signature records).
const RECORDS: &str =
    include_str!("../../../dash-oracle-sys/tests/data/special-tx-signatures.json");

/// The templates [`tx_verdict`] wraps an object in: (registration, Evo, version, txid). Each
/// passes every check before `CheckProviderNetworkFields` but those its version fails (a ProRegTx
/// below v2 is `bad-protx-version-disallowed` under V24), and none has shares or a second owner
/// payout (which an EvoNode may not have before `evo_shares`, a check between them).
const TEMPLATES: [(bool, bool, u16, &str); 10] = [
    (true, false, 1, "213955c27a1edd9aababab39ebc2fd5adca4735604085900c83648bcadbb3f24"),
    (true, false, 2, "3d574c41c67e2b8653182caa7a0182d0dffe8f70f3fa7c1c48cc20f6d48a121f"),
    (true, true, 2, "b03e50436c63fdd1d2eacf5e754d808f88b1af7a86027f080c96d712be1d91fe"),
    (true, false, 3, "5bb21f874f50967df6faf711521c13d0723991d8d1e8c00669c9f49eea248a01"),
    (true, true, 3, "c3578cbc6007be067b53490f594e0ba310bf3335c4886c42e195ed690ebbcffb"),
    (false, false, 1, "c2c940e4bb0c6775cb39e62d78669164e28df81852d23d3aef297d400202e503"),
    (false, false, 2, "b58615251ebd9280ada7d27f3e66da9254e28639497cee1ee967799c928327e5"),
    (false, true, 2, "224e7793e6525919ca9033ce72a00afb2f224ac11a98962fd6da01431ca8c19f"),
    (false, false, 3, "ae738cc6547cf27fab740924adb65af5b71496c77cd4dd76c078a0300c562085"),
    (false, true, 3, "9e548bf24c2d75ce88189228a4ac50aee8a2aeda8ef00c51b25db24815952182"),
];

/// The record `txid`'s transaction.
#[allow(clippy::expect_used)] // a fixed file in the repository
fn record(txid: &str) -> Tx {
    let at = RECORDS.find(txid).expect("the template's record");
    let raw = RECORDS[at..].split("\"raw\": \"").nth(1).and_then(|s| s.split('"').next());
    let raw = raw.expect("the record's transaction");
    let bytes = (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).expect("hex"))
        .collect::<Vec<u8>>();
    let tx = decode_tx_exact(&bytes).expect("Core's transaction").into_value();
    assert_eq!(tx.txid().to_string(), txid, "the record's transaction");
    tx
}

/// A network's side of [`tx_verdict`].
struct Net {
    chain: Chain,
    /// At the first height where DIP3 and V19 are active, V24 active where the network has it
    /// (rc.4's mainnet does not: `DeploymentEnabled` is false and Core ignores the state), and an
    /// empty list.
    context: Context,
    v24: bool,
    rules: NetInfoRules,
}

/// Core's side of [`tx_verdict`], built once per process: the oracle, each network (in
/// [`NetworkKind::ALL`]'s order, as byte 0 picks it) and the templates.
struct Core {
    oracle: Oracle,
    networks: Vec<Net>,
    templates: Vec<Tx>,
}

impl Core {
    #[allow(clippy::expect_used)] // Core's parameters and headers, constants
    fn new() -> Core {
        let oracle = Oracle::new(Pin::V24_0);
        let networks = [Network::Main, Network::Test, Network::Devnet, Network::Regtest]
            .map(|network| {
                let params = network.params();
                let v19 = params.consensus.v19_height.to_u32() as i32;
                let tip = v19.max(DmlParams::new(params).dip3_height);
                let mut header = [0u8; 80];
                header[0..4].copy_from_slice(&4i32.to_le_bytes());
                header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
                let (name, args) = network.core_chain();
                let context = Context::new(
                    tip,
                    vec![header],
                    false,
                    vec![(DEPLOYMENT_V24, ThresholdState::Active)],
                    1_700_000_000,
                    Checkpoints::Disabled,
                    CoinsView::Unchecked(vec![]),
                    MnList::Supplied(empty_list()),
                    vec![],
                );
                // The oracle's process: every network reachable, mainnet's `Params()` (EV1-1 D40).
                let config = NetInfoConfig {
                    cjdns_reachable: true,
                };
                Net {
                    chain: Chain::new(name, args),
                    context,
                    v24: bip9_enabled(params, DeploymentPos::V24),
                    rules: NetInfoRules::new(params, &self::params()[0], config),
                }
            })
            .into();
        Core {
            oracle,
            networks,
            templates: TEMPLATES.iter().map(|t| record(t.3)).collect(),
        }
    }
}

/// An empty list at no block (`CDeterministicMNList`'s serialization).
fn empty_list() -> Vec<u8> {
    let mut list = vec![0u8; 32];
    list.extend([0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0]);
    list
}

thread_local! {
    static CORE: Core = Core::new();
}

/// The target's start: Core's oracle, the networks and the templates, built once, then one
/// oracle check per network and template, so that Core's first-call set-up runs before any
/// input too. Built inside the first input instead, it lent its coverage to whichever input
/// ran first, and certify's two replays of one corpus differed (D28g). A check that fails or
/// does not reach Core stops the process before its first input.
pub(super) fn validate_start() {
    for (i, check) in start_checks().into_iter().enumerate() {
        assert!(matches!(check, Ok(Some(_))), "netinfo_validate start: check {i}: {check:?}");
    }
}

/// The start's oracle checks, each template unchanged on each network; a template without a
/// provider payload is a divergence.
fn start_checks() -> Vec<Result<Option<bool>, Divergence>> {
    let net_infos: Vec<Option<NetInfo>> = CORE.with(|core| {
        core.templates
            .iter()
            .map(|t| match t.payload() {
                Some(Ok(decoded)) => match &decoded.payload {
                    SpecialPayload::ProviderRegister(p) => Some(p.net_info.clone()),
                    SpecialPayload::ProviderUpdateService(p) => Some(p.net_info.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    });
    let mut checks = Vec::new();
    for network in 0..4u8 {
        for ((registration, evo, version, _), net_info) in TEMPLATES.iter().zip(&net_infos) {
            let Some(net_info) = net_info else {
                checks.push(Err(Divergence {
                    field: "template payload",
                }));
                continue;
            };
            let b0 = network
                | u8::from(*version >= 3) << 2
                | u8::from(*evo) << 5
                | u8::from(*registration) << 6;
            checks.push(tx_verdict(b0, *version as u8, [0, 1, 0, 2], net_info.clone()));
        }
    }
    checks
}

/// Our checks up to `CheckProviderNetworkFields` on `tx` (V19 active).
fn ours(tx: &Tx, net: &Net) -> Result<(), TxValidationError> {
    let (t, slot) = (tx.tx_type(), tx.payload());
    let (net_info, mn_type, version, node_id, p2p, http, allow_empty) = match t {
        TxType::ProviderRegister => {
            let p = validated_payload::<ProRegTx>(t, slot, true, net.v24)?;
            if net.v24 && p.version < 2 {
                return Err(TxValidationError::new(
                    TxValidationResult::Consensus,
                    BAD_PROTX_VERSION_DISALLOWED,
                ));
            }
            (
                &p.net_info,
                p.mn_type,
                p.version,
                p.platform_node_id.as_ref(),
                p.platform_p2p_port,
                p.platform_http_port,
                true,
            )
        }
        _ => {
            let p = validated_payload::<ProUpServTx>(t, slot, true, net.v24)?;
            (
                &p.net_info,
                p.mn_type,
                p.version,
                p.platform_node_id.as_ref(),
                p.platform_p2p_port,
                p.platform_http_port,
                false,
            )
        }
    };
    // The trivial checks passed: the type is a known one.
    let mn_type = MnType::from_raw(RawMnType(mn_type.to_base())).unwrap_or(MnType::Regular);
    // Core's consensus callers pass the payload's id, the null one when it has none.
    let null = PlatformNodeId::from([0; 20]);
    let fields = ProviderNetworkFields {
        net_info,
        mn_type,
        version,
        platform_node_id: Some(node_id.unwrap_or(&null)),
        platform_p2p_port: p2p.unwrap_or(0),
        platform_http_port: http.unwrap_or(0),
    };
    check_provider_network_fields(&fields, allow_empty, &net.rules)
}

/// The transaction verdict (D29): `net_info` in the template byte 0 (bit 6: a registration, as
/// `allow_empty`; bit 5: an EvoNode) and byte 1 (the version, when a template has it and it
/// carries the object's kind) pick, with the platform fields of the input where the template
/// has them, through ours and Core's `CheckSpecialTx` (`check_sigs` off) on the network byte 0
/// picks. Where ours rejects, Core rejects alike (class and reason); where ours passes, Core
/// rejects at the next step: the empty list's `bad-protx-hash` for a ProUpServTx, and for a
/// ProRegTx the empty coins view's `bad-protx-collateral` (its external collateral, C6). Returns
/// whether ours passed, `None` when no template fits.
fn tx_verdict(
    b0: u8,
    b1: u8,
    ports: [u8; 4],
    net_info: NetInfo,
) -> Result<Option<bool>, Divergence> {
    let (registration, evo, version) = (b0 & 64 != 0, b0 & 32 != 0, u16::from(b1 & 0x7f));
    if matches!(net_info, NetInfo::Extended(_)) != (version >= 3) {
        return Ok(None);
    }
    let Some(at) = TEMPLATES.iter().position(|t| (t.0, t.1, t.2) == (registration, evo, version))
    else {
        return Ok(None);
    };
    CORE.with(|core| {
        let net = &core.networks[usize::from(b0 & 3)];
        let template = &core.templates[at];
        let Some(Ok(decoded)) = template.payload() else {
            return Err(Divergence {
                field: "template payload",
            });
        };
        let node_id = PlatformNodeId::from([u8::from(b0 & 0x80 == 0); 20]);
        let mut payload = Vec::new();
        match decoded.payload.clone() {
            SpecialPayload::ProviderRegister(mut p) => {
                p.net_info = net_info;
                platform(&mut p.platform_node_id, &mut p.platform_p2p_port, node_id, ports);
                platform_port(&mut p.platform_http_port, [ports[2], ports[3]]);
                p.encode(&mut payload);
            }
            SpecialPayload::ProviderUpdateService(mut p) => {
                p.net_info = net_info;
                platform(&mut p.platform_node_id, &mut p.platform_p2p_port, node_id, ports);
                platform_port(&mut p.platform_http_port, [ports[2], ports[3]]);
                p.encode(&mut payload);
            }
            _ => {
                return Err(Divergence {
                    field: "template type",
                });
            }
        }
        let mut tx: Transaction = template.to_mutable();
        tx.extra_payload = payload;
        let tx = Tx::new(tx);
        let mut raw = Vec::new();
        tx.raw().encode(&mut raw);
        let verdict =
            core.oracle.check_special_tx(&net.chain, &raw, &net.context, false).map_err(|_| {
                Divergence {
                    field: "oracle error",
                }
            })?;
        let Verdict::Reject(rejection) = verdict else {
            return Err(Divergence {
                field: "tx verdict: Core accepts",
            });
        };
        let ours = ours(&tx, net);
        match &ours {
            Err(e) => {
                ensure(rejection.result == ResultCode::Tx(e.result() as i32), "tx verdict: class")?;
                ensure(rejection.reason == e.reject_reason(), "tx verdict: reason")?;
            }
            Ok(()) if registration => ensure(
                rejection.reason == BAD_PROTX_COLLATERAL,
                "tx verdict: Core rejects a registration we pass",
            )?,
            Ok(()) => ensure(
                rejection.reason == BAD_PROTX_HASH,
                "tx verdict: Core rejects an update we pass",
            )?,
        }
        Ok(Some(ours.is_ok()))
    })
}

/// The input's platform node id and P2P port, where the template has them.
fn platform(
    id: &mut Option<PlatformNodeId>,
    p2p: &mut Option<u16>,
    node_id: PlatformNodeId,
    ports: [u8; 4],
) {
    if id.is_some() {
        *id = Some(node_id);
    }
    platform_port(p2p, [ports[0], ports[1]]);
}

fn platform_port(port: &mut Option<u16>, bytes: [u8; 2]) {
    if port.is_some() {
        *port = Some(u16::from_be_bytes(bytes));
    }
}

pub(super) fn add_entry_check(data: &[u8]) -> Result<(), Divergence> {
    let [b0, purpose, versions, len, rest @ ..] = data else {
        return Ok(());
    };
    let Some((wire, input)) = rest.split_at_checked(usize::from(*len)) else {
        return Ok(());
    };
    let s = setup(*b0);
    let Some(before) = decode(s.extended, wire) else {
        return Ok(());
    };
    let purpose = NIPurpose::from_base(*purpose);
    let prior_ok = is_empty(&before) || validate(&before, &s.rules) == NetInfoStatus::Success;

    let mut after = before.clone();
    let status = add_entry_bytes(&mut after, purpose, input, &s.rules);
    let mut again = before.clone();
    ensure(
        add_entry_bytes(&mut again, purpose, input, &s.rules) == status,
        "AddEntry status determinism",
    )?;
    ensure(again == after, "AddEntry object determinism")?;
    if status == NetInfoStatus::Success {
        ensure(has_entries(&after, purpose), "added entry missing")?;
        if prior_ok {
            ensure(validate(&after, &s.rules) == NetInfoStatus::Success, "added entry invalid")?;
        }
    } else {
        ensure(after == before, "failed AddEntry changed the object")?;
        // AddEntry never returns Malformed or BadType.
        ensure(
            !matches!(status, NetInfoStatus::Malformed | NetInfoStatus::BadType),
            "AddEntry status",
        )?;
    }

    let cur = u16::from(versions & 3) + 1;
    let target = u16::from((versions >> 4) & 3) + 1;
    // The ProUpReg leg needs the oracle process's rules: regtest, every network reachable.
    let leg = usize::from(*b0 & 3) == REGTEST && *b0 & 16 != 0;
    for net_info in [&before, &after] {
        let reject = migration_reject(net_info, cur, target, s.mn_type, &s.rules);
        ensure(
            migration_reject(net_info, cur, target, s.mn_type, &s.rules) == reject,
            "migration determinism",
        )?;
        if leg {
            up_reg_migration(net_info, cur, target, s.mn_type, &s.rules)?;
        }
    }
    Ok(())
}

/// Regtest's place in [`NetworkKind::ALL`], as byte 0 picks the network.
const REGTEST: usize = 3;

/// D29's ProUpReg leg: `net_info` in the state of a masternode at `cur`, raised by a ProUpRegTx
/// at `target` that keeps its operator key (`node_l1::specialtx::migration_case`): Core's
/// `CheckSpecialTx` and ours agree, and the verdict is `migration_reject`'s, its reason or
/// acceptance (every other check passes). Returns whether Core accepted, `None` when no list holds
/// that state.
fn up_reg_migration(
    net_info: &NetInfo,
    cur: u16,
    target: u16,
    mn_type: MnType,
    rules: &NetInfoRules,
) -> Result<Option<bool>, Divergence> {
    let Some(case) = migration_case(mn_type == MnType::Evo, cur, net_info.clone(), target) else {
        return Ok(None);
    };
    let Some(v) = CORE.with(|core| verdicts(&core.oracle, &case))? else {
        return Ok(None);
    };
    ensure(v.core == v.ours, "ProUpReg migration: ours vs Core")?;
    let expected = migration_reject(net_info, cur, target, mn_type, rules)
        .map(|r| (TxValidationResult::BadSpecial as i32, r.to_owned()));
    ensure(v.core == expected, "ProUpReg migration: migration_reject")?;
    Ok(Some(v.core.is_none()))
}

/// `netinfo_add_entry`'s start: Core's oracle and the ProUpReg leg's world, built once, then one
/// migration each way (D28n, as [`validate_start`]).
pub(super) fn add_entry_start() {
    for (i, check) in add_entry_start_checks().into_iter().enumerate() {
        assert!(matches!(check, Ok(Some(_))), "netinfo_add_entry start: check {i}: {check:?}");
    }
}

/// The start's checks: a regtest address at state version 2 raised to 3 (accepted) and the same
/// address on port 22, which ExtNetInfo refuses (`bad-protx-netinfo-version`).
fn add_entry_start_checks() -> Vec<Result<Option<bool>, Divergence>> {
    let config = NetInfoConfig {
        cjdns_reachable: true,
    };
    let rules = NetInfoRules::new(&params()[REGTEST], &params()[0], config);
    [19_899, 22]
        .map(|port| {
            let mut addr = [0u8; 16];
            addr[10..].copy_from_slice(&[0xff, 0xff, 1, 2, 3, 200]);
            let net_info = NetInfo::Legacy(NetInfoV1(ServiceV1 {
                addr: AddrV1(addr),
                port,
            }));
            up_reg_migration(&net_info, 2, 3, MnType::Regular, &rules)
        })
        .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a failed read of a seed is the failure
mod tests {
    use super::*;

    fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
        let dir = format!("{}/seeds/{target}", env!("CARGO_MANIFEST_DIR"));
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect();
        out.sort();
        assert!(!out.is_empty());
        out
    }

    /// Every seed reaches the checks (its object decodes) and holds; `main-ext-unordered` is the
    /// refusal rc.4's purpose rule makes at decode (CE4-01).
    #[test]
    fn seeds_decode_and_hold() {
        for (name, data) in seeds("netinfo_validate") {
            let decodes = decode(data[0] & 4 != 0, &data[6..]).is_some();
            assert_eq!(decodes, name != "main-ext-unordered", "{name}");
            assert_eq!(validate_check(&data), Ok(()), "{name}");
        }
        for (name, data) in seeds("netinfo_add_entry") {
            let wire = &data[4..4 + usize::from(data[3])];
            assert!(decode(data[0] & 4 != 0, wire).is_some(), "{name}");
            assert_eq!(add_entry_check(&data), Ok(()), "{name}");
        }
    }

    /// The transaction verdict (D29) on every seed's object in every template on every network,
    /// with the platform fields as given: each template, on each network, sees inputs ours
    /// rejects and inputs ours passes, and Core agrees on all of them. The exceptions pass
    /// nothing: a v3 template on rc.4's mainnet, where V24 cannot be active, and the v1
    /// registration where it can (`bad-protx-version-disallowed`).
    #[test]
    fn tx_verdicts_reach_every_template() {
        let mut tally = std::collections::BTreeMap::<(u8, usize), (usize, usize)>::new();
        for (name, data) in seeds("netinfo_validate") {
            for network in 0..4u8 {
                for (at, (registration, evo, version, _)) in TEMPLATES.iter().enumerate() {
                    for kind in [0u8, 4] {
                        let b0 = (data[0] & 0x80)
                            | kind
                            | network
                            | u8::from(*evo) << 5
                            | u8::from(*registration) << 6;
                        let Some(net_info) = decode(kind != 0, &data[6..]) else {
                            continue;
                        };
                        let ports = [data[2], data[3], data[4], data[5]];
                        let b1 = (data[1] & 0x80) | *version as u8;
                        let reached = tx_verdict(b0, b1, ports, net_info).unwrap_or_else(|e| {
                            panic!("{name} network {network} template {at}: {e:?}")
                        });
                        let entry = tally.entry((network, at)).or_default();
                        match reached {
                            Some(true) => entry.0 += 1,
                            Some(false) => entry.1 += 1,
                            None => {}
                        }
                    }
                }
            }
        }
        for network in 0..4u8 {
            for (at, (registration, _, version, _)) in TEMPLATES.iter().enumerate() {
                let (passed, rejected) = tally.get(&(network, at)).copied().unwrap_or_default();
                let mainnet = network == 0;
                let passes = if mainnet {
                    *version < 3
                } else {
                    !(*registration && *version < 2)
                };
                assert!(rejected > 0, "network {network} template {at}: nothing rejected");
                assert_eq!(passed > 0, passes, "network {network} template {at}: {passed} passed");
            }
        }
    }

    /// The start reaches Core with every template on every network, so none of Core's
    /// first-call set-up is left to the first input.
    #[test]
    fn start_checks_every_template_on_every_network() {
        let checks = start_checks();
        assert_eq!(checks.len(), 4 * TEMPLATES.len());
        for (i, check) in checks.into_iter().enumerate() {
            assert!(matches!(check, Ok(Some(_))), "check {i}: {check:?}");
        }
        validate_start();
    }

    /// D29's ProUpReg leg on every seed's object, with byte 0 on regtest with every network
    /// reachable and every pair of versions: the check holds, and the leg sees Core accept the
    /// migration and refuse it.
    #[test]
    fn migration_leg_reaches_both_verdicts() {
        let (mut accepted, mut refused) = (0, 0);
        for (name, mut data) in seeds("netinfo_add_entry") {
            data[0] = (data[0] & !3) | REGTEST as u8 | 16;
            let s = setup(data[0]);
            let wire = &data[4..4 + usize::from(data[3])];
            let net_info = decode(s.extended, wire).unwrap();
            for versions in 0..16u8 {
                data[2] = (versions & 3) | (versions >> 2) << 4;
                assert_eq!(add_entry_check(&data), Ok(()), "{name} {versions:#x}");
                let (cur, target) = (u16::from(versions & 3) + 1, u16::from(versions >> 2) + 1);
                match up_reg_migration(&net_info, cur, target, s.mn_type, &s.rules) {
                    Ok(Some(true)) => accepted += 1,
                    Ok(Some(false)) => refused += 1,
                    Ok(None) => {}
                    Err(e) => panic!("{name} {versions:#x}: {e:?}"),
                }
            }
        }
        assert!(accepted > 0 && refused > 0, "{accepted} accepted, {refused} refused");
    }

    /// The start reaches Core with a migration it accepts and one it refuses.
    #[test]
    fn add_entry_start_migrates_each_way() {
        assert_eq!(add_entry_start_checks(), [Ok(Some(true)), Ok(Some(false))]);
        add_entry_start();
    }

    /// Byte flips of the seeds (a fixed xorshift), so a plain `cargo test` covers the
    /// neighbourhood the fuzzer starts from.
    #[test]
    fn mutated_seeds_hold() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for (target, check) in [
            ("netinfo_validate", validate_check as fn(&[u8]) -> Result<(), Divergence>),
            ("netinfo_add_entry", add_entry_check),
        ] {
            for (name, seed) in seeds(target) {
                for _ in 0..2000 {
                    let mut data = seed.clone();
                    for _ in 0..1 + next() % 4 {
                        let at = (next() % data.len() as u64) as usize;
                        data[at] = next() as u8;
                    }
                    assert_eq!(check(&data), Ok(()), "{target}/{name}: {data:02x?}");
                }
            }
        }
    }
}

//! `netinfo_validate` and `netinfo_add_entry` (EV1 §6.5, D29): `dash_dml::netinfo` on arbitrary
//! network info and inputs, on every network. Until the NetInfo oracle entries (A-3) back an L1
//! comparison, the property is the narrowed one: each function is total (a panic is libFuzzer's
//! crash) and deterministic, and the results agree with each other the way Core's do:
//! - `netinfo_validate`: `Validate()`, `CoreNetInfoValidity`, `CheckNetInfo` and
//!   `CheckProviderNetworkFields`;
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

use dash_dml::MnType;
use dash_dml::codec::NetInfoValidity;
use dash_dml::netinfo::{
    CoreNetInfoValidity, NetInfoConfig, NetInfoRules, NetInfoStatus, add_entry_bytes, has_entries,
    is_empty, migration_reject, validate,
};
use dash_dml::specialtx::{ProviderNetworkFields, check_net_info, check_provider_network_fields};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{NIPurpose, NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use node_l1::Divergence;
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
    )
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
    for net_info in [&before, &after] {
        let reject = migration_reject(net_info, cur, target, s.mn_type, &s.rules);
        ensure(
            migration_reject(net_info, cur, target, s.mn_type, &s.rules) == reject,
            "migration determinism",
        )?;
    }
    Ok(())
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

    /// Every seed reaches the checks (its object decodes) and holds.
    #[test]
    fn seeds_decode_and_hold() {
        for (name, data) in seeds("netinfo_validate") {
            assert!(decode(data[0] & 4 != 0, &data[6..]).is_some(), "{name}");
            assert_eq!(validate_check(&data), Ok(()), "{name}");
        }
        for (name, data) in seeds("netinfo_add_entry") {
            let wire = &data[4..4 + usize::from(data[3])];
            assert!(decode(data[0] & 4 != 0, wire).is_some(), "{name}");
            assert_eq!(add_entry_check(&data), Ok(()), "{name}");
        }
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

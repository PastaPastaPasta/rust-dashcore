//! The `coreimport_*` targets of IM1b: the `CFlatDB` files (`governance.dat`, `mncache.dat`,
//! `netfulfilled.dat`, `sporks.dat`), `dash.conf` and the onion and I2P key files of
//! `dash_coreimport`, on arbitrary bytes, under the runner. A reader is a total function: a
//! panic, an abort or an allocation beyond the process limit is libFuzzer's crash. These checks
//! add what a crash does not show: determinism, results within the bounds the input gives,
//! detection agreeing with the reader, and the key files' secrecy and canonical form.
//!
//! A `CFlatDB` file ends in a SHA256d that a mutation almost never keeps valid, so each check
//! also runs the reader on the input with the checksum repaired (and the regtest magic put in
//! when the magic message is intact), which reaches the object parse. The readers are the same
//! under every release; none decodes a transaction. `governance.dat` embeds a masternode list,
//! decoded by the code the private-route `dml_codec_roundtrip` target runs, so its target is
//! private-route too; the others are public-route.

use bitcoin_hashes::{Hash, sha256d};
use dash_coreimport::config::parse_dash_conf;
use dash_coreimport::keys::{I2pPrivateKey, OnionV3Key};
use dash_coreimport::netstate::{
    Version, detect_named, governance, mncache, netfulfilled, sniff, sporks,
};
use dash_network::Network;
use node_l1::Divergence;

type Check = Result<(), &'static str>;

fn divergence(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

/// The input as it is, and with the checksum recomputed over everything before it (and, when the
/// magic message is intact, the regtest network magic put in), so a mutation reaches the object.
fn flat_variants(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![data.to_vec()];
    if data.len() >= 32 + 2 {
        let mut fixed = data[..data.len() - 32].to_vec();
        let message_len = usize::from(fixed[0]);
        if message_len < 253 && fixed.len() >= 1 + message_len + 4 {
            let at = 1 + message_len;
            fixed[at..at + 4].copy_from_slice(&Network::Regtest.magic().to_le_bytes());
        }
        let sum = sha256d::Hash::hash(&fixed).to_byte_array();
        fixed.extend_from_slice(&sum);
        out.push(fixed);
    }
    out
}

/// The network the bytes' own magic names (regtest when it names none).
fn flat_network(data: &[u8]) -> Network {
    let message_len = usize::from(data.first().copied().unwrap_or(0));
    data.get(1 + message_len..1 + message_len + 4)
        .and_then(|m| <[u8; 4]>::try_from(m).ok())
        .and_then(|m| Network::from_magic(u32::from_le_bytes(m)))
        .unwrap_or(Network::Regtest)
}

/// `sniff` never claims a format whose name does not then detect identically.
fn sniff_agrees(data: &[u8]) -> Check {
    if let Ok(found) = sniff(data)
        && detect_named(found.format.file_name(), data) != Ok(found)
    {
        return Err("sniff and detect_named");
    }
    Ok(())
}

/// What a parsed flat file must agree with `detect_named` on.
fn detects(name: &str, data: &[u8], network: Network, current: bool) -> Check {
    match detect_named(name, data) {
        Ok(d)
            if d.network == Some(network)
                && d.version
                    == (Version::Flat {
                        current,
                    }) =>
        {
            Ok(())
        }
        _ => Err("flat file detection"),
    }
}

fn governance_one(data: &[u8]) -> Check {
    let network = flat_network(data);
    let Ok(l) = governance::parse(data, network) else {
        return Ok(());
    };
    if governance::parse(data, network).as_ref() != Ok(&l) {
        return Err("governance.dat determinism");
    }
    let g = &l.value;
    // Every item takes at least one byte of the input.
    let items = g.erased.len()
        + g.orphan_votes.items.len()
        + g.objects.len()
        + g.last_masternode_object.len()
        + g.mn_list.len();
    if items > data.len() || l.trailing_bytes + 32 > data.len() {
        return Err("governance.dat counts");
    }
    if g.objects.values().any(|o| o.vote_file.votes.len() > data.len()) {
        return Err("governance.dat vote counts");
    }
    // A discarded object is Core's `Clear()`ed one: every container empty, the list at its default.
    if l.discarded.is_some()
        && !(g.objects.is_empty()
            && g.erased.is_empty()
            && g.orphan_votes.items.is_empty()
            && g.last_masternode_object.is_empty()
            && g.mn_list.is_default())
    {
        return Err("governance.dat discarded object not cleared");
    }
    if l.discarded.is_some() {
        return Ok(());
    }
    // A store is of a version the reader loads exactly when it says so.
    if g.version_matched != g.version.is_some() {
        return Err("governance.dat version");
    }
    detects("governance.dat", data, network, g.version_matched)
}

fn mncache_one(data: &[u8]) -> Check {
    let network = flat_network(data);
    let Ok(l) = mncache::parse(data, network) else {
        return Ok(());
    };
    if mncache::parse(data, network).as_ref() != Ok(&l) {
        return Err("mncache.dat determinism");
    }
    let m = &l.value;
    if m.infos.len() + m.used_masternodes.len() > data.len() || l.trailing_bytes + 32 > data.len() {
        return Err("mncache.dat counts");
    }
    if l.discarded.is_some() && (!m.infos.is_empty() || !m.used_masternodes.is_empty()) {
        return Err("mncache.dat discarded object not cleared");
    }
    if m.infos.iter().any(|(k, v)| *k != v.protx_hash) {
        return Err("mncache.dat key");
    }
    if l.discarded.is_some() {
        return Ok(());
    }
    detects("mncache.dat", data, network, m.version_matched)
}

fn netfulfilled_one(data: &[u8]) -> Check {
    let network = flat_network(data);
    let Ok(l) = netfulfilled::parse(data, network) else {
        return Ok(());
    };
    if netfulfilled::parse(data, network).as_ref() != Ok(&l) {
        return Err("netfulfilled.dat determinism");
    }
    let n = &l.value;
    if n.requests.len() > data.len() || l.trailing_bytes + 32 > data.len() {
        return Err("netfulfilled.dat counts");
    }
    // `CheckAndRemove` only removes: what it leaves is a subset, with no empty service.
    let kept = n.without_expired(0);
    if kept.requests.values().any(|e| e.is_empty()) || kept.requests.len() > n.requests.len() {
        return Err("netfulfilled.dat without_expired");
    }
    if l.discarded.is_some() {
        return if n.requests.is_empty() {
            Ok(())
        } else {
            Err("netfulfilled.dat not cleared")
        };
    }
    detects("netfulfilled.dat", data, network, true)
}

fn sporks_one(data: &[u8]) -> Check {
    let network = flat_network(data);
    let Ok(l) = sporks::parse(data, network) else {
        return Ok(());
    };
    if sporks::parse(data, network).as_ref() != Ok(&l) {
        return Err("sporks.dat determinism");
    }
    let s = &l.value;
    if s.by_hash.len() + s.active.len() > data.len() || l.trailing_bytes + 32 > data.len() {
        return Err("sporks.dat counts");
    }
    // Signature checks run on whatever the file holds: no panic, deterministic.
    for message in s.by_hash.values().chain(s.active.values()) {
        if message.signature.len() as u64 > sporks::MAX_SIGNATURE_SIZE {
            return Err("sporks.dat signature length");
        }
        for mode in [false, true] {
            let a = message.check_signature(&[7; 20], mode);
            if a != message.check_signature(&[7; 20], mode) {
                return Err("sporks.dat signature check determinism");
            }
        }
    }
    if l.discarded.is_some() {
        return if s.by_hash.is_empty() && s.active.is_empty() {
            Ok(())
        } else {
            Err("sporks.dat not cleared")
        };
    }
    detects("sporks.dat", data, network, s.version_matched)
}

fn flat_check(data: &[u8], one: fn(&[u8]) -> Check) -> Result<(), Divergence> {
    sniff_agrees(data).map_err(divergence)?;
    flat_variants(data).iter().try_for_each(|d| one(d)).map_err(divergence)
}

pub(super) fn governance_dat(data: &[u8]) -> Result<(), Divergence> {
    flat_check(data, governance_one)
}

pub(super) fn mncache_dat(data: &[u8]) -> Result<(), Divergence> {
    flat_check(data, mncache_one)
}

pub(super) fn netfulfilled_dat(data: &[u8]) -> Result<(), Divergence> {
    flat_check(data, netfulfilled_one)
}

pub(super) fn sporks_dat(data: &[u8]) -> Result<(), Divergence> {
    flat_check(data, sporks_one)
}

fn dash_conf_one(data: &[u8], network: Network, ignore: bool) -> Check {
    let Ok(conf) = parse_dash_conf(data, network, ignore) else {
        return Ok(());
    };
    // The same text reads the same.
    let again = parse_dash_conf(data, network, ignore).map_err(|_| "dash.conf determinism")?;
    if again.sections() != conf.sections() || again.includes != conf.includes {
        return Err("dash.conf determinism");
    }
    let sections = conf.sections();
    // Every listed include is a value of `includeconf` in some section.
    for value in &conf.includes {
        let listed = sections.values().filter_map(|s| s.get("includeconf")).any(|values| {
            values
                .iter()
                .any(|v| matches!(v, dash_coreimport::config::SettingsValue::Str(p) if p == value))
        });
        if !listed {
            return Err("dash.conf includes");
        }
    }
    // The typed getters do not panic on whatever the file set.
    for name in ["datadir", "port", "rpcport", "dbcache", "txindex", "includeconf", "externalip"] {
        let _ = conf.args().get_arg_opt(name);
        let _ = conf.args().get_int_arg_opt(name);
        let _ = conf.args().get_bool_arg_opt(name);
        let _ = conf.args().get_args(name);
    }
    Ok(())
}

pub(super) fn dash_conf(data: &[u8]) -> Result<(), Divergence> {
    for network in [Network::Mainnet, Network::Testnet] {
        for ignore in [false, true] {
            dash_conf_one(data, network, ignore).map_err(divergence)?;
        }
    }
    Ok(())
}

/// A key file: parse is deterministic; a key that parses re-serializes to the same bytes and
/// shows nothing of itself in `Debug`.
pub(super) fn onion_key(data: &[u8]) -> Result<(), Divergence> {
    let Ok(parsed) = OnionV3Key::parse(data) else {
        return Ok(());
    };
    let Some(key) = parsed else {
        return if data.is_empty() {
            Ok(())
        } else {
            Err(divergence("onion key: empty"))
        };
    };
    if key.to_file_bytes().as_bytes() != data {
        return Err(divergence("onion key: not canonical"));
    }
    let shown = format!("{key:?} {:?}", key.to_file_bytes());
    if data.len() != dash_coreimport::keys::ONION_FILE_SIZE
        || key.expanded_secret().len() != 64
        || shown.contains(&String::from_utf8_lossy(&data[11..]).into_owned())
    {
        return Err(divergence("onion key: shape or Debug"));
    }
    Ok(())
}

pub(super) fn i2p_key(data: &[u8]) -> Result<(), Divergence> {
    let Ok(key) = I2pPrivateKey::parse(data) else {
        return Ok(());
    };
    if key.as_bytes() != data || key.destination().len() > data.len() || data.len() < 387 {
        return Err(divergence("i2p key: bytes"));
    }
    let cert = usize::from(u16::from_be_bytes([data[385], data[386]]));
    if key.destination().len() != 387 + cert {
        return Err(divergence("i2p key: destination"));
    }
    let b32 = key.b32_address();
    if b32.len() != 52 + ".b32.i2p".len() || !b32.ends_with(".b32.i2p") {
        return Err(divergence("i2p key: address"));
    }
    if format!("{key:?}").len() > 40 {
        return Err(divergence("i2p key: Debug"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat file whose object is empty is accepted by every model.
    #[test]
    fn variants_repair_the_checksum() {
        let mut data = vec![15u8];
        data.extend_from_slice(b"magicSporkCache");
        data.extend_from_slice(&Network::Regtest.magic().to_le_bytes());
        data.push(23);
        data.extend_from_slice(b"CSporkManager-Version-3");
        data.extend_from_slice(&[0, 0]);
        let sum = sha256d::Hash::hash(&data).to_byte_array();
        data.extend_from_slice(&sum);
        assert!(sporks_dat(&data).is_ok());
        let mut broken = data.clone();
        if let Some(last) = broken.last_mut() {
            *last ^= 1;
        }
        let variants = flat_variants(&broken);
        assert_eq!(variants.len(), 2);
        assert!(sporks::parse(&variants[0], flat_network(&variants[0])).is_err());
        assert!(sporks::parse(&variants[1], flat_network(&variants[1])).is_ok());
    }
}

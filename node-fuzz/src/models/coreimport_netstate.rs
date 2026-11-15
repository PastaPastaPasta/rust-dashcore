//! The `coreimport_*` net-state reader targets (IM1a): `peers.dat`, `anchors.dat`,
//! `banlist.json`, `mempool.dat`, `fee_estimates.dat` and `settings.json` of
//! `dash_coreimport::netstate` on arbitrary bytes, under the runner. A reader is a total function:
//! a panic, an abort or an allocation beyond the process limit is libFuzzer's crash. These checks
//! add what a crash does not show: each reader is deterministic, what it returns is within the
//! bounds its input and Core's limits give, and detection agrees with the reader.
//!
//! The two `addrdb` envelopes (`peers.dat`, `anchors.dat`) end in a SHA256d that a mutation
//! almost never keeps valid, so those checks also run the reader on the input with the network
//! magic and the checksum repaired, which reaches the parse results.
//!
//! `mempool.dat` decodes transactions: a private-route target (PLAN §7.5). The readers are the
//! same under every release.

use bitcoin_hashes::{Hash, sha256d};
use dash_coreimport::netstate::mempool::{Outcome, Phase};
use dash_coreimport::netstate::subnet::{self, Subnet};
use dash_coreimport::netstate::{
    Value, Version, anchors, banlist, detect_named, fee_estimates, mempool, peers, settings, sniff,
};
use dash_network::Network;
use node_l1::Divergence;

type Check = Result<(), &'static str>;

/// Every mutation of an enveloped file is a candidate for a parse, and so is the same bytes with
/// the regtest magic and a valid checksum over everything before it.
fn envelope_variants(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![data.to_vec()];
    if data.len() >= 4 + 32 {
        let mut fixed = data[..data.len() - 32].to_vec();
        fixed[..4].copy_from_slice(&Network::Regtest.magic().to_le_bytes());
        let sum = sha256d::Hash::hash(&fixed).to_byte_array();
        fixed.extend_from_slice(&sum);
        out.push(fixed);
    }
    out
}

/// The network the bytes' own magic names (regtest when it names none: the reader then reports
/// the magic).
fn network_of(data: &[u8]) -> Network {
    data.first_chunk::<4>()
        .and_then(|m| Network::from_magic(u32::from_le_bytes(*m)))
        .unwrap_or(Network::Regtest)
}

/// What every input must satisfy whatever the format: `sniff` never claims a format whose name
/// does not then detect identically.
fn sniff_agrees(data: &[u8]) -> Check {
    if let Ok(found) = sniff(data)
        && detect_named(found.format.file_name(), data) != Ok(found)
    {
        return Err("sniff and detect_named");
    }
    Ok(())
}

fn peers_one(data: &[u8]) -> Check {
    let network = network_of(data);
    let Ok(p) = peers::parse(data, network) else {
        return Ok(());
    };
    if peers::parse(data, network).as_ref() != Ok(&p) {
        return Err("peers.dat determinism");
    }
    if p.new.len() > (peers::NEW_BUCKET_COUNT * peers::BUCKET_SIZE) as usize
        || p.tried.len() > (peers::TRIED_BUCKET_COUNT * peers::BUCKET_SIZE) as usize
    {
        return Err("peers.dat entry counts");
    }
    if p.bucket_refs.iter().any(|r| r.index as usize >= p.new.len()) {
        return Err("peers.dat bucket reference");
    }
    if (p.bucket_refs.len() as u64).saturating_add(p.ignored_bucket_refs) > data.len() as u64 / 4 {
        return Err("peers.dat bucket reference count");
    }
    if p.lowest_compatible > peers::FILE_FORMAT || p.trailing_bytes + 32 > data.len() {
        return Err("peers.dat header or trailing bytes");
    }
    let want = Version::Peers {
        format: p.format,
        lowest_compatible: p.lowest_compatible,
    };
    match detect_named("peers.dat", data) {
        Ok(d) if d.version == want && d.network == Some(p.network) => Ok(()),
        _ => Err("peers.dat detection"),
    }
}

pub(super) fn peers_dat(data: &[u8]) -> Result<(), Divergence> {
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    envelope_variants(data).iter().try_for_each(|d| peers_one(d)).map_err(|field| Divergence {
        field,
    })
}

fn anchors_one(data: &[u8]) -> Check {
    let network = network_of(data);
    let Ok(a) = anchors::parse(data, network) else {
        return Ok(());
    };
    if anchors::parse(data, network).as_ref() != Ok(&a) {
        return Err("anchors.dat determinism");
    }
    if a.addresses.len() > data.len() / 13 || a.trailing_bytes + 32 > data.len() {
        return Err("anchors.dat counts");
    }
    let first = a.addresses.first().map(|x| x.v2);
    match detect_named("anchors.dat", data) {
        Ok(d)
            if d.version
                == (Version::Anchors {
                    first_addrv2: first,
                }) =>
        {
            Ok(())
        }
        _ => Err("anchors.dat detection"),
    }
}

pub(super) fn anchors_dat(data: &[u8]) -> Result<(), Divergence> {
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    envelope_variants(data).iter().try_for_each(|d| anchors_one(d)).map_err(|field| Divergence {
        field,
    })
}

pub(super) fn banlist_json(data: &[u8]) -> Result<(), Divergence> {
    let fail = |field| {
        Err(Divergence {
            field,
        })
    };
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    // `-cjdnsreachable` is a node option, not part of the file: both readings must hold.
    for cjdns in [false, true] {
        let Ok(list) = banlist::parse_with(data, cjdns) else {
            continue;
        };
        if banlist::parse_with(data, cjdns).as_ref() != Ok(&list) {
            return fail("banlist.json determinism");
        }
        // Every entry that is kept (or replaced a kept one) takes at least 59 bytes
        // (`{"version":1,"address":"","ban_created":0,"banned_until":0}`), one dropped for its
        // version at least 13 (`{"version":2}`), one dropped for its subnet at least 26
        // (`{"version":1,"address":""}`).
        let least = (list.entries.len() as u64 + list.replaced_duplicates) * 59
            + list.dropped_unknown_version * 13
            + list.dropped_invalid_subnet * 26;
        if least > data.len() as u64 {
            return fail("banlist.json entry count");
        }
        // `banmap_t` is a `std::map`: strictly ascending in `CSubNet::operator<`, written out
        // here (class, address bytes, netmask bytes) rather than through `Subnet`'s own `Ord`.
        let key = |s: &Subnet| -> (u8, Vec<u8>, Vec<u8>) {
            match s {
                Subnet::V4 {
                    network,
                    mask,
                } => (1, network.to_vec(), mask.to_vec()),
                Subnet::V6 {
                    network,
                    mask,
                } => (2, network.to_vec(), mask.to_vec()),
                Subnet::Onion(a) => (3, a.to_vec(), Vec::new()),
                Subnet::I2p(a) => (4, a.to_vec(), Vec::new()),
                Subnet::Cjdns(a) => (5, a.to_vec(), Vec::new()),
            }
        };
        if !list.entries.windows(2).all(|w| key(&w[0].subnet) < key(&w[1].subnet)) {
            return fail("banlist.json order");
        }
        for entry in &list.entries {
            // The address text reads back as the subnet it was kept for (self-consistency of
            // `lookup_subnet`, not agreement with Core: the integration tests judge that), and a
            // network is masked and a mask is one run of 1-bits.
            if subnet::lookup_subnet(entry.address.as_bytes(), cjdns) != Some(entry.subnet)
                || entry.address.as_bytes().len() > data.len()
            {
                return fail("banlist.json subnet");
            }
            let (network, mask): (&[u8], &[u8]) = match &entry.subnet {
                Subnet::V4 {
                    network,
                    mask,
                } => (network, mask),
                Subnet::V6 {
                    network,
                    mask,
                } => (network, mask),
                _ => (&[], &[]),
            };
            let masked = network.iter().zip(mask).all(|(n, m)| n & m == *n);
            let bits: Vec<bool> =
                mask.iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
            if !masked || !bits.is_sorted_by(|a, b| a >= b) {
                return fail("banlist.json subnet");
            }
        }
    }
    Ok(())
}

/// Counts the nodes of a value and checks that a number keeps its text. (The first-member rule of
/// `UniValue::operator[]` is judged by the integration tests, not here: restating `Object::get`
/// would only compare it with itself.)
fn walk(value: &Value, nodes: &mut usize) -> Check {
    *nodes += 1;
    match value {
        Value::Arr(items) => items.iter().try_for_each(|v| walk(v, nodes)),
        Value::Obj(o) => o.entries().iter().try_for_each(|(_, v)| walk(v, nodes)),
        Value::Num(text) if text.is_empty() => Err("settings.json empty number"),
        _ => Ok(()),
    }
}

pub(super) fn settings_json(data: &[u8]) -> Result<(), Divergence> {
    let fail = |field| {
        Err(Divergence {
            field,
        })
    };
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    let Ok(s) = settings::parse(data) else {
        return Ok(());
    };
    if settings::parse(data).as_ref() != Ok(&s) {
        return fail("settings.json determinism");
    }
    if s.empty_file != data.is_empty() || s.values.len() > data.len() / 5 {
        return fail("settings.json size");
    }
    // Only the top level has unique keys; nested objects keep repeats.
    let mut keys: Vec<_> = s.values.iter().map(|(k, _)| k.as_bytes()).collect();
    keys.sort_unstable();
    if keys.windows(2).any(|w| w[0] == w[1]) {
        return fail("settings.json duplicate key");
    }
    // Every node takes a byte or more of the input.
    let mut nodes = 0;
    for (_, v) in &s.values {
        walk(v, &mut nodes).map_err(|field| Divergence {
            field,
        })?;
    }
    if nodes > data.len() {
        return fail("settings.json node count");
    }
    Ok(())
}

pub(super) fn mempool_dat(data: &[u8]) -> Result<(), Divergence> {
    let fail = |field| {
        Err(Divergence {
            field,
        })
    };
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    let Ok(loaded) = mempool::load(data) else {
        return Ok(());
    };
    if mempool::load(data).as_ref() != Ok(&loaded) {
        return fail("mempool.dat determinism");
    }
    let strict = mempool::parse(data);
    if strict.is_ok() != matches!(loaded.outcome, Outcome::Complete) {
        return fail("mempool.dat strict and load");
    }
    let m = &loaded.dump;
    let range = data.as_ptr_range();
    let mut used = 16 + m.trailing_bytes;
    for tx in &m.txs {
        // Each transaction is a slice of the input and at least 10 bytes.
        let raw = tx.raw.as_ptr_range();
        if raw.start < range.start || raw.end > range.end || tx.raw.len() < 10 {
            return fail("mempool.dat transaction slice");
        }
        used += tx.raw.len() + 16;
    }
    // A stage that failed leaves the later containers empty; a repeated key takes bytes without
    // an entry, so the sums are lower bounds.
    match &loaded.outcome {
        Outcome::Complete => used += 2,
        Outcome::Stopped {
            phase: Phase::Unbroadcast,
            ..
        } => {
            if !m.unbroadcast.is_empty() {
                return fail("mempool.dat stage");
            }
            used += 1;
        }
        Outcome::Stopped {
            ..
        } => {
            if !m.deltas.is_empty() || !m.unbroadcast.is_empty() {
                return fail("mempool.dat stage");
            }
        }
    }
    used += m.deltas.len() * 40 + m.unbroadcast.len() * 32;
    if used > data.len() {
        return fail("mempool.dat accounting");
    }
    if strict.is_err() {
        return Ok(());
    }
    match detect_named("mempool.dat", data) {
        Ok(d)
            if d.version
                == (Version::Mempool {
                    version: 1,
                }) =>
        {
            Ok(())
        }
        _ => fail("mempool.dat detection"),
    }
}

pub(super) fn fee_estimates_dat(data: &[u8]) -> Result<(), Divergence> {
    let fail = |field| {
        Err(Divergence {
            field,
        })
    };
    sniff_agrees(data).map_err(|field| Divergence {
        field,
    })?;
    let Ok(f) = fee_estimates::parse(data) else {
        return Ok(());
    };
    // The statistics are `f64`s read from the file and may be NaN, which `PartialEq` never
    // equates with itself, so compare the rendered values.
    match fee_estimates::parse(data) {
        Ok(again) if format!("{again:?}") == format!("{f:?}") => {}
        _ => return fail("fee_estimates.dat determinism"),
    }
    if !(2..=1000).contains(&f.buckets.len())
        || f.historical_first > f.historical_best
        || f.historical_best > f.best_seen_height
    {
        return fail("fee_estimates.dat header");
    }
    for s in [&f.medium, &f.short, &f.long] {
        let rows = s.conf_avg.len() as u64;
        // `TxConfirmStats::Read`'s own test: `decay <= 0 || decay >= 1` throws and nothing else
        // does, so a NaN passes (both comparisons are false) and Core writes it back (R2-M1).
        if s.decay <= 0.0
            || s.decay >= 1.0
            || s.scale == 0
            || rows == 0
            || u64::from(s.scale) * rows > 1008
            || s.fail_avg.len() as u64 != rows
            || s.feerate_avg.len() != f.buckets.len()
            || s.tx_ct_avg.len() != f.buckets.len()
            || s.conf_avg.iter().chain(&s.fail_avg).any(|r| r.len() != f.buckets.len())
        {
            return fail("fee_estimates.dat stats shape");
        }
    }
    match detect_named("fee_estimates.dat", data) {
        Ok(d)
            if d.version
                == (Version::FeeEstimates {
                    required: f.version_required,
                    wrote: f.version_that_wrote,
                }) =>
        {
            Ok(())
        }
        _ => fail("fee_estimates.dat detection"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a failed read of a fixture is the failure
mod tests {
    use super::*;

    const DIR: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-coreimport/tests/data/netstate/v24.0.0-rc.3");

    fn file(dir: &str) -> Vec<u8> {
        std::fs::read(format!("{DIR}/{dir}/fee_estimates.dat")).unwrap()
    }

    fn model(data: &[u8]) -> Result<(), Divergence> {
        fee_estimates_dat(data)
    }

    /// The offset of each horizon's decay word (medium, short, long): the first occurrence of
    /// its value, which the test checks is the only one.
    fn decay_offsets(data: &[u8]) -> Vec<usize> {
        let f = fee_estimates::parse(data).unwrap();
        [f.medium.decay, f.short.decay, f.long.decay]
            .iter()
            .map(|d| {
                let word = d.to_le_bytes();
                let at: Vec<_> = data.windows(8).enumerate().filter(|(_, w)| **w == word).collect();
                assert_eq!(at.len(), 1, "decay {d} must occur once");
                at[0].0
            })
            .collect()
    }

    /// R2-M1 (Sol r2): Core accepts a NaN decay (`decay <= 0 || decay >= 1` is false for NaN),
    /// loads the file and writes it back, so the reader accepts it and the model must not call
    /// that a divergence. `nan-decay/` is the file a real Core v24.0.0-rc.3 wrote after loading
    /// it (PROVENANCE.md).
    #[test]
    fn a_nan_decay_that_core_accepts_is_not_a_divergence() {
        let core_written = file("nan-decay");
        let f = fee_estimates::parse(&core_written).unwrap();
        assert!(f.medium.decay.is_nan() && f.short.decay.is_nan() && f.long.decay.is_nan());
        assert_eq!(model(&core_written), Ok(()));
    }

    /// The same for each horizon on its own, starting from the ordinary Core file, and for the
    /// other non-finite and out-of-range words: the reader refuses what Core refuses, and the model
    /// holds on both sides.
    #[test]
    fn every_horizon_and_every_decay_word_agrees_with_core() {
        let ordinary = file("populated");
        assert_eq!(model(&ordinary), Ok(()));
        for (horizon, at) in decay_offsets(&ordinary).into_iter().enumerate() {
            for (word, core_accepts) in [
                (f64::NAN, true),
                (f64::from_bits(0xfff8_0000_0000_0001), true),
                (0.5, true),
                (0.0, false),
                (-0.0, false),
                (1.0, false),
                (-0.5, false),
                (2.0, false),
                (f64::INFINITY, false),
                (f64::NEG_INFINITY, false),
            ] {
                let mut data = ordinary.clone();
                data[at..at + 8].copy_from_slice(&word.to_le_bytes());
                assert_eq!(
                    fee_estimates::parse(&data).is_ok(),
                    core_accepts,
                    "horizon {horizon}, decay {word}"
                );
                assert_eq!(model(&data), Ok(()), "horizon {horizon}, decay {word}");
            }
        }
    }
}

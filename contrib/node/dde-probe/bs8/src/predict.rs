//! base-sdk's read of a case, with the node's BLS rules applied: the
//! prediction of Core's read that the judge checks.
//!
//! base-sdk keeps BLS fields as wire bytes (Q-12). Core reads some eagerly:
//! under its process-wide scheme (clsig, mnauth, qdata's keys, the mnlistdiff
//! CL-sig map) or the basic scheme (platformban). An eager field Core cannot
//! read throws, and one it reads writes back canonical or as zeros. The
//! prediction applies that with the fork's `BlsEager`, and builds the CL-sig
//! map Core's `std::map` holds: keys as re-serialized, first entry kept,
//! ordered by the key's double SHA-256. Lazy fields (isdlock, qsigrec, the
//! SML operator key) write back their wire bytes. Final commitments decode
//! eagerly in base-sdk already. A diff's coinbase transaction is written back
//! as the node writes it (CS0's `core_bytes`). For the objects Core hashes,
//! the prediction is base-sdk's `Hashable` of what it read, with the node's
//! BLS rules applied.

use std::collections::BTreeMap;

use bitcoin_hashes::sha256d;
use dash_p2p_core::{
    CommandString, MnListDiff, MnListFormat, P2pMsg, ProtoCtx, ProtocolVersion, QData, QrInfo,
    QuorumClSig, QuorumDataError, QuorumDataRequest, QuorumSnapshot, SimplifiedMnListEntry,
};
use dash_pkc::bls::{
    BlsCtx, BlsEagerPk, BlsEagerSig, BlsIesBlobBytes, BlsPkBytes, BlsScIetf, BlsSchemeKind,
    BlsSchemeSource, BlsSigBytes,
};
use dash_primitives::Transaction;
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};

/// One input: a command's payload at a stream version and BLS flag, with the
/// qdata limits (the quorum's threshold and valid member count).
#[derive(Clone, Debug)]
pub struct Case {
    pub cmd: String,
    /// The payload's layout and the field the case varies, for selection
    /// (`make_corpus.py`): e.g. "v2 quorum_sig".
    pub tag: String,
    pub version: i32,
    pub legacy: bool,
    pub raw: Vec<u8>,
    pub max_a: usize,
    pub max_b: usize,
}

/// A read: bytes read and the object written back, for qdata the error
/// code and a verification vector size other than the threshold, and the
/// hash Core keys the object by, where it hashes one (little-endian bytes).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Read {
    pub consumed: usize,
    pub reenc: Vec<u8>,
    pub error_code: Option<u8>,
    pub vvec_size: Option<usize>,
    pub hash: Option<Vec<u8>>,
}

impl Read {
    fn plain(consumed: usize, reenc: Vec<u8>) -> Self {
        Self {
            consumed,
            reenc,
            error_code: None,
            vvec_size: None,
            hash: None,
        }
    }

    fn hashed<T: Hashable<Hash: Numeric>>(consumed: usize, reenc: Vec<u8>, obj: &T) -> Self {
        Self {
            hash: Some(obj.hash().to_lendian().as_ref().to_vec()),
            ..Self::plain(consumed, reenc)
        }
    }
}

fn wire<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn malleable<E>(_: E) -> String {
    "malleable BLS object".into()
}

/// An eager signature as Core reads and writes it under `scheme`.
fn sig(
    bytes: &BlsSigBytes<BlsScIetf>,
    scheme: BlsSchemeKind,
) -> Result<BlsSigBytes<BlsScIetf>, String> {
    let eager = BlsEagerSig::decode(bytes.as_bytes(), BlsSchemeSource::Version(scheme))
        .map_err(malleable)?;
    Ok(BlsSigBytes::from_bytes(eager.to_bytes(scheme)))
}

/// An eager public key as Core reads and writes it under `scheme`.
fn pk(
    bytes: &BlsPkBytes<BlsScIetf>,
    scheme: BlsSchemeKind,
) -> Result<BlsPkBytes<BlsScIetf>, String> {
    let eager = BlsEagerPk::decode(bytes.as_bytes(), BlsSchemeSource::Version(scheme))
        .map_err(malleable)?;
    Ok(BlsPkBytes::from_bytes(eager.to_bytes(scheme)))
}

/// Core's `std::map<CBLSSignature, std::set<uint16_t>>` of the wire entries.
pub fn cl_sig_map(
    wire_order: &[QuorumClSig],
    scheme: BlsSchemeKind,
) -> Result<Vec<QuorumClSig>, String> {
    let mut map = BTreeMap::new();
    for entry in wire_order {
        let key = sig(&entry.sig, scheme)?;
        let hash = sha256d::Hash::hash(key.as_bytes()).to_byte_array();
        map.entry(hash).or_insert_with(|| QuorumClSig {
            sig: key,
            index_set: entry.index_set.clone(),
        });
    }
    Ok(map.into_values().collect())
}

/// The coinbase transaction as the node writes it back: CS0's `core_bytes`,
/// Core's serialization of the transaction it read.
fn node_tx(tx: &Transaction) -> Result<Vec<u8>, String> {
    let bytes = wire(tx);
    let d = dash_consensus::decode_tx_exact(&bytes).map_err(|e| format!("node tx: {e:?}"))?;
    Ok(d.core_bytes().as_slice().to_vec())
}

/// A diff as Core writes it back, in its own layout.
fn diff(d: &MnListDiff, scheme: BlsSchemeKind, out: &mut Vec<u8>) -> Result<(), String> {
    let f = d.format;
    if f >= MnListFormat::VersionFirst {
        d.version.encode(out);
    }
    d.base_block_hash.encode(out);
    d.block_hash.encode(out);
    d.cb_tx_merkle_tree.encode(out);
    out.extend(node_tx(&d.cb_tx)?);
    if matches!(f, MnListFormat::VersionAfterCbTx | MnListFormat::VersionedEntries) {
        d.version.encode(out);
    }
    d.deleted_mns.encode(out);
    count(d.mn_list.len(), out);
    for e in &d.mn_list {
        e.encode_as(out, f.has_entry_version());
    }
    d.deleted_quorums.encode(out);
    d.new_quorums.encode(out);
    if f >= MnListFormat::ClSigs {
        cl_sig_map(&d.quorum_cl_sigs, scheme)?.encode(out);
    }
    Ok(())
}

fn count(n: usize, out: &mut Vec<u8>) {
    dash_types::CompactSize::from(n).encode(out);
}

/// Rotation info as Core writes it back.
fn qrinfo(q: &QrInfo, scheme: BlsSchemeKind) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for s in [&q.snapshot_h_c, &q.snapshot_h_2c, &q.snapshot_h_3c] {
        s.encode(&mut out);
    }
    for d in [&q.diff_tip, &q.diff_h, &q.diff_h_c, &q.diff_h_2c, &q.diff_h_3c] {
        diff(d, scheme, &mut out)?;
    }
    out.push(u8::from(q.cycle_h_4c.is_some()));
    if let Some(c) = &q.cycle_h_4c {
        c.snapshot.encode(&mut out);
        diff(&c.diff, scheme, &mut out)?;
    }
    q.last_commitment_per_index.encode(&mut out);
    q.quorum_snapshot_list.encode(&mut out);
    count(q.mn_list_diff_list.len(), &mut out);
    for d in &q.mn_list_diff_list {
        diff(d, scheme, &mut out)?;
    }
    Ok(out)
}

/// qdata as Core's handler reads it with the case's limits.
fn qdata(q: &QData, consumed_head: usize, c: &Case, scheme: BlsSchemeKind) -> Result<Read, String> {
    let error_code = Some(q.error.to_base());
    let mut reenc = wire(&q.request);
    if q.error != QuorumDataError::Undefined {
        reenc.push(q.error.to_base());
    }
    if q.error != QuorumDataError::None {
        return Ok(Read {
            error_code,
            ..Read::plain(consumed_head, reenc)
        });
    }
    let data = q.read_data(c.max_a, c.max_b).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    let mut vvec_size = None;
    if let Some(vvec) = &data.vvec {
        let keys = vvec.iter().map(|k| pk(k, scheme)).collect::<Result<Vec<_>, _>>()?;
        if keys.len() != c.max_a {
            vvec_size = Some(keys.len());
        }
        keys.encode(&mut body);
    }
    if let Some(contributions) = &data.contributions {
        let blobs = contributions
            .iter()
            .map(|b| {
                Ok(BlsIesBlobBytes::new(
                    pk(b.ephemeral_pk(), scheme)?,
                    *b.iv_seed(),
                    b.data().to_vec(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        blobs.encode(&mut body);
    }
    // Every field has one width, so the bytes read are the bytes written.
    let consumed = consumed_head + body.len();
    reenc.extend(body);
    Ok(Read {
        error_code,
        vvec_size,
        ..Read::plain(consumed, reenc)
    })
}

/// base-sdk's read of `c`, with the node's BLS rules, or why it failed.
pub fn ours(c: &Case) -> Result<Read, String> {
    let global = BlsSchemeKind::from_legacy(c.legacy);
    let mut data = &c.raw[..];
    match c.cmd.as_str() {
        "smlentry" => {
            let versioned = c.version >= ProtocolVersion::SMNLE_VERSIONED.to_base();
            let e = SimplifiedMnListEntry::decode_as(&mut data, versioned)
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            e.encode_as(&mut out, versioned);
            return Ok(Read::hashed(c.raw.len() - data.len(), out, &e));
        }
        "qsnapshot" => {
            let s = QuorumSnapshot::decode(&mut data).map_err(|e| e.to_string())?;
            return Ok(Read::plain(c.raw.len() - data.len(), wire(&s)));
        }
        _ => {}
    }
    let ctx = ProtoCtx::new(ProtocolVersion::from_base(c.version), BlsCtx::from_legacy(c.legacy));
    let cmd = CommandString::from_static(&c.cmd);
    let d = P2pMsg::decode_with_policy(&cmd, &c.raw, &ctx).map_err(|e| e.to_string())?;
    let msg = match d.msg {
        P2pMsg::ClSig(mut m) => {
            m.sig = sig(&m.sig, global)?;
            P2pMsg::ClSig(m)
        }
        P2pMsg::MnAuth(mut m) => {
            m.sig = sig(&m.sig, global)?;
            P2pMsg::MnAuth(m)
        }
        P2pMsg::PlatformBan(mut m) => {
            m.sig = sig(&m.sig, BlsSchemeKind::Basic)?;
            P2pMsg::PlatformBan(m)
        }
        P2pMsg::MnListDiff(m) => {
            let mut out = Vec::new();
            diff(&m, global, &mut out)?;
            return Ok(Read::plain(d.consumed, out));
        }
        P2pMsg::QrInfo(m) => return Ok(Read::plain(d.consumed, qrinfo(&m, global)?)),
        P2pMsg::QData(q) => {
            // Core reads the error byte when there is one, 0xff included.
            let head = c.raw.len().min(QuorumDataRequest::SIZE + 1);
            return qdata(&q, head, c, global);
        }
        other => other,
    };
    let mut reenc = Vec::new();
    msg.encode_payload(&mut reenc);
    Ok(match &msg {
        P2pMsg::ClSig(m) => Read::hashed(d.consumed, reenc, m),
        P2pMsg::IsdLock(m) => Read::hashed(d.consumed, reenc, m),
        P2pMsg::QSigRec(m) => Read::hashed(d.consumed, reenc, m),
        P2pMsg::QfCommit(m) => Read::hashed(d.consumed, reenc, m.as_ref()),
        P2pMsg::Spork(m) => Read::hashed(d.consumed, reenc, m),
        P2pMsg::PlatformBan(m) => Read::hashed(d.consumed, reenc, m),
        _ => Read::plain(d.consumed, reenc),
    })
}

/// Whether base-sdk refused only flag bytes Core read as `true` (BSX A-12,
/// kept strict by DEC-98): with each refused flag byte set to 1, base-sdk
/// reads what Core read.
pub fn strict_flags(c: &Case, ours: &Result<Read, String>, core: &Result<Read, String>) -> bool {
    if !matches!((ours, core), (Err(e), Ok(_)) if refused_flag(e)) {
        return false;
    }
    let mut fixed = c.clone();
    while let Err(e) = self::ours(&fixed) {
        if !refused_flag(&e) {
            return false;
        }
        match refused_flag_at(&fixed) {
            Some(at) if e.ends_with(&format!("got {}", fixed.raw[at])) => fixed.raw[at] = 1,
            _ => return false,
        }
    }
    compare(&self::ours(&fixed), core) == Outcome::Match
}

fn refused_flag(e: &str) -> bool {
    e.starts_with("non-canonical bool")
}

/// The flag byte base-sdk refuses: the last byte of the shortest prefix that,
/// zero-filled to the input's length, it refuses for a flag. A zero is a
/// valid flag and the reads are sequential, so a prefix that ends before the
/// flag reads past it.
fn refused_flag_at(c: &Case) -> Option<usize> {
    let (mut lo, mut hi) = (0, c.raw.len());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let mut raw = c.raw[..mid].to_vec();
        raw.resize(c.raw.len(), 0);
        let prefix = Case {
            raw,
            ..c.clone()
        };
        match self::ours(&prefix) {
            Err(e) if refused_flag(&e) => hi = mid,
            _ => lo = mid,
        }
    }
    hi.checked_sub(1)
}

/// How base-sdk's prediction compares with Core's read.
#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Same verdict, bytes read, write-back and object hash.
    Match,
    /// Same verdict and bytes read; the write-back differs (PLAN §7: counted,
    /// not printed).
    Reencoding,
    /// A different verdict or bytes read.
    Mismatch(String),
}

pub fn compare(ours: &Result<Read, String>, core: &Result<Read, String>) -> Outcome {
    match (ours, core) {
        // Verdicts only: no Core logic observes the error class (DEC-98).
        (Err(_), Err(_)) => Outcome::Match,
        (Ok(_), Err(e)) => Outcome::Mismatch(format!("base-sdk read, Core threw: {}", class(e))),
        (Err(e), Ok(_)) => Outcome::Mismatch(format!("base-sdk refused ({e}), Core read")),
        (Ok(o), Ok(c)) => {
            if o.consumed != c.consumed {
                Outcome::Mismatch(format!("bytes read {} vs {}", o.consumed, c.consumed))
            } else if o.error_code != c.error_code || o.vvec_size != c.vvec_size {
                Outcome::Mismatch(format!(
                    "qdata error/vvec {:?}/{:?} vs {:?}/{:?}",
                    o.error_code, o.vvec_size, c.error_code, c.vvec_size
                ))
            } else if o.reenc != c.reenc {
                Outcome::Reencoding
            } else if o.hash != c.hash {
                Outcome::Mismatch(format!(
                    "object hash {} vs {}",
                    o.hash.as_deref().map(hex::encode).unwrap_or_default(),
                    c.hash.as_deref().map(hex::encode).unwrap_or_default()
                ))
            } else {
                Outcome::Match
            }
        }
    }
}

/// The class of a Core exception, from its text.
pub fn class(what: &str) -> &'static str {
    const CLASSES: [(&str, &str); 7] = [
        ("end of data", "eof"),
        ("non-canonical ReadCompactSize()", "noncanonical"),
        ("ReadCompactSize(): size too large", "size"),
        ("Vector length limit exceeded", "length_limit"),
        ("Out-of-range bits set", "padding"),
        ("malleable BLS object", "malleable"),
        ("oversized", "oversized"),
    ];
    CLASSES.iter().find(|(text, _)| what.contains(text)).map_or("other", |(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(n: usize, b: &[u8]) -> Result<Read, String> {
        Ok(Read::plain(n, b.to_vec()))
    }

    #[test]
    fn every_difference_is_reported() {
        assert_eq!(compare(&read(3, &[1]), &read(3, &[1])), Outcome::Match);
        assert_eq!(compare(&Err("a".into()), &Err("b".into())), Outcome::Match);
        assert_eq!(compare(&read(3, &[1]), &read(3, &[2])), Outcome::Reencoding);
        assert!(matches!(compare(&read(2, &[1]), &read(3, &[1])), Outcome::Mismatch(_)));
        assert!(matches!(compare(&read(3, &[1]), &Err("x".into())), Outcome::Mismatch(_)));
        assert!(matches!(compare(&Err("x".into()), &read(3, &[1])), Outcome::Mismatch(_)));
        let mut q = Read::plain(3, vec![1]);
        q.error_code = Some(0);
        assert!(matches!(compare(&Ok(q), &read(3, &[1])), Outcome::Mismatch(_)));
        let mut h = Read::plain(3, vec![1]);
        h.hash = Some(vec![7]);
        assert!(matches!(compare(&read(3, &[1]), &Ok(h.clone())), Outcome::Mismatch(_)));
        assert_eq!(compare(&Ok(h.clone()), &Ok(h)), Outcome::Match);
    }

    #[test]
    fn cl_sig_map_keeps_the_first_of_a_key_in_hash_order() {
        // All-zero signatures are the invalid object in either scheme.
        let zero = QuorumClSig {
            sig: BlsSigBytes::from_bytes([0; 96]),
            index_set: [1].into(),
        };
        let zero2 = QuorumClSig {
            sig: BlsSigBytes::from_bytes([0; 96]),
            index_set: [2].into(),
        };
        let map = cl_sig_map(&[zero.clone(), zero2], BlsSchemeKind::Basic).unwrap();
        assert_eq!(map, [zero]);
    }
}

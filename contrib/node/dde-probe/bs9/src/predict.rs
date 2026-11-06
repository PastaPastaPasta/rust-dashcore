//! base-sdk's read of a case, with the node's BLS rules applied: the
//! prediction of Core's read that the judge checks.
//!
//! base-sdk keeps BLS fields as wire bytes (Q-12). Core reads every BLS
//! field of a DKG message eagerly under its process-wide scheme: one it
//! cannot read throws, and one it reads writes back canonical or as zeros.
//! The prediction applies that with base-sdk's `DkgMessage::eager`, the
//! code the node runs. The shares of the signature share messages are lazy
//! and write back their wire bytes.
//!
//! A DKG case goes through Core's pipeline: the length of the type and
//! quorum hash, `DkgMessage::check_payload` against the case's quorum
//! parameters, then the whole read. Its hash is `dkg_inventory_hash` of the
//! payload, Core's inventory hash; its sign hash is base-sdk's over the
//! object as Core re-serializes it.

use dash_p2p_core::{
    CommandString, DkgComplaint, DkgContribution, DkgJustification, DkgMessage,
    DkgPrematureCommitment, DsSignedInputs, P2pDecodeError, P2pMsg, ProtoCtx, ProtocolVersion,
    dkg_inventory_hash, dkg_quorum,
};
use dash_params::LlmqType;
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqParams, Network, Release, available_params};
use dash_pkc::bls::{BlsCtx, BlsDecodeError};
use dash_types::{Hashable, Numeric};

/// One input: a command's payload at a BLS flag, with a DKG case's quorum
/// parameters (size, minimum size, threshold).
#[derive(Clone, Debug)]
pub struct Case {
    pub cmd: String,
    /// The payload's layout and the field the case varies, for selection
    /// (`make_corpus.py`).
    pub tag: String,
    pub legacy: bool,
    pub raw: Vec<u8>,
    pub quorum: [i32; 3],
    /// The stream version, the peer's common version; only `dsa`'s layout
    /// depends on it.
    pub version: i32,
}

/// Core's `PROTOCOL_VERSION` at the pin, the version a case is read at
/// unless it varies it.
pub const VERSION: i32 = 70242;

/// A read: bytes read, the object written back, the hash Core keys the
/// object by (a DKG message's inventory hash, a queue's, a mixed
/// transaction's txid, a governance object's or vote's), and the sign
/// hashes Core computes (little-endian bytes, concatenated).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Read {
    pub consumed: usize,
    pub reenc: Vec<u8>,
    pub hash: Option<Vec<u8>>,
    pub sign_hash: Option<Vec<u8>>,
}

fn le<H: Numeric>(h: H) -> Vec<u8> {
    h.to_lendian().as_ref().to_vec()
}

/// The quorum parameters of a case: an available type's, with its counts
/// replaced.
// Core: llmq/params.h:138 (available_llmqs); chainparams.cpp:1019-1028 overrides a copy of it
pub fn params(quorum: [i32; 3]) -> LlmqParams {
    let mut p = available_params(LlmqType::LlmqTest).expect("llmq_test");
    [p.size, p.min_size, p.threshold] =
        quorum.map(|n| u16::try_from(n).expect("a count dash-params holds (quirk 46)"));
    p
}

/// Mainnet's parameters, which bound a CoinJoin entry's and signature
/// list's counts (the judge selects mainnet).
fn mainnet() -> &'static ChainSetup {
    static PARAMS: std::sync::OnceLock<ChainSetup> = std::sync::OnceLock::new();
    PARAMS.get_or_init(|| {
        ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet")
    })
}

/// The hashes Core computes on a message other than a DKG one.
type Hashes = (Option<Vec<u8>>, Option<Vec<u8>>);

fn both<H: Numeric, S: Numeric>(hash: H, sign_hash: S) -> Hashes {
    (Some(le(hash)), Some(le(sign_hash)))
}

/// Core's hashes of a message other than a DKG one, or the handler's
/// refusal after the read that base-sdk leaves to the node (a `dsi` list
/// past the pool, a `govsync` filter past its size constraints).
fn hashes(msg: &P2pMsg) -> Result<Hashes, String> {
    Ok(match msg {
        P2pMsg::QSigSesAnn(m) => {
            (None, Some(m.anns.iter().flat_map(|a| le(a.sign_hash())).collect()))
        }
        P2pMsg::QSigShare(m) => {
            (None, Some(m.shares.iter().flat_map(|s| le(s.sign_hash())).collect()))
        }
        P2pMsg::Dsi(m) if !m.fits_pool(mainnet()) => return Err("vector size too large".into()),
        P2pMsg::GovSync(m) if !m.bloom_filter.is_within_size_constraints() => {
            return Err("govsync filter: size constraints".into());
        }
        P2pMsg::Dsq(m) => both(m.hash(), m.signature_hash()),
        P2pMsg::Dstx(m) => both(m.tx.hash(), m.signature_hash()),
        P2pMsg::GovObj(m) => both(m.hash(), m.signature_hash()),
        P2pMsg::GovObjVote(m) => both(m.hash(), m.signature_hash()),
        _ => (None, None),
    })
}

/// The node's checks before a CoinJoin or governance read: `dss`'s count
/// (Core's `ERR_MAXIMUM` reply).
fn pre_read(c: &Case) -> Result<(), String> {
    if c.cmd != "dss" {
        return Ok(());
    }
    match DsSignedInputs::check_payload(&c.raw, mainnet()) {
        Err(P2pDecodeError::TooManyEntries {
            ..
        }) => Err("ERR_MAXIMUM".into()),
        other => other.map_err(|e| e.to_string()),
    }
}

/// A failed read, named as Core's handler stages it: `govsync`'s hash is
/// read apart from its filter.
fn read_error(c: &Case, e: P2pDecodeError) -> String {
    match c.cmd.as_str() {
        "govsync" if c.raw.len() < 32 => format!("govsync hash: {e}"),
        "govsync" => format!("govsync filter: {e}"),
        _ => e.to_string(),
    }
}

/// A DKG message as Core's worker holds it, with its sign hash.
fn dkg(msg: &P2pMsg, bls: BlsCtx) -> Result<(P2pMsg, Vec<u8>), BlsDecodeError> {
    Ok(match msg {
        P2pMsg::QContrib(m) => {
            let m = m.eager(bls)?;
            let sign_hash = le(m.sign_hash());
            (P2pMsg::QContrib(Box::new(m)), sign_hash)
        }
        P2pMsg::QComplaint(m) => {
            let m = m.eager(bls)?;
            let sign_hash = le(m.sign_hash());
            (P2pMsg::QComplaint(m), sign_hash)
        }
        P2pMsg::QJustify(m) => {
            let m = m.eager(bls)?;
            let sign_hash = le(m.sign_hash());
            (P2pMsg::QJustify(m), sign_hash)
        }
        P2pMsg::QpCommit(m) => {
            let m = m.eager(bls)?;
            let sign_hash = le(m.sign_hash());
            (P2pMsg::QpCommit(m), sign_hash)
        }
        other => panic!("not a DKG message: {}", other.command()),
    })
}

/// Core's DKG handler checks for `c`, before the read.
fn dkg_checks(c: &Case) -> Result<(), String> {
    dkg_quorum(&c.raw).ok_or("short")?;
    let p = params(c.quorum);
    let checked = match c.cmd.as_str() {
        "qcontrib" => DkgContribution::check_payload(&c.raw, &p),
        "qcomplaint" => DkgComplaint::check_payload(&c.raw, &p),
        "qjustify" => DkgJustification::check_payload(&c.raw, &p),
        "qpcommit" => DkgPrematureCommitment::check_payload(&c.raw, &p),
        _ => return Ok(()),
    };
    checked.map_err(|e| e.to_string())
}

pub fn is_dkg(cmd: &str) -> bool {
    matches!(cmd, "qcontrib" | "qcomplaint" | "qjustify" | "qpcommit")
}

/// base-sdk's read of `c`, with the node's BLS rules, or why it failed.
pub fn ours(c: &Case) -> Result<Read, String> {
    let bls = BlsCtx::from_legacy(c.legacy);
    if is_dkg(&c.cmd) {
        dkg_checks(c)?;
    }
    pre_read(c)?;
    let ctx = ProtoCtx::new(ProtocolVersion::from_base(c.version), bls);
    let cmd = CommandString::from_static(&c.cmd);
    let d = P2pMsg::decode_with_policy(&cmd, &c.raw, &ctx).map_err(|e| read_error(c, e))?;
    let (msg, hash, sign_hash) = if is_dkg(&c.cmd) {
        let (msg, sign_hash) = dkg(&d.msg, bls).map_err(|e| e.to_string())?;
        (msg, Some(le(dkg_inventory_hash(&c.raw))), Some(sign_hash))
    } else {
        let (hash, sign_hash) = hashes(&d.msg)?;
        (d.msg, hash, sign_hash)
    };
    let mut reenc = Vec::new();
    msg.encode_payload(&mut reenc);
    Ok(Read {
        consumed: d.consumed,
        reenc,
        hash,
        sign_hash,
    })
}

/// How base-sdk's prediction compares with Core's read.
#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Same verdict, bytes read, write-back and hashes.
    Match,
    /// Same verdict and bytes read; the write-back differs (PLAN §7: counted,
    /// not printed).
    Reencoding,
    /// A different verdict, bytes read or hash.
    Mismatch(String),
}

pub fn compare(ours: &Result<Read, String>, core: &Result<Read, String>) -> Outcome {
    match (ours, core) {
        // Verdicts only: no Core logic observes the error class (DEC-98),
        // except the DKG handler's, whose refusals before the read score
        // the peer apart from a failed read; see `stage`.
        (Err(o), Err(k)) if stage(o) == stage(k) => Outcome::Match,
        (Err(o), Err(k)) => Outcome::Mismatch(format!("refused at {} vs {}", stage(o), stage(k))),
        (Ok(_), Err(e)) => Outcome::Mismatch(format!("base-sdk read, Core threw: {}", class(e))),
        (Err(e), Ok(_)) => Outcome::Mismatch(format!("base-sdk refused ({e}), Core read")),
        (Ok(o), Ok(c)) => {
            if o.consumed != c.consumed {
                Outcome::Mismatch(format!("bytes read {} vs {}", o.consumed, c.consumed))
            } else if o.hash != c.hash {
                Outcome::Mismatch("inventory hash".into())
            } else if o.sign_hash != c.sign_hash {
                // Before the write-back: a re-encoding must not hide it.
                Outcome::Mismatch("sign hash".into())
            } else if o.reenc != c.reenc {
                Outcome::Reencoding
            } else {
                Outcome::Match
            }
        }
    }
}

/// Where a refusal happens, for the handlers that tell them apart: a DKG
/// message's before the read (the handler's length, size cap or walk) or
/// in it; `dss`'s count (a reply); `govsync`'s hash (logged) or filter
/// (scored).
pub fn stage(what: &str) -> &'static str {
    if what == "ERR_MAXIMUM" {
        "reply"
    } else if what.starts_with("govsync hash") {
        "log"
    } else if what.starts_with("govsync filter") {
        "score"
    } else if what == "short"
        || what.contains("oversized")
        || what.contains("over its handler's limit")
    {
        "size"
    } else if what == "framing" || what.contains("framing does not fit") {
        "framing"
    } else {
        "read"
    }
}

/// The class of a Core exception, from its text.
pub fn class(what: &str) -> &'static str {
    const CLASSES: [(&str, &str); 17] = [
        ("ERR_MAXIMUM", "err_maximum"),
        ("govsync hash", "short_hash"),
        ("govsync filter", "filter"),
        ("end of data", "eof"),
        ("non-canonical ReadCompactSize()", "noncanonical"),
        ("ReadCompactSize(): size too large", "size"),
        ("ReadVarInt(): size too large", "varint"),
        ("Vector length limit exceeded", "length_limit"),
        ("vector size too large", "length_limit"),
        ("CCoinJoinEntry::", "length_limit"),
        ("bad governance vote signature size", "vote_sig_size"),
        ("count too large", "length_limit"),
        ("Out-of-range bits set", "padding"),
        ("declared size exceeds remaining bytes", "eof"),
        ("malleable BLS object", "malleable"),
        ("isVarInts", "form"),
        ("out of bounds index", "index"),
    ];
    match what {
        "short" | "oversized" => "oversized",
        "framing" => "framing",
        _ => CLASSES.iter().find(|(text, _)| what.contains(text)).map_or("other", |(_, c)| c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(n: usize, b: &[u8]) -> Result<Read, String> {
        Ok(Read {
            consumed: n,
            reenc: b.to_vec(),
            hash: None,
            sign_hash: None,
        })
    }

    #[test]
    fn every_difference_is_reported() {
        assert_eq!(compare(&read(3, &[1]), &read(3, &[1])), Outcome::Match);
        assert_eq!(compare(&Err("a".into()), &Err("b".into())), Outcome::Match);
        assert!(matches!(
            compare(&Err("framing".into()), &Err("eof".into())),
            Outcome::Mismatch(_)
        ));
        assert!(matches!(
            compare(&Err("oversized".into()), &Err("framing".into())),
            Outcome::Mismatch(_)
        ));
        // The CoinJoin and governance handlers' stages.
        assert_eq!(compare(&Err("ERR_MAXIMUM".into()), &Err("ERR_MAXIMUM".into())), Outcome::Match);
        for (o, k) in [
            ("ERR_MAXIMUM", "vector size too large"),
            ("govsync hash: end of data", "govsync filter: end of data"),
            ("govsync filter: x", "end of data"),
        ] {
            assert!(
                matches!(compare(&Err(o.into()), &Err(k.into())), Outcome::Mismatch(_)),
                "{o} vs {k}"
            );
        }
        assert_eq!(compare(&read(3, &[1]), &read(3, &[2])), Outcome::Reencoding);
        assert!(matches!(compare(&read(2, &[1]), &read(3, &[1])), Outcome::Mismatch(_)));
        assert!(matches!(compare(&read(3, &[1]), &Err("x".into())), Outcome::Mismatch(_)));
        assert!(matches!(compare(&Err("x".into()), &read(3, &[1])), Outcome::Mismatch(_)));
        let mut h = read(3, &[1]).unwrap();
        h.sign_hash = Some(vec![7]);
        assert!(matches!(compare(&read(3, &[1]), &Ok(h.clone())), Outcome::Mismatch(_)));
        assert!(matches!(compare(&read(3, &[2]), &Ok(h.clone())), Outcome::Mismatch(_)));
        h.sign_hash = None;
        h.hash = Some(vec![7]);
        assert!(matches!(compare(&read(3, &[2]), &Ok(h)), Outcome::Mismatch(_)));
    }
}

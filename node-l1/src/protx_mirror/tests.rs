//! The mirror's encoder against base-sdk's (DEC-1066 condition 2), on every valid provider payload
//! of the mirrored types in the vectors node-l1 reaches: Core's fixtures (`trivially_valid.json`,
//! `trivially_invalid.json`, the oracle's recorded transactions and blocks) and dash-dml's
//! vector files. A payload is valid when base-sdk decodes it whole. For each one:
//!
//! 1. the mirror read from base-sdk's payload encodes to base-sdk's encoding, byte for byte, and
//!    so does its signature hash (`SER_GETHASH`, dash-dml's `SignedPayload`);
//! 2. base-sdk's decode of the mirror's bytes is the payload base-sdk's own encoding decodes to,
//!    by `PartialEq` and by `Debug` (which also shows a lazy key's scheme), and the payload itself
//!    whenever base-sdk's own round trip gives it back.
//!
//! The test fails if a (type, version) pair of [`EXPECTED`] finds no vector.
//!
//! [`mirror_sighashes_as_base_sdk`] checks the mirror's copies of base-sdk's ProRegTx consent
//! bytes and sign string against base-sdk's on the same payloads.

use std::collections::BTreeMap;
use std::fmt::Debug;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::Tx;
use dash_dml::specialtx::sig::{make_sign_string, payload_sig_hash, shared_reg_consent_hash};
use dash_dml::testkit::replay::{mainnet, testnet};
use dash_primitives::{Block, Transaction};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::ToBsdk;

/// Files holding transactions or blocks as hex, relative to the workspace root.
const FILES: &[&str] = &[
    "dash-oracle-sys/tests/data/special-tx-signatures.json",
    "dash-oracle-sys/tests/data/dml-crafted-v24.0.json",
    "dash-oracle-sys/tests/data/contextual-mainnet.json",
    "dash-oracle-sys/tests/data/contextual-regtest-v24.0.json",
    "dash-oracle-sys/tests/data/testnet.json",
    "dash-dml/tests/data/core_signed_protx.txt",
    "dash-dml/tests/data/ev2b-quirks.txt",
    "node-l1/tests/data/proupserv-mainnet-1028257.json",
    "node-l1/src/dml_apply/shared-case.json",
];

/// Core's own fixtures, under `src/test/data` of the vendored tree (`dash-core-vectors`).
const CORE_FILES: &[&str] =
    &["src/test/data/trivially_valid.json", "src/test/data/trivially_invalid.json"];

/// Every (type, version) pair the files above hold, which the test must reach.
const EXPECTED: &[(u16, u16)] = &[
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 1),
    (3, 2),
    (3, 3),
    (4, 1),
    (4, 2),
    (4, 3),
    (10, 1),
    (11, 1),
    (12, 1),
];

/// The runs of at least 120 hex digits of even length in `text`.
fn hex_runs(text: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut run = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_hexdigit() {
            run.push(c);
            continue;
        }
        if run.len() >= 120 && run.len().is_multiple_of(2) {
            let bytes = (0..run.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&run[i..i + 2], 16))
                .collect::<Result<Vec<u8>, _>>();
            out.extend(bytes.ok());
        }
        run.clear();
    }
    out
}

/// The special transactions in `bytes`: one transaction or one block, decoded whole by base-sdk.
fn special_txs(bytes: &[u8]) -> Vec<Transaction> {
    let mut data = bytes;
    if let Ok(tx) = Transaction::decode(&mut data)
        && data.is_empty()
    {
        return vec![tx];
    }
    let mut data = bytes;
    match Block::decode(&mut data) {
        Ok(block) if data.is_empty() => block.transactions().clone(),
        _ => Vec::new(),
    }
}

/// The signature hashes of a mirror and of the base-sdk payload it was read from.
type SigHashes<M> = fn(&M, &<M as ToBsdk>::Bsdk) -> (Option<[u8; 32]>, Option<[u8; 32]>);

/// One payload's checks; `false` when base-sdk does not decode it whole (not a valid payload).
#[allow(clippy::use_debug, clippy::expect_used)] // the comparison is of the Debug forms
fn check<M>(payload: &[u8], signed: Option<SigHashes<M>>) -> bool
where
    M: ToBsdk + for<'a> From<&'a M::Bsdk> + PartialEq + Debug,
    M::Bsdk: PartialEq + Debug,
{
    let mut data = payload;
    let Ok(p) = M::Bsdk::decode(&mut data) else {
        return false;
    };
    if !data.is_empty() {
        return false;
    }
    let mut want = Vec::new();
    p.encode(&mut want);
    let m = M::from(&p);
    assert_eq!(m.to_bytes(), want, "mirror encoding of {p:?}");
    if let Some(hashes) = signed {
        let (ours, theirs) = hashes(&m, &p);
        assert!(ours.is_some(), "a signature hash");
        assert_eq!(ours, theirs, "signature hash of {p:?}");
    }
    let back = m.to_bsdk().expect("base-sdk decodes the mirror's bytes whole");
    let own = M::Bsdk::decode(&mut want.as_slice()).expect("base-sdk decodes its own encoding");
    assert_eq!(back, own, "round trip of {p:?}");
    assert_eq!(format!("{back:?}"), format!("{own:?}"), "round trip of {p:?}");
    if own == p && format!("{own:?}") == format!("{p:?}") {
        assert_eq!(back, p);
    }
    assert_eq!(M::from(&back), M::from(&own));
    true
}

macro_rules! sig_hash {
    ($t:ident) => {
        Some(|m: &super::$t, p: &dash_primitives::$t| (payload_sig_hash(m), payload_sig_hash(p)))
    };
}

/// Every vector file of [`FILES`] and [`CORE_FILES`], by name.
#[allow(clippy::expect_used)]
fn texts() -> Vec<(String, String)> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let mut texts: Vec<(String, String)> = FILES
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(format!("{root}/{f}")).expect("a vector file");
            ((*f).to_owned(), text)
        })
        .collect();
    texts
        .extend(CORE_FILES.iter().map(|f| ((*f).to_owned(), dash_core_vectors::read_core_text(f))));
    texts
}

#[test]
fn mirror_encodes_as_base_sdk() {
    let texts = texts();
    let mut seen: BTreeMap<(u16, u16), usize> = BTreeMap::new();
    let mut files: BTreeMap<&str, usize> = BTreeMap::new();
    for (file, text) in &texts {
        for bytes in hex_runs(text) {
            for tx in special_txs(&bytes) {
                let payload = tx.extra_payload().as_slice();
                let ty = tx.tx_type().to_base();
                let ok = match ty {
                    1 => check::<super::ProRegTx>(payload, sig_hash!(ProRegTx)),
                    2 => check::<super::ProUpServTx>(payload, sig_hash!(ProUpServTx)),
                    3 => check::<super::ProUpRegTx>(payload, sig_hash!(ProUpRegTx)),
                    4 => check::<super::ProUpRevTx>(payload, sig_hash!(ProUpRevTx)),
                    10 => check::<super::ProDisTx>(payload, None),
                    11 => check::<super::ProUpShareTx>(payload, sig_hash!(ProUpShareTx)),
                    12 => check::<super::ProUpSharedRegTx>(payload, sig_hash!(ProUpSharedRegTx)),
                    _ => false,
                };
                if ok {
                    let version = u16::from_le_bytes([payload[0], payload[1]]);
                    *seen.entry((ty, version)).or_default() += 1;
                    *files.entry(file.as_str()).or_default() += 1;
                }
            }
        }
    }
    eprintln!("protx mirror round trip: {} cases", seen.values().sum::<usize>());
    for ((ty, version), n) in &seen {
        eprintln!("  type {ty} version {version}: {n}");
    }
    for (file, n) in &files {
        eprintln!("  {file}: {n}");
    }
    for pair in EXPECTED {
        assert!(seen.contains_key(pair), "no vector of (type, version) {pair:?}");
    }
}

/// The mirror's copies of base-sdk's `shared_consent_bytes` and `sign_string` against
/// base-sdk's (DEC-1066 decision 1), through dash-dml's `shared_reg_consent_hash` and
/// `make_sign_string`, on every ProRegTx of the round trip's vectors that base-sdk decodes whole:
/// the consent hash over the carrying transaction, and the sign string under mainnet's,
/// testnet's and regtest's prefixes. base-sdk defines both for any ProRegTx. Core signs the
/// consent hash only for a shared registration and the sign string only for an external
/// collateral, so the test also needs one of each. A ProUpSharedRegTx's signatures sign its
/// `SER_GETHASH`, which the round trip checks.
#[test]
#[allow(clippy::use_debug)] // the failure messages name the payload
fn mirror_sighashes_as_base_sdk() {
    let networks = [mainnet(), testnet(), crate::chain::build::regtest()];
    let (mut cases, mut shared, mut external) = (0usize, 0usize, 0usize);
    for (_, text) in texts() {
        for bytes in hex_runs(&text) {
            for tx in special_txs(&bytes) {
                if tx.tx_type().to_base() != 1 {
                    continue;
                }
                let mut data = tx.extra_payload().as_slice();
                let Ok(p) = dash_primitives::ProRegTx::decode(&mut data) else {
                    continue;
                };
                if !data.is_empty() {
                    continue;
                }
                let m = super::ProRegTx::from(&p);
                let carrier = Tx::new(tx.clone());
                assert_eq!(
                    Some(sha256d(&m.shared_consent_bytes(&tx))),
                    shared_reg_consent_hash(&p, &carrier),
                    "consent hash of {p:?}"
                );
                for params in networks {
                    assert_eq!(
                        Some(m.sign_string(params)),
                        make_sign_string(&p, params),
                        "sign string of {p:?}"
                    );
                }
                cases += 1;
                shared += usize::from(!p.shares().is_empty());
                external += usize::from(!p.collateral_hash().is_null());
            }
        }
    }
    eprintln!(
        "protx mirror sighashes: {cases} ProRegTx cases ({shared} shared, {external} with an \
         external collateral), consent hash on each, sign string on each under {} networks",
        networks.len()
    );
    assert!(shared > 0, "no shared registration in the vectors");
    assert!(external > 0, "no external collateral in the vectors");
}

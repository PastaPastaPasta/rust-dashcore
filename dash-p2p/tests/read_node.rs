//! Corpus and golden coverage for the node reader.

use dash_consensus::primitives::{
    CommitmentBls, MnListDiffBls, P2pCommitment, P2pMnListDiff, P2pQrInfo,
};
use dash_p2p::read;
use dash_p2p_core::{MnListDiff, MnListFormat, P2pDecodeError, ProtocolVersion, QrInfo};
use dash_pkc::bls::BlsCtx;
use dash_primitives::Commitment;
use dash_types::codec::{BaseCodec, DecodeError};
use read::HandlerRead;

#[path = "common/read.rs"]
pub mod read_helpers;

use read_helpers::*;

fn assert_message_repr(actual: &str, expected: &str) {
    if let Some(debug) = expected.strip_prefix("debug:") {
        assert_eq!(actual, debug);
    } else if let Some(hash) = expected.strip_prefix("sha256:") {
        assert_eq!(hex_bytes(&dash_sha256::sha256(actual.as_bytes())), hash);
    } else {
        panic!("invalid golden message representation: {expected}");
    }
}

fn read_at<T: HandlerRead>(bytes: &[u8], ctx: &read::PeerCtx) -> Result<(T, usize), String> {
    let mut cursor = bytes;
    T::read(&mut cursor, ctx).map(|read| (read, bytes.len() - cursor.len())).map_err(fail)
}

/// base-sdk's own decode of `bytes`, with the reader's error mapping.
fn base_at<T: std::fmt::Debug>(
    bytes: &[u8],
    decode: impl FnOnce(&mut &[u8]) -> Result<T, DecodeError>,
) -> Result<(String, usize), String> {
    let mut cursor = bytes;
    decode(&mut cursor)
        .map(|value| (format!("{value:?}"), bytes.len() - cursor.len()))
        .map_err(|error| fail(read::ReadFail::Decode(P2pDecodeError::Consensus(error))))
}

fn assert_own_bls(qc: &Commitment, bls: CommitmentBls<'_>) {
    assert!(std::ptr::eq(bls.quorum_public_key, qc.quorum_public_key()));
    assert!(std::ptr::eq(bls.quorum_sig, qc.quorum_sig()));
    assert!(std::ptr::eq(bls.members_sig, qc.members_sig()));
}

/// Checks that each commitment of `side` is the matching one of `own` and that its side-car holds
/// its own BLS objects; returns the number of commitments.
fn assert_side_cars<'a>(
    side: impl ExactSizeIterator<Item = (&'a Commitment, CommitmentBls<'a>)>,
    own: &'a [Commitment],
) -> usize {
    assert_eq!(side.len(), own.len());
    for ((qc, bls), own) in side.zip(own) {
        assert!(std::ptr::eq(qc, own));
        assert_own_bls(qc, bls);
    }
    own.len()
}

/// Checks that `view` is `diff` and that each new quorum's side-car holds its own BLS objects;
/// returns the number of new quorums.
fn assert_diff_side_car(view: MnListDiffBls<'_>, diff: &MnListDiff) -> usize {
    assert!(std::ptr::eq(&*view, diff));
    assert_side_cars(view.new_quorum_bls(), diff.new_quorums())
}

/// `qfcommit`, `mnlistdiff` and `qrinfo` read through their side-car types exactly as base-sdk's
/// decoders read them, and every side-car holds its commitment's own BLS objects.
#[test]
fn commitment_messages_read_the_same_through_their_side_car() {
    let mut reads = 0usize;
    let mut commitments = 0usize;
    let mut by_command = std::collections::BTreeMap::<&str, (usize, usize)>::new();
    for CorpusCase {
        name,
        bytes,
        ..
    } in corpus_cases()
    {
        if !matches!(name, "qfcommit" | "mnlistdiff" | "qrinfo") {
            continue;
        }
        for &version in &SHADOW_VERSIONS {
            let format = MnListFormat::for_version(ProtocolVersion::new(version));
            for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
                let ctx = ctx(version, bls);
                reads += 1;
                let before = commitments;
                let (base, side) = match name {
                    "qfcommit" => {
                        let side = read_at::<P2pCommitment>(&bytes, &ctx);
                        if let Ok((qc, _)) = &side {
                            assert_own_bls(qc, qc.bls());
                            commitments += 1;
                        }
                        (
                            base_at(&bytes, Commitment::decode),
                            side.map(|(qc, n)| (format!("{:?}", qc.into_commitment()), n)),
                        )
                    }
                    "mnlistdiff" => {
                        let side = read_at::<P2pMnListDiff>(&bytes, &ctx);
                        if let Ok((diff, _)) = &side {
                            commitments += assert_diff_side_car(diff.bls(), diff);
                        }
                        (
                            base_at(&bytes, |data| MnListDiff::decode_as(data, format)),
                            side.map(|(diff, n)| (format!("{:?}", diff.into_diff()), n)),
                        )
                    }
                    _ => {
                        let side = read_at::<P2pQrInfo>(&bytes, &ctx);
                        if let Ok((info, _)) = &side {
                            let own: &QrInfo = info;
                            commitments +=
                                assert_diff_side_car(info.diff_tip_bls(), own.diff_tip())
                                    + assert_diff_side_car(info.diff_h_bls(), own.diff_h())
                                    + assert_diff_side_car(info.diff_h_c_bls(), own.diff_h_c())
                                    + assert_diff_side_car(info.diff_h_2c_bls(), own.diff_h_2c())
                                    + assert_diff_side_car(info.diff_h_3c_bls(), own.diff_h_3c())
                                    + assert_side_cars(
                                        info.last_commitment_bls(),
                                        own.last_commitment_per_index(),
                                    );
                            let cycle = own.cycle_h_4c().as_ref().map(|cycle| cycle.diff());
                            assert_eq!(info.diff_h_4c_bls().is_some(), cycle.is_some());
                            if let (Some(view), Some(diff)) = (info.diff_h_4c_bls(), cycle) {
                                commitments += assert_diff_side_car(view, diff);
                            }
                            assert_eq!(
                                info.mn_list_diff_list_bls().len(),
                                own.mn_list_diff_list().len()
                            );
                            for (view, diff) in
                                info.mn_list_diff_list_bls().zip(own.mn_list_diff_list())
                            {
                                commitments += assert_diff_side_car(view, diff);
                            }
                        }
                        (
                            base_at(&bytes, |data| QrInfo::decode_as(data, format)),
                            side.map(|(info, n)| (format!("{:?}", info.into_info()), n)),
                        )
                    }
                };
                let ok = base.is_ok();
                assert_eq!(base, side, "{name} at {version} under {bls:?}");
                let (decoded, checked) = by_command.entry(name).or_default();
                *decoded += usize::from(ok);
                *checked += commitments - before;
            }
        }
    }
    eprintln!("side-car reads {reads}, (decoded, commitments checked) {by_command:?}");
    assert_eq!(reads, 5_700);
    assert_eq!(
        by_command.into_iter().collect::<Vec<_>>(),
        [("mnlistdiff", (518, 840)), ("qfcommit", (2_980, 2_980)), ("qrinfo", (68, 68))]
    );
}

const CORPUS_TSV_HEADER: &str = "# Provenance: extracted by regenerate_corpus_cases_from_json5 from test-vectors/p2p-decode/*.json5, corpora copied from base-sdk fork 29e01c834d3eb5d647db7eb51578350a77593847.\n# source|command|payload (hex, or sha256:<digest>:<length> read from the source file for payloads over 4096 bytes)\n";

#[test]
#[ignore = "rewrites the checked-in corpus manifest from the json5 corpora"]
fn regenerate_corpus_cases_from_json5() {
    let mut output = String::from(CORPUS_TSV_HEADER);
    for file in corpus_files() {
        for (name, bytes) in &file.rows {
            output.push_str(&format!("{}|{name}|{}\n", file.name, payload_key(bytes)));
        }
    }
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/p2p-decode/corpus_cases.tsv"),
        output,
    )
    .unwrap();
}

#[test]
fn corpus_cases_match_the_json5_corpora() {
    use std::collections::BTreeMap;
    let tsv = include_str!("../../test-vectors/p2p-decode/corpus_cases.tsv");
    assert!(tsv.starts_with(CORPUS_TSV_HEADER));
    let mut manifest = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for line in tsv.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let [source, name, key] = line.split('|').collect::<Vec<_>>().try_into().unwrap();
        manifest.entry(source).or_default().push((name, key));
    }
    let mut skipped = [0usize; CORPUS_SKIPS.len()];
    for file in corpus_files() {
        let rows =
            file.rows.iter().map(|(name, bytes)| (*name, payload_key(bytes))).collect::<Vec<_>>();
        let listed = manifest.remove(file.name.as_str()).unwrap_or_default();
        let listed = listed.iter().map(|(name, key)| (*name, key.to_string())).collect::<Vec<_>>();
        let missing = rows.iter().filter(|row| !listed.contains(row)).collect::<Vec<_>>();
        let stale = listed.iter().filter(|row| !rows.contains(row)).collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "{}: payloads missing from corpus_cases.tsv: {missing:?}",
            file.name
        );
        assert!(
            stale.is_empty(),
            "{}: corpus_cases.tsv rows not in the file: {stale:?}",
            file.name
        );
        assert_eq!(listed.len(), rows.len(), "{}: row counts differ", file.name);
        assert_eq!(
            file.rows.len() + file.skips.len(),
            file.payload_fields,
            "{}: rows plus skips must equal the payload fields in the file",
            file.name
        );
        for &skip in &file.skips {
            skipped[skip] += 1;
        }
    }
    assert!(manifest.is_empty(), "corpus_cases.tsv names missing files: {:?}", manifest.keys());
    for ((file, path, command, count, reason), seen) in CORPUS_SKIPS.iter().zip(skipped) {
        assert_eq!(seen, *count, "skip {file} {path} {command} ({reason})");
    }
}

#[test]
fn goldens_match_node_reader() {
    for line in include_str!("../../test-vectors/p2p-decode/goldens.txt").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let [name, version, payload, verdict, consumed, expected_error, expected_message] =
            line.split('|').collect::<Vec<_>>().try_into().unwrap();
        let bytes = payload_bytes(payload);
        let result =
            read::decode(&command(name), &bytes, &ctx(version.parse().unwrap(), BlsCtx::BASIC));
        match (verdict, result) {
            ("ok", Ok(decoded)) => {
                assert_eq!(decoded.consumed, consumed.parse::<usize>().unwrap());
                assert_eq!(expected_error, "ok");
                assert_message_repr(&new_debug(decoded.msg), expected_message);
            }
            ("error", Err(error)) => {
                assert_eq!(consumed, "0");
                assert_eq!(fail(error), expected_error);
                assert_eq!(expected_message, "none");
            }
            (verdict, result) => panic!("golden {verdict} disagreed: {result:?}"),
        }
    }
}

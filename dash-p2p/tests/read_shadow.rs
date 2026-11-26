//! Differential coverage for the dormant node reader.

use dash_consensus::primitives::{
    CommitmentBls, MnListDiffBls, P2pCommitment, P2pMnListDiff, P2pQrInfo,
};
use dash_p2p::{CommandString, read};
use dash_p2p_core::{
    MnListDiff, MnListFormat, P2pDecodeError, P2pMsg, ProtoCtx, ProtocolVersion, QrInfo,
};
use dash_pkc::bls::BlsCtx;
use dash_primitives::Commitment;
use dash_types::codec::{BaseCodec, DecodeError};
use proptest::prelude::*;
use read::HandlerRead;

fn command(name: &str) -> CommandString {
    let mut bytes = [0; 12];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    CommandString::from_bytes(bytes)
}

fn ctx(version: i32, bls: BlsCtx) -> read::PeerCtx {
    read::PeerCtx::new(ProtocolVersion::new(version), bls)
}

fn fail(e: read::ReadFail) -> String {
    match &e {
        read::ReadFail::VersionTail {
            head,
            ..
        }
        | read::ReadFail::Decode(dash_p2p_core::P2pDecodeError::VersionTail {
            head,
            ..
        }) => format!("{e} head={head:?}"),
        _ => e.to_string(),
    }
}

fn old_fail(e: dash_p2p_core::P2pDecodeError) -> String {
    match &e {
        dash_p2p_core::P2pDecodeError::VersionTail {
            head,
            ..
        } => format!("{e} head={head:?}"),
        _ => e.to_string(),
    }
}

fn new_debug(msg: read::Msg) -> String {
    match msg {
        read::Msg::Known(msg) => format!("{msg:?}"),
        read::Msg::Unknown {
            command,
            payload,
        } => {
            format!(
                "{:?}",
                P2pMsg::Unknown {
                    command,
                    payload
                }
            )
        }
    }
}

fn message_repr(debug: &str) -> String {
    if debug.len() <= 4096 && !debug.contains('|') {
        format!("debug:{debug}")
    } else {
        format!("sha256:{}", hex_bytes(&dash_sha256::sha256(debug.as_bytes())))
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn assert_message_repr(actual: &str, expected: &str) {
    if let Some(debug) = expected.strip_prefix("debug:") {
        assert_eq!(actual, debug);
    } else if let Some(hash) = expected.strip_prefix("sha256:") {
        assert_eq!(hex_bytes(&dash_sha256::sha256(actual.as_bytes())), hash);
    } else {
        panic!("invalid golden message representation: {expected}");
    }
}

const SHADOW_VERSIONS: [i32; 10] =
    [70221, 70225, 70228, 70229, 70230, 70234, 70235, 70240, 70241, 70242];
const MN_LIST_VERSIONS: [i32; 7] = [70221, 70224, 70225, 70227, 70228, 70229, 70230];

fn shadow_at(version: i32, cmd: CommandString, payload: &[u8], bls: BlsCtx) {
    let old = P2pMsg::decode_with_policy(
        &cmd,
        payload,
        &ProtoCtx::new(ProtocolVersion::new(version), bls),
    );
    let new = read::decode(&cmd, payload, &ctx(version, bls));
    let old = old.map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(old_fail);
    let new = new.map(|d| (new_debug(d.msg), d.consumed)).map_err(fail);
    assert_eq!(old, new, "command={cmd:?} payload={payload:02x?}");
}

fn shadow(cmd: CommandString, payload: &[u8], bls: BlsCtx) {
    shadow_at(70242, cmd, payload, bls);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("P2P_SHADOW_CASES").ok().and_then(|value| value.parse().ok()).unwrap_or(100_000), .. ProptestConfig::default() })]
    #[test]
    fn randomized_shadow(cmd_index in 0usize..74, version in prop_oneof![Just(70221), Just(70222), Just(70223), Just(70224), Just(70225), Just(70226), Just(70227), Just(70228), Just(70229), Just(70230), Just(70233), Just(70234), Just(70235), Just(70240), Just(70241), Just(70242)], payload in proptest::collection::vec(any::<u8>(), 0..256), bls_index in 0u8..2) {
        let cmd = dash_p2p::policy::ALL_NET_MESSAGE_TYPES[cmd_index];
        let bls = if bls_index == 0 { BlsCtx::BASIC } else { BlsCtx::LEGACY };
        let old = P2pMsg::decode_with_policy(&cmd, &payload, &ProtoCtx::new(ProtocolVersion::new(version), bls)).map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(old_fail);
        let new = read::decode(&cmd, &payload, &ctx(version, bls)).map(|d| (new_debug(d.msg), d.consumed)).map_err(fail);
        prop_assert_eq!(old, new);
    }
}

#[test]
fn addrv2_rows_use_library_entry_decode() {
    for payload in [
        "00",
        "01 61bc6649 00 01 04 01020304 270f",
        "02 61bc6649 00 02 10 00000000000000000000000000000001 0000 79627683 01 02 10 00000000000000000000000000000001 00f1",
    ] {
        let bytes: Vec<u8> = payload
            .split_whitespace()
            .flat_map(|b| {
                b.as_bytes()
                    .chunks(2)
                    .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
            })
            .collect();
        shadow(command("addrv2"), &bytes, BlsCtx::BASIC);
        shadow(command("addrv2"), &bytes, BlsCtx::LEGACY);
        let mut cursor = bytes.as_slice();
        let decoded =
            dash_p2p_core::AddrV2Msg::read(&mut cursor, &ctx(70242, BlsCtx::BASIC)).unwrap();
        assert_eq!(cursor.len(), 0);
        assert_eq!(decoded.addrs().len(), bytes[0] as usize);
    }
}

#[test]
fn bls_carrying_commands_are_shadowed_under_both_schemes() {
    for name in [
        "qfcommit",
        "govobjvote",
        "mnlistdiff",
        "qdata",
        "qsigrec",
        "clsig",
        "isdlock",
        "mnauth",
        "platformban",
        "qcontrib",
        "qcomplaint",
        "qjustify",
        "qpcommit",
        "qsigsesann",
        "qsigsinv",
        "qgetsigs",
        "qbsigs",
        "qsigshare",
    ] {
        for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            shadow(command(name), &[], bls);
            shadow(command(name), &[0; 128], bls);
        }
    }
}

#[test]
fn every_handler_bound_is_pinned_and_exercised_at_edges() {
    assert_eq!(
        dash_p2p::limits::MAX_HEADERS_UNCOMPRESSED_RESULT,
        dash_p2p_core::limits::MAX_HEADERS_UNCOMPRESSED_RESULT
    );
    assert_eq!(
        dash_p2p::limits::MAX_HEADERS_COMPRESSED_RESULT,
        dash_p2p_core::limits::MAX_HEADERS_COMPRESSED_RESULT
    );
    assert_eq!(dash_p2p_core::MAX_SIG_SES_ANNS, 100);
    assert_eq!(dash_p2p_core::MAX_SIG_SHARES_INVS, 200);
    assert_eq!(dash_p2p_core::MAX_SIG_SHARES, 32);
    assert_eq!(dash_p2p_core::MAX_BATCHED_SIGS, 400);
    assert_eq!(dash_p2p_core::QuorumDataRequest::SIZE, 67);
    assert_eq!(1usize << 20, 1_048_576);

    for (name, max) in [
        ("headers", 2_000u64),
        ("headers2", 8_000),
        ("qsigsesann", 100),
        ("qsigsinv", 200),
        ("qgetsigs", 200),
        ("qbsigs", 400),
        ("qsigshare", 32),
    ] {
        for count in [max, max + 1] {
            let mut payload = Vec::new();
            dash_types::CompactSize::from(count).encode(&mut payload);
            shadow(command(name), &payload, BlsCtx::BASIC);
        }
    }
    for (version, max) in [(70240, 2_000u64), (70241, 8_000u64)] {
        for count in [max, max + 1] {
            let mut payload = Vec::new();
            dash_types::CompactSize::from(count).encode(&mut payload);
            let cmd = command("headers2");
            let old = P2pMsg::decode_with_policy(
                &cmd,
                &payload,
                &ProtoCtx::new(ProtocolVersion::new(version), BlsCtx::BASIC),
            );
            let new = read::decode(&cmd, &payload, &ctx(version, BlsCtx::BASIC));
            let old = old.map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(old_fail);
            let new = new.map(|d| (new_debug(d.msg), d.consumed)).map_err(fail);
            assert_eq!(old, new);
        }
    }
    for size in [67usize, 68, 1 << 20, (1 << 20) + 1] {
        let name = if size <= 68 {
            "qgetdata"
        } else {
            "qcontrib"
        };
        shadow(command(name), &vec![0; size], BlsCtx::BASIC);
    }
}

#[test]
fn sigshare_count_checks_bytes_before_entry_decode() {
    let payload = [1u8];
    let cmd = command("qsigsesann");
    let old = P2pMsg::decode_with_policy(
        &cmd,
        &payload,
        &ProtoCtx::new(ProtocolVersion::new(70230), BlsCtx::BASIC),
    );
    let new = read::decode(&cmd, &payload, &ctx(70230, BlsCtx::BASIC));
    assert_eq!(
        old.as_ref().map_err(|e| e.to_string()),
        Err("compact size value 1 exceeds limit 0".to_owned())
    );
    assert!(matches!(
        new,
        Err(read::ReadFail::Decode(dash_p2p_core::P2pDecodeError::Consensus(
            dash_types::codec::DecodeError::CompactSizeExceedsLimit {
                limit: 0,
                value: 1
            }
        )))
    ));
}

fn batched_sigshares_payload(counts: &[u64]) -> Vec<u8> {
    let mut payload = Vec::new();
    dash_types::CompactSize::from(counts.len() as u64).encode(&mut payload);
    for &count in counts {
        payload.push(0);
        dash_types::CompactSize::from(count).encode(&mut payload);
        for member in 0..count {
            (member as u16).encode(&mut payload);
            payload.extend_from_slice(&[0; 96]);
        }
    }
    payload
}

#[test]
fn qbsigs_checks_the_total_share_bound() {
    for counts in [[200, 200].as_slice(), [200, 201].as_slice(), [401].as_slice()] {
        let payload = batched_sigshares_payload(counts);
        let cmd = command("qbsigs");
        let old = P2pMsg::decode_with_policy(
            &cmd,
            &payload,
            &ProtoCtx::new(ProtocolVersion::new(70242), BlsCtx::BASIC),
        )
        .map(|d| (format!("{:?}", d.msg), d.consumed))
        .map_err(old_fail);
        let new = read::decode(&cmd, &payload, &ctx(70242, BlsCtx::BASIC))
            .map(|d| (new_debug(d.msg), d.consumed))
            .map_err(fail);
        assert_eq!(old, new);
        if counts == [200, 200] {
            assert!(old.is_ok());
        } else if counts == [200, 201] {
            assert_eq!(old, Err("qbsigs lists 401 entries, max 400".to_owned()));
        } else {
            assert_eq!(old, Err("compact size value 401 exceeds limit 400".to_owned()));
        }
    }
}

#[test]
fn corpus_shadow_smoke_covers_all_commands_and_versions() {
    let commands = [
        "version",
        "verack",
        "addr",
        "addrv2",
        "sendaddrv2",
        "inv",
        "getdata",
        "notfound",
        "getheaders",
        "headers",
        "getheaders2",
        "headers2",
        "govsync",
        "govobj",
        "govobjvote",
        "getmnlistd",
        "mnlistdiff",
        "senddsq",
        "qsendrecsigs",
        "block",
        "tx",
        "getblocks",
        "filterload",
        "filteradd",
        "sendcmpct",
        "cmpctblock",
        "getblocktxn",
        "blocktxn",
        "getcfilters",
        "getcfheaders",
        "getcfcheckpt",
        "merkleblock",
        "cfilter",
        "cfheaders",
        "cfcheckpt",
        "sendtxrcncl",
        "ssc",
        "spork",
        "qfcommit",
        "govobjvote",
        "mnlistdiff",
        "qdata",
        "qsigrec",
        "qgetdata",
        "qdata",
        "clsig",
        "isdlock",
        "mnauth",
        "getqrinfo",
        "qrinfo",
        "platformban",
        "qcontrib",
        "qcomplaint",
        "qjustify",
        "qpcommit",
        "qsigsesann",
        "qsigsinv",
        "qgetsigs",
        "qbsigs",
        "qsigshare",
        "dsa",
        "dsi",
        "dsf",
        "dss",
        "dsc",
        "dssu",
        "dstx",
        "dsq",
        "verack",
        "getaddr",
        "sendheaders",
        "sendheaders2",
        "filterclear",
        "mempool",
        "qwatch",
        "getsporks",
    ];
    for version in [70230, 70233, 70234, 70240, 70241, 70242] {
        for name in commands {
            let cmd = command(name);
            for len in [0usize, 1, 8, 32] {
                let payload = vec![len as u8; len];
                for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
                    let old = P2pMsg::decode_with_policy(
                        &cmd,
                        &payload,
                        &ProtoCtx::new(ProtocolVersion::new(version), bls),
                    );
                    let new = read::decode(&cmd, &payload, &ctx(version, bls));
                    let old = old.map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(old_fail);
                    let new = new.map(|d| (new_debug(d.msg), d.consumed)).map_err(fail);
                    assert_eq!(old, new);
                }
            }
        }
    }
}

#[test]
fn every_manifest_corpus_case_is_shadowed() {
    let mut cases = 0usize;
    let mut runs = 0usize;
    let mut differences = Vec::new();
    for CorpusCase {
        source,
        name,
        key,
        bytes,
    } in corpus_cases()
    {
        cases += 1;
        let bls_values = if matches!(
            name,
            "qfcommit"
                | "govobjvote"
                | "mnlistdiff"
                | "qdata"
                | "qsigrec"
                | "clsig"
                | "isdlock"
                | "mnauth"
                | "platformban"
                | "qcontrib"
                | "qcomplaint"
                | "qjustify"
                | "qpcommit"
                | "qsigsesann"
                | "qsigsinv"
                | "qgetsigs"
                | "qbsigs"
                | "qsigshare"
        ) {
            [BlsCtx::BASIC, BlsCtx::LEGACY].as_slice()
        } else {
            [BlsCtx::BASIC].as_slice()
        };
        for &version in &SHADOW_VERSIONS {
            for &bls in bls_values {
                runs += 1;
                let cmd = command(name);
                let old = P2pMsg::decode_with_policy(
                    &cmd,
                    &bytes,
                    &ProtoCtx::new(ProtocolVersion::new(version), bls),
                )
                .map(|d| (format!("{:?}", d.msg), d.consumed))
                .map_err(old_fail);
                let new = read::decode(&cmd, &bytes, &ctx(version, bls))
                    .map(|d| (new_debug(d.msg), d.consumed))
                    .map_err(fail);
                if old != new {
                    differences.push(format!(
                        "{source}:{name}:{version}:{bls:?}:len={}:{key}: old={old:?} new={new:?}",
                        bytes.len()
                    ));
                }
            }
        }
    }
    eprintln!("corpus cases {cases}, differential runs {runs}, differences {}", differences.len());
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert_eq!(cases, 2_914);
    assert_eq!(runs, 38_200);
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

const INLINE_PAYLOAD_MAX: usize = 4096;

/// Keys that hold a payload in the corpora.
const PAYLOAD_FIELDS: [&str; 3] = ["raw", "payload", "frame"];

/// Top-level sections whose entries carry no `command` key but are all one message type.
const SECTION_COMMANDS: [(&str, &str, &str); 5] = [
    ("core_headers2.json5", "capture", "headers2"),
    ("core_headers2.json5", "networks", "headers2"),
    ("headers2.json5", "uncompress", "headers2"),
    ("headers2.json5", "reply", "headers2"),
    ("headers2.json5", "announce", "headers2"),
];

/// Payload entries left out of the differential: file, key path (indices as `[]`), the entry's
/// `command` value (`*` for any), how many, and why.
const CORPUS_SKIPS: [(&str, &str, &str, usize, &str); 6] = [
    (
        "core_captures.json5",
        "messages[].raw",
        "reject",
        2,
        "no reject message type in Core, so an unknown command",
    ),
    ("core_overlay.json5", "cases[].raw", "smlentry", 50, "a masternode list entry, not a message"),
    ("core_overlay.json5", "cases[].raw", "qsnapshot", 7, "a quorum snapshot, not a message"),
    ("core_verdicts.json5", "framing[].frame", "*", 6, "a whole V1 frame, for the transport tests"),
    (
        "decode_domain.json5",
        "v1_commands[].raw",
        "*",
        6,
        "invalid V1 command bytes, for the transport tests",
    ),
    (
        "decode_domain.json5",
        "ignored_payload[].raw",
        "bs4aprobe",
        1,
        "an unknown command, covered by read_class_d",
    ),
];

struct CorpusCase {
    source: &'static str,
    name: &'static str,
    key: &'static str,
    bytes: Vec<u8>,
}

fn corpus_cases() -> Vec<CorpusCase> {
    include_str!("../../test-vectors/p2p-decode/corpus_cases.tsv")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let [source, name, key] = line.split('|').collect::<Vec<_>>().try_into().unwrap();
            CorpusCase {
                source,
                name,
                key,
                bytes: payload_bytes(key),
            }
        })
        .collect()
}

fn payload_key(bytes: &[u8]) -> String {
    if bytes.len() <= INLINE_PAYLOAD_MAX {
        hex_bytes(bytes)
    } else {
        format!("sha256:{}:{}", hex_bytes(&dash_sha256::sha256(bytes)), bytes.len())
    }
}

/// The bytes behind a manifest or golden payload column.
fn payload_bytes(key: &str) -> Vec<u8> {
    use std::collections::HashMap;
    static LONG: std::sync::OnceLock<HashMap<String, Vec<u8>>> = std::sync::OnceLock::new();
    if !key.starts_with("sha256:") {
        return unhex(key);
    }
    let long = LONG.get_or_init(|| {
        corpus_files()
            .iter()
            .flat_map(|file| file.rows.iter().map(|(_, bytes)| bytes.clone()))
            .chain(old_layout_rows().into_iter().map(|(_, _, bytes)| bytes))
            .filter(|bytes| bytes.len() > INLINE_PAYLOAD_MAX)
            .map(|bytes| (payload_key(&bytes), bytes))
            .collect()
    });
    long.get(key).unwrap_or_else(|| panic!("no corpus payload for {key}")).clone()
}

fn unhex(payload: &str) -> Vec<u8> {
    (0..payload.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&payload[i..i + 2], 16).unwrap())
        .collect()
}

enum Json5 {
    Object(Vec<(String, Json5)>),
    Array(Vec<Json5>),
    String(String),
    Integer(i128),
    Other,
}

struct Json5Parser<'a> {
    text: &'a [u8],
    pos: usize,
}

impl Json5Parser<'_> {
    fn skip_blank(&mut self) {
        loop {
            match self.text.get(self.pos..) {
                Some([b'/', b'/', ..]) => {
                    while self.pos < self.text.len() && self.text[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                Some([b'/', b'*', ..]) => {
                    let end = self.text[self.pos + 2..]
                        .windows(2)
                        .position(|w| w == b"*/")
                        .expect("unterminated comment");
                    self.pos += end + 4;
                }
                Some([b, ..]) if b.is_ascii_whitespace() => self.pos += 1,
                _ => return,
            }
        }
    }

    fn peek(&mut self) -> u8 {
        self.skip_blank();
        *self.text.get(self.pos).expect("unexpected end of json5")
    }

    fn word(&mut self) -> &str {
        let start = self.pos;
        while self.text.get(self.pos).is_some_and(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'+' | b'-' | b'.')
        }) {
            self.pos += 1;
        }
        assert!(self.pos > start, "unexpected byte at {start}");
        std::str::from_utf8(&self.text[start..self.pos]).unwrap()
    }

    fn string(&mut self) -> String {
        let quote = self.text[self.pos];
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let b = self.text[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    let e = self.text[self.pos];
                    self.pos += 1;
                    let mut hex = |n: usize| {
                        let v = u32::from_str_radix(
                            std::str::from_utf8(&self.text[self.pos..self.pos + n]).unwrap(),
                            16,
                        )
                        .unwrap();
                        self.pos += n;
                        v
                    };
                    let c = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'v' => '\u{b}',
                        b'0' => '\0',
                        b'u' => char::from_u32(hex(4)).unwrap_or('\u{fffd}'),
                        b'x' => char::from_u32(hex(2)).unwrap(),
                        b'\n' => continue,
                        other => other as char,
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                b if b == quote => return String::from_utf8(out).unwrap(),
                b => out.push(b),
            }
        }
    }

    fn value(&mut self) -> Json5 {
        match self.peek() {
            b'{' => {
                self.pos += 1;
                let mut fields = Vec::new();
                while self.peek() != b'}' {
                    let key = match self.peek() {
                        b'"' | b'\'' => self.string(),
                        _ => self.word().to_owned(),
                    };
                    assert_eq!(self.peek(), b':', "expected ':' at {}", self.pos);
                    self.pos += 1;
                    fields.push((key, self.value()));
                    if self.peek() == b',' {
                        self.pos += 1;
                    }
                }
                self.pos += 1;
                Json5::Object(fields)
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != b']' {
                    items.push(self.value());
                    if self.peek() == b',' {
                        self.pos += 1;
                    }
                }
                self.pos += 1;
                Json5::Array(items)
            }
            b'"' | b'\'' => Json5::String(self.string()),
            _ => {
                let word = self.word();
                let (negative, digits) = match word.as_bytes()[0] {
                    b'-' => (true, &word[1..]),
                    b'+' => (false, &word[1..]),
                    _ => (false, word),
                };
                let value = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
                    Some(hex) => i128::from_str_radix(hex, 16).ok(),
                    None if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                        digits.parse().ok()
                    }
                    None => None,
                };
                value.map_or(Json5::Other, |v| {
                    Json5::Integer(if negative {
                        -v
                    } else {
                        v
                    })
                })
            }
        }
    }

    fn values(text: &str) -> Vec<Json5> {
        let mut parser = Json5Parser {
            text: text.as_bytes(),
            pos: 0,
        };
        let mut values = Vec::new();
        loop {
            parser.skip_blank();
            if parser.pos == parser.text.len() {
                return values;
            }
            values.push(parser.value());
        }
    }
}

fn corpus_command(value: Option<&Json5>) -> Option<&'static str> {
    let Some(Json5::String(name)) = value else {
        return None;
    };
    dash_p2p::policy::ALL_NET_MESSAGE_TYPES
        .iter()
        .map(|cmd| cmd.as_str())
        .find(|known| known == name)
}

fn hex_string(hex: &str) -> Option<Vec<u8>> {
    (hex.len().is_multiple_of(2) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| unhex(hex))
}

/// A payload as the corpora write it: one hex string, or (over 4 KiB) a list of hex strings and
/// `{"b": "<hex byte>", "n": <count>}` runs. Integer bytes and integer `b` are accepted too.
fn corpus_payload(value: &Json5) -> Option<Vec<u8>> {
    let byte = |value: &Json5| match value {
        Json5::Integer(b @ 0..=255) => Some(*b as u8),
        Json5::String(hex) if hex.len() == 2 => hex_string(hex).map(|b| b[0]),
        _ => None,
    };
    match value {
        Json5::String(hex) => hex_string(hex),
        Json5::Array(items) => {
            let mut bytes = Vec::new();
            for item in items {
                match item {
                    Json5::String(hex) => bytes.extend(hex_string(hex)?),
                    Json5::Integer(_) => bytes.push(byte(item)?),
                    Json5::Object(fields)
                        if fields.len() == 2
                            && fields.iter().any(|(k, _)| k == "b")
                            && fields.iter().any(|(k, _)| k == "n") =>
                    {
                        let field = |name: &str| {
                            fields.iter().find(|(k, _)| k == name).map(|(_, v)| v).unwrap()
                        };
                        let Json5::Integer(n) = field("n") else {
                            return None;
                        };
                        let b = byte(field("b"))?;
                        bytes.extend(std::iter::repeat_n(b, usize::try_from(*n).ok()?));
                    }
                    _ => return None,
                }
            }
            Some(bytes)
        }
        _ => None,
    }
}

struct CorpusFile {
    name: String,
    rows: Vec<(&'static str, Vec<u8>)>,
    /// Index into `CORPUS_SKIPS` of each entry left out.
    skips: Vec<usize>,
    /// Payload fields in the parsed file, counted without decoding them.
    payload_fields: usize,
}

fn count_payload_fields(value: &Json5) -> usize {
    match value {
        Json5::Object(fields) => fields
            .iter()
            .map(|(key, child)| {
                usize::from(PAYLOAD_FIELDS.contains(&key.as_str())) + count_payload_fields(child)
            })
            .sum(),
        Json5::Array(items) => items.iter().map(count_payload_fields).sum(),
        _ => 0,
    }
}

/// Collects every payload entry under `value` as a row or a listed skip, and panics on any other.
fn collect_corpus_rows(
    file: &mut CorpusFile,
    value: &Json5,
    path: &str,
    inherited: Option<&'static str>,
) {
    match value {
        Json5::Object(fields) => {
            let field = |name: &str| fields.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v);
            let declared = match field("command").or(field("cmd")) {
                Some(Json5::String(name)) => Some(name.as_str()),
                _ => None,
            };
            let cmd = match declared {
                Some(name) => corpus_command(Some(&Json5::String(name.to_owned()))),
                None => corpus_command(field("type")).or(inherited),
            };
            for key in PAYLOAD_FIELDS {
                let Some(payload) = field(key) else {
                    continue;
                };
                let entry = format!("{path}.{key}");
                let pattern = entry.trim_start_matches('.').to_owned();
                let mut pattern_chars = pattern.chars().peekable();
                let mut normalized = String::new();
                while let Some(c) = pattern_chars.next() {
                    normalized.push(c);
                    if c == '[' {
                        while pattern_chars.next_if(char::is_ascii_digit).is_some() {}
                    }
                }
                if let Some(skip) = CORPUS_SKIPS.iter().position(|(f, p, c, _, _)| {
                    *f == file.name && *p == normalized && (*c == "*" || Some(*c) == declared)
                }) {
                    file.skips.push(skip);
                    continue;
                }
                let name = &file.name;
                let cmd = cmd.unwrap_or_else(|| {
                    panic!("{name}: {entry}: no message type for {declared:?}; name it or list it in CORPUS_SKIPS")
                });
                let bytes = corpus_payload(payload)
                    .unwrap_or_else(|| panic!("{name}: {entry}: payload is not in a corpus form"));
                file.rows.push((cmd, bytes));
            }
            for (key, child) in fields {
                if PAYLOAD_FIELDS.contains(&key.as_str()) {
                    continue;
                }
                let section = SECTION_COMMANDS
                    .iter()
                    .find(|(f, k, _)| path.is_empty() && *f == file.name && k == key)
                    .map(|(_, _, cmd)| *cmd);
                let named = corpus_command(Some(&Json5::String(key.clone()))).or(section);
                collect_corpus_rows(file, child, &format!("{path}.{key}"), named.or(cmd));
            }
        }
        Json5::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_corpus_rows(file, item, &format!("{path}[{index}]"), inherited);
            }
        }
        _ => {}
    }
}

fn corpus_files() -> &'static [CorpusFile] {
    static FILES: std::sync::OnceLock<Vec<CorpusFile>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/p2p-decode");
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".json5"))
            .collect::<Vec<_>>();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let text = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
                let values = Json5Parser::values(&text);
                let mut file = CorpusFile {
                    name,
                    rows: Vec::new(),
                    skips: Vec::new(),
                    payload_fields: values.iter().map(count_payload_fields).sum(),
                };
                let single = values.len() == 1;
                for (index, value) in values.iter().enumerate() {
                    let path = if single {
                        String::new()
                    } else {
                        format!("[{index}]")
                    };
                    collect_corpus_rows(&mut file, value, &path, None);
                }
                file
            })
            .collect()
    })
}

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

#[test]
#[ignore = "rewrites the checked-in goldens from the pinned old decoder"]
fn regenerate_goldens_from_old_decoder() {
    let mut output = String::from(
        "# Provenance: generated from the pinned base-sdk fork 29e01c834d3eb5d647db7eb51578350a77593847.\n# command|version|payload (as in corpus_cases.tsv)|verdict|consumed|error|message\n",
    );
    let mut rows = Vec::new();
    for case in corpus_cases() {
        rows.push((case.name, 70242, case.key.to_owned()));
        if matches!(case.name, "mnlistdiff" | "qrinfo") {
            for &version in &MN_LIST_VERSIONS {
                rows.push((case.name, version, case.key.to_owned()));
            }
        }
    }
    for (name, _, payload) in old_layout_rows() {
        for &version in MN_LIST_VERSIONS.iter().chain(&[70242]) {
            rows.push((name, version, payload_key(&payload)));
        }
    }
    for (name, version, payload) in rows {
        let bytes = payload_bytes(&payload);
        let old = P2pMsg::decode_with_policy(
            &command(name),
            &bytes,
            &ProtoCtx::new(ProtocolVersion::new(version), BlsCtx::BASIC),
        );
        match old {
            Ok(decoded) => {
                let debug = format!("{:?}", decoded.msg);
                output.push_str(&format!(
                    "{name}|{version}|{payload}|ok|{}|ok|{}\n",
                    decoded.consumed,
                    message_repr(&debug)
                ));
            }
            Err(error) => output.push_str(&format!(
                "{name}|{version}|{payload}|error|0|{}|none\n",
                old_fail(error)
            )),
        }
    }
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/p2p-decode/goldens.txt"),
        output,
    )
    .unwrap();
}

#[test]
fn class_d_handler_reads_keep_the_fork_boundaries() {
    for (name, max) in [
        ("qsigsesann", 100u64),
        ("qsigsinv", 200),
        ("qgetsigs", 200),
        ("qbsigs", 400),
        ("qsigshare", 32),
    ] {
        for count in [max, max + 1] {
            let mut payload = Vec::new();
            dash_types::CompactSize::from(count).encode(&mut payload);
            let result = read::decode(&command(name), &payload, &ctx(70242, BlsCtx::BASIC));
            let old = P2pMsg::decode_with_policy(
                &command(name),
                &payload,
                &ProtoCtx::new(ProtocolVersion::new(70242), BlsCtx::BASIC),
            );
            assert_eq!(
                old.map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(old_fail),
                result.map(|d| (format!("{:?}", d.msg), d.consumed)).map_err(fail)
            );
        }
    }
}

#[test]
fn version_tail_errors_compare_the_decoded_head() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&70242i32.to_le_bytes());
    payload.extend_from_slice(&5u64.to_le_bytes());
    payload.extend_from_slice(&1_700_000_000i64.to_le_bytes());
    payload.extend_from_slice(&1u64.to_le_bytes());
    payload.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4, 0x27, 0x0f]);
    payload.push(0x01);
    let cmd = command("version");
    let old = P2pMsg::decode_with_policy(
        &cmd,
        &payload,
        &ProtoCtx::new(ProtocolVersion::new(70242), BlsCtx::BASIC),
    )
    .map(|d| (format!("{:?}", d.msg), d.consumed))
    .map_err(old_fail);
    let new = read::decode(&cmd, &payload, &ctx(70242, BlsCtx::BASIC))
        .map(|d| (new_debug(d.msg), d.consumed))
        .map_err(fail);
    assert_eq!(old, new);
    let error = old.unwrap_err();
    assert!(error.starts_with("version after addr_recv: "), "{error}");
    assert!(error.contains(" head=Version"), "{error}");
}

const MN_LIST_LAYOUTS: [(MnListFormat, &[i32]); 4] = [
    (MnListFormat::Unversioned, &[70221, 70224]),
    (MnListFormat::VersionAfterCbTx, &[70225, 70227]),
    (MnListFormat::VersionedEntries, &[70228]),
    (MnListFormat::VersionFirst, &[70229]),
];

fn compact(buf: &mut Vec<u8>, count: u64) {
    dash_types::CompactSize::from(count).encode(buf);
}

fn mn_list_entry(buf: &mut Vec<u8>, with_version: bool, version: u16, evo: bool, seed: u8) {
    if with_version {
        buf.extend_from_slice(&version.to_le_bytes());
    }
    buf.extend_from_slice(&[seed; 32]);
    buf.extend_from_slice(&[seed.wrapping_add(1); 32]);
    buf.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, seed, 0x27, 0x0f]);
    buf.extend_from_slice(&[seed.wrapping_add(2); 48]);
    buf.extend_from_slice(&[seed.wrapping_add(3); 20]);
    buf.push(1);
    if version >= 2 {
        buf.extend_from_slice(&u16::from(evo).to_le_bytes());
        if evo {
            if version < 3 {
                buf.extend_from_slice(&443u16.to_le_bytes());
            }
            buf.extend_from_slice(&[seed.wrapping_add(4); 20]);
        }
    }
}

/// One `mnlistdiff` in `format`, written field by field in the fork's layout.
fn mn_list_diff_in(buf: &mut Vec<u8>, format: MnListFormat, seed: u8) {
    let diff_version = 2u16;
    if format.has_version_first() {
        buf.extend_from_slice(&diff_version.to_le_bytes());
    }
    buf.extend_from_slice(&[seed; 32]);
    buf.extend_from_slice(&[seed.wrapping_add(1); 32]);
    buf.extend_from_slice(&1u32.to_le_bytes());
    compact(buf, 1);
    buf.extend_from_slice(&[seed.wrapping_add(2); 32]);
    compact(buf, 1);
    buf.push(1);
    buf.extend_from_slice(&1i32.to_le_bytes());
    compact(buf, 0);
    compact(buf, 0);
    buf.extend_from_slice(&0u32.to_le_bytes());
    if format.has_version_after_cb_tx() {
        buf.extend_from_slice(&diff_version.to_le_bytes());
    }
    compact(buf, 1);
    buf.extend_from_slice(&[seed.wrapping_add(3); 32]);
    let with_version = format.has_entry_version();
    if with_version {
        compact(buf, 2);
        mn_list_entry(buf, true, 1, false, seed.wrapping_add(10));
        mn_list_entry(buf, true, 2, true, seed.wrapping_add(20));
    } else {
        compact(buf, 1);
        mn_list_entry(buf, false, 1, false, seed.wrapping_add(10));
    }
    compact(buf, 0);
    compact(buf, 0);
}

fn quorum_snapshot(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&1i32.to_le_bytes());
    compact(buf, 3);
    buf.push(0b101);
    compact(buf, 1);
    buf.extend_from_slice(&2i32.to_le_bytes());
}

fn old_layout_mnlistdiff(format: MnListFormat) -> Vec<u8> {
    let mut payload = Vec::new();
    mn_list_diff_in(&mut payload, format, 0x30);
    payload
}

/// One `qrinfo` whose diffs are in `format`, with the H-4C cycle and one base entry.
fn old_layout_qrinfo(format: MnListFormat) -> Vec<u8> {
    let mut payload = Vec::new();
    for _ in 0..3 {
        quorum_snapshot(&mut payload);
    }
    for seed in [0x40, 0x50, 0x60, 0x70, 0x80] {
        mn_list_diff_in(&mut payload, format, seed);
    }
    payload.push(1);
    quorum_snapshot(&mut payload);
    mn_list_diff_in(&mut payload, format, 0x90);
    compact(&mut payload, 0);
    compact(&mut payload, 1);
    quorum_snapshot(&mut payload);
    compact(&mut payload, 1);
    mn_list_diff_in(&mut payload, format, 0xa0);
    payload
}

fn old_layout_rows() -> Vec<(&'static str, MnListFormat, Vec<u8>)> {
    MN_LIST_LAYOUTS
        .iter()
        .flat_map(|&(format, _)| {
            [
                ("mnlistdiff", format, old_layout_mnlistdiff(format)),
                ("qrinfo", format, old_layout_qrinfo(format)),
            ]
        })
        .collect()
}

type Outcome = Result<(String, usize), String>;

fn old_and_new(name: &str, version: i32, payload: &[u8], bls: BlsCtx) -> (Outcome, Outcome) {
    let cmd = command(name);
    let old = P2pMsg::decode_with_policy(
        &cmd,
        payload,
        &ProtoCtx::new(ProtocolVersion::new(version), bls),
    )
    .map(|d| (format!("{:?}", d.msg), d.consumed))
    .map_err(old_fail);
    let new = read::decode(&cmd, payload, &ctx(version, bls))
        .map(|d| (new_debug(d.msg), d.consumed))
        .map_err(fail);
    (old, new)
}

#[test]
fn old_mn_list_layouts_are_accepted_at_their_versions() {
    let mut runs = 0usize;
    for (name, format, payload) in old_layout_rows() {
        let versions = MN_LIST_LAYOUTS
            .iter()
            .find(|(f, _)| *f == format)
            .map(|(_, versions)| *versions)
            .unwrap();
        for &version in versions {
            assert_eq!(MnListFormat::for_version(ProtocolVersion::new(version)), format);
            for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
                let (old, new) = old_and_new(name, version, &payload, bls);
                runs += 1;
                let (_, consumed) =
                    old.as_ref().unwrap_or_else(|e| panic!("{name} {format:?} {version}: {e}"));
                assert_eq!(*consumed, payload.len());
                assert_eq!(old, new, "{name} {format:?} at {version}");
            }
        }
        let mut data = payload.as_slice();
        if name == "mnlistdiff" {
            let diff = MnListDiff::decode_as(&mut data, format).unwrap();
            assert_eq!(diff.format(), format);
            assert_eq!(
                diff.version(),
                if format == MnListFormat::Unversioned {
                    1
                } else {
                    2
                }
            );
            assert_eq!(
                diff.mn_list().len(),
                if format.has_entry_version() {
                    2
                } else {
                    1
                }
            );
            if format.has_entry_version() {
                assert_eq!(diff.mn_list()[1].mn_type(), dash_primitives::MnType::Evo);
                assert_eq!(diff.mn_list()[1].platform_http_port(), Some(443));
            }
        } else {
            let info = QrInfo::decode_as(&mut data, format).unwrap();
            assert_eq!(info.diff_h().format(), format);
            assert!(info.cycle_h_4c().is_some());
            assert_eq!(info.mn_list_diff_list().len(), 1);
        }
        assert!(data.is_empty());
    }
    assert_eq!(runs, 2 * (2 + 2 + 1 + 1) * 2);
}

#[test]
fn old_mn_list_layouts_match_at_every_other_version() {
    let mut runs = 0usize;
    for (name, format, payload) in old_layout_rows() {
        for &version in MN_LIST_VERSIONS.iter().chain(&[70242]) {
            if MnListFormat::for_version(ProtocolVersion::new(version)) == format {
                continue;
            }
            for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
                let (old, new) = old_and_new(name, version, &payload, bls);
                runs += 1;
                assert_eq!(old, new, "{name} {format:?} at {version}");
            }
        }
    }
    assert_eq!(runs, 104);
}

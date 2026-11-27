//! Differential coverage for the node reader and the pinned decoder.

use dash_p2p::{CommandString, read};
use dash_p2p_core::{MnListDiff, MnListFormat, P2pMsg, ProtoCtx, ProtocolVersion, QrInfo};
use dash_pkc::bls::BlsCtx;
use dash_types::codec::BaseCodec;
use proptest::prelude::*;
use read::HandlerRead;

#[path = "common/read.rs"]
pub mod read_helpers;

use read_helpers::*;

fn old_fail(e: dash_p2p_core::P2pDecodeError) -> String {
    match &e {
        dash_p2p_core::P2pDecodeError::VersionTail {
            head,
            ..
        } => format!("{e} head={head:?}"),
        _ => e.to_string(),
    }
}

fn message_repr(debug: &str) -> String {
    if debug.len() <= 4096 && !debug.contains('|') {
        format!("debug:{debug}")
    } else {
        format!("sha256:{}", hex_bytes(&dash_sha256::sha256(debug.as_bytes())))
    }
}

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

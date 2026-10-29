//! The fixed cases: each command's fields at their edges, every BLS field
//! with a sample of each outcome class Core's wrapper has (pkc's
//! `corpus/bls_wrapper.json5`, Core-judged), the version-dependent types in
//! every protocol band, and the regenerated O4 fixtures. They are the fuzz
//! seeds and, judged, base-sdk's `core_overlay.json5`.

use serde_json::Value;

use crate::predict::Case;

/// The stream versions where Core's overlay layouts change, and the current.
pub const BANDS: [i32; 7] = [70221, 70225, 70227, 70228, 70229, 70230, 70242];

/// Every command the judge reads.
pub const COMMANDS: [&str; 14] = [
    "clsig",
    "isdlock",
    "qsigrec",
    "qfcommit",
    "mnauth",
    "spork",
    "platformban",
    "qgetdata",
    "qdata",
    "getqrinfo",
    "qrinfo",
    "mnlistdiff",
    "smlentry",
    "qsnapshot",
];

/// Commands whose layout depends on the stream version.
pub fn banded(cmd: &str) -> bool {
    matches!(cmd, "qrinfo" | "mnlistdiff" | "smlentry")
}

/// One sample per eager outcome class of each BLS wire type.
pub struct Bls {
    pub pks: Vec<(String, Vec<u8>)>,
    pub sigs: Vec<(String, Vec<u8>)>,
}

impl Bls {
    pub fn load(path: &str) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let json: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let v: Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{path}: {e}"));
        let samples = |kind: &str| {
            let mut seen = std::collections::BTreeSet::new();
            v[kind]["eager"]
                .as_array()
                .expect("eager section")
                .iter()
                .filter(|e| seen.insert(e["class"].as_str().expect("class").to_string()))
                .map(|e| {
                    (
                        e["class"].as_str().unwrap().to_string(),
                        hex::decode(e["in"].as_str().unwrap()).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        Self {
            pks: samples("pk"),
            sigs: samples("sig"),
        }
    }
}

fn count(n: usize) -> Vec<u8> {
    match n {
        0..=0xfc => vec![n as u8],
        0xfd..=0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        _ => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
    }
}

/// A named case at the current version and basic scheme.
fn case(cmd: &str, raw: Vec<u8>) -> Case {
    Case {
        cmd: cmd.into(),
        tag: String::new(),
        version: 70242,
        legacy: false,
        raw,
        max_a: 0,
        max_b: 0,
    }
}

/// The case under both process-wide schemes.
fn kind(evo: bool) -> &'static str {
    if evo {
        "evo"
    } else {
        "regular"
    }
}

fn tagged(tag: impl Into<String>, c: Case) -> Case {
    Case {
        tag: tag.into(),
        ..c
    }
}

fn both(c: Case) -> [Case; 2] {
    let mut l = c.clone();
    l.legacy = true;
    [c, l]
}

fn bitset(bits: usize, byte: u8) -> Vec<u8> {
    let mut out = count(bits);
    out.extend(vec![byte; bits.div_ceil(8)]);
    out
}

fn commitment(version: u16, pk: &[u8], qsig: &[u8], msig: &[u8]) -> Vec<u8> {
    let mut out = version.to_le_bytes().to_vec();
    out.push(1);
    out.extend([0x31; 32]);
    if version == 2 || version == 4 {
        out.extend(3i16.to_le_bytes());
    }
    out.extend(bitset(10, 0x01));
    out.extend(bitset(10, 0x03));
    out.extend(pk);
    out.extend([0x32; 32]);
    out.extend(qsig);
    out.extend(msig);
    out
}

/// A version 1, 2 or 3 SML entry, with or without its version.
pub fn sml_entry(version: u16, evo: bool, with_version: bool, key: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if with_version {
        out.extend(version.to_le_bytes());
    }
    out.extend([0x41; 32]);
    out.extend([0x42; 32]);
    let service = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 192, 0, 2, 1, 0x27, 0x0f];
    if version >= 3 {
        // ExtNetInfo v1: core_p2p as an ADDRV2 service, platform_https as a
        // domain.
        out.extend([1, 2, 0, 1, 1]);
        out.extend([1, 4, 192, 0, 2, 1, 0x27, 0x0f]);
        out.extend([2, 1, 2]);
        out.extend(count(16));
        out.extend(b"node.example.com");
        out.extend(443u16.to_be_bytes());
    } else {
        out.extend(service);
    }
    out.extend(key);
    out.extend([0x43; 20]);
    out.push(1);
    if with_version && version >= 2 {
        out.extend(u16::from(evo).to_le_bytes());
        if evo {
            if version < 3 {
                out.extend(26657u16.to_le_bytes());
            }
            out.extend([0x44; 20]);
        }
    }
    out
}

/// A list diff laid out for stream `version`, with `entries` and CL sigs.
pub fn mnlistdiff(version: i32, entries: &[Vec<u8>], cl_sigs: &[(&[u8], &[u16])]) -> Vec<u8> {
    let mut out = Vec::new();
    if version >= 70229 {
        out.extend(1u16.to_le_bytes());
    }
    out.extend([0x0a; 32]);
    out.extend([0x0b; 32]);
    out.extend(1u32.to_le_bytes());
    out.extend(count(1));
    out.extend([0x0c; 32]);
    out.extend([1, 1]);
    out.extend([3, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0]);
    if (70225..70229).contains(&version) {
        out.extend(1u16.to_le_bytes());
    }
    out.extend(count(0));
    out.extend(count(entries.len()));
    for e in entries {
        out.extend(e);
    }
    out.extend(count(1));
    out.push(1);
    out.extend([0x0d; 32]);
    out.extend(count(0));
    if version >= 70230 {
        out.extend(count(cl_sigs.len()));
        for (sig, idx) in cl_sigs {
            out.extend(*sig);
            out.extend(count(idx.len()));
            for i in *idx {
                out.extend(i.to_le_bytes());
            }
        }
    }
    out
}

fn snapshot(mode: i32, bits: usize, byte: u8, skips: &[i32]) -> Vec<u8> {
    let mut out = mode.to_le_bytes().to_vec();
    out.extend(bitset(bits, byte));
    out.extend(count(skips.len()));
    for s in skips {
        out.extend(s.to_le_bytes());
    }
    out
}

fn qrinfo(flag: u8, diff: &[u8]) -> Vec<u8> {
    let snap = snapshot(1, 4, 0x0f, &[2]);
    let mut out = Vec::new();
    for _ in 0..3 {
        out.extend(&snap);
    }
    for _ in 0..5 {
        out.extend(diff);
    }
    out.push(flag);
    if flag != 0 {
        out.extend(&snap);
        out.extend(diff);
    }
    out.extend(count(1));
    out.extend(commitment(2, &[0; 48], &[0; 96], &[0; 96]));
    out.extend(count(1));
    out.extend(&snap);
    out.extend(count(1));
    out.extend(diff);
    out
}

fn qgetdata(mask: u16) -> Vec<u8> {
    let mut out = vec![1];
    out.extend([0x51; 32]);
    out.extend(mask.to_le_bytes());
    out.extend([0x52; 32]);
    out
}

/// The hand-built cases.
pub fn cases(bls: &Bls) -> Vec<Case> {
    let mut out = Vec::new();
    let sigs = || bls.sigs.iter().map(|(_, s)| s.as_slice());
    let pks = || bls.pks.iter().map(|(_, s)| s.as_slice());
    let zero_sig = [0u8; 96];
    let zero_pk = [0u8; 48];

    // Eager signatures, under both schemes, and lazy ones.
    for s in sigs() {
        let mut clsig = 1_000_000i32.to_le_bytes().to_vec();
        clsig.extend([0x21; 32]);
        clsig.extend(s);
        out.extend(both(tagged("sig", case("clsig", clsig))));
        let mut mnauth = vec![0x22; 32];
        mnauth.extend(s);
        out.extend(both(tagged("sig", case("mnauth", mnauth))));
        let mut ban = vec![0x23; 32];
        ban.extend(2000i32.to_le_bytes());
        ban.extend([0x24; 32]);
        ban.extend(s);
        out.extend(both(tagged("sig", case("platformban", ban))));
        let mut rec = vec![1];
        rec.extend([0x25; 96]);
        rec.extend(s);
        out.push(tagged("sig", case("qsigrec", rec)));
        let mut isd = vec![1];
        isd.extend(count(2));
        isd.extend([0x26; 72]);
        isd.extend([0x27; 64]);
        isd.extend(s);
        out.push(tagged("sig", case("isdlock", isd)));
        // Final commitments, each BLS field in turn, every version.
        for v in 1..=4 {
            out.extend(both(tagged(
                format!("v{v} quorum_sig"),
                case("qfcommit", commitment(v, &zero_pk, s, &zero_sig)),
            )));
            out.extend(both(tagged(
                format!("v{v} members_sig"),
                case("qfcommit", commitment(v, &zero_pk, &zero_sig, s)),
            )));
        }
        // ChainLock signatures of a diff, and a repeated key.
        let diff = mnlistdiff(70242, &[], &[(s, &[0, 2, 2]), (&zero_sig, &[1]), (s, &[5])]);
        out.extend(both(tagged("cl_sig", case("mnlistdiff", diff))));
    }
    for k in pks() {
        for v in 1..=4 {
            out.extend(both(tagged(
                format!("v{v} quorum_pk"),
                case("qfcommit", commitment(v, k, &zero_sig, &zero_sig)),
            )));
        }
        for (v, evo) in [(1, false), (2, true), (3, true)] {
            out.extend(both(tagged(
                format!("v{v} {} operator_key", kind(evo)),
                case("smlentry", sml_entry(v, evo, true, k)),
            )));
        }
        // A verification vector key and a contribution's ephemeral key.
        let mut qdata = qgetdata(3);
        qdata.push(0);
        qdata.extend(count(1));
        qdata.extend(k);
        qdata.extend(count(1));
        qdata.extend(k);
        qdata.extend([0x53; 32]);
        qdata.extend(count(3));
        qdata.extend([1, 2, 3]);
        for c in both(tagged("vvec_pk contribution_pk", case("qdata", qdata))) {
            out.push(Case {
                max_a: 1,
                max_b: 1,
                ..c
            });
        }
    }
    // Two invalid keys of one map: Core keeps the first.
    let invalid: Vec<&[u8]> = bls
        .sigs
        .iter()
        .filter(|(c, _)| c.ends_with("_legacy_s0") || c.ends_with("_basic_s0") || c == "zero")
        .map(|(_, s)| s.as_slice())
        .collect();
    for pair in invalid.windows(2) {
        let diff = mnlistdiff(70242, &[], &[(pair[0], &[1]), (pair[1], &[2])]);
        out.extend(both(tagged("cl_sig invalid_pair", case("mnlistdiff", diff))));
    }

    // Protocol bands: entries of each version and type, with and without
    // their version, alone and in a diff and rotation info.
    let key = &bls.pks[0].1;
    for &v in &BANDS {
        let versioned = v >= 70228;
        // Before 70228 every entry reads as version 1.
        let kinds: &[(u16, bool)] = if versioned {
            &[(1, false), (2, false), (2, true), (3, false), (3, true)]
        } else {
            &[(1, false), (1, true)]
        };
        let entries: Vec<Vec<u8>> =
            kinds.iter().map(|&(ev, evo)| sml_entry(ev, evo, versioned, key)).collect();
        for (e, &(ev, evo)) in entries.iter().zip(kinds) {
            out.push(Case {
                version: v,
                ..tagged(format!("v{ev} {}", kind(evo)), case("smlentry", e.clone()))
            });
        }
        let diff = mnlistdiff(v, &entries, &[(&zero_sig, &[0])]);
        out.push(Case {
            version: v,
            ..tagged("band", case("mnlistdiff", diff.clone()))
        });
        // The extra share flag as 0, 1 and a byte Core reads as `true`.
        for flag in [0, 1, 2] {
            out.push(Case {
                version: v,
                ..tagged(format!("extra_share {flag}"), case("qrinfo", qrinfo(flag, &diff)))
            });
        }
        // The current layout read in an older band.
        let current = mnlistdiff(70242, &entries[..1], &[]);
        out.push(Case {
            version: v,
            ..tagged("current_layout", case("mnlistdiff", current))
        });
    }
    // isValid as a byte Core reads as `true`.
    for v in [1, 2] {
        let mut e = sml_entry(v, false, true, key);
        let at = e.len()
            - if v == 1 {
                1
            } else {
                3
            };
        e[at] = 0x80;
        out.push(tagged(format!("v{v} is_valid_0x80"), case("smlentry", e)));
    }
    for raw in [vec![], vec![2], vec![5, 0], vec![3, 0, 7, 0]] {
        let mut e = sml_entry(2, false, true, key);
        e.splice(0..2, raw.iter().copied().chain(std::iter::repeat(0)).take(2));
        out.push(tagged("entry_version_bytes", case("smlentry", e)));
    }

    // Fixed fields at their edges.
    for len in [0usize, 64, 65, 66] {
        let mut spork = 10001i32.to_le_bytes().to_vec();
        spork.extend(1i64.to_le_bytes());
        spork.extend(1_700_000_000i64.to_le_bytes());
        spork.extend(count(len));
        spork.extend(vec![0x1b; len]);
        out.push(tagged(format!("sig_len {len}"), case("spork", spork)));
    }
    let q = qgetdata(1);
    out.push(case("qgetdata", q[..66].to_vec()));
    out.push(case("qgetdata", q.clone()));
    for tail in [&[0u8][..], &[0xff], &[0, 1]] {
        out.push(case("qgetdata", [&q[..], tail].concat()));
    }
    for (tail, a, b) in [
        (vec![], 0, 0),
        (vec![0xff], 0, 0),
        (vec![3, 9, 9], 0, 0),
        (vec![7], 0, 0),
        ([vec![0], count(2), vec![0; 96]].concat(), 2, 0),
        ([vec![0], count(2), vec![0; 96]].concat(), 1, 0),
        ([vec![0], count(1), vec![0; 48]].concat(), 2, 0),
    ] {
        for mask in [0u16, 1, 2, 3] {
            out.push(Case {
                max_a: a,
                max_b: b,
                ..tagged(
                    format!("mask {mask}"),
                    case("qdata", [qgetdata(mask), tail.clone()].concat()),
                )
            });
        }
    }
    for n in [0usize, 1, 4096, 4097] {
        let mut g = count(n);
        g.extend(vec![0x61; 32 * n.min(4096) + 32]);
        g.push(1);
        out.push(tagged(format!("bases {n}"), case("getqrinfo", g)));
    }
    let mut g = count(1);
    g.extend([0x61; 64]);
    g.push(2);
    out.push(tagged("extra_share 2", case("getqrinfo", g)));
    for (mode, bits, byte) in
        [(-1, 0, 0), (0, 10, 0xff), (3, 10, 0x03), (4, 8, 0xff), (1, 10, 0xff)]
    {
        out.push(case("qsnapshot", snapshot(mode, bits, byte, &[0, -1, 7])));
    }
    for raw in [vec![], vec![0x01, 0x02, 0x00, 0x00]] {
        out.push(case("qsnapshot", raw));
    }
    out
}

/// The regenerated O4 fixtures, read at the current version.
pub fn fixtures(rdc_root: &str) -> Vec<(String, Case)> {
    let mut out = Vec::new();
    for (file, cmd) in [("mnlistdiff", "mnlistdiff"), ("qrinfo", "qrinfo")] {
        let path = format!("{rdc_root}/test-vectors/golden/{file}-core-ground-truth.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture list"))
            .expect("json");
        for r in v.as_array().expect("records") {
            let name = r["name"].as_str().expect("name").to_string();
            let raw =
                std::fs::read(format!("{rdc_root}/{}", r["repoPath"].as_str().expect("repoPath")))
                    .expect("fixture payload");
            out.push((name, tagged("fixture", case(cmd, raw))));
        }
    }
    out
}

//! The fixed cases: each command's fields at their edges, every BLS field
//! with a sample of each outcome class Core's wrapper has (pkc's
//! `corpus/bls_wrapper.json5`, Core-judged), the DKG messages against
//! several quorums' parameters at and past every bound of the handler's size
//! cap and walk, and the signature share messages at their limits, in every
//! `VARINT` width and both `AUTOBITSET` forms; and (BS9b) the CoinJoin and
//! governance messages at the versions, pool counts, booleans and
//! signature sizes their reads depend on. They are the fuzz seeds and,
//! judged, base-sdk's `core_overlay_ii.json5` and `core_overlay_iii.json5`.

use serde_json::Value;

use crate::predict::{Case, VERSION};

/// Every command the judge reads.
pub const COMMANDS: [&str; 20] = [
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
    "govobj",
    "govobjvote",
    "govsync",
];

/// Stream versions: before and at `COINJOIN_REBALANCE_VERSION`, and the
/// pin's.
pub const VERSIONS: [i32; 3] = [70240, 70241, VERSION];

/// Quorum parameters (size, minimum size, threshold): `llmq_test`,
/// `llmq_50_60`, `llmq_400_60`, a single member, and devnet overrides Core
/// does not check (zero, a threshold past the size, a size past the 1 MiB
/// ceiling). No negative counts: dash-params refuses them (quirk 46).
pub const QUORUMS: [[i32; 3]; 7] =
    [[3, 2, 2], [50, 40, 30], [400, 300, 240], [1, 1, 1], [0, 0, 0], [4, 6, 5], [8000, 1, 1]];

/// One sample per eager outcome class of each BLS wire type, and a valid one.
pub struct Bls {
    pub pks: Vec<(String, Vec<u8>)>,
    pub sigs: Vec<(String, Vec<u8>)>,
    pub sks: Vec<(String, Vec<u8>)>,
    /// A key, signature and secret key Core reads valid under the basic
    /// scheme, and under the legacy one where a sample is.
    pub valid: (Vec<u8>, Vec<u8>, Vec<u8>),
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
        let eager = |kind: &str| v[kind]["eager"].as_array().expect("eager section").clone();
        let samples = |kind: &str| {
            let mut seen = std::collections::BTreeSet::new();
            eager(kind)
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
        // "B"/"L": Core's outcome under the basic/legacy global scheme,
        // "V:<bytes>" for a valid object.
        let valid = |kind: &str| {
            let all = eager(kind);
            let read = |e: &Value, s: &str| e[s].as_str().is_some_and(|o| o.starts_with("V:"));
            let e = all
                .iter()
                .find(|e| read(e, "B") && read(e, "L"))
                .or_else(|| all.iter().find(|e| read(e, "B")))
                .unwrap_or_else(|| panic!("no valid {kind} sample"));
            hex::decode(e["in"].as_str().unwrap()).unwrap()
        };
        Self {
            pks: samples("pk"),
            sigs: samples("sig"),
            sks: samples("sk"),
            valid: (valid("pk"), valid("sig"), valid("sk")),
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

/// Core's `VARINT` of `n`.
pub fn varint(mut n: u32) -> Vec<u8> {
    let mut out = vec![(n & 0x7f) as u8];
    while n > 0x7f {
        n = (n >> 7) - 1;
        out.push((n & 0x7f) as u8 | 0x80);
    }
    out.reverse();
    out
}

fn case(cmd: &str, tag: impl Into<String>, quorum: [i32; 3], raw: Vec<u8>) -> Case {
    Case {
        cmd: cmd.into(),
        tag: tag.into(),
        legacy: false,
        raw,
        quorum,
        version: VERSION,
    }
}

/// The case under both process-wide schemes.
fn both(c: Case) -> [Case; 2] {
    let mut l = c.clone();
    l.legacy = true;
    [c, l]
}

fn prefix() -> Vec<u8> {
    let mut out = vec![100];
    out.extend([0x11; 32]);
    out.extend([0x22; 32]);
    out
}

fn bitset(bits: usize, last: u8) -> Vec<u8> {
    let mut out = count(bits);
    let len = bits.div_ceil(8);
    out.extend(vec![0x01; len]);
    if let Some(b) = out.last_mut().filter(|_| len > 0) {
        *b = last;
    }
    out
}

struct Contribution<'a> {
    vvec: Vec<&'a [u8]>,
    eph: &'a [u8],
    blobs: Vec<usize>,
    sig: &'a [u8],
}

impl Contribution<'_> {
    fn bytes(&self) -> Vec<u8> {
        let mut out = prefix();
        out.extend(count(self.vvec.len()));
        for k in &self.vvec {
            out.extend(*k);
        }
        out.extend(self.eph);
        out.extend([0x33; 32]);
        out.extend(count(self.blobs.len()));
        for &n in &self.blobs {
            out.extend(count(n));
            out.extend(vec![0x44; n]);
        }
        out.extend(self.sig);
        out
    }
}

fn complaint(bad: &[u8], complain_for: &[u8], sig: &[u8]) -> Vec<u8> {
    [prefix(), bad.to_vec(), complain_for.to_vec(), sig.to_vec()].concat()
}

fn justification(keys: &[&[u8]], sig: &[u8]) -> Vec<u8> {
    let mut out = prefix();
    out.extend(count(keys.len()));
    for (i, k) in keys.iter().enumerate() {
        out.extend((i as u32).to_le_bytes());
        out.extend(*k);
    }
    out.extend(sig);
    out
}

fn premature(valid: &[u8], pk: &[u8], qsig: &[u8], sig: &[u8]) -> Vec<u8> {
    [prefix(), valid.to_vec(), pk.to_vec(), vec![0x55; 32], qsig.to_vec(), sig.to_vec()].concat()
}

fn dkg_cases(bls: &Bls) -> Vec<Case> {
    let (pk, sig, sk) = bls.valid.clone();
    let mut out = Vec::new();
    for q in QUORUMS {
        let [size, min, threshold] = q.map(|v| usize::try_from(v).unwrap_or(0));
        if size > 400 {
            continue;
        }
        let at = |vvec: usize, blobs: usize| Contribution {
            vvec: vec![&pk[..]; vvec],
            eph: &pk,
            blobs: vec![3; blobs],
            sig: &sig,
        };
        let tag = |what: &str| format!("q{size} {what}");
        // The walk's bounds: the vector at the threshold, blobs from the
        // minimum size to the size.
        out.push(case("qcontrib", tag("ok"), q, at(threshold, min.max(1).min(size)).bytes()));
        out.push(case("qcontrib", tag("blobs min"), q, at(threshold, min).bytes()));
        out.push(case("qcontrib", tag("blobs size"), q, at(threshold, size).bytes()));
        out.push(case("qcontrib", tag("blobs over"), q, at(threshold, size + 1).bytes()));
        if min > 0 {
            out.push(case("qcontrib", tag("blobs under"), q, at(threshold, min - 1).bytes()));
        }
        out.push(case("qcontrib", tag("vvec over"), q, at(threshold + 1, size).bytes()));
        if threshold > 0 {
            out.push(case("qcontrib", tag("vvec under"), q, at(threshold - 1, size).bytes()));
        }
        let ok = at(threshold, size).bytes();
        out.push(case("qcontrib", tag("trailing"), q, [&ok[..], &[0]].concat()));
        out.push(case("qcontrib", tag("short"), q, ok[..ok.len() - 1].to_vec()));

        let bits = |n: usize| bitset(n, 0x01);
        out.push(case("qcomplaint", tag("ok"), q, complaint(&bits(size), &bits(size), &sig)));
        out.push(case(
            "qcomplaint",
            tag("differ"),
            q,
            complaint(&bits(size), &bits(size.saturating_sub(1)), &sig),
        ));
        out.push(case(
            "qcomplaint",
            tag("over"),
            q,
            complaint(&bits(size + 1), &bits(size + 1), &sig),
        ));
        out.push(case("qcomplaint", tag("empty"), q, complaint(&bits(0), &bits(0), &sig)));
        let keys = vec![&sk[..]; size];
        out.push(case("qjustify", tag("ok"), q, justification(&keys, &sig)));
        let over = vec![&sk[..]; size + 1];
        out.push(case("qjustify", tag("over"), q, justification(&over, &sig)));
        out.push(case("qpcommit", tag("ok"), q, premature(&bits(size), &pk, &sig, &sig)));
        out.push(case("qpcommit", tag("over"), q, premature(&bits(size + 1), &pk, &sig, &sig)));
    }

    // Padding bits: last bytes of 1 to 7 bits, with no padding bit, the
    // first one or the last one set, in each bitset.
    let q = [50, 40, 30];
    for bits in [1, 7, 9, 15, 50] {
        let rem = bits % 8;
        for last in [0x00u8, 1 << rem, 0x80] {
            let b = bitset(bits, last);
            let ok = bitset(bits, 0x00);
            let tag = format!("padding {bits} {last:02x}");
            out.push(case("qcomplaint", format!("{tag} bad"), q, complaint(&b, &ok, &sig)));
            out.push(case("qcomplaint", format!("{tag} for"), q, complaint(&ok, &b, &sig)));
            out.push(case("qpcommit", tag, q, premature(&b, &pk, &sig, &sig)));
        }
    }

    // The size cap: a contribution whose framing fits, at the cap and one
    // byte past it, for each quorum whose cap a framed payload can reach.
    for q in [[3, 2, 2], [50, 40, 30], [8000, 1, 1]] {
        let p = crate::predict::params(q);
        let cap = <dash_p2p_core::DkgContribution as dash_p2p_core::DkgMessage>::max_size(&p);
        for extra in [0usize, 1] {
            let mut c = Contribution {
                vvec: vec![&pk[..]; q[2] as usize],
                eph: &pk,
                blobs: vec![0; q[1] as usize],
                sig: &sig,
            };
            let base = c.bytes().len();
            // Grow the first blob (its length prefix included) to reach the
            // target size.
            let want = cap + extra;
            let mut n = want - base;
            while count(n).len() - 1 + n + base > want {
                n -= 1;
            }
            c.blobs[0] = n;
            let raw = c.bytes();
            assert_eq!(raw.len(), want, "cap seed for {q:?}");
            out.push(case("qcontrib", format!("q{} cap +{extra}", q[0]), q, raw));
        }
    }
    // The other messages' layouts are fixed by the quorum, under their caps:
    // a fitting payload padded to the cap fails the walk, one byte more the
    // cap. The large quorum also bounds their walks.
    use dash_p2p_core::{DkgComplaint, DkgJustification, DkgMessage, DkgPrematureCommitment};
    for q in [[3, 2, 2], [50, 40, 30], [400, 300, 240], [8000, 1, 1]] {
        let p = crate::predict::params(q);
        let size = q[0] as usize;
        let bits = |n: usize| bitset(n, 0x01);
        let mut fitting = vec![
            ("qcomplaint", DkgComplaint::max_size(&p), complaint(&bits(size), &bits(size), &sig)),
            (
                "qpcommit",
                DkgPrematureCommitment::max_size(&p),
                premature(&bits(size), &pk, &sig, &sig),
            ),
        ];
        if size <= 400 {
            let keys = vec![&sk[..]; size];
            fitting.push(("qjustify", DkgJustification::max_size(&p), justification(&keys, &sig)));
        } else {
            out.push(case("qcomplaint", format!("q{size} ok"), q, fitting[0].2.clone()));
            out.push(case(
                "qcomplaint",
                format!("q{size} over"),
                q,
                complaint(&bits(size + 1), &bits(size + 1), &sig),
            ));
            out.push(case("qpcommit", format!("q{size} ok"), q, fitting[1].2.clone()));
            out.push(case(
                "qpcommit",
                format!("q{size} over"),
                q,
                premature(&bits(size + 1), &pk, &sig, &sig),
            ));
        }
        for (cmd, cap, raw) in fitting {
            assert!(raw.len() < cap, "{cmd} {q:?} fits under its cap");
            for extra in [0usize, 1] {
                let mut padded = raw.clone();
                padded.resize(cap + extra, 0);
                out.push(case(cmd, format!("q{size} cap +{extra}"), q, padded));
            }
        }
    }

    // Non-canonical CompactSizes in the walked counts.
    let q = [3, 2, 2];
    let three = [vec![0xfd, 3, 0], vec![0x01]].concat();
    out.push(case("qcomplaint", "noncanonical bad", q, complaint(&three, &bitset(3, 0x01), &sig)));
    out.push(case("qcomplaint", "noncanonical for", q, complaint(&bitset(3, 0x01), &three, &sig)));
    out.push(case("qpcommit", "noncanonical", q, premature(&three, &pk, &sig, &sig)));
    let mut j = justification(&[&sk[..]], &sig);
    j.splice(65..66, [0xfd, 1, 0]);
    out.push(case("qjustify", "noncanonical", q, j));
    let mut c = Contribution {
        vvec: vec![&pk[..]; 2],
        eph: &pk,
        blobs: vec![3; 3],
        sig: &sig,
    }
    .bytes();
    c.splice(65..66, [0xfd, 2, 0]);
    out.push(case("qcontrib", "noncanonical vvec", q, c));

    // Every BLS field with every eager class, under both schemes.
    let q = [3, 2, 2];
    let bits = bitset(3, 0x01);
    for (class, b) in &bls.pks {
        let c = Contribution {
            vvec: vec![&b[..], &pk[..]],
            eph: &pk,
            blobs: vec![3; 3],
            sig: &sig,
        };
        out.extend(both(case("qcontrib", format!("vvec {class}"), q, c.bytes())));
        let c = Contribution {
            vvec: vec![&pk[..]; 2],
            eph: b,
            blobs: vec![3; 3],
            sig: &sig,
        };
        out.extend(both(case("qcontrib", format!("eph {class}"), q, c.bytes())));
        out.extend(both(case(
            "qpcommit",
            format!("pk {class}"),
            q,
            premature(&bits, b, &sig, &sig),
        )));
    }
    for (class, b) in &bls.sigs {
        let c = Contribution {
            vvec: vec![&pk[..]; 2],
            eph: &pk,
            blobs: vec![3; 3],
            sig: b,
        };
        out.extend(both(case("qcontrib", format!("sig {class}"), q, c.bytes())));
        out.extend(both(case("qcomplaint", format!("sig {class}"), q, complaint(&bits, &bits, b))));
        out.extend(both(case("qjustify", format!("sig {class}"), q, justification(&[&sk[..]], b))));
        out.extend(both(case(
            "qpcommit",
            format!("qsig {class}"),
            q,
            premature(&bits, &pk, b, &sig),
        )));
        out.extend(both(case(
            "qpcommit",
            format!("sig {class}"),
            q,
            premature(&bits, &pk, &sig, b),
        )));
    }
    for (class, b) in &bls.sks {
        out.extend(both(case(
            "qjustify",
            format!("sk {class}"),
            q,
            justification(&[&b[..]], &sig),
        )));
    }
    // Shorter than the type and quorum hash.
    for n in [0, 1, 32, 33] {
        out.push(case("qpcommit", "short", q, vec![100; n]));
    }
    out
}

fn ann(session: &[u8]) -> Vec<u8> {
    [session.to_vec(), vec![100], vec![0x21; 32], vec![0x22; 32], vec![0x23; 32]].concat()
}

/// A share inventory with `size` bits, in the fixed form with these bytes,
/// or the varint form with these offsets.
fn inv_fixed(session: u32, size: usize, bytes: &[u8]) -> Vec<u8> {
    [varint(session), count(size), vec![0], bytes.to_vec()].concat()
}

fn inv_varints(session: u32, size: usize, offsets: &[u32]) -> Vec<u8> {
    let mut out = [varint(session), count(size), vec![1]].concat();
    for &o in offsets {
        out.extend(varint(o));
    }
    out.push(0);
    out
}

fn batch(session: u32, shares: usize, sig: &[u8]) -> Vec<u8> {
    let mut out = [varint(session), count(shares)].concat();
    for i in 0..shares {
        out.extend((i as u16).to_le_bytes());
        out.extend(sig);
    }
    out
}

fn share(member: u16, sig: &[u8]) -> Vec<u8> {
    [
        vec![100],
        vec![0x21; 32],
        member.to_le_bytes().to_vec(),
        vec![0x23; 32],
        vec![0x24; 32],
        sig.to_vec(),
    ]
    .concat()
}

fn list(entries: &[Vec<u8>]) -> Vec<u8> {
    [count(entries.len()), entries.concat()].concat()
}

fn sig_share_cases(bls: &Bls) -> Vec<Case> {
    // Shares are lazy, read as bytes: the count bounds use zeros, which the
    // corpus writes as runs; `sig {class}` covers the share's forms.
    let sig = [0u8; 96];
    let q = [0, 0, 0];
    let mut out = Vec::new();
    // Session ids in every VARINT width, at the widths' edges and past
    // u32::MAX.
    for n in
        [0u32, 0x7f, 0x80, 0x407f, 0x4080, 0x20_407f, 0x20_4080, 0x1020_407f, 0x1020_4080, u32::MAX]
    {
        out.push(case(
            "qsigsesann",
            format!("varint {}", varint(n).len()),
            q,
            list(&[ann(&varint(n))]),
        ));
    }
    for digits in [
        vec![0x8e, 0xfe, 0xfe, 0xff, 0x00],
        vec![0x8f, 0x80, 0x80, 0x80, 0x00],
        vec![0x8f, 0xfe, 0xfe, 0xfe, 0x7f],
        vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x00],
        vec![0x80],
    ] {
        out.push(case("qsigsesann", "varint over", q, list(&[ann(&digits)])));
    }
    for n in [0, 1, 100, 101] {
        out.push(case("qsigsesann", format!("count {n}"), q, list(&vec![ann(&[5]); n])));
    }

    for cmd in ["qsigsinv", "qgetsigs"] {
        for size in [0usize, 1, 7, 8, 9, 16, 50, 400] {
            let len = size.div_ceil(8);
            let all = vec![0xff; len];
            let mut full = all.clone();
            if size % 8 != 0 {
                *full.last_mut().unwrap() = 0xff >> (8 - size % 8);
            }
            let tag = |what: &str| format!("{size} {what}");
            out.push(case(cmd, tag("fixed full"), q, list(&[inv_fixed(1, size, &full)])));
            out.push(case(cmd, tag("fixed empty"), q, list(&[inv_fixed(1, size, &vec![0; len])])));
            if size % 8 != 0 {
                out.push(case(cmd, tag("fixed padding"), q, list(&[inv_fixed(1, size, &all)])));
            }
            out.push(case(
                cmd,
                tag("varints full"),
                q,
                list(&[inv_varints(1, size, &vec![1; size])]),
            ));
            out.push(case(cmd, tag("varints empty"), q, list(&[inv_varints(1, size, &[])])));
            out.push(case(
                cmd,
                tag("varints past"),
                q,
                list(&[inv_varints(1, size, &[size as u32 + 1])]),
            ));
            if size > 0 {
                out.push(case(
                    cmd,
                    tag("varints last"),
                    q,
                    list(&[inv_varints(1, size, &[size as u32])]),
                ));
            }
            out.push(case(
                cmd,
                tag("varints big"),
                q,
                list(&[inv_varints(1, size, &[1, u32::MAX])]),
            ));
        }
        out.push(case(cmd, "size over", q, list(&[inv_fixed(1, 401, &[0; 51])])));
        out.push(case(cmd, "form 2", q, list(&[[varint(1), count(8), vec![2, 0]].concat()])));
        out.push(case(
            cmd,
            "noncanonical size",
            q,
            list(&[[varint(1), vec![0xfd, 8, 0, 0, 0xff]].concat()]),
        ));
        out.push(case(
            cmd,
            "noncanonical count",
            q,
            [vec![0xfd, 1, 0], inv_fixed(1, 8, &[0xff])].concat(),
        ));
        out.push(case(cmd, "no stopper", q, list(&[[varint(1), count(8), vec![1, 1]].concat()])));
        for n in [200usize, 201] {
            out.push(case(cmd, format!("count {n}"), q, list(&vec![inv_fixed(1, 0, &[]); n])));
        }
    }

    for (shares, batches) in
        [(0, 1), (1, 1), (400, 1), (401, 1), (200, 2), (201, 2), (1, 400), (0, 401)]
    {
        let b = vec![batch(7, shares, &sig); batches];
        out.push(case("qbsigs", format!("{batches}x{shares}"), q, list(&b)));
    }
    out.push(case("qbsigs", "200+201", q, list(&[batch(7, 200, &sig), batch(8, 201, &sig)])));
    out.push(case(
        "qbsigs",
        "noncanonical batches",
        q,
        [vec![0xfd, 1, 0], batch(7, 1, &sig)].concat(),
    ));
    let mut b = batch(7, 1, &sig);
    b.splice(1..2, [0xfd, 1, 0]);
    out.push(case("qbsigs", "noncanonical shares", q, list(&[b])));
    out.push(case("qsigsesann", "noncanonical count", q, [vec![0xfd, 1, 0], ann(&[5])].concat()));

    for n in [0usize, 1, 32, 33] {
        out.push(case("qsigshare", format!("count {n}"), q, list(&vec![share(3, &sig); n])));
    }
    for (class, b) in &bls.sigs {
        out.extend(both(case("qsigshare", format!("sig {class}"), q, list(&[share(1, b)]))));
        out.extend(both(case("qbsigs", format!("sig {class}"), q, list(&[batch(7, 1, b)]))));
    }
    out
}

/// `n`'s low `width` bytes, little-endian.
fn int_le(n: impl Into<i64>, width: usize) -> Vec<u8> {
    n.into().to_le_bytes()[..width].to_vec()
}

/// A list of `n` copies of `item`.
fn repeat(n: usize, item: &[u8]) -> Vec<u8> {
    list(&vec![item.to_vec(); n])
}

/// A byte vector of `n` copies of `b`.
fn bytes(n: usize, b: u8) -> Vec<u8> {
    repeat(n, &[b])
}

/// A byte vector of `n` copies of `b`, its size in a non-canonical 3-byte
/// CompactSize.
fn noncanonical(n: u16, b: u8) -> Vec<u8> {
    [vec![0xfd], n.to_le_bytes().to_vec(), vec![b; n.into()]].concat()
}

/// A CoinJoin or governance case: no quorum.
fn cj(cmd: &str, tag: impl Into<String>, raw: Vec<u8>) -> Case {
    case(cmd, tag, [0; 3], raw)
}

/// An input spending output 1 of 0x11.., with a one-byte script.
fn txin() -> Vec<u8> {
    [vec![0x11; 32], int_le(1, 4), bytes(1, 0x51), vec![0xff; 4]].concat()
}

/// A denominated P2PKH output.
fn txout() -> Vec<u8> {
    let script = [&[0x76, 0xa9, 0x14][..], &[0x12; 20], &[0x88, 0xac]].concat();
    [int_le(100_001, 8), count(script.len()), script].concat()
}

/// A transaction of `version` and `kind` with one input and output, and
/// `payload` after the lock time (Core reads it at version 3 and later of a
/// type other than 0).
fn tx(version: u16, kind: u16, payload: &[u8]) -> Vec<u8> {
    let head = u32::from(version) | u32::from(kind) << 16;
    [int_le(head, 4), repeat(1, &txin()), repeat(1, &txout()), int_le(0, 4), payload.to_vec()]
        .concat()
}

/// Transactions: normal, special with a payload, a type below version 3
/// (no payload read), an empty one, and a payload count past its bytes.
fn txs() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("normal", tx(2, 0, &[])),
        ("v3 normal", tx(3, 0, &[])),
        ("special", tx(3, 5, &bytes(3, 0xaa))),
        ("special empty", tx(3, 1, &bytes(0, 0))),
        ("v2 typed", tx(2, 1, &[])),
        ("no ins", [int_le(2, 4), count(0), count(0), int_le(0, 4)].concat()),
        ("payload short", tx(3, 1, &[5, 0xaa])),
    ]
}

fn dsq(ready: u8, time: i64, sig: &[u8]) -> Vec<u8> {
    [int_le(4, 4), vec![0x22; 32], int_le(time, 8), vec![ready], sig.to_vec()].concat()
}

fn dstx(tx: &[u8], sig: &[u8]) -> Vec<u8> {
    [tx.to_vec(), vec![0x44; 32], sig.to_vec(), int_le(1_700_000_000, 8)].concat()
}

fn govobj(data: &[u8], kind: i32, sig: &[u8]) -> Vec<u8> {
    [
        vec![0; 32],
        int_le(1, 4),
        int_le(1_700_000_000, 8),
        vec![0x99; 32],
        data.to_vec(),
        int_le(kind, 4),
        vec![0x66; 32],
        int_le(2, 4),
        sig.to_vec(),
    ]
    .concat()
}

fn vote(outcome: i32, signal: i32, sig: &[u8]) -> Vec<u8> {
    [
        vec![0x66; 32],
        int_le(2, 4),
        vec![0x77; 32],
        int_le(outcome, 4),
        int_le(signal, 4),
        int_le(1_700_000_000, 8),
        sig.to_vec(),
    ]
    .concat()
}

fn govsync(data: &[u8], hash_funcs: u32) -> Vec<u8> {
    [vec![0; 32], data.to_vec(), int_le(hash_funcs, 4), int_le(7, 4), vec![1]].concat()
}

/// The CoinJoin and governance cases (BS9b), with each message's base
/// layout cut short and with a byte after it.
fn coinjoin_gov_cases() -> Vec<Case> {
    let mut out = Vec::new();
    let base_tx = tx(2, 0, &[]);

    for (name, t) in txs() {
        for version in VERSIONS {
            for flags in [None, Some(0u8), Some(3), Some(0xff)] {
                let raw = [int_le(4, 4), t.clone(), Vec::from_iter(flags)].concat();
                out.push(Case {
                    version,
                    ..cj("dsa", format!("{name} flags {flags:?}"), raw)
                });
            }
        }
        out.push(cj("dsf", name, [int_le(7, 4), t.clone()].concat()));
        out.push(cj("dstx", name, dstx(&t, &bytes(96, 0x55))));
        out.push(cj(
            "dsi",
            format!("collateral {name}"),
            [repeat(1, &txin()), t.clone(), repeat(1, &txout())].concat(),
        ));
    }
    for n in [0usize, 1, 9, 180, 181] {
        let other = n.min(1);
        out.push(cj(
            "dsi",
            format!("ins {n}"),
            [repeat(n, &txin()), base_tx.clone(), repeat(other, &txout())].concat(),
        ));
        out.push(cj(
            "dsi",
            format!("outs {n}"),
            [repeat(other, &txin()), base_tx.clone(), repeat(n, &txout())].concat(),
        ));
        out.push(cj("dss", format!("ins {n}"), repeat(n, &txin())));
    }
    for big in [0x0200_0000usize, 0x0200_0001] {
        out.push(cj("dsi", format!("ins count {big:#x}"), count(big)));
        out.push(cj("dss", format!("ins count {big:#x}"), count(big)));
    }
    // dss's count is checked before any input is read.
    out.push(cj("dss", "count 181 no ins", count(181)));
    out.push(cj("dss", "count 180 no ins", count(180)));
    out.push(cj(
        "dsi",
        "noncanonical ins",
        [vec![0xfd, 1, 0], txin(), base_tx.clone(), count(0)].concat(),
    ));
    out.push(cj("dss", "noncanonical ins", [vec![0xfd, 1, 0], txin()].concat()));
    out.push(cj(
        "dss",
        "script 10000",
        repeat(1, &[vec![0x11; 32], int_le(1, 4), bytes(10_000, 0x51), vec![0xff; 4]].concat()),
    ));
    out.push(cj("dsc", "base", [int_le(7, 4), int_le(20, 4)].concat()));
    out.push(cj("dsc", "message -1", [int_le(7, 4), int_le(-1, 4)].concat()));
    out.push(cj(
        "dssu",
        "base",
        [int_le(7, 4), int_le(3, 4), int_le(1, 4), int_le(19, 4)].concat(),
    ));
    out.push(cj(
        "dssu",
        "out of range",
        [int_le(-7, 4), int_le(99, 4), int_le(2, 4), int_le(i32::MIN, 4)].concat(),
    ));

    for ready in [0u8, 1, 2, 0xff] {
        out.push(cj("dsq", format!("ready {ready}"), dsq(ready, 1_700_000_000, &bytes(96, 0x33))));
    }
    for n in [0usize, 95, 97, 300] {
        out.push(cj("dsq", format!("sig {n}"), dsq(1, 1_700_000_000, &bytes(n, 0x33))));
        out.push(cj("dstx", format!("sig {n}"), dstx(&base_tx, &bytes(n, 0x55))));
    }
    out.push(cj("dsq", "time negative", dsq(0, -1, &bytes(96, 0x33))));
    out.push(cj("dsq", "noncanonical sig", dsq(0, 1, &noncanonical(96, 0x33))));
    out.push(cj("dstx", "noncanonical sig", dstx(&base_tx, &noncanonical(96, 0x55))));

    for (name, data) in
        [("data 0", bytes(0, 0)), ("data 3", bytes(3, 0x7b)), ("data 600", bytes(600, 0x22))]
    {
        out.push(cj("govobj", name, govobj(&data, 1, &bytes(65, 0xab))));
    }
    for kind in [0, 2, 7, -1] {
        out.push(cj(
            "govobj",
            format!("type {kind}"),
            govobj(&bytes(3, 0x7b), kind, &bytes(96, 0xab)),
        ));
    }
    for n in [0usize, 64, 65, 96, 97, 200] {
        out.push(cj("govobj", format!("sig {n}"), govobj(&bytes(3, 0x7b), 1, &bytes(n, 0xab))));
    }
    for n in [0usize, 1, 64, 65, 66, 95, 96, 97, 200] {
        out.push(cj("govobjvote", format!("sig {n}"), vote(1, 1, &bytes(n, 0x88))));
    }
    out.push(cj("govobjvote", "sig count 10000", vote(1, 1, &[0xfd, 0x10, 0x27])));
    out.push(cj("govobjvote", "sig count 97 no bytes", vote(1, 1, &[97])));
    out.push(cj("govobjvote", "noncanonical sig 65", vote(1, 1, &noncanonical(65, 0x88))));
    for (outcome, signal) in [(2, 1), (0, 0), (3, 4), (9, -1), (i32::MIN, i32::MAX)] {
        out.push(cj(
            "govobjvote",
            format!("outcome {outcome} signal {signal}"),
            vote(outcome, signal, &bytes(96, 0x88)),
        ));
    }
    for n in [0usize, 3, 36_000, 36_001] {
        out.push(cj("govsync", format!("data {n}"), govsync(&bytes(n, 0x01), 5)));
    }
    out.push(cj("govsync", "noncanonical data", govsync(&noncanonical(3, 0x01), 5)));
    out.push(cj("govsync", "data count huge", [vec![0; 32], count(0x0200_0001)].concat()));
    // The handler's size constraints after the read; the hash read apart
    // from the filter.
    for funcs in [50, 51, u32::MAX] {
        out.push(cj("govsync", format!("hash funcs {funcs}"), govsync(&bytes(3, 0x01), funcs)));
    }
    out.push(cj("govsync", "hash short", vec![0; 31]));
    out.push(cj("govsync", "hash only", vec![0; 32]));

    // Each command's base layouts cut short, with a byte more, and empty.
    const BASES: [&str; 9] = [
        "normal",
        "normal flags None",
        "normal flags Some(0)",
        "base",
        "ready 1",
        "data 3",
        "sig 65",
        "sig 96",
        "ins 1",
    ];
    let bases: Vec<Case> =
        out.iter().filter(|c| BASES.contains(&c.tag.as_str())).cloned().collect();
    for b in bases {
        for (what, raw) in [
            ("short", b.raw[..b.raw.len() - 1].to_vec()),
            ("trailing", [b.raw.clone(), vec![0x5a]].concat()),
            ("empty", Vec::new()),
        ] {
            out.push(Case {
                tag: format!("{} {what}", b.tag),
                raw,
                ..b.clone()
            });
        }
    }
    out
}

/// Every fixed case.
pub fn cases(bls: &Bls) -> Vec<Case> {
    let mut out = dkg_cases(bls);
    out.extend(sig_share_cases(bls));
    out.extend(coinjoin_gov_cases());
    out
}

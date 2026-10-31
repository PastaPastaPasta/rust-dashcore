//! Classifier fixtures: for every scan a positive and a negative block, and a mutant per
//! predicate that must make at least one fixture fail (`mutants_are_killed`).

use dash_pkc::bls::{BlsScChia, BlsScIetf, BlsSecretKey, BlsSigId};
use dashcore::hashes::{Hash, sha256d};
use serde_json::Value;

use super::classify::{CClass, Mutant, c_hit, cl_sequence_hits, classify_c, scan_block};
use super::params::{Net, NetParams, llmq};
use super::payload::{BlsState, sig_state};
use super::{BlockInput, CRow, Cfg, ClFact, Scan, ScanSet, V24, merkle_root};
use dash_pkc::bls::BlsSchemeKind;

// ---------------------------------------------------------------------------------------
// Builders

fn compact(n: usize, out: &mut Vec<u8>) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        _ => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }
}

/// A transaction with `n_in` inputs (each a `sig_len`-byte scriptSig), the given output values
/// and, if `payload` is set, a `vExtraPayload` (the caller says when Core would read one).
fn tx(
    version: i16,
    ty: u16,
    n_in: usize,
    outs: &[u64],
    sig_len: usize,
    payload: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(version as u16 as u32 | (u32::from(ty) << 16)).to_le_bytes());
    compact(n_in, &mut b);
    for _ in 0..n_in {
        b.extend_from_slice(&[0; 36]);
        compact(sig_len, &mut b);
        b.extend(std::iter::repeat_n(0x51, sig_len));
        b.extend_from_slice(&[0xff; 4]);
    }
    compact(outs.len(), &mut b);
    for v in outs {
        b.extend_from_slice(&v.to_le_bytes());
        compact(1, &mut b);
        b.push(0x51);
    }
    b.extend_from_slice(&[0; 4]);
    if let Some(p) = payload {
        compact(p.len(), &mut b);
        b.extend_from_slice(&p);
    }
    b
}

/// A block of `txs` whose header commits to them.
fn block(txs: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 80];
    let txids: Vec<_> = txs.iter().map(|t| sha256d::Hash::hash(t).to_byte_array()).collect();
    b[36..68].copy_from_slice(&merkle_root(&txids));
    compact(txs.len(), &mut b);
    for t in txs {
        b.extend_from_slice(t);
    }
    b
}

/// A coinbase paying `value` in total (one output).
fn coinbase_paying(value: u64) -> Vec<u8> {
    tx(1, 0, 1, &[value], 4, None)
}

fn plain_coinbase() -> Vec<u8> {
    coinbase_paying(50)
}

/// A block of a plain coinbase followed by `txs`.
fn block_after_coinbase(txs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut all = vec![plain_coinbase()];
    all.extend(txs);
    block(&all)
}

fn cbtx_payload(version: u16, height: i32, diff: u32, sig: &[u8; 96]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&version.to_le_bytes());
    p.extend_from_slice(&height.to_le_bytes());
    p.extend_from_slice(&[1; 32]);
    if version >= 2 {
        p.extend_from_slice(&[2; 32]);
        if version >= 3 {
            compact(diff as usize, &mut p);
            p.extend_from_slice(sig);
            p.extend_from_slice(&0i64.to_le_bytes());
            if version >= 4 {
                p.extend_from_slice(&[3; 32]);
            }
        }
    }
    p
}

fn cbtx_block(version: u16, cb_height: i32, diff: u32, sig: &[u8; 96]) -> Vec<u8> {
    let cb = tx(3, 5, 1, &[50], 4, Some(cbtx_payload(version, cb_height, diff, sig)));
    block(&[cb])
}

fn mnhf_payload(version: u8, bit: u8) -> Vec<u8> {
    let mut p = vec![version, bit];
    p.extend_from_slice(&[9; 32]);
    p.extend_from_slice(&[0; 96]);
    p
}

fn bitset(nbits: usize, set: bool) -> Vec<u8> {
    let mut b = Vec::new();
    compact(nbits, &mut b);
    let mut bytes = vec![0u8; nbits.div_ceil(8)];
    if set {
        bytes[0] |= 1;
    }
    b.extend_from_slice(&bytes);
    b
}

/// A quorum commitment payload of version 3 (basic, not indexed).
fn qc_payload(llmq_type: u8, null: bool) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&1u16.to_le_bytes()); // qcTx version
    p.extend_from_slice(&0u32.to_le_bytes()); // height
    p.extend_from_slice(&3u16.to_le_bytes()); // commitment version
    p.push(llmq_type);
    p.extend_from_slice(&[4; 32]); // quorumHash
    p.extend_from_slice(&bitset(50, !null)); // signers
    p.extend_from_slice(&bitset(50, !null)); // validMembers
    p.extend_from_slice(&[0; 48]); // quorumPublicKey
    p.extend_from_slice(&[0; 32]); // quorumVvecHash
    p.extend_from_slice(&[0; 96]); // quorumSig
    p.extend_from_slice(&[0; 96]); // membersSig
    p
}

fn qc_tx(llmq_type: u8, null: bool) -> Vec<u8> {
    tx(3, 6, 0, &[], 0, Some(qc_payload(llmq_type, null)))
}

fn mnhf_tx(version: u8, bit: u8) -> Vec<u8> {
    tx(3, 7, 0, &[], 0, Some(mnhf_payload(version, bit)))
}

/// A real compressed G2 point in the basic scheme.
fn valid_sig() -> [u8; 96] {
    let sk = BlsSecretKey::<BlsScIetf>::from_bytes(&[7; 32]).expect("secret key");
    sk.sign_with(b"o6 scan fixture", BlsSigId::Basic).to_bytes()
}

/// Bytes canonical in the legacy scheme only.
fn other_scheme_sig() -> [u8; 96] {
    for seed in 1u8..=40 {
        let sk = BlsSecretKey::<BlsScIetf>::from_bytes(&[seed; 32]).expect("secret key");
        let sig = sk.sign_with(b"o6 scan fixture", BlsSigId::Basic);
        let legacy = sig.to_scheme::<BlsScChia>().expect("lowers to legacy").to_bytes();
        if sig_state(&legacy, BlsSchemeKind::Basic) == BlsState::OtherScheme {
            return legacy;
        }
    }
    panic!("no legacy-only signature among 40 keys");
}

// ---------------------------------------------------------------------------------------
// Cases

struct Case {
    name: &'static str,
    net: Net,
    scans: &'static [Scan],
    /// Blocks by height.
    blocks: Vec<(u32, Vec<u8>)>,
    /// Scan (c): `(height, reward, limit)` the oracle would give.
    rewards: Vec<(u32, i64, Option<i64>)>,
    /// `class@height` (and `bit=n` for scan (e)), sorted.
    expect: Vec<&'static str>,
    /// The `v24` deployment state.
    v24: V24,
    /// The median time past of every block's parent, where a fixture needs one.
    mtp: Option<i64>,
}

impl Case {
    fn v24(mut self, v24: V24) -> Self {
        self.v24 = v24;
        self
    }

    fn mtp(mut self, mtp: i64) -> Self {
        self.mtp = Some(mtp);
        self
    }
}

fn run(case: &Case, mutant: Option<Mutant>) -> Vec<String> {
    let mut cfg = Cfg::new(case.net, ScanSet::of(case.scans));
    cfg.mutant = mutant;
    cfg.v24 = case.v24;
    let mtp = || case.mtp;
    let mut out = Vec::new();
    let mut facts: Vec<ClFact> = Vec::new();
    let mut rows: Vec<CRow> = Vec::new();
    for (h, bytes) in &case.blocks {
        let r = scan_block(
            &cfg,
            &BlockInput {
                bytes,
                height: *h,
                hash: [0; 32],
                stale: false,
                mtp_prev: Some(&mtp),
            },
        );
        assert!(
            r.unparsed.is_none() || case.name.starts_with("unframed"),
            "{}: {:?}",
            case.name,
            r.unparsed
        );
        for hit in &r.hits {
            out.push(label(hit.class, hit.height, &hit.detail));
        }
        facts.extend(r.cl);
        rows.extend(r.c_row);
    }
    facts.sort_by_key(|f| f.height);
    for hit in cl_sequence_hits(&cfg, &facts) {
        out.push(label(hit.class, hit.height, &hit.detail));
    }
    for row in &rows {
        match case.rewards.iter().find(|(h, ..)| *h == row.height) {
            Some(&(_, reward, limit)) => {
                let class = classify_c(&cfg, row, reward, limit);
                out.push(label(c_hit(row, class, reward, limit).class, row.height, &Value::Null));
            }
            None => out.push(format!("c.row_without_reward@{}", row.height)),
        }
    }
    out.sort();
    out
}

fn label(class: &str, height: u32, detail: &Value) -> String {
    match detail.get("bit").and_then(Value::as_u64) {
        Some(bit) if class == "e.signal" => format!("{class}@{height} bit={bit}"),
        _ => format!("{class}@{height}"),
    }
}

fn case(
    name: &'static str,
    net: Net,
    scans: &'static [Scan],
    blocks: Vec<(u32, Vec<u8>)>,
    expect: &[&'static str],
) -> Case {
    let mut expect = expect.to_vec();
    expect.sort();
    Case {
        name,
        net,
        scans,
        blocks,
        rewards: Vec::new(),
        expect,
        v24: V24::NeverActive,
        mtp: None,
    }
}

#[allow(clippy::vec_init_then_push)]
fn cases() -> Vec<Case> {
    use Scan::*;
    let sig = valid_sig();
    let legacy = other_scheme_sig();
    let m = Net::Mainnet;
    let t = Net::Testnet;
    let dip3 = NetParams::of(m).dip3;
    let mut v = Vec::new();

    // (a) nVersion > 3 && nType != 0, and the framing the two rules disagree on
    v.push(case(
        "a-pos-v4-typed",
        m,
        &[A],
        vec![(100_000, block(&[plain_coinbase(), tx(4, 7, 0, &[], 0, Some(vec![0xaa]))]))],
        &["a.framing_differs@100000", "a.version_gt3_typed@100000"],
    ));
    v.push(case(
        "a-pos-v5-type1",
        m,
        &[A],
        vec![(100_000, block(&[plain_coinbase(), tx(5, 1, 1, &[1], 1, Some(vec![1, 2]))]))],
        &["a.framing_differs@100000", "a.version_gt3_typed@100000"],
    ));
    v.push(case(
        "a-neg-v3-typed",
        m,
        &[A],
        vec![(100_000, block(&[plain_coinbase(), tx(3, 7, 0, &[], 0, Some(vec![0xaa]))]))],
        &[],
    ));
    v.push(case(
        "a-neg-v4-untyped",
        m,
        &[A],
        vec![(100_000, block(&[plain_coinbase(), tx(4, 0, 1, &[1], 1, None)]))],
        &[],
    ));
    v.push(case(
        "a-neg-negative-version",
        m,
        &[A],
        // nVersion is signed: 0x8001 is negative, so not > 3, and carries no payload
        vec![(100_000, block(&[plain_coinbase(), tx(i16::MIN + 1, 7, 0, &[], 0, None)]))],
        &[],
    ));

    // (b) pre-DIP3 type bytes under version < 3
    v.push(case(
        "b-pos-type-2a",
        m,
        &[B],
        vec![(500_000, block(&[plain_coinbase(), tx(2, 0x2a, 1, &[1], 1, None)]))],
        &["b.pre_dip3@500000"],
    ));
    v.push(case(
        "b-pos-negative-version",
        m,
        &[B],
        vec![(500_000, block(&[plain_coinbase(), tx(-1, 7, 0, &[], 0, None)]))],
        &["b.pre_dip3@500000"],
    ));
    v.push(case(
        "b-neg-untyped",
        m,
        &[B],
        vec![(500_000, block(&[plain_coinbase(), tx(2, 0, 1, &[1], 1, None)]))],
        &[],
    ));
    v.push(case(
        "b-neg-special-version",
        m,
        &[B],
        vec![(500_000, block(&[plain_coinbase(), tx(3, 5, 1, &[1], 1, Some(vec![1]))]))],
        &[],
    ));
    v.push(case(
        "b-post-dip3",
        m,
        &[B],
        vec![(dip3, block(&[plain_coinbase(), tx(2, 7, 0, &[], 0, None)]))],
        &["b.post_dip3@1028160"],
    ));
    v.push(case(
        "b-pre-dip3-last-block",
        m,
        &[B],
        vec![(dip3 - 1, block(&[plain_coinbase(), tx(2, 7, 0, &[], 0, None)]))],
        &["b.pre_dip3@1028159"],
    ));

    // (c) coinbase against the block reward
    let mut c = case(
        "c-window-and-superblock",
        m,
        &[C],
        vec![
            (332_320, block(&[coinbase_paying(1000)])), // window, over
            (332_321, block(&[coinbase_paying(1000)])), // window, equal
            (332_419, block(&[coinbase_paying(900)])),  // last window block, under
            (332_420, block(&[coinbase_paying(5000)])), // first block after the window
            (631_408, block(&[coinbase_paying(2000)])), // superblock within its limit
            (648_024, block(&[coinbase_paying(9000)])), // superblock over
            (700_001, block(&[coinbase_paying(5000)])), // neither
        ],
        &[
            "c.c1_window_overpay@332320",
            "c.window_within@332321",
            "c.window_within@332419",
            "c.c2_superblock@631408",
            "c.c3_anomaly@648024",
        ],
    );
    c.rewards = vec![
        (332_320, 999, None),
        (332_321, 1000, None),
        (332_419, 1000, None),
        (631_408, 1000, Some(1500)),
        (648_024, 1000, Some(500)),
    ];
    v.push(c);

    v.push(case(
        "c-neg-neither-window-nor-superblock",
        m,
        &[C],
        vec![
            (700_001, block(&[coinbase_paying(5000)])),
            (332_420, block(&[coinbase_paying(5000)])),
        ],
        &[],
    ));

    // (d) CbTx fields
    let h0 = 2_000_000u32;
    let cb = |version, h: u32, diff, sig: &[u8; 96]| cbtx_block(version, h as i32, diff, sig);
    v.push(case("d-neg-valid", m, &[D], vec![(h0, cb(3, h0, 5, &sig))], &[]));
    v.push(case(
        "d-pos-diff-equals-height",
        m,
        &[D],
        vec![(h0, cb(3, h0, h0, &sig))],
        &["d.diff_ge_height@2000000"],
    ));
    v.push(case("d-neg-diff-below-height", m, &[D], vec![(h0, cb(3, h0, h0 - 1, &sig))], &[]));
    v.push(case("d-neg-null-zero-diff", m, &[D], vec![(h0, cb(3, h0, 0, &[0; 96]))], &[]));
    v.push(case(
        "d-pos-null-nonzero-diff",
        m,
        &[D],
        vec![(h0, cb(3, h0, 3, &[0; 96]))],
        &["d.null_sig_nonzero_diff@2000000"],
    ));
    v.push(case(
        "d-pos-other-scheme-sig",
        m,
        &[D],
        vec![(h0, cb(3, h0, 0, &legacy))],
        &["d.sig_other_scheme@2000000"],
    ));
    v.push(case(
        "d-pos-malleable-sig",
        m,
        &[D],
        vec![(h0, cb(3, h0, 0, &[0xff; 96]))],
        &["d.sig_malleable@2000000"],
    ));
    {
        let mut p = cbtx_payload(3, h0 as i32, 0, &sig);
        // bestCLHeightDiff 5 written in three bytes: non-canonical
        let at = 2 + 4 + 32 + 32;
        p.splice(at..at + 1, [0xfd, 5, 0]);
        let b = block(&[tx(3, 5, 1, &[50], 4, Some(p))]);
        v.push(case(
            "d-pos-non-canonical-diff",
            m,
            &[D],
            vec![(h0, b)],
            &["d.unparseable@2000000"],
        ));
    }
    let v20 = NetParams::of(m).v20;
    v.push(case("d-neg-v3-at-v20", m, &[D], vec![(v20, cb(3, v20, 0, &sig))], &[]));
    v.push(case(
        "d-pos-v2-at-v20",
        m,
        &[D],
        vec![(v20, cb(2, v20, 0, &sig))],
        &["d.version@1987776"],
    ));
    v.push(case(
        "d-neg-plain-coinbase-before-dip3",
        m,
        &[D],
        vec![(dip3 - 1, block(&[plain_coinbase()]))],
        &[],
    ));
    // the v24 leg of CheckCbTx: version 4 exactly where v24 and V20 are both active
    let v24_from = 1_578_000;
    let cb4 = |version, h: u32| cbtx_block(version, h as i32, 0, &sig);
    v.push(
        case("d-neg-v4-at-v24-start", t, &[D], vec![(v24_from, cb4(4, v24_from))], &[])
            .v24(V24::ActiveFrom(v24_from)),
    );
    v.push(
        case(
            "d-pos-v3-at-v24-start",
            t,
            &[D],
            vec![(v24_from, cb4(3, v24_from))],
            &["d.version@1578000"],
        )
        .v24(V24::ActiveFrom(v24_from)),
    );
    v.push(
        case("d-neg-v3-before-v24", t, &[D], vec![(v24_from - 1, cb4(3, v24_from - 1))], &[])
            .v24(V24::ActiveFrom(v24_from)),
    );
    v.push(
        case(
            "d-pos-v4-before-v24",
            t,
            &[D],
            vec![(v24_from - 1, cb4(4, v24_from - 1))],
            &["d.version@1577999"],
        )
        .v24(V24::ActiveFrom(v24_from)),
    );
    v.push(case(
        "d-pos-v4-where-v24-is-never-active",
        m,
        &[D],
        vec![(h0, cb4(4, h0))],
        &["d.version@2000000"],
    ));
    v.push(
        case(
            "d-pos-v4-where-v24-is-inactive",
            t,
            &[D],
            vec![(1_000_000, cb4(4, 1_000_000))],
            &["d.version@1000000"],
        )
        .v24(V24::Inactive),
    );
    v.push(case(
        "d-pos-v3-before-v20",
        m,
        &[D],
        vec![(1_500_000, cb(3, 1_500_000, 0, &sig))],
        &["d.version@1500000"],
    ));
    v.push(case(
        "d-pos-v2-after-v20",
        m,
        &[D],
        vec![(h0, cb(2, h0, 0, &sig))],
        &["d.version@2000000"],
    ));
    v.push(case(
        "d-pos-v1-after-dip8",
        m,
        &[D],
        vec![(1_100_000, cb(1, 1_100_000, 0, &sig))],
        &["d.version@1100000"],
    ));
    v.push(case("d-neg-v1-at-dip8", m, &[D], vec![(1_088_640, cb(1, 1_088_640, 0, &sig))], &[]));
    v.push(case(
        "d-pos-wrong-height",
        m,
        &[D],
        vec![(h0, cbtx_block(3, h0 as i32 + 1, 0, &sig))],
        &["d.height@2000000"],
    ));
    v.push(case(
        "d-pos-coinbase-not-cbtx",
        m,
        &[D],
        vec![(dip3, block(&[plain_coinbase()]))],
        &["d.coinbase_not_cbtx@1028160"],
    ));
    v.push(case(
        "d-pos-cbtx-not-first",
        m,
        &[D],
        vec![(
            h0,
            block(&[
                tx(3, 5, 1, &[50], 4, Some(cbtx_payload(3, h0 as i32, 0, &sig))),
                tx(3, 5, 1, &[50], 4, Some(cbtx_payload(3, h0 as i32, 0, &sig))),
            ]),
        )],
        &["d.cbtx_not_first@2000000"],
    ));
    // the sequential rule: CL height non-decreasing, null after non-null refused
    let seq = |diffs: &[(u32, &[u8; 96])]| -> Vec<(u32, Vec<u8>)> {
        diffs
            .iter()
            .enumerate()
            .map(|(i, (d, s))| {
                let h = 3_000_000 + i as u32;
                (h, cb(3, h, *d, s))
            })
            .collect()
    };
    v.push(case("d-neg-sequence", m, &[D], seq(&[(5, &sig), (6, &sig), (0, &sig)]), &[]));
    v.push(case(
        "d-pos-older-clsig",
        m,
        &[D],
        seq(&[(5, &sig), (7, &sig)]),
        &["d.older_clsig@3000001"],
    ));
    v.push(case(
        "d-pos-null-after-non-null",
        m,
        &[D],
        seq(&[(5, &sig), (0, &[0; 96])]),
        &["d.null_after_non_null@3000001"],
    ));
    v.push(case("d-neg-null-after-null", m, &[D], seq(&[(0, &[0; 96]), (0, &[0; 96])]), &[]));
    {
        let mut blocks = seq(&[(5, &sig)]);
        // a gap: block 3000002 has no non-null predecessor at 3000001
        blocks.push((3_000_002, cb(3, 3_000_002, 0, &[0; 96])));
        v.push(case("d-neg-gap-resets", m, &[D], blocks, &[]));
    }

    // (e) MNHF signals
    v.push(case(
        "e-pos-bit-10",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, 10)]))],
        &["e.signal@2100000 bit=10"],
    ));
    v.push(case(
        "e-pos-bad-version",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(2, 11)]))],
        &["e.signal@2100000 bit=11", "e.bad_version@2100000"],
    ));
    // IsValidMNActivation: only the non-EHF TESTDUMMY bit (28) is refused
    v.push(
        case(
            "e-pos-bit-28-testdummy",
            m,
            &[E],
            vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, 28)]))],
            &["e.signal@2100000 bit=28", "e.invalid_activation@2100000"],
        )
        .mtp(1_790_000_000),
    );
    v.push(
        case(
            "e-neg-ehf-bit-inside-its-window",
            t,
            &[E],
            vec![(1_565_132, block_after_coinbase(vec![mnhf_tx(1, 13)]))],
            &["e.signal@1565132 bit=13"],
        )
        .mtp(1_790_208_000),
    );
    v.push(
        case(
            "e-neg-ehf-bit-before-its-window",
            t,
            &[E],
            vec![(1_565_132, block_after_coinbase(vec![mnhf_tx(1, 13)]))],
            &["e.signal@1565132 bit=13"],
        )
        .mtp(1_790_207_999),
    );
    v.push(case(
        "e-pos-bit-28-without-a-median-time",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, 28)]))],
        &["e.signal@2100000 bit=28", "e.invalid_activation@2100000"],
    ));
    // VERSIONBITS_NUM_BITS is 29: bit 28 is in bounds (its TESTDUMMY deployment is the
    // refusal above), bits 29..=31 are out of bounds and get no activation verdict, and so is 32+
    v.push(case(
        "e-neg-bit-27-in-bounds",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, 27)]))],
        &["e.signal@2100000 bit=27"],
    ));
    let out_of_bounds: [(&str, u8, &str); 4] = [
        ("e-pos-bit-29-out-of-bounds", 29, "e.signal@2100000 bit=29"),
        ("e-pos-bit-30-out-of-bounds", 30, "e.signal@2100000 bit=30"),
        ("e-pos-bit-31-out-of-bounds", 31, "e.signal@2100000 bit=31"),
        ("e-pos-bit-out-of-bounds", 32, "e.signal@2100000 bit=32"),
    ];
    for (name, bit, signal) in out_of_bounds {
        v.push(case(
            name,
            m,
            &[E],
            vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, bit)]))],
            &[signal, "e.bit_out_of_bounds@2100000"],
        ));
    }
    v.push(case(
        "e-pos-duplicate-in-block",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![mnhf_tx(1, 12), mnhf_tx(1, 12)]))],
        &["e.duplicate_in_block@2100000", "e.signal@2100000 bit=12", "e.signal@2100000 bit=12"],
    ));
    v.push(case(
        "e-neg-quorum-commitment",
        m,
        &[E],
        vec![(2_100_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_400_60, true)]))],
        &[],
    ));
    v.push(case(
        "e-neg-type7-without-payload",
        m,
        &[E],
        vec![(100_000, block_after_coinbase(vec![tx(2, 7, 0, &[], 0, None)]))],
        &[],
    ));

    // (f) commitments of types that are not enabled
    v.push(case(
        "f-pos-50_60-after-disable-null",
        m,
        &[F],
        vec![(1_738_699, block_after_coinbase(vec![qc_tx(llmq::LLMQ_50_60, true)]))],
        &["f1.after_disable.null@1738699"],
    ));
    v.push(case(
        "f-neg-50_60-last-enabled-block",
        m,
        &[F],
        vec![(1_738_698, block_after_coinbase(vec![qc_tx(llmq::LLMQ_50_60, true)]))],
        &[],
    ));
    v.push(case(
        "f-pos-50_60-after-disable-nonnull",
        m,
        &[F],
        vec![(1_800_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_50_60, false)]))],
        &["f1.after_disable.nonnull@1800000"],
    ));
    v.push(case(
        "f-neg-400_60",
        m,
        &[F],
        vec![(1_800_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_400_60, true)]))],
        &[],
    ));
    v.push(case(
        "f-pos-100_67-before-enable",
        m,
        &[F],
        vec![(1_000_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_100_67, true)]))],
        &["f2.before_enable.null@1000000"],
    ));
    v.push(case(
        "f-pos-unconfigured-type",
        m,
        &[F],
        vec![(1_800_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_25_67, true)]))],
        &["f.unknown_type@1800000"],
    ));
    v.push(case(
        "f-neg-testnet-50_60",
        t,
        &[F],
        vec![(1_000_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_50_60, true)]))],
        &[],
    ));
    v.push(case(
        "f-pos-testnet-25_67-before-pin",
        t,
        &[F],
        vec![(800_000, block_after_coinbase(vec![qc_tx(llmq::LLMQ_25_67, true)]))],
        &["f2.before_enable.null@800000"],
    ));
    v
}

#[test]
fn fixtures_hold() {
    for c in cases() {
        assert_eq!(run(&c, None), c.expect, "case {}", c.name);
    }
}

#[test]
fn every_scan_has_a_positive_and_a_negative_fixture() {
    for scan in Scan::ALL {
        let mine: Vec<Case> = cases().into_iter().filter(|c| c.scans == [scan]).collect();
        assert!(mine.iter().any(|c| !c.expect.is_empty()), "no positive for {scan:?}");
        assert!(mine.iter().any(|c| c.expect.is_empty()), "no negative for {scan:?}");
    }
}

#[test]
fn mutants_are_killed() {
    let mutants = [
        Mutant::AGtToGe,
        Mutant::ATypeInvert,
        Mutant::VersionUnsigned,
        Mutant::BLtToLe,
        Mutant::BDip3Boundary,
        Mutant::CGtToGe,
        Mutant::CWindowLe,
        Mutant::DGeToGt,
        Mutant::DOlderGe,
        Mutant::DV20Gt,
        Mutant::DV24Gt,
        Mutant::DNullAfterNonNull,
        Mutant::EBitPlusOne,
        Mutant::ETypeSix,
        Mutant::EBoundLe,
        Mutant::EBound32,
        Mutant::EEhfInverted,
        Mutant::FBoundary,
        Mutant::FNullSwap,
    ];
    let all = cases();
    for m in mutants {
        let killed: Vec<&str> =
            all.iter().filter(|c| run(c, Some(m)) != c.expect).map(|c| c.name).collect();
        assert!(!killed.is_empty(), "mutant {m:?} survives every fixture");
    }
}

#[test]
fn stale_blocks_count_apart_and_skip_the_sequential_scans() {
    let cfg = Cfg::new(Net::Mainnet, ScanSet::all());
    let sig = valid_sig();
    // a window height with a wrong CbTx height: an unstale block would give a (c) row and a hit
    let h = 332_320;
    let bytes = cbtx_block(3, h as i32 + 1, 5, &sig);
    let r = scan_block(
        &cfg,
        &BlockInput {
            bytes: &bytes,
            height: h,
            hash: [1; 32],
            stale: true,
            mtp_prev: None,
        },
    );
    assert!(r.cl.is_none() && r.c_row.is_none());
    assert!(r.counters.keys().all(|k| k.starts_with("stale.")), "{:?}", r.counters);
    assert!(!r.hits.is_empty() && r.hits.iter().all(|x| x.stale));
    let unstale = scan_block(
        &cfg,
        &BlockInput {
            bytes: &bytes,
            height: h,
            hash: [1; 32],
            stale: false,
            mtp_prev: None,
        },
    );
    assert!(unstale.c_row.is_some(), "the fixture is a (c) candidate when not stale");
}

#[test]
fn a_body_that_is_not_the_blocks_is_reported() {
    let cfg = Cfg::new(Net::Mainnet, ScanSet::all());
    let mut bytes = block(&[plain_coinbase()]);
    let r = |b: &[u8]| {
        scan_block(
            &cfg,
            &BlockInput {
                bytes: b,
                height: 5,
                hash: [0; 32],
                stale: false,
                mtp_prev: None,
            },
        )
    };
    assert!(r(&bytes).unparsed.is_none());
    bytes[40] ^= 1;
    let res = r(&bytes);
    assert_eq!(res.unparsed.as_deref(), Some("the merkle root differs from the header's"));
    assert_eq!(res.counters["unparsed_blocks"], 1);
}

#[test]
fn signature_fixtures_have_the_states_they_claim() {
    assert_eq!(sig_state(&valid_sig(), BlsSchemeKind::Basic), BlsState::Valid);
    assert_eq!(sig_state(&other_scheme_sig(), BlsSchemeKind::Basic), BlsState::OtherScheme);
    assert_eq!(sig_state(&[0xff; 96], BlsSchemeKind::Basic), BlsState::Malleable);
}

#[test]
fn an_unframed_block_is_reported_not_classified() {
    let cfg = Cfg::new(Net::Mainnet, ScanSet::all());
    let mut bytes = block(&[plain_coinbase()]);
    bytes.push(0); // trailing byte
    let r = scan_block(
        &cfg,
        &BlockInput {
            bytes: &bytes,
            height: 5,
            hash: [0; 32],
            stale: false,
            mtp_prev: None,
        },
    );
    assert!(r.unparsed.is_some());
    assert!(r.hits.is_empty());
}

#[test]
fn classify_c_boundaries() {
    use super::CRowKind;
    let cfg = Cfg::new(Net::Mainnet, ScanSet::all());
    let row = |kind, v| CRow {
        height: 1,
        block: [0; 32],
        kind,
        coinbase_value: v,
        coinbase_txid: [0; 32],
        blk: None,
    };
    assert_eq!(classify_c(&cfg, &row(CRowKind::Window, 100), 100, None), CClass::Within);
    assert_eq!(classify_c(&cfg, &row(CRowKind::Window, 101), 100, None), CClass::C1WindowOverpay);
    assert_eq!(
        classify_c(&cfg, &row(CRowKind::Superblock, 600), 100, Some(500)),
        CClass::C2Superblock
    );
    assert_eq!(
        classify_c(&cfg, &row(CRowKind::Superblock, 601), 100, Some(500)),
        CClass::C3Anomaly
    );
    assert_eq!(classify_c(&cfg, &row(CRowKind::Superblock, 101), 100, None), CClass::C3Anomaly);
}

fn u32_of(h: impl Into<u32>) -> u32 {
    h.into()
}

#[test]
fn the_parameter_table_matches_dash_params() {
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};
    for (net, kind) in [(Net::Mainnet, NetworkKind::Mainnet), (Net::Testnet, NetworkKind::Testnet)]
    {
        let p = ChainParamsBuilder::new(kind, Release::V24_0).build().expect("params");
        let c = &p.consensus;
        let mine = NetParams::of(net);
        assert_eq!(mine.dip3, u32_of(c.dip0003_height), "{net:?} dip3");
        assert_eq!(mine.dip8, u32_of(c.dip0008_height), "{net:?} dip8");
        assert_eq!(mine.dip20, u32_of(c.dip0020_height), "{net:?} dip20");
        assert_eq!(mine.dip24, u32_of(c.dip0024_height), "{net:?} dip24");
        assert_eq!(mine.dip24_quorums, u32_of(c.dip0024_quorums_height), "{net:?} dip24 quorums");
        assert_eq!(mine.v20, u32_of(c.v20_height), "{net:?} v20");
        assert_eq!(
            mine.budget_start,
            u32_of(c.budget_payments_start_block),
            "{net:?} budget start"
        );
        assert_eq!(i64::from(mine.budget_cycle), i64::from(c.budget_payments_cycle_blocks));
        assert_eq!(i64::from(mine.budget_window), i64::from(c.budget_payments_window_blocks));
        assert_eq!(i64::from(mine.superblock_cycle), i64::from(c.superblock_cycle));
        assert_eq!(mine.superblock_start, u32_of(c.superblock_start.0), "{net:?} superblock start");
        let mut theirs: Vec<String> = c.llmqs.iter().map(|l| l.name.to_ascii_lowercase()).collect();
        theirs.sort();
        let mut ours: Vec<String> =
            mine.llmqs.iter().map(|t| llmq::name(*t).to_ascii_lowercase()).collect();
        ours.sort();
        assert_eq!(ours, theirs, "{net:?} configured LLMQ types");
    }
}

#[test]
fn an_unknown_v24_state_lets_versions_3_and_4_through_and_counts_them() {
    let sig = valid_sig();
    let cfg = Cfg::new(Net::Mainnet, ScanSet::of(&[Scan::D]));
    assert_eq!(cfg.v24, V24::Unknown);
    let h = 2_000_000u32;
    for version in [3, 4] {
        let bytes = cbtx_block(version, h as i32, 0, &sig);
        let r = scan_block(
            &cfg,
            &BlockInput {
                bytes: &bytes,
                height: h,
                hash: [0; 32],
                stale: false,
                mtp_prev: None,
            },
        );
        assert!(r.hits.is_empty(), "version {version}: {:?}", r.hits);
        assert_eq!(r.counters["d.v24_undecided"], 1, "version {version}");
    }
    // before V20 nothing is undecided: version 3 is already wrong
    let bytes = cbtx_block(3, 1_500_000, 0, &sig);
    let r = scan_block(
        &cfg,
        &BlockInput {
            bytes: &bytes,
            height: 1_500_000,
            hash: [0; 32],
            stale: false,
            mtp_prev: None,
        },
    );
    assert_eq!(r.hits.len(), 1);
    assert!(!r.counters.contains_key("d.v24_undecided"));
}

#[test]
fn the_v24_state_is_read_from_getdeploymentinfo() {
    use serde_json::json;
    // as the testnet oracle answered
    let active = json!({"deployments": {"v20": {"type": "buried", "active": true, "height": 905100},
        "v24": {"type": "bip9", "height": 1578000, "active": true,
                "bip9": {"status": "active", "since": 1578000}}}});
    assert_eq!(V24::from_deployment_info(&active), Ok(V24::ActiveFrom(1_578_000)));
    let inactive = json!({"deployments": {"v24": {"type": "bip9", "active": false,
        "bip9": {"status": "started", "since": 100}}}});
    assert_eq!(V24::from_deployment_info(&inactive), Ok(V24::Inactive));
    // as the mainnet oracle answered: v24 is not listed, since it is `NEVER_ACTIVE` there
    let unlisted =
        json!({"deployments": {"v20": {"type": "buried", "active": true, "height": 1987776}}});
    assert_eq!(V24::from_deployment_info(&unlisted), Ok(V24::NeverActive));
    assert!(V24::from_deployment_info(&json!({})).is_err());
    assert!(V24::from_deployment_info(&json!({"deployments": {"v24": {"active": true}}})).is_err());
}

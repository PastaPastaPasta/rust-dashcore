//! History sweeps over a header dump (`dump_headers.py`: every header from genesis on a
//! reference node's best header chain, 80 bytes each), and the sample of it that
//! `dash-consensus/tests/history_sample.rs` replays on every run (CS2 §6.4).

use std::path::Path;

use dash_consensus::BlockId;
use dash_consensus::pow::{check_proof_of_work, get_next_work_required, nbits_acceptable};
use dash_consensus::subsidy;
use dash_core_vectors::hex::{hex_str, parse_hex};
use dash_num::CompactTarget;
use dash_oracle_sys::{Chain, Oracle, Pin};
use dash_types::Numeric;
use serde_json::json;

use crate::cases::{self, Variant};
use crate::common::VecChain;
use crate::{Tally, WINDOW, both, core_subsidy, fp};

/// A header dump `dash-consensus/tests/data/PROVENANCE.md` records, and the tally a sweep over
/// it prints. `sweep` and `sample` refuse any other dump unless given `--unpinned`, and `sweep`
/// fails unless its tally is exactly this one, so an exit 0 proves the coverage (a truncated or
/// altered dump cannot pass).
struct RecordedDump {
    net: &'static str,
    headers: usize,
    sha256: &'static str,
    tally: &'static [(u64, &'static str)],
}

const RECORDED: [RecordedDump; 2] = [
    RecordedDump {
        net: "main",
        headers: 2_551_767,
        sha256: "58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251",
        tally: &[
            (2_551_766, POW),
            (68_589, TOLERANCE_CORE),
            (68_589, TOLERANCE_PREDICATE),
            (34_122, TOLERANCE_EQUAL),
            (34_467, TOLERANCE_DIFFERENT),
            (18_940, KGW),
            (2_483_177, NEXT_WORK),
            (2_551_766, ACCEPTED),
            (2_551_766, SUBSIDY),
        ],
    },
    RecordedDump {
        net: "test",
        headers: 1_568_460,
        sha256: "a88526024d3c3a2a82c797bd6348a8789ce5ee388ecf33742f3987e414659b27",
        tally: &[
            (1_568_459, POW),
            (1_568_459, NEXT_WORK),
            (1_568_459, ACCEPTED),
            (1_568_459, SUBSIDY),
        ],
    },
];

const POW: &str = "check_proof_of_work: true (every header but the tip)";
const TOLERANCE_CORE: &str = "next_work <= 68589: equals Core's GetNextWorkRequired (both pins)";
const TOLERANCE_PREDICATE: &str =
    "next_work <= 68589: the tolerance equals Core's predicate on Core's doubles (both pins)";
const TOLERANCE_EQUAL: &str = "next_work <= 68589: the header's nBits are equal";
const TOLERANCE_DIFFERENT: &str = "next_work <= 68589: the header's nBits differ (the tolerance)";
const KGW: &str = "next_work: KGW's heights (counted above)";
const NEXT_WORK: &str = "next_work: equals the header's nBits";
const ACCEPTED: &str = "nbits_acceptable: accepts the header (Core accepted it)";
const SUBSIDY: &str = "subsidy: equals Core's (both pins)";

/// A header dump and the chain it belongs to.
struct History {
    raw: Vec<u8>,
    /// `(nTime, nBits)` per height.
    headers: Vec<(u32, u32)>,
    variant: Variant,
    /// The [`RECORDED`] entry of the dump; `None` only with `--unpinned`.
    recorded: Option<&'static RecordedDump>,
}

impl History {
    /// Loads a dump and checks, before anything else runs, that it is the recorded one (unless
    /// `unpinned`) and Core's chain: every header links to its predecessor by X11 hash, and
    /// Core's genesis, checkpoint and other hard-coded block hashes are at their heights.
    fn load(net: &str, path: &Path, unpinned: bool) -> Self {
        let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!raw.is_empty() && raw.len().is_multiple_of(80), "not a file of 80-byte headers");
        let sha256 = hex_str(&dash_sha256::sha256(&raw));
        println!("{}: sha256 {sha256}", path.display());
        let recorded = RECORDED.iter().find(|d| d.net == net && d.sha256 == sha256);
        match recorded {
            Some(d) => assert_eq!(raw.len() / 80, d.headers, "the recorded dump's length"),
            None if unpinned => println!("not a recorded dump (--unpinned): no tally to match"),
            None => panic!(
                "{}: not a {net} dump PROVENANCE.md records; pass --unpinned to use it",
                path.display()
            ),
        }
        let field = |h: usize, off: usize| {
            u32::from_le_bytes(raw[h * 80 + off..h * 80 + off + 4].try_into().unwrap())
        };
        let headers = (0..raw.len() / 80).map(|h| (field(h, 68), field(h, 72))).collect();
        let (name, chain) = match net {
            "main" => ("main", Chain::main()),
            "test" => ("test", Chain::test()),
            _ => panic!("main or test"),
        };
        let cp = cases::chainparams(&Oracle::new(Pin::V24_0), &chain);
        let anchors = check_links(&raw, &cp);
        println!("every header links by X11 hash; {anchors} of Core's block hashes match");
        // the views of dash-params' parameters for the network are Core's (common checks it)
        let variant = Variant::new(name, Pin::V24_0.name(), chain, cp);
        let history = Self {
            raw,
            headers,
            variant,
            recorded,
        };
        // whatever the dump, it reaches every rule the sweep compares
        let past = history.len() - 1;
        let c = history.variant.pow.config();
        assert!(past > history.v20_height(), "the dump ends before V20 ({past})");
        assert!(past > c.dgw_height as usize, "the dump ends before DGW ({past})");
        if net == "main" {
            assert!(past > history.tolerance_until(), "the dump ends in the tolerance");
        }
        history
    }

    /// The last height of mainnet's `nBits` tolerance, `dash-params`' pin (68589).
    fn tolerance_until(&self) -> usize {
        let until = self.variant.pow.config().pre_dgw_tolerance_until.expect("mainnet's pin");
        usize::try_from(until).expect("a height")
    }

    fn len(&self) -> usize {
        self.headers.len()
    }

    /// The hash of the header at `h` (internal byte order): the next header's `hashPrevBlock`.
    fn hash(&self, h: usize) -> Option<[u8; 32]> {
        let next = self.raw.get((h + 1) * 80..(h + 2) * 80)?;
        Some(next[4..36].try_into().unwrap())
    }

    fn v20_height(&self) -> usize {
        self.variant.subsidy.config().v20_height as usize
    }

    /// How many headers before `h` Core's `GetNextWorkRequired` for the block at `h` may read:
    /// BTC's retarget interval or min-difficulty walk (577), KGW's whole lookback (4033), DGW's
    /// 24 blocks.
    fn lookback(&self, h: usize) -> usize {
        let c = self.variant.pow.config();
        let next = h as i64;
        if next < i64::from(c.kgw_height) {
            577
        } else if next < i64::from(c.dgw_height) {
            4033
        } else {
            25
        }
    }

    /// Core's `GetNextWorkRequired` for the block at `h`, at the pin, over a window that reaches
    /// as far back as Core reads ([`Self::lookback`]).
    fn core_next_work(&self, h: usize) -> u32 {
        let start = h.saturating_sub(self.lookback(h));
        let (window, new_header) = (&self.raw[start * 80..h * 80], &self.raw[h * 80..h * 80 + 80]);
        let new_header: &[u8; 80] = new_header.try_into().unwrap();
        both(&format!("GetNextWorkRequired at {h}"), WINDOW, |o| {
            o.next_work_required(&self.variant.chain, window, start as i32, new_header)
        })
        .expect("the window covers Core's lookback")
    }

    /// Core's subsidy split for the block at `h`, at the pin.
    fn core_subsidy(&self, h: usize) -> (i64, i64) {
        let (bits, v20) = (self.headers[h - 1].1, h >= self.v20_height());
        core_subsidy(&self.variant.chain, bits, h as i32 - 1, v20).expect("every block's subsidy")
    }
}

/// Checks that `raw` is Core's chain: each header's `hashPrevBlock` is the X11 hash of the one
/// before, and every block hash Core's chain parameters `cp` hard-code at a height in the dump
/// (genesis, the checkpoints, BIP34, DIP0003 enforcement, the first superblock) is there.
/// Returns how many of those anchors it compared.
fn check_links(raw: &[u8], cp: &serde_json::Value) -> usize {
    let n = raw.len() / 80;
    // X11 over the whole dump, in parallel
    let mut hashes = vec![[0u8; 32]; n];
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
    let per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (headers, out) in raw.chunks(per * 80).zip(hashes.chunks_mut(per)) {
            s.spawn(move || {
                for (header, hash) in headers.as_chunks::<80>().0.iter().zip(out) {
                    *hash = dash_pow::hash(header);
                }
            });
        }
    });
    for h in 1..n {
        assert!(raw[h * 80 + 4..h * 80 + 36] == hashes[h - 1], "hashPrevBlock at {h}");
    }
    // Core's hex is in display order, the reverse of the internal one
    let internal = |hex: &str| -> [u8; 32] {
        let mut bytes: [u8; 32] = parse_hex(hex).try_into().expect("a 32-byte hash");
        bytes.reverse();
        bytes
    };
    let c = &cp["consensus"];
    let mut anchors = vec![(0, cp["genesisHash"].as_str().expect("genesisHash"))];
    for (height, hash) in cp["checkpoints"].as_object().expect("checkpoints") {
        anchors.push((height.parse().expect("a height"), hash.as_str().expect("a hash")));
    }
    for (height, hash) in [
        ("BIP34Height", "BIP34Hash"),
        ("DIP0003EnforcementHeight", "DIP0003EnforcementHash"),
        ("nSuperblockStartBlock", "nSuperblockStartHash"),
    ] {
        anchors.push((c[height].as_u64().expect(height) as usize, c[hash].as_str().expect(hash)));
    }
    let mut compared = 0;
    for (height, hash) in anchors {
        // an all-zero hash is "not set" (testnet's superblock start)
        if height < n && internal(hash) != [0; 32] {
            assert_eq!(
                hex_str(&hashes[height]),
                hex_str(&internal(hash)),
                "Core's hash at {height}"
            );
            compared += 1;
        }
    }
    compared
}

/// Every header of a dump: `CheckProofOfWork`, the `nBits` the port computes against the
/// header's (where Core requires equality) or against Core's `GetNextWorkRequired` (mainnet ≤
/// 68589), and every block's subsidy against Core at the pin.
pub fn run(net: &str, path: &Path, unpinned: bool) {
    let history = History::load(net, path, unpinned);
    let n = history.len();
    let p = history.variant.pow;
    let chain = VecChain::new(0, history.headers.iter().copied());
    println!("{net}: {n} headers (heights 0..={}), V20 at {}", n - 1, history.v20_height());

    let mut tally = Tally::default();
    for h in 0..n - 1 {
        let hash = history.hash(h).unwrap();
        let bits = CompactTarget::new(history.headers[h].1);
        assert!(check_proof_of_work(&hash, bits, &p), "CheckProofOfWork fails at {h}");
        tally.add(POW);
    }

    let c = *p.config();
    let mut highest_differing = None;
    for h in 1..n {
        let (time, bits) = history.headers[h];
        let header_bits = CompactTarget::new(bits);
        let expected = get_next_work_required(&chain, BlockId(h as u32 - 1), time, &p);
        if c.tolerance_applies(h as u32) {
            assert_eq!(expected.to_base(), history.core_next_work(h), "GetNextWorkRequired at {h}");
            tally.add(TOLERANCE_CORE);
            assert!(
                fp::check_tolerance(bits, expected.to_base()),
                "Core accepted the header at {h}"
            );
            tally.add(TOLERANCE_PREDICATE);
            if expected == header_bits {
                tally.add(TOLERANCE_EQUAL);
            } else {
                highest_differing = Some(h);
                tally.add(TOLERANCE_DIFFERENT);
            }
        } else {
            assert_eq!(expected, header_bits, "next_work differs from the header's at {h}");
            tally.add(NEXT_WORK);
        }
        if (c.kgw_height..c.dgw_height).contains(&(h as i32)) {
            tally.add(KGW);
        }
        // Core accepted every header of its chain
        assert!(nbits_acceptable(h as u32, header_bits, expected, &p), "nbits_acceptable at {h}");
        tally.add(ACCEPTED);
    }
    if let Some(h) = highest_differing {
        println!("highest height whose nBits differ from GetNextWorkRequired: {h}");
    }

    for h in 1..n {
        let (prev_bits, prev_height) = (history.headers[h - 1].1, h as i32 - 1);
        let v20 = h >= history.v20_height();
        let split =
            subsidy::block_subsidy_helper(prev_bits, prev_height, v20, &history.variant.subsidy)
                .unwrap_or_else(|e| panic!("subsidy at {h}: {e:?}"));
        assert_eq!((split.block, split.treasury), history.core_subsidy(h), "subsidy at {h}");
        tally.add(SUBSIDY);
    }
    tally.print();
    if let Some(recorded) = history.recorded {
        tally.assert_equals(recorded.tally);
        println!("the tally is the recorded dump's");
    }
}

/// The sample ranges per network: `(check_from, last_height)`. [`sample`] starts each range's
/// window as far back as Core reads for its checked heights ([`History::lookback`]: BTC's
/// retarget interval or min-difficulty walk, KGW's 4032 blocks, DGW's 24).
/// `highest_differing` is mainnet's tolerance evidence, which the sample must hold.
fn sample_ranges(net: &str, n: usize, highest_differing: Option<usize>) -> Vec<(usize, usize)> {
    let tip = (n - 25, n - 1);
    let mut ranges = match net {
        "main" => vec![
            (1, 600),               // BTC from genesis, the first retarget at 576
            (4_495, 4_505),         // the early-mainnet difficulty ends after 4500
            (5_460, 5_470),         // the first formula ends at 5465
            (15_191, 15_210),       // BTC to KGW at 15200
            (16_995, 17_005),       // the CPU formula's height bound, 17000
            (23_995, 24_005),       // the dDiff <= 75 bound ends at 24000
            (34_101, 34_170),       // KGW to DGW at 34140
            (68_565, 68_620),       // the tolerance's end at 68589
            (210_235, 210_245),     // the first decline, from block 210241
            (328_005, 328_015),     // the treasury share, from block 328010
            (1_987_775, 1_987_800), // V20's fixed subsidy at 1987776
            tip,
        ],
        _ => vec![
            (1, 600),           // BTC with the min-difficulty walk
            (3_457, 4_040),     // BTC to DGW at 4002
            (5_460, 5_470),     // the first formula ends at 5465
            (16_995, 17_005),   // the CPU formula's height bound
            (23_995, 24_005),   // the dDiff <= 75 bound
            (905_100, 905_125), // V20 at 905100
            tip,
        ],
    };
    if let Some(h) = highest_differing.filter(|&h| h < 68_565) {
        ranges.push((h, h));
    }
    ranges
}

/// Writes the history sample: Core's headers over [`sample_ranges`], with Core's
/// `GetNextWorkRequired` and subsidy for each checked height, and on mainnet Core's evidence for
/// the tolerance pin: the heights up to 68589 whose header nBits differ from Core's
/// `GetNextWorkRequired`, which Core accepted.
pub fn sample(net: &str, path: &Path, out: &Path, unpinned: bool) {
    let history = History::load(net, path, unpinned);
    let n = history.len();
    let differing: Option<Vec<usize>> = (net == "main").then(|| {
        let until = history.tolerance_until();
        (1..=until).filter(|&h| history.core_next_work(h) != history.headers[h].1).collect()
    });
    let highest_differing = differing.as_ref().and_then(|d| d.last().copied());
    let tolerance = differing.as_ref().map(|d| {
        json!({"checked": [1, history.tolerance_until()], "differing": d.len(),
            "highest_differing": highest_differing})
    });
    println!("tolerance: {tolerance:?}");

    let mut ranges = Vec::new();
    for (check_from, last) in sample_ranges(net, n, highest_differing) {
        let first =
            (check_from..=last).map(|h| h.saturating_sub(history.lookback(h))).min().unwrap();
        // a hash (for CheckProofOfWork) only where the height is checked
        let headers: Vec<_> = (first..=last)
            .map(|h| {
                let (time, bits) = history.headers[h];
                let hash = (h >= check_from).then(|| history.hash(h)).flatten();
                json!([time, bits, hash.map(|x| hex_str(&x))])
            })
            .collect();
        // the window the test builds must give Core's answer at every checked height
        let window = VecChain::new(first as u32, history.headers[first..=last].iter().copied());
        let core: Vec<_> = (check_from..=last)
            .map(|h| {
                let core_bits = history.core_next_work(h);
                let id = BlockId((h - 1 - first) as u32);
                let time = history.headers[h].0;
                let bits = get_next_work_required(&window, id, time, &history.variant.pow);
                assert_eq!(bits.to_base(), core_bits, "the sample window at {h}");
                let (block, treasury) = history.core_subsidy(h);
                json!([core_bits, block, treasury])
            })
            .collect();
        ranges.push(json!({"first_height": first, "check_from": check_from, "headers": headers,
            "core": core}));
    }
    let count = ranges.len();
    let sample =
        json!({"network": net, "headers_in_dump": n, "tolerance": tolerance, "ranges": ranges});
    std::fs::write(out, serde_json::to_string(&sample).unwrap() + "\n").expect("write");
    println!("{}: {count} ranges", out.display());
}

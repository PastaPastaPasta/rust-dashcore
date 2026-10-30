//! The recorded corpus of `dash-consensus/tests/data` (`PROVENANCE.md` there): Core's answers on
//! the pin for inputs drawn from a fixed seed, checked against the port as they are recorded.
//! `--check` regenerates everything in memory and compares it with the files instead.
//!
//! The corpus is a function of the seed and of the order of the generators' draws: a change
//! that adds, drops or reorders a draw changes the corpus, and `--check` shows it.

use std::path::Path;

use dash_consensus::payments::MnRewardEra;
use dash_core_vectors::hex::hex_str;
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{Oracle, Pin};
use dash_types::Numeric;
use serde_json::{Value, json};

use crate::cases::{self, NextWorkCase, Variant};
use crate::common::{ENCODINGS_OF_2_232, ERAS, era_name};
use crate::rng::Rng;
use crate::{
    CORE_ASSERTS, PLATFORM_SHARE_EDGE, Tally, both, check_mn_payment, check_next_work,
    check_payments_limit, check_platform_share, check_pow, check_split, check_subsidy, fp,
    payments_limit_heights, pow_case, subsidy_only,
};

/// The corpus seed.
const SEED: u64 = 0xC520_0001;

/// Writes the corpus under `out`, or with `check` compares it with the files there. Returns
/// whether every file matched and no file under the recorder's directories is stale (one it no
/// longer produces), which it reports but never deletes.
pub fn run(out: &Path, check: bool) -> bool {
    let files = corpus();
    let mut same = true;
    for (relative, text) in &files {
        let path = out.join(relative);
        if check {
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            let equal = if relative.starts_with("chainparams/") {
                // Core's values, not the oracle's version string (which names the shim commit)
                without_version(&old) == without_version(text)
            } else {
                old == *text
            };
            if !equal {
                println!("differs: {relative}");
                same = false;
            }
        } else {
            std::fs::create_dir_all(path.parent().expect("a file")).expect("mkdir");
            std::fs::write(&path, text).expect("write");
        }
    }
    // a file the recorder no longer writes would linger, replayed but never re-checked
    for stale in stale_files(out, &files) {
        println!("stale (no longer recorded; delete it): {stale}");
        same = false;
    }
    println!(
        "{} files {}",
        files.len(),
        if check {
            "checked"
        } else {
            "written"
        }
    );
    same
}

/// The directories under tests/data the recorder owns entirely.
const RECORDED_DIRS: [&str; 2] = ["chainparams", "oracle"];

/// Files under [`RECORDED_DIRS`] that are not in `files`.
fn stale_files(out: &Path, files: &[(String, String)]) -> Vec<String> {
    fn walk(dir: &Path, relative: &str, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("read_dir");
            let name = format!("{relative}/{}", entry.file_name().to_string_lossy());
            if entry.file_type().expect("file type").is_dir() {
                walk(&entry.path(), &name, found);
            } else {
                found.push(name);
            }
        }
    }
    let mut found = Vec::new();
    for dir in RECORDED_DIRS {
        walk(&out.join(dir), dir, &mut found);
    }
    found.retain(|f| !files.iter().any(|(relative, _)| relative == f));
    found.sort();
    found
}

fn without_version(text: &str) -> Option<Value> {
    let mut v: Value = serde_json::from_str(text).ok()?;
    v.as_object_mut()?.remove("core_version");
    Some(v)
}

/// Every file of the corpus, as (path under tests/data, contents).
fn corpus() -> Vec<(String, String)> {
    let v24 = Oracle::new(Pin::V24_0);
    let variants = cases::variants(&v24);
    let mut files = Vec::new();
    let mut tally = Tally::default();

    // Chain parameters: every selection.
    for v in &variants {
        files.push((
            format!("chainparams/v24.0/{}.json", v.name),
            v24.chainparams_json(&v.chain).expect("chainparams_json"),
        ));
    }

    let mut rng = Rng::new(SEED);
    let (mut next_work, mut pow, mut subsidies, mut mn, mut limits) =
        (vec![], vec![], vec![], vec![], vec![]);
    for v in &variants {
        if !subsidy_only(v) {
            let mut recorded = 0;
            while recorded < 150 {
                let case = cases::next_work_case(&mut rng, &v.pow, false, 600);
                if let Some(bits) = check_next_work(v, &case, &mut tally) {
                    next_work.push(next_work_json(None, v, &case, bits));
                    recorded += 1;
                }
            }
            for _ in 0..60 {
                let (hash, bits) = pow_case(&mut rng, v);
                let ok = check_pow(v, &hash, bits);
                pow.push(json!({"chain": v.name, "hash": hex_str(&hash), "bits": bits, "ok": ok}));
            }
        }
        for _ in 0..120 {
            let (bits, h) = (cases::prev_bits(&mut rng, v), cases::subsidy_heights(&mut rng, v));
            let v20 = rng.chance(1, 2);
            let core = check_subsidy(v, bits, h, v20, &mut tally);
            // Core's answers at the same height with two other nBits, the extremes of the
            // floating-point path before V20: more cases the port must match
            let alt = |b: u32| {
                both("subsidy", CORE_ASSERTS, |o| o.block_subsidy_inner(&v.chain, b, h, v20))
            };
            subsidies.push(
                json!({"chain": v.name, "prev_bits": bits, "prev_height": h, "v20": v20,
                "block": core.map(|c| c.0), "treasury": core.map(|c| c.1),
                "block_bits_1": alt(1), "block_bits_207fffff": alt(0x207f_ffff)}),
            );
        }
        if !subsidy_only(v) {
            for _ in 0..80 {
                let height = cases::payment_height(&mut rng, v);
                let (value, era) = (cases::block_value(&mut rng), rng.pick(&ERAS).0);
                let payment = check_mn_payment(v, height, value, era);
                mn.push(json!({"chain": v.name, "height": height, "block_value": value,
                    "era": era_name(era), "payment": payment}));
            }
        }
        for h in payments_limit_heights(v) {
            limits.push(check_payments_limit(v, h, &mut tally));
        }
    }

    let edge = PLATFORM_SHARE_EDGE;
    let mut rewards = vec![0, 1, -1, -2, -3, 100, 1_000_000_000, 21_000_000 * 100_000_000];
    rewards.extend([edge - 1, edge, edge + 1, edge + 2, edge + 3, i64::MAX / 375, i64::MIN / 375]);
    for _ in 0..200 {
        rewards.push(match rng.below(3) {
            0 => rng.range(-1000, edge + 1000),
            1 => rng.range(i64::MIN / 375, i64::MAX / 375),
            _ => rng.range(0, 600 * 100_000_000),
        });
    }
    let shares: Vec<Value> = rewards
        .into_iter()
        .map(|reward| json!({"reward": reward, "share": check_platform_share(reward)}))
        .collect();

    let quirks = quirks(&variants, &mut rng, &mut tally);
    let noncanonical = noncanonical(&variants, &mut tally);

    // the floating point (CS2 phase C): KGW's boundary windows, Core's ConvertBitsToDouble,
    // and the tolerance on Core's doubles
    let kgw = fp::kgw_boundaries(variant(&variants, "main"), &mut tally);
    let mut words = fp::difficulty_edges();
    words.extend((0..1000).map(|_| fp::random_bits(&mut rng)));
    let difficulty: Vec<Value> = words
        .into_iter()
        .map(|bits| json!([bits, format!("{:016x}", fp::check_difficulty(bits))]))
        .collect();
    let tolerance: Vec<Value> = (0..1000)
        .map(|_| {
            let (header, expected) = fp::tolerance_pair(&mut rng);
            json!([header, expected, fp::check_tolerance(header, expected)])
        })
        .collect();

    // the large-spacing devnets and the payout split, from a stream of their own, so the files
    // above keep their draws
    let mut rng = Rng::new(SEED ^ 0x10);
    let mut large_spacing = Vec::new();
    for v in &cases::large_spacing_variants(&v24) {
        files.push((
            format!("chainparams/v24.0/{}.json", v.name),
            v24.chainparams_json(&v.chain).expect("chainparams_json"),
        ));
        let mut recorded = 0;
        while recorded < 100 {
            let case = cases::large_spacing_case(&mut rng, &v.pow);
            if let Some(bits) = check_next_work(v, &case, &mut tally) {
                large_spacing.push(next_work_json(None, v, &case, bits));
                recorded += 1;
            }
        }
    }
    let mut splits: Vec<Value> = (0..300)
        .map(|_| {
            let (total, shares) = cases::split_case(&mut rng);
            json!([total, shares, check_split(total, &shares, &mut tally)])
        })
        .collect();
    // a zero share total over two shares, where Core throws
    splits.push(json!([1000, [0, 0], check_split(1000, &[0, 0], &mut tally)]));
    for (name, value) in [
        ("next_work.json", json!(next_work)),
        ("check_proof_of_work.json", json!(pow)),
        ("subsidy.json", json!(subsidies)),
        ("masternode_payment.json", json!(mn)),
        ("platform_share.json", json!(shares)),
        ("quirks.json", quirks),
        ("noncanonical.json", noncanonical),
        ("payments_limit.json", json!(limits)),
        ("kgw_boundaries.json", json!(kgw)),
        ("convert_bits_to_double.json", json!(difficulty)),
        ("tolerance.json", json!(tolerance)),
        ("next_work_large_spacing.json", json!(large_spacing)),
        ("split_amount_by_shares.json", json!(splits)),
    ] {
        let text = serde_json::to_string(&value).expect("json") + "\n";
        files.push((format!("oracle/{name}"), text));
    }
    tally.print();
    files
}

fn next_work_json(label: Option<&str>, v: &Variant, case: &NextWorkCase, bits: u32) -> Value {
    let mut case = json!({"chain": v.name, "first_height": case.first_height,
        "headers": case.headers, "new_time": case.new_time, "bits": bits});
    if let Some(label) = label {
        case["label"] = json!(label);
    }
    case
}

/// A labelled next-work case, which Core must answer.
fn labelled(label: &str, v: &Variant, case: NextWorkCase, tally: &mut Tally) -> Value {
    let bits = check_next_work(v, &case, tally).expect("the oracle refused a labelled case");
    next_work_json(Some(label), v, &case, bits)
}

/// The chain selection named `name`.
fn variant<'a>(variants: &'a [Variant], name: &str) -> &'a Variant {
    variants.iter().find(|v| v.name == name).expect(name)
}

/// 24 headers from time 1600000000, `spacing` apart, with `bits(k)` for the k-th.
fn window(spacing: i64, bits: impl Fn(usize) -> u32) -> Vec<(u32, u32)> {
    (0..24).map(|k| ((1_600_000_000 + spacing * k as i64) as u32, bits(k))).collect()
}

/// Does the DGW window wrap mod 2^256 in `avg * k + t`? (quirk 8)
fn dgw_wraps(headers: &[(u32, u32)]) -> bool {
    let mut avg = Arith256::ZERO;
    let mut wrapped = false;
    for (k, &(_, bits)) in headers.iter().rev().take(24).enumerate() {
        let k = k as u32 + 1;
        let t = CompactTarget::new(bits).expand().value;
        if k == 1 {
            avg = t;
        } else {
            let (m, o) = avg.mul_u64(u64::from(k));
            let sum = m + t;
            wrapped |= o || sum < m;
            avg = sum / u64::from(k + 1);
        }
    }
    wrapped
}

/// Labelled cases for the quirk tests (8, 9, 11 and 12), Core's answers from the pin.
fn quirks(variants: &[Variant], rng: &mut Rng, tally: &mut Tally) -> Value {
    let main = variant(variants, "main");
    let mut next_work = Vec::new();
    // 23 intervals measured against a 24-spacing target: steady 150 s blocks retarget harder
    let h = window(150, |_| 0x1b04_04cb);
    let t = h[23].0 + 150;
    let case = NextWorkCase::new(1_000_000 - 23, h, t);
    next_work.push(labelled("dgw_23_intervals", main, case, tally));
    // per-step truncation and the k/(k+1) weights: not the mean of the window
    for _ in 0..3 {
        let bits: Vec<u32> =
            (0..24).map(|_| 0x1b00_0000 | rng.range(0x00_8000, 0x7f_ffff) as u32).collect();
        let h = window(150, |k| bits[k]);
        let t = h[23].0 + 150;
        let case = NextWorkCase::new(1_000_000 - 23, h, t);
        next_work.push(labelled("dgw_not_a_mean", main, case, tally));
    }
    // a window whose `avg * k + t` wraps mod 2^256 (only nBits above powLimit can do that),
    // where the wrapped value shows in the result
    let mut found = 0;
    while found < 4 {
        let choices = [0x2100_ffff, 0x2100_8000, 0x2000_ffff, 0x1d00_ffff, 0x1b04_04cb];
        let bits: Vec<u32> = (0..24).map(|_| *rng.pick(&choices)).collect();
        let h = window(rng.range(20, 600), |k| bits[k]);
        if !dgw_wraps(&h) {
            continue;
        }
        let t = h[23].0 + 150;
        let case = NextWorkCase::new(2_000_000 - 23, h, t);
        let core = cases::oracle_next_work(&Oracle::new(Pin::V24_0), &main.chain, &case);
        if core.expect("a DGW window") == main.pow.pow_limit_bits().to_base() {
            continue;
        }
        next_work.push(labelled("dgw_wrap", main, case, tally));
        found += 1;
    }
    // devnet's "10x easier after 4 spacings": `* 10` wraps mod 2^256
    let devnet = variant(variants, "devnet");
    for last_bits in [0x201a_aaaau32, 0x2019_9999, 0x2020_0000, 0x207f_ffff, 0x1e0f_ffff] {
        let h = window(150, |k| {
            if k == 23 {
                last_bits
            } else {
                0x1e0f_ffff
            }
        });
        let t = h[23].0 + 601;
        next_work.push(labelled("devnet_x10", devnet, NextWorkCase::new(5000 - 23, h, t), tally));
    }

    // quirk 12 on mainnet: separate truncation, period 8 skipped, the reallocation start, and
    // the first height of every reallocation period (each entry of Core's vecPeriods table)
    let mut mn = Vec::new();
    let pay = |label: &str, h: i32, value: i64, era: MnRewardEra| {
        let payment = check_mn_payment(main, h, value, era);
        json!({"label": label, "chain": "main", "height": h, "block_value": value,
            "era": era_name(era), "payment": payment})
    };
    for value in [39, 1_234_567_891, 5_000_000_000] {
        mn.push(pay("separate_truncation", 313_521, value, MnRewardEra::Classic));
    }
    for h in [296_240, 296_241, 313_520, 313_521] {
        mn.push(pay("period_8_skipped", h, 5_000_000_007, MnRewardEra::Classic));
    }
    let (start, step) = (1_379_128, 3 * 16_616);
    for h in [1_374_911, 1_374_912, start - 1, start, start + step - 1, start + step] {
        mn.push(pay("reallocation", h, 361_111_111, MnRewardEra::Classic));
    }
    for k in [18, 20] {
        mn.push(pay("reallocation", start + k * step, 361_111_111, MnRewardEra::Classic));
    }
    for (era, _) in ERAS {
        mn.push(pay("era", 2_000_000, 361_111_111, era));
    }
    // Core's own values, not the port's: BRRHeight and nSuperblockCycle from its parameters
    let c = &main.cp["consensus"];
    let (brr, cycle) = (c["BRRHeight"].as_i64().unwrap(), c["nSuperblockCycle"].as_i64().unwrap());
    assert_eq!((brr - brr % cycle + cycle, 3 * cycle), (i64::from(start), i64::from(step)));
    for period in 0..=19 {
        let h = start + period * step;
        mn.push(pay("reallocation_period", h, 1_000_000_000_000, MnRewardEra::Classic));
    }

    // quirk 9: the mainnet <= 68589 tolerance at its boundary, with Core's doubles
    let tolerance: Vec<Value> = fp::tolerance_edges()
        .into_iter()
        .map(|(label, header, expected)| {
            let (n1, n2) = (fp::core_difficulty(header), fp::core_difficulty(expected));
            json!({"label": label, "header_bits": header, "expected_bits": expected,
                "n1": format!("{n1:016x}"), "n2": format!("{n2:016x}"),
                "accepts": fp::check_tolerance(header, expected)})
        })
        .collect();

    // quirk 11: the subsidy from the previous block's nBits and height
    let mut subsidy = Vec::new();
    let mut sub = |label: &str, bits: u32, prev_height: i32| {
        let (block, treasury) =
            check_subsidy(main, bits, prev_height, false, tally).expect("a mainnet subsidy");
        subsidy.push(json!({"label": label, "chain": "main", "prev_bits": bits,
            "prev_height": prev_height, "block": block, "treasury": treasury}));
    };
    // the early difficulty ignores the size byte: encodings of 2^232 differ up to 4500 only
    for bits in ENCODINGS_OF_2_232 {
        sub("early_difficulty", bits, 4500);
        sub("early_difficulty", bits, 4501);
    }
    // truncation, not rounding, before the clamp: the genesis difficulty in each formula
    for prev_height in [5000, 10_000, 30_000] {
        sub("truncation", 0x1e0f_fff0, prev_height);
    }
    // the formula boundaries, with dDiff 16 (<= 75) and 4096 (> 75)
    for bits in [0x1c0f_ffff, 0x1b0f_ffff] {
        for prev_height in [5464, 5465, 16_999, 17_000, 23_999, 24_000] {
            sub("formula_boundary", bits, prev_height);
        }
    }
    // the decline's first passes (blocks 210241 and 420481) and the treasury share (328010)
    for prev_height in [210_239, 210_240, 420_479, 420_480, 328_008, 328_009] {
        sub("decline_and_treasury", 0x1b0f_ffff, prev_height);
    }
    json!({"next_work": next_work, "masternode_payment": mn, "tolerance": tolerance,
        "subsidy": subsidy})
}

/// Non-canonical nBits (CS2 §6.1, GPT M2): three encodings of 2^232, their sign-bit and
/// overflowing neighbours, through CheckProofOfWork at the target and one either side, and
/// windows where GetNextWorkRequired hands a raw word back.
fn noncanonical(variants: &[Variant], tally: &mut Tally) -> Value {
    let words: [u32; 9] = [
        0x1e01_0000,
        0x1f00_0100,
        0x2000_0001, // 2^232, canonical and two non-canonical
        0x1e81_0000,
        0x1f80_0100,
        0x2080_0001, // the sign bit set
        0x2101_0000,
        0x2200_0100,
        0x2300_0001, // overflowing sizes
    ];
    let mut pow = Vec::new();
    for name in ["main", "test", "regtest"] {
        let v = variant(variants, name);
        for bits in words {
            let target = CompactTarget::new(bits).expand().value;
            for (label, hash) in [
                ("target-1", target - Arith256::ONE),
                ("target", target),
                ("target+1", target + Arith256::ONE),
            ] {
                let hash = hash.to_lendian();
                let ok = check_pow(v, &hash, bits);
                pow.push(json!({"label": label, "chain": name, "hash": hex_str(&hash),
                    "bits": bits, "ok": ok}));
            }
        }
    }
    let (main, test, devnet) =
        (variant(variants, "main"), variant(variants, "test"), variant(variants, "devnet"));
    let mut next_work = Vec::new();
    for word in [0x1f00_0100u32, 0x2000_0001] {
        // mainnet BTC between retargets returns the last block's raw nBits
        let h = vec![(1_400_000_000, 0x1d00_ffff), (1_400_000_150, word)];
        let case = NextWorkCase::new(999, h, 1_400_000_300);
        next_work.push(labelled("btc_returns_raw", main, case, tally));
        // testnet's min-difficulty walk skips powLimit blocks and returns the raw word below them
        let limit = test.pow.pow_limit_bits().to_base();
        let h = vec![(1_400_000_000, word), (1_400_000_150, limit), (1_400_000_300, limit)];
        let case = NextWorkCase::new(1001, h, 1_400_000_450);
        next_work.push(labelled("min_difficulty_walk_returns_raw", test, case, tally));
        // DGW over a window holding the word: the average is of decoded targets
        let h = window(150, |k| {
            if k % 2 == 0 {
                word
            } else {
                0x1e01_0000
            }
        });
        let t = h[23].0 + 150;
        let case = NextWorkCase::new(5000 - 23, h, t);
        next_work.push(labelled("dgw_decodes", devnet, case, tally));
    }
    json!({"check_proof_of_work": pow, "next_work": next_work})
}

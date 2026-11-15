//! CS2's differential harness against Dash Core's in-process oracle (`dash-oracle-sys`, both
//! pins). Run it through `run.sh`; `README.md` has the commands and what each one proves.
//!
//! ```text
//! cs2-diff params <pin> <chain> [args..]         print Core's chain parameters
//! cs2-diff record <tests/data> [--check]         write (or check) the recorded corpus
//! cs2-diff diff <cases-per-chain> <seed>         live random differential
//! cs2-diff sweep <main|test> <headers.bin> [--unpinned]   history sweep over a header dump
//! cs2-diff sample <main|test> <headers.bin> <out.json> [--unpinned]   the history sample
//! cs2-diff kgw                                   KGW's boundary windows (CS2 §2.4)
//! cs2-diff exhaustive bits <threads> [<from> <to>]     ConvertBitsToDouble, every nBits
//! cs2-diff exhaustive subsidy <chain> <prev_height> [<from> <to>]   the subsidy, every nBits
//! ```
//!
//! `--dev` (anywhere) allows oracle archives other than `dash-oracle-sys/pins.txt`'s; without it
//! such a build refuses to run. Every command first prints both oracles' Core versions (stderr).

#[path = "../../../../dash-consensus/tests/common/mod.rs"]
mod common;

mod cases;
mod fp;
mod record;
mod rng;
mod sweep;

use std::collections::BTreeMap;
use std::path::Path;

use dash_consensus::payments::MnRewardEra;
use dash_consensus::pow::{check_proof_of_work, get_next_work_required};
use dash_consensus::subsidy::{self, COIN, SubsidyError};
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{ARCHIVES_PINNED, Chain, ErrorKind, Oracle, Pin, RewardEra};
use dash_types::Numeric;
use serde_json::{Value, json};

use crate::cases::{NextWorkCase, Variant};
use crate::common::{ERAS, int, int32};
use crate::rng::Rng;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // archives other than pins.txt's only on request, and every log names the two Cores
    let dev = args.iter().any(|a| a == "--dev");
    args.retain(|a| a != "--dev");
    if !ARCHIVES_PINNED && !dev {
        eprintln!("cs2-diff: dash-oracle-sys links unpinned archives; pass --dev to use them");
        std::process::exit(2);
    }
    for oracle in Oracle::all() {
        eprintln!("oracle {}: {}", oracle.pin(), oracle.core_version());
    }
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_else(|| usage());
    // an optional trailing flag at `i`; anything else there is a usage error
    let flag = |i: usize, name: &str| match args.get(i) {
        None => false,
        Some(a) if a == name && args.len() == i + 1 => true,
        Some(_) => usage(),
    };
    match arg(0) {
        "params" => {
            let pin = Pin::ALL.into_iter().find(|p| p.name() == arg(1)).unwrap_or_else(|| usage());
            let rest: Vec<&str> = args[3..].iter().map(String::as_str).collect();
            let chain = Chain::new(arg(2), &rest);
            println!("{}", Oracle::new(pin).chainparams_json(&chain).expect("chainparams_json"));
        }
        "record" => {
            let check = flag(2, "--check");
            if !record::run(Path::new(arg(1)), check) {
                std::process::exit(1);
            }
        }
        "diff" => diff(arg(1).parse().expect("cases"), arg(2).parse().expect("seed")),
        "sweep" => sweep::run(arg(1), Path::new(arg(2)), flag(3, "--unpinned")),
        "sample" => {
            sweep::sample(arg(1), Path::new(arg(2)), Path::new(arg(3)), flag(4, "--unpinned"))
        }
        "kgw" => {
            let mut tally = Tally::default();
            let main = cases::chainparams(&Oracle::new(Pin::V24_0), &Chain::main());
            let v = Variant::new("main", Pin::V24_0.name(), Chain::main(), main);
            fp::kgw_boundaries(&v, &mut tally);
            tally.print();
        }
        "exhaustive" => {
            // an optional [<from> <to>) slice of the nBits space, 0..2^32 by default
            let slice = |i: usize| match (args.get(i), args.get(i + 1)) {
                (None, None) => (0, 1u64 << 32),
                (Some(from), Some(to)) => (from.parse().expect("from"), to.parse().expect("to")),
                _ => usage(),
            };
            match arg(1) {
                "bits" => fp::exhaustive_bits(arg(2).parse().expect("threads"), slice(3)),
                "subsidy" => {
                    fp::exhaustive_subsidy(arg(2), arg(3).parse().expect("height"), slice(4))
                }
                _ => usage(),
            }
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: cs2-diff params <pin> <chain> [args..] | record <tests/data> [--check] | \
         diff <cases> <seed> | sweep <main|test> <headers.bin> [--unpinned] | \
         sample <main|test> <headers.bin> <out.json> [--unpinned] | kgw | \
         exhaustive bits <threads> [<from> <to>] | \
         exhaustive subsidy <chain> <prev_height> [<from> <to>]; \
         --dev anywhere allows unpinned oracle archives"
    );
    std::process::exit(2)
}

pub fn era_oracle(e: MnRewardEra) -> RewardEra {
    match e {
        MnRewardEra::Classic => RewardEra::Classic,
        MnRewardEra::CreditPool => RewardEra::CreditPool,
        MnRewardEra::EvoReward => RewardEra::EvoReward,
    }
}

/// The refusals a next-work window may meet: shorter than Core's lookback.
pub const WINDOW: &[ErrorKind] = &[ErrorKind::Context];
/// The refusals of an input Core asserts on or overflows with.
pub const CORE_ASSERTS: &[ErrorKind] = &[ErrorKind::Argument];
/// No refusal is expected.
pub const NONE: &[ErrorKind] = &[];

/// The answer of every linked pin, which must agree (one pin today, DEC-82). `None`: the pins
/// refused the input, with an error kind in `refusals`; any other error panics, so a refusal
/// where none is due cannot pass unseen.
pub fn both<T: PartialEq + std::fmt::Debug>(
    what: &str,
    refusals: &[ErrorKind],
    f: impl Fn(&Oracle) -> Result<T, dash_oracle_sys::Error>,
) -> Option<T> {
    let answers: Vec<Option<T>> = Oracle::all()
        .map(|o| match f(&o) {
            Ok(v) => Some(v),
            Err(e) if refusals.contains(&e.kind) => None,
            Err(e) => panic!("{what}: {} failed: {e}", o.pin()),
        })
        .collect();
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{what}: the pins disagree: {answers:?}");
    answers.into_iter().next().expect("a linked pin")
}

/// Counts of what was compared, printed at the end of each command.
#[derive(Default)]
pub struct Tally(BTreeMap<String, u64>);

impl Tally {
    pub fn add(&mut self, k: impl Into<String>) {
        *self.0.entry(k.into()).or_default() += 1;
    }

    pub fn get(&self, k: &str) -> u64 {
        self.0.get(k).copied().unwrap_or(0)
    }

    pub fn print(&self) {
        for (k, n) in &self.0 {
            println!("{n:>9}  {k}");
        }
    }

    /// Panics unless the tally is exactly `expected`: the same keys, the same counts.
    pub fn assert_equals(&self, expected: &[(u64, &str)]) {
        let expected: BTreeMap<String, u64> =
            expected.iter().map(|&(n, k)| (k.to_owned(), n)).collect();
        assert_eq!(self.0, expected, "the tally differs from the recorded one");
    }
}

/// One next-work case at the pin; Core's bits when it gave them.
pub fn check_next_work(v: &Variant, case: &NextWorkCase, tally: &mut Tally) -> Option<u32> {
    let what = format!("next_work {} h={}", v.name, case.last_height());
    let Some(core) = both(&what, WINDOW, |o| cases::oracle_next_work(o, &v.chain, case)) else {
        tally.add("next_work: oracle refused the window");
        return None;
    };
    let chain = common::VecChain::new(case.first_height, case.headers.iter().copied());
    let bits = get_next_work_required(&chain, chain.tip(), case.new_time, &v.pow);
    assert_eq!(
        bits.to_base(),
        core,
        "next_work mismatch: {} first {} headers {:?} new_time {}",
        v.name,
        case.first_height,
        case.headers,
        case.new_time
    );
    tally.add(if cases::reaches_kgw(&v.pow, case) {
        "next_work: equal (KGW)"
    } else {
        "next_work: equal"
    });
    Some(core)
}

/// A `(hash, nBits)` pair for `CheckProofOfWork`: at, around and away from the target.
pub fn pow_case(rng: &mut Rng, v: &Variant) -> ([u8; 32], u32) {
    let pow_limit_bits = v.pow.pow_limit_bits().to_base();
    let bits = match rng.below(4) {
        0 => pow_limit_bits,
        1 => rng.next() as u32,
        _ => {
            let exp = rng.range(0x01, 0x22) as u32;
            (exp << 24) | (rng.range(0, 0xff_ffff) as u32)
        }
    };
    let target = CompactTarget::new(bits).expand().value;
    let mut hash = match rng.below(4) {
        0 => target.to_lendian(),
        1 => (target - Arith256::ONE).to_lendian(),
        2 => (target + Arith256::ONE).to_lendian(),
        _ => std::array::from_fn(|_| rng.next() as u8),
    };
    if rng.chance(1, 8) {
        hash = [0u8; 32];
    }
    (hash, bits)
}

/// `CheckProofOfWork` at the pin against the port; Core's verdict.
pub fn check_pow(v: &Variant, hash: &[u8; 32], bits: u32) -> bool {
    let core = both("check_pow", NONE, |o| o.check_proof_of_work(&v.chain, hash, bits))
        .expect("CheckProofOfWork refuses nothing");
    let ours = check_proof_of_work(hash, CompactTarget::new(bits), &v.pow);
    assert_eq!(ours, core, "check_pow {} {bits:#x} {hash:02x?}", v.name);
    core
}

/// `GetMasternodePayment` at the pin against the port; Core's payment.
pub fn check_mn_payment(v: &Variant, height: i32, value: i64, era: MnRewardEra) -> i64 {
    let core = both("mn_payment", NONE, |o| o.mn_payment(&v.chain, height, value, era_oracle(era)))
        .expect("the generators stay inside GetMasternodePayment's domain");
    let ours = dash_consensus::payments::masternode_payment(height, value, era, &v.payments);
    assert_eq!(ours, core, "mn_payment {} {height} {value} {era:?}", v.name);
    core
}

/// Core's `GetBlockSubsidyInner` and `GetSuperblockSubsidyInner` at the pin, or `None` where
/// both refuse.
pub fn core_subsidy(chain: &Chain, bits: u32, prev_height: i32, v20: bool) -> Option<(i64, i64)> {
    let block =
        both("subsidy", CORE_ASSERTS, |o| o.block_subsidy_inner(chain, bits, prev_height, v20));
    let treasury = both("treasury", CORE_ASSERTS, |o| {
        o.superblock_subsidy_inner(chain, bits, prev_height, v20)
    });
    assert_eq!(block.is_some(), treasury.is_some(), "subsidy at {prev_height}");
    block.zip(treasury)
}

/// The subsidy split against Core at the pin; Core's split.
pub fn check_subsidy(
    v: &Variant,
    bits: u32,
    h: i32,
    v20: bool,
    tally: &mut Tally,
) -> Option<(i64, i64)> {
    let core = core_subsidy(&v.chain, bits, h, v20);
    match (core, subsidy::block_subsidy_helper(bits, h, v20, &v.subsidy)) {
        (Some((b, t)), Ok(split)) => {
            assert_eq!(
                (split.block, split.treasury),
                (b, t),
                "subsidy {} {bits:#x} {h} {v20}",
                v.name
            );
            tally.add("subsidy: equal");
        }
        (None, Err(SubsidyError::Unreachable)) => tally.add("subsidy: both refuse (Core asserts)"),
        (None, _) => {
            // the oracle also refuses heights whose halving loop overflows int (Core's UB)
            let iv = v.subsidy.config().halving_interval;
            assert!(h >= (i32::MAX / iv) * iv, "the oracle refused {} {h} {v20}", v.name);
            tally.add("subsidy: oracle refuses (halving-loop overflow)");
        }
        other => panic!("subsidy {} {bits:#x} {h} {v20}: {other:?}", v.name),
    }
    core
}

/// Core's `SplitAmountByShares` (O1b) against the port; Core's
/// amounts, or `None` where Core throws on a zero share total over two or more shares.
pub fn check_split(total: i64, shares: &[i64], tally: &mut Tally) -> Option<Vec<i64>> {
    let ours = dash_consensus::payments::split_amount_by_shares(total, shares);
    match Oracle::new(Pin::V24_0).split_amount_by_shares(total, shares) {
        Ok(core) => {
            assert_eq!(ours.as_ref(), Ok(&core), "split {total} {shares:?}");
            tally.add("split_amount_by_shares: equal (v24.0)");
            Some(core)
        }
        Err(e) if e.kind == ErrorKind::Exception => {
            let zero = Err(dash_consensus::payments::PaymentError::ZeroShareTotal);
            assert_eq!(ours, zero, "split {total} {shares:?}: Core threw {e}");
            tally.add("split_amount_by_shares: both refuse (a zero share total)");
            None
        }
        Err(e) => panic!("split {total} {shares:?}: {e}"),
    }
}

/// The smallest reward whose platform share leaves `MoneyRange`, about.
pub const PLATFORM_SHARE_EDGE: i64 = 21_000_000 * COIN * 1000 / 375;

/// `PlatformShare` at the pin against the port; Core's share, or `None` where it asserts.
pub fn check_platform_share(reward: i64) -> Option<i64> {
    let core = both("platform_share", CORE_ASSERTS, |o| o.platform_share(reward));
    let ours = dash_consensus::payments::platform_share(reward).ok();
    assert_eq!(ours, core, "platform_share {reward}");
    core
}

/// `CSuperblock::GetPaymentsLimit`'s test heights for a chain: superblock heights around the
/// first superblock and around V20Height, one height after each, and heights before the first.
pub fn payments_limit_heights(v: &Variant) -> Vec<i32> {
    let c = &v.cp["consensus"];
    let (start, cycle) = (int(&c["nSuperblockStartBlock"]), int(&c["nSuperblockCycle"]));
    let first = (start + cycle - 1) / cycle * cycle;
    let v20_cycle = int(&c["V20Height"]) / cycle * cycle;
    let mut heights = Vec::new();
    for base in [first, v20_cycle, v20_cycle + cycle] {
        for k in [-2, -1, 0, 1, 2, 50, 500] {
            for off in [0, 1] {
                if let Ok(h) = i32::try_from(base + k * cycle + off) {
                    heights.push(h);
                }
            }
        }
    }
    heights.sort_unstable();
    heights.dedup();
    heights
}

/// Core's `GetPaymentsLimit` at `h` (O1b) against the port, with the two treasury parts it
/// could have been computed from: with `nBits` 1 and with the powLimit compact (quirk 15).
/// `superblock` is computed here from Core's parameters, as a label of the input.
pub fn check_payments_limit(v: &Variant, h: i32, tally: &mut Tally) -> Value {
    let limit = both("payments_limit", CORE_ASSERTS, |o| o.superblock_payments_limit(&v.chain, h));
    let c = &v.cp["consensus"];
    let superblock =
        h >= int32(&c["nSuperblockStartBlock"]) && i64::from(h) % int(&c["nSuperblockCycle"]) == 0;
    let v20 = h >= int32(&c["V20Height"]);
    let treasury = |bits: u32| {
        let what = "payments_limit treasury";
        both(what, CORE_ASSERTS, |o| o.superblock_subsidy_inner(&v.chain, bits, h - 1, v20))
    };
    let pow_limit_bits = v.pow.pow_limit_bits().to_base();
    let (with_1, with_pow_limit) = if superblock {
        (treasury(1), treasury(pow_limit_bits))
    } else {
        (None, None)
    };
    match (limit, subsidy::superblock_payments_limit(h, &v.subsidy)) {
        (Some(core), Ok(ours)) => {
            assert_eq!(ours, core, "payments_limit {} {h}", v.name);
            tally.add("payments_limit: equal");
        }
        (None, Err(SubsidyError::Unreachable)) => tally.add("payments_limit: both refuse"),
        other => panic!("payments_limit {} {h}: {other:?}", v.name),
    }
    json!({"chain": v.name, "height": h, "superblock": superblock, "limit": limit,
        "treasury_bits_1": with_1, "treasury_bits_pow_limit": with_pow_limit})
}

/// Whether the oracle's chains skip next-work and proof-of-work cases: the high-subsidy devnets
/// and the budget regtest differ from their base chains only in the subsidy.
pub fn subsidy_only(v: &Variant) -> bool {
    v.name.starts_with("devnet-highsubsidy") || v.name == "regtest-budget"
}

/// Live random differential: `per` cases per chain selection and kind, at the pin.
fn diff(per: u64, seed: u64) {
    let variants = cases::variants(&Oracle::new(Pin::V24_0));
    let mut tally = Tally::default();
    let mut rng = Rng::new(seed);
    let mut windows = 0;
    for v in &variants {
        if !subsidy_only(v) {
            for _ in 0..per {
                let kgw = rng.chance(1, 50);
                let case = cases::next_work_case(&mut rng, &v.pow, kgw, 5000);
                check_next_work(v, &case, &mut tally);
                windows += 1;
            }
            for _ in 0..per {
                let (hash, bits) = pow_case(&mut rng, v);
                check_pow(v, &hash, bits);
                tally.add("check_pow: equal");
            }
            for _ in 0..per {
                let height = cases::payment_height(&mut rng, v);
                let (value, era) = (cases::block_value(&mut rng), rng.pick(&ERAS).0);
                check_mn_payment(v, height, value, era);
                tally.add("mn_payment: equal");
            }
        }
        for _ in 0..per {
            let (bits, h) = (cases::prev_bits(&mut rng, v), cases::subsidy_heights(&mut rng, v));
            check_subsidy(v, bits, h, rng.chance(1, 2), &mut tally);
        }
        for h in payments_limit_heights(v) {
            check_payments_limit(v, h, &mut tally);
        }
    }
    for v in &cases::large_spacing_variants(&Oracle::new(Pin::V24_0)) {
        for _ in 0..per {
            let case = cases::large_spacing_case(&mut rng, &v.pow);
            check_next_work(v, &case, &mut tally);
        }
    }
    for _ in 0..per {
        let (total, shares) = cases::split_case(&mut rng);
        check_split(total, &shares, &mut tally);
    }
    for _ in 0..per * 10 {
        let bits = fp::random_bits(&mut rng);
        fp::check_difficulty(bits);
        tally.add("ConvertBitsToDouble: equal");
        let (header, expected) = fp::tolerance_pair(&mut rng);
        fp::check_tolerance(header, expected);
        tally.add("tolerance: equal to Core's predicate on Core's doubles");
    }
    let edge = PLATFORM_SHARE_EDGE;
    for _ in 0..per * 10 {
        let reward = match rng.below(3) {
            0 => rng.range(-1000, edge + 1000),
            1 => rng.range(i64::MIN / 375, i64::MAX / 375),
            _ => rng.range(edge - 10, edge + 10),
        };
        check_platform_share(reward);
        tally.add("platform_share: equal");
    }
    tally.print();
    // A generator that only produced windows Core refuses would compare nothing.
    let compared = tally.get("next_work: equal") + tally.get("next_work: equal (KGW)");
    assert!(compared * 10 >= windows * 9, "only {compared} of {windows} windows were compared");
}

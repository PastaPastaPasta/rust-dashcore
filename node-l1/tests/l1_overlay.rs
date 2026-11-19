//! The overlay targets (O2c) on their recorded inputs and on deterministic mutations of them.
//!
//! Every recorded input agrees with Core: BS8's corpus (with the strict flags it marks counted
//! apart, and no others) and the O4 fixtures. The mutation twin replays BS8's mutations
//! (`contrib/node/dde-probe/bs8` `fuzz.rs`, without its BLS sample pool) and the header bytes the
//! fuzz targets read, a fixed number per command, and fails on any divergence. Inputs are never
//! printed (PLAN §7.5): a failure names the command and the field.

use std::collections::BTreeMap;
use std::fmt;

use dash_oracle_sys::{P2P_COMMANDS, Pin};
use node_l1::Outcome;
use node_l1::overlay::{Case, compare_case};

#[path = "support/overlay_corpus.rs"]
mod overlay_corpus;

use overlay_corpus::{MAX_SEED, Recorded, bs8_corpus, o4_fixtures};

/// Mutations per command.
const MUTATIONS: usize = 300;

#[derive(Default)]
struct Tally {
    accept: usize,
    reject: usize,
    strict: usize,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::BothAccept => self.accept += 1,
            Outcome::BothReject => self.reject += 1,
            Outcome::StrictFlag => self.strict += 1,
            Outcome::OutOfDomain | Outcome::Discarded => {
                panic!("the overlay comparisons compare every input")
            }
        }
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} read, {} refused, {} strict", self.accept, self.reject, self.strict)
    }
}

/// Each command's outcomes; a case is a strict flag exactly when the corpus marks it.
fn tally(recorded: &[Recorded]) -> BTreeMap<&'static str, Tally> {
    let mut tallies = BTreeMap::<_, Tally>::new();
    for r in recorded {
        let outcome = compare_case(&r.case, Pin::V24_0)
            .unwrap_or_else(|d| panic!("{}: diverges in {d}", r.name));
        assert_eq!(outcome == Outcome::StrictFlag, r.strict, "{}: the strict-flag mark", r.name);
        tallies.entry(r.case.cmd).or_default().add(outcome);
    }
    tallies
}

#[test]
fn bs8_corpus_agrees() {
    let corpus = bs8_corpus();
    assert_eq!(corpus.len(), 445);
    let tallies = tally(&corpus);
    assert_eq!(tallies.len(), P2P_COMMANDS.len(), "every command has cases");
    let strict: usize = tallies.values().map(|t| t.strict).sum();
    assert_eq!(strict, 12, "the corpus's strict flags");
    for (cmd, t) in &tallies {
        eprintln!("{cmd}: {t}");
    }
}

#[test]
fn o4_fixtures_agree() {
    let fixtures = o4_fixtures();
    assert_eq!(fixtures.len(), 37);
    let tallies = tally(&fixtures);
    let read: usize = tallies.values().map(|t| t.accept).sum();
    assert_eq!(read, 37, "Core and the node read every fixture whole");
}

/// The fuzz targets' input of a case survives the round trip.
#[test]
fn inputs_encode_cases() {
    for r in bs8_corpus() {
        let input = r.case.to_input().expect(&r.name);
        assert!(Case::from_input(r.case.cmd, &input) == r.case, "{}", r.name);
    }
}

/// Every command is a registered target that compares on v24.0 alone (DEC-82).
#[test]
fn every_command_is_a_v24_target() {
    for cmd in P2P_COMMANDS {
        let target = node_l1::target(&format!("diff_p2p_{cmd}")).expect(cmd);
        assert_eq!(target.pins, [Pin::V24_0], "{cmd}");
    }
}

/// xorshift64* (BS8's `Rng`).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// A CompactSize prefix of `n`, possibly in a non-canonical width.
fn compact(n: u64, width: usize) -> Vec<u8> {
    match width {
        0 if n < 0xfd => vec![n as u8],
        0 | 1 if n <= 0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        0..=2 if n <= 0xffff_ffff => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}

/// BS8's byte mutations of `seed`, splicing from `others`.
fn mutate(rng: &mut Rng, seed: &[u8], others: &[&[u8]]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..=rng.below(4) {
        let pos = rng.below(b.len().max(1));
        match rng.below(9) {
            0 => b.truncate(pos),
            1 if !b.is_empty() => b[pos] ^= 1 << rng.below(8),
            2 if !b.is_empty() => {
                b[pos] = [0x00, 0xfc, 0xfd, 0xfe, 0xff, 0x01, 0x02, 0x03, 0x04][rng.below(9)]
            }
            3 => {
                let n = [
                    0u64,
                    1,
                    2,
                    0xfc,
                    0xfd,
                    0xffff,
                    0x1000,
                    0x1001,
                    0x0200_0000,
                    0x0200_0001,
                    u64::MAX,
                ][rng.below(11)];
                let c = compact(n, rng.below(4));
                let end = (pos + 1 + rng.below(9)).min(b.len());
                b.splice(pos..end, c);
            }
            4 => {
                let extra: Vec<u8> = (0..rng.below(16)).map(|_| rng.next() as u8).collect();
                b.splice(pos..pos, extra);
            }
            5 if !b.is_empty() => {
                let end = (pos + 1 + rng.below(32)).min(b.len());
                b.drain(pos..end);
            }
            6 => {
                let other = others[rng.below(others.len())];
                let start = rng.below(other.len());
                let end = (start + rng.below(64)).min(other.len());
                b.splice(pos..pos, other[start..end].iter().copied());
            }
            7 => b.extend((0..rng.below(8)).map(|_| rng.next() as u8)),
            _ => {}
        }
    }
    b
}

#[test]
fn mutations_agree() {
    let recorded: Vec<Recorded> = bs8_corpus()
        .into_iter()
        .chain(o4_fixtures())
        .filter(|r| r.case.raw.len() <= MAX_SEED)
        .collect();
    for (i, cmd) in P2P_COMMANDS.into_iter().enumerate() {
        let seeds: Vec<&Case> = recorded.iter().map(|r| &r.case).filter(|c| c.cmd == cmd).collect();
        let others: Vec<&[u8]> = seeds.iter().map(|c| &c.raw[..]).collect();
        let mut rng = Rng(0x0b5e_ed00 + i as u64);
        let mut tally = Tally::default();
        for _ in 0..MUTATIONS {
            let seed = seeds[rng.below(seeds.len())];
            // The header half the time (the fuzz targets' version, scheme and limits).
            let mut input = seed.to_input().expect("encodable");
            if rng.below(2) == 0 {
                input[0] = rng.next() as u8;
            }
            let raw = mutate(&mut rng, &seed.raw, &others);
            input.truncate(input.len() - seed.raw.len());
            input.extend(raw);
            tally.add(
                compare_case(&Case::from_input(cmd, &input), Pin::V24_0)
                    .unwrap_or_else(|d| panic!("{cmd}: a mutation diverges in {d}")),
            );
        }
        eprintln!("{cmd}: {tally}");
        assert!(tally.accept + tally.strict > 0, "{cmd}: no mutation was read");
    }
}

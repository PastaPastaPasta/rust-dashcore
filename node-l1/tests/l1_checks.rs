//! Coverage of the CS3 targets (`diff_check_transaction`, `diff_check_block`,
//! `diff_contextual_check_tx`, `diff_contextual_check_block_header`,
//! `diff_contextual_check_block`): each runs over a fixed set of generated inputs, every
//! comparison must agree, and the agreements must reach the reject reasons the target exists to
//! compare, so a generator that stops reaching a rule fails here rather than passing vacuously.
//! For the same reason, at least half the inputs must reach a check's verdict (not a decode
//! rejection both sides share) and at most a fifth may fall outside the oracle's domain.
//!
//! Output: one line per target with the counts (check verdicts, decode rejections, inputs out of
//! domain) and the reasons reached (Core's reject strings and counts; no input).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/checks_gen.rs"]
mod checks_gen;
#[path = "../../dash-consensus/tests/support/gen.rs"]
mod generate;

use std::collections::BTreeMap;

use dash_oracle_sys::Pin;
use node_l1::check_inputs::ours;
use node_l1::{
    Compared, Outcome, compare_check_block, compare_check_transaction,
    compare_contextual_check_block, compare_contextual_check_block_header,
    compare_contextual_check_tx,
};
use node_sink::sink::record_for_test;

use generate::Rng;

struct Census {
    target: &'static str,
    cases: u64,
    /// Inputs both sides accept.
    accepted: u64,
    /// Inputs both sides reject in a check, alike.
    check_rejects: u64,
    /// Inputs both sides fail to decode (or that are too short for the format).
    decode_rejects: u64,
    /// Inputs the oracle refuses as outside its domain: nothing compared.
    out_of_domain: u64,
    reasons: BTreeMap<String, usize>,
    failures: Vec<String>,
}

impl Census {
    fn run(
        target: &'static str,
        cases: u64,
        build: fn(&mut Rng) -> Vec<u8>,
        compare: fn(&[u8], Pin) -> Compared,
    ) -> Self {
        let mut census = Census {
            target,
            cases,
            accepted: 0,
            check_rejects: 0,
            decode_rejects: 0,
            out_of_domain: 0,
            reasons: BTreeMap::new(),
            failures: Vec::new(),
        };
        for seed in 0..cases {
            let bytes = build(&mut Rng::new(seed ^ 0xc5_3000));
            let outcome = match compare(&bytes, Pin::V24_0) {
                Ok(outcome) => outcome,
                Err(divergence) => {
                    let recorded = record_for_test(target, &bytes, "v24.0", divergence.field);
                    census.failures.push(format!("case {seed}: {divergence} ({recorded})"));
                    continue;
                }
            };
            match outcome {
                Outcome::BothAccept => census.accepted += 1,
                Outcome::OutOfDomain => census.out_of_domain += 1,
                Outcome::StrictFlag => census.failures.push(format!("case {seed}: a strict flag")),
                Outcome::Discarded => census.failures.push(format!("case {seed}: discarded")),
                Outcome::BothReject => match ours(target, &bytes) {
                    Some(Err((_, reason, _))) => {
                        census.check_rejects += 1;
                        *census.reasons.entry(reason).or_default() += 1;
                    }
                    _ => census.decode_rejects += 1,
                },
            }
        }
        census
    }

    /// Fails unless every comparison agreed, some inputs were accepted, at least half reached a
    /// check's verdict and at most a fifth were out of domain, and every reason in `required`
    /// was reached.
    fn check(&self, required: &[&str]) {
        let reasons: Vec<String> =
            self.reasons.iter().map(|(reason, n)| format!("{reason} {n}")).collect();
        let checked = self.accepted + self.check_rejects;
        eprintln!(
            "{}: {} cases: {checked} check verdicts ({} accepted), {} decode rejections, {} out of domain | {}",
            self.target,
            self.cases,
            self.accepted,
            self.decode_rejects,
            self.out_of_domain,
            reasons.join(", ")
        );
        assert!(self.failures.is_empty(), "{}: {}", self.target, self.failures.join("; "));
        assert!(self.accepted > 0, "{}: nothing accepted", self.target);
        assert!(2 * checked >= self.cases, "{}: {checked} check verdicts", self.target);
        assert!(
            5 * self.out_of_domain <= self.cases,
            "{}: {} out of domain",
            self.target,
            self.out_of_domain
        );
        let missing: Vec<&&str> =
            required.iter().filter(|r| !self.reasons.contains_key(**r)).collect();
        assert!(missing.is_empty(), "{}: never reached {missing:?}", self.target);
    }
}

#[test]
fn check_transaction() {
    Census::run(
        "diff_check_transaction",
        3000,
        |rng| {
            if rng.chance(4) {
                let height = rng.below(3_000_000) as u32;
                let coinbase = checks_gen::coinbase(rng, height);
                generate::mutate(rng, coinbase)
            } else {
                generate::mutated(rng.next_u64(), generate::tx_bytes)
            }
        },
        compare_check_transaction,
    )
    .check(&[
        "bad-txns-vin-empty",
        "bad-txns-vout-empty",
        "bad-txns-vout-negative",
        "bad-txns-vout-toolarge",
        "bad-txns-txouttotal-toolarge",
        "bad-txns-prevout-null",
        "bad-cb-length",
    ]);
}

#[test]
fn check_block() {
    Census::run("diff_check_block", 3000, checks_gen::check_block_bytes, compare_check_block)
        .check(&[
            "high-hash",
            "bad-txnmrklroot",
            "bad-txns-duplicate",
            "bad-blk-length",
            "bad-cb-missing",
            "bad-cb-multiple",
            "bad-cb-length",
        ]);
}

#[test]
fn contextual_check_tx() {
    Census::run(
        "diff_contextual_check_tx",
        3000,
        checks_gen::contextual_tx_bytes,
        compare_contextual_check_tx,
    )
    .check(&["bad-txns-type", "bad-txns-cb-type"]);
}

#[test]
fn contextual_check_block() {
    Census::run(
        "diff_contextual_check_block",
        2000,
        checks_gen::contextual_block_bytes,
        compare_contextual_check_block,
    )
    .check(&[
        "bad-blk-length",
        "bad-txns-nonfinal",
        "bad-txns-type",
        "bad-cb-height",
        "bad-cb-type",
    ]);
}

#[test]
fn contextual_check_block_header() {
    Census::run(
        "diff_contextual_check_block_header",
        800,
        checks_gen::contextual_header_bytes,
        compare_contextual_check_block_header,
    )
    .check(&[
        "bad-diffbits",
        "bad-fork-prior-to-checkpoint",
        "time-too-old",
        "time-too-new",
        "bad-version(0x00000001)",
        "bad-version(0x00000002)",
        "bad-version(0x00000003)",
        "bad-version(0x00000000)",
        "bad-version(0xffffffff)",
    ]);
}

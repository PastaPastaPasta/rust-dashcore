//! Shared by the `bls_wrapper` targets' tests (`l1_bls_aggregate`, `l1_bls_threshold`,
//! `l1_bls_ies`, `l1_bls_verify`): seed replays, random and mutated scripts, and coverage
//! checks.

use std::collections::BTreeSet;

use dash_oracle_sys::Pin;
use dash_oracle_sys::ffi::{ORACLE_BLS_FLAG_LEGACY, ORACLE_E_ARG, ORACLE_OK};
use node_l1::bls_arith::{Hit, OpKind};
use node_l1::bls_wrapper::{WrapperOp, read, selector};
use node_l1::{Compared, Outcome};

pub const PIN: Pin = Pin::V24_0;

/// A target's op table and comparison.
pub struct Target {
    pub name: &'static str,
    pub table: &'static [WrapperOp],
    pub compare: fn(&[u8], Pin) -> Compared,
}

/// Splitmix64 bytes from `seed`.
pub fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

/// The target's public seeds (`node-fuzz/public-seeds/<name>`).
pub fn seeds(t: &Target) -> Vec<(String, Vec<u8>)> {
    let dir = format!("{}/../node-fuzz/public-seeds/{}", env!("CARGO_MANIFEST_DIR"), t.name);
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .expect("the public seeds")
        .map(|e| {
            let path = e.expect("an entry").path();
            (path.display().to_string(), std::fs::read(&path).expect("a seed"))
        })
        .collect();
    out.sort();
    out
}

/// The node agrees with Core on every public seed, and there are at least `min`.
pub fn public_seeds_agree(t: &Target, min: usize) {
    let seeds = seeds(t);
    for (path, input) in &seeds {
        assert!(!read(t.table, input).code.is_empty(), "{path}");
        assert_eq!((t.compare)(input, PIN), Ok(Outcome::BothAccept), "{path}");
    }
    assert!(seeds.len() >= min, "{} seeds", seeds.len());
    eprintln!("{}: {} seeds", t.name, seeds.len());
}

fn agrees(t: &Target, input: &[u8], what: &str) {
    let outcome = (t.compare)(input, PIN);
    assert!(
        matches!(outcome, Ok(Outcome::BothAccept | Outcome::OutOfDomain)),
        "{what}: {outcome:?}"
    );
}

/// A random script from `seed`, the first op a load (one of the table's three), so that later
/// ops find registers.
fn random_script(t: &Target, seed: u64) -> Vec<u8> {
    let loads = [OpKind::LoadPoint, OpKind::LoadSk, OpKind::LoadTorsion];
    let load = selector(t.table, WrapperOp::Base(loads[(seed % 3) as usize]), false, false, false)
        .expect("a load in the table");
    let mut input = bytes(seed, 900);
    input[0] = input[0] & 0xe0 | load;
    input
}

/// Random scripts ([`random_script`]).
pub fn random_scripts_agree(t: &Target, n: u64) {
    for seed in 0..n {
        agrees(t, &random_script(t, seed), &format!("seed {seed}"));
    }
}

/// Each public seed with one to three bytes changed or the script cut short.
pub fn mutated_seeds_agree(t: &Target, per_seed: u64) {
    for (i, (path, seed)) in seeds(t).iter().enumerate() {
        for k in 0..per_seed {
            let r = bytes((i as u64) << 16 | k, 8);
            let mut input = seed.clone();
            if k % 4 == 3 {
                input.truncate(usize::from(r[0]) * input.len() / 256);
            } else {
                for j in 0..=usize::from(r[1] % 3) {
                    let at =
                        usize::from(u16::from_le_bytes([r[2 + 2 * j], r[3 + 2 * j]])) % input.len();
                    input[at] ^= r[7] | 1;
                }
            }
            agrees(t, &input, &format!("{path} mutation {k}"));
        }
    }
}

/// Every record of the public seeds.
pub fn hits(t: &Target) -> Vec<Hit> {
    seeds(t).iter().flat_map(|(_, input)| read(t.table, input).hits().to_vec()).collect()
}

/// `op` runs with each of `statuses` (`ORACLE_OK` among them) and, under each flag, on an
/// invalid operand.
pub fn op_reaches(hits: &[Hit], op: u8, statuses: &[i32], flags: u8) {
    let of_op: Vec<&Hit> = hits.iter().filter(|h| h.op == op && h.flags & flags == flags).collect();
    let seen: BTreeSet<i32> = of_op.iter().map(|h| h.status).collect();
    for status in statuses.iter().chain(&[ORACLE_OK]) {
        assert!(seen.contains(status), "op {op} flags {flags:#x}: status {status} (saw {seen:?})");
    }
    for legacy in [0, ORACLE_BLS_FLAG_LEGACY] {
        assert!(
            of_op.iter().any(|h| h.operands.invalid && h.flags & ORACLE_BLS_FLAG_LEGACY == legacy),
            "op {op} flags {flags:#x}: no invalid operand under flag {legacy}"
        );
    }
}

/// `op` runs under the legacy flag, past its argument checks, on a point with no usable
/// element, which it pairs on relic's engine.
// `l1_bls_ies` runs no verification.
#[allow(dead_code)]
pub fn legacy_unusable_reached(hits: &[Hit], op: u8) {
    assert!(
        hits.iter().any(|h| {
            h.op == op
                && h.flags & ORACLE_BLS_FLAG_LEGACY != 0
                && h.operands.unusable
                && !h.operands.invalid
                && h.status != ORACLE_E_ARG
        }),
        "op {op}: no unusable operand under the legacy flag"
    );
}

/// Some op of `ops` runs, under each flag, on a kept element and on an Exotic one.
pub fn ops_take_kept_and_exotic(hits: &[Hit], ops: &[u8]) {
    for legacy in [0, ORACLE_BLS_FLAG_LEGACY] {
        let ours = || {
            hits.iter()
                .filter(|h| ops.contains(&h.op) && h.flags & ORACLE_BLS_FLAG_LEGACY == legacy)
        };
        assert!(ours().any(|h| h.operands.kept), "no kept operand under flag {legacy}");
        assert!(ours().any(|h| h.operands.exotic), "no Exotic operand under flag {legacy}");
    }
}

/// Prints each record of the input `EXPLAIN` names (a `random_scripts_agree` seed or a file)
/// beside Core's: the op, the node's status, Core's status, relic's code and the logged calls
/// that left one (debugging).
pub fn explain(t: &Target) {
    let what = std::env::var("EXPLAIN").expect("EXPLAIN");
    let input = match what.parse::<u64>() {
        Ok(seed) => random_script(t, seed),
        Err(_) => std::fs::read(&what).expect("an input file"),
    };
    let script = read(t.table, &input);
    let records = dash_oracle_sys::Oracle::new(PIN).bls_exec(&script.code).expect("records");
    eprintln!("{} records", records.len());
    for (i, (hit, record)) in script.hits().iter().zip(&records).enumerate() {
        let calls: Vec<String> = record
            .log
            .iter()
            .flatten()
            .filter(|c| c.code != 0)
            .map(|c| format!("{}={}", c.symbol, c.code))
            .collect();
        eprintln!(
            "{i}: op {} flags {:#x} node {} core {} relic {} {:?} {calls:?}",
            hit.op, hit.flags, hit.status, record.status, record.relic_code, hit.operands
        );
    }
}

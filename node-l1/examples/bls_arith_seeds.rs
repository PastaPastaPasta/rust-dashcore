//! Writes `diff_bls_arith`'s public seeds (`node_l1::bls_arith`, BS11a-3 D20), each file one
//! target input named by the first 8 bytes of its SHA-256, to
//! `node-fuzz/public-seeds/diff_bls_arith/`:
//! - every row of base-sdk's Core-judged step scripts (`pkgs/pkc/corpus/bls_oracle/arith.json5`
//!   and `relic.json5`: §8's fixed vectors, the shape table, the scalar classes, aggregation, DH
//!   and `SER_HASH`'s cache), its objects loaded raw and its register operands picked as the
//!   target picks them; `VERIFY` steps are left out (slice 4);
//! - one torsion script per group, cofactor part and generator bit, under each flag.
//!
//! It also writes `bls_engine_model`'s seeds to `node-fuzz/seeds/bls_engine_model/`: per group,
//! the §8 scalar classes on the generator and on a `PROJC` point, both infinity shapes through
//! every op, aggregation and rescaling.
//!
//! `cargo run -p node-l1 --features oracle --example bls_arith_seeds -- <arith.json5>
//! <relic.json5>`, the corpus at the pinned base SDK revision.

use bitcoin_hashes::sha256;
use node_l1::bls_arith::OpKind;
use serde_json::Value;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn read(path: &str) -> Value {
    let text = std::fs::read_to_string(path).expect(path);
    let json: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    serde_json::from_str(&json).expect(path)
}

/// A selector: the op, the legacy flag, the signature group, the alternative form.
fn selector(op: OpKind, legacy: bool, sig: bool, alt: bool) -> u8 {
    op as u8 | u8::from(legacy) << 4 | u8::from(sig) << 5 | u8::from(alt) << 6
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sk,
    Pk,
    Sig,
}

/// A step script as a target input. Registers are tracked as the target fills them: each
/// storing step's object kind, or none where Core left the destination empty.
fn translate(row: &Value) -> Vec<u8> {
    let legacy = row["scheme"] == "legacy";
    let mut regs: Vec<Option<Kind>> = Vec::new();
    // Per step, the register it stored to.
    let mut step_reg: Vec<Option<usize>> = Vec::new();
    let mut out = Vec::new();
    for step in row["steps"].as_array().expect("steps") {
        let op = step["op"].as_str().expect("op");
        let flag = step["flag"].as_str().map_or(legacy, |f| f == "legacy");
        let reg = |field: &str| step_reg[step[field].as_u64().expect(field) as usize].expect(field);
        // The position of register `r` among the live ones of its kind.
        let pick = |r: usize| {
            let kind = regs[r];
            regs[..r].iter().filter(|k| **k == kind).count() as u8
        };
        let status = step["status"].as_i64().expect("status");
        let stored = match op {
            "decode" => {
                let input = unhex(step["input"].as_str().expect("input"));
                let kind = match step["kind"].as_str().expect("kind") {
                    "sk" => Kind::Sk,
                    "pk" => Kind::Pk,
                    _ => Kind::Sig,
                };
                if kind == Kind::Sk {
                    assert_eq!(input.len(), 32);
                    out.extend([selector(OpKind::LoadSk, legacy, false, false), 0]);
                    out.extend(&input);
                } else {
                    assert_eq!(
                        input.len(),
                        if kind == Kind::Pk {
                            48
                        } else {
                            96
                        }
                    );
                    out.extend([selector(OpKind::LoadPoint, legacy, kind == Kind::Sig, false), 0]);
                    out.extend(&input);
                    // The flag byte the raw class does not read.
                    out.push(0);
                }
                (status != -1).then_some(kind)
            }
            "pubkey" => {
                out.extend([selector(OpKind::Pubkey, legacy, false, false), pick(reg("a"))]);
                Some(Kind::Pk)
            }
            "sign" => {
                out.extend([selector(OpKind::Sign, legacy, false, false), pick(reg("a"))]);
                out.extend(unhex(step["msg"].as_str().expect("msg")));
                Some(Kind::Sig)
            }
            "add" | "neg" | "mul" | "is_valid" | "eq" | "impl_eq" | "ser" | "ser_hash" => {
                let a = reg("a");
                let kind = regs[a].expect("an operand");
                let (kind_op, operands) = match op {
                    "add" => (OpKind::Add, 2),
                    "neg" => (OpKind::Neg, 1),
                    "mul" => (OpKind::Mul, 1),
                    "is_valid" => (OpKind::IsValid, 1),
                    "eq" => (OpKind::Eq, 2),
                    "impl_eq" => (OpKind::ImplEq, 2),
                    "ser" => (OpKind::Ser, 1),
                    _ => (OpKind::SerHash, 1),
                };
                let sk = kind == Kind::Sk;
                out.push(selector(kind_op, flag, kind == Kind::Sig || sk, sk));
                out.push(pick(a));
                if operands == 2 {
                    out.push(pick(reg("b")));
                }
                if op == "mul" {
                    out.push(0);
                    out.extend(unhex(step["k"].as_str().expect("k")));
                }
                matches!(op, "add" | "neg" | "mul").then_some(kind)
            }
            "agg_pk" | "agg_sig" => {
                let sig = op == "agg_sig";
                let pair = step["pair"].as_bool().unwrap_or(false);
                let items: Vec<usize> = step["items"]
                    .as_array()
                    .expect("items")
                    .iter()
                    .map(|i| step_reg[i.as_u64().expect("item") as usize].expect("item"))
                    .collect();
                out.push(selector(OpKind::Agg, flag, sig, pair));
                out.push(items.len() as u8);
                out.extend(items.iter().map(|&i| pick(i)));
                (status != -4).then_some(if sig {
                    Kind::Sig
                } else {
                    Kind::Pk
                })
            }
            "dh" => {
                let recv = step.get("recv").filter(|r| !r.is_null()).map(|_| reg("recv"));
                out.push(selector(OpKind::Dh, flag, false, recv.is_some()));
                out.extend([pick(reg("sk")), pick(reg("pk")), recv.map_or(0, pick)]);
                Some(Kind::Pk)
            }
            "verify" => {
                step_reg.push(None);
                continue;
            }
            other => panic!("op {other}"),
        };
        let stores = !matches!(op, "is_valid" | "eq" | "impl_eq" | "ser" | "ser_hash");
        if stores {
            step_reg.push(Some(regs.len()));
            regs.push(stored);
        } else {
            step_reg.push(None);
        }
    }
    out
}

/// A torsion point of `part` in the group, a subgroup point, and the ops over them.
fn torsion(sig: bool, part: u8, legacy: bool) -> Vec<u8> {
    let len = if sig {
        96
    } else {
        48
    };
    let mut out = vec![selector(OpKind::LoadTorsion, legacy, sig, false), part, 0x80];
    out.extend((0..len).map(|i| (i as u8).wrapping_mul(29).wrapping_add(part)));
    // A subgroup point.
    out.extend([selector(OpKind::LoadPoint, legacy, sig, false), 1, 0x80]);
    out.extend([7u8; 96][..len].iter());
    let s = |op| selector(op, legacy, sig, false);
    let b = |op| selector(op, !legacy, sig, false);
    out.extend([s(OpKind::Add), 0, 1, s(OpKind::Neg), 0, s(OpKind::Add), 2, 3]);
    // λ, then r − 1 (the scalar's first byte is its signed offset).
    out.extend([s(OpKind::Mul), 0, 5]);
    out.extend([0u8; 32]);
    out.extend([s(OpKind::Mul), 4, 2, 0xff]);
    out.extend([0u8; 31]);
    out.extend([s(OpKind::Eq), 3, 4, s(OpKind::ImplEq), 0, 5, s(OpKind::IsValid), 4]);
    out.extend([b(OpKind::SerHash), 4, s(OpKind::SerHash), 4, b(OpKind::Ser), 2]);
    out.extend([selector(OpKind::Agg, legacy, sig, false), 3, 0, 2, 4]);
    out.extend([selector(OpKind::Agg, legacy, sig, true), 0, 1, 3]);
    out
}

/// `bls_engine_model`'s load (op 0) of a scalar class with a first byte, or of an infinity
/// (class 8, `BASIC`; class 9, `PROJC` from register `reg`), rebuilt with `lambda`.
fn model_load(g: u8, class: u8, first: u8, reg: u8, lambda: u64) -> Vec<u8> {
    let mut out = vec![
        g,
        class,
        if class == 9 {
            reg
        } else {
            first
        },
    ];
    out.extend([0x5a; 31]);
    out.extend(lambda.to_le_bytes());
    out
}

/// λ values for the rebuilds: one, zero (read as one), small, and the largest.
const LAMBDAS: [u64; 4] = [1, 0, 3, u64::MAX];

fn model_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for g in [0u8, 8] {
        // The scalar classes on the generator, each offset by −1, 0 and 1, then each result
        // compared with the generator and negated.
        for first in [0xffu8, 0, 1] {
            let mut s = Vec::new();
            for class in 0..8 {
                s.extend(model_load(g, class, first, 0, LAMBDAS[usize::from(class) % 4]));
            }
            for r in 1..9u8 {
                s.extend([g | 6, 0, r, g | 2, r]);
            }
            seeds.push(s);
        }
        // The scalar classes on a PROJC point (G + G) and on the PROJC infinity.
        let mut s = vec![g | 1, 0, 0];
        s.extend(model_load(g, 9, 0, 0, 2));
        for class in 0..8 {
            for reg in [1u8, 2] {
                s.extend([g | 3, reg, class, 0]);
                s.extend([0x33; 31]);
            }
        }
        seeds.push(s);
        // The infinities: BASIC (1) and PROJC (2) through every op.
        let mut s = model_load(g, 8, 0, 0, 1);
        s.extend(model_load(g, 9, 0, 0, 5));
        s.extend([g | 1, 1, 2, g | 1, 2, 0, g | 1, 0, 2, g | 1, 2, 2, g | 2, 2, g | 2, 1]);
        s.extend([g | 6, 1, 2, g | 6, 2, 0, g | 6, 2, 3, g | 6, 1, 1]);
        s.extend([g | 4, 3, 1, 2, 0, g | 4, 0, g | 4, 2, 2, 1]);
        for lambda in LAMBDAS {
            s.push(g | 5);
            s.push(2);
            s.extend(lambda.to_le_bytes());
            s.extend([g | 6, 2, 0]);
        }
        seeds.push(s);
        // Chains in the engine: sums and multiples of sums, rescaled between steps.
        let mut s = model_load(g, 1, 0, 0, 7);
        for step in 0..6u8 {
            s.extend([g | 1, step, step + 1, g | 3, step + 2, 5, step]);
            s.extend([0x11; 31]);
            s.push(g | 5);
            s.push(step);
            s.extend(u64::from(step).wrapping_mul(0x9e37_79b9).to_le_bytes());
        }
        seeds.push(s);
    }
    seeds
}

fn write_seeds(dir: &str, inputs: &[Vec<u8>]) -> std::io::Result<usize> {
    // A corpus change can drop a row: start empty.
    if std::path::Path::new(dir).exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let mut written = std::collections::BTreeSet::new();
    for input in inputs {
        let digest = sha256::Hash::hash(input).to_byte_array();
        let file: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{dir}/{file}"), input)?;
        written.insert(file);
    }
    Ok(written.len())
}

fn main() -> std::io::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(paths.len(), 2, "the paths of arith.json5 and relic.json5");
    let mut inputs = Vec::new();
    for path in &paths {
        for row in read(path)["rows"].as_array().expect("rows") {
            inputs.push(translate(row));
        }
    }
    // The torsion parts: 1 and h/ℓ^e per small prime ℓ, and for h₂ its 448-bit prime's part.
    for (sig, parts) in [(false, 6u8), (true, 7)] {
        for part in 0..parts {
            for with_gen in [0, 0x80] {
                for legacy in [false, true] {
                    inputs.push(torsion(sig, part | with_gen, legacy));
                }
            }
        }
    }
    let root = format!("{}/../node-fuzz", env!("CARGO_MANIFEST_DIR"));
    let n = write_seeds(&format!("{root}/public-seeds/diff_bls_arith"), &inputs)?;
    eprintln!("diff_bls_arith: {n} seeds");
    let n = write_seeds(&format!("{root}/seeds/bls_engine_model"), &model_seeds())?;
    eprintln!("bls_engine_model: {n} seeds");
    Ok(())
}

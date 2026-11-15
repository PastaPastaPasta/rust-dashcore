//! `bls_engine_model` (BS11a-3 D18): `dash_pkc`'s two representations of Core's BLS elements
//! against each other. A `Group` element is blst's point with relic's coord tag; an engine
//! element is relic's coordinates, which `diff_bls_arith` checks against Core. Rebuilt into the
//! engine with any rescaling λ (Lemma T′), a `Group` element must behave as the element Core
//! holds, so every op is run on both and their results compared:
//! - the serialization under both schemes;
//! - `is_valid`;
//! - relic's shape: the `Group` value's tag and infinity against the engine's tag and which of
//!   its coordinates are zero;
//! - that `Group` operands give a `Group` result and engine operands an engine one;
//! - `core_eq` across the three pairings (`Group`–`Group`, `Group`–engine, engine–engine).
//!
//! The input is an op script over two register files per group, the `Group` values and their
//! engine twins, which start with the generator. Each op is a selector byte (bits 0-2: the op;
//! bit 3: the group, G1 or G2) and its operands:
//! - 0, load: a class byte, then 32 bytes: the generator times a [`scalar`] (class 0-7), the
//!   `BASIC` infinity (8), or P + (−P) for a register P (9, its index in the next byte), the
//!   `PROJC` infinity; its twin is rebuilt with λ from the next 8 bytes (zero reads as one);
//! - 1, add; 2, neg: register operands, one byte each, modulo the file's length;
//! - 3, mul: a register, a class byte and 32 bytes ([`scalar`]);
//! - 4, aggregate: a count (modulo 7) and the registers (`AggregateInsecure`'s span form);
//! - 5, rescale: a register's twin rebuilt from its `Group` value with λ from 8 bytes;
//! - 6, eq: two registers compared across the pairings;
//! - 7: as 1.
//!
//! At most [`MAX_OPS`] ops run, and the files stop growing at [`MAX_REGS`].

use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::{BlsPkElem, BlsPkValue, BlsSigElem, BlsSigValue};
use node_l1::Divergence;
use node_l1::bls_arith::{G1_GEN, G2_GEN, scalar};

const MAX_OPS: usize = 64;
const MAX_REGS: usize = 32;

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(diverged(field))
    }
}

/// `BlsPkElem` and `BlsSigElem`'s shared surface.
trait Elem: Clone + Default {
    fn generator() -> Self;
    fn add(&self, other: &Self) -> Self;
    fn neg(&self) -> Self;
    fn mul(&self, k: &[u8; 32]) -> Self;
    fn core_eq(&self, other: &Self) -> bool;
    fn aggregate(items: &[&Self]) -> Option<Self>;
    fn checked(&self, twin: &Self) -> Result<(), Divergence>;
    fn to_relic(&self, lambda: u64) -> Self;
}

macro_rules! impl_elem {
    ($elem:ty, $value:ty, $gen:expr) => {
        impl Elem for $elem {
            fn generator() -> Self {
                <$value>::from_canonical(&$gen, BlsSchemeKind::Basic)
                    .map(|v| <$elem>::from_impl(&v))
                    .unwrap_or_default()
            }

            fn add(&self, other: &Self) -> Self {
                <$elem>::add(self, other)
            }

            fn neg(&self) -> Self {
                <$elem>::neg(self)
            }

            fn mul(&self, k: &[u8; 32]) -> Self {
                <$elem>::mul(self, k)
            }

            fn core_eq(&self, other: &Self) -> bool {
                <$elem>::core_eq(self, other)
            }

            /// Through the wrappers, as Core aggregates them.
            fn aggregate(items: &[&Self]) -> Option<Self> {
                let values: Vec<$value> = items.iter().map(|e| (*e).clone().into()).collect();
                <$elem>::aggregate_insecure(values.iter())
            }

            /// `self`, a `Group` value, against its engine `twin`.
            fn checked(&self, twin: &Self) -> Result<(), Divergence> {
                ensure(!self.__is_relic(), "group result in the engine")?;
                ensure(twin.__is_relic(), "engine result out of the engine")?;
                for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
                    ensure(self.to_bytes(scheme) == twin.to_bytes(scheme), "serialization")?;
                }
                ensure(self.is_valid() == twin.is_valid(), "is_valid")?;
                ensure(self.__shape() == twin.__shape(), "shape")
            }

            fn to_relic(&self, lambda: u64) -> Self {
                self.__to_relic(lambda)
            }
        }
    };
}

impl_elem!(BlsPkElem, BlsPkValue, G1_GEN);
impl_elem!(BlsSigElem, BlsSigValue, G2_GEN);

fn byte(data: &[u8], at: usize) -> u8 {
    data.get(at).copied().unwrap_or(0)
}

fn lambda(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(std::array::from_fn(|i| byte(data, at + i)))
}

fn tail(data: &[u8], at: usize) -> &[u8] {
    data.get(at..).unwrap_or_default()
}

/// A group's two register files: the `Group` values and their engine twins.
struct Files<E> {
    group: Vec<E>,
    engine: Vec<E>,
}

impl<E: Elem> Files<E> {
    fn new() -> Self {
        let g = E::generator();
        let e = g.to_relic(1);
        Self {
            group: vec![g],
            engine: vec![e],
        }
    }

    fn reg(&self, pick: u8) -> usize {
        usize::from(pick) % self.group.len()
    }

    /// Stores a result and its twin, once they agree.
    fn push(&mut self, g: E, e: E) -> Result<(), Divergence> {
        g.checked(&e)?;
        if self.group.len() < MAX_REGS {
            self.group.push(g);
            self.engine.push(e);
        }
        Ok(())
    }

    /// One op (module documentation), its operands from `rest`. Returns the bytes read.
    fn op(&mut self, op: u8, rest: &[u8]) -> Result<usize, Divergence> {
        match op {
            0 => {
                let class = byte(rest, 0);
                let g = match class % 10 {
                    8 => E::default(),
                    9 => {
                        let p = &self.group[self.reg(byte(rest, 1))];
                        p.add(&p.neg())
                    }
                    c => self.group[0].mul(&scalar(c, tail(rest, 1))),
                };
                let e = g.to_relic(lambda(rest, 33));
                self.push(g, e)?;
                Ok(41)
            }
            1 | 7 => {
                let (a, b) = (self.reg(byte(rest, 0)), self.reg(byte(rest, 1)));
                let g = self.group[a].add(&self.group[b]);
                let e = self.engine[a].add(&self.engine[b]);
                self.push(g, e)?;
                Ok(2)
            }
            2 => {
                let a = self.reg(byte(rest, 0));
                let (g, e) = (self.group[a].neg(), self.engine[a].neg());
                self.push(g, e)?;
                Ok(1)
            }
            3 => {
                let a = self.reg(byte(rest, 0));
                let k = scalar(byte(rest, 1), tail(rest, 2));
                let (g, e) = (self.group[a].mul(&k), self.engine[a].mul(&k));
                self.push(g, e)?;
                Ok(34)
            }
            4 => {
                let n = usize::from(byte(rest, 0) % 7);
                let picks: Vec<usize> = (0..n).map(|i| self.reg(byte(rest, 1 + i))).collect();
                let g = E::aggregate(&picks.iter().map(|&i| &self.group[i]).collect::<Vec<_>>());
                let e = E::aggregate(&picks.iter().map(|&i| &self.engine[i]).collect::<Vec<_>>());
                match (g, e) {
                    (Some(g), Some(e)) => self.push(g, e)?,
                    (None, None) => {}
                    _ => return Err(diverged("aggregate")),
                }
                Ok(1 + n)
            }
            5 => {
                let a = self.reg(byte(rest, 0));
                self.engine[a] = self.group[a].to_relic(lambda(rest, 1));
                self.group[a].checked(&self.engine[a])?;
                Ok(9)
            }
            _ => {
                let (a, b) = (self.reg(byte(rest, 0)), self.reg(byte(rest, 1)));
                let (ga, gb, ea, eb) =
                    (&self.group[a], &self.group[b], &self.engine[a], &self.engine[b]);
                let eq = ga.core_eq(gb);
                ensure(ga.core_eq(eb) == eq && ea.core_eq(gb) == eq, "eq group-engine")?;
                ensure(ea.core_eq(eb) == eq, "eq engine-engine")?;
                Ok(2)
            }
        }
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let mut g1 = Files::<BlsPkElem>::new();
    let mut g2 = Files::<BlsSigElem>::new();
    let mut at = 0;
    for _ in 0..MAX_OPS {
        let Some(&selector) = data.get(at) else {
            break;
        };
        let rest = tail(data, at + 1);
        let used = if selector & 8 == 0 {
            g1.op(selector & 7, rest)?
        } else {
            g2.op(selector & 7, rest)?
        };
        at += 1 + used;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeds() -> Vec<(String, Vec<u8>)> {
        let dir = format!("{}/seeds/bls_engine_model", env!("CARGO_MANIFEST_DIR"));
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap_or_default(),
                )
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn seeds_hold() {
        let seeds = seeds();
        assert!(seeds.len() >= 12, "{} seeds", seeds.len());
        for (name, data) in seeds {
            assert_eq!(check(&data), Ok(()), "{name}");
        }
    }

    /// Byte flips of the seeds (a fixed xorshift), so a plain `cargo test` covers the
    /// neighbourhood the fuzzer starts from.
    #[test]
    fn mutated_seeds_hold() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for (name, seed) in seeds() {
            for _ in 0..8 {
                let mut data = seed.clone();
                for _ in 0..4 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    if !data.is_empty() {
                        let at = (state as usize) % data.len();
                        data[at] ^= (state >> 32) as u8;
                    }
                }
                assert_eq!(check(&data), Ok(()), "{name} mutated");
            }
        }
    }

    /// The infinity shapes and their twins: `BASIC` from the default, `PROJC` from P + (−P).
    #[test]
    fn infinities_keep_their_shapes() {
        let mut files = Files::<BlsSigElem>::new();
        let mut load = vec![8u8];
        load.extend([0u8; 40]);
        assert_eq!(files.op(0, &load), Ok(41));
        let mut sum = vec![9u8, 0];
        sum.extend([0u8; 39]);
        assert_eq!(files.op(0, &sum), Ok(41));
        assert!(!files.group[1].__shape().projc);
        assert!(files.group[2].__shape().projc);
        // Both infinities equal the generator's negation's sum; the eq op checks all pairings.
        assert_eq!(files.op(6, &[1, 2]), Ok(2));
    }
}

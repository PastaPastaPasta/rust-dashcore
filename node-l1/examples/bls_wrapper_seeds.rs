//! Writes the public seeds of `diff_bls_aggregate`, `diff_bls_threshold`, `diff_bls_ies`
//! (`node_l1::bls_wrapper`, BS11b D25) and `diff_bls_verify` (BS11a-4 D10), each file one target
//! input named by the first 8 bytes of its SHA-256, to `node-fuzz/public-seeds/diff_bls_<name>/`.
//! Each script runs under each flag and puts every new op on Group, invalid, kept-element and
//! Exotic operands, on both its accepting and its refusing paths:
//! - aggregate: the insecure aggregates (span and pair), `SubInsecure`, the secure aggregate
//!   and its verification over 1 to 8 signers, duplicate keys and unequal lists, and
//!   `SER_HASH`'s cache;
//! - threshold: shares and recovery for thresholds 2 to 5 over each id class, the recovered
//!   signature verified, and each refusal (one share, duplicate ids, ids equal modulo r, a
//!   zero id, unequal lists, an invalid share), with and without a receiver;
//! - IES: DH, encryption to valid, invalid and Exotic recipients over each plaintext class,
//!   and decryption of each container and blob, as it is and mutated;
//! - verify: the three verifications over 1 to 4 signers, accepting and refusing, distinct
//!   and repeated hashes, unequal and empty lists, the aggregates they verify, and invalid,
//!   kept-element and Exotic operands.
//!
//! `cargo run -p node-l1 --features oracle --example bls_wrapper_seeds`.

use bitcoin_hashes::sha256;
use dash_oracle_sys::BlsKind;
use node_l1::bls_arith::OpKind;
use node_l1::bls_wrapper::WrapperOp::{
    AggGroup, AggSecure, AggSk, Base, Dh, IesDecrypt, IesDecryptBlob, IesEncrypt, Keep, PkShare,
    Recover, SkShare, SubSig, Verify, VerifyAgg, VerifySec,
};
use node_l1::bls_wrapper::{
    AGGREGATE_OPS, IES_OPS, THRESHOLD_OPS, VERIFY_OPS, WrapperOp, read, selector,
};

/// A script under construction for a target (its op table), its ops run under `legacy`;
/// `out` is the input so far.
struct B {
    table: &'static [WrapperOp],
    legacy: bool,
    out: Vec<u8>,
}

/// A hash operand: 32 bytes as they are.
fn h(x: u8) -> [u8; 33] {
    let mut out = [x; 33];
    out[0] = 0;
    out
}

/// A hash operand: the `i`th signed hash.
fn signed(i: u8) -> [u8; 33] {
    let mut out = [0; 33];
    out[0] = 2 * i + 1;
    out
}

/// An id operand of `class` with `k` (`Script::id`).
fn id(class: u8, k: u8) -> [u8; 33] {
    let mut out = [0x3c; 33];
    out[0] = class;
    out[1] = k;
    out
}

/// A random-class id from `x`.
fn rid(x: u8) -> [u8; 33] {
    let mut out: [u8; 33] = std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(x));
    out[0] = 5;
    out
}

/// A plaintext spec (`bls_wrapper::plaintext`): length class, size, content class, parameter.
type Spec = [u8; 4];
const EMPTY: Spec = [0, 0, 0, 0];
const ZEROS32: Spec = [1, 2, 2, 0];
const R48: Spec = [1, 3, 1, 0];
const R_PLUS_1: Spec = [1, 2, 1, 1];
const PATTERN96: Spec = [1, 6, 0, 7];
/// 17 bytes: CBC refuses it.
const ODD17: Spec = [2, 1, 0, 3];
/// 95 bytes.
const ODD95: Spec = [2, 113, 0, 9];

/// A secret key's 32 bytes as the plaintext's head (`pick` among the secret keys).
fn sk_spec(pick: u8, len16: u8) -> Spec {
    [1, len16, 3, pick]
}

impl B {
    fn new(table: &'static [WrapperOp], legacy: bool) -> Self {
        Self {
            table,
            legacy,
            out: Vec::new(),
        }
    }

    fn kinds(&self) -> Vec<Option<BlsKind>> {
        read(self.table, &self.out).register_kinds()
    }

    /// Register `r`'s pick: its position among the live registers of its kind.
    fn p(&self, r: usize) -> u8 {
        let kinds = self.kinds();
        let kind = kinds[r];
        assert!(kind.is_some(), "register {r} is empty");
        kinds[..r].iter().filter(|k| **k == kind).count() as u8
    }

    fn ps(&self, rs: &[usize]) -> Vec<u8> {
        let mut out = vec![rs.len() as u8];
        out.extend(rs.iter().map(|&r| self.p(r)));
        out
    }

    /// Appends an op and its operands, which must run; returns the last register.
    fn op(&mut self, op: WrapperOp, legacy: bool, sig: bool, alt: bool, operands: &[u8]) -> usize {
        let before = read(self.table, &self.out).records();
        self.out.push(selector(self.table, op, legacy, sig, alt).expect("an op of the table"));
        self.out.extend(operands);
        let script = read(self.table, &self.out);
        assert!(script.records() > before, "{op:?} did not run");
        script.register_kinds().len() - 1
    }

    /// Register `r` when Core stored an object there.
    fn stored(&self, r: usize) -> Option<usize> {
        self.kinds()[r].is_some().then_some(r)
    }

    fn base(&mut self, kind: OpKind, sig: bool, alt: bool, operands: &[u8]) -> usize {
        self.op(Base(kind), self.legacy, sig, alt, operands)
    }

    fn sk(&mut self, bytes: [u8; 32]) -> usize {
        self.base(OpKind::LoadSk, false, false, &[&[0][..], &bytes].concat())
    }

    /// r plus a signed byte, when it decodes.
    fn sk_near_r(&mut self, delta: i8) -> Option<usize> {
        let r =
            self.base(OpKind::LoadSk, false, false, &[&[1, delta as u8][..], &[0; 31]].concat());
        self.stored(r)
    }

    /// A raw point: the bytes then the unused flag byte.
    fn point(&mut self, sig: bool, bytes: &[u8]) -> usize {
        self.base(OpKind::LoadPoint, sig, false, &[&[0][..], bytes, &[0]].concat())
    }

    /// The invalid object of each kind: zeros.
    fn zero_sk(&mut self) -> usize {
        self.sk([0; 32])
    }

    fn zero_pk(&mut self) -> usize {
        self.point(false, &[0; 48])
    }

    fn zero_sig(&mut self) -> usize {
        self.point(true, &[0; 96])
    }

    /// An ℓ-power torsion point (cofactor part `part`, bit 7 adds the generator), decoded
    /// under the legacy flag, an Exotic value for either flag's ops; the invalid point when it
    /// does not decode.
    fn torsion(&mut self, sig: bool, part: u8) -> usize {
        let len = if sig {
            96
        } else {
            48
        };
        let mut operands = vec![part, 0x80];
        operands.extend((0..len).map(|i| (i as u8).wrapping_mul(29).wrapping_add(part)));
        let r = self.op(Base(OpKind::LoadTorsion), true, sig, false, &operands);
        match self.stored(r) {
            Some(r) => r,
            None if sig => self.zero_sig(),
            None => self.zero_pk(),
        }
    }

    fn pubkey(&mut self, sk: usize) -> usize {
        let p = self.p(sk);
        self.base(OpKind::Pubkey, false, false, &[p])
    }

    fn sign(&mut self, sk: usize, x: u8) -> usize {
        let p = self.p(sk);
        self.base(OpKind::Sign, false, false, &[&[p][..], &[x; 32]].concat())
    }

    /// The group and alternative bits of an op on register `r` (`SER`'s secret key).
    fn bits(&self, r: usize) -> (bool, bool) {
        match self.kinds()[r] {
            Some(BlsKind::SecretKey) => (true, true),
            Some(BlsKind::Signature) => (true, false),
            _ => (false, false),
        }
    }

    fn ser(&mut self, r: usize, hash: bool, legacy: bool) {
        let (sig, alt) = self.bits(r);
        let p = self.p(r);
        let kind = if hash {
            OpKind::SerHash
        } else {
            OpKind::Ser
        };
        self.op(Base(kind), legacy, sig, alt, &[p]);
    }

    /// `EQ` or `IMPL_EQ` of two points; for secret keys, which the targets compare by their
    /// serializations, a `SER` of each.
    fn eq(&mut self, a: usize, b: usize, wrapper: bool) {
        let (sig, sk) = self.bits(a);
        if sk {
            self.ser(a, false, self.legacy);
            self.ser(b, false, self.legacy);
            return;
        }
        let kind = if wrapper {
            OpKind::Eq
        } else {
            OpKind::ImplEq
        };
        let ops = [self.p(a), self.p(b)];
        self.base(kind, sig, false, &ops);
    }

    fn is_valid(&mut self, a: usize) {
        let (sig, _) = self.bits(a);
        let p = self.p(a);
        self.base(OpKind::IsValid, sig, false, &[p]);
    }

    fn add(&mut self, a: usize, b: usize) -> usize {
        let (sig, _) = self.bits(a);
        let ops = [self.p(a), self.p(b)];
        self.base(OpKind::Add, sig, false, &ops)
    }

    fn mul(&mut self, a: usize, k: [u8; 32]) -> usize {
        let (sig, _) = self.bits(a);
        let p = self.p(a);
        self.base(OpKind::Mul, sig, false, &[&[p, 0][..], &k].concat())
    }

    fn agg_sk(&mut self, rs: &[usize]) -> usize {
        let ops = self.ps(rs);
        self.op(AggSk, self.legacy, false, false, &ops)
    }

    fn agg(&mut self, sig: bool, rs: &[usize]) -> usize {
        let ops = self.ps(rs);
        self.op(AggGroup, self.legacy, sig, false, &ops)
    }

    fn pair(&mut self, sig: bool, a: usize, b: usize) -> usize {
        let ops = [0, self.p(a), self.p(b)];
        self.op(AggGroup, self.legacy, sig, true, &ops)
    }

    fn sub(&mut self, a: usize, b: usize) -> usize {
        let ops = [self.p(a), self.p(b)];
        self.op(SubSig, self.legacy, false, false, &ops)
    }

    /// `AGG_SECURE`; `pks` may differ in length from `sigs`.
    fn secure(&mut self, sigs: &[usize], pks: &[usize], hash: [u8; 33]) -> usize {
        let unequal = sigs.len() != pks.len();
        let mut ops = self.ps(sigs);
        ops.extend(self.ps(pks));
        ops.extend(hash);
        self.op(AggSecure, self.legacy, unequal, false, &ops)
    }

    fn verify_sec(&mut self, s: usize, pks: &[usize], hash: [u8; 33]) {
        let mut ops = vec![self.p(s)];
        ops.extend(self.ps(pks));
        ops.extend(hash);
        self.op(VerifySec, self.legacy, false, false, &ops);
    }

    fn keep(&mut self, r: usize) -> usize {
        let (sig, alt) = self.bits(r);
        let p = self.p(r);
        self.op(Keep, self.legacy, sig, alt, &[p])
    }

    fn share(&mut self, sk: bool, recv: Option<usize>, coeffs: &[usize], id: [u8; 33]) -> usize {
        let mut ops = vec![recv.map_or(0, |r| self.p(r))];
        ops.extend(self.ps(coeffs));
        ops.extend(id);
        let op = if sk {
            SkShare
        } else {
            PkShare
        };
        self.op(op, self.legacy, false, recv.is_some(), &ops)
    }

    /// `RECOVER_SIG`; `ids` may differ in length from `sigs`.
    fn recover(&mut self, recv: Option<usize>, sigs: &[usize], ids: &[[u8; 33]]) -> usize {
        let unequal = sigs.len() != ids.len();
        let mut ops = vec![recv.map_or(0, |r| self.p(r))];
        ops.extend(self.ps(sigs));
        ops.push(ids.len() as u8);
        ops.extend(ids.iter().flatten());
        self.op(Recover, self.legacy, unequal, recv.is_some(), &ops)
    }

    fn verify(&mut self, s: usize, pk: usize, hash: [u8; 33]) {
        let ops = [&[self.p(s), self.p(pk)][..], &hash].concat();
        self.op(Verify, self.legacy, false, false, &ops);
    }

    /// `VERIFY_AGG`; `hashes` may differ in length from `pks`.
    fn verify_agg(&mut self, s: usize, pks: &[usize], hashes: &[[u8; 33]]) {
        let unequal = pks.len() != hashes.len();
        let mut ops = vec![self.p(s)];
        ops.extend(self.ps(pks));
        ops.push(hashes.len() as u8);
        ops.extend(hashes.iter().flatten());
        self.op(VerifyAgg, self.legacy, unequal, false, &ops);
    }

    fn dh(&mut self, sk: usize, pk: usize, recv: Option<usize>) -> usize {
        let ops = [self.p(sk), self.p(pk), recv.map_or(0, |r| self.p(r))];
        // The register DH stored, before the SER after it.
        self.op(Dh, self.legacy, false, recv.is_some(), &ops)
    }

    /// `IES_ENCRYPT`; `specs` may differ in length from `pks`.
    fn encrypt(&mut self, eph: usize, seed: u8, pks: &[usize], specs: &[Spec]) {
        let unequal = pks.len() != specs.len();
        let mut ops = vec![self.p(eph)];
        ops.extend([seed; 32]);
        ops.extend(self.ps(pks));
        ops.push(specs.len() as u8);
        ops.extend(specs.iter().flatten());
        self.op(IesEncrypt, self.legacy, unequal, false, &ops);
    }

    /// `IES_DECRYPT` (or `_BLOB`) of container `c`, blob `j`, with an index class, a mutation
    /// class and its two parameters.
    fn decrypt(&mut self, blob: bool, sk: usize, c: u8, j: u8, idx: u8, mutation: [u8; 3]) {
        let ops = [idx, self.p(sk), c, j, mutation[0], mutation[1], mutation[2]];
        let op = if blob {
            IesDecryptBlob
        } else {
            IesDecrypt
        };
        self.op(op, self.legacy, false, false, &ops);
    }
}

/// Distinct valid secret keys.
fn key(i: u8) -> [u8; 32] {
    let mut k: [u8; 32] =
        std::array::from_fn(|j| (j as u8).wrapping_mul(53).wrapping_add(i.wrapping_mul(17)));
    k[0] = 0x20 + i % 0x40;
    k
}

/// `n` keys, their public keys and their signatures of hash `x`.
fn signers(b: &mut B, n: u8, x: u8) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let sks: Vec<usize> = (0..n).map(|i| b.sk(key(i + 1))).collect();
    let pks: Vec<usize> = sks.iter().map(|&k| b.pubkey(k)).collect();
    let sigs: Vec<usize> = sks.iter().map(|&k| b.sign(k, x)).collect();
    (sks, pks, sigs)
}

fn aggregate_seeds(legacy: bool) -> Vec<Vec<u8>> {
    let new = || B::new(&AGGREGATE_OPS, legacy);
    let mut seeds = Vec::new();
    // Group operands: every aggregate over 1 to 8 signers, the secure one verified.
    for n in [1u8, 2, 4, 8] {
        let mut b = new();
        let (sks, pks, sigs) = signers(&mut b, n, 0xab);
        let n = usize::from(n);
        b.agg_sk(&sks);
        b.agg_sk(&[]);
        b.agg_sk(&[sks[0], sks[0]]);
        let apk = b.agg(false, &pks);
        b.agg(false, &[]);
        let asig = b.agg(true, &sigs);
        b.agg(true, &[]);
        if n >= 2 {
            let pp = b.pair(false, pks[0], pks[1]);
            let sum = b.add(pks[0], pks[1]);
            b.eq(pp, sum, false);
            let ps = b.pair(true, sigs[0], sigs[1]);
            b.eq(pp, apk, true);
            let d = b.sub(asig, sigs[0]);
            b.eq(d, ps, false);
            b.sub(sigs[1], sigs[1]);
        }
        let sec = b.secure(&sigs, &pks, h(0x11));
        b.verify_sec(sec, &pks, signed(0));
        b.verify_sec(sec, &pks, h(0x12));
        b.verify_sec(asig, &pks, signed(0));
        b.verify_sec(sec, &[], signed(0));
        b.secure(&sigs, &pks[..n - 1], h(0x11));
        b.secure(&[], &[], h(0));
        b.ser(sec, true, legacy);
        b.ser(sec, true, !legacy);
        b.ser(sec, false, !legacy);
        b.ser(apk, true, !legacy);
        b.ser(sks[0], true, legacy);
        b.is_valid(sec);
        seeds.push(b.out);
    }
    // Duplicate keys (sorted stably below 17 signers) and a key twice with one signature.
    {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 3, 0x21);
        let s1 = b.secure(&[sigs[0], sigs[1], sigs[2]], &[pks[1], pks[0], pks[1]], h(0));
        let s2 = b.secure(&[sigs[2], sigs[1], sigs[0]], &[pks[1], pks[0], pks[1]], h(0));
        b.eq(s1, s2, true);
        b.secure(&[sigs[0], sigs[0]], &[pks[0], pks[0]], h(0));
        let all = [sigs[0], sigs[1], sigs[2], sigs[0], sigs[1], sigs[2], sigs[0], sigs[1]];
        let keys = [pks[0], pks[1], pks[2], pks[2], pks[1], pks[0], pks[0], pks[1]];
        let s = b.secure(&all, &keys, h(0));
        b.verify_sec(s, &keys, signed(0));
        seeds.push(b.out);
    }
    // Invalid operands: Core sums their default elements, refuses the pair forms and
    // SubInsecure, and the secure aggregate and its verification take them unchecked.
    {
        let mut b = new();
        let (sks, pks, sigs) = signers(&mut b, 2, 0x31);
        let (zk, zp, zs) = (b.zero_sk(), b.zero_pk(), b.zero_sig());
        b.agg_sk(&[sks[0], zk]);
        b.agg_sk(&[zk]);
        b.agg(false, &[pks[0], zp]);
        b.agg(false, &[zp]);
        b.agg(true, &[zs, sigs[1]]);
        b.pair(false, pks[0], zp);
        b.pair(false, zp, pks[0]);
        b.pair(true, zs, sigs[0]);
        b.sub(sigs[0], zs);
        b.sub(zs, sigs[0]);
        let s = b.secure(&[sigs[0], zs], &[pks[0], zp], h(0));
        b.verify_sec(s, &[pks[0], zp], signed(0));
        b.verify_sec(sigs[0], &[pks[0]], signed(0));
        b.verify_sec(zs, &[pks[0]], signed(0));
        b.secure(&[zs], &[zp], h(0));
        b.ser(zp, true, legacy);
        b.ser(zk, true, legacy);
        seeds.push(b.out);
    }
    // Kept elements: a failed in-place op keeps its receiver's element, which the ungated
    // aggregates sum and the gated ones refuse.
    {
        let mut b = new();
        let (sks, pks, sigs) = signers(&mut b, 3, 0x41);
        let kk = b.keep(sks[0]);
        let kp = b.keep(pks[0]);
        let ks = b.keep(sigs[0]);
        b.is_valid(kp);
        b.eq(kp, pks[0], false);
        b.eq(kp, pks[0], true);
        b.agg_sk(&[kk, sks[1]]);
        b.agg(false, &[kp, pks[1]]);
        b.agg(true, &[ks, sigs[1]]);
        b.pair(false, kp, pks[1]);
        b.pair(true, sigs[1], ks);
        b.sub(ks, sigs[1]);
        let s = b.secure(&[ks, sigs[1], sigs[2]], &[kp, pks[1], pks[2]], h(0));
        b.verify_sec(s, &[kp, pks[1], pks[2]], signed(0));
        let good = b.secure(&[sigs[0], sigs[1], sigs[2]], &[pks[0], pks[1], pks[2]], h(0));
        b.verify_sec(good, &[kp, pks[1], pks[2]], signed(0));
        b.ser(ks, true, legacy);
        b.ser(kk, true, legacy);
        // A kept element kept again.
        let k2 = b.keep(kp);
        b.agg(false, &[k2]);
        seeds.push(b.out);
    }
    // Exotic points: torsion keys and signatures through every aggregate.
    for part in [0u8, 1, 3, 0x82, 0x85] {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 2, 0x51);
        let tp = b.torsion(false, part);
        let ts = b.torsion(true, part);
        b.agg(false, &[tp, pks[0]]);
        b.agg(true, &[ts, sigs[0], sigs[1]]);
        b.pair(false, tp, pks[1]);
        b.pair(true, sigs[1], ts);
        b.sub(ts, sigs[0]);
        b.sub(sigs[0], ts);
        let s = b.secure(&[sigs[0], ts, sigs[1]], &[pks[0], tp, pks[1]], h(0));
        b.secure(&[ts, sigs[1]], &[pks[0], pks[1]], h(0));
        b.verify_sec(s, &[pks[0], tp, pks[1]], signed(0));
        b.verify_sec(sigs[0], &[pks[0], tp], signed(0));
        b.verify_sec(ts, &[pks[0]], signed(0));
        let m = b.mul(tp, [0x33; 32]);
        b.agg(false, &[m, tp]);
        b.ser(tp, true, legacy);
        b.ser(tp, true, !legacy);
        b.keep(tp);
        seeds.push(b.out);
    }
    seeds
}

fn threshold_seeds(legacy: bool) -> Vec<Vec<u8>> {
    let new = || B::new(&THRESHOLD_OPS, legacy);
    let mut seeds = Vec::new();
    // Shares, recovery from two subsets and verification, per threshold and id class.
    let classes: [&[[u8; 33]]; 4] = [
        &[rid(1), rid(2), rid(3), rid(4), rid(5), rid(6)],
        &[id(3, 1), id(3, 2), id(3, 3), id(4, 1), id(4, 7), id(2, 9)],
        &[id(2, 5), id(4, 0xff), rid(9), id(3, 0xfe), id(2, 1), rid(10)],
        &[id(4, 2), id(3, 9), rid(11), id(2, 3), id(3, 4), id(4, 4)],
    ];
    for (t, ids) in [2usize, 3, 4, 5].into_iter().zip(classes) {
        let mut b = new();
        let msk: Vec<usize> = (0..t).map(|i| b.sk(key(i as u8 + 1))).collect();
        let mpk: Vec<usize> = msk.iter().map(|&k| b.pubkey(k)).collect();
        let master = b.sign(msk[0], 0x61);
        let n = t + 1;
        let mut shares = Vec::new();
        let mut sigs = Vec::new();
        let mut pk_shares = Vec::new();
        for id in &ids[..n] {
            let s = b.share(true, None, &msk, *id);
            let p = b.share(false, None, &mpk, *id);
            let q = b.pubkey(s);
            b.eq(p, q, true);
            shares.push(s);
            pk_shares.push(p);
            sigs.push(b.sign(s, 0x61));
        }
        b.verify(sigs[0], pk_shares[0], signed(0));
        b.verify(sigs[0], pk_shares[1], signed(0));
        let r1 = b.recover(None, &sigs[..t], &ids[..t]);
        let r2 = b.recover(None, &sigs[1..], &ids[1..n]);
        b.eq(r1, r2, true);
        b.eq(r1, master, true);
        b.verify(r1, mpk[0], signed(0));
        b.verify(r2, mpk[0], h(0x62));
        // All n shares, over-determined.
        b.recover(None, &sigs, &ids[..n]);
        b.ser(r1, true, legacy);
        seeds.push(b.out);
    }
    // Ids ≡ 0 or each other modulo r, and the refusals.
    {
        let mut b = new();
        let msk: Vec<usize> = (0..3).map(|i| b.sk(key(i + 7))).collect();
        let mpk: Vec<usize> = msk.iter().map(|&k| b.pubkey(k)).collect();
        // id 0 and id r give the free coefficient; r + 5 is id 5.
        let s0 = b.share(true, None, &msk, id(0, 0));
        b.eq(s0, msk[0], true);
        let sr = b.share(true, None, &msk, id(1, 0));
        b.eq(sr, s0, true);
        let p0 = b.share(false, None, &mpk, id(0, 0));
        b.eq(p0, mpk[0], true);
        b.share(false, None, &mpk, id(1, 0));
        let a = b.share(true, None, &msk, id(2, 5));
        let c = b.share(true, None, &msk, id(3, 5));
        b.eq(a, c, true);
        // One coefficient, none, an invalid one.
        b.share(true, None, &msk[..1], rid(1));
        b.share(true, None, &[], rid(1));
        b.share(false, None, &mpk[..1], rid(1));
        let zk = b.zero_sk();
        let zp = b.zero_pk();
        b.share(true, None, &[msk[0], zk], rid(1));
        b.share(false, None, &[zp, mpk[1]], rid(1));
        // Shares at ids 1, 2, 3 and recoveries that fail.
        let sigs: Vec<usize> = [1u8, 2, 3]
            .iter()
            .map(|&k| {
                let s = b.share(true, None, &msk, id(3, k));
                b.sign(s, 0x71)
            })
            .collect();
        let ok = b.recover(None, &sigs, &[id(3, 1), id(3, 2), id(3, 3)]);
        b.verify(ok, mpk[0], signed(0));
        b.recover(None, &sigs[..1], &[id(3, 1)]);
        b.recover(None, &[], &[]);
        // Unequal lists, a repeated id, an id ≡ an earlier one, a zero id.
        b.recover(None, &sigs, &[id(3, 1), id(3, 2)]);
        b.recover(None, &sigs[..2], &[id(3, 1), id(3, 2), id(3, 3)]);
        // Ids 23 and 26 of the script's history are id(3, 1).
        b.recover(None, &sigs, &[id(3, 1), id(3, 2), id(6, 23)]);
        b.recover(None, &sigs, &[id(3, 1), id(3, 2), id(7, 26)]);
        b.recover(None, &sigs, &[id(3, 1), id(0, 0), id(3, 3)]);
        b.recover(None, &sigs, &[id(3, 1), id(1, 0), id(3, 3)]);
        let zs = b.zero_sig();
        b.recover(None, &[sigs[0], zs, sigs[2]], &[id(3, 1), id(3, 2), id(3, 3)]);
        seeds.push(b.out);
    }
    // In place: a failed call keeps the receiver's element; a successful one replaces it.
    {
        let mut b = new();
        let msk: Vec<usize> = (0..2).map(|i| b.sk(key(i + 11))).collect();
        let mpk: Vec<usize> = msk.iter().map(|&k| b.pubkey(k)).collect();
        let s1 = b.share(true, None, &msk, id(3, 1));
        let s2 = b.share(true, None, &msk, id(3, 2));
        let g1 = b.sign(s1, 0x81);
        let g2 = b.sign(s2, 0x81);
        let ks = b.share(true, Some(s1), &msk[..1], id(3, 1));
        b.share(true, Some(s1), &msk, id(3, 3));
        let kp = b.share(false, Some(mpk[1]), &[], id(3, 1));
        b.share(false, Some(mpk[1]), &mpk, id(3, 3));
        let kr = b.recover(Some(g1), &[g1], &[id(3, 1)]);
        let r = b.recover(Some(g1), &[g1, g2], &[id(3, 1), id(3, 2)]);
        b.verify(r, mpk[0], signed(0));
        // The kept elements as operands: refused as coefficients and shares.
        b.share(true, None, &[ks, msk[1]], id(3, 4));
        b.share(false, None, &[mpk[0], kp], id(3, 4));
        b.recover(None, &[kr, g2], &[id(3, 1), id(3, 2)]);
        b.recover(Some(kr), &[g1, g2], &[id(3, 1), id(3, 2)]);
        b.eq(kp, mpk[1], false);
        b.verify(kr, mpk[0], signed(0));
        // Invalid receivers, replaced on success and kept on failure, and invalid operands
        // in place.
        let (zk, zp, zs) = (b.zero_sk(), b.zero_pk(), b.zero_sig());
        b.share(true, Some(zk), &msk, id(3, 5));
        b.share(true, Some(zk), &msk[..1], id(3, 5));
        b.share(true, Some(s2), &[msk[0], zk], id(3, 5));
        b.share(false, Some(zp), &mpk, id(3, 5));
        b.share(false, Some(mpk[0]), &[zp, mpk[1]], id(3, 5));
        b.recover(Some(zs), &[g1, g2], &[id(3, 1), id(3, 2)]);
        b.recover(Some(g2), &[g1, zs], &[id(3, 1), id(3, 2)]);
        b.agg(false, &[kp, mpk[0]]);
        seeds.push(b.out);
    }
    // Exotic coefficients and shares (legacy-only reach; refused or kept under basic).
    for part in [0u8, 2, 0x81, 0x84] {
        let mut b = new();
        let msk: Vec<usize> = (0..2).map(|i| b.sk(key(i + 21))).collect();
        let mpk: Vec<usize> = msk.iter().map(|&k| b.pubkey(k)).collect();
        let tp = b.torsion(false, part);
        let ts = b.torsion(true, part);
        let p = b.share(false, None, &[tp, mpk[1]], id(3, 2));
        b.share(false, None, &[mpk[0], tp, mpk[1]], rid(3));
        b.share(false, Some(tp), &mpk[..1], id(3, 2));
        let s = b.share(true, None, &msk, id(3, 1));
        let g = b.sign(s, 0x91);
        let r = b.recover(None, &[ts, g], &[id(3, 2), id(3, 1)]);
        b.recover(Some(ts), &[g], &[id(3, 1)]);
        b.verify(r, mpk[0], signed(0));
        b.verify(ts, tp, signed(0));
        b.verify(g, p, signed(0));
        b.ser(r, true, legacy);
        seeds.push(b.out);
    }
    seeds
}

fn ies_seeds(legacy: bool) -> Vec<Vec<u8>> {
    let new = || B::new(&IES_OPS, legacy);
    let mut seeds = Vec::new();
    // DH: plain, in place, invalid sides.
    {
        let mut b = new();
        let (sks, pks, _) = signers(&mut b, 3, 0xa1);
        let d1 = b.dh(sks[0], pks[1], None);
        let d2 = b.dh(sks[1], pks[0], None);
        b.eq(d1, d2, true);
        b.dh(sks[0], pks[1], Some(pks[2]));
        let zk = b.zero_sk();
        let zp = b.zero_pk();
        b.dh(zk, pks[0], None);
        b.dh(sks[0], zp, None);
        let kept = b.dh(zk, pks[0], Some(pks[2]));
        b.is_valid(kept);
        b.eq(kept, pks[2], false);
        b.dh(sks[1], kept, Some(pks[0]));
        // A basic key equal to r gives the infinity.
        if let Some(r) = b.sk_near_r(0) {
            b.dh(r, pks[0], None);
        }
        let tp = b.torsion(false, 3);
        b.dh(sks[0], tp, None);
        b.dh(sks[0], tp, Some(tp));
        seeds.push(b.out);
    }
    // Encryption to three recipients and every decryption of it, as it is and mutated.
    for (seed, specs) in [
        (0x11u8, [sk_spec(0, 2), R48, ZEROS32]),
        (0x22, [PATTERN96, EMPTY, R_PLUS_1]),
        (0x33, [sk_spec(3, 4), ODD95, ZEROS32]),
    ] {
        let mut b = new();
        let (sks, pks, _) = signers(&mut b, 3, 0xb1);
        let eph = b.sk(key(40));
        let zk = b.zero_sk();
        b.encrypt(eph, seed, &pks, &specs);
        for blob in [false, true] {
            // An invalid key: no DH, so no plaintext.
            b.decrypt(blob, zk, 0, 0, 0, [0, 0, 0]);
            for j in 0..3u8 {
                let k = sks[usize::from(j)];
                b.decrypt(blob, k, 0, j, 0, [0, 0, 0]);
                // The wrong key, the wrong index, an index past the blobs.
                b.decrypt(blob, sks[usize::from((j + 1) % 3)], 0, j, 0, [0, 0, 0]);
                b.decrypt(blob, k, 0, j, 0xc0 + (j + 1) % 3, [0, 0, 0]);
                b.decrypt(blob, k, 0, j, 0xc5, [0, 0, 0]);
                // Truncated, a byte flipped in the key, the seed or the blob, the other
                // flag, a trailing byte.
                b.decrypt(blob, k, 0, j, 0, [2, j, 0]);
                b.decrypt(blob, k, 0, j, 0, [3, 0, 1]);
                b.decrypt(blob, k, 0, j, 0, [3, 9, 4]);
                b.decrypt(blob, k, 0, j, 0, [3, 13, 0x80]);
                b.decrypt(blob, k, 0, j, 0, [4, 0, 0]);
                b.decrypt(blob, k, 0, j, 0, [5, 7, 0]);
            }
        }
        b.decrypt(false, sks[0], 0, 0, 0xff, [0, 0, 0]);
        seeds.push(b.out);
    }
    // Encryptions Core gives up on or refuses, and their containers.
    {
        let mut b = new();
        let (sks, pks, _) = signers(&mut b, 3, 0xc1);
        let eph = b.sk(key(41));
        let zp = b.zero_pk();
        let zk = b.zero_sk();
        // A length CBC refuses: that blob zero-filled, the later ones empty.
        b.encrypt(eph, 1, &pks, &[R48, ODD17, ZEROS32]);
        // An invalid recipient: its blob empty.
        b.encrypt(eph, 2, &[pks[0], zp, pks[2]], &[R48, ZEROS32, ZEROS32]);
        // No recipients.
        b.encrypt(eph, 3, &[], &[]);
        // Unequal lists, an invalid or zero ephemeral key: refused.
        b.encrypt(eph, 4, &pks, &[R48, ZEROS32]);
        b.encrypt(eph, 4, &pks[..1], &[R48, ZEROS32]);
        b.encrypt(zk, 5, &pks[..1], &[R48]);
        for delta in [0, 1, -1] {
            if let Some(r) = b.sk_near_r(delta) {
                b.encrypt(r, 6, &pks[..1], &[R48]);
            }
        }
        for c in 0..3u8 {
            for j in 0..3u8 {
                b.decrypt(false, sks[usize::from(j)], c, j, 0, [0, 0, 0]);
                b.decrypt(true, sks[usize::from(j)], c, j, 0, [0, 0, 0]);
            }
        }
        seeds.push(b.out);
    }
    // Exotic recipients, a kept ephemeral key's container, and plaintext keys at the order.
    for part in [0u8, 1, 0x83] {
        let mut b = new();
        let (sks, pks, _) = signers(&mut b, 2, 0xd1);
        let eph = b.sk(key(42));
        let tp = b.torsion(false, part);
        b.encrypt(eph, 7, &[pks[0], tp, pks[1]], &[R_PLUS_1, sk_spec(1, 3), ZEROS32]);
        b.decrypt(false, sks[0], 0, 0, 0, [0, 0, 0]);
        b.decrypt(true, sks[1], 0, 2, 0, [0, 0, 0]);
        b.decrypt(true, eph, 0, 1, 0, [0, 0, 0]);
        b.decrypt(false, sks[1], 0, 2, 0, [4, 0, 0]);
        let d = b.dh(eph, tp, None);
        b.ser(d, true, legacy);
        seeds.push(b.out);
    }
    seeds
}

fn verify_seeds(legacy: bool) -> Vec<Vec<u8>> {
    let new = || B::new(&VERIFY_OPS, legacy);
    let mut seeds = Vec::new();
    // Group operands over 1 to 4 signers: each signature alone, the aggregate of distinct
    // messages, and the aggregates of one message, the secure one included (design D26).
    for n in [1u8, 2, 4] {
        let mut b = new();
        let sks: Vec<usize> = (0..n).map(|i| b.sk(key(i + 1))).collect();
        let pks: Vec<usize> = sks.iter().map(|&k| b.pubkey(k)).collect();
        let sigs: Vec<usize> = sks.iter().zip(0u8..).map(|(&k, i)| b.sign(k, 0x60 + i)).collect();
        let n = usize::from(n);
        let hashes: Vec<[u8; 33]> = (0..n as u8).map(signed).collect();
        for i in 0..n {
            b.verify(sigs[i], pks[i], hashes[i]);
        }
        b.verify(sigs[0], pks[0], h(0x77));
        let asig = b.agg(true, &sigs);
        b.verify_agg(asig, &pks, &hashes);
        if n >= 2 {
            b.verify(sigs[0], pks[n - 1], hashes[n - 1]);
            b.verify_agg(sigs[0], &pks[..1], &hashes[..1]);
        }
        b.verify_agg(asig, &pks, &vec![h(0x78); n]);
        b.verify_agg(asig, &[], &[]);
        b.verify_agg(asig, &pks, &hashes[..n - 1]);
        b.verify_sec(asig, &pks, hashes[0]);
        // One message: the insecure aggregates verify together, the secure one secured.
        let ones: Vec<usize> = sks.iter().map(|&k| b.sign(k, 0x5f)).collect();
        let one = signed(n as u8);
        let apk = b.agg(false, &pks);
        let aone = b.agg(true, &ones);
        b.verify(aone, apk, one);
        b.verify(aone, pks[0], one);
        let sec = b.secure(&ones, &pks, h(0));
        b.verify_sec(sec, &pks, one);
        b.verify_sec(aone, &pks, one);
        b.verify_sec(sec, &pks[..1], one);
        b.verify_sec(sec, &[], one);
        seeds.push(b.out);
    }
    // Repeated hashes (design D36): two signers of one message, and one signature twice.
    {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 2, 0x71);
        let asig = b.agg(true, &sigs);
        b.verify_agg(asig, &pks, &[signed(0), signed(0)]);
        let twice = b.agg(true, &[sigs[0], sigs[0]]);
        b.verify_agg(twice, &[pks[0], pks[0]], &[signed(0), signed(0)]);
        b.verify_agg(sigs[0], &[pks[0], pks[0]], &[signed(0), signed(0)]);
        seeds.push(b.out);
    }
    // Invalid operands: an invalid signature is false before the list checks, an invalid key
    // fails the insecure checks, and the secure one takes it unchecked as infinity.
    {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 2, 0x81);
        let (zp, zs) = (b.zero_pk(), b.zero_sig());
        b.verify(zs, pks[0], signed(0));
        b.verify(sigs[0], zp, signed(0));
        b.verify_agg(zs, &pks, &[signed(0), h(1)]);
        b.verify_agg(zs, &[], &[]);
        b.verify_agg(sigs[0], &[pks[0], zp], &[signed(0), h(1)]);
        b.verify_sec(sigs[0], &[pks[0], zp], signed(0));
        b.verify_sec(zs, &[zp], signed(0));
        b.verify_sec(zs, &pks, signed(0));
        seeds.push(b.out);
    }
    // Kept elements (design D69): a failed in-place op's receiver, verified as it is kept.
    {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 3, 0x91);
        let kp = b.keep(pks[0]);
        let ks = b.keep(sigs[0]);
        b.verify(ks, pks[0], signed(0));
        b.verify(sigs[0], kp, signed(0));
        b.verify_agg(ks, &[pks[0]], &[signed(0)]);
        b.verify_agg(sigs[1], &[kp, pks[1]], &[h(2), signed(0)]);
        let sec = b.secure(&sigs, &pks, h(0));
        b.verify_sec(sec, &[kp, pks[1], pks[2]], signed(0));
        b.verify_sec(ks, &pks, signed(0));
        let k2 = b.keep(kp);
        b.verify(sigs[0], k2, signed(0));
        seeds.push(b.out);
    }
    // Exotic points: torsion keys and signatures, alone and in aggregates, beside other
    // signers' keys and signatures.
    for part in [0u8, 1, 3, 0x82, 0x85] {
        let mut b = new();
        let (_, pks, sigs) = signers(&mut b, 3, 0xa1);
        let tp = b.torsion(false, part);
        let ts = b.torsion(true, part | 0x80);
        b.verify(sigs[0], tp, signed(0));
        b.verify(ts, pks[0], signed(0));
        b.verify_agg(sigs[0], &[pks[1], tp], &[signed(0), h(3)]);
        b.verify_agg(ts, &[pks[1]], &[signed(0)]);
        b.verify_sec(sigs[0], &[pks[1], tp], signed(0));
        b.verify_sec(ts, &[pks[1], pks[2]], signed(0));
        let apk = b.agg(false, &[tp, pks[1], pks[2]]);
        b.verify(sigs[0], apk, signed(0));
        let asig = b.agg(true, &[ts, sigs[1], sigs[2]]);
        b.verify(asig, pks[0], signed(0));
        let sec = b.secure(&[sigs[1], sigs[2]], &[pks[1], tp], h(0));
        b.verify_sec(sec, &[pks[0], tp], signed(0));
        seeds.push(b.out);
    }
    seeds
}

/// A target's seeds under a flag.
type Seeds = fn(bool) -> Vec<Vec<u8>>;

fn write_seeds(dir: &str, inputs: &[Vec<u8>]) -> std::io::Result<usize> {
    // A harness change can drop a seed: start empty.
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
    let root = format!("{}/../node-fuzz/public-seeds", env!("CARGO_MANIFEST_DIR"));
    let targets: [(&str, Seeds); 4] = [
        ("diff_bls_aggregate", aggregate_seeds),
        ("diff_bls_threshold", threshold_seeds),
        ("diff_bls_ies", ies_seeds),
        ("diff_bls_verify", verify_seeds),
    ];
    for (name, seeds) in targets {
        let inputs: Vec<Vec<u8>> = [false, true].into_iter().flat_map(seeds).collect();
        let n = write_seeds(&format!("{root}/{name}"), &inputs)?;
        eprintln!("{name}: {n} seeds");
    }
    Ok(())
}

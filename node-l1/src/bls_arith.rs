//! `diff_bls_arith` (BS11a-3 D19): `dash_pkc::bls::core`'s element arithmetic (`BlsElem`,
//! `BlsValue`, `ies_key`) against Core's BLS wrappers, dashbls and relic, through
//! `oracle_bls_exec` scripts over mixed registers (BS11a §8).
//!
//! Input: a script of ops, each a selector byte and its operands:
//! - bits 0-3: the op ([`OpKind`], modulo 15);
//! - bit 4: the op's legacy flag: objects are decoded, made and serialized under it;
//! - bit 5: the group of a group op (0: public keys, G1; 1: signatures, G2); `SER` and
//!   `SER_HASH` take a secret key when bits 5 and 6 are both set;
//! - bit 6: `AGG`'s pair form; `DH`'s in-place receiver (a public key register before the
//!   operands).
//!
//! A register operand is one byte, read modulo the live registers of the kind the op needs; an
//! op whose kind has no live register is skipped. Loads build their bytes with
//! `diff_bls_decode`'s classes ([`crate::bls`]), plus the torsion load: a curve point times r and
//! a divisor of the cofactor, so an ℓ-power torsion point, optionally plus the generator. `MUL`
//! scalars come from [`scalar`]'s classes. At most [`MAX_OPS`] ops run, every one logged.
//!
//! Compared per op: Core's status and output against `dash_pkc`'s, with `SER_HASH`'s `GetHash()`
//! cache modelled per register (F10). relic's code after the op and each logged call's code
//! must be `RLC_OK` (but the decode symbols of a `VERIFY_SEC`'s re-decode): anything else is the
//! divergence `relic code` (D19). Each `DH` is followed by
//! a `SER` of its result under the basic flag, against which `ies_key` is compared.

use bitcoin_hashes::sha256d;
use blst::{
    BLST_ERROR, blst_p1, blst_p1_add_or_double, blst_p1_affine, blst_p1_affine_compress,
    blst_p1_from_affine, blst_p1_mult, blst_p1_to_affine, blst_p1_uncompress, blst_p2,
    blst_p2_add_or_double, blst_p2_affine, blst_p2_affine_compress, blst_p2_from_affine,
    blst_p2_mult, blst_p2_to_affine, blst_p2_uncompress,
};
use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_INPLACE, ORACLE_BLS_FLAG_LEGACY, ORACLE_BLS_FLAG_LOG, ORACLE_BLS_FLAG_PAIR,
    ORACLE_BLS_OP_ADD, ORACLE_BLS_OP_AGG_PK, ORACLE_BLS_OP_AGG_SIG, ORACLE_BLS_OP_DECODE,
    ORACLE_BLS_OP_DH, ORACLE_BLS_OP_EQ, ORACLE_BLS_OP_IMPL_EQ, ORACLE_BLS_OP_ISVALID,
    ORACLE_BLS_OP_MUL, ORACLE_BLS_OP_NEG, ORACLE_BLS_OP_PUBKEY, ORACLE_BLS_OP_SER,
    ORACLE_BLS_OP_SER_HASH, ORACLE_BLS_OP_SIGN, ORACLE_BLS_OP_VERIFY_SEC, ORACLE_E_ARG,
    ORACLE_E_DECODE, ORACLE_OK, ORACLE_REJECT,
};
use dash_oracle_sys::{BlsKind, BlsMode, BlsRecord, Oracle, Pin};
use dash_pkc::bls::core::{
    BlsPkElem, BlsPkValue, BlsSigElem, BlsSigValue, BlsSkValue, aggregate_secret_keys_insecure,
    ies_key,
};
use dash_pkc::bls::{
    BLS_PK_LEN, BLS_SIG_LEN, BLS_SK_LEN, BlsEager, BlsScChia, BlsScIetf, BlsSchemeKind,
    BlsSchemeSource, BlsSecretKey, BlsValue, BlsWire,
};

use crate::bls::{
    COMPRESSED, Curve, G1_CURVE, G2_CURVE, PointClass, R, SkClass, add, below_p, byte, laid_out,
    point_bytes, search, sk_bytes, take, unhex, with_flags,
};
use crate::{Compared, Outcome, check, divergence};

/// The most ops one input runs (a `DH` and the `SER` after it count as one): enough for
/// `arith.json5`'s longest script as a seed (D27).
pub const MAX_OPS: usize = 320;
/// The registers at which a script ends, below the oracle's 256: no op starts once a script
/// holds as many. An op stores at most two (a `bls_wrapper` IES decryption's `DECODE`s), so a
/// script holds at most `MAX_REGS + 1`.
pub const MAX_REGS: usize = 250;
/// relic's GLV λ = x² − 1, which bounds `bn_rec_glv`'s digits.
pub const LAMBDA: [u8; BLS_SK_LEN] =
    unhex("00000000000000000000000000000000ac45a4010001a40200000000ffffffff");
/// |x|, the BLS12-381 parameter and the base of `bn_rec_frb`'s digits.
pub const X_ABS: u64 = 0xd201_0000_0001_0000;
/// The generators, compressed in the basic layout.
pub const G1_GEN: [u8; BLS_PK_LEN] = unhex(
    "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb",
);
pub const G2_GEN: [u8; BLS_SIG_LEN] = unhex(
    "93e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8",
);

/// The ops of a script, by selector bits 0-3 (modulo the count).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// `DECODE` of a point built by a [`PointClass`] (operands: class, flags, x).
    LoadPoint,
    /// `DECODE` of a secret key built by an [`SkClass`] (operands: class, 32 bytes).
    LoadSk,
    /// `DECODE` of an ℓ-power torsion point (operands: cofactor part, flags, x).
    LoadTorsion,
    Pubkey,
    /// `SIGN` of a 32-byte hash.
    Sign,
    Add,
    Neg,
    /// `MUL` by a [`scalar`] (operands: register, class, 32 bytes).
    Mul,
    /// `AGG_PK`/`AGG_SIG`: a count (modulo 7) and the registers, or a pair.
    Agg,
    Dh,
    Ser,
    SerHash,
    IsValid,
    Eq,
    ImplEq,
}

impl OpKind {
    const ALL: [Self; 15] = [
        Self::LoadPoint,
        Self::LoadSk,
        Self::LoadTorsion,
        Self::Pubkey,
        Self::Sign,
        Self::Add,
        Self::Neg,
        Self::Mul,
        Self::Agg,
        Self::Dh,
        Self::Ser,
        Self::SerHash,
        Self::IsValid,
        Self::Eq,
        Self::ImplEq,
    ];
}

/// A register's object as Core's wrapper holds it. A destination Core leaves empty
/// (`ORACLE_E_DECODE`, `ORACLE_E_ARG`) is [`Obj::Empty`], never an operand.
#[derive(Clone)]
pub(crate) enum Obj {
    Empty,
    Sk(BlsSkValue),
    Pk(BlsPkValue),
    Sig(BlsSigValue),
}

impl Obj {
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Empty => false,
            Self::Sk(v) => v.is_valid(),
            Self::Pk(v) => v.is_valid(),
            Self::Sig(v) => v.is_valid(),
        }
    }

    /// `ToByteVector(scheme)`: zeros when invalid.
    pub(crate) fn bytes(&self, scheme: BlsSchemeKind) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::Sk(v) => v.to_bytes(scheme).to_vec(),
            Self::Pk(v) => v.to_bytes(scheme).to_vec(),
            Self::Sig(v) => v.to_bytes(scheme).to_vec(),
        }
    }

    /// A destination op's status: `ORACLE_OK` when the object is valid.
    fn status(&self) -> i32 {
        if self.is_valid() {
            ORACLE_OK
        } else {
            ORACLE_REJECT
        }
    }

    fn kind(&self) -> Option<BlsKind> {
        match self {
            Self::Empty => None,
            Self::Sk(_) => Some(BlsKind::SecretKey),
            Self::Pk(_) => Some(BlsKind::PublicKey),
            Self::Sig(_) => Some(BlsKind::Signature),
        }
    }

    /// Whether the element is held in relic's coordinates.
    fn exotic(&self) -> bool {
        match self {
            Self::Pk(v) => BlsPkElem::from_impl(v).__is_relic(),
            Self::Sig(v) => BlsSigElem::from_impl(v).__is_relic(),
            Self::Sk(_) | Self::Empty => false,
        }
    }

    /// Whether the element the object holds, valid or not, has no usable point: off the curve,
    /// outside the subgroup or of order 3 (reachable through legacy decoding). A legacy
    /// verification pairs it on relic's engine (BS11a-4 D1); a subset of [`Self::exotic`], which
    /// also holds for an engine result in the subgroup.
    fn unusable(&self) -> bool {
        match self {
            Self::Pk(v) => !BlsPkElem::from_impl(v).is_valid(),
            Self::Sig(v) => !BlsSigElem::from_impl(v).is_valid(),
            Self::Sk(_) | Self::Empty => false,
        }
    }

    /// Whether the object is invalid but holds an element other than the default one, as a
    /// failed in-place op leaves it.
    fn kept(&self) -> bool {
        match self {
            Self::Pk(v) => !v.is_valid() && !BlsPkElem::from_impl(v).core_eq(&BlsPkElem::default()),
            Self::Sig(v) => {
                !v.is_valid() && !BlsSigElem::from_impl(v).core_eq(&BlsSigElem::default())
            }
            // The sum of one key is its element, valid.
            Self::Sk(v) => {
                !v.is_valid()
                    && aggregate_secret_keys_insecure(&[v]).to_bytes(BlsSchemeKind::Basic)
                        != [0; BLS_SK_LEN]
            }
            Self::Empty => false,
        }
    }

    /// `GetHash()` under `scheme`, through `BlsValue::serialize_hash`.
    fn serialize_hash(&self, scheme: BlsSchemeKind) -> [u8; 32] {
        match self {
            Self::Sk(v) => *v.serialize_hash(scheme).as_bytes(),
            Self::Pk(v) => *v.serialize_hash(scheme).as_bytes(),
            Self::Sig(v) => *v.serialize_hash(scheme).as_bytes(),
            Self::Empty => [0; 32],
        }
    }
}

/// A register: its object and its `GetHash()` cache.
pub(crate) struct Reg {
    obj: Obj,
    hash: Option<[u8; 32]>,
}

/// An op's record as the node expects it.
pub(crate) struct Expected {
    pub(crate) status: i32,
    pub(crate) output: Vec<u8>,
}

/// What an op's register operands held, for coverage ([`Hit`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operands {
    /// An invalid object.
    pub invalid: bool,
    /// An invalid object that kept the element of a failed in-place op.
    pub kept: bool,
    /// A point held in relic's coordinates (an Exotic legacy value).
    pub exotic: bool,
    /// A point with no usable element ([`Obj::unusable`]), which a legacy verification pairs on
    /// relic's engine.
    pub unusable: bool,
    /// The record decodes the secret key a decrypted plaintext holds.
    pub plaintext: bool,
    /// `VERIFY_AGG`'s key list is non-empty and not as long as its hash list.
    pub unequal: bool,
}

/// One record of a script, for coverage: the op, the status the node expects, the op's flags
/// (`ORACLE_BLS_FLAG_*` but `LOG`), whether the register it stored (or the last one) is held in
/// relic's coordinates, and what its operands held (the `bls_wrapper` targets' ops only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub op: u8,
    pub status: i32,
    pub flags: u8,
    pub relic: bool,
    pub operands: Operands,
}

/// An op's selector bits: its scheme, its group (public keys or signatures) and its
/// alternative form.
#[derive(Clone, Copy)]
pub(crate) struct Sel {
    pub(crate) legacy: bool,
    pub(crate) group: BlsKind,
    pub(crate) alt: bool,
}

impl Sel {
    /// The bits at `legacy`, `group` and `alt` of `selector`.
    pub(crate) fn read(selector: u8, legacy: u8, group: u8, alt: u8) -> Self {
        Self {
            legacy: selector & legacy != 0,
            group: if selector & group != 0 {
                BlsKind::Signature
            } else {
                BlsKind::PublicKey
            },
            alt: selector & alt != 0,
        }
    }
}

/// A script: Core's bytes, and the node's registers and expected records.
#[derive(Default)]
pub struct Script {
    pub code: Vec<u8>,
    pub(crate) regs: Vec<Reg>,
    pub(crate) expected: Vec<Expected>,
    /// Per `SER` appended after a `DH`: its record's index and the node's IES key for the
    /// operands (`None` where `ies_key` refuses them).
    pub(crate) ies: Vec<(usize, Option<[u8; 32]>)>,
    /// A divergence found building the script, between the node's own answers: reported by
    /// [`compare_script`] without the oracle.
    mismatch: Option<&'static str>,
    /// Every hash a `SIGN` signed, for the `bls_wrapper` targets' hash operands.
    pub(crate) signed: Vec<[u8; 32]>,
    /// `SER_HASH` through `BlsValue::serialize_hash` rather than SHA-256d here.
    pub(crate) value_hash: bool,
    /// The `bls_wrapper` targets' state: ids and IES containers made so far.
    pub(crate) ext: crate::bls_wrapper::ExtState,
    /// The next record's operand facts, set by the op that emits it.
    pub(crate) pending: Operands,
    hits: Vec<Hit>,
}

pub(crate) fn scheme(legacy: bool) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(legacy)
}

/// An op's flags for the scheme.
pub(crate) fn legacy_flag(legacy: bool) -> u8 {
    if legacy {
        ORACLE_BLS_FLAG_LEGACY
    } else {
        0
    }
}

/// A register index, a list's count or an encoding's length, each below 253: a script holds
/// at most [`MAX_REGS`] + 1 registers, lists of at most 8 and encodings of at most 96 bytes,
/// so each is its own `CompactSize`.
pub(crate) fn index(i: usize) -> u8 {
    i as u8
}

fn eager<K: BlsWire<N>, const N: usize>(bytes: &[u8], legacy: bool) -> Option<BlsValue<K, N>> {
    let bytes = bytes.try_into().ok()?;
    let source = BlsSchemeSource::Version(scheme(legacy));
    BlsEager::<K, N>::decode(bytes, source).ok().map(BlsEager::into_value)
}

fn curve(kind: BlsKind) -> &'static Curve {
    if kind == BlsKind::PublicKey {
        &G1_CURVE
    } else {
        &G2_CURVE
    }
}

impl Script {
    /// Each record's coverage facts, in order.
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Each register's kind, `None` where Core leaves it empty.
    pub fn register_kinds(&self) -> Vec<Option<BlsKind>> {
        self.regs.iter().map(|r| r.obj.kind()).collect()
    }

    /// How many records the script makes.
    pub fn records(&self) -> usize {
        self.expected.len()
    }

    /// Notes what the registers `operands` hold, for the next record's [`Hit`].
    pub(crate) fn note(&mut self, operands: &[usize]) {
        for &i in operands {
            let obj = &self.regs[i].obj;
            self.pending.invalid |= !obj.is_valid();
            self.pending.kept |= obj.kept();
            self.pending.exotic |= obj.exotic();
            self.pending.unusable |= obj.unusable();
        }
    }

    /// Records a divergence in the node's own answers, the first of a script.
    pub(crate) fn diverge(&mut self, field: &'static str) {
        self.mismatch.get_or_insert(field);
    }

    /// `kind`'s live register picked by `pick`.
    pub(crate) fn pick(&self, kind: BlsKind, pick: u8) -> Option<usize> {
        let live: Vec<usize> =
            (0..self.regs.len()).filter(|&i| self.regs[i].obj.kind() == Some(kind)).collect();
        (!live.is_empty()).then(|| live[usize::from(pick) % live.len()])
    }

    pub(crate) fn obj(&self, i: usize) -> &Obj {
        &self.regs[i].obj
    }

    /// Appends an op, logged: its code, flags and operands, then, for an op that stores, the
    /// next register, which holds `made`.
    pub(crate) fn emit(
        &mut self,
        op: u8,
        flags: u8,
        operands: &[u8],
        made: Option<Obj>,
        expected: Expected,
    ) {
        self.code.extend_from_slice(&[op, flags | ORACLE_BLS_FLAG_LOG]);
        self.code.extend_from_slice(operands);
        if let Some(obj) = made {
            self.code.push(index(self.regs.len()));
            self.regs.push(Reg {
                obj,
                hash: None,
            });
        }
        let relic = self.regs.last().is_some_and(|r| r.obj.exotic());
        self.hits.push(Hit {
            op,
            status: expected.status,
            flags,
            relic,
            operands: std::mem::take(&mut self.pending),
        });
        self.expected.push(expected);
    }

    /// A destination op whose result is `made`, output under the scheme `flags` name.
    pub(crate) fn stores(&mut self, op: u8, flags: u8, operands: &[u8], made: Obj) {
        let expected = Expected {
            status: made.status(),
            output: made.bytes(scheme(flags & ORACLE_BLS_FLAG_LEGACY != 0)),
        };
        self.emit(op, flags, operands, Some(made), expected);
    }

    /// A destination op that fails with `status`: Core leaves the destination empty.
    pub(crate) fn fails(&mut self, op: u8, flags: u8, operands: &[u8], status: i32) {
        let expected = Expected {
            status,
            output: Vec::new(),
        };
        self.emit(op, flags, operands, Some(Obj::Empty), expected);
    }

    /// An op that stores nothing and succeeds with `output`.
    fn reports(&mut self, op: u8, flags: u8, operands: &[u8], output: Vec<u8>) {
        let expected = Expected {
            status: ORACLE_OK,
            output,
        };
        self.emit(op, flags, operands, None, expected);
    }

    /// `DECODE` of `bytes` as an eager object of `kind` under `legacy`.
    fn decode(&mut self, kind: BlsKind, legacy: bool, bytes: &[u8]) {
        let flags = legacy_flag(legacy);
        let obj = match kind {
            BlsKind::PublicKey => eager(bytes, legacy).map(Obj::Pk),
            BlsKind::Signature => eager(bytes, legacy).map(Obj::Sig),
            BlsKind::SecretKey => eager(bytes, legacy).map(Obj::Sk),
        };
        let mut operands = vec![kind as u8, BlsMode::Eager as u8, index(bytes.len())];
        operands.extend_from_slice(bytes);
        match obj {
            Some(obj) => self.stores(ORACLE_BLS_OP_DECODE, flags, &operands, obj),
            None => self.fails(ORACLE_BLS_OP_DECODE, flags, &operands, ORACLE_E_DECODE),
        }
    }

    /// One op, its operands read from `rest` (module documentation). Returns the bytes read.
    fn op(&mut self, selector: u8, rest: &[u8]) -> usize {
        let kind = OpKind::ALL[usize::from(selector & 15) % OpKind::ALL.len()];
        self.run(kind, Sel::read(selector, 0x10, 0x20, 0x40), rest)
    }

    /// `kind` with the selector bits `sel`, its operands read from `rest`. Returns the bytes
    /// read.
    pub(crate) fn run(&mut self, kind: OpKind, sel: Sel, rest: &[u8]) -> usize {
        let Sel {
            legacy,
            group,
            alt,
        } = sel;
        let flags = legacy_flag(legacy);
        let at = |i: usize| byte(rest, i);
        let after = |i: usize| rest.get(i..).unwrap_or_default();
        match kind {
            OpKind::LoadPoint => {
                let curve = curve(group);
                let class = PointClass::from_selector(usize::from(at(0)));
                self.decode(group, legacy, &point_bytes(curve, class, after(1), legacy));
                2 + curve.len
            }
            OpKind::LoadSk => {
                let class = SkClass::from_selector(usize::from(at(0)));
                self.decode(BlsKind::SecretKey, legacy, &sk_bytes(class, after(1)));
                1 + BLS_SK_LEN
            }
            OpKind::LoadTorsion => {
                self.decode(group, legacy, &torsion_bytes(group, at(0), after(1), legacy));
                2 + curve(group).len
            }
            OpKind::Pubkey => {
                if let Some(sk) = self.pick(BlsKind::SecretKey, at(0)) {
                    let made = Obj::Pk(pubkey(self.obj(sk)));
                    self.stores(ORACLE_BLS_OP_PUBKEY, flags, &[index(sk)], made);
                }
                1
            }
            OpKind::Sign => {
                if let Some(sk) = self.pick(BlsKind::SecretKey, at(0)) {
                    let h: [u8; 32] = take(rest, 1);
                    self.signed.push(h);
                    let made = Obj::Sig(sign(self.obj(sk), &h, legacy));
                    let operands = [&[index(sk)][..], &h].concat();
                    self.stores(ORACLE_BLS_OP_SIGN, flags, &operands, made);
                }
                33
            }
            OpKind::Add => {
                if let (Some(a), Some(b)) = (self.pick(group, at(0)), self.pick(group, at(1))) {
                    let made = elem_add(self.obj(a), self.obj(b));
                    self.stores(ORACLE_BLS_OP_ADD, flags, &[index(a), index(b)], made);
                }
                2
            }
            OpKind::Neg => {
                if let Some(a) = self.pick(group, at(0)) {
                    let made = elem_neg(self.obj(a));
                    self.stores(ORACLE_BLS_OP_NEG, flags, &[index(a)], made);
                }
                1
            }
            OpKind::Mul => {
                if let Some(a) = self.pick(group, at(0)) {
                    let k = scalar(at(1), after(2));
                    let made = elem_mul(self.obj(a), &k);
                    let operands = [&[index(a)][..], &k].concat();
                    self.stores(ORACLE_BLS_OP_MUL, flags, &operands, made);
                }
                34
            }
            OpKind::Agg => {
                let n = if alt {
                    2
                } else {
                    usize::from(at(0) % 7)
                };
                let items: Option<Vec<usize>> =
                    (0..n).map(|i| self.pick(group, at(1 + i))).collect();
                if let Some(items) = items {
                    self.agg(group, &items, alt, flags);
                }
                1 + n
            }
            OpKind::Dh => {
                let sk = self.pick(BlsKind::SecretKey, at(0));
                let pk = self.pick(BlsKind::PublicKey, at(1));
                // The receiver, where the form has one: `Some(None)` is no receiver.
                let recv = if alt {
                    self.pick(BlsKind::PublicKey, at(2)).map(Some)
                } else {
                    Some(None)
                };
                if let (Some(sk), Some(pk), Some(recv)) = (sk, pk, recv) {
                    self.dh(sk, pk, recv, flags);
                }
                3
            }
            OpKind::Ser | OpKind::SerHash => {
                let target = if alt && group == BlsKind::Signature {
                    BlsKind::SecretKey
                } else {
                    group
                };
                if let Some(a) = self.pick(target, at(0)) {
                    self.ser(a, kind == OpKind::SerHash, legacy);
                }
                1
            }
            OpKind::IsValid => {
                if let Some(a) = self.pick(group, at(0)) {
                    let element = match self.obj(a) {
                        Obj::Pk(v) => BlsPkElem::from_impl(v).is_valid(),
                        Obj::Sig(v) => BlsSigElem::from_impl(v).is_valid(),
                        Obj::Sk(_) | Obj::Empty => false,
                    };
                    let output = vec![u8::from(self.obj(a).is_valid()), u8::from(element)];
                    self.reports(ORACLE_BLS_OP_ISVALID, flags, &[index(a)], output);
                }
                1
            }
            OpKind::Eq | OpKind::ImplEq => {
                if let (Some(a), Some(b)) = (self.pick(group, at(0)), self.pick(group, at(1))) {
                    let wrapper = kind == OpKind::Eq;
                    let eq = equal(self.obj(a), self.obj(b), wrapper);
                    let op = if wrapper {
                        ORACLE_BLS_OP_EQ
                    } else {
                        ORACLE_BLS_OP_IMPL_EQ
                    };
                    self.reports(op, flags, &[index(a), index(b)], vec![u8::from(eq)]);
                }
                2
            }
        }
    }

    /// `AGG_PK`/`AGG_SIG` over `items`: the span `AggregateInsecure`, invalid when empty, or
    /// the pair form, which Core asserts on when a side is invalid (`ORACLE_E_ARG`, the
    /// destination left empty).
    fn agg(&mut self, group: BlsKind, items: &[usize], pair: bool, flags: u8) {
        let (op, made) = if group == BlsKind::PublicKey {
            let pks = items.iter().filter_map(|&i| match self.obj(i) {
                Obj::Pk(v) => Some(v),
                _ => None,
            });
            let agg = BlsPkElem::aggregate_insecure(pks);
            (ORACLE_BLS_OP_AGG_PK, Obj::Pk(agg.map_or_else(BlsPkValue::invalid, Into::into)))
        } else {
            let sigs = items.iter().filter_map(|&i| match self.obj(i) {
                Obj::Sig(v) => Some(v),
                _ => None,
            });
            let agg = BlsSigElem::aggregate_insecure(sigs);
            (ORACLE_BLS_OP_AGG_SIG, Obj::Sig(agg.map_or_else(BlsSigValue::invalid, Into::into)))
        };
        let mut operands = vec![index(items.len())];
        operands.extend(items.iter().map(|&i| index(i)));
        let flags = if pair {
            flags | ORACLE_BLS_FLAG_PAIR
        } else {
            flags
        };
        if !pair || items.iter().all(|&i| self.obj(i).is_valid()) {
            self.stores(op, flags, &operands, made);
        } else {
            self.fails(op, flags, &operands, ORACLE_E_ARG);
        }
    }

    /// `DHKeyExchange` on a fresh wrapper, or on a copy of `recv`, which keeps its element but
    /// not its validity when the exchange fails; then a `SER` of the result under the basic
    /// flag, against which the node's IES key is checked.
    fn dh(&mut self, sk: usize, pk: usize, recv: Option<usize>, flags: u8) {
        let (Obj::Sk(k), Obj::Pk(p)) = (self.obj(sk), self.obj(pk)) else {
            return;
        };
        let key = ies_key(k, p).map(|key| *key);
        let made = if k.is_valid() && p.is_valid() {
            BlsPkElem::from_impl(p).mul(&k.to_bytes(BlsSchemeKind::Basic)).into()
        } else {
            let mut kept = match recv.map(|r| self.obj(r)) {
                Some(Obj::Pk(v)) => v.clone(),
                _ => BlsPkValue::invalid(),
            };
            kept.invalidate();
            kept
        };
        let mut operands: Vec<u8> = recv.map(index).into_iter().collect();
        operands.extend_from_slice(&[index(sk), index(pk)]);
        let flags = if recv.is_some() {
            flags | ORACLE_BLS_FLAG_INPLACE
        } else {
            flags
        };
        self.stores(ORACLE_BLS_OP_DH, flags, &operands, Obj::Pk(made));
        self.ies.push((self.expected.len(), key));
        self.ser(self.regs.len() - 1, false, false);
    }

    /// `SER`, or `SER_HASH`, which fills the register's `GetHash()` cache on first use.
    pub(crate) fn ser(&mut self, a: usize, hash: bool, legacy: bool) {
        let flags = legacy_flag(legacy);
        let value_hash = self.value_hash;
        let reg = &mut self.regs[a];
        let bytes = reg.obj.bytes(scheme(legacy));
        let status = reg.obj.status();
        let (op, output) = if hash {
            let digest = *reg.hash.get_or_insert_with(|| {
                if value_hash {
                    reg.obj.serialize_hash(scheme(legacy))
                } else {
                    sha256d::Hash::hash(&bytes).to_byte_array()
                }
            });
            (ORACLE_BLS_OP_SER_HASH, [bytes, digest.to_vec()].concat())
        } else {
            (ORACLE_BLS_OP_SER, bytes)
        };
        let expected = Expected {
            status,
            output,
        };
        self.emit(op, flags, &[index(a)], None, expected);
    }
}

/// `GetPublicKey`: the generator times the key, or the invalid key.
fn pubkey(sk: &Obj) -> BlsPkValue {
    match (sk, BlsPkValue::from_canonical(&G1_GEN, BlsSchemeKind::Basic)) {
        (Obj::Sk(k), Some(g)) if k.is_valid() => {
            BlsPkElem::from_impl(&g).mul(&k.to_bytes(BlsSchemeKind::Basic)).into()
        }
        _ => BlsPkValue::invalid(),
    }
}

/// `Sign(h, legacy)`: the scheme's hash to G2 times the key, or the invalid signature.
fn sign(sk: &Obj, h: &[u8; 32], legacy: bool) -> BlsSigValue {
    let Obj::Sk(k) = sk else {
        return BlsSigValue::invalid();
    };
    if !k.is_valid() {
        return BlsSigValue::invalid();
    }
    let k = k.to_bytes(BlsSchemeKind::Basic);
    let signed = if legacy {
        BlsSecretKey::<BlsScChia>::from_bytes(&k).ok().map(|k| k.sign_digest(h).to_bytes())
    } else {
        BlsSecretKey::<BlsScIetf>::from_bytes(&k).ok().map(|k| k.sign_digest(h).to_bytes())
    };
    // A key the typed one refuses is r (a basic key): the hash times r, the infinity.
    signed
        .and_then(|bytes| BlsSigValue::from_canonical(&bytes, scheme(legacy)))
        .unwrap_or_else(|| BlsSigElem::default().into())
}

fn elem_add(a: &Obj, b: &Obj) -> Obj {
    match (a, b) {
        (Obj::Pk(a), Obj::Pk(b)) => {
            Obj::Pk(BlsPkElem::from_impl(a).add(&BlsPkElem::from_impl(b)).into())
        }
        (Obj::Sig(a), Obj::Sig(b)) => {
            Obj::Sig(BlsSigElem::from_impl(a).add(&BlsSigElem::from_impl(b)).into())
        }
        _ => Obj::Empty,
    }
}

fn elem_neg(a: &Obj) -> Obj {
    match a {
        Obj::Pk(a) => Obj::Pk(BlsPkElem::from_impl(a).neg().into()),
        Obj::Sig(a) => Obj::Sig(BlsSigElem::from_impl(a).neg().into()),
        _ => Obj::Empty,
    }
}

fn elem_mul(a: &Obj, k: &[u8; 32]) -> Obj {
    match a {
        Obj::Pk(a) => Obj::Pk(BlsPkElem::from_impl(a).mul(k).into()),
        Obj::Sig(a) => Obj::Sig(BlsSigElem::from_impl(a).mul(k).into()),
        _ => Obj::Empty,
    }
}

/// `EQ` (the wrapper's `operator==`) or `IMPL_EQ` (the elements').
fn equal(a: &Obj, b: &Obj, wrapper: bool) -> bool {
    match (a, b) {
        (Obj::Pk(a), Obj::Pk(b)) if wrapper => a.core_eq(b),
        (Obj::Sig(a), Obj::Sig(b)) if wrapper => a.core_eq(b),
        (Obj::Pk(a), Obj::Pk(b)) => BlsPkElem::from_impl(a).core_eq(&BlsPkElem::from_impl(b)),
        (Obj::Sig(a), Obj::Sig(b)) => BlsSigElem::from_impl(a).core_eq(&BlsSigElem::from_impl(b)),
        _ => false,
    }
}

/// A `MUL` scalar, big-endian, from a class byte and the bytes after it: the bytes as they
/// are; a value of at most 64 bits (relic's `bn_mul_dig` path); r, 2⁶⁴, λ, |x|^j (j = 1 to 4,
/// truncated to 256 bits) or ⌊r/2⌋ plus a signed byte; or 2²⁵⁶ − 1 minus a byte (BS11a §8: the
/// reduction and recoding boundaries).
pub fn scalar(class: u8, rest: &[u8]) -> [u8; 32] {
    let near = |mut k: [u8; 32]| {
        add(&mut k, i16::from(byte(rest, 0) as i8));
        k
    };
    match class % 8 {
        0 => take(rest, 0),
        1 => {
            let mut k = [0u8; 32];
            k[24..].copy_from_slice(&take::<8>(rest, 0));
            k
        }
        2 => near(R),
        3 => {
            let mut k = [0u8; 32];
            k[23] = 1;
            near(k)
        }
        4 => {
            let mut k = [0xff; 32];
            k[31] = !byte(rest, 0);
            k
        }
        5 => near(LAMBDA),
        6 => {
            let mut k = [0u8; 32];
            k[31] = 1;
            for _ in 0..=byte(rest, 1) % 4 {
                k = mul_u64(&k, X_ABS);
            }
            near(k)
        }
        _ => {
            let mut half = [0u8; 32];
            let mut carry = 0u8;
            for (h, r) in half.iter_mut().zip(R) {
                *h = carry << 7 | r >> 1;
                carry = r & 1;
            }
            near(half)
        }
    }
}

/// `a × m` modulo 2²⁵⁶, big-endian.
fn mul_u64(a: &[u8; 32], m: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut carry = 0u128;
    for (o, &b) in out.iter_mut().zip(a).rev() {
        let v = u128::from(b) * u128::from(m) + carry;
        *o = v as u8;
        carry = v >> 8;
    }
    out
}

/// Each group's cofactor h, big-endian hex, and its small prime factors ℓ with their exponents.
const G1_COFACTOR: (&str, &[(u64, u32)]) = (
    "396c8c005555e1568c00aaab0000aaab",
    &[(3, 1), (11, 2), (10177, 2), (859_267, 2), (52_437_899, 2)],
);
const G2_COFACTOR: (&str, &[(u64, u32)]) = (
    "5d543a95414e7f1091d50792876a202cd91de4547085abaa68a205b2e5a7ddfa628f1cb4d9e82ef21537e293a6691ae1616ec6e786f0c70cf1c38e31c7238e5",
    &[(13, 2), (23, 2), (2713, 1), (11953, 1), (262_069, 1)],
);

/// The multipliers of the torsion load, little-endian: 1 (the whole cofactor torsion),
/// h/ℓ^e for each small prime ℓ of h (its ℓ-power part) and, when h has a prime beyond them
/// (h₂'s 448-bit one), the product of the ℓ^e (that prime's part).
fn cofactor_parts(kind: BlsKind) -> Vec<Vec<u8>> {
    let (h, primes) = if kind == BlsKind::PublicKey {
        G1_COFACTOR
    } else {
        G2_COFACTOR
    };
    let small: u128 = primes.iter().map(|&(prime, e)| u128::from(prime).pow(e)).product();
    let large = format!("{small:x}") != h;
    let padded = format!("{h:0>width$}", width = h.len().div_ceil(8) * 8);
    let h: Vec<u32> = (0..padded.len())
        .step_by(8)
        .map(|i| u32::from_str_radix(&padded[i..i + 8], 16).unwrap_or_default())
        .collect();
    let mut parts = vec![vec![1]];
    for &(prime, e) in primes {
        let mut d = h.clone();
        for _ in 0..e {
            let mut rem = 0u64;
            for limb in &mut d {
                let cur = rem << 32 | u64::from(*limb);
                rem = cur % prime;
                *limb = (cur / prime) as u32;
            }
        }
        parts.push(d.iter().rev().flat_map(|l| l.to_le_bytes()).collect());
    }
    if large {
        parts.push(small.to_le_bytes().to_vec());
    }
    parts
}

/// A scalar, little-endian, widened past 256 bits: blst's `mult` takes its GLV path, which is
/// correct only in the subgroup, for 193 to 256 bits.
fn wide(le: &[u8]) -> Vec<u8> {
    let mut out = le.to_vec();
    out.resize(le.len().max(33), 0);
    out
}

/// An ℓ-power torsion point's encoding: the first curve point from `rest` (as
/// [`PointClass::OnCurve`] finds it) times r and the cofactor part `part` picks, plus the
/// generator when bit 7 of `part` is set; laid out under `legacy` with `rest`'s flag bits.
fn torsion_bytes(kind: BlsKind, part: u8, rest: &[u8], legacy: bool) -> Vec<u8> {
    let curve = curve(kind);
    let flags = byte(rest, 0);
    let (x, step) = below_p(curve, rest, flags);
    let x = search(curve, x, step, true);
    let parts = cofactor_parts(kind);
    let mut r = R;
    r.reverse();
    let m = [wide(&r), wide(&parts[usize::from(part & 0x7f) % parts.len()])];
    let with_gen = part & 0x80 != 0;
    let compressed = with_flags(x.clone(), COMPRESSED);
    let point = if kind == BlsKind::PublicKey {
        g1_torsion(&compressed, &m, with_gen)
    } else {
        g2_torsion(&compressed, &m, with_gen)
    };
    // The basic layout without its flags, which `laid_out` sets from the input's.
    laid_out(with_flags(point.unwrap_or(x), 0), flags, legacy)
}

// SAFETY (both functions): blst's point functions read their inputs and write their outputs
// only; each encoding holds the 48 or 96 bytes uncompress reads, and each scalar the
// `8 × len` bits mult reads.
#[allow(unsafe_code)]
fn g1_torsion(x: &[u8], m: &[Vec<u8>; 2], with_gen: bool) -> Option<Vec<u8>> {
    let mut aff = blst_p1_affine::default();
    if unsafe { blst_p1_uncompress(&mut aff, x.as_ptr()) } != BLST_ERROR::BLST_SUCCESS {
        return None;
    }
    let mut p = blst_p1::default();
    unsafe { blst_p1_from_affine(&mut p, &aff) };
    for k in m {
        let q = p;
        unsafe { blst_p1_mult(&mut p, &q, k.as_ptr(), 8 * k.len()) };
    }
    if with_gen {
        let (mut g, mut gp) = (blst_p1_affine::default(), blst_p1::default());
        let q = p;
        unsafe {
            blst_p1_uncompress(&mut g, G1_GEN.as_ptr());
            blst_p1_from_affine(&mut gp, &g);
            blst_p1_add_or_double(&mut p, &q, &gp);
        }
    }
    let mut out = [0u8; BLS_PK_LEN];
    unsafe {
        blst_p1_to_affine(&mut aff, &p);
        blst_p1_affine_compress(out.as_mut_ptr(), &aff);
    }
    Some(out.to_vec())
}

#[allow(unsafe_code)]
fn g2_torsion(x: &[u8], m: &[Vec<u8>; 2], with_gen: bool) -> Option<Vec<u8>> {
    let mut aff = blst_p2_affine::default();
    if unsafe { blst_p2_uncompress(&mut aff, x.as_ptr()) } != BLST_ERROR::BLST_SUCCESS {
        return None;
    }
    let mut p = blst_p2::default();
    unsafe { blst_p2_from_affine(&mut p, &aff) };
    for k in m {
        let q = p;
        unsafe { blst_p2_mult(&mut p, &q, k.as_ptr(), 8 * k.len()) };
    }
    if with_gen {
        let (mut g, mut gp) = (blst_p2_affine::default(), blst_p2::default());
        let q = p;
        unsafe {
            blst_p2_uncompress(&mut g, G2_GEN.as_ptr());
            blst_p2_from_affine(&mut gp, &g);
            blst_p2_add_or_double(&mut p, &q, &gp);
        }
    }
    let mut out = [0u8; BLS_SIG_LEN];
    unsafe {
        blst_p2_to_affine(&mut aff, &p);
        blst_p2_affine_compress(out.as_mut_ptr(), &aff);
    }
    Some(out.to_vec())
}

/// The script `bytes` describe (module documentation).
pub fn read_script(bytes: &[u8]) -> Script {
    read_with(Script::default(), bytes, Script::op)
}

/// The script `bytes` describe, each op read by `op` from its selector and the bytes after
/// it, into `script`: at most [`MAX_OPS`] ops, ending before one that could store past
/// [`MAX_REGS`] registers.
pub(crate) fn read_with(
    mut script: Script,
    bytes: &[u8],
    op: impl Fn(&mut Script, u8, &[u8]) -> usize,
) -> Script {
    let mut at = 0;
    for _ in 0..MAX_OPS {
        let Some(&selector) = bytes.get(at) else {
            break;
        };
        if script.regs.len() >= MAX_REGS {
            break;
        }
        at += 1 + op(&mut script, selector, bytes.get(at + 1..).unwrap_or_default());
    }
    script
}

/// `diff_bls_arith`.
pub fn compare_bls_arith(bytes: &[u8], pin: Pin) -> Compared {
    compare_script(&read_script(bytes), pin)
}

/// A script against Core's: out of domain when it runs no op.
pub(crate) fn compare_script(script: &Script, pin: Pin) -> Compared {
    if script.expected.is_empty() {
        return Ok(Outcome::OutOfDomain);
    }
    if let Some(field) = script.mismatch {
        return Err(divergence(field));
    }
    let records = Oracle::new(pin).bls_exec(&script.code).map_err(|_| divergence("oracle"))?;
    compare_records(script, &records)
}

/// Core's records against the node's.
pub fn compare_records(script: &Script, records: &[BlsRecord]) -> Compared {
    check(records.len() == script.expected.len(), "records")?;
    for (i, (record, expected)) in records.iter().zip(&script.expected).enumerate() {
        // The decode symbols of `VERIFY_SEC`'s re-decode set relic's code for a point Core
        // then refuses (BS11a D8 §3.6): dashbls clears it right after the read.
        let redecode = script.hits[i].op == ORACLE_BLS_OP_VERIFY_SEC;
        let logged = record.log.iter().flatten().all(|call| {
            call.code == 0
                || redecode && matches!(call.symbol.as_str(), "ep_read_bin" | "ep2_read_bin")
        });
        check(record.relic_code == 0 && logged, "relic code")?;
        check(record.status == expected.status, "status")?;
        check(record.output == expected.output, "output")?;
    }
    for &(i, key) in &script.ies {
        let core = (records[i].status == ORACLE_OK).then(|| take::<32>(&records[i].output, 0));
        check(key == core, "ies key")?;
    }
    Ok(Outcome::BothAccept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(x: [u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut carry = 0u16;
        for (o, b) in out.iter_mut().zip(x).rev() {
            let v = u16::from(b) * 2 + carry;
            *o = v as u8;
            carry = v >> 8;
        }
        out
    }

    #[test]
    fn scalar_classes_hit_their_boundaries() {
        assert_eq!(scalar(2, &[0]), R);
        let mut r_minus_one = R;
        add(&mut r_minus_one, -1);
        assert_eq!(double(scalar(7, &[0])), r_minus_one, "r is odd");
        assert_eq!(scalar(6, &[0, 0])[24..], X_ABS.to_be_bytes());
        assert_eq!(scalar(4, &[0]), [0xff; 32]);
        assert_eq!(scalar(5, &[0]), LAMBDA);
        let mut small = [0u8; 32];
        small[31] = 9;
        assert_eq!(scalar(1, &[0, 0, 0, 0, 0, 0, 0, 9]), small);
    }

    #[test]
    fn cofactor_parts_cover_each_prime() {
        // h₁ factors into its five small primes; h₂ also has a 448-bit one.
        for (kind, n) in [(BlsKind::PublicKey, 6), (BlsKind::Signature, 7)] {
            let parts = cofactor_parts(kind);
            assert_eq!(parts.len(), n);
            assert_eq!(parts[0], [1]);
        }
        let small = 13u128.pow(2) * 23u128.pow(2) * 2713 * 11953 * 262_069;
        assert_eq!(cofactor_parts(BlsKind::Signature)[6], small.to_le_bytes());
    }

    #[test]
    fn torsion_loads_leave_the_subgroup() {
        for kind in [BlsKind::PublicKey, BlsKind::Signature] {
            let len = curve(kind).len;
            for part in 0..cofactor_parts(kind).len() as u8 {
                let bytes = torsion_bytes(kind, part, &[COMPRESSED, 7], false);
                assert_eq!(bytes.len(), len);
                // On the curve, and neither the infinity nor in the subgroup.
                let verdict = if kind == BlsKind::PublicKey {
                    blst::min_pk::PublicKey::uncompress(&bytes).and_then(|p| p.validate())
                } else {
                    blst::min_sig::PublicKey::uncompress(&bytes).and_then(|p| p.validate())
                };
                assert_eq!(
                    verdict,
                    Err(BLST_ERROR::BLST_POINT_NOT_IN_GROUP),
                    "{kind:?} part {part}"
                );
            }
        }
    }

    #[test]
    fn a_script_skips_ops_without_operands() {
        // ADD with no registers runs nothing.
        assert!(read_script(&[5]).expected.is_empty());
        // A secret key load, PUBKEY and DH: four records with the SER after the DH.
        let mut bytes = vec![1, 0];
        bytes.extend([0u8; 31]);
        bytes.push(5);
        bytes.extend([3, 0, 9, 0, 0, 0]);
        let script = read_script(&bytes);
        assert_eq!(script.expected.len(), 4);
        assert_eq!(script.ies.len(), 1);
    }

    /// The public seeds run every op, each group op on both representations, and every
    /// status the harness expects (`ORACLE_E_DECODE`, `ORACLE_E_ARG`, `ORACLE_REJECT`).
    #[test]
    fn public_seeds_reach_every_op() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_bls_arith");
        let mut seen = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let input = std::fs::read(entry.path()).unwrap_or_default();
            seen.extend(read_script(&input).hits().iter().copied());
        }
        let ops = [
            ORACLE_BLS_OP_DECODE,
            ORACLE_BLS_OP_PUBKEY,
            ORACLE_BLS_OP_SIGN,
            ORACLE_BLS_OP_ADD,
            ORACLE_BLS_OP_NEG,
            ORACLE_BLS_OP_MUL,
            ORACLE_BLS_OP_AGG_PK,
            ORACLE_BLS_OP_AGG_SIG,
            ORACLE_BLS_OP_DH,
            ORACLE_BLS_OP_SER,
            ORACLE_BLS_OP_SER_HASH,
            ORACLE_BLS_OP_ISVALID,
            ORACLE_BLS_OP_EQ,
            ORACLE_BLS_OP_IMPL_EQ,
        ];
        for op in ops {
            assert!(seen.iter().any(|h| h.op == op), "op {op}");
        }
        for op in [ORACLE_BLS_OP_ADD, ORACLE_BLS_OP_NEG, ORACLE_BLS_OP_MUL, ORACLE_BLS_OP_DH] {
            assert!(seen.iter().any(|h| h.op == op && h.relic), "op {op} on the engine");
            assert!(seen.iter().any(|h| h.op == op && !h.relic), "op {op} on blst");
        }
        for status in [ORACLE_E_DECODE, ORACLE_E_ARG, ORACLE_REJECT] {
            assert!(seen.iter().any(|h| h.status == status), "status {status}");
        }
    }
}

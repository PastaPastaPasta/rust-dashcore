//! The BLS wrapper targets, against Core's through `oracle_bls_exec` scripts over
//! `diff_bls_arith`'s registers ([`crate::bls_arith`], BS11a-3 D19):
//! - `diff_bls_aggregate`, `diff_bls_threshold` and `diff_bls_ies` (BS11b D25): the wrapper
//!   operations `dash_pkc::bls::core` adds in BS11b (aggregates, `SubInsecure`, the secure
//!   aggregate and its verification, threshold shares and recovery, `DHKeyExchange` and
//!   multi-recipient BLS-IES);
//! - `diff_bls_verify` (BS11a-4 D10): Core's three verifications (`VERIFY`, `VERIFY_AGG`,
//!   `VERIFY_SEC`) under both flags, with the aggregates and kept elements they verify.
//!
//! Input: a script of ops, each a selector byte and its operands, as `diff_bls_arith`'s but
//! with a selector of its own:
//! - bits 0-4: the op, modulo the length of the target's table ([`AGGREGATE_OPS`],
//!   [`THRESHOLD_OPS`], [`IES_OPS`], [`VERIFY_OPS`]); each table lists the target's own ops
//!   first, which the modulo gives double weight, then `diff_bls_arith`'s loads and glue
//!   ([`WrapperOp::Base`]), which read their operands as there;
//! - bit 5: the op's legacy flag;
//! - bit 6: the group of a group op (0: public keys; 1: signatures); for `RECOVER_SIG`,
//!   `AGG_SECURE`, `VERIFY_AGG` and `IES_ENCRYPT`, a second list of a length of its own;
//! - bit 7: the alternative form: `AGG`'s pair, the in-place receiver of `SK_SHARE`,
//!   `PK_SHARE`, `RECOVER_SIG` and `DH`, and `SER`'s secret key with bit 6.
//!
//! A list is a count byte (modulo [`LIST_MAX`] + 1) and its entries. An id is a class byte and
//! 32 bytes ([`Script::id`]); a hash is a byte whose low bit picks an earlier `SIGN`'s hash
//! and 32 bytes otherwise. The IES ops decrypt a container an earlier `IES_ENCRYPT` made,
//! mutated or not ([`Script::container`]), and each decryption that succeeds is followed by
//! `DECODE`s of its first 32 plaintext bytes as a secret key under each flag, against which
//! `decrypt_secret_key` is compared.
//!
//! Compared per op as `diff_bls_arith` compares, with `SER_HASH` through
//! `BlsValue::serialize_hash`. Every verification is compared, a legacy one over a point with
//! no usable element (off the curve, outside the subgroup or of order 3) included; a verdict
//! `Unsupported` is a divergence. An in-place op's `bool` is compared with its result's
//! validity. `AGG_SECURE` over more than 16 signers with two equal keys and different
//! signatures, where Core's `std::sort` is unstable (D8), is left out; lists of at most
//! [`LIST_MAX`] never reach it.

use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_INPLACE, ORACLE_BLS_FLAG_LEGACY, ORACLE_BLS_FLAG_PAIR, ORACLE_BLS_OP_AGG_PK,
    ORACLE_BLS_OP_AGG_SECURE, ORACLE_BLS_OP_AGG_SIG, ORACLE_BLS_OP_AGG_SK, ORACLE_BLS_OP_DECODE,
    ORACLE_BLS_OP_DH, ORACLE_BLS_OP_IES_DECRYPT, ORACLE_BLS_OP_IES_DECRYPT_BLOB,
    ORACLE_BLS_OP_IES_ENCRYPT, ORACLE_BLS_OP_PK_SHARE, ORACLE_BLS_OP_RECOVER_SIG,
    ORACLE_BLS_OP_SK_SHARE, ORACLE_BLS_OP_SUB_SIG, ORACLE_BLS_OP_VERIFY, ORACLE_BLS_OP_VERIFY_AGG,
    ORACLE_BLS_OP_VERIFY_SEC, ORACLE_E_ARG, ORACLE_E_DECODE, ORACLE_E_TRAILING, ORACLE_OK,
    ORACLE_REJECT,
};
use dash_oracle_sys::{BlsKind, BlsMode, Pin};
use dash_pkc::bls::core::{
    BlsIesBlobValue, BlsIesMultiBlobs, BlsPkElem, BlsPkValue, BlsSigValue, BlsSkValue, BlsVerdict,
    aggregate_public_keys_insecure, aggregate_public_keys_insecure_pair,
    aggregate_secret_keys_insecure, aggregate_signatures_insecure,
    aggregate_signatures_insecure_pair, aggregate_signatures_secure, dh_key_exchange, ies_key,
    public_key_share, recover_signature, secret_key_share, sub_signatures_insecure,
    verify_insecure, verify_insecure_aggregated, verify_secure_aggregated,
};
use dash_pkc::bls::{BLS_SK_LEN, BlsSchemeKind};

use crate::Compared;
use crate::bls::{R, add, byte, take};
use crate::bls_arith::{
    Expected, Obj, OpKind, Script, Sel, compare_script, index, legacy_flag, read_with, scheme,
};

/// The most entries of a script list: registers, ids, recipients and plaintexts.
pub const LIST_MAX: usize = 8;
// D8's unstable-sort exclusion is unreachable at that bound.
const _: () = assert!(LIST_MAX <= 16);
/// The width of an id or hash operand: a class byte and 32 bytes.
const H: usize = 33;
/// The width of a plaintext operand ([`plaintext`]).
const PLAINTEXT: usize = 4;
/// The widest IV index a blob decryption hashes to: `CBLSIESEncryptedBlob::Decrypt` hashes
/// the seed `idx` times, unbounded.
const BLOB_IDX: u8 = 32;

/// An op of the four targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapperOp {
    /// A `diff_bls_arith` op, its operands read as there.
    Base(OpKind),
    /// `AGG_SK` over a list.
    AggSk,
    /// `AGG_PK` or `AGG_SIG` (bit 6) over a list, or the pair form (bit 7).
    AggGroup,
    /// `SUB_SIG` (operands: two signatures).
    SubSig,
    /// `AGG_SECURE` (operands: signatures, public keys, a hash Core ignores).
    AggSecure,
    /// `VERIFY_SEC` (operands: a signature, public keys, a hash).
    VerifySec,
    /// A failed in-place op that keeps its receiver's element: `SK_SHARE`, `PK_SHARE` or
    /// `RECOVER_SIG` (by bits 6 and 7, as `SER`) on empty lists (operand: the receiver).
    Keep,
    /// `SK_SHARE` (operands: receiver, coefficients, id).
    SkShare,
    /// `PK_SHARE` (operands: receiver, coefficients, id).
    PkShare,
    /// `RECOVER_SIG` (operands: receiver, shares, ids).
    Recover,
    /// `VERIFY` (operands: a signature, a public key, a hash).
    Verify,
    /// `VERIFY_AGG` (operands: a signature, public keys, hashes).
    VerifyAgg,
    /// `DH` (operands: secret key, public key, receiver), then a `SER` of its result under the
    /// basic flag, against which `ies_key` is compared.
    Dh,
    /// `IES_ENCRYPT` (operands: ephemeral key, seed, recipients, plaintexts).
    IesEncrypt,
    /// `IES_DECRYPT` of a container (operands: index, secret key, container).
    IesDecrypt,
    /// `IES_DECRYPT_BLOB` of one blob of a container (operands: index, secret key, container,
    /// blob).
    IesDecryptBlob,
}

use WrapperOp::{
    AggGroup, AggSecure, AggSk, Base, Dh, IesDecrypt, IesDecryptBlob, IesEncrypt, Keep, PkShare,
    Recover, SkShare, SubSig, Verify, VerifyAgg, VerifySec,
};

/// `diff_bls_arith`'s loads and glue, which every table ends with.
const BASE_OPS: [WrapperOp; 13] = [
    Base(OpKind::LoadPoint),
    Base(OpKind::LoadSk),
    Base(OpKind::LoadTorsion),
    Base(OpKind::Pubkey),
    Base(OpKind::Sign),
    Base(OpKind::Add),
    Base(OpKind::Neg),
    Base(OpKind::Mul),
    Base(OpKind::Ser),
    Base(OpKind::SerHash),
    Base(OpKind::IsValid),
    Base(OpKind::Eq),
    Base(OpKind::ImplEq),
];

/// A target's table of `N` ops: its `own`, then [`BASE_OPS`].
const fn table<const OWN: usize, const N: usize>(own: [WrapperOp; OWN]) -> [WrapperOp; N] {
    assert!(OWN + BASE_OPS.len() == N);
    let mut out = [Base(OpKind::LoadPoint); N];
    let mut i = 0;
    while i < N {
        out[i] = if i < OWN {
            own[i]
        } else {
            BASE_OPS[i - OWN]
        };
        i += 1;
    }
    out
}

/// `diff_bls_aggregate`'s ops, the target's own first: `selector & 0x1f` modulo the table's length
/// weighs the first 13 double.
pub const AGGREGATE_OPS: [WrapperOp; 19] =
    table([AggSk, AggGroup, SubSig, AggSecure, VerifySec, Keep]);

/// `diff_bls_threshold`'s ops, the target's own first: `selector & 0x1f` modulo the table's length
/// weighs the first 14 double.
pub const THRESHOLD_OPS: [WrapperOp; 18] = table([SkShare, PkShare, Recover, Verify, AggGroup]);

/// `diff_bls_ies`'s ops, the target's own first: `selector & 0x1f` modulo the table's length
/// weighs the first 15 double.
pub const IES_OPS: [WrapperOp; 17] = table([Dh, IesEncrypt, IesDecrypt, IesDecryptBlob]);

/// `diff_bls_verify`'s ops, the target's own first: the three verifications, then the
/// aggregates and kept elements they verify (BS11a design D26, D69). `selector & 0x1f` modulo
/// the table's length weighs the first 13 double.
pub const VERIFY_OPS: [WrapperOp; 19] =
    table([Verify, VerifyAgg, VerifySec, AggGroup, AggSecure, Keep]);

/// The selector of `op` in `table` with its bits (module documentation), `None` when `table`
/// has no `op`.
pub fn selector(
    table: &[WrapperOp],
    op: WrapperOp,
    legacy: bool,
    sig: bool,
    alt: bool,
) -> Option<u8> {
    let at = table.iter().position(|&o| o == op)?;
    Some(index(at) | u8::from(legacy) << 5 | u8::from(sig) << 6 | u8::from(alt) << 7)
}

/// The targets' state beyond `diff_bls_arith`'s registers.
#[derive(Default)]
pub(crate) struct ExtState {
    /// Every id a script used, for the repeat classes.
    ids: Vec<[u8; 32]>,
    /// Every container an `IES_ENCRYPT` made, with the flag it is serialized under.
    containers: Vec<(BlsIesMultiBlobs, bool)>,
}

/// `CompactSize(n)`.
fn compact(n: usize, out: &mut Vec<u8>) {
    if n < 253 {
        out.push(index(n));
    } else if let Ok(n) = u16::try_from(n) {
        out.push(253);
        out.extend(n.to_le_bytes());
    } else {
        out.push(254);
        out.extend((n as u32).to_le_bytes());
    }
}

/// `x + y` and `x − y` modulo 2²⁵⁶, big-endian, with the carry or borrow out.
fn add256(x: &[u8; 32], y: &[u8; 32], sub: bool) -> ([u8; 32], bool) {
    let mut out = [0u8; 32];
    let mut carry = 0i32;
    for i in (0..32).rev() {
        let v = if sub {
            i32::from(x[i]) - i32::from(y[i]) + carry
        } else {
            i32::from(x[i]) + i32::from(y[i]) + carry
        };
        out[i] = v.rem_euclid(256) as u8;
        carry = v.div_euclid(256);
    }
    (out, carry != 0)
}

/// A list operand: the count, then the registers.
fn list_operand(items: &[usize]) -> Vec<u8> {
    let mut out = vec![index(items.len())];
    out.extend(items.iter().map(|&i| index(i)));
    out
}

/// A list's length from its count byte.
fn count(byte: u8) -> usize {
    usize::from(byte) % (LIST_MAX + 1)
}

/// The length of a second list: with `sig` (bit 6), from its own count byte; else the first
/// list's, which took `used` bytes, a count byte among them.
fn second_len(sig: bool, used: usize, count_byte: u8) -> usize {
    if sig {
        count(count_byte)
    } else {
        used - 1
    }
}

/// The registers a receiver and its list operands read, for [`Script::note`].
fn read_regs(recv: Option<usize>, items: &[usize]) -> Vec<usize> {
    recv.into_iter().chain(items.iter().copied()).collect()
}

/// A plaintext of a length from {0, 16k, 16k + j} (k ≤ 5, 1 ≤ j ≤ 15) from its four operand
/// bytes: the length's class and size, the content's class and a parameter. The content is a
/// byte pattern, r plus a signed byte, zeros, or a secret key's bytes (`sk`), each padded with
/// the pattern.
fn plaintext(spec: &[u8], sk: Option<[u8; 32]>) -> Vec<u8> {
    let (size, param) = (byte(spec, 1), byte(spec, 3));
    let len = match byte(spec, 0) % 3 {
        0 => 0,
        1 => 16 * usize::from(size % 7),
        _ => 16 * usize::from(size % 6) + 1 + usize::from((size >> 3) % 15),
    };
    let mut out: Vec<u8> =
        (0..len).map(|i| (i as u8).wrapping_mul(29).wrapping_add(param)).collect();
    let head = match byte(spec, 2) % 4 {
        0 => None,
        1 => {
            let mut x = R;
            add(&mut x, i16::from(param as i8));
            Some(x)
        }
        2 => Some([0; 32]),
        _ => sk,
    };
    if let Some(head) = head {
        let n = len.min(32);
        out[..n].copy_from_slice(&head[..n]);
    }
    out
}

/// A container's bytes mutated by `class` with two parameter bytes: as it is, truncated, a
/// byte flipped, or one byte appended. Returns whether to decode under the other flag.
fn mutate(bytes: &mut Vec<u8>, class: u8, a: u8, b: u8) -> bool {
    match class % 6 {
        2 => bytes.truncate(bytes.len().saturating_sub(1 + usize::from(a % 16))),
        3 if !bytes.is_empty() => {
            let at = (usize::from(a) * 7 + usize::from(b)) % bytes.len();
            bytes[at] ^= b | 1;
        }
        4 => return true,
        5 => bytes.push(a),
        _ => {}
    }
    false
}

impl Script {
    fn sk(&self, i: usize) -> Option<&BlsSkValue> {
        match self.obj(i) {
            Obj::Sk(v) => Some(v),
            _ => None,
        }
    }

    fn pk(&self, i: usize) -> Option<&BlsPkValue> {
        match self.obj(i) {
            Obj::Pk(v) => Some(v),
            _ => None,
        }
    }

    fn sig(&self, i: usize) -> Option<&BlsSigValue> {
        match self.obj(i) {
            Obj::Sig(v) => Some(v),
            _ => None,
        }
    }

    fn sks(&self, items: &[usize]) -> Vec<&BlsSkValue> {
        items.iter().filter_map(|&i| self.sk(i)).collect()
    }

    fn pks(&self, items: &[usize]) -> Vec<&BlsPkValue> {
        items.iter().filter_map(|&i| self.pk(i)).collect()
    }

    fn sigs(&self, items: &[usize]) -> Vec<&BlsSigValue> {
        items.iter().filter_map(|&i| self.sig(i)).collect()
    }

    /// `n` registers of `kind` picked by `picks`; `None` when there is none to pick.
    fn picks(&self, kind: BlsKind, n: usize, picks: &[u8]) -> Option<Vec<usize>> {
        (0..n).map(|i| self.pick(kind, byte(picks, i))).collect()
    }

    /// A list operand from `rest`: the registers, and the bytes read.
    fn list(&self, kind: BlsKind, rest: &[u8]) -> (Option<Vec<usize>>, usize) {
        let n = count(byte(rest, 0));
        (self.picks(kind, n, rest.get(1..).unwrap_or_default()), 1 + n)
    }

    /// An in-place op's receiver, a register of `kind` picked by `pick` when `alt` (bit 7):
    /// `Some(None)` for none, `None` when there is no register to pick.
    fn receiver(&self, alt: bool, kind: BlsKind, pick: u8) -> Option<Option<usize>> {
        if alt {
            self.pick(kind, pick).map(Some)
        } else {
            Some(None)
        }
    }

    /// A hash operand: an earlier `SIGN`'s hash when the class byte's low bit is set and
    /// there is one, else the 32 bytes after it.
    fn hash(&self, rest: &[u8]) -> [u8; 32] {
        let class = byte(rest, 0);
        if class & 1 == 1 && !self.signed.is_empty() {
            self.signed[usize::from(class >> 1) % self.signed.len()]
        } else {
            take(rest, 1)
        }
    }

    /// An id operand, a `CBLSId`'s 32 stored bytes, by the class byte modulo 8: 0, r, r + k,
    /// a small k, 2⁵⁶·k, the 32 bytes as they are, an earlier id, or an earlier id ± r (≡ it
    /// modulo r). k is the first of the 32 bytes and picks the earlier id; with no earlier id
    /// the classes that need one take the bytes as they are.
    pub(crate) fn id(&mut self, rest: &[u8]) -> [u8; 32] {
        let class = byte(rest, 0) % 8;
        let k = byte(rest, 1);
        let earlier =
            (!self.ext.ids.is_empty()).then(|| self.ext.ids[usize::from(k) % self.ext.ids.len()]);
        let small = |at: usize| {
            let mut x = [0u8; 32];
            x[at] = k;
            x
        };
        let id = match (class, earlier) {
            (0, _) => [0; 32],
            (1, _) => R,
            (2, _) => add256(&R, &small(31), false).0,
            (3, _) => small(31),
            (4, _) => small(24),
            (6, Some(e)) => e,
            (7, Some(e)) => match add256(&e, &R, false) {
                (sum, false) => sum,
                _ => add256(&e, &R, true).0,
            },
            _ => take(rest, 1),
        };
        self.ext.ids.push(id);
        id
    }

    /// A destination op on `recv` (in place) or none, whose Core call returned `ok` with the
    /// result `made`; `rest` are the operands after the receiver.
    fn stores_in_place(
        &mut self,
        op: u8,
        flags: u8,
        recv: Option<usize>,
        rest: &[u8],
        made: Obj,
        ok: bool,
    ) {
        // Core reports `OK` iff the result is valid and the call returned true, so the port's
        // `bool` must be the result's validity.
        if ok != made.is_valid() {
            self.diverge("return value");
        }
        let mut operands: Vec<u8> = recv.map(index).into_iter().collect();
        operands.extend(rest);
        let flags = if recv.is_some() {
            flags | ORACLE_BLS_FLAG_INPLACE
        } else {
            flags
        };
        self.stores(op, flags, &operands, made);
    }

    /// A verification with the verdict `v`. `dash_pkc` gives a verdict on every operand
    /// (BS11a-4 D2), so `Unsupported` is a divergence.
    fn verifies(&mut self, op: u8, flags: u8, operands: &[u8], v: BlsVerdict) {
        let status = match v {
            BlsVerdict::Valid => ORACLE_OK,
            BlsVerdict::Invalid => ORACLE_REJECT,
            BlsVerdict::Unsupported => {
                self.diverge("unsupported");
                ORACLE_REJECT
            }
        };
        self.emit(
            op,
            flags,
            operands,
            None,
            Expected {
                status,
                output: Vec::new(),
            },
        );
    }

    /// One op of a target's table with its selector bits, its operands read from `rest`.
    /// Returns the bytes read.
    fn wrapper_op(&mut self, op: WrapperOp, sel: Sel, rest: &[u8]) -> usize {
        let flags = legacy_flag(sel.legacy);
        let at = |i: usize| byte(rest, i);
        let after = |i: usize| rest.get(i..).unwrap_or_default();
        let sig = sel.group == BlsKind::Signature;
        match op {
            Base(kind) => self.run(kind, sel, rest),
            AggSk => {
                let (items, used) = self.list(BlsKind::SecretKey, rest);
                if let Some(items) = items {
                    let made = Obj::Sk(aggregate_secret_keys_insecure(&self.sks(&items)));
                    self.note(&items);
                    self.stores(ORACLE_BLS_OP_AGG_SK, flags, &list_operand(&items), made);
                }
                used
            }
            AggGroup => {
                let n = if sel.alt {
                    2
                } else {
                    count(at(0))
                };
                if let Some(items) = self.picks(sel.group, n, after(1)) {
                    self.agg_core(sel.group, &items, sel.alt, flags);
                }
                1 + n
            }
            SubSig => {
                let (a, b) =
                    (self.pick(BlsKind::Signature, at(0)), self.pick(BlsKind::Signature, at(1)));
                if let (Some(a), Some(b)) = (a, b) {
                    self.note(&[a, b]);
                    let operands = [index(a), index(b)];
                    match (self.sig(a), self.sig(b)) {
                        (Some(x), Some(y)) if x.is_valid() && y.is_valid() => {
                            let made = Obj::Sig(sub_signatures_insecure(x, y));
                            self.stores(ORACLE_BLS_OP_SUB_SIG, flags, &operands, made);
                        }
                        _ => self.fails(ORACLE_BLS_OP_SUB_SIG, flags, &operands, ORACLE_E_ARG),
                    }
                }
                2
            }
            AggSecure => {
                let (sigs, n) = self.list(BlsKind::Signature, rest);
                let m = second_len(sig, n, at(n));
                let pks = self.picks(BlsKind::PublicKey, m, after(n + 1));
                let h = self.hash(after(n + 1 + m));
                if let (Some(sigs), Some(pks)) = (sigs, pks) {
                    self.agg_secure(&sigs, &pks, &h, flags);
                }
                n + 1 + m + H
            }
            VerifySec => {
                let s = self.pick(BlsKind::Signature, at(0));
                let (pks, n) = self.list(BlsKind::PublicKey, after(1));
                let h = self.hash(after(1 + n));
                if let (Some(s), Some(pks)) = (s, pks) {
                    let v = match self.sig(s) {
                        Some(x) => {
                            verify_secure_aggregated(x, &self.pks(&pks), &h, scheme(sel.legacy))
                        }
                        None => BlsVerdict::Invalid,
                    };
                    let mut operands = vec![index(s)];
                    operands.extend(list_operand(&pks));
                    operands.extend(h);
                    self.note(&[&[s][..], &pks].concat());
                    self.verifies(ORACLE_BLS_OP_VERIFY_SEC, flags, &operands, v);
                }
                1 + n + H
            }
            Keep => {
                let kind = if sel.alt && sig {
                    BlsKind::SecretKey
                } else {
                    sel.group
                };
                if let Some(recv) = self.pick(kind, at(0)) {
                    match kind {
                        BlsKind::SecretKey => self.sk_share(Some(recv), &[], &[0; 32], flags),
                        BlsKind::PublicKey => self.pk_share(Some(recv), &[], &[0; 32], flags),
                        BlsKind::Signature => self.recover(Some(recv), &[], &[], flags),
                    }
                }
                1
            }
            SkShare | PkShare => {
                let kind = if op == SkShare {
                    BlsKind::SecretKey
                } else {
                    BlsKind::PublicKey
                };
                let recv = self.receiver(sel.alt, kind, at(0));
                let (items, n) = self.list(kind, after(1));
                let id = self.id(after(1 + n));
                if let (Some(recv), Some(items)) = (recv, items) {
                    if op == SkShare {
                        self.sk_share(recv, &items, &id, flags);
                    } else {
                        self.pk_share(recv, &items, &id, flags);
                    }
                }
                1 + n + H
            }
            Recover => {
                let recv = self.receiver(sel.alt, BlsKind::Signature, at(0));
                let (sigs, n) = self.list(BlsKind::Signature, after(1));
                let m = second_len(sig, n, at(1 + n));
                let ids: Vec<[u8; 32]> = (0..m).map(|j| self.id(after(2 + n + H * j))).collect();
                if let (Some(recv), Some(sigs)) = (recv, sigs) {
                    self.recover(recv, &sigs, &ids, flags);
                }
                2 + n + H * m
            }
            Verify => {
                let (s, p) =
                    (self.pick(BlsKind::Signature, at(0)), self.pick(BlsKind::PublicKey, at(1)));
                let h = self.hash(after(2));
                if let (Some(s), Some(p)) = (s, p) {
                    let v = match (self.sig(s), self.pk(p)) {
                        (Some(x), Some(y)) => verify_insecure(x, y, &h, scheme(sel.legacy)),
                        _ => BlsVerdict::Invalid,
                    };
                    let operands = [&[index(s), index(p)][..], &h].concat();
                    self.note(&[s, p]);
                    self.verifies(ORACLE_BLS_OP_VERIFY, flags, &operands, v);
                }
                2 + H
            }
            VerifyAgg => {
                let s = self.pick(BlsKind::Signature, at(0));
                let (pks, n) = self.list(BlsKind::PublicKey, after(1));
                let m = second_len(sig, n, at(1 + n));
                let hs: Vec<[u8; 32]> = (0..m).map(|j| self.hash(after(2 + n + H * j))).collect();
                if let (Some(s), Some(pks)) = (s, pks) {
                    self.verify_agg(s, &pks, &hs, flags);
                }
                2 + n + H * m
            }
            Dh => {
                let sk = self.pick(BlsKind::SecretKey, at(0));
                let pk = self.pick(BlsKind::PublicKey, at(1));
                let recv = self.receiver(sel.alt, BlsKind::PublicKey, at(2));
                if let (Some(sk), Some(pk), Some(recv)) = (sk, pk, recv) {
                    self.dh_core(sk, pk, recv, flags);
                }
                3
            }
            IesEncrypt => {
                let eph = self.pick(BlsKind::SecretKey, at(0));
                let seed: [u8; 32] = take(rest, 1);
                let (pks, n) = self.list(BlsKind::PublicKey, after(33));
                let m = second_len(sig, n, at(33 + n));
                let specs = after(34 + n);
                if let (Some(eph), Some(pks)) = (eph, pks) {
                    let plaintexts: Vec<Vec<u8>> = (0..m)
                        .map(|j| {
                            let spec = specs.get(PLAINTEXT * j..).unwrap_or_default();
                            let sk = self
                                .pick(BlsKind::SecretKey, byte(spec, 3))
                                .and_then(|i| self.sk(i))
                                .map(|k| k.to_bytes(BlsSchemeKind::Basic));
                            plaintext(spec, sk)
                        })
                        .collect();
                    self.encrypt(eph, &seed, &pks, &plaintexts, sel.legacy);
                }
                34 + n + PLAINTEXT * m
            }
            IesDecrypt | IesDecryptBlob => {
                let blob = op == IesDecryptBlob;
                let sk = self.pick(BlsKind::SecretKey, at(1));
                let c = usize::from(at(2)).checked_rem(self.ext.containers.len());
                if let (Some(sk), Some(c)) = (sk, c) {
                    self.decrypt(c, blob, sk, rest);
                }
                7
            }
        }
    }

    /// `AGG_PK`/`AGG_SIG` through `dash_pkc`'s aggregates: the span, invalid when empty, or
    /// the pair form, which Core refuses (`ORACLE_E_ARG`) when a side is invalid.
    fn agg_core(&mut self, group: BlsKind, items: &[usize], pair: bool, flags: u8) {
        let valid = items.iter().all(|&i| self.obj(i).is_valid());
        let (op, made) = if group == BlsKind::PublicKey {
            let pks = self.pks(items);
            let made = match (pair, pks.as_slice()) {
                (false, _) => Some(aggregate_public_keys_insecure(&pks)),
                (true, [a, b]) if valid => Some(aggregate_public_keys_insecure_pair(a, b)),
                _ => None,
            };
            (ORACLE_BLS_OP_AGG_PK, made.map(Obj::Pk))
        } else {
            let sigs = self.sigs(items);
            let made = match (pair, sigs.as_slice()) {
                (false, _) => Some(aggregate_signatures_insecure(&sigs)),
                (true, [a, b]) if valid => Some(aggregate_signatures_insecure_pair(a, b)),
                _ => None,
            };
            (ORACLE_BLS_OP_AGG_SIG, made.map(Obj::Sig))
        };
        let flags = if pair {
            flags | ORACLE_BLS_FLAG_PAIR
        } else {
            flags
        };
        self.note(items);
        let operands = list_operand(items);
        match made {
            Some(made) => self.stores(op, flags, &operands, made),
            None => self.fails(op, flags, &operands, ORACLE_E_ARG),
        }
    }

    /// `VERIFY_AGG`: Core answers false for an invalid signature before
    /// `VerifyInsecureAggregated` asserts non-empty lists of equal length (`bls.cpp:319`),
    /// which the shim refuses (`ORACLE_E_ARG`).
    fn verify_agg(&mut self, s: usize, pks: &[usize], hs: &[[u8; 32]], flags: u8) {
        let mut operands = vec![index(s)];
        operands.extend(list_operand(pks));
        operands.push(index(hs.len()));
        operands.extend(hs.iter().flatten());
        self.note(&[&[s][..], pks].concat());
        self.pending.unequal = !pks.is_empty() && pks.len() != hs.len();
        let valid = self.sig(s).is_some_and(BlsSigValue::is_valid);
        if valid && (pks.is_empty() || pks.len() != hs.len()) {
            let expected = Expected {
                status: ORACLE_E_ARG,
                output: Vec::new(),
            };
            self.emit(ORACLE_BLS_OP_VERIFY_AGG, flags, &operands, None, expected);
            return;
        }
        let v = match self.sig(s) {
            Some(x) => verify_insecure_aggregated(
                x,
                &self.pks(pks),
                hs,
                scheme(flags & ORACLE_BLS_FLAG_LEGACY != 0),
            ),
            None => BlsVerdict::Invalid,
        };
        self.verifies(ORACLE_BLS_OP_VERIFY_AGG, flags, &operands, v);
    }

    /// `AGG_SECURE`, unless it falls in D8's unstable-sort class.
    fn agg_secure(&mut self, sigs: &[usize], pks: &[usize], h: &[u8; 32], flags: u8) {
        let flag = scheme(flags & ORACLE_BLS_FLAG_LEGACY != 0);
        let (s, p) = (self.sigs(sigs), self.pks(pks));
        if unstable_sort(&s, &p, flag) {
            return;
        }
        let made = Obj::Sig(aggregate_signatures_secure(&s, &p, flag));
        let mut operands = list_operand(sigs);
        operands.extend(list_operand(pks));
        operands.extend(h);
        self.note(&[sigs, pks].concat());
        self.stores(ORACLE_BLS_OP_AGG_SECURE, flags, &operands, made);
    }

    /// `SK_SHARE` on a fresh key, or on a copy of `recv`.
    fn sk_share(&mut self, recv: Option<usize>, msk: &[usize], id: &[u8; 32], flags: u8) {
        let mut out = recv.and_then(|r| self.sk(r)).cloned().unwrap_or_else(BlsSkValue::invalid);
        let ok = secret_key_share(&mut out, &self.sks(msk), id);
        let mut rest = list_operand(msk);
        rest.extend(id);
        self.note(&read_regs(recv, msk));
        self.stores_in_place(ORACLE_BLS_OP_SK_SHARE, flags, recv, &rest, Obj::Sk(out), ok);
    }

    /// `PK_SHARE` on a fresh key, or on a copy of `recv`.
    fn pk_share(&mut self, recv: Option<usize>, mpk: &[usize], id: &[u8; 32], flags: u8) {
        let mut out = recv.and_then(|r| self.pk(r)).cloned().unwrap_or_else(BlsPkValue::invalid);
        let ok = public_key_share(&mut out, &self.pks(mpk), id);
        let mut rest = list_operand(mpk);
        rest.extend(id);
        self.note(&read_regs(recv, mpk));
        self.stores_in_place(ORACLE_BLS_OP_PK_SHARE, flags, recv, &rest, Obj::Pk(out), ok);
    }

    /// `RECOVER_SIG` on a fresh signature, or on a copy of `recv`.
    fn recover(&mut self, recv: Option<usize>, sigs: &[usize], ids: &[[u8; 32]], flags: u8) {
        let mut out = recv.and_then(|r| self.sig(r)).cloned().unwrap_or_else(BlsSigValue::invalid);
        let ok = recover_signature(&mut out, &self.sigs(sigs), ids);
        let mut rest = list_operand(sigs);
        rest.push(index(ids.len()));
        rest.extend(ids.iter().flatten());
        self.note(&read_regs(recv, sigs));
        self.stores_in_place(ORACLE_BLS_OP_RECOVER_SIG, flags, recv, &rest, Obj::Sig(out), ok);
    }

    /// `DH` through `dh_key_exchange` on a fresh key, or on a copy of `recv`, then a `SER` of
    /// its result under the basic flag, against which the node's IES key is checked.
    fn dh_core(&mut self, sk: usize, pk: usize, recv: Option<usize>, flags: u8) {
        let (Some(k), Some(p)) = (self.sk(sk), self.pk(pk)) else {
            return;
        };
        let key = ies_key(k, p).map(|key| *key);
        let mut out = recv.and_then(|r| self.pk(r)).cloned().unwrap_or_else(BlsPkValue::invalid);
        let ok = dh_key_exchange(&mut out, k, p);
        self.note(&read_regs(recv, &[sk, pk]));
        let rest = [index(sk), index(pk)];
        self.stores_in_place(ORACLE_BLS_OP_DH, flags, recv, &rest, Obj::Pk(out), ok);
        self.ies.push((self.expected.len(), key));
        self.ser(self.regs.len() - 1, false, false);
    }

    /// `IES_ENCRYPT`: Core refuses unequal lists and an invalid or zero ephemeral key
    /// (`ORACLE_E_ARG`); otherwise its container, kept for the decryptions.
    fn encrypt(
        &mut self,
        eph: usize,
        seed: &[u8; 32],
        pks: &[usize],
        plaintexts: &[Vec<u8>],
        legacy: bool,
    ) {
        let flags = legacy_flag(legacy);
        let mut operands = vec![index(eph)];
        operands.extend(seed);
        operands.extend(list_operand(pks));
        compact(plaintexts.len(), &mut operands);
        for p in plaintexts {
            compact(p.len(), &mut operands);
            operands.extend(p);
        }
        let Some(k) = self.sk(eph) else {
            return;
        };
        let zero = k.to_bytes(BlsSchemeKind::Basic) == [0; BLS_SK_LEN];
        let (expected, container) = if pks.len() != plaintexts.len() || !k.is_valid() || zero {
            (
                Expected {
                    status: ORACLE_E_ARG,
                    output: Vec::new(),
                },
                None,
            )
        } else {
            let recipients: Vec<(&BlsPkValue, &[u8])> =
                self.pks(pks).into_iter().zip(plaintexts.iter().map(Vec::as_slice)).collect();
            let (container, ok) = BlsIesMultiBlobs::encrypt_with(k, seed, &recipients);
            let expected = Expected {
                status: if ok {
                    ORACLE_OK
                } else {
                    ORACLE_REJECT
                },
                output: container.to_bytes(scheme(legacy)),
            };
            (expected, Some(container))
        };
        self.ext.containers.extend(container.map(|c| (c, legacy)));
        self.note(&[&[eph][..], pks].concat());
        self.emit(ORACLE_BLS_OP_IES_ENCRYPT, flags, &operands, None, expected);
    }

    /// The bytes a decryption reads from container `c` (operands: blob pick, mutation class,
    /// two parameters), as a whole or its blob, the flag to read them under (the container's,
    /// or the other one) and the blob picked.
    fn container(&self, c: usize, blob: bool, ops: &[u8]) -> (Vec<u8>, bool, usize) {
        let (container, legacy) = &self.ext.containers[c];
        let n = container.blobs.len();
        let j = usize::from(byte(ops, 0)) % n.max(1);
        let mut bytes = if blob {
            container
                .get(j)
                .unwrap_or_else(|| BlsIesBlobValue {
                    ephemeral: container.ephemeral.clone(),
                    iv_seed: container.iv_seed,
                    data: Vec::new(),
                })
                .to_bytes(scheme(*legacy))
        } else {
            container.to_bytes(scheme(*legacy))
        };
        let other = mutate(&mut bytes, byte(ops, 1), byte(ops, 2), byte(ops, 3));
        (bytes, *legacy != other, j)
    }

    /// `IES_DECRYPT` or `IES_DECRYPT_BLOB` (operands: index class, secret key, container,
    /// blob, mutation class, two parameters) of container `c`, then, when it succeeds, a
    /// `DECODE` of the plaintext as a secret key under each flag.
    fn decrypt(&mut self, c: usize, blob: bool, sk: usize, rest: &[u8]) {
        let (bytes, legacy, j) = self.container(c, blob, rest.get(3..).unwrap_or_default());
        // Mostly the blob's own index; otherwise another, or one past every blob.
        let class = byte(rest, 0);
        let idx: u32 = match class {
            0..0xc0 => j as u32,
            0xff if !blob => u32::MAX,
            _ => u32::from(class % BLOB_IDX),
        };
        let flags = legacy_flag(legacy);
        let flag = scheme(legacy);
        let mut operands = idx.to_le_bytes().to_vec();
        operands.push(index(sk));
        compact(bytes.len(), &mut operands);
        operands.extend(&bytes);
        let op = if blob {
            ORACLE_BLS_OP_IES_DECRYPT_BLOB
        } else {
            ORACLE_BLS_OP_IES_DECRYPT
        };
        let Some(k) = self.sk(sk) else {
            return;
        };
        let idx = idx as usize;
        let mut data = bytes.as_slice();
        // The plaintext and the secret key it holds under each flag.
        let decoded = if blob {
            BlsIesBlobValue::decode(&mut data, flag).map(|b| {
                let keys = [BlsSchemeKind::Basic, BlsSchemeKind::Legacy]
                    .map(|f| b.decrypt_secret_key(idx, k, f));
                (b.decrypt(idx, k).map(|p| p.to_vec()), keys)
            })
        } else {
            BlsIesMultiBlobs::decode(&mut data, flag).map(|b| {
                let keys = [BlsSchemeKind::Basic, BlsSchemeKind::Legacy]
                    .map(|f| b.decrypt_secret_key(idx, k, f));
                (b.decrypt(idx, k).map(|p| p.to_vec()), keys)
            })
        };
        let (status, plain, keys) = match decoded {
            Err(_) => (ORACLE_E_DECODE, None, None),
            Ok(_) if !data.is_empty() => (ORACLE_E_TRAILING, None, None),
            Ok((None, _)) => (ORACLE_REJECT, None, None),
            Ok((Some(p), keys)) => (ORACLE_OK, Some(p), Some(keys)),
        };
        self.note(&[sk]);
        self.emit(
            op,
            flags,
            &operands,
            None,
            Expected {
                status,
                output: plain.clone().unwrap_or_default(),
            },
        );
        let (Some(plain), Some(keys)) = (plain, keys) else {
            return;
        };
        let head = &plain[..plain.len().min(BLS_SK_LEN)];
        for (legacy, key) in [false, true].into_iter().zip(keys) {
            let mut operands =
                vec![BlsKind::SecretKey as u8, BlsMode::Eager as u8, index(head.len())];
            operands.extend(head);
            let flags = legacy_flag(legacy);
            self.pending.plaintext = true;
            match key {
                Some(key) => self.stores(ORACLE_BLS_OP_DECODE, flags, &operands, Obj::Sk(key)),
                None => self.fails(ORACLE_BLS_OP_DECODE, flags, &operands, ORACLE_E_DECODE),
            }
        }
    }
}

/// D8's class: more than 16 signers, where libstdc++'s `std::sort` stops being stable, and two
/// equal keys under `flag` with different signatures.
fn unstable_sort(sigs: &[&BlsSigValue], pks: &[&BlsPkValue], flag: BlsSchemeKind) -> bool {
    if pks.len() <= 16 || sigs.len() != pks.len() {
        return false;
    }
    let keys: Vec<_> = pks.iter().map(|pk| BlsPkElem::from_impl(pk).to_bytes(flag)).collect();
    (0..keys.len())
        .any(|i| (i + 1..keys.len()).any(|j| keys[i] == keys[j] && !sigs[i].core_eq(sigs[j])))
}

/// The script `bytes` describe over a target's `table` (module documentation).
pub fn read(table: &[WrapperOp], bytes: &[u8]) -> Script {
    // Struct-update syntax cannot name `Script`'s private fields from here.
    let mut script = Script::default();
    script.value_hash = true;
    read_with(script, bytes, |script, selector, rest| {
        let op = table[usize::from(selector & 0x1f) % table.len()];
        script.wrapper_op(op, Sel::read(selector, 0x20, 0x40, 0x80), rest)
    })
}

/// `diff_bls_aggregate`.
pub fn compare_bls_aggregate(bytes: &[u8], pin: Pin) -> Compared {
    compare_script(&read(&AGGREGATE_OPS, bytes), pin)
}

/// `diff_bls_threshold`.
pub fn compare_bls_threshold(bytes: &[u8], pin: Pin) -> Compared {
    compare_script(&read(&THRESHOLD_OPS, bytes), pin)
}

/// `diff_bls_ies`.
pub fn compare_bls_ies(bytes: &[u8], pin: Pin) -> Compared {
    compare_script(&read(&IES_OPS, bytes), pin)
}

/// `diff_bls_verify`.
pub fn compare_bls_verify(bytes: &[u8], pin: Pin) -> Compared {
    compare_script(&read(&VERIFY_OPS, bytes), pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_hit_their_classes() {
        let mut script = Script::default();
        let with = |class: u8, k: u8| {
            let mut rest = vec![class, k];
            rest.extend([0x5a; 31]);
            rest
        };
        assert_eq!(script.id(&with(0, 9)), [0; 32]);
        assert_eq!(script.id(&with(1, 9)), R);
        let mut r9 = R;
        add(&mut r9, 9);
        assert_eq!(script.id(&with(2, 9)), r9);
        assert_eq!(script.id(&with(3, 9))[31], 9);
        assert_eq!(script.id(&with(4, 9))[24], 9);
        // An earlier id plus r: id 3 (9) plus r is id 2.
        assert_eq!(script.id(&with(7, 3)), r9);
        // Less r where the sum overflows.
        let mut top = vec![5, 0xff];
        top.extend([0xff; 31]);
        let max = script.id(&top);
        assert_eq!(script.id(&with(7, 6)), add256(&max, &R, true).0);
        assert_eq!(script.id(&with(6, 1)), R);
    }

    #[test]
    fn plaintexts_have_their_lengths() {
        assert!(plaintext(&[0, 5, 0, 0], None).is_empty());
        assert_eq!(plaintext(&[1, 6, 0, 0], None).len(), 96);
        assert_eq!(plaintext(&[2, 0, 0, 0], None).len(), 1);
        assert_eq!(plaintext(&[2, 113, 0, 0], None).len(), 95);
        assert_eq!(plaintext(&[1, 2, 1, 0], None)[..32], R);
    }

    #[test]
    fn selectors_round_trip() {
        for table in [&AGGREGATE_OPS[..], &THRESHOLD_OPS, &IES_OPS, &VERIFY_OPS] {
            assert!(table.len() <= 32);
            for &op in table {
                let s = selector(table, op, true, true, true).unwrap_or_default();
                assert_eq!(table[usize::from(s & 0x1f) % table.len()], op);
                assert_eq!(s & 0xe0, 0xe0);
            }
        }
    }
}

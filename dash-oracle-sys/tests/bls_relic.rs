//! The ABI 26 additions to `oracle_bls_exec` (BS11a §7): the element ops, the PAIR and
//! INPLACE forms, relic's code field, the call log and its injection selftest (D59), and
//! the design's relic probes P1–P5 through Core's wrappers.
//!
//! Expected values are group-law identities on subgroup points, outcomes Core's wrapper
//! code defines (cited per check), or the design's probe results (§2.2, §8's fixed
//! vectors).

use std::collections::BTreeSet;
use std::ffi::{c_int, c_void};
use std::ptr::{null, null_mut};

use bitcoin_hashes::{Hash, sha256d};
use dash_oracle_sys::ffi::{
    ABI_VERSION, ORACLE_BLS_FLAG_INPLACE as INPLACE, ORACLE_BLS_FLAG_LEGACY as LEGACY,
    ORACLE_BLS_FLAG_LOG as LOG, ORACLE_BLS_FLAG_PAIR as PAIR, ORACLE_BLS_OP_ADD as ADD,
    ORACLE_BLS_OP_AGG_PK as AGG_PK, ORACLE_BLS_OP_AGG_SIG as AGG_SIG,
    ORACLE_BLS_OP_AGG_SK as AGG_SK, ORACLE_BLS_OP_DECODE as DECODE, ORACLE_BLS_OP_DH as DH,
    ORACLE_BLS_OP_EQ as EQ, ORACLE_BLS_OP_IMPL_EQ as IMPL_EQ, ORACLE_BLS_OP_ISVALID as ISVALID,
    ORACLE_BLS_OP_MUL as MUL, ORACLE_BLS_OP_NEG as NEG, ORACLE_BLS_OP_PUBKEY as PUBKEY,
    ORACLE_BLS_OP_SER as SER, ORACLE_BLS_OP_SER_HASH as SER_HASH, ORACLE_BLS_OP_SIGN as SIGN,
    ORACLE_BLS_OP_SK_SHARE as SK_SHARE, ORACLE_BLS_OP_VERIFY as VERIFY,
    ORACLE_BLS_OP_VERIFY_SEC as VERIFY_SEC, ORACLE_E_ARG, ORACLE_E_EXCEPTION, ORACLE_OK,
    ORACLE_REJECT, v24_0,
};
use dash_oracle_sys::{
    BlsKind, BlsMode, BlsRecord, Chain, Checkpoints, CoinCodec, CoinsView, Context, ErrorKind,
    ListEncoding, MnList, Oracle, P2pStream, Pin, RelicCall, Result,
};

/// relic's `RLC_ERR`, the code the selftest injects.
const RLC_ERR: i32 = 1;
const MSG: [u8; 32] = [0x11; 32];
/// The design's probe scalar (§2.2 P2, §8's fixed vectors): 3a5a…5a.
const K: [u8; 32] = {
    let mut k = [0x5a; 32];
    k[0] = 0x3a;
    k
};

/// An `oracle_bls_exec` script; each op returns the index of its record.
#[derive(Clone, Default)]
struct Script {
    bytes: Vec<u8>,
    ops: usize,
}

impl Script {
    fn op(&mut self, op: u8, flags: u8, operands: &[&[u8]]) -> usize {
        self.bytes.extend([op, flags]);
        for operand in operands {
            self.bytes.extend_from_slice(operand);
        }
        self.ops += 1;
        self.ops - 1
    }

    fn decode(&mut self, flags: u8, kind: BlsKind, bytes: &[u8], dst: u8) -> usize {
        let head = [kind as u8, BlsMode::Eager as u8, bytes.len() as u8];
        self.op(DECODE, flags, &[&head, bytes, &[dst]])
    }

    /// Op i writes register i: r0 = sk 1, r1 = G1's generator, r2 = H(MSG), r3 = sk 2,
    /// r4 = 2·G1, r5 = 2·H(MSG), r6 = an invalid public key, r7 = an invalid signature.
    fn keys(flags: u8) -> Self {
        let mut s = Self::default();
        s.decode(flags, BlsKind::SecretKey, &scalar(1), 0);
        s.op(PUBKEY, flags, &[&[0, 1]]);
        s.op(SIGN, flags, &[&[0], &MSG, &[2]]);
        s.decode(flags, BlsKind::SecretKey, &scalar(2), 3);
        s.op(PUBKEY, flags, &[&[3, 4]]);
        s.op(SIGN, flags, &[&[3], &MSG, &[5]]);
        s.decode(flags, BlsKind::PublicKey, &[0; 48], 6);
        s.decode(flags, BlsKind::Signature, &[0; 96], 7);
        s
    }
}

fn scalar(k: u64) -> [u8; 32] {
    let mut out = [0; 32];
    out[24..].copy_from_slice(&k.to_be_bytes());
    out
}

fn list(regs: &[u8]) -> Vec<u8> {
    [&[regs.len() as u8][..], regs].concat()
}

fn run(oracle: &Oracle, script: &Script) -> Vec<BlsRecord> {
    let records = oracle.bls_exec(&script.bytes).unwrap();
    assert_eq!(records.len(), script.ops, "{}", oracle.pin());
    records
}

/// Every log entry and every post-op code is `RLC_OK` (D8's claim; these scripts call no
/// decode symbol inside a window).
fn assert_no_code(oracle: &Oracle, records: &[BlsRecord]) {
    for (i, r) in records.iter().enumerate() {
        assert_eq!(r.relic_code, 0, "{} op {i}", oracle.pin());
        for call in r.log.iter().flatten() {
            assert_eq!(call.code, 0, "{} op {i}: {}", oracle.pin(), call.symbol);
        }
    }
}

fn flat_log(records: &[BlsRecord]) -> Vec<&RelicCall> {
    records.iter().flat_map(|r| r.log.iter().flatten()).collect()
}

/// The shim refuses the script as malformed.
fn refused(oracle: &Oracle, script: &Script) {
    let err = oracle.bls_exec(&script.bytes).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument, "{}: {err:?}", oracle.pin());
}

/// A SER_HASH output's digest is sha256d of its serialization.
fn hash_matches(record: &BlsRecord) -> bool {
    let (ser, hash) = record.output.split_at(record.output.len() - 32);
    hash == sha256d::Hash::hash(ser).to_byte_array()
}

/// The ops of one base point in [`element_ops`].
struct Case {
    b2: usize,
    write: &'static str,
    neg: usize,
    inf: usize,
    zero: usize,
    two: usize,
    dbl_eq: usize,
    one_eq: usize,
    one_impl_eq: usize,
    valid: usize,
    inf_valid: usize,
    neg_eq: usize,
    hashes: [usize; 3],
}

fn count(record: &BlsRecord, symbol: &str) -> usize {
    record.log.as_ref().unwrap().iter().filter(|c| c.symbol == symbol).count()
}

#[test]
fn element_ops() {
    for oracle in Oracle::all() {
        for flags in [0, LEGACY] {
            let f = flags | LOG;
            let mut s = Script::keys(flags);
            let mut cases = Vec::new();
            for (b, b2, write) in [(1u8, 4usize, "ep_write_bin"), (2, 5, "ep2_write_bin")] {
                let neg = s.op(NEG, f, &[&[b, 10]]);
                let inf = s.op(ADD, f, &[&[b, 10, 11]]);
                let zero = s.op(MUL, f, &[&[b], &scalar(0), &[12]]);
                let two = s.op(MUL, f, &[&[b], &scalar(2), &[13]]);
                s.op(ADD, f, &[&[b, b, 14]]);
                let dbl_eq = s.op(IMPL_EQ, f, &[&[13, 14]]);
                s.op(MUL, f, &[&[b], &scalar(1), &[15]]);
                let one_eq = s.op(EQ, f, &[&[15, b]]);
                let one_impl_eq = s.op(IMPL_EQ, f, &[&[15, b]]);
                let valid = s.op(ISVALID, f, &[&[b]]);
                let inf_valid = s.op(ISVALID, f, &[&[11]]);
                let neg_eq = s.op(IMPL_EQ, f, &[&[10, b]]);
                // GetHash() caches in the register: uncached, cached, then after an op
                // that writes the register (F10).
                let h1 = s.op(SER_HASH, f, &[&[b]]);
                let h2 = s.op(SER_HASH, f, &[&[b]]);
                s.op(ADD, f, &[&[b, 12, b]]);
                let h3 = s.op(SER_HASH, f, &[&[b]]);
                cases.push(Case {
                    b2,
                    write,
                    neg,
                    inf,
                    zero,
                    two,
                    dbl_eq,
                    one_eq,
                    one_impl_eq,
                    valid,
                    inf_valid,
                    neg_eq,
                    hashes: [h1, h2, h3],
                });
            }
            let records = run(&oracle, &s);
            let pin = oracle.pin();
            assert_no_code(&oracle, &records);
            for c in cases {
                for i in [c.neg, c.inf, c.zero, c.two] {
                    assert_eq!(records[i].status, ORACLE_OK, "{pin} op {i}");
                }
                // [2]b = b + b = PUBKEY(sk 2) or SIGN(sk 2) of the same message.
                assert_eq!(records[c.two].output, records[c.b2].output, "{pin}");
                for i in [c.dbl_eq, c.one_eq, c.one_impl_eq] {
                    assert_eq!(records[i].output, [1], "{pin} op {i}");
                }
                // Both infinity shapes serialize alike; dashbls IsValid() holds for it.
                assert_eq!(records[c.inf].output, records[c.zero].output, "{pin}");
                assert_eq!(records[c.valid].output, [1, 1], "{pin}");
                assert_eq!(records[c.inf_valid].output, [1, 1], "{pin}");
                assert_eq!(records[c.neg_eq].output, [0], "{pin}");
                let [h1, h2, h3] = c.hashes;
                for h in c.hashes {
                    assert!(hash_matches(&records[h]), "{pin} op {h}");
                }
                // b + infinity = b, written back into b.
                assert_eq!(records[h1].output, records[h2].output, "{pin}");
                assert_eq!(records[h1].output, records[h3].output, "{pin}");
                // ToBytes, then SerializeHash's ToBytes when the cache is null.
                let writes = c.hashes.map(|h| count(&records[h], c.write));
                assert_eq!(writes, [2, 1, 2], "{pin} {}", c.write);
            }
        }
    }
}

#[test]
fn aggregate_spans_and_pairs() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for flags in [0, LEGACY] {
            let f = flags | LOG;
            let mut s = Script::keys(flags);
            let mut cases = Vec::new();
            for (op, a, b) in [(AGG_PK, 1u8, 4u8), (AGG_SIG, 2, 5)] {
                let empty = s.op(op, f, &[&list(&[]), &[20]]);
                let one = s.op(op, f, &[&list(&[a]), &[21]]);
                s.op(op, f, &[&list(&[a, b]), &[22]]);
                s.op(op, f | PAIR, &[&list(&[a, b]), &[23]]);
                s.op(MUL, f, &[&[a], &scalar(3), &[24]]);
                let span_eq = s.op(IMPL_EQ, f, &[&[22, 24]]);
                let pair_eq = s.op(IMPL_EQ, f, &[&[23, 24]]);
                let one_eq = s.op(EQ, f, &[&[21, a]]);
                cases.push([empty, one, span_eq, pair_eq, one_eq]);
            }
            let records = run(&oracle, &s);
            assert_no_code(&oracle, &records);
            for [empty, one, span_eq, pair_eq, one_eq] in cases {
                // An empty span returns the wrapper `{}`: invalid (bls.cpp:155-159).
                assert_eq!(records[empty].status, ORACLE_REJECT, "{pin}");
                assert_eq!(records[one].status, ORACLE_OK, "{pin}");
                for i in [span_eq, pair_eq, one_eq] {
                    assert_eq!(records[i].output, [1], "{pin} op {i}");
                }
            }

            // Core asserts on an invalid pair operand: ORACLE_E_ARG, d left empty.
            for (op, valid, invalid) in [(AGG_PK, 1u8, 6u8), (AGG_SIG, 2, 7)] {
                for regs in [[valid, invalid], [invalid, valid]] {
                    // d holds an object before the refused op, so its emptying shows.
                    let mut s = Script::keys(flags);
                    s.op(NEG, flags, &[&[valid, 30]]);
                    let i = s.op(op, flags | PAIR, &[&list(&regs), &[30]]);
                    assert_eq!(run(&oracle, &s)[i].status, ORACLE_E_ARG, "{pin}");
                    s.op(SER, flags, &[&[30]]);
                    refused(&oracle, &s);
                }
            }
            // PAIR takes exactly two registers, and only on ops 8 and 9.
            for (op, regs) in [(AGG_PK, &[1u8, 4, 1][..]), (AGG_PK, &[1]), (AGG_SK, &[0, 3])] {
                let mut s = Script::keys(flags);
                s.op(op, flags | PAIR, &[&list(regs), &[30]]);
                refused(&oracle, &s);
            }
        }
    }
}

#[test]
fn inplace_receivers() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for flags in [0, LEGACY] {
            let f = flags | LOG;
            let mut s = Script::keys(flags);
            // DHKeyExchange clears fValid and returns before touching impl when an operand
            // is invalid (bls.cpp:208-220): the receiver's element stays.
            let kept = s.op(DH, f | INPLACE, &[&[1, 0, 6, 30]]);
            let kept_valid = s.op(ISVALID, f, &[&[30]]);
            let kept_eq = s.op(EQ, f, &[&[30, 1]]);
            let kept_impl_eq = s.op(IMPL_EQ, f, &[&[30, 1]]);
            // A fresh receiver holds the default element, infinity.
            s.op(DH, f, &[&[0, 6, 31]]);
            let fresh_valid = s.op(ISVALID, f, &[&[31]]);
            let fresh_impl_eq = s.op(IMPL_EQ, f, &[&[31, 1]]);
            // With valid operands the receiver's element is replaced: 2·G.
            let dh = s.op(DH, f | INPLACE, &[&[1, 3, 1, 32]]);
            let dh_eq = s.op(IMPL_EQ, f, &[&[32, 4]]);
            // SecretKeyShare on a used receiver gives the fresh receiver's share.
            let id = [7; 32];
            let share = s.op(SK_SHARE, f | INPLACE, &[&[3], &list(&[0, 3]), &id, &[33]]);
            let fresh_share = s.op(SK_SHARE, f, &[&list(&[0, 3]), &id, &[34]]);
            let records = run(&oracle, &s);
            assert_no_code(&oracle, &records);
            assert_eq!(records[kept].status, ORACLE_REJECT, "{pin}");
            assert_eq!(records[kept_valid].output, [0, 1], "{pin}");
            assert_eq!(records[kept_eq].output, [0], "{pin}");
            assert_eq!(records[kept_impl_eq].output, [1], "{pin}");
            assert_eq!(records[fresh_valid].output, [0, 1], "{pin}");
            assert_eq!(records[fresh_impl_eq].output, [0], "{pin}");
            assert_eq!((records[dh].status, &records[dh_eq].output), (ORACLE_OK, &vec![1]));
            assert_eq!(records[share].status, ORACLE_OK, "{pin}");
            assert_eq!(records[share].output, records[fresh_share].output, "{pin}");

            // The receiver must hold the result's kind.
            for recv in [2u8, 0] {
                let mut s = Script::keys(flags);
                s.op(DH, flags | INPLACE, &[&[recv, 0, 1, 30]]);
                refused(&oracle, &s);
            }
            // INPLACE is refused where Core has no receiver to reuse.
            let mut s = Script::keys(flags);
            s.op(ADD, flags | INPLACE, &[&[1, 1, 1, 30]]);
            refused(&oracle, &s);
        }
    }
}

/// A receiver's cached hash does not reach the result of an op that copies it: the INPLACE
/// receiver and the PAIR form's first operand are hashed first, and the result's
/// SER_HASH still hashes its own serialization.
#[test]
fn cached_hash_does_not_carry() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for flags in [0, LEGACY] {
            let f = flags | LOG;
            let mut s = Script::keys(flags);
            let mut hashes = Vec::new();
            s.op(SER_HASH, f, &[&[1]]);
            s.op(DH, f | INPLACE, &[&[1, 0, 4, 30]]);
            hashes.push(s.op(SER_HASH, f, &[&[30]]));
            s.op(DH, f | INPLACE, &[&[1, 0, 6, 31]]);
            hashes.push(s.op(SER_HASH, f, &[&[31]]));
            s.op(SER_HASH, f, &[&[0]]);
            s.op(SK_SHARE, f | INPLACE, &[&[0], &list(&[0, 3]), &[7; 32], &[32]]);
            hashes.push(s.op(SER_HASH, f, &[&[32]]));
            for (op, a, b) in [(AGG_PK, 1u8, 4u8), (AGG_SIG, 2, 5)] {
                s.op(SER_HASH, f, &[&[a]]);
                s.op(op, f | PAIR, &[&list(&[a, b]), &[33]]);
                hashes.push(s.op(SER_HASH, f, &[&[33]]));
            }
            let records = run(&oracle, &s);
            assert_no_code(&oracle, &records);
            for h in hashes {
                assert!(hash_matches(&records[h]), "{pin} op {h}: {:?}", records[h]);
            }
        }
    }
}

#[test]
fn log_flag_and_record_format() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let mut s = Script::default();
        // DECODE runs outside the window (D3): its log is empty.
        let decode = s.decode(LOG, BlsKind::SecretKey, &scalar(1), 0);
        let plain = s.op(PUBKEY, 0, &[&[0, 1]]);
        let logged = s.op(PUBKEY, LOG, &[&[0, 2]]);
        let add = s.op(ADD, LOG, &[&[1, 2, 3]]);
        let records = run(&oracle, &s);
        assert_no_code(&oracle, &records);
        assert_eq!(records[decode].log, Some(vec![]), "{pin}");
        assert_eq!(records[plain].log, None, "{pin}");
        assert!(!records[logged].log.as_ref().unwrap().is_empty(), "{pin}");
        assert_eq!(records[plain].output, records[logged].output, "{pin}");
        assert!(count(&records[add], "ep_add_projc") >= 1, "{pin}: {:?}", records[add].log);
    }
}

/// Every op kind the selftest and the path scripts (§3.3) cover, all under LOG.
fn required_script(flags: u8) -> Script {
    let f = flags | LOG;
    // Unlogged ops count towards an injection's nth too, so every op here logs.
    let mut s = Script::keys(f);
    for (op, a, b) in [(AGG_PK, 1u8, 4u8), (AGG_SIG, 2, 5)] {
        s.op(op, f, &[&list(&[]), &[20]]);
        s.op(op, f, &[&list(&[a]), &[21]]);
        s.op(op, f, &[&list(&[a, b, a]), &[22]]);
        s.op(op, f | PAIR, &[&list(&[a, b]), &[23]]);
        s.op(SER_HASH, f, &[&[23]]);
        s.op(SER_HASH, f, &[&[23]]);
        s.op(NEG, f, &[&[a, 24]]);
        s.op(ADD, f, &[&[a, 24, 25]]);
        s.op(MUL, f, &[&[a], &K, &[26]]);
        s.op(MUL, f, &[&[a], &scalar(5), &[27]]);
        s.op(EQ, f, &[&[25, 26]]);
        s.op(IMPL_EQ, f, &[&[25, 26]]);
        s.op(ISVALID, f, &[&[26]]);
    }
    s.op(VERIFY, f, &[&[2, 1], &MSG]);
    s.op(VERIFY_SEC, f, &[&[2], &list(&[1]), &MSG]);
    s.op(DH, f, &[&[0, 4, 28]]);
    s
}

/// D59's selftest: for every symbol the required scripts log, a code set after its first
/// windowed call shows at that entry and stays until Core's clear point (C* then 0*);
/// earlier entries are untouched, and the next call starts clean.
#[test]
fn call_log_injection_selftest() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for flags in [0, LEGACY] {
            let script = required_script(flags);
            // dashbls reads two constants once per process on the first legacy hash to G2
            // (legacy.cpp's call_once), whose calls would be in only one run's log.
            run(&oracle, &script);
            let baseline = run(&oracle, &script);
            assert_no_code(&oracle, &baseline);
            let base = flat_log(&baseline);
            let symbols: BTreeSet<_> = base.iter().map(|c| c.symbol.clone()).collect();
            for must in
                ["ep_add_projc", "ep2_add_projc", "ep_write_bin", "ep2_write_bin", "fp12_cmp"]
            {
                assert!(symbols.contains(must), "{pin}: {must} not logged in {symbols:?}");
            }
            for symbol in &symbols {
                let at = base.iter().position(|c| &c.symbol == symbol).unwrap();
                let records =
                    oracle.bls_exec_injecting(&script.bytes, symbol, 1, RLC_ERR, false).unwrap();
                let log = flat_log(&records);
                assert_eq!(log[..at], base[..at], "{pin} {symbol}");
                assert_eq!((&log[at].symbol, log[at].code), (symbol, RLC_ERR), "{pin}");
                let tail: Vec<_> = log[at..].iter().map(|c| c.code).collect();
                let set = tail.iter().take_while(|&&c| c == RLC_ERR).count();
                assert!(tail[set..].iter().all(|&c| c == 0), "{pin} {symbol}: {tail:?}");
                // A throw (CheckRelicErrors on the set code) ends the script; Core reads
                // nothing after it, so bytes no op could start with change nothing.
                if let Some(i) = records.iter().position(|r| r.status == ORACLE_E_EXCEPTION) {
                    assert_eq!(i + 1, records.len(), "{pin} {symbol}");
                    let junk = [script.bytes.as_slice(), &[0xff, 0xff]].concat();
                    let again = oracle.bls_exec_injecting(&junk, symbol, 1, RLC_ERR, false);
                    assert_eq!(again.unwrap(), records, "{pin} {symbol}");
                }
                // The record's code is the post-op read: the code the window's last call
                // left, or 0 after a clear point (CheckRelicErrors writes the field).
                for (i, r) in records.iter().enumerate() {
                    if let Some(last) = r.log.as_ref().and_then(|l| l.last()) {
                        assert!([0, last.code].contains(&r.relic_code), "{pin} {symbol} op {i}");
                    }
                }
                assert_eq!(run(&oracle, &script), baseline, "{pin} {symbol}: the code leaked");
            }

            // The nth call counts across the script; past the last call nothing fires.
            let writes = base.iter().filter(|c| c.symbol == "ep_write_bin").count();
            assert!(writes >= 2, "{pin}");
            let second = base.iter().enumerate().filter(|(_, c)| c.symbol == "ep_write_bin");
            let second = second.map(|(i, _)| i).nth(1).unwrap();
            let records = oracle
                .bls_exec_injecting(&script.bytes, "ep_write_bin", 2, RLC_ERR, false)
                .unwrap();
            let log = flat_log(&records);
            assert_eq!((&log[..second], log[second].code), (&base[..second], RLC_ERR), "{pin}");

            // SER_HASH's own ep_write_bin: no clear point follows in its window, so the
            // record's code is the injected one.
            let f = flags | LOG;
            let mut s = Script::keys(f);
            let h = s.op(SER_HASH, f, &[&[1]]);
            let before =
                run(&oracle, &s)[..h].iter().map(|r| count(r, "ep_write_bin")).sum::<usize>();
            let nth = u32::try_from(before + 1).unwrap();
            let records =
                oracle.bls_exec_injecting(&s.bytes, "ep_write_bin", nth, RLC_ERR, false).unwrap();
            assert_eq!(records.len(), s.ops, "{pin}");
            assert_eq!(records[h].relic_code, RLC_ERR, "{pin}: {:?}", records[h]);
            assert!(records[..h].iter().all(|r| r.relic_code == 0), "{pin}");
            let past = writes as u32 + 1;
            let records = oracle
                .bls_exec_injecting(&script.bytes, "ep_write_bin", past, RLC_ERR, false)
                .unwrap();
            assert_eq!(records, baseline, "{pin}");
        }

        // A code set after the window, by the store's serialization (D18): the log is clean
        // and the record still reports it.
        for flags in [0, LEGACY] {
            let mut s = Script::default();
            s.decode(flags, BlsKind::SecretKey, &scalar(1), 0);
            let pubkey = s.op(PUBKEY, flags | LOG, &[&[0, 1]]);
            let records =
                oracle.bls_exec_injecting(&s.bytes, "ep_write_bin", 1, RLC_ERR, true).unwrap();
            assert_eq!(records.len(), s.ops, "{pin}");
            let r = &records[pubkey];
            assert_eq!(r.relic_code, RLC_ERR, "{pin}: {r:?}");
            assert!(r.log.iter().flatten().all(|c| c.code == 0), "{pin}: {r:?}");
            // Windowed calls only: the store's call is not counted, nothing fires.
            let records =
                oracle.bls_exec_injecting(&s.bytes, "ep_write_bin", 1, RLC_ERR, false).unwrap();
            assert_eq!(records, run(&oracle, &s), "{pin}");
        }
        let err = oracle.bls_exec_injecting(&[], "ep_add_projc", 0, RLC_ERR, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{pin}");
        let err = oracle.bls_exec_injecting(&[], "ep_add\0", 1, RLC_ERR, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{pin}");
    }
}

/// relic's code left on the thread when a call starts (relic code run outside the oracle;
/// D26, D42): every entry point, under each of the shim's guards, fails with it and clears
/// it, so the call after it gives the clean result. The inputs need not be valid: the check
/// runs before any.
#[test]
fn a_code_left_on_the_thread_fails_any_entry_point() {
    type Call<'a> = Box<dyn Fn(&Oracle) -> Result<String> + 'a>;
    fn show<T: std::fmt::Debug>(result: Result<T>) -> Result<String> {
        result.map(|v| format!("{v:?}"))
    }
    let main = Chain::main();
    let ctx = Context::new(
        1,
        vec![],
        false,
        vec![],
        0,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    );
    let stream = P2pStream {
        version: 70242,
        bls_legacy: false,
        qdata_max_vvec: 0,
        qdata_max_contrib: 0,
    };
    let keys = Script::keys(LOG);
    let calls: Vec<(&str, Call)> = vec![
        ("bls_exec", Box::new(|o| show(o.bls_exec(&keys.bytes)))),
        ("bls_exec null script", Box::new(|o| show(o.bls_exec_null_script()))),
        (
            "bls_decode",
            Box::new(|o| show(o.bls_decode(BlsKind::PublicKey, BlsMode::Eager, true, &[0; 48]))),
        ),
        (
            "bls_decode lazy",
            Box::new(|o| show(o.bls_decode(BlsKind::Signature, BlsMode::Lazy, false, &[0; 96]))),
        ),
        ("bls_verify", Box::new(|o| show(o.bls_verify(true, &[0; 48], &[0; 96], &MSG)))),
        ("payload_decode", Box::new(|o| show(o.payload_decode(1, true, &[0; 4])))),
        ("p2p_decode", Box::new(|o| show(o.p2p_decode("clsig", &[0; 4], &stream)))),
        ("special_tx_sign_hash", Box::new(|o| show(o.special_tx_sign_hash(&main, &[0; 4])))),
        ("sml_merkle_root", Box::new(|o| show(o.sml_merkle_root(&main, ListEncoding::Disk, &[])))),
        ("check_special_tx", Box::new(|o| show(o.check_special_tx(&main, &[0; 4], &ctx, true)))),
        ("llmq_context", Box::new(|o| show(o.llmq_context(&main).map(drop)))),
        (
            "coin_codec",
            Box::new(|o| show(o.coin_codec(CoinCodec::VarIntEncode, &300u64.to_le_bytes()))),
        ),
        ("addrman_chance", Box::new(|o| show(o.addrman_chance(3600, 2)))),
    ];
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for (name, call) in &calls {
            let clean = format!("{:?}", call(&oracle));
            let err = oracle.after_relic_code(RLC_ERR, |o| call(o)).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Exception, "{pin} {name}: {err:?}");
            assert!(
                err.message.contains("relic's code is 1 when the call starts"),
                "{pin} {name}: {err:?}"
            );
            assert_eq!(format!("{:?}", call(&oracle)), clean, "{pin} {name}: the code stayed set");
        }
    }
}

/// Opus r1 m1's probe: a code left on the thread made `bls_verify` reject a valid signature
/// and `bls_decode` call a valid key invalid, with no error (dashbls's CheckRelicErrors took
/// the code and Core's wrapper caught the throw).
#[test]
fn a_left_code_turns_no_verdict() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let records = run(&oracle, &Script::keys(0));
        let pk: [u8; 48] = records[1].output.clone().try_into().unwrap();
        let sig: [u8; 96] = records[2].output.clone().try_into().unwrap();
        let verify = |o: &Oracle| o.bls_verify(false, &pk, &sig, &MSG);
        let decode = |o: &Oracle| o.bls_decode(BlsKind::PublicKey, BlsMode::Eager, false, &pk);
        assert!(verify(&oracle).unwrap(), "{pin}");
        let err = oracle.after_relic_code(RLC_ERR, verify).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Exception, "{pin}: {err:?}");
        assert!(verify(&oracle).unwrap(), "{pin}");
        let err = oracle.after_relic_code(RLC_ERR, decode).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Exception, "{pin}: {err:?}");
        assert!(decode(&oracle).unwrap().valid, "{pin}");
    }
}

/// A code a call leaves and no dashbls check takes (here the write-back's ep_write_bin, which
/// no CheckRelicErrors follows) fails that call, not the thread's next one (D42); in
/// `oracle_bls_exec` the op's record reports it and the call clears it on every return.
#[test]
fn a_code_left_when_the_call_ends() {
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let records = run(&oracle, &Script::keys(0));
        let pk = records[1].output.clone();
        let decode = |o: &Oracle| o.bls_decode(BlsKind::PublicKey, BlsMode::Eager, false, &pk);
        let clean = decode(&oracle).unwrap();
        let err = oracle.injecting("ep_write_bin", 1, RLC_ERR, true, decode).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Exception, "{pin}: {err:?}");
        assert!(err.message.contains("relic's code is 1 when the call ends"), "{pin}: {err:?}");
        assert_eq!(decode(&oracle).unwrap(), clean, "{pin}: the code stayed set");

        // A DECODE whose output's ep_write_bin sets the code: its record reports it, and the
        // call clears it when it ends, also when a bad op after it fails the call.
        let mut s = Script::default();
        s.decode(0, BlsKind::PublicKey, &pk, 0);
        let set = oracle.bls_exec_injecting(&s.bytes, "ep_write_bin", 1, RLC_ERR, true).unwrap();
        assert_eq!(set[0].relic_code, RLC_ERR, "{pin}: {:?}", set[0]);
        assert_no_code(&oracle, &run(&oracle, &s));
        let bad = [s.bytes.as_slice(), &[0xff, 0]].concat();
        let err = oracle.bls_exec_injecting(&bad, "ep_write_bin", 1, RLC_ERR, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{pin}: {err:?}");
        assert!(err.message.contains("bad op"), "{pin}: the end replaced the error: {err:?}");
        assert_no_code(&oracle, &run(&oracle, &s));
    }
}

/// relic's code left on the thread when an entry point returns before its guard runs (Sol r2
/// m1, D47): one refusing its arguments fails with it, as inside the guard, and a getter or a
/// free clears it unreported; either way the call after it gives the clean result. A test
/// hook refusing its arguments leaves the code to the next call. The chain-session
/// declarations are O2b-1's to bind.
#[test]
fn a_code_left_before_any_guard_is_taken() {
    unsafe extern "C" {
        #[link_name = "oracle_v24_0_chain_step"]
        fn chain_step(
            s: *mut c_void,
            op: *const u8,
            op_len: usize,
            out: *mut u8,
            out_len: *mut usize,
        ) -> c_int;
        #[link_name = "oracle_v24_0_chain_free"]
        fn chain_free(s: *mut c_void) -> c_int;
    }
    struct Case {
        name: &'static str,
        /// Its status.
        clean: c_int,
        /// Its status when it starts with a code set.
        seeded: c_int,
        /// Whether it leaves the code to the next call.
        leaves_code: bool,
        call: fn() -> c_int,
    }
    let cases = [
        Case {
            name: "chain_step of no session",
            clean: ORACLE_E_ARG,
            seeded: ORACLE_E_EXCEPTION,
            leaves_code: false,
            call: || unsafe { chain_step(null_mut(), null(), 0, null_mut(), null_mut()) },
        },
        Case {
            name: "credit_pool_step of a null context of length 1",
            clean: ORACLE_E_ARG,
            seeded: ORACLE_E_EXCEPTION,
            leaves_code: false,
            call: || unsafe {
                v24_0::credit_pool_step(
                    null(),
                    0,
                    null(),
                    1,
                    null(),
                    0,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            },
        },
        Case {
            name: "llmq_set_mined of a null commitment of length 1",
            clean: ORACLE_E_ARG,
            seeded: ORACLE_E_EXCEPTION,
            leaves_code: false,
            call: || unsafe { v24_0::llmq_set_mined(null_mut(), 0, null(), 1, 0) },
        },
        Case {
            name: "chain_free of no session",
            clean: ORACLE_OK,
            seeded: ORACLE_OK,
            leaves_code: false,
            call: || unsafe { chain_free(null_mut()) },
        },
        Case {
            name: "abi_version",
            clean: ABI_VERSION,
            seeded: ABI_VERSION,
            leaves_code: false,
            call: || unsafe { v24_0::abi_version() },
        },
        Case {
            name: "test_bls_inject counting from 0",
            clean: ORACLE_E_ARG,
            seeded: ORACLE_E_ARG,
            leaves_code: true,
            call: || unsafe { v24_0::test_bls_inject(c"ep_write_bin".as_ptr(), 0, RLC_ERR, 1) },
        },
    ];
    let oracle = Oracle::all().into_iter().find(|o| o.pin() == Pin::V24_0).expect("the v24.0 pin");
    // Every mismatch is collected; the next call is guarded, so it takes a code a case left.
    let mut wrong = vec![];
    // The call after each: an Argument error when the code was cleared.
    let next = |name: &str, leaves_code: bool| {
        let err = oracle.bls_exec_null_script().unwrap_err();
        let still_set =
            err.kind == ErrorKind::Exception && err.message.contains("relic's code is 1");
        (still_set != leaves_code).then(|| format!("{name}: the next call gave {err:?}"))
    };
    for case in &cases {
        let status = (case.call)();
        if status != case.clean {
            wrong.push(format!("{}: status {status}, not {}", case.name, case.clean));
        }
        let status = oracle.after_relic_code(RLC_ERR, |_| Ok((case.call)())).unwrap();
        if status != case.seeded {
            wrong.push(format!(
                "{}: status {status} with the code set, not {}",
                case.name, case.seeded
            ));
        }
        if let Some(m) = next(case.name, case.leaves_code) {
            wrong.push(m);
        }
    }
    // A live LLMQ context freed with the code set: the free tears it down and clears the code.
    // The context holds the pin, so the code is set through the raw hook.
    let llmq = oracle.llmq_context(&Chain::main()).unwrap();
    assert_eq!(unsafe { v24_0::test_bls_set_code(RLC_ERR) }, ORACLE_OK);
    drop(llmq);
    if let Some(m) = next("llmq_ctx_free of a live context", false) {
        wrong.push(m);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The design's relic probes (§2.2) through Core's wrappers, under the legacy scheme that
/// decodes these points: no code is set on any of them (§3.3).
#[test]
fn relic_probes_p1_to_p5() {
    // (0, ±2), G1's order-3 points: legacy x = 0 with the sign bit (wrap_codec's rule).
    let mut order3 = [0u8; 48];
    order3[0] = 0x80;
    // x = 1 is off both curves: 1 + 4 and 1 + 4(1 + u) are non-residues.
    let mut g1_off = [0u8; 48];
    g1_off[47] = 1;
    let mut g2_off = [0u8; 96];
    g2_off[47] = 1;
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let f = LEGACY | LOG;
        let mut s = Script::keys(LEGACY);
        s.decode(LEGACY, BlsKind::PublicKey, &order3, 40);
        s.decode(LEGACY, BlsKind::PublicKey, &g1_off, 41);
        s.decode(LEGACY, BlsKind::Signature, &g2_off, 42);
        let exotic_valid = [40u8, 41].map(|r| s.op(ISVALID, f, &[&[r]]));
        let sig_valid = s.op(ISVALID, f, &[&[42]]);
        // P1, P2: [k](0, 2) normalizes through fp_inv(0) = 0 to the affine (0, 0).
        let p2 = s.op(MUL, f, &[&[40], &K, &[43]]);
        s.op(NEG, f, &[&[1, 44]]);
        s.op(ADD, f, &[&[1, 44, 45]]);
        s.op(MUL, f, &[&[1], &scalar(0), &[46]]);
        s.op(NEG, f, &[&[45, 47]]);
        // §8's fixed vectors (Sol r2, r3): the PROJC infinity G + (−G) differs from it,
        // the BASIC infinity [0]G and NEG's reset of the PROJC one equal it.
        let sol = [45u8, 46, 47].map(|r| s.op(IMPL_EQ, f, &[&[r, 43]]));
        // P3: g1_add and g1_mul on an off-curve point.
        let p3 = [s.op(ADD, f, &[&[41, 41, 48]]), s.op(MUL, f, &[&[41], &K, &[49]])];
        // P4, P5: an off-curve G2 operand with y = 0 makes a zero Miller value, and the
        // final exponentiation of 0 is 0, so the legacy check fails with no code.
        let p5 = s.op(VERIFY, f, &[&[42, 1], &MSG]);
        let records = run(&oracle, &s);
        assert_no_code(&oracle, &records);
        for i in exotic_valid {
            assert_eq!(records[i].output, [1, 0], "{pin}: valid wrapper, element off G1");
        }
        assert_eq!(records[sig_valid].output, [1, 0], "{pin}");
        assert_eq!(records[p2].status, ORACLE_OK, "{pin}: {:?}", records[p2]);
        let sol: Vec<_> = sol.iter().map(|&i| records[i].output.clone()).collect();
        assert_eq!(sol, [vec![0], vec![1], vec![1]], "{pin}");
        for i in p3 {
            assert!(records[i].log.as_ref().is_some_and(|l| !l.is_empty()), "{pin}");
        }
        assert_eq!(records[p5].status, ORACLE_REJECT, "{pin}");
        assert!(count(&records[p5], "pp_map_sim_oatep_k12") >= 1, "{pin}: {:?}", records[p5]);
        assert!(count(&records[p5], "fp12_cmp") >= 1, "{pin}");
    }
}

//! The scan classifiers: pure functions of a block's bytes, its height and the network's
//! parameters. Each cites the Dash Core `v24.0.0-rc.4` predicate it mirrors.

use std::collections::HashSet;

use dash_pkc::bls::BlsSchemeKind;
use serde_json::{Value, json};

use super::params::llmq;
use super::params::{Activation, VERSIONBITS_NUM_BITS};
use super::payload::{BlsState, parse_cbtx, parse_mnhf, parse_qc, sig_state};
use super::{BlockInput, BlockResult, CRow, CRowKind, Cfg, ClFact, Hit, Scan, V24, merkle_root};
use crate::check::{core_txid, rev_hex};
use crate::split::{BlockSpans, PayloadRule, SplitError, TxSpan, split_block_with};

/// Named single-predicate flips, used by the mutant tests to show that every fixture pair
/// actually pins its predicate. Only constructible in test builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum Mutant {
    /// (a) `nVersion > 3` becomes `>= 3`.
    AGtToGe,
    /// (a) `nType != 0` becomes `== 0`.
    ATypeInvert,
    /// (a) and (b) read `nVersion` as unsigned.
    VersionUnsigned,
    /// (b) `nVersion < 3` becomes `<= 3`.
    BLtToLe,
    /// (b) `h < DIP0003Height` becomes `<=`.
    BDip3Boundary,
    /// (c) `V > R` becomes `V >= R`.
    CGtToGe,
    /// (c) the window `offset < 100` becomes `<=`.
    CWindowLe,
    /// (d) `diff >= height` becomes `>`.
    DGeToGt,
    /// (d) `diff > prevDiff + 1` becomes `>=`.
    DOlderGe,
    /// (d) `h >= V20Height` becomes `>`.
    DV20Gt,
    /// (d) the block is v24-active from `h >= since`; becomes `>`.
    DV24Gt,
    /// (d) a null signature after a non-null one is no longer reported.
    DNullAfterNonNull,
    /// (e) the bit is read one higher.
    EBitPlusOne,
    /// (e) selects type 6 instead of type 7.
    ETypeSix,
    /// (e) `bit >= VERSIONBITS_NUM_BITS` becomes `>= 28`.
    EBoundLe,
    /// (e) `bit >= VERSIONBITS_NUM_BITS` becomes `>= 32`.
    EBound32,
    /// (e) the `useEHF` test of `IsValidMNActivation` is inverted.
    EEhfInverted,
    /// (f) `prev >= DIP0024QuorumsHeight` becomes `>`.
    FBoundary,
    /// (f) null and non-null swapped.
    FNullSwap,
}

/// Appends a hit; `tx` names the transaction (index and span) to take the txid from.
fn push(
    out: &mut BlockResult,
    inp: &BlockInput<'_>,
    scan: Scan,
    class: &'static str,
    tx: Option<(usize, &TxSpan)>,
    detail: Value,
) {
    out.hits.push(Hit {
        scan,
        class,
        height: inp.height,
        block: inp.hash,
        tx_index: tx.map(|(i, _)| i as u32),
        txid: tx.map(|(_, s)| core_txid(&inp.bytes[s.range.clone()], s)),
        stale: inp.stale,
        detail,
    });
}

fn cl_sig_state(cfg: &Cfg, sig: &[u8; 96]) -> BlsState {
    if let Some(s) = cfg.sig_cache.lock().expect("sig cache").get(sig) {
        return *s;
    }
    // The coinbase's `bestCLSignature` is serialized under Core's global flag, which is the
    // basic scheme from V19 on; every CbTx with the field is V20+ (quirks 22, 35).
    let s = sig_state(sig, BlsSchemeKind::Basic);
    let mut cache = cfg.sig_cache.lock().expect("sig cache");
    if cache.len() > 1 << 20 {
        cache.clear();
    }
    cache.insert(*sig, s);
    s
}

/// Scan (a) beyond the plain predicate: the block framed under today's rule and under the
/// pre-v21 `nVersion == 3` rule. A transaction with `nVersion > 3 && nType != 0` makes the two
/// disagree (`src/primitives/transaction.h:276,362`, `72d2008901`).
fn framing_differs(
    ge3: &Result<BlockSpans, SplitError>,
    eq3: &Result<BlockSpans, SplitError>,
) -> bool {
    match (ge3, eq3) {
        (Ok(a), Ok(b)) => {
            a.txs.len() != b.txs.len()
                || a.txs
                    .iter()
                    .zip(&b.txs)
                    .any(|(x, y)| x.range != y.range || x.payload != y.payload)
        }
        (Err(_), Err(_)) => false,
        _ => true,
    }
}

/// Runs the selected classifiers on one block.
pub fn scan_block(cfg: &Cfg, inp: &BlockInput<'_>) -> BlockResult {
    let mut out = BlockResult::default();
    let ge3 = split_block_with(inp.bytes, PayloadRule::Ge3);
    if cfg.scans.contains(Scan::A) {
        let eq3 = split_block_with(inp.bytes, PayloadRule::Eq3);
        if framing_differs(&ge3, &eq3) {
            let detail = json!({
                "ge3": ge3.as_ref().map(|s| s.txs.len()).map_err(|e| e.to_string()),
                "eq3": eq3.as_ref().map(|s| s.txs.len()).map_err(|e| e.to_string()),
            });
            push(&mut out, inp, Scan::A, "a.framing_differs", None, detail);
        }
    }
    let spans = match ge3 {
        Ok(s) => s,
        Err(e) => {
            out.unparsed = Some(e.to_string());
            out.add("unparsed_blocks", 1);
            out.finish(inp.stale);
            return out;
        }
    };
    out.add("blocks", 1);
    out.add("txs", spans.txs.len() as u64);
    // The copy read is the first of any duplicates and is not otherwise checked: a body that
    // frames but is not the block the header commits to must not be classified silently.
    let txids: Vec<_> =
        spans.txs.iter().map(|s| core_txid(&inp.bytes[s.range.clone()], s)).collect();
    if merkle_root(&txids)[..] != inp.bytes[36..68] {
        out.unparsed = Some("the merkle root differs from the header's".into());
        out.add("unparsed_blocks", 1);
    }
    let net = &cfg.net;
    let h = inp.height;

    let mut mnhf_bits_seen: HashSet<u8> = HashSet::new();
    for (i, span) in spans.txs.iter().enumerate() {
        let (ver, ty) = (span.version, span.tx_type);
        let tx = Some((i, span));
        let payload = span.payload.as_ref().map(|r| &inp.bytes[r.clone()]);
        // `nVersion > 3`, or `>= 3` if `ge`; unsigned under the `VersionUnsigned` mutant.
        let ver_gt3 = |ge: bool| -> bool {
            let v = if cfg.mutated(Mutant::VersionUnsigned) {
                i32::from(ver as u16)
            } else {
                i32::from(ver)
            };
            if ge {
                v >= 3
            } else {
                v > 3
            }
        };

        if cfg.scans.contains(Scan::A) {
            // Core: primitives/transaction.h:244,325-337
            let version_hit = ver_gt3(cfg.mutated(Mutant::AGtToGe));
            let type_hit = if cfg.mutated(Mutant::ATypeInvert) {
                ty == 0
            } else {
                ty != 0
            };
            if version_hit && type_hit {
                push(
                    &mut out,
                    inp,
                    Scan::A,
                    "a.version_gt3_typed",
                    tx,
                    json!({"version": ver, "type": ty, "payload": span.payload.is_some()}),
                );
            }
        }

        if cfg.scans.contains(Scan::B) {
            // Core: validation.cpp:351-386 (type checks only after DIP3), consensus/tx_check.cpp
            // `nVersion < 3`, or `<= 3` under the mutant
            let low_version = !ver_gt3(!cfg.mutated(Mutant::BLtToLe));
            if ty != 0 && low_version {
                let pre = if cfg.mutated(Mutant::BDip3Boundary) {
                    h <= net.dip3
                } else {
                    h < net.dip3
                };
                let class = if pre {
                    "b.pre_dip3"
                } else {
                    "b.post_dip3"
                };
                out.add(&format!("b.type.{ty}"), 1);
                let exemption = (matches!(ty, 6 | 7) && (span.n_in == 0 || span.n_out == 0))
                    || (ty == 9 && span.n_in == 0);
                if exemption {
                    out.add("b.exemption_used", 1);
                }
                let cb_one_byte = i == 0
                    && ty == 5
                    && span.first_script_sig.as_ref().is_some_and(|r| r.len() == 1);
                if cb_one_byte {
                    out.add("b.coinbase_type5_one_byte_script", 1);
                }
                push(
                    &mut out,
                    inp,
                    Scan::B,
                    class,
                    tx,
                    json!({
                        "version": ver, "type": ty, "n_in": span.n_in, "n_out": span.n_out,
                        "exemption_used": exemption, "coinbase_one_byte_script": cb_one_byte,
                    }),
                );
            }
        }

        if cfg.scans.contains(Scan::D) {
            classify_d_tx(cfg, inp, &mut out, i, span, payload);
        }

        // A payload is present exactly for `nVersion >= 3 && nType != 0` (`PayloadRule::Ge3`).
        if cfg.scans.contains(Scan::E) {
            let sel = if cfg.mutated(Mutant::ETypeSix) {
                6
            } else {
                7
            };
            if ty == sel {
                match payload {
                    Some(p) => classify_e(cfg, inp, &mut out, i, span, p, &mut mnhf_bits_seen),
                    None => out.add("e.type7_without_payload", 1),
                }
            }
        }
        if cfg.scans.contains(Scan::F)
            && ty == 6
            && let Some(p) = payload
        {
            classify_f(cfg, inp, &mut out, i, span, p);
        }
    }

    if cfg.scans.contains(Scan::C) && !inp.stale && h >= net.budget_start {
        let window = net.in_old_budget_window(h, cfg.mutated(Mutant::CWindowLe));
        let kind = if window {
            Some(CRowKind::Window)
        } else if net.is_superblock(h) {
            Some(CRowKind::Superblock)
        } else {
            None
        };
        if let (Some(kind), Some(cb)) = (kind, spans.txs.first()) {
            out.c_row = Some(CRow {
                height: h,
                block: inp.hash,
                kind,
                coinbase_value: cb.value_out,
                coinbase_txid: core_txid(&inp.bytes[cb.range.clone()], cb),
                blk: None,
            });
        }
    }
    out.finish(inp.stale);
    out
}

/// Scan (d), per transaction: the coinbase and any CbTx.
fn classify_d_tx(
    cfg: &Cfg,
    inp: &BlockInput<'_>,
    out: &mut BlockResult,
    i: usize,
    span: &TxSpan,
    payload: Option<&[u8]>,
) {
    let net = &cfg.net;
    let h = inp.height;
    let (ver, ty) = (span.version, span.tx_type);
    let tx = Some((i, span));
    if i == 0 {
        // Core: validation.cpp:4597 (`bad-cb-type`), :372-377
        if h >= net.dip3 && !(ver >= 3 && ty == 5) {
            push(out, inp, Scan::D, "d.coinbase_not_cbtx", tx, json!({"version": ver, "type": ty}));
        }
    } else if ty == 5 {
        push(out, inp, Scan::D, "d.cbtx_not_first", tx, json!({"version": ver}));
    }
    // A CbTx with a payload: `nVersion >= 3` is implied (`PayloadRule::Ge3`).
    let (5, Some(p)) = (ty, payload) else {
        return;
    };
    if h < net.dip3 {
        push(out, inp, Scan::D, "d.cbtx_before_dip3", tx, json!({}));
    }
    let cb = match parse_cbtx(p) {
        Ok(cb) => cb,
        Err(reason) => {
            push(out, inp, Scan::D, "d.unparseable", tx, json!({"reason": reason}));
            return;
        }
    };
    out.add("d.cbtx", 1);
    out.add(&format!("d.version.{}", cb.version), 1);
    // CheckCbTx: evo/cbtx.cpp:24-55
    let dip8_active_at_prev = h > net.dip8;
    let is_v20 = if cfg.mutated(Mutant::DV20Gt) {
        h > net.v20
    } else {
        h >= net.v20
    };
    // `requires_unlock_root = is_v24_active && isV20` (evo/cbtx.cpp:45-50): version 4 is
    // required then and invalid otherwise. Without the deployment state, versions 3 and 4 are
    // both let through at V20 and counted as undecided.
    let v24_active = match cfg.v24 {
        V24::ActiveFrom(from) if cfg.mutated(Mutant::DV24Gt) => Some(h > from),
        v24 => v24.active_at(h),
    };
    let unlock_root_version_wrong = match v24_active {
        Some(v24) => (v24 && is_v20) != (cb.version >= 4),
        None => {
            if is_v20 && matches!(cb.version, 3 | 4) {
                out.add("d.v24_undecided", 1);
            }
            false
        }
    };
    let wrong_version = cb.version == 0
        || cb.version >= 5
        || (dip8_active_at_prev && cb.version < 2)
        || is_v20 != (cb.version >= 3)
        || unlock_root_version_wrong;
    if wrong_version {
        push(
            out,
            inp,
            Scan::D,
            "d.version",
            tx,
            json!({
                "cbtx_version": cb.version, "dip8_active": dip8_active_at_prev, "v20": is_v20,
                "v24": v24_active,
            }),
        );
    }
    if i64::from(cb.height) != i64::from(h) {
        push(out, inp, Scan::D, "d.height", tx, json!({"cbtx_height": cb.height}));
    }
    if !cb.has_cl {
        return;
    }
    let state = cl_sig_state(cfg, &cb.best_cl_sig);
    out.add(&format!("d.sig.{}", state.name()), 1);
    let diff = cb.best_cl_height_diff;
    if state == BlsState::Malleable {
        // Canonical in neither scheme: `GetTxPayload` fails (`bad-cbtx-payload`).
        push(out, inp, Scan::D, "d.sig_malleable", tx, json!({"diff": diff}));
        return;
    }
    if state == BlsState::OtherScheme {
        push(out, inp, Scan::D, "d.sig_other_scheme", tx, json!({"diff": diff}));
    }
    // Core: evo/specialtxman.cpp:183-185 (valid sig), :209-211 (null sig)
    let ge = if cfg.mutated(Mutant::DGeToGt) {
        diff > h
    } else {
        diff >= h
    };
    if ge {
        push(out, inp, Scan::D, "d.diff_ge_height", tx, json!({"diff": diff, "sig": state.name()}));
    }
    if !state.is_valid() && diff != 0 {
        push(out, inp, Scan::D, "d.null_sig_nonzero_diff", tx, json!({"diff": diff}));
    }
    // Core reads only `vtx[0]`'s ChainLock (`d.cbtx_not_first` flags the others).
    if i == 0 {
        out.cl = Some(ClFact {
            height: h,
            block: inp.hash,
            txid: core_txid(&inp.bytes[span.range.clone()], span),
            diff,
            sig_valid: state.is_valid(),
            // `GetNonNullCoinbaseChainlock` (evo/cbtx.cpp:181-211)
            non_null: cb.height == h as i32 && state.is_valid() && diff < h,
        });
    }
}

/// The sequential part of scan (d): a block's coinbase ChainLock against the previous block's
/// (`CheckCbTxBestChainlock`, evo/specialtxman.cpp:169-176). `facts` are the best chain's, by
/// height.
pub fn cl_sequence_hits(cfg: &Cfg, facts: &[ClFact]) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut prev: Option<&ClFact> = None;
    for f in facts {
        let before = prev.filter(|p| p.height + 1 == f.height && p.non_null);
        if let Some(p) = before {
            let class = if !f.sig_valid {
                (!cfg.mutated(Mutant::DNullAfterNonNull)).then_some("d.null_after_non_null")
            } else {
                let limit = u64::from(p.diff) + 1;
                let older = if cfg.mutated(Mutant::DOlderGe) {
                    u64::from(f.diff) >= limit
                } else {
                    u64::from(f.diff) > limit
                };
                older.then_some("d.older_clsig")
            };
            if let Some(class) = class {
                let detail = json!({"diff": f.diff, "prev_diff": p.diff});
                hits.push(Hit::on_coinbase(Scan::D, class, f.height, f.block, f.txid, detail));
            }
        }
        prev = Some(f);
    }
    hits
}

/// Scan (e), one MNHF transaction. Core: evo/mnhftx.cpp:95-103 (`IsTriviallyValid`), :195-205.
fn classify_e(
    cfg: &Cfg,
    inp: &BlockInput<'_>,
    out: &mut BlockResult,
    i: usize,
    span: &TxSpan,
    payload: &[u8],
    seen: &mut HashSet<u8>,
) {
    let tx = Some((i, span));
    let m = match parse_mnhf(payload) {
        Ok(m) => m,
        Err(reason) => {
            push(out, inp, Scan::E, "e.unparseable", tx, json!({"reason": reason}));
            return;
        }
    };
    let bit = if cfg.mutated(Mutant::EBitPlusOne) {
        m.bit.wrapping_add(1)
    } else {
        m.bit
    };
    out.add(&format!("e.bit.{bit}"), 1);
    // `Params().IsValidMNActivation(bit, pindexPrev->GetMedianTimePast())` (evo/mnhftx.cpp:124).
    let mtp = inp.mtp_prev.and_then(|f| f());
    push(
        out,
        inp,
        Scan::E,
        "e.signal",
        tx,
        json!({
            "bit": bit, "payload_version": m.version, "quorum_hash": rev_hex(&m.quorum_hash),
            "sig": m.sig.name(), "mtp_prev": mtp,
        }),
    );
    // CURRENT_VERSION is 1 (evo/mnhftx.h)
    if m.version != 1 {
        push(out, inp, Scan::E, "e.bad_version", tx, json!({"version": m.version}));
    }
    // `IsTriviallyValid`: `signal.versionBit >= VERSIONBITS_NUM_BITS` (evo/mnhftx.cpp:100)
    let bound = if cfg.mutated(Mutant::EBound32) {
        32
    } else if cfg.mutated(Mutant::EBoundLe) {
        VERSIONBITS_NUM_BITS - 1
    } else {
        VERSIONBITS_NUM_BITS
    };
    let in_bounds = bit < bound;
    if !in_bounds {
        push(out, inp, Scan::E, "e.bit_out_of_bounds", tx, json!({"bit": bit}));
    }
    // `IsValidMNActivation` asserts `nBit < VERSIONBITS_NUM_BITS`, which the bound above checks
    // first (`IsTriviallyValid`).
    if in_bounds {
        let verdict = cfg.net.mn_activation(bit, mtp, cfg.mutated(Mutant::EEhfInverted));
        let class = match verdict {
            Activation::Valid => None,
            Activation::Invalid => Some("e.invalid_activation"),
            Activation::Undecided => Some("e.activation_undecided"),
        };
        if let Some(class) = class {
            push(out, inp, Scan::E, class, tx, json!({"bit": bit, "mtp_prev": mtp}));
        }
    }
    if !seen.insert(bit) {
        push(out, inp, Scan::E, "e.duplicate_in_block", tx, json!({"bit": bit}));
    }
}

/// Scan (f), one quorum-commitment transaction.
fn classify_f(
    cfg: &Cfg,
    inp: &BlockInput<'_>,
    out: &mut BlockResult,
    i: usize,
    span: &TxSpan,
    payload: &[u8],
) {
    let tx = Some((i, span));
    let qc = match parse_qc(payload) {
        Ok(q) => q,
        Err(reason) => {
            push(out, inp, Scan::F, "f.unparseable", tx, json!({"reason": reason}));
            return;
        }
    };
    let h = inp.height;
    let ty = qc.llmq_type;
    out.add("f.commitments", 1);
    out.add(&format!("f.type.{}", llmq::name(ty)), 1);
    let is_null = if cfg.mutated(Mutant::FNullSwap) {
        !qc.is_null
    } else {
        qc.is_null
    };
    let nullness = if is_null {
        "null"
    } else {
        "nonnull"
    };
    let Some(prev) = h.checked_sub(1) else {
        return;
    };
    // Evaluated at the mined block's `pprev`, the rule for which types a block must carry
    // (llmq/blockprocessor.cpp). Core verifies a non-null commitment against its quorum base
    // block's `pprev` instead (llmq/utils.cpp), so a non-null row here is not by itself a
    // commitment Core rejects.
    let enabled = cfg.net.quorum_type_enabled(ty, prev, cfg.mutated(Mutant::FBoundary));
    let detail = json!({
        "type": llmq::name(ty), "type_byte": ty, "null": is_null,
        "quorum_hash": rev_hex(&qc.quorum_hash), "quorum_index": qc.quorum_index,
        "commitment_version": qc.version, "qc_height": qc.qc_height,
    });
    match enabled {
        // Core: llmq/blockprocessor.cpp:564-604 (`bad-qc-commitment-type`)
        None => push(out, inp, Scan::F, "f.unknown_type", tx, detail),
        Some(true) => out.add(&format!("f.enabled.{nullness}"), 1),
        Some(false) => {
            // Disabled by height: after its disable height (50_60 on mainnet) or before its
            // enable height (the rest).
            let after_disable = ty == llmq::LLMQ_50_60;
            let class = match (after_disable, is_null) {
                (true, true) => "f1.after_disable.null",
                (true, false) => "f1.after_disable.nonnull",
                (false, true) => "f2.before_enable.null",
                (false, false) => "f2.before_enable.nonnull",
            };
            push(out, inp, Scan::F, class, tx, detail);
        }
    }
}

/// What scan (c) concludes about one candidate block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CClass {
    /// `V <= R`.
    Within,
    /// `V > R` in an old-budget window: accepted under `NoCheck` (payments.cpp:277-286).
    C1WindowOverpay,
    /// `V > R` at a superblock height, within `R + GetPaymentsLimit`.
    C2Superblock,
    /// `V > R` anywhere else, or beyond the superblock limit: Core rejects (`bad-cb-amount`).
    C3Anomaly,
}

/// Core's `IsBlockValueValid` (masternode/payments.cpp:313-345) reduced to the classes scan (c)
/// reports. `reward` is `R = subsidy + fees`; `limit` is `GetPaymentsLimit(h)`.
pub fn classify_c(cfg: &Cfg, row: &CRow, reward: i64, limit: Option<i64>) -> CClass {
    let v = i128::from(row.coinbase_value);
    let r = i128::from(reward);
    let within = if cfg.mutated(Mutant::CGtToGe) {
        v < r
    } else {
        v <= r
    };
    if within {
        return CClass::Within;
    }
    match row.kind {
        CRowKind::Window => CClass::C1WindowOverpay,
        CRowKind::Superblock => match limit {
            Some(l) if v <= r + i128::from(l) => CClass::C2Superblock,
            _ => CClass::C3Anomaly,
        },
    }
}

/// Turns a classified candidate into its reported row.
pub fn c_hit(row: &CRow, class: CClass, reward: i64, limit: Option<i64>) -> Hit {
    let name = match (class, row.kind) {
        (CClass::C1WindowOverpay, _) => "c.c1_window_overpay",
        (CClass::C2Superblock, _) => "c.c2_superblock",
        (CClass::C3Anomaly, _) => "c.c3_anomaly",
        (CClass::Within, CRowKind::Window) => "c.window_within",
        (CClass::Within, CRowKind::Superblock) => "c.superblock_within",
    };
    let detail = json!({
        "coinbase_value": row.coinbase_value, "reward": reward,
        "excess": i128::from(row.coinbase_value) - i128::from(reward),
        "payments_limit": limit,
        "blk": row.blk.map(|(file, offset)| json!({"file": file, "record_offset": offset})),
    });
    Hit::on_coinbase(Scan::C, name, row.height, row.block, row.coinbase_txid, detail)
}

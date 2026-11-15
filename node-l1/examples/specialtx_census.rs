//! Counts a `diff_check_special_tx` corpus for the run report (EV1 §6.2, D30): per type, the
//! cases Core judged, those both sides accepted, and those where Core's verdict and ours differ;
//! and the cases whose BLS signature was verified, by the global flag they verify under
//! ([`node_l1::specialtx::Case::legacy_flag`]), accepted or refused at the signature step, the
//! last: a ProUpServTx's or ProUpRevTx's operator signature in a world that checks signatures
//! (`bad-protx-sig`), and the quorum signature of an MNHF signal (`bad-mnhf-invalid`) or an asset
//! unlock (`bad-assetunlock-not-verified`), which Core always verifies.
//!
//! `cargo run -p node-l1 --features oracle --example specialtx_census -- <corpus dir>...`, a
//! directory whose name ends `_quorum` read as `diff_check_special_tx_quorum`'s.

use std::collections::BTreeMap;

use dash_dml::specialtx::reasons::{BAD_ASSETUNLOCK_NOT_VERIFIED, BAD_MNHF_INVALID, BAD_PROTX_SIG};
use dash_oracle_sys::{Oracle, Pin};
use dash_primitives::TxType;
use node_l1::specialtx::{generate, generate_quorum, verdicts};

/// Judged, accepted, Core ≠ ours; BLS verified under the basic flag (accepted, refused), under
/// the legacy flag (accepted, refused).
#[derive(Default)]
struct Row {
    judged: usize,
    accepted: usize,
    mismatched: usize,
    basic: (usize, usize),
    legacy: (usize, usize),
}

impl Row {
    fn add(&mut self, other: &Row) {
        self.judged += other.judged;
        self.accepted += other.accepted;
        self.mismatched += other.mismatched;
        self.basic.0 += other.basic.0;
        self.basic.1 += other.basic.1;
        self.legacy.0 += other.legacy.0;
        self.legacy.1 += other.legacy.1;
    }
}

fn main() -> std::io::Result<()> {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut inputs, mut diverged) = (0usize, 0usize);
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for dir in std::env::args().skip(1) {
        let quorum = dir.trim_end_matches('/').ends_with("_quorum");
        for entry in std::fs::read_dir(&dir)? {
            let bytes = std::fs::read(entry?.path())?;
            inputs += 1;
            let case = if quorum {
                generate_quorum(&bytes)
            } else {
                generate(&bytes)
            };
            let Some(case) = case else {
                continue;
            };
            let v = match verdicts(&oracle, &case) {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(_) => {
                    diverged += 1;
                    continue;
                }
            };
            let row = rows.entry(format!("{:?}", case.tx_type())).or_default();
            row.judged += 1;
            row.accepted += usize::from(v.core.is_none() && v.ours.is_none());
            row.mismatched += usize::from(v.core != v.ours);
            let sig_step = match case.tx_type() {
                TxType::ProviderUpdateService | TxType::ProviderUpdateRevoke if case.world.sigs => {
                    Some(BAD_PROTX_SIG)
                }
                TxType::MnhfSignal => Some(BAD_MNHF_INVALID),
                TxType::AssetUnlock => Some(BAD_ASSETUNLOCK_NOT_VERIFIED),
                _ => None,
            };
            let refused = v.core.as_ref().map(|(_, reason)| reason.as_str());
            if sig_step.is_some_and(|step| refused.is_none_or(|reason| reason == step)) {
                let bls = if case.legacy_flag() {
                    &mut row.legacy
                } else {
                    &mut row.basic
                };
                match refused {
                    None => bls.0 += 1,
                    Some(_) => bls.1 += 1,
                }
            }
        }
    }
    println!("inputs {inputs}, harness divergences {diverged}");
    println!(
        "type: judged, accepted, Core != ours; BLS verified, basic flag (accepted, refused at the \
         signature), legacy flag (accepted, refused at the signature)"
    );
    let mut total = Row::default();
    for (tx_type, row) in &rows {
        print_row(tx_type, row);
        total.add(row);
    }
    print_row("total", &total);
    Ok(())
}

fn print_row(name: &str, r: &Row) {
    println!(
        "{name}: {}, {}, {}; basic ({}, {}), legacy ({}, {})",
        r.judged, r.accepted, r.mismatched, r.basic.0, r.basic.1, r.legacy.0, r.legacy.1
    );
}

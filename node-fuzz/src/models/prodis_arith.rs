//! `prodis_arith` (EV1 §6.5): the dissolution arithmetic of `CheckProDisTxForList` (§3.7 steps
//! 6-12) against a reference transcribed from Core, with `ProRataFloor` over `dash_num::Arith256`
//! (Core's `arith_uint256`):
//! - `pro_rata_floor` equals the reference for every three `u64`s, `None` exactly where the
//!   divisor is zero (Core's division throws);
//! - `check_pro_dis_tx_for_list` on a shared masternode and a dissolution built from the input
//!   gives the reference's verdict wherever Core's signed arithmetic is defined (no `CAmount`
//!   overflow on the path Core runs): its first reject, or acceptance, and for a zero divisor
//!   `failed-check-special-tx`, which Core's catch makes of the throw. Consensus state cannot
//!   reach that path (at least two shares of at least 100 DASH, §3.7), so that case is
//!   defensive. Where Core's arithmetic overflows, ours only has to be total and deterministic.
//!
//! Input: bytes 0-23 are the floor's three `u64`s (little-endian: total, weight, weight total).
//! Byte 24 gives the share count (2 + mod 7), byte 25 the actor (mod the count), byte 26 the
//! mode: unanimous (bit 0), early (bit 1), an actor output (bit 2), 5-byte amounts (bit 3; else
//! 8). Then the early penalty, each share's amount and each output's value (non-actor outputs in
//! share order, then the actor's), each a little-endian signed number of that width. Missing
//! bytes read as zero.

use std::sync::Arc;

use dash_consensus::primitives::Tx;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_dml::specialtx::reasons::{
    BAD_PRODIS_ACTOR_OUTPUT_ZERO, BAD_PRODIS_BONUS, BAD_PRODIS_FEE, BAD_PRODIS_PENALTY_FLOOR,
    BAD_PRODIS_PENALTY_SUM, FAILED_CHECK_SPECIAL_TX,
};
use dash_dml::specialtx::shared::PRODIS_MAX_FEE;
use dash_dml::specialtx::{check_pro_dis_tx_for_list, pro_rata_floor};
use dash_dml::testkit::{dmn_with, pro_tx};
use dash_dml::{Dmn, MnList, MnType};
use dash_num::Arith256;
use dash_primitives::{
    BlockHash, COMPACT_SIG_LEN, CollateralShare, CompactSig, NetInfo, NetInfoV2, ProDisTx,
    Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::codec::BaseCodec;
use node_l1::Divergence;

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(Divergence {
            field,
        })
    }
}

/// Core's `ProRataFloor` (`providertx_util.cpp` rc.4:88-95): `arith_uint256` of each amount read
/// as `uint64_t`, the product divided, the low 64 bits as a `CAmount`; `None` where the division
/// throws.
fn floor_reference(total: i64, weight: i64, weight_total: i64) -> Option<i64> {
    let product = Arith256::from_u64(total as u64) * Arith256::from_u64(weight as u64);
    Some(product.checked_div(Arith256::from_u64(weight_total as u64))?.low_u64() as i64)
}

/// The little-endian signed number of `width` (at most 8) bytes at `at`; missing bytes read as 0.
fn signed_le(data: &[u8], at: usize, width: usize) -> i64 {
    let mut le = [0u8; 8];
    for (i, b) in le.iter_mut().take(width).enumerate() {
        *b = data.get(at + i).copied().unwrap_or(0);
    }
    // Sign-extend from `width` bytes.
    let shift = 64 - 8 * width as u32;
    (i64::from_le_bytes(le) << shift) >> shift
}

/// A verdict as Core gives it: acceptance, or the class and reason.
type Verdict = Result<(), (TxValidationResult, String)>;

fn special(reason: &str) -> Verdict {
    Err((TxValidationResult::BadSpecial, reason.to_owned()))
}

/// The dissolution the input describes.
struct Dissolution {
    amounts: Vec<i64>,
    actor: usize,
    unanimous: bool,
    early: bool,
    penalty: i64,
    values: Vec<i64>,
}

impl Dissolution {
    fn read(data: &[u8]) -> Dissolution {
        let byte = |i: usize| data.get(i).copied().unwrap_or(0);
        let count = 2 + usize::from(byte(24)) % 7;
        let mode = byte(26);
        let width = if mode & 8 != 0 {
            5
        } else {
            8
        };
        let mut at = 27;
        let mut number = || {
            let value = signed_le(data, at, width);
            at += width;
            value
        };
        let penalty = number();
        let amounts: Vec<i64> = (0..count).map(|_| number()).collect();
        let outputs = count - 1 + usize::from(mode & 4 != 0);
        let values = (0..outputs).map(|_| number()).collect();
        Dissolution {
            amounts,
            actor: usize::from(byte(25)) % count,
            unanimous: mode & 1 != 0,
            early: mode & 2 != 0,
            penalty,
            values,
        }
    }

    /// Core's steps 6-12 (`specialtxman.cpp` rc.4:1766-1813) in checked arithmetic: `None` where
    /// a `CAmount` sum or difference Core computes on the way to its verdict overflows.
    fn reference(&self) -> Option<Verdict> {
        let required = if self.unanimous || !self.early {
            0
        } else {
            self.penalty
        };
        let non_actor = || self.amounts.iter().enumerate().filter(|&(i, _)| i != self.actor);
        let mut non_actor_total = 0i64;
        for (_, &amount) in non_actor() {
            non_actor_total = non_actor_total.checked_add(amount)?;
        }
        let mut bonus_total = 0i64;
        for ((_, &amount), &value) in non_actor().zip(&self.values) {
            let bonus = value.checked_sub(amount)?;
            let Some(floor) = floor_reference(required, amount, non_actor_total) else {
                return Some(Err((
                    TxValidationResult::Consensus,
                    FAILED_CHECK_SPECIAL_TX.to_owned(),
                )));
            };
            if bonus < floor {
                return Some(special(BAD_PRODIS_PENALTY_FLOOR));
            }
            bonus_total = bonus_total.checked_add(bonus)?;
        }
        if self.values.len() == self.amounts.len() && self.values.last() == Some(&0) {
            return Some(special(BAD_PRODIS_ACTOR_OUTPUT_ZERO));
        }
        if bonus_total < required {
            return Some(special(BAD_PRODIS_PENALTY_SUM));
        }
        let mut out_total = 0i64;
        for &value in &self.values {
            out_total = out_total.checked_add(value)?;
        }
        let fee = non_actor_total.checked_add(self.amounts[self.actor])?.checked_sub(out_total)?;
        if fee > PRODIS_MAX_FEE {
            return Some(special(BAD_PRODIS_FEE));
        }
        if !self.unanimous && bonus_total > self.penalty {
            return Some(special(BAD_PRODIS_BONUS));
        }
        Some(Ok(()))
    }

    /// The shared masternode: a version-3 state (shares exist only there) with one core address,
    /// registered at 0 with an early period of 10 blocks, each share refunding to its own script.
    #[allow(clippy::expect_used)] // a constant wire form
    fn masternode(&self) -> Arc<Dmn> {
        // ExtNetInfo version 1: one purpose (core P2P) with one IPv4 service, 1.2.3.4:19899.
        let mut wire: &[u8] = &[1, 1, 0, 1, 1, 1, 4, 1, 2, 3, 4, 0x4d, 0xbb];
        let net_info = NetInfo::Extended(NetInfoV2::decode(&mut wire).expect("an ExtNetInfo"));
        let shares = self
            .amounts
            .iter()
            .enumerate()
            .map(|(i, &amount)| CollateralShare {
                amount,
                script_refund: refund(i).into(),
                script_reward: Vec::new().into(),
                key_id_owner: [0x40 + i as u8; 20].into(),
            })
            .collect();
        dmn_with(1, MnType::Regular, |_, c| {
            c.version = 3;
            c.net_info = Arc::new(net_info);
            c.registered_height = 0;
            c.key_id_owner = [0; 20].into();
            c.early_period_blocks = 10;
            c.early_penalty = self.penalty;
            c.shares = shares;
        })
    }

    /// Ours: the dissolution, at height 5 (early) or 20, against a list holding the masternode.
    fn ours(&self) -> Result<Verdict, Divergence> {
        let dmn = self.masternode();
        let mut list = MnList::new(BlockHash::from([0; 32]), 100, 1);
        list.try_add(dmn.clone(), true).map_err(|_| Divergence {
            field: "the list refuses the masternode",
        })?;
        let refunds = (0..self.amounts.len()).filter(|&i| i != self.actor).chain([self.actor]);
        let outputs = refunds
            .zip(&self.values)
            .map(|(i, &value)| TxOut {
                value,
                script_pubkey: refund(i).into(),
            })
            .collect();
        let sig = CompactSig::decode(&mut &[0; COMPACT_SIG_LEN][..]).map_err(|_| Divergence {
            field: "signature",
        })?;
        let p = ProDisTx {
            version: 1,
            pro_tx_hash: TxHash::from(*pro_tx(1).as_bytes()),
            actor_index: self.actor as u16,
            sigs: vec![
                sig;
                if self.unanimous {
                    self.amounts.len()
                } else {
                    1
                }
            ],
        };
        let tx = Tx::new(Transaction {
            version: 3,
            tx_type: TxType::ProviderDissolve,
            inputs: vec![TxIn {
                prevout: dmn.collateral,
                script_sig: Vec::new().into(),
                sequence: u32::MAX,
            }],
            outputs,
            lock_time: 0,
            extra_payload: Vec::new(),
        });
        let height = if self.early {
            5
        } else {
            20
        };
        Ok(check_pro_dis_tx_for_list(&tx, &p, &list, height, false)
            .map_err(|e: TxValidationError| (e.result(), e.reject_reason().to_owned())))
    }
}

/// Share `i`'s refund script: pay-to-pubkey-hash of `[0x20 + i; 20]`.
fn refund(i: usize) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 20];
    script.extend([0x20 + i as u8; 20]);
    script.extend([0x88, 0xac]);
    script
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let word = |i: usize| signed_le(data, 8 * i, 8);
    let (total, weight, weight_total) = (word(0), word(1), word(2));
    ensure(
        pro_rata_floor(total, weight, weight_total) == floor_reference(total, weight, weight_total),
        "ProRataFloor",
    )?;

    let dissolution = Dissolution::read(data);
    let ours = dissolution.ours()?;
    ensure(dissolution.ours()? == ours, "determinism")?;
    if let Some(expected) = dissolution.reference() {
        ensure(ours == expected, "dissolution verdict")?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a failed read of a seed is the failure
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn seeds() -> Vec<(String, Vec<u8>)> {
        let dir = format!("{}/seeds/prodis_arith", env!("CARGO_MANIFEST_DIR"));
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// The reference's verdict, as a name.
    fn verdict(data: &[u8]) -> String {
        match Dissolution::read(data).reference() {
            None => "undefined".to_owned(),
            Some(Ok(())) => "accept".to_owned(),
            Some(Err((_, reason))) => reason,
        }
    }

    /// `ProRataFloor` at the edges: products past 64 bits, amounts read as `uint64_t`, a zero
    /// divisor.
    #[test]
    fn floor_edges() {
        let cases = [
            (i64::MAX, i64::MAX, i64::MAX, Some(i64::MAX)),
            (i64::MAX, 4, 2, Some(-2)),
            (-1, 2, 2, Some(-1)),
            (1, 1, -1, Some(0)),
            (i64::MIN, -1, -1, Some(i64::MIN)),
            (5, 7, 0, None),
        ];
        for (total, weight, weight_total, expected) in cases {
            assert_eq!(floor_reference(total, weight, weight_total), expected);
            assert_eq!(pro_rata_floor(total, weight, weight_total), expected);
        }
    }

    /// Each seed holds, and its name is the reference's verdict (seeds named for the floor leg
    /// end in a zero divisor or, for 8-byte amounts, an overflow).
    #[test]
    fn seeds_hold_and_reach_each_verdict() {
        let mut seen = BTreeMap::new();
        for (name, data) in seeds() {
            assert_eq!(check(&data), Ok(()), "{name}");
            *seen.entry(verdict(&data)).or_insert(0) += 1;
        }
        let expected = [
            "accept",
            BAD_PRODIS_PENALTY_FLOOR,
            BAD_PRODIS_ACTOR_OUTPUT_ZERO,
            BAD_PRODIS_PENALTY_SUM,
            BAD_PRODIS_FEE,
            BAD_PRODIS_BONUS,
            FAILED_CHECK_SPECIAL_TX,
            "undefined",
        ];
        for v in expected {
            assert!(seen.contains_key(v), "no seed gives {v}: {seen:?}");
        }
    }

    /// Byte flips of the seeds and random inputs (a fixed xorshift), so a plain `cargo test`
    /// covers the neighbourhood the fuzzer starts from; most have a defined reference.
    #[test]
    fn mutated_and_random_inputs_hold() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut seen = BTreeMap::new();
        for (name, seed) in seeds() {
            for _ in 0..2000 {
                let mut data = seed.clone();
                for _ in 0..1 + next() % 3 {
                    let at = (next() % data.len() as u64) as usize;
                    data[at] = next() as u8;
                }
                assert_eq!(check(&data), Ok(()), "{name}: {data:02x?}");
                *seen.entry(verdict(&data)).or_insert(0) += 1;
            }
        }
        for _ in 0..20_000 {
            let len = 24 + (next() % 80) as usize;
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            assert_eq!(check(&data), Ok(()), "{data:02x?}");
            *seen.entry(verdict(&data)).or_insert(0) += 1;
        }
        let undefined = seen.get("undefined").copied().unwrap_or(0);
        let total: i32 = seen.values().sum();
        assert!(undefined * 2 < total, "{seen:?}");
        assert!(seen.len() >= 7, "{seen:?}");
    }
}

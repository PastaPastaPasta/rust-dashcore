//! Payee and projection tests (EV2 §6.2): the indexed path against the literal scans, and the
//! reviewer fixtures where only the scan is Core's answer.

use std::sync::Arc;

use super::*;
use crate::MnType;
use crate::scan::{payee_scan, projected_scan};
use crate::testkit::{dmn_with, pro_tx};
use crate::types::MnHot;

const PRE_RR: PayeeRules = PayeeRules {
    v19: true,
    mn_rr: false,
};
const RR: PayeeRules = PayeeRules {
    v19: true,
    mn_rr: true,
};

fn mn(n: u8, mn_type: MnType, f: impl FnOnce(&mut MnHot, &mut i32)) -> Arc<Dmn> {
    dmn_with(n, mn_type, |dmn, cold| {
        let mut registered = cold.registered_height;
        f(&mut dmn.hot, &mut registered);
        cold.registered_height = registered;
    })
}

fn list(height: i32, mns: impl IntoIterator<Item = Arc<Dmn>>) -> MnList {
    let mut list = MnList::default_pre_dip3().with_height(height);
    for dmn in mns {
        list.try_add(dmn, true).unwrap();
    }
    list
}

fn ids(v: &[Arc<Dmn>]) -> Vec<u8> {
    v.iter().map(|d| (1..=255u8).find(|n| pro_tx(*n) == d.pro_tx_hash).unwrap()).collect()
}

fn assert_paths_agree(list: &MnList) {
    for rules in [PRE_RR, RR, PayeeRules::default()] {
        let fast = list.payee(rules).map(|d| d.pro_tx_hash);
        let scan = payee_scan(list, rules).map(|d| d.pro_tx_hash);
        assert_eq!(fast, scan, "payee, {rules:?}, {list:?}");
        for n in [0, 1, 5, 40, 1000] {
            let fast: Vec<_> =
                list.projected_payees(rules, n).iter().map(|d| d.pro_tx_hash).collect();
            let scan: Vec<_> =
                projected_scan(list, rules, n).iter().map(|d| d.pro_tx_hash).collect();
            assert_eq!(fast, scan, "projection of {n}, {rules:?}");
        }
    }
}

/// Quirk 13 (Core's vectors: `tests/quirks_ev2b.rs`): ties on the pay height break by memcmp; an
/// unconfirmed masternode is payable; an all-banned list has no payee.
#[test]
fn ties_unconfirmed_and_all_banned() {
    let tied = list(200, (1..=6).map(|n| mn(n, MnType::Regular, |h, _| h.last_paid_height = 150)));
    let first = tied.payee(PRE_RR).unwrap();
    assert_eq!(first.pro_tx_hash, *tied.iter().map(|d| &d.pro_tx_hash).min().unwrap());
    assert!(first.cold.confirmed_hash.is_null());
    assert_paths_agree(&tied);

    let banned = list(200, (1..=3).map(|n| mn(n, MnType::Regular, |h, _| h.ban_if_not_banned(10))));
    assert!(banned.payee(PRE_RR).is_none());
    assert!(banned.projected_payees(PRE_RR, 10).is_empty());
    assert!(MnList::default_pre_dip3().payee(PRE_RR).is_none());
}

/// An evonode paid at the list height with fewer than four payments is paid again before MN_RR
/// and projected with its remaining payments first.
#[test]
fn evonode_continuation_before_mn_rr() {
    let l = list(
        300,
        [
            mn(1, MnType::Evo, |h, _| {
                h.last_paid_height = 300;
                h.consecutive_payments = 2;
            }),
            mn(2, MnType::Regular, |h, _| h.last_paid_height = 10),
            mn(3, MnType::Evo, |h, _| h.last_paid_height = 20),
        ],
    );
    assert!(l.fast_payee_ok());
    assert_eq!(l.payee(PRE_RR).unwrap().pro_tx_hash, pro_tx(1));
    assert_eq!(l.payee(RR).unwrap().pro_tx_hash, pro_tx(2));
    // Remaining 2, then by pay height: 2 (once), 3 (four times), 1's two paid copies.
    assert_eq!(ids(&l.projected_payees(PRE_RR, 100)), [1, 1, 2, 3, 3, 3, 3, 1, 1]);
    assert_eq!(ids(&l.projected_payees(RR, 100)), [2, 3, 1]);
    assert_paths_agree(&l);
}

/// The reviewer's two-Evo fixture (EV2 §3.4, rev3): two valid evonodes paid at the list height
/// break P1, so the indexed path is not taken; the literal scan pays the last one in Core's
/// order and leaves only the earlier one's copies unsorted at the front, as Core does.
#[test]
fn two_evo_candidates_take_the_literal_scan() {
    let l = list(
        100,
        [
            mn(1, MnType::Evo, |h, _| h.last_paid_height = 100),
            mn(2, MnType::Evo, |h, _| h.last_paid_height = 100),
            mn(3, MnType::Regular, |h, _| h.last_paid_height = 1),
        ],
    );
    assert!(!l.fast_payee_ok());
    let order = l.core_order();
    let (first, last) = if order.iter().position(|p| *p == pro_tx(1))
        < order.iter().position(|p| *p == pro_tx(2))
    {
        (1, 2)
    } else {
        (2, 1)
    };
    assert_eq!(l.payee(PRE_RR).unwrap().pro_tx_hash, pro_tx(last));
    let projected = ids(&l.projected_payees(PRE_RR, 100));
    // Core sorts from the last candidate's remaining count (4), so only the first candidate's
    // four copies stay at the front; the last's copies are sorted with the rest. Sorted: 3 (paid
    // at 1), then the candidate with the smaller memcmp key, four times; truncated to the
    // weighted count (9).
    let key = |n| l.get(&pro_tx(n)).unwrap().payee_key();
    let smaller = if key(1) < key(2) {
        1
    } else {
        2
    };
    assert_eq!(projected, [first, first, first, first, 3, smaller, smaller, smaller, smaller]);
}

/// Streaks far below zero (Core's reader accepts any): every run is capped at the projection's
/// length (D6), so the projection is Core's first `count` entries, the first candidate's copies
/// (its run, at least `count` long, fills Core's unsorted prefix), without a huge vector.
#[test]
fn negative_streaks_are_capped_at_the_count() {
    let l = list(
        100,
        [
            mn(1, MnType::Evo, |h, _| {
                h.last_paid_height = 100;
                h.consecutive_payments = i32::MIN;
            }),
            mn(2, MnType::Evo, |h, _| {
                h.last_paid_height = 100;
                h.consecutive_payments = i32::MIN;
            }),
            mn(3, MnType::Regular, |h, _| h.last_paid_height = 1),
        ],
    );
    assert!(!l.fast_payee_ok());
    let order = l.core_order();
    let first = if order.iter().position(|p| *p == pro_tx(1))
        < order.iter().position(|p| *p == pro_tx(2))
    {
        1
    } else {
        2
    };
    assert_eq!(ids(&l.projected_payees(PRE_RR, 6)), [first; 6]);
}

/// The future-revival fixture: an evonode paid at the list height with a revival height above it
/// has a queue key above the height and is still the continuation (`E E E R E` shape).
#[test]
fn future_revival_is_found_by_the_unbounded_range() {
    let l = list(
        100,
        [
            mn(1, MnType::Evo, |h, _| {
                h.last_paid_height = 100;
                h.consecutive_payments = 1;
                h.pose_revived_height = 101;
            }),
            mn(2, MnType::Regular, |h, _| h.last_paid_height = 50),
        ],
    );
    assert!(l.fast_payee_ok());
    assert_eq!(l.payee(PRE_RR).unwrap().pro_tx_hash, pro_tx(1));
    assert_eq!(ids(&l.projected_payees(PRE_RR, 100)), [1, 1, 1, 2, 1]);
    assert_paths_agree(&l);
}

/// P2: a height-0 list with negative registration heights takes the scan.
#[test]
fn negative_registrations_at_height_zero_take_the_scan() {
    let l = list(0, [mn(1, MnType::Evo, |_, r| *r = -2), mn(2, MnType::Regular, |_, r| *r = -1)]);
    assert!(!l.fast_payee_ok());
    assert_paths_agree(&l);
}

/// The indexed path equals the scan on random lists (the property behind P1 and P2).
#[test]
fn indexed_path_equals_the_scan_on_random_lists() {
    let mut seed = 0x5eed_u64;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for round in 0..400 {
        let height = [0, 1, 50, 100][round % 4];
        let n = 1 + next(12) as u8;
        let mns: Vec<_> = (1..=n)
            .map(|i| {
                let evo = next(3) == 0;
                let paid = [0, height, height - 1, 20, height + 1][next(5) as usize];
                let revived = [-1, -1, height, height + 1, 5][next(5) as usize];
                let streak = next(6) as i32 - 1;
                let banned = next(5) == 0;
                let registered = [0, 3, height, -2][next(4) as usize];
                mn(
                    i,
                    if evo {
                        MnType::Evo
                    } else {
                        MnType::Regular
                    },
                    |h, r| {
                        h.last_paid_height = paid;
                        h.pose_revived_height = revived;
                        h.consecutive_payments = streak;
                        if banned {
                            h.ban_if_not_banned(1);
                        }
                        *r = registered;
                    },
                )
            })
            .collect();
        assert_paths_agree(&list(height, mns));
    }
}

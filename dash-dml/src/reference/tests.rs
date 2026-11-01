use std::sync::Arc;

use dash_num::Hash256;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{CollateralShare, NetInfo, NetInfoV2};

use super::*;
use crate::MnType;
use crate::testkit::{dmn_with, fresh, numbered, operator_key, pro_tx};

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Masternodes that share one 64-bit hash (EV2 §2.7), apart from [`numbered`]'s ids.
fn colliding(n: u8) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |dmn, _| {
        let mut bytes = [7u8; 32];
        bytes[31] = n;
        dmn.pro_tx_hash = ProTxHash::from(bytes);
        dmn.internal_id = 1_000 + u64::from(n);
    })
}

/// Unconfirmed masternodes registered across the heights step 7 confirms at.
fn unconfirmed(n: u32) -> Arc<Dmn> {
    let base = numbered(100 + n);
    let mut cold = (*base.cold).clone();
    cold.registered_height = 1_000_000 + (n as i32 * 7) % 60;
    cold.confirmed_hash = Default::default();
    cold.confirmed_hash_with_pro_tx = Default::default();
    Arc::new(Dmn {
        cold: Arc::new(fresh(cold)),
        ..(*base).clone()
    })
}

/// Shared masternodes (EV2b D23) whose share owners overlap: each holds owners `n` and `n + 1`.
fn shared(n: u8) -> Arc<Dmn> {
    let share = |b: u8| CollateralShare {
        amount: 1,
        script_refund: Default::default(),
        script_reward: Default::default(),
        key_id_owner: [0x40 + b; 20].into(),
    };
    dmn_with(60 + n, MnType::Regular, |dmn, cold| {
        dmn.internal_id = 2_000 + u64::from(n);
        cold.version = 3;
        cold.net_info = Arc::new(NetInfo::Extended(NetInfoV2 {
            version: 1,
            entries: Vec::new(),
        }));
        cold.shares = vec![share(n), share(n + 1)];
    })
}

fn pool() -> Vec<Arc<Dmn>> {
    (0..40)
        .map(numbered)
        .chain((1..=5).map(colliding))
        .chain((0..12).map(unconfirmed))
        .chain((0..4).map(shared))
        .collect()
}

/// One random operation on both lists; the results must be equal.
fn step(rng: &mut Rng, pool: &[Arc<Dmn>], fast: &mut MnList, twin: &mut RefMnList) {
    let pick = |rng: &mut Rng| pool[rng.below(pool.len() as u64) as usize].pro_tx_hash;
    let height = fast.height();
    match rng.below(9) {
        0 | 1 => {
            let dmn = Arc::clone(&pool[rng.below(pool.len() as u64) as usize]);
            let bump = rng.below(2) == 0;
            let a = BuildList::try_add(fast, Arc::clone(&dmn), bump);
            assert_eq!(a, twin.try_add(dmn, bump));
        }
        2 => {
            let pro_tx = pick(rng);
            assert_eq!(BuildList::remove(fast, &pro_tx), twin.remove(&pro_tx));
        }
        3 => {
            let pro_tx = pick(rng);
            let Some(mut hot) = fast.get(&pro_tx).map(|d| d.hot) else {
                assert!(ScanList::get(twin, &pro_tx).is_none());
                return;
            };
            match rng.below(4) {
                0 => hot.pose_penalty = rng.below(4) as i32,
                1 => hot.ban_if_not_banned(height),
                2 => hot.revive(height),
                _ => {
                    hot.last_paid_height = height - 1 + rng.below(3) as i32;
                    hot.consecutive_payments = rng.below(5) as i32;
                }
            }
            assert_eq!(BuildList::update_hot(fast, &pro_tx, hot), twin.update_hot(&pro_tx, hot));
        }
        4 => {
            let pro_tx = pick(rng);
            let Some(dmn) = fast.get(&pro_tx).cloned() else {
                return;
            };
            let other = &pool[rng.below(pool.len() as u64) as usize];
            let mut cold = (*dmn.cold).clone();
            match rng.below(3) {
                // The owner key of another masternode: a duplicate unless it is gone.
                0 => cold.key_id_owner = other.cold.key_id_owner,
                1 if cold.version >= 2 => {
                    cold.pub_key_operator = operator_key(rng.below(6) as u8, BlsSchemeKind::Basic)
                }
                _ => cold.net_info = Arc::clone(&other.cold.net_info),
            }
            cold.reset_derived();
            let cold = Arc::new(cold);
            let a = BuildList::update(fast, &pro_tx, dmn.hot, Arc::clone(&cold));
            assert_eq!(a, twin.update(&pro_tx, dmn.hot, cold));
        }
        5 => {
            let pro_tx = pick(rng);
            let penalty = BuildList::calc_penalty(fast, 66);
            assert_eq!(penalty, twin.calc_penalty(66));
            let a = BuildList::pose_punish(fast, &pro_tx, penalty, &mut crate::NoTrace);
            assert_eq!(a, twin.pose_punish(&pro_tx, penalty, &mut crate::NoTrace));
        }
        6 => assert_eq!(BuildList::decrease_scores(fast), twin.decrease_scores()),
        7 => {
            let (prev_fast, prev_twin) = (fast.clone(), twin.clone());
            *fast = BuildList::start_build(&prev_fast, height + 1);
            *twin = prev_twin.start_build(height + 1);
            let confirmed = Hash256::from([height as u8; 32]);
            let min_conf = rng.below(3) as i32;
            // The unconfirmed masternodes register from 1,000,000 to 1,000,059: some are old
            // enough to confirm, some are not.
            let prev_height = 1_000_000 + rng.below(70) as i32;
            let a = fast.confirm(&prev_fast, prev_height, min_conf, &confirmed);
            assert_eq!(a, twin.confirm(&prev_twin, prev_height, min_conf, &confirmed));
        }
        _ => {
            let mn_rr = rng.below(2) == 0;
            let payee = (rng.below(4) != 0).then(|| pick(rng));
            let a = BuildList::reset_streaks(fast, payee, mn_rr, &mut crate::NoTrace);
            assert_eq!(a, twin.reset_streaks(payee, mn_rr, &mut crate::NoTrace));
        }
    }
}

/// EV2 §6.2 `mnlist_model`: random operations on the fast list and the twin, collisions,
/// confirmations and shared masternodes included, compared by [`xcheck`] after every step.
#[test]
fn the_fast_list_matches_the_twin_under_random_operations() {
    let pool = pool();
    let (mut confirmed, mut shared) = (0, 0);
    for seed in 0..40u64 {
        let mut rng = Rng(seed);
        // Decoded: the random hot states break the premises I-P rests on (EV2 rev4).
        let mut fast = MnList::new(BlockHash::from([0; 32]), 1_000_020, 0);
        let mut twin = RefMnList::from_list(&fast).expect("an empty list");
        for i in 0..300 {
            let (len, unconfirmed) = (fast.len(), fast.unconfirmed().count());
            step(&mut rng, &pool, &mut fast, &mut twin);
            // Only a confirmation drops the count while the masternodes stay.
            if fast.len() == len && fast.unconfirmed().count() < unconfirmed {
                confirmed += 1;
            }
            shared += fast.iter().filter(|d| d.cold.is_shared()).count();
            let rules = PayeeRules {
                v19: rng.below(2) == 0,
                mn_rr: rng.below(2) == 0,
            };
            if let Err(e) = xcheck(&fast, &twin, rules) {
                panic!("seed {seed} step {i}: {e}");
            }
        }
        // A twin of the fast list equals the twin that followed it.
        let copy = RefMnList::from_list(&fast).expect("I-U holds");
        assert!(copy.unique == twin.unique && copy.buckets == twin.buckets, "seed {seed}");
    }
    assert!(confirmed > 0 && shared > 0, "confirmations {confirmed}, shared {shared}");
}

/// The twin's buckets follow immer: a set moves a key to the front, an erase keeps the rest's
/// order, a single survivor leaves the bucket (the fast list's `collision_buckets_follow_immer`).
#[test]
fn twin_buckets_follow_immer() {
    let key = |n: u8| colliding(n).pro_tx_hash;
    let hash = key(1).immer_hash();
    let bucket = |twin: &RefMnList| twin.buckets.get(&hash).map(|b| Vec::from((**b).clone()));
    let mut twin = RefMnList::from_list(&MnList::default_pre_dip3()).expect("an empty list");
    for n in 1..=3 {
        twin.try_add(colliding(n), true).expect("addable");
    }
    assert_eq!(bucket(&twin), Some(vec![key(3), key(2), key(1)]));
    let hot = MnHot {
        pose_penalty: 1,
        ..MnHot::default()
    };
    twin.update_hot(&key(1), hot).expect("present");
    assert_eq!(bucket(&twin), Some(vec![key(1), key(3), key(2)]));
    twin.remove(&key(3)).expect("present");
    assert_eq!(bucket(&twin), Some(vec![key(1), key(2)]));
    twin.remove(&key(1)).expect("present");
    assert_eq!(bucket(&twin), None);
}

/// A failed `AddMN` restores the unique map: the twin is unchanged.
#[test]
fn a_failed_add_leaves_the_twin_unchanged() {
    let mut twin = RefMnList::from_list(&MnList::default_pre_dip3()).expect("an empty list");
    twin.try_add(numbered(1), true).expect("addable");
    let before = twin.unique.clone();
    let clash = dmn_with(2, MnType::Regular, |_, cold| {
        cold.key_id_owner = numbered(1).cold.key_id_owner;
    });
    assert_eq!(twin.try_add(clash, true), Err(MnListError::DuplicateOwner(pro_tx(2))));
    assert!(twin.unique == before && twin.len() == 1);
}

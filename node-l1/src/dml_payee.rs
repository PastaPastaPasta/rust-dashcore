//! `mnlist_payee_queue` (EV2 §6.2; EV2b acceptance): `MnList::payee` and
//! `MnList::projected_payees` against Core's `GetMNPayee` and `GetProjectedMNPayees`
//! (`oracle_mnlist_payee`, `oracle_mnlist_projected`) on lists built by a sequence of
//! additions, hot-field updates and removals, so the payee queue and the streak index are the
//! incrementally maintained ones. The lists have heavy ties, evonode streaks (negative ones
//! included), past and future revivals, future payments and registrations, negative
//! registration heights and all-banned states: some keep P1 and P2 and take the range walk,
//! the others take `scan.rs`. The same list read back through our canonical reader (a
//! `Decoded` list, which computes P1 and P2 itself) must give the same answers.
//!
//! The eras: mainnet before V19 and its V19 activation block, and regtest (V19 at 1, MN_RR at
//! 432) at heights 0 to 2, at 100 on, at the MN_RR activation block and after it.

use std::sync::OnceLock;

use dash_dml::testkit::dmn_with;
use dash_dml::{MnHot, MnList, MnType, PayeeRules};
use dash_oracle_sys::{Chain, ErrorKind, ListEncoding, Oracle, Pin};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::BlockHash;

use crate::dml::{
    canonical, canonical_refusals, mainnet, order_key, payee_rules, read_canonical, spread,
};
use crate::{Compared, Divergence, Outcome, check, divergence};

/// The most operations an input runs.
pub const MAX_OPS: usize = 24;

/// The longest projection asked for: past every list's weighted size (16 evonodes weigh 64).
const MAX_PROJECTION: u8 = 80;

/// A network's parameters and the oracle's chain for it.
struct Net {
    params: ChainSetup,
    chain: fn() -> Chain,
}

/// Mainnet and regtest.
#[allow(clippy::expect_used)] // Core's parameters, constants, always build
fn nets() -> &'static (Net, Net) {
    static NETS: OnceLock<(Net, Net)> = OnceLock::new();
    NETS.get_or_init(|| {
        let regtest = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .expect("Core's parameters");
        (
            Net {
                params: mainnet().clone(),
                chain: Chain::main,
            },
            Net {
                params: regtest,
                chain: Chain::regtest,
            },
        )
    })
}

/// The network and list height (`pindexPrev`'s) of era `era`, `offset` into it.
fn era(era: u8, offset: u8) -> (&'static Net, i32) {
    let (main, regtest) = nets();
    let v19 = main.params.consensus.v19_height.to_u32() as i32;
    let mn_rr = regtest.params.consensus.mn_rr_height.to_u32() as i32;
    let offset = i32::from(offset);
    match era % 6 {
        0 => (main, v19 - 400_000 + offset),
        // The next block activates V19.
        1 => (main, v19 - 1),
        2 => (regtest, offset % 3),
        3 => (regtest, 100 + offset),
        // The next block activates MN_RR.
        4 => (regtest, mn_rr - 1),
        _ => (regtest, mn_rr + offset),
    }
}

/// The hot fields bytes `paid`, `streak` and `pose` give at list height `h`: few distinct
/// values near `h`, so ties are common.
fn hot(h: i32, paid: u8, streak: u8, pose: u8) -> MnHot {
    let k = i32::from(paid >> 3);
    let mut hot = MnHot::default();
    hot.last_paid_height = match paid % 8 {
        0 => 0,
        1 | 2 => h - k % 4,
        3 => h,
        4 => h + 1,
        5 => h - k,
        6 => h - 1,
        _ => h + 1 + k % 3,
    };
    hot.consecutive_payments = match streak % 8 {
        s @ 0..=3 => i32::from(s),
        4 => 0,
        5 => -1,
        6 => 4,
        _ => i32::from(streak >> 3),
    };
    let p = i32::from(pose >> 2);
    match pose % 4 {
        1 => {
            if pose & 0x40 != 0 {
                hot.revive(h - 5);
            }
            hot.ban_if_not_banned(h - p % 16);
        }
        2 => hot.revive(h - p % 4),
        3 => hot.revive(h + 1 + p % 2),
        _ => {}
    }
    hot
}

/// The registration height byte `reg` gives at list height `h`.
fn registered(h: i32, reg: u8) -> i32 {
    match reg % 8 {
        0 => h - i32::from(reg >> 3) % 4,
        1 => -2,
        2 => -1,
        3 => 0,
        4 => h + 1,
        _ => h - 10 - i32::from(reg >> 3),
    }
}

/// EV2 §6.2's payee fixtures as inputs (`[era, offset, count]`, then
/// `[op, n, paid, reg, streak, pose]` per masternode). At list height h: paid 0 = never,
/// 3 = h, 5 | k << 3 = h - k; reg 0 = h, 1 = -2, 2 = -1, 4 = h + 1, 5 | k << 3 = h - 10 - k;
/// streak s < 4 = s; pose 0 = valid, 1 = banned at h, 3 = revived at h + 1. Evonodes have
/// `n & 0x10`.
pub fn fixtures() -> [(&'static str, Vec<u8>); 5] {
    [
        // Two evonodes paid at h with streaks 1 and 2 (P1 fails: the literal scan).
        ("two-evo", vec![3, 0, 8, 0, 0x12, 3, 0, 1, 0, 0, 0x14, 3, 0, 2, 0]),
        // Height 0, V19 on, MN_RR off: a regular MN registered at -2, an Evo at -1.
        ("height-0", vec![2, 0, 8, 0, 0x01, 0, 1, 0, 0, 0, 0x12, 0, 2, 0, 0]),
        // R regular paid long ago, E Evo paid at h, revived at h + 1, streak 1.
        ("future-revival", vec![3, 0, 5, 0, 0x01, 5 | 99 << 3, 5, 0, 0, 0, 0x12, 3, 5, 1, 3]),
        // E Evo never paid, registered at h + 1, streak 1.
        ("future-registration", vec![2, 0, 5, 0, 0x01, 5 | 1 << 3, 5, 0, 0, 0, 0x12, 0, 4, 1, 0]),
        // Everyone banned: no payee, no projection.
        ("all-banned", vec![3, 7, 8, 0, 0x01, 6, 5, 0, 1, 0, 0x12, 3, 5, 0, 1]),
    ]
}

/// One `mnlist_payee_queue` input, built.
pub struct Built {
    net: &'static Net,
    /// The list's height (`pindexPrev`'s).
    pub height: i32,
    pub rules: PayeeRules,
    /// The projection length.
    pub count: u8,
    pub list: MnList,
}

/// `mnlist_payee_queue`'s list. Byte 0 picks the era (mod 6), byte 1 the offset into it, byte 2
/// the projection length (mod 81); then each 6 bytes `[op, n, paid, reg, streak, pose]` are one
/// operation on masternode `n % 16 + 1` (`diff_mnlist_order`'s keys: 1 to 4 and 5 to 8 share
/// their 64-bit hashes). `op` mod 4: 0 and 1 add it (Evo when `n & 0x10`, registered at
/// `registered(reg)`) or, when present, update its hot fields; 2 updates them; 3 removes it.
/// An update or removal of an absent masternode is skipped. `Err` is our list refusing an
/// operation or failing its invariants, which none of these should.
pub fn build(input: &[u8]) -> Option<Result<Built, Divergence>> {
    let (&[era_byte, offset, count], ops) = input.split_first_chunk::<3>()?;
    let (net, h) = era(era_byte, offset);
    let mut list = MnList::new(BlockHash::from(spread(era_byte, offset, 9)), h, 0);
    for &[op, n, paid, reg, streak, pose] in ops.as_chunks::<6>().0.iter().take(MAX_OPS) {
        let pro_tx = order_key(n % 16 + 1);
        let present = list.get(&pro_tx).is_some();
        let hot = hot(h, paid, streak, pose);
        let applied = match op % 4 {
            0 | 1 if !present => {
                let mn_type = if n & 0x10 != 0 {
                    MnType::Evo
                } else {
                    MnType::Regular
                };
                let id = u64::from(list.total_registered());
                let dmn = dmn_with(n % 16 + 1, mn_type, |d, cold| {
                    d.pro_tx_hash = pro_tx;
                    d.internal_id = id;
                    d.hot = hot;
                    cold.registered_height = registered(h, reg);
                });
                list.try_add(dmn, true)
            }
            0..=2 if present => list.update_hot(&pro_tx, hot),
            3 if present => list.remove(&pro_tx),
            _ => Ok(()),
        };
        if applied.is_err() {
            return Some(Err(divergence("list operation")));
        }
    }
    if list.check_invariants().is_err() {
        return Some(Err(divergence("invariants")));
    }
    Some(Ok(Built {
        net,
        height: h,
        rules: payee_rules(&net.params, h),
        count: count % (MAX_PROJECTION + 1),
        list,
    }))
}

/// Whether `list` holds D6's evonode, the shim's refusal condition: before MN_RR, a valid
/// evonode paid at height `h` (so it would continue its streak) with a negative streak.
fn negative_streak(list: &MnList, h: i32, rules: &PayeeRules) -> bool {
    !rules.mn_rr
        && list
            .iter_valid()
            .any(|d| d.is_evo() && d.hot.last_paid_height == h && d.hot.consecutive_payments < 0)
}

/// `mnlist_payee_queue` (see [`build`]).
pub fn compare_payee_queue(input: &[u8], pin: Pin) -> Compared {
    let Some(built) = build(input) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Built {
        net,
        height: h,
        rules,
        count,
        list,
    } = built?;
    let oracle = Oracle::new(pin);
    let chain = (net.chain)();
    let bytes = canonical(&list);
    let core_payee = match oracle.mnlist_payee(&chain, ListEncoding::Canonical, &bytes, h) {
        Ok(payee) => payee,
        // Core's reader refuses what ours must refuse too (a registration below -1, say).
        Err(core) => return canonical_refusals(read_canonical(&bytes), &core),
    };
    // The shim refuses a projection that would repeat an evonode a negative number of times
    // (EV2b D6; Core would size its vector from it): only the payee is compared then. Any other
    // refusal is a divergence.
    let core_projected =
        match oracle.mnlist_projected(&chain, ListEncoding::Canonical, &bytes, h, i32::from(count))
        {
            Ok(projected) => Some(projected),
            Err(core) if core.kind == ErrorKind::Argument && negative_streak(&list, h, &rules) => {
                None
            }
            Err(_) => return Err(divergence("projection verdict")),
        };
    let read = read_canonical(&bytes).map_err(|_| divergence("decode verdict"))?;

    // Canonical bytes carry no collision-bucket history, so Core iterates its read list (and
    // ours) in the reader's bucket order; the built list is compared only when its order is
    // the same (the continuing evonode is the last one met in that order, `scan.rs`).
    let lists: &[&MnList] = if list.core_order() == read.core_order() {
        &[&list, &read]
    } else {
        &[&read]
    };
    for list in lists {
        let payee = list.payee(rules).map(|d| *d.pro_tx_hash.as_bytes());
        check(payee == core_payee, "payee")?;
        if let Some(core_projected) = &core_projected {
            let projected: Vec<[u8; 32]> = list
                .projected_payees(rules, usize::from(count))
                .iter()
                .map(|d| *d.pro_tx_hash.as_bytes())
                .collect();
            check(projected == *core_projected, "projected payees")?;
        }
    }
    Ok(if core_projected.is_some() {
        Outcome::BothAccept
    } else {
        Outcome::OutOfDomain
    })
}

/// Both sides of one input, for triage.
pub fn explain(input: &[u8]) -> String {
    let Some(built) = build(input) else {
        return String::from("too short");
    };
    let built = match built {
        Ok(built) => built,
        Err(d) => return format!("ours refused: {d}"),
    };
    let short = |p: &[u8; 32]| format!("{:02x}{:02x}", p[31], p[0]);
    let mut out = format!("height {} {:?} count {}\n", built.height, built.rules, built.count);
    for d in built.list.iter() {
        out += &format!(
            "  {} {:?} reg {} {:?}\n",
            short(d.pro_tx_hash.as_bytes()),
            d.mn_type,
            d.cold.registered_height,
            d.hot
        );
    }
    let oracle = Oracle::new(Pin::V24_0);
    let chain = (built.net.chain)();
    let bytes = canonical(&built.list);
    let core = oracle.mnlist_payee(&chain, ListEncoding::Canonical, &bytes, built.height);
    out += &format!("core payee {:?}\n", core.map(|p| p.as_ref().map(short)));
    let ours = built.list.payee(built.rules).map(|d| short(d.pro_tx_hash.as_bytes()));
    out += &format!("our payee {ours:?}\n");
    if let Ok(read) = read_canonical(&bytes) {
        let read = read.payee(built.rules).map(|d| short(d.pro_tx_hash.as_bytes()));
        out += &format!("read payee {read:?}\n");
    }
    let core = oracle.mnlist_projected(
        &chain,
        ListEncoding::Canonical,
        &bytes,
        built.height,
        i32::from(built.count),
    );
    out += &format!("core projected {:?}\n", core.map(|v| v.iter().map(short).collect::<Vec<_>>()));
    let ours: Vec<String> = built
        .list
        .projected_payees(built.rules, usize::from(built.count))
        .iter()
        .map(|d| short(d.pro_tx_hash.as_bytes()))
        .collect();
    out += &format!("our projected {ours:?}");
    out
}

//! Variant footprints (O10 plan §2.2, Q-10): what a catalogue variant's block sets beyond its
//! own transactions. O10-2's composer (§4.5) puts two variants in one block. Two footprints
//! sharing a field are §4.3's first interaction rule, and the composer refuses them. A variant's
//! own transactions are never a shared field: each component spends its own coins.
//!
//! The table is keyed by variant id, `<op>.<variant>`. A bare `<op>` covers every variant of
//! that operator that has no entry of its own. `every_variant_has_one_footprint` holds the table
//! complete over [`catalogue`]. `footprints_match_the_bytes` checks the fields it can see
//! (header fields, the coinbase, the shared coins, the coinbase's presence) against what each
//! variant's `apply` changes on a template.

use crate::chain::mutate::{Operator, Variant, catalogue};

/// The block's version (v:4525-4530, CCBH's version rows).
pub const HEADER_VERSION: &str = "header.version";
/// The block's time (v:4516, v:4520).
pub const HEADER_TIME: &str = "header.time";
/// The block's `nBits` (v:4504, CBH's proof of work).
pub const HEADER_BITS: &str = "header.bits";
/// The header's merkle root, set apart from the transactions' (v:4395, CheckMerkleRoot).
pub const HEADER_MERKLE: &str = "header.merkle";
/// Any field of the coinbase: its value, script, version, type or payload.
pub const COINBASE: &str = "coinbase";
/// The positions of the block's transactions: a duplicate pair at the end of the list
/// (CheckMerkleRoot's mutation, v:4348-4351).
pub const BLOCK_ORDER: &str = "block.order";
/// The transaction list itself: no coinbase first, or no transaction (v:4405, v:4409).
pub const BLOCK_TXS: &str = "block.txs";
/// A target over the whole block's serialized size (v:4405, v:4564).
pub const BLOCK_SIZE: &str = "block.size";
/// A target over the whole block's sigops (v:4432, v:4585, v:2752).
pub const BLOCK_SIGOPS: &str = "block.sigops";
/// A target over the whole block's fees: the coinbase's bound (v:2857) or their range (v:2729).
pub const BLOCK_FEES: &str = "block.fees";
/// The leg's immature coinbase (at depth 99), one per height.
pub const COIN_IMMATURE: &str = "coin.immature";
/// The leg's mature coinbase (at depth 100), one per height.
pub const COIN_MATURE: &str = "coin.mature";
/// The coin the setup spent before the height (`spent_earlier`'s).
pub const COIN_SPENT: &str = "coin.spent";
/// The setup transactions a `REUSE_SETUP` operator repeats (BIP30's).
pub const COIN_REUSE: &str = "coin.reuse";
/// A named scenario's earlier ops: the parent's or the index's state (a10-5 group S).
pub const CHAIN: &str = "chain";

/// What a variant's block sets beyond its own transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Footprint {
    /// The shared fields it sets (the constants above).
    pub fields: &'static [&'static str],
    /// The guard attributes of the model it changes as a side effect, beyond its own row's path
    /// (§4.3's second rule). A variant's own defect is never one: place() fixes its path's
    /// guards. No catalogue variant has one today: an assumevalid or deployment change is a
    /// leg's or site's, never a variant's.
    pub touches: &'static [&'static str],
}

const fn of(fields: &'static [&'static str]) -> Footprint {
    Footprint {
        fields,
        touches: &[],
    }
}

const NONE: Footprint = of(&[]);

/// The table, in catalogue order.
pub const FOOTPRINTS: &[(&str, Footprint)] = &[
    // OPERATORS (diff_chain's).
    ("cb_amount", of(&[COINBASE, BLOCK_FEES])),
    ("duplicate_tx", of(&[BLOCK_ORDER])),
    ("merkle_root", of(&[HEADER_MERKLE])),
    ("bits_minus", of(&[HEADER_BITS])),
    ("bits_plus", of(&[HEADER_BITS])),
    ("time_at_mtp", of(&[HEADER_TIME])),
    ("time_too_new", of(&[HEADER_TIME])),
    ("version_3", of(&[HEADER_VERSION])),
    ("cb_height", of(&[COINBASE])),
    ("missing_input", NONE),
    ("double_spend", NONE),
    ("duplicate_input", NONE),
    ("premature_coinbase", of(&[COIN_IMMATURE])),
    ("in_below_out", NONE),
    ("vout_negative", NONE),
    ("vout_toolarge", NONE),
    ("script_false", NONE),
    ("sig_not_der", NONE),
    ("sig_bad", NONE),
    ("nulldummy", NONE),
    ("p2sh_false", NONE),
    ("cltv_unmet", NONE),
    ("csv_unmet", NONE),
    ("locktime_height", NONE),
    ("locktime_mtp", NONE),
    ("bip68_height", NONE),
    ("cb_missing", of(&[COINBASE, BLOCK_TXS])),
    ("cb_multiple", NONE),
    ("tx_type_dip3", NONE),
    ("queued_script_then_bip68", NONE),
    // L3_OPERATORS (a10-5's).
    ("control", NONE),
    ("cb_value", of(&[COINBASE, BLOCK_FEES])),
    ("sigops_check", of(&[BLOCK_SIGOPS])),
    ("sigops_ctx", of(&[BLOCK_SIGOPS])),
    ("sigops_connect", of(&[BLOCK_SIGOPS])),
    ("block_size", of(&[BLOCK_SIZE])),
    ("tx_size_100k", NONE),
    ("tx_size_1m", NONE),
    ("payload_size", NONE),
    ("vin_empty", NONE),
    ("vout_empty", NONE),
    ("prevout_null", NONE),
    ("txouttotal", NONE),
    ("cb_length", of(&[COINBASE])),
    ("inputvalues", NONE),
    ("accumulated_fee", of(&[BLOCK_FEES])),
    ("spent_earlier", of(&[COIN_SPENT])),
    ("maturity.99", of(&[COIN_IMMATURE])),
    ("maturity.100", of(&[COIN_MATURE])),
    ("bip30", of(&[COIN_REUSE])),
    ("p2pkh_bad_sig", NONE),
    ("p2sh_bad_sig", NONE),
    ("dersig", NONE),
    ("bip147", NONE),
    ("cltv", NONE),
    ("csv", NONE),
    ("bip68", NONE),
    ("bip113", NONE),
    ("bip113_strict", NONE),
    ("bad_version", of(&[HEADER_VERSION])),
    ("locktime", NONE),
    ("bip68_blocks", NONE),
    ("bip68_time", NONE),
    ("bip68_off", NONE),
    ("tx_version", NONE),
    ("tx_version_type", NONE),
    ("header_version", of(&[HEADER_VERSION])),
    ("special_tx_type", NONE),
    ("special_cb_type", of(&[COINBASE])),
    ("cb_type5", of(&[COINBASE])),
    ("empty_block", of(&[COINBASE, BLOCK_TXS])),
    ("v3_type0", NONE),
    // Group M: two faults each, with the mature coin as their script carrier.
    ("m_missing", of(&[COIN_MATURE])),
    ("m_premature", of(&[COIN_MATURE, COIN_IMMATURE])),
    ("m_in_belowout", of(&[COIN_MATURE])),
    ("m_bip68", of(&[COIN_MATURE])),
    ("m_sigops", of(&[COIN_MATURE, BLOCK_SIGOPS])),
    ("m_inputvalues", of(&[COIN_MATURE])),
    ("m_accumulated_fee", of(&[COIN_MATURE, BLOCK_FEES])),
    ("m_script_cb_amount", of(&[COIN_MATURE, COINBASE, BLOCK_FEES])),
    ("m_script_dip3_gate", of(&[COIN_MATURE])),
    ("m_script_cbtx_payload", of(&[COIN_MATURE, COINBASE])),
    // Group S: scenarios, whose later steps read what the earlier ones left.
    ("s1_mutated_then_honest", of(&[CHAIN, BLOCK_ORDER])),
    ("s2_child_of_invalid", of(&[CHAIN, COINBASE])),
    ("s3_resubmit.failed", of(&[CHAIN, COINBASE])),
    ("s3_resubmit.valid", of(&[CHAIN])),
    ("s3h_failed_header_resubmit", of(&[CHAIN, COINBASE])),
    ("s4_proposal_off_tip", of(&[CHAIN])),
    ("s5_unknown_parent", of(&[CHAIN])),
    ("s6_child_of_unmarked_descendant", of(&[CHAIN, COINBASE])),
];

/// A variant's id in [`FOOTPRINTS`] and in O10's pair ids: `<op>.<variant>`.
pub fn variant_id(op: &Operator, variant: &Variant) -> String {
    format!("{}.{}", op.name, variant.name)
}

/// `variant`'s footprint: its own entry, else its operator's.
pub fn footprint(op: &Operator, variant: &Variant) -> Option<Footprint> {
    let id = variant_id(op, variant);
    FOOTPRINTS
        .iter()
        .find(|(k, _)| *k == id)
        .or_else(|| FOOTPRINTS.iter().find(|(k, _)| *k == op.name))
        .map(|(_, f)| *f)
}

/// The fields two footprints share (§4.3's footprint rule), in `a`'s order.
pub fn shared(a: &Footprint, b: &Footprint) -> Vec<&'static str> {
    a.fields.iter().copied().filter(|f| b.fields.contains(f)).collect()
}

/// Every operator–variant pair of the catalogue.
pub fn variants() -> impl Iterator<Item = (&'static Operator, &'static Variant)> {
    catalogue().flat_map(|op| op.variants.iter().map(move |v| (op, v)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use dash_consensus::script::sigops::legacy_sigop_count;

    use crate::chain::build::{BlockPlan, Input, Lock, REGTEST_BITS, TxPlan, coinbase};
    use crate::chain::mutate::{Cx, L3Cx, MAX_MONEY, Site, max_money_coins, scenario_steps};
    use crate::chain::world::Coin;

    /// The table is complete and has no dead entry: every catalogue variant resolves to exactly
    /// one entry (its own or its operator's), every key names a catalogue operator (and variant),
    /// and no field is outside the vocabulary.
    #[test]
    fn every_variant_has_one_footprint() {
        let vocabulary = [
            HEADER_VERSION,
            HEADER_TIME,
            HEADER_BITS,
            HEADER_MERKLE,
            COINBASE,
            BLOCK_ORDER,
            BLOCK_TXS,
            BLOCK_SIZE,
            BLOCK_SIGOPS,
            BLOCK_FEES,
            COIN_IMMATURE,
            COIN_MATURE,
            COIN_SPENT,
            COIN_REUSE,
            CHAIN,
        ];
        let mut keys = BTreeSet::new();
        for (key, f) in FOOTPRINTS {
            assert!(keys.insert(*key), "{key} repeats");
            let (op, variant) = key.split_once('.').map_or((*key, None), |(o, v)| (o, Some(v)));
            let op =
                catalogue().find(|o| o.name == op).unwrap_or_else(|| panic!("{key}: no operator"));
            if let Some(v) = variant {
                assert!(op.variants.iter().any(|x| x.name == v), "{key}: no variant");
            } else {
                // A bare operator key must cover at least one variant without its own entry.
                assert!(
                    op.variants.iter().any(|v| !keys_has(&variant_id(op, v))),
                    "{key}: every variant has its own entry"
                );
            }
            assert!(f.fields.iter().all(|x| vocabulary.contains(x)), "{key}: {:?}", f.fields);
            assert!(f.touches.is_empty(), "{key}: a catalogue variant touches no guard");
        }
        for (op, v) in variants() {
            assert!(footprint(op, v).is_some(), "{}: no footprint", variant_id(op, v));
        }
    }

    fn keys_has(id: &str) -> bool {
        FOOTPRINTS.iter().any(|(k, _)| *k == id)
    }

    fn coin(tag: u8, height: u32, coinbase: bool, value: i64) -> Coin {
        Coin {
            outpoint: ([tag; 32], 0),
            value,
            lock: Lock::True,
            height,
            coinbase,
        }
    }

    /// The fields the bytes show, from each variant's `apply` on a coinbase-only template at its
    /// first site: header fields changed, the coinbase changed, the leg's shared coins spent
    /// (immature 0x63, mature 0x62, spent 0x61, the reused setup 0x71/0x72), no coinbase first
    /// (or no transaction), a scenario's steps. The table must say exactly these; its targets
    /// (size, sigops, fees, order) are checked one way: a size target builds a block of at least
    /// 999,990 bytes, a legacy sigops target (CheckBlock's, ContextualCheckBlock's) at least
    /// 20,000.
    #[test]
    fn footprints_match_the_bytes() {
        let reuse = vec![
            TxPlan::simple(vec![Input::new(([0x71; 32], 0), vec![])], vec![(5, vec![0x51])]),
            TxPlan::simple(vec![Input::new(([0x72; 32], 0), vec![])], vec![(5, vec![0x51])]),
        ];
        for (op, v) in variants() {
            let id = variant_id(op, v);
            let site = v.sites[0];
            let height = match site {
                Site::SB(h) | Site::At(h) => h,
                Site::P1 => 115,
                Site::BO => 1_170,
                Site::BW => 1_001,
                Site::SO => 1_510,
                Site::Reg => 300,
            };
            let money = max_money_coins(op.name, v.name, site);
            let coins: Vec<Coin> = if money > 0 {
                (0..money).map(|k| coin(0x40 + k as u8, 1_001, true, MAX_MONEY)).collect()
            } else {
                (1..=3).map(|n| coin(n, 101, false, 1_000_000)).collect()
            };
            let l3 = L3Cx {
                trial: *b"trial-a\0",
                carrier: v.carrier,
                variant: v.name,
                site,
                reward: 50_000,
                sb_limit: 900_000,
                spent: ([0x61; 32], 0),
                mature: coin(0x62, height - 100, true, 1_000_000),
                coin_mtp: 900,
                reuse: &reuse,
            };
            let cx = Cx {
                height,
                mtp: 1_000,
                mocktime: 2_000,
                coins: &coins,
                immature: Some(coin(0x63, height - 99, true, 1_000_000)),
                l3: Some(&l3),
            };
            let base = BlockPlan {
                prev: [0xaa; 32],
                version: 0x2000_0000,
                time: 1_500,
                bits: REGTEST_BITS,
                txs: vec![coinbase(height, 50_000, 1)],
                merkle_override: None,
            };
            let mut p = base.clone();
            (op.apply)(&mut p, &cx);
            let spent: BTreeSet<u8> = p
                .txs
                .iter()
                .skip(1)
                .flat_map(|t| &t.inputs)
                .map(|i| i.prevout.0[0])
                .filter(|b| [0x61, 0x62, 0x63, 0x71, 0x72].contains(b))
                .collect();
            let first_is_coinbase = p.txs.first().is_some_and(|t| {
                t.inputs.first().is_some_and(|i| i.prevout == ([0; 32], u32::MAX))
            });
            let mut seen = BTreeSet::new();
            for (field, shows) in [
                (HEADER_VERSION, p.version != base.version),
                (HEADER_TIME, p.time != base.time),
                (HEADER_BITS, p.bits != base.bits),
                (HEADER_MERKLE, p.merkle_override.is_some()),
                (COINBASE, p.txs.first() != base.txs.first()),
                (BLOCK_TXS, !first_is_coinbase),
                (COIN_IMMATURE, spent.contains(&0x63)),
                (COIN_MATURE, spent.contains(&0x62)),
                (COIN_SPENT, spent.contains(&0x61)),
                (COIN_REUSE, spent.contains(&0x71) || spent.contains(&0x72)),
                (CHAIN, scenario_steps(op.name, v.name).is_some()),
            ] {
                if shows {
                    seen.insert(field);
                }
            }
            let f = footprint(op, v).expect("a footprint");
            let declared: BTreeSet<&str> = f
                .fields
                .iter()
                .copied()
                .filter(|x| ![BLOCK_SIZE, BLOCK_SIGOPS, BLOCK_FEES, BLOCK_ORDER].contains(x))
                .collect();
            assert_eq!(declared, seen, "{id}");
            if f.fields.contains(&BLOCK_SIZE) {
                assert!(crate::chain::mutate::block_size(&p) >= 999_990, "{id}");
            }
            // ConnectBlock's target (`sigops_connect`, group M's) counts P2SH sigops too (v:2752).
            if f.fields.contains(&BLOCK_SIGOPS) && ["sigops_check", "sigops_ctx"].contains(&op.name)
            {
                let sigops: usize =
                    p.txs.iter().map(|t| legacy_sigop_count(t.tx().raw()) as usize).sum();
                assert!(sigops >= 20_000, "{id}");
            }
        }
    }
}

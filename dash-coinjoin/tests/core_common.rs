//! Every row Dash Core's `coinjoin/common.h` produced (`data/core_common.json`, from
//! `contrib/node/cj0-vectors/gen_common.cpp`) and every amount text the pinned dashd printed
//! (`data/core_amount_strings.json`), against [`dash_coinjoin::denom`]; and Core's
//! `IsFullyMixed` coin flip (`data/core_fully_mixed.json`, from `gen_fully_mixed.cpp`) against
//! [`dash_coinjoin::rounds::stops_mixing`]. Regenerate them with
//! `contrib/node/cj0-vectors/regen.sh`.

mod common;

use common::{Numeric, outpoint};
use dash_coinjoin::denom::{self, MixShape, MixSideCounts};
use dash_coinjoin::rounds;
use dash_primitives::TxHash;
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    let rows = v[key].as_array().unwrap_or_else(|| panic!("{key} is not an array"));
    assert!(!rows.is_empty(), "{key} is empty");
    rows
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("{v} is not an integer"))
}

fn int32(v: &Value) -> i32 {
    i32::try_from(int(v)).unwrap_or_else(|_| panic!("{v} is not an int"))
}

fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| panic!("{v} is not a bool"))
}

#[test]
fn constants() {
    let v = load("core_common.json");
    let denoms: Vec<i64> = rows(&v, "standard_denominations").iter().map(int).collect();
    assert_eq!(denoms, denom::STANDARD_DENOMINATIONS);
    assert_eq!(int(&v["smallest_denomination"]), denom::SMALLEST_DENOMINATION);
    assert_eq!(int(&v["collateral_amount"]), denom::COLLATERAL_AMOUNT);
    assert_eq!(int(&v["max_collateral_amount"]), denom::MAX_COLLATERAL_AMOUNT);
    assert_eq!(int32(&v["promotion_ratio"]), denom::PROMOTION_RATIO);
    assert_eq!(int32(&v["gap_divisor"]), denom::GAP_DIVISOR);
    assert_eq!(int32(&v["min_gap_threshold"]), denom::MIN_GAP_THRESHOLD);
}

#[test]
fn denominations() {
    let v = load("core_common.json");
    for row in rows(&v, "denoms") {
        let d = int32(&row[0]);
        let index = denom::denomination_index(d).map_or(-1, |i| i64::try_from(i).unwrap());
        assert_eq!(index, int(&row[1]), "GetDenominationIndex({d})");
        assert_eq!(denom::denomination_to_amount(d), int(&row[2]), "DenominationToAmount({d})");
        assert_eq!(denom::is_valid_denomination(d), flag(&row[3]), "IsValidDenomination({d})");
        assert_eq!(denom::larger_adjacent_denom(d), int32(&row[4]), "GetLargerAdjacentDenom({d})");
        assert_eq!(
            denom::smaller_adjacent_denom(d),
            int32(&row[5]),
            "GetSmallerAdjacentDenom({d})"
        );
    }
}

#[test]
fn adjacency() {
    let v = load("core_common.json");
    for row in rows(&v, "adjacent") {
        let (a, b) = (int32(&row[0]), int32(&row[1]));
        assert_eq!(
            denom::are_adjacent_denominations(a, b),
            flag(&row[2]),
            "AreAdjacentDenominations({a}, {b})"
        );
    }
}

#[test]
fn amounts() {
    let v = load("core_common.json");
    for row in rows(&v, "amounts") {
        let a = int(&row[0]);
        assert_eq!(denom::amount_to_denomination(a), int32(&row[1]), "AmountToDenomination({a})");
        assert_eq!(denom::is_denominated_amount(a), flag(&row[2]), "IsDenominatedAmount({a})");
        assert_eq!(denom::is_collateral_amount(a), flag(&row[3]), "IsCollateralAmount({a})");
        assert_eq!(
            denom::calculate_amount_priority(a),
            int32(&row[4]),
            "CalculateAmountPriority({a})"
        );
    }
}

#[test]
fn gap_thresholds() {
    let v = load("core_common.json");
    for row in rows(&v, "gap_threshold") {
        let goal = int32(&row[0]);
        assert_eq!(denom::gap_threshold(goal), int32(&row[1]), "GetGapThreshold({goal})");
    }
}

#[test]
fn rebalance_decisions() {
    let v = load("core_common.json");
    for row in rows(&v, "rebalance") {
        let (a, b, mixed, goal) = (int32(&row[0]), int32(&row[1]), int32(&row[2]), int32(&row[3]));
        assert_eq!(
            denom::should_promote_denoms(a, b, mixed, goal),
            flag(&row[4]),
            "ShouldPromoteDenoms({a}, {b}, {mixed}, {goal})"
        );
        assert_eq!(
            denom::should_demote_denoms(a, b, mixed, goal),
            flag(&row[5]),
            "ShouldDemoteDenoms({a}, {b}, {mixed}, {goal})"
        );
    }
}

#[test]
fn side_counts() {
    // Core's MixShape order: UNKNOWN, STANDARD, PROMOTION, DEMOTION.
    const SHAPES: [MixShape; 4] =
        [MixShape::Unknown, MixShape::Standard, MixShape::Promotion, MixShape::Demotion];
    let v = load("core_common.json");
    for row in rows(&v, "sides") {
        let shapes: Vec<MixShape> = row[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| SHAPES[usize::try_from(int(s)).unwrap()])
            .collect();
        let counts: MixSideCounts = shapes.iter().copied().collect();
        assert_eq!(i64::from(counts.inputs), int(&row[1]), "{shapes:?} inputs");
        assert_eq!(i64::from(counts.outputs), int(&row[2]), "{shapes:?} outputs");
        assert_eq!(counts.is_covered(), flag(&row[3]), "{shapes:?} IsCovered");
    }
}

#[test]
fn denomination_strings() {
    let v = load("core_amount_strings.json");
    let mut checked = 0;
    for row in rows(&v, "amounts") {
        let (amount, text) = (int(&row[0]), row[1].as_str().unwrap());
        let d = denom::amount_to_denomination(amount);
        if d != 0 {
            assert_eq!(denom::denomination_to_string(d), text, "DenominationToString({d})");
            checked += 1;
        }
    }
    assert_eq!(checked, denom::STANDARD_DENOMINATIONS.len());
    // Core: coinjoin/common.cpp:12 DenominationToString, its sentinel names.
    assert_eq!(denom::denomination_to_string(0), "N/A");
    assert_eq!(denom::denomination_to_string(32), "out-of-bounds");
    assert_eq!(denom::denomination_to_string(3), "non-denom");
}

fn bytes32(v: &Value) -> [u8; 32] {
    let text = v.as_str().unwrap_or_else(|| panic!("{v} is not a string"));
    assert_eq!(text.len(), 64, "{v}");
    std::array::from_fn(|i| {
        u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap_or_else(|e| panic!("{v}: {e}"))
    })
}

/// `CWallet::IsFullyMixed`'s coin flip (wallet/coinjoin.cpp:416-420) as Core computed it.
#[test]
fn fully_mixed_coin_flip() {
    let data = load("core_fully_mixed.json");
    let rows = rows(&data, "rows");
    assert_eq!(rows.len(), 24 * 9);
    let mut answers = [0usize; 2];
    for row in rows {
        let txid = TxHash::from_lendian(bytes32(&row[0]));
        let index = u32::try_from(int(&row[1])).unwrap_or_else(|_| panic!("{row}"));
        let salt = bytes32(&row[2]);
        let stops = flag(&row[4]);
        assert_eq!(rounds::stops_mixing(&outpoint(txid, index), &salt), stops, "{row}");
        answers[usize::from(stops)] += 1;
    }
    assert!(answers[0] > 0 && answers[1] > 0, "{answers:?}");
}

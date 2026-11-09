//! Denominations, collateral amounts and the post-V24 rebalance decisions: Core's
//! `src/coinjoin/common.{h,cpp}`.
//!
//! A denomination is Core's bit form (`nDenom`, an `int`): bit `i` is
//! [`STANDARD_DENOMINATIONS`]`[i]`, largest first. Amounts are duffs (`CAmount`).

/// `COIN`, in duffs.
pub const COIN: i64 = 100_000_000;
/// `MAX_MONEY`, in duffs.
pub const MAX_MONEY: i64 = 21_000_000 * COIN;

// Core: coinjoin/common.h:41 vecStandardDenominations
/// The standard denominations, largest first: each is a power of ten DASH plus a thousandth of it
/// plus 1 duff for the first, 10.0001 DASH, down to 0.00100001 DASH.
pub const STANDARD_DENOMINATIONS: [i64; 5] =
    [(10 * COIN) + 10_000, COIN + 1_000, (COIN / 10) + 100, (COIN / 100) + 10, (COIN / 1_000) + 1];

/// `GetSmallestDenomination()`.
pub const SMALLEST_DENOMINATION: i64 = STANDARD_DENOMINATIONS[STANDARD_DENOMINATIONS.len() - 1];

/// `GetCollateralAmount()`: a tenth of the smallest denomination, 10,000 duffs.
pub const COLLATERAL_AMOUNT: i64 = SMALLEST_DENOMINATION / 10;
/// `GetMaxCollateralAmount()`: four collaterals, 40,000 duffs.
pub const MAX_COLLATERAL_AMOUNT: i64 = COLLATERAL_AMOUNT * 4;

// Core: coinjoin/common.h:121 PROMOTION_RATIO
/// Post-V24: ten coins of one denomination are worth one of the next larger one.
pub const PROMOTION_RATIO: i32 = 10;
/// [`PROMOTION_RATIO`] as a count.
pub(crate) const PROMOTION_RATIO_COUNT: usize = PROMOTION_RATIO as usize;
/// Post-V24: the deficit gap that triggers a conversion, as `1/GAP_DIVISOR` of the goal.
pub const GAP_DIVISOR: i32 = 5;
/// Post-V24: the smallest gap that keeps a conversion from being reversed at once.
pub const MIN_GAP_THRESHOLD: i32 = (PROMOTION_RATIO + 1) / 2;

// Core: coinjoin/common.h:56 GetDenominationIndex
/// The index of `denom` in [`STANDARD_DENOMINATIONS`] (0 = largest), or `None` (Core's `-1`)
/// unless `denom` is a single valid bit.
pub const fn denomination_index(denom: i32) -> Option<usize> {
    if denom <= 0 {
        return None;
    }
    let mut i = 0;
    while i < STANDARD_DENOMINATIONS.len() {
        if denom == 1 << i {
            return Some(i);
        }
        i += 1;
    }
    None
}

// Core: coinjoin/common.h:71 AmountToDenomination
/// The denomination bit of `amount`, or 0 when it is not a standard denomination.
pub const fn amount_to_denomination(amount: i64) -> i32 {
    let mut i = 0;
    while i < STANDARD_DENOMINATIONS.len() {
        if amount == STANDARD_DENOMINATIONS[i] {
            return 1 << i;
        }
        i += 1;
    }
    0
}

// Core: coinjoin/common.h:87 DenominationToAmount
/// The amount of `denom`, with Core's sentinels: 0 for an uninitialised session (`denom == 0`),
/// -1 out of bounds, -2 for several bits ("non-denom"), -3 for a conversion error (unreachable).
pub const fn denomination_to_amount(denom: i32) -> i64 {
    if denom == 0 {
        return 0;
    }
    if denom >= (1 << STANDARD_DENOMINATIONS.len()) || denom < 0 {
        return -1;
    }
    if (denom & (denom - 1)) != 0 {
        return -2;
    }
    match denomination_index(denom) {
        Some(i) => STANDARD_DENOMINATIONS[i],
        None => -3,
    }
}

/// `IsDenominatedAmount`.
pub const fn is_denominated_amount(amount: i64) -> bool {
    amount_to_denomination(amount) > 0
}

/// `IsValidDenomination`.
pub const fn is_valid_denomination(denom: i32) -> bool {
    denomination_to_amount(denom) > 0
}

// Core: coinjoin/common.cpp:12 DenominationToString
/// `DenominationToString`: the amount as Core's `ValueFromAmount` prints it, or the sentinel's
/// name.
pub fn denomination_to_string(denom: i32) -> String {
    match denomination_to_amount(denom) {
        0 => "N/A".to_owned(),
        -1 => "out-of-bounds".to_owned(),
        -2 => "non-denom".to_owned(),
        -3 => "to-amount-error".to_owned(),
        amount => value_from_amount(amount),
    }
}

// Core: core_write.cpp:35 ValueFromAmount
/// `ValueFromAmount(amount).getValStr()` of a positive amount: `"%d.%08d"`.
fn value_from_amount(amount: i64) -> String {
    format!("{}.{:08}", amount / COIN, amount % COIN)
}

/// `IsCollateralAmount`: between one and four collaterals, both included.
pub const fn is_collateral_amount(amount: i64) -> bool {
    amount >= COLLATERAL_AMOUNT && amount <= MAX_COLLATERAL_AMOUNT
}

// Core: coinjoin/common.h:267 CalculateAmountPriority
/// `CalculateAmountPriority`, the client's coin ordering: a denomination ranks by
/// `(float)COIN / denom * 10000` (smaller first), any other amount below 1 DASH at 20000, and
/// larger ones by minus their whole DASH. Out of `0..=MAX_MONEY` it is 0.
///
/// Core computes the denomination's rank in single precision and converts it to `int` by
/// truncation; so does this, with IEEE division and multiplication (no libm).
#[allow(clippy::float_arithmetic, clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn calculate_amount_priority(amount: i64) -> i32 {
    if !(0..=MAX_MONEY).contains(&amount) {
        return 0;
    }
    if is_denominated_amount(amount) {
        return (COIN as f32 / amount as f32 * 10_000.0) as i32;
    }
    if amount < COIN {
        return 20_000;
    }
    // In range, so the whole-DASH count fits an `int`.
    -((amount / COIN) as i32)
}

// Core: coinjoin/common.h:139 GetGapThreshold
/// `GetGapThreshold(goal)`: `max(goal / GAP_DIVISOR, MIN_GAP_THRESHOLD)`.
pub const fn gap_threshold(goal: i32) -> i32 {
    let gap = goal / GAP_DIVISOR;
    if gap > MIN_GAP_THRESHOLD {
        gap
    } else {
        MIN_GAP_THRESHOLD
    }
}

/// Which side(s) of the session denomination a participant occupies (Core's `MixShape`, in its
/// order).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MixShape {
    /// Empty or malformed: occupies neither side.
    Unknown,
    /// N session-denomination inputs to N outputs: both sides.
    Standard,
    /// Ten session-denomination inputs to one output of the next larger denomination: the input
    /// side.
    Promotion,
    /// One input of the next larger denomination to ten session-denomination outputs: the output
    /// side.
    Demotion,
}

impl MixShape {
    // Core: coinjoin/coinjoin.h:228 CCoinJoinEntry::GetMixShape
    /// The shape of an entry with `inputs` inputs and `outputs` outputs.
    pub const fn of_entry(inputs: usize, outputs: usize) -> MixShape {
        let ratio = PROMOTION_RATIO_COUNT;
        if inputs == 0 || outputs == 0 {
            MixShape::Unknown
        } else if inputs == outputs {
            MixShape::Standard
        } else if inputs == ratio && outputs == 1 {
            MixShape::Promotion
        } else if inputs == 1 && outputs == ratio {
            MixShape::Demotion
        } else {
            MixShape::Unknown
        }
    }
}

/// How many participants occupy each side of the session denomination (Core's
/// `MixSideCounts`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct MixSideCounts {
    /// Participants contributing session-denomination inputs (standard and promotions).
    pub inputs: i32,
    /// Participants receiving session-denomination outputs (standard and demotions).
    pub outputs: i32,
}

impl MixSideCounts {
    /// `Add`: count one participant of `shape`.
    pub fn add(&mut self, shape: MixShape) {
        match shape {
            MixShape::Standard => {
                self.inputs += 1;
                self.outputs += 1;
            }
            MixShape::Promotion => self.inputs += 1,
            MixShape::Demotion => self.outputs += 1,
            MixShape::Unknown => {}
        }
    }

    /// `IsCovered`: each side holds nobody or at least two participants. A lone participant on a
    /// side would hold the only coins of their size there.
    pub const fn is_covered(&self) -> bool {
        self.inputs != 1 && self.outputs != 1
    }
}

impl FromIterator<MixShape> for MixSideCounts {
    fn from_iter<I: IntoIterator<Item = MixShape>>(shapes: I) -> Self {
        let mut counts = MixSideCounts::default();
        shapes.into_iter().for_each(|shape| counts.add(shape));
        counts
    }
}

/// How many coins of a final transaction sit at the session denomination on each side (Core's
/// `SessionDenomCounts`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SessionDenomCounts {
    /// Inputs at the session denomination.
    pub inputs: usize,
    /// Outputs at the session denomination.
    pub outputs: usize,
}

// Core: coinjoin/common.h:197 AreAdjacentDenominations
/// Whether two denominations are one step apart.
pub const fn are_adjacent_denominations(a: i32, b: i32) -> bool {
    match (denomination_index(a), denomination_index(b)) {
        (Some(i), Some(j)) => i == j + 1 || j == i + 1,
        _ => false,
    }
}

// Core: coinjoin/common.h:208 GetLargerAdjacentDenom
/// The next larger denomination, or 0 for the largest or an invalid one.
pub const fn larger_adjacent_denom(denom: i32) -> i32 {
    match denomination_index(denom) {
        Some(i) if i > 0 => 1 << (i - 1),
        _ => 0,
    }
}

// Core: coinjoin/common.h:218 GetSmallerAdjacentDenom
/// The next smaller denomination, or 0 for the smallest or an invalid one.
pub const fn smaller_adjacent_denom(denom: i32) -> i32 {
    match denomination_index(denom) {
        Some(i) if i + 1 < STANDARD_DENOMINATIONS.len() => 1 << (i + 1),
        _ => 0,
    }
}

/// `goal - count` when `count` is below the goal, else 0. Widened: Core's `int` arithmetic never
/// overflows on its option ranges, and this agrees with it there.
const fn deficit(count: i32, goal: i32) -> i64 {
    if count < goal {
        goal as i64 - count as i64
    } else {
        0
    }
}

// Core: coinjoin/common.h:232 ShouldPromoteDenoms
/// Post-V24: whether to combine [`PROMOTION_RATIO`] fully mixed coins of the smaller denomination
/// into one of the larger, which must be further from `goal` by more than [`gap_threshold`].
pub const fn should_promote_denoms(
    smaller_count: i32,
    larger_count: i32,
    smaller_fully_mixed: i32,
    goal: i32,
) -> bool {
    if smaller_count < goal / 2 {
        return false;
    }
    if smaller_fully_mixed < PROMOTION_RATIO {
        return false;
    }
    deficit(larger_count, goal) > deficit(smaller_count, goal) + gap_threshold(goal) as i64
}

// Core: coinjoin/common.h:250 ShouldDemoteDenoms
/// Post-V24: whether to split one fully mixed coin of the larger denomination into
/// [`PROMOTION_RATIO`] of the smaller, which must be further from `goal` by more than
/// [`gap_threshold`].
pub const fn should_demote_denoms(
    larger_count: i32,
    smaller_count: i32,
    larger_fully_mixed: i32,
    goal: i32,
) -> bool {
    if larger_count < goal / 2 {
        return false;
    }
    if larger_fully_mixed < 1 {
        return false;
    }
    deficit(smaller_count, goal) > deficit(larger_count, goal) + gap_threshold(goal) as i64
}

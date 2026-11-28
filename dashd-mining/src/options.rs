//! Block assembler options and Core's fee-rate arithmetic.

use dash_args::{ArgsManager, RuntimeError};
use dash_consensus::MAX_MONEY;

/// Core's `DEFAULT_BLOCK_MAX_SIZE` (policy/policy.h:23).
pub const DEFAULT_BLOCK_MAX_SIZE: u64 = 2_000_000;
/// Core's `DEFAULT_BLOCK_MIN_TX_FEE` in duffs per kB (policy/policy.h:25).
pub const DEFAULT_BLOCK_MIN_TX_FEE: i64 = 1_000;
const RESERVED_SIZE: u64 = 1_000;
const MAX_LEGACY_BLOCK_SIZE: u32 = 1_000_000;
const MAX_DIP0001_BLOCK_SIZE: u32 = 2_000_000;

const fn max_block_size(dip0001_active: bool) -> u32 {
    if dip0001_active {
        MAX_DIP0001_BLOCK_SIZE
    } else {
        MAX_LEGACY_BLOCK_SIZE
    }
}

const fn max_block_sigops(dip0001_active: bool) -> u32 {
    max_block_size(dip0001_active) / 50
}

/// A fee rate in duffs per kB (`CFeeRate`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeeRate {
    pub sat_per_k: i64,
}

impl FeeRate {
    pub const fn new(sat_per_k: i64) -> Self {
        Self {
            sat_per_k,
        }
    }

    pub const fn sat_per_k(self) -> i64 {
        self.sat_per_k
    }

    /// Core: policy/feerate.cpp:23-36 (`CFeeRate::GetFee`).
    ///
    /// The multiplication is checked because Core's signed overflow has no defined result. The
    /// supported Core domain is nonnegative rates and sizes whose product fits in `i64`; the
    /// sentinel is the documented port behaviour outside that domain.
    pub fn get_fee(self, num_bytes: u32) -> i64 {
        let size = i64::from(num_bytes);
        let product = match self.sat_per_k.checked_mul(size) {
            Some(product) => product,
            None => return i64::MAX,
        };
        let mut fee = ((product as f64) / 1_000.0).ceil() as i64;
        if fee == 0 && size != 0 {
            fee = if self.sat_per_k > 0 {
                1
            } else if self.sat_per_k < 0 {
                -1
            } else {
                0
            };
        }
        fee
    }

    #[allow(non_snake_case)]
    pub fn GetFee(self, num_bytes: u32) -> i64 {
        self.get_fee(num_bytes)
    }
}

/// The options used by `BlockAssembler`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub block_max_size: u64,
    pub block_max_sigops: u32,
    pub block_min_fee_rate: FeeRate,
    pub test_block_validity: bool,
    pub print_priority: bool,
}

/// The descriptive name used by mining code and callers.
pub type AssemblerOptions = Options;

impl Default for Options {
    fn default() -> Self {
        Self {
            block_max_size: DEFAULT_BLOCK_MAX_SIZE,
            block_max_sigops: 0,
            block_min_fee_rate: FeeRate::new(DEFAULT_BLOCK_MIN_TX_FEE),
            test_block_validity: true,
            print_priority: false,
        }
    }
}

impl Options {
    /// Core's per-call resource clamp (`BlockAssembler::resetBlock`).
    pub fn clamp(mut self, dip0001_active: bool) -> Self {
        self.block_max_size = self
            .block_max_size
            .clamp(RESERVED_SIZE, u64::from(max_block_size(dip0001_active)) - RESERVED_SIZE);
        self.block_max_sigops = max_block_sigops(dip0001_active);
        self
    }

    /// Core's constructor clamp (`ClampOptions`).
    pub fn new(configured: Self) -> Self {
        ClampOptions(configured)
    }
}

/// Core's constructor clamp.
#[allow(non_snake_case)]
pub fn ClampOptions(mut options: Options) -> Options {
    options.block_max_size = options.block_max_size.clamp(RESERVED_SIZE, DEFAULT_BLOCK_MAX_SIZE);
    options
}

/// Core's `ParseMoney` for the two mining amount options.
pub fn parse_money(value: &str) -> Option<i64> {
    if value.contains('\0') {
        return None;
    }
    let value = value.trim_matches([' ', '\x0c', '\n', '\r', '\t', '\x0b']);
    if value.is_empty() {
        return None;
    }
    let bytes = value.as_bytes();
    let mut whole = 0i64;
    let mut whole_digits = 0usize;
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        whole_digits += 1;
        whole = whole.checked_mul(10)?.checked_add(i64::from(bytes[i] - b'0'))?;
        i += 1;
    }
    if whole_digits > 10 {
        return None;
    }
    let mut units = 0i64;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let mut multiplier = 10_000_000i64;
        while i < bytes.len() && bytes[i].is_ascii_digit() && multiplier > 0 {
            units += i64::from(bytes[i] - b'0') * multiplier;
            multiplier /= 10;
            i += 1;
        }
    }
    if i != bytes.len() || units > 100_000_000 {
        return None;
    }
    let amount = whole.checked_mul(100_000_000)?.checked_add(units)?;
    (0..=MAX_MONEY).contains(&amount).then_some(amount)
}

/// Core's `ApplyArgsManOptions` (`node/miner.cpp:85-91`). A malformed `-blockmintxfee` leaves the
/// default in place; init's separate sanity check reports the `AmountErrMsg` before the node
/// starts. `-printpriority` is read here too; Core reads it at each assembly (`:452`), the same
/// value, since nothing sets it after init.
#[allow(non_snake_case)]
pub fn ApplyArgsManOptions(args: &ArgsManager, options: &mut Options) -> Result<(), RuntimeError> {
    options.block_max_size =
        args.get_int_arg("-blockmaxsize", options.block_max_size as i64)? as u64;
    if let Some(value) = args.get_arg_opt("-blockmintxfee")?
        && let Some(amount) = parse_money(&value)
    {
        options.block_min_fee_rate = FeeRate::new(amount);
    }
    options.print_priority = args.get_bool_arg("-printpriority", options.print_priority)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_args::ArgsManager;

    #[test]
    fn clamp_table() {
        for (dip1, max) in [(false, 999_000), (true, 1_999_000)] {
            for (input, expected) in [(0, 1_000), (u64::MAX, max)] {
                assert_eq!(
                    Options::default().with_size(input).clamp(dip1).block_max_size,
                    expected
                );
            }
        }
    }

    #[test]
    fn constructor_clamp_preserves_core_maximum() {
        assert_eq!(Options::new(Options::default().with_size(2_000_000)).block_max_size, 2_000_000);
    }

    #[test]
    fn fee_vectors_and_boundaries() {
        for ((rate, size), expected) in [
            ((1001, 100), 101),
            ((1000, 100), 100),
            ((1, 1), 1),
            ((999, 1), 1),
            ((0, 100), 0),
            ((1000, 0), 0),
            ((-1, 1), -1),
            ((-1001, 100), -100),
            ((-1000, 0), 0),
        ] {
            assert_eq!(FeeRate::new(rate).get_fee(size), expected);
        }
        assert_eq!(FeeRate::new(2_147_483_648).get_fee(u32::MAX), 9_223_372_034_707_292);
        assert_eq!(
            FeeRate::new(MAX_MONEY).get_fee(4_392),
            ((MAX_MONEY as i128 * 4_392 + 999) / 1000) as i64
        );
        assert_eq!(FeeRate::new(MAX_MONEY).get_fee(4_393), i64::MAX);
        assert_eq!(FeeRate::new(1000).get_fee((u32::MAX as u64 + 6) as u32), 5);
    }

    #[test]
    fn parse_money_matches_mining_boundaries() {
        assert_eq!(parse_money("21000000"), Some(MAX_MONEY));
        assert_eq!(parse_money("21000000.00000001"), None);
        assert_eq!(parse_money("-1"), None);
        let args = ArgsManager::new();
        let mut options = Options::default();
        ApplyArgsManOptions(&args, &mut options).unwrap();
        assert_eq!(options.block_min_fee_rate, FeeRate::new(DEFAULT_BLOCK_MIN_TX_FEE));
    }

    #[test]
    fn apply_args_man_options_preserves_fee_on_invalid_blockmintxfee() {
        for value in ["-1", "21000000.00000001", "abc"] {
            let args = ArgsManager::new();
            args.force_set_arg("-blockmintxfee", value);
            let mut options = Options {
                block_min_fee_rate: FeeRate::new(1234),
                ..Options::default()
            };
            ApplyArgsManOptions(&args, &mut options).unwrap();
            assert_eq!(options.block_min_fee_rate, FeeRate::new(1234), "{value}");
        }
    }

    trait WithSize {
        fn with_size(self, size: u64) -> Self;
    }
    impl WithSize for Options {
        fn with_size(mut self, size: u64) -> Self {
            self.block_max_size = size;
            self
        }
    }
}

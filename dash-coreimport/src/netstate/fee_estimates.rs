//! `fee_estimates.dat`: `CBlockPolicyEstimator::Write`/`Read` in `policy/fees.cpp`.
//!
//! Layout (Core v24.0.0-rc.3): `i32 version required` (140100), `i32 version that wrote` (the
//! writer's `CLIENT_VERSION`), `u32 best seen height`, `u32 historical first`, `u32 historical
//! best`, the feerate buckets (compact-size count, then each as the `u64` bit pattern of an
//! `f64`), and three `TxConfirmStats` (the medium, short and long horizons, in that order). There
//! is no network magic and no checksum.
//!
//! This reader sees bytes only. Core does not read the file at all when it is more than 60 whole
//! hours old (`policy/fees.cpp:570`-`575`, unless `-acceptstalefeeestimates`, regtest only); a
//! loader must apply that rule to the file's modification time before using [`parse`]'s result
//! (the crate's loader duties, in [`super`]).
//!
//! Core's ordering of its sanity checks differs here only where the file is rejected either way:
//! counts are compared before the elements are read, so a corrupt count reserves nothing.

use super::envelope::check_size;
use super::error::{FormatError, FormatResult};
use super::reader::Reader;

const WHAT: &str = "fee_estimates.dat";

/// `CLIENT_VERSION` of Core v24.0.0: a file that requires a newer reader is refused.
pub const CORE_CLIENT_VERSION: i32 = 240_000;
/// The first version with the format this reader parses (`nVersionRequired < 140100` is ignored
/// by Core with a log line).
pub const MIN_VERSION_REQUIRED: i32 = 140_100;

/// Core: at most 1,000 buckets and 1,008 confirmations (one week) per stats block. A valid file
/// is below 3 x 2 x 1,008 x 1,000 x 8 bytes.
pub const MAX_FEE_ESTIMATES_SIZE: usize = 64 << 20;
const MAX_BUCKETS: u64 = 1000;
const MAX_CONFIRMS: u64 = 6 * 24 * 7;

/// One `TxConfirmStats`: the moving averages of one horizon.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmStats {
    /// The decay factor. Core rejects `decay <= 0 || decay >= 1` and nothing else, so a NaN passes
    /// (both comparisons are false) and Core writes it back; this reader accepts it the same way.
    pub decay: f64,
    /// Blocks per confirmation period; non-zero.
    pub scale: u32,
    pub feerate_avg: Vec<f64>,
    pub tx_ct_avg: Vec<f64>,
    /// `confAvg[period][bucket]`.
    pub conf_avg: Vec<Vec<f64>>,
    /// `failAvg[period][bucket]`.
    pub fail_avg: Vec<Vec<f64>>,
}

/// A parsed `fee_estimates.dat`.
#[derive(Debug, Clone, PartialEq)]
pub struct FeeEstimates {
    pub version_required: i32,
    pub version_that_wrote: i32,
    pub best_seen_height: u32,
    pub historical_first: u32,
    pub historical_best: u32,
    /// The feerate bucket boundaries (2 to 1,000). Core copies them without checking their
    /// order or finiteness, and so does this reader: a consumer that indexes by them must.
    pub buckets: Vec<f64>,
    pub medium: ConfirmStats,
    pub short: ConfirmStats,
    pub long: ConfirmStats,
    /// Bytes after the last stats block; Core does not look at them.
    pub trailing_bytes: usize,
}

/// The two version words, for detection.
pub fn versions(bytes: &[u8]) -> FormatResult<(i32, i32)> {
    let mut r = Reader::new(bytes, WHAT);
    Ok((r.i32_le()?, r.i32_le()?))
}

/// A compact-size count that must equal `expected`, then that many encoded doubles.
fn read_doubles(
    r: &mut Reader<'_>,
    expected: usize,
    mismatch: &'static str,
) -> FormatResult<Vec<f64>> {
    let n = r.compact_size(true)?;
    if n != expected as u64 {
        return Err(r.corrupt(mismatch));
    }
    let mut out = Vec::with_capacity(r.capacity_for(n, 8));
    for _ in 0..n {
        out.push(r.f64_encoded()?);
    }
    Ok(out)
}

/// `periods` rows of `num_buckets` doubles after a compact-size row count (checked by the
/// caller through `expect_rows`).
fn read_matrix(
    r: &mut Reader<'_>,
    num_buckets: usize,
    expect_rows: impl FnOnce(u64) -> Result<(), &'static str>,
    row_mismatch: &'static str,
) -> FormatResult<Vec<Vec<f64>>> {
    let rows = r.compact_size(true)?;
    expect_rows(rows).map_err(|reason| r.corrupt(reason))?;
    let mut out = Vec::with_capacity(r.capacity_for(rows, 1 + num_buckets * 8));
    for _ in 0..rows {
        out.push(read_doubles(r, num_buckets, row_mismatch)?);
    }
    Ok(out)
}

// Core: policy/fees.cpp TxConfirmStats::Read
fn read_stats(r: &mut Reader<'_>, num_buckets: usize) -> FormatResult<ConfirmStats> {
    let decay = r.f64_encoded()?;
    // As Core writes it: a NaN decay passes (both comparisons are false).
    if decay <= 0.0 || decay >= 1.0 {
        return Err(r.corrupt("decay must be between 0 and 1 (non-inclusive)"));
    }
    let scale = r.u32_le()?;
    if scale == 0 {
        return Err(r.corrupt("scale must be non-zero"));
    }
    let feerate_avg = read_doubles(r, num_buckets, "feerate average bucket count mismatch")?;
    let tx_ct_avg = read_doubles(r, num_buckets, "tx count bucket count mismatch")?;
    let scale64 = u64::from(scale);
    let conf_avg = read_matrix(
        r,
        num_buckets,
        |periods| {
            // `maxConfirms = scale * periods` must be in [1, 1008].
            if periods == 0 || periods > MAX_CONFIRMS || scale64 * periods > MAX_CONFIRMS {
                Err("must track between 1 and 1008 confirmations")
            } else {
                Ok(())
            }
        },
        "confirmation average bucket count mismatch",
    )?;
    let periods = conf_avg.len() as u64;
    let fail_avg = read_matrix(
        r,
        num_buckets,
        |rows| {
            if rows == periods {
                Ok(())
            } else {
                Err("failure average periods mismatch")
            }
        },
        "failure average bucket count mismatch",
    )?;
    Ok(ConfirmStats {
        decay,
        scale,
        feerate_avg,
        tx_ct_avg,
        conf_avg,
        fail_avg,
    })
}

/// Parses a `fee_estimates.dat`.
///
/// A file whose required version is older than [`MIN_VERSION_REQUIRED`] is
/// [`FormatError::UnsupportedVersion`]; Core logs it and starts with empty estimates, and so
/// should the caller.
// Core: policy/fees.cpp CBlockPolicyEstimator::Read
pub fn parse(bytes: &[u8]) -> FormatResult<FeeEstimates> {
    check_size(bytes.len(), MAX_FEE_ESTIMATES_SIZE, WHAT)?;
    let mut r = Reader::new(bytes, WHAT);
    let version_required = r.i32_le()?;
    let version_that_wrote = r.i32_le()?;
    if !(MIN_VERSION_REQUIRED..=CORE_CLIENT_VERSION).contains(&version_required) {
        return Err(FormatError::UnsupportedVersion {
            what: WHAT,
            version: version_required as u32 as u64,
        });
    }
    let best_seen_height = r.u32_le()?;
    let historical_first = r.u32_le()?;
    let historical_best = r.u32_le()?;
    if historical_first > historical_best || historical_best > best_seen_height {
        return Err(r.corrupt("historical block range for estimates is invalid"));
    }
    let n = r.compact_size(true)?;
    if !(2..=MAX_BUCKETS).contains(&n) {
        return Err(r.corrupt("must have between 2 and 1000 feerate buckets"));
    }
    let num_buckets = n as usize;
    let mut buckets = Vec::with_capacity(r.capacity_for(n, 8));
    for _ in 0..n {
        buckets.push(r.f64_encoded()?);
    }
    let medium = read_stats(&mut r, num_buckets)?;
    let short = read_stats(&mut r, num_buckets)?;
    let long = read_stats(&mut r, num_buckets)?;
    Ok(FeeEstimates {
        version_required,
        version_that_wrote,
        best_seen_height,
        historical_first,
        historical_best,
        buckets,
        medium,
        short,
        long,
        trailing_bytes: r.remaining(),
    })
}

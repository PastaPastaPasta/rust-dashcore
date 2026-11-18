//! Sampling and `.range.json` sidecar support shared by `digest extract`, `digest join` and
//! `digest compare` (StateDigest tooling X1–X3).
//!
//! - **Sample specification** (`--sample every=E,edges=D,windows=NxL,seed=S,span=P..Q`): all
//!   fields are required. Bounds, each refused by [`SampleSpec::parse`]: `E ≥ 1`, `D ≤ 1_000_000`,
//!   `L ≥ 1`, `N·L ≤ Q−P+1`, `P ≥ 0`, `Q ≤ 2^40`. [`sample_set`] is a pure function of
//!   [`SampleSpec`] and the chain's boundary set (`extract::Options::default_boundaries` plus
//!   `--boundary`), never of the shard bounds `A` and `B`.
//! - **Deterministic PRNG**: [`SplitMix64`] written out in-crate with rejection-sampled uniform
//!   bounded draws, so window starts in `P..=(Q - L + 1)` have no modulo bias and add no
//!   dependency.
//! - **Sidecar** (`<out>.range.json`): canonical JSON (sorted keys, no whitespace, trailing `\n`)
//!   recording `from`, `to`, `sparse_only`, `sample`, `sampled` and `sampled_sha256`.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_statedigest::compare::FieldId;
use dash_statedigest::{Class, FIELDS, FieldSource, Kind, Value};
use dashcore::hashes::{HashEngine as _, sha256};
use serde::Deserialize;

/// Maximum `edges` (`D`). Larger values make the edge walk unbounded.
const EDGES_MAX: i64 = 1_000_000;

/// Maximum span end (`Q`), `2^40`.
const SPAN_END_MAX: i64 = 1 << 40;

/// SplitMix64 deterministic PRNG (Steele, Lea, Flood 2014).
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seeds a generator with `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed,
        }
    }

    /// Next 64-bit pseudorandom word.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform integer in `0..bound` (`bound > 0`), unbiased via rejection sampling.
    pub fn uniform(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "SplitMix64::uniform bound must be > 0");
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let x = self.next_u64();
            if x >= threshold {
                return x % bound;
            }
        }
    }
}

/// Parsed `--sample every=E,edges=D,windows=NxL,seed=S,span=P..Q` specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleSpec {
    pub every: i64,
    pub edges: i64,
    pub windows_n: u64,
    pub windows_len: i64,
    pub seed: u64,
    pub span_from: i64,
    pub span_to: i64,
}

fn parse_u64_strict(text: &str, what: &str) -> Result<u64> {
    ensure!(
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid {what} {text:?}"
    );
    text.parse::<u64>().with_context(|| format!("invalid {what} {text:?}"))
}

fn parse_i64_nonneg(text: &str, what: &str) -> Result<i64> {
    ensure!(
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid {what} {text:?}"
    );
    text.parse::<i64>().with_context(|| format!("invalid {what} {text:?}"))
}

impl SampleSpec {
    /// Parses `every=E,edges=D,windows=NxL,seed=S,span=P..Q`. Every field is required.
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(!input.is_empty(), "--sample: empty specification");
        let mut every = None;
        let mut edges = None;
        let mut windows = None;
        let mut seed = None;
        let mut span = None;

        for part in input.split(',') {
            let (key, val) = part
                .split_once('=')
                .ok_or_else(|| anyhow!("--sample: expected key=value in {part:?}"))?;
            match key {
                "every" => {
                    ensure!(every.is_none(), "--sample: duplicate field 'every'");
                    let e = parse_i64_nonneg(val, "every")?;
                    ensure!(e >= 1, "--sample: every must be >= 1, got {e}");
                    every = Some(e);
                }
                "edges" => {
                    ensure!(edges.is_none(), "--sample: duplicate field 'edges'");
                    let d = parse_i64_nonneg(val, "edges")?;
                    ensure!(d <= EDGES_MAX, "--sample: edges must be <= {EDGES_MAX}, got {d}");
                    edges = Some(d);
                }
                "windows" => {
                    ensure!(windows.is_none(), "--sample: duplicate field 'windows'");
                    let (n_str, l_str) = val
                        .split_once('x')
                        .ok_or_else(|| anyhow!("--sample: windows must be NxL, got {val:?}"))?;
                    let n = parse_u64_strict(n_str, "windows count")?;
                    let l = parse_i64_nonneg(l_str, "windows length")?;
                    ensure!(l >= 1, "--sample: windows length must be >= 1, got {l}");
                    windows = Some((n, l));
                }
                "seed" => {
                    ensure!(seed.is_none(), "--sample: duplicate field 'seed'");
                    seed = Some(parse_u64_strict(val, "seed")?);
                }
                "span" => {
                    ensure!(span.is_none(), "--sample: duplicate field 'span'");
                    let (p_str, q_str) = val
                        .split_once("..")
                        .ok_or_else(|| anyhow!("--sample: span must be P..Q, got {val:?}"))?;
                    if p_str.is_empty() || !p_str.bytes().all(|b| b.is_ascii_digit()) {
                        bail!("--sample: span start must be >= 0, got {p_str:?}");
                    }
                    let p = parse_i64_nonneg(p_str, "span start")?;
                    let q = parse_i64_nonneg(q_str, "span end")?;
                    ensure!(q <= SPAN_END_MAX, "--sample: span end must be <= 2^40, got {q}");
                    ensure!(p <= q, "--sample: span start {p} is above end {q}");
                    span = Some((p, q));
                }
                other => bail!("--sample: unknown field {other:?}"),
            }
        }

        let every = every.ok_or_else(|| anyhow!("--sample: missing 'every'"))?;
        let edges = edges.ok_or_else(|| anyhow!("--sample: missing 'edges'"))?;
        let (windows_n, windows_len) =
            windows.ok_or_else(|| anyhow!("--sample: missing 'windows'"))?;
        let seed = seed.ok_or_else(|| anyhow!("--sample: missing 'seed'"))?;
        let (span_from, span_to) = span.ok_or_else(|| anyhow!("--sample: missing 'span'"))?;

        if windows_n > 0 {
            let span_len = i128::from(span_to) - i128::from(span_from) + 1;
            let product = i128::from(windows_n)
                .checked_mul(i128::from(windows_len))
                .ok_or_else(|| anyhow!("--sample: windows count times length overflows"))?;
            ensure!(
                product <= span_len,
                "--sample: windows {windows_n}x{windows_len} cover {product} heights, but the span \
                 {span_from}..{span_to} has {span_len}"
            );
        }

        Ok(Self {
            every,
            edges,
            windows_n,
            windows_len,
            seed,
            span_from,
            span_to,
        })
    }

    /// Whether `h` lies inside `[P, Q]`.
    pub fn in_span(&self, h: i64) -> bool {
        (self.span_from..=self.span_to).contains(&h)
    }
}

impl fmt::Display for SampleSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "every={},edges={},windows={}x{},seed={},span={}..{}",
            self.every,
            self.edges,
            self.windows_n,
            self.windows_len,
            self.seed,
            self.span_from,
            self.span_to
        )
    }
}

impl FromStr for SampleSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Computes the sampled height set inside `[P, Q]` as a pure function of `spec` and `boundaries`.
pub fn sample_set(spec: &SampleSpec, boundaries: &BTreeSet<i64>) -> BTreeSet<i64> {
    let mut set = BTreeSet::new();
    let (p, q, e) = (spec.span_from, spec.span_to, spec.every);

    // 1. Every height h in [P, Q] with h % E == 0.
    let rem = p % e;
    let first = if rem == 0 {
        Some(p)
    } else {
        p.checked_add(e - rem)
    };
    if let Some(mut h) = first {
        while h <= q {
            set.insert(h);
            match h.checked_add(e) {
                Some(next) => h = next,
                None => break,
            }
        }
    }

    // 2. Each boundary in [P, Q], and P and Q themselves, +-D, clipped to [P, Q].
    let mut add_edges = |center: i64| {
        let lo = center.saturating_sub(spec.edges).max(p);
        let hi = center.saturating_add(spec.edges).min(q);
        for h in lo..=hi {
            set.insert(h);
        }
    };
    add_edges(p);
    add_edges(q);
    for &b in boundaries.range(p..=q) {
        add_edges(b);
    }

    // 3. N windows of L consecutive heights, starts drawn uniformly from [P, Q - L + 1].
    let mut rng = SplitMix64::new(spec.seed);
    if spec.windows_n > 0 {
        let max_start = q - spec.windows_len + 1;
        let choices =
            u64::try_from(i128::from(max_start) - i128::from(p) + 1).expect("span fits in u64");
        for _ in 0..spec.windows_n {
            let offset = rng.uniform(choices);
            let start =
                i64::try_from(i128::from(p) + i128::from(offset)).expect("window start fits i64");
            let end = start.checked_add(spec.windows_len).expect("window end fits i64");
            for h in start..end {
                set.insert(h);
            }
        }
    }

    set
}

/// Counts `heights` (assumed sorted ascending) and hashes them as one decimal per line (`"{h}\n"`).
pub fn digest_heights<'a>(heights: impl IntoIterator<Item = &'a i64>) -> (u64, String) {
    let mut engine = sha256::Hash::engine();
    let mut count = 0u64;
    let mut line = Vec::with_capacity(24);
    for h in heights {
        line.clear();
        writeln!(line, "{h}").expect("writing to Vec cannot fail");
        engine.input(&line);
        count += 1;
    }
    (count, sha256::Hash::from_engine(engine).to_string())
}

/// The six §5.3 golden coin-stat keys of `C.s`, in [`FIELDS`] order. Extract, join and compare
/// all read this list.
pub const COIN_STAT_KEYS: [&str; 6] = ["muhash", "txouts", "amount", "unspendable", "bi", "stats"];

/// [`COIN_STAT_KEYS`] as the comparator's [`FieldId`]s.
pub fn coin_stat_fields() -> Vec<FieldId> {
    let fields: Vec<FieldId> = COIN_STAT_KEYS
        .iter()
        .map(|key| {
            FieldId::find(Kind::Connect, Class::Semantic, key).expect("coin-stat key is in FIELDS")
        })
        .collect();
    debug_assert_eq!(
        FIELDS
            .iter()
            .filter(|f| f.source == FieldSource::Golden)
            .map(|f| f.key)
            .collect::<Vec<_>>(),
        COIN_STAT_KEYS
    );
    fields
}

/// Coin-key pattern of one height against a sample rule.
///
/// Inside `[from, to] ∩ [P, Q]` and outside `set`, all six keys are absent. Everywhere else
/// (no sample, outside that intersection, or in `set`) all six are present. Any other pattern
/// is an error naming `h`.
pub fn enforce_coin_pattern(
    h: i64,
    has: impl Fn(&str) -> bool,
    spec: Option<&SampleSpec>,
    set: &BTreeSet<i64>,
    from: i64,
    to: i64,
) -> Result<()> {
    let unsampled = spec.is_some_and(|spec| {
        let lo = from.max(spec.span_from);
        let hi = to.min(spec.span_to);
        lo <= hi && (lo..=hi).contains(&h) && !set.contains(&h)
    });
    if unsampled {
        let present: Vec<&str> = COIN_STAT_KEYS.iter().copied().filter(|k| has(k)).collect();
        ensure!(
            present.is_empty(),
            "height {h}: coin-stat keys present at an unsampled height ({})",
            present.join(", ")
        );
    } else {
        let missing: Vec<&str> = COIN_STAT_KEYS.iter().copied().filter(|k| !has(k)).collect();
        ensure!(missing.is_empty(), "height {h}: coin-stat keys absent ({})", missing.join(", "));
    }
    Ok(())
}

/// The `<out>.range.json` sidecar written next to an extracted or joined golden.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeSidecar {
    pub from: i64,
    pub to: i64,
    pub sparse_only: bool,
    pub sample: Option<String>,
    pub sampled: u64,
    pub sampled_sha256: String,
}

impl RangeSidecar {
    /// Builds a sidecar for `from..=to`, computing `sampled` and `sampled_sha256` from `sample`
    /// and `boundaries`.
    pub fn build(
        from: i64,
        to: i64,
        sparse_only: bool,
        sample: Option<String>,
        boundaries: &BTreeSet<i64>,
    ) -> Result<Self> {
        ensure!(0 <= from && from <= to, "invalid sidecar range {from}..={to}");
        let (sampled, sampled_sha256) = match &sample {
            Some(raw) => {
                let spec = SampleSpec::parse(raw)?;
                let set = sample_set(&spec, boundaries);
                digest_heights(set.range(from..=to))
            }
            None => digest_heights(std::iter::empty()),
        };
        Ok(Self {
            from,
            to,
            sparse_only,
            sample,
            sampled,
            sampled_sha256,
        })
    }

    /// Reads and parses a `.range.json` file.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let sidecar: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        ensure!(
            0 <= sidecar.from && sidecar.from <= sidecar.to,
            "{}: invalid range {}..={}",
            path.display(),
            sidecar.from,
            sidecar.to
        );
        Ok(sidecar)
    }

    /// Recomputes the sampled height set in `[self.from, self.to]` from `self.sample` and
    /// `boundaries` and verifies `sampled` and `sampled_sha256`. Returns `Some((spec, set_in_range))`
    /// when `self.sample` is `Some`, or `None` when `self.sample` is `None`.
    pub fn verify(
        &self,
        boundaries: &BTreeSet<i64>,
    ) -> Result<Option<(SampleSpec, BTreeSet<i64>)>> {
        ensure!(
            0 <= self.from && self.from <= self.to,
            "invalid range {}..={}",
            self.from,
            self.to
        );
        match &self.sample {
            Some(raw) => {
                let spec = SampleSpec::parse(raw)?;
                let full = sample_set(&spec, boundaries);
                let in_range: BTreeSet<i64> = full.range(self.from..=self.to).copied().collect();
                let (want_n, want_sha) = digest_heights(&in_range);
                ensure!(
                    self.sampled == want_n,
                    "sampled count is {}, recomputed {want_n}",
                    self.sampled
                );
                ensure!(
                    self.sampled_sha256 == want_sha,
                    "sampled_sha256 is {}, recomputed {want_sha}",
                    self.sampled_sha256
                );
                Ok(Some((spec, in_range)))
            }
            None => {
                let (want_n, want_sha) = digest_heights(std::iter::empty());
                ensure!(
                    self.sampled == want_n,
                    "sampled count is {}, expected 0 when sample is null",
                    self.sampled
                );
                ensure!(
                    self.sampled_sha256 == want_sha,
                    "sampled_sha256 is {}, expected {want_sha} when sample is null",
                    self.sampled_sha256
                );
                Ok(None)
            }
        }
    }

    /// Encodes the sidecar as canonical JSON (sorted keys, compact, trailing `\n`).
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let sample_val = match &self.sample {
            Some(s) => Value::string(s),
            None => Value::Null,
        };
        let sampled_i64 = i64::try_from(self.sampled).expect("sampled height count fits in i64");
        let obj = Value::obj([
            ("from", Value::Int(self.from)),
            ("sample", sample_val),
            ("sampled", Value::Int(sampled_i64)),
            ("sampled_sha256", Value::string(&self.sampled_sha256)),
            ("sparse_only", Value::Bool(self.sparse_only)),
            ("to", Value::Int(self.to)),
        ]);
        let mut out = obj.to_vec();
        out.push(b'\n');
        out
    }
}

/// `<out>.range.json` next to `out`.
pub fn sidecar_path(out: &Path) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(".range.json");
    PathBuf::from(name)
}

/// Sibling `.partial` path for atomic file replacement.
pub fn partial_path(target: &Path) -> Result<PathBuf> {
    let mut name = target
        .file_name()
        .with_context(|| format!("{} has no file name", target.display()))?
        .to_owned();
    name.push(".partial");
    Ok(target.with_file_name(name))
}

/// Writes `out` and `<out>.range.json` atomically via `.partial` files, syncing before rename and
/// cleaning up `.partial` files on error.
///
/// The sidecar is renamed first, then the golden. A crash between the two leaves the previous
/// golden beside the new sidecar; [`enforce_coin_pattern`] then refuses that pair.
pub fn write_golden_and_sidecar<T>(
    out: &Path,
    make: impl FnOnce(&mut BufWriter<File>) -> Result<(T, RangeSidecar)>,
) -> Result<T> {
    let out_partial = partial_path(out)?;
    let sidecar = sidecar_path(out);
    let sidecar_partial = partial_path(&sidecar)?;

    let res = (|| -> Result<T> {
        let mut writer = BufWriter::new(
            File::create(&out_partial)
                .with_context(|| format!("creating {}", out_partial.display()))?,
        );
        let (val, sc) = make(&mut writer)?;
        writer.flush().context("flushing golden file")?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()
            .context("syncing golden file")?;

        let mut sc_file = File::create(&sidecar_partial)
            .with_context(|| format!("creating {}", sidecar_partial.display()))?;
        sc_file
            .write_all(&sc.to_canonical_bytes())
            .with_context(|| format!("writing {}", sidecar_partial.display()))?;
        sc_file.sync_all().context("syncing sidecar file")?;

        // Sidecar first: a crash before the golden rename leaves the old golden with the new
        // sidecar, which the coin-key pattern check refuses. Renaming the golden first would
        // leave a new golden with a stale sidecar.
        std::fs::rename(&sidecar_partial, &sidecar).with_context(|| {
            format!("renaming {} to {}", sidecar_partial.display(), sidecar.display())
        })?;
        std::fs::rename(&out_partial, out)
            .with_context(|| format!("renaming {} to {}", out_partial.display(), out.display()))?;
        Ok(val)
    })();

    if res.is_err() {
        let _ = std::fs::remove_file(&out_partial);
        let _ = std::fs::remove_file(&sidecar_partial);
    }
    res
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::digest::json;

    #[test]
    fn sample_spec_parses_and_rejects_invalid_inputs() {
        let raw = "every=50,edges=3,windows=4x5,seed=12345,span=100..500";
        let spec = SampleSpec::parse(raw).unwrap();
        assert_eq!(
            spec,
            SampleSpec {
                every: 50,
                edges: 3,
                windows_n: 4,
                windows_len: 5,
                seed: 12345,
                span_from: 100,
                span_to: 500,
            }
        );
        assert_eq!(spec.to_string(), raw);

        // Field order does not matter. N = 0 draws no windows; L is still at least 1.
        let reordered =
            SampleSpec::parse("span=10..10,seed=0,windows=0x1,edges=0,every=1").unwrap();
        assert_eq!(reordered.windows_n, 0);
        assert_eq!(reordered.windows_len, 1);
        assert_eq!(reordered.span_from, 10);
        assert_eq!(reordered.span_to, 10);

        for bad in [
            "",
            "every=10,edges=2,windows=1x2,seed=1",
            "every=10,edges=2,windows=1x2,span=0..10",
            "every=0,edges=2,windows=1x2,seed=1,span=0..10",
            "every=-1,edges=2,windows=1x2,seed=1,span=0..10",
            "every=10,edges=-1,windows=1x2,seed=1,span=0..10",
            "every=10,edges=2,windows=0x0,seed=1,span=0..10",
            "every=10,edges=2,windows=1x0,seed=1,span=0..10",
            "every=10,edges=2,windows=1x12,seed=1,span=0..10",
            "every=10,edges=2,windows=1-2,seed=1,span=0..10",
            "every=10,edges=2,windows=1x2,seed=-1,span=0..10",
            "every=10,edges=2,windows=1x2,seed=+1,span=0..10",
            "every=10,edges=2,windows=1x2,seed=abc,span=0..10",
            "every=10,edges=2,windows=1x2,seed=1,span=10..5",
            "every=10,edges=2,windows=1x2,seed=1,span=-1..5",
            "every=10,edges=2,windows=1x2,seed=1,span=0..=5",
            "every=10,every=10,edges=2,windows=1x2,seed=1,span=0..5",
            "every=10,edges=2,windows=1x2,seed=1,span=0..5,extra=1",
        ] {
            assert!(SampleSpec::parse(bad).is_err(), "expected {bad:?} to fail");
        }
    }

    #[test]
    fn sample_spec_bounds_are_refused() {
        let refuse = |raw: &str, needle: &str| {
            let err = SampleSpec::parse(raw).expect_err(raw);
            let msg = format!("{err:#}");
            assert!(msg.contains(needle), "{raw}: expected {needle:?} in {msg}");
        };
        refuse("every=0,edges=0,windows=1x1,seed=1,span=0..10", "every must be >= 1");
        refuse("every=1,edges=1000001,windows=1x1,seed=1,span=0..10", "edges must be <= 1000000");
        refuse("every=1,edges=0,windows=0x0,seed=1,span=0..10", "windows length must be >= 1");
        refuse("every=1,edges=0,windows=1x0,seed=1,span=0..10", "windows length must be >= 1");
        refuse("every=1,edges=0,windows=3x5,seed=1,span=0..9", "windows 3x5 cover 15 heights");
        refuse(
            "every=1,edges=0,windows=1000000000000000000x1,seed=1,span=0..10",
            "cover 1000000000000000000 heights",
        );
        refuse("every=1,edges=0,windows=1x1,seed=1,span=-1..5", "span start must be >= 0");
        let over = (1i64 << 40) + 1;
        refuse(
            &format!("every=1,edges=0,windows=1x1,seed=1,span=0..{over}"),
            "span end must be <= 2^40",
        );

        // Each bound's largest accepted value still parses. Q = 2^40 is not expanded into a set.
        let q = 1i64 << 40;
        let ok =
            SampleSpec::parse(&format!("every=1,edges=1000000,windows=0x1,seed=1,span=0..{q}"))
                .unwrap();
        assert_eq!(ok.edges, EDGES_MAX);
        assert_eq!(ok.span_to, SPAN_END_MAX);
        assert_eq!(ok.span_from, 0);
    }

    #[test]
    fn sample_set_unions_cadence_edges_and_prng_windows_independently_of_shards() {
        let spec = SampleSpec::parse("every=20,edges=2,windows=2x3,seed=7,span=10..60").unwrap();
        // Boundary 5 is outside [10, 60] so its +-2 does NOT bleed into [10, 60]; 35 is inside.
        let boundaries = BTreeSet::from([5, 35, 70]);
        let set = sample_set(&spec, &boundaries);

        // Every height in set is within [10, 60].
        assert!(set.iter().all(|&h| (10..=60).contains(&h)));
        // Cadence every=20 in [10, 60]: 20, 40, 60.
        for h in [20, 40, 60] {
            assert!(set.contains(&h), "missing cadence {h}");
        }
        // Edges +-2 around P=10, boundary=35, Q=60, clipped to [10, 60].
        for h in [10, 11, 12, 33, 34, 35, 36, 37, 58, 59, 60] {
            assert!(set.contains(&h), "missing edge {h}");
        }
        // Boundary 5 outside [10, 60] did not pull in 7 unless a window hit it; test with 0 windows:
        let no_win = SampleSpec::parse("every=20,edges=2,windows=0x1,seed=7,span=10..60").unwrap();
        let det = sample_set(&no_win, &boundaries);
        assert_eq!(det, BTreeSet::from([10, 11, 12, 20, 33, 34, 35, 36, 37, 40, 58, 59, 60]));
    }

    #[test]
    fn sample_set_uses_prng_seed() {
        let s1 = SampleSpec::parse("every=10000,edges=0,windows=4x3,seed=1,span=1..1000").unwrap();
        let s2 = SampleSpec::parse("every=10000,edges=0,windows=4x3,seed=2,span=1..1000").unwrap();
        let b = BTreeSet::new();
        let set1 = sample_set(&s1, &b);
        let set2 = sample_set(&s2, &b);
        assert_eq!(set1, sample_set(&s1, &b), "deterministic for the same seed");
        assert_ne!(set1, set2, "different PRNG seeds must select different windows");
    }

    #[test]
    fn uniform_rejection_threshold_is_unbiased_for_a_bound_above_2_63() {
        // bound = 3·2^62. Then 2^64 % bound = 2^62, so a plain `x % bound` lands in
        // [0, 2^62) twice as often as in each later third. Rejection sampling drops x < 2^62,
        // which is exactly one full set of residues.
        let bound = 3 * (1u64 << 62);
        assert_eq!(bound.wrapping_neg() % bound, 1u64 << 62);
        let mut rng = SplitMix64::new(0x1234_5678_9abc_def0);
        let n = 3000u64;
        let third = 1u64 << 62;
        let mut low = 0u64;
        for _ in 0..n {
            let x = rng.uniform(bound);
            assert!(x < bound);
            if x < third {
                low += 1;
            }
        }
        let delta = (low as f64 - n as f64 / 3.0).abs();
        assert!(delta < 150.0, "low third {low} of {n}; a biased modulo would land near {}", n / 2);
    }

    #[test]
    fn sidecar_is_renamed_before_the_golden() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("golden.jsonl");
        // Renaming a file onto a directory fails, so the second rename cannot succeed.
        std::fs::create_dir(&out).unwrap();
        let old = dir.path().join("golden.jsonl.range.json");
        std::fs::write(&old, b"old-sidecar\n").unwrap();
        let err = write_golden_and_sidecar(&out, |writer| {
            writer.write_all(b"new golden\n")?;
            let sc = RangeSidecar::build(0, 1, false, None, &BTreeSet::new())?;
            Ok(((), sc))
        })
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("renaming"), "{msg}");
        assert!(out.is_dir(), "the old golden must still be in place");
        let side = std::fs::read(sidecar_path(&out)).unwrap();
        assert_ne!(side, b"old-sidecar\n");
        assert!(side.windows(14).any(|w| w == b"sampled_sha256"), "{side:?}");
        assert!(!partial_path(&out).unwrap().exists());
        assert!(!partial_path(&sidecar_path(&out)).unwrap().exists());
    }

    #[test]
    fn sidecar_canonical_json_and_verification() {
        let raw = "every=10,edges=1,windows=1x2,seed=42,span=5..25".to_owned();
        let boundaries = BTreeSet::new();
        let sc = RangeSidecar::build(0, 30, false, Some(raw), &boundaries).unwrap();
        let bytes = sc.to_canonical_bytes();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let reparsed = json::parse(&bytes).unwrap();
        assert_eq!(reparsed.to_vec(), &bytes[..bytes.len() - 1]);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.range.json");
        std::fs::write(&p, &bytes).unwrap();
        let loaded = RangeSidecar::read(&p).unwrap();
        assert_eq!(loaded, sc);
        assert!(loaded.verify(&boundaries).unwrap().is_some());

        let mut bad_count = loaded.clone();
        bad_count.sampled += 1;
        assert!(bad_count.verify(&boundaries).is_err());

        let mut bad_sha = loaded;
        bad_sha.sampled_sha256 = "00".repeat(32);
        assert!(bad_sha.verify(&boundaries).is_err());
    }
}

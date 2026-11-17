//! `digest join`: joins contiguous golden shards written by `digest extract` into a single golden
//! file and `.range.json` sidecar (StateDigest tooling X2).
//!
//! Every check fails closed before replacing `--out` or `<out>.range.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_statedigest::{FileKind, Header, Kind, Origin, Record, parse_line};

use super::extract::{Options as ExtractOptions, SparseRpc};
use super::sample::{
    RangeSidecar, SampleSpec, enforce_coin_pattern, sample_set, sidecar_path,
    write_golden_and_sidecar,
};
use super::store::Records;

/// One `--c-lines FILE:A..B` specification supplying `C` records for a `--sparse-only` shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CLinesSpec {
    pub path: PathBuf,
    pub from: i64,
    pub to: i64,
}

fn parse_nonneg_i64(text: &str, what: &str) -> Result<i64> {
    ensure!(
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid {what} {text:?}"
    );
    text.parse::<i64>().with_context(|| format!("invalid {what} {text:?}"))
}

impl CLinesSpec {
    /// Parses `FILE:A..B`.
    pub fn parse(spec: &str) -> Result<Self> {
        let (file, range) = spec
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("--c-lines {spec:?}: expected FILE:A..B"))?;
        ensure!(!file.is_empty(), "--c-lines {spec:?}: empty file path");
        let (a_str, b_str) = range
            .split_once("..")
            .ok_or_else(|| anyhow!("--c-lines {spec:?}: expected range A..B"))?;
        let from = parse_nonneg_i64(a_str, "--c-lines start")?;
        let to = parse_nonneg_i64(b_str, "--c-lines end")?;
        ensure!(from <= to, "--c-lines {spec:?}: start {from} is above end {to}");
        Ok(Self {
            path: PathBuf::from(file),
            from,
            to,
        })
    }
}

struct JoinedSparse {
    key: (i64, SparseRpc, usize, usize),
    line: Vec<u8>,
}

/// Reads the `H` line of `path`, checking it is a `Golden` header.
fn parse_golden_header(path: &Path, line_bytes: &[u8]) -> Result<Header> {
    match parse_line(line_bytes).with_context(|| format!("{}:1", path.display()))? {
        Record::Header(h) => {
            ensure!(
                h.file == FileKind::Golden,
                "{}:1: H.file is {}, expected golden",
                path.display(),
                h.file.as_str()
            );
            Ok(h)
        }
        other => bail!("{}:1: expected H record, got {:?}", path.display(), other.kind()),
    }
}

/// Reads `C` lines for `from..=to` from `source`, stopping right after height `to` so any later
/// lines (including a torn final line) are ignored.
fn read_c_lines(source: &Path, from: i64, to: i64, want_h_line: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut lines = Records::raw(source)?;
    let (_, h_bytes) = lines
        .next()
        .ok_or_else(|| anyhow!("{}: empty file", source.display()))?
        .with_context(|| format!("reading {}", source.display()))?;
    let _ = parse_golden_header(source, &h_bytes)?;
    ensure!(
        h_bytes == want_h_line,
        "{}: H record is not byte-identical to the shards' H",
        source.display()
    );

    let want_len = usize::try_from(to - from + 1).context("range length")?;
    let mut collected = Vec::with_capacity(want_len);
    let mut prev_below: Option<i64> = None;

    for item in lines {
        let (line_no, bytes) = item?;
        let rec = parse_line(&bytes).with_context(|| format!("{}:{line_no}", source.display()))?;
        let Record::Outcome(o) = rec else {
            bail!(
                "{}:{line_no}: expected C record before completing {from}..={to}, got {:?}",
                source.display(),
                rec.kind()
            );
        };
        ensure!(
            o.kind() == Kind::Connect && o.id.org == Origin::Tip,
            "{}:{line_no}: expected tip C record",
            source.display()
        );
        if o.id.h < from {
            ensure!(
                collected.is_empty(),
                "{}:{line_no}: out-of-order C record at height {}",
                source.display(),
                o.id.h
            );
            if let Some(prev) = prev_below {
                ensure!(
                    o.id.h > prev,
                    "{}:{line_no}: non-rising C height {} after {prev}",
                    source.display(),
                    o.id.h
                );
            }
            prev_below = Some(o.id.h);
            continue;
        }
        let want_h = from + collected.len() as i64;
        ensure!(
            o.id.h == want_h,
            "{}:{line_no}: expected C record at height {want_h}, got {}",
            source.display(),
            o.id.h
        );
        collected.push(bytes);
        if o.id.h == to {
            break;
        }
    }

    ensure!(
        collected.len() == want_len,
        "{}: C records ended before completing {from}..={to} (got {} of {want_len})",
        source.display(),
        collected.len()
    );
    Ok(collected)
}

/// Joins `shards` (each with `<shard>.range.json`) and optional `c_lines` into `out` and
/// `<out>.range.json` atomically. Returns the joined sidecar.
pub fn join(
    shards: &[PathBuf],
    c_lines: &[CLinesSpec],
    extra_boundaries: &BTreeSet<i64>,
    out: &Path,
) -> Result<RangeSidecar> {
    ensure!(!shards.is_empty(), "digest join requires at least one shard");

    let mut sidecars = Vec::with_capacity(shards.len());
    for (idx, shard) in shards.iter().enumerate() {
        let sc_path = sidecar_path(shard);
        let sc = RangeSidecar::read(&sc_path)?;
        if idx > 0 {
            let prev: &RangeSidecar = &sidecars[idx - 1];
            ensure!(
                sc.from == prev.to + 1,
                "shard {} ({}..={}) is not contiguous with previous shard ({}..={})",
                shard.display(),
                sc.from,
                sc.to,
                prev.from,
                prev.to
            );
            ensure!(
                sc.sample == sidecars[0].sample,
                "shard {} has sample {:?}, expected {:?}",
                shard.display(),
                sc.sample,
                sidecars[0].sample
            );
        }
        sidecars.push(sc);
    }

    // Bijection between --c-lines ranges and sparse-only shards.
    let mut c_map: BTreeMap<(i64, i64), &CLinesSpec> = BTreeMap::new();
    for cl in c_lines {
        ensure!(
            c_map.insert((cl.from, cl.to), cl).is_none(),
            "duplicate --c-lines for range {}..{}",
            cl.from,
            cl.to
        );
        let matched = sidecars.iter().find(|sc| sc.from == cl.from && sc.to == cl.to);
        match matched {
            None => bail!(
                "--c-lines {}:{}..{} matches no shard's range",
                cl.path.display(),
                cl.from,
                cl.to
            ),
            Some(sc) if !sc.sparse_only => bail!(
                "--c-lines {}:{}..{}: matching shard is not sparse-only",
                cl.path.display(),
                cl.from,
                cl.to
            ),
            Some(_) => {}
        }
    }
    for (shard, sc) in shards.iter().zip(&sidecars) {
        if sc.sparse_only && !c_map.contains_key(&(sc.from, sc.to)) {
            bail!(
                "sparse-only shard {} ({}..={}) has no matching --c-lines",
                shard.display(),
                sc.from,
                sc.to
            );
        }
    }

    let sample_spec = match sidecars[0].sample.as_deref() {
        Some(raw) => Some(SampleSpec::parse(raw)?),
        None => None,
    };
    let mut coin_set = BTreeSet::new();
    let mut first_h: Option<(Vec<u8>, Header)> = None;
    let mut boundaries = BTreeSet::new();
    let mut all_c_lines: Vec<Vec<u8>> = Vec::new();
    let mut sparse_records: Vec<JoinedSparse> = Vec::new();
    let mut seen_rotationinfo: BTreeMap<i64, (usize, Vec<u8>)> = BTreeMap::new();

    for (shard_idx, (shard_path, sc)) in shards.iter().zip(&sidecars).enumerate() {
        let mut lines = Records::raw(shard_path)?;
        let (_, h_bytes) = lines
            .next()
            .ok_or_else(|| anyhow!("{}: empty shard", shard_path.display()))?
            .with_context(|| format!("reading {}", shard_path.display()))?;
        let header = parse_golden_header(shard_path, &h_bytes)?;

        if let Some((want_bytes, _)) = &first_h {
            ensure!(
                &h_bytes == want_bytes,
                "{}: H record is not byte-identical to {}",
                shard_path.display(),
                shards[0].display()
            );
        } else {
            boundaries = ExtractOptions::default_boundaries(header.chain);
            boundaries.extend(extra_boundaries);
            if let Some(spec) = &sample_spec {
                coin_set = sample_set(spec, &boundaries);
            }
            first_h = Some((h_bytes.clone(), header));
        }

        sc.verify(&boundaries)
            .with_context(|| format!("verifying {}", sidecar_path(shard_path).display()))?;

        let mut shard_cs: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut saw_sparse = false;
        let mut within_seq = 0usize;

        for item in lines {
            let (line_no, raw_line) = item?;
            let rec = parse_line(&raw_line)
                .with_context(|| format!("{}:{line_no}", shard_path.display()))?;
            match rec {
                Record::Outcome(o) if o.kind() == Kind::Connect && o.id.org == Origin::Tip => {
                    ensure!(
                        !saw_sparse,
                        "{}:{line_no}: C record after S record",
                        shard_path.display()
                    );
                    ensure!(
                        !sc.sparse_only,
                        "{}:{line_no}: sparse-only shard contains a C record",
                        shard_path.display()
                    );
                    enforce_coin_pattern(
                        o.id.h,
                        |k| o.s().get(k).is_some(),
                        sample_spec.as_ref(),
                        &coin_set,
                        sc.from,
                        sc.to,
                    )
                    .with_context(|| format!("{}:{line_no}", shard_path.display()))?;
                    shard_cs.push((o.id.h, raw_line));
                }
                Record::Sparse(s) => {
                    saw_sparse = true;
                    let rpc = SparseRpc::from_bytes(&s.rpc).ok_or_else(|| {
                        anyhow!(
                            "{}:{line_no}: unknown sparse rpc {:?}",
                            shard_path.display(),
                            String::from_utf8_lossy(&s.rpc)
                        )
                    })?;
                    if rpc == SparseRpc::Rotationinfo {
                        ensure!(
                            0 <= s.h && s.h <= sc.to,
                            "{}:{line_no}: rotationinfo height {} outside 0..={}",
                            shard_path.display(),
                            s.h,
                            sc.to
                        );
                        if let Some((prev_shard, prev_bytes)) = seen_rotationinfo.get(&s.h) {
                            ensure!(
                                *prev_shard != shard_idx,
                                "{}:{line_no}: duplicate rotationinfo at height {} within one shard",
                                shard_path.display(),
                                s.h
                            );
                            ensure!(
                                *prev_bytes == raw_line,
                                "rotationinfo at height {} differs between {} and {}",
                                s.h,
                                shards[*prev_shard].display(),
                                shard_path.display()
                            );
                        } else {
                            seen_rotationinfo.insert(s.h, (shard_idx, raw_line.clone()));
                            within_seq += 1;
                            sparse_records.push(JoinedSparse {
                                key: (s.h, rpc, shard_idx, within_seq),
                                line: raw_line,
                            });
                        }
                    } else {
                        ensure!(
                            (sc.from..=sc.to).contains(&s.h),
                            "{}:{line_no}: {} at height {} outside shard range {}..={}",
                            shard_path.display(),
                            rpc.name(),
                            s.h,
                            sc.from,
                            sc.to
                        );
                        within_seq += 1;
                        sparse_records.push(JoinedSparse {
                            key: (s.h, rpc, shard_idx, within_seq),
                            line: raw_line,
                        });
                    }
                }
                other => bail!(
                    "{}:{line_no}: unexpected record kind {:?}",
                    shard_path.display(),
                    other.kind()
                ),
            }
        }

        if sc.sparse_only {
            let cl = c_map[&(sc.from, sc.to)];
            let (want_h_bytes, _) = first_h.as_ref().expect("first_h populated");
            let supplied = read_c_lines(&cl.path, cl.from, cl.to, want_h_bytes)?;
            for line in &supplied {
                let Record::Outcome(o) =
                    parse_line(line).with_context(|| format!("--c-lines {}", cl.path.display()))?
                else {
                    bail!("--c-lines {}: expected a C record", cl.path.display());
                };
                enforce_coin_pattern(
                    o.id.h,
                    |k| o.s().get(k).is_some(),
                    sample_spec.as_ref(),
                    &coin_set,
                    cl.from,
                    cl.to,
                )
                .with_context(|| format!("--c-lines {}", cl.path.display()))?;
            }
            all_c_lines.extend(supplied);
        } else {
            let want_len = usize::try_from(sc.to - sc.from + 1).context("shard range length")?;
            ensure!(
                shard_cs.len() == want_len,
                "{}: expected {want_len} C records for {}..={}, got {}",
                shard_path.display(),
                sc.from,
                sc.to,
                shard_cs.len()
            );
            for (offset, (h, raw_c)) in shard_cs.into_iter().enumerate() {
                let want_h = sc.from + offset as i64;
                ensure!(
                    h == want_h,
                    "{}: expected C record at height {want_h}, got {h}",
                    shard_path.display()
                );
                all_c_lines.push(raw_c);
            }
        }
    }

    sparse_records.sort_by_key(|s| s.key);
    let (h_line, _) = first_h.expect("shards is non-empty");
    let joined_sidecar = RangeSidecar::build(
        sidecars[0].from,
        sidecars.last().expect("shards is non-empty").to,
        false,
        sidecars[0].sample.clone(),
        &boundaries,
    )?;

    write_golden_and_sidecar(out, |writer| {
        writer.write_all(&h_line).context("writing H record")?;
        for c_line in &all_c_lines {
            writer.write_all(c_line).context("writing C record")?;
        }
        for s in &sparse_records {
            writer.write_all(&s.line).context("writing S record")?;
        }
        Ok((joined_sidecar.clone(), joined_sidecar))
    })
}

// EMBARGO-1
//! The private corpora's manifests (CS0 §9.4, §10).
//!
//! A test reads private files only through a manifest that names each file with its SHA-256, so
//! a missing, altered, unlisted or empty corpus fails instead of passing quietly.
//!
//! **Case manifest** (`<dir>/MANIFEST`), for a private test's cases:
//!
//! ```text
//! cases <N>
//! min <M>
//! <sha256>  cases/<name>
//! ```
//!
//! `M ≥ 1` is a floor that only rises as cases are added; `N < M` fails, so an empty corpus
//! always fails. Every case is a file directly in `cases/`, and no path or SHA-256 appears twice,
//! so each entry counts one distinct case.
//!
//! **Seed manifest** (`<fuzz dir>/MANIFEST`), for the private seeds of the L1 targets:
//!
//! ```text
//! min <M>
//! target <name> pending|required
//! <sha256>  <name>/seeds/<file>
//! ```
//!
//! `required` targets must exist in the tree under test; `pending` ones (written before their
//! task merges) are replayed when they exist and skipped otherwise. The manager flips a task's
//! entries to `required` at its merge (DEC-32). The seeds of `required` targets must number at
//! least `M ≥ 1`. Within a target no path or SHA-256 appears twice.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use crate::sha256_hex;

/// Why a private corpus does not pass its manifest. The messages name no file contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// No `MANIFEST`, or it could not be read.
    Missing,
    /// A line that does not parse, or an entry that repeats an earlier path or SHA-256 or lies
    /// outside its directory (1-based).
    Malformed {
        line: usize,
    },
    /// `min` is 0 or the corpus has fewer entries than `min`.
    BelowMinimum {
        entries: usize,
        min: u64,
    },
    /// `cases <N>` disagrees with the number of entries.
    CountMismatch {
        declared: u64,
        entries: usize,
    },
    /// A listed file is missing (its 0-based entry index).
    MissingFile {
        entry: usize,
    },
    /// A listed file's SHA-256 differs.
    Altered {
        entry: usize,
    },
    /// Files or seed directories present but not listed.
    Unlisted {
        count: usize,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("the manifest is missing or unreadable"),
            Self::Malformed {
                line,
            } => write!(f, "manifest line {line} is malformed, repeated or outside its directory"),
            Self::BelowMinimum {
                entries,
                min,
            } => write!(
                f,
                "{entries} entries, below the minimum {min} (and the minimum must be at least 1)"
            ),
            Self::CountMismatch {
                declared,
                entries,
            } => write!(f, "the manifest declares {declared} cases but lists {entries}"),
            Self::MissingFile {
                entry,
            } => write!(f, "entry {entry}: the file is missing"),
            Self::Altered {
                entry,
            } => write!(f, "entry {entry}: the file's SHA-256 differs"),
            Self::Unlisted {
                count,
            } => write!(f, "{count} files or directories are not in the manifest"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn malformed(line: usize) -> ManifestError {
    ManifestError::Malformed {
        line,
    }
}

struct Entry {
    line: usize,
    sha256: String,
    path: String,
}

fn parse_entry(line: &str, number: usize) -> Result<Entry, ManifestError> {
    let (sha256, path) = line.split_once("  ").ok_or(malformed(number))?;
    let valid_digest = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
    let valid_path = !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    if !valid_digest || !valid_path {
        return Err(malformed(number));
    }
    Ok(Entry {
        line: number,
        sha256: sha256.to_ascii_lowercase(),
        path: path.to_owned(),
    })
}

/// Fails on the first entry that repeats an earlier entry's path or SHA-256, so a corpus cannot
/// reach its count or floor by listing one file twice or one content under two names.
fn check_distinct(entries: &[Entry]) -> Result<(), ManifestError> {
    let (mut paths, mut digests) = (BTreeSet::new(), BTreeSet::new());
    for entry in entries {
        if !paths.insert(entry.path.as_str()) || !digests.insert(entry.sha256.as_str()) {
            return Err(malformed(entry.line));
        }
    }
    Ok(())
}

fn parse_number(line: &str, key: &str, number: usize) -> Result<u64, ManifestError> {
    line.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|value| value.parse().ok())
        .ok_or(malformed(number))
}

fn read_manifest(dir: &Path) -> Result<Vec<(usize, String)>, ManifestError> {
    let text = std::fs::read_to_string(dir.join("MANIFEST")).map_err(|_| ManifestError::Missing)?;
    Ok(text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim_end().to_owned()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .collect())
}

fn read_listed(dir: &Path, entries: &[Entry]) -> Result<Vec<Vec<u8>>, ManifestError> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let bytes =
                std::fs::read(dir.join(&entry.path)).map_err(|_| ManifestError::MissingFile {
                    entry: index,
                })?;
            if sha256_hex(&bytes) != entry.sha256 {
                return Err(ManifestError::Altered {
                    entry: index,
                });
            }
            Ok(bytes)
        })
        .collect()
}

/// The names of the regular files and directories directly inside `dir` (none when it does not
/// exist).
fn names_in(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Loads a private test's cases through `<dir>/MANIFEST`, in manifest order.
pub fn load_cases(dir: &Path) -> Result<Vec<Vec<u8>>, ManifestError> {
    let lines = read_manifest(dir)?;
    let mut lines = lines.into_iter();
    let (n1, first) = lines.next().ok_or(malformed(1))?;
    let declared = parse_number(&first, "cases", n1)?;
    let (n2, second) = lines.next().ok_or(malformed(n1 + 1))?;
    let min = parse_number(&second, "min", n2)?;
    let entries = lines.map(|(n, line)| parse_entry(&line, n)).collect::<Result<Vec<_>, _>>()?;
    let in_cases =
        |e: &Entry| e.path.strip_prefix("cases/").is_some_and(|name| !name.contains('/'));
    if let Some(outside) = entries.iter().find(|e| !in_cases(e)) {
        return Err(malformed(outside.line));
    }
    check_distinct(&entries)?;
    if min == 0 || (entries.len() as u64) < min {
        return Err(ManifestError::BelowMinimum {
            entries: entries.len(),
            min,
        });
    }
    if declared != entries.len() as u64 {
        return Err(ManifestError::CountMismatch {
            declared,
            entries: entries.len(),
        });
    }
    let listed: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let unlisted = names_in(&dir.join("cases"))
        .into_iter()
        .filter(|name| !listed.contains(format!("cases/{name}").as_str()))
        .count();
    if unlisted > 0 {
        return Err(ManifestError::Unlisted {
            count: unlisted,
        });
    }
    read_listed(dir, &entries)
}

/// Whether a target's seeds must be replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedState {
    /// Written before the target's task merged: replayed when the target exists.
    Pending,
    /// The target's task has merged: the target must exist and its seeds must pass.
    Required,
}

/// One target's private seeds, read and checked against the manifest.
pub struct SeedTarget {
    pub name: String,
    pub state: SeedState,
    pub seeds: Vec<Vec<u8>>,
}

/// The seed manifest's contents.
pub struct Seeds {
    pub min: u64,
    pub targets: Vec<SeedTarget>,
}

/// Loads every target's private seeds through `<fuzz dir>/MANIFEST`.
pub fn load_seeds(fuzz_dir: &Path) -> Result<Seeds, ManifestError> {
    let lines = read_manifest(fuzz_dir)?;
    let mut lines = lines.into_iter();
    let (n1, first) = lines.next().ok_or(malformed(1))?;
    let min = parse_number(&first, "min", n1)?;

    let mut targets: Vec<(String, SeedState, Vec<Entry>)> = Vec::new();
    for (number, line) in lines {
        if let Some(rest) = line.strip_prefix("target ") {
            let (name, state) = rest.split_once(' ').ok_or(malformed(number))?;
            let state = match state {
                "pending" => SeedState::Pending,
                "required" => SeedState::Required,
                _ => {
                    return Err(malformed(number));
                }
            };
            let valid_name = !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && targets.iter().all(|(other, _, _)| other != name);
            if !valid_name {
                return Err(malformed(number));
            }
            targets.push((name.to_owned(), state, Vec::new()));
        } else {
            let entry = parse_entry(&line, number)?;
            let Some((name, _, entries)) = targets.last_mut() else {
                return Err(malformed(number));
            };
            if !entry.path.starts_with(&format!("{name}/seeds/")) {
                return Err(malformed(number));
            }
            entries.push(entry);
        }
    }

    for (_, _, entries) in &targets {
        check_distinct(entries)?;
    }

    let required: usize = targets
        .iter()
        .filter(|(_, state, _)| *state == SeedState::Required)
        .map(|(_, _, e)| e.len())
        .sum();
    if min == 0 || (required as u64) < min {
        return Err(ManifestError::BelowMinimum {
            entries: required,
            min,
        });
    }

    // Every seed directory and every file in it must be listed.
    let mut unlisted = 0;
    for dir_name in names_in(fuzz_dir) {
        let seeds_dir = fuzz_dir.join(&dir_name).join("seeds");
        if !seeds_dir.is_dir() {
            continue;
        }
        match targets.iter().find(|(name, _, _)| *name == dir_name) {
            None => unlisted += 1,
            Some((_, _, entries)) => {
                let listed: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
                unlisted += names_in(&seeds_dir)
                    .into_iter()
                    .filter(|file| !listed.contains(format!("{dir_name}/seeds/{file}").as_str()))
                    .count();
            }
        }
    }
    if unlisted > 0 {
        return Err(ManifestError::Unlisted {
            count: unlisted,
        });
    }

    let targets = targets
        .into_iter()
        .map(|(name, state, entries)| {
            Ok(SeedTarget {
                seeds: read_listed(fuzz_dir, &entries)?,
                name,
                state,
            })
        })
        .collect::<Result<_, ManifestError>>()?;
    Ok(Seeds {
        min,
        targets,
    })
}

/// Writers of synthetic corpora, for the tests of the private-test drivers. They hold no private
/// data, so they write ordinary files and run on every platform.
pub mod testing {
    use std::path::Path;

    use crate::sha256_hex;

    fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, bytes);
    }

    /// A case corpus in `dir` (emptied first): `header` (the `cases`/`min` lines), each of
    /// `cases` listed as `cases/<i>`, and each of `unlisted` written but not listed.
    pub fn write_case_corpus(dir: &Path, header: &str, cases: &[&[u8]], unlisted: &[&[u8]]) {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::create_dir_all(dir.join("cases"));
        let mut manifest = String::from(header);
        for (i, case) in cases.iter().enumerate() {
            manifest.push_str(&format!("{}  cases/{i}\n", sha256_hex(case)));
            write(dir, &format!("cases/{i}"), case);
        }
        for (i, extra) in unlisted.iter().enumerate() {
            write(dir, &format!("cases/unlisted-{i}"), extra);
        }
        write(dir, "MANIFEST", manifest.as_bytes());
    }

    /// A seed corpus in `dir` (emptied first). `manifest` is the manifest text, except that a
    /// line `seed <path>` becomes `<sha256 of that file>  <path>`; `files` are written as given.
    pub fn write_seed_corpus(dir: &Path, manifest: &str, files: &[(&str, &[u8])]) {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::create_dir_all(dir);
        for (rel, bytes) in files {
            write(dir, rel, bytes);
        }
        let mut text = String::new();
        for line in manifest.lines() {
            match line.strip_prefix("seed ") {
                Some(rel) => {
                    let bytes = files.iter().find(|(r, _)| *r == rel).map_or(&b""[..], |(_, b)| *b);
                    text.push_str(&format!("{}  {rel}\n", sha256_hex(bytes)));
                }
                None => text.push_str(&format!("{line}\n")),
            }
        }
        write(dir, "MANIFEST", text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/tmp/node-sink-manifest").join(name)
    }

    fn case_dir(name: &str, header: &str, cases: &[&[u8]], unlisted: &[&[u8]]) -> PathBuf {
        let dir = scratch(name);
        testing::write_case_corpus(&dir, header, cases, unlisted);
        dir
    }

    #[test]
    fn a_complete_case_corpus_loads_in_order() {
        let dir = case_dir("ok", "cases 2\nmin 2\n", &[b"two", b"one"], &[]);
        assert_eq!(load_cases(&dir), Ok(vec![b"two".to_vec(), b"one".to_vec()]));
    }

    #[test]
    fn an_empty_or_short_case_corpus_fails() {
        let empty = case_dir("empty", "cases 0\nmin 0\n", &[], &[]);
        assert!(matches!(load_cases(&empty), Err(ManifestError::BelowMinimum { .. })));
        let short = case_dir("short", "cases 1\nmin 2\n", &[b"one"], &[]);
        assert!(matches!(load_cases(&short), Err(ManifestError::BelowMinimum { .. })));
        let missing = scratch("no-manifest");
        assert_eq!(load_cases(&missing), Err(ManifestError::Missing));
    }

    #[test]
    fn missing_altered_unlisted_and_miscounted_cases_fail() {
        let dir = case_dir("missing", "cases 1\nmin 1\n", &[b"one"], &[]);
        let _ = std::fs::remove_file(dir.join("cases/0"));
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::MissingFile {
                entry: 0
            })
        );

        let dir = case_dir("altered", "cases 1\nmin 1\n", &[b"one"], &[]);
        let _ = std::fs::write(dir.join("cases/0"), b"changed");
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Altered {
                entry: 0
            })
        );

        let dir = case_dir("unlisted", "cases 1\nmin 1\n", &[b"one"], &[b"extra"]);
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Unlisted {
                count: 1
            })
        );

        let dir = case_dir("count", "cases 3\nmin 1\n", &[b"one"], &[]);
        assert!(matches!(load_cases(&dir), Err(ManifestError::CountMismatch { .. })));
    }

    #[test]
    fn repeated_or_misplaced_cases_fail() {
        // The same path with two digests: without the path check this would be `Altered`.
        let (one, two) = (sha256_hex(b"one"), sha256_hex(b"two"));
        let dir = case_dir("repeated-path", "cases 1\nmin 1\n", &[b"one"], &[]);
        let _ = std::fs::write(
            dir.join("MANIFEST"),
            format!("cases 2\nmin 2\n{one}  cases/0\n{two}  cases/0\n"),
        );
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Malformed {
                line: 4
            })
        );

        let dir = case_dir("repeated-digest", "cases 2\nmin 2\n", &[b"one", b"one"], &[]);
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Malformed {
                line: 4
            })
        );

        let dir = case_dir("outside", "cases 1\nmin 1\n", &[], &[]);
        let _ = std::fs::write(dir.join("PROVENANCE.md"), b"one");
        let _ =
            std::fs::write(dir.join("MANIFEST"), format!("cases 1\nmin 1\n{one}  PROVENANCE.md\n"));
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Malformed {
                line: 3
            })
        );

        let _ = std::fs::create_dir_all(dir.join("cases/sub"));
        let _ = std::fs::write(dir.join("cases/sub/x"), b"one");
        let _ =
            std::fs::write(dir.join("MANIFEST"), format!("cases 1\nmin 1\n{one}  cases/sub/x\n"));
        assert_eq!(
            load_cases(&dir),
            Err(ManifestError::Malformed {
                line: 3
            })
        );
    }

    fn seed_dir(name: &str, manifest: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = scratch(name);
        testing::write_seed_corpus(&dir, manifest, files);
        dir
    }

    #[test]
    fn seeds_load_with_their_states() {
        let dir = seed_dir(
            "seeds-ok",
            "min 1\ntarget t1 required\nseed t1/seeds/a\ntarget t2 pending\nseed t2/seeds/b\n",
            &[("t1/seeds/a", b"x"), ("t2/seeds/b", b"y")],
        );
        let seeds = load_seeds(&dir).map_err(|e| e.to_string());
        let Ok(seeds) = seeds else {
            panic!("seed manifest rejected");
        };
        assert_eq!(seeds.targets.len(), 2);
        assert_eq!(seeds.targets[0].state, SeedState::Required);
        assert_eq!(seeds.targets[1].state, SeedState::Pending);
        assert_eq!(seeds.targets[1].seeds, vec![b"y".to_vec()]);
    }

    #[test]
    fn seed_manifests_without_required_seeds_or_with_strays_fail() {
        let only_pending = seed_dir(
            "seeds-pending",
            "min 1\ntarget t2 pending\nseed t2/seeds/b\n",
            &[("t2/seeds/b", b"y")],
        );
        assert!(matches!(load_seeds(&only_pending), Err(ManifestError::BelowMinimum { .. })));

        let stray = seed_dir(
            "seeds-stray",
            "min 1\ntarget t1 required\nseed t1/seeds/a\n",
            &[("t1/seeds/a", b"x"), ("t3/seeds/c", b"z")],
        );
        assert!(matches!(
            load_seeds(&stray),
            Err(ManifestError::Unlisted {
                count: 1
            })
        ));

        let outside = seed_dir(
            "seeds-outside",
            "min 1\ntarget t1 required\nseed t2/seeds/b\n",
            &[("t2/seeds/b", b"y")],
        );
        assert!(matches!(load_seeds(&outside), Err(ManifestError::Malformed { .. })));

        let repeated = seed_dir(
            "seeds-repeated",
            "min 2\ntarget t1 required\nseed t1/seeds/a\nseed t1/seeds/b\n",
            &[("t1/seeds/a", b"x"), ("t1/seeds/b", b"x")],
        );
        assert_eq!(
            load_seeds(&repeated).err(),
            Some(ManifestError::Malformed {
                line: 4
            })
        );
    }
}

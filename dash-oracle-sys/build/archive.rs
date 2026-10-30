//! Authenticating a cached oracle archive against `pins.txt` (used by `build.rs`
//! and by `tests/archive_check.rs`).
//!
//! The archive's identity is the sha256 of its bytes. `snapshot` copies the cached
//! archive to a private path first and hashes the copy, so the bytes that get linked
//! are the bytes that were checked, even if another build replaces the cached file
//! in between.

use std::fs;
use std::path::Path;

use bitcoin_hashes::{Hash, sha256};

/// One line of `pins.txt`: `pin commit branch sha256(plain) sha256(fuzz)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub commit: String,
    pub branch: String,
    /// sha256 of the plain and fuzz archives ("-" if not recorded)
    pub sha256: [String; 2],
}

impl Pin {
    /// `dash_oracle_<pin>` ("v24.0" -> "dash_oracle_v24_0").
    pub fn lib_name(&self) -> String {
        format!("dash_oracle_{}", self.name.replace('.', "_"))
    }

    /// The archive's file name, `lib<lib_name>.a`.
    pub fn archive_name(&self) -> String {
        format!("lib{}.a", self.lib_name())
    }

    pub fn symbol_prefix(&self) -> String {
        format!("oracle_{}_", self.name.replace('.', "_"))
    }

    /// The recorded sha256 for `flavor`, or None if pins.txt has none ("-").
    pub fn expected_sha256(&self, flavor: &str) -> Option<&str> {
        let column = match flavor {
            "plain" => 0,
            "fuzz" => 1,
            _ => return None,
        };
        let sha = &self.sha256[column];
        (sha != "-").then_some(sha.as_str())
    }
}

/// Every non-comment line of pins.txt.
pub fn parse_pins(text: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [name, commit, branch, plain, fuzz] = f[..] else {
            return Err(format!("pins.txt line {line:?}: expected 5 columns"));
        };
        for sha in [plain, fuzz] {
            if sha != "-" && !(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())) {
                return Err(format!("pins.txt line {line:?}: {sha:?} is not a sha256"));
            }
        }
        pins.push(Pin {
            name: name.into(),
            commit: commit.into(),
            branch: branch.into(),
            sha256: [plain.into(), fuzz.into()],
        });
    }
    if pins.is_empty() {
        return Err("pins.txt lists no pins".into());
    }
    Ok(pins)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    sha256::Hash::hash(bytes).to_string()
}

/// The directory under `<cache>/<commit>/` that holds `flavor`'s archive: the flavor's
/// name, except `fuzz-nocmp` for fuzz (BL-371, relic and dashbls without trace-cmp). A fuzz
/// recipe change that moves its sha256 takes a new name, so a tree still pinned to the old
/// archive keeps finding it (contrib/node/build-oracle.sh keeps the same table).
pub fn cache_dir_name(flavor: &str) -> &str {
    match flavor {
        "fuzz" => "fuzz-nocmp",
        other => other,
    }
}

/// What `snapshot` copied to its destination.
#[derive(Debug)]
pub struct Snapshot {
    pub sha256: String,
    /// Whether `sha256` is the one pins.txt records (false only with `allow_unpinned`).
    pub pinned: bool,
}

/// Check that `cache_dir` (`<cache>/<commit>/<cache_dir_name(flavor)>`) holds the archive `pin`
/// records for `flavor`, and copy it to `dest`. Fails if the MANIFEST does not name
/// this pin, commit, flavor and symbol prefix, or if the copied bytes do not hash to
/// the recorded sha256. With `allow_unpinned`, an unrecorded ("-") or different
/// sha256 is accepted and reported as `pinned: false`.
pub fn snapshot(
    cache_dir: &Path,
    pin: &Pin,
    flavor: &str,
    dest: &Path,
    allow_unpinned: bool,
) -> Result<Snapshot, String> {
    // ubsan is a development flavor: never pinned, so it links only unpinned.
    if !matches!(flavor, "plain" | "fuzz" | "ubsan") {
        return Err(format!("unknown flavor {flavor:?} (plain, fuzz or ubsan)"));
    }
    let manifest_path = cache_dir.join("MANIFEST");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    for (key, want) in [
        ("pin", pin.name.as_str()),
        ("commit", pin.commit.as_str()),
        ("flavor", flavor),
        ("symbol_prefix", pin.symbol_prefix().as_str()),
    ] {
        let got = manifest.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' '));
        if got != Some(want) {
            return Err(format!(
                "{}: {key} is {got:?}, expected {want:?}",
                manifest_path.display()
            ));
        }
    }

    let archive = cache_dir.join(pin.archive_name());
    fs::copy(&archive, dest)
        .map_err(|e| format!("copy {} to {}: {e}", archive.display(), dest.display()))?;
    let bytes = fs::read(dest).map_err(|e| format!("read {}: {e}", dest.display()))?;
    let sha256 = sha256_hex(&bytes);
    let expected = pin.expected_sha256(flavor);
    let pinned = expected == Some(sha256.as_str());
    if !pinned && !allow_unpinned {
        let _ = fs::remove_file(dest);
        return Err(match expected {
            Some(expected) => format!(
                "{} has sha256 {sha256}, but pins.txt records {expected} for {} ({flavor}); \
                 refusing to link it. Rebuild with contrib/node/build-oracle.sh --force {}",
                archive.display(),
                pin.name,
                pin.name
            ),
            None => format!(
                "pins.txt records no {flavor} sha256 for {}, so {} cannot be authenticated; \
                 set DASH_ORACLE_ALLOW_UNPINNED=1 to link it anyway (development only)",
                pin.name,
                archive.display()
            ),
        });
    }
    Ok(Snapshot {
        sha256,
        pinned,
    })
}

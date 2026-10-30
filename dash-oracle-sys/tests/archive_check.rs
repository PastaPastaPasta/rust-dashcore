//! Regression tests for the archive authentication in `build.rs` (O1 review B1): a
//! cached archive is linked only if the sha256 of its bytes is the one pins.txt
//! records, whatever its MANIFEST claims.

#[path = "../build/archive.rs"]
mod archive;

use std::fs;
use std::path::{Path, PathBuf};

use archive::{Pin, cache_dir_name, parse_pins, sha256_hex, snapshot};

/// A fresh scratch directory under the target directory (not /tmp).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("archive_check").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A cache directory as build-oracle.sh publishes it, holding `bytes` as the archive,
/// and the pin recording `recorded` (None: "-") as its plain sha256.
fn cache_with(name: &str, bytes: &[u8], recorded: Option<&str>) -> (PathBuf, Pin) {
    let pin = Pin {
        name: "v24.0".into(),
        commit: "6d5c9ac2ed1bbd4325f72d67d21326e7307f29be".into(),
        branch: "node/oracle-shim-v24.0".into(),
        sha256: [recorded.unwrap_or("-").into(), "-".into()],
    };
    let dir = scratch(name);
    fs::write(dir.join(pin.archive_name()), bytes).unwrap();
    fs::write(
        dir.join("MANIFEST"),
        format!(
            "pin {}\ncommit {}\nflavor plain\nsymbol_prefix {}\nsha256 {}\n",
            pin.name,
            pin.commit,
            pin.symbol_prefix(),
            sha256_hex(bytes)
        ),
    )
    .unwrap();
    (dir, pin)
}

#[test]
fn the_pinned_archive_is_linked() {
    let bytes = b"!<arch>\nthe pinned archive";
    let (dir, pin) = cache_with("pinned", bytes, Some(&sha256_hex(bytes)));
    let dest = dir.join("linked.a");
    let s = snapshot(&dir, &pin, "plain", &dest, false).unwrap();
    assert!(s.pinned);
    assert_eq!(s.sha256, sha256_hex(bytes));
    assert_eq!(fs::read(&dest).unwrap(), bytes);
}

#[test]
fn a_substituted_archive_with_the_original_manifest_is_refused() {
    // The review's counterexample: the archive is replaced, the MANIFEST (and so its
    // sha256 line) is left as it was.
    let original = b"!<arch>\nthe pinned archive";
    let (dir, pin) = cache_with("substituted", original, Some(&sha256_hex(original)));
    fs::write(dir.join(pin.archive_name()), b"!<arch>\nreturns 42").unwrap();
    let dest = dir.join("linked.a");
    let err = snapshot(&dir, &pin, "plain", &dest, false).unwrap_err();
    assert!(err.contains("refusing to link"), "{err}");
    assert!(!dest.exists(), "a refused archive must not be left where the linker looks");
}

#[test]
fn an_archive_with_a_one_bit_difference_is_refused() {
    let mut bytes = b"!<arch>\nthe pinned archive".to_vec();
    let recorded = sha256_hex(&bytes);
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let (dir, pin) = cache_with("bitflip", &bytes, Some(&recorded));
    assert!(snapshot(&dir, &pin, "plain", &dir.join("linked.a"), false).is_err());
}

#[test]
fn a_manifest_for_another_pin_commit_or_flavor_is_refused() {
    let bytes = b"!<arch>\nthe pinned archive";
    for (field, value) in [
        ("pin", "v25.0"),
        ("commit", "684cb425e2bbd2eb11b842083ac6754004495abc"),
        ("flavor", "fuzz"),
        ("symbol_prefix", "oracle_v25_0_"),
    ] {
        let (dir, pin) = cache_with(&format!("manifest-{field}"), bytes, Some(&sha256_hex(bytes)));
        let manifest = fs::read_to_string(dir.join("MANIFEST")).unwrap();
        let edited: String = manifest
            .lines()
            .map(|l| {
                if l.split(' ').next() == Some(field) {
                    format!("{field} {value}\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        fs::write(dir.join("MANIFEST"), edited).unwrap();
        let err = snapshot(&dir, &pin, "plain", &dir.join("linked.a"), false).unwrap_err();
        assert!(err.contains(field), "{field}: {err}");
    }
}

#[test]
fn unrecorded_or_different_archives_need_the_explicit_opt_in() {
    let bytes = b"!<arch>\na development build";
    let (dir, pin) = cache_with("unrecorded", bytes, None);
    let dest = dir.join("linked.a");
    let err = snapshot(&dir, &pin, "plain", &dest, false).unwrap_err();
    assert!(err.contains("DASH_ORACLE_ALLOW_UNPINNED"), "{err}");
    let s = snapshot(&dir, &pin, "plain", &dest, true).unwrap();
    assert!(!s.pinned, "an opted-in archive is reported as unpinned");

    let (dir, pin) = cache_with("different", bytes, Some(&sha256_hex(b"something else")));
    let s = snapshot(&dir, &pin, "plain", &dir.join("linked.a"), true).unwrap();
    assert!(!s.pinned);
}

#[test]
fn the_fuzz_flavor_is_checked_against_its_own_column() {
    let bytes = b"!<arch>\nthe fuzz archive";
    let (dir, mut pin) = cache_with("fuzz", bytes, Some(&sha256_hex(b"the plain archive")));
    let manifest =
        fs::read_to_string(dir.join("MANIFEST")).unwrap().replace("flavor plain", "flavor fuzz");
    fs::write(dir.join("MANIFEST"), manifest).unwrap();
    assert!(snapshot(&dir, &pin, "fuzz", &dir.join("linked.a"), false).is_err());
    pin.sha256[1] = sha256_hex(bytes);
    assert!(snapshot(&dir, &pin, "fuzz", &dir.join("linked.a"), false).unwrap().pinned);
}

#[test]
fn an_unknown_flavor_is_refused() {
    let bytes = b"!<arch>\nthe pinned archive";
    let (dir, pin) = cache_with("flavor", bytes, Some(&sha256_hex(bytes)));
    let err = snapshot(&dir, &pin, "Fuzz", &dir.join("linked.a"), true).unwrap_err();
    assert!(err.contains("unknown flavor"), "{err}");
}

#[test]
fn pins_txt_parses_and_records_every_digest() {
    let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/pins.txt")).unwrap();
    let pins = parse_pins(&text).unwrap();
    assert_eq!(pins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["v24.0"]);
    for pin in &pins {
        for flavor in ["plain", "fuzz"] {
            assert!(
                pin.expected_sha256(flavor).is_some(),
                "{} {flavor}: pins.txt records no sha256",
                pin.name
            );
        }
    }
    assert!(parse_pins("v1 c b nothex -\n").is_err());
    assert!(parse_pins("# only comments\n").is_err());
}

#[test]
#[cfg(target_os = "linux")] // build-oracle.sh is Linux-only (GNU coreutils and binutils)
fn build_oracle_publishes_where_build_rs_looks() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/pins.txt")).unwrap();
    let pin = &parse_pins(&text).unwrap()[0];
    for flavor in ["plain", "fuzz"] {
        let out = std::process::Command::new("bash")
            .arg(Path::new(root).join("contrib/node/build-oracle.sh"))
            .args(["--print-dir", "--flavor", flavor, "--cache", "/cache", &pin.name])
            .output()
            .unwrap();
        assert!(out.status.success(), "{flavor}: {}", String::from_utf8_lossy(&out.stderr));
        let want = format!("/cache/{}/{}\n", pin.commit, cache_dir_name(flavor));
        assert_eq!(String::from_utf8(out.stdout).unwrap(), want, "{flavor}");
    }
    assert_eq!(cache_dir_name("fuzz"), "fuzz-nocmp"); // not the pre-BL-371 fuzz archive's directory
}

//! `conformance/scripts/suffix-check.sh`, the nightly mainnet suffix check (CH3a D167, DEC-357),
//! over a stub `dash-conformance` and a cache of small files under the installs' names. It passes
//! only when `digest l2` exits 0 and the compare exits 4 with our side holding `muhash` at 2 pairs
//! and each evo key at 0; an input that does not match its SHA256SUMS exits 2 before anything
//! runs; and a log that exists is refused, never read.
//!
//! One test: the stub is executed while it is a new file, which a concurrent test's fork could
//! hold open for writing (ETXTBSY).
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/suffix-check.sh");

/// The compare's listing as the mainnet run printed it (D169 (b)).
const LISTING: &str = "\
CLEAN EXCEPT THE EXCLUDED KEYS
excluded, not compared (pairs holding the key: left, right):
  C.s.mnl        0, 25001
  C.s.qc         0, 25001
  C.s.cp         0, 25001
  C.s.ehf        0, 25001
  C.s.muhash     2, 25001
";

/// Logs its arguments, and exits as the case says; its compare prints the case's listing.
const STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_ARGS"
case "$2" in
l2) exit "$STUB_L2" ;;
compare) cat "$STUB_LISTING"; exit "$STUB_COMPARE" ;;
esac
exit 99
"#;

struct Rig {
    dir: tempfile::TempDir,
    /// Each install's stored file and its SHA256SUMS, in the script's order.
    installs: Vec<(PathBuf, PathBuf)>,
}

impl Rig {
    /// The cache, with each install the script names holding one small file and its SHA256SUMS.
    fn new() -> Rig {
        // Under the target directory: a `noexec` TMPDIR would refuse the stub.
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let stub = dir.path().join("dash-conformance");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(dir.path().join("out")).unwrap();
        let mut installs = Vec::new();
        for line in fs::read_to_string(SCRIPT).unwrap().lines() {
            let Some((var, name)) = line.split_once("=$cache/") else {
                continue;
            };
            let install = dir.path().join("cache").join(name);
            let (at, file) = match var {
                "headers" => (install.clone(), "mainnet-headers.bin"),
                "golden" => (install.clone(), "golden.jsonl.zst"),
                "blocks" => (install.join("blocks"), "blk00000.dat"),
                "utxo" => (install.clone(), "utxo-599999.dat"),
                _ => panic!("an install the test does not know: {line}"),
            };
            fs::create_dir_all(&at).unwrap();
            fs::write(at.join(file), var).unwrap();
            let o = Command::new("sha256sum").arg(file).current_dir(&at).output().unwrap();
            let mut sums = String::from_utf8(o.stdout).unwrap();
            if var == "golden" {
                sums += &format!("{}  golden.jsonl (uncompressed, not stored)\n", "0".repeat(64));
            }
            fs::write(install.join("SHA256SUMS"), sums).unwrap();
            installs.push((at.join(file), install.join("SHA256SUMS")));
        }
        assert_eq!(installs.len(), 4);
        Rig {
            dir,
            installs,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The script's exit code and its output, with the stub exiting `l2` and `compare`.
    fn run(&self, name: &str, l2: i32, compare: i32, listing: &str) -> (Option<i32>, String) {
        let listing_path = self.path(&format!("{name}.listing"));
        fs::write(&listing_path, listing).unwrap();
        let o = Command::new("bash")
            .arg(SCRIPT)
            .args([self.path("dash-conformance"), self.path("out")])
            .arg(name)
            .arg(self.path("cache"))
            .env("DASH_EMBARGO_DIR", self.dir.path())
            .env("STUB_ARGS", self.path(&format!("{name}.args")))
            .env("STUB_L2", l2.to_string())
            .env("STUB_COMPARE", compare.to_string())
            .env("STUB_LISTING", listing_path)
            .output()
            .unwrap();
        let text = [o.stdout, o.stderr].concat();
        (o.status.code(), String::from_utf8(text).unwrap())
    }

    /// The stub's command lines in the run `name`; none when it never ran.
    fn calls(&self, name: &str) -> Vec<String> {
        let path = self.path(&format!("{name}.args"));
        fs::read_to_string(path).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default()
    }
}

/// CH3a D167: the recipe's guards, each failure alone.
#[test]
fn the_suffix_check_passes_only_on_a_clean_run_and_compare() {
    if !Path::new("/usr/bin/time").exists() {
        eprintln!("skipped: the script runs /usr/bin/time, which this host lacks");
        return;
    }
    let rig = Rig::new();
    let (code, text) = rig.run("good", 0, 4, LISTING);
    assert_eq!(code, Some(0), "{text}");
    assert!(text.starts_with("suffix check: passed ("), "{text}");
    let file = |i: usize| rig.installs[i].0.display().to_string();
    let blocks = rig.installs[2].0.parent().unwrap().display().to_string();
    let out = rig.path("out").display().to_string();
    assert_eq!(
        rig.calls("good"),
        [
            format!(
                "digest l2 --chain main --blocks-dir {blocks} --out {out}/good.jsonl --to 625000 \
                 --assumevalid on --headers {} --par 16 --golden {} --reverse-to 614000 \
                 --start-utxo {}",
                file(0),
                file(1),
                file(3)
            ),
            format!(
                "digest compare --mode linear --no-dump --require \
                 undo,stats,bi,txouts,amount,unspendable,bls,bur,dep --exclude muhash,mnl,qc,cp,ehf \
                 --from 600000 --to 625000 {out}/good.jsonl {}",
                file(1)
            ),
        ]
    );

    // Logs that exist, here a passing run's made read-only: refused before anything runs, though
    // the run and the compare would now fail.
    let log = rig.path("out/good.log");
    for path in [&log, &rig.path("out/good.compare.log")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    }
    let (code, text) = rig.run("good", 1, 1, LISTING);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(
        text,
        format!("suffix check: {} exists: the check writes only new files\n", log.display())
    );
    assert_eq!(rig.calls("good").len(), 2);
    let cmp = rig.path("out/cmp-only.compare.log");
    fs::write(&cmp, "EXIT 4\n").unwrap();
    let (code, text) = rig.run("cmp-only", 0, 1, LISTING);
    assert_eq!((code, text.contains("cmp-only.compare.log exists: ")), (Some(1), true), "{text}");
    assert!(rig.calls("cmp-only").is_empty());
    std::os::unix::fs::symlink(rig.path("nowhere"), rig.path("out/dangling.log")).unwrap();
    let (code, text) = rig.run("dangling", 0, 4, LISTING);
    assert_eq!((code, text.contains("dangling.log exists: ")), (Some(1), true), "{text}");
    assert!(rig.calls("dangling").is_empty());

    let keyed = |from: &str, to: &str| LISTING.replace(from, to);
    let cases = [
        ("l2-failed", 1, 4, LISTING.to_owned(), "suffix check: digest l2 exited 1, see "),
        ("clean", 0, 0, LISTING.to_owned(), "suffix check: the compare exited 0, not 4, see "),
        ("incomplete", 0, 3, LISTING.to_owned(), "suffix check: the compare exited 3, not 4, see "),
        (
            "muhash-everywhere",
            0,
            4,
            keyed("muhash     2,", "muhash     25001,"),
            "suffix check: the log holds muhash at 25001 pairs, not 2: ",
        ),
        (
            "muhash-once",
            0,
            4,
            keyed("muhash     2,", "muhash     1,"),
            "suffix check: the log holds muhash at 1 pairs, not 2: ",
        ),
        (
            "evo-held",
            0,
            4,
            keyed("cp         0,", "cp         7,"),
            "suffix check: the log holds cp at 7 pairs, not 0: V writes it (CH7)",
        ),
        (
            "evo-unlisted",
            0,
            4,
            keyed("  C.s.qc         0, 25001\n", ""),
            "suffix check: the compare lists no qc, see ",
        ),
        ("no-listing", 0, 4, String::new(), "suffix check: the compare lists no muhash, see "),
    ];
    for (name, l2, compare, listing, want) in cases {
        let (code, text) = rig.run(name, l2, compare, &listing);
        assert_eq!(code, Some(1), "{name}: {text}");
        assert!(text.starts_with(want), "{name}: {text}");
    }

    // An input that does not match its SHA256SUMS, or a list that is malformed or missing: exit
    // 2, and nothing runs.
    for (i, (file, sums)) in rig.installs.iter().enumerate() {
        let kept = fs::read(file).unwrap();
        fs::write(file, [&kept[..], b"x"].concat()).unwrap();
        let name = format!("changed-{i}");
        assert_eq!(rig.run(&name, 0, 4, LISTING).0, Some(2), "{}", file.display());
        assert!(rig.calls(&name).is_empty());
        fs::write(file, kept).unwrap();

        let kept = fs::read_to_string(sums).unwrap();
        fs::write(sums, format!("{kept}not a sum\n")).unwrap();
        let name = format!("malformed-{i}");
        assert_eq!(rig.run(&name, 0, 4, LISTING).0, Some(2), "{}", sums.display());
        assert!(rig.calls(&name).is_empty());
        fs::remove_file(sums).unwrap();
        let name = format!("missing-{i}");
        assert_eq!(rig.run(&name, 0, 4, LISTING).0, Some(2), "{}", sums.display());
        assert!(rig.calls(&name).is_empty());
        fs::write(sums, kept).unwrap();
    }
    assert_eq!(rig.run("restored", 0, 4, LISTING).0, Some(0));

    // Usage errors, and no output directory.
    let script = |args: &[PathBuf], embargo: bool| {
        let mut c = Command::new("bash");
        c.arg(SCRIPT).args(args).env_remove("DASH_EMBARGO_DIR");
        if embargo {
            c.env("DASH_EMBARGO_DIR", rig.dir.path());
        }
        c.output().unwrap()
    };
    let (stub, absent) = (rig.path("dash-conformance"), rig.path("absent"));
    for (args, embargo) in
        [(vec![stub.clone()], true), (vec![stub.clone(), rig.path("out")], false)]
    {
        let o = script(&args, embargo);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stderr).starts_with("usage: "), "{args:?}");
    }
    let o = script(&[stub, absent.clone()], true);
    assert_eq!(o.status.code(), Some(1));
    let want = format!("suffix check: {} is not a directory\n", absent.display());
    assert_eq!(String::from_utf8_lossy(&o.stdout), want);
}

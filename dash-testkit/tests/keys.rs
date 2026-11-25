//! `dash_testkit::keys` and the `testkit-keys` CLI (BL875-K). The last test needs dashd
//! (`eval $(python3 contrib/setup-dashd.py)`) and checks the derived keys against Core.

use std::collections::HashSet;
use std::process::Command;

use dash_pkc::bls::{BlsPkBytes, BlsPublicKey, BlsScChia, BlsScIetf, BlsSecretKey};
use dash_testkit::keys::{BasicBlsKey, derive_basic_bls_key};
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N, "{s}");
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

/// The key as the CLI prints it.
fn row(index: u32, key: &BasicBlsKey) -> Value {
    serde_json::json!({
        "index": index,
        "scheme": "basic",
        "sk": hex(key.sk.as_bytes()),
        "pk": hex(key.pk.as_bytes()),
    })
}

#[test]
fn derive_is_deterministic_and_index_separating() {
    let seed = [0x42; 32];
    assert_eq!(derive_basic_bls_key(&seed, 3), derive_basic_bls_key(&seed, 3));
    let pks: HashSet<_> = (0..=64).map(|i| derive_basic_bls_key(&seed, i).pk).collect();
    assert_eq!(pks.len(), 65);
}

#[test]
fn derived_key_round_trips_through_the_basic_scheme() {
    for index in [0, 1, 7, u32::MAX] {
        let key = derive_basic_bls_key(&[7; 32], index);
        let sk = BlsSecretKey::<BlsScIetf>::try_from(key.sk.clone()).unwrap();
        let pk = BlsPublicKey::<BlsScIetf>::try_from(key.pk).unwrap();
        assert_eq!(sk.public_key(), pk, "index {index}");
    }
}

/// `pk` is the basic-scheme encoding of `sk`'s point, and not the legacy encoding of the same
/// point (or of any point a legacy derivation would give).
#[test]
fn derived_key_is_basic_scheme_not_legacy() {
    for index in [0, 1, 7, u32::MAX] {
        let key = derive_basic_bls_key(&[0x42; 32], index);
        let sk = BlsSecretKey::<BlsScIetf>::try_from(key.sk.clone()).unwrap();
        let legacy = sk.to_scheme::<BlsScChia>().unwrap().public_key();
        assert_eq!(key.pk, BlsPkBytes::from(sk.public_key()), "index {index}");
        assert_ne!(key.pk, BlsPkBytes::from(legacy), "index {index}");
    }
}

/// `test-vectors/evo-v24/derive-kat.json`: `{seed, index, scheme, sk, pk}` rows.
fn kat_rows() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/evo-v24/derive-kat.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn derive_matches_the_kat() {
    let rows = kat_rows();
    assert_eq!(rows.len(), 12);
    let seeds = ["00".repeat(32), "42".repeat(32), "0123456789abcdef".repeat(4)];
    let want: HashSet<(String, u64)> = seeds
        .iter()
        .flat_map(|seed| [0, 1, 7, u64::from(u32::MAX)].map(|index| (seed.clone(), index)))
        .collect();
    let got: HashSet<(String, u64)> = rows
        .iter()
        .map(|r| (r["seed"].as_str().unwrap().to_owned(), r["index"].as_u64().unwrap()))
        .collect();
    assert_eq!(got, want);
    for kat in &rows {
        let seed = unhex::<32>(kat["seed"].as_str().unwrap());
        let index = u32::try_from(kat["index"].as_u64().unwrap()).unwrap();
        let mut want = kat.clone();
        want.as_object_mut().unwrap().remove("seed");
        assert_eq!(row(index, &derive_basic_bls_key(&seed, index)), want, "{kat}");
    }
}

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_testkit-keys")).args(args).output().unwrap()
}

#[test]
fn cli_bls_derive_prints_the_library_keys_and_rejects_bad_args() {
    let seed_hex = "0123456789abcdef".repeat(4);
    let out = cli(&["bls-derive", "--seed", &seed_hex, "--index", "7", "--count", "3"]);
    assert!(out.status.success(), "{out:?}");
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let seed = unhex::<32>(&seed_hex);
    let want: Vec<Value> = (7..10).map(|i| row(i, &derive_basic_bls_key(&seed, i))).collect();
    assert_eq!(lines, want);

    for bad in [
        &["bls-derive", "--seed", "not-hex", "--index", "0"][..],
        &["bls-derive", "--seed", &"zz".repeat(32), "--index", "0"],
        &["bls-derive", "--seed", &seed_hex[..62], "--index", "0"],
        &["bls-derive", "--seed", &seed_hex],
        &["bls-derive", "--seed", &seed_hex, "--index", "-1"],
        &["bls-derive", "--seed", &seed_hex, "--index", "4294967295", "--count", "2"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", "0"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--bogus", "1"],
        &["bls-derive", "--seed", &seed_hex, "--index", "+7"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", "+7"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", " 7"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", "7\n"],
        &["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", ""],
        &["bls-derive", "--seed", &seed_hex, "--index", &seed_hex],
        &["bls-derive", &seed_hex, "--index", "0"],
        &["frobnicate"],
        &[],
    ] {
        let out = cli(bad);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(out.stdout.is_empty() && !out.stderr.is_empty(), "{bad:?}");
        assert!(!stderr(&out).contains(&seed_hex), "{bad:?}");
    }
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn cli_names_a_stray_argument() {
    let seed_hex = "0123456789abcdef".repeat(4);
    let out = cli(&["bls-derive", "--seed", &seed_hex, "--index", "0", "extra"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(stderr(&out).contains(r#"unknown argument "extra""#), "{out:?}");
}

#[cfg(unix)]
#[test]
fn cli_rejects_a_non_utf8_argument() {
    use std::os::unix::ffi::OsStrExt;

    let out = Command::new(env!("CARGO_BIN_EXE_testkit-keys"))
        .args(["bls-derive", "--seed"])
        .arg(std::ffi::OsStr::from_bytes(b"\xff"))
        .args(["--index", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(stderr(&out).contains("argument 3 is not valid UTF-8"), "{out:?}");
}

/// A reader that stops early (`| head -1`) ends the command quietly with exit 1, not a panic.
#[test]
fn cli_stops_quietly_when_stdout_closes() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let seed_hex = "0123456789abcdef".repeat(4);
    // About 400 kB of output, far more than a pipe buffers, so the writes outlast the reader.
    let mut child = Command::new(env!("CARGO_BIN_EXE_testkit-keys"))
        .args(["bls-derive", "--seed", &seed_hex, "--index", "0", "--count", "2000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut first).unwrap();
    assert!(first.starts_with(r#"{"index":0,"#), "{first}");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
}

/// A 0700 directory under the temp dir, removed on drop.
#[cfg(unix)]
struct PrivateDir(std::path::PathBuf);

#[cfg(unix)]
impl PrivateDir {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;

        let path = std::env::temp_dir().join(format!("testkit-keys-{name}-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    /// A new file `name` with exactly `mode`, holding `content`. It is created 0600 or stricter, and a
    /// looser test mode is set on the open file afterwards, because the umask strips bits from `mode`.
    fn file(&self, name: &str, mode: u32, content: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let path = self.0.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode & 0o700)
            .open(&path)
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }
}

#[cfg(unix)]
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn cli_seed_file_needs_an_owner_only_regular_file_of_hex() {
    let seed_hex = "0123456789abcdef".repeat(4);
    let derive = |seed_flag: &str, seed: &str| {
        cli(&["bls-derive", seed_flag, seed, "--index", "7", "--count", "2"])
    };
    let want = derive("--seed", &seed_hex);
    assert!(want.status.success(), "{want:?}");

    let dir = PrivateDir::new("seed-file");
    let path = |p: &std::path::Path| p.to_str().unwrap().to_owned();
    let ok = dir.file("ok", 0o600, &format!("{seed_hex} \n"));
    let owner_ro = dir.file("owner-ro", 0o400, &seed_hex);
    let group = dir.file("group", 0o640, &seed_hex);
    let other = dir.file("other", 0o604, &seed_hex);
    let link_ok = dir.0.join("link-ok");
    std::os::unix::fs::symlink(&ok, &link_ok).unwrap();
    let link_group = dir.0.join("link-group");
    std::os::unix::fs::symlink(&group, &link_group).unwrap();
    let link_dir = dir.0.join("link-dir");
    std::os::unix::fs::symlink(&dir.0, &link_dir).unwrap();

    for accepted in [&ok, &owner_ro, &link_ok] {
        let out = derive("--seed-file", &path(accepted));
        assert_eq!(out.status.code(), Some(0), "{accepted:?}: {out:?}");
        assert_eq!(out.stdout, want.stdout, "{accepted:?}");
    }
    for refused in [&group, &other, &link_group, &link_dir, &dir.0] {
        let out = derive("--seed-file", &path(refused));
        assert_eq!(out.status.code(), Some(2), "{refused:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{refused:?}");
        assert!(stderr(&out).contains("mode 0600 or stricter"), "{refused:?}: {out:?}");
        assert!(!stderr(&out).contains(&seed_hex), "{refused:?}");
    }

    let malformed = [
        format!("{seed_hex}00"),
        seed_hex[..62].to_owned(),
        format!("{}zz", &seed_hex[..62]),
        format!(" {seed_hex}"),
        format!("{seed_hex}\n{seed_hex}"),
        String::new(),
        format!("{seed_hex}{}junk", " ".repeat(4096 - 64)),
    ];
    for (i, content) in malformed.iter().enumerate() {
        let file = dir.file(&format!("bad-{i}"), 0o600, content);
        let out = derive("--seed-file", &path(&file));
        assert_eq!(out.status.code(), Some(2), "{content:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{content:?}");
        assert!(stderr(&out).contains("64 hex digits"), "{content:?}: {out:?}");
        assert!(!stderr(&out).contains(&seed_hex[..62]), "{content:?}");
    }

    let missing = derive("--seed-file", &path(&dir.0.join("missing")));
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");

    for both in [
        &["bls-derive", "--seed", &seed_hex, "--seed-file", &path(&ok), "--index", "0"][..],
        &["bls-derive", "--seed-file", &path(&ok), "--seed", &seed_hex, "--index", "0"],
    ] {
        let out = cli(both);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert!(stderr(&out).contains("conflict"), "{out:?}");
        assert!(!stderr(&out).contains(&seed_hex), "{out:?}");
    }
}

/// Core's `bls fromsecret <sk>` (basic scheme, `legacy` false) gives back `pk` for each derived
/// `sk`, and for every row of the KAT, so the recorder can hand both to Core as they are printed.
#[cfg(all(unix, feature = "harness"))]
#[tokio::test]
async fn dashd_bls_fromsecret_agrees_with_derived_keys() {
    use dash_testkit::{MixedNetwork, NodeId};
    use dashcore_rpc::RpcApi;

    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping dashd integration test (SKIP_DASHD_TESTS is set)");
        return;
    }
    let network = MixedNetwork::builder().dashd(1).start().await.unwrap();
    let rpc = network.rpc(NodeId::Dashd(0)).unwrap();
    let seed = [0x42; 32];
    for index in 0..16 {
        let key = derive_basic_bls_key(&seed, index);
        let sk = hex(key.sk.as_bytes());
        let basic: Value =
            rpc.call("bls", &["fromsecret".into(), sk.clone().into(), false.into()]).unwrap();
        assert_eq!(basic["secret"], sk, "index {index}");
        assert_eq!(basic["public"], hex(key.pk.as_bytes()), "index {index}");
        // `scheme` only echoes the `legacy` argument; what pins the scheme is that Core's legacy
        // encoding of the same key differs from `pk`.
        let legacy: Value =
            rpc.call("bls", &["fromsecret".into(), sk.clone().into(), true.into()]).unwrap();
        assert_eq!(legacy["scheme"], "legacy", "index {index}");
        assert_ne!(legacy["public"], basic["public"], "index {index}");
    }
    for kat in kat_rows() {
        let reply: Value =
            rpc.call("bls", &["fromsecret".into(), kat["sk"].clone(), false.into()]).unwrap();
        assert_eq!(reply["public"], kat["pk"], "{kat}");
    }
}

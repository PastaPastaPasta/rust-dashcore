//! `testkit-keys`: the fixtures' key derivations, for scripts that drive Dash Core.
//!
//! ```text
//! testkit-keys bls-derive (--seed <64-hex> | --seed-file <path>) --index <u32> [--count <n>]
//! ```
//!
//! `bls-derive` prints one JSON object per key, index ascending:
//! `{"index":7,"scheme":"basic","sk":"<64 hex>","pk":"<96 hex>"}`. The hex is in Core's RPC order
//! (`bls generate`, `bls fromsecret`): `sk` big-endian, `pk` its 48 serialized bytes.
//!
//! `--seed-file` reads the seed from a regular file (a symlink to one is followed) with no group
//! or other permission bits: 64 hex digits, optionally followed by whitespace.
//!
//! Bad arguments exit 2 with a message on stderr, before anything is printed; no message shows a
//! seed or a key. If stdout closes early the command stops quietly and exits 1.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::process::exit;

use dash_testkit::keys::derive_basic_bls_key;

const USAGE: &str = "usage: testkit-keys bls-derive (--seed <64-hex> | --seed-file <path>) \
                     --index <u32> [--count <n>]";

/// The longest argument a message quotes; a 64-hex seed is never shown.
const SHOWN_MAX: usize = 16;

fn fail(message: &str) -> ! {
    eprintln!("testkit-keys: {message}\n{USAGE}");
    exit(2);
}

/// `arg` quoted for an error message, or a placeholder if it is long enough to be a secret.
fn shown(arg: &str) -> String {
    if arg.chars().count() <= SHOWN_MAX {
        format!("{arg:?}")
    } else {
        format!("({} chars, not shown)", arg.chars().count())
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1).enumerate().map(|(i, arg)| utf8(i + 1, arg));
    match args.next().as_deref() {
        Some("bls-derive") => bls_derive(args),
        Some(other) => fail(&format!("unknown subcommand {}", shown(other))),
        None => fail("missing subcommand"),
    }
}

fn utf8(position: usize, arg: OsString) -> String {
    arg.into_string().unwrap_or_else(|_| fail(&format!("argument {position} is not valid UTF-8")))
}

fn bls_derive(mut args: impl Iterator<Item = String>) {
    let (mut seed, mut seed_file, mut index, mut count) = (None, None, None, None);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--seed" => &mut seed,
            "--seed-file" => &mut seed_file,
            "--index" => &mut index,
            "--count" => &mut count,
            _ => fail(&format!("unknown argument {}", shown(&arg))),
        };
        *slot = Some(args.next().unwrap_or_else(|| fail(&format!("{arg} needs a value"))));
    }
    let seed = match (seed, seed_file) {
        (Some(_), Some(_)) => fail("--seed and --seed-file conflict; give one"),
        (Some(hex), None) => parse_seed(&hex).unwrap_or_else(|| fail("--seed takes 64 hex digits")),
        (None, Some(path)) => read_seed_file(&path),
        (None, None) => fail("--seed or --seed-file is required"),
    };
    let index = parse_u32("--index", &index.unwrap_or_else(|| fail("--index is required")));
    let count = count.map_or(1, |count| parse_u32("--count", &count));
    if count == 0 || index.checked_add(count - 1).is_none() {
        fail("--count must be at least 1 and keep every index within u32");
    }
    let mut out = io::stdout().lock();
    for index in index..=index + (count - 1) {
        let key = derive_basic_bls_key(&seed, index);
        let sk: String = key.sk.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let line = format!(r#"{{"index":{index},"scheme":"basic","sk":"{sk}","pk":"{}"}}"#, key.pk);
        if let Err(err) = writeln!(out, "{line}").and_then(|()| out.flush()) {
            if err.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("testkit-keys: cannot write to stdout: {err}");
            }
            exit(1);
        }
    }
}

/// A decimal u32: ASCII digits only, so no sign or whitespace.
fn parse_u32(flag: &str, value: &str) -> u32 {
    Some(value)
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| fail(&format!("{flag} takes a decimal u32, not {}", shown(value))))
}

fn parse_seed(value: &str) -> Option<[u8; 32]> {
    let digits: Vec<u8> =
        value.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    (digits.len() == 64).then(|| std::array::from_fn(|i| digits[2 * i] << 4 | digits[2 * i + 1]))
}

const SEED_FILE_MAX: u64 = 4096;

const SEED_FILE_MODE: &str = "--seed-file must be a regular file with mode 0600 or stricter \
                              (no group or other permission bits)";

/// The seed in `path`, a regular file (symlinks followed) that only its owner can access.
fn read_seed_file(path: &str) -> [u8; 32] {
    let unreadable =
        |err: io::Error| -> ! { fail(&format!("cannot read --seed-file: {}", err.kind())) };
    check_owner_only(&std::fs::metadata(path).unwrap_or_else(|err| unreadable(err)));
    let file = File::open(path).unwrap_or_else(|err| unreadable(err));
    // Checked again on the open file, in case `path` changed in between.
    check_owner_only(&file.metadata().unwrap_or_else(|err| unreadable(err)));
    let mut content = Vec::new();
    // Far more than a seed and trailing whitespace. A longer file is refused, not read whole.
    file.take(SEED_FILE_MAX + 1).read_to_end(&mut content).unwrap_or_else(|err| unreadable(err));
    let malformed =
        || -> ! { fail("--seed-file must hold 64 hex digits, optionally followed by whitespace") };
    if content.len() as u64 > SEED_FILE_MAX {
        malformed();
    }
    std::str::from_utf8(&content)
        .ok()
        .and_then(|text| parse_seed(text.trim_end()))
        .unwrap_or_else(|| malformed())
}

#[cfg(unix)]
fn check_owner_only(meta: &Metadata) {
    use std::os::unix::fs::PermissionsExt;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        fail(SEED_FILE_MODE);
    }
}

#[cfg(not(unix))]
fn check_owner_only(_meta: &Metadata) {
    fail("--seed-file needs Unix file permissions");
}

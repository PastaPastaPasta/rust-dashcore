//! `testkit-keys`: the fixtures' key derivations, for scripts that drive Dash Core.
//!
//! ```text
//! testkit-keys bls-derive (--seed <64-hex> | --seed-file <path>) --index <u32> [--count <n>]
//! testkit-keys quorum-sk [--input-file <path>]
//! ```
//!
//! `bls-derive` prints one JSON object per key, index ascending:
//! `{"index":7,"scheme":"basic","sk":"<64 hex>","pk":"<96 hex>"}`. The hex is in Core's RPC order
//! (`bls generate`, `bls fromsecret`): `sk` big-endian, `pk` its 48 serialized bytes.
//!
//! `--seed-file` reads the seed from a regular file (a symlink to one is followed) with no group
//! or other permission bits: 64 hex digits, optionally followed by whitespace.
//!
//! `quorum-sk` recovers a quorum's secret key with [`recover_quorum_sk`]. It reads one request
//! from stdin (a pipe, or a file under `--seed-file`'s rules; never a terminal) or from
//! `--input-file`, which has `--seed-file`'s rules: `{"pk": "<96 hex>", "threshold": <n>,
//! "shares": [{"id": "<64 hex>", "sk": "<64 hex>"}, ...]}`, with each `id` a member's proTxHash
//! and every value in Core's RPC order (`quorum info <type> <hash> true`). It prints
//! `{"sk":"<64 hex>"}`, or exits 1 if the shares do not recover one key whose public key is `pk`.
//!
//! Bad arguments, files or JSON exit 2 with a message on stderr, before anything is printed; a
//! request that `recover_quorum_sk` refuses (a bad key or share, too few shares, a mismatch)
//! exits 1. No message shows a seed, a share or a key. If stdout closes early the command stops
//! quietly and exits 1.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::process::exit;

use dash_pkc::bls::{BlsPkBytes, BlsShareId, BlsSkBytes};
use dash_testkit::keys::{derive_basic_bls_key, recover_quorum_sk};

const USAGE: &str = "usage: testkit-keys bls-derive (--seed <64-hex> | --seed-file <path>) \
                     --index <u32> [--count <n>]\n       \
                     testkit-keys quorum-sk [--input-file <path>] < request.json";

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
        Some("quorum-sk") => quorum_sk(args),
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
        (Some(hex), None) => parse_hex(&hex).unwrap_or_else(|| fail("--seed takes 64 hex digits")),
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
        let sk = hex(key.sk.as_bytes());
        print_line(
            &mut out,
            &format!(r#"{{"index":{index},"scheme":"basic","sk":"{sk}","pk":"{}"}}"#, key.pk),
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `line`; a closed stdout ends the command with exit 1, quietly if the reader left.
fn print_line(out: &mut impl Write, line: &str) {
    if let Err(err) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        if err.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("testkit-keys: cannot write to stdout: {err}");
        }
        exit(1);
    }
}

/// Far more than a quorum's shares.
const INPUT_MAX: u64 = 1 << 20;

fn quorum_sk(mut args: impl Iterator<Item = String>) {
    let mut input_file = None;
    while let Some(arg) = args.next() {
        if arg != "--input-file" {
            fail(&format!("unknown argument {}", shown(&arg)));
        }
        input_file = Some(args.next().unwrap_or_else(|| fail("--input-file needs a value")));
    }
    let input = match input_file {
        Some(path) => read_owner_only_file("--input-file", &path, INPUT_MAX),
        None => read_stdin(),
    };
    if input.len() as u64 > INPUT_MAX {
        fail(&format!("the quorum-sk request is longer than {INPUT_MAX} bytes"));
    }
    let (pk, threshold, shares) = std::str::from_utf8(&input)
        .ok()
        .and_then(Json::parse)
        .ok_or_else(|| "the quorum-sk request is not JSON with only objects, arrays, plain strings and integers".to_owned())
        .and_then(request)
        .unwrap_or_else(|msg| fail(&msg));
    match recover_quorum_sk(&pk, &shares, threshold) {
        Ok(sk) => {
            print_line(&mut io::stdout().lock(), &format!(r#"{{"sk":"{}"}}"#, hex(sk.as_bytes())))
        }
        Err(err) => {
            eprintln!("testkit-keys: quorum-sk: {err}");
            exit(1);
        }
    }
}

fn read_stdin() -> Vec<u8> {
    use std::io::IsTerminal;
    let stdin = io::stdin();
    if stdin.is_terminal() {
        fail("quorum-sk reads its request from a pipe or --input-file, not a terminal");
    }
    check_stdin_file();
    let mut input = Vec::new();
    stdin
        .lock()
        .take(INPUT_MAX + 1)
        .read_to_end(&mut input)
        .unwrap_or_else(|err| fail(&format!("cannot read stdin: {}", err.kind())));
    input
}

/// Stdin redirected from a regular file gets `--input-file`'s mode rule; a pipe or socket passes.
#[cfg(unix)]
fn check_stdin_file() {
    use std::os::fd::AsFd;
    let meta = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .and_then(|file| file.metadata())
        .unwrap_or_else(|err| fail(&format!("cannot stat stdin: {}", err.kind())));
    if meta.is_file() {
        check_owner_only("stdin", &meta);
    }
}

#[cfg(not(unix))]
fn check_stdin_file() {}

/// A quorum-sk request: `pk`, `threshold` and the shares with their ids in curve order.
type Request = (BlsPkBytes, usize, Vec<(BlsShareId, BlsSkBytes)>);

fn request(json: Json) -> Result<Request, String> {
    let [pk, threshold, shares] = json.fields(["pk", "threshold", "shares"], "the request")?;
    let pk = BlsPkBytes::from_bytes(pk.hex("\"pk\"")?);
    let threshold = match threshold {
        Json::Num(n) => usize::try_from(n).ok(),
        _ => None,
    }
    .ok_or("\"threshold\" must be an unsigned integer")?;
    let Json::Arr(shares) = shares else {
        return Err("\"shares\" must be an array".to_owned());
    };
    let shares = shares
        .into_iter()
        .enumerate()
        .map(|(i, share)| {
            let what = format!("share {i}");
            let [id, sk] = share.fields(["id", "sk"], &what)?;
            // From the RPC hex, so stored as Core's CBLSId keeps it (see `recover_quorum_sk`).
            // `hex` first requires exactly 64 digits, which `from_hex` alone does not.
            let id = BlsShareId::from_hex(&hex(&id.hex::<32>(&format!("{what} \"id\""))?))
                .expect("64 hex digits parse");
            Ok((id, BlsSkBytes::from(sk.hex::<32>(&format!("{what} \"sk\""))?)))
        })
        .collect::<Result<_, String>>()?;
    Ok((pk, threshold, shares))
}

/// The JSON a quorum-sk request uses: objects, arrays, strings without escapes and unsigned
/// integers. Parse errors carry no position or text, so no share is ever quoted.
enum Json {
    Obj(Vec<(String, Json)>),
    Arr(Vec<Json>),
    Str(String),
    Num(u64),
}

impl Json {
    /// Nesting deeper than a request's is refused, which also bounds the recursion.
    const MAX_DEPTH: usize = 3;

    fn parse(text: &str) -> Option<Self> {
        let mut rest = text.as_bytes();
        let value = Self::value(&mut rest, 0)?;
        skip_ws(&mut rest);
        rest.is_empty().then_some(value)
    }

    fn value(rest: &mut &[u8], depth: usize) -> Option<Self> {
        skip_ws(rest);
        let (&first, tail) = rest.split_first()?;
        match first {
            b'{' if depth < Self::MAX_DEPTH => {
                *rest = tail;
                sequence(rest, b'}', |rest| {
                    skip_ws(rest);
                    let key = string(rest)?;
                    eat(rest, b':').then_some(())?;
                    Some((key, Self::value(rest, depth + 1)?))
                })
                .map(Self::Obj)
            }
            b'[' if depth < Self::MAX_DEPTH => {
                *rest = tail;
                sequence(rest, b']', |rest| Self::value(rest, depth + 1)).map(Self::Arr)
            }
            b'"' => string(rest).map(Self::Str),
            b'0'..=b'9' => {
                let end = rest.iter().position(|b| !b.is_ascii_digit()).unwrap_or(rest.len());
                let digits = std::str::from_utf8(&rest[..end]).ok()?;
                if digits.len() > 1 && digits.starts_with('0') {
                    return None;
                }
                *rest = &rest[end..];
                digits.parse().ok().map(Self::Num)
            }
            _ => None,
        }
    }

    /// The values of exactly the keys `names`, in that order.
    fn fields<const N: usize>(self, names: [&str; N], what: &str) -> Result<[Self; N], String> {
        let Self::Obj(fields) = self else {
            return Err(format!("{what} must be a JSON object"));
        };
        let mut found: [Option<Self>; N] = std::array::from_fn(|_| None);
        for (key, value) in fields {
            let i = names
                .iter()
                .position(|name| *name == key)
                .ok_or_else(|| format!("{what} has an unexpected key {}", shown(&key)))?;
            if found[i].replace(value).is_some() {
                return Err(format!("{what} repeats the key {key:?}"));
            }
        }
        if let Some(i) = found.iter().position(Option::is_none) {
            return Err(format!("{what} lacks the key {:?}", names[i]));
        }
        Ok(found.map(|value| value.expect("every key was found")))
    }

    /// A string of exactly `2 * N` hex digits, as bytes.
    fn hex<const N: usize>(self, what: &str) -> Result<[u8; N], String> {
        match self {
            Self::Str(text) => parse_hex(&text),
            _ => None,
        }
        .ok_or_else(|| format!("{what} must be a string of {} hex digits", 2 * N))
    }
}

/// Comma-separated `item`s up to `close`, after the opening bracket.
fn sequence<T>(
    rest: &mut &[u8],
    close: u8,
    mut item: impl FnMut(&mut &[u8]) -> Option<T>,
) -> Option<Vec<T>> {
    let mut items = Vec::new();
    if eat(rest, close) {
        return Some(items);
    }
    loop {
        items.push(item(rest)?);
        if eat(rest, close) {
            return Some(items);
        }
        if !eat(rest, b',') {
            return None;
        }
    }
}

/// A string without escapes or control characters, at the start of `rest`.
fn string(rest: &mut &[u8]) -> Option<String> {
    let (b'"', tail) = rest.split_first()? else {
        return None;
    };
    let end = tail.iter().position(|&b| b == b'"')?;
    let text = &tail[..end];
    if text.iter().any(|&b| b == b'\\' || b < 0x20) {
        return None;
    }
    *rest = &tail[end + 1..];
    std::str::from_utf8(text).ok().map(str::to_owned)
}

fn skip_ws(rest: &mut &[u8]) {
    while let [b' ' | b'\t' | b'\n' | b'\r', tail @ ..] = rest {
        *rest = tail;
    }
}

fn eat(rest: &mut &[u8], byte: u8) -> bool {
    skip_ws(rest);
    match rest.split_first() {
        Some((&first, tail)) if first == byte => {
            *rest = tail;
            true
        }
        _ => false,
    }
}

/// A decimal u32: ASCII digits only, so no sign or whitespace.
fn parse_u32(flag: &str, value: &str) -> u32 {
    Some(value)
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| fail(&format!("{flag} takes a decimal u32, not {}", shown(value))))
}

fn parse_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    let digits: Vec<u8> =
        value.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    (digits.len() == 2 * N).then(|| std::array::from_fn(|i| digits[2 * i] << 4 | digits[2 * i + 1]))
}

/// Far more than a seed and trailing whitespace. A longer file is refused, not read whole.
const SEED_FILE_MAX: u64 = 4096;

/// The seed in `path`, a regular file (symlinks followed) that only its owner can access.
fn read_seed_file(path: &str) -> [u8; 32] {
    let content = read_owner_only_file("--seed-file", path, SEED_FILE_MAX);
    let malformed =
        || -> ! { fail("--seed-file must hold 64 hex digits, optionally followed by whitespace") };
    if content.len() as u64 > SEED_FILE_MAX {
        malformed();
    }
    std::str::from_utf8(&content)
        .ok()
        .and_then(|text| parse_hex(text.trim_end()))
        .unwrap_or_else(|| malformed())
}

/// At most `max + 1` bytes of `path`, a regular file (symlinks followed) that only its owner can
/// access; `flag` names it in errors.
fn read_owner_only_file(flag: &str, path: &str, max: u64) -> Vec<u8> {
    let unreadable = |err: io::Error| -> ! { fail(&format!("cannot read {flag}: {}", err.kind())) };
    check_owner_only(flag, &std::fs::metadata(path).unwrap_or_else(|err| unreadable(err)));
    let file = File::open(path).unwrap_or_else(|err| unreadable(err));
    // Checked again on the open file, in case `path` changed in between.
    check_owner_only(flag, &file.metadata().unwrap_or_else(|err| unreadable(err)));
    let mut content = Vec::new();
    file.take(max + 1).read_to_end(&mut content).unwrap_or_else(|err| unreadable(err));
    content
}

#[cfg(unix)]
fn check_owner_only(flag: &str, meta: &Metadata) {
    use std::os::unix::fs::PermissionsExt;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        fail(&format!(
            "{flag} must be a regular file with mode 0600 or stricter (no group or other \
             permission bits)"
        ));
    }
}

#[cfg(not(unix))]
fn check_owner_only(flag: &str, _meta: &Metadata) {
    fail(&format!("{flag} needs Unix file permissions"));
}

use dash_p2p::{CommandString, read};
use dash_p2p_core::{MnListFormat, P2pMsg, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use dash_types::codec::BaseCodec;

pub fn command(name: &str) -> CommandString {
    let mut bytes = [0; 12];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    CommandString::from_bytes(bytes)
}

pub fn ctx(version: i32, bls: BlsCtx) -> read::PeerCtx {
    read::PeerCtx::new(ProtocolVersion::new(version), bls)
}

pub fn fail(e: read::ReadFail) -> String {
    match &e {
        read::ReadFail::VersionTail {
            head,
            ..
        } => format!("{e} head={head:?}"),
        _ => e.to_string(),
    }
}

pub fn new_debug(msg: read::Msg) -> String {
    match msg {
        read::Msg::Known(msg) => format!("{msg:?}"),
        read::Msg::Unknown {
            command,
            payload,
        } => {
            format!(
                "{:?}",
                P2pMsg::Unknown {
                    command,
                    payload
                }
            )
        }
    }
}

pub fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub const SHADOW_VERSIONS: [i32; 10] =
    [70221, 70225, 70228, 70229, 70230, 70234, 70235, 70240, 70241, 70242];

const INLINE_PAYLOAD_MAX: usize = 4096;

/// Keys that hold a payload in the corpora.
const PAYLOAD_FIELDS: [&str; 3] = ["raw", "payload", "frame"];

/// Top-level sections whose entries carry no `command` key but are all one message type.
const SECTION_COMMANDS: [(&str, &str, &str); 5] = [
    ("core_headers2.json5", "capture", "headers2"),
    ("core_headers2.json5", "networks", "headers2"),
    ("headers2.json5", "uncompress", "headers2"),
    ("headers2.json5", "reply", "headers2"),
    ("headers2.json5", "announce", "headers2"),
];

/// Payload entries left out of the differential: file, key path (indices as `[]`), the entry's
/// `command` value (`*` for any), how many, and why.
pub const CORPUS_SKIPS: [(&str, &str, &str, usize, &str); 6] = [
    (
        "core_captures.json5",
        "messages[].raw",
        "reject",
        2,
        "no reject message type in Core, so an unknown command",
    ),
    ("core_overlay.json5", "cases[].raw", "smlentry", 50, "a masternode list entry, not a message"),
    ("core_overlay.json5", "cases[].raw", "qsnapshot", 7, "a quorum snapshot, not a message"),
    ("core_verdicts.json5", "framing[].frame", "*", 6, "a whole V1 frame, for the transport tests"),
    (
        "decode_domain.json5",
        "v1_commands[].raw",
        "*",
        6,
        "invalid V1 command bytes, for the transport tests",
    ),
    (
        "decode_domain.json5",
        "ignored_payload[].raw",
        "bs4aprobe",
        1,
        "an unknown command, covered by read_class_d",
    ),
];

pub struct CorpusCase {
    pub source: &'static str,
    pub name: &'static str,
    pub key: &'static str,
    pub bytes: Vec<u8>,
}

pub fn corpus_cases() -> Vec<CorpusCase> {
    include_str!("../../../test-vectors/p2p-decode/corpus_cases.tsv")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let [source, name, key] = line.split('|').collect::<Vec<_>>().try_into().unwrap();
            CorpusCase {
                source,
                name,
                key,
                bytes: payload_bytes(key),
            }
        })
        .collect()
}

pub fn payload_key(bytes: &[u8]) -> String {
    if bytes.len() <= INLINE_PAYLOAD_MAX {
        hex_bytes(bytes)
    } else {
        format!("sha256:{}:{}", hex_bytes(&dash_sha256::sha256(bytes)), bytes.len())
    }
}

/// The bytes behind a manifest or golden payload column.
pub fn payload_bytes(key: &str) -> Vec<u8> {
    use std::collections::HashMap;
    static LONG: std::sync::OnceLock<HashMap<String, Vec<u8>>> = std::sync::OnceLock::new();
    if !key.starts_with("sha256:") {
        return unhex(key);
    }
    let long = LONG.get_or_init(|| {
        corpus_files()
            .iter()
            .flat_map(|file| file.rows.iter().map(|(_, bytes)| bytes.clone()))
            .chain(old_layout_rows().into_iter().map(|(_, _, bytes)| bytes))
            .filter(|bytes| bytes.len() > INLINE_PAYLOAD_MAX)
            .map(|bytes| (payload_key(&bytes), bytes))
            .collect()
    });
    long.get(key).unwrap_or_else(|| panic!("no corpus payload for {key}")).clone()
}

fn unhex(payload: &str) -> Vec<u8> {
    (0..payload.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&payload[i..i + 2], 16).unwrap())
        .collect()
}

enum Json5 {
    Object(Vec<(String, Json5)>),
    Array(Vec<Json5>),
    String(String),
    Integer(i128),
    Other,
}

struct Json5Parser<'a> {
    text: &'a [u8],
    pos: usize,
}

impl Json5Parser<'_> {
    fn skip_blank(&mut self) {
        loop {
            match self.text.get(self.pos..) {
                Some([b'/', b'/', ..]) => {
                    while self.pos < self.text.len() && self.text[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                Some([b'/', b'*', ..]) => {
                    let end = self.text[self.pos + 2..]
                        .windows(2)
                        .position(|w| w == b"*/")
                        .expect("unterminated comment");
                    self.pos += end + 4;
                }
                Some([b, ..]) if b.is_ascii_whitespace() => self.pos += 1,
                _ => return,
            }
        }
    }

    fn peek(&mut self) -> u8 {
        self.skip_blank();
        *self.text.get(self.pos).expect("unexpected end of json5")
    }

    fn word(&mut self) -> &str {
        let start = self.pos;
        while self.text.get(self.pos).is_some_and(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'+' | b'-' | b'.')
        }) {
            self.pos += 1;
        }
        assert!(self.pos > start, "unexpected byte at {start}");
        std::str::from_utf8(&self.text[start..self.pos]).unwrap()
    }

    fn string(&mut self) -> String {
        let quote = self.text[self.pos];
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let b = self.text[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    let e = self.text[self.pos];
                    self.pos += 1;
                    let mut hex = |n: usize| {
                        let v = u32::from_str_radix(
                            std::str::from_utf8(&self.text[self.pos..self.pos + n]).unwrap(),
                            16,
                        )
                        .unwrap();
                        self.pos += n;
                        v
                    };
                    let c = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'v' => '\u{b}',
                        b'0' => '\0',
                        b'u' => char::from_u32(hex(4)).unwrap_or('\u{fffd}'),
                        b'x' => char::from_u32(hex(2)).unwrap(),
                        b'\n' => continue,
                        other => other as char,
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                b if b == quote => return String::from_utf8(out).unwrap(),
                b => out.push(b),
            }
        }
    }

    fn value(&mut self) -> Json5 {
        match self.peek() {
            b'{' => {
                self.pos += 1;
                let mut fields = Vec::new();
                while self.peek() != b'}' {
                    let key = match self.peek() {
                        b'"' | b'\'' => self.string(),
                        _ => self.word().to_owned(),
                    };
                    assert_eq!(self.peek(), b':', "expected ':' at {}", self.pos);
                    self.pos += 1;
                    fields.push((key, self.value()));
                    if self.peek() == b',' {
                        self.pos += 1;
                    }
                }
                self.pos += 1;
                Json5::Object(fields)
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != b']' {
                    items.push(self.value());
                    if self.peek() == b',' {
                        self.pos += 1;
                    }
                }
                self.pos += 1;
                Json5::Array(items)
            }
            b'"' | b'\'' => Json5::String(self.string()),
            _ => {
                let word = self.word();
                let (negative, digits) = match word.as_bytes()[0] {
                    b'-' => (true, &word[1..]),
                    b'+' => (false, &word[1..]),
                    _ => (false, word),
                };
                let value = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
                    Some(hex) => i128::from_str_radix(hex, 16).ok(),
                    None if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                        digits.parse().ok()
                    }
                    None => None,
                };
                value.map_or(Json5::Other, |v| {
                    Json5::Integer(if negative {
                        -v
                    } else {
                        v
                    })
                })
            }
        }
    }

    fn values(text: &str) -> Vec<Json5> {
        let mut parser = Json5Parser {
            text: text.as_bytes(),
            pos: 0,
        };
        let mut values = Vec::new();
        loop {
            parser.skip_blank();
            if parser.pos == parser.text.len() {
                return values;
            }
            values.push(parser.value());
        }
    }
}

fn corpus_command(value: Option<&Json5>) -> Option<&'static str> {
    let Some(Json5::String(name)) = value else {
        return None;
    };
    dash_p2p::policy::ALL_NET_MESSAGE_TYPES
        .iter()
        .map(|cmd| cmd.as_str())
        .find(|known| known == name)
}

fn hex_string(hex: &str) -> Option<Vec<u8>> {
    (hex.len().is_multiple_of(2) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| unhex(hex))
}

/// A payload as the corpora write it: one hex string, or (over 4 KiB) a list of hex strings and
/// `{"b": "<hex byte>", "n": <count>}` runs. Integer bytes and integer `b` are accepted too.
fn corpus_payload(value: &Json5) -> Option<Vec<u8>> {
    let byte = |value: &Json5| match value {
        Json5::Integer(b @ 0..=255) => Some(*b as u8),
        Json5::String(hex) if hex.len() == 2 => hex_string(hex).map(|b| b[0]),
        _ => None,
    };
    match value {
        Json5::String(hex) => hex_string(hex),
        Json5::Array(items) => {
            let mut bytes = Vec::new();
            for item in items {
                match item {
                    Json5::String(hex) => bytes.extend(hex_string(hex)?),
                    Json5::Integer(_) => bytes.push(byte(item)?),
                    Json5::Object(fields)
                        if fields.len() == 2
                            && fields.iter().any(|(k, _)| k == "b")
                            && fields.iter().any(|(k, _)| k == "n") =>
                    {
                        let field = |name: &str| {
                            fields.iter().find(|(k, _)| k == name).map(|(_, v)| v).unwrap()
                        };
                        let Json5::Integer(n) = field("n") else {
                            return None;
                        };
                        let b = byte(field("b"))?;
                        bytes.extend(std::iter::repeat_n(b, usize::try_from(*n).ok()?));
                    }
                    _ => return None,
                }
            }
            Some(bytes)
        }
        _ => None,
    }
}

pub struct CorpusFile {
    pub name: String,
    pub rows: Vec<(&'static str, Vec<u8>)>,
    /// Index into `CORPUS_SKIPS` of each entry left out.
    pub skips: Vec<usize>,
    /// Payload fields in the parsed file, counted without decoding them.
    pub payload_fields: usize,
}

fn count_payload_fields(value: &Json5) -> usize {
    match value {
        Json5::Object(fields) => fields
            .iter()
            .map(|(key, child)| {
                usize::from(PAYLOAD_FIELDS.contains(&key.as_str())) + count_payload_fields(child)
            })
            .sum(),
        Json5::Array(items) => items.iter().map(count_payload_fields).sum(),
        _ => 0,
    }
}

/// Collects every payload entry under `value` as a row or a listed skip, and panics on any other.
fn collect_corpus_rows(
    file: &mut CorpusFile,
    value: &Json5,
    path: &str,
    inherited: Option<&'static str>,
) {
    match value {
        Json5::Object(fields) => {
            let field = |name: &str| fields.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v);
            let declared = match field("command").or(field("cmd")) {
                Some(Json5::String(name)) => Some(name.as_str()),
                _ => None,
            };
            let cmd = match declared {
                Some(name) => corpus_command(Some(&Json5::String(name.to_owned()))),
                None => corpus_command(field("type")).or(inherited),
            };
            for key in PAYLOAD_FIELDS {
                let Some(payload) = field(key) else {
                    continue;
                };
                let entry = format!("{path}.{key}");
                let pattern = entry.trim_start_matches('.').to_owned();
                let mut pattern_chars = pattern.chars().peekable();
                let mut normalized = String::new();
                while let Some(c) = pattern_chars.next() {
                    normalized.push(c);
                    if c == '[' {
                        while pattern_chars.next_if(char::is_ascii_digit).is_some() {}
                    }
                }
                if let Some(skip) = CORPUS_SKIPS.iter().position(|(f, p, c, _, _)| {
                    *f == file.name && *p == normalized && (*c == "*" || Some(*c) == declared)
                }) {
                    file.skips.push(skip);
                    continue;
                }
                let name = &file.name;
                let cmd = cmd.unwrap_or_else(|| {
                    panic!("{name}: {entry}: no message type for {declared:?}; name it or list it in CORPUS_SKIPS")
                });
                let bytes = corpus_payload(payload)
                    .unwrap_or_else(|| panic!("{name}: {entry}: payload is not in a corpus form"));
                file.rows.push((cmd, bytes));
            }
            for (key, child) in fields {
                if PAYLOAD_FIELDS.contains(&key.as_str()) {
                    continue;
                }
                let section = SECTION_COMMANDS
                    .iter()
                    .find(|(f, k, _)| path.is_empty() && *f == file.name && k == key)
                    .map(|(_, _, cmd)| *cmd);
                let named = corpus_command(Some(&Json5::String(key.clone()))).or(section);
                collect_corpus_rows(file, child, &format!("{path}.{key}"), named.or(cmd));
            }
        }
        Json5::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_corpus_rows(file, item, &format!("{path}[{index}]"), inherited);
            }
        }
        _ => {}
    }
}

pub fn corpus_files() -> &'static [CorpusFile] {
    static FILES: std::sync::OnceLock<Vec<CorpusFile>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/p2p-decode");
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".json5"))
            .collect::<Vec<_>>();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let text = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
                let values = Json5Parser::values(&text);
                let mut file = CorpusFile {
                    name,
                    rows: Vec::new(),
                    skips: Vec::new(),
                    payload_fields: values.iter().map(count_payload_fields).sum(),
                };
                let single = values.len() == 1;
                for (index, value) in values.iter().enumerate() {
                    let path = if single {
                        String::new()
                    } else {
                        format!("[{index}]")
                    };
                    collect_corpus_rows(&mut file, value, &path, None);
                }
                file
            })
            .collect()
    })
}

pub const MN_LIST_LAYOUTS: [(MnListFormat, &[i32]); 4] = [
    (MnListFormat::Unversioned, &[70221, 70224]),
    (MnListFormat::VersionAfterCbTx, &[70225, 70227]),
    (MnListFormat::VersionedEntries, &[70228]),
    (MnListFormat::VersionFirst, &[70229]),
];

fn compact(buf: &mut Vec<u8>, count: u64) {
    dash_types::CompactSize::from(count).encode(buf);
}

fn mn_list_entry(buf: &mut Vec<u8>, with_version: bool, version: u16, evo: bool, seed: u8) {
    if with_version {
        buf.extend_from_slice(&version.to_le_bytes());
    }
    buf.extend_from_slice(&[seed; 32]);
    buf.extend_from_slice(&[seed.wrapping_add(1); 32]);
    buf.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, seed, 0x27, 0x0f]);
    buf.extend_from_slice(&[seed.wrapping_add(2); 48]);
    buf.extend_from_slice(&[seed.wrapping_add(3); 20]);
    buf.push(1);
    if version >= 2 {
        buf.extend_from_slice(&u16::from(evo).to_le_bytes());
        if evo {
            if version < 3 {
                buf.extend_from_slice(&443u16.to_le_bytes());
            }
            buf.extend_from_slice(&[seed.wrapping_add(4); 20]);
        }
    }
}

/// One `mnlistdiff` in `format`, written field by field in the fork's layout.
fn mn_list_diff_in(buf: &mut Vec<u8>, format: MnListFormat, seed: u8) {
    let diff_version = 2u16;
    if format.has_version_first() {
        buf.extend_from_slice(&diff_version.to_le_bytes());
    }
    buf.extend_from_slice(&[seed; 32]);
    buf.extend_from_slice(&[seed.wrapping_add(1); 32]);
    buf.extend_from_slice(&1u32.to_le_bytes());
    compact(buf, 1);
    buf.extend_from_slice(&[seed.wrapping_add(2); 32]);
    compact(buf, 1);
    buf.push(1);
    buf.extend_from_slice(&1i32.to_le_bytes());
    compact(buf, 0);
    compact(buf, 0);
    buf.extend_from_slice(&0u32.to_le_bytes());
    if format.has_version_after_cb_tx() {
        buf.extend_from_slice(&diff_version.to_le_bytes());
    }
    compact(buf, 1);
    buf.extend_from_slice(&[seed.wrapping_add(3); 32]);
    let with_version = format.has_entry_version();
    if with_version {
        compact(buf, 2);
        mn_list_entry(buf, true, 1, false, seed.wrapping_add(10));
        mn_list_entry(buf, true, 2, true, seed.wrapping_add(20));
    } else {
        compact(buf, 1);
        mn_list_entry(buf, false, 1, false, seed.wrapping_add(10));
    }
    compact(buf, 0);
    compact(buf, 0);
}

fn quorum_snapshot(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&1i32.to_le_bytes());
    compact(buf, 3);
    buf.push(0b101);
    compact(buf, 1);
    buf.extend_from_slice(&2i32.to_le_bytes());
}

fn old_layout_mnlistdiff(format: MnListFormat) -> Vec<u8> {
    let mut payload = Vec::new();
    mn_list_diff_in(&mut payload, format, 0x30);
    payload
}

/// One `qrinfo` whose diffs are in `format`, with the H-4C cycle and one base entry.
fn old_layout_qrinfo(format: MnListFormat) -> Vec<u8> {
    let mut payload = Vec::new();
    for _ in 0..3 {
        quorum_snapshot(&mut payload);
    }
    for seed in [0x40, 0x50, 0x60, 0x70, 0x80] {
        mn_list_diff_in(&mut payload, format, seed);
    }
    payload.push(1);
    quorum_snapshot(&mut payload);
    mn_list_diff_in(&mut payload, format, 0x90);
    compact(&mut payload, 0);
    compact(&mut payload, 1);
    quorum_snapshot(&mut payload);
    compact(&mut payload, 1);
    mn_list_diff_in(&mut payload, format, 0xa0);
    payload
}

pub fn old_layout_rows() -> Vec<(&'static str, MnListFormat, Vec<u8>)> {
    MN_LIST_LAYOUTS
        .iter()
        .flat_map(|&(format, _)| {
            [
                ("mnlistdiff", format, old_layout_mnlistdiff(format)),
                ("qrinfo", format, old_layout_qrinfo(format)),
            ]
        })
        .collect()
}

//! The crash information of src/stacktraces.cpp at v24.0.2: the report `PrintExceptionContinue`
//! prints, and `-printcrashinfo`'s decoding of it (`GetCrashInfoStrFromSerializedStr`), with
//! `EncodeBase32` / `DecodeBase32` of src/util/strencodings.cpp.
//!
//! The release dash-cli has no debug information, so its libbacktrace resolves every stack frame
//! to an entry with no file, line or function; that is what `stack_frame_infos` gives. Core adds
//! the executable's load address to each address it decodes and subtracts it again before it
//! re-encodes them; here that address is 0.

/// Core: `g_exeFileBaseName` of the stock binary whose output this reproduces.
const EXE_FILE_BASE_NAME: &str = "dash-cli";
const MAGIC: &[u8] = b"DashCrashInfo";
/// Core: `MAX_SIZE` (src/serialize.h), the bound of `ReadCompactSize`.
const MAX_SIZE: u64 = 0x0200_0000;
/// Core: `my_backtrace_full_callback` stops after this many frame infos.
const MAX_FRAME_INFOS: usize = 128;
const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Core: `stackframe_info`.
struct StackFrameInfo {
    pc: u64,
    filename: Vec<u8>,
    lineno: i32,
    function: Vec<u8>,
}

/// Core: `crash_info`.
pub(crate) struct CrashInfo {
    description: Vec<u8>,
    stackframes: Vec<u64>,
    infos: Vec<StackFrameInfo>,
}

impl CrashInfo {
    /// A report with no stack frames, as a release binary makes for an exception.
    pub(crate) fn new(description: &str) -> Self {
        CrashInfo {
            description: description.as_bytes().to_vec(),
            stackframes: Vec::new(),
            infos: Vec::new(),
        }
    }
}

/// Core: `EncodeBase32` (lower case, padded).
fn encode_base32(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = buf.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        // 5 input bytes make 8 symbols; a short chunk makes ceil(8 * len / 5) of them.
        let symbols = (chunk.len() * 8).div_ceil(5);
        for i in 0..8 {
            out.push(if i < symbols {
                char::from(BASE32_ALPHABET[((n >> (35 - 5 * i)) & 31) as usize])
            } else {
                '='
            });
        }
    }
    out
}

/// Core: `DecodeBase32`: either case, a length that is a multiple of 8 with 1, 3, 4 or 6
/// padding characters, and no set bits left over.
fn decode_base32(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(8) {
        return None;
    }
    let mut s = s.as_bytes();
    for strip in [1, 2, 1, 2] {
        if s.len() >= strip && s[s.len() - strip..].iter().all(|&c| c == b'=') {
            s = &s[..s.len() - strip];
        }
    }
    // ConvertBits<5, 8, false>
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::with_capacity(s.len() * 5 / 8));
    for &c in s {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = ((acc << 5) | u32::from(v)) & 0xfff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 5 || (acc << (8 - bits)) & 0xff != 0 {
        return None;
    }
    Some(out)
}

/// Core: `WriteCompactSize`.
fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=252 => out.push(n as u8),
        253..=0xffff => {
            out.push(253);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(254);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(255);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// Core: `Serialize` of a `std::string`: a compact-size length, then the bytes.
fn write_bytes(out: &mut Vec<u8>, s: &[u8]) {
    write_compact_size(out, s.len() as u64);
    out.extend_from_slice(s);
}

/// The serialized `crash_info_header` and `crash_info` (`SER_DISK`).
fn serialize(ci: &CrashInfo) -> Vec<u8> {
    let mut out = Vec::new();
    write_bytes(&mut out, MAGIC);
    out.extend_from_slice(&1u16.to_le_bytes());
    write_bytes(&mut out, EXE_FILE_BASE_NAME.as_bytes());
    write_bytes(&mut out, &ci.description);
    write_compact_size(&mut out, ci.stackframes.len() as u64);
    for pc in &ci.stackframes {
        out.extend_from_slice(&pc.to_le_bytes());
    }
    write_compact_size(&mut out, ci.infos.len() as u64);
    for si in &ci.infos {
        out.extend_from_slice(&si.pc.to_le_bytes());
        write_bytes(&mut out, &si.filename);
        out.extend_from_slice(&si.lineno.to_le_bytes());
        write_bytes(&mut out, &si.function);
    }
    out
}

/// A `CDataStream` being read; `None` is Core's `std::ios_base::failure`.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }

    /// Core: `ReadCompactSize` with its range check: canonical encodings up to `MAX_SIZE`.
    fn compact_size(&mut self) -> Option<u64> {
        let (n, min) = match self.take::<1>()?[0] {
            253 => (u64::from(u16::from_le_bytes(self.take()?)), 253),
            254 => (u64::from(u32::from_le_bytes(self.take()?)), 0x1_0000),
            255 => (self.u64()?, 0x1_0000_0000),
            first => (u64::from(first), 0),
        };
        (min..=MAX_SIZE).contains(&n).then_some(n)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = usize::try_from(self.compact_size()?).ok()?;
        if self.0.len() < len {
            return None;
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(head.to_vec())
    }

    fn header(&mut self) -> Option<(Vec<u8>, u16, Vec<u8>)> {
        Some((self.bytes()?, u16::from_le_bytes(self.take()?), self.bytes()?))
    }

    fn crash_info(&mut self) -> Option<CrashInfo> {
        let description = self.bytes()?;
        let stackframes = (0..self.compact_size()?).map(|_| self.u64()).collect::<Option<_>>()?;
        let infos = (0..self.compact_size()?)
            .map(|_| {
                Some(StackFrameInfo {
                    pc: self.u64()?,
                    filename: self.bytes()?,
                    lineno: i32::from_le_bytes(self.take()?),
                    function: self.bytes()?,
                })
            })
            .collect::<Option<_>>()?;
        Some(CrashInfo {
            description,
            stackframes,
            infos,
        })
    }
}

/// Core: `GetStackFrameInfos` in a binary without debug information: one entry per frame with
/// no file, line or function, at most `MAX_FRAME_INFOS`.
fn stack_frame_infos(stackframes: &[u64]) -> Vec<StackFrameInfo> {
    stackframes
        .iter()
        .take(MAX_FRAME_INFOS)
        .map(|&pc| StackFrameInfo {
            pc,
            filename: Vec::new(),
            lineno: 0,
            function: Vec::new(),
        })
        .collect()
}

/// Core: `GetCrashInfoStrFromSerializedStr`: what `-printcrashinfo=<ci_str>` prints.
pub(crate) fn crash_info_str_from_serialized(ci_str: &str) -> Vec<u8> {
    let Some(buf) = decode_base32(ci_str).filter(|b| !b.is_empty()) else {
        return b"Error while deserializing crash info".to_vec();
    };
    let mut ds = Reader(&buf);
    let Some((magic, version, exe_file_name)) = ds.header() else {
        return b"Error while deserializing crash info header".to_vec();
    };
    if magic != MAGIC {
        return b"Invalid magic string".to_vec();
    }
    if version != 1 {
        return b"Unsupported version".to_vec();
    }
    if exe_file_name != EXE_FILE_BASE_NAME.as_bytes() {
        return b"Crash info is not for this executable".to_vec();
    }
    let Some(mut ci) = ds.crash_info() else {
        return b"Error while deserializing crash info".to_vec();
    };
    if ci.infos.is_empty() {
        ci.infos = stack_frame_infos(&ci.stackframes);
    }
    crash_info_str(&ci)
}

/// Core: `GetCrashInfoStr` with two spaces of indentation, and `GetCrashInfoStrNoDebugInfo` when
/// no frame names a file or a function.
pub(crate) fn crash_info_str(ci: &CrashInfo) -> Vec<u8> {
    let mut s = ci.description.clone();
    s.push(b'\n');
    if !ci.infos.iter().any(|si| !si.filename.is_empty() || !si.function.is_empty()) {
        s.extend_from_slice(
            format!(
                "No debug information available for stacktrace. You should add debug information and then run:\n\
                 {EXE_FILE_BASE_NAME} -printcrashinfo={}\n",
                encode_base32(&serialize(ci))
            )
            .as_bytes(),
        );
        return s;
    }
    let locations: Vec<Vec<u8>> = ci
        .infos
        .iter()
        .map(|si| {
            let mut l = if si.filename.is_empty() {
                b"<unknown-file>".to_vec()
            } else {
                file_name(&si.filename).to_vec()
            };
            if si.lineno != 0 {
                l.extend_from_slice(format!(":{}", si.lineno).as_bytes());
            }
            l
        })
        .collect();
    let width = locations.iter().map(Vec::len).max().unwrap_or(0);
    for (i, (si, l)) in ci.infos.iter().zip(&locations).enumerate() {
        // "  %2d#: (0x%08X) %-<width>s - %s\n"
        s.extend_from_slice(format!("  {i:2}#: (0x{:08X}) ", si.pc).as_bytes());
        s.extend_from_slice(l);
        s.resize(s.len() + (width - l.len()), b' ');
        s.extend_from_slice(b" - ");
        s.extend_from_slice(if si.function.is_empty() {
            b"???"
        } else {
            &si.function
        });
        s.push(b'\n');
    }
    s
}

/// `fs::path::filename()` of a path as bytes: what follows the last separator.
fn file_name(path: &[u8]) -> &[u8] {
    let sep = |c: &u8| *c == b'/' || (cfg!(windows) && *c == b'\\');
    path.rsplit(sep).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pc: u64, filename: &str, lineno: i32, function: &str) -> StackFrameInfo {
        StackFrameInfo {
            pc,
            filename: filename.into(),
            lineno,
            function: function.into(),
        }
    }

    #[test]
    fn base32_round_trips_and_rejects_what_core_rejects() {
        for len in 0..12 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let enc = encode_base32(&data);
            assert_eq!(decode_base32(&enc).as_deref(), Some(&data[..]));
            assert_eq!(decode_base32(&enc.to_uppercase()).as_deref(), Some(&data[..]));
        }
        for bad in ["x", "a=======", "aa======a", "ab======", "1aaaaaaa", "aaaaaaa ="] {
            assert_eq!(decode_base32(bad), None, "{bad}");
        }
    }

    #[test]
    fn compact_sizes_must_be_canonical_and_bounded() {
        let read = |b: &[u8]| Reader(b).compact_size();
        assert_eq!(read(&[252]), Some(252));
        assert_eq!(read(&[253, 253, 0]), Some(253));
        assert_eq!(read(&[253, 252, 0]), None);
        assert_eq!(read(&[254, 0xff, 0xff, 0, 0]), None);
        assert_eq!(read(&[254, 0, 0, 1, 0]), Some(0x1_0000));
        assert_eq!(read(&[254, 0, 0, 0, 2]), Some(MAX_SIZE));
        assert_eq!(read(&[254, 1, 0, 0, 2]), None);
        assert_eq!(read(&[255, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]), None);
        assert_eq!(read(&[253, 0]), None);
    }

    #[test]
    fn frames_with_names_print_one_line_each() {
        let ci = CrashInfo {
            description: b"desc".to_vec(),
            stackframes: vec![],
            infos: vec![frame(0x10, "/a/b/foo.cpp", 42, "fn1"), frame(0x2000, "", 0, "")],
        };
        assert_eq!(
            String::from_utf8(crash_info_str(&ci)).unwrap(),
            "desc\n   0#: (0x00000010) foo.cpp:42     - fn1\n   1#: (0x00002000) <unknown-file> - ???\n"
        );
    }

    #[test]
    fn a_report_decodes_to_itself() {
        let report = crash_info_str(&CrashInfo::new("Exception: boom"));
        let text = String::from_utf8(report.clone()).unwrap();
        let encoded = text.rsplit_once("-printcrashinfo=").unwrap().1.trim_end();
        assert_eq!(crash_info_str_from_serialized(encoded), report);
    }
}

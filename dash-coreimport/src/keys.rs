//! Core's onion and I2P private key files (IM1b; FR-IMP-04, FR-P2P-19, US-IMP-07).
//!
//! | file | written by | content |
//! |---|---|---|
//! | `onion_v3_private_key` | `torcontrol.cpp` (`WriteBinaryFile`) | the text `ED25519-V3:` and the standard base64 of Tor's 64-byte expanded secret key, no newline |
//! | `i2p_private_key` | `i2p.cpp` (`WriteBinaryFile`) | the I2P private destination, binary: the destination (387 bytes plus the certificate length at offset 385) followed by the private keys |
//!
//! # Secrets
//!
//! Both are long-lived private keys that decide the node's onion and I2P addresses. The types
//! here ([`OnionV3Key`], [`I2pPrivateKey`]) never print their content (`Debug` is redacted), hold
//! it in a buffer that is overwritten on drop, and are written only by [`write_secret_file`],
//! which creates the file with mode `0600` (exclusively, never following a link, and never
//! replacing an existing file). A write that installs the key but cannot remove its temporary
//! copy or sync the directory says so ([`KeyWriteError::Installed`]). Nothing in this module logs
//! a key or puts one in an error. Moving a key into another type is the caller's responsibility;
//! the reader's own buffers (the file read into one fixed buffer, the decoded base64) are cleared
//! before they are dropped.
//!
//! # Core's rules, and the one place this reader is stricter
//!
//! - **`onion_v3_private_key`**: Core reads any bytes, and uses the string as it is: an empty file
//!   means "no key" and Core asks Tor for a new one (`NEW:ED25519-V3`); anything else is pasted
//!   into the control-port command `ADD_ONION <file content> Port=...`. Tor accepts only
//!   `ED25519-V3:` and the base64 of 64 bytes. [`OnionV3Key::parse`] returns `Ok(None)` for the
//!   empty file and accepts exactly that form (canonical padded base64, no whitespace). Anything
//!   else Core would paste into a command that Tor refuses, or, with a space or line break in it,
//!   that would run as more than one command; it is an error here and never reaches a socket
//!   through this crate. That is the one deliberate difference from Core's reader.
//! - **`i2p_private_key`**: Core reads any bytes and later fails with "The private key is too
//!   short" below 387 bytes or when `387 + the big-endian u16 at offset 385` exceeds the length
//!   (`Session::MyDestination`). [`I2pPrivateKey::parse`] applies exactly those two rules; an empty
//!   file is therefore an error, as it is for Core. A size cap of
//!   [`MAX_I2P_KEY_FILE_SIZE`] is the only bound (Core has none; the longest destination is
//!   65,922 bytes and a real Ed25519 key file is 679).

use std::io::Read;
use std::path::{Path, PathBuf};

use bitcoin_hashes::{Hash, sha256};

use crate::netstate::{FormatError, LoadError, NetAddr, open_regular_file};

/// The prefix of a Tor v3 service key: `ADD_ONION`'s key type.
const ONION_PREFIX: &[u8] = b"ED25519-V3:";
/// The size of Tor's expanded Ed25519 secret key.
const ONION_KEY_BYTES: usize = 64;
/// `ED25519-V3:` and 88 base64 characters.
pub const ONION_FILE_SIZE: usize = 11 + 88;

/// The largest `i2p_private_key` accepted (Core: no limit). A destination is at most 65,922
/// bytes and a private destination adds under 300 more.
pub const MAX_I2P_KEY_FILE_SIZE: usize = 128 << 10;

/// `Session::MyDestination`: the destination is `DEST_LEN_BASE` bytes plus the certificate.
const I2P_DEST_LEN_BASE: usize = 387;
const I2P_CERT_LEN_POS: usize = 385;

/// A buffer that is overwritten when it is dropped.
struct Secret(Vec<u8>);

impl Secret {
    fn wipe(bytes: &mut [u8]) {
        for b in bytes.iter_mut() {
            // SAFETY: `b` is a valid, aligned, exclusive reference.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        Secret::wipe(&mut self.0);
    }
}

/// Reads at most `limit + 1` bytes of `reader` into one buffer allocated up front and returns
/// them (`limit + 1` bytes mean "too large"). The buffer is never reallocated, so a reader that
/// delivers more than anyone expected cannot leave a freed copy of a prefix of the key behind, and
/// nothing is read through a stack buffer either (`Read::read_to_end` probes with one).
fn read_secret(mut reader: impl Read, limit: usize) -> std::io::Result<Secret> {
    let mut secret = Secret(vec![0; limit.saturating_add(1)]);
    let mut filled = 0;
    while filled < secret.0.len() {
        match reader.read(&mut secret.0[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // The buffer is overwritten when `secret` drops.
            Err(e) => return Err(e),
        }
    }
    // Only what was read is kept; the rest was never written and is zero.
    secret.0.truncate(filled);
    Ok(secret)
}

/// Reads a key file: the file is opened first and refused unless it is a regular file, and at
/// most `limit + 1` bytes are read into one buffer (see [`read_secret`]).
fn read_secret_file(path: &Path, limit: usize, what: &'static str) -> Result<Secret, LoadError> {
    let file = open_regular_file(path)?;
    let secret = read_secret(file, limit).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if secret.0.len() > limit {
        return Err(FormatError::TooLarge {
            what,
            size: secret.0.len() as u64,
            limit: limit as u64,
        }
        .into());
    }
    Ok(secret)
}

/// A temporary copy of a key that could not be removed: a file holding the key, mode `0600`,
/// that the caller has to delete.
#[derive(Debug)]
pub struct Leftover {
    pub path: PathBuf,
    pub source: std::io::Error,
}

/// How [`write_secret_file`] failed, or only half succeeded.
#[derive(Debug)]
pub enum KeyWriteError {
    /// Nothing is installed at `path` (it was left alone). `leftover` is set if the temporary
    /// copy of the key could not be removed either.
    NotInstalled {
        source: std::io::Error,
        leftover: Option<Leftover>,
    },
    /// The key **is** installed at `path`, complete and mode `0600`, but a step after the link
    /// failed. `leftover`: a second copy of the key remains in the temporary file. `sync`: the
    /// directory could not be flushed, so the new name may not survive a crash (a node that
    /// finds no key then makes a new identity).
    Installed {
        leftover: Option<Leftover>,
        sync: Option<std::io::Error>,
    },
}

impl KeyWriteError {
    /// Whether the key is at the destination.
    pub fn is_installed(&self) -> bool {
        matches!(self, KeyWriteError::Installed { .. })
    }

    /// The error of the step that decided the outcome: the failure for `NotInstalled`, else the
    /// first of the cleanup failures.
    pub fn source_error(&self) -> Option<&std::io::Error> {
        match self {
            KeyWriteError::NotInstalled {
                source,
                ..
            } => Some(source),
            KeyWriteError::Installed {
                leftover,
                sync,
            } => leftover.as_ref().map(|l| &l.source).or(sync.as_ref()),
        }
    }
}

impl std::fmt::Display for KeyWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let leftover = |f: &mut std::fmt::Formatter<'_>, l: &Option<Leftover>| match l {
            Some(l) => write!(
                f,
                "; the temporary copy of the key {} could not be removed ({}) and must be deleted",
                l.path.display(),
                l.source
            ),
            None => Ok(()),
        };
        match self {
            KeyWriteError::NotInstalled {
                source,
                leftover: l,
            } => {
                write!(f, "the key file was not written: {source}")?;
                leftover(f, l)
            }
            KeyWriteError::Installed {
                leftover: l,
                sync,
            } => {
                f.write_str("the key file was written, but")?;
                match sync {
                    Some(e) => write!(
                        f,
                        " its directory could not be synced ({e}): the file may not survive a crash"
                    )?,
                    None => f.write_str(" cleaning up failed")?,
                }
                leftover(f, l)
            }
        }
    }
}

impl std::error::Error for KeyWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source_error().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// The steps of [`write_secret_file`] that follow the link, behind a seam so the tests can make
/// each of them fail (a real directory refuses neither on demand; a failed link is a real one:
/// an existing destination).
trait KeyFs {
    fn remove(&self, path: &Path) -> std::io::Result<()>;
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()>;
}

struct RealFs;

impl KeyFs for RealFs {
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::fs::File::open(dir)?.sync_all()
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
            Ok(())
        }
    }
}

/// Writes `bytes` to `path` as a new file of mode `0600`, atomically and without replacing
/// anything.
///
/// The bytes go to a sibling file created with `O_EXCL` and `O_NOFOLLOW` and mode `0600` (so the
/// content is never readable by anyone else at any moment), are flushed to disk, and the file is
/// linked into place; an existing `path` (file, directory or link) is a
/// [`NotInstalled`](KeyWriteError::NotInstalled) error of kind
/// [`AlreadyExists`](std::io::ErrorKind::AlreadyExists) and is left alone. The sibling's name is
/// unique per call (process id, a counter and the clock); one that is taken anyway is skipped,
/// never removed, and is not reported as `path` existing.
///
/// Once the key is linked in, the sibling is removed and the directory flushed. Neither failure is
/// swallowed: either one returns [`Installed`](KeyWriteError::Installed), which says the key is
/// in place and what is wrong around it (a second copy left in the sibling, a name that a crash
/// may lose). A failure before the link removes the sibling too, and reports it as a `leftover`
/// when that fails. A crash between creating the sibling and removing it leaves a `0600` copy of
/// the key named `.<name>.tmp...` in the directory. On non-Unix targets the mode cannot be set;
/// the caller must put the file in a directory only the node's user can read.
pub fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), KeyWriteError> {
    write_secret_file_with(&RealFs, path, bytes)
}

fn write_secret_file_with(fs: &dyn KeyFs, path: &Path, bytes: &[u8]) -> Result<(), KeyWriteError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let not_installed = |source| KeyWriteError::NotInstalled {
        source,
        leftover: None,
    };
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        not_installed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no file name in the key path",
        ))
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut created = None;
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(
            ".tmp{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Relaxed)
        ));
        let tmp = dir.join(tmp_name);
        match options.open(&tmp) {
            Ok(file) => {
                created = Some((tmp, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(not_installed(e)),
        }
    }
    let Some((tmp, mut file)) = created else {
        return Err(not_installed(std::io::Error::other(
            "no free temporary file name for the key",
        )));
    };
    let linked = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&tmp, path)
    })();
    // Only the sibling this call created is removed; one that is already gone is as good.
    let leftover = match fs.remove(&tmp) {
        Err(source) if source.kind() != std::io::ErrorKind::NotFound => Some(Leftover {
            path: tmp,
            source,
        }),
        _ => None,
    };
    match linked {
        Err(source) => Err(KeyWriteError::NotInstalled {
            source,
            leftover,
        }),
        Ok(()) => {
            let sync = fs.sync_dir(dir).err();
            if leftover.is_none() && sync.is_none() {
                Ok(())
            } else {
                Err(KeyWriteError::Installed {
                    leftover,
                    sync,
                })
            }
        }
    }
}

/// The standard base64 alphabet, padded.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// Decodes canonical padded base64 of exactly [`ONION_KEY_BYTES`] bytes (88 characters, `==`).
fn decode_key_b64(text: &[u8]) -> Option<Secret> {
    if text.len() != 88 || !text.ends_with(b"==") {
        return None;
    }
    let mut out = Secret(Vec::with_capacity(66));
    let groups = text.len() / 4;
    for (i, chunk) in text.chunks(4).enumerate() {
        let mut acc = 0u32;
        let mut pad = 0;
        for &c in chunk {
            acc <<= 6;
            if c == b'=' {
                pad += 1;
            } else if pad == 0 {
                acc |= u32::from(b64_value(c)?);
            } else {
                return None;
            }
        }
        // Padding belongs to the last group only, and a group has two data characters at least.
        if pad > 0 && (i + 1 != groups || pad > 2) {
            return None;
        }
        let bytes = acc.to_be_bytes();
        out.0.extend_from_slice(&bytes[1..4 - pad]);
    }
    // The last group is two data characters (12 bits) for one byte: the 4 spare bits must be
    // zero for the encoding to be canonical.
    if out.0.len() != ONION_KEY_BYTES || b64_value(text[85])? & 0x0f != 0 {
        return None;
    }
    Some(out)
}

fn encode_key_b64(key: &[u8; ONION_KEY_BYTES]) -> Secret {
    let mut out = Secret(Vec::with_capacity(88));
    for chunk in key.chunks(3) {
        let n = chunk.len();
        let acc = chunk.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - n));
        for i in 0..4 {
            if i <= n {
                out.0.push(B64[((acc >> (18 - 6 * i)) & 63) as usize]);
            } else {
                out.0.push(b'=');
            }
        }
    }
    out
}

/// A Tor v3 onion service's key as Tor returns it from `ADD_ONION` (the 64-byte expanded secret
/// key) and Core stores it.
pub struct OnionV3Key {
    key: Secret,
}

impl OnionV3Key {
    /// Parses the content of `onion_v3_private_key`. `Ok(None)` is the empty file ("no key": Core
    /// requests a new one). See the module documentation for what is accepted.
    // Core: torcontrol.cpp TorController::TorController (ReadBinaryFile), add_onion_cb
    pub fn parse(bytes: &[u8]) -> Result<Option<OnionV3Key>, FormatError> {
        const WHAT: &str = "onion_v3_private_key";
        if bytes.is_empty() {
            return Ok(None);
        }
        let Some(b64) = bytes.strip_prefix(ONION_PREFIX) else {
            return Err(FormatError::Corrupt {
                what: WHAT,
                reason: "not an ED25519-V3 key",
            });
        };
        match decode_key_b64(b64) {
            Some(key) => Ok(Some(OnionV3Key {
                key,
            })),
            None => Err(FormatError::Corrupt {
                what: WHAT,
                reason: "key is not canonical base64 of 64 bytes",
            }),
        }
    }

    /// Reads and parses the file at `path` (a regular file of at most [`ONION_FILE_SIZE`] bytes).
    pub fn read(path: &Path) -> Result<Option<OnionV3Key>, LoadError> {
        let secret = read_secret_file(path, ONION_FILE_SIZE, "onion_v3_private_key")?;
        Ok(OnionV3Key::parse(&secret.0)?)
    }

    /// The 64-byte expanded secret key (Tor's format: the clamped scalar and the hash prefix).
    pub fn expanded_secret(&self) -> &[u8] {
        &self.key.0
    }

    /// The file content Core writes: `ED25519-V3:` and the base64 of the key. The buffer is
    /// overwritten when it is dropped.
    pub fn to_file_bytes(&self) -> SecretBytes {
        let key: &[u8; ONION_KEY_BYTES] = self.key.0.as_slice().try_into().expect("64 bytes");
        let mut out = Secret(Vec::with_capacity(ONION_FILE_SIZE));
        out.0.extend_from_slice(ONION_PREFIX);
        out.0.extend_from_slice(&encode_key_b64(key).0);
        SecretBytes(out)
    }

    /// Writes the key to `path` as Core would write it, with mode `0600` (see
    /// [`write_secret_file`]).
    pub fn write(&self, path: &Path) -> Result<(), KeyWriteError> {
        write_secret_file(path, &self.to_file_bytes().0.0)
    }
}

impl std::fmt::Debug for OnionV3Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnionV3Key(<redacted>)")
    }
}

/// An I2P private destination as `DEST GENERATE` returns it and Core stores it.
pub struct I2pPrivateKey {
    key: Secret,
    /// The length of the destination (the public part) at the front of the key.
    destination_len: usize,
}

impl I2pPrivateKey {
    /// Parses the content of `i2p_private_key` with Core's two rules (see the module
    /// documentation).
    // Core: i2p.cpp Session::MyDestination, Session::CreateIfNotCreatedAlready
    pub fn parse(bytes: &[u8]) -> Result<I2pPrivateKey, FormatError> {
        const WHAT: &str = "i2p_private_key";
        if bytes.len() > MAX_I2P_KEY_FILE_SIZE {
            return Err(FormatError::TooLarge {
                what: WHAT,
                size: bytes.len() as u64,
                limit: MAX_I2P_KEY_FILE_SIZE as u64,
            });
        }
        let Some(cert_len) = bytes.get(I2P_CERT_LEN_POS..I2P_CERT_LEN_POS + 2) else {
            return Err(FormatError::Corrupt {
                what: WHAT,
                reason: "the private key is too short",
            });
        };
        let destination_len =
            I2P_DEST_LEN_BASE + usize::from(u16::from_be_bytes([cert_len[0], cert_len[1]]));
        if destination_len > bytes.len() {
            return Err(FormatError::Corrupt {
                what: WHAT,
                reason: "the certificate length designates a destination longer than the key",
            });
        }
        Ok(I2pPrivateKey {
            key: Secret(bytes.to_vec()),
            destination_len,
        })
    }

    /// Reads and parses the file at `path` (a regular file of at most [`MAX_I2P_KEY_FILE_SIZE`]).
    pub fn read(path: &Path) -> Result<I2pPrivateKey, LoadError> {
        let secret = read_secret_file(path, MAX_I2P_KEY_FILE_SIZE, "i2p_private_key")?;
        Ok(I2pPrivateKey::parse(&secret.0)?)
    }

    /// The whole private destination, as the file holds it.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key.0
    }

    /// The public destination at the front of the key (`Session::MyDestination`).
    pub fn destination(&self) -> &[u8] {
        &self.key.0[..self.destination_len]
    }

    /// The node's I2P address: the SHA-256 of the destination (`DestBinToAddr`).
    // Core: i2p.cpp DestBinToAddr
    pub fn address(&self) -> NetAddr {
        NetAddr::I2p(sha256::Hash::hash(self.destination()).to_byte_array())
    }

    /// The address as I2P writes it: the lowercase unpadded base32 of the hash and `.b32.i2p`.
    pub fn b32_address(&self) -> String {
        const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
        let hash = sha256::Hash::hash(self.destination()).to_byte_array();
        let mut out = String::with_capacity(52 + 8);
        let (mut acc, mut bits) = (0u32, 0);
        for byte in hash {
            acc = (acc << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(char::from(ALPHABET[((acc >> bits) & 31) as usize]));
            }
            acc &= (1 << bits) - 1;
        }
        if bits > 0 {
            out.push(char::from(ALPHABET[((acc << (5 - bits)) & 31) as usize]));
        }
        out.push_str(".b32.i2p");
        out
    }

    /// Writes the key to `path` as Core would write it, with mode `0600` (see
    /// [`write_secret_file`]).
    pub fn write(&self, path: &Path) -> Result<(), KeyWriteError> {
        write_secret_file(path, &self.key.0)
    }
}

impl std::fmt::Debug for I2pPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("I2pPrivateKey(<redacted>)")
    }
}

/// Bytes of a key file that are overwritten when dropped; its `Debug` shows only the length.
pub struct SecretBytes(Secret);

impl SecretBytes {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0.0
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes({} bytes, <redacted>)", self.0.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    /// A reader that hands out `total` bytes in `chunk`-byte pieces, whatever size anyone expected,
    /// and notes where in memory each piece was asked for.
    struct Growing {
        total: usize,
        chunk: usize,
        given: usize,
        buffers: Vec<(usize, usize)>,
    }

    impl Read for Growing {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.buffers.push((buf.as_ptr() as usize, buf.len()));
            let n = self.chunk.min(buf.len()).min(self.total - self.given);
            for b in &mut buf[..n] {
                *b = 0xa5;
            }
            self.given += n;
            Ok(n)
        }
    }

    #[test]
    fn a_reader_that_delivers_more_than_expected_never_moves_the_buffer() {
        // A file that grows while it is read brings far more than the limit, in small pieces:
        // every request must land in the one buffer, so no freed copy of a prefix is left.
        let limit = 99;
        let mut reader = Growing {
            total: 10_000,
            chunk: 7,
            given: 0,
            buffers: Vec::new(),
        };
        let secret = read_secret(&mut reader, limit).unwrap();
        // One byte past the limit is read, which is how "too large" is told.
        assert_eq!(secret.0.len(), limit + 1);
        assert!(secret.0.iter().all(|&b| b == 0xa5));
        let buffers = &reader.buffers;
        let base = buffers[0].0;
        let mut at = 0;
        for &(ptr, len) in buffers.iter() {
            assert_eq!(ptr, base + at, "a read was not into the next bytes of the first buffer");
            assert_eq!(len, limit + 1 - at);
            at += reader.chunk.min(len);
        }
        assert_eq!(reader.given, limit + 1, "nothing past the limit is read");
    }

    #[test]
    fn a_short_reader_and_a_failing_one() {
        let secret = read_secret(&b"abc"[..], 99).unwrap();
        assert_eq!(secret.0, b"abc");
        assert!(read_secret(&b""[..], 99).unwrap().0.is_empty());
        struct Fails(bool);
        impl Read for Fails {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if std::mem::replace(&mut self.0, true) {
                    return Err(std::io::Error::other("disk gone"));
                }
                buf[..2].copy_from_slice(b"ke");
                Ok(2)
            }
        }
        let err = read_secret(Fails(false), 99).unwrap_err();
        assert_eq!(err.to_string(), "disk gone");
        // An interrupted read is retried.
        struct Interrupted(u8);
        impl Read for Interrupted {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.0 += 1;
                match self.0 {
                    1 => Err(ErrorKind::Interrupted.into()),
                    2 => {
                        buf[0] = 7;
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            }
        }
        assert_eq!(read_secret(Interrupted(0), 99).unwrap().0, [7]);
    }

    /// The real file operations, except the ones named, which fail.
    #[derive(Default)]
    struct Faulty {
        remove: Option<ErrorKind>,
        sync: Option<ErrorKind>,
    }

    impl KeyFs for Faulty {
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            match self.remove {
                Some(kind) => Err(kind.into()),
                None => RealFs.remove(path),
            }
        }

        fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
            match self.sync {
                Some(kind) => Err(kind.into()),
                None => RealFs.sync_dir(dir),
            }
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    const KEY: &[u8] = b"not a real key";

    /// One write of `KEY` to `<dir>/key` through `fs`; `existing` is put at the destination first.
    fn attempt(
        fs: &dyn KeyFs,
        existing: Option<&[u8]>,
    ) -> (tempfile::TempDir, PathBuf, Result<(), KeyWriteError>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        if let Some(old) = existing {
            std::fs::write(&path, old).unwrap();
        }
        let result = write_secret_file_with(fs, &path, KEY);
        (dir, path, result)
    }

    #[test]
    fn a_clean_write_leaves_only_the_key() {
        let (dir, path, result) = attempt(&Faulty::default(), None);
        result.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), KEY);
        assert_eq!(names(dir.path()), ["key"]);
    }

    #[test]
    fn a_failed_link_installs_nothing_and_removes_the_copy() {
        // The destination exists, so the link fails; the old file stays and no copy of `KEY` does.
        let (dir, path, result) = attempt(&Faulty::default(), Some(b"old"));
        let err = result.unwrap_err();
        assert!(!err.is_installed());
        let KeyWriteError::NotInstalled {
            source,
            leftover,
        } = &err
        else {
            panic!("{err:?}")
        };
        assert_eq!(source.kind(), ErrorKind::AlreadyExists);
        assert!(leftover.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(names(dir.path()), ["key"]);
    }

    #[test]
    fn a_failed_link_and_a_failed_removal_name_the_copy_that_is_left() {
        let fs = Faulty {
            remove: Some(ErrorKind::PermissionDenied),
            ..Faulty::default()
        };
        let (_dir, path, result) = attempt(&fs, Some(b"old"));
        let err = result.unwrap_err();
        let KeyWriteError::NotInstalled {
            leftover: Some(left),
            ..
        } = &err
        else {
            panic!("{err:?}")
        };
        // The copy is where the error says, and holds the key: the caller can delete it.
        assert_eq!(std::fs::read(&left.path).unwrap(), KEY);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(err.to_string().contains(&left.path.display().to_string()));
        assert!(!err.to_string().contains("not a real key"), "an error never carries the key");
    }

    #[test]
    fn an_installed_key_whose_copy_cannot_be_removed_is_a_partial_success() {
        let fs = Faulty {
            remove: Some(ErrorKind::PermissionDenied),
            ..Faulty::default()
        };
        let (_dir, path, result) = attempt(&fs, None);
        let err = result.unwrap_err();
        assert!(err.is_installed());
        let KeyWriteError::Installed {
            leftover: Some(left),
            sync: None,
        } = &err
        else {
            panic!("{err:?}")
        };
        assert_eq!(std::fs::read(&path).unwrap(), KEY);
        assert_eq!(std::fs::read(&left.path).unwrap(), KEY);
        assert_eq!(err.source_error().unwrap().kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn an_installed_key_whose_directory_cannot_be_synced_is_a_partial_success() {
        let fs = Faulty {
            sync: Some(ErrorKind::Other),
            ..Faulty::default()
        };
        let (dir, path, result) = attempt(&fs, None);
        let err = result.unwrap_err();
        assert!(err.is_installed());
        assert!(matches!(
            err,
            KeyWriteError::Installed {
                leftover: None,
                sync: Some(_)
            }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), KEY);
        assert_eq!(names(dir.path()), ["key"]);
        assert!(err.to_string().contains("may not survive a crash"));
    }

    #[test]
    fn both_cleanup_failures_are_reported_together() {
        let fs = Faulty {
            remove: Some(ErrorKind::PermissionDenied),
            sync: Some(ErrorKind::Other),
        };
        let (_dir, _path, result) = attempt(&fs, None);
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            KeyWriteError::Installed {
                leftover: Some(_),
                sync: Some(_)
            }
        ));
    }

    #[test]
    fn a_copy_that_is_already_gone_is_no_failure() {
        // The copy vanished between the link and its removal: the goal is reached.
        struct Vanishes;
        impl KeyFs for Vanishes {
            fn remove(&self, path: &Path) -> std::io::Result<()> {
                RealFs.remove(path)?;
                Err(ErrorKind::NotFound.into())
            }

            fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
                RealFs.sync_dir(dir)
            }
        }
        let (dir, _path, result) = attempt(&Vanishes, None);
        result.unwrap();
        assert_eq!(names(dir.path()), ["key"]);
    }
}

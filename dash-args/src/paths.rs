//! The `std::filesystem` behaviour ArgsManager relies on.
//!
//! `GetPathArg` normalizes with `fs::path::lexically_normal`, whose rules (keep `..` that
//! cannot be resolved, keep a trailing separator, turn an empty result into `.`) differ from
//! `std::path::Path::components`. This module ports them, for POSIX paths and, on Windows,
//! drive root names with both separators.

use std::fs;
use std::path::{Path, PathBuf};

use crate::univalue::RuntimeError;

/// Core: `BITCOIN_CONF_FILENAME`.
pub const DASH_CONF_FILENAME: &str = "dash.conf";
/// Core: `BITCOIN_SETTINGS_FILENAME`.
pub const DASH_SETTINGS_FILENAME: &str = "settings.json";

#[cfg(windows)]
fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}
#[cfg(not(windows))]
fn is_separator(c: char) -> bool {
    c == '/'
}

#[cfg(windows)]
const PREFERRED_SEPARATOR: char = '\\';
#[cfg(not(windows))]
const PREFERRED_SEPARATOR: char = '/';

/// Split off the root name (Windows `C:`; none on POSIX).
#[cfg(windows)]
fn split_root_name(s: &str) -> (&str, &str) {
    // libstdc++ looks at UTF-16 units: a non-BMP first character takes two.
    let units: Vec<u16> = s.encode_utf16().collect();
    let len = win::root_name_len(&units);
    let mut end = 0;
    let mut seen = 0;
    for c in s.chars() {
        if seen >= len {
            break;
        }
        seen += c.len_utf16();
        end += c.len_utf8();
    }
    s.split_at(end)
}
#[cfg(not(windows))]
fn split_root_name(s: &str) -> (&str, &str) {
    ("", s)
}

/// libstdc++'s `std::filesystem::path` on Windows (MinGW, which Core's Windows releases are built
/// with): root names, `is_absolute`, `operator/` and the input `absolute` hands to
/// `GetFullPathNameW`. Generic over UTF-16 units (Windows) and chars, so it is tested anywhere.
///
/// libstdc++ only knows drive root names: `\\server\share` and `\\?\C:\x` are a root
/// directory and a relative path, so `absolute` resolves them on the current drive.
#[cfg(any(windows, test))]
pub(crate) mod win {
    pub(crate) trait Unit: Copy + PartialEq + Into<u32> + From<u8> {}
    impl<T: Copy + PartialEq + Into<u32> + From<u8>> Unit for T {}

    fn is_sep<T: Unit>(c: T) -> bool {
        let c = c.into();
        c == u32::from(b'/') || c == u32::from(b'\\')
    }

    /// The length of the root name: 2 for `X:` (any `X` but a separator), else 0.
    pub(crate) fn root_name_len<T: Unit>(p: &[T]) -> usize {
        if p.len() >= 2 && !is_sep(p[0]) && p[1].into() == u32::from(b':') {
            2
        } else {
            0
        }
    }

    fn has_root_directory<T: Unit>(p: &[T]) -> bool {
        p.get(root_name_len(p)).is_some_and(|&c| is_sep(c))
    }

    /// A root name and a root directory: `C:x` and `\x` are relative.
    pub(crate) fn is_absolute<T: Unit>(p: &[T]) -> bool {
        root_name_len(p) > 0 && has_root_directory(p)
    }

    /// Whether the last element is a filename (not a root and not after a trailing separator).
    fn has_filename<T: Unit>(p: &[T]) -> bool {
        let rest = &p[root_name_len(p)..];
        rest.last().is_some_and(|&c| !is_sep(c))
    }

    /// `base / p`. An absolute `p`, or one on another root name, replaces `base`; a `p` with a
    /// root directory keeps only `base`'s root name; otherwise `p` (less a root name equal to
    /// `base`'s) is appended, after a separator if `base` ends in a filename. Root names are
    /// compared as strings, as libstdc++ does.
    pub(crate) fn join<T: Unit>(base: &[T], p: &[T]) -> Vec<T> {
        let base_root = &base[..root_name_len(base)];
        let p_root_len = root_name_len(p);
        if is_absolute(p) || (p_root_len > 0 && &p[..p_root_len] != base_root) {
            return p.to_vec();
        }
        let mut out = if has_root_directory(p) {
            base_root.to_vec()
        } else {
            base.to_vec()
        };
        if !has_root_directory(p) && has_filename(base) {
            out.push(T::from(b'\\'));
        }
        out.extend_from_slice(&p[p_root_len..]);
        out
    }

    /// What msvcrt's `_wstat` reports as `st_mode` and `st_dev` for an existing file: its type,
    /// read and (unless read-only) write permission, execute permission for a directory or a
    /// name ending in `.exe`, `.bat`, `.cmd` or `.com` (any case), each copied to group and
    /// others; and the number of the drive the path names (0 for `A:`), or of the current
    /// drive. libstdc++'s `equivalent` compares both before the files' identities.
    pub(crate) fn msvcrt_stat<T: Unit>(
        p: &[T],
        is_dir: bool,
        readonly: bool,
        current_drive: i32,
    ) -> (u32, i32) {
        const S_IFDIR: u32 = 0o040000;
        const S_IFREG: u32 = 0o100000;
        let lower = |c: T| char::from_u32(c.into()).map(|c| c.to_ascii_lowercase());
        let executable = [".exe", ".bat", ".cmd", ".com"].iter().any(|ext| {
            p.len() >= 4
                && p[p.len() - 4..].iter().zip(ext.chars()).all(|(&c, e)| lower(c) == Some(e))
        });
        let mut user = 0o400;
        if !readonly {
            user |= 0o200;
        }
        if is_dir || executable {
            user |= 0o100;
        }
        let mode = if is_dir {
            S_IFDIR
        } else {
            S_IFREG
        } | user
            | user >> 3
            | user >> 6;
        let drive = match p {
            [letter, colon, ..] if (*colon).into() == u32::from(b':') => {
                match char::from_u32((*letter).into()) {
                    Some(c) if c.is_ascii_alphabetic() => {
                        i32::from(c.to_ascii_uppercase() as u8 - b'A')
                    }
                    _ => current_drive,
                }
            }
            _ => current_drive,
        };
        (mode, drive)
    }

    /// What `std::filesystem::absolute` passes to `GetFullPathNameW` for a non-empty relative
    /// path: the path, less all but one of several leading separators (GCC PR 88884).
    pub(crate) fn full_path_input<T: Unit>(p: &[T]) -> &[T] {
        if root_name_len(p) == 0 && has_root_directory(p) {
            let seps = p.iter().take_while(|&&c| is_sep(c)).count();
            return &p[seps - 1..];
        }
        p
    }
}

/// The Win32 calls Core makes through libstdc++ and `GetSpecialFolderPath`.
#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    /// `BY_HANDLE_FILE_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct ByHandleFileInformation {
        pub file_attributes: u32,
        pub creation_time: [u32; 2],
        pub last_access_time: [u32; 2],
        pub last_write_time: [u32; 2],
        pub volume_serial_number: u32,
        pub file_size_high: u32,
        pub file_size_low: u32,
        pub number_of_links: u32,
        pub file_index_high: u32,
        pub file_index_low: u32,
    }

    pub(super) const CSIDL_APPDATA: i32 = 0x001a;
    pub(super) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    pub(super) const MAX_PATH: usize = 260;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn GetFullPathNameW(
            file_name: *const u16,
            buffer_length: u32,
            buffer: *mut u16,
            file_part: *mut *mut u16,
        ) -> u32;
        pub(super) fn GetFileInformationByHandle(
            file: *mut c_void,
            info: *mut ByHandleFileInformation,
        ) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        pub(super) fn SHGetSpecialFolderPathW(
            owner: *mut c_void,
            path: *mut u16,
            csidl: i32,
            create: i32,
        ) -> i32;
    }
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().collect()
}

#[cfg(windows)]
fn from_wide(units: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_wide(units))
}

/// `std::filesystem::path::is_absolute`: on Windows, a root name and a root directory.
pub fn is_absolute(path: &Path) -> bool {
    #[cfg(windows)]
    {
        win::is_absolute(&wide(path))
    }
    #[cfg(not(windows))]
    {
        path.is_absolute()
    }
}

/// `std::filesystem::path::operator/`.
fn join(base: &Path, path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        from_wide(&win::join(&wide(base), &wide(path)))
    }
    #[cfg(not(windows))]
    {
        base.join(path)
    }
}

/// `std::filesystem::path::lexically_normal` (C++17 [fs.path.generic] normal form).
pub fn lexically_normal(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let (root_name, rest) = split_root_name(path);
    let has_root_dir = rest.starts_with(is_separator);
    let elements: Vec<&str> = rest.split(is_separator).filter(|e| !e.is_empty()).collect();
    let mut trailing_sep = rest.ends_with(is_separator) && !elements.is_empty();

    let mut out: Vec<&str> = Vec::new();
    let last = elements.len().saturating_sub(1);
    for (i, &e) in elements.iter().enumerate() {
        match e {
            // Remove each dot filename and any immediately following separator; a dot at the
            // end leaves the separator before it.
            "." => trailing_sep |= i == last,
            ".." => {
                if out.last().is_some_and(|&prev| prev != "..") {
                    // Remove a non-dot-dot filename followed by a dot-dot.
                    out.pop();
                    trailing_sep |= i == last;
                } else if has_root_dir && out.is_empty() {
                    // Dot-dot directly after the root directory is removed.
                    trailing_sep |= i == last;
                } else {
                    out.push(e);
                }
            }
            _ => out.push(e),
        }
    }
    // If the last filename is dot-dot, remove any trailing separator.
    if out.last() == Some(&"..") {
        trailing_sep = false;
    }

    let mut result = root_name.replace(is_separator, &PREFERRED_SEPARATOR.to_string());
    if has_root_dir {
        result.push(PREFERRED_SEPARATOR);
    }
    result.push_str(&out.join(&PREFERRED_SEPARATOR.to_string()));
    if trailing_sep && !out.is_empty() {
        result.push(PREFERRED_SEPARATOR);
    }
    // If the path is empty, add a dot.
    if result.is_empty() {
        result.push('.');
    }
    result
}

/// `has_filename() ? p : p.parent_path()` on a normalized path: drops a trailing separator.
pub(crate) fn strip_trailing_separator(normal: &str) -> String {
    let (root_name, rest) = split_root_name(normal);
    let trimmed = rest.trim_end_matches(is_separator);
    if rest.ends_with(is_separator) && !trimmed.is_empty() {
        format!("{root_name}{trimmed}")
    } else {
        normal.to_string()
    }
}

/// Core: `ArgsManager::GetPathArg`'s normalization of a non-empty value.
pub fn normalize_path_arg(value: &str) -> PathBuf {
    PathBuf::from(strip_trailing_separator(&lexically_normal(value)))
}

/// The throwing `std::filesystem::absolute`: an absolute path as is; a relative one joined to
/// the current directory without normalization on POSIX, or resolved by `GetFullPathNameW` on
/// Windows (which applies the current directory of the path's drive). libstdc++ refuses an
/// empty path.
pub fn absolute(path: &Path) -> Result<PathBuf, RuntimeError> {
    const OP: &str = "cannot make absolute path";
    if path.as_os_str().is_empty() {
        // libstdc++ reports EINVAL, whose strerror text this is.
        return Err(RuntimeError(format!(
            "filesystem error: {OP}: Invalid argument [{}]",
            path.display()
        )));
    }
    if is_absolute(path) {
        return Ok(path.to_path_buf());
    }
    #[cfg(windows)]
    {
        get_full_path_name(win::full_path_input(&wide(path))).map_err(|e| fs_error(OP, &e, path))
    }
    #[cfg(not(windows))]
    {
        std::env::current_dir().map(|cwd| cwd.join(path)).map_err(|e| fs_error(OP, &e, path))
    }
}

/// `GetFullPathNameW`, growing the buffer as libstdc++ does.
#[cfg(windows)]
fn get_full_path_name(input: &[u16]) -> std::io::Result<PathBuf> {
    let mut name = input.to_vec();
    name.push(0);
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: `name` is NUL-terminated and `buf` holds `buf.len()` units.
        let len = unsafe {
            sys::GetFullPathNameW(
                name.as_ptr(),
                buf.len() as u32,
                buf.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        } as usize;
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if len <= buf.len() {
            return Ok(from_wide(&buf[..len]));
        }
        // Too small: `len` is the size needed, terminator included.
        buf.resize(len, 0);
    }
}

/// Core: `fsbridge::AbsPathJoin`. Panics, as Core asserts, if `base` is not absolute.
pub fn abs_path_join(base: &Path, path: &Path) -> PathBuf {
    assert!(is_absolute(base), "AbsPathJoin: base path {} is not absolute", base.display());
    if path.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        join(base, path)
    }
}

/// Whose default data directory [`default_data_dir`] names: Dash Core's, or the Rust node's,
/// which must never share it (DEC-201 Q5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Product {
    /// Dash Core's programs: `DashCore` and `.dashcore`.
    #[default]
    DashCore,
    /// rdashd: `RDashCore` and `.rdashcore`.
    RDashCore,
}

impl Product {
    /// The directory name on Windows and macOS, and the one in the home directory elsewhere.
    fn dir_names(self) -> (&'static str, &'static str) {
        match self {
            Product::DashCore => ("DashCore", ".dashcore"),
            Product::RDashCore => ("RDashCore", ".rdashcore"),
        }
    }
}

/// Core: `GetDefaultDataDir`, for `product` (Dash Core's names shown):
/// - Windows: the shell's roaming application data folder (`CSIDL_APPDATA`), `\DashCore`
/// - macOS: `~/Library/Application Support/DashCore`
/// - other Unix: `~/.dashcore`
///
/// `log` gets Core's log line if the shell cannot name the folder (Core then uses a relative
/// `DashCore`).
pub fn default_data_dir(product: Product, log: &mut dyn FnMut(String)) -> PathBuf {
    #[cfg_attr(windows, allow(unused_variables))]
    let (named, dotted) = product.dir_names();
    #[cfg(windows)]
    {
        // Core: `GetSpecialFolderPath(CSIDL_APPDATA)`, which creates the folder if needed.
        let mut buf = [0u16; sys::MAX_PATH];
        // SAFETY: `buf` holds MAX_PATH units, as SHGetSpecialFolderPathW requires.
        let ok = unsafe {
            sys::SHGetSpecialFolderPathW(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                sys::CSIDL_APPDATA,
                1,
            )
        } != 0;
        let appdata = if ok {
            from_wide(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
        } else {
            log("SHGetSpecialFolderPathW() failed, could not obtain requested path.\n".to_string());
            PathBuf::new()
        };
        join(&appdata, Path::new(named))
    }
    #[cfg(not(windows))]
    {
        let _ = log;
        let home = match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home),
            _ => PathBuf::from("/"),
        };
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support").join(named)
        } else {
            home.join(dotted)
        }
    }
}

/// `std::filesystem::equivalent`, for the case Core uses it in (`base` exists): true if both
/// paths exist and name the same file.
pub(crate) fn equivalent(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        // libstdc++ compares msvcrt `_wstat`'s mode and device first, then opens both and
        // compares the volume serial number and file index.
        let current_drive = std::env::current_dir()
            .ok()
            .map(|cwd| wide(&cwd))
            .and_then(|cwd| match cwd.as_slice() {
                [letter, colon, ..] if *colon == u16::from(b':') => {
                    char::from_u32((*letter).into())
                        .filter(char::is_ascii_alphabetic)
                        .map(|c| i32::from(c.to_ascii_uppercase() as u8 - b'A'))
                }
                _ => None,
            })
            // msvcrt's `_getdrive` is 0 off a drive, which makes the device -1.
            .unwrap_or(-1);
        let stat = |path: &Path| {
            let meta = fs::metadata(path).ok()?;
            let readonly = meta.permissions().readonly();
            Some(win::msvcrt_stat(&wide(path), meta.is_dir(), readonly, current_drive))
        };
        match (stat(a), stat(b)) {
            (Some(sa), Some(sb)) if sa == sb => {}
            _ => return false,
        }
        match (file_identity(a), file_identity(b)) {
            (Some(ia), Some(ib)) => ia == ib,
            _ => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(ca), Ok(cb)) => ca == cb,
            _ => false,
        }
    }
}

/// The volume serial number and file index of the file `path` names (hard links share them).
#[cfg(windows)]
fn file_identity(path: &Path) -> Option<(u32, u32, u32)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    // libstdc++: CreateFileW with no access, sharing everything, OPEN_EXISTING and
    // FILE_FLAG_BACKUP_SEMANTICS (which also opens directories).
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(7)
        .custom_flags(sys::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    let mut info = sys::ByHandleFileInformation::default();
    // SAFETY: the handle is open for the duration of the call and `info` is writable.
    let ok = unsafe { sys::GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0;
    ok.then_some((info.volume_serial_number, info.file_index_high, info.file_index_low))
}

/// What reading a file opened in text mode yields on Windows (Core's `std::ifstream`s): the
/// input ends at the first Ctrl-Z and each CR LF reads as LF.
#[cfg(any(windows, test))]
pub(crate) fn text_mode_read(bytes: &[u8]) -> Vec<u8> {
    let end = bytes.iter().position(|&b| b == 0x1a).unwrap_or(bytes.len());
    let mut out = Vec::with_capacity(end);
    for (i, &b) in bytes[..end].iter().enumerate() {
        if !(b == b'\r' && bytes[..end].get(i + 1) == Some(&b'\n')) {
            out.push(b);
        }
    }
    out
}

/// What writing `text` in text mode stores on Windows (Core's `std::ofstream`s): each LF as
/// CR LF.
#[cfg(any(windows, test))]
pub(crate) fn text_mode_write(text: &str) -> String {
    text.replace('\n', "\r\n")
}

/// The `what()` of the `std::filesystem::filesystem_error` libstdc++ throws:
/// `filesystem error: <op>: <strerror> [<path>]`.
pub(crate) fn fs_error(op: &str, err: &std::io::Error, path: &Path) -> RuntimeError {
    let message = err.to_string();
    // Rust appends " (os error N)" to the strerror text both use.
    let message = match message.rfind(" (os error ") {
        Some(i) if err.raw_os_error().is_some() => &message[..i],
        _ => &message,
    };
    RuntimeError(format!("filesystem error: {op}: {message} [{}]", path.display()))
}

/// `std::filesystem::status(p).type()`: `None` for "not found" (`ENOENT`, `ENOTDIR`), an error
/// for any other failure. On Windows libstdc++ calls msvcrt's `_wstat`, which reports every
/// failure (such as an invalid name) as `ENOENT`. (Its successes differ slightly: see
/// ARCHITECTURE.md.)
fn status(path: &Path) -> Result<Option<fs::Metadata>, RuntimeError> {
    if cfg!(windows) {
        return Ok(fs::metadata(path).ok());
    }
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(fs_error("status", &e, path)),
    }
}

/// The throwing `std::filesystem::exists`.
pub(crate) fn exists(path: &Path) -> Result<bool, RuntimeError> {
    Ok(status(path)?.is_some())
}

/// The throwing `std::filesystem::is_directory`.
pub(crate) fn is_directory(path: &Path) -> Result<bool, RuntimeError> {
    Ok(status(path)?.is_some_and(|meta| meta.is_dir()))
}

/// The throwing `std::filesystem::create_directories`. libstdc++ reports an existing
/// non-directory as `ENOTDIR` where `create_dir_all` reports `EEXIST`.
pub(crate) fn create_directories(path: &Path) -> Result<(), RuntimeError> {
    const OP: &str = "cannot create directories";
    fs::create_dir_all(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists && !path.is_dir() {
            RuntimeError(format!("filesystem error: {OP}: Not a directory [{}]", path.display()))
        } else {
            fs_error(OP, &e, path)
        }
    })
}

/// The throwing `std::filesystem::remove`: false if nothing was there; removes a file or an
/// empty directory.
pub(crate) fn remove(path: &Path) -> Result<bool, RuntimeError> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir(path),
        _ => fs::remove_file(path),
    };
    match result {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(fs_error("cannot remove", &e, path)),
    }
}

/// Core: `fs::PathToString`.
pub(crate) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexically_normal_matches_cpp17() {
        let cases = [
            ("", ""),
            (".", "."),
            ("./", "."),
            ("..", ".."),
            ("../", ".."),
            ("/", "/"),
            ("//", "/"),
            ("/.", "/"),
            ("/..", "/"),
            ("/../a", "/a"),
            ("a/.", "a/"),
            ("a/./", "a/"),
            ("a/..", "."),
            ("a/../", "."),
            ("a/b/..", "a/"),
            ("a/../..", ".."),
            ("a/../../", ".."),
            ("../a/..", ".."),
            ("foo/./bar/..", "foo/"),
            ("foo/.///bar/../", "foo/"),
            ("/home/user/.bitcoin//", "/home/user/.bitcoin/"),
            ("/root/../home/user/.bitcoin", "/home/user/.bitcoin"),
            ("somewhere/../user/.bitcoin", "user/.bitcoin"),
        ];
        for (input, expected) in cases {
            #[cfg(not(windows))]
            assert_eq!(lexically_normal(input), expected, "{input:?}");
            #[cfg(windows)]
            assert_eq!(lexically_normal(input), expected.replace('/', "\\"), "{input:?}");
        }
    }

    #[test]
    fn path_arg_drops_trailing_separator() {
        assert_eq!(normalize_path_arg("/"), PathBuf::from("/"));
        assert_eq!(normalize_path_arg("/.//"), PathBuf::from("/"));
        assert_eq!(normalize_path_arg("user/.bitcoin/./"), PathBuf::from("user/.bitcoin"));
    }

    #[test]
    fn equivalent_compares_file_identity() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dash.conf");
        std::fs::write(&file, "").unwrap();
        assert!(equivalent(&file, &dir.path().join(".").join("dash.conf")));
        assert!(!equivalent(&file, &dir.path().join("missing.conf")));
        // Hard links name the same file; a copy is another file.
        let link = dir.path().join("other.conf");
        std::fs::hard_link(&file, &link).unwrap();
        assert!(equivalent(&file, &link));
        let copy = dir.path().join("copy.conf");
        std::fs::write(&copy, "").unwrap();
        assert!(!equivalent(&file, &copy));
        assert!(equivalent(dir.path(), &dir.path().join(".")));
    }

    /// libstdc++'s Windows results, printed by gen/windows_paths_probe.cpp (MinGW-w64 g++ 13)
    /// under Wine: (path, root name, is_absolute).
    const WINDOWS_PATHS: &[(&str, &str, bool)] = &[
        ("C:", "C:", false),
        ("C:x", "C:", false),
        (r"C:\", "C:", true),
        (r"C:\x", "C:", true),
        ("C:/x", "C:", true),
        (r"\x", "", false),
        ("/x", "", false),
        (r"\\x", "", false),
        ("x", "", false),
        (r"\\server", "", false),
        (r"\\server\share", "", false),
        ("//server/share/x", "", false),
        (r"\\\x", "", false),
        (r"\\?\C:\x", "", false),
        (r"\\?\UNC\server\share", "", false),
        ("1:x", "1:", false),
        ("", "", false),
        (r"\\.\C:\x", "", false),
        ("/:x", "", false),
        (r"\:x", "", false),
    ];
    /// The same program's `base / path` results.
    const WINDOWS_JOINS: &[(&str, &str, &str)] = &[
        (r"C:\d", "x", r"C:\d\x"),
        (r"C:\d\", "x", r"C:\d\x"),
        (r"C:\d", "C:x", r"C:\d\x"),
        (r"C:\d", "D:x", "D:x"),
        (r"C:\d", r"\x", r"C:\x"),
        (r"C:\d", r"C:\x", r"C:\x"),
        (r"C:\d", r"D:\x", r"D:\x"),
        (r"C:\d", "c:x", "c:x"),
        ("C:", "x", "C:x"),
        (r"C:\", "x", r"C:\x"),
        ("C:/d", "x", r"C:/d\x"),
        (r"\\server\share", "x", r"\\server\share\x"),
        (r"\\server\share", r"\x", r"\x"),
        (r"\\server\share", "C:x", "C:x"),
        (r"\\server", "x", r"\\server\x"),
        (r"\\?\C:\d", "x", r"\\?\C:\d\x"),
        (r"\\?\C:\d", "C:x", "C:x"),
        (r"C:\d", r"\\?\C:\x", r"C:\\?\C:\x"),
        (r"C:\d", r"\\server\x", r"C:\\server\x"),
        (r"C:\d", "//server", "C://server"),
        ("", "x", "x"),
        (r"C:\d", r"x/y\z", r"C:\d\x/y\z"),
        (r"C:\d", "/x", "C:/x"),
        (r"C:\d", "C:", r"C:\d\"),
        (r"C:\d", r"\\\x", r"C:\\\x"),
        (r"C:\d", r"\:x", r"C:\:x"),
    ];

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn windows_root_names_and_absolute_match_libstdcxx() {
        for &(path, root_name, absolute) in WINDOWS_PATHS {
            let p = chars(path);
            assert_eq!(p[..win::root_name_len(&p)].iter().collect::<String>(), root_name, "{path}");
            assert_eq!(win::is_absolute(&p), absolute, "{path}");
        }
    }

    /// msvcrt's `_wstat` results, printed by gen/msvcrt_stat_probe.cpp under Wine 10 in
    /// `Z:\p\run` (current drive 25): (path, directory, read-only, st_mode, st_dev). Relative
    /// names of up to six characters are left out: Wine's msvcrt only checks the extension of
    /// longer paths, Windows' of any.
    const MSVCRT_STATS: &[(&str, bool, bool, u32, i32)] = &[
        (r"Z:\p\run\a.conf", false, false, 0o100666, 25),
        (r"Z:\p\run\a.exe", false, false, 0o100777, 25),
        (r"Z:\p\run\b.EXE", false, false, 0o100777, 25),
        (r"Z:\p\run\a.bat", false, false, 0o100777, 25),
        (r"Z:\p\run\a.cmd", false, false, 0o100777, 25),
        (r"Z:\p\run\a.com", false, false, 0o100777, 25),
        (r"Z:\p\run\a.exe.txt", false, false, 0o100666, 25),
        (r"Z:\p\run\a.exex", false, false, 0o100666, 25),
        (r"Z:\p\run\exe", false, false, 0o100666, 25),
        (r"Z:\p\run\ro.conf", false, true, 0o100444, 25),
        (r"x\..\a.exe", false, false, 0o100777, 25),
        ("a.exe.txt", false, false, 0o100666, 25),
        ("d", true, false, 0o40777, 25),
        ("d.exe", true, false, 0o40777, 25),
        (r"Z:\p\run\d", true, false, 0o40777, 25),
        (r"z:\p\run\a.conf", false, false, 0o100666, 25),
        (r"C:\windows", true, false, 0o40777, 2),
    ];

    #[test]
    fn msvcrt_stat_matches_msvcrt() {
        for &(path, is_dir, readonly, mode, dev) in MSVCRT_STATS {
            assert_eq!(win::msvcrt_stat(&chars(path), is_dir, readonly, 25), (mode, dev), "{path}");
        }
    }

    #[test]
    fn windows_joins_match_libstdcxx() {
        for &(base, path, joined) in WINDOWS_JOINS {
            let result: String = win::join(&chars(base), &chars(path)).into_iter().collect();
            assert_eq!(result, joined, "{base} / {path}");
        }
    }

    /// Drive-relative, root-relative, UNC and verbatim paths are all relative to libstdc++, so
    /// `absolute` hands them to `GetFullPathNameW` (less extra leading separators), which the
    /// probe shows resolves `Z:x` in the current directory, and `\\server\share` and
    /// `\\?\Z:\x` as `Z:\server\share` and `Z:\?\Z:\x` on the current drive.
    #[test]
    fn windows_absolute_resolves_relative_forms() {
        for (path, input) in [
            ("x", "x"),
            ("C:x", "C:x"),
            ("C:", "C:"),
            (r"\x", r"\x"),
            ("/x", "/x"),
            (r"\\x", r"\x"),
            (r"\\\x", r"\x"),
            (r"\\server\share", r"\server\share"),
            (r"\\?\C:\x", r"\?\C:\x"),
            ("//", "/"),
        ] {
            let p = chars(path);
            assert!(!win::is_absolute(&p), "{path}");
            assert_eq!(win::full_path_input(&p).iter().collect::<String>(), input, "{path}");
        }
    }

    /// The probe's `absolute()` results, run in `Z:\p\sub`, with this process's current
    /// directory (`{C}`), its parent (`{P}`) and drive (`{D}`) in their place.
    #[cfg(windows)]
    #[test]
    fn windows_absolute_matches_libstdcxx() {
        let cwd = std::env::current_dir().unwrap();
        let cwd = cwd.to_str().unwrap();
        let fill = |s: &str| {
            s.replace("{C}", cwd)
                .replace("{P}", &cwd[..cwd.rfind('\\').unwrap()])
                .replace("{D}", &cwd[..2])
        };
        for (path, expected) in [
            ("x", r"{C}\x"),
            ("{D}x", r"{C}\x"),
            (r"{D}..\x", r"{P}\x"),
            (r"\x", r"{D}\x"),
            ("/x", r"{D}\x"),
            (r"\\\x", r"{D}\x"),
            (r"\\server\share", r"{D}\server\share"),
            (r"\\?\{D}\x", r"{D}\?\{D}\x"),
            (r"{D}\x", r"{D}\x"),
            (".", "{C}"),
            ("x/", r"{C}\x\"),
        ] {
            let result = absolute(Path::new(&fill(path))).unwrap();
            assert_eq!(result, PathBuf::from(fill(expected)), "{path}");
        }
    }

    /// rdashd's default datadir sits beside Core's, under its own name (DEC-201 Q5).
    #[test]
    fn each_product_has_its_own_default_data_dir() {
        let core = default_data_dir(Product::DashCore, &mut |line| panic!("{line}"));
        let rust = default_data_dir(Product::RDashCore, &mut |line| panic!("{line}"));
        assert_eq!(core.parent(), rust.parent());
        let (named, dotted) = if cfg!(any(windows, target_os = "macos")) {
            ("DashCore", "RDashCore")
        } else {
            (".dashcore", ".rdashcore")
        };
        assert!(core.ends_with(named) && rust.ends_with(dotted), "{core:?} {rust:?}");
    }

    /// Core's datadir under the shell's application data folder, whatever `APPDATA` says.
    #[cfg(windows)]
    #[test]
    fn windows_default_data_dir_uses_the_shell_folder() {
        let expected = default_data_dir(Product::DashCore, &mut |line| panic!("{line}"));
        // SAFETY: no other test in this binary reads APPDATA.
        unsafe { std::env::remove_var("APPDATA") };
        let without = default_data_dir(Product::DashCore, &mut |line| panic!("{line}"));
        assert_eq!(without, expected);
        assert!(is_absolute(&without) && without.ends_with("DashCore"), "{}", without.display());
        assert!(without.parent().unwrap().is_dir());
    }

    // msvcrt text mode, which Core's streams use on Windows: reading ends at Ctrl-Z and turns
    // CR LF into LF (other CRs stay); writing turns each LF into CR LF.
    #[test]
    fn text_mode_transforms() {
        assert_eq!(text_mode_read(b"{}\x1a garbage"), b"{}");
        assert_eq!(text_mode_read(b"\x1a{}"), b"");
        assert_eq!(text_mode_read(b"a\r\nb\rc\r\r\n\r"), b"a\nb\rc\r\n\r");
        assert_eq!(text_mode_write("{\n    \"a\": 1\n}\n"), "{\r\n    \"a\": 1\r\n}\r\n");
    }

    // The libstdc++ texts below were printed by a g++ 15 program making the same calls.
    #[cfg(unix)]
    #[test]
    fn filesystem_errors_match_libstdcxx() {
        assert_eq!(
            absolute(Path::new("")).unwrap_err().0,
            "filesystem error: cannot make absolute path: Invalid argument []"
        );
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("blocks");
        std::fs::write(&blocks, "x").unwrap();
        assert_eq!(
            create_directories(&blocks).unwrap_err().0,
            format!(
                "filesystem error: cannot create directories: Not a directory [{}]",
                blocks.display()
            )
        );
        let sub = blocks.join("sub");
        assert_eq!(
            create_directories(&sub).unwrap_err().0,
            format!(
                "filesystem error: cannot create directories: Not a directory [{}]",
                sub.display()
            )
        );
        let full = dir.path().join("d");
        std::fs::create_dir_all(full.join("x")).unwrap();
        assert_eq!(
            remove(&full).unwrap_err().0,
            format!("filesystem error: cannot remove: Directory not empty [{}]", full.display())
        );
        let empty = dir.path().join("e");
        std::fs::create_dir(&empty).unwrap();
        assert!(remove(&empty).unwrap());
        assert!(!remove(&dir.path().join("none")).unwrap());
    }
}

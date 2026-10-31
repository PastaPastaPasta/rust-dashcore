//! RPC authentication (`httprpc.cpp`'s `InitRPCAuthentication` and `RPCAuthorized`,
//! `rpc/request.cpp`'s cookie functions): the `.cookie` secret, `-rpcuser`/`-rpcpassword`,
//! `-rpcauth` and the `-rpcwhitelist` sets.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bitcoin_hashes::{Hash as _, HashEngine as _, hmac, sha256};
use dash_args::ArgsManager;

/// The user name of cookie authentication.
pub const COOKIEAUTH_USER: &str = "__cookie__";
/// The cookie file's default name, in the network's data directory.
const COOKIEAUTH_FILE: &str = ".cookie";

/// Why authentication could not be set up (Core's `InitRPCAuthentication` returning false;
/// the failure is logged).
#[derive(Debug, PartialEq, Eq)]
pub struct AuthInitFailed;

/// The credentials and whitelists the server checks requests against.
#[derive(Debug, Default)]
pub struct RpcAuth {
    /// `user:password`: the cookie's or `-rpcuser`'s and `-rpcpassword`'s.
    user_colon_pass: Vec<u8>,
    /// `-rpcauth` entries: name, salt, HMAC.
    rpcauth: Vec<[String; 3]>,
    pub whitelist: BTreeMap<String, BTreeSet<String>>,
    pub whitelist_default: bool,
    /// The cookie this process wrote, deleted at shutdown. The only state that changes after
    /// init: requests read the rest without a lock.
    cookie: Mutex<Option<PathBuf>>,
}

impl RpcAuth {
    /// Core's `InitRPCAuthentication`.
    // Core: httprpc.cpp:247-317
    pub fn init(args: &ArgsManager) -> Result<RpcAuth, AuthInitFailed> {
        let mut auth = RpcAuth::default();
        let arg = |name: &str| args.get_arg(name, "").map_err(|_| AuthInitFailed);
        let password = arg("-rpcpassword")?;
        if password.is_empty() {
            tracing::info!("Using random cookie authentication.");
            let (cookie, path) = generate_auth_cookie(args).ok_or(AuthInitFailed)?;
            auth.user_colon_pass = cookie.into_bytes();
            auth.cookie = Mutex::new(Some(path));
        } else {
            tracing::info!(
                "Config options rpcuser and rpcpassword will soon be deprecated. Locally-run instances may remove rpcuser to use cookie-based auth, or may be replaced with rpcauth. Please see share/rpcauth for rpcauth auth generation."
            );
            auth.user_colon_pass = format!("{}:{password}", arg("-rpcuser")?).into_bytes();
        }
        // A failure from here on leaves no cookie behind.
        if let Err(e) = auth.read_users(args) {
            auth.delete_cookie();
            return Err(e);
        }
        Ok(auth)
    }

    /// The rest of `InitRPCAuthentication`: `-rpcauth`, then the whitelists.
    fn read_users(&mut self, args: &ArgsManager) -> Result<(), AuthInitFailed> {
        let auth = self;
        let arg = |name: &str| args.get_arg(name, "").map_err(|_| AuthInitFailed);
        // Only the last -rpcauth decides whether any is used.
        if !arg("-rpcauth")?.is_empty() {
            tracing::info!("Using rpcauth authentication.");
            for rpcauth in args.get_args("-rpcauth").map_err(|_| AuthInitFailed)? {
                let mut fields: Vec<&str> = rpcauth.split(':').collect();
                let salt_hmac: Vec<&str> =
                    fields.last().expect("split yields one").split('$').collect();
                if fields.len() == 2 && salt_hmac.len() == 2 {
                    fields.pop();
                    auth.rpcauth.push([fields[0].into(), salt_hmac[0].into(), salt_hmac[1].into()]);
                } else {
                    tracing::info!("Invalid -rpcauth argument.");
                    return Err(AuthInitFailed);
                }
            }
        }
        auth.whitelist_default = args
            .get_bool_arg("-rpcwhitelistdefault", args.is_arg_set("-rpcwhitelist"))
            .map_err(|_| AuthInitFailed)?;
        for entry in args.get_args("-rpcwhitelist").map_err(|_| AuthInitFailed)? {
            let (user, list) = match entry.split_once(':') {
                Some((user, list)) => (user, Some(list)),
                None => (entry.as_str(), None),
            };
            let intersect = auth.whitelist.contains_key(user);
            let whitelist = auth.whitelist.entry(user.to_owned()).or_default();
            if let Some(list) = list {
                // Core's SplitString on ", " splits at either character.
                let new: BTreeSet<String> = list.split([',', ' ']).map(str::to_owned).collect();
                *whitelist = if intersect {
                    new.intersection(whitelist).cloned().collect()
                } else {
                    new
                };
            }
        }
        Ok(())
    }

    /// Core's `RPCAuthorized`: the user name (whenever the credentials have a colon) and
    /// whether they are valid.
    // Core: httprpc.cpp:136-160
    pub fn authorized(&self, header: &str) -> (Option<String>, bool) {
        if self.user_colon_pass.is_empty() {
            return (None, false);
        }
        let Some(encoded) = header.as_bytes().strip_prefix(b"Basic ") else {
            return (None, false);
        };
        let Some(user_pass) = decode_base64(trim(encoded)) else {
            return (None, false);
        };
        let user = user_pass
            .iter()
            .position(|&b| b == b':')
            .map(|colon| String::from_utf8_lossy(&user_pass[..colon]).into_owned());
        let ok = timing_resistant_equal(&user_pass, &self.user_colon_pass)
            || self.multi_user_authorized(&user_pass);
        (user, ok)
    }

    /// Core's `multiUserAuthorized`: an `-rpcauth` entry whose HMAC of the password matches.
    // Core: httprpc.cpp:107-134
    fn multi_user_authorized(&self, user_pass: &[u8]) -> bool {
        let Some(colon) = user_pass.iter().position(|&b| b == b':') else {
            return false;
        };
        let (user, pass) = (&user_pass[..colon], &user_pass[colon + 1..]);
        self.rpcauth.iter().any(|[name, salt, hash]| {
            if !timing_resistant_equal(name.as_bytes(), user) {
                return false;
            }
            let mut engine = hmac::HmacEngine::<sha256::Hash>::new(salt.as_bytes());
            engine.input(pass);
            let mac = hmac::Hmac::<sha256::Hash>::from_engine(engine);
            let hex: String = mac.to_byte_array().iter().map(|b| format!("{b:02x}")).collect();
            timing_resistant_equal(hex.as_bytes(), hash.as_bytes())
        })
    }

    /// Core's `DeleteAuthCookie`: removes the cookie this process wrote, if any.
    // Core: rpc/request.cpp:131-140
    pub fn delete_cookie(&self) {
        let path = self.cookie.lock().expect("not poisoned").take();
        if let Some(path) = path
            && let Err(e) = fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::info!("DeleteAuthCookie: Unable to remove random auth cookie file: {e}");
        }
    }
}

/// `GetAuthCookieFile`: `-rpccookiefile` (default `.cookie`), `.tmp` appended for the file
/// written first, relative to the network's data directory.
// Core: rpc/request.cpp:73-81
fn auth_cookie_file(args: &ArgsManager, temp: bool) -> Option<PathBuf> {
    let mut arg =
        args.get_path_arg("-rpccookiefile", Path::new(COOKIEAUTH_FILE)).ok()?.into_os_string();
    if temp {
        arg.push(".tmp");
    }
    let arg = PathBuf::from(arg);
    if arg.is_absolute() {
        return Some(arg);
    }
    Some(dash_args::paths::abs_path_join(&args.get_data_dir_net().ok()?, &arg))
}

/// Core's `GenerateAuthCookie`: `__cookie__:<64 hex digits>`, written to a temporary file and
/// renamed over the cookie. The file mode comes from the process umask (077, as Core sets it).
// Core: rpc/request.cpp:85-115
fn generate_auth_cookie(args: &ArgsManager) -> Option<(String, PathBuf)> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).expect("the OS random source works");
    let cookie = format!(
        "{COOKIEAUTH_USER}:{}",
        secret.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let tmp = auth_cookie_file(args, true)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // Owner-only whatever the umask (rdashd sets 077, as dashd does; a library user may not).
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&tmp).and_then(|mut f| f.write_all(cookie.as_bytes()));
    if written.is_err() {
        tracing::info!("Unable to open cookie authentication file {} for writing", tmp.display());
        return None;
    }
    let path = auth_cookie_file(args, false)?;
    if fs::rename(&tmp, &path).is_err() {
        tracing::info!(
            "Unable to rename cookie authentication file {} to {}",
            tmp.display(),
            path.display()
        );
        return None;
    }
    tracing::info!("Generated RPC authentication cookie {}", path.display());
    Some((cookie, path))
}

/// Core's `TrimString` set: `" \f\n\r\t\v"`.
pub fn trim(s: &[u8]) -> &[u8] {
    const WS: &[u8] = b" \x0c\n\r\t\x0b";
    let start = s.iter().position(|b| !WS.contains(b)).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !WS.contains(b)).map_or(start, |i| i + 1);
    &s[start..end]
}

/// Core's `DecodeBase64`: a multiple of four characters, at most two `=` at the end, no
/// leftover bits.
// Core: util/strencodings.cpp DecodeBase64, ConvertBits<6, 8, false>
pub fn decode_base64(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut s = s;
    for _ in 0..2 {
        if let Some(rest) = s.strip_suffix(b"=") {
            s = rest;
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = ((acc << 6) | u32::from(v)) & 0x1fff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 6 || (acc << (8 - bits)) & 0xff != 0 {
        return None;
    }
    Some(out)
}

/// Core's `TimingResistantEqual`: compares every byte of `a` whatever differs.
pub fn timing_resistant_equal(a: &[u8], b: &[u8]) -> bool {
    if b.is_empty() {
        return a.is_empty();
    }
    let mut acc = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        acc |= usize::from(x ^ b[i % b.len()]);
    }
    acc == 0
}

#[cfg(test)]
mod tests;

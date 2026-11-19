use super::*;
use crate::testutil::test_args;

fn basic(user_pass: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = user_pass.as_bytes();
    let mut out = String::from("Basic ");
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Core's `rpc_users` vector: user `rt` with [`RT_PASSWORD`].
const RT_RPCAUTH: &str = "rt:93648e835a54c573682c2eb19f882535$7681e9c5b74bdd85e78166031d2058e1069b3ed7ed967c93fc63abba06f31144";
const RT_PASSWORD: &str = "cA773lm788buwYe4g4WT+05pKyNruVKjQ25x3n0DQcM=";

#[test]
fn a_cookie_is_written_and_removed() {
    let t = test_args(&[]);
    let auth = RpcAuth::init(&t.args).unwrap();
    let path = t.args.get_data_dir_net().unwrap().join(".cookie");
    let cookie = fs::read_to_string(&path).unwrap();
    let (user, secret) = cookie.split_once(':').unwrap();
    assert_eq!(user, "__cookie__");
    assert_eq!(secret.len(), 64);
    assert!(secret.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert!(!t.args.get_data_dir_net().unwrap().join(".cookie.tmp").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(auth.authorized(&basic(&cookie)), (Some("__cookie__".into()), true));
    assert_eq!(auth.authorized(&basic("__cookie__:x")), (Some("__cookie__".into()), false));
    auth.delete_cookie();
    assert!(!path.exists());
    // Twice is harmless.
    auth.delete_cookie();
}

#[test]
fn the_cookie_file_follows_rpccookiefile() {
    let t = test_args(&["-rpccookiefile=sub.cookie"]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert!(t.args.get_data_dir_net().unwrap().join("sub.cookie").exists());
    auth.delete_cookie();
    let absolute = t.dir.path().join("abs.cookie");
    let t2 = test_args(&[&format!("-rpccookiefile={}", absolute.display())]);
    let auth = RpcAuth::init(&t2.args).unwrap();
    assert!(absolute.exists());
    auth.delete_cookie();
    assert!(!absolute.exists());
    // A directory that does not exist: init fails.
    let t3 = test_args(&["-rpccookiefile=missing/dir/.cookie"]);
    assert_eq!(RpcAuth::init(&t3.args).unwrap_err(), AuthInitFailed);
}

#[test]
fn rpcuser_and_rpcpassword_replace_the_cookie() {
    let t = test_args(&["-rpcuser=alice", "-rpcpassword=secret"]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert!(!t.args.get_data_dir_net().unwrap().join(".cookie").exists());
    assert_eq!(auth.authorized(&basic("alice:secret")), (Some("alice".into()), true));
    assert_eq!(auth.authorized(&basic("alice:wrong")), (Some("alice".into()), false));
    assert_eq!(auth.authorized(&basic("alice")), (None, false));
    // The scheme is case-sensitive; the credentials are trimmed.
    let header = basic("alice:secret");
    assert_eq!(auth.authorized(&header.replace("Basic", "basic")), (None, false));
    assert_eq!(auth.authorized(&format!("{header} \t")), (Some("alice".into()), true));
    assert_eq!(auth.authorized("Basic !!!!"), (None, false));
}

#[test]
fn rpcauth_checks_the_hmac() {
    let t = test_args(&[&format!("-rpcauth={RT_RPCAUTH}"), "-rpcauth=bob:00$00"]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert_eq!(auth.authorized(&basic(&format!("rt:{RT_PASSWORD}"))), (Some("rt".into()), true));
    assert_eq!(auth.authorized(&basic(&format!("rt:{RT_PASSWORD}x"))), (Some("rt".into()), false));
    assert_eq!(auth.authorized(&basic(&format!("rT:{RT_PASSWORD}"))), (Some("rT".into()), false));
    assert_eq!(auth.authorized(&basic("bob:rt")), (Some("bob".into()), false));
    // The cookie still works beside rpcauth.
    let cookie = fs::read_to_string(t.args.get_data_dir_net().unwrap().join(".cookie")).unwrap();
    assert!(auth.authorized(&basic(&cookie)).1);
}

#[test]
fn a_malformed_rpcauth_fails_init_and_removes_the_cookie() {
    for bad in ["rt", "rt:nodollar", "rt:a$b$c", "a:b:c$d"] {
        let t = test_args(&[&format!("-rpcauth={RT_RPCAUTH}"), &format!("-rpcauth={bad}")]);
        assert_eq!(RpcAuth::init(&t.args).unwrap_err(), AuthInitFailed, "{bad}");
        assert!(!t.args.get_data_dir_net().unwrap().join(".cookie").exists(), "{bad}");
    }
    // Only the last -rpcauth decides whether any is read.
    let t = test_args(&["-rpcauth=bad", "-rpcauth="]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert!(auth.rpcauth.is_empty());
}

#[test]
fn whitelists_split_and_intersect() {
    let t = test_args(&[
        "-rpcwhitelist=a:getinfo,help stop",
        "-rpcwhitelist=a:help,stop,uptime",
        "-rpcwhitelist=b",
    ]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert!(auth.whitelist_default);
    assert_eq!(auth.whitelist["a"].iter().collect::<Vec<_>>(), ["help", "stop"]);
    assert!(auth.whitelist["b"].is_empty());
    let t = test_args(&["-rpcwhitelist=a:x,,y", "-rpcwhitelistdefault=0"]);
    let auth = RpcAuth::init(&t.args).unwrap();
    assert!(!auth.whitelist_default);
    assert_eq!(auth.whitelist["a"].iter().collect::<Vec<_>>(), ["", "x", "y"]);
    let t = test_args(&["-rpcwhitelistdefault"]);
    assert!(RpcAuth::init(&t.args).unwrap().whitelist_default);
    let t = test_args(&[]);
    assert!(!RpcAuth::init(&t.args).unwrap().whitelist_default);
}

#[test]
fn base64_decodes_strictly() {
    assert_eq!(decode_base64(b"YWJj").unwrap(), b"abc");
    assert_eq!(decode_base64(b"YWI=").unwrap(), b"ab");
    assert_eq!(decode_base64(b"YQ==").unwrap(), b"a");
    assert_eq!(decode_base64(b"").unwrap(), b"");
    for bad in [&b"YQ="[..], b"YQ", b"Y===", b"YR==", b"YWJ=", b"YW=j", b"Y Jj", b"YWJj\n"] {
        assert_eq!(decode_base64(bad), None, "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn timing_resistant_equal_compares_whole_strings() {
    assert!(timing_resistant_equal(b"", b""));
    assert!(timing_resistant_equal(b"abc", b"abc"));
    assert!(!timing_resistant_equal(b"abc", b"abcd"));
    assert!(!timing_resistant_equal(b"abcd", b"abc"));
    assert!(!timing_resistant_equal(b"abc", b""));
    assert!(!timing_resistant_equal(b"", b"abc"));
    assert!(!timing_resistant_equal(b"abcabc", b"abc"));
}

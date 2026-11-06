//! The numeric host forms Core's `WrappedGetAddrInfo(name, allow_lookup = false)` decodes: glibc's
//! `getaddrinfo` with `AI_NUMERICHOST` and `AF_UNSPEC`, which tries `inet_aton` (exact) and then
//! `inet_pton(AF_INET6)`. Core links the platform's resolver; these are the rules of the glibc it
//! runs on in Linux builds (verified against glibc 2.43).
//!
//! A scope id (`fe80::1%32`) is not modelled: a name holding `%` decodes to nothing (EV1-1 D36).

use crate::netaddr::NetAddr;

/// What glibc's numeric `getaddrinfo` returns for `name`, as Core's `CNetAddr` constructors read it
/// (an IPv6 result goes through `SetLegacyIPv6`, so `::ffff:a.b.c.d` is IPv4). `None` when glibc
/// reports `EAI_NONAME`. The internal-address filter of `LookupIntern` is the caller's.
// Core: netbase.cpp:44-95 WrappedGetAddrInfo; netaddress.cpp:299-303 CNetAddr(in6_addr, scope)
pub fn getaddrinfo_numeric(name: &str) -> Option<NetAddr> {
    let b = name.as_bytes();
    // A C string ends at its first NUL; Core refuses such a name before (`ContainsNoNUL`).
    if b.contains(&0) {
        return None;
    }
    if let Some(v4) = inet_aton_exact(b) {
        return Some(NetAddr::ipv4(v4));
    }
    if b.contains(&b'%') {
        return None;
    }
    inet_pton6(b).map(NetAddr::from_legacy_ipv6)
}

/// glibc's `__inet_aton_exact`: one to four parts separated by `.`, each a C integer literal as
/// `strtoul(…, 0)` reads it (`0x` hex, leading `0` octal, else decimal) that starts with a digit;
/// the last part fills the remaining bytes (`a`, `a.b` with b ≤ 0xffffff, `a.b.c` with c ≤ 0xffff,
/// `a.b.c.d`), and nothing may follow it.
// glibc: resolv/inet_addr.c inet_aton_end, __inet_aton_exact
fn inet_aton_exact(b: &[u8]) -> Option<[u8; 4]> {
    const MAX: [u32; 4] = [0xffff_ffff, 0x00ff_ffff, 0xffff, 0xff];
    let mut parts = [0u8; 4];
    let (mut n, mut i) = (0, 0);
    let last = loop {
        if !b.get(i).is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let (val, end) = strtoul0(b, i)?;
        i = end;
        if b.get(i) != Some(&b'.') {
            break val;
        }
        if n > 2 || val > 0xff {
            return None;
        }
        parts[n] = val as u8;
        n += 1;
        i += 1;
    };
    if i != b.len() || last > MAX[n] {
        return None;
    }
    Some((u32::from_be_bytes(parts) | last).to_be_bytes())
}

/// `strtoul(b + i, &end, 0)` for a part that starts with a digit: the value and the index after
/// the digits it read, or `None` for a value above 32 bits (glibc's `ul > 0xffffffff` and
/// `ERANGE` checks). `0x` with no hex digit after it reads as `0` and stops at the `x`.
fn strtoul0(b: &[u8], mut i: usize) -> Option<(u32, usize)> {
    let (radix, digits_from) = match (b.get(i), b.get(i + 1)) {
        (Some(b'0'), Some(b'x' | b'X')) if b.get(i + 2).is_some_and(u8::is_ascii_hexdigit) => {
            (16, i + 2)
        }
        (Some(b'0'), _) => (8, i),
        _ => (10, i),
    };
    i = digits_from;
    let mut val: u64 = 0;
    while let Some(d) = b.get(i).and_then(|&c| (c as char).to_digit(radix)) {
        val = val * u64::from(radix) + u64::from(d);
        if val > u64::from(u32::MAX) {
            return None;
        }
        i += 1;
    }
    Some((val as u32, i))
}

/// glibc's `inet_pton4` as `inet_pton6` calls it on an embedded IPv4 tail: exactly four decimal
/// octets of at most 255 with no leading zero.
// glibc: resolv/inet_pton.c inet_pton4
fn inet_pton4(b: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let (mut octets, mut saw_digit, mut k) = (0, false, 0);
    for &c in b {
        if c.is_ascii_digit() {
            if saw_digit && out[k] == 0 {
                return None;
            }
            let new = u32::from(out[k]) * 10 + u32::from(c - b'0');
            if new > 255 {
                return None;
            }
            out[k] = new as u8;
            if !saw_digit {
                octets += 1;
                if octets > 4 {
                    return None;
                }
                saw_digit = true;
            }
        } else if c == b'.' && saw_digit {
            if octets == 4 {
                return None;
            }
            k += 1;
            saw_digit = false;
        } else {
            return None;
        }
    }
    (octets == 4).then_some(out)
}

/// glibc's `inet_pton6`: hex groups of one to four digits, at most one `::`, an optional
/// dotted-quad tail in the last 32 bits, and exactly 16 bytes in all (`::` never stands for no
/// group).
// glibc: resolv/inet_pton.c inet_pton6
fn inet_pton6(src: &[u8]) -> Option<[u8; 16]> {
    let mut tmp = [0u8; 16];
    let (mut tp, mut colonp) = (0usize, None);
    let mut i = 0;
    if src.is_empty() {
        return None;
    }
    if src[0] == b':' {
        i = 1;
        if src.get(1) != Some(&b':') {
            return None;
        }
    }
    let mut curtok = i;
    let (mut xdigits, mut val) = (0, 0u32);
    while i < src.len() {
        let ch = src[i];
        i += 1;
        if let Some(d) = (ch as char).to_digit(16) {
            if xdigits == 4 {
                return None;
            }
            val = (val << 4) | d;
            xdigits += 1;
            continue;
        }
        if ch == b':' {
            curtok = i;
            if xdigits == 0 {
                if colonp.is_some() {
                    return None;
                }
                colonp = Some(tp);
                continue;
            } else if i == src.len() || tp + 2 > 16 {
                return None;
            }
            tmp[tp..tp + 2].copy_from_slice(&(val as u16).to_be_bytes());
            tp += 2;
            (xdigits, val) = (0, 0);
            continue;
        }
        if ch == b'.' && tp + 4 <= 16 {
            let v4 = inet_pton4(&src[curtok..])?;
            tmp[tp..tp + 4].copy_from_slice(&v4);
            tp += 4;
            xdigits = 0;
            break;
        }
        return None;
    }
    if xdigits > 0 {
        if tp + 2 > 16 {
            return None;
        }
        tmp[tp..tp + 2].copy_from_slice(&(val as u16).to_be_bytes());
        tp += 2;
    }
    if let Some(c) = colonp {
        if tp == 16 {
            return None;
        }
        let n = tp - c;
        tmp.copy_within(c..tp, 16 - n);
        tmp[c..16 - n].fill(0);
        tp = 16;
    }
    (tp == 16).then_some(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> Option<[u8; 4]> {
        inet_aton_exact(s.as_bytes())
    }

    /// Every row was checked against glibc 2.43's `getaddrinfo(AI_NUMERICHOST)` on agentbox.
    #[test]
    fn inet_aton_is_glibcs() {
        assert_eq!(v4("127.1"), Some([127, 0, 0, 1]));
        assert_eq!(v4("010.0.0.1"), Some([8, 0, 0, 1]));
        assert_eq!(v4("16909060"), Some([1, 2, 3, 4]));
        assert_eq!(v4("1.2.3.04"), Some([1, 2, 3, 4]));
        assert_eq!(v4("0x7f.1"), Some([127, 0, 0, 1]));
        assert_eq!(v4("4294967295"), Some([255; 4]));
        assert_eq!(v4("1.2.3"), Some([1, 2, 0, 3]));
        assert_eq!(v4("1.65535"), Some([1, 0, 255, 255]));
        assert_eq!(v4("0.0.0.0"), Some([0; 4]));
        for bad in [
            "1.2.3.4.",
            "1.2.3.256",
            "4294967296",
            "08.1.1.1",
            "1.2.3.4x",
            "1..2",
            "",
            ".",
            "0x",
            "1.2.3.4 ",
            "1.16777216",
            "1.2.65536",
            "+1.2.3.4",
            "1.2.3.4.5",
            "99999999999999999999",
        ] {
            assert_eq!(v4(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn inet_pton6_is_glibcs() {
        let ok = |s: &str| inet_pton6(s.as_bytes()).unwrap_or_else(|| panic!("{s:?}"));
        assert_eq!(ok("::"), [0; 16]);
        assert_eq!(ok("::1")[15], 1);
        assert_eq!(ok("1:2:3:4:5:6:7::")[12..], [0, 7, 0, 0]);
        assert_eq!(ok("::ffff:1.2.3.4")[10..], [0xff, 0xff, 1, 2, 3, 4]);
        assert_eq!(ok("::1.2.3.4")[12..], [1, 2, 3, 4]);
        assert_eq!(ok("::FFFF:1.2.3.4")[10..12], [0xff, 0xff]);
        for bad in [
            "1::2::3",
            "[::1]",
            "00000::1",
            "0:0:0:0:0:0:0:0:0",
            ":::",
            ":1::",
            "1:",
            "1::2:",
            "::ffff:01.2.3.4",
            "1:2:3:4:5:6:7:8::",
            "::1:2:3:4:5:6:7:8",
            "1.2.3.4",
            "::1.2.3",
            "1:2:3:4:5:6:1.2.3.4:5",
            "",
        ] {
            assert_eq!(inet_pton6(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn getaddrinfo_reclassifies_like_core() {
        assert_eq!(getaddrinfo_numeric("::ffff:1.2.3.4"), Some(NetAddr::ipv4([1, 2, 3, 4])));
        assert!(getaddrinfo_numeric("fe80::1%1").is_none());
        assert!(getaddrinfo_numeric("fd6b:88c0:8724:1:2:3:4:5").unwrap().is_internal());
    }
}

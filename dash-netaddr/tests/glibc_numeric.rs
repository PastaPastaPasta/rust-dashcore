//! `getaddrinfo_numeric` against the host's glibc, which is what Core's `WrappedGetAddrInfo` calls
//! with `AI_NUMERICHOST` (EV1-1 D35). Random strings over the characters NetInfo's filters admit
//! (`SAFE_CHARS_IPV4_6` plus `x`) and a few glibc treats specially (upper-case `0X`, signs,
//! whitespace), weighted towards address shapes, and random IPv6-shaped strings. `%` (a scope
//! id, D36) is left out: neither filter admits it.

#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::ffi::CString;
use std::net::{Ipv4Addr, Ipv6Addr};

use dash_netaddr::{NetAddr, getaddrinfo_numeric};

/// glibc's first result for `name` as Core turns it into a `CNetAddr`.
fn glibc(name: &str) -> Option<NetAddr> {
    let c = CString::new(name).ok()?;
    let hints = libc::addrinfo {
        ai_flags: libc::AI_NUMERICHOST,
        ai_family: libc::AF_UNSPEC,
        ai_socktype: libc::SOCK_STREAM,
        ai_protocol: libc::IPPROTO_TCP,
        ai_addrlen: 0,
        ai_addr: std::ptr::null_mut(),
        ai_canonname: std::ptr::null_mut(),
        ai_next: std::ptr::null_mut(),
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: valid C string and hints; the result list is freed below.
    if unsafe { libc::getaddrinfo(c.as_ptr(), std::ptr::null(), &hints, &mut res) } != 0 {
        return None;
    }
    // SAFETY: getaddrinfo succeeded, so `res` points at a list whose addresses match their family.
    let out = unsafe {
        let ai = &*res;
        match ai.ai_family {
            libc::AF_INET => {
                let sa = &*(ai.ai_addr as *const libc::sockaddr_in);
                Some(NetAddr::ipv4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr)).octets()))
            }
            libc::AF_INET6 => {
                let sa = &*(ai.ai_addr as *const libc::sockaddr_in6);
                Some(NetAddr::from_ip(Ipv6Addr::from(sa.sin6_addr.s6_addr).into()))
            }
            _ => None,
        }
    };
    // SAFETY: `res` came from getaddrinfo.
    unsafe { libc::freeaddrinfo(res) };
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

#[test]
fn numeric_lookup_matches_glibc() {
    const ATOMS: &[&str] = &[
        "0",
        "1",
        "7",
        "8",
        "9",
        "00",
        "01",
        "010",
        "08",
        "255",
        "256",
        "4294967295",
        "4294967296",
        "16777215",
        "65535",
        "65536",
        "0x",
        "0x7f",
        "0xff",
        "0xfffffffff",
        "ffff",
        "FFFF",
        "0X",
        "0XFF",
        "0xFFFFFFFF",
        "0Xdead",
        "::ffff:",
        "A",
        "F",
        "+",
        "-",
        " ",
        "\t",
        "12345",
        "a",
        "fc00",
        "db8",
        ".",
        ".",
        ".",
        ":",
        ":",
        "::",
        "::",
        "[",
        "]",
        "x",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut agreed = (0, 0);
    for _ in 0..200_000 {
        let n = 1 + rng.next() % 12;
        let s: String = (0..n).map(|_| rng.pick(ATOMS)).collect();
        let ours = getaddrinfo_numeric(&s);
        assert_eq!(ours, glibc(&s), "{s:?}");
        if ours.is_some() {
            agreed.0 += 1
        } else {
            agreed.1 += 1
        }
    }
    // Both outcomes must be well represented for the comparison to mean something.
    assert!(agreed.0 > 5_000 && agreed.1 > 5_000, "{agreed:?}");
}

/// IPv6 shapes: 1-9 groups of 0-5 hex digits of either case, a `::` at a random place, sometimes
/// a dotted IPv4 tail.
#[test]
fn ipv6_shapes_match_glibc() {
    const HEX: &[u8] = b"0123456789abcdefABCDEF";
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut agreed = (0, 0);
    for _ in 0..200_000 {
        let groups = 1 + rng.next() % 9;
        let gap = rng.next() % (groups + 4);
        let mut s = String::new();
        for g in 0..groups {
            if g == gap {
                s.push_str("::");
            } else if g > 0 {
                s.push(':');
            }
            for _ in 0..rng.next() % 6 {
                s.push(HEX[(rng.next() % HEX.len() as u64) as usize] as char);
            }
        }
        if gap == groups {
            s.push_str("::");
        }
        if rng.next().is_multiple_of(4) {
            let ip: Vec<String> = (0..4).map(|_| (rng.next() % 300).to_string()).collect();
            s.push(':');
            s.push_str(&ip.join("."));
        }
        let ours = getaddrinfo_numeric(&s);
        assert_eq!(ours, glibc(&s), "{s:?}");
        if ours.is_some() {
            agreed.0 += 1
        } else {
            agreed.1 += 1
        }
    }
    assert!(agreed.0 > 2_000 && agreed.1 > 2_000, "{agreed:?}");
}

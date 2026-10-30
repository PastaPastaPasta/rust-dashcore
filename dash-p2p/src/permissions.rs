//! `-whitebind` and `-whitelist` as Core parses them (`net_permissions.cpp`, over
//! [`crate::lookup`]'s numeric lookups) and `NetPermissions::ToStrings` (`getpeerinfo`'s
//! `permissions`). P4a D3.
//!
//! Errors are Core's untranslated texts; the host prefixes `Error: ` as `InitError` does.

use crate::conn::Permissions;
use crate::lookup::{lookup, lookup_subnet};
use crate::netaddr::{Service, SubNet};

/// Core's `NetPermissionFlags::All`.
// Core: net_permissions.h:40
pub const ALL: Permissions = Permissions(
    Permissions::BLOOM_FILTER.0
        | Permissions::FORCE_RELAY.0
        | Permissions::RELAY.0
        | Permissions::NO_BAN.0
        | Permissions::MEMPOOL.0
        | Permissions::DOWNLOAD.0
        | Permissions::ADDR.0,
);

/// Core's `TryParsePermissionFlags`: the flags before an `@` and the offset of what follows it,
/// or `Implicit` and offset 0 when there is no `@`.
// Core: net_permissions.cpp:24-67
fn parse_flags(s: &str) -> Result<(Permissions, usize), String> {
    let Some(at) = s.find('@') else {
        return Ok((Permissions::IMPLICIT, 0));
    };
    let mut flags = 0;
    // Core splits on ',' and allows empty entries.
    for permission in s[..at].split(',') {
        flags |= match permission {
            "bloomfilter" | "bloom" => Permissions::BLOOM_FILTER.0,
            "noban" => Permissions::NO_BAN.0,
            "forcerelay" => Permissions::FORCE_RELAY.0,
            "mempool" => Permissions::MEMPOOL.0,
            "download" => Permissions::DOWNLOAD.0,
            "all" => ALL.0,
            "relay" => Permissions::RELAY.0,
            "addr" => Permissions::ADDR.0,
            "" => 0,
            other => return Err(format!("Invalid P2P permission: '{other}'")),
        };
    }
    Ok((Permissions(flags), at + 1))
}

/// Core's `NetWhitebindPermissions::TryParse`: `[permissions@]addr:port`, the address numeric
/// (`Lookup(str, 0, false)`) and the port required.
// Core: net_permissions.cpp:84-105
pub fn parse_whitebind(s: &str) -> Result<(Service, Permissions), String> {
    let (flags, offset) = parse_flags(s)?;
    let bind = &s[offset..];
    let service =
        lookup(bind).ok_or_else(|| format!("Cannot resolve -whitebind address: '{bind}'"))?;
    if service.port == 0 {
        return Err(format!("Need to specify a port with -whitebind: '{bind}'"));
    }
    Ok((service, flags))
}

/// Core's `NetWhitelistPermissions::TryParse`: `[permissions@]subnet`, read by
/// [`lookup_subnet`].
// Core: net_permissions.cpp:107-124
pub fn parse_whitelist(s: &str, cjdns_reachable: bool) -> Result<(SubNet, Permissions), String> {
    let (flags, offset) = parse_flags(s)?;
    let net = &s[offset..];
    let subnet = lookup_subnet(net, cjdns_reachable)
        .ok_or_else(|| format!("Invalid netmask specified in -whitelist: '{net}'"))?;
    Ok((subnet, flags))
}

/// Core's `NetPermissions::ToStrings`, in Core's order (`Implicit` has no name).
// Core: net_permissions.cpp:71-82
pub fn permission_strings(flags: Permissions) -> Vec<&'static str> {
    [
        (Permissions::BLOOM_FILTER, "bloomfilter"),
        (Permissions::NO_BAN, "noban"),
        (Permissions::FORCE_RELAY, "forcerelay"),
        (Permissions::RELAY, "relay"),
        (Permissions::MEMPOOL, "mempool"),
        (Permissions::DOWNLOAD, "download"),
        (Permissions::ADDR, "addr"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.has(*flag))
    .map(|(_, name)| name)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core's `netpermissions_test` (`test/netbase_tests.cpp:497-597`), case by case.
    #[test]
    fn netpermissions_test() {
        use Permissions as P;
        let bind = |s| parse_whitebind(s).map(|(_, f)| f);
        let or = |fs: &[P]| P(fs.iter().fold(0, |a, f| a | f.0));

        // Detect invalid white bind.
        assert!(bind("").unwrap_err().contains("Cannot resolve -whitebind address"));
        assert!(bind("127.0.0.1").unwrap_err().contains("Need to specify a port with -whitebind"));
        assert!(bind("").is_err());

        // If no permission flags, assume backward compatibility.
        assert_eq!(bind("1.2.3.4:32"), Ok(P::IMPLICIT));
        assert!(bind("1.2.3.4:32").unwrap().has(P::IMPLICIT));

        // Can set one permission.
        assert_eq!(bind("bloom@1.2.3.4:32"), Ok(P::BLOOM_FILTER));
        assert_eq!(bind("@1.2.3.4:32"), Ok(P::NONE));

        // "noban" implies "download"; "noban,download" and "download,noban" are "noban".
        let noban = bind("noban@1.2.3.4:32").unwrap();
        assert_eq!(noban, P::NO_BAN);
        assert!(noban.has(P::DOWNLOAD) && noban.has(P::NO_BAN));
        assert_eq!(bind("noban,download@1.2.3.4:32"), Ok(noban));
        assert_eq!(bind("download,noban@1.2.3.4:32"), Ok(noban));
        // "download" excludes (does not imply) "noban".
        let download = bind("download@1.2.3.4:32").unwrap();
        assert_eq!(download, P::DOWNLOAD);
        assert!(download.has(P::DOWNLOAD) && !download.has(P::NO_BAN));

        // Happy path, can parse flags; forcerelay also activates relay.
        assert_eq!(
            bind("bloom,forcerelay@1.2.3.4:32"),
            Ok(or(&[P::BLOOM_FILTER, P::FORCE_RELAY, P::RELAY]))
        );
        assert_eq!(
            bind("bloom,relay,noban@1.2.3.4:32"),
            Ok(or(&[P::BLOOM_FILTER, P::RELAY, P::NO_BAN]))
        );
        assert!(bind("bloom,forcerelay,noban@1.2.3.4:32").is_ok());
        assert_eq!(bind("all@1.2.3.4:32"), Ok(ALL));

        // Allow dups ("noban" implies "download").
        assert_eq!(
            bind("bloom,relay,noban,noban@1.2.3.4:32"),
            Ok(or(&[P::BLOOM_FILTER, P::RELAY, P::NO_BAN, P::DOWNLOAD]))
        );

        // Allow empty.
        assert_eq!(
            bind("bloom,relay,,noban@1.2.3.4:32"),
            Ok(or(&[P::BLOOM_FILTER, P::RELAY, P::NO_BAN]))
        );
        assert_eq!(bind(",@1.2.3.4:32"), Ok(P::NONE));
        assert_eq!(bind(",,@1.2.3.4:32"), Ok(P::NONE));

        // Detect invalid flag.
        assert!(
            bind("bloom,forcerelay,oopsie@1.2.3.4:32")
                .unwrap_err()
                .contains("Invalid P2P permission")
        );

        // Check netmask error.
        let e = parse_whitelist("bloom,forcerelay,noban@1.2.3.4:32", false).unwrap_err();
        assert!(e.contains("Invalid netmask specified in -whitelist"), "{e}");

        // Happy path for whitelist parsing.
        let (_, flags) = parse_whitelist("noban@1.2.3.4", false).unwrap();
        assert_eq!(flags, P::NO_BAN);
        assert!(flags.has(P::NO_BAN));
        let (subnet, flags) =
            parse_whitelist("bloom,forcerelay,noban,relay@1.2.3.4/32", false).unwrap();
        assert_eq!(flags, or(&[P::BLOOM_FILTER, P::FORCE_RELAY, P::NO_BAN, P::RELAY]));
        assert_eq!(subnet.to_string(), "1.2.3.4/32");
        assert!(parse_whitelist("bloom,forcerelay,noban,relay,mempool@1.2.3.4/32", false).is_ok());

        // ToStrings: all seven names (and, beyond Core's test, in Core's order).
        assert_eq!(
            permission_strings(ALL),
            ["bloomfilter", "noban", "forcerelay", "relay", "mempool", "download", "addr"]
        );
    }

    /// Core's exact texts (`net_permissions.cpp:57`, `:97`, `:116`; `util/error.cpp:49`).
    #[test]
    fn errors_are_cores() {
        assert_eq!(parse_whitebind("").unwrap_err(), "Cannot resolve -whitebind address: ''");
        assert_eq!(
            parse_whitebind("bloom,oopsie@1.2.3.4:32").unwrap_err(),
            "Invalid P2P permission: 'oopsie'"
        );
        assert_eq!(
            parse_whitebind("1.2.3.4").unwrap_err(),
            "Need to specify a port with -whitebind: '1.2.3.4'"
        );
        assert_eq!(
            parse_whitelist("1.2.3.4:32", false).unwrap_err(),
            "Invalid netmask specified in -whitelist: '1.2.3.4:32'"
        );
    }

    /// `-whitebind` and `-whitelist` take what `getaddrinfo(AI_NUMERICHOST)` takes, with Core's
    /// texts for what it refuses (Sol P4a r1 P2; D20).
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn numeric_forms_are_getaddrinfos() {
        let (s, flags) = parse_whitebind("noban@127.1:9999").unwrap();
        assert_eq!(
            (s.addr.to_string(), s.port, flags),
            ("127.0.0.1".to_owned(), 9999, Permissions::NO_BAN)
        );
        let (s, _) = parse_whitebind("0x7f.1:9999").unwrap();
        assert_eq!(s.addr.to_string(), "127.0.0.1");
        let (net, _) = parse_whitelist("11.1", false).unwrap();
        assert_eq!(net.to_string(), "11.0.0.1/32");
        let (net, _) = parse_whitelist("mempool@011.1/24", false).unwrap();
        assert_eq!(net.to_string(), "9.0.0.0/24");
        assert_eq!(
            parse_whitebind("noban@1.2.3.4.:9999").unwrap_err(),
            "Cannot resolve -whitebind address: '1.2.3.4.:9999'"
        );
        assert_eq!(
            parse_whitelist("noban@127.0.0.256", false).unwrap_err(),
            "Invalid netmask specified in -whitelist: '127.0.0.256'"
        );
    }

    #[test]
    fn whitebind_reads_brackets_and_names_as_core() {
        let (s, _) = parse_whitebind("noban@[::1]:9999").unwrap();
        assert_eq!((s.addr.to_string(), s.port), ("::1".to_owned(), 9999));
        assert!(parse_whitebind("noban@localhost:9999").is_err());
        assert!(parse_whitebind("noban@1.2.3.4:0").unwrap_err().contains("Need to specify"));
    }
}

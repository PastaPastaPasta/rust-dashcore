//! One SPV disposition for each command in the shared decode policy.

use dash_p2p_core::CommandString;

use super::MessageType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpvRoute {
    Handshake,
    Ping,
    Pong,
    Addr,
    /// The request ledger sees it before subscribers of `MessageType::NotFound`.
    NotFound,
    Subscribed(MessageType),
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub cmd: CommandString,
    pub route: SpvRoute,
}

impl Row {
    const fn new(cmd: &'static str, route: SpvRoute) -> Self {
        Self {
            cmd: CommandString::from_static(cmd),
            route,
        }
    }
}

/// Command order matches `dash_p2p::policy::table()`.
pub const ROWS: [Row; 74] = [
    Row::new("version", SpvRoute::Handshake),
    Row::new("verack", SpvRoute::Handshake),
    Row::new("addr", SpvRoute::Addr),
    Row::new("addrv2", SpvRoute::Addr),
    Row::new("sendaddrv2", SpvRoute::Handshake),
    Row::new("inv", SpvRoute::Subscribed(MessageType::Inv)),
    Row::new("getdata", SpvRoute::Ignore),
    Row::new("merkleblock", SpvRoute::Subscribed(MessageType::MerkleBlock)),
    Row::new("getblocks", SpvRoute::Ignore),
    Row::new("getheaders", SpvRoute::Ignore),
    Row::new("tx", SpvRoute::Subscribed(MessageType::Tx)),
    Row::new("headers", SpvRoute::Subscribed(MessageType::Headers)),
    Row::new("block", SpvRoute::Subscribed(MessageType::Block)),
    Row::new("getaddr", SpvRoute::Ignore),
    Row::new("mempool", SpvRoute::Ignore),
    Row::new("ping", SpvRoute::Ping),
    Row::new("pong", SpvRoute::Pong),
    Row::new("notfound", SpvRoute::NotFound),
    Row::new("filterload", SpvRoute::Ignore),
    Row::new("filteradd", SpvRoute::Ignore),
    Row::new("filterclear", SpvRoute::Ignore),
    Row::new("sendheaders", SpvRoute::Ignore),
    Row::new("sendcmpct", SpvRoute::Ignore),
    Row::new("cmpctblock", SpvRoute::Ignore),
    Row::new("getblocktxn", SpvRoute::Ignore),
    Row::new("blocktxn", SpvRoute::Ignore),
    Row::new("getcfilters", SpvRoute::Ignore),
    Row::new("cfilter", SpvRoute::Subscribed(MessageType::CFilter)),
    Row::new("getcfheaders", SpvRoute::Ignore),
    Row::new("cfheaders", SpvRoute::Subscribed(MessageType::CfHeaders)),
    Row::new("getcfcheckpt", SpvRoute::Ignore),
    Row::new("cfcheckpt", SpvRoute::Ignore),
    Row::new("sendtxrcncl", SpvRoute::Ignore),
    Row::new("spork", SpvRoute::Ignore),
    Row::new("getsporks", SpvRoute::Ignore),
    Row::new("senddsq", SpvRoute::Ignore),
    Row::new("dsa", SpvRoute::Ignore),
    Row::new("dsi", SpvRoute::Ignore),
    Row::new("dsf", SpvRoute::Ignore),
    Row::new("dss", SpvRoute::Ignore),
    Row::new("dsc", SpvRoute::Ignore),
    Row::new("dssu", SpvRoute::Ignore),
    Row::new("dstx", SpvRoute::Ignore),
    Row::new("dsq", SpvRoute::Ignore),
    Row::new("ssc", SpvRoute::Ignore),
    Row::new("govsync", SpvRoute::Ignore),
    Row::new("govobj", SpvRoute::Ignore),
    Row::new("govobjvote", SpvRoute::Ignore),
    Row::new("getmnlistd", SpvRoute::Ignore),
    Row::new("mnlistdiff", SpvRoute::Subscribed(MessageType::MnListDiff)),
    Row::new("qsendrecsigs", SpvRoute::Ignore),
    Row::new("qfcommit", SpvRoute::Ignore),
    Row::new("qcontrib", SpvRoute::Ignore),
    Row::new("qcomplaint", SpvRoute::Ignore),
    Row::new("qjustify", SpvRoute::Ignore),
    Row::new("qpcommit", SpvRoute::Ignore),
    Row::new("qwatch", SpvRoute::Ignore),
    Row::new("qsigsesann", SpvRoute::Ignore),
    Row::new("qsigsinv", SpvRoute::Ignore),
    Row::new("qgetsigs", SpvRoute::Ignore),
    Row::new("qbsigs", SpvRoute::Ignore),
    Row::new("qsigrec", SpvRoute::Ignore),
    Row::new("qsigshare", SpvRoute::Ignore),
    Row::new("qgetdata", SpvRoute::Ignore),
    Row::new("qdata", SpvRoute::Ignore),
    Row::new("clsig", SpvRoute::Subscribed(MessageType::ChainLock)),
    Row::new("isdlock", SpvRoute::Subscribed(MessageType::IsDLock)),
    Row::new("mnauth", SpvRoute::Ignore),
    Row::new("getheaders2", SpvRoute::Ignore),
    Row::new("sendheaders2", SpvRoute::Ignore),
    Row::new("headers2", SpvRoute::Subscribed(MessageType::Headers)),
    Row::new("getqrinfo", SpvRoute::Ignore),
    Row::new("qrinfo", SpvRoute::Subscribed(MessageType::QrInfo)),
    Row::new("platformban", SpvRoute::Ignore),
];

pub fn lookup(cmd: &CommandString) -> Option<&'static Row> {
    ROWS.iter().find(|row| row.cmd == *cmd)
}

/// Tracks which ignored commands have already been logged by one router.
pub struct Registry {
    ignored: [bool; ROWS.len()],
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            ignored: [false; ROWS.len()],
        }
    }
}

impl Registry {
    pub fn route(&mut self, cmd: &CommandString) -> Option<SpvRoute> {
        let index = ROWS.iter().position(|row| row.cmd == *cmd)?;
        let route = ROWS[index].route;
        if route == SpvRoute::Ignore && !self.ignored[index] {
            self.ignored[index] = true;
            tracing::debug!(command = cmd.as_str(), "Ignoring P2P command");
        }
        Some(route)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use dash_p2p::policy;

    use super::*;

    #[test]
    fn one_row_per_command() {
        assert_eq!(ROWS.len(), policy::table().len());
        let mut seen = HashSet::new();
        for row in &ROWS {
            assert!(seen.insert(row.cmd));
            assert!(policy::lookup(&row.cmd).is_some());
            assert_eq!(lookup(&row.cmd), Some(row));
        }
        for row in policy::table() {
            assert!(seen.contains(&row.cmd));
        }
        assert!(lookup(&CommandString::from_static("unknown")).is_none());
    }

    #[test]
    fn rows_in_policy_order() {
        assert_eq!(
            ROWS.iter().map(|row| row.cmd).collect::<Vec<_>>(),
            policy::table().iter().map(|row| row.cmd).collect::<Vec<_>>()
        );
    }

    #[test]
    fn subscribed_kinds_have_rows() {
        for kind in MessageType::ALL {
            assert!(
                ROWS.iter().any(|row| {
                    row.route == SpvRoute::Subscribed(kind)
                        || (kind == MessageType::NotFound && row.route == SpvRoute::NotFound)
                }),
                "{kind:?}"
            );
            assert_eq!(MessageType::from_cmd(kind.cmd()), Some(kind));
        }
        for row in &ROWS {
            if let SpvRoute::Subscribed(kind) = row.route {
                assert!(MessageType::ALL.contains(&kind));
            }
        }
    }

    #[test]
    fn routes_match_the_spv_handlers() {
        let mut registry = Registry::default();
        for row in &ROWS {
            let expected = match row.cmd.as_str() {
                "version" | "verack" | "sendaddrv2" => SpvRoute::Handshake,
                "ping" => SpvRoute::Ping,
                "pong" => SpvRoute::Pong,
                "addr" | "addrv2" => SpvRoute::Addr,
                "notfound" => SpvRoute::NotFound,
                "headers2" => SpvRoute::Subscribed(MessageType::Headers),
                other => MessageType::from_cmd(other)
                    .map(SpvRoute::Subscribed)
                    .unwrap_or(SpvRoute::Ignore),
            };
            assert_eq!(registry.route(&row.cmd), Some(expected), "{}", row.cmd.as_str());
            assert_eq!(registry.route(&row.cmd), Some(expected));
        }
        assert_eq!(registry.route(&CommandString::from_static("unknown")), None);
    }
}

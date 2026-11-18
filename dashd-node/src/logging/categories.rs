//! Core's log categories and severity levels (`logging.h:36-104`, `logging.cpp:150-385`).

use std::fmt;

/// Core's `BCLog::LogFlags`: a category bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogFlags(pub u64);

impl LogFlags {
    pub const NONE: LogFlags = LogFlags(0);
    pub const NET: LogFlags = LogFlags(1 << 0);
    pub const TOR: LogFlags = LogFlags(1 << 1);
    pub const MEMPOOL: LogFlags = LogFlags(1 << 2);
    pub const HTTP: LogFlags = LogFlags(1 << 3);
    pub const BENCHMARK: LogFlags = LogFlags(1 << 4);
    pub const ZMQ: LogFlags = LogFlags(1 << 5);
    pub const WALLETDB: LogFlags = LogFlags(1 << 6);
    pub const RPC: LogFlags = LogFlags(1 << 7);
    pub const ESTIMATEFEE: LogFlags = LogFlags(1 << 8);
    pub const ADDRMAN: LogFlags = LogFlags(1 << 9);
    pub const SELECTCOINS: LogFlags = LogFlags(1 << 10);
    pub const REINDEX: LogFlags = LogFlags(1 << 11);
    pub const CMPCTBLOCK: LogFlags = LogFlags(1 << 12);
    pub const RANDOM: LogFlags = LogFlags(1 << 13);
    pub const PRUNE: LogFlags = LogFlags(1 << 14);
    pub const PROXY: LogFlags = LogFlags(1 << 15);
    pub const MEMPOOLREJ: LogFlags = LogFlags(1 << 16);
    pub const LIBEVENT: LogFlags = LogFlags(1 << 17);
    pub const COINDB: LogFlags = LogFlags(1 << 18);
    pub const QT: LogFlags = LogFlags(1 << 19);
    pub const LEVELDB: LogFlags = LogFlags(1 << 20);
    pub const VALIDATION: LogFlags = LogFlags(1 << 21);
    pub const I2P: LogFlags = LogFlags(1 << 22);
    pub const IPC: LogFlags = LogFlags(1 << 23);
    // LOCK (1 << 24) exists only in DEBUG_LOCKCONTENTION builds, which releases are not.
    pub const BLOCKSTORAGE: LogFlags = LogFlags(1 << 26);
    pub const TXRECONCILIATION: LogFlags = LogFlags(1 << 27);
    /// WAL-29: v24's `scan` category.
    pub const SCAN: LogFlags = LogFlags(1 << 28);
    pub const CHAINLOCKS: LogFlags = LogFlags(1 << 32);
    pub const GOBJECT: LogFlags = LogFlags(1 << 33);
    pub const INSTANTSEND: LogFlags = LogFlags(1 << 34);
    pub const LLMQ: LogFlags = LogFlags(1 << 36);
    pub const LLMQ_DKG: LogFlags = LogFlags(1 << 37);
    pub const LLMQ_SIGS: LogFlags = LogFlags(1 << 38);
    pub const MNPAYMENTS: LogFlags = LogFlags(1 << 39);
    pub const MNSYNC: LogFlags = LogFlags(1 << 40);
    pub const COINJOIN: LogFlags = LogFlags(1 << 41);
    pub const SPORK: LogFlags = LogFlags(1 << 42);
    pub const NETCONN: LogFlags = LogFlags(1 << 43);
    pub const EHF: LogFlags = LogFlags(1 << 44);
    pub const CREDITPOOL: LogFlags = LogFlags(1 << 45);
    pub const DASH: LogFlags = LogFlags(
        Self::CHAINLOCKS.0
            | Self::GOBJECT.0
            | Self::INSTANTSEND.0
            | Self::LLMQ.0
            | Self::LLMQ_DKG.0
            | Self::LLMQ_SIGS.0
            | Self::MNPAYMENTS.0
            | Self::MNSYNC.0
            | Self::COINJOIN.0
            | Self::SPORK.0
            | Self::NETCONN.0
            | Self::EHF.0
            | Self::CREDITPOOL.0,
    );
    /// Logged in `net` and in `netconn`.
    pub const NET_NETCONN: LogFlags = LogFlags(Self::NET.0 | Self::NETCONN.0);
    pub const ALL: LogFlags = LogFlags(!0);
}

/// Core's `LogCategories[]`, in Core's order: the names `-debug`, `-debugexclude`, `-loglevel`
/// and the `logging` RPC accept.
// Core: logging.cpp:155-201
const LOG_CATEGORIES: &[(LogFlags, &str)] = &[
    (LogFlags::NONE, "0"),
    (LogFlags::NONE, ""),
    (LogFlags::NET, "net"),
    (LogFlags::TOR, "tor"),
    (LogFlags::MEMPOOL, "mempool"),
    (LogFlags::HTTP, "http"),
    (LogFlags::BENCHMARK, "bench"),
    (LogFlags::ZMQ, "zmq"),
    (LogFlags::WALLETDB, "walletdb"),
    (LogFlags::RPC, "rpc"),
    (LogFlags::ESTIMATEFEE, "estimatefee"),
    (LogFlags::ADDRMAN, "addrman"),
    (LogFlags::SELECTCOINS, "selectcoins"),
    (LogFlags::REINDEX, "reindex"),
    (LogFlags::CMPCTBLOCK, "cmpctblock"),
    (LogFlags::RANDOM, "rand"),
    (LogFlags::PRUNE, "prune"),
    (LogFlags::PROXY, "proxy"),
    (LogFlags::MEMPOOLREJ, "mempoolrej"),
    (LogFlags::LIBEVENT, "libevent"),
    (LogFlags::COINDB, "coindb"),
    (LogFlags::QT, "qt"),
    (LogFlags::LEVELDB, "leveldb"),
    (LogFlags::VALIDATION, "validation"),
    (LogFlags::I2P, "i2p"),
    (LogFlags::IPC, "ipc"),
    (LogFlags::BLOCKSTORAGE, "blockstorage"),
    (LogFlags::TXRECONCILIATION, "txreconciliation"),
    (LogFlags::SCAN, "scan"),
    (LogFlags::ALL, "1"),
    (LogFlags::ALL, "all"),
    (LogFlags::CHAINLOCKS, "chainlocks"),
    (LogFlags::GOBJECT, "gobject"),
    (LogFlags::INSTANTSEND, "instantsend"),
    (LogFlags::LLMQ, "llmq"),
    (LogFlags::LLMQ_DKG, "llmq-dkg"),
    (LogFlags::LLMQ_SIGS, "llmq-sigs"),
    (LogFlags::MNPAYMENTS, "mnpayments"),
    (LogFlags::MNSYNC, "mnsync"),
    (LogFlags::COINJOIN, "coinjoin"),
    (LogFlags::SPORK, "spork"),
    (LogFlags::NETCONN, "netconn"),
    (LogFlags::CREDITPOOL, "creditpool"),
    (LogFlags::EHF, "ehf"),
    (LogFlags::DASH, "dash"),
];

/// Core's `GetLogCategory`: the flag a user-given category name selects; the empty string is
/// `ALL`.
// Core: logging.cpp:203-217
pub fn get_log_category(name: &str) -> Option<LogFlags> {
    if name.is_empty() {
        return Some(LogFlags::ALL);
    }
    LOG_CATEGORIES.iter().find(|(_, n)| *n == name).map(|(f, _)| *f)
}

/// Core's `LogCategoryToStr`: the name in a line's prefix. `None` for a set Core never logs
/// under (a mask that is not one category, `DASH` or `NET_NETCONN`).
// Core: logging.cpp:236-336
pub fn log_category_to_str(category: LogFlags) -> Option<&'static str> {
    match category {
        LogFlags::NONE => Some(""),
        LogFlags::NET_NETCONN => Some("net|netconn"),
        LogFlags::ALL => Some("all"),
        LogFlags::DASH => Some("dash"),
        c => LOG_CATEGORIES.iter().find(|(f, n)| *f == c && !n.is_empty()).map(|(_, n)| *n),
    }
}

/// The category a `tracing` target names (DEC-200): a name in a Core line prefix, other than
/// `all` and the empty name. Any other target is not a category.
pub fn category_of_target(target: &str) -> Option<LogFlags> {
    if target == "net|netconn" {
        return Some(LogFlags::NET_NETCONN);
    }
    match get_log_category(target)? {
        LogFlags::NONE | LogFlags::ALL => None,
        f => Some(f),
    }
}

/// The names `LogCategoriesList` reports, sorted, with their flags: every entry but the `NONE`
/// and `ALL` aliases and `dash`.
// Core: logging.cpp:353-372
pub(super) fn listed_categories() -> Vec<(LogFlags, &'static str)> {
    let mut list: Vec<_> = LOG_CATEGORIES
        .iter()
        .copied()
        .filter(|(f, _)| !matches!(*f, LogFlags::NONE | LogFlags::ALL | LogFlags::DASH))
        .collect();
    list.sort_by_key(|(_, n)| *n);
    list
}

/// Core's `BCLog::Level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace = 0,
    Debug,
    Info,
    Warning,
    Error,
}

impl Level {
    /// Core's `DEFAULT_LOG_LEVEL`.
    pub const DEFAULT: Level = Level::Debug;
    /// The highest level `-loglevel` and the `logging` RPC may set (`logging.cpp:17`); warnings
    /// and errors always log.
    pub const MAX_USER_SETTABLE: Level = Level::Info;
    /// Core's `LogLevelsList()`: the levels a user may select, in Core's order.
    pub const USER_LEVELS: [Level; 3] = [Level::Info, Level::Debug, Level::Trace];

    /// Core's `LogLevelToStr`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }

    /// Core's `GetLogLevel`.
    pub fn parse(s: &str) -> Option<Level> {
        [Level::Trace, Level::Debug, Level::Info, Level::Warning, Level::Error]
            .into_iter()
            .find(|l| l.as_str() == s)
    }

    /// A user-given level (`-loglevel`, the `logging` RPC): at most `Info`.
    pub(super) fn parse_user(s: &str) -> Option<Level> {
        Level::parse(s).filter(|l| *l <= Level::MAX_USER_SETTABLE)
    }

    pub(super) fn from_u8(v: u8) -> Level {
        match v {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warning,
            _ => Level::Error,
        }
    }

    /// The level of a `tracing` event.
    pub fn of_tracing(level: tracing::Level) -> Level {
        match level {
            tracing::Level::TRACE => Level::Trace,
            tracing::Level::DEBUG => Level::Debug,
            tracing::Level::INFO => Level::Info,
            tracing::Level::WARN => Level::Warning,
            tracing::Level::ERROR => Level::Error,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_the_prefix_names() {
        for (flag, name) in listed_categories() {
            assert_eq!(get_log_category(name), Some(flag));
            assert_eq!(log_category_to_str(flag), Some(name));
            assert_eq!(category_of_target(name), Some(flag));
        }
        assert_eq!(get_log_category(""), Some(LogFlags::ALL));
        assert_eq!(get_log_category("1"), Some(LogFlags::ALL));
        assert_eq!(get_log_category("0"), Some(LogFlags::NONE));
        assert_eq!(get_log_category("lock"), None);
        assert_eq!(category_of_target("all"), None);
        assert_eq!(category_of_target("dash_chain::actor"), None);
        assert_eq!(category_of_target("net|netconn"), Some(LogFlags::NET_NETCONN));
    }

    #[test]
    fn the_listed_categories_are_core_s_sorted_list() {
        let names: Vec<_> = listed_categories().into_iter().map(|(_, n)| n).collect();
        // Core's `-debug` help embeds LogCategoriesString(); dash-args generates it from v24.
        let args = dash_args::ArgsManager::new();
        dash_args::Program::Dashd.setup_args(&args);
        let help = args.registered_args().into_iter().find(|a| a.name == "-debug").unwrap();
        let list = help.help_text.split("<category> can be: ").nth(1).unwrap();
        let list = list.split('.').next().unwrap();
        assert_eq!(names.join(", "), list);
    }
}

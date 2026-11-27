//! The option support table (DEC-201 Q12): what rdashd does with each option `dashd` takes.
//!
//! An option the table does not mark [`Support::Implemented`] is refused when the user sets it,
//! unless its value is [`Support::Inert`]: what rdashd does anyway. The refusal is computed from
//! the user's settings before any soft-set, and reported after Core's own argument checks, so an
//! argument error dashd reports is reported the same way first.

use dash_args::{ArgsManager, OptionsCategory, RuntimeError};

/// How rdashd treats an option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Implemented,
    /// Accepted only with this value, which is what rdashd does without the option.
    Inert(Inert),
    /// Refused when set; the task that implements it.
    Unsupported(&'static str),
}

/// The value an [`Support::Inert`] option may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inert {
    /// The option read as a boolean (`GetBoolArg`).
    Bool(bool),
    /// Every value is this string; a negation reads `0`.
    Value(&'static str),
    /// Any value: nothing in rdashd reads it yet, and no value changes what it does.
    Any,
}

use Support::{Implemented, Unsupported};

const OFF: Support = Support::Inert(Inert::Bool(false));
const ON: Support = Support::Inert(Inert::Bool(true));
const ANY: Support = Support::Inert(Inert::Any);
const fn only(value: &'static str) -> Support {
    Support::Inert(Inert::Value(value))
}

/// Cap on [`Support::Inert`] entries: keep the list small (Q12). Raising it needs a reason on
/// each new entry.
pub const MAX_INERT: usize = 33; // 33: -unsafesqlitesync (plan Q12, the FT framework)

/// rdashd's own options, registered after Core's.
// `-importmode` is registered by its owner (CH6);
// `-consensusxcheck` is O9b's; `-storageprofile` is BL-634's.
pub fn setup_server_args(args: &ArgsManager) {
    dash_args::Program::Dashd.setup_args(args);
    // Placeholders that only refuse: debug-only until their owners serve them, so `-help` lists
    // only what an operator can use (DEC-1173, BL-661). Serving one makes it operator-facing,
    // which needs its own E4 (DEC-1070 rule (b)).
    args.add_arg(
        "-spv",
        "Run as an SPV node: headers, masternode lists and filters only, no blocks (not supported yet)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
    args.add_arg(
        "-spvwallet=<file>",
        "Load the wallet in <file>, relative to the network data directory, read-only under -spv \
         (not supported yet)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
    args.add_arg(
        "-importcoredatadir=<dir>",
        "Import the Dash Core data directory <dir> (not supported yet)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
    // Registered as Core registers its consistency checks (`-checkblockindex`, `-checkmempool`):
    // debug-only, so `-help` leaves it out and `-help-debug` lists it.
    // Core: init.cpp:801, :805
    args.add_arg(
        "-consensusxcheck",
        "Cross-check the connect loop's coin reads against direct reads while validating, and \
         stop the node on any difference (default: 0)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
    args.add_arg(
        "-statedigestlog=<file>",
        "Append a StateDigest v1 record for every block connect and disconnect attempt to <file> (relative to the data directory)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
    // Regtest-only and debug-only, as Core's `-acceptstalefeeestimates` (registered debug-only,
    // init.cpp:840; refused off regtest, init.cpp:1723-1725). DEC-1070 (BL-634).
    args.add_arg(
        "-storageprofile=<profile>",
        "Open the chainstate with this storage profile (regtest-only): livecopy keeps a copy of \
         the running data directory reopenable; production is the mainnet and testnet profile \
         and makes no such promise (default: livecopy)",
        ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY,
        OptionsCategory::DebugTest,
    );
}

/// Every option `setup_server_args` registers.
#[rustfmt::skip]
pub const SUPPORT: &[(&str, Support)] = &[
    // Help, configuration and the data directory (dash-args, ND1).
    ("-?", Implemented), ("-h", Implemented), ("-help", Implemented), ("-help-debug", Implemented),
    ("-version", Implemented), ("-conf", Implemented), ("-datadir", Implemented),
    ("-includeconf", Implemented), ("-settings", Implemented), ("-allowignoredconf", Implemented),
    ("-pid", Implemented), ("-daemon", Implemented), ("-daemonwait", Implemented),
    ("-startupnotify", Implemented), ("-shutdownnotify", Implemented),
    ("-mocktime", Implemented),
    // Logging (DEC-200).
    ("-debug", Implemented), ("-debugexclude", Implemented), ("-debuglogfile", Implemented),
    ("-loglevel", Implemented), ("-loglevelalways", Implemented), ("-logips", Implemented),
    ("-logsourcelocations", Implemented), ("-logthreadnames", Implemented),
    ("-logtimemicros", Implemented), ("-logtimestamps", Implemented),
    ("-printtoconsole", Implemented), ("-shrinkdebugfile", Implemented),
    // Chain parameters (dash-params; -sporkaddr selects the signer there).
    ("-chain", Implemented), ("-regtest", Implemented), ("-testnet", Implemented),
    ("-devnet", Implemented), ("-budgetparams", Implemented), ("-dip3params", Implemented),
    ("-highsubsidyblocks", Implemented), ("-highsubsidyfactor", Implemented),
    ("-llmqchainlocks", Implemented), ("-llmqdevnetparams", Implemented),
    ("-llmqinstantsenddip0024", Implemented), ("-llmqmnhf", Implemented),
    ("-llmqplatform", Implemented), ("-llmqtestinstantsenddip0024", Implemented),
    ("-llmqtestinstantsendparams", Implemented), ("-llmqtestparams", Implemented),
    ("-llmqtestplatformparams", Implemented), ("-minimumdifficultyblocks", Implemented),
    ("-powtargetspacing", Implemented), ("-testactivationheight", Implemented),
    ("-vbparams", Implemented), ("-sporkaddr", Implemented),
    // Features rdashd does not have yet, accepted when the value turns them off.
    // rdashd has no port mapping yet (BL-327 D4) and no persistent I2P SAM session (P8c-2);
    // with `-i2psam`, `-i2pacceptincoming` is judged by `i2p_accept_refusal`.
    ("-upnp", OFF),
    ("-natpmp", OFF), ("-i2pacceptincoming", OFF),
    ("-disablewallet", ON),
    ("-prune", only("0")),
    ("-addressindex", OFF), ("-spentindex", OFF),
    ("-timestampindex", OFF),
    // IX1 e2 and d3: the txindex and the coinstats index on the index host.
    ("-txindex", Implemented), ("-coinstatsindex", Implemented),
    // GV1b: governance, on by default as in Core (BL-423).
    ("-disablegovernance", Implemented),
    // The basic block filter index on the index host, and BIP157 serving from it.
    ("-blockfilterindex", Implemented), ("-peerblockfilters", Implemented),
    // The functional-test framework sets it on every node; no rdashd code reads it: the RPC help
    // check (help is AR2's verbatim text; nothing to check).
    ("-rpcdoccheck", ANY),
    // The functional-test framework writes it into every dash.conf; rdashd has no wallet.
    ("-unsafesqlitesync", ANY),
    // GUI options, which dashd accepts and ignores too.
    ("-choosedatadir", ANY), ("-lang", ANY), ("-min", ANY),
    ("-resetguisettings", ANY), ("-splash", ANY), ("-uiplatform", ANY),
    // rdashd's own.
    ("-spv", Unsupported("SB18")), ("-spvwallet", Unsupported("SB18")),
    ("-importcoredatadir", Unsupported("CH5")),
    ("-consensusxcheck", Implemented), ("-storageprofile", Implemented),
    ("-statedigestlog", Implemented),
    // Storage, chainstate and validation.
    ("-blocksdir", Unsupported("ST2")), ("-fastprune", Unsupported("ST2")),
    ("-tinyblk", Unsupported("ST2")), ("-dbcache", Implemented),
    ("-dbbatchsize", Implemented), ("-dbcrashratio", Implemented),
    ("-forcecompactdb", Unsupported("CH2b")), ("-assumevalid", Implemented),
    ("-maxsigcachesize", Implemented), ("-minimumchainwork", Unsupported("CH1-node")),
    ("-par", Implemented), ("-parbls", Implemented),
    ("-checkblockindex", Implemented), ("-checkpoints", Unsupported("CH1-node")),
    ("-maxtipage", Unsupported("CH1-node")), ("-stopatheight", Implemented),
    ("-blocknotify", Implemented), ("-alertnotify", Implemented),
    ("-reindex", Implemented), ("-reindex-chainstate", Implemented),
    ("-loadblock", Implemented), ("-stopafterblockimport", Implemented),
    ("-checkblocks", Implemented), ("-checklevel", Implemented),
    ("-forceevodbrepair", Unsupported("EV2b")),
    // Network (P3 S6a outbound; BL-327 inbound over P2b's listener).
    ("-connect", Implemented), ("-dnsseed", Implemented), ("-fixedseeds", Implemented),
    ("-peertimeout", Implemented), ("-uacomment", Implemented), ("-v2transport", Implemented),
    ("-listen", Implemented), ("-discover", Implemented), ("-maxconnections", Implemented),
    ("-bind", Implemented), ("-whitebind", Implemented), ("-port", Implemented),
    ("-whitelist", Implemented), ("-whitelistrelay", Implemented),
    ("-whitelistforcerelay", Implemented),
    ("-addnode", Implemented), ("-seednode", Implemented),
    ("-forcednsseed", Implemented), ("-dns", Implemented),
    // Privacy networks (P8a), Tor control (P8b), I2P's transient sessions (P8c-1), and asmap
    // (P8d).
    ("-externalip", Implemented), ("-onlynet", Implemented),
    ("-proxy", Implemented), ("-proxyrandomize", Implemented),
    ("-onion", Implemented), ("-torcontrol", Implemented),
    ("-torpassword", Implemented), ("-listenonion", Implemented),
    ("-i2psam", Implemented),
    ("-cjdnsreachable", Implemented), ("-asmap", Implemented),
    ("-networkactive", Implemented), ("-timeout", Implemented),
    ("-allowprivatenet", Unsupported("P2a-net")), ("-socketevents", Unsupported("P2a-net")),
    ("-checkaddrman", Implemented),
    ("-maxreceivebuffer", Implemented), ("-maxsendbuffer", Implemented), ("-bantime", Implemented),
    ("-blocksonly", Implemented), ("-maxtimeadjustment", Unsupported("P2a-core")),
    ("-txreconciliation", Unsupported("P2a-core")), ("-capturemessages", Unsupported("P2a-core")),
    ("-pushversion", Unsupported("P2a-core")), ("-syncmempool", Implemented),
    ("-blockreconstructionextratxn", Unsupported("P3")),
    // P4b serves BIP37 filters (D7); `-maxuploadtarget` needs the connection manager's byte
    // accounting (P4b D6).
    ("-peerbloomfilters", Implemented), ("-maxuploadtarget", Unsupported("P2a-net")),
    // Mempool, relay policy and mining.
    // MP1b-G2: the pool's `LimitMempoolSize` enforces both.
    ("-maxmempool", Implemented), ("-mempoolexpiry", Implemented),
    ("-maxorphantxsize", Unsupported("MP1b")), ("-persistmempool", Unsupported("MP1b")),
    // MP1a-9: the node's pool applies these (`MemPoolOptions`). The relay fees stay refused
    // while `getnetworkinfo` reports their defaults (BL-391 D-rows).
    ("-checkmempool", Implemented), ("-limitancestorcount", Implemented),
    ("-limitancestorsize", Implemented), ("-limitdescendantcount", Implemented),
    ("-limitdescendantsize", Implemented), ("-acceptnonstdtxn", Implemented),
    ("-bytespersigop", Implemented), ("-datacarrier", Implemented),
    ("-datacarriersize", Implemented), ("-dustrelayfee", Implemented),
    ("-incrementalrelayfee", Unsupported("MP1a")), ("-minrelaytxfee", Unsupported("MP1a")),
    ("-permitbaremultisig", Implemented), ("-acceptstalefeeestimates", Unsupported("MP2")),
    // MN1-7: the node's block assembler reads these (`dashd_mining::ApplyArgsManOptions`).
    ("-blockmaxsize", Implemented), ("-blockmintxfee", Implemented),
    // MN0-1: `CreateNewBlock`'s override (`dashd_mining::block_version_arg`, DEC-595 D3).
    ("-blockversion", Implemented), ("-printpriority", Implemented),
    // Masternodes, quorums, sporks and ChainLocks.
    ("-masternodeblsprivkey", Implemented), ("-llmq-data-recovery", Implemented),
    ("-llmq-qvvec-sync", Implemented), ("-watchquorums", Implemented),
    ("-maxrecsigsage", Implemented), ("-sporkkey", Implemented),
    ("-chainlocknotify", Implemented),
    // RPC.
    // The RPC server (dashd-rpc). `-deprecated-platform-user` is registered and read by nothing
    // in Core v24 either.
    ("-server", Implemented),
    ("-rpcallowip", Implemented), ("-rpcauth", Implemented),
    ("-rpcbind", Implemented), ("-rpccookiefile", Implemented),
    ("-rpcexternaluser", Implemented), ("-rpcexternalworkqueue", Implemented),
    ("-rpcpassword", Implemented), ("-rpcport", Implemented),
    ("-rpcservertimeout", Implemented), ("-rpcthreads", Implemented),
    ("-rpcuser", Implemented), ("-rpcwhitelist", Implemented),
    ("-rpcwhitelistdefault", Implemented), ("-rpcworkqueue", Implemented),
    ("-deprecatedrpc", Implemented), ("-deprecated-platform-user", Implemented),
    ("-rest", Unsupported("ZQ1")), ("-statsport", Unsupported("ZQ1")),
    ("-maxtxfee", Unsupported("WL2a")),
    // Crash-report decoding has no planned owner.
    ("-printcrashinfo", Unsupported("unplanned")),
];

/// Options [`SUPPORT`] marks implemented that mainnet still refuses, with the task that lifts the
/// refusal there (DEC-1864: an active masternode without DKG is accepted on the test networks
/// only). A chain selection that does not parse refuses, as mainnet does.
const REFUSED_ON_MAINNET: &[(&str, &str)] = &[("-masternodeblsprivkey", "DK1")];

/// Options whose owner covers a whole help group.
fn category_owner(category: OptionsCategory) -> Option<&'static str> {
    match category {
        OptionsCategory::Zmq | OptionsCategory::Statsd => Some("ZQ1"),
        OptionsCategory::WalletCoinjoin => Some("WL6"),
        OptionsCategory::Wallet
        | OptionsCategory::WalletFee
        | OptionsCategory::WalletHd
        | OptionsCategory::WalletDebugTest => Some("WL2a"),
        _ => None,
    }
}

/// How rdashd treats `name` (with its dash). `None` for an option the table misses, which the
/// ratchet test forbids.
pub fn support(args: &ArgsManager, name: &str) -> Option<Support> {
    if let Some((_, owner)) = REFUSED_ON_MAINNET.iter().find(|(n, _)| *n == name) {
        let main = args.get_chain_name().map_or(true, |chain| chain == dash_args::chain::MAIN);
        return Some(if main {
            Unsupported(owner)
        } else {
            Implemented
        });
    }
    if let Some((_, s)) = SUPPORT.iter().find(|(n, _)| *n == name) {
        return Some(*s);
    }
    let arg = args.registered_args().into_iter().find(|a| a.name == name)?;
    category_owner(arg.category).map(Unsupported)
}

/// Whether rdashd serves the option `name`: [`Implemented`] or [`Support::Inert`]. Code an
/// unserved option would drive runs only once it is served, so until then setting it gives
/// only its refusal (BL-679). A name with no support row is not served.
pub(crate) fn served(args: &ArgsManager, name: &str) -> bool {
    matches!(support(args, name), Some(Implemented | Support::Inert(_)))
}

/// The refusal for the first option, in help order (by category, then name), that the user
/// set to a value rdashd does not support: `-<opt> is not supported by rdashd yet (<owner>).`
/// Call it before any soft-set.
pub fn unsupported_option(args: &ArgsManager) -> Result<Option<String>, RuntimeError> {
    for arg in args.registered_args() {
        let name = arg.name.as_str();
        if !args.is_arg_set(name) || (name == "-i2pacceptincoming" && i2psam_set(args)?) {
            continue;
        }
        let owner = match support(args, name) {
            Some(Implemented) => continue,
            Some(Support::Inert(inert)) if is_inert(args, name, inert)? => continue,
            Some(Support::Inert(_)) => owner_of_inert(name),
            Some(Unsupported(owner)) => owner,
            None => "unplanned",
        };
        return Ok(Some(not_supported(name, owner)));
    }
    Ok(None)
}

fn i2psam_set(args: &ArgsManager) -> Result<bool, RuntimeError> {
    Ok(!args.get_arg("-i2psam", "")?.is_empty())
}

/// With `-i2psam`, `-i2pacceptincoming` true after parameter interaction (Core's default, unless
/// `-listen=0` soft-set it to 0) would create the persistent session that accepts I2P
/// connections, which P8c-2 brings: refused until then. Without `-i2psam`,
/// [`unsupported_option`] judges the option as an inert one. Call it after
/// `InitParameterInteraction`.
// Core: init.cpp:1053-1055, :2758; net.cpp:4038-4041
pub fn i2p_accept_refusal(args: &ArgsManager) -> Result<Option<String>, RuntimeError> {
    let accept =
        args.get_bool_arg("-i2pacceptincoming", crate::init::consts::DEFAULT_I2P_ACCEPT_INCOMING)?;
    Ok((i2psam_set(args)? && accept).then(|| not_supported("-i2pacceptincoming", "P8c-2")))
}

/// The refusal text for the option `name` (`-<opt>`), which `owner` will implement.
pub fn not_supported(name: &str, owner: &str) -> String {
    format!("{name} is not supported by rdashd yet ({owner}).")
}

fn is_inert(args: &ArgsManager, name: &str, inert: Inert) -> Result<bool, RuntimeError> {
    Ok(match inert {
        Inert::Bool(b) => args.get_bool_arg(name, !b)? == b,
        Inert::Value(v) if args.is_arg_negated(name) => v == "0",
        Inert::Value(v) => args.get_args(name)?.iter().all(|x| x == v),
        Inert::Any => true,
    })
}

/// Who turns an inert option on.
fn owner_of_inert(name: &str) -> &'static str {
    match name {
        "-i2pacceptincoming" => "P8c",
        "-upnp" | "-natpmp" => "P8e",
        "-disablewallet" => "WL2a",
        "-prune" => "CH8",
        "-addressindex" | "-spentindex" | "-timestampindex" => "IX2",
        _ => "unplanned",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::init::test_args;

    fn refusal(argv: &[&str]) -> Option<String> {
        unsupported_option(&test_args(argv)).unwrap()
    }

    #[test]
    fn every_registered_option_has_one_entry_and_every_entry_is_registered() {
        let args = test_args(&[]);
        let registered: BTreeSet<String> =
            args.registered_args().into_iter().map(|a| a.name).collect();
        let mut seen = BTreeSet::new();
        for (name, _) in SUPPORT {
            assert!(seen.insert(*name), "{name} is listed twice");
            assert!(registered.contains(*name), "{name} is not an option");
        }
        let missing: Vec<_> = registered.iter().filter(|n| support(&args, n).is_none()).collect();
        assert!(missing.is_empty(), "options without a support entry: {missing:?}");
    }

    /// Whether `-help-debug` lists `name` in its "Debugging/Testing options:" group, from the
    /// heading to the next one (headings are the unindented lines).
    fn in_debug_group(name: &str) -> bool {
        let debug = test_args(&["-help-debug"]).get_help_message().unwrap();
        let start = debug.find("Debugging/Testing options:").unwrap();
        let group = &debug[start..];
        let end = group
            .lines()
            .skip(1)
            .find(|l| !l.is_empty() && !l.starts_with(' '))
            .map_or(group.len(), |next| group.find(next).unwrap());
        group[..end].contains(name)
    }

    /// `-consensusxcheck` is rdashd's own debug option (O9b): accepted, listed by `-help-debug`
    /// only, as Core's `-checkblockindex`.
    #[test]
    fn consensusxcheck_is_a_debug_only_option() {
        assert_eq!(refusal(&["-noconsensusxcheck"]), None);
        let plain = test_args(&[]).get_help_message().unwrap();
        assert!(!plain.contains("-consensusxcheck"));
        assert!(!plain.contains("-checkblockindex"), "Core's own: the control");
        let debug = test_args(&["-help-debug"]).get_help_message().unwrap();
        assert!(debug.contains("-checkblockindex"));
        assert!(in_debug_group("-consensusxcheck"), "in Core's group");
    }

    #[test]
    fn statedigestlog_is_a_debug_only_option() {
        assert_eq!(refusal(&["-statedigestlog=log.jsonl"]), None);
        assert_eq!(support(&test_args(&[]), "-statedigestlog"), Some(Implemented));
        assert!(!test_args(&[]).get_help_message().unwrap().contains("-statedigestlog"));
        assert!(in_debug_group("-statedigestlog=<file>"));
    }

    /// `-storageprofile` is rdashd's own regtest-only debug option (BL-634, DEC-1070): served,
    /// listed by `-help-debug` only, in Core's debug group.
    #[test]
    fn storageprofile_is_a_debug_only_option() {
        assert_eq!(refusal(&["-storageprofile=production"]), None);
        assert_eq!(support(&test_args(&[]), "-storageprofile"), Some(Implemented));
        let plain = test_args(&[]).get_help_message().unwrap();
        assert!(!plain.contains("-storageprofile"));
        assert!(in_debug_group("-storageprofile=<profile>"), "in Core's group");
    }

    /// MN1-7: the node's block assembler reads the three options (`ApplyArgsManOptions`).
    #[test]
    fn the_block_assembler_options_are_implemented() {
        let args = test_args(&[]);
        for name in ["-blockmaxsize", "-blockmintxfee", "-printpriority"] {
            assert_eq!(support(&args, name), Some(Implemented), "{name}");
        }
        assert_eq!(
            refusal(&["-blockmaxsize=1500", "-blockmintxfee=0.0001", "-printpriority"]),
            None
        );
    }

    /// `-spv`, `-spvwallet` and `-importcoredatadir` refuse until SB18 and CH5 serve them, so
    /// `-help-debug` lists them in Core's debug group and `-help` does not (DEC-1173, BL-661).
    #[test]
    fn the_unserved_placeholders_are_debug_only() {
        let args = test_args(&[]);
        assert_eq!(support(&args, "-spv"), Some(Unsupported("SB18")));
        assert_eq!(support(&args, "-spvwallet"), Some(Unsupported("SB18")));
        assert_eq!(support(&args, "-importcoredatadir"), Some(Unsupported("CH5")));
        let plain = args.get_help_message().unwrap();
        assert!(!plain.contains("-spv"), "{plain}");
        assert!(!plain.contains("-importcoredatadir"), "{plain}");
        assert!(in_debug_group("  -spv\n"), "in Core's group");
        assert!(in_debug_group("-spvwallet=<file>"), "in Core's group");
        assert!(in_debug_group("-importcoredatadir=<dir>"), "in Core's group");
    }

    /// `served` opens only for a served row: an unknown name (a typo at a call site) stays shut.
    #[test]
    fn only_implemented_and_inert_options_are_served() {
        let args = test_args(&[]);
        assert!(served(&args, "-consensusxcheck"));
        assert!(served(&args, "-txindex"), "implemented (IX1 e2)");
        assert!(served(&args, "-coinstatsindex"), "implemented (IX1 d3)");
        assert!(served(&args, "-spentindex"), "inert");
        assert!(!served(&args, "-spv"));
        assert!(!served(&args, "-spvwallet"));
        assert!(!served(&args, "-no-such-option"));
    }

    /// U1: `-dbcrashratio` is served (CH3b b4b): accepted at any value, and 0 reads as unset.
    #[test]
    fn dbcrashratio_is_implemented() {
        assert_eq!(support(&test_args(&[]), "-dbcrashratio"), Some(Implemented));
        for arg in ["-dbcrashratio=0", "-dbcrashratio=1", "-dbcrashratio=-1"] {
            assert_eq!(refusal(&[arg]), None, "{arg}");
        }
        let ratio = |argv: &[&str]| crate::init::caches::crash_ratio(&test_args(argv)).unwrap();
        assert_eq!(ratio(&["-dbcrashratio=0"]), ratio(&[]));
    }

    /// MP1b-G2: the pool's `LimitMempoolSize` enforces `-maxmempool` and `-mempoolexpiry`, so
    /// both are served at any value; the rest of MP1b's options stay refused.
    #[test]
    fn maxmempool_and_mempoolexpiry_are_served() {
        for arg in ["-maxmempool=5", "-maxmempool=301", "-mempoolexpiry=1", "-mempoolexpiry=-1"] {
            assert_eq!(refusal(&[arg]), None, "{arg}");
        }
        assert_eq!(refusal(&["-persistmempool=0"]), Some(not_supported("-persistmempool", "MP1b")));
    }

    #[test]
    fn the_inert_list_stays_small() {
        let inert = SUPPORT.iter().filter(|(_, s)| matches!(s, Support::Inert(_))).count();
        assert!(inert <= MAX_INERT, "{inert} inert options; the cap is {MAX_INERT}");
        for (name, s) in SUPPORT {
            if let Support::Inert(Inert::Bool(_) | Inert::Value(_)) = s {
                assert_ne!(owner_of_inert(name), "unplanned", "{name} needs an owner");
            }
        }
    }

    /// PF0-N's classifier for a refused node workload (BL-128), written out: the last stderr line
    /// must match `^Error: (-[a-z0-9-]+) is not supported by rdashd yet \(([A-Za-z0-9-]+)\)\.$`.
    /// node-bench reads this text, so it changes only with PF0-N and the manager told first.
    fn pf0n_refusal(line: &str) -> Option<(&str, &str)> {
        let rest = line.strip_prefix("Error: ")?;
        let (name, rest) = rest.split_once(" is not supported by rdashd yet (")?;
        let owner = rest.strip_suffix(").")?;
        let all = |s: &str, ok: fn(char) -> bool| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-' || ok(c))
        };
        let name_ok = name.strip_prefix('-').is_some_and(|n| all(n, |c| c.is_ascii_lowercase()));
        (name_ok && all(owner, |c| c.is_ascii_alphabetic())).then_some((name, owner))
    }

    /// Every option rdashd can refuse, refused through init's error path, is one line PF0-N's
    /// classifier reads back as that option and its owner.
    #[test]
    fn every_refusal_matches_pf0_n_s_classifier() {
        assert_eq!(
            pf0n_refusal("Error: -a1 is not supported by rdashd yet (P2a-net)."),
            Some(("-a1", "P2a-net"))
        );
        for bad in [
            "Error: -A is not supported by rdashd yet (RP0).",
            "Error: -a is not supported by rdashd yet (R P0).",
            "Error: -a is not supported by rdashd yet (RP0)",
            "-a is not supported by rdashd yet (RP0).",
        ] {
            assert_eq!(pf0n_refusal(bad), None, "{bad}");
        }
        let args = test_args(&[]);
        let (logger, _) = crate::init::test_logger();
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let ui =
            crate::ui::Ui::with_stderr(logger, Box::new(move |s| sink.lock().unwrap().push_str(s)));
        let mut refusable = 0;
        for arg in args.registered_args() {
            let name = arg.name.as_str();
            let owner = match support(&args, name) {
                Some(Implemented) | Some(Support::Inert(Inert::Any)) => continue,
                Some(Support::Inert(_)) => owner_of_inert(name),
                Some(Unsupported(owner)) => owner,
                None => "unplanned",
            };
            stderr.lock().unwrap().clear();
            ui.init_error(&not_supported(name, owner));
            let text = stderr.lock().unwrap().clone();
            let last = text.strip_suffix('\n').and_then(|t| t.lines().last());
            assert_eq!(last.and_then(pf0n_refusal), Some((name, owner)), "{text:?}");
            refusable += 1;
        }
        // The loop's breadth, not a target: each option a slice implements lowers it (172 after
        // BL-273's `-assumevalid` and `-maxsigcachesize` met T22's network options, P4b's
        // `-peerbloomfilters`, BL-327's nine listening options and its buffer and ban options,
        // P5a's `-blocksonly`, and BL-391's `-networkactive`; 165 after P8a's proxy and
        // reachability options; 164 after SP1's `-syncmempool` (T40, DEC-555); 161 after P8b's
        // Tor control options; 160 after MN0-1's `-blockversion`, DEC-595 D3; 158 after ST3b S0's
        // `-dbcache` and `-dbbatchsize`, DEC-698; 157 after BL-525's `-checkblockindex`; 155 after
        // CH5-1a's `-checkblocks` and `-checklevel`; 154 after CH3b b4b's `-dbcrashratio`; 152
        // after CH5-2a's `-reindex-chainstate` and `-stopafterblockimport`; 151 after CH5-3b-1's
        // `-reindex`; 150 after `-chainlocknotify`; 149 after `-maxrecsigsage`; 146 after QM1b-7's
        // `-llmq-data-recovery`, `-llmq-qvvec-sync` and `-watchquorums`; 135 after MP1a-9's eleven
        // pool options; 134 after IX1-e2's `-txindex`; 132 after F3's `-blockfilterindex` and
        // `-peerblockfilters`; 130 after MP1b-G2's `-maxmempool` and `-mempoolexpiry`; 129 after
        // IX1-d3's `-coinstatsindex`; 126 after P8 S0's `-addnode`, `-seednode` and
        // `-forcednsseed`; 125 after P8 S1's `-i2psam`, whose `-i2pacceptincoming` refusal
        // (P8c-2) is checked below and not counted; 123 after P8 S2's `-asmap` and
        // `-checkaddrman`; 122 after CH5-3c's `-loadblock`; 121 after GV1b S5a's
        // `-disablegovernance`; 118 after MN1-7a's `-blockmaxsize`, `-blockmintxfee` and
        // `-printpriority`).
        assert!(refusable >= 118, "{refusable}");
        let accept = not_supported("-i2pacceptincoming", "P8c-2");
        stderr.lock().unwrap().clear();
        ui.init_error(&accept);
        let text = stderr.lock().unwrap().clone();
        let last = text.strip_suffix('\n').and_then(|t| t.lines().last());
        assert_eq!(last.and_then(pf0n_refusal), Some(("-i2pacceptincoming", "P8c-2")), "{text:?}");
    }

    /// DEC-1864: `-masternodeblsprivkey` runs on regtest, devnet and testnet, and mainnet still
    /// refuses it with its owner.
    #[test]
    fn the_masternode_key_is_refused_on_mainnet_only() {
        // `test_args` selects regtest when `argv` names no chain, so the default chain and a
        // selection that does not parse need arguments parsed without it.
        let key_on = |chain: &[&str]| {
            let args = ArgsManager::new();
            setup_server_args(&args);
            let mut argv = vec!["dashd", "-masternodeblsprivkey=00"];
            argv.extend_from_slice(chain);
            args.parse_parameters(&argv).unwrap();
            unsupported_option(&args).unwrap()
        };
        let refused = "-masternodeblsprivkey is not supported by rdashd yet (DK1).";
        for chain in [&[][..], &["-chain=main"], &["-regtest", "-testnet"]] {
            assert_eq!(key_on(chain).as_deref(), Some(refused), "{chain:?}");
        }
        for net in ["-regtest", "-testnet", "-devnet=x", "-chain=regtest"] {
            assert_eq!(key_on(&[net]), None, "{net}");
        }
    }

    /// P8 S1: `-i2psam` is served. With it, `-i2pacceptincoming` refuses (P8c-2) unless it is
    /// false after parameter interaction; without it, the option keeps its inert row (P8c).
    #[test]
    fn i2pacceptincoming_with_i2psam_waits_for_p8c_2() {
        let judged = |argv: &[&str]| {
            let args = test_args(argv);
            let first = unsupported_option(&args).unwrap();
            let (logger, _) = crate::init::test_logger();
            crate::init::init_parameter_interaction(&args, &logger).unwrap();
            first.or(i2p_accept_refusal(&args).unwrap())
        };
        let p8c2 = Some(not_supported("-i2pacceptincoming", "P8c-2"));
        let sam = "-i2psam=127.0.0.1:7656";
        assert_eq!(judged(&[sam]), p8c2);
        assert_eq!(judged(&[sam, "-i2pacceptincoming"]), p8c2);
        assert_eq!(judged(&[sam, "-i2pacceptincoming=1", "-listen=0"]), p8c2);
        for off in ["-i2pacceptincoming=0", "-noi2pacceptincoming", "-listen=0"] {
            assert_eq!(judged(&[sam, off]), None, "{off}");
        }
        // Both soft-set `-listen=0`, which soft-sets `-i2pacceptincoming=0`.
        for listen_off in ["-connect=1.2.3.4", "-proxy=1.2.3.4"] {
            assert_eq!(judged(&[sam, listen_off]), None, "{listen_off}");
        }
        assert_eq!(judged(&[]), None);
        assert_eq!(judged(&["-i2psam="]), None, "an empty -i2psam is unset");
        assert_eq!(judged(&["-i2pacceptincoming=0"]), None);
        assert_eq!(
            judged(&["-i2pacceptincoming=1"]),
            Some(not_supported("-i2pacceptincoming", "P8c"))
        );
        assert_eq!(support(&test_args(&[]), "-i2psam"), Some(Implemented));
    }

    #[test]
    fn unsupported_options_are_refused_with_their_owner() {
        assert_eq!(refusal(&[]), None);
        assert_eq!(refusal(&["-debug=net", "-printtoconsole", "-mocktime=5"]), None);
        assert_eq!(refusal(&["-blocknotify=true %s"]), None);
        assert_eq!(refusal(&["-loadblock=x"]), None);
        assert_eq!(refusal(&["-chainlocknotify=true %s"]), None);
        assert_eq!(refusal(&["-chainlocknotify="]), None);
        for arg in ["-blocksonly=0", "-blocksonly=1"] {
            assert_eq!(refusal(&[arg]), None, "{arg}");
        }
        assert_eq!(refusal(&["-stopatheight=-1"]), None);
        for arg in ["-checkblockindex", "-checkblockindex=0", "-checkblockindex=foo"] {
            assert_eq!(refusal(&[arg]), None, "BL-525: {arg}");
        }
        // `rpc_invalidateblock`'s restarts pass `-checkblocks=5` (CH5-1a).
        for arg in ["-checkblocks=5", "-checkblocks=0", "-checklevel=4", "-checklevel=2"] {
            assert_eq!(refusal(&[arg]), None, "CH5-1a: {arg}");
        }
        assert_eq!(refusal(&["-server", "-rpcport=1", "-rpcallowip=1.2.3.4", "-rpcauth=x"]), None);
        assert_eq!(refusal(&["-rest"]).unwrap(), "-rest is not supported by rdashd yet (ZQ1).");
        assert_eq!(refusal(&["-spv"]).unwrap(), "-spv is not supported by rdashd yet (SB18).");
        assert_eq!(
            refusal(&["-spvwallet=w.txt"]).unwrap(),
            "-spvwallet is not supported by rdashd yet (SB18)."
        );
        assert_eq!(
            refusal(&["-importcoredatadir=/x"]).unwrap(),
            "-importcoredatadir is not supported by rdashd yet (CH5)."
        );
        assert_eq!(
            refusal(&["-zmqpubhashblock=tcp://x"]).unwrap(),
            "-zmqpubhashblock is not supported by rdashd yet (ZQ1)."
        );
        assert_eq!(
            refusal(&["-statshost=x"]).unwrap(),
            "-statshost is not supported by rdashd yet (ZQ1)."
        );
    }

    #[test]
    fn inert_values_pass_and_others_name_their_owner() {
        assert_eq!(
            refusal(&["-listen=0", "-nodnsseed", "-connect=0", "-prune=0", "-txindex=0"]),
            None
        );
        assert_eq!(
            refusal(&["-noconnect", "-server=0", "-disablewallet", "-par=4", "-uacomment=x"]),
            None
        );
        assert_eq!(refusal(&["-listen=1", "-bind=127.0.0.1", "-port=5", "-discover"]), None);
        assert_eq!(refusal(&["-listenonion=1", "-torcontrol=127.0.0.1:1", "-torpassword=x"]), None);
        assert_eq!(refusal(&["-upnp"]).unwrap(), "-upnp is not supported by rdashd yet (P8e).");
        assert_eq!(
            refusal(&["-prune=550"]).unwrap(),
            "-prune is not supported by rdashd yet (CH8)."
        );
        assert_eq!(
            refusal(&["-spentindex"]).unwrap(),
            "-spentindex is not supported by rdashd yet (IX2)."
        );
    }

    /// `-blockfilterindex` and `-peerblockfilters` are implemented: the table refuses no value of
    /// either and names no owner; Core's own checks refuse what Core refuses.
    #[test]
    fn the_filter_options_are_implemented() {
        for name in ["-blockfilterindex", "-peerblockfilters"] {
            assert_eq!(support(&test_args(&[]), name), Some(Implemented), "{name}");
            assert_eq!(owner_of_inert(name), "unplanned", "{name}");
        }
        for argv in [
            &["-blockfilterindex"][..],
            &["-blockfilterindex=1"],
            &["-blockfilterindex=basic"],
            &["-blockfilterindex=0"],
            &["-noblockfilterindex"],
            &["-blockfilterindex=x"],
            &["-peerblockfilters"],
            &["-peerblockfilters=0"],
            &["-nopeerblockfilters"],
        ] {
            assert_eq!(refusal(argv), None, "{argv:?}");
        }
    }

    /// IX1 e2: `-txindex` is implemented, so no value of it is refused.
    #[test]
    fn txindex_is_implemented() {
        assert_eq!(support(&test_args(&[]), "-txindex"), Some(Implemented));
        for argv in [&["-txindex"][..], &["-txindex=1"], &["-txindex=0"], &["-notxindex"]] {
            assert_eq!(refusal(argv), None, "{argv:?}");
        }
    }

    /// P8 S2: `-asmap` and `-checkaddrman` are implemented; `-checkaddrman` stays `DEBUG_ONLY`.
    // Core: init.cpp:653, :807
    #[test]
    fn asmap_and_checkaddrman_are_implemented() {
        let args = test_args(&[]);
        assert_eq!(support(&args, "-asmap"), Some(Implemented));
        assert_eq!(support(&args, "-checkaddrman"), Some(Implemented));
        for arg in [
            "-asmap",
            "-asmap=",
            "-asmap=custom.map",
            "-noasmap",
            "-checkaddrman=0",
            "-checkaddrman=1",
            "-checkaddrman=-5",
            "-checkaddrman=2000000",
        ] {
            assert_eq!(refusal(&[arg]), None, "{arg}");
        }
        let plain = args.get_help_message().unwrap();
        assert!(plain.contains("-asmap=<file>"));
        assert!(!plain.contains("-checkaddrman"));
        assert!(in_debug_group("-checkaddrman=<n>"));
    }

    /// IX1 d3: `-coinstatsindex` is implemented, so no value of it is refused and it names no
    /// owner.
    #[test]
    fn coinstatsindex_is_implemented() {
        assert_eq!(support(&test_args(&[]), "-coinstatsindex"), Some(Implemented));
        assert_eq!(owner_of_inert("-coinstatsindex"), "unplanned");
        for argv in [
            &["-coinstatsindex"][..],
            &["-coinstatsindex=1"],
            &["-coinstatsindex=0"],
            &["-nocoinstatsindex"],
        ] {
            assert_eq!(refusal(argv), None, "{argv:?}");
        }
    }
}

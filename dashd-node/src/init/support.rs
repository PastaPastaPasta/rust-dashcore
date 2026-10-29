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
// `-importmode` and `-statedigestlog` are registered by their owners (CH6, O8).
pub fn setup_server_args(args: &ArgsManager) {
    dash_args::Program::Dashd.setup_args(args);
    args.add_arg(
        "-spv",
        "Run as an SPV node: headers, masternode lists and filters only, no blocks (not supported yet)",
        ArgsManager::ALLOW_ANY,
        OptionsCategory::Options,
    );
    args.add_arg(
        "-importcoredatadir=<dir>",
        "Import the Dash Core data directory <dir> (not supported yet)",
        ArgsManager::ALLOW_ANY,
        OptionsCategory::Options,
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
    // rdashd has no port mapping or I2P SAM session yet (BL-327 D4).
    ("-upnp", OFF),
    ("-natpmp", OFF), ("-i2pacceptincoming", OFF),
    ("-disablewallet", ON), ("-disablegovernance", ON),
    ("-prune", only("0")), ("-txindex", OFF),
    ("-addressindex", OFF), ("-spentindex", OFF),
    ("-timestampindex", OFF), ("-coinstatsindex", OFF),
    ("-blockfilterindex", only("0")), ("-peerblockfilters", OFF),
    // The functional-test framework sets these on every node; no rdashd code reads them yet:
    // BLS check threads (LQ3) and the RPC help check (help is AR2's verbatim text; nothing to
    // check).
    ("-parbls", ANY), ("-rpcdoccheck", ANY),
    // The functional-test framework writes it into every dash.conf; rdashd has no wallet.
    ("-unsafesqlitesync", ANY),
    // GUI options, which dashd accepts and ignores too.
    ("-choosedatadir", ANY), ("-lang", ANY), ("-min", ANY),
    ("-resetguisettings", ANY), ("-splash", ANY), ("-uiplatform", ANY),
    // rdashd's own.
    ("-spv", Unsupported("SB18")), ("-importcoredatadir", Unsupported("CH5")),
    // Storage, chainstate and validation.
    ("-blocksdir", Unsupported("ST2")), ("-fastprune", Unsupported("ST2")),
    ("-tinyblk", Unsupported("ST2")), ("-dbcache", Unsupported("CH2a")),
    ("-dbbatchsize", Unsupported("CH2b")), ("-dbcrashratio", Unsupported("CH2b")),
    ("-forcecompactdb", Unsupported("CH2b")), ("-assumevalid", Implemented),
    ("-maxsigcachesize", Implemented), ("-minimumchainwork", Unsupported("CH1-node")),
    ("-par", Implemented),
    ("-checkblockindex", Implemented), ("-checkpoints", Unsupported("CH1-node")),
    ("-maxtipage", Unsupported("CH1-node")), ("-stopatheight", Implemented),
    ("-blocknotify", Implemented), ("-alertnotify", Implemented),
    ("-reindex", Unsupported("CH5")), ("-reindex-chainstate", Unsupported("CH5")),
    ("-loadblock", Unsupported("CH5")), ("-stopafterblockimport", Unsupported("CH5")),
    ("-checkblocks", Unsupported("CH5")), ("-checklevel", Unsupported("CH5")),
    ("-forceevodbrepair", Unsupported("EV2b")),
    // Network (P3 S6a outbound; BL-327 inbound over P2b's listener).
    ("-connect", Implemented), ("-dnsseed", Implemented), ("-fixedseeds", Implemented),
    ("-peertimeout", Implemented), ("-uacomment", Implemented), ("-v2transport", Implemented),
    ("-listen", Implemented), ("-discover", Implemented), ("-maxconnections", Implemented),
    ("-bind", Implemented), ("-whitebind", Implemented), ("-port", Implemented),
    ("-whitelist", Implemented), ("-whitelistrelay", Implemented),
    ("-whitelistforcerelay", Implemented),
    ("-addnode", Unsupported("P2a-net")), ("-seednode", Unsupported("P2a-net")),
    ("-forcednsseed", Unsupported("P2a-net")), ("-dns", Implemented),
    // Privacy networks (P8a), Tor control (P8b); I2P and asmap are P8c and P8d.
    ("-externalip", Implemented), ("-onlynet", Implemented),
    ("-proxy", Implemented), ("-proxyrandomize", Implemented),
    ("-onion", Implemented), ("-torcontrol", Implemented),
    ("-torpassword", Implemented), ("-listenonion", Implemented),
    ("-i2psam", Unsupported("P8c")),
    ("-cjdnsreachable", Implemented), ("-asmap", Unsupported("P8d")),
    ("-networkactive", Implemented), ("-timeout", Implemented),
    ("-allowprivatenet", Unsupported("P2a-net")), ("-socketevents", Unsupported("P2a-net")),
    ("-checkaddrman", Unsupported("P2a-net")),
    ("-maxreceivebuffer", Implemented), ("-maxsendbuffer", Implemented), ("-bantime", Implemented),
    ("-blocksonly", Implemented), ("-maxtimeadjustment", Unsupported("P2a-core")),
    ("-txreconciliation", Unsupported("P2a-core")), ("-capturemessages", Unsupported("P2a-core")),
    ("-pushversion", Unsupported("P2a-core")), ("-syncmempool", Implemented),
    ("-blockreconstructionextratxn", Unsupported("P3")),
    // P4b serves BIP37 filters (D7); `-maxuploadtarget` needs the connection manager's byte
    // accounting (P4b D6).
    ("-peerbloomfilters", Implemented), ("-maxuploadtarget", Unsupported("P2a-net")),
    // Mempool, relay policy and mining.
    ("-maxmempool", Unsupported("MP1b")), ("-mempoolexpiry", Unsupported("MP1b")),
    ("-maxorphantxsize", Unsupported("MP1b")), ("-persistmempool", Unsupported("MP1b")),
    ("-checkmempool", Unsupported("MP1a")), ("-limitancestorcount", Unsupported("MP1a")),
    ("-limitancestorsize", Unsupported("MP1a")), ("-limitdescendantcount", Unsupported("MP1a")),
    ("-limitdescendantsize", Unsupported("MP1a")), ("-acceptnonstdtxn", Unsupported("MP1a")),
    ("-bytespersigop", Unsupported("MP1a")), ("-datacarrier", Unsupported("MP1a")),
    ("-datacarriersize", Unsupported("MP1a")), ("-dustrelayfee", Unsupported("MP1a")),
    ("-incrementalrelayfee", Unsupported("MP1a")), ("-minrelaytxfee", Unsupported("MP1a")),
    ("-permitbaremultisig", Unsupported("MP1a")), ("-acceptstalefeeestimates", Unsupported("MP2")),
    ("-blockmaxsize", Unsupported("MN1")), ("-blockmintxfee", Unsupported("MN1")),
    // MN0-1: `CreateNewBlock`'s override (`dashd_mining::block_version_arg`, DEC-595 D3).
    ("-blockversion", Implemented), ("-printpriority", Unsupported("MN1")),
    // Masternodes, quorums, sporks and ChainLocks.
    ("-masternodeblsprivkey", Unsupported("AM1")), ("-llmq-data-recovery", Unsupported("QM1b")),
    ("-llmq-qvvec-sync", Unsupported("QM1b")), ("-watchquorums", Unsupported("QM1b")),
    ("-maxrecsigsage", Unsupported("SG1a")), ("-sporkkey", Implemented),
    ("-chainlocknotify", Unsupported("LK1a")),
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
    if let Some((_, s)) = SUPPORT.iter().find(|(n, _)| *n == name) {
        return Some(*s);
    }
    let arg = args.registered_args().into_iter().find(|a| a.name == name)?;
    category_owner(arg.category).map(Unsupported)
}

/// The refusal for the first option, in registration order, that the user set to a value
/// rdashd does not support: `-<opt> is not supported by rdashd yet (<owner>).` Call it before
/// any soft-set.
pub fn unsupported_option(args: &ArgsManager) -> Result<Option<String>, RuntimeError> {
    for arg in args.registered_args() {
        let name = arg.name.as_str();
        if !args.is_arg_set(name) {
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
        "-disablegovernance" => "GV1b",
        "-prune" => "CH8",
        "-txindex" | "-blockfilterindex" | "-coinstatsindex" => "IX1",
        "-addressindex" | "-spentindex" | "-timestampindex" => "IX2",
        "-peerblockfilters" => "P4",
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
        // Tor control options; 160 after MN0-1's `-blockversion`, DEC-595 D3; 159 after BL-525's
        // `-checkblockindex`).
        assert!(refusable >= 159, "{refusable}");
    }

    #[test]
    fn unsupported_options_are_refused_with_their_owner() {
        assert_eq!(refusal(&[]), None);
        assert_eq!(refusal(&["-debug=net", "-printtoconsole", "-mocktime=5"]), None);
        assert_eq!(refusal(&["-blocknotify=true %s"]), None);
        assert_eq!(refusal(&["-blocksonly=0"]), None, "DEC-322");
        assert_eq!(refusal(&["-stopatheight=-1"]), None);
        for arg in ["-checkblockindex", "-checkblockindex=0", "-checkblockindex=foo"] {
            assert_eq!(refusal(&[arg]), None, "BL-525: {arg}");
        }
        assert_eq!(refusal(&["-server", "-rpcport=1", "-rpcallowip=1.2.3.4", "-rpcauth=x"]), None);
        assert_eq!(refusal(&["-rest"]).unwrap(), "-rest is not supported by rdashd yet (ZQ1).");
        assert_eq!(refusal(&["-spv"]).unwrap(), "-spv is not supported by rdashd yet (SB18).");
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
            refusal(&["-txindex"]).unwrap(),
            "-txindex is not supported by rdashd yet (IX1)."
        );
    }
}

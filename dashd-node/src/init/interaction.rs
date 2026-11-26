//! Core's `InitParameterInteraction`: options that soft-set others.

use dash_args::{ArgsManager, RuntimeError};

use super::consts::*;
use super::support::served;
use crate::logging::Logger;

/// Core's `Network` as `ParseNetwork` returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Unroutable,
    Ipv4,
    Ipv6,
    Onion,
    I2p,
    Cjdns,
}

/// Core's `ParseNetwork`: case-insensitive; `tor` is `onion` with a deprecation line.
// Core: netbase.cpp:99-115
pub fn parse_network(logger: &Logger, net: &str) -> Network {
    match net.to_ascii_lowercase().as_str() {
        "ipv4" => Network::Ipv4,
        "ipv6" => Network::Ipv6,
        "onion" => Network::Onion,
        "tor" => {
            logger.log_printf("Warning: net name 'tor' is deprecated and will be removed in the future. You should use 'onion' instead.\n");
            Network::Onion
        }
        "i2p" => Network::I2p,
        "cjdns" => Network::Cjdns,
        _ => Network::Unroutable,
    }
}

/// Core's `InitParameterInteraction`: each soft-set that takes effect logs a line.
// Core: init.cpp:1000-1101
pub fn init_parameter_interaction(args: &ArgsManager, logger: &Logger) -> Result<(), RuntimeError> {
    interaction_as(args, logger, served(args, "-spv"))
}

/// [`init_parameter_interaction`] as if `-spv` were served or not.
pub(super) fn interaction_as(
    args: &ArgsManager,
    logger: &Logger,
    spv_served: bool,
) -> Result<(), RuntimeError> {
    let soft_set = |arg: &str, value: bool, why: &str, shown: &str| {
        if args.soft_set_bool_arg(arg, value) {
            logger.log_printf(&format!(
                "InitParameterInteraction: parameter interaction: {why} -> setting {arg}={shown}\n"
            ));
        }
    };

    // rdashd: an SPV node takes no inbound connections and keeps no blocks, so no transaction
    // index, and governance (which reads collateral through it) is not validated (SB0 P8). These
    // come first, so no soft-set of Core's turns listening on. Only once `-spv` is served (BL-679).
    if spv_served && args.get_bool_arg("-spv", false)? {
        soft_set("-listen", false, "-spv set", "0");
        soft_set("-txindex", false, "-spv set", "0");
        soft_set("-disablegovernance", true, "-spv set", "1");
    }

    // An explicit binding address listens even with -connect or -proxy.
    if args.is_arg_set("-bind") {
        soft_set("-listen", true, "-bind set", "1");
    }
    if args.is_arg_set("-whitebind") {
        soft_set("-listen", true, "-whitebind set", "1");
    }

    if args.is_arg_set("-connect")
        || args.get_int_arg("-maxconnections", DEFAULT_MAX_PEER_CONNECTIONS)? <= 0
    {
        soft_set("-dnsseed", false, "-connect or -maxconnections=0 set", "0");
        soft_set("-listen", false, "-connect or -maxconnections=0 set", "0");
    }

    let proxy = args.get_arg("-proxy", "")?;
    if !proxy.is_empty() && proxy != "0" {
        for arg in ["-listen", "-upnp", "-natpmp", "-discover"] {
            soft_set(arg, false, "-proxy set", "0");
        }
    }

    if !args.get_bool_arg("-listen", DEFAULT_LISTEN)? {
        for arg in ["-upnp", "-natpmp", "-discover", "-listenonion", "-i2pacceptincoming"] {
            soft_set(arg, false, "-listen=0", "0");
        }
    }

    if args.is_arg_set("-externalip") {
        soft_set("-discover", false, "-externalip set", "0");
    }

    if args.get_bool_arg("-blocksonly", DEFAULT_BLOCKSONLY)? {
        soft_set("-whitelistrelay", false, "-blocksonly=1", "0");
        let size = DEFAULT_BLOCKSONLY_MAX_MEMPOOL_SIZE_MB.to_string();
        if args.soft_set_arg("-maxmempool", &size) {
            logger.log_printf(&format!(
                "InitParameterInteraction: parameter interaction: -blocksonly=1 -> setting -maxmempool={size}\n"
            ));
        }
    }

    if args.get_bool_arg("-whitelistforcerelay", DEFAULT_WHITELISTFORCERELAY)? {
        soft_set("-whitelistrelay", true, "-whitelistforcerelay=1", "1");
    }

    if args.is_arg_set("-onlynet") {
        // `std::any_of` stops at the first clearnet network, as does `any`.
        let clearnet_reachable = args
            .get_args("-onlynet")?
            .iter()
            .any(|net| matches!(parse_network(logger, net), Network::Ipv4 | Network::Ipv6));
        if !clearnet_reachable {
            soft_set("-dnsseed", false, "-onlynet excludes IPv4 and IPv6", "0");
        }
    }

    let prune = args.get_int_arg("-prune", 0)?;
    if prune > 0 {
        let why = format!("-prune={prune}");
        soft_set("-disablegovernance", true, &why, "true");
        soft_set("-txindex", false, &why, "false");
    }

    if args.is_arg_set("-masternodeblsprivkey") {
        let why = "-masternodeblsprivkey set";
        soft_set("-disablewallet", true, why, "1");
        // Block filters for masternodes improve network services.
        soft_set("-peerblockfilters", true, why, "1");
        if args.soft_set_arg("-blockfilterindex", "basic") {
            logger.log_printf(&format!(
                "InitParameterInteraction: parameter interaction: {why} -> setting -blockfilterindex=basic\n"
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{TestArgs, test_args, test_logger};

    fn interact(argv: &[&str]) -> (TestArgs, Vec<String>) {
        let args = test_args(argv);
        let (logger, lines) = test_logger();
        init_parameter_interaction(&args, &logger).unwrap();
        let lines = lines.lock().unwrap().clone();
        (args, lines)
    }

    #[test]
    fn connect_disables_seeding_and_listening_and_what_follows() {
        let (args, lines) = interact(&["-connect=127.0.0.1"]);
        assert!(!args.get_bool_arg("-dnsseed", true).unwrap());
        assert!(!args.get_bool_arg("-listen", true).unwrap());
        assert!(!args.get_bool_arg("-discover", true).unwrap());
        let p = "InitParameterInteraction: parameter interaction: ";
        assert_eq!(
            lines,
            [
                format!("{p}-connect or -maxconnections=0 set -> setting -dnsseed=0\n"),
                format!("{p}-connect or -maxconnections=0 set -> setting -listen=0\n"),
                format!("{p}-listen=0 -> setting -upnp=0\n"),
                format!("{p}-listen=0 -> setting -natpmp=0\n"),
                format!("{p}-listen=0 -> setting -discover=0\n"),
                format!("{p}-listen=0 -> setting -listenonion=0\n"),
                format!("{p}-listen=0 -> setting -i2pacceptincoming=0\n"),
            ]
        );
    }

    #[test]
    fn explicit_settings_are_kept() {
        let (args, lines) = interact(&["-bind=127.0.0.1", "-connect=127.0.0.1", "-listen=0"]);
        assert!(!args.get_bool_arg("-listen", true).unwrap());
        assert!(lines.iter().all(|l| !l.contains("setting -listen=")), "{lines:?}");
    }

    /// Core's default relays transactions, so no argument soft-sets nothing (BL-330).
    // Core: net.h:124, init.cpp:1061-1068
    #[test]
    fn the_default_relays_and_soft_sets_nothing() {
        let (args, lines) = interact(&[]);
        assert!(!args.is_arg_set("-whitelistrelay"));
        assert!(!args.is_arg_set("-maxmempool"));
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// `-blocksonly` turns `-whitelistrelay` off and the mempool down to 5 MB, with Core's lines.
    // Core: init.cpp:1061-1068
    #[test]
    fn blocks_only_soft_sets_whitelistrelay_and_maxmempool() {
        let (args, lines) = interact(&["-blocksonly"]);
        assert!(!args.get_bool_arg("-whitelistrelay", true).unwrap());
        assert_eq!(args.get_arg("-maxmempool", "").unwrap(), "5");
        let p = "InitParameterInteraction: parameter interaction: -blocksonly=1 -> setting";
        assert_eq!(lines, [format!("{p} -whitelistrelay=0\n"), format!("{p} -maxmempool=5\n")]);
        let (args, lines) = interact(&["-blocksonly", "-maxmempool=50", "-whitelistrelay"]);
        assert_eq!(args.get_arg("-maxmempool", "").unwrap(), "50");
        assert!(args.get_bool_arg("-whitelistrelay", false).unwrap());
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn blocksonly_prune_and_masternode() {
        let (args, lines) = interact(&["-blocksonly", "-prune=550", "-masternodeblsprivkey=00"]);
        assert_eq!(args.get_arg("-maxmempool", "").unwrap(), "5");
        assert!(args.get_bool_arg("-disablegovernance", false).unwrap());
        assert!(!args.get_bool_arg("-txindex", true).unwrap());
        assert_eq!(args.get_arg("-blockfilterindex", "").unwrap(), "basic");
        assert!(
            lines.iter().any(|l| l.ends_with("-prune=550 -> setting -disablegovernance=true\n"))
        );
        assert!(lines.iter().any(|l| l.ends_with("-blocksonly=1 -> setting -maxmempool=5\n")));
    }

    #[test]
    fn onlynet_without_clearnet_disables_seeding_and_tor_is_deprecated() {
        let (args, lines) = interact(&["-onlynet=TOR"]);
        assert!(!args.get_bool_arg("-dnsseed", true).unwrap());
        assert!(lines[0].starts_with("Warning: net name 'tor' is deprecated"));
        let (args, _) = interact(&["-onlynet=onion", "-onlynet=IPv6"]);
        assert!(args.get_bool_arg("-dnsseed", true).unwrap());
    }

    /// Served `-spv` (SB18) turns off listening, the transaction index and governance before any
    /// of Core's soft-sets, so `-bind` turns listening on no more and Core's `-listen=0` soft-sets
    /// follow. None of them is the user's setting. Until served it soft-sets nothing (BL-679).
    #[test]
    fn spv_soft_sets_come_first_once_served() {
        let args = test_args(&["-spv", "-bind=127.0.0.1", "-connect=127.0.0.1"]);
        let (logger, lines) = test_logger();
        interaction_as(&args, &logger, true).unwrap();
        assert!(!args.get_bool_arg("-listen", true).unwrap());
        assert!(!args.get_bool_arg("-txindex", true).unwrap());
        assert!(args.get_bool_arg("-disablegovernance", false).unwrap());
        let p = "InitParameterInteraction: parameter interaction: ";
        let want: Vec<_> = [
            "-spv set -> setting -listen=0",
            "-spv set -> setting -txindex=0",
            "-spv set -> setting -disablegovernance=1",
            "-connect or -maxconnections=0 set -> setting -dnsseed=0",
            "-listen=0 -> setting -upnp=0",
            "-listen=0 -> setting -natpmp=0",
            "-listen=0 -> setting -discover=0",
            "-listen=0 -> setting -listenonion=0",
            "-listen=0 -> setting -i2pacceptincoming=0",
        ]
        .iter()
        .map(|l| format!("{p}{l}\n"))
        .collect();
        assert_eq!(*lines.lock().unwrap(), want);
        for soft in ["-listen", "-txindex", "-disablegovernance", "-dnsseed"] {
            assert!(args.is_arg_set(soft) && !args.is_arg_set_by_user(soft), "{soft}");
        }

        let (args, lines) = interact(&["-spv"]);
        assert!(!args.is_arg_set("-txindex"));
        assert!(!args.is_arg_set("-disablegovernance"));
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// `-spv -bind` is refused for `-bind`: the user did not set `-listen`, and `-spv` soft-sets
    /// it off before `-bind` would turn it on.
    #[test]
    fn spv_with_bind_gets_the_bind_refusal() {
        let args = test_args(&["-spv", "-bind=127.0.0.1"]);
        let Err(crate::InitFailure::Error(e)) = crate::init::spv_preempt_as(&args, true, true)
        else {
            panic!("-bind is refused");
        };
        assert_eq!(e, "-bind cannot be used with -spv: an SPV node takes no inbound connections.");
        let (logger, _) = test_logger();
        interaction_as(&args, &logger, true).unwrap();
        assert!(!args.get_bool_arg("-listen", true).unwrap());
        assert!(!args.is_arg_set_by_user("-listen"));
    }
}

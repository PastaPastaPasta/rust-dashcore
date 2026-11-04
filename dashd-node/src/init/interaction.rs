//! Core's `InitParameterInteraction`: options that soft-set others.

use dash_args::{ArgsManager, RuntimeError};

use super::consts::*;
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
    let soft_set = |arg: &str, value: bool, why: &str, shown: &str| {
        if args.soft_set_bool_arg(arg, value) {
            logger.log_printf(&format!(
                "InitParameterInteraction: parameter interaction: {why} -> setting {arg}={shown}\n"
            ));
        }
    };

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

    // rdashd: an SPV node keeps no blocks, so no transaction index, and governance (which reads
    // collateral through it) is not validated (SB0 P8).
    if args.get_bool_arg("-spv", false)? {
        soft_set("-txindex", false, "-spv set", "0");
        soft_set("-disablegovernance", true, "-spv set", "1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{TestArgs, test_args, test_logger};

    /// `argv` with transaction relay on: rdashd's default `-blocksonly=1` (DEC-322) adds its
    /// soft-sets to every run, which [`blocks_only_is_the_default_until_p5`] checks once.
    fn interact(argv: &[&str]) -> (TestArgs, Vec<String>) {
        interact_as_is(&[argv, &["-blocksonly=0"]].concat())
    }

    fn interact_as_is(argv: &[&str]) -> (TestArgs, Vec<String>) {
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

    /// dashd's `-blocksonly=1` interaction, with no argument (P5a D23).
    // Core: init.cpp:1058-1065
    #[test]
    fn blocks_only_is_the_default_until_p5() {
        let (args, lines) = interact_as_is(&[]);
        assert!(!args.get_bool_arg("-whitelistrelay", true).unwrap());
        assert_eq!(args.get_arg("-maxmempool", "").unwrap(), "5");
        let p = "InitParameterInteraction: parameter interaction: -blocksonly=1 -> setting";
        assert_eq!(lines, [format!("{p} -whitelistrelay=0\n"), format!("{p} -maxmempool=5\n")]);
    }

    #[test]
    fn blocksonly_prune_and_masternode() {
        let (args, lines) =
            interact_as_is(&["-blocksonly", "-prune=550", "-masternodeblsprivkey=00"]);
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

    #[test]
    fn spv_turns_off_the_transaction_index_and_governance() {
        let (args, lines) = interact(&["-spv"]);
        assert!(!args.get_bool_arg("-txindex", true).unwrap());
        assert!(args.get_bool_arg("-disablegovernance", false).unwrap());
        assert_eq!(
            lines[0],
            "InitParameterInteraction: parameter interaction: -spv set -> setting -txindex=0\n"
        );
    }
}

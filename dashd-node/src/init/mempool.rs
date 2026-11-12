//! Core's mempool options from the arguments (`node/mempool_args.cpp`) and the relay-policy
//! comparison a masternode is held to.

use dash_args::{ArgsManager, RuntimeError};
use dash_params::ChainSetup;

use super::consts::*;
use super::parse::{amount_err_msg, fee_rate_string, parse_money};
use crate::logging::Logger;

/// Core's `kernel::MemPoolLimits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemPoolLimits {
    pub ancestor_count: i64,
    pub ancestor_size_vbytes: i64,
    pub descendant_count: i64,
    pub descendant_size_vbytes: i64,
}

impl Default for MemPoolLimits {
    fn default() -> MemPoolLimits {
        MemPoolLimits {
            ancestor_count: DEFAULT_ANCESTOR_LIMIT,
            ancestor_size_vbytes: DEFAULT_ANCESTOR_SIZE_LIMIT_KVB * 1_000,
            descendant_count: DEFAULT_DESCENDANT_LIMIT,
            descendant_size_vbytes: DEFAULT_DESCENDANT_SIZE_LIMIT_KVB * 1_000,
        }
    }
}

/// Core's `kernel::MemPoolOptions`, without the component pointers. Fee rates are in duffs per
/// kB (`CFeeRate`); the integer types are Core's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemPoolOptions {
    pub check_ratio: i32,
    pub max_size_bytes: i64,
    pub expiry_secs: i64,
    pub incremental_relay_feerate: i64,
    pub min_relay_feerate: i64,
    pub dust_relay_feerate: i64,
    pub max_datacarrier_bytes: Option<u32>,
    pub permit_bare_multisig: bool,
    pub require_standard: bool,
    pub address_index_enabled: bool,
    pub spent_index_enabled: bool,
    pub limits: MemPoolLimits,
}

impl Default for MemPoolOptions {
    fn default() -> MemPoolOptions {
        MemPoolOptions {
            check_ratio: 0,
            max_size_bytes: DEFAULT_MAX_MEMPOOL_SIZE_MB * 1_000_000,
            expiry_secs: DEFAULT_MEMPOOL_EXPIRY_HOURS * 3600,
            incremental_relay_feerate: DEFAULT_INCREMENTAL_RELAY_FEE,
            min_relay_feerate: DEFAULT_MIN_RELAY_TX_FEE,
            dust_relay_feerate: DUST_RELAY_TX_FEE,
            max_datacarrier_bytes: DEFAULT_ACCEPT_DATACARRIER.then_some(MAX_OP_RETURN_RELAY as u32),
            permit_bare_multisig: DEFAULT_PERMIT_BAREMULTISIG,
            require_standard: true,
            address_index_enabled: false,
            spent_index_enabled: false,
            limits: MemPoolLimits::default(),
        }
    }
}

/// A fee option: Core's `ParseMoney` or its `AmountErrMsg`.
fn fee_arg(args: &ArgsManager, name: &str) -> Result<Option<Result<i64, String>>, RuntimeError> {
    if !args.is_arg_set(&format!("-{name}")) {
        return Ok(None);
    }
    let value = args.get_arg(&format!("-{name}"), "")?;
    Ok(Some(parse_money(&value).ok_or_else(|| amount_err_msg(name, &value))))
}

/// Core's `ApplyArgsManOptions(argsman, chainparams, mempool_opts)`: the outer error is an
/// argument exception, the inner one the `bilingual_str` Core returns.
// Core: node/mempool_args.cpp:43-104
pub fn apply_args_man_options(
    args: &ArgsManager,
    chain: &ChainSetup,
    logger: &Logger,
    opts: &mut MemPoolOptions,
) -> Result<Result<(), String>, RuntimeError> {
    // Core's narrowing and (wrapping) overflow, as its `int`/`int64_t`/`unsigned` fields have.
    opts.check_ratio = args.get_int_arg("-checkmempool", i64::from(opts.check_ratio))? as i32;
    if let Some(mb) = args.get_int_arg_opt("-maxmempool")? {
        opts.max_size_bytes = mb.wrapping_mul(1_000_000);
    }
    if let Some(hours) = args.get_int_arg_opt("-mempoolexpiry")? {
        opts.expiry_secs = hours.wrapping_mul(3600);
    }

    // The incremental relay fee is how far the mempool's minimum fee rises above the fee rate
    // of transactions the size limit evicts.
    match fee_arg(args, "incrementalrelayfee")? {
        Some(Ok(fee)) => opts.incremental_relay_feerate = fee,
        Some(Err(e)) => return Ok(Err(e)),
        None => {}
    }
    match fee_arg(args, "minrelaytxfee")? {
        Some(Ok(fee)) => opts.min_relay_feerate = fee,
        Some(Err(e)) => return Ok(Err(e)),
        // The incremental fee alone sets both.
        None if opts.incremental_relay_feerate > opts.min_relay_feerate => {
            opts.min_relay_feerate = opts.incremental_relay_feerate;
            logger.log_printf(&format!(
                "Increasing minrelaytxfee to {} to match incrementalrelayfee\n",
                fee_rate_string(opts.min_relay_feerate)
            ));
        }
        None => {}
    }
    match fee_arg(args, "dustrelayfee")? {
        Some(Ok(fee)) => opts.dust_relay_feerate = fee,
        Some(Err(e)) => return Ok(Err(e)),
        None => {}
    }

    opts.permit_bare_multisig =
        args.get_bool_arg("-permitbaremultisig", DEFAULT_PERMIT_BAREMULTISIG)?;
    opts.max_datacarrier_bytes = if args.get_bool_arg("-datacarrier", DEFAULT_ACCEPT_DATACARRIER)? {
        Some(args.get_int_arg("-datacarriersize", MAX_OP_RETURN_RELAY)? as u32)
    } else {
        None
    };

    opts.require_standard = !args.get_bool_arg("-acceptnonstdtxn", DEFAULT_ACCEPT_NON_STD_TXN)?;
    if !chain.is_test_chain && !opts.require_standard {
        return Ok(Err(format!(
            "acceptnonstdtxn is not currently supported for {} chain",
            chain.network.network_id()
        )));
    }

    let limits = &mut opts.limits;
    limits.ancestor_count = args.get_int_arg("-limitancestorcount", limits.ancestor_count)?;
    if let Some(vkb) = args.get_int_arg_opt("-limitancestorsize")? {
        limits.ancestor_size_vbytes = vkb.wrapping_mul(1_000);
    }
    limits.descendant_count = args.get_int_arg("-limitdescendantcount", limits.descendant_count)?;
    if let Some(vkb) = args.get_int_arg_opt("-limitdescendantsize")? {
        limits.descendant_size_vbytes = vkb.wrapping_mul(1_000);
    }

    opts.address_index_enabled = args.get_bool_arg("-addressindex", DEFAULT_ADDRESSINDEX)?;
    opts.spent_index_enabled = args.get_bool_arg("-spentindex", DEFAULT_SPENTINDEX)?;
    Ok(Ok(()))
}

/// Core's `GetStricterThanDefaultRelayPolicy`: the options that make `opts` relay less than the
/// defaults do.
// Core: node/mempool_args.cpp:106-133
pub fn stricter_than_default_relay_policy(opts: &MemPoolOptions) -> Vec<&'static str> {
    let d = MemPoolOptions::default();
    let mut stricter = Vec::new();
    // -incrementalrelayfee also raises the minimum relay fee unless -minrelaytxfee is set.
    if opts.incremental_relay_feerate > d.incremental_relay_feerate {
        stricter.push("-incrementalrelayfee");
    } else if opts.min_relay_feerate > d.min_relay_feerate {
        stricter.push("-minrelaytxfee");
    }
    if opts.dust_relay_feerate > d.dust_relay_feerate {
        stricter.push("-dustrelayfee");
    }
    if opts.max_datacarrier_bytes.unwrap_or(0) < d.max_datacarrier_bytes.unwrap_or(0) {
        stricter.push("-datacarrier/-datacarriersize");
    }
    if !opts.permit_bare_multisig && d.permit_bare_multisig {
        stricter.push("-permitbaremultisig");
    }
    let (l, dl) = (&opts.limits, &d.limits);
    if l.ancestor_count < dl.ancestor_count {
        stricter.push("-limitancestorcount");
    }
    if l.ancestor_size_vbytes < dl.ancestor_size_vbytes {
        stricter.push("-limitancestorsize");
    }
    if l.descendant_count < dl.descendant_count {
        stricter.push("-limitdescendantcount");
    }
    if l.descendant_size_vbytes < dl.descendant_size_vbytes {
        stricter.push("-limitdescendantsize");
    }
    // A full smaller mempool evicts what a default-sized one keeps and raises its fee floor.
    if opts.max_size_bytes < d.max_size_bytes {
        stricter.push("-maxmempool");
    }
    if opts.expiry_secs < d.expiry_secs {
        stricter.push("-mempoolexpiry");
    }
    stricter
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{select_params, test_args, test_logger};

    fn apply(argv: &[&str]) -> Result<MemPoolOptions, String> {
        let args = test_args(argv);
        let chain = select_params(&args, &args.get_chain_name().unwrap()).unwrap().chain;
        let (logger, _) = test_logger();
        let mut opts = MemPoolOptions::default();
        apply_args_man_options(&args, &chain, &logger, &mut opts).unwrap().map(|()| opts)
    }

    fn stricter_for(argv: &[&str]) -> Vec<&'static str> {
        stricter_than_default_relay_policy(&apply(argv).unwrap())
    }

    // Core v24.0.0-rc.3: src/test/masternode_params_tests.cpp (on mainnet, as
    // `BasicTestingSetup`).
    #[test]
    fn default_relay_policy_is_not_stricter() {
        assert!(stricter_for(&["-chain=main"]).is_empty());
    }

    #[test]
    fn looser_relay_policy_is_not_stricter() {
        let looser = [
            "-chain=main",
            "-minrelaytxfee=0.000001",
            "-limitancestorcount=50",
            "-datacarriersize=160",
            "-maxmempool=301",
            "-mempoolexpiry=337",
        ];
        assert!(stricter_for(&looser).is_empty());
    }

    #[test]
    fn each_stricter_relay_option_is_reported() {
        for (arg, reported) in [
            ("-minrelaytxfee=0.001", "-minrelaytxfee"),
            ("-incrementalrelayfee=0.001", "-incrementalrelayfee"),
            ("-dustrelayfee=0.001", "-dustrelayfee"),
            ("-datacarrier=0", "-datacarrier/-datacarriersize"),
            // An asset lock carries a two-byte OP_RETURN, which a smaller limit makes
            // nonstandard.
            ("-datacarriersize=1", "-datacarrier/-datacarriersize"),
            ("-permitbaremultisig=0", "-permitbaremultisig"),
            ("-limitancestorcount=5", "-limitancestorcount"),
            ("-limitancestorsize=50", "-limitancestorsize"),
            ("-limitdescendantcount=5", "-limitdescendantcount"),
            ("-limitdescendantsize=50", "-limitdescendantsize"),
            ("-maxmempool=299", "-maxmempool"),
            ("-mempoolexpiry=335", "-mempoolexpiry"),
            ("-mempoolexpiry=-1", "-mempoolexpiry"),
        ] {
            assert_eq!(stricter_for(&["-chain=main", arg]), [reported], "{arg}");
        }
    }

    #[test]
    fn errors_and_the_incremental_fee_raise() {
        assert_eq!(
            apply(&["-dustrelayfee=x"]),
            Err("Invalid amount for -dustrelayfee=<amount>: 'x'".to_owned())
        );
        assert_eq!(
            apply(&["-chain=main", "-acceptnonstdtxn"]),
            Err("acceptnonstdtxn is not currently supported for main chain".to_owned())
        );
        assert!(apply(&["-acceptnonstdtxn"]).is_ok());

        let args = test_args(&["-incrementalrelayfee=0.0001"]);
        let chain = select_params(&args, "regtest").unwrap().chain;
        let (logger, lines) = test_logger();
        let mut opts = MemPoolOptions::default();
        apply_args_man_options(&args, &chain, &logger, &mut opts).unwrap().unwrap();
        assert_eq!(opts.min_relay_feerate, 10_000);
        assert_eq!(
            *lines.lock().unwrap(),
            ["Increasing minrelaytxfee to 0.00010000 DASH/kB to match incrementalrelayfee\n"]
        );
    }
}

//! Core's `SelectParams`: the chain parameters with the command-line overrides applied.

use dash_args::{ArgsManager, RuntimeError};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, ParamsError, Release};

/// The Core release whose parameters rdashd runs.
pub const RELEASE: Release = Release::V24_0;

/// The selected chain parameters.
#[derive(Clone, Debug)]
pub struct SelectedParams {
    pub chain: ChainSetup,
    /// `-sporkaddr`'s refusal. Core checks the spork address in `AppInitMain`, after every
    /// parameter-interaction error, so `chain` holds the default signer and the error waits for
    /// that step.
    pub spork_error: Option<String>,
}

/// Core's `SelectParams(chain)`: selects the base parameters, then builds the chain's. The error
/// is the `what()` of Core's exception.
// Core: chainparams.cpp `SelectParams`, `CreateChainParams`
pub fn select_params(args: &ArgsManager, chain: &str) -> Result<SelectedParams, RuntimeError> {
    args.select_base_params(chain)?;
    let network = Network::from_network_id(chain)
        .ok_or_else(|| RuntimeError(format!("CreateChainParams: Unknown chain {chain}.")))?;
    let builder = builder(args, network, true)?;
    match builder.build() {
        Ok(chain) => Ok(SelectedParams {
            chain,
            spork_error: None,
        }),
        Err(e @ ParamsError::SporkAddress) => {
            let chain = self::builder(args, network, false)?.build().map_err(params_error)?;
            Ok(SelectedParams {
                chain,
                spork_error: Some(e.to_string()),
            })
        }
        Err(e) => Err(params_error(e)),
    }
}

fn params_error(e: ParamsError) -> RuntimeError {
    RuntimeError(e.to_string())
}

/// The builder with `args`' resolved settings: one occurrence for a single-valued option, the
/// list for a repeatable one.
fn builder(
    args: &ArgsManager,
    network: Network,
    with_spork_addr: bool,
) -> Result<ChainParamsBuilder, RuntimeError> {
    let mut b = ChainParamsBuilder::new(network, RELEASE);
    for option in ChainOption::ALL {
        let name = option.name();
        if !args.is_arg_set(name) {
            continue;
        }
        if args.is_arg_negated(name) {
            b = b.negate(option);
            continue;
        }
        match option {
            ChainOption::TestActivationHeight | ChainOption::VbParams => {
                for value in args.get_args(name)? {
                    b = b.arg(option, value);
                }
            }
            // v24 reads one address (`GetArg`) and warns when `GetArgs` holds more: pass the
            // others first so the count holds and the last is `GetArg`'s.
            ChainOption::SporkAddr if !with_spork_addr => {}
            ChainOption::SporkAddr => {
                let mut values = args.get_args(name)?;
                let chosen = args.get_arg(name, "")?;
                if let Some(i) = values.iter().position(|v| *v == chosen) {
                    values.remove(i);
                }
                for value in values {
                    b = b.arg(option, value);
                }
                b = b.arg(option, chosen);
            }
            _ => {
                if let Some(value) = args.get_arg_opt(name)? {
                    b = b.arg(option, value);
                }
            }
        }
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::test_args;

    #[test]
    fn regtest_overrides_reach_the_parameters() {
        let args =
            test_args(&["-regtest", "-dip3params=200:300", "-testactivationheight=dip0001@50"]);
        let p = select_params(&args, "regtest").unwrap();
        assert_eq!(p.chain.network, Network::Regtest);
        assert_eq!(p.chain.consensus.dip0003_height.to_u32(), 200);
        assert_eq!(p.chain.consensus.dip0001_height.to_u32(), 50);
        assert!(p.spork_error.is_none());
    }

    #[test]
    fn a_chain_error_is_core_s_exception_text() {
        let args = test_args(&["-regtest", "-dip3params=1"]);
        let e = select_params(&args, "regtest").unwrap_err();
        assert_eq!(e.0, "DIP3 parameters malformed, expecting <activation>:<enforcement>");
    }

    #[test]
    fn the_spork_address_error_waits_for_app_init_main() {
        let args = test_args(&["-regtest", "-sporkaddr=nonsense"]);
        let p = select_params(&args, "regtest").unwrap();
        assert_eq!(
            p.spork_error.as_deref(),
            Some("Invalid spork address specified with -sporkaddr")
        );
        assert_eq!(p.chain.spork_signers.key_ids.len(), 1);
    }

    #[test]
    fn several_spork_addresses_warn_and_use_get_arg_s() {
        let default = select_params(&test_args(&["-regtest"]), "regtest").unwrap();
        let address = default.chain.spork_address;
        let args = test_args(&[
            "-regtest",
            &format!("-sporkaddr={address}"),
            &format!("-sporkaddr={address}"),
        ]);
        let p = select_params(&args, "regtest").unwrap();
        assert_eq!(
            p.chain.spork_signers.warning,
            Some(format!("More than one spork address is provided. Using {address}."))
        );
    }
}

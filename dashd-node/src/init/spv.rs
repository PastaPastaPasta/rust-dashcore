//! rdashd's `-spv` refusals, raised first in init: an SPV node keeps no blocks, chainstate or
//! index, takes no inbound connections and serves no data.

use dash_args::ArgsManager;

use super::checks::blockfilter_index_requested;
use super::support::served;
use crate::InitFailure;

fn error(msg: impl Into<String>) -> InitFailure {
    InitFailure::Error(msg.into())
}

/// When a [`Row`] refuses: the user set the option, and
#[derive(Clone, Copy)]
enum When {
    /// to true;
    On,
    /// to a number other than 0;
    NonZero,
    /// to any value but a negation.
    Set,
}

/// One refusal of [`SPV_PREEMPT_ROWS`]: `<option> cannot be used with -spv: <why>`.
struct Row {
    option: &'static str,
    when: When,
    why: &'static str,
}

const NO_CHAINSTATE: &str = "an SPV node keeps no blocks or chainstate.";
const NO_INBOUND: &str = "an SPV node takes no inbound connections.";
const SERVES_NOTHING: &str = "an SPV node serves no data.";

/// The refusals after the wallet, masternode, governance and index checks, in the order they
/// are raised.
#[rustfmt::skip]
const SPV_PREEMPT_ROWS: &[Row] = &[
    Row { option: "-reindex", when: When::On, why: NO_CHAINSTATE },
    Row { option: "-reindex-chainstate", when: When::On, why: NO_CHAINSTATE },
    Row { option: "-prune", when: When::NonZero, why: NO_CHAINSTATE },
    Row { option: "-listen", when: When::On, why: NO_INBOUND },
    Row { option: "-listenonion", when: When::On, why: NO_INBOUND },
    Row { option: "-bind", when: When::Set, why: NO_INBOUND },
    Row { option: "-whitebind", when: When::Set, why: NO_INBOUND },
    Row { option: "-externalip", when: When::Set, why: NO_INBOUND },
    Row { option: "-peerblockfilters", when: When::On, why: SERVES_NOTHING },
    Row { option: "-peerbloomfilters", when: When::On, why: SERVES_NOTHING },
];

impl Row {
    fn check(&self, args: &ArgsManager) -> Result<(), InitFailure> {
        if !args.is_arg_set_by_user(self.option) {
            return Ok(());
        }
        let refused = match self.when {
            When::On => args.get_bool_arg(self.option, false)?,
            When::NonZero => args.get_int_arg(self.option, 0)? != 0,
            When::Set => !args.is_arg_negated(self.option),
        };
        if refused {
            return Err(error(format!("{} cannot be used with -spv: {}", self.option, self.why)));
        }
        Ok(())
    }
}

fn index_refusal(index: &str) -> InitFailure {
    error(format!("-{index} cannot be used with -spv: an SPV node keeps no blocks or undo data."))
}

/// The SPV refusals, raised before the support table's and every Core check, so none meets a
/// generic refusal or a Core text first. The first failing check wins. It runs before every
/// soft-set, so each check reads the user's own value, never a default a later soft-set changes.
pub fn spv_preempt(args: &ArgsManager) -> Result<(), InitFailure> {
    spv_preempt_as(args, served(args, "-spv"), served(args, "-spvwallet"))
}

/// [`spv_preempt`] as if `-spv` and `-spvwallet` were served or not.
pub(crate) fn spv_preempt_as(
    args: &ArgsManager,
    spv_served: bool,
    spvwallet_served: bool,
) -> Result<(), InitFailure> {
    let user = |o: &str| args.is_arg_set_by_user(o) && !args.is_arg_negated(o);
    let on = |o: &str| -> Result<bool, InitFailure> {
        Ok(args.is_arg_set_by_user(o) && args.get_bool_arg(o, false)?)
    };
    let spv = spv_served && args.get_bool_arg("-spv", false)?;
    if spvwallet_served && user("-spvwallet") {
        if !spv {
            return Err(error("-spvwallet requires -spv."));
        }
        if args.get_args("-spvwallet")?.len() > 1 {
            return Err(error("-spvwallet can be given only once with -spv (SB18b)."));
        }
        if on("-disablewallet")? {
            return Err(error("-spvwallet cannot be used with -disablewallet."));
        }
    }
    if !spv {
        return Ok(());
    }
    // A negation counts, as it does for the masternode soft-sets and checks that follow.
    if args.is_arg_set_by_user("-masternodeblsprivkey") {
        return Err(error(
            "-masternodeblsprivkey cannot be used with -spv: a masternode must run a full node.",
        ));
    }
    if args.is_arg_set_by_user("-disablegovernance")
        && !args.get_bool_arg("-disablegovernance", true)?
    {
        return Err(error(
            "-disablegovernance=0 cannot be used with -spv: governance needs a full node.",
        ));
    }
    // `-txindex` defaults to on, and `-spv` soft-sets it off later: only the user's value counts.
    for index in ["txindex", "addressindex", "spentindex", "timestampindex", "coinstatsindex"] {
        if on(&format!("-{index}"))? {
            return Err(index_refusal(index));
        }
    }
    if blockfilter_index_requested(args)? {
        return Err(index_refusal("blockfilterindex"));
    }
    for row in SPV_PREEMPT_ROWS {
        row.check(args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{TestArgs, test_args, test_args_with_conf};

    fn preempt(argv: &[&str]) -> Result<(), String> {
        preempt_args(&test_args(argv))
    }

    fn preempt_args(args: &TestArgs) -> Result<(), String> {
        match spv_preempt_as(args, true, true) {
            Ok(()) => Ok(()),
            Err(InitFailure::Error(e)) => Err(e),
            Err(other) => panic!("{other:?}"),
        }
    }

    fn refusal(option: &str, why: &str) -> String {
        format!("{option} cannot be used with -spv: {why}")
    }

    /// Each refused option with a value that refuses it, and the refusal, in raising order.
    fn ordered_cases() -> Vec<(String, String)> {
        let mut cases = vec![
            (
                "-masternodeblsprivkey=00".to_owned(),
                refusal("-masternodeblsprivkey", "a masternode must run a full node."),
            ),
            (
                "-disablegovernance=0".to_owned(),
                refusal("-disablegovernance=0", "governance needs a full node."),
            ),
        ];
        for index in [
            "txindex",
            "addressindex",
            "spentindex",
            "timestampindex",
            "coinstatsindex",
            "blockfilterindex",
        ] {
            let why = "an SPV node keeps no blocks or undo data.";
            cases.push((format!("-{index}=1"), refusal(&format!("-{index}"), why)));
        }
        for row in SPV_PREEMPT_ROWS {
            let value = match row.when {
                When::On => "1",
                When::NonZero => "550",
                When::Set => "127.0.0.1",
            };
            cases.push((format!("{}={value}", row.option), refusal(row.option, row.why)));
        }
        cases
    }

    /// Every row's exact text, on regtest and testnet.
    #[test]
    fn each_row_refuses_with_its_text() {
        for chain in ["-regtest", "-testnet"] {
            for (arg, text) in ordered_cases() {
                assert_eq!(preempt(&[chain, "-spv", &arg]), Err(text), "{chain} {arg}");
            }
        }
    }

    /// For each pair of rows the earlier one wins, whatever the order on the command line.
    #[test]
    fn the_earlier_row_wins() {
        let cases = ordered_cases();
        for (i, (first, text)) in cases.iter().enumerate() {
            for (later, _) in &cases[i + 1..] {
                for argv in [["-spv", first, later], ["-spv", later, first]] {
                    assert_eq!(preempt(&argv), Err(text.clone()), "{argv:?}");
                }
            }
        }
    }

    /// No row reads a default that a later soft-set changes: `-txindex` defaults to on, and a
    /// soft-set `-txindex=1` is not the user's.
    #[test]
    fn defaults_and_soft_sets_pass() {
        assert_eq!(preempt(&["-spv"]), Ok(()));
        assert_eq!(preempt(&["-testnet", "-spv"]), Ok(()));
        assert_eq!(preempt(&["-spv", "-txindex=0"]), Ok(()));
        let args = test_args(&["-spv"]);
        assert!(args.soft_set_arg("-txindex", "1"));
        assert!(args.soft_set_arg("-listen", "1"));
        assert_eq!(preempt_args(&args), Ok(()));
    }

    /// The value that turns an option off passes, and so does a negation.
    #[test]
    fn values_that_turn_an_option_off_pass() {
        let off = [
            "-disablegovernance=1",
            "-reindex=0",
            "-prune=0",
            "-listen=0",
            "-nolisten",
            "-listenonion=0",
            "-nobind",
            "-noexternalip",
            "-peerblockfilters=0",
            "-peerbloomfilters=0",
            "-blockfilterindex=0",
        ];
        for arg in off {
            assert_eq!(preempt(&["-spv", arg]), Ok(()), "{arg}");
        }
    }

    /// A negated masternode key is set, as every later masternode check reads it, so it gets
    /// row 2's text rather than Core's masternode checks.
    #[test]
    fn a_negated_masternode_key_is_refused() {
        let text = refusal("-masternodeblsprivkey", "a masternode must run a full node.");
        for chain in ["-regtest", "-testnet"] {
            let refused = preempt(&[chain, "-spv", "-nomasternodeblsprivkey"]);
            assert_eq!(refused, Err(text.clone()), "{chain}");
        }
    }

    /// A setting in the config file is the user's too.
    #[test]
    fn a_config_file_setting_is_refused() {
        let args = test_args_with_conf(&["-spv"], Some("txindex=1\n"));
        let text = refusal("-txindex", "an SPV node keeps no blocks or undo data.");
        assert_eq!(preempt_args(&args), Err(text));
    }

    /// `-spvwallet` needs `-spv`, is given once and refuses a disabled wallet, before every
    /// other row; `-nospvwallet` is no wallet.
    #[test]
    fn spvwallet_rows_come_first() {
        let cases: [(&[&str], &str); 5] = [
            (&["-spvwallet=w.txt"], "-spvwallet requires -spv."),
            (&["-spv=0", "-spvwallet=w.txt"], "-spvwallet requires -spv."),
            (
                &["-spv", "-spvwallet=a.txt", "-spvwallet=b.txt"],
                "-spvwallet can be given only once with -spv (SB18b).",
            ),
            (
                &["-spv", "-spvwallet=a.txt", "-spvwallet=b.txt", "-disablewallet"],
                "-spvwallet can be given only once with -spv (SB18b).",
            ),
            (
                &["-spv", "-spvwallet=w.txt", "-disablewallet", "-masternodeblsprivkey=00"],
                "-spvwallet cannot be used with -disablewallet.",
            ),
        ];
        for (argv, text) in cases {
            assert_eq!(preempt(argv), Err(text.to_owned()), "{argv:?}");
        }
        for argv in [
            &["-nospvwallet"][..],
            &["-spv", "-nospvwallet"],
            &["-spv", "-spvwallet=w.txt"],
            &["-spv", "-spvwallet=w.txt", "-disablewallet=0"],
        ] {
            assert_eq!(preempt(argv), Ok(()), "{argv:?}");
        }
        assert!(spv_preempt(&test_args(&["-spvwallet=w.txt"])).is_ok(), "unserved");
    }

    /// Until `-spv` is served, nothing is refused here: the support table refuses `-spv` itself.
    #[test]
    fn unserved_spv_refuses_nothing() {
        for (arg, _) in ordered_cases() {
            let args = test_args(&["-spv", &arg]);
            assert!(spv_preempt(&args).is_ok(), "{arg}");
            assert!(spv_preempt_as(&args, false, false).is_ok(), "{arg}");
        }
        assert!(spv_preempt_as(&test_args(&["-txindex=1"]), true, true).is_ok(), "no -spv");
        assert!(spv_preempt_as(&test_args(&["-spv=0", "-txindex=1"]), true, true).is_ok());
    }
}

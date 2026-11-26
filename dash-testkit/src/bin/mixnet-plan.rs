//! `mixnet-plan`: prints O12b's role plan for a preset, caps set and context.
//!
//! ```text
//! mixnet-plan --preset <preset> --caps <none|cap,...> --context <regtest|devnet>
//! ```
//!
//! It prints the [`RolePlan`] as pretty JSON on stdout and exits 0. Bad or repeated arguments, or
//! a preset of the other context, exit 2 with a message on stderr and print nothing; if stdout
//! closes early it exits 1.
//! `dash-testkit/mixnet/plan-*.json` are examples of its output.

use std::fmt;
use std::io::{self, Write};
use std::process::exit;
use std::str::FromStr;

use dash_testkit::mixnet::contract::{Caps, Context, Preset, RolePlan};

const USAGE: &str = "usage: mixnet-plan --preset <modeb5|alt5|alt8|allrust:<R>:<E>|devnet|ratio1:3|ratio3:1> \
                     --caps <none|mn,sig,lock,qdata,gbt,dkg,gov,cj> --context <regtest|devnet>";

fn fail(message: &str) -> ! {
    eprintln!("mixnet-plan: {message}\n{USAGE}");
    exit(2);
}

fn arg<T: FromStr<Err: fmt::Display>>(value: Option<String>, flag: &str) -> T {
    let value = value.unwrap_or_else(|| fail(&format!("{flag} is required")));
    value.parse::<T>().unwrap_or_else(|e| fail(&e.to_string()))
}

fn main() {
    let (mut preset, mut caps, mut context) = (None, None, None);
    let mut args = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string().unwrap_or_else(|_| fail("an argument is not UTF-8")));
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--preset" => &mut preset,
            "--caps" => &mut caps,
            "--context" => &mut context,
            _ => fail(&format!("unknown argument {flag:?}")),
        };
        let Some(value) = args.next() else {
            fail(&format!("{flag} needs a value"))
        };
        if slot.replace(value).is_some() {
            fail(&format!("{flag} is given twice"));
        }
    }
    let preset: Preset = arg(preset, "--preset");
    let caps: Caps = arg(caps, "--caps");
    let context: Context = arg(context, "--context");
    let plan = RolePlan::new(preset, caps, context).unwrap_or_else(|e| fail(&e.to_string()));
    let json = serde_json::to_string_pretty(&plan).expect("a role plan serializes");
    if writeln!(io::stdout().lock(), "{json}").is_err() {
        exit(1);
    }
}

//! Dash Core v24's RPC interface as data (A1 §2.17).
//!
//! [`COMMANDS`] holds every command Dash Core v24.0.0-rc.4 registers: its name (Dash's two-word
//! sub-commands such as `protx register` included), category, wallet gating, whitelist name,
//! declared arguments, PRD §5 class and Core's help text verbatim. [`CONVERT_PARAMS`] is
//! `vRPCConvertParams` of Core's `src/rpc/client.cpp`, the table dash-cli uses to decide which
//! command-line arguments are parsed as JSON.
//!
//! The table is generated from Core by `generator/generate.py` and never edited by hand; see
//! `PROVENANCE.md`. [`help`] renders Core's `help` RPC from it.

#![forbid(unsafe_code)]

mod generated;

pub use generated::{COMMANDS, CONVERT_PARAMS, CORE_VERSION};

/// `RPCArg::Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgType {
    Obj,
    Arr,
    Str,
    Num,
    Bool,
    /// An object whose keys the user chooses (for example an address → amount map).
    ObjUserKeys,
    /// A number or a string holding a decimal amount.
    Amount,
    /// A string of hex characters.
    StrHex,
    /// A number or a `[begin, end]` pair.
    Range,
}

/// `RPCArg::Fallback`: what an argument means when the caller leaves it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fallback {
    /// `RPCArg::Optional::NO`.
    Required,
    /// `RPCArg::Optional::OMITTED`: optional, with no default shown.
    Omitted,
    /// `RPCArg::DefaultHint`: optional; the default is described by this text.
    DefaultHint(&'static str),
    /// `RPCArg::Default`: optional; the default value, as JSON.
    Default(&'static str),
}

impl Fallback {
    /// `RPCArg::IsOptional`.
    pub fn is_optional(self) -> bool {
        self != Fallback::Required
    }
}

/// One declared argument of a command (`RPCArg`).
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct RpcArg {
    /// Core's `m_names`: the name, with any aliases accepted for named arguments after `|`
    /// (`"conf_target|estimate_mode"` style). Empty for some inner arguments.
    pub names: &'static str,
    pub ty: ArgType,
    pub fallback: Fallback,
    /// The argument skips the server's JSON type check (it takes a string or a number, or
    /// the handler checks it itself).
    pub skip_type_check: bool,
    /// Left out of the help text (test-only arguments). Every later argument is hidden too.
    pub hidden: bool,
    /// Fields of an object or elements of an array.
    pub inner: &'static [RpcArg],
}

impl RpcArg {
    /// `RPCArg::GetFirstName`: the name shown in help.
    pub fn first_name(&self) -> &'static str {
        self.names.split_once('|').map_or(self.names, |(first, _)| first)
    }

    /// The name and its aliases, each accepted as a named argument.
    pub fn aliases(&self) -> impl Iterator<Item = &'static str> {
        self.names.split('|')
    }
}

/// When a command is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gating {
    /// Always.
    Always,
    /// Only when the wallet is enabled (not under `-disablewallet`).
    Wallet,
    /// Always, by the wallet when it is enabled and by the node otherwise (`protx list`,
    /// `protx info`, `getcoinjoininfo`): the handler is the same, it uses the wallet if any.
    Hybrid,
}

impl Gating {
    /// Whether the command is registered when the wallet is enabled or not.
    pub fn registered(self, wallet_enabled: bool) -> bool {
        wallet_enabled || self != Gating::Wallet
    }
}

/// The PRD §5 class of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Must,
    Should,
    Wont,
}

/// One registered RPC command.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct RpcCommand {
    /// The method name; Dash's sub-commands are `"<method> <subcommand>"`.
    pub name: &'static str,
    /// Core's category; `"hidden"` commands are left out of the `help` overview.
    pub category: &'static str,
    /// The name `-rpcwhitelist` matches: the method and sub-command without the space.
    pub whitelist_name: &'static str,
    pub gating: Gating,
    pub class: Class,
    /// The declared arguments in position order, hidden ones included.
    pub args: &'static [RpcArg],
    /// Core's `help <name>` output, verbatim.
    pub help: &'static str,
}

impl RpcCommand {
    /// The command is in the `hidden` category.
    pub fn is_hidden(&self) -> bool {
        self.category == "hidden"
    }

    /// The method part of the name (`protx` for `protx register`).
    pub fn method(&self) -> &'static str {
        self.name.split_once(' ').map_or(self.name, |(method, _)| method)
    }

    /// The sub-command part of a two-word name.
    pub fn subcommand(&self) -> Option<&'static str> {
        self.name.split_once(' ').map(|(_, sub)| sub)
    }

    /// `RPCHelpMan::GetArgNames`: each argument's `names`, aliases included, in position order.
    pub fn arg_names(&self) -> impl Iterator<Item = &'static str> {
        self.args.iter().map(|a| a.names)
    }
}

/// The command registered under `name`.
pub fn command(name: &str) -> Option<&'static RpcCommand> {
    COMMANDS.binary_search_by_key(&name, |c| c.name).ok().map(|i| &COMMANDS[i])
}

/// Core's `help` RPC (`CRPCTable::help`) for a node with or without the wallet enabled.
///
/// `command` is the help request's arguments joined by a space, as Core joins them
/// (`help protx register` asks for `"protx register"`); empty for the overview of every
/// command that is neither hidden nor a sub-command, grouped by category. Core's undocumented
/// `help dump_all_command_conversions` (the server's argument map) is not rendered: it answers
/// "unknown command" here.
pub fn help(command: &str, wallet_enabled: bool) -> String {
    if !command.is_empty() {
        return match self::command(command) {
            Some(c) if c.gating.registered(wallet_enabled) => c.help.to_owned(),
            _ => format!("help: unknown command: {command}"),
        };
    }
    // Core sorts by category + name, concatenated.
    let mut listed: Vec<&RpcCommand> = COMMANDS
        .iter()
        .filter(|c| {
            c.gating.registered(wallet_enabled) && !c.is_hidden() && c.subcommand().is_none()
        })
        .collect();
    listed.sort_by_cached_key(|c| [c.category, c.name].concat());
    let mut out = String::new();
    let mut category = "";
    for c in listed {
        if c.category != category {
            if !category.is_empty() {
                out.push('\n');
            }
            category = c.category;
            out.push_str("== ");
            out.push_str(&capitalize(category));
            out.push_str(" ==\n");
        }
        out.push_str(c.help.split_once('\n').map_or(c.help, |(first, _)| first));
        out.push('\n');
    }
    out.pop();
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// One entry of `vRPCConvertParams`: dash-cli parses this argument as JSON.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ConvertParam {
    /// The method, or `"<method> <subcommand>"`.
    pub method: &'static str,
    /// The argument's position on the dash-cli command line, counted from 0; for a two-word
    /// method the sub-command is position 0.
    pub index: usize,
    /// The argument's name for `-named` calls.
    pub name: &'static str,
    /// Parse only a value that looks like a JSON array or object; otherwise pass the string.
    pub preserve_str: bool,
    /// Inside `#if ENABLE_MINER` (the `generate*` commands).
    pub miner_only: bool,
}

/// The conversion rule for a positional argument. Core keeps the first entry for a key.
pub fn convert_param(method: &str, index: usize) -> Option<&'static ConvertParam> {
    CONVERT_PARAMS.iter().find(|p| p.method == method && p.index == index)
}

/// The conversion rule for a named argument. Core keeps the first entry for a key.
pub fn convert_param_by_name(method: &str, name: &str) -> Option<&'static ConvertParam> {
    CONVERT_PARAMS.iter().find(|p| p.method == method && p.name == name)
}

/// Whether the conversion table has any rule for `method` (Core's `IsDefined`), which is how
/// dash-cli decides that `<method> <first argument>` names a sub-command.
pub fn has_convert_params(method: &str) -> bool {
    CONVERT_PARAMS.iter().any(|p| p.method == method)
}

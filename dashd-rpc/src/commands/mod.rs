//! The commands that need no chain: `rpc/server.cpp`'s `help`, `stop`, `uptime` and
//! `getrpcinfo`, and `rpc/node.cpp`'s `logging`, `debug`, `echo`, `echojson`, `echoipc`,
//! `setmocktime` and `mockscheduler`.
//!
//! Chain, network, mempool and wallet commands register through
//! [`RpcServer::append_command`](crate::server::RpcServer::append_command) from their own crates.

use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::UniValue;
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i32, get_int_i64, param, values};
use crate::server::{Command, NodeControl, RpcServer};

/// Registers every command of this module on `server`.
pub fn register_control_commands(server: &Arc<RpcServer>) {
    // `help` reads the server's argument map; a weak handle keeps the table from owning itself.
    let weak = Arc::downgrade(server);
    server.append_command(Command::new("help", move |r| help(&weak, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("stop", move |r| stop(&*control, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("uptime", move |_| {
        Ok((control.now() - control.startup_time()).into())
    }));
    let weak = Arc::downgrade(server);
    server.append_command(Command::new("getrpcinfo", move |_| {
        let server = weak.upgrade().expect("the server outlives its commands");
        Ok(getrpcinfo(&server))
    }));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("logging", move |r| logging(&*control, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("debug", move |r| debug(&*control, &r.params)));
    for name in ["echo", "echojson"] {
        let control = Arc::clone(server.control());
        server.append_command(Command::new(name, move |r| echo(&*control, &r.params)));
    }
    // Without a multiprocess build Core echoes in process.
    server.append_command(Command::new("echoipc", |r| Ok(param(&r.params, 0).get_str()?.into())));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("setmocktime", move |r| setmocktime(&*control, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("mockscheduler", move |r| {
        mockscheduler(&*control, &r.params)
    }));
}

/// `Ticks<std::chrono::seconds>(std::chrono::nanoseconds::max())`: the largest mock time.
pub const MAX_MOCKTIME: i64 = i64::MAX / 1_000_000_000;

/// Core's `setmocktime` (regtest only).
// Core: rpc/node.cpp:250-289
fn setmocktime(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    if !control.is_mockable_chain() {
        return Err(RpcError::Runtime(
            "setmocktime is for regression testing (-regtest mode) only".into(),
        ));
    }
    let time = get_int_i64(param(params, 0))?;
    if !(0..=MAX_MOCKTIME).contains(&time) {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("Mocktime must be in the range [0, {MAX_MOCKTIME}], not {time}."),
        ));
    }
    control.set_mocktime(time);
    Ok(UniValue::Null)
}

/// Core's `mockscheduler` (regtest only).
// Core: rpc/node.cpp:893-918
fn mockscheduler(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    if !control.is_mockable_chain() {
        return Err(RpcError::Runtime(
            "mockscheduler is for regression testing (-regtest mode) only".into(),
        ));
    }
    let delta = get_int_i64(param(params, 0))?;
    if !(1..=3600).contains(&delta) {
        return Err(RpcError::Runtime(
            "delta_time must be between 1 and 3600 seconds (1 hr)".into(),
        ));
    }
    control.mock_scheduler(Duration::from_secs(delta.unsigned_abs()));
    Ok(UniValue::Null)
}

/// Core's `help` RPC. It describes Core v24's whole command table for a node without a wallet,
/// whichever commands this node serves yet (D-row RP0-D1).
// Core: rpc/server.cpp:79-161
fn help(server: &Weak<RpcServer>, params: &UniValue) -> Result<UniValue, RpcError> {
    let mut command = String::new();
    if !values(params).is_empty() {
        command = param(params, 0).get_str()?.to_owned();
    }
    if values(params).len() > 1 {
        command.push(' ');
        command.push_str(param(params, 1).get_str()?);
    }
    if command == "dump_all_command_conversions" {
        let server = server.upgrade().expect("the server outlives its commands");
        return Ok(server.dump_arg_map());
    }
    Ok(dash_rpc_spec::help(&command, false).into())
}

/// Core's `stop`: shutdown is requested; a hidden `wait` argument sleeps that many milliseconds
/// before the reply.
// Core: rpc/server.cpp:163-185
fn stop(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    control.start_shutdown();
    let wait = param(params, 0);
    if wait.is_num() {
        let ms = get_int_i32(wait)?;
        std::thread::sleep(Duration::from_millis(u64::try_from(ms).unwrap_or(0)));
    }
    Ok(format!("{} stopping", control.package_name()).into())
}

/// Core's `getrpcinfo`.
// Core: rpc/server.cpp:208-247
fn getrpcinfo(server: &RpcServer) -> UniValue {
    let mut active = UniValue::new_array();
    for (method, elapsed) in server.active_commands() {
        let mut entry = UniValue::new_object();
        entry.push_kv_end("method", method.into());
        entry
            .push_kv_end("duration", i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX).into());
        active.push_back(entry);
    }
    let mut result = UniValue::new_object();
    result.push_kv_end("active_commands", active);
    result.push_kv_end("logpath", server.control().log_path().into());
    result
}

/// Core's `EnableOrDisableLogCategories`.
// Core: rpc/node.cpp:1010-1026
fn toggle_categories(
    control: &dyn NodeControl,
    cats: &UniValue,
    enable: bool,
) -> Result<(), RpcError> {
    for cat in values(cats) {
        let cat = cat.get_str()?;
        let ok = if enable {
            control.enable_log_category(cat)
        } else {
            control.disable_log_category(cat)
        };
        if !ok {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("unknown logging category {cat}"),
            ));
        }
    }
    Ok(())
}

/// Core's `logging`: include, then exclude, then every category's state.
// Core: rpc/node.cpp:1028-1090
fn logging(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    if param(params, 0).is_array() {
        toggle_categories(control, param(params, 0), true)?;
    }
    if param(params, 1).is_array() {
        toggle_categories(control, param(params, 1), false)?;
    }
    let mut result = UniValue::new_object();
    for (category, active) in control.log_categories() {
        result.push_kv_end(category, active.into());
    }
    Ok(result)
}

/// Dash's `debug`: replaces the enabled categories with the `+`-separated list (`0` for none).
// Core: rpc/node.cpp:52-93
fn debug(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    let mode = param(params, 0).get_str()?;
    control.disable_log_category("all");
    let categories: Vec<&str> = mode.split('+').collect();
    if !categories.contains(&"0") {
        for cat in categories {
            control.enable_log_category(cat);
        }
    }
    let enabled: Vec<&str> =
        control.log_categories().into_iter().filter(|(_, on)| *on).map(|(c, _)| c).collect();
    Ok(format!("Debug mode: {}", enabled.join(", ")).into())
}

/// Core's `echo` and `echojson`: the arguments. A tenth argument `trigger_internal_bug` fails
/// Core's `CHECK_NONFATAL`, reported with this node's identity (D-row RP0-D5).
// Core: rpc/node.cpp:1092-1125
fn echo(control: &dyn NodeControl, params: &UniValue) -> Result<UniValue, RpcError> {
    if let UniValue::Str(s) = param(params, 9)
        && s == "trigger_internal_bug"
    {
        return Err(internal_bug(
            control,
            "request.params[9].get_str() != \"trigger_internal_bug\"",
            file!(),
            line!(),
            "echo",
        ));
    }
    Ok(params.clone())
}

/// A failed `CHECK_NONFATAL(check)` in `func` at `file:line`: Core's `NonFatalCheckError` text,
/// with this node's identity (D-row RP0-D5).
// Core: util/check.cpp:19-25 StrFormatInternalBug
pub(crate) fn internal_bug(
    control: &dyn NodeControl,
    check: &str,
    file: &str,
    line: u32,
    func: &str,
) -> RpcError {
    RpcError::Runtime(format!(
        "Internal bug detected: {check}\n{file}:{line} ({func})\n{} {}\nPlease report this issue \
         here: {}\n",
        control.package_name(),
        control.full_version(),
        crate::BUG_REPORT_URL,
    ))
}

#[cfg(test)]
mod tests;

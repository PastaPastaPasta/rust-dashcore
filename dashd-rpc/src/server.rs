//! The command table and its dispatch (`CRPCTable`, `rpc/server.cpp`), with the argument checks
//! `RPCHelpMan::HandleRequest` makes before a command runs (`rpc/util.cpp`).
//!
//! Every command is registered with its entry in [`dash_rpc_spec`]: the spec supplies the
//! argument names named arguments map to, the arity and type checks, and the help text a wrong
//! argument count throws. A method nobody registered answers as in Core, `Method not found`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use dash_rpc_spec::{ArgType, RpcCommand};

use crate::UniValue;
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_IN_WARMUP, RPC_INVALID_PARAMETER, RPC_METHOD_NOT_FOUND,
    RPC_TYPE_ERROR, RpcError,
};
use crate::request::{JsonRpcRequest, param, values};

/// A command's handler. It runs after the server's arity and type checks.
pub type Actor = Box<dyn Fn(&JsonRpcRequest) -> Result<UniValue, RpcError> + Send + Sync>;

/// One registered command (`CRPCCommand` built from an `RPCHelpMan`).
pub struct Command {
    spec: &'static RpcCommand,
    actor: Actor,
}

impl Command {
    /// The command `name` of the v24 table, run by `actor`.
    ///
    /// # Panics
    /// If Core v24 has no command `name`: every command the node serves is one of Core's.
    pub fn new(
        name: &str,
        actor: impl Fn(&JsonRpcRequest) -> Result<UniValue, RpcError> + Send + Sync + 'static,
    ) -> Command {
        let spec = dash_rpc_spec::command(name)
            .unwrap_or_else(|| panic!("{name} is not a Dash Core v24 RPC command"));
        Command::with_spec(spec, actor)
    }

    /// A command described by `spec`, which need not be one of Core's (tests).
    pub fn with_spec(
        spec: &'static RpcCommand,
        actor: impl Fn(&JsonRpcRequest) -> Result<UniValue, RpcError> + Send + Sync + 'static,
    ) -> Command {
        Command {
            spec,
            actor: Box::new(actor),
        }
    }

    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    /// `RPCHelpMan::HandleRequest`: the arity and type checks, then the handler.
    // Core: rpc/util.cpp:525-551
    fn handle(&self, request: &JsonRpcRequest) -> Result<UniValue, RpcError> {
        if !valid_num_args(self.spec, values(&request.params).len()) {
            return Err(RpcError::Runtime(self.spec.help.to_owned()));
        }
        let mut mismatch = UniValue::new_object();
        for (i, arg) in self.spec.args.iter().enumerate() {
            if let Some(msg) = type_mismatch(arg, param(&request.params, i)) {
                mismatch.push_kv_end(format!("Position {} ({})", i + 1, arg.names), msg.into());
            }
        }
        if !values(&mismatch).is_empty() {
            return Err(RpcError::new(
                RPC_TYPE_ERROR,
                format!("Wrong type passed:\n{}", mismatch.write_pretty(4, 0)),
            ));
        }
        (self.actor)(request)
    }
}

/// `RPCHelpMan::IsValidNumArgs`.
// Core: rpc/util.cpp:622-632
fn valid_num_args(spec: &RpcCommand, n: usize) -> bool {
    let required = spec.args.iter().rposition(|a| !a.fallback.is_optional()).map_or(0, |i| i + 1);
    required <= n && n <= spec.args.len()
}

/// `RPCArg::MatchesType`: the message for a value of the wrong JSON type.
// Core: rpc/util.cpp:714-758
fn type_mismatch(arg: &dash_rpc_spec::RpcArg, value: &UniValue) -> Option<String> {
    use dash_args::univalue::ValueType;
    if arg.skip_type_check || (arg.fallback.is_optional() && value.is_null()) {
        return None;
    }
    let expected = match arg.ty {
        ArgType::Str | ArgType::StrHex => ValueType::Str,
        ArgType::Num => ValueType::Num,
        ArgType::Bool => ValueType::Bool,
        ArgType::Obj | ArgType::ObjUserKeys => ValueType::Obj,
        ArgType::Arr => ValueType::Arr,
        // Checked by the handler (`AmountFromValue`, `ParseRange`).
        ArgType::Amount | ArgType::Range => return None,
    };
    (value.value_type() != expected).then(|| {
        format!(
            "JSON value of type {} is not of expected type {}",
            value.value_type().name(),
            expected.name()
        )
    })
}

/// `RPCHelpMan::GetArgMap`: `[method, position, name, is a string]` for every name and alias.
// Core: rpc/util.cpp:695-712
fn arg_map(spec: &RpcCommand, out: &mut UniValue) {
    for (i, arg) in spec.args.iter().enumerate() {
        for name in arg.aliases() {
            out.push_back(UniValue::Arr(vec![
                spec.name.into(),
                (i as i64).into(),
                name.into(),
                matches!(arg.ty, ArgType::Str | ArgType::StrHex).into(),
            ]));
        }
    }
}

/// Core's `transformNamedArguments`: named arguments to positions, with `args` holding
/// positional ones.
// Core: rpc/server.cpp:435-497
fn transform_named_arguments(
    request: &JsonRpcRequest,
    arg_names: &[&'static str],
) -> Result<JsonRpcRequest, RpcError> {
    let UniValue::Obj(named) = &request.params else {
        unreachable!("called for named arguments")
    };
    let mut args_in: Vec<(&str, &UniValue)> = Vec::with_capacity(named.len());
    for (key, value) in named {
        if args_in.iter().any(|(k, _)| *k == key) {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Parameter {key} specified multiple times"),
            ));
        }
        args_in.push((key, value));
    }
    let mut out = Vec::new();
    let mut hole = 0;
    let mut initial_hole_size = 0;
    for pattern in arg_names {
        let found =
            pattern.split('|').find_map(|name| args_in.iter().position(|(k, _)| *k == name));
        match found {
            Some(at) => {
                out.extend(std::iter::repeat_n(UniValue::Null, hole));
                hole = 0;
                out.push(args_in.remove(at).1.clone());
            }
            None => {
                hole += 1;
                if out.is_empty() {
                    initial_hole_size = hole;
                }
            }
        }
    }
    // `args` is taken out whatever its type; only an array is used.
    if let Some(at) = args_in.iter().position(|(k, _)| *k == "args")
        && let (_, UniValue::Arr(positional)) = args_in.remove(at)
    {
        let has_named_arguments = initial_hole_size < arg_names.len();
        if initial_hole_size < positional.len() && has_named_arguments {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!(
                    "Parameter {} specified twice both as positional and named argument",
                    arg_names[initial_hole_size]
                ),
            ));
        }
        let named_args = std::mem::replace(&mut out, positional.clone());
        if named_args.len() > out.len() {
            out.extend(named_args.into_iter().skip(positional.len()));
        }
    }
    // Core reports the first key of an `unordered_map`; with one unknown key that is the key.
    if let Some((key, _)) = args_in.first() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, format!("Unknown named parameter {key}")));
    }
    let mut transformed = request.clone();
    transformed.params = UniValue::Arr(out);
    Ok(transformed)
}

/// The node services the control commands use; `dashd-node` implements it.
pub trait NodeControl: Send + Sync {
    /// Core's `StartShutdown`.
    fn start_shutdown(&self);
    /// Core's `ShutdownRequested`.
    fn shutdown_requested(&self) -> bool;
    /// `GetTime()`: seconds, mock time included.
    fn now(&self) -> i64;
    /// `GetStartupTime()`.
    fn startup_time(&self) -> i64;
    /// `-logips`.
    fn log_ips(&self) -> bool;
    /// `LogInstance().m_file_path`.
    fn log_path(&self) -> String;
    /// `LogInstance().EnableCategory(name)`: false for an unknown name.
    fn enable_log_category(&self, name: &str) -> bool;
    /// `LogInstance().DisableCategory(name)`: false for an unknown name.
    fn disable_log_category(&self, name: &str) -> bool;
    /// `LogInstance().LogCategoriesList()`: every category, sorted, and whether it is on.
    fn log_categories(&self) -> Vec<(&'static str, bool)>;
    /// The product name in texts (`PACKAGE_NAME`).
    fn package_name(&self) -> &'static str;
    /// `FormatFullVersion()`.
    fn full_version(&self) -> &'static str;
    /// `Params().IsMockableChain()`.
    fn is_mockable_chain(&self) -> bool;
    /// `setmocktime`'s `SetMockTime(time)` under `cs_main`, then each chain client's
    /// `setMockTime`: an implementation sets it between two chainstate operations, never inside
    /// one (RP1a D6). `time` is in `[0, MAX_MOCKTIME]`; 0 is the system clock again.
    fn set_mocktime(&self, time: i64);
    /// `mockscheduler`'s `scheduler->MockForward(delta)`, then
    /// `SyncWithValidationInterfaceQueue()`. `delta` is in (0 s, 1 h].
    fn mock_scheduler(&self, delta: Duration);
}

/// One running command (`RPCCommandExecutionInfo`).
struct Active {
    method: String,
    start: Instant,
}

/// Core's `CRPCTable` and the state `rpc/server.cpp` keeps beside it: running, warmup, the
/// commands in progress.
pub struct RpcServer {
    commands: RwLock<BTreeMap<String, Vec<Arc<Command>>>>,
    running: AtomicBool,
    /// `Some(status)` while in warmup.
    warmup: Mutex<Option<String>>,
    active: Mutex<BTreeMap<u64, Active>>,
    next_active: AtomicU64,
    control: Arc<dyn NodeControl>,
    /// `g_rpcSignals`: `Started`'s and `Stopped`'s slots.
    signals: Mutex<Signals>,
}

/// A slot of `g_rpcSignals`.
type Slot = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Signals {
    started: Vec<Slot>,
    stopped: Vec<Slot>,
    /// How far the server has come; it only moves forward, and `interrupt` does not move it
    /// back as it does `running`.
    phase: Phase,
}

/// The server's lifecycle, as the slots see it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    #[default]
    New,
    /// [`RpcServer::start`] has begun: `Started` has fired or is firing.
    Started,
    /// [`RpcServer::stop`] has begun: `Stopped` has fired or is firing.
    Stopped,
}

impl RpcServer {
    pub fn new(control: Arc<dyn NodeControl>) -> RpcServer {
        RpcServer {
            commands: RwLock::new(BTreeMap::new()),
            running: AtomicBool::new(false),
            warmup: Mutex::new(Some("RPC server started".to_owned())),
            active: Mutex::new(BTreeMap::new()),
            next_active: AtomicU64::new(0),
            control,
            signals: Mutex::default(),
        }
    }

    pub fn control(&self) -> &Arc<dyn NodeControl> {
        &self.control
    }

    /// `CRPCTable::appendCommand`. A name may have several handlers; the first one runs.
    ///
    /// # Panics
    /// Once the server runs, as Core's `CHECK_NONFATAL(!IsRPCRunning())`.
    pub fn append_command(&self, command: Command) {
        assert!(!self.is_running(), "commands are added before the RPC server runs");
        let mut commands = self.commands.write().expect("not poisoned");
        commands.entry(command.name().to_owned()).or_default().push(Arc::new(command));
    }

    /// `CRPCTable::listCommands`.
    pub fn list_commands(&self) -> Vec<String> {
        self.commands.read().expect("not poisoned").keys().cloned().collect()
    }

    /// `RPCServer::OnStarted`: `slot` runs at [`start`](Self::start).
    ///
    /// # Panics
    /// Once `start` has begun, after an `interrupt` too: the slot would never run.
    pub fn on_started(&self, slot: impl Fn() + Send + Sync + 'static) {
        let mut signals = self.signals.lock().expect("not poisoned");
        assert_eq!(signals.phase, Phase::New, "Started slots connect before the RPC server starts");
        signals.started.push(Box::new(slot));
    }

    /// `RPCServer::OnStopped`: `slot` runs at [`stop`](Self::stop). As in Core it connects at
    /// any time before then, while the server runs too.
    ///
    /// # Panics
    /// Once `stop` has begun: the slot would never run.
    pub fn on_stopped(&self, slot: impl Fn() + Send + Sync + 'static) {
        let mut signals = self.signals.lock().expect("not poisoned");
        assert!(
            signals.phase < Phase::Stopped,
            "Stopped slots connect before the RPC server stops"
        );
        signals.stopped.push(Box::new(slot));
    }

    /// `StartRPC`.
    // Core: rpc/server.cpp:307-312
    pub fn start(&self) {
        tracing::debug!(target: "rpc", "Starting RPC");
        let mut signals = self.signals.lock().expect("not poisoned");
        signals.phase = signals.phase.max(Phase::Started);
        self.running.store(true, Ordering::SeqCst);
        for slot in &signals.started {
            slot();
        }
    }

    /// `StopRPC`'s `g_rpcSignals.Stopped()`, after the caller's log line and cookie removal.
    ///
    /// # Panics
    /// Before [`interrupt`](Self::interrupt), as Core's release-build `assert`: the woken waits
    /// would wait on, and the HTTP workers' join would hang.
    // Core: rpc/server.cpp:325-336
    pub fn stop(&self) {
        assert!(!self.is_running(), "StopRPC follows InterruptRPC");
        let mut signals = self.signals.lock().expect("not poisoned");
        signals.phase = Phase::Stopped;
        for slot in &signals.stopped {
            slot();
        }
    }

    /// `InterruptRPC`: runs once.
    pub fn interrupt(&self) {
        if self.running.swap(false, Ordering::SeqCst) {
            tracing::debug!(target: "rpc", "Interrupting RPC");
        }
    }

    /// `IsRPCRunning`.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// `RpcInterruptionPoint`: long commands call it to give up at shutdown.
    pub fn interruption_point(&self) -> Result<(), RpcError> {
        if self.is_running() {
            Ok(())
        } else {
            Err(RpcError::new(RPC_CLIENT_NOT_CONNECTED, "Shutting down"))
        }
    }

    /// `SetRPCWarmupStatus`: init messages become the warmup error's text.
    pub fn set_warmup_status(&self, status: &str) {
        let mut warmup = self.warmup.lock().expect("not poisoned");
        if let Some(s) = warmup.as_mut() {
            *s = status.to_owned();
        }
    }

    /// `SetRPCWarmupFinished`.
    pub fn set_warmup_finished(&self) {
        let mut warmup = self.warmup.lock().expect("not poisoned");
        assert!(warmup.is_some(), "warmup finishes once");
        *warmup = None;
    }

    /// `RPCIsInWarmup`: the status while in warmup.
    pub fn warmup_status(&self) -> Option<String> {
        self.warmup.lock().expect("not poisoned").clone()
    }

    /// The commands in progress and how long each has run, oldest first (`getrpcinfo`).
    pub fn active_commands(&self) -> Vec<(String, std::time::Duration)> {
        let active = self.active.lock().expect("not poisoned");
        active.values().map(|a| (a.method.clone(), a.start.elapsed())).collect()
    }

    /// `CRPCTable::execute`: warmup, sub-command squashing, then the first handler.
    // Core: rpc/server.cpp:499-518
    pub fn execute(&self, request: &JsonRpcRequest) -> Result<UniValue, RpcError> {
        if let Some(status) = self.warmup_status() {
            return Err(RpcError::new(RPC_IN_WARMUP, status));
        }
        let found = {
            let commands = self.commands.read().expect("not poisoned");
            let composite = match param(&request.params, 0) {
                UniValue::Str(sub) => commands
                    .get(&format!("{} {sub}", request.method))
                    .map(|c| (Arc::clone(&c[0]), true)),
                _ => None,
            };
            composite.or_else(|| commands.get(&request.method).map(|c| (Arc::clone(&c[0]), false)))
        };
        let Some((command, composite)) = found else {
            return Err(RpcError::new(RPC_METHOD_NOT_FOUND, "Method not found"));
        };
        let request = if composite {
            request.squashed()?
        } else {
            request.clone()
        };
        self.execute_command(&command, &request)
    }

    /// `ExecuteCommand`: named arguments to positions, and the command listed as active while it
    /// runs.
    // Core: rpc/server.cpp:520-536
    fn execute_command(
        &self,
        command: &Command,
        request: &JsonRpcRequest,
    ) -> Result<UniValue, RpcError> {
        let id = self.next_active.fetch_add(1, Ordering::Relaxed);
        let active = Active {
            method: request.method.clone(),
            start: Instant::now(),
        };
        self.active.lock().expect("not poisoned").insert(id, active);
        let _done = ActiveGuard(self, id);
        if request.params.is_object() {
            let names: Vec<_> = command.spec.arg_names().collect();
            command.handle(&transform_named_arguments(request, &names)?)
        } else {
            command.handle(request)
        }
    }

    /// `help dump_all_command_conversions` (`CRPCTable::dumpArgMap`) over the v24 table, as
    /// `help` describes it (see [`crate::commands::help`]).
    pub fn dump_arg_map(&self) -> UniValue {
        let mut ret = UniValue::new_array();
        for spec in dash_rpc_spec::COMMANDS {
            if spec.subcommand().is_none() && spec.gating.registered(false) {
                arg_map(spec, &mut ret);
            }
        }
        ret
    }
}

struct ActiveGuard<'a>(&'a RpcServer, u64);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.active.lock().expect("not poisoned").remove(&self.1);
    }
}

/// `IsDeprecatedRPCEnabled`: `-deprecatedrpc=<method>` was given.
pub fn is_deprecated_rpc_enabled(args: &dash_args::ArgsManager, method: &str) -> bool {
    args.get_args("-deprecatedrpc").unwrap_or_default().iter().any(|m| m == method)
}

#[cfg(test)]
mod tests;

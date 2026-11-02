//! `main`, `AppInitRPC`, `CommandLineRPC`, `CallRPC` and the result printing of
//! src/bitcoin-cli.cpp at v24.0.0-rc.3.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_args::args::ReadConfigError;
use dash_args::args::{abs_path_for_config_val, check_data_dir_option, help_requested};
use dash_args::options::Program;
use dash_args::paths::Product;
use dash_args::strencodings::format_paragraph;
use dash_args::univalue::RuntimeError;

use crate::handlers::{
    AddrinfoRequestHandler, DEFAULT_NBLOCKS, DefaultRequestHandler,
    GenerateToAddressRequestHandler, GetinfoRequestHandler, NetinfoRequestHandler, RequestHandler,
};
use crate::http::{self, Request, http_error_string, uri_encode};
use crate::json::{UniValue, UniValueExt};
use crate::stdin::{NoEcho, getline, stdin_ready, stdin_terminal};
use crate::{CLIENT_VERSION, COPYRIGHT_YEAR, PACKAGE_NAME};

const DEFAULT_RPCCONNECT: &str = "127.0.0.1";
const DEFAULT_HTTP_CLIENT_TIMEOUT: i64 = 900;
const DEFAULT_WAIT_CLIENT_TIMEOUT: i64 = 0;
/// Core: `DEFAULT_COLOR_SETTING`.
const DEFAULT_COLOR_SETTING: &str = "auto";
/// Core: `CURRENCY_UNIT`.
const CURRENCY_UNIT: &str = "DASH";
/// Core: `RPC_IN_WARMUP` and `RPC_WALLET_NOT_SPECIFIED` (rpc/protocol.h).
const RPC_IN_WARMUP: i32 = -28;
const RPC_WALLET_NOT_SPECIFIED: i32 = -19;
const EXIT_FAILURE: i32 = 1;
const EXIT_SUCCESS: i32 = 0;

/// The two kinds of `std::runtime_error` dash-cli distinguishes. Both print as
/// `error: <what>`; only a connection failure is retried by `-rpcwait`.
#[derive(Debug, PartialEq, Eq)]
pub enum CliError {
    /// Core: `CConnectionFailed`.
    Connection(String),
    /// Any other `std::runtime_error`.
    Runtime(String),
    /// A `std::runtime_error` whose text is bytes of a server reply, printed as they are.
    RuntimeBytes(Vec<u8>),
}

impl CliError {
    /// `what()`: a C string, so it ends at the first NUL.
    pub fn what(&self) -> Vec<u8> {
        let bytes = match self {
            CliError::Connection(s) | CliError::Runtime(s) => s.as_bytes(),
            CliError::RuntimeBytes(b) => b,
        };
        bytes.split(|&b| b == 0).next().unwrap_or_default().to_vec()
    }
}

impl From<RuntimeError> for CliError {
    fn from(e: RuntimeError) -> Self {
        CliError::Runtime(e.0)
    }
}

impl From<String> for CliError {
    fn from(e: String) -> Self {
        CliError::Runtime(e)
    }
}

/// The error for the command-line argument `arg` at `position` (0 is the program name) that is
/// not valid UTF-8. It never contains any of the argument but the name of an option: arguments
/// carry passwords (`-rpcpassword=...`), and an error line ends up in terminals and logs.
///
/// Options end at the first argument that is not one (`after_command`: an earlier argument was
/// not); from there on `-x=...` is a value for the command, perhaps a passphrase, and is only
/// named by its position. Before that, the option is named only when it is one that rdash-cli
/// registers (`-rpcpassword`, or `-norpcwait`): any other key may be a secret that was put in
/// the wrong place (`-MyPassphrase=...`).
pub fn non_utf8_argument_message(
    position: usize,
    arg: &std::ffi::OsStr,
    after_command: bool,
) -> String {
    let bytes = arg.as_encoded_bytes();
    let dashes = bytes.iter().take(2).take_while(|&&b| b == b'-').count();
    let name = &bytes[dashes..];
    let name = &name[..name.iter().position(|&b| b == b'=').unwrap_or(name.len())];
    let known = dashes > 0 && !after_command && registered_option(name);
    let which = if known {
        format!(" (option -{})", String::from_utf8_lossy(name))
    } else {
        String::new()
    };
    format!("Error parsing command line arguments: argument {position}{which} is not valid UTF-8")
}

/// Whether `name` (an option without its dashes or value) is registered by `dash-cli`, with or
/// without the `no` that negates it.
fn registered_option(name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    let args = ArgsManager::new();
    Program::DashCli.setup_args(&args);
    let registered: Vec<String> = args.registered_args().into_iter().map(|a| a.name).collect();
    let is = |key: &str| registered.iter().any(|r| *r == format!("-{key}"));
    is(name) || name.strip_prefix("no").is_some_and(is)
}

/// Core: `IsSwitchChar`.
fn is_switch_char(first: Option<char>) -> bool {
    match first {
        Some('-') => true,
        Some('/') => cfg!(windows),
        _ => false,
    }
}

fn write_stdout(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn write_stderr(text: &str) {
    write_stderr_bytes(text.as_bytes());
}

fn write_stderr_bytes(text: &[u8]) {
    let mut err = io::stderr().lock();
    let _ = err.write_all(text);
    let _ = err.flush();
}

/// Core: the license text of `LicenseInfo()`.
fn license_info() -> String {
    let url_source_code = "<https://github.com/dashpay/dash>";
    format!(
        "Copyright (C) 2014-{COPYRIGHT_YEAR} The {PACKAGE_NAME} developers\n\
         Copyright (C) 2009-{COPYRIGHT_YEAR} The Bitcoin Core developers\n\
         \n\
         Please contribute if you find {PACKAGE_NAME} useful. Visit <https://dash.org/> for further information about the software.\n\
         The source code is available from {url_source_code}.\n\
         \n\
         This is experimental software.\n\
         Distributed under the MIT software license, see the accompanying file COPYING or <https://opensource.org/licenses/MIT>\n"
    )
}

/// Core: `SplitHostPort` (util/strencodings.cpp). Returns `(host, port)`; the port keeps
/// `default_port` unless the input names a valid one.
fn split_host_port(input: &str, default_port: u16) -> (String, u16) {
    let mut port = default_port;
    let mut input = input;
    let colon = input.rfind(':');
    // if a : is found, and it either follows a [...], or no other : is in the string, treat it as port separator
    let bytes = input.as_bytes();
    let bracketed = colon.is_some_and(|c| bytes[0] == b'[' && bytes[c - 1] == b']');
    let multi_colon = colon.is_some_and(|c| c != 0 && input[..c].contains(':'));
    if let Some(c) = colon
        && (c == 0 || bracketed || !multi_colon)
    {
        // `ParseUInt16`: digits only, after one optional '+'.
        let text = &input[c + 1..];
        let digits = text.strip_prefix('+').unwrap_or(text);
        if !text.starts_with("+-")
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = digits.parse::<u16>()
        {
            input = &input[..c];
            port = n;
        }
    }
    let host = if input.len() > 1 && input.starts_with('[') && input.ends_with(']') {
        input[1..input.len() - 1].to_string()
    } else {
        input.to_string()
    };
    (host, port)
}

/// Core: `GetAuthCookie`: the first line of the cookie file, if it can be opened.
fn get_auth_cookie(args: &ArgsManager) -> Result<Option<Vec<u8>>, RuntimeError> {
    let arg = args.get_path_arg("-rpccookiefile", Path::new(".cookie"))?;
    let path = abs_path_for_config_val(args, &arg, true)?;
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(None);
    };
    let mut cookie = Vec::new();
    // `std::getline`: a read that fails leaves the cookie empty.
    let _ = io::BufReader::new(file).read_until(b'\n', &mut cookie);
    if cookie.last() == Some(&b'\n') {
        cookie.pop();
    }
    Ok(Some(cookie))
}

/// The `-stdinrpcpass` line as read. `-rpcpassword` is a `String` in the argument manager, so
/// bytes that are not UTF-8 would not survive there.
static STDIN_RPC_PASSWORD: OnceLock<Vec<u8>> = OnceLock::new();

/// Parameters from standard input are JSON strings. Stock writes bytes that are not UTF-8 into
/// the request as they are, which makes it invalid JSON; here that is an error instead of a
/// silently changed value.
fn utf8_stdin(line: Vec<u8>) -> Result<String, CliError> {
    String::from_utf8(line)
        .map_err(|_| CliError::Runtime("standard input is not valid UTF-8".to_string()))
}

/// Core: `EncodeBase64`.
fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(char::from(ALPHABET[(n >> 18) as usize & 63]));
        out.push(char::from(ALPHABET[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[n as usize & 63])
        } else {
            '='
        });
    }
    out
}

/// Core: `CallRPC`.
fn call_rpc(
    args: &ArgsManager,
    rh: &mut dyn RequestHandler,
    method: &str,
    rargs: &[String],
    rpcwallet: Option<&str>,
) -> Result<UniValue, CliError> {
    // In preference order, we choose the following for the port:
    //     1. -rpcport
    //     2. port in -rpcconnect (ie following : in ipv4 or ]: in ipv6)
    //     3. default port for chain
    let default_port = args.base_params().rpc_port();
    let (host, port) =
        split_host_port(&args.get_arg("-rpcconnect", DEFAULT_RPCCONNECT)?, default_port);
    let port = args.get_int_arg("-rpcport", i64::from(port))? as u16;

    // Set connection timeout. Indefinite request timeouts are not possible in libevent-http,
    // so a very long time period is used instead.
    let timeout = args.get_int_arg("-rpcclienttimeout", DEFAULT_HTTP_CLIENT_TIMEOUT)? as i32;
    let timeout = if timeout > 0 {
        Duration::from_secs(timeout as u64)
    } else {
        const YEAR_IN_SECONDS: u64 = 31_556_952; // Average length of year in Gregorian calendar
        Duration::from_secs(5 * YEAR_IN_SECONDS)
    };

    // Get credentials.
    let mut failed_to_get_auth_cookie = false;
    let user_colon_pass: Vec<u8> = if args.get_arg("-rpcpassword", "")?.is_empty() {
        // Try fall back to cookie-based authentication if no password is provided.
        match get_auth_cookie(args)? {
            Some(cookie) => cookie,
            None => {
                failed_to_get_auth_cookie = true;
                Vec::new()
            }
        }
    } else {
        let mut credentials = format!("{}:", args.get_arg("-rpcuser", "")?).into_bytes();
        // A password read from standard input is kept as the bytes that were typed.
        match STDIN_RPC_PASSWORD.get() {
            Some(password) => credentials.extend_from_slice(password),
            None => credentials.extend_from_slice(args.get_arg("-rpcpassword", "")?.as_bytes()),
        }
        credentials
    };

    // Attach request data.
    let request = rh.prepare_request(method, rargs)?;
    let body = format!("{}\n", request.write()).into_bytes();

    // Check if we should use a special wallet endpoint.
    let endpoint = match rpcwallet {
        Some(wallet) => format!("/wallet/{}", uri_encode(wallet.as_bytes())),
        None => "/".to_string(),
    };
    let headers = [
        ("Host", host.clone()),
        ("Connection", "close".to_string()),
        ("Content-Type", "application/json".to_string()),
        ("Authorization", format!("Basic {}", encode_base64(&user_colon_pass))),
    ];
    let response = http::post(&Request {
        host: &host,
        port,
        endpoint: &endpoint,
        headers: &headers,
        body: &body,
        timeout,
    });

    const HTTP_BAD_REQUEST: i32 = 400;
    const HTTP_UNAUTHORIZED: i32 = 401;
    const HTTP_NOT_FOUND: i32 = 404;
    const HTTP_INTERNAL_SERVER_ERROR: i32 = 500;
    const HTTP_SERVICE_UNAVAILABLE: i32 = 503;

    if response.status == 0 {
        let response_error_message = if response.error != -1 {
            format!(" (error code {} - \"{}\")", response.error, http_error_string(response.error))
        } else {
            String::new()
        };
        return Err(CliError::Connection(format!(
            "Could not connect to the server {host}:{port}{response_error_message}\n\n\
             Make sure the dashd server is running and that you are connecting to the correct RPC port.\n\
             Use \"dash-cli -help\" for more info."
        )));
    } else if response.status == HTTP_UNAUTHORIZED {
        return Err(CliError::Runtime(if failed_to_get_auth_cookie {
            format!(
                "Could not locate RPC credentials. No authentication cookie could be found, and RPC password is not set.  See -rpcpassword and -stdinrpcpass.  Configuration file: ({})",
                args.get_config_file_path().to_string_lossy()
            )
        } else {
            "Authorization failed: Incorrect rpcuser or rpcpassword".to_string()
        }));
    } else if response.status == HTTP_SERVICE_UNAVAILABLE {
        return Err(CliError::RuntimeBytes(
            [b"Server response: ".as_slice(), &response.body].concat(),
        ));
    } else if response.status >= 400
        && response.status != HTTP_BAD_REQUEST
        && response.status != HTTP_NOT_FOUND
        && response.status != HTTP_INTERNAL_SERVER_ERROR
    {
        return Err(CliError::Runtime(format!("server returned HTTP error {}", response.status)));
    } else if response.body.is_empty() {
        return Err(CliError::Runtime("no response from server".to_string()));
    }

    // Parse reply.
    let Some(val_reply) = UniValue::read(&response.body) else {
        return Err(CliError::Runtime("couldn't parse reply from server".to_string()));
    };
    let reply = rh.process_reply(&val_reply)?;
    if reply.is_empty_container() {
        return Err(CliError::Runtime(
            "expected reply to have result, error and id properties".to_string(),
        ));
    }
    Ok(reply)
}

/// Core: `ConnectAndCallRPC`: `CallRPC` with `-rpcwait`.
fn connect_and_call_rpc(
    args: &ArgsManager,
    rh: &mut dyn RequestHandler,
    method: &str,
    rargs: &[String],
    rpcwallet: Option<&str>,
) -> Result<UniValue, CliError> {
    // Execute and handle connection failures with -rpcwait.
    let wait = args.get_bool_arg("-rpcwait", false)?;
    let timeout = args.get_int_arg("-rpcwaittimeout", DEFAULT_WAIT_CLIENT_TIMEOUT)? as i32;
    let deadline = Instant::now() + Duration::from_secs(timeout.max(0) as u64);

    loop {
        let attempt = call_rpc(args, rh, method, rargs, rpcwallet).and_then(|response| {
            if wait {
                let error = response.find_value("error");
                if !error.is_null() && error.key("code").get_int()? == RPC_IN_WARMUP {
                    return Err(CliError::Connection("server in warmup".to_string()));
                }
            }
            Ok(response)
        });
        match attempt {
            Err(CliError::Connection(what)) => {
                if wait && (timeout <= 0 || Instant::now() < deadline) {
                    std::thread::sleep(Duration::from_secs(1));
                } else {
                    return Err(CliError::Connection(format!(
                        "timeout on transient error: {what}"
                    )));
                }
            }
            other => return other,
        }
        // `while (fWait)`: only reached when waiting.
    }
}

/// Core: `ParseResult`.
fn parse_result(result: &UniValue, str_print: &mut String) {
    if result.is_null() {
        return;
    }
    *str_print = match result {
        UniValue::Str(s) => s.clone(),
        other => other.write_pretty(2, 0),
    };
}

/// Core: `ParseError`.
fn parse_error(error: &UniValue, str_print: &mut String, n_ret: &mut i32) -> Result<(), CliError> {
    if error.is_object() {
        let err_code = error.find_value("code");
        let err_msg = error.find_value("message");
        if !err_code.is_null() {
            *str_print = format!("error code: {}\n", err_code.get_val_str());
        }
        if let UniValue::Str(msg) = err_msg {
            str_print.push_str(&format!("error message:\n{msg}"));
        }
        if err_code.is_num() && err_code.get_int()? == RPC_WALLET_NOT_SPECIFIED {
            str_print.push_str(
                "\nTry adding \"-rpcwallet=<filename>\" option to dash-cli command line.",
            );
        }
    } else {
        *str_print = format!("error: {}", error.write());
    }
    *n_ret = error.key("code").get_int()?.wrapping_abs();
    Ok(())
}

/// Core: `GetWalletBalances`: if more than one wallet is loaded, the `mine.trusted` balance of
/// each is pushed to `result`.
fn get_wallet_balances(args: &ArgsManager, result: &mut UniValue) -> Result<(), CliError> {
    let mut rh = DefaultRequestHandler::new(args);
    let listwallets = connect_and_call_rpc(args, &mut rh, "listwallets", &[], None)?;
    if !listwallets.find_value("error").is_null() {
        return Ok(());
    }
    let wallets = listwallets.find_value("result");
    if wallets.size() <= 1 {
        return Ok(());
    }

    let mut balances = UniValue::new_object();
    for wallet in wallets.get_values()? {
        let wallet_name = wallet.get_str()?;
        let getbalances =
            connect_and_call_rpc(args, &mut rh, "getbalances", &[], Some(wallet_name))?;
        let balance = getbalances.find_value("result").key("mine").key("trusted");
        balances.push_kv(wallet_name, balance.clone());
    }
    if !result.is_object() {
        return Err(CliError::Runtime(format!(
            "JSON value of type {} is not of expected type object",
            result.value_type().name()
        )));
    }
    result.push_kv("balances", balances);
    Ok(())
}

/// Core: `GetProgressBar`: a progress bar with 5% intervals.
fn get_progress_bar(progress: f64, progress_bar: &mut String) {
    if !(0.0..=1.0).contains(&progress) {
        return;
    }
    const INCREMENT: f64 = 0.05;
    let mut i = 0;
    while f64::from(i) < progress / INCREMENT {
        progress_bar.push('\u{2592}');
        i += 1;
    }
    let mut i = 0;
    while f64::from(i) < (1.0 - progress) / INCREMENT {
        progress_bar.push('\u{2591}');
        i += 1;
    }
}

/// Core: `ParseGetInfoResult`: turns the `-getinfo` result into the text to print.
fn parse_getinfo_result(args: &ArgsManager, result: &mut UniValue) -> Result<(), CliError> {
    if !result.find_value("error").is_null() {
        return Ok(());
    }

    // By default, only print colored text if OS is not WIN32 and stdout is connected to a terminal.
    let mut should_colorize = cfg!(unix) && io::IsTerminal::is_terminal(&io::stdout());

    if args.is_arg_set("-color") {
        let color = args.get_arg("-color", DEFAULT_COLOR_SETTING)?;
        if color == "always" {
            should_colorize = true;
        } else if color == "never" {
            should_colorize = false;
        } else if color != "auto" {
            return Err(CliError::Runtime(
                "Invalid value for -color option. Valid values: always, auto, never.".to_string(),
            ));
        }
    }

    let (reset, green, blue, yellow, magenta, cyan) = if should_colorize {
        ("\x1B[0m", "\x1B[32m", "\x1B[34m", "\x1B[33m", "\x1B[35m", "\x1B[36m")
    } else {
        ("", "", "", "", "", "")
    };

    let val = |v: &UniValue| v.get_val_str().to_string();

    let mut s = format!("{blue}Chain: {}{reset}\n", val(result.key("chain")));
    s.push_str(&format!("Blocks: {}\n", val(result.key("blocks"))));
    s.push_str(&format!("Headers: {}\n", val(result.key("headers"))));

    let ibd_progress = result.key("verificationprogress").get_real()?;
    let mut ibd_progress_bar = String::new();
    // Display the progress bar only if IBD progress is less than 99%
    if ibd_progress < 0.99 {
        get_progress_bar(ibd_progress, &mut ibd_progress_bar);
        // Add padding between progress bar and IBD progress
        ibd_progress_bar.push(' ');
    }

    s.push_str(&format!("Verification progress: {ibd_progress_bar}{:.4}%\n", ibd_progress * 100.0));
    s.push_str(&format!("Difficulty: {}\n\n", val(result.key("difficulty"))));

    let connections = result.key("connections");
    s.push_str(&format!(
        "{green}Network: in {}, out {}, total {}, mn_in {}, mn_out {}, mn_total {}{reset}\n",
        val(connections.key("in")),
        val(connections.key("out")),
        val(connections.key("total")),
        val(connections.key("mn_in")),
        val(connections.key("mn_out")),
        val(connections.key("mn_total")),
    ));
    s.push_str(&format!("Version: {}\n", val(result.key("version"))));
    s.push_str(&format!("Time offset (s): {}\n", val(result.key("timeoffset"))));

    // proxies
    let mut proxy_networks: Vec<(String, Vec<String>)> = Vec::new();
    for network in result.key("networks").get_values()? {
        let proxy = val(network.key("proxy"));
        if proxy.is_empty() {
            continue;
        }
        match proxy_networks.iter_mut().find(|(p, _)| *p == proxy) {
            Some((_, names)) => names.push(val(network.key("name"))),
            None => proxy_networks.push((proxy, vec![val(network.key("name"))])),
        }
    }
    let formatted_proxies: Vec<String> = proxy_networks
        .iter()
        .map(|(proxy, names)| format!("{proxy} ({})", names.join(", ")))
        .collect();
    s.push_str(&format!(
        "Proxies: {}\n",
        if formatted_proxies.is_empty() {
            "n/a".to_string()
        } else {
            formatted_proxies.join(", ")
        }
    ));

    s.push_str(&format!(
        "Min tx relay fee rate ({CURRENCY_UNIT}/kB): {}\n\n",
        val(result.key("relayfee"))
    ));

    if !result.key("has_wallet").is_null() {
        let walletname = val(result.key("walletname"));
        s.push_str(&format!(
            "{magenta}Wallet: {}{reset}\n",
            if walletname.is_empty() {
                "\"\""
            } else {
                walletname.as_str()
            }
        ));
        s.push_str(&format!(
            "{cyan}CoinJoin balance:{reset} {}\n",
            val(result.key("coinjoin_balance"))
        ));
        s.push_str(&format!("Keypool size: {}\n", val(result.key("keypoolsize"))));
        if !result.key("unlocked_until").is_null() {
            s.push_str(&format!("Unlocked until: {}\n", val(result.key("unlocked_until"))));
        }
        s.push_str(&format!(
            "Transaction fee rate (-paytxfee) ({CURRENCY_UNIT}/kB): {}\n\n",
            val(result.key("paytxfee"))
        ));
    }
    if !result.key("balance").is_null() {
        s.push_str(&format!("{cyan}Balance:{reset} {}\n\n", val(result.key("balance"))));
    }

    if !result.key("balances").is_null() {
        s.push_str(&format!("{cyan}Balances{reset}\n"));
        let balances = result.key("balances");
        let wallets = balances.get_keys()?;
        let mut max_balance_length = 10usize;
        for wallet in &wallets {
            max_balance_length = max_balance_length.max(balances.key(wallet).get_val_str().len());
        }
        for wallet in &wallets {
            let balance = balances.key(wallet).get_val_str();
            s.push_str(&format!(
                "{}{balance} {}\n",
                " ".repeat(max_balance_length.saturating_sub(balance.len())),
                if wallet.is_empty() {
                    "\"\""
                } else {
                    wallet
                }
            ));
        }
        s.push('\n');
    }

    let warnings = val(result.key("warnings"));
    s.push_str(&format!(
        "{yellow}Warnings:{reset} {}",
        if warnings.is_empty() {
            "(none)"
        } else {
            warnings.as_str()
        }
    ));

    *result = UniValue::Str(s);
    Ok(())
}

/// Core: `SetGenerateToAddressArgs`: check bounds and set up args for RPC `generatetoaddress`
/// params: nblocks, address, maxtries.
fn set_generate_to_address_args(address: &str, args: &mut Vec<String>) -> Result<(), CliError> {
    if args.len() > 2 {
        return Err(CliError::Runtime(
            "too many arguments (maximum 2 for nblocks and maxtries)".to_string(),
        ));
    }
    if args.is_empty() {
        args.push(DEFAULT_NBLOCKS.to_string());
    } else if args[0] == "0" {
        return Err(CliError::Runtime(format!(
            "the first argument (number of blocks to generate, default: {DEFAULT_NBLOCKS}) must be an integer value greater than zero"
        )));
    }
    args.insert(1, address.to_string());
    Ok(())
}

/// What the end of `CommandLineRPC` does.
enum Outcome {
    /// Print `str_print` (to stdout on success, stderr otherwise) and exit with `n_ret`.
    Print,
    /// `return 0` without printing: the text was already written.
    Done,
}

/// Core: `CommandLineRPC`.
fn command_line_rpc(args: &ArgsManager, argv: &[String]) -> i32 {
    let mut str_print = String::new();
    let mut error_print: Option<Vec<u8>> = None;
    let mut n_ret = 0;
    let outcome = (|| -> Result<Outcome, CliError> {
        // Skip switches
        let mut argv = argv;
        while argv.len() > 1 && is_switch_char(argv[1].chars().next()) {
            argv = &argv[1..];
        }
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        if args.get_bool_arg("-stdinrpcpass", false)? {
            let _no_echo = NoEcho::new();
            if !stdin_ready() {
                write_stderr("RPC password> ");
            }
            let Some(rpc_pass) = getline(&mut stdin) else {
                return Err(CliError::Runtime(
                    "-stdinrpcpass specified but failed to read from standard input".to_string(),
                ));
            };
            if stdin_terminal() {
                write_stdout("\n");
            }
            args.force_set_arg("-rpcpassword", &String::from_utf8_lossy(&rpc_pass));
            let _ = STDIN_RPC_PASSWORD.set(rpc_pass);
        }
        let mut cmd_args: Vec<String> = argv.iter().skip(1).cloned().collect();
        if args.get_bool_arg("-stdinwalletpassphrase", false)? {
            let _no_echo = NoEcho::new();
            if cmd_args.is_empty() || !cmd_args[0].as_bytes().starts_with(b"walletpassphrase") {
                return Err(CliError::Runtime(
                    "-stdinwalletpassphrase is only applicable for walletpassphrase(change)"
                        .to_string(),
                ));
            }
            if !stdin_ready() {
                write_stderr("Wallet passphrase> ");
            }
            let Some(wallet_pass) = getline(&mut stdin) else {
                return Err(CliError::Runtime(
                    "-stdinwalletpassphrase specified but failed to read from standard input"
                        .to_string(),
                ));
            };
            if stdin_terminal() {
                write_stdout("\n");
            }
            cmd_args.insert(1, utf8_stdin(wallet_pass)?);
        }
        if args.get_bool_arg("-stdin", false)? {
            // Read one arg per line from stdin and append
            while let Some(line) = getline(&mut stdin) {
                cmd_args.push(utf8_stdin(line)?);
            }
            if stdin_terminal() {
                write_stdout("\n");
            }
        }
        let mut rh: Option<Box<dyn RequestHandler + '_>> = None;
        let mut method = String::new();
        if args.is_arg_set("-getinfo") {
            rh = Some(Box::new(GetinfoRequestHandler));
        } else if args.get_bool_arg("-netinfo", false)? {
            if cmd_args.first().is_some_and(|a| a == "help") {
                write_stdout(&format!("{}\n", NetinfoRequestHandler::help_doc()));
                return Ok(Outcome::Done);
            }
            rh = Some(Box::new(NetinfoRequestHandler::new(args)));
        } else if args.get_bool_arg("-generate", false)? {
            let getnewaddress = get_new_address(args)?;
            let error = getnewaddress.find_value("error");
            if error.is_null() {
                set_generate_to_address_args(
                    getnewaddress.find_value("result").get_str()?,
                    &mut cmd_args,
                )?;
                rh = Some(Box::new(GenerateToAddressRequestHandler::default()));
            } else {
                parse_error(error, &mut str_print, &mut n_ret)?;
            }
        } else if args.get_bool_arg("-addrinfo", false)? {
            rh = Some(Box::new(AddrinfoRequestHandler));
        } else {
            rh = Some(Box::new(DefaultRequestHandler::new(args)));
            if cmd_args.is_empty() {
                return Err(CliError::Runtime(
                    "too few parameters (need at least command)".to_string(),
                ));
            }
            method = cmd_args.remove(0); // Remove trailing method name from arguments vector
        }
        if n_ret == 0 {
            // Perform RPC call
            let wallet_name = rpcwallet_arg(args)?;
            // A `getnewaddress` error with code 0 leaves no handler; stock dereferences null.
            // The error is printed on standard error and the exit code is a failure: stock
            // aborts, and a failed -generate must not look like a success.
            let Some(rh) = rh.as_mut() else {
                n_ret = EXIT_FAILURE;
                return Ok(Outcome::Print);
            };
            let reply = connect_and_call_rpc(
                args,
                rh.as_mut(),
                &method,
                &cmd_args,
                wallet_name.as_deref(),
            )?;

            // Parse reply
            let mut result = reply.find_value("result").clone();
            let error = reply.find_value("error");
            if error.is_null() {
                if args.get_bool_arg("-getinfo", false)? {
                    if !args.is_arg_set("-rpcwallet") {
                        get_wallet_balances(args, &mut result)?; // fetch multiwallet balances and append to result
                    }
                    parse_getinfo_result(args, &mut result)?;
                }
                parse_result(&result, &mut str_print);
            } else {
                parse_error(error, &mut str_print, &mut n_ret)?;
            }
        }
        Ok(Outcome::Print)
    })();

    match outcome {
        Ok(Outcome::Done) => return EXIT_SUCCESS,
        Ok(Outcome::Print) => {}
        Err(e) => {
            error_print = Some([b"error: ".as_slice(), &e.what(), b"\n"].concat());
            n_ret = EXIT_FAILURE;
        }
    }

    if let Some(text) = error_print {
        write_stderr_bytes(&text);
    } else if !str_print.is_empty() {
        let text = format!("{str_print}\n");
        if n_ret == 0 {
            write_stdout(&text)
        } else {
            write_stderr(&text)
        }
    }
    n_ret
}

fn rpcwallet_arg(args: &ArgsManager) -> Result<Option<String>, RuntimeError> {
    if args.is_arg_set("-rpcwallet") {
        args.get_arg("-rpcwallet", "").map(Some)
    } else {
        Ok(None)
    }
}

/// Core: `GetNewAddress`: call RPC `getnewaddress`.
fn get_new_address(args: &ArgsManager) -> Result<UniValue, CliError> {
    let wallet_name = rpcwallet_arg(args)?;
    let mut rh = DefaultRequestHandler::new(args);
    connect_and_call_rpc(args, &mut rh, "getnewaddress", &[], wallet_name.as_deref())
}

/// Core: `AppInitRPC`: `Some(code)` when the process should exit, `None` to continue.
fn app_init_rpc(args: &ArgsManager, argv: &[String]) -> Option<i32> {
    Program::DashCli.setup_args(args); // SetupCliArgs, which starts with SetupHelpOptions
    // The Rust node's data directory (DEC-03), where Core's `GetDefaultDataDir` has `DashCore`.
    args.set_product(Product::RDashCore);
    if let Err(error) = args.parse_parameters(argv) {
        write_stderr(&format!("Error parsing command line arguments: {error}\n"));
        return Some(EXIT_FAILURE);
    }

    let wants_version = args.is_arg_set("-version");
    if argv.len() < 2 || help_requested(args) || wants_version {
        let mut usage = format!("{PACKAGE_NAME} RPC client version {CLIENT_VERSION}\n");

        if wants_version {
            usage.push_str(&format_paragraph(&license_info(), 79, 0));
        } else {
            usage.push_str(&format!(
                "\nUsage:  dash-cli [options] <command> [params]  Send command to {PACKAGE_NAME}\n\
                 or:     dash-cli [options] -named <command> [name=value]...  Send command to {PACKAGE_NAME} (with named arguments)\n\
                 or:     dash-cli [options] help                List commands\n\
                 or:     dash-cli [options] help <command>      Get help for a command\n"
            ));
            match args.get_help_message() {
                Ok(help) => {
                    usage.push('\n');
                    usage.push_str(&help);
                }
                Err(e) => return Some(report_exception("AppInitRPC()", &e)),
            }
        }

        write_stdout(&usage);
        if argv.len() < 2 {
            write_stderr("Error: too few parameters\n");
            return Some(EXIT_FAILURE);
        }
        return Some(EXIT_SUCCESS);
    }
    match check_data_dir_option(args) {
        Ok(true) => {}
        Ok(false) => {
            let datadir = args.get_arg("-datadir", "").unwrap_or_default();
            write_stderr(&format!(
                "Error: Specified data directory \"{datadir}\" does not exist.\n"
            ));
            return Some(EXIT_FAILURE);
        }
        Err(e) => return Some(report_exception("AppInitRPC()", &e)),
    }
    match args.read_config_files(true) {
        Ok(()) => {}
        Err(ReadConfigError::Failed(error)) => {
            write_stderr(&format!("Error reading configuration file: {error}\n"));
            return Some(EXIT_FAILURE);
        }
        Err(ReadConfigError::Exception(e)) => return Some(report_exception("AppInitRPC()", &e)),
    }
    // Check for chain settings (BaseParams() calls are only valid after this clause).
    let chain = match args.get_chain_name() {
        Ok(chain) => chain,
        Err(e) => {
            write_stderr(&format!("Error: {e}\n"));
            return Some(EXIT_FAILURE);
        }
    };
    if let Err(e) = args.select_base_params(&chain) {
        write_stderr(&format!("Error: {e}\n"));
        return Some(EXIT_FAILURE);
    }
    None
}

/// Core: `EncodeBase32` (lower case, padded).
fn encode_base32(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    for chunk in input.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = buf.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        // 5 input bytes make 8 symbols; a short chunk makes ceil(8 * len / 5) of them.
        let symbols = (chunk.len() * 8).div_ceil(5);
        for i in 0..8 {
            out.push(if i < symbols {
                char::from(ALPHABET[((n >> (35 - 5 * i)) & 31) as usize])
            } else {
                '='
            });
        }
    }
    out
}

/// Core: `ser_writedata` of a `std::string`: a compact-size length, then the bytes.
fn serialize_string(out: &mut Vec<u8>, s: &str) {
    match s.len() {
        n if n < 253 => out.push(n as u8),
        n if n <= 0xffff => {
            out.push(253);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        n => {
            out.push(254);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }
    out.extend_from_slice(s.as_bytes());
}

/// Core: `PrintExceptionContinue` for a `std::runtime_error` that escapes `AppInitRPC`
/// (src/stacktraces.cpp). The release binary has no debug information, so the report has no
/// stack frames: the serialized `crash_info` is the header, the description and two empty lists.
fn report_exception(thread_name: &str, e: &RuntimeError) -> i32 {
    let description = format!("Exception: type=std::runtime_error, what=\"{e}\"");
    let mut crash_info = Vec::new();
    serialize_string(&mut crash_info, "DashCrashInfo");
    crash_info.extend_from_slice(&1u16.to_le_bytes()); // version
    serialize_string(&mut crash_info, "dash-cli");
    serialize_string(&mut crash_info, &description);
    crash_info.extend_from_slice(&[0, 0]); // no stack frames, no frame infos
    write_stderr(&format!(
        "\n\n************************\n\"{thread_name}\" raised an exception\n{description}\n\
         No debug information available for stacktrace. You should add debug information and then run:\n\
         dash-cli -printcrashinfo={}\n\n",
        encode_base32(&crash_info)
    ));
    EXIT_FAILURE
}

/// The program: `main` of src/bitcoin-cli.cpp. `argv[0]` is the program name.
pub fn main_with(argv: &[String]) -> i32 {
    let args = ArgsManager::new();
    if let Some(code) = app_init_rpc(&args, argv) {
        return code;
    }
    command_line_rpc(&args, argv)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn non_utf8_argument_message_never_echoes_content() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let arg = |b: &[u8]| OsString::from_vec(b.to_vec());
        let m = |b: &[u8]| non_utf8_argument_message(3, &arg(b), false);
        let canary = "CANARY-secret";
        for bytes in [
            &b"-rpcpassword=CANARY-secret\xff"[..],
            b"--rpcpassword=CANARY-secret\xff",
            b"CANARY-secret\xff",
            b"-CANARY-secret\xff=x",
            b"-rpc password=CANARY-secret\xff",
            b"-=CANARY-secret\xff",
            b"-CANARY-private-passphrase=\xff",
            b"--CANARY-private-passphrase",
        ] {
            assert!(!m(bytes).contains(canary), "{}", m(bytes));
        }
        assert_eq!(
            m(b"-rpcpassword=CANARY-secret\xff"),
            "Error parsing command line arguments: argument 3 (option -rpcpassword) is not valid UTF-8"
        );
        assert_eq!(
            m(b"CANARY\xff"),
            "Error parsing command line arguments: argument 3 is not valid UTF-8"
        );
        // only registered options are named; negated ones too
        assert_eq!(
            m(b"-CANARY-private-passphrase=\xff"),
            "Error parsing command line arguments: argument 3 is not valid UTF-8"
        );
        assert!(m(b"-norpcwait=\xff").contains("(option -norpcwait)"));
        assert!(m(b"--rpcuser=\xff").contains("(option -rpcuser)"));
        // after the command, a dash-led value is a value, not an option
        assert_eq!(
            non_utf8_argument_message(3, &arg(b"-MyS3cretPass=\xe9t\xe9"), true),
            "Error parsing command line arguments: argument 3 is not valid UTF-8"
        );
    }

    use super::*;

    #[test]
    fn host_port_splitting() {
        assert_eq!(split_host_port("127.0.0.1", 1), ("127.0.0.1".into(), 1));
        assert_eq!(split_host_port("127.0.0.1:2", 1), ("127.0.0.1".into(), 2));
        assert_eq!(split_host_port("[::1]", 1), ("::1".into(), 1));
        assert_eq!(split_host_port("[::1]:2", 1), ("::1".into(), 2));
        assert_eq!(split_host_port("::1", 1), ("::1".into(), 1));
        assert_eq!(split_host_port("a:b:c", 1), ("a:b:c".into(), 1));
        assert_eq!(split_host_port("127.0.0.1:99999", 1), ("127.0.0.1:99999".into(), 1));
        assert_eq!(split_host_port(":5", 1), ("".into(), 5));
        assert_eq!(split_host_port("", 1), ("".into(), 1));
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"u:p"), "dTpw");
    }

    #[test]
    fn generate_args() {
        let mut a = vec![];
        set_generate_to_address_args("addr", &mut a).unwrap();
        assert_eq!(a, ["1", "addr"]);
        let mut a = vec!["4".to_string(), "1000".to_string()];
        set_generate_to_address_args("addr", &mut a).unwrap();
        assert_eq!(a, ["4", "addr", "1000"]);
        assert!(set_generate_to_address_args("addr", &mut vec!["0".to_string()]).is_err());
        assert!(
            set_generate_to_address_args("addr", &mut vec!["1".into(), "2".into(), "3".into()])
                .is_err()
        );
    }
}

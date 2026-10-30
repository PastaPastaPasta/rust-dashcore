# rdash-cli: porting notes

`rdash-cli` is a byte-compatible port of Dash Core v24's `dash-cli` (`src/bitcoin-cli.cpp` at
tag `v24.0.0-rc.3`, the version `dash-rpc-spec` pins). The stock binary is the oracle: the XCLI
test (`tests/xcli`) runs both programs with the same arguments, standard input and environment,
and compares standard output, standard error and the exit code byte for byte.

## Mapping

| Core (`bitcoin-cli.cpp`) | here |
|---|---|
| option table, `SetupCliArgs`, `ParseParameters`, `ReadConfigFiles`, chain selection | `dash-args` (`Program::DashCli`); no argument parser of its own |
| `AppInitRPC`, `CommandLineRPC`, `main` | `cli.rs`: `app_init_rpc`, `command_line_rpc`, `main_with` |
| `CallRPC`, `ConnectAndCallRPC` (the `-rpcwait` loop), `GetAuthCookie` | `cli.rs`: `call_rpc`, `connect_and_call_rpc`, `get_auth_cookie` |
| `ParseResult`, `ParseError`, `GetWalletBalances`, `ParseGetInfoResult` | `cli.rs` |
| `DefaultRequestHandler`, `GetinfoRequestHandler`, `NetinfoRequestHandler`, `AddrinfoRequestHandler`, `GenerateToAddressRequestHandler` | `handlers.rs` |
| `RPCConvertValues`, `RPCConvertNamedValues` (`rpc/client.cpp`) | `convert.rs`, driven by the generated `dash-rpc-spec` table (`CONVERT_PARAMS`); no hand-written table |
| `UniValue` | `dash_args::univalue::SettingsValue` plus the `UniValueExt` helpers in `json.rs` |
| `JSONRPCRequestObj`, `JSONRPCReplyObj`, `JSONRPCProcessBatchReply` | `json.rs` |
| libevent `evhttp` client (`bufferevent_socket_connect_hostname`, `evhttp_make_request`) | `http.rs`: a hand-written HTTP/1.1 client over `std::net` that reproduces what the CLI can observe (see below) |
| `GetPassword`, `StdinReady`, `StdinTerminal`, `NoechoInstance` | `stdin.rs` |
| `std::sort` of the `-netinfo` peers | `introsort.rs`: libstdc++'s introsort, step for step |
| `PrintExceptionContinue` / `HandleExceptions` crash banner | `cli.rs`: `report_exception` |

## HTTP behaviour taken from libevent, each checked against the stock binary

Lines, headers and the status line are C strings in libevent: each ends at its first NUL.

- Request bytes: `POST <endpoint> HTTP/1.1`, then `Host`, `Connection: close`, `Content-Type`,
  `Authorization`, `Content-Length`. The endpoint is `/` or `/wallet/<evhttp_uriencode(name)>`,
  where only the unreserved characters are left alone.
- Failure codes: a refused or timed-out connection has no error code; any other connect error
  (network or host unreachable), a name that does not resolve, a closed connection, a short body
  and a missing terminating chunk are `EOF reached` (1); an unparsable status line, header or
  `Content-Length` is `invalid header` (2); an idle `-rpcclienttimeout` is `timeout reached`
  (0); a corrupt chunk size or trailer is `response body is larger than allowed` (5). Only the
  first address a name resolves to is tried.
- Status line (`evhttp_parse_response_line`): the version is `sscanf("HTTP/%d.%d%c") == 2` with a
  major number of at most 1 (so `HTTP/1.2`, `HTTP/0.9`, `HTTP/1.01` and `HTTP/-1.1` pass and a
  character after the minor number fails); the code is `atoi` of the second word (wrapping to
  `int`, leading C white space skipped, trailing junk ignored) and must not be 0.
- Headers (`evhttp_parse_headers_`): the value loses leading spaces (not tabs) and trailing
  spaces and tabs; a line that starts with a blank continues the previous header, joined with
  one space; a CR or LF in a value must be followed by a blank, and none may be in a name. The
  first header of a name wins. The headers of an interim `100 Continue` stay and the real
  reply's are added behind them; a chunked reply's trailer is parsed into the same list.
- Body: exactly `Transfer-Encoding: chunked` (any case) wins over `Content-Length`; any other
  value is ignored. `Content-Length` is `strtoll` base 10, non-empty, whole and non-negative.
  With no length, `Connection: close` or no `Connection` header reads to the end of the
  connection and any other `Connection` value is an empty body. 1xx (other than 100), 204 and
  304 have no body. Stock puts no bound on a reply (see the limits under Deviations).
- `100 Continue`: libevent re-parses already buffered bytes through a deferred callback, so the
  next response is read unless the peer's close was seen first; this is emulated with a
  non-blocking read after the interim response.
- A chunk size is `strtoll` base 16 (sign, `0x` prefix and leading blanks accepted; a size
  that overflows saturates); the rest of the line must be empty or start with a space. Trailers
  are read after the last chunk.
- Standard output and error: bytes of a server reply are written as they are, and the text of
  an error ends at its first NUL (`what()` is a C string). A password read with
  `-stdinrpcpass` is sent as the bytes that were typed.
- SIGPIPE: the binary restores the default action, so a closed output pipe ends the program as
  it ends Core's.

## Deviations, and why

- **Default data directory (DEC-201, DEC-03).** With no `-datadir`, the data directory, the
  configuration file and `.cookie` are looked up in the Rust node's directory: `~/.rdashcore`
  on Linux, `~/Library/Application Support/RDashCore` on macOS, `%APPDATA%\RDashCore` on
  Windows, where Core has `.dashcore`/`DashCore`. `cli.rs` selects rdashd's directory with
  `ArgsManager::set_product(Product::RDashCore)`, as rdashd does. The XCLI harness runs
  with both directories present and reads rdash-cli's output with that spelling mapped to
  stock's (one registry entry, class 1); the two rows of group `deviations` check the lookup.
- **Short batch replies.** `-getinfo` and `-netinfo` index a `std::vector` sized by the server's
  reply. A server that answers the batch with fewer members than were sent makes stock read out
  of bounds (an empty reply segfaults it; two members print garbage). Here a missing member is
  `null`, so the command fails with a normal error (`error: ...`, exit 1). Not comparable with
  stock; five rows of group `deviations` (empty and short batches for both commands) check the
  error, the exit code and that nothing panics.
- **`getnewaddress` failing with error code 0 under `-generate`.** Stock's `ParseError` leaves
  the request handler null for a code of 0, and it dereferences it: the process aborts. Here the
  RPC error is printed on standard error and the exit code is 1, as for any other RPC error, so
  a script never reads a failed generate as a success. Two rows of group `deviations` (with and
  without a count), with stock's abort as the premise.
- **`100 Continue` followed at once by the peer closing.** Whether stock sees the close before
  the data is a race inside libevent (observed both ways). Not asserted; the two deterministic
  cases (reply on the same open connection, reply later) are.
- **Arguments must be UTF-8.** Stock passes raw bytes on; `main` refuses a non-UTF-8 argument
  with exit code 1. The message names the argument's position and, before the command, the
  option when it is one that `dash-cli` registers (`-rpcpassword`, `-norpcwait`); any other
  key may be a secret put in the wrong place, so it is named by position only. It never
  contains any of the argument's content, because that may be a password (`-rpcpassword=...`). Options end at the
  first argument that is not one, as in Core: after the command a dash-led argument is the
  command's value (a passphrase, say) and is named by its position only. Twelve rows of group
  `deviations` run raw argv with a canary and check that it is not echoed.
- **Parameters read from standard input must be UTF-8.** A `-stdin` line or a
  `-stdinwalletpassphrase` passphrase that is not UTF-8 makes stock write invalid JSON into the
  request. Here it is the error `standard input is not valid UTF-8` (exit 1), so that a value is
  never sent changed. Checked for its own behaviour (group `deviations`).
- **A signal ends the password prompt.** Stock turns terminal echo off for the prompt and, when
  SIGINT, SIGTERM, SIGHUP or SIGQUIT ends the process there, leaves it off: the user's shell
  then types invisibly. Here handlers installed for the duration of the prompt restore the
  saved termios, put the signal's default action back and raise it again, so the exit status is
  the signal's, as stock's is (stock's SIGQUIT ends as SIGABRT). The handlers block SIGTTOU
  while they run: from a background job (`kill %1`) writing the termios would stop the process
  and it could not die. A signal that was ignored when the prompt started (`trap '' INT`) is
  left ignored, as it is for stock. `NoEcho` values nest: the first saves the terminal settings and installs the handlers, the
  last to drop (in any order) restores both; a unit test on a pseudo-terminal drops two in each
  order. Six rows of group `deviations`: each signal through a pseudo-terminal with the echo
  flag read afterwards (stock's echo-off is the premise), SIGINT ignored by the parent, and a
  prompt in a background job that gets SIGTERM and SIGCONT.
- **Bounded replies.** libevent's client reads any reply (a stock dash-cli took a 16 MiB header
  line, a million headers and a 16 MiB status line in a probe, and any body), so a hostile or
  impersonated server can use all the memory of the process. Here (`http::Limits::DEFAULT`)
  a status, header or chunk-size line is at most 64 KiB, a reply has at most 1000 header lines
  (interim replies and trailer included) and 256 KiB of status and header lines together, and
  the body (the chunks added up, or everything up to the close) at most 1 GiB. All of dashd's
  replies are far inside: a handful of headers under 1 KiB; the largest bodies (`getblock` with
  verbosity 2, wallet listings) a few hundred MiB at most. Beyond a limit the request fails as
  libevent's own bounds do, with error code 5 (`response body is larger than allowed`); limits
  count bytes received, so a short body behind a huge `Content-Length` is still the EOF it is
  for stock. The limits bound what the HTTP layer holds, not what the JSON parser then needs
  for a large body: a dense array of small values costs a few tens of times its size (20 MB of
  `[0,0,...]` reached 1.3 GiB), here as in stock, so this is a bound on the transport and not
  a memory guarantee. Rows of group `http` pass replies just inside each limit through both programs;
  eight rows of group `deviations` fail just beyond them (stock accepts the five header and
  line replies, which the rows use as their premise; the three 1 GiB bodies, by
  `Content-Length`, by chunks and up to the close, are streamed to rdash-cli only).
- **Windows.** No `WSAStartup`, no console colour or `SetupEnvironment` handling; terminal
  detection and echo suppression use `isatty`/termios (`libc`) and are Unix-only. The XCLI
  test is not built there (see below).

## Not ported

- The libevent `event_base` and its logging. `-rpcclienttimeout` is the socket's idle timeout
  instead (compared with a server that never answers). libevent's timeout is also an idle
  timeout between reads but is restarted by its own event loop; a server that trickles bytes
  slowly, or takes long over the whole request, is not modelled and no row covers it.

## What the test covers

`cargo test -p rdash-cli` runs `tests/xcli`:

- `xcli_offline`: needs only the stock binary. Command-line and configuration handling, every
  HTTP failure path against a scripted server (also comparing the request bytes each program
  sent), JSON formatting, error objects, `-rpcwait`, credentials/cookies/wallet URLs, `-stdin*`,
  `-named`, `-getinfo`/`-netinfo`/`-addrinfo`/`-generate` against canned replies (wide columns,
  odd transports, proxies, balance tables, `%.4f` ties), handler precedence, an 80-round fuzz of
  the `-netinfo` peer order, a closed output pipe, pseudo-terminal rows (`-color=auto` on a
  terminal, the `RPC password> ` and `Wallet passphrase> ` prompts, echo off while typing, the
  newline printed after terminal input), and the request each program sends for all 289
  commands of `dash-rpc-spec`, positional and `-named`.
- `xcli_dashd`: both programs against live v24 regtest nodes (cookie and password auth, two
  wallets, peers, address manager entries, a node started while the CLIs wait, and a pair of
  byte-identical nodes for `-generate` and other state-changing commands).
- `xcli_deviations`: the places where rdash-cli differs on purpose (above), each checked for its
  own behaviour and, where stock is cheap to run, for the premise that stock behaves otherwise.

A row that expects a failure is about the failure it names: the stock binary's output must
contain the text recorded for the row (`tests/xcli/support/says.rs`, taken from the first line
stock printed), and a failing row with no entry fails the test. Fake-server rows (`fake_row*`)
also check how many requests the stock binary received (at least one, unless the row is listed
as failing before any request), and the HTTP failure rows require exactly one `getblockcount`.

**Platforms.** Full parity is proven on Linux only. On other Unix systems `xcli_offline` runs the
portable subset (command-line, configuration and the conversion sweep) when a pinned dashd is
found, `xcli_dashd` and `xcli_deviations` return early, and `xcli_full_parity_is_linux_only`
appears as an ignored test with the reason; where the harness cannot be built (not Unix),
`xcli_unsupported.rs` does the same with `xcli_not_run_on_this_platform`.

The v24 `dashd`/`dash-cli` come from `DASHD_V24_PATH`, `ST2_DASHD`, `DASHD_PATH` or the
`contrib/setup-dashd.py` cache (`DASHVERSION=24.0.0-rc.3`). `SKIP_DASHD_TESTS` skips all three tests. `XCLI_REPORT_DIR` receives the row-by-row report.

## Benchmarks

None. `rdash-cli` adds no bench target (PF0 has nothing to measure here).

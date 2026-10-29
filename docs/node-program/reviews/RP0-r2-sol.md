# RP0 round-2 review (Sol)

Range: `bf2b565d2..3e9967b7a` on `node/rp0-rpc-core`; review checkout at `3e9967b7a37f383040b043dba7de04e507cd73c6`.
Reference: Dash Core v24.0.0-rc.3.

## Findings

| Finding | Disposition | Evidence |
|---|---|---|
| R1 minor: signed HTTP version components were parsed as unsigned, so `HTTP/1.-1` and `HTTP/-1.1` differed from Core | **CLOSED** | Rust parses signed `i32` components in [`dashd-rpc/src/http/parse.rs:521-540`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse.rs:521), carries them through replies in [`dashd-rpc/src/http/reply.rs:31-49`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/reply.rs:31), and tests both forms in [`dashd-rpc/src/http/parse/tests.rs:183-191`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse/tests.rs:183) and DIFF-RPC cases in [`rdashd/tests/rpc_diff.rs:117-121`](/work/home/agent/workspace/rdc-wt/rp0/rdashd/tests/rpc_diff.rs:117). |
| R1 redacted major note | **CLOSED** | possible security note: see manager. The replacement resource model is one event thread plus fixed workers ([`dashd-rpc/src/http/mod.rs:390-400`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:390)); the required 600-connection test and thread-per-connection mutant are recorded below. |
| R-1: port Core's single event loop, bounded workers/queue, lifecycle behavior, and regression evidence | **CLOSED** | The event loop owns listener accept, connection I/O, replies, timeout expiry, and shutdown ([`dashd-rpc/src/http/mod.rs:567-639`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:567)); exactly `-rpcthreads` workers plus one `http` event thread are started ([`dashd-rpc/src/http/mod.rs:390-400`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:390)). The required test passed, and the mutant failed as intended. |

No new defects were found in the event-thread/worker handoff, queueing, descriptor lifecycle, starvation behavior, or idle CPU behavior.

## Core comparison

| Area | Rust head | Core v24.0.0-rc.3 | Result |
|---|---|---|---|
| Thread count | `start` creates `threads` workers and one event thread ([`mod.rs:390-400`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:390)); every accepted connection remains in `EventLoop::conns` ([`mod.rs:641-683`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:641)). | `StartHTTPServer` starts one `ThreadHTTP` and `rpcThreads` workers ([`src/httpserver.cpp:529-537`](/work/home/agent/workspace/dash/src/httpserver.cpp:529)). | Bounded independently of connection count. |
| Queue and 503 | Queue depth checks count waiting items, normal work has priority over external work, and interruption drains existing items ([`mod.rs:185-237`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:185)). A full queue returns HTTP 503 with the exact Core text ([`mod.rs:883-897`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:883)). | Core's `WorkQueue::Enqueue/Run` has the same depth and priority rules ([`src/httpserver.cpp:67-137`](/work/home/agent/workspace/dash/src/httpserver.cpp:67)); callback returns “Work queue depth exceeded” (or external equivalent) with 503 ([`src/httpserver.cpp:355-370`](/work/home/agent/workspace/dash/src/httpserver.cpp:355)). | Matches. Unit test `a_full_queue_answers_503` passed. |
| `rpcservertimeout` | The argument is converted to Core's signed `int` domain ([`mod.rs:291-293`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:291)); `-1` and `0` disable expiry, values below `-1` map to an already-due deadline, and positive values use idle deadlines ([`mod.rs:327-334`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:327)). A connection awaiting a worker reply has no deadline ([`mod.rs:722-729`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:722)). | Core passes the value to `evhttp_set_timeout` and sets max limits ([`src/httpserver.cpp:493-495`](/work/home/agent/workspace/dash/src/httpserver.cpp:493)); libevent treats `-1` as no timeout, zero as no timer in the measured v24 behavior, and a negative value below `-1` as immediately due. | Matches measured v24 behavior for 0, -1, below -1, positive, and 32-bit wrap cases. Timeout tests passed; a worker-reply probe also confirmed in-flight work is not expired. |
| Keep-alive and pipelining | Parser preserves buffered bytes and `Conn::awaiting` permits one request at a time; the next request is driven only after the prior reply is fully written ([`mod.rs:704-790`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:704)). Reply rendering implements HTTP/1.1 persistence, HTTP/1.0 keep-alive, content length, and close handling ([`reply.rs:53-100`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/reply.rs:53)). | Core's `evhttp_send_done` only associates the next request after the response is written and closes according to version/connection headers `http.c:2770-2784`. | Matches. Parser pipelining, HTTP/1.0 keep-alive, interface HTTP, and DIFF cases passed. |
| Shutdown | `stop` interrupts and joins workers, wakes the event thread, closes listeners, waits for `awaiting`/pending output, then closes connections ([`mod.rs:411-430`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:411), [`mod.rs:623-635`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/mod.rs:623)). | Core joins workers, removes listener sockets, waits for the request tracker, schedules `evhttp_free` on the event thread, then joins it ([`src/httpserver.cpp:552-590`](/work/home/agent/workspace/dash/src/httpserver.cpp:552)). | In-flight replies and listener closure are covered and match the v24 ordering. |
| Large bodies | Header limit is 8192 and body limit is `0x0200_0000` ([`parse.rs:6-11`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse.rs:6)); Content-Length and chunk accumulation reject over-limit bodies before extending beyond the cap ([`parse.rs:290-337`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse.rs:290), [`parse.rs:617-626`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse.rs:617)). Large buffers are shrunk back to 64 KiB after a request ([`parse.rs:280-283`](/work/home/agent/workspace/rdc-wt/rp0/dashd-rpc/src/http/parse.rs:280)). | Core configures `MAX_HEADERS_SIZE` and `MAX_SIZE` through libevent ([`src/httpserver.cpp:493-495`](/work/home/agent/workspace/dash/src/httpserver.cpp:493)); chunk accounting rejects when accumulated body exceeds the configured max `http.c:948-959`. | Body/header limits match for RPC requests. The branch intentionally bounds an unterminated chunk-size line at the header limit (D13), while libevent itself does not impose that line-length cap. The method body scope (POST/PUT/PATCH) is inherited from the prior parser and is outside this round. |

## Tests and probes

All commands used the private target directory `/work/scratch/rp0-r2-sol/target`.

- `cargo test -p dashd-rpc --all-features -- --test-threads=4`: **59 passed, 0 failed**.
- `cargo test -p rdashd --all-features --test rpc idle_connections_hold_no_threads -- --nocapture --test-threads=1`: **passed**. Output: `threads: 20 before, 20 with 600 connections held, 5 of them the server's (-rpcthreads=4)`.
- Thread-per-connection mutant: detached `bf2b565d2` checkout with the new test copied in; test failed as intended with `threads: 21 before, 621 with 600 connections held, 606 of them the server's` and assertion `expected 5, got 606`.
- `cargo test -p rdashd --all-features --test rpc interface_http ...`: **passed**.
- `cargo test -p rdashd --all-features --test rpc requests_run_on_parallel_workers ...`: **passed**.
- `cargo test -p rdashd --all-features --test rpc -- --test-threads=1`: **15 passed, 0 failed**.
- Idle CPU probe with 500 idle plus 100 half-sent sockets, `-rpcservertimeout=0`, `-rpcthreads=4`: process CPU ticks were **0 over 5 seconds (0.0%)**.
- Descriptor-starvation probe with RLIMIT_NOFILE=1024: 164 connections held, then **1 process tick over 5 seconds**; no accept-retry spin was observed.

ACCEPT


#!/usr/bin/python3
"""A stand-in for the PF0 stub in node-bench's run_attempt tests: the daemon argv's `-mode=` picks a behaviour.

    refuse    print rdashd's refusal line, exit 1
    spew      1 MiB of output, then the refusal line, exit 1
    serve     answer getblockcount on $PF0_TEST_PORT until SIGTERM, exit 0
    garbage   answer every request with HTTP 500 and a non-JSON body until SIGTERM
    stubborn  serve, and ignore SIGTERM
    touch     touch <datadir>/ready, then wait for SIGTERM (no RPC)
    quiet     wait for SIGTERM without any output, like rdashd with -printtoconsole=0 (no RPC, no ready file)
    late      on SIGTERM print the refusal line and exit 1 (it refused after the settle check)
    dirty     on SIGTERM print a line and exit 3
    killed    on SIGTERM die by it (stopped before its handlers were installed)
    serve-late   serve, and on SIGTERM print the refusal line and exit 1 (it refused after it answered RPC)
    serve-dirty  serve, and on SIGTERM print a line and exit 3
    fresh     replace <datadir> with a new directory, as the broker does for every spawn, then like touch
    retry     with -txindex: touch <datadir>/ready, and on SIGTERM refuse -txindex (exit 1); without: quiet
    late-server  with -server: serve-late, refusing -server; else with -txindex: refuse -txindex; else: serve
    nomethod  serve, answering every method with -32601 Method not found (RP0's server without the method)
    partial   serve: getblockcount answers, getblocktemplate and `protx list` are -32601, any other call a parameter
              error (-1)
    blockcount-error  serve: getblockcount answers an internal error (-32603)
    stopping  answer HTTP 503 (an interrupted server's page) for 1 s, then print the refusal line and exit 1
    unavailable  answer HTTP 503 until SIGTERM, exit 0
    nomethod-txindex  with -txindex: refuse -txindex; else: nomethod
"""

import http.server
import json
import os
import shutil
import signal
import sys
import threading

mode = next(a.split("=", 1)[1] for a in sys.argv if a.startswith("-mode="))
REFUSAL = "Error: -mode is not supported by rdashd yet (XX1)."
if mode == "retry":
    mode, REFUSAL = ("touch-late", "Error: -txindex is not supported by rdashd yet (XX1).") if "-txindex" in sys.argv \
        else ("quiet", REFUSAL)
if mode == "nomethod-txindex":
    mode, REFUSAL = ("refuse", "Error: -txindex is not supported by rdashd yet (XX1).") if "-txindex" in sys.argv \
        else ("nomethod", REFUSAL)
if mode == "late-server":
    if "-server" in sys.argv:
        mode, REFUSAL = "serve-late", "Error: -server is not supported by rdashd yet (XX2)."
    elif "-txindex" in sys.argv:
        mode, REFUSAL = "refuse", "Error: -txindex is not supported by rdashd yet (XX1)."
    else:
        mode = "serve"
if mode in ("refuse", "spew"):
    if mode == "spew":
        sys.stdout.write(("x" * 99 + "\n") * 10486)
    print(REFUSAL, flush=True)
    sys.exit(1)


UNAVAILABLE = b"<HTML><HEAD>\n<TITLE>503 Service Unavailable</TITLE>\n</HEAD><BODY>\n<H1>Service Unavailable</H1>\n"


def rpc_error(status, code, message):
    return status, json.dumps({"result": None, "error": {"code": code, "message": message}, "id": 1}).encode()


def answer(method, params):
    """(HTTP status, body) for a JSON-RPC request of `method` with `params`."""
    if mode == "garbage":
        return 500, b"<html>no</html>"
    if mode in ("stopping", "unavailable"):
        return 503, UNAVAILABLE + b"</BODY></HTML>\n"
    if mode == "nomethod" or (mode == "partial" and (method == "getblocktemplate" or [method, *params][:2] == [
            "protx", "list"])):
        return rpc_error(404, -32601, "Method not found")
    if mode == "partial" and method != "getblockcount":
        return rpc_error(500, -1, f"{method} needs parameters")
    if mode == "blockcount-error" and method == "getblockcount":
        return rpc_error(500, -32603, "internal error")
    return 200, b'{"result": 0, "error": null, "id": 1}'


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        status, body = answer(req["method"], req.get("params") or [])
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


if mode in ("touch", "quiet", "late", "dirty", "killed", "fresh", "touch-late"):
    if mode in ("touch", "fresh", "touch-late"):
        datadir = next(a.split("=", 1)[1] for a in sys.argv if a.startswith("-datadir="))
        if mode == "fresh":
            shutil.rmtree(datadir, ignore_errors=True)
        os.makedirs(datadir, exist_ok=True)
        open(os.path.join(datadir, "ready"), "w").close()
    done = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: done.set())
    done.wait(60)
    if mode in ("late", "touch-late"):
        print(REFUSAL, flush=True)
    if mode == "dirty":
        print("init message: loading", flush=True)
    if mode == "killed":
        signal.signal(signal.SIGTERM, signal.SIG_DFL)
        os.kill(os.getpid(), signal.SIGTERM)
    sys.exit({"late": 1, "touch-late": 1, "dirty": 3}.get(mode, 0))

srv = http.server.HTTPServer(("127.0.0.1", int(os.environ["PF0_TEST_PORT"])), Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()
stop = threading.Event()
signal.signal(signal.SIGTERM, (lambda *_: None) if mode == "stubborn" else (lambda *_: stop.set()))
if mode == "stopping":  # an init error after the HTTP server started: rdashd interrupts it, then exits
    stop.wait(1)
    print(REFUSAL, flush=True)
    sys.exit(1)
stop.wait()
if mode == "serve-late":
    print(REFUSAL, flush=True)
    sys.exit(1)
if mode == "serve-dirty":
    print("shutdown: dirty", flush=True)
    sys.exit(3)
sys.exit(0)

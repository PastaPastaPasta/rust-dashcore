"""PF0 mode (`workload --pf0`): node-bench as perfgate's trusted driver of an untrusted node (PF0-N plan rev4.1 §1,
§2, D31, D46, D59).

perfgate runs node-bench in a bwrap view that holds no node binary. Every daemon start goes through the stub at
`/pf0/bin/node-launch`, which asks the broker (socket `/pf0/ctl/broker`) for one spawn of a class; the broker builds
the jail, creates the datadir fresh and does the counting. So in PF0 mode node-bench:

- names only the stub (`--rust-bin` is forced to it, no `--wrap`, no CPU slices: placement is the broker's);
- uses the fixed per-run paths and ports below, never `free_port()` or `mkdtemp` for a daemon argv: the
  invocation's own netns makes fixed ports safe, and every argv field is then pinned (D31);
- never creates, copies or reuses a node datadir (the broker does, D46);
- caps every RPC response it reads.

Before a row is measured, a **preflight batch** (one invocation) runs the exact measured argv as class-preflight
spawns to learn whether rdashd accepts it (§7). rdashd refuses an option it doesn't implement yet with one line,
written by `not_supported()` (dashd-node init/support.rs), and names only the first; each retry drops exactly that
token, so the record lists every refused option with its owner. A start that finishes init and answers RPC but lacks a
method the row needs (`-32601 Method not found`: warmup is answered before any method lookup, so init is done)
accepted the argv; the record then also names each missing method, as `rpc:<method>` owned by PENDING (D113).
"""

import hashlib
import http.client
import os
import re
import signal
import subprocess
import time

STUB = "/pf0/bin/node-launch"
RUN = "/pf0/run"
NODE_DATADIR = "/pf0/run/node"
VERSION_DATADIR = "/pf0/run/version"
SEED_DATADIR = "/pf0/run/seed"
MANIFEST = "/pf0/run/manifest.json"
NODE_RPC, NODE_P2P, SEED_RPC, SEED_P2P = 19501, 19502, 19503, 19504
RPC_RESPONSE_CAP = 64 << 20

# The preflight contract (node input component 17): the refusal line, the retry rules, and this version.
PREFLIGHT_VERSION = 2  # 2: a missing RPC method is a refusal (`rpc:<method>`, PENDING), an HTTP 503 not yet ready
REFUSAL_RE = re.compile(r"^Error: (-[a-z0-9-]+) is not supported by rdashd yet \(([A-Za-z0-9-]+)\)\.$")
METHOD_PREFIX, METHOD_OWNER = "rpc:", "PENDING"  # a refused_on entry for a method the node lacks
MAX_RETRIES = 32  # the preflight spawn budget is 1 + this (D75 as amended by D89)
ARGV_VOLATILE = ()  # options dropped before hashing the argv: none (§7; an entry needs its own D-row)
STOP_WAIT = 120
NOT_READY = (-28, 401, -503)  # RPC warmup; an interrupted HTTP server (503: stopping, or its work queue full)
LOG_TAIL = 65536
# After a refusal, a start that runs this long without refusing accepted the argv (D89, D93); it must then stop with
# exit 0 after SIGTERM: a start stopped before its refusal check exits with its refusal or by the signal, never 0.
SETTLE_S = 30

_on = False


class Pf0Error(Exception):
    """Something PF0 mode forbids (a refused daemon argv field, an oversized RPC response)."""


def enable():
    global _on
    _on = True


def on():
    return _on


def prep_datadir(row):
    return f"/pf0/prep/{row}/datadir"


def stub_argv(cls, argv):
    """The stub's command line for one spawn of `cls` with the daemon arguments `argv` (argv[0] is ignored: the
    broker runs the side's exe). Only the run class is measured."""
    measure = "counter" if cls == "run" else "none"
    return [STUB, "--client", "--class", cls, "--measure", measure, "--", "rdashd", *argv]


def check_args(args):
    """`workload --pf0` takes no node binary, wrapper or slice of its own."""
    problems = []
    if args.impl != "rust":
        problems.append("--impl rust")
    if args.rust_bin not in (None, STUB):
        problems.append(f"no --rust-bin (the daemon is the stub {STUB})")
    if args.wrap or args.ctl_fifo:
        problems.append("no --wrap or --ctl-fifo (the broker counts)")
    if args.node_slice or args.aux_slice:
        problems.append("--node-slice '' --aux-slice '' (the broker places the node)")
    if args.keep:
        problems.append("no --keep")
    if problems:
        raise SystemExit("workload --pf0 needs " + "; ".join(problems))
    args.rust_bin = STUB


class PreflightError(Pf0Error):
    """A preflight that is neither an acceptance nor a refusal by the contract: the row is an ERROR."""


def argv_sha256(argv):
    """sha256 over the NUL-joined daemon argv of the first preflight, the stub path included (§7, D31)."""
    kept = [a for a in argv if a.split("=", 1)[0] not in ARGV_VOLATILE]
    return hashlib.sha256(b"\0".join(a.encode() for a in kept)).hexdigest()


def printable(text):
    """Node-written `text` with every non-printable character escaped: it reaches manifests, logs and reports."""
    return "".join(c if c.isprintable() else c.encode("unicode_escape").decode("ascii") for c in str(text))


def refusal(lines):
    """(option, owner) when the last non-empty output line is rdashd's refusal, else None."""
    last = next((x.strip() for x in reversed(lines) if x.strip()), "")
    m = REFUSAL_RE.match(last)
    return (m.group(1), m.group(2)) if m else None


def _token(argv, option, original):
    """The index of the one token in `argv` that sets `option` (`-name` or `-name=…`), which must also be a token
    of the original argv."""
    hits = [i for i, a in enumerate(argv) if a == option or a.startswith(option + "=")]
    if not hits or not any(a == option or a.startswith(option + "=") for a in original):
        raise PreflightError(f"rdashd refused {option}, which is not in the row's argv")
    if len(hits) > 1:
        raise PreflightError(f"rdashd refused {option}, which the row's argv sets {len(hits)} times")
    return hits[0]


def preflight(argv, attempt):
    """The preflight batch over `argv` (argv[0] the stub). `attempt(argv, i)` runs one class-preflight spawn and
    returns ("ready", how) when the node accepted the argv (how: "ready" or "settled"), ("missing", methods) when it
    accepted the argv and answers RPC without those methods, or ("exit", (code, output lines)). Returns the record
    {status: ok|refused, argv_sha256, refused_on, attempts, accepted_by, version}; anything else raises
    PreflightError."""
    original, cur, refused_on = list(argv), list(argv), []
    # Each retry drops a distinct original token, so a row with fewer options runs out of them first: a refusal
    # past the last is "not in the row's argv".
    for i in range(1 + MAX_RETRIES):
        kind, detail = attempt(cur, i)
        if kind == "missing":  # accepted, and final: the argv needs no more retries
            refused_on += [{"option": METHOD_PREFIX + m, "owner": METHOD_OWNER} for m in detail]
            kind, detail = "ready", "answered"
        if kind == "ready":
            return {"status": "refused" if refused_on else "ok", "argv_sha256": argv_sha256(original),
                    "refused_on": refused_on, "attempts": i + 1, "accepted_by": detail,
                    "version": PREFLIGHT_VERSION}
        code, lines = detail
        r = refusal(lines) if code == 1 else None
        if r is None:
            tail = next((x.strip() for x in reversed(lines) if x.strip()), "no output")
            raise PreflightError(f"preflight attempt {i + 1}: exit {code}, not a refusal: {printable(tail[:300])}")
        option, owner = r
        if any(x["option"] == option for x in refused_on):
            raise PreflightError(f"rdashd refused {option} twice")
        del cur[_token(cur, option, original)]
        refused_on.append({"option": option, "owner": owner})
    raise PreflightError(f"rdashd still refuses after {len(refused_on)} attempts "
                         f"({', '.join(x['option'] for x in refused_on)})")


def log_tail(path):
    """The last LOG_TAIL bytes of a log the node wrote, as lines (never the whole file: RUN is GiB-sized)."""
    with open(path, "rb") as f:
        f.seek(max(0, os.fstat(f.fileno()).st_size - LOG_TAIL))
        return f.read(LOG_TAIL).decode(errors="replace").splitlines()


def _dir_id(path):
    """(st_dev, st_ino) of `path` itself (never followed), or None when it is absent."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        return None
    return st.st_dev, st.st_ino


def _ready_probe(ready, stale, methods=()):
    """A check that returns None before the start shows the row's ready signal, then the calls of `methods`
    ((method, *subcommand) tuples) the node lacks, as "method[ subcommand]" names ([] for none); raises for an
    answer that is no node's. `stale` is the node datadir's _dir_id before this spawn. With ready "rpc",
    getblockcount is the signal and is always needed: it answers, or it is missing; any other error raises."""
    if ready not in ("file", "rpc"):
        raise ValueError(f"unknown ready signal {ready!r}")
    if ready == "file":
        def probe():
            # The marker counts only in this spawn's datadir (D99): the broker replaces the datadir for every spawn
            # (rmtree, then mkdir: a new inode, which a tmpfs does not reuse at once), so until it has, a retry
            # would see the previous attempt's marker. Never followed or opened: the node wrote that datadir. The
            # datadir must be the same one before and after the marker check.
            d = _dir_id(NODE_DATADIR)
            if d is None or d == stale or _dir_id(f"{NODE_DATADIR}/ready") is None:
                return None
            return [] if _dir_id(NODE_DATADIR) == d else None
        return probe, (lambda: None)
    from .rpc import Rpc, RpcError

    rpc = Rpc(NODE_RPC, "bench", "bench", timeout=30)

    def lacks(call):
        """Whether the node answers `call` ((method, *subcommand)) with Method not found. Any other JSON-RPC answer is
        the method's own (a parameter error, say); an HTTP-level failure or warmup is no answer, and raises."""
        try:
            rpc.call(*call)
        except RpcError as e:
            if e.method != call[0] or e.code in NOT_READY:
                raise
            return e.code == -32601
        return False

    def probe():
        try:
            try:  # the ready signal: an answer, or Method not found; any other error is the node's (an error)
                rpc.call("getblockcount")
                missing = []
            except RpcError as e:
                if e.method != "getblockcount" or e.code != -32601:
                    raise
                missing = ["getblockcount"]
            return missing + [" ".join(c) for c in methods if c != ("getblockcount",) and lacks(c)]
        except RpcError as e:  # a stopping node's exit decides; one never past these is not ready by the deadline
            if e.code in NOT_READY:
                return None
            raise
        except (ConnectionError, OSError, http.client.HTTPException):
            rpc.close()
            return None
    return probe, rpc.close


def run_attempt(argv, i, ready="rpc", deadline=None, methods=()):
    """One class-preflight spawn through the stub. Accepted ("ready", how) once the row's ready signal shows
    ("ready"; ("missing", names) instead when the node lacks getblockcount or RPC methods of `methods`, D113), or,
    after a refusal (i > 0), once the start runs SETTLE_S without refusing ("settled": the remaining argv may have
    lost what the ready signal needs, D89). Either way the start must then stop with exit 0 (D93, D99): one that
    stops with its refusal is that refusal (a late one, one between the poll and the settle check, or one after a
    ready signal: a PR node is not trusted to refuse before it answers); any other exit is an error, so a start too
    slow to reach its refusal check never shortens `refused_on`. Else its exit code and output. Without `deadline`
    the broker's deadline_preflight bounds it (the broker kills the spawn and the stub exits). Every path ends with
    the stub gone."""
    log = f"{RUN}/preflight-{i}.log"
    stale = _dir_id(NODE_DATADIR)  # before the spawn: the broker makes this spawn's datadir after it
    with open(log, "wb") as out:
        p = subprocess.Popen(stub_argv("preflight", argv[1:]), stdin=subprocess.DEVNULL, stdout=out,
                             stderr=subprocess.STDOUT)
    try:
        t0 = time.monotonic()  # not the node's first output: with -printtoconsole=0 an accepting start writes none
        probe, close = _ready_probe(ready, stale, methods)
        how, why, missing = None, None, None
        try:
            while p.poll() is None:
                missing = probe()
                if missing is not None:
                    how = "ready"
                elif i > 0 and time.monotonic() - t0 >= SETTLE_S:
                    how = "settled"
                elif deadline is not None and time.monotonic() - t0 > deadline:
                    why = f"no ready signal in {deadline} s"
                if how or why:
                    break
                time.sleep(0.05)
        except Exception as e:  # noqa: BLE001 — whatever a PR node answers, the attempt is an error, not a crash
            why = e
        finally:
            close()
        ready_ok = how is not None
        if not ready_ok and p.poll() is not None:
            p.wait()
            return "exit", (p.returncode, log_tail(log))
        p.send_signal(signal.SIGTERM)  # the stub turns it into {stop}; the broker stops the node
        try:
            p.wait(STOP_WAIT)
        except subprocess.TimeoutExpired:
            raise PreflightError(f"preflight attempt {i + 1}: the node did not stop within {STOP_WAIT} s of "
                                 "SIGTERM") from None
        if not ready_ok:
            raise PreflightError(f"preflight attempt {i + 1}: running, but not ready: {printable(why)}"[:300])
        if p.returncode != 0:
            lines = log_tail(log)
            if refusal(lines):  # it refused after all: preflight() takes it only with exit 1
                return "exit", (p.returncode, lines)
            hint = f" (init not done in {SETTLE_S} s: load?)" if how == "settled" else ""
            raise PreflightError(f"preflight attempt {i + 1}: {how}, but stopped with {p.returncode}{hint}: "
                                 f"{printable(next((x for x in reversed(lines) if x.strip()), 'no output'))}"[:300])
        return ("missing", missing) if missing else ("ready", how)
    finally:
        if p.poll() is None:
            p.kill()
            p.wait()

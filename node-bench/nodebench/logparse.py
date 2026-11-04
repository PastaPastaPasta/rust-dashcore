"""Per-block log parsers.

dashd: `UpdateTip:` lines are always logged; with -debug=bench every connected
block also logs "- Verify N txins: X ms" and "- Connect block: X ms".

jsonl: the contract for the Rust node. One JSON object per connected block in
<datadir>/bench.jsonl:
    {"height": 123, "ts": 1700000000.123456, "connect_ms": 1.9,
     "verify_txins": 42, "verify_ms": 0.8}
`ts` is the unix time the block became the tip; the other fields are optional
but needed for the matching metrics.
"""

import json
import re
import time
from datetime import datetime, timezone

RE_TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z ")
RE_UPDATETIP = re.compile(r"UpdateTip: new best=(\w+) height=(\d+) .* tx=(\d+) ")
RE_VERIFY = re.compile(r"- Verify (\d+) txins: ([\d.]+)ms")
RE_CONNECT_BLOCK = re.compile(r"^- Connect block: ([\d.]+)ms")
RE_CONNECT_TXS = re.compile(r"- Connect (\d+) transactions: ([\d.]+)ms")
RE_CACHE = re.compile(r"\* Using ([\d.]+) MiB for ")
RE_THREADS = re.compile(r"Script verification uses (\d+) additional threads")
RE_ASSUME = re.compile(r"Assuming ancestors of block (\w+) have valid signatures")
RE_CNB = re.compile(r"CreateNewBlock\(\) packages: ([\d.]+)ms \((\d+) packages, (\d+) updated descendants\), "
                    r"validity: ([\d.]+)ms \(total ([\d.]+)ms\)")


def parse_ts(s):
    return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()  # dashd logs UTC


def iter_dashd_blocks(path):
    """Yield one dict per connected block: height, ts, and bench fields when present."""
    cur = {}
    with open(path, errors="replace") as f:
        for line in f:
            m = RE_TS.match(line)
            if not m:
                continue
            msg = line[m.end():]
            # Category prefixes ("[bench] ") appear in some builds; strip them.
            if msg.startswith("["):
                msg = msg.split("] ", 1)[-1]
            if "UpdateTip:" in msg:
                u = RE_UPDATETIP.search(msg)
                if not u:
                    continue
                if "height" in cur and "connect_ms" not in cur:
                    # No bench logging: every UpdateTip closes a block.
                    yield cur
                    cur = {}
                cur.update(height=int(u.group(2)), ts=parse_ts(m.group(1)), chain_tx=int(u.group(3)))
                continue
            v = RE_VERIFY.search(msg)
            if v:
                cur["verify_txins"], cur["verify_ms"] = int(v.group(1)), float(v.group(2))
                continue
            t = RE_CONNECT_TXS.search(msg)
            if t:
                cur["txs"], cur["connect_txs_ms"] = int(t.group(1)), float(t.group(2))
                continue
            c = RE_CONNECT_BLOCK.search(msg.strip()) if msg.lstrip().startswith("- Connect block") else None
            if c:
                cur["connect_ms"] = float(c.group(1))
                if "height" in cur:
                    yield cur
                cur = {}
    if "height" in cur:
        yield cur


def iter_jsonl_blocks(path):
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def iter_blocks(path, fmt):
    return iter_dashd_blocks(path) if fmt == "dashd" else iter_jsonl_blocks(path)


def tip_time(path, fmt, height, since=None, offset=0):
    """Unix time at which block `height` first became the tip in this invocation, or None."""
    return (update_tip(path, fmt, height, since, offset) or (None, None))[0]


def iter_tips(path, fmt, since=None, offset=0):
    """(unix time, height, hash) of every tip update written by this invocation: dashd lines after byte
    `offset`; and in both formats only timestamps from `since` (its spawn time, 1 s slack) up to now. A log
    left over from an earlier run can never answer for this one."""
    lo, hi = (since - 1.0) if since is not None else None, time.time() + 5.0
    if fmt == "dashd":
        with open(path, errors="replace") as f:
            f.seek(offset)
            for line in f:
                if "UpdateTip:" in line:
                    m, u = RE_TS.match(line), RE_UPDATETIP.search(line)
                    if m and u:
                        ts = parse_ts(m.group(1))
                        if (lo is None or ts >= lo) and ts <= hi:
                            yield ts, int(u.group(2)), u.group(1)
        return
    for b in iter_jsonl_blocks(path):
        ts = b.get("ts")
        if ts is not None and (lo is None or ts >= lo) and ts <= hi:
            yield ts, b.get("height"), b.get("hash")


def update_tip(path, fmt, height, since=None, offset=0):
    """(unix time, block hash) of the first tip update to `height` in this invocation, or None."""
    for ts, h, block in iter_tips(path, fmt, since, offset):
        if h == height:
            return ts, block
    return None


def last_tip(path, fmt, since=None, offset=0):
    """(unix time, height, hash) of the last tip update in this invocation, or None."""
    last = None
    for tip in iter_tips(path, fmt, since, offset):
        last = tip
    return last


def init_config(path, offset=0):
    """What dashd logged at startup (after byte `offset`) about settings it has no RPC for:
    dbcache_mib (sum of the "Cache configuration" lines, whole MiB), script_threads (-par, main thread included) and
    assumevalid ("0" = every signature checked, else the assumed-valid block hash). None when not logged."""
    out = {"dbcache_mib": None, "script_threads": None, "assumevalid": None}
    cache, in_cache = 0.0, False
    with open(path, errors="replace") as f:
        f.seek(offset)
        for line in f:
            if "Cache configuration:" in line:
                cache, in_cache = 0.0, True
                continue
            c = RE_CACHE.search(line)
            if in_cache and c:
                cache += float(c.group(1))
                out["dbcache_mib"] = round(cache)  # whole MiB: the lines are rounded to 0.1 each
                continue
            in_cache = False
            t = RE_THREADS.search(line)
            if t:
                out["script_threads"] = int(t.group(1)) + 1
            a = RE_ASSUME.search(line)
            if a:
                out["assumevalid"] = a.group(1)
            elif "Validating signatures for all blocks" in line:
                out["assumevalid"] = "0"
    return out


def record_block_metrics(result, path, fmt, min_height=0):
    connect, interval, verify_ms, verify_in = [], [], 0.0, 0
    per_bucket = {}
    prev_ts = None
    n = 0
    for b in iter_blocks(path, fmt):
        ts = b.get("ts")
        if b.get("height", 0) < min_height:
            prev_ts = ts
            continue
        n += 1
        if "connect_ms" in b:
            connect.append(b["connect_ms"])
            per_bucket.setdefault(b["height"] // 250000, []).append(b["connect_ms"])
        if ts is not None and prev_ts is not None:
            interval.append((ts - prev_ts) * 1000)
        prev_ts = ts
        if b.get("verify_txins"):
            verify_in += b["verify_txins"]
            verify_ms += b.get("verify_ms", 0.0)
    result.info("block_count", n, "blocks", f"Blocks parsed from the per-block log at height >= {min_height}")
    if connect:
        defn = ("Per-block ConnectTip latency from the node's own instrumentation (dashd -debug=bench "
                "'- Connect block'; Rust: connect_ms). Includes disk read, validation, flush, index writes.")
        for stat in ("median", "mean", "p99"):
            result.metric(f"block_connect_ms_{stat}", connect, "ms", statistic=stat, definition=defn,
                          max_samples=2000, gate=False)
        for bucket, xs in sorted(per_bucket.items()):
            xs.sort()
            result.info(f"block_connect_ms_median_h{bucket * 250}k", xs[len(xs) // 2], "ms",
                        f"Median connect latency for heights [{bucket * 250}k, {(bucket + 1) * 250}k)")
    if interval:
        result.metric("block_interval_ms_median", interval, "ms", definition=(
            "Median wall time between consecutive tip updates (includes waiting for block download)."),
            gate=False, max_samples=2000)
    if verify_in and verify_ms:
        result.metric("script_verify_txin_per_s", [verify_in / (verify_ms / 1000)], "txin/s", better="higher",
                      gate=False,
                      definition=("Inputs verified per second of block-validation script/sig checking, summed over "
                                  "all blocks (dashd '- Verify N txins'). Only meaningful when scripts are actually "
                                  "checked: assumevalid=0 or blocks after the assumevalid point."))
        result.info("script_verify_txins", verify_in, "txins")


def parse_create_new_block(path, since_offset=0):
    """CreateNewBlock() bench lines (dashd -debug=bench) after byte offset `since_offset`."""
    out = []
    with open(path, errors="replace") as f:
        f.seek(since_offset)
        for line in f:
            m = RE_CNB.search(line)
            if m:
                out.append({"packages_ms": float(m.group(1)), "packages": int(m.group(2)),
                            "validity_ms": float(m.group(4)), "total_ms": float(m.group(5))})
    return out

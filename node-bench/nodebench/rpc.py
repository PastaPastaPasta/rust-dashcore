"""Minimal dashd-compatible JSON-RPC client (stdlib only, persistent connection)."""

import base64
import http.client
import json
import time

from . import pf0


class RpcError(Exception):
    def __init__(self, method, error):
        self.method = method
        self.code = error.get("code") if isinstance(error, dict) else None
        self.message = error.get("message") if isinstance(error, dict) else str(error)
        super().__init__(f"{method}: {self.message} (code {self.code})")


class Rpc:
    """One keep-alive HTTP connection. Not thread-safe: give each worker its own."""

    def __init__(self, port, user, password, host="127.0.0.1", timeout=600, wallet=None):
        self.host, self.port, self.timeout = host, port, timeout
        self.path = f"/wallet/{wallet}" if wallet else "/"
        token = base64.b64encode(f"{user}:{password}".encode()).decode()
        self.headers = {"Authorization": f"Basic {token}", "Content-Type": "application/json"}
        self.conn = None
        self._id = 0

    def _connect(self):
        self.conn = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)

    def close(self):
        if self.conn:
            self.conn.close()
            self.conn = None

    def raw(self, body):
        """POST an already-encoded body; returns the decoded response object."""
        for attempt in (0, 1):
            if self.conn is None:
                self._connect()
            try:
                self.conn.request("POST", self.path, body, self.headers)
                resp = self.conn.getresponse()
                data = resp.read(pf0.RPC_RESPONSE_CAP + 1) if pf0.on() else resp.read()
                break
            except (http.client.HTTPException, ConnectionError, OSError):
                # A stale keep-alive socket fails once; anything else fails twice.
                self.close()
                if attempt:
                    raise
        if pf0.on() and len(data) > pf0.RPC_RESPONSE_CAP:
            self.close()
            raise pf0.Pf0Error(f"RPC response over {pf0.RPC_RESPONSE_CAP >> 20} MiB (PF0 cap)")
        if resp.status == 401:
            raise RpcError("auth", {"code": 401, "message": "unauthorized"})
        try:
            return json.loads(data)
        except ValueError:
            raise RpcError("http", {"code": -resp.status, "message": f"HTTP {resp.status}: {data[:200]!r}"}) from None

    def call(self, method, *params):
        self._id += 1
        out = self.raw(json.dumps({"jsonrpc": "1.0", "id": self._id, "method": method, "params": list(params)}))
        if out.get("error"):
            raise RpcError(method, out["error"])
        return out["result"]

    def batch(self, calls):
        """calls: [(method, [params])]. Returns results in order; raises on the first error."""
        body = [{"jsonrpc": "1.0", "id": i, "method": m, "params": list(p)} for i, (m, p) in enumerate(calls)]
        out = sorted(self.raw(json.dumps(body)), key=lambda r: r["id"])
        for r in out:
            if r.get("error"):
                raise RpcError(calls[r["id"]][0], r["error"])
        return [r["result"] for r in out]

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        return lambda *p: self.call(name, *p)

    def wait_ready(self, timeout, proc=None, poll=0.005):
        """Block until the node answers getblockcount (i.e. RPC warmup finished).

        Returns seconds waited. Raises if `proc` exits or `timeout` elapses.
        """
        start = time.monotonic()
        while True:
            if proc is not None and proc.poll() is not None:
                raise RuntimeError(f"node exited with code {proc.returncode} before RPC was ready")
            try:
                self.call("getblockcount")
                return time.monotonic() - start
            except RpcError as e:
                if e.code not in (-28, 401):  # -28 = RPC_IN_WARMUP
                    raise
            except (ConnectionError, OSError, http.client.HTTPException):
                self.close()
            if time.monotonic() - start > timeout:
                raise TimeoutError(f"RPC not ready after {timeout}s")
            time.sleep(poll)

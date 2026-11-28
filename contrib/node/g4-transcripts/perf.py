#!/usr/bin/env python3
"""Measure instructions on the same 10k INV/GETDATA/NOTFOUND request transcript.

Setup and trace parsing are outside the perf interval. perf -p attaches every
existing thread; process startup, mining and teardown are excluded. Core includes
its network and background threads, while Rust uses the deterministic actor sim.
Usage: perf.py core OUT_DIR | perf.py rust OUT_DIR RELEASE_EXAMPLE
"""
import hashlib
import importlib.util
import json
import os
import pathlib
import signal
import socket
import struct
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("g4_timing", pathlib.Path(__file__).with_name("timing.py"))
cap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cap)
p2a = cap.p2a


class Counter:
    def __init__(self, pid, out):
        self.control = out / "control"
        self.ack = out / "ack"
        for path in [self.control, self.ack]:
            if path.exists():
                path.unlink()
            os.mkfifo(path, 0o600)
        self.ctrl_fd = os.open(self.control, os.O_RDWR)
        self.ack_fd = os.open(self.ack, os.O_RDWR)
        self.log = open(out / "instructions.txt", "w")
        self.proc = subprocess.Popen([
            "perf", "stat", "-e", "instructions:u", "-p", str(pid), "-D", "-1",
            "--control", f"fifo:{self.control},{self.ack}"], stderr=self.log)
        self.command("enable")

    def command(self, command):
        os.write(self.ctrl_fd, (command + "\n").encode())
        if not os.read(self.ack_fd, 100).startswith(b"ack\n"):
            raise RuntimeError("perf control did not acknowledge")

    def stop(self):
        self.command("disable")
        self.proc.send_signal(signal.SIGINT)
        self.proc.wait(timeout=30)
        self.log.close()
        os.close(self.ctrl_fd)
        os.close(self.ack_fd)
        self.control.unlink()
        self.ack.unlink()


def main():
    mode, directory = sys.argv[1:3]
    out = pathlib.Path(directory)
    out.mkdir(parents=True, exist_ok=True)
    batches = [bytes([20]) + b"".join(struct.pack("<I", 1) + hashlib.sha256(
        f"g4-request-{batch * 20 + i}".encode()).digest() for i in range(20)) for batch in range(500)]
    trace = "\n".join(payload.hex() for payload in batches)
    trace_path = out / "requests.txt"
    trace_path.write_text(trace + "\n")
    digest = hashlib.sha256(trace.encode()).hexdigest()
    started = time.monotonic()
    if mode == "rust":
        proc = subprocess.Popen([sys.argv[3], "requests", str(trace_path)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        if proc.stdout.readline().strip() != "READY":
            raise RuntimeError("Rust driver did not initialize")
        counter = Counter(proc.pid, out)
        proc.stdin.write("\n")
        proc.stdin.flush()
        done = proc.stdout.readline().strip()
        counter.stop()
        if proc.wait(timeout=30) != 0 or done != f"DONE {digest}":
            raise RuntimeError(f"Rust response mismatch: {done}")
    else:
        if cap.cap.p3.dashd_sha256() != cap.cap.DASHD_SHA256:
            raise RuntimeError("requires pinned Dash Core final binary")
        node = p2a.Node("tx-perf", 6, cap.cap.cap.SIM_START, ())
        peer = None
        try:
            node.mine(110)
            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            node.rpc("addnode", f"127.0.0.1:{listener.getsockname()[1]}", "onetry", False)
            listener.settimeout(120)
            sock, _ = listener.accept()
            listener.close()
            peer = p2a.Peer(0, sock)
            peer.send("version", p2a.version_payload(cap.cap.cap.FULL | cap.cap.cap.BIT11, p2a.VERSION_NONCE))
            peer.send("verack", b"")
            cap.barrier([peer], 1, lambda *_: None, drain=False)
            counter = Counter(node.proc.pid, out)
            responses = []
            for batch in batches:
                peer.send("inv", batch)
                got = []
                deadline = time.monotonic() + 120
                while len(got) < 20:
                    for cmd, body in peer.poll(0.01):
                        if cmd == "getdata":
                            if body[0] >= 0xfd or len(body) != 1 + body[0] * 36:
                                raise RuntimeError("unexpected GETDATA encoding")
                            got.extend(body[1 + i * 36:1 + (i + 1) * 36] for i in range(body[0]))
                        elif cmd == "ping":
                            peer.send("pong", body)
                        elif cmd == "closed":
                            raise RuntimeError("Core disconnected the trace peer")
                    if time.monotonic() > deadline:
                        raise RuntimeError("Core request timeout")
                response = bytes([len(got)]) + b"".join(got)
                if response != batch:
                    raise RuntimeError("Core response differs from request sequence")
                responses.append(response.hex())
                peer.send("notfound", response)
            cap.barrier([peer], 2, lambda *_: None, drain=False)
            counter.stop()
            if hashlib.sha256("\n".join(responses).encode()).hexdigest() != digest:
                raise RuntimeError("Core response digest mismatch")
        finally:
            if peer:
                peer.sock.close()
            node.stop()
    result = {"mode": mode, "requests": 10000, "batches": 500,
              "trace_and_response_sha256": digest,
              "wall_seconds_including_setup": time.monotonic() - started}
    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()

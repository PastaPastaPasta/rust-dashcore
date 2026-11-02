"""Helpers the StateDigest rig scripts share (O8 spec §9.1, §13)."""
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Command-line args of a recorded start that belong to the test run, not to the chain (and
# -txindex, which -reindex-chainstate refuses).
DROP = {"datadir", "mocktime", "statedigestlog", "recordstatedecisions", "masternodeblsprivkey", "connect",
        "addnode", "listen", "port", "bind", "rpcport", "reindex", "reindex-chainstate", "stopatheight",
        "stopafterblockimport", "txindex", "debug", "debugexclude", "loglevel", "logsourcelocations",
        "logthreadnames", "logtimemicros", "printtoconsole", "uacomment", "daemon"}

# Config-file keys that bind or connect; the copies run offline.
CONF_DROP = {"bind", "whitebind", "port", "rpcport", "rpcbind", "listen", "connect", "addnode"}


# chainargs.json's value for an arg Core logs as **** (ArgsManager::SENSITIVE)
REDACTED = "<redacted>"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def last_start(chainargs):
    return json.loads(Path(chainargs).read_text())["starts"][-1]


def run_args(chainargs):
    """The last start's command-line args minus DROP (the config file is in the datadir)."""
    return start_args(last_start(chainargs))


def start_args(start):
    """One start's command-line args minus DROP. Redacted (sensitive) values are not replayed;
    a negated arg (false) becomes -no<arg>."""
    args = []
    for key, values in sorted(start["cmdline"].items()):
        if key in DROP:
            continue
        args += ["-no" + key if v is False else f"-{key}={v}" for v in values if v != REDACTED]
    return args


def offline_copy(frozen, datadir):
    """Reflinks a frozen node datadir and drops the test's network settings from its config."""
    shutil.rmtree(datadir, ignore_errors=True)
    subprocess.run(["cp", "-a", "--reflink=always", str(frozen), str(datadir)], check=True)
    conf = datadir / "dash.conf"
    conf.write_text("".join(l for l in conf.read_text().splitlines(keepends=True)
                            if l.split("=", 1)[0].strip() not in CONF_DROP))


def run_group(cmd, timeout, **kwargs):
    """subprocess.run in its own process group, so that a timeout also kills the grandchildren
    (valgrind's dashd, for one). Raises CalledProcessError or TimeoutExpired."""
    proc = subprocess.Popen(cmd, start_new_session=True, **kwargs)
    try:
        rc = proc.wait(timeout=timeout)
    except BaseException:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()
        raise
    if rc != 0:
        raise subprocess.CalledProcessError(rc, cmd)


def rig_provenance():
    """The rig's commit, whether its directory is dirty, and its files' hashes."""
    git = ["git", "-C", str(HERE)]
    return {
        "rdc_commit": subprocess.run([*git, "rev-parse", "HEAD"], capture_output=True, text=True,
                                     check=True).stdout.strip(),
        "rig_dirty": bool(subprocess.run([*git, "status", "--porcelain", "--", "."], capture_output=True,
                                         text=True, check=True).stdout.strip()),
        "files": {p.name: sha256(p) for p in sorted(HERE.iterdir()) if p.is_file()},
    }


def checked_build_info(bin_dir):
    """build-info.txt as a dict, after checking its sha256 lines against the binaries beside it."""
    info, hashes = {}, {}
    for line in (bin_dir / "build-info.txt").read_text().splitlines():
        key, _, value = line.partition(" ")
        if key == "sha256":
            digest, name = value.split()
            hashes[name] = digest
        else:
            info[key] = value
    for name, digest in hashes.items():
        if sha256(bin_dir / name) != digest:
            raise SystemExit(f"{bin_dir / name} does not match its build-info.txt")
    info["sha256"] = hashes
    return info

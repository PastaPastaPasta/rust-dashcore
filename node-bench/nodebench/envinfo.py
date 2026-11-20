"""Environment capture recorded with every result file."""

import os
import platform
import socket
import subprocess
import time

from . import proc as procmod


def _run(cmd, cwd=None):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=30, cwd=cwd).stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return None


def _read(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return None


def git_rev(repo):
    if not repo or not os.path.isdir(repo):
        return None
    return {
        "path": repo,
        "commit": _run(["git", "rev-parse", "HEAD"], repo),
        "branch": _run(["git", "rev-parse", "--abbrev-ref", "HEAD"], repo),
        "dirty": bool(_run(["git", "status", "--porcelain", "--untracked-files=no"], repo)),
    }


def cpu_model():
    for line in (_read("/proc/cpuinfo") or "").splitlines():
        if line.startswith("model name"):
            return line.split(":", 1)[1].strip()
    return platform.processor()


def meminfo_gib():
    for line in (_read("/proc/meminfo") or "").splitlines():
        if line.startswith("MemTotal"):
            return round(int(line.split()[1]) / 1024 / 1024, 1)
    return None


def disk_for(path):
    out = _run(["findmnt", "-n", "-o", "SOURCE,FSTYPE,OPTIONS", "-T", path])
    dev = out.split()[0] if out else None
    rota = None
    if dev and dev.startswith("/dev/"):
        rota = _read(f"/sys/block/{os.path.basename(dev).rstrip('0123456789')}/queue/rotational")
    return {"mount": out, "rotational": rota}


def other_heavy_processes(min_cpu=20.0):
    """Processes using >= min_cpu % right now: evidence of a shared/noisy host."""
    out = _run(["ps", "-eo", "pcpu=,comm=", "--sort=-pcpu"]) or ""
    busy = []
    for line in out.splitlines()[:25]:
        pcpu, _, comm = line.strip().partition(" ")
        try:
            if float(pcpu) >= min_cpu:
                busy.append(f"{comm.strip()}:{pcpu}")
        except ValueError:
            pass
    return busy


def capture(bench_root, repo_root, cpu_slice=None, extra_repos=()):
    load = (_read("/proc/loadavg") or "").split()
    return {
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hostname": socket.gethostname(),
        "kernel": platform.release(),
        "os": (_read("/etc/os-release") or "").split("PRETTY_NAME=")[-1].split("\n")[0].strip('"'),
        "python": platform.python_version(),
        "cpu_model": cpu_model(),
        "cpu_count": os.cpu_count(),
        "numa": procmod.numa_nodes(),
        "cpu_governor": _read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "mem_total_gib": meminfo_gib(),
        "disk": disk_for(bench_root),
        "cpu_slice": cpu_slice,
        "slice_cpus": procmod.slice_cpu_count(cpu_slice),
        "loadavg_at_start": load[:3],
        "busy_processes_at_start": other_heavy_processes(),
        "virtualized": _run(["systemd-detect-virt"]),
        "rustc": _run(["rustc", "--version"]),
        "git": {"harness": git_rev(repo_root), **{os.path.basename(r): git_rev(r) for r in extra_repos}},
    }

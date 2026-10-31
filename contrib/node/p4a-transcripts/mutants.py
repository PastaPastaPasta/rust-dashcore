#!/usr/bin/env python3
"""P4a's mutation checks (P4a-notes D21): each mutant is applied, its test run, and the files
restored and touched (a restored file with an old mtime leaves cargo's mutant build current).

Usage: mutants.py [--out DIR] [M1 M2 ...]; logs go to DIR (default $TMPDIR)/mut-<name>.log.
Run from a clean tree; nothing is left modified. A mutant is killed when a test fails (a
`FAILED` or `panicked at` line); a mutant that does not build is reported as such, not killed.
"""

import os
import pathlib
import subprocess
import sys

WT = pathlib.Path(__file__).resolve().parents[3]
TRANSCRIPT = "dashd-p2p/src/sim/transcript.rs"
STUBS = "dashd-p2p/src/processing/stubs.rs"
HASH_FROM = "<dash_num::Hash256 as dash_types::Numeric>::from_lendian"
QFCOMMIT_READ = (
    "                let qc: Commitment = cx.read(&mut data)?;\n"
    "                // Core's `::SerializeHash(qc)`.\n"
    "                let inv = Inventory {\n"
    "                    inv_type: InvType::QuorumFinalCommitment,\n"
    "                    hash: qc.hash(),"
)
# The Unix function (the other platforms' one parses IP literals already).
NUMERIC_HOST = "pub fn numeric_host(name: &str) -> Option<NetAddr> {\n    let name = std::ffi::CString"

# (name, what, [(file, old, new)], command)
MUTANTS = [
    (
        "M1",
        "listener flags ignored",
        [
            (
                TRANSCRIPT,
                'parse_whitebind(&format!("{flags}@127.0.0.1:1")).expect("whitebind flags").1',
                "{ let _ = flags; Permissions::NONE }",
            )
        ],
        "cargo test -p dashd-p2p --test dashd_inbound_transcripts -- inbound_permissions",
    ),
    (
        "M2",
        "capture time +1 us on sends",
        [
            (
                "dash-p2p/src/netcore.rs",
                "capture.capture(self.clock.node_now_micros(), peer.addr(),",
                "capture.capture(self.clock.node_now_micros() + i64::from(!incoming), peer.addr(),",
            )
        ],
        "cargo test -p dashd-net --test dashd_inbound_policy -- inbound_capture",
    ),
    (
        "M3",
        "masternode peer left unverified",
        [(TRANSCRIPT, "s.handle.set_verified_proreg_hash(hash);", "let _ = hash;")],
        "cargo test -p dashd-p2p --test dashd_inbound_transcripts -- inbound_evict_mn",
    ),
    (
        "M4",
        "numeric hosts by IpAddr::parse",
        [
            (
                "dash-p2p/src/lookup.rs",
                NUMERIC_HOST,
                NUMERIC_HOST.replace(
                    "    let name",
                    "    return name.parse::<std::net::IpAddr>().ok().map(NetAddr::from_ip);\n    let name",
                ),
            )
        ],
        "cargo test -p dash-p2p --features inbound,test-utils --lib -- numeric; a=$?; "
        "cargo test -p dashd-net --test dashd_inbound_policy -- setban; b=$?; "
        'echo "lib=$a setban=$b"; [ $a = 0 ] && [ $b = 0 ]',
    ),
    (
        "M5a",
        "qfcommit inventory by the raw payload hash",
        [
            (
                STUBS,
                QFCOMMIT_READ,
                QFCOMMIT_READ.replace(
                    "let qc: Commitment", "let raw = data;\n                let _qc: Commitment"
                ).replace("qc.hash()", HASH_FROM + "(dash_sha256::sha256d(raw))"),
            ),
            ("dashd-p2p/Cargo.toml", "[dependencies]\n", '[dependencies]\ndash-sha256 = { path = "../dash-sha256" }\n'),
        ],
        "cargo test -p dashd-p2p --test sim_m2 -- qfcommit_answers",
    ),
    (
        "M5b",
        "qfcommit inventory by a constant hash",
        [(STUBS, "hash: qc.hash(),", "hash: " + HASH_FROM + "([7; 32]),")],
        "cargo test -p dashd-p2p --test sim_m2 -- qfcommit_answers",
    ),
    (
        "M7a",
        "a ban held at its expiry second (is_banned now <= ban_until)",
        [("dash-p2p/src/ban.rs", "any(|(net, e)| now < e.ban_until", "any(|(net, e)| now <= e.ban_until")],
        "cargo test -p dashd-net --test dashd_inbound_policy -- --exact inbound_ban_expiry",
    ),
    (
        "M7b",
        "an entry swept at its expiry second (keep now < ban_until)",
        [("dash-p2p/src/ban.rs", "net.is_valid() && now <= e.ban_until", "net.is_valid() && now < e.ban_until")],
        "cargo test -p dashd-net --test dashd_inbound_policy -- --exact inbound_ban_expiry",
    ),
    (
        "M6",
        "IPv6 scope id zeroed in Service::to_socket_addr",
        [
            (
                "dash-p2p/src/netaddr.rs",
                "SocketAddrV6::new(ip, self.port, 0, self.addr.scope_id)",
                "SocketAddrV6::new(ip, self.port, 0, 0)",
            )
        ],
        "cargo test -p dash-p2p --features inbound,test-utils --lib -- scope_id_is_cores numeric_hosts",
    ),
]


def substitute(path, old, new):
    p = WT / path
    text = p.read_text()
    assert text.count(old) == 1, (path, old)
    p.write_text(text.replace(old, new))


def main():
    args = sys.argv[1:]
    out = pathlib.Path(os.environ.get("TMPDIR", "/tmp"))
    if args[:1] == ["--out"]:
        out = pathlib.Path(args[1])
        args = args[2:]
    unknown = set(args) - {m[0] for m in MUTANTS}
    assert not unknown, f"unknown mutants: {sorted(unknown)}"
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "12"))
    survived = 0
    for name, what, edits, cmd in MUTANTS:
        if args and name not in args:
            continue
        files = sorted({path for path, _, _ in edits} | {"Cargo.lock"})
        saved = {f: (WT / f).read_bytes() for f in files}
        try:
            for edit in edits:
                substitute(*edit)
            r = subprocess.run(["sh", "-c", cmd], cwd=WT, env=env, capture_output=True, text=True)
            log = r.stdout + r.stderr
            (out / f"mut-{name}.log").write_text(log)
            failures = [line for line in log.splitlines() if "panicked at" in line or " ... FAILED" in line]
            verdict = "KILLED" if r.returncode and failures else "SURVIVED" if r.returncode == 0 else "NO TEST RAN"
            survived += verdict != "KILLED"
            print(f"{name} ({what}): {verdict}", *failures[:4], sep="\n  ", flush=True)
        finally:
            for f, data in saved.items():
                (WT / f).write_bytes(data)
                os.utime(WT / f)
    sys.exit(1 if survived else 0)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Record Dash Core's script-layer results for CS1's tests (node program, CS1 design §7.2).

Builds record.cpp against the pinned Core release's already-built archives (the oracle-shim
worktree, O1), runs it and writes dash-consensus/tests/data/script_recorded.json with a
provenance header. DEC-82 (v24 only): v24.0.0-rc.3 is the one required tree. Set CS1_CORE_V23 to a
v23.1.8 tree to also run it and require byte-identical output, as the file was first recorded;
that is a check of history, not a requirement of a re-record.

Usage: contrib/node/cs1-script-recorder/record.py [--check]
  --check   rebuild, rerun and compare the records with the committed file instead of writing
            it. A provenance that names other shim commits is reported, not failed.

Environment:
  CS1_CORE_V24   Core v24.0.0-rc.3 build tree (default ~/workspace/dash-wt/oracle-shim-v24.0)
  CS1_CORE_V23   optional: a Core v23.1.8 build tree; when set, its output must equal v24's
  CS1_SCRATCH    build directory (default /work/scratch/cs1/recorder)

Each tree must be a configured and built Dash Core checkout of that release (the oracle-shim
branches add only src/test/oracle/, which the recorder does not use).
"""
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
OUT = REPO / "dash-consensus" / "tests" / "data" / "script_recorded.json"
PINS = {}
if os.environ.get("CS1_CORE_V23"):
    PINS["v23.1.8"] = Path(os.environ["CS1_CORE_V23"]).expanduser()
PINS["v24.0.0-rc.3"] = Path(
    os.environ.get("CS1_CORE_V24", "~/workspace/dash-wt/oracle-shim-v24.0")
).expanduser()
SCRATCH = Path(os.environ.get("CS1_SCRATCH", "/work/scratch/cs1/recorder"))


def describe(tree: Path) -> str:
    return subprocess.run(["git", "-C", str(tree), "describe", "--tags", "--always", "--dirty"], check=True,
                          capture_output=True, text=True).stdout.strip()


def build_and_run(pin: str, tree: Path) -> bytes:
    src = tree / "src"
    binary = SCRATCH / f"record-{pin}"
    SCRATCH.mkdir(parents=True, exist_ok=True)
    cmd = ["g++", "-std=c++20", "-O2", "-DHAVE_CONFIG_H", f"-I{src}", f"-I{src / 'secp256k1/include'}",
           str(HERE / "record.cpp"), "-Wl,--start-group", str(src / "libbitcoin_consensus.a"),
           str(src / "libbitcoin_common.a"), str(src / "libbitcoin_util.a"),
           *sorted(str(p) for p in (src / "crypto/.libs").glob("*.a")),
           str(src / "secp256k1/.libs/libsecp256k1.a"), str(src / "secp256k1/.libs/libsecp256k1_precomputed.a"),
           "-Wl,--end-group", "-lpthread", "-o", str(binary)]
    subprocess.run(cmd, check=True)
    return subprocess.run([str(binary)], check=True, capture_output=True).stdout


def render(records: dict, versions: dict) -> str:
    lines = ["{",
             '"provenance": ' + json.dumps(
                 "Computed by Dash Core's own script code (EvalScript and VerifyScript with BaseSignatureChecker "
                 "and with record.cpp's MockChecker, FindAndDelete, GetSigOpCount, GetScriptOp, GetOpName, CScriptNum, "
                 "scriptnum_tests' CScriptNum10, CheckSignatureEncoding, CPubKey::Verify/CheckLowS, "
                 "ecdsa_signature_parse_der_lax, ScriptErrorString), " +
                 ("identical on " if len(PINS) > 1 else "on ") +
                 " and ".join(f"{pin} ({versions[pin]})" for pin in PINS) +
                 "; regenerate with contrib/node/cs1-script-recorder/record.py") + ","]
    sections = list(records.items())
    for i, (name, rows) in enumerate(sections):
        lines.append(json.dumps(name) + ": [")
        for j, row in enumerate(rows):
            lines.append(json.dumps(row, separators=(",", ":")) + ("," if j + 1 < len(rows) else ""))
        lines.append("]" + ("," if i + 1 < len(sections) else ""))
    lines.append("}")
    return "\n".join(lines) + "\n"


def main() -> int:
    outputs = {}
    versions = {}
    for pin, tree in PINS.items():
        versions[pin] = describe(tree)
        outputs[pin] = build_and_run(pin, tree)
    first = outputs["v24.0.0-rc.3"]
    if any(data != first for data in outputs.values()):
        print("the pins disagree: an O17 PIN-DELTA finding; see the outputs in", SCRATCH, file=sys.stderr)
        for pin, data in outputs.items():
            (SCRATCH / f"output-{pin}.json").write_bytes(data)
        return 1
    records = json.loads(first)
    text = render(records, versions)
    if "--check" in sys.argv[1:]:
        # The records are what is checked. The provenance names the shim commits, which move when
        # O1 adds oracle entry points without touching Core's script code, so a stale provenance
        # with matching records is reported, not failed.
        committed = json.loads(OUT.read_text())
        provenance = committed.pop("provenance", None)
        if committed != records:
            changed = sorted(k for k in set(committed) | set(records) if committed.get(k) != records.get(k))
            print(f"{OUT}: records differ from what Core computes now in: {', '.join(changed)}", file=sys.stderr)
            return 1
        if provenance != json.loads(text)["provenance"]:
            print(f"{OUT}: records match {', '.join(PINS)}; the provenance names other shim commits "
                  f"({', '.join(versions.values())} now); rerun without --check to refresh it")
            return 0
        print(f"{OUT}: matches {', '.join(PINS)}")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text)
    print(f"wrote {OUT}: " + ", ".join(f"{k} {len(v)}" for k, v in records.items()))
    return 0


if __name__ == "__main__":
    sys.exit(main())

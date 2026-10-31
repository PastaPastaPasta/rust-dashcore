#!/usr/bin/env python3
"""Write dash-dml/tests/data/codec/budget.txt: Core's canonical list reader on the nested
CompactSize budget cases (EV2a fix r1, R1-M1).

The cases are dash-dml's (`budget_cases` in src/codec/tests.rs): one-entry lists whose
version-3 entry exhausts the per-entry budget early in one nested field. Every verdict is the
probe's, which links Core's v24 codecs (see build.sh and its MANIFEST).

usage:
  cargo test -p dash-dml --all-features --lib print_budget_cases -- --ignored --nocapture \\
      | grep -E '^[a-z-]+ [0-9a-f]+$' > cases.txt
  budget.py --probe <ev2a_codec_probe> --cases cases.txt --out dash-dml/tests/data/codec/budget.txt
"""

import argparse
import subprocess
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--probe", required=True)
    ap.add_argument("--cases", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    manifest = Path(args.probe).with_name("MANIFEST").read_text()
    probe = subprocess.Popen([args.probe, "main"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    with open(args.out, "w") as out:
        out.write("".join(f"# probe {line}\n" for line in manifest.splitlines()))
        out.write("# canon <case> <input> <Core's answer: ok CanonicalMNListHash | err what>\n")
        for line in Path(args.cases).read_text().splitlines():
            name, data = line.split(" ")
            probe.stdin.write(f"canon {data}\n")
            probe.stdin.flush()
            fields = probe.stdout.readline().rstrip("\n").split(" ")
            # `ok <re-encoding> <hash>`: keep the hash, as regen.py does for lists.
            answer = f"ok {fields[2]}" if fields[0] == "ok" else " ".join(fields)
            out.write(f"canon {name} {data} {answer}\n")
    probe.stdin.close()
    probe.wait()


if __name__ == "__main__":
    main()

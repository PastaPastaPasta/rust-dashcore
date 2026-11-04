#!/usr/bin/env python3
"""Regenerate dash-dml's Core-judged codec vectors (dash-dml/tests/data/codec/) with the probe.

Every byte and verdict in the output comes from Dash Core's own code (the probe links the v24.0
oracle pin's libraries; see build.sh and its MANIFEST):
- lists.txt: Core-built lists (the probe's `synth`) and Core-cut sublists of mainnet lists
  (`take`), each with its canonical bytes, CanonicalMNListHash, evodb bytes in Core's iteration
  order, and the evodb bytes after Core reloads them (`s3`, which reverses collision buckets).
- diffs.txt: BuildDiff between those lists, canonical and evodb (dmn_D4) bytes.
- apply.txt: ApplyDiff of each diff to its source list.
- verdicts.txt: Core's canonical readers on mutated canonical lists and diffs (seeded).

The mainnet lists are `prev_list` fields of dml-listdiff-check.py samples: list content from
`protx listdiff` on the mainnet v24 oracle (a pure read), encoded as evodb stores it; internal
ids and the counter are the script's. Core decodes them here and cuts the sublists itself.

usage: regen.py --probe <ev2a_codec_probe> --samples <samples.jsonl.gz> --heights H1,H2 --out <dir>
"""

import argparse
import gzip
import hashlib
import json
import random
import subprocess
from pathlib import Path

TAKE = 48
SEED = 1


def sha256d_hex(hex_bytes):
    return hashlib.sha256(hashlib.sha256(bytes.fromhex(hex_bytes)).digest()).digest().hex()


def short(kind, answer):
    """Core's answer with its re-encoding replaced by a hash: CanonicalMNListHash for a list
    (SHA256d of the canonical bytes), SHA256d of the canonical bytes for a diff."""
    fields = answer.split(" ")
    if fields[0] != "ok":
        return answer
    return f"ok {fields[2] if kind == 'list' else sha256d_hex(fields[1])}"


class Probe:
    def __init__(self, path):
        self.proc = subprocess.Popen([path, "main"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def ask(self, line):
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()
        return self.proc.stdout.readline().rstrip("\n")

    def synth(self):
        self.proc.stdin.write("synth\n")
        self.proc.stdin.flush()
        out = []
        while (line := self.proc.stdout.readline().rstrip("\n")) != "end":
            out.append(line)
        return out


def mutations(rng, data, count):
    """Truncations, byte flips and fixed-offset edits of `data` (bytes)."""
    out = set()
    for _ in range(count):
        out.add(data[: rng.randrange(len(data))])
    for _ in range(count * 3):
        b = bytearray(data)
        i = rng.randrange(len(b))
        b[i] ^= rng.randrange(1, 256)
        out.add(bytes(b))
    for _ in range(count):
        b = bytearray(data)
        i = rng.randrange(len(b))
        b[i:i] = bytes([rng.randrange(256)])
        out.add(bytes(b))
    out.discard(data)
    out.discard(b"")
    return sorted(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--probe", required=True)
    ap.add_argument("--samples", required=True)
    ap.add_argument("--heights", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    probe = Probe(args.probe)
    manifest = Path(args.probe).with_name("MANIFEST").read_text()
    header = "".join(f"# probe {line}\n" for line in manifest.splitlines())

    lists = {}  # name -> (canonical, hash, s3)
    diffs = {}  # name -> (canonical, d4)
    for line in probe.synth():
        kind, name, *rest = line.split(" ")
        (lists if kind == "list" else diffs)[name] = tuple(rest)

    heights = [int(h) for h in args.heights.split(",")]
    with gzip.open(args.samples, "rt") as f:
        for sample_line in f:
            sample = json.loads(sample_line)
            if sample["height"] in heights:
                answer = probe.ask(f"take {TAKE} {sample['prev_list']}").split(" ")
                assert answer[0] == "ok", answer
                lists[f"mainnet{sample['height'] - 1}"] = tuple(answer[1:])
    assert all(f"mainnet{h - 1}" in lists for h in heights), "a height is not in the samples"
    names = sorted(lists)
    for a in names:
        for b in names:
            if a != b and f"{a}>{b}" not in diffs and a.startswith("mainnet") and b.startswith("mainnet"):
                answer = probe.ask(f"build {lists[a][0]} {lists[b][0]}").split(" ")
                assert answer[0] == "ok", answer
                diffs[f"{a}>{b}"] = tuple(answer[1:])

    with open(out / "lists.txt", "w") as f:
        f.write(header + "# list <name> <canonical> <CanonicalMNListHash> <evodb> <evodb after Core's reload>\n")
        for name in names:
            canonical, hash_, s3 = lists[name]
            reload = probe.ask(f"s3 {s3}").split(" ")
            assert reload[0] == "ok" and reload[1] == canonical and reload[2] == hash_, name
            f.write(f"list {name} {canonical} {hash_} {s3} {reload[3]}\n")
    with open(out / "diffs.txt", "w") as f:
        f.write(header + "# diff <from>><to> <canonical diff> <evodb diff>\n")
        for name in sorted(diffs):
            f.write(f"diff {name} {' '.join(diffs[name])}\n")
    with open(out / "apply.txt", "w") as f:
        f.write(header + "# apply <diff name> <block hash> <height> <Core's answer: ok CanonicalMNListHash | err what>\n")
        for name in sorted(diffs):
            a, b = name.split(">")
            canonical_b = bytes.fromhex(lists[b][0])
            block, height = canonical_b[:32].hex(), int.from_bytes(canonical_b[32:36], "little", signed=True)
            answer = probe.ask(f"apply {lists[a][0]} {diffs[name][0]} {block} {height}")
            f.write(f"apply {name} {block} {height} {short('list', answer)}\n")
    rng = random.Random(SEED)
    with open(out / "verdicts.txt", "w") as f:
        f.write(header + f"# seed {SEED}\n# <canon|cdiff> <input> <Core's answer: ok <SHA256d of the re-encoding> | err what>\n")
        for name in names:
            if name.startswith("mainnet"):
                continue
            for data in mutations(rng, bytes.fromhex(lists[name][0]), 8):
                f.write(f"canon {data.hex()} {short('list', probe.ask('canon ' + data.hex()))}\n")
        for name in sorted(diffs):
            if name.startswith("mainnet"):
                continue
            for data in mutations(rng, bytes.fromhex(diffs[name][0]), 2):
                f.write(f"cdiff {data.hex()} {short('diff', probe.ask('cdiff ' + data.hex()))}\n")


if __name__ == "__main__":
    main()

#!/usr/bin/env bash
# Copies the slim parts of every extracted FT recording into test-vectors (O8 spec §13.1):
# digest, decisions, golden and blocks.dat as zstd -19, plus chainargs, provenance, extract.json, the DEC-177
# verdicts and, per test, manifest.json. Before anything is copied, every check passes, or the
# script stops and leaves <dest> as it was:
#  * the tests are exactly those of ft-list.txt (next to this script; STATEDIGEST_FT_LIST names
#    another list, for the script's own tests), each present under <ft-out>;
#  * each test's manifest.json (written by record-ft.py) lists the nodes the FT created, and the
#    rec/node* directories present are exactly its "recorded" nodes (nodes minus "unstarted"), so
#    a node directory missing from <ft-out> is noticed;
#  * each recorded node was extracted, reproducibly, with every DEC-177 verdict "match", by a
#    clean rig at record and extract time, and the digest, decisions, golden and blocks.dat files
#    are the bytes extract.json hashed (a digest cut down after extraction no longer matches);
#  * the extraction's summary is for the golden's tip, and every BIP9 height it left unchecked
#    (runs with other -vbparams) was checked by check-bip9 on a copy with those runs' params, at
#    the same tip; the other-params runs are derived again from chainargs.json and the digest;
#  * statedigest-check passes on the three files (strict parse, H first, record kinds, the A join,
#    the golden as the digest's last tip connects plus the golden-only keys), and the digest and
#    decisions counts it prints equal provenance.json's "records". It subsumes the strict parse
#    that statedigest-stats did, so the script no longer runs that example.
# <dest> is then replaced in one step: the new tree is built next to it and exchanged (mv
# -T --exchange) with <dest> if that exists, and the old tree deleted afterwards.
# Usage: store-vectors.sh <ft-out> <statedigest-check> <dest, e.g. test-vectors/statedigest/regtest-v24>
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ft=$1 check=$2 dest=$3
list=${STATEDIGEST_FT_LIST:-$here/ft-list.txt}
new=$(mktemp -d "$(dirname "$dest")/.store.XXXXXX")
trap 'rm -rf "$new"' EXIT

# Prints "<test> <node>" for every recorded node, or exits with the first problem found.
recordings=$(python3 -I - "$ft" "$check" "$list" "$here" <<'EOF'
import hashlib, json, re, subprocess, sys
from pathlib import Path

sys.path.insert(0, sys.argv[4])
from extract import other_params_runs  # noqa: E402

ft, check, listing = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])


def fail(where, what):
    sys.exit(f"{where}: {what}")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        fail(path, "missing")


def derived_runs(rec):
    """extract.py's other-params runs of rec, or its reason for refusing them."""
    try:
        return other_params_runs(rec)
    except SystemExit as refusal:
        return f"none: {refusal}"


def names(path):
    return {d.name for d in path.iterdir() if d.is_dir() and re.fullmatch(r"node\d+", d.name)}


tests = listing.read_text().split()
if not tests or tests != sorted(set(tests)):
    fail(listing, "must list at least one test, sorted, without repeats")
for test in tests:
    top = ft / test.removesuffix(".py")
    if not top.is_dir():
        fail(top, "the recording of a listed test is missing")
    m = load(top / "manifest.json")
    present = names(top / "rec") if (top / "rec").is_dir() else set()
    if m["test"] != test:
        fail(top, f"manifest.json is for {m['test']}")
    if set(m["recorded"]) != present:
        fail(top, f"manifest.json records {sorted(m['recorded'])} but rec/ has {sorted(present)}")
    if set(m["recorded"]) != set(m["nodes"]) - set(m["unstarted"]) or not m["recorded"]:
        fail(top, "manifest.json: recorded is not nodes minus unstarted, or is empty")
    for node in sorted(present, key=lambda n: int(n[4:])):
        rec = top / "rec" / node
        try:
            e, p = load(rec / "extract.json"), load(rec / "provenance.json")
            verdicts = [json.loads(l)["result"] for l in (rec / "dec177.jsonl").read_text().splitlines()]
            problems = [what for what, bad in [
                ("not reproducible", e["reproducible"] is not True),
                ("a DEC-177 verdict other than match", any(v != "match" for v in verdicts)),
                ("fewer DEC-177 verdicts than samples", len(verdicts) != e["dec177"]["samples"]),
                ("a dirty rig at extraction", e["rig"]["rig_dirty"]),
                ("a dirty rig at recording", p["rig"]["rig_dirty"]),
                ("a provenance for another test", p["test"] != test or p["node"] != node),
                ("unstarted nodes that differ from the manifest's", p["unstarted_nodes"] != m["unstarted"]),
                ("a summary for another tip than the golden's",
                 e["summary"]["tip"] != (e["golden"]["heights"] or [None, None])[1]),
                ("other-params runs that chainargs.json and the digest do not give",
                 e["other_params_runs"] != derived_runs(rec)),
                ("BIP9 heights left unchecked", e["bip9"]["unchecked"] != e["summary"]["bip9_unchecked"]
                 or not set(e["bip9"]["unchecked"]) <= {h for g in e["bip9"]["groups"] for h in g["checked"]}
                 or sorted(r for g in e["bip9"]["groups"] for r in g["runs"]) != sorted(e["other_params_runs"])
                 or any(g["tip"] != e["summary"]["run"] for g in e["bip9"]["groups"])),
            ] + [
                (f"a {name} file other than the one extracted", sha256(rec / f"{name}.jsonl") != e[name]["sha256"])
                for name in ("digest", "decisions", "golden")
            ] + [
                ("a blocks.dat other than the one extracted", sha256(rec / "blocks.dat") != e["blocks_dat_sha256"]),
            ] if bad]
            if problems:
                fail(rec, ", ".join(problems))
            r = subprocess.run([check, *(str(rec / f"{n}.jsonl") for n in ("digest", "decisions", "golden"))],
                               capture_output=True, text=True)
            if r.returncode:
                fail(rec, f"statedigest-check: {r.stderr.strip()}")
            counts = json.loads(r.stdout)
            for name in ("digest", "decisions"):
                if counts[name] != p["records"][name]:
                    fail(rec, f"{name} holds {counts[name]} but provenance.json says {p['records'][name]}")
        except KeyError as key:
            fail(rec, f"extract.json or provenance.json lacks {key}")
        except OSError as err:
            fail(rec, str(err))
        print(test, node)
EOF
)
[ -n "$recordings" ] || { echo "no recordings under $ft" >&2; exit 1; }

stored=0
while read -r test node; do
    top=$ft/${test%.py}
    rec=$top/rec/$node out=$new/${test%.py}/$node
    mkdir -p "$out"
    for f in digest.jsonl decisions.jsonl golden.jsonl blocks.dat; do
        zstd -19 -q "$rec/$f" -o "$out/$f.zst"
    done
    cp "$rec/chainargs.json" "$rec/provenance.json" "$rec/extract.json" "$rec/dec177.jsonl" "$out/"
    cp "$top/manifest.json" "$new/${test%.py}/"
    stored=$((stored + 1))
done <<<"$recordings"
chmod 755 "$new"
if [ -e "$dest" ]; then
    mv -T --exchange "$new" "$dest"
else
    mv -T "$new" "$dest"
fi
rm -rf "$new" # the old tree after an exchange, nothing otherwise
trap - EXIT
echo "stored $stored nodes in $dest"

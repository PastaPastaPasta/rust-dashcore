#!/usr/bin/env bash
# The dash-dml leg of DEC-177 (O8 spec §13.4) over the stored recordings: every golden's
# canonical_mnlist samples go through dash-dml's canonical_list_check example, which must decode
# each one, re-encode it unchanged and hash it to the RPC's hash. Each node must yield as many
# samples as its extract.json counted, and the set at least one. Prints a JSON summary; exits 1 on
# the first other verdict.
# Usage: dml-leg.sh <vectors, e.g. test-vectors/statedigest/regtest-v24> <canonical_list_check>
set -euo pipefail
shopt -s nullglob
vectors=$1 check=$2
nodes=0 samples=0
for golden in "$vectors"/*/node*/golden.jsonl.zst; do
    node=$(dirname "$golden")
    n=$(zstdcat "$golden" | python3 -c '
import json, sys
for line in sys.stdin:
    r = json.loads(line)
    if r["k"] == "S" and r["rpc"] == "canonical_mnlist":
        print(r["h"], r["res"]["hash"], r["res"]["hex"])
' | "$check" | python3 -c '
import json, sys
node, expected = sys.argv[1], json.load(open(sys.argv[1] + "/extract.json"))["dec177"]["samples"]
verdicts = [json.loads(l) for l in sys.stdin]
bad = [v for v in verdicts if v["result"] != "match"]
if bad or len(verdicts) != expected:
    sys.exit(f"{node}: {len(verdicts)} of {expected} samples, not match: {bad[:3]}")
print(len(verdicts))
' "$node")
    nodes=$((nodes + 1)) samples=$((samples + n))
done
[ "$samples" -gt 0 ] || { echo "no canonical_mnlist samples under $vectors" >&2; exit 1; }
echo "{\"nodes\":$nodes,\"samples\":$samples,\"result\":\"match\"}"

#!/usr/bin/env bash
# T6 OK-3 D1 (README.md): builds both sides, writes the input rows, runs both and compares.
#   contrib/node/t6-probe/run.sh <out-dir>
# Needs base-sdk at ~/workspace/base-sdk (read with git only) and, for the regtest chain,
# `python3 contrib/setup-dashd.py` run once (its cache under ~/.rust-dashcore-test).
set -euo pipefail

out=${1:?usage: run.sh <out-dir>}
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
probe=$root/contrib/node/t6-probe
bsdk=$HOME/workspace/base-sdk
rc=de0d0c7ef200f8d6ba1df2f3464d648620298898
target=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$out/corpora" "$out/chains"

# base-sdk's corpora at the rc, and payload_core.json5 as JSON for the item 5 check.
for f in proregtx v24 payload_core sighash decode_domain; do
  git -C "$bsdk" show "$rc:pkgs/primitives/corpus/$f.json5" > "$out/corpora/$f.json5"
done
python3 -I - "$out/corpora/payload_core.json5" "$out/payload_core.json" <<'EOF'
import json, re, sys
text = "\n".join(l for l in open(sys.argv[1]).read().splitlines() if not l.lstrip().startswith("//"))
json.dump(json.loads(re.sub(r",(\s*[}\]])", r"\1", text)), open(sys.argv[2], "w"))
EOF

# The regtest chains: setup-dashd.py's and the statedigest sets', in -loadblock framing.
inputs=()
while IFS= read -r f; do
  inputs+=("blk:$f")
done < <(find "$HOME/.rust-dashcore-test" -path '*regtest-blockchain*' -name 'blk*.dat' | sort)
if [ "${#inputs[@]}" -eq 0 ]; then
  echo "no regtest chain under ~/.rust-dashcore-test: run python3 contrib/setup-dashd.py" >&2
  exit 1
fi
n=0
while IFS= read -r f; do
  n=$((n + 1))
  zstd -qdc "$f" > "$out/chains/statedigest-$n.dat"
  inputs+=("blk:$out/chains/statedigest-$n.dat")
done < <(find "$root/test-vectors/statedigest" -name 'blocks.dat.zst' | sort)
if [ "$n" -eq 0 ]; then
  echo "no blocks.dat.zst under test-vectors/statedigest" >&2
  exit 1
fi

# The rdc fixtures, scanned for transactions and blocks in hex.
for f in "$out"/corpora/*.json5 \
  "$root"/dash-oracle-sys/tests/data/*.json \
  "$root"/dash-dml/tests/data/*.txt \
  "$root"/node-l1/tests/data/*.json \
  "$root"/node-l1/src/dml_apply/shared-case.json \
  "$root"/conformance/tests/data/*.json \
  "$root"/test-vectors/statedigest/testnet-v24/*.jsonl; do
  inputs+=("scan:$f")
done

# dash-dml's T6 corpus and node-l1's BS4b rows, dumped by their ignored tests.
(cd "$root" && T6_ROWS="$out/dml-rows.tsv" cargo test -p dash-dml --lib t6_probe_rows -- --ignored)
(cd "$root" && T6_ROWS="$out/bsdk-rows.tsv" cargo test -p node-l1 --test bsdk_recorded t6_probe_rows -- --ignored)
inputs+=("rows:$out/dml-rows.tsv" "rows:$out/bsdk-rows.tsv" "crafted:$out/payload_core.json")

for side in old new; do
  (cd "$probe/$side" && CARGO_TARGET_DIR="$target/t6-probe-$side" cargo build --release --locked --offline)
done
new=$target/t6-probe-new/release/t6-probe
old=$target/t6-probe-old/release/t6-probe-old

"$new" gen "$out/rows.tsv" "$out/values.json" "${inputs[@]}"
"$old" "$out/rows.tsv" > "$out/old.jsonl"
"$new" eval "$out/rows.tsv" > "$out/new.jsonl"
status=0
"$new" compare "$out/old.jsonl" "$out/new.jsonl" "$out/payload_core.json" "$out/rows.tsv" \
  > "$out/d1.jsonl" || status=$?
tail -n 1 "$out/d1.jsonl"
exit "$status"

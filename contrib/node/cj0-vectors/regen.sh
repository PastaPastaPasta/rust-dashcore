#!/usr/bin/env bash
# Regenerates dash-coinjoin's Core vectors (CJ0):
#   dash-coinjoin/tests/data/core_common.json
#     gen_common.cpp compiled against src/ of the pinned Dash Core commit
#     (dash-oracle-sys/pins.txt) and run: Core's own constexpr functions of
#     src/coinjoin/common.h over a fixed grid.
#   dash-coinjoin/tests/data/core_fully_mixed.json
#     gen_fully_mixed.cpp: CWallet::IsFullyMixed's hashing lines
#     (wallet/coinjoin.cpp, copied and checked against the pinned file below)
#     over Core's CDataStream, COutPoint/uint256 serialisation, CSHA256
#     (crypto/sha256.cpp, sha256_sse4.cpp, support/cleanse.cpp compiled in)
#     and ReadLE64, run over a fixed grid of outpoints and salts.
#   dash-coinjoin/tests/data/core_amount_strings.json
#     The pinned dashd's `decoderawtransaction` of a transaction paying each
#     standard denomination and the collateral bounds: the "value" texts are
#     Core's ValueFromAmount, which DenominationToString prints.
#
# Usage: contrib/node/cj0-vectors/regen.sh <dash-core-clone> <scratch-dir>
# Needs g++ (C++20) and the pinned oracle dashd under
# ${DASH_ORACLE_CACHE:-/work/cache/oracle}/<commit>/plain/dashd.
set -euo pipefail

core="$1"
scratch="$2"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
commit="$(awk '$1 == "v24.0" { print $2 }' "$repo/dash-oracle-sys/pins.txt")"
dashd="${DASH_ORACLE_CACHE:-/work/cache/oracle}/$commit/plain/dashd"
out="$repo/dash-coinjoin/tests/data"
[ -x "$dashd" ] || { echo "no pinned dashd at $dashd" >&2; exit 2; }

mkdir -p "$scratch" "$out"
chmod 700 "$scratch"
src="$scratch/core-src"
rm -rf "$src"
mkdir -p "$src"
git -C "$core" archive "$commit" src | tar -x -C "$src"

g++ -std=c++20 -O2 -Wall -I"$src/src" "$here/gen_common.cpp" -o "$scratch/gen_common"
"$scratch/gen_common" > "$scratch/common.json"
python3 -I - "$scratch/common.json" "$out/core_common.json" "$commit" <<'PY'
import json, sys
def dump(data, path):
    # One row per line: reviewable diffs, a modest file.
    lines = ["{"]
    items = list(data.items())
    for i, (k, v) in enumerate(items):
        end = "," if i + 1 < len(items) else ""
        if isinstance(v, list) and v and isinstance(v[0], list):
            lines.append(f" {json.dumps(k)}: [")
            lines += [" " + json.dumps(r, separators=(",", ":")) + ("," if j + 1 < len(v) else "") for j, r in enumerate(v)]
            lines.append(" ]" + end)
        else:
            lines.append(f" {json.dumps(k)}: {json.dumps(v, separators=(',', ':'))}{end}")
    open(path, "w").write("\n".join(lines) + "\n}\n")
data = json.load(open(sys.argv[1]))
data = {"_provenance": f"contrib/node/cj0-vectors/gen_common.cpp compiled against Dash Core {sys.argv[3]} src/ (coinjoin/common.h) and run; regen.sh", **data}
dump(data, sys.argv[2])
PY

# gen_fully_mixed.cpp copies IsFullyMixed's hashing lines: refuse if the pinned
# Core no longer has them, rather than write stale vectors.
for line in 'CDataStream ss(SER_GETHASH, PROTOCOL_VERSION);' 'ss << outpoint << nCoinJoinSalt;' \
    'CSHA256().Write(reinterpret_cast<const uint8_t*>(ss.data()), ss.size()).Finalize(nHash.begin());' \
    'if (ReadLE64(nHash.begin()) % 2 == 0) {'; do
    grep -qF -- "$line" "$src/src/wallet/coinjoin.cpp" ||
        { echo "wallet/coinjoin.cpp at $commit lacks: $line" >&2; exit 3; }
done
g++ -std=c++20 -O2 -Wall -I"$src/src" "$here/gen_fully_mixed.cpp" "$src/src/crypto/sha256.cpp" \
    "$src/src/crypto/sha256_sse4.cpp" "$src/src/support/cleanse.cpp" -o "$scratch/gen_fully_mixed"
"$scratch/gen_fully_mixed" > "$scratch/fully_mixed.json"
python3 -I - "$scratch/fully_mixed.json" "$out/core_fully_mixed.json" "$commit" <<'PY'
import json, sys
rows = json.load(open(sys.argv[1]))["rows"]
head = (f'{{\n "_provenance": "contrib/node/cj0-vectors/gen_fully_mixed.cpp compiled against Dash Core {sys.argv[3]} '
        'src/ (the hashing lines of IsFullyMixed, wallet/coinjoin.cpp:416-420, over CDataStream/COutPoint/CSHA256/ReadLE64) and run; regen.sh",\n'
        ' "_columns": ["txid bytes as stored", "index", "salt bytes as stored", "ReadLE64(SHA256(outpoint || salt))", "stops mixing"],\n'
        ' "rows": [\n')
body = ",\n".join(" " + json.dumps(r, separators=(",", ":")) for r in rows)
open(sys.argv[2], "w").write(head + body + "\n ]\n}\n")
PY

# dashd's ValueFromAmount texts.
datadir="$scratch/dashd"
rm -rf "$datadir"
mkdir -p "$datadir"
port=$((20000 + RANDOM % 20000))
"$dashd" -regtest -datadir="$datadir" -daemon=0 -listen=0 -server=1 -rpcuser=cj0 -rpcpassword=cj0 \
    -rpcport="$port" -disablewallet -printtoconsole=0 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$datadir" "$src"' EXIT
python3 -I - "$port" "$out/core_amount_strings.json" "$commit" "$src/src/consensus/amount.h" <<'PY'
import http.client, base64, json, re, sys, time
def dump(data, path):
    # One row per line: reviewable diffs, a modest file.
    lines = ["{"]
    items = list(data.items())
    for i, (k, v) in enumerate(items):
        end = "," if i + 1 < len(items) else ""
        if isinstance(v, list) and v and isinstance(v[0], list):
            lines.append(f" {json.dumps(k)}: [")
            lines += [" " + json.dumps(r, separators=(",", ":")) + ("," if j + 1 < len(v) else "") for j, r in enumerate(v)]
            lines.append(" ]" + end)
        else:
            lines.append(f" {json.dumps(k)}: {json.dumps(v, separators=(',', ':'))}{end}")
    open(path, "w").write("\n".join(lines) + "\n}\n")

port, dest, commit = int(sys.argv[1]), sys.argv[2], sys.argv[3]
# Amounts: the standard denominations and the collateral bounds (Core's
# common.h values, read from the generated grid).
common = json.load(open(dest.replace("core_amount_strings.json", "core_common.json")))
amounts = common["standard_denominations"] + [common["collateral_amount"], common["max_collateral_amount"]]
def le(n, w):
    return n.to_bytes(w, "little", signed=n < 0).hex()
tx = le(1, 4) + "01" + "11" * 32 + le(0, 4) + "00" + "ffffffff"
tx += "%02x" % len(amounts) + "".join(le(a, 8) + "00" for a in amounts) + le(0, 4)
auth = base64.b64encode(b"cj0:cj0").decode()
body = json.dumps({"method": "decoderawtransaction", "params": [tx], "id": 1})
for _ in range(120):
    try:
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
        c.request("POST", "/", body, {"Authorization": "Basic " + auth})
        r = c.getresponse().read().decode()
        if '"error":null' in r.replace(" ", ""):
            break
    except OSError:
        pass
    time.sleep(0.5)
else:
    sys.exit("dashd did not answer decoderawtransaction")
texts = re.findall(r'"value":\s*([-0-9.]+)', r)
assert len(texts) == len(amounts), (texts, amounts)
rows = [[a, t] for a, t in zip(amounts, texts)]
data = {"_provenance": f"Dash Core {commit} dashd -regtest decoderawtransaction: each output's \"value\" text (ValueFromAmount); regen.sh", "amounts": rows}
dump(data, dest)
PY
echo "wrote $out/core_common.json $out/core_fully_mixed.json $out/core_amount_strings.json"

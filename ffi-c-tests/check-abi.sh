#!/bin/bash
# Checks that the current dash-spv-ffi keeps the baseline ABI: every struct layout, enum
# constant and integer macro of the baseline headers, every baseline function and callback
# signature and every baseline function symbol, and that the baseline-header C caller links
# against it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
BASE="$SCRIPT_DIR/baseline"
CC="${CC:-cc}"
BASELINE_SYMBOLS=43

cd "$ROOT"
cargo build -p dash-spv-ffi
if command -v jq >/dev/null; then
    TARGET=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
else
    TARGET=$(cargo metadata --format-version 1 --no-deps |
        python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
fi
LIB="$TARGET/debug"
CUR="$TARGET/debug/include"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

python3 "$SCRIPT_DIR/gen-abi-probe.py" "$BASE" | diff - "$SCRIPT_DIR/abi-probe.c" ||
    { echo "abi-probe.c does not match gen-abi-probe.py's output"; exit 1; }

"$CC" -std=c11 -Wall -Werror -I "$BASE" "$SCRIPT_DIR/abi-probe.c" -o "$OUT/abi-base"
"$CC" -std=c11 -Wall -Werror -I "$CUR" "$SCRIPT_DIR/abi-probe.c" -o "$OUT/abi-cur"
"$OUT/abi-base" >"$OUT/base.txt"
"$OUT/abi-cur" >"$OUT/cur.txt"
diff "$OUT/base.txt" "$OUT/cur.txt" || { echo "layout differs from the baseline"; exit 1; }
echo "layout: $(wc -l <"$OUT/base.txt") values match the baseline"

python3 - "$BASE/dash-spv-ffi/dash-spv-ffi.h" >"$OUT/symbols.txt" <<'EOF'
import re, sys
text = re.sub(r"/\*.*?\*/|//[^\n]*", "", open(sys.argv[1]).read(), flags=re.S)
print("\n".join(sorted(set(re.findall(r"\b(\w+)\(", text)))))
EOF
count=$(wc -l <"$OUT/symbols.txt")
[ "$count" -eq "$BASELINE_SYMBOLS" ] ||
    { echo "found $count baseline functions, expected $BASELINE_SYMBOLS"; exit 1; }
nm -D --defined-only "$LIB/libdash_spv_ffi.so" | awk '$2 == "T" { print $3 }' | sort -u >"$OUT/exported.txt"
missing=$(comm -23 "$OUT/symbols.txt" "$OUT/exported.txt")
[ -z "$missing" ] || { echo "baseline symbols not exported:"; echo "$missing"; exit 1; }
echo "symbols: all $count baseline functions exported"

"$CC" -std=c11 -Wall -Werror -I "$BASE" "$SCRIPT_DIR/baseline-caller/caller.c" \
    -L "$LIB" -ldash_spv_ffi -Wl,-rpath,"$LIB" -o "$OUT/caller"
echo "caller: the baseline-header caller links against the current library"

#!/usr/bin/env bash
# No libm in dash-consensus's compiled code (CS2 §2.2, §2.6). Core's consensus floating point
# needs only IEEE + - * / and comparisons, which are bit-exact everywhere; a libm result depends
# on the platform's libm. clippy.toml bans the f32/f64 methods that call libm, and
# tests/fp_confinement.rs keeps `%` (fmod) out of `fp`, both on the source. This checks what the
# compiler emitted, which a macro, a glob-imported trait method or inlined dependency code cannot
# get past: the crate's release rlib may not reference a libm function, nor compiler-builtins'
# powi.
#
# Usage: contrib/node/check-no-libm.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
command -v nm > /dev/null || { echo "check-no-libm.sh: needs nm (binutils)" >&2; exit 2; }
rlib="$(cargo build --locked --release -p dash-consensus --lib --message-format=json-render-diagnostics |
    python3 -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "dash_consensus":
        print(*(f for f in m["filenames"] if f.endswith(".rlib")))
')"
[ -f "$rlib" ] || { echo "check-no-libm.sh: the build named no dash-consensus rlib" >&2; exit 2; }

# C99/POSIX libm (with the float and long double suffixes), and powi's compiler-builtins. Not
# fabs, copysign, fmin or fmax, which are exact and which Rust lowers inline on x86_64.
names='acos|acosh|asin|asinh|atan|atan2|atanh|cbrt|ceil|cos|cosh|erf|erfc|exp|exp10|exp2|expm1'
names+='|fdim|floor|fma|fmod|frexp|hypot|ilogb|ldexp|lgamma|lgamma_r|llrint|llround|log|log10'
names+='|log1p|log2|logb|lrint|lround|modf|nearbyint|nextafter|nexttoward|pow|pow10|remainder'
names+='|remquo|rint|round|roundeven|scalb|scalbln|scalbn|sin|sincos|sinh|sqrt|tan|tanh|tgamma'
names+='|trunc|j0|j1|jn|y0|y1|yn'
# `nm -P` prints `name type ...` per symbol (a leading `_` on Mach-O), and a header per member
undefined="$(nm -u -P "$rlib" 2> /dev/null | awk '$2 == "U" { print $1 }' | sort -u)"
found="$(grep -E "^_?(($names)[fl]?|__powi[sd]f2)$" <<< "$undefined" || true)"
if [ -n "$found" ]; then
    echo "check-no-libm.sh: $rlib calls libm:" $found >&2
    exit 1
fi
echo "dash-consensus: no libm among the $(grep -c . <<< "$undefined") symbols its release rlib imports"

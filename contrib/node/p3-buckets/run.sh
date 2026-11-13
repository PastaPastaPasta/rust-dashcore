#!/usr/bin/env bash
# P3 D7/D22/D90: hold `dashd-p2p`'s libstdc++ bucket emulation (`processing/compact.rs`) to the
# Linux release dashd.
#
#   1. Read `__prime_list` and `__fast_bkt` out of the release dashd (v24.0.0-rc.3, sha256
#      e204e316…5bb4) and check them against `compact.rs`'s `PRIME_LIST`.
#   2. Build probe.cpp statically with GCC 13.3.0 (the release's `.comment`) in the pinned
#      `gcc:13.3.0` container, and with this box's g++ as a diagnostic. Each prints
#      `bucket_count()` of `std::unordered_map<uint64_t, uint16_t>(n)` after construction and
#      after n distinct inserts, for n = 0..=65535.
#   3. Write the GCC 13.3.0 run's bucket-count transitions to the fixture the sweep test reads.
#
# usage: contrib/node/p3-buckets/run.sh <release-dashd> <work-dir>
# The work dir must be empty or new; nothing in it is kept by the repo.
set -euo pipefail

release=$1
work=$2
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
fixture=$repo/dashd-p2p/tests/fixtures/libstdcxx/unordered-map-buckets-gcc13.3.0.txt
image='gcc@sha256:1d71f0f3450214bef38fe09e6f610fb6cca90cf97b43f4ce845bfc32a4168818'  # gcc:13.3.0
want_sha=e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4

[ "$(sha256sum "$release" | cut -d' ' -f1)" = "$want_sha" ] || { echo "not the release dashd" >&2; exit 1; }
mkdir -p "$work"
cp "$here/probe.cpp" "$work/"

# 1. The release's tables against compact.rs.
python3 -I -B "$here/extract.py" "$release" > "$work/release-tables.txt"
python3 -I -B - "$work/release-tables.txt" "$repo/dashd-p2p/src/processing/compact.rs" <<'EOF'
import json, re, sys
table = json.loads(open(sys.argv[1]).read().splitlines()[-1])
src = open(sys.argv[2]).read()
body = src[src.index("const PRIME_LIST"):]
body = body[body.index("[\n") + 2:body.index("];")]
assert [int(x) for x in re.findall(r"\d+", body)] == table, "compact.rs PRIME_LIST differs"
print("PRIME_LIST matches the release dashd")
EOF

# 2. The two probes.
docker run --rm --user "$(id -u):$(id -g)" -v "$work":/p -w /p "$image" \
  g++ -O2 -static probe.cpp -o probe-gcc13
g++ -O2 -static "$work/probe.cpp" -o "$work/probe-host"
"$work/probe-gcc13" > "$work/sweep-gcc13.txt"
"$work/probe-host" > "$work/sweep-host.txt"
tail -n 1 "$work/sweep-gcc13.txt" "$work/sweep-host.txt"
awk 'NR <= 65536 && $2 != $3 { bad++ } END { if (bad) { print "rehash after n inserts" > "/dev/stderr"; exit 1 } }' \
  "$work/sweep-gcc13.txt"
if cmp -s <(head -n 65536 "$work/sweep-gcc13.txt") <(head -n 65536 "$work/sweep-host.txt"); then
  echo "diagnostic: this box's g++ agrees with GCC 13.3.0"
else
  echo "diagnostic: this box's g++ DIFFERS from GCC 13.3.0 (E4's oracle must use GCC 13.3.0, D21)"
fi

# 3. The fixture.
{
  echo "# bucket_count() of std::unordered_map<uint64_t, uint16_t>(n), n = 0..=65535, from"
  echo "# contrib/node/p3-buckets/run.sh: probe.cpp built -O2 -static in $image."
  echo "# Each line is 'n count': the count from n until the next line's n. It is the same after"
  echo "# n distinct inserts (no rehash)."
  awk 'NR <= 65536 { if ($2 != p) { print $1, $2; p = $2 } }' "$work/sweep-gcc13.txt"
} > "$fixture"
echo "wrote $fixture ($(grep -vc '^#' "$fixture") transitions)"

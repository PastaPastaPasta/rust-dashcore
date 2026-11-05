#!/usr/bin/env bash
# Writes tests/data/core-fixture.json: SHA-256 digests of Core's output streams for inputs both
# sides generate (tests/core.rs `core_fixture` recomputes them with this crate).
#
# Usage: contrib/core-fixture.sh <harness> > tests/data/core-fixture.json
# <harness> is a binary built by contrib/core-harness/build.sh.
set -euo pipefail
shopt -s inherit_errexit

harness=$1 seed=0x58313131 n=2000
# The digest of a harness run's output; fails if the harness does or prints nothing.
sum() {
    local out
    out=$("$harness" "$@" | sha256sum | cut -d' ' -f1)
    [ "$out" != e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 ] || return 1
    echo "$out"
}
random=$(sum stream random $seed $n)
long=$(sum stream long $seed $n)
header=$(sum stream header $seed $n)
stages=()
for stage in blake bmw groestl skein jh keccak luffa cubehash shavite simd echo; do
    stages+=("$stage" "$(sum stage "$stage" $seed $n)")
done
echo "{"
echo "  \"seed\": $((seed)),"
echo "  \"cases\": $n,"
echo "  \"x11\": {"
echo "    \"random\": \"$random\","
echo "    \"long\": \"$long\","
echo "    \"header\": \"$header\""
echo "  },"
echo "  \"stages\": {"
for ((i = 0; i < ${#stages[@]}; i += 2)); do
    sep=","
    [ $((i + 2)) -lt ${#stages[@]} ] || sep=""
    echo "    \"${stages[i]}\": \"${stages[i + 1]}\"$sep"
done
echo "  }"
echo "}"

#!/usr/bin/env bash
# Copy Dash Core's JSON/hex test data, verbatim, into test-vectors/core/<commit>/.
#
# The files are read from git objects (`git show <commit>:<path>`), never from a working tree,
# so local edits or build artifacts in the Core checkout cannot leak into the vectors.
# Updating the pin is a reviewed change: run this, update PROVENANCE.md, commit.
#
# Usage: contrib/node/sync-core-vectors.sh <dash-core-checkout> <commit-ish>
#   contrib/node/sync-core-vectors.sh ~/workspace/dash v24.0.0-rc.3
#
# Copies:
#   src/test/data/*            (every file)
#   src/bench/data/*           (every file; block813851.raw, Core's benchmark block)
#   test/functional/data/*     (everything except Python modules)
#   test/util/data/*           (everything except the backport bookkeeping list)
#   COPYING                    (Core's MIT licence text)
# Then rewrites core/<commit>/FILES.sha256, the checksum list that `cargo test -p
# dash-core-vectors` verifies.
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <dash-core-checkout> <commit-ish>" >&2
    exit 2
fi
core="$1"
rev="$2"

commit="$(git -C "$core" rev-parse --verify "${rev}^{commit}")"
short="${commit:0:12}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dest="$here/test-vectors/core/$short"

mkdir -p "$dest"
git -C "$core" ls-tree -r --name-only "$commit" -- src/test/data src/bench/data test/functional/data test/util/data |
    grep -v -e '\.py$' -e '^test/util/data/non-backported\.txt$' |
    while IFS= read -r path; do
        mkdir -p "$dest/$(dirname "$path")"
        git -C "$core" show "$commit:$path" >"$dest/$path"
    done
git -C "$core" show "$commit:COPYING" >"$dest/COPYING"

# Checksums of the verbatim files only (not of PROVENANCE.md, transcribed/ or golden/).
(
    cd "$dest"
    find src test COPYING -type f -print0 | sort -z | xargs -0 sha256sum >FILES.sha256
)

echo "synced $(wc -l <"$dest/FILES.sha256") files from $commit into $dest"
echo "now update $dest/PROVENANCE.md (commit $commit, tag/describe: $(git -C "$core" describe --tags "$commit" 2>/dev/null || echo none))"

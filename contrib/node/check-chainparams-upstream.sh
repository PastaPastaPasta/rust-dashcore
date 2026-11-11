#!/usr/bin/env bash
# POL-3c's upstream gate: dash-chainparams builds and passes its tests on upstream base-sdk, not
# only on the node's fork. The workspace builds the facade against the fork through the root
# `[patch."https://github.com/dashpay/base-sdk"]` table. This script copies the checkout to a
# scratch directory, deletes that table there, so base-sdk resolves to the `rev` the root
# `[workspace.dependencies]` names (upstream), and runs `cargo test -p dash-chainparams` with
# default features and with `std`. `fork-equivalence`, which needs the fork's builder, stays off.
# The lock is re-resolved (no `--locked`), and the build has its own target directory.
#
# The gate certifies one upstream revision, CERTIFIED below. The root `bsdk-params` rev must
# name it, and the copy's lock must resolve every base-sdk package to exactly that GitHub source.
# The base-sdk packages are those the removed table or the root's base-sdk declarations name, any
# locked package whose source mentions base-sdk or is a local git checkout, and any path package
# inside a base-sdk checkout (a directory with pkgs/params naming dash-params). A base-sdk pin
# move re-certifies here: run the gate at the new rev, then update CERTIFIED.
#
# Usage: contrib/node/check-chainparams-upstream.sh [scratch-dir]
#   scratch-dir  where the copy and its target go (default: a new directory under $TMPDIR);
#                it is deleted on exit unless KEEP_SCRATCH=1.
# Exit status: cargo's; 1 when a base-sdk package resolves anywhere but CERTIFIED; 2 when the root
# pin is not CERTIFIED or the copy cannot be prepared.
set -euo pipefail

CERTIFIED=76832fafed4a8f120ef7fbf8d25dec8bbdb45c9b
root="$(cd "$(dirname "$0")/../.." && pwd)"
pinned="$(sed -n 's/^bsdk-params = .*rev = "\([0-9a-f]*\)".*/\1/p' "$root/Cargo.toml")"
if [ "$pinned" != "$CERTIFIED" ]; then
    echo "check-chainparams-upstream.sh: the root bsdk-params rev is ${pinned:-missing}, but the gate" \
        "certifies $CERTIFIED; a base-sdk pin move re-certifies here (run the gate, update CERTIFIED)" >&2
    exit 2
fi
scratch="${1:-}"
if [ -z "$scratch" ]; then
    scratch="$(mktemp -d "${TMPDIR:-/tmp}/chainparams-upstream.XXXXXX")"
else
    mkdir -p "$scratch"
fi
case "${KEEP_SCRATCH:-0}" in
    1) ;;
    *) trap 'rm -rf "$scratch/src" "$scratch/base-sdk-packages"' EXIT ;;
esac
copy="$scratch/src"
rm -rf "$copy"
mkdir -p "$copy"

# The tracked and untracked (not ignored) files, as the checkout has them. A tracked file deleted
# in the checkout is skipped; any other copy failure fails the gate.
if ! (cd "$root" && git ls-files -z --cached --others --exclude-standard |
    while IFS= read -r -d '' f; do if [ -e "$f" ] || [ -L "$f" ]; then printf '%s\0' "$f"; fi; done |
    xargs -0 -r cp --parents -P -t "$copy"); then
    echo "check-chainparams-upstream.sh: copying the checkout failed" >&2
    exit 2
fi
if [ ! -f "$copy/Cargo.toml" ]; then
    echo "check-chainparams-upstream.sh: the copy has no Cargo.toml" >&2
    exit 2
fi

# Delete the root `[patch."https://github.com/dashpay/base-sdk"]` table: its header and every
# line up to the next table header.
python3 -I - "$copy/Cargo.toml" "$scratch/base-sdk-packages" <<'EOF' || exit 2
import re
import sys
import tomllib


def fail(message):
    print(message, file=sys.stderr)
    sys.exit(2)


path, names_path = sys.argv[1:]
text = open(path).read()
lines = text.splitlines(keepends=True)
header = '[patch."https://github.com/dashpay/base-sdk"]'
starts = [i for i, line in enumerate(lines) if line.strip() == header]
if len(starts) != 1:
    fail(f"expected one {header} table, found {len(starts)}")
end = next((j for j in range(starts[0] + 1, len(lines)) if re.match(r"\s*\[", lines[j])), len(lines))
removed = [l for l in lines[starts[0] + 1:end] if l.strip() and not l.lstrip().startswith("#")]
if not removed:
    fail(f"{header} has no entries")
try:
    manifest = tomllib.loads(text)
except ValueError as error:
    fail(f"{path}: {error}")
declared = list(manifest["patch"]["https://github.com/dashpay/base-sdk"].items())
declared += [(k, v) for k, v in manifest.get("workspace", {}).get("dependencies", {}).items()
             if isinstance(v, dict) and "dashpay/base-sdk" in str(v.get("git", ""))]
names = sorted({v.get("package", k) if isinstance(v, dict) else k for k, v in declared})
open(names_path, "w").write("".join(f"{n}\n" for n in names))
open(path, "w").write("".join(lines[:starts[0]] + lines[end:]))
print(f"removed {header} ({len(removed)} entries)")
EOF
if grep -q 'patch."https://github.com/dashpay/base-sdk"' "$copy/Cargo.toml"; then
    echo "check-chainparams-upstream.sh: the base-sdk [patch] is still there" >&2
    exit 2
fi

cd "$copy"
export CARGO_TARGET_DIR="$scratch/target"
echo "base-sdk dash-params at $CERTIFIED (upstream)"
cargo test -p dash-chainparams
cargo test -p dash-chainparams --features std
want="git+https://github.com/dashpay/base-sdk?rev=$CERTIFIED#$CERTIFIED"
cargo metadata --format-version 1 --locked >"$scratch/src/.metadata.json"
python3 -I - Cargo.lock "$want" "$scratch/base-sdk-packages" "$scratch/src/.metadata.json" <<'EOF' || exit 1
import json
import pathlib
import re
import sys
import tomllib

lock, want, names_path, metadata_path = sys.argv[1:]
named = set(open(names_path).read().split())


def in_base_sdk_checkout(manifest):
    for directory in pathlib.Path(manifest).resolve().parents:
        params = directory / "pkgs" / "params" / "Cargo.toml"
        if params.is_file() and re.search(r'(?m)^name = "dash-params"$', params.read_text()):
            return True
    return False


checkout_paths = {(p["name"], p["version"]) for p in json.load(open(metadata_path))["packages"]
                  if p["source"] is None and in_base_sdk_checkout(p["manifest_path"])}
bad, locked = [], set()
for package in tomllib.load(open(lock, "rb")).get("package", []):
    source = package.get("source")
    if (package["name"] in named or (package["name"], package["version"]) in checkout_paths
            or (source and ("base-sdk" in source or source.startswith("git+file:")))):
        locked.add(package["name"])
        if source != want:
            bad.append(f"{package['name']} {package['version']} resolves to {source or 'a path'}")
if "dash-params" not in locked:
    bad.append("dash-params is not in Cargo.lock")
for line in bad:
    print(f"check-chainparams-upstream.sh: {line}, not {want}", file=sys.stderr)
sys.exit(1 if bad else 0)
EOF
echo "dash-chainparams passes on upstream base-sdk $CERTIFIED"

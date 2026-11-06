#!/usr/bin/env bash
# The shared-runtime hygiene rules (SB0 §2.3 L5, §7) for every crate
# layers.toml puts in S:
#   - clippy with contrib/node/shared-clippy.toml, plus pure-clippy.toml's
#     blst and dash-pkc entries (DEC-79's pooled calls, under its audit
#     stamp), rejects process exits, OS threads, runtimes and pools,
#     blocking offload and resolver calls, in its library, with all features
#     and with no default features, on the host and for aarch64-linux-android
#     (the Apple targets need the Mac: BL-74). The lints are forbidden, so
#     `#[allow]` cannot lift them;
#   - cargo-deny with contrib/node/shared-deny.toml, rooted at that crate
#     with all features and without dev-dependencies, rejects crates that
#     start threads, pools or runtimes (blst's pool only through blst and
#     dash-pkc, whose pooled calls clippy bans);
#   - contrib/node/link-probe.py on the host and each LINT_TARGET: every
#     native package of its all-features closure (a `links` key, or a cc,
#     cmake or bindgen build-dependency) has a reviewed layers.toml [[native]]
#     record (DEC-188 (3)); a consumer binary that links the crate and sets
#     its own allocator links, with all features, the default ones and none;
#     and, as an advisory warning only, no `global_allocator` token in the
#     closure's sources (DEC-176, DEC-188 (4)). The allocator proof is the
#     canary link of every registered product (check-products.sh, DEC-181);
#     consumers outside layers.toml's [[product]] registry are out of scope;
#     A reviewed layers.toml [[l5-exception]] record (crate, disallowed path,
#     file, function, decision, until) exempts exactly one call: the one in
#     that function's body. Any other hit fails, and so does a record whose
#     function no longer holds the call (stale) or holds two (DEC-205;
#     contrib/node/l5-exceptions.py). Clippy lints the crate alone
#     (`--no-deps`): a workspace dependency's hits, and its records, are
#     judged by that dependency's own check, so a hit there neither stops
#     the crate from being linted nor takes cover under the crate's records;
#   - every workspace crate in its normal and build closure (all features,
#     every target) is itself S or P, so that a check (this one or
#     check-pure.sh) lints it;
#   - only dash-p2p depends on tokio (directly, in any dependency table but
#     dev); it uses the host's Handle.
# Network parameters in statics are left to review.
#
# Usage: contrib/node/check-shared.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
. contrib/node/clippy-lib.sh
list="$(layer_crates S)"

if [ -z "$list" ]; then
    echo "shared crates: layers.toml has no S workspace crate, nothing to check"
    exit 0
fi

read_stamp
lint_env

# The S crates with a direct normal or build tokio dependency, other than dash-p2p.
tokio_users="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c '
import json, sys
shared = set(sys.argv[1:])
for p in json.load(sys.stdin)["packages"]:
    if p["name"] in shared and p["name"] != "dash-p2p" and any(
            d["name"] == "tokio" and d["kind"] != "dev" for d in p["dependencies"]):
        print(p["name"])
' $(cut -d ' ' -f 1 <<< "$list"))"

# `<crate> <member>` for each workspace crate outside S and P in an S crate's
# normal or build closure: --no-deps lints only the crate itself, so every
# workspace crate it builds on must be linted by its own check.
unchecked="$(cargo metadata --locked --all-features --format-version 1 | python3 -c '
import json, sys
meta, shared, checked = json.load(sys.stdin), set(sys.argv[1].split()), set(sys.argv[2].split())
members = set(meta["workspace_members"])
name = {p["id"]: p["name"] for p in meta["packages"]}
deps = {n["id"]: [d["pkg"] for d in n["deps"] if any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
        for n in meta["resolve"]["nodes"]}
for root in sorted(i for i in members if name[i] in shared):
    seen, todo = {root}, [root]
    while todo:
        for d in deps[todo.pop()]:
            if d not in seen:
                seen.add(d)
                todo.append(d)
    for d in sorted(seen & members, key=name.get):
        if name[d] not in checked:
            print(name[root], name[d])
' "$(cut -d ' ' -f 1 <<< "$list")" "$(layer_crates S P | cut -d ' ' -f 1)")"

# lint with L5's lints forbidden, as clippy-lib.sh's lint does, judged by
# l5-exceptions.py so that a [[l5-exception]] covers its one call.
lint_shared() {  # lint_shared <crate> <conf dir> <manifest>
    local features target n=0 runs=() dir status
    dir="$(mktemp -d)"
    for target in "" "${LINT_TARGETS[@]}"; do
        for features in --all-features --no-default-features; do
            n=$((n + 1))
            status=0
            CLIPPY_CONF_DIR="$2" cargo clippy --locked --no-deps -p "$1" --lib "$features" ${target:+--target "$target"} \
                --message-format=json -- -F clippy::disallowed_methods -F clippy::disallowed_types \
                > "$dir/$n.json" || status=$?
            runs+=("$status:$dir/$n.json")
        done
    done
    status=0
    python3 contrib/node/l5-exceptions.py "$1" "$3" "${runs[@]}" || status=1
    rm -rf "$dir"
    return $status
}

failed=()
while read -r crate manifest; do
    start_crate "$crate" "$manifest" || continue
    ok=1
    if grep -qx "$crate" <<< "$tokio_users"; then
        echo "error: $crate depends on tokio; only dash-p2p uses tokio, through the host's Handle (SB0 L5)" >&2
        ok=
    fi
    while read -r root dep; do
        [ "$root" = "$crate" ] || continue
        echo "error: $crate depends on $dep, a workspace crate outside S and P that no check lints; give it a layer (SB0 L5)" >&2
        ok=
    done <<< "$unchecked"
    if ! python3 contrib/node/link-probe.py "$crate" "$manifest" "${LINT_TARGETS[@]}"; then
        ok=
    fi
    if ! cargo deny --manifest-path "$manifest" --config contrib/node/shared-deny.toml \
            --all-features --exclude-dev --locked check bans; then
        ok=
    fi
    if ! conf="$(write_conf node-shared-clippy "$crate" "$manifest" contrib/node/shared-clippy.toml \
            contrib/node/pure-clippy.toml:blst::,dash_pkc::)" || ! blst_audited "$crate" ||
        ! lint_shared "$crate" "$conf" "$manifest"; then
        ok=
    fi
    [ -n "$ok" ] || failed+=("$crate")
done <<< "$list"

finish "shared crates" "$list"

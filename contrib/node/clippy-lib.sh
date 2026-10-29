# Shared by check-pure.sh (L4) and check-shared.sh (L5): the per-crate clippy
# configuration, the lint run (host and Android), and the blst audit stamp.
# Source it from the repository root.

# layer_crates <layer>... [pure]: one `<crate> <manifest>` line per workspace
# crate layers.toml puts in one of the layers (with `pure`, also each crate it
# marks `pure = true`); `-` when it has no library.
layer_crates() { python3 contrib/node/check-layers.py crates "$@"; }

# Per crate: `== <crate>`; fails (and records it) when the crate has no library.
start_crate() {  # start_crate <crate> <manifest>
    echo "== $1"
    [ "$2" != - ] && return
    echo "error: $1 is not a workspace library crate" >&2
    failed+=("$1")
    return 1
}

# finish <check> <list>: reports a stale stamp and the failed crates, exiting 1 on either.
finish() {
    [ -z "$stale" ] || echo "$1 check failed: pure-clippy.toml's audit stamp is stale or missing" >&2
    [ "${#failed[@]}" = 0 ] || echo "$1 check failed for: ${failed[*]}" >&2
    if [ -n "$stale" ] || [ "${#failed[@]}" != 0 ]; then
        exit 1
    fi
    echo "$1: $(wc -l <<< "$2") crate(s) ok"
}

# write_conf <dir name> <crate> <manifest> <file[:prefix,...]>...: merges the
# root clippy.toml, the given files and the crate's own clippy.toml (so its
# bans are forbid-level too) into target/<dir name>/<crate>/clippy.toml; prints
# that directory. A `:prefix,...` suffix keeps only the file's entries whose
# `path` starts with one of the prefixes. The files may set the same list (the
# root bans base-sdk's inherent Block::merkle for every crate, and a crate's
# file repeats the root's), so they are merged as TOML: lists are concatenated
# without duplicates, and any other key set in more than one must agree.
write_conf() {
    local conf="target/$1/$2" manifest="$3" own
    shift 3
    local files=(clippy.toml "$@")
    own="$(dirname "$manifest")/clippy.toml"
    [ -f "$own" ] && files+=("$own")
    mkdir -p "$conf"
    python3 - "${files[@]}" > "$conf/clippy.toml.new" <<'EOF' || { rm -f "$conf/clippy.toml.new"; return 1; }
import json, sys, tomllib

merged = {}
for arg in sys.argv[1:]:
    path, _, prefixes = arg.partition(":")
    prefixes = tuple(prefixes.split(",")) if prefixes else None
    with open(path, "rb") as f:
        for key, value in tomllib.load(f).items():
            if prefixes is not None:
                if not isinstance(value, list):
                    continue
                value = [v for v in value if isinstance(v, dict) and v.get("path", "").startswith(prefixes)]
            if key not in merged:
                merged[key] = value
            elif isinstance(value, list) and isinstance(merged[key], list):
                merged[key] = merged[key] + [item for item in value if item not in merged[key]]
            elif merged[key] != value:
                sys.exit(f"{path}: {key} conflicts with an earlier clippy.toml")

def emit(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)  # a valid TOML basic string
    if isinstance(value, int):
        return str(value)
    if isinstance(value, list):
        return "[\n" + "".join(f"    {emit(item)},\n" for item in value) + "]"
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{k} = {emit(v)}" for k, v in value.items()) + " }"
    sys.exit(f"unsupported clippy.toml value {value!r}")

for key, value in merged.items():
    print(f"{key} = {emit(value)}")
EOF
    # Rewritten only when it changes, so clippy's cached results stay valid.
    cmp -s "$conf/clippy.toml.new" "$conf/clippy.toml" || mv "$conf/clippy.toml.new" "$conf/clippy.toml"
    rm -f "$conf/clippy.toml.new"
    echo "$PWD/$conf"
}

# The targets lint runs on besides the host: a body behind
# `cfg(target_os = "android")` is linted only when built for Android. The
# Apple targets need the Mac (BL-74).
LINT_TARGETS=(aarch64-linux-android)

# lint_env: the NDK environment for the LINT_TARGETS (contrib/mobile/android-env.sh);
# exits when there is no NDK, as the Android run is part of the gate.
lint_env() {
    . contrib/mobile/android-env.sh || {
        echo "error: the Android lint needs the NDK (contrib/mobile/android-env.sh)" >&2
        exit 1
    }
    # The SQLite trim is for the L7 build only (check-mobile.sh): with it here, every host lint
    # would rebuild libsqlite3-sys, which reads LIBSQLITE3_FLAGS whatever the target.
    unset LIBSQLITE3_FLAGS
}

lint() {  # lint <crate> <conf dir>: on the host and each LINT_TARGET, with all features and with none
    local features target
    for target in "" "${LINT_TARGETS[@]}"; do
        for features in --all-features --no-default-features; do
            CLIPPY_CONF_DIR="$2" cargo clippy --locked -p "$1" --lib "$features" ${target:+--target "$target"} -- \
                -F clippy::disallowed_methods -F clippy::disallowed_types || return 1
        done
    done
}

# pure-clippy.toml's audit stamp: the blst release and base SDK revision its
# thread-pool bans were audited against (DEC-71/79). A crate linking any
# other blst, or a base SDK pin at any other revision, fails until the bans
# are re-audited and the stamp updated. Sets audited_blst and stale.
read_stamp() {
    local stamp audited_base_sdk pinned_base_sdk
    stamp="$(sed -n 's/^# audited: blst \([0-9.]*\), base-sdk \([0-9a-f]\{40\}\)$/\1 \2/p' contrib/node/pure-clippy.toml)"
    read -r audited_blst audited_base_sdk <<< "$stamp" || true
    stale=
    if [ -z "${audited_base_sdk:-}" ]; then
        echo "error: contrib/node/pure-clippy.toml has no \`# audited: blst <version>, base-sdk <revision>\` line (DEC-71/79)" >&2
        stale=1
        return
    fi
    pinned_base_sdk="$(python3 contrib/node/base_sdk_pins.py show | cut -d ' ' -f 2)"
    if [ "$pinned_base_sdk" != "$audited_base_sdk" ]; then
        echo "error: the base SDK pin is $pinned_base_sdk, but pure-clippy.toml's thread-pool bans were audited at $audited_base_sdk; re-audit them and update its stamp (DEC-71/79)" >&2
        stale=1
    fi
}

blst_audited() {  # blst_audited <crate>
    local found
    found="$(cargo tree --locked -q -p "$1" --all-features -e normal --prefix none --format '{p}' |
        sed -n 's/^blst v\([^ ]*\).*/\1/p' | sort -u | paste -sd ' ')" || {
        echo "error: cargo tree failed for $1" >&2
        return 1
    }
    if [ -z "$found" ] || [ "$found" = "$audited_blst" ]; then
        return 0
    fi
    echo "error: $1 links blst $found; pure-clippy.toml's thread-pool bans were audited against blst $audited_blst (DEC-71)" >&2
    return 1
}

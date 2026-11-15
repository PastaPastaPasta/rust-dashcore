#!/usr/bin/env bash
# CS1 §7.2 branch census: every site in the script system that returns a ScriptError must be
# reached by Core-backed evidence, and so must every other line of those files, so that success
# paths are held to the same standard. Core-backed means the test binaries whose expectations
# Dash Core computed: O4's transcribed records (script_records), the recorder's fixture
# (script_core_recorded, quirks) and Core's own unit tests (script_core_units). The in-crate unit
# tests and the property tests are not counted: they check the port against its own expectations.
#
# Lines that Core's own logic makes unreachable carry a `// census: unreachable (<reason>)` marker,
# and lines whose Core evidence arrives with CS1 phase B carry `// census: phase B (<reason>)`, on
# the same line or the line above; a marker in the comments directly above a `fn` covers the
# whole function. Both are listed, not counted. Coverage is per line, so the marker is also how a
# site sharing its line with code that always runs (an eager `ok_or`) is kept honest. A `?` after
# a call that can fail shares its line with the success path; the CS1a review measured those arms
# with region coverage over the same binaries (every reachable one taken).
#
# Builds the tests with -C instrument-coverage into target/census (inside this worktree, so the
# normal build is not invalidated), runs them, merges the profiles with the pinned toolchain's
# llvm-profdata and reads line counts with llvm-cov. Needs `rustup component add llvm-tools`.
#
# Usage: contrib/node/script-branch-census.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
target="target/census"
profiles="$target/profiles"
rm -rf "$profiles"
mkdir -p "$profiles"

sysroot="$(rustc --print sysroot)"
tools="$sysroot/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -x "$tools/llvm-cov" ] || { echo "llvm-cov missing: rustup component add llvm-tools" >&2; exit 2; }

export RUSTFLAGS="-C instrument-coverage"
export LLVM_PROFILE_FILE="$PWD/$profiles/%p-%m.profraw"
core_backed=(--test script_records --test script_core_recorded --test script_core_units --test quirks)
objects="$(cargo test --locked -p dash-consensus --target-dir "$target" "${core_backed[@]}" --no-run \
    --message-format=json |
    python3 -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["executable"])')"
[ "$(wc -w <<<"$objects")" -eq 4 ] || { echo "expected 4 test binaries, got: $objects" >&2; exit 2; }
# The test output goes to stderr, so that a failure is diagnosable and stdout is the census.
cargo test --locked -p dash-consensus --target-dir "$target" "${core_backed[@]}" --quiet >&2

"$tools/llvm-profdata" merge -sparse "$profiles"/*.profraw -o "$target/census.profdata"
args=()
for o in $objects; do args+=(--object "$o"); done
"$tools/llvm-cov" export --format=lcov --instr-profile "$target/census.profdata" "${args[@]}" \
    --ignore-filename-regex='(/\.cargo/|/rustc/|/tests/)' > "$target/census.lcov"

python3 - "$target/census.lcov" <<'EOF'
import re, sys
from pathlib import Path

FILES = ["interpreter.rs", "encoding.rs", "num.rs", "parse.rs", "checker.rs", "ecdsa.rs", "sigops.rs", "flags.rs"]
hits = {}
current = None
for line in open(sys.argv[1]):
    if line.startswith("SF:"):
        current = line[3:].strip()
    elif line.startswith("DA:") and current:
        n, count = line[3:].split(",")[:2]
        hits.setdefault(current, {})[int(n)] = int(count)

site = re.compile(r"\bErr\((ScriptError::)?[A-Z]\w*\)|\.ok_or\(ScriptError::|Err\(ScriptNumError\)")
sites_reached, lines_reached, unreachable, deferred, missed = 0, 0, [], [], []
for name in FILES:
    path = Path("dash-consensus/src/script") / name
    counts = next((v for k, v in hits.items() if k.endswith(f"/script/{name}")), {})
    if not counts:
        missed.append(f"{name}: no coverage data")
    in_tests = False
    lines = path.read_text().splitlines()
    # A marker in the `//` comments directly above a function (after its docs) covers all of it.
    block = {}
    for j, text in enumerate(lines):
        code = text.lstrip()
        if not re.match(r"(pub(\(crate\))? )?(const )?fn ", code):
            continue
        k = j - 1
        while k >= 0 and lines[k].lstrip().startswith("//") and not lines[k].lstrip().startswith("///"):
            if "census: " in lines[k]:
                indent = text[: len(text) - len(code)]
                last = j if code.rstrip().endswith("}") else next(
                    e for e in range(j + 1, len(lines)) if lines[e] == indent + "}")
                for e in range(j, last + 1):
                    block[e + 1] = lines[k]
                break
            k -= 1
    for n, text in enumerate(lines, 1):
        if text.startswith("#[cfg(test)]"):
            in_tests = True
        is_site = bool(site.search(text))
        if in_tests or text.lstrip().startswith("//") or not (is_site or n in counts):
            continue
        where = f"{name}:{n}: {text.strip()}"
        marker = text + (lines[n - 2] if n > 1 else "") + block.get(n, "")
        if "census: unreachable" in marker:
            unreachable.append(where)
        elif "census: phase B" in marker:
            deferred.append(where)
        elif counts.get(n, 0) == 0:
            missed.append(where)
        else:
            lines_reached += 1
            sites_reached += is_site
print(f"lines reached by Core-backed tests: {lines_reached} (script error sites among them: "
      f"{sites_reached}); marked unreachable: {len(unreachable)}; phase B: {len(deferred)}; "
      f"missed: {len(missed)}")
for u in unreachable:
    print("  unreachable", u)
for d in deferred:
    print("  phase B    ", d)
for m in missed:
    print("  MISSED     ", m)
sys.exit(1 if missed else 0)
EOF

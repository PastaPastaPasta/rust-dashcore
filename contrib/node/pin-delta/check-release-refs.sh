#!/usr/bin/env bash
# DEC-82 (v24 only): no per-release code. The node targets Dash Core v24 alone, so no Rust file
# may name the v23.1 release or bring back the release-profile axis: `Release::V23_1`, the
# `V23_1` identifiers (`Pin::V23_1`, `UngatedRules::V23_1`), `UngatedRules`, `ReleaseProfile`,
# `release_profile` or the `-releaseprofile` option. A match or comparison on the release is the
# same defect in other words, so `Release::` may only be named as `Release::V24_0`, the value
# `dash-params`' chain-parameter builder takes. It is never compared (`r == Release::V24_0`,
# `Release::V24_0 != r`), tested with `matches!` or `if let`, or used as a match arm
# (`Release::V24_0 =>`, `Release::V24_0 | …`), on one line or several. `assert_eq!` on a stored
# release is not a branch and is not refused.
#
# Replaces the O17 §2 rule-2 check (no `Release::` outside release modules; CS0, DEC-41), which
# governed the profile groups DEC-82 removed. docs/ (the O17 audit and the node-program notes) is
# history and is not checked.
#
# Usage: contrib/node/pin-delta/check-release-refs.sh [--selftest]
set -euo pipefail

PATTERN='\bRelease::[A-Za-z_0-9]+|[vV]23_1|\bUngatedRules\b|\bReleaseProfile\b|\brelease_profile\b|releaseprofile'
# A branch on the release, found in whole files so rustfmt's line breaks cannot hide it. It is
# matched on code only: comments (nested block comments too) and the contents of char, string and
# raw string (`r#"…"#`, `br"…"`) literals are blanked first (CODE_ONLY), so prose such as
# "match <height> … --release" in a doc comment is not a branch (BL-262).
BRANCH='(?:matches!\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\bRelease::'\
'|(?:==|!=)\s*Release::'\
'|\bRelease::\w+\s*(?:==|!=|=>|\|(?!\|)|if\b)'\
'|\blet\s+Release::'\
'|\bmatch\s[^{;]*\brelease\b)'
# Perl: print each NUL-separated file name whose code, with comments and literals blanked, has a $BRANCH.
CODE_ONLY='chomp; my $f = $_; open(my $h, "<", $f) or next; my $s = do { local $/; <$h> };
    $s =~ s{(?<ch>\x27(?:\\.|[^\\\x27\n])\x27)|(?<str>(?<!\w)b?r(?<h>#*)".*?"\k<h>|"(?:\\.|[^"\\])*")|//[^\n]*|(?<cm>/\*(?:(?&cm)|(?!/\*|\*/).)*\*/)}{defined $+{ch} ? "\x27 \x27" : defined $+{str} ? "\"\"" : " "}gse;
    print "$f\n" if $s =~ /$ENV{BRANCH}/;'

check() {  # check <repository root>
    local hit text found=""
    while IFS= read -r hit; do
        text="${hit#*:*:}"
        # Release::V24_0 is the one name allowed; refuse the line if anything else matches.
        if grep -qE "$PATTERN" <<<"$(sed -E 's/\bRelease::V24_0\b//g' <<<"$text")"; then
            found+="$hit"$'\n'
        fi
    done < <(git -C "$1" grep -n -E "$PATTERN" -- '*.rs' ':(exclude)docs' || true)
    local file branches=""
    while IFS= read -r -d '' file; do
        branches+="$file: a branch on the release"$'\n'
    done < <(git -C "$1" ls-files -z -- '*.rs' ':(exclude)docs' | (cd "$1" && BRANCH="$BRANCH" perl -0 -ne "$CODE_ONLY") | tr '\n' '\0')
    found+="$branches"
    if [ -n "$found" ]; then
        echo "per-release code (DEC-82: v24 only); only Release::V24_0 may be named, never branched on:" >&2
        printf '%s' "$found" >&2
        return 1
    fi
}

if [ "${1:-}" = --selftest ]; then
    dir="$(mktemp -d "${TMPDIR:-/work/scratch}/check-release-refs.XXXXXX")"
    trap 'rm -rf "$dir"' EXIT
    git -C "$dir" init -q
    accepted() {  # accepted <path> <content>: the check passes with this file added
        mkdir -p "$(dirname "$dir/$1")"
        printf '%s\n' "$2" >"$dir/$1"
        git -C "$dir" add "$1"
        check "$dir" || { echo "selftest: $1 was rejected" >&2; exit 1; }
    }
    rejected() {  # rejected <path> <content>: the check fails with this file, which is removed
        mkdir -p "$(dirname "$dir/$1")"
        printf '%s\n' "$2" >"$dir/$1"
        git -C "$dir" add "$1"
        if check "$dir" 2>/dev/null; then echo "selftest: $1 passed" >&2; exit 1; fi
        git -C "$dir" rm -q -f "$1"
    }
    accepted a/src/lib.rs 'fn p() { build(Release::V24_0); }'
    accepted a/tests/fixtures.rs 'let chain = ChainParamsBuilder::new(network, Release::V24_0).build();'
    accepted a/src/notes.rs '// DEC-82 dropped the old release, so nothing names it in code.'
    accepted a/src/pin.rs 'fn t() -> Pin { Pin::V24_0 }'
    accepted docs/example.rs 'fn history() { build(Release::V23_1); }'
    rejected b/src/a.rs 'fn g() -> bool { rules.release == Release::V23_1 }'
    rejected b/tests/t.rs 'fn t() { build(Release::V23_1); }'
    rejected b/src/pin.rs 'fn t() -> Pin { Pin::V23_1 }'
    rejected b/src/rules.rs 'fn t(r: &UngatedRules) {}'
    rejected b/src/profile.rs 'struct ReleaseProfile;'
    rejected b/src/profile2.rs 'let release_profile = 1;'
    rejected b/src/option.rs 'const OPT: &str = "-releaseprofile";'
    rejected b/src/cmp.rs 'fn g(r: Release) -> bool { r == Release::V25_0 }'
    rejected b/src/all.rs 'for release in Release::ALL {}'
    accepted a/src/multi.rs $'let cp = ChainParamsBuilder::new(\n    network,\n    Release::V24_0,\n)\n.build();'
    accepted a/src/other.rs 'fn g(x: Foo) -> bool { matches!(x, Foo::Bar) && x == Foo::Baz }'
    accepted a/tests/stored.rs 'assert_eq!(chain.release, Release::V24_0);'
    rejected b/src/rev.rs 'fn g(r: Release) -> bool { Release::V24_0 == r }'
    rejected b/src/revne.rs 'fn g(r: Release) -> bool { Release::V24_0 != r }'
    rejected b/src/m1.rs 'fn g(r: Release) -> bool { matches!(r, Release::V24_0) }'
    rejected b/src/m2.rs $'fn g(r: Release) -> bool {\n    matches!(\n        r,\n        Release::V24_0\n    )\n}'
    rejected b/src/iflet.rs 'fn g(r: Release) { if let Release::V24_0 = r { f() } }'
    rejected b/src/orpat.rs 'fn g(r: Release) { match r { Release::V24_0 | Release::V25_0 => 1, _ => 2 }; }'
    rejected b/src/split.rs $'fn g(r: Release) -> bool {\n    r ==\n        Release::V24_0\n}'
    rejected b/src/eq.rs 'fn g(c: &Chain) -> bool { c.release == Release::V24_0 }'
    rejected b/src/ne.rs 'fn g(c: &Chain) -> bool { c.release != Release::V24_0 }'
    rejected b/src/arm.rs 'fn g(r: Release) { match r { Release::V24_0 => 1, _ => 2 }; }'
    rejected b/src/lower.rs 'let pin = oracle_v23_1_tx_decode;'
    rejected b/src/mixed.rs 'fn g() { build(Release::V24_0); build(Release::V23_1); }'
    # BL-262: a branch is matched in code, never across comments or literals.
    accepted a/src/doc.rs $'//! Prints `match <height>` per block.\n//!\n//! `cargo run -p dash-dml --release`\nfn main() {}'
    accepted a/src/line.rs $'fn f(x: u8) -> u8 { match x { _ => 1 } } // the release\n'
    accepted a/src/block.rs $'/* match the\n   release */ fn f() {}'
    accepted a/src/str.rs 'const S: &str = "match the release";'
    accepted a/src/chr.rs $'fn f(c: char) -> u8 { match c { \'"\' => 1, _ => 2 } }\nconst S: &str = "release";'
    rejected b/src/scrut.rs $'fn g(c: &Chain) -> u8 {\n    match c\n        .release\n    {\n        _ => 1,\n    }\n}'
    rejected b/src/url.rs $'fn g(c: &Chain) -> u8 { let _u = "http://x"; match c.release { _ => 1 } }'
    rejected b/src/cmt.rs $'fn g(r: Release) -> bool { /* v24 */ r == Release::V24_0 }'
    rejected b/src/raw.rs 'fn g(c: &Chain) -> u8 { let s = r#"a"b"#; match c.release { _ => 1 }; let t = "z"; 0 }'
    accepted a/src/rawtxt.rs 'const S: &str = r#"match "the" release"#;'
    accepted a/src/braw.rs 'const B: &[u8] = br#"say "match" on the release"#;'
    accepted a/src/nest.rs $'/* outer /* inner */ match x on the release */\nfn f() {}'
    rejected b/src/nest.rs $'/* a /* b */ c */ fn g(c: &Chain) -> u8 { match c.release { _ => 1 } }'
    echo "check-release-refs selftest: ok"
    exit 0
fi

check "$(cd "$(dirname "$0")/../../.." && pwd)"
echo "release refs ok: no per-release code (DEC-82: v24 only)"

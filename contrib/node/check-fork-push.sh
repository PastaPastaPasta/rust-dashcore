#!/usr/bin/env bash
# The pre-push check for pasta's personal rust-dashcore fork (DEC-650 3a, DEC-670): it checks exactly
# what one push would publish and exits non-zero on any hit. It never pushes.
#
# Usage: contrib/node/check-fork-push.sh snapshot <rev> [--workflow <rev>:<path> | <file>]
#        contrib/node/check-fork-push.sh range <remote-ref>..<rev> --ref <branch>
#        contrib/node/check-fork-push.sh --selftest
#        contrib/node/check-fork-push.sh --install-gitleaks <dir>
#
# snapshot builds the parentless commit of <rev>'s tree with the message
# "ci snapshot of node/integration <full sha>" (DEC-670's history form (c)), its dates set to
# <rev>'s committer date, after rewriting the root Cargo.toml and Cargo.lock's local sources to
# their public URLs (contrib/node/fork-push-sources.txt; the worktree is not touched). It checks
# that one commit (tree, raw commit text, ref name ci/<12-hex short sha>). On PASS only it points
# refs/fork-push/ci/<short> at the commit and writes a private receipt
# (fork-push/receipts/<short>.json); contrib/node/push-fork-snapshot.sh pushes only that. range
# checks every object of the range, its commit messages and <branch>, for a later real-history push
# (DEC-670: not used now).
#
# The nine checks (docs/node-program/designs/CI-FORK.md): 1 private denylist over every blob,
# decoded archive, message and ref name (4 is folded in); 2 content fingerprint against the private
# registry; 3 pointer report (warns only); 5 local paths and machine config; 6 size and data;
# 7 secret scan; 8 private tests skip cleanly; 9 workflow triggers, including the fork's default
# branch (gh api, read-only). Output names a check and a pushed path, never a denylist term or
# private content.
#
# Environment: DASH_EMBARGO_REGISTRY (default ~/private/embargo; not DASH_EMBARGO_DIR, which names
# the private-test tree below it) holds fork-push/denylist.txt (required, 0600 in a 0700 directory),
# and optionally fork-push/fingerprint-scope.txt and fork-push/fingerprint-pins.txt. GITLEAKS is the
# pinned gitleaks binary (checked by hash; default below). CHECK_FORK_PUSH_THREADS (default 8).
# FORK_PUSH_FP_TARGET is fork-push-fp's build directory (default target/fork-push-fp). The
# CHECK_FORK_PUSH_* variables below that replace an input are for the selftest: snapshot and range
# refuse to run with any of them set unless --allow-overrides comes first.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
registry="${DASH_EMBARGO_REGISTRY:-$HOME/private/embargo}"
gitleaks="${GITLEAKS:-/work/scratch/ci-stage0/tools/gitleaks-8.30.1/gitleaks}"
checker="$root/contrib/node/check_fork_push.py"
overrides=(CHECK_FORK_PUSH_FP CHECK_FORK_PUSH_PUBLIC CHECK_FORK_PUSH_POINTERS CHECK_FORK_PUSH_ALLOW
    CHECK_FORK_PUSH_SOURCES CHECK_FORK_PUSH_SECRETS CHECK_FORK_PUSH_FORK_FIXTURE CHECK_FORK_PUSH_MAX_BLOB
    CHECK_FORK_PUSH_GIT_URL_MAP CHECK_FORK_PUSH_WORKFLOW)

die() { echo "check-fork-push: $*" >&2; exit 2; }

refuse_overrides() {
    local name set=()
    for name in "${overrides[@]}"; do [ -z "${!name+x}" ] || set+=("$name"); done
    [ ${#set[@]} -eq 0 ] || die "refusing to run with ${set[*]} set (test overrides; pass --allow-overrides first)"
}

fp_bin() {
    if [ -n "${CHECK_FORK_PUSH_FP:-}" ]; then echo "$CHECK_FORK_PUSH_FP"; return; fi
    local target="${FORK_PUSH_FP_TARGET:-$root/target/fork-push-fp}"
    cargo build --quiet --release --locked --manifest-path "$root/contrib/node/fork-push-fp/Cargo.toml" \
        --target-dir "$target" >&2 || die "building fork-push-fp failed"
    echo "$target/release/fork-push-fp"
}

run_checks() {  # run_checks <repo> <python args...>
    local repo="$1" work fp status=0
    shift
    fp="$(fp_bin)"
    work="$(mktemp -d "${TMPDIR:-/tmp}/check-fork-push.XXXXXX")"
    python3 -I "$checker" check --repo "$repo" --registry "$registry" --work "$work" \
        --fp-bin "$fp" --gitleaks "$gitleaks" --threads "${CHECK_FORK_PUSH_THREADS:-8}" \
        --public "${CHECK_FORK_PUSH_PUBLIC:-$root/contrib/node/fork-push-public.txt}" \
        --pointers "${CHECK_FORK_PUSH_POINTERS:-$root/contrib/node/fork-push-pointers.txt}" \
        --allow "${CHECK_FORK_PUSH_ALLOW:-$root/contrib/node/fork-push-allow.txt}" \
        --sources "${CHECK_FORK_PUSH_SOURCES:-$root/contrib/node/fork-push-sources.txt}" \
        --secrets-baseline "${CHECK_FORK_PUSH_SECRETS:-$root/contrib/node/fork-push-secrets.txt}" \
        ${CHECK_FORK_PUSH_FORK_FIXTURE:+--fork-fixture "$CHECK_FORK_PUSH_FORK_FIXTURE"} \
        ${CHECK_FORK_PUSH_MAX_BLOB:+--max-blob "$CHECK_FORK_PUSH_MAX_BLOB"} "$@" || status=$?
    rm -rf -- "$work"
    return "$status"
}

snapshot() {  # snapshot <rev> [--workflow <rev>:<path> | <file>] [extra python args]
    local rev="$1" repo full tree short commit raw date rewritten workflow status=0
    shift
    workflow="${CHECK_FORK_PUSH_WORKFLOW:-$root/contrib/node/node-gate-minimal.yml}"
    if [ "${1:-}" = --workflow ]; then workflow="${2:?}"; shift 2; fi
    repo="$(git rev-parse --show-toplevel)"
    full="$(git rev-parse --verify "$rev^{commit}")" || die "not a commit: $rev"
    short="$(git rev-parse --short=12 "$full")"
    rewritten="$(mktemp "${TMPDIR:-/tmp}/check-fork-push-rewrite.XXXXXX")"
    local workflow_args=()
    case "$workflow" in
        keep) ;;  # the selftest's check-9 plants keep their own workflows
        *:*) git show "$workflow" >"$rewritten.yml" || die "no workflow at $workflow"
             workflow_args=(--workflow "$rewritten.yml") ;;
        *) [ -f "$workflow" ] || die "no workflow file $workflow"; workflow_args=(--workflow "$workflow") ;;
    esac
    tree="$(python3 -I "$checker" rewrite --repo "$repo" --tree "$full^{tree}" \
        --sources "${CHECK_FORK_PUSH_SOURCES:-$root/contrib/node/fork-push-sources.txt}" \
        "${workflow_args[@]}" 2>"$rewritten")" ||
        { cat "$rewritten" >&2; die "rewriting the sources failed"; }
    rm -f -- "$rewritten.yml"
    cat "$rewritten"
    date="$(git log -1 --format=%ct "$full") +0000"
    commit="$(GIT_AUTHOR_DATE="$date" GIT_COMMITTER_DATE="$date" \
        git commit-tree "$tree" -m "ci snapshot of node/integration $full")"
    # One tree, no parent, the one message: what the push would carry.
    [ "$(git rev-parse "$commit^{tree}")" = "$tree" ] && [ -z "$(git rev-list --parents -n 1 "$commit" | cut -s -d' ' -f2)" ] ||
        die "the snapshot commit is not a parentless commit of the snapshot tree"
    raw="$(mktemp "${TMPDIR:-/tmp}/check-fork-push-commit.XXXXXX")"
    git cat-file commit "$commit" >"$raw"
    git update-ref -d "refs/fork-push/ci/$short" 2>/dev/null || true
    run_checks "$repo" --tree "$commit" --message-file "$raw" --ref "refs/heads/ci/$short" \
        --label "$(date -u +%Y%m%dT%H%M%SZ)-ci-$short" --tip "$full" --commit "$commit" \
        --rewritten "$(sed -n 's/^check-fork-push: rewrote \([^ ]*\) .*/\1/p' "$rewritten" | paste -sd, -)" \
        --receipt "$registry/fork-push/receipts/$short.json" "$@" || status=$?
    rm -f -- "$raw" "$rewritten"
    if [ "$status" -eq 0 ]; then
        git update-ref "refs/fork-push/ci/$short" "$commit"
        echo "check-fork-push: snapshot of $full: commit $commit at refs/fork-push/ci/$short (not pushed; push with contrib/node/push-fork-snapshot.sh $short)"
    else
        rm -f -- "$registry/fork-push/receipts/$short.json"
        echo "check-fork-push: snapshot of $full: not pushable (the check failed)"
    fi
    return "$status"
}

range_mode() {  # range_mode <a>..<b> --ref <branch> [extra python args]
    local range="$1" ref repo tip
    [ "${2:-}" = --ref ] && [ -n "${3:-}" ] || die "usage: $0 range <remote-ref>..<rev> --ref <branch>"
    ref="$3"
    shift 3
    case "$range" in *..*) ;; *) die "not a range: $range" ;; esac
    repo="$(git rev-parse --show-toplevel)"
    tip="$(git rev-parse --verify "${range#*..}^{commit}")" || die "not a commit: ${range#*..}"
    case "$ref" in refs/*) ;; *) ref="refs/heads/$ref" ;; esac
    run_checks "$repo" --tree "$tip" --objects "$range" --messages-range "$range" --ref "$ref" \
        --label "$(date -u +%Y%m%dT%H%M%SZ)-range-$(git rev-parse --short=12 "$tip")" "$@"
}

selftest() {
    local out status
    # Global, so the EXIT trap still sees it.
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-fork-push-selftest.XXXXXX")"
    trap 'chmod -R u+rwX "$tmp" 2>/dev/null; rm -rf -- "$tmp"' EXIT
    st_fail() { echo "check-fork-push selftest: $*" >&2; exit 1; }
    # fork-push-fp's own tests: exact runs at every alignment and file edge, the review's probes.
    cargo test --quiet --release --locked --manifest-path "$root/contrib/node/fork-push-fp/Cargo.toml" \
        --target-dir "${FORK_PUSH_FP_TARGET:-$root/target/fork-push-fp}" >"$tmp/fp-tests.txt" 2>&1 ||
        st_fail "fork-push-fp's tests failed: $(tail -20 "$tmp/fp-tests.txt")"
    # Synthetic registry: nothing here is real private data.
    local reg="$tmp/registry"
    mkdir -m 700 "$reg" "$reg/fork-push" "$reg/repro" "$reg/scoped-out"
    printf '# synthetic\n  # an indented comment\nzebra quartz violet\t\r\nre:plum[- ]?nebula\n' >"$reg/fork-push/denylist.txt"
    printf 'scoped-out/\n' >"$reg/fork-push/fingerprint-scope.txt"
    chmod 600 "$reg/fork-push/denylist.txt" "$reg/fork-push/fingerprint-scope.txt"
    local secret_text public_text pinned_text scoped_text
    secret_text="$(printf 'synthetic private note %s; ' $(seq 1 12))"
    public_text="$(printf 'synthetic public paragraph line %s. ' $(seq 1 6))"
    pinned_text="$(printf 'synthetic reviewed run %s. ' $(seq 1 6))"
    scoped_text="$(printf 'synthetic machine corpus %s. ' $(seq 1 6))"
    printf '%s\n' "$secret_text" >"$reg/notes.md"
    head -c 4096 /dev/urandom >"$reg/repro/input.bin"
    printf 'header\n%s\nfooter\n' "$public_text" >"$reg/public-copy.txt"
    printf '%s\n' "$pinned_text" >"$reg/pinned.txt"
    printf '%s\n' "$scoped_text" >"$reg/scoped-out/corpus.txt"
    printf 'a small private file, 40 bytes long....\n' >"$reg/small.txt"
    # A public reference repository: its tip content is exempt from check 2, and a private file
    # equal to a blob of its history is not read.
    git init -q "$tmp/public"
    git -C "$tmp/public" remote add upstream https://github.com/example/public
    printf 'old public text %s\n' "$(seq 1 40)" >"$tmp/public/old.txt"
    git -C "$tmp/public" add old.txt
    git -C "$tmp/public" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q -m old
    cp "$tmp/public/old.txt" "$reg/public-history-copy.txt"
    git -C "$tmp/public" rm -q old.txt
    printf 'intro\n%s\n' "$public_text" >"$tmp/public/doc.txt"
    git -C "$tmp/public" add doc.txt
    git -C "$tmp/public" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q -m public
    git -C "$tmp/public" update-ref refs/remotes/upstream/main HEAD
    printf '%s refs/remotes/upstream/\n' "$tmp/public" >"$tmp/public-spec.txt"
    # The fork's default branch, as gh api would report it.
    fork_fixture() {  # fork_fixture <state of the scheduled workflow> [extra trigger of rust.yml]
        python3 -I - "$tmp/fork.json" "$1" "${2:-}" <<'PY'
import json, sys
extra = f"  {sys.argv[3]}:\n" if sys.argv[3] else ""
json.dump({"default_branch": "master", "workflows": {
    ".github/workflows/rust.yml": {"content": "on:\n  push:\n    branches: [master]\n" + extra, "state": "active"},
    ".github/workflows/fuzz.yml": {"content": "on:\n  schedule:\n    - cron: '0 0 * * *'\n", "state": sys.argv[2]}}},
    open(sys.argv[1], "w"))
PY
    }
    fork_fixture disabled_manually
    printf 'docs/pointer.md\ncontrib/node/private-tests.txt\n' >"$tmp/pointers.txt"
    printf 'contrib/allowed-local.sh 5 local-tool\ncontrib/literal.rs 5 inert-literal chains=1\n' >"$tmp/allow.txt"
    : >"$tmp/sources.txt"
    : >"$tmp/secrets.txt"
    export DASH_EMBARGO_REGISTRY="$reg" CHECK_FORK_PUSH_PUBLIC="$tmp/public-spec.txt" \
        CHECK_FORK_PUSH_POINTERS="$tmp/pointers.txt" CHECK_FORK_PUSH_ALLOW="$tmp/allow.txt" \
        CHECK_FORK_PUSH_SOURCES="$tmp/sources.txt" CHECK_FORK_PUSH_SECRETS="$tmp/secrets.txt" \
        CHECK_FORK_PUSH_FORK_FIXTURE="$tmp/fork.json" CHECK_FORK_PUSH_MAX_BLOB=65536 \
        CHECK_FORK_PUSH_WORKFLOW=keep
    registry="$reg"
    CHECK_FORK_PUSH_FP="$(fp_bin)"
    export CHECK_FORK_PUSH_FP

    # A clean tree.
    local repo="$tmp/repo"
    git init -q "$repo"
    cd "$repo"
    git config user.name t; git config user.email t@t; git config commit.gpgsign false
    mkdir -p src docs contrib .github/workflows dash-consensus/tests contrib/node
    printf '# demo\n' >README.md
    printf '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nx = { git = "https://github.com/example/x", rev = "abc" }\ny = { path = "src/y" }\n' >Cargo.toml
    printf 'pub fn f() {}\n' >src/lib.rs
    printf 'See /work/chains/mainnet for the local data.\n' >docs/local.md
    printf 'data=/work/chains/mainnet\n' >contrib/allowed-local.sh
    printf 'const GUARD: &str = "/work/chains";\n' >contrib/literal.rs
    printf 'Refers to EMBARGO-1 by name only.\n' >docs/pointer.md
    printf 'on:\n  push:\n    branches: [main, dev]\n  pull_request:\n' >.github/workflows/rust.yml
    printf 'on:\n  push:\n    branches: ["ci/**"]\n' >.github/workflows/node-gate.yml
    printf '#[test]\nfn quirk_01_embargo_1() {\n    let Some(root) = private_tree(TEST) else {\n        return;\n    };\n}\n' \
        >dash-consensus/tests/quirk_01_embargo_1.rs
    printf '# synthetic\ndash-consensus/tests/quirk_01_embargo_1.rs quirk_01_embargo_1\n' >contrib/node/private-tests.txt
    printf 'case "$1" in --expect-skipped) ;; esac\n' >contrib/node/check-private-tests.sh
    git add -- README.md Cargo.toml src docs contrib .github dash-consensus && git commit -q -m clean
    local clean
    clean="$(git rev-parse HEAD)"

    check() { "$root/contrib/node/check-fork-push.sh" --allow-overrides "$@"; }
    expect_pass() {  # expect_pass <what> [mode args...]
        local what="$1"; shift
        status=0; out="$(check "$@" 2>&1)" || status=$?
        [ "$status" -eq 0 ] && grep -q '^check-fork-push: PASS' <<<"$out" || st_fail "$what did not pass ($status): $out"
    }
    expect_fail() {  # expect_fail <what> <check> <path or label> [mode args...]
        local what="$1" check="$2" label="$3"; shift 3
        status=0; out="$(check "$@" 2>&1)" || status=$?
        [ "$status" -eq 1 ] && grep -qF "check-fork-push: check $check FAIL $label" <<<"$out" ||
            st_fail "$what: check $check did not fail on $label ($status): $out"
    }
    expect_exit2() {  # expect_exit2 <what> <message> [mode args...]
        local what="$1" message="$2"; shift 2
        status=0; out="$(check "$@" 2>&1)" || status=$?
        [ "$status" -eq 2 ] && grep -qF "$message" <<<"$out" || st_fail "$what was not refused ($status): $out"
    }
    plant() {  # plant <path> <content via stdin>: commit it on a branch off the clean tree
        git checkout -q --detach "$clean"
        mkdir -p "$(dirname "$1")"
        cat >"$1"
        git add -- "$1" && git commit -q -m "plant $1"
    }

    expect_pass "the clean tree" snapshot "$clean"
    local short
    short="$(git rev-parse --short=12 "$clean")"
    grep -qF "at refs/fork-push/ci/$short" <<<"$out" || st_fail "no snapshot ref reported: $out"
    [ -f "$reg/fork-push/receipts/$short.json" ] || st_fail "no receipt was written"
    grep -q '^check-fork-push: check 3 WARN' <<<"$out" && st_fail "the pinned pointer file was reported as new"
    grep -q 'check 9 fork .* fuzz.yml=schedule/disabled_manually' <<<"$out" || st_fail "check 9 did not record the fork state: $out"
    grep -q 'check 2 .* 1 skipped as public blobs' <<<"$out" || st_fail "a private copy of a public blob was read: $out"
    grep -q 'check 2 scope line 1: 1 files not read' <<<"$out" || st_fail "the scope count was not reported: $out"
    local first
    first="$(git rev-parse "refs/fork-push/ci/$short")"
    expect_pass "the clean tree, twice" snapshot "$clean"
    [ "$(git rev-parse "refs/fork-push/ci/$short")" = "$first" ] || st_fail "the snapshot commit is not reproducible"

    # Test overrides need --allow-overrides.
    status=0; out="$("$root/contrib/node/check-fork-push.sh" snapshot "$clean" 2>&1)" || status=$?
    [ "$status" -eq 2 ] && grep -q 'refusing to run with CHECK_FORK_PUSH_FP' <<<"$out" || st_fail "an override was honoured without --allow-overrides: $out"

    # The push step: only the checked commit, re-verified, by SHA, never forced.
    git init -q --bare "$tmp/fork.git"
    local push="$root/contrib/node/push-fork-snapshot.sh"
    status=0; out="$("$push" --allow-overrides --remote "$tmp/fork.git" "$short" 2>&1)" || status=$?
    [ "$status" -eq 0 ] && [ "$(git -C "$tmp/fork.git" rev-parse "refs/heads/ci/$short")" = "$first" ] ||
        st_fail "the push step did not push the checked commit ($status): $out"
    status=0; out="$("$push" --allow-overrides --remote "$tmp/fork.git" "$short" 2>&1)" || status=$?
    [ "$status" -ne 0 ] || st_fail "the push step overwrote an existing branch: $out"
    status=0; out="$("$push" --allow-overrides --remote "$tmp/fork.git" --delete "$short" 2>&1)" || status=$?
    [ "$status" -eq 0 ] && ! git -C "$tmp/fork.git" rev-parse -q --verify "refs/heads/ci/$short" >/dev/null &&
        ! git rev-parse -q --verify "refs/fork-push/ci/$short" >/dev/null || st_fail "--delete did not clean up ($status): $out"
    expect_pass "the clean tree, for the receipt checks" snapshot "$clean"
    python3 -I -c 'import json,sys; p=sys.argv[1]; r=json.load(open(p)); r["tree"]="0"*40; json.dump(r,open(p,"w"))' \
        "$reg/fork-push/receipts/$short.json"
    status=0; out="$("$push" --allow-overrides --remote "$tmp/fork.git" "$short" 2>&1)" || status=$?
    [ "$status" -eq 2 ] && grep -q 'does not match its receipt' <<<"$out" || st_fail "a commit that disagrees with its receipt was pushed: $out"
    printf 'zebra quartz violet\n' | plant notes/fail.txt
    expect_fail "a failing snapshot" 1 notes/fail.txt snapshot HEAD
    grep -q 'not pushable' <<<"$out" || st_fail "a failing snapshot was reported pushable: $out"
    git rev-parse -q --verify "refs/fork-push/ci/$(git rev-parse --short=12 HEAD)" >/dev/null &&
        st_fail "a failing snapshot left a pushable ref"

    # 1: denylist in a blob (case, line breaks, comment leaders, Unicode spacing and split literals
    # do not matter), in compressed members (gzip, nested gzip, zstd, xz), in a message, in a ref name.
    printf 'Notes: ZEBRA  Quartz\nviolet.\n' | plant notes/plan.txt
    expect_fail "a denylist term in a blob" 1 notes/plan.txt snapshot HEAD
    printf '/// The zebra quartz\n/// violet case.\nfn f() {}\n' | plant src/wrapped.rs
    expect_fail "a term wrapped in a /// comment" 1 src/wrapped.rs snapshot HEAD
    printf 'zebra\xc2\xa0quartz\xe2\x80\x8bviolet\n' | plant docs/unicode.md
    expect_fail "a term joined by NBSP and ZWSP" 1 docs/unicode.md snapshot HEAD
    printf 'let s = concat!("zebra quartz ", "violet");\n' | plant src/split.rs
    expect_fail "a term split across string literals" 1 src/split.rs snapshot HEAD
    printf 'zebra quartz violet\n' | gzip -c | plant data/fixture.json.gz
    expect_fail "a denylist term in a gzip blob" 1 "data/fixture.json.gz (decompressed)" snapshot HEAD
    printf 'zebra quartz violet\n' | gzip -c | gzip -c | plant data/nested.gz
    expect_fail "a denylist term in a nested gzip blob" 1 "data/nested.gz (decompressed)" snapshot HEAD
    { printf 'zebra quartz violet %.0s' $(seq 1 50); } | zstd -q -c | plant data/vectors.jsonl.zst
    expect_fail "a denylist term in a zstd blob" 1 "data/vectors.jsonl.zst (decompressed)" snapshot HEAD
    { printf 'zebra quartz violet %.0s' $(seq 1 50); } | xz -c | plant data/vectors.xz
    expect_fail "a denylist term in an xz blob" 1 "data/vectors.xz (decompressed)" snapshot HEAD
    { printf 'harmless %.0s' $(seq 1 50); } | gzip -c | head -c 25 | plant data/truncated.gz
    expect_fail "a truncated gzip blob" 1 "data/truncated.gz: cannot be fully decoded (gzip: truncated)" snapshot HEAD
    { { printf 'harmless %.0s' $(seq 1 50); } | gzip -c; printf 'TRAILING'; } | plant data/trailing.gz
    expect_fail "a gzip blob with trailing bytes" 1 "data/trailing.gz: cannot be fully decoded (gzip: trailing bytes" snapshot HEAD
    git checkout -q --detach "$clean"
    git commit -q --allow-empty -m "fix the plum nebula"
    expect_fail "a denylist term in a commit message" 1 "commit message" range "$clean..HEAD" --ref ci/test
    expect_fail "a denylist term in a ref name" 1 "ref name" range "$clean..$clean" --ref plum-nebula-1
    # 2: a 64-byte run of a private file; not when the run is public, out of scope or pinned; a
    # small file's copy; a run inside a zstd member; an unreadable private file fails closed.
    { head -c 100 /dev/zero; dd if="$reg/repro/input.bin" bs=1 skip=1000 count=70 2>/dev/null; } | plant tests/data/x.bin
    expect_fail "a run of a private binary file" 2 tests/data/x.bin snapshot HEAD
    printf 'quote: %s\n' "${secret_text:20:80}" | plant docs/quote.md
    expect_fail "a run of a private note" 2 docs/quote.md snapshot HEAD
    printf 'quote: %s\n' "${secret_text:20:80}" | zstd -q -c | plant tests/data/quote.zst
    expect_fail "a run of a private note inside a zstd blob" 2 "tests/data/quote.zst (decompressed)" snapshot HEAD
    printf 'copied: %s\n' "$public_text" | plant docs/public.md
    expect_pass "a run that is also public" snapshot HEAD
    printf 'copied: %s\n' "$scoped_text" | plant docs/scoped.md
    expect_pass "a run of a file out of the scope" snapshot HEAD
    plant tests/small.txt <"$reg/small.txt"
    expect_fail "a copy of a small private file" 2 tests/small.txt snapshot HEAD
    printf 'reviewed: %s\n' "$pinned_text" | plant docs/pinned.md
    expect_fail "a run before it is pinned" 2 docs/pinned.md snapshot HEAD --write-pins "$tmp/pins.txt"
    install -m 600 "$tmp/pins.txt" "$reg/fork-push/fingerprint-pins.txt"
    expect_pass "a pinned run" snapshot HEAD
    rm "$reg/fork-push/fingerprint-pins.txt"
    chmod 000 "$reg/repro/input.bin"
    expect_fail "an unreadable private file" 2 "fork-push-fp exited 2" snapshot "$clean"
    chmod 600 "$reg/repro/input.bin"
    # 3: a new pointer file warns and does not fail.
    printf 'see CANDIDATE-X\n' | plant docs/new-pointer.md
    expect_pass "a new pointer file" snapshot HEAD
    grep -qF 'check 3 WARN new pointer file: docs/new-pointer.md' <<<"$out" || st_fail "the new pointer file was not reported: $out"
    # 5: local paths and machine config; allowlist guards.
    printf '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nx = { git = "file:///work/x", rev = "abc" }\n' | plant Cargo.toml
    expect_fail "a file:// git dependency" 5 "Cargo.toml: [dependencies] x" snapshot HEAD
    printf '[package]\nname = "demo"\nversion = "0.1.0"\n\n[patch.crates-io]\nz = { path = "/abs/z" }\n' | plant Cargo.toml
    expect_fail "an absolute [patch] path" 5 "Cargo.toml: [patch.crates-io] z" snapshot HEAD
    printf '[source.crates-io]\nreplace-with = "vendored"\n' | plant .cargo/config.toml
    expect_fail "a source replacement" 5 ".cargo/config.toml" snapshot HEAD
    printf 'cp -r ~/private/x .\n' | plant contrib/run.sh
    expect_fail "a private path in a script" 5 contrib/run.sh snapshot HEAD
    printf 'const GUARD: &str = "/work/chains";\nconst MORE: &str = "/work/chains/x";\n' | plant contrib/literal.rs
    expect_fail "an allowlisted file that names one more local path" 5 "contrib/literal.rs: allowlist line 2 (inert-literal): its local path count changed" snapshot HEAD
    printf 'jobs:\n  x:\n    steps:\n      - run: contrib/allowed-local.sh\n' | plant .github/workflows/local.yml
    expect_fail "a local-tool entry that CI runs" 5 "contrib/allowed-local.sh: allowlist line 1 (local-tool): named by a CI file" snapshot HEAD
    # 5: a local source rewritten for the snapshot to a published fork tag (DEC-680 option (a)).
    git init -q "$tmp/base"
    printf '[package]\nname = "base"\nversion = "0.1.0"\nedition = "2021"\n' >"$tmp/base/Cargo.toml"
    mkdir "$tmp/base/src" && printf 'pub fn b() {}\n' >"$tmp/base/src/lib.rs"
    git -C "$tmp/base" add . && git -C "$tmp/base" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q -m base
    local base_rev
    base_rev="$(git -C "$tmp/base" rev-parse HEAD)"
    git -C "$tmp/base" update-ref refs/tags/node-base-1 HEAD  # lightweight, as tag.gpgSign may be set
    printf 'file://%s https://github.com/example/base refs/tags/node-base-*\n' "$tmp/base" >"$tmp/sources.txt"
    export CHECK_FORK_PUSH_GIT_URL_MAP="https://github.com/example/base=$tmp/base" CARGO_HOME="$tmp/cargo-home"
    git checkout -q --detach "$clean"
    printf '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nbase = { git = "file://%s", rev = "%s" }\n' \
        "$tmp/base" "$base_rev" >Cargo.toml
    cargo generate-lockfile --quiet 2>"$tmp/lock.err" || st_fail "cargo generate-lockfile failed: $(cat "$tmp/lock.err")"
    git add Cargo.toml Cargo.lock && git commit -q -m "a local source"
    expect_pass "a local source whose rev is a published fork tag" snapshot HEAD
    grep -q 'rewrote Cargo.toml' <<<"$out" && grep -q 'rewrote Cargo.lock' <<<"$out" || st_fail "the sources were not rewritten: $out"
    # Nested workspaces are rewritten too, and a local source left in a form the rewrite does not
    # know refuses the snapshot.
    local with_source
    with_source="$(git rev-parse HEAD)"
    mkdir -p nested/src && printf 'pub fn n() {}\n' >nested/src/lib.rs
    { sed 's/^name = "demo"/name = "nested"/' Cargo.toml; printf '\n[workspace]\n'; } >nested/Cargo.toml
    sed 's/^name = "demo"/name = "nested"/' Cargo.lock >nested/Cargo.lock
    git add nested && git commit -q -m "a nested workspace with a local source"
    expect_pass "a nested workspace's local source" snapshot HEAD
    grep -q 'rewrote nested/Cargo.toml' <<<"$out" && grep -q 'rewrote nested/Cargo.lock' <<<"$out" ||
        st_fail "the nested workspace was not rewritten: $out"
    git grep -q 'file://' "refs/fork-push/ci/$(git rev-parse --short=12 HEAD)" -- nested &&
        st_fail "a local source was left in the nested workspace"
    mkdir -p odd && printf '[package]\nname = "odd"\nversion = "0.1.0"\n\n[dependencies]\nbase = { git = "file://%s/", rev = "%s" }\n' \
        "$tmp/base" "$base_rev" >odd/Cargo.toml
    git add odd && git commit -q -m "a local source in another form"
    expect_exit2 "a local source the rewrite does not know" 'odd/Cargo.toml: a local source is left' snapshot HEAD
    git checkout -q --detach "$with_source"
    git -C "$tmp/base" update-ref -d refs/tags/node-base-1
    expect_fail "a fork rev with no published tag" 5 "Cargo.toml: https://github.com/example/base rev ${base_rev:0:12} is not exactly a published" snapshot HEAD
    git -C "$tmp/base" update-ref refs/tags/node-base-1 "$base_rev"
    git -C "$tmp/base" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q --allow-empty -m later
    sed -i "s/$base_rev/$(git -C "$tmp/base" rev-parse HEAD)/g" Cargo.lock
    git add Cargo.lock && git commit -q -m "a lock that disagrees with the manifest"
    expect_fail "a rewritten lock that does not resolve unchanged" 5 "Cargo.lock: the rewritten lock does not resolve unchanged" snapshot HEAD
    git checkout -q --detach "$clean"
    : >"$tmp/sources.txt"
    unset CHECK_FORK_PUSH_GIT_URL_MAP CARGO_HOME
    # 6: size and data; an allowlisted fixture, then a changed one.
    head -c 70000 /dev/urandom | plant data/big.bin
    expect_fail "a blob over the size limit" 6 data/big.bin snapshot HEAD
    printf 'x\n' | plant datadir/chainstate/000005.ldb
    expect_fail "a datadir file name" 6 datadir/chainstate/000005.ldb snapshot HEAD
    printf 'SQLite format 3\0rest' | plant data/state.bin
    expect_fail "an SQLite database" 6 data/state.bin snapshot HEAD
    python3 -I -c 'import sys; rec = bytes.fromhex("bf0c6bbd") + (81).to_bytes(4, "little") + bytes(81); sys.stdout.buffer.write(rec * 2)' |
        plant data/blocks.bin
    expect_fail "a raw block file" 6 data/blocks.bin snapshot HEAD
    printf 'fixture\n' | plant tests/data/peers.dat
    printf 'tests/data/peers.dat 6 synthetic-data blob=%s\n' "$(git rev-parse HEAD:tests/data/peers.dat)" >>"$tmp/allow.txt"
    expect_pass "an allowlisted fixture" snapshot HEAD
    printf 'fixture, changed\n' | plant tests/data/peers.dat
    expect_fail "a changed allowlisted fixture" 6 "tests/data/peers.dat: allowlist line 3 (synthetic-data): the file changed" snapshot HEAD
    # 7: a secret (built here, so this file holds none), then accepted once it is in the baseline;
    # the pushed tree cannot switch the scan off or hide a value in a skipped path.
    local token
    token="ghp_$(head -c 64 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 36)"
    printf 'token = "%s"\n' "$token" | plant src/config.py
    expect_fail "a GitHub token" 7 "src/config.py: github-pat" snapshot HEAD --write-secrets-baseline "$tmp/secrets.txt"
    expect_pass "a baselined value" snapshot HEAD
    : >"$tmp/secrets.txt"
    printf '[extend]\nuseDefault = true\n[allowlist]\npaths = ['"'''"'.*'"'''"']\n' >.gitleaks.toml
    printf '*\n' >.gitleaksignore
    git add .gitleaks.toml .gitleaksignore && git commit -q -m "a gitleaks config"
    GITLEAKS_CONFIG="$PWD/.gitleaks.toml" expect_fail "a pushed gitleaks config and ignore file" 7 "src/config.py: github-pat" snapshot HEAD
    printf 'token = "%s" # gitleaks:allow\n' "$token" | plant src/inline.py
    expect_fail "an inline gitleaks:allow" 7 "src/inline.py: github-pat" snapshot HEAD
    printf 'token = "%s"\n' "$token" | plant tests/data/blob.bin
    expect_fail "a token in a path gitleaks skips" 7 "tests/data/blob.bin: github-pat" snapshot HEAD
    # 8: private tests.
    printf 'on:\n  pull_request:\nenv:\n  DASH_EMBARGO_DIR: /x\n' | plant .github/workflows/env.yml
    expect_fail "a workflow that sets DASH_EMBARGO_DIR" 8 .github/workflows/env.yml snapshot HEAD
    printf '#[test]\nfn quirk_01_embargo_1() {\n    let root = std::env::var("DASH_EMBARGO_DIR").unwrap();\n}\n' |
        plant dash-consensus/tests/quirk_01_embargo_1.rs
    expect_fail "a private test without the skip guard" 8 dash-consensus/tests/quirk_01_embargo_1.rs snapshot HEAD
    git checkout -q --detach "$clean"
    git mv dash-consensus/tests/quirk_01_embargo_1.rs dash-consensus/tests/renamed.rs && git commit -q -m rename
    expect_fail "a moved private test" 8 "dash-consensus/tests/quirk_01_embargo_1.rs: listed in" snapshot HEAD
    expect_fail "an unlisted private test" 8 "dash-consensus/tests/renamed.rs: reaches the private tree" snapshot HEAD
    # 9: workflow triggers in the pushed tree and on the fork.
    printf 'on: push\n' | plant .github/workflows/any.yml
    expect_fail "a workflow on every push" 9 ".github/workflows/any.yml: fires on push" snapshot HEAD
    # The real export replaces .github/workflows/ with node-gate.yml alone.
    status=0; out="$(CHECK_FORK_PUSH_WORKFLOW="$root/contrib/node/node-gate-minimal.yml" check snapshot HEAD 2>&1)" || status=$?
    [ "$status" -eq 0 ] && grep -q 'rewrote .github/workflows/node-gate.yml (the only workflow)' <<<"$out" ||
        st_fail "the workflow export did not pass: $out"
    [ "$(git ls-tree -r --name-only "refs/fork-push/ci/$(git rev-parse --short=12 HEAD)" -- .github/workflows)" = \
        .github/workflows/node-gate.yml ] || st_fail "the snapshot kept other workflows"
    status=0; out="$(CHECK_FORK_PUSH_WORKFLOW=HEAD:.github/workflows/node-gate.yml check snapshot HEAD 2>&1)" || status=$?
    [ "$status" -eq 0 ] || st_fail "a workflow taken from <rev>:<path> did not pass: $out"
    printf 'on:\n  push:\n    branches: ["ci/**"]\n' | plant .github/workflows/other.yml
    expect_fail "a workflow on ci/* that is not node-gate.yml" 9 ".github/workflows/other.yml: fires on push" snapshot HEAD
    printf 'on:\n  push:\n    branches: ["cix?/**"]\n' | plant .github/workflows/optional.yml
    expect_fail "a filter whose ? makes a character optional" 9 ".github/workflows/optional.yml: fires on push" snapshot HEAD
    printf 'on:\n  push:\n    branches: ["**", "!ci/**"]\n' | plant .github/workflows/negated.yml
    expect_pass "a workflow that excludes ci/*" snapshot HEAD
    fork_fixture active
    expect_fail "an active scheduled workflow on the fork" 9 "PastaPastaPasta/rust-dashcore@master:.github/workflows/fuzz.yml: active schedule trigger" snapshot "$clean"
    fork_fixture disabled_manually delete
    expect_fail "an active delete workflow on the fork" 9 "PastaPastaPasta/rust-dashcore@master:.github/workflows/rust.yml: active delete trigger" snapshot "$clean"
    fork_fixture disabled_manually
    # 9: the live query fails closed (a stand-in gh: the repository answers, the contents call fails,
    # or a workflow Actions lists as active is missing from the contents).
    mkdir "$tmp/gh-bin"
    cat >"$tmp/gh-bin/gh" <<'SH'
#!/usr/bin/env bash
case "$2" in
    repos/*/contents/.github/workflows\?*) [ "$GH_CASE" = contents-fail ] && exit 1; echo '[]' ;;
    repos/*/actions/workflows*) echo '{"workflows": [{"path": ".github/workflows/fuzz.yml", "state": "active"}]}' ;;
    repos/*) echo '{"default_branch": "master"}' ;;
esac
SH
    chmod +x "$tmp/gh-bin/gh"
    for case in contents-fail absent; do
        status=0; out="$(env -u CHECK_FORK_PUSH_FORK_FIXTURE PATH="$tmp/gh-bin:$PATH" GH_CASE=$case \
            "$root/contrib/node/check-fork-push.sh" --allow-overrides snapshot "$clean" 2>&1)" || status=$?
        [ "$status" -eq 1 ] && grep -q 'check 9 FAIL PastaPastaPasta/rust-dashcore: ' <<<"$out" ||
            st_fail "check 9 passed when the fork query was $case: $out"
    done

    # The denylist is required and private.
    chmod 644 "$reg/fork-push/denylist.txt"
    expect_exit2 "a world-readable denylist" 'readable by others' snapshot "$clean"
    chmod 600 "$reg/fork-push/denylist.txt"
    printf '# only a comment\n' >"$reg/fork-push/denylist.txt"
    expect_exit2 "an empty denylist" 'denylist is empty' snapshot "$clean"
    mv "$reg/fork-push/denylist.txt" "$tmp/denylist.away"
    expect_exit2 "a missing denylist" 'denylist is missing' snapshot "$clean"
    mv "$tmp/denylist.away" "$reg/fork-push/denylist.txt"
    printf '# synthetic\nzebra quartz violet\nre:plum[- ]?nebula\n' >"$reg/fork-push/denylist.txt"

    # Without the pinned gitleaks the check fails closed.
    status=0; out="$(GITLEAKS="$tmp/absent" check snapshot "$clean" 2>&1)" || status=$?
    [ "$status" -eq 1 ] && grep -q 'check 7 FAIL gitleaks' <<<"$out" || st_fail "a missing gitleaks did not fail: $out"
    printf '#!/bin/sh\necho "{}"\n' >"$tmp/fake-gitleaks" && chmod +x "$tmp/fake-gitleaks"
    status=0; out="$(GITLEAKS="$tmp/fake-gitleaks" check snapshot "$clean" 2>&1)" || status=$?
    [ "$status" -eq 1 ] && grep -q 'check 7 FAIL gitleaks at .* is not the pinned' <<<"$out" || st_fail "a gitleaks of the wrong build was used: $out"

    expect_pass "the clean tree, again" snapshot "$clean"
    echo "check-fork-push selftest: ok"
}

allow_overrides=0
if [ "${1:-}" = --allow-overrides ]; then allow_overrides=1; shift; fi
case "${1:-}" in
    snapshot) [ $# -ge 2 ] || die "usage: $0 snapshot <rev>"; shift
        [ "$allow_overrides" -eq 1 ] || refuse_overrides; snapshot "$@" ;;
    range) [ $# -ge 2 ] || die "usage: $0 range <remote-ref>..<rev> --ref <branch>"; shift
        [ "$allow_overrides" -eq 1 ] || refuse_overrides; range_mode "$@" ;;
    --selftest) selftest ;;
    --install-gitleaks) [ $# -eq 2 ] || die "usage: $0 --install-gitleaks <dir>"; python3 -I "$checker" install-gitleaks "$2" ;;
    *) die "usage: $0 [--allow-overrides] snapshot <rev> | range <remote-ref>..<rev> --ref <branch> | --selftest | --install-gitleaks <dir>" ;;
esac

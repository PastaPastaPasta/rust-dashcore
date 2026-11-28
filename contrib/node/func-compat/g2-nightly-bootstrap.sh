#!/usr/bin/env bash
# The G2 nightly's entry point (CH5), installed outside any checkout (see g2-nightly.sh). It moves
# the nightly's own harness worktree, G2_NIGHTLY_DIR/harness (detached), to G2_NIGHTLY_HARNESS
# and execs that worktree's g2-nightly.sh, so the nightly never runs a live checkout's files.
#
# env (besides g2-nightly.sh's):
#   G2_NIGHTLY_REPO     the repository the refs resolve in; default ~/workspace/rust-dashcore
#   G2_NIGHTLY_HARNESS  the ref whose g2-at, func-compat.py and g2-llmq.json run; default
#                       G2_NIGHTLY_REF (node/integration)
#
# Exit status: g2-nightly.sh's; 2, with one summary line, when the harness ref does not resolve
# or has no g2-at, or the harness worktree has uncommitted changes or cannot be moved.
set -u

REPO=${G2_NIGHTLY_REPO:-$HOME/workspace/rust-dashcore}
REF=${G2_NIGHTLY_REF:-node/integration}
HARNESS=${G2_NIGHTLY_HARNESS:-$REF}
DIR=${G2_NIGHTLY_DIR:-/work/scratch/g2-nightly}
TREE=$DIR/harness
FC=contrib/node/func-compat

no_result() {
  local line="g2-nightly: $REF: no result: $1"
  echo "$line"
  mkdir -p "$DIR" && echo "$(date -u +%FT%TZ) $line" >> "$DIR/summary.log"
  exit 2
}

sha=$(git -C "$REPO" rev-parse --verify --quiet "$HARNESS^{commit}") \
  || no_result "harness $HARNESS does not resolve in $REPO"
for f in g2-at g2-nightly.sh; do
  git -C "$REPO" cat-file -e "$sha:$FC/$f" 2>/dev/null || no_result "harness $HARNESS has no $f (${sha:0:12})"
done

if [ -e "$TREE" ]; then
  common=$(git -C "$TREE" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
  top=$(git -C "$TREE" rev-parse --show-toplevel 2>/dev/null)
  if [ "$common" != "$(git -C "$REPO" rev-parse --path-format=absolute --git-common-dir)" ] \
      || [ "$top" != "$(cd "$TREE" && pwd -P)" ]; then
    no_result "$TREE is not a worktree of $REPO"
  fi
  [ -z "$(git -C "$TREE" status --porcelain)" ] \
    || no_result "the harness worktree $TREE has uncommitted changes; not moved to $HARNESS (${sha:0:12})"
  git -C "$TREE" checkout -q --detach "$sha" 2>/dev/null \
    || no_result "the harness worktree $TREE could not move to $HARNESS (${sha:0:12})"
elif ! { mkdir -p "$DIR" && git -C "$REPO" worktree add -q -f --detach "$TREE" "$sha" 2>/dev/null; }; then
  no_result "could not add the harness worktree $TREE at $HARNESS (${sha:0:12})"
fi
exec /bin/bash "$TREE/$FC/g2-nightly.sh"

#!/usr/bin/env bash
# ST3b S7 r2 N1: st3b-priv's `kernel` verb refuses what it must, with exit 2, its message, and
# nothing written: no <dest>, and no `.tmp.` copy left under $ROOT/vm. Runs the verb through
# `sudo -n` (as the harness does), plus once without root. It never copies a kernel: the copying
# path is run by hand (`sudo -n st3b-priv kernel <new dest under $ROOT/vm>`), and its file removed.
#
#   contrib/node/crash/st3b-priv-tests.sh
set -euo pipefail
umask 077

ROOT=/work/scratch/st3b
VM=$ROOT/vm
priv=$(realpath -e -- "$(dirname -- "$0")/st3b-priv")
fail=0
links=()
# shellcheck disable=SC2329 # run by the EXIT trap.
cleanup() {
	local l
	for l in "${links[@]}"; do rm -f -- "$l"; done
}
trap cleanup EXIT

tmps() { find "$VM" -maxdepth 2 -name '*.tmp.*' | sort; }
before=$(tmps)

# expect <label> <message part> <command...>: exit 2, the message on stderr, nothing written.
expect() {
	local label=$1 want=$2 err rc=0
	shift 2
	err=$("$@" 2>&1 >/dev/null) || rc=$?
	if [ "$rc" != 2 ] || [[ $err != *"$want"* ]]; then
		echo "FAIL $label: exit $rc, stderr: $err (want exit 2 and \"$want\")"
		fail=1
		return
	fi
	echo "ok   $label: $err"
}

# shellcheck disable=SC2329 # run through expect.
k() { sudo -n "$priv" kernel "$@"; }

[ -d "$VM" ] || {
	echo "no $VM"
	exit 1
}
# Only paths that cannot exist, or that must not be touched (an existing file).
new=$VM/st3b-priv-test-$$
[ ! -e "$new" ] || {
	echo "$new exists"
	exit 1
}
existing=$(find "$VM" -maxdepth 1 -type f -print -quit)
[ -n "$existing" ] || {
	echo "no file under $VM to refuse overwriting"
	exit 1
}
existing_sum=$(sha256sum <"$existing")
# A directory link under vm/ that resolves outside it, and a dangling link as <dest>.
out_link=$VM/st3b-priv-test-out-$$
dangling=$VM/st3b-priv-test-dangling-$$
ln -s -- "$ROOT/run" "$out_link" && links+=("$out_link")
ln -s -- "$VM/st3b-priv-test-nothing-$$" "$dangling" && links+=("$dangling")

expect "not root" "run me with sudo -n" "$priv" kernel "$new"
expect "no argument" "kernel <dest>" k
expect "two arguments" "kernel <dest>" k "$new" "$new.2"
expect "empty name" "bad file name" k "$VM/"
expect "dot name" "bad file name" k "$VM/.hidden"
expect "name with a space" "bad file name" k "$VM/a b"
expect "name with a newline" "bad file name" k "$VM/a"$'\n'"b"
expect "name too long" "bad file name" k "$VM/$(printf 'a%.0s' {1..101})"
expect "no directory" "no directory for" k "$VM/st3b-priv-test-none-$$/x"
expect "outside the root" "not under $VM" k /etc/st3b-priv-test
expect "beside vm/" "not under $VM" k "$ROOT/tmp/st3b-priv-test"
expect "dot-dot out of vm/" "not under $VM" k "$VM/../run/st3b-priv-test"
expect "vm/ itself" "not under $VM" k "$ROOT/vm"
expect "a link out of vm/" "not under $VM" k "$out_link/st3b-priv-test"
expect "dest exists" "already there" k "$existing"
expect "dest a dangling link" "already there" k "$dangling"
expect "no SUDO_UID" "no SUDO_UID/SUDO_GID" sudo -n env -u SUDO_UID "$priv" kernel "$new"
expect "bad SUDO_GID" "no SUDO_UID/SUDO_GID" sudo -n env SUDO_GID=x "$priv" kernel "$new"
expect "root's sudo" "not root's" sudo -n env SUDO_UID=0 SUDO_GID=0 "$priv" kernel "$new"

# Nothing written.
if [ -e "$new" ] || [ -L "$new" ]; then
	echo "FAIL: $new was created"
	fail=1
fi
if [ "$(sha256sum <"$existing")" != "$existing_sum" ]; then
	echo "FAIL: $existing changed"
	fail=1
fi
if [ "$(tmps)" != "$before" ]; then
	echo "FAIL: a .tmp. copy was left under $VM:"
	comm -13 <(echo "$before") <(tmps)
	fail=1
fi
[ "$fail" = 0 ] && echo "st3b-priv kernel refusals: all ok"
exit "$fail"

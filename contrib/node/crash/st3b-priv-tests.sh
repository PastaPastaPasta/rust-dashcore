#!/usr/bin/env bash
# ST3b S7 r2 N1: st3b-priv's `kernel` verb refuses what it must, with exit 2, its message, and
# nothing written: no <dest>, and no `.tmp.` copy left under $ROOT/vm. Runs the verb through
# `sudo -n` (as the harness does), plus once without root. It never copies a kernel: the copying
# path is run by hand (`sudo -n st3b-priv kernel <new dest under $ROOT/vm>`), and its file removed.
# Then `umount` takes a mountpoint directly under $ROOT/mnt (BL-902), which `mount` takes too,
# and refuses paths outside it; it never unmounts anything (no such mount exists). Last, `holders`
# (BL-762) on mounts of its own (ext4s on loop devices under $ROOT/run/<id>, its lock held): it
# refuses what `umount` refuses; on a clean mount it says `none` in every section but the mount
# namespaces one, which lists the slave copies other namespaces get of a mount under a shared
# /work, but leaves out our own idle copy. It counts a mount on top of ours, names a process whose
# cwd is in the mount and whose fd is on it (by st_dev and by mount ID), two threads whose own cwds
# (unshared CLONE_FS) are in it on one pid/tid,tid line, and a loop device backed by a file in it;
# on an xfs shut down (`xfs_io -x -c shutdown`), where every stat fails, it still names an fd by
# mount ID. The shutdown is guarded: it runs only on our xfs, mounted from our loop device backed
# under $ROOT/run, and the guard is shown to refuse /work and our other mounts. Every setup step is
# fatal. A sweep that cannot unmount prints the same report. Everything it made is swept and
# removed, and a failed sweep in the exit trap fails the run.
#
#   contrib/node/crash/st3b-priv-tests.sh
set -euo pipefail
shopt -s inherit_errexit
umask 077

ROOT=/work/scratch/st3b
VM=$ROOT/vm
MNT=$ROOT/mnt
priv=$(realpath -e -- "$(dirname -- "$0")/st3b-priv")
fail=0
links=()
run_id=$(date +%s)-$$
run=$ROOT/run/$run_id
mnt=$MNT/$run_id/h
kids=()
inner=""
# shellcheck disable=SC2329 # run by the EXIT trap.
cleanup() {
	local l swept=1
	for l in "${links[@]}"; do rm -f -- "$l"; done
	for l in "${kids[@]}"; do kill "$l" 2>/dev/null || true; done
	for l in "${kids[@]}"; do wait "$l" 2>/dev/null || true; done
	[ -z "$inner" ] || sudo -n losetup -d "$inner" || true
	[ -d "$run" ] || return 0
	exec 8<&-
	sudo -n "$priv" sweep "$run_id" >/dev/null || swept=0
	rm -f -- "$run/data.img" "$run/sub.img" "$run/xfs.img" "$run/lock"
	rmdir -- "$mnt" "$MNT/$run_id/x" "$MNT/$run_id" "$run" 2>/dev/null || true
	[ "$swept" = 1 ] || {
		echo "FAIL: run $run_id did not sweep"
		exit 1
	}
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

# shellcheck disable=SC2329 # run through expect.
u() { sudo -n "$priv" umount "$@"; }
expect "umount directly under mnt/" "not mounted: $MNT/st3b-priv-test-none-$$" \
	u "$MNT/st3b-priv-test-none-$$"
expect "umount mnt/ itself" "not under $MNT" u "$MNT"
expect "umount beside mnt/" "not under $MNT" u "$ROOT/run/st3b-priv-test"
expect "umount dot-dot" "bad mountpoint" u "$MNT/.."
expect "umount under a missing dir" "not under $MNT" u "$MNT/st3b-priv-test-none-$$/x"

# shellcheck disable=SC2329 # run through expect.
h() { sudo -n "$priv" holders "$@"; }
expect "holders no argument" "holders <mnt>" h
expect "holders directly under mnt/" "not mounted: $MNT/st3b-priv-test-none-$$" \
	h "$MNT/st3b-priv-test-none-$$"
expect "holders mnt/ itself" "not under $MNT" h "$MNT"
expect "holders beside mnt/" "not under $MNT" h "$ROOT/run/st3b-priv-test"
expect "holders dot-dot" "bad mountpoint" h "$MNT/.."
expect "holders under a missing dir" "not under $MNT" h "$MNT/st3b-priv-test-none-$$/x"

# The lines of section $1 (its title's start) of report $2.
section() { awk -v t="== $1" '/^== / { on = index($0, t) == 1; next } on' <<<"$2"; }

# check <label> <section> <ERE> <report>: a line of that section matches.
check() {
	if section "$2" "$4" | grep -qE -- "$3"; then
		echo "ok   $1"
	else
		echo "FAIL $1: no line /$3/ under \"== $2\" in:"
		echo "$4"
		fail=1
	fi
}

# no <label> <section> <ERE> <report>: no line of that section matches.
no() {
	if section "$2" "$4" | grep -qE -- "$3"; then
		echo "FAIL $1: a line /$3/ under \"== $2\" in:"
		echo "$4"
		fail=1
	else
		echo "ok   $1"
	fi
}

# fs <image> <mountpoint> [xfs]: an ext4 (or xfs) on a loop device of ours, mounted and given to
# us; the device in $fs_dev. Called as a plain command, so any failed step ends the run.
fs() {
	truncate -s 320M -- "$1"
	fs_dev=$(sudo -n "$priv" losetup-attach "$1")
	sudo -n "$priv" mkfs "$fs_dev" "${3:-ext4}"
	mkdir -p -- "$2"
	sudo -n "$priv" mount "$fs_dev" "$2" "${3:-ext4}"
	sudo -n "$priv" chown-mnt "$2"
}

# our_xfs <mountpoint> <device>: true only for an xfs mounted at <mountpoint> from <device>, one
# of our loop devices, backed under $ROOT/run.
our_xfs() {
	[ "$(findmnt -rn -o FSTYPE,SOURCE --mountpoint "$1" 2>/dev/null)" = "xfs $2" ] || return 1
	[[ $2 =~ ^/dev/loop[0-9]+$ ]] || return 1
	case $(losetup -nO BACK-FILE "$2" 2>/dev/null) in "$ROOT"/run/?*) ;; *) return 1 ;; esac
}

# cwd_in <pid> <dir> [<n>]: waits until <n> (1) of <pid>'s threads have their cwd at <dir>.
cwd_in() {
	local t n
	for _ in $(seq 100); do
		n=0
		for t in /proc/"$1"/task/*; do [ "$(readlink "$t/cwd")" != "$2" ] || n=$((n + 1)); done
		[ "$n" -lt "${3:-1}" ] || return 0
		sleep 0.1
	done
	return 1
}

mkdir -m 0700 -- "$run"
: >"$run/lock"
exec 8<"$run/lock"
flock -n 8
fs "$run/data.img" "$mnt"
loop=$fs_dev
ns="^[0-9]+ [^ ]+ $mnt shared:[0-9]+"

report=$(h "$mnt")
for s in "fuser -vm" "lsof +f" "/proc:" "loop devices" "swap files"; do
	check "holders of a clean mount: $s none" "$s" '^none$' "$report"
done
no "holders of a clean mount: our own idle copy left out" "mount namespaces" " own$" "$report"

fs "$run/sub.img" "$mnt/sub"
sub=$fs_dev
report=$(h "$mnt")
check "holders: a mount on top of ours" "mount namespaces" "$ns on-top 1 fd-pids 0 own$" "$report"
if [[ $(section "mount namespaces" "$report" | head -1) =~ $ns\ on-top\ 1\ fd-pids\ 0\ own$ ]]; then
	echo "ok   holders: the busy copy first"
else
	echo "FAIL holders: the busy copy not first in:"
	echo "$report"
	fail=1
fi
sudo -n "$priv" umount "$mnt/sub"
sudo -n "$priv" losetup-detach "$sub"

: >"$mnt/held"
(cd "$mnt" && exec sleep 600 3<"$mnt/held") 8<&- &
holder=$!
kids+=("$holder")
# shellcheck disable=SC2016 # a Python program.
python3 -I -c '
import ctypes, os, sys, threading, time
libc = ctypes.CDLL(None, use_errno=True)
def own_cwd():
    if libc.unshare(0x200) != 0:
        os._exit(1)
    os.chdir(sys.argv[1])
    time.sleep(600)
for _ in range(2):
    threading.Thread(target=own_cwd, daemon=True).start()
time.sleep(600)
' "$mnt" 8<&- &
threaded=$!
kids+=("$threaded")
cwd_in "$holder" "$mnt"
cwd_in "$threaded" "$mnt" 2
truncate -s 8M -- "$mnt/inner.img"
inner=$(sudo -n losetup --find --show -- "$mnt/inner.img")
report=$(h "$mnt")
while IFS= read -r l; do echo "     | $l"; done <<<"$report"
check "holders: a cwd and an fd, by fuser" "fuser -vm" " $holder f\.c\.\. sleep$" "$report"
check "holders: a cwd in the mount, by lsof" "lsof +f" "^sleep +$holder .* cwd " "$report"
check "holders: an fd on the mount, by lsof" "lsof +f" "^sleep +$holder .* 3r .*/held$" "$report"
check "holders: a cwd and an fd, in /proc" "/proc:" "^$holder sleep cwd:dev,fd/3:dev\+mnt$" \
	"$report"
check "holders: two threads' own cwds, as one pid/tid,tid" "/proc:" \
	"^$threaded/[0-9]+,[0-9]+ [^ ]+ cwd:dev$" "$report"
no "holders: the thread's process not named" "/proc:" "^$threaded " "$report"
check "holders: our copy with an fd on it" "mount namespaces" "$ns on-top 0 fd-pids 1 own$" \
	"$report"
check "holders: a loop backed in the mount" "loop devices" "^$inner $mnt/inner.img$" "$report"
sudo -n losetup -d "$inner"
inner=""
kill "$threaded"
wait "$threaded" 2>/dev/null || true
kids=("$holder")

x=$MNT/$run_id/x
fs "$run/xfs.img" "$x" xfs
xdev=$fs_dev
for bad in "/work $(findmnt -rn -o SOURCE --mountpoint /work)" "$run $xdev" "$mnt $loop" \
	"$x $loop" "$x /dev/null"; do
	if our_xfs "${bad% *}" "${bad##* }"; then
		echo "FAIL the shutdown guard took ${bad% *} on ${bad##* }"
		fail=1
	else
		echo "ok   the shutdown guard refuses ${bad% *} on ${bad##* }"
	fi
done
: >"$x/held"
(exec sleep 600 3<"$x/held") 8<&- &
xholder=$!
kids+=("$xholder")
for _ in $(seq 100); do
	[ "$(readlink "/proc/$xholder/fd/3")" != "$x/held" ] || break
	sleep 0.1
done
our_xfs "$x" "$xdev" || {
	echo "FAIL: $x is not our xfs on $xdev; not shutting it down"
	exit 1
}
sudo -n xfs_io -x -c shutdown "$x"
report=$(h "$x")
check "holders on a shut-down xfs: an fd by mount ID" "/proc:" "^$xholder sleep fd/3:mnt$" \
	"$report"
kill "$xholder"
wait "$xholder" 2>/dev/null || true
kids=("$holder")
sudo -n "$priv" umount "$x"
sudo -n "$priv" losetup-detach "$xdev"

exec 8<&-
rc=0
err=$(sudo -n "$priv" sweep "$run_id" 2>&1 >/dev/null) || rc=$?
if [ "$rc" = 1 ] && [[ $err == *"holders of $mnt "* ]] &&
	[[ $err == *$'\n'"$holder sleep cwd:dev,fd/3:dev+mnt"$'\n'* ]]; then
	echo "ok   a sweep that cannot unmount prints its holders"
else
	echo "FAIL a sweep that cannot unmount: exit $rc, stderr: $err"
	fail=1
fi
kill "$holder"
wait "$holder" 2>/dev/null || true
kids=()
if sudo -n "$priv" sweep "$run_id" >/dev/null && [ -z "$(sudo -n "$priv" leaks "$run_id")" ]; then
	echo "ok   the holders run swept"
else
	echo "FAIL: run $run_id did not sweep"
	fail=1
fi

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
[ "$fail" = 0 ] && echo "st3b-priv kernel, umount and holders: all ok"
exit "$fail"

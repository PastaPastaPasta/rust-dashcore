#!/usr/bin/env bash
# ST3b S2: the device stack leaves nothing behind on any exit path. For each filesystem: a clean
# selftest, a selftest that panics with its recording stack up (Drop tears it down), one killed
# with SIGKILL with the stack up (a sweep of it by name refuses while it lives; the stale sweep
# fails on it while its directory is gone, and tears it down once it is back), and two paused
# while another mount namespace starts, then resumed. In the first a sandbox like perfgate's
# (`bwrap --ro-bind / /`) starts: its copy of our mount is a slave of `/work`'s, so our umount
# takes it too, and the run tears down while the sandbox lives. In the second a namespace with
# private propagation (`unshare --mount`, as perfgate's holder: BL-510) starts and lives 20 s: its
# copy holds our device, and the teardown's `dm-remove` waits it out. After each, `dmsetup ls`,
# `losetup -a` and `findmnt` must show nothing of ours (anywhere on the box: run it when no other
# st3b run is up).
#
#   contrib/node/crash/leak-tests.sh <path to st3b-crash>
#
# The panic and kill cases keep their run directories (images, as for any failed run) and print
# them; remove those afterwards.
set -euo pipefail
umask 077

bin=${1:?usage: leak-tests.sh <path to st3b-crash>}
ROOT=/work/scratch/st3b
export TMPDIR=${TMPDIR:-$ROOT/tmp}
fail=0

# Nothing of ours in the kernel's own lists (not only what the wrapper reports).
no_leaks() {
	local left
	left=$({
		sudo -n dmsetup ls --noheadings 2>/dev/null | awk '$1 ~ /^st3b-/'
		losetup -nO NAME,BACK-FILE | grep -F " $ROOT/" || true
		findmnt -rn -o TARGET | grep -F "$ROOT/" || true
	})
	if [ -n "$left" ]; then
		echo "LEAK after $1:"
		echo "  ${left//$'\n'/$'\n'  }"
		fail=1
	else
		echo "ok   $1: no devices or mounts left"
	fi
}

# Waits for the paused selftest writing to $1; prints its run id, or nothing.
paused_run() {
	local run=
	for _ in $(seq 120); do
		run=$(sed -n 's/^run \([^ ]*\) .*/\1/p' "$1")
		[ -n "$run" ] && [ -e "$ROOT/run/$run/paused" ] && break
		sleep 0.5
	done
	[ -n "$run" ] && [ -e "$ROOT/run/$run/paused" ] && echo "$run"
}

# The innermost process under $1 (a sandbox's command), once it runs `sleep`.
sandboxed() {
	local pid=$1 kid
	for _ in $(seq 60); do
		pid=$1
		while kid=$(ps -o pid= --ppid "$pid" | head -1) && [ -n "$kid" ]; do pid=${kid// /}; done
		[ "$pid" != "$1" ] && [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = sleep ] && {
			echo "$pid"
			return
		}
		sleep 0.5
	done
}

for fs in ext4 xfs; do
	"$bin" selftest --fs "$fs" --rand 4 --torn 4 >/dev/null || {
		echo "FAIL $fs clean selftest"
		fail=1
	}
	no_leaks "$fs clean"

	out=$("$bin" selftest --fs "$fs" --fault panic 2>&1) && {
		echo "FAIL $fs panic case exited 0"
		fail=1
	}
	grep -q 'selftest: panicked' <<<"$out" || {
		echo "FAIL $fs panic case did not panic: $out"
		fail=1
	}
	no_leaks "$fs panic"
	sed -n 's/^run \([^ ]*\) .*/  kept run \1/p' <<<"$out"

	log=$(mktemp "$TMPDIR/leak-kill.XXXXXX")
	"$bin" selftest --fs "$fs" --fault pause >"$log" 2>&1 &
	pid=$!
	run=$(paused_run "$log")
	if [ -z "$run" ]; then
		echo "FAIL $fs kill case never paused: $(cat "$log")"
		kill -KILL "$pid" 2>/dev/null || true
		fail=1
	else
		# Alive, it holds its lock: a sweep of it by name must refuse and touch nothing.
		if "$bin" sweep "$run" >/dev/null 2>&1; then
			echo "FAIL $fs live: sweeping live run $run succeeded"
			fail=1
		elif ! sudo -n dmsetup ls | grep -q "^st3b-$run-"; then
			echo "FAIL $fs live: the refused sweep still removed run $run's devices"
			fail=1
		else
			echo "ok   $fs live: a sweep of live run $run refused, its stack intact"
		fi
		kill -KILL "$pid"
		wait "$pid" 2>/dev/null || true
		# The kill really left a stack behind, or this case proves nothing.
		if sudo -n dmsetup ls --noheadings | grep -q "^st3b-$run-"; then
			echo "ok   $fs kill: run $run left its stack up"
		else
			echo "FAIL $fs kill: run $run left nothing to sweep"
			fail=1
		fi
		# Orphaned (its directory gone, as a prune would leave it, its stack up): the stale sweep
		# must fail and name it, touching nothing; with the directory back it sweeps.
		mv -- "$ROOT/run/$run" "$ROOT/orphan-$run"
		if out=$("$bin" sweep --stale 2>&1); then
			echo "FAIL $fs orphan: the stale sweep passed over orphaned run $run: $out"
			fail=1
		elif [[ $out != *"$run"* ]] || ! sudo -n dmsetup ls | grep -q "^st3b-$run-"; then
			echo "FAIL $fs orphan: the stale sweep did not name run $run, or removed its stack: $out"
			fail=1
		else
			echo "ok   $fs orphan: the stale sweep failed on orphaned run $run, its stack intact"
		fi
		mv -- "$ROOT/orphan-$run" "$ROOT/run/$run"
		"$bin" sweep --stale | sed 's/^/  /' || fail=1
		echo "  kept run $run"
	fi
	rm -f -- "$log"
	no_leaks "$fs kill + sweep"

	log=$(mktemp "$TMPDIR/leak-pin.XXXXXX")
	"$bin" selftest --fs "$fs" --fault pause --rand 4 --torn 4 >"$log" 2>&1 &
	pid=$!
	run=$(paused_run "$log")
	if [ -z "$run" ]; then
		echo "FAIL $fs pin case never paused: $(cat "$log")"
		kill -KILL "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
		"$bin" sweep --stale | sed 's/^/  /' || fail=1
		fail=1
	else
		# Started with the recording stack mounted, as perfgate's bench sandboxes do.
		bwrap --ro-bind / / --dev /dev --proc /proc --unshare-pid --unshare-ipc --die-with-parent \
			-- sleep 600 &
		box=$!
		inner=$(sandboxed "$box")
		# The sandbox must hold a copy of the run's mount, or this case proves nothing.
		if [ -z "$inner" ]; then
			echo "FAIL $fs pin: the sandbox never started"
			fail=1
		elif ! grep -qF " $ROOT/mnt/$run/" "/proc/$inner/mountinfo"; then
			echo "FAIL $fs pin: the sandbox has no copy of run $run's mount, so this proves nothing"
			fail=1
		else
			echo "ok   $fs pin: the sandbox holds a copy of run $run's mount"
		fi
		rm -f -- "$ROOT/run/$run/paused"
		if wait "$pid"; then
			echo "ok   $fs pin: run $run tore down with the sandbox up"
		else
			echo "FAIL $fs pin: run $run failed with the sandbox up: $(tail -3 "$log")"
			fail=1
		fi
		kill -0 "$box" 2>/dev/null || {
			echo "FAIL $fs pin: the sandbox exited before the teardown, so this proves nothing"
			fail=1
		}
		if [ -n "$inner" ] && grep -qF " $ROOT/mnt/$run/" "/proc/$inner/mountinfo"; then
			echo "FAIL $fs pin: the sandbox still holds run $run's mount after the teardown"
			fail=1
		fi
		no_leaks "$fs pin, the sandbox still up"
		kill "$box" 2>/dev/null || true
		wait "$box" 2>/dev/null || true
		"$bin" sweep --stale | sed 's/^/  /' || fail=1
	fi
	rm -f -- "$log"
	no_leaks "$fs pin"

	log=$(mktemp "$TMPDIR/leak-hold.XXXXXX")
	"$bin" selftest --fs "$fs" --fault pause --rand 4 --torn 4 >"$log" 2>&1 &
	pid=$!
	run=$(paused_run "$log")
	if [ -z "$run" ]; then
		echo "FAIL $fs hold case never paused: $(cat "$log")"
		kill -KILL "$pid" 2>/dev/null || true
		wait "$pid" 2>/dev/null || true
		"$bin" sweep --stale | sed 's/^/  /' || fail=1
		fail=1
	else
		unshare --user --map-root-user --mount --propagation private -- sleep 20 &
		holder=$!
		for _ in $(seq 20); do
			grep -qF " $ROOT/mnt/$run/" "/proc/$holder/mountinfo" 2>/dev/null && break
			sleep 0.5
		done
		if grep -qF " $ROOT/mnt/$run/" "/proc/$holder/mountinfo" 2>/dev/null; then
			echo "ok   $fs hold: a private namespace holds a copy of run $run's mount"
		else
			echo "FAIL $fs hold: the holder has no copy of run $run's mount, so this proves nothing"
			fail=1
		fi
		t0=$SECONDS
		rm -f -- "$ROOT/run/$run/paused"
		if wait "$pid"; then
			echo "ok   $fs hold: run $run passed, its teardown waiting out the holder ($((SECONDS - t0)) s)"
		else
			echo "FAIL $fs hold: run $run failed with the holder up: $(tail -3 "$log")"
			fail=1
		fi
		kill "$holder" 2>/dev/null || true
		wait "$holder" 2>/dev/null || true
		"$bin" sweep --stale | sed 's/^/  /' || fail=1
	fi
	rm -f -- "$log"
	no_leaks "$fs hold"
done

[ $fail = 0 ] && echo "leak tests: PASS" || echo "leak tests: FAIL"
exit $fail

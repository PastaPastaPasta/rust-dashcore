#!/usr/bin/env bash
# Builds ST3b S7's guest initramfs (PLAN-ST3b-S7 §2.1) with no download:
#   - the host's static busybox and its applet links;
#   - <rdashd>, stripped into the image (`strip -o`; the given binary is never touched), every
#     shared library `ldd` lists for it, and the loader;
#   - /xfs.ko, the running kernel's xfs module decompressed (it has no dependencies; one with
#     some is refused);
#   - /init (this directory's `init`).
#
#   contrib/node/crash/vm/mkguest.sh <rdashd> <out dir> [kernel]
#
# <out dir> must be new or empty, under /work/scratch/. The guest is built in <out dir>.partial.<pid>
# beside it and renamed into place whole, so a failed build leaves no half guest behind (S7 r1
# m9). <out dir> gets root/ (the guest's tree),
# initramfs.cpio.gz and manifest.txt: the sha256 of every file in the image (`sha256sum` lines,
# paths relative to root/), then `kernel <sha256> <path>`, `rdashd <sha256> <path>` (the unstripped
# input) and `initramfs <sha256> initramfs.cpio.gz`. `st3b-crash vm --guest <out dir>` boots
# that kernel with that initramfs and checks both against the manifest first.
#
# [kernel] defaults to /work/scratch/st3b/vm/vmlinuz-$(uname -r), the copy
# `sudo -n contrib/node/crash/st3b-priv kernel <dest>` makes; the xfs module is the running
# kernel's, so the kernel must be the running one too.
set -euo pipefail
umask 022

die() {
	echo "mkguest: $*" >&2
	exit 2
}

[ $# -ge 2 ] && [ $# -le 3 ] || die "usage: mkguest.sh <rdashd> <out dir> [kernel]"
rdashd=$(realpath -e -- "$1") || die "no rdashd at $1"
out=$(realpath -m -- "$2")
kernel=$(realpath -e -- "${3:-/work/scratch/st3b/vm/vmlinuz-$(uname -r)}") ||
	die "no kernel (make one with st3b-priv kernel)"
here=$(cd "$(dirname "$0")" && pwd)

case $out in /work/scratch/?*) ;; *) die "the out dir must be under /work/scratch/: $out" ;; esac
[ -f "$rdashd" ] && [ -x "$rdashd" ] || die "not an executable: $rdashd"
case $(basename "$kernel") in vmlinuz-"$(uname -r)") ;; *) die "not the running kernel's image: $kernel" ;; esac
if [ -e "$out" ]; then
	[ -d "$out" ] && [ -z "$(ls -A "$out")" ] || die "the out dir is not new or empty: $out"
fi
final=$out
out=$final.partial.$$
mkdir "$out"
# The build's own directory: removed whole on any failure.
trap 'rm -rf -- "$out"' EXIT
busybox=/usr/bin/busybox
ldd "$busybox" >/dev/null 2>&1 && die "$busybox is not static"
xfs=$(modinfo -n xfs) || die "no xfs module"
[ -z "$(modinfo -F depends xfs)" ] || die "xfs.ko has dependencies: $(modinfo -F depends xfs)"

root=$out/root
mkdir -p "$root"/{bin,sbin,usr/bin,usr/sbin,proc,sys,dev,data}

cp -- "$busybox" "$root/bin/busybox"
"$busybox" --list-full | while read -r applet; do
	[ "$applet" = bin/busybox ] || ln -s /bin/busybox "$root/$applet"
done

strip -o "$root/usr/bin/rdashd" -- "$rdashd"
# `libx.so => /path (0x...)`, and the loader as `/path (0x...)`; the vDSO has no file.
ldd "$rdashd" | awk '$2 == "=>" && $3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' | sort -u |
	while read -r lib; do
		mkdir -p "$root$(dirname "$lib")"
		cp -L -- "$lib" "$root$lib"
	done
[ -e "$root/lib64/ld-linux-x86-64.so.2" ] || die "ldd listed no loader for $rdashd"

zstd -q -d -o "$root/xfs.ko" -- "$xfs"
install -m 0755 "$here/init" "$root/init"

(cd "$root" && find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc -R 0:0 --quiet) |
	gzip -1 >"$out/initramfs.cpio.gz"

sha() {
	sha256sum -- "$1" | cut -d' ' -f1
}
{
	(cd "$root" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum)
	echo "kernel $(sha "$kernel") $kernel"
	echo "rdashd $(sha "$rdashd") $rdashd"
	echo "initramfs $(sha "$out/initramfs.cpio.gz") initramfs.cpio.gz"
} >"$out/manifest.txt"
du -sh "$out/initramfs.cpio.gz" "$root"
# Over an empty <out dir> too (rename(2) replaces an empty directory).
mv -T -- "$out" "$final"
trap - EXIT
echo "guest in $final"

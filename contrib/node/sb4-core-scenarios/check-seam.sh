#!/usr/bin/env bash
# Prove the harness's seams: the -Wl,--wrap lines of wrap-symbols.txt
# (designs/SB4-dash-chain.md §15 `cl`; Sol r3, rev3 O-m2). For each symbol:
#
#  0. MANIFEST holds: the binary, its --commit (the v24.0 pin) and every linked
#     archive match their recorded SHA-256. The binary was linked with exactly
#     these wraps: MANIFEST's `wrap` lines and the binary's __wrap_ symbols are
#     both the set in wrap-symbols.txt (the binary's __wrap_ symbols add the
#     interposed ones, checked after the wraps).
#  1. Every object in every archive the link read (MANIFEST) that references it
#     is listed; the only referrer is the one wrap-symbols.txt names
#     (chainlock/handler for both), a member name unique in its archive.
#  2. `ld -r --wrap` of that object leaves every .text section byte-identical
#     and every relocation the same except that those against <sym> now name
#     __wrap_<sym> (there is at least one).
#  3. In the linked binary, the named caller calls __wrap_<sym>, and no code
#     outside the wrapper calls the real definition (VerifyChainLock's wrapper
#     never does; EnforceBlock's wrapper calls __real_ = the definition once).
#
# usage: check-seam.sh <out-dir from build.sh>   (prints a report; exit 1 on failure)
set -euo pipefail

out=$(cd "${1:?usage: check-seam.sh <out-dir>}" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
commit=$(awk '$1 == "commit" { print $2 }' "$out/MANIFEST")
build=${SB4_BUILD:-/work/scratch/sb4-core-scenarios/core-build}/$commit
src=$build/src
bin=$out/sb4_scenarios
work=$(mktemp -d "${TMPDIR:-/dev/shm}/sb4-seam.XXXXXX")
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
# Every relocation as "<section> <offset> <type> <symbol> <addend>", sorted: ld -r
# renumbers symbols, moves the sections in the file and reorders them.
relocs() {
  readelf -rW "$1" | awk '/^Relocation section/ { sec = $3; next }
    /^ *[0-9a-f]+ +[0-9a-f]+ +R_/ { $2 = ""; $4 = ""; print sec, $0 }' | sort
}
strip0() { local a=$1; echo "${a#"${a%%[!0]*}"}"; }

# Step 0 (regen.py's verify_build checks the same before generating).
bin_sha=$(sha256sum "$bin" | cut -d' ' -f1)
[ "$bin_sha" = "$(awk '$1 == "binary" { print $2 }' "$out/MANIFEST")" ] ||
  fail "sb4_scenarios does not match MANIFEST's binary hash"
pin=$(awk '!/^#/ && $1 == "v24.0" { print $2 }' "$here/../../../dash-oracle-sys/pins.txt")
[ "$("$bin" --commit)" = "$commit" ] && [ "$commit" = "$pin" ] || fail "binary --commit, MANIFEST and the v24.0 pin differ"
echo "binary     ${bin_sha:0:16} as in MANIFEST; --commit $commit = v24.0 pin"
objdump -d --no-show-raw-insn "$bin" > "$work/dis.txt"
nm "$bin" > "$work/bin.nm"
# The binary's text symbols, demangled<TAB>mangled, for finding a caller by name.
awk '$2 ~ /^[Tt]$/ { print $3 }' "$work/bin.nm" > "$work/fns.txt"
c++filt < "$work/fns.txt" | paste - "$work/fns.txt" > "$work/fns.tsv"
grep -v '^#' "$here/wrap-symbols.txt" | cut -f1 | sort > "$work/want.txt"
awk '$1 == "wrap" { print $2 }' "$out/MANIFEST" | sort > "$work/manifest.txt"
awk '$3 ~ /^__wrap_/ { sub(/^__wrap_/, "", $3); print $3 }' "$work/bin.nm" | sort -u > "$work/bin-wraps.txt"
cmp -s "$work/want.txt" "$work/manifest.txt" || fail "MANIFEST's wraps differ from wrap-symbols.txt"
grep -hv '^#' "$here/wrap-symbols.txt" "$here/interpose-symbols.txt" | cut -f1 | sort |
  cmp -s - "$work/bin-wraps.txt" || fail "the binary's __wrap_ symbols differ from wrap-symbols.txt + interpose-symbols.txt"
echo "wraps      $(wc -l < "$work/want.txt") in wrap-symbols.txt, MANIFEST and the binary"
n_archives=0
while read -r key digest path; do
  [ "$key" = archive ] || continue
  [ -r "$build/$path" ] || fail "linked archive $path is missing"
  [ "$(sha256sum "$build/$path" | cut -d' ' -f1)" = "$digest" ] || fail "archive $path does not match MANIFEST"
  (cd "$build" && nm -A "$path" 2> /dev/null) || fail "nm failed on $path"
  n_archives=$((n_archives + 1))
done < "$out/MANIFEST" > "$work/archives.nm"
echo "archives   $n_archives linked archives match MANIFEST's SHA-256"

while IFS=$'\t' read -r sym member caller want; do
  echo "== $want"
  # 1. Who references it.
  refs=$(awk -v s="$sym" '$2 == "U" && $3 == s { print $1 }' "$work/archives.nm" | sort -u)
  echo "referrers  ${refs:-none} (of $n_archives archives)"
  [ "$refs" = "src/libbitcoin_node.a:$member:" ] || fail "expected exactly $member, got: $refs"
  [ "$(ar t "$src/libbitcoin_node.a" | grep -cxF "$member")" = 1 ] || fail "$member is not unique in its archive"
  obj=$(find "$src" -name "$member" | head -1)
  cmp -s <(ar p "$src/libbitcoin_node.a" "$member") "$obj" || fail "archive member differs from $obj"
  echo "defined    $(awk -v s="$sym" '$2 == "T" && $3 == s { print $1 }' "$work/archives.nm")"

  # 2. ld -r --wrap: .text unchanged, reference retargeted.
  ld -r --wrap="$sym" "$obj" -o "$work/wrapped.o"
  nm "$work/wrapped.o" > "$work/wrapped.nm"
  grep -q " U __wrap_$sym\$" "$work/wrapped.nm" || fail "wrapped object does not reference __wrap_$sym"
  grep -q " U $sym\$" "$work/wrapped.nm" && fail "wrapped object still references the real symbol"
  n=0
  for sec in $(readelf -SW "$obj" | sed -n 's/^ *\[ *[0-9]*\] \(\.text[^ ]*\) .*/\1/p'); do
    objcopy -O binary --only-section="$sec" "$obj" "$work/a.bin"
    objcopy -O binary --only-section="$sec" "$work/wrapped.o" "$work/b.bin"
    cmp -s "$work/a.bin" "$work/b.bin" || fail "section $sec differs after --wrap"
    n=$((n + 1))
  done
  [ "$n" -gt 0 ] || fail "no .text sections found"
  echo "text       $n .text sections of $member byte-identical after ld -r --wrap (sha256 $(sha256sum "$obj" | cut -c1-16))"
  relocs "$obj" > "$work/rel-a.txt"
  relocs "$work/wrapped.o" > "$work/rel-b.txt"
  moved=$(grep -c " __wrap_$sym " "$work/rel-b.txt" || true)
  [ "$moved" -gt 0 ] || fail "no relocation names __wrap_$sym"
  [ "$(grep -c " $sym " "$work/rel-a.txt" || true)" = "$moved" ] || fail "not every relocation against $sym moved"
  sed "s/ __wrap_$sym / $sym /" "$work/rel-b.txt" | cmp -s - "$work/rel-a.txt" ||
    fail "relocations differ beyond $sym -> __wrap_$sym"
  echo "relocs     $moved relocations retargeted to __wrap_, every other relocation unchanged"

  # 3. The linked binary.
  wrap_addr=$(awk -v s="__wrap_$sym" '$3 == s { print $1 }' "$work/bin.nm")
  real_addr=$(awk -v s="$sym" '$3 == s { print $1 }' "$work/bin.nm")
  [ -n "$wrap_addr" ] || fail "binary has no __wrap_$sym"
  fn=$(awk -F'\t' -v c="$caller" 'index($1, c) == 1 && $2 !~ /\./ { print $2 }' "$work/fns.tsv") # (not the .cold split)
  [ "$(echo "$fn" | grep -c .)" = 1 ] || fail "expected one $caller, got: $fn"
  objdump -d --no-show-raw-insn --disassemble="$fn" "$bin" > "$work/fn.txt"
  call=$(grep -E "call +$(strip0 "$wrap_addr") <__wrap_" "$work/fn.txt" | sed 's/^ *//' | head -1 || true)
  [ -n "$call" ] || fail "$caller does not call __wrap_"
  echo "call       $caller…) → $call"
  if [ -n "$real_addr" ]; then
    # References to the real definition (call, jmp, jcc, lea ... # addr), by enclosing function.
    callers=$(awk -v t="$(strip0 "$real_addr")" '
      /^[0-9a-f]+ <.*>:$/ { f = $2 }
      $0 ~ "[ \t]" t " <" { print f }' "$work/dis.txt" | sort | uniq -c)
    echo "real       ${callers:-no calls}"
    others=$(echo "$callers" | grep -v "<__wrap_$sym>" | grep -c . || true)
    [ "$others" = 0 ] || fail "the real definition is called from outside its wrapper"
  fi
done < <(grep -v '^#' "$here/wrap-symbols.txt")

# The interposed definitions (interpose-symbols.txt, HX1 X0). For each symbol:
#  1. no linked archive references it (every caller shares the defining member);
#  2. build.sh's copy of that member equals the archive's in every section but the
#     symbol table, relocation for relocation, and its symbol table differs only in
#     <sym> GLOBAL -> WEAK and one added GLOBAL FUNC __real_<sym> at <sym>'s
#     address; the member holds one relocation against <sym> in each listed caller
#     and no other, no clone of <sym> but its .cold part, and no relocation outside
#     the two that reaches into them through another symbol;
#  3. in the binary <sym> is __wrap_<sym>, every listed caller calls it, and only
#     the wrapper references __real_<sym> (any instruction naming its address).
grep -v '^#' "$here/interpose-symbols.txt" | cut -f1 | sort > "$work/want-i.txt"
awk '$1 == "interpose" { print $2 }' "$out/MANIFEST" | sort | cmp -s "$work/want-i.txt" - ||
  fail "MANIFEST's interposes differ from interpose-symbols.txt"
while read -r key digest path; do
  [ "$key" = object ] || continue
  [ "$(sha256sum "$out/$path" | cut -d' ' -f1)" = "$digest" ] || fail "object $path does not match MANIFEST"
done < "$out/MANIFEST"
n_interposed=0
while IFS=$'\t' read -r sym member callers want; do
  echo "== $want (interposed)"
  n_interposed=$((n_interposed + 1))
  refs=$(awk -v s="$sym" '$2 ~ /^[Uw]$/ && $3 == s { print $1 }' "$work/archives.nm" | sort -u)
  [ -z "$refs" ] || fail "linked archives reference $sym: $refs"
  defs=$(awk -v s="$sym" '$2 ~ /^[TWVi]$/ && $3 == s { sub(/[0-9a-f]+$/, "", $1); print $1 }' "$work/archives.nm")
  [ "$defs" = "src/libbitcoin_node.a:$member:" ] || fail "expected one definition in $member, got: $defs"
  echo "referrers  none outside $member (of $n_archives archives)"
  ar p "$src/libbitcoin_node.a" "$member" > "$work/orig.o"
  cmp -s "$work/orig.o" "$out/derived/orig-$member" || fail "derived/orig-$member is not the archive's $member"
  copy=$out/derived/sb4-$member
  grep -q " derived/sb4-$member\$" "$out/MANIFEST" || fail "MANIFEST does not record $copy"
  readelf -SW "$work/orig.o" > "$work/sec-a.txt"
  readelf -SW "$copy" > "$work/sec-b.txt"
  python3 -I - "$work/orig.o" "$copy" "$work/sec-a.txt" "$work/sec-b.txt" <<'EOF' || fail "$member's copy differs outside its symbol table"
import re, sys
# Section headers: [Nr] Name Type Address Off Size ES Flg Lk Inf Al. Compare every
# section's header and bytes, by index, except the symbol and string tables.
def sections(path):
    out = []
    for line in open(path):
        m = re.match(r"\s*\[\s*(\d+)\]\s+(\S*)\s+(\S+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+(.*)$", line)
        if m:
            out.append((int(m[1]), m[2], m[3], int(m[5], 16), int(m[6], 16), m[7].split()))
    return out
a, b = sections(sys.argv[3]), sections(sys.argv[4])
da, db = open(sys.argv[1], "rb").read(), open(sys.argv[2], "rb").read()
assert len(a) == len(b) > 1, "section counts differ"
n = 0
for (ia, na, ta, oa, sa, fa), (ib, nb, tb, ob, sb, fb) in zip(a, b):
    assert (ia, na, ta) == (ib, nb, tb), f"section {ia} {na} became {ib} {nb}"
    if na in (".symtab", ".strtab", ".shstrtab"):  # names are compared above
        continue
    assert sa == sb and fa == fb, f"section {ia} {na} header differs"
    if ta != "NOBITS":
        assert da[oa:oa + sa] == db[ob:ob + sb], f"section {ia} {na} bytes differ"
    n += 1
print(f"sections   {n} of {len(a)} sections byte-identical (all but .symtab, .strtab, .shstrtab)")
EOF
  relocs "$work/orig.o" > "$work/rel-a.txt"
  relocs "$copy" > "$work/rel-b.txt"
  cmp -s "$work/rel-a.txt" "$work/rel-b.txt" || fail "relocations of $member's copy differ"
  echo "relocs     $(wc -l < "$work/rel-a.txt") relocations identical"
  syms() { readelf -sW "$1" | awk '$1 ~ /^[0-9]+:$/ { $1 = ""; print }' | sort; }
  diff <(syms "$work/orig.o") <(syms "$copy") | grep '^[<>]' > "$work/sym-diff.txt" || true
  # Every symbol interposed in this member: GLOBAL -> WEAK, plus __real_<sym>.
  : > "$work/sym-want.txt"
  while IFS=$'\t' read -r isym imember _; do
    [ "$imember" = "$member" ] || continue
    read -r ivalue isize itype _ ivis indx _ < <(syms "$work/orig.o" | awk -v s="$isym" '$7 == s') || true
    [ -n "${ivalue:-}" ] || fail "$member does not define $isym"
    printf '< %s %s %s GLOBAL %s %s %s\n> %s %s %s WEAK %s %s %s\n> %s 0 %s GLOBAL %s %s __real_%s\n' \
      "$ivalue" "$isize" "$itype" "$ivis" "$indx" "$isym" "$ivalue" "$isize" "$itype" "$ivis" "$indx" "$isym" \
      "$ivalue" "$itype" "$ivis" "$indx" "$isym" >> "$work/sym-want.txt"
    [ "$isym" = "$sym" ] && value=$ivalue
  done < <(grep -v '^#' "$here/interpose-symbols.txt")
  cmp -s <(sed 's/  */ /g' "$work/sym-diff.txt" | sort) <(sort "$work/sym-want.txt") ||
    { cat "$work/sym-diff.txt" >&2; fail "symbol table of $member's copy differs beyond its interposed symbols"; }
  echo "symbols    only the interposed symbols GLOBAL -> WEAK, each with __real_ added ($sym at 0x$(strip0 "$value"))"
  # One relocation against <sym> per listed caller, none elsewhere.
  objdump -dr --no-show-raw-insn "$work/orig.o" | awk -v s="$sym" '
    /^[0-9a-f]+ <.*>:$/ { f = substr($2, 2, length($2) - 3) }
    $2 ~ /^R_X86_64_/ && ($3 == s || index($3, s "-") == 1 || index($3, s "+") == 1) { print f }' |
    c++filt | sort > "$work/sites.txt"
  IFS=';' read -ra want_callers <<< "$callers"
  [ "$(wc -l < "$work/sites.txt")" = "${#want_callers[@]}" ] ||
    { cat "$work/sites.txt" >&2; fail "$sym has $(wc -l < "$work/sites.txt") call sites, want ${#want_callers[@]}"; }
  for c in "${want_callers[@]}"; do
    [ "$(awk -v c="$c" 'index($0, c) == 1' "$work/sites.txt" | wc -l)" = 1 ] || fail "$c does not reference $sym once"
  done
  echo "sites      ${#want_callers[@]} relocations against $sym, one in each of: ${callers//;/, }"
  # No other way in: no clone of <sym> (only its .cold split), and no relocation
  # outside <sym>, its .cold part and the debug/unwind tables that reaches into
  # them through another symbol (a section symbol plus addend, say).
  readelf -sW "$work/orig.o" > "$work/syms.txt"
  readelf -rW "$work/orig.o" > "$work/rels.txt"
  python3 -I - "$sym" "$work/syms.txt" "$work/rels.txt" <<'EOF' || fail "$sym is reachable other than through its symbol"
import re, sys
sym = sys.argv[1]
syms, secs = {}, {}  # name -> (value, size, type, section index); section name -> indexes
for line in open(sys.argv[2]):
    c = line.split()
    if len(c) == 8 and re.fullmatch(r"\d+:", c[0]) and c[6].isdigit():
        if c[3] == "SECTION":
            secs.setdefault(c[7], set()).add(int(c[6]))
        else:
            syms[c[7]] = (int(c[1], 16), int(c[2], 0), c[3], int(c[6]))
clones = [n for n, e in syms.items() if n.startswith(sym + ".") and e[2] == "FUNC" and n != sym + ".cold"]
assert not clones, f"clones of {sym}: {clones}"
ranges = [(syms[n][3], syms[n][0], syms[n][0] + syms[n][1]) for n in (sym, sym + ".cold") if n in syms]
code = {ndx for ndx, _, _ in ranges}
def inside(ndx, addr):
    return any(n == ndx and lo <= addr < hi for n, lo, hi in ranges)
n, sec = 0, None
for line in open(sys.argv[3]):
    m = re.match(r"Relocation section '\.rela(\S+)'", line)
    if m:
        sec = m[1]
        continue
    m = re.match(r"([0-9a-f]+)\s+[0-9a-f]+\s+(R_X86_64_\S+)\s+[0-9a-f]+\s+(\S+)(?:\s+([+-])\s+([0-9a-f]+))?$", line.strip())
    if not m or sec.startswith(".debug") or sec == ".eh_frame":
        continue
    target = m[3]
    addend = int(m[5] or "0", 16) * (-1 if m[4] == "-" else 1)
    if target == sym:
        continue  # the counted call sites
    if sec in secs and len(secs[sec]) == 1 and inside(next(iter(secs[sec])), int(m[1], 16)):
        continue  # inside <sym> or its .cold part
    if target in syms:
        bases = [(syms[target][0], syms[target][3])]
    else:
        ndxs = secs.get(target, set())
        assert len(ndxs) <= 1 or not ndxs & code, f"section name {target} is ambiguous"
        bases = [(0, i) for i in ndxs]
    for value, ndx in bases:
        # S+A for an absolute relocation; a RIP-relative field ends 4 to 8 bytes
        # (an immediate may follow it) before the next instruction, its base.
        pc = m[2] in ("R_X86_64_PC32", "R_X86_64_PLT32", "R_X86_64_PC64") or "GOTPC" in m[2]
        for a in range(value + addend + 4, value + addend + 9) if pc else (value + addend,):
            assert not inside(ndx, a), f"{sec}+0x{m[1]} reaches {sym} via {target}{addend:+#x}"
        n += 1
print(f"reach      {n} other relocations against defined symbols; none reaches {sym} or its .cold part")
EOF

  # 3. The linked binary.
  addr=$(awk -v s="$sym" '$3 == s { print $1 }' "$work/bin.nm")
  wrap_addr=$(awk -v s="__wrap_$sym" '$3 == s { print $1 }' "$work/bin.nm")
  real_addr=$(awk -v s="__real_$sym" '$3 == s { print $1 }' "$work/bin.nm")
  [ -n "$addr" ] && [ "$addr" = "$wrap_addr" ] || fail "$sym is not __wrap_$sym in the binary"
  [ -n "$real_addr" ] && [ "$real_addr" != "$addr" ] || fail "no separate __real_$sym in the binary"
  for c in "${want_callers[@]}"; do
    fn=$(awk -F'\t' -v c="$c" 'index($1, c) == 1 && $2 !~ /\./ { print $2 }' "$work/fns.tsv")
    [ "$(echo "$fn" | grep -c .)" = 1 ] || fail "expected one $c, got: $fn"
    objdump -d --no-show-raw-insn --disassemble="$fn" "$bin" > "$work/fn.txt"
    grep -qE "(call|jmp) +$(strip0 "$addr") <" "$work/fn.txt" || fail "$c does not call __wrap_$sym"
  done
  echo "call       each caller → __wrap_ at $(strip0 "$addr")"
  callers_real=$(awk -v t="$(strip0 "$real_addr")" '
    /^[0-9a-f]+ <.*>:$/ { f = $1 }
    $0 ~ "[ \t]" t " <" { print f }' "$work/dis.txt" | sort | uniq -c)
  echo "real       ${callers_real:-no calls} (function at the __wrap_ address)"
  others=$(echo "$callers_real" | awk -v w="$addr" '$2 != w' | grep -c . || true)
  [ -n "$callers_real" ] && [ "$others" = 0 ] || fail "__real_$sym is called from outside its wrapper"
done < <(grep -v '^#' "$here/interpose-symbols.txt")
[ "$n_interposed" = "$(wc -l < "$work/want-i.txt")" ] || fail "checked $n_interposed interposed symbols, want $(wc -l < "$work/want-i.txt")"
echo "OK: the --wrap and interposed seams are the only changes"

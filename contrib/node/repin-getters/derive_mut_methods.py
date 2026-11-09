"""Lists the stable `&mut self` methods of the std types repin-getters touches, from the toolchain's rust-src.

Usage: python3 -I derive_mut_methods.py <rust-src library dir>
Prints one name per line, sorted; a per-file count goes to stderr.

A method counts when its signature takes `&mut self` (or `&'a mut self`) and the attributes right above it
include `#[stable(` and no `#[unstable(`. Inherent methods come from the types' own files; trait methods come
from the trait definitions those types implement (only that trait's block is read).
"""

import re
import sys
from pathlib import Path

INHERENT = [
    "alloc/src/vec/mod.rs",
    "alloc/src/collections/vec_deque/mod.rs",
    "alloc/src/collections/btree/map.rs",
    "alloc/src/collections/btree/map/entry.rs",
    "alloc/src/collections/btree/set.rs",
    "std/src/collections/hash/map.rs",
    "std/src/collections/hash/set.rs",
    "alloc/src/string.rs",
    "core/src/option.rs",
    "core/src/slice/mod.rs",
    "core/src/slice/ascii.rs",
    "alloc/src/slice.rs",
    "core/src/array/mod.rs",
    "core/src/str/mod.rs",
]
# (file, trait): the std traits with `&mut self` methods that those types implement.
TRAITS = [
    ("core/src/clone.rs", "Clone"),
    ("core/src/convert/mod.rs", "AsMut"),
    ("core/src/borrow.rs", "BorrowMut"),
    ("core/src/ops/deref.rs", "DerefMut"),
    ("core/src/ops/index.rs", "IndexMut"),
    ("core/src/ops/arith.rs", "AddAssign"),
    ("core/src/iter/traits/collect.rs", "Extend"),
    ("core/src/fmt/mod.rs", "Write"),
]

FN = re.compile(r"\bfn\s+(\w+)")


def skip_generics(text: str, i: int) -> int:
    """Index just past a `<...>` starting at `i` (nested, `->` ignored)."""
    depth = 0
    while i < len(text):
        c = text[i]
        if c == "<":
            depth += 1
        elif c == ">" and text[i - 1] != "-":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return i


def takes_mut_self(text: str, i: int) -> bool:
    while text[i].isspace():
        i += 1
    if text[i] == "<":
        i = skip_generics(text, i)
        while text[i].isspace():
            i += 1
    if text[i] != "(":
        return False
    return re.match(r"\(\s*&\s*(?:'\w+\s+)?mut\s+self\b", text[i:]) is not None


def attrs_above(text: str, start: int) -> str:
    """The attribute and doc lines directly above the line holding `start`."""
    lines = text[:start].splitlines()
    head = lines.pop() if lines else ""  # the fn line itself, up to `fn`
    out = [head]
    in_attr = False
    while lines:
        s = lines.pop().strip()
        if s.startswith(("#[", "///", "//")) or in_attr:
            out.append(s)
            # a multi-line attribute: its first line opens with `#[`, the ones below it close it
            in_attr = False
            continue
        if s.endswith((",", "(")) or s.startswith((")", "]")):
            out.append(s)
            in_attr = True
            continue
        break
    return " ".join(out)


def stable_mut_fns(text: str) -> set[str]:
    out = set()
    for m in FN.finditer(text):
        if not takes_mut_self(text, m.end()):
            continue
        attrs = attrs_above(text, m.start())
        if "#[stable(" in attrs and "#[unstable(" not in attrs:
            out.add(m.group(1))
    return out


def trait_block(text: str, name: str) -> str:
    m = re.search(rf"\bpub\s+(?:const\s+)?(?:unsafe\s+)?trait\s+{name}\b[^{{]*\{{", text)
    if not m:
        raise SystemExit(f"trait {name} not found")
    depth, i = 1, m.end()
    while depth:
        depth += {"{": 1, "}": -1}.get(text[i], 0)
        i += 1
    return text[m.start() : i]


def main(lib: str) -> None:
    names = set()
    for f in INHERENT:
        found = stable_mut_fns((Path(lib) / f).read_text())
        print(f"{f}: {len(found)}", file=sys.stderr)
        names |= found
    for f, t in TRAITS:
        found = stable_mut_fns(trait_block((Path(lib) / f).read_text(), t))
        print(f"{f} trait {t}: {sorted(found)}", file=sys.stderr)
        names |= found
    for n in sorted(names):
        print(n)
    print(f"total {len(names)}", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1])

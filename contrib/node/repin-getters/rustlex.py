"""A small Rust lexer: enough to find delimiter groups, paths and top-level commas.

Comments and whitespace are dropped; every token keeps its byte offsets into the
source text, so callers can splice the original text back together.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

_IDENT = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z0-9_]*")
_NUM = re.compile(r"[0-9][0-9A-Za-z_]*(?:\.[0-9][0-9A-Za-z_]*)?(?:[eE][+-]?[0-9_]+)?")
_PUNCT3 = ("<<=", ">>=", "...", "..=")
_PUNCT2 = ("::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=",
           "^=", "&=", "|=", "<<", ">>", "..")
OPEN = {"(": ")", "[": "]", "{": "}"}
CLOSE = {")": "(", "]": "[", "}": "{"}


@dataclass
class Tok:
    kind: str  # ident | lifetime | lit | punct
    text: str
    start: int
    end: int
    match: int = -1  # index of the matching delimiter, for ( [ {


def _skip_block_comment(src: str, i: int) -> int:
    depth = 0
    n = len(src)
    while i < n:
        if src.startswith("/*", i):
            depth += 1
            i += 2
        elif src.startswith("*/", i):
            depth -= 1
            i += 2
            if depth == 0:
                return i
        else:
            i += 1
    return n


def _string_end(src: str, i: int) -> int:
    """`i` points at the opening quote of a normal (escaped) string."""
    i += 1
    n = len(src)
    while i < n:
        c = src[i]
        if c == "\\":
            i += 2
            continue
        if c == '"':
            return i + 1
        i += 1
    return n


def _raw_string_end(src: str, i: int) -> int | None:
    """`i` points at `r` (after an optional `b`/`c`). Returns end or None."""
    m = re.compile(r'r(#*)"').match(src, i)
    if not m:
        return None
    hashes = m.group(1)
    close = '"' + hashes
    j = src.find(close, m.end())
    return len(src) if j < 0 else j + len(close)


def lex(src: str) -> list[Tok]:
    toks: list[Tok] = []
    i = 0
    n = len(src)
    stack: list[int] = []
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
            continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if src.startswith("/*", i):
            i = _skip_block_comment(src, i)
            continue
        # raw strings / byte strings / c strings
        if c in "bcr":
            k = i + 1 if c in "bc" and i + 1 < n and src[i + 1] == "r" else i
            if src[k] == "r":
                e = _raw_string_end(src, k)
                if e is not None:
                    toks.append(Tok("lit", src[i:e], i, e))
                    i = e
                    continue
            if c in "bc" and i + 1 < n and src[i + 1] == '"':
                e = _string_end(src, i + 1)
                toks.append(Tok("lit", src[i:e], i, e))
                i = e
                continue
            if c == "b" and i + 1 < n and src[i + 1] == "'":
                j = i + 2
                if j < n and src[j] == "\\":
                    j += 2
                while j < n and src[j] != "'":
                    j += 1
                toks.append(Tok("lit", src[i:j + 1], i, j + 1))
                i = j + 1
                continue
        if c == '"':
            e = _string_end(src, i)
            toks.append(Tok("lit", src[i:e], i, e))
            i = e
            continue
        if c == "'":
            # char literal or lifetime
            if i + 2 < n and src[i + 1] == "\\":
                j = i + 2
                while j < n and src[j] != "'":
                    j += 1
                toks.append(Tok("lit", src[i:j + 1], i, j + 1))
                i = j + 1
                continue
            if i + 2 < n and src[i + 2] == "'":
                toks.append(Tok("lit", src[i:i + 3], i, i + 3))
                i += 3
                continue
            # multi-byte char literal like '€'
            m = re.compile(r"'[^'\\\n]'").match(src, i)
            if m:
                toks.append(Tok("lit", m.group(0), i, m.end()))
                i = m.end()
                continue
            m = _IDENT.match(src, i + 1)
            if m:
                toks.append(Tok("lifetime", src[i:m.end()], i, m.end()))
                i = m.end()
                continue
            toks.append(Tok("punct", c, i, i + 1))
            i += 1
            continue
        m = _IDENT.match(src, i)
        if m:
            toks.append(Tok("ident", m.group(0), i, m.end()))
            i = m.end()
            continue
        if c.isdigit():
            m = _NUM.match(src, i)
            e = m.end()
            # don't swallow `..` ranges or method calls on integers like `1.max(2)`
            txt = src[i:e]
            if "." in txt:
                dot = txt.index(".")
                after = txt[dot + 1:dot + 2]
                if not after.isdigit():
                    e = i + dot
            toks.append(Tok("lit", src[i:e], i, e))
            i = e
            continue
        for p in _PUNCT3:
            if src.startswith(p, i):
                toks.append(Tok("punct", p, i, i + 3))
                i += 3
                break
        else:
            for p in _PUNCT2:
                if src.startswith(p, i):
                    toks.append(Tok("punct", p, i, i + 2))
                    i += 2
                    break
            else:
                toks.append(Tok("punct", c, i, i + 1))
                if c in OPEN:
                    stack.append(len(toks) - 1)
                elif c in CLOSE and stack:
                    o = stack.pop()
                    toks[o].match = len(toks) - 1
                    toks[-1].match = o
                i += 1
    return toks


def tok_at(toks: list[Tok], offset: int) -> int:
    """Index of the token starting at or containing `offset` (binary search)."""
    lo, hi = 0, len(toks)
    while lo < hi:
        mid = (lo + hi) // 2
        if toks[mid].end <= offset:
            lo = mid + 1
        else:
            hi = mid
    return lo


def split_top(toks: list[Tok], lo: int, hi: int, sep: str = ",") -> list[tuple[int, int]]:
    """Split token range [lo, hi) at top-level `sep` tokens. Returns [lo, hi) ranges."""
    out = []
    start = lo
    i = lo
    while i < hi:
        t = toks[i]
        if t.kind == "punct" and t.text in OPEN and t.match > i:
            i = t.match + 1
            continue
        if t.kind == "punct" and t.text == sep:
            out.append((start, i))
            start = i + 1
        i += 1
    if start < hi:
        out.append((start, hi))
    return out


def enclosing_groups(toks: list[Tok], idx: int, opener: str = "{") -> list[int]:
    """Indices of `opener` tokens whose group contains token `idx`, innermost first."""
    out = []
    for j in range(idx - 1, -1, -1):
        t = toks[j]
        if t.kind == "punct" and t.text == opener and t.match > idx:
            out.append(j)
    return out


def path_before(toks: list[Tok], brace: int) -> tuple[int, str] | None:
    """The `a::b::C` (optionally with turbofish-free generics skipped) ending right before `brace`.

    Returns (index of first path token, path text without spaces) or None.
    """
    j = brace - 1
    if j < 0:
        return None
    # skip generic args `<...>` directly before the brace: `Foo::<T> {` is rare; `Foo<T> {` is a type
    if toks[j].kind != "ident":
        return None
    first = j
    while first - 2 >= 0 and toks[first - 1].text == "::" and toks[first - 2].kind == "ident":
        first -= 2
    if first - 1 >= 0 and toks[first - 1].text == "::":
        first -= 1  # leading `::path`
    text = "".join(t.text for t in toks[first:brace])
    return first, text


def line_starts(src: str) -> list[int]:
    out = [0]
    for i, c in enumerate(src):
        if c == "\n":
            out.append(i + 1)
    return out


def line_of(starts: list[int], offset: int) -> int:
    import bisect

    return bisect.bisect_right(starts, offset)

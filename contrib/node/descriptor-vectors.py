#!/usr/bin/env python3
"""Extract Check/CheckUnparsable vectors from Core's descriptor_tests.cpp into a Rust table.

Usage (from the repository root, with a Dash Core checkout at CORE):

    git -C CORE show v24.0.0-rc.4:src/test/descriptor_tests.cpp > descriptor_tests.cpp
    python3 contrib/node/descriptor-vectors.py descriptor_tests.cpp dashd-rpc/src/descriptor/core_vectors.rs
"""
import re, sys
src, out = sys.argv[1], sys.argv[2]
lines = open(src, encoding='utf-8').read().split('\n')
lit = re.compile(r'"((?:[^"\\]|\\.)*)"')
FLAGS = {'DEFAULT': 0, 'RANGE': 1, 'HARDENED': 2, 'UNSOLVABLE': 4, 'SIGNABLE': 8, 'DERIVE_HARDENED': 16}
checks, unparsable = [], []
def rs(s):
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'
for no, line in enumerate(lines, 1):
    t = line.strip()
    if t.startswith('Check('):
        lits = lit.findall(t)
        prv, pub, norm = lits[0], lits[1], lits[2]
        # the flags: between the third literal and the scripts' "{{"
        after_norm = t[list(lit.finditer(t))[2].end():]
        flags_expr = after_norm[:after_norm.index('{{')].strip(' ,')
        flags = 0
        for name in re.split(r'\s*\|\s*', flags_expr):
            flags |= FLAGS[name]
        scripts_txt = after_norm[after_norm.index('{{') + 1:]
        depth, groups, cur = 0, [], None
        i = 0
        # parse {"a", "b"}, {"c"} up to the closing brace of the outer list
        body = scripts_txt
        end = 0
        d = 0
        for j, c in enumerate(body):
            if c == '{': d += 1
            elif c == '}':
                if d == 0:
                    end = j; break
                d -= 1
        body = body[:end]
        groups = [lit.findall(g) for g in re.findall(r'\{([^{}]*)\}', body)]
        checks.append((no, prv, pub, norm, flags, groups))
    elif t.startswith('CheckUnparsable('):
        lits = lit.findall(t)
        unparsable.append((no, lits[0], lits[1], lits[2]))
with open(out, 'w', encoding='utf-8') as f:
    f.write('//! Dash Core v24.0.0-rc.4 `src/test/descriptor_tests.cpp`, `descriptor_test`: every `Check` and\n')
    f.write('//! `CheckUnparsable` call, its string literals copied verbatim (mainnet parameters), with\n')
    f.write('//! the flags resolved. Extracted by contrib/node/descriptor-vectors.py; do not edit.\n\n')
    f.write('pub(super) const DEFAULT: u32 = 0;\npub(super) const RANGE: u32 = 1;\npub(super) const HARDENED: u32 = 2;\n\n')
    f.write('/// `Check(prv, pub, norm_pub, flags, scripts, ...)` at Core line `line`.\n')
    f.write('pub(super) struct Check {\n    pub line: u32,\n    pub prv: &\'static str,\n    pub pub_: &\'static str,\n    pub norm_pub: &\'static str,\n    pub flags: u32,\n    pub scripts: &\'static [&\'static [&\'static str]],\n}\n\n')
    f.write('pub(super) const CHECKS: &[Check] = &[\n')
    for no, prv, pub, norm, flags, groups in checks:
        g = ', '.join('&[' + ', '.join(rs(x) for x in grp) + ']' for grp in groups)
        f.write(f'    Check {{\n        line: {no},\n        prv: {rs(prv)},\n        pub_: {rs(pub)},\n        norm_pub: {rs(norm)},\n        flags: {flags},\n        scripts: &[{g}],\n    }},\n')
    f.write('];\n\n/// `CheckUnparsable(prv, pub, expected_error)` at Core line `.0`.\n')
    f.write('pub(super) const UNPARSABLE: &[(u32, &str, &str, &str)] = &[\n')
    for no, a, b, c in unparsable:
        f.write(f'    ({no}, {rs(a)}, {rs(b)}, {rs(c)}),\n')
    f.write('];\n')
print(len(checks), len(unparsable))

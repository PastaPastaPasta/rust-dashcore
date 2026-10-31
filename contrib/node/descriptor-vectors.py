#!/usr/bin/env python3
"""Extract Check/CheckUnparsable/CheckInferDescriptor vectors from Core's descriptor_tests.cpp into a
Rust table.

Usage (from the repository root, with a Dash Core checkout at CORE):

    git -C CORE show v24.0.0-rc.4:src/test/descriptor_tests.cpp > descriptor_tests.cpp
    python3 contrib/node/descriptor-vectors.py descriptor_tests.cpp dashd-rpc/src/descriptor/core_vectors.rs
"""
import re, sys
src, out = sys.argv[1], sys.argv[2]
lines = open(src, encoding='utf-8').read().split('\n')
lit = re.compile(r'"((?:[^"\\]|\\.)*)"')
FLAGS = {'DEFAULT': 0, 'RANGE': 1, 'HARDENED': 2, 'UNSOLVABLE': 4, 'SIGNABLE': 8, 'DERIVE_HARDENED': 16}
checks, unparsable, infer = [], [], []
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
        # the expected key paths: the set after the output type, ONLY_EMPTY ({{}}) by default
        rest = scripts_txt[end + 1:]
        m = re.search(r'(?:OutputType::\w+|std::nullopt)\s*,\s*\{(.*)\}\s*\)\s*;', rest)
        if m:
            paths = [[int(x.rstrip('UL'), 0) for x in re.findall(r'0x[0-9A-Fa-f]+UL|\d+', g)]
                     for g in re.findall(r'\{([^{}]*)\}', m.group(1))]
        else:
            paths = [[]]
        checks.append((no, prv, pub, norm, flags, groups, paths))
    elif t.startswith('CheckInferDescriptor('):
        # (script, expected, {provider scripts}, {{pubkey, origin}...})
        lits = lit.findall(t)
        tail = t[list(lit.finditer(t))[1].end():]
        scripts_part = tail[:tail.index('}') + 1]
        n = len(lit.findall(scripts_part))
        rest = lits[2 + n:]
        infer.append((no, lits[0], lits[1], lits[2:2 + n], list(zip(rest[0::2], rest[1::2]))))
    elif t.startswith('CheckUnparsable('):
        lits = lit.findall(t)
        unparsable.append((no, lits[0], lits[1], lits[2]))
with open(out, 'w', encoding='utf-8') as f:
    f.write('//! Dash Core v24.0.0-rc.4 `src/test/descriptor_tests.cpp`, `descriptor_test`: every `Check`,\n')
    f.write('//! `CheckUnparsable` and `CheckInferDescriptor` call, its string literals copied verbatim\n')
    f.write('//! (mainnet parameters), with the flags and key paths resolved. Extracted by\n')
    f.write('//! contrib/node/descriptor-vectors.py; do not edit.\n\n')
    f.write('pub(super) const DEFAULT: u32 = 0;\npub(super) const RANGE: u32 = 1;\npub(super) const HARDENED: u32 = 2;\n\n')
    f.write('/// `Check(prv, pub, norm_pub, flags, scripts, ...)` at Core line `line`.\n')
    f.write('pub(super) struct Check {\n    pub line: u32,\n    pub prv: &\'static str,\n    pub pub_: &\'static str,\n    pub norm_pub: &\'static str,\n    pub flags: u32,\n    pub scripts: &\'static [&\'static [&\'static str]],\n    /// The key origins\' paths the expansions give (`paths`, `ONLY_EMPTY` by default).\n    pub paths: &\'static [&\'static [u32]],\n}\n\n')
    f.write('pub(super) const CHECKS: &[Check] = &[\n')
    for no, prv, pub, norm, flags, groups, paths in checks:
        g = ', '.join('&[' + ', '.join(rs(x) for x in grp) + ']' for grp in groups)
        ps = ', '.join('&[' + ', '.join(f'0x{x:08x}' if x >> 31 else str(x) for x in pa) + ']' for pa in paths)
        f.write(f'    Check {{\n        line: {no},\n        prv: {rs(prv)},\n        pub_: {rs(pub)},\n        norm_pub: {rs(norm)},\n        flags: {flags},\n        scripts: &[{g}],\n        paths: &[{ps}],\n    }},\n')
    f.write('];\n\n/// `CheckUnparsable(prv, pub, expected_error)` at Core line `.0`.\n')
    f.write('pub(super) const UNPARSABLE: &[(u32, &str, &str, &str)] = &[\n')
    for no, a, b, c in unparsable:
        f.write(f'    ({no}, {rs(a)}, {rs(b)}, {rs(c)}),\n')
    f.write('];\n\n/// `CheckInferDescriptor(script_hex, expected_desc, hex_scripts, origin_pubkeys)` at Core line\n')
    f.write('/// `.0`; each origin is `fingerprint/path`, or empty for a key with no origin.\n')
    f.write('#[allow(clippy::type_complexity)]\n')
    f.write('pub(super) const INFER: &[(u32, &str, &str, &[&str], &[(&str, &str)])] = &[\n')
    for no, script, want, scripts, origins in infer:
        sc = ', '.join(rs(x) for x in scripts)
        og = ', '.join(f'({rs(a)}, {rs(b)})' for a, b in origins)
        f.write(f'    ({no}, {rs(script)}, {rs(want)}, &[{sc}], &[{og}]),\n')
    f.write('];\n')
print(len(checks), len(unparsable), len(infer))

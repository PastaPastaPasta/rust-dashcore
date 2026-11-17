#!/usr/bin/env python3
"""Extract Core's bip32_tests.cpp vectors (test1-test4 chains, TEST5) into a Rust table.

Usage (from the repository root, with a Dash Core checkout at CORE):

    git -C CORE show v24.0.0-rc.4:src/test/bip32_tests.cpp > bip32_tests.cpp
    python3 contrib/node/bip32-vectors.py bip32_tests.cpp dashd-rpc/src/key_io/bip32_vectors.rs
"""
import re, sys
src, out = sys.argv[1], sys.argv[2]
text = open(src, encoding='utf-8').read()
chains = []
for name, body in re.findall(r'TestVector (test\d) =(.*?);', text, re.S):
    steps = re.findall(r'\(\s*"(xpub\w+)",\s*"(xprv\w+)",\s*(0x[0-9a-fA-F]+|\d+)\)', body)
    line = text[:text.index(f'TestVector {name} =')].count('\n') + 1
    chains.append((name, line, [(p, k, int(c, 0)) for p, k, c in steps]))
t5 = re.search(r'TEST5 = \{(.*?)\};', text, re.S).group(1)
t5 = re.findall(r'"(\w+)"', t5)
with open(out, 'w') as f:
    f.write('//! Dash Core v24.0.0-rc.4 `src/test/bip32_tests.cpp`: the derivation chains of `test1`-`test4`\n')
    f.write('//! (each step\'s xpub, xprv and the child derived from it) and `TEST5`, copied verbatim.\n')
    f.write('//! Extracted by contrib/node/bip32-vectors.py; do not edit.\n\n')
    f.write('/// One derivation step: the key\'s xpub and xprv, and the child derived from it.\n')
    f.write('pub(super) type Step = (&\'static str, &\'static str, u32);\n\n')
    f.write('/// `(name, Core line, steps)`.\n')
    f.write('pub(super) const CHAINS: &[(&str, u32, &[Step])] = &[\n')
    for name, line, steps in chains:
        f.write(f'    (\n        "{name}",\n        {line},\n        &[\n')
        for p, k, c in steps:
            f.write(f'            (\n                "{p}",\n                "{k}",\n                0x{c:08x},\n            ),\n')
        f.write('        ],\n    ),\n')
    f.write('];\n\n/// `TEST5`: neither an xprv nor an xpub.\n')
    f.write('pub(super) const TEST5: &[&str] = &[\n')
    for s in t5:
        f.write(f'    "{s}",\n')
    f.write('];\n')
print([(n, len(s)) for n, _, s in chains], len(t5))

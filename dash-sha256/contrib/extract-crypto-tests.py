#!/usr/bin/env python3
"""Extracts the SHA-256 vectors of Dash Core's src/test/crypto_tests.cpp (`sha256_testvectors`).

Usage: extract-crypto-tests.py <dash-core-git-dir> <commit> > tests/data/core-crypto-tests.json

Each `TestSHA256(input, digest)` call becomes one entry with its source line. Inputs are string
literals, except `std::string(1000000, 'a')` (a repeat) and `test1` (Core's `LongTestString()`,
which the test regenerates).
"""
import json
import re
import subprocess
import sys

repo, commit = sys.argv[1], sys.argv[2]
commit = subprocess.check_output(['git', '-C', repo, 'rev-parse', commit], text=True).strip()
path = 'src/test/crypto_tests.cpp'
src = subprocess.check_output(['git', '-C', repo, 'show', f'{commit}:{path}'], text=True)

start = src.index('BOOST_AUTO_TEST_CASE(sha256_testvectors)')
body = src[start : src.index('\n}\n', start)]
first_line = src[:start].count('\n') + 1

vectors = []
for m in re.finditer(r'TestSHA256\((.*?),\s*"([0-9a-f]{64})"\);', body, re.S):
    arg = m.group(1).strip()
    line = first_line + body[: m.start()].count('\n')
    if (s := re.fullmatch(r'"((?:[^"\\]|\\.)*)"', arg)):
        assert '\\' not in s.group(1), arg
        inp = {'text': s.group(1)}
    elif (r := re.fullmatch(r"std::string\((\d+), '(.)'\)", arg)):
        inp = {'repeat': r.group(2), 'count': int(r.group(1))}
    elif arg == 'test1':
        inp = {'generator': 'LongTestString'}
    else:
        sys.exit(f'unhandled input at line {line}: {arg}')
    vectors.append({'line': line, 'input': inp, 'digest': m.group(2)})

json.dump({'core_commit': commit, 'file': path, 'test': 'sha256_testvectors', 'vectors': vectors},
          sys.stdout, indent=2)
print()

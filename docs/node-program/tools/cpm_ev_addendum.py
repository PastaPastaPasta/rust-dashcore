#!/usr/bin/env python3
"""EV addendum (designs/EV-addendum-SB0.md, rev1; LQ1m added in rev6) CPM runs over SB0's committed overlay (run A).

Pinned inputs: cpm.py and cpm_sb0_rev1.py beside this file, checked by git blob hash; a mismatch
stops the run (re-pin deliberately). Model conventions are cpm.py's: "i:X" constrains only the
merge (>= X + 0.5); merged rows are not modelled as done (week 0 = 2026-10-08).

  P   PLAN edges + the addendum's: EV0 `CS0, i:BS6a` (PLAN's row, unchanged), LQ1a + i:SB7,
      LQ2 + SB8 (DEC-109 (4)), and (rev6, Opus r7 O5) the new row LQ1m (S, LLMQ, deps LQ1a)
      feeding CH7b, with LQ1b + i:LQ1m. This is the plan of record.
  P1  P with EV0 S -> M;  P2  P with EV0 S -> L;  P3  P with LQ1a M -> L (rev1 adds tests to LQ1a).
  P4  P with LQ1m S -> M (rev6: its float to CH7b).
  X   counterfactual (Sol r1 M4): EV0 `CS0, BS6a` as a merge edge (rev0's wrong row text).
  X1  X with EV0 S -> M.

Usage: python3 cpm_ev_addendum.py
"""
import contextlib, copy, hashlib, importlib.util, io, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
PIN = {'cpm.py': 'c19f918ff067be32eb3a34d86caf38d84f6bbee7',
       'cpm_sb0_rev1.py': 'd581db4271d69766e58a3e00c9bd64a050480431'}

def blob(path):
    data = open(path, 'rb').read()
    return hashlib.sha1(b'blob %d\0' % len(data) + data).hexdigest()

for name, want in PIN.items():
    got = blob(os.path.join(HERE, name))
    if got != want:
        sys.exit(f"{name}: blob {got}, pinned {want}")

spec = importlib.util.spec_from_file_location('ov', os.path.join(HERE, 'cpm_sb0_rev1.py'))
ov = importlib.util.module_from_spec(spec)
argv, sys.argv = sys.argv, [sys.argv[0]]
with contextlib.redirect_stdout(io.StringIO()):   # the overlay prints its own runs on load
    spec.loader.exec_module(ov)
sys.argv = argv

def edit(T, row, size=None, deps=None, add=()):
    s, d, r, old = T[row]
    T[row] = (size or s, d, r, (deps if deps is not None else old) + list(add))

def plan():
    T = ov.build(False)
    assert T['EV0'][3] == ['CS0', 'i:BS6a'], T['EV0']
    edit(T, 'LQ1a', add=['i:SB7'])
    edit(T, 'LQ2', add=['SB8'])
    assert 'LQ1m' not in T
    T['LQ1m'] = ('S', '-', 'LLMQ', ['LQ1a'])
    edit(T, 'LQ1b', add=['i:LQ1m'])
    edit(T, 'CH7b', add=['LQ1m'])
    return T

RUNS = {}
RUNS['P'] = plan()
RUNS['P1'] = plan(); edit(RUNS['P1'], 'EV0', size='M')
RUNS['P2'] = plan(); edit(RUNS['P2'], 'EV0', size='L')
RUNS['P3'] = plan(); edit(RUNS['P3'], 'LQ1a', size='L')
RUNS['P4'] = plan(); edit(RUNS['P4'], 'LQ1m', size='M')
RUNS['X'] = plan(); edit(RUNS['X'], 'EV0', deps=['CS0', 'BS6a'])
RUNS['X1'] = copy.deepcopy(RUNS['X']); edit(RUNS['X1'], 'EV0', size='M')

ROWS = ['EV0', 'EV2a', 'LQ1a', 'LQ1m', 'SB7', 'SB8', 'SB11', 'SB19', 'CH7b', 'REL11']
print('run  ' + ' '.join(f'{r:>6s}' for r in ROWS))
for name, T in RUNS.items():
    c = ov.run(T)
    print(f'{name:4s} ' + ' '.join(f'{c.ef(r):6.1f}' for r in ROWS))
for name in ('P', 'X'):
    c = ov.run(RUNS[name]); c.ef('REL11')
    print(f'{name} REL11 chain: {c.chain("REL11")}')

#!/usr/bin/env python3
"""Plan-of-record critical-path model after SB0 (PLAN §3 "SB0 recheck"; DEC-119, 2026-10-09).

cpm.py keeps the A1-rev1 table unchanged: cpm_ev_addendum.py pins its blob, and SB0 §13.4 and
cpm_sb0_rev1.py are stated against it. This file applies the PLAN integration's rows on top:
  - SB0 rev6.1 §13.1/§13.2: SB1-SB19 and SB12a; CH1 -> CH1-node; EV4 -> EV4-serve; P2b + SB6;
    LK1a + SB8; IX1 + SB9; M5_exit + SB18 (SB0 §13.4 run A);
  - DEC-103's CH2 -> CH2a/b/c (SB0 §13.4 run B): CH2's consumers take CH2b, SB10 takes CH2a;
  - the EV addendum (rev5 726ff9a1, approval pending) §5: LQ1a + i:SB7, LQ2 + SB8; EV0 keeps
    PLAN's `CS0, i:BS6a`;
  - P2a -> P2a-core (M) + P2a-net (S), both on SB6 (PLAN §4.5); P2a-net merges after P2a-core;
  - RP-V24 (DEC-82) as an interface edge of P2a-core and ND1;
  - the rows PLAN marks DROPPED (BS6c and CH7c by DEC-82, O13b by DEC-83) and their edges removed.
    cpm.py and SB0 §13.4 still carry them; O13b is what held M5_exit at 41.5.
Outside the model: SB18b (M6; WL2a is not modelled), SB-ST (contingent), #902/SB0 Q1 and #1104
(external), WL2a. Conventions are cpm.py's: S/M/L = 1/1.5/3 weeks, review +1 (+0.5 OTH), design
steps from week 1, "i:X" constrains only the merge (>= X + 0.5), week 0 = 2026-10-08, no
progress state (merged rows are not modelled as done).

Runs:
  plan   the plan of record (PLAN §4 after the SB0 integration)
  sb0    plan with the dropped rows kept, for comparison with SB0 §13.4 run B (same conventions)
  merge  plan with SB4's i:CS2/i:CS3 and CH1-node's i:SB5 as merge edges (SB0 §13.4 run C's analogue)
  lever  plan with SB6 on i:P1 instead of P1 (a sensitivity; not adopted)

Usage: python3 cpm_plan.py
"""
import copy, hashlib, importlib.util, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
PIN = 'c19f918ff067be32eb3a34d86caf38d84f6bbee7'   # cpm.py (76ad33ea model + the O17 recheck)

data = open(os.path.join(HERE, 'cpm.py'), 'rb').read()
got = hashlib.sha1(b'blob %d\0' % len(data) + data).hexdigest()
if got != PIN:
    sys.exit(f"cpm.py: blob {got}, pinned {PIN}")
spec = importlib.util.spec_from_file_location('cpm', os.path.join(HERE, 'cpm.py'))
cpm = importlib.util.module_from_spec(spec); spec.loader.exec_module(cpm)
BASE = copy.deepcopy(cpm.T)

NEW = {
    'SB1':  ('S', '-', 'OTH',   []),
    'SB2':  ('S', '-', 'OTH',   ['SB1']),
    'SB3':  ('S', '-', 'OTH',   ['SB1']),
    'SB4':  ('L', 'D', 'STATE', ['CS0', 'i:CS2', 'i:CS3', 'i:BS5']),
    'SB5':  ('M', '-', 'STATE', ['SB4']),
    'SB6':  ('L', '-', 'P2P',   ['P1', 'O17']),
    'SB7':  ('M', '-', 'CON',   ['EV0']),
    'SB8':  ('M', '-', 'LLMQ',  ['SB7', 'LQ1a', 'BS12']),
    'SB9':  ('M', '-', 'P2P',   ['CS0']),
    'SB10': ('M', '-', 'STATE', ['ST3a', 'CH2a']),
    'SB11': ('L', '-', 'LLMQ',  ['SB5', 'SB8', 'SB10', 'LQ1b']),
    'SB12a': ('S', '-', 'CON',  ['CS0', 'SB7', 'SB9']),
    'SB12': ('M', '-', 'CON',   ['SB4', 'SB9', 'SB12a']),
    'SB13': ('M', '-', 'STATE', ['SB10']),
    'SB14': ('L', '-', 'STATE', ['SB5', 'SB12', 'SB13']),             # + #1104 (external)
    'SB15': ('M', '-', 'LLMQ',  ['SB11', 'SB14']),
    'SB16': ('L', '-', 'P2P',   ['SB6', 'SB14']),                     # + #902 or SB0 Q1 (external)
    'SB17': ('M', '-', 'OTH',   ['SB14', 'SB15', 'SB16']),
    'SB18': ('M', '-', 'STATE', ['SB15', 'SB16', 'ND1']),
    'SB19': ('S', '-', 'LLMQ',  ['SB11', 'EV4-serve']),
    'CH1-node': ('M', 'D', 'STATE', ['ST1', 'SB4', 'i:SB5']),
    'EV4-serve': ('M', '-', 'CON', ['EV2b', 'BS8', 'SB7']),
    'P2a-core': ('M', 'D2', 'P2P', ['P1', 'O17', 'i:SB6', 'i:CH1-node', 'i:RP-V24']),
    'P2a-net':  ('S', 'D2', 'P2P', ['P1', 'O17', 'i:SB6', 'i:P2a-core']),
    'RP-V24': ('M', '-', 'CON', ['O2a', 'BS5', 'BS6a']),
    'O2a-CO': ('S', '-', 'CON', ['O2a']),
    'CH2a': ('M', 'D2', 'STATE', ['ST1', 'CS5a', 'CS0']),
    'CH2b': ('L', '-', 'STATE', ['CH2a', 'ST2', 'ST3a', 'i:SB5']),
    'CH2c': ('M', '-', 'CON', ['CH2b', 'PF0', 'O2a-CO']),
}
ADD = {'P2b': ['SB6'], 'LK1a': ['SB8'], 'IX1': ['SB9'], 'M5_exit': ['SB18'],
       'LQ1a': ['i:SB7'], 'LQ2': ['SB8'], 'ND1': ['i:RP-V24']}
RENAME = {'CH1': ['CH1-node'], 'EV4': ['EV4-serve'], 'CH2': ['CH2b'],
          'P2a': {'P2b': ['P2a-core'], 'P5a': ['P2a-core'], 'SP0': ['P2a-core'],
                  'P3': ['P2a-core', 'P2a-net']}}
DROPPED = {'BS6c', 'CH7c', 'O13b'}
M2 = ['CH1-node', 'P2a-core', 'P2a-net', 'P2b', 'O13a', 'ND1', 'SB1', 'SB4', 'SB5', 'SB6']


def plan(drops=True):
    T = copy.deepcopy(BASE)
    for old in RENAME:
        del T[old]
    if drops:
        for old in DROPPED:
            del T[old]
    T.update(NEW)
    for k, (s, d, r, deps) in list(T.items()):
        out = []
        for x in deps:
            iface, name = x.startswith('i:'), x[2:] if x.startswith('i:') else x
            if drops and name in DROPPED:
                continue
            if name in RENAME:
                to = RENAME[name]
                to = to[k] if isinstance(to, dict) else to
                out += [('i:' if iface else '') + t for t in to]
            else:
                out.append(x)
        T[k] = (s, d, r, out + ADD.get(k, []))
    return T


def edit(T, row, deps):
    s, d, r, _ = T[row]
    T[row] = (s, d, r, deps)


def run(T):
    cpm.T = T; cpm.EF.clear(); cpm.PRED.clear()
    return cpm


RUNS = {'plan': plan(), 'sb0': plan(drops=False), 'merge': plan(), 'lever': plan()}
edit(RUNS['merge'], 'SB4', ['CS0', 'CS2', 'CS3', 'i:BS5'])
edit(RUNS['merge'], 'CH1-node', ['ST1', 'SB4', 'SB5'])
edit(RUNS['lever'], 'SB6', ['i:P1', 'O17'])

ROWS = ['P1', 'SB1', 'SB4', 'SB5', 'SB6', 'CH1-node', 'P2a-core', 'P2a-net', 'P2b', 'O13a', 'ND1',
        'O11', 'O16', 'SB7', 'SB9', 'SB10', 'SB12a', 'SB12', 'SB8', 'SB11', 'SB13', 'SB14',
        'SB15', 'SB16', 'SB17', 'SB18', 'SB19', 'EV0', 'EV2a', 'LQ1a', 'CH2b', 'CH3b', 'CH7b',
        'REL11', 'M4_soak', 'M5_exit', 'RL4']

if __name__ == '__main__':
    print('row       ' + ' '.join(f'{n:>6s}' for n in RUNS))
    res = {}
    for n, T in RUNS.items():
        c = run(T)
        res[n] = {r: c.ef(r) for r in ROWS}
        res[n]['M2'] = max(c.ef(t) for t in M2)
    for r in ROWS[:11] + ['M2'] + ROWS[11:]:
        print(f'{r:9s} ' + ' '.join(f'{res[n][r]:6.1f}' for n in RUNS))
    for n in RUNS:
        c = run(RUNS[n])
        m = max(M2, key=lambda t: c.ef(t))
        for goal in (m, 'REL11', 'M5_exit'):
            c.ef(goal)
            print(f'{n}: {goal} wk {c.EF[goal]:.1f}: {c.chain(goal)}')

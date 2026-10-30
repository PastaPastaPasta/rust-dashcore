#!/usr/bin/env python3
"""SB0 rev1 overlay on the committed critical-path model (docs/node-program/tools/cpm.py).

Run A applies only SB0 rev1's rows and edges to the committed table (the same method as Sol r1's
sensitivity check). Run B additionally applies DEC-103's CH2 -> CH2a/b/c split (CH2's consumers
take CH2b). Model conventions are unchanged: design steps start at week 1 (this assumes SB0 is
approved by then, as every design step in the model does), "i:X" constrains only the merge.
Run C is run A with SB4's CS2/CS3 and CH1-node's SB5 as merge edges (no coding against stubs).

Usage: python3 cpm_sb0_rev1.py [path to cpm.py]   (default: cpm.py beside this file)
"""
import importlib.util, os, sys, copy

CPM = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cpm.py')
spec = importlib.util.spec_from_file_location("cpm", CPM)
cpm = importlib.util.module_from_spec(spec); spec.loader.exec_module(cpm)
ORIG = copy.deepcopy(cpm.T)

SB0 = {
    'SB1':  ('S', '-', 'OTH',   []),
    'SB2':  ('S', '-', 'OTH',   ['SB1']),
    'SB3':  ('S', '-', 'OTH',   ['SB1']),
    'SB4':  ('L', 'D', 'STATE', ['CS0', 'i:CS2', 'i:CS3', 'i:BS5']),
    'SB5':  ('M', '-', 'STATE', ['SB4']),
    'CH1':  ('M', 'D', 'STATE', ['ST1', 'SB4', 'i:SB5']),            # CH1-node
    'SB6':  ('L', '-', 'P2P',   ['P1', 'O17']),
    'P2a':  ('M', 'D2', 'P2P',  ['P1', 'O17', 'i:SB6', 'i:CH1']),    # P2a-node
    'P2b':  ('M', '-', 'P2P',   ['P2a', 'SB6']),
    'SB7':  ('M', '-', 'CON',   ['EV0']),
    'SB8':  ('M', '-', 'LLMQ',  ['SB7', 'LQ1a', 'BS12']),
    'SB9':  ('M', '-', 'P2P',   ['CS0']),
    'SB10': ('M', '-', 'STATE', ['ST3a', 'CH2']),                     # CH2 = CH2a in run B
    'SB11': ('L', '-', 'LLMQ',  ['SB5', 'SB8', 'SB10', 'LQ1b']),
    'SB12': ('M', '-', 'CON',   ['SB4', 'SB9']),
    'SB13': ('M', '-', 'STATE', ['SB10']),
    'SB14': ('L', '-', 'STATE', ['SB5', 'SB12', 'SB13']),
    'SB15': ('M', '-', 'LLMQ',  ['SB11', 'SB14']),
    'SB16': ('L', '-', 'P2P',   ['SB6', 'SB14']),                     # + #902 or Q1 (external, unmodelled)
    'SB17': ('M', '-', 'OTH',   ['SB14', 'SB15', 'SB16']),
    'SB18': ('M', '-', 'STATE', ['SB15', 'SB16', 'ND1']),
    'SB19': ('S', '-', 'LLMQ',  ['SB11', 'EV4']),
    'EV4':  ('M', '-', 'CON',   ['EV2b', 'BS8', 'SB7']),              # EV4-serve
}
ADD = {'LK1a': ['SB8'], 'IX1': ['SB9'], 'CH2': ['i:SB5'],           # CH2's new edge is CH2b's
       'M5_exit': ['SB18']}                                           # M5's SPV smoke

def build(split_ch2, merge_edges=False):
    T = copy.deepcopy(ORIG)
    T.update(SB0)
    if merge_edges:   # run C: CS2/CS3 and SB5 as merge edges (rev0-style; Sol r1's sensitivity)
        T['SB4'] = ('L', 'D', 'STATE', ['CS0', 'CS2', 'CS3', 'i:BS5'])
        T['CH1'] = ('M', 'D', 'STATE', ['ST1', 'SB4', 'SB5'])
    for k, extra in ADD.items():
        s, d, r, deps = T[k]; T[k] = (s, d, r, deps + extra)
    if split_ch2:
        T['O2a_CO'] = ('S', '-', 'CON', ['O2a'])
        T['CH2a'] = ('M', 'D2', 'STATE', ['ST1', 'CS5a', 'CS0'])
        T['CH2b'] = ('L', '-', 'STATE', ['CH2a', 'ST2', 'ST3a', 'i:SB5'])
        T['CH2c'] = ('M', '-', 'CON', ['CH2b', 'PF0', 'O2a_CO'])
        del T['CH2']
        for k, (s, d, r, deps) in list(T.items()):
            new = []
            for x in deps:
                if x == 'CH2': new.append('CH2a' if k == 'SB10' else 'CH2b')
                elif x == 'i:CH2': new.append('i:CH2b')
                else: new.append(x)
            T[k] = (s, d, r, new)
    return T

def run(T):
    cpm.T = T; cpm.EF.clear(); cpm.PRED.clear()
    return cpm

M2 = ['CH1', 'P2a', 'P2b', 'O13a', 'ND1', 'SB1', 'SB4', 'SB5', 'SB6']
GOALS = ['SB4', 'SB5', 'CH1', 'SB6', 'P2a', 'P2b', 'O13a', 'ND1', 'CH3b', 'EV2b', 'LQ3', 'CH7b', 'REL11',
         'SB11', 'SB14', 'SB16', 'SB17', 'SB18', 'M4_soak', 'M5_exit', 'RL4']

def report(name, T, goals, m2):
    c = run(T)
    print(f"== {name}")
    for g in goals:
        if g in T:
            c.ef(g); print(f"{g:8s} wk {c.EF[g]:5.1f}   {c.chain(g)}")
    m = max(m2, key=lambda t: c.ef(t)); print(f"M2 exit  wk {c.EF[m]:5.1f}   (max of {', '.join(m2)}; limited by {m})")

report('baseline (committed cpm.py)', copy.deepcopy(ORIG),
       ['CH1', 'P2a', 'P2b', 'O13a', 'ND1', 'CH3b', 'CH7b', 'REL11', 'M4_soak', 'M5_exit', 'RL4'],
       ['CH1', 'P2a', 'P2b', 'O13a', 'ND1'])
report('run A: committed + SB0 rev1', build(False), GOALS, M2)
report('run B: run A + DEC-103 CH2a/b/c', build(True), GOALS + ['CH2a', 'CH2b'], M2)
report('run C: run A with CS2/CS3 and SB5 as merge edges', build(False, True), GOALS, M2)

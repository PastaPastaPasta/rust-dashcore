#!/usr/bin/env python3
"""Critical-path model behind PLAN.md §3 (A1-rev1, 2026-10-08).

Calendar weeks from 2026-10-08. Duration = size + review latency (+ calendar time for soaks).
Sizes S=1, M=1.5, L=3 agent-weeks (midpoints). Review+fix: CON/STATE/LLMQ/P2P +1.0, OTH +0.5.
Design steps (D=1, D2=2 weeks) start at DESIGN_START, in parallel with predecessors.
Deps: plain = merge dependency; "i:X" = interface dependency (overlaps; merge >= X + 0.5).
Run: python3 cpm.py   (prints the earliest finish and the critical chain for key milestones)
"""
SZ = {"S": 1.0, "M": 1.5, "L": 3.0}
DS = {"-": 0.0, "D": 1.0, "D2": 2.0}
RV = {"CON": 1.0, "STATE": 1.0, "LLMQ": 1.0, "P2P": 1.0, "OTH": 0.5}
DESIGN_START = 1.0
CAL = {'devnet_soak': 2.0, 'RL4': 4.285714285714286, 'M4_soak': 1.0}
T = {
    'AM1': ('L', '-', 'P2P', ['P5a', 'SP0', 'EV2b', 'i:P4', 'i:CH7a']),
    'AR1': ('M', '-', 'OTH', []),
    'AR2': ('M', '-', 'OTH', []),
    'AR3': ('M', '-', 'OTH', ['AR1', 'AR2']),
    'B1': ('M', '-', 'OTH', []),
    'BS10': ('M', '-', 'P2P', ['BS9a', 'BS9b']),
    'BS11a': ('L', 'D', 'LLMQ', ['BS11a_t']),
    'BS11a_acc': ('S', '-', 'LLMQ', ['BS11a', 'O2a']),
    'BS11a_t': ('S', '-', 'LLMQ', ['BS3']),
    'BS11b': ('M', '-', 'LLMQ', ['BS11a_acc']),
    'BS12': ('M', '-', 'LLMQ', ['O2a']),
    'BS13': ('S', '-', 'CON', ['BS4a']),
    'BS15': ('S', '-', 'CON', []),
    'BS16': ('S', '-', 'OTH', ['BS5']),
    'BS2': ('S', '-', 'OTH', []),
    'BS3': ('S', '-', 'OTH', []),
    'BS4a': ('M', '-', 'CON', ['BS3']),
    'BS4b': ('S', '-', 'CON', ['BS4a', 'O1', 'O3']),
    'BS5': ('L', '-', 'CON', ['BS4a', 'O1']),
    'BS6a': ('M', '-', 'CON', ['BS4a', 'BS11a_t']),
    'BS6b': ('M', '-', 'CON', ['BS6a', 'O2a']),
    'BS6c': ('S', '-', 'CON', ['BS6a']),  # O17/DEC-37 (EVO-14)
    'BS7': ('M', '-', 'P2P', ['BS4a']),
    'BS8': ('L', '-', 'LLMQ', ['BS6a', 'BS7', 'BS11a_t']),
    'BS9a': ('M', '-', 'LLMQ', ['BS8']),
    'BS9b': ('M', '-', 'P2P', ['BS8']),
    'CH1': ('L', 'D', 'STATE', ['ST1', 'i:BS5', 'i:CS2', 'i:CS3']),
    'CH2': ('L', 'D2', 'STATE', ['ST1', 'ST2', 'ST3a', 'CS5a']),
    'CH3a': ('L', 'D', 'CON', ['CH1', 'CH2', 'CS1', 'CS3', 'CS4', 'CS5b', 'CS6', 'O9a']),
    'CH3b': ('L', '-', 'STATE', ['CH3a']),
    'CH4': ('L', '-', 'STATE', ['CH3a', 'PF1']),
    'CH5': ('M', '-', 'STATE', ['CH3b', 'ST2']),
    'CH6a': ('M', '-', 'STATE', ['CH5']),
    'CH7a': ('L', 'D', 'CON', ['CH3a', 'EV3', 'EV4', 'i:CH3b', 'i:O2b']),
    'CH7b': ('L', '-', 'CON', ['CH7a', 'LQ2', 'LQ3', 'BS11a_acc']),
    'CH7c': ('S', '-', 'CON', ['CH7b', 'EV4']),  # O17/DEC-37 (DKG-08)
    'CJ0': ('M', '-', 'CON', ['BS9b']),
    'CJ1': ('L', '-', 'P2P', ['CJ0', 'AM1', 'MP1a']),
    'CS0': ('M', 'D', 'CON', ['BS3', 'i:BS4a']),
    'CS1': ('L', 'D', 'CON', []),
    'CS2': ('L', 'D', 'CON', ['O1']),
    'CS3': ('M', '-', 'CON', ['CS0', 'BS4a']),
    'CS4': ('M', '-', 'CON', ['BS5', 'CS0']),
    'CS5a': ('S', '-', 'CON', ['CS0']),
    'CS5b': ('M', '-', 'CON', ['CS3']),
    'CS6': ('M', '-', 'CON', ['CS1', 'PF1']),
    'DK1': ('L', 'D', 'LLMQ', ['LQ4b', 'QM1a', 'P7']),
    'EV0': ('S', '-', 'CON', ['CS0', 'i:BS6a']),
    'EV1': ('L', 'D', 'CON', ['EV0', 'CS3', 'BS6a', 'i:BS6c']),
    'EV2a': ('L', 'D2', 'CON', ['EV0']),
    'EV2b': ('L', '-', 'CON', ['EV2a', 'EV1', 'PF1']),
    'EV3': ('L', '-', 'CON', ['EV2b']),
    'EV4': ('M', '-', 'CON', ['EV2b', 'BS8']),
    'GS1': ('M', '-', 'CON', ['GV1b', 'AM1']),
    'GV1a': ('M', '-', 'CON', ['EV2b']),
    'GV1b': ('L', 'D', 'CON', ['GV1a', 'IX1', 'P5a', 'i:P4']),
    'IM1a': ('S', '-', 'OTH', []),
    'IX1': ('L', '-', 'STATE', ['CH3b']),
    'IX2': ('L', '-', 'STATE', ['IX1', 'MP1a']),
    'LK1a': ('M', '-', 'LLMQ', ['LQ3', 'CH7b', 'P5a', 'SP0']),
    'LK1b': ('M', '-', 'LLMQ', ['LQ3', 'MP1a', 'P6a']),
    'LK1c': ('M', '-', 'LLMQ', ['SG1a', 'LK1a', 'LK1b']),
    'LQ1a': ('M', 'D', 'LLMQ', ['EV2a', 'BS5']),
    'LQ1b': ('L', '-', 'LLMQ', ['LQ1a', 'i:EV2b']),
    'LQ2': ('L', '-', 'LLMQ', ['LQ1b', 'BS8']),
    'LQ3': ('M', '-', 'LLMQ', ['LQ2', 'BS11b']),
    'LQ4a': ('L', 'D', 'LLMQ', ['LQ1b', 'BS9a', 'BS11b', 'BS12', 'O17']),
    'LQ4b': ('L', '-', 'LLMQ', ['LQ4a']),
    'LQ5': ('M', '-', 'LLMQ', ['LQ3']),
    'M4_soak': ('S', '-', 'OTH', ['O15', 'LK1a']),
    'M5_exit': ('S', '-', 'OTH', ['P4', 'P5', 'P6a', 'P6b', 'SP1', 'MP1b', 'MP2', 'MN1', 'IX2', 'ZQ1', 'RP1b', 'RP2b', 'RP3a', 'GV1b', 'CJ0', 'LK1b', 'O13b', 'O14', 'O12a', 'IM1a', 'M4_soak']),
    'MN0': ('M', '-', 'CON', ['CH3a']),
    'MN1': ('L', '-', 'CON', ['MP1a', 'CH7b', 'GV1b', 'LQ2']),
    'MP1a': ('L', 'D', 'CON', ['CH7b']),
    'MP1b': ('M', '-', 'STATE', ['MP1a']),
    'MP2': ('M', '-', 'OTH', ['MP1a']),
    'ND1': ('M', '-', 'STATE', ['AR1', 'CH1']),
    'O1': ('M', '-', 'CON', []),
    'O10': ('L', '-', 'CON', ['O9a', 'MN0']),
    'O11': ('M', '-', 'OTH', ['ND1', 'RP0']),
    'O12a': ('M', '-', 'LLMQ', ['O11', 'O16']),
    'O12b': ('M', '-', 'LLMQ', ['O12a', 'AM1', 'DK1', 'SG1b', 'LK1c', 'QM1b']),
    'O13a': ('M', '-', 'P2P', ['P2b']),
    'O13b': ('L', '-', 'P2P', ['O13a', 'P6a', 'P6b', 'P5']),
    'O14': ('M', '-', 'OTH', ['RP1a']),
    'O15': ('S', '-', 'STATE', ['CH7b', 'P3', 'P5a', 'RP1a', 'RP2a']),
    'O16': ('M', '-', 'OTH', ['ND1']),
    'O17': ('M', '-', 'CON', []),
    'O2a': ('L', '-', 'CON', ['O1']),
    'O2b': ('L', 'D', 'CON', ['O2a', 'i:CH3a']),
    'O3': ('M', '-', 'OTH', ['O1']),
    'O4': ('M', '-', 'CON', []),
    'O7': ('M', 'D', 'STATE', ['BS2']),
    'O8': ('M', '-', 'STATE', ['O7']),
    'O9a': ('M', '-', 'STATE', ['O7', 'i:CH1']),
    'O9b': ('M', '-', 'STATE', ['O9a', 'CH3a', 'O8']),
    'P1': ('M', '-', 'P2P', ['BS7']),
    'P2a': ('L', 'D2', 'P2P', ['P1', 'O17', 'i:CH1']),
    'P2b': ('M', '-', 'P2P', ['P2a']),
    'P3': ('L', '-', 'P2P', ['P2a', 'i:CH3b']),
    'P4': ('L', '-', 'P2P', ['P3', 'P2b']),
    'P5': ('L', '-', 'P2P', ['P4', 'MP1a', 'O17']),
    'P5a': ('M', '-', 'P2P', ['P2a', 'SP0', 'i:P3']),
    'P6a': ('M', '-', 'P2P', ['P5a', 'P4']),
    'P6b': ('M', '-', 'P2P', ['P4', 'EV4', 'LQ3', 'IX1', 'BS10']),
    'P7': ('M', '-', 'P2P', ['AM1', 'LQ5']),
    'PF0': ('M', '-', 'OTH', ['B1']),
    'PF1': ('M', '-', 'OTH', ['B1']),
    'QM1a': ('M', '-', 'LLMQ', ['LQ3', 'CH7b']),
    'QM1b': ('L', '-', 'LLMQ', ['QM1a', 'P7']),
    'REL11': ('S', '-', 'CON', ['CH7b', 'O9b', 'O8']),
    'RL4': ('S', '-', 'LLMQ', ['devnet_soak', 'M5_exit', 'M4_soak']),
    'RP0': ('M', '-', 'OTH', ['AR2', 'ND1']),
    'RP1a': ('M', '-', 'STATE', ['RP0', 'CH3b']),
    'RP1b': ('M', '-', 'OTH', ['RP1a', 'IX1']),
    'RP2a': ('M', '-', 'OTH', ['RP1a', 'CH7b']),
    'RP2b': ('M', '-', 'OTH', ['RP2a', 'GV1b', 'SP1']),
    'RP3a': ('M', '-', 'OTH', ['MP1a', 'MN1']),
    'RP3b': ('M', '-', 'OTH', ['SG1a', 'LK1c']),
    'SG1a': ('M', 'D', 'LLMQ', ['QM1a']),
    'SG1b': ('L', '-', 'LLMQ', ['SG1a', 'LQ5']),
    'SP0': ('M', '-', 'P2P', ['P2a', 'BS8', 'O17']),
    'SP1': ('M', '-', 'P2P', ['SP0', 'P4']),
    'ST1': ('M', 'D', 'STATE', []),
    'ST2': ('M', '-', 'STATE', ['BS2']),
    'ST3a': ('M', 'D', 'STATE', ['ST1', 'i:ST2']),
    'ST3b': ('M', '-', 'STATE', ['ST3a', 'ND1', 'CH2', 'i:CH3b']),
    'V24a': ('M', '-', 'CON', []),
    'ZQ1': ('M', '-', 'OTH', ['CH7b', 'MP1a', 'LK1b']),
    'devnet_soak': ('S', '-', 'OTH', ['O12b', 'CJ1', 'GS1']),
}


EF, PRED = {}, {}


def ef(t):
    if t in EF:
        return EF[t]
    s, dsn, r, deps = T[t]
    start, p = 0.0, None
    for d in deps:
        if not d.startswith("i:") and ef(d) > start:
            start, p = ef(d), d
    if DS[dsn] and DESIGN_START + DS[dsn] > start:
        start = DESIGN_START + DS[dsn]
    fin = start + SZ[s] + RV[r] + CAL.get(t, 0.0)
    for d in deps:
        if d.startswith("i:") and ef(d[2:]) + 0.5 > fin:
            fin, p = ef(d[2:]) + 0.5, d[2:]
    EF[t], PRED[t] = fin, p
    return fin


def chain(t):
    out = []
    while t:
        out.append(f"{t}({EF[t]:.1f})")
        t = PRED[t]
    return " <- ".join(out)


if __name__ == "__main__":
    for goal in ["CH1", "P2b", "CH3b", "EV2b", "LQ3", "CH7b", "REL11", "M4_soak", "M5_exit",
                 "AM1", "QM1a", "DK1", "SG1b", "O12b", "devnet_soak", "RL4"]:
        ef(goal)
        print(f"{goal:12s} wk {EF[goal]:5.1f}   {chain(goal)}")


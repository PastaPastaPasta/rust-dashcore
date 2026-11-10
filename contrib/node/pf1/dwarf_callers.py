#!/usr/bin/env python3
"""PF1: caller-side inclusive shares from DWARF-unwound runs (report section 2.2).

    dwarf_callers.py RUN_DIR... > table.md

For each run the instruction samples in fold/instructions_u.folded are counted once per row if ANY frame of the stack
matches the row's regex ("inclusive": a function and everything it calls; rows overlap). Shares are of all samples of
the window, samples without a stack included (reported per run, they are the lower bound of what a row can miss).
Regexes are matched against demangled frame names as written by perf_fold.py.
"""

import re
import sys

ROWS = [
    ("ProcessSpecialTxsInBlock", r"ProcessSpecialTxsInBlock"),
    ("CQuorumBlockProcessor::ProcessBlock (commitments)", r"CQuorumBlockProcessor::ProcessBlock"),
    ("BLS Verify* / AggregateVerify*", r"bls::\w+::(Verify|AggregateVerify)|CBLS\w*::Verify|VerifySecureAggregated|"
                                       r"VerifyInsecureAggregated"),
    ("BLS check-queue work (CCheckQueue<..Bls..>)", r"CCheckQueue<.*Bls|BlsCheck"),
    ("G2Element::FromBytes / IsValid (signatures)", r"G2Element::(FromBytes|FromByteVector|IsValid|CheckValid)"),
    ("G1Element::FromBytes / IsValid (public keys)", r"G1Element::(FromBytes|FromByteVector|IsValid|CheckValid)"),
    ("G1 or G2 decode/check (union)", r"G[12]Element::(FromBytes|FromByteVector|IsValid|CheckValid)"),
    ("chainlock (CheckCbTxBestChainlock / GetNonNullCoinbaseChainlock / VerifyChainLock)",
     r"CheckCbTxBestChainlock|GetNonNullCoinbaseChainlock|VerifyChainLock"),
    ("GetCreditDataFromBlock", r"GetCreditDataFromBlock"),
    ("RebuildListFromBlock", r"RebuildListFromBlock"),
    ("CheckMerkleRoot", r"CheckMerkleRoot"),
    ("VerifyScript", r"VerifyScript"),
]


def main():
    runs = sys.argv[1:]
    comp = [(n, re.compile(p)) for n, p in ROWS]
    res = {}
    for r in runs:
        tot = nostack = 0
        cnt = {n: 0 for n, _ in comp}
        for line in open(f"{r.rstrip('/')}/fold/instructions_u.folded"):
            st, _, n = line.rstrip().rpartition(" ")
            n = int(n)
            tot += n
            if "no-stack" in st or st.strip() in ("", "[unknown]"):
                nostack += n
            for name, p in comp:
                if p.search(st):
                    cnt[name] += n
        res[r] = (tot, nostack, cnt)
    names = [r.rstrip("/").rsplit("/", 1)[-1] for r in runs]
    print("| inclusive share of all instructions | " + " | ".join(names) + " |")
    print("|---|" + "---|" * len(runs))
    print("| total instructions (G) / samples without a stack | " + " | ".join(
        f"{res[r][0] / 1e9:.1f} / {100 * res[r][1] / res[r][0]:.1f} %" for r in runs) + " |")
    for name, _ in comp:
        print(f"| {name} | " + " | ".join(f"{100 * res[r][2][name] / res[r][0]:.1f} %" for r in runs) + " |")


if __name__ == "__main__":
    main()

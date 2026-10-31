# Manager check of Sol r1 Major 1 (load drift / not paired), 10-27 23:5xZ

Evidence from /work/scratch/sp1b/accept1/samples.tsv and timing.py at f8c61e079:

- Session ids are assigned at **dispatch** in `pick()` (timing.py:578-592), which picks min(done + inflight). That sum is the dispatched count for each pair, so it doesn't depend on which node finishes first, and ties break in fixed tuple order.
- The result is strict round-robin: every (node, kind) pair has exactly one residue of the session id mod 4. dashd f6 is 0, dashd s30 is 1, rdashd f6 is 2 and rdashd s30 is 3, 10,000 sessions each. Every block of 4 consecutive dispatches holds one session of each pair, so a dashd session and its rdashd partner start within about a second of each other and share the same load window (sessions last 10–40 s).
- KS D between the dispatch positions of dashd and rdashd sessions is 0.0001 for both f6 and s30.
- p50 drift from the first quarter to the last quarter is small and has no consistent direction for either node: first6 9.51→9.44 (dashd) and 9.40→9.50 (rdashd); reset_tick 2.97→2.94 and 2.98→3.04; gap_tick 6.001 constant; first30 33.42→33.57 and 33.44→33.45.

**Conclusion:** the bias claim (one implementation's samples skewed to later, differently loaded periods) doesn't hold, because dispatch is matched pairwise in time. What remains valid is narrower:
- The load ceiling is checked only at start.
- No per-session dispatch time or load is recorded.
- Heavy load adds noise, which lowers the test's power.

These are Minor, worth fixing for the nightly job.

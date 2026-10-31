# O2b `diff_chain` rev3.1 — short Sol r4 review

**Verdict: ACCEPT WITH NITS.** All four r3 findings are resolved; the delta adds no substantive inconsistency.

1. §8 maps `SPORK` to `sporkupdate` and observes with `spork show`, matching Core v24's RPC handlers.
2. §8 uses separate `KILL` and `CLEAN` trials from a clean-saved, spork-19-on baseline. Each updates to `4070908800`, checks off before restart, then checks on after `KILL` or off after `CLEAN` on `dashd`, the session, and `NodeSut`.
3. §2.3, PLAN SP0, and R17 carry the existing shutdown `SporkManager::store` call at `6ce75bf1e` without adding another call.
4. §0 says “no LLMQ signer”; §6.5/R15 describe delayed mirrors; R16 covers suppression on `KILL` and restore teardown.

**Nit — §8:** “all six assertions” counts only one check per trial and system. The specified pre-restart and post-restart checks total **twelve** assertions. Change the count to twelve, or say “all specified assertions.”

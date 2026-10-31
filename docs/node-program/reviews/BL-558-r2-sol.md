> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-558 narrow re-review, round 2

Reviewed `6d0f988570eabbf8cb69507dcd0d0482857b70b6..c43b019daeeeaee8d071d602380984ca46caef27` in the read-only `/work/home/agent/workspace/rdc-wt/nd1-bl558` worktree. Scope: the Sol r1 major and defects introduced in the changed README lines, following the BL-558 narrow brief and `briefs/REVIEW-COMMON.md`.

## R1 finding

1. **CLOSED — major, default onion-listener collision:** `rdashd/README.md:51–53,70` now uses `listen=0` in both primary examples; parameter interaction disables `listenonion`, and `ConnMan::start` skips every P2P listener while retaining outbound dial threads. The inbound alternative at lines 59–63 resolves to `0.0.0.0:21999` and `127.0.0.1:21996=onion`, disables wildcard/default binding, and suppresses insertion of the default onion target. Neither example requires the existing Core node's default P2P/onion sockets. The new table and explanation correctly identify the independently selected onion target.

## New finding

- **N1 — nit — `rdashd/README.md:45–46`: the mainnet-only statement is too broad for `listen`.** The replacement says “The options are per network” and that keys before any section apply only to mainnet, immediately before introducing `listen=0`. In fact, `-listen` lacks `NETWORK_ONLY` (`dash-args/src/options/generated.rs:74`), and `State::use_default_section` (`dash-args/src/args.rs:1050–1051`) reads such default-section settings on every network. A reader putting `listen=0` before the sections, expecting it to affect mainnet alone, also disables inbound listening on testnet/regtest unless overridden there. The shown examples remain valid. Suggested fix: name `port`, `rpcport`, and `bind` as the options requiring network sections, and qualify the parenthetical accordingly; optionally explain that unsectioned `listen` applies to every network. Dash Core v24.0.0-rc.3 has the same declaration and section-selection behavior.

## Verification

- Inspected the actual bind construction and startup paths at the reviewed head: `dashd-node/src/p2p/mod.rs:317–370`, `dashd-node/src/init/interaction.rs:47–72`, and `dashd-net/src/connman.rs:128–170`. Checked explicit bind execution in `dashd-net/src/binds.rs:176–196` and onion port constants in `dash-args/src/chain.rs:45–49`.
- Compared the pertinent Core v24.0.0-rc.3 `src/init.cpp` bind construction (2686–2695), option declaration, and `src/util/system.cpp:1087–1089`. Also checked the corresponding v23.1.8 onion-target construction; the README's v24 configuration is supported by the v24 behavior.
- Checked the other changed factual claims against error construction and UI formatting; no further defect found in those lines.
- Reused the existing r1 compiled argument harness after `diff -qr` confirmed its copied `dash-args/src` is identical to the reviewed head. Ran `documented_command_lines_parse` and `regtest_no_listen_config_parses`: **2 passed, 0 failed**. These exercise argument parsing and disabled-listen config handling; they do not execute the revised inbound example or start a daemon. Tests ran with `TMPDIR=/dev/shm/bl558-r2-sol-tmp` (0700), subsequently removed.
- `git diff --check 6d0f98857 c43b019da` passed. Worktree HEAD and clean status were verified. No branch edits, commits, pushes, GitHub posts, or daemon processes.
- No build: preflight reported five-minute load 44.09 and **194 GB available**, below the brief's required 200 GB. No independent daemon runtime experiment was performed; listener closure is established from the actual startup code. Did not read `/work/cache`, `/work/chains`, or a restricted notes tree.

Verdict: **APPROVE-WITH-NITS**

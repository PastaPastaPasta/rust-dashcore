> **Superseded by DEC-82 (v24 only).** The O17 audit compared v23.1.8 and v24.0.0-rc.3; the node now targets v24
> alone, so these tools are history. `check-release-refs.sh` here is the DEC-82 guard (no per-release code).

# PIN-DELTA auditor brief (template)

The brief every auditor of a PIN-DELTA audit gets (node-program task O17; re-run at every re-pin by SW-V24). Replace `<OLD>` and `<NEW>` with the two pins, generate the packets with `inventory.py`, and give each auditor this file plus its packet name.

    python3 -I contrib/node/pin-delta/inventory.py --core ~/workspace/dash --old <OLD> --new <NEW> --out /work/scratch/<slug>/pin-delta

## Goal

A complete, classified list of every behavioural difference between Dash Core `<OLD>` and `<NEW>`. rdashd emulates a pinned release per network (DEC-05); the rows decide which behaviours it implements twice, selected by `ReleaseProfile` (`designs/O17-release-profile.md`).

## Inputs

- Dash Core repo `~/workspace/dash`, **read-only**: `git show|diff|log|grep|blame` against the tags only. Never check out, switch branches, stash, fetch or modify anything; other agents share it.
- Your packet: `/work/scratch/<slug>/pin-delta/packets/<packet>.prs.txt` and `<packet>.files.txt`.
  - `.prs.txt` lists the PRs whose main area is yours, with per-file +/-, the functional tests each PR touched (`test:` lines: they show what the PR asserts) and the commits inside each merge. Entries marked `OLD-ONLY` exist only in `<OLD>` (when it is not an ancestor of `<NEW>`) and need the same review.
  - `.files.txt` lists the files you own for the file sweep, with net +/- between the pins and how many `<OLD>` release-branch commits touched each (`old-side:`).
- The current report, `designs/O17-pin-delta.md`, whose rows in your area you confirm or correct.
- PLAN task IDs for the Consumers column.

## Ground truth and the backport trap

When `<OLD>` is a release branch merged back into the `<NEW>` line, many PRs in the range were already backported to `<OLD>`, sometimes adapted so patch-ids differ. A PR diff is therefore not proof of a delta.
- The authority is the tag-to-tag state: `git diff <OLD> <NEW> -- <file>` and `git show <OLD>:<file>` vs `git show <NEW>:<file>`.
- `IN-OLD-PIN?=` flags are signals, not verdicts:
  - `yes`: every commit was cherry-picked into `<OLD>`;
  - `tree`: the PR's diff reverse-applies to `<OLD>`'s tree;
  - `ref`: the PR number is cited in a release-branch commit, a hint only, since "not on this branch" is cited the same way;
  - `partial:m/n`: some commits were cherry-picked.

  A later merge can still drop part of a backport, so none of them proves "no delta".
- Confirm both behaviours in source and cite both before writing a row.

## What counts as a delta

Anything a block validator, a peer (honest or probing), a masternode quorum member, an RPC/ZMQ/REST client (functional tests, Platform, dashmate) or a wallet user could observe differently: timers, thresholds, misbehaviour scores, disconnects, message order, accepted/sent messages, inv types, version gates, service bits, option defaults, InitErrors, RPC names/args/fields/errors, reject strings, mempool acceptance, and persisted-file formats users move between versions. Log lines count only when functional tests assert them.

Refactors, renames, lock or thread changes with no observable effect, logging, test/GUI/build changes and internal perf go to "Not deltas" with a reason, so coverage stays provable.

## Classification

**Class:** `CON` consensus · `P2P` P2P-observable (incl. relay and mempool policy) · `MN` masternode behaviour (DKG, signing, PoSe, MNAUTH, IS/CL creation, CoinJoin servicing, governance voting, MN init) · `RPC` RPC/REST/ZMQ/CLI · `INT` internal-only. Add a secondary with `+`.

**Keying:**
- `gated`: behind a deployment checked at runtime; same code, per-network params decide.
- `params`: a per-network chain-params value that differs between pins.
- `profile`: implement both behaviours, selected by `ReleaseProfile`; propose the field path.
- `new-all` (written with the new pin's label, e.g. `v24-all`; `merge.py --new-name` sets the label): implement only the newer behaviour everywhere. It is a declared deviation on old-pin networks, so it needs a written argument that holds for **every** input an honest or probing party can produce there (O17's DEC-37 standard). These count as reachable:
  - process-history dependence (caches, global flags), which is state to model, not a reason to skip;
  - historical verification (`verifychain`, reindex), which checkpoints do not exclude;
  - bootstrap tips below old activations, reached through RPC;
  - cheap probes from an ordinary peer, a verified masternode or a QWATCH observer, even with crafted input;
  - stable RPC results, errors, authorisation and startup/config acceptance (storage freedom does not cover these).

  What remains allowed is narrow: the same bytes accepted and rejected with only allocation or timing different; an abuse quota honest traffic stays under; undefined or non-terminating old behaviour; code that runs only on networks pinned to the new release; randomised behaviour neither release specifies; disk faults. State the scope exactly. If in doubt, key it `profile`.
- `old-all` (e.g. `v23-all`): rare; explain.
- `none`: internal-only.

**Interop rule:** on a network that runs the old pin, rdashd lives among old-pin peers and masternodes. If the new behaviour would mis-score, disconnect, mark bad or reject something an old-pin node sends or expects, the row is `profile`.

## Output

`/work/scratch/<slug>/pin-delta/out/<packet>.md` with four sections:

1. **Delta rows** — `| id | Difference: <OLD> → <NEW> | Class | Keying | Both? | Profile field | Consumers | PRs | Evidence | Prior row |`. One row per distinct behaviour, values on both sides, `#NNNN` PR refs, `<OLD>: path:line` / `<NEW>: path:line` evidence.

   `merge.py` parses this table and rejects anything else, so:
   - the section heading contains "Delta rows" and has no subheadings;
   - ids are `<PREFIX>-NN` (letters and digits, then a number);
   - every row has exactly 10 cells, so escape a literal `|` as `\|`;
   - Class starts with one of `CON`, `P2P`, `MN`, `RPC`, `INT`;
   - Keying starts with one of the keywords above.
2. **Not deltas** — `| PR | Reason |` for every other PR in the packet.
3. **File sweep** — files whose tag-to-tag diff has a behaviour change no PR row explains; end with "all N files swept".
4. **Notes** — surprises, cross-packet hand-offs, profile-shape suggestions.

## Rules

- Read-only except your output file. No network, no posting.
- Never read `~/private/`. Do not write about suspected unfixed security weaknesses in the output; mention them in one neutral line in your final reply only.
- Source reading only; no dashd runs or builds.
- A row without evidence on both sides is not finished.

## Integration

The integrator copies the packets to `designs/O17-pin-delta/` and records every decision in `designs/O17-pin-delta/decisions.json`:
- duplicates (dropped → kept; the kept row absorbs the dropped row's PRs and consumers);
- keying overrides, with a reason;
- the home of every `profile` row;
- titles;
- `add_consumers`: tasks appended to a row's consumers (e.g. the oracle task of a reclassified row);
- `deviation`: for every `new-all`/`old-all` row, `[policy, verification task or "none", argument]`. The argument is the written unreachability or unobservability case, and the report prints it.

Then:

    python3 -I contrib/node/pin-delta/merge.py --packets designs/O17-pin-delta \
        --decisions designs/O17-pin-delta/decisions.json > index.md      # becomes the report's index
    python3 -I contrib/node/pin-delta/inventory.py --core ~/workspace/dash --old <OLD> --new <NEW> \
        --out /work/scratch/<slug>/pin-delta --report designs/O17-pin-delta.md designs/O17-pin-delta/*.md

Then regenerate the spec's per-field and per-task tables:

    python3 -I -B contrib/node/pin-delta/merge.py ... --json /work/scratch/<slug>/rows.json > index.md
    python3 -I -B contrib/node/pin-delta/spec_tables.py --rows /work/scratch/<slug>/rows.json \
        --plan PLAN.md --fields fields.md --tasks tasks.md

`merge.py` must exit 0. `inventory.py` must exit 0: zero uncited reviewable PRs, including `OLD-ONLY` commits, and no unattributed runtime files. A backport merge (a release-branch merge of cherry-picked develop merges, e.g. a v24.0.x rc) is split into its sub-PRs: each upstream PR, and each direct commit (labelled by sha), is its own row, packet entry and coverage check, and the umbrella PR need not be cited (`--no-split` restores one row per merge). Packet files keep their auditor row ids, so the decisions file and the report's cross-references stay valid.

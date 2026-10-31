# Sonnet 5.5 calibration A/B (DEC-93): write-up

Three ready Sonnet-tier tasks were each implemented twice from the same brief (`briefs/AB-*.md`), in separate worktrees:
- `-s`: Sonnet 5.5 high;
- `-o`: Opus 5.5 medium.

Both arms went through the same GPT-6.1-Sol review brief. All six arms launched at 05:02 on 2026-10-09. The raw per-round log is in the manager's notes; this page is the summary DEC-93 asks for.

## Results

| Task | Sol rounds to converge (s / o) | r1 findings (s / o) | Fix time (s / o) | Winner | Deciding difference |
|---|---|---|---|---|---|
| AR1 `dash-args` (the worry case) | 3 / 3 | 2 major + 4 minor / 1 major + 6 minor | 1 h 49 m / 3 h 09 m | **o** | Windows fidelity. o verified against the win64 release under Wine (83/83 init cases, msvcrt probes), as DEC-84 asks for every platform. s handled Windows roots but had no binary-level Windows evidence. |
| AR2 `dash-rpc-spec` | 2 / 2 | 1 major + 1 minor / 1 major + 1 minor | 1 h 04 m / 42 m | **o** | o models nested args, `skip_type_check` and hidden RPCs via a Core-linked dump. s models top-level args only, a gap it reported itself. |
| O5 quirk register | stopped after 3 / 3 (converged, DEC-121) | 2 major + 2 minor + 1 nit / 1 major + 4 minor | about 2 h 50 m over 2 rounds / about 2 h 46 m over 2 rounds | **o** | s's r2 fix introduced a regression that r3 caught: a cross-package duplicate of a registered id passed. o's checker enforces one test per id over the whole workspace, at source and compiled level. s ran no self-review in any round. |

- **First-pass wall time** was about 6 h for every arm except O5-o (about 2 h 50 m). The shared sccache stall and load of 60–180 dominated, so wall time doesn't separate the tiers here.
- **Manager interventions:** one, a nudge to O5-o, which had ended its turn waiting.
- **Gates:** every arm reached a green gate. The only red steps were the BL-20 flake and the private-tests receipt race, both environmental and since fixed (e9127064).

## What this says about the tiers

- **Sonnet 5.5 high is not the risk the old assumptions suggested.**
  - It converged in the same number of Sol rounds as Opus medium on AR1 and AR2, including the case flagged as the worry.
  - Its majors were of the same kind and count as Opus's.
  - On AR1 its fix rounds were faster.
- **It lost every pair on the same axis: fidelity breadth beyond what the brief enumerates.** Examples: every platform at binary level, nested or hidden cases of a Core structure, and a checker's correctness against ordinary developer moves.
  - Opus medium went looking for those cases unprompted.
  - Sonnet did what was listed and reported the rest as gaps, which was honest but less complete.
- **Self-review matters for the Sonnet tier.** The arm that skipped `code-review-validator` (O5-s) is the one that regressed.

## Re-tier (DEC-130)

1. **S-h stays** for tasks whose acceptance is fully enumerated by the brief, e.g. a fixed vector or oracle set, a closed list of cases, or a mechanical port with a stated surface.
2. **S-h → O-m** for any task whose brief requires one of the following:
   - **(a)** all-platform behaviour (DEC-84) beyond what the vectors already cover;
   - **(b)** modelling a Core structure "completely", where completeness isn't enumerated;
   - **(c)** gate, checker or guard tooling that other tasks' correctness relies on.
3. **Every Sonnet brief requires a `code-review-validator` pass before reporting.** "Sol reviews next" is not an exemption.
4. Unchanged: a struggling Sonnet implementer is redone one tier up, not given more rounds (PLAN §4 legend).

The row-by-row application to PLAN §4 is a separate docs change (the PLAN re-tier task).

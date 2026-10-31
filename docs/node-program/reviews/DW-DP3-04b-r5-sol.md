# DP3-04b storage pin and pool rotation: Sol review, round 5

Reviewed platform `425f4d498f..82fb33aff8` and desktop `9faeeff5..f5d0ae5b` in detached scratch worktrees. The round-4 Unix fix and the round-3 restore schedules pass. One item needs manager assessment.

## Finding

possible security note: see manager

## Validation

- The owner-only regression test passed at the committed `umask 022`, and in scratch variants at `umask 000` and `002`. Source inspection confirmed the Unix directory modes are supplied to creation, the staged and backup temp files are created `0600`, and `fs::copy` opens the proof copy with the staged file's `0600` mode before copying bytes. Existing auto-backup directories and the proof's migration backup use `0700` creation modes. No wider Unix creation window was found in these paths.
- Reverting both new creation-mode changes in a separate detached worktree made the regression test fail: a backup parent was `0777` instead of `0700` under `umask 000`. Reverting only the proof-directory mode also made it fail, with `InsecureParentDir` on the proof directory. The reviewed branches were not edited.
- Re-ran both round-3 CLI refusal schedules with `--no-auto-backup`: deleted fork V1 history under `umask 000` and upstream V1 checksum `0` under `umask 002`. Each restore exited **3**, left `live.db` SHA-256 identical, and `migrate` reopened it with exit **0**. No stage or proof artifacts remained; the source's own WAL/SHM siblings were the only extra files.
- A live source with `birth_height=111` only in WAL restored over a destination at `222`: restore exited **0**, the reopened destination read **111**, and the source main file alone had no row. Restore over itself also exited **0** and retained **111**. No stage or proof artifacts remained.
- Desktop changes are limited to `rust/Cargo.toml`, `rust/Cargo.lock`, and the D39 design note. All **8** direct platform revisions and all **31** platform lockfile package sources point to `82fb33aff8`; no old revision remains. `cargo metadata --locked --offline --format-version 1` and both range `git diff --check` commands passed.

VERDICT: CHANGES-REQUIRED

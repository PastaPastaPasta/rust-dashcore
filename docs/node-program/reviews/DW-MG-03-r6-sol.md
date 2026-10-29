# DW-MG-03 — independent Sol round-6 review

Target: `dw/mg-03-masternodes` at `374c7a0b`, in a detached worktree, compared with round-5 head `8121c4ea` using `git diff 8121c4ea..374c7a0b -- . ':!docs/generated'`. I did not edit, commit, or push the branch.

## Verdict: APPROVED

No findings in the round-6 scope. D39 closes the round-5 Major 1 and the validator's intervening leak. D40 accurately describes the module's actual gate for the repository's release and dist profiles.

## Findings

None.

## Security and compatibility checks

- **Value/link composition:** `rust/bin/dwcli/src/log.rs:173-207` marks every occurrence of every registered option-value form in the original event, unions overlapping byte ranges, and separately finds the original event's first link. It emits only the prefix before that cut with covered runs replaced, then `<link>` and the terminal line break. Thus a matched value crossing the cut cannot leave its head visible, while a short value cannot alter link recognition. Concrete inputs covered by the writer tests: `--network devnet:p` with `dashpay://invite?pk=SECRET` becomes a line containing `<link>` but no `SECRET` (`:259-299`); `/home/a/x/dash:w` in `open datadir=/home/a/x/dash:w/db failed\n` becomes `open datadir=<redacted><link>\n` (`:319-333`). The same byte-range argument holds for overlapping values, multiple links, a value prefixing a scheme, non-ASCII UTF-8, and a link on a later line: only bytes before the first cut are emitted, and matches are found before any replacement. The matching contract is still the literal values and derived forms in `option_values`/`forms` (`:50-122`), not arbitrary encodings a future library might invent.
- **`dw-uri` compatibility:** `rust/crates/dw-uri/src/redact.rs:155-190` extracts the old `redact_links` scan into `first_link` and its unchanged suffix calculation into `after_cut`. For a link it constructs exactly the former prefix + `<link>` + terminal line break; with no link it still returns `Cow::Borrowed`. This preserves the `dw-app` callers' behavior, including multi-line events, non-ASCII text, percent-encoded `dashpay%3a`, and ANSI escape handling in the unchanged scanner.
- **D40:** `rust/crates/dw-engine/src/lib.rs:47-52` requires both `feature = "test-support"` and `debug_assertions`. The only dependency activation in the repository is `dwcli`'s dev-dependency (`rust/bin/dwcli/Cargo.toml:37-40`); `rust/Cargo.toml:111-122` leaves release's default `debug-assertions = false`, and dist inherits release. The public read-only module remains available to an explicitly feature-enabled debug build, as the revised comments say, but is absent from ordinary release and dist builds.
- **Merge boundary:** The `7bd49f28` main merge did not change `dw-uri` redaction, masternode source, `rust/crates/dw-engine/tests/mn_masternodes_contract.rs`, or `docs/contracts/mn-engine.md`; the latter still says `Contract-Version: 3`. The broad unrelated diff is DP4-02 avatar work.

## Validation

Private `CARGO_TARGET_DIR=/work/scratch/dw-mg03-r6-sol/target` and detached worktree; the branch stayed clean.

- `cargo test -p dwcli`: unit tests passed (49 passed, 1 intentionally ignored); earlier integration binaries passed, then `dashpay_r5::a_healthy_engine_shuts_down_within_the_deadline` missed its 10-second wall-clock bound at 11.83 seconds under load above 50, stopping the command. Rerunning that exact test alone passed. The two integration binaries not reached by the first command also passed: `dashpay_r6` 2/2 and `mn_cli` 3/3.
- `cargo test -p dw-uri`: passed (35 unit, 4 ext, 8 golden, 5 link-fuzz, 1 QR golden).
- `cargo test -p dw-app --test link_redaction`: passed (1/1).
- `git diff --check` on the changed review-scope files: clean.

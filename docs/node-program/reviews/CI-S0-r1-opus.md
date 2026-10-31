# CI-S0 review r1 (Opus-h): fork pre-push check, as a security gate

Branch `node/ci-stage0` at 4ec76dc3a: 4 signed commits, ff289d304..4ec76dc3a, on ffee3ed7f (DEC-670). That base
descends from b26e94cf7. Spec: `brief-ci-stage0.txt` plus GITHUB-CI-STUDY §3a/§3d. I reviewed the worktree read-only.
Every run happened in a reflink copy of `contrib/` under `/work/scratch/ci-s0-review/`: the selftest, the probes and
`fork-push-fp`, built with its own target dir there. Each run used synthetic registries only. The copy was removed afterwards
(rm-if-idle.sh); the probes are described in each finding so they can be re-run. I did not run real-tip
mode or open anything under ~/private.

## Verdict: CHANGES-REQUIRED

The structure is sound. Snapshot mode checks the raw commit text, the tree and the ref name. Every check I traced
fails closed when a file or tool is missing, apart from check 9's contents listing (M-2). The 64-byte guarantee holds
for the ordinary case: an exact 64-byte run is found at all 32 private alignments, and at the start and end of both
files. The selftest passes in 58 s. Nothing the script prints leaks a term or private content.

The problem is five false-pass paths, each reproduced with a synthetic plant:
- a `.gitleaks.toml` in the pushed tree switches check 7 off;
- check 9 passes when the GitHub contents call fails;
- one fingerprint bookkeeping shortcut hides a real 64-byte copy;
- compressed content is mostly invisible to checks 1 and 2: 324 `.zst` blobs, 1.36 GB decompressed, plus truncated
  or trailing-byte gzip;
- a multi-word term wrapped inside a `///` comment passes check 1.

Each fix is small.

Counts: **0 Critical, 5 Major, 8 Minor, 11 Nit.**

## Departures from the spec

| Departure | Judgement |
|---|---|
| Registry variable `DASH_EMBARGO_REGISTRY`, not `DASH_EMBARGO_DIR` | **Accept.** The gate sets `DASH_EMBARGO_DIR` to the private-test tree below the registry. Reusing that name would point the denylist lookup at the wrong directory. The default is still `~/private/embargo`. Fails closed when the registry is missing. |
| Check 2 exemptions (public runs, private scope file, private pins) | **Accept, with conditions.** The public exemption is sound: the whole maximal run must occur in a public blob. All three sources are public GitHub remotes (checked: dashpay/rust-dashcore, the PastaPastaPasta forks, dashpay/dash, bitcoin/bitcoin, dashpay/base-sdk). Pins are hashes of private chunks, kept privately. **The scope file can't be judged from here** (see the private questions below). The spec also never mentions a fourth exemption: runs with fewer than 4 distinct bytes. It is reasonable, but its side effect is m-2. |
| Check 8 is static | **Accept for the pre-push.** Running the private tests in the pre-push would build two test crates for every push. CI runs `--expect-skipped` for real. The static form has two defects of its own (m-4). |
| The snapshot SHA varies with the commit timestamp | **Content is preserved, the object is not.** Tree, message, headers and ref are all checked. Only the dates would differ in a re-created commit. But the printed SHA is an unreferenced object, nothing ties the push to it, and it is printed even on FAIL. Fix in m-1. |

## Major

### M-1: a `.gitleaks.toml` in the pushed tree switches check 7 off

- **Where:** `contrib/node/check_fork_push.py:352-370`. The pushed blobs are written at their paths under
  `$work/gitleaks-tree`, then `gitleaks dir <that root>` runs with no `--config`.
- **Why:** with no `--config`, gitleaks 8.x looks in this order: `GITLEAKS_CONFIG`, `GITLEAKS_CONFIG_TOML`, then
  `<source>/.gitleaks.toml`. The pushed tree controls that last file, and the subprocess inherits the operator's
  environment.
- **Scenario (reproduced):** plant `src/config.py` with a `ghp_` token. Alone it fails check 7 (control). Then add
  `.gitleaks.toml` with `[extend] useDefault = true` and `[allowlist] paths = ['''.*''']`. Result: "0 findings",
  **PASS**, exit 0. Upstream projects ship `.gitleaks.toml` files with path allowlists, so this can also come in
  with a merge.
- **Fix:**
  - write `$work/gitleaks.toml` containing `[extend]\nuseDefault = true` and pass `--config` pointing at it;
  - pass `--gitleaks-ignore-path` pointing at an empty file in `$work`;
  - run gitleaks with `GITLEAKS_CONFIG` and `GITLEAKS_CONFIG_TOML` removed from its environment;
  - optionally, report a pushed `.gitleaks.toml` or `.gitleaksignore` as a check 7 hit.
- **Selftest:** add this plant.

### M-2: check 9 passes when GitHub's contents API call fails

- **Where:** `check_fork_push.py:519`: `listing = gh_json(f"repos/{fork}/contents/.github/workflows?ref={branch}") or []`.
- **Scenario (reproduced with a stub `gh`):** the repo call and the actions call succeed. The actions call lists
  `fuzz.yml` as active. The contents call fails (rate limit: GraphQL/REST limits are shared box-wide; a 5xx; a token
  problem). `fork_state` returns `{'workflows': {}}`. Check 9 prints "no workflows" and passes.
- **Fix:**
  - return `None` when the contents call fails;
  - fail when any workflow that `actions/workflows` lists as `active` is absent from the contents listing;
  - fail on any `.yml` entry that has no `content` (the contents API returns no content for large files);
  - add a selftest case with a failing `gh` stub. The selftest only ever uses the fixture, so the live path has no
    test.

### M-3: the fingerprint misses a real copy when the same blob already holds a short match of its chunk

- **Where:** `contrib/node/fork-push-fp/src/main.rs:447-449` and `:468`. `covered` is keyed by blob alone. It is set
  for every candidate, before the window, distinct-byte, public and pin tests.
- **Mechanism:** candidates are examined in offset order. Suppose the first occurrence of a private aligned chunk in
  a blob is a coincidental 32-byte match. It sets `covered[blob] = at + c`. Every later candidate for the same chunk
  in that blob then satisfies `at + c <= end` and is skipped. If the real copy has only that one aligned chunk, and a
  64-byte run at an unaligned offset always has exactly one, nothing finds it.
- **Scenario (reproduced, probe A):** private file = 16 B + L (64 B) + 32 B. Pushed blob = 50 B, then `L[16..48]`,
  then 50 B, then all of L, then 50 B. `fork-push-fp` exits 0 with 0 hits. The control, L alone in the blob, exits 1.
- **A second path:** a first occurrence whose run is public-exempt (`:473`) also hides a later private copy in the
  same blob.
- **Realistic cases:** text whose boilerplate repeats (a shebang plus `set -euo pipefail`, a recurring doc sentence),
  or binary fixtures with repeated 32-byte hashes.
- **Fix:** key `covered` by diagonal (`(blob, offset - at)`), or insert into it only once a run is counted (after
  `:482`). Add a unit test for probe A.
- **Doc:** `CI-FORK.md:43` says "so no shared 64-byte run is missed". That claim is false until this and m-2 are fixed.

### M-4: compressed content is mostly invisible to checks 1 and 2

- **Where:** `check_fork_push.py:113-121` (`gunzip`), used at `:163` and `:212`.
- **Truncated or trailing-byte gzip:** `GzipFile.read` raises `EOFError` on a truncated stream. It raises
  `BadGzipFile` on bytes after the last member. `gunzip` then returns `None`, so **all** of the decompressed prefix is
  dropped. Reproduced: a gzip of a denylist term plus base64 padding fails check 1 intact. It **PASSes** with its last
  6 bytes cut, and **PASSes** with `TRAILING` appended. Truncated `.gz` fixtures are an ordinary decoder negative test.
- **Other formats are never decoded:** the tip has **324 `.zst` blobs** (`test-vectors/statedigest/...`) that expand
  to **1,360,135,063 bytes**, about 7× the whole tree. The 83 `.gz` blobs expand to 85 MB. A compressible zstd or xz
  member holding a term **PASSes** (reproduced). A short member only fails because zstd and xz store tiny input as raw
  literals. Gzip inside gzip **PASSes** (reproduced). Check 7 does open archives (`--max-archive-depth 4`), but
  checks 1 and 2 don't.
- **The cap fails open:** `GUNZIP_CAP` (512 MiB) truncates silently. A 50 MB gzip can expand past it, and nothing is
  printed.
- **Fix:**
  - decode with `zlib.decompressobj(31)` in a loop, keeping all output up to the error, and loop over members;
  - add zstd (the `zstandard` module, or the pinned `zstd` CLI) and xz/bz2 (`lzma`, `bz2` from the stdlib);
  - recurse to a small depth;
  - make hitting the cap a check failure, not a silent truncation.
- **Memory:** for check 2, 1.36 GB of zstd output cannot go through the current pushed-side index (m-7). Either decode
  zstd for check 1 only and make an explicit, recorded decision for check 2, or flip the index first.

### M-5: a multi-word term wrapped inside a comment, or split by Unicode spacing, passes check 1

- **Where:** `check_fork_push.py:124-125`: `normalized` = ASCII `lower()` plus `\s+` → space. Also `:145-150`.
- **Scenario (reproduced):** with the synthetic three-word term, each of these **PASSes**:
  - `/// zebra quartz\n/// violet` (normalizes to `zebra quartz /// violet`);
  - the term joined by U+00A0;
  - the term joined by U+200B;
  - the term split across two string literals.
- **Why it matters:** the selftest comment says "case and line breaks do not matter". Multi-word "weakness wording"
  is the main kind of term the brief asks for. In this repo, prose lives mostly in wrapped `///`/`//!` comments, with
  `#` in shell and Python and ` * ` in C++. `re:` lines can't help, because `\n` is gone before they match.
- **Fix:** before collapsing whitespace:
  - drop comment leaders after a line break (`\n[ \t]*(//[/!]?|#+|\*|--|;+|>+)?`);
  - decode as UTF-8 with `errors="surrogateescape"`, apply NFKC, remove category-Cf characters, `casefold()`, and
    treat every Unicode space as whitespace;
  - then re-encode.

  Keep matching the raw normalized form as well, so nothing that matches today stops matching.
- **Selftest:** add the `///`-wrapped plant.
- Encoded text (base64, hex) is out of reach for any denylist. The CI-FORK.md "what it can't catch" paragraph should
  say so.

## Minor

### m-1: snapshot: the checked commit is not pinned to the push, and its SHA is printed on FAIL

- **Where:** `contrib/node/check-fork-push.sh:68` and `:77`.
- **What happens:** `commit-tree` stamps the current time, so every run makes a different SHA. The only record of the
  checked one is stdout. It is an unreachable loose object, which gc may prune after `gc.pruneExpire`. Line 77 prints
  `commit … as refs/heads/ci/… (not pushed)` whatever the status, and that reads like a push instruction even after a
  FAIL.
- **Minimal fix (both parts):**
  - Run `commit-tree` with `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE` set to `"$(git log -1 --format=%ct "$full") +0000"`.
    The SHA then depends only on the tree, the message, the identity and the signature. The box's ed25519 signing is
    deterministic, so a re-run reproduces the same SHA.
  - On PASS only, `git update-ref refs/fork-push/ci/$short "$commit"` and print that ref. The Stage 2 push step must
    then push exactly that ref, `git push <fork> refs/fork-push/ci/$short:refs/heads/ci/$short`, and refuse when it
    is absent. On FAIL, print "not pushable" rather than the commit.
- A local ref outside `refs/heads`/`refs/tags` is never pushed by default, and it keeps the object alive.

### m-2: the fingerprint misses a run whose only aligned chunk is low-entropy, or is a very common chunk

- **Where:** `main.rs:440` (private aligned chunks with fewer than 4 distinct bytes are never looked up), `:187`, and
  `:441` (`CANDIDATE_CAP` = 4096, taking the lowest offsets in the whole pushed corpus).
- **Low-entropy chunk (reproduced, probe B):** a 64-byte shared run = 16 random bytes, 32 zero bytes at a private
  multiple of 32, 16 random bytes. The run has dozens of distinct values, but its only aligned chunk is zeros. Exit 0.
  Zero-filled 32-byte fields (null hashes, null prevouts) are typical of crafted binary inputs.
- **Common chunk:** a run whose only aligned chunk occurs more than 4096 times in the pushed tree (license and header
  boilerplate) is not checked beyond the first 4096 occurrences.
- **Fix:** both are narrow. Either look up the neighbouring unaligned private offsets when an aligned chunk is skipped
  or capped, or state the real guarantee in CI-FORK.md: a run of at least 64 bytes is found if one of its aligned
  32-byte chunks has at least 4 distinct bytes and at most 4096 occurrences.

### m-3: test-only overrides stay live in the real modes

- **Where:** `check-fork-push.sh:51-56` and `:38`.
- **What:** each of these env vars is honoured in `snapshot` and `range`, not just under `--selftest`:
  - `CHECK_FORK_PUSH_FORK_FIXTURE` replaces the gh api query with a JSON file, and the check 9 line prints the same
    either way;
  - `CHECK_FORK_PUSH_PUBLIC` and `CHECK_FORK_PUSH_SECRETS` widen exemptions;
  - `CHECK_FORK_PUSH_MAX_BLOB` changes the size limit;
  - `CHECK_FORK_PUSH_FP` swaps in an unverified binary.
- **Scenario:** one of them is left exported in a manager shell after debugging, and the real-tip gate then checks
  against a fixture.
- **Fix:** in the real modes, refuse them (except `CHECK_FORK_PUSH_THREADS`), or print one `OVERRIDE <name>` line for
  each and exit 2 unless `--allow-overrides` is given.

### m-4: check 8 passes silently when a private test file moves, and its list is duplicated

- **Where:** `check_fork_push.py:402-405` and `:419-420` (`if data is None: continue`);
  `check-private-tests.sh:322-323`.
- **Scenario:** `node-l1/tests/private_seed_replay.rs` is renamed. Check 8 skips it, and `--expect-skipped` fails
  only in CI, after the push.
- **Fix:**
  - fail when a listed file is absent;
  - fail on any `*.rs` that calls `private_tree(` or `embargo_dir(` and is not in the list;
  - keep the list in one place, for example a tracked `contrib/node/private-tests.txt` read by both scripts.

### m-5: the `--expect-skipped` selftest writes into the live checkout

- **Where:** `check-private-tests.sh:368-370`.
- **What:** it creates `dash-dml/tests/zz_private_selftest.rs` in `$root`, which is the real worktree. `sk_fail`
  (`:352`) exits without removing it.
- **Scenario:** an interrupted or failed selftest leaves a `zz_private_*` file in the tree. Any concurrent cargo run
  or gate copy picks it up, and so does the next `git add` of `dash-dml/tests`. It also breaks the "never patch the
  live worktree" rule (DEC-493).
- **Fix:** point `expect_skipped` at a root it is given and run the plant in a temp copy, or remove the file with an
  EXIT trap.

### m-6: the selftest does not prove several fail-closed paths

- **Untested failure paths:**
  - `fork-push-fp` exit 2 (an unreadable private file or a malformed list);
  - a gh api failure (M-2);
  - a gitleaks binary with the wrong hash;
  - a pushed gitleaks config (M-1).
- **Untested fingerprint edges:**
  - one unaligned 70-byte run is the only check-2 window test;
  - `fork-push-fp` has no unit tests.

  I verified by probe that exact 64-byte runs are found at all 32 alignments and at file starts and ends.
- **Fix:** add `#[cfg(test)]` cases for exact-64 runs at alignments 0..31, file edges, probe A (M-3) and probe B
  (m-2), and selftest plants for the paths above.

### m-7: the fingerprint's memory is about 30× the pushed bytes

- **Measured:** `/usr/bin/time -v` on the real tip's 19,196 blobs (186 MB, without gzip members) with a 1 MB
  synthetic private dir: **5.5 GB max RSS**, 55 s.
- **Cause:** `index` builds `Vec<(u64, u32)>` (16 B) for every pushed byte, then 12 B per byte plus a bit filter of up
  to 2 GiB. The real tip adds 85 MB of gzip members, so expect about 8 GB. Decoding the zstd vectors (M-4) the same
  way would need about 40 GB.
- **Effect:** the private side is bounded at one file per thread, and time is fine. But this is the box where several
  threads run gates at once.
- **Fix:** flip the roles. Index the private files at aligned offsets (about 12 B per 32 private bytes) and roll the
  hash over each pushed blob as it streams. Memory then follows the registry rather than the tree. It also removes
  the 4 GiB `u32` ceiling on the pushed side.

### m-8: gitleaks' own suppressions apply to pushed content

- **Inline comments:** a line containing `gitleaks:allow` is skipped (reproduced: PASS). Pass
  `--ignore-gitleaks-allow`.
- **Path allowlist:** the default config skips some paths entirely (`*.bin`, `*.pdf`, `*.png`, lock files and
  others). The blobs are written at their pushed paths, so a token in a `.bin` fixture is never scanned. That is
  gitleaks' default and arguably in spec. Either record it in CI-FORK.md, or also scan the blobs under neutral names
  (the `.objects/<sha>` form).

## Nit

- **n-1** `check_fork_push.py:438`: GitHub's `?` means "zero or one of the preceding character", but here it maps to
  `.`. Example: `cix?/**` matches `ci/abc` on GitHub, but not here (a false pass for that filter). Map `?` to `?`.
- **n-2** `:501`: GitHub runs `delete` events from the default branch. The plan deletes each `ci/*` branch after its
  run, so add `delete` to the default-branch event list (with `schedule` and `workflow_run`).
- **n-3** `:316` (check 6) and `:356` (check 7) use only a blob's first path. The same blob at a datadir-like second
  path, or at a second path that gitleaks would scan, is missed. Iterate `pushed.blobs[sha]`.
- **n-4** `:145-150`: denylist lines are not stripped. A trailing space or `\r` (a CRLF edit) becomes a required
  trailing space, and an indented `#` comment becomes a term.
- **n-5** `:26-28`: `LOCAL_PATH` misses `/Users/<x>/private` (pasta's Macs run this script too) and `/work/scratch`.
  Neither is in the spec list, so this is optional.
- **n-6** `check-fork-push.sh:50`: `"$(fp_bin)"` sits inside the argument list, so a failed build gives `--fp-bin ''`
  and a Python traceback (exit 1, which is still non-zero). Assign it to a variable first, so `die`'s exit 2 lands.
- **n-7** The gitleaks version is pinned twice: `.sh:31-33` (version and tarball hash) and `.py:336-337` (version and
  binary hash). The private-test list is also duplicated (m-4). Keep one source each.
- **n-8** Range mode (`.py:565`) checks `%B` only. Author and committer names and emails, and annotated tag objects,
  are not checked. Range mode is unused now, so this is a note for later.
- **n-9** `public_blobs` trusts a remote-tracking ref's name and does not check its remote's URL. Checking that each
  source remote is `https://github.com/…` would make the exemption self-enforcing. Today all of them are, as checked
  above.
- **n-10** Pins are global content hashes. A chunk pinned for one pushed file is exempt in every other pushed file.
  That fits the design, but say so in CI-FORK.md's pins bullet.
- **n-11** Style: `__import__("io")` (`:118`) and an `import threading` inside the method (`:90`). Hoist both.

## Leak review (no finding)

- **Console output:**
  - check names, pushed paths, the labels `commit message` and `ref name`, counts, and 16-hex prefixes of secret
    SHA-256s;
  - `fork-push-fp` error lines, which name private files, are counted and never printed;
  - the check 2 detail report goes to `fork-push/reports/` under the checked 0700 `fork-push` dir, as 0600.
- **Temp files:** `mktemp -d`, so 0700. Commit text and decompressed pushed content only.
- **Python tracebacks:** a bad `re:` line or bad UTF-8 shows the code line and an offset, not the term.
- **`--expect-skipped`:** prints test names and reasons only, never cargo output.

## Questions only the private content can settle (for the manager)

1. **`fork-push/fingerprint-scope.txt`:** confirm that no excluded subtree holds repro scripts, crafted inputs or
   notes. Each exclusion removes check 2 for that subtree entirely.
2. **Symlinks:** `find ~/private/embargo -type l | wc -l`. `fork-push-fp` skips symlinks (`main.rs:397`), so a
   symlinked repro file is never fingerprinted. If the count is non-zero, resolve symlinks whose target is a regular
   file.
3. **The denylist:** check how many lines are multi-word (exposed to M-5), whether any are non-ASCII (case folding is
   ASCII-only), and whether any have trailing whitespace (n-4).
4. **The registry's total size:** check its size and largest file against m-7's memory model. One file is read whole
   per thread.

## Checked and fine

- `check-self-devdep.py` is byte-identical to `/work/scratch/gate-self-devdep.py` (`cmp`).
- The existing gate mode of `check-private-tests.sh` is unchanged. `--expect-skipped` refuses a set
  `DASH_EMBARGO_DIR`, needs exactly one SKIPPED line and one libtest result, and catches panics. `node_sink::report`
  writes to stderr directly, so the SKIPPED line survives libtest's capture. Both `private_seed_replay` variants (with
  and without `oracle`) start with the guard.
- Snapshot mode checks the raw commit (tree, author, committer, any signature, message). The pushed objects are
  exactly the tree's blobs. The tip has no gitlinks, no LFS (`.gitattributes` has `text=auto` and one `binary` rule),
  1 symlink (whose target text is scanned) and a largest blob of 7.3 MB.
- The shell uses `set -euo pipefail`. Exit codes are carried through `|| status=$?`. Quoting is correct throughout.
- In `fork-push-fp`, errors exit 2. The window arithmetic (a run of at least 2c−1 bytes contains an aligned c-chunk)
  is right. Extension is bounded. Exit 1 matches a non-empty report.
- Cargo checks: `toml_deps` covers `workspace.`, `target.*.`, `[patch.*]` and `[replace]`. A TOML parse failure fails
  check 5.
- Check 9's tree-side logic handles `on` as a string, list or map, YAML's `on` → `True`, `tags`-only filters,
  `branches-ignore`, and ordered negations.

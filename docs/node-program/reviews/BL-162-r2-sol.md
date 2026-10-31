# BL-162 narrow re-review (96c1be92..9ae20c20)

## Findings

1. **P3 — A deleted leak can be reported as still present.** `apps/desktop/e2e-tauri/scratch.ts:55,65-72`; `apps/desktop/e2e-tauri/profile-write.ts:172,90-92`. `secrets` normally contains the mnemonic twice when it already uses single spaces. `leaks()` therefore reports the same file twice, both as `(0)`. `removeOwned()` deletes it on the first entry, then puts the second in `kept` because the file no longer exists. The subsequent `found.filter(!kept.includes)` also removes both entries from `deleted`. A run correctly fails, but tells the operator to clean up a file that was deleted. Deduplicate leak paths before deletion, or track deletion by path and classify every matching entry consistently. Cover a file matching multiple secrets in `scratch.test.ts`.

2. **P3 — The timing still rules out a cause the evidence cannot rule out.** `docs/design/BL-162-write-after-sync.md:67`. Two failures of about 123 seconds are consistent with a fixed timeout budget, but a flaky network can exhaust that same budget. Replace “a fixed budget, not a flaky network” with “consistent with a fixed timeout budget; the cause is unknown,” matching D5 and the explanation at lines 75-83.

## Prior findings

- **P1 leak deletion boundary: closed.** `removeOwned()` requires a regular file inside the new run or new output directory; extra scans outside those directories are read-only. `leaks()` uses `lstatSync` and skips symlinks. The reporting nit above does not extend deletion outside owned paths.
- **P2 `never` finalization: closed.** The branch at `profile-write.ts:228-229` reaches the shared `result.json`, sweep and PASS/FAIL path at lines 243-251.
- **P2 scratch-only writes: closed for the default probe.** `layout()` places runtime data, output, XDG dirs and TMPDIR under a new run in the checked scratch root; caller output is canonicalized and constrained to that root. The wrapper reuses binaries and builds only when `DW_E2E_SKIP_BUILD=0` is explicit.
- **P2 cause and error mapping: closed apart from the P3 wording above.** D5 calls the cause unresolved, and the document describes the UI message as an unavailable/timeout family without claiming a transport trace or Platform acceptance.

Source review only, as permitted by the brief; no build or test was run. No files in the checkout were changed.

VERDICT: APPROVE-WITH-NITS

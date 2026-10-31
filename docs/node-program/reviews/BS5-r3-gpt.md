# BS5 round 3 — corpus provenance confirmation

**Item 2: CLOSED. Verdict: APPROVE. No new findings.**

Reviewed rust-dashcore **ed390f110ea04f2329fdd34c6a5529c2440176b4**, specifically the provenance fix **3893d7f109a9be74065bf70614cba9b84d99538c**, against base-sdk **8c23be55abb3f038313bf4b6dc47349ee95c031c**. This is only the requested confirmation of round-2 item 2; no other BS5 items or unrelated changes were reviewed. The report was written first, then finalized with the results below.

## Confirmation evidence

- **Round-2 swap-and-restore: refused.** Built a fresh v24 pinned tree through the reviewed `build.sh`, copied it privately, changed regtest `nCreditPoolPeriodBlocks = 100` to `101`, rebuilt `libbitcoin_common_a-chainparams.o`, inserted it into `libbitcoin_common.a`, and restored `chainparams.cpp` byte-for-byte. `BS5_REUSE=1 build.sh` exited **1**, reporting `differs from the anchor` and identifying the archive and object. This independently reruns the reproduction covered by selftest check 7; I did not run the unrelated selftest checks.
- **Anchor tampering: refused.** Made the private anchor writable, appended a line, and restored mode 444. Reuse exited **1** at the anchor comparison.
- **Anchor regeneration after the swap: default publication refused.** Regenerated the anchor using the exact file/symlink digest algorithm. Reuse then exited **0**, emitted `build reused`, and produced a dump with unchanged `params` but credit period **100 → 101**. Default `run.py --verify-dump` exited **1** with `provenance check failed: fresh build`. The complete default generator also exited **1** at that check and left its corpus directory empty. Passing `--allow-reused-build` explicitly accepted the dump, as documented for development. Mode 444 is not an independent authenticity boundary against its owner; the default fresh-build policy and publication gate close the original silent relabeling path.
- **Depends-output library swap: refused.** Started again from the clean private tree and replaced `depends/x86_64-pc-linux-gnu/lib/libgmp.a` with `libbacktrace.a`. Reuse exited **1** at the anchor comparison, explicitly identifying `libgmp.a`, before linking.
- **Positive control: accepted only as intended.** Intact anchored reuse exited **0**; default verification refused it, while `--allow-reused-build` accepted it. Thus archive refusal was not caused by a generally broken reuse path.
- **Default fresh corpus: equal.** Ran both pinned builds with `BS5_REUSE` unset, from fresh archive-derived trees and empty depends build caches. Both manifests record `build fresh`. Ran the full generator without `--allow-reused-build`: **178 cases per pin**, **147 oracle-cross-checked per pin**, all equal; corpus publication succeeded. The JSON bodies of `chainparams_v23_1.json5`, `chainparams_v24_0.json5`, and `brr_payments.json5` are **byte-identical** to base-sdk **8c23be5**. The oracle and payment case lists are byte-identical to round 2; reused copies of that round's independently executed, pinned oracle result files for these comparisons.

## Evidence and limits

Everything ran under `/work/scratch/bs5-r3-gpt/`, using an archive of the reviewed rust-dashcore commit. Main evidence: `probe.py`, `probe-summary.log`, `swap-restored-refusal.log`, `anchor-tamper-refusal.log`, `regenerated-default-refusal.log`, `regenerated-publication-refusal.log`, `depends-swap-refusal.log`, `fresh-v24.log`, `fresh-v23.log`, `regenerate.log`, and `corpus-compare.log`. Fresh manifests/binaries are in `fresh-dumps/`; published fixtures are in `gen/corpus/`; reference bodies were extracted directly from base-sdk 8c23be5 into `base-corpus/`.

Used **at most 8 build jobs**, with the two fresh builds and mutation rebuild run sequentially. `TMPDIR=/dev/shm/bs5-r3-gpt-tmp` had mode **700** and was removed after completion. No implementation branch edits, commits, pushes, GitHub comments, or writes under `/work/scratch/bs5`. No daemons started. This review confirms the authorized default generator path; it does not claim that a writable local anchor resists deliberate regeneration, or that explicitly opted-in reused builds have fresh-build provenance.

**Final verdict: APPROVE. Item 2 CLOSED; no blocker or major remains in this confirmation scope.**

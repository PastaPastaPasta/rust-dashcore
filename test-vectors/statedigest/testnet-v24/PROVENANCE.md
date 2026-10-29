# StateDigest v1 testnet golden (node program task O8, BL-200)

The golden file is `dash-conformance digest extract` (non-dense, spec §9.3 testnet cadences, no
`--boundary`) run twice against a frozen reflinked copy of the patched testnet recording node
(Core `node/state-digest-v24.0` cc11b5d525, `-assumevalid=0 -coinstatsindex=1`, unpruned), without the
digest flags. Both runs are byte-identical. Every key of every merged `C` record passed the §13.5 checks
against the copy; `MANIFEST.json` gives how many `mnl` values passed by the registration-counter rule.
Format: `docs/node-program/designs/O8-statedigest-v1.md`.

```
testnet-v24/MANIFEST.json         sha256 (compressed and uncompressed), record counts per type, height
                                  range, frozen tip, extractor and Core commits, oracle pin, deviations
testnet-v24/golden-sample.jsonl   the records at the DEC-177 heights, every buried-deployment height and
                                  every 50000th height (S records over 2048 bytes left out)
testnet-v24/dec177.jsonl          DEC-177 verdicts: getcanonicalmnlist against oracle_mnlist_canonical
testnet-v24/dec177-summary.json   the 34 heights, per-height hashes and the verdict counts
```

The full file (about 10.6 GB uncompressed) is not in git. It is stored zstd -19 and read-only at
`/work/cache/statedigest/testnet-v24/<sha256 of the uncompressed file>/golden.jsonl.zst`; the
uncompressed sha256 is `MANIFEST.json`'s `golden.sha256`. Full-file comparisons read that cache through
`STATEDIGEST_GOLDENS_DIR` and are skipped when it is unset.

Deviation: the frozen copy ran with an `LD_PRELOAD` shim that makes `fsync`, `fdatasync` and
`sync_file_range` no-ops (disposable copy, read RPCs only; RPC answers are unchanged). Approved by the
manager on 2026-10-17.

# StateDigest v1 recordings (node program task O8)

Every record's content comes from Dash Core: the digest and decisions files are written by the
patched dashd of branch `node/state-digest-v24.0` (v24.0.0-rc.3 plus the StateDigest patch;
`describe` and binary hashes in each `provenance.json`) while it runs Core's own functional tests,
and the golden file is `dash-conformance`'s encoding of the same binary's RPC answers. The metadata
files (`chainargs.json`, `provenance.json`, `extract.json`, `dec177.jsonl`) are written by the rig
in `contrib/node/statedigest/`. Nothing is hand-written. Format:
`docs/node-program/designs/O8-statedigest-v1.md` (the spec).

```
regtest-v24/<functional test>/manifest.json   the nodes the test created and which were recorded
regtest-v24/<functional test>/node<i>/
  digest.jsonl.zst     -statedigestlog of the node (H, C, R, D), zstd -19
  decisions.jsonl.zst  -recordstatedecisions of the node (H, A), zstd -19
  golden.jsonl.zst     dash-conformance digest extract --dense on a frozen copy (C + S), zstd -19
  blocks.dat.zst       the frozen copy's active chain in -loadblock framing, zstd -19
  chainargs.json       the node's args per process start (passwords redacted)
  provenance.json      Core build info and hashes, the test file's hash, the rig commit
  extract.json         the extraction: mocktime, chain args, hashes (digest, decisions, golden,
                       blocks.dat),
                       reproducible (two runs equal)
  dec177.jsonl         DEC-177 verdicts on the golden's canonical_mnlist samples
```

How they were made (spec §13.1):

1. `contrib/node/statedigest/build.sh` builds the Core branch from tracked files only.
2. `contrib/node/statedigest/record-ft.py` runs one functional test with
   `contrib/node/statedigest/dashd-wrapper` (both flags on, files in each node's datadir) and
   `--nocleanup`, then freezes a reflinked copy of every started node.
3. `contrib/node/statedigest/extract.py --twice` restarts each frozen copy offline (no wallet,
   mocktime pinned to the node's last one), runs `digest extract`, extracts again from a fresh copy
   and requires byte-identical output, and runs `dash-oracle-sys`'s `canonical_mnlist_check` on
   every `canonical_mnlist` sample (oracle re-encode, oracle hash = RPC hash, hash = sha256d(hex)).
4. `contrib/node/statedigest/store-vectors.sh` requires exactly the tests of `ft-list.txt`, each
   with the `manifest.json` that `record-ft.py` wrote (the nodes the test created) matching the
   recorded node directories. Per node it refuses irreproducible or mismatching extractions and a
   dirty rig, requires the digest, decisions and golden to be the bytes `extract.json` hashed, and
   runs the `dash-statedigest` `statedigest-check` example: strict parse, record kinds, the `A`
   join of the digest with the decisions (spec §7.3), the golden as the digest's last tip
   connects plus the golden-only keys (spec §9.2), and the digest and decisions counts equal to
   `provenance.json`'s. It then replaces this directory in one step.
5. `contrib/node/statedigest/dml-leg.sh` runs DEC-177's dash-dml leg over this directory:
   `dash-dml`'s `canonical_list_check` example decodes every sample, re-encodes it unchanged and
   hashes it to the RPC's hash. Its verdicts are not stored; rerun it after a dash-dml change.

Quirks:

- A node a test started from a copied datadir has a digest that begins after genesis; its golden
  covers the contiguous suffix the digest has (spec §9).
- Nodes a test never started have no files (`unstarted_nodes` in the test's `provenance.json`
  and `unstarted` in its `manifest.json`).
- The frozen datadirs are not in git.
- Each `extract.json` names the conformance build that made the stored files (sha256 `98ac3f62…`). A
  re-extraction of all 81 nodes with the later build `99d6b0d3cd63` (which also checks the tip's
  `since`) gave byte-identical digest, decisions and golden files, so they were not stored again.

Check a node (the three files together, as store-vectors.sh does):

```
cargo build -p dash-statedigest --example statedigest-check
d=test-vectors/statedigest/regtest-v24/mining_basic/node0 t=$(mktemp -d)
for f in digest decisions golden; do zstd -dc "$d/$f.jsonl.zst" > "$t/$f.jsonl"; done
target/debug/examples/statedigest-check "$t"/digest.jsonl "$t"/decisions.jsonl "$t"/golden.jsonl
rm -r "$t"
```

`statedigest-stats` (same crate) prints per-kind, effect and query counts of any StateDigest file.

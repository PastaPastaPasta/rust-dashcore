# SB7 Sol review, round 2

Reviewed head: `2f69c2f001277d0241d99cb0ec4c082b7d70f93f`, branch
`node/sb7-sml-core`; review range `068f270a6..2f69c2f00`.
Detached review worktree: `/work/scratch/sb7-r2-sol/wt`.

Scope: the r1 M1 history-acceptance fix, stitched-list checks, retained-dump
argument, and PF0 wording. The range changes only the history test and two
benchmark comments; SML entry/hash, list/quorum roots, CbTx checks, coinbase-CL
predicate, quorum scores, and their adapters have no runtime changes.

**M1's original completeness failures are closed. One new major remains: the
checker can award acceptance to genuine data from the wrong network.** Both
supplied full-history replays nevertheless passed with the advertised counts
and zero mismatches. This finding does not establish that those supplied files
are actually mislabeled.

## Findings

### M2 — major: acceptance does not bind a dump to its declared network

`dash-evo/tests/cbtx_root_history.rs:304`, `:316`, `:334`, `:265`.

`SB7_HISTORY_NETWORK` is an operator-supplied string. It is checked against
three allowed names and then reduced to a boolean selecting the mainnet height
cap. Neither the record's `hash` nor any trusted network identifier is checked.
The CbTx root and the list it checks both come from the same dump.

Concrete reproduction: replay the supplied **testnet** history to height
1,028,160, write its full list at that height followed by its next five actual
records, and run that slice as **mainnet**. Heights, block hashes, and CbTx roots
are preserved; only the first record's `up` is expanded into the full list
required by the file format. Its first block hash is
`000000930ecbd842f51c8bdec8a7282e1184f7f7a5fb72a03770a560948dbc6f`.
The supplied mainnet record at that same height has hash
`000000000000002099caaf1a877911d99a5980ede9b981280eecb291afedf87b`.

```text
SB7_HISTORY=/work/scratch/sb7-r2-sol/cases/testnet-overlap.jsonl.gz
SB7_HISTORY_NETWORK=mainnet
SB7_HISTORY_START=1028160
SB7_HISTORY_END=1028165
SB7_FULL_EVERY=1
nice -n 10 <review-built cbtx_root_history binary> \
  --ignored --exact cbtx_mn_list_root_over_history --nocapture --test-threads=1

ACCEPTANCE mainnet: heights 1028160..=1028165, 6 blocks,
6 CbTx roots compared, 6 full recomputes, up to 522 entries, 0 mismatches
exit: 0
```

The same slice passes with the correct testnet label. Conversely, six unchanged
mainnet records pass with a testnet label. Both tests use heights eligible on
both networks, so this is not merely a pre-activation-height loophole.

**The height cap plus CbTx roots are insufficient to establish network identity.**
They establish a bounded, internally consistent replay against the supplied
roots. Root matching has no network-domain separation and cannot identify the
chain that supplied those roots. The cap remains useful for preventing a
mainnet-labelled run from covering forbidden heights.

Suggested fix: bind acceptance inputs to independently trusted capture
provenance, and reject a network/range mismatch. For example, pin a manifest
mapping each accepted file digest to its network, exact range, and trusted
oracle block/CbTx anchors, then verify both the file digest and expected network
before granting acceptance. Trusted expected block hashes at suitable heights
can reject accidental cross-network substitution; a network/genesis field
merely copied into an otherwise untrusted dump is not authentication. Add a
negative test using real wrong-network records at overlapping eligible heights.
This belongs to the acceptance harness/evidence, not SB11's runtime header
orchestration. An independently validated provenance manifest could also close
the evidence gap without re-dumping already correct captures.

possible security note: see manager

## M1 mechanism and adversarial results

The strict checker now requires START/END/network, enforces exactly START through
END across files, rejects read/gzip/JSON failures and missing CbTxs, checks the
final block/root counts, and refuses a mainnet END above 2,090,000. Advisory mode
labels its output and fails even on a complete valid input. `SB7_FULL_EVERY=0`
also fails. Fifteen built-in strictness tests passed.

External cases exercise the actual ignored-test entry point, environment
parsing, gzip readers, and final assertions. The fixtures use real mainnet
records and independently framed Python SHA-256d roots where a fixture needs a
new self-consistent list. The final 24-case run gave these results:

- A valid six-block dump and a correctly stitched equivalent pass.
- A shorter, otherwise valid gzip dump fails at the final expected height.
- A partial JSON record fails at parsing.
- A missing gzip trailer and a corrupt gzip CRC fail even after every expected
  height has been read; reaching END does not bypass decoder integrity checks.
- A gap and a duplicated height fail at the first unexpected height.
- A missing CbTx, missing input file, missing START, missing END, and missing
  network all fail.
- A later full list missing a genuinely carried entry fails with
  `carried over but missing from the full list` at height 1,028,164. That
  fixture's supplied CbTx root was recomputed for its shortened list, so it
  specifically challenges the stitch membership check.
- A duplicated full-list row and an entry both listed and removed fail.
- Changed carried-entry contents with the original CbTx root fail with root
  mismatches. Changed contents with a correspondingly changed supplied root
  can pass. This is expected at the stitch boundary: that block may legitimately
  update an existing entry, so byte equality with the previous block cannot be
  required. Authenticity of the supplied root must come from capture provenance.
- Above-cap END, zero full-check frequency, and advisory acceptance all fail.
- Genuine wrong-network data passes, as described in M2.

The built-in tests additionally confirm that the initial block may delete a
masternode absent from the review's empty starting state, and that a later file
may start at a deletion. Initial deleted entries are correctly absent from the
post-block full list. These are valid cases, not continuity failures.

## Independent full-history replay

Built the checker from the pinned detached worktree, using a reflink copy of
only the implementer's release target and `RUSTC_WRAPPER=`. Each complete
network was checked **once**, sequentially, with `SB7_FULL_EVERY=10000`.

Mainnet inputs, in order, under `/work/scratch/sb7/hist-sml/`:
`mainnet-a.jsonl.gz`, `mainnet-g1r.jsonl.gz`, `mainnet-g1b.jsonl.gz`,
`mainnet-v19-r2.jsonl.gz`, `mainnet-g2.jsonl.gz`, `mainnet-v20.jsonl.gz`,
`mainnet-g3.jsonl.gz`, `mainnet-tip.jsonl.gz`.

```text
ACCEPTANCE mainnet: heights 1028160..=2090000, 1061841 blocks,
1061841 CbTx roots compared, 107 full recomputes, up to 5168 entries, 0 mismatches
1 passed; exit 0; 58.42s

ACCEPTANCE testnet: heights 7000..=1569775, 1562776 blocks,
1562776 CbTx roots compared, 156 full recomputes, up to 614 entries, 0 mismatches
1 passed; exit 0; 4.87s
```

The counts equal END − START + 1 for both networks. Mainnet now covers the whole
specified capped interval across all eight files, including the gaps present
in r1. No oracle requests were made at any height.

## Retained-dump argument

Accept retaining `mainnet-a`, `g1b`, `v20`, `tip`, and `testnet` **for the MN-root
parity result, conditional on trusted capture provenance**. Re-dumping with the
new fetcher is unnecessary solely because its version changed: every block's
reconstructed list must match that block's recorded CbTx root. A hash-relevant
list drift in an earlier record would produce a mismatch there; a later resync
or file's full list cannot retroactively conceal it. The strict reader also
prevents a damaged tail or omitted height from being discarded as successful
end-of-input. The actual complete runs supplied the missing coverage evidence.

Do not broaden this into proof that all metadata is correct or that the files
belong to the claimed network. Roots commit to serialized hash-relevant entry
state, and root checks rely on the CbTx roots' independent origin. M2's genuine
wrong-network slice passes exactly those strict checks. Thus the retained-dump
argument is sound for detecting hash-relevant reconstruction drift against
trusted roots, but does not supply the missing network provenance.

## R1 conclusions and validation limits

R1's minor conclusions remain closed. Commit `7f7d93b73` explicitly qualifies
the PF0 full-root row as a Rust baseline following Core's algorithm, not measured
C++ Core performance. No benchmark workload behavior changes. The previous
entry/hash, root-cache, CbTx/proof/predicate, quorum-order, adapter and layering
conclusions remain applicable because their runtime sources are unchanged.

Additional checks passed at the reviewed head:

```text
cargo test --locked -p dash-evo --release --test cbtx_root_history --no-run
<release cbtx_root_history binary> --skip cbtx_mn_list_root_over_history \
  --nocapture --test-threads=1                         # 15 strictness tests
cargo test --locked -p dash-evo --release --lib --test sml_core_vectors --test cbtx
                                                       # 15 + 1 + 6 tests
cargo clippy --locked -p dash-evo --release --test cbtx_root_history -- -D warnings
cargo fmt --all --check
```

Builds and checker invocations used `nice -n 10`, `CARGO_BUILD_JOBS=8`,
`TMPDIR=/work/scratch/sb7-r2-sol/tmp` (0700), and `RUSTC_WRAPPER=`. Scheduling
oversight: the full-history Python launcher also had nice 10, so its nested
`nice -n 10` checker commands inherited effective niceness 19, a further priority
reduction. The full histories were not repeated. Other checker/test invocations
were launched directly at nice 10.

No new C++ oracle comparison, Android link, benchmark measurement, or broad
adapter rerun was needed for this test/comment-only delta. No private paths were
read. Neither the implementation branch nor detached worktree sources were
edited; no commits, pushes, or GitHub actions occurred.

Evidence, fixture generators, the 24-case results, complete replay logs, and
SHA-256 digests of the nine replay input files are retained under
`/work/scratch/sb7-r2-sol/`. The copied/build target was deleted after validation.
This report is uncommitted.

**Verdict:** CHANGES-REQUIRED — M1's completeness and coverage defects are closed;
M2's network-provenance gap prevents unconditional history acceptance.

Sol gate satisfied: no

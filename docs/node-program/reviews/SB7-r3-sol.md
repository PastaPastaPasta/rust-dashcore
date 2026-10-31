# SB7 Sol review, round 3 (narrow)

Reviewed head: `14fdebf2cb4f16155ee1353484a14b0f8564945c`, branch
`node/sb7-sml-core`; review range `2f69c2f00..14fdebf2c`.
Detached review worktree: `/work/scratch/sb7-r3-sol/wt`.

**R2 M2 is closed. One medium proof-input integrity finding remains.** The
network/header/coinbase binding prevents the wrong-network and substituted-CbTx
acceptance investigated here. The proof reader does not, however, fail closed
on the entire proof artifact.

## Findings

### F1 — medium: strict acceptance returns before finalizing the proof stream

Location: `dash-evo/tests/cbtx_root_history.rs:173` (`Chain::block` consumes only
the needed line), `:526` (`check` returns without checking the remaining proof
reader), and `:559` (the proof reader wraps a `MultiGzDecoder`).

After consuming the END proof, strict mode checks the dump's counts and returns
success. It never reads the proof stream to EOF. As a result, a duplicate END
proof, a malformed/partial trailing proof line, or a damaged gzip trailer can
all produce `ACCEPTANCE ... 0 mismatches` and exit 0. The gzip decoder's deferred
error is never observed.

Reproduction uses fresh oracle proofs and six real testnet records at
1,069,870..=1,069,875; END is a genuine testnet checkpoint. The untouched control
passes. Each of the following proof-file changes also passes:

- Append a duplicate proof at height 1,069,875.
- Append `garbled trailing proof`, or an incomplete `1069876 abc` line.
- Remove the gzip stream's eight-byte trailer.
- Flip a bit in its CRC or uncompressed-size trailer field.
- Append an incomplete second gzip member after the complete range.

All four damaged gzip variants independently fail Python's full-stream gzip
decoder. A valid extra higher-height proof is also ignored. In contrast,
missing proofs and duplicate heights before END are rejected.

Concrete command, from the detached review worktree:

```bash
TMPDIR=/work/scratch/sb7-r3-sol/tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=8 \
SB7_HISTORY=/work/scratch/sb7-r3-sol/cases/testnet-valid.jsonl.gz \
SB7_HISTORY_PROOFS=/work/scratch/sb7-r3-sol/cases/proof-gzip-bad-crc.proof.gz \
SB7_HISTORY_NETWORK=testnet SB7_HISTORY_START=1069870 SB7_HISTORY_END=1069875 \
SB7_FULL_EVERY=1 nice -n 10 cargo test --locked -p dash-evo --release \
  --test cbtx_root_history -- --ignored --exact cbtx_mn_list_root_over_history \
  --nocapture --test-threads=1
```

Observed: six headers linked, one anchor matched, six coinbases proven, six
roots compared, zero mismatches; exit 0. The equivalent missing-trailer,
duplicate-END and garbled-tail commands also exit 0. Reproduction generators,
input digests and individual logs are retained in the review scratch directory.

Fix: finalize the proof stream before strict success and propagate every read
and gzip error. For range-sized proof files, require EOF after END and reject
extra lines. If reusing wider proof files is intentional, explicitly specify
that format and validate ignored prefixes/suffixes for complete syntax and
unique, ascending heights while consuming the reader to EOF. The existing
synthetic tests use proofs for heights 0..=20 while checking shorter ranges;
adapt those fixtures or document and test the wider-file policy. Add regression
coverage for duplicate END, trailing garbage, missing/bad trailers and an
incomplete concatenated gzip member.

This is a proof-artifact integrity defect: every requested record in these
reproductions still has a valid anchored header and coinbase proof. It does not
reopen M2 or show acceptance of wrong-network CbTx contents.

## M2 closure and fresh adversarial constructions

The binding is complete for the claimed version and MN-list root:

1. The record hash must equal X11 of its proof header.
2. Heights must be consecutive across all dump files, and each header must
   point to the preceding record's hash; `Chain::prev` persists across files.
3. Every encountered anchor must match, every file must contain one, and END
   must itself be a trusted anchor. Thus the fixed terminal hash binds every
   preceding header to its declared position in that network's chain.
4. The partial merkle proof must reproduce the header root and match exactly
   the supplied coinbase at transaction index zero. The decoded complete CbTx's
   version and `merkleRootMNList` must equal the record's values.

Fresh external constructions, beyond the four supplied regressions:

- **Wrong-network prefix with a genuine mainnet terminal anchor.** Use five
  consecutive real testnet records/headers/CbTxs from 1,069,870..=1,069,874,
  relabel their heights 2,089,995..=2,089,999, then append mainnet's actual block
  2,090,000 and its proof. Each supplied hash matches its own header and each
  CbTx is genuine for that header. The run rejects at 2,090,000 because its
  header does not link to the foreign prefix.
- **Skipped headers across files despite consecutive labels.** File one holds
  the real testnet anchor at 1,069,875. File two supplies testnet's real header,
  CbTx and full list at 1,143,608, relabelled 1,069,876; the proof height is
  relabelled too. With END set to the actual anchor 1,143,608, the initial range
  precondition passes, but the second file fails the header-link check.
- **Reordered headers across files despite consecutive labels.** Replace the
  second file above with real block 1,069,874 relabelled 1,069,876. It likewise
  fails the header-link check at the file boundary.
- The corresponding constructions preserving the original skipped/reordered
  height labels fail the exact-height check.

The current mainnet and testnet checkpoint-plus-manifest height sets have no
intersection, so a shared-anchor-height construction is unavailable at this
head. This was checked against the pinned dash-params source, not assumed.

The parser also rejected 15 required-line mutations (odd/garbled merkleblock
hex, truncated merkleblocks, truncated coinbases and extra fields, each at the
first, middle and final height), all three missing-line placements, and first
and middle duplicate heights. Final duplicate/trailing data and deferred gzip
errors are the F1 exceptions.

## Anchor trust and byte order

Fresh read-only `getblockhash` calls matched **all 28 anchors used by the full
acceptance ranges**: 12 mainnet and 16 testnet, including every manifest entry:

```text
mainnet 1900000 000000000000001b8187c744355da78857cca5b9aeb665c39d12f26a0e3a9af5
mainnet 2000000 0000000000000009bd68b5e00976c3f7482d4cc12b6596614fbba5678ef13a59
mainnet 2090000 0000000000000019f6242b4246bc6bde163a3b4eac87f13fc0a4868f4a8535d7
testnet 1569775 000000b2bffc5aeb51a2bde42b8a2f6cf9daa5ff3223c2cec3d131eddf085a6a
```

Checkpoint values were read from base-sdk revision
`dbe245f7e95a88b0de37ee857f9f02e0de33d275`, the source pinned by this head's
Cargo.lock. The manifest cannot silently replace a checkpoint with a different
hash: `anchors` asserts consistency. All mainnet RPC queries were at or below
2,090,000. Only read-only oracle RPCs were used; no chain or benchmark data was
modified.

`strictness::the_anchors_are_cores_checkpoints_and_the_manifest` passed. It
hashes four captured real headers and compares their internal X11 bytes with
mainnet/testnet anchors, exercising both checkpoint and display-hex manifest
conversion. This pins both byte-order paths; the two remaining mainnet manifest
entries use the same conversion and also matched fresh oracle hashes.

## Runtime scope, validation and limitations

The range modifies only `dash-evo/tests/cbtx_root_history.rs` and adds
`dash-evo/tests/data/sb7_anchor_hashes.txt`. There are no runtime, adapter,
dependency, build-configuration or benchmark changes. Earlier runtime-review
conclusions remain applicable.

Independent validation at the pinned detached head:

```text
cargo test --locked -p dash-evo --release --test cbtx_root_history --no-run
history strictness tests:                         27 passed
fresh real-history controls:                       2 passed
external adversarial/parser cases:                35 executed
  required-line and splice cases:                 25 rejected as expected
  proof-stream integrity cases:                    8 unexpectedly accepted (F1)
cargo test --locked -p dash-evo --release --lib --test sml_core_vectors --test cbtx
                                                  15 + 1 + 6 passed
cargo clippy --locked -p dash-evo --release --test cbtx_root_history -- -D warnings
                                                  passed
cargo fmt --all --check                            passed
```

All builds and CPU work used nice 10, `RUSTC_WRAPPER=`,
`CARGO_BUILD_JOBS=8`, and disk TMPDIR `/work/scratch/sb7-r3-sol/tmp` (0700).
The detached worktree's release target was seeded with a reflink copy of the
implementer's release cache, then rebuilt by Cargo at the pinned head. Targets
were not shared.

Read the supplied `/work/scratch/sb7/accept-final-r2.txt`: mainnet
1,028,160..=2,090,000 has 1,061,841 comparisons, 12 anchors and zero mismatches;
testnet 7,000..=1,569,775 has 1,562,776 comparisons, 16 anchors and zero
mismatches. These full runs were not repeated. The supplied gate output is for
14fdebf2c and ends `GATE EXIT 0`, with 3,592 passes/two dashd failures and the
BL-20 rerun 54/0. This is implementer evidence, distinct from the independent
checks above. No broad gate, new benchmark, or platform build was needed for
this test-only delta.

Evidence, oracle observations, fixture generators, input digests and logs remain
under `/work/scratch/sb7-r3-sol/`. The review build target was deleted after
validation. No private paths were read, no branch was edited, and no commit,
push, GitHub action or subordinate reviewer was used. This report is uncommitted.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no

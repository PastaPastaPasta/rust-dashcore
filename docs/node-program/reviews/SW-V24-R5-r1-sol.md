# SW-V24 R5 (EV-RC4) review round 1

## Scope

Reviewed branch `node/ev-rc4`, range `88b37e6c3..15edfad82`, in
`~/workspace/rdc-wt/ev-rc4`, including the signed merge of
`node/swv24-pinmove` and the oracle change to `375c64d2317`. The review
worktree is clean and was not modified.

The requested path `designs/SW-V24-rc4-pin-delta-ce.md` is absent. I reviewed
the CE packet at
`designs/SW-V24-rc4-pin-delta/ce.md`, together with
`SW-V24-rc4-pin-delta.md`, DEC-461 and DEC-468.

## Findings

### Blocker

None.

### Major

None.

### Minor

None.

### Nit

None.

## Checks with no finding

- **CE4-01 / strict purposes.** The record reader rejects unknown, repeated and
  descending purposes with Core's text at
  [dash-dml/src/codec.rs:482](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/codec.rs:482)
  and [dash-dml/src/codec.rs:501](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/codec.rs:501).
  The payload gate applies the same check to ProReg and ProUpServ at
  [dash-dml/src/build/mod.rs:244](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/build/mod.rs:244),
  using the rc.4 offsets 42 and 36. The SML decoder independently rejects the
  same cases at
  [dash/src/sml/masternode_list_entry/net_info.rs:479](/work/home/agent/workspace/rdc-wt/ev-rc4/dash/src/sml/masternode_list_entry/net_info.rs:479).
  The pre-V24 versus V24 gating remains in the check walk; CE4-03's removed
  entry-loop position is represented by the network-field rows at
  [node-l1/src/dml_checks.rs:193](/work/home/agent/workspace/rdc-wt/ev-rc4/node-l1/src/dml_checks.rs:193)
  and [node-l1/src/dml_checks.rs:244](/work/home/agent/workspace/rdc-wt/ev-rc4/node-l1/src/dml_checks.rs:244).

- **CE4-03 / reject order and activation.** The ProReg walk puts the invalid
  entry at the network-fields step after `bad-protx-operator-reward`, and the
  ProUpServ walk does the same after its earlier trivial checks; the rows and
  callers are explicit at
  [node-l1/src/dml_checks.rs:193](/work/home/agent/workspace/rdc-wt/ev-rc4/node-l1/src/dml_checks.rs:193),
  [node-l1/src/dml_checks.rs:244](/work/home/agent/workspace/rdc-wt/ev-rc4/node-l1/src/dml_checks.rs:244)
  and [node-l1/src/dml_checks.rs:837](/work/home/agent/workspace/rdc-wt/ev-rc4/node-l1/src/dml_checks.rs:837).
  The strict ExtNetInfo difference is reachable only for v3 under V24; before
  V24 the version gate wins, preserving Core's pre-V24 reject ordering/string.

- **CE4-02 / RFC1918.** The implementation ignores `allow_private_net` for
  routability and calls `is_routable(false)` at
  [dash-dml/src/netinfo/mod.rs:123](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/netinfo/mod.rs:123).
  Legacy and extended vectors cover the RFC1918 range edges and the
  regtest exception at
  [dash-dml/src/netinfo/tests.rs:558](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/netinfo/tests.rs:558).

- **CE4-04 / operator-key scheme bit.** `PUB_KEY_OPERATOR` is set when
  either the captured serialization or the state-selected scheme changes at
  [dash-dml/src/diff.rs:244](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/diff.rs:244).
  The equal-byte migration cases are tested in both directions at
  [dash-dml/src/diff.rs:529](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/diff.rs:529).

- **CE4-05 / checked credit-pool sums.** Per-unlock, lock-session,
  unlock-session and total-balance arithmetic use checked operations and the
  required reject/overflow mapping at
  [dash-dml/src/creditpool.rs:233](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/creditpool.rs:233),
  [dash-dml/src/creditpool.rs:419](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/creditpool.rs:419).
  Int64-edge vectors assert the three transaction strings and the
  `failed-checkcreditpooldiff` path at
  [dash-dml/src/creditpool/tests.rs:507](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/creditpool/tests.rs:507).

- **D1–D6/D5b.** The codec vectors make `a..b` (D5) invalid, keep
  `a.b` (D5b) as the valid control, and retain the second entry after the
  cleared D5/D6 first entry at
  [dash-dml/src/codec/tests.rs:673](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/codec/tests.rs:673).
  The oracle tests independently require strict-purpose decode errors and
  cleared-entry behavior at
  [dash-oracle-sys/tests/netinfo.rs:392](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-oracle-sys/tests/netinfo.rs:392).

- **Mutation sensitivity.** In a detached scratch copy, reverting each of the
  strict-purpose guard, RFC1918 argument, operator-key scheme clause and
  checked unlock sum made its corresponding focused test fail:
  `records_read_purposes_strictly`,
  `rfc1918_is_never_routable`,
  `a_scheme_change_with_equal_bytes_is_recorded` and
  `diff_sums_are_checked`. The tests therefore detect the intended code
  mutations and are not tautological.

- **R6 workarounds.** The fixed-offset payload guard is explicitly documented as
  compensating for base-sdk's first-wins purpose reader at
  [dash-dml/src/build/mod.rs:244](/work/home/agent/workspace/rdc-wt/ev-rc4/dash-dml/src/build/mod.rs:244).
  The conformance code marks the five temporary base-sdk divergence vectors as
  R6 workarounds at
  [conformance/src/verbose.rs:1942](/work/home/agent/workspace/rdc-wt/ev-rc4/conformance/src/verbose.rs:1942).
  No unrelated workaround or new `#[ignore]` was added.

- **Pins and SB4.** `dash-oracle-sys/pins.txt` moves to commit
  `375c64d2317a787b868afe8b63e96d79079c90a4`, branch
  `node/ev-rc4-oracle`, with the corresponding plain/fuzz hashes. The SB4
  gzip re-record comparison found semantic JSON fields unchanged; only
  provenance and `/vector/instructions_u` changed, in the two expected
  instruction-count files.

## Validation

Using `TMPDIR=/work/scratch/ev-rc4-sol-r1`, a target directory below that
path, and `CARGO_BUILD_JOBS=12`:

- `cargo test -p dash-dml --all-features`: passed (179 tests; 2 existing
  ignored).
- `cargo test -p dash-evo --all-features`: passed.
- `cargo test -p dash-oracle-sys --all-features`: passed.
- `cargo test -p node-l1 --all-features`: passed.
- `cargo test -p dashcore --all-features`: passed.
- The conformance fixture target passed with the one pre-existing
  `unknown_special_type_is_unsupported_by_both_libraries` test excluded
  (14 passed). That test fails identically on both `15edfad82` and the
  range base `88b37e6c3`, so it is not attributable to this range; its
  mismatch is the existing `bsdk.payload` handling for type 10.
- `cargo fmt --all -- --check`: passed.
- `git diff --check 88b37e6c3..15edfad82`: passed.
- No new `#[ignore]` directives were found in the range.

## Verdict

**ACCEPT**


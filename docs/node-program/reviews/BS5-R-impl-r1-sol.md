# BS5-R implementation review, round 1 (Sol)

**Verdict: CHANGES-REQUIRED. Findings: 0 M / 2 m / 0 n.**

Target: `node/bs5r` at `430f5c7723164fa6d5219f28b214b442dfe4f555`, in `/work/home/agent/workspace/bsdk-wt/bs5r`. F2–F8 reviewed against `56c4cd90`; F1 checked separately against upstream `89828032` and the merge conflict resolutions. References: the accepted BS5-R rev2 plan, DEC-900/903/907/911/943, and Dash Core `v24.0.0-rc.4` / `v23.1.8`.

The blocker is a downstream API gap: the node's existing three-deployment MNHF tests cannot be faithfully adapted with the public API. The second finding concerns preservation of the required oracle coverage. I found no additional wrong network default, table value, genesis serialization, or option result in the reviewed domain. The accepted u16 refusal, unsigned-height limitations and checkpoint allocation are not new findings.

## Findings

### m-1 — Required corpus cases can disappear without failing either equality test

Commit: `7f59383f` (F6). Locations: `pkgs/params/tests/core_corpus.rs:429-431`; `pkgs/primitives/tests/chainparams_blocks.rs:85-109`.

Concrete failing input: delete the `error/llmq_range_then_dip0024` record (`-regtest -llmqtestparams=-1:2 -llmqtestinstantsenddip0024=bogus`) from either chain-params corpus. The params test only checks `cases.len() > 100`, then visits the records still present. The primitives test skips error records altogether. The removed required error-order case is therefore undetected. Deleting all ten new cases also leaves each file above the count threshold. Pin the required case names and their chain/arguments (and reject duplicates), or compare the corpus census with an independently maintained manifest. This is a coverage-integrity defect, not evidence that the current builder mishandles that option set.

Reproduced in the isolated copy: removing this record from both pins left both comparison binaries passing. Removing all ten new records per pin (188 → 178 cases) also left both binaries passing. In contrast, changing a deviation mark from 46 to 47, appending text to an expected error, or incrementing a base's default port caused the params comparison to fail. Thus existing records are checked, but required coverage can silently disappear. Experiment script, original corpus backups and logs: `/work/scratch/bs5r-impl-r1-sol/review-tmp/`; summary: `corpus-mutation-results.json`. The corpora were restored byte-for-byte afterward.

### m-2 — The mutable deployment accessor is inaccessible to downstream MNHF tests

Commit: `7347bfdd` (F4), with the non-public `shares` field introduced by `2d599a51` (F2). Location: `pkgs/params/src/setup.rs:306` (also `setup.rs:280`).

Concrete input: the existing node test `dash-dml/src/mnhf/tests.rs:140` at `832e99bf2`, `activation_by_window_and_use_ehf`, writes `(bit, start_time, timeout, use_ehf) = (9, T + 30, T + 40, false)` into deployment index 2. It must mutate the real third deployment to preserve its three-overlapping-windows test. `dash-dml/tests/mnhf_step_vectors.rs:126` also fills every deployment from its oracle vectors. The old public Vec allowed this. In the replacement, `shares` is non-public, `get`/`iter` borrow immutably, and `deployment_mut` is `pub(crate)`. A downstream caller cannot mutate the third deployment at all; `-vbparams` cannot change its bit. Mutating the dereferenced `Bip9Deployments.evo_shares` would change the shadowed copy, not the release-aware deployment the node reads.

Confirmed in the isolated review copy: an integration-test caller of `p.consensus.deployments.deployment_mut(DeploymentPos::EvoShares)` fails with E0624, pointing at `setup.rs:306`. Make this release-aware accessor public (or provide an equivalent public mutable accessor); returning `None` at V23_1 retains the intended release guard. This needs to land in the fork before the node-side adaptation can retain those tests faithfully.

Minimal downstream probe:

```rust
let mut p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
    .build().unwrap();
p.consensus.deployments.deployment_mut(DeploymentPos::EvoShares)
    .unwrap().bit = 9; // E0624: method is non-public
```

Probe retained as `pkgs/params/tests/review_public_deployment.rs.disabled` in the isolated copy. This is a gap in the planned API's ability to support the existing caller, rather than an objection to making the third deployment optional/non-public or to using release-aware iteration.

## What I checked

### F1 and fork boundary

- `git diff --exit-code 89828032 56c4cd90 -- pkgs/params` is empty.
- Read F1's remerge diff outside params. The resolutions are mechanical import/type changes, constructor updates and adaptations to upstream's API; no unrelated new behavior found there.
- Checked the F2–F8 changes against the upstream-shape restrictions. The existing mainnet/testnet/regtest tables acquire the four planned consensus fields; their existing values are retained. The new devnet table and release overlays carry the additive BS5 behavior.
- Checked commit naming, the existing CodeQL type policy (`enforced, no-serde, unencodable`), new type derives, and the existing lint/semgrep configuration. No correctness finding from those checks. No CodeQL run. The known pre-existing libz-sys lint failure was excluded as instructed.

### ChainSetup and option parity

- Compared the regtest/devnet option phases and error messages with Core's constructors, option parsers, `init.cpp` spork initialization and `spork.cpp` signer resolution, at both pins. Checked version-bits arities and field parse order, optional `-1` sentinels, repeated settings/negation, activation-name lookup, DIP3/budget heights, fast pruning, devnet naming, signed subsidy parameters and target spacing.
- Checked configured-quorum name lookup and rotation requirements for the ChainLocks, DIP0024 InstantSend, Platform and MNHF roles. Checked all four size-override options, threshold-derived fields, v24's cap after both integer parses, and the absence of that cap at v23.
- Verified quirk 46 records the first unrepresentable override and waits until later Core parsing, role checks, constructor invariants and spork validation have passed. Ignored-network overrides remain ignored. The refused domain matches §2h: v24 negative size or threshold outside u16 after Core's size cap; v23 size or threshold outside u16.
- Checked the release-aware deployment view, clone/method resolution and the builder's synchronization of both static consensus copies. V23 exposes two deployments with bits 28/12, omits EvoShares from the release view and omits the Platform HRP. The inactive static third deployment follows the accepted plan. The downstream mutable-access regression is m-2.
- Checked spork network prefixes, checksum and length validation, NUL and whitespace handling, Platform HRP precheck, v23 distinct-key ordering/majority threshold, and v24 last-address/warning behavior. The hash-blob ordering matches Core's raw-byte set ordering.
- Core's listed chain-params/LLMQ/init/spork sources are unchanged between rc.3 and rc.4, supporting the accepted v24 corpus reuse. `src/kernel/chainparams.cpp` does not exist at the referenced rc.4 tag; the implementation is in `src/chainparams.cpp`.

### Genesis and corpus coverage

- Reviewed coinbase transaction bytes, CompactSize lengths, script push boundaries, reward, merkle calculation, header endianness, time/bits inheritance, X11 target comparison and the nonce search bounds against Core. Checked primitives' conversion from both genesis descriptors.
- Checked the named-devnet height-1 checkpoint and the leaked static slice. The builder has the required startup/tests-only, never-per-request documentation (DEC-911).
- Both corpora have 188 uniquely named cases; all 178 earlier case names remain. The four base dumps per pin are unchanged from `1eda0100`. The changes to old case results are the intended split of formerly mixed accepted/refused overrides and the v23 out-of-u16 refusal.
- Each pin contains the required ten additions: three later-error precedence cases, two ignored-network successes, and five range witnesses. At v24, the size-70000 witness correctly carries Core's maximum-size error, rather than deviation 46. The expected marked-refusal counts are six at v23 and four at v24.
- The table dump comparison covers consensus fields, deployments, address/network fields, seeds, checkpoints, extras, configured LLMQs and all available static LLMQs. The primitives comparison checks the two block-byte fields excluded by params. Pins and BRR values have separate tests. The corpus includes named-devnet direct/PUSHDATA1/PUSHDATA2 examples; the primitives boundary tests also cover lengths 0, 1, 75, 76, 255, 256 and UTF-8.
- Additional combinations such as `-regtest -llmqtestparams=-1:2 -sporkaddr=invalid` are absent from the corpora but exercised by `builder.rs`. I found no missing member of the specifically required ten-case addition. The failure to enforce their continued presence is m-1.

### Call sites and p2p_core

- Compared production p2p_core against `1eda0100`: import moves, widening the now-u16 DKG parameter to u64, and reuse of `MAX_LLMQ_SIZE`. The old negative-to-zero fallback lies outside the newly accepted u16 domain; no other production behavior change found. Reviewed the intentional skipping of negative-quorum corpus inputs in the adapted tests.
- Checked the `build() -> Result` adaptations in fork consumers, and `Network` discriminants/order against the old network order. No further call-site regression found. Existing node MNHF test callers were examined at `832e99bf2` to establish m-2; the node-side R-commits themselves are outside this review.

## Validation and limits

All builds ran in the reflink copy `/work/scratch/bs5r-impl-r1-sol`, with `nice -n 19`, `CARGO_BUILD_JOBS=8` and `TMPDIR` inside the copy. Resource checks before building satisfied ≥230G free and five-minute load <80. The copy is retained. The reviewed worktree was not edited.

Passed, 174 tests total:

- `cargo test -p dash-params --all-features --test core_corpus` — 10 tests, including both complete corpora.
- `cargo test -p dash-params --all-features --lib --test builder --test setup --test brr_payments` — 127 tests.
- `cargo test -p dash-primitives --all-features --test chainparams_blocks --test genesis_valid` — 29 tests.
- `cargo test -p dash-script --all-features --test spork_key_ids` — 8 tests.

The public-access probe failed as expected with E0624. Corpus mutation outcomes are recorded above; these intentional failures are separate from the passing baseline suites.

I did not rebuild/replay fresh Core oracle binaries, run the full workspace/node adaptation build, or run CodeQL. Oracle provenance and rebuilt dump-binary hashes were treated as accepted by the brief. The numbered departure 2–12 report was not available in the supplied brief/plan/notes, so I reviewed the resulting implementation against the accepted plan and Core rather than claiming a numbered departure-by-departure audit. No commits, pushes or external comments were made.

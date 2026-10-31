# Combined BS5 + BS6a re-pin — Sol review, round 1

Reviewed 2026-10-09: `node/repin-bs5-bs6a`, head `302157a5`, the 17 commits after `b36afd84`. Scope is the rust-dashcore changes; the already-reviewed fork implementation was inspected only to establish the dependency and BLS contracts needed by this review. Review worktree: `/work/scratch/repin-sol/wt`, initially detached at the requested head.

## Verdict

**APPROVED WITH SUGGESTIONS.** No blocking correctness or consensus finding. The requested tests and all three historical build spot-checks passed. One non-blocking regression-test suggestion follows.

## Findings

### Low S1 — Preserve a successful, scheme-sensitive CS0 regression test

**Commit:** `1f3e6206` (16). **Location:** `dash-consensus/src/primitives/tx.rs:240` (especially lines 245–248).

`the_payload_is_decoded_once` constructs the existing `special()` fixture with a three-byte payload. That cannot decode a CbTx, so this checks reuse of a cached error, plus the absence of a payload on a spend. It does not exercise a successful CbTx/asset-unlock decode or the signature validity/serialization difference between basic and legacy contexts.

The implementation is sound on inspection; this is a coverage suggestion, not a demonstrated behavior bug. Add successful Core-recorded CbTx and asset-unlock samples and resolve each field in alternating basic/legacy/basic order from the same cached `PayloadDecode`, checking Core's expected bytes and that the payload reference stays the same. A temporary reviewer probe covers this scenario; its result is recorded below and its source is retained outside the worktree with the review logs.

## Consensus assessment of commit 16

- `dash-consensus/src/primitives/tx.rs:139–153`: the only retained state is `OnceLock<Box<Result<PayloadDecode, PayloadError>>>`. It stores the scheme-independent decode, not a resolved eager signature. At this pin CbTx `best_cl_signature` and asset-unlock `quorum_sig` are `BlsGlobalSig`. Its decode accepts zeros or bytes canonical in either scheme; `get(ctx)` independently obtains the eager value under the requested scheme. There is no first-caller scheme captured in the CS0 slot.
- A repository-wide search for `.payload(`, `Tx::payload`, `BlsGlobal`, signature-field access, and BLS-context use found **no production CS0 `Tx::payload()` consumers**. Its three calls are the new unit test at lines 246, 248 and 249. Other methods named `payload()` belong to address/wallet types. The implementer's claim is correct for this head.
- The separate conformance consumers do not use CS0's cache: `conformance/src/payload.rs:61` resolves globals through `get(ORACLE_BLS)` for field comparisons, and `conformance/src/verbose.rs:941` / `:996` use that helper for RPC output. `conformance/src/check.rs:771` explicitly fixes the oracle context to basic because the supported sampled oracles' tips are past V19. Version-sourced and lazy fields retain their distinct paths.
- A1 §1.4.1 requires block-height context during block processing and tip context for mempool/RPC/other uses. `dash-consensus/src/ctx.rs:104` still selects legacy iff `height < V19Height`; its boundary test remains. Future rule consumers must resolve globals under their own correct context and must not carry a resolved value across context changes. The API permits a caller to supply the wrong context, as the previous API also did, but this patch introduces no such caller or cached cross-context result.
- DEC-79 governs the call-level pool bans and the audited dependency pin, not a requirement for two CS0 slots. The refactor adds no verification call. DEC-98's accepted Q-9/Q-12 direction may remove/rework `BlsGlobal` later; this interim change does not prevent that work or establish a permanent upstream API requirement. Payload layout/release keying remains the BS6c follow-up and is not solved by retaining a redundant scheme slot.

## DEC-79 re-audit verification

For commit `7b018077` (12), the stamp in `contrib/node/pure-clippy.toml:24` names `blst 0.3.17` and `a1d4dd3bbed0f78f89c04d92d160f6cbd0863275`. `contrib/node/check-pure.sh:98` checks it against the configured fork pin; `:113` checks each normal closure's blst version.

I inspected `git diff 73ab4b0..a1d4dd3` in the fork:

1. Changes are in `pkgs/params`, `pkgs/dev/src/bin/bsdk_util/bspcheck.rs`, `pkgs/primitives/tests/payload_core.rs`, and a documentation-only change in `pkgs/pkc/src/bls/wrap_global.rs`. **Literal qualification to the report:** root `Cargo.lock` changes too, by one added `dash-dev` dependency entry corresponding to the new dev dependency. No additional executable library change outside params is hidden by that description.
2. `pkgs/params/Cargo.toml` changes only `[dev-dependencies]`, adding `dash-dev`; its normal dependency declarations and features are unchanged.
3. Ran `cargo tree --locked -p <crate> -e normal --all-features` using rust-dashcore commit `50bdf1e8` at `73ab4b0` and head `302157a5` at `a1d4dd3`. After normalizing only source revision hashes and worktree paths, the complete trees are identical: `dash-sha256` 1 line, `dash-consensus` 122 lines, `dash-x11` 1 line. The consensus closure still contains `blst 0.3.17 → threadpool 1.8.1`.
4. The BLS/pkc executable implementation is unchanged across the audit interval. The newly added params source has no direct blst or BLS-wrapper calls; its new parameter construction/genesis/address paths do not enter pooled BLS verification or multi-scalar methods. There is no new blst call path requiring an additional ban in this re-stamp. The existing 34 bans remain in place, including the wrapper/scheme methods; the re-export is the same underlying item.

Raw dependency trees and the non-params fork diff are retained in `/work/scratch/repin-sol/logs/`.

## Release-reference guard and BL-40

**Commits:** `7f83df08`, `deb232f0`, `de32a922` (10, 11, 15). **Locations:** `contrib/node/pin-delta/check-release-refs.sh:133–150`, BL-40 selftests at `:239`; `dashd-store/src/flatfile/blocks.rs:388`.

The exception is keyed by exact path and the complete trimmed production line, with a budget of one per line. Consuming an exception decrements that budget. It does not exempt the rest of `blocks.rs`, adjacent modules, or another copy of the same match. The selftest rejects an extra reference in the allowed file, a duplicate allowed line, a reference in the adjacent module, and the same allowed text in another file. It also checks production before/after inline tests and production beside out-of-line test modules. Both `--selftest` and the actual repository check passed.

Commit `71611d50` (14) changes the store's type to the `dash-consensus::Release` re-export of `dash_params::Release`; the two rev-file-finalization branch expressions are unchanged. BL-40's eventual rules/profile migration remains explicit.

## Other changes and v24 behavior

Commit `7b018077` maps the prior network-string/devnet override views to the new `ChainParams` fields without changing the PoW, subsidy, or payment algorithms. `15b68302` ties the tolerance pin's presence to mainnet, rejects mismatched network labels, and checks the Core-recorded view construction. `node-fuzz/src/lib.rs:141` correctly uses CS0 `parse_release`, preserving the prior accepted release spellings rather than silently adopting a different SDK parser.

The conformance adapters add the new v24 payload fields/types, resolve BLS according to field source, and compare NetInfo as Core holds it. The updated fixture expectations distinguish a known type's decode failure from an unknown type. The expected base-sdk stable asset-unlock txid/merkle discrepancy is still reported explicitly by `conformance/tests/v24_findings.rs:70`; it is not silently accepted by CS0.

The BS5 dump builder uses fresh archived Core sources by default, checks tracked source blobs before/after building, anchors all build-tree files/symlink targets for an opt-in reuse, and records source/archive/binary provenance. Its generator verifies the binary, pin, tree and fresh-build marker before accepting output and checks the shared params subset against the pinned oracle archives. I reviewed these additions but did not regenerate the full fork corpus or rebuild both Core dump trees.

Commit `302157a5` adds sweep provenance. Comparing `81117ec8` with the reviewed head confirms identical executable sources under `dash-consensus`, `contrib/node/cs2-diff`, and `dash-oracle-sys`; only the provenance file differs within those paths. I did not independently rerun the billions-of-input exhaustive sweeps, so their recorded success is implementer evidence rather than a fresh reviewer run.

## Local validation

The shared sccache queue stalled the initial test invocation; subsequent build/test commands use `RUSTC_WRAPPER=` for this review only, retaining `TMPDIR=/dev/shm/repin-sol` and `CARGO_BUILD_JOBS=8`, and keeping the worktree's own `target/`.

- At `302157a5`: `cargo test -p dash-consensus -p dashd-store -p dash-conformance` — exit 0; **353 passed, 0 failed, 3 ignored**, including doc tests. Ignored: the existing forged-undo-size case, and the two disk-full/background-error tests requiring `DASHD_STORE_SMALL_FS`. These were not forcibly un-ignored.
- Temporary reviewer test: `cargo test -p dash-consensus --test repin_review_probe` — exit 0, 1 passed. Four Core-recorded transactions (CbTx basic/legacy signatures and asset-unlock basic/legacy signatures), each resolved basic/legacy/basic/legacy from the same CS0 payload, match Core's exact signature bytes; the fixture expectations agree at both Core pins. Probe source: `/work/scratch/repin-sol/logs/repin_review_probe.rs`; output: `payload-probe.log`. The temporary source was removed from the worktree before historical builds.
- `check-release-refs.sh --selftest`, the repository release-ref check, `bash -n` on the changed checks/build scripts, and `git diff --check b36afd84..302157a5` — passed. The BS5 generator's case-only outputs also generated successfully.
- Historical `cargo check --workspace --locked` — all exit 0, without editing manifests or lockfiles:

  | Commit | Position | Fork pin | Result |
  | --- | --- | --- | --- |
  | `50bdf1e8` | 3, BS6a conformance expectations | `73ab4b0` | passed |
  | `7b018077` | 12, combined re-pin/adapt/re-stamp | `a1d4dd3` | passed |
  | `1f3e6206` | 16, single payload slot | `a1d4dd3` | passed |

  Commands ran sequentially in the detached worktree, checking out each exact commit and then returning to `302157a5`. Logs: `/work/scratch/repin-sol/logs/check-<commit>.log`; exit codes: `check-results.txt`.

Cleanup completed: the clean detached worktree was removed with `git worktree remove`, its absence was verified on disk and in Git's worktree list, and the temporary historical source archive and empty TMPDIR were removed. Review logs and the probe source remain under `/work/scratch/repin-sol/logs/`. No repository implementation changes or commits were made; this report is intentionally uncommitted.

## Open questions / limits

No blocking open question. This is a review of the current rust-dashcore tree, not proof of future EV/asset-unlock consumers, a fresh full-chain conformance pass, or a re-review of the fork. Three historical builds are spot-checks, not a claim that all 17 commits were individually built by this reviewer.

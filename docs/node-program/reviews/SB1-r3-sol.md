# SB1 layer gates — Sol r3 (narrow)

**Verdict: CHANGES-REQUIRED.** Reviewed `node/sb1-layer-gates` at **3cd103974941a60017b2aa7397b762d3d3ef11b7**, against the r1/r2 Sol reports and DEC-170 as amended at cb9506b91. The original plants have been addressed, but the mechanisms still have the gaps reproduced below. Findings concern gate coverage.

| Finding | Closure |
|---|---|
| R2-1 | The original committed-record probe now fails the second +200 KiB step against the fixed 12,615,752 B reference. The default integration-row lookup also defeats a locally rewritten DEC-170. The immutable-reference mechanism remains incomplete through `L7_BASE_REF` (R3-1). |
| R2-2 | The original external `dashd-p2p → dashd-wallet → key-wallet` plant now fails L3 at the external hot-path root. General graph and ban checks visit all classified resolved IDs. The separate `pure = true` rule still has a member-only gap (R3-3). |
| R2-3 | Closed for the original proc-macro-generated allocator in a normal dylib dependency: the linked consumers reject it on host and Android, with all features and with none. The linked-product approach is the right replacement for source/rlib scans, but its feature coverage misses an ordinary default build (R3-2). |
| R2-4 | Closed. Both the Android-only leaf baseline and that same graph after adding the Linux-only declaration pass. L6 now uses platform-filtered metadata and evaluates each declaration's condition. Nested `any/all/not`, empty `all()/any()`, and a trailing-comma case also produce exactly the three applicable declarations under each product/target, with the two inactive declarations omitted (12 offending edges in the positive-control plant). |
| R2-5 | Closed. Allocator-probes case 1 now passes the full L5 gate: the allocator-bearing host build helper stays outside the normal consumer linkage on both host and Android. |

**R3-1 — major — `L7_BASE_REF` can redirect both the immutable size reference and the authoritative decision lookup.**

Refs: `contrib/node/check-layers.py:624`, `:640`, `:683`, `:689`.

With the default ref, a 13,025,352 B measurement correctly fails against 12,615,752 B: growth **409,600 B**, allowance **252,315 B**. In a detached scratch history, record 12,820,552 B (+200 KiB), commit it, then make a further signed empty commit. Set `L7_BASE_REF` to the first scratch commit, or to `HEAD`. The exact same 13,025,352 B measurement returns **0**: its reference has become 12,820,552 B and the reported growth is only 204,800 B. No ruling is supplied. The special `HEAD` case chooses the previous same-branch record through the first-parent fallback.

The override also defeats bootstrap's integration-row authority. Set `[size].bytes` to 13,025,352 and rewrite the local DEC-170 row to state that full key. With the default ref this fails; with `L7_BASE_REF=$(git merge-base HEAD node/integration)` it passes, because that older ref lacks DEC-170 and `logged()` falls back to the rewritten tree row. Thus even reusing the old decision ID can move the constant.

Reproduction: `/work/scratch/sb1-r3-sol/extra-size.py`; results in `logs/extra-size.run.log`, `size-rewritten-row-{default,redirected}.log`, and `size-redirect-{default-control,branch-record,head}.log`. The scratch commits report signature status **G**.

Suggested fix: keep the merge gate bound to `node/integration`, and keep the authoritative decision lookup bound to that branch independently of any test/comparison override. If alternate refs are needed for fixture testing, mark those runs incomplete/non-gating, or validate them against the actual integration ref so a record from this change cannot become the reference. Do not let an arbitrary `HEAD` alias activate the integration first-parent exception.

**R3-2 — major — the allocator consumer never tests default features, so a normal default build can install an allocator while the full L5 gate passes.**

Ref: `contrib/node/link-probe.py:93`.

Plant a workspace S `dash-chain` with:

```toml
[features]
default = ["std"]
std = []
consumer-allocator = []
```

```rust
#[cfg(all(feature = "std", not(feature = "consumer-allocator")))]
#[global_allocator]
static A: std::alloc::System = std::alloc::System;
pub fn touch() {}
```

`check-shared.sh` returns **0**, including its release consumers on host and Android; the layer gate also returns **0**. All features enable `consumer-allocator`, removing the attribute, and no default features disable `std`, also removing it. The ordinary default configuration enables `std` without `consumer-allocator` and therefore installs the allocator. Using the same release consumer mechanism with `--features=std` (defaults remain on, and `consumer-allocator` remains off) reports `installs a global allocator: dash_chain` on both host and Android. The control script asserts those conflicts.

Reproduction: `/work/scratch/sb1-r3-sol/extra-allocator.py` and `allocator-default-proof.py`; gate evidence in `alloc/logs/allocator-default-feature-{gate,layers}.log`, and the corrected product controls in `alloc/logs/allocator-default-feature-product.log`.

Suggested fix: include ordinary default-feature products in the allocator matrix. Define and check the supported product feature configurations rather than treating the two extreme feature sets as exhaustive. The same selection issue applies to P consumers using this helper; source and rlib scans need not return.

**R3-3 — major — external copies of a name marked `pure = true` can escape L4 when their layer is outside P/Pn/S.**

Refs: `contrib/node/check-layers.py:430`, `:434`, `:787`, `:790`, `:792`.

The external-package fail-closed check tests only the layer P/Pn/S. `crates P Pn pure`, which supplies all roots to `check-pure.sh`, still reads workspace-only metadata. Therefore the documented `pure = true` extension does not apply to every resolved ID with that name.

Plant `[crates] dashd-review-pure = { layer = "N", pure = true, pending = "review probe" }`. Resolve an external path package of exactly that name under the existing N `dash-rpc-spec`, with this compiled library body:

```rust
pub fn read() -> std::io::Result<String> {
    std::fs::read_to_string("probe")
}
```

The full layer gate returns **0**. `crates P Pn pure` returns **0** but omits this package, so L4 never visits its body. Its release library build succeeds. This is the promised name-keyed purity rule escaping, rather than a missing denylist entry: `std::fs::read_to_string` is already forbidden by L4.

Reproduction: `/work/scratch/sb1-r3-sol/extra-graph.py`; `graph/logs/outside-pure-name-{layer-gate,enumeration,build}.log`.

Suggested fix: apply the name-keyed pure flag to all resolved IDs. Extend the external fail-closed check to names carrying that flag regardless of layer, or lint each such ID. A workspace-only enumeration is sufficient only when the complementary rejection covers every rule-selected external root.

Integration verification: in a detached copy of **node/integration at f56d7044c**, the L1/L2/L3/L8 graph and all four L6 product/target checks pass. The overlay preserves integration's manifests, lockfile and SDK pin. Three mechanical layer-table updates were needed for crates that landed since this branch's base: add `dash-filters` as P, and remove `pending` from `dash-p2p-transport` and `dashd-chainstate`. Integration's existing SDK audit stamp was retained. All **28 release consumer links** pass for integration's seven P/Pn/S libraries (`dash-sha256`, `dash-x11`, `dash-consensus`, `dash-evo`, `dash-dml`, `dash-filters`, `dash-p2p-transport`): host and Android, all features and no default features. Results are in `logs/integration-links.json` and the per-crate `integration-link-*.log` files. The L7 arithmetic accepts the reported **12,615,752 B** at the stated environment; I did not independently remeasure that Android artifact. No false failure from the changed graph mechanisms was observed on integration's real graph.

Verification limits: the five requested r2 finding plants were rerun, plus the three new reproductions and target-condition controls. The completed macro/dylib L5 invocation is the closure evidence; its later redundant rlib comparison was stopped. I did not rerun the full 265-case selftest, full integration gate, or Apple lint. `cdylib`-only and `staticlib`-only S roots fail closed as unsupported library targets, rather than bypassing the allocator check. The thread checks' retention in clippy/cargo-deny is justified: an empty linked consumer strips uncalled spawn/pool code and cannot prove absence of library call sites. No accepted denylist, name-matching, or BL-74 limit is reopened. No private material or excluded marked module was inspected. All plants and their normal signed experiment commits stayed under `/work/scratch/sb1-r3-sol/`; the reviewed branch was not edited, committed, or pushed. TMPDIR is on disk there, `RUSTC_WRAPPER` is empty, builds use six jobs and nice 10. Private offline Cargo homes reuse registry/git data; each worktree owns its target directory.

Sol gate satisfied: **no**.

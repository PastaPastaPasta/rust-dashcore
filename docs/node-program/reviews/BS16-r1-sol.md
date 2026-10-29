# BS16 round 1 — Sol review

Under review: `node/bs16-network-kind`, `13257a2624cd4c9af5201d711ffeb1627ccc70c6`, one commit on `e8cdec55d63aa83ef29c0df0bedfe7f7ab2d9b74` (6 files, +265/-1). Independent review in detached head/base worktrees under `/work/scratch/bs16-review/`, with `TMPDIR=/dev/shm/bs16-review-tmp` (0700) and `CARGO_BUILD_JOBS=8`. The branch under review was not modified; no commits, pushes or GitHub comments were made.

## Findings

### Minor — the new helper warning is hidden from public API documentation

**Location:** `dash-network/src/params.rs:14`; private module declaration at `dash-network/src/lib.rs:26`.

The warning about `v20_activation_height()` returning 0 for devnet/regtest, versus Core's defaults of 2/432, is a module-level `//!` comment inside private `mod params`. Normal public rustdoc omits that module's documentation. The public conversion methods only describe conversion and show examples; the existing height helper still says these networks activate at height 0. A consumer reading the public bridge API can therefore miss the explicit warning and assume the legacy helper supplies ChainParams' consensus activation heights.

**Suggested fix:** expose the kind-only semantics, devnet-name handling and helper-value warning in the public conversion method documentation or crate documentation, and clarify `v20_activation_height()`'s documentation without changing its return values. Nonblocking documentation issue; the conversion itself does not read or alter activation heights.

## Verified behavior and API judgment

- The resolved SDK pin is `4b4e76b27c33d7c78ec4d6a5ae462b59291eabe2`, inherited through the existing workspace dependency and patch. Its `NetworkKind` is an exhaustive `#[repr(u8)]` enum with exactly the four unit variants and explicit 0–3 discriminants. Its documentation explicitly keeps a devnet's name in chain parameters. The matches and both `const fn` declarations are correct.
- `From` is appropriate for these current types: every value has a lossless counterpart and there is no honest failure case. The concern about a future named devnet variant does not justify inventing one now. Adding a variant or changing `Devnet` into a payload variant would already change the SDK's public enum contract and break these exhaustive matches at compilation; that pin update must revisit conversion semantics. `TryFrom` today would not automatically preserve names in a future representation.
- `ChainParamsBuilder::devnet_name` is the actual named-devnet API at the resolved pin. The test's named builder construction succeeds for all four kinds. Core's `src/chainparams.cpp` at both v23.1.8 and v24.0.0-rc.3 agrees with the tested magic bytes and ports (9999, 19999, 19799, 19899); devnet V20 is 2 and regtest defaults to DIP0003Height=432.
- A repository-wide search for `NetworkKind`, `dash_params`, and chain conversion matches found no existing `Network` ↔ `NetworkKind` mapping to reroute. Other mappings are to distinct domains (FFINetwork, subsidy categories, Core chain ID strings). `dash-spv-ffi/src/config.rs:121` still separately chooses devnet port 29999 instead of 19799; this predates BS16 and is the already-recorded BL-54, not a regression here.

## Encoding and FFI comparison against the base

An identical standalone snapshot program was built against the exact base and head `dash-network` paths with `ffi`, `serde` and `bincode` enabled. Both builds resolved the same serde 1.0.229, serde_json 1.0.151, grovedb-bincode/grovedb-bincode-derive 2.1.0 and cbindgen 0.29.4. Their complete runtime output was byte-for-byte identical:

| Network | Network / FFINetwork value | serde JSON | Standard bincode | Fixed-integer bincode |
|---|---|---|---|---|
| Mainnet | 0 / 0 | `"mainnet"` | `[0]` | `[0, 0, 0, 0]` |
| Testnet | 1 / 1 | `"testnet"` | `[1]` | `[1, 0, 0, 0]` |
| Devnet | 2 / 2 | `"devnet"` | `[2]` | `[2, 0, 0, 0]` |
| Regtest | 3 / 3 | `"regtest"` | `[3]` | `[3, 0, 0, 0]` |

Both snapshots round-tripped JSON and standard bincode, rejected the tested serde aliases/case variants and invalid bincode variant 4, and returned identical C names. FFINetwork is size 4 / alignment 4 on this x86_64 Linux target. Generated C headers from the base snapshot, head snapshot and all-feature head test build were byte-for-byte identical. Existing enum definitions, representation attributes, derives and production FFI code are unchanged in the source diff.

## Feature-unification review

- Resolver 2 keeps the self-dev-dependency out of ordinary dependency builds. `cargo tree -p dash-network --no-default-features -e normal,build` has no normal dependency and no dash-params; its existing cbindgen build dependency remains. `cargo tree -p dashcore -e normal` likewise contains no dash-params. A standalone downstream snapshot build activates only its requested ffi/serde/bincode features, not params.
- When dash-network itself is tested (including `cargo test --workspace`), the self-dev-dependency intentionally unifies bincode/ffi/params/serde for that package, so the new tests run without CLI feature switches. This can hide feature-off coverage in tests; the independent feature-free library build below covers that configuration.
- The normal/build dependency closures of all current node-pure members (dash-sha256, dash-consensus, dash-x11) contain no dash-network. Dash-consensus already depends on dash-params directly; the other two reach neither crate. There is no new path from the self-dev-dependency into a pure crate, and the SDK dependency inherits `default-features = false`.
- The program's pure crates are ordinary std/sans-I/O crates, not no_std crates (A1 §2.1/X-1). dash-network itself already uses std and String at the base. BS16 does not remove an existing no_std guarantee; purity and R2 are checked independently below.

## Independent validation

All commands below completed successfully on the exact head, using the requested TMPDIR and job limit:

- `cargo test --locked -p dash-network`: **19 unit tests + 4 integration tests + 6 doctests passed; 0 failures**. No feature switches were required, confirming the self-dev-dependency activates the new tests.
- `cargo build --locked -p dash-network --no-default-features`: passed; its library fingerprint records `features: []`.
- `cargo clippy --locked -p dash-network --all-features --all-targets -- -D warnings`: passed.
- `cargo fmt --check -p dash-network`: passed.
- `cargo doc --locked -p dash-network --all-features --no-deps`: passed. Inspection of generated `dash_network/enum.Network.html` confirms both new public methods exist, but the module-level helper warning/devnet-name explanation is absent; no public `params/index.html` is generated.
- `contrib/node/check-pure.sh`: **exit 0**, `pure crates: 3 crate(s) ok`; includes bans and clippy for all three pure crates under all-features and no-default-features configurations.
- `contrib/node/check-r2.sh`: **exit 0**, R2 reachability and wrapper checks passed.
- `cargo deny check`: **exit 0**, `advisories ok, bans ok, licenses ok, sources ok`. Only nonfatal unmatched wrapper/license allowance warnings were emitted.
- Base and head standalone snapshot builds: **exit 0**; complete runtime snapshots and generated headers compared equal. Head fingerprint confirms this downstream build did not enable params (`bincode`, `default`, `ffi`, `serde` only).

Scratch worktrees, snapshot build directories and TMPDIR were removed after validation; this uncommitted review report is the retained artifact.

The implementer's reported full gate (3147 passed) and BL-20 rerun (54/0) were not independently rerun; this review uses the targeted runtime, ABI, feature and purity checks above.

mergeable: yes

Verdict: APPROVE-WITH-NITS

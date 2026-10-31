CHANGES-REQUIRED

Independent pre-merge review of BS6a, 2026-10-08. Base SDK: `9094d8111fee32929d9b4ee2e9f1686d34043cd2`, 14 commits over `481f8c3`. rust-dashcore: `242e0b7fb7f457e005af9fe85c7df9ae3f6e8b18`, four conformance commits over `30be7902`, tested with the supplied local pin to `9094d81`. I used fresh detached worktrees under `/work/scratch/bs6a-gpt/`; neither implementation worktree was edited. Core citations below are from `git show` of the exact `v23.1.8` and `v24.0.0-rc.3` tags in `~/workspace/dash`.

1. **Major — the required DEC-68 integration is missing, and banning direct blst batch calls alone would leave a threaded SDK path available.**

   **Files:** rust-dashcore `contrib/node/pure-deny.toml:46`, `contrib/node/pure-clippy.toml:31`, `contrib/node/check-pure.sh:43`; Base SDK `pkgs/pkc/src/bls/public_ops.rs:130`, `pkgs/pkc/src/bls/scheme_ietf.rs:127`.

   With the candidate pin, the actual pure gate fails:

   ```sh
   TMPDIR=/dev/shm/bs6a-gpt-tmp CARGO_BUILD_JOBS=2 cargo deny \
     --manifest-path dash-consensus/Cargo.toml \
     --config contrib/node/pure-deny.toml \
     --all-features --exclude-dev --locked check bans
   ```

   It reports `threadpool 1.8.1 → blst 0.3.17 → dash-pkc → dash-primitives → dash-consensus`. The reviewed tree still has the unconditional threadpool ban, no blst disallowed methods, and no blst selftest. This is a reproducible merge-gate failure, not a hypothetical payload rejection.

   There is a second requirement for the repair: include single verification and SDK wrappers in the call boundary. A concrete runtime input is `BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32])`, signing and verifying `b"BS6a independent thread probe"` once. `pk.verify(message, &sig)` increases `/proc/self/task` from **2 to 34** on this host. It calls blst `Signature::verify`, which calls threaded `aggregate_verify`, which initializes `ThreadPool::default()` even for this single signature. `default-features = false` does not stop this: blst's build script enables `std` on Linux.

   I also tested a consumer with `-F clippy::disallowed_methods`: a direct `blst::min_pk::Signature::verify` call fails the ban, while a call to the external Git dependency's `dash_pkc::bls::BlsPublicKey<BlsScIetf>::verify` passes it. `check-pure.sh` does not lint the bodies of external SDK dependencies. A direct blst-only list therefore does not enforce the promised rule through the existing SDK API. A `blst::MultiPoint::mult` trait ban does catch a direct method call on a PublicKey slice, so the trait paths should be included rather than attempting only slice-specific inherent paths.

   **Core citations:** the dependency becomes necessary because `CCbTx::bestCLSignature` is an eager object, not a byte bag: v23 `src/evo/cbtx.h:46,57`, `src/bls/bls.h:185–209`; v24 `src/evo/cbtx.h:47,62`, `src/bls/bls.h:184–208`. No Core wire mismatch is alleged by this finding; the violated requirement is DEC-68 / A1 X-1's call-level prohibition on threads in pure crates.

   **Fix:** land the exact DEC-68 wrapper exception `{ crate = "threadpool", wrappers = ["blst"] }`, the full call bans and selftests. Enforce the SDK boundary too: ban its threaded public wrappers in pure callers, or provide an audited serial verification interface and restrict pure callers to it. Test direct calls, trait calls and SDK wrapper calls. Keep parallel verification in non-pure workers. The exception alone is insufficient, and a list covering only methods with “multiple” or “aggregate” in their names is insufficient.

   **Exact thread-capable public blst 0.3.17 entry points under the current Linux configuration:**

   | Receiver/path | Methods | Implementation citations |
   |---|---|---|
   | `blst::min_pk::Signature`, `blst::min_sig::Signature` | `verify`, `aggregate_verify`, `fast_aggregate_verify`, `fast_aggregate_verify_pre_aggregated`, `verify_multiple_aggregate_signatures` | `src/lib.rs:1130,1201,1282,1303,1316`; pool at `:1216,1337` |
   | Both schemes' `AggregatePublicKey`, `AggregateSignature` | `aggregate_with_randomness` | `src/lib.rs:1002,1624`, delegates to slice validation/MSM |
   | `blst::blst_fp12` | `miller_loop_n` | `src/lib.rs:175–181` |
   | `blst::p1_affines`, `blst::p2_affines` | `from`, `mult`, `add` | `src/pippenger.rs:75,120,125`; pool at `:82` and delegated slice operations |
   | `blst::MultiPoint` implementations for `[blst_p1_affine]`, `[blst_p2_affine]`, and both schemes' `[PublicKey]` / `[Signature]` | `mult`, `add`, `validate` | `src/pippenger.rs:130,141,348,414`; `src/lib.rs:1721–1764` |

   These are methods that can create/use the pool, not a claim that every argument takes a parallel branch. Some small-input fallbacks still initialize the pool first. Single-point validation, ordinary `AggregatePublicKey::aggregate` / `AggregateSignature::aggregate`, and scalar C FFI operations do not themselves create the Rust pool. SDK paths requiring attention include `BlsPublicKey::verify`, `verify_with`, `verify_possession`, and IETF `BlsSignature::{fast_verify_aggregates,secure_verify_aggregates,verify_aggregates}`. Evidence: `pure-deny.log`, `thread-count.log`, `clippy-direct.log`, `clippy-sdk.log`, `clippy-multipoint.log` in reviewer scratch.

2. **Minor — context-free decoding silently selects basic for the two global-sourced payloads.**

   **Files:** Base SDK `pkgs/primitives/src/payload/cbtx.rs:79–84`, `pkgs/primitives/src/payload/assetunlock.rs:65–70`.

   The documentation warns callers, and transaction dispatch correctly requires `BlsCtx`; I found no current consensus caller taking the wrong path. However, an ordinary generic `BaseCodec::decode` remains callable and silently changes the decoded value. Concrete inputs are `inputs.json` rows `cb-v3-sub_legacy_s1` and `au-v1-sub_legacy_s1`. For both, context-free decoding succeeds with an **invalid** signature, while `decode_sptx(..., BlsCtx::LEGACY)` succeeds with a **valid** signature. Subsequent re-encoding substitutes 96 zeros on the default path.

   The exact 96-byte signature used by these inputs is:

   ```text
   8831663a7bd2ac358e4b816aa71c57b1eead59b83ab8002845e17376a9616b1394b77013020b92cb4e59b4bac3e2951c0d338ab3442f984659977f0f110a24ec0ff215a5c5619824c2953ccdf5dfbec2bd53e6bd41ce456b83f32f97ee7db402
   ```

   **Core citations:** v23 `src/evo/cbtx.h:57`, `src/evo/assetlocktx.h:99–108`, `src/bls/bls.h:207–209`; v24 `src/evo/cbtx.h:62`, `src/evo/assetlocktx.h:109–118`, `src/bls/bls.h:206–208`. Both use the global flag at the read; neither infers basic from the payload version.

   **Fix:** require context for consensus decoding at the type/API boundary, or make the context-free path explicitly fail for these types and expose a named basic-only convenience decoder. Add a lint/compile gate against accidental use from pure consensus code. Keep the explicit transaction dispatcher. `default-scheme.log` records the reproduction. This is API hardening, not evidence of a currently reachable chain split.

3. **Minor — payload encoding uses the captured decode scheme instead of Core's serialization-time scheme source.**

   **Files:** Base SDK `pkgs/primitives/src/payload/bls.rs:78–91`, `pkgs/primitives/src/payload/prouprevtx.rs:77–82`, `pkgs/primitives/src/payload/cbtx.rs:98–100`, `pkgs/primitives/src/payload/assetunlock.rs:78`.

   Unchanged decoded payloads re-encode correctly under the same context; the broader encoder contract has a trap. Decode `sig-uprev-v1-sub_legacy_s1` from `inputs.json`, then set its public `version` to 2 and encode. Core's version wrapper writes the same signature point in basic encoding; Rust still writes legacy encoding. The first differing payload byte is **68**: Rust's signature begins `8831663a7bd2`, Core's begins `ad338ab3442f`. Both pins reproduce this.

   There is no contextual encoder for the global fields either. Decode `cb-v3-sub_legacy_s1` or `au-v1-sub_legacy_s1` under legacy, then serialize after the call site's flag changes to basic. Core changes the signature encoding; Rust's `BaseCodec::encode` keeps legacy. The first differing bytes are **71** and **49**, respectively, at both pins. This matters when consumers construct/reuse payloads or supply a serialization context different from the read context. I found no live node consumer doing this in the reviewed changes, so this is not charged as an existing acceptance divergence.

   **Core citations:** v23 `src/evo/providertx.h:277–280`, `src/bls/bls.h:171–181,332–337`; v24 `src/evo/providertx.h:417–420`, `src/bls/bls.h:170–180,332–337`. The version wrapper recomputes `nVersion == LegacyBLS` on write. For global fields, the same default `Serialize` loads the global flag on write. Commitments and lazy version wrappers have the analogous contract; type 12 explicitly requests basic on write (`v24 src/evo/providertx.h:531–535`, absent in v23).

   **Fix:** supply an explicit `SptxEncode` context for global fields and select the version/AlwaysBasic source at the payload serializer for the other fields. Use `to_bytes_as` for lazy fields so Core's explicit-scheme-before-Get behaviour is preserved. Alternatively, constrain construction/mutation and name the current encoder as a serialization under the captured scheme, then ensure consensus hash/miner paths use a separate Core-compatible encoder. Add version-change and context-change tests alongside the existing round trips. Evidence: `encode-source-core-v23.txt`, `encode-source-core-v24.txt`, `encode-source-rust.json`, `encode-source-summary.json`.

4. **Nit — the CbTx asset-unlock root documentation should name instance hashes.**

   **File:** Base SDK `pkgs/primitives/src/payload/cbtx.rs:51`.

   “Merkle root of the block's stable-txid asset unlocks” leaves the hash choice unclear. With one v2 AssetUnlock, the correct leaf is its full `instance_hash`, including quorum signing information, not its stable `txid`. The decoder currently copies the correct 32-byte field; this is a documentation correction for future root computation.

   **Core citations:** v24 `src/evo/cbtx.h:49–52` and `src/evo/cbtx.cpp:158–171`, especially `GetInstanceHash()` at `:168`. v23 `src/evo/cbtx.h:49–60` has no asset-unlock root, and its cbtx.cpp has no corresponding function.

   **Fix:** say “Merkle root over instance hashes of the block's v2+ asset unlocks, in block order.”

**BLS decode audit.** Every payload BLS field was checked against both pins, including the scheme's independence from the surrounding global flag:

| Payload / field | Versions examined | Rust timing/source | Core v23.1.8 | Core v24.0.0-rc.3 | Result |
|---|---|---|---|---|---|
| Type 1 ProReg `pub_key_operator` | 1, 2, 3 | lazy; legacy iff v1 | `evo/providertx.h:99` | `evo/providertx.h:180` | correct |
| Type 2 ProUpServ `sig` | 1, 2, 3 | eager; legacy iff v1 | `evo/providertx.h:180` | `evo/providertx.h:301` | correct |
| Type 3 ProUpReg `pub_key_operator` | 1, 2, 3 | lazy; legacy iff v1 | `evo/providertx.h:221` | `evo/providertx.h:346` | correct |
| Type 4 ProUpRev `sig` | 1, 2, 3 | eager; legacy iff v1 | `evo/providertx.h:279` | `evo/providertx.h:419` | correct |
| Type 5 CbTx `best_cl_signature` | present at v≥3; also unknown versions | eager; global context | `evo/cbtx.h:55–58` | `evo/cbtx.h:60–65` | correct contextual decode |
| Type 6 inner commitment `quorum_public_key`, `quorum_sig`, `members_sig` | inner 0, 1, 2, 3, 4, 5, 65535 | all eager; legacy iff inner v1/v2; outer payload version irrelevant | `llmq/commitment.h:95–113,158–160` | same lines | correct |
| Type 7 MnHF `sig` | all decoded versions | eager; AlwaysBasic | `evo/mnhftx.h:43–46,81–83` | `evo/mnhftx.h:46–49,84–86` | correct |
| Type 9 AssetUnlock `quorum_sig` | 0, 1, 2, 255 | eager; global context, including v2 | `evo/assetlocktx.h:99–108` | `evo/assetlocktx.h:109–118` | correct contextual decode |
| Type 12 ProUpSharedReg `pub_key_operator` | 0, 1, 2, 65535 | lazy; AlwaysBasic, independent of version | absent | `evo/providertx.h:531–535` | correct |
| Types 0, 8, 10, 11; other ProTx fields | all applicable versions | no BLS fields; byte-vector/compact ECDSA signatures stay raw | applicable old types only | `evo/providertx.h:452–465,497–503`; `evo/assetlocktx.h:50–56` | correct |

Unknown versions of types 1–4 never reach a BLS read. `UnknownProTxVersion` consumes only the two version bytes. The ordinary structural decoders do not confuse semantic version acceptance with the read domain.

The eager wrapper exactly implements Core's rule: zero bytes give INVALID; canonical bytes under the selected scheme give VALID; canonical bytes only under the opposite scheme give INVALID; bytes canonical under neither scheme throw/fail the enclosing payload. This is a round-trip/canonicality check, not a generic subgroup-validity check. DEC-40/46's legacy acceptance of exotic points remains intact. Opposite-scheme decoding is a fallback only for the eager canonicality test; lazy `Get()` has no opposite-scheme fallback.

Core citations: v23 `src/bls/bls.h:100–140,185–220`; v24 `:100–138,184–220`. Invalid eager objects write zeros. Lazy fields accept a full raw key even when it is malformed, preserve those bytes before first use, and write zeros after failed `Get()`. That distinction is required by `CBLSLazyWrapper::Unserialize/Get/Serialize` at **both pins** `src/bls/bls.h:446–511`. The broad claim “invalid objects re-encode as zeros” needs that lazy-state qualification. Independent C++ before/after comparisons confirmed it at each payload use site.

**Layout and cursor audit.** ProReg v3 order matches v24 `src/evo/providertx.h:162–232`: version, type, mode, outpoint, ExtNetInfo, owner id, lazy key, voting id, reward; one-byte payout count and `(script,u16 reward)` entries; one-byte collateral-share count and `(i64 amount,refund script,reward script,owner id)` entries; exactly that many fixed 65-byte join signatures; u32 early-period blocks, i64 early penalty, inputsHash, Evo-only 20-byte node id, then CompactSize-prefixed collateral signature. V3 has no platform ports; older Evo payloads have the two u16 ports. ProUpReg v3's one-byte payout list replaces its single payout script (`v24 providertx.h:333–367`). V23 retains the single-script layouts (`providertx.h:81–115,208–230`); BS6c owns that parameterization.

Types 10–12 match the v24 serializers at `src/evo/providertx.h:452–465,497–503,531–545`, including raw u8 counts rather than CompactSize for their fixed 65-byte signature lists. Counts 252, 253 and 255 decode; contextual limits belong in EV1. Type 11's individual `vchSig` remains a CompactSize byte vector. Neither pin's unknown-version ProTx branch reads beyond the u16 (`v23 providertx.h:86–90,153–157,213–217,267–271`; v24 `:167–171,274–278,338–342,407–411`). A version-only payload can finish the read; a suffix remains trailing and must fail `GetTxPayload` (`specialtx.h:18–27`, both pins). `PayloadDecode::is_complete()` exposes that distinction correctly.

CbTx v4's minimum layout is 207 bytes: u16 version, **i32** height, two 32-byte roots, CompactSize height difference, 96-byte eager signature, i64 credit-pool balance, 32-byte asset-unlock root. `0x80000000`, `0xffffffff`, zero and `0x7fffffff` preserve their signed height meaning. Core uses the signed height at both pins (`v23 evo/cbtx.h:42,51`; v24 `:43,56`). The v≥ branches, including unknown versions, match each release's source. V23's missing final root is the documented SIG-05 profile difference, not an unexplained BS6a decode miss.

AssetLock v2 retains the u8-version / CompactSize credit-output vector layout; signed output values remain signed. AssetUnlock v1/v2 both have the same **145-byte** layout: u8 version, u64 index, u32 fee, u32 requested height, 32-byte quorum hash, 96-byte signature (`v23 assetlocktx.h:45–51,99–108`; v24 `:50–56,109–118`). Version acceptance is contextual; v2 stable-txid work belongs to BS6b.

NetInfo matches **v24** `NetInfoEntry::Unserialize`, including catch scope and cursor position. Service read failures are caught after whatever bytes Core consumed; unknown entry tags read one byte and re-encode `ff`; known networks with the wrong size stop before the address/port; unknown networks skip their claimed address and port then become invalid; invalid addresses/domains serialize `ff`. Domain reads use the 253-byte cap and propagate truncated/oversized-body failures. Duplicate purpose groups decode all their entries before the first map entry wins; unknown ExtNetInfo versions stop after their version. Citations: v24 `src/evo/netinfo.h:176–180,228–269,422–436`; v23 `:168–172,220–257,410–424`; corresponding address formatters in `src/netaddress.h` and validity/domain checks in `src/netaddress.cpp`, `src/evo/netinfo.cpp`. V23 catches domain failures and has no LIMITED_STRING serialization cap. The 46 direct-entry differences in my two-pin probe are exclusively this assigned BS6c rule.

Dropping `Eq`/`Hash` from ProReg, ProUpReg, ProUpSharedReg and their enclosing payload enum is sound. Their inherited lazy `PartialEq` can initialize/reset state and change subsequent serialization; equal raw buffers use a different fast path from decoded/default objects. An ordinary immutable hash-key contract cannot be promised. Core's equality branches are at both pins `src/bls/bls.h:514–534`. The CodeQL exception is scoped to lazy holders rather than removing the equality/hash requirement globally. Eager holders can still implement those traits.

**Independent validation and evidence limits.**

- `cargo test --offline -p dash-primitives --features full`: **286 passed**, including its Core payload, allocation and 74-row trivial corpus tests; no failures. `cargo clippy --offline -p dash-primitives --lib --features full -- -D warnings`: passed.
- `cargo test --offline --locked -p dash-conformance` at the candidate local pin: **74 passed**, including nine v24 fixture tests and the intentionally remaining BS6b stable-txid mismatch.
- Expanded the implementer's 84 RPC cases by **2,279**, to **2,363**. Executed **9,452** `decoderawtransaction` calls: both pins × both flags × all cases, on private sequential regtest daemons, private unused ports, `/dev/shm` datadirs, `-par=1 -dbcache=32`. All original 84 outcomes reproduced. **Zero v24 payload-decode verdict differences, zero compared v24 field differences, zero RPC errors.**
- New BLS cases isolate **each** commitment field, avoiding a malformed first field masking later ones; they include every represented eager/lazy corpus class, all relevant version branches and unknown versions. Other cases cover signed heights/amounts/penalties, u8 count extremes, v3 optional Evo fields, duplicate NetInfo groups, invalid/unknown address families, malformed CompactSize prefixes and domains.
- A separate C++ serializer probe linked against the two existing Core oracle builds; the relevant Core sources/headers were verified unchanged against their tags. For v24, **2,308 successful flag/case reads** matched Rust's consumed length, full re-encoding before lazy key use, and full re-encoding after lazy key use. No decoder-success mismatch was found among all 4,726 flag/case combinations. V23 differences all belong to the same 183 pin-dependent inputs: **60 BS6c layout/domain cases, 34 SIG-05 CbTx cases, 89 v24-only type cases**. There are no unassigned serialization differences.
- A separate direct `NetInfoEntry` probe used **991** inputs at each pin, including every possible tag, short reads at every service boundary, size limits and noncanonical size prefixes. V24 had **zero verdict, cursor or canonical-byte differences**. V23 had 46 differences, all assigned to BS6c's lenient domain rule.
- Scheme-change encoder probes and the context-free decoder probes reproduced findings 2/3 at both pins. The pure-deny gate failed as described in finding 1.
- I inspected, rather than reran, the implementer's full-testnet/chain campaign. Its final testnet summary reports **1,568,785 blocks, 8,574,218 transactions, zero mismatches and no incomplete/unverified units**. Full testnet has no types 10–12 in that summary; the five Core functional-test chains and 38 Core-built v24 transactions provide that coverage. These are useful fixtures, not substitutes for malformed-input probes or BS6b's planned differential campaign.
- The supplied instruction-count block benchmark supports about **−0.00082%** after subtracting corpus-loading counts. It is effectively flat; I did not rerun it on the busy host and do not claim an independent speedup measurement. Raw block decoding still keeps `vExtraPayload` opaque, so this result does not bound eager payload/BLS cost.

Reviewer artifacts are retained at `/work/scratch/bs6a-gpt/`: `comparison-summary.json`, `comparison-details.json`, `pin-difference-owners.json`, `core-payload-summary.json`, `core-payload.json`, `net-summary.json`, `net-v23-differences.json`, the generated inputs and probe sources, and command logs. The scratch Rust probes are untracked files in the detached SDK worktree. No branch edits, commits, pushes or GitHub posts were made. Private daemons were stopped and their datadirs removed; heavy runs used two Cargo jobs and uptime/load was checked.

The v24 decode shapes, per-field BLS timing/source, and unchanged-object canonical reserialization passed this review. Merge remains **CHANGES-REQUIRED** for DEC-68's missing pure-crate gate repair and effective SDK call-boundary enforcement. The decoder/encoder API traps and root documentation are separate nonblocking findings; BS6b/BS6c ownership does not change their stated limits.

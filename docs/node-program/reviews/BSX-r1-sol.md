# BSX r1 — Sol review

**Verdict: CHANGES-REQUIRED.**

Reviewed 2026-10-09. The direction accepted by DEC-98 is sound: strict SDK APIs, byte-bag BLS payload fields, one named wire classifier, node-owned compatibility policy, and small upstream units. This review does not reopen those decisions. The catalogue needs corrections before it can serve as the exhaustive removal checklist and dependency graph. The first batch below can proceed after the manager records these corrections; U11 and U19 need the specific fixes below before opening.

## Scope and evidence

- Catalogue: `rdc-wt/integration/docs/node-program/designs/BSX-leniency-catalogue.md`, unchanged from `7dc2e63748566170ef90f7f3bc45fb93cf0c5996`; read all sections, including §7 and §9, and DEC-86/91/98.
- Independently fetched `https://github.com/dashpay/base-sdk.git develop`. `FETCH_HEAD` was `33fcc59e1868c803f9608e0e78ee8bddf3abcee7`, matching the catalogue's baseline. Compared this tree with `node/base-sdk` (`a1d4dd3bbed0f78f89c04d92d160f6cbd0863275`) and separately compared the BS7 increment through `b69fa558910cdde1cf17297b937f6c6f1039a2c8`. The first comparison changes 105 files, +28,504/−1,371 lines, much of it corpus data.
- Core evidence below is from **`git show v24.0.0-rc.3:<path>`**, commit `da567d7dba945143eee4d2ce1621c21b32089989`, not the newer working-tree HEAD. Core paths are relative to `src/` unless prefixed `test/`. SDK paths are relative to the repository root; fork line numbers refer to the stated fork head.
- Read upstream `AGENTS.md`, `docs/dev/guide_rust.md`, `.github/PULL_REQUEST_TEMPLATE.md`, `Justfile`, commit vocabulary/linter, CI commit cap, and CodeQL policy/query implementation. `CLAUDE.md` is a tracked symlink to `AGENTS.md`; no CONTRIBUTING file exists in the fetched tree.
- Ran the requested `gh pr list --repo dashpay/base-sdk --state all --limit 40`; read recent kwvg PR bodies/commit lists, notably #44, #54 and #56, and issue #40. These are convention/background reads only; no PR was opened or worked on, and no GitHub comment or other outward mutation was made.
- This is a source/design review. I did not run rebuilt PR gates, a Core runtime oracle, or a historical-chain scan. No proposed rebuilt unit exists here to certify with `just test lint`. Claims below distinguish source proof from executed tests. No private material was read; `/work/chains` and `/work/bench` were not accessed. Repository source and branches were left unchanged; the requested review file is the sole deliverable and is uncommitted.

## Findings by severity

### Medium M1 — U11 / B-18 / B-21: U11 is blocked by the policy unit scheduled after it

**Evidence:** catalogue §7.3, lines 962 and 982, schedules U11 in tier 1 and U25 in tier 3 **after U11**. At upstream `33fcc59`, `maint/codeql/rust/lib/policy.qll:48–76` classifies names containing `Share` but not `Shared` as secret, with no exceptions for `PayoutShare`, `CollateralShare` or `ProUpShareTx`. U11 introduces exactly those names. Fork commit `b497f65` adds their exclusions at `policy.qll:71–73`. Upstream `lib/secret.qll:281–299` reports missing wiping and derived Debug for such types, and `secret.ql` emits these findings. The new public types derive Debug and do not implement secret wiping. Moving BLS wrappers out does not resolve this independent problem.

**Impact:** the proposed standalone U11 cannot satisfy the required CodeQL gate with the advertised unchanged upstream policy. The plan contains a dependency inversion, not merely an optional cleanup.

**Required correction:** coordinate the public-share classification with kwvg and land the small U25 policy correction before U11, or agree on public type names that avoid the false classification. Record this dependency explicitly. Do not suppress the findings in U11 or treat a missing CodeQL executable as a pass. The source predicates establish the blocker; I have not executed CodeQL on a rebuilt U11 branch.

### Medium M2 — B-18 / U11: the advertised leniency-free reshape leaves two bends unresolved

**Evidence:** B-18 at catalogue lines 439–447 notes byte-count wrapping and raw signed collateral amounts, but assigns the entire item S/(a). U11 explicitly fixes only bend 2, the join-signature mismatch. Fork `pkgs/primitives/src/payload/shares.rs:99–105` casts `items.len()` to `u8` and then writes **every** item. A 256-element list therefore emits count zero followed by 256 bodies; decoding does not recover the encoded value. This affects payout lists in ProRegTx and ProUpRegTx as well as collateral shares. `shares.rs:49–58` also exposes any `i64` as a collateral amount, and `proregtx.rs` exposes an unchecked signed early penalty. These are representational Core tolerances, not required wire definitions for valid transactions.

Core likewise narrows list sizes (`evo/providertx.h:184–207`), but valid payloads are bounded: payout count 1–8 (`providertx.cpp:25–29`), shared collateral count 2–8 (`:63–67`), positive bounded collateral amounts (`:77–80`), and nonnegative bounded penalties (`:120–126`). No accepted transaction requires a >255-element encoder or a negative collateral amount/penalty. Fork ProRegTx `check()` does not currently enforce the wire count bounds.

**Impact:** copying these parts still exports invalid-state construction and a non-round-tripping encoder in a tier-1 unit described as leniency-free. This also makes the stated catalogue counts incomplete at the sub-behavior level.

**Required correction:** split B-18 into protocol layout, count/encoder tolerance, and amount representation subitems; classify the latter two explicitly. Add count bounds to the accepted `Checkable` approach or use validated bounded collections; preserve the DEC-98 choice for join-signature checking. Reshape collateral amount/penalty into appropriate typed values, or justify a separately named raw wire representation with validation. Tests should include 255/256-element framing and negative/out-of-range amounts, not just the join-signature mismatch. No new owner/operator-signature or consensus decision is needed to fix these wire invariants.

### Medium M3 — B-NEW-01 / B-17 / U11: `UnknownProTxVersion` is missing from the catalogue

**Evidence:** the independent diff adds `SpecialPayload::UnknownProTxVersion` in `4e6bb33`; at `a1d4dd3`, `pkgs/primitives/src/payload/mod.rs:348–355` defines it, `:465–466` checks it, and `:501–512` returns success after consuming only the two-byte version for provider types 1–4. Individual provider decoders separately reject unknown versions through `check_version`. Searching the entire catalogue yields no occurrence of `UnknownProTxVersion`; B-17 covers new transaction types 10–12, not this behavior. The background API-shape brief explicitly calls this kind of API out for review.

Core's short read is real (`evo/providertx.h:167–170`, and equivalent provider serializers), but unknown versions still fail validation. `GetTxPayload` additionally requires the payload cursor to be empty (`evo/specialtx.h:18–26`). Thus a two-byte unknown-version payload and an unknown version with a tail can produce different rejection stages/reasons. The transaction retains raw extra-payload bytes, so this behavior does not rescue a consensus-valid transaction or change its txid.

**Required correction:** add an explicit L/E, (c) entry for the Core short-read/reporting behavior, plus an API-shape disposition. Keep the strict SDK failure unless kwvg wants an explicitly separate inspection API; put Core reason distinctions in the node's version/reason walker. Map both `4e6bb33` and the shared-helper changes to this entry and exclude it deliberately when rebuilding U11. Recalculate the counts rather than treating 87 as proof of exhaustiveness.

### Medium M4 — D-07 / U19 / b-12: version-zero headers are not unconditionally invalid

**Evidence:** catalogue D-07 (line 535) and b-12 (line 692) say the headers produced by the lenient uncompress are always invalid. Core `primitives/block.cpp:107–114` really leaves `nVersion == 0` when a version offset exceeds the cache. However, the cited version check in `validation.cpp:4513–4518` is conditional on HEIGHTINCB, DERSIG and CLTV activation. `CheckBlockHeader` at `:4307–4318` does not independently reject version zero.

**Source counterexample:** on regtest with those three buried activations postponed using accepted `-testactivationheight` overrides, start a two-header `headers2` message with a valid version-1 header whose parent is known. The cache has one version. Give the second header version offset 2 and otherwise valid fields/PoW extending the first. Core reconstructs version zero, the sequence is continuous, and the three version gates are inactive. A strict out-of-cache-offset error rejects this message before the otherwise acceptable header reaches validation. Core's uncompress and validation source establish this counterexample; I did not execute a regtest oracle for it.

The first-header statement also needs precision: Core returns without reconstructing omitted fields, retaining **all literal fields**, not only the version (`block.cpp:97–100`; SDK `headers2.rs:178–181`). Missing nBits and missing timestamp have different validation consequences from a missing version or previous hash.

**Required correction:** retain the DEC-98 strict upstream decoder, but remove the unconditional-invalid/no-verdict-effect rationale. Split the cases and make the node's compatibility uncompress cover the accepted version-zero case, as well as required penalty mapping. Reuse one wire-field parser; make strict versus Core reconstruction a policy over that parsed representation. This is a P2P acceptance difference on supported configurations, not evidence of a split between existing Core nodes or a demand for lenient upstream defaults.

### Medium M5 — B-05 / B-03 / §5.2: one classifier does not imply one failure policy for every BLS field

**Evidence:** §5.2 step 3 maps canonical-in-neither bytes to `bad-*-payload` for the proposed eager holder per field. Core eager deserialization does throw (`bls/bls.h:184–203`), but lazy operator-key deserialization does not (`:467–475`). Lazy `Get()` returns an invalid key (`:492–511`), and provider validation then returns `bad-protx-key-null` (`evo/providertx.cpp:300–301`, `:468–469`). B-05 itself correctly describes this reason distinction. Turning malformed lazy keys into a decode-stage return also risks bypassing earlier checks in Core's order.

**Impact:** verdicts remain reject, but the generic recipe loses reason/stage parity that DEC-98 Q-1 now requires node-side. This is an implementation detail of the accepted eager-holder decision, not a reason to retain laziness upstream.

**Required correction:** specify one pure classifier with per-field node policies. Eager fields can produce a payload-decode error; formerly lazy keys keep a classified invalid value until the corresponding validation check emits Core's key-null reason in the right order. Keep wire bytes and scheme in the holder for valid-state serialization/hash/migration. Test eager versus formerly lazy handling on the same malformed bytes and on multiply-invalid payloads.

### Medium M6 — U1–U29 / §8: the scrub rule conflicts with the accepted attribution trailer

**Evidence:** catalogue §8 line 1049 forbids every `Opus`/`GPT` hit in the full commit history, then line 1052 says trailers follow Q-3. DEC-98 requires retaining `Co-Authored-By` attribution. The catalogue's own §6 describes the existing Claude Opus trailer.

**Required correction:** give approved attribution trailers a narrow, explicit exception in the scrub checker while continuing to reject internal program references in code, fixtures, prose and unrelated commit-message text. Record Q-3 as decided. Do not resolve the contradiction by deleting the required trailer or ignoring all model-name matches.

### Medium M7 — C-11 / U10 / b-8: `u32` and factor ≥1 alone do not eliminate subsidy overflow

**Evidence:** U10's proposed strict reshape and Q-20 specify nonnegative integers/factor ≥1, but no upper multiplier bound or arithmetic contract. Core computes up to 500 coins of base subsidy (`validation.cpp:1831–1851`) and multiplies its signed amount by the factor (`:1858–1860`). A factor of 1,000,000,000 is positive, fits both `i32` and `u32`, and gives `500 * 100,000,000 * 1,000,000,000 = 50,000,000,000,000,000,000`, above `i64::MAX`. The wire/config newtype alone does not cure the bug already identified in b-8.

**Required correction:** make U10's valid configuration domain explicit: either reject factors exceeding a documented safe bound or require checked/wider subsidy arithmetic with a defined typed failure. DEC-98 already authorizes stricter devnet configuration; this just makes that decision executable. Carry a large positive factor case in the validation evidence.

### Low L1 — A-13 / A-15 / A-17 / A-24: preserve the distinction between consensus and interoperability

All four are necessary interoperability fixes/policies, but none by itself changes block/transaction consensus acceptance or hashing. Their evidence is P, correctly shown in the detailed entries, whereas §1 defines (a) exclusively as consensus-required and §3 includes them in the 13 (a) leniencies. In particular, the honest `govsync` problem is an incomplete message definition, not proof that *all* trailing bytes are consensus-required. Keep their accepted homes and priority; explain that this use of (a) includes required protocol interoperability, or separate the two effect counts. U5 and U6 can still be leniency-free after strict reshaping.

### Low L2 — C-22 / U9: `dev%refac` is not an accepted namespace

Catalogue lines 501 and 960 propose a `dev%refac` commit for bspcheck. Upstream `maint/unconv.toml` lists num, p2p_core, params, pkc, pow, primitives, script, sdk and types, but no dev; `lint_commit.py` rejects unknown namespaces. Use an allowed namespace, for example `sdk%refac(dev): ...`. This does not block the API design, but would block the actual commit gate.

### Low L3 — A-01 / A-02 / D-07 / U22 / U24 / document status: materialize DEC-98 in the executable plan

The status still says draft/uncommitted (line 6), and several node replacements and upstream units remain conditional on already-answered Qs. Update the text to name required node reason/penalty replacements, mark the rejected SDK-only reason-parity route U22 inactive, retain only actual kwvg coordination gates, and state that U24's home/tag questions are resolved. For A-01, replacement acceptance criteria must also include block-in-flight cleanup and penalty dispatch, already identified in its detailed entry; producing a reason string alone is not equivalent node behavior. Preserve the required replacement-before-removal order in §7.5.

## Classification sample against Core

The following checks cover all 13 listed (a) leniencies, the B-05 split, all three class-(b) items, every b-1…b-12 flag, and further (c), strictness and API items across all four areas. Supporting rows tagged (a) outside the leniency count are included as well. “Keep” below means preserve the specified semantics somewhere, not necessarily in upstream's default codec.

| Item | Core evidence at rc.3 | Review result |
|---|---|---|
| A-07 / b-1 | `netinfo.h:401,428`; `serialize.h:1232–1242`; `providertx.cpp:391–392` | Genuine C/(a): first-wins map plus sorted reserialization affects validation, payload sign hash and stored state. Raw txid/input sighash must remain distinct from the effective view. Bug flag's noncanonical-map premise confirmed. |
| A-13 | `net_processing.cpp:3950–4002` | Required P: optional suffix groups are read only if data remains; sender address and nonce are one group. Nested prefix representation fits. No direct C effect. |
| A-15 | `net_processing.cpp:4224–4226,4485–4504` | Required P: these carry a bool, not an empty body. Strict bool types fix honest traffic. No direct C effect. |
| A-17 | `governance/net_governance.cpp:59,93–101`; `common/bloom.h` | Required P part confirmed: whole CBloomFilter must be represented. Node prefix tolerance is feasible, but general ignored tails are not a consensus requirement. |
| A-24 | `net_processing.cpp:1210–1215,4818–4824` | Required P: the compressed-header limit can be 8000. Payload-bounded allocation plus node version gate is the right split. |
| B-02 | `bls/bls.h:100–120,211–214`; `dashbls/src/elements.cpp:57–85`; `providertx.cpp:300–304` | Genuine C/(a): wrapper validity follows SetBytes/round-trip, and legacy read lacks strict point/subgroup validity enforcement. Keep exotic wire classification. Historical occurrence remains unverified; absence would not prove the acceptance rule removable. |
| B-03 / b-4 | `bls/bls.h:184–203`; `llmq/commitment.h:118–130` | Genuine C/(a): opposite-scheme-only bytes reset to invalid/null, including null commitments. Neither-scheme throw is strictness relative to upstream bags. Second-null-encoding premise confirmed. |
| B-05 | `bls/bls.h:446–457,467–475,492–511,542–556`; `evo/deterministicmns.h:459–475` | Split justified: laziness is removable; scheme-aware serialization/hash/migration must remain. Formerly lazy rejection reasons need the M5 policy distinction. |
| B-06 / b-4 | `bls/bls.h:206–209`; `evo/cbtx.cpp:205–208`; `specialtxman.cpp:169–181` | Genuine C/(a) for null CL interpretation. AssetUnlock invalid signatures do not become accepted. Accepted basic-scheme/config policy stands. |
| B-10 | `netinfo.h:243–263`; `netinfo.cpp:596–615`; map read above | Genuine C/(a) when the invalid entry is in a dropped duplicate-purpose group. Strict unknown-tag rejection cannot replace the effective Core parse. |
| B-11 / b-2 | `netinfo.h:247–252`; `netaddress.h` service read | Genuine C/(a): swallowed service failure resumes within the payload; discarded groups can contain the result. Cursor must be modeled at this field boundary. Bug flag premise confirmed. |
| B-14 / b-3 | `netinfo.h:212–216,326–347,355`; `providertx.cpp:318–324,391–392` | Genuine C/(a): an invalid legacy address becomes empty, which ProRegTx permits. Effective zero service affects sign hash/SML state. Raw service plus effective view is feasible. |
| C-12(ii) | `chainparams.cpp:1225–1244`; `test/functional/feature_llmq_singlenode.py:37` | Required regtest configuration compatibility: the ISDIP0024 role accepts a nonrotated test quorum. Keep this independently of size/threshold validation. Not a mainnet wire leniency. |
| C-15 | `validation.cpp:4476–4485,6670,6705` | Genuine C: early-mainnet difficulty tolerance and testnet quorum activation literal affect accepted history. Node constants are sufficient; no decoder duplication. |
| C-07 / b-5 | `util/system.cpp:432–435,529–535,1051–1054` | Negation is a set false value converted to string `0`, yielding devnet-0. Minor local-config bug premise confirmed; accepted refusal stands. |
| C-09 / b-6 | `util/system.cpp:543–560`; `chainparams.cpp:1251–1255` | Narrowing GetIntArg to int is real; the specific wrap is implementation-defined C++ behavior of the supported build, not a portable law. Parsing/occurrence model belongs in dash-args. |
| C-10 / b-7 | `chainparams.cpp:1051,1108`; `versionbits.cpp:121–125` | Negative min-activation heights are admitted in one route but refused in another. Configuration-only inconsistency; accepted unsigned setters/refusal stand. |
| C-11 / b-8 | `chainparams.cpp:1251–1255`; `validation.cpp:1858–1860` | Signed/unchecked multiplier and overflow premise confirmed. Stricter upstream domain allowed; positive u32 alone is insufficient (M7). |
| C-12(i) / b-9 | `chainparams.cpp:1019–1027,1211–1222` | Only upper size is checked on the regtest route; positive size/threshold and threshold ≤ size are not. Validated typed overrides are appropriate. |
| C-13 / b-10 | `chainparams.cpp:1352–1368`; `pow.cpp:140,153`; `rpc/mining.cpp:84–86` | Positive spacing can produce zero interval; reachable RPC modulus-by-zero premise confirmed. `Option`/checked handling is justified. Catalogue's `pow.cpp:162` is not another modulus site. |
| C-14 / b-11 | `init.cpp:1746–1751`; `util/system.cpp:529–535` | Negated spork address becomes `0` and fails address resolution. Last-value/warning behavior confirmed; dash-args owns this policy. |
| D-07 / b-12 | `primitives/block.cpp:95–133`; `validation.cpp:4307–4318,4513–4518` | Core's reconstruction quirk confirmed; unconditional-invalid conclusion refuted (M4). Strict upstream policy stands; node reconstruction must account for the configuration-dependent accepted case. |
| A-01 | `consensus/tx_check.cpp:37–46`; `validation.cpp:4867–4893` | No valid tx needs negative/>MAX_MONEY outputs. (c) in the SDK is sound; node reason/penalty/in-flight replacement is required by DEC-98. Preserve raw-byte framing on failure. |
| A-02 | `serialize.h:292–320`; `consensus/tx_check.cpp:34–35` | No valid tx needs >10,000 payload bytes. Keeping the SDK bound does not reject valid txs; node walker handles decode-vs-validation reasons. |
| A-08 | `evo/specialtx.h:18–26` | Whole-payload consumption is mandatory. Strict SDK API can replace the report; CS0 needs adaptation. |
| A-12 / D-11 | `serialize.h:238–239`; `net_processing.cpp:4274–4276` | Core nonzero bool tolerance is real; honest emissions are 0/1. Strict SDK bool is appropriate. Crafted-input peer-state differences are policy deviations, not literally “no node behavior.” |
| A-16 | `net_processing.cpp:5505–5527` | Short pong affects ping bookkeeping, not tx/block validity. SDK can require nonce; node length check preserves desired handler policy. |
| A-18 / A-19 | `net_processing.cpp` empty/unknown handler paths | Default strict errors versus ignored/raw unknown payloads do not affect chain hashing. Their node mapping and optional Unknown API disposition are coherent. |
| A-22 / A-23 / D-04 | `net.cpp:1293–1306,1496–1533` | Framing is separate from message dispatch; extra short-form capacity and reserved slots are protocol tolerances, not consensus. Accepted strict reserved-ID handling can preserve “drop, keep peer.” |
| A-26 | `net_processing.cpp:5336–5341` | Core ignores a canonical header tx_count. Dropping tolerance does change receipt of crafted otherwise valid headers; describe it as an intentional strict P2P policy, not a proof that the cursor/value is unobserved. No txid/merkle/sighash change. |
| B-07 | `governance/object.cpp:569–570`; `governance/vote.cpp:201–202` | Direct basic SetBytes without eager round-trip is real. Deferral stands; future governance work must revisit it instead of treating unused today as permanently unnecessary. |
| B-15 | `netinfo.h:424–426`; `netinfo.cpp:590–591` | Unknown ExtNetInfo versions cannot pass effective validation. Strict early failure is safe for accepted txs; node reason mapping may differ. |
| C-01 / C-08 / C-18 | `chainparams.cpp:1422–1445`; `util/system.cpp` | Release axis removal and typed params/args separation fit the single-pin decision; argument occurrence and exact Display text are node concerns. |
| D-09 / D-13 / D-17 / D-18 | `serialize.h:292–320,795–851,1047–1053`; bounded P2P framing below | Failed-read cursor differences do not change current successful decode results or subsequent P2P boundaries. Drop these global read changes under DEC-98; retain field-specific NetInfo failure semantics. |
| D-14 | `net_processing.cpp:3489,3520,3563`; command dispatch | A server's lack of receive handlers is not an SDK type rule. Type both directions; put ignoring in the full-node handler. |

Additional (a)-supporting/S checks: **B-01/B-08** scheme selection is version/global/always-basic protocol data (`providertx.h:180,301,346,419,533`; `commitment.h:110–113`). **B-04/B-09** error and serializer rules follow the wrapper distinctions above, with BLS domain errors outside dash-types. **B-13/B-16** match `netinfo.h:178,355,442` and are independently reshapeable strict fixes. **B-17** types 10–12 match `primitives/transaction.h:39–41`. **B-18** layouts match `providertx.h:184–208`, subject to M2. **B-19** v4 CbTx matches `cbtx.h:36–65`; replacing signed height by BlockHeight does not exclude an accepted height. **B-20** v2 lock/unlock and script changes match `assetlocktx.cpp:56–80,207–212`, with V24 gates remaining node-side. **D-01/D-02** wire limits/V1 framing match `net.h:84` and `net.cpp:731–741,776–812`; curate handler-only constants as §5 already requires. **D-03** short-ID version availability matches `net.cpp:993–1005`. **D-06** inventory names match `protocol.h:495–528`. **D-12** layout and instance-hash short IDs match `blockencodings.h:116–123` and `blockencodings.cpp:27–50`; BS6b's hash work remains a real dependency. **D-15** control/block/tx wire typing is an interop addition, not a consensus leniency.

## BS7 cursor and P2P boundaries

**The requested boundary concern is resolved in favor of dropping D-17/D-18.** Core's V1 transport takes exactly the header-declared body length (`net.cpp:749–764`), moves that body into a separate `CNetMessage` (`:783`), and resets its transport for the next frame (`:810–812`). V2 likewise decrypts a complete length-framed packet, copies only that packet's payload into `msg.m_recv`, clears its decode buffer and returns to APP (`:1536–1563`). `ProcessMessages` calls `ProcessMessage` on this bounded message and merely logs the outer exception (`net_processing.cpp:5925–5943`). It does not advance the network receive stream by the payload decoder's consumed count.

Consequently an impossible vector count can stop earlier or later **inside that bounded body**, but cannot steal bytes from the next message. The node must preserve this architecture: isolate the frame before passing a local slice to `BaseCodec`; discard the full frame on a payload error. Do not use a failed decoder's cursor to locate the next V1 header or V2 packet. A malformed-count message followed by a valid ping is a useful integration check when this reaches the handler implementation.

The relevant in-payload exception is B-11. That try block reads a service's fixed-size/address-length fields, not a general vector/string body whose global behavior BS7 changed. `CQuorumDataRequest`'s catch reads the fixed `nError` field (`llmq/quorums.h:117–128`). No current caught/resumed path found needs the changed generic vector reader. Current vector elements consume at least one byte, including an invalid NetInfo entry's tag; a count above remaining bytes cannot turn into a successful decode. Chunking only differs beyond 5,000,000 bytes and cannot affect the supported block/P2P domain here. This does not promise identical failed cursor behavior to arbitrary external BaseCodec callers; the justification is scoped to the audited node consumers.

## Completeness and node-side homes

The independent diff supports the catalogue's broad inventory of BS4a, params, BLS wrappers, NetInfo normalization and BS7 policy additions. **It does not support the claim of exhaustive classified coverage:** B-NEW-01 is absent, and B-18's several distinct tolerances are only noted as bends without classification/disposition. Add these before using the counts as a removal gate. I found no further independently identified Core-acceptance expansion outside the listed areas; that is a source-review conclusion, not a fuzzing or historical-chain result.

The proposed homes are feasible without a second set of full transaction/provider decoders if these boundaries are explicit:

1. **BLS:** SDK payloads decode fixed byte bags once. One pkc classifier implements wire arithmetic/encoding rules. The node applies eager/formerly-lazy/basic field policies, retains bytes plus scheme, and owns effective serialization/hash/migration. Never compute txids by serializing the effective payload; the raw extra payload remains authoritative. This shares the structural decoder while keeping compatibility semantics out of strict SDK types.
2. **NetInfo:** one field parser must record exact consumed spans and produce raw and effective views, including caught service failures and first-wins purpose groups. The four provider decoders invoke that field parser rather than being reimplemented in dash-evo. Effective views feed Validate, signature hashes and SML/state; raw spans feed byte-faithful operations. Unknown entries cannot be represented solely by a tag plus a guessed body length after a swallowed service failure. If upstream declines, the documented small fork patch is safer than parallel full provider decoders.
3. **A-13/A-15/A-24:** strict wire types upstream are the right home; these are message-definition/interop fixes. **A-17:** node prefix dispatch over the same SDK message codecs; complete GovSync separately. Expose a neutral prefix dispatch helper if necessary to avoid maintaining two command tables, without importing ProtoCtx into the SDK.
4. **C-12(ii)/C-15:** validated config setters and node consensus constants need no extra decoder. Rejecting invalid override ranges remains compatible with the accepted configuration deviations.
5. **A-01/A-02 and D-07:** node reason/reconstruction adapters are secondary compatibility mechanisms. Reuse the structural byte walker / CompressedHeader wire parser; do not create an alternative accepted-block validation engine. Adapters need bounded input, check-order/penalty mapping, and affected peer-state actions. Their tests should compare strict and Core-compatible paths on canonical valid data as well as failures.

## PR plan and first batch

The conventional vocabulary, PR template, labels, released-crate changelog rule and ≤100-commit cap are substantially accurate. Upstream's actual gate is `just test lint`; `just comb` applies it to each commit. Proposed units are far below the cap. Read-back of the PR body and full-history scrub remain manager obligations under DEC-91. The observed kwvg PR style supports small focused feature/fix units and tag-based embedded generators; no title needs to be shortened to commit width, but individual commit messages do.

Tier 1 is a **dependency-ordered series**, not a set of 17 individually develop-based branches. U4 needs U1, U5/U6 need U3, the params series needs U2/U7 onward, and the BS7 framing units build on U15. Keep these dependencies visible in branches/PR bases or wait for merges. The meaningful hidden tier-3 blocker is **U25 → U11** (M1). U11 also needs M2's explicit strict reshape. U19 can remain strict upstream after M4's node consequence is recorded. The decisions formerly blocking U7/U9/U10/U15–U18 and U24 are resolved by DEC-98; kwvg coordination for U20/U21 and U26–U29 remains. U23 is a leniency-free message-definition bug fix once implemented, with a work-readiness dependency rather than an unresolved leniency ruling.

**Recommended first batch, in this order; keep at most one or two open at a time:**

| PR unit | Scope | Label | Opening condition |
|---|---|---|---|
| **U1** | `types%fix: range-check length prefixes and vector reservation` | Codec | Develop-based; include CompactSize boundaries/allocation evidence and types changelog. |
| **U2** | `primitives%feat!: name LLMQ types and reuse them in params` | Primitives | Develop-based; narrow enum/API diff; required private scrub before any push. |
| **U3** | `p2p_core%fix!: signed versions and strict framing errors/limits` | P2P | Develop-based; use strict BaseCodec dispatch plus end-of-payload checks so no ProtoCtx/BLS dependency is imported. |
| **U4** | `primitives%fix: transaction-size check and bounded length reads` | Primitives | After U1; rebuild tests below the retained payload cap; preserve Amount; private scrub. |
| **U5** | `p2p_core%fix!: typed control-message payloads` | P2P | After U3; strict bools, no ignored tails/empty-command tolerance or unrelated BIP coordination work. |

U7 is a good next independent batch once its rc.3 fixture is regenerated. U11 should not be used as an early “easy v24 types” PR until M1–M3 are resolved.

## Embargo hygiene

The catalogue's only EMBARGO-bearing lines are **553 and 554**, and they are codename-only handling stubs. I found no suspicious technical disclosure in the catalogue text to report. This is not a private-classifier receipt for any future PR: every branch history, vector, PR body and comment still needs the prescribed pre-push scrub. No private content or private-derived reproducer is included in this review.

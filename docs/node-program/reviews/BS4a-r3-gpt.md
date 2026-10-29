# BS4a independent review, round 3 (GPT)

**APPROVE.** All prior in-scope findings are closed by their permitted resolutions, and I found no new decoder or allocation regression. This round found a major normative DKG read-shape error and a minor overstatement of NetInfo re-serialization parity; the owner corrected both while this review was running, and I independently checked the corrections. With those edits included, DDE-SWEEP §4's normative pre-read policy is settled under DEC-45.

Reviewed under `/work/home/agent/workspace/node-program/briefs/REVIEW-COMMON.md`, A1 §1.4/§2.14 and DEC-45, against all three prior reviews (`BS4a-r1-gpt.md`, `BS4a-r1-opus.md`, `BS4a-r2-opus.md`).

| Repository | Reviewed snapshot |
|---|---|
| base-sdk | `node/bsdk-bs4a-decode-domain`, final HEAD `3c971b1a93bc5c61fd48f3abbda287109b4acaa3`, above `098dafd` (BS11a-t included); executable source tested at `0c71f50` is unchanged |
| rust-dashcore | `node/bs4a-decode-domain`, final HEAD `2713e6b024b82c8ac60a14973de01d5c489131c7`, above `f29edb27`, including the DDE-SWEEP, checker and probe README corrections |
| Core | Source at `v23.1.8` and `v24.0.0-rc.3`, and the corresponding release binaries on private regtest nodes |

Both owners committed the reviewed corrections before final handoff. G3-1 is closed by rust-dashcore `2575f215` (DKG row and checker), and G3-2 by base-sdk `3c971b1` plus rust-dashcore `d808d969`; `2713e6b0` updates the sweep's head reference. Final correction files are frozen in `/work/scratch/bs4a-r3-gpt/final-fixes/`, with SHA-256 hashes in `manifest.json` and refs in `heads.json`. Both reviewed worktrees are clean. Base-sdk's executable source is unchanged from the tested `0c71f50` snapshot.

Scratch and reproduction sources are in `/work/scratch/bs4a-r3-gpt/`. Tests used `TMPDIR=/dev/shm/bs4a-gpt3-tmp` with mode 0700. The private nodes used P2P/RPC ports 30311/30312 and 30321/30322; they were stopped and their temporary datadirs deleted. I did not edit either branch under review; no commits, pushes or GitHub messages were made.

## Findings opened and closed during this round

### G3-1 — Major, closed: the normative `qpcommit` read shape counted its member signature twice

**Location:** [DDE-SWEEP.md:181](/work/home/agent/workspace/rdc-wt/bs4a-decode-domain/docs/node-program/DDE-SWEEP.md:181), the **Handler reads** cell.

The original row described a premature commitment as `DYNBITSET, public key, hash, two signatures`, followed by the common `then a signature`. That specified three signatures for `qpcommit`. Both pins serialize exactly:

```
u8 llmqType, uint256 quorumHash, uint256 proTxHash,
DYNBITSET(validMembers), quorumPublicKey, quorumVvecHash,
quorumSig, sig
```

Core source is `src/llmq/dkgmessages.h:167-178` at **both** tags. The last two fields are the two signatures in total; there is no third common signature after them. The v24 framing walk independently confirms this in `net_dkg.cpp:150-159`: after the bitset it skips `48 + 32 + 2 * 96` bytes and requires an empty stream.

**Concrete failure:** for a three-member quorum, a correctly framed premature commitment is `65 + 1 + 1 + 48 + 32 + 192 = 339` bytes. An implementation following the normative cell asks for 435 bytes and hits EOF on the fictitious third signature, rejecting/scoring a message Core can decode. Conversely, adding that extra signature leaves 96 bytes and fails v24's whole-payload check. I compiled Core's actual v24 `ReadBoundedDynBitset` and `CheckPrematureCommitmentWireStructure` bodies with its serialization headers: 339 bytes passes with nothing left; 435 bytes fails with 96 bytes left. This is a framing test, not a claim that zero-filled signatures pass later verification.

**Suggested fix, now implemented:** write the four message bodies separately and guard the complete field lists at both tags. The revised row does this, explicitly lists exactly `quorumSig, sig`, and gives the correct 339-byte size. It also expands the contribution's IES fields (`ephemeralPubKey, ivSeed, blobs`), which the old shorthand omitted. The checker now derives all four lists from both pins' serializers and compares them with the actual normative cell, checks the stated size, and pins v24's IES/two-signature framing skips.

**Closure verification:** the final checker passes all 22 checks. Eight scratch-only negative variants fail as expected: the old row, a third signature, a 435-byte stated size, omission of the IES IV seed, swapped complaint bitsets, swapped justification fields, swapped contribution prefix fields, and a missing row. Evidence: `pcommit-shape.json`, `pcommit-framing.cpp`, `pcommit-framing.log`, `check_sources-expanded-final.log`, and `final-fixes/negative-results.json`. **No remaining major.**

### G3-2 — Minor, closed: the new NetInfo documentation overstated re-encoding parity

**Locations:** [netinfo.rs:299](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/primitives/src/types/netinfo.rs:299), [DDE-SWEEP.md:198](/work/home/agent/workspace/rdc-wt/bs4a-decode-domain/docs/node-program/DDE-SWEEP.md:198).

First-group retention, purpose ordering and the resulting **map** serialization match Core. The original unqualified claim that re-encoding gives Core's re-serialization did not hold for every entry the SDK successfully decodes.

**Concrete example:** `01010201020b4578616d706c652e636f6d01bb` is version 1, purpose 2, one Domain entry `Example.com:443`. The SDK accepts it and re-encodes those same 19 bytes. Both release binaries decode the enclosing ProUpServTx but expose `[invalid entry]`: `NetInfoEntry::Unserialize` clears an invalid `DomainPort`, and its serializer writes the one-byte invalid tag `ff`. Consequently Core's NetInfo re-serialization is `01010201ff`, five bytes. This is an inherited entry-normalization gap belonging to the existing BS6a owner row, not a regression in the new map implementation.

**Suggested fix, now implemented:** the `NetInfoV2` field docs qualify the claim as group retention/order and map framing parity, and explicitly describe invalid-entry re-serialization differences. DDE-SWEEP §5 records the concrete byte example, entry normalization and invalid duplicates under BS6a. Core reads and discards invalid duplicate entries, while the SDK can still fail during their entry decode. **Closed as a documentation correction; the previously deferred entry decoders are not claimed fixed.**

Evidence: `netinfo-core.json`, `sdk-requests.jsonl`, `sdk-results.jsonl`; Core `evo/netinfo.h`'s `NetInfoEntry::{Unserialize,Serialize}` at both tags.

## Closure of the previous reviews

| Finding | Round-3 status and verification |
|---|---|
| GPT F1 / Opus m-6: V2 payload bound | **Closed.** SDK tests and fresh encrypted probes at both pins accept short-ID payloads through 3,145,740 bytes, reject 3,145,741, and accept long-form payloads only through 3,145,728. Oversize wins over an unassigned short ID. |
| GPT F2 / Opus M-1: short pong | **Closed.** `Pong::decode_msg` returns `None` without consuming bytes for lengths 0–7. Fresh state probes reproduce cancellation without latency; mismatch remains pending; matching nonce with trailing bytes is measured. |
| GPT F3: handshake gates | **Closed.** Fresh probes assert the version-dependent `sendaddrv2` order and the reconciliation-disabled/enabled `sendtxrcncl` behavior, including disconnect reasons. |
| Opus M-2: generic gates, catches and trailing bytes | **Closed.** Transport rejection precedes handler gates; blocks-only/probe/loading/IBD gates are stated; bloom, governance, spork, sig-share and DKG catches are distinguished; v24 DKG and `qgetdata` exceptions are named. Fresh blocks-only/bloom controls pass. |
| Opus M-3: the original incorrect policy rows | **Closed.** The specific original corrections survive, including govsync's split catch, DKG intake, sig-share caps at both pins, quorum-data authorization, and conditional handshake/filter gates. The additional G3-1 read-shape error was also corrected and checked this round. |
| Opus M-4: headers allocation | **Closed.** Public `BaseCodec` counts are bounded by the 81/39-byte minimum; policy readers cap counts before allocation at 2000/8000. Regression tests pass. |
| Opus M-5: CodeQL failures | **Closed.** Fresh CodeQL 2.27.2 analysis exits 0, scans all 235 Rust files without extraction errors, and reports no findings. The original derive, impl-order, foreign re-export and import-grouping fixes remain in the reviewed head; BS11a-t supplies `BlsCtx`. |
| Opus m-1: transaction check re-encodes | **Closed.** `ByteCount` measures the serialization without allocating it; the large-payload test skip is gone. |
| Opus m-2 / r2 N-7: gappy Version values | **Accepted documented limitation, not eliminated.** Public/serde construction can still make a non-prefix tail. The docs and `has_prefix_tail()` describe it, and encoding stops at the first gap; no debug assertion now panics on serde input. Successful wire decode always constructs a prefix. This follows N-7's permitted “keep the docs” resolution. |
| Opus m-3: provenance overclaims | **Closed.** Source-derived cases are labelled, corpus groups are distinguished from observations, and both releases' `senddsq` captures are checked. |
| Opus m-4 / r2 N-5: fragile probe assertions | **Closed.** Exact peer matching replaces substring matching; checks assert reconciliation ignore/disconnect reasons, bloom score reasons, blocks-only no-exception behavior, control exceptions/scores and the exact logged unknown V2 type. I freshly ran these checks and regenerated the corpus. |
| Opus m-5: reproducibility | **Closed within the documented agentbox environment.** Rev pins and lockfiles are committed; O1 ABI/commit is named. The local `file://` dependency URLs remain an explicit portability limitation. |
| Opus m-7: generic/table omissions | **Closed.** VARINT/type overflow, un-range-checked counts, network-address lengths, version/handshake conditions and loading-block gates are recorded. |
| Opus m-8: version error class | **Closed.** Tests distinguish a mandatory-head failure from `VersionTail`, retaining the head checks after byte 46. |
| Opus r1 nits | **Closed or recorded.** Success-path clone removed, `PayloadDecode::trailing()` added, MsgDecode docs narrowed; the positional `ProtoCtx::new(version, bls)` remains consistent with merged BS11a-t. The conformance companion branch is named in merge notes. |
| r2 N-1: fourteen after-read rows and DKG intake shapes | **Closed under DEC-45.** The post-read column is explicitly informative; the cited fourteen corrections are present. The pin-specific intake equalities/bounds, exception catches and full-consumption rule are in the normative cells and agree with source. The final G3-1 correction closes the additional DKG read-shape error. |
| r2 N-2: NetInfo allocation amplification | **Closed.** No purpose-count reservation; numeric-byte-keyed BTreeMap keeps at most 256 groups, decoding each duplicate before dropping it. Inner lists use exact bounded vector batches. Both the impossible-count and fully backed repeated-group tests pass. |
| r2 N-3: CS2 import break | **Closed as an explicit merge dependency.** §5.1 names the affected CS2 import and the required move to `dash_primitives`. The CS2 consumer import is not changed by this review or BS4a; its actual move remains required at re-pin. |
| r2 N-4: invalid commands reach handler gates | **Closed.** V1 validates through `is_valid_v1`; V2 splitting validates long-form types through `is_valid_v2`. Both return `InvalidCommand`, mapped to transport drop before probe bookkeeping. |
| r2 N-6: public unbounded V2 payload decode | **Closed.** The unbounded public method is gone. `V2Message` has private fields and no serde/default/public constructor; only bound-checked `split_v2` constructs one. |

## Focused attack results

**NetInfo maps.** The numeric `u8` BTreeMap key is important: even unknown purpose bytes sort exactly like Core's `enum class ... : uint8_t` map keys. `or_insert(group)` preserves the first list, including an empty first list; duplicates are fully decoded before insertion, as Core's `UnserializeMap` does. There is no outer up-front reservation. I differentially checked 1,024 deterministic maps containing repeated/descending/unknown purposes, empty lists, valid IPv4/domain entries and trailing bytes against each tag's actual map/vector serialization templates: verdicts, consumption and re-encoded bytes agree. The C++ probe deliberately restricts entry shapes; it is not an oracle for the deferred `NetInfoEntry` decoder. Fresh release-binary RPC probes also confirm first-group behavior and the inherited invalid-entry/version gaps. **Answer: map semantics are byte-exact; the entire NetInfo type is not yet byte-exact, as §5 already acknowledges.**

**Command ranges and V2 typing.** I re-read `CMessageHeader::IsCommandValid` and `V2Transport::GetMessageType` at both tags: V1 accepts 0x20–0x7e before NUL, V2 accepts 0x20–0x7f, and all bytes after the first NUL must be zero. All-zero and full twelve-byte printable types are valid. A scratch SDK probe checked 39,936 combinations spanning every byte value, position and prefix/padding boundary through validation and dispatch/splitting. All 255 nonzero short IDs agree with both Core tables, including reserved 5/29–32 and out-of-slot errors. Fresh V1 and encrypted V2 probes pass every committed command/boundary case at both pins. Type errors remain distinct from payload errors; a typed short `ping` retains its command when its handler throws. **No new mismatch found.**

**Vector reservation/count sweep.** `reserve_exact(allocated - items.len())` requests the same total element capacity as Core's `reserve(allocated)`; normal allocation does not geometrically overshoot the target. I recorded successful and failing `Vec<Vec<u8>>` decodes immediately below/at/above the first two batch boundaries and at 600,000 elements. On this platform a 24-byte element gives reservations of 4,999,992, 9,999,984 and finally 14,400,000 bytes for the latter case; each increase is at most one batch beyond the items read. I swept every count/size decode and reservation site in all SDK crates: transaction inputs/outputs, block txs, asset-lock outputs, inventory/locators, address lists, payload lists and NetInfo inner lists use `Vec<T>`; byte strings/scripts/payloads use checked `read_bytes` before copying; DynBitset uses the rounded byte count; headers have their proven minima/handler caps. Fixed cache reservations are constant. ECDSA/address length prefixes have tighter fixed bounds; the old IES decode-size gaps remain assigned to BS9a/BS11a in §5. **No new amplification path found among node-decoded types.** Core statically disallows vector elements larger than the allocation batch; the SDK's generic implementation has no equivalent restriction, but no reviewed wire element approaches that size.

**`check_sources.py`.** The original 16 assertions passed; the final expanded version passes all 22. I also checked their meaning in the surrounding functions: map insertion keeps the first key; the command range distinction is real; v23 DKG intake performs typed/BLS decode with bitset equalities and allows trailing bytes; v24 does a BLS-free framing walk with bounded bitsets, equal complaint counts and whole-payload consumption, deferring size equality to PreVerify. The added checks derive and compare DKG field order, expand IES and justification fields, compute the premature-commitment size, and pin v24's two-signature skip. Eight negative table fixtures confirm those checks fail on the relevant mistakes. It remains a source regression guard for named claims, not a complete proof of every normative row or post-read behavior.

**Settledness.** **Yes, with the reviewed round-3 corrections included.** DEC-45 resolves the former post-read completeness objection. The transport rules, gate ordering, normative read shapes, catch/scoring policy and trailing rules pass this round. No post-read behavior is being promoted back into BS4a's normative scope. The BS6a/BS9a/BS11a deferred decoder work and documented re-pin dependencies remain explicit; settledness of the policy table does not claim those future implementations already exist.

## Validation

| Check I ran | Result |
|---|---|
| `cargo nextest run --config-file maint/nextest.toml --profile=ci --workspace --features full` | **1175 passed, 1 skipped** |
| `RUSTFLAGS='-D warnings' cargo clippy --workspace --exclude dash-pow --all-targets --no-default-features` | **Pass** |
| Same clippy command with `--features full` | **Pass** |
| `cargo fmt --all --check` | **Pass** |
| `python3 contrib/node/dde-probe/check_sources.py /work/home/agent/workspace/dash` | **Pass**, original 16 and final expanded 22 checks |
| Final checker against eight malformed scratch copies of DDE-SWEEP | **Pass**: every variant rejected, correct baseline accepted |
| Fresh `probe_p2p.py`, `probe_payload.py`, `probe_gates.py`, `probe_v2.py`, `gen_corpus.py` | **Pass at both release pins**; 68 regenerated Core outcome strings equal the committed corpus |
| Scratch SDK command/short-ID/map/allocation probes and Core-template/framing probes | **Pass**, with the scoped NetInfo parity limitations and corrected G3-1 described above |
| CodeQL 2.27.2, `lint_codeql.py run --lang=rust --with-suite=rust-security-and-quality`, on an exported 0c71f50 snapshot | **Pass, zero findings**; 235/235 Rust files extracted without error |
| CodeQL formatting check | **Pass**, all 22 QL files conform |
| `git diff --check` on both final reviewed worktrees; compile final checker | **Pass** |

Logs and probe sources are retained in `/work/scratch/bs4a-r3-gpt/`. The block instruction benchmark was not rerun in this round; this report makes no new independent claim about the recorded −2.45% result.

**Verdict: APPROVE.** No remaining blocker or major at the final reviewed heads. The prior findings are closed by their permitted resolutions, including the explicit re-pin dependencies and accepted Version documentation limitation.

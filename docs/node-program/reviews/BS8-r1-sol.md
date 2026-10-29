# BS8 first review — Sol

Reviewed base-sdk `d53e0927907919a0a29c679c73e79288ac68f393` against `4b4e76b` (50fb1f1, 53e5354, d8e4b5e, d53e092), and rust-dashcore `4a4b1299b9f465a57baa60b0ee4536e429695eb0` against `58fd0e75` (63906f07, 9e5e7c1a, 4a4b1299). This is the Sol pass before Opus high under DEC-92. Read REVIEW-COMMON, CHARTER, PROCESS, the BS8 PLAN row, DEC-98, the BSX leniency catalogue, and BS7's p2p-core facts.

The review used detached worktrees under `/work/scratch/bs8-review/`, `TMPDIR=/dev/shm/bs8-review-tmp` with mode 700, `RUSTC_WRAPPER=`, `EIO_BACKEND=posix`, and 12 Cargo build jobs. The Core judge was rebuilt from a fresh archive of pin `a9a483e51efdadeee37cf40ff8697da62b3cd012`; the scoped serializer and handler files are identical to release tag `v24.0.0-rc.3` (`da567d7dba945143eee4d2ce1621c21b32089989`). PLAN §7's cbTx write-back class was treated as a codename; its mechanics were not investigated.

## Findings

No blocker or major finding.

### F1 — minor: corpus reduction drops indexed commitments and current-band SML v3

**Locations:** rust-dashcore `contrib/node/dde-probe/bs8/make_corpus.py:73`, `:78`; base-sdk `pkgs/p2p_core/corpus/core_overlay.json5:16`.

The selector groups by command, stream protocol version, scheme, verdict, and a few result properties, then retains the three shortest inputs. It does not retain representatives by the payload's own version/layout. The committed corpus has ten FinalCommitment v1 cases and eight v3 cases, but no v2 or v4 case: the two layouts carrying a quorum index disappear. Standalone SML v3 survives only at 70228/70229/70230, with no current-band v3 case. A regression in indexed commitment decoding can therefore pass this compact regression suite. The full external fixed suite covers these variants, so this is a durable coverage gap, not a demonstrated codec failure.

**Fix:** include payload version/layout and SML type in selection, preserve representatives for each BLS-field position, and assert required versions/bands before writing the corpus. Regenerate the fixture.

### F2 — minor: the four-commit series is not independently lintable at every commit

**Locations:** base-sdk at `53e5354`, `pkgs/p2p_core/src/msg/llmq.rs:205`, `pkgs/p2p_core/src/msg/rotation.rs:118`, and `pkgs/p2p_core/src/msg/mn_list.rs:198`; fixed by `d53e092`.

The feature commit introduces non-wire `QuorumData` and `CycleInfo` with `TypeId` rather than `Unencodable`, and puts `MsgDecode` implementations before the inherent implementations. Those violate the existing codec/declaration CodeQL policies. They persist through `d8e4b5e`; only the fourth commit corrects them. The final head is clean, but exporting the literal four commits would fail the BSX §6 requirement that every commit compile and lint on its own. This follows from the introducing/fixing diffs and existing policy; I did not rerun CodeQL at each historical commit.

**Fix before upstream export:** fold each policy correction into the commit introducing the type/implementation, or rebuild the upstream units as BSX §7 requires, then run their gates per commit. Retain the Co-Authored-By trailer under DEC-98 Q-3.

## Serializer, strictness, and API assessment

- **Protocol layouts:** MnListDiff correctly separates unversioned, version-after-cbTx, versioned-entry, version-first, and ChainLock-signature bands at 70225/70228/70229/70230. QRInfo passes the same layout into all its nested diffs. SML defaults to v1 before 70228, adds the type from entry v2, switches network info from entry v3, and drops the Evo HTTP port from v3. NetInfo retains the authorized lossy reader.
- **Limits and edges:** getqrinfo caps base hashes at 4096, spork caps the signature at 65, and qgetdata rejects payloads over 67 bytes before reading. qdata uses the caller's threshold/member bounds before reading vectors. Unknown enum values, empty vectors, CompactSize forms, padding bits, truncated fields, and trailing bytes were checked against the appropriate serializer or read component. Reject cursors and Core reason/penalty text are outside the DEC-98 comparison contract.
- **BLS and judge fidelity:** the judge executes genuine `CDataStream >> CoreType` reads and Core write-back, rather than a duplicate parser. It resets the process-wide scheme per request. Both schemes are exercised for eager and lazy fields; platformban always selects basic, while a commitment selects its scheme from its own version (v1/v2 legacy, otherwise basic). ISDLOCK/recsig signatures and SML operator keys remain lazy wire bytes. The differential composes SDK decoding with the declared node-side eager BLS classification and CL-signature map normalization. Its equality result describes that composition, not every bare SDK value.
- **qdata boundary:** the judge faithfully invokes Core's request and bounded vector deserializers. It does not execute the complete stateful NetQuorum handler. Exact-size bans, state/membership checks, and whether contribution reads are reachable remain node-side, as authorized by Q-1/Q-12. The caller must preserve that distinction when wiring these types into P2a.
- **Strict flags:** SML isValid and getqrinfo/qrinfo extraShare reject values outside 0/1. Core accepts nonzero bytes and writes 1. A crafted getqrinfo extraShare=2 therefore has an observable decode difference, but no rejection of an honest Core-emitted message was demonstrated. This matches A-12's explicit class-(c) ruling and DEC-98's strict SDK intent. The differential accounts for these separately and verifies that changing only the offending flag to 1 restores agreement. Future node handling/reasons are not established by this SDK gate.
- **Box and breaking APIs:** `BaseCodec for Box<T>` delegates the cursor and errors to T, allocates only after successful decoding, and delegates encoding. The boxed MsgDecode implementation forwards ProtoCtx, so boxed QRInfo keeps negotiated layouts. The `feat!` commit and changelog identify the stub-to-typed variants, boxes, removed MnListDiffPayload, and changed SML fields. No existing production rust-dashcore consumer of the removed API was found; the new standalone differential uses the new API.
- **Upstream conventions:** all four commit messages pass the repository's namespace/type/width/height lint, including the breaking marker and DEC-98 attribution. No policy exemption was added. The fork remains subject to the existing BSX rebuild/scrub conventions; p2p_core is unreleased, so its fork changelog is omitted when rebuilding upstream units. F2 concerns the historical series, not final-head lint health.

## Independent validation and evidence limits

- SDK: `cargo test -p dash-p2p-core -p dash-types --features full`: **212 passed, zero failed; two doctests ignored**. Clippy for both crates, all targets, with `full` and with no default features, `-D warnings`: **passed**. Formatting, whitespace checks, and `lint_all.py --exclude lint_codeql`: **passed**; Semgrep reported zero findings. Explicit commit lint over `4b4e76b..d53e092`: **4/4 passed**.
- Original locked `bs8-tool` release build and its three comparator/normalization tests: **passed**. Fresh, unmodified judge build: **passed**, manifest records `build fresh`, the stated pin, source SHA-256 `4a88a85e51b2952b33821a45670a1462d53bd7a2e23f3429fe44007ec2d28b38`, and binary SHA-256 `de3e983d8bd76f509c6fadd9fc0e434573a37cc8db65b1f29e83ff8da17e0480`.
- Fixed differential: **2,296 cases**, **zero mismatches and zero write-back differences**, with **12 declared strict-flag cases counted separately**. Per type: clsig 114; mnauth 114; platformban 114; qfcommit 1,248; isdlock 57; qsigrec 57; smlentry 284; mnlistdiff 154; qrinfo 21; qsnapshot 7; qgetdata 5; qdata 112; getqrinfo 5; spork 4.
- O4 fixtures: **35 MnListDiff and two QRInfo fixtures**, all read completely and round-tripped exactly. Regenerated `messages.py` vectors from the pinned tree are byte-identical to the committed **22 vectors**, and all pass the fresh judge comparison.
- Regenerated `core_overlay.json5` from the live fixed cases and fresh manifest: **206 cases, identical JSON body** to the committed corpus. The binary provenance hash differs because this is an independent build; the judge source hash matches.
- Additional review-only boundary suite: **80,482 cases under both global schemes**, **zero unclassified differences**; **660 noncanonical strict-flag cases counted separately**. It covers all commitment versions plus unknown versions and signed index edges; empty and unknown-version ISDLOCK/recsig forms; snapshot count/padding/mode edges; getqrinfo 4096/4097; spork 64/65/66; qdata masks, unknown errors, vector caps and zero sizes; SML entry versions/types and protocol boundaries; empty QRInfo lists, optional cycles, tails, and truncations.
- An initial artificial `i32::MAX` stream probe produced **60 SML differences**. That integer also sets Core's internal `ADDRV2_FORMAT` bit (`src/netaddress.h:30`, `:245`, `:258`), which changes legacy address serialization. It is not a v24 negotiated common protocol version: Core clamps the advertised version to `PROTOCOL_VERSION` (`src/net_processing.cpp:4042`). The final suite uses flag-free 70243 for that direct future-band test. This is an out-of-domain serializer-flag probe, not a reachable overlay interop finding.
- Independent mutation smoke, classifier off, nice 19, two workers, about 31 seconds each: qdata **22,983 cases**, qsnapshot **90,240 cases**; **zero mismatches and zero write-back differences**. These short runs do not replace BL-55's remaining budget.

The implementer's final-head CodeQL run reports all six policy queries with zero findings; I did not independently rerun CodeQL. The initial violations and their fixes are source-verifiable as F2 describes. Nix lint skipped because Nix is unavailable. These qualifications are not presented as independently passing those checks.

The four full fuzz runs were clsig, qfcommit, mnauth, and mnlistdiff, each reported at 6600 seconds. That allocation is defensible: the first three are the explicit PLAN priorities, and mnlistdiff adds nested, version-dependent structure. CLSIG and MNAUTH share eager BLS machinery, so QRInfo, qdata, and SML deserve priority when paying the remaining budget. The recorded 8.2/24 CPU-h shortfall and pending root re-pin are BL-55, not new findings; its existing restriction before node P2P consumption remains in force.

Review branches were not modified; no commits, pushes, or GitHub messages were made. The report is left uncommitted. Owned scratch worktrees, build products, helper source, logs, processes, and RAM scratch are removed before handoff.

**Verdict: APPROVE-WITH-NITS.**

**Sol gate satisfied: yes.** Ready for the sequential Opus-high review under DEC-92; the existing BL-55 prerequisite before node P2P consumption remains unchanged.

# BS8 review — Opus high (sequential, after Sol r1)

Reviewed base-sdk `node/bsdk-bs8-overlay` **4b4e76b → 81b89c7** (50fb1f1, 7f22c2c, 81b89c7) and rust-dashcore `node/bs8-overlay` **58fd0e75 → a00b8f0a** (judge, differential, vectors, corpus generator). Inputs: REVIEW-COMMON, the PLAN BS8 row, DEC-98 (A-12 strict flags, Q-1, Q-12 byte bags), DEC-109, BL-55, the Sol r1 report and its two nits. Core reference: v24.0.0-rc.3 (`da567d7d`); the judge pin `a9a483e5` was checked to be identical to the tag for every scoped file (`src/evo/{smldiff,simplifiedmns,netinfo}.h`, `src/llmq/{snapshot,commitment,quorums,signing}.h`, `src/llmq/net_quorum.cpp`, `src/bls/`, `src/serialize.h`, `src/net_processing.cpp`, `src/version.h`, and the rest of the judge's headers).

Environment: `/work/scratch/bs8-opus-review`, `TMPDIR=/dev/shm/bs8-opus-tmp` (0700), `RUSTC_WRAPPER=`, `EIO_BACKEND=posix`, separate targets. No branch under review was modified, nothing was committed or pushed. PLAN §7: one item touches an embargoed class; it is referred to by codename only (see "PLAN §7" below).

## Findings

### F1 — major: `Hashable` on `SimplifiedMnListEntry` (and `PlatformBan`) is not Core's object hash

**Where:** base-sdk `pkgs/p2p_core/src/msg/mn_list.rs` `hash_impl!(SimplifiedMnListEntry)` (after `impl BaseCodec for SimplifiedMnListEntry`); `pkgs/p2p_core/src/msg/masternode.rs` `codec_p2p!(PlatformBan {..})` (which expands to `hash_impl!`, `pkgs/primitives/src/codec.rs:71/75`).

`hash_impl!` hashes `BaseCodec::encode`, which for an SML entry is `encode_as(.., true)`: the 2-byte entry version comes first. Core's only hash of this object is `CSimplifiedMNListEntry::CalcHash()` (`src/evo/simplifiedmns.cpp:45-50`), written with `SER_GETHASH`; the version and the DMN-type cut-off are gated on `SER_NETWORK` (`src/evo/simplifiedmns.h`, `SERIALIZE_METHODS`), so the hash covers the entry **without** its version but **with** the type fields. That is exactly `encode_as(.., false)`. These are the leaves of `merkleRootMNList` (`simplifiedmns.cpp:86`).

**Concrete case:** the 15 entries of Core's `simplifiedmns_merkleroots` (`src/test/evo_simplifiedmns_tests.cpp:19-66`, legacy keys from secrets `[i,0,…]`). `sha256d(encode_as(false))` reproduces all 15 expected hashes (`3a1010e2…90c1` … `67798d40…be9b`); `entry.hash()` matches none (first: `a4a7d722…68f4`). Any consumer that builds the SML root from `Hashable::hash()` (SPV, EV4-serve, SB7) computes a root Core rejects. The impl predates BS8, but BS8 retyped this struct as the SML entry of the PLAN row and kept it.

Same class, smaller: `PlatformBan::hash()` hashes the signature too, while Core's `PlatformBanMessage::GetHash()` (`src/masternode/meta.cpp:267`, with the `SER_GETHASH` gate in `meta.h:176-182`) excludes it and is the inventory/seen-cache hash (`net_processing.cpp:3850-3860`). `PlatformBan::signed_hash()` is right; `hash()` disagrees with it. `ClSig::hash()` hashes the wire signature, whereas Core's inventory hash `::SerializeHash(clsig)` (`net_processing.cpp:5798`, `chainlock/handler.cpp:130`) re-serializes the eager signature (zeros for a signature canonical only under the other scheme); document that the node-side classifier must normalize first.

**Fix:** implement `Hashable` for `SimplifiedMnListEntry` as `sha256d(encode_as(false))` (or drop it in favour of an explicit `calc_hash`), and add Core's `simplifiedmns_merkleroots` hashes and root (`0bae2176…4022`) as a test. Make `PlatformBan`'s `Hashable` equal `signed_hash()` or drop the derive. A doc line on `ClSig`.

### F2 — minor: qgetdata "oversized" reuses the transport `PayloadTooLarge`

**Where:** `pkgs/p2p_core/src/msg/llmq.rs` `impl MsgDecode for QuorumDataRequest`; `decode_with_policy` docs in `pkgs/p2p_core/src/macros.rs:177-181` ("Returns `PayloadTooLarge`, on which Core disconnects").

Core's qgetdata handler first applies the role/verified gate (`Misbehaving 10`, `src/llmq/net_quorum.cpp:80-83`), then the length check `Misbehaving(100, "oversized qgetdata")` (`:90-93`). That is a discouragement (with the usual noban/manual exceptions), not the transport's unconditional disconnect for a payload over `MAX_PROTOCOL_MESSAGE_LENGTH`. A P2a implementer following the documented mapping gets the penalty wrong; distinguishing the two today needs `command == "qgetdata"` or `max == 67`. **Fix:** a distinct error variant (or reuse `TooManyEntries`-style handler errors), and correct the `decode_with_policy` doc. The state gate ordering is already covered by the "gates run before this" contract.

### F3 — minor: `mnlistdiff`/`qrinfo` decode where Core's handler reads nothing, at a measurable cost

**Where:** `impl MsgDecode for MnListDiff` / `for QrInfo` (`mn_list.rs`, `rotation.rs`); `decode_with_policy` "decodes … as Core's handler for `cmd` reads its payload".

Core never reads either from a peer: its handlers are `Misbehaving(100, "received not-requested …")` and return (`src/net_processing.cpp:5654-5657`, `:5739-5743`). The judge says so too (`judge.cpp` header). The SDK necessarily implements the serializer (SPV and the node's own requests need it), but the docs claim handler semantics, and the cost is real, because Core never had to harden these paths:

- 3,145,792-byte `mnlistdiff` with 10,180 v3 commitments (valid basic key/signature samples from `bls_wrapper.json5`): **5.0 s CPU** to decode (eager commitment BLS), on agentbox at load ~85.
- 3,100,315-byte `mnlistdiff` with one v3 SML entry whose ExtNetInfo group holds 3.1 M unknown-tag entries (1 wire byte, 48 bytes in memory): **150 MB RSS**, 0.19 s. Core's NetInfo reader has the same ratio (it reads ProTx payloads from peers), so this is parity, not a reader defect.

**Fix (docs + consumer contract, no read-domain change):** state on these two `MsgDecode` impls that Core's handler reads nothing and bans unsolicited ones; P2a must apply "not requested → ban" before decoding, and SPV must decode only solicited responses, off the network thread, with a per-peer budget. Record it where P2a and the SPV wiring will see it.

### F4 — minor: `QData::read_data` cannot mirror Core's stateful contribution read

**Where:** `pkgs/p2p_core/src/msg/llmq.rs` `QData::read_data` (`read_bounded` is private).

Core reads the verification vector, bans on a size other than the threshold **before** reading anything else (`net_quorum.cpp:244-255`), and reads contributions only when the node is an active masternode, the vvec is valid and the requested member is in the quorum (`:303-334`). `read_data` reads both whenever the mask asks. **Case:** mask 3, vvec of exactly `threshold` valid keys, contributions count `valid members + 1` (my probe `qdata_vvec_eq_contrib_over`): `read_data` fails as a whole; a non-masternode Core node never reads the contributions and accepts the vvec. The doc defers "reachability" to the caller, but the caller cannot read the vvec alone and then decide. **Fix:** expose `read_vvec` and `read_contributions` (continuing from the vvec's end), or a flag; keep `read_data` as the composition.

### F5 — nit: `MnListDiff` codec doc contradicts `encode`

The type doc says "the codec reads and writes the current layout", but `encode` writes `self.format`. A diff read at 70228 and re-encoded is not readable by `BaseCodec::decode` (current layout). `QrInfo`'s doc states it correctly. Reword.

### F6 — nit: CL-sig map semantics and where the normalization lives

`MnListDiff::quorum_cl_sigs` says Core "drops repeated keys"; say the **first** entry's index set is kept (`std::map` `Unserialize` uses `insert`, `src/serialize.h:1232-1243`), the key being the re-serialized eager signature (all signatures canonical only under the other scheme collapse onto the zero key). The only implementation of that normalization is the test tool's `predict::cl_sig_map`; move it to the node-side BLS classifier (DEC-109's `dash-evo::bls`) before EV/P2a need it.

### F7 — nit: scheme tags on BLS byte bags are nominal

Some BLS byte-bag fields carry a scheme tag that is nominal rather than authoritative (`SimplifiedMnListEntry::operator_key`, and the global-scheme fields clsig, mnauth and qdata vvec). Consumers must take the scheme from `operator_key_scheme()` or the context, never from the type's tag (PLAN §7 handling; details private). Add a field doc pointing at `operator_key_scheme()`, or use a scheme-neutral bag for fields whose scheme is not static.

### F8 — nit: the A-12 refusal has no dedicated error

The strict-flag refusal surfaces as `DecodeError::InvalidValue { expected: [0, 1] }`, and the differential recognizes it by message text. P2a must not map it like other getqrinfo failures: Core bans any getqrinfo read failure with 100 (`net_processing.cpp:5662-5668`) but **serves** a request with `extraShare = 2`. A dedicated variant (e.g. `NonCanonicalBool`) would make that mapping and the tool's classification robust.

## Focus questions

1. **Read-domain equivalence.** Checked by hand against rc.3 and confirmed by the differential:
   - MnListDiff (`smldiff.h:64-79`): version first ≥ 70229; after cbTx for [70225, 70229); entry versions ≥ 70228; CL sigs ≥ 70230; default 1 elsewhere. `MIN_PEER_PROTO_VERSION` is 70221, so the unversioned band is 70221–70224. EFFICIENT_QRINFO (70236), ISDLOCK_CYCLEHASH (70237), PLATFORM_BAN(_V2) (70238/70240) change construction, semantics or short IDs, not these serializers.
   - SML entry (`simplifiedmns.h:72-100`): version ≥ 70228 only; NetInfo extended iff version ≥ 3; operator key lazy, legacy iff version == 1 (version 0 and ≥ 4 are basic, matching `protx_bls_scheme`); type iff version ≥ 2; HTTP port iff Evo and version < 3; the < 70227 early return is unreachable because the version defaults to 1 below 70228. Unknown types read no extra fields. ExtNetInfo: unknown version reads one byte; map reader is first-wins in purpose order.
   - QRInfo (`snapshot.h:125-218`) and snapshot (`:42-86`, `ReadFixedBitSet` `serialize.h:452-475`, skip mode `int32`): matched, nested diffs take the stream version.
   - qdata (`net_quorum.cpp:204-334`, `quorums.h` request reader with the caught error byte, `UnserializeVectorWithMaxSize` `serialize.h:1000-1009`): matched, subject to F4.
   - qgetdata: the 67-byte cap is `GetSerializeSize` of a request with `UNDEFINED` error (not serialized), `net_quorum.cpp:90`; matched, subject to F2.
   - CL-sig map: eager `CBLSSignature` under the global scheme with the opposite-scheme reset (`bls.h:184-200`), first-wins, hash order; the prediction reproduces Core's write-back byte for byte (subject to F6).
   - Remaining types (clsig, isdlock lazy, qsigrec lazy, qfcommit v1–4 with version-chosen scheme, mnauth, spork `LIMITED_VECTOR(65)`, platformban basic-only eager): matched; trailing bytes are ignored as in Core.
2. **Judge independence: yes.** `judge.cpp` links dashd's own archives (`libbitcoin_node/common/util`) from a fresh `git archive` of the pin and runs `CDataStream >> T` and `<< T`; no base-sdk code. Two hand-written parts mirror the handler: the qgetdata length check (using Core's `GetSerializeSize`) and the qdata read sequence, which omits the state gates (F4). Inputs are Rust-generated; verdicts and write-backs are Core's. The prediction composes base-sdk with rdc's BLS rules and `dash-consensus` cbTx write-back, so equality covers that composition, as Sol noted.
3. **Strict flags: safe for honest Core peers.** Core writes `bool` as 0/1; `qrinfo.extraShare` is Core's own bool copied from the parsed request; SML `isValid` comes from list state. No honest Core message is refused. The only interop deviation is a third-party getqrinfo with a non-canonical bool, which Core serves and we refuse (DEC-98 accepts this); the ban mapping is F8. In my probes the other strict hits were malformed entries (an unknown ExtNetInfo version shifting later fields onto `isValid`).
4. **Allocation and DoS.** Counts are bounded by bytes left (`into_len(data.len())`), with Core's per-batch reservation in `Vec::decode`, `ReadFixedBitSet`'s remaining-bytes bound, `LIMITED_VECTOR` caps checked before reads (getqrinfo 4096, spork 65, qdata limits), and `BTreeSet<u16>` index sets capped at 65,536 distinct values. Struct sizes: entry 200 B, diff 336 B, commitment 904 B, NIEntry 48 B, `QuorumClSig` 120 B. Worst cases are in F3; nothing unbounded.
5. **API fit.** P2a: typed variants, boxed large payloads, `ProtoCtx`-driven layouts; F2/F3/F8 are contract gaps for the penalty walkers (Q-1). EV2/EV4/SB7: F1 must be fixed; F6, F7. LQ: commitments eager per version, `QuorumSnapshot`/`SnapshotSkipMode::Unknown`, `DynBitset` fit; F4 for the qdata consumer.
6. **Fuzz gap (15.8 CPU-h).** It does not block the merge into `node/base-sdk`, provided BL-55's condition holds (no node P2P consumer until it is paid; M1 exit item 3 needs ≥ 24 CPU-h). O2c's in-process `diff_p2p_*` targets were built against d53e092, whose source equals 81b89c7 except `core_overlay.json5`, so the farm can now pay it. Priority: qrinfo, smlentry, qdata, mnlistdiff (nested and versioned), then getqrinfo, isdlock, platformban.

## Independent validation

- base-sdk head 81b89c7: `cargo test -p dash-p2p-core -p dash-types --features full` **212 passed, 0 failed** (2 doctests ignored); `cargo clippy … --all-targets --features full -D warnings` **clean**. Per commit (from `git archive`, fresh target each): 50fb1f1 clippy clean; 7f22c2c clippy clean and 119 p2p-core tests pass. Sol's F2 (per-commit lint) is fixed: `QuorumData`/`CycleInfo` are `Unencodable` from 7f22c2c on. I did not rerun CodeQL.
- Judge: **fresh** cold build at pin `a9a483e5` (≈ 14 min with cached depends sources), source SHA-256 `4a88a85e…8b38` = the committed `judge.cpp`, binary `953da480…b5fd`.
- `bs8-tool` (`--locked`, release) and its 3 unit tests: pass. `cases`: **2,296 cases, 0 mismatches, 0 write-back differences, 12 strict-flag cases** (getqrinfo 1, qrinfo 7, smlentry 4); **37 O4 fixtures** (35 mnlistdiff, 2 qrinfo) read and round-trip exactly. Per-type counts equal Sol's.
- `make_corpus.py` on my run: **445 cases, body identical** to the committed `core_overlay.json5` (Sol's F1, the coverage nit, is fixed; the census includes qfcommit v1–v4 × three BLS fields and SML v3 at 70242). `gen_vectors.py` from the pin tree: byte-identical **22 vectors**, all matched.
- My own probe set (333 cases, `vectors` mode): CL-sig map with duplicate keys, distinct opposite-scheme keys collapsing onto zero, zero/opposite orderings, a throwing key after a valid one and repeated indexes (at 70230 and 70242); SML versions {0,1,2,3,4,0xffff} × types {0,1,2,0xffff} at 70228/70229/70230/70242, standalone and inside diffs; ten ExtNetInfo oddities (versions 0/2/255, duplicate and unknown purposes, invalid and valid domains, a 600-byte addrv2 address, TORv3, unknown network); six operator-key classes at v1–v3; diff versions {0,2,0xffff} at five bands; ten qdata edges; qgetdata at 1/66/67/68/200 bytes; qrinfo with/without the extra cycle at four bands; snapshot modes and padding; isdlock versions; spork signature lengths 0/65/66 and a non-canonical length; opposite-scheme signatures in platformban/mnauth/clsig. **0 mismatches, 0 write-back differences**; 2 strict-flag hits (malformed ExtNetInfo version 255).
- Fuzz smoke, nice 19, 4 workers × 300 s, classifier off: qrinfo 684,570 runs, smlentry 4,883,102, isdlock 6,387,156, getqrinfo 106,697; **0 mismatches, 0 write-back differences**. Not a substitute for BL-55.
- `check-base-sdk-pins.sh`: every error is in `contrib/node/dde-probe/bs8/Cargo.lock` (the known BL-55 re-pin); nothing else fails. I did not rerun `selftest-checks.sh`.

## PLAN §7

The SML-entry operator key and the MnListDiff BLS fields were checked against EMBARGO-5's handling section: **consistent**, no finding beyond F7's general doc point. Details are in the private reviewer note, not here.

## Verdict

**CHANGES-REQUIRED**, for F1 only: a small fix (entry hash over `encode_as(false)` with Core's merkle-root vector, and `PlatformBan`'s `Hashable`). F2–F4 should be fixed or recorded as P2a/LQ contract items before node consumption; F5–F8 are documentation and ergonomics. Read-domain equivalence, judge independence and the differential claims all reproduce; with F1 fixed this is APPROVE-WITH-NITS, and the BL-55 restriction stays in force.

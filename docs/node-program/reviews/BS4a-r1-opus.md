# BS4a review, round 1 (Opus)

**Scope.**
- base-sdk branch `node/bsdk-bs4a-decode-domain` at f8a088f: 19 commits on `node/base-sdk` 33fcc59, 49 files.
- rust-dashcore branch `node/bs4a-decode-domain` at c5534013: `docs/node-program/DDE-SWEEP.md` and `contrib/node/dde-probe/`.

**Context read:** the PLAN BS4a row, A1 §1.4, §1.4.1 and §2.14, DEC-24, base-sdk CLAUDE.md/AGENTS.md, and Dash Core at v23.1.8 and v24.0.0-rc.3.

**What I did:**
- read the whole diff;
- ran the `code-review` skill at high effort as one input, and verified its findings;
- ran the base-sdk gate;
- re-ran the bench gate;
- had three independent passes. The first checked every DDE-SWEEP §2 and §4 row against Core's handlers at both tags. The second re-ran the Core evidence (probe, `decoderawtransaction` and the O1 oracle) at both pins. The third trial-merged with BS11a-t.

Scratch: `/work/scratch/bs4a-r1-opus/`.

## Verdict: CHANGES-REQUIRED

The base-sdk code is close to mergeable:
- the shapes match A1 §1.4 rows #1–#9;
- the commits are small and in base-sdk style;
- every Core-run verdict I re-ran reproduces at both pins;
- the tests pass (1113/1113);
- block decode is not slower.

What holds it back (all major, no blocker):
- **The §4 policy table is not "settled".** PLAN §3 makes that the precondition for BS4a's critical-path slot. Many rows misstate what Core's handler does on a decode failure or which penalty it applies. The generic rule is wrong for several unmarked commands, and one pre-decode gate is missing (M-2, M-3).
- **`pong`, typed in base-sdk now, still diverges** from Core's decode domain (M-1).
- a memory-amplification regression in the public `Headers`/`Headers2` decoders (M-4);
- `just lint` fails on CodeQL policy findings that the author's environment could not run (M-5).

## Results I reproduced

| Check | Result |
|---|---|
| base-sdk gate (`just test lint`) | `just test` fails as-is in `dash-pow`: dead SIMD code under `-D warnings`. That failure is pre-existing at 33fcc59 and not BS4a's. With the known workaround: clippy with `--exclude dash-pow` passes in both feature sets; nextest passes **1113/1113**; doc tests and `cargo doc -D warnings` pass; `lint_all.py --exclude lint_codeql` passes (nix skipped). **`lint_codeql.py run` (part of `just lint`) passes at 33fcc59 and fails on BS4a with 11 findings (M-5)** |
| Bench gate (34 mainnet blocks × 200, `instructions:u`, minus the load run) | 33fcc59: 912.1 M. BS4a: 894.2 M, a **−2.0%** change that matches DDE-SWEEP §6 |
| P2P probe, regtest release binaries of both pins (own ports and datadirs, stopped afterwards) | All 40 corpus entries regenerate identically, apart from the bytes Core randomizes (time, nonce, challenge). `gen_corpus.py`'s agreement asserts pass |
| `probe_payload.py` | The 6 cases reproduce at both pins |
| O1 oracle cases | The 11 cases match the `primitives/corpus/decode_domain.json5` `core` values at both pins. This works only with `node/o1-oracle-sys` at 4ff3d4c2 (m-5) |
| Core test-vector bytes | The tx_invalid.json negative, MAX_MONEY+1 and MAX_MONEY+1-output vectors are byte-exact; so are `fdfc00` (`noncanonical`), `fe00000002` (`compactsize`) and the at-max `dynbitset_accepts_maximum_size` bytes (a v24-only test, labelled as such) |
| 74 commands vs `allNetMessageTypes` | Equal at both tags |
| Merge with BS11a-t | See the BlsCtx section |

## Findings

### Major

**M-1. `pong`: base-sdk rejects bytes Core's handler accepts, and the table calls that "equivalent".**
- **Where:** base-sdk `pkgs/p2p_core/src/msg/ping.rs:27-33` (`Pong { nonce: u64 }` via `codec_p2p!`) and `msg/mod.rs:200` (default `MsgDecode`); DDE-SWEEP.md:83.
- **What Core does:**
  - Core reads the nonce only if `nAvail >= 8`. Otherwise it reads nothing, sets `bPingFinished`, and clears `m_ping_nonce_sent` (v24 `net_processing.cpp:5509-5524`, v23 `:5434-5447`). A short pong therefore **cancels the outstanding ping**.
  - Treating it as a decode error leaves the ping pending, and the peer is later disconnected for ping timeout (v24 np:6156, v23 np:6010). Core would keep it.
- **Why the table doesn't save it:** the table maps decode errors to "logged". A `dashd-p2p` built from it therefore diverges.
- **Under rule DDE,** this is a payload Core's handler accepts, so it is BS4a's to fix in the decoder, not to defer.
- **Fix:** give `Pong` a `MsgDecode` that reads the nonce only when at least 8 bytes remain, e.g. `Pong { nonce: Option<u64> }` or a dedicated `ShortPong` outcome. Correct row 83 to say a short pong cancels the ping without a penalty.

**M-2. DDE-SWEEP §4 preamble: the generic rule and the gate list are wrong for several commands.**

DDE-SWEEP.md:63 states three things:
- an exception "costs nothing, unless the handler catches it itself (marked below)";
- trailing bytes are ignored;
- the gates are only version-order and pre-verack.

Core contradicts all three.

- **A pre-decode disconnect is missing: blocks-only.**
  - `RejectIncomingTxs(pfrom) && NetMessageViolatesBlocksOnly(msg_type)` disconnects before any decode (v24 np:4382-4386, v23 np:4336-4341).
  - It covers 25 commands (`protocol.cpp:185-212`, the same at both tags): `tx`, `dstx`, every `ds*`, `getqrinfo`, `qrinfo`, `platformban`, `qsendrecsigs`, `qsigrec`, the sig-share and DKG messages, `qgetdata`, `qdata` and `qwatch`.
  - Failure: a malformed `qsendrecsigs` from a block-relay-only peer disconnects in Core. Under the table it is "logged".
  - The masternode-probe rule is also missing: on a probe connection the first message must be `mnauth`, or the peer is disconnected (v24 np:4368-4379).
- **Handlers that catch the exception and score it, but are not marked:**
  - `filterload`: **any** decode failure is +100 (v24 np:5535-5542, v23 np:5458-5465). That includes truncation, not just "> 36,000 bytes".
  - `filteradd`: any decode failure is +100 (v24 np:5573, v23 np:5496). A `filteradd` with no filter loaded is a further +100.
  - Sig-share messages: a decode or count violation is +100 via `BanNode` at both tags (v23 `llmq/signing_shares.cpp:316-417`, v24 `llmq/net_signing.cpp:47-150`).
  - DKG at v23: a full structural decode of a copy, scored +100 (v23 `llmq/dkgsessionmgr.cpp:269-277`; verified).
- **A second trailing-bytes exception:** at v24, DKG messages must consume their whole payload, or +100 (`llmq/net_dkg.cpp:600-603`). The text names only `qgetdata`.
- **Fix:** add the blocks-only and probe gates to the gate list. Make the generic-rule sentence true by marking every catch-and-score handler. Add DKG v24 to the trailing-byte exceptions.

**M-3. DDE-SWEEP §4.2 rows that misstate Core.** These rows feed `dashd-p2p::policy` and the owners' `diff_p2p_*` seeds. Each one below was checked at both tags; the ones marked "verified" I re-read myself.

| Row (line) | Doc says | Core |
|---|---|---|
| DKG `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit` (122) | "v23.1.8: no such checks" | v23 has the same `MaxDKGMessageSize` +100 and a structure check +100 (`dkgsessionmgr.cpp:269-277`, verified). Also at both pins: +10 from a non-verified or non-MN peer before decode; an empty payload is +100 at v23 (`:198-200`, verified); a payload under 33 bytes is +100 at v24 (`net_dkg.cpp:531`); v24 scores an unrequested message +10. The cited lines "~583–599" are really 593–603 |
| `mnauth` (116) | "invalid: +100" | Only `IsBlockchainSynced` gates the read. A signature that throws (malleable in both schemes) is logged with no penalty. After decode: +100 for a duplicate, missing NETWORK or BLOOM services, a null proRegTxHash, or `!sig.IsValid()`; **+10** for an unknown MN and for a failed `VerifyInsecure` (v24 `evo/mnauth.cpp:57-101`, verified) |
| sig shares (123) | "counts capped at v24" | The same caps hold at v23 (`signing_shares.h:50,453-459`). These messages are read only on masternodes, and are +100 on violation at both pins |
| `qsigrec` (114) | — | An unknown llmqType is +100 right after decode, at both tags (v23 `llmq/net_signing.cpp:29-31`, v24 `:56-59`) |
| `qfcommit` (115) | — | v24 scores an unrequested commitment +10 after decode (`blockprocessor.cpp:111-119`). There are +100 paths at both pins |
| `qgetdata` (118), `qdata` (119) | — | A +10 gate runs before decode at both tags: we must be a masternode, and the peer must be verified or `qwatch` (v23 `quorumsman.cpp:420-422`/`:511-513`, v24 `net_quorum.cpp:80-83`/`:205-208`). `qdata` vector-count errors are +100 |
| `govsync` (95) | "decode failure +100" | Only the bloom-filter read is inside the try. A truncated `nProp` escapes with no penalty (`governance/net_governance.cpp:92-93`; v23 the same) |
| `dsi`, `dss` (126) | "inputs ≤ 180" | `dsi` caps both vectors at decode, which throws with no penalty. `dss` above the cap answers `ERR_MAXIMUM`, with no throw and no penalty (`coinjoin/server.cpp:259-263`) |
| `isdlock` (113) | "> 48,780 inputs: +100" | Checked after a full, unbounded decode. Write the order down |
| `sendtxrcncl` (109) | "after verack: disconnect" | Only with `-txreconciliation`; otherwise ignored entirely. Three further disconnects run before the read |
| `getcfilters` etc. (108) | "violations disconnect" | The payload is decoded **before** the NODE_COMPACT_FILTERS disconnect, so a malformed payload is logged and the peer stays connected |
| `filterload`/`filteradd` (105-106) | see M-2 | see M-2 |

**Fix:** correct each row and re-run the source check. Optionally add `dde-probe` cases for the cheap pre-decode gates (blocks-only, filterload truncation, short pong), as was done for `version` and `headers`.

**M-4. `Headers::decode` and `Headers2::decode` regress to an ~80× allocation amplification.**
- **Where:** base-sdk `pkgs/p2p_core/src/msg/headers.rs:101-104` with `:77`; `headers2.rs:281-284` with `:260`.
- **What changed:** at 33fcc59, `BaseCodec::decode` capped the count with `into_len(MAX_HEADERS = 2000)` before `Vec::with_capacity(count)`. Now the cap is `data.len()`.
- **Failure:** a 3 MiB `headers` payload starting `fe f8ff2f00` (count 3,145,720), followed by anything short, makes `Headers::decode` reserve about 250 MB (3.1 M × 80-byte `BlockHeader`) before the first read fails. The `Decode` trait path (`impl_p2p!`, 3 MiB limit) does the same. `headers2` is the same.
- **Exposure:** `decode_with_policy` is safe, because `read_header_count` bounds the count at 2000/8000 first. These are public upstream decoders, though, and other callers of base-sdk use them.
- **Fix:** bound the reservation. For example, use `into_len(data.len() / 81)` for `headers`, since each entry is at least 81 bytes, or batch allocation as `Vec<T>::decode` does with `MAX_VECTOR_ALLOCATE`. Do the same for `headers2`, and add a test.

**M-5. `just lint` fails: the CodeQL policy queries flag 11 BS4a sites.**
- **How I ran it:** `maint/lint/lint_codeql.py run --lang=rust --with-suite=rust-security-and-quality`, with CodeQL 2.27.2 (the copy BS11a-t downloaded, `/work/scratch/bs11at-bls-types/codeql-2.27.2`). It exits 0 at 33fcc59 and 1 on f8a088f. PLAN's acceptance criterion is "base-sdk `just test lint`", and base-sdk AGENTS.md says "All changes must pass".
- **The findings:**
  - `p2p_core/src/ctx.rs:13`: `ProtoCtx` has no serde derives and no codec coverage. It is not a wire type, so derive `Unencodable`.
  - `pkc/src/bls/scheme_ctx.rs:9`: `BlsCtx` has the same problem. BS11a-t's version already has `#[cfg_attr(feature = "codec", derive(Unencodable))]`, so copy that.
  - `p2p_core/src/msg/version.rs:314`: the private serde helper `Wrap` lacks the required derives and coverage.
  - Impl-order rule (BaseCodec, then Hashable, then inherent, then other traits):
    - `headers.rs:100,120` and `headers2.rs:280,295`: the new inherent `read` and the `MsgDecode` impl were inserted before `BaseCodec`/`hash_impl!`;
    - `version.rs:291`;
    - `types/src/core_bool.rs:32`.
  - `params/src/lib.rs:25`: `pub use dash_primitives::LlmqType` is a foreign-crate re-export, which the policy bans. Either have callers import it from `dash_primitives`, or add a justified exception to `maint/codeql/rust/lib/policy.qll`.
  - `primitives/src/support.rs:262`: the test module needs a blank line between `use super`/`crate` and the external imports.
- **Cause:** the author's environment had no CodeQL; the team memory said so, and it is now corrected. The fixes are mechanical.

### Minor

**m-1. `Transaction::check` now serializes the whole transaction to measure it** (`primitives/src/transaction.rs:262-266`, commit c955c4e).
- The cost is a full allocation and copy per tx check; `Block::check` already encodes each tx once for the merkle root.
- The test skips `check()` above 1 MB because "re-encoding in `check` is only slow".
- It is also out of BS4a's decode scope: `Checkable` is non-normative (A1 R5), and CS3 owns the rule.
- Fix: use a counting `EncodeBuf` (or BS13's `encoded_len`), or drop the commit from BS4a and record the ordering for CS3.

**m-2. `Version` makes impossible states representable** (`p2p_core/src/msg/version.rs:203-216,278-290`).
- Core reads a prefix chain, but the struct has seven independent `Option`s.
- `Version { user_agent: None, relay: Some(..), .. }` encodes silently without `relay`, so wire, hash and JSON disagree. `addr_send: Some, nonce: None` silently drops `addr_send`.
- This conflicts with base-sdk's "make invalid states unrepresentable".
- Fix: a nested tail (`Option<VersionTail { addr_send, nonce, rest: Option<…> }>`) or a group-count enum. If kept, at least `debug_assert` the prefix property in `encode`.

**m-3. Evidence claims are stronger than the tests.**
- DDE-SWEEP §1 says "Each BS4a fix has a unit test whose bytes Core produced or Core judged". Most do, and I found no test pinning a non-Core verdict.
- Some cases are derived only from source, or are mislabelled:
  - `types/src/compact.rs:213-218`, `above_max`: the bytes match the bounded-vector test from #7439 (`serialize_tests.cpp` ~433 v23 / ~481 v24), not the cited `compactsize` test. The `u32_max` and `u64` cases come from source.
  - `primitives/src/support.rs:302`: the above-max DynBitset case comes from source. DDE-SWEEP §3 "Core's dynbitset bytes" holds only at max.
  - `types/src/codec.rs:411` `vec_length_prefix_is_range_checked`, `core_bool.rs` `0x80`, and `p2p_core/tests/decode_domain.rs:217` (the 3 MiB bound) come from source.
  - `groups` in `p2p_core/corpus/decode_domain.json5` is a constant in `gen_corpus.py:9-10`, not a Core observation. getpeerinfo `subver` and `startingheight` could confirm groups 1–3.
  - The "captured" `senddsq` row reads only v23's capture (`probe_p2p.py:142-144`), although the comment says "both nodes". v24 also sends `01`; I checked.
- Fix: label each source-derived case as such, or add a Core run. Narrow the §1 sentence.

**m-4. The probe classifier is fragile** (`dde-probe/gen_corpus.py:27-35`, `probe_p2p.py:61-66`).
- `outcome()` greps the log window without filtering by peer id or command.
- "accept" ignores `synced` and `closed`.
- A "disconnect" label doesn't check that the socket closed.
- It was harmless in my re-run: every line named the probed peer and command, every message case synced, and both disconnect cases closed. But a future case could be mislabelled silently.
- Fix: filter lines by `peer=<id>`, require `synced and not closed` for "accept", and require `closed` for "disconnect".

**m-5. The evidence tooling is not reproducible as committed.**
- `dde-probe/oracle/Cargo.toml` and `bench/Cargo.toml` track `branch = "node/bsdk-bs4a-decode-domain"` over `file:///work/home/agent/...`, with no `Cargo.lock`. The branch will be deleted after merge.
- `dash-oracle-sys` is a path dependency on whatever is checked out. At its current head (54221217, ABI 4), it fails to link against the archives its own `pins.txt` names, because the O1b symbols are missing. It works at 4ff3d4c2.
- Fix: pin `rev = "f8a088f…"` (or the merged commit), commit lockfiles, and name the `dash-oracle-sys` commit in the README and DDE-SWEEP §6.

**m-6. The V2 bound is 12 bytes too strict for short IDs** (`p2p_core/src/bip324.rs:61-69` via `macros.rs:15`).
- Core's V2 contents limit is `1 + 12 + min(MAX_SIZE, 3 MiB)` (v24 `net.cpp:1297-1304`, v23 `:1303-1310`).
- A short-ID message can therefore carry up to 3 MiB + 12 payload bytes, and `decode_v2` rejects 3 MiB + 1 … + 12.
- Fix: since this belongs to framing, add it to DDE-SWEEP §5 under BS7, or apply the long-form overhead in `decode_v2`.

**m-7. Smaller gaps in DDE-SWEEP §2 and §4.1.**
- §2:37, "only addrv2 `nServices`" skips the range check: `qbsigs`' outer count also calls `ReadCompactSize(.., false)` (v24 `llmq/net_signing.cpp:30`, v23 `signing_shares.cpp:51`). The verdict is the same, because the count is compared with 400 at once.
- §2 omits two rules: VARINT (the sig-share `sessionId`) and the `COMPACTSIZE` type-range throw (`serialize.h:715-723`).
- `addrv2` (:90): add that a known BIP155 network with the wrong length throws (`netaddress.cpp:44-87`). base-sdk already matches this.
- `version` (:79): the outbound expected-services disconnect runs before the version check (v24 np:3963-3968). Also add the inbound self-nonce disconnect (np:4016-4021), the devnet name and feeler rules.
- `headers`/`headers2`: the `LoadingBlocks` gate runs before decode (v24 np:5312).
- Control rows: `sendaddrv2` after verack disconnects (np:4299) and is ignored below 70223. `mempool` is exempted by the Mempool permission and also disconnects at the outbound target (np:5429-5447). `qwatch` is ignored without a +10 when SPORK_17 is off.

**m-8. `version_reads_optional_fields_as_core` doesn't pin which error a "throw" case gets** (`p2p_core/tests/decode_domain.rs:79-86`).
- It accepts `VersionTail` or `Consensus` for every "throw" row.
- A regression that returns `Consensus` for a cut past `addr_recv` would lose the head, and with it Core's version and services disconnects. Only `obsolete_short_tail` would notice.
- Fix: assert `VersionTail` for cuts past byte 46 (the end of `addr_recv`) and `Consensus` for cuts inside the first 46 bytes.

### Nits

- `version.rs:297-306`: `decode_msg` clones the head on every success. Clone only in the `Err` arm.
- `transaction.rs:327-330` and `docs/samples/parser/parser.rs:31-35` repeat `len - consumed` and the "trailing bytes" text. Add a `PayloadDecode::trailing()`.
- `codec.rs:31-32`, the `MsgDecode` `# Errors` text: "where Core's handler throws or rejects the payload before acting on it" also describes `inv` above 50,000, which deliberately decodes `Ok`. Say "throws, or rejects before reading the rest", which is the `headers` count case.
- `ctx.rs:17-31`: `#[non_exhaustive]` plus a positional `new(version, bls)` means BS8's next field still breaks `new`. Consider `new(version)` with `with_bls(..)`. This affects the BS11a-t merge only cosmetically.
- The rdc adaptation for the conformance crate is on a separate branch (`node/bs4a-conformance-adapt`, f0cbb863). It doesn't build at the 33fcc59 pin and must land together with the re-pin. Mention it in the merge note.

## Focus questions

**Minimal and upstream-shaped (base-sdk CLAUDE.md/AGENTS.md).** Mostly yes, apart from M-4, M-5 (base-sdk's own CodeQL policy), m-1 and m-2.
- Each commit is one decode-domain change, with `!` on breaking ones and a passing commit lint.
- Comments are short, Latin-1, and cite Core file:line.
- Tests use `rstest`, and the corpora are JSON5 with provenance comments.
- `no_std` and the prelude rules are respected.
- The `primitives`/`p2p_core`/`params` CHANGELOGs are still "Initial release", so the missing entries there follow repo convention. `types` and `pkc` got entries.
- The `bool` codec removal is justified: no wire type may stay strict.
- The `LlmqType` unification is the minimal form of A1 #4.
- Dropping `impl_payload`/`MAX_SPTX_PAYLOAD_SIZE` is exactly row #3.
- Out of scope: c955c4e (m-1), which changes a non-normative rule.

**Is the evidence Core-produced?** For every verdict that matters, yes.
- I re-ran the full P2P probe, the payload probe and the oracle cases at both pins; all reproduce.
- Every Core test-vector byte string is exact.
- The exceptions are labelling and source-derived edge cases (m-3), plus tooling fragility and pinning (m-4, m-5).

**Does the policy table match `net_processing` at both pins?**
- **Matches:** the typed §4.1 rows other than `pong` (`version`, `headers`/`headers2` including the `GetCommonVersion` 70235 rule, `inv`/`getdata`/`notfound` 116 vs 50,000, `addr`, `getheaders`, `getmnlistd`, `govobj`); the exception sites (v23 :5795, v24 :5941); "Unknown command"; the version and verack gates; `MAX_PROTOCOL_MESSAGE_LENGTH`; and the 74 commands.
- **Doesn't match:** `pong` (M-1), the preamble's generic rule and gate list (M-2), and a dozen §4.2 rows (M-3).

**Will the `BlsCtx` placeholder merge cleanly with BS11a-t?** See below.

## BlsCtx and BS11a-t

The placeholder's whole API exists in BS11a-t with the same names, values and meaning:
- `BASIC` and `LEGACY`;
- `from_legacy(bool)`;
- `is_legacy(self)`.

BS11a-t's version adds `BlsSchemeKind`, `new(kind)`, `scheme()` and an `Unencodable` derive under `codec`. Both files sit at `pkgs/pkc/src/bls/scheme_ctx.rs`.

**What BS4a uses.** BS4a uses only `BlsCtx`, `BlsCtx::BASIC` and the derives (`p2p_core/src/ctx.rs:11-28`, `tests/decode_domain.rs:16,44`). All of them exist in BS11a-t, spelled the same.
- The derives match in both versions: `Clone, Copy, Debug, Eq, Hash, PartialEq`, and no `Default`.
- `mod scheme_ctx` sits outside the `bls` `cfg_if` in both, so the type needs no feature.
- BS4a changes no `Cargo.toml` or `Cargo.lock`. `dash-p2p-core` already depended on `dash-pkc` with `codec`, and the merged tree pulls neither `blst` nor `spin` into p2p_core.

**Trial merges.** I tried both orders on `node/base-sdk`, once independently and once by hand.
- The same three files conflict in both orders: `scheme_ctx.rs` (add/add), `bls/mod.rs` (the re-export line) and `pkc/CHANGELOG.md` (the Added entry). Nothing else overlaps.
- Taking BS11a-t's version of all three gives identical trees for both orders. BS11a-t's changelog line already covers `BlsCtx`.
- The merged tree builds with `--all-features`. clippy with `--exclude dash-pow` passes in both feature sets. nextest passes 1157 run (1 skipped), and doc tests, `cargo doc` and `bench --no-run` pass too.

**So the claim holds: the merge takes BS11a-t's file.**
- If the branches are combined by rebase instead of merge, drop 65f1d53 and the pkc line in 5b6d1f5.
- One standing gap: on its own, the placeholder lacks BS11a-t's `#[cfg_attr(feature = "codec", derive(Unencodable))]`, so CodeQL's codec-coverage rule flags it until the merge (see M-5).

# BS9a review r1 (Opus, high)

Scope: base-sdk `dbe245f..ac61b6c` (651ec35, bffc80b, 5fd1e89, ac61b6c) and rdc `c16104259..c808d4e37`
(28eece62f; c808d4e37 is the unrelated selftest `target/` fix riding the range). D-rows: `/work/scratch/bs9/notes.md`
D1–D11. Core pin v24.0 d4c16888b45a (`/work/scratch/bs9/core-src`). No branch was edited.

What I checked against the pin, line by line:

- `MaxDKGMessageSize` (net_dkg.cpp:43–75) against `DkgMessage::max_size` + `capped` (dkg.rs:157–163, 221–230, 295–297,
  368–370, 440–442). The formulas, `COMPACT`=5, `BLOB`=133, `SLACK`, the 1 MiB ceiling and the clamping of negative
  parameters to 0 (`param`) all match.
- The four `Check*WireStructure` walks (net_dkg.cpp:99–159) against `framing_fits` (dkg.rs:232–451). The order of
  steps, `ReadCompactSize`'s `MAX_SIZE` and non-canonical refusals (`Walk::count`), `ds.ignore` past the end
  (`Walk::ignore`), the `min_size..=size` blob bound, `bad == complain_for`, and `ds.empty()` (`walked`) all match.
  `ReadBoundedDynBitset`→`ReadFixedBitSet` refuses padding with `~(0xff >> rem)` (serialize.h:470–475). That is the
  same test as `bytes[last] >> (bits % 8) != 0` (dkg.rs:138), and the `nbytes > s.size()` pre-check is the `get(..len)?`.
- The worker read (`DeserializeCheckedDKGMessage`, net_dkg.cpp:370–389) against `decode_whole`. `CheckDKGMessageStructure`
  after the read is implied by a walk that passed, because the walk reads the same counts. Only `EnqueueOwn` skips the
  walk, and that path is not a decode.
- The layouts (dkgmessages.h) and `GetSignHash`. Contribution, complaint and justification hash the message with a
  default (all-zero) `CBLSSignature`, and `hash_without_sig` zeroes the last 96 bytes, which is right because `sig` is
  the last field in all three. `BuildCommitmentHash` (commitment.cpp:312) matches `DkgPrematureCommitment::sign_hash`.
- The sig-share family (net_signing.cpp:27–145, signing_shares.h:40–160, serialize.h:497–583, 1000–1033). `VARINT`
  (`read_varint_u32` reads the digit before the overflow test, as `ReadVarInt` does), `AUTOBITSET` reads in both forms
  (form byte 0/1, padding, `idx >= size`), `WriteAutoBitSet`'s `size1 < size2` tie rule, the `invSize > 400` throw, the
  `LIMITED_VECTOR` per batch, `UnserializeBatchedSigShares`' batch count (range check off, which gives the same verdict
  because 400 ≪ `MAX_SIZE`) and its running total, which it checks after each batch is fully read. All match.
  The lazy shares (`CBLSLazyWrapper::Unserialize`, bls.h:467) read 96 bytes with no validation and write the same bytes
  back, so `BlsSigBytes` is exact under both schemes.
- Evidence: `/work/scratch/bs9/cases.out` has 1,440 cases, every one `match`, 0 reencodings. The nine BS9a fuzz legs
  (`/work/scratch/bs9/fuzz/*.log`, 1,200 s each, 2.9M–35M runs) have 0 mismatches and 0 reencodings. The logs postdate
  the ac61b6c commit. I assume they ran at that head, but I did not rebuild to confirm.

## D-row verdicts

- **D1 (BLS as wire bytes, eager under both schemes): agree.** I confirmed `CBLSWrapper::Unserialize` (bls.h:184–200):
  bytes that are invalid under both schemes throw "malleable BLS object"; bytes valid only under the other scheme are
  read and then `Reset()`, so they write back as zeros; all-zero bytes read and write back as zeros. `predict.rs::dkg`
  applies exactly that rule through `BlsEager*`, and the differential exercises it per field under both schemes. Gaps
  in the node-facing API are under M2 and M3.
- **D2 (params checks outside the stateless decode): agree with the split.** The node gets `dkg_quorum` (Core's 33-byte
  peek; `LlmqType` has an `Unknown` arm, so any type byte reads), `check_payload` (cap, then walk), and the whole read.
  Two gaps: the stateless dispatch reads `qcontrib` with no size bound (M1), and the variant docs point the node at a
  helper that merges two steps Core keeps apart (M2).
- **D3 (qjustify share as `[u8; 32]` with hex serde): agree.** Core reads `CBLSSecretKey` eagerly. The corpus shows that
  `r+1` and `2r-1` throw while `0`, `1` and `r` read, and that the node's `BlsEagerSk` reproduces this. The share goes to
  the relay members and to any `qwatch` peer (`RelayInvToParticipants`, net_dkg.cpp:340–343), so it is public on the
  wire.
- **D4 (CodeQL exemptions): agree.** The names are exact matches, each with a reason in policy.qll. Nit N1 covers a
  name the notes get wrong.
- **D5 (judge `#include`s net_dkg.cpp): it does exercise Core's code.** The anonymous-namespace `MaxDKGMessageSize`
  and `Check*WireStructure` compile into the judge from the pinned tree (`-I"$src"`, the archive of the pin). The link
  line still carries `libbitcoin_node.a`. Its `net_dkg.o` defines the same strong symbols (`CheckDKGMessageWireStructure`,
  `NetDKG::*`), so if it were pulled the link would fail on duplicates. The link succeeds, so the judge's copy is the
  only one. Each command's `cap +1` corpus case returns "oversized" and `cap +0` reaches the walk, which shows the
  judge's command strings select Core's real branches (a mismatched string would return `HARD_CEILING`, and every walk
  would fail). The one caveat: the walk is compiled with the judge's `-O1`, not dashd's flags. The source is the same,
  so I accept it. Nit N2 covers one more point.
- **D6 (no root re-pin on the BS9a branch): agree**, as with BS8/BL-55.
- **D7 (count bounds with zero shares): agree.** Lazy shares never inspect their bytes, so zero shares test the counts
  exactly. The corpus keeps `qbsigs` 1×400/1×401, 2×200/2×201, 200+201, 400×1, 401×0, `qsigshare` 32/33, `qsigsesann`
  100/101 and `qsigsinv` 200/201, and the `sig tors_*` cases cover the share bytes under both schemes.
- **D8 (keep them parsed + `decode_checked`): agree with keeping them parsed.** The docs need the change in M2.
- **D9 (CodeQL baseline dropped): agree.**
- **D10: agree.**
- **D11 (corpus grouping + RLE): agree.** The grouping key keeps verdict, class and write-back-differs. `check()` fails
  the build if any (command, tag group, scheme, class) or any cap edge is lost. The RLE (runs of 64 or more, payloads
  over 4 KiB) round-trips through `Raw::bytes`. 475 of 1,440 cases are kept, and every sample stays in the differential.

## Findings

### M1 — Minor: `P2pMsg::decode_with_policy` reads `qcontrib` with no DKG size bound, ~24× allocation amplification
- Where: base-sdk `pkgs/p2p_core/src/msg/mod.rs:164` (the `QContrib` parsed variant), which reaches
  `pkgs/pkc/src/bls/ies_bytes.rs:183–190` (`BlsIesMultiBytes::decode`).
- Scenario: a peer sends a 3 MiB `qcontrib` (V1 cap) with ~3.1M zero-length blobs, one byte each. The generic dispatch
  allocates one `Vec<u8>` header (24 B) per blob, about 75 MB plus growth slack, before anything checks the quorum's
  bound. Core never reaches its typed read for that payload, because `MaxDKGMessageSize` (at most 1 MiB, and
  size×133 + … for real quorums) refuses it first. D8 relies on the node calling `check_payload` first, but nothing
  enforces that on the generic path. A node component that logs, relays or replays through `decode_with_policy`
  inherits the amplification.
- Fix: in `dispatch`, refuse a payload over Core's parameter-free `HARD_CEILING` (1 MiB; `MaxDKGMessageSize` never
  exceeds it) for the four DKG commands with `HandlerPayloadTooLarge`, before the read. Core refuses every such payload
  at the same stage and score, so this adds a bound without changing a verdict. Documenting the hazard on the variant
  is the weaker fallback.

### M2 — Minor: the variant docs steer the node to `decode_checked`, which merges two steps Core keeps apart (D2/D8)
- Where: base-sdk `pkgs/p2p_core/src/msg/mod.rs:162–173` ("Gate and check it first, as Core does
  ([`DkgMessage::decode_checked`])"); `pkgs/p2p_core/src/msg/dkg.rs:10–16, 55–92`.
- Scenario: Core does not read at intake. After the walk it hashes the raw payload, applies the GETDATA-response gate
  (an unrequested message scores `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE` = 10, msg_result.h:21), queues the bytes, and
  reads them later on the phase worker (a failed read scores 100). A node that follows the docs and calls
  `decode_checked` at intake scores an unrequested, framed but unreadable payload (e.g. one with a malleable BLS field)
  100 where Core scores 10. It also reads payloads Core drops unread: unrequested ones, those with no handler, and
  those whose phase has passed.
- Fix: document the order on `check_payload` and in the module: `check_payload` → inventory hash = sha256d of the raw
  payload → unrequested gate → queue → `MsgDecode::decode_msg` on the worker. Say that `decode_checked` is for
  single-step use such as tests. Optionally expose `dkg_inventory_hash(payload)`, since `dsha256` is `pub(crate)` and
  `m.hash()` equals the raw payload's hash only after a successful read.

### M3 — Minor: the BLS normalization that `sign_hash` depends on lives only in the contrib tool (D1)
- Where: base-sdk `pkgs/p2p_core/src/msg/dkg.rs:18–21, 209–213, 421–432`; rdc
  `contrib/node/dde-probe/bs9/src/predict.rs:59–140`.
- Scenario: `sign_hash()` hashes the BLS fields as held. Core hashes them after its eager read, which zeroes a field
  that is valid only under the other scheme. If a `vvec` key or the `quorumPublicKey` is in the other scheme, a node
  that calls `m.sign_hash()` directly and verifies the operator signature against it can accept a message whose
  signature Core's batch verify rejects (Core scores that 100). The module docs warn about this, but the only code
  that applies the rule is `predict.rs::dkg`, which the node will not link. The differential therefore checks a
  normalization the node has to write again separately.
- Fix: move the normalization into base-sdk behind `bls`, e.g. `DkgMessage::eager(&self, BlsSchemeKind) ->
  Result<Self, Malleable>`, returning the message as Core holds it, or the read failure. Have `predict.rs` call it,
  so the judge checks the code the node will run.

### M4 — Minor: the base-sdk corpus test never checks a write-back that differs from the bytes read
- Where: base-sdk `pkgs/p2p_core/tests/core_overlay_ii.rs:183–190`; the corpus header
  `pkgs/p2p_core/corpus/core_overlay_ii.json5:7–9` and rdc `contrib/node/dde-probe/bs9/make_corpus.py:31–32`.
- Scenario: 49 corpus cases carry `reenc`. 38 are `qsigsinv`/`qgetsigs`, where Core rewrites an `AUTOBITSET` in its
  shorter form. That is not a BLS rule, and base-sdk claims to reproduce it exactly (CHANGELOG, `sigshare.rs:186–188`).
  The test skips both the write-back and the sign-hash assertions whenever `reenc` is present, so base-sdk's CI never
  checks `write_auto_bitset` against Core. Only the hand-written unit test (`overlay_ii.rs:282`) and the rdc
  differential cover it. The header also says `reenc` means "an eager BLS field re-serialized", which is wrong for
  those 38 cases.
- Fix: for non-DKG commands, assert `r.reenc == vec_from_hex(reenc)` when `reenc` is present (and the sign hash
  unconditionally). Change the header wording to "an eager BLS field re-serialized, or an AUTOBITSET rewritten in its
  shorter form".

### N1 — Nit: notes D4 lists `DkgShare`; the code exempts `DkgRevealedShare`
- `/work/scratch/bs9/notes.md` D3/D4 vs `maint/codeql/rust/lib/policy.qll:79`. Update the notes to the shipped name.

### N2 — Nit: the judge mirrors `DeserializeCheckedDKGMessage` instead of calling it
- rdc `contrib/node/dde-probe/bs9-judge/judge.cpp:97–101`. The template is in the judge's translation unit (it sits in
  net_dkg.cpp's anonymous namespace). The copy is equivalent today and keeps the exception text that the corpus
  classes need. Add a comment citing net_dkg.cpp:370–389, so that a pin bump that changes the worker's checks is
  noticed.

### N3 — Nit: `make_corpus.py` says it keeps "a padding refusal of each bitset", but for DKG it accepts any framing verdict
- rdc `contrib/node/dde-probe/bs9/make_corpus.py:90–93`. For `qcomplaint`/`qpcommit`, any `framing` case passes. The
  per-tag coverage check already keeps the padding tags, so behaviour is fine. Reword the comment, or key the DKG check
  on a padding tag.

### N4 — Nit: an absolute `file:///work/home/agent/...` git dependency in `bs9/Cargo.toml`
- rdc `contrib/node/dde-probe/bs9/Cargo.toml:17–21`. Same as bs8 (D5), so it is consistent. It only builds on
  agentbox-like layouts; mention this in the README's BS9 section.

Out of scope, noted only: c808d4e37 (selftest `target/` canary) is a separate fix that rides this range. It looks
correct (`git ls-files -oi` limited to `*Cargo.lock`, excluding `target/`; the canary fails loudly), but it was not
reviewed as part of BS9a.

No Dash Core security-sensitive observations arose from Part 1.

VERDICT: APPROVE-WITH-NITS

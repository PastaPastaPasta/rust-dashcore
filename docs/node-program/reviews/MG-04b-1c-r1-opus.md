# MG-04b slice 1c, review r1 (Opus)

Branch `dw/mg-04b-1c`, head 3224891e: 10 first-parent commits on desktop main 67da2b55, all signed (`%G?` = G).
The follow-merge 3244d0bc has 67da2b55 as its second parent. Read-only review. No build: `/work` had 226 GB free,
under the brief's 230 GB floor (see "What I ran").

## Verdict: ACCEPT-WITH-NITS

Turning `SHARED_SENDING` on is safe at this head. 1c changes no sending logic. Outside tests and docs it touches
only the switch, the service reader and its three callers, `service_read`, and the list's net-info entries. The
1b sending paths, which reviews r1 and r2 of 1b checked through the override, now run through the shipped facade
in the engine tests. D39 matches Core for every entry type the wallet can put in a payload. D78's reader matches
rc.3 and glibc on every rule I traced. It fails closed whatever the coordinator's libc is. The M9 oracle is a real
cross-implementation check: it links rc.3's `mnsharesession.cpp` unmodified. The 1b r2 nits are closed.

The five findings are all nits: three test gaps where a cheap mutant on a sending gate survives (n-1, n-2), one
narrow parity gap in a contrived case that Core catches at the hand-off (n-3), and two gaps in evidence or
coverage (n-4, n-5). None of them can move funds wrongly.

0 M, 0 m, 5 n.

## Findings

### n-1: two cheap mutants of `parse_core_service` survive (the frozen check's reader)

- **Where:** `rust/crates/dw-protx/src/shared/netinfo.rs`. Mutant A is in `parse_u16` (the all-digits test,
  `:144`). Mutant B is in `inet_aton_exact` (`bytes.iter().any(|&b| b > 0xff)`, `:169`). Their
  tests are `test_QT_126_services_parse_as_core_reads_them` (`:830-880`) and the corpus in
  `testdata/oracle/dashqt/gen_envelopes.py:381-419`.
- **Mutant A:** drop `|| !digits.bytes().all(|b| b.is_ascii_digit())`.
  - For `127.0.0.1:++19901`, one `+` is stripped. Rust's `u16::from_str` then accepts the remaining `+19901`,
    so the wallet reads 127.0.0.1:19901.
  - Core reads it differently. `ParseIntegral` strips one `+`, and `from_chars` refuses the second
    (`util/strencodings.cpp:234-270`). The port then stays in the host, `+` fails `SAFE_CHARS_IPV4_6`, and so
    does `:` under the RFC 1035 filter, so the entry is `BadInput` (`evo/netinfo.cpp:512-539`).
  - No test has a doubled `+`. `127.0.0.1:+-1` dies on the `-` either way.
- **Mutant B:** drop the per-part byte bound.
  - For `383.0.0.1:19901`, `383 << 24` wraps to `0x7f000000` (Rust checks only the shift amount), so the wallet
    reads 127.0.0.1:19901.
  - glibc's `inet_aton_end` refuses it: it requires `val > 0xff` to be false for every part before the last.
  - Every "past 255" case in the tests (`127.0.0.256`, `127.16777216`, `127.256`) is a last part.
- **Effect if either regressed:** a participant's frozen check (`net_info_matches`) would accept a
  `coreP2PAddrs` that dash-qt refuses, for a payload holding 127.0.0.1. The reading is still the payload, so
  consent shows what is registered. The loss is parity with dash-qt (D78), not funds. Today the code is right.
- **Fix:** add `127.0.0.1:++19901`, `383.0.0.1:19901` and `383.1:19901` to the `None` list in
  `test_QT_126_services_parse_as_core_reads_them`. Better still, add them to the corpus so dash-qt gives the
  verdict.

### n-2: no test reaches `listed_net_info`'s IPv6 arm

- **Where:** `rust/crates/dw-protx/src/rules.rs:96`, `Bip155Network::Ipv6 => BIP155_IPV6`.
- **What:**
  - Every listed version-3 entry in the tests is IPv4, TorV3 or a domain: `rules.rs:707-791`,
    `shared/rules.rs:1821-1859` and `protx/tests.rs:1111-1118`.
  - The only IPv6 that a listed entry keys is the legacy IPv4-mapped one, which takes `legacy_entry`.
  - So the mutant `Bip155Network::Ipv6 => BIP155_CJDNS` (or `BIP155_IPV4`) survives all 17 gated mutants'
    suites.
- **Scenario under the mutant:**
  - The list holds a version-3 masternode whose second Core P2P entry is `[2a01:4f8::1]:9999` (BIP155 IPv6).
  - A participant joins a dash-qt session with `coreP2PAddrs` `[2a01:4f8::1]:9999` (D5 accepts IPv6 from a
    dash-qt coordinator).
  - `check_registration` passes. Core refuses it with `bad-protx-dup-netinfo-entry` at the hand-off, and the
    parties abandon (D72), paying a relay fee.
- **Fix:** in `shared/rules.rs`'s D39 block, add a listed entry with an IPv6 Core P2P service equal to an IPv6
  payload entry, expecting `Err("bad-protx-dup-netinfo-entry")`.

### n-3: D39 says services and domains are keyed "in their own namespaces"; Core's key is untagged

- **Where:**
  - D39 in `docs/design/MG-04b-shared-masternodes.md:1040`;
  - `rust/crates/dw-protx/src/rules.rs:81-112` (`listed_net_info`) and `:134-148` (`check_net_info_unique`).
  - Core: rc.3 `evo/deterministicmns.h:526-547` (`GetUniquePropertyHash`, a plain `SerializeHash`) and
    `evo/netinfo.h:176-180` (a `DomainPort` serializes as a compact-size string, then a big-endian port).
- **What:**
  - An ADDRv1 CService is its 16 address bytes and then the port, 18 bytes.
  - A 15-character `DomainPort` is `0x0f`, the 15 bytes, then the port, also 18 bytes.
  - Both go through the same `SerializeHash`, so when the bytes are equal Core holds them as one unique property.
  - The wallet compares a typed `NetInfoEntry::Service` against a `NetInfoEntry::Domain` and never finds them
    equal.
- **Scenario:**
  - An evonode lists a Platform HTTPS domain `abcdefghij.klmn:19999`.
  - A dash-qt coordinator's session has `coreP2PAddrs` `[f61:6263:6465:6667:6869:6a2e:6b6c:6d6e]:19999`. That
    address is routable under `IsRoutable`, so the participant's D5 check and §5 pass it.
  - Core refuses the registration with `bad-protx-dup-netinfo-entry` (`specialtxman.cpp:1235-1240`). The parties
    abandon (D72) and pay a relay fee.
  - Only a coordinator who picks such an address can hit this, and no funds are at risk.
- **Fix:** either key every entry by its Core serialization (ADDRv1 for IPv4 and IPv6, ADDRv2 otherwise, a
  domain as Core serializes it) in one namespace, or keep the code and correct D39's sentence, naming this case
  as a known gap that Core catches at the hand-off.

### n-4: M9 proves the text and `fromJson`, not the wallet's own transitions or dash-qt's merge rules

- **Where:** `rust/crates/dw-protx/tests/oracle_envelopes.rs:134-183` and `gen_envelopes.py`'s corpus.
- **What:**
  - **Transitions:** the stage replay calls wallet code only for `rebuild_funding_tx` and `add_signature`.
    Freeze, combine, funding-signed and broadcast are set by hand (`session.stage = …`, `session.protx = …`)
    and only then compared with dash-qt's text. The engine's own `build::freeze`, `build::combine` and the
    Broadcast transition in `send.rs:146` are never compared with dash-qt's output. By reading, they do match
    dash-qt: `freeze` keeps the revision and clears `sigs`, and `combine` sets only `protx` and the stage.
  - **Merge rules:** the oracle doesn't drive `mergeEnvelope`, `absorbDraftReply`, `adoptLockedTerms` or
    `replaceProTx` (`mnsharesession.cpp:1169-1497`), which are the checks dash-qt applies when it imports a reply.
  - **Frozen-stage corpus:** it changes the change amount and removes the change, but never changes the change
    address. It changes an input's sequence and txid case, but never its vout. `payloadMatchesEnvelope` checks
    all four (`:1060-1083`).
- **Why a nit:** M9 as ruled (`toJson`'s order, `fromJson`'s checks, `check_registration` against dashd's
  mutated join signatures) is met. These are the parts of `MnShareSession` that are still proven by
  inspection.
- **Fix (follow-up):**
  - have the replay call `build::freeze`/`combine` on the wallet's session (with dashd's protx substituted);
  - add the change-address and vout texts to the corpus;
  - list merge-rule oracle cases as a follow-up in §13.

### n-5: the glibc cross-check behind D78's "glibc's reading" isn't in the tree

- **Where:** D78 (`docs/design/MG-04b-shared-masternodes.md:1079`) and `netinfo.rs:87-101`.
- **What:**
  - DEC-740 reports a validator cross-check of `parse_core_service` against glibc `getaddrinfo` (300,068
    inputs, 0 mismatches). No harness, generator or vectors for it are committed. The implementer's scratch
    (`/work/scratch/dw-mg-04b-1c`) has none either.
  - What the tree proves is dash-qt's verdict on 39 forms and the unit list.
  - I checked the port against glibc's `inet_aton_end`/`__inet_aton_exact` and `inet_pton6` by reading, and it
    agrees on every case I traced (list below). n-1's two forms are exactly the kind a committed differential
    would have pinned.
- **Fix:** commit the differential under `testdata/oracle/` (a short C driver over `getaddrinfo(AI_NUMERICHOST)`
  plus its input generator, with the vectors it wrote, as `dashqt/` does), have a test read the vectors, and
  cite it in D78.

## Focus 1: is sending safe at this head?

Flows present at this head: create with 2–8 shares (`start/join/lock/approve/sign_shared_funding/send_shared_registration`),
`abandon_shared_session`, `update_share` (the reward change) and `resend_operation` of an `update_share` row.
Dissolve, standby and key rotation are slice 2 and absent here (no facade calls).

- **The switch:**
  - `SHARED_SENDING = true` (`shared.rs:70`).
  - `mod.rs:65` seeds `shared_send` from it.
  - In a non-test build, `shared_can_send` (`shared.rs:140-149`) returns the const.
  - The override is compiled only under `test` or a debug `test-support` build. The release check runs in the
    nightly, which closes 1b r2's n-3.
- **Terms other than those approved:**
  - `sign_shared_funding` (`shared.rs:666-704`) and `send_shared_registration` (`send.rs:106-108`) re-run
    `consent_checks` against a fresh `protx_context`: `check_frozen` → `payload_matches_envelope` →
    `net_info_matches`, then `check_registration` against the live list, then the join signatures.
  - Both then require the stored approval's consent hash to equal the frozen registration's.
  - The consent hash covers the payload, net info included, so the bytes signed are the bytes approved.
  - 1c touches none of this.
- **Coordinator strings reaching the payload in another form:**
  - The service is read by one function, `parse_core_service`, for D5 (`participant_services_ok`), the
    placeholder (`net_info_of`), the frozen check (`net_info_matches`) and the view (`service_read`). So what is
    shown, what is checked and what the payload holds are the same reading.
  - Payout scripts and addresses are unchanged from 1b.
- **Collateral, holds, fees:**
  - 1c doesn't change `holds`, `reserved_inputs`, `send.rs`, `update.rs`, `flow.rs` or the follower, per the
    diff stat.
  - The fence and reservation re-checks that 1b r1 and r2 traced stand.
  - `SharedSession.debit` sizes the grant as before.
- **Frozen-check bypass:** none. Every signing call reaches `check_frozen` before it signs (`consent_checks`),
  and approval re-checks D5 (`shared.rs:649`).
- **Tests on the shipped path:**
  - The engine `Fx::mn()` is now the shipped facade, and `sending_off()` covers the refusals.
  - `test_QT_127_update_share_sends_an_operation` resends for real once the switch is on (`tests.rs:2733-2740`).

## Focus 2: D39 against rc.3

- **Core:**
  - `AddMN` puts every `GetEntries()` entry, service or domain, into the unique-property map
    (`deterministicmns.cpp:459-482`).
  - `CheckProRegTx` refuses any payload entry that the map holds for another collateral
    (`specialtxman.cpp:1235-1245`).
  - The key is `SerializeHash` of the CService, ADDRv1 for v1-compatible networks
    (`deterministicmns.h:526-547`), so IPv4 and ::ffff:IPv4 are one key and a legacy fc00::/8 is IPv6.
- **The wallet:**
  - `listed_net_info` takes every purpose and every entry, domains included, and canonicalizes a legacy mapped
    address. `check_net_info_unique` loops over every payload entry (`all_entries()`).
  - Both `check_registration` (v3) and `check_registration_list` (v2) use it. M11–M14 pin the "all entries"
    and "all purposes" parts.
- **Where the list comes from:**
  - The live path (`read_list` → `masternode_list_summaries` → `MasternodeListSummary::from_entry`, platform
    dc0c946307 `list.rs:100-101`) always carries `net_info: Some`.
  - The `(None, Some(service))` fallback in `protx/build.rs:462-466` is reached only by test seams. Tracked
    snapshots never feed `listed()`.
- **The pin move 35e96ca16b → dc0c946307:**
  - It is two commits: `MasternodeListSummary.net_info` and a test fix.
  - `net_info` isn't persisted, and no equality on summaries changes behaviour.
- **Gaps:** n-2 (test) and n-3 (the untagged key). No entry type Core compares is skipped for the payloads the
  wallet can hold (IPv4/IPv6 Core P2P). TorV3, I2P and CJDNS list entries can't equal an IP payload entry.

## Focus 3: D78

- **The port, against rc.3:**
  - `SplitHostPort` (`strencodings.cpp:101-126`): last colon, the bracketed or single-colon rule, port only on
    `ParseUInt16`, one pair of brackets stripped. `ParseUInt16` takes one `+` and no `+-`.
  - `AddEntry`'s `SAFE_CHARS_IPV4_6` filter on the host (`netinfo.cpp:512`).
  - `Lookup`'s second split, keeping the first port as default (`netbase.cpp:194-209`).
  - `LookupIntern`: `SetSpecial` can't pass the filter, and internal results are dropped (`:147-174`).
  - `WrappedGetAddrInfo` with `AI_NUMERICHOST` (`:44-59`).
  - All ported faithfully.
- **The port, against glibc:**
  - `inet_aton_exact`: each part starts with a digit; `strtoul` base 0 (a leading 0 is octal); every part but
    the last ≤ 0xff; the last bounded by `max[parts]`; nothing trailing.
  - IPv6 is checked by reading against `inet_pton6`: at most 4 hex digits per group, one `::`, an embedded IPv4
    only last and strict (no leading zeros), no 9th group. Rust's `Ipv6Addr` parser agrees on each of those.
  - `SetLegacyIPv6` maps an IPv4-mapped address to IPv4. A TorV2- or internal-prefixed result is refused by
    `from_socket_addr`, as Core makes it invalid or drops it.
  - Gaps: n-1 and n-5.
- **Fail-closed:**
  - The wallet's reader is pure Rust, so it is glibc's on every desktop OS.
  - When a coordinator's libc reads a form differently, there are three cases, and each fails closed:
    - glibc refuses the form: D5 refuses the session at join or approve.
    - Both read it, as different addresses: the payload doesn't match and the frozen check refuses.
    - The other libc refuses it: no payload is ever built.
  - The coordinator's own input stays strict (`coordinator_services` → `parse_entry`, IPv4 only), and every
    reader takes that form.
- **`service` vs `service_read`:**
  - The approval binds the consent hash, not `ShareTermsView`. `sameTerms` merges compare `Terms.core_p2p_addrs`
    exactly.
  - `service_read` is display only: built from the same reader, `None` when every entry reads as written.
  - The payload is written from `net_info_of`, the same reading.
  - No UI in this repo reads it yet. dwcli prints it through serde.

## Focus 4: M9

- **A real cross-implementation check:**
  - `mnshare_oracle.cpp` links rc.3's `qt/mnsharesession.cpp`, regtest params, unmodified.
  - The stage texts and fingerprints, and the 220 `fromJson` verdicts with their read-back, come from it.
  - The Rust test compares `to_text`, `message_code` and `from_json` with them byte for byte. Each
    `wallet_refuses` row names its D28 reason.
- **Coverage:**
  - It covers what `fromJson` and `payloadMatchesEnvelope` check: types and ranges, stages, consent hash,
    collateral index, shares, terms, contributions, the 39 service forms, signatures at combined, unsigned
    inputs at funding-signed.
  - Gaps: n-4.

## Focus 5: contract v13 and old approvals

- `Approved.terms` is `ShareTermsView`, and serde derive reads a missing `Option` field as `None`. The test at
  `shared/tests.rs:644-650` reads a pre-v13 row.
- A row that fails to parse (`store.rs:305-308`, `.ok()`) loses its approval. Signing then refuses on the consent
  hash, so nothing is silently re-approved.
- An old binary reading a v13 row ignores the new field (no `deny_unknown_fields`).
- No 1b approval can hold a non-plain form. 1b's `SocketAddr` reader would have keyed `::ffff:` as IPv6 and
  failed the frozen check against dash-qt's IPv4 payload.

## Focus 6: tests, mutants, 1b r2 nits

- **The 17 gated mutants:** `/work/scratch/dw-mg-04b-1c/logs/mutants.log` records baseline pass and M1–M17
  killed, run at 12:45–12:49 after the head commit (12:27). Surviving cheap mutants: n-1 (two) and n-2 (one).
- **1b r2 n-1:** closed. The contract's `resend_operation` row and D71 now say where the `update_share` resend
  refuses.
- **1b r2 n-2:** closed by the alternative fix. The dwcli `update-share` assertion is gone, and the switch is
  covered by `test_QT_126_sending_is_on_as_shipped_and_the_switch_refuses`.
- **1b r2 n-3:** closed. The nightly `release-check` job runs the release leg, and D77 records `copy_rank`'s
  evidence, with the list-based pick as a follow-up.

## What I ran

- **Read:**
  - the 1c diff (`git diff 67da2b55 3224891e`, 33 files) and the first-parent log;
  - rc.3's `evo/netinfo.cpp`/`.h`, `netbase.cpp`, `util/strencodings.cpp`, `evo/deterministicmns.cpp`/`.h`,
    `evo/specialtxman.cpp` and `qt/mnsharesession.cpp` (via `git show v24.0.0-rc.3:…`);
  - the platform pin diff `35e96ca16..dc0c94630` in the cargo checkout;
  - the oracle driver, the build script, the README and the corpus definition;
  - 1b r2's review and DEC-740.
- **Not built or run:** `df -BG /work` read 226 GB at my build check, under the brief's 230 GB floor, so I made
  no reflink copy and ran no cargo command.
- **The claims, checked against the implementer's logs at head:**
  - `ready-tier.log` (fmt, clippy, workspace tests 12:58–13:24, after the 12:27 head commit): 2,560 passed,
    0 failed;
  - `ready-release.log`: 3 passed;
  - `mutants.log`: 17/17 killed.
  - The 300,068-input glibc cross-check isn't reproducible from the tree (n-5).
- The mutant survivals in n-1 and n-2 are by reading (no test input reaches them), not by a run.

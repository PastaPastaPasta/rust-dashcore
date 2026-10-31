# MG-04b slice 1c — review r2 (Opus, narrow: the r1 fix round)

- Repo: dashwallet-desktop, branch `dw/mg-04b-1c`
- Diff: `3224891e..98f9d00c` (one commit, `98f9d00c`, signature `G`)
- Ruling under check: DEC-743
- Core reference: Dash Core `v24.0.0-rc.3` (`~/workspace/dash`, read with `git show`)

## Verdict: ACCEPT-WITH-NITS (0 M / 0 m / 1 n)

Every DEC-743 item is closed. Nothing in the fix round can move funds wrongly. The one nit is in the new glibc harness: about 1.7% of its entries are answered by CPython's IDNA codec, not by glibc. The committed tally still holds when those entries go to glibc.

## Findings

### n-1 — `testdata/oracle/glibc/core_read.py:70-71`: 5,142 entries never reach glibc

`lookup_intern` passes the host to `socket.getaddrinfo` as a `str`. CPython encodes a `str` host with the `idna` codec before calling glibc. That codec raises `UnicodeError` for two kinds of host: one with an empty label before the last dot, and one with a label of 64 or more characters. The harness catches the error and answers `-`, so glibc is never asked.

- With the default seed, 5,142 of the 300,074 entries end this way (counted by running the harness's own `split_host_port` over `EDGES + generated(20261029, 300000)`).
- None of them can hide a reader bug in this run. With the host passed as bytes, the tally is unchanged: 300,074 entries, 14,894 read, 285,180 refused, 0 mismatches.
- The blind spot does reach forms glibc accepts. These are IPv4 parts with many leading zeros:

  | Entry | Harness | Wallet | glibc (bytes host) |
  |---|---|---|---|
  | `"0"*63 + "1.2.3.4:9999"` | `-` | `1:01020304:9999` | 1.2.3.4 |
  | `"0"*70 + "1:9999"` | `-` | `1:00000001:9999` | 0.0.0.1 |
  | `"1.2.3." + "0"*64 + "4:9999"` | `-` | `1:01020304:9999` | 1.2.3.4 |

  On these the wallet agrees with glibc, and the harness reports a false mismatch. The generator never builds such parts (its longest `DEC_PARTS` piece is 10 characters). So this is a fidelity gap in the oracle, not in the reader.
- The README says entries are compared over "the real `getaddrinfo`". For these entries that isn't true.
- **Fix:** pass `name.encode()` to `getaddrinfo`; bytes skip the IDNA codec. Then drop `UnicodeError` from the `except`. Add one long-leading-zero entry to `EDGES`, for example `"0"*63 + "1.2.3.4:9999"`.

No other findings.

## What I checked, per DEC-743 item

1. **Opus n-1 (refused forms) — closed.**
   - `parse_u16` (`netinfo.rs:143-149`) strips one `+`, then requires digits only. That matches rc.3 `ParseIntegral` (`util/strencodings.cpp:234-250`): refuse `+-`, strip one `+`, then `ToIntegral` over `from_chars`.
   - `inet_aton_exact` (`:155-178`) bounds every part but the last by `0xff`.
   - `127.0.0.1:++19901`, `383.0.0.1:19901` and `383.1:19901` are asserted `None` (`netinfo.rs:918-931`).
   - **Ran M18** (the all-digits check in `parse_u16` dropped). Killed by `test_QT_126_services_parse_as_core_reads_them` at the `127.0.0.1:++19901` assertion: Rust's `u16::from_str` takes a leading `+`.
   - **Ran M19** (`b > 0xff` dropped). Killed by the same test: `383 << 24` silently drops the high bits and gives 127.
2. **Opus n-2 (listed IPv6 entry) — closed.**
   - `every_listed_net_info_entry_is_unique` (`rules.rs:794-816`) now lists a version-3 Core P2P entry `2a01:4f8::1` and asserts it equals the wallet's IPv6 entry.
   - **Ran M20** (`Bip155Network::Ipv6 => BIP155_CJDNS` in `listed_net_info`). Killed at `rules.rs:812`: network 6 against network 2.
3. **Opus n-3 (Core's untagged dup key) — closed. `NetInfoEntry::unique_property` (`netinfo.rs:682-720`) is byte-exact with Core for every entry the wallet can hold.**
   - **How Core keys it.** `GetUniquePropertyHash` (rc.3 `evo/deterministicmns.h:524-547`) runs `SerializeHash(v, SER_GETHASH, PROTOCOL_VERSION)`. It sets `ADDRV2_FORMAT` only for a `CService` that is `!IsAddrV1Compatible()`; per `netaddress.cpp:483-497` those are ONION, I2P and CJDNS.
   - **No type byte.** `specialtxman.cpp:1235-1248` and `deterministicmns.cpp:459-475` hash `*entry.GetAddrPort()` or `*entry.GetDomainPort()`, never the `NetInfoEntry`. So no type byte is prepended. `NetInfoEntry::Serialize`, which would add `m_type` and force ADDRv2 (`evo/netinfo.h:226-239`), isn't used for the key.
   - **Serialization version.** `PROTOCOL_VERSION` doesn't carry `ADDRV2_FORMAT`, and `SER_GETHASH` doesn't change how `CService` or `DomainPort` serialize.
   - **Service.** It is `CNetAddr` then the port big-endian (`netaddress.h:563-567`).
   - **ADDRv1** (`SerializeV1Array`, `netaddress.h:329-361`):
     - IPv6 is its 16 raw bytes.
     - IPv4 is `IPV4_IN_IPV6_PREFIX` plus 4 bytes. The wallet emits the same 12-byte prefix plus 4 bytes, so an IPv4-mapped IPv6 address and its IPv4 address give one key either way.
     - Internal is `INTERNAL_IN_IPV6_PREFIX` plus 10 bytes. That equals the raw 16 bytes the wallet would hold.
   - **ADDRv2** (`SerializeV2Stream`, `:381-395`): the BIP155 id, a compact size, then the address. The wallet emits `[4, 32, …]` for TORv3, `[5, 32, …]` for I2P and `[6, 16, …]` for CJDNS. `test_QT_126_unique_property_is_cores_serialization` pins TORv3.
   - **Domain.** `DomainPort` is a `LIMITED_STRING` (compact size and bytes) then the port big-endian (`evo/netinfo.h:176-180`). It matches.
   - **TORv2 and internal never meet the wallet's own entries.**
     - In Core they are never unique properties. A TORv2-in-IPv6 address reads as `::` (`SetLegacyIPv6`, `netaddress.cpp:135-162`). An internal address is `!IsValid`. Both become an `Invalid` `NetInfoEntry` (`evo/netinfo.h:212-217`) or an empty `MnNetInfo` (`netinfo.cpp:332-341`).
     - The wallet has no TORv2 network id. `ServiceAddr::from_socket_addr` refuses both prefixes (`netinfo.rs:81-85`), so the wallet's own entries can never carry them.
     - A legacy list entry still kept with such bytes (`legacy_entry`, unchanged) is a key that no entry of the wallet can match, and no 15-character domain can match it either (its first byte isn't `0x0f`).
   - **The review's pair.** `0f 61 … 6e` is compact size 15 followed by `abcdefghij.klmn`. Both tests assert the clash, and that a one-byte change (`…6d6f` / `klmo`) doesn't clash (`netinfo.rs:953-960`, `rules.rs:817-844`).
   - **Ran M21** (the old namespaced `e.net_info.contains(entry)`). Killed by `every_listed_net_info_entry_is_unique`.
   - **Ran M22** (an IPv6 key tagged as ADDRv2 `[2, 16, …]`). Killed by `test_QT_126_unique_property_is_cores_serialization`.
   - **Checked, not a finding.** Core's unique-property map is one namespace for every property kind. A 17-character domain serializes to 20 bytes, the same length as a key id or platform node id. A TORv3 or I2P service, or a 33-character domain, is 36 bytes, the same as a collateral `COutPoint`. The wallet's own entries are IPv4 (18 bytes, D5), and no other property kind is 18 bytes, so this doesn't reach `check_net_info_unique`.
4. **Sol m-1 (no libc gate, release on refusal) — closed. Both parts of the manager ruling hold.**
   - There is no libc gate. The reader is pure Rust, and D78 now says the reading is target-independent and the fail-closed point is the coordinator's libc.
   - **Where the refusal happens.** `Session` parsing runs `check_frozen` for every stage other than `Draft` (`dw-protx/src/shared/session/mod.rs:472-474`). So `import_shared_message` fails in `decode_message` with `envelope_refusal(Parse(_))`, which is `shared_message{reason:"parse"}` (`dw-engine/.../shared.rs:398-409`, `store.rs:230-238`). That happens before `claim_shared_session` or any row read. The stored row is untouched: the participant is still `joined` (`Draft`, contribution recorded, coins reserved, `out_at` none).
   - **(a) The session is still discardable.** `discard_shared_session` (`shared.rs:800-830`) refuses only when `out_at` is set, and reads the stored row, not the envelope. It is exposed on the same surface as `join_shared` and `import_shared_message`: the engine API and `dwcli masternodes discard` (`rust/bin/dwcli/src/masternodes.rs:309,1051`). There is no GUI for either yet. D17 describes no timeout, and D20 rules timeouts out ("No timeouts … the user keeps or discards"). So no timeout path is owed, and none exists.
   - **(b) Nothing signs, funds or re-reserves while the session is refused.**
     - `approve_shared` needs a locked stage (`Frozen`/`Signing`), and the stored stage is `Draft`.
     - Funding and signing need `Combined` or later.
     - `join_shared` claims `SharedStage::Invited` only, so it can't re-reserve.
     - The refused message was never stored, so no later call can act on it.
   - **The test.** `test_QT_126_a_frozen_service_read_otherwise_is_refused_and_releases_the_share` (`tests.rs:657-706`) rebuilds the frozen payload with only the service changed and the consent hash recomputed. The 127.0.0.1 control passing `check_frozen` shows the 127.0.0.2 refusal comes from the net info alone. The test then asserts the import refusal is `parse`, `out_at` is none, and `locked(1)` is empty after discard. I read this test and did not run it (no dw-engine build, see below).
5. **Opus n-4 (M9's D-row) — closed.** This is a wording change only, in D78.
   - It says the replay drives `rebuild_funding_tx` and `add_signature`. It sets freeze, combine, funding-signed and broadcast by hand, so `build::freeze`, `build::combine` and dash-qt's merge rules are checked by reading.
   - The cited range `mnsharesession.cpp:1169-1497` matches rc.3: `replaceProTx` at :1169, `absorbDraftReply` at :1204, `adoptLockedTerms` at :1286, `mergeEnvelope` at :1353.
6. **Opus n-5 (glibc harness) — closed, apart from n-1.**
   - **Ran it:** glibc 2.43, seed 20261029, 300,074 entries (74 hand-picked), 14,894 read as a service, 285,180 refused, 0 mismatches. This matches the README and D78.
   - **It compares against glibc,** not against the wallet's reader on both sides. The Core side is a Python port of `AddEntry`'s IP path over the real `getaddrinfo(AI_NUMERICHOST)`. Its `SplitHostPort`, `ParseIntegral`, `LookupIntern` (the max-solutions check before the internal skip), `Lookup`'s second split and `SetLegacyIPv6` each match the rc.3 source I read.
   - **The `::` reasoning is right.** In Core a literal `::` and a TORv2-in-IPv6 address both come out of `Lookup` as `[::]:port`. `ValidateService` then refuses both as `BadAddress` (`!IsValid`, `evo/netinfo.cpp:452-455`).
   - On the wallet side a literal `::` is kept by `from_socket_addr` and refused later by `validate_service` (`netinfo.rs:540-542`). A TORv2 address is refused at once by `from_socket_addr`.
   - The harness's `-` for TORv2 matches where the wallet refuses, not Core's intermediate `[::]`. That is said openly in `core_reads`' docstring, and both sides end refused, so it is a fair normalisation, not a weakening.
   - An internal-prefix result is skipped, as `LookupIntern` does (`netbase.cpp:162-166`), and the wallet refuses it too.
7. **Scope — clean.** The commit touches only D39/D78, the new engine test, the `core_service_read` example, `rules.rs` (the unique-property compare and the n-2/n-3 tests), `netinfo.rs` (`unique_property`, its test, the n-1 forms and a doc line) and the glibc harness and READMEs. That's all DEC-743 items. No scope creep.

**Not a finding.** The doc comment on `listed_net_info` (`rules.rs:73-80`, unchanged) still says a service is keyed "by network, address and port". D39 now gives the exact bytes. Consider aligning the comment in a later touch; it changes no behaviour.

## What I ran

- Commands, all in a reflink copy at `/work/scratch/mg04b-1c-r2-opus`:
  - The copy was made with `cp -a --reflink=always`; `/work` had 316G free.
  - Builds used `CARGO_BUILD_JOBS=8`, nice 19 and a TMPDIR inside the copy. I ran no git commands in either tree except read-only `git diff` and `git log` (with `GIT_OPTIONAL_LOCKS=0`) in the live worktree.
  - `cargo test -p dw-protx --lib`: 99 passed, 0 failed, both before and after the mutant runs.
  - `cargo build -p dw-protx --example core_service_read`, then `python3 -I testdata/oracle/glibc/core_read.py`: the result above.
- Mutants, each applied with sed or perl, run against the named tests, then restored from the live worktree with `cp` and `touch`, then checked with `cmp`. All were killed:
  - M18 (`parse_u16` all-digits check dropped)
  - M19 (per-byte bound dropped)
  - M20 (`Ipv6 => CJDNS`)
  - M21 (namespaced `contains` compare)
  - M22 (IPv6 key tagged as ADDRv2)
- A probe script (`/work/scratch/mg04b-1c-r2-opus/.probe/`) for n-1:
  - a copy of the harness passing a bytes host: same tally;
  - an IDNA-refusal count: 5,142;
  - the three long-leading-zero forms in the n-1 table.
- **Not run:** the dw-engine test `test_QT_126_a_frozen_service_read_otherwise_is_refused_and_releases_the_share`. Building dw-engine at load around 85–105 was outside the time budget, so I checked it by reading only. Also not re-run: the implementer's workspace tally (2,562/0/17), fmt, clippy and the 21/21 gated-mutant run.
- The copy at `/work/scratch/mg04b-1c-r2-opus` is left in place, as the brief asks.

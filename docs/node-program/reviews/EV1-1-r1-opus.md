# EV1-1 implementation review r1 (Opus, high)

Branch `node/ev1-1-netinfo`, head 36fc5ac59, base 5ae846351. I reviewed 9e99e5ef1, 5f92c0027, 257404235
and edaa86c00. I skipped 36fc5ac59 (a contrib-only cherry-pick). Reference: Dash Core v24.0.0-rc.3 (tag
peels to 0364ed626e), read through `git show` from `~/workspace/dash`. Design: EV1-specialtx.md (§3.15,
§3.16, §3.16.1, §6.5, §7) and the owner's D32–D51.

## Verdict in one line

I found no input where the port's verdict or first reject reason differs from Core's. There are no Blocker
or Major findings. One Minor finding is a process gap: certify evidence for the two fuzz targets. The rest
are nits.

## What I checked, and how

All checks were read-only against the branch. Tests ran in a detached worktree
(`/work/scratch/ev1-1-r1-opus/wt`) with its own target dir. Both are removed after this review.

1. **`ExtNetInfo::Validate` order and nesting (M6)**: `dash-dml/src/netinfo/mod.rs:200-233` against
   netinfo.cpp:588-638, line by line:
   - version, `m_data.empty()`, then the global `HasAddrPortDuplicates`, all before any purpose check;
   - then, for each purpose in map order, `IsValidPurpose` → empty → `> 4` → `HasAddrDuplicates` → each
     entry (trivially valid → service rules / domain-purpose → `ValidateDomainPort` / Malformed).

   The order is the same. The first error is always Core's: every branch returns at the same point Core
   returns, and nothing is checked ahead of Core.
   - **Map order and "first wins" (D41).** base-sdk's `NetInfoV2::decode` (dbe245f, netinfo.rs:421-449)
     collects groups into a `BTreeMap<u8, _>` with `or_insert`. That is Core's `std::map` hint-insert
     semantics: sorted by the raw purpose byte, first group kept. Unknown purposes sort by their byte, so
     an unknown purpose 7 falls after a bad purpose-1 entry, as in Core. An unknown Ext version yields no
     groups, as Core's early return does.
   - **Set equivalence.** Core's `std::set<NetInfoEntry>` equivalence (`operator<`) reduces to
     `operator==` for every reachable entry: types differ, or same-type `CService`/`DomainPort`
     tie-compare, or two monostates. So the derived `Ord`/`Eq` on `Entry` gives the same duplicate
     verdicts. That includes "two invalid entries are Duplicate before Malformed".
2. **Duplicates over full strings (M3, D15).**
   - `has_addr_duplicates` compares whole `AddrStr`s (≤ 261 bytes).
   - Domains are read only if `DomainPort::Validate` passes (≤ 253, lowercase).
   - ToStringAddr forms are the same as Core's, including two collisions:
     - a CJDNS entry and an IPv6 entry with the same bytes;
     - a domain spelling an onion or I2P name, or a dotted-quad domain like `1.2.3.4` next to the IPv4
       service.
   - `entry.rs` `Entry::read` matches Core's `NetInfoEntry::Unserialize` validity. A non-UTF-8 domain
     always fails Core's RFC1035 filter, so reading it as Invalid is exact.
3. **dash-netaddr vs Core and glibc.**
   - **Core functions.** `SplitHostPort`/`ParseUInt16` (strencodings.cpp:101-126, 232-271): the `+`,
     `+-`, leading-zero and overflow cases all match.
   - `SetLegacyIPv6`, `UnserializeV2Stream`, `IsValid`, `IsRoutable` (with `fAllowPrivateNet`), all RFC
     predicates, `IsLocal`, `IsBadPort` (netbase.cpp:801-831) and `MaybeFlipIPv6toCJDNS` match.
   - `SetSpecial`/`SetTor`/`SetI2P` and `DecodeBase32` match: the strip order 1-2-1-2, either-case
     letters, the `ConvertBits<5,8,false>` tail check, the case-sensitive `.onion` and the
     case-insensitive `.b32.i2p` suffix.
   - `IPv6ToString` (D37) and `CService::ToStringAddrPort` bracketing match. `operator<`/`==` match
     (network, then prevector size, then bytes; scope ignored).
   - The glibc port (`numeric.rs`) matches glibc's `inet_aton_end`/`__inet_aton_exact`, `inet_pton6` and
     `inet_pton4` statement by statement.
   - **Independent differential.** I wrote my own differential (scratch only, `opus_wide.rs`) against
     this box's glibc `getaddrinfo(AI_NUMERICHOST, AF_UNSPEC)` and ran it on 3.0M strings:
     - 1.5M strings over `0-9a-fA-FxX.:[]+-`, space and tab, mixed with atoms (`0X`, `0xFFFFFFFF`,
       `::ffff:`, …), up to 24 tokens;
     - 1.5M IPv6-shaped strings (1–9 groups of 0–5 hex digits, random `::`, optional dotted tail).

     147,256 of them parsed (2,852,744 rejected), and all 3.0M agree with glibc 2.43. The only disagreement came from my first alphabet,
     which included `%`: `"::%2"` gives `None` against glibc's `Some(::)`. That is D36, the scope id,
     which is not modelled. Neither NetInfo filter admits `%`, so NetInfo can't reach it.
   - **Consensus exposure is narrow.** The only consensus caller of `AddEntry` is `AddNetInfoEntries`
     (specialtxman.cpp:33-42). It feeds back Core's own canonical `ToStringAddrPort()` output, which no
     resolver parses differently. The glibc quirks (octal, `127.1`, `16909060`) are therefore RPC-only at
     the pin.
4. **Per-network settings (D34).** base-sdk params at dbe245f match Core's chainparams.cpp on all four
   networks:

   | Network | Default port | Platform P2P | Platform HTTP | `RequireRoutableExternalIP` |
   |---|---|---|---|---|
   | mainnet | 9999 | 26656 | 443 | true |
   | testnet | 19999 | 22000 | 22001 | true |
   | devnet | 19799 | 22100 | 22101 | true |
   | regtest | 19899 | 22200 | 22201 | false |

   - `IsNodeOnMainnet` is network-kind equality.
   - `MainParams()` is asserted to be mainnet's.
   - `CheckProviderNetworkFields` (`specialtx/network.rs:84-140`) follows providertx.cpp:212-262 step for
     step: version, empty/`allow_empty`, trivially valid, then `CheckNetInfo`, Evo-only node id with
     Core's null-pointer skip (D44), the ExtAddr zero ports, the mainnet ports, the `MainParams()`
     default-port bans, and dup ports against `GetPrimary().GetPort()`.
   - A probe confirmed that an Ext object of unknown version 2 under a v3 ProTx gives Core's
     `bad-protx-netinfo-empty`, not `-bad`. `IsEmpty()` is false, and `HasEntries(CORE_P2P)` is false
     because Core skipped `m_data`.
5. **Census.**
   - `contrib/node/ev1-census.py --core ~/workspace/dash --check` → "281 rows match v24.0.0-rc.3".
   - **Coverage.** I listed every `Invalid(` in each scoped file. All of specialtxman.cpp's 144 sites lie
     inside the script's ranges, and nothing follows the last range (1849-1878). The same holds for
     cbtx, mnhftx, assetlocktx, commitment and providertx.h.
   - **Files with no scope.** These are creditpool.cpp, deterministicmns.cpp, simplifiedmns.cpp and
     llmq/blockprocessor.cpp. All their sites are on the block or credit-pool path. `GetDataFromUnlockTx`
     is reached only from `CCreditPoolDiff`, not from `CheckAssetUnlockTx`. None is reachable from the
     EV1 functions.
   - **Non-literal classifications.**
     - :1367 and :1476 are `TX_BAD_SPECIAL`. `ApplyProUpServTx`/`SetStateVersion` can fail only at :38
       and :94, and both carry `bad-protx-netinfo-version`.
     - :826, :858 and :873 are `BLOCK_CONSENSUS` re-wraps.
   - **Unreachable marks.** The :191 mark is right: both `Validate()`s gate on `IsTriviallyValid`, so a
     valid `CService` reaches `ValidateService` and `BadAddress` can't occur. I rechecked :49 and :105
     against `IsValidPayoutScript`.
6. **Panic freedom and determinism.**
   - `expect`s on `AddrStr` cannot fire: an `Entry` domain is ≤ 253, and every service form is far
     shorter.
   - `has_addr_duplicates` indexes ≤ 4, guarded by the `> 4` check before it.
   - `to_addrv2`'s `unreachable!` is for `Unroutable`, which is never stored.
   - `maybe_flip`'s `expect` is on 16 bytes.
   - `process_candidate`'s `debug_assert` is backed by Success from `validate_service_ext` or
     `validate_domain_port`.
   - `add_entry_bytes`'s non-UTF-8 shortcut is exact: a non-ASCII byte can never land in the port,
     because the port parses only ASCII digits.
   - The fuzz model properties are all true of Core: success leaves the object valid; failure leaves it
     unchanged; AddEntry never returns Malformed or BadType; Validate never returns BadAddress.
7. **Codec switch and the dash-p2p API.**
   - Only node-l1 and node-fuzz take `CoreNetInfoValidity`. `ContextFreeNetInfo` remains for dash-dml's
     benches, example and tests. dash-spv, dash-spv-ffi and key-wallet* depend on neither dash-dml nor
     dash-p2p, so no SPV/mobile path changes.
   - dash-p2p's public API is unchanged:
     - `netaddr::*` glob-re-exports the same `Network`/`NetAddr`/`Service`/`SubNet` with the same
       methods (the old file and the new crate differ only in `Display`, base32 padding and docs);
     - `Address` stays in place;
     - `dial::is_bad_port` is a `pub use`;
     - `NetCore::maybe_flip_ipv6_to_cjdns` keeps its signature;
     - `split_host_port` was and is `pub(crate)`.
   - Behaviour changes are the intended parity fixes: numeric `pre_lookup` now follows glibc (D35), and
     IPv6/CJDNS `Display` is Core's (D37).
   - `I2P` `Display` now uses the local base32 writer, which has the same output (the Core vectors in
     `core_vectors.rs` pass).

**Edge-case probes** (scratch `dash-dml/tests/opus_probe.rs`, mainnet, CJDNS unreachable). Each matches
the result I derived by hand from Core:

| Purpose | Input | Status | Why |
|---|---|---|---|
| — | `"[1.2.3.4:9998]:6"` | Success at port 9998 | double SplitHostPort: the outer port is discarded |
| `CORE_P2P` | `"[1.2.3.4:5]:6"` | BadPort | |
| `CORE_P2P` | `"example.com:9999"` | BadInput | domain outside `PLATFORM_HTTPS` |
| `PLATFORM_HTTPS` | `"Example.COM:443"` | Success, stored lowercased | |
| — | `"example.ONION:443"` | BadInput | the case-sensitive privacy match misses; the lowercased domain hits `TLDS_PRIVACY` |
| — | `"aaaa.B32.I2P:0"` | BadInput | |
| — | `"[fd87:d87e:eb43::1]:9999"` | BadAddress | TORv2 → `::` |
| — | `"[fd6b:88c0:8724::1]:9999"` | BadInput | internal → Lookup empty |
| — | `"16909060:9998"`, `"1.2.3.4:+9998"`, `"1.2.3.4:09998"` | Success | |
| — | `"1.2.3.4:-9998"` | BadInput | |
| — | `"0x01020304:9998"` | BadInput | RFC1035 path, dotless |
| — | upper-case Tor v3 base32 + `.onion` | Success as a Tor service | |
| `PLATFORM_HTTPS` | `"dead.beef:443"` | BadInput | see N2; `Validate()` accepts the same stored domain |

**Tests run.**
- `cargo test --release -p dash-netaddr -p dash-dml --lib --tests`: all pass, including dash-dml lib 84/0
  (2 ignored), `core_vectors` and `glibc_numeric`.
- I did not rebuild node-fuzz or node-l1: they need the oracle crate, and the owner's gate covers them.
  The seed and mutation replay and the ABI-13 differential are taken from the owner's evidence.
- The first build stalled on the shared sccache, and I reran it with `RUSTC_WRAPPER=`. That was an
  environment problem, not a code one.

**D33.** I checked parity only. `allow_private_net` reaches exactly `IsRoutable`'s RFC1918 clause, where
Core reads `fAllowPrivateNet` (netaddress.cpp:467). `NetInfoConfig::default()` is false, Core's
`DEFAULT_ALLOWPRIVATENET`. Behaviour equals Core's.

## Findings

### Minor

**m1. The fuzz targets have no `fuzzctl certify` evidence for the §7 checkpoint.**
- **Where:** `node-fuzz/targets.toml:544-561`; design §6.5 ("Each target gets a `fuzzctl certify` run at
  merge") and §7 EV1-1 ("`netinfo_validate`, `netinfo_add_entry` certified panic-free and
  deterministic").
- **Scenario:** the evidence shows only the gate's 30 s smoke legs (52/52) and the in-crate 2000-flip
  seed replay. Neither is a DEC-186 certify run, and the gate-r2 log has no certify verdict for either
  leg.
- **Core vs ours:** not a parity issue.
- **Fix:** run `fuzzctl certify netinfo_validate@v24.0` and `netinfo_add_entry@v24.0` and attach the
  verdicts. Otherwise record, as a D-row, that certify runs at the train merge, so the checkpoint is
  not claimed early.

### Nits

**N1. A census table comment names the wrong function.**
- **Where:** `contrib/node/ev1-census-classified.toml:27-31` (site `specialtxman.cpp:826`).
- **Scenario:** the comment says "CheckSpecialTxInner on the block path". Core's :822-826 is the
  coinbase branch, which calls `CheckCbTx` and re-wraps its reason. :873 is the `CheckSpecialTxInner`
  one.
- **Core vs ours:** the classification (`BLOCK_CONSENSUS`, `*`) is right; only the comment is wrong.
- **Fix:** change the comment to "CheckCbTx on the block path (CH7a)".

**N2. A hex-only domain passes `Validate()` but fails the AddEntry round trip, and no vector pins it.**
- **Where:** `dash-dml/src/netinfo/add.rs:99-131`; tests in `netinfo/tests.rs`.
- **Scenario:** a stored `PLATFORM_HTTPS` domain made only of `SAFE_CHARS_IPV4_6` characters, such as
  `dead.beef:443`, passes `Validate()` with Success. `AddEntry("dead.beef:443")` routes it to the
  numeric `Lookup`, which fails, so the result is BadInput.
- **Core vs ours:** the same in both (netinfo.cpp:512 vs :530). My probe confirms ours.
- **Why it matters:** this is the one class of valid stored entry that `AddNetInfoEntries` would fail
  to carry. An Ext→Ext `SetStateVersion` can't happen at the pin, since ExtAddr is the highest version,
  but it would surface as `bad-protx-netinfo-version` once one can. That makes it worth pinning before
  EV2b relies on `add_entry`.
- **Fix:** add a vector: `validate` gives Success with that domain, and `add_entry` gives BadInput,
  while `dead.beeg:443` gives Success.

**N3. The sorted-purpose invariant (D41) is not enforced on hand-built values.**
- **Where:** `dash-dml/src/netinfo/mod.rs:160-196, 200-245`.
- **Scenario:** `validate_ext`, `has_entries`, `primary` and `process_candidate` rely on purpose groups
  being sorted by purpose byte and unique. `NetInfoV2` doesn't enforce that, because `entries` is a pub
  `Vec`. Decoded values and `add_entry` keep the invariant. A hand-built value would not, for example
  one from EV2b's `migrate`, a testkit builder, or a `MnList` built in memory. With such a value, the
  walk would be in insertion order, and `find` would read a shadowed group.
- **Core vs ours:** no divergence at present, because every caller hands over decoded values.
- **Fix:** add `debug_assert!(entries.windows(2).all(|w| w[0].0.to_base() < w[1].0.to_base()))` in
  `validate_ext`, plus a sentence in the `pub fn` docs stating the precondition.

**N4. `glibc_numeric` covers a narrow alphabet.**
- **Where:** `dash-netaddr/tests/glibc_numeric.rs:64-102`.
- **Scenario:** the atoms have no uppercase `0X`, no uppercase hex beyond `FFFF`/`FFFF` groups, no
  surrounding whitespace, no `+`/`-`, and no long random runs.
- **Core vs ours:** they agree. My 3.0M-string run over exactly those found no difference outside D36.
- **Fix:** widen the alphabet cheaply, or add a second loop of IPv6-shaped strings. The run takes
  about 4 s in release.

**N5. The `cjdns_reachable` doc doesn't state Core's runtime default.**
- **Where:** `dash-dml/src/netinfo/mod.rs:57-67`.
- **Scenario:** Core's running node removes `NET_CJDNS` from `g_reachable_nets` unless
  `-cjdnsreachable` is set (default off). `NetInfoConfig::default()` agrees: false. The oracle runs with
  it on (D40). This matters for node wiring: a migrated CJDNS entry `[fc..]:p` flips back to CJDNS only
  when reachable. Otherwise it stays IPv6 in `fc00::/7` and is NotRoutable on routable networks.
- **Core vs ours:** no divergence. The doc should state the default so the node wiring keeps it.
- **Fix:** add "(Core: `-cjdnsreachable`, default false)" to the field doc.

## Comparison with Sol r1

I read Sol's r1 (`/work/scratch/ev1-1-r1-sol/EV1-1-r1-sol.md`) after writing the above.

- **No disagreement on parity.** Sol ACCEPTED with no findings, and I also found no input that diverges
  from Core.
- **Where I differ.** My items are not parity bugs:
  - m1 asks for the §6.5/§7 certify evidence, which Sol did not raise.
  - N1–N5 are a comment fix, two hardening suggestions (a vector, a debug assertion), wider test
    coverage, and a doc line.
- **Why APPROVE-WITH-NITS rather than ACCEPT.** m1 should be settled before the checkpoint is claimed:
  either attach certify, or record a D-row deferring it to the train merge.

VERDICT: APPROVE-WITH-NITS

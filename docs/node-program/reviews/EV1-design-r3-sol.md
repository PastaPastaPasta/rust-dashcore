# EV1 special-transaction design — Sol round 3

Target: `node/ev1-design`, `docs/node-program/designs/EV1-specialtx.md` at `dd0711045`.
Range reviewed: `d4c52e4cb..dd0711045` (D17–D31). Reference: Dash Core `v24.0.0-rc.3`.
Only the changed design sections and the cited Core paths were reviewed. The EV1-J joint row is accepted per the manager ruling.

## Opus round-1 findings

| Item | Status | Evidence |
|---|---|---|
| M1 — ProUpReg `key_changed` equality | CLOSED | The design defines step 5a as `!(ptx.pubKeyOperator == state.pubKeyOperator)` and spells out the lazy-wrapper state matrix at `EV1-specialtx.md:446-460,973`; Core uses `CBLSLazyWrapper::operator==` at `bls.h:514-535`, over `CBLSWrapper::operator==` at `bls.h:77-80`. The pairwise vectors are specified at `:855`. |
| M2 — block coinbase bypass and `check_cbtx` | CLOSED | `EV1-specialtx.md:187-192,305-318,974` exports `check_cbtx`, records the pre-loop B1–B4 path, the B6 coinbase skip, the lack of a DIP3 gate, and the `check_sigs` binding. This matches Core `specialtxman.cpp:816-837,853-875`; the block flag binding is `validation.cpp:2663-2665` and rollforward false is `:5197-5200`. The test scope is narrowed to non-coinbase type 5 in the ordinary dispatcher path. |
| M3 — per-element order and pairwise first reject | CLOSED | The payout/share/non-actor loops are explicitly scoped and interleaved at `EV1-specialtx.md:474-510,975`; the L1 generator requires k = 1, 2, 3 simultaneous mutations and ordered pair coverage at `:874-875`. Core’s share loop is `providertx.cpp:77-117`, and the ProDis payee/floor checks are interleaved at `specialtxman.cpp:1717-1737`. |
| M4 — bare unknown ProTx version | CLOSED | D6 and the vectors distinguish a complete two-byte `UnknownProTxVersion` from trailing bytes at `EV1-specialtx.md:202,856,962,976`. Core’s ProTx decoders bail after the version at `providertx.h:167-171,274-278,338-342,407-411`; `GetValidatedPayload` rejects the complete unknown version at `specialtxman.cpp:1097-1122`, while trailing bytes fail payload decoding. |
| m1 — mempool rules must come from tip | CLOSED | `EV1-specialtx.md:138-151,231-238,977` binds mempool rules from the tip, including expired-AU retry and replacement. Core passes the caller’s rules through `validation.cpp:802-825`, supplies them from the tip at `:1097-1104`, and uses tip rules/`CoinsTip`/true signatures for replacement at `:1365-1404` (especially `:1400-1402`). |
| m2 — three BLS schemes | CLOSED | The three-way decode/verify contract is explicit at `EV1-specialtx.md:204-225,978`; vectors cover v2/v3 legacy-only and neither-canonical signatures at `:854`. Core eagerly decodes with fallback at `bls.h:183-201`, lazily decodes keys at `bls.h:494-511`, and verifies under the global scheme at `bls.cpp:296-312`. |
| m3 — noncanonical-wire preimages | CLOSED | `EV1-specialtx.md:793-807,859-860,979` states decoded-object re-encoding and adds repeated/out-of-order ExtNetInfo vectors for all three affected preimages. Core’s serializers are `specialtxman.cpp:1070,1089` and `providertx.cpp:335-395`; ExtNetInfo’s map decode/re-encoding is the relevant Core behavior. |
| m4 — migration converter ownership | CLOSED | `EV1-specialtx.md:25,97-104,748-770,980` removes `convert_for_version` from EV1’s public API, keeps only `migration_reject` over private scratch state, and requires EV2b’s independent `add_entry` conversion to agree. This preserves Core’s ordered migration steps at `specialtxman.cpp:51-106,110-137` and forwarded `TX_BAD_SPECIAL` at `:1363-1367,1471-1476`, consistent with the settled D9 ruling. |
| m5 — Core throws behind seams | CLOSED | `EV1-specialtx.md:106-133,981` introduces `SeamError::CoreThrow`, maps it to `failed-check-special-tx`, and keeps storage/scan-capacity faults separate. This covers the outer catch at `specialtxman.cpp:267-271` and the seam paths through `commitment.cpp:113-208,240-310`, `utils.cpp:634-700`, and `quorumsman.cpp:58-103,149-180,482-536`. |
| m6 — slice/checkpoint and dependency gaps | CLOSED | D26–D30 and the revised §7 at `EV1-specialtx.md:866-869,914-915,939-947,982-986` move coins-view tests to a model in EV1-3, move real L3 checks to MP1a, add `dash-netaddr`, move NetInfo transaction verdicts after the prerequisite trivial checks, and exclude/count unresolved legacy-global verification cases. D27’s EV1-J joint certification removes the prior EV1-5 ↔ CH7a cycle. Core’s two mempool views are `validation.cpp:802-810,997-1018`; block covenant ordering is `specialtxman.cpp:853-875` and `validation.cpp:2700-2716`; the NetInfo address operations named for `dash-netaddr` are exercised by Core at `netinfo.cpp:286-305,307-349,452-498`; legacy verification remains `bls.cpp:296-312`. |
| m7 — empty port, voting key, ancestor, divisor nits | PARTIALLY CLOSED | The voting-key parameter and ancestor-above-prev behavior are recorded at `EV1-specialtx.md:182,348-350,987`; the defensive zero-divisor characterization is at `:875,987`; and the empty-NetInfo `GetPrimary` observation is at `:697-699,857,987`. Core has the voting-key use in `providertx.cpp:301-304`, empty `GetPrimary()` at `netinfo.cpp:343-349`, and defensive arithmetic context in `providertx.cpp:67-81`. The port wording still has the precision issue below. |

## New findings

### Minor N1 — D31 overstates when empty NetInfo produces the duplicate-port reject

**Location:** `docs/node-program/designs/EV1-specialtx.md:697-699,857,987`.

The text says that an empty `netInfo` makes `core_port = 0`, so `platformP2PPort == 0` (or HTTP port zero) gives `bad-protx-platform-dup-ports`, and the vector omits the ProReg version and network. Core reaches the duplicate check only after earlier gates: `version >= ExtAddr` returns at `providertx.cpp:233-241`, and for legacy/basic versions the mainnet platform-port checks and default-port checks run first at `providertx.cpp:243-255`; only then does line `:258-260` compare against the zero `core_port` returned by `netinfo.cpp:343-349`. Qualify the statement/vector as a non-mainnet Evo ProReg with `version < ExtAddr` (and values that do not trigger lines 243–255), or record the earlier expected reason for the other contexts.

No new Major findings. The §7 dependency audit found no remaining cycle: EV1-J is a joint certification row rather than an EV1-5 dependency, as settled by D27.

APPROVE-WITH-NITS

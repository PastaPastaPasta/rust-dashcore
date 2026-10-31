# SW-V24 rc.3 → rc.4 pin-delta audit review (r1)

Reviewed the committed audit and packets against Dash Core tags `v24.0.0-rc.3` and `v24.0.0-rc.4`. The independent tag diff is 107 changed files (+3,839/−1,070). I spot-checked the consensus/evo, masternode, InstantSend, governance, CoinJoin, networking, mempool, RPC, init and wallet areas.

The changed-file sweep is complete in substance. The classifications for #7888 (strict purpose decoding, RFC1918 handling, pre-V24 error and check-order effects), #7909 (block-connect refactor versus incremental template behavior), #7876, and #7839 are supported by the two tags. D1–D4 and D6 in §4.2/§4.3 also agree with the source. The findings below are the corrections needed before the audit can be used as the conformance plan.

## Findings

### 1. **major** — D5 uses a valid domain and therefore predicts the wrong Core result

**Evidence**

- The vector and expected result are stated in `designs/SW-V24-rc4-pin-delta.md:213-228`, specifically D5 at lines 225-226; the same row is repeated in `designs/SW-V24-rc4-pin-delta/ce.md:14`.
- `v24.0.0-rc.3:src/evo/netinfo.cpp:90-113` and `v24.0.0-rc.4:src/evo/netinfo.cpp:90-113` both accept `a.b`: it has length 3, two non-empty labels, allowed characters, and valid label boundaries.
- For a decoded domain in the CORE_P2P group, rc.4 reaches `CheckNetInfo`: `v24.0.0-rc.4:src/evo/netinfo.cpp:610-615` returns `BadInput` because domains are only permitted for `PLATFORM_HTTPS`; `v24.0.0-rc.4:src/evo/providertx.cpp:187-208` maps that to `bad-protx-netinfo-entry`.
- Before V24, the version check occurs before contextual network validation: `v24.0.0-rc.4:src/evo/specialtxman.cpp:1158-1175` returns `bad-protx-version` for a v3 payload.

**Problem**

D5 says `a.b` is “not a valid domain”, is cleared as an invalid entry, and later yields `bad-protx-netinfo-bad`. That is false at both pins. The vector does not test the cleared-entry path. With V24 active its expected result is `bad-protx-netinfo-entry`; pre-V24 it is `bad-protx-version`.

**Suggested correction**

Use a genuinely invalid, fully-read domain such as `a..b` or `a-.b` and retain the expected cleared-entry / `bad-protx-netinfo-bad` result. Alternatively retain `a.b` as a valid-domain control and change the V24 expectation to `bad-protx-netinfo-entry`. Regenerate the D5 corpus and all R7/oracle assertions after choosing one.

### 2. **major** — CE4-06 has no explicit full-node conformance scenario or certification owner

**Evidence**

- CE4-06 is described in `designs/SW-V24-rc4-pin-delta/ce.md:14`; the impact map only asks dash-mnlist/EV2b to “add a test”.
- Core’s regression sequence is explicit at `v24.0.0-rc.4:test/functional/rpc_masternode.py:110-120`: construct a block with `generateblock(..., submit=False)`, submit only its header, call `protx info`, `protx listdiff`, and `masternode payments`, require `-8 Block is not in main chain`, then submit/connect the block and mine the next block successfully.
- The underlying cache writes are `v24.0.0-rc.4:src/evo/deterministicmns.cpp:729,753`; the active-chain guards are `v24.0.0-rc.4:src/rpc/evo.cpp:1805-1808,1939-1941` and `src/rpc/masternode.cpp:411-414`.
- The audit’s task list puts general recertification/full replay at `designs/SW-V24-rc4-pin-delta.md:274-275`, but neither item names this RPC-to-connect sequence or makes it a prerequisite.

**Problem**

A header-only query can fabricate/cache a list for a block that is not connected. In rc.3, connecting that block can leave the stale empty list in place, so the following valid block fails `bad-cbtx-mnmerkleroot` and becomes `BLOCK_FAILED_VALID`. A dash-mnlist unit test and broad replay do not guarantee that the full-node RPC query is interleaved with header submission and connection as Core does. This is consensus-visible state poisoning and needs a dedicated conformance artifact.

**Suggested correction**

Add a named full-node/dashd-RPC integration scenario owned by the RPC/full-node component. Require all three `-8` errors, the successful submit/connect, and successful next-block connection. Add it to the rc.4 certification/recertification gate (R16/R17 or a dependency immediately before them), and retain the EV2b cache unit test as a lower-level check.

### 3. **minor** — The prescribed task order omits the consensus-critical R18 dependency

**Evidence**

- The order in `designs/SW-V24-rc4-pin-delta.md:252-255` ends `R0 → R1–R4 → R5–R13 → R16 → R17`.
- R18 is separately defined at `designs/SW-V24-rc4-pin-delta.md:276` for MN4-08’s consensus IS outpoint-index semantics, with a scenario required against the rc.4 oracle.
- The packet identifies MN4-08 as consensus-relevant at `designs/SW-V24-rc4-pin-delta/mn-overlay.md:103-106`.

**Problem**

R18 is omitted from the order even though it is a consensus scenario and R16/R17 are the final certification/replay steps. R14/R15 can reasonably run independently as planning/tooling work, but R18 cannot silently be outside the final gate.

**Suggested correction**

Insert R18 before final certification (for example, `… → R5–R13 → R18 → R16 → R17`) or explicitly make R16/R17 depend on R18 and state that R18 is deferred only until the IS backend exists.

### 4. **nit** — The hunk total is off by one unless the aggregation is documented

**Evidence**

- The audit claims “349 of 349 classified” at `designs/SW-V24-rc4-pin-delta.md:291-295`.
- A default-context tag diff has 350 unified-diff `@@` hunks.
- `designs/SW-V24-rc4-pin-delta/hunks.md:288` aggregates the `src/wallet/ismine.h → src/wallet/types.h` rename as one row, while `designs/SW-V24-rc4-pin-delta/wallet.md:57` shows two distinct hunk ranges (`+3,19` and `+50,18`).

**Problem**

The classification coverage is effectively present, but the headline count is not reproducible from the tag diff and can make a completeness check appear to pass with one hunk missing.

**Suggested correction**

Either report 350 classified hunks and split the rename row, or state explicitly in §1.2/§8 that the two rename hunks are intentionally aggregated and that “349” means index rows rather than unified-diff hunks.

## Verdict

Verdict: CHANGES REQUIRED

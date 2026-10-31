# O2a-SP independent oracle review (r1, Sol)

Reviewed Dash Core `8319ddce40` against `d4c16888b4..244073f5a2` and the merge remerge diff, and rust-dashcore `dcbf2130a` against `de90fbc14`. This was a read-only source and artifact review; I did not rebuild. The supplied gate result (5123 tests, 55 fuzz smoke, no divergence) is useful, but it exercises the seeded contexts described below.

## Findings

### Blocker — The positive block-path quorum fixtures are states Core cannot store

**Locations:** `dash-oracle-sys/tests/specialtx_block.rs:107-116, 297-303`; `src/test/oracle/oracle.cpp:1442-1458, 3580-3586`; compare Core `src/llmq/commitment.cpp:177-196` and `src/llmq/blockprocessor.cpp:447-455`.

The fixture sets all three `validMembers` and `signers` bits but supplies an empty quorum-member list. The shared `MinedCommitmentRefusal` checks size and minimum counts, then stops before Core's member-tail check. `BlockQuorums` therefore writes the commitment to EvoDB. Core's `ProcessCommitment` calls `CFinalCommitment::Verify(..., false)`, which rejects every set bit beyond `members.size()`, so the real node would never have that mined quorum. The fixture also signs the arbitrary message `[0x22; 32]`; a normal `ProcessBlock` with BLS checks would reject those commitment signatures before `ProcessCommitment`. A validly signed MNHF or asset unlock using the seeded public key can consequently return `Accept` here when there is no reachable Core chain state with that quorum. This under-refusal also affects `oracle_llmq_set_mined` through the shared helper.

**Suggested fix:** Validate seeded commitments with Core's actual members and `CFinalCommitment::Verify`, including the signature checks required by the modeled block connection, then replace the positive fixtures with commitments whose member set and signatures pass. If a truncated context cannot supply the members, return `ORACLE_E_CONTEXT`; do not write an unchecked commitment and claim it was mined. Add a regression case for the current three-bits/zero-members fixture.

### Major — NetInfo cannot model `-allowprivatenet`, although its verdict reads that global

**Locations:** `src/test/oracle/oracle.cpp:1550-1573, 3720-3758`; compare Core `src/init.cpp:1291`, `src/netaddress.cpp:464-470`, `src/evo/netinfo.cpp:286-304, 452-477`; Rust surface `dash-oracle-sys/src/netinfo.rs:69-119`.

Both NetInfo functions call Core's real `Validate`/`AddEntry`, but `CService::IsRoutable()` uses process-wide `fAllowPrivateNet`. `oracle_set_network` registers only chain-parameter options and calls `SelectParams`; it never assigns this global from an explicit input. On mainnet, `10.0.0.1:9999` gives `NotRoutable` under the oracle's default. A real node started with `-allowprivatenet=1` considers that RFC1918 address routable and can return `Success` for the same otherwise valid MnNetInfo or ExtNetInfo. Passing the option in `Chain.args` does not reproduce Core startup's assignment. The tests cover the default only. Thus network name alone does not determine these statuses.

**Suggested fix:** Make the private-network setting an explicit oracle input (or a supported, applied network-setting argument), set it under the existing process-state lock, and restore/reset it when changing configurations. Exercise default/allowed/default calls in one process for both entries.

### Minor — A valid commitment at a truncated context's first header is misclassified as a bad argument

**Locations:** `src/test/oracle/oracle.cpp:1443, 3570-3582`; compare `src/llmq/commitment.cpp:122-128`.

For a context starting above genesis, its first header is a checkpoint-like index with `pprev == nullptr`. If a real, valid quorum base is that header, the node has its parent and can store the commitment; the oracle's `MinedCommitmentRefusal` reports `parentless quorum base block` and `oracle_check_special_tx_block` returns `ORACLE_E_ARG` before reaching the special transaction. The parent is missing from the *context*, not the real chain. This is an over-refusal; it also applies to `oracle_llmq_set_mined` with such a base.

**Suggested fix:** Return `ORACLE_E_CONTEXT` and ask for the prior header when the base is the context boundary above height zero. Reserve `ORACLE_E_ARG` for an actual genesis/parentless base.

## Scope and checks

- `oracle_check_special_tx_block` does invoke the private `CheckSpecialTxInner` with a non-null `CChain`, the tip, `GetSpecialTxRules(tip)`, the context coins view, a present `CRangesSet`, and the caller's `check_sigs`. This reaches the chain overloads for MNHF and asset unlock. `WriteState` copies `TxValidationState`'s result, reject reason and debug message; the Rust `OracleValidation` has the corresponding `repr(C)` fields. It is a transaction-level oracle. Its documented omission of the earlier `CheckSharedCollateralTemplateOutputs` loop means it must be composed with that check before treating `Accept` as a ConnectBlock verdict: at V24 a type-7/9 transaction paying the shared-collateral template can pass this entry while `ProcessSpecialTxsInBlock` rejects `bad-shared-collateral-create`. Coinbase prechecks and the block-state debug wrapper are likewise outside this entry's stated scope.
- ABI 13, the three new FFI signatures, NetInfo status numbers, and the C/Rust validation layout match by inspection. `git show --remerge-diff 8319ddce40` shows only the ABI-version conflict resolved to 13; the CS4 `oracle_versionbits_state` declaration and Rust binding remain present. The installed plain/fuzz archives hash to `1d9ce8643813578ce40517c1a872680c73c95f1fb7e94457163182ae4f39e829` / `c9dfe2779c94f72343cedf3f5a334704ce046670969b85f926581734c90d89e2`, matching `pins.txt` and both manifests.
- The Rust wrappers pass borrowed slice pointers and lengths only for the duration of each C call, use owned output buffers, and run the new calls under `on_chain`'s exclusive pin lock. I found no new FFI ownership or panic-across-FFI issue in these bindings. `netinfo_add_entry` retries after `ORACLE_E_BUFFER` on a fresh Core object, so that retry does not carry mutation into the next call.

VERDICT: CHANGES-REQUESTED

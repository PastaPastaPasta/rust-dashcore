# O2a-GV oracle entries — independent review, round 2 (Sol)

Reviewed rust-dashcore `75367dd61` and the Core shim `b79e08e056` against Core v24.0.0-rc.3. **No blocker, major, minor, or nit findings.**

## Review

- **Block value short circuit:** `oracle.cpp:4525-4568` runs the payee reward guard, `IsBlockPayeeValid()`, and output construction only after `IsBlockValueValid()` succeeds. This matches `validation.cpp:2846-2850`. The record marks an unrun payee check with `payee_valid = 0` and uncomputed outputs with `outputs_computed = 0`; `payments.rs:292-325` maps the former to `None` and rejects the new field contradictions. The overpaying regular block without a masternode list returns `bad-cb-amount`; the same context needs its list when the value check passes (`tests/payments.rs:268-287`).
- **Credit-pool window:** `oracle.cpp:4653-4678` requires the window-start block only after the diff check accepts, immediately before `GetCreditPool(next)`. `creditpool.cpp:376-391` reads the parent's pool for the diff; the shim seeds that pool in the new manager's cache. An over-limit unlock with no window block returns `failed-creditpool-unlock-too-much`, while an accepted step without it still returns `ORACLE_E_CONTEXT` (`tests/payments.rs:812-843`).
- **Guard before DIP3 enforcement:** Core's `IsTransactionValid()` returns before reading the list at `payments.cpp:230-234`; the shim's guard can run after DIP3 activation. I found no valid-chain divergence in that gap. ProRegTx validation limits the operator percentage to 10000 and validates payout/share totals (`providertx.cpp:25-121,328`). Because the guard tests the tip and Core checks the next block, the affected next-block heights are mainnet 1,028,161–1,047,199, testnet 7,001–7,299, and regtest 433–499, with no devnet gap. At those heights, a 100% operator multiplication would need more than 9.22 million DASH of masternode reward to overflow `int64`, beyond the reward funded by the spendable supply in the window. Arbitrary high-fee or malformed-list context bytes can cause a refusal, but they do not represent a chain Core can produce.
- **Pin and regenerated data:** Both cached archive SHA-256 values equal `pins.txt`. Comparing decoded JSON at `13567deee` and `75367dd61`, all 54 regenerated SB4 files change provenance; only two also change `vector/instructions_u`. The perfgate `bench_inputs` hashes match across the two revisions for all 13 packages. I did not rerun the owner's full tier-1 gate; its reported 5,223/0 and 295/0 counts remain owner evidence. The live governance comparison still uses synthetic triggers, as D20 now says, so it does not establish parity with a reference node's full trigger set.

## Test and mutation evidence

- `TMPDIR=/dev/shm/o2a-gv-r2-sol-tmp cargo test -p dash-oracle-sys --features oracle`: **exit 0** on the pinned plain archive; the 12 payment tests and the SB4 provenance tests passed.
- Scratch Rust decoder mutant: removed the `!value_valid && payee_valid` refusal. `payments::tests::records_parse_exactly` failed at the planted contradictory byte (offset 15).
- Scratch Core mutant: restored the old window-start requirement before `CheckCreditPoolDiffForBlock()` and linked an explicitly unpinned archive. `credit_pool_unlock_block` failed with `ErrorKind::Context`, missing block 1,152,576, on the rejecting no-window case. The reviewed branches and pinned archives were not changed.

**Verdict: APPROVE.**

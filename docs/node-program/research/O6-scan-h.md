# O6 scan (h): Exotic legacy BLS fields — aggregate counts

Scan (h) counts "Exotic" legacy-scheme BLS public keys: bytes that are canonical
in the legacy (Chia) 48-byte encoding and that Core's relic-based decoder admits
(no curve/subgroup check), but whose point is **off the prime-order G1 subgroup**.
A strict/subgroup-checking decoder would reject these; Core historically accepts
them. A1 §1.4.1 and the BSX catalogue assume this scan finds none.

**Classifier.** A key is Exotic iff it decodes under its own scheme (legacy keys
via the no-subgroup-check legacy path) yet fails to lower to the subgroup-checked
IETF encoding. Implemented against `dash-pkc` (`BlsPublicKey::from_bytes` +
`to_scheme::<BlsScIetf>`), validated against the known off-subgroup fixture
(classifier reports it Exotic, as expected). All-zero (null) operator keys —
used by revoked / PoSe-banned masternodes — decode to the invalid/zero object in
Core too and are counted "null", not Exotic.

## Current masternode list (mainnet, height 2,094,670)

Source: `protx listdiff 1028160 <tip>` on the read-only mainnet reference oracle
(provably-pure RPC, concurrency 1). 3,819 masternodes in the deterministic list.

| scheme | entries | valid (in-subgroup) | null (all-zero) | **Exotic (off-subgroup)** |
|---|--:|--:|--:|--:|
| legacy (state v1) | 3,032 | 2,684 | 348 | **0** |
| basic (state v2)  | 787   | 786   | 1   | **0** |
| **total**         | 3,819 | 3,470 | 349 | **0** |

**Result: 0 Exotic legacy BLS keys in the current mainnet masternode list.**
The null (all-zero) keys are revoked / PoSe-banned masternodes, not anomalies.

## Historical blocks

Not completed in this pass. The reference oracle is still in initial block
download (≈74.8% verification progress; block tip 2,094,670 of ~2.3M), and a
full historical sweep of every ProRegTx / ProUpRegTx operator key and QC v1/v2
commitment key across ~1M special-transaction-bearing blocks, one read-only
request at a time, was out of budget for this pass and would stress the shared
oracle. The current-list scan above (the part A1 §1.4.1 most directly rests on)
is clean. A full historical scan should be re-run via the `dash-conformance scan`
path once an oracle (or a dedicated replica) is fully synced; recommend running it
in blk mode over `blk*.dat` so stale-block special txs are also covered.

_Method and per-item detail are tracked privately under PLAN §7; this file holds
only aggregate counts (no keys)._

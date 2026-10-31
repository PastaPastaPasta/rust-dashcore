# O9b S6a review, round 1

Verdict: **ACCEPT-WITH-NITS**

M findings: 0.  m findings: 0.  n findings: 1.

## n-1 — storage encoder changed which refusal wins

**Location:** `dash-dml/src/codec.rs:329-339` (`check_canonical_list`), compared with the pre-split implementation at the parent commit's `dash-dml/src/codec.rs:333-349`.

The storage `encode_canonical_list` path used to walk Core order once: it checked each masternode's nested invariants and immediately round-tripped that same masternode with `ValidateMNEncoding`. The split now runs `check_canonical_invariants` over the entire list first and only then runs all encoding checks. This changes the observable refusal for a concrete in-memory list containing, in Core iteration order, (A) a masternode whose operator-key bytes are noncanonical for its version and (B) a masternode with invalid non-empty network info. Before the split, A returns `noncanonical MN object encoding`; after it, B is encountered during the first pass and the result is `invalid canonical MN nested collection`. The acceptance class is unchanged, but existing storage callers no longer have the same behavior they had before `fc81e511b`.

Fix by keeping the storage wrapper's old interleaved per-entry check (or adding a dedicated helper for that order), while retaining the separated `ValidateCanonicalMNInvariants` then `ValidateMNEncoding` sequence for the EvoSnapshot validation path, which is what Core itself does.

## Checks performed

- Read the complete S6a diff (`8465296b5..74c47cc96`) and the implementer's D17–D24 notes.
- Compared `dash-dml/src/snapshot/{codec,validate,mod}.rs` and the codec split against Dash Core v24.0.0-rc.4 `src/evo/snapshot.{h,cpp}`, `src/evo/deterministicmns.{h,cpp}`, `src/serialize.h`, `src/evo/dmnstate.h`, and `src/saltedhasher.{h,cpp}`.
- Checked all bounded decoder fields, at-limit/over-limit vectors, canonical ordering, replay-chain checks, operation and reconstruction budgets, BLS re-encoding, credit-pool encoding, MNHF signals, and snapshot/list hashes. The recorded 142 Core verdicts and vector hashes agree for the exact comparisons, with the documented D20 stricter-domain and D22 class-only exceptions.
- Confirmed the hash path is Core's single SHA256 over the sorted canonical re-encoding, including empty and populated vectors.
- Confirmed `updatedMNs` is a salted `std::unordered_map`; the salt is generated from `GetRand<uint64_t>()` at process startup. Libstdc++ bucket traversal therefore has no portable fixed order for IDs 5 and 7. The eight-run (and additional repeated) judge observation naming the same masternode does not establish a deterministic order, so the CLASS_ONLY treatment is appropriate; descending ID is only a heuristic for the port's diagnostic text.
- Ran the standalone Core judge read-only from `/work/scratch/o9b-s6a-r1-sol` on the duplicate-owner crafted input; it consistently refused, matching the recorded Core verdict. No build was needed.
- The 8,000,000-record default is cited from Core and the reconstruction code's boundary is exercised with explicit smaller budgets (`reconstruct:33` accepted and `reconstruct:32` refused). A full 8-million-record fixture is not necessary to validate the same comparison logic, and the other snapshot horizon bounds make such a fixture impractical.

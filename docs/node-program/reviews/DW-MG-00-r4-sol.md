# MG-00 r4 independent design review

Reviewed `26aed42b323446d644ac13b575e95e8330d62106..685654f50caf4011d0c6edc7979c1a372c9f5f0d`, the nine changed PNGs at original resolution, and the generated HTML. `git diff --check` is clean. I assume a user-marked outcome may remain an unverified terminal annotation when proof never arrives, provided it does not block another withdrawal and the wallet keeps looking for proof.

## Findings

1. **Major — The late-proof notice asserts the second withdrawal succeeded and used the first destination.** `docs/design/UX-SPEC.md:1065-1068`; `docs/design/MG-00-masternodes-ux.md:145` (D47). “If you withdrew again since, both withdrawals went to yPay8vK2…Wm4c” does not follow from proof of the first transition. The later request may still be pending or have failed, and D23 lets a payout-key withdrawal go to any address. This notice can misdirect a user checking for funds after a possible duplicate withdrawal. Name the proven first withdrawal and its stored destination; say that a later withdrawal **may** also have gone through, and direct the user to that request's own status and destination. State that both completed only when both transitions have proof.

2. **Minor — The revised create notice treats the budget ceiling as a guaranteed payout.** `docs/design/UX-SPEC.md:1155`; `docs/design/ux/mg-00/src/governance.html:193`; frames `gov-create-light` and `gov-create-dark`. “Each superblock pays out 6,018” is not generally true when funded proposals use less than the available budget. The exact 6,018 figure is fine; phrase it as “Up to 6,018 Dash is available at each superblock” and keep the peer-seen estimate on 4,760.

## Closure check

- **R3 #1:** The manual choices are now explicitly user-marked, separate from proven Done. The transition ID and signed bytes persist, lookup continues, and late proof upgrades or corrects the mark. Finding 1 concerns the newly added correction notice.
- **R3 #2:** Governance numbers now separates exact chain/list figures from estimated vote-derived totals, and the create notice qualifies the passing total. Finding 2 concerns the new claim about actual payout.
- **R3 #3:** Both rebroadcast nonce errors enter Couldn't confirm; the too-far-in-past copy makes no claim that the nonce was used.
- **R3 #4:** `git diff --check` reports no whitespace errors. The changed PNGs show the intended copy and no visible layout regression.
- **State machine:** Sent can resolve to Done, Rejected, or Couldn't confirm; Couldn't confirm can resolve by proof or either user choice; a later proof can resolve either marked state to Done. Rejected has the specified Try again path, and Done is terminal. Marked states may remain unverified indefinitely, but Withdraw is enabled and lookup continues, so they do not trap the user.

VERDICT: CHANGES-REQUESTED

# MG-00 r3 independent design review

Reviewed `173c59bc..26aed42b` in `dw/mg-00-ux-spec`, the 26 changed light/dark PNGs at original resolution, Dash Core's confirmation constants and operator-payout reset paths, and Platform's nonce validation. I assume D47's manual choice is an accepted escape path, but a user's observation of an amount at an address is not a record uniquely attributable to the signed transition.

## Findings

1. **Major — Manual arrival is promoted to proven success.** `docs/design/UX-SPEC.md:1034-1052`; `docs/design/MG-00-masternodes-ux.md:142,145,165` (D44/D47). D44 requires proof of *this* transition before the wallet says “Withdrawal done,” yet D47 maps [It arrived] directly to that success state. An equal amount at the payout address, or the user's recollection, does not identify the transition; D44 itself rejects balance changes as proof. Keep D47's manual escape path, but label it as a user-marked, unverified outcome, retain the transition ID and signed bytes for later lookup, and reserve the app's “Withdrawal done” state for uniquely attributable proof. State how late proof reconciles a manual choice.

2. **Major — The D45 estimate rule is still missing on fee-decision and numbers screens.** `docs/design/UX-SPEC.md:1115-1133`; `docs/design/ux/mg-00/src/governance.html:269-273,193`; frames `gov-numbers`, `gov-create` (both themes). D45 says vote-derived counts are conditional estimates over votes peers sent. The Governance numbers sheet still gives “4,760 to 9 passing proposals” and “9 passing · 2 over budget” as unqualified figures, and the proposal-payment warning says “Passing proposals already ask for 4,760” before a non-refundable 1 Dash fee. “From 8 peers” and “Up to date at block” do not tell a user that latest votes may be missing. Add an estimate label scoped to these figures, use “about” where appropriate, and carry the peer-seen qualification into the create warning; distinguish exact chain/list figures such as the funding line and superblock budget.

3. **Minor — D47's nonce explanation can state a fact the wallet does not know.** `docs/design/UX-SPEC.md:1042-1050`; `docs/design/MG-00-masternodes-ux.md:145` (D47). D44 correctly includes “too far in past” among ambiguous rebroadcast nonce errors, but D47 only opens after a nonce “was used,” then says it was used by this withdrawal or another request. Platform's `NonceTooFarInPast` can mean the tip moved beyond its history window, without proving that nonce was used. That error either leaves this withdrawal stuck outside D47 or makes the D47 copy false. Allow the escape path when identical rebroadcast can no longer be accepted, but say “the original request can no longer be retried; we still cannot confirm whether it went through” for the too-old branch.

4. **Nit — Generated HTML fails the whitespace check.** `docs/design/ux/mg-00/masternodes.html:788,876`. The new backdrop frame emits whitespace-only lines with trailing spaces; `git diff --check 173c59bc 26aed42b` reports both. Trim the generated frame separators and regenerate the HTML.

## Closure check

- **R2 #1 (D43): closed.** The verified-header lookup uses 15 confirmations on mainnet and 1 on testnet, devnet and regtest, as in Core's `chainparams.cpp`. It derives `(ProRegTx txid, payload index)` for null-hash internal collateral, checks that output's amount and script, and maps votes by the derived outpoint.
- **R2 #2 (D44): mostly closed; finding 1 remains.** The upstream API dependency now includes pre-broadcast persistence of signed bytes, transition ID and nonce, lookup by ID, identical rebroadcast, and proof-only automatic success. D47's manual success label conflicts with that proof rule.
- **R2 #3 (D45): partly closed; finding 2 remains.** The list/detail/vote screens now qualify peer-seen votes, show ranges and “last vote seen,” and use “Too close to call” for a range crossing the line or budget order. The secondary surfaces still have unqualified vote-derived totals.
- **R2 #4 (D46): closed.** The spec scans ProUpServTx, ProUpRevTx and both operator-key-change forms in block order, and the shared frames show both the current owner share and the conditional share after the operator payout is set. Core's `ResetOperatorFields()` clears that script on revocation and operator-key change.
- **R2 #5 and #6: closed.** The governance legend says “about,” and `mn-withdraw-pending` uses `mn-pending-behind`, whose summary and evonode pane say “withdrawal being checked” with Withdraw disabled.

VERDICT: CHANGES-REQUESTED

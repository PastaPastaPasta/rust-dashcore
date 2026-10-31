# DP3-06 review — Sol r1

Reviewed `b006a680` against `913893dd`, the DP3-06 design, the renderer send path, the engine's contact broadcast contract, and the GL harness. Static review only; I did not run tests or alter the worktree.

## Findings

1. **Low — A fast unknown retry can strand the GL run.** `apps/desktop/e2e-tauri/pay-contact.ts:229-235` starts observing the Retry button only after WebDriver's click returns, then requires seeing its temporary disabled state (or Sent). A retry that immediately returns another unknown result, such as a no-peers response, can disable and re-enable the button before that poll sees it. The harness then waits for a state that has passed, times out, and records the ledger as failed. This does not cause another payment—the ledger blocks reruns, and the app retries the same prepared handle—but it defeats D23's load recovery. **Fix:** install an attempt observer before clicking, or expose a stable retry attempt/completion marker, and wait for that marker plus the settled result. Cover an immediate unknown retry in a fixture or harness test.

2. **Low — The accepted-but-unsent result repeats the same refusal.** `apps/desktop/src/screens/pay/ConfirmSheet.tsx:212` prefixes `The payment was not sent:` to `failureText`. For `send.no_peers`, `send.broadcast_rejected`, and `send.cancelled`, `apps/desktop/src/screens/pay/model.ts:192-202` already ends the reason with “The payment was not sent” or its equivalent. The resulting copy repeats itself; `apps/desktop/e2e/pay-contact.spec.ts:119` currently asserts that wording. **Fix:** render the acceptance sentence and the existing refusal as separate sentences, or strip the duplicate status phrase before joining them. Update the C8 assertion.

## Money-safety assessment

The selected contact ID and wallet scope carry through to the draft. The engine rechecks the live relation and broken channel at prepare, and claims and records the contact lock before first dispatch. The renderer refuses the estimate placeholder, extra outputs, altered amounts, fee, change, and debit. An unknown retry passes the same draft and prepared handle back to `draftBroadcast`; the engine's `Unknown` phase repeats that signed transaction. A failed accept never reaches prepare. I found no branch change that can make this path pay twice or send a newly prepared transaction on retry.

The contact promise check deliberately accepts any single non-wallet address other than the placeholder; the engine's reserved-address derivation remains the payee authority. The proof-trust gate for unverified Platform data is explicitly deferred to DP3-04 in the design, so this review does not treat it as a DP3-06 regression.

VERDICT: APPROVE-WITH-NITS

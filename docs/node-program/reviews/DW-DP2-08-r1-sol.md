# DP2-08 independent review r1

Reviewed `8276bf62..2d4ba8c2` at head `2d4ba8c2` without editing the branch. I treated the design D-rows as the intended behavior. Findings are ordered by risk.

## Findings

1. **Major — An ignored sender can be offered Send during a stale-list window.** `apps/desktop/src/contacts/add.ts:69-72`, `apps/desktop/src/screens/contacts/AddContact.tsx:158-162`. `currentRelation` replaces a fresh search or verified-scan relation of `ignored` with *any* cached unfiltered-list relation, including `stranger` when the row is absent. The accompanying unit test at `add.test.ts:83` explicitly expects `ignored` plus `[]` to become `stranger`. If an ignore has reached the engine while the Contacts list is still stale or refetching, the fresh hit can say `ignored` but the card asks eligibility using `stranger`. The engine treats ignored senders as eligible strangers, so the card can offer Send; sending can turn the ignored request into an established contact. Preserve `ignored` until a list read known to be newer than that hit proves it was unignored, or check the ignored state directly before offering Send. Add a regression test with a fresh ignored hit and a stale empty or pending-incoming list.

2. **Major — Oversized dropped or pasted files are read before the 32 MiB limit is checked.** `apps/desktop/src/screens/contacts/AddContact.tsx:368-370`, `apps/desktop/src/contacts/add.ts:265-269`. `take` awaits `file.arrayBuffer()` before `imageBytes` compares the byte length with `MAX_IMAGE_BYTES`. Dropping a multi-gigabyte file can allocate its entire contents in the webview, freezing or crashing the app before it shows “The image is too large.” Check `file.size` before `arrayBuffer()`, then keep the existing byte-length check as defense in depth. Cover the early refusal with a File whose `arrayBuffer` must not be called.

3. **Minor — An auto-accept code's expiry note does not age while the card sits open.** `apps/desktop/src/screens/contacts/AddContact.tsx:225-226`, `apps/desktop/src/contacts/add.ts:139-143`. The note uses `backend.now()` only during render, with no timer or expiry-triggered state update. A claim initially shown as “It expires in 30 min.” can still say that after 30 minutes, and the Send button remains offered until the engine refuses it after the grant. Update the displayed time (at least when the 60-second margin is crossed), and disable or replace Send when the claim is no longer sendable. The engine's final expiry check is present, so this is misleading UI rather than an expired-code write.

## Checks and scope

- Focused Vitest in a detached scratch worktree: `actions.test.ts` and `add.test.ts`, 35/35 passed. Removing `if (this.running) return null` in that scratch copy made `actions.test.ts` fail its “runs one action at a time” assertion. The scratch worktree was removed afterward.
- The link route is parsed natively and the renderer rebuilds and verifies it before showing a candidate. Scanned text follows the same verification path; a failed verification exposes no Send action. The candidate's `unverified` field drives the badge, including the trusted fallback case. No additional finding there.
- The Linux portal checks the response path and sender against the method reply, closes the request and connection on cancel, and does not write the captured bytes. Windows COM initialization and cleanup are present. Windows runtime behavior remains unverified as documented in F4.
- The GL ledger uses exclusive creation before Send and blocks a rerun after an unfinished state; output and copied logs are scanned for the fixture phrases and passphrase. I did not run the testnet script or repeat the full gate. The reported full-gate and real-app results are taken as supplied evidence.

VERDICT: CHANGES-REQUESTED

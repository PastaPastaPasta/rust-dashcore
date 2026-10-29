# DP3-03b independent code review, round 2 (Sol)

**Head:** `b64dea6c` (`dw/dp3-03b-username-optout`), compared with round-1 head `122170dd`.

## Findings

No blocker, major, or minor finding in the DP3-03b changes.

## Round-1 findings

1. **Stored requests after opt-out — closed.** `apps/desktop/src/screens/Receive.tsx:229-259` derives the sheet's URI, username row, QR and copy value from the same setting-gated request. `apps/desktop/src/api/username.ts:33-35` removes the engine's trailing `user=` and clears `username`. The pending-change marker is set before the awaited save (`Privacy.tsx:31-45`), and the setting query reads off during that interval (`queries.ts:148-164`). In my scratch copy, making the sheet use its raw request failed the stored-request Playwright test at `username-optout.spec.ts:174`, where `&user=alice` appeared. Removing the pending-change condition failed the turning-on test at line 204, where Copy link yielded `?user=alice`. The unchanged tests passed.

2. **Engine setting/name race — closed.** `rust/crates/dw-engine/src/receive.rs:466-481` takes the per-wallet read lock before the setting and name reads. `create_receive_request` and `receive_requests` retain that guard through URI construction (`:380-397`, `:416-438`); `set_username_in_requests` takes the write side (`:514-521`). In my scratch copy, deleting the setter's write guard failed `turning_the_username_off_waits_for_a_request_read_under_way` at `users_tests.rs:687`: the opt-out completed while the named read was paused. The unchanged test passed. A URI already constructed before the opt-out's write may still be delivered later; the renderer masks an outstanding result while its own change is pending.

3. **Old write after remove/re-import — closed.** The setter holds `wallet_lifecycle` from the wallet check through the app DB write (`receive.rs:514-521`). Removal (`wallets.rs:225`), unload/load (`multiwallet.rs:265,305`), seed import (`keys.rs:471`), watch-only import (`multiwallet.rs:442`) and backup restore (`backup.rs:576`) acquire that lifecycle lock before their wallet mutations. None of those other lifecycle paths acquires `UsernameGate`, and the request read side does not acquire lifecycle while it holds the username guard, so I found no reverse order or lock cycle. In my scratch copy, deleting the setter's lifecycle guard failed `a_held_turn_off_never_reaches_a_reimported_wallet` at `users_tests.rs:750`: the re-imported wallet started off. The unchanged test passed.

4. **QR before setting answer — closed.** `Receive.tsx:42-47` returns `undefined` until the setting has answered; the QR query is disabled then (`:68-79`). The name is used only when the effective setting is true. In my scratch copy, restoring the old “only literal false suppresses a name” rule failed `username-optout.spec.ts:227`: Copy link became enabled before the held setting read answered. The unchanged test passed.

## URI and privacy sinks

`dw-uri` appends `user=` last (`rust/crates/dw-uri/src/core.rs:116-123`), after amount, label and message; the latter two are percent-encoded (`:97-106`). The fixture producer also appends it last (`apps/desktop/src/fixture/send.ts:197-208`). The sheet receives only engine or fixture `ReceiveRequest` values, whose URI is computed on read, not stored as arbitrary text. A scratch Vitest case confirmed that `withoutUsername` removes `user=a%26b%3Dc` while preserving earlier percent-encoded fields. I also checked the negative case: a nontrailing `user=` would remain, but no current request producer can supply that ordering. This producer invariant should be kept if URI formatting changes later.

The sheet's sanitized URI supplies both `Qr` and `CopyValue` (`Receive.tsx:233-255`); CopyValue writes only the supplied value to the clipboard, and the toast contains only “Copied” (`components/detail.tsx:20`, `components/toast.tsx:23-29`). The address QR uses a separate setting-gated URI query (`Receive.tsx:62-79`); an old named QR matrix can remain in the in-memory query cache, but the current text selects a different key after opt-out and does not render or copy that matrix. I found no new logging of usernames or URI values. `ReceivePage` is keyed by wallet ID in `App.tsx:187`, so an open request sheet cannot survive a wallet switch with the previous wallet's request.

## Validation and limits

All work was read-only on the reviewed worktree; mutants ran in `/work/scratch/dw-dp303b-r2-sol/repo`. My checks: three unmodified Chromium Playwright privacy cases passed; the three corresponding renderer mutants failed at the privacy assertions; five Vitest cases (including two scratch-only delimiter/order cases) passed; the unmodified engine's two race tests and plain-URI test passed; both corresponding engine mutants failed at their intended assertions; `e1_offline::username_in_requests_survives_a_restart` passed. The brief reports a wider green gate (Rust 2018/0, Vitest 900, Playwright 1332/0), which I did not rerun. The merge from main includes unrelated DP1-10 work; this review covered the DP3-03b fixes and their interactions, not a full review of that unrelated work.

## Open questions or assumptions

No blocking question. The renderer's immediate masking assumes the configured single Tauri main window is the setting writer, as DP3-03b D7 states. An independent writer would need an event or another invalidation path to refresh that window's cached setting.

**Verdict: APPROVE**

APPROVE-WITH-NITS

1. **CLOSED — Round-2 MAJOR: queued lifecycle actions no longer run after node release.**

   **Where:** `Vendor/swift-cross-ui/Sources/SwiftCrossUI/Views/Modifiers/Lifecycle/LifecycleHookModifier.swift:77–106`; `OnAppearModifier.swift:30`; `Views/Modifiers/Handlers/OnChangeModifier.swift:76`.

   I reran my original `deferredHooksAfterRemoval`, `changedHookAfterRemoval`, and `taskCreatedByLateAppearLeaks` probes against the pinned source, retaining their assertions and changing only helper names to avoid collisions with the new repository tests. All three pass: the events remain `["disappear"]`, the resource stays inactive, and the late resource task is never created. The repository's three corresponding regressions, task-cancellation test, and task-before-appear test also pass.

   The flag is allocated once with the modifier's children, is never reset, and is invalidated in their synchronous `deinit`. The callback captures that separate flag and the user action, without capturing the children or node. My additional probe removes the node and drains the backend **without first yielding to cleanup**: no appear/change runs. Thus validity does not depend on the disappearance task winning a scheduling race.

   Removal followed by recreation before dispatch gets a fresh flag: only the new generation's appear/change runs. Moving content between two structural parents drops the old appear and runs the new parent's appear. A keyed `ForEach` reorder preserves the surviving nodes and their pending actions; removing and reinserting an ID creates a new lifetime. There is no node pool here that resets an ended flag and allows an old callback to execute on a recycled node. Ordinary layout/rebinding does not replace the lifetime holder, so it does not end too early.

   Weak-reference assertions establish that both node and children are released while the callback remains queued. The flag remains temporarily alive through the callback and is released when the fake backend consumes it. I found no ownership cycle in the new holder. This conclusion concerns the holder; see finding 2 for GTK queue ownership.

2. **MINOR, PRE-EXISTING / NONBLOCKING — GTK retains completed callback contexts, including the new captured flags.**

   **Where:** `Vendor/swift-cross-ui/Sources/GtkBackend/Features/Core.swift:71–81` and `Helpers/ThreadActionContext.swift:2`.

   `runInMainThread` passes a retained `ThreadActionContext` to GLib, reads it with `takeUnretainedValue`, and supplies an empty destroy callback. No balancing release occurs. Consequently the completed callback retains its action and flag indefinitely on GTK, even though the graph children have been released and the flag is correctly ended. This is an existing backend leak, not a cycle introduced by `LifecycleHookChildren`, and does not undo the late-action suppression.

   I extracted the exact scheduling method from the reviewed source and executed it against real GLib with an action-captured weak sentinel. The sentinel remains alive after the action runs and its one-shot source is removed (`gtk-queue-retention.swift`, `gtk-queue-retention.log`). The method is unchanged from the pre-D4 baseline `cdc8e23`; round 2 already noted retained callback-box ownership as outside the change.

   **Suggested follow-up:** balance `passRetained` in GLib's destroy notifier, covering both execution and source cancellation, with a weak-sentinel regression. Do not describe the whole GTK scheduling path as leak-free on the strength of the fake-backend tests. This does not require another D4 lifecycle redesign before merge.

3. **ACCEPTABLE RESIDUAL — Node identity is not data-item identity in `List`/`Table`/index-keyed rows.**

   **Where:** `Vendor/PATCHES.md:121–124`; `Views/List.swift:182–208`; `Views/Table.swift:59–85`; `Views/ForEach.swift:151`.

   The answer to “can an old queued action run after the row is rebound to different logical content?” is **yes**. I reproduced it with actual `List`, actual `Table`, and an index-keyed `ForEach`: replacing row 1 with row 9 before dispatch leaves text `row:9` but delivers `appear:1`, `change:1`, then `change:9`, without disappearance. Subsequent real row removal drops the newly queued change. The unchanged upstream disappearance modifier also keeps its creation-time cleanup action (`disappear:1`).

   This is continuation of one graph-node lifetime, not resurrection of a released node. P8 now explicitly states the limitation. It is acceptable for this app: no CrossUI row has a deferred appear/change resource handler, and its three explicit change handlers are on the persistent wallet root. For future item-owned resources, use keyed `ForEach` or validate the current item/generation in callbacks; supplying `List`'s `id` alone does not change its index-based node ownership. D4 should not silently expand into a collection-identity rewrite.

4. **CLOSED / SAFE FOR CURRENT USES — Lifecycle ordering is candidly documented and does not break the app's hook dependencies.**

   **Where:** `Vendor/PATCHES.md:125–137`; `Sources/CrossUI/WalletRootView.swift:43–60`; `Sources/DashWalletCross/DashWalletCrossApp.swift:229,304`; vendored `TaskModifier.swift:64–79`.

   I searched all application sources, then inspected the CrossUI/composition handlers. `Sources/CrossUI`, `Sources/DashUICross`, and `Sources/DashWalletCross` contain **no explicit `.onAppear`, `.onDisappear`, or framework `.sheet` uses**. MacUI/DashUIMac uses are SwiftUI and do not use these vendored modifiers.

   The three CrossUI `.onChange` handlers record the current sync status, refresh the shell on network changes, and clear an explicit tool page when a route is raised. They read current model state and do not establish resources that disappearance must undo. The `.task` handlers initialize their own fields and load/start models; none depends on appear setup. Examples include transaction filter initialization (`TransactionsScreen.swift:67`), detail label initialization (`TransactionDetailViews.swift:112`), wallet/model loading, and the root's file chooser/setup. The splash deliberately starts network launch separately because setting `session` removes its task-owning view (`DashWalletCrossApp.swift:304`).

   Cleanup without prior appear and task-before-appear are therefore safe for these current uses. Framework `.task` still starts during commit and its internal disappearance cancels the task stored in that node's own state. A recreated task has a different state box; old cleanup does not cancel it.

   My recreation probe nevertheless demonstrates the public shared-resource hazard exactly: `["appear:1", "change:1", "disappear:0"]`, ending with a shared resource inactive. P8 accurately permits this ordering. It is not safe for arbitrary application-global appear/disappear resource pairs without generation-aware cleanup, but upstream already had the new-appear/old-cleanup hazard. No such pair exists in the reviewed CrossUI app.

5. **ACCEPTABLE RESIDUAL — Presented-sheet retention and the extra change bookkeeping layout do not block merge.**

   **Where:** `Views/Modifiers/SheetModifier.swift:153,172–194`; `Views/Modifiers/Handlers/OnChangeModifier.swift:59`; `Vendor/PATCHES.md:138–159`.

   The sheet's dismissal closure retains its children/content graph while the sheet exists. My sheet-capable fake-backend probe releases the host graph with a sheet still presented: the content hooks run, then dismissal releases the content lifetime and schedules disappearance. Programmatic dismissal before the queue runs drops both queued hooks. This is upstream presentation ownership; a still-presented sheet has live content. It is acceptable here, especially since the app uses inline page/confirmation content rather than this framework sheet API. These probes are not a claim that every GTK parent-window destruction path releases every sheet object.

   Updating `previousValue` still publishes legacy `@State` bookkeeping and can cause one extra layout. My probe measured text-layout calls `2 -> 3`, then no further increase on another drain; the action ran once for the committed change. The comparison prevents repeated assignment of the same value, so the extra work does not inherently form a feedback loop. This bounded cost is acceptable for D4. Broad observation scopes and writes during layout remain documented limits; the known stale-ancestor test remains declared, not silently treated as fixed.

6. **CLOSED — Patch regressions are now included in the headless test path; reported evidence checks out.**

   **Where:** `Package.swift:130`; `scripts/linux-docker-test.sh:28`; `Tests/SwiftCrossUIPatchTests/LifecycleTests.swift:65`.

   An independent `DWD_HEADLESS=1 swift package dump-package` confirms the patch-test target depends on framework core and remains in the headless graph. The default Linux script filter includes it. I inspected `final2/linux-docker-test.console`: **565 tests pass**, with the one declared known issue. I did not rerun the entire application headless suite in this focused round.

   The supplied mutation code removes the actual lifetime guard; `r2b-mut-nolifetime.log` reproduces all three removed-view failures. `r2b-full-12.log` records 12 successful runs of all 26 repository patch tests. I also checked the broader accessibility result JSONs: m1 **103**, m2 **179**, ux **242** checks, each with **zero hard failures**. Those broader runs were inspected, not independently repeated here.

7. **CLOSED FOR THE EXERCISED FLOW — Send still settles below the requested idle-demand threshold.**

   I rebuilt `dash-wallet` from the fresh pinned source, using isolated Swift build files and the unchanged existing engine artifact. The real GTK/Xvfb send flow enters recipient/amount, shows and confirms review, and lists the sent transaction. Both independent runs pass **11 checks with zero hard failures**. Their three consecutive two-second idle windows measure **0%, 0%, 1%** and **0%, 0%, 0%** demand respectively, each at or below 25%. Measurement counts main-thread run time plus runnable wait time, without accessibility queries during each sample.

   This is a settling result, not an assertion that every later five-second interval stays below 25%. My first run's additional samples were **0.50%, 8.20%, 59.23%**; the final assertion for that optional extension failed. I retained this evidence and repeated the flow with a longer trace rather than discarding the spike. The app has a 60-second observed sync-overlay recheck (`Sources/WalletFeatures/Main/MainViewModel.swift:55,130`), and P8 retains whole-window layouts. These can produce later bounded work; I have not instrumented the spike enough to attribute it specifically to that timer.

   The repeat's twelve consecutive five-second samples were **0.51%, 0.40%, 0.58%, 0.47%, 0.43%, 0.42%, 0.46%, 51.95%, 0.50%, 0.42%, 0.44%, 0.48%**. The isolated burst is followed by four quiet windows, demonstrating that work stops again. The original sustained send-triggered busy loop is absent in these runs; occasional expensive background updates remain. Logs: `/work/scratch/dw-d4-r3-gpt/send-probe.log` and `send-repeat.log`.

**Summary:** The round-2 lifecycle blocker and headless-test nit are closed. All **42** tests in my final framework harness pass (26 repository tests, five original round-2 probes, eleven round-3 probes), with the single previously declared known issue. The three requested lifecycle regressions pass, the holder does not retain its graph node, and both real send runs meet the three-window ≤25% criterion. The documented row-identity, cleanup-ordering, presented-sheet, and extra-layout limits are acceptable for the current app. Merge D4; track the existing GTK callback-context leak separately.

Reviewed `3a035cd..bdf072ff0c5d45885b8ba2a14a7989949c2c46fa` in fresh detached worktree `/work/scratch/dw-d4-r3-gpt/worktree`. Authoritative test evidence: `final-build.log`, `final-direct-tests.log`, and `mini/Tests/SwiftCrossUIPatchTests/Round3Tests.swift` under that scratch directory. Earlier expanded-probe `final-tests.log` contains a scratch-harness failure from a missing fake padding stub; the corrected final harness passes. Import lint, shell syntax, and diff whitespace checks pass. No macOS/Windows runtime validation or full GTK memory audit was performed. No reviewed-branch edits, commits, pushes, or posts were made. The dedicated mode-700 TMPDIR `/dev/shm/dw-d4-r3-gpt-tmp` was removed after the probes finished.

Final verdict: **APPROVE-WITH-NITS**.

# DW-D4 review r1 (Opus): dashwallet-desktop `dw/crossui-busy-loop`

Scope: `git diff main...HEAD`, 6 commits 72ac19a..cdc8e23 (vendor patches P8, P9 and P10, the
idle check in the demo harness, docs). Reviewed read-only from the worktree
`/work/home/agent/workspace/dw-wt/crossui-busy-loop`.

## What I ran

- **Demo test product.** I built `DashWalletDesktopPackageTests` in `dwd-linux-crossui`, using
  copies of the author's build volumes, and ran `--filter SwiftCrossUIPatchTests`. All 4 P8
  tests pass. Log: `/work/scratch/dw-d4-review/out/test-product.log`.
- **Review harness.** `/work/scratch/dw-d4-review/mini` is a package that depends only on a
  copy of `Vendor/swift-cross-ui`. Its fake backend (`FakeBackend.swift`) is built on the
  vendored `BackendFeatures.BaseStubs`, and it drives a real `ViewGraph`/`ViewGraphNode` and a
  real `WindowReference`:
  - `IntegrationTests.swift` covers P8: nested readers, an ancestor that doesn't read the
    property, an ancestor that drops the descendant in the same batch, a change before the
    flush, changes during layout and during commit, a `.onChange` write, and two windows.
  - `P10Tests.swift` covers P10.

  `run-mini.sh <mutation>` applies one mutation and runs the author's 4 tests plus mine. The
  mutations are `none`, `upstream` (the observe hop calls `viewModelDidChange` directly),
  `nodepth`, `nocurrent`, `nosort` and `noP10`. `repeat.sh` runs the order-sensitive tests
  12 times in separate processes. Logs are in `/work/scratch/dw-d4-review/out/mini-*.log`.
- **Author's runs.** I read the logs in `/work/scratch/crossui-busy-loop/final/`. The m1-B
  failure "sent payment is listed" happened under the old 20 s timeout. The 7-send-done AT-SPI
  dump shows the payment row, so this was timing and not a lost update.
- **Not run.** I did not run the GUI demo suites. Both majors are reproduced with real
  framework code over the fake backend. The GTK behaviour that P10's finding relies on comes
  from `gtk_custom_root_widget.c` (cited below).

## Findings

### MAJOR 1: P8 shallowest-first ordering makes stale ancestors deterministic when layout writes observed state (`.onChange`, `.onAppear`)

**Where:** `Vendor/swift-cross-ui/Sources/SwiftCrossUI/State/ModelObserver.swift:126-128`
(the sort and the loop). The trigger is in
`Views/Modifiers/Handlers/OnChangeModifier.swift:42,44` (`action()` runs inside
`computeLayout`) and `Views/Modifiers/Lifecycle/OnAppearModifier.swift:24` (`action()` runs in
`asWidget`, which is called while the parent lays out).

**Mechanism:**
- `withObservationTracking` installs its observers only after `apply` returns.
- P8 always runs the shallowest affected node first. While that node runs, every ancestor of
  the node whose `.onChange` or `.onAppear` fires is still on the stack, inside its own
  `observe` computation.
- A write from that action therefore notifies none of those ancestors. The ancestors' own
  bodies read the old value earlier in the same pass. Their new trackings are then installed
  on a value that has already changed.
- The ancestors' pending items from the original change are gone: they already ran (they were
  shallowest). Nothing else ever refreshes them, so they stay stale until some unrelated
  change.
- In upstream, the per-observer hops run in registrar order. A deeper node often runs first,
  and its ancestor's still-pending hop then re-reads the new value. Upstream is racy here.
  P8 makes the bad order certain.

**Reproduction:** `onChangeWriteReadByAncestor` and `changeDuringLayout` in
`mini/Tests/P8IntegrationTests/IntegrationTests.swift`. The test view is:
`DerivedRoot { VStack { Text("derived:\(m.derived)"); Watcher } }`, with
`Watcher = Text(m.title).onChange(of: m.title) { m.derived = m.title }`. Then set
`m.title = "B"` and drain.

| Build | Stale result | Correct result |
|---|---|---|
| P8 | 12 of 12 runs: `["derived:A", "w:B"]` while `m.derived == "B"`, and it stays stale | 0 of 12 |
| Upstream (hook reverted) | 3 of 12 runs | 9 of 12 runs: `["derived:B", "w:B"]` |

The layout-time write in `changeDuringLayout` gives the same pattern: P8 is stale 12 of 12,
upstream 1 of 12. Writes during commit stay correct (`changeDuringCommit` passes), which covers
GTK signal handlers fired by widget updates.

**App impact today:** none that I found.
- CrossUI and DashUICross have three `.onChange` modifiers, all in
  `Sources/CrossUI/WalletRootView.swift:43-53`.
- They write `main.syncRates` and `state.page`. Only descendants of the modifiers read those,
  and descendants are laid out after the write.
- There are no `.onAppear` calls.

The hazard is latent, but it lives in the framework core. It breaks a pattern the framework
explicitly supports, and the PATCHES.md claim that "one batch of changes therefore costs one
update of each shallowest affected subtree" doesn't mention it.

**Suggested fix (the cheapest is a, plus c):**
- (a) Patch `OnChangeModifier` and `OnAppearModifier` to defer `action()` until after the update.
  For example, call `backend.runInMainThread` from `commit`, which matches SwiftUI's
  "after the update" semantics. Upstream's own TODO asks "Should this go in computeLayout or
  commit?".
- (b) Alternatively, keep a per-flush "observable write happened while an observe computation
  was on the stack" flag. This can't be detected generically, so (a) is preferable.
- (c) Either way, document in PATCHES.md that writes to observed state during
  body/layout/appear go stale under P8, and add `onChangeWriteReadByAncestor` as a regression
  test.

### MAJOR 2: P10 treats an honoured request as refused, so a window shrunk to its GTK minimum stays clipped instead of snapping back

**Where:**
- `Vendor/swift-cross-ui/Sources/SwiftCrossUI/Scenes/WindowReference.swift:77-82` (the refusal
  detection in the resize handler), `:257-261` (`isRefusedAgain`) and `:300` (where
  `requestedWindowSize` is set).
- The interaction: `Sources/GtkCHelpers/gtk_custom_root_widget.c:49-51` (an allocation equal to
  the preempted size returns before calling the resize callback) and
  `Sources/GtkBackend/Features/CoreWindowing.swift:77` (`setSizeLimits` applies the content
  minimum to the whole window, menu bar included; `CustomRootWidget`'s own minimum is never
  set).

**Mechanism:**
- When GTK honours `setSize(ofWindow:)`, the root widget gets exactly the preempted size and
  **no resize event fires**. `requestedWindowSize` is therefore never cleared after a
  successful request.
- The next resize event of any other size is recorded as a "refusal" of that old request.
- The user can drag the window down to its GTK minimum. That minimum equals the content
  minimum, so the content area ends up one menu-bar height below the content minimum.
- The resulting event (w, min-27) is recorded as `(requested: (w, min), kept: (w, min-27))`.
  The update then clamps to exactly `(w, min)`, so `isRefusedAgain` suppresses the re-request.
- Upstream re-requested `(w, min)` here, and the window snapped back.

**Reproduction:** `userShrinksToWindowMinimum` in `mini/Tests/P8IntegrationTests/P10Tests.swift`
uses the app's real numbers: default size 1100x760 and a content minimum of 787.
1. The first update requests 1100x787. GTK honours it silently.
2. The user drags the window to its minimum, so the content is 1100x760.

| Build | Content after the drag | Requests sent |
|---|---|---|
| With P10 | stays 1100x760 (27 px clipped, no re-request) | 1 |
| P10 reverted (`noP10`) | back to 1100x787 | 2 |

This persists until the window gets another size. A width change re-arms the same trap,
because the new request is again honoured silently. The genuine-refusal case P10 targets does
work: `refusedRequestDoesNotLoop` takes 1 round with P10 and 6 or more without.

**Suggested fix:**
- Make GtkBackend's window minimum include the menu bar: in `setSizeLimits`, use
  `minimumSize.y + menubarHeight(ofWindow:)`. That removes the user path below the content
  minimum, and it also fixes the pre-existing upstream quirk.
- Also make success observable. For example, have the C allocate callback report the first
  allocation after a preempt even when it matches, or clear `requestedWindowSize` when it does.
  Then only a real mismatch counts as a refusal.
- Add the fake-backend `WindowReference` test. PATCHES.md says `WindowReference` can't be
  unit-tested because "the vendored package has no test backend". `BackendFeatures.BaseStubs`
  makes one about 80 lines long (see `FakeBackend.swift`).

### MINOR 3: the P8 unit tests don't fail when P8's wiring is removed

**Where:** `Tests/SwiftCrossUIPatchTests/ModelObserverUpdateQueueTests.swift`. The tests call
`ModelObserverUpdateQueue.enqueue` directly with hand-made depths.

**Mutation results (author's 4 tests):**

| Mutation | Author's tests | Effect |
|---|---|---|
| `upstream`: `observe` calls `viewModelDidChange` directly, so P8 is off in practice | 4/4 pass | P8 disabled |
| `nodepth`: drop `environment.viewGraphDepth = observationDepth` | 4/4 pass | text layouts in the quiet-ancestor case go from 12 to 25 |
| `nocurrent` | 2 fail | |
| `nosort` | 2 fail | |

So only the queue's internals are guarded. The hook in `observe` and the depth propagation
(the parts that touch real view graphs) are covered only by the coarse GUI idle check, and only
in the container demo.

**Fix:** add `ViewGraph` tests over a `BaseStubs` fake backend. Assert the final widget text and
an upper bound on layouts per batch, for example the quiet-ancestor case: P8 does 12 text
layouts, upstream 21 to 44.

### MINOR 4: `check_settles` measures CPU used, not CPU wanted, so it can pass on a loaded host while the main thread is still busy

**Where:** `ci/linux/crossui/atspi_demo.py:118-150`.

**Problem:** the check computes `utime+stime` per wall second and passes at 25 % or less of a
core. On a contended box, a thread that wants 100 % gets only its share. Agentbox ran at load
average 75 to 104 on 32 vCPU during this review, which gives each runnable thread roughly 30 %
of a core or less. A spinning main thread can then read as 21 % and pass. The author's m1-A run
passed on a single 21 % sample.

**Fix:** use `/proc/<pid>/task/<pid>/schedstat`. Its fields 1 and 2 are run time and run-queue
wait. Count (run + wait) / wall time as demand, or sample the task state (`R` vs `S`) several
times per window.

### NIT 5: sheet and alert content nodes get the presenting node's depth, not +1

**Where:** `SheetModifier.commit` (`Views/Modifiers/SheetModifier.swift:98-101`) and the alert
equivalent.

**Problem:** these create content nodes with the commit-time `environment`. That is the
modifier node's `parentEnvironment`, so the content root gets the same depth as the
`SheetModifier` node. Those trees track independently and are laid out from `commit`, so only
the order changes and nothing is dropped. The PATCHES.md text ("takes its depth from … its
parent") is slightly inaccurate.

### NIT 6: the default `observationDepth` makes a forgotten depth silent

**Where:** `ModelObserver.swift:40`.

**Problem:** the protocol-extension default `observationDepth { 0 }` lets any future
`ModelObserver` silently get window depth. `WindowReference` relies on it.

**Fix:** declare it explicitly in `WindowReference`, or drop the default so the compiler forces
a choice.

### NIT 7: the P8 tests run only in the GUI demo harness

**Problem:** `SwiftCrossUIPatchTests` is declared only when not `DWD_HEADLESS`. It runs only in
`container-demo.sh`, not in `scripts/linux-docker-test.sh`. The author's
`linux-docker-test.console` has no P8 suite. That's fine if intended, but the headless Linux
test path never exercises the patch.

## Checked and found sound

- **P8 skip rule.** A pending item is dropped only when the node's `observe` ran again after the
  change. That happens only through `computeLayout`. In every path I traced (`bottomUpUpdate`,
  `onResize` up to `WindowReference.update`, the probing then final passes, Either/Optional
  branch switches, and sheet content laid out in `commit`), the initiator commits the subtree
  it re-laid out.
- **Cached layouts.** A cached `computeLayout` doesn't renew the observation, so that node's
  own update still runs.
- **Removed descendants.** A descendant the ancestor no longer contains is either deallocated
  (`weak` gives nil, which counts as not current) or runs a harmless detached update, as
  upstream does.
- **Tests that pass with P8.** These cover the cases above: `quietAncestorStillUpdatesLeaves`,
  `ancestorDropsDescendantInSameBatch`, `secondChangeBeforeFlush`, `changeDuringCommit` and
  `twoWindowsShareAModel`.
- **Re-entrancy.** `flush` snapshots and clears `pending` and `isFlushScheduled` before running
  items, so a nested flush (for example from an AppKit modal run loop servicing the main queue)
  is consistent.
- **Flush ordering.** GLib idles at equal priority run FIFO, and an idle added during dispatch
  runs on the next iteration. So "the flush runs after the hops already scheduled" holds for
  GTK, and also for `DispatchQueue.main` and the WinUI dispatcher.
- **Across windows.** The queue is global and sorted by depth. Updates in one window can't
  renew another window's nodes, so mixing windows only changes the order and never drops
  anything.
- **P9 CSS guard.** The provider-level `loadedCSS` guard is safe. Every GTK change to a
  provider goes through `loadCss`, a provider belongs to one widget, and identical data parses
  to identical rules. A new widget gets a new provider with `loadedCSS == nil`.
- **Multi-writer providers.** Providers with several writers (Pickers, `GtkCustomButton`, the
  `Widget.css` `didSet`) end with the same last-writer content as upstream.
- **P9 line-limit measurement.** It now uses the label's own context, which `Text.computeLayout`
  styles for the same environment just before measuring. This is the same basis upstream
  already used for the main text measurement, so it is more consistent, not less. Text editors
  keep the shared label.
- **P10 menu-bar measurement.** `gtk_widget_measure` on the `menubar` direct child, with a
  25 px fallback, is fine.
- **Busy-loop fix effect.** In the review harness, a change read only by leaves costs 12 text
  layouts with P8 against 21 to 44 for upstream.

## Verdict

**CHANGES-REQUIRED.** P8 does what it claims for the busy-loop case and holds up for removed
descendants, ancestors that don't read the property, changes before the flush, changes during
commit, and multiple windows. Two problems remain:
- Major 1: shallowest-first ordering turns upstream's intermittent stale-ancestor race (state
  written from layout via `.onChange`/`.onAppear`) into a certain one.
- Major 2: P10's refusal memory misreads honoured requests, which can never be seen on GTK, and
  stops the window from growing back to its content minimum.

Both have small, contained fixes and are reproduced by tests under
`/work/scratch/dw-d4-review/mini`.

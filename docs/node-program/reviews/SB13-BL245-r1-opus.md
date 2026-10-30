# BL-245 r1 review (Opus): dash-spv-ffi last error per thread

Target: `node/bl245-ffi-last-error` @ `9a4d6aeee` (on 62adfd74e), worktree /work/scratch/bl245-r1-opus/wt.

## Verdict: CHANGES (small)

The core fix is right. The thread-local slot closes the cross-thread use-after-free: ASan flags the old code and is clean on the new code. Every setter runs synchronously on the caller's thread, so no error is lost. The tests pin the regression and fail deterministically on the global slot. Two things need fixing before merge, each a few lines:

- **F1:** the documented pointer lifetime is wrong in one case, thread exit, and following the doc literally gives a use-after-free.
- **F2:** any FFI error call made during thread teardown now aborts the process. Before this change it did not.

## Findings

### F1 (required): the lifetime contract omits thread exit and cross-thread hand-off

**Where:** `dash-spv-ffi/src/error.rs:43-45`, `src/platform_integration.rs:19-21`, `README.md:110-113`, `FFI_API.md:590`

All four say the string "stays valid until the calling thread makes another call that sets or clears its error". That is not true. The `CString` lives in that thread's TLS, so it is also freed when the thread exits. It can also be freed by a later call on that thread that the reader does not know about.

`FFIResult` is returned by value, which makes this easy to hit. A caller can pass it to a completion handler on another queue or thread. Nothing in the docs warns against that.

Call sequence (ASan-confirmed, `mini/examples/thread_exit.rs`):

1. Worker thread W calls a failing entry point, for example `ffi_dash_spv_get_quorum_public_key(NULL, ...)`. It gets back an `FFIResult r` whose `r.error_message` points into W's TLS.
2. W sends `r` to the main thread (Swift: `DispatchQueue.main.async { ... r.error_message ... }`), then W exits or is retired from the pool.
3. W's TLS destructor drops the `CString`.
4. Main runs `CStr::from_ptr(r.error_message)` / `String(cString:)`. ASan reports: `heap-use-after-free`.

The old global slot happened to survive thread exit. It was broken in the other direction.

There is a variant: W stays alive but runs another failing call before main reads the pointer. That is "this thread's next call", but the reader on main cannot see it happen.

The same applies to errors set from inside event callbacks. Those run on tokio `dash-spv-worker` threads, whose slots are freed when `dash_spv_ffi_client_destroy` drops the runtime.

**Fix:** reword all four places along these lines: "valid only on the calling thread, until that thread makes another call that sets or clears its error or exits; copy it before calling into the library again or passing it to another thread."

Optionally add a Swift note to the README: copy the message before any `await`. A Swift async function can resume on a different thread, which then reads another task's error or null. That is the errno caveat. There is no Swift consumer in the workspace today.

### F2 (required, robustness): TLS access during thread teardown aborts the process

**Where:** `dash-spv-ffi/src/error.rs:31`, `:36`, `:48`

All three use `LAST_ERROR.with(..)`. After the slot's TLS destructor has run, `with` panics with `AccessError`. The panic cannot unwind out of an `extern "C"` fn, so the process aborts with `fatal runtime error: failed to initiate panic ... aborting`, exit 134.

On glibc, Rust and C++ TLS destructors run before pthread TSD destructors. On Darwin, the order of Rust TLS destructors relative to other TSD keys, such as ObjC autorelease pools, is not specified. A cleanup hook that calls into the library at thread exit is therefore reachable. Even `dash_spv_ffi_get_last_error()` alone aborts.

Call sequence (`mini/examples/tls_teardown.rs` and `tls_teardown_get.rs`, stable 1.99 and nightly):

1. Thread T makes any failing call, so its `LAST_ERROR` destructor is registered.
2. T registers a pthread TSD destructor and exits.
3. The Rust TLS destructor frees the slot.
4. The TSD destructor calls a failing entry point (the `null_check!` path or `FFIResult::error`) or `dash_spv_ffi_get_last_error()`. The process aborts.

The old code returned normally here (exit 0). This is not memory-unsafe, but it is a new crash mode that the change introduces.

**Fix:** use `LAST_ERROR.try_with(..)` everywhere. If the setter or `clear_last_error` fails, drop the error silently. If the getter fails, return `ptr::null()`. The cost is nil.

### F3 (nit): tests do not cover same-thread invalidation or thread exit

The two new tests do pin the bug:

- **Global slot:** I ran both against a copy of the old storage (`mini --features old`). They fail deterministically. `test_concurrent_error_handling` reads "Error from thread 9" on every thread, and the second test's spawned thread sees the caller's error.
- **Pointer re-allocated per read:** the `get() == ptr` assert in the second test would fail.

Two cases are not covered:

- A same-thread unrelated call between `get` and the read.
- The thread-exit lifetime.

Neither can be tested for UAF without a sanitizer. I'm not asking for more tests. If F2 is fixed, a small test is cheap: a TSD or `thread_local` destructor that calls `dash_spv_ffi_get_last_error()` and expects null.

### F4 (FYI, pre-existing, not this PR)

- `key-wallet-ffi`'s `FFIResult.error_message` is `*mut`, owned, and freed by the caller (`key-wallet-ffi/src/types.rs:152`). This crate's `FFIResult.error_message` is a borrowed alias of the TLS slot. A caller who knows the key-wallet convention and frees it here gets an invalid free. The new field doc ("Owned by the library") helps. A rename or a header note would help more.
- No production entry point clears the error on success: `handle_error*` and `ffi_result!` are unused outside tests. The error therefore lingers after later successful calls. The new doc wording ("last failed call") is accurate, and this does not affect pointer lifetime.

## Answers to the brief's questions

**1. Is it now memory-safe? Can any handed-out pointer dangle?**

- **Within the stated "same thread, before the next set or clear" use: yes, it is safe.**
- **Same-thread re-entrancy:** the `RefCell` borrows only span the assignment, the `CString` drop (`free`) and `as_ptr`. No user code or callback can run while a borrow is held, so `BorrowMutError` is impossible.
- **Callbacks:** callbacks are dispatched from spawned tasks on tokio worker threads, never inside a borrow. A callback that fails an FFI call sets the worker's own slot. The callback strings (`on_client_error` and similar) are separate `CString`s, not the TLS slot.
- **Thread exit:** pointers do dangle at thread exit (F1), and teardown-time calls abort (F2).
- **Async tasks:** no setter runs inside a future. All setters are after `block_on` or in synchronous bodies, so Rust never touches the slot from a migrating task. The caller-side Swift `await` hop is covered in the F1 doc note.

**2. Do any setters run off the caller's thread, so their errors are now lost?**

None. I checked every setter site:

| File | Lines |
|---|---|
| `client.rs` | 65, 106, 134, 180, 218, 245, 259, 310, 342 |
| `config.rs` | 85, 127, 141, 158, 163, 192, 377, 399 |
| `utils.rs` | 40, 48, 63, 86 |
| `platform_integration.rs` | 27 (`FFIResult::error`) |
| `error.rs` | `null_check!` / `ffi_result!` / `handle_error*` |

All run synchronously in the `extern "C"` body on the caller's thread. Errors from background tasks never went through `LAST_ERROR`; they use `on_client_error`.

**3. Does `FFIResult.error_message` alias the TLS string, and is the lifetime contract documented where users will read it?**

- It aliases the TLS string: `FFIResult::error` sets the error and then returns `dash_spv_ffi_get_last_error()`.
- The contract is documented in all the right places. cbindgen emits the doc comments into the generated header, and README and FFI_API.md carry it too. I regenerated FFI_API.md in a scratch copy and it is identical.
- The contract text needs the F1 correction.

**4. Do the tests pin the fix?**

- Yes for the global-slot regression (see F3).
- Partly for "freed on the next unrelated call": per-read re-allocation is caught, same-thread invalidation is not tested.

**5. Is BREAKING CHANGE the right framing? Did any caller rely on cross-thread visibility?**

The framing is right. No caller relied on cross-thread visibility. I grepped every repo in `~/workspace` for `dash_spv_ffi_get_last_error`, `ffi_dash_spv_get_quorum_public_key` and `ffi_dash_spv_get_platform_activation_height`. Nothing outside the rust-dashcore copies uses them. Inside the repo:

- `dash-spv-cli/src/bin/dash_spv_ffi.rs` reads on the same thread as the call.
- `contrib/node/spv-feature-off` reads synchronously in the same test thread.
- The `dash-spv-ffi` tests do the same, including `test_thread_safety_concurrent_access`, which only checks error codes.

The `!` is reasonable for an observable semantic change.

## What I ran

- `git diff 62adfd74e 9a4d6aeee`, and a full read of `error.rs`, `platform_integration.rs`, the `client.rs` / `config.rs` / `utils.rs` setter sites, and the affected tests.
- `systemd-run --user --unit=bl245-review-tests` with `CARGO_TARGET_DIR=/work/scratch/bl245-r1-opus/target` and `-j 8`:
  - `cargo test -p dash-spv-ffi --lib -- error`: 10 passed.
  - `--test test_error --test test_platform_integration_safety --test test_platform_integration_minimal`: 2 + 9 + 1 passed.
  - Logs in `/work/scratch/bl245-r1-opus/logs/`.
- The full lib test binary: 50 passed, 4 ignored.
- `test_error_handling::tests::test_*` looped 100 times: 0 failures.
- A standalone copy of the old and new storage in `/work/scratch/bl245-r1-opus/mini`, with the PR's two tests copied verbatim:
  - New storage: 2/2 pass, plus 200 more loops all pass.
  - Old storage (`--features old`): both fail.
- ASan (`RUSTFLAGS=-Zsanitizer=address`, nightly) on the PR tests and three probes:

  | Probe | Old storage | New storage |
  |---|---|---|
  | PR tests | not run | clean |
  | `cross_thread`: the BL-245 bug | heap-use-after-free | clean |
  | `thread_exit`: F1 | clean | heap-use-after-free |
  | `tls_teardown` / `tls_teardown_get`: F2 | exits 0 | aborts, exit 134 (also on stable 1.99) |

- `python3 scripts/generate_ffi_docs.py` in a scratch copy: FFI_API.md is up to date.
- Miri is not installed for nightly. I did not install it, because ASan covered the UAF questions.

Nothing in the worktree was edited, committed or pushed. Scratch for this review: `mini/`, `mini-target*/`, `docgen/`, `logs/`, `target/` under /work/scratch/bl245-r1-opus.

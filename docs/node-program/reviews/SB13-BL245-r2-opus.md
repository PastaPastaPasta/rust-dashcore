# BL-245 r2 review (Opus): dash-spv-ffi last error per thread, round 2

Range: `9a4d6aeee..1ad9314a6`, one commit (signed, `G`), 5 files, +57/−13. Worktree /work/scratch/bl245-r1-opus/wt, detached at 1ad9314a6.

## Verdict: ACCEPT WITH NITS

F1 and F2 are fixed. All three `LAST_ERROR` accesses now use `try_with`, and the corrected lifetime wording appears in all four places, plus the cbindgen header. The new test fails on the old code: it aborts on 9a4d6aeee, and it kills every mutant I tried on the three accesses. The teardown probes from round 1 now exit 0, under ASan too. Nothing in the diff goes beyond the stated intent. The nits below are optional.

## 1. F1 and F2

**F2: fixed.** `grep LAST_ERROR dash-spv-ffi/src` finds exactly three accesses, all `try_with`:

| Location | Access | On teardown |
|---|---|---|
| `error.rs:35` | `set_last_error` | `let _ =` drops the error |
| `error.rs:40` | `clear_last_error` | `let _ =`, a no-op |
| `error.rs:54-56` | getter | `.unwrap_or(std::ptr::null())` |

There are no other `.with(` calls in `src/`. `FFIResult::error` (`platform_integration.rs:28-34`) goes through both functions, so it is covered too.

**F1: fixed.** Each place now says the pointer is valid only on the calling thread, until that thread sets or clears its error or exits, and that the caller must copy it before handing it to another thread:

- `error.rs:47-51`, the getter doc.
- `error.rs:10`, the thread-local comment: "or freed when it exits".
- `platform_integration.rs:19-22`, the `FFIResult.error_message` field doc.
- `README.md:110-114`. It adds "The same holds for `FFIResult.error_message`", and the Swift `await` note at `:116-117`.
- `FFI_API.md:590`.

**Generated outputs match.**

- **Header:** cbindgen writes the header to `target/<profile>/include/dash-spv-ffi/dash-spv-ffi.h`, which is not committed. I checked the freshly built header: it has the new text on both the `dash_spv_ffi_get_last_error` decl (lines 1190-1201) and `FFIResult.error_message` (lines 825-831).
- **FFI_API.md:** it is generated from the doc comments. I re-ran `scripts/generate_ffi_docs.py` on a scratch copy of the 1ad9314a6 `src/`, and the output is byte-identical to the committed file.

## 2. Does the new test exercise teardown?

**Yes, on every platform std supports today.** Each access is tested by an abort or by the assertion. I copied the test verbatim into `mini/src/r2_tests.rs` (checked with `diff`) and ran it against each storage and against four mutants:

| Storage / mutant | Result |
|---|---|
| 1ad9314a6 (`--features r2`) | pass |
| 9a4d6aeee (`with`) | abort, "thread local panicked on drop" |
| setter back to `with` | abort |
| `clear_last_error` back to `with` | abort |
| getter `.unwrap()` instead of `.unwrap_or(null)` | abort, AccessError |
| getter returns non-null on `Err` | assertion fails |

The last row answers the question about clearing before reading. The `is_null()` assertion is not vacuous: it is the only thing that catches a getter returning garbage from the `Err` branch. The three `with` regressions are caught by the abort, not by the assertion.

**Destructor order.** The test comment hedges for a platform that runs TLS destructors in registration order (FIFO). No std platform does:

- On Linux and Android, std registers through `__cxa_thread_atexit_impl`, which runs LIFO.
- Everywhere else (Apple, Windows, and the fallback), std uses `sys/thread_local/destructors/list.rs`, whose `run()` loops on `dtors.pop()`, also LIFO.

I confirmed the order with the probe `mini/examples/tls_dtor_order.rs`: a guard registered *before* `LAST_ERROR` sees the slot destroyed, and one registered *after* sees it alive. So the guard really does run after the slot's destructor, and the test hits the `Err` path. On a FIFO platform it would degrade to a vacuous pass rather than a false failure, which is acceptable.

## Findings

### N1 (nit, docs): `FFIResult.error_message` "null on success" now also means null on failure in teardown

`platform_integration.rs:19-20`. The doc says the field is "null on success". With `try_with`, `FFIResult::error` called after the slot is destroyed returns `error_code != 0` with `error_message == NULL`.

Scenario:

1. A pthread or ObjC cleanup hook calls `ffi_dash_spv_get_quorum_public_key(NULL, ...)` while its thread exits.
2. It gets back `{error_code: 1, error_message: NULL}`.
3. Swift code that trusts the doc runs `String(cString: r.error_message!)` and crashes on the null.

Before this round, this case aborted; on the old global storage it got a non-null pointer. The getter's own doc already says it returns null during teardown, so this is only the field doc lagging behind. Suggested wording: "null on success (and on a failure during thread teardown)". Optional.

### N2 (FYI, not introduced here): a first-ever failure inside a TSD destructor leaks about 42 bytes

Probe: `mini/examples/tls_teardown_untouched.rs`. Scenario:

1. A thread never touches `LAST_ERROR` while it runs.
2. A pthread TSD destructor on that thread calls a failing entry point at exit.
3. The slot is lazily initialised at that point, so `try_with` succeeds and the getter returns non-null. This is consistent with the doc's qualifier "once the thread's error slot is destroyed".
4. glibc registers the slot's destructor after its TLS destructor pass has already run, so the destructor never runs. LSan reports two leaks: the 10-byte `CString` and glibc's 32-byte dtor-list node.

This is the same on 9a4d6aeee (`with` behaves identically for an uninitialised slot). It is bounded to one string per such thread and needs an unusual caller. No change needed. Darwin's behaviour for a TLV registered this late is untested here.

### N3 (nit, test robustness): `receiver.recv()` blocks forever if TLS destructors never run

`tests/unit/test_error_handling.rs:102`. On a target where thread-local destructors never run (std leaks them on wasm, for example), the `Sender` would never be dropped, so the test would hang instead of failing. That is irrelevant to the platforms this crate targets. `recv_timeout` would make it fail instead of hang. Optional.

### N4 (nit, wording): the Swift note gives only half the reason

`README.md:116-117`. "Copy ... before any `await`" is the right instruction. The reason given, "would read that thread's error instead", only covers calling `dash_spv_ffi_get_last_error()` after the `await`. The other half: a pointer obtained before the `await` can also be freed by the original thread meanwhile, by a later call there or by the thread exiting. Optional.

## 3. ASan teardown probe with the fixed storage

`mini/src/lib.rs` now has a `--features r2` variant with the 1ad9314a6 storage (`try_with` everywhere). Results:

| Probe | 9a4d6aeee | 1ad9314a6 (r2) | old global |
|---|---|---|---|
| `tls_teardown` (failing call in TSD dtor) | exit 134 | **exit 0**, ptr null | exit 0 |
| `tls_teardown_get` (getter only in TSD dtor) | exit 134 | **exit 0**, ptr null | exit 0 |
| `tls_dtor_order` (Rust TLS dtors on both sides) | exit 134 | **exit 0** | exit 0 |
| `tls_teardown_untouched` (N2) | exit 0 | exit 0 (LSan: 42 B leak) | exit 0 |

Under nightly ASan with r2:

- `tls_teardown`, `tls_teardown_get`, `tls_dtor_order` and `cross_thread`: clean, exit 0.
- The mini tests (the PR's 2 plus the new one): clean, 3 passed.
- `thread_exit`: still reports heap-use-after-free. That is expected. It is the misuse F1 documents (reading another thread's pointer after that thread exits), and the docs now forbid it.

## 4. Scope

The diff touches only the three accesses and their doc comments, the `FFIResult` field doc, README, FFI_API.md, and the one new test. There are no signature, behaviour or dependency changes. The commit message matches the diff. The new test needs no `#[serial]`: it touches only a fresh thread's slot.

## What I ran

- `git diff 9a4d6aeee 1ad9314a6`, a full read of `error.rs` and `platform_integration.rs` at 1ad9314a6, and `grep LAST_ERROR` / `.with(` over `src/`.
- In unit `bl245-r2-test`, with `CARGO_TARGET_DIR=/work/scratch/bl245-r1-opus/target`, `-j8` and no sccache:
  - `cargo test -p dash-spv-ffi --lib -- test_error_handling`: 9 passed.
  - `cargo test -p dash-spv-ffi --lib`: 51 passed, 4 ignored.
  - The new test run 100× with `--exact`: 0 failures.
  - The whole `test_error_handling` module run 100× at `--test-threads=16`: 0 failures.
  - `cargo fmt --check -p dash-spv-ffi`: clean.
  - Logs: `logs/r2-lib-error.log`, `logs/r2-lib-full.log`.
- The generated header: grepped `target/debug/include/dash-spv-ffi/dash-spv-ffi.h`. I re-ran the FFI_API.md generator on a scratch copy (`docgen/`): output identical.
- In mini, on stable 1.99:
  - Four teardown probes against each of the three storages: `logs/r2-mini.log`.
  - The new test, verbatim, against each storage: `logs/r2-mini-test.log`.
  - Four mutants of the r2 storage: `logs/r2-mutants.log`.
- In mini, on nightly with `-Zsanitizer=address` and `detect_leaks=1`: six probes and all tests on r2 (`logs/r2-mini-asan.log`).
- I did not re-run clippy or GATE-FAST: the code change is three `try_with` lines and fmt is clean.

Nothing in ~/workspace/rust-dashcore was edited, committed or pushed. Scratch additions: `mini/` gained the `r2` feature, `src/r2_tests.rs`, and the examples `tls_teardown_untouched.rs` and `tls_dtor_order.rs`. The worktree and the target, mini-target and mini-target-asan dirs are removed after this review.

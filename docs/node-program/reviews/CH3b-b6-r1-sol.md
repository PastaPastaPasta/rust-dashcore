# CH3b b6 (BL-453) — Sol round 1 review

Verdict: **ACCEPT**

Findings: **0 M, 0 m, 0 n**

I found no correctness, liveness, test-quality, lint, or benchmark-gating issue.

The replacement keeps the original operation order: until, bell, lane 0, lane 1. Crossbeam 0.5.17's `select!` expands to the unbiased `internal::select` with `_IS_BIASED = false`; its `run_select` shuffles the operation handles exactly as `Select::select` does. The selected receive is completed by the macro before `Selected` is returned. `never()` is the non-ready flavor and has no allocation. A lane is marked dead only after its selected receive returns `Err`, which means the channel is disconnected and empty; queued messages are received first. A disconnected bell is consumed as `Err` and removed, while a queued bell still returns `Bell`. The until receiver remains the first arm and is handled through the existing `Until::received` logic, preserving barrier and closed-wake behavior and the existing deadline/wake semantics. No lost-wakeup path was found.

The two tests are deterministic for the properties asserted. The closed-lane/bell test must take the two initially disconnected operations and then the delayed lane close, yielding exactly three selects; the alloc-tag test establishes a parked warm-up and checks zero events for immediate, parked-lane, bell, and waiter wake selections. I ran the alloc-tag test repeatedly without a failure.

The clippy deviation is effective. The `crossbeam_channel::Select` type and its direct `select`/`select_timeout` paths are rejected; `select!` and `select_biased!` receive arms are rejected through the existing receiver bans, send arms through `internal::select`, and timeout arms through `internal::select_timeout`. Only V's function-level expectation permits the production macro expansion.

The depth-1 gate change is consistent with D215's reported exact repetition after removing the per-reply heap allocation; retaining only `page_faults` as ungated matches the documented 0 spread for allocations and bytes.

Validation performed in reflink copy `/work/scratch/ch3b6-rv-sol1/`:

- `cargo clippy -p dashd-chainstate --all-targets -- -D warnings` — pass.
- `cargo clippy -p dashd-chainstate --all-targets --features alloc-tag -- -D warnings` — pass.
- `cargo test -p dashd-chainstate --lib` — 749 passed.
- `cargo test -p dashd-chainstate --features alloc-tag --lib chain::actor::tests::vs_lane_select_allocates_nothing -- --exact` — pass, repeated 10 times.

Review path: `/work/home/agent/workspace/node-program/reviews/CH3b-b6-r1-sol.md`

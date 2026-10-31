# SB1 layer gates — Sol round 9 (DEC-188 am. 1)

Reviewed detached HEAD `979e77f02127b1583002d94ce1d1fce5d87d15c1`, the four commits after `b94d73c88`, against DEC-188 and DEC-188 am. 1 items (1)–(3). The prior r6–r8 Sol reports were read for the allocator-ownership history. This review does not reopen the native or gate-harness issues.

**Result: APPROVE-WITH-NITS.** I found no false pass on a registered product's real build through an allow-listed option. The r8 joined-extern escape is closed: both joined and split unmanaged `--extern` forms are rejected before either canary can certify ownership. The three unplanted real `dash-spv-bench` rows pass, including the heap-profile `allocator="own"` row. Therefore no finding meets DEC-188(5)'s blocking threshold.

## Code review

`tokens()` is the one parser used for the product's captured unit, every other unit recorded by the wrapper, and the build-script arguments synthesized from Cargo messages (`check-products.py:273, 340, 390`). It normalizes short joined/split, long joined/split and aliases into the same option/value representation. The `SHORT`/`LONG` table matches this machine's `rustc 1.98.1 --help -v` in the nine-test suite. Whole `@argfile` arguments, including an option's next value, and `--` are refused. Off-list options retain the source in `refusals()` diagnostics. An `@` embedded inside a joined `--cfg` value is not a whole argv argument; rustc itself rejects the tested invalid `plant@value`. This is a conservative boundary, not an argfile escape.

The per-option checks at `check-products.py:351` enforce Cargo artifact ownership by device/inode for `--extern`, allow only `native`/`framework` and Cargo profile `deps/` search paths for `-L`, normalize `-C` underscores before checking each key, require the row triple for `--target`, and refuse any `=` output path in `--emit`. Source-level `--cfg` selected an allocator in a separate dependency in both spellings; the exact gate rejected the declared-own registered fixture. A cfg-selected allocator in the product's own package passed, as intended. `--crate-type=staticlib` and `--crate-type=rlib` mixed into a registered bin's rustflags made its real compile fail, so neither produced a gate pass. Rustc's printed cfg set was identical for ordinary bin and rlib types in this compiler probe.

## Verification

- `python3 -I contrib/node/test-check-products.py`: **9/9 pass**.
- Targeted, verbatim selftest plants from `selftest-checks.sh:1127–1155`: joined and split unmanaged `--extern` rejected; `--remap-path-prefix` and `--sysroot` rejected with the option and product source; valid joined/split allow-listed flags passed; embedded Python test passed. **6/6 pass**. The targeted extraction used the original fixture helpers and assertions, but did not run the rest of the large selftest script, which includes unrelated checks and fixture commits.
- `python3 contrib/node/check-products.py dash-spv-bench`: **3/3 real rows pass**, overall exit 0. Default and cpu-profile link the canary (133 s, 98 s); heap-profile reports its own package's allocator only (142 s).
- Independent option plants: unmanaged `--extern` files in and outside `deps/` refused in both spellings; a Cargo-owned file accepted; `-L dependency=` outside profile `deps/` refused in both spellings, the real profile path accepted; `-Clink_arg` and `-Cextra_filename` accepted, `-Clink_args` and unknown `-Cpasses` refused; split, joined and mixed `--emit` paths refused, pathless kinds accepted; whole and value-position `@argfile` refused; off-list sysroot/remap refused. See `option-plants.log`.
- Independent registered-fixture plants: joined and split `--cfg plant` selected a foreign allocator and were rejected; an own allocator selected by `--cfg` passed; a whole `@argfile` rustflag was rejected. See `own-plants.log`.

### Mutation table

Each mutation ran the same nine tests in isolation. “Rule → any” removes only the named special check; the count is failing assertions/subtests, not affected test methods.

| Disabled rule | Tests catch it | Failing assertions |
| --- | --- | ---: |
| `--extern` artifact/pathless identity → any | yes | 6 |
| `-L` search-kind/path rule → any | yes | 4 |
| `-C` key allow-list → any | yes | 4 |
| `--target` row-triple rule → any | yes | 1 |
| `--emit` no-path rule → any | yes | 2 |
| off-list `--sysroot` admitted as any | yes | 4 |

Removing each of the **18 named allowed options** from `ALLOWED` independently also failed the positive spelling test (1–24 assertions each). Removing the pseudo-option `""` for a source input produced zero test failures; it would make real product rows fail closed. This is a small test-completeness nit, not a false pass. Logs: `mutations.log`, `membership-mutations.log`.

## Nonblocking suggestions

1. Add a positive assertion that the source-input pseudo-option remains accepted, so every `ALLOWED` entry is mutation-covered. It is not itself a rustc option; losing it would only reject valid rows.
2. An `--emit=link=/path` rustflag made Cargo's target-information query fail before `check()` ran; on a second fixture after reset, the gate exited nonzero with a `cargo clean` traceback rather than its source-labelled “an output path” diagnostic. It still fails closed. Consider turning these early Cargo failures into a concise diagnostic if this input is expected to be common. `crate-type-plants.log` records the exact output.

All review jobs used `nice -n 10`, `CARGO_BUILD_JOBS=12`, Rust 1.98.1, and a private mode-0700 `TMPDIR` and Cargo target under `/work/scratch/sb1-r9-sol/`. The full product run used a user systemd unit because of box load. No Android/Apple builds, full selftest, commits, pushes, GitHub actions, branch edits or implementer-worktree writes were performed. The gate-harness wrapper-directory issue is outside this review.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes

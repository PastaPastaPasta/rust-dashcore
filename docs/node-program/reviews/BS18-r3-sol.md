# BS18 independent review, round 3 (Sol)

Scope: the single `G`-signed commit `7d93d56d25735f7d78eb653d995ccc427e36f107` on `c2774829508393e403cf2cea2aa8a74ef9848af0`. I reviewed it in a detached scratch worktree without changing the implementer's worktree. Core reference: peeled `v24.0.0-rc.3` at `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

## Round-2 Low closure

**Closed.** `dash/src/bip152.rs:294-303` now rejects an absolute index above `u16::MAX` before encoding that differential. Core v24's `BlockTransactionsRequest::indexes` is `std::vector<uint16_t>` (`src/blockencodings.h:44-52`). Its `DifferenceFormatter::Unser` adds each CompactSize differential to the running shift and rejects an absolute index outside `uint16_t` (`src/blockencodings.h:33-40`). The Rust encoder's accepted values are strictly increasing in `0..=65_535`; their differentials reproduce exactly those values in Core, with no arithmetic overflow. The empty request is accepted. A request of 65,536 indexes can end at 65,535 and is valid; a longer strictly increasing request cannot fit. Values above 65,535 cannot inhabit Core's request type and Core rejects corresponding wire differentials. Decreasing or duplicate Rust indexes still fail the encoder, as Core's serializer does; no nonnegative differential can encode such a sequence.

An independent temporary test verified exact bytes and both Rust decode entry points for `[]`, `[65_534, 65_535]`, and `[0, 65_535]`. It also verified rejection of `[65_536]`, `[0, 65_536]`, and `[65_535, 65_536]`. The committed test covers `[65_535]` round-tripping and four invalid upper-bound cases. The edited old test retains its decreasing `[1, 0]` and `[0, 3, 2]` cases and duplicate `[5, 5]` case (`dash/src/bip152.rs:551-574`).

## New findings and caller caveat

No new blocking finding. The error is **not** atomic: `consensus_encode` writes the 32-byte hash, count, and any preceding differentials before checking a later invalid index (`dash/src/bip152.rs:290-303`). The independent test observed a 33-byte prefix for `[65_536]` and a 34-byte prefix for `[0, 65_536]`; callers using a persistent sink must discard it after `Err`. This also applied to the preexisting decreasing/duplicate error path. The convenience `serialize` helper panics on any encoder error, including this one (`dash/src/consensus/encode.rs:219-223`); callers handling untrusted constructed indexes should use the fallible encoder. These are API caveats, not a reason to block the bound fix.

No new security-relevant Core issue was found.

## Mutation checks

Each mutant was applied alone in the detached worktree, then restored; the review-only boundary test was removed. `git diff --exit-code` passed after restoration.

| Mutant | Result |
|---|---|
| Restore `*idx == u64::MAX` | **Killed:** committed `getblocktxn_encoding_refuses_indexes_core_cannot_represent` fails because `[65_536]` succeeds. |
| Change `> u16::MAX` to `>= u16::MAX` | **Killed:** independent boundary test fails because valid 65,535 is rejected. |

## Validation

- `cargo test -p dashcore`: passed, including 568 library tests and 13/13 `core_v24_protocol_tables` tests; integration and doctests passed (with expected ignores).
- Independent boundary and partial-write test: passed before mutation. Committed upper-bound test passed again after source restoration.
- `cargo clippy -p dashcore --lib -- -D warnings`: passed.
- `cargo clippy -p dashcore --all-targets -- -D warnings`: stopped on unused `serde_derive::{Deserialize, Serialize}` imports at `dash/src/hash_types.rs:352`, unchanged from parent `c27748295`; this is unrelated to the reviewed commit.
- `cargo fmt --all -- --check`: passed. Scratch worktree was clean after mutations.

**Verdict:** APPROVE
Sol gate satisfied: yes

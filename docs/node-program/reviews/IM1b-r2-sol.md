# IM1b r2 review — `eeb59b911`

I reviewed `git diff ddbac4a76 eeb59b911` in a detached scratch worktree. Assumptions: the 8 MiB guard is judged against current mainnet/testnet Core-written lists, and the secure key-write guarantee is the Unix case described in the round-one review. All four commits show valid signatures (`%G? = G`).

## Findings

None.

## Round-one closure

- **Major (closed), `dash-dml/src/list.rs:633-667`:** A new key in an existing collision bucket now uses `push_front` without scanning or copying the bucket. Updates remove the old occurrence and move it to the front; removals preserve survivor order. `Arc::make_mut` retains copy-on-write isolation for cloned lists. I traced `core_order`, snapshot trailers, diff trailers, `is_equal`, canonical hashing, and the `try_add`/`update`/`remove` failure paths: the representation change does not change their observable order or content. The existing mutation and codec tests cover bucket order; a temporary clone test also passed with the parent order unchanged, the child order updated, both invariant checks passing, and `is_equal` unchanged. The temporary test was removed.
- **Minor (closed), `dash-coreimport/src/keys.rs:88-103`:** The reader allocates `limit + 1` bytes once, reads directly into that buffer, and wipes it on error or drop. `Secret`'s `Debug` is redacted. A revert-equivalent `read_to_end` mutation failed `a_reader_that_delivers_more_than_expected_never_moves_the_buffer` on a changed read-buffer address.
- **Minor (closed), `dash-coreimport/src/keys.rs:322-344`:** The writer distinguishes a failed install from a linked key with failed cleanup or directory sync, retains both failure details, and tolerates a missing temporary name. Mutating it to swallow those two errors made four of the nine fault-injection tests fail, including both installed-key partial outcomes.
- **Cap (verified), `dash-coreimport/src/netstate/governance.rs:397-405`:** The 8 MiB check occurs before the unbudgeted list decoder. Removing it made `a_masternode_list_over_its_cap_is_a_bad_file` fail. The real Core testnet `governance.dat` fixture is 136,219 bytes *in total*; the supplied mainnet fixture has an empty list. The repository's current-mainnet estimate is about 3,900 masternodes at about 400 bytes each, around 1.6 MiB. These support the cap for current networks, although 8 MiB is a deliberate compatibility limit rather than a Core-format maximum.

`decode_list_s3` still has no internal budget. That remains suitable for the EV backlog: this import call is capped, and the other current call sites are the oracle/fuzz and tests. A future product path accepting arbitrary Core-disk list bytes should give the decoder its own budget.

## Checks

- `dash-dml --test same_prefix_scaling --all-features`: passed. Copying the bucket on every set made it fail at 11.7× requested allocations for 4× the keys; restored source passed again.
- `dash-coreimport --all-features --lib keys::tests`: 9 passed. The revert-equivalent reader and writer mutations failed as described above.
- `dash-coreimport --all-features --test flatfiles a_masternode_list_over_its_cap_is_a_bad_file`: passed; failed when the cap was removed.
- `dash-coreimport --all-features --test flatfiles_memory`: passed. The same-prefix 4× case used 4.34× requested allocations and 2.93× peak live bytes; the largest tested list expansion was 143.98 requested bytes per list byte, below the stated 150 bound.
- `git diff --check ddbac4a76 eeb59b911`: clean. The detached worktree was restored to a clean `eeb59b911` after mutation probes. I did not rerun the full gate or Core oracle suite; the brief reports those as passing, with the unrelated wall-clock selftest passing alone.

VERDICT: ACCEPT

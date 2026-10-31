# DW-E0-07 review, round 2 (Opus)

**Branch:** `dw/e0-07-appdb-migrations` (worktree `/work/home/agent/workspace/dw-wt/e0-07-appdb-migrations`)
**New commits:** fb21711 (schema fixes), 8ff2938 (a bad `avatars` entry is non-fatal), ab7b988 (fmt), 3eb4c1f (docs). They sit on 01e70d3 and 830a565, which round 1 reviewed.
**Round 1:** `reviews/DW-E0-07-r1-opus.md`
**Reviewer contract:** `briefs/REVIEW-COMMON.md`. I only read the branch. The worktree was clean before and after the review.

## What I ran

All tests ran with `TMPDIR=/dev/shm/e0-07-r2-tmp` (mode 0700).

| Command | Result |
|---|---|
| `cargo test -p dw-appdb` | 26 passed, 0 failed |
| `cargo test -p dw-engine --test umask_002 --test session_lifecycle` | 1 + 9 passed |
| `cargo test -p dw-engine --lib session` | 2 passed |
| `cargo test -p dw-fs` | 11 passed |
| `cargo fmt --check -p dw-appdb -p dw-engine`; `cargo clippy -p dw-appdb -p dw-engine --all-targets -- -D warnings` | clean |
| `git check-attr -a` on both migrations; `git diff --check 830a565 HEAD` | `text`, `eol: lf`; clean |
| `EXPLAIN QUERY PLAN`, an upsert, and canonical-form probes against the final SQL (Python sqlite3 3.46.1) | see §1 |
| A scratch binary calling the branch's `dw_fs::create_owned_dir` on `avatars` symlinks (`/work/scratch/dw-e0-07-r2/fsprobe`) | see §3 |

---

## 1. The final SQL as a one-way door

### The migration was edited in place, and its pinned checksum changed

- `V2026100801__dashpay.sql` is not released:
  - `main` has only `V2026100501__initial.sql`;
  - no branch other than this one contains 01e70d3;
  - `git ls-remote origin 'refs/heads/dw/e0-07*'` is empty, so the branch has never been pushed.

  Editing the file in place is therefore correct. A second migration would be wrong.
- `CHECKSUM_DASHPAY` went from 17807412343103651304 to 10226747534079937336 (`migration_tests.rs:185`). `shipped_migrations_are_unchanged` passes against the file, so the pin matches the bytes as checked out. The old value appears nowhere else.
- The only casualty is a developer `app.sqlite` that applied the old version: refinery reports it as divergent. That is acceptable before release.
- The LF attribute still covers both files.

### `dp_registration_lock`

```sql
CREATE UNIQUE INDEX dp_registration_lock ON dp_registration (wallet_id, asset_lock_outpoint)
    WHERE asset_lock_outpoint IS NOT NULL;
```

The index is correct:
- Rows without an outpoint are left out. They would also be distinct as NULLs, so leaving them out only keeps the index small.
- The lookup `WHERE wallet_id=? AND asset_lock_outpoint=?` plans as `SEARCH … USING COVERING INDEX dp_registration_lock`. The equality implies `IS NOT NULL`, so the partial index qualifies.
- On import, `INSERT OR IGNORE` turns a conflict into "keep the existing flow". `restoring_a_registration_with_a_held_outpoint_adds_no_second_flow` pins that behaviour, and it is the one we want.
- `an_asset_lock_outpoint_binds_one_flow_per_wallet` covers the late `UPDATE` path, another vout of the same transaction, and another wallet.
- `upgraded_schema_equals_the_fresh_one` asserts that the index exists after an upgrade too.

Two details belong to DP1-02, not to this branch (n6 and n7 below):
- An upsert must name the partial index's predicate. Without it the statement fails at prepare time with `ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint`.
- The invariant holds only for one canonical outpoint text: an upper-case txid was accepted as a second flow.

### `dp_trust_unverified`, keyed `(wallet_id, kind, key)`

The SQL is correct:
- `delete_wallet` lists the table (`lib.rs:243`), and the generic `delete_wallet_covers_every_wallet_scoped_table` guard still enumerates every wallet-scoped table.
- The export carries the table.
- Tests cover the same entity flagged in two wallets.

This key is strictly more expressive than the network-wide `(kind, key)`. A gate can still ask "does any wallet flag this entity" with `WHERE kind=? AND key=?`. That query is a scan, which is harmless on a table that only ever holds what was fetched during the fallback window.

Checking the store, I have to correct part of my round-1 premise (m5 below).

### docs/contracts/dwbackup-v1.md: the restore rules

§3.1, the §2.3 row and the §4 automatic backup at FundingSent are accurate against the code:
- Every wallet-scoped `dp_*` table is listed.
- `dp_avatar` is correctly excluded.
- The rowid aliases are renumbered in export order, and `rows.rs:109` exports `ORDER BY rowid`.
- The manager decision is stated, with the four DP1-02 conditions.
- An invitation-funded row ends in a typed failure, `invitation.invalid`, and does not block a new registration.
- The format stays v1: older builds skip tables and columns they lack, and newer builds read a missing `initial_profile` as NULL.

The DASHPAY §3.4 "Registration rows" block matches it, and the DP1-02 acceptance in ROADMAP now points to it. One precision nit, n8: the step-5 text still says only "identical rows are skipped".

---

## 2. Round-1 findings

| R1 | Status | Evidence |
|---|---|---|
| **M1** `initial_profile` | **Closed** | Column at `V2026100801__dashpay.sql:52`, documented at `:27-33` (nullable, JSON, nothing secret, and why `id` cannot key it). `Draft{…, initial_profile?}` added to §3.4, and the §3.6 request comment points to it. Tests: `constraints_reject_out_of_domain_rows` uses it, `initial_profile_is_optional_and_kept_verbatim`, and `dashpay_rows_travel_in_the_wallet_export` checks that the profile survives a restore under a new id. |
| **m1** one lock, one flow | **Closed** | §1 above. |
| **m2** dwbackup-v1 | **Closed** | §1 above. |
| **m3** trust-flag scoping | **Closed in the SQL**; the doc text needs one correction (m5) | `:113-126`, `lib.rs:243`, tests. |
| **m4** avatars must not fail the open | **Closed** | `session.rs:211-221`: a warning, `avatars_ready = false`, and `avatars_dir()` returns `None`. `umask_002.rs:135-147` covers a stray file, a self-looping symlink and recovery. The `dp_avatar` notes are closed too: `status` is the fetch/decode outcome only (`:133-138`), and the shared content-named files are refcounted on rotation (`:139-143`). Both are mirrored in DASHPAY §3.4. |
| **n1** paging index | Not adopted | It was optional, and a later migration can add it without a rebuild. Fine. |
| **n2** `ref` for recurring events | **Closed** | `:83-87`: the DPNS document id, or the label plus the contest end, never the label alone. |
| **n3** stale "§3.4 keys it by identity alone" | **Closed** | `:67-70`. |
| **n4** `updated_at` is never a wait start | **Closed** | `:41-43`, and in DASHPAY §3.4. |
| **n5** 01e70d3 commit message | Unchanged | It would need a history rewrite. It is harmless, so leave it. |

---

## 3. The disputed point: following an `avatars` symlink

**The author's claim holds.** A dangling `avatars` symlink whose target can be created does not fail the open, and it does not disable avatars. dw-fs reads the link, walks the target as the "chosen" part, and `mkdirat`s each missing component 0700.

My probe ran the branch's `dw_fs::create_owned_dir(root, "regtest/avatars")`, with `regtest` at 0700:

| `avatars` is | Result |
|---|---|
| a symlink to `outside/a/b/c` (missing), with `outside` at 0755 | `Ok`; `a`, `a/b` and `a/b/c` created 0700; `outside` stays 0755 |
| a symlink to an existing 0775 directory | `Ok`; the target stays 0775 (no chmod) |
| a symlink to `/proc/nope/x` | `Err`, so avatars are off and the session opens |
| a symlink owned by `nobody` (planted with sudo) | `Err("… belongs to uid 65534, not the current user")`, so avatars are off |

**Can a local attacker who controls the symlink make the app create, or chmod 0700, a directory it should not? No.**
- **chmod.** It cannot happen through an owned symlink. After the walk follows one, `owned_link` is set, and `set_mode` returns before any `fchmod` (`dw-fs/src/lib.rs:317`). Target components are `Part::Chosen`, whose wanted mode is their current one (`:249`). My second probe row confirms this.
- **create, by another user.** A symlink in the app's part must belong to the user, root, or the owner of `/` (`check_owner`, `:276-278`), and a symlink's owner is whoever created it. Another user's link is refused, as the fourth probe row shows. With m4 fixed, that refusal now disables avatars instead of failing the open.
- **create, through a race.** The only way to swap the entry between `statat` and `readlinkat` is to write `<root>/<network>`. When every ancestor is safe, the walk has already `fchmod`ed that directory 0700 before it reaches `avatars`. When an ancestor is not safe, `SqlitePersister::open` refuses the database further down the same `open` (`session.rs:252-267`; the storage ancestor check), so the session never comes up and `avatars_dir()` is never used. What remains is an empty 0700 directory chain created as the user during an open that then fails. That is exactly D1's existing behaviour for `spv/` and `backups/auto` (the same walk), and D1-r3 analysed and accepted it ("an untrusted-replaceable parent taints the walk and prevents chmod …").
- **create, by the user.** A same-uid "attacker" is the user. No boundary is crossed.

**This is consistent with D1's stated guarantees.** D1 never promised "never follows a symlink" for path resolution. It promised that modes are never changed by path or through a symlink. The sources:
- the dw-fs crate description: "changed through descriptors that never follow a swapped-in symlink";
- the module docs, `:10-35`;
- `create_owned_dir`'s doc: "A symlink among them is followed, but nothing at or below it changes";
- README §umask: "A symlink in place of one of the app's directories is followed but nothing behind it changes".

D1's own test `a_symlink_at_or_above_an_owned_directory_changes_nothing_outside` asserts that a missing directory behind an owned symlink is "created, as mkdir -p would" (`:603-607`). Commit 830a565's message ("no symlink followed") overstated this, as round 1 noted. Commit 8ff2938's message says "an unresolvable symlink", which is accurate.

**Verdict on the dispute: not a hole, and no major.** Two follow-ups:
- the code comment is now wrong in the other direction (n9);
- DP4-02 must not write by path through `avatars_dir()` (n10).

---

## 4. Findings

No blockers. No majors.

### Minor

**m5: DASHPAY §2.2 and §3.4, and the `dp_trust_unverified` comment (`V2026100801__dashpay.sql:116-119`), say "two wallets can hold a copy of one entity, and each copy is cleared only when that wallet's store has re-fetched it". That is true for contact requests and DPNS states, but not for identities, which round 1 also got wrong.**

In `wallet.sqlite` at the pin (bc41f1b):
- `identities` is keyed by `identity_id` alone (`V001__initial.rs:204-211`);
- the upsert is a **no-op when the row belongs to another wallet** (`sqlite/schema/identities.rs:44-58`: "A cross-wallet write becomes a no-op … preserving the resident blob");
- `contacts` is `(wallet_id, owner_id, contact_id)`, and `dpns_name_states` is `(wallet_id, document_id)`.

So there is one stored identity copy, owned by whichever wallet wrote it first.

**Failure scenario:**
1. Wallet A's changeset stores identity X while the fallback is in use, and the tap writes `(A, identity, X)`.
2. Wallet B later relies on X without its own changeset touching X.
3. A gate that checks only `wallet_id = B` lets money move against fallback data.

The opposite case is harmless: B's re-fetch of X is a no-op write, and clearing `(B, identity, X)` leaves A's flag in place.

The SQL needs no change; the key supports the safe query. **Fix (doc, now or in E0-10b and DP3-04):**
- state that the money-move gate blocks when *any* wallet holds a row for the entity (`WHERE kind=? AND key=?`);
- state that a verified re-fetch of an `identity` may clear that identity's rows in every wallet, because the store holds one copy;
- reword the comment at `:116-119` the same way before the file freezes.

It is a comment in an append-only file, which is why I raise it in this round.

### Nits

- **n6 (DP1-02):** an upsert keyed on the lock must repeat the predicate: `ON CONFLICT(wallet_id, asset_lock_outpoint) WHERE asset_lock_outpoint IS NOT NULL DO …`. Without it, SQLite refuses the statement. The failure is loud, so this is only a note for the implementer.
- **n7 (`:34-36`):** the unique index holds only for one canonical text, and `'T:0'` and `'t:0'` were accepted as two flows. Platform still arbitrates the lock, so no funds are at risk. Before the file freezes, name the canonical form in the comment: "lower-case hex txid in display order, `:`, decimal vout (`OutPoint`'s `Display`)". Optionally, add a CHECK now (one cannot be added later without a rebuild), for example:

  ```sql
  CHECK (asset_lock_outpoint IS NULL OR (
    substr(asset_lock_outpoint, 65, 1) = ':' AND length(asset_lock_outpoint) BETWEEN 66 AND 75
    AND substr(asset_lock_outpoint, 1, 64) NOT GLOB '*[^0-9a-f]*'
    AND substr(asset_lock_outpoint, 66) NOT GLOB '*[^0-9]*'))
  ```

  The rest of the schema deliberately avoids format CHECKs (for example on `wallet_id`), so the comment alone is acceptable.
- **n8 (`dwbackup-v1.md` §3 step 5):** "identical rows are skipped" should also say that a row whose key or unique index (a PK, `dp_events`'s UNIQUE, `dp_registration_lock`) matches an existing row keeps the existing one. The test `restoring_a_registration_with_a_held_outpoint_adds_no_second_flow` already pins this.
- **n9 (`session.rs:211-213`):** "a stray file or a dangling symlink named `avatars` turns avatars off". A dangling symlink with a creatable target is *followed and its target created*, and avatars stay on (§3). Reword it as "a stray file, or a symlink that cannot be resolved or that another user owns", matching the commit message and the README.
- **n10 (DP4-02 condition, already noted by the author):** `avatars_dir()` returns a path checked once, at open. DP4-02 must write thumbnails through descriptors: open the directory `O_NOFOLLOW|O_DIRECTORY` (or through dw-fs), and create files with `O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600. It must not write by path. A user-chosen symlink target keeps its own mode, so thumbnails there are as private as that directory, which is the user's choice.

---

## Verdict: **APPROVE-WITH-NITS**

Every round-1 finding is closed correctly: M1, m1, m2, m3 in the SQL, m4, n2, n3 and n4. The final SQL is sound as a one-way door:
- an unreleased file edited in place, with its pin updated;
- the partial unique index behaves as intended on import and update;
- the wallet-scoped trust key is strictly more expressive than the old one.

The disputed `avatars` symlink behaviour is not a hole and matches D1's contract (§3).

Before the file freezes, I recommend fixing two comments in the migration: m5's identity caveat at `:116-119` and n7's canonical outpoint form at `:34-36`. Both are comment edits that change the checksum pin again, and neither changes the schema. n8 and n9 are wording; n6 and n10 are for DP1-02 and DP4-02.

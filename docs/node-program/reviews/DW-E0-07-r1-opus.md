# DW-E0-07 review, round 1 (Opus)

**Branch:** `dw/e0-07-appdb-migrations` (worktree `/work/home/agent/workspace/dw-wt/e0-07-appdb-migrations`)
**Diff:** `git diff main...HEAD`: 01e70d3 (migration `V2026100801__dashpay.sql`, tests, `delete_wallet`, `.gitattributes`, DASHPAY §3.4 edit) and 830a565 (`<network>/avatars/`).
**Reviewer contract:** `briefs/REVIEW-COMMON.md`. I only read the branch; I changed nothing in it.

## What I ran

| Command | Result |
|---|---|
| `TMPDIR=/work/scratch/dw-e0-07-review/tmp cargo test -p dw-appdb` | 23 passed, 0 failed |
| `TMPDIR=/work/scratch/dw-e0-07-review/tmp cargo test -p dw-engine --test umask_002` | 1 passed, 0 failed (see the note) |
| `EXPLAIN QUERY PLAN` of the planned `dp_*` queries, run against both migrations on 20k `dp_events` rows (`/work/scratch/dw-e0-07-review/qp.py`, SQLite 3.46) | see item 2(c) |
| `git check-attr -a` on both migration files | `text: set`, `eol: lf` |

Note on the umask test: the first run failed with `database ancestor /work/scratch/dw-e0-07-review/tmp is group/other-writable (mode 0775)`. That was my environment, not the branch: agentbox runs with umask 002, so my scratch directory was created 0775, and the storage check rightly refused it. After `chmod 0700` on my scratch directories the test passed. Anyone who reruns it on agentbox needs a 0700 TMPDIR.

I also read:
- DASHPAY §2.2, §2.3, §2.6, §2.7 and §3.3–§3.8;
- the ROADMAP rows E0-06, E0-07, E0-10b, DP1-01..10, DP2-*, DP3-*, DP4-*, DP5-*, H-03 and H-06;
- `dw-appdb/src/{lib.rs,rows.rs}`, `dw-engine/src/backup.rs` (export and restore) and `docs/contracts/dwbackup-v1.md`;
- `dw-fs::create_owned_dir`;
- platform-wallet at the pin (`bc41f1b`): `wallet/asset_lock/{build.rs,tracked.rs,sync/recovery.rs,orchestration.rs}`;
- refinery-core 0.9.2's checksum code (`runner.rs:84-96`).

---

## 1. Consumers: does every planned use work without another migration?

| Consumer | Needs | Verdict |
|---|---|---|
| §3.4 state machine, `Draft{name, contested, funding, temp_name?}` | label, temp_label, funding; `contested` follows from the label (`check_username`) | ok |
| `KeysPrepared{identity_index, pubkeys}` | identity_index (pubkeys live in platform-wallet and are rederivable from the seed) | ok |
| `FundingSent{txid, outpoint}` | asset_lock_outpoint `<txid>:<vout>` | ok |
| `ProofWaiting{IS\|CL, since}` | IS or CL can be encoded in `phase` (no CHECK on it); `since` has no column | ok, see 2(b) |
| `IdentityRegistered{identity_id}` | `identity` | ok |
| `NameRequested → NameRegistered \| Contested{ends_at, temp_name_state}` | ends_at and last_state in `dp_contest_watch(wallet_id, identity, label)` | ok |
| **`ProfileCreated (optional)`** | the initial profile from `RegistrationRequest.initial_profile`, kept from Draft until this step | **missing**: M1 |
| `Failed{phase, code, retryable}` | phase kept, error, retryable (with a CHECK) | ok |
| §3.6 `registrations()` (phase, label, identity, txid, error, retryable) | all present | ok |
| `discard_registration` (only before FundingSent) | `asset_lock_outpoint IS NULL` | ok |
| `registration.in_progress` | `(wallet_id, phase)` index plus `error IS NULL` | ok |
| E0-06 / §3.5 journal, idempotent per (kind, contact, ref) | UNIQUE plus `INSERT OR IGNORE`; restore-pass events are written with `read_at` set | ok (n2 is about name events) |
| DP2-05 `events(cursor)`, `unread_count`, `mark_read(up_to)` | partial unread index; paging is in 2(c) | ok |
| DP1-04 contest watch | upsert on the PK; a scan for due rows (the table is tiny) | ok |
| DP1-03/DP1-05 main identity and main name | `dp_main_identity` upsert; the main name in `dp_prefs` | ok |
| DP3-01 per-contact lock and its resolution | `dp_payment_lock` PK `(wallet_id, identity, contact)` and txid | ok |
| DP3-04 / E0-10b trust gate | `dp_trust_unverified(kind, key)` | ok with m3 |
| DP4-02 avatar cache and the 200 MB rotation | `bytes`, `fetched_at` index | ok with m4 |
| X4 `dpns_name_states` mirror | planned as its own later migration | out of scope |

Apart from M1, every planned query, upsert and state transition I could derive from §3.5–§3.8 and the DP1–DP5 rows works on this schema.

---

## 2. The three suggestions the author skipped

### (a) A column for the initial profile on `dp_registration`: **needed** (M1)

**M1, major: `V2026100801__dashpay.sql:32-47`.** §3.6's `RegistrationRequest` carries `initial_profile?`, and §3.4 makes `ProfileCreated` a transition of the persisted machine. DP1-02's acceptance says "kill -9 at every transition … every case ends registered or resumable", and H-03 covers "registration … and profile".

The profile input exists from Draft. It is consumed only after `IdentityRegistered` and the name step. Between those points sits `ProofWaiting`, which on the ChainLock path can park keyless for minutes and survives restarts by design.

The input has no home that survives a restart:
- `dp_prefs` is keyed by identity, and the identity is unknown until IdentityRegistered.
- `settings_kv` keyed by the registration id breaks on a `.dwbackup` restore. `export_wallet_rows` drops the rowid alias (`rows.rs:63`), so `dp_registration.id` is renumbered on the target machine. The manager wants exactly that restore.
- Stuffing it into `funding` puts two meanings in one column.

**Failure:** the user registers with a display name and avatar, and the app is killed or restarted during the ChainLock wait. The resumed flow ends `Done` with no profile, which loses user input silently, or DP1-02 adds another migration.

**Fix:** add `initial_profile TEXT` (nullable, a JSON object: display name, public message, avatar URL, avatar hash and fingerprint; nothing secret) to `dp_registration`. Add it to the §3.4 row, and to the `registration` helper in `constraints_reject_out_of_domain_rows`. It costs nothing now. Later it costs a migration that every user's database runs.

### (b) A stored start time for the 300 s InstantSend window: **not needed; the skip is accepted**

- §2.6 bounds the key's lifetime by the **in-memory** flow lease, and a lease never survives a restart. That bound must come from an in-memory `Instant` taken at broadcast. A persisted time would be the wrong source, and `updated_at` would be a dangerous one, because a touch would extend how long the key is kept.
- After a restart the IS-or-CL wait belongs to platform-wallet. `resume_asset_lock(&out_point, Some(timeout))` (`orchestration.rs:701`) takes a relative timeout. The lock's status and proof are persisted in `wallet.sqlite`. A late or early "since" only moves when the UI switches from "waiting for InstantSend" to "waiting for a ChainLock". Funds and keys are not affected.
- If DP1-02 does want it, `ALTER TABLE … ADD COLUMN phase_at INTEGER` is a cheap migration with no table rebuild, so this is not a one-way door.

**Condition:** DP1-02 must not use `updated_at` as the start of the IS window (n4 asks for this to be documented).

### (c) An index for paging `dp_events` by id: **not needed now; the skip is accepted (nit n1)**

The query plans:
- **First page** (`WHERE wallet_id=? AND identity=? ORDER BY id DESC LIMIT n`): `SEARCH … sqlite_autoindex_dp_events_1 (wallet_id=? AND identity=?)` plus `USE TEMP B-TREE FOR ORDER BY`. Every event of the identity is sorted.
- **Later pages** (`AND id < ?`): `SEARCH USING INTEGER PRIMARY KEY (rowid<?)`, which walks the rowid backwards and filters wallet and identity.
- **`unread_count` and `mark_read(up_to)`** use `dp_events_unread`, `rowid<?` included. Both are good.

At the volumes H-06 sets (500 contacts, 10k history rows) the sort costs milliseconds. `CREATE INDEX dp_events_page ON dp_events (wallet_id, identity)` makes both pages a pure index range, because the rowid is implicit in the index. I checked that plan. It can be added by any later migration without a rebuild. Adding it now is free; leaving it out is fine.

---

## 3. `dp_registration.funding`: the encoding gap the author flagged

`TEXT NOT NULL` with no CHECK is the right container: an encoding can grow without a rebuild. The gap is that nothing defines the encoding yet. DP1-02 (and DP5-02 for invitations) must define it before the first row ships. Concrete requirements:

1. **Versioned and tagged.** For example `{"v":1,"kind":"core_balance",…}`. These rows now travel in `.dwbackup` between app versions (`rows.rs` imports by column name). A newer build must parse an older row, and an older build must refuse a newer one cleanly, as a `Failed` row with a code and not a panic.
2. **`CoreBalance`** should carry the locked amount in duffs from the accepted quote. A resume before FundingSent can then rebuild the same lock under the grant's cap. Otherwise a resume re-quotes silently and can cross the cap the user approved.
3. **`Invitation{link_id}`.** The link (a WIF, a bearer credential) lives in the vault as `invitation/<id>` (§3.3), and `.dwbackup` bundles carry only `mnemonic`, `mnemonic_passphrase` and `seed` (dwbackup-v1 §2.1). A restored invitation-funded row is therefore **unresumable on another machine**. That is safe: the asset lock is the inviter's, so no funds of the user are stranded. But DP1-02/DP5-02 must surface it as a typed failure, for example `invitation.invalid` or "open the invitation link again". It must neither retry forever nor block a new registration through `registration.in_progress`.
4. **`ExistingIdentity{id}`** duplicates the new `identity` column. Make `identity` canonical (set at Draft for this funding kind) and leave the id out of `funding`, or require the two to be equal. Otherwise the two can disagree after a partial update.
5. **`FaucetAssetLock{..}`** (developer builds): its key is a bearer credential (§3.8). It goes to the vault, never into `funding`, which is documented as "never a secret". The outpoint goes into `asset_lock_outpoint`.

No schema change is needed for this item. It is a precondition on DP1-02, and I suggest adding it to the DP1-02 row or to `m4-dashpay-engine.md` when that is written.

---

## 4. Backup restore of `dp_registration` (manager decision: restore it)

Can the restored rows double-spend an asset lock or register an identity twice?

**What the schema and the export get right:**
- **Platform arbitrates an asset lock.** An outpoint funds one identity: the identity id is derived from the outpoint, and Platform rejects a consumed outpoint with a typed error (`tracked.rs:40-110`, `RecoveredFromChain`). Two rows with the same `asset_lock_outpoint` can therefore never spend it twice; the second ends `Failed`.
- **The target machine can resume a funded row.** It rebuilds `wallet.sqlite` by scanning from the birth height. platform-wallet's restore scan reconstructs finalized asset locks whose credit outputs pay this wallet's funding accounts as `RecoveredFromChain`, with a ChainLock proof (`tracked.rs:69-80`). The signing key is rederived from the seed through the tracked lock's derivation, not from our row. So a restored row with an outpoint has what it needs to resume. That is the manager's goal, and it works.
- **Nothing in the row is secret** (but see §3, item 5, for the faucet key). The backup payload is AEAD-sealed, so a forged backup cannot plant rows.
- **Restore never merges into a live wallet.** It refuses a wallet that already has keys (`backup.rs:587-589`, `backup.already_exists`). Rows can therefore only land next to existing rows when keys are attached to a watch-only wallet, and a watch-only wallet cannot have run a registration.

**What can still go wrong, and who must handle it:**

**m1, minor: no uniqueness binding an asset lock to one flow (`V2026100801__dashpay.sql:32-48`; `rows.rs:166-176`).** A rowid-alias table is deduplicated on import only by whole-row equality, so two snapshots of the same flow at different phases import as **two rows**. Today the only path to that is the watch-only-plus-keys case above, so it is theoretical. But the invariant "one asset lock, one flow" costs one line now. Added later, it is a `CREATE UNIQUE INDEX` that fails on any user database that already holds a duplicate.
**Fix:** `CREATE UNIQUE INDEX dp_registration_lock ON dp_registration (wallet_id, asset_lock_outpoint);` (NULLs stay distinct, so Draft and KeysPrepared rows are unaffected). DP1-02's `finish_asset_locks` then reuses the failed row of a stranded lock instead of opening a second one, which is the behaviour it wants anyway. The comment at `rows.rs:166` ("Tables numbered by a rowid alias have no other key") is no longer true for `dp_events`; it is harmless there.

**Stale snapshots: conditions on DP1-02, not findings against this branch.** Automatic backups are written when a wallet is added and when a session opens (dwbackup-v1 §4), never on a `dp_registration` transition. A restored row can therefore be **behind** reality. For example, the backup says `KeysPrepared` while the source machine already funded and registered.

The real double-registration risk is that the target resumes such a row by building a **new** asset lock. Platform rejects identity keys that are already registered at that identity index, so the second identity fails, but the second lock's funds are then stranded until `finish_asset_locks`. If the row was still a Draft (index NULL) and a fresh index is chosen, a second identity really is created, and its name request fails as `taken`.

The schema cannot prevent this. DP1-02 must:
1. never advance a restored row past KeysPrepared (never fund) until SPV has synced and the DP1-05 identity discovery and asset-lock reconstruction pass has run. §2.2 rule 5 already holds registrations until SPV sync; DP1-02 should add discovery to that gate;
2. before funding, check for an identity at `identity_index` and for `RecoveredFromChain` registration locks, and adopt them;
3. write `asset_lock_outpoint` when the lock is **built**, before broadcast, so a kill or snapshot after broadcast never shows an unfunded phase for a funded flow;
4. schedule an automatic backup when the flow reaches FundingSent, so the newest backup carries the outpoint the manager wants restorable.

**m2, minor: `docs/contracts/dwbackup-v1.md` §2 and §3 step 5 do not mention the `dp_*` rows.** The contract lists "labels, address book, receive requests, UTXO locks and wallet settings". The author asked whether it should say more; it should. Add the wallet-scoped `dp_*` tables, the manager's decision that `dp_registration` rows are restored to resume locked funds, and a pointer to the DP1-02 conditions above. Note that invitation-funded rows do not resume elsewhere (§3, item 3).

---

## 5. The rest

### `delete_wallet` coverage: correct

- It deletes every table with a `wallet_id` column (`lib.rs:228-252`). `delete_wallet_covers_every_wallet_scoped_table` enumerates them from `pragma_table_info`, so the next migration that adds a wallet-scoped table and forgets the list fails. That is a good guard.
- `removing_a_wallet_removes_its_dashpay_rows_only` covers a shared identity across two wallets.
- The restore rollback (`backup.rs:615-617`) and `remove_wallet` (`wallets.rs:229-231`) both go through it.

**m3, minor: `dp_trust_unverified` is keyed `(kind, key)` network-wide (`V2026100801__dashpay.sql:96-101`).** Two wallets can hold the same entity. Identity and DPNS-label rows coincide across wallets; contact-request ids do not.

If both wallets stored it while the fallback was in use, one row stands for both copies. E0-10b clears a row "when its re-fetch verifies". If that re-fetch refreshes only one wallet's copy, the other wallet keeps fallback-verified data that is no longer flagged.

The money path mostly rests on per-pair contact-request rows, which stay distinct, so I do not rate this higher. Two fixes:
- add `wallet_id` to the key now, the cheap and clean one: `(wallet_id, kind, key)`. It then also joins `delete_wallet` and the export. Restoring unverified flags is conservative, so that is safe;
- or keep the schema and require in E0-10b that a row is cleared only after the entity has been re-fetched for **every** wallet whose store holds it.

Changing a PK later means rebuilding or replacing the table, so decide now.

The same goes for removal hygiene. After `remove_wallet`, or a full wipe (IOS-109: `remove_wallet` for each wallet, then `Vault.destroy`), `dp_trust_unverified` rows and `dp_avatar` rows and thumbnails still name or show the removed wallet's contacts. Wallet-scoping the trust table fixes half of that. For the avatar cache, which belongs to the network, have DP4-02 clear it when the last wallet of a network is removed.

### Export round trip: correct

- `dashpay_rows_travel_in_the_wallet_export` covers `dp_main_identity` and `dp_registration`, idempotent re-import, and refusal of rows from another wallet.
- `dp_events` ids are renumbered in rowid order (the export uses `ORDER BY rowid`), so the relative order holds for `mark_read(up_to)` after a restore.
- A restored event with the same unique key as one written by the first pass is ignored (`INSERT OR IGNORE`), and the restored `read_at` survives. That matches catch-up silence (§2.7).
- **Not tested:** `dp_events` and `dp_payment_lock` through the export. Low risk, because the code is generic. Consider adding `dp_events` to the round-trip test, since it is the one table with both a rowid alias and a UNIQUE key.

### Checksum pinning and `.gitattributes`: correct

- refinery 0.9.2 hashes `(name, version, sql)` with a fixed-key SipHasher13 (`runner.rs:92-96`), so the checksum is the same on every little-endian target.
- `text eol=lf` forces LF on checkout whatever `core.autocrlf` says. `git check-attr` confirms both files are covered.
- `shipped_migrations_are_unchanged` pins both checksums, so a CRLF tree, or an edit to a shipped file, fails the tests loudly instead of shipping a divergent checksum. One caveat: a Windows clone made before this commit keeps its CRLF files until they are checked out again. The pin catches that, which is the right behaviour.
- The new file contains `§`, which is UTF-8 and stable; no issue.

### `avatars/` directory: correct, with one robustness note

- It is created through `dw_fs::create_owned_dir` after `app.sqlite` (`session.rs:209`): 0700 under umask 002, and a legacy 0775 directory is restricted. The umask test passes.
- A swapped-in symlink is not chmod-ed through: the target stays 0755, which the test checks, and a missing directory is recreated.
- This matches the documented dw-fs policy ("a symlink … is followed, but nothing at or below it changes"). Commit 830a565's message ("no symlink followed") overstates it. The symlink *is* followed for path resolution, so DP4-02 writes thumbnails through it. The author already noted that DP4-02 must write files 0600 through dw-fs.

**m4, minor: the avatar cache can fail the whole session open (`session.rs:209`).** The author reports that a bad `avatars` entry fails `NetworkSession::open`, like `spv/` and `backups/` do. For a disposable thumbnail cache that is out of proportion: a stray file or a dangling symlink named `avatars` would make the wallet unopenable.

**Fix:** create the directory lazily in DP4-02's cache, or log a warning at open and disable avatars until it can be created. While there, two notes for `dp_avatar`:
- `status` is documented as "the fetch and verification outcome" (line 107). Verification against `avatarHash` and the dHash is per **profile**, not per URL: two profiles can cite one URL with different hashes. Store only the fetch and decode outcome, and compare `content_sha`/`dhash` with the profile when reading.
- Files are "named by SHA-256" of the content, so two URL rows can share one file. Rotation must check references before unlinking, and the 200 MB sum counts shared files twice.

### Nits

- **n1** (2c): optionally add `CREATE INDEX dp_events_page ON dp_events (wallet_id, identity)` now.
- **n2** (`:64-66`): the frozen comment prescribes "puts its DPNS label in `ref`" for name events. A label can recur for one identity under X4, where a name is sold and bought back, and the second `username_registered` would then be deduplicated away. Prefer the DPNS domain document id, or label plus contest end, in `ref`. E0-06 can choose that without a schema change, but the comment is permanent once shipped.
- **n3** (`:51`): "§3.4 keys it by identity alone" contradicts §3.4 as this branch edited it. Fix it before the file freezes.
- **n4** (`:31`): add to the `updated_at` comment "never the start of a wait (§2.6)", per 2(b).
- **n5:** commit 01e70d3's message says `delete_wallet` also removes "the identity-scoped rows of that wallet's identities". It removes rows by `wallet_id` only, which is right but not what the message says.

---

## Verdict: **CHANGES-REQUIRED**

The migration is careful and well tested: STRICT tables, a CHECK domain that never needs widening, the wallet scoping that makes removal and export generic, pinned checksums, and the LF attribute. The one change I require before the file freezes:

- **M1 (major):** add `initial_profile TEXT` to `dp_registration`. Without it DP1-02 cannot resume `ProfileCreated` after a restart without another migration, and a key on the registration id breaks under the restore the manager asked for.

Strongly recommended in the same round, because each is free now and costs a migration (or a failing one) later:
- **m1:** a unique index on `(wallet_id, asset_lock_outpoint)`;
- **m3:** `wallet_id` in `dp_trust_unverified`'s key, or the E0-10b rule stated in writing;
- **n2/n3/n4:** the comment fixes.

**m2** is a contract edit. The §3 and §4 conditions belong to DP1-02.

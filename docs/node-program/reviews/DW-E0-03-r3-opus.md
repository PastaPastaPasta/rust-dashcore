# DW-E0-03, round 3: security review (Opus, standing in for GPT per DEC-42)

- **Reviewed:** `dw/e0-03-signer-glue` at `d36acdbe51`. The new commits are 0ce7292, 1cd50db, 589e709, 084051a and d36acdb, on top of 119ee06 (`git diff 119ee06..HEAD`, 13 files, +1030/−291).
- **Read:** REVIEW-COMMON, the original brief (REVIEW-DW-E0-03.md), DW-E0-03-r1-gpt, r1-opus and r2-gpt. Also DASHPAY §2.6, the ROADMAP E0-04 row, and m1-engine §2.2 as amended.
- **Platform:** the pinned `bc41f1bc23` is used for the commit-point question.
- **Paths:** relative to the worktree unless they are absolute.
- **No changes made:** I did not modify the branch, commit, push or comment.
- **Scratch work:** done in an exported copy (`git archive HEAD`) under `/work/scratch/dw-e0-03-r3-opus/`.

## What I ran

| Check | Result |
|---|---|
| `umask 022; TMPDIR=/dev/shm/e0-03-r3-tmp cargo test -p dw-vault -p dw-engine` ×3 | All 3 runs green: **254 passed, 1 ignored** each (`suite.log`). |
| `cargo test -p dw-vault --lib lock_race -- --nocapture` ×5 | 7/7 each run, every mode, 16 workers × 300 locks. Per mode: 6.2k–15.3k results; 4,800 refused `Locked`; 4.2k–4.8k released while a `lock()` waited; **0 released under an ended epoch; 0 refused by the release check** (`stress.log`). |
| Mutation runs of the lock-race tests (`mutations.log`) | **Check removed** (`release` always `Some`): the three stress tests still pass, and only `a_result_is_not_released_in_a_later_epoch` fails.<br>**Gate removed** (the shared hold on a dummy lock): all three stress tests fail with 2,641–2,933 refusals. The release check alone still lets no stale result out.<br>**Both removed:** `check_gate_log` fails with "a result of epoch 1 was released in epoch 2". |
| Scratch `ErasingSha256` tests in the copy (`probes.log`, `sha-big.log`) | Three-way splits at every position, for every length 0–200 (splits up to 70 apart); 20,000 random chunkings up to 4,200 bytes; reuse after finalize, an empty final message and empty updates; HMAC against `hmac 0.12` for message lengths 0–599; the state after HMAC-style use is fully reset. All pass.<br>Release run of lengths 2^29−1 … 2^29+120 bytes, which crosses 2^32 bits at 9 padding classes: **all match sha2**. |
| Scratch vault probes (public API, plus one test-only hook in the copy's `key_for`) | Reproduce m1, m2 and m3 below. |
| `git diff --check 119ee06..HEAD` | Clean. The worktree is clean at `d36acdb`. |

## Round-2 closure

### M1 (grant token from another vault): **closed**

- **Construction.** The token is built only in `redeem_grant` (`vault.rs:1215`).
  - Its fields are `pub(crate)`. It has no `Clone`, `Default` or serde, and `Debug` omits the instance id.
  - Code outside the crate cannot build or alter a token.
  - The instance id comes from `crypto::random_array()` (OS RNG) at `Vault::open`. It is never persisted, and it is shared by `Vault` clones (`Arc<Shared>`).
- **Every token consumer goes through `token_binding` (`vault.rs:1231`):**
  - `key_for` (`:1247`), which covers reveal, export, `with_revealed_seed`, wipe, encrypt, enroll and set-spend-limit;
  - `token_signer` (`:1552`), which covers `signer`, `platform_signer` and `scan_key`.
  - The `GrantToken` getters (`purpose`, `wallet`, `max_duffs`) return only public metadata.
  - No other code reads `token.key`.
  - `token_signer` refuses another vault's token before it loads any key, and repeats the full check under the `inner` guard that issues the signer.
- **Replay cases:**

  | Replay | Result |
  |---|---|
  | Another vault with the same wallet id and epoch | Refused (`GrantInvalid`); committed tests cover both own-key and vault-key tokens. |
  | An earlier opening of the same file | Refused (new instance id). |
  | Across an unlock | Refused (the epoch is bumped, and the epoch only ever increases within an instance). |
  | After destroy, recover or encrypt | Refused (`forget_key` bumps the epoch). |
  | After change-passphrase | **Not refused** (m3). |
  | After wipe | **Not refused** (n2). |

- **Constant time.** `subtle::ConstantTimeEq` on `[u8; 32]` and `u64`, with both comparisons evaluated before the branch. It is genuinely constant-time. However, neither value is secret: anyone who can present a token already holds it. So this is defence in depth, not a security property. (`signing_seed` and `OpGuard`'s epoch check elsewhere use `!=` and `ct_eq` inconsistently; that is harmless.)

### M2 (no release after lock): **closed for the vault boundary**; the deferred part is under-specified (M1 below)

- **One lock orders release against epoch changes.**
  - `OpGuard::release` (`vault.rs:113`) reads `inner.epoch` holding `inner`.
  - Every epoch change goes through `Inner::next_epoch`, which is reached only from `forget_key` and `install_key`. Both take `&EpochGuard`, so the type system requires the exclusive gate. They run under `inner`.
  - The only other writer is the test-only `bump_epoch_ungated`.
  - `Vault::gated` (`:506`) is the only way to build an `OpGuard`. It reads the start epoch under `inner` after taking the gate, then consumes the guard in `release`.
- **Drop order inside `release`.** The local `inner` guard is dropped first, then the guard's `_ops` read lock. Nothing ordered after an epoch change can release.
- **Hand-off.** There is no TOCTOU inside the vault. The window between `release` returning `Some(out)` and the caller looking at it is exactly the documented, accepted boundary.
- **Escape routes checked:**
  - **Early `Err`.** `body(&op)?` bypasses `release` for errors. None of the `VaultError`/`SignerError` values carry secret bytes: they hold paths, library error strings and fixed text. `KeyMismatch` and the contactInfo decrypt error are inside `Ok(..)` and are released normally.
  - **Panics.** They unwind through `Op::drop`, which erases the master key, and through `Zeroizing` seeds. The read guard does not poison. No panic payload carries a secret (`expect("the buffer has room…")`).
  - **Callbacks.** `run` and `with_key` are `pub(crate)`. All 14 call sites (platform.rs, signer.rs) pass pure closures that return their product. Nothing writes to captured state.
  - **`with_revealed_seed`.** Its closure `f` runs after release, by design, and m1-engine §2.2 documents it. The module-doc sentence "none still computes with a key once `lock()` has returned" (`vault.rs:50`) is broader than that (n3).
  - **Results dropped on refusal.** The secret-bearing types (`Zeroizing<..>`, `ExtendedPrivKey`, `WalletSecret`, `RevealedMnemonic`, `ContactInfoOpened`) all erase on drop. No raw `SecretKey` passes through a gate (the engine converts the auto-accept key after release).
- **Is the stress test vacuous?** No, as a test of the gate. The mutation runs show that removing the gate fails all three stress tests, and that removing both the gate and the check trips the log reconstruction. Two further assertions are meaningful:
  - `released == results + ROUNDS` proves that every caller-visible `Ok` was a logged release;
  - `released_while_lock_waited` (4.2k–4.8k) proves that the race was exercised.

  The stress test cannot detect a broken release check, because the gate makes the check redundant. `a_result_is_not_released_in_a_later_epoch` covers the check, and the mutation run confirms it does. That division is sound and should be stated in the test doc (n1).

### N1 (HMAC intermediates): **closed**

`ErasingSha256` (`dip15.rs:87-200`):

- **Padding.** The two-block condition `filled + 1 > 56` is correct for every `filled` from 0 to 63.
- **Length.** The length is kept as a byte count and encoded as `length.wrapping_mul(8)` in big-endian. That is exact modulo 2^64 bits, as FIPS 180-4 requires. It was verified across the 2^32-bit boundary.
- **Reuse.** `finalize_into` resets to the IV, so reuse yields the next message's digest. A second finalize gives SHA-256("").
- **Erasure.** `Zeroize` on arrays and integers uses `write_volatile` plus a compiler fence, so the wipe cannot be optimised out. `Drop` and `reset` both call it.
- **No stale copies of the hasher.** The hasher is neither `Clone` nor moved after it absorbs the key block. Both HMAC passes reuse `h` in place, and the pad block, inner digest and output are `Zeroizing`.
- **Constant time.** No branch or index depends on the data; only the public lengths drive control flow.
- **Remaining-risk notes.** Those in `dip15.rs:27-30` and `lib.rs:35-42` are accurate:
  - the compression's `a..h` and round temporaries in registers or spilled;
  - return-value moves of `Zeroizing<[u8; 32]>`;
  - libsecp256k1's ECDH stack, including rust-secp256k1's by-value `SharedSecret` return.

  I found no further residue owned by this code.

## Findings

### Blockers

None.

### Major

**M1. The E0-04 commit-point rule allows the racy check that round 2 called insufficient, and names no commit point for library-driven flows.**

- **Where:** `docs/design/DASHPAY.md:451-454`, `docs/design/ROADMAP.md:66` (E0-04 acceptance), `docs/contracts/m1-engine.md:163-167`.
- **The rule as written:** "each flow checks its lease at its own commit point … and drops the result once the lease is revoked". The test is "a flow paused (barrier) after its signature was released and before its commit, then resumed after a lock, broadcasts or submits nothing".
- **Problem 1: check-then-act is allowed and passes the test.**
  - The wording is satisfied by `if lease.revoked() { drop } else { broadcast(tx).await }`.
  - That is the check-then-act that r2 M2 rejected ("consumption serialized against cancellation; a racy final epoch check is insufficient").
  - The specified barrier sits *before* the check, so that racy implementation passes the acceptance test.
  - A lock that lands between the check and the hand-off to the transport still loses: "Lock to cancel" fails silently.
- **Problem 2: the main flows have no engine-owned commit point.**
  - At `bc41f1bc23`, platform-wallet signs and submits inside single library calls:
    - `put_to_platform_and_wait_for_response` (`document.rs:316`, `contract.rs:297`, `invitation.rs:551`, identity registration);
    - `self.broadcaster.broadcast(&tx)` (`asset_lock/build.rs:1142`, `payments.rs:1446`, `core/broadcast.rs:158`).
  - The engine sees only the `Signer` adapter, and that is before release.
  - The requirement does not say where those flows fence. Candidates are an engine-supplied broadcaster/transport wrapper, or a refusal in the signer adapter that is accepted as the limit.
  - It also does not say whether the background `DashPayCrypto` drain (crypto products → a contact-request document) counts as a "flow".
- **Problem 3: lock-time behaviour is unstated.**
  - Does `lock()` wait for an in-flight hand-off, and with what bound? A held permit must not span the network round trip, or Lock stops "winning immediately".
  - What happens to a submission already handed off (platform-wallet's `MaybeSent`)?
  - Is persisting a signed artifact (retry queue, changeset) a commit point?
  - Must a parked flow re-sign under a new lease after re-unlock, rather than reuse a stored signature?
- **Failure scenario:** E0-04 ships `if !lease.is_revoked() { broadcaster.broadcast(&tx).await }` and passes the barrier test. A user presses Lock during a registration, the lock lands between the check and `broadcast`, and the asset-lock transaction goes out after the UI showed "cancelled".
- **Fix (docs only, in this branch):**
  1. State the rule as a linearization. The commit is the hand-off to the transport. It happens while holding a shared permit of the lease, and revocation takes that permit exclusively. A hand-off either completes before `lock()` returns or is refused. The permit covers only the hand-off, not the network wait.
  2. List each flow's commit points, including the library-internal ones, and how the engine reaches them. Examples: a wrapping `TransactionBroadcaster` for Core transactions; for state transitions, a DAPI/SDK hook, or else the residual stated explicitly. Say whether the background drain counts.
  3. Add a second barrier test that pauses *between* the lease check and the hand-off, locks, and asserts that either `lock()` blocks until the hand-off finishes, or nothing is handed off.
  4. State the `MaybeSent` reporting and that results of a revoked lease are never persisted or reused.

### Minor

**m1. The newly gated `seed_derivation` and `core_mnemonic_check` can block `lock()` on the OS store.**

- **Where:** `vault.rs:1472-1481`, `vault/compat.rs:296-299`; the key load is in `full_dek` (`vault.rs:1261-1306`).
- **What happens:** on an unencrypted vault after a `lock()`, the next call reloads the data key from the OS store *inside* the gate. A scratch `OsSecretStore` that blocks in `get` kept `lock()` waiting for more than 500 ms for both functions. `core_mnemonic_check` is called from `dw-engine/src/compat.rs:737`.
- **Why it matters:**
  - With the real keyring backend, the read can stall on Secret Service or D-Bus, including a keyring-unlock prompt. `lock()` then waits on the user.
  - This breaks the module's own rule that a gate holder must not block (`vault.rs:25-28`).
  - It is new in d36acdb: those two functions were ungated before. The token paths do not reach the OS store in practice, because `authorize(None)` caches the key and nothing un-caches it without an epoch change.
- **Fix:** call `self.full_dek()?` before `gated`, to warm the cache. Inside the gate, use only the in-memory key and return `Locked` if it is gone.

**m2. For ungated token consumers, `key_for` checks the token and then uses the key in two separate steps, so the operation completes after `lock()` returns.**

- **Where:** `vault.rs:1247-1256` (two `inner` acquisitions), used outside the gate by:
  - `wipe_wallet_secret` (`:1435`);
  - `encrypt` (`:756`);
  - `enroll_quick_unlock` (`:1724`);
  - `set_quick_unlock_spend_limit` (`:1799`).
- **Reproduction:** with a scratch hook between the binding check and the key fetch, `lock()` returned in about 25 µs with a wipe past its check. The wipe then returned `Ok(true)` and deleted the records. This happened both with a passphrase grant's own key on a locked encrypted vault, and on an unencrypted vault (key reloaded from the OS store).
- **Why it matters:**
  - The vault's own lock doc says `lock()` "invalidates every outstanding grant token". The r3 commit says "every token use checks it". Both are true only at the moment of the check.
  - m1-engine §2.2 lists `enroll_quick_unlock` as ungated, but not wipe, encrypt or set-spend-limit.
  - `enroll_quick_unlock` returns a new slot-B wrap key, which is a credential for the data key, possibly after `lock()` returned.
- **Fix:** take `writer`, then the gate, then run `key_for` and the commit through a `commit_records` variant that accepts the held writer guard. The lock order writer → ops → inner forbids taking `writer` inside `gated`. Alternatively, list all four operations in m1-engine §2.2 as ungated and token-checked at entry only.

**m3. `change_passphrase` neither ends the epoch nor clears pending grants.**

- **Where:** `vault.rs:843-870`.
- **Reproduction:** on a locked vault, the following were all made under the old passphrase and still worked after `change_passphrase(old, new)`:
  - a pending `RevealSecret` grant (redeemed with `reveal_mnemonic`);
  - a redeemed `RevealSecret` token (`export_wallet_secret` returned `Ok`);
  - a `PlatformOp` token (`scan_key(..).master_key()` returned `Ok`);
  - a `DashPayCrypto` signer that carries the grant's own key (ECDH returned `Ok`).
- **Why it matters:** the data key is deliberately not rotated (review H2), so this is consistent with Core's `walletpassphrasechange`. But a user who changes the passphrase because the old one leaked gets no revocation, and a pending grant can be redeemed for its TTL (120 s in the tests).
- **Fix:** clear `inner.grants` in `change_passphrase`, at least those that carry their own key. Document that outstanding signers and tokens survive, or bump the epoch if that is acceptable.

### Nits

- **n1.** State in the lock-race tests' doc that the stress tests verify the gate and the log reconstruction, and that the release check on its own is covered only by `a_result_is_not_released_in_a_later_epoch` (confirmed by mutation). Separately, a worker that panics mid-round (`lock_race_tests.rs:403`) leaves the others blocked on the `Barrier`, so the test hangs instead of failing. Catching the panic, or using a timed wait, would fix that.
- **n2.** Tokens are single-use grants but multi-use tokens until the epoch ends, and wipe or store does not end the epoch. In a probe, an old `Wipe` token wiped a re-imported wallet a second time, and a `ScanKey` issued before the wipe served the re-stored seed. `WalletId` is derived from the root xpub (`multiwallet.rs:137`), so in practice the re-import is the same seed. Worth one sentence in m1-engine §2.2.
- **n3.** The `vault.rs:50` sentence "none still computes with a key once `lock()` has returned" should exclude `with_revealed_seed`'s closure, as m1-engine §2.2 already does.
- **n4.** The `ErasingSha256` tests stop at 1,100 bytes. A cheap check of the length encoding would pin the >2^32-bit behaviour I verified. It is not needed for DIP-15's short inputs.

### Out of scope (pre-existing, already documented)

`open_backup_bundle` (`vault/compat.rs:456`) returns the plaintext `WalletSecret` (phrase and seed) of this vault's own bundle on an unlocked vault. It needs no grant and no gate. m1-engine §2.2 lists it, and today only engine restore code calls it. It is the one remaining path that yields the phrase without the passphrase that `RevealSecret` requires. It deserves a dw-ffi `disallowed-methods` entry like `scan_key`'s.

## Answers to the four questions

1. **M1:** no path bypasses `token_binding`. Tokens cannot be forged outside the crate. Replay is refused across instances, reopenings, unlocks, destroy, recover and encrypt; it is **not** refused after change-passphrase (m3) or wipe (n2). The compare is constant-time but protects non-secret values. For ungated consumers, the check is at entry only (m2).
2. **M2:** the release check and every epoch change are serialized by `inner`, and there is no in-vault TOCTOU. No secret-bearing result escapes a gated operation through errors, panics or callbacks; the ungated exceptions are m2's and `open_backup_bundle`. The stress test is not vacuous for the gate (mutation-verified). The check itself is covered by its own unit test.
3. **ErasingSha256:** correct at every padding class and across 2^32 bits, reusable after finalize, and erased with volatile writes. The residual-risk notes are accurate.
4. **The E0-04 requirement:** it is not yet precise enough to implement. It allows a racy check, its barrier test cannot tell, and it names no commit point for flows that platform-wallet signs and submits internally (M1).

## Verdict

**CHANGES-REQUIRED.** The vault code fixes for r2 M1, r2 M2 and r2 N1 are sound and verified, and I would approve the code as is with m1–m3 fixed or explicitly deferred. The blocking item is M1, a docs-only fix: the E0-04 rule and its acceptance test must require that the commit be serialized against revocation, and must name the commit points of the library-driven flows. The deferral of r2 M2 rests on that rule.

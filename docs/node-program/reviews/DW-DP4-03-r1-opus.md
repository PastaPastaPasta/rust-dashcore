# DW DP4-03 Edit Profile + avatars in every list: Opus-h review, round 1

- **Branch:** `dw/dp4-03-edit-profile` at `e7bc9270`, off main `06d2be9b`. I reviewed `git diff 06d2be9b e7bc9270`, plus the engine paths it calls (dw-engine `platform/{profile,profiles,avatar,avatar_image}.rs`, dw-app `api/files.rs`, `shell/files.rs`) and the pinned DashPay contract JSON.
- **Method:** read-only, in a detached worktree at `/work/scratch/dp403-review/wt`.
  - Vitest: the 4 DP4-03 files, 48/48 green, and a wider run of `src/dashpay`, `src/fixture/profile.test.ts` and `src/components`, 133/133 green.
  - 7 mutants of my own, run against vitest. The toggle mutant was also run against the Playwright D13/D14 specs (Chromium, pinned Docker image).
  - A throwaway browser probe of PNG eXIf orientation on Chromium and WebKit.
  - The full gate was not rerun. dw-app's `bindings_are_current` and the new `files` test passed in the implementer's gate log (`/work/scratch/dw-dp4-03/dw-gate-test.log`).

## Verdict: ACCEPT WITH NITS

I found no Blocker or Major. Most of the implementer's claims hold:

- **Privacy:** every avatar call site goes through the switch.
- **Save:** Save needs a real change and can't be double-submitted.
- **Conflict:** the baseline moves on a conflict.
- **Discard guard:** it covers ×, Esc, the scrim, Cancel and every `navigate`.
- **Crop:** only the cropped square is sent.
- **Gravatar email:** it is cleared as the fetch starts, and is in no query key, log or storage.
- **Fixture:** its order matches the engine's.

Two things are weaker than claimed:

- The UI's D13 guarantee, "no other user's avatar is read at all", has no test (Minor 1).
- The crop is not WYSIWYG for one class of PNG on WebKit (Minor 2).

**Fix in this PR:** Minor 1 and Minor 4. Both are small.

**Can wait for B1:** Minor 2 and Minor 3. Both are on the file path, which no build can reach until an upload host exists (D5). Record them under F2 so they land before Choose File… is turned on.

## Item 2: the avatar fingerprint (BL-190)

**What DP4-03 sends.** Every save is `dashpay_update_profile(identity, {displayName, publicMessage, avatar, baseline})`. The engine (`profiles.rs` `update` → `merged` → `set_avatar`, :209) turns `avatar` into the document's three avatar fields:

| DP4-03 choice | `avatarUrl` | `avatarHash` | `avatarFingerprint` |
|---|---|---|---|
| **Public URL** (`set`) | the https URL the engine fetched | SHA-256 of the fetched bytes as served (`calculate_avatar_hash`) | **the library's dHash** of those bytes (`platform_wallet::calculate_dhash_fingerprint`, `avatar_image.rs:215`), 8 bytes |
| **Gravatar** (`set`) | `https://www.gravatar.com/avatar/<md5(trimmed, lower-cased email)>?s=200&d=404&rating=g` | SHA-256 of the fetched bytes | **the library's dHash** |
| **File** (`set`; disabled until B1, D5) | the upload URL | SHA-256 of the engine's re-encoded ≤ 512 px PNG | **the library's dHash** of that PNG |
| **Keep** | unchanged (Platform's fields are carried over) | unchanged | unchanged |
| **Remove** | removed | removed | removed |

- **What is published:** yes, DP4-03 publishes an `avatarFingerprint` and an `avatarHash` whenever the user sets a URL or Gravatar picture and saves. The fingerprint is the library's dHash, the one BL-190 says differs from the mobile wallets' (by 12–35 bits on 44 of 53 avatars). The design note already admits this in F3.
- **The contract:** it does **not** allow `avatarUrl` without `avatarFingerprint`. The DashPay contract pinned by the engine (`dashpay-contract` 5.0.0-beta.2 from `PastaPastaPasta/platform@82fb33aff`) has the same `profile.dependentRequired` in both `schema/v1/dashpay.schema.json` and `schema/v2/dashpay.schema.json`:
  ```
  avatarUrl:         [avatarHash, avatarFingerprint]
  avatarHash:        [avatarUrl, avatarFingerprint]
  avatarFingerprint: [avatarUrl, avatarHash]
  ```
  The fingerprint itself is `byteArray`, exactly 8 bytes, and its content is not checked. So a picture can't be published without some 8-byte fingerprint.
- **The options:**
  - (a) Hold the URL and Gravatar save until BL-190.
  - (b) Ship as is. Mobile wallets using the 20-bit rule will show initials for most desktop-set pictures.
  - (c) Keep the sources but refuse `set` until BL-190 lands. Keep and Remove are unaffected: Remove never writes a fingerprint, and Keep re-sends the existing one byte for byte.
- **What doesn't depend on the decision:** no DP4-03 read compares fingerprints (DP4-02 D32), so the desktop shows its own and others' pictures correctly either way.

## Findings

### Minor

**1. D13's "no other user's avatar is read at all" has no test. A mutant that ignores the switch everywhere passes vitest and the Playwright D13/D14 specs.**

- **Where:** `apps/desktop/src/dashpay/profileQueries.ts:111` (the code is correct), `apps/desktop/e2e/avatars.spec.ts:64`, `apps/desktop/src/fixture/handlers.ts:1127-1133`.
- **The mutant:** `enabled = … && (own || true)`.
  - Vitest: 133/133 pass.
  - `--project=chromium e2e/avatars.spec.ts -g "D13|D14"`: 2/2 pass.
- **Why it survives:** the fixture's `dashpay_avatar` already answers `null` for others while off. So the specs only prove that nothing is *shown*, which the engine guarantees anyway, not that nothing is *read*. The fixture's call log (`fixture/index.ts:95`) records command names only.
- **Impact:** the engine (DP4-02 D33/D40) still returns `None` and fetches nothing while off. A regression would lose the UI's defence in depth silently, but leak nothing. The brief's example mutant, "ignore the privacy switch in one list", is a weaker mutant than this one and would survive too.
- **Fix:** either is enough.
  - Record `dashpay_avatar`'s `identity` in the fixture (or count non-own calls). Then have D13 assert zero reads of other identities across Home, Contacts, Pay, the bell and Activity, with the own identity read.
  - Or add a vitest of `useAvatar` with `PictureScope` `{others:false}` that asserts `dashpayAvatar` is never called for `own=false`.

**2. On WebKit, the crop the user frames is not the one the engine cuts for a PNG with an eXIf orientation tag.**

- **Where:** `EditProfileSheet.tsx:330` (`naturalWidth/Height`), `editProfile.ts:371`, the engine's `avatar_image.rs:114-116`, the fixture's `handlers.ts:99`.
- **The disagreement:**
  - The engine (`image` 0.25.10) applies orientation for PNG too: `ImageDecoder::orientation()` defaults to `exif_metadata()`, which `PngDecoder` implements from `eXIf`.
  - Browsers disagree with each other. My probe used a 40×20 PNG with `eXIf` Orientation=6. In the pinned Playwright image, `naturalWidth×naturalHeight` was **Chromium `[20,40]`, WebKit `[40,20]`**.
- **What happens on WebKit** (Playwright's Linux WebKit, the WebKitGTK path; WKWebView unverified):
  - **Orientations 5–8, non-square image:** the default rectangle `{x:10,y:0,w:20}` lies outside the engine's 20×40 picture. The engine refuses it with `invalid_argument`, and the UI says "This picture could not be read."
  - **Orientations 2–4, or a 5–8 crop that happens to fit both shapes:** the engine silently cuts a different region, flipped or rotated, from what the user framed.
- **What does not happen:** a rectangle outside the image never reaches the encoder. The engine's `cropped` checks it first. The JPEG case is fine: WebKit applies JPEG EXIF, and the `ROTATED_JPEG` spec covers it.
- **The fixture:** it checks a PNG's crop against the raw IHDR size, which models WebKit and not the engine. The D19 note ("a JPEG's is unchecked: its size … needs the EXIF orientation") holds for PNG too.
- **Fix, before B1:**
  - Crop on an engine-oriented preview: a `prepare_avatar(File{crop:null})` or a new "orient" answer giving the oriented size and a preview, with the rectangle scaled to it.
  - Or refuse or strip PNG `eXIf` orientation consistently on both sides.

**3. The engine's pixel limit (4096 px per side) is not checked in the renderer, and when it trips the UI gives the wrong reason.**

- **Where:** `editProfile.ts:158` and `:95`; `avatar_image.rs:19`, `:86`, `:112`.
- **Example:** a 6000×4000 or 8000×6000 phone JPEG under 5 MB.
  - It passes `#crop`'s byte check and is drawn at full size in the webview. The user frames it.
  - Use Photo then gets `avatar.too_large` and the text "This picture is too large. Choose one under 5 MB.", which the file already is.
- **The webview's memory:** it also decodes whatever dimensions a 5 MB file declares. A highly compressed 16k×16k PNG is about 1 GB RGBA in the renderer, a picture the engine would refuse unread.
- **Fix:**
  - Read the width and height from the header (PNG IHDR, GIF LSD, JPEG SOF) before showing the crop, or at least in `pictureLoaded`, and refuse above 4096 with its own text, such as "Choose a picture at most 4096 pixels wide and high."
  - Or give `avatar.too_large` from a file the pixel wording.
- **When:** file path only, so it can wait for B1.

**4. Any `invalid_argument` from a save with a new picture is read as "the candidate expired": the user's picture choice is dropped, and the text is wrong.**

- **Where:** `editProfile.ts:593-598` (`#candidateGone`), used at `:444` and `:580`.
- **The cause:** `dashpay_update_profile` returns `invalid_argument` for other reasons too:
  - `profiles.rs:312` "the wallet only watches this identity; it cannot publish";
  - an unparseable identity;
  - the dw-app argument layer.
- **What the user sees:** with one of these, the picture goes back to "keep" and the user reads "The picture you chose is no longer ready. Choose it again." Choosing it again loops the same way. That is an edit lost with a misleading reason, which D9's "a refused save keeps every edit" rules out.
- **Fix:** match the candidate's own refusals only. The engine's details are `unknown avatar candidate` and `the avatar candidate has no URL; upload it first` in `avatar.rs` `with_candidate`/`published`. A typed `kind` would be better than matching the text. Everything else should go through `saveErrorText` with the picture kept.

### Nit

1. **Conflict with nothing left to save.** After `profile.conflict` whose `current` already equals the user's fields (or whose picture state makes "remove" a no-op), `dirty` becomes false and Save is disabled. The error still says "Save again to replace it with your changes". Consider clearing the error, or using a "Your profile already matches" line, when `!dirty` after a conflict. Where: `editProfile.ts:443-444`.
2. **The second-submit test doesn't catch removing `save()`'s own guard.** Mutant M7 drops `!this.canSave ||` from `save()` (`:414`) and passes vitest. The second call parks on a passphrase prompt that the test never answers, so `toHaveBeenCalledTimes(1)` still holds. The button guard is covered: M6, which removes `saving === null` from `canSave`, is caught. To cover both, assert after the first save that no second grant is pending, or that `grantRequest` was called once.
3. **U-05's purpose list is stale.** U-05 D2's list of `FilePurpose`s (`docs/design/U-05-tools.md:133`) lacks both `qrImage` (DP2-08) and `avatarImage`. No contract doc covers `FilePurpose`, so D3's "no contract change" is right; this is just a one-line doc touch-up.
4. **A drop with no upload host does nothing.** D5 says a drop or paste "says the same". With no upload host, the drop handler returns without a word (`EditProfileSheet.tsx:110-119`), and only the static E10 hint says it. That's acceptable, but the D-row overstates it.

## Checks that passed

- **Privacy (item 1):**
  - All 13 avatar renders go through `Avatar` → `useAvatar`, with `own` only on the chip, My Profile's header and Edit Profile's "keep". There are no other `contact-avatar` or `data:image` renders.
  - The 13 renders: ContactList rows and NetworkResults, ContactPane, AddContact, the TxDetail contact, history rows (Home and Activity), FrequentStrip, the PayToContact picker and payee, ConfirmSheet, Notifications and bell requests and events, the identity chip, My Profile, Edit Profile.
  - `PictureScope` wraps the whole Shell. Portalled sheets keep the context.
  - Before `load_contact_pictures` answers, or if it errors, `others` is false and the query is not enabled. The same holds on a cold start and on first render.
  - Cached data is hidden as soon as `enabled` turns false (`return enabled ? … : null`). Off → `resetQueries`.
  - Edit Profile's `picturesOn` defaults to false, so no URL fetch and no Gravatar hash happen before the setting is read.
  - The Gravatar email lives only in `model.email`. It is cleared on fetch, Back, every step change and every reset. It is not in a query key. The renderer logs nothing (the only `console.*` is engine notices). There's no local or session storage outside the fixture, and the engine's `AvatarSource` `Debug` hides it. The published Gravatar *URL* (an MD5 of the address) is inherent to Gravatar and matches Android.
- **Save (item 3):**
  - `canSave` needs `dirty`, both lengths within the limits, a non-empty profile, the form step, and nothing preparing or saving.
  - `saving` is set synchronously, before any await.
  - Every save carries `baseline`. `profile.conflict` moves it, and the engine's no-op check comes before the conflict check, as in the fixture.
  - The limits come from `profile_limits`, with a fallback of 25/140. Length counts scalar values of the trimmed text, as the engine does.
  - A cancelled grant, from a leave during the upload or the publish, keeps the edits and shows no error.
- **Crop (item 4):**
  - `cropRect` is always integer, at least 1 px, and inside `naturalWidth×naturalHeight`.
  - The engine refuses any outside rectangle before decoding or encoding (`cropped`).
  - GIF uses the logical screen size on both sides. A tiny 1×1 picture works.
  - The uncropped original is never sent. Only `{bytes, crop}` go to `prepare_avatar`, and the upload is the engine's re-encoded ≤ 512 px PNG. Mutant M3 (`crop: null`) is caught.
- **dw-app (item 5):**
  - `AvatarImage` takes png, jpg, jpeg and gif.
  - `max_bytes = 5 MiB + 1`, and `read_capped` refuses `len >= max`, so 5 MiB is allowed and larger is refused unread. This matches the engine's `MAX_BYTES = 5 << 20` and the renderer's `MAX_FILE_BYTES`.
  - The generated `FilePurpose.ts` follows, and `bindings_are_current` passed in the gate. `FilePurpose` is dw-app's, so no m4 contract change is needed.
- **Fixture (D19):** the order is trim / too_long → candidate → no-op → conflict → empty → credits → lease, the engine's order.

## Mutants

| # | Mutant | Caught? |
|---|---|---|
| M1 | `useAvatar` ignores the switch (`own \|\| true`) | **No.** Vitest 133/133 and Playwright D13/D14 2/2 pass (Minor 1) |
| M2 | `canSave` without `dirty` | Yes: "IOS-076 starts from the profile; only a real change is dirty" |
| M3 | Use Photo sends `crop: null` (uncropped) | Yes: "D4 a chosen file is cropped, and only the square goes to the engine" |
| M4 | the Gravatar email kept in the model | Yes: "D15 …" |
| M5 | a conflict doesn't move the baseline | Yes: "a conflict moves the baseline …" |
| M6 | `canSave` ignores `saving` | Yes: "a save in flight cannot be submitted again …" |
| M7 | `save()` without its own `canSave` guard | No (Nit 2; the button guard stays covered by M6) |

The review worktree, the e2e cache and the scratch tools are deleted after this report. The report itself stays here.


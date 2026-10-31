# DW DP4-03 Edit Profile + avatars: Opus-h review, round 2 (the fix delta)

- **Branch:** `dw/dp4-03-edit-profile` at `c902d51d`. Scope: the fix commits since r1's `e7bc9270`:
  - `3b696390`, `affbf224`: the r1 minors and nits;
  - `0d8d582a`, `fa40f42d`: BL-190;
  - `5332ecc3`, `c902d51d`: docs D26–D29;
  - and the `m4-dashpay-engine.md` resolution in `a8c42ab1`.

  Read as `git diff a4a782c2 c902d51d`.
- **Method:** read-only, in a detached worktree `/work/scratch/dp403-review/r2/wt` with its own target, reflink-seeded from `/work/scratch/dw-dp4-03/target`. Steps:
  - Built `dw-engine --lib` once and ran the avatar tests, 42/42.
  - Installed pnpm in the worktree and ran `editProfile.test.ts`, 32/32.
  - Wrote **an independent Python version** of Android's path from the sources, not from the port. It was run over the implementer's corpus (`/work/scratch/dw-dp4-03/bl190/corpus`).
  - Ran 4 TS revert mutants and 9 env-gated Rust mutants.
- **Sources read:**
  - dash-wallet master `7b2e2318` (2026-10-19): `CocoaImageDHash.kt` and `ProfilePictureHelper.kt` are identical to `v11.9.1`, as D27 cites. Also its publish and read sites (`EditProfileViewModel`, `EditProfileActivity`, `UpdateProfileWorker`, `ExternalUrlProfilePictureDialog`, `DashPayUserBottomSheet*`).
  - Skia `android12-release` (android.googlesource.com): `SkBitmapProcState{.h,.cpp,_matrixProcs.cpp}`, `SkBitmapProcState_opts.h` and `SkBlitter.cpp`.
  - DIP-15, platform `c5726706` (`calculate_dhash_fingerprint`, `docs/dashpay/INTEROP_DESK_CHECK.md`, the Kotlin SDK's `Dashpay.kt`), DashSync-iOS `DSBlockchainIdentity.m` and dashwallet-ios `DWProfileUpdateBridge.swift`.
- **Not rerun:** Playwright (the D13 read-count spec), for which I relied on reading the spec and its positive control, and the full gate.
- **Interruption:** agentbox powered off at 04:37Z partway through the review. Nothing was in flight. The worktree and binaries survived, and the review continued from them.

## Verdict: ACCEPT WITH NITS

There is no Blocker, Major or Minor.

- **The port is right.** It is a faithful line-by-line port of Android's `CocoaImageDHash` on Skia's legacy filtered scale.
- **An independent version matches.** My version, written from the Skia and Kotlin sources and fed libjpeg-turbo's pixels, gives the published fingerprint bit for bit on **all 43** Android-set avatars:
  - 42 with Pillow's default ISLOW IDCT. That is the 39 plus the 3 the implementer attributes to zune-jpeg rounding.
  - The last one with `djpeg -dct fast`.

  So the implementer's 39 / 3 / 1 / 9 / 1 breakdown holds, including both decoder explanations. My version also reproduces the fixture's distances for the 9 library vectors and the robohash exactly.
- **The r1 findings hold.** Each one I asked to fix is fixed, and each has a test that fails when the fix is reverted.
- **The merge is fine.** D29's merge keeps MG-06's version 33 and item 60, and adds no surface change.
- **The new code is safer on hostile images.** It drops the library's second, unbounded `load_from_memory` decode, so every fingerprint now comes from the single decode that is bounded.

## Item 1: the dHash port against Android's code

| Step | Android / Skia (Android 12) | Port (`avatar_dhash.rs`) | Verdict |
|---|---|---|---|
| Decode | `BitmapFactory.decodeFile` of the bytes at the URL. ARGB_8888, EXIF not applied, GIF frame 0 | `decode_unoriented` (same limits as `decode`); the orientation is applied only after the hash (`avatar_image.rs:213-215`) | Same |
| What is hashed | `avatarHashAndFingerprint(url)`: what the URL serves, for both the external URL and the post-upload paths (`EditProfileActivity:290`) | URL / Gravatar: the whole fetched picture, zoom crop ignored. File: the PNG to upload (`ingest_file`, `avatar_image.rs:242`), i.e. what Android would fetch from Imgur | Same. The brief's "computed from the cropped, EXIF-oriented image, before re-encoding" is not what the code does: it is unoriented and uncropped for URLs, and after re-encoding for files. **The code is the correct one**, and D27's own wording is right. |
| Scale matrix | `createScaledBitmap`: `sx = 9f / w` (float), `SkMatrix` invert, `1/sx` in float | `inverse(n) = 1.0 / (9.0 / n)` in f32 | Same |
| Row tap | `SkBitmapProcStateAutoMapper`: `(y + .5) · inv` in float, `SkScalarToFractionalInt` (32.32, trunc), minus `fFilterOneY >> 1` (= 0x8000 for clamp), then `>> 16` | `Tap::row`: `fixed_3232((i + .5) · inv) − 2³¹`, then `>> 16` | Same |
| Column tap | The mapper maps column 0 only, then `fx += fInvSxFractionalInt` per column (`filter_scale`). The decal path gives the same taps when both ends are `< maxX`. | `Tap::column`: first + i · `fixed_3232(inv)` | Same (D27's r2 fix) |
| Pack / clamp | `clamp = SkTPin(f >> 16, 0, max)`, far = `clamp((f + SK_Fixed1) >> 16)`, weight `(f >> 12) & 0xF` | `Tap::at` | Same |
| Filter | `S32_alpha_D32_filter_DX`: the portable, NEON, SSE2, SSSE3 and AVX2 variants all compute `Σ p · w` with `w` in `{(16−x)(16−y), x(16−y), (16−x)y, xy}`, then `>> 8`. `fAlphaScale` is 256 (opaque paint). | `filter` | Same |
| Premultiply | ARGB_8888 is premultiplied in memory | `premultiply` (`SkMulDiv255Round`) | Same; no vector (Nit 1) |
| Grey | `getPixel` gives straight colour; `c · alpha / 255` (Int); `(r + g + b) / 3f` compared with `>` | `grey_of`: unpremultiply rounded, `· a / 255`, sum (×3 preserves order), `>` | Same for opaque. The unpremultiply rounding of partial alpha is unverified (Nit 1) |
| Bits | Rows y = 0..7 of the 9 × 9 grid; `rowDiffs.setBit(x)`; `diffs = diffs << 8 \| row` | `out[y] \|= 1 << x` | Same |
| Bytes | `ProfilePictureHelper.toByteArray` gives the last 8 big-endian bytes, so row 0 is byte 0 | `[u8; 8]`, row y in byte y | Same. This is also the library's layout (`hash.to_le_bytes()`, bit `y·8+x`), so both algorithms use one byte order. |

**Edge paths:**
- **Width 1:** Skia routes this to `S32_D32_constX_shaderproc`, with the same Y taps, so the result is the same.
- **9 × 9 sources:** these are an identity copy (`createBitmap` returns the source), and the port gives the identity too (sub 0).
- **Out of scope:** `valid_for_filtering` (< 16384 px) and the near-identity snap don't apply within the 4096-px decode limit.

**The vector test:** `equals_android_on_the_testnet_avatars_android_set` passes, as do the 5 other `avatar_dhash` tests and the 3 new `avatar_tests`: before EXIF, whole not crop, and the library hash plus the mobile dHash.

**Independent check:** `/work/scratch/dp403-review/r2/tools/indep_dhash.py`, before cleanup.

| maker | count | my value (libjpeg-turbo ISLOW) | fixture's (engine decoder) |
|---|---|---|---|
| android | 39 | 0 bits | 0 |
| android-libjpeg | 3 | **0** | 1, 1, 2 |
| android-ifast | 1 | 4 bits; **0** with `djpeg -dct fast` | 3 |
| library | 9 | 12–23 bits, each equal to the fixture | 12–23 |
| other (robohash) | 1 | 13 | 13 |

## Item 2: is publishing this value right, and which one?

**Who reads `avatarFingerprint` today:** no one compares it.

| Reader | What it does with the fingerprint |
|---|---|
| Android (dash-wallet master) | `CocoaImageDHash.getDistance` has no caller outside its own file. The profile screens only carry the field through. |
| DashSync-iOS (old iOS) | Writes CocoaImageHashing's dHash (`dataWithUInt64`, `DSBlockchainIdentity.m:3450`); compares nothing. |
| dashwallet-ios 9.1.3 / SwiftDashSDK | Passes the bytes to platform-wallet, which writes `calculate_dhash_fingerprint`; compares nothing. |
| platform-wallet, the Kotlin/JNI SDK | Write the library's value; compare nothing. The library's own doc says "compare by Hamming distance, never by equality". `INTEROP_DESK_CHECK.md:508` says "never assert that avatarFingerprint bytes match across clients". |
| The desktop | DP4-02 D32: does not compare. |
| DIP-15 | "Clients should compare the Hamming distance between the document and the downloaded thumbnail". No threshold and no algorithm are given, and no client implements it. |

**What 1–3 bits can harm:** nothing today. A future reader that applies DIP-15 with any useful threshold (anything ≥ 4 of 64) accepts a 1–3-bit difference. Two unrelated pictures differ by about 32 bits.

The real cross-client gap is between the two algorithms (12–35 bits, median about 21). That gap exists on testnet regardless of what the desktop does.

The pinned contract (`dependentRequired`) requires a fingerprint whenever `avatarUrl` is set. So "publish none" is not possible; it would mean publishing no picture at all.

**The options:**

- **(a) Android's, as implemented (recommended).**
  - *Matches:* 43 of the 53 byte-exact testnet avatars, the shipping Android app, and the original DashSync algorithm family (CocoaImageHashing, which Android ported).
  - *Breaks:* nothing now. If a future library-based reader adopts a strict threshold, desktop pictures would fall outside it, exactly as Android's own pictures already do.
  - *Cost:* about 290 lines of Skia-exact code to maintain.
- **(b) The library's `calculate_dhash_fingerprint`.**
  - *Matches:* iOS 9.1.3, and the Kotlin SDK's `createOrUpdateProfile` (which computes it in Rust from `avatarBytes`). So if dash-wallet moves to the unified SDK, Android will start publishing this one too.
  - *Breaks:* nothing now. It is the 12–35-bit outlier against every Android-set profile.
  - *Cost:* zero code, since it is one call. Reverting is a one-line change in `ingest` plus the D6 vectors.
- **(c) None until a cross-client spec exists.** This blocks URL, Gravatar and (later) file pictures entirely, because the contract won't take a URL without a fingerprint. Keep and Remove would still work. It is the safest only in the sense that it publishes nothing; it loses the feature for no reader-visible gain.

**Recommendation: (a). Ship Android's.** Pair it with an upstream ask (platform and DIP-15) to pin one algorithm, with vectors, before either SDK grows a reader that applies a threshold. Do (b) only if platform says the unified SDK's value is the one going forward.

The choice is cheap to reverse: a fingerprint is written only when the user sets a picture, and Keep re-sends the published bytes. So switching later changes no existing profile.

*Aside, not for this PR:*
- Android's `UpdateProfileWorker:143` publishes `BigInteger.toByteArray()` directly. That is 9 bytes when row 0's top bit is set, and fewer than 8 when it is zero, so that path would fail the contract's 8-byte rule.
- Android's Keep path (`EditProfileActivity:480`, `BigInteger(bytes)` and back) zero-pads a fingerprint that starts `FF 8x…` into `00 8x…`.
- Both suggest that nothing downstream relies on exact fingerprint bytes.

## Item 3: the r1 findings

Each fix was checked with a revert mutant on `editProfile.ts`, run against `editProfile.test.ts`.

| r1 | Fix | Revert mutant → test that fails |
|---|---|---|
| Minor 1 (D13 "nothing is read") | The fixture counts `dashpay_avatar` per identity (`handlers.ts`). D13 asserts `others == []` across Home, Activity, Contacts (row and pane), Pay and the bell, with own reads > 0. D14 adds the positive control (`others` non-empty while on). | Not rerun (Playwright). By reading, the r1 mutant `own \|\| true` now fails D13's `reads.others` assertion; D14's control shows the count is live. |
| Minor 2 (WebKit PNG eXIf) | Deferred to F7(a), before B1 | — (unreachable: see item 5) |
| Minor 3 (4096 px) | `pictureLoaded` refuses over 4096 with `tooManyPixels`; the header pre-check is F7(b) | No check → "a picture over the engine's 4096 pixels…" fails |
| Minor 4 (any `invalid_argument` = gone) | Only the two engine texts (`CANDIDATE_GONE`, `endsWith`). The engine constants are pinned to the TS by `edit_profile_knows_the_candidate_refusals_by_their_text`. Upload's other `invalid_argument` shows the engine's text. | Back to `hasCode(invalid_argument)` → "any other invalid_argument keeps the new picture…" fails |
| Nit 1 (conflict, nothing left) | `conflictMatches` line when `!dirty` after a conflict | Line removed → "a conflict whose profile already is the user's…" fails |
| Nit 2 (`save()` guard) | The test asserts `grantRequest` once and no pending grant | `!this.canSave \|\|` removed → "a save in flight cannot be submitted again…" fails |
| Nit 3 (U-05 list) | `qrImage` and `avatarImage` listed | — |
| Nit 4 (D5 drop wording) | D5 says a drop without an upload host is ignored | — |

## Item 4: D29's merge of `m4-dashpay-engine.md`

- **Contents after the merge:** `Contract-Version: 33`, the surface hash stamped `version: 33`, and §7 items 59 (MG-05) and 60 (MG-06) intact. All of these are main's, unchanged by the merge.
- **The branch's whole change** (`git diff a4a782c2 c902d51d`) is one sentence of the §2.7 `prepare_avatar` row: the fingerprint is "the mobile wallets' dHash of them (DP4-03 D27)".
- **No version bump or new item is needed.** The rule ("a signature, a record, a trait impl or a code") isn't triggered by a prose change to a row.
- **The contract test** gains `avatar_dhash.rs` in `NOT_FACADE`, which is correct: it is a private module with no `pub` facade items.
- **Item 57** (DP4-02) still reads "publishes the candidate's URL, hash and fingerprint". That is neutral and still true.

## Item 5: untrusted image input

- **One bounded decode.**
  - Before r2: `ingest` decoded within `limits(MAX_SIDE)`, then handed the raw bytes to the library's `calculate_dhash_fingerprint`. That is `image::load_from_memory`, a second decode with default limits.
  - Now: the fingerprint comes from the same bounded `DynamicImage`: 4096 px a side, a 128 MiB allocation budget, and a 64 MiB decoded cap checked before decoding.
  - Peak memory is unchanged: the unoriented image plus one rotated or cropped copy, as `decode` already did.
  - This is a strict improvement.
- **Arithmetic.**
  - `n ≤ 4096`, so `fixed_3232` of at most ~455 · 2³² fits in i64, and the 16.16 value fits in i32, as Skia assumes.
  - The filter sums are at most 255 · 256.
  - `grey_of` handles `a == 0`, and `c · 255 ≤ 65025`.
  - The taps are clamped to `[0, n−1]` (`taps_stay_inside_the_picture` covers 1…4096).
  - The one panic is `clamp(0, -1)` when `n == 0`. A zero-size picture can't come out of the decoder, and even so `mobile_dhash` runs inside `guarded`, so a panic would become `avatar.unsupported`.
  - CPU cost is 324 `get_pixel` calls, constant.
- **Who runs it.** The dHash runs only for a candidate, i.e. a URL or Gravatar the user typed, or a file the user chose. Reads of other users' profiles use `fingerprint = false` (D32), so a hostile profile can't make the wallet run it.
- **F7 deferral is safe.**
  - The renderer never decodes network bytes: avatars are engine PNGs as `data:` URLs (`Avatar.tsx:35`), and the candidate preview is the engine's 256 px PNG.
  - The webview decodes only the user's own local file in the crop step (`EditProfileSheet.tsx:326`). That path is reachable only through `canUse("file")`, which needs `uploadAvailable` (false until B1) for Choose File…, drop and paste alike (`editProfile.ts:297`, `:362`, `:375`; `EditProfileSheet.tsx:104-119`).
  - The engine side of a file is bounded anyway: 5 MiB, 4096 px, and the allocation budget.
  - So F7(a) and F7(b) can't happen in a shipped build, and must land before B1 as F7 says.

## Item 6: my mutants on the dHash

All 9 were built once behind `A6MUT` and run with the `avatar` filter (42 tests).

| # | Mutant | Killed by |
|---|---|---|
| A | Columns map each centre (the first port) | the vectors |
| B | No premultiply before the filter | **survives** (Nit 1) |
| C | Comparison reversed (`<`) | uniform/ramp, vectors, EXIF test |
| D | Lerp weight rounded, not truncated | the Skia tap test, vectors |
| E | Bit order reversed in a byte | the vectors |
| F | Filter sum rounded (`+128`) | the vectors |
| G | Grey without the `· a / 255` | `transparent_pixels_count_as_black` |
| H | `>=` instead of `>` | 1-px, uniform, vectors, EXIF test |
| I | Bias of a whole pixel, not half | the tap test, vectors |

## Findings

### Nit

1. **The partial-alpha path is untested, and one mutant there survives.**
   - **Where:** `avatar_dhash.rs:40` (`premultiply` before the filter) and `:143` (`grey_of`'s unpremultiply rounding).
   - **The surviving mutant:** B, which feeds straight colour to the filter. It passes all 42 tests.
   - **Why:** `transparent_pixels_count_as_black` only uses alpha 0 and 255, and the testnet vectors are all opaque (D27). A file upload is re-encoded to RGBA, so a semi-transparent PNG a user chooses at B1 would go through this path.
   - **Second gap:** whether Android 12's `getPixel` unpremultiply rounds as `(c·255 + a/2)/a` is also unverified. It goes through `SkConvertPixels`, which I did not trace.
   - **Fix:** add one synthetic partial-alpha case, with the value worked out by hand from the Skia code, before B1. Better still, take one from an Android device. File it with F7.
2. **A stale test comment.** `avatar_tests.rs:415-416` still says "the mobile wallets' dHash is another algorithm", but the engine now publishes that dHash. The point the test makes is that publishers use two algorithms (D32's revised wording in `avatar_image.rs:274`). Use that wording here too.
3. **The brief's order claim is inverted** (not a code finding).
   - The code hashes the whole picture, unoriented and uncropped, for URL and Gravatar.
   - For a file, it hashes the upload PNG, after re-encoding.
   - That matches what Android computes from the published bytes. D27 and the doc on `ingest` describe this correctly.
   - The manager's summary is the only place that states it otherwise. Correct it there if it's reused, for example in the PR text.

## Cleanup

These are deleted after this report:
- the worktree (`git worktree remove`);
- the target, the pnpm store, the uv cache;
- the downloaded sources;
- the tools directory.

This report stays.

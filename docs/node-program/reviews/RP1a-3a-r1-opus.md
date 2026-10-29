# RP1a-3a review, round 1 (Opus)

Branch `node/rp1a-3a` at 46b4b003c (`d90512244..46b4b003c`, 10 commits, 18 files).
Core reference: v24.0.0-rc.4 (copies in `/work/scratch/rp1a-3a/`, plus `~/workspace/dash` at 0e7382df80d for strencodings, spanparsing, pubkey.h and standard.cpp).

**Verdict: ACCEPT-WITH-NITS.** 0 M, 1 m, 4 n.

I found no behaviour divergence from Core in any of the six RPCs or the ported primitives. Every finding is a test gap or a doc nit. m-1 is cheap and pins a path real users reach (two typos in a Platform address), so I'd take it before merge.

## Findings

### m-1: the Bech32/Bech32m tie in `LocateErrors` is untested, and a mutant that flips it survives every test

- Port: `dashd-rpc/src/bech32.rs:331-332`. Core: `bech32.cpp:565` (`possible_errors.size() < error_locations.size()`, a strict `<`, so on a tie the first encoding tried, Bech32, wins).
- The code is correct. But mutant RV-1 (`<` → also replace on an equal count, so Bech32m wins ties) passes all 158 unit tests. It also passes every DIFF-RPC case: `edge_va_two_err` (`rpc_diff.rs:2320`) is not a tie.
- Ties are reachable with two typos in a Platform address. A brute force over the two-character substitutions of `tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl` found them at once. dashd v24.0.0-rc.4 (regtest, run by me) answers:
  - `validateaddress tdash1q3g3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl` → `"error_locations":[19,34],"error":"Invalid Bech32 checksum"` (the mutant gives `"Invalid Bech32m checksum"`, `[6,7]`)
  - `validateaddress tdash12qg3wyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl` → `[35,41]`, `"Invalid Bech32 checksum"` (mutant: Bech32m, `[6,10]`)
  
  The port gives Core's answer for both.
- Fix: add the first string to `bech32/tests.rs::core_bech32_invalid_vectors_locate_their_errors` as `("Invalid Bech32 checksum", &[19, 34])`, and as an `edge_va_tie` row in `util_commands_match_dashd_v24`.

### n-1: checksum-error precedence is untested (mutant survives unit and DIFF)

- Port: `dashd-rpc/src/descriptor.rs:120-142`. Core: `script/descriptor.cpp:1212-1245`. The checksum-length error (`:1227`) comes before "Invalid characters in payload" (`:1233`).
- The port's order is right. Mutant RV-4 checks the payload characters first and survives everything. No case has both a bad payload character and a wrong-length checksum: `short_checksum` has a clean payload, and `bad_char` has no `#`.
- dashd rc.4: `getdescriptorinfo "pk(\x01)#abc"` → -5 `Expected 8 character checksum, not 3 characters`. `deriveaddresses "pk(\x01)"` → -5 `Missing checksum`.
- Fix: add both as DIFF edge rows, or as unit rows in `checksum_errors_are_cores`.

### n-2: the `Range is too large` boundary is pinned only by DIFF-RPC

- Port: `dashd-rpc/src/util.rs:183`. Core: `rpc/util.cpp:1150-1151`.
- Mutant RV-2 (`>=` → `>`) passes all unit tests: `core_deriveaddresses_functional_test` uses `[1e9, 2e9]`, far from the edge. DIFF `edge_da_range_toolarge` (`[5,1000005]`, `rpc_diff.rs:2404`) does kill it, so tier 1 with dashd catches it.
- Fix (optional): add `[5,1000005]` → -8 `Range is too large` to the unit test, so a run without dashd also catches it.

### n-3: the doc comments overstate what createmultisig's inference can reach

- `descriptor.rs:822-826` and D25 in `RP1a-notes.md` say `sh(pk(...))` and the `addr()`/`raw()` fallbacks are reachable. Only two shapes are:
  - createmultisig's redeem is always `GetScriptForMultisig`, so the P2PK branch (`:844-848`) never fires.
  - The top script is always P2SH, so `ExtractDestination` succeeds and `raw()` (`:873`) never fires.
  - The `[1, 17..=20]` arm of `multisig_key_count` (`:904`) is also dead: 17 or more keys exceed 520 bytes (17×34+3 = 581), and createmultisig refuses that first.
- Mutants RV-8 (drop the 17..20 arm) and RV-9 (drop P2PK inference) survive, and both are equivalent.
- Fix: say "only `sh(multi(...))` and, for a hybrid key, `addr()` are reachable; the other arms keep Core's shape", or drop the dead arms.

### n-4: the implementer's mutant definitions weren't kept

- `/work/scratch/rp1a-3a/mut.sh` runs `A6MUT=1..13` in `/work/scratch/rp1a-3a/mut`, which is gone, so the 13 mutants can't be identified. The logs (`mut1.log`) do match the claim: 12 killed, mutant 3 survives. Commit b7fc96ee9 identifies mutant 3 as `sh()`'s `IsSolvable` recursion. I agree it is equivalent: parsing refuses `addr()` and `raw()` inside `sh()` (`:606-620`), and every other inner descriptor is solvable.
- Fix: keep the mutant patch (or a list naming them) next to the logs.

## Checked and ran

Reviewer copy: `/work/scratch/rp1a-3a-rv-opus`, a reflink of the frozen worktree. HEAD is 46b4b003c5b8c66f3862c9b9fa8f86c54bd65afd. `git status --short` was empty at copy time. It now shows my review mutants and probes, all gated behind `RVMUT`/`crate::RV` and `#[ignore]` tests: `bech32.rs`, `bech32/tests.rs`, `descriptor.rs`, `key_io.rs`, `lib.rs`, `message.rs`, `util.rs`, `util/tests.rs`. The mutant runner and logs are in `/work/scratch/rp1a-3a-rv-opus-mut/` (`run.sh`, `m*.log`, `probe-port.txt`), and the throwaway dashd datadir (stopped) is under it too.

Read against Core line by line:
- **util.rs** vs `rpc/output_script.cpp`, `rpc/signmessage.cpp`, and `rpc/util.cpp` (HexToPubKey, AddAndGetMultisigDestination, DescribeAddress, ParseRange/ParseDescriptorRange). Checked error codes, exact texts, check order (hex before length, range before descriptor, null-range counting), and result field order. No divergence.
- **key_io.rs** vs `key_io.cpp` (DecodeDestination's branch order and error selection, DecodeSecret, Ext key decode, the DIP-18 decode/encode/script), `base58.cpp` (whitespace, NUL, the leading-zero limit, buffer sizing), and `standard.cpp` (Solver/ExtractDestination, MatchPayToPubkey, MatchMultisig, GetMultisigKeyCount). No divergence. The 112-char Base58Check cap is sound: past it Core's limited decode also fails, and the unchecked fallback still runs on the original string.
- **bech32.rs** vs `bech32.cpp`: CheckCharacters, Decode, both LocateErrors branches, the tie rule, the GF tables, PolyMod, and ConvertBits. No divergence.
- **message.rs** vs `util/message.cpp`, CPubKey::RecoverCompact, CKey::SignCompact, and Dash's DecodeBase64/EncodeBase64/ConvertBits. `(h-27)&3` and `&4` on a u8 agree with Core's int arithmetic for all 256 header bytes. Empty, `====` and stray-bit inputs agree too.
- **descriptor.rs** vs `script/descriptor.cpp` (ParseKeyPath, ParsePubkeyInner/ParsePubkey, ParseScript, CheckChecksum, the providers' ToString and GetPubKey, MakeScripts, InferPubkey/InferScript) and `spanparsing.cpp` (Dash's Expr has no `{}`, and the port matches that).
  - Apostrophe flag flow: the origin sets it, then `*'`, then path elements. Last one wins, shared by the origin and the key.
  - Bare multi is capped at 3 keys and P2SH at 520 bytes. `sh()` with leftover input fails with an empty error. CPubKey `operator<` matches `Vec<u8>` ordering for valid keys.
  - The `combo()` scripts and getScriptFromDescriptor's pick match.
  - No divergence.
- **46b4b003c** (EINTR retry): correct. Both reads in `exchange` retry `Interrupted`, the close watch restarts its timeout, and the compared bytes don't change.
- **Claims**:
  - DIFF edge cases: va 20 + cm 11 + gi 23 + da 23 + vm 10 + sm 2 = 89, and 125 + 89 = 214, as stated.
  - Mutant log: 12 of 13 killed, mutant 3 equivalent.

Ran:
- `cargo test -p dashd-rpc` in the copy: 158 passed, 0 failed.
- 8 env-gated review mutants (one build; the lib test binary rerun per `RVMUT`):

  | Mutant | Change | Result |
  |---|---|---|
  | RV-1 | bech32 tie → Bech32m | **survives** (m-1) |
  | RV-2 | range `>=` → `>` | survives unit; DIFF kills (n-2) |
  | RV-4 | payload characters checked before checksum length | **survives** (n-1) |
  | RV-7 | base64 checked before the key-address check in MessageVerify | killed (`core_message_verify_vectors`) |
  | RV-8 | drop the 17..20 key-count arm | survives, equivalent (n-3) |
  | RV-9 | drop P2PK inference | survives, equivalent (n-3) |
  | RV-10 | hardened xpub can't find the root key | killed |
  | RV-11 | uncompressed WIF read as compressed | killed (4 tests) |
- Brute-force tie search (`bech32/tests.rs::rv_find_tie`, ignored), then those inputs and the n-1 inputs against dashd v24.0.0-rc.4 regtest.
- Ten extra descriptor probes run through both the port (`util/tests.rs::rv_probe`) and dashd rc.4: `getdescriptorinfo` plus `deriveaddresses` with Core's checksum. All byte-equal:
  - an uncompressed WIF in `pkh()`, `combo()` and bare `multi()` (the last gives "Descriptor does not have a corresponding address")
  - a hardened `tpub/0'/1` derived through a WIF of the same root key inside `sh(multi())`
  - the same without the WIF ("Cannot derive script without private keys")
  - 7 vs 8 uncompressed keys at the 520-byte P2SH limit (465 OK; 531 refused)
  - a WIF with a path, a whitespace-only key, and an empty checksum after `#`
- Not run: tier 1 and the full DIFF-RPC suite (the implementer reports them green).

No Dash Core security note.

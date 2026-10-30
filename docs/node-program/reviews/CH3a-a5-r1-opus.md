# CH3a a5 (DisconnectBlock): independent consensus review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `node/ch3a-a5` at c0afbd91c, only the a5 delta: `git diff d577a1ad3 f5bbb7cd2` (cbd42f0e6, 5cb6e2b0a, 757c52804, f06509261, f5bbb7cd2). c0afbd91c only follow-merges docs. |
| Against | Dash Core `v24.0.0-rc.3`: `validation.cpp` (`v`) :2252-2278 `ApplyTxInUndo`, :2282-2378 `DisconnectBlock`, :3249-3330 `DisconnectTip`, :5029-5160 `VerifyDB`, :5222- `ReplayBlocks`; `node/blockstorage.cpp` (`bs`) :509-575 `UndoWriteToDisk`/`UndoReadFromDisk`, :649-652 `OpenUndoFile`; `coins.cpp` :70-108 `AddCoin`, :129-149 `SpendCoin`, :162-165 `HaveCoin`, :342-353 `AccessByTxid`; `undo.h` `TxInUndoFormatter`; `streams.cpp` :24-28 `AutoFile::read`; `hash.h` :171-201 `CHashVerifier` |
| Context | CH3 §2.10.2, §2.11, §2.12, §7.1, §8.1 (a5 row); CH3a notes D103–D115 on the branch; BL-33, and BL-276 on node/integration |
| Method | Read line by line against Core. `git archive` of c0afbd91c in /work/scratch/ch3a-a5-review with a target reflink-seeded from the implementer's (both deleted afterwards). Ran the `dashd-chainstate --features crash` lib suite in full twice (565/565 both times), the `disconnect roundtrip` filter 31 times, `dashd-store flatfile::tests` (43/43), and `contrib/node/ch3a-connect-census.py` against an rc.3 tree (0 failures). 15 hand mutants, each against `connector::disconnect` + `crash::roundtrip` (single-threaded) and, for ST2's, `dashd-store flatfile::tests`. I did not rerun D107's dashd oracle. |
| Verdict | **CHANGES REQUIRED**: one Major, and it is test-only. The consensus code has no B or M items and I would accept it as is. |

## 1. Branch-for-branch parity

Everything below matches rc.3.

- **`DisconnectBlock`** (`connect/disconnect.rs:51-208` against `v:2282-2378`), in order:
  1. H1's scheme context for the tip only. This is `DisconnectTip`'s guard (`v:3272-3273`); `VerifyDB` has none per block (its guard at `v:5044` only restores).
  2. G1. It is port-only and unreachable while G1's startup check holds (§2.12 row 1, D109).
  3. DIP3 at `pindex`, then `VerifyBestBlock`, `AbortNode(EvoDbInconsistencyMessage())` and `Failed` (`v:2287-2292`). `active_at(id, Dip0003)` is `DeploymentActiveAt(*pindex)`.
  4. A null undo position (`undo_pos` is `None` without `HAVE_UNDO`, as `GetUndoPos` is) gives "no undo data available".
  5. `UndoReadFromDisk`, then the `vtxundo.size() + 1` check, then H9 before any coin (`v:2299-2313`). Each failure is `Failed` with Core's line.
  6. The transactions from last to first. For each one, every spendable output is spent with moveout and compared on presence, `out`, height and coinbase flag (`v:2333-2344`). `overlay.spend` returning `None` for both "absent" and "spent in the child" is right: Core's `SpendCoin` on a spent cache entry returns true with a null coin (`nValue == -1`), which never equals a valid output, so `fClean` turns false either way. `fEnforceBIP30` is literally true, so `is_bip30_exception` is always false and the port drops it. That is equivalent.
  7. Then, for transactions other than the coinbase: the `vprevout.size()` check, and the inputs from last to first (`v:2347-2359`). `zip(..).rev()` is safe because the lengths were just checked equal.
  8. H6 (`WriteBestBlock(pprev)`) and H7 with `undo = true`, on `Ok` and `Unclean` alike, as Core's `v:2365-2371` are. H1's commit runs only for `tip && clean`. Core commits the scheme only after a successful `DisconnectTip` (`v:3320`), and `DisconnectTip` gets there only on `DISCONNECT_OK`.
- **`ApplyTxInUndo`** (`:216-242` against `v:2252-2278`):
  - `HaveCoin` comes first and sets `fClean`.
  - Then the height-0 path: `access_by_txid` over the overlay scans 0..`MAX_OUTPUTS_PER_BLOCK`, as `coins.cpp:344-353` does. It sees coins this disconnect has already restored, which is what makes the reverse input order matter.
  - With no alternate, the result is `Failed` with no log, as in Core. With one, both height and coinbase flag are overwritten (`with_metadata`), which matches `undo.nHeight`/`undo.fCoinBase =`.
  - `add(.., possible_overwrite = !clean)`: `CoinSrc::pack` skips an unspendable coin (`coins.cpp:72`) after the `HaveCoin` and legacy checks, so an unspendable undo coin can still make the result unclean or failed, as in Core.
- **Error classes.**
  - Fatal: a coins read error (`spend`, `have_coin`, `access_by_txid`), `add`'s `logic_error` (unreachable, since `possible_overwrite` is `HaveCoin`'s answer), and H9/H6 hook faults.
  - `Failed` with `UndoReadFromDisk: <text>` then `DisconnectBlock(): failure reading undo data`: every rev-file failure, whether open, EOF, EIO, decode or checksum. This is Core's class: `AutoFile::read` throws and `UndoReadFromDisk` returns false.
- **The commit rule** (`connector.rs:269-299`, D106):
  - `Active` commits only `Ok`, and an `Unclean` or `Failed` tip disconnect rolls the evo transaction back. This is `v:3281-3287`, where `dbTx` is dropped without `Commit`.
  - `Scratch` keeps `Ok` and `Unclean` with the parent as best block (`v:5113-5122`).
  - `Failed` commits nothing on either target.
  - The prefetch map is cleared only after an `Active` commit. §2.11 says "every disconnect", but a failed one changes no coin, so D106's narrowing is sound.
- **`UndoReadFromDisk`** (`blocks.rs:971-1059` against `bs:540-575`):
  - The decode starts at `pos`. The checksum is `SHA256d(prev ‖ consumed bytes)`, compared with the 32 bytes right after what the decode consumed. That is `CHashVerifier`'s hash of exactly what `>> blockundo` read, then `filein >> hashChecksum`.
  - The size field is read best-effort (`size_hint(..).ok()?`). Core never reads it, so a failed read of it is rightly swallowed.
  - A hint above `MAX_UNDO_RECORD_SIZE`, or past the file's end, falls back to 64 KiB.
  - The read doubles only on `EndOfData` and is capped at `avail`, so the buffer never outgrows the file.
- **Can a byte sequence be accepted by one side and rejected by the other?** I found none. The decoder is streaming and its only non-EOF errors (`NonCanonicalCompactSize`, `CompactSizeTooLarge`, `VarIntTooLarge`) are decided by the bytes already consumed. So a non-EOF error on a prefix is Core's error on the whole stream, and a success on a prefix consumes exactly the bytes Core consumes. Some edge cases:
  - A position past the end gives "end of file" in both. Core's `fseek` past EOF succeeds and the read then hits EOF.
  - A position inside the header decodes from that byte in both (`odd_positions_read_as_a_stream`).
  - EIO maps to "fread failed", as with Core's `ferror` without `feof`.
  - The only class with no Core line is `try_reserve` failure, logged as `std::bad_alloc` (D112). It is log-only and both sides give `Failed`.
- **Fresh fd per read:** `OsFs::open_read` is `File::open` on every call, apart from the handle cache (`fs.rs:219-222`). The trait method has no default, so every `FlatFs` has to choose. `FaultyFlatFiles` and the test filesystems open by path or forward. A file unlinked under a cached write handle does not open, and that is tested in both crates (see M7).

## 2. Tests and fuzz

The directed tests cover every row of §2.10.2 and §7.1:
- a flipped record byte and a flipped checksum byte;
- a cut record and a cut checksum;
- unlink;
- a non-canonical count;
- a record of the wrong size and a transaction's wrong count;
- H9 false;
- G1 and H2;
- legacy with and without an alternate;
- the reverse input order shown through the legacy path;
- the size field at 0, `len ± 1`, past the file, and `u32::MAX`;
- unclean at the tip commits nothing; scratch keeps unclean; a failed scratch disconnect leaves its view; the prefetch is cleared only on commit.

`connect_disconnect_roundtrip` checks every result class and every touched coin against an independent reference over a map, under random damage. `undo_decode` compares `read_undo_stream`, under any size field and any first read, with a whole-file stream reference. The census passes with the three new ranges.

### Mutants

| # | Mutant | Result |
|---|---|---|
| M1 | drop the transactions' `.rev()` (`disconnect.rs:160`) | killed (`a_disconnect_restores_…_reverse_orders`, roundtrip) |
| M2 | drop the inputs' `.rev()` (`:184`) | killed (`inputs_are_restored_last_to_first`) |
| M3 | skip `HaveCoin` for `fClean`, keeping `possible_overwrite` right | killed (`an_unclean_tip_disconnect_commits_nothing`, roundtrip) |
| M4 | `Unclean` returned as `Failed` | killed |
| M5 | checksum over the size field's length instead of what was consumed (`blocks.rs:1033`) | killed (3 `flatfile::tests`, and the chainstate size-field test) |
| M6 | commit `Unclean` on `Active` (`connector.rs:271`) | killed |
| M7 | `OsFs::open_read` through the handle cache (`open_by_path`) | killed in both `an_undo_read_opens_the_file_by_path` (store) and the chainstate Unlink case (`disconnect.rs:553`) |
| M8 | `possible_overwrite` always true in `ApplyTxInUndo` | **survives**: equivalent. Only the FRESH flag differs, and nothing persisted changes |
| M9 | legacy entry with no alternate → `Unclean` | killed |
| M10/M11 | output comparison ignores coinbase flag / height | killed (roundtrip's `changed`) |
| M12 | H1 commit on an unclean tip disconnect | killed |
| M13 | H9 removed | killed |
| M14 | trust the size field (stop at the hinted read) | killed |
| M15 | `Scratch` drops `Unclean` | killed |

## 3. Findings

### Major

- **M-1. `a_failed_undo_read_fails_the_disconnect_with_cores_lines` is flaky under the default parallel test harness.**
  - It asserts that each expected log line's process-wide count rose by exactly one (`chain/tests/connector/disconnect.rs:559-561`, and `:575`/`:580` for the reorg case).
  - `accept_levels` counts every captured line from every thread (`chain/tests/support.rs:259-262`). Other tests in the same binary log the same texts at the same time: `crash::roundtrip`'s 300 inputs (flipped bytes, unlinked rev files), `recovery::tests` (`DisconnectBlock(): failure reading undo data`) and the file's other disconnect tests. The texts are `DisconnectBlock(): failure reading undo data`, `UndoReadFromDisk: Checksum mismatch` and the EOF line.
  - Measured on the review copy with the implementer's binary: `cargo test -p dashd-chainstate --features crash --lib -- disconnect roundtrip` failed 7 of 31 runs. The failures were at `:561` (`left: [1, 5, 14]`, `right: [1, 5, 13]`; `[4, 5]` against `[3, 4]`; `[2, 2]` against `[1, 1]`) and at `:580` (`12` against `11`). The test passes alone, and with `--test-threads 1`. The full lib suite passed both times I ran it, because the roundtrip seldom overlaps this test there, but nothing prevents the overlap, so the gate can fail on it.
  - The consensus code is not affected. The risk is a red gate that costs a gate re-run, or a real line-count regression hidden behind "flaky".
  - Fix: count only lines from this test's world.
    - The disconnect runs on V's thread, so record V's `ThreadId` in the world and filter `CAPTURED` by it, as `levels_here` does for the test thread.
    - Or assert on lines that carry the block hash (`tip_failed_lines` already does).
    - Or make every test that logs these texts share one `static` lock.
  - Do not loosen the assertions to `>=`: the "exactly once" is D107's oracle claim.

### Minor

None.

### Nits

- **N1. D104 says a5 "closes BL-33's undo half", but recovery still reads undo the old way, and BL-276 does not say that this is observable.**
  - `recovery/replay.rs:440` reads through ST2's `read_undo`, which bounds and trusts the size field (D111).
  - Core's `ReplayBlocks` disconnects through `DisconnectBlock` and so through `UndoReadFromDisk` (`v:5275`). With a damaged size field, then, Core's startup replay rolls back cleanly, while rdashd's init fails with `ReplayBlocks(): DisconnectBlock failed`. BL-33's ~489 MB forged-size allocation also stays reachable on that path.
  - Fix:
    - Scope D104's sentence to `DisconnectBlock` (VerifyDB and tip disconnects).
    - Keep BL-33's undo half open until BL-276 lands.
    - Add to BL-276 that a node can fail to start where Core starts, so it is prioritised as a parity fix, not a tidy-up.
- **N2. M8 is a live but equivalent mutant.** No test pins that `ApplyTxInUndo` restores a coin as FRESH where Core's `AddCoin(possible_overwrite = false)` would (`coins.cpp:80-98`). It changes no persisted state, only flush work and CH2's flag parity. If CH2 claims flag-for-flag parity with Core's cache, one assertion on the restored entry's flags in `a_disconnect_restores_the_coins_in_cores_reverse_orders` would pin it. Otherwise leave it.
- **N3. D110's `#[cfg(not(test))] debug_assert!(false)` can fire on checksum-valid disk bytes in any debug build that links the library**, for example dashd-node's integration tests or a node-fuzz build with debug assertions. That is D110's intent, and imports normalize legacy records. I note it only so that a future "panic in debug" report from a Core datadir copied without import is recognised as this. No change needed.

## 4. Possible security note

None from this slice.

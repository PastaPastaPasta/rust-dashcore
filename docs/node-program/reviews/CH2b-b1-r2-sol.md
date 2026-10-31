# CH2b b1 — Sol narrow round 2

**R1-1 is closed by mechanism. No blocker or major was found; one minor diagnostic mismatch remains.** Pinned head: `eb1089e9005d4267b51328aea1b1ee218ed35d5e`; base: `96f9b2cc7`. Reference: Dash Core **v24.0.0-rc.3**, `da567d7dba945143eee4d2ce1621c21b32089989`. Scope: R1-1 closure, block/undo read parity, registered corrupt-store deviations, and the shutdown PF0 row's other counters. The manager's write-stall ruling, commit 6's CRASH suite and later CH2b checkpoints are outside this review.

I did not edit, commit or push the branch. Scratch and test temporary files are under `/work/scratch/ch2b-r2-sol/`; tests use `nice -n 10`, `CARGO_BUILD_JOBS=8`, an empty `RUSTC_WRAPPER`, and mode-0700 disk `TMPDIR` (removed when empty at completion). No private files were read; PLAN §7 codenames only. The tree was clean initially. An external, uncommitted edit to `benches/coins_flush.rs` appeared during review, and the branch advanced to `ee034f61f` during finalization. Those later changes are untouched and excluded. Exact pinned recovery, metadata and benchmark sources are preserved under `/work/scratch/ch2b-r2-sol/pinned/`, with `pinned-sources-sha256.json`.

## Evidence collected

- Read `REVIEW-COMMON.md` and prior `CH2b-b1-r1-sol.md`. The original R1-1 reproducer and build arguments are preserved under `/work/scratch/ch2b-b1-sol/`.
- Confirmed the requested CH2b head and a clean working tree before inspection.
- Rebuilt the preserved R1-1 repro against this head's freshly built production library. The only API adaptation adds `RecoveryConfig::pow` from the same chain parameters as the real index loader. Its old success expectation now fails at `recover(...).unwrap()` with `UnableToReplay`; see `/work/scratch/ch2b-r2-sol/original-repro-api-adapted.log`.
- Fresh real-index probes reject zero, a valid but unmet tighter target (`0x1f00ffff`), negative (`0x20800001`), overflow (`0x23010000`), and above-limit (`0x2100ffff`) targets. Every rejected case leaves the write epoch unchanged, no coins, the original marker, and a clear latch. The valid `0x207fffff` control replays and finalizes. Source, build commands and logs are preserved in `/work/scratch/ch2b-r2-sol/`.
- Source audit: both undo and forward legs call the same private `read_block`, whose only decoder call is in `read_block_at`. The decoded header's `bits()` feeds `check_proof_of_work`; index-hash comparison follows it. `Opened::recover` and `Opened::recovery` share this step machine. No separate block-import/reindex scanner is introduced by b1; its interrupted-wipe path resumes the wipe and leaves CH5 to rebuild.
- A further scratch-only probe puts an invalid-PoW block **inside** a three-block real index and sets the sub-batch bound to zero. The first valid block is written, the second fails the header check, the third is never replayed, and no final batch runs. Reopening and replaying the surviving sub-batch rejects the same block again, retaining the marker and only the first block's coin. This checks the step-machine path, progress after an earlier write, and retry rather than only failure at the first block.
- The committed R1-1 test passes its forward, undo, PoW-before-hash and valid-control cases. Disabling only the production header check in the scratch copy makes it fail (exit 101, replay unexpectedly succeeds). The branch is never mutated. Logs: `baseline-a_block_whose_header_lacks_its_proof_of_work_cannot_be_replayed.log` and `pow-disabled-a_block_whose_header_lacks_its_proof_of_work_cannot_be_replayed.log`.

## Core read order and undo checksum

At `recovery/replay.rs:434–478`, the sequence is position/open, full decode, PoW over the **decoded header's own** target with `RecoveryConfig::pow`, then index-hash comparison. The null position emits Core's null-position open-failure text without touching a file, matching `FlatFileSeq::Open`'s early null return (`flatfile.cpp:36–38`). Core's two `ReadBlockFromDisk` overloads are `node/blockstorage.cpp:805–848`. A header failure uses `ReadBlockFromDisk: Errors in block header at <pos>` followed by the correct forward or undo caller line (`validation.cpp:5189`, `:5271`), and returns the init error at `node/chainstate.cpp:252`; it does not trip the latch. No alternate byte-read/decoder path was found in the series' flush or recovery production code.

The index-hash failure's surrounding text matches `node/blockstorage.cpp:843–844`. Its entry description is deliberately the hash, rather than Core's `CBlockIndex(pprev=%p, nHeight=..., merkle=..., hashBlock=...)` (`chain.cpp:16–20`). The DEC-165(3) marker explicitly records this non-identical substitution. It occurs only after a valid-PoW read with an index-hash mismatch.

The undo checksum uses `on_branch(b, b.height - 1).hash` at `replay.rs:403–409`, hence the **index parent**, as Core's `verifier << pindex->pprev->GetBlockHash()` does (`node/blockstorage.cpp:560`). The committed regression rejects a record checksummed over a different block-file parent, without writes. Restoring the wrong parent in a scratch source mutation makes that test fail (exit 101). A new positive control keeps the block-file/index-parent disagreement but checksums over the index parent: rollback succeeds. Thus the test is not passing merely because all inconsistent headers are rejected. Evidence: `baseline-r2_.log`, `checksum-file-parent-the_undo_checksum_is_over_the_index_parent.log`, and `build-extra-tests.py`.

The eight-case read-error table passes: missing/permission-denied block file, failed read, injected EOF, truncated block record, oversized size field, missing undo file, and truncated undo record. Its open/EOF/`fread`/checksum texts match the cited Core paths for those cases. R2-1 below covers an additional actual open failure the table does not exercise.

## Registered differences and DEC-180

The size-field/position checks (the existing ST2 BL-33 difference), decoder-reason substitution, checksum-before-decode ordering and index-description substitution are marked at their failure sites. The first is confined to anomalous record framing or allocation failure; the latter three affect diagnostics or ordering when reads/decodes/checks fail. They do not remove PoW or index-hash validation, accept a bad checksum, or change healthy replay's coin operations. The earlier `HAVE_DATA` check at `new` remains the accepted D-b1-g corrupt-metadata ordering difference. No closed r1 item is reopened.

Quirks **43 and 44** are registered with dedicated passing tests. For quirk 43, Core ignores a failed block-file-info decode up to `last_file` and ends its subsequent scan (`node/blockstorage.cpp:364–375`); Rust fails the load. For **DEC-180 / quirk 44**, an absent or empty marker remains consistent, a complete nonempty vector with count other than two reaches Core's `unknown inconsistent state`, and a partial hash record fails open with Corruption / `Error opening block database`. Core turns a failed vector decode into an absent marker (`dbwrapper.h:322–326`, `txdb.cpp:105–111`, `validation.cpp:5230`). The Rust difference is confined to malformed local records, is fail-closed, and adds no peer-visible or consensus difference on a healthy store. The register selftest's two changed cases now derive the final id/test from the register; neither retains the hard-coded 42 assumption.

## PF0 shutdown row

**The pinned row's other counters are sound.** At `coins_flush.rs:1079–1104`, setup fills a deterministic cache to LARGE outside the region; the region executes the real ALWAYS flush, explicitly runs `AfterBk`, waits for background work to finish, checks the durable tip, and returns the state so destruction is outside the region. These boundaries do not omit the post-release flush or outstanding background jobs. The coins-only budget, quiet database and ungated page faults are the accepted fixture choices.

I inspected `/work/scratch/ch2b/pf0/measure-r1fix.json`: the three shutdown samples have identical read/write byte and syscall counters (4,392,286 / 359,149,268 bytes; 122 / 387 calls), instruction spread below 0.005%, and ordinary RSS/scheduling variation. That measurement is of **d9741914e**, not this head; the benchmark and its executed shutdown flush/cache/engine implementation are unchanged between those checkpoints. This is corroborating counter evidence, not a new PF0 PASS. The manager's write-stall/allocation-noise ruling is left separate, and the subsequent uncommitted benchmark experiment is not assessed here.

## Validation

`cargo test --locked -p dashd-chainstate --all-features -- --test-threads=4` passed **183 unit tests, both quirk integration tests, and both doctests**, with no failures or ignored tests (`chainstate-tests.log`). The production library used by the standalone repros was freshly built by that run. The scratch harness's two added probes and both existing regressions pass; the two targeted source mutations each make the corresponding regression fail. Exact rustc arguments and logs are retained in scratch.

`cargo fmt --package dashd-chainstate -- --check` and `git diff --check 96f9b2cc7..eb1089e90` passed. `check-quirks.py --source-only --core ...` passed with **44 quirks / 44 tests**; this static check does not claim a full compiled workspace inventory. I did not rerun the workspace gate, the manager's performance gate, or the ST3a CRASH suite.

## Findings

### R2-1 — minor: some genuine open failures are logged as `fread` failures

**Location:** `dashd-chainstate/src/recovery/replay.rs:781`, `open_failed`, and the `FileRead::line` fallback at line 748; the underlying handle acquisition is `dashd-store/src/flatfile/fs.rs:130`.

**Concrete scenario:** with valid block bytes and a fresh `OsFs` handle cache, exhaust a process's file descriptors (`RLIMIT_NOFILE = 64`, opening `/dev/null` until `EMFILE`). Startup replay cannot open the block file. The production library logs `ReadBlockFromDisk: Deserialize or I/O error - AutoFile::read: fread failed: iostream error at FlatFilePos(nFile=0, nPos=8)` and omits `Unable to open file <path>`. Core rc3's `fopen` failure takes `flatfile.cpp:44–46`, then `node/blockstorage.cpp:811–813`: it logs `Unable to open file <path>` followed by `ReadBlockFromDisk: OpenBlockFile failed for <pos>`. The Rust classifier recognizes only four `ErrorKind`s as open failures and explicitly puts other open failures into the read bucket. The same classifier serves undo reads.

This is a diagnostic parity gap, not a replay bypass: the experiment returned `UnableToReplay`, changed no database write epoch, and stopped. It is not one of the marked size/decoder/checksum-order/pointer-text differences. Repro source, exact build command and observed lines: `/work/scratch/ch2b-r2-sol/emfile-probe.rs`, `emfile-build-command.json`, `emfile-probe.log`. The Core result here is established by source, not an end-to-end Core replay.

**Suggested fix:** preserve the failure stage (handle open versus actual read) through the flat-file adapter and map it to Core's corresponding log path. An errno allowlist cannot fully distinguish the stages; an I/O error can occur in either. Add a real open-error regression such as `EMFILE` alongside the existing fault-injected `fread` case, or obtain an explicit ruling for this diagnostic difference.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes

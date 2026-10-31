Verdict: **APPROVE-WITH-NITS**

Focused independent closure review of ST2 at **0304cada1bd5305b2737c330537c1861ff044947**, including fixes `18006d25` and `0304cada`. Reviewed in the fresh detached worktree `/work/scratch/st2-r3-gpt/repo` on 2026-10-08. Read REVIEW-COMMON and my round-2 report, reused its reproducer source and release binaries, and inspected Core using `git show` at **v23.1.8** and **v24.0.0-rc.3**. No reviewed source edits, commits, pushes, or GitHub posts. Evidence is under `/work/scratch/st2-r3-gpt/`.

1. **Round-2 major 1 — CLOSED: the scanner report is now bounded.**

   **Location:** `dash-coreimport/src/blockfiles/external.rs:360–390`, event dispatch at `:427–433`, `:469–470`, and `:481–482`.

   Re-ran the original generated `Read` reproducer, with its sink retaining nothing. The report counts all records while retaining only eight header positions:

   ```text
   records=100000  input=8800000  retained_headers=100000  capacity=8 retained_bytes=64
   records=1000000 input=88000000 retained_headers=1000000 capacity=8 retained_bytes=64
   ```

   Each of the three diagnostic vectors is capped at eight positions: at most 192 bytes of position storage, independent of input length. The 1,000/30,000-record regression passes for both unknown-parent records and malformed `New` records, including all three counts and constant capacities. Full collection is caller-owned through no-op-by-default hooks. Neither the report cap nor its hooks truncate the scan. Evidence: `scan-reproducer.log`, `tests.log`.

2. **Round-2 major 2 — CLOSED: the original abort is replaced by bounded, typed errors.**

   **Location:** `dashd-store/src/flatfile/blocks.rs:733–755`, `:765–779`.

   Re-ran the `0xffffffff` sparse blk reproducer under a 256 MiB address-space limit: it returns `Corrupt`, exit **0**, before reserving the claimed payload; round 2 exited **134**. The sparse regressions reject `MAX_BLOCK_RECORD_SIZE + 1`, `MAX_UNDO_RECORD_SIZE + 1`, and near-u32-max undo sizes without a large allocation. Checked conversion and `try_reserve_exact` cover representability and allocator refusal; `resize` then stays within the successfully reserved capacity.

   Independently forged an undo size of exactly **488,921,945** over a real one-byte Core undo, extending the file sparsely. Rust returns `Io`/`OutOfMemory`, capacity zero, under the same limit at both pins. The ignored 400 MB reservation regression also passes under `prlimit --as=268435456`. Evidence: `scan-reproducer.log`, `forged-core.log`, `undo-limited-test.log`. The remaining within-bound resource cost is discussed in finding 4; this closure does not claim protection against an operating-system OOM kill after an overcommitted allocation succeeds.

3. **Nit — the undo ceiling is safe at both pins, but its explanation has a false premise and calls a conservative bound an exact maximum.**

   **Location:** `dashd-store/src/flatfile/blocks.rs:38–49`; `dashd-store/ARCHITECTURE.md:66–71`.

   The comment says “every non-coinbase transaction has an input.” Both pins explicitly allow empty inputs for quorum commitments, MNHF signals, and asset unlocks (`src/consensus/tx_check.cpp:16–24`). Such transactions still contribute an empty `CTxUndo` to `vtxundo`, so charging every transaction's count to an input is not a complete proof. This is a documentation defect, **not** an undersized limit.

   The coin-size terms themselves are safe at both pins. `Coin::nHeight` is 31 bits (`src/coins.h:40–41`); `TxInUndoFormatter` writes its doubled code and the one-byte dummy (`src/undo.h:29–35`); amount compression fits uint64 and therefore at most ten VARINT bytes; an uncompressed 10,000-byte script has a two-byte size code (`src/compressor.h`). `CCoinsViewCache::AddCoin` excludes unspendable scripts (`src/coins.cpp:70–72`), and `IsUnspendable` excludes lengths **greater than** 10,000 (`src/script/script.h:534–536`), leaving length 10,000 allowed. Special compression only reduces size. Undo generation supplies one coin per spent input, not additional special-transaction state (`src/validation.cpp`, `UpdateCoins` and its `ConnectBlock` call).

   An independent proof that includes empty-input transactions is straightforward. Let `I` be spent inputs and `T` non-coinbase transactions. Every input costs at least 41 block bytes; every transaction has at least ten bytes of framing, including an empty-input transaction. Ignoring all coinbase bytes and using only the 80-byte header plus a minimum one-byte block transaction count gives:

   ```text
   41*I + 10*T + 81 <= 2,000,000
   serialized undo <= 5 + 10,018*I + 5*T
   ```

   Each vector count fits five CompactSize bytes. Maximizing this deliberately loose integer relaxation yields **488,658,019** bytes, below the implemented **488,921,945** ceiling by **263,926** bytes (`undo_bound.py`, `undo-bound.log`). Actual limits are tighter: this ignores the coinbase, outputs, transaction-size limits, and smaller amount/count encodings. An empty `vtxundo` needs one byte. Core's 2,000,000-byte block maximum and the relevant serialization rules hold at both pins.

   Consequently, **no undo record generated from a valid block at either pin can exceed this per-record ceiling**. A whole rev file can exceed it by holding several records; the reader does not impose a file-size ceiling. This is an upper bound, not the exact largest realizable undo.

   **Suggested fix:** call it a conservative ceiling and replace the nonempty-input assertion with a proof accounting for transaction framing and empty undo vectors. Preserve the constant unless a tighter, independently justified bound is useful.

4. **Minor, nonblocking — a forged within-bound undo size still buys a large allocation; streaming would remove that amplification.**

   **Location:** `dashd-store/src/flatfile/blocks.rs:748–755`, `:779–788`; deviation 6 at `dashd-store/ARCHITECTURE.md:59–71`.

   On the one-byte Core undo with its size field forged to the ceiling, the unrestricted Rust scratch harness returned checksum `Corrupt`, exit 0, but retained a **488,921,977-byte** buffer including the checksum. Peak RSS was **478,756 KiB**; this unoptimized run took **44.68 s**, so the time is not a production-performance estimate. Evidence: `undo-unrestricted.log`.

   On the **same bytes**, both release binaries started with `-checkblocks=1 -checklevel=2` and returned **true** from `verifychain 2 1`. Core's `UndoReadFromDisk` streams `CBlockUndo` through `CHashVerifier`, then reads the immediately following checksum; it never reads or reserves from the framing size (v23 `src/node/blockstorage.cpp:516–548`; v24 `:540–574`). For this record it consumes one undo byte and its original checksum. Core can still consume substantial memory on a genuinely large decoded undo; streaming avoids the extra cost induced solely by a forged framing field.

   **Assessment:** acceptable for this ST2 closure as a finite, fallibly reserved raw-byte API under the already explicit deviation 6; the original unbounded-u32 allocation/allocator-abort finding is closed. It is not equivalent to Core's resource behavior. Before treating attacker-shaped local rev files as cheap to read in full-node integration, prefer a streaming undo decoder/checksum reader that captures only consumed serialization bytes, or expose an explicit caller memory budget. Do not simply lower the universal protocol ceiling: large legitimate undo records must remain readable. Treat `try_reserve_exact` as handling allocator refusal, not guaranteeing recovery from system-wide memory exhaustion.

5. **Compatibility observation — the requested “never rejects anything Core reads at an indexed position” property is false, within the existing declared deviation.**

   **Location:** `dashd-store/src/flatfile/blocks.rs:735–737`, `:765–766`; `dashd-store/ARCHITECTURE.md:59–71`.

   I repeated the both-pin differential with an intact indexed genesis payload and only its size field changed to `0xffffffff`, extending the blk file sparsely. Both Core binaries returned the **306-byte** genesis from `getblock <genesis> 0`; Rust returned `Corrupt`. Core reads from `nDataPos`, deserializes, checks proof of work and the indexed hash, and ignores the preceding size (v23 `src/node/blockstorage.cpp:748–789`; v24 `:805–849`). It does not apply the scanner's framing-size check there. File extension is not even necessary for Core's read; it was retained to reproduce round 2 exactly.

   Thus the new bound **does reject some records Core's indexed reader accepts**. No honest proof can identify the scanner's 2 MB framing limit with indexed-reader acceptance. Conversely, normally written valid Core blocks have canonical serialized size at most 2 MB; normal Core writes and records indexed through the external scanner do not acquire a larger accepted framing size. The maximum-size framing regression passes and still returns a shorter block plus trailing record bytes within the limit.

   This is the documented framing-read deviation that round 2 already accepted while requesting a bound, rather than a new ordinary-file parity regression. Exact indexed-read acceptance would require bounded streaming deserialization from the indexed position for blocks as well as undo. Preserve this qualification in any “Core-identical reads” claim.

6. **Report/scanner regression check — no regression found in the examined semantics or byte parity.**

   **Location:** `dash-coreimport/src/blockfiles/external.rs:427–433`, `:466–492`; `dash-coreimport/tests/scanner_parity.rs:286–290`; `fuzz/fuzz_targets/dash_coreimport/external_scan.rs:51–67`.

   Tally updates and hooks occupy the old vector-push sites. Headers are still counted before a torn remainder fails; `undecodable` remains restricted to `New` decode failures; unexpected-data offsets remain `rewind - 1`. Header classification, rewind positions, read limits, interruption, and block/after callbacks are unchanged. The differential test now uses total counts, not the eight-position samples. The fuzz target checks every header/unexpected-data event through hooks, preserving its prior exhaustive position assertions.

   Independent validation at the reviewed revision:

   - Targeted build/tests: **27 importer unit tests, 35 store unit tests**, and the remaining targeted integration/doc tests pass; the resource-heavy ignored store test was run separately as described above. Unconfigured oracle tests in the initial run skip; the configured differential runs below actually exercise Core.
   - Both-pin scanner differential: **208/208** damaged files match `dashd -loadblock` byte-for-byte: 200 cases using 401 blocks, plus eight using the full 40,001-block dataset and ring-fill damage. Includes default and fastprune cases. `scanner.log`.
   - Existing dataset parity: **40,001 blocks, both blk/rev files byte-identical**. `parity.log`. I did not regenerate the author's full 16-case matrix in this focused round; round 2 already independently ran that matrix, and neither closure commit changes writer logic.
   - Targeted all-target Clippy with `-D warnings` and workspace `cargo fmt --all -- --check` pass. `clippy.log`, `fmt.log`.

   The release binary SHA-256 values remain `7bdb2ce35e2be3e0e388eb9bfb99b2d742d07c813b93b970069d42043875aa36` (v23.1.8) and `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (v24.0.0-rc.3). Tests used the dedicated mode-700 `/dev/shm/st2-r3-gpt-tmp`; private dashds were stopped and that directory was removed after evidence capture. The detached review worktree remains unchanged.

Both round-2 majors are **closed**, with no new merge blocker. The undo ceiling safely covers both pins, its proof needs a documentation correction, and framed reads retain the declared compatibility/resource limitations. **Final verdict: APPROVE-WITH-NITS.**

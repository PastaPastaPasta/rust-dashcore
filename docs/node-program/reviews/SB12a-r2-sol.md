# SB12a Sol r2 review

Scope: `node/sb12a-bridge` at `f36da3f1d`, against the r1 head `1545af63f`. This is the brief's narrow follow-up review.

## Findings

None in scope.

## Verification

- **Consensus hash boundary:** `dash-consensus::Header` has private fields and `Header::new` is its only constructor (`dash-consensus/src/primitives/block.rs:18-44`). Both consensus decoders call it. The bridge's plain header and block conversions call it, and its conversions from `HashedBlockHeader` and `HashedBlock` compare the freshly computed hash with the supplied one and return `WrongHash` without a build-mode gate (`dash-spv/src/bridge/headers.rs:59-94,109-145`). The old constructor and decompressed-pair conversion are gone. PoW reads the hash bound to that header (`dash-consensus/src/header.rs:55-69`).
- **Hash count:** `headers2_to_consensus` gives base-sdk's `decode_with_hasher` a `Header::new` closure, retains the headers hashed during decompression, and hashes the remaining headers once (`dash-spv/src/bridge/messages.rs:102-119`). The pinned base-sdk decoder calls that closure at most once for each predecessor whose hash it needs. The active SPV sync path is unchanged from its pre-SB12a code: `headers2` decompresses with cached hashes, and ordinary `headers` hashes once in the manager. It does not yet cross into `dash-consensus`.
- **X11 backend:** the f1 diff changes `dash-consensus::hash::x11` from `dash_pow::hash` to `dash_x11::x11` without modifying either backend. XH1's independent differential (`docs/node-program/reviews/XH1-r1-opus.md`) found identical bytes on every mainnet header and 200,000 random inputs. This review's release capture test confirms the resulting consensus headers and blocks retain the captured wire bytes and dashcore hashes. I did not repeat XH1's full historical sweep.
- **F2, F3, vector:** the three `__x11-count` modules and call sites require `debug_assertions` when enabled by a feature, so a normal release `--all-features` library build excludes the counters. The Python guards use explicit exits before each mainnet read, including tip reads, and the optimized-mode tests pass. The synthetic `headers2` test sets cached version offsets 6 and 7, a negative 600-second time delta, a full timestamp, changed bits, and a redundant previous hash; the decoder and re-encoder checks exercise those fields. I inspected that vector but did not run its unit test in this narrow test pass.

## Validation

- `git diff --check 1545af63f..f36da3f1d`: passed.
- `python3 -I -B contrib/node/test-sb12a-captures.py`: 3 tests passed.
- `CARGO_TARGET_DIR=/work/scratch/sb12a-r2-sol/target cargo test -p dash-spv --test bridge_round_trips --release`: 1 test passed, 0 failed. The private target directory was deleted afterward.

VERDICT: APPROVED

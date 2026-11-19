# LK1a C2 ChainLock vectors

The release-v24.0.0 capture adds per-case ChainLock messages and the matched tip, fork,
quorum, and BLS-scheme state here. The node verifier tests consume these files when present;
archive CLSIGs remain supplementary and do not replace matched states.

Each `*.jsonl` record contains a `case`, `node`, `outcome`, `state`, and `clsig` object. The
`clsig` object carries `bytes` (the 132-byte wire value), `height`, `block_hash`, `signature`,
`hash` (SHA256d of `bytes`), and `scheme` (`basic` or `legacy`). A malformed-signature case may
set `expect_decode` to `false`. The `block` and `input` objects preserve the captured block and
wire input so a later verifier test can rebuild the exact state at the captured tip.

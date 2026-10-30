# Review: f7abb5ef3 (`dash-p2p` govsync/govobjvote adaptation)

Reviewed the commit against base-sdk `8382b7662b9e47faa714bb293e76dc799edfa933` and Dash Core `v24.0.0-rc.3`. I did not edit either branch.

## Finding

- **Low, nonblocking — `GovSyncRead` has two writable copies of the filter tail.** `dash-p2p/src/policy.rs:947-977` exposes `hash_funcs`, `tweak`, and `flags` as public fields, but `encode` serializes only `sync.bloom_filter`. For example, decoding a filter with `hash_funcs = 3`, then setting `m.hash_funcs = 7`, still writes 3. Before this adaptation, those outer fields were the fields `GovSyncRead::encode` wrote. The current receive path has no caller that mutates this type, and an unmodified decoded value encodes to Core's bytes. Consider making the outer values accessors of `sync.bloom_filter`, or explicitly choosing and documenting one authoritative set of fields. Add an encode assertion using the nontrivial `(3, 0xdead_beef, 2)` test vector at `policy.rs:1725-1736`.

## Wire and error checks

- **`govobjvote` remap is correct for this exact SDK pin.** The pinned `pkgs/p2p_core/src/msg/gov.rs` `GovVote::decode_msg` has one `P2pDecodeError::InvalidValue` exit: a fully read signature whose length is neither 65 nor 96. `VoteOutcome` and `VoteSignal` use open `Unknown` variants, and `P2pMsg::dispatch` leaves trailing bytes unread. Thus `gov_rest(cmd, payload, 0)` always returns the Core `bad governance vote signature size` failure in this arm. The zero is a dummy, not Core's actual stream position: `gov_rest` returns an error before it can return `consumed`. A complete bad-length signature has been read through byte `85 + len` by Core. The arm's broad `InvalidValue { .. }` match is fragile if the SDK decoder later adds another such exit; matching `field: "governance vote signature size"` would make that dependency explicit.
- **Throw order and score match Core.** For a declared length of 65 or 96 with too few signature bytes, both readers fail while reading the vector with end-of-data text, before the size check. A declared length above 96 is rejected from the CompactSize count before vector bytes are read, with `Vector length limit exceeded`. A complete length such as 64 or 95 reaches the size check and throws `bad governance vote signature size`. `governance/vote.h` performs those steps in that order; `governance/net_governance.cpp:207-211` catches each `std::ios_base::failure` and scores 100 as `malformed governance vote`.
- **`govsync` read/encode matches the complete filter.** At this SDK pin, `GovSync` contains `FilterLoad`, whose codec reads and writes `vData`, `nHashFuncs`, `nTweak`, and `nFlags` in Core's order. `GovSyncRead::decode` now consumes those same bytes once; encoding an unchanged decoded value delegates to `GovSync` and writes them once. Both Core's `CBloomFilter` and SDK `FilterLoad` cap `vData` at 36,000 during decode, before reading its bytes. Neither decoder rejects `nHashFuncs > 50`; Core's `IsWithinSizeConstraints()` check is after the full filter read and scores 100 without a throw. A truncated 32-byte object hash remains outside Core's bloom-filter catch and is only logged; failures in the filter are caught and scored 100.

## Test coverage and validation

The existing vote test checks Core's throw text for complete invalid lengths 0, 64, and 95, so it would catch an omitted signature remap; it also checks a length over 96. It checks only success/failure through `decode`. A useful addition is a vote declaring 65 signature bytes but supplying 64, asserting end-of-data text and the handler's 100 score. Another is a valid 65-byte vote with unknown outcome/signal values and one trailing byte, asserting success with `consumed == 150` and one byte left by `read`; this guards the broad remap's assumptions. The existing govsync test should also assert `GovSyncRead::encode` reproduces its nontrivial input bytes.

I attempted `cargo test -p dash-p2p` in a detached worktree with an isolated target directory. Under current box load, its shared `sccache` jobs made no compiled artifacts after several minutes, so I stopped it and removed the worktree and target. The verdict is based on source review, not a completed local test run.

The pinned decoder has no other vote `InvalidValue` path, and the changed receive paths preserve Core's consumed bytes, throws, and scores. The mutable duplicate fields are an encode API issue and the broad error match deserves tightening, but neither changes the current network read behavior.

VERDICT: APPROVE-WITH-NITS

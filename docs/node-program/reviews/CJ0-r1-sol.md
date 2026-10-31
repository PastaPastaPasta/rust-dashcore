# CJ0 dash-coinjoin — Sol r1 review

**Verdict: ACCEPT**

## Numbered findings

None. I found no actionable parity defect or new peer-triggerable panic in the reviewed diff.

## Review basis

I compared `6993f2f2f..adc4cac2d` with Dash Core `0364ed626e` at the cited functions: denominations and rebalance rules (`src/coinjoin/common.h:41-279`); collateral and entry/final-transaction checks (`src/coinjoin/coinjoin.cpp:264-503,654-767`); queue handling (`src/coinjoin/server.cpp:129-199`, `src/coinjoin/walletman.cpp:243-329`); server admission, signing, fees and timeouts (`src/coinjoin/server.cpp:67-127,201-282,302-660,711-1154,1158-1287`); client updates, final transaction and rounds (`src/coinjoin/client.cpp:304-332,414-630`, `src/wallet/coinjoin.cpp:261-425`); and DSTX/relay (`src/coinjoin/coinjoin.cpp:82-154,583-640`, `src/net_processing.cpp:2972-2983,3633-3711,6626-6635`). The oracle pin `e610a33e64` has identical `src/coinjoin/`, `src/wallet/coinjoin.cpp` and `src/masternode/meta.cpp` to the review revision.

D11 is acceptable: `dash-coinjoin/src/server.rs:840-876` chooses uniformly among *offence entries*, so duplicate entries for unsigned inputs retain the same weighting as Core's shuffle-first selection (`src/coinjoin/server.cpp:480-536`). The random stream differs, as declared; no wire or consensus rule depends on it. D12 is also acceptable: wrapping collateral sums (`dash-coinjoin/src/collateral.rs:45-67`) define Core's signed-overflow case, and any collateral that survives the fee check still needs Core-equivalent mempool test acceptance (`src/coinjoin/coinjoin.cpp:458-502`). CJ1 must implement that trait contract exactly.

The vectors are independent where claimed: `contrib/node/cj0-vectors/gen_common.cpp:16-133` calls Core's functions; `regen.sh:83-135` gets amount text from pinned `dashd`; and `gen_fully_mixed.cpp:30-57` uses Core serialization and SHA-256 for the copied, source-checked coin-flip lines. The 46 case ports and 21 session tests exercise many Core branches, but they are hand-written expectations, not a differential oracle. D5 correctly leaves the `diff_coinjoin`/session replay proof to O2a-CJ; do not describe CJ0 alone as L1 differential parity.

D17's layer P classification is consistent with the crate's pure dependencies and the mobile client use. `check-layers.py` and the L6 mobile closure check passed. The server and DSTX modules are extra code for a client build, but they introduce no node runtime or platform API into the mobile closure; BLS is also used for client queue verification (`dash-coinjoin/src/queue.rs:41-46,358-394`). There is no new unbounded collection fed by arbitrary peers: server entries are capped by accepted collaterals and pool limits (`dash-coinjoin/src/server.rs:534-546,582-608`), and queues require a tip-valid masternode and are deduplicated/pruned (`dash-coinjoin/src/queue.rs:129-183,310-394`). Unconfirmed DSTX records can persist (`dash-coinjoin/src/dstx.rs:194-209`), exactly as in Core's `IsExpired`/`CheckDSTXes` (`src/coinjoin/coinjoin.cpp:92-98,583-595`); a change to that policy needs a separate Core-parity decision.

**Validation:** `cargo test -p dash-coinjoin --all-features --locked` passed all 88 tests; `git diff --check 6993f2f2f adc4cac2d` and `contrib/node/check-layers.py` passed. `check-mobile.sh` reported L6 closure success; I stopped its full Android size build because this crate is not yet in a mobile product closure. The purity gate's dependency ban and both host Clippy feature modes passed; I stopped it during the Android Clippy build, before its link probe. The worktree remained clean; no commit or push was made.

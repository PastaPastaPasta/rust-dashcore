# SB15 pure slices: additive API

The shipped sync and query paths keep using the legacy masternode engine.

- `dash_mnlist::ListView` adds `coinbase: Option<dash_consensus::primitives::TxRef>`.
- `dash_mnlist`, with `test-util`, adds `replay_rotated_capture(&[u8], ListDecoder) -> Vec<(LlmqType, QuorumHash, QuorumState, Commitment)>` and `ListDecoder = fn(&[u8]) -> Vec<SmlEntry>`. The bytes are one gzipped rotated capture. The caller's decoder turns each of its evodb lists into SML entries, so dash-mnlist takes no normal dependency on dash-dml. Invalid fixtures panic. No member list is exposed.
- `dash_spv::ClientConfig` adds `clock: Arc<dyn dash_clock::Clock>` (system time by default) and `quorum_lookup_timeout: Duration` (10 seconds by default; `validate` refuses zero).
- `DashSpvClient::llmq_config()` returns the immutable per-client `Arc<LlmqConfig>` built at construction. Invalid chain parameters fail construction with `SpvError::Config`, before applying legacy global overrides.
- `dash_spv::client::anchor` exposes the unwired `InitialSync` driver, its `Phase`, `Input`, `Output`, and coverage retry constants. Reset outputs carry fresh Candidate metadata; the host executes durable store writes before constructing the chain. Retry rounds allow three attempts with distinct peers, followed by a 30-second delay or a new header tip.
- `dash_spv::client::mn_status` exposes the masternode owner's contract: `MnStatus` (published after each input that moves `gen`, `lookup_waits` or the snapshot), `MnCommand`, `Resolved { Answer, Asked, Busy }` and `MnHandle`.
- `dash_spv::client::quorum_lookup` exposes `QuorumQuery`, `QuorumLookupEngine` (`status()` and `resolve(query, reask)`, implemented by `MnHandle`) and `QuorumLookupBridge`. The bridge returns the engine's verified `FoundQuorum`, distinguishes not found from unavailable errors, waits on the owner's status for fetches, capacity and progress, and limits unavailable re-asks to one per query and one per type/hash per published tip. Each lookup is checked and registered in one owner step (`MnCommand::Resolve`); there is no separate tip call. The client query methods are not wired to it yet.

The Rust additions do not alter the C ABI or its error codes. The new configuration fields require updating exhaustive Rust struct initializers.

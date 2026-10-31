# BL-232 governance Tauri bindings: review r1 (Opus)

Target: dashwallet-desktop `dw/bl-232-gov-tauri` at `59f605a1`, range `9a13320e..59f605a1` (3b5d42c1, 59f605a1),
reviewed in the detached worktree `/work/scratch/dw-bl232-r1-opus/wt`. Nothing edited, committed or pushed.

## Verdict: ACCEPT WITH NITS

The binding is thin and faithful: eleven commands, each a direct facade call plus an exhaustive DTO conversion.
There's no operator-key path, the grants are least-privilege, and the error mapping is exhaustive and pinned to the
engine's `code()`. The design rows already document both real gaps a user could meet: the epoch precision loss
(D97b) and the stale clock under the `["governance"]` key (D96). The findings below are nits. N1 widens D97(b) to two
more fields it misses. N2 is a cheap way to close a gap in the contract test that I confirmed by mutation. None of
them blocks.

## Findings

### N1 (nit, Q2): D97(b) misses two attacker-chosen i64 times that also cross as JS numbers

- `rust/crates/dw-app/src/dto/governance.rs:388` `ProposalDetail.created_at: i64` is the proposal object's `nTime`.
  For proposals it has no bound at all. `dw-governance/src/validate.rs:306-309` checks only the end epoch, and the
  time window at `validate.rs:320-324` applies to triggers only. Core works the same way: `MasternodeRateCheck`
  returns early for proposals. Anyone who pays a proposal's collateral picks it, and on testnet that is cheap.
- `dto/governance.rs:444` `CurrentVoteRow.time`, and `:495`/`:501` `VotingMasternode.vote_time` /
  `key_last_vote_time`: a vote's time has only an upper bound (`dw-governance/src/store.rs:1957`, `:2140`, now + 1 h),
  so it can be any negative i64. Only a voting-key holder can choose it.
- Concrete input: a proposal object with `time = 9007199254740993` and valid collateral. `governance_proposal`
  returns `createdAt: 9007199254740992` after `JSON.parse`. A vote with `time = -9007199254740993` shows up rounded
  in `governance_current_votes`.
- Two stated invariants say the opposite and are now wrong for these fields: the module header
  (`dto/governance.rs:2-4`, "times are Unix seconds") read together with `codegen.rs:25-26`/`dto/mod.rs:13-15`
  ("Duff amounts and UNIX times fit a JSON number exactly (< 2^53)").
- Fix (doc-only is fine, as with D97b): name `createdAt` and the three vote times in D97(b), and in the
  `dto/governance.rs` header say that these "times" are peer-chosen i64s that MG-09 must range-check. The binding
  doesn't need to change.
- Everything else that could exceed 2^53 is bounded. `payment_amount` is capped at MAX_MONEY: `amount_from_number`,
  `dw-governance/src/proposal.rs:286-289`, and `parse_payment_amount` with `v <= MAX_MONEY` (`validate.rs:439`).
  `budget` and `proposal_fee` come from chain params and are ≤ MAX_MONEY. `net_yes_estimate` is a sum of u32
  weights. `votes` and `bytes_received` are local counters. The ETAs are tip time + blocks × 157.2 s. So only the
  epochs (already in D97b) and the times above can reach it.

### N2 (nit, Q4): the §3 contract test checks names and nesting, not leaf value types or enum spellings

`apps/desktop/src/api/governance.test.ts:73-88` (`expectRecord`) compares each record's key set and recurses into
nested §3 records, and it does catch drift there. I ran mutations on a reflinked copy (worktree untouched):

| Mutation of `fixture/vectors/governance.json` | Result |
|---|---|
| baseline | 4/4 pass |
| `governance_current_votes[1].weight.extra = 1` (extra field, 2nd element, nested) | **fails** (good) |
| `governance_info.proposals = null` (the only `ProposalCounts`) | **fails** via the `seen` check (good) |
| `governance_proposals[0].tally = 5` (nested record replaced by a scalar) | **fails** (good) |
| `governance_proposal.createdAt = "1790000000"` (number → string) | **passes** |
| `governance_proposals[0].status = "NOT_A_STATUS"` (enum value) | **passes** |

So a DTO change like `#[serde(with = "…string")]` on a field passes every test once the vectors are regenerated: the
Rust vectors test accepts the new JSON, ts-rs doesn't see `serde(with)`, so the generated TS keeps `number`, and
this test doesn't look at leaf types. The record and field claim in the owner summary holds, but "exactly the §3
records" is weaker than it sounds. A cheap fix inside the loop at `:78`: map §3's Rust leaf type to a JSON type
(`u32|u64|i64|f64` → number, `String` → string, `bool` → boolean, `Option<…>` → also null), and check enum-typed
leaves against the camelCased variants of the §3 `pub enum` (parsed the same way as the structs).

A smaller hole of the same kind: the command → record wiring in `governance_vectors_tests.rs:221-235`
(`"governance_proposal": dto::ProposalDetail::from(…)` and so on) is written by hand, separately from
`api/governance.rs`, and `Vectors.results` is a `JSON.parse` cast. If a command's declared return type changed, the
only thing that could notice is tsc on the one field the test reads (`governance.test.ts:146-147`). The risk is low,
because each command's `.into()` needs a matching `From<engine::X>`.

### N3 (nit, Q6): D97(a) can't be hit by a user today, and the binding treats its two string arguments differently

- The only path where a user types an outpoint is the console. `dw-console/src/governance.rs:634-641` parses the
  txid with `parse_hash_v` and the vout with `atoi_u32`, then rebuilds the canonical `"{txid}-{vout}"`, so console
  users get Core's semantics. Through Tauri, only renderer code can pass `outpoint`, and the strings the engine
  hands out (`CurrentVoteRow.outpoint`, `object.rs:168-170`) are already canonical. No screen builds one yet.
  Answer to Q6: not reachable through these bindings today, only by a future renderer that formats its own outpoint.
- In the binding, `hash` is case-insensitive (`parse_display_hex` uses `hex::decode`, `dw-governance/src/object.rs:158-165`)
  while `outpoint` is a case-sensitive string compare (`dw-engine/src/governance/reads.rs:240-243`). Concrete input:
  `governanceCurrentVotes("5C…5C", "<TXID upper>-0")` finds the proposal and then returns `[]`.
  `governanceCurrentVotes(h, "zz-0")` returns `[]`, where Core's `getcurrentvotes` raises an error
  (`ParseHashV`). `"<txid>-01"` returns `[]`, where Core's `atoi` reads vout 1.
- Optional: type the argument as `{ txid: string, vout: number }` and build the canonical string in the binding,
  parsing the txid the way `parse_wallet_id` parses a wallet id. That takes D97(a) off the renderer's surface
  without touching the engine. Leaving it to the engine row, as D97 proposes, is also fine.

### N4 (nit, Q5): put a pointer to D96's staleness note in the code

I confirmed what D96 says. A new tip sends no `governance` event: `apply_list` compares only the entries
(`dw-engine/src/governance/sync.rs:327`), and `store.set_tip` (`sync.rs:319`, `store.rs:741-743`) doesn't call
`on_change`. `events.ts:38-41` (`sync`) doesn't invalidate `["governance"]`, so `tipHeight`, `blocksToSuperblock`,
`votingOpen` and the tip-derived statuses go stale between events, and with sync off they stay stale indefinitely.
There is **no refetch loop**: no read path calls `changed()`, and `voting_masternodes` →
`masternode_keys` (public derivation, `masternode_keys.rs:204-270`) sends no governance event. A running sync does
refetch every governance query at ≤ 1 Hz, as D96 says. D96 assigns the fix to MG-09. The comment at
`apps/desktop/src/api/events.ts:91` says only "(≤ 1 Hz, mg-engine §5)". Adding "not on a new tip; see MG-02 D96"
there would put the warning where MG-09 will edit the code.

## Answers to the brief's questions

1. **Semantics, key policy.** Each command is `state.governance().await?` plus one facade call plus `.into()`
   (`api/governance.rs:29-121`). The only check the binding adds is `parse_wallet_id`, as every wallet-taking
   command does. The engine does the rest of the validation: `upcoming_superblocks` `count > 24` gives
   `invalid_argument` before the tip read (`reads.rs:150-156`), and hashes go through `parse_hash`. The error
   mapping (`error/governance.rs:30-64`) is an exhaustive `match`, `Engine(e)` falls through to the common mapping,
   and `codes_match_the_engine` ties every string to `GovernanceError::code()` and requires each code to be produced.
   The data kinds match the contract. `vote` and `relay_vote` aren't bound. `voting_masternodes` derives only
   `MasternodeKeyRole::Voting` public keys (`voting.rs:37-40`) and returns indexes and proTxHashes, not key
   material. Nothing can sign with, read or return an operator key, so MG-01 holds. `set_sync_enabled` and
   `resync` let the renderer open governance P2P sessions. That is the intended setting, and repeated `resync`
   calls don't re-ask peers inside Core's fulfilled-request window (MG-02 §2, D68), so spamming the button doesn't
   get the wallet banned.
2. **Round-trip.** Every `From` destructures both sides, so a new engine field breaks the build. Hashes and txids
   are display-order lower-case hex. Outpoints are `COutPoint::ToStringShort`. Amounts are ≤ MAX_MONEY. Values that
   can pass 2^53: `startEpoch`/`endEpoch` (D97b; input `start_epoch: 1, end_epoch: 9007199254740993` passes
   `check_proposal` since end > start and end > now), plus N1's `createdAt` and vote times. `f64` fields
   (`cycleProgress`, `objectsSecs`, `votesSecs`) would serialize NaN as `null`, but none of them can be NaN (fixed
   divisors, measured durations).
3. **Capability.** `capabilities/governance.json` has exactly the eleven `allow-governance-*` permissions,
   `windows: ["main"]` and `local: true`, the same shape as the other domain files. `tauri.conf.json` lists it.
   `api::tests::every_command_is_granted_in_its_domain_file` pins command ⇔ grant. Nothing beyond the eleven.
4. **Contract test.** Not vacuous for records and fields: three deliberate breakages fail (table in N2). The leaf
   value types and enum spellings are unchecked (N2).
5. **Events.** No loop. Staleness on a new tip, plus the 1 Hz full refetch during sync, are both real and
   documented in D96 (N4).
6. **D97(a).** Not reachable by a user through these bindings today. The console normalizes and the engine's own
   outpoint strings are canonical (N3).
7. **Dead, duplicated or simpler.** Nothing dead. The DTO derives `Deserialize` on output-only records, which
   matches every other `dto/*` module (vectors and fixtures parse them). `ErrorData::Reason` (`vote_rejected`) and
   `Call` (`not_implemented`) can't be reached through these commands until MG-05 binds `vote`. They're kept
   because the contract requires the codes, and `codes_carry_their_fields` covers them. The fixture backend
   rejects every `governance_*` command with `platform.not_implemented` (`fixture/handlers.ts:5`). That's fine
   until MG-09 adds handlers.

## What I ran

- Static review of the whole range: all Rust (`api/governance.rs`, `dto/governance.rs`, `error/governance.rs`,
  `error/mod.rs`, `state.rs`, `coverage.rs`, `governance_vectors_tests.rs`, dw-engine/dw-ffi comment changes), the
  capability file, `tauri.conf.json`, `events.ts`/`queries.ts`, `governance.test.ts`, the fixture changes, and the
  mg-engine / MG-02 D92–D97 / E0-13 coverage doc changes. To answer Q1, Q2, Q5 and Q6 I also traced the facade
  (`dw-engine/src/governance/{mod,reads,voting,sync}.rs`), validation (`dw-governance/src/{validate,proposal,object,store}.rs`),
  `masternode_keys.rs` and the console's `getcurrentvotes`/`voteraw`.
- `pnpm install --frozen-lockfile`, then `pnpm exec vitest run src/api src/fixture`: 23 files, 375/375 pass.
  `pnpm exec tsc --noEmit`: clean.
- Contract-test mutations (N2) on a reflinked copy at `/work/scratch/dw-bl232-r1-opus/mut`:
  `vitest run src/api/governance.test.ts` for each mutation, results in the N2 table.
- `CARGO_TARGET_DIR=/work/scratch/dw-bl232-r1-opus/target cargo test -j 8 -p dw-app`: lib 172/172, `link_redaction` 1/1, doctest 1 ignored; exit 0. These include
  `every_command_is_granted_in_its_domain_file`, `facades_are_covered`, `codes_match_the_engine`,
  `codes_carry_their_fields`, `governance_vectors_are_current` and `every_command_has_a_vector`. I didn't rerun
  fmt, clippy or the full workspace; the owner's gate covers those.
- Playwright: not run. The range adds no screen. The fixture backend has no governance handlers, so no e2e path
  reaches the new commands.

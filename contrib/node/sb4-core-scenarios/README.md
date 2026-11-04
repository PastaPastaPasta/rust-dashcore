# sb4-core-scenarios

Dash Core's verdicts for SB4 (`dash-chain` core): the oracle harness of
`docs/node-program/designs/SB4-dash-chain.md` §15 (D22, DEC-123 Q3, DEC-127,
DEC-128, DEC-136). Every acceptance vector SB4's tests compare against comes from
here.

## Build

```sh
JOBS=8 nice -n 10 contrib/node/sb4-core-scenarios/build.sh /work/scratch/sb4-core-scenarios/out
contrib/node/sb4-core-scenarios/check-seam.sh /work/scratch/sb4-core-scenarios/out
```

`build.sh` `git archive`s the v24.0 oracle pin (`dash-oracle-sys/pins.txt`; Dash
Core v24.0.0-rc.5 plus the oracle shim, which the harness does not use), builds
depends and Core **with tests enabled**, and links `harness.cpp` against the
**unlocalized** server libraries plus `libtest_util.a` (`RegTestingSetup`-style
fixtures), with dashd's own link line. About an hour on a loaded agentbox;
`SB4_REUSE=1` relinks against an earlier tree (configured with the same flags).
`MANIFEST` records the commit, the configure flags, the wraps and interposed
symbols, the harness source and the SHA-256 of every static archive the link
read and of each object `build.sh` derives for an interposition (its path
relative to the out-dir).

### The seams

`wrap-symbols.txt` lists the `-Wl,--wrap=` symbols (mangled name, referring
object, caller, demangled name); `build.sh` checks each demangles as recorded.

- `chainlock::VerifyChainLock(Consensus::Params const&, llmq::CQuorumManager
  const&, chainlock::ChainLockSig const&, CBlockIndex const*)`
  (`chainlock/clsig.h:69-70`): `ProcessNewChainLock` verifies the lock's BLS
  recovered signature against an LLMQ, and a test chain has none. The wrapper
  returns `Valid` without calling the real function (`Invalid` for a `cl` op
  with `sig: "bad"`).
- `Chainstate::EnforceBlock(BlockValidationState&, CBlockIndex const*)` (SB4
  rev3): the wrapper marks the pass in the trace, calls the real function, then
  records every entry's status, the tip and `m_best_header`
  (`enforce_block_boundaries` in the view) before `EnforceBestChainLock` goes on
  to `ActivateBestChain`, and delivers the handler's waiting signals (a delivery
  point, below).

`interpose-symbols.txt` lists two more (HX1 X0, `designs/CH1-node.md` §11). Their
callers share the defining object (`validation.cpp`), and `--wrap` rewrites only
undefined references, so it cannot reach them. Every call is a `R_X86_64_PLT32`
relocation against the global symbol, though, so `build.sh` copies the defining
member out of `libbitcoin_node.a` into `<out-dir>/derived/` (not the shared build
tree, which every build would overwrite), makes the symbol weak there and adds
`__real_<sym>` at its address (`objcopy --weaken-symbol --add-symbol`), links the
copy ahead of the archives (the archive's member is then never pulled) and points
the symbol at the harness's `__wrap_<sym>` (`-Wl,--defsym`):

- `Chainstate::ConnectBlock(CBlock const&, BlockValidationState&, CBlockIndex*,
  CCoinsViewCache&, bool)`, called by `ConnectTip`, `TestBlockValidity`
  (`fJustCheck`) and `CVerifyDB::VerifyDB`;
- `Chainstate::DisconnectBlock(CBlock const&, CBlockIndex const*,
  CCoinsViewCache&)`, called by `DisconnectTip`, `CVerifyDB::VerifyDB` and
  `ReplayBlocks`.

The wrappers call the real function and return its result unchanged.

No other Core code changes. `check-seam.sh` proves it: the binary, its
`--commit` (the v24.0 pin) and every linked archive match `MANIFEST`'s SHA-256;
the wraps in `wrap-symbols.txt`, `MANIFEST` and the binary's `__wrap_` symbols
are the same set; and for each symbol the handler object is the only referrer in
every linked archive, `ld -r --wrap` leaves its `.text` sections byte-identical
and every relocation unchanged except those against the symbol (now
`__wrap_…`), the named caller in the binary calls `__wrap_…`, and nothing outside
the wrapper calls the real definition. For each interposed symbol: no linked
archive references it; the copy equals the archive member in every section's
header and bytes but the symbol and string tables, relocation for relocation,
and its symbol table differs only in the interposed symbols (GLOBAL → WEAK) and
one added `__real_` each at the same address; the member holds exactly one
relocation against the symbol in each listed caller, defines no clone of it
(only its `.cold` part), and holds no other relocation, outside the symbol, its
`.cold` part and the debug and unwind tables, whose target (S+A, or S+A+4…8 for a
RIP-relative one) falls inside either, whatever symbol it names; in the binary the symbol's address is `__wrap_…`'s, each
listed caller calls it, and only the wrapper calls `__real_…`.

### What else is fixture

The seams are the only change to Core's code, not the only choice the harness
makes. Each vector is Core's verdict under these, and SB4 must not read more into
it:

- **Setup.** libtest_util's `ChainTestingSetup` plus `TestingSetup`'s own steps
  (`ScenarioSetup` in `harness.cpp`), with disk-backed coins and block tree and
  in-memory Dash databases; the ChainstateManager gets dashd's
  `-checkblockindex` default (regtest only) rather than the fixture's `true`,
  which a devnet's genesis load trips. A scenario's `check_block_index: false`
  holds across its reloads. Chainstate options other than `checkblockindex` are
  the defaults; `args` change chain parameters.
- **Time.** Mock time: regtest and devnet start 10,000,000 s after genesis, and
  a block built on `G` is dated an hour before the clock (any other one second
  after its parent unless `time` says otherwise); preloaded networks start an
  hour after their latest header. The handler's 5 s timer is not started; `timer`
  runs its body.
- **Signal delivery** (DEC-147, below): an explicit schedule, named in each
  vector's provenance.
- **Spork signer.** Regtest uses its published spork key. Other networks get a
  key derived from a fixed seed, installed as `-sporkaddr`/`-sporkkey` would be;
  only the signer of SPORK_19 differs.
- **Receipt.** A lock's BLS signature is never checked (the first seam decides
  it: valid unless the op says `sig: "bad"`);
  `submitchainlock` is replayed from its height checks; `seed-cl`,
  `mark-conflicting`, `notify` and `checkpoint` are stimuli, not receipts.
- **Restart.** `reload` keeps the spork manager and the other fixture managers;
  it is an index and ChainLock restart, not a whole-node one.

## Scenarios are data

`scenarios/*.json` hold one case each: `name`, `rows` (the §15.1 ids it
oracles), `network` (`regtest`, `devnet`, `test` or `main`), `args` (Core
arguments; regtest moves DIP3/V20/MN_RR to 2,000 so coinbase-only blocks connect
up to height 1,000), an optional `preload` (`{file, count}`: the first `count`
headers of a CS2 dump, given with `--headers <dir>`), and `ops`. Blocks are
labelled `<branch><height>` (`A100`, `B110`); genesis is `G`, a devnet's block 1
`DG`, preloaded headers `R<height>`. A case is added by writing a file and running
`regen.py`.

| op | Core call |
|---|---|
| `build {branch, parent, count, submit}` | mines coinbase-only blocks; `submit`: `connect` (ProcessNewBlock each), `headers` (one ProcessNewBlockHeaders batch) or `none`. Overrides: `time` (`parent±N`, `mtp±N`, `now±N`), `bits` (hex, `powlimit`, or `next`: Core's `GetNextWorkRequired` on the indexed parent; default `powlimit` on regtest, `next` elsewhere), `version`, `pow: "fail"`, `invalid` (`cb-amount`: a coinbase paying more than any subsidy, which ConnectBlock rejects; `cb-height`: BIP34's coinbase height off by one, which ContextualCheckBlock rejects below DIP3). The result lists every block's 80-byte header (`headers`, label → hex), `hashes` (only the ends of runs over 64) and `bits` when given. Blocks are coinbase-only, so a header replay (§15's `ScriptedV`, whose `connect_tip` takes a header id) needs nothing else |
| `pnbh {headers}` | ProcessNewBlockHeaders on built headers (`A81..A90` ranges) |
| `connect {blocks}` | ProcessNewBlock (`force_processing` true) |
| `pnb {block, force}` | one ProcessNewBlock with the given `force_processing` (false: AcceptBlock's unrequested-block rule); result `ok`, `new_block` |
| `propose {block}` | `getblocktemplate` in proposal mode (BIP23, `rpc/mining.cpp:697-724`), Core's caller of TestBlockValidity (`fJustCheck`); result its BIP22 verdict (null if valid) |
| `cl {block \| unknown+height, height?, via?, sig?}` | a lock received as dashd receives one. `via: "p2p"` (default): dropped while SPORK_19 is off (`net_processing.cpp:5794`; result `dropped`). `via: "submitchainlock"`: the RPC's path, which has no spork gate, replayed from its height checks on (`rpc/quorums.cpp:1366-1373`: a height ≤ 0 is `invalid block height`, one ≤ the best lock's is answered with the best height; its signature decoding and 3-argument check are not run). `sig: "bad"` makes the VerifyChainLock seam answer Invalid for this lock (default `valid`; the result then says `verify_failed`). Then `ChainlockHandler::ProcessNewChainLock` in Core's order (DEC-128): initial CheckActiveState, seen/height pre-check, LookupBlockIndex + UpdateBestChainlock, then the scheduled CheckActiveState + EnforceBestChainLock |
| `seed-cl` | `Chainlocks::UpdateBestChainlock` directly: a **seeding** operation for pre-states, never the oracle for receiving a lock |
| `spork19 {value: on\|off}` | a SPORK_19 message signed with the spork key (regtest's, or the harness key elsewhere; `UpdateSpork`), one mock second after the previous one |
| `timer` | the body of `ChainlockHandler::Start`'s 5 s task (CheckActiveState, EnforceBestChainLock, Cleanup) on the handler's scheduler |
| `check-active-state`, `enforce` | the two handler methods directly |
| `notify {new, fork, ibd}` | `ChainlockHandler::UpdatedBlockTip` with chosen arguments; reports whether it scheduled the check |
| `invalidate`, `reconsider {ignore_chainlocks?}`, `precious` | the `invalidateblock` / `reconsiderblock` / `preciousblock` RPCs |
| `mark-conflicting` | `Chainstate::MarkConflictingBlock` (seed), then ActivateBestChain |
| `checkpoint {block}` | patches a regtest checkpoint into `CChainParams` |
| `reload {checkpoints?}` | node restart: flush, drop chainstate and ChainLock state, `LoadChainstate` from the same datadir (sporks survive, as `sporks.dat` does) |
| `locator`, `diskindex` | `GetLocator`; `CDiskBlockIndex` bytes |
| `mocktime {advance}` | moves the mock clock |

### Replaying the ops through V (HX1 X5)

CH1-node replays these scenarios through its validation actor V
(`designs/CH1-node.md` §2.2, §9.2). Per op: what the harness has Core do, and the
V command or test adapter that reproduces it. *Fixture* ops have no Core caller
and go through narrow test adapters in `dashd-chainstate`'s test-support, which
send an ordinary command to a running V or seed SB4 through its test
constructors before V starts; none constructs a `VContext`, `OnV` or
`AttemptHandle` (EV addendum §2.6).

| op | Core (harness) | V |
|---|---|---|
| `build` `submit: connect` | `ProcessNewBlock(force_processing=true)` per block | one `ProcessBlock { force: true, reply: Activated }` per block, in order |
| `build` `submit: headers`, `pnbh` | one `ProcessNewBlockHeaders` batch | one `ProcessHeaders` with the same batch |
| `build` `submit: none` | nothing; the blocks exist only in the harness | nothing |
| `build` overrides (`time`, `bits`, `version`, `pow`, `invalid`) | the bytes Core is given | the same bytes, from the vector's `headers` and `blocks_submitted`; a `ScriptedV` header replay needs only the headers (blocks are coinbase-only) |
| `connect` | `ProcessNewBlock(force_processing=true)` per block | `ProcessBlock { force: true, reply: Activated }` per block |
| `pnb {force}` | one `ProcessNewBlock(force_processing)` | `ProcessBlock { force, reply: Activated }`; result `ok`/`new_block` against `BlockResult { accepted, new_block }` |
| `propose` | `getblocktemplate` proposal: the RPC's duplicate and not-on-tip answers, then `TestBlockValidity(check_pow=false, check_merkle=true)` | RPC adapter: the duplicate/`inconclusive-not-best-prevblk` answers from a `Query`, then `TestBlockValidity { check_pow: false, check_merkle: true }` (CH3a/CH7a implement it) |
| `invalidate` | `invalidateblock` (InvalidateBlock, then the RPC's ActivateBestChain) | `InvalidateBlock` (which includes the RPC's ABC, `rpc:2015-2018`) |
| `reconsider {ignore_chainlocks?}` | `reconsiderblock` | `ReconsiderBlock { ignore_chainlocks }` |
| `precious` | `preciousblock` | `PreciousBlock` |
| `cl` (`via: p2p`) | dropped while SPORK_19 is off; else `ProcessNewChainLock(from=-1)`: CheckActiveState, seen and height prechecks, verification (the seam: valid, or invalid with `sig: "bad"`), UpdateBestChainlock, the scheduled CheckActiveState + EnforceBestChainLock | LK1a's sequence (§5.1): the spork gate in the P2P adapter, `ClIntake`, the seen/height prechecks, verification through a test verifier returning the same status, `RecordChainLock { from: None }`; its queued `ClEnforce { Recorded }` runs after the reply |
| `cl` (`via: submitchainlock`) | the RPC's height checks, then as above without the spork gate | the RPC adapter's height checks, then the same sequence |
| `seed-cl` | *fixture*: `Chainlocks::UpdateBestChainlock` directly | *fixture*: SB4's ChainLock state seeded through its test constructor before V starts, or the record step alone (`RecordChainLock` without intake) |
| `spork19` | a signed SPORK_19 message (`UpdateSpork`) | the spork manager's state the adapter's `ClSigs` reads; Core's reaction comes at the next `CheckActiveState` (`ClIntake` or `ClEnforce`) |
| `timer` | the 5 s task: CheckActiveState, EnforceBestChainLock, Cleanup | `ClEnforce { Timer }`; Cleanup is LK1a's (D6) |
| `check-active-state` | the handler's CheckActiveState | `ClIntake`, whose first step is `cl_active_state` (its reply ignored) |
| `enforce` | the handler's EnforceBestChainLock alone | *test adapter*: `ClEnforce`'s body without its leading `cl_active_state`; the same as `ClEnforce { Timer }` unless SPORK_19 changed since the last CheckActiveState |
| `notify {new, fork, ibd}` | *fixture*: the handler's UpdatedBlockTip with chosen arguments | *test adapter*: `cl_on_updated_block_tip(new, fork, ibd)` on V, which may queue `ClEnforce { TipScheduled }` |
| `mark-conflicting` | *fixture*: `Chainstate::MarkConflictingBlock`, then ActivateBestChain | *fixture*: the mark seeded through SB4's test constructor before V starts, then `ActivateBestChain` |
| `checkpoint` | *fixture*: a regtest checkpoint patched into `CChainParams` | *fixture*: V built with chain parameters holding that checkpoint |
| `reload {checkpoints?}` | flush, drop chainstate and ChainLock state, `LoadChainstate` from the same datadir (sporks kept) | `Flush { mode: Always }`, a clean shutdown and recovery from the same database (§7.3); a restart test, not power loss |
| `locator`, `diskindex` | `GetLocator`; `CDiskBlockIndex` bytes | `Query` (`ChainQuery`'s locator); CH2's block-index record encoding |
| `mocktime {advance}` | the mock clock | the test clock V reads |

Every op's verdict in a vector (tip, `best_header`, statuses, ChainLock state,
`dump`, `transcript`) is what the replay compares; the `transcript` is what
`RecordedConnector` replays (§8.1): one entry per `ConnectBlock`/`DisconnectBlock`
call, which V's connector consumes in order. The 17 `ch1-*` scenarios of §9.2 use
only these ops (`pnb {force: false}` for `ch1-acceptblock-unrequested`,
`reconsider {ignore_chainlocks}` for `ch1-reconsider-ignore-cl`, `cl {sig: "bad"}`
for `ch1-cl-intake-reset`, `cb-height` for `ch1-accept-contextual-reject`), plus
scenario `args` such as `-minimumchainwork`.

The handler listens to validation signals as in dashd (`init.cpp:2153`), through
the harness's listener, and its 5 s timer is not started. Its scheduler thread is
held while an op runs, so whatever the op makes it schedule runs after the op,
when the harness drains.

**Delivery (DEC-147).** In dashd the validation thread delivers queued signals to
the handler whenever it gets to them, and that timing decides how often
enforcement runs: the handler's `UpdatedBlockTip` schedules a check-and-enforce
task unless one is pending (`tryLockChainTipScheduled`), and each enforcement
emits tip signals of its own (`MarkConflictingBlock`, `ActivateBestChain`). One
delivered after the running task clears its flag schedules another pass, so with
the lock on a header-only branch dashd repeats enforcement an unbounded, timing-
dependent number of times (2 to 81 passes in ten runs). The harness delivers
Core's real callbacks under a recorded schedule instead: the queued callbacks the
handler listens to (`UpdatedBlockTip`, `BlockConnected`,
`TransactionAddedToMempool`) wait in an inbox, in queue order, and reach the
handler's own methods (whose compare-exchange decides, at delivery) at two
points:

- right after each `EnforceBlock`, once the validation queue is synced;
- when the drain's pump task runs on the handler's scheduler thread, after every
  task, with no handler task running.

The policy `boundary-flush` delivers everything waiting at each point; every
committed vector uses it, and provenance says so (`delivery_policy`).
`--delivery random:<seed>` delivers a pseudo-random prefix instead (at least one
callback when the scheduler is otherwise idle; a full flush once a drain has seen
12 passes, so it ends). Each schedule matches one dashd could run in effect, with
one difference: `ActivateBestChain` syncs the validation queue whenever more than
10 callbacks wait (`LimitValidationInterfaceQueue`), so in a long dashd reorg the
handler gets some signals mid-reorg, while here they wait for the next delivery
point. Those would meet a set or pending flag either way.

A vector is therefore an oracle of the trace up to each lock's first
EnforceBlock boundary and of the state at the enforcement fixed point (DEC-167,
which replaces DEC-147's idempotent repeats). Each enforcement pass is bracketed
in the trace: `EnforceBlock(X) #k` where it starts, `EnforceBlock(X) #k boundary`
where the real EnforceBlock returned, and `EnforceBlock(X) #k end` where it
ended. Each boundary record carries its `pass`, `repeat` (pass > 1) and
`pass_end`: the tip and best header where the pass ended, and whether it moved the
state (`changed_state`: tip, best header, failed blocks, ChainLock state or any
`nStatus`). Passes are counted per lock from one drain to the next (a drain runs
what one stimulus left: an op, or each block of a `connect`), and a main-thread
`enforce` op's own pass opens the count of the drain that follows.

The exact part, which SB4 matches, is the trace outside the passes and each
lock's first pass up to (not including) its `#1 boundary` marker, with that
boundary's record: SB4 ports
Core's single EnforceBlock pass exactly. The rest of pass #1 (the
`ActivateBestChain` after it), every later pass, the repeat boundary records and
every `pass_end` are transients: recorded, not asserted. The view's
`trace_transient_spans` lists their `[begin, end]` trace indices. `changed_state` compares only what that list
names: a change elsewhere in the chainstate (the candidate set, say) without one
there reads as no change.

A pass need not leave a fixed point. Pass 1 resets failure flags only when the
locked block itself is not `IsValid()` (validation.cpp:4075); in
`hx1-enforce-fixed-point` its parent is invalidated off the active chain and the
block is not yet flagged, so nothing is reset; the `ActivateBestChain` after the
pass flags the block FAILED_CHILD, and only pass 2 resets the branch and connects
it. dashd always gets that pass (the pass's tip signals schedule one, and
`Start()`'s 5 s timer runs EnforceBestChainLock regardless), so it re-runs
enforcement until a pass changes nothing. The harness does the same: once an op's
passes have drained, while the last one moved the state it schedules
EnforceBestChainLock on the handler's scheduler and drains again (Settle). The
scenario fails after 20 settle passes without a fixed point, or when a round
makes no pass at all (the handler off or the lock without a block), since the
state the last pass left is then no fixed point. These passes run without CheckActiveState,
which would observe the spork where no op asked for it (A-29(ix)); the `timer` op
runs the timer's whole body, and Settle follows it like any other op (so does
`enforce`). Settle passes continue the op's pass count and are never numbered
below 2, so none is a lock's `#1`. The step's view is taken after them, at the fixed point; for SB4,
`ScriptedV` and C4's tests compare there, not after a single pass.

The harness has no delivery point between the tip task's `ActivateBestChain` and
its clearing of `tryLockChainTipScheduled`, so the signals of a reorg made there
always schedule one more pass, which dashd may leave to its timer; both reach the
same fixed point. A lock whose block is already on the active chain gets one pass
and emits nothing that schedules another; a lock not yet in the index gets none.
`regen.py --check` runs every scenario that turns SPORK_19 on under 8 random
schedules and fails unless each one's results, fixed-point state, first
boundaries and exact trace equal `boundary-flush`'s. So far all do; a schedule
that reached another fixed point would be recorded as a set of allowed states.

Each step prints the op, its result (for header batches: result class, reject
reason, debug message, last accepted, and the block-tree records the flush wrote,
re-serialized by Core), and a view: tip, `m_best_header`,
`GetLocator(m_best_header)[0]` (both `{"address_tie": [...]}` when the
`m_best_header` pick ties others on work and `nSequenceId`, where Core compares
addresses, D9), `m_failed_blocks`, the ChainLock state (best,
best with a known block, the floor `bestChainLockBlockIndex`, the observed
`isEnabled`, the live spork), every status that changed (`nStatus`), the
`enforce_block_boundaries` recorded by the EnforceBlock seam, the
`CValidationInterface` signal trace with `trace_transient_spans`, and
`getchaintips` with §10.2's projection (equal heights sorted by hash, D9). Each
step also lists `blocks_submitted`: the full serialized bytes (hex) of every block
the op handed to Core (`connect`, `pnb`, `propose`, `build` with `submit:
connect`). Every step carries all of it: the exact trace, every status and entry
change and every record written, never a count in their place.

**Dumps (HX1 X3).** Each view also carries `dump`, the selection state after the
op: `entries` (each index entry's `[nSequenceId, nChainTx]` that changed since the
previous dump; the unprojected `nStatus` is `status_changes`), `candidates`
(`setBlockIndexCandidates` in `CBlockIndexWorkComparator`'s order, least work
first), `unlinked` (`m_blocks_unlinked`'s `[parent, child]` pairs) and `cl` (`bestChainLock`, `bestChainLockHash`, the CLSIG's own hash, and
`bestChainLockWithKnownBlock`, as stored). Two of these orders are heap addresses
(D1, D9) and are flagged rather than hidden: `candidate_address_ties` lists
adjacent candidates equal in work and `nSequenceId` (only loaded entries, all id
0, can be), and
`unlinked_parents_address_ordered` marks a multimap with more than one parent:
Core keeps those parents in pointer order, which differs between runs, so the dump
lists them by label (each parent's children stay in multimap order, which for one
key is insertion order).
`regen.py --check` reruns every case, so two runs giving identical dumps is part
of the check, under every delivery schedule too.

**Address sensitivity (DEC-172, DEC-175).** Where Core decides by heap address
(D9: AcceptBlockHeader's walk takes the first failed ancestor in
`m_failed_blocks`' pointer order; `CBlockIndexWorkComparator` compares the
addresses of entries tied on work and `nSequenceId`, which loaded entries, all
id 0, can be, and so can live ones, since `preciousblock` resets its counter and
hands out -1 again), a vector would record one run's heap layout. The harness
does not predict when that happens: predicates missed three times (Sol HX1 R1-3,
R2-1, R3-1). Every run carries `possibly_address_sensitive`, a diagnostic label
only (`failed_blocks`: two or more entries in `m_failed_blocks` around a step or
before a submission; `loaded_tie`: two loaded entries equal on work), which
decides and gates nothing. `--perturb-heap <mode>` (first argument) makes the
harness's replacement `operator new` serve every `BlockMap` node, the object
holding each block-index entry, and nothing else, from a private region of 2^24
256-byte slots it never reuses. A node is recognised by its size and by its
caller: the return address lies in a function that allocates `BlockMap` nodes at
this pin (`AddToBlockIndex`, `InsertBlockIndex`, or an out-of-line instantiation
for `BlockMap`'s value type, such as the `operator[]` `ReplayBlocks` uses). Each
node takes the next ordinal, counted on the main thread, where the ops create
every entry, and its slot is a function of the mode and that ordinal alone:
ordinal k goes to slot k under `up` and to the k-th slot from the top under
`down`, so any two entries swap places between the two. Under a numeric seed it
goes to the slot a seeded two-level permutation gives k (blocks of 4,096 slots,
then slots within a block), so a middle one of three tied entries may lie lowest.
Every other allocation is plain `malloc`, so one mode gives one vector in every
process. Sol HX1 r4 R4-3 showed the earlier scheme did not: it drew each small
main-thread allocation's lane from one shared stream. A run whose region runs out
fails, since later entries would not be perturbed. Without the flag `operator
new` is plain `malloc`.

Every run `regen.py` writes or accepts goes through one admission rule (R4-1):
- **Which runs:** a generated sequence, a committed scenario or header job
  (written as well as checked), and each `--check` delivery schedule.
- **The replays:** the plain run, then replays under `up`, `down` and at least
  one seeded heap. `--perturb-replays` defaults to 1, and 0 is refused (R4-2).
- **Seeds:** each is derived from the run's own identity: a sequence's seed,
  index and length; a scenario's name and delivery policy; a header job's name.
- **The verdict:** a difference between any two runs, the plain one included,
  in outcome, state or trace is reported at its earliest step over every
  channel (step results and views, transcript and acceptance verdicts).
  - A generated sequence is cut before that step (`cut`).
  - A committed scenario, header job or schedule that differs fails, and is not
    written in one heap's layout.
  - A replay that fails where the plain run passed fails, as do runs that
    differ at setup.
  - A bench's instruction count moves with the heap, so admission ignores it;
    the committed count is checked within `BENCH_TOLERANCE` instead.

Because `up` and `down` reproduce and disagree wherever two entries' order
decides, rerunning a seed reproduces the cut. Random replays alone each found a
sensitive sequence about half the time, and a 1,000-sequence smoke's rerun
differed in two sequences. Among three or more tied entries (a reload's
`m_best_header` pick, say) `up` and `down` pick the extremes. The seeded replay
can pick a middle one, and since its seed derives from the sequence's, a cut it
causes reproduces from the sequence alone.

`--check` also runs two kinds of regression:
- **`address-sensitive/`:** each sequence must be refused by admission first at
  exactly its step in regen.py's `SENSITIVE_CUTS`.
  - Sol's R2-1 batch: a parent built earlier in the same header batch, under
    two failed blocks; step 5.
  - Sol's R3-1 tie: two live blocks with `nSequenceId` -1 after a precious
    reset; step 7.
- **`reproducible/`:** under each seed in `REPRODUCIBLE`, four independent
  processes must give one vector, and it must first differ from `up`'s and
  `down`'s at the recorded step. This holds Sol's R4-3 three-way case: a reload
  ties three tips, and `mark-conflicting` on a middle pick moves
  `m_best_header`.

**Re-pin: rerun the allocation audit.** `up` and `down` reach the right pointers
only because, at the v24.0 pin, the entries Core orders by address are
`CBlockIndex` objects (144 bytes) inside `BlockMap` nodes (192 bytes, alignment
8). The main thread allocates those nodes through `operator new`, from the
functions above. A re-pin (or a harness change to how entries are created) must
rerun Sol's allocation audit: `allocation-audit.py --bin <out>`.
- It runs the address-sensitive and reproducible scenarios under
  `--audit-index`.
- The harness itself fails a dump unless every entry lies in the region and the
  region holds nothing else: its live slots must equal the entries.
- The script also requires that `up` and `down` order every pair oppositely, and
  that each seed gives the same region offsets in two processes.
- `AuditIndex` and the perturbation name libstdc++'s `_Hash_node`, so a
  toolchain change may need that type updated. A pin that allocates nodes
  elsewhere fails the audit until `IsNodeAllocator` names the new site.

The audit checks the mechanism, not the scope: also grep the new pin for
pointer-keyed ordered containers (`std::set<T*>`, `std::map<T*, …>`,
`std::multimap<T*, …>`) and comparators that fall back to addresses, and add a
scenario under `address-sensitive/` for any new one.

**Calls (HX1 X1).** After setup the vector records, in `transcript`, every call
through the ConnectBlock/DisconnectBlock seam as `{step, phase, site, block,
hash, occurrence, result, state}`: `phase` is `connect`, `check` (ConnectBlock
with `fJustCheck`, from TestBlockValidity) or `disconnect`; `site` the calling
function (ConnectTip, TestBlockValidity, CVerifyDB::VerifyDB, DisconnectTip,
ReplayBlocks), found from the return address in the binary's symbol table;
`occurrence` counts calls per (phase, hash) from 1, so the same bytes connecting
after an earlier failure show 2; `result` is ConnectBlock's bool or
DisconnectBlock's `ok`/`unclean`/`failed`; `state` the BlockValidationState.
`accept_checks` holds the other `BlockChecked` verdicts (ProcessNewBlock's
CheckBlock and AcceptBlock's checks). `core_counts` counts the same calls without
the seams, from Core's own `-debug` log lines: `connect_tip` ("Load block from
disk", logged by ConnectTip just before its ConnectBlock), `disconnect_tip`
("Disconnect block", after DisconnectTip's), `verify_db_disconnect` and
`verify_db_connect` (VerifyDB's "Verifying last N blocks at level L" and "No coin
database inconsistencies in last B blocks": B disconnects at level 3 and up, B
reconnects at 4, none when it skipped level 3), and `replay_disconnect`
("Rolling back", before each of ReplayBlocks' DisconnectBlock calls). Two are
partly the harness's own: `test_block_validity` is the proposals the harness
sent less those TestBlockValidity rejected before ConnectBlock (which it logs;
it logs nothing on the others), and `tip_block_checked` counts ConnectTip's
BlockChecked, which follows every ConnectBlock there, as the harness saw it. The
gate requires every site's transcript length to equal its counter.

The trace lists `SynchronousUpdatedBlockTip(new, fork, ibd)`, the queued
`UpdatedBlockTip(new, fork, ibd)`, `BlockConnected(X)`, `BlockDisconnected(X)`
and the pass markers in emission order. Core always emits the synchronous tip
signal immediately before queueing its asynchronous twin, so the listener
rebuilds the order from that pairing (and fails if it breaks).

A-1a's network cases are scenarios too: `a1a-devnet-*` (the
`-minimumdifficultyblocks` window on either side of its edge at a retarget
height, and the devnet-genesis rule), `a1a-testnet-min-difficulty` (mined forks
off testnet's real headers at the 2×spacing minimum-difficulty boundary below
KGW, the last-other-bits rule, and the 2 h boundary in the DGW era) and
`a1a-mainnet-stale-branch-chainlock` (mined stale headers off mainnet's real
headers against a seeded lock: bad-chainlock, duplicate, bad-prevblk-chainlock,
and acceptance above the lock).

`headers` mode runs on the CS2 real-header dumps (H-M/H-T): a P2P-sized batch
after preloading (`pnbh`), `GetLocator` at pseudo-random heights (`locator`), and
the A-25 Core side (`bench-pnbh`: instructions:u of one ProcessNewBlockHeaders
call on 2,000 headers, PF0's counter method). Instruction counts depend on the
CPU's SHA-256 dispatch, so the committed bench numbers are agentbox's (AVX2, no
SHA-NI) and `--check` compares them within 0.5% only there.

## Vectors

```sh
contrib/node/sb4-core-scenarios/regen.py --bin /work/scratch/sb4-core-scenarios/out           # write
contrib/node/sb4-core-scenarios/regen.py --bin /work/scratch/sb4-core-scenarios/out --check   # byte-compare
```

**Random sequences (HX1 X4).** `regen.py --bin <out> --random N --seed S` judges
N generated operation sequences instead of the committed scenarios. Each is an
ordinary scenario (branches built, announced and delivered in random orders,
invalid blocks, the RPCs, locks with SPORK_19 flipping, timers, reloads) built
from `random.Random("S:i")`, so sequence i can be rerun alone; it is written with
its vector and provenance to `--random-out` (default
`/work/scratch/sb4-core-scenarios/random`), not to the committed vectors.
`--random-delivery` runs each under its own `random:<seed>` delivery schedule
(DEC-147) rather than `boundary-flush`; the transcript stays per step either way.
With `--check` it compares with the files already there: the same seed must give
the same sequences and outputs byte for byte. A sequence that differs under a
perturbed heap is cut before the step that differs and rerun (`cut`, see Address
sensitivity); a harness failure, such as a Core assertion, is a finding (`fail`, or `known`, see
Findings; stderr in `<name>.err`). `python3 -I node-fuzz/fuzzctl.py harness-smoke --bin <out>` is the
1,000-sequence smoke: it generates, reruns with `--check`, and prints one
`node-fuzz harness-smoke …` line, then the name of every known, failing or
differing sequence; it passes with no `fail` and no `differ`. A rerun without
`--check` first removes that seed's earlier outputs.

**Findings.** `findings/` (absent while empty) is for minimized sequences of Core
behaviours the random runs found that stop Core, such as an assertion, so they
cannot be vectors (`sb4_scenarios run findings/<name>.json --delivery
boundary-flush` reproduces one). Each names how the run fails (`kind`: `error`,
the harness's exit 1; `abort`, SIGABRT such as an assertion; `signal`) and
non-empty text of its terminal diagnostic, the last stderr line (`signature`). A
random sequence counts as `known`, not `fail`, only when it fails the same way
and its own terminal diagnostic contains the signature; earlier stderr lines
never decide, and a finding without a signature or kind stops the run. So the
smoke stays green on what is already recorded; a `known` count that grows is
worth a look.
- CANDIDATE-UNLINKED (private, PLAN §7): its repro and signature are not in this
  tree; with `SB4_PRIVATE_FINDINGS` naming the private directory that holds its
  `signatures.json`, sequences carrying it count as `known` and their stderr is
  kept there, not beside the outputs; without it they count as `fail`.

The first smoke's other finding, a repeated pass that moved the state, is not a
failure under DEC-167: its minimized sequence is the vector
`hx1-enforce-fixed-point`.

Output goes to `dash-chain/tests/data/core-scenarios/<case>.json.gz` as
`{provenance, vector}`, gzipped deterministically (level 9, mtime 0, no file
name), since complete views run to 26 MB in all, 9 MB gzipped (17 MB for
`a2-pnbh-rules-anchored` alone); provenance names the Core tag and commit, configure flags,
the binary, archive and harness hashes, the delivery policy, the command and the
SHA-256 of its inputs (scenario file, the preloaded prefix of a header dump, or
the dump). Before any job `regen.py` checks the build: the binary's and
`harness.cpp`'s hashes and the binary's `--commit` against `MANIFEST` and the
v24.0 pin, and every linked archive still in the build tree. `--check`
regenerates and requires byte equality of the JSON and, on agentbox's zlib, of its
gzip bytes (another zlib build may compress the same JSON differently; `--check`
then says so), except that a bench file's
`instructions_u` must be within 0.5% (the rest of it exactly); it reports
"N exact + 2 tolerance-checked", then runs the schedule check above. The gate test
`dash-oracle-sys/tests/sb4_core_scenarios.rs` always checks provenance, the
harness and input hashes, the delivery policy, row coverage, every build's
headers (one per block, X11-hashing to the listed hash and chaining), that each
transcript agrees with `core_counts` site by site, numbers occurrences per
(phase, hash) and only connects or checks blocks submitted earlier with bytes
hashing to them, that some vectors show HX1's cases (bad-cb-amount at ConnectTip;
the same bytes failing bad-chainlock and then connecting; a passing and a
failing check; a block without data that `pnb {force: false}` receives and Core
leaves without data, then `pnb {force: true}` stores (`BLOCK_HAVE_DATA`), `precious`,
`propose`, `cb-height`), that every step's view is complete (trace, status
changes, a full dump, and every header batch's records; any `*_count` field
fails), and for A-21b,
A-29(x), A-29(x') and A-1a's network rows that the vector shows the case (floor 30 below the
tip then enforcement; a CONFLICT mark on the floor's own path; the network and
its accept/reject verdicts). It regenerates the smoke set when
`SB4_CORE_SCENARIOS_BIN` names a build. Core's
archives embed their build path (`__FILE__`), so that byte comparison passes only
for a build under the same `SB4_BUILD` directory
(`/work/scratch/sb4-core-scenarios/core-build`).

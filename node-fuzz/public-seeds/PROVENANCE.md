# Public seeds

## `diff_p2p_<command>/` (O2c)

The overlay targets' seeds, written by `cargo run -p node-l1 --features oracle --example
overlay_seeds` (node-l1 `examples/overlay_seeds.rs`). Each file is one target input (the header
`node_l1::overlay` reads, then the payload), named by the first 8 bytes of its SHA-256; identical
inputs are one file. The inputs are:

- every case of BS8's Core-judged corpus, dash-oracle-sys `tests/data/bs8-core-overlay.json5`
  (base-sdk `pkgs/p2p_core/corpus/core_overlay.json5` at dbe245f, read by bs8-judge against the
  v24 pin; see that directory's PROVENANCE.md), on its recorded stream version, BLS scheme and
  qdata limits;
- the O4 mnlistdiff and qrinfo fixtures (`test-vectors/golden/*-core-ground-truth.json`, payloads
  served by Dash Core) of at most 64 KiB, on `PROTOCOL_VERSION` (70242) with the basic scheme.

node-l1's `l1_overlay` test checks every one of these against the in-process oracle (v24.0):
none diverges. Regenerate after a change to the corpus, the fixtures or the input layout; the
example overwrites files of the same name and leaves others, so delete the directories first.

## `diff_mnlist_*/`, `diff_unique_hash/` (EV2a), `diff_dml_apply/`, `diff_dml_block/`, `mnlist_payee_queue/` (EV2b)

The EV2a targets' seeds (`node_l1::dml`), written by `cargo run -p node-l1 --features oracle
--example dml_seeds` (node-l1 `examples/dml_seeds.rs`), named as above. The inputs are:

- Core's lists, diffs and canonical-reader mutation inputs in dash-dml `tests/data/codec/`
  (written by Dash Core v24 through `contrib/node/ev2a-codec-probe`; see that directory's
  README), each list also in the evodb encoding the v24 oracle writes back;
- edit scripts (`diff_mnlist_diff` mode 0) and operation histories (`diff_mnlist_order`),
  splitmix64 bytes from fixed seeds: inputs only, judged by the oracle when run;
- `node_l1::dml::unique_seeds`: one value of each unique-property type and class, each in
  Core's domain; `l1_dml` checks that each reaches its property (and operator-key scheme) and
  both accept it;
- `diff_dml_apply/` (EV2b): each of O2a-EV's crafted cases (`dash-oracle-sys/tests/data/
  dml-crafted-v24.0.json`) with one operation of each kind, and with a splitmix64 operation
  script; inputs only, judged by the oracle when run;
- `diff_dml_block/` (EV2b D54): checked sequences of each case, crafted and testnet's DIP3
  enforcement block, with one operation of each kind and with splitmix64 operation scripts,
  plus (D56) probed sequences (`FLAG_PROBE`), one per case, probe step, arm and operation kind;
  inputs only, judged by the oracle when run (the target also reads `diff_dml_apply/`);
- `mnlist_payee_queue/` (EV2b): EV2 §6.2's payee fixtures (`node_l1::dml_payee::fixtures`) and
  splitmix64 operation scripts in each of the target's six eras; node-l1's `l1_payee_queue`
  test runs the fixtures, and the scripts are judged by the oracle when run.

node-l1's `l1_dml` test runs every target on every one of these against the in-process oracle
(v24.0): none diverges. Regenerate as above.

## `diff_quorum_members/`, `diff_type_enabled/`, `diff_scan_quorums/`, `diff_select_quorum/` (LQ1a)

The LQ1a targets' seeds (`node_l1::llmq::seed_inputs`), written by `cargo run -p node-l1
--features oracle --example llmq_seeds` (node-l1 `examples/llmq_seeds.rs`), named as above. The
inputs are regtest scenarios (the activation tables, LLMQ_TEST of 3 and of 5, nine masternodes
always present or absent every third block), mined commitments at the DKG cycles, and calls in
the shapes of `l1_llmq`'s boundary rows or splitmix64 bytes from fixed seeds: inputs only, judged
by the oracle when run.

node-l1's `l1_llmq` test checks that each file is current and runs its target on it against the
in-process oracle (v24.0): none diverges. Regenerate as above, after deleting the directories.

## `diff_bls_decode/` (BS11a-1)

The BLS decode target's seeds (`node_l1::bls`), written by `cargo run -p node-l1 --features oracle
--example bls_seeds -- <bls_wrapper.json5>` (node-l1 `examples/bls_seeds.rs`), named as above,
from base-sdk's Core-judged BLS wrapper corpus `pkgs/pkc/corpus/bls_wrapper.json5` at the pinned
revision (8382b76; the file is unchanged since dbe245f, see its header for provenance). Each is
the first sample of one eager class (42 public-key, 57 signature and 11 secret-key classes) as
raw input on every leg: public keys and signatures eager and lazy, secret keys eager, each under
both flags: 410 files, as two pairs of classes share their first sample (public-key `tors_3` and
`x0_101`, signature `x0_legacy_s1` and `x0_basic_s0`). The example replaces the directory.

node-l1's `l1_bls` test checks every one of these against the in-process oracle (v24.0): none
diverges. Regenerate as above.

## `diff_bls_arith/` (BS11a-3)

The BLS arithmetic target's seeds (`node_l1::bls_arith`), written by `cargo run -p node-l1
--features oracle --example bls_arith_seeds -- <arith.json5> <relic.json5>` (node-l1
`examples/bls_arith_seeds.rs`), named as above, from base-sdk's Core-judged step scripts
`pkgs/pkc/corpus/bls_oracle/{arith,relic}.json5` at the pinned revision. Each row is one seed:
its objects loaded raw, its register operands picked as the target picks them, its `VERIFY`
steps left out (23 files). The example adds one torsion script per group, cofactor part and
generator bit under each flag (52 files: G2 has a seventh part, its 448-bit prime's): 75
files. It also writes `bls_engine_model`'s seeds
(`node-fuzz/seeds/bls_engine_model/`, 12 files). The example replaces both directories.

node-l1's `l1_bls_arith` test checks every one of these against the in-process oracle (v24.0):
none diverges. Regenerate as above.

## `diff_bls_aggregate/`, `diff_bls_threshold/`, `diff_bls_ies/` (BS11b)

The seeds of the BLS wrapper targets (`node_l1::bls_wrapper`, BS11b D25), written by `cargo run
-p node-l1 --features oracle --example bls_wrapper_seeds` (node-l1 `examples/bls_wrapper_seeds.rs`),
named as above. They are scripts built by the example, not read from a corpus, each run under
both flags, over Group keys and signatures, zero (invalid) objects, kept elements (a failed
in-place op's receiver) and legacy-decoded torsion points:
- `diff_bls_aggregate/` (24 files): the insecure aggregates (span and pair), `SubInsecure`, the
  secure aggregate and `VerifySecureAggregated` over 1, 2, 4 and 8 signers, duplicate keys,
  unequal and empty lists, and `GetHash()`'s cache;
- `diff_bls_threshold/` (20 files): `SecretKeyShare`, `PublicKeyShare` and `Recover` for
  thresholds 2 to 5 over each id class, the recovered signature verified, and each refusal
  (too few coefficients or shares, repeated ids, ids equal modulo r, a zero id, unequal lists,
  invalid operands), with and without an in-place receiver;
- `diff_bls_ies/` (16 files): `DHKeyExchange`, then multi-recipient encryption over each
  plaintext class and its refusals, and the decryption of each container and blob as made and
  mutated (truncated, a byte flipped, the other flag, a trailing byte), with the secret key
  each plaintext holds under each flag.

The example replaces these three directories (and `diff_bls_verify/`, below). node-l1's
`l1_bls_aggregate`, `l1_bls_threshold` and `l1_bls_ies` tests check every one of these three
targets' seeds against the in-process oracle (v24.0): none diverges. Regenerate as above. Since
BS11a-4 every verification in them is compared, a legacy one over a point with no usable element
included.

## `diff_bls_verify/` (BS11a-4)

The seeds of `diff_bls_verify` (`node_l1::bls_wrapper`, BS11a-4 D10), written by the same example
into their own directory, named as above, each script run under both flags (22 files): `VERIFY`,
`VERIFY_AGG` and `VERIFY_SEC` over 1, 2 and 4 signers, accepting and refusing; distinct and
repeated hashes; unequal and empty lists; the insecure and secure aggregates of one message
verified; and invalid objects, kept elements (a failed in-place op's receiver) and
legacy-decoded torsion points, alone and in aggregates. node-l1's `l1_bls_verify` test checks
every one against the in-process oracle (v24.0): none diverges.

## `diff_quorum_members_rot/` (LQ1b)

Written by the same example from `node_l1::llmq::seed_inputs` (its `rot_seed_inputs` part).
The inputs are rotated regtest scenarios to height 279: V20 never, at 2 and at the cycle of 48;
V19 at 2, 73 and 74 (either side of the cycle of 72's `height + 1` test); DIP0024 at 2 and 48;
LLMQ_TEST of 3 and of 5. Each holds twelve masternodes always listed, or sixteen whose lists
change (registrations, removals, phases, a ban, an unconfirmed one, reused services and keys,
and state changes between cycles). The calls are precomputes connecting every cycle base,
members calls out of cycle order around a restart, rows seeded in every mode before an undo, or
splitmix64 bytes from fixed seeds: inputs only, judged by the oracle when run. `l1_llmq` checks
them as above.

## `diff_coins_ops/` (O2a-CO, CH2c)

The coins-cache differential's seeds (`node_l1::coins_ops`, registered by CH2c), written by
`cargo run -p node-l1 --features oracle --example coins_ops_seeds` (node-l1
`examples/coins_ops_seeds.rs`). Each file is one hand-written input from node-l1
`tests/support/coins_ops_seeds.rs`, named as there, behind its mode byte (CH2c D3: 0 aligned, 1
hidden): `script_from_bytes` reads the rest into an op script over CH2's cache and overlay, which is
run against Core's coins views (`oracle_coins_ops`). The
scripts cover tip adds (with and without possible_overwrite, unspendable outputs), spends, reads,
Uncache, best blocks and flushes, and block views that commit or drop. They also reach both of the
harness's by-design differences (O2a-CO notes D13). CH2c's seeds add Core's map entries (ENTRY)
and, in hidden mode, F's freeze, W steps and releases between Core's flushes, ending at both
flush-point differences, a frozen FRESH coin spent twice (the re-spend rule), and a coin added
FRESH over the database's unspent one (D26: compared in aligned mode, ended at the freeze in
hidden mode, compared when spent before one, and ended where only CH2 refuses it because F holds
the coin). node-l1's `l1_coins_ops` test checks every one against the in-process
oracle (v24.0), with Core's DynamicMemoryUsage after every op: none diverges.

## `diff_chain/` (O2b-3a)

`diff_chain`'s seeds, written by `cargo run -p node-l1 --features oracle --example chain_seeds`
(node-l1 `examples/chain_seeds.rs`): each program in node-l1 `tests/chain_seeds/*.prog` encoded
to the bytes that decode to it, named as above. The programs are synthetic regtest chains
(blocks the generator builds from the image, OP_TRUE outputs); node-l1's `l1_chain` test runs
every one of them on S0 and S1. Commit the programs, regenerate the bytes: delete the directory
first, as above.
## `diff_coinjoin/` (O2a-CJ1)

The CoinJoin rules' seeds (`node_l1::coinjoin::seed_inputs`), written by `cargo run -p node-l1
--features oracle --example coinjoin_seeds` (node-l1 `examples/coinjoin_seeds.rs`), named by the
first 8 bytes of their SHA-256. The inputs are call bytes only, judged by the oracle when run:
every rule at every pick; promotion and demotion entries at every session denomination; sessions
(standard, promotion, demotion and final-transaction shapes, mempool inputs unlocked and locked,
missing, null and repeated-script inputs) and collaterals (accepted and refused by the mempool,
a lock time, too small and too large a fee, a fee at the 0.1 DASH bound and one duff over it, an
input coin above MAX_MONEY, a negative output, mempool inputs, a non-P2PKH output) on every network;
sessions at denomination 0, which the oracle refuses (one with an output of i64::MIN);
and a queue and three broadcasts signed by a fixed operator key (base-sdk's `BlsSecretKey`, Core's
`SignBasic`) in the hash, signature, time and structure cases, with balanced broadcasts at the
pool sizes (180, 181, 200 and 201 inputs and outputs) on every network, V24 defined and active. node-l1's `l1_coinjoin` test checks
that each file is current and runs the target on it against the in-process oracle (v24.0): none
diverges. The example replaces the directory.

## `diff_gov/` and `diff_gov_vote/` (GV1a)

The governance targets' seeds (`node_l1::gov::gov_seed_inputs`, `gov_vote_seed_inputs`), written
by `cargo run -p node-l1 --features oracle --example gov_seeds` (node-l1 `examples/gov_seeds.rs`),
named by the first 8 bytes of their SHA-256. The inputs are call bytes only, judged by the oracle
when run. `diff_gov`: every `ValidateProposal()` call of O2a-GOV's `proposals.txt` (Core's
`proposals_valid.json` / `proposals_invalid.json`, as `governance_validators_tests.cpp` drives
them) on mainnet, testnet, the fixture's regtest and a devnet named `gv1a`; node 0's five objects from O2a-GOV's
Core-produced regtest network (`dash-oracle-sys/tests/data/gov-regtest-v24.0.*`, provenance in
that directory) on the captured and the re-keyed list, with each collateral active, off the active
chain and absent, each field edited once, and Core's proposal data as object data.
`diff_gov_vote`: node 0's votes on both lists, each field edited and each signing form, clocks
around the one-hour bound, and op scripts holding the objects, replaying node 0's votes and adding
votes on a missing parent, blocks and a later time; on the re-keyed list, synced, four unparsable triggers then valid re-timed copies of the trigger (several at one height), signed by its masternode's operator key and once by another, and the proposals under each collateral mode (GV1a notes D29). The re-keyed list replaces each masternode's
voting key id and operator key with keys the harness derives from fixed bytes (`gov::voting_key`,
`gov::operator_key`) and signs the trigger and votes again (GV1a notes D17). node-l1's `l1_gov`
test checks that each file is current and runs both targets on them against the in-process oracle
(v24.0): none diverges. The example replaces the directories.

## `diff_block_value_payee/` (EV3-1)

The coinbase value and payee checks' seeds (`node_l1::payments::seed_inputs`), written by `cargo
run -p node-l1 --features oracle --example payments_seeds` (node-l1 `examples/payments_seeds.rs`),
named by the first 8 bytes of their SHA-256. The inputs are call bytes only, judged by the oracle
when run: every sample of O2a-GV's payments fixture (`dash-oracle-sys/tests/data/
payments-v24.0.json.gz`) under every flag set, with a ChainLock at and below its height; and each
edit (coinbase outputs, the payee's operator reward, payout list and shares, a ban, the trigger's
payments) at four argument pairs, synced with governance loaded, with and without V24 and the
trigger. node-l1's `l1_payments` test checks that each file is current and runs the target on it
against the in-process oracle (v24.0): none diverges. The example replaces the directory.


## `diff_credit_pool/` (EV3-2)

The credit pool step's seeds (`node_l1::credit_pool::seed_inputs`), written by `cargo run -p
node-l1 --features oracle --example credit_pool_seeds` (node-l1 `examples/credit_pool_seeds.rs`),
named by the first 8 bytes of their SHA-256. The inputs are call bytes only, judged by the oracle
when run: each credit step of O2a-GV's payments fixture (`dash-oracle-sys/tests/data/
payments-v24.0.json.gz`) in each flag state, and every edit (the previous pool, lock and unlock
outputs, payload fees, indexes and version, the CbTx balance, the block's transactions) at a few
arguments, whole-coin deltas among them; two used top indexes merging into one wrapped range
(CONS-10); and a version-2 unlock whose index is used. node-l1's `l1_credit_pool` test runs the
target on every seed against the in-process oracle (v24.0): none diverges, and each outcome
occurs. The example replaces the directory.

## `diff_mnhf_step/` (EV3-3)

The MNHF manager step's seeds (`node_l1::mnhf::seed_inputs`), written by `cargo run -p node-l1
--features oracle --example mnhf_seeds` (node-l1 `examples/mnhf_seeds.rs`), named by the first 8
bytes of their SHA-256. The inputs are call bytes only, judged by the oracle when run: every op (a
signal, a trailing byte or another transaction or payload version, another transaction type, a stage
row, a seeded tip entry below V20, each deployment window's start, timeout and `useEHF`) at a few
arguments on each V20 placement, with and without `fJustCheck`, each transaction and payload version
alone and beside a signal, a signal bit at `VERSIONBITS_NUM_BITS` (outside the domain) and the same
bit in transactions that carry no signal (compared), and the shapes of the crafted cases in
node-l1's `l1_mnhf` test (a held bit, a re-signal around its window start, the V20 boundary's cache
entry, a seeded bit no deployment signals). `l1_mnhf` runs the target on every seed against the
in-process oracle (v24.0): none diverges, and each outcome occurs. The example replaces the
directory.

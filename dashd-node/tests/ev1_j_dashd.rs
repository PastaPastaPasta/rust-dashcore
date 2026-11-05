//! EV1-J J0a, J0b-1, J0b-2 and J2's regtest rows (the EV1-J plan rev4 §3, §5.1-§5.4, and its J2
//! delta, OD-CP-73; EV1 design §6.3 and D27, in `docs/node-program/designs/EV1-specialtx.md`):
//! the block-path rows of `contrib/node/ev1-j-rows.toml`, each judged by dashd v24.0.0-rc.5's
//! `submitblock` and by V on the same bytes. J0a's rows need no quorum with known keys; J0b-1's
//! run on the regtest quorum fixture of `ev1_j/` (constructed commitments the producer mines);
//! J0b-2's BAV paths run on the same fixture below assumevalid (`ev1_j/bav.rs`), each with a mode
//! control that both nodes accept only with their scripts skipped and its positive twin, which
//! both reject with the script error.
//!
//! J2, regtest only (`ev1_j/l2.rs`): the L2-C rows put types 7 and 9 next to a CbTx ChainLock on
//! the same fixture, one setup block past its tip carrying a CLSIG of the fixture's LLMQ_TEST
//! quorum, judged by dashd rc.5 and V as the block rows are; L2-R2 re-judges the O8 recorder's
//! stored regtest recordings (recorder v24.0.0-rc.3-8-g0a36fab32cb5) that carry types 7 and 9 on
//! dashd rc.5, block by block. Every dashd here runs with spork 19 off ([`pinned`]). L2-R1 and
//! L2-R3 run in CH7a-5b's replay (a32d095cb), lib tests of `dashd-node`'s `sut::tests::recorded`
//! whose verdicts this test cannot read: each row names its tests (`l2_tests`), and the closure
//! checks each is a `#[test] fn` there and that they replay all seven recordings
//! ([`lib_problems`]). The testnet and mainnet rows are deferrals (J3), listed as skipped, never
//! passed.
//!
//! What is compared. dashd's side is `submitblock`'s answer only: null (accept) or the reject
//! reason. V's side is the state its `BlockChecked` carries, as Core's `submitblock_StateCatcher`
//! reads it: its answer, which must equal dashd's and the row's, and its reject's class, which
//! must equal the class the rows file gives with a Core file:line cite (`<key>_class`). dashd's
//! class is not observed: an RPC caller sees a failed block (`getchaintips` `invalid`) for
//! BLOCK_CONSENSUS and BLOCK_RESULT_UNSET alike (validation.cpp:2860,2875,3660), so the class is
//! code-cited, not measured. Every J0a reject is BLOCK_CONSENSUS at the tag. A row whose reject
//! Core leaves UNSET (`bad-cb-amount`, `bad-cb-payee`) needs the class measured: relay the probe
//! to dashd over P2P from a test peer and read the punishment (net_processing.cpp:1974-1981:
//! UNSET none, CONSENSUS `Misbehaving(100)` and a disconnect). No J0a row has one. On J2-C's
//! probes V's side also has the step-10 checks its verifier made on the probe, read from its
//! census (`ev1_j::l2::ClChecks`), which must be the ones the probe names.
//!
//! V's B-path answer comes from `process_block` and a `BlockChecked` listener in this test, not
//! from the node's `submitblock` handler. The two are equivalent: the handler's catcher
//! (`dashd-node/src/rpc.rs:1230-1240`) reads the same signal, which V sends after ConnectBlock as
//! Core does (`dashd-chainstate/src/chain/abc.rs:696-710`). This certifies the block path, not
//! the RPC.
//!
//! One producer, a dashd with a wallet driven over RPC, mines the setup chain and builds every
//! transaction and block; two validators judge: a dashd and V, neither of which ever mines.
//! Setup blocks reach both validators through `submitblock` (V: `ProcessNewBlock`) and must be
//! accepted, which is checked as setup, apart from the probes. Every probe is built before any
//! validator sees it, and each one is judged by fresh validators on the setup chain: a dashd on a
//! copy of the setup validator's datadir, and a new V that the setup blocks are fed to again. A
//! rejected probe never feeds the next one.
//!
//! The chain (`CHAIN_ARGS`, regtest at the tag, chainparams.cpp): DIP0003 at 48 with
//! `-dip3params` (:1152-1170; the default is 432, :823); DIP0024 moved out to 1000 as in
//! `tests/v24` (default 1, :830); V20 and MN_RR at 100 with `-testactivationheight` (their
//! defaults are DIP0003's default height, :833-834, which `-dip3params` does not move); V24
//! through miner signalling from 150 with `-vbparams`' ninth field turning its `useEHF` off
//! (:852-859, :1097-1139), as `feature_masternode_shares.py` does. `withdrawals` (600, :835) and
//! `evo_shares` stay inactive; no row reads them. The producer mines 200 blocks, block
//! [`FROZEN_HEIGHT`] carrying an ordinary output paying the shared-collateral template script
//! before V24 is active (D13-B2), then a masternode registration in block 201 (the setup tip).
//!
//! Each row also runs its control (`control`, `b_control`): the probe with its winning defect
//! taken out must give the next check's reject on both nodes, which shows the probe reaches the
//! intended check with every earlier one passing. D13-B and D13-B2 ([`STAGE_ROWS`]) must also run
//! an order probe and an order control (`order_probe`/`b_order`, `order_control`/`b_order_control`):
//! blocks with a second defect, a tx spending a missing input, which Core answers from
//! ConnectBlock's CheckTxInputs loop, placed so that the stage answering the probe shows. Rows at a later stage are listed as skipped, with their stage, and
//! never pass.
//!
//! The rows file is checked first against [`INVENTORY`], this test's own copy of plan §4: every
//! row, each path with its stage and outcome, and each path that does not apply with its reason.
//! Then the closure: every EV1 §6.3 branch and pair, and every D13 case, is covered or recorded
//! as excluded, and each cover's reason is its row's reject.
//!
//! Needs dashd v24.0.0-rc.5: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.5 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips the
//! dashd test; the rows' closure test always runs.

mod ev1_j;
mod v24;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{OutPoint, Transaction, TxIn, TxOut, core_serialize};
use dash_consensus::{Block, Tx};
use dash_dml::specialtx::covenant::{SHARED_COLLATERAL_SCRIPT, is_shared_collateral_script};
use dash_primitives::{TxHash, TxType};
use ev1_j::harness::*;
use serde_json::{Value, json};
use v24::{Dashd, hex};

/// The producer's: [`CHAIN_ARGS`], and its wallet writing without waiting for the disk
/// (`wallet/init.cpp:136`, a test-only option; validators run no wallet).
const PRODUCER_ARGS: [&str; 6] = [
    CHAIN_ARGS[0],
    CHAIN_ARGS[1],
    CHAIN_ARGS[2],
    CHAIN_ARGS[3],
    CHAIN_ARGS[4],
    "-unsafesqlitesync=1",
];
/// The txid of the order controls' missing input: no tx has it.
const MISSING: [u8; 32] = [0x55; 32];
/// The setup block whose hash the probes name as a known active-chain block: no quorum of any
/// type (the setup chain's commitments are all null).
const KNOWN_HEIGHT: u32 = 60;
/// Regtest's testdummy: in its window (start 0, no timeout) and not an EHF deployment
/// (chainparams.cpp:847-850; `useEHF` defaults to false).
const NON_EHF_BIT: u8 = 28;
/// The setup block with D13-B2's template output: past coinbase maturity and before V24, which
/// `-vbparams` activates no earlier than 150.
const FROZEN_HEIGHT: usize = 121;

// ---- the producer ---------------------------------------------------------------------------

/// The producer and what it built for the probes.
struct Produced {
    /// Blocks 1 to 201.
    setup: Vec<Arc<Block>>,
    /// The time every validator runs at, past every block the producer made.
    time: i64,
    /// An unsubmitted template on the setup tip (`generateblock` with no transactions).
    template: Block,
    /// A block of the setup chain, no quorum of any type.
    known: [u8; 32],
    /// The setup masternode's revocations, reason 0 and 1, neither broadcast.
    revocations: [Arc<Tx>; 2],
    /// The producer's wallet signing `tx`'s inputs.
    resign: Box<dyn Fn(&Tx) -> Tx>,
    /// The block mining the shared ProReg on the producer alone (invalidated there since), and
    /// its collateral outpoint.
    registration: (Arc<Block>, OutPoint),
    /// D13-B2: the template output of setup block [`FROZEN_HEIGHT`], and that tx's change output
    /// (a P2PKH output of the producer's wallet, locked there so no later setup tx spends it).
    frozen: (OutPoint, i64),
    change: (OutPoint, i64),
    p2pkh: Vec<u8>,
}

fn produce(producer: Dashd) -> Produced {
    pinned(&producer);
    let producer = Arc::new(producer);
    let p = &*producer;
    rpc(p, "createwallet", json!(["producer"]));
    let address = rpc(p, "getnewaddress", json!([]));
    let time = Cell::new(v24::T0);
    let tick = || {
        time.set(time.get() + 150);
        rpc(p, "setmocktime", json!([time.get()]));
    };
    let mine = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|_| {
                tick();
                rpc(p, "generatetoaddress", json!([1, address]))[0].clone()
            })
            .collect()
    };
    let mut hashes = mine(FROZEN_HEIGHT - 1);

    // D13-B2: an ordinary tx paying the template script, mined while V24 is inactive, when no
    // creation rule applies (`CheckSharedCollateralTemplateOutputs` runs under `rules.v24`,
    // specialtxman.cpp:915-917). The output is no masternode's collateral. It is nonstandard
    // (policy.cpp:119-130), so `generateblock` mines it without the mempool.
    let frozen_value = 10 * COIN;
    let raw = Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: Vec::new(),
        outputs: vec![TxOut {
            value: frozen_value,
            script_pubkey: SHARED_COLLATERAL_SCRIPT.to_vec().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    });
    let funded =
        rpc(p, "fundrawtransaction", json!([hex(&core_serialize(&raw)), {"feeRate": 0.0001}]));
    let signed = rpc(p, "signrawtransactionwithwallet", json!([funded["hex"]]));
    assert_eq!(signed["complete"], json!(true));
    let frozen_tx = tx_of(&signed["hex"]);
    let outputs = &frozen_tx.raw().outputs;
    assert_eq!(outputs.len(), 2, "the template output and the change");
    let at = |template: bool| {
        let index = outputs
            .iter()
            .position(|o| is_shared_collateral_script(o.script_pubkey.as_bytes()) == template);
        let index = index.unwrap();
        let outpoint = OutPoint {
            hash: frozen_tx.txid(),
            index: u32::try_from(index).unwrap(),
        };
        (outpoint, outputs[index].value)
    };
    let (frozen, change) = (at(true), at(false));
    let parent = hashes.last().unwrap().clone();
    let at_parent = rpc(p, "getdeploymentinfo", json!([parent]))["deployments"]["v24"].clone();
    assert_eq!(at_parent["active"], json!(false), "V24 inactive for the next block: {at_parent}");
    tick();
    let mined = rpc(p, "generateblock", json!([address, [signed["hex"]]]))["hash"].clone();
    hashes.push(mined);
    rpc(
        p,
        "lockunspent",
        json!([false, [{"txid": frozen_tx.txid().to_string(), "vout": change.0.index}]]),
    );
    assert_eq!(
        rpc(p, "listlockunspent", json!([])),
        json!([{"txid": frozen_tx.txid().to_string(), "vout": change.0.index}]),
        "the change output is locked"
    );
    hashes.extend(mine(200 - FROZEN_HEIGHT));

    // A masternode with a known operator key, registered in block 201.
    let operator = rpc(p, "bls", json!(["generate"]));
    let fee_source = rpc(p, "getnewaddress", json!([]));
    rpc(p, "sendtoaddress", json!([fee_source, 10]));
    let new_address = || rpc(p, "getnewaddress", json!([]));
    let pro_tx = rpc(
        p,
        "protx",
        json!([
            "register_fund",
            new_address(),
            "127.0.0.1:19999",
            new_address(),
            operator["public"],
            new_address(),
            0,
            new_address(),
            address
        ]),
    );
    hashes.extend(mine(1));
    assert_eq!(rpc(p, "getrawmempool", json!([])), json!([]), "setup mined every tx");
    let deployments = rpc(p, "getdeploymentinfo", json!([]))["deployments"].clone();
    for name in ["dip0003", "v20", "mn_rr", "v24"] {
        assert_eq!(deployments[name]["active"], json!(true), "{name}: {}", deployments[name]);
    }
    let setup: Vec<Arc<Block>> =
        hashes.iter().map(|h| Arc::new(block_of(&rpc(p, "getblock", json!([h, 0]))))).collect();

    let revoke = |reason: u16| {
        let hex = rpc(
            p,
            "protx",
            json!(["revoke", pro_tx, operator["secret"], reason, fee_source, false]),
        );
        Arc::new(tx_of(&hex))
    };
    let revocations = [revoke(0), revoke(1)];
    let known = internal(&rpc(p, "getblockhash", json!([KNOWN_HEIGHT])));
    let p2pkh =
        unhex(rpc(p, "validateaddress", json!([new_address()]))["scriptPubKey"].as_str().unwrap());

    tick();
    let template = block_of(&rpc(p, "generateblock", json!([address, [], false]))["hex"]);

    // A shared ProReg (feature_masternode_shares.py's register_shared), never sent to a
    // validator: a funding tx without its collateral output, which shared_register_prepare adds.
    let collateral = 1000 * COIN;
    let mut pay = serde_json::Map::new();
    pay.insert(new_address().as_str().unwrap().to_owned(), json!(1000));
    let raw = rpc(p, "createrawtransaction", json!([[], pay]));
    let funded = tx_of(&rpc(p, "fundrawtransaction", json!([raw, {"feeRate": 0.0001}]))["hex"]);
    let mut funding = funded.to_mutable();
    funding.outputs.retain(|o| o.value != collateral);
    assert_eq!(funding.outputs.len(), funded.raw().outputs.len() - 1);
    let shares = json!([
        {"amount": 600 * COIN, "refundAddress": new_address(), "ownerAddress": new_address()},
        {"amount": 400 * COIN, "refundAddress": new_address(), "ownerAddress": new_address()},
    ]);
    let prepared = rpc(
        p,
        "protx",
        json!([
            "shared_register_prepare",
            hex(&core_serialize(&Tx::new(funding))),
            shares,
            "127.0.0.1:19998",
            rpc(p, "bls", json!(["generate"]))["public"],
            new_address(),
            0,
            30,
            50 * COIN
        ]),
    );
    let signatures = rpc(p, "protx", json!(["shared_sign", prepared["tx"]]))["signatures"].clone();
    let combined = rpc(p, "protx", json!(["shared_combine", prepared["tx"], signatures]));
    let signed = rpc(p, "signrawtransactionwithwallet", json!([combined]));
    assert_eq!(signed["complete"], json!(true));
    let shared = tx_of(&signed["hex"]);
    let index = u32::try_from(prepared["collateralIndex"].as_u64().unwrap()).unwrap();

    // Mined on the producer alone, so the block's coinbase commits to the registration as dashd's
    // miner does, then invalidated there: the producer is back on the setup tip.
    rpc(p, "sendrawtransaction", json!([signed["hex"]]));
    let mined = mine(1).remove(0);
    let registration = Arc::new(block_of(&rpc(p, "getblock", json!([mined, 0]))));
    assert!(registration.txs().iter().any(|tx| tx.txid() == shared.txid()));
    rpc(p, "invalidateblock", json!([mined]));
    assert_eq!(rpc(p, "getbestblockhash", json!([])), *hashes.last().unwrap());

    let signer = Arc::clone(&producer);
    let resign = Box::new(move |tx: &Tx| {
        let signed =
            rpc(&signer, "signrawtransactionwithwallet", json!([hex(&core_serialize(tx))]));
        assert_eq!(signed["complete"], json!(true));
        tx_of(&signed["hex"])
    });
    Produced {
        setup,
        time: time.get(),
        template,
        known,
        revocations,
        resign,
        registration: (
            registration,
            OutPoint {
                hash: shared.txid(),
                index,
            },
        ),
        frozen,
        change,
        p2pkh,
    }
}

// ---- the probes -----------------------------------------------------------------------------

/// A type 9 unlock of J0a's: index 77, a fee of 1000 and `outputs` P2PKH outputs of 1000.
fn unlock(
    version: u8,
    outputs: usize,
    requested: u32,
    quorum: &[u8; 32],
    sig: &[u8],
    p2pkh: &[u8],
) -> Arc<Tx> {
    unlock_with(version, 77, 1000, 1000, outputs, requested, quorum, sig, p2pkh)
}

/// The J0a rows' probes and controls, built before any validator sees them.
fn probes(made: &Produced, pow: &PowParams) -> Vec<Probe> {
    let mut out = Vec::new();
    let mut add = |row: &'static str,
                   name: &str,
                   expect: &'static str,
                   txs: Vec<Arc<Tx>>,
                   template: &Block| {
        out.push(Probe {
            row,
            name: name.to_owned(),
            expect,
            block: probe_block(template, &txs, pow),
            below: None,
            cl_checks: None,
        })
    };
    let t = &made.template;
    let [rev0, rev1] = &made.revocations;
    // A well-formed basic-scheme BLS signature: the setup masternode's operator's.
    let sig = &rev0.raw().extra_payload[rev0.raw().extra_payload.len() - 96..];
    let tip = made.setup.len() as u32;
    let known = &made.known;
    let p2pkh = &made.p2pkh;

    add("7-S1", "bit 28, known block", "b", vec![mnhf(NON_EHF_BIT, known, sig)], t);
    add("7-S1", "bit 14, known block", "b_control", vec![mnhf(EHF_BIT, known, sig)], t);
    add("7-P1", "bit 28, unknown hash", "b", vec![mnhf(NON_EHF_BIT, &UNKNOWN, sig)], t);
    add("7-P1", "bit 14, unknown hash", "b_control", vec![mnhf(EHF_BIT, &UNKNOWN, sig)], t);
    for (row, quorum, controls) in
        [("9-P5a.1", &UNKNOWN, &[1][..]), ("9-P5a.2", known, &[1, 2][..])]
    {
        for version in [0, 3] {
            let tx = unlock(version, 1, tip, quorum, sig, p2pkh);
            add(row, &format!("payload version {version}"), "b", vec![tx], t);
        }
        for &version in controls {
            let tx = unlock(version, 1, tip, quorum, sig, p2pkh);
            add(row, &format!("payload version {version}"), "b_control", vec![tx], t);
        }
    }
    for (row, quorum) in [("9-P5b.1", &UNKNOWN), ("9-P5b.2", known)] {
        add(row, "33 outputs", "b", vec![unlock(1, 33, tip, quorum, sig, p2pkh)], t);
        add(row, "32 outputs", "b_control", vec![unlock(1, 32, tip, quorum, sig, p2pkh)], t);
    }

    // The reason-0 revocation with the reason-1 one's signature: the operator's key over another
    // payload. Its inputs signed again, as the payload is part of what they sign.
    let tail = |tx: &Tx| tx.raw().extra_payload[tx.raw().extra_payload.len() - 96..].to_vec();
    let mut bad = rev0.to_mutable();
    let n = bad.extra_payload.len();
    bad.extra_payload[n - 96..].copy_from_slice(&tail(rev1));
    let bad = Arc::new((made.resign)(&Tx::new(bad)));
    // The same payload naming no masternode (`CProUpRevTx`: version, proTxHash, ...), on an input
    // no check before CheckProUpRevTx's masternode lookup reads.
    let mut nobody = rev0.to_mutable();
    nobody.extra_payload[2..34].copy_from_slice(&UNKNOWN);
    nobody.inputs = vec![TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0x77; 32]),
            index: 0,
        },
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }];
    let nobody = Arc::new(Tx::new(nobody));
    add("OPSIG-B", "bad signature, then no masternode", "b", vec![bad.clone(), nobody.clone()], t);
    add(
        "OPSIG-B",
        "good signature, then no masternode",
        "b_control",
        vec![rev0.clone(), nobody.clone()],
        t,
    );
    add("OPSIG-B", "no masternode, then bad signature", "b_control", vec![nobody, bad], t);

    // D13-B2: an ordinary tx spending setup block FROZEN_HEIGHT's template output (anyone can
    // spend it at the script layer: an empty scriptSig), on the setup tip, where V24 is active.
    // No masternode has it as collateral, so FinishListForBlock passes it and ConnectBlock's
    // covenant answers (validation.cpp:2716-2725). The control spends the same tx's change output
    // instead, signed by the producer's wallet. Neither pays a fee: the template's coinbase pays the
    // masternode its share of a block without fees, and a fee would give `bad-cb-payee`.
    let spend_of = |(prevout, value): (OutPoint, i64)| Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value,
            script_pubkey: p2pkh.clone().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    let frozen = Arc::new(Tx::new(spend_of(made.frozen)));
    let ordinary = Arc::new((made.resign)(&Tx::new(spend_of(made.change))));
    // The order controls' second defect: an ordinary tx spending an outpoint no tx made, which
    // Core answers from ConnectBlock's CheckTxInputs (validation.cpp:2709-2713), after
    // ProcessSpecialTxsInBlock (:2674) and, in the per-tx loop, before that tx's covenant
    // (:2719-2725).
    let missing = Arc::new(Tx::new(spend_of((
        OutPoint {
            hash: TxHash::from(MISSING),
            index: 0,
        },
        COIN,
    ))));
    add("D13-B2", "a spend of the pre-V24 template output", "b", vec![frozen.clone()], t);
    add(
        "D13-B2",
        "the template spend, then a missing input",
        "b_order",
        vec![frozen.clone(), missing.clone()],
        t,
    );
    add("D13-B2", "a spend of an ordinary output", "b_control", vec![ordinary], t);
    add(
        "D13-B2",
        "a missing input, then the template spend",
        "b_order_control",
        vec![missing.clone(), frozen],
        t,
    );

    let (registration, collateral) = &made.registration;
    let spend = Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: *collateral,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 999 * COIN,
            script_pubkey: p2pkh.clone().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }));
    // The list build (ProcessSpecialTxsInBlock, validation.cpp:2674) runs before ConnectBlock's
    // CheckTxInputs loop (:2699-2713), so it answers the spend even behind a missing input.
    add(
        "D13-B",
        "registration, then a spend of its collateral",
        "b",
        vec![spend.clone()],
        registration,
    );
    add(
        "D13-B",
        "registration, a missing input, then the collateral spend",
        "b_order",
        vec![missing.clone(), spend],
        registration,
    );
    add("D13-B", "the registration block", "b_control", Vec::new(), registration);
    add(
        "D13-B",
        "registration, then a missing input",
        "b_order_control",
        vec![missing],
        registration,
    );
    out
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn rows_close_over_ev1_6_3() {
        let problems = rows_file_problems(&rows_text(), &pairs_text());
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn row_problems_are_found() {
        let text = "[[row]]\nid = \"x\"\ncase = \"c\"\nrecipe = \"r\"\nb = \"bad-x\"\nb_stage = \"J0a\"\n\
                    m = \"n/a: no\"\nm_stage = \"J1\"\nbav = \"bad-y\"\nfoo = \"z\"\n\
                    order_control = \"o\"\nb_control_class = \"BLOCK_CONSENSUS: x.cpp:1\"\n\
                    excludes = [\n  \"D13 list build\",\n]\n";
        assert_eq!(
            row_problems(&parse_rows(text)),
            [
                "x: unknown key foo",
                "x: a run row pins its probe count in b_probes",
                "x: a run row needs control and b_control",
                "x: bav has no stage",
                "x: m does not apply but has a stage",
                "x: order_control and b_order_control go together",
                "x: b is a reject without b_class",
                "x: bav is a reject without bav_class",
                "x: b_control_class without a reject",
                "x: excludes without notes",
            ]
        );
    }

    /// The rows file with `edit` applied to row `id`'s text, from its `[[row]]` to the next.
    fn edit_row(text: &str, id: &str, edit: impl Fn(&str) -> String) -> String {
        let marker = format!("id = \"{id}\"\n");
        let mut parts: Vec<String> = text.split("[[row]]\n").map(str::to_owned).collect();
        let at = parts.iter().position(|p| p.starts_with(&marker)).expect("the row");
        let edited = edit(&parts[at]);
        assert_ne!(edited, parts[at], "the edit changes {id}");
        parts[at] = edited;
        parts.join("[[row]]\n")
    }

    /// The rows file without row `id`.
    fn without_row(text: &str, id: &str) -> String {
        let marker = format!("id = \"{id}\"\n");
        let parts: Vec<&str> = text.split("[[row]]\n").collect();
        let kept: Vec<&str> = parts.iter().copied().filter(|p| !p.starts_with(&marker)).collect();
        assert_eq!(kept.len() + 1, parts.len(), "one row {id}");
        kept.join("[[row]]\n")
    }

    /// `row` without `lines`, each there once.
    fn cut(row: &str, lines: &str) -> String {
        assert_eq!(row.matches(lines).count(), 1, "{lines}");
        row.replace(lines, "")
    }

    /// What the check finds in the rows file with one edit.
    fn problems_after(edit: impl FnOnce(&str) -> String) -> Vec<String> {
        rows_file_problems(&edit(&rows_text()), &pairs_text())
    }

    #[test]
    fn a_dropped_m_path_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S1", |r| cut(r, "m = \"bad-mnhf-non-ehf\"\nm_stage = \"J1\"\n"))
        });
        assert_eq!(problems, ["7-S1 m: missing, §4 has J1 bad-mnhf-non-ehf"]);
    }

    #[test]
    fn a_moved_stage_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S1", |r| r.replace("b_stage = \"J0a\"", "b_stage = \"J0b\""))
        });
        assert_eq!(problems, ["7-S1 b: bad-mnhf-non-ehf at J0b, §4 has J0a bad-mnhf-non-ehf"]);
    }

    #[test]
    fn a_dropped_d14_row_fails() {
        for id in ["D14-33", "D14-34"] {
            let problems = problems_after(|t| without_row(t, id));
            assert_eq!(problems, [format!("{id}: the §4 row is missing")]);
        }
    }

    #[test]
    fn a_dropped_pair_fails() {
        let problems = problems_after(|t| edit_row(t, "7-P1", |r| cut(r, "  \"7c:4>7c:5a\",\n")));
        assert_eq!(problems, ["7c:4>7c:5a: no row covers or excludes it"]);
    }

    #[test]
    fn an_unrecorded_exclusion_fails() {
        // 7c:8 moved from 7-S5 to 9-X, which no path of applies, and D13's list build moved from
        // D13-B's covers to its excludes.
        let problems = problems_after(|t| {
            let t = edit_row(t, "7-S5", |r| cut(r, "  \"7c:8\",\n"));
            edit_row(&t, "9-X", |r| r.replace("  \"9c:13\",\n", "  \"9c:13\",\n  \"7c:8\",\n"))
        });
        assert_eq!(problems, ["7c:8 (9-X): an exclusion §4 does not record"]);
        let problems = problems_after(|t| {
            edit_row(t, "D13-B", |r| {
                cut(r, "covers = [\n  \"D13 list build\",\n]\n")
                    .replace("excludes = [\n", "excludes = [\n  \"D13 list build\",\n")
            })
        });
        assert_eq!(problems, ["D13 list build (D13-B): an exclusion §4 does not record"]);
    }

    #[test]
    fn a_dropped_order_probe_fails() {
        for id in STAGE_ROWS {
            let problems = problems_after(|t| {
                edit_row(t, id, |r| {
                    r.lines()
                        .filter(|l| !l.starts_with("order_probe =") && !l.starts_with("b_order ="))
                        .filter(|l| !l.starts_with("b_order_class ="))
                        .map(|l| format!("{l}\n"))
                        .collect()
                })
            });
            assert_eq!(problems, [format!("{id}: a stage row needs b_order and b_order_control")]);
        }
    }

    #[test]
    fn a_cover_on_another_reject_fails() {
        // 7c:8 (bad-mnhf-invalid) moved from 7-S5 to 7-S1 (bad-mnhf-non-ehf).
        let problems = problems_after(|t| {
            let t = edit_row(t, "7-S5", |r| cut(r, "  \"7c:8\",\n"));
            edit_row(&t, "7-S1", |r| r.replace("  \"7c:4\",\n", "  \"7c:4\",\n  \"7c:8\",\n"))
        });
        assert_eq!(
            problems,
            ["7-S1: covers 7c:8 (bad-mnhf-invalid), but its rejects are \
                 [\"bad-mnhf-non-ehf\", \"bad-mnhf-non-ehf\"]"]
        );
    }

    /// `row` without the lines starting with any of `keys` and " =".
    fn without_keys(row: &str, keys: &[&str]) -> String {
        row.lines()
            .filter(|l| !keys.iter().any(|k| l.starts_with(&format!("{k} ="))))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn a_dropped_pool_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| {
                without_keys(r, &["pool_control", "b_pool_control", "b_pool_control_class"])
            })
        });
        assert_eq!(problems, ["9-P1: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn a_pool_control_without_its_outcome_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| without_keys(r, &["b_pool_control", "b_pool_control_class"]))
        });
        assert_eq!(
            problems,
            [
                "9-P1: a type 9 reject needs its pool control",
                "9-P1: pool_control and b_pool_control go together",
            ]
        );
    }

    #[test]
    fn a_pool_control_on_another_reject_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| {
                r.replace(
                    "b_pool_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_pool_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-P1: a type 9 reject needs its pool control",
                "9-P1: b_pool_control bad-assetunlock-not-verified, a pool control's is \
                 bad-cbtx-assetlocked-amount",
            ]
        );
    }

    #[test]
    fn a_one_defect_unlock_row_without_its_pool_control_fails() {
        // 9-S5's control, the valid unlock in its place, swapped for an unlock signed by another
        // key: a reject of the special-tx loop, which shows nothing about the stage.
        let problems = problems_after(|t| {
            edit_row(t, "9-S5", |r| {
                r.replace(
                    "b_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(problems, ["9-S5: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn a_run_row_without_its_probe_count_fails() {
        let problems =
            problems_after(|t| edit_row(t, "9-OK", |r| r.replace("b_probes = \"4\"\n", "")));
        assert_eq!(problems, ["9-OK: a run row pins its probe count in b_probes"]);
    }

    #[test]
    fn a_pair_control_off_the_loser_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P4", |r| {
                r.replace(
                    "b_control = \"bad-assetunlock-not-verified\"",
                    "b_control = \"bad-assetunlock-too-late\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-P4: covers 9c:12>9c:14, but b_control bad-assetunlock-too-late is not 9c:14's reject"
            ]
        );
    }

    #[test]
    fn a_script_error_is_a_reason() {
        assert!(is_reason(SCRIPT_REJECT));
        assert!(is_reason("block-validation-failed"));
        assert!(is_reason(
            "mandatory-script-verify-flag-failed (Signature must be zero for failed CHECK(MULTI)SIG operation)"
        ));
        for not in [
            "",
            "Bad-x",
            "bad-x (open",
            "bad-x (\"quoted\")",
            " (text)",
            "bad x",
            "bad-x ()",
            "bad-x (a) (b)",
            "bad-x (a)b)",
        ] {
            assert!(!is_reason(not), "{not}");
        }
    }

    #[test]
    fn a_bav_run_row_without_its_mode_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S5", |r| without_keys(r, &["mode_control", "bav_mode_control"]))
        });
        assert_eq!(problems, ["7-S5: a BAV run row needs its mode control, accept"]);
    }

    #[test]
    fn a_mode_control_that_rejects_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-S5", |r| {
                r.replace(
                    "bav_mode_control = \"accept\"",
                    "bav_mode_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-S5: a BAV run row needs its mode control, accept",
                "9-S5: bav_mode_control is a reject without bav_mode_control_class",
            ]
        );
    }

    #[test]
    fn a_bav_run_row_without_its_twin_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-OK", |r| without_keys(r, &["twin", "bav_twin", "bav_twin_class"]))
        });
        assert_eq!(
            problems,
            [format!("9-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}")]
        );
        let problems = problems_after(|t| edit_row(t, "9-OK", |r| without_keys(r, &["twin"])));
        assert_eq!(
            problems,
            [
                format!("9-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}"),
                "9-OK: twin and bav_twin go together".to_owned(),
            ]
        );
    }

    #[test]
    fn a_twin_on_another_reject_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-OK", |r| {
                r.replace(
                    &format!("bav_twin = \"{SCRIPT_REJECT}\""),
                    "bav_twin = \"block-validation-failed\"",
                )
            })
        });
        assert_eq!(
            problems,
            [format!("7-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}")]
        );
    }

    #[test]
    fn a_bav_run_row_without_its_probe_count_fails() {
        for edit in ["", "bav_probes = \"2\"\n"] {
            let problems = problems_after(|t| {
                edit_row(t, "9-S5", |r| r.replace("bav_probes = \"3\"\n", edit))
            });
            assert_eq!(problems, ["9-S5: a run row pins its probe count in bav_probes"]);
        }
    }

    #[test]
    fn a_probe_count_on_a_path_not_run_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-R3", |r| {
                r.replace("b_probes = \"5\"\n", "b_probes = \"5\"\nbav_probes = \"3\"\n")
            })
        });
        assert_eq!(problems, ["9-R3: bav_probes on a path this head does not run"]);
        let problems = problems_after(|t| {
            edit_row(t, "9-X", |r| r.replace("bav = ", "b_probes = \"2\"\nbav = "))
        });
        assert_eq!(problems, ["9-X: b_probes on a path this head does not run"]);
    }

    #[test]
    fn a_dropped_age_control_fails() {
        let problems =
            problems_after(|t| edit_row(t, "D14-34", |r| without_keys(r, &["b_age_control"])));
        assert_eq!(problems, ["D14-34: an age row needs its age control, accept"]);
        let problems = problems_after(|t| {
            edit_row(t, "D14-34", |r| {
                r.replace(
                    "b_age_control = \"accept\"",
                    "b_age_control = \"bad-assetunlock-too-old-quorum\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "D14-34: an age row needs its age control, accept",
                "D14-34: b_age_control is a reject without b_age_control_class",
            ]
        );
    }

    #[test]
    fn an_age_control_off_its_row_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "D14-33", |r| {
                r.replace("b_probes = ", "b_age_control = \"accept\"\nb_probes = ")
            })
        });
        assert_eq!(problems, ["D14-33: b_age_control on a row that is no age row"]);
    }

    #[test]
    fn a_d14_unlock_reject_without_its_pool_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "D14-34", |r| {
                r.replace(
                    "b_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(problems, ["D14-34: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn block_paths_map_to_their_sub_stages() {
        let rows = parse_rows(&rows_text());
        let row = |id: &str| rows.iter().find(|r| r.id() == id).expect("the row");
        assert_eq!(run_stage(row("9-S5"), "b"), Some("J0b-1"));
        assert_eq!(run_stage(row("9-S5"), "bav"), Some("J0b-2"));
        assert_eq!(run_stage(row("D14-33"), "b"), Some("J0b-3"));
        assert_eq!(run_stage(row("7-S1"), "b"), Some("J0a"));
        assert_eq!(run_stage(row("L2-C2"), "b"), Some("J2-C"));
        assert_eq!(run_stage(row("L2-R2"), "l2"), Some("J2-R2"));
        for id in LIB_WIRED {
            assert_eq!(run_stage(row(id), "l2"), Some("J2-lib"));
        }
        for id in ["L2-testnet", "L2-mainnet"] {
            assert_eq!(run_stage(row(id), "l2"), Some(DEFERRED_STAGE));
        }
        assert!(!REACHED.contains(&DEFERRED_STAGE) && REACHED.contains(&"J2-lib"));
    }

    #[test]
    fn a_dropped_l2_row_fails() {
        for id in ["L2-R1", "L2-R2", "L2-R3", "L2-C1", "L2-C2", "L2-C3.9", "L2-C4.7", "L2-testnet"]
        {
            let problems = problems_after(|t| without_row(t, id));
            assert_eq!(problems, [format!("{id}: the §4 row is missing")]);
        }
    }

    #[test]
    fn a_deferral_run_at_j2_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "L2-testnet", |r| r.replace("l2_stage = \"J3\"", "l2_stage = \"J2\""))
        });
        assert_eq!(
            problems,
            [
                "L2-testnet: l2 deferred: CH7b-3a-2 (DEC-1082) at J2: a deferral is J3's, and only it",
                "L2-testnet: l2 at J2-R2, whose test runs [\"L2-R2\"] only",
                "L2-testnet l2: deferred: CH7b-3a-2 (DEC-1082) at J2, §4 has J3 deferred: CH7b-3a-2 \
                 (DEC-1082)",
            ]
        );
    }

    #[test]
    fn a_pass_at_j3_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "L2-mainnet", |r| r.replace("l2 = \"deferred: E-O8-1\"", "l2 = \"accept\""))
        });
        assert_eq!(
            problems,
            [
                "L2-mainnet: l2 accept at J3: a deferral is J3's, and only it",
                "L2-mainnet l2: accept at J3, §4 has J3 deferred: E-O8-1",
            ]
        );
    }

    #[test]
    fn an_l2_run_row_without_its_probe_count_fails() {
        let problems =
            problems_after(|t| edit_row(t, "L2-R2", |r| r.replace("l2_probes = \"7\"\n", "")));
        assert_eq!(problems, ["L2-R2: a run row pins its probe count in l2_probes"]);
        let problems =
            problems_after(|t| edit_row(t, "L2-R1", |r| r.replace("l2_probes = \"7\"\n", "")));
        assert_eq!(problems, ["L2-R1: a run row pins its probe count in l2_probes"]);
        let problems = problems_after(|t| {
            edit_row(t, "L2-testnet", |r| {
                r.replace("l2_stage = \"J3\"\n", "l2_stage = \"J3\"\nl2_probes = \"7\"\n")
            })
        });
        assert_eq!(problems, ["L2-testnet: l2_probes on a path this head does not run"]);
    }

    #[test]
    fn a_dropped_chainlock_null_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "L2-C1", |r| without_keys(r, &["control", "b_control", "b_control_class"]))
        });
        assert_eq!(problems, ["L2-C1: a run row needs control and b_control"]);
    }

    #[test]
    fn an_l2_unlock_row_without_its_pool_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "L2-C3.9", |r| {
                r.replace(
                    "b_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_control = \"bad-cbtx-invalid-clsig\"",
                )
            })
        });
        assert_eq!(problems, ["L2-C3.9: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn a_dropped_l2_c_path_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "L2-C2", |r| {
                r.lines().filter(|l| !l.starts_with("bav =")).map(|l| format!("{l}\n")).collect()
            })
        });
        assert_eq!(problems, [format!("L2-C2 bav: missing, §4 has {L2C_BAV}")]);
    }

    /// J2-C: the L2-C rows, types 7 and 9 next to a verified CbTx ChainLock on J0b's fixture
    /// extended by the ChainLock setup block (`ev1_j::l2`), on fresh validators.
    #[test]
    fn j2_l2_c_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = ev1_j::produce(&binary, &scratch.path().join("quorums"));
        let pow = regtest_pow(&scratch.path().join("pow"));
        let fixture = ev1_j::l2::chainlocked(made, &binary, scratch.path(), &pow);
        eprintln!("ChainLock fixture built at {:?}", started.elapsed());
        let chain = &fixture.chain;
        let base = scratch.path().join("validator");
        let pow = setup_validators(
            &binary,
            scratch.path(),
            &base,
            chain.time,
            &chain.setup,
            &chain.stale,
        );
        eprintln!("ChainLock chain on fresh validators at {:?}", started.elapsed());
        let probes = ev1_j::l2::probes(&fixture, &binary, scratch.path(), &pow);
        assert!(
            probes.iter().all(|p| p.cl_checks.is_some()),
            "every J2-C probe names V's step-10 checks"
        );
        let validators = Validators {
            chain: REGTEST,
            binary: &binary,
            base: &base,
            setup: &chain.setup,
            stale: &chain.stale,
            time: chain.time,
            scratch: scratch.path(),
        };
        let rows = parse_rows(&rows_text());
        judge_stage(&rows, "J2-C", "b", &probes, &validators, started);
    }

    /// J2-R2: L2-R2, every stored regtest recording whose node0 chain carries a type 7 or 9 tx,
    /// re-judged by dashd rc.5 segment by segment (`ev1_j::l2::rejudge`), the deployment states
    /// at each segment's end compared with the golden's. A block dashd rc.5 does not take is a
    /// finding (C6): the test stops there. Every dashd's datadir and log are kept until the test
    /// passes.
    #[test]
    fn j2_recordings_rejudged_by_dashd() {
        use ev1_j::l2::{RECORDINGS, Recording, carrying, rejudge, segments};
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let rows = parse_rows(&rows_text());
        let at_r2: Vec<&str> = rows
            .iter()
            .filter(|r| run_stage(r, "l2") == Some(J2_SUB_STAGES[1]))
            .map(Row::id)
            .collect();
        assert_eq!(at_r2, R2_WIRED, "the rows at J2-R2 are the rows this test runs");
        let row = rows.iter().find(|r| r.id() == "L2-R2").expect("L2-R2");
        assert_eq!(row.get("l2"), Some("accept"));
        assert_eq!(row.get("l2_probes"), Some(RECORDINGS.len().to_string().as_str()));
        let names: BTreeSet<String> = RECORDINGS.iter().map(|p| p.name.to_owned()).collect();
        assert_eq!(carrying(), names, "the recordings whose node0 chain carries a type 7 or 9 tx");
        let mut runs = Vec::new();
        for pin in &RECORDINGS {
            let rec = Recording::open(pin.name);
            let name = pin.name;
            assert_eq!(rec.blocks.len(), pin.blocks, "{name}: its blocks");
            assert_eq!(rec.heights_of(7), pin.type7, "{name}: its type 7 txs");
            assert_eq!(rec.heights_of(9), pin.type9, "{name}: its type 9 txs");
            let order: Vec<(u64, String)> =
                (0_u64..).zip(&rec.blocks).map(|(h, b)| (h, b.hash().to_string())).collect();
            assert_eq!(rec.connected(), order, "{name}: dashd connected each in turn");
            let segs = segments(&rec).unwrap_or_else(|e| panic!("{e}"));
            let first_heights: Vec<usize> = segs.iter().map(|s| s.from).collect();
            assert_eq!(first_heights, pin.segments, "{name}: its segments");
            runs.push((pin, rec, segs));
        }
        let count = |f: fn(&ev1_j::l2::Pinned) -> usize| RECORDINGS.iter().map(f).sum::<usize>();
        assert_eq!((count(|p| p.type7.len()), count(|p| p.type9.len())), (9, 19), "F1's txs");
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        // Kept up front, so a panic keeps every dashd's datadir and log; removed on a pass.
        let scratch = tempfile::tempdir().unwrap().keep();
        let started = std::time::Instant::now();
        let mut table = Vec::new();
        let mut differences = Vec::new();
        for (pin, rec, segs) in &runs {
            let done = rejudge(&binary, &scratch.join(pin.name), rec, segs).unwrap_or_else(|e| {
                panic!("{e}; dashd's datadir and log kept at {}", scratch.join(pin.name).display())
            });
            let (clock, rule) = rec.clock();
            eprintln!("{} re-judged at {:?}", pin.name, started.elapsed());
            table.push(format!(
                "{:<30} segments {:<16} starts {} blocks submitted {:<4} type 7 {} type 9 {:<2} \
                 clock {clock} ({rule}): every answer null, tip the block; deployment states \
                 at {:?}: {}",
                pin.name,
                format!("{:?}", pin.segments),
                done.starts,
                done.submitted,
                pin.type7.len(),
                pin.type9.len(),
                done.observed,
                if done.differences.is_empty() {
                    "the golden's"
                } else {
                    "DIFFER"
                },
            ));
            assert_eq!(done.submitted, pin.blocks - 1, "{}: every block", pin.name);
            let ends: Vec<usize> = segs.iter().map(|s| s.to).collect();
            assert_eq!(done.observed, ends, "{}: each segment's end observed", pin.name);
            differences.extend(done.differences);
        }
        print_skipped(&rows);
        eprintln!("{}", table.join("\n"));
        assert!(
            differences.is_empty(),
            "dashd's deployment states differ from the golden's (datadirs kept at {}): \
             {differences:#?}",
            scratch.display()
        );
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    /// The segment reader on feature_mnehf's own `extract.json` and `chainargs.json`, edited in
    /// memory (the stored files are never touched).
    fn mnehf_segments(edit: impl FnOnce(&mut Value, &mut Value)) -> Result<Vec<String>, String> {
        let rec = ev1_j::l2::Recording::open("feature_mnehf");
        let (mut extract, mut chainargs) = (rec.json("extract.json"), rec.json("chainargs.json"));
        edit(&mut extract, &mut chainargs);
        let tip = rec.blocks.len() - 1;
        let segs = ev1_j::l2::segments_in(&rec.name, tip, &extract, &chainargs)?;
        Ok(segs.iter().map(|s| format!("{}..={} {:?}", s.from, s.to, s.args)).collect())
    }

    #[test]
    fn the_recorded_starts_give_the_segments() {
        let segs = mnehf_segments(|_, _| {}).unwrap();
        let firsts: Vec<&str> = segs.iter().map(|s| s.split_once("..").unwrap().0).collect();
        assert_eq!(firsts, ["0", "177", "179", "201"]);
        for (seg, vbparams) in segs.iter().zip([
            "testdummy:-2:999999999999:0:4:4:4:5:1",
            "testdummy:0:1:0:4:4:4:5:1",
            "testdummy:1417713977:999999999999:0:4:4:4:5:1",
            "testdummy:1418923587:1419923587:0:4:4:4:5:1",
        ]) {
            assert!(seg.contains(&format!("\"-vbparams={vbparams}\"")), "{seg}");
            assert!(seg.contains("\"-llmqtestparams=1:1\""), "{seg}");
            assert!(seg.contains("\"-testactivationheight=mn_rr@100\""), "{seg}");
        }
    }

    #[test]
    fn a_start_with_a_changed_chain_parameter_fails() {
        let e = mnehf_segments(|_, c| c["starts"][0]["cmdline"]["llmqtestparams"] = json!(["2:2"]))
            .unwrap_err();
        assert!(e.starts_with("feature_mnehf: start 0 runs "), "{e}");
        assert!(e.contains("\"-llmqtestparams=2:2\""), "{e}");
    }

    #[test]
    fn a_start_missing_a_chain_parameter_fails() {
        let e = mnehf_segments(|_, c| {
            c["starts"][2]["cmdline"].as_object_mut().unwrap().remove("llmqtestplatformparams");
        })
        .unwrap_err();
        assert!(e.starts_with("feature_mnehf: start 2 runs "), "{e}");
        let config = mnehf_segments(|_, c| {
            let config = c["starts"][1]["config"].as_object_mut().unwrap();
            config.remove("regtest.dip3params");
        })
        .unwrap_err();
        assert!(config.starts_with("feature_mnehf: start 1 runs "), "{config}");
    }

    #[test]
    fn a_changed_final_start_fails() {
        let e = mnehf_segments(|_, c| {
            let last = c["starts"].as_array_mut().unwrap().last_mut().unwrap();
            last["cmdline"]["vbparams"] = json!(["testdummy:1418923587:1419923588:0:4:4:4:5:1"]);
        })
        .unwrap_err();
        assert!(e.starts_with("feature_mnehf: the last start runs "), "{e}");
        let height = mnehf_segments(|_, c| {
            let last = c["starts"].as_array_mut().unwrap().last_mut().unwrap();
            last["config"]["regtest.testactivationheight"] = json!(["v20@100", "mn_rr@101"]);
        })
        .unwrap_err();
        assert!(height.contains("runs ") && height.contains("mn_rr@101"), "{height}");
    }

    #[test]
    fn an_unclassified_start_argument_fails() {
        let e = mnehf_segments(|_, c| c["starts"][3]["cmdline"]["minimumchainwork"] = json!(["0"]))
            .unwrap_err();
        assert_eq!(e, "feature_mnehf: start 3: an unclassified argument -minimumchainwork");
        let group = mnehf_segments(|x, _| {
            let args = x["bip9"]["groups"][1]["args"].as_array_mut().unwrap();
            args.push(json!("-assumevalid=0"));
        })
        .unwrap_err();
        assert_eq!(group, "feature_mnehf: a group: an unclassified argument -assumevalid");
    }

    #[test]
    fn a_chain_parameter_on_the_command_line_and_in_the_config_fails() {
        let e = mnehf_segments(|_, c| {
            let dip3 = c["starts"][1]["config"]["regtest.dip3params"].clone();
            assert!(dip3.is_array(), "start 1's config sets dip3params");
            c["starts"][1]["cmdline"]["dip3params"] = dip3;
        })
        .unwrap_err();
        assert_eq!(
            e,
            "feature_mnehf: start 1: -dip3params on both the command line and in the config"
        );
    }

    #[test]
    fn a_group_off_its_starts_fails() {
        let e = mnehf_segments(|x, _| {
            let args = x["bip9"]["groups"][1]["args"].as_array_mut().unwrap();
            let at = args.iter().position(|a| a == "-vbparams=testdummy:0:1:0:4:4:4:5:1").unwrap();
            args[at] = json!("-vbparams=testdummy:0:2:0:4:4:4:5:1");
        })
        .unwrap_err();
        assert!(e.starts_with("feature_mnehf: a group ran "), "{e}");
    }

    /// `getdeploymentinfo`'s answer as rc.5 gives it, its BIP9 deployments `(name, status,
    /// since, status_next)`.
    fn deployment_info(entries: &[(&str, &str, u64, &str)]) -> Value {
        let mut deployments = serde_json::Map::new();
        for (name, status, since, next) in entries {
            let bip9 = json!({"status": status, "since": since, "status_next": next});
            deployments.insert((*name).to_owned(), json!({"type": "bip9", "bip9": bip9}));
        }
        json!({ "deployments": deployments })
    }

    #[test]
    fn deployment_states_are_read_as_the_recorder_reads_them() {
        use ev1_j::l2::deployment_problems;
        let golden = json!([
            ["testdummy", "failed", 0],
            ["v24", "defined", 0],
            ["evo_shares", "started", 209]
        ]);
        let rpc = deployment_info(&[
            ("v24", "defined", 0, "defined"),
            ("evo_shares", "defined", 0, "started"),
        ]);
        assert_eq!(deployment_problems(208, &rpc, &golden), Vec::<String>::new());
        let rpc = deployment_info(&[
            ("testdummy", "defined", 0, "defined"),
            ("v24", "defined", 0, "defined"),
            ("evo_shares", "started", 144, "started"),
        ]);
        assert_eq!(
            deployment_problems(208, &rpc, &golden),
            [
                "208: testdummy is (\"defined\", 0) on dashd, (\"failed\", 0) in the golden",
                "208: evo_shares is (\"started\", 144) on dashd, (\"started\", 209) in the golden",
            ]
        );
        let rpc = deployment_info(&[
            ("v24", "defined", 0, "defined"),
            ("evo_shares", "defined", 0, "started"),
            ("v25", "defined", 0, "defined"),
        ]);
        assert_eq!(
            deployment_problems(208, &rpc, &golden),
            ["208: dashd lists v25, the golden does not"]
        );
    }

    #[test]
    fn a_deployment_dropped_from_the_golden_or_an_empty_answer_fails() {
        use ev1_j::l2::deployment_problems;
        let rpc = deployment_info(&[
            ("v24", "defined", 0, "defined"),
            ("evo_shares", "defined", 0, "started"),
        ]);
        let dropped = json!([["v24", "defined", 0], ["evo_shares", "started", 209]]);
        assert_eq!(
            deployment_problems(208, &rpc, &dropped),
            ["208: the golden's s.dep names [Some(\"v24\"), Some(\"evo_shares\")], not \
                 [\"testdummy\", \"v24\", \"evo_shares\"]"]
        );
        let empty = deployment_problems(208, &json!({}), &json!([]));
        assert_eq!(
            empty,
            [
                "208: the golden's s.dep names [], not [\"testdummy\", \"v24\", \"evo_shares\"]",
                "208: dashd's getdeploymentinfo has no deployments",
            ]
        );
    }

    #[test]
    fn an_unwired_l2_row_at_j2_fails() {
        let extra = "[[row]]\nid = \"L2-R4\"\ncase = \"c\"\nrecipe = \"r\"\nl2 = \"accept\"\n\
                     l2_stage = \"J2\"\nl2_probes = \"1\"\n\n";
        let problems = problems_after(|t| {
            t.replacen("[[row]]\nid = \"L2-R3\"", &format!("{extra}[[row]]\nid = \"L2-R3\""), 1)
        });
        assert_eq!(
            problems,
            ["L2-R4: l2 at J2-R2, whose test runs [\"L2-R2\"] only", "L2-R4: not a §4 row"]
        );
    }

    /// L2-R3's tests, each replaying one recording that carries a type 7 or 9 tx, and L2-R1's,
    /// the same and the census.
    #[test]
    fn the_j2_lib_rows_name_each_recordings_replay() {
        let recorded = std::fs::read_to_string(RECORDED).unwrap();
        let (tests, consts) = (lib_tests(&recorded), str_consts(&recorded));
        let rows = parse_rows(&rows_text());
        let row = |id: &str| rows.iter().find(|r| r.id() == id).expect("the row");
        let runs = |id: &str| -> BTreeMap<&str, &str> {
            let names = row(id).l2_tests.iter().filter(|t| *t != L2_CENSUS);
            names
                .map(|t| (replay_of(t, &tests[t.as_str()], &consts).unwrap(), t.as_str()))
                .collect()
        };
        let want = BTreeMap::from([
            ("feature_asset_locks", "feature_asset_locks_connects_as_dashd_did"),
            (
                "feature_evo_shares_activation",
                "feature_evo_shares_activation_state_hashes_are_dashds",
            ),
            ("feature_governance_cl", "feature_governance_cl_connects_as_dashd_did"),
            ("feature_llmq_chainlocks", "feature_llmq_chainlocks_connects_as_dashd_did"),
            ("feature_llmq_evo", "feature_llmq_evo_connects_as_dashd_did"),
            ("feature_llmq_rotation", "feature_llmq_rotation_connects_as_dashd_did"),
            ("feature_mnehf", "feature_mnehf_connects_as_dashd_did"),
        ]);
        assert_eq!(runs("L2-R3"), want);
        assert_eq!(runs("L2-R1"), want);
        assert_eq!(row("L2-R1").l2_tests.len(), want.len() + 1);
        assert!(
            tests.contains_key(L2_CENSUS) && row("L2-R1").l2_tests.iter().any(|t| t == L2_CENSUS)
        );
    }

    /// The source readers: the attributes on either side of `#[test]`, a fn that is no test, and
    /// the one replay shape, by literal and by const, against bodies that only resemble it.
    #[test]
    fn lib_tests_and_their_replays_are_read() {
        let source = "const A: &str = \"rec_a\";\n\n#[test]\nfn t1() {\n    \
                      assert_eq!(replay(A), (1, 0));\n}\n\n#[cfg(feature = \"x\")]\n\
                      /// Doc.\n#[test]\n#[ignore]\nfn t2() {\n    \
                      assert_eq!(replay(\"rec_b\"), (1, 0));\n}\n\nfn helper() {\n    \
                      replay(B);\n}\n";
        let tests = lib_tests(source);
        assert_eq!(tests.keys().copied().collect::<Vec<_>>(), ["t1", "t2"]);
        assert!(tests["t1"].attrs.is_empty());
        assert_eq!(tests["t2"].attrs, ["#[cfg(feature = \"x\")]", "#[ignore]"]);
        let consts = str_consts(source);
        assert_eq!(replay_of("t1", &tests["t1"], &consts), Ok("rec_a"));
        assert_eq!(replay_of("t2", &tests["t2"], &consts), Ok("rec_b"));
        let shape = |body: &str| {
            let test = LibTest {
                attrs: Vec::new(),
                text: body,
            };
            replay_of("t", &test, &consts).map(str::to_owned)
        };
        let not = Err("t is not the one-line replay assert".to_owned());
        for body in [
            "fn t() {\n    // assert_eq!(replay(A), (1, 0));\n}",
            "fn t() {\n    return;\n    assert_eq!(replay(A), (1, 0));\n}",
            "fn t() {\n    assert_eq!(replay(A), (1, 0));\n    assert_eq!(replay(A), (1, 0));\n}",
            "fn t() {\n    let _ = \"assert_eq!(replay(A), (1, 0));\";\n}",
            "fn t() {\n    assert_eq!(replay(f(A)), (1, 0));\n}",
        ] {
            assert_eq!(shape(body), not, "{body}");
        }
        assert_eq!(
            shape("fn t() {\n    assert_eq!(replay(B), (1, 0));\n}"),
            Err("t replays B, no const".to_owned())
        );
    }

    /// The closure's problems with `recorded.rs` edited by `edit` (the file is never touched).
    fn recorded_problems_after(edit: impl FnOnce(&str) -> String) -> Vec<String> {
        let recorded = std::fs::read_to_string(RECORDED).unwrap();
        lib_problems(&parse_rows(&rows_text()), &edit(&recorded))
    }

    const MNEHF_TEST: &str = "#[test]\nfn feature_mnehf_connects_as_dashd_did() {\n    assert_eq!(replay(\"feature_mnehf\"), (2, 0));\n}";

    /// The gate's lib step runs no ignored test and no test a `cfg` leaves out, so a replay test
    /// under either is no evidence for its rows.
    #[test]
    fn an_ignored_or_cfg_disabled_lib_test_fails() {
        for (attr, edited) in [
            (
                "#[ignore = \"slow\"]",
                MNEHF_TEST.replacen("#[test]\n", "#[test]\n#[ignore = \"slow\"]\n", 1),
            ),
            ("#[cfg(any())]", format!("#[cfg(any())]\n{MNEHF_TEST}")),
        ] {
            let problems = recorded_problems_after(|r| {
                assert_eq!(r.matches(MNEHF_TEST).count(), 1);
                r.replace(MNEHF_TEST, &edited)
            });
            let carries = |id| {
                format!(
                    "{id}: feature_mnehf_connects_as_dashd_did carries [{attr:?}], so the gate may \
                     not run it"
                )
            };
            assert_eq!(problems, [carries("L2-R1"), carries("L2-R3")], "{attr}");
        }
    }

    /// An attribute `cargo fmt` wraps over lines, one with a `//` line before `#[test]`, and a
    /// `cfg` on the whole of recorded.rs are each read, so none passes as a gated test.
    #[test]
    fn a_wrapped_commented_or_module_attribute_fails() {
        let wrapped =
            "#[cfg(all(\n    feature = \"test-support\",\n    feature = \"not-in-the-gate\"\n))]\n";
        for prefix in [wrapped, "#[ignore]\n// Slow.\n"] {
            let problems = recorded_problems_after(|r| {
                r.replace(MNEHF_TEST, &format!("{prefix}{MNEHF_TEST}"))
            });
            assert_eq!(problems.len(), 2, "{problems:?}");
            for (p, id) in problems.iter().zip(["L2-R1", "L2-R3"]) {
                let carries = format!("{id}: feature_mnehf_connects_as_dashd_did carries [");
                assert!(p.starts_with(&carries), "{p}");
            }
        }
        let problems = recorded_problems_after(|r| format!("#![cfg(any())]\n{r}"));
        let inner = |id| {
            format!(
                "{id}: recorded.rs carries an inner attribute, which may keep it out of the gate"
            )
        };
        assert_eq!(problems, [inner("L2-R1"), inner("L2-R3")]);
    }

    /// A replay commented out, or one behind an early return, leaves a test that passes without
    /// replaying: refused.
    #[test]
    fn a_lib_test_that_skips_its_replay_fails() {
        let call = "    assert_eq!(replay(\"feature_mnehf\"), (2, 0));\n";
        for edited in [
            MNEHF_TEST.replacen(call, &format!("    // {}", &call[4..]), 1),
            MNEHF_TEST.replacen(call, &format!("    return;\n{call}"), 1),
        ] {
            let problems = recorded_problems_after(|r| r.replace(MNEHF_TEST, &edited));
            let shape = |id| {
                format!(
                    "{id}: feature_mnehf_connects_as_dashd_did is not the one-line replay assert"
                )
            };
            let missing = |id| {
                format!("{id}: l2_tests replay 6 of the 7 recordings, missing [\"feature_mnehf\"]")
            };
            assert_eq!(
                problems,
                [shape("L2-R1"), missing("L2-R1"), shape("L2-R3"), missing("L2-R3")],
                "{edited}"
            );
        }
    }

    /// Row `id` with its `l2_tests` item `from` replaced by `to` (removed when `to` is empty).
    fn with_lib_test(t: &str, id: &str, from: &str, to: &str) -> String {
        let item = |n: &str| {
            if n.is_empty() {
                String::new()
            } else {
                format!("  \"{n}\",\n")
            }
        };
        edit_row(t, id, |r| {
            assert_eq!(r.matches(&item(from)).count(), 1, "{from}");
            r.replace(&item(from), &item(to))
        })
    }

    #[test]
    fn a_misspelt_lib_test_fails() {
        let mnehf = "feature_mnehf_connects_as_dashd_did";
        let problems =
            problems_after(|t| with_lib_test(t, "L2-R3", mnehf, &mnehf[..mnehf.len() - 1]));
        assert_eq!(
            problems,
            [
                "L2-R3: l2_tests names feature_mnehf_connects_as_dashd_di, no #[test] fn in recorded.rs",
                "L2-R3: l2_tests replay 6 of the 7 recordings, missing [\"feature_mnehf\"]",
            ]
        );
    }

    #[test]
    fn a_lib_test_that_is_no_test_fails() {
        let problems = problems_after(|t| {
            with_lib_test(t, "L2-R3", "feature_llmq_evo_connects_as_dashd_did", "replay")
        });
        assert_eq!(
            problems,
            [
                "L2-R3: l2_tests names replay, no #[test] fn in recorded.rs",
                "L2-R3: l2_tests replay 6 of the 7 recordings, missing [\"feature_llmq_evo\"]",
            ]
        );
    }

    #[test]
    fn a_dropped_lib_test_fails() {
        let problems = problems_after(|t| {
            with_lib_test(t, "L2-R1", "feature_evo_shares_activation_state_hashes_are_dashds", "")
        });
        assert_eq!(
            problems,
            [
                "L2-R1: l2_tests replay 6 of the 7 recordings, missing [\"feature_evo_shares_activation\"]"
            ]
        );
        let problems = problems_after(|t| with_lib_test(t, "L2-R1", L2_CENSUS, ""));
        assert_eq!(problems, [format!("L2-R1: l2_tests without its census {L2_CENSUS}")]);
    }

    #[test]
    fn a_lib_test_of_a_recording_without_type_7_or_9_fails() {
        let problems = problems_after(|t| {
            with_lib_test(
                t,
                "L2-R3",
                "feature_mnehf_connects_as_dashd_did",
                "feature_governance_state_hashes_are_dashds",
            )
        });
        assert_eq!(
            problems,
            [
                "L2-R3: feature_governance_state_hashes_are_dashds replays feature_governance, \
                 which carries no type 7 or 9 tx",
                "L2-R3: l2_tests replay 6 of the 7 recordings, missing [\"feature_mnehf\"]",
            ]
        );
    }

    #[test]
    fn l2_tests_off_j2_lib_or_missing_fail() {
        let list = "l2_tests = [\n  \"feature_mnehf_connects_as_dashd_did\",\n]\n";
        let problems = problems_after(|t| {
            edit_row(t, "L2-R2", |r| {
                r.replace("l2_probes = \"7\"\n", &format!("l2_probes = \"7\"\n{list}"))
            })
        });
        assert_eq!(problems, ["L2-R2: l2_tests on a row not at J2-lib"]);
        let problems = problems_after(|t| {
            edit_row(t, "L2-R3", |r| {
                let start = r.find("l2_tests = [\n").expect("the list");
                let end = start + r[start..].find("\n]\n").expect("its end") + 3;
                format!("{}{}", &r[..start], &r[end..])
            })
        });
        assert_eq!(problems, ["L2-R3: a J2-lib row names its lib tests in l2_tests"]);
        let problems = problems_after(|t| {
            edit_row(t, "L2-R3", |r| r.replace("l2_probes = \"7\"", "l2_probes = \"6\""))
        });
        assert_eq!(problems, ["L2-R3: l2_probes 6, but 7 recordings"]);
    }

    #[test]
    fn j0b1_block_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = ev1_j::produce(&binary, &scratch.path().join("quorums"));
        eprintln!("quorum chain produced at {:?}", started.elapsed());
        let base = scratch.path().join("validator");
        let pow =
            setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &made.stale);
        eprintln!("quorum chain on fresh validators at {:?}", started.elapsed());
        let probes = ev1_j::probes::probes(&made, &binary, scratch.path(), &pow);
        let validators = Validators {
            chain: REGTEST,
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &made.stale,
            time: made.time,
            scratch: scratch.path(),
        };
        let rows = parse_rows(&rows_text());
        judge_stage(&rows, "J0b-1", "b", &probes, &validators, started);
    }

    /// J0b-2: the BAV rows on fresh validators below assumevalid (`ev1_j::bav`).
    #[test]
    fn j0b2_bav_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = ev1_j::produce(&binary, &scratch.path().join("quorums"));
        let base = scratch.path().join("validator");
        let pow =
            setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &made.stale);
        eprintln!("quorum chain on fresh validators at {:?}", started.elapsed());
        let probes = ev1_j::bav::probes(&made, &binary, scratch.path(), &pow);
        eprintln!("BAV probes and their headers built at {:?}", started.elapsed());
        let validators = Validators {
            chain: REGTEST,
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &made.stale,
            time: made.time,
            scratch: scratch.path(),
        };
        let rows = parse_rows(&rows_text());
        judge_stage(&rows, "J0b-2", "bav", &probes, &validators, started);
    }

    #[test]
    fn j0a_block_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let rows = parse_rows(&rows_text());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = produce(Dashd::start_at(
            &binary,
            &scratch.path().join("producer"),
            v24::T0,
            &PRODUCER_ARGS,
        ));
        eprintln!("produced at {:?}", started.elapsed());

        let base = scratch.path().join("validator");
        let pow = setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &[]);

        eprintln!("setup validators at {:?}", started.elapsed());
        let probes = probes(&made, &pow);
        let validators = Validators {
            chain: REGTEST,
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &[],
            time: made.time,
            scratch: scratch.path(),
        };
        judge_stage(&rows, "J0a", "b", &probes, &validators, started);
    }
}

//! CH3a a6: `TestBlockValidity` through V (CH3 §2.14, §2.17, §2.8, §2.9; `v:4923-4974`), on the
//! connector harness: `ChainCommand::TestBlockValidity` on the tip, its pre-checks with Core's
//! classes and lines, `ConnectBlock(…, fJustCheck = true)` over a dropped overlay, the readiness
//! guard as an RPC error, the store-both caches, and what never happens (an index entry, a coin,
//! an undo or block write, a status bit, an evo commit, a report).

use dash_chain::HeaderStop;
use dash_consensus::hash::hash160;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::Header;
use dash_consensus::script::{
    BuriedScriptRules, SIGHASH_ALL, ScriptFlags, block_script_flags, push_data, signature_hash,
};
use dash_num::CompactTarget;
use dash_primitives::BlockHeader;
use secp256k1::{Message, PublicKey, SecretKey};

use super::*;
use crate::chain::NotTested;
use crate::chain::reply::WaitError;
use crate::chain::tests::support::{accept_levels, batch, levels_on};

/// `TestBlockValidity(m, check_pow, check_merkle)` through V.
fn propose(w: &World, m: &Mined, pow: bool, merkle: bool) -> Result<BlockState, NotTested> {
    w.rig.v.test_block_validity(m.block.clone(), pow, merkle).recv().expect("V replies")
}

/// [`propose`] of a block on the tip: its state.
fn verdict(w: &World, m: &Mined, pow: bool, merkle: bool) -> BlockState {
    propose(w, m, pow, merkle).expect("on the tip")
}

/// The index's entries: its ids are dense from genesis's 0.
fn index_len(rig: &Rig) -> usize {
    let reader = rig.v.reader();
    (0..).take_while(|&i| reader.entry(BlockId(i)).is_some()).count()
}

/// The undo files' bytes on disk.
fn rev_bytes(w: &World) -> u64 {
    let dir = w.stores.dir.path().join("blocks");
    std::fs::read_dir(dir)
        .expect("the blocks directory")
        .map(|e| e.expect("an entry"))
        .filter(|e| e.file_name().to_string_lossy().starts_with("rev"))
        .map(|e| e.metadata().expect("metadata").len())
        .sum()
}

/// The scripted helper's queries so far.
fn queries(w: &World) -> usize {
    w.seen.lock().expect("seen").queries
}

/// The connector's script caches.
fn caches(w: &World) -> Arc<ScriptCaches> {
    w.seen.lock().expect("seen").caches.clone().expect("set by the harness")
}

/// Whether `t`'s script-exec entry at height `h` (no signature checks: OP_TRUE outputs) is held.
fn exec_cached(w: &World, t: &Tx, h: u32) -> bool {
    let p = &w.rig.p;
    let on = |d| h >= p.consensus.deployment_height(d).to_u32();
    let flags: ScriptFlags = block_script_flags(BuriedScriptRules {
        dersig: on(BuriedDeployment::DerSig),
        cltv: on(BuriedDeployment::Cltv),
        csv: on(BuriedDeployment::Csv),
        bip147: on(BuriedDeployment::Bip147),
    });
    let caches = caches(w);
    let entry = caches.exec.entry(t.txid().as_bytes(), flags);
    caches.exec.cache().contains(entry, false)
}

/// `m` with the merkle root of `root` (if any) and the first nonce whose X11 meets regtest's
/// target exactly when `pow`.
fn reheader(p: &ChainSetup, m: &Mined, root: Option<&Mined>, pow: bool) -> Mined {
    let target = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let mut raw = *m.block.header().raw();
    if let Some(r) = root {
        raw.merkle_root = r.block.header().raw().merkle_root;
    }
    let header = (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                nonce,
                ..raw
            })
        })
        .find(|h| {
            check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(raw.bits), &target) == pow
        })
        .expect("a nonce");
    Mined {
        block: Arc::new(Block::new(header, m.block.txs().to_vec())),
        height: m.height,
    }
}

/// The bracket of a proposal rejected at a pre-check (step 3): the evo transaction began and
/// rolled back, and no helper scope opened.
const PRE_CHECKED: [Call; 4] =
    [Call::BeginAttempt, Call::BeginTx, Call::FinishAttempt, Call::RollbackTx];

/// The bracket of a proposal rejected at the ChainLock check (step 1): before `viewNew` and the
/// evo transaction (`v:4937-4950`).
const CL_REJECTED: [Call; 2] = [Call::BeginAttempt, Call::FinishAttempt];

/// Nothing reported, no latch trip, and V serving the next command: `m`'s header batch.
fn assert_serving(w: &World, m: &Mined) {
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    w.rig.assert_no_abort();
    assert!(w.seen.lock().expect("seen").latch.tripped().is_none(), "the latch tripped");
    w.rig.headers(std::slice::from_ref(m));
    assert!(w.rig.v.reader().lookup(&m.hash()).is_some(), "V served the header batch");
}

/// CH3 §2.14 steps 2 and 4 (`v:4942-4973`): a valid proposal on the tip is `Valid` and changes
/// nothing. No index entry (the dense ids, the hash's lookup), no block or undo written, the
/// tip's status as it was, the active coins at the tip with the spent seed unspent and the new
/// output absent, no signal. `ConnectBlock` ran (its helper scope ended) and returned at step 21:
/// no evo best block, the evo transaction rolled back (H10). The block then connects as usual.
#[test]
fn a_valid_proposal_is_valid_and_changes_nothing() {
    let w = world(Setup {
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        ..Setup::default()
    });
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    let t = spend(&[seed_op(1)], 49 * COIN);
    let b2 = block_on(&p, &b1, vec![t.clone()]);
    let before = (index_len(&w.rig), w.stores.blocks.stored(), rev_bytes(&w), w.rig.status(&b1));
    w.rig.sync();
    let _ = w.rig.rec.take();

    assert_eq!(propose(&w, &b2, false, true), Ok(BlockState::Valid));

    assert_eq!(w.rig.v.reader().lookup(&b2.hash()), None, "not inserted");
    let after = (index_len(&w.rig), w.stores.blocks.stored(), rev_bytes(&w), w.rig.status(&b1));
    assert_eq!(after, before, "(index, blocks, undo bytes, the tip's status)");
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    {
        let mut coins = w.coins.lock().expect("coins");
        let seed = coins.fetch(&OutPointKey::new(&seed_op(1))).expect("reads");
        assert!(seed.is_some(), "the spent seed is unspent");
        let added = coins.fetch(&OutPointKey::new(&op(&t, 0))).expect("reads");
        assert!(added.is_none(), "the proposal's output is not a coin");
    }
    {
        let seen = w.seen.lock().expect("seen");
        assert_eq!(seen.last_bracket(), NOT_CONNECTED, "rolled back, no SetBestBlock");
        assert_eq!(seen.evo_best, Some(b1.hash()), "no evo commit");
        assert_eq!(seen.just_checks.last(), Some(&true), "H4 is told fJustCheck (v:2663-2664)");
    }
    w.rig.sync();
    assert!(w.rig.rec.take().is_empty(), "no signal");
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty());

    w.rig.accept(&b2);
    assert_eq!(coins_at(&w.coins), Some(b2.hash()));
    assert_eq!(w.seen.lock().expect("seen").just_checks.last(), Some(&false), "a connect's");
}

/// CH3 §2.14 step 3 (`v:4952-4967`): each pre-check's rejection with Core's class, reason and
/// `LogError` line (`v:4955`, `:4960`, `:4965`), before `ConnectBlock`: the helper is never
/// asked and no helper scope ends. CheckBlock's PoW and merkle checks follow the caller's flags:
/// a bad merkle root is rejected only with merkle on, a missed target only with PoW on; with
/// both off (`CreateNewBlock`'s) each block is valid, as `ConnectBlock`'s own `CheckBlock` runs
/// with both off (`v:2505`).
#[test]
fn each_pre_check_rejects_with_cores_class_and_line() {
    use BlockValidationResult as R;

    install_accept_log();
    let w = world(Setup::default());
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    // b1's MTP is its own time, the median of genesis's and b1's (`chain.h` GetMedianTimePast).
    let t = b1.time();
    let old = Mined {
        block: mine(&p, b1.hash(), t, vec![coinbase_tx(2, 1, 0, COIN)]),
        height: 2,
    };
    let b2 = block_on(&p, &b1, vec![]);
    let bad_root = reheader(&p, &b2, Some(&b1), true);
    let missed = reheader(&p, &b2, None, false);
    let wrong_height = with_coinbase(&p, &b1, coinbase_tx(7, 1, 0, COIN), vec![]);
    let old_line = format!(
        "TestBlockValidity: Consensus::ContextualCheckBlockHeader: time-too-old, block's timestamp \
         is too early {t} {t}"
    );
    let root_line =
        "TestBlockValidity: Consensus::CheckBlock: bad-txnmrklroot, hashMerkleRoot mismatch";
    let cases = [
        (&old, (false, true), R::InvalidHeader, "time-too-old", old_line.clone()),
        (&bad_root, (false, true), R::Mutated, "bad-txnmrklroot", root_line.to_owned()),
        (
            &missed,
            (true, false),
            R::InvalidHeader,
            "high-hash",
            "TestBlockValidity: Consensus::CheckBlock: high-hash, proof of work failed".to_owned(),
        ),
        (
            &wrong_height,
            (false, true),
            R::Consensus,
            "bad-cb-height",
            "TestBlockValidity: Consensus::ContextualCheckBlock: bad-cb-height, block height \
             mismatch in coinbase"
                .to_owned(),
        ),
    ];
    // The lines name no block, and other tests log them too: count this world's V's.
    let logged = |line: &str| levels_on(w.rig.v_thread(), line);
    for (m, (pow, merkle), result, reason, line) in cases {
        let (lines, asked) = (logged(&line).len(), queries(&w));
        let got = verdict(&w, m, pow, merkle);
        assert_verdict(&got, &reject(result, reason));
        assert_eq!(logged(&line)[lines..], [tracing::Level::ERROR], "{reason}");
        assert_eq!(queries(&w), asked, "{reason}: the helper was asked");
        assert_eq!(bracket(&w), PRE_CHECKED, "{reason}");
        assert_eq!(w.rig.v.reader().lookup(&m.hash()), None, "{reason}");
    }
    // Two faults, the header's time and the merkle root: CCBH runs first (`v:4953-4956`), so its
    // reason and line only.
    let both = reheader(&p, &old, Some(&b1), true);
    let lines = (logged(&old_line).len(), logged(root_line).len());
    let got = verdict(&w, &both, false, true);
    assert_verdict(&got, &reject(R::InvalidHeader, "time-too-old"));
    assert_eq!(logged(&old_line)[lines.0..], [tracing::Level::ERROR]);
    assert_eq!(logged(root_line).len(), lines.1, "CheckBlock never ran");
    // The other flag on, then both off: valid.
    for (m, pow, merkle) in [(&bad_root, true, false), (&missed, false, true)] {
        assert_eq!(verdict(&w, m, pow, merkle), BlockState::Valid, "{pow} {merkle}");
        assert_eq!(verdict(&w, m, false, false), BlockState::Valid, "both off");
    }
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty());
}

/// CH3 §2.14 step 4: `ConnectBlock`'s own rejections in a proposal, with its class: an
/// overpaying coinbase (`bad-cb-amount`, `v:2846-2849`) and a missing input (`CheckTxInputs`). The
/// helper was asked and its scope ended; nothing is inserted or reported, the coins stay.
#[test]
fn connect_block_rejects_inside_a_proposal_with_its_class() {
    let w = world(Setup::default());
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    let overpaid = with_coinbase(&p, &b1, coinbase_tx(2, 1, 0, 1_000_000 * COIN), vec![]);
    let missing = block_on(&p, &b1, vec![spend(&[seed_op(9)], COIN)]);
    for (m, want) in [
        (&overpaid, reject(BlockValidationResult::Unset, "bad-cb-amount")),
        (&missing, reject(BlockValidationResult::Consensus, "bad-txns-inputs-missingorspent")),
    ] {
        let asked = queries(&w);
        assert_verdict(&verdict(&w, m, false, true), &want);
        assert!(queries(&w) > asked, "{want}: ConnectBlock asks the helper");
        assert_eq!(bracket(&w), NOT_CONNECTED, "{want}");
        assert_eq!(w.rig.v.reader().lookup(&m.hash()), None, "{want}");
    }
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty());
}

/// The reviewer's `INVALID_PREV` vs `CHAINLOCK` (CH3 §2.14 step 1): one ChainLock view whose lock
/// conflicts with every block at height 2. b2's proposal is `InvalidPrev` with Core's line
/// (`v:4937-4939`), before `viewNew` and the evo transaction. The same block on the tip path
/// passes accept (V's chain holds no lock) and fails `ConnectBlock`'s check (`v:2516-2519`),
/// which `NodeHelper` answers from the same view: `ChainLock`.
#[test]
fn a_conflicting_chainlock_is_invalid_prev_in_a_proposal_and_chainlock_at_connect() {
    const LINE: &str = "ERROR: TestBlockValidity: conflicting with chainlock";
    install_accept_log();
    let w = world(Setup {
        node_helper: true,
        locked_at: Some(2),
        ..Setup::default()
    });
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    let b2 = block_on(&p, &b1, vec![]);
    let lines = accept_levels(LINE).len();
    let got = verdict(&w, &b2, false, true);
    assert_verdict(&got, &reject(BlockValidationResult::InvalidPrev, "bad-chainlock"));
    // The line names no block and `only_a_proposal_reaching_connect_block_opens_the_helpers_scope`
    // logs it too, possibly in between: at least one more, every one at INFO (`LogPrintf`).
    let new = accept_levels(LINE)[lines..].to_vec();
    assert!(!new.is_empty() && new.iter().all(|l| *l == tracing::Level::INFO), "{new:?}");
    assert_eq!(bracket(&w), CL_REJECTED);
    let tip = last_state(&w, std::slice::from_ref(&b2));
    assert_verdict(&tip, &reject(BlockValidationResult::ChainLock, "bad-chainlock"));
    assert!(w.rig.status(&b2).has(Status::FAILED_VALID));
    w.rig.assert_no_abort();
}

/// CH3 §2.17 in a proposal: a G1 refusal (DIP0003 at block 20 without complete hooks) and a G2
/// one (the interim helper's) are `BlockState::Error` with the refusal text, Core's `state.Error`
/// that `getblocktemplate` and `CreateNewBlock` turn into an RPC error. Nothing is reported, the
/// latch stays untripped, and V serves the next command. The tip path's refusal of the same
/// block stops V (`g1_refuses_the_dip3_block_and_again_after_a_restart`).
#[test]
fn the_guard_refuses_a_proposal_without_stopping_the_node() {
    let p = dip3_at_20();
    let w = world(Setup {
        params: Some(p.clone()),
        ..Setup::default()
    });
    let blocks = empty_chain(&p, &w.rig.genesis, 20);
    for m in &blocks[..19] {
        w.rig.accept(m);
    }
    let b20 = &blocks[19];
    let msg = refusal(&p, b20, &g1_capability("DIP0003"));
    assert_eq!(propose(&w, b20, false, true), Ok(BlockState::Error(msg.clone())));
    assert_eq!(bracket(&w), NOT_CONNECTED, "G1: rolled back");
    assert_serving(&w, b20);
    assert_eq!(propose(&w, b20, false, false), Ok(BlockState::Error(msg)), "again");

    let w = world(Setup {
        helper: Scripted {
            interim_at: Some(1),
            ..Scripted::default()
        },
        ..Setup::default()
    });
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let msg = g2_refusal_text(&w.rig.p, &at(&b1), INTERIM_REFUSAL);
    assert_eq!(propose(&w, &b1, false, true), Ok(BlockState::Error(msg)));
    assert_eq!(bracket(&w), NOT_CONNECTED, "G2: rolled back");
    assert_serving(&w, &b1);
}

/// CH7 D2 in a proposal: H4's `NotReady` is G1's unreported refusal, `BlockState::Error` with
/// the capability in the refusal text; the latch stays untripped and V serves the next command.
#[test]
fn an_h4_readiness_refusal_in_a_proposal_does_not_stop_the_node() {
    const NOT_READY: &str = "quorum validation (CH7b)";
    let w = world(Setup {
        steps: StepsCfg {
            not_ready_in_h4: Some(NOT_READY),
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let msg = refusal(&w.rig.p, &b1, NOT_READY);
    assert_eq!(propose(&w, &b1, false, true), Ok(BlockState::Error(msg)));
    assert_eq!(bracket(&w), NOT_CONNECTED, "rolled back");
    assert_serving(&w, &b1);
    w.rig.assert_no_abort();
}

/// D121 with D75: a proposal's G2 refusal is unreported, so when the attempt's log (step 18's
/// IS removal of lock A) then fails to apply, at the effect or at the release, the failure is
/// D4's `AbortNode`: V stops on it, reported once, and nothing of the attempt stands.
#[test]
fn a_failed_application_after_a_refused_proposal_stops_the_node() {
    for k in [0, 1] {
        let mut setup = is_setup(vec![lock_a()], true);
        setup.steps.fail_apply = Some(k);
        setup.helper.interim_at = Some(2);
        let mut w = world(setup);
        let p = w.rig.p.clone();
        // b1 seeds the coins (the harness's first connect).
        let b1 = block_on(&p, &w.rig.genesis, vec![]);
        w.rig.accept(&b1);
        let b2 = block_on(&p, &b1, vec![spend(&[seed_op(1)], COIN)]);
        let r = w.rig.v.test_block_validity(b2.block.clone(), false, true).recv();
        assert_eq!(r, Err(WaitError::Disconnected), "k {k}: V stopped");
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(APPLY_FAILED), "k {k}");
        assert_eq!(w.sink.messages(), [APPLY_FAILED], "k {k}: one report");
        assert_eq!(bracket(&w), NOT_CONNECTED, "k {k}");
        assert_eq!(exit.chain.reader().lookup(&b2.hash()), None, "k {k}: not inserted");
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "k {k}");
    }
}

/// D121: Core's own `AbortNode` inside `ConnectBlock` stays in a proposal. An evo store behind
/// the parent (`v:2525-2531`) is reported once and trips the latch, and the reply is Core's
/// `state.Error`. When the attempt's apply fails too, D75 keeps the reported error: the same
/// reply and report, and `finish_attempt`'s failure only logged.
#[test]
fn an_evodb_inconsistency_aborts_a_proposal_too() {
    install_accept_log();
    for fail in [false, true] {
        let w = world(Setup {
            steps: StepsCfg {
                complete: true,
                verify_best: false,
                ..StepsCfg::default()
            },
            extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
            ..Setup::default()
        });
        w.seen.lock().expect("seen").fail_next_apply = fail;
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        let msg = EVODB_INCONSISTENCY.to_owned();
        let got = propose(&w, &b1, false, true);
        assert_eq!(got, Ok(BlockState::Error(msg.clone())), "{fail}");
        assert_eq!(w.sink.messages(), [msg], "{fail}: one report");
        let latch = w.seen.lock().expect("seen").latch.clone();
        assert!(latch.tripped().is_some(), "{fail}: the latch tripped");
        assert_eq!(w.rig.v.reader().lookup(&b1.hash()), None, "{fail}: not inserted");
        let line = format!("finish_attempt: {APPLY_FAILED}");
        let logged = levels_on(w.rig.v_thread(), &line);
        assert_eq!(
            logged,
            if fail {
                vec![tracing::Level::ERROR]
            } else {
                vec![]
            },
            "{fail}"
        );
    }
}

/// CH3 §2.9's store-both (`v:2750-2752`, `fCacheResults = fJustCheck` as `cacheSigStore` and
/// `cacheFullScriptStore`): a valid proposal spending a seed stores its script-exec entry at
/// `-par=1`. The block's connect then hits it with `erase = !cacheFullScriptStore` (`v:2174`),
/// which only marks the slot erasable (`cuckoocache.h:480-483`): the entry stays. With threads
/// (here 3) the proposal's checks are queued, and Core inserts no exec entry
/// (`cacheFullScriptStore && !pvChecks`, `v:2227`; CH3a D126). A plain connect of the same block
/// stores none.
#[test]
fn a_proposal_stores_script_results_and_a_connect_does_not() {
    for threads in [0, 3] {
        let setup = || Setup {
            seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
            script_threads: threads,
            ..Setup::default()
        };
        let serial = threads == 0;
        let t = spend(&[seed_op(1)], 49 * COIN);
        let w = world(setup());
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        let b2 = block_on(&w.rig.p, &b1, vec![t.clone()]);
        w.rig.accept(&b1);
        assert!(!exec_cached(&w, &t, 2));
        assert_eq!(propose(&w, &b2, false, true), Ok(BlockState::Valid));
        assert_eq!(exec_cached(&w, &t, 2), serial, "the proposal stored the entry ({threads})");
        w.rig.accept(&b2);
        assert_eq!(exec_cached(&w, &t, 2), serial, "a hit marks the slot erasable ({threads})");

        let w = world(setup());
        w.rig.accept(&b1);
        w.rig.accept(&b2);
        assert!(!exec_cached(&w, &t, 2), "a connect stores nothing ({threads})");
    }
}

/// The store-both's signature half (`v:2752`, `cacheSigStore = fCacheResults`): a valid proposal
/// spending a P2PKH seed stores its signature's entry (`script/sigcache.cpp:93-104`); the
/// block's connect then hits the exec entry the proposal stored too (`v:2174`) and never consults
/// the signature cache, so the entry stays. A plain connect of the same block stores none.
#[test]
fn a_proposal_stores_its_signatures_and_a_connect_does_not() {
    let secret = SecretKey::from_secret_bytes([0x5a; 32]).expect("a key");
    let pubkey = PublicKey::from_secret_key(&secret).serialize();
    // OP_DUP OP_HASH160 <hash160(pubkey)> OP_EQUALVERIFY OP_CHECKSIG
    let script = [&[0x76, 0xa9, 20][..], &hash160(&pubkey), &[0x88, 0xac]].concat();
    let mut raw = Transaction {
        version: 1,
        tx_type: TxType::new(0),
        inputs: vec![input(seed_op(1), Vec::new(), u32::MAX)],
        outputs: vec![out(49 * COIN, &[OP_TRUE])],
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    let sighash = signature_hash(&script, &raw, 0, SIGHASH_ALL);
    let der = secp256k1::ecdsa::sign(Message::from_digest(sighash), &secret).serialize_der();
    let mut script_sig = Vec::new();
    push_data(&mut script_sig, &[der.as_ref(), &[SIGHASH_ALL as u8]].concat());
    push_data(&mut script_sig, &pubkey);
    raw.inputs[0].script_sig = script_sig.into();
    let t = Arc::new(Tx::new(raw));
    let setup = || Setup {
        seeds: vec![seed(1, 50 * COIN, &script)],
        ..Setup::default()
    };
    // The checker's entry: the signature without its hash type
    // (`script/interpreter.cpp:1553-1554`).
    let cached = |w: &World| {
        let caches = caches(w);
        caches.sig.cache().contains(caches.sig.entry(&sighash, &pubkey, der.as_ref()), false)
    };
    let w = world(setup());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let b2 = block_on(&w.rig.p, &b1, vec![t]);
    w.rig.accept(&b1);
    assert!(!cached(&w));
    assert_eq!(propose(&w, &b2, false, true), Ok(BlockState::Valid));
    assert!(cached(&w), "the proposal stored the signature");
    w.rig.accept(&b2);
    assert!(cached(&w), "the connect hit the exec entry: the signature entry stays");

    let w = world(setup());
    w.rig.accept(&b1);
    w.rig.accept(&b2);
    assert!(!cached(&w), "a connect stores nothing");
}

/// CH3 §2.8 never applies to a proposal (§2.14): `-assumevalid` at b4, two headers past b2 on
/// the best header chain, over two weeks of proof-equivalent time with [`AvChain`]'s spacing.
/// b2 spends a seed whose script fails. Proposed, its dummy is no ancestor of b4 (`v:2551-2552`
/// compare pointers), so the script is checked and fails, although b2's header is indexed; on
/// the tip path the same block's scripts are skipped and it connects. With threads the dummy's
/// checks go onto the queue, whose failure is `block-validation-failed` (`v:2778-2781`, through
/// `v:4968`; Opus T23 review n-4).
#[test]
fn assumevalid_never_applies_to_a_proposal() {
    let mut p = (*regtest()).clone();
    p.consensus.pow_target_spacing = 1_000_000;
    p.consensus.pow_target_timespan = 10_000_000;
    let p = Arc::new(p);
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![spend(&[seed_op(1)], COIN)]);
    let b3 = block_on(&p, &b2, vec![]);
    let b4 = block_on(&p, &b3, vec![]);
    for (mode, threads) in super::queue::modes() {
        let w = world(Setup {
            params: Some(p.clone()),
            seeds: vec![seed(1, 50 * COIN, &[0x00])],
            assume_valid: Some(b4.hash()),
            script_threads: threads,
            ..Setup::default()
        });
        w.rig.accept(&b1);
        w.rig.headers(&[b2.clone(), b3.clone(), b4.clone()]);
        eprintln!("{mode}");
        assert_verdict(&verdict(&w, &b2, false, true), &super::queue::script_failed(threads));
        assert_verdict(&last_state(&w, std::slice::from_ref(&b2)), &BlockState::Valid);
    }
}

/// `rpc/mining.cpp:717-720`: a block whose parent is not the tip, a sibling of the tip or one on
/// an unknown parent, is [`NotTested::NotBestPrev`] ("inconclusive-not-best-prevblk") and never
/// reaches the connector: no attempt opens.
#[test]
fn a_block_off_the_tip_is_not_best_prev_and_never_reaches_the_connector() {
    let w = world(Setup::default());
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    let sibling = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, 2 * COIN), vec![]);
    let orphan = Mined {
        block: mine(
            &p,
            BlockHash::from([7; 32]),
            b1.time() + 150,
            vec![coinbase_tx(2, 1, 0, COIN)],
        ),
        height: 2,
    };
    let calls = w.seen.lock().expect("seen").calls.len();
    for m in [&sibling, &w.rig.genesis, &orphan] {
        assert_eq!(propose(&w, m, false, true), Err(NotTested::NotBestPrev));
    }
    assert_eq!(NotTested::NotBestPrev.to_string(), "inconclusive-not-best-prevblk");
    assert_eq!(w.seen.lock().expect("seen").calls.len(), calls, "no attempt opened");
    w.rig.assert_no_abort();
}

/// GBT's proposal mode (`rpc/mining.cpp:707-724`) under one V hold (CH3a D123): an indexed block
/// is `duplicate` when valid up to its scripts, `duplicate-invalid` when failed, and
/// `duplicate-inconclusive` otherwise (`:708-715`), before the parent check (`:717-720`) and
/// with no attempt; an invalidated block and its connected descendant are `duplicate-invalid`.
/// A block not indexed is tested with merkle on. `CreateNewBlock`'s call makes no lookup
/// (`node/miner.cpp:339`).
#[test]
fn a_gbt_proposal_answers_cores_duplicates_first() {
    use NotTested::{Duplicate, DuplicateInconclusive, DuplicateInvalid, NotBestPrev};

    let w = world(Setup::default());
    let p = w.rig.p.clone();
    let gbt = |m: &Mined| w.rig.v.propose_block(m.block.clone()).recv().expect("V replies");
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    let b2 = block_on(&p, &b1, vec![]);
    assert_eq!(gbt(&b2), Ok(BlockState::Valid), "not indexed: tested");
    let bad_root = reheader(&p, &b2, Some(&b1), true);
    let got = gbt(&bad_root).expect("on the tip");
    assert_verdict(&got, &reject(BlockValidationResult::Mutated, "bad-txnmrklroot"));
    assert_eq!(gbt(&reheader(&p, &b2, None, false)), Ok(BlockState::Valid), "PoW off");
    let overpaid = with_coinbase(&p, &b1, coinbase_tx(2, 1, 0, 1_000_000 * COIN), vec![]);
    let state = last_state(&w, std::slice::from_ref(&overpaid));
    assert_verdict(&state, &reject(BlockValidationResult::Unset, "bad-cb-amount"));
    assert!(w.rig.status(&overpaid).has(Status::FAILED_VALID));
    w.rig.headers(std::slice::from_ref(&b2));
    let calls = w.seen.lock().expect("seen").calls.len();
    // Genesis's parent is not the tip, so the lookup comes first. Its `ConnectBlock` returns
    // before `RaiseValidity(BLOCK_VALID_SCRIPTS)` (`v:2536-2539`, `:2900`), as in Core.
    for (m, want) in [
        (&w.rig.genesis, DuplicateInconclusive),
        (&b1, Duplicate),
        (&overpaid, DuplicateInvalid),
        (&b2, DuplicateInconclusive),
    ] {
        assert_eq!(gbt(m), Err(want), "{want}");
    }
    assert_eq!(w.seen.lock().expect("seen").calls.len(), calls, "no attempt opened");
    assert_eq!(propose(&w, &w.rig.genesis, false, true), Err(NotBestPrev), "no lookup");
    assert_eq!(propose(&w, &b2, false, false), Ok(BlockState::Valid), "no lookup");
    // `IsValid` fails any failed block (`chain.h:305-306`): with b2 connected, `invalidateblock`
    // disconnects b2, then b1, and leaves both `FAILED_VALID`. Its flip of a child to
    // `FAILED_CHILD` (`v:3983-3988`) compares the walk tip's parent with the block disconnected
    // before it, which lies above it, so it never holds here; the port keeps it verbatim
    // (CH1-node §4.6). b2 stays valid up to its scripts.
    w.rig.accept(&b2);
    let side = with_coinbase(&p, &b1, coinbase_tx(2, 1, 0, 2 * COIN), vec![]);
    assert_ne!(side.hash(), b2.hash());
    w.rig.headers(std::slice::from_ref(&side));
    assert_eq!(w.rig.v.invalidate_block(b1.hash()).recv(), Ok(Ok(())));
    assert!(w.rig.status(&b1).has(Status::FAILED_VALID), "{:?}", w.rig.status(&b1));
    let status = w.rig.status(&b2);
    assert!(status.has(Status::FAILED_VALID) && !status.has(Status::FAILED_CHILD), "{status:?}");
    assert!(status.bits() & Status::VALID_MASK >= Status::VALID_SCRIPTS, "{status:?}");
    for m in [&b1, &b2] {
        assert_eq!(gbt(m), Err(DuplicateInvalid), "{}", m.height);
    }
    // A status that is only `FAILED_CHILD` is failed too (`BLOCK_FAILED_MASK`,
    // `rpc/mining.cpp:712-713`; Opus T23 review n-1). `side`, b2's header-only sibling, was no
    // walk tip, so it is unmarked until a header on it finds b1 in `m_failed_blocks`, which
    // marks it `FAILED_CHILD` and rejects the header as `bad-prevblk` (`v:4678-4688`).
    assert_eq!(gbt(&side), Err(DuplicateInconclusive), "not marked yet");
    let on_side = block_on(&p, &side, vec![]);
    let r = w.rig.v.process_headers(batch(&[on_side]), None).recv().expect("V replies");
    match &r.stop {
        Some(HeaderStop::Invalid {
            reject,
            ..
        }) => assert_eq!(reject.reject_reason(), "bad-prevblk"),
        stop => panic!("{stop:?}"),
    }
    let status = w.rig.status(&side);
    assert!(status.has(Status::FAILED_CHILD) && !status.has(Status::FAILED_VALID), "{status:?}");
    assert_eq!(gbt(&side), Err(DuplicateInvalid), "FAILED_CHILD only");
    let shown = [Duplicate, DuplicateInvalid, DuplicateInconclusive].map(|n| n.to_string());
    assert_eq!(shown, ["duplicate", "duplicate-invalid", "duplicate-inconclusive"]);
    w.rig.assert_no_abort();
}

/// CH3 §2.14's digest scope as a6 can observe it (CH3a D125): the helper's scope opens at step
/// 4. A proposal rejected at step 1 (the ChainLock check) asks the helper nothing and ends no
/// helper scope (step 3's case is [`each_pre_check_rejects_with_cores_class_and_line`]); one
/// that reaches step 4 asks it and ends its scope once. The `R` and `A` records are a7's tap.
#[test]
fn only_a_proposal_reaching_connect_block_opens_the_helpers_scope() {
    let w = world(Setup {
        locked_at: Some(1),
        ..Setup::default()
    });
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let asked = queries(&w);
    let got = verdict(&w, &b1, false, true);
    assert_verdict(&got, &reject(BlockValidationResult::InvalidPrev, "bad-chainlock"));
    assert_eq!(queries(&w), asked, "step 1: the helper was asked");
    assert_eq!(w.seen.lock().expect("seen").last_attempt(), CL_REJECTED);

    let w = world(Setup::default());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let asked = queries(&w);
    assert_eq!(verdict(&w, &b1, false, true), BlockState::Valid);
    assert!(queries(&w) > asked, "step 4 asks the helper");
    let seen = w.seen.lock().expect("seen");
    let ends = seen.last_attempt().iter().filter(|c| **c == Call::EndAttempt).count();
    assert_eq!(ends, 1, "{:?}", seen.last_attempt());
}

/// [`propose`] of a block whose proposal stops V: no reply.
fn propose_stopping(w: &World, m: &Mined, label: &str) {
    let r = w.rig.v.test_block_validity(m.block.clone(), false, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected), "{label}");
}

/// CH3 §3.2 and D75 in a proposal: a failed `begin_tx` is the attempt's `Fatal`; the attempt
/// still finishes and the evo transaction rolls back, before `ConnectBlock` (no helper scope).
/// A held coins cell is a `Fatal` before the evo transaction begins, so none rolls back. Each
/// stops V with one report, and neither unwinds.
#[test]
fn a_proposals_storage_fault_stops_v_inside_the_bracket() {
    let mut w = world(Setup {
        steps: StepsCfg {
            fail_tx: Some(TxStep::Begin),
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let evo_best = w.seen.lock().expect("seen").evo_best;
    propose_stopping(&w, &b1, "begin_tx");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(TX_FAILED));
    assert_eq!(w.sink.messages(), [TX_FAILED], "one report");
    assert_eq!(w.seen.lock().expect("seen").last_attempt(), PRE_CHECKED);
    assert_eq!(w.seen.lock().expect("seen").evo_best, evo_best);
    assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()));

    let mut w = world(Setup::default());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let held = w.coins.lock().expect("coins");
    propose_stopping(&w, &b1, "held");
    drop(held);
    let exit = w.rig.stop();
    let msg = "the coins cell is held outside V's leaf rule";
    assert_eq!(fatal_text(&exit), Some(msg));
    assert_eq!(w.sink.messages(), [msg], "one report");
    assert_eq!(w.seen.lock().expect("seen").last_attempt(), CL_REJECTED);
    let latch = w.seen.lock().expect("seen").latch.clone();
    let text = latch.tripped().map(|f| f.message.clone()).unwrap_or_default();
    assert!(!text.contains("unwound"), "{text}");
}

/// D71 in a proposal: a hook that panics inside the just-check `ConnectBlock` unwinds through
/// the bracket, and `AttemptUnwind` trips the latch before the attempt drops; the panic reaches
/// V's joiner and nothing commits.
#[test]
fn a_hook_panicking_in_a_proposal_trips_the_latch_before_the_attempt_drops() {
    let (cfg, _stores, _sink, seen) = node(Setup {
        steps: StepsCfg {
            panic_in_h4: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let mut rig = Rig::with(cfg);
    let b1 = block_on(&rig.p, &rig.genesis, vec![]);
    let r = rig.v.test_block_validity(b1.block.clone(), false, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic reaches V's joiner");
    let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(seen.dropped_latched.last(), Some(&true), "latched before the attempt dropped");
    assert_eq!(seen.last_attempt(), [Call::BeginAttempt, Call::BeginTx]);
    let f = seen.latch.tripped().expect("tripped");
    assert_eq!(
        (f.site, f.message.as_str()),
        ("chainstate validation", "an EV attempt unwound before its commit or rollback returned")
    );
}

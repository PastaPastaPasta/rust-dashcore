//! EV1-J J0b-1's regtest quorum fixture (plan rev4 §5.3): a producer chain whose non-null
//! quorum commitments the producer dashd itself validates and mines, and the probes of the J0b-1
//! rows on it ([`probes`]).
//!
//! The chain is [`crate::CHAIN_ARGS`]' (V24 active before 200, DIP0024's rotating types out at
//! 1000; the chain stays below 600, so `withdrawals` too). The producer mines 200 blocks,
//! registers three evonodes in block 201 with operator keys from its own `bls generate` (kept),
//! and no regular masternode, so `llmqTypeMnhf` (LLMQ_TEST) and `llmqTypePlatform`
//! (LLMQ_TEST_PLATFORM), both of size 3 (`llmq/params.h:147-163`, `:251-267`), draw the same
//! three. Block 202 funds the credit pool with an asset lock. Sporks stay off: no ChainLock is
//! ever signed, so no CbTx carries one.
//!
//! The quorums ([`QUORUMS`]): one MNHF quorum, then four Platform quorums in the next four DKG
//! cycles (interval 24), each in a cycle of its own so that no base block is a quorum of both
//! types. Each commitment's members are V's own (`ChainQuery::bind(..).members(type, base,
//! false)` on a V the producer's blocks are fed to as they are mined), all valid and signing,
//! with a quorum key of the fixture's ([`sign::commitment`]). Core's own route mines it: the
//! producer, at its base's ninth block after the base, gets it from a P2P peer
//! ([`p2p::give_commitment`]) and verifies it (`llmq/blockprocessor.cpp:217`), and its next
//! template, the first of the mining window (`:311-323`), carries it in place of the null
//! commitment; the test checks that the mined block has the commitment's bytes.
//!
//! Then a stale block (a sibling of setup block [`STALE_HEIGHT`], which both validators are
//! given and keep off their active chain), an asset unlock mined from the producer's mempool
//! under the newest Platform quorum (its index [`MINED_INDEX`] is in the credit pool from then
//! on), and blocks up to [`TIP`]. At the tip the scan (`signingActiveQuorumCount + 1` = 3,
//! `evo/assetlocktx.cpp:120-121`) holds the three newest Platform quorums: the oldest has aged
//! out, the third was mined at or before `TIP - 48`, and the newest after it (the 9-R rows'
//! first and last heights, `validation.cpp:822-831`).

pub mod p2p;
pub mod probes;
pub mod sign;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use dash_consensus::hash::sha256d;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{Header, Transaction, TxOut, core_serialize};
use dash_consensus::{Block, Tx};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash, TxType};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ChainQuery, OnV};
use dashd_mining::{Grind, grind};
use dashd_node::sut::NodeSut;
use serde_json::{Value, json};

use crate::v24::{self, Dashd, hex};
use crate::{CHAIN_ARGS, COIN, Checked, block_of, copy_dir, fresh_sut, rpc, unhex, unlock_with};
use sign::SecretKey;

/// The producer's arguments: [`CHAIN_ARGS`], its wallet writing without waiting for the disk
/// (`wallet/init.cpp:136`), and the LLMQ log, which names the reason of a refused commitment.
const PRODUCER_ARGS: [&str; 7] = [
    CHAIN_ARGS[0],
    CHAIN_ARGS[1],
    CHAIN_ARGS[2],
    CHAIN_ARGS[3],
    CHAIN_ARGS[4],
    "-unsafesqlitesync=1",
    "-debug=llmq",
];
/// The evonodes, each with 4000 DASH of collateral.
const EVONODES: usize = 3;
/// The asset lock funding the credit pool in block 202.
const LOCKED: i64 = 100 * COIN;
/// Both types' quorum size, and the first block of their mining window from the base
/// (`llmq/params.h:149`, `:155`, `:253`, `:259`); their DKG interval is 24.
const QUORUM_SIZE: usize = 3;
const MINING_WINDOW_START: u32 = 10;
/// The fixture's quorums, by base height: the MNHF quorum's, then the Platform quorums', oldest
/// first. Block 201 registers the evonodes; the first base is 15 blocks later, past the work
/// block 8 blocks before a base and the one confirmation a member needs.
const QUORUMS: [(LlmqType, u32); 5] = [
    (LlmqType::LlmqTest, 216),
    (LlmqType::LlmqTestPlatform, 240),
    (LlmqType::LlmqTestPlatform, 264),
    (LlmqType::LlmqTestPlatform, 288),
    (LlmqType::LlmqTestPlatform, 312),
];
/// [`QuorumChain::quorums`]' indexes.
pub const MNHF: usize = 0;
pub const AGED_OUT: usize = 1;
/// Mined at or before `TIP - 48`: scanned at a v2 unlock's first height `TIP - 48`.
pub const FIRST_HEIGHT_QUORUM: usize = 3;
/// Mined after `TIP - 48`: scanned at that unlock's last height, not at its first.
pub const NEWEST: usize = 4;
/// The key material of each quorum's key, by index into [`QUORUMS`].
const QUORUM_SEEDS: [u8; 5] = [0x70, 0x71, 0x72, 0x73, 0x74];
/// The height whose block has a stale sibling.
const STALE_HEIGHT: usize = 230;
/// The height mining the setup's asset unlock, and its index.
const UNLOCK_HEIGHT: usize = 330;
pub const MINED_INDEX: u64 = 1;
/// The setup tip: 48 or more past the third Platform quorum's mining (298), less than 48 past
/// the fourth's (322), and outside every mining window (one closes at 354, the next opens at
/// 370).
pub const TIP: u32 = 358;
/// A withdrawal's outputs and fee: above dust, and a fee over the minimum relay fee, so that a
/// valid one enters the producer's mempool.
const UNLOCK_OUTPUT: i64 = 100_000;
const UNLOCK_FEE: u32 = 10_000;

/// A constructed quorum: its base block (internal bytes), its mined height, and its key.
pub struct Quorum {
    pub hash: [u8; 32],
    pub mined: u32,
    pub key: SecretKey,
}

/// The quorum chain and what the probes need from it.
pub struct QuorumChain {
    /// Blocks 1 to [`TIP`].
    pub setup: Vec<Arc<Block>>,
    /// The stale sibling of setup block [`STALE_HEIGHT`].
    pub stale: Vec<Arc<Block>>,
    /// The time every validator runs at, past every block the producer made.
    pub time: i64,
    /// An unsubmitted template on the setup tip (`generateblock` with no transactions).
    pub template: Block,
    /// [`QUORUMS`]' quorums, in order.
    pub quorums: Vec<Quorum>,
    pub p2pkh: Vec<u8>,
    /// The stopped producer's datadir, which an accept probe's producer copies, and its mining
    /// address.
    producer: PathBuf,
    address: Value,
}

/// An asset unlock's payload fields (`assetlocktx.h:109-119`) and output count, its fee
/// [`UNLOCK_FEE`] and each output [`UNLOCK_OUTPUT`] to the fixture's P2PKH script.
#[derive(Clone, Copy, Debug)]
pub struct Unlock {
    pub version: u8,
    pub index: u64,
    pub outputs: usize,
    pub requested: u32,
    pub quorum: [u8; 32],
}

impl Unlock {
    pub fn with_sig(&self, sig: &[u8; 96], p2pkh: &[u8]) -> Arc<Tx> {
        unlock_with(
            self.version,
            self.index,
            UNLOCK_FEE,
            UNLOCK_OUTPUT,
            self.outputs,
            self.requested,
            &self.quorum,
            sig,
            p2pkh,
        )
    }

    /// The hash a Platform quorum signs for it: `SignHash(llmqTypePlatform, quorumHash,
    /// requestId, msgHash)`, the message the whole serialization with a null signature
    /// (`evo/assetlocktx.cpp:218-226`, version 2 included) and the request id
    /// `SerializeHash(("plwdtx", index))` (`:141`).
    pub fn sign_hash(&self, p2pkh: &[u8]) -> [u8; 32] {
        let msg = sha256d(&core_serialize(&*self.with_sig(&[0; 96], p2pkh)));
        let id = sha256d(&[&[6][..], b"plwdtx", &self.index.to_le_bytes()].concat());
        let quorum = QuorumHash::from(self.quorum);
        dash_evo::locks::sign_hash(LlmqType::LlmqTestPlatform, &quorum, &id, &msg)
    }

    /// Signed by `key`.
    pub fn signed(&self, key: &SecretKey, p2pkh: &[u8]) -> Arc<Tx> {
        self.with_sig(&sign::sign(key, &self.sign_hash(p2pkh)), p2pkh)
    }
}

/// A type 7 signal's fields (`evo/mnhftx.h`: version 1, the bit, the quorum hash).
#[derive(Clone, Copy, Debug)]
pub struct Signal {
    pub bit: u8,
    pub quorum: [u8; 32],
}

impl Signal {
    pub fn with_sig(&self, sig: &[u8; 96]) -> Arc<Tx> {
        crate::mnhf(self.bit, &self.quorum, sig)
    }

    /// The hash an MNHF quorum signs for it: `SignHash(llmqTypeMnhf, quorumHash, requestId,
    /// msgHash)`, the message the txid with a null signature and the request id
    /// `SerializeHash(("mnhf", int64 bit))` (`evo/mnhftx.cpp:33-35`, `:139-153`).
    pub fn sign_hash(&self) -> [u8; 32] {
        let msg = *self.with_sig(&[0; 96]).txid().as_bytes();
        let id = sha256d(&[&[4][..], b"mnhf", &i64::from(self.bit).to_le_bytes()].concat());
        let quorum = QuorumHash::from(self.quorum);
        dash_evo::locks::sign_hash(LlmqType::LlmqTest, &quorum, &id, &msg)
    }

    /// Signed by `key`.
    pub fn signed(&self, key: &SecretKey) -> Arc<Tx> {
        self.with_sig(&sign::sign(key, &self.sign_hash()))
    }
}

/// A version 1 asset lock (type 8) of `value` to `p2pkh` (`assetlocktx.h`: the payload's version
/// and credit outputs; the locked amount on an `OP_RETURN` output with an empty push,
/// `evo/assetlocktx.cpp:39-46`), unfunded.
fn asset_lock(value: i64, p2pkh: &[u8]) -> Tx {
    let mut payload = vec![1u8, 1];
    payload.extend(value.to_le_bytes());
    payload.push(u8::try_from(p2pkh.len()).unwrap());
    payload.extend(p2pkh);
    Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(8),
        inputs: Vec::new(),
        outputs: vec![TxOut {
            value,
            script_pubkey: vec![0x6a, 0x00].into(),
        }],
        lock_time: 0,
        extra_payload: payload,
    })
}

/// The producer at work: its RPC, the V its blocks are fed to, its clock and its blocks.
struct Producing {
    p: Dashd,
    builder: (NodeSut, Arc<Checked>),
    time: Cell<i64>,
    address: Value,
    setup: Vec<Arc<Block>>,
}

impl Producing {
    fn tick(&self) {
        self.time.set(self.time.get() + 150);
        rpc(&self.p, "setmocktime", json!([self.time.get()]));
        self.builder.0.clock().set_mocktime(self.time.get());
    }

    fn new_address(&self) -> Value {
        rpc(&self.p, "getnewaddress", json!([]))
    }

    /// Mines to `height`, each block fed to the builder V, which must accept it.
    fn mine_to(&mut self, height: usize) {
        while self.setup.len() < height {
            self.tick();
            let hash = rpc(&self.p, "generatetoaddress", json!([1, self.address]))[0].clone();
            let block = Arc::new(block_of(&rpc(&self.p, "getblock", json!([hash, 0]))));
            let chain = self.builder.0.chain().expect("V runs");
            let r = chain.process_block(Arc::clone(&block), true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "V on block {}: {r:?}", self.setup.len() + 1);
            self.setup.push(block);
        }
    }

    /// V's members of quorum `(llmq_type, base)`, as ProTx hashes: Core's
    /// `GetAllQuorumMembers`, through route B's binding.
    fn members(&self, llmq_type: LlmqType, base: BlockHash) -> Vec<String> {
        let chain = self.builder.0.chain().expect("V runs");
        let reply = chain.on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
            let id = q.chain.reader().lookup(&base).expect("V has the base block");
            let members = q.bind(o).members(llmq_type, id, false).map_err(|e| e.to_string())?;
            Ok::<_, String>(members.iter().map(|m| m.pro_tx_hash.to_string()).collect())
        });
        reply.recv().expect("V serves").expect("V's members")
    }

    /// Mines quorum `(llmq_type, base)` with key `key` through Core's route (module docs).
    fn quorum(
        &mut self,
        llmq_type: LlmqType,
        base: u32,
        key: SecretKey,
        operators: &[(String, SecretKey)],
        log: &Path,
    ) -> Quorum {
        self.mine_to((base + MINING_WINDOW_START - 1) as usize);
        let base_hash = self.setup[base as usize - 1].hash();
        let members = self.members(llmq_type, base_hash);
        let keys: Vec<&SecretKey> = members
            .iter()
            .map(|m| {
                let found = operators.iter().find(|(pro_tx, _)| pro_tx == m);
                &found.unwrap_or_else(|| panic!("member {m} is no evonode of the fixture")).1
            })
            .collect();
        let qc =
            sign::commitment(llmq_type.to_base(), base_hash.as_bytes(), QUORUM_SIZE, &keys, &key);
        p2p::give_commitment(self.p.p2p_port(), self.time.get(), &qc);
        self.mine_to((base + MINING_WINDOW_START) as usize);
        let carried = self.setup.last().unwrap().txs().iter().any(|tx| {
            tx.raw().tx_type == TxType::new(6) && tx.raw().extra_payload.get(6..) == Some(&qc[..])
        });
        if !carried {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let lines: Vec<&str> =
                text.lines().filter(|l| l.contains("CFinalCommitment")).collect();
            panic!(
                "block {} lacks the {llmq_type:?} commitment of base {base} (members {members:?}): \
                 {lines:#?}",
                self.setup.len()
            );
        }
        eprintln!(
            "quorum chain: {llmq_type:?} at {base} mined at {}, members {members:?}",
            self.setup.len()
        );
        Quorum {
            hash: *base_hash.as_bytes(),
            mined: base + MINING_WINDOW_START,
            key,
        }
    }
}

/// Builds the quorum chain on a producer in `dir` (see the module docs).
pub fn produce(binary: &Path, dir: &Path) -> QuorumChain {
    let started = Instant::now();
    let producer_dir = dir.join("producer");
    let log = producer_dir.join("regtest/debug.log");
    let p = Dashd::start_listening(binary, &producer_dir, v24::T0, &PRODUCER_ARGS);
    rpc(&p, "createwallet", json!(["producer"]));
    let address = rpc(&p, "getnewaddress", json!([]));
    let mut w = Producing {
        p,
        builder: fresh_sut(&dir.join("builder"), v24::T0, &[], &[]),
        time: Cell::new(v24::T0),
        address,
        setup: Vec::new(),
    };
    w.mine_to(200);

    // Three evonodes, registered and funded by the producer's wallet in block 201.
    let mut operators: Vec<(String, SecretKey)> = Vec::new();
    for i in 0..EVONODES {
        let operator = rpc(&w.p, "bls", json!(["generate"]));
        let key = sign::operator_key(
            operator["secret"].as_str().unwrap(),
            operator["public"].as_str().unwrap(),
        );
        let pro_tx = rpc(
            &w.p,
            "protx",
            json!([
                "register_fund_evo",
                w.new_address(),
                format!("127.0.0.1:{}", 20001 + i),
                w.new_address(),
                operator["public"],
                w.new_address(),
                0,
                w.new_address(),
                hex(&[u8::try_from(i + 1).unwrap(); 20]),
                22301 + i,
                22401 + i,
                w.address
            ]),
        );
        operators.push((pro_tx.as_str().unwrap().to_owned(), key));
    }
    w.mine_to(201);
    assert_eq!(rpc(&w.p, "getrawmempool", json!([])), json!([]), "block 201 mined the ProRegs");

    // The credit pool's funding, in block 202.
    let p2pkh = unhex(
        rpc(&w.p, "validateaddress", json!([w.new_address()]))["scriptPubKey"].as_str().unwrap(),
    );
    let lock = hex(&core_serialize(&asset_lock(LOCKED, &p2pkh)));
    let funded = rpc(&w.p, "fundrawtransaction", json!([lock, {"feeRate": 0.0001}]));
    let signed = rpc(&w.p, "signrawtransactionwithwallet", json!([funded["hex"]]));
    assert_eq!(signed["complete"], json!(true));
    rpc(&w.p, "sendrawtransaction", json!([signed["hex"]]));
    w.mine_to(202);
    assert_eq!(rpc(&w.p, "getrawmempool", json!([])), json!([]), "block 202 mined the lock");
    eprintln!("quorum chain: evonodes and the credit pool at {:?}", started.elapsed());

    let mut quorums = Vec::new();
    let mut stale = Vec::new();
    for ((llmq_type, base), seed) in QUORUMS.into_iter().zip(QUORUM_SEEDS) {
        if stale.is_empty() && STALE_HEIGHT < base as usize {
            // A block at STALE_HEIGHT that the producer never connects: a template paying another
            // address, then the setup block at that height in its place.
            w.mine_to(STALE_HEIGHT - 1);
            w.tick();
            let other = rpc(&w.p, "generateblock", json!([w.new_address(), [], false]));
            stale.push(Arc::new(block_of(&other["hex"])));
        }
        let quorum = w.quorum(llmq_type, base, sign::key(seed), &operators, &log);
        quorums.push(quorum);
        eprintln!("quorum chain: base {base} at {:?}", started.elapsed());
    }
    let (first, newest) = (&quorums[FIRST_HEIGHT_QUORUM], &quorums[NEWEST]);
    assert!(first.mined <= TIP - 48 && TIP - 48 < newest.mined && newest.mined < TIP);
    assert_eq!(stale[0].header().prev_hash(), w.setup[STALE_HEIGHT - 2].hash(), "a sibling");

    // A mined unlock under the newest Platform quorum: MINED_INDEX is in the credit pool after.
    w.mine_to(UNLOCK_HEIGHT - 1);
    let unlock = Unlock {
        version: 1,
        index: MINED_INDEX,
        outputs: 1,
        requested: (UNLOCK_HEIGHT - 1) as u32,
        quorum: quorums[NEWEST].hash,
    }
    .signed(&quorums[NEWEST].key, &p2pkh);
    rpc(&w.p, "sendrawtransaction", json!([hex(&core_serialize(&*unlock))]));
    w.mine_to(UNLOCK_HEIGHT);
    let mined = w.setup.last().unwrap();
    assert!(mined.txs().iter().any(|tx| tx.txid() == unlock.txid()), "the unlock mined");
    w.mine_to(TIP as usize);

    w.tick();
    let template = block_of(&rpc(&w.p, "generateblock", json!([w.address, [], false]))["hex"]);
    assert_eq!(rpc(&w.p, "getrawmempool", json!([])), json!([]), "setup mined every tx");
    eprintln!("quorum chain: built at {:?}", started.elapsed());
    let Producing {
        p,
        time,
        address,
        setup,
        ..
    } = w;
    drop(p);
    QuorumChain {
        setup,
        stale,
        time: time.get(),
        template,
        quorums,
        p2pkh,
        producer: producer_dir,
        address,
    }
}

impl QuorumChain {
    /// A block on the setup tip carrying `tx`, as the producer's miner builds it from its
    /// mempool, never submitted: on a copy of the stopped producer's datadir (so that no probe
    /// reaches the next), `tx` through `sendrawtransaction`, then the copy's `getblocktemplate`
    /// with its CbTx payload (credit pool and roots), its coinbase value (the unlock's fee
    /// included) and its masternode and Platform outputs ([`assemble`]), ground on `pow`.
    /// `generateblock` cannot build it: its template reads no mempool
    /// (`BlockAssembler(active_chainstate, node, nullptr)`), and a raw transaction it is given is
    /// appended to that template, CbTx and coinbase unchanged (`rpc/mining.cpp` generateblock).
    /// So that the assembly is Core's, the copy's template before `tx` assembles to exactly
    /// the coinbase of its `generateblock` with no transactions (submit false), whose header the
    /// block keeps.
    pub fn mined_with(&self, binary: &Path, scratch: &Path, tx: &Tx, pow: &PowParams) -> Block {
        let dir = tempfile::tempdir_in(scratch).unwrap();
        copy_dir(&self.producer, dir.path());
        let p = Dashd::start_at(binary, dir.path(), self.time, &CHAIN_ARGS);
        let empty = block_of(&rpc(&p, "generateblock", json!([self.address, [], false]))["hex"]);
        let before = assemble(&empty, &rpc(&p, "getblocktemplate", json!([])), pow);
        assert_eq!(
            core_serialize(&*before.txs()[0]),
            core_serialize(&*empty.txs()[0]),
            "the assembly is generateblock's coinbase"
        );
        assert_eq!(before.txs().len(), 1, "an empty mempool");
        rpc(&p, "sendrawtransaction", json!([hex(&core_serialize(tx))]));
        // The first template is kept until the mempool changed and more than 5 s passed on the
        // mocked clock (`rpc/mining.cpp:812-813`); the block's header is `empty`'s.
        rpc(&p, "setmocktime", json!([self.time + 6]));
        let block = assemble(&empty, &rpc(&p, "getblocktemplate", json!([])), pow);
        assert_eq!(block.header().prev_hash(), self.setup.last().unwrap().hash(), "on the tip");
        let txids: Vec<_> = block.txs()[1..].iter().map(|t| t.txid()).collect();
        assert!(txids.contains(&tx.txid()), "the template mines the tx");
        block
    }
}

/// The block of template `gbt` (`getblocktemplate`'s result) on `empty`'s header and coinbase:
/// the CbTx payload `coinbase_payload`, the masternode and Platform outputs `masternode` in order,
/// and the miner's output the rest of `coinbasevalue`; then the template's transactions; ground
/// on `pow`.
fn assemble(empty: &Block, gbt: &Value, pow: &PowParams) -> Block {
    assert_eq!(gbt["superblock"], json!([]), "no superblock");
    assert_eq!(gbt["version"], json!(empty.header().raw().version), "the template's version");
    let mut coinbase = empty.txs()[0].to_mutable();
    coinbase.extra_payload = unhex(gbt["coinbase_payload"].as_str().unwrap());
    let payees = gbt["masternode"].as_array().unwrap();
    assert_eq!(coinbase.outputs.len(), 1 + payees.len(), "the miner, then each payee");
    let mut rest = gbt["coinbasevalue"].as_i64().unwrap();
    for (out, payee) in coinbase.outputs[1..].iter_mut().zip(payees) {
        let script = unhex(payee["script"].as_str().unwrap());
        assert_eq!(out.script_pubkey.as_bytes(), &script[..], "a payee's script");
        out.value = payee["amount"].as_i64().unwrap();
        rest -= out.value;
    }
    coinbase.outputs[0].value = rest;
    let mut txs = vec![Arc::new(Tx::new(coinbase))];
    for t in gbt["transactions"].as_array().unwrap() {
        txs.push(Arc::new(crate::tx_of(&t["data"])));
    }
    let mut block = Block::new(Header::new(*empty.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    block
}

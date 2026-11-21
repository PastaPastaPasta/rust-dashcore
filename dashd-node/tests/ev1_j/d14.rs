//! EV1-J J0b-3's devnet fixture (the design accepted as OD-CP-74): 34 llmq_60_75 Platform
//! quorums on the devnet [`NAME`] with `-llmqplatform=llmq_60_75`, the only way to reach
//! `signingActiveQuorumCount + 1 = 33` (`llmq/params.h:394`, `evo/assetlocktx.cpp:122`): no regtest
//! type scans more than 3, and mainnet's and testnet's Platform types scan 25. What it shows is
//! for this devnet only.
//!
//! The chain ([`DEVNET`]). A devnet activates DIP0003, DIP0024, V19, V20, MN_RR and withdrawals at
//! height 2 (`chainparams.cpp:574-591`) and keeps V24 out (EHF-gated with a start time after the
//! clock, `:607-614`), so its CbTx is version 3 and its unlocks version 1. Block 1 is the devnet
//! genesis block, which every node makes itself (`:648-649`); the producer mines from 2, below
//! `-minimumdifficultyblocks` at the proof-of-work limit (`pow.cpp:177-179`) and, below
//! `-highsubsidyblocks`, at 100 times the subsidy. Sporks stay off. A devnet's clock is not
//! mockable (`setmocktime` is refused, `rpc/node.cpp:263`), so every node of the fixture runs at
//! `-mocktime` [`TIME`] from its start; block times then follow the median time past.
//!
//! Masternodes. Block 270 funds the credit pool with an asset lock; block 271 registers 60
//! regular masternodes (`protx register_fund`, operator keys from the producer's `bls generate`,
//! one routable address each: a devnet refuses an address that is not, `evo/netinfo.cpp:294`).
//! Core's rotation takes no evonode filter (`llmq/utils.cpp:405-406`), and a rotated quorum is
//! its previous three cycles' quarters and a new quarter of 15 (`:343-545`), so with the
//! masternodes in cycle 1's list (work block 280) the 4th cycle, 1152, is the first whose
//! quorums have 60 members, and 60 masternodes fill its new quarters.
//!
//! The quorums. Cycles 288, 576 and 864 mine null commitments only. A (cycle 1152): A0 (base
//! 1152, quorum index 0) and A1 (base 1153, index 1), given to the producer at 1193 through
//! [`p2p::give_commitments`] (the J0b-1 route: `inv`, `getdata`, `qfcommit`, `ping`) and
//! mined in 1194, the window's first block. B (cycle 1440): B0 to B31 (bases 1440 to 1471),
//! given at 1481 and mined in 1482. Each commitment is version 4 ([`sign::indexed_commitment`])
//! with V's own members (`OnVBinding::members`, which runs V's rotation), each signing with its
//! operator key; Core then checks them against its own members, so a member-set divergence
//! fails here, with Core's log line.
//!
//! The members' order, for rotated quorums only (J-D9 closes for rotation, not for others), rests
//! on two checks. First, the full order: for each of the 34 quorums, V's ordered members (the
//! vector its commitment's operator keys and signing come from) equal, position by position,
//! Core's ordered members from `quorum info` (`members[i]` in index order,
//! `rpc/quorums.cpp:214-239`, `BuildQuorumInfo`; `GetAllQuorumMembers`'s list,
//! `llmq/quorumsman.cpp:80-83`). Second, Core
//! binds the member set to the signature (OD-CP-74 q4): B_i marks the two members at positions
//! `2i mod 60` and `2i + 1 mod 60` invalid, neither signing (58 valid, at least 50 needed,
//! `llmq/commitment.cpp:153-158`), and Core takes B_i only if the members at those positions are,
//! as a set, V's (`membersSig` aggregates the operator keys of the members whose `signers` bit is
//! set, in Core's order, `:76-95`). Its twin control: B0 with the two invalid bits moved to two
//! other positions and the same signers, re-signed over its own commitment hash and given first,
//! which Core refuses (`invalid aggregated members signature`). The aggregate does not see the
//! order inside a set, so the second check alone passes a V that swaps the members of each pair
//! `(2k, 2k + 1)`; the first does not. The negative self-test of the version 4
//! commitment ([`sign::indexed_commitment`]): A0 with a wrong quorum index, and A0 with 49 valid
//! members, both given before A0, both refused by Core (`llmq/blockprocessor.cpp:156-163`,
//! `llmq/commitment.cpp:153-156`). B is mined after every member selection the fixture uses, so
//! the PoSe penalties of B's invalid members change no quorum it reads.
//!
//! The scan at the tip [`TIP`] (1491, past B's window end 1490, `llmq/quorumsman.cpp:203-205`)
//! takes index rounds (`llmq/blockprocessor.cpp:735-815`): `[B0..B31, A0, A1]`. The 33rd is
//! A0 and the 34th A1, although A1 is newer than A0 by height. At [`BEFORE_B`] (1480, before B's
//! window, `quorumsman.cpp:197-201`) the scan's store block is 1202, A's window end, and holds
//! `[A0, A1]`.
//!
//! Images for J1, for the test process only: the producer's datadir at 1480 and at the tip, in
//! the caller's directory, and V's ([`NodeSut::save_image`]) at both, in memory. Nothing is
//! persisted; J1 calls [`produce_d14`] in its own run.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use dash_consensus::pow::PowParams;
use dash_consensus::primitives::core_serialize;
use dash_consensus::{Block, Tx};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, TxType};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ChainQuery, OnV};
use dashd_node::sut::{NodeSut, SutImage};
use serde_json::{Value, json};

use crate::ev1_j::harness::{
    COIN, Chain, Checked, block_of, copy_dir, fresh_sut_on, pinned, rpc, unhex,
};
use crate::ev1_j::p2p::{self, PeerChain};
use crate::ev1_j::sign::{self, SecretKey};
use crate::ev1_j::{Unlock, assemble, asset_lock};
use crate::v24::{self, Dashd, Net, hex};

/// The devnet's name.
pub const NAME: &str = "ev1jd14";
/// The chain parameters every node of the fixture runs with (OD-CP-74).
pub const ARGS: [&str; 4] = [
    "-llmqplatform=llmq_60_75",
    "-minimumdifficultyblocks=2000",
    "-highsubsidyblocks=300",
    "-highsubsidyfactor=100",
];
/// The devnet. V runs without listening: a devnet node that listens needs `-port`
/// (`init.cpp:1263-1266`).
pub const DEVNET: Chain = Chain {
    net: Net::Devnet(NAME),
    sut: &["-devnet=ev1jd14", "-listen=0"],
    args: &ARGS,
    first_height: 2,
};
/// The producer's: [`ARGS`], its wallet writing without waiting for the disk
/// (`wallet/init.cpp:136`), the LLMQ log, which names the reason of a refused commitment, and
/// its test peer unpunished (`net_processing.cpp:5859`), so a refused commitment does not end
/// the connection before its `pong`.
const PRODUCER_ARGS: [&str; 7] = [
    ARGS[0],
    ARGS[1],
    ARGS[2],
    ARGS[3],
    "-unsafesqlitesync=1",
    "-debug=llmq",
    "-whitelist=noban@127.0.0.1",
];
/// A peer of the devnet: its message start (`chainparams.cpp`, CDevNetParams `pchMessageStart`)
/// and a user agent naming it as `devnet.<devnet name>`, where the devnet name carries a `devnet-`
/// prefix (`net_processing.cpp:4037-4046`; otherwise Core logs "connected to wrong devnet" and
/// drops the peer).
pub const PEER: PeerChain = PeerChain {
    magic: [0xe2, 0xca, 0xff, 0xce],
    user_agent: "/ev1-j:0.1(devnet.devnet-ev1jd14)/",
};
/// Every node's clock (module docs).
pub const TIME: i64 = v24::T0;
/// The Platform type, and its size, DKG interval and mining window from the cycle start
/// (`llmq/params.h:380-396`).
pub const PLATFORM: LlmqType = LlmqType::Llmq60_75;
const SIZE: usize = 60;
const INTERVAL: u32 = 288;
pub const WINDOW_START: u32 = 42;
/// The cycles of A and B.
pub const A_CYCLE: u32 = 4 * INTERVAL;
pub const B_CYCLE: u32 = 5 * INTERVAL;
/// A's quorum indexes.
const A_INDEXES: [u32; 2] = [0, 1];
/// B's: every index of llmq_60_75 (`signingActiveQuorumCount` 32).
const B_INDEXES: u32 = 32;
/// The masternodes.
const MASTERNODES: usize = 60;
/// The asset lock's block and amount, then the masternodes' block.
const LOCK_HEIGHT: usize = 270;
const LOCKED: i64 = 100 * COIN;
const REGISTER_HEIGHT: usize = 271;
/// The tip before B's commitments, and the unlocks' `requestedHeight`.
pub const BEFORE_B: u32 = 1480;
/// The setup tip.
pub const TIP: u32 = 1491;
/// The unlocks' index (the credit pool has none).
const UNLOCK_INDEX: u64 = 1;
/// The quorum keys' key material: A0, A1, then B0 to B31.
const A_SEEDS: [u8; 2] = [0x80, 0x81];
const B_SEED: u8 = 0x90;
/// The twin control's invalid positions for B0, whose own are 0 and 1.
const TWIN_INVALID: [usize; 2] = [2, 3];

/// A rotated quorum of the fixture: its base block (internal bytes), quorum index, the height of
/// its window's first block and its key, its members as V gives them (ProTx hashes, in V's
/// order: the vector its operator keys and signing come from), the positions it marks invalid,
/// its commitment's bytes, and, read from Core's `quorum info` at the tip, Core's members in its
/// order and the block Core mined it in (displayed hash).
pub struct Quorum {
    pub hash: [u8; 32],
    pub index: u32,
    pub mined: u32,
    pub key: SecretKey,
    pub members: Vec<String>,
    pub invalid: Vec<usize>,
    pub qc: Vec<u8>,
    pub core_members: Vec<String>,
    pub core_mined: String,
}

/// What Core said of a commitment it refused: the refused commitment and Core's log line.
pub struct Refused {
    pub what: &'static str,
    pub line: String,
}

/// The fixture.
pub struct D14Chain {
    /// Blocks 2 to [`TIP`].
    pub setup: Vec<Arc<Block>>,
    /// A0 and A1.
    pub a: Vec<Quorum>,
    /// B0 to B31.
    pub b: Vec<Quorum>,
    /// Core's `quorum list 34` at the tip and V's unbounded scan of 34 there, as displayed
    /// hashes.
    pub core_order: Vec<String>,
    pub v_order: Vec<String>,
    /// Core's refusals: the negative self-test's and the twin control's.
    pub refused: Vec<Refused>,
    /// An unsubmitted template on the tip (`generateblock` with no transactions).
    pub template: Block,
    /// U33 (under A0) and U34 (under A1).
    pub u33: Arc<Tx>,
    pub u34: Arc<Tx>,
    /// The stopped producer's datadirs at [`BEFORE_B`] and at the tip.
    pub producer_before_b: PathBuf,
    pub producer: PathBuf,
    /// V's images at [`BEFORE_B`] and at the tip, for J1.
    #[allow(dead_code, reason = "J1's M path reads them")]
    pub v_before_b: SutImage,
    #[allow(dead_code, reason = "J1's M path reads them")]
    pub v_tip: SutImage,
    pub address: Value,
    /// How long production took.
    pub built_in: std::time::Duration,
}

impl D14Chain {
    /// A0, A1, then B0 to B31, named.
    pub fn quorums(&self) -> impl Iterator<Item = (String, &Quorum)> {
        let named = |prefix: &'static str| move |(i, q)| (format!("{prefix}{i}"), q);
        self.a.iter().enumerate().map(named("A")).chain(self.b.iter().enumerate().map(named("B")))
    }

    /// The full member-order check: for each quorum, where V's ordered members first differ from
    /// Core's `quorum info` (position, V's member, Core's); empty if every one is equal at every
    /// position.
    pub fn member_order_problems(&self) -> Vec<String> {
        self.quorums()
            .filter_map(|(name, q)| {
                let (v, core) = (&q.members, &q.core_members);
                let at = (0..v.len().max(core.len())).find(|&i| v.get(i) != core.get(i))?;
                Some(format!(
                    "{name}: position {at} of {}: V {:?}, Core {:?}",
                    core.len(),
                    v.get(at),
                    core.get(at)
                ))
            })
            .collect()
    }

    /// Block `height` of the setup chain.
    pub fn block(&self, height: u32) -> &Arc<Block> {
        &self.setup[height as usize - DEVNET.first_height]
    }

    /// Blocks 2 to `height`.
    pub fn setup_to(&self, height: u32) -> &[Arc<Block>] {
        &self.setup[..=height as usize - DEVNET.first_height]
    }

    /// A block on block `tip` carrying `tx`, as the producer's miner builds it from its mempool,
    /// never submitted: on a copy of the producer's datadir at `tip` (`datadir`), `tx` through
    /// `sendrawtransaction`, then `getblocktemplate`, assembled on the coinbase and header of the
    /// copy's `generateblock` with no transactions ([`assemble`]), ground on `pow`. That assembly
    /// is first checked to give `generateblock`'s coinbase exactly. The devnet's clock is fixed,
    /// so a template is kept until the node restarts (`rpc/mining.cpp:812-813`): the copy
    /// restarts between the two templates.
    pub fn mined_with(
        &self,
        binary: &Path,
        scratch: &Path,
        datadir: &Path,
        tip: u32,
        tx: &Tx,
        pow: &PowParams,
    ) -> Block {
        let dir = tempfile::tempdir_in(scratch).unwrap();
        copy_dir(datadir, dir.path());
        let start = || {
            let p = Dashd::start_on(binary, dir.path(), TIME, DEVNET.net, &ARGS, false);
            pinned(&p);
            let at = rpc(&p, "getbestblockhash", json!([]));
            assert_eq!(at, json!(self.block(tip).hash().to_string()), "the copy is at {tip}");
            p
        };
        let p = start();
        let empty = block_of(&rpc(&p, "generateblock", json!([self.address, [], false]))["hex"]);
        // A devnet template also carries the other LLMQ types' null commitments, as
        // generateblock's block does.
        let serialized =
            |b: &Block| b.txs().iter().map(|t| core_serialize(&**t)).collect::<Vec<_>>();
        let before = assemble(&empty, &rpc(&p, "getblocktemplate", json!([])), pow);
        assert_eq!(
            serialized(&before),
            serialized(&empty),
            "the assembly is generateblock's block"
        );
        drop(p);
        let p = start();
        rpc(&p, "sendrawtransaction", json!([hex(&core_serialize(tx))]));
        let block = assemble(&empty, &rpc(&p, "getblocktemplate", json!([])), pow);
        assert_eq!(block.header().prev_hash(), self.block(tip).hash(), "on block {tip}");
        let txids: Vec<_> = block.txs()[1..].iter().map(|t| t.txid()).collect();
        let mut expected: Vec<_> = before.txs()[1..].iter().map(|t| t.txid()).collect();
        expected.push(tx.txid());
        assert_eq!(txids, expected, "the template adds the tx alone to the coinbase it changes");
        block
    }
}

/// The producer at work: its RPC, its datadir, the V its blocks are fed to, and its blocks.
struct Producing<'a> {
    p: Option<Dashd>,
    binary: &'a Path,
    datadir: PathBuf,
    builder: (NodeSut, Arc<Checked>),
    address: Value,
    setup: Vec<Arc<Block>>,
}

impl Producing<'_> {
    fn p(&self) -> &Dashd {
        self.p.as_ref().expect("the producer runs")
    }

    fn start(&mut self) {
        let p = Dashd::start_on(self.binary, &self.datadir, TIME, DEVNET.net, &PRODUCER_ARGS, true);
        pinned(&p);
        self.p = Some(p);
    }

    fn new_address(&self) -> Value {
        rpc(self.p(), "getnewaddress", json!([]))
    }

    fn height(&self) -> usize {
        self.setup.len() + DEVNET.first_height - 1
    }

    fn block(&self, height: u32) -> &Arc<Block> {
        &self.setup[height as usize - DEVNET.first_height]
    }

    /// Mines to `height`, each block fed to the builder V, which must accept it.
    fn mine_to(&mut self, height: usize) {
        while self.height() < height {
            let hash = rpc(self.p(), "generatetoaddress", json!([1, self.address]))[0].clone();
            let block = Arc::new(block_of(&rpc(self.p(), "getblock", json!([hash, 0]))));
            let chain = self.builder.0.chain().expect("V runs");
            let r = chain.process_block(Arc::clone(&block), true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "V on block {}: {r:?}", self.height() + 1);
            self.setup.push(block);
        }
    }

    /// V's members of the Platform quorum at `base`, as ProTx hashes: Core's
    /// `GetAllQuorumMembers`, through route B's binding, which runs V's rotation.
    fn members(&self, base: BlockHash) -> Vec<String> {
        let chain = self.builder.0.chain().expect("V runs");
        let reply = chain.on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
            let id = q.chain.reader().lookup(&base).expect("V has the base block");
            let members = q.bind(o).members(PLATFORM, id, false).map_err(|e| e.to_string())?;
            Ok::<_, String>(members.iter().map(|m| m.pro_tx_hash.to_string()).collect())
        });
        reply.recv().expect("V serves").expect("V's members")
    }

    /// V's unbounded scan of `count` Platform quorums at its tip (`OnVBinding::scan_quorums`,
    /// not the 33-slot block seam), as displayed base hashes.
    fn v_scan(&self, count: usize) -> Vec<String> {
        let chain = self.builder.0.chain().expect("V runs");
        let tip = self.setup.last().unwrap().hash();
        let reply = chain.on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
            let id = q.chain.reader().lookup(&tip).expect("V has its tip");
            let quorums = q.bind(o).scan_quorums(PLATFORM, id, count).map_err(|e| e.to_string())?;
            Ok::<_, String>(quorums.iter().map(|q| q.base_hash.to_string()).collect())
        });
        reply.recv().expect("V serves").expect("V's scan")
    }

    /// The quorum of base height `base`, with key `key` and the members at `invalid` marked
    /// invalid: V's members, each signing with its operator key but the invalid ones.
    fn quorum(
        &self,
        base: u32,
        key: SecretKey,
        invalid: &[usize],
        operators: &[(String, SecretKey)],
    ) -> Quorum {
        let hash = self.block(base).hash();
        let members = self.members(hash);
        assert_eq!(
            members.len(),
            SIZE,
            "V's members of the quorum at {base}: a full llmq_60_75 quorum from cycle 1152 on"
        );
        let index = base % INTERVAL;
        let keys = operator_keys(&members, operators);
        let qc = sign::indexed_commitment(
            PLATFORM.to_base(),
            hash.as_bytes(),
            i16::try_from(index).unwrap(),
            SIZE,
            &keys,
            invalid,
            &key,
        );
        Quorum {
            hash: *hash.as_bytes(),
            index,
            mined: base - index + WINDOW_START,
            key,
            members,
            invalid: invalid.to_vec(),
            qc,
            core_members: Vec::new(),
            core_mined: String::new(),
        }
    }

    /// Core's members of `q`, in its order, and the block Core mined `q` in: `quorum info`, which
    /// always lists the members (`rpc/quorums.cpp:307`; its third argument is `includeSkShare`,
    /// `:296-299`), `members[i]` for `i` in order (`:214-239`).
    fn read_core(&self, q: &mut Quorum) {
        let shown = BlockHash::from(q.hash).to_string();
        let info = rpc(self.p(), "quorum", json!(["info", PLATFORM.to_base(), shown]));
        assert_eq!(info["quorumHash"], json!(shown), "quorum info's quorum");
        let members = info["members"].as_array().expect("quorum info's members");
        q.core_members =
            members.iter().map(|m| m["proTxHash"].as_str().unwrap().to_owned()).collect();
        q.core_mined = info["minedBlock"].as_str().expect("quorum info's minedBlock").to_owned();
    }

    /// Gives `qcs` to the producer, in order, then mines the window's first block, which must
    /// carry each of `mined` (the commitment's bytes in a type 6 payload,
    /// `CFinalCommitmentTxPayload`: version, height, commitment); else Core's LLMQ log lines on
    /// commitments.
    fn give_and_mine(&mut self, qcs: &[Vec<u8>], mined: &[&Quorum]) {
        p2p::give_commitments(PEER, self.p().p2p_port(), TIME, qcs);
        self.mine_to(self.height() + 1);
        let block = self.setup.last().unwrap();
        let carried: Vec<&[u8]> = block
            .txs()
            .iter()
            .filter(|tx| tx.raw().tx_type() == TxType::new(6))
            .filter_map(|tx| tx.raw().extra_payload().get(6..))
            .collect();
        let missing: Vec<u32> = mined
            .iter()
            .filter(|q| !carried.contains(&&q.qc[..]))
            .map(|q| q.mined - WINDOW_START + q.index)
            .collect();
        if !missing.is_empty() {
            let log = self.log();
            let lines: Vec<&str> = log.lines().filter(|l| l.contains("CFinalCommitment")).collect();
            panic!(
                "block {} lacks the commitments of bases {missing:?}: {lines:#?}",
                self.height()
            );
        }
    }

    fn log(&self) -> String {
        let path = self.p().net_dir().join("debug.log");
        std::fs::read_to_string(path).expect("the producer's log")
    }

    /// Core's log line that refused a commitment of base `hash` with `reason`.
    fn refusal(&self, what: &'static str, hash: &[u8; 32], reason: &str) -> Refused {
        let shown = BlockHash::from(*hash).to_string();
        let log = self.log();
        let line = log.lines().find(|l| l.contains(&shown) && l.contains(reason));
        let line = line.unwrap_or_else(|| panic!("Core refused {what} with \"{reason}\""));
        Refused {
            what,
            line: line.to_owned(),
        }
    }

    /// Stops the producer and copies its datadir to `to`, then starts it again.
    fn image(&mut self, to: &Path) {
        self.p = None;
        copy_dir(&self.datadir, to);
        self.start();
    }
}

/// The operator keys of `members` (ProTx hashes), in their order.
fn operator_keys<'k>(
    members: &[String],
    operators: &'k [(String, SecretKey)],
) -> Vec<&'k SecretKey> {
    members
        .iter()
        .map(|m| {
            let found = operators.iter().find(|(pro_tx, _)| pro_tx == m);
            &found.unwrap_or_else(|| panic!("member {m} is no masternode of the fixture")).1
        })
        .collect()
}

/// `quorum list 34`'s Platform hashes on `p`.
fn core_scan(p: &Dashd, count: usize) -> Vec<String> {
    let list = rpc(p, "quorum", json!(["list", count]));
    let hashes = list["llmq_60_75"].as_array().expect("the Platform type's list");
    hashes.iter().map(|h| h.as_str().unwrap().to_owned()).collect()
}

fn masternodes(p: &Dashd) -> Value {
    rpc(p, "masternode", json!(["count"]))["total"].clone()
}

/// Builds the D14 chain on a producer in `dir` (see the module docs).
pub fn produce_d14(binary: &Path, dir: &Path) -> D14Chain {
    let started = Instant::now();
    let builder = fresh_sut_on(DEVNET, &dir.join("builder"), TIME, &[], &[], &[]);
    let mut w = Producing {
        p: None,
        binary,
        datadir: dir.join("producer"),
        builder,
        address: Value::Null,
        setup: Vec::new(),
    };
    w.start();
    let tip = rpc(w.p(), "getblockchaininfo", json!([]));
    assert_eq!(tip["blocks"], json!(1), "a devnet starts at its devnet genesis block");
    rpc(w.p(), "createwallet", json!(["producer"]));
    w.address = w.new_address();

    // The credit pool's funding, in block 270.
    w.mine_to(LOCK_HEIGHT - 1);
    let p2pkh = unhex(
        rpc(w.p(), "validateaddress", json!([w.new_address()]))["scriptPubKey"].as_str().unwrap(),
    );
    let lock = hex(&core_serialize(&asset_lock(LOCKED, &p2pkh)));
    let funded = rpc(w.p(), "fundrawtransaction", json!([lock, {"feeRate": 0.0001}]));
    let signed = rpc(w.p(), "signrawtransactionwithwallet", json!([funded["hex"]]));
    assert_eq!(signed["complete"], json!(true));
    rpc(w.p(), "sendrawtransaction", json!([signed["hex"]]));
    w.mine_to(LOCK_HEIGHT);
    assert_eq!(rpc(w.p(), "getrawmempool", json!([])), json!([]), "block 270 mined the lock");

    // 60 regular masternodes, registered and funded by the producer's wallet in block 271.
    let mut operators: Vec<(String, SecretKey)> = Vec::new();
    for i in 0..MASTERNODES {
        let operator = rpc(w.p(), "bls", json!(["generate"]));
        let key = sign::operator_key(
            operator["secret"].as_str().unwrap(),
            operator["public"].as_str().unwrap(),
        );
        let pro_tx = rpc(
            w.p(),
            "protx",
            json!([
                "register_fund",
                w.new_address(),
                format!("11.0.0.{}:19799", i + 1),
                w.new_address(),
                operator["public"],
                w.new_address(),
                0,
                w.new_address(),
                w.address
            ]),
        );
        operators.push((pro_tx.as_str().unwrap().to_owned(), key));
    }
    w.mine_to(REGISTER_HEIGHT);
    assert_eq!(rpc(w.p(), "getrawmempool", json!([])), json!([]), "block 271 mined the ProRegs");
    assert_eq!(masternodes(w.p()), json!(MASTERNODES), "the masternodes");
    eprintln!("D14 chain: masternodes and the credit pool at {:?}", started.elapsed());

    // A: A0 and A1 in the first block of cycle 1152's window, after the negative self-test.
    let a_window = A_CYCLE + WINDOW_START;
    w.mine_to(a_window as usize - 1);
    let a: Vec<Quorum> = A_INDEXES
        .iter()
        .zip(A_SEEDS)
        .map(|(i, seed)| w.quorum(A_CYCLE + i, sign::key(seed), &[], &operators))
        .collect();
    let a0_keys = operator_keys(&a[0].members, &operators);
    let wrong_index =
        sign::indexed_commitment(PLATFORM.to_base(), &a[0].hash, 1, SIZE, &a0_keys, &[], &a[0].key);
    let short: Vec<usize> = (49..SIZE).collect();
    let forty_nine = sign::indexed_commitment(
        PLATFORM.to_base(),
        &a[0].hash,
        0,
        SIZE,
        &a0_keys,
        &short,
        &a[0].key,
    );
    let qcs = [wrong_index, forty_nine, a[0].qc.clone(), a[1].qc.clone()];
    w.give_and_mine(&qcs, &[&a[0], &a[1]]);
    let mut refused = vec![
        w.refusal(
            "A0 with quorum index 1",
            &a[0].hash,
            "is not the first block in the DKG interval",
        ),
        w.refusal("A0 with 49 valid members", &a[0].hash, "validMembersCount=49"),
    ];
    eprintln!("D14 chain: A mined in {} at {:?}", w.height(), started.elapsed());

    // The first image, before B.
    w.mine_to(BEFORE_B as usize);
    let producer_before_b = dir.join("producer-1480");
    w.image(&producer_before_b);
    let v_before_b = w.builder.0.save_image().expect("V's image at 1480");

    // B: B0 to B31 in the first block of cycle 1440's window, after B0's twin.
    let b_window = B_CYCLE + WINDOW_START;
    w.mine_to(b_window as usize - 1);
    let b: Vec<Quorum> = (0..B_INDEXES)
        .map(|i| {
            let first = 2 * i as usize % SIZE;
            let invalid = [first, first + 1];
            w.quorum(B_CYCLE + i, sign::key(B_SEED + i as u8), &invalid, &operators)
        })
        .collect();
    let b0 = &b[0];
    let b0_keys = operator_keys(&b0.members, &operators);
    let b0_signing: Vec<&SecretKey> = b0_keys
        .iter()
        .enumerate()
        .filter(|(i, _)| !b0.invalid.contains(i))
        .map(|(_, k)| *k)
        .collect();
    let twin = sign::final_commitment(
        PLATFORM.to_base(),
        &b0.hash,
        Some(0),
        &sign::valid_bits(SIZE, SIZE, &TWIN_INVALID),
        &b0_signing,
        &b0.key,
    );
    let mut qcs = vec![twin];
    qcs.extend(b.iter().map(|q| q.qc.clone()));
    w.give_and_mine(&qcs, &b.iter().collect::<Vec<_>>());
    refused.push(w.refusal(
        "B0's twin, its invalid bits moved to positions 2 and 3",
        &b0.hash,
        "invalid aggregated members signature",
    ));
    eprintln!("D14 chain: B mined in {} at {:?}", w.height(), started.elapsed());

    // The tip, the scan order on both nodes, and Core's members of each quorum.
    w.mine_to(TIP as usize);
    let (mut a, mut b) = (a, b);
    for q in a.iter_mut().chain(b.iter_mut()) {
        w.read_core(q);
    }
    let core_order = core_scan(w.p(), 34);
    let v_order = w.v_scan(34);
    let v_tip = w.builder.0.save_image().expect("V's image at the tip");

    let unlock = |quorum: &Quorum| {
        Unlock {
            version: 1,
            index: UNLOCK_INDEX,
            outputs: 1,
            requested: BEFORE_B,
            quorum: quorum.hash,
        }
        .signed_as(PLATFORM, &quorum.key, &p2pkh)
    };
    let (u33, u34) = (unlock(&a[0]), unlock(&a[1]));
    let template = block_of(&rpc(w.p(), "generateblock", json!([w.address, [], false]))["hex"]);
    assert_eq!(rpc(w.p(), "getrawmempool", json!([])), json!([]), "setup mined every tx");
    assert_eq!(masternodes(w.p()), json!(MASTERNODES), "the masternodes at the tip");
    let built_in = started.elapsed();
    eprintln!("D14 chain: built at {built_in:?}");
    let Producing {
        p,
        datadir,
        address,
        setup,
        ..
    } = w;
    drop(p);
    D14Chain {
        setup,
        a,
        b,
        core_order,
        v_order,
        refused,
        template,
        u33,
        u34,
        producer_before_b,
        producer: datadir,
        v_before_b,
        v_tip,
        address,
        built_in,
    }
}

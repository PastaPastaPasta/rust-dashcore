//! EV2b's L2 window (brief acceptance; EV2 §6.4): consecutive mainnet blocks from DIP3's
//! activation through `build_new_list_from_block` + `process_block` with our own list chain, and
//! through Core's `RebuildListFromBlock` (`oracle_dml_apply`) with Core's, each fed its own
//! previous list. Per block the two lists' canonical bytes, hence their `CanonicalMNListHash`
//! digests, must be equal, as must the payees; from DIP3 enforcement the block's coinbase must
//! pay our payee's script, which ties the chain to mainnet's own verdicts. The reference twin
//! (EV2b D24) reads Core's previous list itself and builds every block too: `xcheck` compares it
//! with our list before and after the build (mainnet has no 64-bit proTxHash collision, so our
//! own chain's buckets and a reload's agree; quirk 41).
//!
//! Coins are `Unchecked` (Core's null-best-block path, EV2 §6.4). From DIP3's activation no
//! quorum members are supplied: every commitment in 1,028,160–1,078,159 is a null `LLMQ_50_60`
//! one, which the build never resolves, so a window that needs members stops with an error
//! instead of guessing them. A window may carry the members of its quorums (EV2b D32, D33:
//! `node-l1/scripts/dml-w2-fetch.py`), each added before the block that mines it, and may start
//! from a supplied list with the headers back to its quorums' base blocks.

use std::sync::Arc;

use dash_consensus::primitives::{decode_block, decode_header};
use dash_dml::codec::canonical_mn_list_hash;
use dash_dml::codec::core_disk;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::reference::{RefMnList, build_new_list_from_block_ref, xcheck};
use dash_dml::testkit::list_disk_bytes;
use dash_dml::testkit::replay::{
    CORE_NET_INFO, Headers, Members, ReplayQuorum, mainnet, payee_rules, testnet,
};
use dash_dml::{
    DmlParams, DmlRules, Dmn, MnList, NoCoins, NoTrace, PrevBlock, build_new_list_from_block,
    process_block,
};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView as CoreCoins, Context, MnList as CoreList, Oracle, Pin,
    QuorumMembers, Verdict,
};
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dash_types::Numeric;

use crate::dml::canonical;

/// The network a window's blocks are from (EV2b D38: window 3 is testnet's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    Mainnet,
    Testnet,
}

impl Net {
    fn params(self) -> &'static ChainSetup {
        match self {
            Net::Mainnet => mainnet(),
            Net::Testnet => testnet(),
        }
    }

    fn chain(self) -> Chain {
        match self {
            Net::Mainnet => Chain::main(),
            Net::Testnet => Chain::test(),
        }
    }
}

/// Both list chains after a block.
pub struct Window {
    oracle: Oracle,
    net: Net,
    ours: Arc<MnList>,
    /// Core's list as evodb stores it (`SER_DISK`), its block hash set.
    core: Vec<u8>,
    /// The headers from `first_height` through the last block's.
    headers: Vec<[u8; 80]>,
    first_height: i32,
    /// The most headers (and blocks a quorum's members are kept for) the window keeps; `None`
    /// keeps all (a window from a supplied list).
    history: Option<usize>,
    /// The members of the quorums the window's commitments name, each with the height of the
    /// block it was added before.
    quorums: Vec<(i32, ReplayQuorum)>,
}

/// The headers a window from the default list keeps: more than a quorum base's distance from
/// the block that mines its commitment (the largest DKG interval, `LLMQ_400_85`'s 576).
const HISTORY: usize = 600;

/// One block's outcome: its height and the lists' `CanonicalMNListHash`.
pub struct Step {
    pub height: i32,
    pub digest: [u8; 32],
}

impl Window {
    /// Both chains at the empty list of the block before the first one (`parent`, at
    /// `height`), as Core holds it before DIP3's activation.
    pub fn new(net: Net, parent: &[u8], height: i32) -> Result<Window, String> {
        let parent: [u8; 80] =
            parent.get(..80).and_then(|h| h.try_into().ok()).ok_or("short parent block")?;
        let hash = header_hash(&parent)?;
        let ours = MnList::new(hash, height, 0);
        let core = list_disk_bytes(&ours);
        Ok(Window {
            oracle: Oracle::new(Pin::V24_0),
            net,
            ours: Arc::new(ours),
            core,
            headers: vec![parent],
            first_height: height,
            history: Some(HISTORY),
            quorums: Vec::new(),
        })
    }

    /// Both chains at `list`, the list after the last of `headers` as evodb stores it (its
    /// block hash and height that header's), the headers being consecutive.
    pub fn from_list(net: Net, headers: Vec<[u8; 80]>, list: Vec<u8>) -> Result<Window, String> {
        let ours = core_disk::decode_list_s3(&mut &list[..])
            .map_err(|e| format!("the start list: {e:?}"))?;
        let parent = headers.last().ok_or("no headers")?;
        if *ours.block_hash() != header_hash(parent)? {
            return Err("the start list is not the last header's".into());
        }
        for pair in headers.windows(2) {
            if pair[1][4..36] != *header_hash(&pair[0])?.as_bytes() {
                return Err("the headers are not a chain".into());
            }
        }
        Ok(Window {
            oracle: Oracle::new(Pin::V24_0),
            net,
            first_height: ours.height() + 1 - headers.len() as i32,
            ours: Arc::new(ours),
            core: list,
            headers,
            history: None,
            quorums: Vec::new(),
        })
    }

    /// Adds the members of a quorum a later block's commitment names (once: Core's context
    /// refuses two member lists for one quorum).
    pub fn add_quorum(&mut self, quorum: ReplayQuorum) {
        let known = self
            .quorums
            .iter()
            .any(|(_, q)| (q.llmq_type, q.quorum_hash) == (quorum.llmq_type, quorum.quorum_hash));
        if !known {
            self.quorums.push((self.ours.height() + 1, quorum));
        }
    }

    /// Applies the next block (raw bytes) to both chains and compares them.
    pub fn step(&mut self, raw: &[u8]) -> Result<Step, String> {
        let params = self.net.params();
        let height = self.ours.height() + 1;
        let block = decode_block(&mut &raw[..]).map_err(|_| "undecodable block")?.into_value();
        let header: [u8; 80] = raw[..80].try_into().map_err(|_| "short block")?;
        let hash = header_hash(&header)?;
        let at = format!("block {height}");
        let parent = self.headers.last().ok_or("no headers")?;
        if header[4..36] != *header_hash(parent)?.as_bytes() {
            return Err(format!("{at}: does not extend the last block"));
        }

        // Core: the bare RebuildListFromBlock over its previous list.
        let ctx = Context::new(
            self.first_height,
            self.headers.clone(),
            height < params.consensus.v19_height.to_u32() as i32,
            Vec::new(),
            i64::from(block.header().raw().time),
            Checkpoints::Disabled,
            CoreCoins::Unchecked(Vec::new()),
            CoreList::Supplied(self.core.clone()),
            self.quorums
                .iter()
                .map(|(_, q)| QuorumMembers {
                    llmq_type: q.llmq_type,
                    quorum_hash: q.quorum_hash,
                    members: q.members.clone(),
                })
                .collect(),
        );
        let core = self
            .oracle
            .dml_apply(&self.net.chain(), raw, &ctx)
            .map_err(|e| format!("{at}: oracle error {e:?}"))?;
        let (Verdict::Accept, Some(core_list), Some(core_canonical)) =
            (&core.verdict, core.new_list, core.canonical)
        else {
            return Err(format!("{at}: Core refuses it: {:?}", core.verdict));
        };

        // Ours: the build and ProcessBlock over our previous list.
        let headers = Headers::new(self.first_height, &self.headers).ok_or("bad headers")?;
        let tip = headers.tip();
        let quorums: Vec<_> = self.quorums.iter().map(|(_, q)| q.clone()).collect();
        let members = Members::new(&headers, &quorums).ok_or("undecodable members")?;
        let dml_params = DmlParams::new(params);
        let net_info = NetInfoRules::new(params, mainnet(), CORE_NET_INFO);
        let buried = payee_rules(params, height - 1);
        let rules = DmlRules {
            params: &dml_params,
            net_info: &net_info,
            v19: buried.v19,
            mn_rr: buried.mn_rr,
            v24: false,
        };
        let prev = PrevBlock {
            id: tip,
            hash: header_hash(parent)?,
            height: height - 1,
            chain: &headers,
        };
        let built = build_new_list_from_block(
            &block,
            prev,
            &self.ours,
            &NoCoins,
            &members,
            &rules,
            &mut Vec::new(),
            &mut NoTrace,
        )
        .map_err(|e| format!("{at}: ours refuses it: {e:?}"))?;
        let twin_prev = RefMnList::decode_s3(&mut &self.core[..])
            .map_err(|e| format!("{at}: the twin's read: {e:?}"))?;
        xcheck(&self.ours, &twin_prev, rules.payee()).map_err(|e| format!("{at}: before: {e}"))?;
        let twin = build_new_list_from_block_ref(
            &block,
            prev,
            &twin_prev,
            &NoCoins,
            &members,
            &rules,
            &mut Vec::new(),
            &mut NoTrace,
        )
        .map_err(|e| format!("{at}: the twin refuses it: {e:?}"))?;
        xcheck(&built.list, &twin.list, rules.payee()).map_err(|e| format!("{at}: after: {e}"))?;
        let (list, payee) = (built.list, built.payee);
        if canonical(&list) != core_canonical {
            return Err(format!("{at}: the lists differ"));
        }
        if payee.as_ref().map(|d| *d.pro_tx_hash.as_bytes()) != core.payee {
            return Err(format!("{at}: the payees differ"));
        }
        if height >= dml_params.dip3_enforcement_height
            && let Some(payee) = &payee
        {
            let outputs = block.txs()[0].outputs();
            let scripts = payee_scripts(payee).ok_or_else(|| {
                format!("{at}: payout shares are outside the window's coinbase check")
            })?;
            if !scripts
                .iter()
                .all(|script| outputs.iter().any(|o| o.script_pubkey.as_bytes() == *script))
            {
                return Err(format!(
                    "{at}: the coinbase does not pay our payee {}",
                    payee.pro_tx_hash
                ));
            }
        }
        let processed = process_block(
            &block,
            (hash, height),
            &self.ours,
            list,
            &dml_params,
            height == dml_params.dip3_height,
            &mut Vec::new(),
        )
        .map_err(|e| format!("{at}: process_block: {e:?}"))?;

        let mut core_list = core_list;
        core_list[..32].copy_from_slice(hash.as_bytes());
        self.core = core_list;
        self.ours = processed.list;
        self.headers.push(header);
        if let Some(history) = self.history {
            let drop = self.headers.len().saturating_sub(history);
            self.headers.drain(..drop);
            self.first_height += drop as i32;
            self.quorums.retain(|(added, _)| *added > height - history as i32);
        }
        Ok(Step {
            height,
            digest: canonical_mn_list_hash(&self.ours).to_lendian(),
        })
    }
}

/// `nOperatorReward` of an operator that takes the whole masternode reward (100.00%).
const WHOLE_REWARD: u16 = 10_000;

/// The scripts Core's `GetMasternodeTxOuts` pays for `payee` (v24 `masternode/payments.cpp`):
/// the operator's share, when it has a reward and a payout script, and the owner's rest, unless
/// the operator takes all of it. Version 3 payout shares are not on the windows' heights.
fn payee_scripts(payee: &Dmn) -> Option<Vec<&[u8]>> {
    let cold = &payee.cold;
    if !cold.payouts.is_empty() {
        return None;
    }
    let operator = payee.operator_reward != 0 && !cold.script_operator_payout.is_empty();
    let mut scripts = Vec::new();
    if !operator || payee.operator_reward < WHOLE_REWARD {
        scripts.push(cold.script_payout.as_slice());
    }
    if operator {
        scripts.push(cold.script_operator_payout.as_slice());
    }
    Some(scripts)
}

fn header_hash(header: &[u8; 80]) -> Result<BlockHash, String> {
    let header = decode_header(&mut &header[..]).map_err(|_| "undecodable header")?;
    Ok(header.value().hash())
}

//! One block built over a recorded context (EV2b D25): the list, headers, coins, deployments and
//! quorum members `oracle_dml_apply` takes, as plain data. node-l1's `diff_dml_apply` runs every
//! block through it, and the quirk tests replay the Core-recorded vectors in
//! `tests/data/ev2b-quirks.txt` through it without the oracle.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, OnceLock};

use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::primitives::{decode_block, decode_header};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_evo::EffectSink;
use dash_evo::llmq::{MemberError, QuorumMemberSource, QuorumMembers};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType, OutPoint, TxHash, TxOut};
use dash_types::Numeric;

use crate::codec::{self, core_disk};
use crate::netinfo::{NetInfoConfig, NetInfoRules};
use crate::reference::{RefMnList, XcheckMismatch, build_new_list_from_block_ref, xcheck};
use crate::{
    DmlBuildError, DmlException, DmlParams, DmlRules, DmlTrace, Dmn, MnList, NoTrace, PayeeRules,
    PrevBlock, StoreError, build_new_list_from_block,
};

/// The NetInfo configuration the oracle runs with: every network reachable.
pub const CORE_NET_INFO: NetInfoConfig = NetInfoConfig {
    cjdns_reachable: true,
};

/// Mainnet's parameters, whose NetInfo rules the oracle validates with (EV1-1 D40).
pub fn mainnet() -> &'static ChainParams {
    static MAIN: OnceLock<ChainParams> = OnceLock::new();
    MAIN.get_or_init(|| {
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0)
            .build()
            .expect("Core's parameters")
    })
}

/// Testnet's parameters (EV2b D38: window 3 is testnet's MN_RR crossing).
pub fn testnet() -> &'static ChainParams {
    static TEST: OnceLock<ChainParams> = OnceLock::new();
    TEST.get_or_init(|| {
        ChainParamsBuilder::new(NetworkKind::Testnet, Release::V24_0)
            .build()
            .expect("Core's parameters")
    })
}

/// The payee rules for the block after `height`, from the chain's buried heights.
pub fn payee_rules(params: &ChainParams, height: i32) -> PayeeRules {
    let next = i64::from(height) + 1;
    PayeeRules {
        v19: next >= i64::from(params.consensus.v19_height.to_u32()),
        mn_rr: next >= i64::from(params.consensus.mn_rr_height.to_u32()),
    }
}

/// An unspent output of the context.
#[derive(Clone, Debug)]
pub struct ReplayCoin {
    /// Little-endian, as Core stores it.
    pub txid: [u8; 32],
    pub vout: u32,
    pub height: u32,
    pub coinbase: bool,
    pub amount: i64,
    pub script_pubkey: Vec<u8>,
}

/// What `GetAllQuorumMembers` returns for one quorum: each member in Core's disk format.
#[derive(Clone, Debug)]
pub struct ReplayQuorum {
    pub llmq_type: u8,
    pub quorum_hash: [u8; 32],
    pub members: Vec<Vec<u8>>,
}

/// The context of one block.
#[derive(Clone, Debug)]
pub struct ReplayContext {
    /// Height of `headers[0]`.
    pub first_height: i32,
    /// Consecutive 80-byte headers; the last is the tip.
    pub headers: Vec<[u8; 80]>,
    pub coins: Vec<ReplayCoin>,
    /// Whether the coins view's best block is the tip (else null: no collateral re-check).
    pub coins_at_tip: bool,
    pub quorums: Vec<ReplayQuorum>,
    /// The tip's list, evodb bytes (`SER_DISK`).
    pub list: Vec<u8>,
    /// `GetSpecialTxRules(pindexPrev).v24`.
    pub v24: bool,
}

/// Our build of one block over a context.
pub struct Replayed {
    /// `GetMNPayee` of the tip list.
    pub payee: Option<[u8; 32]>,
    pub result: Result<MnList, DmlBuildError>,
    /// The reference twin's agreement with the build (EV2b D24): `xcheck` of the predecessor
    /// and its twin, then the same error or `xcheck` of the two new lists.
    pub twin: Result<(), XcheckMismatch>,
    /// The twin's new list, when it accepted the block.
    pub twin_list: Option<RefMnList>,
}

/// Our list and the reference twin's, as the builds of earlier blocks left them (EV2b D54): a
/// sequence of blocks carries them instead of reading each tip's list afresh, so an error in
/// what a list keeps beside its masternodes (the unique-property map) reaches the next block.
#[derive(Clone)]
pub struct Carried {
    pub list: Arc<MnList>,
    pub twin: RefMnList,
}

impl Replayed {
    /// Whether our build gives Core's result and, unless Core threw, Core's payee.
    pub fn agrees_with(&self, core: &CoreVerdict, core_payee: Option<[u8; 32]>) -> bool {
        agrees(&self.result, core)
            && (matches!(core, CoreVerdict::Exception(_)) || self.payee == core_payee)
    }
}

/// Builds `block` over `ctx` with `params` (Core's: `params` for the chain, mainnet's NetInfo
/// rules, V19 and MN_RR buried). An error names the input one of our readers refuses.
pub fn replay(
    params: &ChainParams,
    ctx: &ReplayContext,
    block: &[u8],
    trace: &mut dyn DmlTrace,
) -> Result<Replayed, &'static str> {
    replay_carried(params, ctx, None, block, trace)
}

/// [`replay`] over `carried` lists instead of the context's (which still gives the headers,
/// coins and members).
pub fn replay_carried(
    params: &ChainParams,
    ctx: &ReplayContext,
    carried: Option<&Carried>,
    block: &[u8],
    trace: &mut dyn DmlTrace,
) -> Result<Replayed, &'static str> {
    let prev_list = match carried {
        Some(c) => MnList::clone(&c.list),
        None => core_disk::decode_list_s3(&mut &ctx.list[..]).map_err(|_| "our list read")?,
    };
    let block = decode_block(&mut &block[..]).map_err(|_| "our block read")?.into_value();
    let headers = Headers::new(ctx.first_height, &ctx.headers).ok_or("our header read")?;
    let tip = headers.tip();
    let tip_header = headers.header(tip);
    let height = i32::try_from(tip_header.height).map_err(|_| "height")?;
    let coins = Coins::new(&ctx.coins, ctx.coins_at_tip.then_some(tip_header.hash));
    let members = Members::new(&headers, &ctx.quorums).ok_or("our member read")?;

    let dml_params = DmlParams::new(params);
    let net_info = NetInfoRules::new(params, mainnet(), CORE_NET_INFO);
    let buried = payee_rules(params, height);
    let rules = DmlRules {
        params: &dml_params,
        net_info: &net_info,
        v19: buried.v19,
        mn_rr: buried.mn_rr,
        v24: ctx.v24,
    };
    let payee = prev_list.payee(rules.payee()).map(|d| *d.pro_tx_hash.as_bytes());
    let prev = PrevBlock {
        id: tip,
        hash: tip_header.hash,
        height,
        chain: &headers,
    };
    let result = build_new_list_from_block(
        &block,
        prev,
        &prev_list,
        &coins,
        &members,
        &rules,
        &mut Vec::new(),
        trace,
    )
    .map(|built| built.list);

    // The twin's build of the same block, compared with ours (EV2 §6.4's `-consensusxcheck`),
    // from its own read of the context's list.
    let mut twin_list = None;
    let twin = (|| {
        let twin_prev = match carried {
            Some(c) => c.twin.clone(),
            None => RefMnList::decode_s3(&mut &ctx.list[..])
                .map_err(|_| XcheckMismatch("xcheck twin's list read"))?,
        };
        xcheck(&prev_list, &twin_prev, rules.payee())?;
        let twin = build_new_list_from_block_ref(
            &block,
            prev,
            &twin_prev,
            &coins,
            &members,
            &rules,
            &mut Vec::new(),
            &mut NoTrace,
        );
        match (&result, twin) {
            (Ok(list), Ok(twin)) => {
                xcheck(list, &twin.list, rules.payee())?;
                twin_list = Some(twin.list);
                Ok(())
            }
            (Err(a), Err(b)) if *a == b => Ok(()),
            _ => Err(XcheckMismatch("xcheck verdict")),
        }
    })();
    Ok(Replayed {
        payee,
        result,
        twin,
        twin_list,
    })
}

/// The headers of a context as a block tree: `BlockId(i)` is `headers[i]`. The first header has
/// no parent, as in the oracle's chain context (so `prev` breaks `ChainAncestry`'s contract
/// for a context that starts above genesis; the build reads only `ancestor`).
pub struct Headers(Vec<HeaderCtx>);

impl Headers {
    /// `headers` from `first_height`; `None` when there is none or one does not decode.
    pub fn new(first_height: i32, headers: &[[u8; 80]]) -> Option<Headers> {
        let first = u32::try_from(first_height).ok()?;
        if headers.is_empty() {
            return None;
        }
        headers
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                let header = decode_header(&mut &raw[..]).ok()?;
                let header = header.value();
                Some(HeaderCtx {
                    height: first + i as u32,
                    hash: header.hash(),
                    time: header.raw().time,
                    bits: CompactTarget::new(header.raw().bits),
                    version: header.version(),
                    // Neither is read by the build.
                    mtp: 0,
                    chain_work: Arith256::default(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .map(Headers)
    }

    /// The last header.
    pub fn tip(&self) -> BlockId {
        BlockId(self.0.len() as u32 - 1)
    }
}

impl ChainAncestry for Headers {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let first = self.0.first()?.height;
        (first..=self.0[id.0 as usize].height).contains(&height).then(|| BlockId(height - first))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// The context's coins; the best block is the tip's hash when Core's view has one. Also the
/// view node-l1's check layer reads (EV1-3).
pub struct Coins {
    coins: BTreeMap<OutPoint, (TxOut, u32, bool)>,
    best_block: Option<BlockHash>,
}

impl Coins {
    pub fn new(coins: &[ReplayCoin], best_block: Option<BlockHash>) -> Coins {
        let coins = coins
            .iter()
            .map(|c| {
                let outpoint = OutPoint {
                    hash: TxHash::from_lendian(c.txid),
                    index: c.vout,
                };
                let out = TxOut {
                    value: c.amount,
                    script_pubkey: c.script_pubkey.clone().into(),
                };
                (outpoint, (out, c.height, c.coinbase))
            })
            .collect();
        Coins {
            coins,
            best_block,
        }
    }
}

impl CoinsView for Coins {
    type Error = Infallible;

    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
        Ok(self.coins.get(op).map(|(out, height, cb)| CoinRef::from_output(out, *height, *cb)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.best_block
    }
}

/// `GetAllQuorumMembers` as the context answers it: the supplied members of the quorum whose
/// base is `base`, else `MemberError::Missing` (a store fault: nothing to compare).
pub struct Members<'a> {
    headers: &'a Headers,
    quorums: Vec<(u8, [u8; 32], QuorumMembers<Arc<Dmn>>)>,
}

impl<'a> Members<'a> {
    /// The members of the context's quorums, each read as Core's disk format; `None` when one
    /// does not decode.
    pub fn new(headers: &'a Headers, quorums: &[ReplayQuorum]) -> Option<Self> {
        let quorums = quorums
            .iter()
            .map(|q| {
                let members = q
                    .members
                    .iter()
                    .map(|m| core_disk::decode_dmn(&mut &m[..]).ok().map(Arc::new))
                    .collect::<Option<Vec<_>>>()?;
                Some((q.llmq_type, q.quorum_hash, QuorumMembers::from(members)))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Members::from_members(headers, quorums))
    }

    /// The members of `quorums` (llmqType, quorum hash, members), already read.
    pub fn from_members(
        headers: &'a Headers,
        quorums: Vec<(u8, [u8; 32], QuorumMembers<Arc<Dmn>>)>,
    ) -> Self {
        Members {
            headers,
            quorums,
        }
    }
}

impl QuorumMemberSource for Members<'_> {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        let hash = *self.headers.header(base).hash.as_bytes();
        self.quorums
            .iter()
            .find(|(t, h, _)| *t == llmq_type.to_base() && *h == hash)
            .map(|(_, _, members)| Arc::clone(members))
            .ok_or(MemberError::Missing(base))
    }
}

/// Core's result for a vector's block (`oracle_dml_apply`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreVerdict {
    /// Accepted, with the new list's canonical bytes.
    Accept(Vec<u8>),
    /// `state.Invalid(…, reason)`.
    Reject(String),
    /// An exception escaping `RebuildListFromBlock`, with its message.
    Exception(String),
}

/// A block over a context with Core's result: one line per field (`key value`), `end` closing
/// each vector, `#` comments; lists of headers are concatenated hex.
#[derive(Clone, Debug)]
pub struct Vector {
    pub quirk: u32,
    pub name: String,
    pub ctx: ReplayContext,
    pub block: Vec<u8>,
    pub core_payee: Option<[u8; 32]>,
    pub core: CoreVerdict,
    /// What only the oracle reads to run the block again (`oracle_<key> <value>` lines: the
    /// chain, the BLS scheme, the adjusted time, the deployments); the replay ignores them.
    pub oracle: Vec<(String, String)>,
}

/// `bytes` in lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes of lower- or upper-case hex `s`; `None` when it is not hex.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl Vector {
    /// The vector in the text format [`read_vectors`] reads.
    ///
    /// # Panics
    ///
    /// When a text field (the name, Core's reason or message, an oracle field) holds a newline,
    /// which the format cannot carry.
    pub fn write(&self) -> String {
        let text = [&self.name]
            .into_iter()
            .chain(match &self.core {
                CoreVerdict::Reject(s) | CoreVerdict::Exception(s) => Some(s),
                CoreVerdict::Accept(_) => None,
            })
            .chain(self.oracle.iter().flat_map(|(k, v)| [k, v]));
        for field in text {
            assert!(!field.contains('\n'), "a newline in a vector field: {field:?}");
        }
        let c = &self.ctx;
        let mut out = format!("quirk {}\nname {}\n", self.quirk, self.name);
        out += &format!("first_height {}\n", c.first_height);
        out += &format!("headers {}\n", hex(&c.headers.concat()));
        out += &format!("coins_at_tip {}\n", u8::from(c.coins_at_tip));
        for coin in &c.coins {
            out += &format!(
                "coin {} {} {} {} {} {}\n",
                hex(&coin.txid),
                coin.vout,
                coin.height,
                u8::from(coin.coinbase),
                coin.amount,
                hex(&coin.script_pubkey)
            );
        }
        for q in &c.quorums {
            let members: Vec<String> = q.members.iter().map(|m| hex(m)).collect();
            out +=
                &format!("quorum {} {} {}\n", q.llmq_type, hex(&q.quorum_hash), members.join(","));
        }
        out += &format!(
            "list {}\nv24 {}\nblock {}\n",
            hex(&c.list),
            u8::from(c.v24),
            hex(&self.block)
        );
        out += &format!("core_payee {}\n", self.core_payee.map_or("-".into(), |p| hex(&p)));
        out += &match &self.core {
            CoreVerdict::Accept(canonical) => format!("core_accept {}\n", hex(canonical)),
            CoreVerdict::Reject(reason) => format!("core_reject {reason}\n"),
            CoreVerdict::Exception(message) => format!("core_exception {message}\n"),
        };
        for (key, value) in &self.oracle {
            out += &format!("oracle_{key} {value}\n");
        }
        out + "end\n"
    }
}

/// The vectors of `text` ([`Vector::write`]'s format); `None` on a malformed line.
pub fn read_vectors(text: &str) -> Option<Vec<Vector>> {
    let mut out = Vec::new();
    let mut fields: Vec<(&str, &str)> = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "end" {
            out.push(vector(&fields)?);
            fields.clear();
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        fields.push((key, value));
    }
    fields.is_empty().then_some(out)
}

fn vector(fields: &[(&str, &str)]) -> Option<Vector> {
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let all = |key: &'static str| fields.iter().filter(move |(k, _)| *k == key).map(|(_, v)| *v);
    let hash = |s: &str| -> Option<[u8; 32]> { unhex(s)?.try_into().ok() };
    let coins = all("coin")
        .map(|v| {
            let p: Vec<&str> = v.split(' ').collect();
            let [txid, vout, height, coinbase, amount, script] = p[..] else {
                return None;
            };
            Some(ReplayCoin {
                txid: hash(txid)?,
                vout: vout.parse().ok()?,
                height: height.parse().ok()?,
                coinbase: coinbase == "1",
                amount: amount.parse().ok()?,
                script_pubkey: unhex(script)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let quorums = all("quorum")
        .map(|v| {
            let p: Vec<&str> = v.split(' ').collect();
            let [llmq_type, quorum_hash, members] = p[..] else {
                return None;
            };
            Some(ReplayQuorum {
                llmq_type: llmq_type.parse().ok()?,
                quorum_hash: hash(quorum_hash)?,
                members: members
                    .split(',')
                    .filter(|m| !m.is_empty())
                    .map(unhex)
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let headers = unhex(get("headers")?)?
        .chunks(80)
        .map(|h| h.try_into().ok())
        .collect::<Option<Vec<[u8; 80]>>>()?;
    let core = if let Some(c) = get("core_accept") {
        CoreVerdict::Accept(unhex(c)?)
    } else if let Some(r) = get("core_reject") {
        CoreVerdict::Reject(r.to_owned())
    } else {
        CoreVerdict::Exception(get("core_exception")?.to_owned())
    };
    Some(Vector {
        quirk: get("quirk")?.parse().ok()?,
        name: get("name")?.to_owned(),
        ctx: ReplayContext {
            first_height: get("first_height")?.parse().ok()?,
            headers,
            coins,
            coins_at_tip: get("coins_at_tip")? == "1",
            quorums,
            list: unhex(get("list")?)?,
            v24: get("v24")? == "1",
        },
        block: unhex(get("block")?)?,
        core_payee: match get("core_payee")? {
            "-" => None,
            p => Some(hash(p)?),
        },
        core,
        oracle: fields
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix("oracle_")?.to_owned(), (*v).to_owned())))
            .collect(),
    })
}

/// Whether `ours` is Core's result `core`, as `diff_dml_apply` compares them: the canonical
/// bytes of an accepted list, the reject reason, or an exception's text up to its first rendered
/// value (EV2a D3).
pub fn agrees(ours: &Result<MnList, DmlBuildError>, core: &CoreVerdict) -> bool {
    let class = |text: &str| text.split('=').next().unwrap_or(text).to_owned();
    match (ours, core) {
        (Ok(list), CoreVerdict::Accept(canonical)) => {
            let mut bytes = Vec::new();
            codec::encode_canonical_list_unvalidated(list, &mut bytes);
            bytes == *canonical
        }
        (Err(e), CoreVerdict::Reject(reason)) => e.reject_reason() == Some(reason.as_str()),
        (Err(DmlBuildError::Exception(DmlException::List(e))), CoreVerdict::Exception(m)) => {
            class(&e.core_message()) == class(m.strip_suffix(": iostream error").unwrap_or(m))
        }
        _ => false,
    }
}

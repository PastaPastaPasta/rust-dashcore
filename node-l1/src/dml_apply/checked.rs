//! `diff_dml_block`'s checked mode (EV2b D54; Sol r2 F2, F3): a sequence of up to
//! [`MAX_BLOCKS`] blocks whose special transactions each pass Core's check loop against the tip's
//! list, so the verdicts come from the build and `ProcessBlock`. What varies is what the check
//! loop does not see: conflicts between the transactions of one block (an owner key, an
//! operator key, an address or a platform id taken twice; a masternode registered, updated,
//! replaced or revoked, or its collateral spent, in the same block) and values an earlier block of
//! the sequence freed. The sequence carries its state: Core's list after each accepted block,
//! that block's coins, and (in [`crate::dml_block`]) our own lists, so an error in what our list
//! keeps beside its masternodes (the unique-property map) shows as a later block's verdict.
//!
//! Layout: byte 0 picks the case (a crafted one, or one past them [`testnet_case`], where
//! `ProcessBlock` rejects every block: its hash is not the pinned enforcement block's); byte 1
//! holds the flags ([`FLAG_CHECKED`], and `generate`'s). Then the blocks' operations, each a kind
//! byte and its arguments (missing bytes read as 0). Kind 11 ends a block, as do
//! [`MAX_CHECKED_TXS`] operations; a block may have none.
//!
//! | kind mod 12 | transaction |
//! |---|---|
//! | 0–3 | ProRegTx: its own collateral output, a fresh coin, or a listed masternode's (a replacement) |
//! | 4 | ProUpServTx of a listed masternode |
//! | 5 | ProUpRegTx of a listed masternode, its collateral coin in the view |
//! | 6 | ProUpRevTx of a listed masternode |
//! | 7 | a spend of a listed collateral, of one registered earlier in the block, or of a fresh coin |
//! | 8 | QcTx at the block's height, on the last quorum base, members supplied, some invalid |
//! | 9 | ProUpShareTx of a listed shared masternode |
//! | 10 | ProUpSharedRegTx of a listed shared masternode |
//!
//! Each owner key, operator key, address and platform id a payload takes is fresh, or (one in
//! eight each) one an earlier transaction of the block took, or one an earlier block of the
//! sequence freed. Every transaction has its own input and the matching inputs hash; signatures
//! are not checked (`check_sigs` 0). Not generated: shared registrations and ProDisTx (the raw
//! generator reaches them), and Evo payloads under V24 (they need ExtAddr's platform entries).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use dash_consensus::primitives::{decode_header, decode_tx};
use dash_consensus::subsidy::COIN;
use dash_dml::codec::{ContextFreeNetInfo, core_disk, decode_canonical_list};
use dash_dml::specialtx::sig::inputs_hash_of;
use dash_dml::testkit::{dmn_bytes, list_disk_bytes, operator_key_from};
use dash_dml::{DmlParams, Dmn, MnList, OperatorKey, RawMnType};
use dash_oracle_sys::{Chain, CoinsView, Context, ContextCoin, MnList as CoreList, QuorumMembers};
use dash_params::ChainSetup;
use dash_pkc::bls::{
    BlsEagerPk, BlsEagerSig, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey,
};
use dash_primitives::{
    AddrV1, AddrV2, COMPACT_SIG_LEN, CollateralSig, CompactSig, InputsHash, MnType, NIEntry,
    NetInfo, NetInfoV1, NetInfoV2, OutPoint, PayoutShare, PlatformNodeId, ProRegTx, ProTxVersion,
    ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx, QuorumVvecHash,
    RevocationReason, ServiceV1, ServiceV2, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::input::{
    Bytes, Case, FLAG_FORCE_V24, FLAG_UNCHECKED_COINS, assemble, chain_for, crafted, encoded,
    force_v24, p2pkh, spend_of, split_block, testnet_case,
};
use crate::dml::{extended, legacy, payout, spread, twenty};

/// Input flags (byte 1) bit: `diff_dml_block` reads the input as a checked sequence.
pub const FLAG_CHECKED: u8 = 4;
/// Input flags (byte 1) bit, in checked mode: some transactions are probes, each changed so that
/// Core's check loop rejects it at a step the node implements (EV2b D56). Off, the sequence's
/// bytes are those before D56 (`checked_generator_is_byte_stable`).
pub const FLAG_PROBE: u8 = 8;
/// The most blocks a sequence has.
pub const MAX_BLOCKS: usize = 4;
/// The most operations a block has.
pub const MAX_CHECKED_TXS: usize = 50;
// `assemble` writes the transaction count, coinbase included, as one byte.
const _: () = assert!(MAX_CHECKED_TXS < 0xfc);

/// An IPv4 service, the kind of address the generator writes.
type Service = ([u8; 4], u16);

/// Values payloads take, of one kind each.
#[derive(Default)]
struct Values {
    owners: Vec<[u8; 20]>,
    operators: Vec<OperatorKey>,
    services: Vec<Service>,
    platform_ids: Vec<[u8; 20]>,
}

impl Values {
    /// The unique values of `list` (Core's `mnUniquePropertyMap` keys the generator writes).
    fn of(list: &MnList) -> Values {
        let mut values = Values::default();
        for d in list.iter() {
            if d.cold.is_shared() {
                values.owners.extend(d.cold.shares.iter().map(|s| *s.key_id_owner.as_bytes()));
            } else {
                values.owners.push(*d.cold.key_id_owner.as_bytes());
            }
            if d.cold.pub_key_operator != OperatorKey::new(BlsSchemeKind::Basic) {
                values.operators.push(d.cold.pub_key_operator.clone());
            }
            values.services.extend(services(&d.cold.net_info));
            if d.is_evo() {
                values.platform_ids.push(*d.cold.platform_node_id.as_bytes());
            }
        }
        values
    }

    /// Moves the pool past a block that changed the list from `old` to `new`: drops the values
    /// `new` holds (again), and adds those of `old` it no longer holds.
    fn update_freed(&mut self, old: &Values, new: &Values) {
        fn update<T: Clone, K: Ord>(
            pool: &mut Vec<T>,
            old: &[T],
            new: &[T],
            key: impl Fn(&T) -> K,
        ) {
            let held: BTreeSet<K> = new.iter().map(&key).collect();
            pool.retain(|v| !held.contains(&key(v)));
            let mut known: BTreeSet<K> = pool.iter().map(&key).collect();
            for v in old {
                if !held.contains(&key(v)) && known.insert(key(v)) {
                    pool.push(v.clone());
                }
            }
        }
        update(&mut self.owners, &old.owners, &new.owners, |v| *v);
        update(&mut self.operators, &old.operators, &new.operators, operator_id);
        update(&mut self.services, &old.services, &new.services, |v| *v);
        update(&mut self.platform_ids, &old.platform_ids, &new.platform_ids, |v| *v);
    }
}

/// An operator key's identity under any scheme (`HasOperatorKeyUnderAnyScheme`).
fn operator_id(key: &OperatorKey) -> [u8; 48] {
    key.to_bytes_as(BlsSchemeKind::Basic)
}

/// The IPv4 services of a network info.
fn services(net_info: &NetInfo) -> Vec<Service> {
    match net_info {
        NetInfo::Legacy(v1) => {
            let ip = v1.0.addr.0;
            (ip[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff])
                .then(|| ([ip[12], ip[13], ip[14], ip[15]], v1.0.port))
                .into_iter()
                .collect()
        }
        NetInfo::Extended(v2) => v2
            .entries
            .iter()
            .flat_map(|(_, entries)| entries)
            .filter_map(|e| match e {
                NIEntry::Service(ServiceV2 {
                    addr: AddrV2::Ipv4(ip),
                    port,
                }) => Some((*ip, *port)),
                _ => None,
            })
            .collect(),
    }
}

/// The network info of a payload of `version` with `service` as its core address.
fn net_info(service: Service, version: u16) -> NetInfo {
    let service = ServiceV2 {
        addr: AddrV2::Ipv4(service.0),
        port: service.1,
    };
    if version >= 3 {
        extended(service)
    } else {
        legacy(&service)
    }
}

/// What the chain's checks allow a payload at the block.
struct Rules {
    /// `ProTxVersion::GetMax(v19, v24)`.
    max_version: u16,
    /// Before DIP3 enforcement the voting key is the owner key (`bad-protx-key-not-same`).
    owner_is_voting: bool,
    /// Whether to register Evo nodes: from BasicBLS, and not under V24 (module docs).
    evo: bool,
    v24: bool,
    /// The chain's port, the core port of fresh addresses (mainnet's is refused elsewhere).
    port: u16,
}

/// The tip's list as the generator reads it.
struct Tip {
    list: MnList,
    height: i32,
}

/// The transactions of the block being generated, and what they took.
#[derive(Default)]
struct Block {
    txs: Vec<Vec<u8>>,
    taken: Values,
    /// The collateral of each registration so far.
    collaterals: Vec<OutPoint>,
    /// The masternodes updated so far.
    updated: Vec<[u8; 32]>,
    /// Values taken from those an earlier block freed.
    retaken: usize,
}

/// A checked sequence being generated: Core's context for its next block, and the generator's
/// state. [`Walk::next_block`] gives a block; [`Walk::accepted`] moves the context past it when
/// both sides accepted it, else the next block is built on the same tip.
pub struct Walk<'a> {
    r: Bytes<'a>,
    case: &'static Case,
    chain: Chain,
    pub ctx: Context,
    rules: Rules,
    /// The next block's header: the tip's hash as its parent.
    header: [u8; 80],
    coinbase: Transaction,
    /// Values earlier blocks of the sequence freed.
    freed: Values,
    fresh: u32,
    blocks: usize,
    /// The block given last, until it is accepted or replaced.
    pending: Option<Vec<u8>>,
    /// How many values the block given last took from those an earlier block freed.
    pub retaken: usize,
    /// [`FLAG_PROBE`].
    probe: bool,
}

impl<'a> Walk<'a> {
    /// The sequence an input describes; `None` for an empty input.
    pub fn new(input: &'a [u8]) -> Option<Walk<'a>> {
        let mut r = Bytes(input);
        if r.is_empty() {
            return None;
        }
        let cases = super::cases();
        let pick = usize::from(r.next()) % (cases.len() + 1);
        let testnet = pick == cases.len();
        let case = if testnet {
            testnet_case()
        } else {
            &cases[pick]
        };
        let flags = r.next();
        let mut ctx = case.ctx.clone();
        if flags & FLAG_UNCHECKED_COINS != 0
            && let CoinsView::AtTip(coins) = &ctx.coins
        {
            ctx.coins = CoinsView::Unchecked(coins.clone());
        }
        let forced = !testnet && !case.v24 && flags & FLAG_FORCE_V24 != 0;
        if forced {
            force_v24(&mut ctx);
        }
        let v24 = case.v24 || forced;
        let chain = if testnet {
            Chain::test()
        } else {
            chain_for(v24)
        };
        let (header, txs) = split_block(&case.block)?;
        let mut header: [u8; 80] = header.try_into().ok()?;
        if testnet {
            // Another block hash than the enforcement block's.
            header[76] ^= 1;
        }
        let coinbase = Transaction::decode(&mut &txs.first()?[..]).ok()?;
        let tip = tip_of(&ctx)?;
        let params = &case.params;
        let v19 = tip.height + 1 >= i32::try_from(params.consensus.v19_height.to_u32()).ok()?;
        let max_version = match (v19, v24) {
            (_, true) => 3,
            (true, false) => 2,
            (false, false) => 1,
        };
        let rules = Rules {
            max_version,
            owner_is_voting: tip.height < DmlParams::new(params).dip3_enforcement_height,
            evo: max_version == 2,
            v24,
            port: params.default_port,
        };
        Some(Walk {
            r,
            case,
            chain,
            ctx,
            rules,
            header,
            coinbase,
            freed: Values::default(),
            fresh: 0,
            blocks: 0,
            pending: None,
            retaken: 0,
            probe: flags & FLAG_PROBE != 0,
        })
    }

    pub fn chain(&self) -> Chain {
        self.chain.clone()
    }

    pub fn params(&self) -> &'static ChainSetup {
        &self.case.params
    }

    /// The next block; `None` once the input is read or [`MAX_BLOCKS`] are given.
    pub fn next_block(&mut self) -> Option<Vec<u8>> {
        if self.blocks == MAX_BLOCKS || (self.blocks > 0 && self.r.is_empty()) {
            return None;
        }
        self.blocks += 1;
        let tip = tip_of(&self.ctx)?;
        let mut block = Block::default();
        while !self.r.is_empty() && block.txs.len() < MAX_CHECKED_TXS {
            // A probe's byte is read only under the flag, so without it the input reads as before.
            let probe = self.probe.then(|| self.r.next()).filter(|p| p % 4 == 0);
            let tx = match self.r.next() % 12 {
                0..=3 => self.pro_reg(&tip, &mut block),
                4 => self.pro_up_serv(&tip, &mut block),
                5 => self.pro_up_reg(&tip, &mut block),
                6 => self.pro_up_rev(&tip, &mut block),
                7 => self.spend(&tip, &block),
                8 => self.qc(&tip),
                9 => self.pro_up_share(&tip),
                10 => self.pro_up_shared_reg(&tip, &mut block),
                _ => break,
            };
            if let Some(mut tx) = tx {
                if let Some(pick) = probe {
                    self.make_probe(&tip, &mut tx, pick / 4);
                }
                let mut bytes = Vec::new();
                tx.encode(&mut bytes);
                block.txs.push(bytes);
            }
        }
        let height = tip.height + 1;
        let mut coinbase = self.coinbase.clone();
        // The cbTx's `nHeight` follows its version (`bad-cbtx-height`).
        coinbase.extra_payload.get_mut(2..6)?.copy_from_slice(&height.to_le_bytes());
        let coinbase = encoded(&coinbase);
        let mut parts = vec![coinbase.as_slice()];
        parts.extend(block.txs.iter().map(Vec::as_slice));
        // The header is the tip's successor, merkle root not recomputed: every block offered on
        // one tip has the same hash.
        let bytes = assemble(&self.header, &parts);
        self.retaken = block.retaken;
        self.pending = Some(bytes.clone());
        Some(bytes)
    }

    /// Moves the context past the block given last, which both sides accepted with
    /// `core_canonical` as the new list: its header the tip, the list Core's, its outputs coins
    /// and its inputs spent. `None` when there is no such block or the list does not read.
    pub fn accepted(&mut self, core_canonical: &[u8]) -> Option<()> {
        let block = self.pending.take()?;
        let old = tip_of(&self.ctx)?;
        let list = decode_canonical_list(&mut &core_canonical[..], &ContextFreeNetInfo).ok()?;
        self.freed.update_freed(&Values::of(&old.list), &Values::of(&list));
        let (header, txs) = split_block(&block)?;
        let height = u32::try_from(old.height + 1).ok()?;
        let coins = match &mut self.ctx.coins {
            CoinsView::AtTip(coins) | CoinsView::Unchecked(coins) => coins,
        };
        for raw in &txs[1..] {
            let txid = *decode_tx(&mut &raw[..]).ok()?.into_value().txid().as_bytes();
            let tx = Transaction::decode(&mut &raw[..]).ok()?;
            for input in &tx.inputs {
                let spent = (*input.prevout.hash.as_bytes(), input.prevout.index);
                coins.retain(|c| (c.txid, c.vout) != spent);
            }
            for (vout, out) in tx.outputs.iter().enumerate() {
                coins.push(ContextCoin {
                    txid,
                    vout: u32::try_from(vout).ok()?,
                    height,
                    coinbase: false,
                    amount: out.value,
                    script_pubkey: out.script_pubkey.to_vec(),
                });
            }
        }
        let header: [u8; 80] = header.try_into().ok()?;
        let hash = *decode_header(&mut &header[..]).ok()?.value().hash().as_bytes();
        self.ctx.headers.push(header);
        self.ctx.mn_list = CoreList::Supplied(list_disk_bytes(&list));
        self.header[4..36].copy_from_slice(&hash);
        Some(())
    }

    /// Makes `tx` a probe (D56): changed so that Core's check loop rejects it at a step the node
    /// implements, the step `pick` names. A step that does not apply to the transaction leaves it
    /// as it is.
    fn make_probe(&mut self, tip: &Tip, tx: &mut Transaction, pick: u8) {
        let provider = matches!(
            tx.tx_type,
            TxType::ProviderRegister
                | TxType::ProviderUpdateService
                | TxType::ProviderUpdateRegistrar
                | TxType::ProviderUpdateRevoke
        );
        let arm = usize::from(pick / 16);
        match pick % 16 {
            // A payload of only its version, of which Core reads only that: above every known
            // one (`GetValidatedPayload`'s maximum) or 0 (`IsTriviallyValid`); or with a byte
            // after it, which `GetTxPayload` refuses (`bad-protx-payload`).
            0 if provider => {
                let version: u16 = [0, 4, u16::MAX, 200][arm % 4];
                tx.extra_payload = version.to_le_bytes().to_vec();
                if arm % 4 == 3 {
                    tx.extra_payload.push(0);
                }
            }
            // A type the dispatcher has no case for, or a coinbase's on a transaction that is
            // none (`bad-tx-type-check`, `bad-cbtx-invalid`).
            1 => {
                tx.tx_type = [TxType::from_base(13), TxType::CoinbaseCommitment][arm % 2];
            }
            // `CheckInputsHash`: an input the payload's hash does not cover.
            2 if provider => {
                if let Some(input) = tx.inputs.first_mut() {
                    input.prevout.index ^= 1;
                }
            }
            step => {
                if let Some(payload) = self.probe_payload(tip, tx, step, arm) {
                    tx.extra_payload = payload;
                }
            }
        }
    }

    /// [`Walk::make_probe`]'s steps in the payload, re-encoded.
    fn probe_payload(
        &mut self,
        tip: &Tip,
        tx: &Transaction,
        step: u8,
        arm: usize,
    ) -> Option<Vec<u8>> {
        let bytes = &tx.extra_payload[..];
        let max = self.rules.max_version;
        match (step, tx.tx_type) {
            // `IsTriviallyValid`: the mode, the operator reward, the type, the reason.
            (3, TxType::ProviderRegister) => reencoded(bytes, |p: &mut ProRegTx| p.mode = 1),
            (3, TxType::ProviderUpdateRegistrar) => {
                reencoded(bytes, |p: &mut ProUpRegTx| p.mode = 1)
            }
            (4, TxType::ProviderRegister) => {
                reencoded(bytes, |p: &mut ProRegTx| p.operator_reward = 10_001)
            }
            (5, TxType::ProviderUpdateRevoke) => reencoded(bytes, |p: &mut ProUpRevTx| {
                p.reason = RevocationReason::from_base(4);
            }),
            // An undefined type, on a payload whose layout it does not change (no platform
            // fields; a ProUpServTx carries its type from version 2).
            (6, TxType::ProviderRegister) => try_reencoded(bytes, |p: &mut ProRegTx| {
                (p.mn_type == MnType::Regular).then_some(())?;
                p.mn_type = MnType::from_base(2);
                Some(())
            }),
            (6, TxType::ProviderUpdateService) => try_reencoded(bytes, |p: &mut ProUpServTx| {
                (p.mn_type == MnType::Regular && p.version >= ProTxVersion::BasicBls)
                    .then_some(())?;
                p.mn_type = MnType::from_base(2);
                Some(())
            }),
            // A masternode the list does not hold.
            (7, TxType::ProviderUpdateService) => reencoded(bytes, |p: &mut ProUpServTx| {
                p.pro_tx_hash = TxHash::from_lendian(self.fresh(0x66));
            }),
            // A masternode of shares (`bad-protx-shared-mn`), which the generator never updates.
            (7, TxType::ProviderUpdateRegistrar) if arm % 2 == 1 => {
                try_reencoded(bytes, |p: &mut ProUpRegTx| {
                    let shared = tip.list.iter().find(|d| d.cold.is_shared())?;
                    p.pro_tx_hash = TxHash::from_lendian(*shared.pro_tx_hash.as_bytes());
                    Some(())
                })
            }
            (7, TxType::ProviderUpdateRegistrar) => reencoded(bytes, |p: &mut ProUpRegTx| {
                p.pro_tx_hash = TxHash::from_lendian(self.fresh(0x66));
            }),
            (7, TxType::ProviderUpdateRevoke) => reencoded(bytes, |p: &mut ProUpRevTx| {
                p.pro_tx_hash = TxHash::from_lendian(self.fresh(0x66));
            }),
            // An internal collateral's signature, which must be empty.
            (8, TxType::ProviderRegister) => try_reencoded(bytes, |p: &mut ProRegTx| {
                (*p.collateral_hash.as_bytes() == [0; 32]).then_some(())?;
                p.vch_sig = CollateralSig::decode(&mut &[1, 1][..]).ok()?;
                Some(())
            }),
            // Under V24 a step down to the legacy scheme (the generator's states are BasicBLS
            // from V19); below the maximum, a version above it (`GetMax`'s V19 arm included).
            (9, TxType::ProviderUpdateRevoke) if self.rules.v24 => {
                try_reencoded(bytes, |p: &mut ProUpRevTx| {
                    p.version = ProTxVersion::LegacyBls;
                    p.sig = null_sig(1)?;
                    Some(())
                })
            }
            // The registrar's, with a fresh operator key of the legacy scheme, which the version
            // reads (`bad-protx-version-downgrade`).
            (9, TxType::ProviderUpdateRegistrar) if self.rules.v24 => {
                reencoded(bytes, |p: &mut ProUpRegTx| {
                    p.version = ProTxVersion::LegacyBls;
                    p.pub_key_operator = operator_key_from(&self.fresh(0x62), BlsSchemeKind::Basic);
                    p.pub_key_operator.set_scheme(scheme(1));
                })
            }
            (10, TxType::ProviderUpdateRevoke) if max < 3 => {
                try_reencoded(bytes, |p: &mut ProUpRevTx| {
                    p.version = ProTxVersion::try_from(max + 1).ok()?;
                    p.sig = null_sig(max + 1)?;
                    Some(())
                })
            }
            // Under V24 a legacy registration (`bad-protx-version-disallowed`).
            (11, TxType::ProviderRegister) if self.rules.v24 => {
                reencoded(bytes, |p: &mut ProRegTx| p.version = ProTxVersion::LegacyBls)
            }
            // Under V24 a version-2 service update, its first address legacy, sent to an Evo
            // (`bad-protx-version-disallowed`), or to a regular node whose state is version 3,
            // which Core accepts (its legacy address migrates).
            (12, TxType::ProviderUpdateService) if self.rules.v24 && arm % 2 == 1 => {
                let evo = arm == 1;
                let d =
                    tip.list.iter().find(|d| d.is_evo() == evo && (evo || d.cold.version == 3))?;
                try_reencoded(bytes, |p: &mut ProUpServTx| {
                    p.version = ProTxVersion::BasicBls;
                    p.mn_type = mn_type(evo);
                    p.pro_tx_hash = TxHash::from_lendian(*d.pro_tx_hash.as_bytes());
                    p.net_info = net_info(*services(&p.net_info).first()?, 2);
                    (p.platform_node_id, p.platform_p2p_port, p.platform_http_port) = if evo {
                        (Some(d.cold.platform_node_id), Some(26_656), Some(443))
                    } else {
                        (None, None, None)
                    };
                    p.sig = null_sig(2)?;
                    Some(())
                })
            }
            // Under V24 a service update's step down to the legacy scheme, on a regular node (a
            // version-1 payload carries no type).
            (12, TxType::ProviderUpdateService) if self.rules.v24 => {
                try_reencoded(bytes, |p: &mut ProUpServTx| {
                    (p.version == ProTxVersion::BasicBls && p.mn_type == MnType::Regular)
                        .then_some(())?;
                    p.version = ProTxVersion::LegacyBls;
                    p.sig = null_sig(1)?;
                    Some(())
                })
            }
            // An extended entry Core reads as neither address nor domain (`bad-protx-netinfo-bad`,
            // since rc.4 from `CheckNetInfo`'s Malformed at the network-fields step, CE4-03).
            (13, TxType::ProviderRegister) if arm % 2 == 1 => {
                try_reencoded(bytes, |p: &mut ProRegTx| invalid_entry(&mut p.net_info))
            }
            (13, TxType::ProviderUpdateService) if arm % 2 == 1 => {
                try_reencoded(bytes, |p: &mut ProUpServTx| invalid_entry(&mut p.net_info))
            }
            // No address (`bad-protx-netinfo-empty`), in the payload's kind.
            (13, TxType::ProviderUpdateService) => reencoded(bytes, |p: &mut ProUpServTx| {
                p.net_info = empty_net_info(&p.net_info);
            }),
            // The addresses of another masternode of the tip's list, of the payload's kind and
            // the node's type (`bad-protx-dup-netinfo-entry`, against the tip's list).
            (14, TxType::ProviderRegister) => try_reencoded(bytes, |p: &mut ProRegTx| {
                let mn_type = RawMnType(p.mn_type.to_base());
                p.net_info = held_net_info(tip, &p.net_info, mn_type, &[0; 32])?;
                Some(())
            }),
            (14, TxType::ProviderUpdateService) => try_reencoded(bytes, |p: &mut ProUpServTx| {
                let mn_type = RawMnType(p.mn_type.to_base());
                let own = *p.pro_tx_hash.as_bytes();
                p.net_info = held_net_info(tip, &p.net_info, mn_type, &own)?;
                Some(())
            }),
            // An Evo registration of the legacy version (`bad-protx-evo-version`), of a payload
            // whose addresses that version can carry.
            (15, TxType::ProviderRegister) => try_reencoded(bytes, |p: &mut ProRegTx| {
                (p.version < ProTxVersion::ExtAddr).then_some(())?;
                p.version = ProTxVersion::LegacyBls;
                p.mn_type = MnType::Evo;
                p.platform_node_id.get_or_insert(PlatformNodeId::from(twenty(self.fresh(0x67))));
                p.platform_p2p_port.get_or_insert(26656);
                p.platform_http_port.get_or_insert(443);
                Some(())
            }),
            _ => None,
        }
    }

    fn fresh(&mut self, salt: u8) -> [u8; 32] {
        self.fresh += 1;
        let [a, b, ..] = self.fresh.to_le_bytes();
        spread(b, a, salt)
    }

    /// A fresh key id, never one the list or an earlier draw holds.
    fn key_id(&mut self) -> [u8; 20] {
        twenty(self.fresh(0x61))
    }

    /// A payload version the chain accepts, never below `state` (no downgrade under V24).
    fn version(&mut self, state: u16) -> u16 {
        let drawn = if self.rules.max_version == 3 && self.r.next() % 2 == 1 {
            3
        } else {
            2
        };
        drawn.max(state).min(self.rules.max_version)
    }

    /// A value of one kind, which the block then holds: fresh, or (one in eight each) one the
    /// block took or one an earlier block freed.
    fn draw<T: Clone>(
        &mut self,
        block: &mut Block,
        pool: impl Fn(&mut Values) -> &mut Vec<T>,
        fresh: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let mode = self.r.next() % 8;
        let taken = pool(&mut block.taken);
        let value = if mode == 0 && !taken.is_empty() {
            taken[usize::from(self.r.next()) % taken.len()].clone()
        } else {
            let freed = pool(&mut self.freed);
            if mode == 1 && !freed.is_empty() {
                block.retaken += 1;
                freed[usize::from(self.r.next()) % freed.len()].clone()
            } else {
                fresh(self)
            }
        };
        pool(&mut block.taken).push(value.clone());
        value
    }

    fn owner(&mut self, block: &mut Block) -> [u8; 20] {
        self.draw(block, |v| &mut v.owners, Self::key_id)
    }

    fn operator(&mut self, block: &mut Block, version: u16) -> OperatorKey {
        let mut key = self.draw(
            block,
            |v| &mut v.operators,
            |w| operator_key_from(&w.fresh(0x62), BlsSchemeKind::Basic),
        );
        key.set_scheme(scheme(version));
        key
    }

    fn service(&mut self, block: &mut Block) -> Service {
        let port = self.rules.port;
        self.draw(
            block,
            |v| &mut v.services,
            |w| {
                w.fresh += 1;
                let [a, b, ..] = w.fresh.to_le_bytes();
                ([3, 1 + b, a, 9], port)
            },
        )
    }

    fn platform_id(&mut self, block: &mut Block) -> [u8; 20] {
        self.draw(block, |v| &mut v.platform_ids, |w| twenty(w.fresh(0x63)))
    }

    /// A fresh input, as every transaction has one (its inputs hash covers it).
    fn input(&mut self) -> TxIn {
        spend_of(OutPoint {
            hash: TxHash::from_lendian(self.fresh(0x64)),
            index: 0,
        })
    }

    /// A listed masternode: one the block updated already (one in eight), else any that
    /// `eligible` allows.
    fn target(&mut self, tip: &Tip, block: &Block, eligible: impl Fn(&Dmn) -> bool) -> Option<Dmn> {
        let mns: Vec<&Dmn> = tip.list.iter().map(|d| &**d).filter(|d| eligible(d)).collect();
        if self.r.next().is_multiple_of(8)
            && let Some(again) = self.r.pick(&block.updated)
            && let Some(d) = mns.iter().find(|d| d.pro_tx_hash.as_bytes() == again)
        {
            return Some((*d).clone());
        }
        self.r.pick(&mns).map(|d| (*d).clone())
    }

    /// The platform fields of a payload below ExtAddr: a node id, and ports neither Core's nor
    /// each other's (`bad-protx-platform-dup-ports`).
    fn platform_fields(
        &mut self,
        block: &mut Block,
        evo: bool,
    ) -> (Option<PlatformNodeId>, Option<u16>, Option<u16>) {
        if !evo {
            return (None, None, None);
        }
        let id = PlatformNodeId::from(self.platform_id(block));
        (Some(id), Some(26_656), Some(443))
    }

    /// Ensures the view holds a coin at `outpoint` paying `amount` (when given) to a fresh key;
    /// a coin there already stays unless its amount differs.
    fn coin(&mut self, outpoint: OutPoint, amount: Option<i64>, height: i32) -> Option<()> {
        let key = self.key_id();
        let coins = match &mut self.ctx.coins {
            CoinsView::AtTip(coins) | CoinsView::Unchecked(coins) => coins,
        };
        let at = (*outpoint.hash.as_bytes(), outpoint.index);
        if let Some(c) = coins.iter().find(|c| (c.txid, c.vout) == at)
            && amount.is_none_or(|a| a == c.amount)
        {
            return Some(());
        }
        coins.retain(|c| (c.txid, c.vout) != at);
        coins.push(ContextCoin {
            txid: at.0,
            vout: at.1,
            height: u32::try_from(height).ok()?,
            coinbase: false,
            amount: amount.unwrap_or(COIN),
            script_pubkey: p2pkh(key),
        });
        Some(())
    }

    fn pro_reg(&mut self, tip: &Tip, block: &mut Block) -> Option<Transaction> {
        let version = self.version(2.min(self.rules.max_version));
        let mut evo = self.rules.evo && self.r.next().is_multiple_of(4);
        let input = self.input();
        let mut outputs = vec![];
        let mut own_service = None;
        let collateral = match self.r.next() % 3 {
            0 => None,
            1 => {
                let outpoint = OutPoint {
                    hash: TxHash::from_lendian(self.fresh(0x65)),
                    index: 0,
                };
                self.coin(outpoint, Some(collateral_amount(evo)), tip.height)?;
                Some(outpoint)
            }
            _ => {
                // A replacement keeps the masternode's type and may keep its addresses.
                let d = self.target(tip, block, |d| !d.cold.is_shared())?;
                evo = d.is_evo();
                if evo && !self.rules.evo {
                    return None;
                }
                self.coin(d.collateral, Some(collateral_amount(evo)), tip.height)?;
                own_service = services(&d.cold.net_info).first().copied();
                Some(d.collateral)
            }
        };
        let (collateral_hash, collateral_index) = match collateral {
            Some(c) => (c.hash, c.index),
            None => {
                let key = self.key_id();
                outputs.push(TxOut {
                    value: collateral_amount(evo),
                    script_pubkey: p2pkh(key).into(),
                });
                (TxHash::from_lendian([0; 32]), 0)
            }
        };
        let owner = self.owner(block);
        let voting = if self.rules.owner_is_voting {
            owner
        } else {
            self.key_id()
        };
        let operator = self.operator(block, version);
        let service = match own_service {
            Some(s) if self.r.next().is_multiple_of(2) => s,
            _ => self.service(block),
        };
        let (platform_node_id, platform_p2p_port, platform_http_port) =
            self.platform_fields(block, evo);
        let (script_payout, payouts) = payee_fields(version, p2pkh(self.key_id()));
        let operator_reward = if self.r.next().is_multiple_of(2) {
            0
        } else {
            u16::from_le_bytes([self.r.next(), self.r.next()]) % 10_001
        };
        let inputs = vec![input];
        let p = ProRegTx {
            version: ProTxVersion::try_from(version).ok()?,
            mn_type: mn_type(evo),
            mode: 0,
            collateral_hash,
            collateral_index,
            net_info: net_info(service, version),
            key_id_owner: From::from(owner),
            pub_key_operator: operator,
            key_id_voting: From::from(voting),
            operator_reward,
            script_payout: script_payout.into(),
            payouts,
            shares: vec![],
            join_sigs: vec![],
            early_period_blocks: 0,
            early_penalty: 0,
            inputs_hash: inputs_hash(&inputs),
            platform_node_id,
            platform_p2p_port,
            platform_http_port,
            vch_sig: CollateralSig::default(),
        };
        let tx = special(TxType::ProviderRegister, inputs, outputs, encoded(&p));
        let txid = decode_tx(&mut &encoded(&tx)[..]).ok()?.into_value().txid();
        block.collaterals.push(collateral.unwrap_or(OutPoint {
            hash: txid,
            index: 0,
        }));
        Some(tx)
    }

    fn pro_up_serv(&mut self, tip: &Tip, block: &mut Block) -> Option<Transaction> {
        let evo_ok = self.rules.evo;
        let d = self.target(tip, block, |d| evo_ok || !d.is_evo())?;
        let version = self.version(state_version(&d));
        let own = services(&d.cold.net_info).first().copied();
        let service = match own {
            Some(s) if self.r.next().is_multiple_of(4) => s,
            _ => self.service(block),
        };
        let (mut platform_node_id, platform_p2p_port, platform_http_port) =
            self.platform_fields(block, d.is_evo());
        if d.is_evo() && !d.cold.platform_node_id.is_null() && self.r.next().is_multiple_of(2) {
            platform_node_id = Some(d.cold.platform_node_id);
        }
        let script_operator_payout = if d.operator_reward != 0 && self.r.next().is_multiple_of(4) {
            p2pkh(self.key_id())
        } else {
            vec![]
        };
        let inputs = vec![self.input()];
        let p = ProUpServTx {
            version: ProTxVersion::try_from(version).ok()?,
            mn_type: mn_type(d.is_evo()),
            pro_tx_hash: TxHash::from_lendian(*d.pro_tx_hash.as_bytes()),
            net_info: net_info(service, version),
            script_operator_payout: script_operator_payout.into(),
            inputs_hash: inputs_hash(&inputs),
            platform_node_id,
            platform_p2p_port,
            platform_http_port,
            sig: null_sig(version)?,
        };
        block.updated.push(*d.pro_tx_hash.as_bytes());
        Some(special(TxType::ProviderUpdateService, inputs, vec![], encoded(&p)))
    }

    fn pro_up_reg(&mut self, tip: &Tip, block: &mut Block) -> Option<Transaction> {
        let d = self.target(tip, block, |d| !d.cold.is_shared())?;
        // Not past the state's version: a raise with the key kept is `SetStateVersion`'s
        // migration, which a legacy address refuses (`bad-protx-netinfo-version`).
        let version = state_version(&d).clamp(2, 3).min(self.rules.max_version);
        let null = d.cold.pub_key_operator == OperatorKey::new(BlsSchemeKind::Basic);
        let operator = if null || self.r.next().is_multiple_of(2) {
            self.operator(block, version)
        } else {
            let mut key = d.cold.pub_key_operator.clone();
            key.set_scheme(scheme(version));
            key
        };
        let voting = if self.rules.owner_is_voting {
            *d.cold.key_id_owner.as_bytes()
        } else {
            self.key_id()
        };
        self.coin(d.collateral, None, tip.height)?;
        let (script_payout, payouts) = payee_fields(version, p2pkh(self.key_id()));
        let inputs = vec![self.input()];
        let p = ProUpRegTx {
            version: ProTxVersion::try_from(version).ok()?,
            pro_tx_hash: TxHash::from_lendian(*d.pro_tx_hash.as_bytes()),
            mode: 0,
            pub_key_operator: operator,
            key_id_voting: From::from(voting),
            script_payout: script_payout.into(),
            payouts,
            inputs_hash: inputs_hash(&inputs),
            vch_sig: Default::default(),
        };
        block.updated.push(*d.pro_tx_hash.as_bytes());
        Some(special(TxType::ProviderUpdateRegistrar, inputs, vec![], encoded(&p)))
    }

    fn pro_up_rev(&mut self, tip: &Tip, block: &mut Block) -> Option<Transaction> {
        let d = self.target(tip, block, |_| true)?;
        let version = self.version(state_version(&d));
        let inputs = vec![self.input()];
        let p = ProUpRevTx {
            version: ProTxVersion::try_from(version).ok()?,
            pro_tx_hash: TxHash::from_lendian(*d.pro_tx_hash.as_bytes()),
            reason: RevocationReason::from_base(u16::from(self.r.next() % 4)),
            inputs_hash: inputs_hash(&inputs),
            sig: null_sig(version)?,
        };
        block.updated.push(*d.pro_tx_hash.as_bytes());
        Some(special(TxType::ProviderUpdateRevoke, inputs, vec![], encoded(&p)))
    }

    /// A spend: of a listed collateral (shared ones too: `bad-shared-collateral-spend`), of one
    /// registered earlier in the block, or of a fresh coin.
    fn spend(&mut self, tip: &Tip, block: &Block) -> Option<Transaction> {
        let spent = match self.r.next() % 3 {
            0 => *self.r.pick(&block.collaterals)?,
            1 => {
                let collaterals: Vec<OutPoint> = tip.list.iter().map(|d| d.collateral).collect();
                *self.r.pick(&collaterals)?
            }
            _ => return Some(special(TxType::Spend, vec![self.input()], vec![], vec![])),
        };
        Some(special(TxType::Spend, vec![spend_of(spent)], vec![], vec![]))
    }

    /// A QcTx from a template of the case's chain: at the block's height, on the template's
    /// quorum type's last base at or below the tip (`base % dkgInterval == quorumIndex`), with
    /// members from the tip's list supplied, every one signing and some (but `minSize`) valid.
    fn qc(&mut self, tip: &Tip) -> Option<Transaction> {
        let mut p = self.r.pick(&crafted().templates[usize::from(self.case.v24)].qc)?.clone();
        let c = &mut p.commitment;
        // The crafted commitments are invalid ones; Core's `Verify` wants valid keys (BasicBLS).
        if c.version < 3 {
            return None;
        }
        let (pk, sig) = quorum_keys();
        c.quorum_public_key = pk.clone();
        c.quorum_sig = sig.clone();
        c.members_sig = sig.clone();
        c.quorum_vvec_hash = QuorumVvecHash::from_lendian(spread(0, 0, 0x66));
        let llmq = self.case.params.consensus.llmq(c.llmq_type)?;
        let interval = i32::from(llmq.dkg_interval).max(1);
        let index = if llmq.use_rotation {
            i32::from(self.r.next()) % i32::from(llmq.signing_active_quorum_count).max(1)
        } else {
            0
        };
        let mut base = tip.height / interval * interval + index;
        if base > tip.height {
            base -= interval;
        }
        // A base has a parent (`CFinalCommitment::Verify`) and is in the context.
        if base < 1 {
            return None;
        }
        let header = self.ctx.headers.get(usize::try_from(base - self.ctx.first_height).ok()?)?;
        let hash = *decode_header(&mut &header[..]).ok()?.value().hash().as_bytes();
        let mns: Vec<_> = tip.list.iter().collect();
        let size = usize::from(llmq.size);
        let count = size.min(mns.len());
        let min = usize::from(llmq.min_size);
        if count < min {
            return None;
        }
        let start = usize::from(self.r.next());
        let members: Vec<Vec<u8>> =
            (0..count).map(|i| dmn_bytes(mns[(start + i) % mns.len()])).collect();
        let llmq_type = c.llmq_type.to_base();
        if !self.ctx.quorums.iter().any(|q| q.llmq_type == llmq_type && q.quorum_hash == hash) {
            self.ctx.quorums.push(QuorumMembers {
                llmq_type,
                quorum_hash: hash,
                members,
            });
        }
        let bytes = size.div_ceil(8);
        let mut valid = vec![0u8; bytes];
        let mut invalid = usize::from(self.r.next()) % (count - min + 1);
        for i in 0..count {
            let keep = invalid == 0 || self.r.next().is_multiple_of(2);
            if keep {
                valid[i / 8] |= 1 << (i % 8);
            } else {
                invalid -= 1;
            }
        }
        let mut signers = vec![0u8; bytes];
        for i in 0..count {
            signers[i / 8] |= 1 << (i % 8);
        }
        c.quorum_hash = dash_primitives::QuorumHash::from_lendian(hash);
        c.quorum_index = matches!(c.version, 2 | 4).then_some(i16::try_from(index).ok()?);
        c.valid_members.num_bits = size as u64;
        c.valid_members.data = valid;
        c.signers.num_bits = size as u64;
        c.signers.data = signers;
        let mut payload = encoded(&p);
        payload.get_mut(2..6)?.copy_from_slice(&(tip.height + 1).to_le_bytes());
        Some(special(TxType::QuorumCommitment, vec![], vec![], payload))
    }

    fn pro_up_share(&mut self, tip: &Tip) -> Option<Transaction> {
        let d = self.shared(tip)?;
        let inputs = vec![self.input()];
        let p = ProUpShareTx {
            version: 1,
            pro_tx_hash: TxHash::from_lendian(*d.pro_tx_hash.as_bytes()),
            share_index: u16::from(self.r.next()) % u16::try_from(d.cold.shares.len()).ok()?,
            script_reward: p2pkh(self.key_id()).into(),
            inputs_hash: inputs_hash(&inputs),
            vch_sig: vec![0; 65],
        };
        Some(special(TxType::ProviderUpdateShare, inputs, vec![], encoded(&p)))
    }

    fn pro_up_shared_reg(&mut self, tip: &Tip, block: &mut Block) -> Option<Transaction> {
        let d = self.shared(tip)?;
        let operator = self.operator(block, 2);
        let inputs = vec![self.input()];
        let sig = CompactSig::decode(&mut &[0; COMPACT_SIG_LEN][..]).ok()?;
        let p = ProUpSharedRegTx {
            version: 1,
            pro_tx_hash: TxHash::from_lendian(*d.pro_tx_hash.as_bytes()),
            pub_key_operator: operator,
            key_id_voting: From::from(self.key_id()),
            inputs_hash: inputs_hash(&inputs),
            sigs: vec![sig; d.cold.shares.len()],
        };
        block.updated.push(*d.pro_tx_hash.as_bytes());
        Some(special(TxType::ProviderUpdateSharedRegistrar, inputs, vec![], encoded(&p)))
    }

    fn shared(&mut self, tip: &Tip) -> Option<Dmn> {
        let shared: Vec<&Dmn> =
            tip.list.iter().map(|d| &**d).filter(|d| d.cold.is_shared()).collect();
        self.r.pick(&shared).map(|d| (*d).clone())
    }
}

/// The context's tip list and height.
fn tip_of(ctx: &Context) -> Option<Tip> {
    let CoreList::Supplied(bytes) = &ctx.mn_list else {
        return None;
    };
    Some(Tip {
        list: core_disk::decode_list_s3(&mut &bytes[..]).ok()?,
        height: ctx.first_height + i32::try_from(ctx.headers.len()).ok()? - 1,
    })
}

/// A masternode state's version as a payload version.
fn state_version(d: &Dmn) -> u16 {
    u16::try_from(d.cold.version).unwrap_or(1)
}

fn mn_type(evo: bool) -> MnType {
    if evo {
        MnType::Evo
    } else {
        MnType::Regular
    }
}

/// A payload's payout script and payout list: the script below version 3, else the list.
fn payee_fields(version: u16, payee: Vec<u8>) -> (Vec<u8>, Vec<PayoutShare>) {
    if version >= 3 {
        (Vec::new(), payout(&payee).into_iter().collect())
    } else {
        (payee, vec![])
    }
}

/// `GetMnType(nType).collat_amount`.
fn collateral_amount(evo: bool) -> i64 {
    if evo {
        4_000 * COIN
    } else {
        1_000 * COIN
    }
}

/// `CalcTxInputsHash` of a transaction with `inputs`.
fn inputs_hash(inputs: &[TxIn]) -> InputsHash {
    InputsHash::from_lendian(inputs_hash_of(inputs))
}

/// A valid quorum public key and signature (BasicBLS) for generated commitments, whose
/// signatures are not checked.
#[allow(clippy::expect_used)] // fixed key material
fn quorum_keys() -> &'static (BlsEagerPk, BlsEagerSig) {
    static KEYS: OnceLock<(BlsEagerPk, BlsEagerSig)> = OnceLock::new();
    KEYS.get_or_init(|| {
        let sk = BlsSecretKey::<BlsScIetf>::from_ikm(&spread(0, 0, 0x67)).expect("key material");
        let source = BlsSchemeSource::Version(BlsSchemeKind::Basic);
        let pk = BlsEagerPk::decode(&sk.public_key().to_bytes(), source).expect("a public key");
        let sig = BlsEagerSig::decode(&sk.sign_message(b"EV2b D54").to_bytes(), source)
            .expect("a signature");
        (pk, sig)
    })
}

/// The operator key scheme of a payload of `version`.
fn scheme(version: u16) -> BlsSchemeKind {
    if version == 1 {
        BlsSchemeKind::Legacy
    } else {
        BlsSchemeKind::Basic
    }
}

/// A null BLS signature of a payload of `version` (never checked here).
fn null_sig(version: u16) -> Option<BlsEagerSig> {
    BlsEagerSig::decode(&[0; 96], BlsSchemeSource::Version(scheme(version))).ok()
}

/// A NetInfo of `like`'s kind without an address.
fn empty_net_info(like: &NetInfo) -> NetInfo {
    match like {
        NetInfo::Legacy(_) => NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1([0; 16]),
            port: 0,
        })),
        NetInfo::Extended(v2) => NetInfo::Extended(NetInfoV2 {
            version: v2.version,
            entries: Vec::new(),
        }),
    }
}

/// `net_info`'s first extended entry made one Core reads as invalid (its tag byte alone);
/// `None` for a legacy or empty NetInfo.
fn invalid_entry(net_info: &mut NetInfo) -> Option<()> {
    let NetInfo::Extended(v2) = net_info else {
        return None;
    };
    *v2.entries.iter_mut().flat_map(|(_, group)| group).next()? = NIEntry::Invalid;
    Some(())
}

/// The NetInfo of the first masternode of the tip's list, other than `own`, of `mn_type` with
/// addresses of `like`'s kind.
fn held_net_info(tip: &Tip, like: &NetInfo, mn_type: RawMnType, own: &[u8; 32]) -> Option<NetInfo> {
    let extended = matches!(like, NetInfo::Extended(_));
    tip.list
        .iter()
        .filter(|d| d.pro_tx_hash.as_bytes() != own && d.mn_type == mn_type)
        .map(|d| &*d.cold.net_info)
        .find(|n| matches!(n, NetInfo::Extended(_)) == extended && !dash_dml::netinfo::is_empty(n))
        .cloned()
}

/// The payload `bytes` read as a `P`, changed by `edit` and encoded again; `None` when it does not
/// read as one, or `edit` finds it not to apply.
fn try_reencoded<P: BaseCodec>(
    bytes: &[u8],
    edit: impl FnOnce(&mut P) -> Option<()>,
) -> Option<Vec<u8>> {
    let mut payload = P::decode(&mut &bytes[..]).ok()?;
    edit(&mut payload)?;
    Some(encoded(&payload))
}

/// [`try_reencoded`] for an edit that always applies.
fn reencoded<P: BaseCodec>(bytes: &[u8], edit: impl FnOnce(&mut P)) -> Option<Vec<u8>> {
    try_reencoded(bytes, |p| {
        edit(p);
        Some(())
    })
}

/// A special transaction: version 3, `inputs`, `outputs`, the payload.
fn special(
    tx_type: TxType,
    inputs: Vec<TxIn>,
    outputs: Vec<TxOut>,
    payload: Vec<u8>,
) -> Transaction {
    Transaction {
        version: 3,
        tx_type,
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: payload,
    }
}

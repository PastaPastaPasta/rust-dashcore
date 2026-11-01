//! Dash Core v24 wire vectors.
//!
//! `data/core_v24_regtest.json` holds transactions and blocks produced by dashd v24.0.0-rc.3 on
//! regtest while running Core's own functional tests (asset locks, shared and multi-payout
//! masternodes, ProTx versions, extended addresses, Evo nodes), together with Core's verbose JSON
//! for them. One transaction is kept per distinct wire layout. See `contrib/core-vectors` for how
//! the file is generated.
//!
//! Every transaction must decode into its typed payload, re-encode to the same bytes and hash to
//! Core's txid; the payload fields must match Core's JSON. Every block must re-encode to the same
//! bytes, hash to Core's block hash and commit to its transactions through the merkle root.

use std::net::{Ipv4Addr, Ipv6Addr};

use dashcore::address::Payload;
use dashcore::consensus::Decodable;
use dashcore::consensus::{deserialize, serialize};
use dashcore::hashes::{Hash, sha256d};
use dashcore::sign_message::{recover_pubkey, signed_msg_hash};
use dashcore::sml::masternode_list_entry::MasternodeNetInfo;
use dashcore::sml::masternode_list_entry::net_info::{
    Bip155Network, ExtNetInfo, NetInfoEntry, NetInfoPurpose,
};
use dashcore::transaction::special_transaction::provider_registration::{
    MasternodePayoutShare, ProviderRegistrationPayload, SharedCollateral,
};
use dashcore::transaction::special_transaction::provider_update_service::ProviderUpdateServicePayload;
use dashcore::transaction::special_transaction::{
    SpecialTransactionBasePayloadEncodable, TransactionPayload,
};
use dashcore::{Address, Block, Network, PubkeyHash, ScriptBuf, Transaction, VarInt};
use hex_conservative::DisplayHex;
use serde_json::Value;

const VECTORS: &str = include_str!("data/core_v24_regtest.json");
const MNLISTDIFF_VECTORS: &str = include_str!("data/core_v24_regtest_mnlistdiff.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("valid vector file")
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_else(|| panic!("missing string {key} in {v}"))
}

fn bytes_of(v: &Value, key: &str) -> Vec<u8> {
    hex_conservative::decode_to_vec(str_of(v, key)).unwrap_or_else(|e| panic!("{key}: {e}"))
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or_else(|| panic!("missing number {key} in {v}"))
}

/// Core's `uint256::ToString()` in internal byte order, how this crate displays the coinbase
/// merkle roots.
fn internal_order(core_hex: &str) -> String {
    let mut bytes = hex_conservative::decode_to_vec(core_hex).unwrap();
    bytes.reverse();
    bytes.to_lower_hex_string()
}

fn address(hash: &PubkeyHash) -> String {
    Address::new(Network::Regtest, Payload::PubkeyHash(*hash)).to_string()
}

fn script_address(script: &ScriptBuf) -> String {
    Address::from_script(script, Network::Regtest).expect("standard script").to_string()
}

/// RFC 4648 base32 (lowercase, unpadded) as used in .onion and .b32.i2p names.
fn base32_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let value = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => panic!("invalid base32 character {c}"),
        };
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

/// Checks one decoded netInfo entry against Core's `host:port` rendering.
fn check_entry(entry: &NetInfoEntry, rendered: &str) {
    if rendered == "[invalid entry]" {
        assert_eq!(*entry, NetInfoEntry::Invalid);
        return;
    }
    let (host, port) = rendered.rsplit_once(':').expect("host:port");
    let port: u16 = port.parse().unwrap();
    match entry {
        NetInfoEntry::Service {
            network,
            addr,
            port: entry_port,
        } => {
            assert_eq!(*entry_port, port, "{rendered}");
            match network {
                Bip155Network::Ipv4 => {
                    assert_eq!(host.parse::<Ipv4Addr>().unwrap().octets().to_vec(), *addr)
                }
                Bip155Network::Ipv6 | Bip155Network::Cjdns => {
                    let ip = host.trim_start_matches('[').trim_end_matches(']');
                    assert_eq!(ip.parse::<Ipv6Addr>().unwrap().octets().to_vec(), *addr)
                }
                // The name is base32(pubkey ‖ checksum ‖ version); the address is the pubkey.
                Bip155Network::TorV3 => {
                    let name = host.strip_suffix(".onion").expect("onion name");
                    assert_eq!(base32_decode(name)[..32], addr[..])
                }
                Bip155Network::I2p => {
                    let name = host.strip_suffix(".b32.i2p").expect("i2p name");
                    assert_eq!(base32_decode(name), *addr)
                }
            }
        }
        NetInfoEntry::Domain {
            host: entry_host,
            port: entry_port,
        } => {
            assert_eq!((entry_host.as_str(), *entry_port), (host, port), "{rendered}");
        }
        NetInfoEntry::Invalid | NetInfoEntry::Malformed(_) | NetInfoEntry::UnknownType(_) => {
            panic!("Core rendered an invalid entry as {rendered}")
        }
    }
}

/// Checks a decoded `ExtNetInfo` against Core's `addresses` object, which renders the map Core
/// holds: [`ExtNetInfo::canonical`].
fn check_net_info(net_info: &ExtNetInfo, addresses: &Value) {
    let addresses = addresses.as_object().expect("addresses object");
    let net_info = net_info.canonical();
    assert_eq!(net_info.version, 1);
    assert_eq!(net_info.purposes.len(), addresses.len(), "{addresses:?}");
    for (purpose, entries) in &net_info.purposes {
        let key = match purpose {
            NetInfoPurpose::CoreP2P => "core_p2p",
            NetInfoPurpose::PlatformP2P => "platform_p2p",
            NetInfoPurpose::PlatformHttps => "platform_https",
            NetInfoPurpose::Unknown(_) => unreachable!("rc.4 cannot read an unknown purpose"),
        };
        let rendered = addresses[key].as_array().unwrap_or_else(|| panic!("no {key}"));
        assert_eq!(entries.len(), rendered.len());
        for (entry, rendered) in entries.iter().zip(rendered) {
            check_entry(entry, rendered.as_str().unwrap());
        }
    }
}

/// Checks a legacy service address against Core's `addresses` object.
fn check_service_address(ip_port: Option<(std::net::IpAddr, u16)>, addresses: &Value) {
    let addresses = addresses.as_object().expect("addresses object");
    match ip_port {
        Some((ip, port)) if !ip.is_unspecified() => {
            let rendered = addresses["core_p2p"][0].as_str().unwrap();
            assert_eq!(rendered, format!("{ip}:{port}"));
        }
        _ => assert!(addresses.values().all(|v| v.as_array().is_none_or(Vec::is_empty))),
    }
}

/// The extended addresses of a version 3 payload or SML entry.
fn ext(service_address: &MasternodeNetInfo) -> Option<&ExtNetInfo> {
    match service_address {
        MasternodeNetInfo::Extended(net_info) => Some(net_info),
        MasternodeNetInfo::Legacy(_) => None,
    }
}

/// The collateral shares of a shared ProRegTx; a version 3 one without shares is a regular one.
fn shared(p: &ProviderRegistrationPayload) -> Option<&SharedCollateral> {
    p.shared_collateral.as_ref().filter(|c| !c.shares.is_empty())
}

fn check_payouts(payouts: &[MasternodePayoutShare], core: &Value) {
    let core = core.as_array().expect("payouts array");
    assert_eq!(payouts.len(), core.len());
    for (payout, core) in payouts.iter().zip(core) {
        assert_eq!(payout.script_payout.to_hex_string(), str_of(core, "script"));
        assert_eq!(script_address(&payout.script_payout), str_of(core, "address"));
        assert_eq!(payout.reward as u64, u64_of(core, "reward"));
    }
}

fn check_payload(tx: &Transaction, core: &Value) {
    let payload = tx.special_transaction_payload.clone().expect("special transaction");
    match payload {
        TransactionPayload::ProviderRegistrationPayloadType(p) => {
            let c = &core["proRegTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.masternode_type as u64, u64_of(c, "type"));
            assert_eq!(p.collateral_outpoint.txid.to_string(), str_of(c, "collateralHash"));
            assert_eq!(p.collateral_outpoint.vout as u64, u64_of(c, "collateralIndex"));
            assert_eq!(p.operator_public_key.to_string(), str_of(c, "pubKeyOperator"));
            assert_eq!(address(&p.voting_key_hash), str_of(c, "votingAddress"));
            // Core prints the operator reward in percent.
            let reward = c["operatorReward"].as_f64().unwrap();
            assert_eq!(p.operator_reward as u64, (reward * 100.0).round() as u64);
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
            if p.version >= 3 {
                check_net_info(ext(&p.service_address).unwrap(), &c["addresses"]);
                let payouts = p.payouts.as_deref().unwrap();
                if payouts.is_empty() {
                    assert!(c.get("payouts").is_none());
                } else {
                    check_payouts(payouts, &c["payouts"]);
                }
            } else {
                let MasternodeNetInfo::Legacy(addr) = p.service_address else {
                    panic!("version {} with extended addresses", p.version);
                };
                assert!(p.payouts.is_none() && p.shared_collateral.is_none());
                check_service_address(Some((addr.ip(), addr.port())), &c["addresses"]);
                assert_eq!(script_address(&p.script_payout), str_of(c, "payoutAddress"));
            }
            if let Some(terms) = shared(&p) {
                assert!(c.get("ownerAddress").is_none());
                let shares = c["shares"].as_array().unwrap();
                assert_eq!(terms.shares.len(), shares.len());
                assert_eq!(terms.join_sigs.len(), shares.len());
                for (share, core) in terms.shares.iter().zip(shares) {
                    assert_eq!(share.amount as u64, u64_of(core, "amount"));
                    assert_eq!(share.script_refund.to_hex_string(), str_of(core, "refundScript"));
                    // Core pays an empty reward script's rewards to the refund script.
                    let reward = if share.script_reward.is_empty() {
                        &share.script_refund
                    } else {
                        &share.script_reward
                    };
                    assert_eq!(reward.to_hex_string(), str_of(core, "rewardScript"));
                    assert_eq!(address(&share.owner_key_hash), str_of(core, "ownerAddress"));
                }
                assert_eq!(terms.early_period_blocks as u64, u64_of(c, "earlyPeriodBlocks"));
                assert_eq!(terms.early_penalty as u64, u64_of(c, "earlyPenalty"));
            } else {
                assert_eq!(address(&p.owner_key_hash), str_of(c, "ownerAddress"));
                if let Some(terms) = &p.shared_collateral {
                    assert!(terms.join_sigs.is_empty());
                    assert_eq!((terms.early_period_blocks, terms.early_penalty), (0, 0));
                }
            }
            match p.platform_node_id {
                Some(id) => {
                    assert_eq!(id.to_string(), str_of(c, "platformNodeID"));
                    assert_eq!(p.platform_p2p_port.is_some(), p.version < 3);
                }
                None => assert!(c.get("platformNodeID").is_none()),
            }
        }
        TransactionPayload::ProviderUpdateServicePayloadType(p) => {
            let c = &core["proUpServTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.mn_type.unwrap_or(0) as u64, u64_of(c, "type"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
            match &p.service_address {
                MasternodeNetInfo::Extended(net_info) => {
                    assert!(p.version >= 3);
                    check_net_info(net_info, &c["addresses"]);
                }
                MasternodeNetInfo::Legacy(addr) => {
                    assert!(p.version < 3);
                    check_service_address(Some((addr.ip(), addr.port())), &c["addresses"]);
                }
            }
            match c.get("operatorPayoutAddress") {
                Some(addr) => assert_eq!(script_address(&p.script_payout), addr.as_str().unwrap()),
                None => assert!(p.script_payout.is_empty()),
            }
            if let Some(id) = p.platform_node_id {
                assert_eq!(id.to_string(), str_of(c, "platformNodeID"));
            }
        }
        TransactionPayload::ProviderUpdateRegistrarPayloadType(p) => {
            let c = &core["proUpRegTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.operator_public_key.to_string(), str_of(c, "pubKeyOperator"));
            assert_eq!(address(&p.voting_key_hash), str_of(c, "votingAddress"));
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
            match &p.payouts {
                Some(payouts) => check_payouts(payouts, &c["payouts"]),
                None => assert_eq!(script_address(&p.script_payout), str_of(c, "payoutAddress")),
            }
        }
        TransactionPayload::ProviderUpdateRevocationPayloadType(p) => {
            let c = &core["proUpRevTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.reason as u64, u64_of(c, "reason"));
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
        }
        TransactionPayload::CoinbasePayloadType(p) => {
            let c = &core["cbTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.height as u64, u64_of(c, "height"));
            let mn_list_root = internal_order(str_of(c, "merkleRootMNList"));
            assert_eq!(p.merkle_root_masternode_list.to_string(), mn_list_root);
            if p.version >= 2 {
                let quorums_root = internal_order(str_of(c, "merkleRootQuorums"));
                assert_eq!(p.merkle_root_quorums.to_string(), quorums_root);
            }
            if p.version >= 3 {
                assert_eq!(p.best_cl_height.unwrap() as u64, u64_of(c, "bestCLHeightDiff"));
                assert_eq!(p.best_cl_signature.unwrap().to_string(), str_of(c, "bestCLSignature"));
                let balance = c["creditPoolBalance"].as_f64().unwrap();
                assert_eq!(p.asset_locked_amount.unwrap(), (balance * 1e8).round() as u64);
            }
            match p.merkle_root_asset_unlocks {
                Some(root) => {
                    let core_root = internal_order(str_of(c, "merkleRootAssetUnlocks"));
                    assert_eq!(root.to_string(), core_root);
                }
                None => assert!(p.version < 4 && c.get("merkleRootAssetUnlocks").is_none()),
            }
        }
        TransactionPayload::AssetLockPayloadType(p) => {
            let c = &core["assetLockTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            let outputs = c["creditOutputs"].as_array().unwrap();
            assert_eq!(p.credit_outputs.len(), outputs.len());
            for (out, core) in p.credit_outputs.iter().zip(outputs) {
                assert_eq!(out.value, u64_of(core, "valueSat"));
                assert_eq!(out.script_pubkey.to_hex_string(), str_of(&core["scriptPubKey"], "hex"));
            }
        }
        TransactionPayload::AssetUnlockPayloadType(p) => {
            let c = &core["assetUnlockTx"];
            assert_eq!(p.base.version as u64, u64_of(c, "version"));
            assert_eq!(p.base.index, u64_of(c, "index"));
            assert_eq!(p.base.fee as u64, u64_of(c, "fee"));
            assert_eq!(p.request_info.request_height as u64, u64_of(c, "requestedHeight"));
            assert_eq!(p.request_info.quorum_hash.to_string(), str_of(c, "quorumHash"));
            assert_eq!(p.quorum_sig.to_string(), str_of(c, "quorumSig"));
        }
        TransactionPayload::ProviderDissolvePayloadType(p) => {
            let c = &core["proDisTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.actor_index as u64, u64_of(c, "actorIndex"));
            assert_eq!(p.signatures.len() as u64, u64_of(c, "sigCount"));
        }
        TransactionPayload::ProviderUpdateSharePayloadType(p) => {
            let c = &core["proUpShareTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.share_index as u64, u64_of(c, "shareIndex"));
            assert_eq!(script_address(&p.script_reward), str_of(c, "rewardAddress"));
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
        }
        TransactionPayload::ProviderUpdateSharedRegistrarPayloadType(p) => {
            let c = &core["proUpSharedRegTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.pro_tx_hash.to_string(), str_of(c, "proTxHash"));
            assert_eq!(p.operator_public_key.to_string(), str_of(c, "pubKeyOperator"));
            assert_eq!(address(&p.voting_key_hash), str_of(c, "votingAddress"));
            assert_eq!(p.inputs_hash.to_string(), str_of(c, "inputsHash"));
            assert_eq!(p.signatures.len() as u64, u64_of(c, "sigCount"));
        }
        TransactionPayload::QuorumCommitmentPayloadType(p) => {
            // The fields are private; check the leading version and height on the wire.
            let c = &core["qcTx"];
            let raw = serialize(&p);
            assert_eq!(u16::from_le_bytes([raw[0], raw[1]]) as u64, u64_of(c, "version"));
            assert_eq!(
                u32::from_le_bytes(raw[2..6].try_into().unwrap()) as u64,
                u64_of(c, "height")
            );
        }
        TransactionPayload::MnhfSignalPayloadType(p) => {
            let c = &core["mnhfTx"];
            assert_eq!(p.version as u64, u64_of(c, "version"));
            assert_eq!(p.version_bit as u64, u64_of(&c["signal"], "versionBit"));
        }
        other => panic!("payload not decoded into its type: {other:?}"),
    }
}

#[test]
fn core_v24_transactions() {
    let vectors = vectors();
    let txs = vectors["transactions"].as_array().unwrap();
    assert!(txs.len() >= 60);
    for vector in txs {
        let source = str_of(vector, "source");
        let core = &vector["core"];
        let bytes = bytes_of(vector, "hex");
        let tx: Transaction =
            deserialize(&bytes).unwrap_or_else(|e| panic!("{source}: decode failed: {e}"));

        assert_eq!(serialize(&tx), bytes, "{source}: re-encoding differs");
        assert_eq!(tx.txid().to_string(), str_of(core, "txid"), "{source}: txid");
        assert_eq!(tx.size() as u64, u64_of(core, "size"), "{source}: size");
        assert_eq!(tx.version as u64, u64_of(core, "version"), "{source}: version");
        assert_eq!(tx.tx_type().to_u16() as u64, u64_of(core, "type"), "{source}: type");
        let payload = serialize(tx.special_transaction_payload.as_ref().unwrap());
        assert_eq!(payload.to_lower_hex_string(), str_of(core, "extraPayload"), "{source}");
        assert_eq!(payload.len() as u64, u64_of(core, "extraPayloadSize"), "{source}");
        assert_eq!(
            tx.special_transaction_len(),
            VarInt(payload.len() as u64).len() + payload.len()
        );
        match core.get("instanceHash") {
            Some(instance) => {
                assert!(tx.has_stable_txid(), "{source}");
                assert_eq!(tx.instance_hash().to_string(), instance.as_str().unwrap());
                assert_ne!(tx.instance_hash(), tx.txid());
            }
            None => {
                assert!(!tx.has_stable_txid(), "{source}");
                assert_eq!(tx.instance_hash(), tx.txid());
            }
        }
        check_payload(&tx, core);
    }
}

#[test]
fn core_v24_blocks() {
    let vectors = vectors();
    let blocks = vectors["blocks"].as_array().unwrap();
    assert!(!blocks.is_empty());
    let mut checked_asset_unlock_root = false;
    for vector in blocks {
        let source = str_of(vector, "source");
        let bytes = bytes_of(vector, "hex");
        let block: Block =
            deserialize(&bytes).unwrap_or_else(|e| panic!("{source}: decode failed: {e}"));

        assert_eq!(serialize(&block), bytes, "{source}: re-encoding differs");
        assert_eq!(block.block_hash().to_string(), str_of(vector, "hash"), "{source}");
        assert_eq!(block.header.merkle_root.to_string(), str_of(vector, "merkleroot"));
        assert!(block.check_merkle_root(), "{source}: merkle root");
        assert_eq!(block.size(), bytes.len(), "{source}: size");
        let txids: Vec<String> = block.txdata.iter().map(|tx| tx.txid().to_string()).collect();
        let core_txids: Vec<&str> =
            vector["txids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(txids, core_txids, "{source}: txids");
        for tx in &block.txdata {
            assert!(
                !matches!(
                    tx.special_transaction_payload,
                    Some(TransactionPayload::RawPayloadType(..))
                ),
                "{source}: undecoded payload in {}",
                tx.txid()
            );
        }

        let cbtx = block.coinbase().unwrap().special_transaction_payload.clone().unwrap();
        let cbtx = cbtx.to_coinbase_payload().unwrap();
        if let Some(root) = cbtx.merkle_root_asset_unlocks {
            assert_eq!(block.compute_asset_unlocks_merkle_root(), root, "{source}");
            checked_asset_unlock_root |= block.txdata.iter().any(Transaction::has_stable_txid);
        }
    }
    assert!(checked_asset_unlock_root, "no block with a version 2 asset unlock");
}

/// The kinds of address among the entries Core holds in `net_info`.
fn address_kinds(net_info: &ExtNetInfo, kinds: &mut std::collections::BTreeSet<&'static str>) {
    for (_, entries) in &net_info.canonical().purposes {
        for entry in entries {
            kinds.insert(match entry {
                NetInfoEntry::Service {
                    network,
                    ..
                } => match network {
                    Bip155Network::Ipv4 => "ipv4",
                    Bip155Network::Ipv6 => "ipv6",
                    Bip155Network::TorV3 => "tor",
                    Bip155Network::I2p => "i2p",
                    Bip155Network::Cjdns => "cjdns",
                },
                NetInfoEntry::Domain {
                    ..
                } => "domain",
                NetInfoEntry::Invalid
                | NetInfoEntry::Malformed(_)
                | NetInfoEntry::UnknownType(_) => "invalid",
            });
        }
    }
}

/// The vectors cover every v24 layout and every kind of address, in payloads Core decoded and
/// rendered: regenerating them must not lose any.
#[test]
fn core_v24_coverage() {
    use std::collections::BTreeSet;

    let vectors = vectors();
    let shapes: Vec<(u64, u64)> = vectors["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["shape"][0].as_u64().unwrap(), v["shape"][1].as_u64().unwrap()))
        .collect();
    for want in [(1, 3), (2, 3), (3, 3), (4, 3), (5, 4), (8, 2), (9, 2), (10, 1), (11, 1), (12, 1)]
    {
        assert!(shapes.contains(&want), "no vector for type {} version {}", want.0, want.1);
    }

    // Version 3 ProTx layouts and the address kinds of their payloads, in the functional-test
    // vectors and in the crafted ones Core decoded.
    let netinfo: Value = serde_json::from_str(NETINFO_VECTORS).unwrap();
    let decoded =
        netinfo["cases"].as_array().unwrap().iter().filter(|c| c["outcome"] != "unparsable");
    let mut layouts = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for vector in vectors["transactions"].as_array().unwrap().iter().chain(decoded) {
        let tx: Transaction = deserialize(&bytes_of(vector, "hex")).unwrap();
        match tx.special_transaction_payload.unwrap() {
            TransactionPayload::ProviderRegistrationPayloadType(p) if p.version >= 3 => {
                address_kinds(ext(&p.service_address).unwrap(), &mut kinds);
                layouts.insert(format!("ProRegTx v3 type {}", p.masternode_type as u8));
                if shared(&p).is_some() {
                    layouts.insert("ProRegTx v3 shared".to_string());
                }
                if p.collateral_outpoint.txid.to_byte_array() != [0; 32] {
                    layouts.insert("ProRegTx v3 external collateral".to_string());
                }
            }
            TransactionPayload::ProviderUpdateServicePayloadType(p) if p.version >= 3 => {
                address_kinds(ext(&p.service_address).unwrap(), &mut kinds);
                layouts.insert(format!("ProUpServTx v3 type {}", p.mn_type.unwrap()));
            }
            _ => {}
        }
    }
    for want in [
        "ProRegTx v3 type 0",
        "ProRegTx v3 type 1",
        "ProRegTx v3 shared",
        "ProRegTx v3 external collateral",
        "ProUpServTx v3 type 0",
        "ProUpServTx v3 type 1",
    ] {
        assert!(layouts.contains(want), "no vector for {want}: {layouts:?}");
    }
    let every_kind = BTreeSet::from(["cjdns", "domain", "i2p", "invalid", "ipv4", "ipv6", "tor"]);
    assert_eq!(kinds, every_kind, "address kinds in ProTx payloads");
}

/// Recovers the signer of a 65-byte compact signature over `hash` as a P2PKH script.
fn signer_script(signature: &[u8], hash: [u8; 32]) -> ScriptBuf {
    let signature = dashcore::sign_message::from_slice(signature).expect("compact signature");
    let pubkey = recover_pubkey(&signature, sha256d::Hash::from_byte_array(hash)).expect("recover");
    Address::new(Network::Regtest, Payload::p2pkh(&pubkey)).script_pubkey()
}

fn address_script(address: &str) -> ScriptBuf {
    address.parse::<Address<_>>().unwrap().assume_checked().script_pubkey()
}

/// The payload signatures recover to the keys Core checked them against, which validates the
/// signed messages: the collateral sign string of a ProRegTx (version 3 lists the payouts) and
/// the payload hash of ProUpRegTx and of the shared masternode updates.
#[test]
fn core_v24_payload_signatures() {
    let vectors = vectors();
    let mut checked = [0usize; 13];
    for vector in vectors["transactions"].as_array().unwrap() {
        let Some(signers) = vector.get("signers") else {
            continue;
        };
        let source = str_of(vector, "source");
        let bytes = bytes_of(vector, "hex");
        let tx: Transaction = deserialize(&bytes).unwrap();
        match tx.special_transaction_payload.unwrap() {
            TransactionPayload::ProviderRegistrationPayloadType(p) => {
                let message = p.payload_collateral_string(Network::Regtest).unwrap();
                let signer = signer_script(&p.signature, signed_msg_hash(&message).to_byte_array());
                assert_eq!(signer.to_hex_string(), str_of(signers, "collateralScript"), "{source}");
                checked[1] += 1;
            }
            TransactionPayload::ProviderUpdateRegistrarPayloadType(p) => {
                let hash = p.base_payload_hash().to_byte_array();
                let owner = address_script(str_of(signers, "ownerAddress"));
                assert_eq!(signer_script(&p.payload_sig, hash), owner, "{source}");
                checked[3] += 1;
            }
            TransactionPayload::ProviderUpdateSharePayloadType(p) => {
                let owners = signers["shareOwnerAddresses"].as_array().unwrap();
                let owner = address_script(owners[p.share_index as usize].as_str().unwrap());
                let hash = p.base_payload_hash().to_byte_array();
                assert_eq!(signer_script(&p.payload_sig, hash), owner, "{source}");
                checked[11] += 1;
            }
            TransactionPayload::ProviderUpdateSharedRegistrarPayloadType(p) => {
                let owners = signers["shareOwnerAddresses"].as_array().unwrap();
                assert_eq!(p.signatures.len(), owners.len());
                let hash = p.base_payload_hash().to_byte_array();
                for (signature, owner) in p.signatures.iter().zip(owners) {
                    let owner = address_script(owner.as_str().unwrap());
                    assert_eq!(signer_script(signature.as_ref(), hash), owner, "{source}");
                }
                checked[12] += 1;
            }
            other => panic!("{source}: unexpected signers for {:?}", other.get_type()),
        }
    }
    assert!(
        checked[1] >= 3 && checked[3] >= 2 && checked[11] >= 1 && checked[12] >= 1,
        "{checked:?}"
    );
}

/// Masternode list diffs served by a v24 node over P2P (`getmnlistd` from genesis) decode,
/// re-encode to the same bytes and match Core's `protx diff`, including version 3 entries with
/// extended addresses (IPv6, Tor, I2P, domain names), and their entries hash to Core's merkle
/// root.
#[test]
fn core_v24_mnlistdiffs() {
    use dashcore::network::message_sml::MnListDiff;
    use dashcore::sml::masternode_list_entry::EntryMasternodeType;

    let vectors: Value = serde_json::from_str(MNLISTDIFF_VECTORS).unwrap();
    let mut extended_entries = 0;
    let mut kinds = std::collections::BTreeSet::new();
    for vector in vectors["diffs"].as_array().unwrap() {
        let source = str_of(vector, "source");
        let core = &vector["core"];
        let bytes = bytes_of(vector, "mnlistdiff");
        let diff: MnListDiff =
            deserialize(&bytes).unwrap_or_else(|e| panic!("{source}: decode failed: {e}"));
        assert_eq!(serialize(&diff), bytes, "{source}: re-encoding differs");
        assert_eq!(diff.version as u64, u64_of(core, "nVersion"));
        assert_eq!(diff.block_hash.to_string(), str_of(core, "blockHash"));
        assert_eq!(diff.new_quorums.len() as u64, u64_of(core, "newQuorums"));

        let core_list = core["mnList"].as_array().unwrap();
        assert_eq!(diff.new_masternodes.len(), core_list.len(), "{source}");
        for (entry, c) in diff.new_masternodes.iter().zip(core_list) {
            assert_eq!(entry.version as u64, u64_of(c, "nVersion"));
            assert_eq!(
                entry.pro_reg_tx_hash.to_string(),
                internal_order(str_of(c, "proRegTxHash"))
            );
            assert_eq!(entry.operator_public_key.to_string(), str_of(c, "pubKeyOperator"));
            assert_eq!(address(&entry.key_id_voting), str_of(c, "votingAddress"));
            assert_eq!(entry.is_valid, c["isValid"].as_bool().unwrap());
            match &entry.service_address {
                MasternodeNetInfo::Extended(net_info) => {
                    check_net_info(net_info, &c["addresses"]);
                    address_kinds(net_info, &mut kinds);
                    extended_entries += 1;
                }
                MasternodeNetInfo::Legacy(addr) => {
                    check_service_address(Some((addr.ip(), addr.port())), &c["addresses"])
                }
            }
            // The type is only on the wire from entry version 2; Core's JSON shows it regardless.
            match &entry.mn_type {
                EntryMasternodeType::Regular if entry.version < 2 => {}
                EntryMasternodeType::Regular => assert_eq!(u64_of(c, "nType"), 0),
                EntryMasternodeType::HighPerformance {
                    platform_node_id,
                    ..
                } => {
                    assert_eq!(u64_of(c, "nType"), 1);
                    assert_eq!(platform_node_id.to_string(), str_of(c, "platformNodeID"));
                }
            }
        }

        let mut hashes: Vec<_> = diff.new_masternodes.clone();
        hashes.sort();
        let hashes = hashes.iter().map(|entry| entry.calculate_entry_hash());
        let root: sha256d::Hash = dashcore::merkle_tree::calculate_root(hashes).expect("non-empty");
        // `sha256d::Hash` displays like Core's uint256, the coinbase root in internal order.
        assert_eq!(root.to_string(), str_of(core, "merkleRootMNList"), "{source}: merkle root");
        let cbtx = diff.coinbase_tx.special_transaction_payload.clone().unwrap();
        let cbtx = cbtx.to_coinbase_payload().unwrap();
        let core_root = internal_order(str_of(core, "merkleRootMNList"));
        assert_eq!(cbtx.merkle_root_masternode_list.to_string(), core_root, "{source}");
    }
    assert!(extended_entries >= 4, "only {extended_entries} version 3 entries");
    // Every kind of address Core keeps for a masternode.
    let every_kind = std::collections::BTreeSet::from(["domain", "i2p", "ipv4", "ipv6", "tor"]);
    assert_eq!(kinds, every_kind, "address kinds in SML entries");
}

const NETINFO_VECTORS: &str = include_str!("data/core_v24_netinfo.json");

fn payload_net_info(tx: &Transaction) -> &ExtNetInfo {
    match tx.special_transaction_payload.as_ref() {
        Some(TransactionPayload::ProviderRegistrationPayloadType(p)) => ext(&p.service_address),
        Some(TransactionPayload::ProviderUpdateServicePayloadType(p)) => ext(&p.service_address),
        other => panic!("not a version 3 provider payload: {other:?}"),
    }
    .expect("net info")
}

/// Version 3 ProRegTx and ProUpServTx payloads that dashd v24.0.0-rc.4 built and signed, with
/// only their net info rewritten (repeated, reordered and unknown purposes, entries Core reads as
/// invalid, an address claiming more bytes than are left, overlong and truncated domains), then
/// judged by dashd: `rejected` ones it decoded but refused (`bad-protx-netinfo-*`), `unparsable`
/// ones it couldn't decode (`bad-protx-payload`; since rc.4 every repeated, out-of-order or
/// unknown purpose). The `mined` ones are Core's own payloads, which the updates need. See
/// `contrib/core-vectors/craft_netinfo.py`.
///
/// Every transaction re-encodes to its bytes and txid. The payload decodes exactly when Core's
/// does, into the map Core shows, and the mined payloads' signatures check here too.
#[test]
fn core_v24_netinfo() {
    let vectors: Value = serde_json::from_str(NETINFO_VECTORS).unwrap();
    let mut outcomes = std::collections::BTreeMap::<&str, usize>::new();
    for case in vectors["cases"].as_array().unwrap() {
        let name = str_of(case, "name");
        let outcome = str_of(case, "outcome");
        let core = &case["core"];
        let bytes = bytes_of(case, "hex");
        let tx: Transaction =
            deserialize(&bytes).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
        assert_eq!(serialize(&tx), bytes, "{name}: re-encoding differs");
        assert_eq!(tx.txid().to_string(), str_of(core, "txid"), "{name}: txid");
        assert_eq!(case["testmempoolaccept"]["allowed"].as_bool(), Some(outcome == "mined"));
        let payload = bytes_of(core, "extraPayload");
        *outcomes.entry(outcome).or_default() += 1;

        if outcome == "unparsable" {
            assert!(core.get("proRegTx").is_none() && core.get("proUpServTx").is_none());
            let tx_type = u64_of(core, "type") as u16;
            assert_eq!(
                tx.special_transaction_payload,
                Some(TransactionPayload::RawPayloadType(tx_type, payload)),
                "{name}"
            );
            continue;
        }
        check_payload(&tx, core);
        if outcome == "rejected" {
            continue;
        }
        assert_eq!(outcome, "mined");
        let original: Transaction = deserialize(&bytes_of(case, "core_original_hex")).unwrap();
        // A mined case keeps Core's payload; only its inputs are signed again.
        assert_eq!(tx.special_transaction_payload, original.special_transaction_payload, "{name}");
        assert_eq!(payload_net_info(&tx).canonical(), *payload_net_info(&tx), "{name}");
        match tx.special_transaction_payload.as_ref().unwrap() {
            TransactionPayload::ProviderRegistrationPayloadType(p) => {
                let message = p.payload_collateral_string(Network::Regtest).unwrap();
                assert_eq!(message, str_of(case, "sign_message"), "{name}");
                let signer = signer_script(&p.signature, signed_msg_hash(&message).to_byte_array());
                assert_eq!(signer, address_script(str_of(case, "collateral_address")), "{name}");
            }
            #[cfg(feature = "bls")]
            TransactionPayload::ProviderUpdateServicePayloadType(p) => {
                use dashcore::bls_sig_utils::BLSPublicKey;
                use dashcore_crypto::bls::BlsScheme;
                let operator: BLSPublicKey = str_of(case, "operator_public_key").parse().unwrap();
                let digest = p.base_payload_hash().to_byte_array();
                operator.as_scheme(BlsScheme::Modern).verify(&digest, &p.payload_sig).unwrap();
            }
            _ => {}
        }
    }
    assert_eq!(
        outcomes,
        [("mined", 4), ("rejected", 4), ("unparsable", 8)].into_iter().collect(),
        "{outcomes:?}"
    );

    for block in vectors["blocks"].as_array().unwrap() {
        let hash = str_of(block, "hash");
        let bytes = bytes_of(block, "hex");
        let block_decoded: Block = deserialize(&bytes).unwrap();
        assert_eq!(serialize(&block_decoded), bytes, "{hash}: re-encoding differs");
        assert_eq!(block_decoded.block_hash().to_string(), hash);
        assert!(block_decoded.check_merkle_root(), "{hash}: merkle root");
        let txids: Vec<String> =
            block_decoded.txdata.iter().map(|tx| tx.txid().to_string()).collect();
        let core_txids: Vec<&str> =
            block["txids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(txids, core_txids, "{hash}: txids");
    }
}

/// How the junk entries decode: each as Core reads it, ending where Core stops. In
/// `proreg_every_entry_kind_kept` they make up the list Core keeps, so its JSON shows Core's
/// verdict on each (an address or `[invalid entry]`). A repeated, out-of-order or unknown
/// purpose fails the read with Core's text (rc.4, `evo/netinfo.h`:419-437).
#[test]
fn core_v24_netinfo_entries() {
    let vectors: Value = serde_json::from_str(NETINFO_VECTORS).unwrap();
    let case = |name: &str| {
        let case = vectors["cases"].as_array().unwrap().iter().find(|c| c["name"] == name);
        let case = case.unwrap_or_else(|| panic!("no case {name}"));
        (case.clone(), deserialize::<Transaction>(&bytes_of(case, "hex")).unwrap())
    };

    let (kept_case, tx) = case("proreg_every_entry_kind_kept");
    let junk = kept_case["junk"].as_array().unwrap();
    let kept = &payload_net_info(&tx).purposes[0].1[1..];
    let rendered = kept_case["core"]["proRegTx"]["addresses"]["core_p2p"].as_array().unwrap();
    assert_eq!((kept.len(), rendered.len()), (junk.len(), junk.len() + 1));
    let valid = ["CJDNS", "Tor v3", "I2P", "IPv6", "domain", "253-byte domain"];
    for ((what, rendered), entry) in junk.iter().zip(&rendered[1..]).zip(kept) {
        let what = what.as_str().unwrap();
        let core_valid = rendered.as_str().unwrap() != "[invalid entry]";
        assert_eq!(core_valid, valid.contains(&what), "{what}: Core shows {rendered}");
        assert_eq!(entry.is_valid(), core_valid, "{what}: {entry:?}");
        match what {
            "invalid" => assert_eq!(*entry, NetInfoEntry::Invalid),
            "unknown type" => assert_eq!(*entry, NetInfoEntry::UnknownType(0)),
            _ if valid.contains(&what) => assert!(entry.is_valid(), "{what}: {entry:?}"),
            _ => assert!(matches!(entry, NetInfoEntry::Malformed(_)), "{what}: {entry:?}"),
        }
    }
    // Core stops reading a service address at its length when that is wrong or too long.
    let ends: Vec<&[u8]> = kept
        .iter()
        .filter_map(|entry| match entry {
            NetInfoEntry::Malformed(raw) if raw.len() <= 5 => Some(raw.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        [&[0x01, 0x01, 0x05][..], &[0x01, 0x01, 0xfd, 0x04, 0x00], &[0x01, 0x07, 0xfd, 0x01, 0x02]]
    );

    let (_, tx) = case("proreg_short_claim");
    let short_claim = NetInfoEntry::Malformed(vec![0x01, 0x07, 0xfd, 0xff, 0x01]);
    assert_eq!(payload_net_info(&tx).purposes[0].1.last(), Some(&short_claim));

    for name in [
        "proreg_unknown_purpose",
        "proreg_repeated_purpose_junk",
        "proreg_evo_purposes_out_of_order",
        "proupserv_repeated_purpose_junk_short_claim",
        "proupserv_evo_purposes_out_of_order",
    ] {
        let (case, _) = case(name);
        let payload = bytes_of(&case["core"], "extraPayload");
        let err = match u64_of(&case["core"], "type") {
            1 => ProviderRegistrationPayload::consensus_decode(&mut &payload[..]).map(drop),
            2 => ProviderUpdateServicePayload::consensus_decode(&mut &payload[..]).map(drop),
            other => panic!("{name}: type {other}"),
        }
        .unwrap_err();
        assert!(
            err.to_string().contains("ExtNetInfo: purpose unknown or out of order"),
            "{name}: {err}"
        );
    }
}

const QRINFO_VECTORS: &str = include_str!("data/core_v24_regtest_qrinfo.json");

/// Checks a masternode list diff inside a `qrinfo` against Core's JSON for it.
fn check_qrinfo_diff(diff: &dashcore::network::message_sml::MnListDiff, core: &Value, at: &str) {
    assert_eq!(diff.version as u64, u64_of(core, "nVersion"), "{at}");
    assert_eq!(diff.base_block_hash.to_string(), str_of(core, "baseBlockHash"), "{at}");
    assert_eq!(diff.block_hash.to_string(), str_of(core, "blockHash"), "{at}");
    let entries = core["mnList"].as_array().unwrap();
    assert_eq!(diff.new_masternodes.len(), entries.len(), "{at}");
    for (entry, c) in diff.new_masternodes.iter().zip(entries) {
        assert_eq!(entry.version as u64, u64_of(c, "nVersion"), "{at}");
        let pro_reg_tx_hash = internal_order(str_of(c, "proRegTxHash"));
        assert_eq!(entry.pro_reg_tx_hash.to_string(), pro_reg_tx_hash, "{at}");
        assert_eq!(entry.is_valid, c["isValid"].as_bool().unwrap(), "{at}");
    }
    let quorums = core["newQuorums"].as_array().unwrap();
    assert_eq!(diff.new_quorums.len(), quorums.len(), "{at}");
    for (quorum, c) in diff.new_quorums.iter().zip(quorums) {
        assert_eq!(u8::from(quorum.llmq_type) as u64, u64_of(c, "llmqType"), "{at}");
        assert_eq!(quorum.version as u64, u64_of(c, "version"), "{at}");
        assert_eq!(quorum.quorum_hash.to_string(), str_of(c, "quorumHash"), "{at}");
    }
    assert_eq!(diff.deleted_quorums.len(), core["deletedQuorums"].as_array().unwrap().len());
    assert_eq!(
        diff.quorums_chainlock_signatures.len(),
        core["quorumsCLSigs"].as_array().unwrap().len(),
        "{at}"
    );
    let cbtx = diff.coinbase_tx.special_transaction_payload.clone().unwrap();
    let cbtx = cbtx.to_coinbase_payload().unwrap();
    let mn_list_root = internal_order(str_of(core, "merkleRootMNList"));
    assert_eq!(cbtx.merkle_root_masternode_list.to_string(), mn_list_root, "{at}");
}

/// Checks a quorum snapshot inside a `qrinfo` against Core's JSON for it.
fn check_qrinfo_snapshot(
    snapshot: &dashcore::network::message_qrinfo::QuorumSnapshot,
    core: &Value,
    at: &str,
) {
    let members: Vec<bool> = core["activeQuorumMembers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_bool().unwrap())
        .collect();
    assert_eq!(snapshot.active_quorum_members, members, "{at}");
    assert_eq!(snapshot.skip_list_mode.index() as u64, u64_of(core, "mnSkipListMode"), "{at}");
    let skip_list: Vec<i64> =
        core["mnSkipList"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    let crate_skip_list: Vec<i64> = snapshot.skip_list.iter().map(|&v| v as i64).collect();
    assert_eq!(crate_skip_list, skip_list, "{at}");
}

/// `qrinfo` answers of a v24 node (DIP-0024 rotation, `llmq_test_dip0024`) to `getqrinfo` over
/// P2P, with and without the extra share and with one or two base blocks, decode, re-encode to
/// the same bytes and match Core's `quorum rotationinfo` for the same request.
#[test]
fn core_v24_qrinfo() {
    use dashcore::network::message_qrinfo::QRInfo;

    let vectors: Value = serde_json::from_str(QRINFO_VECTORS).unwrap();
    let mut extra_share = [0usize; 2];
    for vector in vectors["qrinfo"].as_array().unwrap() {
        let source = str_of(vector, "source");
        let core = &vector["core"];
        let bytes = bytes_of(vector, "message");
        let qrinfo: QRInfo =
            deserialize(&bytes).unwrap_or_else(|e| panic!("{source}: decode failed: {e}"));
        assert_eq!(serialize(&qrinfo), bytes, "{source}: re-encoding differs");

        let requested = vector["request"]["extraShare"].as_bool().unwrap();
        assert_eq!(core["extraShare"].as_bool(), Some(requested));
        assert_eq!(qrinfo.quorum_snapshot_and_mn_list_diff_at_h_minus_4c.is_some(), requested);
        extra_share[requested as usize] += 1;

        for (snapshot, key) in [
            (&qrinfo.quorum_snapshot_at_h_minus_c, "quorumSnapshotAtHMinusC"),
            (&qrinfo.quorum_snapshot_at_h_minus_2c, "quorumSnapshotAtHMinus2C"),
            (&qrinfo.quorum_snapshot_at_h_minus_3c, "quorumSnapshotAtHMinus3C"),
        ] {
            check_qrinfo_snapshot(snapshot, &core[key], &format!("{source} {key}"));
        }
        for (diff, key) in [
            (&qrinfo.mn_list_diff_tip, "mnListDiffTip"),
            (&qrinfo.mn_list_diff_h, "mnListDiffH"),
            (&qrinfo.mn_list_diff_at_h_minus_c, "mnListDiffAtHMinusC"),
            (&qrinfo.mn_list_diff_at_h_minus_2c, "mnListDiffAtHMinus2C"),
            (&qrinfo.mn_list_diff_at_h_minus_3c, "mnListDiffAtHMinus3C"),
        ] {
            check_qrinfo_diff(diff, &core[key], &format!("{source} {key}"));
        }
        if let Some((snapshot, diff)) = &qrinfo.quorum_snapshot_and_mn_list_diff_at_h_minus_4c {
            check_qrinfo_snapshot(snapshot, &core["quorumSnapshotAtHMinus4C"], source);
            check_qrinfo_diff(diff, &core["mnListDiffAtHMinus4C"], source);
        }

        let commitments = core["lastCommitmentPerIndex"].as_array().unwrap();
        assert_eq!(qrinfo.last_commitment_per_index.len(), commitments.len(), "{source}");
        for (quorum, c) in qrinfo.last_commitment_per_index.iter().zip(commitments) {
            assert_eq!(u8::from(quorum.llmq_type) as u64, u64_of(c, "llmqType"), "{source}");
            assert_eq!(quorum.quorum_index.map(i64::from), c["quorumIndex"].as_i64(), "{source}");
            assert_eq!(quorum.version as u64, u64_of(c, "version"), "{source}");
            assert_eq!(quorum.quorum_hash.to_string(), str_of(c, "quorumHash"), "{source}");
        }
        let snapshots = core["quorumSnapshotList"].as_array().unwrap();
        assert_eq!(qrinfo.quorum_snapshot_list.len(), snapshots.len(), "{source}");
        for (snapshot, c) in qrinfo.quorum_snapshot_list.iter().zip(snapshots) {
            check_qrinfo_snapshot(snapshot, c, source);
        }
        let diffs = core["mnListDiffList"].as_array().unwrap();
        assert_eq!(qrinfo.mn_list_diff_list.len(), diffs.len(), "{source}");
        for (diff, c) in qrinfo.mn_list_diff_list.iter().zip(diffs) {
            check_qrinfo_diff(diff, c, source);
        }
    }
    assert!(extra_share[0] >= 1 && extra_share[1] >= 1, "{extra_share:?}");
}

//! EV2b (the block builder and `process_block`) against EV0: the QcTx PoSe walk through the
//! member source, the error classes, and an effect emitted before a failure.

use std::sync::Arc;

use dash_consensus::primitives::{Block, core_serialize, decode_block};
use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::netinfo::{NetInfoConfig, NetInfoRules};
use dash_dml::testkit::{MemChain, dmn, pro_tx};
use dash_dml::{
    DML_PREV_LIST_MISMATCH, DmlBuildError, DmlEvent, DmlMemberSource, DmlParams, DmlRules, MnList,
    MnType, NoCoins, PrevBlock, ProTxHash, StoreError, build_new_list_from_block, payload,
    process_block,
};
use dash_evo::Effect;
use dash_evo::llmq::MemberError;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType, Transaction, TxType};
use dash_types::Numeric;

use super::lq1a::{AttemptHandle, FixedMembers, T, rotation_fixture};

fn params(kind: NetworkKind) -> ChainParams {
    ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's parameters")
}

/// A block of a coinbase and one version-3 special transaction with `payload`.
fn block(tx_type: TxType, payload: Vec<u8>) -> Block {
    let tx = |version, tx_type, extra_payload| {
        core_serialize(&Transaction {
            version,
            tx_type,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            extra_payload,
        })
    };
    let mut raw = vec![0; 80];
    raw.push(2);
    raw.extend(tx(1, TxType::Spend, vec![]));
    raw.extend(tx(3, tx_type, payload));
    decode_block(&mut &raw[..]).expect("a block").into_value()
}

/// A v1 (`version`) or v2 quorum commitment at `height`: one signer, so not null, and the given
/// valid members; the BLS fields are zero (the build never verifies them).
fn qc(
    llmq_type: LlmqType,
    quorum_hash: BlockHash,
    index: Option<i16>,
    height: u32,
    valid: &[bool],
) -> Block {
    let bits = |set: &[bool]| {
        let mut out = vec![set.len() as u8];
        let mut data = vec![0u8; set.len().div_ceil(8)];
        for (i, _) in set.iter().enumerate().filter(|(_, b)| **b) {
            data[i / 8] |= 1 << (i % 8);
        }
        out.extend(data);
        out
    };
    let version: u16 = if index.is_some() {
        2
    } else {
        1
    };
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(height.to_le_bytes());
    p.extend(version.to_le_bytes());
    p.push(llmq_type.to_base());
    p.extend(quorum_hash.as_bytes());
    if let Some(index) = index {
        p.extend(index.to_le_bytes());
    }
    let mut signers = vec![false; valid.len()];
    signers[0] = true;
    p.extend(bits(&signers));
    p.extend(bits(valid));
    p.extend([0; 48 + 32 + 96 + 96]);
    block(TxType::QuorumCommitment, p)
}

/// A v1 ProUpServTx for `pro_tx`; its signature is zero (`process_block` never checks it).
fn pro_up_serv(pro_tx: ProTxHash) -> Block {
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(pro_tx.as_bytes());
    p.extend([0; 10]);
    p.extend([0xff, 0xff, 127, 0, 0, 1]);
    p.extend(9999u16.to_be_bytes());
    p.push(0);
    p.extend([0; 32]);
    p.extend([0; 96]);
    block(TxType::ProviderUpdateService, p)
}

/// An `ExtNetInfo` (version 1) of one 1.1.1.1:19999 service per purpose in `purposes`.
fn ext_net_info(purposes: &[u8]) -> Vec<u8> {
    let mut out = vec![1, purposes.len() as u8];
    for &purpose in purposes {
        out.extend([purpose, 1, 0x01, 0x01, 0x04, 1, 1, 1, 1, 0x4e, 0x1f]);
    }
    out
}

/// A v3 (extended addresses) regular ProRegTx with an external collateral and `net_info`, no
/// payout shares and an empty signature.
fn pro_reg_v3(net_info: &[u8]) -> Block {
    let mut p = Vec::new();
    p.extend([3u16, 0, 0].iter().flat_map(|v| v.to_le_bytes()));
    p.extend([7; 32]);
    p.extend(0u32.to_le_bytes());
    p.extend(net_info);
    p.extend([1; 20]);
    p.extend([0; 48]);
    p.extend([2; 20]);
    p.extend(0u16.to_le_bytes());
    p.extend([0, 0]);
    p.extend(0u32.to_le_bytes());
    p.extend(0i64.to_le_bytes());
    p.extend([0; 32]);
    p.push(0);
    block(TxType::ProviderRegister, p)
}

/// A v3 regular ProUpServTx for `pro_tx` with `net_info`; its signature is zero.
fn pro_up_serv_v3(pro_tx: ProTxHash, net_info: &[u8]) -> Block {
    let mut p = Vec::new();
    p.extend([3u16, 0].iter().flat_map(|v| v.to_le_bytes()));
    p.extend(pro_tx.as_bytes());
    p.extend(net_info);
    p.push(0);
    p.extend([0; 32]);
    p.extend([0; 96]);
    block(TxType::ProviderUpdateService, p)
}

/// CE4-01 on the build path: rc.4 reads an `ExtNetInfo`'s purposes only if each is known and
/// above the one before it (`netinfo.h` rc.4:419-437), so `GetTxPayload` fails a v3 ProRegTx or
/// ProUpServTx with a repeated, descending or unknown purpose, and the build rejects it with
/// `bad-protx-payload` (`evo/specialtxman.cpp` rc.4:360, 456). Since R6 base-sdk's decoder is
/// what refuses them, so this fails if it reads them again. Ascending purposes read.
#[test]
fn net_info_purposes_read_as_rc4_reads_them() {
    let mut chain = MemChain::new();
    let prev = chain.extend(chain.genesis(), 30);
    let build = Build::over(&chain, prev, &[1]);
    let none = FixedMembers::new(vec![]);
    // The build's reject reason, or `None` where it builds the list.
    let reason = |block: &Block| {
        build.run(block, &none).result.err().and_then(|e| e.reject_reason().map(str::to_owned))
    };
    for (purposes, reads) in [
        (&[0, 1][..], true),
        (&[0, 1, 0][..], false),
        (&[0, 0][..], false),
        (&[1, 0][..], false),
        (&[0, 3][..], false),
    ] {
        let net_info = ext_net_info(purposes);
        for block in [pro_reg_v3(&net_info), pro_up_serv_v3(pro_tx(1), &net_info)] {
            let tx = &block.txs()[1];
            let label = format!("{:?} {purposes:?}", tx.tx_type());
            assert_eq!(payload(tx).is_some(), reads, "{label}");
            let reason = reason(&block);
            assert_eq!(
                reason.as_deref() == Some("bad-protx-payload"),
                !reads,
                "{label}: {reason:?}"
            );
        }
    }
}

/// What a build gave: its result, the effects it emitted and its trace.
struct Run {
    result: Result<MnList, DmlBuildError>,
    effects: Vec<Effect>,
    trace: Vec<DmlEvent>,
}

/// A build over `list` with `prev` as the parent block.
struct Build<'a> {
    prev: PrevBlock<'a>,
    list: MnList,
}

impl<'a> Build<'a> {
    /// The build of the block after `prev` over the list at `prev` holding `ns`.
    fn over(chain: &'a MemChain, prev: BlockId, ns: &[u8]) -> Self {
        let header = chain.header(prev);
        Build {
            prev: PrevBlock {
                id: prev,
                hash: header.hash,
                height: header.height as i32,
                chain,
            },
            list: list_at(chain, prev, ns),
        }
    }

    /// `build_new_list_from_block` on regtest at V24.
    fn run(&self, block: &Block, members: &DmlMemberSource<'_>) -> Run {
        let regtest = params(NetworkKind::Regtest);
        let mainnet = params(NetworkKind::Mainnet);
        let dml = DmlParams::new(&regtest);
        let config = NetInfoConfig {
            cjdns_reachable: true,
        };
        let net_info = NetInfoRules::new(&regtest, &mainnet, config);
        let rules = DmlRules {
            params: &dml,
            net_info: &net_info,
            v19: true,
            mn_rr: true,
            v24: true,
        };
        let (mut effects, mut trace) = (Vec::new(), Vec::new());
        let result = build_new_list_from_block(
            block,
            self.prev,
            &self.list,
            &NoCoins,
            members,
            &rules,
            &mut effects,
            &mut trace,
        )
        .map(|built| built.list);
        Run {
            result,
            effects,
            trace,
        }
    }
}

/// The list at `prev` holding the masternodes numbered `ns`.
fn list_at(chain: &MemChain, prev: BlockId, ns: &[u8]) -> MnList {
    let header = chain.header(prev);
    let mut list = MnList::new(header.hash, header.height as i32, ns.len() as u32);
    for &n in ns {
        list.insert_for_tests(dmn(n, MnType::Regular));
    }
    list
}

/// The masternodes the build punished, in order.
fn punished(trace: &[DmlEvent]) -> Vec<ProTxHash> {
    trace
        .iter()
        .filter_map(|e| match e {
            DmlEvent::Punished {
                pro_tx,
                ..
            } => Some(*pro_tx),
            _ => None,
        })
        .collect()
}

/// `HandleQuorumCommitment`'s member walk (`evo/specialtxman.cpp` v24:283-298): the members that
/// failed the DKG are punished in member-vector order (quirk 41: not the bucket's phase-start
/// order).
#[test]
fn failed_members_come_in_member_order() {
    let mut chain = MemChain::new();
    let base = chain.extend(chain.genesis(), 24);
    let prev = chain.extend(base, 6);
    let source = FixedMembers::new([4, 1, 3].map(|n| dmn(n, MnType::Regular)).to_vec());
    let block = qc(LlmqType::LlmqTest, chain.hash(base), None, 24, &[false, true, false]);
    let run = Build::over(&chain, prev, &[1, 3, 4]).run(&block, &source);
    run.result.unwrap();
    assert_eq!(punished(&run.trace), [pro_tx(4), pro_tx(3)]);
}

/// EV addendum §2.3: a store fault of the member source, a missing input of a connected block,
/// and a previous list that is neither the parent's nor Core's default are node errors, never
/// block verdicts.
#[test]
fn a_store_fault_is_not_a_verdict() {
    let mut chain = MemChain::new();
    let base = chain.extend(chain.genesis(), 24);
    let prev = chain.extend(base, 6);
    let mut build = Build::over(&chain, prev, &[1]);
    let block = qc(LlmqType::LlmqTest, chain.hash(base), None, 24, &[false]);
    let fail =
        |build: &Build<'_>, source: &FixedMembers| build.run(&block, source).result.unwrap_err();

    let read = FixedMembers::failing(MemberError::View(StoreError::read("injected")));
    assert!(matches!(fail(&build, &read), DmlBuildError::Store(StoreError::Read { .. })));
    let missing = FixedMembers::failing(MemberError::Missing(BlockId(0)));
    let err = fail(&build, &missing);
    assert!(matches!(err, DmlBuildError::Store(StoreError::Inconsistent { .. })));
    assert_eq!(err.reject_reason(), None);

    // EV2b D8: Core asserts the previous list is the parent's or `CDeterministicMNList()`
    // (v24:317), whose `operator==` compares the registration counter too.
    let mismatch = DmlBuildError::Store(StoreError::inconsistent(DML_PREV_LIST_MISMATCH));
    let none = FixedMembers::new(vec![]);
    build.list = list_at(&chain, base, &[1]);
    assert_eq!(fail(&build, &none), mismatch);
    build.list = MnList::new(BlockHash::ZERO, -1, 5);
    assert_eq!(fail(&build, &none), mismatch);
    build.list = MnList::new(BlockHash::ZERO, -1, 0);
    assert!(build.run(&block, &none).result.is_ok());
}

/// `process_block` (EV2 §3.8 steps 3 and 7): the platform-ban resets are emitted before the
/// enforcement pin, so they survive `bad-dip3-enf-block`.
#[test]
fn the_reset_survives_the_enforcement_failure() {
    let dml = DmlParams::new(&params(NetworkKind::Mainnet));
    let height = dml.dip3_enforcement_height;
    let pinned = dml.dip3_enforcement_hash.expect("mainnet pins it");
    let block = pro_up_serv(pro_tx(7));
    let prev = Arc::new(MnList::new(BlockHash::from([1; 32]), height - 1, 0));
    let reset = [Effect::ResetPlatformBan {
        pro_tx_hash: pro_tx(7),
        height,
    }];
    let process = |hash, effects: &mut Vec<Effect>| {
        let new_list = MnList::new(BlockHash::ZERO, height, 0);
        process_block(&block, (hash, height), &prev, new_list, &dml, false, effects)
    };

    let mut effects = Vec::new();
    let err = process(BlockHash::from([2; 32]), &mut effects).unwrap_err();
    assert_eq!(err.reject_reason(), "bad-dip3-enf-block");
    assert_eq!(effects, reset);

    let mut effects = Vec::new();
    process(pinned, &mut effects).unwrap();
    assert_eq!(effects, reset);
}

/// EV addendum §2.6 with EV2b (Sol r3 R3-M1): a source that borrows its attempt's view, as
/// CH7b's `AttemptHandle<'v>` does, reaches the build's member walk, and the rows it stores are
/// effects in the build's log.
#[test]
fn an_attempt_bound_source_reaches_the_member_walk() {
    let (chain, b1, b2, store, roster) = rotation_fixture();
    let view = store.view(&chain);
    let handle = AttemptHandle::begin(&view, roster);
    // Height 1 at index 1: the base is b1 (`nHeight - nHeight % dkgInterval + quorumIndex`).
    let block = qc(T, chain.hash(b1), Some(1), 1, &[false]);
    let run = Build::over(&chain, b2, &[1, 2, 3]).run(&block, &handle);
    run.result.unwrap();
    assert_eq!(punished(&run.trace), [pro_tx(2)]);
    assert!(
        matches!(run.effects[..], [Effect::StoreQuorumSnapshot { cycle_base, .. }] if cycle_base == chain.hash(b1))
    );
}

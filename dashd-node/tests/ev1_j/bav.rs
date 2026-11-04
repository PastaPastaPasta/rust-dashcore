//! EV1-J J0b-2: the assumevalid fixture (plan rev4 §5.2) and the BAV rows' probes on the quorum
//! chain, each built before any validator sees it.
//!
//! Core skips a block's scripts (`fScriptChecks = false`, `validation.cpp:2553-2580` at the tag)
//! when an `-assumevalid` block is indexed, the block is its ancestor and the best header's, the
//! best header has the minimum chain work (regtest's is 0, `chainparams.cpp:870`, and no
//! validator sets `-minimumchainwork`), and the best header is more than two weeks of
//! proof-equivalent time above it (`:2579`; `GetBlockProofEquivalentTime`, `chain.cpp:147-162`:
//! the work between them times `nPowTargetSpacing` over the best header's proof). Regtest mines
//! every block at the same target (`fPowNoRetargeting`, `chainparams.cpp:841`) and spaces them
//! 150 s (`:839`), so `n` blocks above it are `150 n` s: [`BURIED`] descendants are 1,209,750 s,
//! over the 1,209,600 s of two weeks, and [`TWO_WEEKS`] are exactly two weeks, which the `<=`
//! still checks. Each probe gets [`BURIED`] descendant headers of its own, ground here and never
//! mined; both validators start with `-assumevalid` at the last one, take the probe's header and
//! the descendants (dashd `submitheader`, V `process_headers`) and then the probe's body. The
//! judge checks the burial on dashd's own chain work.
//!
//! Types 7 and 9 verify their signatures whatever `check_sigs` says (`evo/mnhftx.cpp:152`,
//! `evo/assetlocktx.cpp:148-152`), so 7-S5 and 9-S5 still reject below assumevalid. That the
//! validators are in the mode is shown by each row's mode control: the row's probe, its defect
//! taken out, at the same height on the same parent, with one fault added, a spend of a setup
//! coinbase's P2PKH output with an empty scriptSig, which fails its script. Below assumevalid both
//! accept it; its positive twin, the same block buried by [`TWO_WEEKS`] descendants with
//! `-assumevalid` at the last, has its scripts checked and gets Core's script reject. Every BAV
//! validator runs `-par=1`, so the reject is the failing input's own (`validation.cpp:2763-2765`
//! with `:2234`), not the check queue's `block-validation-failed` (`:2789-2791`).
//!
//! The probes: 7-OK the valid signal, 7-S5 the signal signed by another key, both on the
//! template; 9-OK the producer's block with the valid unlock, version 1 and 2
//! ([`QuorumChain::mined_with`]); 9-S5 the version 1 block with the unlock signed by another key
//! in its place (no pool fault: Core's CbTx counts the unlock). The 7 rows' mode control is the
//! template with the valid signal and the spend; the 9 rows' is the producer's block with the
//! spend.

use std::path::Path;
use std::sync::Arc;

use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{Header, Transaction, TxIn, TxOut};
use dash_consensus::{Block, Tx};
use dash_num::CompactTarget;
use dash_primitives::{BlockHeader, TxType};

use super::probes::{STRANGER, UNUSED_INDEX, without};
use super::{MNHF, NEWEST, QuorumChain, Signal, TIP, Unlock, sign};
use crate::{EHF_BIT, Probe, probe_block};

/// Regtest's `nPowTargetSpacing` (`chainparams.cpp:839`) and the bound of `validation.cpp:2579`.
pub const SPACING: i64 = 150;
pub const TWO_WEEKS_S: i64 = 60 * 60 * 24 * 7 * 2;
/// A BAV probe's descendants: more than two weeks of proof-equivalent time.
pub const BURIED: usize = 8_065;
/// A positive twin's: exactly two weeks, so its scripts are checked.
pub const TWO_WEEKS: usize = 8_064;
const _: () = assert!(BURIED as i64 * SPACING > TWO_WEEKS_S);
const _: () = assert!(TWO_WEEKS as i64 * SPACING == TWO_WEEKS_S);

/// `n` headers on `block`, each at its target: `block`'s version, merkle root and bits, a
/// second later every fifth header (so each is past the median of the eleven before it,
/// `validation.cpp` ContextualCheckBlockHeader, and the last is 1,613 s after `block`, inside the
/// two hours either validator allows ahead of its clock).
pub fn descendants(block: &Block, n: usize, pow: &PowParams) -> Vec<Header> {
    let top = *block.header().raw();
    let target = CompactTarget::new(top.bits);
    let mut prev = block.hash();
    (0..n)
        .map(|i| {
            let raw = BlockHeader {
                prev_hash: prev,
                time: top.time + 1 + u32::try_from(i / 5).unwrap(),
                ..top
            };
            let header = (0..)
                .map(|nonce| {
                    Header::new(BlockHeader {
                        nonce,
                        ..raw
                    })
                })
                .find(|h| check_proof_of_work(h.hash().as_bytes(), target, pow))
                .unwrap();
            prev = header.hash();
            header
        })
        .collect()
}

/// The mode controls' fault: a spend of [`QuorumChain::coinbase`], a P2PKH output, with an empty
/// scriptSig (its `OP_DUP` finds an empty stack), paying its whole value to the fixture's P2PKH
/// script: no fee, so the block's coinbase stays right.
pub fn failing_spend(chain: &QuorumChain) -> Arc<Tx> {
    let (prevout, value) = chain.coinbase;
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value,
            script_pubkey: chain.p2pkh.clone().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// A BAV probe of `row`: `block` under `below`.
fn buried(
    row: &'static str,
    name: String,
    expect: &'static str,
    block: Arc<Block>,
    below: Vec<Header>,
) -> Probe {
    Probe {
        row,
        name,
        expect,
        block,
        below: Some(below),
    }
}

/// `row`'s mode control `block` (the row's probe without its defect, `sound`, with the failing
/// `spend` after its txs) under [`BURIED`] descendants, and its positive twin, the same block under
/// the first [`TWO_WEEKS`] of them.
fn controls(
    out: &mut Vec<Probe>,
    row: &'static str,
    name: &str,
    (block, sound, spend): (&Arc<Block>, &Block, &Tx),
    pow: &PowParams,
) {
    let txids = |b: &Block| b.txs().iter().map(|t| t.txid()).collect::<Vec<_>>();
    assert_eq!(
        txids(block),
        [txids(sound), vec![spend.txid()]].concat(),
        "{row}: the mode control is the sound probe's txs and the failing spend"
    );
    let below = descendants(block, BURIED, pow);
    let twin = below[..TWO_WEEKS].to_vec();
    out.push(buried(
        row,
        format!("{name}, a failing script"),
        "bav_mode_control",
        block.clone(),
        below,
    ));
    out.push(buried(
        row,
        format!("{name}, a failing script, two weeks deep"),
        "bav_twin",
        block.clone(),
        twin,
    ));
}

/// The J0b-2 rows' probes, mode controls and positive twins (see the module docs).
pub fn probes(chain: &QuorumChain, binary: &Path, scratch: &Path, pow: &PowParams) -> Vec<Probe> {
    let mut out = Vec::new();
    let t = &chain.template;
    let stranger = sign::key(STRANGER);
    let spend = failing_spend(chain);
    let probe = |row, name: &str, block: &Arc<Block>| {
        buried(row, name.to_owned(), "bav", block.clone(), descendants(block, BURIED, pow))
    };

    let mnhf = &chain.quorums[MNHF];
    let signal = Signal {
        bit: EHF_BIT,
        quorum: mnhf.hash,
    };
    let valid = signal.signed(&mnhf.key);
    let signalled = probe_block(t, std::slice::from_ref(&valid), pow);
    let forged = probe_block(t, &[signal.signed(&stranger)], pow);
    let control = probe_block(t, &[valid, spend.clone()], pow);
    out.push(probe("7-OK", "the MNHF quorum's signature", &signalled));
    controls(&mut out, "7-OK", "the MNHF quorum's signature", (&control, &signalled, &spend), pow);
    out.push(probe("7-S5", "another key's signature", &forged));
    controls(&mut out, "7-S5", "the MNHF quorum's signature", (&control, &signalled, &spend), pow);

    let newest = &chain.quorums[NEWEST];
    for version in [1, 2] {
        let unlock = Unlock {
            version,
            index: UNUSED_INDEX,
            outputs: 1,
            requested: TIP,
            quorum: newest.hash,
        };
        let tx = unlock.signed(&newest.key, &chain.p2pkh);
        let block = Arc::new(chain.mined_with(binary, scratch, &tx, pow));
        let control = probe_block(&block, std::slice::from_ref(&spend), pow);
        let sound = (&control, &*block, &*spend);
        let name = format!("payload version {version}, the producer's block");
        if version == 1 {
            let forged =
                probe_block(&without(&block, &tx), &[unlock.signed(&stranger, &chain.p2pkh)], pow);
            out.push(probe("9-S5", "the producer's block, another key's signature", &forged));
            controls(&mut out, "9-S5", &name, sound, pow);
        }
        out.push(probe("9-OK", &name, &block));
        controls(&mut out, "9-OK", &name, sound, pow);
    }
    out
}

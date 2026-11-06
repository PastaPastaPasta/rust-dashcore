//! The pure rules of Core's `ConnectBlock` (CH3 §2.5, §2.6, §2.8; CH3a slice a1): a block's script
//! and lock-time flags, the BIP30 exemption, the assumevalid decision and the InstantSend input
//! predicate.
//!
//! `ConnectBlock` itself is dashd-chainstate's (CH3a); these functions read only their arguments.
//! The proof-equivalent time the assumevalid decision needs is
//! [`crate::pow::block_proof_equivalent_time`], and the per-block coin statistics are
//! [`crate::coins::stats`].

use dash_params::BuriedDeployment;

use crate::Tx;
use crate::ctx::{BlockId, Deployment, DeploymentView};
use crate::script::{BuriedScriptRules, ScriptFlags};
use crate::tx::{LockTimeFlags, block_lock_time_flags};

/// `GetBlockScriptFlags(pindex, chainman)`: the script flags `ConnectBlock` checks `block`'s inputs
/// with. Each buried deployment is asked **at** the block (`DeploymentActiveAt`), so a deployment
/// whose height is the block's own height already applies to it.
///
/// Every flag it can set is in `MANDATORY_SCRIPT_VERIFY_FLAGS` ([`ScriptFlags::MANDATORY_V24_0`]).
// Core: validation.cpp:2419-2448 GetBlockScriptFlags
pub fn block_script_flags(deployments: &dyn DeploymentView, block: BlockId) -> ScriptFlags {
    let at = |dep| deployments.active_at(block, Deployment::Buried(dep));
    // Core asks DERSIG, CLTV, CSV and BIP147 in this order; the fields evaluate in it.
    crate::script::block_script_flags(BuriedScriptRules {
        dersig: at(BuriedDeployment::DerSig),
        cltv: at(BuriedDeployment::Cltv),
        csv: at(BuriedDeployment::Csv),
        bip147: at(BuriedDeployment::Bip147),
    })
}

/// `nLockTimeFlags`: `LOCKTIME_VERIFY_SEQUENCE` when CSV is active **at** `block`.
// Core: validation.cpp:2626-2629 ConnectBlock
pub fn lock_time_flags(deployments: &dyn DeploymentView, block: BlockId) -> LockTimeFlags {
    block_lock_time_flags(deployments.active_at(block, Deployment::Buried(BuriedDeployment::Csv)))
}

/// Core's `IsBIP30Repeat`: whether `block` repeats an earlier coinbase's txid and is therefore
/// exempt from `ConnectBlock`'s BIP30 probe. Dash has no such block ("Dash blockchain does not have
/// any BIP30 violations"), so this is false for every block: Bitcoin's two exempt heights (91842
/// and 91880) do not exist here (CH3 D6). It stays a named function so the quirk census finds it.
///
/// Its sibling `IsBIP30Unspendable` (`validation.cpp:6791-6795`) is false too, and no Dash code
/// calls it: Dash's coinstats index has no BIP30 branch ([`crate::coins::stats`]).
// Core: validation.cpp:6785-6789 IsBIP30Repeat
pub const fn is_bip30_repeat(_block: BlockId) -> bool {
    false
}

/// `60 * 60 * 24 * 7 * 2`: `ConnectBlock` checks the scripts of a block on the assumed-valid chain
/// when the best header's proof-equivalent time ahead of it is at most this (two weeks).
// Core: validation.cpp:2569
pub const ASSUMEVALID_MAX_EQUIVALENT_TIME: i64 = 60 * 60 * 24 * 7 * 2;

/// What `ConnectBlock` has resolved about a non-genesis block when it decides whether to check its
/// scripts (CH3 §2.8; `validation.cpp:2542-2571`). The caller resolves each against its block index
/// and its best header (Core's `m_best_header`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssumeValidFacts {
    /// Condition 1: `-assumevalid` names a block (`!AssumedValidBlock().IsNull()`) and the block
    /// index holds it (`m_block_index.find(...) != end()`).
    pub assumed_valid_indexed: bool,
    /// Condition 2: `assumed_valid->GetAncestor(pindex->nHeight) == pindex`.
    pub on_assumed_valid_chain: bool,
    /// Condition 3: `m_best_header->GetAncestor(pindex->nHeight) == pindex`.
    pub on_best_header_chain: bool,
    /// Condition 4: `m_best_header->nChainWork >= MinimumChainWork()`.
    pub best_header_has_min_work: bool,
    /// Condition 5's input: `GetBlockProofEquivalentTime(*m_best_header, *pindex,
    /// *m_best_header, params)` ([`crate::pow::block_proof_equivalent_time`]).
    ///
    /// Core computes it only when conditions 1–4 hold. It has no side effect and it is ignored
    /// otherwise, so a caller may compute it eagerly; one that skips it then may pass any value.
    pub equivalent_time: i64,
}

/// `fScriptChecks`: whether `ConnectBlock` checks the block's input scripts. They are skipped only
/// when conditions 1–4 of [`AssumeValidFacts`] hold and the best header is more than two weeks of
/// proof-equivalent time ahead (condition 5); then `fScriptChecks` is
/// `equivalent_time <= ASSUMEVALID_MAX_EQUIVALENT_TIME`, which is false.
///
/// The same flag is `ProcessSpecialTxsInBlock`'s `fCheckCbTxMerkleRoots` (`validation.cpp:2664`),
/// which gates, besides the input scripts (`validation.cpp:2649`, `:2752`): the CbTx payload,
/// version and merkle-root checks and the check that a CbTx exists from DIP0003
/// (`evo/specialtxman.cpp:816-839`), `CheckCreditPoolDiffForBlock` (it runs only on a CbTx read
/// under the flag, `:882`), the special transactions' signature checks (`check_sigs`, `:871`) and
/// the quorum commitments' BLS checks (`m_qblockman.ProcessBlock(.., fBLSChecks)`, `:895`). The
/// genesis block returns before this decision.
// Core: validation.cpp:2542-2571 ConnectBlock
pub fn script_checks(facts: &AssumeValidFacts) -> bool {
    if facts.assumed_valid_indexed
        && facts.on_assumed_valid_chain
        && facts.on_best_header_chain
        && facts.best_header_has_min_work
    {
        return facts.equivalent_time <= ASSUMEVALID_MAX_EQUIVALENT_TIME;
    }
    true
}

/// `instantsend::HasLockInputs`: whether an InstantSend lock can cover `tx`, i.e. it is not a
/// coinbase and has an input or is an asset unlock (whose lock input is derived from its index,
/// `GetLockInputs`). `ConnectBlock`'s InstantSend conflict loop skips every other transaction.
// Core: instantsend/lock.cpp:59-62 HasLockInputs; used at validation.cpp:2805
pub fn has_lock_inputs(tx: &Tx) -> bool {
    !tx.is_coinbase() && (!tx.inputs().is_empty() || tx.is_asset_unlock_payload())
}

#[cfg(test)]
mod tests {
    use dash_params::Network;
    use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};

    use super::*;
    use crate::HeaderCtx;
    use crate::ctx::ChainAncestry;
    use crate::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
    use crate::params::test_support::{chain, header_ctx};

    /// A chain whose `BlockId(h)` is the block at height `h` (buried deployments read only the
    /// height).
    struct Linear;

    impl ChainAncestry for Linear {
        fn header(&self, id: BlockId) -> HeaderCtx {
            header_ctx(id.0, 0, 0)
        }

        fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
            (height <= id.0).then_some(BlockId(height))
        }

        fn prev(&self, id: BlockId) -> Option<BlockId> {
            id.0.checked_sub(1).map(BlockId)
        }
    }

    /// Core's buried heights (`chainparams.cpp`): DERSIG (BIP66), CLTV (BIP65), CSV, BIP147.
    /// Mainnet :180-183, testnet :389-392, devnet :570-574, regtest :813-816.
    const HEIGHTS: [(Network, [u32; 4]); 4] = [
        (Network::Main, [245_817, 619_382, 622_944, 939_456]),
        (Network::Testnet3, [2_075, 2_431, 8_064, 4_300]),
        (Network::Devnet, [1, 1, 1, 1]),
        (Network::Regtest, [1, 1, 1, 0]),
    ];

    const FLAGS: [ScriptFlags; 4] = [
        ScriptFlags::DERSIG,
        ScriptFlags::CHECKLOCKTIMEVERIFY,
        ScriptFlags::CHECKSEQUENCEVERIFY,
        ScriptFlags::NULLDUMMY,
    ];

    /// Each buried height ±1 on the four networks: a flag applies from its own height, so the
    /// block at the activation height is checked with it and the one below is not.
    #[test]
    fn script_and_lock_time_flags_at_each_boundary() {
        for (network, heights) in HEIGHTS {
            let chain = chain(network);
            let (cache, ehf) = (VersionBitsCache::new(), NoEhfSignals);
            let d = Deployments::new(&chain, &Linear, &ehf, &cache);
            for &boundary in &heights {
                for height in boundary.saturating_sub(1)..=boundary + 1 {
                    let block = BlockId(height);
                    let mut expected = ScriptFlags::P2SH;
                    for (flag, &h) in FLAGS.iter().zip(&heights) {
                        if height >= h {
                            expected = expected | *flag;
                        }
                    }
                    assert_eq!(block_script_flags(&d, block), expected, "{network:?} {height}");
                    let csv = height >= heights[2];
                    assert_eq!(
                        lock_time_flags(&d, block),
                        if csv {
                            LockTimeFlags::VERIFY_SEQUENCE
                        } else {
                            LockTimeFlags::NONE
                        },
                        "{network:?} {height}"
                    );
                }
            }
        }
    }

    /// The flags are the deployments at the block: the block just below an activation height
    /// lacks the flag although the deployment is active *after* it.
    #[test]
    fn flags_follow_active_at_not_active_after() {
        let chain = chain(Network::Main);
        let (cache, ehf) = (VersionBitsCache::new(), NoEhfSignals);
        let d = Deployments::new(&chain, &Linear, &ehf, &cache);
        let below = BlockId(622_943);
        assert!(d.active_after(Some(below), Deployment::Buried(BuriedDeployment::Csv)));
        assert!(!block_script_flags(&d, below).contains(ScriptFlags::CHECKSEQUENCEVERIFY));
        assert_eq!(lock_time_flags(&d, below), LockTimeFlags::NONE);
        assert_eq!(block_script_flags(&d, BlockId(0)), ScriptFlags::P2SH);
    }

    #[test]
    fn no_block_repeats_a_coinbase() {
        for h in [0, 1, 91_842, 91_880, 1_000_000, u32::MAX] {
            assert!(!is_bip30_repeat(BlockId(h)));
        }
    }

    const SKIP: AssumeValidFacts = AssumeValidFacts {
        assumed_valid_indexed: true,
        on_assumed_valid_chain: true,
        on_best_header_chain: true,
        best_header_has_min_work: true,
        equivalent_time: ASSUMEVALID_MAX_EQUIVALENT_TIME + 1,
    };

    /// Turns one of [`AssumeValidFacts`]' conditions off.
    type Flip = fn(&mut AssumeValidFacts);

    /// CH3 D44: with every condition met the scripts are skipped; flipping any one condition alone
    /// turns the checks back on.
    #[test]
    fn assumevalid_each_condition_alone_turns_checks_on() {
        assert!(!script_checks(&SKIP));
        let flips: [(&str, Flip); 4] = [
            ("1 not indexed", |f| f.assumed_valid_indexed = false),
            ("2 not on its chain", |f| f.on_assumed_valid_chain = false),
            ("3 not under the best header", |f| f.on_best_header_chain = false),
            ("4 below minimum work", |f| f.best_header_has_min_work = false),
        ];
        for (name, flip) in flips {
            let mut facts = SKIP;
            flip(&mut facts);
            assert!(script_checks(&facts), "{name}");
        }
        // Condition 5: up to two weeks ahead, two weeks exactly included, the scripts are checked.
        assert_eq!(ASSUMEVALID_MAX_EQUIVALENT_TIME, 1_209_600);
        for time in [i64::MIN, -1, 0, 1_209_600] {
            assert!(
                script_checks(&AssumeValidFacts {
                    equivalent_time: time,
                    ..SKIP
                }),
                "{time}"
            );
        }
        assert!(!script_checks(&AssumeValidFacts {
            equivalent_time: i64::MAX,
            ..SKIP
        }));
    }

    fn tx(version: i16, tx_type: u16, inputs: Vec<TxIn>, payload: Vec<u8>) -> Tx {
        Tx::new(Transaction {
            version,
            tx_type: TxType::new(tx_type),
            inputs,
            outputs: vec![TxOut {
                value: 1,
                script_pubkey: vec![0x51].into(),
            }],
            lock_time: 0,
            extra_payload: payload,
        })
    }

    fn input(prevout: OutPoint) -> TxIn {
        TxIn {
            prevout,
            script_sig: Default::default(),
            sequence: u32::MAX,
        }
    }

    #[test]
    fn lock_inputs_exist_for_spends_and_asset_unlocks_only() {
        let spend = input(OutPoint {
            hash: TxHash::from([7; 32]),
            index: 0,
        });
        let coinbase = input(OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        });
        assert!(has_lock_inputs(&tx(1, 0, vec![spend.clone()], vec![])));
        assert!(!has_lock_inputs(&tx(1, 0, vec![coinbase.clone()], vec![])));
        // A special transaction without inputs (a quorum commitment, type 6) has none.
        assert!(!has_lock_inputs(&tx(3, 6, vec![], vec![0])));
        // An asset unlock (version 3 or more, type 9, a payload of exactly
        // ASSET_UNLOCK_PAYLOAD_SIZE bytes) has one without inputs: `IsAssetUnlockPayload` reads
        // the version, the type and the payload's size, not its contents.
        let size = crate::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE;
        assert!(has_lock_inputs(&tx(3, 9, vec![], vec![0; size])));
        assert!(has_lock_inputs(&tx(4, 9, vec![], vec![0xff; size])));
        assert!(!has_lock_inputs(&tx(3, 9, vec![], vec![0; size - 1])));
        assert!(!has_lock_inputs(&tx(3, 9, vec![], vec![0; size + 1])));
        // Type 9 below the special version is not an asset unlock.
        assert!(!has_lock_inputs(&tx(2, 9, vec![], vec![0; size])));
    }
}

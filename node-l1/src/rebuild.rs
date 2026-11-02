//! Field edits on base-sdk values whose fields are private (the getter port, DEC-1055): copy the
//! fields out, edit the copy, rebuild the value through its plain constructor.
//!
//! Every constructor used here stores each argument as given and sets no other field (the port
//! tool's "plain" constructors: `impl_new!`, `codec_type!`, `codec_payload!` and `TxOut::from_duffs`,
//! `{ Self { value, script_pubkey } }`), so an edit gives the value the old in-place field write
//! gave. The ProTx payloads are not here: their `new`s reset the signature (see
//! [`crate::protx_mirror`]).

use bitcoin_primitives::script::{ScriptPubKeyBuf, ScriptSigBuf};
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsGlobalSig};
use dash_pkc::ecdsa::EcdsaPkHash as PubKeyHash;
use dash_primitives::{
    AssetLock, AssetUnlock, CoinbaseCommitment, CollateralShare, Commitment, DynBitset, MerkleRoot,
    MnHardFork, OutPoint, PayoutShare, QuorumHash, QuorumVvecHash, Transaction, TxHash, TxIn,
    TxOut, TxType,
};

/// A base-sdk value rebuilt from its fields.
pub trait Rebuild: Sized {
    /// The value's fields, public, in the constructor's order.
    type Fields;

    /// The fields, read through base-sdk's getters.
    fn fields(&self) -> Self::Fields;

    /// The value, through its plain constructor.
    fn rebuild(fields: Self::Fields) -> Self;

    /// Applies `f` to the fields and rebuilds the value from them, as the in-place write did.
    fn edit<R>(&mut self, f: impl FnOnce(&mut Self::Fields) -> R) -> R {
        let mut fields = self.fields();
        let r = f(&mut fields);
        *self = Self::rebuild(fields);
        r
    }
}

macro_rules! rebuild {
    (
        $(#[$attr:meta])*
        $ty:ident => $fields:ident via $ctor:ident { $($kind:ident $field:ident: $fty:ty),+ $(,)? }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $fields {
            $(pub $field: $fty,)+
        }

        impl Rebuild for $ty {
            type Fields = $fields;

            fn fields(&self) -> $fields {
                $fields { $($field: rebuild!(@read $kind self $field),)+ }
            }

            fn rebuild(f: $fields) -> Self {
                $ty::$ctor($(f.$field),+)
            }
        }
    };
    (@read copy $s:ident $field:ident) => {
        $s.$field()
    };
    (@read get $s:ident $field:ident) => {
        $s.$field().clone()
    };
}

rebuild! {
    /// `TxOut`'s fields; `TxOut::from_duffs` (`TxOut::new` takes an `Amount`).
    TxOut => TxOutFields via from_duffs { copy value: i64, get script_pubkey: ScriptPubKeyBuf }
}

rebuild! {
    /// `OutPoint`'s fields.
    OutPoint => OutPointFields via new { copy txid: TxHash, copy index: u32 }
}

rebuild! {
    /// `TxIn`'s fields.
    TxIn => TxInFields via new { copy prevout: OutPoint, get script_sig: ScriptSigBuf, copy sequence: u32 }
}

rebuild! {
    /// `PayoutShare`'s fields.
    PayoutShare => PayoutShareFields via new { get script_payout: ScriptPubKeyBuf, copy reward: u16 }
}

rebuild! {
    /// `CollateralShare`'s fields.
    CollateralShare => CollateralShareFields via new {
        copy amount: i64,
        get script_refund: ScriptPubKeyBuf,
        get script_reward: ScriptPubKeyBuf,
        copy key_id_owner: PubKeyHash,
    }
}

rebuild! {
    /// `Transaction`'s fields.
    Transaction => TransactionFields via new {
        copy version: i16,
        copy tx_type: TxType,
        get inputs: Vec<TxIn>,
        get outputs: Vec<TxOut>,
        copy lock_time: u32,
        get extra_payload: Vec<u8>,
    }
}

rebuild! {
    /// `CoinbaseCommitment`'s fields.
    CoinbaseCommitment => CoinbaseCommitmentFields via new {
        copy version: u16,
        copy height: i32,
        copy merkle_root_mn_list: MerkleRoot,
        copy merkle_root_quorums: Option<MerkleRoot>,
        copy best_cl_height_diff: Option<u64>,
        get best_cl_signature: Option<BlsGlobalSig>,
        copy credit_pool_balance: Option<i64>,
        copy merkle_root_asset_unlocks: Option<MerkleRoot>,
    }
}

rebuild! {
    /// `AssetLock`'s fields.
    AssetLock => AssetLockFields via new { copy version: u8, get credit_outputs: Vec<TxOut> }
}

rebuild! {
    /// `MnHardFork`'s fields.
    MnHardFork => MnHardForkFields via new {
        copy version: u8,
        copy version_bit: u8,
        copy quorum_hash: QuorumHash,
        get sig: BlsEagerSig,
    }
}

rebuild! {
    /// `AssetUnlock`'s fields.
    AssetUnlock => AssetUnlockFields via new {
        copy version: u8,
        copy index: u64,
        copy fee: u32,
        copy requested_height: u32,
        copy quorum_hash: QuorumHash,
        get quorum_sig: BlsGlobalSig,
    }
}

rebuild! {
    /// `Commitment`'s fields (a quorum commitment); a bitset is built with `DynBitset::new`.
    Commitment => CommitmentFields via new {
        copy version: u16,
        copy llmq_type: LlmqType,
        copy quorum_hash: QuorumHash,
        copy quorum_index: Option<i16>,
        get signers: DynBitset,
        get valid_members: DynBitset,
        get quorum_public_key: BlsEagerPk,
        copy quorum_vvec_hash: QuorumVvecHash,
        get quorum_sig: BlsEagerSig,
        get members_sig: BlsEagerSig,
    }
}

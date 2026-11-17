//! Chain contexts and the contextual entry points (ABI 6).
//!
//! A [`Context`] describes the chain state a contextual Core function reads: a
//! window of headers ending at the tip, version-bits deployment states, the tip's
//! masternode list, a coins view, the members of quorums and a few flags. The shim
//! builds Core's own objects from it (`src/test/oracle/oracle.h`, "chain contexts").

use super::*;

/// `ThresholdState`: a version-bits deployment's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdState {
    Defined = 0,
    Started = 1,
    LockedIn = 2,
    Active = 3,
    Failed = 4,
}

impl TryFrom<u8> for ThresholdState {
    type Error = u8;

    /// Core's enum value; the unknown value back as the error.
    fn try_from(state: u8) -> std::result::Result<Self, u8> {
        Ok(match state {
            0 => Self::Defined,
            1 => Self::Started,
            2 => Self::LockedIn,
            3 => Self::Active,
            4 => Self::Failed,
            other => return Err(other),
        })
    }
}

/// `Consensus::DEPLOYMENT_V24` (both pins).
pub const DEPLOYMENT_V24: u8 = 1;
/// `Consensus::DEPLOYMENT_EVO_SHARES` (v24 only).
pub const DEPLOYMENT_EVO_SHARES: u8 = 2;

/// An unspent output in a context's coins view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCoin {
    pub txid: [u8; 32],
    pub vout: u32,
    pub height: u32,
    pub coinbase: bool,
    pub amount: i64,
    pub script_pubkey: Vec<u8>,
}

/// What `llmq::utils::GetAllQuorumMembers()` returns for one quorum: each member a
/// `CDeterministicMN` as evodb stores it (`SER_DISK`, `CLIENT_VERSION`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumMembers {
    pub llmq_type: u8,
    pub quorum_hash: [u8; 32],
    pub members: Vec<Vec<u8>>,
}

/// The coins view of a context, and where its best block is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinsView {
    /// The view's best block is null. `RebuildListFromBlock()` then skips the
    /// collateral re-check it runs only when connecting at the tip.
    Unchecked(Vec<ContextCoin>),
    /// The view's best block is the tip, as when a node connects a block there.
    AtTip(Vec<ContextCoin>),
}

impl CoinsView {
    fn coins(&self) -> &[ContextCoin] {
        match self {
            CoinsView::Unchecked(coins) | CoinsView::AtTip(coins) => coins,
        }
    }
}

/// The node's checkpoint setting and the checkpoints its block index knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkpoints {
    /// `-checkpoints=0`: the comparison is a checkpoint-disabled node's.
    Disabled,
    /// Checkpoints on, and the node's best known header is at `best_header_height` (at
    /// least the tip's): it knows every checkpoint up to there.
    Enabled {
        best_header_height: i32,
    },
}

/// The tip's masternode list, or its explicit absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnList {
    /// The context holds no list: a call that reads it fails with [`ErrorKind::Context`].
    Absent,
    /// A `CDeterministicMNList` as evodb stores it, with the tip's hash and height, or
    /// the empty default list (no masternodes).
    Supplied(Vec<u8>),
}

/// The chain state of a contextual call (see the module docs and `oracle.h`).
///
/// There is deliberately no `Default`, and [`Context::new`] takes every field: each one
/// can change Core's verdict, so a differential test must state it rather than have both
/// sides inherit a silent default (the wrong era, a missing list, no checkpoints).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    /// Height of `headers[0]`.
    pub first_height: i32,
    /// Consecutive 80-byte headers; the last is the tip.
    pub headers: Vec<[u8; 80]>,
    /// `bls::bls_legacy_scheme` during the call (true before V19).
    pub bls_legacy: bool,
    /// `(DeploymentPos, state)` for the block after the tip; unlisted ones are DEFINED.
    pub deployments: Vec<(u8, ThresholdState)>,
    /// The node's adjusted (mock) time during the call; 0 = unset (the header check
    /// refuses it).
    pub adjusted_time: i64,
    pub checkpoints: Checkpoints,
    pub coins: CoinsView,
    pub mn_list: MnList,
    /// What `GetAllQuorumMembers()` returns for each quorum the call may ask about.
    pub quorums: Vec<QuorumMembers>,
}

/// Coin records as the shim reads them (`std::vector` of `COutPoint`, height,
/// coinbase flag, `CTxOut`).
pub(crate) fn write_coins(out: &mut Vec<u8>, coins: &[ContextCoin]) {
    write_compact_size(out, coins.len() as u64);
    for coin in coins {
        out.extend(coin.txid);
        out.extend(coin.vout.to_le_bytes());
        out.extend(coin.height.to_le_bytes());
        out.push(coin.coinbase.into());
        out.extend(coin.amount.to_le_bytes());
        write_bytes(out, &coin.script_pubkey);
    }
}

impl Context {
    /// A context with every field given explicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        first_height: i32,
        headers: Vec<[u8; 80]>,
        bls_legacy: bool,
        deployments: Vec<(u8, ThresholdState)>,
        adjusted_time: i64,
        checkpoints: Checkpoints,
        coins: CoinsView,
        mn_list: MnList,
        quorums: Vec<QuorumMembers>,
    ) -> Self {
        Self {
            first_height,
            headers,
            bls_legacy,
            deployments,
            adjusted_time,
            checkpoints,
            coins,
            mn_list,
            quorums,
        }
    }

    /// A first guess at the size of a list derived from the supplied one.
    fn list_capacity(&self) -> usize {
        let list = match &self.mn_list {
            MnList::Supplied(list) => list.len(),
            MnList::Absent => 0,
        };
        list + 64 * 1024
    }

    /// The context in the shim's format (`oracle.h`, format 2).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![2u8];
        out.extend(self.first_height.to_le_bytes());
        write_compact_size(&mut out, self.headers.len() as u64);
        for header in &self.headers {
            out.extend(header);
        }
        out.push(self.bls_legacy.into());
        write_compact_size(&mut out, self.deployments.len() as u64);
        for &(pos, state) in &self.deployments {
            out.extend([pos, state as u8]);
        }
        out.extend(self.adjusted_time.to_le_bytes());
        let (enabled, best_header_height) = match self.checkpoints {
            Checkpoints::Disabled => (0u8, 0i32),
            Checkpoints::Enabled {
                best_header_height,
            } => (1, best_header_height),
        };
        out.push(enabled);
        out.extend(best_header_height.to_le_bytes());
        match &self.mn_list {
            MnList::Absent => {
                out.push(0);
                write_bytes(&mut out, &[]);
            }
            MnList::Supplied(list) => {
                out.push(1);
                write_bytes(&mut out, list);
            }
        }
        out.push(matches!(self.coins, CoinsView::AtTip(_)).into());
        write_coins(&mut out, self.coins.coins());
        write_compact_size(&mut out, self.quorums.len() as u64);
        for quorum in &self.quorums {
            out.push(quorum.llmq_type);
            out.extend(quorum.quorum_hash);
            write_compact_size(&mut out, quorum.members.len() as u64);
            for member in &quorum.members {
                write_bytes(&mut out, member);
            }
        }
        out
    }
}

/// A commitment the node mined on its chain: what `CQuorumBlockProcessor::ProcessCommitment()`
/// wrote for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinedCommitment {
    pub llmq_type: u8,
    /// A `CFinalCommitment`, network serialization.
    pub commitment: Vec<u8>,
    /// Height of the block that mined it: above the quorum base, at most the tip.
    pub mined_height: i32,
}

/// The credit pool's asset-unlock indexes (`CCreditPool::indexes`), as Core's `CRangesSet`
/// stores them: `[begin, end)` ranges, `end == 0` for one that runs to `u64::MAX`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnlockIndexes {
    /// Core's deserializer refuses ranges that are empty, unordered, overlapping or adjacent;
    /// [`UnlockIndexes::from_indexes`] builds the canonical ones.
    pub ranges: Vec<(u64, u64)>,
}

impl UnlockIndexes {
    /// The canonical ranges holding exactly `indexes`.
    pub fn from_indexes(indexes: impl IntoIterator<Item = u64>) -> Self {
        let mut sorted: Vec<u64> = indexes.into_iter().collect();
        sorted.sort_unstable();
        sorted.dedup();
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for index in sorted {
            let end = index.wrapping_add(1);
            match ranges.last_mut() {
                Some(last) if last.1 == index => last.1 = end,
                _ => ranges.push((index, end)),
            }
        }
        Self {
            ranges,
        }
    }

    /// Core's `CRangesSet` encoding.
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        write_compact_size(out, self.ranges.len() as u64);
        for &(begin, end) in &self.ranges {
            out.extend(begin.to_le_bytes());
            out.extend(end.to_le_bytes());
        }
    }
}

/// What the block path reads besides the chain context: the block index off the chain, the
/// node's mined commitments and the credit pool's indexes (`oracle.h`, "special transactions
/// on the block path").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockContext {
    /// Known headers off the chain; each one's previous block is a context header or an
    /// earlier fork header.
    pub fork_headers: Vec<[u8; 80]>,
    /// The node's mined commitments: one left out is one Core does not have.
    pub mined: Vec<MinedCommitment>,
    pub unlock_indexes: UnlockIndexes,
}

impl BlockContext {
    /// The shim's block-context bytes (format 1).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![1u8];
        write_compact_size(&mut out, self.fork_headers.len() as u64);
        for header in &self.fork_headers {
            out.extend(header);
        }
        write_compact_size(&mut out, self.mined.len() as u64);
        for mined in &self.mined {
            out.push(mined.llmq_type);
            write_bytes(&mut out, &mined.commitment);
            out.extend(mined.mined_height.to_le_bytes());
        }
        self.unlock_indexes.write(&mut out);
        out
    }
}

/// Result of [`Oracle::dml_apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmlApply {
    /// Core's verdict on the block's masternode-list changes.
    pub verdict: Verdict,
    /// The new list (`CDeterministicMNList`, `SER_DISK`), when accepted.
    pub new_list: Option<Vec<u8>>,
    /// The same list's `SerializeCanonicalMNList` bytes (sorted by proTxHash), when accepted.
    pub canonical: Option<Vec<u8>>,
    /// `GetMNPayee(tip)` of the tip's list: the masternode this block pays.
    pub payee: Option<[u8; 32]>,
}

/// The stage of [`Oracle::dml_block`] that rejected a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmlStage {
    /// v24's shared-collateral template check or `CheckSpecialTxInner()`.
    Check,
    /// `CSpecialTxProcessor::BuildNewListFromBlock()`.
    Build,
    /// `CDeterministicMNManager::ProcessBlock()`.
    Process,
}

/// Result of [`Oracle::dml_block`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmlBlock {
    /// Core's verdict.
    pub verdict: Verdict,
    /// The stage that rejected, `None` when accepted.
    pub stage: Option<DmlStage>,
    /// When accepted after DIP0003: the new list's canonical bytes (block hash set) and
    /// `CanonicalMNListHash`.
    pub list: Option<(Vec<u8>, [u8; 32])>,
    /// `GetMNPayee(tip)` of the tip's list (none before DIP0003).
    pub payee: Option<[u8; 32]>,
}

/// What `CDeterministicMNManager::ProcessBlock()` left behind for a block
/// ([`Oracle::dml_block_effects`], ABI 26).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmlEffects {
    /// The `dmn_D4` row it wrote (`oldList.BuildDiff(newList)`, `SER_DISK`): only for a block
    /// accepted with `ProcessBlock()` run (a rejected block's evodb transaction is dropped).
    pub diff_row: Option<Vec<u8>>,
    /// The `dmn_S3` row (the new list, `SER_DISK`), when it also wrote one.
    pub snapshot_row: Option<Vec<u8>>,
    /// The proTxHashes whose platform ban `ResetPlatformBan()` reset (internal byte order,
    /// ascending), also when `ProcessBlock()` then rejected the block.
    pub resets: Vec<[u8; 32]>,
}

impl DmlEffects {
    /// The shim's encoding (oracle.h `oracle_dml_block_effects`).
    fn decode(mut bytes: &[u8]) -> Option<DmlEffects> {
        use crate::addrman::{take_hash, take_list, take_u8, take_vec};
        let r = &mut bytes;
        let diff_row = take_vec(r)?;
        // A byte, not `take_bool`: a flag above 1 is a malformed encoding.
        let has_snapshot = take_u8(r)?;
        let snapshot_row = take_vec(r)?;
        let resets = take_list(r, take_hash)?;
        if !r.is_empty() || has_snapshot > 1 || (has_snapshot == 0 && !snapshot_row.is_empty()) {
            return None;
        }
        Some(DmlEffects {
            diff_row: (!diff_row.is_empty()).then_some(diff_row),
            snapshot_row: (has_snapshot == 1).then_some(snapshot_row),
            resets,
        })
    }
}

/// Result of [`Oracle::check_tx_inputs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxInputs {
    pub verdict: Verdict,
    /// The fee, when accepted.
    pub fee: Option<i64>,
}

/// Result of [`Oracle::sequence_locks`]: `CalculateSequenceLocks()` and
/// `EvaluateSequenceLocks()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceLocks {
    pub min_height: i32,
    pub min_time: i64,
    pub satisfied: bool,
}

/// A BLS object kind (`ORACLE_BLS_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlsKind {
    /// `CBLSPublicKey` (G1, 48 bytes)
    PublicKey = 0,
    /// `CBLSSignature` (G2, 96 bytes)
    Signature = 1,
    /// `CBLSSecretKey` (32 bytes)
    SecretKey = 2,
}

/// How Core decodes a BLS object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlsMode {
    /// `CBLSWrapper::Unserialize`: opposite-scheme fallback, round trip, throws when malleable.
    Eager = 0,
    /// `CBLSLazyWrapper::Unserialize` then `Get()`: no fallback, never throws.
    Lazy = 1,
}

/// A decoded BLS object: VALID or INVALID (the third class, Core throwing, is
/// `ErrorKind::Decode`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlsDecoded {
    pub valid: bool,
    /// The object serialized again under the same scheme (zeros when invalid).
    pub reserialized: Vec<u8>,
}

/// One op's record from [`Oracle::bls_exec`](crate::Oracle::bls_exec).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlsRecord {
    /// `ORACLE_OK`, `ORACLE_REJECT` or the op's error status.
    pub status: i32,
    /// relic's error code on the calling thread after Core's call returned, read without
    /// clearing it (ABI 26; the field was reserved and 0 before).
    pub relic_code: i32,
    /// The op's output: a serialization, a plaintext or nothing.
    pub output: Vec<u8>,
    /// With `ORACLE_BLS_FLAG_LOG`: each wrapped relic call Core's call made from outside
    /// relic, in order, with the code after it returned.
    pub log: Option<Vec<RelicCall>>,
}

/// One entry of a [`BlsRecord`]'s relic call log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelicCall {
    pub symbol: String,
    pub code: i32,
}

/// A decoded special-transaction payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPayload {
    /// Whether the decode read every byte (`GetTxPayload()` fails otherwise).
    pub consumed_all: bool,
    pub reencoded: Vec<u8>,
}

impl Oracle {
    /// `CSpecialTxProcessor::CheckSpecialTx()` against the context's tip.
    pub fn check_special_tx(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        check_sigs: bool,
    ) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.check_special_tx)(
                    tx.as_ptr(),
                    tx.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    check_sigs.into(),
                    state,
                )
            })
        })
    }

    /// `CSpecialTxProcessor::CheckSpecialTxInner()` as `ProcessSpecialTxsInBlock()` calls it for
    /// a transaction of a block on top of the context's tip: every special type, MNHF signals
    /// and asset unlocks included, against `block`'s quorums and indexes.
    pub fn check_special_tx_block(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        block: &BlockContext,
        check_sigs: bool,
    ) -> Result<Verdict> {
        self.check_special_tx_block_bytes(chain, tx, ctx, &block.serialize(), check_sigs)
    }

    /// [`Oracle::check_special_tx_block`] with the block context as bytes.
    fn check_special_tx_block_bytes(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        block: &[u8],
        check_sigs: bool,
    ) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.check_special_tx_block)(
                    tx.as_ptr(),
                    tx.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    block.as_ptr(),
                    block.len(),
                    check_sigs.into(),
                    state,
                )
            })
        })
    }

    /// `CSpecialTxProcessor::RebuildListFromBlock()` from the context's masternode list.
    pub fn dml_apply(&self, chain: &Chain, block: &[u8], ctx: &Context) -> Result<DmlApply> {
        let initial = ctx.list_capacity();
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let (mut payee, mut has_payee) = ([0u8; 32], 0 as c_int);
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            // REJECT passes with_buffers' status check; its buffers are then discarded.
            let (new_list, canonical) =
                self.with_buffers(initial, |out, len, canonical, canonical_len| {
                    let status = unsafe {
                        (self.api.dml_apply)(
                            block.as_ptr(),
                            block.len(),
                            ctx.as_ptr(),
                            ctx.len(),
                            out,
                            len,
                            canonical,
                            canonical_len,
                            payee.as_mut_ptr(),
                            &mut has_payee,
                            &mut state,
                        )
                    };
                    accepted = status == ffi::ORACLE_OK;
                    status
                })?;
            Ok(DmlApply {
                verdict: block_verdict(accepted, &state),
                new_list: accepted.then_some(new_list),
                canonical: accepted.then_some(canonical),
                payee: (has_payee != 0).then_some(payee),
            })
        })
    }

    /// The masternode-list part of `ProcessSpecialTxsInBlock()` for a block after the
    /// context's tip (`oracle_dml_block`): the check loop, `BuildNewListFromBlock()` and,
    /// unless `just_check`, `CDeterministicMNManager::ProcessBlock()`.
    pub fn dml_block(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        check_sigs: bool,
        just_check: bool,
    ) -> Result<DmlBlock> {
        let initial = ctx.list_capacity();
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let (mut payee, mut has_payee, mut hash, mut stage) =
                ([0u8; 32], 0 as c_int, [0u8; 32], 0 as c_int);
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let canonical = self.with_buffer(initial, |out, len| {
                let status = unsafe {
                    (self.api.dml_block)(
                        block.as_ptr(),
                        block.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        check_sigs.into(),
                        just_check.into(),
                        &mut stage,
                        out,
                        len,
                        hash.as_mut_ptr(),
                        payee.as_mut_ptr(),
                        &mut has_payee,
                        &mut state,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            dml_block_result(
                accepted,
                stage,
                &state,
                canonical,
                hash,
                (has_payee != 0).then_some(payee),
            )
        })
    }

    /// [`Oracle::dml_block`] with what `ProcessBlock()` left behind
    /// (`oracle_dml_block_effects`, ABI 26): its evodb rows on acceptance and the platform-ban
    /// resets it made.
    pub fn dml_block_effects(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        check_sigs: bool,
        just_check: bool,
    ) -> Result<(DmlBlock, DmlEffects)> {
        let initial = ctx.list_capacity();
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let (mut payee, mut has_payee, mut hash, mut stage) =
                ([0u8; 32], 0 as c_int, [0u8; 32], 0 as c_int);
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let (canonical, effects) = self.with_buffers(initial, |out, len, fx, fx_len| {
                let status = unsafe {
                    (self.api.dml_block_effects)(
                        block.as_ptr(),
                        block.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        check_sigs.into(),
                        just_check.into(),
                        &mut stage,
                        out,
                        len,
                        hash.as_mut_ptr(),
                        payee.as_mut_ptr(),
                        &mut has_payee,
                        &mut state,
                        fx,
                        fx_len,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            let effects = DmlEffects::decode(&effects).ok_or_else(|| Error {
                kind: ErrorKind::Exception,
                message: "oracle_dml_block_effects: undecodable effects".into(),
            })?;
            // The list buffer is read only for an accepted block (`dml_block_result`).
            let result = dml_block_result(
                accepted,
                stage,
                &state,
                canonical,
                hash,
                (has_payee != 0).then_some(payee),
            )?;
            Ok((result, effects))
        })
    }

    /// The JSON `protx listdiff` prints for two lists (without the two heights and
    /// without `collateralAddress`, which needs a transaction index).
    pub fn dml_listdiff_json(&self, chain: &Chain, from: &[u8], to: &[u8]) -> Result<String> {
        self.on_chain(chain, || {
            let bytes = self.with_buffer(from.len() + to.len() + 4096, |out, len| unsafe {
                (self.api.dml_listdiff_json)(
                    from.as_ptr(),
                    from.len(),
                    to.as_ptr(),
                    to.len(),
                    out.cast(),
                    len,
                )
            })?;
            let json = CStr::from_bytes_until_nul(&bytes).expect("NUL-terminated JSON");
            Ok(json.to_string_lossy().into_owned())
        })
    }

    /// `Consensus::CheckTxInputs()` over a coins view holding `coins`.
    pub fn check_tx_inputs(
        &self,
        tx: &[u8],
        coins: &[ContextCoin],
        spend_height: i32,
    ) -> Result<TxInputs> {
        self.shared(|| {
            let mut records = Vec::new();
            write_coins(&mut records, coins);
            let mut fee = 0i64;
            let verdict = self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.check_tx_inputs)(
                    tx.as_ptr(),
                    tx.len(),
                    records.as_ptr(),
                    records.len(),
                    spend_height,
                    &mut fee,
                    state,
                )
            })?;
            let fee = verdict.is_accept().then_some(fee);
            Ok(TxInputs {
                verdict,
                fee,
            })
        })
    }

    /// `CalculateSequenceLocks()` and `EvaluateSequenceLocks()` for `tx` in the block
    /// after the context's tip; `prev_heights[i]` is the height of input i's coin.
    pub fn sequence_locks(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        prev_heights: &[i32],
        flags: i32,
    ) -> Result<SequenceLocks> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let (mut min_height, mut min_time, mut satisfied) = (0i32, 0i64, 0 as c_int);
            self.ok(unsafe {
                (self.api.sequence_locks)(
                    tx.as_ptr(),
                    tx.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    prev_heights.as_ptr(),
                    prev_heights.len(),
                    flags,
                    &mut min_height,
                    &mut min_time,
                    &mut satisfied,
                )
            })?;
            Ok(SequenceLocks {
                min_height,
                min_time,
                satisfied: satisfied != 0,
            })
        })
    }

    /// `ContextualCheckTransaction()` for the block after the context's tip.
    pub fn contextual_check_tx(&self, chain: &Chain, tx: &[u8], ctx: &Context) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.contextual_check_tx)(
                    tx.as_ptr(),
                    tx.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    state,
                )
            })
        })
    }

    /// `ContextualCheckBlockHeader()` of a header after the context's tip (needs
    /// `ctx.adjusted_time`).
    pub fn contextual_check_header(
        &self,
        chain: &Chain,
        header: &[u8; 80],
        ctx: &Context,
    ) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(ResultCode::Block, |state| unsafe {
                (self.api.contextual_check_header)(header.as_ptr(), ctx.as_ptr(), ctx.len(), state)
            })
        })
    }

    /// `ContextualCheckBlock()` of a block after the context's tip.
    pub fn contextual_check_block(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
    ) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(ResultCode::Block, |state| unsafe {
                (self.api.contextual_check_block)(
                    block.as_ptr(),
                    block.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    state,
                )
            })
        })
    }

    /// `CFinalCommitment::Verify()` with the quorum base block and members from the
    /// context. A rejection carries Core's LLMQ log line as `debug_message`.
    pub fn verify_commitment(
        &self,
        chain: &Chain,
        qc: &[u8],
        ctx: &Context,
        check_sigs: bool,
    ) -> Result<Verdict> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            self.verdict(
                |_| ResultCode::Bool,
                |state| unsafe {
                    (self.api.verify_commitment)(
                        qc.as_ptr(),
                        qc.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        check_sigs.into(),
                        state,
                    )
                },
            )
        })
    }

    /// Decode a BLS object as Core does under the legacy or basic scheme (A1 §1.4.1).
    pub fn bls_decode(
        &self,
        kind: BlsKind,
        mode: BlsMode,
        legacy: bool,
        bytes: &[u8],
    ) -> Result<BlsDecoded> {
        self.shared(|| {
            let mut valid: c_int = 0;
            let reserialized = self.with_buffer(96, |out, len| unsafe {
                (self.api.bls_decode)(
                    kind as c_int,
                    mode as c_int,
                    legacy.into(),
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut valid,
                    out,
                    len,
                )
            })?;
            Ok(BlsDecoded {
                valid: valid != 0,
                reserialized,
            })
        })
    }

    /// `CBLSSignature::VerifyInsecure()` with both objects decoded under the scheme.
    pub fn bls_verify(
        &self,
        legacy: bool,
        pubkey: &[u8; 48],
        sig: &[u8; 96],
        msg: &[u8; 32],
    ) -> Result<bool> {
        self.shared(|| {
            self.status(unsafe {
                (self.api.bls_verify)(legacy.into(), pubkey.as_ptr(), sig.as_ptr(), msg.as_ptr())
            })
        })
    }

    /// Run a script of Core's BLS operations (`oracle_bls_exec`, ABI 14; relic's code, the
    /// call log and the element ops ABI 26) and return one record per op, or fewer when an
    /// op under `ORACLE_BLS_FLAG_LOG` threw. A malformed script is `ErrorKind::Argument`
    /// (or `Decode` when truncated); an op's own failure is its record's status.
    pub fn bls_exec(&self, script: &[u8]) -> Result<Vec<BlsRecord>> {
        self.shared(|| self.run_bls_exec(script, None))
    }

    /// Test only (the call log's selftest, BS11a D59): run `script` as
    /// [`bls_exec`](Self::bls_exec) with relic's code set to `code` after the `nth` (from 1)
    /// windowed call of the wrapped relic `symbol` (with `anywhere`, its nth call in or
    /// outside a window), before the log reads it. The shim clears the code when the script
    /// ends. `ErrorKind::Unsupported` when the archive carries no relic wrappers.
    pub fn bls_exec_injecting(
        &self,
        script: &[u8],
        symbol: &str,
        nth: u32,
        code: i32,
        anywhere: bool,
    ) -> Result<Vec<BlsRecord>> {
        let symbol = CString::new(symbol).map_err(|_| Error {
            kind: ErrorKind::Argument,
            message: "a symbol with a NUL byte".into(),
        })?;
        self.shared(|| self.run_bls_exec(script, Some((&symbol, nth, code, anywhere))))
    }

    /// Test only (BS11a-2 D26, D42): set relic's code on the calling thread to `code`, as relic
    /// code run outside the oracle could leave it, then run `f`. An entry point that starts
    /// with the code set fails with `ErrorKind::Exception` and clears it.
    pub fn after_relic_code<T>(&self, code: i32, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.shared(|| self.ok(unsafe { (self.api.test_bls_set_code)(code) }))?;
        f(self)
    }

    /// Test only (BS11a-2 D42): arm the relic injection of
    /// [`bls_exec_injecting`](Self::bls_exec_injecting) for the calling thread's next oracle
    /// call, whatever its entry point, then run `f`.
    pub fn injecting<T>(
        &self,
        symbol: &str,
        nth: u32,
        code: i32,
        anywhere: bool,
        f: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        let symbol = CString::new(symbol).map_err(|_| Error {
            kind: ErrorKind::Argument,
            message: "a symbol with a NUL byte".into(),
        })?;
        self.shared(|| {
            self.ok(unsafe {
                (self.api.test_bls_inject)(symbol.as_ptr(), nth, code, anywhere.into())
            })
        })?;
        f(self)
    }

    /// Test only (BS11a-2 D35): `oracle_bls_exec` of a null script of length 1, which the shim
    /// refuses with `ErrorKind::Argument`.
    pub fn bls_exec_null_script(&self) -> Result<()> {
        self.shared(|| {
            self.with_buffer(16, |out, len| unsafe {
                (self.api.bls_exec)(std::ptr::null(), 1, out, len)
            })
            .map(drop)
        })
    }

    fn run_bls_exec(
        &self,
        script: &[u8],
        inject: Option<(&CStr, u32, i32, bool)>,
    ) -> Result<Vec<BlsRecord>> {
        let malformed = |what: &str| Error {
            kind: ErrorKind::Exception,
            message: format!("the BLS script's {what}"),
        };
        // An op's record can be 36 times its script bytes (SER: 3 in, 106 out), and a
        // logged op's log adds a few KiB (a legacy verify logs about 180 calls); a short
        // buffer makes the shim run the script twice, and the injection is armed for each run.
        let (logged, whole) = bls_script_logged(script);
        let capacity = script.len() * 36 + 256 + logged.iter().filter(|&&l| l).count() * (8 << 10);
        let out = self.with_buffer(capacity, |out, len| unsafe {
            if let Some((symbol, nth, code, anywhere)) = inject {
                let status =
                    (self.api.test_bls_inject)(symbol.as_ptr(), nth, code, anywhere.into());
                if status != ffi::ORACLE_OK {
                    return status;
                }
            }
            (self.api.bls_exec)(script.as_ptr(), script.len(), out, len)
        })?;
        // Core reads no op after a logged throw, so the walk may stop early only past one.
        let records =
            parse_bls_records(&out, &logged).ok_or_else(|| malformed("records are malformed"))?;
        let threw =
            records.last().is_some_and(|r| r.log.is_some() && r.status == ffi::ORACLE_E_EXCEPTION);
        if !whole && !threw {
            return Err(malformed("ops are malformed"));
        }
        Ok(records)
    }

    /// Deserialize a special transaction's extra payload as `GetTxPayload<T>()` does,
    /// with the BLS scheme flag set to `bls_legacy`. `GetTxPayload()` catches every exception,
    /// but only the deserializer refusing the bytes is `ErrorKind::Decode`; any other, and
    /// any in the write-back, is `ErrorKind::Exception` (BL-61).
    pub fn payload_decode(
        &self,
        tx_type: u16,
        bls_legacy: bool,
        payload: &[u8],
    ) -> Result<DecodedPayload> {
        self.shared(|| {
            let mut consumed_all: c_int = 0;
            let reencoded = self.with_buffer(payload.len() + 64, |out, len| unsafe {
                (self.api.payload_decode)(
                    tx_type,
                    bls_legacy.into(),
                    payload.as_ptr(),
                    payload.len(),
                    &mut consumed_all,
                    out,
                    len,
                )
            })?;
            Ok(DecodedPayload {
                consumed_all: consumed_all != 0,
                reencoded,
            })
        })
    }
}

/// Whether each op of an `oracle_bls_exec` script carries `ORACLE_BLS_FLAG_LOG`, walking
/// the operands oracle.h lists, and whether the walk reached the end: it stops at the first
/// op the shim would refuse.
fn bls_script_logged(mut s: &[u8]) -> (Vec<bool>, bool) {
    use ffi::*;
    fn skip(s: &mut &[u8], n: usize) -> Option<()> {
        *s = s.get(n..)?;
        Some(())
    }
    fn list(s: &mut &[u8], item: usize) -> Option<()> {
        let n = usize::try_from(crate::read_compact_size(s)?).ok()?;
        skip(s, n.checked_mul(item)?)
    }
    fn walk(s: &mut &[u8], op: u8, flags: u8) -> Option<()> {
        let recv = usize::from(flags & ORACLE_BLS_FLAG_INPLACE != 0);
        let (r, h) = (1, 32);
        match op {
            ORACLE_BLS_OP_DECODE => {
                skip(s, 2)?;
                list(s, 1)?;
                skip(s, r)?;
            }
            ORACLE_BLS_OP_SER | ORACLE_BLS_OP_ISVALID | ORACLE_BLS_OP_SER_HASH => skip(s, r)?,
            ORACLE_BLS_OP_PUBKEY | ORACLE_BLS_OP_NEG | ORACLE_BLS_OP_EQ | ORACLE_BLS_OP_IMPL_EQ => {
                skip(s, 2 * r)?
            }
            ORACLE_BLS_OP_SUB_SIG | ORACLE_BLS_OP_ADD => skip(s, 3 * r)?,
            ORACLE_BLS_OP_SIGN | ORACLE_BLS_OP_VERIFY | ORACLE_BLS_OP_MUL => skip(s, 2 * r + h)?,
            ORACLE_BLS_OP_HASH_TO_G2 => skip(s, h + r)?,
            ORACLE_BLS_OP_DH => skip(s, recv + 3 * r)?,
            ORACLE_BLS_OP_VERIFY_AGG => {
                skip(s, r)?;
                list(s, r)?;
                list(s, h)?;
            }
            ORACLE_BLS_OP_VERIFY_SEC => {
                skip(s, r)?;
                list(s, r)?;
                skip(s, h)?;
            }
            ORACLE_BLS_OP_AGG_SK | ORACLE_BLS_OP_AGG_PK | ORACLE_BLS_OP_AGG_SIG => {
                list(s, r)?;
                skip(s, r)?;
            }
            ORACLE_BLS_OP_AGG_SECURE => {
                list(s, r)?;
                list(s, r)?;
                skip(s, h + r)?;
            }
            ORACLE_BLS_OP_SK_SHARE | ORACLE_BLS_OP_PK_SHARE => {
                skip(s, recv)?;
                list(s, r)?;
                skip(s, h + r)?;
            }
            ORACLE_BLS_OP_RECOVER_SIG => {
                skip(s, recv)?;
                list(s, r)?;
                list(s, h)?;
                skip(s, r)?;
            }
            ORACLE_BLS_OP_IES_ENCRYPT => {
                skip(s, r + h)?;
                list(s, r)?;
                for _ in 0..crate::read_compact_size(s)? {
                    list(s, 1)?;
                }
            }
            ORACLE_BLS_OP_IES_DECRYPT | ORACLE_BLS_OP_IES_DECRYPT_BLOB => {
                skip(s, 4 + r)?;
                list(s, 1)?;
            }
            _ => return None,
        }
        Some(())
    }
    let mut logged = Vec::new();
    while let [op, flags, rest @ ..] = s {
        let (op, flags) = (*op, *flags);
        s = rest;
        if walk(&mut s, op, flags).is_none() {
            return (logged, false);
        }
        logged.push(flags & ORACLE_BLS_FLAG_LOG != 0);
    }
    let whole = s.is_empty();
    (logged, whole)
}

/// Split `oracle_bls_exec`'s output into its records, `log_flags` saying which ops carry a
/// call log: one record per op, or fewer when a logged op threw (it ends the script).
fn parse_bls_records(mut bytes: &[u8], log_flags: &[bool]) -> Option<Vec<BlsRecord>> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (head, rest) = bytes.split_at_checked(n)?;
        *bytes = rest;
        Some(head)
    }
    fn i32_le(bytes: &mut &[u8]) -> Option<i32> {
        Some(i32::from_le_bytes(take(bytes, 4)?.try_into().ok()?))
    }
    fn string<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
        let len = usize::try_from(crate::read_compact_size(bytes)?).ok()?;
        take(bytes, len)
    }
    fn read_log(bytes: &mut &[u8]) -> Option<Vec<RelicCall>> {
        (0..crate::read_compact_size(bytes)?)
            .map(|_| {
                let symbol = String::from_utf8(string(bytes)?.to_vec()).ok()?;
                Some(RelicCall {
                    symbol,
                    code: i32_le(bytes)?,
                })
            })
            .collect()
    }
    let mut records = Vec::new();
    for &logged in log_flags {
        let status = i32_le(&mut bytes)?;
        let relic_code = i32_le(&mut bytes)?;
        let output = string(&mut bytes)?.to_vec();
        let log = if logged {
            Some(read_log(&mut bytes)?)
        } else {
            None
        };
        records.push(BlsRecord {
            status,
            relic_code,
            output,
            log,
        });
        if logged && status == ffi::ORACLE_E_EXCEPTION {
            break;
        }
    }
    bytes.is_empty().then_some(records)
}

/// [`DmlBlock`] from the shim's outputs.
fn dml_block_result(
    accepted: bool,
    stage: c_int,
    state: &ffi::OracleValidation,
    canonical: Vec<u8>,
    hash: [u8; 32],
    payee: Option<[u8; 32]>,
) -> Result<DmlBlock> {
    let stage = match stage {
        ffi::ORACLE_DML_STAGE_NONE => None,
        ffi::ORACLE_DML_STAGE_CHECK => Some(DmlStage::Check),
        ffi::ORACLE_DML_STAGE_BUILD => Some(DmlStage::Build),
        ffi::ORACLE_DML_STAGE_PROCESS => Some(DmlStage::Process),
        other => {
            return Err(Error {
                kind: ErrorKind::Exception,
                message: format!("oracle_dml_block stage {other}"),
            });
        }
    };
    Ok(DmlBlock {
        verdict: block_verdict(accepted, state),
        stage,
        list: (accepted && !canonical.is_empty()).then_some((canonical, hash)),
        payee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bls_records_parse_exactly() {
        let record = |status: i32, code: i32, out: &[u8]| {
            let mut bytes = [status.to_le_bytes(), code.to_le_bytes()].concat();
            bytes.push(out.len() as u8);
            bytes.extend(out);
            bytes
        };
        let two = [record(0, 0, &[7; 3]), record(-4, 1, &[])].concat();
        let parsed = parse_bls_records(&two, &[false, false]).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            (parsed[0].status, parsed[0].relic_code, &parsed[0].output[..], &parsed[0].log),
            (0, 0, &[7u8; 3][..], &None)
        );
        assert_eq!((parsed[1].status, parsed[1].relic_code, parsed[1].output.len()), (-4, 1, 0));
        assert_eq!(parse_bls_records(&[], &[]), Some(vec![]));
        // A truncated header, an overrunning length, a partial trailing record or a
        // record with no op.
        assert_eq!(parse_bls_records(&two[..5], &[false]), None);
        assert_eq!(parse_bls_records(&two[..two.len() - 10], &[false, false]), None);
        assert_eq!(
            parse_bls_records(&[two.as_slice(), &[1]].concat(), &[false, false, false]),
            None
        );
        assert_eq!(parse_bls_records(&two, &[false]), None);
        // Fewer records than ops, with no logged op that threw.
        assert_eq!(parse_bls_records(&two, &[false, false, false]), None);

        // A logged record: the count, then each symbol and its code.
        let mut logged = record(0, 0, &[1]);
        logged.extend([2, 6]);
        logged.extend(b"ep_add");
        logged.extend(0i32.to_le_bytes());
        logged.extend([6]);
        logged.extend(b"bn_add");
        logged.extend(1i32.to_le_bytes());
        let parsed = parse_bls_records(&logged, &[true]).unwrap();
        let call = |symbol: &str, code| RelicCall {
            symbol: symbol.into(),
            code,
        };
        assert_eq!(parsed[0].log, Some(vec![call("ep_add", 0), call("bn_add", 1)]));
        assert_eq!(parse_bls_records(&logged[..logged.len() - 1], &[true]), None);
        assert_eq!(parse_bls_records(&logged, &[false]), None);
        // A logged op that threw ends the script: the later ops have no record.
        let mut threw = record(ffi::ORACLE_E_EXCEPTION, 1, &[]);
        threw.push(0);
        assert_eq!(parse_bls_records(&threw, &[true, false]).unwrap().len(), 1);
        assert_eq!(
            parse_bls_records(&[threw.as_slice(), &record(0, 0, &[])].concat(), &[true, false]),
            None
        );
    }

    /// The script walker finds every op's flags byte, operands as oracle.h lists them.
    #[test]
    fn bls_script_ops_are_walked() {
        use ffi::*;
        let (log, inplace) = (ORACLE_BLS_FLAG_LOG, ORACLE_BLS_FLAG_INPLACE);
        let h = [0u8; 32];
        let script = [
            &[ORACLE_BLS_OP_DECODE, log, 1, 0, 2, 0xaa, 0xbb, 3][..],
            &[ORACLE_BLS_OP_MUL, 0, 3][..],
            &h,
            &[4][..],
            &[ORACLE_BLS_OP_DH, inplace | log, 9, 1, 2, 4][..],
            &[ORACLE_BLS_OP_DH, 0, 1, 2, 4][..],
            &[ORACLE_BLS_OP_RECOVER_SIG, inplace, 5, 2, 1, 2, 1][..],
            &h,
            &[6][..],
            &[ORACLE_BLS_OP_IES_ENCRYPT, log, 1][..],
            &h,
            &[1, 2, 1, 2, 0xcc, 0xdd][..],
            &[ORACLE_BLS_OP_SER_HASH, log, 6][..],
        ]
        .concat();
        let flags = vec![true, false, true, false, false, true, true];
        assert_eq!(bls_script_logged(&script), (flags.clone(), true));
        assert_eq!(bls_script_logged(&script[..script.len() - 1]), (flags[..6].to_vec(), false));
        assert_eq!(bls_script_logged(&[27, 0, 0]), (vec![], false));
    }

    /// The block contexts [`BlockContext`] cannot produce are refused (oracle.h "block context"):
    /// another format, bytes after it, none at all.
    #[test]
    fn block_context_bytes_are_refused() {
        let oracle = Oracle::new(Pin::V24_0);
        let chain = Chain::regtest();
        // A regtest header at height 0 and a transaction with no inputs or outputs.
        let mut header = [0u8; 80];
        header[0] = 4;
        header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
        let ctx = Context::new(
            0,
            vec![header],
            false,
            vec![],
            1_500_000_000,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            MnList::Absent,
            vec![],
        );
        let tx = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let check =
            |block: &[u8]| oracle.check_special_tx_block_bytes(&chain, &tx, &ctx, block, true);

        let good = BlockContext::default().serialize();
        assert_eq!(check(&good), Ok(Verdict::Accept));
        for (block, what) in [
            ([&[2u8][..], &good[1..]].concat(), "format 2"),
            ([&good[..], &[0][..]].concat(), "a trailing byte"),
            (vec![], "no bytes"),
        ] {
            let err = check(&block).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{what}: {}", err.message);
        }
    }
}

//! `rpc/mining.cpp`'s commands over the chain the node provides ([`NodeChain`]):
//! `submitblock` (DEC-279), `submitheader` (RP1a-1), and the `generate*` commands (RP1a-1b)
//! over the node's block assembler ([`Miner`]); `getnetworkhashps` and `getmininginfo`
//! (RP1a-4) over the chain state ([`info`]). RP1a adds the rest of the file's commands on the
//! same seams.

use std::sync::{Arc, Mutex};

use dash_consensus::{
    Block, BlockValidationError, CoreSerialize as _, Header, Tx, decode_block, decode_header,
    decode_tx_exact,
};
use dash_primitives::BlockHash;

use crate::UniValue;
use crate::blockchain::{Chainman, NodeChain, ensure_chainman, no_chainman};
use crate::commands::internal_bug;
use crate::descriptor::{DescriptorError, script_from_descriptor};
use crate::key_io::{KeyIoParams, decode_destination};
use crate::protocol::{
    RPC_DESERIALIZATION_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY,
    RPC_INVALID_PARAMETER, RPC_METHOD_NOT_FOUND, RPC_MISC_ERROR, RPC_VERIFY_ERROR, RpcError,
};
use crate::request::{get_int_i32, get_int_u64, param, parse_hex, values};
use crate::server::{Command, NodeControl, RpcServer};

/// `DEFAULT_MAX_TRIES`.
// Core: rpc/mining.h:9
const DEFAULT_MAX_TRIES: u64 = 1_000_000;

/// Why the assembler made no block (dashd-mining's `TemplateError`, PLAN-RP1a §2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateRefusal {
    /// The readiness guard: the next block needs a rule this build does not assemble yet, named
    /// in the text. Answered -32603 "Couldn't create new block: <text>", a recorded divergence
    /// (DEC-607 D4, DEC-630): Core never refuses.
    NotReady(String),
    /// The `std::runtime_error` Core's `CreateNewBlock` throws, its full text: RPC_MISC_ERROR.
    Failed(String),
    /// The chainstate is gone: `EnsureChainman`'s error.
    NoChain,
}

/// `GenerateBlock`'s nonce loop's outcome (dashd-mining's `Grind`), decided in Core's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grind {
    /// The header meets its target.
    Found,
    /// The nonce reached `u32::MAX`: no block, and the caller builds a new template.
    NonceExhausted,
    /// `max_tries` ran out.
    OutOfTries,
    /// `ShutdownRequested()`.
    Shutdown,
}

/// `TestBlockValidity(state, ..., block, LookupBlockIndex(hashPrevBlock), fCheckPOW=false,
/// fCheckMerkleRoot=false)`'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockCheck {
    Valid,
    /// Refused: `state.GetRejectReason()`.
    Invalid(String),
    /// The block's parent is no longer the tip (`NotBestPrev`); `generateblock` builds a new
    /// template (PLAN-RP1a D17).
    TipMoved,
    /// Any other "not tested" answer (`getblocktemplate`'s lookup answers, which this call never
    /// asks for): an internal bug.
    NotTested(String),
    /// The chainstate is gone.
    NoChain,
}

/// The node's block assembler and nonce search, as the `generate*` commands reach them.
pub trait Miner: Send + Sync {
    /// `BlockAssembler(chainstate, node, mempool).CreateNewBlock(coinbase_script)`: the
    /// template's block, with a null merkle root and nonce 0. `use_mempool` is false for
    /// `generateblock`, which passes no mempool.
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        use_mempool: bool,
    ) -> Result<Block, TemplateRefusal>;

    /// `GenerateBlock`'s loop: sets the merkle root, then tries nonces upward, one of
    /// `max_tries` each, while `shutdown` is false.
    fn grind(&self, block: &mut Block, max_tries: &mut u64, shutdown: &dyn Fn() -> bool) -> Grind;

    /// [`BlockCheck`] for a block built on the template.
    fn test_block_validity(&self, block: Arc<Block>) -> BlockCheck;

    /// `BlockAssembler::m_last_block_size` and `m_last_block_num_txs`: `None` before the first
    /// assembly.
    fn last_block_stats(&self) -> Option<LastBlockStats>;
}

/// The last assembled block's `nBlockSize` and `nBlockTx` (`node/miner.cpp:259-260`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastBlockStats {
    pub size: u64,
    pub num_txs: u64,
}

/// What the `generate*` commands share.
struct Generator {
    chain: Arc<dyn NodeChain>,
    miner: Arc<dyn Miner>,
    control: Arc<dyn NodeControl>,
    key_io: KeyIoParams,
}

/// Core's `BlockValidationState`, as `BIP22ValidationResult` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationState {
    Valid,
    Invalid(BlockValidationError),
    /// `state.Error(msg)`: `ToString()` is the message.
    Error(String),
}

/// Core's `submitblock_StateCatcher`: the state `BlockChecked` gives the block `hash`.
pub struct StateCatcher {
    hash: BlockHash,
    /// `found` and `state`.
    state: Mutex<Option<ValidationState>>,
}

impl StateCatcher {
    pub fn new(hash: BlockHash) -> StateCatcher {
        StateCatcher {
            hash,
            state: Mutex::new(None),
        }
    }

    /// `BlockChecked(block, state)`: kept for the caught block, ignored for any other.
    pub fn block_checked(&self, block: &Block, state: impl FnOnce() -> ValidationState) {
        if block.hash() != self.hash {
            return;
        }
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = Some(state());
    }

    /// The caught state; `None` while `found` is false.
    pub fn state(&self) -> Option<ValidationState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// Registers every command of this module on `server`.
/// The `generate*` commands mine with `miner` and read addresses and keys with `key_io`.
pub fn register_mining_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    miner: Arc<dyn Miner>,
    key_io: KeyIoParams,
) {
    let c = Arc::clone(&chain);
    server.append_command(Command::new("submitblock", move |r| submitblock(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("submitheader", move |r| submitheader(&*c, &r.params)));
    let generator = Arc::new(Generator {
        chain,
        miner,
        control: Arc::clone(server.control()),
        key_io,
    });
    let g = Arc::clone(&generator);
    server.append_command(Command::new("generatetoaddress", move |r| {
        generatetoaddress(&g, &r.params)
    }));
    let g = Arc::clone(&generator);
    server.append_command(Command::new("generatetodescriptor", move |r| {
        generatetodescriptor(&g, &r.params)
    }));
    server.append_command(Command::new("generateblock", move |r| {
        generateblock(&generator, &r.params)
    }));
    server.append_command(Command::new("generate", |_| {
        let help = dash_rpc_spec::command("generate").expect("a v24 command").help;
        Err(RpcError::new(RPC_METHOD_NOT_FOUND, help))
    }));
}

/// Core's `BIP22ValidationResult`, for a conclusive state.
// Core: rpc/mining.cpp:531-548
fn bip22_validation_result(state: &ValidationState) -> Result<UniValue, RpcError> {
    match state {
        ValidationState::Valid => Ok(UniValue::Null),
        ValidationState::Error(msg) => Err(RpcError::new(RPC_VERIFY_ERROR, msg.clone())),
        ValidationState::Invalid(e) if e.reject_reason().is_empty() => Ok("rejected".into()),
        ValidationState::Invalid(e) => Ok(e.reject_reason().into()),
    }
}

/// Core's `DecodeHexBlk`: `IsHex`, then one block. Bytes after the block are ignored.
// Core: core_read.cpp:144-159
pub(crate) fn decode_hex_blk(hex: &str) -> Option<Block> {
    let bytes = parse_hex(hex)?;
    decode_block(&mut bytes.as_slice()).ok().map(|d| d.into_value())
}

/// Core's `DecodeHexTx`: `IsHex`, then exactly one transaction (`DecodeTx`).
// Core: core_read.cpp DecodeHexTx
pub(crate) fn decode_hex_tx(hex: &str) -> Option<Tx> {
    let bytes = parse_hex(hex)?;
    decode_tx_exact(&bytes).ok().map(|d| d.into_value())
}

/// Core's `DecodeHexBlockHeader`: `IsHex`, then one header. Bytes after it are ignored.
// Core: core_read.cpp:129-142
fn decode_hex_block_header(hex: &str) -> Option<Header> {
    let bytes = parse_hex(hex)?;
    decode_header(&mut bytes.as_slice()).ok().map(|d| d.into_value())
}

/// Core's `submitblock`. The second argument (BIP22's `dummy`) is ignored.
// Core: rpc/mining.cpp:1007-1066
fn submitblock(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let block = decode_hex_blk(param(params, 0).get_str()?)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
    if block.txs().first().is_none_or(|tx| !tx.is_coinbase()) {
        return Err(RpcError::new(
            RPC_DESERIALIZATION_ERROR,
            "Block does not start with a coinbase",
        ));
    }

    let chainman = ensure_chainman(chain)?;
    let hash = block.hash();
    if let Some(index) = chainman.lookup_block_index(&hash) {
        if index.valid_scripts {
            return Ok("duplicate".into());
        }
        if index.failed {
            return Ok("duplicate-invalid".into());
        }
    }

    let sc = Arc::new(StateCatcher::new(hash));
    let processed = chainman.process_new_block(Arc::new(block), true, &sc);
    if !processed.new_block && processed.accepted {
        return Ok("duplicate".into());
    }
    match sc.state() {
        None => Ok("inconclusive".into()),
        Some(state) => bip22_validation_result(&state),
    }
}

/// Core's `submitheader`: the header's parent must be indexed; then the header's state.
// Core: rpc/mining.cpp:1068-1105
fn submitheader(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let header = decode_hex_block_header(param(params, 0).get_str()?)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block header decode failed"))?;
    let chainman = ensure_chainman(chain)?;
    let prev = header.raw().prev_hash;
    if chainman.lookup_block_index(&prev).is_none() {
        return Err(RpcError::new(
            RPC_VERIFY_ERROR,
            format!("Must submit previous header ({prev}) first"),
        ));
    }
    match chainman.process_new_block_header(header) {
        ValidationState::Valid => Ok(UniValue::Null),
        ValidationState::Error(msg) => Err(RpcError::new(RPC_VERIFY_ERROR, msg)),
        ValidationState::Invalid(e) => Err(RpcError::new(RPC_VERIFY_ERROR, e.reject_reason())),
    }
}

/// `GenerateBlock`'s result: Core's `false`, or `true` with or without `block_out`.
enum Generated {
    /// `false`: tries ran out or shutdown was requested.
    Stopped,
    /// `true` with no block: the nonces ran out.
    NonceExhausted,
    Block(Arc<Block>),
}

impl Generator {
    fn shutdown_requested(&self) -> bool {
        self.control.shutdown_requested()
    }

    /// `BlockAssembler(...).CreateNewBlock(coinbase_script)`, its refusals as Core's errors.
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        use_mempool: bool,
    ) -> Result<Block, RpcError> {
        self.miner.create_new_block(coinbase_script, use_mempool).map_err(|e| match e {
            TemplateRefusal::NotReady(why) => {
                RpcError::new(RPC_INTERNAL_ERROR, format!("Couldn't create new block: {why}"))
            }
            TemplateRefusal::Failed(msg) => RpcError::Runtime(msg),
            TemplateRefusal::NoChain => no_chainman(),
        })
    }

    /// Core's `GenerateBlock`: the nonce search, then `ProcessNewBlock(block, force=true)` when
    /// `process_new_block`.
    // Core: rpc/mining.cpp:139-164
    fn generate_block(
        &self,
        chainman: &dyn Chainman,
        mut block: Block,
        max_tries: &mut u64,
        process_new_block: bool,
    ) -> Result<Generated, RpcError> {
        match self.miner.grind(&mut block, max_tries, &|| self.shutdown_requested()) {
            Grind::OutOfTries | Grind::Shutdown => return Ok(Generated::Stopped),
            Grind::NonceExhausted => return Ok(Generated::NonceExhausted),
            Grind::Found => {}
        }
        let block = Arc::new(block);
        if process_new_block {
            let catcher = Arc::new(StateCatcher::new(block.hash()));
            if !chainman.process_new_block(Arc::clone(&block), true, &catcher).accepted {
                return Err(RpcError::new(
                    RPC_INTERNAL_ERROR,
                    "ProcessNewBlock, block not accepted",
                ));
            }
        }
        Ok(Generated::Block(block))
    }

    /// Core's `generateBlocks`: blocks to `coinbase_script` until `n` are made, the tries run
    /// out, or shutdown is requested. `EnsureLLMQContext` has no case here: the node's LLMQ
    /// context exists whenever its chainstate does.
    // Core: rpc/mining.cpp:166-188
    fn generate_blocks(
        &self,
        chainman: &dyn Chainman,
        coinbase_script: &[u8],
        mut n: i32,
        mut max_tries: u64,
    ) -> Result<UniValue, RpcError> {
        let mut hashes = UniValue::new_array();
        while n > 0 && !self.shutdown_requested() {
            let block = self.create_new_block(coinbase_script, true)?;
            match self.generate_block(chainman, block, &mut max_tries, true)? {
                Generated::Stopped => break,
                Generated::NonceExhausted => {}
                Generated::Block(block) => {
                    n -= 1;
                    hashes.push_back(block.hash().to_string().into());
                }
            }
        }
        Ok(hashes)
    }
}

/// Core's `generatetodescriptor`.
// Core: rpc/mining.cpp:224-260
fn generatetodescriptor(g: &Generator, params: &UniValue) -> Result<UniValue, RpcError> {
    let num_blocks = get_int_i32(param(params, 0))?;
    let max_tries = match param(params, 2) {
        UniValue::Null => DEFAULT_MAX_TRIES,
        v => get_int_u64(v)?,
    };
    let coinbase_script = script_from_descriptor(param(params, 1).get_str()?, &g.key_io)
        .map_err(|e| descriptor_error(&*g.control, e))?;
    let chainman = ensure_chainman(&*g.chain)?;
    g.generate_blocks(&*chainman, &coinbase_script, num_blocks, max_tries)
}

/// A [`DescriptorError`]'s reply: the parser's error, which `generatetodescriptor` throws, and
/// the two `getScriptFromDescriptor` throws itself.
// Core: rpc/mining.cpp:190-203, :250
fn descriptor_error(control: &dyn NodeControl, e: DescriptorError) -> RpcError {
    match e {
        DescriptorError::Parse(error) => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, error),
        DescriptorError::Ranged => RpcError::new(
            RPC_INVALID_PARAMETER,
            "Ranged descriptor not accepted. Maybe pass through deriveaddresses first?",
        ),
        DescriptorError::CannotDerive => {
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Cannot derive script without private keys")
        }
        DescriptorError::Check(check) => {
            internal_bug(control, check, file!(), line!(), "getScriptFromDescriptor")
        }
    }
}

/// Core's `generatetoaddress`. `maxtries` is read as an `int` and stored in a `uint64_t`, so
/// -1 is 2^64-1.
// Core: rpc/mining.cpp:262-301
fn generatetoaddress(g: &Generator, params: &UniValue) -> Result<UniValue, RpcError> {
    let num_blocks = get_int_i32(param(params, 0))?;
    let max_tries = match param(params, 2) {
        UniValue::Null => DEFAULT_MAX_TRIES,
        v => i64::from(get_int_i32(v)?) as u64,
    };
    let destination = decode_destination(param(params, 1).get_str()?, &g.key_io)
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Error: Invalid address"))?;
    let chainman = ensure_chainman(&*g.chain)?;
    g.generate_blocks(&*chainman, &destination.script(), num_blocks, max_tries)
}

/// Core's `generateblock`. Until the node has a mempool (MP1a) no txid is in it.
// Core: rpc/mining.cpp:303-420
fn generateblock(g: &Generator, params: &UniValue) -> Result<UniValue, RpcError> {
    let address_or_descriptor = param(params, 0).get_str()?;
    let coinbase_script = match script_from_descriptor(address_or_descriptor, &g.key_io) {
        Ok(script) => script,
        Err(DescriptorError::Parse(_)) => decode_destination(address_or_descriptor, &g.key_io)
            .ok_or_else(|| {
                RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Error: Invalid address or descriptor")
            })?
            .script(),
        Err(e) => return Err(descriptor_error(&*g.control, e)),
    };

    let mut txs = Vec::new();
    for v in values(param(params, 1)) {
        let s = v.get_str()?;
        if s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(RpcError::new(
                RPC_INVALID_ADDRESS_OR_KEY,
                format!("Transaction {s} not in mempool."),
            ));
        }
        let tx = decode_hex_tx(s).map(Arc::new).ok_or_else(|| {
            RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                format!(
                    "Transaction decode failed for {s}. Make sure the tx has at least one input."
                ),
            )
        })?;
        txs.push(tx);
    }
    let process_new_block = match param(params, 2) {
        UniValue::Null => true,
        v => v.get_bool()?,
    };

    let chainman = ensure_chainman(&*g.chain)?;
    let block = loop {
        let template = g.create_new_block(&coinbase_script, false)?;
        // 1 coinbase + could have a few quorum commitments
        if template.txs().is_empty() {
            return Err(internal_bug(
                &*g.control,
                "block.vtx.size() >= 1",
                file!(),
                line!(),
                "generateblock",
            ));
        }
        let mut all = template.txs().to_vec();
        all.extend(txs.iter().cloned());
        let block = Arc::new(Block::new(*template.header(), all));
        match g.miner.test_block_validity(Arc::clone(&block)) {
            BlockCheck::Valid => {
                break Arc::try_unwrap(block)
                    .unwrap_or_else(|b| Block::new(*b.header(), b.txs().to_vec()));
            }
            BlockCheck::Invalid(reason) => {
                return Err(RpcError::new(
                    RPC_VERIFY_ERROR,
                    format!("TestBlockValidity failed: {reason}"),
                ));
            }
            // `GenerateBlock`'s shutdown answer: no rebuild once shutdown is requested.
            BlockCheck::TipMoved if g.shutdown_requested() => {
                return Err(RpcError::new(RPC_MISC_ERROR, "Failed to make block."));
            }
            BlockCheck::TipMoved => continue,
            BlockCheck::NotTested(answer) => {
                return Err(internal_bug(
                    &*g.control,
                    &format!("TestBlockValidity answered {answer}"),
                    file!(),
                    line!(),
                    "generateblock",
                ));
            }
            BlockCheck::NoChain => return Err(no_chainman()),
        }
    };

    let mut max_tries = DEFAULT_MAX_TRIES;
    let Generated::Block(block) =
        g.generate_block(&*chainman, block, &mut max_tries, process_new_block)?
    else {
        return Err(RpcError::new(RPC_MISC_ERROR, "Failed to make block."));
    };
    let mut obj = UniValue::new_object();
    obj.push_kv_end("hash", block.hash().to_string().into());
    if !process_new_block {
        let hex: String = block.core_serialize().iter().map(|b| format!("{b:02x}")).collect();
        obj.push_kv_end("hex", hex.into());
    }
    Ok(obj)
}

mod info;

pub use info::{MiningInfoParams, register_mining_info_commands};

#[cfg(test)]
mod tests;

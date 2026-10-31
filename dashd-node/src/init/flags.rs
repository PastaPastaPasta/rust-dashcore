//! Core's `AppInitParameterInteraction` step 3 (parameter-to-internal-flags): logging
//! categories, pruning, timeouts, the masternode and governance refusals, and the chainstate
//! options.

use dash_args::ArgsManager;
use dash_clock::MockClock;
use dash_params::LlmqType;
use dash_params::{ChainSetup, Network};
use dash_primitives::BlockHash;

use super::checks::NetSetup;
use super::consts::*;
use super::mempool::{MemPoolOptions, apply_args_man_options, stricter_than_default_relay_policy};
use super::parse::{
    ByteUnit, amount_err_msg, is_hex_number, parse_byte_units, parse_int32, parse_money, uint256_s,
};
use crate::InitFailure;
use crate::logging::{set_logging_categories, set_logging_level};
use crate::ui::Ui;

fn error(msg: impl Into<String>) -> InitFailure {
    InitFailure::Error(msg.into())
}

/// Core's `PruneTarget`: `nPruneTarget` and `fPruneMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prune {
    Off,
    /// `-prune=1`: blocks go only on `pruneblockchain`.
    Manual,
    /// A target in bytes.
    Target(u64),
}

impl Prune {
    pub fn enabled(self) -> bool {
        self != Prune::Off
    }
}

/// Core's `llmq::QvvecSyncMode` (without `Invalid`, which is an error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QvvecSyncMode {
    Always = 0,
    OnlyIfTypeMember = 1,
}

/// Core's `ChainstateManager::Options` as `ApplyArgsManOptions` sets them: `None` keeps the
/// chain's default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainmanOptions {
    pub check_block_index: Option<bool>,
    pub checkpoints_enabled: Option<bool>,
    /// `-minimumchainwork`, checked to be hex (`uint256S` reads it later).
    pub minimum_chain_work: Option<String>,
    /// `-assumevalid`, as `uint256S` reads it (the blob's own byte order).
    pub assumed_valid_block: Option<[u8; 32]>,
    pub max_tip_age_secs: Option<i64>,
}

impl ChainmanOptions {
    /// Core's `AssumedValidBlock()`: `-assumevalid`, else the chain's default; `None` when it
    /// is null, which verifies every block's scripts.
    // Core: validation.cpp:6770, node/chainstate.cpp:307-311
    pub fn assumed_valid(&self, params: &ChainSetup) -> Option<BlockHash> {
        let blob =
            self.assumed_valid_block.unwrap_or(*params.consensus.default_assume_valid.as_bytes());
        (blob != [0; 32]).then(|| BlockHash::from(blob))
    }

    /// `-checkblockindex` as `Flatten` resolves it: the option, else the chain's
    /// `fDefaultConsistencyChecks` (on for regtest only). V's `CheckBlockIndex` and the chain's
    /// header subset of it both follow it (BL-525).
    // Core: validation.cpp:6774, chainparams.cpp:291, :494, :686, :892
    pub fn check_block_index(&self, params: &ChainSetup) -> bool {
        self.check_block_index.unwrap_or(params.default_consistency_checks)
    }
}

/// What step 3 decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flags {
    pub prune: Prune,
    /// `nConnectTimeout` in milliseconds.
    pub connect_timeout_ms: i64,
    /// `peer_connect_timeout` in seconds.
    pub peer_connect_timeout_secs: i64,
    /// `nBytesPerSigOp`.
    pub bytes_per_sigop: i64,
    /// `-mocktime` (0: the clock is not mocked).
    pub mocktime: i64,
    /// `NODE_BLOOM` is signalled.
    pub peer_bloom_filters: bool,
    /// `-llmq-qvvec-sync`, one entry per type, in option order.
    pub qvvec_sync: Vec<(LlmqType, QvvecSyncMode)>,
    pub governance_enabled: bool,
    pub chainman: ChainmanOptions,
}

/// Core's `nConnectTimeout` in milliseconds: `-timeout` narrowed to an `int` (two's
/// complement, as GCC and Clang define it), and the default when that is not positive.
// Core: init.cpp:1365-1368, netbase.cpp:35
pub(crate) fn connect_timeout_ms(timeout: i64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = timeout as i32;
    if narrowed <= 0 {
        DEFAULT_CONNECT_TIMEOUT
    } else {
        i64::from(narrowed)
    }
}

/// Step 3 of Core's `AppInitParameterInteraction`.
// Core: init.cpp:1307-1453
pub fn internal_flags(
    ui: &Ui,
    args: &ArgsManager,
    chain: &ChainSetup,
    net: &NetSetup,
    clock: &MockClock,
) -> Result<Flags, InitFailure> {
    set_logging_categories(ui.logger(), args)?;
    set_logging_level(ui.logger(), args)?;

    let prune = prune_target(args)?;

    let connect_timeout_ms =
        connect_timeout_ms(args.get_int_arg("-timeout", DEFAULT_CONNECT_TIMEOUT)?);
    let peer_connect_timeout_secs =
        args.get_int_arg("-peertimeout", DEFAULT_PEER_CONNECT_TIMEOUT)?;
    if peer_connect_timeout_secs <= 0 {
        return Err(error("peertimeout must be a positive integer."));
    }

    if args.is_arg_set("-blockmintxfee") {
        let value = args.get_arg("-blockmintxfee", "")?;
        if parse_money(&value).is_none() {
            return Err(error(amount_err_msg("blockmintxfee", &value)));
        }
    }

    let bytes_per_sigop = args.get_int_arg("-bytespersigop", DEFAULT_BYTES_PER_SIGOP)?;

    // rdashd has no wallet: Core's `DummyWalletInit::ParameterInteraction` is a no-op too. The
    // wallet options are refused by the support table instead.

    // Core's `SetMockTime` asserts on a negative time; rdashd refuses it (a deviation).
    let mocktime = args.get_int_arg("-mocktime", 0)?;
    if mocktime < 0 {
        return Err(error(format!("-mocktime cannot be negative: {mocktime}")));
    }
    clock.set_mocktime(mocktime);

    let peer_bloom_filters = args.get_bool_arg("-peerbloomfilters", DEFAULT_PEERBLOOMFILTERS)?;

    if args.get_bool_arg("-reindex-chainstate", false)? {
        // Indexes that would be corrupted (Core #24630).
        let refuse = |index: &str| {
            error(format!(
                "-reindex-chainstate option is not compatible with -{index}. Please temporarily disable {index} while using -reindex-chainstate, or replace -reindex-chainstate with -reindex to fully rebuild all indexes."
            ))
        };
        if args.get_bool_arg("-coinstatsindex", DEFAULT_COINSTATSINDEX)? {
            return Err(refuse("coinstatsindex"));
        }
        if net.basic_filter_index {
            return Err(refuse("blockfilterindex"));
        }
        if args.get_bool_arg("-txindex", DEFAULT_TXINDEX)? {
            return Err(refuse("txindex"));
        }
    }

    let qvvec_sync = qvvec_sync_entries(args, chain)?.map_err(error)?;
    if !args.get_bool_arg("-llmq-data-recovery", DEFAULT_ENABLE_QUORUM_DATA_RECOVERY)?
        && !qvvec_sync.is_empty()
    {
        ui.init_warning(
            "-llmq-qvvec-sync set but recovery is disabled due to -llmq-data-recovery=0",
        );
    }

    if args.is_arg_set("-masternodeblsprivkey") {
        masternode_checks(ui, args, chain, bytes_per_sigop)?;
    }

    let governance_enabled = !args.get_bool_arg("-disablegovernance", !RDASHD_GOVERNANCE_ENABLE)?;
    // Governance looks up proposal collateral transactions in the transaction index.
    if governance_enabled
        && !args.get_bool_arg("-txindex", DEFAULT_TXINDEX)?
        && chain.network != Network::Regtest
    {
        return Err(error(
            "Transaction index can't be disabled with governance validation enabled. Either start with -disablegovernance command line switch or enable transaction index.",
        ));
    }
    // rdashd: only a set `-disablegovernance` warns, so a default start's stderr is dashd's
    // (DEC-510, SP1 D9).
    if !governance_enabled && args.is_arg_set("-disablegovernance") {
        let pruned = if prune.enabled() {
            " This is expected because you are running a pruned node."
        } else {
            ""
        };
        ui.init_warning(&format!("You are starting with governance validation disabled.{pruned}"));
    }

    let chainman = chainman_options(args)?;

    if args.get_bool_arg("-spv", false)? {
        spv_checks(args, net)?;
    }

    Ok(Flags {
        prune,
        connect_timeout_ms,
        peer_connect_timeout_secs,
        bytes_per_sigop,
        mocktime,
        peer_bloom_filters,
        qvvec_sync,
        governance_enabled,
        chainman,
    })
}

/// `-prune`: MiB to keep for block and undo files.
// Core: init.cpp:1313-1334
fn prune_target(args: &ArgsManager) -> Result<Prune, InitFailure> {
    let mib = args.get_int_arg("-prune", 0)?;
    if mib < 0 {
        return Err(error("Prune cannot be configured with a negative value."));
    }
    let bytes = (mib as u64).wrapping_mul(1024 * 1024);
    if mib == 1 {
        return Ok(Prune::Manual);
    }
    if bytes == 0 {
        return Ok(Prune::Off);
    }
    // Regtest tests pruning with 1 MB blocks.
    let min_mib = if args.get_bool_arg("-regtest", false)? {
        550
    } else {
        MIN_DISK_SPACE_FOR_BLOCK_FILES / 1024 / 1024
    };
    if bytes < min_mib * 1024 * 1024 {
        return Err(error(format!(
            "Prune configured below the minimum of {min_mib} MiB.  Please use a higher number."
        )));
    }
    Ok(Prune::Target(bytes))
}

/// The refusals for a masternode (`-masternodeblsprivkey`).
// Core: init.cpp:1394-1436
fn masternode_checks(
    ui: &Ui,
    args: &ArgsManager,
    chain: &ChainSetup,
    bytes_per_sigop: i64,
) -> Result<(), InitFailure> {
    if !args.get_bool_arg("-listen", DEFAULT_LISTEN)? && chain.require_routable_external_ip {
        return Err(error("Masternode must accept connections from outside, set -listen=1"));
    }
    if !args.get_bool_arg("-txindex", DEFAULT_TXINDEX)? {
        return Err(error("Masternode must have transaction index enabled, set -txindex=1"));
    }
    if !args.get_bool_arg("-peerbloomfilters", DEFAULT_PEERBLOOMFILTERS)? {
        return Err(error("Masternode must have bloom filters enabled, set -peerbloomfilters=1"));
    }
    if args.get_int_arg("-prune", 0)? > 0 {
        return Err(error("Masternode must have no pruning enabled, set -prune=0"));
    }
    if args.get_int_arg("-maxconnections", DEFAULT_MAX_PEER_CONNECTIONS)?
        < DEFAULT_MAX_PEER_CONNECTIONS
    {
        return Err(error(format!(
            "Masternode must be able to handle at least {DEFAULT_MAX_PEER_CONNECTIONS} connections, set -maxconnections={DEFAULT_MAX_PEER_CONNECTIONS}"
        )));
    }
    if args.get_bool_arg("-disablegovernance", !DEFAULT_GOVERNANCE_ENABLE)? {
        return Err(error("You can not disable governance validation on a masternode."));
    }
    if args.get_bool_arg("-blocksonly", DEFAULT_BLOCKSONLY)? {
        return Err(error("Masternode must relay transactions, set -blocksonly=0"));
    }
    if !args.get_bool_arg("-peerblockfilters", DEFAULT_PEERBLOCKFILTERS)? {
        return Err(error("Masternode must serve compact block filters, set -peerblockfilters=1"));
    }
    // Past the daily upload budget the node refuses filtered and historical blocks while still
    // advertising NODE_BLOOM; a cap that never triggers is no cap.
    let upload = args.get_arg("-maxuploadtarget", DEFAULT_MAX_UPLOAD_TARGET)?;
    if parse_byte_units(&upload, ByteUnit::M).unwrap_or(0) != 0 {
        return Err(error(
            "Masternode must serve blocks and SPV clients without an upload limit, set -maxuploadtarget=0",
        ));
    }
    let mut mempool = MemPoolOptions::default();
    apply_args_man_options(args, chain, ui.logger(), &mut mempool)?.map_err(error)?;
    // Regression tests give single masternodes a stricter policy on purpose
    // (feature_llmq_is_retroactive.py).
    let mut stricter = stricter_than_default_relay_policy(&mempool);
    if bytes_per_sigop > DEFAULT_BYTES_PER_SIGOP {
        stricter.push("-bytespersigop");
    }
    if !stricter.is_empty() && chain.network != Network::Regtest {
        return Err(error(format!(
            "Masternode must not use a stricter transaction relay policy than the default, which would make it skip InstantSend signing. Remove or restore: {}",
            stricter.join(", ")
        )));
    }
    Ok(())
}

/// rdashd's `-spv` refusals (SB0 P8): an SPV node keeps no blocks or undo data, so no index can
/// run on it.
fn spv_checks(args: &ArgsManager, net: &NetSetup) -> Result<(), InitFailure> {
    let indexes = [
        ("txindex", DEFAULT_TXINDEX),
        ("addressindex", DEFAULT_ADDRESSINDEX),
        ("spentindex", DEFAULT_SPENTINDEX),
        ("timestampindex", DEFAULT_TIMESTAMPINDEX),
        ("coinstatsindex", DEFAULT_COINSTATSINDEX),
    ];
    for (index, default) in indexes {
        if args.get_bool_arg(&format!("-{index}"), default)? {
            return Err(spv_refusal(index));
        }
    }
    if net.basic_filter_index {
        return Err(spv_refusal("blockfilterindex"));
    }
    Ok(())
}

fn spv_refusal(index: &str) -> InitFailure {
    error(format!("-{index} cannot be used with -spv: an SPV node keeps no blocks or undo data."))
}

/// Core's `GetEnabledQuorumVvecSyncEntries`; the inner error is the `std::invalid_argument`
/// text init reports.
// Core: llmq/options.cpp:64-110
pub fn qvvec_sync_entries(
    args: &ArgsManager,
    chain: &ChainSetup,
) -> Result<Result<Vec<(LlmqType, QvvecSyncMode)>, String>, dash_args::RuntimeError> {
    let mut entries: Vec<(LlmqType, QvvecSyncMode)> = Vec::new();
    for entry in args.get_args("-llmq-qvvec-sync")? {
        let mut fields = GetLine::new(&entry);
        let llmq_type = fields.next().filter(|s| !s.is_empty());
        let mode = fields.next().filter(|s| !s.is_empty());
        let too_many = fields.next().is_some();
        let (Some(llmq_type), Some(mode), false) = (llmq_type, mode, too_many) else {
            return Ok(Err(format!("Invalid format in -llmq-qvvec-sync: {entry}")));
        };
        let Some(t) = chain.consensus.llmqs.iter().copied().find(|t| t.to_string() == llmq_type)
        else {
            return Ok(Err(format!("Invalid llmqType in -llmq-qvvec-sync: {entry}")));
        };
        if entries.iter().any(|(e, _)| *e == t) {
            return Ok(Err(format!("Duplicated llmqType in -llmq-qvvec-sync: {entry}")));
        }
        let mode = match parse_int32(mode) {
            Some(0) => QvvecSyncMode::Always,
            Some(1) => QvvecSyncMode::OnlyIfTypeMember,
            _ => return Ok(Err(format!("Invalid mode in -llmq-qvvec-sync: {entry}"))),
        };
        entries.push((t, mode));
    }
    Ok(Ok(entries))
}

/// `std::getline(stream, field, ':')` over a string: a field ends at `:` or the end, and a read
/// at the end fails, so `a:b:` holds two fields and `a:b::` three.
struct GetLine<'a> {
    rest: Option<&'a str>,
}

impl<'a> GetLine<'a> {
    fn new(s: &'a str) -> GetLine<'a> {
        GetLine {
            rest: (!s.is_empty()).then_some(s),
        }
    }
}

impl<'a> Iterator for GetLine<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let rest = self.rest?;
        Some(match rest.split_once(':') {
            Some((field, tail)) => {
                self.rest = (!tail.is_empty()).then_some(tail);
                field
            }
            None => {
                self.rest = None;
                rest
            }
        })
    }
}

/// Core's `node::ApplyArgsManOptions(args, chainman_opts)`.
// Core: node/chainstatemanager_args.cpp:20-38
fn chainman_options(args: &ArgsManager) -> Result<ChainmanOptions, InitFailure> {
    let minimum_chain_work = args.get_arg_opt("-minimumchainwork")?;
    if let Some(value) = &minimum_chain_work
        && !is_hex_number(value)
    {
        return Err(error(format!("Invalid non-hex ({value}) minimum chain work value specified")));
    }
    Ok(ChainmanOptions {
        check_block_index: args.get_bool_arg_opt("-checkblockindex")?,
        checkpoints_enabled: args.get_bool_arg_opt("-checkpoints")?,
        minimum_chain_work,
        assumed_valid_block: args.get_arg_opt("-assumevalid")?.map(|value| uint256_s(&value)),
        max_tip_age_secs: args.get_int_arg_opt("-maxtipage")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::{checks, init_parameter_interaction, select_params, test_args, test_ui};

    /// `AppInit`'s order: the soft-sets, step 2 (which must pass), then step 3.
    fn run(argv: &[&str]) -> (Result<Flags, InitFailure>, Vec<String>) {
        let args = test_args(argv);
        let chain = select_params(&args, &args.get_chain_name().unwrap()).unwrap().chain;
        let (ui, stderr) = test_ui();
        init_parameter_interaction(&args, ui.logger()).unwrap();
        let net = checks::parameter_interaction(&ui, &args).unwrap();
        let flags = internal_flags(&ui, &args, &chain, &net, &MockClock::new());
        let stderr = stderr.lock().unwrap().clone();
        (flags, stderr)
    }

    fn err(argv: &[&str]) -> String {
        match run(argv).0 {
            Err(InitFailure::Error(e)) => e,
            other => panic!("{argv:?}: {other:?}"),
        }
    }

    #[test]
    fn prune_values() {
        assert_eq!(
            run(&["-prune=1", "-txindex=0", "-disablegovernance"]).0.unwrap().prune,
            Prune::Manual
        );
        assert_eq!(
            run(&["-prune=550", "-txindex=0", "-disablegovernance"]).0.unwrap().prune,
            Prune::Target(550 << 20)
        );
        assert_eq!(
            err(&["-prune=549", "-txindex=0", "-disablegovernance"]),
            "Prune configured below the minimum of 550 MiB.  Please use a higher number."
        );
        assert_eq!(
            err(&["-testnet", "-prune=944", "-txindex=0", "-disablegovernance"]),
            "Prune configured below the minimum of 945 MiB.  Please use a higher number."
        );
        // Step 2 reads -prune=-1 as pruning first (Core: "incompatible with -txindex").
        assert_eq!(
            err(&["-prune=-1", "-txindex=0", "-disablegovernance"]),
            "Prune cannot be configured with a negative value."
        );
    }

    #[test]
    fn simple_refusals() {
        assert_eq!(err(&["-peertimeout=0"]), "peertimeout must be a positive integer.");
        assert_eq!(err(&["-blockmintxfee=x"]), "Invalid amount for -blockmintxfee=<amount>: 'x'");
        assert_eq!(err(&["-mocktime=-1"]), "-mocktime cannot be negative: -1");
        assert!(err(&["-reindex-chainstate"]).starts_with(
            "-reindex-chainstate option is not compatible with -txindex. Please temporarily disable txindex"
        ));
        assert!(
            err(&["-reindex-chainstate", "-txindex=0", "-blockfilterindex"]).starts_with(
                "-reindex-chainstate option is not compatible with -blockfilterindex."
            )
        );
        assert_eq!(
            err(&["-minimumchainwork=xyz"]),
            "Invalid non-hex (xyz) minimum chain work value specified"
        );
        // Governance is off by default until GV1b (DEC-510), so only `-disablegovernance=0`
        // reaches the transaction-index check.
        assert_eq!(
            err(&["-testnet", "-txindex=0", "-disablegovernance=0"]),
            "Transaction index can't be disabled with governance validation enabled. Either start with -disablegovernance command line switch or enable transaction index."
        );
        assert!(run(&["-testnet", "-txindex=0"]).0.is_ok());
        assert!(run(&["-txindex=0"]).0.is_ok());
    }

    /// `-timeout` is Core's `int`: values that narrow to a non-positive one take the default,
    /// and the rest keep their low 32 bits.
    // Core: init.cpp:1365-1368
    #[test]
    fn connect_timeout_narrows_like_cores_int() {
        let ms = connect_timeout_ms;
        assert_eq!((ms(1), ms(20_000), ms(i64::from(i32::MAX))), (1, 20_000, 2_147_483_647));
        assert_eq!((ms(0), ms(-1), ms(i64::MIN)), (5000, 5000, 5000));
        assert_eq!((ms(1 << 31), ms(1 << 32), ms(u32::MAX.into())), (5000, 5000, 5000));
        assert_eq!(ms((1 << 32) + 1), 1);
    }

    #[test]
    fn timeouts_and_governance_warning() {
        let (flags, stderr) = run(&["-timeout=-3", "-disablegovernance"]);
        let flags = flags.unwrap();
        assert_eq!(flags.connect_timeout_ms, DEFAULT_CONNECT_TIMEOUT);
        // Core's `int` narrows 2^31 to a negative value, and 2^32 + 7 to 7.
        let timeout = |arg: &str| run(&[arg]).0.unwrap().connect_timeout_ms;
        assert_eq!(timeout("-timeout=2147483648"), DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(timeout("-timeout=4294967303"), 7);
        assert_eq!(timeout("-timeout=2147483647"), 2_147_483_647);
        assert!(!flags.governance_enabled);
        assert_eq!(stderr, ["Warning: You are starting with governance validation disabled.\n"]);
        // rdashd's default is Core's `-disablegovernance=1` without its warning (DEC-510).
        let (flags, stderr) = run(&[]);
        assert!(!flags.unwrap().governance_enabled);
        assert!(stderr.is_empty(), "{stderr:?}");
        let (flags, stderr) = run(&["-disablegovernance=0"]);
        assert!(flags.unwrap().governance_enabled);
        assert!(stderr.is_empty(), "{stderr:?}");
        let (_, stderr) = run(&["-prune=550", "-txindex=0"]);
        assert_eq!(
            stderr,
            [
                "Warning: You are starting with governance validation disabled. This is expected because you are running a pruned node.\n"
            ]
        );
    }

    #[test]
    fn qvvec_sync() {
        let flags = run(&["-llmq-qvvec-sync=llmq_test:1"]).0.unwrap();
        assert_eq!(flags.qvvec_sync.len(), 1);
        assert_eq!(flags.qvvec_sync[0].1, QvvecSyncMode::OnlyIfTypeMember);
        // `getline` reads `a:b:` as two fields.
        assert!(run(&["-llmq-qvvec-sync=llmq_test:0:"]).0.is_ok());
        for (value, kind) in [
            ("llmq_test", "Invalid format"),
            ("llmq_test:", "Invalid format"),
            ("llmq_test:0::", "Invalid format"),
            (":0", "Invalid format"),
            ("llmq_nope:0", "Invalid llmqType"),
            ("llmq_test:2", "Invalid mode"),
            ("llmq_test:+-1", "Invalid mode"),
        ] {
            assert_eq!(
                err(&[&format!("-llmq-qvvec-sync={value}")]),
                format!("{kind} in -llmq-qvvec-sync: {value}")
            );
        }
        assert_eq!(
            err(&["-llmq-qvvec-sync=llmq_test:0", "-llmq-qvvec-sync=llmq_test:1"]),
            "Duplicated llmqType in -llmq-qvvec-sync: llmq_test:1"
        );
        let (_, stderr) = run(&["-llmq-qvvec-sync=llmq_test:0", "-llmq-data-recovery=0"]);
        assert_eq!(
            stderr,
            [
                "Warning: -llmq-qvvec-sync set but recovery is disabled due to -llmq-data-recovery=0\n"
            ]
        );
    }

    #[test]
    fn spv_refuses_every_index() {
        assert!(run(&["-spv"]).0.is_ok());
        assert!(run(&["-spv", "-testnet"]).0.is_ok());
        for index in [
            "txindex",
            "addressindex",
            "spentindex",
            "timestampindex",
            "coinstatsindex",
            "blockfilterindex",
        ] {
            assert_eq!(
                err(&["-spv", &format!("-{index}=1")]),
                format!(
                    "-{index} cannot be used with -spv: an SPV node keeps no blocks or undo data."
                )
            );
        }
    }

    const MN: &str = "-masternodeblsprivkey=00";
    /// rdashd is in `-blocksonly` mode by default until P5 (DEC-322), which a masternode refuses.
    const RELAYING: &str = "-blocksonly=0";

    #[test]
    fn masternode_refusals() {
        assert_eq!(
            err(&[MN, "-txindex=0"]),
            "Masternode must have transaction index enabled, set -txindex=1"
        );
        assert_eq!(
            err(&[MN, "-peerbloomfilters=0"]),
            "Masternode must have bloom filters enabled, set -peerbloomfilters=1"
        );
        assert_eq!(
            err(&[MN, "-maxconnections=124"]),
            "Masternode must be able to handle at least 125 connections, set -maxconnections=125"
        );
        assert_eq!(
            err(&[MN, "-disablegovernance"]),
            "You can not disable governance validation on a masternode."
        );
        for blocks_only in [&[MN][..], &[MN, "-blocksonly"]] {
            assert_eq!(err(blocks_only), "Masternode must relay transactions, set -blocksonly=0");
        }
        assert_eq!(
            err(&[MN, RELAYING, "-peerblockfilters=0"]),
            "Masternode must serve compact block filters, set -peerblockfilters=1"
        );
        assert_eq!(
            err(&[MN, RELAYING, "-maxuploadtarget=1"]),
            "Masternode must serve blocks and SPV clients without an upload limit, set -maxuploadtarget=0"
        );
        assert!(run(&[MN, RELAYING, "-maxuploadtarget=0M"]).0.is_ok());
        // Regtest may be stricter; testnet may not.
        assert!(run(&[MN, RELAYING, "-bytespersigop=21", "-maxmempool=100"]).0.is_ok());
        assert_eq!(
            err(&["-testnet", MN, RELAYING, "-bytespersigop=21", "-maxmempool=100"]),
            "Masternode must not use a stricter transaction relay policy than the default, which would make it skip InstantSend signing. Remove or restore: -maxmempool, -bytespersigop"
        );
    }

    /// `-checkblockindex` (BL-525) is a boolean as `InterpretBool` reads it: bare, empty or
    /// nonzero on, `0`, a negation or a non-number off, a double negation on, and the last of
    /// repeated ones wins. Unset, `Flatten` takes the chain's
    /// `fDefaultConsistencyChecks`, on for regtest only. There is no 1-in-N ratio in rc.4
    /// (`init.cpp:2012`'s is `-checkmempool`'s).
    // Core: node/chainstatemanager_args.cpp:22, util/system.cpp:103-108, :166-170,
    // util/settings.cpp:200, validation.cpp:6774, chainparams.cpp:291, :494, :686, :892
    #[test]
    fn checkblockindex_is_a_boolean_else_the_chain_s_default() {
        let resolved = |argv: &[&str]| {
            let args = test_args(argv);
            let chain = select_params(&args, &args.get_chain_name().unwrap()).unwrap().chain;
            let options = chainman_options(&args).unwrap();
            (options.check_block_index, options.check_block_index(&chain))
        };
        let nets = [("-chain=main", false), ("-testnet", false), ("-devnet=nd1", false)];
        for (net, default) in nets.into_iter().chain([("-regtest", true)]) {
            assert_eq!(resolved(&[net]), (None, default), "{net}");
            let forms: [(&[&str], bool); 10] = [
                (&["-checkblockindex"], true),
                (&["-checkblockindex="], true),
                (&["-checkblockindex=1"], true),
                (&["-checkblockindex=2"], true),
                (&["-checkblockindex=-1"], true),
                (&["-checkblockindex=0"], false),
                (&["-nocheckblockindex"], false),
                (&["-nocheckblockindex=0"], true),
                (&["-checkblockindex=foo"], false),
                (&["-checkblockindex=0", "-checkblockindex"], true),
            ];
            for (form, on) in forms {
                let argv: Vec<&str> = [net].into_iter().chain(form.iter().copied()).collect();
                assert_eq!(resolved(&argv), (Some(on), on), "{argv:?}");
            }
        }
    }

    /// `AssumedValidBlock()`: `-assumevalid` as `uint256S` reads it, else the chain's
    /// `defaultAssumeValid` (rc.3's chainparams), and a null one verifies every block.
    // Core: node/chainstatemanager_args.cpp:33, validation.cpp:6770, chainparams.cpp:231, :440
    #[test]
    fn assumevalid_is_the_argument_else_the_chain_s_default() {
        let assumed = |argv: &[&str]| {
            let args = test_args(argv);
            let chain = select_params(&args, &args.get_chain_name().unwrap()).unwrap().chain;
            chainman_options(&args).unwrap().assumed_valid(&chain).map(|h| h.to_string())
        };
        let main = "00000000000000119fe42827219e0686d3f7b494ae65f823194c740c5dbab492";
        let test = "000000541a23f9db7411cddbe50f9f1ebd4aa7108ebdcad62214753f648c0239";
        assert_eq!(assumed(&["-chain=main"]).as_deref(), Some(main));
        assert_eq!(assumed(&["-testnet"]).as_deref(), Some(test));
        assert_eq!(assumed(&["-regtest"]), None);
        assert_eq!(assumed(&["-devnet=nd1"]), None);
        for verify_all in ["0", "", "0x", "x"] {
            assert_eq!(assumed(&["-chain=main", &format!("-assumevalid={verify_all}")]), None);
        }
        assert_eq!(assumed(&["-chain=main", "-noassumevalid"]), None, "a negation reads 0");
        let ours = "0x0000002a00000000000000000000000000000000000000000000000000000000";
        assert_eq!(
            assumed(&["-regtest", &format!("-assumevalid= {ours}zz")]).as_deref(),
            Some(&ours[2..])
        );
    }
}

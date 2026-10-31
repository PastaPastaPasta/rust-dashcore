use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use dash_conformance::check::{Context, parse_rev_hex};
use dash_conformance::report::{Identity, Meta, Reporter, Summary, summarize};
use dash_conformance::rpc::{Auth, Rpc};
use dash_conformance::run::{self, BlkOptions, RpcOptions};
use dashcore::Network;

#[derive(Parser)]
#[command(about = "Differential conformance harness: dashcore vs base-sdk vs dashd")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Net {
    Mainnet,
    Testnet,
    Regtest,
}

impl Net {
    fn network(self) -> Network {
        match self {
            Net::Mainnet => Network::Mainnet,
            Net::Testnet => Network::Testnet,
            Net::Regtest => Network::Regtest,
        }
    }

    /// Genesis block hash from Dash Core's `chainparams.cpp` (independent of both libraries).
    fn genesis(self) -> &'static str {
        match self {
            Net::Mainnet => "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6",
            Net::Testnet => "00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c",
            Net::Regtest => "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Net::Mainnet => "mainnet",
            Net::Testnet => "testnet",
            Net::Regtest => "regtest",
        }
    }
}

/// The networks the O6 scans cover.
#[derive(Clone, Copy, ValueEnum)]
enum ScanNet {
    Mainnet,
    Testnet,
}

impl ScanNet {
    fn net(self) -> Net {
        match self {
            ScanNet::Mainnet => Net::Mainnet,
            ScanNet::Testnet => Net::Testnet,
        }
    }

    fn scan_net(self) -> dash_conformance::scan::Net {
        match self {
            ScanNet::Mainnet => dash_conformance::scan::Net::Mainnet,
            ScanNet::Testnet => dash_conformance::scan::Net::Testnet,
        }
    }
}

#[derive(Args)]
struct Common {
    #[arg(long, value_enum)]
    net: Net,
    /// Output directory (mismatches.ndjson, progress.ndjson, summary.json).
    #[arg(long)]
    out: PathBuf,
    /// First height to check. The work units (RPC chunks, blk files) holding the requested
    /// heights are checked whole.
    #[arg(long, default_value_t = 0)]
    from: u32,
    /// Last height to check (default: the tip).
    #[arg(long)]
    to: Option<u32>,
    /// Skip work units that earlier runs of the same code and comparison policy checked, and
    /// whose blocks have not changed since (reorg, rewritten blk file).
    #[arg(long)]
    resume: bool,
    /// Worker threads (default: the number of cores, at most 8).
    #[arg(long)]
    threads: Option<usize>,
    /// Example mismatches kept per group in summary.json.
    #[arg(long, default_value_t = 20)]
    examples: usize,
    /// Lines written to mismatches.ndjson per mismatch group (0 = unlimited); the rest are
    /// counted in summary.json but not written.
    #[arg(long, default_value_t = 1000)]
    max_lines_per_group: u64,
    /// Exit with status 1 when any unit in the requested range has a failing mismatch,
    /// whether this run or an earlier one (with --resume) checked it (for use as a CI gate).
    #[arg(long)]
    fail_on_mismatch: bool,
}

impl Common {
    fn context(&self, source: &'static str) -> Context {
        Context {
            net: self.net.name().into(),
            source,
        }
    }

    fn reporter(
        &self,
        source: &str,
        chunk: Option<u32>,
        blocks_dir: Option<&std::path::Path>,
        identity: &Identity,
    ) -> Result<Reporter> {
        let blocks_dir = match blocks_dir {
            Some(d) => Some(std::fs::canonicalize(d)?.display().to_string()),
            None => None,
        };
        let meta = Meta {
            net: self.net.name().into(),
            source: source.into(),
            chunk,
            blocks_dir,
        };
        Reporter::open(&self.out, &meta, identity, self.examples, self.max_lines_per_group)
    }

    /// Rejects an inverted range before anything is opened or written.
    fn check_range(&self) -> Result<()> {
        if let Some(to) = self.to
            && self.from > to
        {
            bail!("--from {} is above --to {to}: nothing would be checked", self.from);
        }
        Ok(())
    }
}

#[derive(Args)]
struct RpcArgs {
    /// Oracle JSON-RPC URL. Defaults to the shared BS2 oracle for --net (see ORACLE.md).
    #[arg(long)]
    rpc_url: Option<String>,
    /// Cookie file. Defaults to /work/chains/<net>/rpc.cookie.
    #[arg(long, conflicts_with = "rpc_user")]
    cookie: Option<PathBuf>,
    #[arg(long, requires = "rpc_password")]
    rpc_user: Option<String>,
    #[arg(long)]
    rpc_password: Option<String>,
}

impl RpcArgs {
    fn client(&self, net: Net) -> Result<Rpc> {
        let url = match (&self.rpc_url, net) {
            (Some(u), _) => u.clone(),
            (None, Net::Mainnet) => "http://127.0.0.1:27998".into(),
            (None, Net::Testnet) => "http://127.0.0.1:37998".into(),
            (None, Net::Regtest) => bail!("--rpc-url is required for regtest"),
        };
        let auth = match (&self.rpc_user, &self.rpc_password, &self.cookie) {
            (Some(u), Some(p), _) => Auth::UserPass(u.clone(), p.clone()),
            (_, _, Some(c)) => Auth::Cookie(c.clone()),
            _ => Auth::Cookie(PathBuf::from(format!("/work/chains/{}/rpc.cookie", net.name()))),
        };
        Ok(Rpc::new(url, auth))
    }
}

#[derive(Subcommand)]
enum DigestCmd {
    /// Extract the StateDigest v1 golden file (spec O8 §9) from a frozen copy of a recording
    /// node: merged `C` records plus the sparse `S` records.
    ///
    /// The node must be a copy of the recording node's datadir (reflinked, the original stopped),
    /// started with `-connect=0 -rpcdoccheck=0 -coinstatsindex=1` and without the digest flags.
    Extract {
        /// The frozen copy's JSON-RPC URL, e.g. http://127.0.0.1:18443.
        #[arg(long)]
        rpc_url: String,
        /// Cookie file of the copy (`<datadir>/regtest/.cookie`).
        #[arg(long, conflicts_with = "rpc_user", required_unless_present = "rpc_user")]
        cookie: Option<PathBuf>,
        #[arg(long, requires = "rpc_password")]
        rpc_user: Option<String>,
        #[arg(long)]
        rpc_password: Option<String>,
        /// The recording node's uncompressed `digest.jsonl`.
        #[arg(long)]
        digest: PathBuf,
        /// The golden file to write (replaced atomically; byte-reproducible).
        #[arg(long)]
        out: PathBuf,
        /// Regtest cadence: the "every block" rows of spec §9.3 run at every block.
        #[arg(long)]
        dense: bool,
        /// Extra segment heights for the "plus boundaries" rows, comma separated or repeated.
        /// Mainnet starts from A1 §4.6's list; testnet and regtest from none.
        #[arg(long, value_delimiter = ',')]
        boundary: Vec<i64>,
        /// Seconds to wait for coinstatsindex to reach the tip.
        #[arg(long, default_value_t = 600)]
        wait_secs: u64,
        /// The copy's `blocks` directory (`<datadir>/regtest/blocks`): its `rev*.dat` files
        /// check every `undo` (spec O8 §13.5).
        #[arg(long)]
        blocks_dir: PathBuf,
        /// A digest run (`H.run`) whose node ran with other `-vbparams` than the copy: its
        /// records' BIP9 values are not checked here, but by `check-bip9` (spec O8 §13.5).
        /// Repeatable.
        #[arg(long)]
        other_params_run: Vec<String>,
        /// Write the extraction's summary here as JSON: the counts, the skipped calls and the
        /// heights whose BIP9 values were left unchecked.
        #[arg(long)]
        summary: Option<PathBuf>,
    },
    /// Check the BIP9 values of the given runs' `C` records against a copy started with those
    /// runs' parameters (spec O8 §13.5). Prints `{"tip": <hash>, "checked": [<height>, …]}`.
    CheckBip9 {
        /// The copy's JSON-RPC URL.
        #[arg(long)]
        rpc_url: String,
        /// Cookie file of the copy.
        #[arg(long)]
        cookie: PathBuf,
        /// The recording node's uncompressed `digest.jsonl`.
        #[arg(long)]
        digest: PathBuf,
        /// A digest run the copy has the parameters of. Repeatable.
        #[arg(long, required = true)]
        run: Vec<String>,
    },
    /// Replay a stored node's `blocks.dat` through V, logged by the StateDigest tap around the
    /// null connector (node program task O9a-2), and write the tap's digest. The digest carries
    /// the header keys (spec O8 §5.1) only: compare it with the node's golden in linear mode.
    /// Refused (exit 2, no output): a stored file `extract.json` did not hash, a `blocks.dat`
    /// without genesis first and only there, and a replay that does not end on the summary's tip
    /// height and the golden's last block.
    /// These checks trust the node's `extract.json`: on the git-tracked vectors they bind the
    /// replay to the corpus; on any other directory they prove only that its files agree with
    /// its own `extract.json`.
    Replay {
        /// The stored node: `test-vectors/statedigest/<set>/<test>/node<i>`.
        node: PathBuf,
        /// The digest to write (a new file).
        #[arg(long)]
        out: PathBuf,
    },
    /// L2 (CH3a a7-2): a window of a Core `blocks` directory's best chain, or a stored node's
    /// `blocks.dat`, through V and the `CoreConnector` leg, logged as a StateDigest, with the
    /// assumevalid preload, the reverse leg and a run manifest. Exit status: 0 done, 2 error (no
    /// digest and no manifest left).
    L2(L2Args),
    /// D101 (CH3a a7-3): a run's undo records against the ones Core wrote. The run's are
    /// heights 1..=N in order, each in the rev file of its block's blk file; Core's are found by
    /// checksum, in any file and order. Prints the report as JSON, naming heights, files and
    /// offsets, never bytes. Exit status: 0 every record equal, 1 diverged (a record differs or
    /// Core wrote none, the run wrote a count other than N, or left a file below its last
    /// unfinalized), 2 error.
    RevCompare {
        /// The chain of both directories.
        #[arg(long, value_enum)]
        chain: DigestChain,
        /// A Core `blocks` directory (read only): its rev files.
        #[arg(long)]
        core: PathBuf,
        /// The run's blocks directory, `digest l2 --work`'s `blocks`: its rev files, and its best
        /// chain, each height's parent and blk file.
        #[arg(long)]
        ours: PathBuf,
        /// The run's end height N, at least 1.
        #[arg(long)]
        to: u32,
    },
    /// Compare two StateDigest files (spec O8 §10): digests, goldens or a mix.
    ///
    /// Exit status: 0 clean (every key of the mode present and equal in every pair), 1 diverged
    /// (a key differs, or a --require key is absent), 3 incomplete (nothing differs, but
    /// nothing was compared, the range stops short of --from/--to, or a key was absent from
    /// some pair), 4 clean except the --exclude keys, which the first line names, 2 error. On a
    /// divergence the first mismatch is dumped (O8 §10.4): to $DASH_EMBARGO_DIR/statedigest by
    /// default, and for mainnet and testnet nowhere else; stdout then names keys, not values.
    Compare(CompareArgs),
}

/// `H.chain`'s names.
#[derive(Clone, Copy, ValueEnum)]
enum DigestChain {
    Main,
    Test,
    Regtest,
}

#[derive(Clone, Copy, ValueEnum)]
enum OffOn {
    Off,
    On,
}

#[derive(Args)]
struct L2Args {
    /// The chain of `--blocks-dir`.
    #[arg(long, value_enum, required_unless_present = "node", conflicts_with = "node")]
    chain: Option<DigestChain>,
    /// A Core `blocks` directory (read only): its best chain is the input.
    #[arg(long, requires_all = ["chain", "to"], conflicts_with = "node")]
    blocks_dir: Option<PathBuf>,
    /// The window's end height N: blocks 0..=N go through V.
    #[arg(long, requires = "blocks_dir")]
    to: Option<u32>,
    /// The node form: a stored node (`test-vectors/statedigest/<set>/<test>/node<i>`), its
    /// `blocks.dat` replayed with its recorded decisions.
    #[arg(long, required_unless_present = "blocks_dir")]
    node: Option<PathBuf>,
    /// The node form: answer the recorded queries by key rather than in order.
    #[arg(long, requires = "node")]
    keyed: bool,
    /// The golden the reverse leg, its end check, a pruned start and a saved snapshot compare
    /// with.
    #[arg(long)]
    golden: Option<PathBuf>,
    /// The forward digest to write (a new file); the reverse leg's goes beside it as
    /// `<stem>.reverse.jsonl`.
    #[arg(long)]
    out: PathBuf,
    /// `on` preloads the best chain's headers to the directory's tip, or `--headers`' chain, and
    /// sets assumevalid (CH3a D153); `off` is `-assumevalid=0`.
    #[arg(long, value_enum, default_value = "off")]
    assumevalid: OffOn,
    /// `-par`: script-check threads, the main one included, as dashd reads it: 0 is one fewer
    /// than the online processors, -n leaves n free, and at most 15 more than the main one.
    #[arg(long, default_value_t = 1, allow_negative_numbers = true)]
    par: i64,
    /// The coins cache's share, in MiB.
    #[arg(long, default_value_t = 256)]
    coins_cache: u64,
    /// Disconnect back to height L after the forward run, each rewind checked against the golden,
    /// then scan the coins against golden(L).
    #[arg(long, requires = "golden")]
    reverse_to: Option<u32>,
    /// The run manifest to write (a new file). Default: `<out>.manifest.json`.
    #[arg(long)]
    manifest: Option<PathBuf>,
    /// Hash the input files into the manifest.
    #[arg(long)]
    sha256: bool,
    /// A directory to create for V's block and undo files, left in place. Default: a temporary
    /// directory under `TMPDIR`, removed.
    #[arg(long)]
    work: Option<PathBuf>,
    /// With `--assumevalid on`: preload this file's headers (80-byte headers from genesis, in
    /// height order) rather than the blocks directory's, as Core's headers-first chain runs past
    /// the blocks it stores (CH3a D162). Its chain must hold the window's blocks.
    #[arg(long, requires = "blocks_dir")]
    headers: Option<PathBuf>,
    /// Write V's coins at the window's end to this UTXO snapshot (a new file, in Core's
    /// `dumptxoutset` format) and its sidecar `<file>.json`, once a scan of them gives golden(N)'s
    /// MuHash, `txouts` and `amount` (CH3a D165).
    #[arg(long, requires = "golden")]
    save_utxo: Option<PathBuf>,
    /// Start V as a pruned node at this snapshot's base B (the suffix check, CH3 R11; CH3a D165):
    /// blocks B+1..=N go through V. The snapshot must match its sidecar, golden(B) and the
    /// blocks directory's chain, and a scan of its coins must give golden(B)'s MuHash, `txouts`
    /// and `amount`. The log's MuHash is finalized at N and at `--reverse-to` only.
    #[arg(long, requires_all = ["golden", "blocks_dir"])]
    start_utxo: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum CompareMode {
    /// Both inputs projected onto their final active chains; semantic keys compared.
    Linear,
    /// Record for record over the selected origins; semantic and observation keys compared.
    /// Heights both inputs skip go undetected: claim coverage from linear mode.
    Stream,
}

#[derive(Args)]
struct CompareArgs {
    /// The left input (`.jsonl` or `.jsonl.zst`).
    left: PathBuf,
    /// The right input.
    right: PathBuf,
    #[arg(long, value_enum, default_value = "linear")]
    mode: CompareMode,
    /// Stream mode: the origins compared (tip, proposal, verifydb, replay). Default: all.
    /// Linear mode has no origin choice: the projection decides.
    #[arg(long, value_delimiter = ',')]
    org: Vec<String>,
    /// Skip records below this height.
    #[arg(long)]
    from: Option<i64>,
    /// Skip records above this height.
    #[arg(long)]
    to: Option<i64>,
    /// Keys whose absence is a divergence (exit 1) rather than an incomplete comparison
    /// (`prev`, `C.s.prev`, `fx`), comma separated. Each must name a key the mode compares
    /// (linear mode compares no observation keys).
    #[arg(long, value_delimiter = ',')]
    require: Vec<String>,
    /// Keys left out of the comparison and of coverage, comma separated (CH3a D167). Each must
    /// name a key the mode compares, no --require key, and a key some compared record holds on
    /// either side. The report lists each with the pairs holding it, and a run clean on the
    /// other keys exits 4, never 0.
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
    /// Go on after a key mismatch or a linear gap.
    #[arg(long)]
    keep_going: bool,
    /// Where dumps go (regtest and devnet; for mainnet and testnet only inside
    /// $DASH_EMBARGO_DIR).
    #[arg(long)]
    dump_dir: Option<PathBuf>,
    /// Write no dump. Mainnet and testnet still need $DASH_EMBARGO_DIR.
    #[arg(long, conflicts_with = "dump_dir")]
    no_dump: bool,
    /// The dump's directory label (default: the inputs' parent directory names).
    #[arg(long)]
    label: Option<String>,
    /// Lines of context the dump keeps around the mismatching record.
    #[arg(long, default_value_t = 3)]
    context: usize,
    /// The left digest's decisions file, for the dump's `A` record.
    #[arg(long)]
    left_decisions: Option<PathBuf>,
    /// The right digest's decisions file.
    #[arg(long)]
    right_decisions: Option<PathBuf>,
    /// A frozen copy's JSON-RPC URL: the dump adds its answers at the mismatching height.
    /// Never a shared oracle (the dump calls `quorum *`).
    #[arg(long)]
    rpc_url: Option<String>,
    /// Cookie file of the frozen copy.
    #[arg(long, requires = "rpc_url")]
    cookie: Option<PathBuf>,
}

/// Segment replays (node program O9b): snapshot bundles and per-PR window selection.
#[derive(Subcommand)]
enum SegmentCmd {
    /// Bundle a `digest l2 --save-utxo` snapshot (with its sidecar beside it) into a new
    /// directory, once a fresh scan of its coins equals golden(B) and the sidecar.
    Bundle {
        /// The snapshot; its sidecar is `<file>.json`.
        #[arg(long)]
        utxo: PathBuf,
        /// The evo section: Core's `EvoSnapshot` v3 bytes at the same base (phase B).
        #[arg(long)]
        evo: Option<PathBuf>,
        /// The golden holding the base's `C`.
        #[arg(long)]
        golden: PathBuf,
        /// The transition the bundle's window crosses (the registry's `transition`).
        #[arg(long)]
        transition: String,
        /// The bundle directory to create.
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a bundle's checksums and manifest against its sidecar (no scan, no golden), and
    /// print `BUNDLE <chain> <base>`.
    Open {
        /// The bundle directory.
        bundle: PathBuf,
    },
    /// Check a bundle: its checksums and manifest, then a fresh scan of its coins against
    /// golden(B).
    Validate {
        /// The bundle directory.
        bundle: PathBuf,
        /// The golden holding the base's `C`.
        #[arg(long)]
        golden: PathBuf,
    },
    /// The windows a change runs: `RUN <id> <chain> <base> <to>` per window, then `NIGHTLY` and
    /// `DEFERRED` lines and a closing `BUDGET` line, from the changed paths (one per line, `-`
    /// for stdin; none is an error).
    Select {
        /// The registry.
        #[arg(long)]
        registry: PathBuf,
        /// The changed paths, repo-relative, one per line (`git diff --name-only`); `-` is stdin.
        #[arg(long, required_unless_present = "nightly")]
        paths: Option<PathBuf>,
        /// Every active window, with no budget.
        #[arg(long, conflicts_with = "paths")]
        nightly: bool,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// StateDigest v1 tools (node program O8).
    Digest {
        #[command(subcommand)]
        cmd: DigestCmd,
    },
    /// Segment replays (node program O9b).
    Segment {
        #[command(subcommand)]
        cmd: SegmentCmd,
    },
    /// The fingerprint lab's live leg (O13a): diffs two handshake transcripts that
    /// `contrib/node/fingerprint/capture.py live` wrote for dashd and rdashd. Exit 1 when they
    /// differ.
    Fingerprint {
        /// dashd's transcript.
        dashd: PathBuf,
        /// The node's transcript.
        node: PathBuf,
    },
    /// L3 (CH3a a10): the non-evo mutation harness against a real regtest dashd v24, with V and
    /// the shim beside it. `DASHVERSION`, the dashd (`L3_DASHD` or the cached one) and
    /// `L3_DASH_TREE` come from the environment (plan §3.1). Exit 1 when a leg is not done.
    #[cfg(feature = "l3")]
    L3(L3Args),
    /// Stream blocks from the oracle with `getblock <hash> 0|1` (and `2` for samples).
    Rpc {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        rpc: RpcArgs,
        /// Heights per resumable work unit.
        #[arg(long, default_value_t = 1000)]
        chunk: u32,
        /// Blocks per JSON-RPC batch.
        #[arg(long, default_value_t = 50)]
        batch: usize,
        /// Cross-check every Nth block against `getblock <hash> 2` (0 = never).
        #[arg(long, default_value_t = 1000)]
        verbose_every: u32,
        /// Also cross-check every block containing a special tx of these types (QcTx, type 6,
        /// is left out by default: commitments are in a large share of blocks).
        #[arg(long, value_delimiter = ',', default_value = "1,2,3,4,7,8,9,10,11,12")]
        verbose_types: Vec<u16>,
    },
    /// Read blocks straight from a dashd `blocks/` directory (blk*.dat, honoring xor.dat).
    Blk {
        #[command(flatten)]
        common: Common,
        /// The dashd `blocks/` directory.
        #[arg(long)]
        blocks_dir: PathBuf,
        /// Also check blocks that are not on the best chain.
        #[arg(long)]
        include_stale: bool,
        /// Don't use the oracle. By default (mainnet/testnet: the shared oracles; regtest:
        /// with --rpc-url) the best chain is anchored to the oracle's best block, whose hash
        /// is checked, and records not linked to genesis are classified by its block index.
        /// Without it every unlinked record is a failing `blk.orphan`.
        #[arg(long)]
        no_anchor: bool,
        #[command(flatten)]
        rpc: RpcArgs,
    },
    /// O6: scan a dashd `blocks/` directory for the shapes of R4 §2.3.5 (a)-(f). Opens the
    /// blk files read-only; the oracle gives the chain anchor and scan (c)'s block rewards.
    /// Exit status: 0 complete, 2 incomplete (an unframed block, a file changed under the
    /// scan, a region of a blk file that holds no block record, a block whose merkle root
    /// differs from its header's, scan (c) without an oracle) or an error.
    Scan {
        #[arg(long, value_enum)]
        net: ScanNet,
        /// The dashd `blocks/` directory.
        #[arg(long)]
        blocks_dir: PathBuf,
        /// Output directory (scan-<letter>.ndjson, summary.json).
        #[arg(long)]
        out: PathBuf,
        /// Which scans to run, e.g. `abcdef` or `a,b,e`.
        #[arg(long, default_value = "abcdef")]
        scans: String,
        #[arg(long, default_value_t = 0)]
        from: u32,
        #[arg(long)]
        to: Option<u32>,
        /// Worker threads (default: the number of cores, at most 8).
        #[arg(long)]
        threads: Option<usize>,
        /// Don't use the oracle: no anchor, and scan (c) lists its candidates unclassified.
        #[arg(long)]
        no_oracle: bool,
        #[command(flatten)]
        rpc: RpcArgs,
    },
    /// Rebuild summary.json from an output directory (for the latest run's identity).
    Summarize {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 20)]
        examples: usize,
    },
}

fn init_threads(threads: Option<usize>) -> Result<()> {
    let n = threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()).min(8));
    rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global()?;
    Ok(())
}

fn print_groups(summary: &Summary) {
    for (k, g) in &summary.mismatches_by_key {
        eprintln!("{:>9}  {k}  (heights {:?}..{:?})", g.count, g.first_height, g.last_height);
    }
}

/// `fingerprint DASHD NODE`: prints each difference; 0 when there is none, 1 when there are, 2
/// when a transcript cannot be read.
fn run_fingerprint(dashd: &std::path::Path, node: &std::path::Path) -> ExitCode {
    use dash_conformance::fingerprint::{Transcript, diff};
    let read = |p: &std::path::Path| -> Result<Transcript> {
        let text = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
        Transcript::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))
    };
    let (d, n) = match (read(dashd), read(node)) {
        (Ok(d), Ok(n)) => (d, n),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e:#}");
            return ExitCode::from(2);
        }
    };
    let found = diff(&d, &n);
    for f in &found {
        println!("{f}");
    }
    println!("{} difference(s)", found.len());
    if found.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Exit status: 0 = the requested range is fully covered and (with --fail-on-mismatch) has
/// no failing mismatch; 1 = it has failing mismatches; 2 = it is not fully covered (failed
/// units, a reorg during the run, RPC results that are not one chain ending in the run's
/// pinned block, lost mismatch records, or blk findings that mean blocks may have been
/// skipped) or the harness hit an error.
/// `l3`'s options (track L plan rev1 §2.1; a10-4 has tier 1 only).
#[cfg(feature = "l3")]
#[derive(Args)]
struct L3Args {
    /// The case set.
    #[arg(long, default_value = "tier1")]
    set: String,
    /// The main legs, by their `-par`.
    #[arg(long, value_delimiter = ',', default_value = "par1,par4")]
    legs: Vec<String>,
    /// One operator or case id (a filtered run: the whole setup chain, these cases only).
    #[arg(long)]
    only: Option<String>,
    /// Where the report goes.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Keep the legs' datadirs here (a new directory under `TMPDIR` otherwise, removed at the
    /// end unless the run failed).
    #[arg(long)]
    retain: Option<PathBuf>,
    /// The rust-dashcore tree whose workflows pin `DASHVERSION`.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Record each leg's planes after every trial op as `<leg>.json` in this directory.
    #[arg(long)]
    record: Option<PathBuf>,
}

#[cfg(feature = "l3")]
fn run_l3(args: &L3Args) -> Result<ExitCode> {
    use node_l1::chain::mutate::Par;

    ensure!(args.set == "tier1", "--set {}: a10-4 has tier1 only", args.set);
    let legs = args
        .legs
        .iter()
        .map(|l| match l.as_str() {
            "par1" => Ok(Par::One),
            "par4" => Ok(Par::Four),
            other => bail!("--legs: no leg {other} (par1, par4)"),
        })
        .collect::<Result<Vec<_>>>()?;
    let config = node_l1::l3::Config {
        set: args.set.clone(),
        report: args.report.clone(),
        only: args.only.clone(),
        legs,
        record: args.record.clone(),
    };
    let scratch = match &args.retain {
        Some(dir) => {
            std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
            tempfile::Builder::new().prefix("l3-").tempdir_in(dir)?
        }
        None => tempfile::Builder::new().prefix("l3-").tempdir()?,
    };
    let (report, done) =
        node_l1::l3::run(&config, &args.repo, scratch.path()).map_err(anyhow::Error::msg)?;
    for leg in report["legs"].as_array().into_iter().flatten() {
        eprintln!("l3: {} {} {}", leg["name"], leg["status"], leg["error"]);
    }
    if !done || args.retain.is_some() {
        eprintln!("l3: datadirs kept in {}", scratch.keep().display());
    }
    Ok(if done {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let (common, result, reporter, client) = match cli.cmd {
        Cmd::Digest {
            cmd,
        } => return run_digest(cmd),
        Cmd::Segment {
            cmd,
        } => return run_segment(cmd),
        Cmd::Fingerprint {
            dashd,
            node,
        } => return Ok(run_fingerprint(&dashd, &node)),
        #[cfg(feature = "l3")]
        Cmd::L3(args) => return run_l3(&args),
        Cmd::Summarize {
            out,
            examples,
        } => {
            let s = summarize(&out, examples, None, None)?;
            std::fs::write(out.join("summary.json"), serde_json::to_vec_pretty(&s)?)?;
            println!("{}", serde_json::to_string_pretty(&s.totals)?);
            for (k, g) in &s.mismatches_by_key {
                println!("{:>9}  {k}", g.count);
            }
            println!(
                "{} units, {} failing mismatches ({} incomplete-coverage), unverified: {} units, \
                 {} blocks, {} mismatches",
                s.units,
                s.failing,
                s.incomplete,
                s.unverified.units,
                s.unverified.blocks,
                s.unverified.mismatches
            );
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Scan {
            net,
            blocks_dir,
            out,
            scans,
            from,
            to,
            threads,
            no_oracle,
            rpc,
        } => {
            let scans =
                dash_conformance::scan::ScanSet::parse(&scans).map_err(|e| anyhow::anyhow!(e))?;
            if let Some(to) = to {
                ensure!(from <= to, "--from {from} is above --to {to}");
            }
            init_threads(threads)?;
            let oracle = (!no_oracle).then(|| rpc.client(net.net())).transpose()?;
            let anchor = oracle.as_ref().map(run::oracle_best_hash).transpose()?;
            let identity = Identity::of_build("scan");
            let opts = dash_conformance::scan::ScanOptions {
                net: net.scan_net(),
                blocks_dir,
                scans,
                magic: net.net().network().magic().to_le_bytes(),
                genesis: parse_rev_hex(net.net().genesis()).expect("valid genesis hex"),
                from,
                to,
                anchor,
                oracle: oracle.as_ref(),
                out,
                code_id: identity.code,
            };
            let result = dash_conformance::scan::run_scan(&opts)?;
            for line in &result.incomplete {
                eprintln!("incomplete: {line}");
            }
            println!("{}", serde_json::to_string_pretty(&result.summary["hit_counts"])?);
            return Ok(if result.incomplete.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            });
        }
        Cmd::Rpc {
            common,
            rpc,
            chunk,
            batch,
            verbose_every,
            verbose_types,
        } => {
            common.check_range()?;
            init_threads(common.threads)?;
            let client = rpc.client(common.net)?;
            let ctx = common.context("rpc");
            let chunk = chunk.max(1);
            let opts = RpcOptions {
                from: common.from,
                to: common.to,
                chunk,
                batch: batch.max(1),
                verbose_every,
                verbose_types: verbose_types.into_iter().collect::<BTreeSet<_>>(),
                resume: common.resume,
            };
            // Checked before the output directory is touched.
            run::rpc_range(&client, opts.from, opts.to)?;
            let identity = Identity::of_build(opts.policy())
                .with_oracle(run::oracle_version(&client).context("oracle version")?);
            let reporter = common.reporter("rpc", Some(chunk), None, &identity)?;
            let result = run::run_rpc(&client, &ctx, &opts, &reporter)?;
            (common, result, reporter, Some(client))
        }
        Cmd::Blk {
            common,
            blocks_dir,
            include_stale,
            no_anchor,
            rpc,
        } => {
            common.check_range()?;
            init_threads(common.threads)?;
            // The oracle anchors the chain and classifies unlinked records; --no-anchor runs
            // without it.
            let has_oracle = rpc.rpc_url.is_some() || !matches!(common.net, Net::Regtest);
            let oracle = (has_oracle && !no_anchor).then(|| rpc.client(common.net)).transpose()?;
            let anchor = oracle.as_ref().map(run::oracle_best_hash).transpose()?;
            let ctx = common.context("blk");
            let opts = BlkOptions {
                blocks_dir,
                magic: common.net.network().magic().to_le_bytes(),
                genesis: Some(parse_rev_hex(common.net.genesis()).expect("valid genesis hex")),
                from: common.from,
                to: common.to,
                include_stale,
                resume: common.resume,
                anchor,
            };
            let identity = Identity::of_build(opts.policy());
            let reporter = common.reporter("blk", None, Some(&opts.blocks_dir), &identity)?;
            let index = oracle.as_ref().map(|c| c as &dyn run::BlockIndex);
            let result = run::run_blk(&ctx, &opts, index, &reporter)?;
            (common, result, reporter, None)
        }
    };
    let summary = reporter.write_summary().context("writing summary")?;
    eprintln!("this run: {}", serde_json::to_string(&result.stats)?);
    eprintln!(
        "all runs in {}: {} units, {} blocks, {} mismatches ({} informational) in {} groups",
        common.out.display(),
        summary.units,
        summary.totals.blocks,
        summary.totals.mismatches,
        summary.totals.informational,
        summary.mismatches_by_key.len()
    );
    print_groups(&summary);
    if summary.unverified.units > 0 {
        eprintln!(
            "not re-checked under this identity yet: {} units, {} blocks, {} mismatches (from \
             another code or policy identity, or invalidated by a reorg or a changed blk file)",
            summary.unverified.units, summary.unverified.blocks, summary.unverified.mismatches
        );
    }

    // The gate covers every unit of the requested range, whichever run checked it.
    let gate = reporter.gate(&result.scope).context("evaluating the gate")?;
    // RPC mode: the counted results must be one chain ending in the block pinned at the start.
    let mut chain = String::new();
    let mut not_one_chain = Vec::new();
    if let (Some(pin), Some(rpc)) = (&result.snapshot, &client) {
        not_one_chain = pin.problems(rpc, &result.scope, &gate.counted)?;
        let one = if not_one_chain.is_empty() {
            "one chain"
        } else {
            "NOT one chain"
        };
        chain = format!(", {one} ending in block {} {}", pin.height, pin.hash);
    }
    eprintln!(
        "gate: {} units in scope, {} not covered, {} failing mismatches ({} incomplete-coverage), \
         {} informational{chain}",
        result.scope.len(),
        gate.missing.len(),
        gate.failing,
        gate.incomplete,
        gate.totals.informational
    );
    for m in &gate.missing {
        eprintln!("  not covered: {m}");
    }
    for p in &not_one_chain {
        eprintln!("  not one chain: {p}");
    }
    if !not_one_chain.is_empty() {
        eprintln!("  the oracle reorganised during the run; re-run with --resume");
    }
    let covered = result.failed_units == 0
        && gate.missing.is_empty()
        && gate.incomplete == 0
        && not_one_chain.is_empty();
    Ok(if !covered {
        ExitCode::from(2)
    } else if common.fail_on_mismatch && gate.failing > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn run_compare(args: CompareArgs) -> Result<ExitCode> {
    use dash_conformance::digest::compare::{
        Input, chain_of, compare, exit_code, render, shows_values,
    };
    use dash_conformance::digest::dump::{self, Extras, RpcTarget};
    use dash_statedigest::Origin;
    use dash_statedigest::compare::{FieldId, Mode, Options, Verdict};

    let mut inputs = [Input::open(&args.left)?, Input::open(&args.right)?];
    let chain = chain_of(&inputs)?;
    // Refuse before comparing, so that nothing is shown: mainnet and testnet need the private
    // corpus with or without a dump, and a dump's directory is decided here.
    let embargo = std::env::var_os("DASH_EMBARGO_DIR").map(PathBuf::from);
    dump::check_private(chain, embargo.as_deref())?;
    let root = if args.no_dump {
        None
    } else {
        Some(dump::root(chain, args.dump_dir.as_deref(), embargo.as_deref())?)
    };
    let rpc = match (args.rpc_url, args.cookie) {
        (Some(url), Some(cookie)) => Some(RpcTarget::new(url, Auth::Cookie(cookie))?),
        (Some(_), None) => bail!("--rpc-url needs --cookie"),
        _ => None,
    };
    let mode = match args.mode {
        CompareMode::Linear => Mode::Linear,
        CompareMode::Stream => Mode::Stream,
    };
    let mut opts = Options::new(mode);
    if mode == Mode::Linear && !args.org.is_empty() {
        bail!("--org applies to stream mode only; linear mode compares the final chains");
    }
    if !args.org.is_empty() {
        let origins = args
            .org
            .iter()
            .map(|o| {
                Origin::from_bytes(o.as_bytes()).with_context(|| format!("unknown origin {o}"))
            })
            .collect::<Result<Vec<_>>>()?;
        opts.origins = Some(origins);
    }
    if let (Some(from), Some(to)) = (args.from, args.to)
        && from > to
    {
        bail!("--from {from} is above --to {to}");
    }
    opts.from = args.from;
    opts.to = args.to;
    opts.keep_going = args.keep_going;
    let keys = |flag: &str, names: &[String]| -> Result<Vec<FieldId>> {
        let mut out = Vec::new();
        for name in names {
            let ids = FieldId::resolve(name);
            if ids.is_empty() {
                bail!("--{flag} {name}: not a compared key");
            }
            let ids: Vec<FieldId> =
                ids.into_iter().filter(|id| mode.classes().contains(&id.spec().class)).collect();
            if ids.is_empty() {
                bail!("--{flag} {name}: not compared in {} mode", dump::mode_name(mode));
            }
            out.extend(ids);
        }
        Ok(out)
    };
    opts.require = keys("require", &args.require)?;
    opts.exclude = keys("exclude", &args.exclude)?;
    if let Some(f) = opts.exclude.iter().find(|f| opts.require.contains(f)) {
        bail!("--exclude {f}: also a --require key");
    }

    let report = compare(&mut inputs, opts)?;
    // An exclusion no record needs could later hide a key that appears (CH3a D167). A divergence
    // is reported as one: its comparison may have stopped before a record holding the key.
    let unheld: Vec<String> =
        report.excluded.iter().filter(|(_, n)| **n == [0, 0]).map(|(f, _)| f.to_string()).collect();
    if !unheld.is_empty() && report.verdict != Verdict::Diverged {
        bail!("--exclude {}: no compared record holds it, on either side", unheld.join(", "));
    }
    print!("{}", render(&report, &inputs, shows_values(chain)));
    if report.verdict == Verdict::Diverged
        && let Some(root) = root
    {
        let label = args.label.unwrap_or_else(|| {
            let name = |p: &PathBuf| {
                p.parent()
                    .and_then(|d| d.file_name())
                    .map_or("input".into(), |n| n.to_string_lossy().into_owned())
            };
            format!("{}-vs-{}", name(&args.left), name(&args.right))
        });
        let extras = Extras {
            context: args.context,
            decisions: [args.left_decisions, args.right_decisions],
            rpc,
        };
        // A failed dump does not hide the divergence: the exit stays 1.
        match dump::write(&root, &label, chain, &inputs, &report, &extras) {
            Ok(dir) => println!("dump: {}", dir.display()),
            Err(e) => eprintln!("warning: the dump failed: {e:#}"),
        }
    }
    Ok(ExitCode::from(exit_code(&report)))
}

/// A stored node directory as [`dash_conformance::digest::store::Node`].
fn stored_node(node: &std::path::Path) -> Result<dash_conformance::digest::store::Node> {
    // Absolute, so that `.` and `node0` name their test too.
    let node =
        std::fs::canonicalize(node).with_context(|| format!("reading {}", node.display()))?;
    let file = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let (Some(test), Some(name)) = (node.parent().and_then(file), file(&node)) else {
        bail!("{} is not a stored node directory", node.display());
    };
    Ok(dash_conformance::digest::store::Node {
        test,
        name,
        dir: node,
    })
}

/// `chain`'s v24 parameters, and its `H.chain`.
fn chain_params(chain: DigestChain) -> Result<(dash_params::ChainParams, dash_statedigest::Chain)> {
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};
    use dash_statedigest::Chain;

    let (kind, chain) = match chain {
        DigestChain::Main => (NetworkKind::Mainnet, Chain::Main),
        DigestChain::Test => (NetworkKind::Testnet, Chain::Test),
        DigestChain::Regtest => (NetworkKind::Regtest, Chain::Regtest),
    };
    let params = ChainParamsBuilder::new(kind, Release::V24_0)
        .build()
        .map_err(|e| anyhow::anyhow!("the {} params: {e:?}", chain.as_str()))?;
    Ok((params, chain))
}

/// `digest rev-compare` (CH3a D101): each height's parent and blk file from `ours`'s best chain,
/// then the records.
fn run_rev_compare(chain: DigestChain, core: &Path, ours: &Path, to: u32) -> Result<ExitCode> {
    use dash_conformance::digest::blkdir::BlockDir;
    use dash_conformance::digest::revfiles;
    use dashd_chainstate::chain::genesis_block;

    ensure!(to >= 1, "--to {to}: the run's undo records are heights 1..=N, so N is at least 1");
    let (params, _) = chain_params(chain)?;
    let genesis = genesis_block(&params).hash();
    let dir = BlockDir::open(ours, params.message_start, genesis, None)?;
    ensure!(dir.tip() == to, "{}'s best chain ends at {}, not {to}", ours.display(), dir.tip());
    let blocks = revfiles::run_blocks(&dir, to)?;
    let r = revfiles::compare(ours, core, params.message_start, &blocks)?;
    let first =
        r.first.as_ref().map(|(height, what)| serde_json::json!({"height": height, "what": what}));
    println!(
        "{}",
        serde_json::json!({
            "compared": r.compared,
            "equal": r.compared - r.mismatched,
            "mismatched": r.mismatched,
            "same_position": r.same_position,
            "same_position_to": r.same_position_to,
            "first": first,
            "run_issues": r.run_issues,
            "core": {
                "records": r.core_records,
                "repeated": r.core_duplicates,
                "ended_early": r.core_ragged,
            },
        })
    );
    Ok(if r.diverged() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn run_l2(args: L2Args) -> Result<ExitCode> {
    use dash_conformance::digest::blkdir::BlockDir;
    use dash_conformance::digest::l2::{self, Options, Source};
    use dash_statedigest::decisions::Policy;
    use dashd_chainstate::chain::genesis_block;

    let opts = Options {
        assume_valid: matches!(args.assumevalid, OffOn::On),
        par: args.par,
        coins_cache: args.coins_cache.checked_mul(1 << 20).context("--coins-cache overflows")?,
        reverse_to: args.reverse_to,
        golden: args.golden,
        sha256: args.sha256,
        headers: args.headers,
        save_utxo: args.save_utxo,
        start_utxo: args.start_utxo,
    };
    let manifest = args.manifest.unwrap_or_else(|| {
        let mut name = args.out.file_name().unwrap_or_default().to_owned();
        name.push(".manifest.json");
        args.out.with_file_name(name)
    });
    let (temp, work) = match args.work {
        Some(work) => {
            ensure!(!work.exists(), "--work {} exists", work.display());
            (None, work)
        }
        None => {
            let temp = tempfile::Builder::new()
                .prefix("dash-conformance-l2-")
                .tempdir()
                .context("creating the work directory")?;
            let work = temp.path().join("v");
            (Some(temp), work)
        }
    };
    let node;
    let dir;
    let source = match (args.node, args.blocks_dir, args.chain, args.to) {
        (Some(path), ..) => {
            node = stored_node(&path)?;
            let policy = if args.keyed {
                Policy::Keyed
            } else {
                Policy::Strict
            };
            Source::Node {
                node: &node,
                policy,
            }
        }
        (None, Some(blocks), Some(chain), Some(to)) => {
            let (params, chain) = chain_params(chain)?;
            let genesis = genesis_block(&params).hash();
            dir = BlockDir::open(&blocks, params.message_start, genesis, None)?;
            Source::Dir {
                dir: &dir,
                params: std::sync::Arc::new(params),
                chain,
                to,
            }
        }
        _ => bail!("give --node, or --chain, --blocks-dir and --to"),
    };
    let run = l2::run(source, &opts, &args.out, &manifest, &work);
    let closed = temp.map(tempfile::TempDir::close);
    let run = run?;
    closed.transpose().context("removing the work directory")?;
    let f = &run.forward;
    print!("l2: ");
    if let Some(s) = &run.start {
        print!("start check equal ({} coins); ", s.txouts);
    }
    print!(
        "tip {} {}; C {} (skipped {}, checked {}, none {}), R {}",
        run.tip.0, run.tip.1, f.connects, f.skipped, f.checked, f.undecided, f.rejects
    );
    if let Some(r) = &run.reverse {
        print!(
            "; reverse to {}: D {} ok {}, compared {}, end check equal",
            r.to, r.tally.disconnects, r.tally.disconnects_ok, r.compared
        );
    }
    if let Some(saved) = run.manifest.get("save_utxo").filter(|s| !s.is_null()) {
        let path = saved["path"].as_str().unwrap_or_default();
        print!("; saved {} coins to {path}", saved["coins"]);
    }
    println!("; manifest {}", manifest.display());
    Ok(ExitCode::SUCCESS)
}

fn run_segment(cmd: SegmentCmd) -> Result<ExitCode> {
    use dash_conformance::segment::{bundle, select};

    match cmd {
        SegmentCmd::Bundle {
            utxo,
            evo,
            golden,
            transition,
            out,
        } => {
            let made = bundle::make(&utxo, evo.as_deref(), &golden, &transition, &out)?;
            println!("bundled {} at {}: {:?}", made.dir.display(), made.scan.height, made.scan);
            if let Some(evo) = &made.evo {
                println!("evo section: {evo:?}");
            }
        }
        SegmentCmd::Open {
            bundle: dir,
        } => {
            let b = bundle::open(&dir)?;
            println!("BUNDLE {} {}", b.chain, b.scan.height);
        }
        SegmentCmd::Validate {
            bundle: dir,
            golden,
        } => {
            let b = bundle::validate(&dir, &golden)?;
            println!(
                "valid: {} {} at {} ({} coins)",
                b.chain, b.transition, b.scan.height, b.scan.coins
            );
        }
        SegmentCmd::Select {
            registry,
            paths,
            nightly,
        } => {
            let registry = select::Registry::load(&registry)?;
            let text = match paths.as_deref() {
                None => String::new(),
                Some(p) if p == Path::new("-") => std::io::read_to_string(std::io::stdin())?,
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
            };
            let paths: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            // An empty list is a failed diff upstream more often than an empty change.
            ensure!(nightly || !paths.is_empty(), "segment select: no changed paths");
            let s = select::select(&registry, paths, nightly);
            for w in &s.run {
                println!("RUN {} {} {} {}", w.id, w.chain, w.base, w.to);
            }
            for w in &s.nightly {
                println!("NIGHTLY {} (over the {} s budget)", w.id, registry.budget_s);
            }
            for w in &s.deferred {
                println!("DEFERRED {} ({})", w.id, w.deferred.as_deref().unwrap_or_default());
            }
            println!("BUDGET {} of {} s", s.wall_s, registry.budget_s);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_digest(cmd: DigestCmd) -> Result<ExitCode> {
    use dash_conformance::digest::extract::{self, Options};

    if let DigestCmd::Compare(args) = cmd {
        return run_compare(args);
    }

    if let DigestCmd::Replay {
        node,
        out,
    } = cmd
    {
        let node = stored_node(&node)?;
        let replayed = dash_conformance::digest::replay::replay(&node, &out)?;
        println!(
            "replayed {}: {} blocks, tip {} {}",
            node.label(),
            replayed.blocks,
            replayed.tip.0,
            replayed.tip.1
        );
        return Ok(ExitCode::SUCCESS);
    }

    if let DigestCmd::L2(args) = cmd {
        return run_l2(args);
    }

    if let DigestCmd::RevCompare {
        chain,
        core,
        ours,
        to,
    } = cmd
    {
        return run_rev_compare(chain, &core, &ours, to);
    }

    if let DigestCmd::CheckBip9 {
        rpc_url,
        cookie,
        digest,
        run,
    } = cmd
    {
        let node = Rpc::new(rpc_url, Auth::Cookie(cookie));
        let digest =
            std::fs::read(&digest).with_context(|| format!("reading {}", digest.display()))?;
        let runs = run.into_iter().map(String::into_bytes).collect();
        let check = extract::check_bip9(&node, &digest, &runs)?;
        println!("{}", serde_json::json!({"tip": check.tip.to_hex(), "checked": check.checked}));
        return Ok(ExitCode::SUCCESS);
    }
    let DigestCmd::Extract {
        rpc_url,
        cookie,
        rpc_user,
        rpc_password,
        digest,
        out,
        dense,
        boundary,
        wait_secs,
        blocks_dir,
        other_params_run,
        summary: summary_path,
    } = cmd
    else {
        unreachable!("check-bip9 and compare returned above")
    };
    let auth = match (rpc_user, rpc_password, cookie) {
        (Some(u), Some(p), _) => Auth::UserPass(u, p),
        (_, _, Some(c)) => Auth::Cookie(c),
        _ => bail!("give --cookie, or --rpc-user and --rpc-password"),
    };
    let node = Rpc::new(rpc_url, auth);
    let digest_bytes =
        std::fs::read(&digest).with_context(|| format!("reading {}", digest.display()))?;
    let wait = std::time::Duration::from_secs(wait_secs);
    let opts = Options {
        dense,
        boundaries: boundary.into_iter().collect(),
        wait,
        poll: std::time::Duration::from_secs(1),
        blocks_dir: Some(blocks_dir),
        other_params_runs: other_params_run.into_iter().map(String::into_bytes).collect(),
    };

    let partial = {
        let mut name = out.file_name().context("--out has no file name")?.to_owned();
        name.push(".partial");
        out.with_file_name(name)
    };
    let result = (|| {
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(&partial)
                .with_context(|| format!("creating {}", partial.display()))?,
        );
        let summary = extract::extract(&node, &digest_bytes, &opts, &mut file)?;
        std::io::Write::flush(&mut file).context("flushing the golden file")?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all().context("syncing")?;
        std::fs::rename(&partial, &out)
            .with_context(|| format!("renaming {} to {}", partial.display(), out.display()))?;
        Ok::<_, anyhow::Error>(summary)
    })();
    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
    };
    eprintln!(
        "golden {}: chain {}, tip {}, run {}, {} C records, S records {:?}",
        out.display(),
        summary.chain.as_str(),
        summary.tip,
        summary.run,
        summary.blocks,
        summary.sparse
    );
    for skipped in &summary.skipped {
        eprintln!("skipped (node refused): {skipped}");
    }
    if !summary.bip9_unchecked.is_empty() {
        eprintln!(
            "BIP9 values unchecked (other parameters): {} C records",
            summary.bip9_unchecked.len()
        );
    }
    if summary.mnl_counter_adjusted > 0 {
        eprintln!(
            "mnl agreed after the counter adjustment: {} C records",
            summary.mnl_counter_adjusted
        );
    }
    if let Some(path) = summary_path {
        let json = serde_json::json!({
            "chain": summary.chain.as_str(),
            "tip": summary.tip,
            "run": summary.run,
            "blocks": summary.blocks,
            "sparse": summary.sparse,
            "skipped": summary.skipped,
            "bip9_unchecked": summary.bip9_unchecked,
            "mnl_counter_adjusted": summary.mnl_counter_adjusted,
        });
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

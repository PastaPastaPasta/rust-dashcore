//! `-importcoredatadir` (CH6a): a Dash Core data directory's block files staged into an empty
//! destination under [`dash_coreimport::datadir_import`]'s journal, then rdashd's reindex of
//! them. A start runs this or CH5-4b's in-place import ([`crate::core_import`]), never both
//! (DEC-1620): [`check_destination`] in `init_config`, [`prepare`] (P0 to P3) in step 7 before
//! any store opens, [`ImportStart::arm`] (P4) at the load, P5 in the slot after startup's ABC.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use dash_args::ArgsManager;
use dash_consensus::primitives::BlockHash;
#[cfg(any(test, feature = "test-hooks"))]
use dash_coreimport::datadir_import::FileMetadata;
use dash_coreimport::datadir_import::{
    self as driver, ImportError, ImportFs, ImportOptions, JOURNAL, Journal, OsImportFs, Phase,
    PlacementPolicy, ReindexArm, ReindexError, ReportedStatus, STAGING, SourceGuard, SourceMode,
    StageSource, StartOutcome, TipRelation,
};
use dash_coreimport::regtest_import;
use dash_coreimport::source::{self, Refusal, SourceInspection};
use dash_params::ChainSetup;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::recovery::RequestReindexError;
use dashd_store::KvEngine;
use dashd_store::flatfile::FlatFs;

use crate::chain::AfterAbc;
use crate::chainstate::Opening;
use crate::datadir::{self, DirLock};
use crate::error::InitFailure;
use crate::logging::Logger;
use crate::shutdown::ShutdownState;

/// The import's options as the arguments give them (h3: none of them without the directory).
pub(crate) struct Request {
    pub(crate) datadir: PathBuf,
    pub(crate) options: ImportOptions,
}

const OPTIONS: [&str; 3] = ["-importmode", "-importsource", "-importplacement"];

/// Whether `-importcoredatadir` is given (and not negated).
pub(crate) fn flag_set(args: &ArgsManager) -> bool {
    args.is_arg_set("-importcoredatadir") && !args.is_arg_negated("-importcoredatadir")
}

/// `-importcoredatadir` and its options, checked before anything is read or written: `None`
/// without it. `-importmode=fast` is CH6b's; `-importplacement=hardlink` needs
/// `-importsource=lock`.
pub(crate) fn request(args: &ArgsManager) -> Result<Option<Request>, InitFailure> {
    let set = |name: &str| args.is_arg_set(name) && !args.is_arg_negated(name);
    if !flag_set(args) {
        if let Some(name) = OPTIONS.iter().find(|name| set(name)) {
            return Err(InitFailure::Error(format!("{name} requires -importcoredatadir.")));
        }
        return Ok(None);
    }
    let dir = args.get_arg("-importcoredatadir", "")?;
    if dir.is_empty() {
        return Err(InitFailure::Error("-importcoredatadir requires a directory.".into()));
    }
    if !choose(args, "-importmode", [("safe", true), ("fast", false)])? {
        let text = crate::init::support::not_supported("-importmode=fast", "CH6b");
        return Err(InitFailure::Error(text));
    }
    let mode = choose(
        args,
        "-importsource",
        [("lock", SourceMode::Lock), ("snapshot", SourceMode::Snapshot)],
    )?;
    let placement = choose(
        args,
        "-importplacement",
        [("reflink", PlacementPolicy::Reflink), ("hardlink", PlacementPolicy::Hardlink)],
    )?;
    let options = ImportOptions {
        mode,
        placement,
    };
    options.validate().map_err(|e| InitFailure::Error(failed(&e)))?;
    Ok(Some(Request {
        datadir: PathBuf::from(dir),
        options,
    }))
}

/// The value of `name` among `choices`, the first its default.
fn choose<T: Copy, const N: usize>(
    args: &ArgsManager,
    name: &str,
    choices: [(&str, T); N],
) -> Result<T, InitFailure> {
    let v = args.get_arg(name, choices[0].0)?;
    choices
        .iter()
        .find(|(n, _)| *n == v)
        .map(|(_, t)| *t)
        .ok_or_else(|| InitFailure::Error(format!("Unknown {name} value {v}.")))
}

fn failed(e: &dyn std::fmt::Display) -> String {
    format!("Core datadir import: {e}")
}

/// The network directory's name under the base data directory: none on mainnet.
fn subdir(args: &ArgsManager) -> Result<Option<String>, InitFailure> {
    let (base, net) = (args.get_data_dir_base()?, args.get_data_dir_net()?);
    let rel = net.strip_prefix(&base).map_err(|_| {
        InitFailure::Error(failed(&"the network directory is not under the data directory"))
    })?;
    Ok((!rel.as_os_str().is_empty()).then(|| rel.to_string_lossy().into_owned()))
}

/// The options of a journal at `ReindexArmed`.
fn armed(journal: &Journal) -> Option<ImportOptions> {
    let intent = journal.intent().filter(|_| journal.last() == Some(Phase::ReindexArmed))?;
    Some(ImportOptions {
        mode: intent.mode,
        placement: intent.placement,
    })
}

/// `path` made absolute, its longest existing prefix canonical and the rest appended, as the
/// library's `canonical_existing` resolves it; a dangling symlink on the way resolves through
/// its target.
fn resolve(path: &Path, fs: &dyn ImportFs) -> Result<PathBuf, ImportError> {
    let mut path = std::path::absolute(path)?;
    'start: for _ in 0..40 {
        let (mut cur, mut rest) = (path.clone(), Vec::new());
        loop {
            if fs.metadata(&cur)?.is_some() {
                match fs.canonicalize(&cur) {
                    Ok(mut out) => {
                        out.extend(rest.iter().rev());
                        return Ok(out);
                    }
                    Err(e) => {
                        let target = std::fs::read_link(&cur).map_err(|_| e)?;
                        path = cur.parent().unwrap_or(Path::new("/")).join(target);
                        path.extend(rest.iter().rev());
                        continue 'start;
                    }
                }
            }
            match (cur.parent(), cur.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_owned());
                    cur = parent.to_owned();
                }
                _ => return Ok(path),
            }
        }
    }
    Err(std::io::Error::other("too many levels of symbolic links").into())
}

/// A flag-set start past `Placed` names the recorded `-importcoredatadir`, compared by path only
/// ([`resolve`]): the source itself is not read, so one that is gone still matches. Another path
/// is `ImportInProgress`.
fn same_datadir(given: &Path, journal: &Journal, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let recorded = &journal.intent().expect("a later phase follows Intent").datadir;
    let given = resolve(given, fs)?;
    if given != *recorded {
        return Err(ImportError::ImportInProgress(recorded.clone()));
    }
    Ok(())
}

/// h4's answer at `Reported`: "already imported", or the stored mismatch's error line.
fn reported(journal: &Journal) -> ImportError {
    match journal.reported() {
        Some(ReportedStatus::Mismatch) => ImportError::TipMismatch {
            report: driver::report_path(journal),
        },
        status => ImportError::AlreadyImported {
            source_dir: journal.intent().map(|i| i.datadir.clone()).unwrap_or_default(),
            status: status.unwrap_or(ReportedStatus::Complete).as_str().into(),
        },
    }
}

/// Logs [`reported`]'s line.
fn log_reported(e: &ImportError, logger: &Logger) {
    let error = if matches!(e, ImportError::TipMismatch { .. }) {
        "Error: "
    } else {
        ""
    };
    logger.log_printf(&format!("{error}Core datadir import: {e}\n"));
}

/// Whether the destination's import is below `ReindexArmed`, or has no journal yet.
pub(crate) fn fresh(args: &ArgsManager) -> Result<bool, InitFailure> {
    let path = args.get_data_dir_net()?.join(STAGING).join(JOURNAL);
    let fs = OsImportFs::new();
    let journal = fs.metadata(&path).ok().flatten().and_then(|_| Journal::open(&path, &fs).ok());
    Ok(journal.and_then(|j| j.last()).is_none_or(|l| l < Phase::ReindexArmed))
}

/// `init_config`'s check with `-importcoredatadir` given, before anything is written to
/// `dest_net`: the options, then the destination, read only and with no lock. A destination
/// holding blocks, a store, a Core directory's sign, CH5-4b's journal or backup, or another
/// import is `DestinationNotEmpty`. A journal at `ReindexArmed` outside hardlink mode needs no
/// source (§2.3).
pub(crate) fn check_destination(args: &ArgsManager, dest_net: &Path) -> Result<(), String> {
    let text = |e| match e {
        InitFailure::Error(e) | InitFailure::Exception(e) => e,
        InitFailure::Reported => String::new(),
    };
    let Some(request) = request(args).map_err(text)? else {
        return Ok(());
    };
    let not_empty = |path: PathBuf| failed(&ImportError::DestinationNotEmpty(path));
    if datadir::probe_core_data_dir(dest_net).is_some() {
        return Err(not_empty(dest_net.to_owned()));
    }
    if let Ok(entries) = std::fs::read_dir(dest_net) {
        for entry in entries {
            let path = entry.map_err(|e| failed(&e))?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == regtest_import::JOURNAL || name.starts_with(regtest_import::BACKUP_PREFIX) {
                return Err(not_empty(path));
            }
        }
    }
    let fs = OsImportFs::new();
    let journal_path = dest_net.join(STAGING).join(JOURNAL);
    if fs.metadata(&journal_path).map_err(|e| failed(&e))?.is_some() {
        let journal = Journal::open(&journal_path, &fs).map_err(|e| failed(&e))?;
        let at_reported = journal.last() == Some(Phase::Reported)
            && same_datadir(&request.datadir, &journal, &fs).is_ok();
        if at_reported {
            return driver::check_destination_layout(dest_net, &fs).map_err(|e| failed(&e));
        }
        if armed(&journal).is_some_and(|o| o.placement != PlacementPolicy::Hardlink) {
            same_datadir(&request.datadir, &journal, &fs).map_err(|e| failed(&e))?;
            return driver::check_destination_layout(dest_net, &fs).map_err(|e| failed(&e));
        }
    }
    let subdir = subdir(args).map_err(text)?;
    let source =
        driver::source_net_dir(&request.datadir, subdir.as_deref(), &fs).map_err(|e| failed(&e))?;
    driver::preflight_destination(dest_net, &source, &fs).map(drop).map_err(|e| failed(&e))
}

/// The source's lock, held from P0 until `SourceVerified` is durable, or in hardlink mode
/// until `Reported` is.
#[derive(Clone, Default)]
pub(crate) struct SourceLock(Arc<Mutex<Option<DirLock>>>);

impl SourceLock {
    fn take(dir: &Path) -> Result<SourceLock, ImportError> {
        match datadir::lock_existing(dir, datadir::LOCK_FILENAME) {
            Ok(Some(lock)) => Ok(SourceLock(Arc::new(Mutex::new(Some(lock))))),
            Ok(None) => Err(ImportError::LockUnavailable),
            Err(_) => Err(ImportError::InvalidOptions(format!(
                "Cannot obtain a lock on data directory {}. Dash Core is probably already running.",
                dir.to_string_lossy()
            ))),
        }
    }

    fn drop_lock(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

impl SourceGuard for SourceLock {
    fn release(&self) -> Result<(), ImportError> {
        self.drop_lock();
        Ok(())
    }
}

/// The source of a start at P0, inspected under its lock when `stage` asks.
struct Inspect<'a> {
    net_dir: PathBuf,
    chain: &'a ChainSetup,
    done: std::cell::OnceCell<SourceInspection>,
}

impl StageSource for Inspect<'_> {
    fn net_dir(&self) -> &Path {
        &self.net_dir
    }

    fn inspection(&self) -> Result<&SourceInspection, ImportError> {
        if let Some(done) = self.done.get() {
            return Ok(done);
        }
        let genesis = dashd_chainstate::chain::genesis_block(self.chain).hash();
        let inspected =
            source::inspect(&self.net_dir, self.chain.message_start, *genesis.as_bytes())
                .map_err(|e| ImportError::InvalidOptions(format!("cannot read the source: {e}")))?;
        if let Some(refusal) = inspected.refusal() {
            return Err(ImportError::InvalidOptions(refusal_text(&refusal)));
        }
        Ok(self.done.get_or_init(|| inspected))
    }
}

/// The text of a source CH6a refuses (plan §2.1).
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Pruned => {
            "the source is pruned; a pruned data directory cannot be imported".into()
        }
        Refusal::Gap(n) => format!("the source's block files have a gap at blk{n:05}.dat"),
        Refusal::NoGenesisFile => "the source has no blk00000.dat".into(),
        Refusal::WrongGenesis => "the source's genesis block is not this network's".into(),
        Refusal::Reindexing => {
            "the source is reindexing; let dashd finish its reindex, then stop it".into()
        }
        Refusal::CorruptBlockFile {
            file,
            offset,
        } => format!(
            "the source's block file {} is corrupt at offset {offset}",
            file.to_string_lossy()
        ),
        Refusal::MissingBestBlock => {
            "the source's chainstate records no best block; start and stop dashd once".into()
        }
        Refusal::BestBlockNotInFiles => "the source's best block is not in its block files".into(),
    }
}

/// An import at `Placed` or `ReindexArmed`: P4 at the open if `Placed`, then P5.
pub(crate) struct ImportStart {
    pub(crate) journal: Journal,
    pub(crate) fs: Arc<dyn ImportFs>,
    pub(crate) lock: SourceLock,
}

/// Step 7's part, after init's refusals and before any store opens. `None` when there is
/// nothing to import: no `-importcoredatadir` and no journal below `Reported`, or an import
/// already reported (logged). At `ReindexArmed` outside hardlink mode it reads no source (§2.3).
pub(crate) fn prepare(
    args: &ArgsManager,
    chain: &ChainSetup,
    logger: &Logger,
    fs: Arc<dyn ImportFs>,
) -> Result<Option<ImportStart>, InitFailure> {
    #[cfg(feature = "test-hooks")]
    hooks::arm_reflink_failure();
    let err = |e: ImportError| InitFailure::Error(failed(&e));
    let dest = args.get_data_dir_net()?;
    let requested = request(args)?;
    let journal_path = dest.join(STAGING).join(JOURNAL);
    let journal = match fs.metadata(&journal_path).map_err(|e| err(e.into()))? {
        Some(_) => Some(Journal::open(&journal_path, &*fs).map_err(err)?),
        None if requested.is_none() => return Ok(None),
        None => None,
    };
    let subdir = subdir(args)?;
    let recorded = journal.as_ref().and_then(armed);
    let journal = match (journal, recorded) {
        (Some(mut journal), Some(recorded)) if recorded.placement != PlacementPolicy::Hardlink => {
            if let Some(request) = &requested {
                same_datadir(&request.datadir, &journal, &*fs).map_err(err)?;
            }
            if requested.as_ref().is_some_and(|r| r.options != recorded) {
                return Err(err(ImportError::OptionsDiffer {
                    mode: recorded.mode.as_str(),
                    placement: recorded.placement.as_str(),
                }));
            }
            driver::check_destination_layout(&dest, &*fs).map_err(err)?;
            journal.repair(&*fs).map_err(err)?;
            return Ok(Some(ImportStart {
                journal,
                fs,
                lock: SourceLock::default(),
            }));
        }
        (Some(journal), _)
            if journal.last() == Some(Phase::Reported)
                && requested
                    .as_ref()
                    .is_some_and(|r| same_datadir(&r.datadir, &journal, &*fs).is_ok()) =>
        {
            log_reported(&reported(&journal), logger);
            return Ok(None);
        }
        (journal, _) => journal,
    };
    let (datadir, options) = match &requested {
        Some(request) => (request.datadir.clone(), request.options),
        None => {
            let journal = journal.as_ref().expect("a flagless start here has a journal");
            driver::import_start(journal, None, subdir.as_deref(), &*fs).map_err(err)?;
            match (recorded, journal.intent()) {
                (Some(options), Some(intent)) => (intent.datadir.clone(), options),
                _ => return Ok(None),
            }
        }
    };
    let source = driver::source_net_dir(&datadir, subdir.as_deref(), &*fs).map_err(err)?;
    let journal = driver::preflight_destination(&dest, &source, &*fs).map_err(err)?;
    if let Some(journal) = &journal {
        driver::import_start(journal, Some(&datadir), subdir.as_deref(), &*fs).map_err(err)?;
    }
    let last = journal.as_ref().and_then(Journal::last);
    let locked = options.mode == SourceMode::Lock
        && (last.is_none_or(|l| l < Phase::SourceVerified)
            || options.placement == PlacementPolicy::Hardlink);
    let lock = if locked && last != Some(Phase::Reported) {
        SourceLock::take(&source).map_err(err)?
    } else {
        SourceLock::default()
    };
    let src = Inspect {
        net_dir: source,
        chain,
        done: std::cell::OnceCell::new(),
    };
    let staged = driver::stage(&src, options, &datadir, subdir.as_deref(), &dest, &*fs, &lock);
    match staged {
        Ok(Phase::Placed | Phase::ReindexArmed) => {}
        Ok(phase) => {
            return Err(err(ImportError::PhaseOrder {
                last: Some(phase),
                got: Phase::Placed,
            }));
        }
        Err(
            e @ (ImportError::AlreadyImported {
                ..
            }
            | ImportError::TipMismatch {
                ..
            }),
        ) => {
            log_reported(&e, logger);
            return Ok(None);
        }
        Err(e) => return Err(err(e)),
    }
    let journal = Journal::open(&journal_path, &*fs).map_err(err)?;
    if options.placement != PlacementPolicy::Hardlink {
        lock.drop_lock();
    }
    Ok(Some(ImportStart {
        journal,
        fs,
        lock,
    }))
}

/// P4's request on the chainstate opening, refused on a store that holds a chainstate.
struct ArmOn<'a, E: KvEngine, F: FlatFs>(RefCell<&'a mut Opening<E, F>>);

impl<E: KvEngine, F: FlatFs> ReindexArm for ArmOn<'_, E, F> {
    fn request_reindex(&self) -> Result<(), ReindexError> {
        let mut opening = self.0.borrow_mut();
        match opening.holds_chainstate() {
            Ok(true) => return Err(ReindexError::NotEmpty),
            Ok(false) => {}
            Err(e) => return Err(ReindexError::Other(e)),
        }
        opening.request_reindex().map_err(|e| match e {
            RequestReindexError::NotEmpty => ReindexError::NotEmpty,
            RequestReindexError::WipeInProgress => ReindexError::WipeInProgress,
            e => ReindexError::Other(e.to_string()),
        })
    }
}

impl ImportStart {
    /// P4, at `Placed` only: the store's reindex flag, then `ReindexArmed`. A store holding a
    /// chainstate, a block index or a wipe stops the import with nothing written.
    pub(crate) fn arm<E: KvEngine, F: FlatFs>(
        &mut self,
        opening: &mut Opening<E, F>,
    ) -> Result<(), String> {
        if self.journal.last() != Some(Phase::Placed) {
            return Ok(());
        }
        let arm = ArmOn(RefCell::new(opening));
        driver::arm_reindex(&mut self.journal, &arm, &*self.fs).map(drop).map_err(|e| failed(&e))
    }

    /// P5 for the slot after startup's ABC: only a start with no shutdown or abort reports
    /// `complete` or `mismatch` (h6). A reindex scan this start did not finish reaches the slot
    /// only after an `AbortNode`, whose `abort` makes it incomplete.
    pub(crate) fn p5(
        self,
        v: &ChainHandle,
        shutdown: &Arc<ShutdownState>,
        logger: &Arc<Logger>,
    ) -> Box<dyn FnOnce() + Send> {
        #[cfg(feature = "test-hooks")]
        hooks::arm_block(v, shutdown, logger);
        let (v, shutdown, logger) = (v.clone(), Arc::clone(shutdown), Arc::clone(logger));
        Box::new(move || self.report(&v, v.view().snap.tip.tip_hash, &shutdown, &logger))
    }

    /// The source's lock, shared: a holder keeps it while the node runs, until `Reported`.
    pub(crate) fn lock(&self) -> SourceLock {
        self.lock.clone()
    }

    /// P5 on `tip`, at `ReindexArmed` only. Its line names an `ancestor` tip's height and the source's best block's; in hardlink mode
    /// the source's lock stays held until `Reported`.
    pub(crate) fn report(
        mut self,
        v: &ChainHandle,
        tip: BlockHash,
        shutdown: &ShutdownState,
        logger: &Logger,
    ) {
        if self.journal.last() != Some(Phase::ReindexArmed) {
            return;
        }
        let best = self.journal.intent().and_then(|i| i.best_block).map(BlockHash::from);
        let hardlink =
            self.journal.intent().is_some_and(|i| i.placement == PlacementPolicy::Hardlink);
        let relation = relation(v, best, tip);
        let outcome = StartOutcome {
            abc_ok: true,
            shutdown: shutdown.shutdown_requested(),
            abort: shutdown.aborted(),
            relation,
            destination_tip: Some(*tip.as_bytes()),
        };
        match driver::report(&mut self.journal, outcome, &*self.fs) {
            Ok(report) => {
                let mut status = report.status.clone();
                if let Some(reason) = &report.reason {
                    status += &format!(" ({reason})");
                }
                if relation == TipRelation::Ancestor
                    && report.reason.is_some()
                    && let Some(best) = best
                {
                    let reader = v.reader();
                    let height = |h: BlockHash| {
                        reader.lookup(&h).and_then(|id| reader.entry(id)).map_or(0, |e| e.height)
                    };
                    status += &format!(
                        ", relation {}, tip height {}, best block height {}",
                        relation.as_str(),
                        height(tip),
                        height(best)
                    );
                }
                if hardlink && report.reason.is_some() {
                    status += ", the source's lock is held while the node runs";
                }
                logger.log_printf(&format!(
                    "Core datadir import: {status}, report {}\n",
                    report.path.to_string_lossy()
                ));
            }
            Err(e) => logger.log_printf(&format!("Error: Core datadir import: {e}\n")),
        }
        if self.journal.last() == Some(Phase::Reported) {
            self.lock.drop_lock();
        }
    }
}

/// The destination tip's relation to the source's best block `best` (§2.3). A best block the
/// destination does not hold is `mismatch`.
fn relation(v: &ChainHandle, best: Option<BlockHash>, tip: BlockHash) -> TipRelation {
    let reader = v.reader();
    let (Some(best), Some(tip)) = (best.and_then(|b| reader.lookup(&b)), reader.lookup(&tip))
    else {
        return TipRelation::Mismatch;
    };
    let height = |id| reader.entry(id).map(|e| e.height);
    let (Some(best_height), Some(tip_height)) = (height(best), height(tip)) else {
        return TipRelation::Mismatch;
    };
    if best == tip {
        TipRelation::Equal
    } else if tip_height > best_height && reader.ancestor(tip, best_height) == Some(best) {
        TipRelation::Descendant
    } else if tip_height < best_height && reader.ancestor(best, tip_height) == Some(tip) {
        TipRelation::Ancestor
    } else {
        TipRelation::Mismatch
    }
}

/// The slot after startup's ABC: the import's P5 first, then `-stopafterblockimport`'s stop.
pub(crate) fn after_abc(
    p5: Option<Box<dyn FnOnce() + Send>>,
    stop: Option<Box<dyn FnOnce() -> AfterAbc + Send>>,
) -> Option<Box<dyn FnOnce() -> AfterAbc + Send>> {
    let Some(p5) = p5 else {
        return stop;
    };
    Some(Box::new(move || {
        p5();
        stop.map_or(AfterAbc::Continue, |stop| stop())
    }))
}

/// The OS's filesystem, with the test hooks' kill point in a build with them.
pub(crate) fn import_fs() -> Arc<dyn ImportFs> {
    let fs: Arc<dyn ImportFs> = Arc::new(OsImportFs::new());
    #[cfg(feature = "test-hooks")]
    let fs = hooks::wrap(fs);
    fs
}

/// A filesystem whose journal appends first pass the record's phase byte to `before`.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) struct OnAppend {
    pub(crate) inner: Arc<dyn ImportFs>,
    pub(crate) before: Box<dyn Fn(u8) -> std::io::Result<()> + Send + Sync>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl ImportFs for OnAppend {
    fn reflink(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        self.inner.reflink(src, dst)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        self.inner.hard_link(src, dst)
    }
    fn copy(&self, src: &Path, dst: &Path) -> std::io::Result<u64> {
        self.inner.copy(src, dst)
    }
    fn sync_data(&self, path: &Path) -> std::io::Result<()> {
        self.inner.sync_data(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn rename(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        self.inner.rename(src, dst)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove(path)
    }
    fn truncate(&self, path: &Path, len: u64) -> std::io::Result<()> {
        self.inner.truncate(path, len)
    }
    fn append(&self, path: &Path, data: &[u8]) -> std::io::Result<()> {
        if path.file_name().is_some_and(|n| n == JOURNAL)
            && let Some(&phase) = data.get(8)
        {
            (self.before)(phase)?;
        }
        self.inner.append(path, data)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<Option<FileMetadata>> {
        self.inner.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.inner.read(path)
    }
    fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn available_space(&self, path: &Path) -> std::io::Result<u64> {
        self.inner.available_space(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn mount_table(&self) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.mount_table()
    }
    fn mount_id(&self, path: &Path) -> std::io::Result<Option<u64>> {
        self.inner.mount_id(path)
    }
    fn sha256(&self, path: &Path) -> std::io::Result<[u8; 32]> {
        self.inner.sha256(path)
    }
}

/// The process test hooks (CH6a-4 step 8), `RDASHD_TEST_HOOK=datadir-import` with
/// `RDASHD_TEST_HOOK_POINT`:
/// - `kill-in-scan:<n>`: the process exits with status 3 as the reindex scan opens `blk<n>`;
/// - `kill-at-block:<hash>`: it exits as V publishes the block `<hash>` (display hex) as its tip;
/// - `hold-at-block:<hash>`: V waits there until a shutdown is requested, at most two minutes,
///   and panics if none is;
/// - `kill-before-reported`: it exits in P5 once the report is durable, before the `Reported`
///   record's append.
///
/// `RDASHD_TEST_NO_REFLINK=1` makes every FICLONE fail, read once as the import starts.
#[cfg(feature = "test-hooks")]
pub(crate) mod hooks {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use dash_coreimport::datadir_import::{FORCE_REFLINK_FAILURE, ImportFs, Phase};
    use dashd_chainstate::chain::ChainHandle;

    use crate::chain::{BlkFiles, BlkOpen};
    use crate::logging::Logger;
    use crate::shutdown::ShutdownState;

    const HOLD_AT_MOST: Duration = Duration::from_secs(120);

    fn point() -> Option<String> {
        (std::env::var("RDASHD_TEST_HOOK").as_deref() == Ok("datadir-import"))
            .then(|| std::env::var("RDASHD_TEST_HOOK_POINT").expect("RDASHD_TEST_HOOK_POINT"))
    }

    fn at(prefix: &str) -> Option<String> {
        point()?.strip_prefix(prefix).map(str::to_owned)
    }

    pub(crate) fn arm_reflink_failure() {
        if std::env::var("RDASHD_TEST_NO_REFLINK").as_deref() == Ok("1") {
            FORCE_REFLINK_FAILURE.store(true, Ordering::SeqCst);
        }
    }

    struct ScanKill {
        inner: Box<dyn BlkFiles>,
        file: u32,
    }

    impl BlkFiles for ScanKill {
        fn open(&self, file: u32) -> BlkOpen {
            if file == self.file {
                crate::node::exit_now(3);
            }
            self.inner.open(file)
        }
        fn open_external(&self, path: &Path) -> Option<Box<dyn std::io::Read + Send>> {
            self.inner.open_external(path)
        }
    }

    pub(crate) fn scan_files(files: Box<dyn BlkFiles>) -> Box<dyn BlkFiles> {
        match at("kill-in-scan:").map(|n| n.parse().expect("kill-in-scan:<file>")) {
            Some(file) => Box::new(ScanKill {
                inner: files,
                file,
            }),
            None => files,
        }
    }

    pub(crate) fn arm_block(v: &ChainHandle, shutdown: &Arc<ShutdownState>, logger: &Arc<Logger>) {
        let (kill, hash) = match (at("kill-at-block:"), at("hold-at-block:")) {
            (Some(hash), _) => (true, hash),
            (None, Some(hash)) => (false, hash),
            (None, None) => return,
        };
        let (reader, shutdown, logger) =
            (v.reader().clone(), Arc::clone(shutdown), Arc::clone(logger));
        v.set_between_stores(Some(Arc::new(move || {
            if reader.snapshot().tip.tip_hash.to_string() != hash {
                return;
            }
            if kill {
                crate::node::exit_now(3);
            }
            logger.log_printf(&format!("test-hooks: datadir import held at block {hash}\n"));
            let start = Instant::now();
            while !shutdown.shutdown_requested() {
                assert!(start.elapsed() < HOLD_AT_MOST, "hold-at-block: no shutdown requested");
                std::thread::sleep(Duration::from_millis(10));
            }
        })));
    }

    pub(crate) fn wrap(fs: Arc<dyn ImportFs>) -> Arc<dyn ImportFs> {
        match point().as_deref() {
            Some("kill-before-reported") => Arc::new(super::OnAppend {
                inner: fs,
                before: Box::new(|phase| {
                    if phase == Phase::Reported as u8 {
                        crate::node::exit_now(3);
                    }
                    Ok(())
                }),
            }),
            _ => fs,
        }
    }
}

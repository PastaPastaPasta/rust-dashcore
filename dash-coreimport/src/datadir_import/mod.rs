//! Safe Core datadir import: the journaled driver (CH6a-2, plan §2.1-§2.4).
//!
//! The import places every source `blk` file in `<dest_net>/import-staging/blocks`
//! (reflink, else copy; hardlinks only when opted in), verifies the source did
//! not change, and renames the staged dir to `<dest_net>/blocks`. Each step is
//! a durable phase record in `import-staging/JOURNAL`; a restart resumes from
//! the last durable record and what it observes on disk. The tables are the
//! accepted SP2 tables (OD-CP-296) with each row's test; every name in the
//! Test column is a test fn (`module_doc_names_real_tests` checks this).
//!
//! ## Durable phases
//!
//! | Phase | Record | Writes completed before the record | Test |
//! | --- | --- | --- | --- |
//! | P0 | [`Phase::Intent`] | The caller holds the source lock and `inspect` passed; manifest M0 is scanned. The record carries the canonical source path and `(st_dev, st_ino)`, mode, placement, `'B'`, `'l'` and M0. | `intent_record_binds_source_and_m0_durably` |
//! | P1 | [`Phase::Staged`] | Every blk file placed and `fdatasync`ed; each `copy`-method file re-read and compared with M0, a mismatch refusing as `StagedCopyMismatch { path }` with no `Staged` record. Only a FICLONE `EOPNOTSUPP`, `EXDEV` or `EINVAL` falls back to a hardlink or copy; any other error stops with no `Staged` record. Then `fsync(staging/blocks)` and `fsync(import-staging)`. The record carries the per-file method table. | `staged_data_survives_crash_after_staged`, `short_copy_refuses_without_staged_record`, `os_copy_and_reflink_never_open_an_existing_name`, `reflink_errors_outside_the_fallback_set_propagate` |
//! | P2 | [`Phase::SourceVerified`] | M1 compared with M0 (the exact link delta). Only after the record is durable is the lock released; in hardlink mode `stage` never releases it. | `source_verified_is_durable_before_lock_release`, `hardlink_mode_never_releases_the_lock` |
//! | P3 | [`Phase::Placed`] | `rename(import-staging/blocks, <dest_net>/blocks)` onto a missing or empty target, then `fsync(import-staging)` and `fsync(<dest_net>)`. | `placed_survives_crash_right_after_append`, `placed_append_follows_both_parent_syncs` |
//! | P4 | [`Phase::ReindexArmed`] | [`arm_reindex`]: the caller's [`ReindexArm`] returned Ok; `NotEmpty` and `WipeInProgress` stop with no retry and no write. | `arm_reindex_appends_only_after_ok`, `arm_reindex_stops_on_not_empty_or_wipe` |
//! | P5 | [`Phase::Reported`] | [`report`]: `import-report.json` written, `fdatasync`ed and its dir `fsync`ed. `equal` and `descendant` append `Reported`; `mismatch` appends it with that status and returns the error line; `ancestor`, a shutdown, an abort or an ABC failure append nothing and write status `incomplete`. | `report_equal_appends_complete`, `report_descendant_appends_complete`, `report_mismatch_is_terminal_with_error_line`, `report_ancestor_is_incomplete`, `report_shutdown_is_incomplete`, `report_abort_is_incomplete`, `report_requires_reindex_armed` |
//!
//! ## Recovery by observed state
//!
//! *Present* means the dir has an entry; *absent* means it is missing or empty.
//!
//! | Last record | Observed | Action | Test |
//! | --- | --- | --- | --- |
//! | none / `Intent` | anything in staging | Remove every `import-staging` entry except `JOURNAL`, `fsync(import-staging)`, truncate the journal to the magic and `fdatasync` it, then restart at P0. | `resume_intent_clears_staging_and_restarts`, `hardlink_crash_before_staged_leaves_source_intact`, `begin_intent_clears_staged_links_before_reset_cut_matrix` |
//! | `Staged` | `staging/blocks` present and equal to M0 | Redo P2: M1 vs M0 (the caller re-took the lock). | `resume_staged_redoes_p2` |
//! | `Staged` | staged files missing, extra or not matching M0 | The reset below, then refuse with `StagingChanged`. | `resume_staged_rehashes_staged_files` |
//! | `Staged` | M1 ≠ M0 on that redo | Truncate the journal to the magic and `fdatasync` it, then clear staging and `fsync(import-staging)`, release the lock, refuse with `SourceChanged`. | `resume_staged_source_changed_resets`, `source_changed_reset_cut_matrix` |
//! | `Staged` | `staging/blocks` absent | `JournalCorrupt` (pre-`Placed` text); nothing touched. | `staged_without_staging_blocks_is_corrupt` |
//! | `SourceVerified` | staging present, `dest/blocks` absent | Do P3. | `resume_source_verified_places_blocks` |
//! | `SourceVerified` | staging absent, `dest/blocks` present | The rename landed: `fsync` both parents, append `Placed`. | `resume_source_verified_after_landed_rename_appends_placed` |
//! | `SourceVerified` | both present, or neither | `JournalCorrupt` (pre-`Placed` text); nothing touched. | `corrupt_source_verified_both_present`, `corrupt_source_verified_neither_present` |
//! | `Placed` | `dest/blocks` present | `stage` returns `Placed`; the caller runs [`arm_reindex`]. | `resume_placed_returns_placed_then_arms` |
//! | `Placed` | `dest/blocks` absent (missing or empty) | `JournalCorrupt` (from-`Placed` text); nothing touched. | `corrupt_placed_missing_blocks`, `corrupt_placed_empty_blocks` |
//! | `ReindexArmed` | any | A normal start; P5 ([`report`]) runs after a clean, abort-free connect. | `resume_reindex_armed_runs_report` |
//! | `Reported` | same source, any `-importsource`/`-importplacement` | "already imported from `<dir>`" (the base datadir) with the stored status; a stored `mismatch` repeats its error line. | `resume_reported_is_already_imported`, `resume_reported_mismatch_repeats_error`, `resume_reported_with_other_options_is_already_imported` |
//! | `Reported` | another source | Refused (`DestinationNotEmpty`). | `resume_reported_other_source_refused` |
//! | `Reported` | corrupt trailing bytes | Ignored with a warning. | `reported_trailing_corruption_is_ignored` |
//! | `SourceVerified` to `ReindexArmed`, hardlink mode | a linked source file differs from M0 | Every resume re-hashes each linked file against M0: at `SourceVerified` the `SourceChanged` reset; at `Placed` and `ReindexArmed`, `JournalCorrupt` with the destination text. | `hardlink_resume_source_verified_change_resets`, `hardlink_landed_reset_cut_matrix`, `hardlink_resume_placed_change_refuses`, `hardlink_resume_rechecks_every_phase_past_source_verified` |
//! | `Reported`, hardlink mode | a linked source file changed (a later source `-reindex`) | No re-check (OD-CP-303): the normal `Reported` start. | `reported_hardlink_destination_starts_after_a_linked_file_changes` |
//! | any | a destination path the import writes is a symlink or the wrong kind | Refused (`DestinationSymlink`) before any open or write. | `symlinked_destination_paths_are_refused`, `os_symlinked_import_staging_is_refused` |
//! | any | the source and destination nest, or share a dir (by `(st_dev, st_ino)`, through symlinks and bind mounts); the dir walk covers both whole trees | Refused (`Overlap`) before any write. | `exclusion_refuses_nesting_both_ways`, `exclusion_refuses_bind_alias`, `exclusion_refuses_shared_subtree`, `exclusion_refuses_bind_alias_through_scoped_dirs` |
//! | any | the mount table (`/proc/self/mountinfo`, read on every check) lets the canonical source network dir and destination reach one another: each located by its `statx` mount id (by the longest mount-point prefix where the kernel gives none), on one filesystem (`major:minor`) with one's path inside it equal to or under the other's; or a mount whose mount point is at or under one of them with a `(major:minor, root)` equal to, under or above the other's located path. This covers a bind made from under a dir the walk cannot list, either way round and at any depth below either root | Refused (`Overlap`) before any write. | `exclusion_refuses_bind_from_under_an_unlistable_dir`, `exclusion_refuses_a_bind_below_the_destination_from_an_unlistable_source_dir`, `exclusion_refuses_a_source_alias_mounted_under_an_unlistable_destination_dir`, `exclusion_passes_an_unrelated_destination_on_the_same_filesystem`, `exclusion_passes_an_unrelated_mount_below_the_destination`, `exclusion_locates_a_root_by_its_mount_id`, `exclusion_falls_back_to_the_mount_point_prefix_without_a_mount_id`, `os_mount_ids_locate_the_roots_in_the_mount_table`, `mountinfo_parses_escapes_and_optional_fields`, `mountinfo_picks_the_longest_mount_point_prefix` |
//! | any | the mount table cannot be read or parsed, or lacks the mount of either path (a mount id not in it, or no mount point above the path) (Linux) | Refused (`MountTable`, naming `/proc/self/mountinfo`) before any write. | `exclusion_refuses_an_unreadable_or_garbled_mount_table`, `exclusion_refuses_a_mount_id_missing_from_the_table`, `mountinfo_rejects_garbled_lines` |
//! | any | a dir outside the trees the import touches (the source's scoped dirs, the destination's `blocks/` and `import-staging/`) that cannot be listed, or a child of one that cannot be stat'ed, with `EACCES` or `EPERM`, e.g. a `lost+found` or a `backups/` | The dir counts as empty, the child as absent; the import runs. | `os_unreadable_dirs_outside_the_import_scope_are_ignored`, `exclusion_skips_unlistable_dirs_only_outside_the_scope`, `exclusion_skips_unstatable_children_only_outside_the_scope`, `os_unsearchable_dirs_follow_the_scope_rule` |
//! | any | a dir inside those trees that cannot be listed or a child that cannot be stat'ed (any error but a vanished child), any other error outside them, or an unreadable destination `store/` or `chainstate/` | Refused (`UnreadableDir`, naming it as lossy text plus hex bytes) before any write. | `os_unreadable_dir_in_a_scoped_tree_is_refused_by_name`, `exclusion_skips_unlistable_dirs_only_outside_the_scope`, `exclusion_skips_unstatable_children_only_outside_the_scope`, `exclusion_refuses_other_errors_outside_the_scope`, `os_unsearchable_dirs_follow_the_scope_rule`, `unreadable_store_dir_is_refused_by_name`, `os_unreadable_store_dir_is_refused_by_name` |
//! | any | known limit: a scoped dir that is its own mount point holding a dir the importer cannot list, e.g. a root-owned `blocks/lost+found` | Refused (`UnreadableDir`, naming that dir); the operator must move the scoped dir's mount so no unreadable dir sits inside it. | `os_unreadable_dir_in_a_scoped_tree_is_refused_by_name` |
//! | any | known limit: a chroot whose root is not a mount point; the kernel lists no mount whose mount point lies outside the reader's root, so the table is empty or holds only mounts made inside the chroot | Every import refuses (`MountTable`, giving that reason and the remedy: run the node outside the chroot, or bind-mount the chroot root onto itself). | `mount_table_without_the_root_mount_names_the_chroot_remedy` |
//! | any | known limit: a target without a mount table (not Linux) | No mount-table check: a destination bind-mounted from under a source dir outside the scoped trees that the walk cannot list, or the reverse, is not seen. | `exclusion_skips_the_mount_check_without_a_mount_table` |
//! | none | an empty `store/` or `chainstate/` | Absent: the import runs. | `preflight_treats_empty_store_dirs_as_absent` |
//! | below `Reported` | `-importsource`/`-importplacement` differ from the journal | Refused, naming the journal's values. | `resume_options_must_match_journal` |
//! | any | `-importcoredatadir` (with the network subdir) does not resolve to the source by canonical path plus `(st_dev, st_ino)`, e.g. the network dir's parent passed on mainnet | `stage` refuses (`InvalidOptions`) before any write. A symlinked network subdir resolves like [`import_start`] resolves it; the canonical base datadir is recorded. | `os_symlinked_network_subdir_stages_and_resumes`, `os_mainnet_parent_of_the_network_dir_is_refused` |
//! | below `Reported` | known limit: a final append whose lost data is not one zero run from the record's head to a sector boundary (a large append cut out of page order, as XFS can leave it, or a lost page ending inside the 9-byte header) | `JournalCorrupt` with the cleanup text; nothing truncated. | `journal_out_of_order_final_append_is_corrupt` |
//! | below `ReindexArmed` | no matching `-importcoredatadir` | `ImportInProgress`, naming the base datadir to pass; from `ReindexArmed` on P5 runs without the option ([`import_start`]). | `import_start_requires_matching_source_until_reindex_armed`, `operator_texts_name_the_base_datadir` |
//!
//! Every durable cut of `stage`, under `crash`, `crash_keep`, `crash_torn_head`
//! and eight seeds of `crash_mixed`, resumes to the uncut end state with the
//! source unchanged: `cut_matrix_reflink`, `cut_matrix_copy`,
//! `cut_matrix_hardlink`, `cut_matrix_snapshot`. The same holds for every cut
//! of P4 and P5 after `Placed`: `arm_and_report_cut_matrix`.
//!
//! A record whose header fails its crc, names the wrong phase or has an
//! out-of-bound length is `JournalCorrupt` with nothing truncated, wherever it
//! sits: `journal_header_flip_is_corrupt_and_truncates_nothing`,
//! `finished_import_with_a_header_flip_is_corrupt_not_in_progress`,
//! `journal_length_above_the_bound_is_corrupt`,
//! `journal_single_header_bit_flip_with_zero_low_length_byte_is_corrupt`. A
//! final record whose head never reached the disk (an all-zero header, or a
//! valid header and a payload zero up to the next 512-byte sector boundary or
//! EOF) is a torn append: `journal_zero_head_final_frame_is_torn`. The scan
//! for a valid frame after such a record hashes at most four payload bytes
//! per journal byte and refuses with `JournalCorrupt` past that:
//! `journal_torn_tail_scan_is_bounded_by_the_input`.

/// Durable import phases, in journal order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Phase {
    Intent = 0,
    Staged = 1,
    SourceVerified = 2,
    Placed = 3,
    ReindexArmed = 4,
    Reported = 5,
}

#[cfg(any(test, feature = "test-hooks"))]
pub mod cutfs;
mod fs;
mod journal;
mod manifest;
mod mounts;
mod place;
mod report;
#[cfg(test)]
mod tests;

use crate::source::SourceInspection;
#[cfg(feature = "test-hooks")]
pub use fs::FORCE_REFLINK_FAILURE;
pub use fs::{FileMetadata, ImportFs, OsImportFs};
pub use journal::{
    FileMethod, Intent, Journal, Method, Record, RecordBody, ReportedStatus, SourceBinding, frame,
};
pub use manifest::{ManifestEntry, SCOPED_DIRS, SourceManifest};
pub use mounts::MOUNTINFO;
pub use place::{Placement, place, required_space};
pub use report::{
    HARDLINK_GUARANTEE, ImportReport, LOCK_GUARANTEE, NOT_IMPORTED, SNAPSHOT_GUARANTEE,
    StartOutcome, TipRelation, guarantee, report, report_path,
};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub const STAGING: &str = "import-staging";
pub const JOURNAL: &str = "JOURNAL";
/// The destination dirs that mean a store exists.
pub const STORE_DIRS: [&str; 2] = ["store", "chainstate"];

/// `-importsource`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceMode {
    Lock,
    Snapshot,
}
impl SourceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Snapshot => "snapshot",
        }
    }
}

/// `-importplacement`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementPolicy {
    Reflink,
    Hardlink,
}
impl PlacementPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reflink => "reflink",
            Self::Hardlink => "hardlink",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportOptions {
    pub mode: SourceMode,
    pub placement: PlacementPolicy,
}
impl ImportOptions {
    /// The InitError checks, run before anything is read or written.
    pub fn validate(self) -> Result<(), ImportError> {
        if self.mode == SourceMode::Snapshot && self.placement == PlacementPolicy::Hardlink {
            return Err(ImportError::InvalidOptions(
                "-importplacement=hardlink requires -importsource=lock".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    InvalidOptions(String),
    #[error(
        "the import journal was started with -importsource={mode} -importplacement={placement}; restart with those options"
    )]
    OptionsDiffer {
        mode: &'static str,
        placement: &'static str,
    },
    #[error("the source changed since staging; start again to import from scratch ({0:?})")]
    SourceChanged(Vec<PathBuf>),
    #[error(
        "the staged files no longer match the source manifest; start again to import from scratch ({0:?})"
    )]
    StagingChanged(Vec<PathBuf>),
    #[error("staged copy mismatch: {path}")]
    StagedCopyMismatch {
        path: PathBuf,
    },
    #[error("refusing to write {0}: it is a source file's inode")]
    StagingAliasesSource(PathBuf),
    #[error(
        "refusing {0}: the import's destination paths must be plain dirs and singly linked files, not symlinks"
    )]
    DestinationSymlink(PathBuf),
    #[error("destination is not empty: {0}")]
    DestinationNotEmpty(PathBuf),
    #[error("source {source_dir} and destination {dest} are equal, nested or share an inode")]
    Overlap {
        source_dir: PathBuf,
        dest: PathBuf,
    },
    #[error(
        "cannot list {path} (path bytes {path_hex}): {error}; the import must list every dir under the source's blocks, chainstate, evodb, llmq and indexes dirs and the destination's blocks, import-staging, store and chainstate dirs, and skips only a permission denial elsewhere"
    )]
    UnreadableDir {
        path: String,
        path_hex: String,
        error: io::Error,
    },
    #[error(
        "cannot check the mount table {MOUNTINFO}: {0}; the import reads it to refuse a destination bind-mounted into the source or the reverse"
    )]
    MountTable(String),
    #[error("{0} is a network subdir; pass the base -datadir")]
    NetSubdir(PathBuf),
    #[error(
        "cannot take the source's lock; stop dashd, fix permissions, or use -importsource=snapshot"
    )]
    LockUnavailable,
    #[error("-blocksdir sources are not supported")]
    BlocksDirElsewhere,
    #[error(
        "insufficient disk space for the import: {required} bytes needed, {available} available"
    )]
    InsufficientSpace {
        required: u64,
        available: u64,
    },
    #[error("{0}")]
    JournalCorrupt(String),
    #[error("import journal phase out of order: {got:?} after {last:?}")]
    PhaseOrder {
        last: Option<Phase>,
        got: Phase,
    },
    #[error("an import from {0} is in progress; restart with -importcoredatadir={0}")]
    ImportInProgress(PathBuf),
    #[error("already imported from {source_dir} (status {status})")]
    AlreadyImported {
        source_dir: PathBuf,
        status: String,
    },
    #[error("the import did not reproduce the source's tip; see {report}")]
    TipMismatch {
        report: PathBuf,
    },
    #[error("cannot arm the reindex: the destination store is not empty")]
    ReindexNotEmpty,
    #[error("cannot arm the reindex: a store wipe is in progress")]
    ReindexWipeInProgress,
    #[error("the reindex request failed: {0}")]
    ReindexFailed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The `JournalCorrupt` operator text for a journal whose last good phase is
/// `last` (§2.3, O2-1/O3-1).
pub fn journal_corrupt(path: &Path, last: Option<Phase>) -> ImportError {
    let dest = path.parent().and_then(Path::parent).unwrap_or(Path::new("."));
    let d = dest.display();
    let p = path.display();
    ImportError::JournalCorrupt(if last.is_some_and(|l| l >= Phase::Placed) {
        format!(
            "the import journal at {p} is corrupt; the destination holds only this unfinished import: remove {d}/blocks, the rdashd store and {d}/{STAGING}, then restart"
        )
    } else {
        format!(
            "the import journal at {p} is corrupt; remove {d}/{STAGING} and every entry in {d}/blocks, then restart"
        )
    })
}

/// The source lock token; the caller holds it (§2.1).
pub trait SourceGuard {
    fn release(&self) -> Result<(), ImportError>;
}

/// The node's reindex request (CH5's `request_reindex`).
pub trait ReindexArm {
    fn request_reindex(&self) -> Result<(), ReindexError>;
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReindexError {
    NotEmpty,
    WipeInProgress,
    Other(String),
}

/// P4: appends `ReindexArmed` only when the request returns Ok.
pub fn arm_reindex(
    j: &mut Journal,
    arm: &dyn ReindexArm,
    fs: &dyn ImportFs,
) -> Result<Phase, ImportError> {
    if j.last() != Some(Phase::Placed) {
        return Err(ImportError::PhaseOrder {
            last: j.last(),
            got: Phase::ReindexArmed,
        });
    }
    match arm.request_reindex() {
        Ok(()) => {
            j.repair(fs)?;
            j.append(Record::new(RecordBody::ReindexArmed, fs), fs)?;
            Ok(Phase::ReindexArmed)
        }
        Err(ReindexError::NotEmpty) => Err(ImportError::ReindexNotEmpty),
        Err(ReindexError::WipeInProgress) => Err(ImportError::ReindexWipeInProgress),
        Err(ReindexError::Other(e)) => Err(ImportError::ReindexFailed(e)),
    }
}

fn present(path: &Path, fs: &dyn ImportFs) -> Result<bool, ImportError> {
    match fs.metadata(path)? {
        Some(m) if m.is_symlink => Err(ImportError::DestinationSymlink(path.to_owned())),
        Some(m) => Ok(m.is_dir && !list_dir(path, fs)?.is_empty()),
        None => Ok(false),
    }
}

/// Refuses a destination where any path the import writes, removes or
/// renames is a symlink or the wrong kind, so no write or clear can follow
/// one into the source (R1). Only reads.
pub fn check_destination_layout(dest: &Path, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let staging = dest.join(STAGING);
    for (path, dir) in [
        (staging.clone(), true),
        (staging.join("blocks"), true),
        (staging.join(JOURNAL), false),
        (dest.join("blocks"), true),
        (dest.join("import-report.json"), false),
        (dest.join("import-report.json.tmp"), false),
    ] {
        if let Some(m) = fs.metadata(&path)?
            && (m.is_symlink || m.is_dir != dir || (!dir && m.nlink != 1))
        {
            return Err(ImportError::DestinationSymlink(path));
        }
    }
    Ok(())
}

/// The canonical `-importcoredatadir`: the network dir under `datadir`
/// (`subdir`, as in [`source_net_dir`]) must be the source itself, matched by
/// canonical path plus `(st_dev, st_ino)` as [`import_start`] matches it.
fn canonical_datadir(
    datadir: &Path,
    subdir: Option<&str>,
    source: &SourceBinding,
    fs: &dyn ImportFs,
) -> Result<PathBuf, ImportError> {
    let net = source_net_dir(datadir, subdir, fs)?;
    let holds = match source_binding(&net, fs) {
        Ok(b) => b == *source,
        Err(ImportError::Io(e)) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    if !holds {
        return Err(ImportError::InvalidOptions(format!(
            "-importcoredatadir={} does not hold the source network dir {}",
            datadir.display(),
            source.path.display()
        )));
    }
    Ok(fs.canonicalize(datadir)?)
}

/// The source's canonical path and `(st_dev, st_ino)`.
pub fn source_binding(net_dir: &Path, fs: &dyn ImportFs) -> Result<SourceBinding, ImportError> {
    let path = fs.canonicalize(net_dir)?;
    let m = fs
        .metadata(&path)?
        .filter(|m| m.is_dir)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "source dir"))?;
    Ok(SourceBinding {
        path,
        dev: m.dev,
        ino: m.ino,
    })
}

/// Canonicalises the longest existing prefix of `path` and appends the rest.
fn canonical_existing(path: &Path, fs: &dyn ImportFs) -> Result<PathBuf, ImportError> {
    let mut missing = Vec::new();
    let mut cur = path;
    loop {
        if fs.metadata(cur)?.is_some() {
            let mut out = fs.canonicalize(cur)?;
            out.extend(missing.iter().rev());
            return Ok(out);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                cur = parent;
            }
            _ => return Ok(path.to_owned()),
        }
    }
}

/// `(st_dev, st_ino)` of a canonical path and each of its existing ancestors.
fn ancestor_ids(path: &Path, fs: &dyn ImportFs) -> Result<BTreeSet<(u64, u64)>, ImportError> {
    let mut out = BTreeSet::new();
    for p in path.ancestors() {
        if let Some(m) = fs.metadata(p)? {
            out.insert(m.id());
        }
    }
    Ok(out)
}

/// The destination subtrees the import writes, besides the root itself.
const DEST_SCOPED: [&str; 2] = ["blocks", STAGING];

/// Refuses a source and destination that are equal or nested either way, by
/// canonical path, by the `(st_dev, st_ino)` of every ancestor and every dir
/// in both trees, and by the mount table, so neither a symlink nor a bind
/// mount can hide it (§2.1, R7). Only reads.
///
/// - The mount table ([`ImportFs::mount_table`]) is read on every call. Each
///   canonical path is located in it by its mount id
///   ([`ImportFs::mount_id`]), or by the longest mount-point prefix where the
///   kernel gives no id. The two must not sit on one filesystem
///   (`major:minor`) with one's path inside it equal to or under the other's,
///   and no mount whose mount point is at or under one of them may have a
///   `(major:minor, root)` equal to, under or above the other's located path.
///   That catches a bind made from under a dir the walk cannot list, either
///   way round and at any depth. A table that cannot be read or parsed, or
///   that lacks either path's mount, refuses with
///   [`ImportError::MountTable`]; on a target without one (not Linux) this
///   check is skipped.
/// - The dir walk covers both whole trees. Inside the trees the import
///   touches (the source's [`SCOPED_DIRS`], the destination's `blocks/` and
///   `import-staging/`) a dir that cannot be listed, or a child that cannot be
///   stat'ed, refuses with [`ImportError::UnreadableDir`]. Outside them, only
///   `EACCES` or `EPERM` is skipped (the dir counts as having no children, the
///   child as absent); any other error refuses the same way.
pub fn check_exclusion(source: &Path, dest: &Path, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let s = canonical_existing(source, fs)?;
    let d = canonical_existing(dest, fs)?;
    let overlap = || ImportError::Overlap {
        source_dir: s.clone(),
        dest: d.clone(),
    };
    if s.starts_with(&d) || d.starts_with(&s) {
        return Err(overlap());
    }
    if mounts_nest(&s, &d, fs)? {
        return Err(overlap());
    }
    let s_ids = ancestor_ids(&s, fs)?;
    let d_ids = ancestor_ids(&d, fs)?;
    let s_root = fs.metadata(&s)?.map(|m| m.id());
    let d_root = fs.metadata(&d)?.map(|m| m.id());
    if s_root.is_some_and(|id| d_ids.contains(&id)) || d_root.is_some_and(|id| s_ids.contains(&id))
    {
        return Err(overlap());
    }
    // A bind mount of a dir inside the other tree hides behind its own
    // ancestors, so each tree's dirs are checked against the other's ids too.
    let s_tree = tree_dir_ids(&s, &SCOPED_DIRS, fs)?;
    let d_tree = tree_dir_ids(&d, &DEST_SCOPED, fs)?;
    if d_ids.iter().any(|id| s_tree.contains(id))
        || s_ids.iter().any(|id| d_tree.contains(id))
        || !s_tree.is_disjoint(&d_tree)
    {
        return Err(overlap());
    }
    Ok(())
}

/// Whether the mount table lets `a` and `b` reach one another
/// ([`mounts::nest`]); `false` with no mount table. Each path is located by
/// the mount id the kernel gives for it, or for its longest existing
/// ancestor, and by the longest mount-point prefix where the kernel gives
/// none.
fn mounts_nest(a: &Path, b: &Path, fs: &dyn ImportFs) -> Result<bool, ImportError> {
    let text = fs.mount_table().map_err(|e| ImportError::MountTable(e.to_string()))?;
    let Some(text) = text else {
        return Ok(false);
    };
    let table = mounts::parse(&text).map_err(ImportError::MountTable)?;
    let (ia, ib) = (mount_id(a, fs)?, mount_id(b, fs)?);
    mounts::nest(&table, (a, ia), (b, ib)).map_err(ImportError::MountTable)
}

/// [`ImportFs::mount_id`] of `path`, or of its longest existing ancestor.
fn mount_id(path: &Path, fs: &dyn ImportFs) -> Result<Option<u64>, ImportError> {
    for p in path.ancestors() {
        match fs.mount_id(p) {
            Ok(id) => return Ok(id),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ImportError::MountTable(format!(
                    "cannot read the mount id of {}: {e}",
                    p.display()
                )));
            }
        }
    }
    Err(ImportError::MountTable(format!("no existing ancestor of {}", path.display())))
}

/// Whether an error listing or stat'ing a dir outside the scoped trees is
/// skipped: only a permission denial is.
fn denied(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM))
}

/// `(st_dev, st_ino)` of every dir under `root`, `root` included, symlinks
/// not followed; the error rules are [`check_exclusion`]'s. The scoped
/// subtrees are walked first, so an alias of a scoped dir elsewhere in the
/// tree cannot claim its id and skip the strict rule.
fn tree_dir_ids(
    root: &Path,
    scoped: &[&str],
    fs: &dyn ImportFs,
) -> Result<BTreeSet<(u64, u64)>, ImportError> {
    let scoped: Vec<PathBuf> = scoped.iter().map(|d| root.join(d)).collect();
    let mut out = BTreeSet::new();
    let mut todo = vec![root.to_owned()];
    todo.extend(scoped.iter().cloned());
    while let Some(dir) = todo.pop() {
        let strict = scoped.iter().any(|s| dir.starts_with(s));
        let m = match fs.metadata(&dir) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) if !strict && denied(&e) => continue,
            Err(e) => return Err(unreadable(&dir, e)),
        };
        if !m.is_dir || !out.insert(m.id()) {
            continue;
        }
        match fs.read_dir(&dir) {
            Ok(children) => todo.extend(children),
            Err(e) if !strict && denied(&e) => {}
            Err(e) => return Err(unreadable(&dir, e)),
        }
    }
    Ok(out)
}

fn unreadable(dir: &Path, error: io::Error) -> ImportError {
    ImportError::UnreadableDir {
        path: dir.to_string_lossy().into_owned(),
        path_hex: report::hex(dir.as_os_str().as_bytes()),
        error,
    }
}

/// `read_dir` whose failure names the dir ([`ImportError::UnreadableDir`]).
fn list_dir(dir: &Path, fs: &dyn ImportFs) -> Result<Vec<PathBuf>, ImportError> {
    fs.read_dir(dir).map_err(|error| unreadable(dir, error))
}

/// The source network dir under a base `-datadir`: the base itself on
/// mainnet, else `<base>/<subdir>`. A `<dir>` that is itself a network subdir
/// and lacks the expected layout is refused with the hint (§2.1).
pub fn source_net_dir(
    base: &Path,
    subdir: Option<&str>,
    fs: &dyn ImportFs,
) -> Result<PathBuf, ImportError> {
    let net = subdir.map_or_else(|| base.to_owned(), |s| base.join(s));
    let looks_like_subdir = base
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|n| n == "testnet3" || n == "regtest" || n.starts_with("devnet-"));
    let has_layout = fs.metadata(&net.join("blocks"))?.is_some_and(|m| m.is_dir);
    if looks_like_subdir && !has_layout {
        return Err(ImportError::NetSubdir(base.to_owned()));
    }
    Ok(net)
}

/// The destination check before P0, before any lock (§2.1, O1-4). With no
/// journal the destination must hold no `blocks/` entry, no store (a
/// `store/` or `chainstate/` dir with an entry, or anything there that is not
/// a plain dir) and no `import-staging/`; a missing or empty dir is absent. A journal for the same source (canonical path plus
/// `(st_dev, st_ino)`) resumes, and only its row's predicates apply.
/// Only reads.
pub fn preflight_destination(
    dest: &Path,
    source: &Path,
    fs: &dyn ImportFs,
) -> Result<Option<Journal>, ImportError> {
    let binding = source_binding(source, fs)?;
    check_exclusion(&binding.path, dest, fs)?;
    check_destination_layout(dest, fs)?;
    let source_blocks = binding.path.join("blocks");
    if fs.metadata(&source_blocks.join("index"))?.is_some()
        && !fs
            .read_dir(&source_blocks)?
            .iter()
            .any(|p| p.file_name().and_then(OsStr::to_str).is_some_and(|n| n.starts_with("blk")))
    {
        return Err(ImportError::BlocksDirElsewhere);
    }
    let staging = dest.join(STAGING);
    let journal_path = staging.join(JOURNAL);
    let journal = match fs.metadata(&journal_path)? {
        Some(_) => Some(Journal::open(&journal_path, fs)?),
        None => None,
    };
    match journal.as_ref().and_then(Journal::intent) {
        Some(intent) if intent.source != binding => {
            return Err(ImportError::DestinationNotEmpty(staging));
        }
        Some(_) => {}
        None if journal.is_none() && present(&staging, fs)? => {
            return Err(ImportError::DestinationNotEmpty(staging));
        }
        None => {}
    }
    let last = journal.as_ref().and_then(Journal::last);
    let blocks = dest.join("blocks");
    if last.is_none_or(|l| l < Phase::SourceVerified) && present(&blocks, fs)? {
        return Err(ImportError::DestinationNotEmpty(blocks));
    }
    if last.is_none_or(|l| l < Phase::Placed) {
        for store in STORE_DIRS.map(|s| dest.join(s)) {
            let plain_dir = fs.metadata(&store)?.is_none_or(|m| m.is_dir && !m.is_symlink);
            if !plain_dir || present(&store, fs)? {
                return Err(ImportError::DestinationNotEmpty(store));
            }
        }
    }
    Ok(journal)
}

/// The start rule (OD-CP-296 item 5): a journal below `ReindexArmed` refuses
/// unless the same `-importcoredatadir` is given; from `ReindexArmed` on, P5
/// runs either way. `requested` is the `-importcoredatadir` value, the base
/// dir; the network dir under it is `subdir` (none on mainnet), as in
/// [`source_net_dir`]. Returns the journal's last phase.
pub fn import_start(
    journal: &Journal,
    requested: Option<&Path>,
    subdir: Option<&str>,
    fs: &dyn ImportFs,
) -> Result<Option<Phase>, ImportError> {
    let last = journal.last();
    let (Some(intent), true) = (journal.intent(), last.is_some_and(|l| l < Phase::ReindexArmed))
    else {
        return Ok(last);
    };
    let matches = match requested {
        Some(r) => source_net_dir(r, subdir, fs)
            .and_then(|net| source_binding(&net, fs))
            .is_ok_and(|b| b == intent.source),
        None => false,
    };
    if !matches {
        return Err(ImportError::ImportInProgress(intent.datadir.clone()));
    }
    Ok(last)
}

/// Re-hashes every hardlinked source file against M0 (§2.1, S3-1).
fn recheck_links(intent: &Intent, methods: &[FileMethod], fs: &dyn ImportFs) -> Vec<PathBuf> {
    let mut bad = Vec::new();
    for m in methods.iter().filter(|m| m.method == Method::Hardlink) {
        let rel = Path::new("blocks").join(&m.name);
        let p = intent.source.path.join(&rel);
        let same = intent.m0.get(&rel).is_some_and(|e| {
            fs.metadata(&p).ok().flatten().is_some_and(|now| {
                now.len == e.size && now.modified == e.modified && now.id() == (e.dev, e.ino)
            }) && fs.sha256(&p).is_ok_and(|h| h == e.hash)
        });
        if !same {
            bad.push(rel);
        }
    }
    bad
}

/// Checks the staged files are exactly the method table's, each equal to M0.
fn verify_staged(
    staged: &Path,
    intent: &Intent,
    methods: &[FileMethod],
    fs: &dyn ImportFs,
) -> Result<Vec<PathBuf>, ImportError> {
    let mut bad = Vec::new();
    let expected: BTreeSet<&OsStr> = methods.iter().map(|m| m.name.as_os_str()).collect();
    for p in fs.read_dir(staged)? {
        if !p.file_name().is_some_and(|n| expected.contains(n)) {
            bad.push(p);
        }
    }
    for m in methods {
        let rel = Path::new("blocks").join(&m.name);
        let p = staged.join(&m.name);
        let ok = match (intent.m0.get(&rel), fs.metadata(&p)?) {
            (Some(e), Some(now)) => now.len == e.size && fs.sha256(&p)? == e.hash,
            _ => false,
        };
        if !ok {
            bad.push(rel);
        }
    }
    Ok(bad)
}

/// Removes every `import-staging` entry but the journal and syncs the dir.
fn clear_staging(dest: &Path, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let staging = dest.join(STAGING);
    for entry in fs.read_dir(&staging)? {
        if entry.file_name() != Some(OsStr::new(JOURNAL)) {
            fs.remove(&entry)?;
        }
    }
    fs.sync_dir(&staging)?;
    Ok(())
}

/// The O3-2 reset: the journal is truncated to the magic and durable before
/// any clear of staging is visible. At `SourceVerified` the rename may have
/// landed; it is first undone durably, so a crash before the reset re-enters
/// the `SourceVerified` + staging row and resets again.
fn reset_after_change(
    j: &mut Journal,
    dest: &Path,
    options: ImportOptions,
    fs: &dyn ImportFs,
    guard: &dyn SourceGuard,
) -> Result<(), ImportError> {
    let staging = dest.join(STAGING);
    let staged = staging.join("blocks");
    let blocks = dest.join("blocks");
    if j.last() == Some(Phase::SourceVerified) && !present(&staged, fs)? && present(&blocks, fs)? {
        fs.rename(&blocks, &staged)?;
        fs.sync_dir(&staging)?;
        fs.sync_dir(dest)?;
    }
    j.repair(fs)?;
    j.reset(fs)?;
    clear_staging(dest, fs)?;
    if options.placement != PlacementPolicy::Hardlink {
        guard.release()?;
    }
    Ok(())
}

/// P0 and the none/`Intent` row: clear staging, truncate the journal, scan
/// M0, append `Intent`.
fn begin_intent(
    j: &mut Journal,
    src: &SourceInspection,
    options: ImportOptions,
    source: &SourceBinding,
    datadir: PathBuf,
    dest: &Path,
    fs: &dyn ImportFs,
) -> Result<(), ImportError> {
    if !j.exists() {
        *j = Journal::create(&j.path, fs)?;
    }
    clear_staging(dest, fs)?;
    if !j.records().is_empty() {
        j.reset(fs)?;
    }
    let m0 = SourceManifest::scan(&source.path, fs)?;
    let intent = Intent {
        source: source.clone(),
        datadir,
        mode: options.mode,
        placement: options.placement,
        best_block: src.best_block,
        last_file: src.last_file,
        m0,
    };
    j.append(Record::new(RecordBody::Intent(Box::new(intent)), fs), fs)
}

/// P2: M1 against M0 with the exact link delta; on a change, the reset.
fn verify_source(
    j: &mut Journal,
    options: ImportOptions,
    dest: &Path,
    fs: &dyn ImportFs,
    guard: &dyn SourceGuard,
) -> Result<(), ImportError> {
    let intent = j.intent().expect("Staged follows Intent").clone();
    let linked: BTreeSet<PathBuf> = j
        .methods()
        .unwrap_or_default()
        .iter()
        .filter(|m| m.method == Method::Hardlink)
        .map(|m| Path::new("blocks").join(&m.name))
        .collect();
    let m1 = SourceManifest::scan(&intent.source.path, fs)?;
    if let Err(paths) = intent.m0.check_placement(&m1, &linked) {
        reset_after_change(j, dest, options, fs, guard)?;
        return Err(ImportError::SourceChanged(paths));
    }
    j.append(
        Record::new(
            RecordBody::SourceVerified {
                m1_digest: m1.digest,
            },
            fs,
        ),
        fs,
    )?;
    if options.placement != PlacementPolicy::Hardlink {
        guard.release()?;
    }
    Ok(())
}

/// Runs P0-P3 and the state-based recovery of §2.3, returning the journal's
/// last phase. `datadir` is the `-importcoredatadir` value, the base dir;
/// `src.net_dir` must be the network dir under it (`subdir`, none on mainnet),
/// as in [`source_net_dir`]. The caller holds the source lock as `guard`. In
/// snapshot and reflink modes `stage` releases it once `SourceVerified` is
/// durable; in hardlink mode never. On a resume the caller must re-take the
/// lock whenever the journal is below `SourceVerified`; at `SourceVerified` or
/// later it is needed only in hardlink mode, and otherwise `guard` may be a
/// no-op whose `release` succeeds (the operator may have restarted dashd on
/// the source).
pub fn stage(
    src: &SourceInspection,
    options: ImportOptions,
    datadir: &Path,
    subdir: Option<&str>,
    dest: &Path,
    fs: &dyn ImportFs,
    guard: &dyn SourceGuard,
) -> Result<Phase, ImportError> {
    options.validate()?;
    let source = source_binding(&src.net_dir, fs)?;
    let datadir = canonical_datadir(datadir, subdir, &source, fs)?;
    check_exclusion(&source.path, dest, fs)?;
    check_destination_layout(dest, fs)?;
    let staging = dest.join(STAGING);
    let staged = staging.join("blocks");
    let blocks = dest.join("blocks");
    let mut j = Journal::open(&staging.join(JOURNAL), fs)?;
    if let Some(intent) = j.intent() {
        if intent.source != source {
            return Err(ImportError::DestinationNotEmpty(staging));
        }
        // A finished import answers "already imported" whatever the options,
        // and with no re-check (OD-CP-303): a later source -reindex may change
        // hardlinked files.
        if j.last() == Some(Phase::Reported) {
            return Err(report::reported_refusal(&j));
        }
        if intent.mode != options.mode || intent.placement != options.placement {
            return Err(ImportError::OptionsDiffer {
                mode: intent.mode.as_str(),
                placement: intent.placement.as_str(),
            });
        }
    }
    let resumed_past_verified = j.last().is_some_and(|l| l >= Phase::SourceVerified);
    if j.last() == Some(Phase::SourceVerified) && present(&staged, fs)? == present(&blocks, fs)? {
        return Err(journal_corrupt(&j.path, j.last()));
    }
    if resumed_past_verified && options.placement == PlacementPolicy::Hardlink {
        let intent = j.intent().expect("a later phase follows Intent");
        let bad = recheck_links(intent, j.methods().unwrap_or_default(), fs);
        if !bad.is_empty() {
            if j.last() == Some(Phase::SourceVerified) {
                reset_after_change(&mut j, dest, options, fs, guard)?;
                return Err(ImportError::SourceChanged(bad));
            }
            return Err(journal_corrupt(&j.path, j.last()));
        }
    }
    if j.last() == Some(Phase::Placed) && !present(&blocks, fs)? {
        return Err(journal_corrupt(&j.path, j.last()));
    }
    if j.last() == Some(Phase::Staged) && !present(&staged, fs)? {
        return Err(journal_corrupt(&j.path, j.last()));
    }
    if resumed_past_verified && options.placement != PlacementPolicy::Hardlink {
        guard.release()?;
    }
    j.repair(fs)?;
    if j.last().is_none_or(|l| l == Phase::Intent) {
        begin_intent(&mut j, src, options, &source, datadir, dest, fs)?;
        let intent = j.intent().expect("just appended").clone();
        let placement = place(&src.files, intent.last_file, options, &intent.m0, dest, fs)?;
        fs.sync_dir(&staged)?;
        fs.sync_dir(&staging)?;
        j.append(Record::new(RecordBody::Staged(placement.methods), fs), fs)?;
        verify_source(&mut j, options, dest, fs, guard)?;
    } else if j.last() == Some(Phase::Staged) {
        let intent = j.intent().expect("Staged follows Intent").clone();
        let bad = verify_staged(&staged, &intent, j.methods().unwrap_or_default(), fs)?;
        if !bad.is_empty() {
            reset_after_change(&mut j, dest, options, fs, guard)?;
            return Err(ImportError::StagingChanged(bad));
        }
        verify_source(&mut j, options, dest, fs, guard)?;
    }
    if j.last() == Some(Phase::SourceVerified) {
        match (present(&staged, fs)?, present(&blocks, fs)?) {
            (true, false) => {
                fs.rename(&staged, &blocks).map_err(|e| {
                    if e.raw_os_error() == Some(libc::ENOTEMPTY) {
                        ImportError::DestinationNotEmpty(blocks.clone())
                    } else {
                        ImportError::Io(e)
                    }
                })?;
                fs.sync_dir(&staging)?;
                fs.sync_dir(dest)?;
            }
            (false, true) => {
                fs.sync_dir(&staging)?;
                fs.sync_dir(dest)?;
            }
            _ => return Err(journal_corrupt(&j.path, j.last())),
        }
        j.append(Record::new(RecordBody::Placed, fs), fs)?;
    }
    Ok(j.last().expect("stage ends at Placed or later"))
}

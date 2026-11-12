//! VerifyDB's window in a node's debug log, shared by the differential tests that compare
//! rdashd's with dashd's: the lines it keeps, and the named exclusions of Core lines rdashd does
//! not port (CH5-1b, DEC-1610).

use super::{RpcNode, STARTUP_SCHEME, debug_log, read};

/// A log line's text: its time cut, and the ` (mocktime: N)` beside it (`logging.cpp:401-404`).
pub fn unstamped(line: &str) -> &str {
    let text = line.split_once(' ').map_or(line, |(_, text)| text);
    text.strip_prefix("(mocktime: ").and_then(|t| t.split_once(") ")).map_or(text, |(_, t)| t)
}

/// The threads either node starts before its startup VerifyDB: a late start line lands in that
/// check's window, which `Side::start`'s callers read and no barrier can guard. VerifyDB and the
/// disconnects and connects of its walk start no thread and log no `<name> thread start`, so
/// such a line in its window is never its own.
// Core: init.cpp v24.0.0:1566 (scheduler), :2088 LoadChainstate (node/chainstate.cpp:159,
// llmq/quorumsman.cpp:46: q-cache) before :2096 VerifyLoadedChainstate; validation.cpp v24.0.0:5066-5180
// rdashd: dashd-node/src/node.rs:240-241 (scheduler), steps.rs:526;
// dashd-chainstate/src/chain/verify.rs:257-555
pub const STARTED_BEFORE_VERIFYDB: &[&str] = &["scheduler", "q-cache"];

pub fn started_before_verifydb(line: &str) -> bool {
    line.strip_suffix(" thread start").is_some_and(|name| STARTED_BEFORE_VERIFYDB.contains(&name))
}

/// VerifyDB's scheme guard's revert line (`validation.cpp:166` at v24.0.0), before its value.
pub const REVERTED: &str = "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=";

/// VerifyDB's lines in a stretch of debug log, their times cut: from its first line, or init's
/// [`STARTUP_SCHEME`] line before a startup's, through its last and the scheme guard's revert
/// lines right after it, without the bench lines either node may log around a connect or the
/// start lines of [`STARTED_BEFORE_VERIFYDB`].
// Core: validation.cpp:162-167, :5056 (the guard's revert as VerifyDB returns) at v24.0.0
pub fn verifydb_lines(log: &str) -> Vec<String> {
    let lines: Vec<String> = log
        .lines()
        .map(|l| unstamped(l).to_owned())
        .filter(|l| !l.starts_with("[bench]") && !started_before_verifydb(l))
        .collect();
    let first = lines
        .iter()
        .position(|l| l == STARTUP_SCHEME)
        .or_else(|| lines.iter().position(|l| l.starts_with("Verifying last")));
    let last = lines.iter().rposition(|l| {
        l.starts_with("Verification: No coin database")
            || l.contains("Verification error:")
            || l.starts_with("Skipped verification of level >=3")
    });
    match (first, last) {
        (Some(first), Some(last)) => {
            let reverts = lines[last + 1..].iter().take_while(|l| l.starts_with(REVERTED)).count();
            lines[first..=last + reverts].to_vec()
        }
        _ => Vec::new(),
    }
}

/// Core's `GetListForBlockInternal` initial-snapshot line (`evo/deterministicmns.cpp:886-887` at
/// v24.0.0, L4): never ported (OD-CP-274, DEC-1610). Where it falls depends on the process's list
/// cache and another thread's timing, and Core's `mnListsCache` has no rdashd counterpart, so it
/// is dropped from both nodes' windows by its text, with no positive check.
pub fn unstable(line: &str) -> bool {
    line.starts_with("CDeterministicMNManager::GetListForBlockInternal -- initial snapshot. ")
}

/// A Core line inside a VerifyDB window that rdashd does not port: a declared difference
/// (DEC-1610), excluded by its fixed text, with CP's ruling and the BL that owes the port. Each
/// is deterministic, so each carries R3-3's check ([`Exclusions::assert_matched`]).
pub struct Excluded {
    pub line: fn(&str) -> bool,
    /// The lines whose Core text ends in a newline of its own, so an empty line follows each,
    /// dropped with it.
    pub blank_after: fn(&str) -> bool,
    pub ruling: &'static str,
    pub owed_by: &'static str,
}

/// L3, `CDeterministicMNManager::ProcessBlock -- Wrote snapshot. nHeight=…,
/// mapCurMNs.allMNsCount=…` (`evo/deterministicmns.cpp:722-731` at v24.0.0): not ported. rdashd
/// always snapshots the first DIP0003 block (DEC-11), where Core's level 4 logs it over the
/// initial snapshot's child.
pub const WROTE_SNAPSHOT: Excluded = Excluded {
    line: wrote_snapshot,
    blank_after: |_| false,
    ruling: "OD-CP-275, DEC-1610",
    owed_by: "BL-873",
};

pub fn wrote_snapshot(line: &str) -> bool {
    line.starts_with("CDeterministicMNManager::ProcessBlock -- Wrote snapshot. nHeight=")
        && line.contains(", mapCurMNs.allMNsCount=")
}

/// L8, `CSpecialTxProcessor::ApplyTxToList`'s `LogPrintf` lines, `ApplyTxToList -- MN <hash> …`
/// (`evo/specialtxman.cpp:395`, `:450`, `:500`, `:518`, `:583`, `:615`, `:645`, `:698`, `:760` at
/// v24.0.0), and the empty line after those that end in a `CProRegTx` or `CProUpServTx`, whose
/// `ToString` ends in a newline of its own (`evo/providertx.cpp:448`, `:491`): the list build's
/// lines, which rdashd's `DmlLines` does not log on any path (EV2b D19). Level 4 logs them on
/// each reconnect of a block with a ProTx.
pub const APPLY_TX_TO_LIST: Excluded = Excluded {
    line: apply_tx_to_list,
    blank_after: |line| line.contains(": CProRegTx(") || line.contains(": CProUpServTx("),
    ruling: "OD-CP-275, DEC-1610",
    owed_by: "BL-873",
};

pub fn apply_tx_to_list(line: &str) -> bool {
    line.starts_with("ApplyTxToList -- MN ")
}

/// L5, `UndoBlock: exclude mnhf bit=<b> block:<hash> number of known signals:<n>`
/// (`evo/mnhftx.cpp:278` at v24.0.0): a walk's undo of an MNHF signal's block.
pub const MNHF_EXCLUDED: Excluded = Excluded {
    line: |l| l.starts_with("UndoBlock: exclude mnhf bit=") && l.contains(" number of known "),
    blank_after: |_| false,
    ruling: "OD-CP-275, DEC-1610",
    owed_by: "BL-873",
};

/// L6, `CMNHFManager::ProcessBlock: add mnhf bit=<b> block:<hash> number of known signals:<n>`
/// (`evo/mnhftx.cpp:231` at v24.0.0): level 4's reconnect of an MNHF signal's block.
pub const MNHF_ADDED: Excluded = Excluded {
    line: |l| l.starts_with("CMNHFManager::ProcessBlock: add mnhf bit=") && l.contains(" block:"),
    blank_after: |_| false,
    ruling: "OD-CP-275, DEC-1610",
    owed_by: "BL-873",
};

/// L7, `IsValidMNActivation: set MnEHF for bit=<b> is valid` (`chainparams.cpp:124` at v24.0.0):
/// twice per reconnect of an MNHF signal's block, from `CheckMNHFTx` and from
/// `CMNHFManager::ProcessBlock`.
pub const MNHF_VALID: Excluded = Excluded {
    line: |l| l.starts_with("IsValidMNActivation: set MnEHF for bit=") && l.ends_with(" is valid"),
    blank_after: |_| false,
    ruling: "OD-CP-275, DEC-1610",
    owed_by: "BL-873",
};

/// A test's exclusions, filtered per cell, each one's dashd matches counted over the whole test.
pub struct Exclusions(Vec<(&'static Excluded, usize)>);

impl Exclusions {
    pub fn new(named: &[&'static Excluded]) -> Exclusions {
        Exclusions(named.iter().map(|e| (*e, 0)).collect())
    }

    /// One cell's windows: [`unstable`] lines dropped from both; each named exclusion's lines
    /// dropped from dashd's and counted, and never on rdashd's.
    pub fn cell(&mut self, cell: &str, ours: &[String], core: &[String]) -> [Vec<String>; 2] {
        let ours: Vec<String> = ours.iter().filter(|l| !unstable(l)).cloned().collect();
        let mut core: Vec<String> = core.iter().filter(|l| !unstable(l)).cloned().collect();
        for (e, matched) in &mut self.0 {
            let stale = ours.iter().find(|l| (e.line)(l));
            assert!(stale.is_none(), "{cell}: rdashd logs {stale:?}, excluded ({})", e.ruling);
            let mut kept = Vec::with_capacity(core.len());
            let mut lines = core.into_iter();
            while let Some(l) = lines.next() {
                if !(e.line)(&l) {
                    kept.push(l);
                    continue;
                }
                *matched += 1;
                if (e.blank_after)(&l) {
                    assert_eq!(lines.next().as_deref(), Some(""), "{cell}: after {l}");
                }
            }
            core = kept;
        }
        [ours, core]
    }

    /// R3-3: every named exclusion matched a dashd line at least once in the test.
    pub fn assert_matched(&self) {
        for (e, matched) in &self.0 {
            assert!(*matched > 0, "an exclusion ({}, {}) matched nothing", e.ruling, e.owed_by);
        }
    }
}

/// `verifychain level depth` on `node`: the answer, and VerifyDB's window in its log.
pub fn verifychain_on(node: &RpcNode, level: i32, depth: i32) -> (String, Vec<String>) {
    let before = read(&debug_log(&node.datadir)).len();
    let answer = node.call("verifychain", &format!("[{level}, {depth}]")).result().to_owned();
    (answer, verifydb_lines(&read(&debug_log(&node.datadir))[before..]))
}

/// The node's tip and `gettxoutsetinfo`'s `hash_serialized_2`, `txouts` and `total_amount`
/// (CH5-1b, the pool observers' kind 1).
pub fn chain_state(node: &RpcNode) -> (String, [String; 3]) {
    let info = node.call("gettxoutsetinfo", "[]").result().to_owned();
    let info: serde_json::Value = serde_json::from_str(&info).expect("gettxoutsetinfo");
    let utxos = ["hash_serialized_2", "txouts", "total_amount"].map(|f| info[f].to_string());
    (node.call("getbestblockhash", "[]").result().to_owned(), utxos)
}

/// CH5-1b's walks over evo blocks (Legs B and C): `verifychain` over `cells` on rdashd and
/// dashd (`nodes`, in that order) at `at`, true on both, with equal windows but for
/// `exclusions` and Core's summary line. After each depth-0 walk, each node's tip and UTXO set as
/// before the walks, and equal on both.
pub fn walks(nodes: [&RpcNode; 2], at: &str, cells: &[(i32, i32)], exclusions: &mut Exclusions) {
    let before = nodes.map(chain_state);
    assert_eq!(before[0], before[1], "{at}: the tip and UTXO set");
    for &(level, depth) in cells {
        let cell = format!("{at}: verifychain {level} {depth}");
        let [ours, core] = nodes.map(|n| verifychain_on(n, level, depth));
        assert_eq!((ours.0.as_str(), core.0.as_str()), ("true", "true"), "{cell}: {:#?}", ours.1);
        let [ours, core] = exclusions.cell(&cell, &ours.1, &core.1);
        assert_eq!(ours, core, "{cell}");
        let summary = "Verification: No coin database inconsistencies in last ";
        assert!(ours.iter().any(|l| l.starts_with(summary)), "{cell}: {ours:#?}");
        if depth == 0 {
            assert_eq!(nodes.map(chain_state), before, "{cell}: the tip and UTXO set");
        }
    }
}

/// The plan's cells over evo blocks (CH5-1b §4, Legs B and C): the default window and the whole
/// chain at levels 3 and 4.
pub const EVO_CELLS: [(i32, i32); 4] = [(3, 6), (4, 6), (3, 0), (4, 0)];

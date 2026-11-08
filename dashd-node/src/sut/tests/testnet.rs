//! CH7b-2 C16 (PLAN §5.3, T-TN): testnet connects through its first non-null commitments.
//!
//! `CH7B2_TESTNET_WINDOW` names a directory whose `blocks/` holds a testnet datadir's block files
//! (a reflink copy of O8's stopped node's). The node connects genesis to [`H_END`] under
//! `-assumevalid=0`, so every block runs its script checks and with them the CbTx's root checks
//! (`fCheckCbTxMerkleRoots`, v24.0.0-rc.4 `validation.cpp:2669-2671`): step 8's MN-list root at
//! exactly the heights whose coinbase is a CbTx (`evo/specialtxman.cpp:984-996`), step 9's quorum
//! root at exactly those whose CbTx is version 2 or more (`:1003-1011`; none below DIP0008's
//! 78,800), and none skipped. The first non-null commitment, an LLMQ_50_60 one mined at 7,522
//! (DIP0003 at 7,000), connects with its members, and two of its DKG intervals past it. A CbTx's
//! roots commit to the MN list and the mined quorums Core built, so each connect is a
//! differential against the chain Core accepted. Past the first rotated cycle (base 769,824), the
//! heights from the first block that mines a rotated commitment are step 9 over rotated rows.
//!
//! Without the variable the test says it skipped. `CH7B2_TESTNET_TO=<height>` runs it further:
//! CH7b-2's one-off run to 905,099, the last block before V20's CbTx ChainLocks, and R-A's to
//! [`END`] (below).
//!
//! CH7b-3a-2 C9a (CH7b-3 PLAN C9, R-A's census half; OD-CP-55): from V20 (905,100) every CbTx is
//! version 3 or later, so step 10 runs at each. The node holds no best ChainLock, so at each such
//! CbTx it must ask for one once and get Core's null lock, take neither shortcut, and verify each
//! non-null `bestCLSignature` (`evo/specialtxman.cpp:139-215`). Its census, the checks its
//! verifier made in order, must equal the sequence decoded from the CbTxs apart from the node
//! ([`StepTen`], a [`WindowObserver`]), each check valid. The first CbTx ChainLock and, to
//! [`END`], the counts are pinned (an independent decode of the block files gave them).
//! - A run that reaches V20 needs `CH7B3_TESTNET_GOLDEN`, the directory of SW-V24's cut of its
//!   rc.5 testnet recording over 0..[`END`] (DEC-1061): before the connect, the cut's sha256s
//!   (the file's and its decompressed stream's), `provenance.json`'s Core describe and commit and
//!   network, its `H` (testnet) and its `C`s are checked: one per height, the last [`END`], and
//!   each to the run's tip the located block. So the located chain is the recorded one.
//! - The whole recording's sha256, `FULL-GOLDEN.sha256` from the extractor's re-cut, is pinned
//!   too, and so is `provenance.json`'s copy of it.
//! - The run goes through the owner-only runner of the CH7b-3 plan (§5.4), never a shared
//!   console.
//!
//! CH7b-3a-2 C9b (PLAN C9, §6.3, the payments half; DEC-1146): `CH7B3_TESTNET_PAYMENTS` names
//! SW-V24's `masternode payments` fixtures, cut from a replica of run2's live datadir, each
//! `masternode payments <getblockhash(first)> 20` on its oracle.
//! - Before the connect the set's pins are checked: `SHA256SUMS` is the pinned one, every file
//!   it lists is there with its sha256 and none is unlisted, and the fixtures are the plan's
//!   calendar and witness windows. Then each fixture's oracle (Core describe, version and binary
//!   sha256), its exact request (the located block at `first`) and its heights, one entry per
//!   block of `first..=last`.
//! - Each block of each fixture the run reaches is rendered once it has connected
//!   (dashd-chainstate's `masternode_payments`: the fees its undo gives, the payees on its
//!   parent's list), then compared with the fixture's entry field by field, each address
//!   `EncodeDestination` of its script ([`Payments`]).
//! - Each witness's block, its fixture's first, must have the property it was picked for,
//!   checked on the node's side alone ([`PAYMENTS_WITNESSES`], [`witness_holds`]).
//! - The values go only to `CH7B3_OWNER_REPORT`, keyed to the fixtures' `SHA256SUMS` and
//!   `provenance.json` and to the replica (DEC-1146). A re-cut set (run2 wins) reruns with the
//!   same command, its `SHA256SUMS`'s sha256 in `CH7B3_TESTNET_PAYMENTS_SUMS`.
//! - Without the fixtures the test says it skipped C9b; a run to [`END`] needs them.
//!
//! CH7a-5b C4, the state-hash mode: with `CH7A5B_TESTNET_GOLDEN=<slice dir>` and
//! `CH7A5B_OWNER_REPORT=<dir>` (both or neither), the node writes a state-hash log into the owner
//! directory from genesis, `muhash` at every block, and after the connect to [`H_END`] that log
//! must equal SW-V24's slice of its rc.5 testnet recording over 0..7,570 on every key
//! ([`linear_clean`]). The evo keys are null below DIP0003 (7,000); from there `mnl` and `qc` are
//! set, with the first non-null commitment's row at 7,522, and `cp` and `ehf` stay null (no V20).
//! - The slice is checked before the connect (DEC-1061): its sha256, compressed and as its
//!   decompressed stream, `provenance.json`'s Core describe and commit (the rc.5 recording's,
//!   not CH3a D146's), the full golden's sha256 (`FULL-GOLDEN.sha256` and provenance's
//!   `full_golden_sha256`, OD-CP-53), `H.chain` = testnet, and its last `C` at [`H_END`]. Each
//!   golden `C`'s block and header keys are the located block's (CH3a D146).
//! - A failure message carries heights and key names only; the values go to the owner report,
//!   `owner-report.txt` (mode 0600) in `CH7A5B_OWNER_REPORT`. The run goes through the
//!   owner-only runner of CH7a-5b's plan (§5), never a shared console.
//!
//! The span is 0..7,570 only: the compare refuses any `CH7B2_TESTNET_TO` but 7,570. Without the
//! two variables the test says it ran without the compare.
//!
//! CH7b-3b C14 (CH7b-3 PLAN §3.4, R-B), the state-hash mode over the whole span: with
//! `CH7B3_STATE_HASH_DIR=<owner-only dir>` on R-A's set, to [`END`], the node writes a state-hash
//! log into that directory from genesis ([`StateHash`]), `muhash` at [`MUHASH_AT`]'s 24 heights
//! only, and after the connect, the census and the payments, that log must equal the golden cut
//! over 0..=1,574,404 ([`linear_clean_noting`], tied to the cut's plain-stream pin):
//! - the 21 keys but `muhash` at every height, exactly 1,574,405 pairs; `muhash` equal at the 24
//!   heights and absent on the node's side elsewhere;
//! - each of [`WINDOWS`] complete, with `muhash` at its activation height, and `cp` and `ehf` set
//!   at every height from [`V20`] ([`WindowScan`]);
//! - the cut's pins as above, its `SHA256SUMS` the pinned sums and `provenance.json`'s
//!   `slice.last` [`END`].
//!
//! The directory must be one, with no group or other permission bits and no entry yet at
//! either of C14's names, a dangling link included. Before any input is read, the log and the
//! owner report (`state-hash-report.txt`) are created there fresh at mode 0600 ([`fresh`]). From
//! then on the run's diagnostics are value-free ([`Diag`]): stderr gets heights, counts and
//! failure classes, and the owner report alone gets the values, the tip's hash, a failed
//! check's hashes or validation state, the node's `AbortNode` lines and every panic's message
//! ([`owner_panics`]) among them, with the compare's report. R-A's and C4's runs print as they
//! did. The end compare's RSS before it, its time and its own peak RSS go to the run's log,
//! count-only and never failing the run.
//!
//! Which variables go together, checked before any block is located ([`refused_mix`]; each mode's
//! own guards stay too):
//! - `CH7B2_TESTNET_WINDOW` alone is the plain T-TN; without it and without a mode variable
//!   the test skips, whatever `CH7B2_TESTNET_TO` says. Each mode variable needs the window: set
//!   without it, the run is refused rather than skipped.
//! - CH7a-5b C4's two, `CH7A5B_TESTNET_GOLDEN` and `CH7A5B_OWNER_REPORT`, go both or neither,
//!   and only to [`H_END`]: with them, `CH7B2_TESTNET_TO` is unset or 7,570, and any other
//!   height, a run past 7,570 among them, is refused before the slice is read.
//! - CH7b-3a-2's: `CH7B3_TESTNET_GOLDEN` is optional below [`V20`] and needed from it;
//!   `CH7B3_TESTNET_PAYMENTS` needs `CH7B3_OWNER_REPORT`, takes `CH7B3_TESTNET_PAYMENTS_SUMS`,
//!   and is needed at [`END`]. The report and the sums without the fixtures are refused, as
//!   nothing would read them.
//! - CH7b-3b C14's `CH7B3_STATE_HASH_DIR` runs to [`END`] only, so with R-A's set: a run with it
//!   to any other height is refused.
//! - The two families refuse each other: one run is one mode. Each is a different plan's
//!   owner-only run, whose runner sets that family alone and keeps that family's owner report,
//!   and C4's span ends below V20, so it needs neither the 0..[`END`] cut nor the fixtures.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use dash_chain::IndexAccess as _;
use dash_consensus::coins::BlockUndo;
use dash_consensus::primitives::{decode_block, decode_header};
use dash_consensus::{Amount, Block};
use dash_primitives::{QuorumHash, TxType};
use dash_statedigest::{Chain, Kind, Outcome, Record, Value};
use dashd_chainstate::chain::{BlockFiles, ChainHandle, OnV};
use dashd_chainstate::connect::MuHashAt;
use dashd_chainstate::evo::cl::census_under;
use dashd_chainstate::evo::gates::counts_under;
use dashd_chainstate::evo::rpc_shape::{
    BlockPayments, RenderError, RenderParams, masternode_payments, payments_at_fees,
};
use dashd_rpc::key_io::{KeyIoParams, encode_destination, extract_destination};
use serde_json::{Value as Json, json};

use super::state_hash::{
    ALL_C_S, DigestRun, EVO_KEYS, linear_clean_noting, projected, read_records,
};
use super::*;

/// The first non-null testnet commitment's block: LLMQ_50_60 (`dkgInterval` 24), at 7,522.
const FIRST_QC: (u32, &str) =
    (7_522, "000000001c500ea39146e911f9f7630a389af7c603ab48d89b58b5d0b424b43d");
/// [`FIRST_QC`] and two of its type's DKG intervals.
const H_END: (u32, &str) =
    (7_570, "00000000b5f7ec21d990e84ab593a3a5e42185237ebf27bdd32a08769edaa34c");
/// A height by which testnet's first rotated cycle, based at 769,824, has mined its commitments.
const ROTATED_BY: u32 = 769_824 + 288;
/// The indexed (rotated) commitment versions, legacy and basic BLS (`llmq/commitment.h:48-51`).
const INDEXED: [u16; 2] = [2, 4];

/// Testnet's V20 (`chainparams.cpp:405`): the first CbTx of version 3, with a best ChainLock.
const V20: u32 = 905_100;
/// The first non-null CbTx ChainLock: the block at its height, the height it locks, the block.
const FIRST_CL: (u32, u32, &str) =
    (905_100, 905_099, "0000020c5e0f86f385cbf8e90210de9a9fd63633f01433bf47a6b3227a2851fd");
/// The golden cut's last block (`provenance.json`'s `slice.last`).
const END: (u32, &str) =
    (1_574_404, "000000e6b5dae5604cd591f39d946457340ed66eedd5b9ecd302a5e9ec251402");
/// To [`END`]: the CbTxs of version 3 or later, and the non-null best ChainLocks among them.
const END_STEP_TEN: (usize, usize) = (669_305, 669_305);
/// SW-V24's cut of its rc.5 testnet recording, `H` and the `C`s of 0..[`END`] (DEC-1061).
const GOLDEN: &str = "golden-0-1574404.jsonl.zst";
/// The cut's sha256: the file's, and its decompressed stream's (`SHA256SUMS`).
const GOLDEN_SHA256: (&str, &str) = (
    "dcb6642dde2f393ebdd2c90052c0eefab090aa90a1871a45366e21f33ee1567d",
    "fd6b704b3ecaa007b586026a2ca1d9b898b7e8f11dc8c160b0b48aab65043c88",
);
/// The whole recording's sha256, to 1,574,533 (`FULL-GOLDEN.sha256`, the extractor's re-cut):
/// the full golden C9a's cut and C4's slice ([`SLICE`]) were both cut from.
const FULL_GOLDEN_SHA256: &str = "2597ed0d0619daafa0c658f3cc8aa40fcada181e8cb1c34c905a5f012c9862a1";
/// The recording's Core (`provenance.json`'s `core`): describe and commit.
const GOLDEN_CORE: (&str, &str) =
    ("v24.0.0-rc.5-16-g8a8e56406e13", "8a8e56406e135a667b032b8249fcd75c17337b2c");

/// The oracle of SW-V24's payments fixtures (PLAN §5.1, DEC-1061): rc.5's describe, version
/// line and binary sha256.
const PAYMENTS_ORACLE: [&str; 3] = [
    "v24.0.0-rc.5",
    "Dash Core version v24.0.0-rc.5",
    "fa6f61de98950d17adc1065155f783b189bc40b4694534df85354511bc8b8a3c",
];
/// Where the payments fixtures were cut, which each C9b result names.
const PAYMENTS_REPLICA: &str = "replica from run2 live datadir, DEC-1146";
/// The fixture set's `SHA256SUMS`, by its own sha256 (OD-CP-70). A re-cut set (DEC-1146: run2
/// wins) is named by its own in `CH7B3_TESTNET_PAYMENTS_SUMS`.
const PAYMENTS_SUMS: &str = "7f81ef12b52435c117127bf9885c2fb8debe5b7eade707bdc13d0e2309973634";
/// The calendar fixtures' first heights (PLAN §5.1): DIP0003, its enforcement, DIP0024's
/// cycles, V19, V20, MN_RR, the withdrawals, every 100,000th block and the span's end.
const PAYMENTS_CALENDAR: [u32; 24] = [
    7_000, 7_290, 100_000, 200_000, 300_000, 400_000, 500_000, 600_000, 700_000, 769_700, 770_730,
    800_000, 850_100, 900_000, 905_100, 1_000_000, 1_066_900, 1_100_000, 1_148_500, 1_200_000,
    1_300_000, 1_400_000, 1_500_000, 1_574_385,
];
/// The witness fixtures, their first heights, which are their witness blocks, and the property
/// each was picked for (PLAN §5.1, the fixtures' `NOTES.md`): all five were found, so all five
/// must be there (PLAN :409), each block with its property ([`witness_holds`]).
const PAYMENTS_WITNESSES: [(&str, u32, &str); 5] = [
    (
        "1",
        60_937,
        "the coinbase claims less than the subsidy and fees, and the payment on its claim differs",
    ),
    ("2", 1_117_834, "a transaction is a platform transfer"),
    (
        "3",
        7_301,
        "two payees or more, a type-0 transaction paying a fee, and each payee's amount differs \
         from the render at no fees",
    ),
    ("4a", 7_090, "no proTxHash and no payee"),
    ("4b", 7_301, "a proTxHash and a payee"),
];
/// The blocks a fixture covers: `masternode payments <getblockhash(first)> 20`.
const PAYMENTS_COUNT: u32 = 20;

/// `bytes` as `sha256sum` prints a sum.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The golden cut in `dir` is the pinned one and its `C`s are `chain`'s, to the run's tip: see
/// the module doc. A failure names a height or a key, never a value.
fn golden_is_the_located_chain(dir: &Path, chain: &[(BlockHash, Located)]) {
    let provenance = std::fs::read(dir.join("provenance.json")).expect("provenance.json");
    let provenance: serde_json::Value = serde_json::from_slice(&provenance).expect("JSON");
    for (key, want) in [("describe", GOLDEN_CORE.0), ("commit", GOLDEN_CORE.1)] {
        assert_eq!(provenance["core"][key].as_str(), Some(want), "provenance's core.{key}");
    }
    assert_eq!(provenance["network"].as_str(), Some("testnet"), "provenance's network");
    let full = std::fs::read_to_string(dir.join("FULL-GOLDEN.sha256")).expect("FULL-GOLDEN");
    assert_eq!(full.split_whitespace().next(), Some(FULL_GOLDEN_SHA256), "FULL-GOLDEN.sha256");
    let copy = provenance["full_golden_sha256"].as_str();
    assert_eq!(copy, Some(FULL_GOLDEN_SHA256), "provenance's full_golden_sha256");
    let last = &provenance["slice"]["last"];
    let end = (last["h"].as_u64(), last["b"].as_str());
    assert!(end == (Some(END.0.into()), Some(END.1)), "provenance's slice.last is not END");
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).expect("SHA256SUMS");
    let sums: BTreeSet<(&str, &str)> = sums
        .lines()
        .map(|l| l.split_once("  ").expect("SHA256SUMS: `<sha256>  <name>`"))
        .map(|(sum, name)| (name.trim_start_matches('*'), sum))
        .collect();
    let plain = GOLDEN.strip_suffix(".zst").expect("a .zst name");
    let want = BTreeSet::from([(GOLDEN, GOLDEN_SHA256.0), (plain, GOLDEN_SHA256.1)]);
    assert!(sums == want, "SHA256SUMS is not the pinned cut's");

    let zst = std::fs::read(dir.join(GOLDEN)).expect("the golden cut");
    assert!(hex(&dash_sha256::sha256(&zst)) == GOLDEN_SHA256.0, "the cut's sha256");
    let mut lines = BufReader::new(zstd::stream::read::Decoder::new(zst.as_slice()).unwrap());
    let (mut plain, mut line, mut n) = (dash_sha256::Sha256::new(), Vec::new(), 0_u32);
    let mut last = None;
    while lines.read_until(b'\n', &mut line).expect("the cut decompresses") > 0 {
        plain.update(&line);
        let r: serde_json::Value = serde_json::from_slice(&line).expect("a JSON record");
        line.clear();
        if n == 0 {
            assert_eq!((r["k"].as_str(), r["chain"].as_str()), (Some("H"), Some("test")), "H");
            n = 1;
            continue;
        }
        let h = n - 1;
        assert_eq!((r["k"].as_str(), r["h"].as_u64()), (Some("C"), Some(u64::from(h))), "C {h}");
        let b = r["b"].as_str().expect("a block hash");
        if let Some((hash, _)) = chain.get(h as usize) {
            assert!(*b == hash.to_string(), "{h}: the golden's C is not the located block");
        }
        last = Some((h, b.to_owned()));
        n += 1;
    }
    assert!(hex(&plain.finalize()) == GOLDEN_SHA256.1, "the cut's decompressed sha256");
    let last = last.expect("a C");
    assert!(last.0 == END.0 && last.1 == END.1, "the cut's last C is not END");
    assert!(chain.len() < n as usize, "the run's tip is past the cut");
    eprintln!("T-TN: the golden cut's pins hold; its Cs to {} are located", chain.len() - 1);
}

/// A CbTx payload's best ChainLock, from version 3 (`CLSIG_AND_BALANCE`, `evo/cbtx.h:54-68`):
/// past the version, height and two roots (70 bytes), `bestCLHeightDiff` as a compact size, then
/// the 96-byte signature. `None` for the null lock (the zero signature), else the difference.
fn best_chainlock(payload: &[u8]) -> Option<u64> {
    let (diff, at) = match payload[70] {
        n @ 0..=0xfc => (u64::from(n), 71),
        0xfd => (u16::from_le_bytes(payload[71..73].try_into().unwrap()).into(), 73),
        0xfe => (u32::from_le_bytes(payload[71..75].try_into().unwrap()).into(), 75),
        0xff => (u64::from_le_bytes(payload[71..79].try_into().unwrap()), 79),
    };
    (payload[at..at + 96] != [0; 96]).then_some(diff)
}

/// C9's per-block observer (CH7b-3 PLAN C9): T-TN hands it each block once the block has
/// connected. The verify census ([`StepTen`]) and C9b's payments ([`Payments`]) are two; EV1-J's
/// J2 census of types 7 and 9 extends it.
trait WindowObserver {
    fn connected(&mut self, h: u32, block: &Arc<Block>);
}

/// Step 10's work as the CbTxs give it, decoded apart from the node: the heights of the CbTxs of
/// version 3 or later, and each non-null best ChainLock as `(h, h - 1 - bestCLHeightDiff)`.
#[derive(Default)]
struct StepTen {
    v3: Vec<u32>,
    locks: Vec<(u32, u32)>,
}

impl WindowObserver for StepTen {
    fn connected(&mut self, h: u32, block: &Arc<Block>) {
        if cbtx_version(block).is_none_or(|v| v < 3) {
            return;
        }
        self.v3.push(h);
        let payload = block.txs()[0].to_mutable().extra_payload().clone();
        if let Some(diff) = best_chainlock(&payload) {
            let diff = u32::try_from(diff).expect("a difference below the height");
            self.locks.push((h, h - 1 - diff));
        }
    }
}

impl StepTen {
    /// The node in `dir` against the decoded work (the module doc): an ask at each CbTx of
    /// version 3, Core's null lock each time, no shortcut, and its census the decoded locks in
    /// order, each valid. Pins the first lock, and the counts to [`END`].
    fn assert_census(&self, dir: &Path, to: u32, hash: impl Fn(u32) -> BlockHash) {
        let roots = counts_under(dir).expect("the node's cells").root_checks();
        let asked: Vec<u32> = roots.cl_asks.iter().map(|&(h, _)| h).collect();
        assert!(asked == self.v3, "step 10's asks: {}", parting(&asked, &self.v3));
        let answered = roots.cl_asks.iter().find(|(_, best)| best.is_some());
        assert!(answered.is_none(), "a best ChainLock at {:?}", answered.map(|a| a.0));
        assert!(roots.cl_shortcuts.is_empty(), "a shortcut at {:?}", roots.cl_shortcuts.first());
        let census = census_under(dir).expect("the node's census").checks.lock().clone();
        let want = self.locks.iter().map(|&(h, cl)| (h, cl, Some(true)));
        let parted = census.iter().copied().zip(want).position(|(got, want)| got != want);
        assert!(
            parted.is_none() && census.len() == self.locks.len(),
            "the census: {} checks, want {}; parted at #{parted:?}, height {:?}",
            census.len(),
            self.locks.len(),
            parted.map(|i| self.locks[i].0)
        );
        if to >= FIRST_CL.0 {
            assert_eq!(self.locks.first().map(|l| (l.0, l.1)), Some((FIRST_CL.0, FIRST_CL.1)));
            assert_eq!(hash(FIRST_CL.0).to_string(), FIRST_CL.2, "the first CbTx ChainLock's");
        }
        if to == END.0 {
            assert_eq!((self.v3.len(), self.locks.len()), END_STEP_TEN, "the counts to END");
        }
        eprintln!(
            "T-TN step 10: {} CbTxs of version 3 from {:?}, each asked Core's null best \
             ChainLock; {} verifies, each valid, in the decoded order, from {:?}",
            self.v3.len(),
            self.v3.first(),
            census.len(),
            self.locks.first()
        );
    }
}

/// One of SW-V24's payments fixtures: its file, the file's sha256, its heights and the RPC's
/// entries, one per height.
struct Fixture {
    name: String,
    sha256: String,
    first: u32,
    last: u32,
    result: Vec<Json>,
}

/// The payments fixtures in a directory, their pins checked ([`Fixtures::load`]), the sha256 of
/// their `SHA256SUMS` and whether it is the pin, and the owner report their results go to.
struct Fixtures {
    fixtures: Vec<Fixture>,
    sums: String,
    pinned: bool,
    report: File,
}

impl Fixtures {
    /// `dir`'s fixtures, for a run to `to` on the located chain `hash`: `SHA256SUMS`'s sha256 is
    /// `sums_pin`, and it lists every other file of `dir` with its sha256;
    /// `provenance.json` and each fixture name the pinned oracle; the fixtures are the calendar
    /// and witness windows, each `masternode payments <hash(first)> 20` with one entry per height
    /// of `first..=last`, the heights its name gives, each witness's block its first, and each
    /// entry's block the located one where the run reaches it. The owner report, created at
    /// `report`, starts with the sums and the provenance. A failure names a file and a key, never
    /// a value.
    fn load(
        dir: &Path,
        sums_pin: &str,
        report: &Path,
        to: u32,
        hash: impl Fn(u32) -> BlockHash,
    ) -> Fixtures {
        let read =
            |name: &str| std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let sums = String::from_utf8(read("SHA256SUMS")).expect("SHA256SUMS is text");
        let sums_sha256 = hex(&dash_sha256::sha256(sums.as_bytes()));
        assert!(sums_sha256 == sums_pin, "SHA256SUMS is not the pinned fixture set");
        let mut listed = BTreeMap::new();
        for line in sums.lines() {
            let (sum, name) = line.split_once("  ").expect("SHA256SUMS: `<sha256>  <name>`");
            assert!(hex(&dash_sha256::sha256(&read(name))) == sum, "{name}'s sha256");
            assert!(listed.insert(name.to_owned(), sum.to_owned()).is_none(), "{name} twice");
        }
        for entry in std::fs::read_dir(dir).expect("the fixtures") {
            let name = entry.expect("an entry").file_name().into_string().expect("a name");
            assert!(name == "SHA256SUMS" || listed.contains_key(&name), "{name} unlisted");
        }
        let provenance: Json =
            serde_json::from_slice(&read("provenance.json")).expect("provenance.json");
        for (key, want) in [
            ("dashd_version", PAYMENTS_ORACLE[1]),
            ("dashd_sha256", PAYMENTS_ORACLE[2]),
            ("amended_by", "DEC-1146"),
            ("windows_request", "masternode payments <getblockhash(first)> 20"),
        ] {
            assert_eq!(provenance[key].as_str(), Some(want), "provenance.json's {key}");
        }
        let oracle = json!({
            "core_describe": PAYMENTS_ORACLE[0],
            "dashd_version": PAYMENTS_ORACLE[1],
            "dashd_sha256": PAYMENTS_ORACLE[2],
        });
        let mut fixtures = Vec::new();
        for (name, sum) in &listed {
            let is_fixture = name.starts_with("payments-") || name.starts_with("witness-");
            let Some(stem) = name.strip_suffix(".json").filter(|_| is_fixture) else {
                continue;
            };
            let mut heights = stem.rsplitn(3, '-').map(|h| h.parse::<u32>().expect("a height"));
            let (last, first) = (heights.next().unwrap(), heights.next().unwrap());
            let f: Json = serde_json::from_slice(&read(name)).expect("a fixture");
            assert!(f["oracle"] == oracle, "{name}: its oracle");
            assert_eq!(f["method"].as_str(), Some("masternode"), "{name}: its method");
            let covered = (f["first"].as_u64(), f["last"].as_u64());
            assert_eq!(covered, (Some(first.into()), Some(last.into())), "{name}: its heights");
            assert_eq!(last + 1 - first, PAYMENTS_COUNT, "{name}: its count");
            let result = f["result"].as_array().expect("its result").clone();
            let got: Vec<_> = result.iter().map(|e| e["height"].as_u64()).collect();
            let want: Vec<_> = (first..=last).map(|h| Some(u64::from(h))).collect();
            assert!(got == want, "{name}: one entry per height");
            if name.starts_with("witness-") {
                let at = f["witness"]["height"].as_u64();
                assert_eq!(at, Some(first.into()), "{name}: its witness block");
            }
            if last <= to {
                let request = json!(["payments", hash(first).to_string(), PAYMENTS_COUNT]);
                assert!(f["request"] == request, "{name}: its request");
                for (h, e) in (first..).zip(&result) {
                    let located = hash(h).to_string();
                    assert!(e["blockhash"].as_str() == Some(&located), "{name}: {h}'s block");
                }
            }
            fixtures.push(Fixture {
                name: name.clone(),
                sha256: sum.clone(),
                first,
                last,
                result,
            });
        }
        let calendar = PAYMENTS_CALENDAR.map(|h| format!("payments-{h}-{}.json", h + 19));
        let witnesses =
            PAYMENTS_WITNESSES.map(|(w, h, _)| format!("witness-{w}-{h}-{}.json", h + 19));
        let want: BTreeSet<&str> = calendar.iter().chain(&witnesses).map(String::as_str).collect();
        let got: BTreeSet<&str> = fixtures.iter().map(|f| f.name.as_str()).collect();
        let (missing, extra): (Vec<_>, Vec<_>) =
            (want.difference(&got).collect(), got.difference(&want).collect());
        assert!(
            missing.is_empty() && extra.is_empty(),
            "the fixtures: {missing:?} missing, {extra:?} extra"
        );

        let mut report = File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(report)
            .expect("the owner report");
        let pinned = sums_pin == PAYMENTS_SUMS;
        let mut head = format!(
            "T-TN C9b owner report (CH7b-3 PLAN C9, §6.3): masternode payments, the node \
             against SW-V24's fixtures\nfixtures {}\n{PAYMENTS_REPLICA}\nrun to {to}\n\
             SHA256SUMS sha256 {sums_sha256} ({})\n{sums}",
            dir.display(),
            if pinned {
                "the pin"
            } else {
                "CH7B3_TESTNET_PAYMENTS_SUMS"
            }
        );
        for (key, value) in provenance.as_object().expect("provenance.json is an object") {
            head += &format!("provenance {key}: {value}\n");
        }
        report.write_all(head.as_bytes()).expect("the owner report");
        Fixtures {
            fixtures,
            sums: sums_sha256,
            pinned,
            report,
        }
    }
}

/// C9b (the module doc): each block of the fixtures the run reaches, rendered once it has
/// connected, then compared with the fixtures' entries ([`Payments::finish`]).
struct Payments {
    fixtures: Fixtures,
    chain: ChainHandle,
    /// The node's own block files, as the RPC reads undo records through them.
    files: Box<dyn BlockFiles>,
    params: Arc<RenderParams>,
    key_io: KeyIoParams,
    to: u32,
    /// The rendered entries, by height, in the RPC's shape.
    rendered: BTreeMap<u32, Json>,
    /// Whether each reached witness's block has its property.
    witnesses: BTreeMap<&'static str, bool>,
}

impl Payments {
    fn new(fixtures: Fixtures, sut: &mut NodeSut, to: u32) -> Payments {
        let files = sut.steps.take_rpc_files().expect("the node's block files");
        let p = sut.params();
        Payments {
            fixtures,
            chain: sut.chain().expect("a chain").clone(),
            files,
            params: Arc::new(RenderParams::new(p).expect("testnet's render parameters")),
            key_io: crate::rpc::key_io_params(p),
            to,
            rendered: BTreeMap::new(),
            witnesses: BTreeMap::new(),
        }
    }

    /// `got` as the RPC prints it (`rpc/masternode.cpp:470-498`): its one masternode's payees
    /// with their `EncodeDestination` addresses (`""` for a script with none), and `""` for no
    /// payee.
    fn entry(&self, got: &BlockPayments) -> Json {
        let payees: Vec<Json> = got
            .payees
            .iter()
            .map(|out| {
                let script = out.script_pubkey().as_bytes();
                let address = extract_destination(script)
                    .map(|dest| encode_destination(&dest, &self.key_io))
                    .unwrap_or_default();
                json!({"address": address, "script": hex(script), "amount": out.value()})
            })
            .collect();
        let pro_tx_hash = got.pro_tx_hash.map(|h| h.to_string()).unwrap_or_default();
        json!({
            "height": got.height,
            "blockhash": got.block_hash.to_string(),
            "amount": got.amount(),
            "masternodes": [{"proTxHash": pro_tx_hash, "amount": got.amount(), "payees": payees}],
        })
    }

    /// Each reached fixture against the node's entries, written to the owner report, which
    /// alone holds values; then a panic, after the report, if any entry differs. Under C14
    /// (`diag`) the fixture set's SHA256SUMS sha256 goes to C14's owner report, not to stderr.
    fn finish(mut self, diag: &Diag) {
        let (mut reached, mut compared, mut parted) = (0, 0, Vec::new());
        let mut lines = String::new();
        for f in &self.fixtures.fixtures {
            let at = format!("{} sha256 {} {}..{}", f.name, f.sha256, f.first, f.last);
            if f.last > self.to {
                lines += &format!("{at}: not reached\n");
                continue;
            }
            reached += 1;
            let mut first = None;
            let mut differ = 0;
            for (h, want) in (f.first..).zip(&f.result) {
                let got = &self.rendered[&h];
                compared += 1;
                if let Some(path) = differs("", got, want) {
                    differ += 1;
                    first.get_or_insert(format!("first at {h}, {path}: node {got} fixture {want}"));
                }
            }
            match first {
                None => lines += &format!("{at}: {PAYMENTS_COUNT} blocks, each equal\n"),
                Some(first) => {
                    lines += &format!("{at}: {differ} of {PAYMENTS_COUNT} differ, {first}\n");
                    parted.push(f.name.clone());
                }
            }
        }
        let (mut held, mut failed) = (0, Vec::new());
        for (id, first, property) in PAYMENTS_WITNESSES {
            let state = match self.witnesses.get(id) {
                Some(true) => "holds",
                Some(false) => "FAILS",
                None if first + PAYMENTS_COUNT - 1 <= self.to => "not rendered",
                None => "not reached",
            };
            held += usize::from(state == "holds");
            if matches!(state, "FAILS" | "not rendered") {
                failed.push(id);
            }
            lines += &format!("witness {id} at {first}: {property}: {state}\n");
        }
        let total = self.fixtures.fixtures.len();
        lines += &format!(
            "C9b: {reached} of {total} fixtures reached, {compared} blocks compared, {} fixtures \
             differ; {held} witness properties hold, {} fail\n",
            parted.len(),
            failed.len()
        );
        let report = &mut self.fixtures.report;
        report.write_all(lines.as_bytes()).expect("the owner report");
        report.sync_all().expect("the owner report");
        let sums = if self.fixtures.pinned {
            "pin"
        } else {
            "override"
        };
        let keyed = match diag {
            Diag::Print => format!(
                "keyed to SHA256SUMS sha256 {} ({sums}; {PAYMENTS_REPLICA})",
                self.fixtures.sums
            ),
            Diag::Owner(_) => {
                diag.note(&format!("C9b: keyed to SHA256SUMS sha256 {}", self.fixtures.sums));
                format!("keyed to the SHA256SUMS {sums} ({PAYMENTS_REPLICA})")
            }
        };
        eprintln!(
            "T-TN C9b: {reached} of {total} fixtures reached, {compared} blocks compared, {} \
             differ; {held} witness properties hold, {} fail; {keyed}, values in the owner report",
            parted.len(),
            failed.len(),
        );
        assert!(parted.is_empty(), "C9b: {} fixtures differ, first {}", parted.len(), parted[0]);
        assert!(failed.is_empty(), "C9b: witness {}'s property fails", failed[0]);
    }
}

impl WindowObserver for Payments {
    fn connected(&mut self, h: u32, block: &Arc<Block>) {
        let reached = |f: &Fixture| f.last <= self.to && (f.first..=f.last).contains(&h);
        if !self.fixtures.fixtures.iter().any(reached) {
            return;
        }
        let hash = block.hash();
        let looked = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let id = reader.lookup(&hash)?;
            Some((reader.undo_pos(id)?, reader.entry(id)?.header.prev_hash()))
        });
        let (pos, parent) = looked.recv().expect("V serves").expect("an undo position");
        let undo = self.files.read_undo(pos, &parent).unwrap_or_else(|e| panic!("{h}'s undo: {e}"));
        let witnesses: Vec<&'static str> = PAYMENTS_WITNESSES
            .iter()
            .filter(|&&(_, first, _)| first == h && first + PAYMENTS_COUNT - 1 <= self.to)
            .map(|&(id, ..)| id)
            .collect();
        let spend_fee = !witnesses.is_empty() && spend_pays_a_fee(block, &undo);
        let alternatives = !witnesses.is_empty();
        let (shared, params) = (Arc::clone(block), Arc::clone(&self.params));
        let rendered = self.chain.query(move |q| {
            let got = masternode_payments(q, &shared, &undo, &params)?;
            let others = if alternatives {
                let claimed = shared.txs()[0].value_out() - got.subsidy;
                let at_claim = payments_at_fees(q, &shared, claimed, &params)?;
                Some((at_claim, payments_at_fees(q, &shared, 0, &params)?))
            } else {
                None
            };
            Ok::<_, RenderError>((got, others))
        });
        let rendered = rendered.recv().expect("V serves").map_err(|e| e.to_string());
        let (got, others) = rendered.unwrap_or_else(|e| panic!("{h}'s entry: {e}"));
        if let Some((claimed, unpaid)) = &others {
            for id in witnesses {
                let holds = witness_holds(id, block, &got, claimed, unpaid, spend_fee);
                self.witnesses.insert(id, holds);
            }
        }
        let entry = self.entry(&got);
        self.rendered.insert(h, entry);
    }
}

/// Whether some type-0 transaction of `block`, whose undo is `undo`, pays a fee: its coins less
/// its outputs.
fn spend_pays_a_fee(block: &Block, undo: &BlockUndo) -> bool {
    block.txs().iter().skip(1).zip(&undo.txs).any(|(tx, spent)| {
        let value_in: Amount = spent.coins.iter().map(|c| c.value()).sum();
        tx.tx_type() == TxType::Spend && value_in > tx.value_out()
    })
}

/// Whether witness `id`'s block has the property it was picked for ([`PAYMENTS_WITNESSES`]), on
/// the node's side alone: `got` is the block's entry, `claimed` and `unpaid` its entries were its
/// fees what its coinbase claims past the subsidy, or none, and `spend_fee` whether a type-0
/// transaction pays a fee ([`spend_pays_a_fee`]). Nothing is read from the fixture.
fn witness_holds(
    id: &str,
    block: &Block,
    got: &BlockPayments,
    claimed: &BlockPayments,
    unpaid: &BlockPayments,
    spend_fee: bool,
) -> bool {
    match id {
        "1" => {
            let underclaims = block.txs()[0].value_out() < got.subsidy + got.fees;
            underclaims && claimed.amount() != got.amount()
        }
        "2" => block.txs().iter().any(|tx| tx.is_platform_transfer()),
        "3" => {
            let mut paid = got.payees.iter().zip(&unpaid.payees);
            let each_moved = got.payees.len() == unpaid.payees.len()
                && paid.all(|(a, b)| a.value() != b.value());
            got.payees.len() >= 2 && spend_fee && each_moved
        }
        "4a" => got.pro_tx_hash.is_none() && got.payees.is_empty(),
        "4b" => got.pro_tx_hash.is_some() && !got.payees.is_empty(),
        _ => unreachable!("witness {id}"),
    }
}

/// The first path at which `got` and `want` differ, by key and index; `None` where they are equal.
fn differs(path: &str, got: &Json, want: &Json) -> Option<String> {
    match (got, want) {
        (Json::Object(g), Json::Object(w)) => {
            let keys: BTreeSet<&String> = g.keys().chain(w.keys()).collect();
            keys.into_iter().find_map(|k| match (g.get(k), w.get(k)) {
                (Some(a), Some(b)) => differs(&format!("{path}.{k}"), a, b),
                _ => Some(format!("{path}.{k}")),
            })
        }
        (Json::Array(g), Json::Array(w)) if g.len() == w.len() => {
            let mut pairs = g.iter().zip(w).enumerate();
            pairs.find_map(|(i, (a, b))| differs(&format!("{path}[{i}]"), a, b))
        }
        _ => (got != want).then(|| path.to_owned()),
    }
}

/// CH7a-5b C4's slice (DEC-1061): SW-V24's cut of its rc.5 testnet recording, `H` and the `C`s
/// of 0..[`H_END`].
const SLICE: &str = "golden-0-7570.jsonl.zst";
/// The slice's sha256: the file's, and its decompressed stream's (`SHA256SUMS`).
const SLICE_SHA256: (&str, &str) = (
    "ccb8f059877fe23141eb307f011c00df7b6bf0cac95e54d520a8b9fdb79f9102",
    "a11b49056ecaac74819a957961ddafb7b1854ed8c9008371ba7ad8ae6288b226",
);
/// The recording's Core (`provenance.json`'s `core`): describe and commit.
const SLICE_CORE: (&str, &str) =
    ("v24.0.0-rc.5-16-g8a8e56406e13", "8a8e56406e135a667b032b8249fcd75c17337b2c");
/// Testnet's DIP0003 height: below it every evo key is null.
const DIP3: i64 = 7_000;

/// C4's compare: the slice's directory and the owner report's.
struct Compare {
    slice: PathBuf,
    owner: PathBuf,
}

impl Compare {
    /// Both variables, or neither: no compare.
    fn from_env() -> Option<Compare> {
        let slice = std::env::var_os("CH7A5B_TESTNET_GOLDEN").map(PathBuf::from);
        let owner = std::env::var_os("CH7A5B_OWNER_REPORT").map(PathBuf::from);
        match (slice, owner) {
            (Some(slice), Some(owner)) => Some(Compare {
                slice,
                owner,
            }),
            (None, None) => None,
            _ => panic!("CH7A5B_TESTNET_GOLDEN and CH7A5B_OWNER_REPORT go together"),
        }
    }

    /// Appends `what` to the owner report (mode 0600).
    fn note(&self, what: &str) {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(self.owner.join("owner-report.txt")).expect("the owner report");
        writeln!(f, "{what}").expect("the owner report");
    }

    /// Fails with `msg`, which carries no value, after noting `detail` in the owner report.
    fn fail(&self, msg: &str, detail: &str) -> ! {
        self.note(&format!("{msg}: {detail}"));
        panic!("{msg} (details in the owner report)");
    }

    /// The slice's records, once its pins hold: its sha256s, the full golden's, its Core and
    /// network, and its `H`.
    fn pinned_slice(&self) -> Vec<Record> {
        let path = self.slice.join(SLICE);
        let zst = std::fs::read(&path).unwrap_or_else(|e| self.fail("no slice", &e.to_string()));
        let plain = zstd::decode_all(zst.as_slice())
            .unwrap_or_else(|e| self.fail("the slice does not decompress", &e.to_string()));
        for (what, bytes, want) in
            [("file", &zst, SLICE_SHA256.0), ("decompressed stream", &plain, SLICE_SHA256.1)]
        {
            let got = sha256_hex(bytes);
            if got != want {
                self.fail(&format!("the slice's {what} sha256"), &format!("{got}, want {want}"));
            }
        }
        let provenance = std::fs::read(self.slice.join("provenance.json"))
            .unwrap_or_else(|e| self.fail("no provenance.json", &e.to_string()));
        let provenance: serde_json::Value = serde_json::from_slice(&provenance)
            .unwrap_or_else(|e| self.fail("provenance.json does not parse", &e.to_string()));
        for (key, want) in [("describe", SLICE_CORE.0), ("commit", SLICE_CORE.1)] {
            let got = provenance["core"][key].as_str();
            if got != Some(want) {
                self.fail(&format!("provenance's core.{key}"), &format!("{got:?}, want {want}"));
            }
        }
        let full = std::fs::read_to_string(self.slice.join("FULL-GOLDEN.sha256"))
            .unwrap_or_else(|e| self.fail("no FULL-GOLDEN.sha256", &e.to_string()));
        for (what, got) in [
            ("FULL-GOLDEN.sha256", full.split_whitespace().next()),
            ("provenance's full_golden_sha256", provenance["full_golden_sha256"].as_str()),
        ] {
            if got != Some(FULL_GOLDEN_SHA256) {
                self.fail(what, &format!("{got:?}, want {FULL_GOLDEN_SHA256}"));
            }
        }
        let network = provenance["network"].as_str();
        if network != Some("testnet") {
            self.fail("provenance's network", &format!("{network:?}"));
        }
        let records = read_records(&path);
        match records.first() {
            Some(Record::Header(h)) if h.chain == Chain::Test => {}
            other => self.fail("the slice's H is not testnet's", &format!("{other:?}")),
        }
        records
    }

    /// The slice's `C`s are the located chain to [`H_END`] (CH3a D146): one per height, its
    /// block the located one, its header keys that block's header.
    fn golden_is_the_located_chain(
        &self,
        golden: &[(usize, Outcome)],
        chain: &[(BlockHash, Located)],
    ) {
        let want_n = H_END.0 as usize + 1;
        if golden.len() != want_n {
            self.fail("the slice's C count", &format!("{}, want {want_n}", golden.len()));
        }
        for (h, ((_, o), (hash, located))) in golden.iter().zip(chain).enumerate() {
            if o.id.h != i64::try_from(h).expect("a height") || o.id.b != display(*hash) {
                self.fail(
                    &format!("{h}: the slice's C is not the located block"),
                    &format!("{:?}", o.id),
                );
            }
            let block = located.read();
            let header = block.header();
            let prev = h.checked_sub(1).map_or(Value::Null, |p| Value::hash(&display(chain[p].0)));
            let want = [
                ("prev", prev),
                ("ver", Value::Int(header.version().into())),
                ("mrkl", Value::hash(&display(header.merkle_root()))),
                ("time", Value::Int(header.time().into())),
                ("bits", Value::Int(header.bits().into())),
                ("nonce", Value::Int(header.nonce().into())),
            ];
            for (key, want) in want {
                let got = o.s().get(key);
                if got != Some(&want) {
                    self.fail(
                        &format!("{h}: the slice's {key} is not the located block's"),
                        &format!("{got:?}, want {want:?}"),
                    );
                }
            }
        }
        let last = &golden[golden.len() - 1].1.id;
        if last.b.to_string() != H_END.1 {
            self.fail("the slice's last C is not H_END", &format!("{last:?}"));
        }
    }
}

/// C14's `muhash` heights (CH7b-3 PLAN §3.4's S): DIP0003 and its enforcement, DIP0024's cycles,
/// V19, V20, MN_RR, the withdrawals, every 100,000th block and [`END`].
const MUHASH_AT: [u32; 24] = [
    7_000, 7_300, 100_000, 200_000, 300_000, 400_000, 500_000, 600_000, 700_000, 769_700, 770_730,
    800_000, 850_100, 900_000, 905_100, 1_000_000, 1_066_900, 1_100_000, 1_148_500, 1_200_000,
    1_300_000, 1_400_000, 1_500_000, 1_574_404,
];

/// C14's windows (PLAN §3.4): each a name, its first and last height, and its activation height,
/// one of [`MUHASH_AT`]. Each must be complete inside the one compare.
const WINDOWS: [(&str, u32, u32, u32); 5] = [
    ("DIP24", 769_124, 771_306, 769_700),
    ("V19", 849_524, 850_676, 850_100),
    ("V20", 904_524, 905_676, 905_100),
    ("MN_RR", 1_066_324, 1_067_476, 1_066_900),
    ("Withdrawals", 1_147_924, 1_149_076, 1_148_500),
];

/// The owner-only files C14 writes in its directory: the state-hash log ([`DigestRun::on_file`]'s)
/// and the owner report ([`OwnerReport`]).
const STATE_HASH_FILES: [&str; 2] = ["log.jsonl", "state-hash-report.txt"];

/// C14's mode (CH7b-3 PLAN §3.4, R-B): the owner-only directory its state-hash log and owner
/// report go to. T-TN builds it with [`StateHash::from_env`] and its outputs with
/// [`StateHash::open`], the functions the local tests call.
struct StateHash {
    dir: PathBuf,
}

impl StateHash {
    /// `CH7B3_STATE_HASH_DIR`'s mode, when the variable is set: refused unless [`StateHash::at`]
    /// takes the directory.
    fn from_env() -> Option<StateHash> {
        let dir = std::env::var_os("CH7B3_STATE_HASH_DIR")?;
        let mode = StateHash::at(PathBuf::from(dir));
        Some(mode.unwrap_or_else(|why| panic!("CH7B3_STATE_HASH_DIR: {why}")))
    }

    /// `dir` as C14's: a directory itself, not a link to one, with no group or other permission
    /// bits, and no entry yet at either of C14's names, a link or a dangling one among them.
    fn at(dir: PathBuf) -> Result<StateHash, String> {
        use std::os::unix::fs::PermissionsExt as _;

        let meta =
            std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!("{} is mode {mode:o}, not owner-only", dir.display()));
        }
        for file in STATE_HASH_FILES {
            match std::fs::symlink_metadata(dir.join(file)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return Err(format!("{} already holds {file}", dir.display())),
                Err(e) => return Err(format!("{}: {file}: {e}", dir.display())),
            }
        }
        Ok(StateHash {
            dir,
        })
    }

    /// C14's two outputs, each created [`fresh`]: the log with its run and sink (`muhash` at
    /// [`MUHASH_AT`] only, one [`MuHashAt`] for both), and the owner report.
    fn open(&self) -> (DigestRun, Box<dyn dashd_chainstate::connect::DigestSink>, OwnerReport) {
        let [log, report] = STATE_HASH_FILES.map(|name| {
            let path = self.dir.join(name);
            let file = fresh(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            (path, file)
        });
        let muhash = MuHashAt::Only(MUHASH_AT.into());
        let (run, sink) = DigestRun::on_file(log.0, log.1, muhash, Chain::Test);
        (run, sink, OwnerReport(Arc::new(std::sync::Mutex::new(report.1))))
    }
}

/// `path` created for writing at mode 0600, refused if any entry is there, a dangling link too.
fn fresh(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

/// C14's owner report, `state-hash-report.txt`, held open from [`StateHash::open`]: the only
/// place R-B's diagnostics put a value (a hash, a validation state, an `AbortNode` line, a
/// panic's message, the compare's report).
#[derive(Clone)]
struct OwnerReport(Arc<std::sync::Mutex<File>>);

impl OwnerReport {
    /// Appends `what` as a line.
    fn note(&self, what: &str) -> std::io::Result<()> {
        let mut f = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        writeln!(f, "{what}")
    }
}

/// A panic payload whose text carries no value: C14's panic hook prints it as it is.
struct ValueFree(String);

/// T-TN's diagnostics policy (CH7b-3b R2). R-A's and C4's runs print theirs as they always did;
/// C14's prints heights, counts and failure classes only and sends the values to its owner
/// report, a panic's message included ([`owner_panics`]).
enum Diag {
    Print,
    Owner(OwnerReport),
}

impl Diag {
    /// A failure: `full` as the panic's message, or under C14 `value_free`, `full` in the owner
    /// report.
    fn fail(&self, value_free: &str, full: impl FnOnce() -> String) -> ! {
        match self {
            Diag::Print => panic!("{}", full()),
            Diag::Owner(report) => {
                let _ = report.note(&full());
                std::panic::panic_any(ValueFree(format!(
                    "{value_free} (values in the owner report)"
                )))
            }
        }
    }

    /// `assert_eq!(got, want, "{what}")`, `what` value-free.
    fn eq<T: PartialEq + std::fmt::Debug>(&self, got: T, want: T, what: &str) {
        match self {
            Diag::Print => assert_eq!(got, want, "{what}"),
            Diag::Owner(_) if got == want => {}
            Diag::Owner(_) => {
                self.fail(&format!("{what}: differs"), || format!("{what}: {got:?}, want {want:?}"))
            }
        }
    }

    /// `what`, which carries a value, in the owner report under C14; nowhere otherwise.
    fn note(&self, what: &str) {
        if let Diag::Owner(report) = self {
            report.note(what).expect("the owner report");
        }
    }
}

/// C14's panic lines, a panic's `payload` at `at`: what stderr gets, the location and a
/// [`ValueFree`] text only, and what the owner report gets, any other text.
fn panic_lines(payload: &(dyn std::any::Any + Send), at: &str) -> (String, Option<String>) {
    if let Some(ValueFree(text)) = payload.downcast_ref::<ValueFree>() {
        return (format!("T-TN C14: panicked at {at}: {text}"), None);
    }
    let text = payload.downcast_ref::<&str>().map(|s| (*s).to_owned());
    let text = text.or_else(|| payload.downcast_ref::<String>().cloned());
    let text = text.unwrap_or_else(|| "a payload that is not text".to_owned());
    (
        format!("T-TN C14: panicked at {at}; its message is in the owner report"),
        Some(format!("panicked at {at}: {text}")),
    )
}

/// [`panic_lines`] as the process's panic hook, any thread's, until the guard drops.
fn owner_panics(report: OwnerReport) -> PanicHookGuard {
    std::panic::set_hook(Box::new(move |info| {
        let at = info.location().map_or_else(
            || "an unknown place".to_owned(),
            |l| format!("{}:{}", l.file(), l.line()),
        );
        let (line, note) = panic_lines(info.payload(), &at);
        if let Some(note) = note {
            let _ = report.note(&note);
        }
        eprintln!("{line}");
    }));
    PanicHookGuard
}

/// Restores the default panic hook as it drops, outside a panic.
struct PanicHookGuard;

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            drop(std::panic::take_hook());
        }
    }
}

/// T-TN's closing line, the tip's `hash` in it unless C14 leaves it out (R2).
fn summary_line(
    to: u32,
    hash: Option<BlockHash>,
    s8: &[u32],
    s9: &[u32],
    rotated: &[u32],
    first_rotated: Option<u32>,
) -> String {
    let tip = hash.map_or_else(String::new, |h| format!(" ({h})"));
    format!(
        "T-TN to {to}{tip}: S8 {} heights from {:?}, S9 {} from {:?}, rotated {} from {:?}",
        s8.len(),
        s8.first(),
        s9.len(),
        s9.first(),
        rotated.len(),
        first_rotated
    )
}

/// C14's window and era scan over the left's projected records ([`linear_clean_noting`]'s
/// `each_left`): each window's records, whether its activation height carries `muhash`, and the
/// first record from [`V20`] whose `cp` or `ehf` is null or absent.
#[derive(Default)]
struct WindowScan {
    records: [u64; 5],
    muhash: [bool; 5],
    v20_null: Option<(i64, &'static str)>,
}

impl WindowScan {
    fn see(&mut self, o: &Outcome) {
        let h = o.id.h;
        for (i, &(_, first, last, at)) in WINDOWS.iter().enumerate() {
            if (i64::from(first)..=i64::from(last)).contains(&h) {
                self.records[i] += 1;
            }
            if h == i64::from(at) && o.s().get("muhash").is_some() {
                self.muhash[i] = true;
            }
        }
        if h >= i64::from(V20) && self.v20_null.is_none() {
            let null = ["cp", "ehf"].into_iter().find(|k| o.s().get(k).is_none_or(Value::is_null));
            self.v20_null = null.map(|key| (h, key));
        }
    }

    /// Each window complete, one record per height, with `muhash` at its activation height; `cp`
    /// and `ehf` set at every height from V20. Only after the one-to-one compare has passed does
    /// this say the windows were compared.
    fn check(&self) -> Result<(), String> {
        for (i, &(name, first, last, at)) in WINDOWS.iter().enumerate() {
            let want = u64::from(last - first + 1);
            if self.records[i] != want {
                return Err(format!("the {name} window: {} records, want {want}", self.records[i]));
            }
            if !self.muhash[i] {
                return Err(format!("the {name} window: no muhash at {at}"));
            }
        }
        match self.v20_null {
            Some((h, key)) => Err(format!("{h}: {key} is null from V20")),
            None => Ok(()),
        }
    }
}

/// `key`'s count in kB in a `/proc/<pid>/status` text (`VmRSS`, `VmHWM`), or why there is none.
fn status_kb(status: &str, key: &str) -> Result<u64, &'static str> {
    let line = status.lines().find_map(|l| l.split_once(':').filter(|(k, _)| *k == key));
    let (_, rest) = line.ok_or("no such line")?;
    let mut words = rest.split_whitespace();
    match (words.next().map(str::parse::<u64>), words.next(), words.next()) {
        (Some(Ok(n)), Some("kB"), None) => Ok(n),
        _ => Err("not a kB count"),
    }
}

/// This process's `key` in kB ([`status_kb`]), or why it was not measured.
fn proc_kb(key: &str) -> Result<u64, &'static str> {
    let status =
        std::fs::read_to_string("/proc/self/status").map_err(|_| "no /proc/self/status")?;
    status_kb(&status, key)
}

/// Resets this process's peak RSS (`VmHWM`) to its current RSS (`clear_refs` 5).
fn reset_peak() -> Result<(), &'static str> {
    std::fs::write("/proc/self/clear_refs", "5").map_err(|_| "clear_refs refused")
}

/// A count-only measurement: `<name> <n> kB`, or `<name> not measured (<why>)`.
fn kb(name: &str, got: Result<u64, &'static str>) -> String {
    match got {
        Ok(n) => format!("{name} {n} kB"),
        Err(why) => format!("{name} not measured ({why})"),
    }
}

/// C14's node UI: of the node's log lines only `AbortNode`'s (`*** `), each to the owner report
/// whole and to stderr as a count, so R-B's 1.57 million blocks write no log line.
fn aborts_ui(report: OwnerReport) -> Arc<Ui> {
    let logger = Arc::new(Logger::new());
    let aborts = std::sync::atomic::AtomicU32::new(0);
    logger.push_back_callback(move |line| {
        if line.contains("*** ") {
            let n = aborts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let _ = report.note(&format!("AbortNode: {}", line.trim_end_matches('\n')));
            eprintln!("T-TN C14: AbortNode line {n}, its text in the owner report");
        }
    });
    assert!(logger.start_logging());
    Arc::new(Ui::with_stderr(logger, Box::new(|_| {})))
}

/// The compare's node UI: the node's log lines on stderr, which the owner-only runner captures
/// (an `AbortNode` among them).
fn owner_ui() -> Arc<Ui> {
    let logger = Arc::new(Logger::new());
    logger.push_back_callback(|line| eprintln!("{}", line.trim_end_matches('\n')));
    assert!(logger.start_logging());
    Arc::new(Ui::with_stderr(logger, Box::new(|_| {})))
}

/// `bytes`' sha256 as `sha256sum` prints it.
fn sha256_hex(bytes: &[u8]) -> String {
    dash_statedigest::Hash256::from_display_bytes(dash_sha256::sha256(bytes)).to_hex()
}

/// A hash's display text as a record's hash.
fn display(hash: impl std::fmt::Display) -> dash_statedigest::Hash256 {
    let text = format!("{hash:0>64}");
    dash_statedigest::Hash256::from_hex(text.as_bytes()).expect("a 256-bit hash")
}

/// The run's evo keys by era (C4): all null below DIP0003; from it `mnl` and `qc` set, `cp` and
/// `ehf` null (no V20 in the span); the first non-null commitment's row in 7,522's `qc`.
fn evo_keys_by_era(log: &[(usize, Outcome)]) {
    let null = |o: &Outcome, key: &str| o.s().get(key).is_some_and(Value::is_null);
    for (_, o) in log {
        let h = o.id.h;
        if h < DIP3 {
            for key in EVO_KEYS {
                assert!(null(o, key), "{h}: {key} is set below DIP0003");
            }
        } else {
            for key in ["mnl", "qc"] {
                assert!(o.s().get(key).is_some_and(|v| !v.is_null()), "{h}: {key} is null");
            }
            for key in ["cp", "ehf"] {
                assert!(null(o, key), "{h}: {key} is set before V20");
            }
        }
    }
    let first_qc = log.iter().find(|(_, o)| o.id.h == i64::from(FIRST_QC.0)).expect("7,522's C");
    let rows = first_qc.1.s().get("qc");
    assert!(matches!(rows, Some(Value::Arr(rows)) if !rows.is_empty()), "7522: qc has no row");
}

/// Where a located block's bytes are.
struct Located {
    file: PathBuf,
    offset: u64,
    size: usize,
}

impl Located {
    fn read(&self) -> Arc<Block> {
        let mut f = File::open(&self.file).unwrap();
        f.seek(SeekFrom::Start(self.offset)).unwrap();
        let mut raw = vec![0; self.size];
        f.read_exact(&mut raw).unwrap();
        let block = decode_block(&mut raw.as_slice()).expect("a block");
        Arc::new(block.into_value())
    }
}

/// The chain from genesis to the first block met at height `to`, located in `blocks`'s
/// `blk?????.dat` files, read in file order: each record's header is hashed and linked to its
/// parent, so the heights follow from genesis.
fn locate(blocks: &Path, magic: [u8; 4], genesis: BlockHash, to: u32) -> Vec<(BlockHash, Located)> {
    assert!(!blocks.join("xor.dat").exists(), "an obfuscated copy");
    let mut files: Vec<PathBuf> = std::fs::read_dir(blocks)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.starts_with("blk") && name.ends_with(".dat")
        })
        .collect();
    files.sort();
    let mut height = HashMap::from([(genesis, 0_u32)]);
    let mut parent: HashMap<BlockHash, BlockHash> = HashMap::new();
    let mut at: HashMap<BlockHash, Located> = HashMap::new();
    let mut waiting: HashMap<BlockHash, Vec<BlockHash>> = HashMap::new();
    let mut target = None;
    'files: for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let mut i = 0;
        while i + 8 <= bytes.len() && bytes[i..i + 4] != [0; 4] {
            assert_eq!(bytes[i..i + 4], magic, "{}@{i}: a record", file.display());
            let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
            let offset = i + 8;
            i = offset + size;
            let Some(raw) = bytes.get(offset..offset + 80) else {
                break;
            };
            let header = decode_header(&mut &raw[..]).expect("a header").into_value();
            let (hash, prev) = (header.hash(), header.prev_hash());
            let file = file.clone();
            at.insert(
                hash,
                Located {
                    file,
                    offset: offset as u64,
                    size,
                },
            );
            parent.insert(hash, prev);
            let Some(&h) = height.get(&prev) else {
                waiting.entry(prev).or_default().push(hash);
                continue;
            };
            // `hash` and every block waiting on it, at their heights.
            let mut linked = vec![(hash, h + 1)];
            while let Some((hash, h)) = linked.pop() {
                height.insert(hash, h);
                if h == to {
                    target = Some(hash);
                    break 'files;
                }
                let children = waiting.remove(&hash).unwrap_or_default();
                linked.extend(children.into_iter().map(|c| (c, h + 1)));
            }
        }
    }
    let mut chain = vec![target.unwrap_or_else(|| panic!("no block at {to}"))];
    while chain.len() <= to as usize {
        chain.push(parent[chain.last().unwrap()]);
    }
    assert_eq!(chain.last(), Some(&genesis), "linked to genesis");
    chain.reverse();
    // Genesis's own record is in the files too.
    chain.into_iter().map(|hash| (hash, at.remove(&hash).expect("located"))).collect()
}

/// The coinbase's CbTx version: `None` for a coinbase that is no CbTx (`nVersion` 3, type 5).
fn cbtx_version(block: &Block) -> Option<u16> {
    let cb = block.txs()[0].to_mutable();
    let special = cb.version() >= 3 && cb.tx_type() == TxType::new(5);
    special.then(|| u16::from_le_bytes(cb.extra_payload()[..2].try_into().unwrap()))
}

/// The LLMQ type, quorum hash and commitment version of each of `block`'s non-null commitments
/// (type 6): its payload's version and height, then the commitment's version, type and quorum
/// hash (`llmq/commitment.h:95-110`, `:158-161`).
fn non_null_commitments(block: &Block) -> Vec<(dash_params::LlmqType, QuorumHash, u16)> {
    let commitment = |tx: &Arc<Tx>| {
        let tx = tx.to_mutable();
        let p = tx.extra_payload();
        let null = p.len() < 192 || p[p.len() - 192..].iter().all(|&b| b == 0);
        (tx.version() >= 3 && tx.tx_type() == TxType::new(6) && !null).then(|| {
            let version = u16::from_le_bytes(p[6..8].try_into().unwrap());
            let hash: [u8; 32] = p[9..41].try_into().unwrap();
            (dash_params::LlmqType::new(p[8]), QuorumHash::from(hash), version)
        })
    };
    block.txs().iter().filter_map(commitment).collect()
}

/// Where `got` and `want` part, for an assertion's message: a run to 905,099 records some 900,000
/// heights.
fn parting(got: &[u32], want: &[u32]) -> String {
    let at = got.iter().zip(want).position(|(g, w)| g != w).unwrap_or(got.len().min(want.len()));
    let (g, w) = (got.get(at), want.get(at));
    format!("{} heights, want {}; at #{at} {g:?}, want {w:?}", got.len(), want.len())
}

/// [`best_chainlock`] reads `bestCLHeightDiff` as a compact size, then exactly the 96 bytes past
/// it as the signature: the zero one is the null lock.
#[test]
fn best_chainlock_reads_a_compact_difference_then_the_signature() {
    // Past the 70 bytes of version, height and roots: the difference, the signature, a balance.
    let payload = |diff: &[u8], sig: u8| [&[0; 70][..], diff, &[sig; 96], &[7; 8]].concat();
    assert_eq!(best_chainlock(&payload(&[5], 1)), Some(5));
    assert_eq!(best_chainlock(&payload(&[0xfc], 1)), Some(0xfc));
    assert_eq!(best_chainlock(&payload(&[0xfd, 0x2c, 0x01], 1)), Some(300));
    assert_eq!(best_chainlock(&payload(&[0xfe, 1, 0, 1, 0], 1)), Some(0x1_0001));
    assert_eq!(best_chainlock(&payload(&[0], 0)), None, "the null lock, the balance past it");
    // Only the signature's last byte set, past a three-byte difference.
    let mut p = payload(&[0xfd, 0, 0], 0);
    p[73 + 95] = 1;
    assert_eq!(best_chainlock(&p), Some(0));
}

/// CH7a-5b C4's mode variables.
const CH7A5B_VARS: [&str; 2] = ["CH7A5B_TESTNET_GOLDEN", "CH7A5B_OWNER_REPORT"];
/// CH7b-3a-2 C9a's and C9b's mode variables, and CH7b-3b C14's.
const CH7B3_VARS: [&str; 5] = [
    "CH7B3_TESTNET_GOLDEN",
    "CH7B3_TESTNET_PAYMENTS",
    "CH7B3_OWNER_REPORT",
    "CH7B3_TESTNET_PAYMENTS_SUMS",
    "CH7B3_STATE_HASH_DIR",
];

/// Why the mode variables `given` names refuse a run to `to`, with or without the window (the
/// module doc's rule), or `None` where they go together.
fn refused_mix(given: impl Fn(&str) -> bool, window: bool, to: u32) -> Option<String> {
    let set = |vars: &[&'static str]| -> Vec<&'static str> {
        vars.iter().copied().filter(|&v| given(v)).collect()
    };
    let (c4, c9) = (set(&CH7A5B_VARS), set(&CH7B3_VARS));
    if !window {
        let unread = [&c4[..], &c9[..]].concat();
        return (!unread.is_empty()).then(|| format!("{unread:?} need CH7B2_TESTNET_WINDOW"));
    }
    if !c4.is_empty() && !c9.is_empty() {
        return Some(format!("{c4:?} and {c9:?}: one mode per run, CH7a-5b C4's or CH7b-3a-2's"));
    }
    if !c4.is_empty() {
        if c4.len() != CH7A5B_VARS.len() {
            return Some("CH7A5B_TESTNET_GOLDEN and CH7A5B_OWNER_REPORT go together".into());
        }
        if to != H_END.0 {
            return Some(format!("the state-hash compare spans 0..7,570 only, not to {to}"));
        }
    }
    if given("CH7B3_STATE_HASH_DIR") && to != END.0 {
        return Some(format!("the state-hash compare (C14) spans 0..=1,574,404 only, not to {to}"));
    }
    let payments = given("CH7B3_TESTNET_PAYMENTS");
    if payments && !given("CH7B3_OWNER_REPORT") {
        return Some("CH7B3_TESTNET_PAYMENTS needs CH7B3_OWNER_REPORT".into());
    }
    if !payments {
        let stray =
            ["CH7B3_OWNER_REPORT", "CH7B3_TESTNET_PAYMENTS_SUMS"].into_iter().find(|&v| given(v));
        if let Some(stray) = stray {
            return Some(format!("{stray} goes with CH7B3_TESTNET_PAYMENTS"));
        }
    }
    if to >= V20 && !given("CH7B3_TESTNET_GOLDEN") {
        return Some("a run to V20 or past it needs CH7B3_TESTNET_GOLDEN".into());
    }
    if to == END.0 && !payments {
        return Some("a run to END needs CH7B3_TESTNET_PAYMENTS".into());
    }
    None
}

/// [`refused_mix`] allows the module doc's sets and refuses every other mix.
#[test]
fn mode_variables_go_together_by_the_rule() {
    let mix = |vars: &[&str], window: bool, to: u32| refused_mix(|v| vars.contains(&v), window, to);
    let c9b = ["CH7B3_TESTNET_PAYMENTS", "CH7B3_OWNER_REPORT"];
    let ra = ["CH7B3_TESTNET_GOLDEN", "CH7B3_TESTNET_PAYMENTS", "CH7B3_OWNER_REPORT"];
    for (vars, to) in [
        (&[][..], H_END.0),
        (&[], V20 - 1),
        (&CH7A5B_VARS, H_END.0),
        (&["CH7B3_TESTNET_GOLDEN"], H_END.0),
        (&["CH7B3_TESTNET_GOLDEN"], V20),
        (&c9b, H_END.0),
        (&ra, END.0),
        (&CH7B3_VARS, END.0),
    ] {
        assert_eq!(mix(vars, true, to), None, "{vars:?} to {to}");
    }
    for to in [H_END.0, V20, END.0] {
        assert_eq!(mix(&[], false, to), None, "no window, no mode: the skip, to {to}");
    }
    let refused = |vars: &[&str], window: bool, to: u32| {
        assert!(mix(vars, window, to).is_some(), "{vars:?} to {to}, window {window}");
    };
    for var in CH7A5B_VARS.iter().chain(&CH7B3_VARS) {
        refused(&[var], false, H_END.0);
    }
    for var in CH7B3_VARS {
        refused(&[CH7A5B_VARS[0], CH7A5B_VARS[1], var], true, H_END.0);
    }
    for var in CH7A5B_VARS {
        refused(&[var], true, H_END.0);
        refused(&[var, "CH7B3_TESTNET_GOLDEN"], true, H_END.0);
    }
    for to in [H_END.0 - 1, H_END.0 + 1, V20, END.0] {
        refused(&CH7A5B_VARS, true, to);
    }
    refused(&["CH7B3_TESTNET_PAYMENTS"], true, H_END.0);
    refused(&["CH7B3_OWNER_REPORT"], true, H_END.0);
    refused(&["CH7B3_TESTNET_PAYMENTS_SUMS"], true, H_END.0);
    refused(&[], true, V20);
    refused(&["CH7B3_TESTNET_GOLDEN"], true, END.0);
    refused(&c9b, true, V20);

    // CH7b-3b C14: the state-hash directory goes with R-A's set, to END only.
    let rb = [
        "CH7B3_TESTNET_GOLDEN",
        "CH7B3_TESTNET_PAYMENTS",
        "CH7B3_OWNER_REPORT",
        "CH7B3_STATE_HASH_DIR",
    ];
    assert_eq!(mix(&rb, true, END.0), None, "R-B's set to END");
    for to in [END.0 - 1, V20, H_END.0] {
        refused(&rb, true, to);
        refused(&["CH7B3_STATE_HASH_DIR"], true, to);
    }
    refused(&["CH7B3_STATE_HASH_DIR"], true, END.0);
    refused(&rb, false, END.0);
    refused(&[CH7A5B_VARS[0], CH7A5B_VARS[1], "CH7B3_STATE_HASH_DIR"], true, H_END.0);
}

/// A fresh directory at mode 0700, as C14's runner makes its own.
fn owner_only_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// C14's log writes `muhash` at the plan's S, its 24 heights: the activation heights and every
/// 100,000th block to END.
#[test]
fn the_state_hash_mode_selects_plan_s() {
    let dir = owner_only_dir();
    let mode = StateHash::at(dir.path().to_owned()).expect("an owner-only directory");
    let (run, _sink, _report) = mode.open();
    let mut plan_s = BTreeSet::from([
        7_000, 7_300, 769_700, 770_730, 850_100, 905_100, 1_066_900, 1_148_500, 1_574_404,
    ]);
    plan_s.extend((1..=15).map(|n| n * 100_000));
    assert_eq!(plan_s.len(), 24);
    assert_eq!(run.selected(), Some(&plan_s));
    assert_eq!(run.path(), dir.path().join("log.jsonl"));
}

/// C14's sink, on a fresh testnet node: its log is the `H` and genesis's `C`, which has the 21
/// keys but `muhash`, as 0 is not in S; the log and the owner report are the owner's alone.
#[test]
fn the_state_hash_sink_writes_genesis_without_muhash() {
    use std::os::unix::fs::PermissionsExt as _;

    let logs = owner_only_dir();
    let mode = StateHash::at(logs.path().to_owned()).expect("an owner-only directory");
    let (run, sink, _report) = mode.open();
    let dir = tempfile::tempdir().unwrap();
    let flags = ["-testnet", "-printtoconsole=0"];
    let sut = NodeSut::new_with_digest(dir.path(), &flags, MOCKTIME, ui(), sink);
    let sut = sut.expect("accepted");
    let genesis = dashd_chainstate::chain::genesis_block(&sut.params.chain).hash();
    let records = run.records();
    let [Record::Header(header), Record::Outcome(c)] = records.as_slice() else {
        panic!("the H and genesis's C: {records:?}");
    };
    assert_eq!(header.chain, Chain::Test);
    assert_eq!((c.kind(), c.id.h), (Kind::Connect, 0));
    assert_eq!(c.id.b.to_string(), genesis.to_string());
    let keys: Vec<&str> = c.s().iter().map(|(spec, _)| spec.key).collect();
    let want: Vec<&str> = ALL_C_S.into_iter().filter(|&k| k != "muhash").collect();
    assert_eq!(keys, want, "genesis's keys");
    for name in STATE_HASH_FILES {
        let meta = std::fs::symlink_metadata(logs.path().join(name)).unwrap();
        assert!(meta.is_file(), "{name} is a file");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{name}'s mode");
    }
    assert_eq!(run.path(), logs.path().join(STATE_HASH_FILES[0]));
}

/// [`WINDOWS`] is the plan's table: 576 blocks each side of each activation height, DIP24's
/// 1,606 after it to cover its cycles, each activation height one of S.
#[test]
fn the_window_table_is_the_plans() {
    let plan = [
        ("DIP24", 769_124, 771_306, 769_700),
        ("V19", 849_524, 850_676, 850_100),
        ("V20", 904_524, 905_676, 905_100),
        ("MN_RR", 1_066_324, 1_067_476, 1_066_900),
        ("Withdrawals", 1_147_924, 1_149_076, 1_148_500),
    ];
    assert_eq!(WINDOWS, plan);
    assert_eq!(
        WINDOWS.map(|(_, first, last, _)| last - first + 1),
        [2_183, 1_153, 1_153, 1_153, 1_153]
    );
    for (name, first, last, at) in WINDOWS {
        let after = if name == "DIP24" {
            1_606
        } else {
            576
        };
        assert_eq!((at - first, last - at), (576, after), "{name}");
        assert!(MUHASH_AT.contains(&at), "{name}'s activation height is in S");
    }
    assert_eq!(WINDOWS[2].3, V20);
}

/// [`WindowScan`]'s rule over the windows and their neighbours: every height in each window,
/// `muhash` at each activation height and `cp` and `ehf` set from V20 passes; breaking any one
/// fails, naming it.
#[test]
fn windows_need_every_height_and_the_v20_keys() {
    // A C past V20, which is at 100 in this run: `cp` and `ehf` set, with `muhash`.
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/statedigest/regtest-v24/feature_masternode_shares/node0")
        .join("golden.jsonl.zst");
    let template = read_records(&golden)
        .into_iter()
        .find_map(|r| match r {
            Record::Outcome(o) if o.kind() == Kind::Connect && o.id.h == 200 => Some(o),
            _ => None,
        })
        .expect("a C at 200");
    for key in ["muhash", "cp", "ehf"] {
        assert!(template.s().get(key).is_some_and(|v| !v.is_null()), "the template's {key}");
    }
    let complete: Vec<Outcome> = WINDOWS
        .iter()
        .flat_map(|&(_, first, last, _)| first - 1..=last + 1)
        .map(|h| {
            let mut o = template.clone();
            o.id.h = i64::from(h);
            o
        })
        .collect();
    let scan = |edit: &dyn Fn(&mut Vec<Outcome>)| {
        let mut outcomes = complete.clone();
        edit(&mut outcomes);
        let mut seen = WindowScan::default();
        outcomes.iter().for_each(|o| seen.see(o));
        seen.check()
    };
    let at = |outcomes: &[Outcome], h: u32| {
        outcomes.iter().position(|o| o.id.h == i64::from(h)).expect("the height")
    };
    let failed = |edit: &dyn Fn(&mut Vec<Outcome>), why: &str| {
        let message = scan(edit).expect_err(why);
        assert_eq!(message, why);
    };
    // The height's `key` absent, or null.
    let removed = |h: u32, key: &'static str| {
        move |os: &mut Vec<Outcome>| {
            let i = at(os, h);
            os[i].s_mut().remove(key).expect("the key");
        }
    };
    let null = |h: u32, key: &'static str| {
        move |os: &mut Vec<Outcome>| {
            let i = at(os, h);
            os[i].s_mut().set(key, Value::Null).expect("a null");
        }
    };
    assert_eq!(scan(&|_| {}), Ok(()));

    for (name, first, last, at_h) in WINDOWS {
        let want = last - first + 1;
        for h in [first, last] {
            let why = format!("the {name} window: {} records, want {want}", want - 1);
            failed(&|os| drop(os.remove(at(os, h))), &why);
        }
        let why = format!("the {name} window: no muhash at {at_h}");
        failed(&removed(at_h, "muhash"), &why);
    }
    assert_eq!(scan(&removed(769_124, "muhash")), Ok(()), "muhash off an activation height");

    failed(&null(V20, "cp"), "905100: cp is null from V20");
    failed(&null(V20, "ehf"), "905100: ehf is null from V20");
    failed(&removed(1_149_077, "ehf"), "1149077: ehf is null from V20");
    assert_eq!(scan(&null(V20 - 1, "cp")), Ok(()), "below V20, a null cp");
    assert_eq!(scan(&null(V20 - 1, "ehf")), Ok(()), "below V20, a null ehf");
}

/// C14's directory is the owner's alone and holds none of its files yet: any group or other bit,
/// a file, a link, a missing path or a directory already holding either name, a dangling link
/// among them, is refused; a link planted after the check is refused at the open.
#[test]
fn a_shared_state_hash_dir_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;

    let chmod = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let refused = |path: &Path, why: &str| {
        let message = StateHash::at(path.to_owned()).err().unwrap_or_else(|| panic!("{why}"));
        assert!(message.contains(why), "{why}: {message}");
    };
    let root = owner_only_dir();
    let dir = root.path().join("rb");
    std::fs::create_dir(&dir).unwrap();
    for mode in [0o750, 0o770, 0o705, 0o701, 0o710, 0o740, 0o704] {
        chmod(&dir, mode);
        refused(&dir, &format!("is mode {mode:o}, not owner-only"));
    }
    chmod(&dir, 0o700);
    assert!(StateHash::at(dir.clone()).is_ok(), "an owner-only directory");

    let file = root.path().join("file");
    std::fs::write(&file, b"").unwrap();
    chmod(&file, 0o700);
    refused(&file, "is not a directory");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    refused(&link, "is not a directory");
    refused(&root.path().join("missing"), "No such file or directory");
    let owner_only = |name: String| {
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o700);
        dir
    };
    for name in STATE_HASH_FILES {
        let held = owner_only(format!("held-{name}"));
        std::fs::write(held.join(name), b"").unwrap();
        refused(&held, &format!("already holds {name}"));

        // A dangling link at the name, which `Path::exists` would call absent.
        let target = root.path().join(format!("target-{name}"));
        let linked = owner_only(format!("linked-{name}"));
        std::os::unix::fs::symlink(&target, linked.join(name)).unwrap();
        refused(&linked, &format!("already holds {name}"));

        // One planted after the check: the open refuses it, and never creates its target.
        let late = owner_only(format!("late-{name}"));
        let mode = StateHash::at(late.clone()).expect("an owner-only directory");
        std::os::unix::fs::symlink(&target, late.join(name)).unwrap();
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(mode.open())));
        let payload = opened.expect_err("the open refuses the link");
        let message = payload.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains(name) && message.contains("File exists"), "{message}");
        assert!(std::fs::symlink_metadata(&target).is_err(), "{name}'s link target is created");
    }
}

/// [`status_kb`] reads a `kB` count at its exact key only, and says why it read none; the
/// measurement lines are count-only, and none of the three ever fails the run.
#[test]
fn the_status_parser_reads_kb_lines() {
    let status = "Name:\tdashd_node\nVmPeak:\t  999 kB\nVmHWM:\t  123456 kB\n\
                  VmRSS:\t    7890 kB\nVmSwap:\tx kB\nVmLib:\t 12 MB\nThreads:\t9\n";
    assert_eq!(status_kb(status, "VmRSS"), Ok(7_890));
    assert_eq!(status_kb(status, "VmHWM"), Ok(123_456));
    assert_eq!(status_kb(status, "VmData"), Err("no such line"));
    assert_eq!(status_kb(status, "Vm"), Err("no such line"), "a prefix is not the key");
    assert_eq!(status_kb(status, "VmSwap"), Err("not a kB count"));
    assert_eq!(status_kb(status, "VmLib"), Err("not a kB count"));
    assert_eq!(status_kb(status, "Threads"), Err("not a kB count"));
    assert_eq!(status_kb("", "VmRSS"), Err("no such line"));
    assert_eq!(kb("VmRSS", Ok(7_890)), "VmRSS 7890 kB");
    assert_eq!(kb("peak", Err("clear_refs refused")), "peak not measured (clear_refs refused)");
    assert!(proc_kb("VmRSS").is_ok(), "this process's VmRSS");
    let peak = reset_peak().and_then(|()| proc_kb("VmHWM"));
    assert!(kb("peak", peak).starts_with("peak "));
}

/// R2: under C14 a hash-bearing failure and the closing line carry no value on stderr or in a
/// panic's message, and the owner report gets the values; R-A's policy prints them as before.
#[test]
fn c14_diagnostics_carry_no_values() {
    let tip: BlockHash = END.1.parse().expect("a block hash");
    let (s8, s9) = ([7_000, 7_001], [7_300]);
    let ra = summary_line(END.0, Some(tip), &s8, &s9, &[], None);
    assert_eq!(
        ra,
        format!(
            "T-TN to 1574404 ({tip}): S8 2 heights from Some(7000), S9 1 from Some(7300), \
             rotated 0 from None"
        )
    );
    let c14 = summary_line(END.0, None, &s8, &s9, &[], None);
    assert_eq!(c14, ra.replace(&format!(" ({tip})"), ""));

    let dir = owner_only_dir();
    let path = dir.path().join(STATE_HASH_FILES[1]);
    let report = OwnerReport(Arc::new(std::sync::Mutex::new(fresh(&path).unwrap())));
    let failure = |diag: &Diag| {
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            diag.fail("7: not connected", || format!("7: accepted false, tip {tip}"))
        }));
        failed.expect_err("a failure")
    };
    let printed = failure(&Diag::Print);
    assert_eq!(printed.downcast_ref::<String>(), Some(&format!("7: accepted false, tip {tip}")));
    let owner = Diag::Owner(report.clone());
    let payload = failure(&owner);
    let Some(ValueFree(text)) = payload.downcast_ref::<ValueFree>() else {
        panic!("C14's failure is ValueFree");
    };
    assert_eq!(text, "7: not connected (values in the owner report)");
    let differs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owner.eq(tip, H_END.1.parse().expect("a block hash"), "the tip at TO");
    }));
    let differs = differs.expect_err("a difference");
    let Some(ValueFree(text)) = differs.downcast_ref::<ValueFree>() else {
        panic!("C14's difference is ValueFree");
    };
    assert_eq!(text, "the tip at TO: differs (values in the owner report)");
    owner.eq(tip, tip, "the tip at TO");
    let noted = std::fs::read_to_string(&path).unwrap();
    assert_eq!(noted.lines().count(), 2, "{noted}");
    assert!(noted.contains(&format!("7: accepted false, tip {tip}")), "{noted}");
    assert!(noted.contains(&format!("the tip at TO: {tip:?}, want")), "{noted}");

    // The hook's lines: a ValueFree text as it is, any other text in the report alone.
    let at = "dashd-node/src/sut/tests/testnet.rs:1";
    let (line, note) = panic_lines(&ValueFree("7: not connected".into()), at);
    assert_eq!(
        (line.as_str(), note),
        (&*format!("T-TN C14: panicked at {at}: 7: not connected"), None)
    );
    let (line, note) = panic_lines(&format!("the located block at 7: {tip}"), at);
    assert!(!line.contains(&tip.to_string()), "{line}");
    assert_eq!(line, format!("T-TN C14: panicked at {at}; its message is in the owner report"));
    assert_eq!(note, Some(format!("panicked at {at}: the located block at 7: {tip}")));
    let (_, note) = panic_lines(&"a static message", at);
    assert_eq!(note, Some(format!("panicked at {at}: a static message")));
}

/// PLAN §5.3's T-TN.
#[test]
fn testnet_connects_through_its_first_non_null_commitments() {
    let window = std::env::var_os("CH7B2_TESTNET_WINDOW").map(PathBuf::from);
    // Without the window the run skips or is refused whatever TO says, so it is read only with it.
    let to = if window.is_some() {
        std::env::var("CH7B2_TESTNET_TO").map_or(H_END.0, |t| t.parse().expect("a height"))
    } else {
        H_END.0
    };
    let given = |var: &str| std::env::var_os(var).is_some();
    if let Some(why) = refused_mix(given, window.is_some(), to) {
        panic!("T-TN's variables: {why}");
    }
    let compare = Compare::from_env();
    let state_hash = StateHash::from_env();
    let Some(window) = window else {
        assert!(
            compare.is_none(),
            "the state-hash compare (CH7A5B_TESTNET_GOLDEN, CH7A5B_OWNER_REPORT) needs \
             CH7B2_TESTNET_WINDOW"
        );
        eprintln!(
            "\n*** SKIPPED testnet_connects_through_its_first_non_null_commitments: \
             CH7B2_TESTNET_WINDOW is not set. Set it to a directory whose blocks/ holds a \
             testnet datadir's block files (PLAN §5.3). ***\n"
        );
        return;
    };
    if compare.is_some() {
        assert_eq!(to, H_END.0, "the state-hash compare spans 0..7,570 only");
    }
    if state_hash.is_some() {
        assert_eq!(to, END.0, "the state-hash compare (C14) spans 0..=1,574,404 only");
    }
    // C14's outputs, created before any input is read, and its diagnostics policy (R2): from
    // here on every panic's message goes to the owner report.
    let c14 = state_hash.as_ref().map(StateHash::open);
    let diag = c14.as_ref().map_or(Diag::Print, |(_, _, report)| Diag::Owner(report.clone()));
    let _panics = c14.as_ref().map(|(_, _, report)| owner_panics(report.clone()));
    // The slice's pins before the connect.
    let slice_records = compare.as_ref().map(Compare::pinned_slice);
    let probe = tempfile::tempdir().unwrap();
    let flags = ["-testnet", "-printtoconsole=0"];
    let p = NodeSut::new(probe.path(), &flags, MOCKTIME, ui()).unwrap().params.chain.clone();
    let genesis = dashd_chainstate::chain::genesis_block(&p).hash();
    let golden = std::env::var_os("CH7B3_TESTNET_GOLDEN").map(PathBuf::from);
    assert!(to < V20 || golden.is_some(), "a run to V20 or past it needs CH7B3_TESTNET_GOLDEN");
    let payments = std::env::var_os("CH7B3_TESTNET_PAYMENTS").map(PathBuf::from);
    assert!(to != END.0 || payments.is_some(), "a run to END needs CH7B3_TESTNET_PAYMENTS");
    let chain = locate(&window.join("blocks"), p.message_start, genesis, to);
    let hash = |h: u32| chain[h as usize].0;
    for (h, want) in [FIRST_QC, H_END].into_iter().filter(|&(h, _)| h <= to) {
        diag.eq(hash(h).to_string().as_str(), want, &format!("the located block at {h}"));
    }
    if let (Some(compare), Some(golden)) = (&compare, &slice_records) {
        compare.golden_is_the_located_chain(&projected("golden", golden), &chain);
    }
    if let Some(golden) = &golden {
        golden_is_the_located_chain(golden, &chain);
    }
    let fixtures = match &payments {
        Some(dir) => {
            let report = std::env::var_os("CH7B3_OWNER_REPORT").expect("CH7B3_OWNER_REPORT");
            let sums = std::env::var("CH7B3_TESTNET_PAYMENTS_SUMS");
            let sums = sums.as_deref().unwrap_or(PAYMENTS_SUMS);
            Some(Fixtures::load(dir, sums, Path::new(&report), to, hash))
        }
        None => {
            eprintln!(
                "\n*** SKIPPED C9b: CH7B3_TESTNET_PAYMENTS is not set, so no masternode \
                 payments are compared. Set it to SW-V24's payments fixtures (CH7b-3 PLAN §5.1, \
                 DEC-1146). ***\n"
            );
            None
        }
    };

    // A day past the last block, so none is too far ahead of the clock.
    let last = chain.last().unwrap().1.read();
    let mocktime = i64::from(last.header().time()) + 86_400;
    let dir = tempfile::tempdir().unwrap();
    let argv = ["-testnet", "-printtoconsole=0", "-assumevalid=0"];
    let (mut sut, state_log) = match (&compare, c14) {
        (Some(compare), _) => {
            let (run, sink) = DigestRun::on(&compare.owner, MuHashAt::Every, Chain::Test);
            let sut = NodeSut::new_with_digest(dir.path(), &argv, mocktime, owner_ui(), sink);
            (sut.expect("accepted"), Some(run))
        }
        (None, Some((run, sink, report))) => {
            let ui = aborts_ui(report);
            let sut = NodeSut::new_with_digest(dir.path(), &argv, mocktime, ui, sink);
            (sut.expect("accepted"), Some(run))
        }
        (None, None) => (NodeSut::new(dir.path(), &argv, mocktime, ui()).expect("accepted"), None),
    };
    imported(&mut sut);
    let mut payments = fixtures.map(|f| Payments::new(f, &mut sut, to));
    let (mut s8, mut s9, mut first, mut first_rotated) = (Vec::new(), Vec::new(), None, None);
    let mut step_ten = StepTen::default();
    for (h, (hash, located)) in (0_u32..).zip(&chain).skip(1) {
        let b = located.read();
        if b.hash() != *hash {
            diag.eq(b.hash(), *hash, &format!("{h}: the located bytes"));
        }
        if let Some(version) = cbtx_version(&b) {
            s8.push(h);
            if version >= 2 {
                s9.push(h);
            }
        }
        let commitments = non_null_commitments(&b);
        if first.is_none() {
            first = commitments.first().map(|&(llmq_type, quorum, _)| (h, (llmq_type, quorum)));
        }
        if first_rotated.is_none() && commitments.iter().any(|c| INDEXED.contains(&c.2)) {
            first_rotated = Some(h);
        }
        let r = sut.chain().unwrap().process_block(Arc::clone(&b), true, None, true).recv();
        let r = r.expect("V replies");
        let connected = r.accepted && r.state.is_valid() && view_tip(&sut) == *hash;
        if !connected {
            let value_free = format!("{h}: not connected (accepted {})", r.accepted);
            diag.fail(&value_free, || {
                format!("{h}: accepted {}, {}, tip {}", r.accepted, r.state, view_tip(&sut))
            });
        }
        step_ten.connected(h, &b);
        if let Some(payments) = &mut payments {
            payments.connected(h, &b);
        }
    }
    diag.eq(view_tip(&sut), hash(to), "the tip at TO");
    match (&compare, &state_log) {
        (Some(compare), Some(run)) => {
            let slice = compare.slice.join(SLICE);
            let end = i64::from(H_END.0);
            let note = |r: &dash_statedigest::compare::Report| compare.note(&format!("{r:#?}"));
            let clean = linear_clean_noting(run, &slice, SLICE_SHA256.1, 0, end, note, |_| {});
            evo_keys_by_era(&projected("rdashd", &run.records()));
            eprintln!("T-TN state-hash compare: {} heights equal on every key", clean.pairs);
        }
        _ if state_hash.is_some() => {}
        _ => eprintln!(
            "T-TN ran without the state-hash compare: CH7A5B_TESTNET_GOLDEN and \
             CH7A5B_OWNER_REPORT are not set (CH7a-5b C4)"
        ),
    }

    let roots = counts_under(dir.path()).expect("the node's cells").root_checks();
    assert!(roots.mn_list == s8, "S8: {}", parting(&roots.mn_list, &s8));
    assert!(roots.quorums == s9, "S9: {}", parting(&roots.quorums, &s9));
    let unchecked = &roots.unchecked;
    assert!(
        unchecked.is_empty(),
        "{} judged without the flag, from {:?}",
        unchecked.len(),
        unchecked.first()
    );
    // Step 9 over rotated rows: each CbTx's quorum root from the first rotated commitment's block.
    if to >= ROTATED_BY {
        assert!(first_rotated.is_some(), "a rotated commitment by {ROTATED_BY}");
    }
    let rotated = |s: &[u32]| -> Vec<u32> {
        s.iter().copied().filter(|&h| first_rotated.is_some_and(|r| h >= r)).collect()
    };
    let (s9_rotated, checked_rotated) = (rotated(&s9), rotated(&roots.quorums));
    assert!(
        checked_rotated == s9_rotated,
        "S9 rotated: {}",
        parting(&checked_rotated, &s9_rotated)
    );
    let tip = state_hash.is_none().then(|| hash(to));
    eprintln!("{}", summary_line(to, tip, &s8, &s9, &s9_rotated, first_rotated));
    step_ten.assert_census(dir.path(), to, hash);
    if let Some(payments) = payments {
        payments.finish(&diag);
    }
    if let (Some(_), Diag::Owner(report), Some(run)) = (&state_hash, &diag, &state_log) {
        let golden = golden.as_ref().expect("CH7B3_TESTNET_GOLDEN").join(GOLDEN);
        let end = i64::from(END.0);
        let note = |r: &dash_statedigest::compare::Report| {
            report.note(&format!("{r:#?}")).expect("the owner report");
        };
        let mut scan = WindowScan::default();
        // Count-only, each never failing the run (m2): RSS before the compare, then the
        // compare's own peak.
        eprintln!("T-TN C14 end compare: {} before it", kb("VmRSS", proc_kb("VmRSS")));
        let reset = reset_peak();
        let started = std::time::Instant::now();
        let clean = linear_clean_noting(run, &golden, GOLDEN_SHA256.1, 0, end, note, |o| {
            scan.see(o);
        });
        let secs = started.elapsed().as_secs();
        let peak = reset.and_then(|()| proc_kb("VmHWM"));
        eprintln!("T-TN C14 end compare: {secs} s, its own {}", kb("peak", peak));
        if clean.muhash_equal != 24 {
            let why = format!("muhash equal at {} of S's 24 heights", clean.muhash_equal);
            diag.fail(&why, || why.clone());
        }
        if let Err(why) = scan.check() {
            let why = format!("C14's windows: {why}");
            diag.fail(&why, || why.clone());
        }
        eprintln!(
            "T-TN C14 state-hash compare: {} heights equal on the 21 keys, muhash equal at {}, \
             the 5 windows complete, cp and ehf set from V20",
            clean.pairs, clean.muhash_equal
        );
    }

    // The first non-null commitment, through route B at the tip: mined at its height, with its
    // members.
    let (mined_at, (llmq_type, quorum_hash)) = first.expect("a non-null commitment");
    assert_eq!(mined_at, FIRST_QC.0);
    let quorum = sut.chain().unwrap().on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
        let quorum = q.bind(o).quorum(llmq_type, &quorum_hash).map_err(|e| e.to_string());
        quorum.map(|got| got.map(|q| (q.mined_block, q.members.len())))
    });
    let (mined, members) = quorum.recv().expect("served").expect("no fault").expect("mined");
    diag.eq(mined, hash(FIRST_QC.0), "the first non-null commitment's mined block");
    assert!(members > 0, "a non-null commitment's quorum has members");
    eprintln!(
        "T-TN: the first non-null commitment, {llmq_type:?} at {mined_at}: {members} members"
    );
}

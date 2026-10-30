//! A segment snapshot bundle (O9b S2; R4 §2.3.4, A1 §4.6): the state a segment window starts
//! from, at its base B, with what it was checked against.
//!
//! - **Layout.** A new directory holding [`UTXO`], the UTXO snapshot `digest l2 --save-utxo`
//!   wrote (Core's `dumptxoutset` format, [`crate::digest::utxo`]), its sidecar [`SIDECAR`],
//!   [`MANIFEST`] and [`SUMS`], the sha256 of each other file in `sha256sum` format. Phase B
//!   adds the evo section, Core's `EvoSnapshot` v3 (`evo/snapshot.h`), whose hash is
//!   `GetEvoSnapshotHash` (`evo/snapshot.cpp:339`); below DIP3 the manifest's `evo` is null.
//! - **Never trusted on its own** (CH3a D166). [`make`] and [`validate`] read the snapshot whole,
//!   scan its coins into a fresh MuHash (O9b D7) and compare the scan's `muhash`, `txouts` and
//!   `amount`, and the base's block, with golden(B), and with the sidecar. `digest l2
//!   --start-utxo` checks the loaded coins against golden(B) again before V starts, and that scan
//!   is the window's MuHash seed (DEC-697 Q3).
//! - **Files are created new** and never overwritten. [`make`] builds the bundle in
//!   `.<name>.partial` beside it and renames it into place once whole, so a run killed part way
//!   leaves nothing under the bundle's name; a failed one removes the partial directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use dash_consensus::coins::MuHash3072;
use dash_statedigest::Hash256;
use serde_json::{Value as Json, json};

use crate::digest::l2::{Golden, golden_range, raw_sha256};
use crate::digest::replay::{create_new, display};
use crate::digest::utxo::{self, sidecar_path};

/// The UTXO snapshot's name in a bundle.
pub const UTXO: &str = "utxo.dat";
/// Its sidecar's.
pub const SIDECAR: &str = "utxo.dat.json";
/// The manifest's.
pub const MANIFEST: &str = "MANIFEST.json";
/// The checksums'.
pub const SUMS: &str = "SHA256SUMS";
/// The manifest's `format`.
pub const FORMAT: &str = "O9b segment snapshot bundle v1";

/// What a bundle's snapshot gives: its base and the scan of its coins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    pub height: u32,
    pub hash: Hash256,
    pub coins: usize,
    pub muhash: Hash256,
    pub txouts: u64,
    pub amount: i64,
}

impl Scan {
    fn to_json(&self) -> Json {
        json!({
            "base": {"height": self.height, "hash": self.hash.to_hex()},
            "coins": self.coins,
            "scan": {"muhash": self.muhash.to_hex(), "txouts": self.txouts, "amount": self.amount},
        })
    }
}

/// A sidecar's chain and base, and the values `--save-utxo` recorded.
struct Sidecar {
    chain: String,
    scan: Scan,
    sha256: String,
    run: Json,
}

fn read_json(path: &Path) -> Result<Json> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("{}: not JSON", path.display()))
}

fn read_sidecar(path: &Path) -> Result<Sidecar> {
    let json = read_json(path)?;
    let name = path.display();
    let field = |ptr: &str| json.pointer(ptr).with_context(|| format!("{name}: no {ptr}"));
    let text =
        |ptr: &str| field(ptr)?.as_str().with_context(|| format!("{name}: {ptr} is not text"));
    let count =
        |ptr: &str| field(ptr)?.as_u64().with_context(|| format!("{name}: {ptr} is not a count"));
    let hash = |ptr: &str| {
        Hash256::from_hex(text(ptr)?.as_bytes()).with_context(|| format!("{name}: {ptr}"))
    };
    Ok(Sidecar {
        chain: text("/chain")?.to_owned(),
        scan: Scan {
            height: u32::try_from(count("/base/height")?).context("the base's height")?,
            hash: hash("/base/hash")?,
            coins: usize::try_from(count("/coins")?).context("the coin count")?,
            muhash: hash("/scan/muhash")?,
            txouts: count("/scan/txouts")?,
            amount: field("/scan/amount")?
                .as_i64()
                .with_context(|| format!("{name}: /scan/amount is not an amount"))?,
        },
        sha256: text("/sha256")?.to_owned(),
        run: field("/run")?.clone(),
    })
}

/// Reads the snapshot at `path` whole, with its base at `height`, and scans its coins into a
/// fresh MuHash: what nothing but the file's bytes decides. Returns the scan and the file's sha256.
pub fn scan_snapshot(path: &Path, height: u32) -> Result<(Scan, String)> {
    let set = utxo::read(path, height)?;
    let mut muhash = MuHash3072::new();
    let (mut txouts, mut amount) = (0u64, 0i64);
    for (key, coin) in &set.coins {
        muhash.insert_coin(&key.outpoint(), &coin.as_ref());
        txouts += 1;
        amount = amount.checked_add(coin.value()).context("the coins' amount overflows")?;
    }
    let scan = Scan {
        height,
        hash: display(set.base),
        coins: set.coins.len(),
        muhash: Hash256::from_wire_bytes(muhash.finalize()),
        txouts,
        amount,
    };
    Ok((scan, set.sha256))
}

/// `scan` against golden(B) and `side`'s own record: the base's block, `muhash`, `txouts` and
/// `amount`, then the coin count and the file's sha256.
fn check(scan: &Scan, sha256: &str, g: &Golden, side: &Sidecar) -> Result<()> {
    let b = scan.height;
    let differ = |what: &str, have: String, want: String| -> Result<()> {
        bail!("the snapshot at {b} gives {what} {have}, {want}")
    };
    if scan.hash != g.b {
        differ("base", scan.hash.to_hex(), format!("golden({b}) has block {}", g.b.to_hex()))?;
    }
    if scan.muhash != g.muhash {
        differ("muhash", scan.muhash.to_hex(), format!("golden({b}) has {}", g.muhash.to_hex()))?;
    }
    if scan.txouts != g.txouts {
        differ("txouts", scan.txouts.to_string(), format!("golden({b}) has {}", g.txouts))?;
    }
    if scan.amount != g.amount {
        differ("amount", scan.amount.to_string(), format!("golden({b}) has {}", g.amount))?;
    }
    ensure!(*scan == side.scan, "the snapshot gives {scan:?}, its sidecar records {:?}", side.scan);
    ensure!(
        sha256 == side.sha256,
        "the snapshot's sha256 {sha256} is not the sidecar's {}",
        side.sha256
    );
    Ok(())
}

/// golden(`height`) from `golden`.
fn golden_at(golden: &Path, height: u32) -> Result<Golden> {
    golden_range(golden, height, height)?
        .remove(&height)
        .with_context(|| format!("{}: no C at {height}", golden.display()))
}

/// What [`make`] records of the run that bundles.
#[derive(Debug)]
pub struct Made {
    pub dir: PathBuf,
    pub scan: Scan,
}

/// The manifest's `utxo` section for a snapshot giving `scan`, with sha256 `sha256`.
fn utxo_section(scan: &Scan, sha256: &str) -> Json {
    let s = scan.to_json();
    json!({"file": UTXO, "sha256": sha256, "sidecar": SIDECAR, "coins": s["coins"],
           "scan": s["scan"]})
}

/// Bundles the snapshot `snapshot` (with its sidecar beside it), for the window crossing
/// `transition`, into `out`, a new directory: copies the two files in, then, once a scan of the
/// copied snapshot equals golden(B) of `golden` and the copied sidecar's record, writes the
/// manifest and the checksums, all in `.<name>.partial` beside `out`, renamed to `out` once whole.
/// The rename is the commit point: a step before it that fails removes the partial directory; a
/// failure to sync `out`'s parent after it leaves the complete bundle at `out` and says so. An
/// existing partial directory (a killed run's, or a concurrent maker's) is refused, never removed.
pub fn make(snapshot: &Path, golden: &Path, transition: &str, out: &Path) -> Result<Made> {
    // Built under a sibling name and renamed into place once whole, so a run killed part way
    // (where the removal below never runs) leaves no directory under the bundle's name.
    let name = out.file_name().with_context(|| format!("{}: no directory name", out.display()))?;
    let partial = out.with_file_name(format!(".{}.partial", name.to_string_lossy()));
    ensure!(!out.exists(), "creating {}: it exists", out.display());
    std::fs::create_dir(&partial).with_context(|| {
        format!(
            "creating {}: a previous run left it (killed?); remove it to rebuild",
            partial.display()
        )
    })?;
    let made = (|| {
        let copy = |from: &Path, name: &str| -> Result<()> {
            let mut file = create_new(&partial.join(name))?;
            let bytes =
                std::fs::read(from).with_context(|| format!("reading {}", from.display()))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .with_context(|| format!("writing {name}"))
        };
        copy(snapshot, UTXO)?;
        copy(&sidecar_path(snapshot), SIDECAR)?;
        // What is checked is what the bundle holds.
        let side = read_sidecar(&partial.join(SIDECAR))?;
        let g = golden_at(golden, side.scan.height)?;
        let (scan, sha256) = scan_snapshot(&partial.join(UTXO), side.scan.height)?;
        check(&scan, &sha256, &g, &side)?;
        let manifest = json!({
            "format": FORMAT,
            "chain": side.chain,
            "transition": transition,
            "base": scan.to_json()["base"],
            "utxo": utxo_section(&scan, &sha256),
            "evo": Json::Null,
            "validated": {
                "against": "golden",
                "golden_sha256": raw_sha256(golden)?,
                "keys": ["b", "muhash", "txouts", "amount"],
                "how": "a fresh MuHash over the snapshot's coins (O9b D7)",
            },
            "produced_by": side.run,
        });
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        create_new(&partial.join(MANIFEST))?.write_all(&bytes).context("writing the manifest")?;
        let mut sums = String::new();
        for name in [UTXO, SIDECAR, MANIFEST] {
            sums += &format!("{}  {name}\n", raw_sha256(&partial.join(name))?);
        }
        create_new(&partial.join(SUMS))?
            .write_all(sums.as_bytes())
            .context("writing the checksums")?;
        ensure!(!out.exists(), "creating {}: it exists", out.display());
        std::fs::rename(&partial, out).with_context(|| format!("renaming to {}", out.display()))?;
        Ok(scan)
    })();
    let scan = made.map_err(|e| match std::fs::remove_dir_all(&partial) {
        Ok(()) => e,
        Err(rm) => e.context(format!("and removing {} failed: {rm}", partial.display())),
    })?;
    File::open(out.parent().unwrap_or(Path::new("."))).and_then(|d| d.sync_all()).with_context(
        || format!("the bundle is complete at {}, but syncing its parent failed", out.display()),
    )?;
    Ok(Made {
        dir: out.to_owned(),
        scan,
    })
}

/// A checked bundle.
#[derive(Debug)]
pub struct Bundle {
    pub chain: String,
    pub transition: String,
    pub scan: Scan,
    /// The snapshot `digest l2 --start-utxo` takes.
    pub utxo: PathBuf,
}

/// Checks `dir`'s files against its checksums (exactly the bundle's files, each listed once), the
/// snapshot's checksum and the manifest's chain, base and `utxo` section against its sidecar: no
/// scan, no golden.
pub fn open(dir: &Path) -> Result<Bundle> {
    let name = dir.display();
    let sums =
        std::fs::read_to_string(dir.join(SUMS)).with_context(|| format!("{name}: {SUMS}"))?;
    let mut listed = BTreeMap::new();
    for line in sums.lines() {
        let (hex, file) =
            line.split_once("  ").with_context(|| format!("{name}: {SUMS}: {line:?}"))?;
        ensure!(
            listed.insert(file.to_owned(), hex.to_owned()).is_none(),
            "{name}: {SUMS} lists {file} twice"
        );
        let have = raw_sha256(&dir.join(file))?;
        ensure!(have == hex, "{name}: {file} has sha256 {have}, {SUMS} says {hex}");
    }
    let mut present = BTreeSet::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {name}"))? {
        present.insert(entry?.file_name().to_string_lossy().into_owned());
    }
    present.remove(SUMS);
    let want: BTreeSet<String> = [UTXO, SIDECAR, MANIFEST].map(str::to_owned).into();
    let names: BTreeSet<String> = listed.keys().cloned().collect();
    ensure!(names == want, "{name}: {SUMS} lists {names:?}, a bundle has {want:?}");
    ensure!(present == want, "{name}: holds {present:?}, a bundle has {want:?}");

    let manifest = read_json(&dir.join(MANIFEST))?;
    let text = |ptr: &str| {
        manifest.pointer(ptr).and_then(Json::as_str).with_context(|| format!("{name}: {ptr}"))
    };
    ensure!(text("/format")? == FORMAT, "{name}: not a {FORMAT}");
    let side = read_sidecar(&dir.join(SIDECAR))?;
    ensure!(text("/chain")? == side.chain, "{name}: the manifest's chain is not the sidecar's");
    ensure!(
        listed[UTXO] == side.sha256,
        "{name}: {UTXO} has sha256 {}, its sidecar records {}",
        listed[UTXO],
        side.sha256
    );
    ensure!(
        manifest.pointer("/utxo") == Some(&utxo_section(&side.scan, &side.sha256)),
        "{name}: the manifest's utxo section is not the sidecar's"
    );
    let base = manifest.pointer("/base").cloned().unwrap_or_default();
    ensure!(
        base == side.scan.to_json()["base"],
        "{name}: the manifest's base is not the sidecar's"
    );
    ensure!(manifest.pointer("/evo") == Some(&Json::Null), "{name}: an evo section (phase B)");
    Ok(Bundle {
        chain: side.chain,
        transition: text("/transition")?.to_owned(),
        scan: side.scan,
        utxo: dir.join(UTXO),
    })
}

/// [`open`], then the snapshot scanned again and checked against golden(B) of `golden`, as
/// [`make`] checked it.
pub fn validate(dir: &Path, golden: &Path) -> Result<Bundle> {
    let bundle = open(dir)?;
    let side = read_sidecar(&dir.join(SIDECAR))?;
    let g = golden_at(golden, bundle.scan.height)?;
    let (scan, sha256) = scan_snapshot(&bundle.utxo, bundle.scan.height)?;
    check(&scan, &sha256, &g, &side)?;
    Ok(bundle)
}

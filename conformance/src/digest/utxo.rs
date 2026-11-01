//! A UTXO snapshot (CH3a D165): Core's `dumptxoutset` file of a chainstate's coins at its base
//! block, which `digest l2 --save-utxo` writes at a run's end and `--start-utxo` starts a pruned V
//! from.
//!
//! - **Format** (`rpc/blockchain.cpp:3147-3203` CreateUTXOSnapshot): `SnapshotMetadata`, the base
//!   block's hash then the coin count as a `u64` (`node/utxo_snapshot.h:38`), then each coin's
//!   `COutPoint`, its txid and `u32` index (`primitives/transaction.h:70`), and its `Coin`
//!   (`coins.h:60`). Core writes them in its coins database's key order, `'C' ‖ txid ‖
//!   VARINT(n)` (`txdb.cpp:62-68`), which past index 16511 is not the numeric order.
//! - **Reading** refuses what Core's loader refuses (`validation.cpp:6224-6240`, `:6285-6295`): a
//!   truncated coin, a coin above the base, an index of `u32::MAX`, and a whole outpoint after
//!   the last coin. It also refuses keys out of order or repeated, which Core's writer never
//!   gives, and 1 to 35 bytes after the last coin, which Core's loader ignores (its read of one
//!   more outpoint fails). The file is never trusted on its own: `digest l2` checks the loaded
//!   coins' MuHash, `txouts` and `amount` against the golden at the base before V starts.
//! - **The sidecar**, `<file>.json`, names the chain, the base, the count, the file's sha256, the
//!   coins' MuHash, `txouts` and `amount`, and the run that wrote it ([`sidecar_path`]).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_consensus::coins::Coin;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_chainstate::coins::{DB_KEY_MAX, OutPointKey};

use super::frames::Sha256Reader;

/// The file's first bytes: the base block's hash and the coin count.
// Core: node/utxo_snapshot.h:38 (SnapshotMetadata::SERIALIZE_METHODS)
const METADATA_LEN: usize = 32 + 8;

/// A coin with its outpoint, as the snapshot holds it.
pub type SnapshotCoin = (OutPointKey, Coin);

/// A snapshot read whole.
#[derive(Debug)]
pub struct Snapshot {
    /// `m_base_blockhash`.
    pub base: BlockHash,
    /// The coins, in the file's order.
    pub coins: Vec<SnapshotCoin>,
    /// The file's sha256, of its bytes as stored.
    pub sha256: String,
}

/// The coins database's key of `k` without the `'C'` every coin key shares: the file's order.
fn db_key(k: &OutPointKey) -> ([u8; DB_KEY_MAX], usize) {
    let mut buf = [0; DB_KEY_MAX];
    let len = k.db_key(&mut buf);
    (buf, len)
}

/// Writes `coins` at `base` to `file`, the new file at `path`, in the coins database's key order;
/// returns the file's sha256.
///
/// # Errors
///
/// A failed write.
pub fn write(
    file: File,
    path: &Path,
    base: BlockHash,
    mut coins: Vec<SnapshotCoin>,
) -> Result<String> {
    coins.sort_unstable_by_key(|(k, _)| db_key(k));
    let name = path.display();
    let mut out = BufWriter::new(file);
    let mut bytes = Vec::with_capacity(METADATA_LEN);
    bytes.extend_from_slice(&base.to_lendian());
    bytes.extend_from_slice(&(coins.len() as u64).to_le_bytes());
    out.write_all(&bytes).with_context(|| format!("writing {name}"))?;
    for (k, coin) in &coins {
        bytes.clear();
        bytes.extend_from_slice(&k.0);
        coin.write(&mut bytes);
        out.write_all(&bytes).with_context(|| format!("writing {name}"))?;
    }
    out.into_inner()
        .map_err(|e| e.into_error())
        .and_then(|f| f.sync_all())
        .with_context(|| format!("writing {name}"))?;
    let file = File::open(path).with_context(|| format!("opening {name}"))?;
    Sha256Reader::new(file).finish().with_context(|| format!("reading {name}"))
}

/// Reads the snapshot at `path` for a base at `base_height`, with the checks of the module
/// documentation. The file is read whole: D164's mainnet scan at 614,000 held 1,394,727 coins.
///
/// # Errors
///
/// The first check that fails, or a failed read.
pub fn read(path: &Path, base_height: u32) -> Result<Snapshot> {
    let name = path.display();
    let bytes = std::fs::read(path).with_context(|| format!("reading {name}"))?;
    let sha256 =
        Sha256Reader::new(&bytes[..]).finish().with_context(|| format!("hashing {name}"))?;
    let (meta, mut rest) =
        bytes.split_at_checked(METADATA_LEN).with_context(|| format!("{name}: no metadata"))?;
    let base = BlockHash::from_lendian(meta[..32].try_into().expect("32 bytes"));
    let count = u64::from_le_bytes(meta[32..].try_into().expect("8 bytes"));
    // At least 37 bytes a coin: the outpoint and a one-byte code (bounds a corrupt count).
    let room = rest.len() as u64 / 37;
    let mut coins = Vec::with_capacity(usize::try_from(count.min(room)).unwrap_or(0));
    let mut last: Option<([u8; DB_KEY_MAX], usize)> = None;
    for i in 0..count {
        let bad = |what: String| format!("{name}: coin {i} of {count}: {what}");
        let Some((outpoint, after)) = rest.split_first_chunk::<36>() else {
            bail!(bad("the file ends inside its outpoint".into()));
        };
        rest = after;
        let k = OutPointKey(*outpoint);
        // Core: coins.h:69 Coin::Unserialize
        let coin = Coin::decode(&mut rest).map_err(|e| anyhow!(bad(format!("{e:?}"))))?;
        // Core: validation.cpp:6234-6240
        ensure!(
            coin.height() <= base_height,
            bad(format!("height {} above the base's {base_height}", coin.height()))
        );
        ensure!(k.vout() != u32::MAX, bad("index u32::MAX".into()));
        let key = db_key(&k);
        if let Some(prev) = &last {
            let (p, here) = (&prev.0[..prev.1], &key.0[..key.1]);
            ensure!(p != here, bad(format!("{:?} repeated", k.outpoint())));
            ensure!(p < here, bad("out of the coins database's key order".into()));
        }
        last = Some(key);
        coins.push((k, coin));
    }
    // Core: validation.cpp:6285-6295 refuses a whole outpoint after the last coin; any byte here.
    ensure!(rest.is_empty(), "{name}: {} bytes after its {count} coins", rest.len());
    Ok(Snapshot {
        base,
        coins,
        sha256,
    })
}

/// The sidecar beside `snapshot`: `x.dat` gives `x.dat.json`.
pub fn sidecar_path(snapshot: &Path) -> PathBuf {
    let mut name = snapshot.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use dash_consensus::primitives::{OutPoint, TxHash};

    use super::*;

    fn coin(height: u32) -> Coin {
        Coin::new(5_000 + i64::from(height), &[0x51], height, height.is_multiple_of(2))
    }

    fn key(txid: u8, vout: u32) -> OutPointKey {
        OutPointKey::new(&OutPoint {
            hash: TxHash::from_lendian([txid; 32]),
            index: vout,
        })
    }

    fn base() -> BlockHash {
        BlockHash::from_lendian([7; 32])
    }

    /// The file's bytes, its coins in the order given, as the writer lays them out.
    fn layout(coins: &[(OutPointKey, Coin)]) -> Vec<u8> {
        let mut bytes = base().to_lendian().to_vec();
        bytes.extend_from_slice(&(coins.len() as u64).to_le_bytes());
        for (k, c) in coins {
            bytes.extend_from_slice(&k.0);
            c.write(&mut bytes);
        }
        bytes
    }

    /// CH3a D165: the writer sorts by the coins database's key, where `VARINT(16383)` = `fe7f`
    /// follows `VARINT(16512)` = `808000`; the reader gives the coins back in that order.
    #[test]
    fn a_snapshot_round_trips_in_the_coins_database_s_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("utxo.dat");
        let coins = vec![
            (key(2, 0), coin(3)),
            (key(1, 16_512), coin(4)),
            (key(1, 16_383), coin(5)),
            (key(1, 0), coin(6)),
        ];
        let sha256 = write(File::create_new(&path).unwrap(), &path, base(), coins.clone()).unwrap();
        let read = read(&path, 6).unwrap();
        assert_eq!((read.base, &read.sha256), (base(), &sha256));
        let order: Vec<(u8, u32)> = read.coins.iter().map(|(k, _)| (k.0[0], k.vout())).collect();
        assert_eq!(order, [(1, 0), (1, 16_512), (1, 16_383), (2, 0)]);
        for (k, c) in &read.coins {
            assert_eq!(Some(c), coins.iter().find(|(w, _)| w == k).map(|(_, c)| c));
        }
        // The bytes are Core's layout.
        let sorted = [3, 1, 2, 0].map(|i| coins[i].clone());
        assert_eq!(std::fs::read(&path).unwrap(), layout(&sorted));
    }

    /// CH3a D165: Core's loader refusals (`validation.cpp:6224-6240`, `:6285-6295`), and the
    /// order the writer guarantees and a trailing byte, which Core ignores, each alone.
    #[test]
    fn a_bad_snapshot_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let good = [(key(1, 0), coin(2)), (key(1, 1), coin(3))];
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("no metadata", base().to_lendian().to_vec(), "no metadata"),
            (
                "short",
                layout(&good)[..60].to_vec(),
                "coin 0 of 2: the file ends inside its outpoint",
            ),
            ("cut coin", layout(&good)[..layout(&good).len() - 1].to_vec(), "coin 1 of 2: "),
            ("outpoint after", [layout(&good), vec![0; 36]].concat(), "36 bytes after its 2 coins"),
            ("trailing", [layout(&good), vec![0]].concat(), "1 bytes after its 2 coins"),
            ("above", layout(&[(key(1, 0), coin(9))]), "coin 0 of 1: height 9 above the base's 3"),
            ("max index", layout(&[(key(1, u32::MAX), coin(2))]), "coin 0 of 1: index u32::MAX"),
            ("repeated", layout(&[good[0].clone(), good[0].clone()]), "coin 1 of 2: OutPoint"),
            ("order", layout(&[good[1].clone(), good[0].clone()]), "coin 1 of 2: out of the"),
        ];
        let path = dir.path().join("good");
        std::fs::write(&path, layout(&good)).unwrap();
        assert!(read(&path, 3).is_ok());
        for (case, bytes, want) in cases {
            let path = dir.path().join(case);
            std::fs::write(&path, bytes).unwrap();
            let e = format!("{:#}", read(&path, 3).unwrap_err());
            assert!(e.contains(want), "{case}: {e}");
        }
    }

    #[test]
    fn the_sidecar_is_beside_its_snapshot() {
        assert_eq!(sidecar_path(Path::new("/a/utxo.dat")), Path::new("/a/utxo.dat.json"));
    }
}

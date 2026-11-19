//! CH6a-2's import journal, record and manifest decoders, driven through the
//! production `Journal::open` over CutFs (an in-memory `ImportFs`) holding the
//! fuzzed bytes as `import-staging/JOURNAL`.
//!
//! Checked on every input: no panic; the decoded records re-encode to a
//! prefix of the input (every length is checked against the bytes left
//! before anything is allocated for it); a second
//! open sees the same journal and nothing was written; `repair` leaves a file
//! that reopens to the same records with nothing left to repair, and the
//! bytes it removes hold no complete frame of a decodable record at any
//! offset; a single bit flipped in the header of any record but the last
//! makes the journal corrupt; and each decoder that accepts an input
//! re-encodes it exactly.
use dash_coreimport::datadir_import::cutfs::{CutFs, Image};
use dash_coreimport::datadir_import::{ImportFs, Journal, Record, SourceManifest, frame};
use node_l1::Divergence;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

const JOURNAL: &str = "/dest/regtest/import-staging/JOURNAL";
const MAGIC: &[u8] = b"CH6AJ1";

fn fail(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

fn open(fs: &CutFs) -> Result<Option<Journal>, Divergence> {
    catch_unwind(AssertUnwindSafe(|| Journal::open(Path::new(JOURNAL), fs)))
        .map(Result::ok)
        .map_err(|_| fail("journal decoder panic"))
}

fn reencoded(j: &Journal) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for r in j.records() {
        out.extend_from_slice(&frame(&r.encode()));
    }
    out
}

/// Whether a complete frame of a decodable record starts anywhere in `tail`,
/// checked with the production encoder.
fn holds_a_frame(tail: &[u8]) -> bool {
    (0..tail.len()).any(|o| {
        let rest = &tail[o..];
        let Some(len) = rest.first_chunk::<4>() else {
            return false;
        };
        let n = u32::from_le_bytes(*len) as usize;
        let Some(end) = n.checked_add(12).filter(|end| *end <= rest.len()) else {
            return false;
        };
        let payload = &rest[8..8 + n];
        frame(payload) == rest[..end] && Record::decode(payload).is_some()
    })
}

/// Flips one header bit (chosen from the input) of a record that is not the
/// last; the journal must then be corrupt.
fn header_flip(data: &[u8], j: &Journal) -> Result<(), Divergence> {
    let n = j.records().len();
    if n < 2 {
        return Ok(());
    }
    let pick = data.iter().fold(0usize, |a, &b| a.wrapping_mul(31).wrapping_add(b as usize));
    let rec = pick % (n - 1);
    let bit = (pick / (n - 1)) % 72;
    let start =
        MAGIC.len() + j.records()[..rec].iter().map(|r| frame(&r.encode()).len()).sum::<usize>();
    let mut flipped = data.to_vec();
    flipped[start + bit / 8] ^= 1 << (bit % 8);
    let decoded = catch_unwind(|| Journal::decode(Path::new(JOURNAL), &flipped).is_ok())
        .map_err(|_| fail("journal decoder panic"))?;
    if decoded {
        return Err(fail("a header bit flip before the last record was accepted"));
    }
    Ok(())
}

fn journal(data: &[u8]) -> Result<(), Divergence> {
    let fs = CutFs::from_image(Image::default().with_file(JOURNAL, data));
    let Some(first) = open(&fs)? else {
        return Ok(());
    };
    let canonical = reencoded(&first);
    if !first.records().is_empty() && !data.starts_with(&canonical) {
        return Err(fail("decoded records are not a prefix of the input"));
    }
    let second = open(&fs)?.ok_or_else(|| fail("journal reopen failed"))?;
    if second.records() != first.records()
        || second.needs_repair() != first.needs_repair()
        || second.ignored_tail() != first.ignored_tail()
        || fs.ops() != 0
        || fs.read(Path::new(JOURNAL)).ok().as_deref() != Some(data)
    {
        return Err(fail("journal open is not read-only or not idempotent"));
    }
    if first.needs_repair() {
        let mut repaired = first.clone();
        catch_unwind(AssertUnwindSafe(|| repaired.repair(&fs)))
            .map_err(|_| fail("journal repair panic"))?
            .map_err(|_| fail("journal repair failed"))?;
        let after = open(&fs)?.ok_or_else(|| fail("repaired journal does not open"))?;
        if after.records() != first.records() || after.needs_repair() {
            return Err(fail("repair changed the records or left a torn tail"));
        }
        if fs.read(Path::new(JOURNAL)).ok().as_ref() != Some(&canonical) {
            return Err(fail("repair did not leave exactly the decoded records"));
        }
        if data.len() > canonical.len()
            && data.starts_with(&canonical)
            && holds_a_frame(&data[canonical.len()..])
        {
            return Err(fail("repair removed a complete frame"));
        }
    }
    header_flip(data, &first)
}

fn record(data: &[u8]) -> Result<(), Divergence> {
    let decoded =
        catch_unwind(|| Record::decode(data)).map_err(|_| fail("record decoder panic"))?;
    match decoded {
        Some(r) if r.encode() != data => Err(fail("record does not re-encode exactly")),
        _ => Ok(()),
    }
}

fn manifest(data: &[u8]) -> Result<(), Divergence> {
    let decoded = catch_unwind(|| SourceManifest::decode(data))
        .map_err(|_| fail("manifest decoder panic"))?;
    match decoded {
        Some(m) if m.encode() != data => Err(fail("manifest does not re-encode exactly")),
        _ => Ok(()),
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    journal(data)?;
    // The same bytes as one record payload and as one manifest, so the
    // inner decoders see inputs that did not pass a frame's crc.
    let body = data.strip_prefix(MAGIC).unwrap_or(data);
    record(body)?;
    manifest(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_coreimport::datadir_import::{
        FileMethod, Intent, ManifestEntry, Method, PlacementPolicy, RecordBody, ReportedStatus,
        SourceBinding, SourceMode,
    };
    use std::path::PathBuf;

    const SEEDS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/public-seeds/coreimport_journal");

    fn records() -> Vec<Vec<u8>> {
        let m0 = SourceManifest::from_entries(vec![ManifestEntry {
            path: PathBuf::from("blocks/blk00000.dat"),
            size: 96,
            modified: 1,
            dev: 7,
            ino: 11,
            hash: [3; 32],
            nlink: 1,
        }]);
        let digest = m0.digest;
        [
            RecordBody::Intent(Box::new(Intent {
                source: SourceBinding {
                    path: PathBuf::from("/core/regtest"),
                    dev: 7,
                    ino: 2,
                },
                datadir: PathBuf::from("/core"),
                mode: SourceMode::Lock,
                placement: PlacementPolicy::Hardlink,
                best_block: Some([0xbb; 32]),
                last_file: Some(1),
                m0,
            })),
            RecordBody::Staged(vec![FileMethod {
                name: PathBuf::from("blk00000.dat"),
                method: Method::Hardlink,
                reflink_errno: 95,
                link_errno: 0,
            }]),
            RecordBody::SourceVerified {
                m1_digest: digest,
            },
            RecordBody::Placed,
            RecordBody::ReindexArmed,
            RecordBody::Reported(ReportedStatus::Complete),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            frame(
                &Record {
                    at_ms: 1_700_000_000_000 + i as u64,
                    body,
                }
                .encode(),
            )
        })
        .collect()
    }

    /// The seed files: `phase-<n>-<name>` holds the records through phase n.
    fn seeds() -> Vec<(String, Vec<u8>)> {
        let recs = records();
        let names = ["intent", "staged", "source-verified", "placed", "reindex-armed", "reported"];
        let mut out = vec![("magic".to_owned(), MAGIC.to_vec())];
        for (i, name) in names.iter().enumerate() {
            let mut bytes = MAGIC.to_vec();
            for r in &recs[..=i] {
                bytes.extend_from_slice(r);
            }
            out.push((format!("phase-{i}-{name}"), bytes));
        }
        let through = |n: usize| {
            let mut b = MAGIC.to_vec();
            recs[..n].iter().for_each(|r| b.extend_from_slice(r));
            b
        };
        let mut torn = through(1);
        torn.extend_from_slice(&recs[1][..recs[1].len() - 3]);
        out.push(("torn-tail".into(), torn));
        let mut zero = through(3);
        zero.extend_from_slice(&[0; 9]);
        out.push(("zero-tail".into(), zero));
        let mut bad = through(4);
        let at = MAGIC.len() + recs[0].len() + 10;
        bad[at] ^= 1;
        out.push(("bad-frame".into(), bad));
        let mut trailing = through(6);
        trailing.extend_from_slice(&[0xff; 5]);
        out.push(("reported-trailing".into(), trailing));
        out.push(("magic-prefix".into(), b"CH6A".to_vec()));
        let mut flip = through(5);
        flip[MAGIC.len() + recs[0].len() + 1] ^= 0x10;
        out.push(("header-flip".into(), flip));
        let mut zero_head = through(2);
        let at = MAGIC.len() + recs[0].len();
        let mut partial = zero_head.clone();
        zero_head[at..at + 9].fill(0);
        out.push(("zero-head".into(), zero_head));
        partial[at..at + 6].fill(0);
        out.push(("partial-zero-head".into(), partial));
        out
    }

    fn seed<'a>(seeds: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
        let Some((_, bytes)) = seeds.iter().find(|(n, _)| n == name) else {
            panic!("no seed {name}");
        };
        bytes
    }

    fn open_journal(fs: &CutFs) -> Journal {
        let Ok(j) = Journal::open(Path::new(JOURNAL), fs) else {
            panic!("journal does not open");
        };
        j
    }

    #[test]
    fn production_decoder_is_total() {
        for (name, bytes) in seeds() {
            assert!(check(&bytes).is_ok(), "{name}");
        }
        assert!(check(b"").is_ok());
        assert!(check(&[0xff; 64]).is_ok());
        let seeds = seeds();
        let open = |name: &str| Journal::decode(Path::new(JOURNAL), seed(&seeds, name));
        assert!(open("header-flip").is_err());
        let Ok(zero_head) = open("zero-head") else {
            panic!("zero-head does not decode");
        };
        assert_eq!(zero_head.records().len(), 1);
        assert!(zero_head.needs_repair());
        assert!(open("partial-zero-head").is_err());
    }

    #[test]
    fn seeds_are_checked_in_and_reach_each_phase() {
        for (name, bytes) in seeds() {
            let path = Path::new(SEEDS).join(&name);
            assert_eq!(
                std::fs::read(&path).ok().as_deref(),
                Some(&bytes[..]),
                "{name}: run write_seeds"
            );
        }
        let fs = CutFs::from_image(Image::default().with_file(JOURNAL, &seeds()[6].1));
        assert_eq!(open_journal(&fs).records().len(), 6);
        let provenance = include_str!("../../public-seeds/PROVENANCE.md");
        for (name, _) in seeds() {
            assert!(
                provenance.contains(&format!("`coreimport_journal/{name}`")),
                "{name} has no provenance line"
            );
        }
    }

    /// `cargo test -p node-fuzz --lib coreimport_journal -- --ignored write_seeds`
    #[test]
    #[ignore]
    fn write_seeds() {
        for (name, bytes) in seeds() {
            if std::fs::write(Path::new(SEEDS).join(&name), bytes).is_err() {
                panic!("{name}: write failed");
            }
        }
    }

    #[test]
    fn broken_inputs_are_rejected_without_writes() {
        let seeds = seeds();
        let fs = CutFs::from_image(Image::default().with_file(JOURNAL, seed(&seeds, "bad-frame")));
        assert!(Journal::open(Path::new(JOURNAL), &fs).is_err());
        assert_eq!(fs.ops(), 0);
        let fs = CutFs::from_image(Image::default().with_file(JOURNAL, seed(&seeds, "torn-tail")));
        let mut j = open_journal(&fs);
        assert!(j.needs_repair());
        assert!(j.repair(&fs).is_ok());
        let Ok(repaired) = fs.read(Path::new(JOURNAL)) else {
            panic!("repaired journal does not read");
        };
        assert_eq!(repaired.len(), MAGIC.len() + records()[0].len());
    }
}

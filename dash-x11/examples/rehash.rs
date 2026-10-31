//! Re-hashes real Dash headers with every implementation this CPU runs and checks them against
//! Core's hashes. Read-only on its inputs.
//!
//! ```text
//! rehash chain <headers.bin> <best-hash>
//! rehash blk <blocks-dir> <magic-hex> <harness>
//! ```
//!
//! `chain`: a file of consecutive 80-byte headers from height 0 (as dumped from a node with
//! `getblockheader <hash> false`). Header i's hash must be header i+1's `hashPrevBlock`, which
//! Core's node checked when it accepted the header, and the last one's must be `<best-hash>`
//! (`getbestblockheader`'s hash in Core's display order).
//!
//! `blk`: every block record in `<blocks-dir>/blk*.dat` (Core's block files, `magic`, size,
//! block; a record cut short at the end of the newest file is skipped). Every header is hashed
//! here and by `<harness>` (`contrib/core-harness`, `headers` command) and the two must agree;
//! the example also reports how many records link to a parent in the files.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use dash_x11::Implementation;

#[path = "../tests/common/mod.rs"]
mod common;

use common::{display, implementations, unhex};

/// The hash of `header` under every implementation, which must agree.
fn hash_all(imps: &[&Implementation], header: &[u8]) -> [u8; 32] {
    let h = imps[0].x11(header);
    for imp in &imps[1..] {
        assert_eq!(imp.x11(header), h, "{} disagrees on {}", imp.name(), display(&h));
    }
    h
}

fn chain(path: &str, best: &str) {
    let data = std::fs::read(path).unwrap();
    assert!(data.len().is_multiple_of(80) && !data.is_empty(), "{path} is not whole headers");
    let imps = implementations();
    let headers: Vec<&[u8; 80]> = data.as_chunks::<80>().0.iter().collect();
    for (height, pair) in headers.windows(2).enumerate() {
        let h = hash_all(&imps, pair[0]);
        assert_eq!(&h[..], &pair[1][4..36], "height {height}: hash is not the next prev-hash");
    }
    let tip = hash_all(&imps, headers[headers.len() - 1]);
    assert_eq!(display(&tip), best, "the last header is not the best header");
    let names: Vec<_> = imps.iter().map(|i| i.name()).collect();
    println!(
        "{path}: {} headers (heights 0..={}) hash to their successors' prev-hashes and the best \
         header {best} under {names:?}",
        headers.len(),
        headers.len() - 1
    );
}

fn blk(dir: &str, magic: &str, harness: &str) {
    let magic = unhex(magic);
    assert_eq!(magic.len(), 4, "the magic is 4 bytes");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with("blk"))
        .collect();
    files.sort();
    let mut headers = Vec::new();
    let mut cut = 0;
    for file in &files {
        let data = std::fs::read(file).unwrap();
        let mut pos = 0;
        while pos + 8 <= data.len() {
            if data[pos..pos + 4] != magic[..] {
                // Core pre-allocates block files with zeros past the last record.
                assert!(
                    data[pos..].iter().all(|&b| b == 0),
                    "{}: bad magic at {pos}",
                    file.display()
                );
                break;
            }
            let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            assert!(size >= 80, "{}: a {size}-byte record at {pos}", file.display());
            if pos + 8 + size > data.len() {
                cut += 1;
                break;
            }
            headers.extend_from_slice(&data[pos + 8..pos + 88]);
            pos += 8 + size;
        }
    }
    let n = headers.len() / 80;
    let mut child = Command::new(harness)
        .arg("headers")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = headers.clone();
    let writer = std::thread::spawn(move || stdin.write_all(&input).unwrap());
    let mut core = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut core).unwrap();
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(core.len(), 32 * n);
    let imps = implementations();
    let mut hashes = HashSet::new();
    for (i, header) in headers.as_chunks::<80>().0.iter().enumerate() {
        let h = hash_all(&imps, header);
        assert_eq!(&h[..], &core[32 * i..32 * i + 32], "record {i}: differs from Core");
        hashes.insert(h);
    }
    let linked = headers.as_chunks::<80>().0.iter().filter(|h| hashes.contains(&h[4..36])).count();
    let genesis = headers.as_chunks::<80>().0.iter().filter(|h| h[4..36] == [0; 32]).count();
    let names: Vec<_> = imps.iter().map(|i| i.name()).collect();
    println!(
        "{dir}: {n} block records in {} files ({cut} cut short) match Core under {names:?}; \
         {linked} have their parent in the files, {genesis} are a genesis block",
        files.len()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[1..] {
        ["chain", path, best] => chain(path, best),
        ["blk", dir, magic, harness] => blk(dir, magic, harness),
        _ => {
            eprintln!(
                "usage: rehash chain <headers.bin> <best-hash> | blk <dir> <magic> <harness>"
            );
            std::process::exit(2);
        }
    }
}

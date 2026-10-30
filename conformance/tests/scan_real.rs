//! The scan's classifiers on real mainnet blocks (`tests/fixtures/o6-scan/PROVENANCE.md`): the
//! three pre-DIP3 coinbases with type bytes (scan b), and the first block of an old budget
//! window with the block before it (scan c).

use dash_conformance::check::rev_hex;
use dash_conformance::scan::{
    BlockInput, BlockResult, CClass, CRowKind, Cfg, Net, ScanSet, classify_c, scan_block,
};
use dash_conformance::split::split_block;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/o6-scan/{name}", env!("CARGO_MANIFEST_DIR"));
    hex_conservative::decode_to_vec(std::fs::read_to_string(path).unwrap().trim()).unwrap()
}

fn run(cfg: &Cfg, height: u32, bytes: &[u8]) -> BlockResult {
    scan_block(
        cfg,
        &BlockInput {
            bytes,
            height,
            hash: [0; 32],
            stale: false,
            mtp_prev: None,
        },
    )
}

#[test]
fn the_three_pre_dip3_coinbases_with_type_bytes_are_scan_b_hits() {
    let cfg = Cfg::new(Net::Mainnet, ScanSet::parse("b").unwrap());
    let cases = [
        (842_284, "53fa8159168128ed8c0d31c2c2b6466f32ca27a69ea6fb114c1655bdfc734ac6"),
        (844_394, "c811fe2d7b02261e1e0f48f731549d7cef891a27a4595253a44308e58d6ba6b2"),
        (866_404, "5a78111e6f647204eb25744e022b906342ae4f3563794d1c2383e96a562245fc"),
    ];
    for (height, txid) in cases {
        let block = fixture(&format!("mainnet-{height}.block.hex"));
        let coinbase = fixture(&format!("mainnet-{height}.coinbase.hex"));
        let spans = split_block(&block).unwrap();
        assert_eq!(&block[spans.txs[0].range.clone()], &coinbase[..], "{height}");
        assert_eq!(&coinbase[..4], &[0, 0, 0, 0x20], "raw nVersion 0x20000000");

        let res = run(&cfg, height, &block);
        let hits: Vec<_> = res.hits.iter().filter(|h| h.class == "b.pre_dip3").collect();
        assert_eq!(hits.len(), 1, "{height}: {:?}", res.hits);
        let hit = hits[0];
        assert_eq!(hit.tx_index, Some(0));
        assert_eq!(hit.txid.as_ref().map(rev_hex).as_deref(), Some(txid));
        assert_eq!(hit.detail["type"], 0x2000);
        assert_eq!(hit.detail["version"], 0);
        assert!(res.hits.iter().all(|h| h.class != "b.post_dip3"), "{height}");

        // The same bytes at a height at or after DIP3 are not a pre-DIP3 hit.
        let after = run(&cfg, 1_028_160, &block);
        assert!(after.hits.iter().all(|h| h.class != "b.pre_dip3"), "{height} after DIP3");
    }
}

#[test]
fn the_first_block_of_a_budget_window_overpays_and_the_block_before_is_no_candidate() {
    let cfg = Cfg::new(Net::Mainnet, ScanSet::parse("c").unwrap());
    let window = run(&cfg, 332_320, &fixture("mainnet-332320.block.hex"));
    let row = window.c_row.expect("332320 opens a window");
    assert_eq!(row.kind, CRowKind::Window);
    assert_eq!(row.coinbase_value, 118_108_031_847);
    // the oracle's subsidy + totalfee for the block
    let reward = 508_031_847;
    assert_eq!(classify_c(&cfg, &row, reward, None), CClass::C1WindowOverpay);
    assert_eq!(classify_c(&cfg, &row, row.coinbase_value as i64, None), CClass::Within);

    let before = run(&cfg, 332_319, &fixture("mainnet-332319.block.hex"));
    assert!(before.c_row.is_none(), "332319 is outside every window");
    assert!(before.hits.is_empty());
}

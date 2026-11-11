//! The growth guard must reject recordings that do not exercise its workload.
#[path = "../benches/support/deep_window.rs"]
mod deep_window;
#[path = "../benches/support/evo_node.rs"]
mod evo_node;
#[path = "../benches/support/recorded.rs"]
mod recorded;

#[test]
fn recording_header_applies_every_chain_option() {
    let text = "# Dash Core version v24.0.0\n# dashd -regtest -dip3params=2:2 -testactivationheight=v20@1000000 -testactivationheight=mn_rr@1000000 -testactivationheight=dip0024@1000000 -vbparams=testdummy:-2:0\n";
    let p = recorded::params(recorded::header_args(text).split_whitespace());
    assert_eq!(p.consensus.dip0003_height.to_u32(), 2);
    assert_eq!(p.consensus.v20_height.to_u32(), 1_000_000);
    assert_eq!(p.consensus.dip0024_height.to_u32(), 1_000_000);
    assert_eq!(p.consensus.mn_rr_height.to_u32(), 1_000_000);
}

#[test]
#[should_panic(expected = "the recording's chain arguments")]
fn missing_chain_header_is_refused() {
    recorded::header_args("# no chain parameters\n");
}

#[test]
#[should_panic(expected = "one chain-arguments header")]
fn duplicate_chain_header_is_refused() {
    recorded::header_args("# dashd -regtest -dip3params=2:2\n# dashd -regtest -dip3params=48:48\n");
}

#[test]
#[should_panic(expected = "non-null QCs at 59")]
fn null_commitment_precursor_is_refused_for_the_full_run() {
    let blocks = evo_node::decode(include_str!("data/ch7a4b/blocks.txt"));
    deep_window::validate_segment(&blocks[48..], 49);
}

#[test]
fn precursor_header_preserves_chain_parameters() {
    let text = "# dashd -regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000000 -testactivationheight=v20@1000000 -testactivationheight=mn_rr@1000000\n";
    let p = recorded::params(recorded::header_args(text).split_whitespace());
    assert_eq!(p.consensus.dip0003_height.to_u32(), 48);
    assert_eq!(p.consensus.v20_height.to_u32(), 1_000_000);
    assert_eq!(p.consensus.dip0024_height.to_u32(), 1_000_000);
    assert_eq!(p.consensus.mn_rr_height.to_u32(), 1_000_000);
}

#[test]
fn recorded_non_null_segment_has_all_three_types() {
    let blocks = evo_node::decode(include_str!("data/deep_window_block.txt"));
    assert_eq!(deep_window::validate_segment(&blocks, 19_115), 3);
    assert_eq!(deep_window::non_null_commitments(&blocks), 3);
}

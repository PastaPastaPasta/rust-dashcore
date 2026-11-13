//! Core's verdict on a `diff_contextual_check_block_header` input is the same with the last
//! [`Network::history`](node_l1::check_inputs::Network::history) headers of its window as with
//! the whole window: the oracle reads none of the headers the input leaves out.
//! (dash-consensus's `checks_header_windows` checks the kept headers and the node's verdicts.)

use node_l1::check_inputs::{HeaderInput, Input, header_window_inputs};
use node_l1::checks::{CoreCheck, core_check};

/// Core's verdict, `None` when the oracle gives none.
fn core(input: HeaderInput) -> Option<dash_oracle_sys::Verdict> {
    match core_check(&Input::ContextualHeader(input)) {
        Ok(CoreCheck::Verdict(verdict)) => Some(verdict),
        _ => None,
    }
}

#[test]
fn core_reads_only_the_kept_window() {
    let (mut verdicts, mut differing) = (0, Vec::new());
    for bytes in header_window_inputs() {
        let kept = core(HeaderInput::parse(&bytes).expect("an input"));
        let whole = core(HeaderInput::whole(&bytes).expect("an input"));
        if kept != whole {
            differing.push(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        verdicts += usize::from(kept.is_some());
    }
    assert!(differing.is_empty(), "Core's verdict differs on {differing:?}");
    assert!(verdicts > 60, "{verdicts} verdicts");
}

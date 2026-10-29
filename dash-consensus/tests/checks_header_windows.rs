//! A `diff_contextual_check_block_header` input keeps only the last [`Network::history`] headers
//! of the window it selects: byte for byte the whole window's last headers, so the checked
//! header, the clock and the node's verdict are those of the whole window. (node-l1's
//! `l1_header_windows` asks Core the same of its verdicts.)

#[path = "support/checks.rs"]
mod checks;

use checks::{HeaderInput, Input, header_window_inputs};

#[test]
fn a_kept_window_is_the_whole_windows_tail() {
    let mut cut = 0;
    for bytes in header_window_inputs() {
        let kept = HeaderInput::parse(&bytes).expect("an input");
        let whole = HeaderInput::whole(&bytes).expect("an input");
        let at = format!("input {}", hex(&bytes));
        let dropped = whole.window.headers.len() - kept.window.headers.len();
        assert_eq!(kept.window.first_height, whole.window.first_height + dropped as u32, "{at}");
        assert!(kept.window.headers == whole.window.headers[dropped..], "{at}");
        assert!((kept.header, kept.now) == (whole.header, whole.now), "{at}");
        let ours = Input::ContextualHeader(kept).ours();
        assert!(ours == Input::ContextualHeader(whole).ours(), "{at}");
        cut += usize::from(dropped > 0);
    }
    assert!(cut > 40, "{cut} windows cut");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

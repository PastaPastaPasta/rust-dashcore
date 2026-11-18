//! Prints both sides of `diff_dml_apply` (`node_l1::dml_apply::explain`) or `mnlist_payee_queue`
//! (`node_l1::dml_payee::explain`) inputs: one hex input per argument, or the contents of each
//! file argument.
//!
//! `cargo run -p node-l1 --features oracle --example dml_explain -- <apply|payee> <hex|file>...`

fn main() {
    let mut args = std::env::args().skip(1);
    let explain: fn(&[u8]) -> String = match args.next().as_deref() {
        Some("apply") => node_l1::dml_apply::explain,
        Some("payee") => node_l1::dml_payee::explain,
        _ => {
            eprintln!("usage: dml_explain <apply|payee> <hex|file>...");
            std::process::exit(2);
        }
    };
    for arg in args {
        let bytes = std::fs::read(&arg).unwrap_or_else(|_| {
            let hex = arg.replace([' ', ',', '[', ']'], "");
            (0..hex.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                .collect()
        });
        println!("{}\n", explain(&bytes));
    }
}

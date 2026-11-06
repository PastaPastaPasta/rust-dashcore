//! Runs one of the bench workloads a given number of times, for instruction counters:
//! `workload <name> <iterations>`, or `workload --list`. See `contrib/ins-per-byte.sh`.

#[path = "../benches/workloads/mod.rs"]
mod workloads;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut all = workloads::workloads();
    if args.first().map(String::as_str) == Some("--list") {
        for w in &all {
            println!("{} {}", w.name, w.bytes);
        }
        return;
    }
    let [name, iterations] = &args[..] else {
        eprintln!("usage: workload <name> <iterations> | --list");
        std::process::exit(2);
    };
    let iterations: u64 = iterations.parse().expect("iterations");
    let w =
        all.iter_mut().find(|w| w.name == *name).unwrap_or_else(|| panic!("no workload {name}"));
    for _ in 0..iterations {
        w.run();
    }
}

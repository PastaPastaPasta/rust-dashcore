//! OD-L3-22 (plan §7.2) against a real devnet dashd v24: on Core's default devnet arguments both
//! sides refuse Core's own block 1 with `bad-cb-amount` (the regression witness; O10-11's
//! devnet-genesis negative), and with the devnet leg's [`devnet::ARGS`] both start on it (op 0).
//! Its own binary: V's reason is a `tracing` line from V's thread, read by a global subscriber
//! (DEC-895). `contrib/node/l3-tier1.sh` runs it; like `l3_dashd`, it fails closed.

use std::path::Path;
use std::sync::Mutex;

use node_l1::chain::node::NodeSide;
use node_l1::l3::dashd::Process;
use node_l1::l3::devnet;
use node_l1::l3::version::{self, private_dir};

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

#[test]
fn a_devnet_needs_the_high_subsidy_args_for_block_1() {
    const T0: i64 = 1_700_000_000;
    tracing::subscriber::set_global_default(Capture).expect("the only global subscriber");
    let scratch = tempfile::tempdir().expect("a temp dir");
    let dashd = version::resolve(repo(), scratch.path()).expect("the run's dashd (plan §3.1)");
    for (name, args) in [("default", &devnet::ARGS[..1]), ("leg", &devnet::ARGS[..])] {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let datadir = private_dir(scratch.path(), &format!("dashd-{name}")).expect("a datadir");
        let node = Process::start(&dashd.binary, &datadir, &args, T0).expect("dashd started");
        // Like dashd's `Process`: a devnet that listens needs `-port` (Core's init rule).
        let v_args = [args.clone(), vec!["-listen=0".to_owned()]].concat();
        let dir = private_dir(scratch.path(), &format!("v-{name}")).expect("V's dir");
        CAPTURED.lock().expect("the log").clear();
        let v = NodeSide::with_argv(&v_args, T0, &dir, None).expect("V loaded");
        assert_eq!(v.down(), None, "{name}: V's load");
        devnet::dashd_tips(&node).expect("dashd settled");
        if name == "default" {
            let log =
                std::fs::read_to_string(datadir.join("devnet-l3/debug.log")).expect("debug.log");
            devnet::v_block_1(&v).expect("V settled on block 1");
            let v_log = CAPTURED.lock().expect("the log").clone();
            devnet::default_args_refuse_block_1(&node, &log, &v, &v_log)
                .unwrap_or_else(|e| panic!("default args: {e}"));
        } else {
            devnet::op0(&node, &v).unwrap_or_else(|e| panic!("the leg's args: {e}"));
        }
        node.stop().expect("stopped");
    }
}

/// V's ConnectTip lines (`dashd_chainstate::chain::abc`), from every thread.
static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl tracing::Subscriber for Capture {
    fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
        m.target() == "dashd_chainstate::chain::abc"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        CAPTURED.lock().expect("the log").push(m.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

//! ST3b S7's guest test: boots the VM guest, checks its RPC, submits blocks with an
//! acknowledgement after the first ones, kills QEMU, boots again on the same disk and checks C1,
//! C2 and C3 at the acknowledgement's floor, then stops the guest cleanly and checks its filesystem.
//!
//! It needs KVM, Docker, the `st3b-qemu` image and a guest `mkguest.sh` built, so it runs only with
//! `ST3B_VM=1`. `ST3B_VM_GUEST` names the guest directory; `ST3B_VM_FIXTURE` the fixture
//! (default `/work/scratch/st3b/fixtures/w-small`, whose first blocks are one chain from genesis).
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use dash_testkit::NodeRpc;
use dash_testkit::crash::checks::{Floor, c2, c3};
use dash_testkit::crash::devices::{Fs, Run, remove_own_file};
use dash_testkit::crash::fixture::Fixture;
use dash_testkit::crash::vm::{Disk, VM_BOOT_PATIENCE, Vm, guest_args, vm_c1_lines};
use serde_json::json;

/// Blocks submitted before the acknowledgement, and after it.
const ACKED: usize = 20;
const UNACKED: usize = 10;

#[test]
fn a_killed_guest_recovers_to_its_floor() {
    if std::env::var("ST3B_VM").as_deref() != Ok("1") {
        eprintln!("skipped: the guest test runs with ST3B_VM=1");
        return;
    }
    let guest = PathBuf::from(std::env::var("ST3B_VM_GUEST").expect("ST3B_VM_GUEST: a guest dir"));
    let fixture_dir = std::env::var("ST3B_VM_FIXTURE")
        .unwrap_or_else(|_| "/work/scratch/st3b/fixtures/w-small".to_owned());
    let fixture = Fixture::load(fixture_dir.as_ref()).expect("the fixture");
    let run = Run::create().expect("a run");
    let vm = Vm::new(&guest, run.id()).expect("the guest setup");
    let disk = Disk::create(&run.dir().join("data.raw"), Fs::Ext4).expect("a disk");
    let args = guest_args(&fixture, &[], &vm.rpcauth()).expect("guest args");

    let mut first = vm.boot(0, run.dir(), &disk, Fs::Ext4, &args).expect("a boot");
    let took = first.wait_rpc(VM_BOOT_PATIENCE).expect("RPC");
    eprintln!("boot 0: RPC after {:.1} s", took.as_secs_f64());
    let rpc = first.rpc().expect("a client");
    let mut failures = Vec::new();
    let mut floor = Floor::Start;
    for (i, hash) in fixture.trajectory[1..=ACKED + UNACKED].iter().enumerate() {
        let reply = rpc.call("submitblock", &[json!(fixture.blocks[hash])]).expect("submitblock");
        assert!(reply.is_null(), "submitblock {hash}: {reply}");
        if i + 1 == ACKED {
            let tip = c2(&rpc, &fixture, &mut failures).expect("an acknowledgement");
            floor = Floor::Ack(fixture.on_trajectory(&tip, 0).expect("on the trajectory"));
        }
    }
    assert_eq!(floor, Floor::Ack(ACKED));
    // The power cut: SIGKILL straight to QEMU's host process, which must still be running.
    assert!(first.power_cut().expect("the kill"), "QEMU was running");
    first.kill().expect("the container removed");
    failures.extend(vm_c1_lines(&first.console()));

    let mut second = vm.boot(1, run.dir(), &disk, Fs::Ext4, &args).expect("a second boot");
    second.wait_rpc(VM_BOOT_PATIENCE).expect("RPC after the kill");
    let rpc = second.rpc().expect("a client");
    let tip = c2(&rpc, &fixture, &mut failures).expect("C2");
    let pos = c3(&fixture, &tip, floor, &second.console()).expect("C3");
    eprintln!("recovered at {pos} (floor {ACKED}, {} submitted)", ACKED + UNACKED);
    rpc.call("stop", &[]).expect("stop");
    let code = second.wait_exit(Duration::from_secs(300)).expect("a power-off");
    failures.extend(vm_c1_lines(&second.console()));
    assert_eq!(code, Some(0), "rdashd's exit code");
    assert_eq!(failures, Vec::<String>::new());

    drop((first, second));
    // The init unmounted the disk on rdashd's exit: the filesystem checks clean.
    assert_eq!(disk.fsck(Fs::Ext4).expect("fsck"), None);
    disk.remove().expect("the disk removed");
    for boot in 0..2 {
        remove_own_file(&run.dir().join(format!("console-{boot}.log"))).expect("a console removed");
    }
    run.finish().expect("the run cleaned up");
}

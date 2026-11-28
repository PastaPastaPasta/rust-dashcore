//! The phase threads' panics reach the fail-stop hook with their reason.

use dash_llmq::dkg::session::DkgFault;

use super::describe;

#[test]
fn a_fault_and_a_panic_are_described() {
    let fault: Box<dyn std::any::Any + Send> = Box::new(DkgFault::SingleCommitmentUnverified);
    assert_eq!(describe(fault.as_ref()), "SingleCommitmentUnverified");
    let text: Box<dyn std::any::Any + Send> = Box::new("dkgdb write failed");
    assert_eq!(describe(text.as_ref()), "dkgdb write failed");
    let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("x"));
    assert_eq!(describe(owned.as_ref()), "x");
}

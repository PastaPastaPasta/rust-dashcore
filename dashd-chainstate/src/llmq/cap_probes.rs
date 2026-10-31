//! The capability rules inside the crate (CH7b-1 PLAN F2): each probe is one forbidden operation
//! in a sibling of the module that owns the capability, on the line marked `probe: <name>`. A
//! probe is dead in every build without its `cap_probe` cfg; `tests/cap_probes.rs` compiles it
//! and checks that it fails there, with its error code, and nowhere else. Its twin, the same
//! function with the marked line replaced by a permitted one, compiles cleanly. Widening the
//! item a probe uses (a field, `VContext::new`, `VContext::on_v`, the `sealed` module) makes the
//! probe compile, and the test fails. V's own module holds the probes of its context's borrow
//! (`chain/actor.rs`). rustfmt skips them, so each stays on its marked line.

#[cfg(cap_probe = "onv_fields")]
#[rustfmt::skip]
fn onv_fields(token: super::facade::ExternalToken<'_>) {
    let _ = crate::chain::OnV { token, _v_only: std::marker::PhantomData }; // probe: onv_fields
}

#[cfg(cap_probe = "onv_fields_twin")]
#[rustfmt::skip]
fn onv_fields_twin(token: super::facade::ExternalToken<'_>) {
    let _ = token.rows(); // twin: onv_fields
}

#[cfg(cap_probe = "on_v")]
#[rustfmt::skip]
fn on_v(v: &mut crate::chain::VContext, facade: &super::facade::MembersFacade) {
    let _ = v.on_v(facade); // probe: on_v
}

#[cfg(cap_probe = "on_v_twin")]
#[rustfmt::skip]
fn on_v_twin(v: &mut crate::chain::VContext, facade: &super::facade::MembersFacade) {
    let _ = facade.begin_attempt(v, crate::connect::AttemptId::next()); // twin: on_v
}

#[cfg(cap_probe = "vcontext_fields")]
#[rustfmt::skip]
fn vcontext_fields() {
    let _ = crate::chain::VContext { _v_only: std::marker::PhantomData }; // probe: vcontext_fields
}

#[cfg(cap_probe = "vcontext_fields_twin")]
#[rustfmt::skip]
fn vcontext_fields_twin() {
    let _ = std::marker::PhantomData::<crate::chain::VContext>; // twin: vcontext_fields
}

#[cfg(cap_probe = "vcontext_new")]
#[rustfmt::skip]
fn vcontext_new() {
    let _ = crate::chain::VContext::new(); // probe: vcontext_new
}

#[cfg(cap_probe = "vcontext_new_twin")]
#[rustfmt::skip]
fn vcontext_new_twin() {
    let _ = std::mem::size_of::<crate::chain::VContext>(); // twin: vcontext_new
}

// A whole literal: `..h` would move out of a `Drop` type, and borrowck's error ends the compile
// before the privacy check reports.
#[cfg(cap_probe = "handle_fields")]
#[rustfmt::skip]
fn handle_fields() -> super::facade::AttemptHandle<'static> {
    super::facade::AttemptHandle { facade: todo!(), id: todo!(), _gate: todo!(), overlay: todo!(), released: false, _v_only: todo!() } // probe: handle_fields
}

#[cfg(cap_probe = "handle_fields_twin")]
#[rustfmt::skip]
fn handle_fields_twin() -> super::facade::AttemptHandle<'static> {
    todo!() // twin: handle_fields
}

#[cfg(cap_probe = "applied_fields")]
#[rustfmt::skip]
fn applied_fields(id: crate::connect::AttemptId) {
    let _ = crate::evo::effects::EffectsApplied { attempt: id }; // probe: applied_fields
}

#[cfg(cap_probe = "applied_fields_twin")]
#[rustfmt::skip]
fn applied_fields_twin(id: crate::connect::AttemptId) {
    let _ = id.get(); // twin: applied_fields
}

#[cfg(any(cap_probe = "sealed", cap_probe = "sealed_twin"))]
struct Local;

#[cfg(cap_probe = "sealed")]
#[rustfmt::skip]
impl super::capability::sealed::Sealed for Local {} // probe: sealed

#[cfg(cap_probe = "sealed_twin")]
#[rustfmt::skip]
impl Local {} // twin: sealed

#[cfg(cap_probe = "binding_fields")]
#[rustfmt::skip]
fn binding_fields<E: dashd_store::KvEngine>(b: crate::evo::node::AttemptBinding<'_, E>) {
    let _ = crate::evo::node::AttemptBinding { ..b }; // probe: binding_fields
}

#[cfg(cap_probe = "binding_fields_twin")]
#[rustfmt::skip]
fn binding_fields_twin<E: dashd_store::KvEngine>(b: crate::evo::node::AttemptBinding<'_, E>) {
    drop(b); // twin: binding_fields
}

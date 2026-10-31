The members facade's capability rules, as code outside `dashd-chainstate` sees them (EV addendum
§2.6 compile-fail cases 1-6; CH7b-1 PLAN F2). Each forbidden operation is a `compile_fail`
example whose one visible line is the marked one. Stable rustdoc checks only that an example fails,
not its error code, so each scope also has a passing twin: the same hidden scaffolding with the
marked line replaced by a permitted one. The rules inside the crate are `tests/cap_probes.rs`'s.

# The capabilities

The scaffolding: a function given V's context, the facade, a route-A token and its rows, route B's
capability owned and borrowed, and an attempt's handle. The twin uses route B's capability as
the proof it is:

```
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
validates(o); // route B's capability, minted by V
# }
```

1. Only V mints route B's capability, through its own context.

```compile_fail,E0451
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
let _ = OnV { token, _v_only: std::marker::PhantomData }; // OnV's fields are V's
# }
```

```compile_fail,E0624
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
let _ = v.on_v(fa); // only V's loop calls it
# }
```

2. A route-A token proves nothing about V: it is no capability, no members source and no row store, and has neither call.

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
validates(&token); // not OnValidation
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
members(&token); // not a QuorumMemberSource
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
rows(&token); // not SnapshotRows
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
rows(&raw); // nor are its rows
# }
```

```compile_fail,E0599
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
token.members(); // no members call
# }
```

```compile_fail,E0599
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
token.store(); // no row store
# }
```

3. A token's rows answer `snapshot_for_block` and nothing else.

```compile_fail,E0599
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
raw.write(); // no writer
# }
```

```compile_fail,E0599
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
raw.peek(); // no cache read
# }
```

4. Neither capability, nor a borrow of one, leaves its thread.

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
send(on); // OnV is not Send
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
send(o); // nor Sync
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
send(h); // AttemptHandle is not Send
# }
```

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
send(&h); // nor Sync
# }
```

5. The capability trait is sealed.

```compile_fail,E0277
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
impl OnValidation for Local {} // Local is not sealed
# }
```

```compile_fail,E0603
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
impl dashd_chainstate::llmq::capability::sealed::Sealed for Local {} // private
# }
```

6. An attempt opens only with V's context.

```compile_fail,E0624
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
let _ = fa.begin_attempt(&mut VContext::new(), AttemptId::next()); // V's own
# }
```

```compile_fail,E0599
# #![allow(unused)]
# use dash_evo::llmq::{QuorumMemberSource, SnapshotRows};
# use dashd_chainstate::chain::{OnV, VContext};
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::capability::OnValidation;
# use dashd_chainstate::llmq::facade::{AttemptHandle, ExternalToken, MembersFacade, RawRows};
# fn validates(_: &impl OnValidation) {}
# fn members(_: &impl QuorumMemberSource) {}
# fn rows(_: &impl SnapshotRows) {}
# fn send<T: Send>(_: T) {}
# struct Local;
# fn scope<'v>(
#     v: &'v mut VContext,
#     fa: &'v MembersFacade,
#     token: ExternalToken<'v>,
#     raw: RawRows<'v>,
#     on: OnV<'v>,
#     o: &OnV<'v>,
#     h: AttemptHandle<'v>,
# ) {
let _ = token.begin_attempt(v, AttemptId::next()); // not from a token
# }
```

# A second open

An open attempt borrows V's context until its handle is gone, so a second open is refused while
the first handle lives, used or not (its drop releases the gate). The twin releases it first:

```
# #![allow(unused)]
# use dashd_chainstate::chain::VContext;
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::facade::MembersFacade;
# fn scope<'v>(fa: &'v MembersFacade, v: &'v mut VContext) {
let a = fa.begin_attempt(v, AttemptId::next());
drop(a);
let b = fa.begin_attempt(v, AttemptId::next()); // `a` released first
# }
```

```compile_fail,E0499
# #![allow(unused)]
# use dashd_chainstate::chain::VContext;
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::facade::MembersFacade;
# fn scope<'v>(fa: &'v MembersFacade, v: &'v mut VContext) {
let a = fa.begin_attempt(v, AttemptId::next());
let b = fa.begin_attempt(v, AttemptId::next()); // a second open, `a` alive
# }
```

```compile_fail,E0499
# #![allow(unused)]
# use dashd_chainstate::chain::VContext;
# use dashd_chainstate::connect::AttemptId;
# use dashd_chainstate::llmq::facade::MembersFacade;
# fn scope<'v>(fa: &'v MembersFacade, v: &'v mut VContext) {
let a = fa.begin_attempt(v, AttemptId::next());
let b = fa.begin_attempt(v, AttemptId::next()); // a second open
let _ = a.id(); // `a` used after it
# }
```

# The runtime rows

Outside the crate the runtime rows are only built, for the facade and the applier to share: a
row is read or written through a route, never on the rows themselves. The twin builds them:

```
# #![allow(unused)]
# use std::sync::Arc;
# use dash_evo::llmq::QuorumSnapshot;
# use dash_primitives::{BlockHash, LlmqType};
# use dashd_chainstate::flush::EffectLedger;
# use dashd_chainstate::llmq::rows::{RuntimeRows, SnapshotStore};
# fn scope(
#     rows: &RuntimeRows,
#     store: Arc<dyn SnapshotStore>,
#     ledger: Arc<EffectLedger>,
#     t: LlmqType,
#     base: BlockHash,
#     s: QuorumSnapshot,
# ) {
let built = RuntimeRows::new(store, ledger); // built, then handed to the facade and the applier
# }
```

```compile_fail,E0624
# #![allow(unused)]
# use std::sync::Arc;
# use dash_evo::llmq::QuorumSnapshot;
# use dash_primitives::{BlockHash, LlmqType};
# use dashd_chainstate::flush::EffectLedger;
# use dashd_chainstate::llmq::rows::{RuntimeRows, SnapshotStore};
# fn scope(
#     rows: &RuntimeRows,
#     store: Arc<dyn SnapshotStore>,
#     ledger: Arc<EffectLedger>,
#     t: LlmqType,
#     base: BlockHash,
#     s: QuorumSnapshot,
# ) {
rows.write(t, &base, &s); // no writer outside the routes
# }
```

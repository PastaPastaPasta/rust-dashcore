# repin-getters

Ports the workspace to base-sdk's getter shape: upstream #89, #92 and the follow-ups that made
the fields of `Transaction`, `TxIn`, `TxOut`, `OutPoint`, `BlockHeader`, `Block`, the ProTx
payloads, `Commitment`, `CoinbaseCommitment`, `PayloadDecode`, `DynBitset`, the `p2p_core`
messages and others private behind `getset` getters. It is re-runnable: run it again after a
rebase onto a later tip, or after a later base-sdk re-pin.

Python 3.11+, standard library only. Run it from the repository root with `python3 -I`.

## What it does

For every workspace member that reaches base-sdk, in dependency order (`order` prints it;
normal and build dependencies first, dev-dependencies where they don't form a cycle):

1. **Auto pass.** `cargo check -p <crate> --all-targets --all-features --message-format=json`,
   then apply only:
   - rustc's E0616 suggestion "a method `f` also exists, call it with parentheses", when `f` is
     the private field's own name and base-sdk declares a getter for it;
   - for a field upstream renamed (`config.toml` `[[rename]]`, E0609 "no field"), the new
     getter: `outpoint.hash` → `outpoint.txid()`;
   - MachineApplicable suggestions attached to those same errors;
   - the borrow the rewrite makes redundant: `&x.f` through a `get` getter (which returns `&T`)
     becomes `x.f()`, not `&x.f()`. That is the original expression's own type, `&T`;
   - tuple fields read through a hand-written `{ &self.0 }` / `{ self.0 }` method
     (`NetInfoV1(..).0` → `.service()`, `AddrV1.0` → `.as_bytes()`), and `make_num!` newtypes'
     `.0` → `.get()`, `T(v)` → `T::new(v)`;
   - the follow-ups of a `get` getter in a value position: `*x.f()` where rustc reports
     "can't compare `&T` with `T`" (E0277), a mismatch inside a comparison, or a mismatch where
     rustc suggests dereferencing (a `Copy` type) (exact); `.clone()`
     where it reports "expected `T`, found `&T`" (E0308) or a reference outliving a temporary
     (E0716). The clone gives the value the old code moved or copied out (recorded as `clone`).
   The suggestion is never applied to a write: a field followed by an assignment operator, by a
   `&mut self` method (`push`, `resize`, …, `MUT_METHODS`, or any `set_*`) or taken as `&mut x.f`.
   Diagnostics are read from the crate's own package only; errors from a dependency stop the
   crate (`blocked`).
2. **Scripted rewrites**, for what rustc has no suggestion for. Each one is triggered by a rustc
   error at that exact span, so a type of the same name elsewhere is never touched:
   - struct literals of a base-sdk type (E0451, E0560 rename, E0063) become `Type::new(..)`, the
     arguments in the constructor's parameter order. The field order, renames and constructor come
     from base-sdk's source at the pinned rev (the cargo checkout `cargo metadata` names), not from
     a table. A constructor counts only if its body stores every parameter unchanged:
     `impl_new!` and the block forms of `codec_base!`/`codec_type!`/`codec_payload!`/`codec_p2p!`,
     or a hand-written one such as `TxOut::from_duffs` (`TxOut::new` takes an `Amount` and is never
     picked). A functional update (`..base`), a missing field or a type without such a
     constructor goes to the hand list.
     A functional update is converted only when every field it takes from the base reads by copy,
     or in test code, where the base's fields are cloned (`ctor-update`).
   - `let Type { a, b: x, .. } = place;` becomes one `let` per field through its getter. Other
     struct patterns go to the hand list.
   - `x.f = expr;` becomes `x.set_f(expr);` for the six fields with a public setter.
   - A write with no setter (`x.f = v`, `x.f op= v`, `x.f.push(..)`, `x.f[i].g = v`) is a
     **rebuild**: `x = T::new(.., v, ..)` with every other field read back through its getter,
     or, for a mutation, `{ let mut f = x.f().clone(); <the statement on f>; x = T::new(.., f, ..) }`.
     The bare name `x` is assigned when it is a `mut` binding and `*x` when it is a `&mut`. In
     production code only a rebuild that clones nothing is made; one that clones goes to the hand
     list. In test code the rebuild may clone, and it may sit in an expression-bodied closure
     (`&|p| p.f = v`).
   - In test code a type without a plain constructor can still be rebuilt when its `new` stores
     every parameter unchanged and resets the remaining fields (the ProTx `new`s reset `sig`).
     The old value is put back through the `[[restore]]` helper of `config.toml`,
     `dash_dml::testkit::WithSig::with_sig` (`rebuild-restore`). That helper returns the payload
     itself when it already holds that signature. Otherwise it re-encodes the payload with the old
     signature's bytes and decodes it, asserting that everything but the signature round-trips
     by its `Debug` form. A payload that does not encode, or loses in-memory state through the
     encoding, panics there, loudly, instead of running on a changed value. Only crates that are
     dash-dml or depend on it with `test-utils` reach the helper; elsewhere such a write goes to
     the hand list.
   - The restore is skipped when the block writes the reset field before anything reads it
     (`p.vch_sig = Default::default()`, `p.reset_signature()` or `p.sig = v` with no `p` in `v`,
     after only plain writes of other fields): `rebuild-reset-dead`.
   - Consecutive plain writes to the same place (`p.a = x; p.b = y;`) of such a type are one
     rebuild, so the signature is put back once, on the final payload. Steps in between may not
     encode. A write joins only if its value reads the place's fields that the run has not
     written yet, which are the values the old code read.
3. `rustfmt` on the files the tool rewrote.
4. **Hand patches.** `hand/<workspace>/<crate>.<n>.patch`, applied in order with
   `git apply --3way` (against a scratch index, so the real index is untouched, and with rerere
   off, so a resolution recorded for another conflict never resolves this one), each followed by
   another auto pass. A patch that is already in the tree is skipped; one that fails is reported
   and the run goes on.
5. Once the crate builds, clippy's `needless_borrow` and `needless_borrows_for_generic_args`
   fixes are applied, only on lines this port changed.

The tool never stages or commits.

## Commands

```
G="python3 -I contrib/node/repin-getters/repin_getters.py"
$G order                         # members reaching base-sdk, dependency order
$G api > api.json                # base-sdk's private-field table: getters, setters, constructors
$G run --fresh                   # every crate, each reset to HEAD first (git checkout -- <crate dir>);
                                 # or: $G run --fresh dash-consensus dash-evo; --start-at <crate>;
                                 # --stop-on-error stops at the first crate left with errors
$G report [--left] [--nonequiv]  # per-crate table, errors left, hand sites, non-equivalence list
$G hand-save <crate>             # record your edits since the crate's last snapshot as its next patch
$G hand-list [--init]            # the hand-edit list: every hunk with its class (hand-edits.toml)
```

Nested workspaces take `--manifest`, for example
`$G --manifest node-fuzz/Cargo.toml run`; their patches live under `hand/node-fuzz/`.

State (snapshots, per-crate stats) goes to `target/repin-getters/`; delete it to start over.

## Hand edits

What the tool can't do safely is done by hand and kept as patches, so a re-run reproduces it:

1. `$G run <crate>`; it stops at a fixed point with errors left (`$G report --left` lists them
   and the sites the tool refused, with the reason).
2. Edit the tree. Register a new file with `git add -N <file>` (intent to add) so the patch
   records it; `run --fresh` removes the files the patches create before applying them again.
3. `$G hand-save <crate>` writes `hand/<ws>/<crate>.<n>.patch` against the snapshot the tool took
   after its last pass, then `$G run --fresh <crate>`: it replays the auto pass and every patch in
   order. (A plain `run` on a tree that already holds the patches re-applies them over the
   constructor rewrites of the last stage, which can conflict and leave conflict markers.)
4. `$G hand-list --init` refreshes `hand-edits.toml`, one entry per hunk; give each a class:
   `ctor` (literal → constructor, not mechanical), `setter`, `rebuild` (write with no setter →
   rebuilt through the constructor), `ref/copy`, `clone`, `api` (other API changes: compressed
   headers2, encode_v2/short_id taking a common version, Pong's optional nonce, `str_u64`,
   `read_net_info` removed), `mirror` (node-l1's ProTx mirror, DEC-1066: the mirror itself, its
   round-trip test, and the test and fuzz writes moved onto it; see `node-l1/src/protx_mirror.rs`).

## Re-running after a rebase

```
rm -rf target/repin-getters
git checkout <dry-branch> -- contrib/node/repin-getters   # the hand patches and their classes
git checkout <new tip> -- .                                # or rebase the tool commit onto it
$G run --fresh
$G report --left --nonequiv
```

`--3way` carries a hunk over moved or lightly changed lines; a patch that still fails is listed
under `HAND PATCH FAILED` with git's message, and those sites are redone by hand and re-saved. A
re-pin to a newer base-sdk needs no table changes: the struct shapes are re-read from the new
source. `config.toml`'s renames are checked against it on every run, and a run fails if one no
longer matches.

## Limits

- Only code that `cargo check --all-targets` compiles on this host with the crate's features is
  seen: `cfg(target_os = ...)`, `cfg(fuzzing)` and similar gated code, doctests and dead macro
  arms need a separate pass (`cargo test --doc --no-run`, other targets).
- `config.toml` holds per-crate feature or target sets for crates whose `--all-features` build is
  broken for reasons unrelated to the port, and the `[[restore]]` helpers.
- A signed payload rebuilt where its signature cannot be put back (it does not encode, or the
  encoding drops in-memory state such as a relabelled lazy BLS key) is a hand edit, marked
  `NON-EQUIV (repin)` in the code.

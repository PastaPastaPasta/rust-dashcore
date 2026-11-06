//! Floating point stays in `src/fp/` (CS2 §2.2 rule 5, §2.6). Clippy cannot ban a primitive
//! type, and `clippy::float_arithmetic` misses a float that is only compared, so this reads the
//! source with Rust's own lexer (`proc-macro2`) and parser (`syn`). Rules 1 and 2 together keep
//! every `f32`/`f64` value, and every float comparison, inside `fp`; rule 3 keeps libm out of it:
//!
//! 1. Outside `src/fp/`, no token names `f32` or `f64`, is a float literal (`0.5`, `75.0`,
//!    `1e9`, `2.`, `..1.5`, `1f64`, `1_000f64`) or names something `…_f32`/`…_f64`
//!    (`Arith256::to_f64`, which `clippy.toml` also bans). A float whose type is inferred, as
//!    in `if fp::difficulty(b) <= 75.0`, needs one of those to meet. The one exception is a
//!    literal `syn` parses as tuple-field indices: `t.0.1` lexes as `t`, `.`, `0.1`. Macro and
//!    attribute arguments are never parsed as expressions, so every float literal in them counts,
//!    `m!(t.0.1)` included.
//! 2. Nothing in `src/fp/` hands a float, or an ordering of floats, to the rest of the crate.
//!    These may not name `f32`, `f64`, `PartialOrd` or `Ord` (CS2 §2.3: `fp`'s opaque types
//!    derive only `PartialEq`, and every float decision is an `fp` function):
//!    - an item the crate can see (`pub`, `pub(crate)`, and `pub(super)` in `fp`'s root file):
//!      a function's signature, a const's or static's type, an enum's generics, where clause,
//!      derives and variants, a struct's generics, where clause, derives and visible fields, a
//!      `pub use`;
//!    - every trait definition the crate can see, and every trait impl, whatever the visibility
//!      of its items: the header (`impl From<Difficulty> for f64`, `impl PartialOrd for
//!      Difficulty`), its where clause and each item's signature, associated type and const
//!      type;
//!    - an inherent `impl`'s header and its visible items' signatures;
//!    - a type alias, or a `use … as` renaming `f32`, `f64`, `PartialOrd` or `Ord`, at any
//!      visibility; an item-level macro (it is not expanded); every attribute but a doc comment
//!      on a visible type, `cfg_attr(…, derive(PartialOrd))` included.
//!
//!    Function bodies are not read: a value leaves a function only through its signature. An
//!    `impl` inside a body or a const's value is as global as any other, so those are found
//!    wherever they are and checked the same way. `fp`'s values travel in opaque types (CS2
//!    §2.3), such as `pub struct Difficulty(f64)`, whose field is private.
//! 3. Nothing in `src/fp/` uses `%` or `%=`, or names `Rem`, `RemAssign`, `rem` or `rem_assign`
//!    (the method forms, `x.rem(y)` after `use core::ops::*`, and `Rem::rem(x, y)`); and no
//!    `macro_rules!` anywhere in the crate does, since `fp` may expand a macro defined elsewhere.
//!    A float remainder is libm's `fmod` (CS2 §2.2), which `clippy.toml` cannot ban as an
//!    operator, and `fp` needs no integer one. `contrib/node/check-no-libm.sh` backs this up on
//!    the compiled crate: its release rlib may not reference any libm function.
//!
//! Limits: macros are not expanded (rule 2 reports an item-level macro in `fp` that names a
//! float; rule 1 reads macro arguments as tokens). A float from a dependency whose name says
//! nothing of it, and that is only compared, would pass rule 1; besides `to_f64`, the crate's
//! dependencies expose one, the non-consensus `dash-params` field `ChainTxData::tx_rate`. A file
//! is under rule 2 by where it lies, so a `#[path]` mounting a file under `src/fp/` from outside
//! `fp` would escape rule 1. Rule 2 errs on the safe side where `fp` could write it otherwise:
//! an impl of an `fp`-private trait is checked like any trait impl, a `pub` item of a private
//! file module counts as visible (write `pub(super)` or `pub(in crate::fp)`), and a private
//! `macro_rules!` that names a float is reported.

use std::collections::BTreeSet;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::visit::Visit;
use syn::{Generics, ImplItem, Item, ItemImpl, TraitItem, UseTree, Visibility};

/// A source position, `(line, column)`.
type Position = (usize, usize);

/// The lines (1-based) of the tokens in `tokens` that name a float type, are a float literal or
/// name a `…_f32`/`…_f64` item, and with `order`, that name `PartialOrd` or `Ord`. A literal that
/// starts at one of `indices` is a tuple-field index chain, not a float (`t.0.1`).
fn float_lines(tokens: TokenStream, order: bool, indices: &BTreeSet<Position>) -> Vec<usize> {
    let mut found = Vec::new();
    for tree in tokens {
        let float = match &tree {
            TokenTree::Group(group) => {
                found.extend(float_lines(group.stream(), order, indices));
                false
            }
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                let name = name.strip_prefix("r#").unwrap_or(&name);
                // `f64`, `to_f64`, `as_secs_f64` (but not `f64_bits` or `of64`)
                let float = name
                    .strip_suffix("f32")
                    .or_else(|| name.strip_suffix("f64"))
                    .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('_'));
                float || (order && matches!(name, "PartialOrd" | "Ord"))
            }
            TokenTree::Literal(literal) => {
                let start = literal.span().start();
                !indices.contains(&(start.line, start.column))
                    && match syn::Lit::new(literal.clone()) {
                        syn::Lit::Float(_) => true,
                        // syn keeps `1f64` and `1_000f32` as integers with a float suffix
                        syn::Lit::Int(int) => matches!(int.suffix(), "f32" | "f64"),
                        _ => false,
                    }
            }
            TokenTree::Punct(_) => false,
        };
        if float {
            found.push(tree.span().start().line);
        }
    }
    found
}

/// Where `syn` parsed a tuple-field index (`t.0`, and both of `t.0.1`'s, which share a literal),
/// and the `macro_rules!` definitions, anywhere in a file.
#[derive(Default)]
struct Parsed<'a> {
    indices: BTreeSet<Position>,
    macro_rules: Vec<&'a syn::Macro>,
}

impl<'a> Visit<'a> for Parsed<'a> {
    fn visit_expr_field(&mut self, e: &'a syn::ExprField) {
        if let syn::Member::Unnamed(index) = &e.member {
            let start = index.span.start();
            self.indices.insert((start.line, start.column));
        }
        syn::visit::visit_expr_field(self, e);
    }

    fn visit_item_macro(&mut self, m: &'a syn::ItemMacro) {
        if m.mac.path.is_ident("macro_rules") {
            self.macro_rules.push(&m.mac);
        }
        syn::visit::visit_item_macro(self, m);
    }
}

/// Whether the rest of the crate can see an item with `vis`. `fp_root` is true for the items
/// of `fp`'s root file, where `pub(super)` means the crate root.
fn exposed(vis: &Visibility, fp_root: bool) -> bool {
    let Visibility::Restricted(r) = vis else {
        return matches!(vis, Visibility::Public(_));
    };
    let path = r.path.to_token_stream().to_string().replace(' ', "");
    match path.as_str() {
        "crate" => true,
        "super" => fp_root,
        "self" => false,
        // `pub(in crate::fp::…)` stays in fp; any other path leaves it
        _ => !(path == "crate::fp" || path.starts_with("crate::fp::")),
    }
}

/// Adds the lines of everything in `parts` that hand a float or its ordering out to `found`.
fn check(found: &mut Vec<usize>, parts: &[&dyn ToTokens]) {
    for part in parts {
        found.extend(float_lines(part.to_token_stream(), true, &BTreeSet::new()));
    }
}

/// `generics` and its where clause, which `Generics`' tokens leave out.
fn check_generics(found: &mut Vec<usize>, generics: &Generics) {
    check(found, &[generics, &generics.where_clause]);
}

/// A visible type's attributes but its doc comments: `derive`, and a `derive` nested in
/// `cfg_attr` (`#[cfg_attr(target_arch = "x86_64", derive(PartialOrd))]`).
fn check_attrs(found: &mut Vec<usize>, attrs: &[syn::Attribute]) {
    for attr in attrs.iter().filter(|a| !a.path().is_ident("doc")) {
        check(found, &[attr]);
    }
}

/// The lines of a `use` tree that rename `f32`, `f64`, `PartialOrd` or `Ord`
/// (`use core::primitive::f64 as Ratio`, `use core::cmp::PartialOrd as Compare`).
fn check_renames(found: &mut Vec<usize>, tree: &UseTree) {
    match tree {
        UseTree::Path(p) => check_renames(found, &p.tree),
        UseTree::Group(g) => g.items.iter().for_each(|t| check_renames(found, t)),
        UseTree::Rename(r) if ["f32", "f64", "PartialOrd", "Ord"].iter().any(|n| r.ident == n) => {
            found.push(r.ident.span().start().line);
        }
        _ => {}
    }
}

/// An `impl` block in `fp`, wherever it is: its header, and the signatures of the items the
/// crate can see (all of a trait impl's).
fn check_impl(found: &mut Vec<usize>, i: &ItemImpl, fp_root: bool) {
    check_generics(found, &i.generics);
    check(found, &[&i.self_ty]);
    if let Some((_, path, _)) = &i.trait_ {
        check(found, &[path]);
    }
    // a trait impl's items are as visible as the trait
    let visible = |v: &Visibility| i.trait_.is_some() || exposed(v, fp_root);
    for item in &i.items {
        match item {
            ImplItem::Fn(f) if visible(&f.vis) => check(found, &[&f.sig]),
            ImplItem::Const(c) if visible(&c.vis) => {
                check_generics(found, &c.generics);
                check(found, &[&c.ty]);
            }
            ImplItem::Type(t) => check(found, &[t]),
            ImplItem::Macro(m) => check(found, &[m]),
            _ => {}
        }
    }
}

/// The `impl` blocks inside bodies and const values (`const _: () = { impl … };`), which the
/// item walk of [`fp_offenders`] does not reach.
#[derive(Default)]
struct NestedImpls<'a> {
    depth: usize,
    found: Vec<&'a ItemImpl>,
}

impl<'a> Visit<'a> for NestedImpls<'a> {
    fn visit_block(&mut self, block: &'a syn::Block) {
        self.depth += 1;
        syn::visit::visit_block(self, block);
        self.depth -= 1;
    }

    fn visit_item_impl(&mut self, i: &'a ItemImpl) {
        if self.depth > 0 {
            self.found.push(i);
        }
        syn::visit::visit_item_impl(self, i);
    }
}

/// The lines of a file under `src/fp/` that hand a float out (rule 2).
fn fp_file_offenders(file: &syn::File, fp_root: bool) -> Vec<usize> {
    let mut found = Vec::new();
    fp_offenders(&file.items, fp_root, &mut found);
    let mut nested = NestedImpls::default();
    nested.visit_file(file);
    for i in nested.found {
        check_impl(&mut found, i, fp_root);
    }
    found
}

/// The lines of the module-level `items`, in a file under `src/fp/`, that hand a float out.
fn fp_offenders(items: &[Item], fp_root: bool, found: &mut Vec<usize>) {
    for item in items {
        let vis = |v: &Visibility| exposed(v, fp_root);
        match item {
            Item::Fn(f) if vis(&f.vis) => check(found, &[&f.sig]),
            Item::Const(c) if vis(&c.vis) => {
                check_generics(found, &c.generics);
                check(found, &[&c.ty]);
            }
            Item::Static(s) if vis(&s.vis) => check(found, &[&s.ty]),
            Item::Use(u) => {
                check_renames(found, &u.tree);
                if vis(&u.vis) {
                    check(found, &[&u.tree]);
                }
            }
            Item::Enum(e) if vis(&e.vis) => {
                check_attrs(found, &e.attrs);
                check_generics(found, &e.generics);
                check(found, &[&e.variants]);
            }
            Item::Struct(s) if vis(&s.vis) => {
                check_attrs(found, &s.attrs);
                check_generics(found, &s.generics);
                for field in s.fields.iter().filter(|field| vis(&field.vis)) {
                    check(found, &[&field.ty]);
                }
            }
            Item::Union(u) if vis(&u.vis) => {
                check_attrs(found, &u.attrs);
                check_generics(found, &u.generics);
                for field in u.fields.named.iter().filter(|field| vis(&field.vis)) {
                    check(found, &[&field.ty]);
                }
            }
            Item::Type(t) => check(found, &[t]),
            Item::Macro(m) => check(found, &[m]),
            Item::Trait(t) if vis(&t.vis) => {
                check_generics(found, &t.generics);
                check(found, &[&t.supertraits]);
                for item in &t.items {
                    match item {
                        TraitItem::Fn(f) => check(found, &[&f.sig]),
                        TraitItem::Const(c) => {
                            check_generics(found, &c.generics);
                            check(found, &[&c.ty]);
                        }
                        other => check(found, &[other]),
                    }
                }
            }
            Item::Impl(i) => check_impl(found, i, fp_root),
            // an inline module's `pub(super)` is fp itself
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    fp_offenders(items, false, found);
                }
            }
            // items syn keeps as tokens
            Item::Verbatim(tokens) => check(found, &[tokens]),
            // private items; extern crates; extern blocks, whose functions would need `unsafe`,
            // which the crate forbids
            _ => {}
        }
    }
}

/// Every `.rs` file under `dir`, recursively, sorted.
fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// The lines (1-based) of the `%` and `%=` tokens and the `Rem`, `RemAssign`, `rem` and
/// `rem_assign` identifiers in `tokens` (rule 3).
fn remainder_lines(tokens: TokenStream) -> Vec<usize> {
    let mut found = Vec::new();
    for tree in tokens {
        match tree {
            TokenTree::Group(group) => found.extend(remainder_lines(group.stream())),
            TokenTree::Punct(punct) if punct.as_char() == '%' => {
                found.push(punct.span().start().line);
            }
            TokenTree::Ident(ident)
                if ["Rem", "RemAssign", "rem", "rem_assign"].iter().any(|n| ident == n) =>
            {
                found.push(ident.span().start().line);
            }
            _ => {}
        }
    }
    found
}

/// The lines of the file `name` outside `src/fp/` that break rule 1, or rule 3 in a
/// `macro_rules!`.
fn rule_1_lines(name: &str, text: &str) -> Vec<usize> {
    let file =
        syn::parse_file(text).unwrap_or_else(|e| panic!("{name}: {e} at {:?}", e.span().start()));
    let mut parsed = Parsed::default();
    parsed.visit_file(&file);
    let mut found = float_lines(text.parse().expect("parsed above"), false, &parsed.indices);
    for mac in parsed.macro_rules {
        found.extend(remainder_lines(mac.tokens.clone()));
    }
    found
}

/// The lines under `src` that break rule 1 (outside `src/fp/`) or rules 2 and 3 (inside it).
fn offenders(src: &Path) -> Vec<String> {
    let fp = src.join("fp");
    let mut found = Vec::new();
    for path in rust_files(src) {
        let fp_root = path == fp.join("mod.rs") || path == src.join("fp.rs");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut bad = if path.starts_with(&fp) || fp_root {
            let file = syn::parse_file(&text)
                .unwrap_or_else(|e| panic!("{}: {e} at {:?}", path.display(), e.span().start()));
            let mut bad = fp_file_offenders(&file, fp_root);
            bad.extend(remainder_lines(text.parse().expect("parsed above")));
            bad
        } else {
            rule_1_lines(&path.display().to_string(), &text)
        };
        bad.sort_unstable();
        bad.dedup();
        let lines: Vec<&str> = text.lines().collect();
        found.extend(
            bad.into_iter().map(|n| format!("{}:{n}: {}", path.display(), lines[n - 1].trim())),
        );
    }
    found
}

#[test]
fn floats_stay_in_fp() {
    let found = offenders(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(
        found.is_empty(),
        "floating point outside src/fp's private code:\n{}",
        found.join("\n")
    );
}

/// Whether rule 1 reports `code`, statements in a function body.
fn has_float(code: &str) -> bool {
    !rule_1_lines(code, &format!("fn probe() {{ {code}\n}}")).is_empty()
}

/// The scan itself, in both directions, including the escapes review CS2ab-r2 m1 found in the
/// line scanner this replaced.
#[test]
fn the_scan_finds_what_it_must() {
    for code in [
        "fn bad() -> f32 {}",
        "if fp::difficulty(b) <= 75.0 {}",
        "let x = 0.5;",
        "let x = 2.;",
        "let y = 1e9 as u64;",
        "let y = 1E-3;",
        "let z = 1f64;",
        "let z = 2_f32;",
        "let z = 1_000f64;",
        "let z = 1_000.5;",
        "let w = core::primitive::f64::MAX;",
        "let w = r#f64::MAX;",
        "if a.to_f64() <= b.to_f64() {}",
        "let s = d.as_secs_f64();",
        "let v = vec![0.5];",
        // a raw string with a quote, or a string over lines, does not hide the code after it
        "let s = r#\"a \" b\"#; let x = 0.5;",
        "let s = \"a\nb\"; let x = 0.5;",
        // a range bound is not a field access
        "let r = ..0.5;",
        "if x < (..1.5).end {}",
        // in a macro's or an attribute's tokens a dot does not make a field access (review
        // CS2c-r1 GPT 3): `less!` discards the dots and compares two floats
        "let b = less!(. 0.5, . 1.5);",
        "assert_eq!(t.0.1, 2);",
        "#[cfg_attr(any(), doc = 0.5)] fn f() {}",
    ] {
        assert!(has_float(code), "missed: {code}");
    }
    for code in [
        "// f64 in a comment",
        "/* 0.5 f64 */",
        "/// f64 in a doc comment\nfn f() {}",
        "let r = 0..5;",
        "let a = t.0.1;",
        "let a = t.0 .1 + u.2;",
        "let m = 1.max(2);",
        "let h = 0x1e01_0000;",
        "let b = 0b1010;",
        "let s = \"0.5 f64\";",
        "let s = r#\"0.5 \" f64\"#;",
        "let c = '.';",
        "fn ok(f64_bits: u64, of64: u8) {}",
        "let k = 1_000_000;",
        "let k = 1_000u64;",
    ] {
        assert!(!has_float(code), "false positive: {code}");
    }

    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fp-confinement-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let fp_mod = [
        "pub struct Difficulty(f64);",               // 1: private field
        "fn private(x: f64) -> f64 { x * 0.5 }",     // 2: private
        "pub(super) fn leaks() -> f64 { 0.0 }",      // 3: the crate root sees it
        "pub struct Open(pub f64);",                 // 4: a pub field
        "impl From<Difficulty> for f64 {",           // 5: a conversion out
        "    fn from(d: Difficulty) -> f64 { d.0 }", // 6: inside the impl
        "}",                                         // 7
        "type F = f64;",                             // 8: an alias
        "pub(crate) fn laundered(",                  // 9: a signature over lines
        "    x: u32,",                               // 10
        ") -> f64 { 0.0 }",                          // 11
        "pub enum Horizon {",                        // 12: a visible enum
        "    Crossed(f64),",                         // 13: its variant
        "    Open,",                                 // 14
        "}",                                         // 15
        "impl Difficulty {",                         // 16: no float in the header
        "    fn inner(&self) -> f64 { self.0 }",     // 17: private
        "    pub fn ratio(&self) -> u32 { (self.0 * 2.0) as u32 }", // 18: a float only inside
        "}",                                         // 19
        // the r2 probes: a trait hands the float out
        "pub trait AsFloat { fn value(&self) -> f64; }", // 20
        "impl AsFloat for Difficulty { fn value(&self) -> f64 { self.0 } }", // 21
        // a private trait's impl is still checked
        "trait Hidden { type Out; }",                     // 22
        "impl Hidden for Difficulty { type Out = f64; }", // 23
        // a pub field line does not hide the enum after it
        "pub struct Pair {",    // 24
        "    pub count: u32,",  // 25
        "}",                    // 26
        "pub enum AfterPair {", // 27
        "    Crossed(f64),",    // 28
        "    Open,",            // 29
        "}",                    // 30
        // a one-line enum's variants
        "pub enum OneLine { Crossed(f64), Open }", // 31
        "pub struct Named { pub x: f64, y: f64 }", // 32: x only
        "pub const C: f64 = 1.0;",                 // 33
        "macro_rules! m { () => { pub fn g() -> f64 { 1.0 } } }", // 34: not expanded
        "pub use core::f64::consts::PI;",          // 35
        "impl PartialEq for Difficulty {",         // 36: no float in a signature
        "    fn eq(&self, o: &Self) -> bool { self.0 == o.0 * 1.0 }", // 37
        "}",                                       // 38
        "pub(in crate::fp) fn stays() -> f64 { 0.0 }", // 39: stays in fp
        "mod inner { pub(super) fn up() -> f64 { 0.0 } }", // 40: up to fp only
        // review of the rewrite: a renamed f64 in a signature
        "use core::primitive::f64 as Ratio;", // 41
        "impl Difficulty { pub fn r(&self) -> Ratio { self.0 } }", // 42: caught at the rename
        // impls inside a const's value and a function body are global
        "const _: () = { impl crate::Value for Difficulty { type Out = f64; } };", // 43
        "fn hidden() { impl Difficulty { pub fn r2(&self) -> f64 { self.0 } } }",  // 44
        "pub struct W<T>(T) where T: Into<f64>;", // 45: a where clause
        // an ordering of floats, through a derive, an impl or an opaque return type
        "#[derive(Clone, Copy, PartialEq, PartialOrd)]", // 46
        "pub struct Ordered(f64);",                      // 47
        "impl PartialOrd for Difficulty { fn partial_cmp(&self, o: &Self) -> Option<C> { None } }", // 48
        "pub fn sortable() -> impl PartialOrd { 0u8 }", // 49
        // CS2 §2.3's Difficulty, and a private import of a float constant: both fine
        "#[derive(Clone, Copy, Debug, PartialEq)]", // 50
        "pub struct Design(f64);",                  // 51
        "use core::f64::consts::PI;",               // 52
        // rule 3: a remainder is fmod, in either form; a `%` in a doc comment is text
        "fn rem(x: f64) -> f64 { x % 2.0 }", // 53
        "/// within 50% of it",              // 54
        "fn fine() {}",                      // 55
        "fn rem2(x: f64) -> f64 { core::ops::Rem::rem(x, 2.0) }", // 56
        // review CS2c-r1 GPT 2: an ordering through a renamed trait or a cfg_attr derive
        "use core::cmp::PartialOrd as Compare;", // 57
        "impl Compare for Difficulty { fn partial_cmp(&self, o: &Self) -> Option<C> { None } }", // 58
        "#[derive(Clone, Copy, PartialEq)]", // 59
        "#[cfg_attr(target_arch = \"x86_64\", derive(PartialOrd))]", // 60
        "pub struct CfgOrdered(f64);",       // 61
        // review CS2c-r1 GPT 1: the method forms after a glob import name neither `%` nor `Rem`
        "use core::ops::*;",                                   // 62
        "fn rem3(x: f64) -> f64 { x.rem(2.0) }",               // 63
        "fn rem4(mut x: f64) -> f64 { x.rem_assign(2.0); x }", // 64
    ]
    .join("\n");
    let files = [
        ("fp/mod.rs", fp_mod.as_str()),
        ("fp/kgw/table.rs", "pub(super) const P: [f64; 1] = [0.5];\npub(crate) fn g() -> f32 {}"),
        ("pow/fp/mod.rs", "fn nested_fp_is_not_fp() -> f64 { 1.0 }"),
        ("subsidy.rs", "fn bad() -> u64 { if x < 75.0 { 1 } else { 0 } }"),
        // the r2 probes outside fp: one is caught here, the other through the trait in fp
        (
            "fp_probe_user.rs",
            "fn cmp(a: u32, b: u32) -> bool {\n\
             crate::fp::difficulty(a).value() <= crate::fp::difficulty(b).value()\n\
             && crate::fp::difficulty(a).value() <= 1_000f64\n}",
        ),
        // review CS2c-r1 GPT 3: floats compared outside fp through a macro that drops the dots
        (
            "macro_escape.rs",
            "macro_rules! less {\n\
             ($ignore:tt $a:literal, $other:tt $b:literal) => { $a < $b };\n\
             }\n\
             fn outside() -> bool { less!(. 0.5, . 1.5) }",
        ),
        // review CS2c-r1 Opus 1: a `%` that fp would expand from a macro defined elsewhere
        (
            "pow/remainder.rs",
            "macro_rules! remainder { ($a:expr, $b:expr) => { $a % $b } }\n\
             pub(crate) use remainder;",
        ),
    ];
    for (file, text) in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let found = offenders(&root);
    let lines: Vec<&str> = found.iter().map(|f| f.split(": ").next().unwrap()).collect();
    let expected = [
        "fp/kgw/table.rs:2",  // pub(crate) from inside fp
        "fp/mod.rs:3",        // pub(super) from fp's root is the crate root
        "fp/mod.rs:4",        // a pub field
        "fp/mod.rs:5",        // a trait impl handing out a float: its header
        "fp/mod.rs:6",        // and its method
        "fp/mod.rs:8",        // a type alias
        "fp/mod.rs:11",       // the multi-line signature, at the float
        "fp/mod.rs:13",       // a visible enum's variant
        "fp/mod.rs:20",       // a visible trait's method
        "fp/mod.rs:21",       // a trait impl's method
        "fp/mod.rs:23",       // a trait impl's associated type
        "fp/mod.rs:28",       // the enum after a pub field
        "fp/mod.rs:31",       // a one-line enum
        "fp/mod.rs:32",       // a visible named field
        "fp/mod.rs:33",       // a visible const
        "fp/mod.rs:34",       // a macro
        "fp/mod.rs:35",       // a re-export
        "fp/mod.rs:41",       // a renamed f64
        "fp/mod.rs:43",       // an impl in a const's value
        "fp/mod.rs:44",       // an impl in a function body
        "fp/mod.rs:45",       // a where clause
        "fp/mod.rs:46",       // a PartialOrd derive
        "fp/mod.rs:48",       // a PartialOrd impl
        "fp/mod.rs:49",       // an ordering returned opaquely
        "fp/mod.rs:53",       // a float remainder
        "fp/mod.rs:56",       // and its method form
        "fp/mod.rs:57",       // a renamed PartialOrd (its impl at 58 is caught here)
        "fp/mod.rs:60",       // a derive in cfg_attr
        "fp/mod.rs:63",       // rem() through a glob import
        "fp/mod.rs:64",       // rem_assign() through it
        "fp_probe_user.rs:3", // `1_000f64`
        "macro_escape.rs:4",  // the floats in the macro's arguments
        "pow/fp/mod.rs:1",    // only src/fp is exempt
        "pow/remainder.rs:1", // a `%` in a macro_rules! outside fp
        "subsidy.rs:1",       // an inferred float
    ];
    assert_eq!(lines.len(), expected.len(), "{found:#?}");
    for (line, expected) in lines.iter().zip(expected) {
        assert!(line.ends_with(expected), "{line} is not {expected}: {found:#?}");
    }
}

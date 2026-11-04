# Phase 11: WebIDL parser and generator (design note)

Gate (build-spec section 9): IDL subset generates and compiles; WPT `dom/`
subset at threshold. This note covers the first half, the generator front end
in `crates/px-bindings`. Hand-written glue count stays zero: anything that
cannot be generated is a generator bug.

## Pipeline

`parser::parse` (text to `ast`) -> `resolve::Model::build` (merge partials and
mixins, resolve names and typedefs, check) -> `ir::lower` (engine-neutral
`Bindings`) -> `emit::Emitter` (text). `px_bindings::generate` runs all four.

Limits are constants at the point of enforcement: 2 MiB source, 4 KiB words,
64 KiB strings, nesting 32, typedef depth 64, 4096 type nodes per expansion,
inheritance depth 256, 100 diagnostics. The crate carries the workspace panic
lints (no unwrap, expect, indexing, panic) and is listed in
`PANIC_LINT_CRATES` of `cargo xtask unsafe-headers`.

## Decisions

**Generator form: library plus the `px-bindgen` binary.** Not a `build.rs`
(would run on every consumer build and needs file access in the build chain)
and not `cargo xtask bindings` (xtask is std-only with no dependencies, so it
cannot link the library without breaking that rule). The binary is the only
code that touches the file system: `cargo run -p px-bindings --bin px-bindgen
-- [--lenient] [--out FILE] PATH...`. A consuming crate checks the generated
file in or runs the binary from its own xtask gate; the DOM crate's choice is
made when it adopts the output.

**The host trait takes the arena, not `self`.** Generated methods are
`fn x(&mut self, this: rt::Handle<T>, ...) -> rt::HostResult<R>`. A handle is
`{index, generation}` like `px_dom::NodeId`; a stale one is
`Err(HostError::Stale)`. There is no infallible accessor and nothing indexes.
`tests/subset.rs` asserts every emitted trait method has that shape.

**`DOMString` is emitted as `String`, which is lossy.** DOMString is a
sequence of UTF-16 code units and may hold lone surrogates; `String` cannot.
The alias is one line in the generated prelude (`pub type DOMString`), so
changing it is a generator change, not a rewrite. Decide when the Boa binding
lands (Boa strings are UTF-16). `USVString` is exact; `ByteString` is
`Vec<u8>`.

**`async_iterable` is recorded in the IR but not emitted.** There is no host
method for it; the engine emitter binds it. Likewise the overload resolution
algorithm is not emitted: the IR keeps each overload set (argument conversions,
required count, `max_args`) and overloads become `name`, `name_1`, ...

## Not handled

Overload resolution and distinguishability beyond category checks; checking
that referenced types are exposed in the referrer's globals; validating
`[Exposed]` names against real globals; `[LegacyFactoryFunction]`,
`[LegacyNamespace]`, `[Default]`/`toJSON`, `[PutForwards]` target checking
(recorded, not validated); the engine emitter itself.

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
(recorded, not validated).

## Boa glue emitter

`emit::BoaGlueEmitter` (`crates/px-bindings/src/emit/boa.rs`, runtime helpers
in `boa_prelude.txt`) turns the same IR into `boa.rs`, a sibling module of
`host.rs` that exposes the host traits to script. `px-bindgen --boa-out FILE`
writes it. **`px-bindings` gains no dependency** (ADR 040 keeps `boa_engine`
in `px-script` only); the emitter writes text and the consumer compiles it
against `boa_engine =0.22.0`. No `px-script` wiring was done: this note did not
call for it, and the DOM crate has not adopted `host.rs` yet.

Shape: a platform object is a Boa object whose native data is the tuple
`(index, generation, interface index)` (a tuple, because `Trace` cannot be
derived without `unsafe` and tuples of integers already implement `Trace` and
`JsData`; the generated code contains no `unsafe`). Each member is a
`fn name<H: ScriptHost>`; `install::<H>(ctx, Rc<RefCell<H>>)` builds the
prototypes, constructors (with `[[Prototype]]` set to the parent interface
object), statics, constants, namespaces and globals. The host sits in the
context's host-defined data; a re-entrant call finds the `RefCell` borrowed and
throws. Integers use WebIDL ConvertToInt (wrap, `[EnforceRange]`, `[Clamp]`),
floats the restricted/unrestricted rules, enums exact match. Overloads are
selected by argument count only; if two overloads share a count the member is
not installed and is listed.

`ScriptHost::dynamic_interface` exists because a handle only knows its static
type (a `Node` that is an `Element`); the embedder answers with the most
derived interface and the glue checks it descends from the static one.

Everything not bound is data: `boa::UNSUPPORTED` lists `Interface.member:
reason`, and `tests/boa_glue.rs` pins the committed subset's list (20 entries).

### STATUS

Done, and checked how:
- Emitter, flag, 12 tests in `tests/boa_glue.rs` (text level: determinism, no
  `unsafe`, every `<H as XHost>::name` exists in `host.rs`, conversions, parent
  order, gaps listed).
- **Compiled and run against real Boa**, but not in the workspace: the
  generated `host.rs` + `boa.rs` for the whole IDL subset compile warning-free
  against `boa_engine =0.22.0`, and a 34-case script test over a small IDL
  (constructor, attributes, enums, nullable, `[EnforceRange]`/`[Clamp]`/wrap,
  default arguments, sequence return, errors mapped to `RangeError`/`TypeError`,
  static, constants, inheritance, `instanceof`, dynamic interface) passed. That
  harness lives outside the repo, so CI does not run it. It needs a home in
  `px-script` tests (dev wiring, a decision for the owner).

Not done:
- Conversions for dictionaries, unions, callback functions/interfaces, `any`,
  `object`, symbols, buffer sources, records, promises, `FrozenArray`,
  `ObservableArray`, and sequences as arguments (return only). 20 members of
  the committed subset are skipped for this reason or the next two.
- `iterable`/`async_iterable`/`maplike`/`setlike`; special operations
  (indexed/named getters, setters, deleters need exotic objects).
- Overload resolution by type; `[LegacyUnforgeable]`, `[Replaceable]`,
  `[PutForwards]`, `[SameObject]` (no wrapper cache: `a.firstChild !==
  a.firstChild`), `[LegacyFactoryFunction]`, `[SecureContext]` and `[Exposed]`
  filtering (every interface becomes a global).
- Exceptions: `HostError::Dom` becomes a plain `Error` prefixed `name:` because
  there is no `DOMException` yet. Constructors ignore `new.target`, so script
  subclasses get the base prototype.
- DOMString is still lossy (lone surrogates become U+FFFD); see Decisions.

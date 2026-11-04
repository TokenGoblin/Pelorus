//! Emitters turn `ir::Bindings` into source text.
//!
//! `Emitter` is the extension point: the engine-binding emitter that will
//! target the script engine implements the same trait over the same IR, so
//! adding it changes nothing in the front end. `RustHostEmitter` is the
//! engine-neutral one: host traits, dictionary structs, enums with string
//! conversion, constants, and the small runtime vocabulary (`rt`) the
//! signatures use.
//!
//! # Shape of the emitted Rust
//!
//! * Each interface `Foo` becomes an uninhabited marker `enum Foo {}` (the
//!   type parameter of `rt::Handle<Foo>`), an `impl Foo` holding `NAME` and
//!   the constants, `impl rt::Upcast<Ancestor> for Foo {}` per ancestor, and
//!   `trait FooHost: ParentHost`.
//! * Host trait methods take the arena owner (`&mut self`) and the target as
//!   an explicit generational `rt::Handle`, and every one returns
//!   `Result<_, rt::HostError>`: a stale handle is `Err(HostError::Stale)`,
//!   never a panic and never an unchecked index. This is the DOM crate's
//!   "every accessor returns Option" rule carried into generated glue.
//! * Dictionaries are structs (inheritance is a `parent` field), enums carry
//!   `as_idl_str` / `from_idl_str`, unions are generated enums.

use crate::ast::{Special, StringType};
use crate::ir::{
    Bindings, CollectionKind, Conv, DictionaryBinding, InterfaceBinding, IrConst, IrDefault,
    Overload,
};
use crate::resolve::{ContainerKind, Exposure};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub message: String,
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "emit: {}", self.message)
    }
}

impl std::error::Error for EmitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Relative path the caller should write this to.
    pub path: String,
    pub contents: String,
}

/// A back end over the engine-neutral IR.
pub trait Emitter {
    fn name(&self) -> &'static str;
    fn emit(&self, bindings: &Bindings) -> Result<Vec<OutputFile>, EmitError>;
}

/// The runtime vocabulary emitted at the top of the Rust output.
pub const RUNTIME_PRELUDE: &str = r#"/// Vocabulary the generated signatures use. Engine-neutral.
pub mod rt {
    use ::std::fmt;
    use ::std::hash::{Hash, Hasher};
    use ::std::marker::PhantomData;
    use ::std::num::NonZeroU32;

    /// Marks that `Self` is, or inherits from, `U`.
    pub trait Upcast<U> {}

    /// A generational reference to a platform object. It carries no
    /// pointer: the host resolves it and answers `HostError::Stale` when the
    /// object is gone.
    pub struct Handle<T> {
        index: u32,
        generation: NonZeroU32,
        _marker: PhantomData<fn() -> T>,
    }

    impl<T> Handle<T> {
        pub fn new(index: u32, generation: NonZeroU32) -> Self {
            Handle { index, generation, _marker: PhantomData }
        }
        pub fn index(self) -> u32 {
            self.index
        }
        pub fn generation(self) -> NonZeroU32 {
            self.generation
        }
        pub fn upcast<U>(self) -> Handle<U>
        where
            T: Upcast<U>,
        {
            Handle::new(self.index, self.generation)
        }
    }

    impl<T> Clone for Handle<T> {
        fn clone(&self) -> Self {
            *self
        }
    }
    impl<T> Copy for Handle<T> {}
    impl<T> PartialEq for Handle<T> {
        fn eq(&self, other: &Self) -> bool {
            self.index == other.index && self.generation == other.generation
        }
    }
    impl<T> Eq for Handle<T> {}
    impl<T> Hash for Handle<T> {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.index.hash(state);
            self.generation.hash(state);
        }
    }
    impl<T> fmt::Debug for Handle<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "Handle({}v{})", self.index, self.generation)
        }
    }

    macro_rules! opaque_id {
        ($(#[$doc:meta])* $name:ident) => {
            $(#[$doc])*
            pub struct $name<T> {
                pub id: u64,
                _marker: PhantomData<fn() -> T>,
            }
            impl<T> $name<T> {
                pub fn new(id: u64) -> Self {
                    $name { id, _marker: PhantomData }
                }
            }
            impl<T> Clone for $name<T> {
                fn clone(&self) -> Self {
                    *self
                }
            }
            impl<T> Copy for $name<T> {}
            impl<T> PartialEq for $name<T> {
                fn eq(&self, other: &Self) -> bool {
                    self.id == other.id
                }
            }
            impl<T> Eq for $name<T> {}
            impl<T> fmt::Debug for $name<T> {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}({})", stringify!($name), self.id)
                }
            }
        };
    }

    opaque_id!(
        /// A script callback (function or callback-interface object) the
        /// engine keeps alive for the host.
        Callback
    );
    opaque_id!(
        /// A script promise the engine will settle.
        Promise
    );

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ObjectHandle(pub u64);
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SymbolHandle(pub u64);
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BufferHandle(pub u64);

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BigInt {
        pub negative: bool,
        /// Little-endian magnitude.
        pub magnitude_le: ::std::vec::Vec<u8>,
    }

    /// A script value the host does not need to interpret (`any`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum ScriptValue {
        Undefined,
        Null,
        Boolean(bool),
        Number(f64),
        BigInt(BigInt),
        String(::std::string::String),
        Symbol(SymbolHandle),
        Object(ObjectHandle),
    }

    /// Why a host call failed. The engine layer turns each into the matching
    /// script exception.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HostError {
        /// The handle no longer names a live object.
        Stale,
        Type(::std::string::String),
        Range(::std::string::String),
        /// A `DOMException` with this name.
        Dom { name: ::std::string::String, message: ::std::string::String },
        NotSupported(::std::string::String),
    }

    impl fmt::Display for HostError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                HostError::Stale => write!(f, "stale handle"),
                HostError::Type(m) => write!(f, "TypeError: {m}"),
                HostError::Range(m) => write!(f, "RangeError: {m}"),
                HostError::Dom { name, message } => write!(f, "{name}: {message}"),
                HostError::NotSupported(m) => write!(f, "not supported: {m}"),
            }
        }
    }

    impl ::std::error::Error for HostError {}

    pub type HostResult<T> = ::std::result::Result<T, HostError>;
}
"#;

const RESULT: &str = "rt::HostResult";
const OPT: &str = "::std::option::Option";
const VEC: &str = "::std::vec::Vec";

// ---- naming --------------------------------------------------------------

const STRICT_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn",
    "abstract", "become", "box", "do", "final", "gen", "macro", "override", "priv", "try",
    "typeof", "unsized", "virtual", "yield",
];
const UNRAWABLE: &[&str] = &["self", "Self", "super", "crate"];

/// `getElementById` -> `get_element_by_id`, `innerHTML` -> `inner_html`.
pub fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' {
            out.push('_');
        } else if c.is_ascii_uppercase() {
            let prev = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit() => true,
                Some(p) if p.is_ascii_uppercase() => next.is_some_and(|n| n.is_ascii_lowercase()),
                _ => false,
            };
            if boundary {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A usable Rust value-namespace identifier for `snake`.
pub fn value_ident(snake: &str) -> String {
    if UNRAWABLE.contains(&snake) {
        format!("{snake}_")
    } else if STRICT_KEYWORDS.contains(&snake) {
        format!("r#{snake}")
    } else {
        snake.to_owned()
    }
}

/// PascalCase type name; a lowercase first letter is raised.
pub fn type_ident(name: &str) -> String {
    let mut s = String::new();
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            s.push(c.to_ascii_uppercase());
        } else if c == '-' {
            s.push('_');
        } else {
            s.push(c);
        }
    }
    if s == "Self" { "Self_".to_owned() } else { s }
}

/// `"text/html"` -> `TextHtml`; `""` -> `Empty`.
fn variant_ident(value: &str) -> String {
    let mut out = String::new();
    for part in value.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut cs = part.chars();
        if let Some(f) = cs.next() {
            out.push(f.to_ascii_uppercase());
            out.push_str(cs.as_str());
        }
    }
    if out.is_empty() {
        out.push_str("Empty");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    if out == "Self" {
        out.push('_');
    }
    out
}

struct Claims(BTreeSet<String>);

impl Claims {
    fn new() -> Self {
        Claims(BTreeSet::new())
    }
    fn claim(&mut self, base: String) -> String {
        if self.0.insert(base.clone()) {
            return base;
        }
        let mut n = 2usize;
        loop {
            let c = format!("{base}_{n}");
            if self.0.insert(c.clone()) {
                return c;
            }
            n += 1;
        }
    }
}

// ---- the Rust host emitter ----------------------------------------------------

pub struct RustHostEmitter;

impl Emitter for RustHostEmitter {
    fn name(&self) -> &'static str {
        "rust-host"
    }

    fn emit(&self, b: &Bindings) -> Result<Vec<OutputFile>, EmitError> {
        let text = Gen {
            b,
            unions: BTreeMap::new(),
            out: String::new(),
        }
        .run()?;
        Ok(vec![OutputFile {
            path: "host.rs".to_owned(),
            contents: text,
        }])
    }
}

struct Gen<'a> {
    b: &'a Bindings,
    /// Union enum name -> (variant, rust type) in member order.
    unions: BTreeMap<String, Vec<(String, String)>>,
    out: String,
}

fn int_type(bits: u8, signed: bool) -> &'static str {
    match (bits, signed) {
        (8, true) => "i8",
        (8, false) => "u8",
        (16, true) => "i16",
        (16, false) => "u16",
        (32, true) => "i32",
        (32, false) => "u32",
        (_, true) => "i64",
        (_, false) => "u64",
    }
}

impl Gen<'_> {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn w(&mut self, args: fmt::Arguments<'_>) {
        // Writing to a String cannot fail.
        let _ = self.out.write_fmt(args);
        self.out.push('\n');
    }

    fn label(&mut self, c: &Conv) -> String {
        match c {
            Conv::Any => "Any".into(),
            Conv::Undefined => "Undefined".into(),
            Conv::Boolean => "Boolean".into(),
            Conv::Integer { bits, signed, .. } => match (bits, signed) {
                (8, true) => "Byte",
                (8, false) => "Octet",
                (16, true) => "Short",
                (16, false) => "UnsignedShort",
                (32, true) => "Long",
                (32, false) => "UnsignedLong",
                (_, true) => "LongLong",
                (_, false) => "UnsignedLongLong",
            }
            .into(),
            Conv::BigInt => "BigInt".into(),
            Conv::Float { double, unrestricted } => format!(
                "{}{}",
                if *unrestricted { "Unrestricted" } else { "" },
                if *double { "Double" } else { "Float" }
            ),
            Conv::String { kind, .. } => match kind {
                StringType::DOMString => "DomString",
                StringType::USVString => "UsvString",
                StringType::ByteString => "ByteString",
            }
            .into(),
            Conv::Object => "Object".into(),
            Conv::Symbol => "Symbol".into(),
            Conv::Buffer { ty, .. } => ty.name().into(),
            Conv::Interface(n)
            | Conv::CallbackInterface(n)
            | Conv::CallbackFunction(n)
            | Conv::Dictionary(n)
            | Conv::Enum(n) => type_ident(n),
            Conv::Nullable(i) => format!("Nullable{}", self.label(i)),
            Conv::Sequence(i) => format!("SequenceOf{}", self.label(i)),
            Conv::AsyncSequence(i) => format!("AsyncSequenceOf{}", self.label(i)),
            Conv::FrozenArray(i) => format!("FrozenArrayOf{}", self.label(i)),
            Conv::ObservableArray(i) => format!("ObservableArrayOf{}", self.label(i)),
            Conv::Record { value, .. } => format!("RecordOf{}", self.label(value)),
            Conv::Promise(i) => format!("PromiseOf{}", self.label(i)),
            Conv::Union(u) => {
                let mut parts = Vec::new();
                for m in &u.members {
                    parts.push(self.label(m));
                }
                parts.join("Or")
            }
        }
    }

    fn rust_type(&mut self, c: &Conv) -> String {
        match c {
            Conv::Any => "rt::ScriptValue".into(),
            Conv::Undefined => "()".into(),
            Conv::Boolean => "bool".into(),
            Conv::Integer { bits, signed, .. } => int_type(*bits, *signed).into(),
            Conv::BigInt => "rt::BigInt".into(),
            Conv::Float { double, .. } => if *double { "f64" } else { "f32" }.into(),
            Conv::String { kind, .. } => match kind {
                StringType::DOMString => "DOMString",
                StringType::USVString => "USVString",
                StringType::ByteString => "ByteString",
            }
            .into(),
            Conv::Object | Conv::AsyncSequence(_) => "rt::ObjectHandle".into(),
            Conv::Symbol => "rt::SymbolHandle".into(),
            Conv::Buffer { .. } => "rt::BufferHandle".into(),
            Conv::Interface(n) => format!("rt::Handle<{}>", type_ident(n)),
            Conv::CallbackInterface(n) | Conv::CallbackFunction(n) => {
                format!("rt::Callback<{}>", type_ident(n))
            }
            Conv::Dictionary(n) | Conv::Enum(n) => type_ident(n),
            Conv::Nullable(i) => format!("{OPT}<{}>", self.rust_type(i)),
            Conv::Sequence(i) | Conv::FrozenArray(i) | Conv::ObservableArray(i) => {
                format!("{VEC}<{}>", self.rust_type(i))
            }
            Conv::Record { key, value } => {
                let k = self.rust_type(&Conv::String {
                    kind: *key,
                    null_to_empty: false,
                });
                format!("{VEC}<({k}, {})>", self.rust_type(value))
            }
            Conv::Promise(i) => format!("rt::Promise<{}>", self.rust_type(i)),
            Conv::Union(u) => {
                let name = self.label(c);
                if !self.unions.contains_key(&name) {
                    let mut claims = Claims::new();
                    let mut variants = Vec::new();
                    for m in &u.members {
                        let v = claims.claim(self.label(m));
                        variants.push((v, self.rust_type(m)));
                    }
                    self.unions.insert(name.clone(), variants);
                }
                name
            }
        }
    }

    fn default_expr(&mut self, c: &Conv, d: &IrDefault) -> Result<String, EmitError> {
        let bad = || EmitError {
            message: format!("default value {d:?} does not fit {c:?}"),
        };
        if let Conv::Nullable(inner) = c {
            return Ok(match d {
                IrDefault::Null => format!("{OPT}::None"),
                _ => format!("{OPT}::Some({})", self.default_expr(inner, d)?),
            });
        }
        Ok(match d {
            IrDefault::Bool(b) => b.to_string(),
            IrDefault::Int(n) => format!("{n}"),
            IrDefault::Float(f) => format!("{f:?}"),
            IrDefault::Infinity => float_const(c, "INFINITY"),
            IrDefault::NegInfinity => float_const(c, "NEG_INFINITY"),
            IrDefault::NaN => float_const(c, "NAN"),
            IrDefault::Str(s) => match c {
                Conv::String { kind: StringType::ByteString, .. } => {
                    format!("{s:?}.as_bytes().to_vec()")
                }
                _ => format!("::std::string::String::from({s:?})"),
            },
            IrDefault::EnumValue { enum_name, value } => {
                let e = self.b.enums.iter().find(|e| &e.name == enum_name).ok_or_else(bad)?;
                let vs = variant_names(&e.values);
                let pos = e.values.iter().position(|x| x == value).ok_or_else(bad)?;
                format!("{}::{}", type_ident(enum_name), vs.get(pos).ok_or_else(bad)?)
            }
            IrDefault::EmptySequence => format!("{VEC}::new()"),
            IrDefault::EmptyDictionary => match c {
                Conv::Record { .. } => format!("{VEC}::new()"),
                _ => "::std::default::Default::default()".into(),
            },
            IrDefault::Null => match c {
                Conv::Any => "rt::ScriptValue::Null".into(),
                _ => return Err(bad()),
            },
            IrDefault::Undefined => match c {
                Conv::Any => "rt::ScriptValue::Undefined".into(),
                Conv::Undefined => "()".into(),
                _ => return Err(bad()),
            },
            IrDefault::UnionMember { index, value } => {
                let Conv::Union(u) = c else { return Err(bad()) };
                let name = self.rust_type(c);
                let variants = self.unions.get(&name).cloned().ok_or_else(bad)?;
                let (variant, _) = variants.get(*index).ok_or_else(bad)?;
                let m = u.members.get(*index).ok_or_else(bad)?;
                format!("{name}::{variant}({})", self.default_expr(m, value)?)
            }
        })
    }

    fn run(mut self) -> Result<String, EmitError> {
        // Every top-level name must be distinct after PascalCasing.
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        let mut reserve = |n: &str, what: &'static str, host: bool| -> Result<(), EmitError> {
            let id = if host {
                format!("{}Host", type_ident(n))
            } else {
                type_ident(n)
            };
            if let Some(prev) = seen.insert(id.clone(), what) {
                return Err(EmitError {
                    message: format!("`{id}` ({what}) collides with a {prev}"),
                });
            }
            Ok(())
        };
        for i in &self.b.interfaces {
            reserve(&i.name, "interface", false)?;
            if i.kind != ContainerKind::CallbackInterface {
                reserve(&i.name, "host trait", true)?;
            }
        }
        for d in &self.b.dictionaries {
            reserve(&d.name, "dictionary", false)?;
        }
        for e in &self.b.enums {
            reserve(&e.name, "enum", false)?;
        }
        for c in &self.b.callbacks {
            reserve(&c.name, "callback", false)?;
        }

        self.line("// @generated by px-bindings (emitter `rust-host`). Do not edit.");
        self.line("//");
        self.line("// IDL definitions in, host traits out: the host (the DOM) implements the");
        self.line("// `*Host` traits; the engine layer converts script values to the types");
        self.line("// named here. Every method is fallible: a stale handle is an `Err`.");
        self.line("");
        self.line("pub type DOMString = ::std::string::String;");
        self.line("pub type USVString = ::std::string::String;");
        self.line("pub type ByteString = ::std::vec::Vec<u8>;");
        self.line("");
        self.out.push_str(RUNTIME_PRELUDE);

        let b = self.b;
        for e in &b.enums {
            self.enum_(e)?;
        }
        for d in &b.dictionaries {
            self.dictionary(d)?;
        }
        for c in &b.callbacks {
            self.line("");
            self.w(format_args!(
                "/// Marker for the callback `{}`; script functions are `rt::Callback<{}>`.",
                c.name,
                type_ident(&c.name)
            ));
            self.line("#[allow(non_camel_case_types)]");
            self.w(format_args!("pub enum {} {{}}", type_ident(&c.name)));
            // Register types the signature mentions so unions are emitted.
            self.overload_types(&c.overload);
        }
        for i in &b.interfaces {
            self.interface(i)?;
        }
        self.emit_unions();
        Ok(self.out)
    }

    fn overload_types(&mut self, o: &Overload) {
        for a in &o.args {
            self.rust_type(&a.conv);
        }
        self.rust_type(&o.ret);
    }

    fn enum_(&mut self, e: &crate::ir::EnumBinding) -> Result<(), EmitError> {
        let name = type_ident(&e.name);
        let vs = variant_names(&e.values);
        self.line("");
        self.w(format_args!("/// IDL enum `{}`.", e.name));
        self.line("#[allow(non_camel_case_types)]");
        self.line("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
        self.w(format_args!("pub enum {name} {{"));
        for v in &vs {
            self.w(format_args!("    {v},"));
        }
        self.line("}");
        self.w(format_args!("impl {name} {{"));
        self.w(format_args!("    pub const ALL: &'static [{name}] = &["));
        for v in &vs {
            self.w(format_args!("        {name}::{v},"));
        }
        self.line("    ];");
        self.line("    /// The IDL string for this value.");
        self.line("    pub fn as_idl_str(self) -> &'static str {");
        self.line("        match self {");
        for (v, s) in vs.iter().zip(&e.values) {
            self.w(format_args!("            {name}::{v} => {s:?},"));
        }
        self.line("        }");
        self.line("    }");
        self.line("    /// Exact, case-sensitive match; `None` means the script layer throws a `TypeError`.");
        self.w(format_args!(
            "    pub fn from_idl_str(s: &str) -> {OPT}<{name}> {{"
        ));
        self.line("        match s {");
        for (v, s) in vs.iter().zip(&e.values) {
            self.w(format_args!("            {s:?} => {OPT}::Some({name}::{v}),"));
        }
        self.w(format_args!("            _ => {OPT}::None,"));
        self.line("        }");
        self.line("    }");
        self.line("}");
        Ok(())
    }

    fn dictionary(&mut self, d: &DictionaryBinding) -> Result<(), EmitError> {
        let name = type_ident(&d.name);
        let mut claims = Claims::new();
        let parent_field = d.parent.as_ref().map(|_| claims.claim("parent".to_owned()));
        let mut fields: Vec<(String, String, String, Option<String>)> = Vec::new(); // ident, type, doc, default expr
        for m in &d.members {
            let ident = claims.claim(snake_case(&m.name));
            let base = self.rust_type(&m.conv);
            let (ty, def) = match (&m.default, m.required) {
                (_, true) => (base, None),
                (Some(dv), false) => {
                    let e = self.default_expr(&m.conv, dv)?;
                    (base, Some(e))
                }
                (None, false) => (format!("{OPT}<{base}>"), Some(format!("{OPT}::None"))),
            };
            fields.push((value_ident(&ident), ty, m.name.clone(), def));
        }
        self.line("");
        self.w(format_args!("/// IDL dictionary `{}`.", d.name));
        let order: Vec<String> = d
            .conversion_order
            .iter()
            .map(|r| format!("{}.{}", r.owner, r.name))
            .collect();
        self.w(format_args!("/// Script-object conversion order: {}.", order.join(", ")));
        self.line("#[allow(non_camel_case_types)]");
        self.line("#[derive(Debug, Clone, PartialEq)]");
        self.w(format_args!("pub struct {name} {{"));
        if let (Some(p), Some(f)) = (&d.parent, &parent_field) {
            self.w(format_args!("    /// Members inherited from `{p}`."));
            self.w(format_args!("    pub {}: {},", value_ident(f), type_ident(p)));
        }
        for (ident, ty, idl, _) in &fields {
            self.w(format_args!("    /// `{idl}`"));
            self.w(format_args!("    pub {ident}: {ty},"));
        }
        self.line("}");
        // `has_required` covers the whole inheritance chain.
        if !d.has_required {
            self.w(format_args!("impl ::std::default::Default for {name} {{"));
            self.line("    fn default() -> Self {");
            self.w(format_args!("        {name} {{"));
            if let Some(f) = &parent_field {
                self.w(format_args!(
                    "            {}: ::std::default::Default::default(),",
                    value_ident(f)
                ));
            }
            for (ident, _, _, def) in &fields {
                let e = def.clone().unwrap_or_default();
                self.w(format_args!("            {ident}: {e},"));
            }
            self.line("        }");
            self.line("    }");
            self.line("}");
        }
        Ok(())
    }

    fn sig_args(&mut self, o: &Overload, claims: &mut Claims) -> String {
        let mut s = String::new();
        for a in &o.args {
            let ident = claims.claim(snake_case(&a.name));
            let mut ty = self.rust_type(&a.conv);
            if a.variadic {
                ty = format!("{VEC}<{ty}>");
            } else if a.optional && a.default.is_none() {
                ty = format!("{OPT}<{ty}>");
            }
            let _ = write!(s, ", {}: {ty}", value_ident(&ident));
        }
        s
    }

    fn method(
        &mut self,
        doc: &str,
        name: &str,
        this: Option<&str>,
        o: &Overload,
        ret: &str,
    ) {
        let mut claims = Claims::new();
        claims.claim("self".to_owned());
        claims.claim("this".to_owned());
        let args = self.sig_args(o, &mut claims);
        self.w(format_args!("    /// {doc}"));
        let this_arg = this.map_or(String::new(), |t| format!(", this: rt::Handle<{t}>"));
        self.w(format_args!(
            "    fn {name}(&mut self{this_arg}{args}) -> {RESULT}<{ret}>;"
        ));
    }

    fn interface(&mut self, i: &InterfaceBinding) -> Result<(), EmitError> {
        let ty = type_ident(&i.name);
        self.line("");
        self.w(format_args!(
            "/// {} `{}`{}.",
            match i.kind {
                ContainerKind::Interface => "Interface",
                ContainerKind::CallbackInterface => "Callback interface",
                ContainerKind::Namespace => "Namespace",
            },
            i.name,
            match &i.exposure {
                Exposure::Unspecified => String::new(),
                Exposure::All => ", exposed everywhere".to_owned(),
                Exposure::Globals(g) => format!(", exposed in {}", g.join(", ")),
            }
        ));
        if i.flags.secure_context {
            self.line("/// Secure contexts only.");
        }
        self.line("#[allow(non_camel_case_types)]");
        self.w(format_args!("pub enum {ty} {{}}"));
        self.w(format_args!("impl {ty} {{"));
        self.w(format_args!("    pub const NAME: &'static str = {:?};", i.name));
        for c in &i.constants {
            let t = self.rust_type(&c.conv);
            let v = match &c.value {
                IrConst::Bool(b) => b.to_string(),
                IrConst::Int(n) => n.to_string(),
                IrConst::Float(f) => format!("{f:?}"),
                IrConst::Infinity => float_const(&c.conv, "INFINITY"),
                IrConst::NegInfinity => float_const(&c.conv, "NEG_INFINITY"),
                IrConst::NaN => float_const(&c.conv, "NAN"),
            };
            let name = snake_case(&c.name).to_ascii_uppercase();
            self.w(format_args!("    pub const {name}: {t} = {v};"));
        }
        self.line("}");
        for a in &i.ancestors {
            self.w(format_args!("impl rt::Upcast<{}> for {ty} {{}}", type_ident(a)));
        }
        if i.kind == ContainerKind::CallbackInterface {
            // Implemented by script objects; the host only holds callbacks.
            for o in &i.operations {
                for ov in &o.overloads {
                    self.overload_types(ov);
                }
            }
            return Ok(());
        }
        let sup = i
            .parent
            .as_ref()
            .map_or(String::new(), |p| format!(": {}Host", type_ident(p)));
        self.line("");
        self.w(format_args!("/// What the host implements for `{}`.", i.name));
        self.line("#[allow(non_camel_case_types)]");
        self.w(format_args!("pub trait {ty}Host{sup} {{"));
        let mut claims = Claims::new();
        let this_ty = ty.as_str();
        for c in &i.constructors {
            let name = claims.claim("construct".to_owned());
            let ret = format!("rt::Handle<{ty}>");
            self.method("Constructor.", &name, None, c, &ret);
        }
        for a in &i.attributes {
            let t = self.rust_type(&a.conv);
            let this = (!a.is_static).then_some(this_ty);
            let this_arg = this.map_or(String::new(), |t| format!(", this: rt::Handle<{t}>"));
            let mut notes = Vec::new();
            if a.flags.same_object {
                notes.push("[SameObject]: return the same object each time");
            }
            if a.flags.secure_context {
                notes.push("[SecureContext]");
            }
            if a.flags.legacy_unforgeable {
                notes.push("[LegacyUnforgeable]");
            }
            let n = if notes.is_empty() { String::new() } else { format!(" {}", notes.join("; ")) };
            let g = claims.claim(snake_case(&a.name));
            self.w(format_args!("    /// Getter for `{}`.{n}", a.name));
            self.w(format_args!(
                "    fn {}(&mut self{this_arg}) -> {RESULT}<{t}>;",
                value_ident(&g)
            ));
            if !a.readonly && a.flags.put_forwards.is_none() {
                let s = claims.claim(format!("set_{}", snake_case(&a.name)));
                self.w(format_args!("    /// Setter for `{}`.", a.name));
                self.w(format_args!(
                    "    fn {}(&mut self{this_arg}, value: {t}) -> {RESULT}<()>;",
                    value_ident(&s)
                ));
            }
        }
        if i.has_bare_stringifier {
            let name = claims.claim("stringifier".to_owned());
            self.w(format_args!("    /// `stringifier;`"));
            self.w(format_args!(
                "    fn {name}(&mut self, this: rt::Handle<{ty}>) -> {RESULT}<DOMString>;"
            ));
        }
        for op in &i.operations {
            let base = match (&op.name, op.special) {
                (Some(n), _) => snake_case(n),
                (None, Some(Special::Getter)) => "anonymous_getter".into(),
                (None, Some(Special::Setter)) => "anonymous_setter".into(),
                (None, Some(Special::Deleter)) => "anonymous_deleter".into(),
                (None, None) => "anonymous".into(),
            };
            for (n, ov) in op.overloads.iter().enumerate() {
                let ident = if n == 0 { base.clone() } else { format!("{base}_{n}") };
                let ident = value_ident(&claims.claim(ident));
                let ret = self.rust_type(&ov.ret);
                let this = (!op.is_static).then_some(this_ty);
                let doc = match op.special {
                    Some(Special::Getter) => "Special operation: getter.",
                    Some(Special::Setter) => "Special operation: setter.",
                    Some(Special::Deleter) => "Special operation: deleter.",
                    None if op.is_static => "Static operation.",
                    None => "Operation.",
                };
                let doc = if op.flags.new_object {
                    format!("{doc} [NewObject]: returns a fresh object.")
                } else {
                    doc.to_owned()
                };
                self.method(&doc, &ident, this, ov, &ret);
            }
        }
        if let Some(col) = &i.collection {
            let k = col.key.as_ref().map(|c| self.rust_type(c));
            let v = self.rust_type(&col.value);
            let entry = match &k {
                Some(k) => format!("({k}, {v})"),
                None => v.clone(),
            };
            let this_arg = format!(", this: rt::Handle<{ty}>");
            match col.kind {
                CollectionKind::Iterable => {
                    let n = claims.claim("iterable_entry".to_owned());
                    self.line("    /// `iterable`: entry at `index`, `None` past the end.");
                    self.w(format_args!(
                        "    fn {n}(&mut self{this_arg}, index: u32) -> {RESULT}<{OPT}<{entry}>>;"
                    ));
                }
                CollectionKind::AsyncIterable => {
                    self.line("    // `async_iterable` is recorded in the IR; the engine emitter binds it.");
                }
                CollectionKind::Maplike | CollectionKind::Setlike => {
                    let p = if col.kind == CollectionKind::Maplike { "maplike" } else { "setlike" };
                    let key_ty = k.clone().unwrap_or_else(|| v.clone());
                    let n = claims.claim(format!("{p}_size"));
                    self.w(format_args!("    fn {n}(&mut self{this_arg}) -> {RESULT}<u32>;"));
                    let n = claims.claim(format!("{p}_entry"));
                    self.w(format_args!(
                        "    fn {n}(&mut self{this_arg}, index: u32) -> {RESULT}<{OPT}<{entry}>>;"
                    ));
                    let n = claims.claim(format!("{p}_has"));
                    self.w(format_args!(
                        "    fn {n}(&mut self{this_arg}, key: {key_ty}) -> {RESULT}<bool>;"
                    ));
                    if col.kind == CollectionKind::Maplike {
                        let n = claims.claim("maplike_get".to_owned());
                        self.w(format_args!(
                            "    fn {n}(&mut self{this_arg}, key: {key_ty}) -> {RESULT}<{OPT}<{v}>>;"
                        ));
                    }
                    if !col.readonly {
                        if col.kind == CollectionKind::Maplike {
                            let n = claims.claim("maplike_set".to_owned());
                            self.w(format_args!(
                                "    fn {n}(&mut self{this_arg}, key: {key_ty}, value: {v}) -> {RESULT}<()>;"
                            ));
                        } else {
                            let n = claims.claim("setlike_add".to_owned());
                            self.w(format_args!(
                                "    fn {n}(&mut self{this_arg}, value: {v}) -> {RESULT}<()>;"
                            ));
                        }
                        let n = claims.claim(format!("{p}_delete"));
                        self.w(format_args!(
                            "    fn {n}(&mut self{this_arg}, key: {key_ty}) -> {RESULT}<bool>;"
                        ));
                        let n = claims.claim(format!("{p}_clear"));
                        self.w(format_args!("    fn {n}(&mut self{this_arg}) -> {RESULT}<()>;"));
                    }
                }
            }
        }
        self.line("}");
        Ok(())
    }

    fn emit_unions(&mut self) {
        // Emitting a union can only have registered it already; nested unions
        // were registered while their parents were typed.
        let unions = std::mem::take(&mut self.unions);
        for (name, variants) in &unions {
            self.line("");
            self.w(format_args!("/// Flattened IDL union."));
            self.line("#[allow(non_camel_case_types)]");
            self.line("#[derive(Debug, Clone, PartialEq)]");
            self.w(format_args!("pub enum {name} {{"));
            for (v, t) in variants {
                self.w(format_args!("    {v}({t}),"));
            }
            self.line("}");
        }
    }
}

fn float_const(c: &Conv, name: &str) -> String {
    let ty = match c {
        Conv::Float { double: false, .. } => "f32",
        _ => "f64",
    };
    format!("::std::{ty}::{name}")
}

fn variant_names(values: &[String]) -> Vec<String> {
    let mut claims = Claims::new();
    values.iter().map(|v| claims.claim(variant_ident(v))).collect()
}

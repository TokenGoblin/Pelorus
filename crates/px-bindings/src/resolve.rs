//! Semantic checking: name resolution across files, partial and mixin
//! merging, inheritance validity, typedef expansion, duplicate detection and
//! `[Exposed]` sets, producing the resolved `Model` the IR is lowered from.
//!
//! Everything is bounded: a typedef chain deeper than `MAX_TYPEDEF_DEPTH` or a
//! type that expands past `MAX_TYPE_NODES` nodes is an error, and the graph
//! walks (inheritance, dictionary containment) are iterative.

use crate::ast::{
    self, Ast, BufferType, ConstValue, DefaultValue, DefinitionKind, ExtAttr, ExtAttrValue,
    Special, StringType, TypeKind, find_ext,
};
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MAX_TYPEDEF_DEPTH: u32 = 64;
pub const MAX_TYPE_NODES: u32 = 4096;
/// Longest interface or dictionary inheritance chain accepted.
pub const MAX_INHERITANCE_DEPTH: usize = 256;
const MAX_ERRORS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticError {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: error: {}",
            self.file, self.line, self.col, self.message
        )
    }
}

impl std::error::Error for SemanticError {}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    /// Require `[Exposed]` on every interface and namespace (WebIDL section
    /// 3.3.1 says they must have it).
    pub require_exposed: bool,
}

// ---- resolved types ---------------------------------------------------

/// Extended attributes that change how a value of the type converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeExt {
    pub clamp: bool,
    pub enforce_range: bool,
    pub null_to_empty: bool,
    pub allow_shared: bool,
    pub allow_resizable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RType {
    pub kind: RKind,
    pub nullable: bool,
    pub ext: TypeExt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RKind {
    Any,
    Undefined,
    Boolean,
    Integer { bits: u8, signed: bool },
    Bigint,
    Float { double: bool, unrestricted: bool },
    String(StringType),
    Object,
    Symbol,
    Buffer(BufferType),
    Interface(String),
    CallbackInterface(String),
    CallbackFunction(String),
    Dictionary(String),
    Enum(String),
    Sequence(Box<RType>),
    AsyncSequence(Box<RType>),
    FrozenArray(Box<RType>),
    ObservableArray(Box<RType>),
    Record(StringType, Box<RType>),
    Promise(Box<RType>),
    Union(Vec<RType>),
}

impl RType {
    pub fn plain(kind: RKind) -> RType {
        RType {
            kind,
            nullable: false,
            ext: TypeExt::default(),
        }
    }

    /// The spec's "flattened member types" of a union (nested unions
    /// expanded); a non-union yields itself.
    pub fn flattened_members(&self) -> Vec<&RType> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(t) = stack.pop() {
            match &t.kind {
                RKind::Union(ms) => stack.extend(ms.iter().rev()),
                _ => out.push(t),
            }
        }
        out
    }
}

impl fmt::Display for RType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            RKind::Any => write!(f, "any")?,
            RKind::Undefined => write!(f, "undefined")?,
            RKind::Boolean => write!(f, "boolean")?,
            RKind::Integer { bits, signed } => {
                let u = if *signed { "" } else { "unsigned " };
                let n = match bits {
                    8 if *signed => "byte",
                    8 => "octet",
                    16 => "short",
                    32 => "long",
                    _ => "long long",
                };
                let u = if n == "octet" { "" } else { u };
                write!(f, "{u}{n}")?;
            }
            RKind::Bigint => write!(f, "bigint")?,
            RKind::Float {
                double,
                unrestricted,
            } => {
                let u = if *unrestricted { "unrestricted " } else { "" };
                write!(f, "{u}{}", if *double { "double" } else { "float" })?;
            }
            RKind::String(s) => write!(f, "{s:?}")?,
            RKind::Object => write!(f, "object")?,
            RKind::Symbol => write!(f, "symbol")?,
            RKind::Buffer(b) => write!(f, "{}", b.name())?,
            RKind::Interface(n)
            | RKind::CallbackInterface(n)
            | RKind::CallbackFunction(n)
            | RKind::Dictionary(n)
            | RKind::Enum(n) => write!(f, "{n}")?,
            RKind::Sequence(t) => write!(f, "sequence<{t}>")?,
            RKind::AsyncSequence(t) => write!(f, "async_sequence<{t}>")?,
            RKind::FrozenArray(t) => write!(f, "FrozenArray<{t}>")?,
            RKind::ObservableArray(t) => write!(f, "ObservableArray<{t}>")?,
            RKind::Record(k, v) => write!(f, "record<{k:?}, {v}>")?,
            RKind::Promise(t) => write!(f, "Promise<{t}>")?,
            RKind::Union(ms) => {
                write!(f, "(")?;
                for (i, m) in ms.iter().enumerate() {
                    if i > 0 {
                        write!(f, " or ")?;
                    }
                    write!(f, "{m}")?;
                }
                write!(f, ")")?;
            }
        }
        if self.nullable {
            write!(f, "?")?;
        }
        Ok(())
    }
}

// ---- the resolved model -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exposure {
    /// No `[Exposed]` written.
    Unspecified,
    /// `[Exposed=*]`
    All,
    /// Sorted, deduplicated global names.
    Globals(Vec<String>),
}

impl Exposure {
    /// Whether everything exposed under `self` is also exposed under `other`.
    pub fn is_subset_of(&self, other: &Exposure) -> bool {
        match (self, other) {
            (Exposure::Unspecified, _) | (_, Exposure::Unspecified) => true,
            (_, Exposure::All) => true,
            (Exposure::All, Exposure::Globals(_)) => false,
            (Exposure::Globals(a), Exposure::Globals(b)) => a.iter().all(|x| b.contains(x)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Interface,
    CallbackInterface,
    Namespace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    pub ty: RType,
    pub optional: bool,
    pub variadic: bool,
    pub default: Option<DefaultValue>,
    pub ext_attrs: Vec<ExtAttr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemberKind {
    Const {
        name: String,
        ty: RType,
        value: ConstValue,
    },
    Attribute {
        name: String,
        ty: RType,
        readonly: bool,
        inherit: bool,
        is_static: bool,
        stringifier: bool,
    },
    Operation {
        name: Option<String>,
        special: Option<Special>,
        is_static: bool,
        stringifier: bool,
        ret: RType,
        args: Vec<Arg>,
    },
    Constructor {
        args: Vec<Arg>,
    },
    Stringifier,
    Iterable {
        key: Option<RType>,
        value: RType,
    },
    AsyncIterable {
        key: Option<RType>,
        value: RType,
        args: Vec<Arg>,
    },
    Maplike {
        readonly: bool,
        key: RType,
        value: RType,
    },
    Setlike {
        readonly: bool,
        ty: RType,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
    /// The mixin this member was included from.
    pub origin: Option<String>,
    pub kind: MemberKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub kind: ContainerKind,
    pub inherits: Option<String>,
    pub ext_attrs: Vec<ExtAttr>,
    pub exposure: Exposure,
    /// Own members, partials merged, then mixin members in `includes` order.
    pub members: Vec<Member>,
    pub span: Span,
}

impl Interface {
    pub fn has_ext(&self, name: &str) -> bool {
        find_ext(&self.ext_attrs, name).is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictMember {
    pub name: String,
    pub ty: RType,
    pub required: bool,
    pub default: Option<DefaultValue>,
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dictionary {
    pub name: String,
    pub inherits: Option<String>,
    pub ext_attrs: Vec<ExtAttr>,
    pub members: Vec<DictMember>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Enum {
    pub name: String,
    pub values: Vec<String>,
    pub ext_attrs: Vec<ExtAttr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Callback {
    pub name: String,
    pub ret: RType,
    pub args: Vec<Arg>,
    pub ext_attrs: Vec<ExtAttr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Interface,
    Mixin,
    Namespace,
    CallbackInterface,
    Dictionary,
    Enum,
    Typedef,
    Callback,
}

/// The resolved IDL. Vectors keep first-definition order (files in the order
/// given), so iteration is deterministic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// Interfaces, callback interfaces and namespaces.
    pub containers: Vec<Interface>,
    pub dictionaries: Vec<Dictionary>,
    pub enums: Vec<Enum>,
    pub callbacks: Vec<Callback>,
    /// Typedef names, for documentation; uses are already expanded.
    pub typedefs: Vec<String>,
    /// Mixin names, for documentation; their members are merged away.
    pub mixins: Vec<String>,
    index: BTreeMap<String, (NameKind, usize)>,
}

impl Model {
    pub fn container(&self, name: &str) -> Option<&Interface> {
        match self.index.get(name) {
            Some((NameKind::Interface | NameKind::CallbackInterface | NameKind::Namespace, i)) => {
                self.containers.get(*i)
            }
            _ => None,
        }
    }

    pub fn dictionary(&self, name: &str) -> Option<&Dictionary> {
        match self.index.get(name) {
            Some((NameKind::Dictionary, i)) => self.dictionaries.get(*i),
            _ => None,
        }
    }

    pub fn enumeration(&self, name: &str) -> Option<&Enum> {
        match self.index.get(name) {
            Some((NameKind::Enum, i)) => self.enums.get(*i),
            _ => None,
        }
    }

    /// The interface chain from `name` up to the root, nearest first
    /// (excluding `name`). Bounded by the number of containers.
    pub fn ancestors(&self, name: &str) -> Vec<&Interface> {
        let mut out = Vec::new();
        let mut cur = self.container(name).and_then(|c| c.inherits.as_deref());
        while let Some(n) = cur {
            if out.len() > MAX_INHERITANCE_DEPTH {
                break;
            }
            let Some(c) = self.container(n) else { break };
            out.push(c);
            cur = c.inherits.as_deref();
        }
        out
    }

    /// Parse and resolve sources given as `(file name, text)`.
    pub fn from_sources(
        sources: &[(String, String)],
        opts: BuildOptions,
    ) -> Result<Model, Vec<String>> {
        let mut asts = Vec::new();
        for (i, (name, text)) in sources.iter().enumerate() {
            let id = u32::try_from(i).map_err(|_| vec!["too many files".to_owned()])?;
            match crate::parser::parse(id, name, text) {
                Ok(a) => asts.push(a),
                Err(e) => return Err(vec![e.to_string()]),
            }
        }
        Model::build(&asts, opts).map_err(|es| es.iter().map(ToString::to_string).collect())
    }

    pub fn build(asts: &[Ast], opts: BuildOptions) -> Result<Model, Vec<SemanticError>> {
        let mut b = Builder {
            asts,
            opts,
            errors: Vec::new(),
            names: BTreeMap::new(),
            typedefs: BTreeMap::new(),
            containers: BTreeMap::new(),
            dicts: BTreeMap::new(),
            order: Vec::new(),
        };
        let model = b.run();
        if b.errors.is_empty() {
            Ok(model)
        } else {
            Err(b.errors)
        }
    }
}

// ---- builder -------------------------------------------------------------

struct MSrc<'a> {
    m: &'a ast::Member,
    origin: Option<String>,
    inherited_ext: Vec<ExtAttr>,
}

struct CSrc<'a> {
    kind: NameKind,
    inherits: Option<&'a str>,
    ext: &'a [ExtAttr],
    span: Span,
    members: Vec<MSrc<'a>>,
}

struct DSrc<'a> {
    inherits: Option<&'a str>,
    ext: &'a [ExtAttr],
    span: Span,
    members: Vec<&'a ast::DictMember>,
}

struct Builder<'a> {
    asts: &'a [Ast],
    opts: BuildOptions,
    errors: Vec<SemanticError>,
    names: BTreeMap<&'a str, (NameKind, Span)>,
    typedefs: BTreeMap<&'a str, &'a ast::Type>,
    containers: BTreeMap<&'a str, CSrc<'a>>,
    dicts: BTreeMap<&'a str, DSrc<'a>>,
    order: Vec<(&'a str, NameKind)>,
}

/// Interpret an integer literal (decimal, hex or octal, optional minus).
pub fn parse_integer(text: &str) -> Option<i128> {
    let (neg, body) = match text.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if body.len() > 1 && body.starts_with('0') {
        i128::from_str_radix(body.get(1..)?, 8).ok()?
    } else {
        body.parse::<i128>().ok()?
    };
    if neg { v.checked_neg() } else { Some(v) }
}

/// Inclusive range of an IDL integer type.
pub fn int_range(bits: u8, signed: bool) -> (i128, i128) {
    let b = u32::from(bits);
    if signed {
        (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1)
    } else {
        (0, (1i128 << b) - 1)
    }
}

impl<'a> Builder<'a> {
    fn file_name(&self, file: u32) -> String {
        self.asts
            .iter()
            .find(|a| a.file == file)
            .map_or_else(String::new, |a| a.file_name.clone())
    }

    fn err(&mut self, span: Span, message: impl Into<String>) {
        if self.errors.len() >= MAX_ERRORS {
            return;
        }
        let file = self.file_name(span.file);
        self.errors.push(SemanticError {
            file,
            line: span.line,
            col: span.col,
            message: message.into(),
        });
    }

    fn where_(&self, span: Span) -> String {
        format!("{}:{}:{}", self.file_name(span.file), span.line, span.col)
    }

    fn run(&mut self) -> Model {
        self.collect();
        let mut model = Model::default();
        // Containers, in definition order.
        let order = self.order.clone();
        for (name, kind) in order {
            match kind {
                NameKind::Interface | NameKind::CallbackInterface | NameKind::Namespace => {
                    if let Some(i) = self.build_container(name) {
                        model.index.insert(
                            name.to_owned(),
                            (kind, model.containers.len()),
                        );
                        model.containers.push(i);
                    }
                }
                NameKind::Dictionary => {
                    if let Some(d) = self.build_dictionary(name) {
                        model
                            .index
                            .insert(name.to_owned(), (kind, model.dictionaries.len()));
                        model.dictionaries.push(d);
                    }
                }
                NameKind::Enum => {
                    // Built during collection into `enums` below.
                }
                NameKind::Callback => {}
                NameKind::Typedef => model.typedefs.push(name.to_owned()),
                NameKind::Mixin => model.mixins.push(name.to_owned()),
            }
        }
        self.enums_and_callbacks(&mut model);
        self.check_inheritance(&model);
        self.check_dictionary_cycles(&model);
        model
    }

    fn declare(&mut self, name: &'a str, kind: NameKind, span: Span) -> bool {
        if let Some((_, prev)) = self.names.get(name) {
            let at = self.where_(*prev);
            self.err(span, format!("`{name}` is already defined at {at}"));
            return false;
        }
        self.names.insert(name, (kind, span));
        self.order.push((name, kind));
        true
    }

    fn collect(&mut self) {
        let asts = self.asts;
        // Pass 1: defining declarations.
        for ast in asts {
            for d in &ast.definitions {
                match &d.kind {
                    DefinitionKind::Interface(c)
                    | DefinitionKind::InterfaceMixin(c)
                    | DefinitionKind::Namespace(c)
                    | DefinitionKind::CallbackInterface(c)
                        if !c.partial =>
                    {
                        let kind = match &d.kind {
                            DefinitionKind::Interface(_) => NameKind::Interface,
                            DefinitionKind::InterfaceMixin(_) => NameKind::Mixin,
                            DefinitionKind::Namespace(_) => NameKind::Namespace,
                            _ => NameKind::CallbackInterface,
                        };
                        if self.declare(&c.name, kind, d.span) {
                            self.containers.insert(
                                &c.name,
                                CSrc {
                                    kind,
                                    inherits: c.inherits.as_deref(),
                                    ext: &d.ext_attrs,
                                    span: d.span,
                                    members: Vec::new(),
                                },
                            );
                        }
                    }
                    DefinitionKind::Dictionary(dd) if !dd.partial => {
                        if self.declare(&dd.name, NameKind::Dictionary, d.span) {
                            self.dicts.insert(
                                &dd.name,
                                DSrc {
                                    inherits: dd.inherits.as_deref(),
                                    ext: &d.ext_attrs,
                                    span: d.span,
                                    members: Vec::new(),
                                },
                            );
                        }
                    }
                    DefinitionKind::Enum(e) => {
                        self.declare(&e.name, NameKind::Enum, d.span);
                    }
                    DefinitionKind::Typedef(t) => {
                        if self.declare(&t.name, NameKind::Typedef, d.span) {
                            self.typedefs.insert(&t.name, &t.ty);
                        }
                    }
                    DefinitionKind::Callback(c) => {
                        self.declare(&c.name, NameKind::Callback, d.span);
                    }
                    _ => {}
                }
            }
        }
        // Pass 2: members, partials, includes, in source order. Own members
        // of a non-partial definition come before any partial's.
        for want_partial in [false, true] {
          for ast in asts {
            for d in &ast.definitions {
                match &d.kind {
                    DefinitionKind::Interface(c)
                    | DefinitionKind::InterfaceMixin(c)
                    | DefinitionKind::Namespace(c)
                    | DefinitionKind::CallbackInterface(c) => {
                        if c.partial == want_partial {
                            self.add_members(c, d);
                        }
                    }
                    DefinitionKind::Dictionary(dd) if dd.partial == want_partial => {
                        match self.dicts.get_mut(dd.name.as_str()) {
                            Some(src) => src.members.extend(dd.members.iter()),
                            None => {
                                if dd.partial {
                                    self.err(
                                        d.span,
                                        format!(
                                            "partial dictionary `{}` has no dictionary to extend",
                                            dd.name
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
          }
        }
        // Pass 3: includes statements, after every mixin has its members.
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        for ast in asts {
            for d in &ast.definitions {
                let DefinitionKind::Includes(inc) = &d.kind else {
                    continue;
                };
                let target_ok = matches!(
                    self.names.get(inc.target.as_str()),
                    Some((NameKind::Interface, _))
                );
                let mixin_ok = matches!(
                    self.names.get(inc.mixin.as_str()),
                    Some((NameKind::Mixin, _))
                );
                if !target_ok {
                    self.err(
                        d.span,
                        format!("`{}` in an includes statement is not an interface", inc.target),
                    );
                    continue;
                }
                if !mixin_ok {
                    self.err(
                        d.span,
                        format!("`{}` in an includes statement is not an interface mixin", inc.mixin),
                    );
                    continue;
                }
                if !seen.insert((inc.target.as_str(), inc.mixin.as_str())) {
                    continue;
                }
                let moved: Vec<(&'a ast::Member, Vec<ExtAttr>)> = self
                    .containers
                    .get(inc.mixin.as_str())
                    .map(|m| m.members.iter().map(|x| (x.m, x.inherited_ext.clone())).collect())
                    .unwrap_or_default();
                if let Some(t) = self.containers.get_mut(inc.target.as_str()) {
                    for (m, ext) in moved {
                        t.members.push(MSrc {
                            m,
                            origin: Some(inc.mixin.clone()),
                            inherited_ext: ext,
                        });
                    }
                }
            }
        }
    }

    fn add_members(&mut self, c: &'a ast::Container, d: &'a ast::Definition) {
        let inherited_ext: Vec<ExtAttr> = if c.partial {
            d.ext_attrs
                .iter()
                .filter(|a| a.name == "Exposed" || a.name == "SecureContext")
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let want = match &d.kind {
            DefinitionKind::Interface(_) => NameKind::Interface,
            DefinitionKind::InterfaceMixin(_) => NameKind::Mixin,
            DefinitionKind::Namespace(_) => NameKind::Namespace,
            _ => NameKind::CallbackInterface,
        };
        let have = self.containers.get(c.name.as_str()).map(|s| s.kind);
        match have {
            Some(k) if k == want => {
                if let Some(src) = self.containers.get_mut(c.name.as_str()) {
                    for m in &c.members {
                        src.members.push(MSrc {
                            m,
                            origin: None,
                            inherited_ext: inherited_ext.clone(),
                        });
                    }
                }
            }
            Some(_) => self.err(
                d.span,
                format!("partial definition of `{}` does not match its kind", c.name),
            ),
            None => {
                if c.partial {
                    let msg = match self.names.get(c.name.as_str()) {
                        Some(_) => format!("partial definition of `{}` does not match its kind", c.name),
                        None => format!("partial definition of `{}` has nothing to extend", c.name),
                    };
                    self.err(d.span, msg);
                }
            }
        }
    }

    // ---- types ----------------------------------------------------------

    fn rtype(&mut self, t: &ast::Type, extra: &[ExtAttr]) -> Option<RType> {
        let mut budget = MAX_TYPE_NODES;
        let mut r = self.rtype_in(t, 0, &mut budget)?;
        for a in extra {
            apply_type_ext(&mut r.ext, a);
        }
        self.check_type_ext(&r, t.span)?;
        self.check_union(&r, t.span)?;
        Some(r)
    }

    fn rtype_in(&mut self, t: &ast::Type, depth: u32, budget: &mut u32) -> Option<RType> {
        if *budget == 0 {
            self.err(
                t.span,
                format!("type expands to more than {MAX_TYPE_NODES} nodes"),
            );
            return None;
        }
        *budget -= 1;
        let mut ext = TypeExt::default();
        for a in &t.ext_attrs {
            apply_type_ext(&mut ext, a);
        }
        let mut nullable = t.nullable;
        let sub = |this: &mut Self, x: &ast::Type, budget: &mut u32| this.rtype_in(x, depth, budget);
        let kind = match &t.kind {
            TypeKind::Any => RKind::Any,
            TypeKind::Undefined => RKind::Undefined,
            TypeKind::Boolean => RKind::Boolean,
            TypeKind::Byte => RKind::Integer { bits: 8, signed: true },
            TypeKind::Octet => RKind::Integer { bits: 8, signed: false },
            TypeKind::Short { unsigned } => RKind::Integer { bits: 16, signed: !unsigned },
            TypeKind::Long { unsigned } => RKind::Integer { bits: 32, signed: !unsigned },
            TypeKind::LongLong { unsigned } => RKind::Integer { bits: 64, signed: !unsigned },
            TypeKind::Float { unrestricted } => RKind::Float { double: false, unrestricted: *unrestricted },
            TypeKind::Double { unrestricted } => RKind::Float { double: true, unrestricted: *unrestricted },
            TypeKind::Bigint => RKind::Bigint,
            TypeKind::String(s) => RKind::String(*s),
            TypeKind::Object => RKind::Object,
            TypeKind::Symbol => RKind::Symbol,
            TypeKind::Buffer(b) => RKind::Buffer(*b),
            TypeKind::Sequence(i) => RKind::Sequence(Box::new(sub(self, i, budget)?)),
            TypeKind::AsyncSequence(i) => RKind::AsyncSequence(Box::new(sub(self, i, budget)?)),
            TypeKind::FrozenArray(i) => RKind::FrozenArray(Box::new(sub(self, i, budget)?)),
            TypeKind::ObservableArray(i) => RKind::ObservableArray(Box::new(sub(self, i, budget)?)),
            TypeKind::Record(k, v) => RKind::Record(*k, Box::new(sub(self, v, budget)?)),
            TypeKind::Promise(i) => RKind::Promise(Box::new(sub(self, i, budget)?)),
            TypeKind::Union(ms) => {
                let mut out = Vec::new();
                for m in ms {
                    out.push(sub(self, m, budget)?);
                }
                RKind::Union(out)
            }
            TypeKind::Named(n) => {
                let Some((nk, _)) = self.names.get(n.as_str()).copied() else {
                    self.err(t.span, format!("unknown type `{n}`"));
                    return None;
                };
                match nk {
                    NameKind::Interface => RKind::Interface(n.clone()),
                    NameKind::CallbackInterface => RKind::CallbackInterface(n.clone()),
                    NameKind::Callback => RKind::CallbackFunction(n.clone()),
                    NameKind::Dictionary => RKind::Dictionary(n.clone()),
                    NameKind::Enum => RKind::Enum(n.clone()),
                    NameKind::Mixin | NameKind::Namespace => {
                        self.err(t.span, format!("`{n}` cannot be used as a type"));
                        return None;
                    }
                    NameKind::Typedef => {
                        if depth >= MAX_TYPEDEF_DEPTH {
                            self.err(
                                t.span,
                                format!("typedef `{n}` is part of a cycle or a chain deeper than {MAX_TYPEDEF_DEPTH}"),
                            );
                            return None;
                        }
                        let target = *self.typedefs.get(n.as_str())?;
                        let mut inner = self.rtype_in(target, depth + 1, budget)?;
                        if inner.nullable && nullable {
                            self.err(t.span, format!("`{n}` is already nullable; `{n}?` is not allowed"));
                            return None;
                        }
                        nullable |= inner.nullable;
                        inner.nullable = false;
                        merge_ext(&mut ext, inner.ext);
                        inner.kind
                    }
                }
            }
        };
        if nullable && matches!(kind, RKind::Any | RKind::Promise(_)) {
            self.err(t.span, "`any` and `Promise` types cannot be nullable");
            return None;
        }
        Some(RType { kind, nullable, ext })
    }

    fn check_type_ext(&mut self, t: &RType, span: Span) -> Option<()> {
        let mut ok = true;
        if t.ext.clamp && t.ext.enforce_range {
            self.err(span, "[Clamp] and [EnforceRange] cannot be combined");
            ok = false;
        }
        if (t.ext.clamp || t.ext.enforce_range) && !matches!(t.kind, RKind::Integer { .. }) {
            self.err(span, "[Clamp] and [EnforceRange] apply only to integer types");
            ok = false;
        }
        if t.ext.null_to_empty && !matches!(t.kind, RKind::String(StringType::DOMString)) {
            self.err(span, "[LegacyNullToEmptyString] applies only to DOMString");
            ok = false;
        }
        if (t.ext.allow_shared || t.ext.allow_resizable) && !matches!(t.kind, RKind::Buffer(_)) {
            self.err(span, "[AllowShared] and [AllowResizable] apply only to buffer types");
            ok = false;
        }
        ok.then_some(())
    }

    fn check_union(&mut self, t: &RType, span: Span) -> Option<()> {
        let mut stack = vec![t];
        let mut ok = true;
        while let Some(x) = stack.pop() {
            match &x.kind {
                RKind::Sequence(i)
                | RKind::AsyncSequence(i)
                | RKind::FrozenArray(i)
                | RKind::ObservableArray(i)
                | RKind::Promise(i)
                | RKind::Record(_, i) => stack.push(i),
                RKind::Union(ms) => {
                    stack.extend(ms.iter());
                    let flat = x.flattened_members();
                    let nullable_members = flat.iter().filter(|m| m.nullable).count();
                    if nullable_members > 1 || (nullable_members == 1 && x.nullable) {
                        self.err(span, "a union may contain at most one nullable type, and not when it is itself nullable");
                        ok = false;
                    }
                    let mut seen: BTreeMap<String, String> = BTreeMap::new();
                    let mut has_dict = false;
                    for m in &flat {
                        match &m.kind {
                            RKind::Any | RKind::Promise(_) | RKind::Undefined => {
                                self.err(span, format!("`{m}` is not allowed in a union"));
                                ok = false;
                            }
                            RKind::Dictionary(_) => has_dict = true,
                            _ => {}
                        }
                        let cat = distinguish_category(&m.kind);
                        if let Some(prev) = seen.insert(cat.clone(), m.to_string()) {
                            self.err(
                                span,
                                format!("union members `{prev}` and `{m}` are not distinguishable"),
                            );
                            ok = false;
                        }
                    }
                    if has_dict && (nullable_members > 0 || x.nullable) {
                        self.err(span, "a union cannot include a dictionary and a nullable type");
                        ok = false;
                    }
                }
                _ => {}
            }
        }
        ok.then_some(())
    }

    fn args(&mut self, args: &[ast::Argument]) -> Option<Vec<Arg>> {
        let mut out = Vec::new();
        let mut ok = true;
        for (i, a) in args.iter().enumerate() {
            if a.variadic && i + 1 != args.len() {
                self.err(a.span, "a variadic argument must be the last");
                ok = false;
            }
            if out.iter().any(|x: &Arg| x.name == a.name) {
                self.err(a.span, format!("duplicate argument name `{}`", a.name));
                ok = false;
            }
            match self.rtype(&a.ty, &a.ext_attrs) {
                Some(ty) => out.push(Arg {
                    name: a.name.clone(),
                    ty,
                    optional: a.optional,
                    variadic: a.variadic,
                    default: a.default.clone(),
                    ext_attrs: a.ext_attrs.clone(),
                }),
                None => ok = false,
            }
        }
        ok.then_some(out)
    }

    // ---- containers --------------------------------------------------------

    fn exposure(&mut self, attrs: &[ExtAttr], span: Span) -> Exposure {
        let Some(a) = find_ext(attrs, "Exposed") else {
            return Exposure::Unspecified;
        };
        match &a.value {
            ExtAttrValue::Wildcard => Exposure::All,
            ExtAttrValue::Ident(g) => Exposure::Globals(vec![g.clone()]),
            ExtAttrValue::IdentList(gs) => {
                let mut v = gs.clone();
                v.sort();
                v.dedup();
                Exposure::Globals(v)
            }
            _ => {
                self.err(span, "[Exposed] takes an identifier, a list of identifiers or `*`");
                Exposure::Unspecified
            }
        }
    }

    fn build_container(&mut self, name: &'a str) -> Option<Interface> {
        let src = self.containers.remove(name)?;
        let kind = match src.kind {
            NameKind::Interface => ContainerKind::Interface,
            NameKind::Namespace => ContainerKind::Namespace,
            _ => ContainerKind::CallbackInterface,
        };
        let exposure = self.exposure(src.ext, src.span);
        if self.opts.require_exposed
            && exposure == Exposure::Unspecified
            && kind != ContainerKind::CallbackInterface
        {
            self.err(src.span, format!("`{name}` is missing an [Exposed] extended attribute"));
        }
        let mut members = Vec::new();
        for ms in &src.members {
            if let Some(m) = self.member(ms) {
                members.push(m);
            }
        }
        self.check_members(name, &members);
        Some(Interface {
            name: name.to_owned(),
            kind,
            inherits: src.inherits.map(str::to_owned),
            ext_attrs: src.ext.to_vec(),
            exposure,
            members,
            span: src.span,
        })
    }

    fn member(&mut self, ms: &MSrc<'a>) -> Option<Member> {
        let m = ms.m;
        let mut ext_attrs = m.ext_attrs.clone();
        ext_attrs.extend(ms.inherited_ext.iter().cloned());
        let kind = match &m.kind {
            ast::MemberKind::Const { ty, name, value } => {
                let ty = self.rtype(ty, &[])?;
                self.check_const(&ty, value, name, m.span)?;
                MemberKind::Const {
                    name: name.clone(),
                    ty,
                    value: value.clone(),
                }
            }
            ast::MemberKind::Attribute {
                readonly,
                inherit,
                is_static,
                stringifier,
                ty,
                name,
            } => {
                if find_ext(&m.ext_attrs, "SameObject").is_some() && !readonly {
                    self.err(m.span, "[SameObject] applies only to readonly attributes");
                }
                MemberKind::Attribute {
                    name: name.clone(),
                    ty: self.rtype(ty, &m.ext_attrs)?,
                    readonly: *readonly,
                    inherit: *inherit,
                    is_static: *is_static,
                    stringifier: *stringifier,
                }
            }
            ast::MemberKind::Operation {
                special,
                is_static,
                stringifier,
                ret,
                name,
                args,
            } => {
                let ret = self.rtype(ret, &m.ext_attrs)?;
                MemberKind::Operation {
                    name: name.clone(),
                    special: *special,
                    is_static: *is_static,
                    stringifier: *stringifier,
                    ret,
                    args: self.args(args)?,
                }
            }
            ast::MemberKind::Constructor { args } => MemberKind::Constructor {
                args: self.args(args)?,
            },
            ast::MemberKind::Stringifier => MemberKind::Stringifier,
            ast::MemberKind::Iterable(d) => {
                let (key, value) = self.iter_types(d)?;
                MemberKind::Iterable { key, value }
            }
            ast::MemberKind::AsyncIterable { decl, args } => {
                let (key, value) = self.iter_types(decl)?;
                MemberKind::AsyncIterable {
                    key,
                    value,
                    args: self.args(args)?,
                }
            }
            ast::MemberKind::Maplike { readonly, key, value } => MemberKind::Maplike {
                readonly: *readonly,
                key: self.rtype(key, &[])?,
                value: self.rtype(value, &[])?,
            },
            ast::MemberKind::Setlike { readonly, ty } => MemberKind::Setlike {
                readonly: *readonly,
                ty: self.rtype(ty, &[])?,
            },
        };
        Some(Member {
            ext_attrs,
            span: m.span,
            origin: ms.origin.clone(),
            kind,
        })
    }

    fn iter_types(&mut self, d: &ast::IterDecl) -> Option<(Option<RType>, RType)> {
        match &d.second {
            None => Some((None, self.rtype(&d.first, &[])?)),
            Some(v) => {
                let k = self.rtype(&d.first, &[])?;
                Some((Some(k), self.rtype(v, &[])?))
            }
        }
    }

    fn check_const(&mut self, ty: &RType, v: &ConstValue, name: &str, span: Span) -> Option<()> {
        if ty.nullable {
            self.err(span, format!("constant `{name}` cannot have a nullable type"));
            return None;
        }
        let bad = |this: &mut Self, why: &str| {
            this.err(span, format!("constant `{name}`: {why}"));
            None
        };
        match (&ty.kind, v) {
            (RKind::Boolean, ConstValue::Boolean(_)) => Some(()),
            (RKind::Integer { bits, signed }, ConstValue::Integer(t)) => {
                let (lo, hi) = int_range(*bits, *signed);
                match parse_integer(t) {
                    Some(n) if (lo..=hi).contains(&n) => Some(()),
                    _ => bad(self, &format!("`{t}` is out of range for `{ty}`")),
                }
            }
            (RKind::Float { .. }, ConstValue::Integer(_) | ConstValue::Decimal(_)) => Some(()),
            (
                RKind::Float { unrestricted: true, .. },
                ConstValue::Infinity | ConstValue::NegInfinity | ConstValue::NaN,
            ) => Some(()),
            (RKind::Boolean | RKind::Integer { .. } | RKind::Float { .. }, _) => {
                bad(self, &format!("value does not fit type `{ty}`"))
            }
            _ => bad(self, &format!("`{ty}` is not a constant type")),
        }
    }

    fn check_members(&mut self, owner: &str, members: &[Member]) {
        let mut plain_names: BTreeMap<(bool, &str), &str> = BTreeMap::new(); // (static, name) -> what
        let mut ops: BTreeMap<(bool, &str), Vec<(String, Span)>> = BTreeMap::new();
        let mut ctors: Vec<(String, Span)> = Vec::new();
        let mut collections = 0usize;
        let mut stringifiers = 0usize;
        for m in members {
            match &m.kind {
                MemberKind::Const { name, .. } => {
                    if plain_names.insert((false, name), "constant").is_some() {
                        self.err(m.span, format!("duplicate member `{name}` in `{owner}`"));
                    }
                }
                MemberKind::Attribute { name, is_static, .. } => {
                    if plain_names.insert((*is_static, name), "attribute").is_some() {
                        self.err(m.span, format!("duplicate member `{name}` in `{owner}`"));
                    }
                }
                MemberKind::Operation {
                    name: Some(name),
                    is_static,
                    stringifier,
                    args,
                    ..
                } => {
                    if *stringifier {
                        stringifiers += 1;
                    }
                    let sig = args
                        .iter()
                        .map(|a| format!("{}{}", a.ty, if a.variadic { "..." } else { "" }))
                        .collect::<Vec<_>>()
                        .join(",");
                    ops.entry((*is_static, name)).or_default().push((sig, m.span));
                }
                MemberKind::Constructor { args } => {
                    let sig = args.iter().map(|a| a.ty.to_string()).collect::<Vec<_>>().join(",");
                    ctors.push((sig, m.span));
                }
                MemberKind::Iterable { .. }
                | MemberKind::AsyncIterable { .. }
                | MemberKind::Maplike { .. }
                | MemberKind::Setlike { .. } => collections += 1,
                MemberKind::Stringifier => stringifiers += 1,
                MemberKind::Operation { stringifier: true, .. } => stringifiers += 1,
                MemberKind::Operation { .. } => {}
            }
            if matches!(m.kind, MemberKind::Operation { .. })
                && find_ext(&m.ext_attrs, "SameObject").is_some()
            {
                self.err(m.span, "[SameObject] applies only to readonly attributes");
            }
        }
        for ((is_static, name), sigs) in &ops {
            if plain_names.contains_key(&(*is_static, *name))
                && let Some((_, span)) = sigs.first()
            {
                self.err(*span, format!("operation `{name}` clashes with an attribute or constant of the same name in `{owner}`"));
            }
            let mut seen = BTreeSet::new();
            for (sig, span) in sigs {
                if !seen.insert(sig.as_str()) {
                    self.err(*span, format!("duplicate overload of `{name}({sig})` in `{owner}`"));
                }
            }
        }
        let mut seen = BTreeSet::new();
        for (sig, span) in &ctors {
            if !seen.insert(sig.as_str()) {
                self.err(*span, format!("duplicate constructor `({sig})` in `{owner}`"));
            }
        }
        if collections > 1 {
            self.err(
                members.first().map_or_else(Span::default, |m| m.span),
                format!("`{owner}` declares more than one of iterable, async iterable, maplike and setlike"),
            );
        }
        if stringifiers > 1 {
            self.err(
                members.first().map_or_else(Span::default, |m| m.span),
                format!("`{owner}` declares more than one stringifier"),
            );
        }
    }

    // ---- dictionaries --------------------------------------------------------

    fn build_dictionary(&mut self, name: &'a str) -> Option<Dictionary> {
        let src = self.dicts.remove(name)?;
        let mut members = Vec::new();
        let mut seen = BTreeSet::new();
        for m in &src.members {
            if !seen.insert(m.name.as_str()) {
                self.err(m.span, format!("duplicate dictionary member `{}` in `{name}`", m.name));
                continue;
            }
            if m.required && m.default.is_some() {
                self.err(m.span, "a required dictionary member cannot have a default value");
            }
            if let Some(ty) = self.rtype(&m.ty, &m.ext_attrs) {
                members.push(DictMember {
                    name: m.name.clone(),
                    ty,
                    required: m.required,
                    default: m.default.clone(),
                    ext_attrs: m.ext_attrs.clone(),
                    span: m.span,
                });
            }
        }
        Some(Dictionary {
            name: name.to_owned(),
            inherits: src.inherits.map(str::to_owned),
            ext_attrs: src.ext.to_vec(),
            members,
            span: src.span,
        })
    }

    fn enums_and_callbacks(&mut self, model: &mut Model) {
        let asts = self.asts;
        for ast in asts {
            for d in &ast.definitions {
                match &d.kind {
                    DefinitionKind::Enum(e) => {
                        let mut seen = BTreeSet::new();
                        for v in &e.values {
                            if !seen.insert(v.as_str()) {
                                self.err(d.span, format!("duplicate enum value \"{v}\" in `{}`", e.name));
                            }
                        }
                        if matches!(self.names.get(e.name.as_str()), Some((NameKind::Enum, s)) if *s == d.span) {
                            model.index.insert(e.name.clone(), (NameKind::Enum, model.enums.len()));
                            model.enums.push(Enum {
                                name: e.name.clone(),
                                values: e.values.clone(),
                                ext_attrs: d.ext_attrs.clone(),
                            });
                        }
                    }
                    DefinitionKind::Callback(c) => {
                        if matches!(self.names.get(c.name.as_str()), Some((NameKind::Callback, s)) if *s == d.span) {
                            let ret = self.rtype(&c.ret, &[]);
                            let args = self.args(&c.args);
                            if let (Some(ret), Some(args)) = (ret, args) {
                                model
                                    .index
                                    .insert(c.name.clone(), (NameKind::Callback, model.callbacks.len()));
                                model.callbacks.push(Callback {
                                    name: c.name.clone(),
                                    ret,
                                    args,
                                    ext_attrs: d.ext_attrs.clone(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Typedefs and mixins are also resolvable names for diagnostics.
        for (n, k) in &self.order {
            if matches!(k, NameKind::Typedef | NameKind::Mixin) {
                model.index.entry((*n).to_owned()).or_insert((*k, 0));
            }
        }
    }

    // ---- graph checks ------------------------------------------------------------

    fn check_inheritance(&mut self, model: &Model) {
        for c in &model.containers {
            if let Some(p) = &c.inherits {
                match model.container(p) {
                    Some(parent) if parent.kind == ContainerKind::Interface => {
                        if c.kind == ContainerKind::Interface
                            && !c.exposure.is_subset_of(&parent.exposure)
                        {
                            self.err(
                                c.span,
                                format!("`{}` is exposed in globals where its parent `{p}` is not", c.name),
                            );
                        }
                    }
                    _ => self.err(c.span, format!("`{}` inherits `{p}`, which is not an interface", c.name)),
                }
            }
            // Cycle: walk at most `containers.len()` steps.
            let mut cur = c.inherits.as_deref();
            let mut steps = 0usize;
            while let Some(n) = cur {
                if n == c.name {
                    self.err(c.span, format!("`{}` inherits from itself", c.name));
                    break;
                }
                steps += 1;
                if steps > MAX_INHERITANCE_DEPTH {
                    self.err(c.span, format!("`{}` has an inheritance chain deeper than {MAX_INHERITANCE_DEPTH} (or cyclic)", c.name));
                    break;
                }
                cur = model.container(n).and_then(|x| x.inherits.as_deref());
            }
        }
        for d in &model.dictionaries {
            if let Some(p) = &d.inherits
                && model.dictionary(p).is_none()
            {
                self.err(d.span, format!("`{}` inherits `{p}`, which is not a dictionary", d.name));
                continue;
            }
            let mut cur = d.inherits.as_deref();
            let mut steps = 0usize;
            let mut inherited: BTreeSet<&str> = BTreeSet::new();
            while let Some(n) = cur {
                if n == d.name {
                    self.err(d.span, format!("`{}` inherits from itself", d.name));
                    break;
                }
                steps += 1;
                if steps > MAX_INHERITANCE_DEPTH {
                    self.err(d.span, format!("`{}` has an inheritance chain deeper than {MAX_INHERITANCE_DEPTH} (or cyclic)", d.name));
                    break;
                }
                let Some(p) = model.dictionary(n) else { break };
                for m in &p.members {
                    inherited.insert(m.name.as_str());
                }
                cur = p.inherits.as_deref();
            }
            for m in &d.members {
                if inherited.contains(m.name.as_str()) {
                    self.err(m.span, format!("dictionary member `{}` of `{}` is already inherited", m.name, d.name));
                }
            }
        }
    }

    /// A dictionary that holds itself inline (not behind a sequence or record)
    /// has no finite size, so it cannot be a generated struct.
    fn check_dictionary_cycles(&mut self, model: &Model) {
        fn inline_deps<'m>(d: &'m Dictionary, out: &mut Vec<&'m str>) {
            if let Some(p) = &d.inherits {
                out.push(p);
            }
            for m in &d.members {
                let mut stack = vec![&m.ty];
                while let Some(t) = stack.pop() {
                    match &t.kind {
                        RKind::Dictionary(n) => out.push(n),
                        RKind::Union(ms) => stack.extend(ms.iter()),
                        _ => {}
                    }
                }
            }
        }
        // 0 = unvisited, 1 = on stack, 2 = done. Iterative DFS.
        let mut color: BTreeMap<&str, u8> = BTreeMap::new();
        for root in &model.dictionaries {
            if color.contains_key(root.name.as_str()) {
                continue;
            }
            let mut deps = Vec::new();
            inline_deps(root, &mut deps);
            let mut stack: Vec<(&Dictionary, Vec<&str>, usize)> = vec![(root, deps, 0)];
            color.insert(root.name.as_str(), 1);
            while let Some((d, deps, i)) = stack.last_mut() {
                let d: &Dictionary = d;
                match deps.get(*i).copied() {
                    None => {
                        color.insert(d.name.as_str(), 2);
                        stack.pop();
                    }
                    Some(n) => {
                        *i += 1;
                        match color.get(n).copied() {
                            Some(1) => {
                                self.err(d.span, format!("dictionary `{}` contains itself through `{n}`", d.name));
                            }
                            Some(_) => {}
                            None => {
                                if let Some(nd) = model.dictionary(n) {
                                    let mut nd_deps = Vec::new();
                                    inline_deps(nd, &mut nd_deps);
                                    color.insert(nd.name.as_str(), 1);
                                    stack.push((nd, nd_deps, 0));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn apply_type_ext(ext: &mut TypeExt, a: &ExtAttr) {
    match a.name.as_str() {
        "Clamp" => ext.clamp = true,
        "EnforceRange" => ext.enforce_range = true,
        "LegacyNullToEmptyString" => ext.null_to_empty = true,
        "AllowShared" => ext.allow_shared = true,
        "AllowResizable" => ext.allow_resizable = true,
        _ => {}
    }
}

fn merge_ext(a: &mut TypeExt, b: TypeExt) {
    a.clamp |= b.clamp;
    a.enforce_range |= b.enforce_range;
    a.null_to_empty |= b.null_to_empty;
    a.allow_shared |= b.allow_shared;
    a.allow_resizable |= b.allow_resizable;
}

fn distinguish_category(k: &RKind) -> String {
    match k {
        RKind::Boolean => "boolean".into(),
        RKind::Integer { .. } | RKind::Float { .. } => "numeric".into(),
        RKind::Bigint => "bigint".into(),
        RKind::String(_) | RKind::Enum(_) => "string".into(),
        RKind::Symbol => "symbol".into(),
        RKind::Object => "object".into(),
        RKind::Interface(n) => format!("interface:{n}"),
        RKind::Buffer(b) => format!("buffer:{}", b.name()),
        RKind::Sequence(_)
        | RKind::AsyncSequence(_)
        | RKind::FrozenArray(_)
        | RKind::ObservableArray(_) => "sequence".into(),
        RKind::Record(..) | RKind::Dictionary(_) | RKind::CallbackInterface(_) => {
            "dictionary-like".into()
        }
        RKind::CallbackFunction(_) => "callback".into(),
        RKind::Any | RKind::Undefined | RKind::Promise(_) | RKind::Union(_) => "other".into(),
    }
}

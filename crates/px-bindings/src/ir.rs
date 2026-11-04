//! The engine-neutral description of the glue every interface needs.
//!
//! `lower` turns the resolved `Model` into `Bindings`: plain data, no engine
//! types and no text. Each IDL type becomes a `Conv`, the exact conversion
//! the WebIDL ECMAScript binding section prescribes between a script value
//! and the host type (integer modes, string flavours, union step order,
//! dictionary member order). An engine emitter consumes this data; the Rust
//! host-trait emitter in `emit` uses it for signatures and defaults.

use crate::ast::{
    BufferType, ConstValue, DefaultValue, ExtAttr, ExtAttrValue, Special, StringType, find_ext,
};
use crate::resolve::{
    Arg, ContainerKind, Exposure, Member, MemberKind, Model, RKind, RType, int_range, parse_integer,
};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    pub context: String,
    pub message: String,
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.message)
    }
}

impl std::error::Error for LowerError {}

/// How an integer is brought into range when converting from a script value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntMode {
    /// ToInt32-style modular conversion (the default).
    Wrap,
    /// `[EnforceRange]`: out of range or non-finite throws a `TypeError`.
    EnforceRange,
    /// `[Clamp]`: clamp, rounding half to even.
    Clamp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Conv {
    Any,
    Undefined,
    Boolean,
    Integer {
        bits: u8,
        signed: bool,
        mode: IntMode,
    },
    BigInt,
    /// `unrestricted` floats pass NaN and infinities; restricted ones throw.
    Float {
        double: bool,
        unrestricted: bool,
    },
    String {
        kind: StringType,
        null_to_empty: bool,
    },
    Object,
    Symbol,
    Buffer {
        ty: BufferType,
        allow_shared: bool,
        allow_resizable: bool,
    },
    Interface(String),
    CallbackInterface(String),
    CallbackFunction(String),
    Dictionary(String),
    Enum(String),
    Nullable(Box<Conv>),
    Sequence(Box<Conv>),
    AsyncSequence(Box<Conv>),
    FrozenArray(Box<Conv>),
    ObservableArray(Box<Conv>),
    Record {
        key: StringType,
        value: Box<Conv>,
    },
    Promise(Box<Conv>),
    Union(UnionConv),
}

/// A union, flattened, with the order the conversion algorithm tries things.
#[derive(Debug, Clone, PartialEq)]
pub struct UnionConv {
    /// Flattened member types, nullability removed (it wraps the union).
    pub members: Vec<Conv>,
    /// Indices into `members`, in the order WebIDL section 3.2.24 tries them.
    pub steps: Vec<UnionStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnionStep {
    /// `null`/`undefined` selects the nullable member or the dictionary.
    NullishToDictionaryOrNull,
    PlatformObject(Vec<usize>),
    Buffer(Vec<usize>),
    CallbackFunction(Vec<usize>),
    /// Iterable object: sequence-likes.
    SequenceLike(Vec<usize>),
    /// Callback interface, dictionary, record: object-shaped.
    DictionaryLike(Vec<usize>),
    Boolean(usize),
    Numeric(usize),
    BigInt(usize),
    /// String types and enums; the final fallback when present.
    String(usize),
    Object(usize),
    Symbol(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrDefault {
    Bool(bool),
    Int(i128),
    Float(f64),
    Infinity,
    NegInfinity,
    NaN,
    Str(String),
    EnumValue {
        enum_name: String,
        value: String,
    },
    EmptySequence,
    EmptyDictionary,
    Null,
    Undefined,
    /// The default selects member `index` of the union's flattened members.
    UnionMember {
        index: usize,
        value: Box<IrDefault>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArgBinding {
    pub name: String,
    pub conv: Conv,
    pub optional: bool,
    pub variadic: bool,
    pub default: Option<IrDefault>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Overload {
    pub args: Vec<ArgBinding>,
    pub ret: Conv,
    /// The spec's operation "length": arguments before the first optional.
    pub required: usize,
    /// `None` when the last argument is variadic.
    pub max_args: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub same_object: bool,
    pub new_object: bool,
    pub secure_context: bool,
    pub legacy_unforgeable: bool,
    pub replaceable: bool,
    pub put_forwards: Option<String>,
    pub cross_origin_isolated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrConst {
    Bool(bool),
    Int(i128),
    Float(f64),
    Infinity,
    NegInfinity,
    NaN,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstBinding {
    pub name: String,
    pub conv: Conv,
    pub value: IrConst,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttrBinding {
    pub name: String,
    pub is_static: bool,
    pub readonly: bool,
    pub inherit: bool,
    pub stringifier: bool,
    pub conv: Conv,
    pub flags: Flags,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperationBinding {
    /// `None` for an anonymous special operation.
    pub name: Option<String>,
    pub is_static: bool,
    pub special: Option<Special>,
    pub stringifier: bool,
    pub overloads: Vec<Overload>,
    pub flags: Flags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    Iterable,
    AsyncIterable,
    Maplike,
    Setlike,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionBinding {
    pub kind: CollectionKind,
    pub readonly: bool,
    /// Present for pair iterables and maplikes.
    pub key: Option<Conv>,
    pub value: Conv,
    /// Async iterable arguments.
    pub args: Vec<ArgBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceBinding {
    pub name: String,
    pub kind: ContainerKind,
    pub parent: Option<String>,
    /// Ancestors, nearest first.
    pub ancestors: Vec<String>,
    pub exposure: Exposure,
    pub flags: Flags,
    pub global: Option<Vec<String>>,
    pub constructors: Vec<Overload>,
    pub constants: Vec<ConstBinding>,
    pub attributes: Vec<AttrBinding>,
    pub operations: Vec<OperationBinding>,
    pub collection: Option<CollectionBinding>,
    pub has_bare_stringifier: bool,
    pub ext_attrs: Vec<ExtAttr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictMemberBinding {
    pub name: String,
    pub conv: Conv,
    pub required: bool,
    pub default: Option<IrDefault>,
}

/// Which member of which dictionary, for `DictionaryBinding::conversion_order`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRef {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryBinding {
    pub name: String,
    pub parent: Option<String>,
    /// Own members, in declaration order.
    pub members: Vec<DictMemberBinding>,
    /// The order the spec converts a script object into this dictionary:
    /// least-derived ancestor first, each dictionary's members in
    /// lexicographic order (WebIDL "dictionary types" conversion).
    pub conversion_order: Vec<MemberRef>,
    pub has_required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumBinding {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallbackBinding {
    pub name: String,
    pub overload: Overload,
}

/// Everything an emitter needs. Vectors are in definition order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bindings {
    pub interfaces: Vec<InterfaceBinding>,
    pub dictionaries: Vec<DictionaryBinding>,
    pub enums: Vec<EnumBinding>,
    pub callbacks: Vec<CallbackBinding>,
}

// ---- lowering ------------------------------------------------------------

pub fn lower(model: &Model) -> Result<Bindings, LowerError> {
    let mut out = Bindings::default();
    for c in &model.containers {
        out.interfaces.push(lower_container(model, c)?);
    }
    for d in &model.dictionaries {
        out.dictionaries.push(lower_dictionary(model, d)?);
    }
    for e in &model.enums {
        out.enums.push(EnumBinding {
            name: e.name.clone(),
            values: e.values.clone(),
        });
    }
    for c in &model.callbacks {
        let ctx = format!("callback `{}`", c.name);
        out.callbacks.push(CallbackBinding {
            name: c.name.clone(),
            overload: lower_overload(model, &ctx, &c.args, &c.ret)?,
        });
    }
    Ok(out)
}

pub fn conv(t: &RType) -> Conv {
    let base = match &t.kind {
        RKind::Any => Conv::Any,
        RKind::Undefined => Conv::Undefined,
        RKind::Boolean => Conv::Boolean,
        RKind::Integer { bits, signed } => Conv::Integer {
            bits: *bits,
            signed: *signed,
            mode: if t.ext.enforce_range {
                IntMode::EnforceRange
            } else if t.ext.clamp {
                IntMode::Clamp
            } else {
                IntMode::Wrap
            },
        },
        RKind::Bigint => Conv::BigInt,
        RKind::Float {
            double,
            unrestricted,
        } => Conv::Float {
            double: *double,
            unrestricted: *unrestricted,
        },
        RKind::String(s) => Conv::String {
            kind: *s,
            null_to_empty: t.ext.null_to_empty,
        },
        RKind::Object => Conv::Object,
        RKind::Symbol => Conv::Symbol,
        RKind::Buffer(b) => Conv::Buffer {
            ty: *b,
            allow_shared: t.ext.allow_shared,
            allow_resizable: t.ext.allow_resizable,
        },
        RKind::Interface(n) => Conv::Interface(n.clone()),
        RKind::CallbackInterface(n) => Conv::CallbackInterface(n.clone()),
        RKind::CallbackFunction(n) => Conv::CallbackFunction(n.clone()),
        RKind::Dictionary(n) => Conv::Dictionary(n.clone()),
        RKind::Enum(n) => Conv::Enum(n.clone()),
        RKind::Sequence(i) => Conv::Sequence(Box::new(conv(i))),
        RKind::AsyncSequence(i) => Conv::AsyncSequence(Box::new(conv(i))),
        RKind::FrozenArray(i) => Conv::FrozenArray(Box::new(conv(i))),
        RKind::ObservableArray(i) => Conv::ObservableArray(Box::new(conv(i))),
        RKind::Record(k, v) => Conv::Record {
            key: *k,
            value: Box::new(conv(v)),
        },
        RKind::Promise(i) => Conv::Promise(Box::new(conv(i))),
        RKind::Union(_) => {
            let flat = t.flattened_members();
            let any_nullable = flat.iter().any(|m| m.nullable);
            let members: Vec<Conv> = flat
                .iter()
                .map(|m| {
                    let mut plain = (*m).clone();
                    plain.nullable = false;
                    conv(&plain)
                })
                .collect();
            let steps = union_steps(&members, t.nullable || any_nullable);
            let u = Conv::Union(UnionConv { members, steps });
            return if t.nullable || any_nullable {
                Conv::Nullable(Box::new(u))
            } else {
                u
            };
        }
    };
    if t.nullable {
        Conv::Nullable(Box::new(base))
    } else {
        base
    }
}

fn union_steps(members: &[Conv], nullable: bool) -> Vec<UnionStep> {
    let idx = |pred: &dyn Fn(&Conv) -> bool| -> Vec<usize> {
        members
            .iter()
            .enumerate()
            .filter(|(_, m)| pred(m))
            .map(|(i, _)| i)
            .collect()
    };
    let first = |pred: &dyn Fn(&Conv) -> bool| members.iter().position(pred);
    let mut steps = Vec::new();
    let has_dict = members.iter().any(|m| matches!(m, Conv::Dictionary(_)));
    if nullable || has_dict {
        steps.push(UnionStep::NullishToDictionaryOrNull);
    }
    let v = idx(&|m| matches!(m, Conv::Interface(_)));
    if !v.is_empty() {
        steps.push(UnionStep::PlatformObject(v));
    }
    let v = idx(&|m| matches!(m, Conv::Buffer { .. }));
    if !v.is_empty() {
        steps.push(UnionStep::Buffer(v));
    }
    let v = idx(&|m| matches!(m, Conv::CallbackFunction(_)));
    if !v.is_empty() {
        steps.push(UnionStep::CallbackFunction(v));
    }
    let v = idx(&|m| {
        matches!(
            m,
            Conv::Sequence(_)
                | Conv::FrozenArray(_)
                | Conv::ObservableArray(_)
                | Conv::AsyncSequence(_)
        )
    });
    if !v.is_empty() {
        steps.push(UnionStep::SequenceLike(v));
    }
    let v = idx(&|m| {
        matches!(
            m,
            Conv::CallbackInterface(_) | Conv::Dictionary(_) | Conv::Record { .. }
        )
    });
    if !v.is_empty() {
        steps.push(UnionStep::DictionaryLike(v));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::Boolean)) {
        steps.push(UnionStep::Boolean(i));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::Integer { .. } | Conv::Float { .. })) {
        steps.push(UnionStep::Numeric(i));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::BigInt)) {
        steps.push(UnionStep::BigInt(i));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::Object)) {
        steps.push(UnionStep::Object(i));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::Symbol)) {
        steps.push(UnionStep::Symbol(i));
    }
    if let Some(i) = first(&|m| matches!(m, Conv::String { .. } | Conv::Enum(_))) {
        steps.push(UnionStep::String(i));
    }
    steps
}

fn lower_default(model: &Model, ty: &RType, v: &DefaultValue) -> Result<IrDefault, String> {
    if matches!(v, DefaultValue::Null) {
        return if ty.nullable || matches!(ty.kind, RKind::Any) {
            Ok(IrDefault::Null)
        } else if let RKind::Union(_) = ty.kind {
            if ty.flattened_members().iter().any(|m| m.nullable) {
                Ok(IrDefault::Null)
            } else {
                Err(format!("`null` is not a value of `{ty}`"))
            }
        } else {
            Err(format!("`null` is not a value of `{ty}`"))
        };
    }
    let mismatch = || format!("default value does not fit type `{ty}`");
    match (&ty.kind, v) {
        (RKind::Union(_), _) => {
            for (i, m) in ty.flattened_members().into_iter().enumerate() {
                let mut plain = m.clone();
                plain.nullable = false;
                if let Ok(d) = lower_default(model, &plain, v) {
                    return Ok(IrDefault::UnionMember {
                        index: i,
                        value: Box::new(d),
                    });
                }
            }
            Err(mismatch())
        }
        (RKind::Any | RKind::Undefined, DefaultValue::Undefined) => Ok(IrDefault::Undefined),
        (RKind::Boolean, DefaultValue::Const(ConstValue::Boolean(b))) => Ok(IrDefault::Bool(*b)),
        (RKind::Integer { bits, signed }, DefaultValue::Const(ConstValue::Integer(t))) => {
            let (lo, hi) = int_range(*bits, *signed);
            match parse_integer(t) {
                Some(n) if (lo..=hi).contains(&n) => Ok(IrDefault::Int(n)),
                _ => Err(format!("`{t}` is out of range for `{ty}`")),
            }
        }
        (
            RKind::Float { .. },
            DefaultValue::Const(ConstValue::Integer(t) | ConstValue::Decimal(t)),
        ) => decimal_to_f64(t).map(IrDefault::Float).ok_or_else(mismatch),
        (
            RKind::Float {
                unrestricted: true, ..
            },
            DefaultValue::Const(c),
        ) => match c {
            ConstValue::Infinity => Ok(IrDefault::Infinity),
            ConstValue::NegInfinity => Ok(IrDefault::NegInfinity),
            ConstValue::NaN => Ok(IrDefault::NaN),
            _ => Err(mismatch()),
        },
        (RKind::String(_), DefaultValue::String(s)) => Ok(IrDefault::Str(s.clone())),
        (RKind::Enum(n), DefaultValue::String(s)) => {
            let ok = model
                .enumeration(n)
                .is_some_and(|e| e.values.iter().any(|x| x == s));
            if ok {
                Ok(IrDefault::EnumValue {
                    enum_name: n.clone(),
                    value: s.clone(),
                })
            } else {
                Err(format!("\"{s}\" is not a value of enum `{n}`"))
            }
        }
        (
            RKind::Sequence(_) | RKind::FrozenArray(_) | RKind::ObservableArray(_),
            DefaultValue::EmptyArray,
        ) => Ok(IrDefault::EmptySequence),
        (RKind::Record(..), DefaultValue::EmptyDictionary) => Ok(IrDefault::EmptyDictionary),
        (RKind::Dictionary(n), DefaultValue::EmptyDictionary) => {
            if dictionary_has_required(model, n) {
                Err(format!(
                    "`{{}}` is not a valid default: `{n}` has required members"
                ))
            } else {
                Ok(IrDefault::EmptyDictionary)
            }
        }
        _ => Err(mismatch()),
    }
}

fn dictionary_has_required(model: &Model, name: &str) -> bool {
    let mut cur = Some(name);
    let mut steps = 0usize;
    while let Some(n) = cur {
        let Some(d) = model.dictionary(n) else {
            return false;
        };
        if d.members.iter().any(|m| m.required) {
            return true;
        }
        steps += 1;
        if steps > crate::resolve::MAX_INHERITANCE_DEPTH {
            return false;
        }
        cur = d.inherits.as_deref();
    }
    false
}

fn decimal_to_f64(t: &str) -> Option<f64> {
    // Rust's parser rejects a trailing `.` and a bare leading `.`? It accepts
    // both, but not octal/hex, which the lexer only produces for integers.
    if let Some(n) = parse_integer(t) {
        return Some(n as f64);
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

fn lower_arg(model: &Model, ctx: &str, a: &Arg) -> Result<ArgBinding, LowerError> {
    let default = match &a.default {
        Some(d) => Some(
            lower_default(model, &a.ty, d).map_err(|message| LowerError {
                context: format!("{ctx}, argument `{}`", a.name),
                message,
            })?,
        ),
        None => None,
    };
    Ok(ArgBinding {
        name: a.name.clone(),
        conv: conv(&a.ty),
        optional: a.optional,
        variadic: a.variadic,
        default,
    })
}

fn lower_overload(
    model: &Model,
    ctx: &str,
    args: &[Arg],
    ret: &RType,
) -> Result<Overload, LowerError> {
    let mut out = Vec::new();
    for a in args {
        out.push(lower_arg(model, ctx, a)?);
    }
    let required = out
        .iter()
        .position(|a| a.optional || a.variadic)
        .unwrap_or(out.len());
    let variadic = out.last().is_some_and(|a| a.variadic);
    let max_args = if variadic { None } else { Some(out.len()) };
    Ok(Overload {
        args: out,
        ret: conv(ret),
        required,
        max_args,
    })
}

fn flags(attrs: &[ExtAttr], owner_secure: bool) -> Flags {
    let has = |n: &str| find_ext(attrs, n).is_some();
    let ident = |n: &str| match find_ext(attrs, n).map(|a| &a.value) {
        Some(ExtAttrValue::Ident(s)) => Some(s.clone()),
        _ => None,
    };
    Flags {
        same_object: has("SameObject"),
        new_object: has("NewObject"),
        secure_context: has("SecureContext") || owner_secure,
        legacy_unforgeable: has("LegacyUnforgeable"),
        replaceable: has("Replaceable"),
        put_forwards: ident("PutForwards"),
        cross_origin_isolated: has("CrossOriginIsolated"),
    }
}

fn const_ir(v: &ConstValue, ty: &RType) -> Result<IrConst, String> {
    Ok(match v {
        ConstValue::Boolean(b) => IrConst::Bool(*b),
        ConstValue::Integer(t) => match ty.kind {
            RKind::Float { .. } => IrConst::Float(decimal_to_f64(t).ok_or("bad number")?),
            _ => IrConst::Int(parse_integer(t).ok_or("bad integer")?),
        },
        ConstValue::Decimal(t) => IrConst::Float(decimal_to_f64(t).ok_or("bad number")?),
        ConstValue::Infinity => IrConst::Infinity,
        ConstValue::NegInfinity => IrConst::NegInfinity,
        ConstValue::NaN => IrConst::NaN,
    })
}

fn lower_container(
    model: &Model,
    c: &crate::resolve::Interface,
) -> Result<InterfaceBinding, LowerError> {
    let owner_secure = c.has_ext("SecureContext");
    let mut b = InterfaceBinding {
        name: c.name.clone(),
        kind: c.kind,
        parent: c.inherits.clone(),
        ancestors: model
            .ancestors(&c.name)
            .iter()
            .map(|a| a.name.clone())
            .collect(),
        exposure: c.exposure.clone(),
        flags: flags(&c.ext_attrs, false),
        global: match find_ext(&c.ext_attrs, "Global").map(|a| &a.value) {
            Some(ExtAttrValue::Ident(s)) => Some(vec![s.clone()]),
            Some(ExtAttrValue::IdentList(l)) => Some(l.clone()),
            Some(ExtAttrValue::NoArgs) => Some(vec![c.name.clone()]),
            _ => None,
        },
        constructors: Vec::new(),
        constants: Vec::new(),
        attributes: Vec::new(),
        operations: Vec::new(),
        collection: None,
        has_bare_stringifier: false,
        ext_attrs: c.ext_attrs.clone(),
    };
    // Overload groups: named operations sharing (static, special, name).
    let mut group_of: BTreeMap<(bool, u8, String), usize> = BTreeMap::new();
    for m in &c.members {
        let ctx = format!("{} `{}`", kind_word(c.kind), c.name);
        lower_member(model, &ctx, m, owner_secure, &mut b, &mut group_of)?;
    }
    Ok(b)
}

fn kind_word(k: ContainerKind) -> &'static str {
    match k {
        ContainerKind::Interface => "interface",
        ContainerKind::CallbackInterface => "callback interface",
        ContainerKind::Namespace => "namespace",
    }
}

fn special_code(s: Option<Special>) -> u8 {
    match s {
        None => 0,
        Some(Special::Getter) => 1,
        Some(Special::Setter) => 2,
        Some(Special::Deleter) => 3,
    }
}

fn lower_member(
    model: &Model,
    ctx: &str,
    m: &Member,
    owner_secure: bool,
    b: &mut InterfaceBinding,
    group_of: &mut BTreeMap<(bool, u8, String), usize>,
) -> Result<(), LowerError> {
    let fl = flags(&m.ext_attrs, owner_secure);
    match &m.kind {
        MemberKind::Const { name, ty, value } => {
            let value = const_ir(value, ty).map_err(|message| LowerError {
                context: format!("{ctx}, constant `{name}`"),
                message: message.to_owned(),
            })?;
            b.constants.push(ConstBinding {
                name: name.clone(),
                conv: conv(ty),
                value,
            });
        }
        MemberKind::Attribute {
            name,
            ty,
            readonly,
            inherit,
            is_static,
            stringifier,
        } => b.attributes.push(AttrBinding {
            name: name.clone(),
            is_static: *is_static,
            readonly: *readonly,
            inherit: *inherit,
            stringifier: *stringifier,
            conv: conv(ty),
            flags: fl,
        }),
        MemberKind::Operation {
            name,
            special,
            is_static,
            stringifier,
            ret,
            args,
        } => {
            let octx = format!(
                "{ctx}, operation `{}`",
                name.as_deref().unwrap_or("(anonymous)")
            );
            let ov = lower_overload(model, &octx, args, ret)?;
            let key = name
                .as_ref()
                .map(|n| (*is_static, special_code(*special), n.clone()));
            match key.as_ref().and_then(|k| group_of.get(k)).copied() {
                Some(i) => {
                    if let Some(op) = b.operations.get_mut(i) {
                        op.overloads.push(ov);
                    }
                }
                None => {
                    if let Some(k) = key {
                        group_of.insert(k, b.operations.len());
                    }
                    b.operations.push(OperationBinding {
                        name: name.clone(),
                        is_static: *is_static,
                        special: *special,
                        stringifier: *stringifier,
                        overloads: vec![ov],
                        flags: fl,
                    });
                }
            }
        }
        MemberKind::Constructor { args } => {
            let ctor_ctx = format!("{ctx}, constructor");
            b.constructors.push(lower_overload(
                model,
                &ctor_ctx,
                args,
                &RType::plain(RKind::Interface(b.name.clone())),
            )?);
        }
        MemberKind::Stringifier => b.has_bare_stringifier = true,
        MemberKind::Iterable { key, value } => {
            b.collection = Some(CollectionBinding {
                kind: CollectionKind::Iterable,
                readonly: true,
                key: key.as_ref().map(conv),
                value: conv(value),
                args: Vec::new(),
            });
        }
        MemberKind::AsyncIterable { key, value, args } => {
            let mut a = Vec::new();
            for x in args {
                a.push(lower_arg(model, ctx, x)?);
            }
            b.collection = Some(CollectionBinding {
                kind: CollectionKind::AsyncIterable,
                readonly: true,
                key: key.as_ref().map(conv),
                value: conv(value),
                args: a,
            });
        }
        MemberKind::Maplike {
            readonly,
            key,
            value,
        } => {
            b.collection = Some(CollectionBinding {
                kind: CollectionKind::Maplike,
                readonly: *readonly,
                key: Some(conv(key)),
                value: conv(value),
                args: Vec::new(),
            });
        }
        MemberKind::Setlike { readonly, ty } => {
            b.collection = Some(CollectionBinding {
                kind: CollectionKind::Setlike,
                readonly: *readonly,
                key: None,
                value: conv(ty),
                args: Vec::new(),
            });
        }
    }
    Ok(())
}

fn lower_dictionary(
    model: &Model,
    d: &crate::resolve::Dictionary,
) -> Result<DictionaryBinding, LowerError> {
    let mut members = Vec::new();
    for m in &d.members {
        let default = match &m.default {
            Some(v) => Some(
                lower_default(model, &m.ty, v).map_err(|message| LowerError {
                    context: format!("dictionary `{}`, member `{}`", d.name, m.name),
                    message,
                })?,
            ),
            None => None,
        };
        members.push(DictMemberBinding {
            name: m.name.clone(),
            conv: conv(&m.ty),
            required: m.required,
            default,
        });
    }
    // Ancestors, least derived first.
    let mut chain = vec![d];
    let mut cur = d.inherits.as_deref();
    while let Some(n) = cur {
        if chain.len() > crate::resolve::MAX_INHERITANCE_DEPTH {
            break;
        }
        let Some(p) = model.dictionary(n) else { break };
        chain.push(p);
        cur = p.inherits.as_deref();
    }
    chain.reverse();
    let mut conversion_order = Vec::new();
    let mut has_required = false;
    for dd in chain {
        let mut names: Vec<&str> = dd.members.iter().map(|m| m.name.as_str()).collect();
        names.sort_unstable();
        for n in names {
            conversion_order.push(MemberRef {
                owner: dd.name.clone(),
                name: n.to_owned(),
            });
        }
        has_required |= dd.members.iter().any(|m| m.required);
    }
    Ok(DictionaryBinding {
        name: d.name.clone(),
        parent: d.inherits.clone(),
        members,
        conversion_order,
        has_required,
    })
}

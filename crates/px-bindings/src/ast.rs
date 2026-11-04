//! The typed syntax tree. It records what was written; nothing is resolved
//! here (typedefs, partials, mixins and inheritance are `resolve`'s job).

use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Ast {
    pub file: u32,
    pub file_name: String,
    pub definitions: Vec<Definition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
    pub kind: DefinitionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefinitionKind {
    Interface(Container),
    InterfaceMixin(Container),
    Namespace(Container),
    CallbackInterface(Container),
    Dictionary(DictionaryDef),
    Enum(EnumDef),
    Typedef(TypedefDef),
    Callback(CallbackDef),
    Includes(IncludesDef),
}

/// An interface, mixin, namespace or callback interface body.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub name: String,
    pub partial: bool,
    pub inherits: Option<String>,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryDef {
    pub name: String,
    pub partial: bool,
    pub inherits: Option<String>,
    pub members: Vec<DictMember>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictMember {
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
    pub required: bool,
    pub ty: Type,
    pub name: String,
    pub default: Option<DefaultValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedefDef {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallbackDef {
    pub name: String,
    pub ret: Type,
    pub args: Vec<Argument>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncludesDef {
    pub target: String,
    pub mixin: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
    pub kind: MemberKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Getter,
    Setter,
    Deleter,
}

/// `iterable<V>` / `iterable<K, V>`, and the async form.
#[derive(Debug, Clone, PartialEq)]
pub struct IterDecl {
    pub first: Type,
    pub second: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemberKind {
    Const {
        ty: Type,
        name: String,
        value: ConstValue,
    },
    Attribute {
        readonly: bool,
        inherit: bool,
        is_static: bool,
        stringifier: bool,
        ty: Type,
        name: String,
    },
    Operation {
        special: Option<Special>,
        is_static: bool,
        stringifier: bool,
        ret: Type,
        name: Option<String>,
        args: Vec<Argument>,
    },
    Constructor {
        args: Vec<Argument>,
    },
    /// A bare `stringifier;`.
    Stringifier,
    Iterable(IterDecl),
    AsyncIterable {
        decl: IterDecl,
        args: Vec<Argument>,
    },
    Maplike {
        readonly: bool,
        key: Type,
        value: Type,
    },
    Setlike {
        readonly: bool,
        ty: Type,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
    pub optional: bool,
    pub variadic: bool,
    pub ty: Type,
    pub name: String,
    pub default: Option<DefaultValue>,
}

/// Numeric literals keep their source text; `ir::parse_integer` interprets it.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Boolean(bool),
    Integer(String),
    Decimal(String),
    Infinity,
    NegInfinity,
    NaN,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefaultValue {
    Const(ConstValue),
    String(String),
    EmptyArray,
    EmptyDictionary,
    Null,
    Undefined,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtAttr {
    pub name: String,
    pub span: Span,
    pub value: ExtAttrValue,
}

/// The syntactic forms of an extended attribute (WebIDL section 2.4), plus
/// `Other` for anything the generic grammar accepts but no form names.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtAttrValue {
    /// `[Name]`
    NoArgs,
    /// `[Name(args)]`
    ArgList(Vec<Argument>),
    /// `[Name=Ident]`
    Ident(String),
    /// `[Name=(A, B)]`
    IdentList(Vec<String>),
    /// `[Name=Ident(args)]`
    NamedArgList { name: String, args: Vec<Argument> },
    /// `[Name="text"]`
    String(String),
    /// `[Name=*]`
    Wildcard,
    Integer(String),
    Decimal(String),
    /// Source text of each token, strings re-quoted.
    Other(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringType {
    ByteString,
    DOMString,
    USVString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferType {
    ArrayBuffer,
    SharedArrayBuffer,
    DataView,
    Int8Array,
    Int16Array,
    Int32Array,
    Uint8Array,
    Uint16Array,
    Uint32Array,
    Uint8ClampedArray,
    BigInt64Array,
    BigUint64Array,
    Float16Array,
    Float32Array,
    Float64Array,
}

impl BufferType {
    pub const ALL: [(&'static str, BufferType); 15] = [
        ("ArrayBuffer", BufferType::ArrayBuffer),
        ("SharedArrayBuffer", BufferType::SharedArrayBuffer),
        ("DataView", BufferType::DataView),
        ("Int8Array", BufferType::Int8Array),
        ("Int16Array", BufferType::Int16Array),
        ("Int32Array", BufferType::Int32Array),
        ("Uint8Array", BufferType::Uint8Array),
        ("Uint16Array", BufferType::Uint16Array),
        ("Uint32Array", BufferType::Uint32Array),
        ("Uint8ClampedArray", BufferType::Uint8ClampedArray),
        ("BigInt64Array", BufferType::BigInt64Array),
        ("BigUint64Array", BufferType::BigUint64Array),
        ("Float16Array", BufferType::Float16Array),
        ("Float32Array", BufferType::Float32Array),
        ("Float64Array", BufferType::Float64Array),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, t)| *t == self)
            .map_or("", |(n, _)| n)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub nullable: bool,
    /// Extended attributes written directly on the type
    /// (`[Clamp] long`, `sequence<[EnforceRange] octet>`).
    pub ext_attrs: Vec<ExtAttr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Any,
    Undefined,
    Boolean,
    Byte,
    Octet,
    Short { unsigned: bool },
    Long { unsigned: bool },
    LongLong { unsigned: bool },
    Float { unrestricted: bool },
    Double { unrestricted: bool },
    Bigint,
    String(StringType),
    Object,
    Symbol,
    Buffer(BufferType),
    /// A reference to an interface, dictionary, enum, typedef or callback.
    Named(String),
    Sequence(Box<Type>),
    AsyncSequence(Box<Type>),
    FrozenArray(Box<Type>),
    ObservableArray(Box<Type>),
    Record(StringType, Box<Type>),
    Promise(Box<Type>),
    Union(Vec<Type>),
}

/// Find an extended attribute by name.
pub fn find_ext<'a>(attrs: &'a [ExtAttr], name: &str) -> Option<&'a ExtAttr> {
    attrs.iter().find(|a| a.name == name)
}

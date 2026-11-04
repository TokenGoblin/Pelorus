//! Grammar tests. Expected trees are derived by hand from the WebIDL grammar
//! (appendix "IDL grammar") and the examples in its prose, not from running
//! the parser.

/// Test failure with a message. `panic_any` rather than `panic!` so helpers
/// outside `#[test]` bodies satisfy the crate's panic lint without a waiver.
macro_rules! fail {
    () => {
        ::std::panic::panic_any("unexpected shape")
    };
    ($($t:tt)*) => {
        ::std::panic::panic_any(format!($($t)*))
    };
}

fn piece(rng: &mut Rng) -> &'static str {
    const PIECES: &[&str] = &[
        "interface", "partial", "mixin", "namespace", "dictionary", "enum", "typedef",
        "callback", "includes", "const", "attribute", "readonly", "static", "getter",
        "setter", "stringifier", "iterable", "maplike", "setlike", "async_iterable",
        "constructor", "optional", "required", "long", "unsigned", "DOMString",
        "sequence", "record", "Promise", "any", "or", "null", "true", "Infinity",
        "Foo", "x", "_y", "{", "}", "(", ")", "[", "]", "<", ">", ";", ",", ":", "=",
        "?", "...", "*", "\"s\"", "\"", "/*", "*/", "//", "\n", " ", "0", "1.5", "-",
        "0x", "\u{e9}", "\0",
    ];
    let i = usize::try_from(rng.next() % PIECES.len() as u64).unwrap_or(0);
    PIECES.get(i).copied().unwrap_or("")
}

use px_bindings::ast::*;
use px_bindings::parser::{MAX_NESTING, parse};

fn p(src: &str) -> Ast {
    match parse(0, "t.idl", src) {
        Ok(a) => a,
        Err(e) => fail!("{e}\n--- source:\n{src}"),
    }
}

fn perr(src: &str) -> px_bindings::span::SyntaxError {
    match parse(0, "t.idl", src) {
        Ok(a) => fail!("expected an error, parsed {a:?}"),
        Err(e) => e,
    }
}

fn iface(src: &str) -> Container {
    match p(src).definitions.into_iter().next().map(|d| d.kind) {
        Some(DefinitionKind::Interface(c)) => c,
        other => fail!("not an interface: {other:?}"),
    }
}

fn first_member(src: &str) -> MemberKind {
    iface(src).members.into_iter().next().unwrap_or_else(|| fail!("no member")).kind
}

fn ty_of_attr(src: &str) -> Type {
    match first_member(&format!("interface I {{ attribute {src} x; }};")) {
        MemberKind::Attribute { ty, .. } => ty,
        other => fail!("{other:?}"),
    }
}

#[test]
fn empty_and_comments() {
    assert!(p("").definitions.is_empty());
    assert!(p("// only a comment").definitions.is_empty());
    assert!(p("/* a */ /** b **/ // c\n").definitions.is_empty());
}

#[test]
fn simple_interface_with_inheritance() {
    let c = iface("interface Dog : Animal { };");
    assert_eq!(c.name, "Dog");
    assert_eq!(c.inherits.as_deref(), Some("Animal"));
    assert!(!c.partial);
    assert!(c.members.is_empty());
}

#[test]
fn partial_interface_and_mixin_and_includes() {
    let a = p("partial interface A { }; interface mixin M { }; partial interface mixin M { }; A includes M;");
    assert!(matches!(&a.definitions[0].kind, DefinitionKind::Interface(c) if c.partial));
    assert!(matches!(&a.definitions[1].kind, DefinitionKind::InterfaceMixin(c) if !c.partial));
    assert!(matches!(&a.definitions[2].kind, DefinitionKind::InterfaceMixin(c) if c.partial));
    assert!(matches!(&a.definitions[3].kind,
        DefinitionKind::Includes(i) if i.target == "A" && i.mixin == "M"));
}

#[test]
fn namespace_and_partial_namespace() {
    let a = p("namespace N { readonly attribute long x; undefined f(); const long C = 1; }; partial namespace N { };");
    let DefinitionKind::Namespace(c) = &a.definitions[0].kind else {
        fail!()
    };
    assert_eq!(c.members.len(), 3);
    assert!(matches!(&a.definitions[1].kind, DefinitionKind::Namespace(c) if c.partial));
}

#[test]
fn namespace_rejects_writable_attribute() {
    let e = perr("namespace N { attribute long x; };");
    assert_eq!((e.line, e.col), (1, 15));
}

#[test]
fn dictionary_members_required_default_inherit() {
    let a = p("dictionary D : B { required long a; DOMString b = \"x\"; boolean c = true; sequence<long> d = []; E e = {}; double f = -Infinity; any g = null; };");
    let DefinitionKind::Dictionary(d) = &a.definitions[0].kind else {
        fail!()
    };
    assert_eq!(d.inherits.as_deref(), Some("B"));
    assert_eq!(d.members.len(), 7);
    assert!(d.members[0].required && d.members[0].default.is_none());
    assert_eq!(d.members[1].default, Some(DefaultValue::String("x".into())));
    assert_eq!(
        d.members[2].default,
        Some(DefaultValue::Const(ConstValue::Boolean(true)))
    );
    assert_eq!(d.members[3].default, Some(DefaultValue::EmptyArray));
    assert_eq!(d.members[4].default, Some(DefaultValue::EmptyDictionary));
    assert_eq!(
        d.members[5].default,
        Some(DefaultValue::Const(ConstValue::NegInfinity))
    );
    assert_eq!(d.members[6].default, Some(DefaultValue::Null));
}

#[test]
fn enum_with_trailing_comma() {
    let a = p("enum E { \"a\", \"\", \"b-c\", };");
    let DefinitionKind::Enum(e) = &a.definitions[0].kind else {
        fail!()
    };
    assert_eq!(e.values, ["a", "", "b-c"]);
}

#[test]
fn enum_must_not_be_empty() {
    perr("enum E { };");
}

#[test]
fn typedef_and_callback_and_callback_interface() {
    let a = p("typedef (long or DOMString)? T; callback CB = undefined (long a, optional DOMString b = \"z\"); callback interface CI { const long X = 1; undefined handle(Event e); };");
    let DefinitionKind::Typedef(t) = &a.definitions[0].kind else {
        fail!()
    };
    assert!(t.ty.nullable && matches!(t.ty.kind, TypeKind::Union(ref m) if m.len() == 2));
    let DefinitionKind::Callback(c) = &a.definitions[1].kind else {
        fail!()
    };
    assert_eq!(c.args.len(), 2);
    assert!(c.args[1].optional);
    assert!(matches!(&a.definitions[2].kind, DefinitionKind::CallbackInterface(c) if c.members.len() == 2));
}

#[test]
fn const_members() {
    let c = iface("interface I { const unsigned short A = 0x1F; const long B = -7; const double C = 1.5e3; const boolean D = false; const double E = NaN; const double F = Infinity; const octet G = 017; };");
    let vals: Vec<_> = c
        .members
        .iter()
        .map(|m| match &m.kind {
            MemberKind::Const { value, .. } => value.clone(),
            _ => fail!(),
        })
        .collect();
    assert_eq!(
        vals,
        [
            ConstValue::Integer("0x1F".into()),
            ConstValue::Integer("-7".into()),
            ConstValue::Decimal("1.5e3".into()),
            ConstValue::Boolean(false),
            ConstValue::NaN,
            ConstValue::Infinity,
            ConstValue::Integer("017".into()),
        ]
    );
}

#[test]
fn attributes_all_flavours() {
    let c = iface("interface I { attribute long a; readonly attribute long b; inherit attribute long c; static attribute long d; static readonly attribute long e; stringifier attribute DOMString f; stringifier readonly attribute DOMString g; attribute long required; };");
    let flags: Vec<_> = c
        .members
        .iter()
        .map(|m| match &m.kind {
            MemberKind::Attribute {
                readonly,
                inherit,
                is_static,
                stringifier,
                name,
                ..
            } => (name.clone(), *readonly, *inherit, *is_static, *stringifier),
            _ => fail!(),
        })
        .collect();
    assert_eq!(flags[0], ("a".into(), false, false, false, false));
    assert_eq!(flags[1], ("b".into(), true, false, false, false));
    assert_eq!(flags[2], ("c".into(), false, true, false, false));
    assert_eq!(flags[3], ("d".into(), false, false, true, false));
    assert_eq!(flags[4], ("e".into(), true, false, true, false));
    assert_eq!(flags[5], ("f".into(), false, false, false, true));
    assert_eq!(flags[6], ("g".into(), true, false, false, true));
    assert_eq!(flags[7].0, "required");
}

#[test]
fn operations_special_static_stringifier() {
    let c = iface("interface I { constructor(); constructor(long a); undefined f(); static long g(); getter DOMString (unsigned long i); getter DOMString item(DOMString n); setter undefined (DOMString n, DOMString v); deleter undefined (DOMString n); stringifier; stringifier DOMString toS(); long includes(long x); };");
    assert!(matches!(&c.members[0].kind, MemberKind::Constructor { args } if args.is_empty()));
    assert!(matches!(&c.members[1].kind, MemberKind::Constructor { args } if args.len() == 1));
    assert!(matches!(&c.members[3].kind, MemberKind::Operation { is_static: true, name: Some(n), .. } if n == "g"));
    assert!(matches!(&c.members[4].kind, MemberKind::Operation { special: Some(Special::Getter), name: None, .. }));
    assert!(matches!(&c.members[5].kind, MemberKind::Operation { special: Some(Special::Getter), name: Some(n), .. } if n == "item"));
    assert!(matches!(&c.members[6].kind, MemberKind::Operation { special: Some(Special::Setter), .. }));
    assert!(matches!(&c.members[7].kind, MemberKind::Operation { special: Some(Special::Deleter), .. }));
    assert!(matches!(&c.members[8].kind, MemberKind::Stringifier));
    assert!(matches!(&c.members[9].kind, MemberKind::Operation { stringifier: true, .. }));
    assert!(matches!(&c.members[10].kind, MemberKind::Operation { name: Some(n), .. } if n == "includes"));
}

#[test]
fn iterable_maplike_setlike() {
    let c = iface("interface I { iterable<long>; iterable<DOMString, long>; async_iterable<long>; async_iterable<DOMString, long>(optional long x); maplike<DOMString, long>; readonly maplike<DOMString, long>; setlike<long>; readonly setlike<long>; };");
    assert!(matches!(&c.members[0].kind, MemberKind::Iterable(d) if d.second.is_none()));
    assert!(matches!(&c.members[1].kind, MemberKind::Iterable(d) if d.second.is_some()));
    assert!(matches!(&c.members[2].kind, MemberKind::AsyncIterable { args, .. } if args.is_empty()));
    assert!(matches!(&c.members[3].kind, MemberKind::AsyncIterable { args, .. } if args.len() == 1));
    assert!(matches!(&c.members[4].kind, MemberKind::Maplike { readonly: false, .. }));
    assert!(matches!(&c.members[5].kind, MemberKind::Maplike { readonly: true, .. }));
    assert!(matches!(&c.members[6].kind, MemberKind::Setlike { readonly: false, .. }));
    assert!(matches!(&c.members[7].kind, MemberKind::Setlike { readonly: true, .. }));
}

#[test]
fn arguments_optional_variadic_default_keyword_names() {
    let c = iface("interface I { undefined f(long a, optional long b, optional long c = 5, long... d, optional DOMString interface = \"x\", any async); };");
    let MemberKind::Operation { args, .. } = &c.members[0].kind else {
        fail!()
    };
    assert_eq!(args.len(), 6);
    assert!(args[1].optional && args[1].default.is_none());
    assert_eq!(
        args[2].default,
        Some(DefaultValue::Const(ConstValue::Integer("5".into())))
    );
    assert!(args[3].variadic);
    assert_eq!(args[4].name, "interface");
    assert_eq!(args[5].name, "async");
}

#[test]
fn keyword_not_allowed_as_attribute_name() {
    let e = perr("interface I { attribute long interface; };");
    assert_eq!((e.line, e.col), (1, 30));
}

#[test]
fn underscore_escapes_keyword() {
    let c = iface("interface _interface { attribute long _attribute; };");
    assert_eq!(c.name, "interface");
    assert!(matches!(&c.members[0].kind, MemberKind::Attribute { name, .. } if name == "attribute"));
}

#[test]
fn primitive_types() {
    let k = |s: &str| ty_of_attr(s).kind;
    assert_eq!(k("boolean"), TypeKind::Boolean);
    assert_eq!(k("byte"), TypeKind::Byte);
    assert_eq!(k("octet"), TypeKind::Octet);
    assert_eq!(k("short"), TypeKind::Short { unsigned: false });
    assert_eq!(k("unsigned short"), TypeKind::Short { unsigned: true });
    assert_eq!(k("long"), TypeKind::Long { unsigned: false });
    assert_eq!(k("unsigned long"), TypeKind::Long { unsigned: true });
    assert_eq!(k("long long"), TypeKind::LongLong { unsigned: false });
    assert_eq!(k("unsigned long long"), TypeKind::LongLong { unsigned: true });
    assert_eq!(k("float"), TypeKind::Float { unrestricted: false });
    assert_eq!(k("unrestricted float"), TypeKind::Float { unrestricted: true });
    assert_eq!(k("double"), TypeKind::Double { unrestricted: false });
    assert_eq!(k("unrestricted double"), TypeKind::Double { unrestricted: true });
    assert_eq!(k("bigint"), TypeKind::Bigint);
    assert_eq!(k("DOMString"), TypeKind::String(StringType::DOMString));
    assert_eq!(k("USVString"), TypeKind::String(StringType::USVString));
    assert_eq!(k("ByteString"), TypeKind::String(StringType::ByteString));
    assert_eq!(k("object"), TypeKind::Object);
    assert_eq!(k("symbol"), TypeKind::Symbol);
    assert_eq!(k("any"), TypeKind::Any);
    assert_eq!(k("undefined"), TypeKind::Undefined);
    assert_eq!(k("Foo"), TypeKind::Named("Foo".into()));
}

#[test]
fn every_buffer_type() {
    for (name, bt) in BufferType::ALL {
        assert_eq!(ty_of_attr(name).kind, TypeKind::Buffer(bt), "{name}");
        assert_eq!(bt.name(), name);
    }
}

#[test]
fn nullable_and_generic_types() {
    assert!(ty_of_attr("long?").nullable);
    assert!(ty_of_attr("DOMString?").nullable);
    assert!(ty_of_attr("Foo?").nullable);
    assert!(ty_of_attr("sequence<long>?").nullable);
    assert!(ty_of_attr("(long or Foo)?").nullable);
    let t = ty_of_attr("sequence<sequence<DOMString?>>");
    let TypeKind::Sequence(inner) = t.kind else {
        fail!()
    };
    assert!(matches!(inner.kind, TypeKind::Sequence(_)));
    assert!(matches!(
        ty_of_attr("FrozenArray<Foo>").kind,
        TypeKind::FrozenArray(_)
    ));
    assert!(matches!(
        ty_of_attr("ObservableArray<Foo>").kind,
        TypeKind::ObservableArray(_)
    ));
    assert!(matches!(
        ty_of_attr("async_sequence<Foo>").kind,
        TypeKind::AsyncSequence(_)
    ));
    assert!(matches!(
        ty_of_attr("record<USVString, long>").kind,
        TypeKind::Record(StringType::USVString, _)
    ));
    assert!(matches!(
        ty_of_attr("Promise<sequence<long>>").kind,
        TypeKind::Promise(_)
    ));
}

#[test]
fn any_and_promise_cannot_be_nullable() {
    perr("interface I { attribute any? x; };");
    perr("interface I { attribute Promise<long>? x; };");
}

#[test]
fn record_key_must_be_a_string_type() {
    let e = perr("interface I { attribute record<long, long> x; };");
    assert_eq!(e.expected, ["`ByteString`", "`DOMString`", "`USVString`"]);
}

#[test]
fn union_types_nested_and_flattening_input() {
    let t = ty_of_attr("(long or (Foo or DOMString?) or sequence<long>)");
    let TypeKind::Union(m) = t.kind else { fail!() };
    assert_eq!(m.len(), 3);
    assert!(matches!(&m[1].kind, TypeKind::Union(inner) if inner.len() == 2));
    perr("interface I { attribute (long) x; };");
    perr("interface I { attribute (long or) x; };");
}

#[test]
fn extended_attributes_on_types() {
    let t = ty_of_attr("[Clamp] long");
    assert_eq!(t.ext_attrs[0].name, "Clamp");
    let t = ty_of_attr("sequence<[EnforceRange] octet>");
    let TypeKind::Sequence(i) = t.kind else { fail!() };
    assert_eq!(i.ext_attrs[0].name, "EnforceRange");
    let t = ty_of_attr("([Clamp] long or DOMString)");
    let TypeKind::Union(m) = t.kind else { fail!() };
    assert_eq!(m[0].ext_attrs[0].name, "Clamp");
}

fn ext(src: &str) -> Vec<ExtAttr> {
    p(&format!("{src} interface I {{ }};"))
        .definitions
        .into_iter()
        .next()
        .map_or_else(Vec::new, |d| d.ext_attrs)
}

#[test]
fn ext_attr_forms_all_five_and_extras() {
    let e = ext("[Foo]");
    assert_eq!(e[0].value, ExtAttrValue::NoArgs);
    let e = ext("[Foo(long a, optional DOMString b)]");
    assert!(matches!(&e[0].value, ExtAttrValue::ArgList(a) if a.len() == 2));
    let e = ext("[Foo=Bar]");
    assert_eq!(e[0].value, ExtAttrValue::Ident("Bar".into()));
    let e = ext("[Exposed=(Window,Worker)]");
    assert_eq!(
        e[0].value,
        ExtAttrValue::IdentList(vec!["Window".into(), "Worker".into()])
    );
    let e = ext("[LegacyFactoryFunction=Image(DOMString src)]");
    assert!(matches!(&e[0].value,
        ExtAttrValue::NamedArgList { name, args } if name == "Image" && args.len() == 1));
    let e = ext("[Exposed=*]");
    assert_eq!(e[0].value, ExtAttrValue::Wildcard);
    let e = ext("[Foo=\"text\"]");
    assert_eq!(e[0].value, ExtAttrValue::String("text".into()));
    let e = ext("[Foo=12]");
    assert_eq!(e[0].value, ExtAttrValue::Integer("12".into()));
    let e = ext("[Foo=1.5]");
    assert_eq!(e[0].value, ExtAttrValue::Decimal("1.5".into()));
}

#[test]
fn ext_attr_multiple_and_whitespace() {
    let e = ext("[Exposed=Window, SecureContext,\n  LegacyUnforgeable, SameObject]");
    let names: Vec<_> = e.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["Exposed", "SecureContext", "LegacyUnforgeable", "SameObject"]);
}

#[test]
fn ext_attr_generic_fallback_keeps_tokens() {
    let e = ext("[Foo=(\"a\", \"b\")]");
    assert_eq!(
        e[0].value,
        ExtAttrValue::Other(
            ["Foo", "=", "(", "\"a\"", ",", "\"b\"", ")"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        )
    );
    let e = ext("[Foo=[1, 2], Bar]");
    assert!(matches!(&e[0].value, ExtAttrValue::Other(t) if t.len() == 7));
    assert_eq!(e[1].value, ExtAttrValue::NoArgs);
    let e = ext("[A B C]");
    assert!(matches!(&e[0].value, ExtAttrValue::Other(t) if t.len() == 3));
    let e = ext("[Foo(1 2)]");
    assert!(matches!(&e[0].value, ExtAttrValue::Other(_)));
}

#[test]
fn ext_attr_errors() {
    perr("[] interface I { };");
    perr("[Foo interface I { };");
    perr("[Foo(] interface I { };");
    perr("[Foo=(a] interface I { };");
}

#[test]
fn ext_attrs_on_members_arguments_and_dictionary_members() {
    let c = iface("[Exposed=Window] interface I { [SameObject] readonly attribute Foo f; undefined g([Clamp] long a); };");
    assert_eq!(c.members[0].ext_attrs[0].name, "SameObject");
    let MemberKind::Operation { args, .. } = &c.members[1].kind else {
        fail!()
    };
    assert_eq!(args[0].ext_attrs[0].name, "Clamp");
    let a = p("dictionary D { [EnforceRange] required long x; };");
    let DefinitionKind::Dictionary(d) = &a.definitions[0].kind else {
        fail!()
    };
    assert!(d.members[0].required);
    assert_eq!(d.members[0].ext_attrs[0].name, "EnforceRange");
}

#[test]
fn numbers_lexing_corner_cases() {
    let c = iface("interface I { const double A = .5; const double B = 5.; const double C = 1E+2; const long D = 0; const long E = 0X1f; const double F = -0.0; };");
    let v: Vec<_> = c
        .members
        .iter()
        .map(|m| match &m.kind {
            MemberKind::Const { value, .. } => value.clone(),
            _ => fail!(),
        })
        .collect();
    assert_eq!(v[0], ConstValue::Decimal(".5".into()));
    assert_eq!(v[1], ConstValue::Decimal("5.".into()));
    assert_eq!(v[2], ConstValue::Decimal("1E+2".into()));
    assert_eq!(v[3], ConstValue::Integer("0".into()));
    assert_eq!(v[4], ConstValue::Integer("0X1f".into()));
    assert_eq!(v[5], ConstValue::Decimal("-0.0".into()));
    // `08` is `0` followed by `8`, which is not a legal const initialiser.
    perr("interface I { const long A = 08; };");
}

#[test]
fn error_positions_and_expected() {
    let e = perr("interface I {\n  attribute long ;\n};");
    assert_eq!((e.line, e.col), (2, 18));
    assert_eq!(e.expected, ["attribute name"]);
    assert_eq!(e.found, "`;`");
    assert_eq!(
        e.to_string(),
        "t.idl:2:18: error: unexpected `;` (expected attribute name, found `;`)"
    );
    let e = perr("interface I { }");
    assert_eq!((e.line, e.col), (1, 16));
    assert_eq!(e.expected, ["`;`"]);
    assert_eq!(e.found, "end of file");
    let e = perr("\n\n   bogus");
    assert_eq!((e.line, e.col), (3, 4));
    assert!(e.expected.iter().any(|x| x == "`interface`"));
}

#[test]
fn error_column_counts_characters_not_bytes() {
    // The e-acute is two bytes; the error is the `;` after it.
    let e = perr("/* \u{e9} */ interface I { attribute long ; };");
    assert_eq!(e.col, 38);
}

#[test]
fn constructor_not_allowed_in_partial_or_mixin() {
    perr("partial interface I { constructor(); };");
    perr("interface mixin M { constructor(); };");
    perr("interface mixin M { iterable<long>; };");
    perr("callback interface C { attribute long x; };");
}

// ---- hostile input ------------------------------------------------------

#[test]
fn unterminated_comment_and_string() {
    let e = perr("interface I { /* never ends");
    assert!(e.message.contains("unterminated block comment"));
    assert_eq!((e.line, e.col), (1, 15));
    let e = perr("enum E { \"abc");
    assert!(e.message.contains("unterminated string"));
}

#[test]
fn nul_bytes_rejected() {
    let e = perr("interface I { \0 };");
    assert!(e.message.contains("NUL"));
    perr("\0");
}

#[test]
fn huge_tokens_rejected() {
    let big = "a".repeat(10_000);
    let e = perr(&format!("interface {big} {{ }};"));
    assert!(e.message.contains("too long"));
    let s = "x".repeat(100_000);
    let e = perr(&format!("enum E {{ \"{s}\" }};"));
    assert!(e.message.contains("too long"));
    let n = "1".repeat(10_000);
    perr(&format!("interface I {{ const double A = {n}; }};"));
}

#[test]
fn oversized_source_rejected() {
    let src = " ".repeat(px_bindings::lexer::MAX_SOURCE_BYTES + 1);
    assert!(perr(&src).message.contains("limit"));
}

#[test]
fn deep_nesting_is_an_error_not_an_overflow() {
    let depth = 5000;
    let open = "sequence<".repeat(depth);
    let close = ">".repeat(depth);
    let e = perr(&format!("interface I {{ attribute {open}long{close} x; }};"));
    assert!(e.message.contains("nesting"), "{e}");
    let u = "(long or ".repeat(depth) + "DOMString" + &")".repeat(depth);
    assert!(perr(&format!("typedef {u} T;")).message.contains("nesting"));
    let br = "(".repeat(depth);
    assert!(perr(&format!("[Foo={br}] interface I {{ }};")).message.contains("nesting"));
    let br = "[".repeat(depth);
    assert!(perr(&format!("[Foo {br}] interface I {{ }};")).message.contains("nesting"));
    // At the limit it still parses.
    let n = (MAX_NESTING as usize) / 2 - 2;
    let ok = format!(
        "interface I {{ attribute {}long{} x; }};",
        "sequence<".repeat(n),
        ">".repeat(n)
    );
    p(&ok);
}

#[test]
fn many_definitions_parse_linearly() {
    let src = "interface I { };\n".repeat(20_000);
    assert_eq!(p(&src).definitions.len(), 20_000);
}

/// xorshift64, fixed seed: the loop is deterministic.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn fuzz_loop_never_panics_and_terminates() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let seed = "[Exposed=Window] interface Foo : Bar { constructor(optional long a = 1); \
                attribute (long or DOMString)? x; readonly attribute sequence<Foo> y; \
                undefined f(Foo... z); }; enum E { \"a\" }; dictionary D { long q = 1; };";
    for _ in 0..4000 {
        let mut s = String::new();
        let n = (rng.next() % 40) as usize;
        for _ in 0..n {
            s.push_str(piece(&mut rng));
            if !rng.next().is_multiple_of(3) {
                s.push(' ');
            }
        }
        let _ = parse(0, "f.idl", &s);
        // Mutate the valid seed: delete, duplicate or splice a random range.
        let b = seed.as_bytes();
        let i = (rng.next() % b.len() as u64) as usize;
        let j = i + (rng.next() % 20) as usize;
        let mut m = String::new();
        m.push_str(seed.get(..i).unwrap_or(""));
        if rng.next().is_multiple_of(2) {
            m.push_str(piece(&mut rng));
        }
        m.push_str(seed.get(j.min(seed.len())..).unwrap_or(""));
        let _ = parse(0, "f.idl", &m);
    }
    // The unmutated seed is valid.
    p(seed);
}

#[test]
fn every_prefix_of_a_valid_file_terminates() {
    let src = "[Exposed=(Window,Worker), SecureContext] interface Foo : Bar { \
               constructor(); [SameObject] readonly attribute (long or Baz)? x; \
               undefined f(optional record<DOMString, any> o = {}, long... r); \
               iterable<DOMString, long>; }; partial dictionary D { required long a; };";
    for (i, _) in src.char_indices() {
        let _ = parse(0, "p.idl", src.get(..i).unwrap_or(""));
    }
}

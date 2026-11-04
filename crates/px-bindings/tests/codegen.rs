//! IR and emitter tests, and synthetic IDL whose generated Rust must compile.

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

mod common;

use common::{assert_compiles, generate};
use px_bindings::ast::StringType;
use px_bindings::emit::{snake_case, type_ident, value_ident};
use px_bindings::ir::{Conv, IntMode, IrDefault, UnionStep, lower};
use px_bindings::resolve::{BuildOptions, Model};

fn ir(idl: &str) -> px_bindings::ir::Bindings {
    let m = Model::from_sources(
        &[("t.idl".to_owned(), idl.to_owned())],
        BuildOptions::default(),
    )
    .unwrap_or_else(|e| fail!("{e:#?}"));
    lower(&m).unwrap_or_else(|e| fail!("{e}"))
}

#[test]
fn naming_helpers() {
    assert_eq!(snake_case("getElementById"), "get_element_by_id");
    assert_eq!(snake_case("innerHTML"), "inner_html");
    assert_eq!(snake_case("URL"), "url");
    assert_eq!(snake_case("XMLHttpRequest"), "xml_http_request");
    assert_eq!(snake_case("ELEMENT_NODE"), "element_node");
    assert_eq!(snake_case("a-b"), "a_b");
    assert_eq!(snake_case("x2D"), "x2_d");
    assert_eq!(value_ident("type"), "r#type");
    assert_eq!(value_ident("self"), "self_");
    assert_eq!(value_ident("foo"), "foo");
    assert_eq!(type_ident("console"), "Console");
    assert_eq!(type_ident("Self"), "Self_");
}

#[test]
fn integer_modes_and_string_flavours() {
    let b = ir("interface I { undefined f([Clamp] long a, [EnforceRange] unsigned long long b, octet c,
                   [LegacyNullToEmptyString] DOMString d, USVString e, ByteString g, unrestricted double h, float i); };");
    let args = &b.interfaces[0].operations[0].overloads[0].args;
    let int = |i: usize| match &args[i].conv {
        Conv::Integer { bits, signed, mode } => (*bits, *signed, *mode),
        o => fail!("{o:?}"),
    };
    assert_eq!(int(0), (32, true, IntMode::Clamp));
    assert_eq!(int(1), (64, false, IntMode::EnforceRange));
    assert_eq!(int(2), (8, false, IntMode::Wrap));
    assert_eq!(
        args[3].conv,
        Conv::String {
            kind: StringType::DOMString,
            null_to_empty: true
        }
    );
    assert_eq!(
        args[4].conv,
        Conv::String {
            kind: StringType::USVString,
            null_to_empty: false
        }
    );
    assert_eq!(
        args[5].conv,
        Conv::String {
            kind: StringType::ByteString,
            null_to_empty: false
        }
    );
    assert_eq!(
        args[6].conv,
        Conv::Float {
            double: true,
            unrestricted: true
        }
    );
    assert_eq!(
        args[7].conv,
        Conv::Float {
            double: false,
            unrestricted: false
        }
    );
}

#[test]
fn union_flattening_and_step_order() {
    let b = ir("interface Foo { }; dictionary D { }; callback CB = undefined ();
                interface I { attribute (long or (DOMString or boolean) or Foo or sequence<long> or record<DOMString, long> or CB or ArrayBuffer)? u; };");
    let Conv::Nullable(inner) = &b
        .interfaces
        .iter()
        .find(|i| i.name == "I")
        .expect("I")
        .attributes[0]
        .conv
    else {
        fail!()
    };
    let Conv::Union(u) = &**inner else { fail!() };
    assert_eq!(u.members.len(), 8, "nested union flattened");
    let kinds: Vec<&str> = u
        .steps
        .iter()
        .map(|s| match s {
            UnionStep::NullishToDictionaryOrNull => "nullish",
            UnionStep::PlatformObject(_) => "platform",
            UnionStep::Buffer(_) => "buffer",
            UnionStep::CallbackFunction(_) => "callback",
            UnionStep::SequenceLike(_) => "sequence",
            UnionStep::DictionaryLike(_) => "dictionary",
            UnionStep::Boolean(_) => "boolean",
            UnionStep::Numeric(_) => "numeric",
            UnionStep::BigInt(_) => "bigint",
            UnionStep::String(_) => "string",
            UnionStep::Object(_) => "object",
            UnionStep::Symbol(_) => "symbol",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "nullish",
            "platform",
            "buffer",
            "callback",
            "sequence",
            "dictionary",
            "boolean",
            "numeric",
            "string"
        ]
    );
}

#[test]
fn dictionary_conversion_order_is_ancestors_first_then_lexicographic() {
    let b = ir(
        "dictionary A { long z; long b; }; dictionary B : A { long y; long a; };
                dictionary C : B { long m; };",
    );
    let c = b.dictionaries.iter().find(|d| d.name == "C").expect("C");
    let order: Vec<String> = c
        .conversion_order
        .iter()
        .map(|r| format!("{}.{}", r.owner, r.name))
        .collect();
    assert_eq!(order, ["A.b", "A.z", "B.a", "B.y", "C.m"]);
    // Declaration order is kept separately.
    let a = b.dictionaries.iter().find(|d| d.name == "A").expect("A");
    assert_eq!(a.members[0].name, "z");
}

#[test]
fn defaults_lower_to_typed_values() {
    let b = ir("enum E { \"a\", \"b\" };
                dictionary D { long x = 3; double y = 2; unrestricted double z = -Infinity; E e = \"b\";
                  sequence<long> s = []; DOMString? n = null; (long or DOMString) u = \"hi\"; any a = undefined; };");
    let d = &b.dictionaries[0];
    let get = |n: &str| {
        d.members
            .iter()
            .find(|m| m.name == n)
            .expect("member")
            .default
            .clone()
    };
    assert_eq!(get("x"), Some(IrDefault::Int(3)));
    assert_eq!(get("y"), Some(IrDefault::Float(2.0)));
    assert_eq!(get("z"), Some(IrDefault::NegInfinity));
    assert_eq!(
        get("e"),
        Some(IrDefault::EnumValue {
            enum_name: "E".into(),
            value: "b".into()
        })
    );
    assert_eq!(get("s"), Some(IrDefault::EmptySequence));
    assert_eq!(get("n"), Some(IrDefault::Null));
    assert!(matches!(
        get("u"),
        Some(IrDefault::UnionMember { index: 1, .. })
    ));
    assert_eq!(get("a"), Some(IrDefault::Undefined));
}

#[test]
fn bad_defaults_are_lowering_errors() {
    for (idl, needle) in [
        ("dictionary D { long x = \"a\"; };", "does not fit"),
        ("dictionary D { long x = 3000000000; };", "out of range"),
        (
            "enum E { \"a\" }; dictionary D { E e = \"zz\"; };",
            "not a value of enum",
        ),
        ("dictionary D { long x = null; };", "`null` is not a value"),
        (
            "dictionary R { required long r; }; dictionary D { R x = {}; };",
            "required members",
        ),
        ("dictionary D { boolean b = 1; };", "does not fit"),
    ] {
        let m = Model::from_sources(&[("t.idl".into(), idl.into())], BuildOptions::default())
            .unwrap_or_else(|e| fail!("{e:?}"));
        let e = lower(&m).expect_err(idl);
        assert!(e.to_string().contains(needle), "{e} / {needle}");
    }
}

#[test]
fn operation_overload_groups() {
    let b = ir("interface I { undefined f(); undefined f(long a); undefined f(long a, optional long b, long... r);
                static undefined f(DOMString s); getter DOMString (unsigned long i); };");
    let i = &b.interfaces[0];
    let named: Vec<_> = i.operations.iter().filter(|o| o.name.is_some()).collect();
    assert_eq!(named.len(), 2, "static f is its own group");
    assert_eq!(named[0].overloads.len(), 3);
    let o = &named[0].overloads[2];
    assert_eq!(o.required, 1);
    assert_eq!(o.max_args, None);
    assert_eq!(named[0].overloads[1].max_args, Some(1));
}

#[test]
fn synthetic_everything_compiles() {
    let text = generate(
        r#"
[Exposed=Window] interface Base {
  constructor(); constructor(DOMString a, optional long b = 1);
  const unsigned short ONE = 1; const double PI = 3.5; const float F = 1; const unrestricted double INF = Infinity;
  const long NEG = -5; const boolean T = true;
  readonly attribute long id;
  attribute DOMString? label;
  attribute (DOMString or long)? either;
  static attribute long counter;
  static undefined reset();
  undefined over(); undefined over(long a); undefined over(DOMString a, optional sequence<long> b = []);
  Promise<undefined> later(optional (Base or DOMString) who);
  any raw(any v, object o, symbol s, bigint n, ArrayBuffer buf, Uint8Array bytes);
  record<DOMString, long> rec(record<USVString, Base> r);
  FrozenArray<DOMString> frozen();
  stringifier;
  getter Base? item(unsigned long i);
  getter DOMString (DOMString name);
  setter undefined (DOMString name, DOMString value);
  deleter undefined (DOMString name);
  undefined typeOp(DOMString self, long match, long fn);
  attribute long type;
  attribute long self;
};
[Exposed=Window] interface Derived : Base {
  iterable<DOMString, long>;
  undefined go(optional Loose o = {}, optional Mode m = "fast-mode", optional (Opts or long) u = 3);
};
[Exposed=Window] interface Maps { maplike<DOMString, long>; };
[Exposed=Window] interface RMaps { readonly maplike<DOMString, long>; };
[Exposed=Window] interface Sets { setlike<DOMString>; };
[Exposed=Window] interface RSets { readonly setlike<long>; };
[Exposed=Window] interface Values { iterable<long>; };
[Exposed=Window] interface Async { async_iterable<long>; };
[Exposed=Window] namespace console { undefined log(any... data); readonly attribute long level; const long X = 2; };
callback interface Listener { undefined handle(Base b); const long K = 1; };
callback Fn = long (long a, optional DOMString b);
interface mixin Mix { attribute long mixed; undefined mixedOp(); };
Derived includes Mix;
enum Mode { "fast-mode", "", "text/html", "3d", "Self", "a b", "a-b", "A_B" };
dictionary Opts { boolean flag = true; required long must; };
dictionary Loose : Base2 { DOMString name = "xy"; Mode mode = "3d"; sequence<Loose> kids = []; record<DOMString, long> r = {};
                           double d = 1.5; unrestricted float u = NaN; ByteString bs = "ab"; Listener? l; Fn f; (long or sequence<long>) both = []; any w = null; };
dictionary Base2 { long inherited = 7; };
dictionary AllReq : Opts { long extra; };
typedef sequence<(Base or long)> Bag;
[Exposed=Window] interface UsesBag { attribute Bag bag; undefined take(Bag b, Loose l, AllReq r, Fn f, Listener l2); };
"#,
    );
    assert_compiles(&text);
    for needle in [
        "r#type",
        "fn self_(",
        "pub const ONE: u16 = 1;",
        "pub const INF: f64 = ::std::f64::INFINITY;",
        "pub const F: f32 = 1.0;",
        "pub enum Mode {",
        "FastMode",
        "Empty,",
        "TextHtml",
        "V3d",
        "Self_",
        "pub struct Loose {",
        "pub parent: Base2,",
        "impl ::std::default::Default for Loose {",
        "pub trait DerivedHost: BaseHost {",
        "pub trait ConsoleHost {",
        "fn maplike_set(",
        "fn setlike_add(",
        "fn iterable_entry(",
        "fn over_1(",
        "fn over_2(",
        "fn construct_2(",
    ] {
        assert!(text.contains(needle), "missing {needle:?}");
    }
    // Opts has a required member, so no Default; AllReq inherits that.
    assert!(!text.contains("Default for Opts"));
    assert!(!text.contains("Default for AllReq"));
    // Callback interfaces get a marker but no host trait; read-only collections
    // have no mutators.
    assert!(!text.contains("trait ListenerHost"));
    assert!(
        !text.contains("fn rmaps_clear")
            && !text.contains("fn maplike_clear(&mut self, this: rt::Handle<RMaps>")
    );
}

#[test]
fn name_collisions_between_members_are_disambiguated() {
    let text = generate(
        "interface I { readonly attribute long rootNode; long getRootNode(); attribute long x; undefined setX(long v); undefined set_x(long v); };",
    );
    assert_compiles(&text);
    assert!(text.contains("fn root_node("));
    assert!(text.contains("fn get_root_node("));
}

#[test]
fn colliding_top_level_names_are_an_error() {
    let e = common::generate_from(&[("t.idl", "interface foo { }; interface Foo { };")], false)
        .expect_err("collision");
    assert!(e[0].contains("collides"), "{e:?}");
}

#[test]
fn deterministic_double_run_on_a_synthetic_file() {
    let idl = "[Exposed=*] interface A { attribute (long or DOMString or A)? x; undefined f(optional (A or long) y = 1); };
               dictionary D : E { (A or long) q = 1; }; dictionary E { }; enum Z { \"b\", \"a\" };";
    assert_eq!(generate(idl), generate(idl));
}

#[test]
fn generated_enum_round_trips_at_runtime() {
    // Compile the generated code together with a main that exercises it.
    let text = generate("enum Mode { \"fast-mode\", \"\", \"3d\" };");
    let program = format!(
        "{text}\nfn main() {{\n  for m in Mode::ALL {{ assert_eq!(Mode::from_idl_str(m.as_idl_str()), Some(*m)); }}\n  assert_eq!(Mode::from_idl_str(\"nope\"), None);\n  assert_eq!(Mode::FastMode.as_idl_str(), \"fast-mode\");\n  let h: rt::Handle<Mode> = rt::Handle::new(1, std::num::NonZeroU32::MIN);\n  assert_eq!(h.index(), 1);\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!("px-bindings-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let src = dir.join("m.rs");
    std::fs::write(&src, program).expect("write");
    let exe = dir.join("m");
    let out = std::process::Command::new("rustc")
        .args(["--edition", "2024", "-o"])
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("rustc");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = std::process::Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

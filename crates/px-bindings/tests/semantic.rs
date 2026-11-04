//! Semantic checking: merging, name resolution, and every diagnostic.

use px_bindings::resolve::{
    BuildOptions, ContainerKind, Exposure, MemberKind, Model, RKind, parse_integer,
};

fn build_files(files: &[&str], strict: bool) -> Result<Model, Vec<String>> {
    let sources: Vec<(String, String)> = files
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("f{i}.idl"), (*t).to_owned()))
        .collect();
    Model::from_sources(
        &sources,
        BuildOptions {
            require_exposed: strict,
        },
    )
}

fn ok(src: &str) -> Model {
    match build_files(&[src], false) {
        Ok(m) => m,
        Err(e) => panic!("{e:#?}\n{src}"),
    }
}

fn errs(src: &str) -> Vec<String> {
    match build_files(&[src], false) {
        Ok(_) => panic!("expected errors for:\n{src}"),
        Err(e) => e,
    }
}

#[track_caller]
fn has_err(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.iter().any(|m| m.contains(needle)),
        "no error containing {needle:?} in {e:#?}\nsource:\n{src}"
    );
}

fn member_names(m: &Model, iface: &str) -> Vec<String> {
    m.container(iface)
        .expect("interface")
        .members
        .iter()
        .map(|x| match &x.kind {
            MemberKind::Attribute { name, .. } | MemberKind::Const { name, .. } => name.clone(),
            MemberKind::Operation { name, .. } => name.clone().unwrap_or_default(),
            _ => "?".to_owned(),
        })
        .collect()
}

#[test]
fn partials_merge_in_file_order_even_when_they_come_first() {
    let m = build_files(
        &[
            "partial interface A { attribute long late; };",
            "interface A { attribute long first; }; partial interface A { attribute long second; };",
        ],
        false,
    )
    .expect("builds");
    assert_eq!(member_names(&m, "A"), ["first", "late", "second"]);
}

#[test]
fn mixins_merge_after_own_members_in_includes_order() {
    let m = ok("interface mixin M1 { attribute long m1; }; interface mixin M2 { attribute long m2; };
                partial interface mixin M1 { attribute long m1b; };
                interface A { attribute long own; };
                A includes M2; A includes M1; A includes M2;");
    assert_eq!(member_names(&m, "A"), ["own", "m2", "m1", "m1b"]);
    let a = m.container("A").expect("A");
    assert_eq!(a.members[1].origin.as_deref(), Some("M2"));
    assert_eq!(a.members[0].origin, None);
    assert!(m.container("M1").is_none(), "mixins are merged away");
    assert_eq!(m.mixins, ["M1", "M2"]);
}

#[test]
fn partial_exposed_and_secure_context_flow_to_members() {
    let m = ok("interface A { }; [SecureContext, Exposed=Window] partial interface A { attribute long x; };");
    let x = &m.container("A").expect("A").members[0];
    let names: Vec<_> = x.ext_attrs.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["SecureContext", "Exposed"]);
}

#[test]
fn typedefs_expand_and_merge_nullability_and_ext_attrs() {
    let m = ok("typedef [Clamp] long C; typedef DOMString? NS; typedef (C or NS) U;
                interface I { attribute C a; attribute C? b; attribute NS c; attribute U d; };");
    let i = m.container("I").expect("I");
    let ty = |n: usize| match &i.members[n].kind {
        MemberKind::Attribute { ty, .. } => ty.clone(),
        _ => panic!(),
    };
    assert!(ty(0).ext.clamp && !ty(0).nullable);
    assert!(ty(1).ext.clamp && ty(1).nullable);
    assert!(ty(2).nullable && matches!(ty(2).kind, RKind::String(_)));
    assert!(matches!(ty(3).kind, RKind::Union(ref m) if m.len() == 2));
    assert_eq!(m.typedefs, ["C", "NS", "U"]);
}

#[test]
fn typedef_of_nullable_made_nullable_again_is_an_error() {
    has_err("typedef long? T; interface I { attribute T? x; };", "already nullable");
}

#[test]
fn exposure_sets() {
    let m = ok("[Exposed=(Worker,Window,Window)] interface A { }; [Exposed=*] interface B { }; interface C { };
                [Exposed=Window] interface D { };");
    assert_eq!(
        m.container("A").expect("A").exposure,
        Exposure::Globals(vec!["Window".into(), "Worker".into()])
    );
    assert_eq!(m.container("B").expect("B").exposure, Exposure::All);
    assert_eq!(m.container("C").expect("C").exposure, Exposure::Unspecified);
    assert_eq!(m.container("D").expect("D").exposure, Exposure::Globals(vec!["Window".into()]));
}

#[test]
fn exposure_subset_rule_for_inheritance() {
    has_err(
        "[Exposed=*] interface P { }; [Exposed=*] interface C : P { };
         [Exposed=Window] interface Q { }; [Exposed=(Window,Worker)] interface D : Q { };",
        "`D` is exposed in globals where its parent `Q` is not",
    );
    ok("[Exposed=(Window,Worker)] interface P { }; [Exposed=Window] interface C : P { };");
    has_err("[Exposed=Window] interface P { }; [Exposed=*] interface C : P { };", "exposed in globals");
    has_err("[Exposed=1.5] interface P { };", "[Exposed] takes");
}

#[test]
fn require_exposed_option() {
    assert!(build_files(&["interface A { };"], false).is_ok());
    let e = build_files(&["interface A { };"], true).expect_err("strict");
    assert!(e[0].contains("missing an [Exposed]"), "{e:?}");
    assert!(build_files(&["[Exposed=*] interface A { }; callback interface B { };"], true).is_ok());
    assert!(build_files(&["namespace N { };"], true).is_err());
}

#[test]
fn definition_errors() {
    has_err("interface A { }; interface A { };", "`A` is already defined at f0.idl:1:1");
    has_err("interface A { }; enum A { \"x\" };", "already defined");
    has_err("partial interface A { };", "has nothing to extend");
    has_err("dictionary A { }; partial interface A { };", "does not match its kind");
    has_err("partial dictionary D { };", "no dictionary to extend");
    has_err("interface A { }; A includes B;", "not an interface mixin");
    has_err("interface mixin M { }; M includes M;", "not an interface");
    has_err("interface A { attribute Nope x; };", "unknown type `Nope`");
    has_err("interface mixin M { }; interface A { attribute M x; };", "cannot be used as a type");
    has_err("namespace N { }; interface A { attribute N x; };", "cannot be used as a type");
}

#[test]
fn error_positions_point_at_the_construct() {
    let e = errs("interface A {\n  attribute Nope x;\n};");
    assert!(e[0].starts_with("f0.idl:2:13: error: unknown type `Nope`"), "{}", e[0]);
}

#[test]
fn inheritance_checks() {
    has_err("dictionary D { }; interface A : D { };", "not an interface");
    has_err("interface A : Missing { };", "not an interface");
    has_err("callback interface C { }; interface A : C { };", "not an interface");
    has_err("interface A : B { }; interface B : A { };", "inherits from itself");
    has_err("interface A : A { };", "inherits from itself");
    has_err("interface A { }; dictionary D : A { };", "not a dictionary");
    has_err("dictionary D : E { }; dictionary E : D { };", "inherits from itself");
    has_err(
        "dictionary B { long x; }; dictionary D : B { long x; };",
        "already inherited",
    );
    let m = ok("interface A { }; interface B : A { }; interface C : B { };");
    let anc: Vec<_> = m.ancestors("C").iter().map(|i| i.name.clone()).collect();
    assert_eq!(anc, ["B", "A"]);
}

#[test]
fn long_inheritance_chains_are_bounded() {
    let chain = |n: usize| {
        let mut s = String::from("interface I0 { };\n");
        for i in 1..n {
            s.push_str(&format!("interface I{i} : I{} {{ }};\n", i - 1));
        }
        s
    };
    let m = ok(&chain(200));
    assert_eq!(m.ancestors("I199").len(), 199);
    has_err(&chain(20_000), "inheritance chain deeper than 256");
    // A long cycle terminates too.
    let mut c = String::new();
    for i in 0..5000 {
        c.push_str(&format!("interface I{i} : I{} {{ }};\n", (i + 1) % 5000));
    }
    has_err(&c, "(or cyclic)");
    has_err("interface A : B { }; interface B : C { }; interface C : B { };", "inherits from itself");
}

#[test]
fn duplicate_members() {
    has_err("interface A { attribute long x; attribute long x; };", "duplicate member `x`");
    has_err("interface A { const long x = 1; attribute long x; };", "duplicate member `x`");
    has_err("interface A { attribute long x; undefined x(); };", "clashes with an attribute");
    has_err("interface A { undefined f(long a); undefined f(long b); };", "duplicate overload");
    has_err("interface A { constructor(long a); constructor(long b); };", "duplicate constructor");
    has_err("dictionary D { long a; long a; };", "duplicate dictionary member `a`");
    has_err("interface A { undefined f(long a, long a); };", "duplicate argument name");
    has_err("enum E { \"a\", \"b\", \"a\" };", "duplicate enum value");
    // Legal: overloads by arity, static and instance sharing a name.
    ok("interface A { undefined f(); undefined f(long a); static undefined g(); attribute long g; };");
    // Duplicates across a mixin include are found too.
    has_err(
        "interface mixin M { attribute long x; }; interface A { attribute long x; }; A includes M;",
        "duplicate member `x`",
    );
}

#[test]
fn collection_and_stringifier_limits() {
    has_err("interface A { iterable<long>; maplike<long, long>; };", "more than one of iterable");
    has_err("interface A { setlike<long>; setlike<long>; };", "more than one of iterable");
    has_err("interface A { stringifier; stringifier DOMString f(); };", "more than one stringifier");
    ok("interface A { iterable<long>; stringifier; };");
}

#[test]
fn argument_rules() {
    has_err("interface A { undefined f(long... a, long b); };", "variadic argument must be the last");
    ok("interface A { undefined f(long a, long... b); };");
}

#[test]
fn typedef_cycles_and_depth_are_bounded() {
    has_err("typedef A B; typedef B A; interface I { attribute A x; };", "cycle");
    has_err("typedef A A; interface I { attribute A x; };", "cycle");
    // A chain of 200 is deeper than the limit.
    let mut s = String::from("typedef long T0;\n");
    for i in 1..200 {
        s.push_str(&format!("typedef T{} T{i};\n", i - 1));
    }
    s.push_str("interface I { attribute T199 x; };");
    has_err(&s, "chain deeper than");
    // A chain of 20 is fine.
    let mut s = String::from("typedef long T0;\n");
    for i in 1..20 {
        s.push_str(&format!("typedef T{} T{i};\n", i - 1));
    }
    s.push_str("interface I { attribute T19 x; };");
    ok(&s);
    // An unused cycle is not reported (only uses are resolved).
    ok("typedef A B; typedef B A;");
}

#[test]
fn exponential_typedef_expansion_is_cut_off() {
    let mut s = String::from("typedef long T0;\n");
    for i in 1..40 {
        s.push_str(&format!("typedef (sequence<T{p}> or DOMString) T{i};\n", p = i - 1));
    }
    s.push_str("interface I { attribute T39 x; };");
    // Linear here, so it is accepted or cut by the node budget, never a hang.
    let _ = build_files(&[&s], false);
    // Genuinely exponential: each level uses the previous twice.
    let mut s = String::from("typedef long T0;\n");
    for i in 1..30 {
        s.push_str(&format!(
            "typedef (sequence<T{p}> or record<DOMString, T{p}>) T{i};\n",
            p = i - 1
        ));
    }
    s.push_str("interface I { attribute T29 x; };");
    has_err(&s, "expands to more than");
}

#[test]
fn type_extended_attribute_validation() {
    has_err("interface A { attribute [Clamp] DOMString x; };", "apply only to integer");
    has_err("interface A { attribute [Clamp, EnforceRange] long x; };", "cannot be combined");
    has_err("interface A { attribute [LegacyNullToEmptyString] long x; };", "applies only to DOMString");
    has_err("interface A { attribute [AllowShared] long x; };", "buffer types");
    ok("interface A { attribute [Clamp] long x; attribute [AllowShared] ArrayBuffer b; undefined f([EnforceRange] unsigned long long a); };");
    // Member-level attributes apply to the type, as the spec says.
    let m = ok("interface A { [Clamp] attribute octet x; };");
    let MemberKind::Attribute { ty, .. } = &m.container("A").expect("A").members[0].kind else {
        panic!()
    };
    assert!(ty.ext.clamp);
}

#[test]
fn union_rules() {
    has_err("interface A { attribute (long or double) x; };", "not distinguishable");
    has_err("interface A { attribute (DOMString or USVString) x; };", "not distinguishable");
    has_err("interface A { attribute (sequence<long> or sequence<DOMString>) x; };", "not distinguishable");
    has_err("interface A { attribute (long? or DOMString?) x; };", "at most one nullable");
    has_err("interface A { attribute (long? or DOMString)? x; };", "at most one nullable");
    has_err("dictionary D { }; interface A { attribute (D or long)? x; };", "dictionary and a nullable");
    has_err("interface A { attribute (any or long) x; };", "unexpected `any`");
    has_err("interface A { attribute (Promise<long> or DOMString) x; };", "unexpected `Promise`");
    has_err("interface A { attribute (A or A) x; };", "not distinguishable");
    // Nested unions are flattened before the check.
    has_err("interface A { attribute (long or (DOMString or double)) x; };", "not distinguishable");
    ok("interface A { attribute (long or DOMString or boolean or A or sequence<long> or object) x; };");
    ok("enum E { \"a\" }; interface A { attribute (A or E) x; };");
    has_err("enum E { \"a\" }; interface A { attribute (DOMString or E) x; };", "not distinguishable");
    has_err("interface A { attribute any? x; };", "unexpected `?`");
}

#[test]
fn const_checks() {
    has_err("interface A { const octet X = 256; };", "out of range");
    has_err("interface A { const byte X = -129; };", "out of range");
    has_err("interface A { const long X = 1.5; };", "does not fit");
    has_err("interface A { const double X = true; };", "does not fit");
    has_err("interface A { const double X = NaN; };", "does not fit");
    has_err("interface A { const DOMString X = 1; };", "expected identifier");
    has_err("interface A { const boolean X = 1; };", "does not fit");
    ok("interface A { const unrestricted double X = NaN; const double Y = 1; const unsigned long long Z = 18446744073709551615; const long N = -2147483648; const short H = 0x7fff; const octet O = 0377; };");
}

#[test]
fn dictionary_containment_cycles() {
    has_err("dictionary D { D x; };", "contains itself");
    has_err("dictionary D { D? x; };", "contains itself");
    has_err("dictionary D { (D or long) x; };", "contains itself");
    has_err("dictionary A { B b; }; dictionary B { A a; };", "contains itself");
    has_err("dictionary A : B { }; dictionary B { A a; };", "contains itself");
    // Behind a sequence or record the size is finite.
    ok("dictionary D { sequence<D> x; record<DOMString, D> y; };");
}

#[test]
fn sameobject_and_newobject_placement() {
    has_err("interface A { [SameObject] attribute long x; };", "readonly attributes");
    has_err("interface A { [SameObject] undefined f(); };", "readonly attributes");
    ok("interface A { [SameObject] readonly attribute long x; [NewObject] A f(); };");
}

#[test]
fn many_errors_are_capped() {
    let mut s = String::new();
    for i in 0..1000 {
        s.push_str(&format!("interface I{i} {{ attribute Nope x; }};\n"));
    }
    let e = errs(&s);
    assert_eq!(e.len(), 100);
}

#[test]
fn syntax_errors_surface_from_from_sources() {
    let e = build_files(&["interface {"], false).expect_err("syntax");
    assert!(e[0].starts_with("f0.idl:1:11: error:"), "{}", e[0]);
}

#[test]
fn container_kinds() {
    let m = ok("interface A { }; callback interface C { undefined f(); }; namespace N { undefined g(); };");
    assert_eq!(m.container("A").expect("A").kind, ContainerKind::Interface);
    assert_eq!(m.container("C").expect("C").kind, ContainerKind::CallbackInterface);
    assert_eq!(m.container("N").expect("N").kind, ContainerKind::Namespace);
}

#[test]
fn integer_literal_parsing() {
    assert_eq!(parse_integer("0"), Some(0));
    assert_eq!(parse_integer("-0"), Some(0));
    assert_eq!(parse_integer("017"), Some(15));
    assert_eq!(parse_integer("0x1F"), Some(31));
    assert_eq!(parse_integer("-0x10"), Some(-16));
    assert_eq!(parse_integer("123456789012345678901234567890"), Some(123456789012345678901234567890));
    assert_eq!(parse_integer("0x"), None);
    assert_eq!(parse_integer("1.5"), None);
    assert_eq!(parse_integer(&"9".repeat(60)), None);
}

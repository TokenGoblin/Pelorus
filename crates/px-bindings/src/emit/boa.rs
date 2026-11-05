//! The Boa glue emitter: `ir::Bindings` to `boa.rs`, the code that exposes the
//! host traits of `host.rs` to script.
//!
//! This crate does not depend on `boa_engine` (ADR 040: only `px-script`
//! links it). The emitter writes text; the consumer compiles it against
//! `boa_engine =0.22.0` next to `host.rs`, both as modules of one parent
//! (`boa.rs` starts with `use super::host::*;`).
//!
//! # Shape of the emitted glue
//!
//! * A platform object is a Boa object whose native data is the tuple
//!   `Wrapped = (index, generation, interface index)`. A tuple, because
//!   `Trace` cannot be derived without `unsafe` and a tuple of integers
//!   already implements both `Trace` and `JsData`. No pointer is stored: the
//!   host resolves the handle on every call and a stale one is an `Err`.
//! * One `fn` per operation, attribute accessor and constructor, generic over
//!   `H: ScriptHost`, installed by `install::<H>`. The host lives in
//!   `HostSlot<H>` (`Rc<RefCell<H>>`) in the context's host-defined data; a
//!   re-entrant call finds it borrowed and throws instead of aliasing.
//! * Values are converted by the WebIDL algorithms (`conv_int` is section
//!   3.2.4's ConvertToInt). A member whose signature needs a conversion this
//!   emitter does not have is not installed, and is listed in
//!   `UNSUPPORTED`, so the gap is data, not a silent hole.

use super::{Claims, EmitError, Emitter, Gen, OutputFile, snake_case, type_ident, value_ident};
use crate::ast::{Special, StringType};
use crate::ir::{
    ArgBinding, AttrBinding, Bindings, Conv, IntMode, InterfaceBinding, IrConst, Overload,
};
use crate::resolve::ContainerKind;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Emits `boa.rs`.
pub struct BoaGlueEmitter;

impl Emitter for BoaGlueEmitter {
    fn name(&self) -> &'static str {
        "boa-glue"
    }

    fn emit(&self, b: &Bindings) -> Result<Vec<OutputFile>, EmitError> {
        let text = BoaGen::new(b).run();
        Ok(vec![OutputFile {
            path: "boa.rs".to_owned(),
            contents: text,
        }])
    }
}

/// Host-trait method names of one interface, in the order `RustHostEmitter`
/// claims them. `tests/boa_glue.rs` checks every name the glue calls exists in
/// `host.rs`, so a drift between the two fails a test.
#[derive(Default)]
struct HostNames {
    ctors: Vec<String>,
    /// (getter, setter) per attribute, parallel to `attributes`.
    attrs: Vec<(String, Option<String>)>,
    stringifier: Option<String>,
    /// Per operation, one name per overload.
    ops: Vec<Vec<String>>,
}

fn host_names(i: &InterfaceBinding) -> HostNames {
    let mut claims = Claims::new();
    let mut n = HostNames::default();
    for _ in &i.constructors {
        n.ctors.push(claims.claim("construct".to_owned()));
    }
    for a in &i.attributes {
        let g = value_ident(&claims.claim(snake_case(&a.name)));
        let s = (!a.readonly && a.flags.put_forwards.is_none())
            .then(|| value_ident(&claims.claim(format!("set_{}", snake_case(&a.name)))));
        n.attrs.push((g, s));
    }
    if i.has_bare_stringifier {
        n.stringifier = Some(value_ident(&claims.claim("stringifier".to_owned())));
    }
    for op in &i.operations {
        let base = match (&op.name, op.special) {
            (Some(x), _) => snake_case(x),
            (None, Some(Special::Getter)) => "anonymous_getter".into(),
            (None, Some(Special::Setter)) => "anonymous_setter".into(),
            (None, Some(Special::Deleter)) => "anonymous_deleter".into(),
            (None, None) => "anonymous".into(),
        };
        let mut v = Vec::new();
        for k in 0..op.overloads.len() {
            let ident = if k == 0 {
                base.clone()
            } else {
                format!("{base}_{k}")
            };
            v.push(value_ident(&claims.claim(ident)));
        }
        n.ops.push(v);
    }
    n
}

struct BoaGen<'a> {
    b: &'a Bindings,
    out: String,
    /// "Interface.member: reason".
    unsupported: Vec<String>,
    /// Statements for `install`.
    install: String,
}

/// The generated expression, or why there is none.
type Expr = Result<String, String>;

impl<'a> BoaGen<'a> {
    fn new(b: &'a Bindings) -> Self {
        BoaGen {
            b,
            out: String::new(),
            unsupported: Vec::new(),
            install: String::new(),
        }
    }

    fn w(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn iface_known(&self, n: &str) -> bool {
        self.b
            .interfaces
            .iter()
            .any(|i| i.name == n && i.kind != ContainerKind::CallbackInterface)
    }

    /// Expression of the Rust host type for the script value `v` (`&JsValue`).
    fn convert_in(&self, c: &Conv, v: &str) -> Expr {
        Ok(match c {
            Conv::Boolean => format!("{v}.to_boolean()"),
            Conv::Integer { bits, signed, mode } => {
                let m = match mode {
                    IntMode::Wrap => "Wrap",
                    IntMode::EnforceRange => "EnforceRange",
                    IntMode::Clamp => "Clamp",
                };
                format!(
                    "conv_int({v}, ctx, {bits}, {signed}, ConvMode::{m})? as {}",
                    super::int_type(*bits, *signed)
                )
            }
            Conv::Float {
                double,
                unrestricted,
            } => {
                let cast = if *double { "" } else { " as f32" };
                format!("conv_float({v}, ctx, {double}, {unrestricted})?{cast}")
            }
            Conv::String {
                kind,
                null_to_empty,
            } => match kind {
                StringType::ByteString => format!("conv_byte_string({v}, ctx)?"),
                _ => format!("conv_string({v}, ctx, {null_to_empty})?"),
            },
            Conv::Interface(n) if self.iface_known(n) => {
                let t = type_ident(n);
                format!("this_handle::<{t}>({v}, {t}::NAME)?")
            }
            Conv::Enum(n) => format!(
                "{t}::from_idl_str(&conv_string({v}, ctx, false)?).ok_or_else(|| type_error(\"not a valid {n} value\"))?",
                t = type_ident(n)
            ),
            Conv::Nullable(inner) => {
                let e = self.convert_in(inner, v)?;
                format!("if {v}.is_null_or_undefined() {{ None }} else {{ Some({e}) }}")
            }
            other => {
                return Err(format!(
                    "no script-to-host conversion for {}",
                    kind_of(other)
                ));
            }
        })
    }

    /// Expression producing a `JsValue` from the host value `r`.
    fn convert_out(&self, c: &Conv, r: &str, depth: usize) -> Expr {
        Ok(match c {
            Conv::Undefined => format!("{{ let () = {r}; JsValue::undefined() }}"),
            Conv::Boolean => format!("JsValue::from({r})"),
            Conv::Integer { .. } | Conv::Float { .. } => format!("JsValue::from({r} as f64)"),
            Conv::String { kind, .. } => match kind {
                StringType::ByteString => format!(
                    "JsValue::from(JsString::from({r}.iter().map(|&b| char::from(b)).collect::<::std::string::String>()))"
                ),
                _ => format!("JsValue::from(JsString::from({r}.as_str()))"),
            },
            Conv::Enum(_) => format!("JsValue::from(JsString::from({r}.as_idl_str()))"),
            Conv::Interface(n) if self.iface_known(n) => {
                format!("wrap::<H, _>(ctx, {}::NAME, {r})?", type_ident(n))
            }
            Conv::Nullable(inner) => {
                let x = format!("x{depth}");
                let e = self.convert_out(inner, &x, depth + 1)?;
                format!("match {r} {{ None => JsValue::null(), Some({x}) => {e} }}")
            }
            Conv::Sequence(inner) => {
                let x = format!("x{depth}");
                let e = self.convert_out(inner, &x, depth + 1)?;
                format!(
                    "{{ let mut items = ::std::vec::Vec::new(); for {x} in {r} {{ items.push({e}); }} JsValue::from(JsArray::from_iter(items, ctx)) }}"
                )
            }
            other => {
                return Err(format!(
                    "no host-to-script conversion for {}",
                    kind_of(other)
                ));
            }
        })
    }

    /// The statements binding `a0..` from `args`, and the call argument list.
    fn arg_prep(&self, o: &Overload) -> Result<(String, String), String> {
        let mut pre = String::new();
        let mut names = Vec::new();
        for (k, a) in o.args.iter().enumerate() {
            let name = format!("a{k}");
            self.arg(&mut pre, &name, k, a)?;
            names.push(name);
        }
        Ok((pre, names.join(", ")))
    }

    fn arg(&self, pre: &mut String, name: &str, k: usize, a: &ArgBinding) -> Result<(), String> {
        let wrap_err = |e: String| format!("argument `{}`: {e}", a.name);
        let e = self.convert_in(&a.conv, "v").map_err(wrap_err)?;
        if a.variadic {
            let _ = writeln!(
                pre,
                "    let {name} = args.iter().skip({k}).map(|v| -> JsResult<_> {{ Ok({e}) }}).collect::<JsResult<::std::vec::Vec<_>>>()?;"
            );
        } else if a.optional {
            let (some, none) = match &a.default {
                None => (format!("Some({e})"), "None".to_owned()),
                Some(d) => {
                    let mut g = Gen {
                        b: self.b,
                        unions: BTreeMap::new(),
                        out: String::new(),
                    };
                    let d = g
                        .default_expr(&a.conv, d)
                        .map_err(|x| format!("argument `{}`: {}", a.name, x.message))?;
                    (e, d)
                }
            };
            let _ = writeln!(
                pre,
                "    let {name} = match args.get({k}) {{ Some(v) if !v.is_undefined() => {some}, _ => {none} }};"
            );
        } else {
            let _ = writeln!(
                pre,
                "    let {name} = {{ let v = args.get({k}).cloned().unwrap_or_default(); let v = &v; {e} }};"
            );
        }
        Ok(())
    }

    /// One native function for one overload. `call` is the host call, with
    /// `{args}` standing for the converted arguments (each preceded by a comma).
    fn overload_fn(
        &self,
        fname: &str,
        o: &Overload,
        this_ty: Option<&str>,
        call: &str,
        guard: bool,
    ) -> Result<String, String> {
        let (pre, args) = self.arg_prep(o)?;
        let r = self.convert_out(&o.ret, "r", 0)?;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "fn {fname}<H: ScriptHost>(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {{"
        );
        let guard = guard && o.required > 0;
        if o.args.is_empty() && !guard {
            s.push_str("    let _ = args;\n");
        }
        if guard {
            let _ = writeln!(
                s,
                "    if args.len() < {} {{ return Err(type_error(\"not enough arguments\")); }}",
                o.required
            );
        }
        match this_ty {
            Some(t) => {
                let _ = writeln!(s, "    let this = this_handle::<{t}>(this, {t}::NAME)?;");
            }
            None => {
                let _ = writeln!(s, "    let _ = this;");
            }
        }
        s.push_str(&pre);
        let list = if args.is_empty() {
            String::new()
        } else {
            format!(", {args}")
        };
        let call = call.replace("{args}", &list);
        let _ = writeln!(s, "    let r = with_host::<H, _>(ctx, |h| {call})?;");
        let _ = writeln!(s, "    Ok({r})");
        s.push_str("}\n");
        Ok(s)
    }

    /// Native function(s) for an overload set; returns the text and the
    /// function's `length`. Overloads are told apart by argument count only.
    fn overload_set(
        &self,
        fname: &str,
        overloads: &[Overload],
        this_ty: Option<&str>,
        calls: &[String],
    ) -> Result<(String, usize), String> {
        let len = overloads.iter().map(|o| o.required).max().unwrap_or(0);
        if let ([o], [call]) = (overloads, calls) {
            return Ok((self.overload_fn(fname, o, this_ty, call, true)?, len));
        }
        let ranges: Vec<(usize, usize)> = overloads
            .iter()
            .map(|o| (o.required, o.max_args.unwrap_or(usize::MAX)))
            .collect();
        for (x, a) in ranges.iter().enumerate() {
            for b in ranges.iter().skip(x + 1) {
                if a.0 <= b.1 && b.0 <= a.1 {
                    return Err("overloads share an argument count; overload resolution by type is not implemented".to_owned());
                }
            }
        }
        let mut text = String::new();
        let mut dispatch = String::new();
        for (k, ((o, call), (lo, hi))) in overloads.iter().zip(calls).zip(&ranges).enumerate() {
            let inner = format!("{fname}_o{k}");
            text.push_str(&self.overload_fn(&inner, o, this_ty, call, false)?);
            let hi_check = if *hi == usize::MAX {
                String::new()
            } else {
                format!(" && args.len() <= {hi}")
            };
            let _ = writeln!(
                dispatch,
                "    if args.len() >= {lo}{hi_check} {{ return {inner}::<H>(this, args, ctx); }}"
            );
        }
        let _ = writeln!(
            text,
            "fn {fname}<H: ScriptHost>(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {{\n{dispatch}    Err(type_error(\"no overload takes this many arguments\"))\n}}"
        );
        Ok((text, len))
    }

    fn skip(&mut self, iface: &str, member: &str, why: &str) {
        self.unsupported.push(format!("{iface}.{member}: {why}"));
    }

    fn attr_fns(
        &mut self,
        i: &InterfaceBinding,
        a: &AttrBinding,
        names: &(String, Option<String>),
        prefix: &str,
        target: &str,
    ) -> String {
        let ty = type_ident(&i.name);
        let this = (!a.is_static).then_some(ty.as_str());
        let this_arg = if a.is_static { "" } else { ", this" };
        let id = ident_part(&a.name);
        let mut text = String::new();
        let getter = format!("{prefix}_get_{id}");
        let gcall = format!("<H as {ty}Host>::{}(h{this_arg}{{args}})", names.0);
        let go = Overload {
            args: Vec::new(),
            ret: a.conv.clone(),
            required: 0,
            max_args: Some(0),
        };
        let mut get_name = None;
        match self.overload_fn(&getter, &go, this, &gcall, false) {
            Ok(t) => {
                text.push_str(&t);
                get_name = Some(getter);
            }
            Err(e) => self.skip(&i.name, &a.name, &format!("getter: {e}")),
        }
        let mut set_name = None;
        if let Some(sn) = &names.1 {
            let setter = format!("{prefix}_set_{id}");
            let so = Overload {
                args: vec![ArgBinding {
                    name: "value".to_owned(),
                    conv: a.conv.clone(),
                    optional: false,
                    variadic: false,
                    default: None,
                }],
                ret: Conv::Undefined,
                required: 1,
                max_args: Some(1),
            };
            let scall = format!("<H as {ty}Host>::{sn}(h{this_arg}{{args}})");
            match self.overload_fn(&setter, &so, this, &scall, true) {
                Ok(t) => {
                    text.push_str(&t);
                    set_name = Some(setter);
                }
                Err(e) => self.skip(&i.name, &a.name, &format!("setter: {e}")),
            }
        }
        if get_name.is_some() || set_name.is_some() {
            let g = get_name.map_or("None".to_owned(), |n| format!("Some({n}::<H>)"));
            let s = set_name.map_or("None".to_owned(), |n| format!("Some({n}::<H>)"));
            let _ = writeln!(
                self.install,
                "    def_accessor(ctx, &{target}, {:?}, {g}, {s})?;",
                a.name
            );
        }
        text
    }

    fn interface(&mut self, i: &InterfaceBinding, fns: &mut String) {
        let ty = type_ident(&i.name);
        let var = format!("o_{}", ident_part(&i.name));
        let prefix = ident_part(&i.name);
        let ns = i.kind == ContainerKind::Namespace;
        let names = host_names(i);
        let _ = writeln!(self.install, "    // {}", i.name);
        // The object that holds statics: the constructor, or the namespace.
        let holder = if ns {
            var.clone()
        } else {
            format!("{var}_ctor")
        };
        if ns {
            let _ = writeln!(
                self.install,
                "    let {var} = ObjectInitializer::new(ctx).build();"
            );
        } else {
            let ctor_native = if i.constructors.is_empty() {
                "illegal_constructor".to_owned()
            } else {
                let fname = format!("{prefix}_construct");
                let calls: Vec<String> = names
                    .ctors
                    .iter()
                    .map(|n| format!("<H as {ty}Host>::{n}(h{{args}})"))
                    .collect();
                match self.overload_set(&fname, &i.constructors, None, &calls) {
                    Ok((t, _)) => {
                        fns.push_str(&t);
                        format!("{fname}::<H>")
                    }
                    Err(e) => {
                        self.skip(&i.name, "constructor", &e);
                        "illegal_constructor".to_owned()
                    }
                }
            };
            let parent = i
                .parent
                .as_ref()
                .map_or("None".to_owned(), |p| format!("protos.get({p:?}).cloned()"));
            let parent_ctor = i
                .parent
                .as_ref()
                .map_or("None".to_owned(), |p| format!("ctors.get({p:?}).cloned()"));
            let _ = writeln!(
                self.install,
                "    let {var} = make_proto(ctx, {parent});\n    let {holder} = make_constructor(ctx, {:?}, {ctor_native}, &{var}, {parent_ctor})?;\n    protos.insert({ty}::NAME, {var}.clone());\n    ctors.insert({ty}::NAME, {holder}.clone());",
                i.name
            );
        }
        for c in &i.constants {
            let v = match &c.value {
                IrConst::Bool(b) => format!("JsValue::from({b})"),
                IrConst::Int(n) => format!("JsValue::from({n}_f64)"),
                IrConst::Float(f) => format!("JsValue::from({f:?}_f64)"),
                IrConst::Infinity => "JsValue::from(f64::INFINITY)".to_owned(),
                IrConst::NegInfinity => "JsValue::from(f64::NEG_INFINITY)".to_owned(),
                IrConst::NaN => "JsValue::from(f64::NAN)".to_owned(),
            };
            let _ = writeln!(
                self.install,
                "    def_const(ctx, &{holder}, {:?}, {v})?;",
                c.name
            );
            if !ns {
                let _ = writeln!(
                    self.install,
                    "    def_const(ctx, &{var}, {:?}, {v})?;",
                    c.name
                );
            }
        }
        for (a, n) in i.attributes.iter().zip(&names.attrs) {
            let target = if a.is_static { &holder } else { &var };
            let t = self.attr_fns(i, a, n, &prefix, target);
            fns.push_str(&t);
        }
        if let Some(sn) = &names.stringifier {
            let fname = format!("{prefix}_to_string");
            let o = Overload {
                args: Vec::new(),
                ret: Conv::String {
                    kind: StringType::DOMString,
                    null_to_empty: false,
                },
                required: 0,
                max_args: Some(0),
            };
            let call = format!("<H as {ty}Host>::{sn}(h, this)");
            match self.overload_fn(&fname, &o, Some(&ty), &call, false) {
                Ok(t) => {
                    fns.push_str(&t);
                    let _ = writeln!(
                        self.install,
                        "    def_method(ctx, &{var}, \"toString\", {fname}::<H>, 0)?;"
                    );
                }
                Err(e) => self.skip(&i.name, "stringifier", &e),
            }
        }
        for (op, hn) in i.operations.iter().zip(&names.ops) {
            let Some(name) = &op.name else {
                self.skip(
                    &i.name,
                    "special operation",
                    "getter/setter/deleter need exotic objects",
                );
                continue;
            };
            let fname = format!("{prefix}_{}", ident_part(name));
            let this_arg = if op.is_static { "" } else { ", this" };
            let calls: Vec<String> = hn
                .iter()
                .map(|n| format!("<H as {ty}Host>::{n}(h{this_arg}{{args}})"))
                .collect();
            let this_ty = (!op.is_static).then_some(ty.as_str());
            match self.overload_set(&fname, &op.overloads, this_ty, &calls) {
                Ok((t, len)) => {
                    fns.push_str(&t);
                    let target = if op.is_static { &holder } else { &var };
                    let _ = writeln!(
                        self.install,
                        "    def_method(ctx, &{target}, {name:?}, {fname}::<H>, {len})?;"
                    );
                    if op.stringifier {
                        let _ = writeln!(
                            self.install,
                            "    def_method(ctx, &{target}, \"toString\", {fname}::<H>, 0)?;"
                        );
                    }
                }
                Err(e) => self.skip(&i.name, name, &e),
            }
        }
        if i.collection.is_some() {
            self.skip(
                &i.name,
                "iterable/maplike/setlike",
                "collection bindings are not emitted",
            );
        }
        let global = if ns { &var } else { &holder };
        let _ = writeln!(
            self.install,
            "    ctx.register_global_property(js_string!({:?}), {global}, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;",
            i.name
        );
    }

    fn run(mut self) -> String {
        let b = self.b;
        let mut ifaces: Vec<&InterfaceBinding> = b
            .interfaces
            .iter()
            .filter(|i| i.kind != ContainerKind::CallbackInterface)
            .collect();
        // Parents before children so `install` can look a prototype up.
        ifaces.sort_by_key(|i| (i.ancestors.len(), i.name.clone()));
        let mut fns = String::new();
        for i in &ifaces {
            self.interface(i, &mut fns);
        }

        self.w("// @generated by px-bindings (emitter `boa-glue`). Do not edit.");
        self.w("//");
        self.w("// Boa glue for the host traits in `host.rs`. Compile against");
        self.w("// `boa_engine =0.22.0` (ADR 040) as a sibling module of `host`.");
        self.w("");
        self.w("use super::host::*;");
        self.out.push_str(PRELUDE);
        self.w("");
        self.w("/// Each interface with its inheritance chain, itself first.");
        self.w("pub static INTERFACES: &[Interface] = &[");
        for i in &ifaces {
            let mut chain = vec![i.name.clone()];
            chain.extend(i.ancestors.iter().cloned());
            let list: Vec<String> = chain.iter().map(|a| format!("{a:?}")).collect();
            self.w(&format!(
                "    Interface {{ name: {:?}, chain: &[{}] }},",
                i.name,
                list.join(", ")
            ));
        }
        self.w("];");
        self.w("");
        self.w("/// Members that have no binding, with the reason. Empty means complete.");
        self.w("pub static UNSUPPORTED: &[&str] = &[");
        for u in std::mem::take(&mut self.unsupported) {
            self.w(&format!("    {u:?},"));
        }
        self.w("];");
        self.w("");
        let mut supers: Vec<String> = ifaces
            .iter()
            .map(|i| format!("{}Host", type_ident(&i.name)))
            .collect();
        supers.push("'static".to_owned());
        self.w("/// Everything the glue needs from the embedder: all the host traits, plus");
        self.w("/// the one question a handle cannot answer for itself.");
        self.w(&format!("pub trait ScriptHost: {} {{", supers.join(" + ")));
        self.w("    /// The most derived interface of the object `index`/`generation`, when it");
        self.w("    /// is more specific than the static type of the place it was returned");
        self.w("    /// from (a `Node` that is an `Element`). It must name `static_interface`");
        self.w("    /// or a descendant; anything else is a `TypeError`.");
        self.w("    fn dynamic_interface(&mut self, static_interface: &'static str, index: u32, generation: u32) -> Option<&'static str> {");
        self.w("        let _ = (static_interface, index, generation);");
        self.w("        None");
        self.w("    }");
        self.w("}");
        self.w("");
        self.out.push_str(&fns);
        self.w("");
        self.w("/// Create the prototypes, constructors, namespaces and globals.");
        self.w("pub fn install<H: ScriptHost>(ctx: &mut Context, host: Rc<RefCell<H>>) -> JsResult<()> {");
        self.w("    ctx.insert_data(HostSlot(host));");
        self.w("    let mut protos: BTreeMap<&'static str, JsObject> = BTreeMap::new();");
        self.w("    let mut ctors: BTreeMap<&'static str, JsObject> = BTreeMap::new();");
        let ins = std::mem::take(&mut self.install);
        self.out.push_str(&ins);
        self.w("    ctx.insert_data(Prototypes(protos));");
        self.w("    Ok(())");
        self.w("}");
        self.out
    }
}

fn ident_part(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .to_ascii_lowercase()
}

fn kind_of(c: &Conv) -> &'static str {
    match c {
        Conv::Any => "any",
        Conv::Undefined => "undefined",
        Conv::Boolean => "boolean",
        Conv::Integer { .. } => "integer",
        Conv::BigInt => "bigint",
        Conv::Float { .. } => "float",
        Conv::String { .. } => "string",
        Conv::Object => "object",
        Conv::Symbol => "symbol",
        Conv::Buffer { .. } => "buffer source",
        Conv::Interface(_) => "interface",
        Conv::CallbackInterface(_) => "callback interface",
        Conv::CallbackFunction(_) => "callback function",
        Conv::Dictionary(_) => "dictionary",
        Conv::Enum(_) => "enum",
        Conv::Nullable(_) => "nullable",
        Conv::Sequence(_) => "sequence",
        Conv::AsyncSequence(_) => "async sequence",
        Conv::FrozenArray(_) => "FrozenArray",
        Conv::ObservableArray(_) => "ObservableArray",
        Conv::Record { .. } => "record",
        Conv::Promise(_) => "Promise",
        Conv::Union(_) => "union",
    }
}

const PRELUDE: &str = include_str!("boa_prelude.txt");

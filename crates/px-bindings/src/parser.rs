//! Recursive-descent parser for the WebIDL grammar (appendix "IDL grammar").
//!
//! Every nested construct (types, unions, raw extended-attribute brackets)
//! passes through `Parser::enter`, which fails at `MAX_NESTING` instead of
//! overflowing the stack. Extended attributes are parsed in one of the five
//! spec forms with a bounded backtrack to the generic `Other` capture, so any
//! sentence of the generic grammar is accepted and nothing is lost.

use crate::ast::*;
use crate::lexer::{Token, TokenKind, lex};
use crate::span::{Span, SyntaxError};

/// Deepest nesting of types, unions and bracketed extended-attribute text.
pub const MAX_NESTING: u32 = 32;

const ARG_NAME_KEYWORDS: &[&str] = &[
    "attribute",
    "callback",
    "const",
    "constructor",
    "deleter",
    "dictionary",
    "enum",
    "getter",
    "includes",
    "inherit",
    "interface",
    "iterable",
    "maplike",
    "mixin",
    "namespace",
    "partial",
    "readonly",
    "required",
    "setlike",
    "setter",
    "static",
    "stringifier",
    "typedef",
    "unrestricted",
];

const OTHER_KEYWORDS: &[&str] = &[
    "any",
    "async_iterable",
    "async_sequence",
    "optional",
    "or",
    "null",
    "true",
    "false",
    "Infinity",
    "-Infinity",
    "NaN",
    "undefined",
    "object",
    "symbol",
    "sequence",
    "record",
    "Promise",
    "FrozenArray",
    "ObservableArray",
    "boolean",
    "byte",
    "octet",
    "bigint",
    "float",
    "double",
    "short",
    "long",
    "unsigned",
    "ByteString",
    "DOMString",
    "USVString",
];

fn is_keyword(text: &str) -> bool {
    ARG_NAME_KEYWORDS.contains(&text)
        || OTHER_KEYWORDS.contains(&text)
        || BufferType::ALL.iter().any(|(n, _)| *n == text)
}

/// Which members a container body admits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Interface,
    PartialInterface,
    Mixin,
    CallbackInterface,
    Namespace,
}

struct Parser<'a> {
    toks: Vec<Token<'a>>,
    pos: usize,
    depth: u32,
    file_name: &'a str,
    eof: Token<'a>,
}

type PResult<T> = Result<T, SyntaxError>;

/// Parse one IDL file. `file` is the index recorded in every `Span`.
pub fn parse(file: u32, file_name: &str, source: &str) -> Result<Ast, SyntaxError> {
    let toks = lex(file, file_name, source)?;
    let eof = Token {
        kind: TokenKind::Eof,
        text: "",
        escaped: false,
        span: Span {
            file,
            ..Span::default()
        },
    };
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
        file_name,
        eof,
    };
    let mut definitions = Vec::new();
    while p.peek().kind != TokenKind::Eof {
        definitions.push(p.definition()?);
    }
    Ok(Ast {
        file,
        file_name: file_name.to_owned(),
        definitions,
    })
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Token<'a> {
        self.peek_n(0)
    }

    fn peek_n(&self, n: usize) -> Token<'a> {
        self.pos
            .checked_add(n)
            .and_then(|i| self.toks.get(i))
            .copied()
            .unwrap_or(self.eof)
    }

    fn bump(&mut self) -> Token<'a> {
        let t = self.peek();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    fn describe(t: Token<'_>) -> String {
        match t.kind {
            TokenKind::Eof => "end of file".to_owned(),
            TokenKind::String => format!("\"{}\"", t.text),
            _ => format!("`{}`", t.text),
        }
    }

    fn err_expected(&self, expected: &[&str]) -> SyntaxError {
        let t = self.peek();
        SyntaxError {
            file: self.file_name.to_owned(),
            line: t.span.line.max(1),
            col: t.span.col.max(1),
            message: format!("unexpected {}", Self::describe(t)),
            expected: expected.iter().map(|s| (*s).to_owned()).collect(),
            found: Self::describe(t),
        }
    }

    fn err_msg(&self, span: Span, message: &str) -> SyntaxError {
        SyntaxError {
            file: self.file_name.to_owned(),
            line: span.line.max(1),
            col: span.col.max(1),
            message: message.to_owned(),
            expected: Vec::new(),
            found: String::new(),
        }
    }

    fn enter(&mut self) -> PResult<()> {
        if self.depth >= MAX_NESTING {
            return Err(self.err_msg(
                self.peek().span,
                &format!("nesting is deeper than the limit of {MAX_NESTING}"),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn span_from(&self, start: Span) -> Span {
        let end = self
            .pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(start.end, |t| t.span.end);
        Span { end, ..start }
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.is_kw_at(0, kw)
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        let t = self.peek_n(n);
        t.kind == TokenKind::Identifier && !t.escaped && t.text == kw
    }

    fn is_punct(&self, p: &str) -> bool {
        self.is_punct_at(0, p)
    }

    fn is_punct_at(&self, n: usize, p: &str) -> bool {
        let t = self.peek_n(n);
        t.kind == TokenKind::Other && t.text == p
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let hit = self.is_kw(kw);
        if hit {
            self.bump();
        }
        hit
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        let hit = self.is_punct(p);
        if hit {
            self.bump();
        }
        hit
    }

    fn expect_kw(&mut self, kw: &str) -> PResult<Token<'a>> {
        if self.is_kw(kw) {
            Ok(self.bump())
        } else {
            Err(self.err_expected(&[&format!("`{kw}`")]))
        }
    }

    fn expect_punct(&mut self, p: &str) -> PResult<Token<'a>> {
        if self.is_punct(p) {
            Ok(self.bump())
        } else {
            Err(self.err_expected(&[&format!("`{p}`")]))
        }
    }

    /// An identifier that is not a keyword (or is escaped), accepting extra
    /// keyword spellings the grammar allows in this position.
    fn name_with(&mut self, extra: &[&str], what: &str) -> PResult<String> {
        let t = self.peek();
        if t.kind == TokenKind::Identifier
            && (t.escaped || !is_keyword(t.text) || extra.contains(&t.text))
        {
            self.bump();
            Ok(t.text.to_owned())
        } else {
            Err(self.err_expected(&[what]))
        }
    }

    fn ident(&mut self) -> PResult<String> {
        self.name_with(&[], "identifier")
    }

    fn string_lit(&mut self) -> PResult<String> {
        let t = self.peek();
        if t.kind == TokenKind::String {
            self.bump();
            Ok(t.text.to_owned())
        } else {
            Err(self.err_expected(&["string"]))
        }
    }

    // ---- definitions -------------------------------------------------

    fn definition(&mut self) -> PResult<Definition> {
        let ext_attrs = self.ext_attr_list()?;
        let start = self.peek().span;
        let kind = if self.eat_kw("callback") {
            if self.eat_kw("interface") {
                let name = self.ident()?;
                let members = self.members(Ctx::CallbackInterface)?;
                DefinitionKind::CallbackInterface(Container {
                    name,
                    partial: false,
                    inherits: None,
                    members,
                })
            } else {
                let name = self.ident()?;
                self.expect_punct("=")?;
                let ret = self.ty()?;
                self.expect_punct("(")?;
                let args = self.argument_list()?;
                self.expect_punct(")")?;
                self.expect_punct(";")?;
                DefinitionKind::Callback(CallbackDef { name, ret, args })
            }
        } else if self.eat_kw("interface") {
            if self.eat_kw("mixin") {
                let name = self.ident()?;
                let members = self.members(Ctx::Mixin)?;
                DefinitionKind::InterfaceMixin(Container {
                    name,
                    partial: false,
                    inherits: None,
                    members,
                })
            } else {
                let name = self.ident()?;
                let inherits = self.inheritance()?;
                let members = self.members(Ctx::Interface)?;
                DefinitionKind::Interface(Container {
                    name,
                    partial: false,
                    inherits,
                    members,
                })
            }
        } else if self.eat_kw("partial") {
            if self.eat_kw("interface") {
                if self.eat_kw("mixin") {
                    let name = self.ident()?;
                    let members = self.members(Ctx::Mixin)?;
                    DefinitionKind::InterfaceMixin(Container {
                        name,
                        partial: true,
                        inherits: None,
                        members,
                    })
                } else {
                    let name = self.ident()?;
                    let members = self.members(Ctx::PartialInterface)?;
                    DefinitionKind::Interface(Container {
                        name,
                        partial: true,
                        inherits: None,
                        members,
                    })
                }
            } else if self.eat_kw("dictionary") {
                let name = self.ident()?;
                let members = self.dict_members()?;
                DefinitionKind::Dictionary(DictionaryDef {
                    name,
                    partial: true,
                    inherits: None,
                    members,
                })
            } else if self.eat_kw("namespace") {
                let name = self.ident()?;
                let members = self.members(Ctx::Namespace)?;
                DefinitionKind::Namespace(Container {
                    name,
                    partial: true,
                    inherits: None,
                    members,
                })
            } else {
                return Err(self.err_expected(&["`interface`", "`dictionary`", "`namespace`"]));
            }
        } else if self.eat_kw("namespace") {
            let name = self.ident()?;
            let members = self.members(Ctx::Namespace)?;
            DefinitionKind::Namespace(Container {
                name,
                partial: false,
                inherits: None,
                members,
            })
        } else if self.eat_kw("dictionary") {
            let name = self.ident()?;
            let inherits = self.inheritance()?;
            let members = self.dict_members()?;
            DefinitionKind::Dictionary(DictionaryDef {
                name,
                partial: false,
                inherits,
                members,
            })
        } else if self.eat_kw("enum") {
            let name = self.ident()?;
            self.expect_punct("{")?;
            let mut values = vec![self.string_lit()?];
            while self.eat_punct(",") {
                if self.peek().kind == TokenKind::String {
                    values.push(self.string_lit()?);
                } else {
                    break;
                }
            }
            self.expect_punct("}")?;
            self.expect_punct(";")?;
            DefinitionKind::Enum(EnumDef { name, values })
        } else if self.eat_kw("typedef") {
            let ty = self.type_with_ext()?;
            let name = self.ident()?;
            self.expect_punct(";")?;
            DefinitionKind::Typedef(TypedefDef { name, ty })
        } else if self.peek().kind == TokenKind::Identifier && self.is_kw_at(1, "includes") {
            let target = self.ident()?;
            self.expect_kw("includes")?;
            let mixin = self.ident()?;
            self.expect_punct(";")?;
            DefinitionKind::Includes(IncludesDef { target, mixin })
        } else {
            return Err(self.err_expected(&[
                "`interface`",
                "`partial`",
                "`namespace`",
                "`dictionary`",
                "`enum`",
                "`typedef`",
                "`callback`",
                "an includes statement",
            ]));
        };
        Ok(Definition {
            ext_attrs,
            span: self.span_from(start),
            kind,
        })
    }

    fn inheritance(&mut self) -> PResult<Option<String>> {
        if self.eat_punct(":") {
            Ok(Some(self.ident()?))
        } else {
            Ok(None)
        }
    }

    fn dict_members(&mut self) -> PResult<Vec<DictMember>> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        while !self.is_punct("}") {
            let ext_attrs = self.ext_attr_list()?;
            let start = self.peek().span;
            let required = self.eat_kw("required");
            let (ty, default) = if required {
                (self.type_with_ext()?, None)
            } else {
                (self.ty()?, None)
            };
            let name = self.ident()?;
            let default = if required {
                default
            } else {
                self.default_opt()?
            };
            self.expect_punct(";")?;
            out.push(DictMember {
                ext_attrs,
                span: self.span_from(start),
                required,
                ty,
                name,
                default,
            });
        }
        self.expect_punct("}")?;
        self.expect_punct(";")?;
        Ok(out)
    }

    // ---- members -----------------------------------------------------

    fn members(&mut self, ctx: Ctx) -> PResult<Vec<Member>> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        while !self.is_punct("}") {
            let ext_attrs = self.ext_attr_list()?;
            let start = self.peek().span;
            let kind = self.member_kind(ctx)?;
            out.push(Member {
                ext_attrs,
                span: self.span_from(start),
                kind,
            });
        }
        self.expect_punct("}")?;
        self.expect_punct(";")?;
        Ok(out)
    }

    fn member_expected(ctx: Ctx) -> &'static [&'static str] {
        match ctx {
            Ctx::Interface => &[
                "`const`",
                "`constructor`",
                "`attribute`",
                "`readonly`",
                "`static`",
                "`stringifier`",
                "`iterable`",
                "`maplike`",
                "`setlike`",
                "an operation",
                "`}`",
            ],
            Ctx::PartialInterface => &[
                "`const`",
                "`attribute`",
                "`readonly`",
                "`static`",
                "`stringifier`",
                "`iterable`",
                "`maplike`",
                "`setlike`",
                "an operation",
                "`}`",
            ],
            Ctx::Mixin => &[
                "`const`",
                "`attribute`",
                "`readonly`",
                "`stringifier`",
                "an operation",
                "`}`",
            ],
            Ctx::CallbackInterface => &["`const`", "an operation", "`}`"],
            Ctx::Namespace => &["`const`", "`readonly`", "an operation", "`}`"],
        }
    }

    fn member_kind(&mut self, ctx: Ctx) -> PResult<MemberKind> {
        let full = ctx == Ctx::Interface;
        let iface = matches!(ctx, Ctx::Interface | Ctx::PartialInterface);
        let regular_only = matches!(ctx, Ctx::Mixin | Ctx::CallbackInterface | Ctx::Namespace);

        if self.is_kw("const") {
            return self.const_member();
        }
        if full && self.is_kw("constructor") {
            self.bump();
            self.expect_punct("(")?;
            let args = self.argument_list()?;
            self.expect_punct(")")?;
            self.expect_punct(";")?;
            return Ok(MemberKind::Constructor { args });
        }
        if iface && self.is_kw("static") {
            self.bump();
            if self.is_kw("readonly") || self.is_kw("attribute") {
                let readonly = self.eat_kw("readonly");
                return self.attribute(readonly, false, true, false);
            }
            return self.operation(None, true, false);
        }
        if matches!(ctx, Ctx::Interface | Ctx::PartialInterface | Ctx::Mixin)
            && self.is_kw("stringifier")
        {
            self.bump();
            if self.eat_punct(";") {
                return Ok(MemberKind::Stringifier);
            }
            if self.is_kw("readonly") || self.is_kw("attribute") {
                let readonly = self.eat_kw("readonly");
                return self.attribute(readonly, false, false, true);
            }
            return self.operation(None, false, true);
        }
        if iface && self.is_kw("iterable") {
            self.bump();
            self.expect_punct("<")?;
            let decl = self.iter_decl()?;
            self.expect_punct(">")?;
            self.expect_punct(";")?;
            return Ok(MemberKind::Iterable(decl));
        }
        if iface && self.is_kw("async_iterable") {
            self.bump();
            self.expect_punct("<")?;
            let decl = self.iter_decl()?;
            self.expect_punct(">")?;
            let args = if self.eat_punct("(") {
                let a = self.argument_list()?;
                self.expect_punct(")")?;
                a
            } else {
                Vec::new()
            };
            self.expect_punct(";")?;
            return Ok(MemberKind::AsyncIterable { decl, args });
        }
        if iface && (self.is_kw("maplike") || self.is_kw("setlike")) {
            return self.collection(false);
        }
        if self.is_kw("readonly") {
            self.bump();
            if iface && (self.is_kw("maplike") || self.is_kw("setlike")) {
                return self.collection(true);
            }
            return self.attribute(true, false, false, false);
        }
        if iface && self.is_kw("inherit") {
            self.bump();
            return self.attribute(false, true, false, false);
        }
        if self.is_kw("attribute") && ctx != Ctx::CallbackInterface && ctx != Ctx::Namespace {
            return self.attribute(false, false, false, false);
        }
        if full && (self.is_kw("getter") || self.is_kw("setter") || self.is_kw("deleter")) {
            let special = if self.eat_kw("getter") {
                Special::Getter
            } else if self.eat_kw("setter") {
                Special::Setter
            } else {
                self.bump();
                Special::Deleter
            };
            return self.operation(Some(special), false, false);
        }
        // A regular operation starts with a type; anything that cannot start
        // one is reported against the member list instead.
        if self.starts_type() || regular_only || iface {
            if !self.starts_type() {
                return Err(self.err_expected(Self::member_expected(ctx)));
            }
            return self.operation(None, false, false);
        }
        Err(self.err_expected(Self::member_expected(ctx)))
    }

    fn starts_type(&self) -> bool {
        let t = self.peek();
        match t.kind {
            TokenKind::Identifier => {
                t.escaped
                    || !is_keyword(t.text)
                    || matches!(
                        t.text,
                        "any"
                            | "Promise"
                            | "undefined"
                            | "object"
                            | "symbol"
                            | "sequence"
                            | "async_sequence"
                            | "record"
                            | "FrozenArray"
                            | "ObservableArray"
                            | "boolean"
                            | "byte"
                            | "octet"
                            | "bigint"
                            | "float"
                            | "double"
                            | "short"
                            | "long"
                            | "unsigned"
                            | "unrestricted"
                            | "ByteString"
                            | "DOMString"
                            | "USVString"
                    )
                    || BufferType::ALL.iter().any(|(n, _)| *n == t.text)
            }
            TokenKind::Other => t.text == "(",
            _ => false,
        }
    }

    fn const_member(&mut self) -> PResult<MemberKind> {
        self.expect_kw("const")?;
        let ty = match self.primitive()? {
            Some(t) => t,
            None => {
                let start = self.peek().span;
                let name = self.ident()?;
                Type {
                    kind: TypeKind::Named(name),
                    nullable: false,
                    ext_attrs: Vec::new(),
                    span: self.span_from(start),
                }
            }
        };
        let name = self.ident()?;
        self.expect_punct("=")?;
        let value = self.const_value()?.ok_or_else(|| {
            self.err_expected(&["boolean", "integer", "decimal", "`Infinity`", "`NaN`"])
        })?;
        self.expect_punct(";")?;
        Ok(MemberKind::Const { ty, name, value })
    }

    fn const_value(&mut self) -> PResult<Option<ConstValue>> {
        let t = self.peek();
        let v = match (t.kind, t.text) {
            (TokenKind::Identifier, "true") if !t.escaped => ConstValue::Boolean(true),
            (TokenKind::Identifier, "false") if !t.escaped => ConstValue::Boolean(false),
            (TokenKind::Identifier, "Infinity") if !t.escaped => ConstValue::Infinity,
            (TokenKind::Identifier, "-Infinity") if !t.escaped => ConstValue::NegInfinity,
            (TokenKind::Identifier, "NaN") if !t.escaped => ConstValue::NaN,
            (TokenKind::Integer, text) => ConstValue::Integer(text.to_owned()),
            (TokenKind::Decimal, text) => ConstValue::Decimal(text.to_owned()),
            _ => return Ok(None),
        };
        self.bump();
        Ok(Some(v))
    }

    fn default_opt(&mut self) -> PResult<Option<DefaultValue>> {
        if !self.eat_punct("=") {
            return Ok(None);
        }
        if let Some(c) = self.const_value()? {
            return Ok(Some(DefaultValue::Const(c)));
        }
        let t = self.peek();
        match (t.kind, t.text) {
            (TokenKind::String, text) => {
                self.bump();
                Ok(Some(DefaultValue::String(text.to_owned())))
            }
            (TokenKind::Other, "[") => {
                self.bump();
                self.expect_punct("]")?;
                Ok(Some(DefaultValue::EmptyArray))
            }
            (TokenKind::Other, "{") => {
                self.bump();
                self.expect_punct("}")?;
                Ok(Some(DefaultValue::EmptyDictionary))
            }
            (TokenKind::Identifier, "null") if !t.escaped => {
                self.bump();
                Ok(Some(DefaultValue::Null))
            }
            (TokenKind::Identifier, "undefined") if !t.escaped => {
                self.bump();
                Ok(Some(DefaultValue::Undefined))
            }
            _ => Err(self.err_expected(&[
                "a constant",
                "string",
                "`[`",
                "`{`",
                "`null`",
                "`undefined`",
            ])),
        }
    }

    fn attribute(
        &mut self,
        readonly: bool,
        inherit: bool,
        is_static: bool,
        stringifier: bool,
    ) -> PResult<MemberKind> {
        self.expect_kw("attribute")?;
        let ty = self.type_with_ext()?;
        let name = self.name_with(&["required"], "attribute name")?;
        self.expect_punct(";")?;
        Ok(MemberKind::Attribute {
            readonly,
            inherit,
            is_static,
            stringifier,
            ty,
            name,
        })
    }

    fn operation(
        &mut self,
        special: Option<Special>,
        is_static: bool,
        stringifier: bool,
    ) -> PResult<MemberKind> {
        let ret = self.ty()?;
        let name = if self.is_punct("(") {
            None
        } else {
            Some(self.name_with(&["includes"], "operation name or `(`")?)
        };
        self.expect_punct("(")?;
        let args = self.argument_list()?;
        self.expect_punct(")")?;
        self.expect_punct(";")?;
        Ok(MemberKind::Operation {
            special,
            is_static,
            stringifier,
            ret,
            name,
            args,
        })
    }

    fn iter_decl(&mut self) -> PResult<IterDecl> {
        let first = self.type_with_ext()?;
        let second = if self.eat_punct(",") {
            Some(self.type_with_ext()?)
        } else {
            None
        };
        Ok(IterDecl { first, second })
    }

    fn collection(&mut self, readonly: bool) -> PResult<MemberKind> {
        if self.eat_kw("maplike") {
            self.expect_punct("<")?;
            let key = self.type_with_ext()?;
            self.expect_punct(",")?;
            let value = self.type_with_ext()?;
            self.expect_punct(">")?;
            self.expect_punct(";")?;
            Ok(MemberKind::Maplike {
                readonly,
                key,
                value,
            })
        } else {
            self.expect_kw("setlike")?;
            self.expect_punct("<")?;
            let ty = self.type_with_ext()?;
            self.expect_punct(">")?;
            self.expect_punct(";")?;
            Ok(MemberKind::Setlike { readonly, ty })
        }
    }

    // ---- arguments ---------------------------------------------------

    fn argument_list(&mut self) -> PResult<Vec<Argument>> {
        let mut out = Vec::new();
        if self.is_punct(")") {
            return Ok(out);
        }
        loop {
            out.push(self.argument()?);
            if !self.eat_punct(",") {
                return Ok(out);
            }
        }
    }

    fn argument(&mut self) -> PResult<Argument> {
        let ext_attrs = self.ext_attr_list()?;
        let start = self.peek().span;
        if self.eat_kw("optional") {
            let ty = self.type_with_ext()?;
            let name = self.arg_name()?;
            let default = self.default_opt()?;
            return Ok(Argument {
                ext_attrs,
                span: self.span_from(start),
                optional: true,
                variadic: false,
                ty,
                name,
                default,
            });
        }
        let ty = self.ty()?;
        let variadic = self.eat_punct("...");
        let name = self.arg_name()?;
        Ok(Argument {
            ext_attrs,
            span: self.span_from(start),
            optional: false,
            variadic,
            ty,
            name,
            default: None,
        })
    }

    fn arg_name(&mut self) -> PResult<String> {
        self.name_with(ARG_NAME_KEYWORDS, "argument name")
    }

    // ---- types -------------------------------------------------------

    fn type_with_ext(&mut self) -> PResult<Type> {
        let ext = self.ext_attr_list()?;
        let mut t = self.ty()?;
        t.ext_attrs = ext;
        Ok(t)
    }

    fn null(&mut self) -> bool {
        self.eat_punct("?")
    }

    fn ty(&mut self) -> PResult<Type> {
        self.enter()?;
        let r = self.ty_inner();
        self.leave();
        r
    }

    fn ty_inner(&mut self) -> PResult<Type> {
        let start = self.peek().span;
        if self.is_punct("(") {
            let kind = self.union_body()?;
            let nullable = self.null();
            return Ok(self.mk(kind, nullable, start));
        }
        if self.eat_kw("any") {
            return Ok(self.mk(TypeKind::Any, false, start));
        }
        if self.is_kw("Promise") {
            self.bump();
            self.expect_punct("<")?;
            let inner = self.ty()?;
            self.expect_punct(">")?;
            return Ok(self.mk(TypeKind::Promise(Box::new(inner)), false, start));
        }
        self.distinguishable()
    }

    fn mk(&self, kind: TypeKind, nullable: bool, start: Span) -> Type {
        Type {
            kind,
            nullable,
            ext_attrs: Vec::new(),
            span: self.span_from(start),
        }
    }

    fn union_body(&mut self) -> PResult<TypeKind> {
        self.expect_punct("(")?;
        let mut members = vec![self.union_member()?];
        self.expect_kw("or")?;
        members.push(self.union_member()?);
        while self.eat_kw("or") {
            members.push(self.union_member()?);
        }
        self.expect_punct(")")?;
        Ok(TypeKind::Union(members))
    }

    fn union_member(&mut self) -> PResult<Type> {
        self.enter()?;
        let r = self.union_member_inner();
        self.leave();
        r
    }

    fn union_member_inner(&mut self) -> PResult<Type> {
        let ext = self.ext_attr_list()?;
        let start = self.peek().span;
        let mut t = if self.is_punct("(") {
            let kind = self.union_body()?;
            let nullable = self.null();
            self.mk(kind, nullable, start)
        } else {
            self.distinguishable()?
        };
        t.ext_attrs = ext;
        Ok(t)
    }

    /// `PrimitiveType Null` or `identifier`-like leaf without the null suffix.
    fn primitive(&mut self) -> PResult<Option<Type>> {
        let start = self.peek().span;
        let kind = if self.eat_kw("boolean") {
            TypeKind::Boolean
        } else if self.eat_kw("byte") {
            TypeKind::Byte
        } else if self.eat_kw("octet") {
            TypeKind::Octet
        } else if self.eat_kw("bigint") {
            TypeKind::Bigint
        } else if self.is_kw("unsigned") || self.is_kw("short") || self.is_kw("long") {
            let unsigned = self.eat_kw("unsigned");
            if self.eat_kw("short") {
                TypeKind::Short { unsigned }
            } else if self.eat_kw("long") {
                if self.eat_kw("long") {
                    TypeKind::LongLong { unsigned }
                } else {
                    TypeKind::Long { unsigned }
                }
            } else {
                return Err(self.err_expected(&["`short`", "`long`"]));
            }
        } else if self.is_kw("unrestricted") || self.is_kw("float") || self.is_kw("double") {
            let unrestricted = self.eat_kw("unrestricted");
            if self.eat_kw("float") {
                TypeKind::Float { unrestricted }
            } else if self.eat_kw("double") {
                TypeKind::Double { unrestricted }
            } else {
                return Err(self.err_expected(&["`float`", "`double`"]));
            }
        } else {
            return Ok(None);
        };
        Ok(Some(self.mk(kind, false, start)))
    }

    fn string_type(&mut self) -> Option<StringType> {
        let st = if self.is_kw("ByteString") {
            StringType::ByteString
        } else if self.is_kw("DOMString") {
            StringType::DOMString
        } else if self.is_kw("USVString") {
            StringType::USVString
        } else {
            return None;
        };
        self.bump();
        Some(st)
    }

    fn angle_inner(&mut self) -> PResult<Box<Type>> {
        self.expect_punct("<")?;
        let t = self.type_with_ext()?;
        self.expect_punct(">")?;
        Ok(Box::new(t))
    }

    fn distinguishable(&mut self) -> PResult<Type> {
        self.enter()?;
        let r = self.distinguishable_inner();
        self.leave();
        r
    }

    fn distinguishable_inner(&mut self) -> PResult<Type> {
        let start = self.peek().span;
        if let Some(mut t) = self.primitive()? {
            t.nullable = self.null();
            t.span = self.span_from(start);
            return Ok(t);
        }
        let kind = if let Some(st) = self.string_type() {
            TypeKind::String(st)
        } else if self.eat_kw("object") {
            TypeKind::Object
        } else if self.eat_kw("symbol") {
            TypeKind::Symbol
        } else if self.eat_kw("undefined") {
            TypeKind::Undefined
        } else if self.eat_kw("sequence") {
            TypeKind::Sequence(self.angle_inner()?)
        } else if self.eat_kw("async_sequence") {
            TypeKind::AsyncSequence(self.angle_inner()?)
        } else if self.eat_kw("FrozenArray") {
            TypeKind::FrozenArray(self.angle_inner()?)
        } else if self.eat_kw("ObservableArray") {
            TypeKind::ObservableArray(self.angle_inner()?)
        } else if self.eat_kw("record") {
            self.expect_punct("<")?;
            let Some(key) = self.string_type() else {
                return Err(self.err_expected(&["`ByteString`", "`DOMString`", "`USVString`"]));
            };
            self.expect_punct(",")?;
            let v = self.type_with_ext()?;
            self.expect_punct(">")?;
            TypeKind::Record(key, Box::new(v))
        } else if let Some(bt) = BufferType::ALL
            .iter()
            .find(|(n, _)| self.is_kw(n))
            .map(|(_, t)| *t)
        {
            self.bump();
            TypeKind::Buffer(bt)
        } else {
            let t = self.peek();
            if t.kind == TokenKind::Identifier && (t.escaped || !is_keyword(t.text)) {
                self.bump();
                TypeKind::Named(t.text.to_owned())
            } else {
                return Err(self.err_expected(&["a type"]));
            }
        };
        let nullable = self.null();
        Ok(self.mk(kind, nullable, start))
    }

    // ---- extended attributes ------------------------------------------

    fn ext_attr_list(&mut self) -> PResult<Vec<ExtAttr>> {
        let mut out = Vec::new();
        if !self.eat_punct("[") {
            return Ok(out);
        }
        loop {
            out.push(self.ext_attr()?);
            if self.eat_punct(",") {
                continue;
            }
            self.expect_punct("]")?;
            return Ok(out);
        }
    }

    fn at_attr_end(&self) -> bool {
        self.is_punct(",") || self.is_punct("]")
    }

    fn ext_attr(&mut self) -> PResult<ExtAttr> {
        let save = self.pos;
        let start = self.peek().span;
        if let Some(attr) = self.ext_attr_form(start)
            && self.at_attr_end()
        {
            return Ok(attr);
        }
        self.pos = save;
        // `enter`/`leave` are balanced even on error paths, so depth is
        // already back to its value at the save point.
        self.ext_attr_other(start)
    }

    /// One of the five named forms, or `None` if the tokens do not fit.
    fn ext_attr_form(&mut self, start: Span) -> Option<ExtAttr> {
        let t = self.peek();
        if t.kind != TokenKind::Identifier {
            return None;
        }
        self.bump();
        let name = t.text.to_owned();
        let value = if self.is_punct("(") {
            self.bump();
            let args = self.argument_list().ok()?;
            self.expect_punct(")").ok()?;
            ExtAttrValue::ArgList(args)
        } else if self.eat_punct("=") {
            let v = self.peek();
            match v.kind {
                TokenKind::Identifier => {
                    self.bump();
                    if self.eat_punct("(") {
                        let args = self.argument_list().ok()?;
                        self.expect_punct(")").ok()?;
                        ExtAttrValue::NamedArgList {
                            name: v.text.to_owned(),
                            args,
                        }
                    } else {
                        ExtAttrValue::Ident(v.text.to_owned())
                    }
                }
                TokenKind::String => {
                    self.bump();
                    ExtAttrValue::String(v.text.to_owned())
                }
                TokenKind::Integer => {
                    self.bump();
                    ExtAttrValue::Integer(v.text.to_owned())
                }
                TokenKind::Decimal => {
                    self.bump();
                    ExtAttrValue::Decimal(v.text.to_owned())
                }
                TokenKind::Other if v.text == "*" => {
                    self.bump();
                    ExtAttrValue::Wildcard
                }
                TokenKind::Other if v.text == "(" => {
                    self.bump();
                    let mut ids = Vec::new();
                    loop {
                        let i = self.peek();
                        if i.kind != TokenKind::Identifier {
                            return None;
                        }
                        self.bump();
                        ids.push(i.text.to_owned());
                        if self.eat_punct(",") {
                            continue;
                        }
                        self.expect_punct(")").ok()?;
                        break;
                    }
                    ExtAttrValue::IdentList(ids)
                }
                _ => return None,
            }
        } else {
            ExtAttrValue::NoArgs
        };
        Some(ExtAttr {
            name,
            span: self.span_from(start),
            value,
        })
    }

    /// Capture tokens up to the next top-level `,` or `]`, balanced.
    fn ext_attr_other(&mut self, start: Span) -> PResult<ExtAttr> {
        let mut toks: Vec<String> = Vec::new();
        let mut stack: Vec<&'static str> = Vec::new();
        let first = self.peek();
        loop {
            let t = self.peek();
            if t.kind == TokenKind::Eof {
                return Err(self.err_expected(&["`]`"]));
            }
            if stack.is_empty() && t.kind == TokenKind::Other && (t.text == "," || t.text == "]") {
                break;
            }
            if t.kind == TokenKind::Other {
                match t.text {
                    "(" | "[" | "{" => {
                        if stack.len() >= MAX_NESTING as usize {
                            return Err(self.err_msg(
                                t.span,
                                &format!("nesting is deeper than the limit of {MAX_NESTING}"),
                            ));
                        }
                        stack.push(match t.text {
                            "(" => ")",
                            "[" => "]",
                            _ => "}",
                        });
                    }
                    ")" | "]" | "}" if stack.pop() != Some(t.text) => {
                        return Err(self.err_expected(&["a matching close bracket"]));
                    }
                    _ => {}
                }
            }
            toks.push(if t.kind == TokenKind::String {
                format!("\"{}\"", t.text)
            } else {
                t.text.to_owned()
            });
            self.bump();
        }
        if toks.is_empty() {
            return Err(self.err_expected(&["an extended attribute"]));
        }
        let name = if first.kind == TokenKind::Identifier {
            first.text.to_owned()
        } else {
            String::new()
        };
        Ok(ExtAttr {
            name,
            span: self.span_from(start),
            value: ExtAttrValue::Other(toks),
        })
    }
}

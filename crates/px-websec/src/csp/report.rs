//! Violation report data (CSP3 §5). Data only: nothing here sends anything.
//!
//! [`Violation`] holds the fields of a CSP violation. [`Violation::to_report_uri_json`]
//! renders the legacy `report-uri` body (`{"csp-report": {...}}`) and
//! [`Violation::to_reporting_api_json`] the `csp-violation` Reporting API
//! body. Delivery (and the network layer's rules about it) is someone else's
//! job; the endpoints to deliver to are on the policy ([`Policy::report_uri`],
//! [`Policy::report_to`]).
//!
//! Deviations: URLs are carried through [`CspUrl`], which drops query strings
//! and fragments, so `blocked-uri` and `document-uri` never contain a query.
//! That is stricter than the spec, which keeps the query.

use super::check::Blocked;
use super::policy::{Disposition, Policy};
use super::url::CspUrl;

/// Samples are limited to this many characters (CSP3 §5.3: 40).
pub const MAX_SAMPLE_CHARS: usize = 40;

/// What was blocked.
#[derive(Debug, Clone, Copy)]
pub enum BlockedResource<'a> {
    /// A network or other URL.
    Url(&'a CspUrl),
    /// A URL reached through a redirect to another origin: only the origin
    /// is reported (leaks nothing about the redirect target's path).
    UrlAfterCrossOriginRedirect(&'a CspUrl),
    /// Inline script/style/attribute.
    Inline,
    /// `eval` and friends.
    Eval,
    /// WebAssembly compilation.
    WasmEval,
}

/// Where in script the violation happened.
#[derive(Debug, Clone, Copy)]
pub struct SourcePosition<'a> {
    /// Script URL.
    pub file: &'a CspUrl,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
}

/// Everything known at the violation site.
#[derive(Debug, Clone, Copy)]
pub struct ViolationContext<'a> {
    /// The protected document.
    pub document_url: &'a CspUrl,
    /// Its referrer, if any.
    pub referrer: Option<&'a CspUrl>,
    /// What was blocked.
    pub blocked: BlockedResource<'a>,
    /// HTTP status of the document response (0 if none).
    pub status_code: u16,
    /// Script position, if known.
    pub position: Option<SourcePosition<'a>>,
    /// Text of the offending inline code / eval string, used only when the
    /// governing directive has `'report-sample'`.
    pub sample: Option<&'a str>,
}

/// A CSP violation record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// `document-uri`.
    pub document_url: String,
    /// `referrer` (empty if none).
    pub referrer: String,
    /// `blocked-uri`: a stripped URL, its origin, or `inline`/`eval`/`wasm-eval`.
    pub blocked_url: String,
    /// `effective-directive`.
    pub effective_directive: String,
    /// `violated-directive`: the directive in the policy that blocked.
    pub violated_directive: String,
    /// `original-policy`.
    pub original_policy: String,
    /// `enforce` or `report`.
    pub disposition: Disposition,
    /// `status-code`.
    pub status_code: u16,
    /// `source-file` (empty if unknown).
    pub source_file: String,
    /// `line-number`.
    pub line: u32,
    /// `column-number`.
    pub column: u32,
    /// `script-sample` (empty unless `'report-sample'`).
    pub sample: String,
}

/// URL as it may appear in a report: local schemes collapse to the scheme,
/// everything else loses credentials, query and fragment.
pub fn strip_url_for_report(u: &CspUrl) -> String {
    match u.scheme() {
        "data" | "blob" | "about" | "javascript" | "filesystem" => u.scheme().to_string(),
        _ => u.serialize_stripped(),
    }
}

impl Violation {
    /// Build a record for `blocked` by `policy`.
    pub fn new(policy: &Policy, blocked: Blocked, ctx: &ViolationContext<'_>) -> Violation {
        let blocked_url = match ctx.blocked {
            BlockedResource::Url(u) => strip_url_for_report(u),
            BlockedResource::UrlAfterCrossOriginRedirect(u) => match u.origin() {
                super::url::Origin::Tuple { scheme, host, port } => {
                    format!("{scheme}://{host}:{port}")
                }
                super::url::Origin::Opaque => u.scheme().to_string(),
            },
            BlockedResource::Inline => "inline".to_string(),
            BlockedResource::Eval => "eval".to_string(),
            BlockedResource::WasmEval => "wasm-eval".to_string(),
        };
        let sample = match ctx.sample {
            Some(s) if policy.wants_sample(blocked.violated_directive) => {
                s.chars().take(MAX_SAMPLE_CHARS).collect()
            }
            _ => String::new(),
        };
        let (source_file, line, column) = match ctx.position {
            Some(p) => (strip_url_for_report(p.file), p.line, p.column),
            None => (String::new(), 0, 0),
        };
        Violation {
            document_url: strip_url_for_report(ctx.document_url),
            referrer: ctx.referrer.map(strip_url_for_report).unwrap_or_default(),
            blocked_url,
            effective_directive: blocked.effective_directive.to_string(),
            violated_directive: blocked.violated_directive.to_string(),
            original_policy: policy.serialize(),
            disposition: policy.disposition(),
            status_code: ctx.status_code,
            source_file,
            line,
            column,
            sample,
        }
    }

    fn disposition_str(&self) -> &'static str {
        match self.disposition {
            Disposition::Enforce => "enforce",
            Disposition::Report => "report",
        }
    }

    /// The legacy `report-uri` JSON body.
    pub fn to_report_uri_json(&self) -> String {
        let mut o = JsonObject::new();
        o.str("document-uri", &self.document_url);
        o.str("referrer", &self.referrer);
        o.str("violated-directive", &self.violated_directive);
        o.str("effective-directive", &self.effective_directive);
        o.str("original-policy", &self.original_policy);
        o.str("disposition", self.disposition_str());
        o.str("blocked-uri", &self.blocked_url);
        o.num("status-code", u64::from(self.status_code));
        if !self.sample.is_empty() {
            o.str("script-sample", &self.sample);
        }
        if !self.source_file.is_empty() {
            o.str("source-file", &self.source_file);
            o.num("line-number", u64::from(self.line));
            o.num("column-number", u64::from(self.column));
        }
        format!("{{\"csp-report\":{}}}", o.finish())
    }

    /// The Reporting API `csp-violation` body.
    pub fn to_reporting_api_json(&self) -> String {
        let mut o = JsonObject::new();
        o.str("documentURL", &self.document_url);
        o.str("referrer", &self.referrer);
        o.str("blockedURL", &self.blocked_url);
        o.str("effectiveDirective", &self.effective_directive);
        o.str("originalPolicy", &self.original_policy);
        o.str("disposition", self.disposition_str());
        o.num("statusCode", u64::from(self.status_code));
        o.str("sample", &self.sample);
        if !self.source_file.is_empty() {
            o.str("sourceFile", &self.source_file);
            o.num("lineNumber", u64::from(self.line));
            o.num("columnNumber", u64::from(self.column));
        }
        o.finish()
    }
}

/// Append `s` as a JSON string literal: quotes, backslash, control
/// characters and U+2028/2029 escaped, so attacker-controlled text (policy,
/// sample) cannot break out of the report body.
pub fn json_escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct JsonObject {
    buf: String,
    first: bool,
}

impl JsonObject {
    fn new() -> Self {
        JsonObject {
            buf: String::from("{"),
            first: true,
        }
    }
    fn key(&mut self, k: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        json_escape_into(&mut self.buf, k);
        self.buf.push(':');
    }
    fn str(&mut self, k: &str, v: &str) {
        self.key(k);
        json_escape_into(&mut self.buf, v);
    }
    fn num(&mut self, k: &str, v: u64) {
        self.key(k);
        self.buf.push_str(&v.to_string());
    }
    fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csp::check::{RequestKind, RequestOptions, check_request};
    use crate::csp::policy::PolicySource;
    use crate::csp::url::Origin;

    fn pol(s: &str) -> Policy {
        Policy::parse(s.as_bytes(), Disposition::Enforce, PolicySource::Header).unwrap()
    }
    fn url(s: &str) -> CspUrl {
        CspUrl::parse(s).unwrap()
    }

    #[test]
    fn violation_from_a_real_block() {
        let p = pol("default-src 'self'; script-src-elem 'self' 'report-sample'");
        let doc = url("https://page.example/index.html?secret=1#frag");
        let target = url("https://evil.test/x.js?token=abc#h");
        let d = check_request(
            &p,
            RequestKind::Script,
            &target,
            &Origin::from_url_str("https://page.example/"),
            &RequestOptions::default(),
        );
        let crate::csp::check::Decision::Blocked(b) = d else {
            unreachable!("should block")
        };
        let ctx = ViolationContext {
            document_url: &doc,
            referrer: None,
            blocked: BlockedResource::Url(&target),
            status_code: 200,
            position: None,
            sample: Some("ignored for urls but harmless"),
        };
        let v = Violation::new(&p, b, &ctx);
        assert_eq!(v.document_url, "https://page.example/index.html");
        assert_eq!(v.blocked_url, "https://evil.test/x.js");
        assert_eq!(v.effective_directive, "script-src-elem");
        assert_eq!(v.violated_directive, "script-src-elem");
        assert_eq!(v.disposition, Disposition::Enforce);
        assert_eq!(v.original_policy, p.serialize());
        assert_eq!(v.status_code, 200);
    }

    #[test]
    fn blocked_uri_forms() {
        let p = pol("default-src 'none'");
        let doc = url("https://page.example/");
        let b = Blocked {
            effective_directive: "img-src",
            violated_directive: "default-src",
        };
        fn run(p: &Policy, b: Blocked, doc: &CspUrl, r: BlockedResource<'_>) -> String {
            Violation::new(
                p,
                b,
                &ViolationContext {
                    document_url: doc,
                    referrer: None,
                    blocked: r,
                    status_code: 0,
                    position: None,
                    sample: None,
                },
            )
            .blocked_url
        }
        let mk = |r: BlockedResource<'_>| run(&p, b, &doc, r);
        assert_eq!(mk(BlockedResource::Inline), "inline");
        assert_eq!(mk(BlockedResource::Eval), "eval");
        assert_eq!(mk(BlockedResource::WasmEval), "wasm-eval");
        assert_eq!(
            mk(BlockedResource::Url(&url("data:image/png;base64,AAAA"))),
            "data"
        );
        assert_eq!(
            mk(BlockedResource::Url(&url("blob:https://a.test/uuid"))),
            "blob"
        );
        assert_eq!(
            mk(BlockedResource::UrlAfterCrossOriginRedirect(&url(
                "https://a.test/secret/path?q"
            ))),
            "https://a.test:443"
        );
    }

    #[test]
    fn sample_needs_report_sample_and_is_truncated() {
        let doc = url("https://page.example/");
        let b = Blocked {
            effective_directive: "script-src-elem",
            violated_directive: "script-src",
        };
        let long = "x".repeat(100);
        let ctx = |s| ViolationContext {
            document_url: &doc,
            referrer: None,
            blocked: BlockedResource::Inline,
            status_code: 200,
            position: None,
            sample: Some(s),
        };
        let with = Violation::new(&pol("script-src 'report-sample'"), b, &ctx(&long));
        assert_eq!(with.sample.chars().count(), MAX_SAMPLE_CHARS);
        let without = Violation::new(&pol("script-src 'self'"), b, &ctx(&long));
        assert!(without.sample.is_empty());
        let multi = "\u{1F600}".repeat(60);
        let m = Violation::new(&pol("script-src 'report-sample'"), b, &ctx(&multi));
        assert_eq!(m.sample.chars().count(), MAX_SAMPLE_CHARS);
    }

    #[test]
    fn json_is_escaped_and_well_formed() {
        let p = Policy::parse(
            b"script-src 'self' \"quoted\\\" report-sample; img-src a\x01b",
            Disposition::Report,
            PolicySource::Header,
        )
        .unwrap();
        let doc = url("https://page.example/");
        let b = Blocked {
            effective_directive: "script-src-elem",
            violated_directive: "script-src",
        };
        let v = Violation::new(
            &p,
            b,
            &ViolationContext {
                document_url: &doc,
                referrer: Some(&url("https://ref.test/a")),
                blocked: BlockedResource::Inline,
                status_code: 200,
                position: Some(SourcePosition {
                    file: &doc,
                    line: 3,
                    column: 9,
                }),
                sample: None,
            },
        );
        let j = v.to_report_uri_json();
        assert!(j.starts_with("{\"csp-report\":{"));
        assert!(j.contains("\"disposition\":\"report\""));
        assert!(j.contains("\\\"quoted\\\\\\\""), "{j}");
        assert!(j.contains("\\u0001"));
        assert!(j.contains("\"line-number\":3"));
        assert!(!j.contains('\u{1}'));
        let r = v.to_reporting_api_json();
        assert!(r.contains("\"blockedURL\":\"inline\""));
        assert!(r.contains("\"lineNumber\":3"));
        // balanced braces outside strings: cheap well-formedness check
        for s in [&j, &r] {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            for c in s.chars() {
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '"' {
                        in_str = false;
                    }
                } else if c == '"' {
                    in_str = true;
                } else if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                }
            }
            assert_eq!((depth, in_str), (0, false), "{s}");
        }
    }

    #[test]
    fn escape_specials() {
        let mut s = String::new();
        json_escape_into(&mut s, "a\"b\\c\n\u{2028}\u{7f}\u{0}");
        assert_eq!(s, "\"a\\\"b\\\\c\\n\\u2028\\u007f\\u0000\"");
    }
}

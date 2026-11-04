//! Just enough JSON, std only: a parser (for `update-hsts`) and a pretty
//! printer whose output is byte-identical to Python's
//! `json.dumps(v, indent=2, sort_keys=True, ensure_ascii=False)` (for
//! `make-fetch-corpus`). Committed files are compared byte for byte, so the
//! printer's layout is a contract, not a style.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number as written; this crate never does arithmetic on JSON.
    Num(String),
    Str(String),
    Array(Vec<Value>),
    /// Insertion order kept; on a duplicate key the last one wins, as in
    /// Python's `json.loads`.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Python truthiness, for the one place a script tested `if entry.get(..)`.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => n.parse::<f64>().map(|f| f != 0.0).unwrap_or(true),
            Value::Str(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    pub fn int(n: usize) -> Value {
        Value::Num(n.to_string())
    }

    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    pub fn object(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

// ---------------------------------------------------------------------------
// Printer

fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, v: &Value, depth: usize) {
    let pad = |out: &mut String, d: usize| out.push_str(&"  ".repeat(d));
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(n),
        Value::Str(s) => escape_into(out, s),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Array(a) => {
            out.push_str("[\n");
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, depth + 1);
                write_value(out, item, depth + 1);
            }
            out.push('\n');
            pad(out, depth);
            out.push(']');
        }
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Object(m) => {
            // sort_keys=True; a later duplicate replaces an earlier one.
            let mut sorted: Vec<(&String, &Value)> = Vec::new();
            for (k, val) in m {
                match sorted.iter_mut().find(|(sk, _)| *sk == k) {
                    Some(slot) => slot.1 = val,
                    None => sorted.push((k, val)),
                }
            }
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            out.push_str("{\n");
            for (i, (k, val)) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, depth + 1);
                escape_into(out, k);
                out.push_str(": ");
                write_value(out, val, depth + 1);
            }
            out.push('\n');
            pad(out, depth);
            out.push('}');
        }
    }
}

/// `json.dumps(v, indent=2, sort_keys=True, ensure_ascii=False)`.
pub fn to_pretty(v: &Value) -> String {
    let mut s = String::new();
    write_value(&mut s, v, 0);
    s
}

// ---------------------------------------------------------------------------
// Parser

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

type R<T> = Result<T, String>;

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> R<T> {
        Err(format!("JSON: {what} at byte {}", self.i))
    }

    fn lit(&mut self, word: &str, v: Value) -> R<Value> {
        if self.b.get(self.i..self.i + word.len()) == Some(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("bad literal")
        }
    }

    fn hex4(&mut self) -> R<u32> {
        let h = self
            .b
            .get(self.i..self.i + 4)
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| u32::from_str_radix(s, 16).ok());
        match h {
            Some(v) => {
                self.i += 4;
                Ok(v)
            }
            None => self.err("bad \\u escape"),
        }
    }

    fn string(&mut self) -> R<String> {
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else {
                        return self.err("unterminated escape");
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi)
                                && self.b.get(self.i..self.i + 2) == Some(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    return self.err("bad surrogate pair");
                                }
                            } else {
                                hi
                            };
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("bad escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return self.err("control character in string"),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "JSON: invalid UTF-8 in string".to_string())
    }

    fn value(&mut self, depth: usize) -> R<Value> {
        if depth > 512 {
            return self.err("nesting too deep");
        }
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Object(m));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected a key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Object(m));
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Array(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Array(a));
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'n') => self.lit("null", Value::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                while matches!(
                    self.b.get(self.i),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.i += 1;
                }
                let s = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
                if s.parse::<f64>().is_err() {
                    return self.err("bad number");
                }
                Ok(Value::Num(s))
            }
            _ => self.err("unexpected character"),
        }
    }
}

/// Parse one JSON document; trailing non-whitespace is an error.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return p.err("trailing data");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_matches_python_layout() {
        // python3 -c 'import json;print(json.dumps({"b":[],"a":{"z":1,"y":[{"k":"v"}]},"c":{}},
        //   indent=2,sort_keys=True,ensure_ascii=False))'
        let v = Value::object(vec![
            ("b", Value::Array(vec![])),
            (
                "a",
                Value::object(vec![
                    ("z", Value::int(1)),
                    (
                        "y",
                        Value::Array(vec![Value::object(vec![("k", Value::str("v"))])]),
                    ),
                ]),
            ),
            ("c", Value::Object(vec![])),
        ]);
        let want = "{\n  \"a\": {\n    \"y\": [\n      {\n        \"k\": \"v\"\n      }\n    ],\n    \"z\": 1\n  },\n  \"b\": [],\n  \"c\": {}\n}";
        assert_eq!(to_pretty(&v), want);
    }

    #[test]
    fn printer_escapes_like_python_without_ascii_forcing() {
        let v = Value::str("a\"b\\c\n\t\u{1}\u{7f}é🙂");
        assert_eq!(to_pretty(&v), "\"a\\\"b\\\\c\\n\\t\\u0001\u{7f}é🙂\"");
    }

    #[test]
    fn parses_what_it_prints() {
        let src = r#"{"entries":[{"name":"a.example","mode":"force-https","include_subdomains":true},
            {"name":"\u00e9\ud83d\ude42","n":-1.5e3}],"x":null}"#;
        let v = parse(src).unwrap();
        let e = v.get("entries").unwrap();
        let Value::Array(a) = e else { panic!() };
        assert_eq!(a.len(), 2);
        assert_eq!(a[1].get("name"), Some(&Value::str("é🙂")));
        assert_eq!(to_pretty(&parse(&to_pretty(&v)).unwrap()), to_pretty(&v));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "\"x",
            "tru",
            "{} x",
            "[\"\\q\"]",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn truthiness_and_duplicate_keys() {
        let v = parse(r#"{"a":1,"a":0,"s":"","t":"x","n":null}"#).unwrap();
        assert!(!v.get("a").unwrap().truthy());
        assert!(!v.get("s").unwrap().truthy());
        assert!(v.get("t").unwrap().truthy());
        assert!(!v.get("n").unwrap().truthy());
    }
}

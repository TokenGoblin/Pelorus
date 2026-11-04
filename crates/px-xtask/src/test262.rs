//! The engine-independent half of the test262 work (Phase 10, unit 5).
//!
//! * [`IndexEntry`]: one line of `tests/test262/INDEX`, written by
//!   `cargo xtask vendor-test262` and read by the runner, so that nothing at
//!   test time needs a YAML parser.
//!
//! Nothing here knows about any JavaScript engine.

/// The phase in which a negative test must fail (test262 `negative.phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// The source text must be rejected by the parser.
    Parse,
    /// An early error: rejected before any of the script runs.
    Early,
    /// Module resolution (no module tests are vendored; kept for the format).
    Resolution,
    /// An exception thrown while the script runs.
    Runtime,
}

impl Phase {
    pub fn parse(s: &str) -> Option<Phase> {
        match s {
            "parse" => Some(Phase::Parse),
            "early" => Some(Phase::Early),
            "resolution" => Some(Phase::Resolution),
            "runtime" => Some(Phase::Runtime),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Parse => "parse",
            Phase::Early => "early",
            Phase::Resolution => "resolution",
            Phase::Runtime => "runtime",
        }
    }
}

/// `negative: { phase, type }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negative {
    pub phase: Phase,
    /// The expected error constructor name, e.g. `SyntaxError`.
    pub kind: String,
}

/// One vendored test and the metadata the runner needs about it.
///
/// Line format, tab separated, one test per line, sorted by path:
///
/// ```text
/// <path>\t<flags>\t<negative>\t<includes>\t<features>
/// ```
///
/// `path` is relative to `tests/test262/test/`. `flags`, `includes` and
/// `features` are comma separated; `negative` is `<phase>:<Type>`. An empty
/// field is `-`. Lines starting with `#` and blank lines are comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: String,
    pub flags: Vec<String>,
    pub negative: Option<Negative>,
    pub includes: Vec<String>,
    pub features: Vec<String>,
}

fn join_field(v: &[String]) -> String {
    if v.is_empty() {
        "-".to_string()
    } else {
        v.join(",")
    }
}

fn split_field(s: &str) -> Vec<String> {
    if s == "-" || s.is_empty() {
        Vec::new()
    } else {
        s.split(',').map(str::to_string).collect()
    }
}

impl IndexEntry {
    pub fn to_line(&self) -> String {
        let neg = match &self.negative {
            Some(n) => format!("{}:{}", n.phase.as_str(), n.kind),
            None => "-".to_string(),
        };
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.path,
            join_field(&self.flags),
            neg,
            join_field(&self.includes),
            join_field(&self.features)
        )
    }

    /// `Ok(None)` for a comment or blank line.
    pub fn parse_line(line: &str) -> Result<Option<IndexEntry>, String> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') {
            return Ok(None);
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [path, flags, neg, includes, features] = f.as_slice() else {
            return Err(format!("INDEX line needs 5 fields, got {}: {line}", f.len()));
        };
        if path.is_empty() {
            return Err("INDEX line with an empty path".to_string());
        }
        let negative = if *neg == "-" {
            None
        } else {
            let (p, k) = neg
                .split_once(':')
                .ok_or_else(|| format!("bad negative field `{neg}` for {path}"))?;
            let phase =
                Phase::parse(p).ok_or_else(|| format!("unknown negative phase `{p}` for {path}"))?;
            if k.is_empty() {
                return Err(format!("negative field without a type for {path}"));
            }
            Some(Negative {
                phase,
                kind: k.to_string(),
            })
        };
        Ok(Some(IndexEntry {
            path: (*path).to_string(),
            flags: split_field(flags),
            negative,
            includes: split_field(includes),
            features: split_field(features),
        }))
    }

    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
}

/// Parse a whole INDEX file. Fails on the first malformed line, and on a path
/// listed twice (an index that lists a test twice would count it twice).
pub fn parse_index(text: &str) -> Result<Vec<IndexEntry>, String> {
    let mut out: Vec<IndexEntry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (n, l) in text.lines().enumerate() {
        match IndexEntry::parse_line(l).map_err(|e| format!("INDEX line {}: {e}", n + 1))? {
            Some(e) => {
                if !seen.insert(e.path.clone()) {
                    return Err(format!("INDEX line {}: duplicate path {}", n + 1, e.path));
                }
                out.push(e);
            }
            None => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> IndexEntry {
        IndexEntry {
            path: "language/a/b.js".into(),
            flags: vec!["onlyStrict".into(), "async".into()],
            negative: Some(Negative {
                phase: Phase::Early,
                kind: "SyntaxError".into(),
            }),
            includes: vec!["compareArray.js".into(), "propertyHelper.js".into()],
            features: vec!["Symbol".into()],
        }
    }

    #[test]
    fn index_line_round_trips() {
        let e = entry();
        assert_eq!(IndexEntry::parse_line(&e.to_line()).unwrap(), Some(e));
        let bare = IndexEntry {
            path: "x.js".into(),
            flags: vec![],
            negative: None,
            includes: vec![],
            features: vec![],
        };
        assert_eq!(bare.to_line(), "x.js\t-\t-\t-\t-");
        assert_eq!(IndexEntry::parse_line(&bare.to_line()).unwrap(), Some(bare));
    }

    #[test]
    fn index_comments_and_errors() {
        assert_eq!(IndexEntry::parse_line("# header").unwrap(), None);
        assert_eq!(IndexEntry::parse_line("").unwrap(), None);
        assert!(IndexEntry::parse_line("a.js\t-\t-").is_err());
        assert!(IndexEntry::parse_line("a.js\t-\tlater:Error\t-\t-").is_err());
        assert!(IndexEntry::parse_line("a.js\t-\tparse:\t-\t-").is_err());
        assert!(IndexEntry::parse_line("a.js\t-\tparse\t-\t-").is_err());
        let two = "a.js\t-\t-\t-\t-\na.js\t-\t-\t-\t-\n";
        assert!(parse_index(two).unwrap_err().contains("duplicate"));
        let ok = "# h\na.js\t-\t-\t-\t-\nb.js\tnoStrict\t-\t-\t-\n";
        assert_eq!(parse_index(ok).unwrap().len(), 2);
    }
}

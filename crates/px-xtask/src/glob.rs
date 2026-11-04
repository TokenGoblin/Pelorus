//! Shell-style glob matching (`*` `?` `[set]`, backslash escape) where `*`
//! also crosses `/`, which is what `[[ $path == $pattern ]]` did in the
//! scripts this replaces.

pub fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

fn go(p: &[char], t: &[char]) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some(('*', rest)) => (0..=t.len()).any(|i| go(rest, t.get(i..).unwrap_or(&[]))),
        Some(('?', rest)) => t.split_first().is_some_and(|(_, tr)| go(rest, tr)),
        Some(('\\', rest)) => match (rest.split_first(), t.split_first()) {
            (Some((pc, pr)), Some((tc, tr))) => pc == tc && go(pr, tr),
            _ => false,
        },
        Some(('[', rest)) => {
            let Some((c, tr)) = t.split_first() else {
                return false;
            };
            match parse_set(rest) {
                Some((set, after)) => set.contains(*c) && go(after, tr),
                // No closing bracket: a literal `[`.
                None => *c == '[' && go(rest, tr),
            }
        }
        Some((pc, rest)) => t
            .split_first()
            .is_some_and(|(tc, tr)| pc == tc && go(rest, tr)),
    }
}

struct Set {
    negate: bool,
    items: Vec<(char, char)>,
}

impl Set {
    fn contains(&self, c: char) -> bool {
        self.items.iter().any(|(lo, hi)| *lo <= c && c <= *hi) != self.negate
    }
}

/// Parse the body after `[`; returns the set and the pattern after `]`.
fn parse_set(p: &[char]) -> Option<(Set, &[char])> {
    let mut i = 0;
    let negate = matches!(p.first(), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == ']' && !first {
            return Some((Set { negate, items }, p.get(i + 1..)?));
        }
        first = false;
        if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|e| *e != ']') {
            items.push((c, *p.get(i + 2)?));
            i += 3;
        } else {
            items.push((c, c));
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn star_crosses_slash() {
        assert!(matches("crates/*/CLAUDE.md", "crates/px-a/CLAUDE.md"));
        assert!(matches("*.md", "docs/adr/x.md"));
        assert!(!matches("crates/*/CLAUDE.md", "crates/px-a/src/x.rs"));
    }

    #[test]
    fn literal_and_question() {
        assert!(matches("README.md", "README.md"));
        assert!(!matches("README.md", "README.mdx"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
    }

    #[test]
    fn sets() {
        assert!(matches("f[0-9].txt", "f7.txt"));
        assert!(!matches("f[0-9].txt", "fx.txt"));
        assert!(matches("f[!0-9].txt", "fx.txt"));
        assert!(matches("a[]]b", "a]b"));
        assert!(matches("a[b", "a[b"));
    }

    #[test]
    fn escape() {
        assert!(matches("a\\*b", "a*b"));
        assert!(!matches("a\\*b", "axb"));
    }

    #[test]
    fn empty() {
        assert!(matches("", ""));
        assert!(matches("*", ""));
        assert!(!matches("?", ""));
    }
}

//! Name / argument patterns.
//!
//! A pattern string is either
//! * a **glob** (default): `*` matches any sequence, `?` matches one character,
//!   everything else is literal. A glob without wildcards is an exact match.
//!   Every wildcard becomes a numbered capture group (`$1`, `$2`, ...).
//! * a **regular expression** when prefixed with `re:`, e.g. `re:^REALM_(\w+)$`.
//!   Regexes are *not* anchored automatically.

use regex::{Captures, Regex, RegexBuilder};

#[derive(Debug, Clone)]
pub struct Pattern {
    re: Regex,
}

impl Pattern {
    pub fn new(source: &str, case_insensitive: bool) -> Result<Self, String> {
        let body = match source.strip_prefix("re:") {
            Some(re) => re.to_string(),
            None => glob_to_regex(source),
        };
        let re = RegexBuilder::new(&body)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| format!("invalid pattern {source:?}: {e}"))?;
        Ok(Self { re })
    }

    pub fn is_match(&self, s: &str) -> bool {
        self.re.is_match(s)
    }

    pub fn captures<'a>(&self, s: &'a str) -> Option<Captures<'a>> {
        self.re.captures(s)
    }
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("(?s)^");
    let mut buf = [0u8; 4];
    for ch in glob.chars() {
        match ch {
            '*' => out.push_str("(.*)"),
            '?' => out.push_str("(.)"),
            c => out.push_str(&regex::escape(c.encode_utf8(&mut buf))),
        }
    }
    out.push('$');
    out
}

/// Expand a replacement template (`$1`, `${1}`, `${name}`, `$$`) with captures.
pub fn expand(caps: &Captures<'_>, template: &str) -> String {
    let mut out = String::new();
    caps.expand(template, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact_and_wildcards() {
        let p = Pattern::new("REALM_NAME_*", false).unwrap();
        assert!(p.is_match("REALM_NAME_A"));
        assert!(p.is_match("REALM_NAME_"));
        assert!(!p.is_match("XREALM_NAME_A"));
        let c = p.captures("REALM_NAME_PROD").unwrap();
        assert_eq!(expand(&c, "NEW_${1}"), "NEW_PROD");

        let exact = Pattern::new("--log.level", false).unwrap();
        assert!(exact.is_match("--log.level"));
        assert!(!exact.is_match("--logXlevel"));
    }

    #[test]
    fn regex_prefix() {
        let p = Pattern::new(r"re:^--(debug|verbose)$", false).unwrap();
        assert!(p.is_match("--debug"));
        assert!(!p.is_match("--debugger"));
    }

    #[test]
    fn case_insensitive() {
        let p = Pattern::new("path", true).unwrap();
        assert!(p.is_match("PATH"));
    }
}

//! Boolean search expressions: `a AND b`, `a OR b`, `NOT c`.
//!
//! Operators are the upper-case words `AND`, `OR`, `NOT` with a plain space (or the start / end
//! of the query) on both sides; in regex mode an escaped space (`\ `) does not count. The text
//! between operators is one term, spaces included. Precedence: NOT > AND > OR, so
//! `a AND NOT b OR c` is `(a AND (NOT b)) OR c`. A query without operators is searched as is.

use crate::config::{Case, Mode, Scope};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// One AND group: every positive term must be present, no negated one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub terms: Vec<String>,
    pub negated: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Term(String),
    And,
    Or,
    Not,
}

/// Parses `query` into OR-ed AND groups. `None` when it has no operators (or only operators).
pub fn parse(query: &str, mode: Mode) -> Result<Option<Vec<Group>>, String> {
    let tokens = tokenize(query, mode);
    if !tokens.iter().any(|t| matches!(t, Token::And | Token::Or | Token::Not)) {
        return Ok(None);
    }
    let mut groups = Vec::new();
    let mut cur = Group { terms: vec![], negated: vec![] };
    let mut not = false;
    for t in tokens {
        match t {
            Token::Term(s) if not => {
                cur.negated.push(s);
                not = false;
            }
            Token::Term(s) => cur.terms.push(s),
            Token::Not => not = true,
            Token::And => not = false,
            Token::Or => {
                not = false;
                groups.push(std::mem::replace(&mut cur, Group { terms: vec![], negated: vec![] }));
            }
        }
    }
    groups.push(cur);
    groups.retain(|g| !g.terms.is_empty() || !g.negated.is_empty());
    if groups.is_empty() {
        return Ok(None);
    }
    if groups.iter().any(|g| g.terms.is_empty()) {
        return Err("NOT needs a term to search for next to it (e.g. foo AND NOT bar)".into());
    }
    Ok(Some(groups))
}

fn tokenize(query: &str, mode: Mode) -> Vec<Token> {
    let b = query.as_bytes();
    // A space that separates words; in regex mode `\ ` is a literal space.
    let plain_space = |i: usize| {
        b[i] == b' ' && (mode == Mode::Literal || b[..i].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 0)
    };
    let mut tokens = Vec::new();
    let mut seg_start = 0;
    let mut i = 0;
    while i < b.len() {
        let op = [("AND", Token::And), ("OR", Token::Or), ("NOT", Token::Not)].into_iter().find(|(w, _)| {
            let end = i + w.len();
            b[i..].starts_with(w.as_bytes())
                && (i == 0 || plain_space(i - 1))
                && (end == b.len() || b[end] == b' ')
        });
        if let Some((w, tok)) = op {
            push_term(&mut tokens, &query[seg_start..i], mode);
            tokens.push(tok);
            i += w.len();
            seg_start = i;
        } else {
            i += 1;
        }
    }
    push_term(&mut tokens, &query[seg_start..], mode);
    tokens
}

fn push_term(tokens: &mut Vec<Token>, seg: &str, mode: Mode) {
    let mut t = seg.trim_matches(' ').to_string();
    // Keep a trailing space that a backslash escapes (`foo\ `).
    if mode == Mode::Regex && t.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1 && seg.trim_start_matches(' ').len() > t.len() {
        t.push(' ');
    }
    if !t.is_empty() {
        tokens.push(Token::Term(t));
    }
}

/// Evaluates an expression on lines or whole files. Terms are indexed: bit `i` of a mask
/// means term `i` occurs.
#[derive(Debug)]
pub struct Filter {
    pub scope: Scope,
    terms: Vec<RegexMatcher>,
    /// Per group: (positive terms, negated terms).
    groups: Vec<(u64, u64)>,
    /// Union of all positive terms.
    pub positive: u64,
    /// Matches any term, negated ones included (file scope must see those lines too).
    pub scan: RegexMatcher,
}

impl Filter {
    /// Terms occurring in `line`.
    pub fn mask(&self, line: &[u8]) -> u64 {
        self.terms
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_match(line).unwrap_or(false))
            .fold(0, |acc, (i, _)| acc | 1 << i)
    }

    /// Positive terms of the groups that `mask` satisfies; 0 when none is.
    pub fn satisfied(&self, mask: u64) -> u64 {
        self.groups
            .iter()
            .filter(|(pos, neg)| mask & pos == *pos && mask & neg == 0)
            .fold(0, |acc, (pos, _)| acc | pos)
    }

    /// No group can be satisfied any more once `mask` has been seen (file scope).
    pub fn hopeless(&self, mask: u64) -> bool {
        self.groups.iter().all(|(_, neg)| mask & neg != 0)
    }
}

/// Builds the matcher for a query: the highlight matcher (positive terms) and, for a query
/// with operators, the filter that decides which lines / files count.
pub fn compile(query: &str, mode: Mode, case: Case, scope: Scope) -> Result<(RegexMatcher, Option<Filter>), String> {
    let Some(groups) = parse(query, mode)? else {
        return crate::search::build_matcher(query, mode, case).map(|m| (m, None)).map_err(|e| format!("regex error: {e}"));
    };
    let mut names: Vec<(String, bool)> = Vec::new(); // (pattern, negated-only)
    let mut index = |s: &String| match names.iter().position(|(n, _)| n == s) {
        Some(i) => i,
        None => {
            names.push((s.clone(), true));
            names.len() - 1
        }
    };
    let mut bits = Vec::new();
    for g in &groups {
        let pos = g.terms.iter().map(&mut index).fold(0u64, |a, i| a | 1u64.checked_shl(i as u32).unwrap_or(0));
        let neg = g.negated.iter().map(&mut index).fold(0u64, |a, i| a | 1u64.checked_shl(i as u32).unwrap_or(0));
        bits.push((pos, neg));
    }
    if names.len() > 64 {
        return Err("too many terms (at most 64)".into());
    }
    for (pos, _) in &bits {
        for (i, n) in names.iter_mut().enumerate() {
            if pos & (1 << i) != 0 {
                n.1 = false;
            }
        }
    }
    let regexes: Vec<String> = names.iter().map(|(n, _)| term_regex(n, mode, case)).collect();
    let build = |p: &str| {
        RegexMatcherBuilder::new()
            .build(p)
            .map_err(|e| format!("regex error: {}", e.to_string().split_whitespace().collect::<Vec<_>>().join(" ")))
    };
    let terms = regexes.iter().map(|r| build(r)).collect::<Result<Vec<_>, _>>()?;
    let positive = bits.iter().fold(0, |a, (p, _)| a | p);
    let join = |only_pos: bool| {
        regexes
            .iter()
            .enumerate()
            .filter(|(i, _)| !only_pos || positive & (1 << i) != 0)
            .map(|(_, r)| r.as_str())
            .collect::<Vec<_>>()
            .join("|")
    };
    let highlight = build(&join(true))?;
    let scan = build(&join(false))?;
    Ok((highlight, Some(Filter { scope, terms, groups: bits, positive, scan })))
}

/// Regex for one term with its own case rule (smart case looks at each term separately).
fn term_regex(term: &str, mode: Mode, case: Case) -> String {
    let body = match mode {
        Mode::Literal => escape(term),
        Mode::Regex => term.to_string(),
    };
    let insensitive = match case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !has_upper(term, mode),
    };
    format!("(?{}:{body})", if insensitive { "i" } else { "-i" })
}

/// Upper-case letters, ignoring regex escapes like `\S` or `\W`.
fn has_upper(term: &str, mode: Mode) -> bool {
    let mut esc = false;
    term.chars().any(|c| {
        let upper = !esc && c.is_uppercase();
        esc = mode == Mode::Regex && !esc && c == '\\';
        upper
    })
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(terms: &[&str], negated: &[&str]) -> Group {
        Group {
            terms: terms.iter().map(|s| s.to_string()).collect(),
            negated: negated.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_operators_with_precedence() {
        let p = |q| parse(q, Mode::Literal).unwrap();
        assert_eq!(p("foo bar"), None);
        assert_eq!(p("ANDROID OR_X xAND FOO_AND_BAR"), None, "only whole words are operators");
        assert_eq!(p("AND"), None, "only an operator: searched as text");
        assert_eq!(p("Error AND Stack"), Some(vec![g(&["Error", "Stack"], &[])]));
        assert_eq!(
            p("Error AND NOT Stack OR Warn"),
            Some(vec![g(&["Error"], &["Stack"]), g(&["Warn"], &[])])
        );
        assert_eq!(p("user name AND id"), Some(vec![g(&["user name", "id"], &[])]), "terms keep inner spaces");
        assert_eq!(p("foo AND "), Some(vec![g(&["foo"], &[])]), "dangling operator while typing");
        assert_eq!(p("foo NOT bar"), Some(vec![g(&["foo"], &["bar"])]));
        assert!(parse("NOT foo", Mode::Literal).is_err());
        assert!(parse("a OR NOT b", Mode::Literal).is_err());
    }

    #[test]
    fn escaped_space_is_not_a_separator_in_regex_mode() {
        assert_eq!(parse(r"foo\ AND\ bar", Mode::Regex).unwrap(), None);
        assert_eq!(parse(r"foo\ AND bar", Mode::Regex).unwrap(), None, "space before AND is escaped");
        assert_eq!(parse(r"a\\ AND b", Mode::Regex).unwrap(), Some(vec![g(&[r"a\\", "b"], &[])]), "escaped backslash");
        assert_eq!(parse(r"foo\  AND b", Mode::Regex).unwrap(), Some(vec![g(&[r"foo\ ", "b"], &[])]));
        assert_eq!(
            parse(r"foo\ AND bar", Mode::Literal).unwrap(),
            Some(vec![g(&[r"foo\", "bar"], &[])]),
            "in literal mode a backslash is just text"
        );
    }

    #[test]
    fn filter_evaluates_lines_and_files() {
        let (hl, f) = compile("Error AND NOT Stack OR warn", Mode::Literal, Case::Smart, Scope::Line).unwrap();
        let f = f.unwrap();
        let ok = |l: &str| f.satisfied(f.mask(l.as_bytes())) != 0;
        assert!(ok("Error here"));
        assert!(!ok("Error in Stack"));
        assert!(ok("WARN: x"), "lower-case term is case-insensitive under smart case");
        assert!(!ok("error here"), "upper-case term is case-sensitive");
        assert!(hl.is_match(b"Error").unwrap() && !hl.is_match(b"Stack").unwrap(), "negated terms are not highlighted");
        assert!(f.scan.is_match(b"Stack").unwrap());
        assert!(!f.hopeless(f.mask(b"Stack")), "warn group is still possible");
    }

    #[test]
    fn literal_terms_are_escaped() {
        let (_, f) = compile("a.b AND (c)", Mode::Literal, Case::Smart, Scope::Line).unwrap();
        let f = f.unwrap();
        assert_eq!(f.satisfied(f.mask(b"a.b (c)")), 0b11);
        assert_eq!(f.satisfied(f.mask(b"axb (c)")), 0);
    }
}

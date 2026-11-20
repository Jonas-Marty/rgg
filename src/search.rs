//! Search backend built on ripgrep's crates (`ignore` + `grep-searcher` + `grep-regex`).

use crate::config::{Case, Mode};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::Lossy};
use ignore::types::{Types, TypesBuilder};
use ignore::{WalkBuilder, WalkState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum bytes of line text kept for list display.
const MAX_TEXT: usize = 240;
/// Maximum occurrences kept per file; files with more are marked `truncated`.
pub const MAX_PER_FILE: usize = 5_000;

/// Display text of a matching line, shared by all occurrences on that line.
#[derive(Debug)]
pub struct LineText {
    /// Line text with leading whitespace trimmed, cut to `MAX_TEXT` bytes.
    pub text: String,
    /// Byte ranges of the occurrences that are visible within `text`.
    pub ranges: Vec<(usize, usize)>,
}

/// One occurrence of the pattern.
#[derive(Debug, Clone)]
pub struct Occurrence {
    /// 1-based line number.
    pub line: u64,
    /// 1-based column in UTF-16 code units (what VS Code expects).
    pub col: usize,
    /// Byte range within the full line.
    pub start: usize,
    pub end: usize,
    pub text: Arc<LineText>,
    /// Index into `text.ranges` if this occurrence is visible in the display text.
    pub nth: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct FileResult {
    /// Path relative to the search root.
    pub rel: PathBuf,
    /// Repository the file belongs to: nearest ancestor containing `.git`,
    /// or the search root when there is none (same rule as folder settings).
    pub repo: Arc<Path>,
    pub matches: Vec<Occurrence>,
    /// More than `MAX_PER_FILE` occurrences exist; only the first ones are kept.
    pub truncated: bool,
}

pub enum Msg {
    File { generation: u64, file: FileResult },
    Done { generation: u64, elapsed: Duration, limit_reached: bool },
}

#[derive(Debug, Clone)]
pub struct Params {
    pub root: PathBuf,
    pub hidden: bool,
    pub no_ignore: bool,
    pub max_filesize: u64,
    pub max_files: usize,
}

pub fn build_matcher(query: &str, mode: Mode, case: Case) -> Result<RegexMatcher, String> {
    let mut b = RegexMatcherBuilder::new();
    b.fixed_strings(mode == Mode::Literal);
    match case {
        Case::Smart => b.case_smart(true),
        Case::Insensitive => b.case_insensitive(true),
        Case::Sensitive => b.case_insensitive(false),
    };
    b.build(query).map_err(|e| one_line(&e.to_string()))
}

pub fn build_types(selected: &[String]) -> Result<Types, String> {
    let mut b = TypesBuilder::new();
    b.add_defaults();
    for t in selected {
        b.select(t);
    }
    b.build().map_err(|e| one_line(&e.to_string()))
}

/// All built-in ripgrep types as (name, globs joined).
pub fn type_definitions() -> Vec<(String, String)> {
    let mut b = TypesBuilder::new();
    b.add_defaults();
    b.definitions()
        .into_iter()
        .map(|d| (d.name().to_string(), d.globs().join(", ")))
        .collect()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs a search on a background thread. Results stream through `tx`; setting `cancel` stops it.
pub fn spawn(
    params: Params,
    matcher: RegexMatcher,
    types: Types,
    generation: u64,
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut wb = WalkBuilder::new(&params.root);
        wb.hidden(!params.hidden)
            .max_filesize(Some(params.max_filesize))
            .types(types);
        if params.no_ignore {
            wb.ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
        }
        let found = Arc::new(AtomicUsize::new(0));
        let limit = Arc::new(AtomicBool::new(false));
        wb.build_parallel().run(|| {
            let matcher = matcher.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            let found = found.clone();
            let limit = limit.clone();
            let root = params.root.clone();
            let fallback: Arc<Path> = Arc::from(params.root.as_path());
            let mut repos: HashMap<PathBuf, Arc<Path>> = HashMap::new();
            let max_files = params.max_files;
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(b'\x00'))
                .line_number(true)
                .build();
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) || limit.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else { return WalkState::Continue };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let mut matches = Vec::new();
                let mut truncated = false;
                let _ = searcher.search_path(
                    &matcher,
                    entry.path(),
                    Lossy(|lnum, line| {
                        matches.extend(occurrences(&matcher, lnum, line));
                        if matches.len() > MAX_PER_FILE {
                            matches.truncate(MAX_PER_FILE);
                            truncated = true;
                            return Ok(false);
                        }
                        Ok(!cancel.load(Ordering::Relaxed))
                    }),
                );
                if matches.is_empty() || cancel.load(Ordering::Relaxed) {
                    return WalkState::Continue;
                }
                if found.fetch_add(1, Ordering::Relaxed) >= max_files {
                    limit.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap_or(entry.path())
                    .to_path_buf();
                let dir = entry.path().parent().unwrap_or(&root);
                let repo = repo_of(dir, &mut repos, &fallback);
                let file = FileResult { rel, repo, matches, truncated };
                if tx.send(Msg::File { generation, file }).is_err() {
                    return WalkState::Quit;
                }
                WalkState::Continue
            })
        });
        if !cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Msg::Done {
                generation,
                elapsed: start.elapsed(),
                limit_reached: limit.load(Ordering::Relaxed),
            });
        }
    });
}

/// Nearest ancestor of `dir` (inclusive) containing `.git`, else `fallback`.
/// Every directory visited is cached, so each is checked at most once per worker.
pub fn repo_of(dir: &Path, cache: &mut HashMap<PathBuf, Arc<Path>>, fallback: &Arc<Path>) -> Arc<Path> {
    let mut visited = Vec::new();
    let mut found = None;
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if let Some(r) = cache.get(d) {
            found = Some(r.clone());
            break;
        }
        visited.push(d.to_path_buf());
        if d.join(".git").exists() {
            found = Some(Arc::from(d));
            break;
        }
        cur = d.parent();
    }
    let repo = found.unwrap_or_else(|| fallback.clone());
    for d in visited {
        cache.insert(d, repo.clone());
    }
    repo
}

/// All non-empty occurrences on one line.
pub fn occurrences(matcher: &RegexMatcher, lnum: u64, line: &str) -> Vec<Occurrence> {
    let line = line.trim_end_matches(['\n', '\r']);
    let mut full = Vec::new();
    let _ = matcher.find_iter(line.as_bytes(), |m| {
        if m.start() < m.end() {
            full.push((m.start(), m.end()));
        }
        true
    });
    if full.is_empty() {
        return Vec::new();
    }
    let trimmed = line.trim_start();
    let shift = line.len() - trimmed.len();
    let cut = floor_char(trimmed, MAX_TEXT);
    let text = trimmed[..cut].to_string();
    let mut ranges = Vec::new();
    let mut nths = Vec::with_capacity(full.len());
    for &(s, e) in &full {
        let ds = floor_char(&text, s.saturating_sub(shift).min(cut));
        let de = floor_char(&text, e.saturating_sub(shift).min(cut));
        if ds < de {
            nths.push(Some(ranges.len()));
            ranges.push((ds, de));
        } else {
            nths.push(None);
        }
    }
    let shared = Arc::new(LineText { text, ranges });
    full.into_iter()
        .zip(nths)
        .map(|((start, end), nth)| Occurrence {
            line: lnum,
            col: utf16_col(line, start),
            start,
            end,
            text: shared.clone(),
            nth,
        })
        .collect()
}

/// 1-based UTF-16 column of byte offset `byte` in `line`.
pub fn utf16_col(line: &str, byte: usize) -> usize {
    line[..floor_char(line, byte)].encode_utf16().count() + 1
}

/// Largest char boundary <= i.
pub fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Fresh, empty temp directory for tests.
#[cfg(test)]
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn run(root: &std::path::Path, q: &str, mode: Mode, types: &[String]) -> Vec<FileResult> {
        let (tx, rx) = mpsc::channel();
        let params = Params {
            root: root.to_path_buf(),
            hidden: false,
            no_ignore: false,
            max_filesize: 10 << 20,
            max_files: 100,
        };
        spawn(
            params,
            build_matcher(q, mode, Case::Smart).unwrap(),
            build_types(types).unwrap(),
            1,
            tx,
            Arc::new(AtomicBool::new(false)),
        );
        let mut out = Vec::new();
        for msg in rx {
            match msg {
                Msg::File { file, .. } => out.push(file),
                Msg::Done { .. } => break,
            }
        }
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        out
    }

    #[test]
    fn finds_literal_regex_and_filters_types() {
        let dir = temp_dir("search");
        std::fs::write(dir.join("a.cs"), "class Foo {}\n  var x = foo.bar;\n").unwrap();
        std::fs::write(dir.join("b.py"), "foo = 1\n").unwrap();

        let all = run(&dir, "foo", Mode::Literal, &[]);
        assert_eq!(all.len(), 2);
        let cs = &all[0];
        assert_eq!(cs.matches.len(), 2, "smart case matches Foo and foo");
        let m = &cs.matches[1];
        assert_eq!((m.line, m.col, m.start, m.end), (2, 11, 10, 13));
        assert_eq!(m.text.text, "var x = foo.bar;");
        assert_eq!(m.text.ranges, vec![(8, 11)]);
        assert!(!cs.truncated);

        assert_eq!(run(&dir, "foo", Mode::Literal, &["cs".into()]).len(), 1);
        assert_eq!(run(&dir, "o.b", Mode::Literal, &[]).len(), 1);
        assert_eq!(run(&dir, r"fo+\b", Mode::Regex, &[]).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_occurrence_on_a_line_is_a_match() {
        let m = build_matcher("foo", Mode::Literal, Case::Smart).unwrap();
        let occ = occurrences(&m, 1, "foo foo\n");
        assert_eq!(occ.len(), 2);
        assert_eq!((occ[0].col, occ[0].start, occ[0].nth), (1, 0, Some(0)));
        assert_eq!((occ[1].col, occ[1].start, occ[1].nth), (5, 4, Some(1)));
        assert!(Arc::ptr_eq(&occ[0].text, &occ[1].text), "line text is shared");
    }

    #[test]
    fn columns_are_utf16_for_the_editor() {
        let m = build_matcher("foo", Mode::Literal, Case::Smart).unwrap();
        let occ = occurrences(&m, 1, "😀foo");
        assert_eq!(occ[0].col, 3);
        assert_eq!(utf16_col("äfoo", 2), 2);
    }

    #[test]
    fn files_over_the_cap_are_marked_truncated() {
        let dir = temp_dir("cap");
        std::fs::write(dir.join("many.txt"), "foo\n".repeat(MAX_PER_FILE + 1)).unwrap();
        std::fs::write(dir.join("exact.txt"), "foo\n".repeat(MAX_PER_FILE)).unwrap();
        let res = run(&dir, "foo", Mode::Literal, &[]);
        let exact = res.iter().find(|f| f.rel.ends_with("exact.txt")).unwrap();
        let many = res.iter().find(|f| f.rel.ends_with("many.txt")).unwrap();
        assert_eq!((exact.matches.len(), exact.truncated), (MAX_PER_FILE, false));
        assert_eq!((many.matches.len(), many.truncated), (MAX_PER_FILE, true));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_know_their_repository() {
        let dir = temp_dir("repos");
        for sub in ["alpha", "beta", "beta/nested"] {
            std::fs::create_dir_all(dir.join(sub).join(".git")).unwrap();
        }
        std::fs::create_dir_all(dir.join("alpha/src")).unwrap();
        std::fs::create_dir_all(dir.join("loose")).unwrap();
        std::fs::write(dir.join("alpha/src/a.cs"), "foo").unwrap();
        std::fs::write(dir.join("beta/b.cs"), "foo").unwrap();
        std::fs::write(dir.join("beta/nested/n.cs"), "foo").unwrap();
        std::fs::write(dir.join("loose/l.cs"), "foo").unwrap();
        let res = run(&dir, "foo", Mode::Literal, &[]);
        let repo = |name: &str| res.iter().find(|f| f.rel.ends_with(name)).unwrap().repo.to_path_buf();
        assert_eq!(repo("a.cs"), dir.join("alpha"));
        assert_eq!(repo("b.cs"), dir.join("beta"));
        assert_eq!(repo("n.cs"), dir.join("beta/nested"));
        // No repository above the temp dir: falls back to the search root.
        assert_eq!(repo("l.cs"), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_regex_and_type_report_errors() {
        assert!(build_matcher("foo(", Mode::Regex, Case::Smart).is_err());
        assert!(build_matcher("foo(", Mode::Literal, Case::Smart).is_ok());
        assert!(build_types(&["nope-not-a-type".into()]).is_err());
        assert!(type_definitions().iter().any(|(n, _)| n == "cs"));
    }
}

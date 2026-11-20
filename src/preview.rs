//! File preview: decoding, incremental syntax highlighting, match highlighting and wrapping.

use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::search::floor_char;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

/// Lines longer than this stop highlighting for the rest of the file.
const MAX_HL_LINE: usize = 4_000;
/// Highlight at most this many lines per file; the rest is shown plain.
const MAX_HL_LINES: usize = 50_000;
/// Highlighting time spent synchronously while rendering a frame.
const RENDER_BUDGET: Duration = Duration::from_millis(4);

pub const MATCH_BG: Color = Color::Rgb(90, 75, 20);
pub const CURRENT_BG: Color = Color::Rgb(235, 185, 40);

pub fn match_style() -> Style {
    Style::default().bg(MATCH_BG)
}

pub fn current_style() -> Style {
    Style::default().bg(CURRENT_BG).fg(Color::Black)
}

/// Loads syntaxes and themes on a background thread so startup stays fast.
pub fn warm_up() {
    std::thread::spawn(init_blocking);
}

pub fn init_blocking() {
    THEMES.get_or_init(ThemeSet::load_defaults);
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
}

pub fn theme_names() -> Vec<String> {
    THEMES.get_or_init(ThemeSet::load_defaults).themes.keys().cloned().collect()
}

fn theme(name: &str) -> &'static Theme {
    let set = THEMES.get_or_init(ThemeSet::load_defaults);
    set.themes
        .get(name)
        .or_else(|| set.themes.get("base16-ocean.dark"))
        .or_else(|| set.themes.values().next())
        .expect("syntect ships themes")
}

/// Decodes file bytes like ripgrep: UTF-16 with BOM, UTF-8 (BOM stripped), otherwise lossy UTF-8.
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The occurrence to emphasise: (0-based line, byte start, byte end).
pub type Current = Option<(usize, usize, usize)>;

pub struct Preview {
    pub path: PathBuf,
    pub lines: Vec<String>,
    theme_name: String,
    hl: Option<HighlightLines<'static>>,
    /// Tried to set up highlighting (successfully or not).
    hl_tried: bool,
    styled: Vec<Vec<(Style, Range<usize>)>>,
    /// Lines from this index on are rendered without syntax colours.
    plain_from: usize,
    /// Visible window requested by the last render.
    want: Range<usize>,
}

impl Preview {
    pub fn load(path: &Path, theme_name: &str) -> Self {
        let text = std::fs::read(path).map(|b| decode(&b)).unwrap_or_else(|e| format!("<cannot read file: {e}>"));
        Self::from_text(path, &text, theme_name)
    }

    pub fn from_text(path: &Path, text: &str, theme_name: &str) -> Self {
        let lines = text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
        Self {
            path: path.to_path_buf(),
            lines,
            theme_name: theme_name.to_string(),
            hl: None,
            hl_tried: false,
            styled: Vec::new(),
            plain_from: MAX_HL_LINES,
            want: 0..0,
        }
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    #[cfg(test)]
    pub fn highlighted_lines(&self) -> usize {
        self.styled.len()
    }

    fn try_init_highlighter(&mut self) {
        if self.hl_tried {
            return;
        }
        let (Some(ss), Some(_)) = (SYNTAXES.get(), THEMES.get()) else { return };
        self.hl_tried = true;
        let syntax = ss
            .find_syntax_for_file(&self.path)
            .ok()
            .flatten()
            .or_else(|| self.lines.first().and_then(|l| ss.find_syntax_by_first_line(l)));
        if let Some(syntax) = syntax
            && syntax.name != "Plain Text"
        {
            self.hl = Some(HighlightLines::new(syntax, theme(&self.theme_name)));
        }
    }

    fn hl_target(&self) -> usize {
        self.want.end.min(self.lines.len()).min(self.plain_from)
    }

    /// True while highlighting still has to catch up with the visible window.
    pub fn pending(&self) -> bool {
        if !self.hl_tried {
            return true; // syntaxes are still loading
        }
        self.hl.is_some() && self.want.start < self.plain_from && self.styled.len() < self.hl_target()
    }

    /// Advances syntax highlighting towards the visible window for at most `budget`.
    /// Returns true if any line was highlighted.
    pub fn advance(&mut self, budget: Duration) -> bool {
        self.try_init_highlighter();
        if self.want.start >= self.plain_from {
            return false; // the visible window is past the highlight cap: stay plain
        }
        let target = self.hl_target();
        let (Some(hl), Some(ss)) = (self.hl.as_mut(), SYNTAXES.get()) else { return false };
        let start = Instant::now();
        let before = self.styled.len();
        while self.styled.len() < target {
            let line = &self.lines[self.styled.len()];
            if line.len() > MAX_HL_LINE {
                self.plain_from = self.styled.len();
                break;
            }
            let with_nl = format!("{line}\n");
            let Ok(pieces) = hl.highlight_line(&with_nl, ss) else {
                self.plain_from = self.styled.len();
                break;
            };
            let mut pos = 0;
            let mut segs = Vec::with_capacity(pieces.len());
            for (st, piece) in pieces {
                let end = (pos + piece.len()).min(line.len());
                if pos < end {
                    segs.push((convert_style(st), pos..end));
                }
                pos += piece.len();
            }
            self.styled.push(segs);
            if self.styled.len().is_multiple_of(16) && start.elapsed() >= budget {
                break;
            }
        }
        self.styled.len() > before
    }

    /// Renders lines `top..top+count` (text only, no gutter). Lines not yet highlighted are
    /// shown plain; highlighting continues in later `advance` calls.
    pub fn render(&mut self, top: usize, count: usize, matcher: Option<&RegexMatcher>, current: Current) -> Vec<Line<'static>> {
        let end = (top + count).min(self.lines.len());
        self.want = top..end;
        self.advance(RENDER_BUDGET);
        (top..end)
            .map(|i| {
                let line = &self.lines[i];
                let base: Vec<(Style, Range<usize>)> = match self.styled.get(i) {
                    Some(segs) => segs.clone(),
                    None => vec![(Style::default(), 0..line.len())],
                };
                let mut ranges = Vec::new();
                if let Some(m) = matcher {
                    let _ = m.find_iter(line.as_bytes(), |r| {
                        if r.start() < r.end() {
                            let strong = current == Some((i, r.start(), r.end()));
                            ranges.push((r.start(), r.end(), if strong { current_style() } else { match_style() }));
                        }
                        true
                    });
                }
                Line::from(compose(line, &base, &ranges))
            })
            .collect()
    }

    /// Width of the line-number column (digits only).
    pub fn gutter_digits(&self) -> usize {
        self.lines.len().max(1).to_string().len().max(3)
    }

    /// Number of visual rows line `i` takes when wrapped at `w` cells.
    pub fn rows(&self, i: usize, w: usize) -> usize {
        self.lines.get(i).map_or(1, |l| wrap_starts(&sanitize(l), w).len())
    }

    /// Visual row (within line `i` wrapped at `w`) that contains byte offset `byte`.
    pub fn row_of(&self, i: usize, byte: usize, w: usize) -> usize {
        let Some(line) = self.lines.get(i) else { return 0 };
        let pos = sanitize(&line[..floor_char(line, byte)]).chars().count();
        let starts = wrap_starts(&sanitize(line), w);
        starts.partition_point(|&s| s <= pos).saturating_sub(1)
    }

    /// Display column (in cells) of byte offset `byte` on line `i`.
    pub fn display_col(&self, i: usize, byte: usize) -> usize {
        self.lines.get(i).map_or(0, |l| sanitize(&l[..floor_char(l, byte)]).width())
    }
}

fn convert_style(st: syntect::highlighting::Style) -> Style {
    let mut s = Style::default().fg(Color::Rgb(st.foreground.r, st.foreground.g, st.foreground.b));
    if st.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    if st.font_style.contains(FontStyle::ITALIC) {
        s = s.add_modifier(Modifier::ITALIC);
    }
    s
}

/// Splits syntax segments at match boundaries and patches each match range with its style.
pub fn compose(line: &str, base: &[(Style, Range<usize>)], matches: &[(usize, usize, Style)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut push = |style: Style, a: usize, b: usize| {
        let (a, b) = (floor_char(line, a), floor_char(line, b));
        if a < b {
            spans.push(Span::styled(sanitize(&line[a..b]), style));
        }
    };
    for (style, seg) in base {
        let mut pos = seg.start;
        for &(s, e, ms) in matches {
            if e <= pos || s >= seg.end {
                continue;
            }
            let s = s.max(pos);
            let e = e.min(seg.end);
            push(*style, pos, s);
            push(style.patch(ms), s, e);
            pos = e;
        }
        push(*style, pos, seg.end);
    }
    spans
}

/// Expands tabs and replaces control characters so they cannot break the terminal.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Char indices where each visual row starts when `text` is hard-wrapped at `w` cells.
pub fn wrap_starts(text: &str, w: usize) -> Vec<usize> {
    let w = w.max(1);
    let mut starts = vec![0];
    let mut used = 0;
    for (ci, c) in text.chars().enumerate() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w && used > 0 {
            starts.push(ci);
            used = 0;
        }
        used += cw;
    }
    starts
}

/// Hard-wraps a rendered line at `w` cells, using the same rule as `wrap_starts`.
pub fn wrap_line(line: Line<'static>, w: usize) -> Vec<Line<'static>> {
    let w = w.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in line.spans {
        for c in span.content.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > w && used > 0 {
                rows.push(std::mem::take(&mut cur));
                used = 0;
            }
            match cur.last_mut() {
                Some(last) if last.style == span.style => last.content.to_mut().push(c),
                _ => cur.push(Span::styled(c.to_string(), span.style)),
            }
            used += cw;
        }
    }
    rows.push(cur);
    rows.into_iter().map(Line::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_boms() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi"), "hi");
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'h', 0, b'i']), "hi");
        assert_eq!(decode(b"a\xFFb"), "a\u{FFFD}b");
    }

    #[test]
    fn compose_splits_segments_at_matches() {
        let line = "let foo = 1;";
        let base = vec![(Style::default(), 0..4), (Style::default().fg(Color::Red), 4..12)];
        let spans = compose(line, &base, &[(4, 7, match_style())]);
        let texts: Vec<_> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["let ", "foo", " = 1;"]);
        assert_eq!(spans[1].style.bg, Some(MATCH_BG));
        assert_eq!(spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn only_the_current_occurrence_is_strong() {
        let mut p = Preview::from_text(Path::new("x.txt"), "foo foo\n", "base16-ocean.dark");
        let m = crate::search::build_matcher("foo", crate::config::Mode::Literal, crate::config::Case::Smart).unwrap();
        let line = &p.render(0, 1, Some(&m), Some((0, 4, 7)))[0];
        let bgs: Vec<_> = line.spans.iter().filter(|s| s.content == "foo").map(|s| s.style.bg).collect();
        assert_eq!(bgs, vec![Some(MATCH_BG), Some(CURRENT_BG)]);
    }

    #[test]
    fn late_match_renders_without_highlighting_the_whole_file() {
        init_blocking();
        let mut text = "fn main() { let x = 1; }\n".repeat(3_000);
        text.push_str("let needle = 1;\n");
        let mut p = Preview::from_text(Path::new("big.rs"), &text, "base16-ocean.dark");
        let lines = p.render(2_995, 10, None, None);
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("needle"))));
        assert!(p.highlighted_lines() < 3_000, "render must stay within its budget");
        while p.pending() {
            p.advance(Duration::from_millis(50));
        }
        assert!(p.highlighted_lines() >= 3_001);

        // Past the highlight cap nothing is highlighted at all.
        let huge = "x\n".repeat(MAX_HL_LINES + 100);
        let mut p = Preview::from_text(Path::new("huge.rs"), &huge, "base16-ocean.dark");
        p.render(MAX_HL_LINES + 10, 10, None, None);
        assert_eq!(p.highlighted_lines(), 0);
        assert!(!p.pending());
    }

    #[test]
    fn wrapping_is_consistent() {
        let text = "abcdefghij";
        assert_eq!(wrap_starts(text, 4), vec![0, 4, 8]);
        let rows = wrap_line(Line::from(text.to_string()), 4);
        let row_texts: Vec<String> = rows.iter().map(|l| l.to_string()).collect();
        assert_eq!(row_texts, vec!["abcd", "efgh", "ij"]);
        // Wide characters never split across the edge.
        assert_eq!(wrap_starts("a😀b", 2), vec![0, 1, 2]);
        let p = Preview::from_text(Path::new("x.txt"), "abcdefghij", "t");
        assert_eq!(p.rows(0, 4), 3);
        assert_eq!(p.row_of(0, 9, 4), 2);
    }

    #[test]
    fn sanitize_expands_tabs() {
        assert_eq!(sanitize("\ta\u{1b}"), "    a ");
    }
}

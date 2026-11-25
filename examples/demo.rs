//! Renders the README demo: drives the real app with scripted keys on an offscreen terminal
//! and writes `docs/demo.svg` (animated) and `docs/screenshot.svg` (still).
//!
//! ```
//! cargo run --release --example demo
//! ```
//!
//! It searches this repository and uses an in-memory config, so nothing on disk changes
//! except the two SVGs.

#![allow(dead_code)]

#[path = "../src/app.rs"]
mod app;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/editor.rs"]
mod editor;
#[path = "../src/preview.rs"]
mod preview;
#[path = "../src/search.rs"]
mod search;
#[path = "../src/ui.rs"]
mod ui;

use app::{App, Popup};
use config::{Config, clean_canonical, git_root};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

const COLS: u16 = 112;
const ROWS: u16 = 30;
const CW: f64 = 8.4; // cell width (px)
const CH: f64 = 18.0; // cell height (px)
const PAD: f64 = 12.0;
const BAR: f64 = 28.0; // window title bar
const FG: &str = "#d4d4d4";
const BG: &str = "#1e1e1e";

enum Step {
    Key(KeyEvent),
    Type(&'static str),
    Hold(u64),
}

fn key(code: KeyCode) -> Step {
    Step::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn ctrl(c: char) -> Step {
    Step::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

/// A captured screen: cells plus where the cursor is (if shown) and how long it stays.
struct Frame {
    buf: Buffer,
    cursor: Option<(u16, u16)>,
    ms: u64,
}

fn main() {
    let repo = clean_canonical(Path::new(env!("CARGO_MANIFEST_DIR")));
    let docs = repo.join("docs");
    // Earlier output would show up in the search results.
    for name in ["demo.svg", "screenshot.svg"] {
        let _ = std::fs::remove_file(docs.join(name));
    }
    search::build_types(&[]).unwrap();
    preview::init_blocking();
    let (cfg, store, _) = Config::load_from(None);
    let mut app = App::new(cfg, store, repo.clone(), git_root(&repo), Vec::new(), false);
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();

    use KeyCode::*;
    let script = vec![
        Step::Hold(900),
        Step::Type("preview"),
        Step::Hold(1400),
        key(Down),
        Step::Hold(700),
        key(Down),
        Step::Hold(700),
        ctrl('n'),
        Step::Hold(600),
        ctrl('n'),
        Step::Hold(1000),
        // Restrict to Rust files.
        ctrl('t'),
        Step::Hold(700),
        Step::Type("rust"),
        Step::Hold(500),
        key(Char(' ')),
        Step::Hold(700),
        key(Enter),
        Step::Hold(1200),
        // One row per match.
        ctrl('l'),
        Step::Hold(1000),
        key(Down),
        Step::Hold(350),
        key(Down),
        Step::Hold(350),
        key(Down),
        Step::Hold(1000),
        // Regex mode.
        ctrl('u'),
        ctrl('r'),
        Step::Hold(400),
        Step::Type(r"fn \w+_preview"),
        Step::Hold(1300),
        key(Down),
        Step::Hold(500),
        key(Down),
        Step::Hold(2500),
    ];

    let mut frames: Vec<Frame> = Vec::new();
    let mut still: Option<Frame> = None;
    capture(&mut term, &mut app, &mut frames, 0);
    for (i, step) in script.iter().enumerate() {
        match step {
            Step::Hold(ms) => frames.last_mut().unwrap().ms += ms,
            Step::Key(k) => {
                app.on_key(*k);
                capture(&mut term, &mut app, &mut frames, 120);
            }
            Step::Type(s) => {
                for c in s.chars() {
                    app.on_key(KeyEvent::new(Char(c), KeyModifiers::NONE));
                    capture(&mut term, &mut app, &mut frames, 110);
                }
            }
        }
        // The still: Rust-only file list, second match of the selected file.
        if i == 10 {
            let f = frames.last().unwrap();
            still = Some(Frame { buf: f.buf.clone(), cursor: f.cursor, ms: 0 });
        }
    }

    // For checking single frames: DEMO_FRAMES=<dir> writes each frame as its own SVG.
    if let Some(dir) = std::env::var_os("DEMO_FRAMES") {
        for (i, f) in frames.iter().enumerate() {
            let one = Frame { buf: f.buf.clone(), cursor: f.cursor, ms: 0 };
            std::fs::write(Path::new(&dir).join(format!("{i:02}.svg")), render(&[one], false)).unwrap();
        }
    }

    let anim = render(&frames, true);
    std::fs::write(docs.join("demo.svg"), &anim).unwrap();
    let still = render(&[still.unwrap()], false);
    std::fs::write(docs.join("screenshot.svg"), &still).unwrap();
    println!(
        "{} frames, demo.svg {} KB, screenshot.svg {} KB",
        frames.len(),
        anim.len() / 1024,
        still.len() / 1024
    );
}

/// Lets searches and preview highlighting finish, then draws one frame.
fn capture(term: &mut Terminal<TestBackend>, app: &mut App, frames: &mut Vec<Frame>, ms: u64) {
    let start = Instant::now();
    loop {
        app.tick();
        while app.advance_preview(Duration::from_millis(50)) {}
        term.draw(|f| ui::draw(f, app)).unwrap();
        // A second draw lets the preview pick up the size from the first one.
        while app.advance_preview(Duration::from_millis(50)) {}
        if !app.is_busy() && !app.preview_pending() || start.elapsed() > Duration::from_secs(5) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    term.draw(|f| ui::draw(f, app)).unwrap();
    let cursor = matches!(app.popup, Popup::None)
        .then(|| term.backend_mut().get_cursor_position().ok())
        .flatten()
        .map(|p| (p.x, p.y));
    let buf = term.backend().buffer().clone();
    if let Some(last) = frames.last_mut()
        && last.buf == buf
        && last.cursor == cursor
    {
        last.ms += ms;
        return;
    }
    frames.push(Frame { buf, cursor, ms });
}

fn color(c: Color, default: &str) -> String {
    match c {
        Color::Reset => default.into(),
        Color::Black => "#000000".into(),
        Color::Red => "#f14c4c".into(),
        Color::Green => "#23d18b".into(),
        Color::Yellow => "#e5c07b".into(),
        Color::Blue => "#3b8eea".into(),
        Color::Magenta => "#d670d6".into(),
        Color::Cyan => "#29b8db".into(),
        Color::Gray => "#cccccc".into(),
        Color::DarkGray => "#767676".into(),
        Color::LightRed => "#f48771".into(),
        Color::LightGreen => "#89d185".into(),
        Color::LightYellow => "#f5f543".into(),
        Color::LightBlue => "#6cb6ff".into(),
        Color::LightMagenta => "#e08ce0".into(),
        Color::LightCyan => "#56d4e8".into(),
        Color::White => "#ffffff".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("#{0:02x}{0:02x}{0:02x}", i),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// SVG for one terminal row (background runs, then text runs), positioned at y = 0.
fn row_svg(buf: &Buffer, y: u16, cursor: Option<u16>) -> String {
    let mut out = String::new();
    let mut cells = Vec::new();
    for x in 0..COLS {
        let c = &buf[(x, y)];
        let (mut fg, mut bg) = (color(c.fg, FG), color(c.bg, BG));
        if c.modifier.contains(Modifier::REVERSED) ^ (cursor == Some(x)) {
            std::mem::swap(&mut fg, &mut bg);
        }
        cells.push((c.symbol().to_string(), fg, bg, c.modifier.contains(Modifier::BOLD)));
    }
    // Backgrounds.
    let mut x = 0;
    while x < cells.len() {
        let bg = &cells[x].2;
        let start = x;
        while x < cells.len() && cells[x].2 == *bg {
            x += 1;
        }
        if bg != BG {
            let _ = write!(
                out,
                r#"<rect x="{:.1}" width="{:.1}" height="{CH}" fill="{bg}"/>"#,
                start as f64 * CW,
                (x - start) as f64 * CW
            );
        }
    }
    // Text: runs of one style; blank cells join any run. Wide chars leave an empty cell after them.
    let mut x = 0;
    while x < cells.len() {
        if cells[x].0.trim().is_empty() {
            x += 1;
            continue;
        }
        let (fg, bold) = (cells[x].1.clone(), cells[x].3);
        let start = x;
        let mut text = String::new();
        let mut end = x;
        while x < cells.len() {
            let (s, f, _, b) = &cells[x];
            let blank = s.trim().is_empty();
            if !blank && (*f != fg || *b != bold) {
                break;
            }
            text.push_str(if s.is_empty() { "" } else if blank { " " } else { s });
            if !blank {
                end = x + 1;
            }
            x += 1;
        }
        let text: String = text.trim_end().to_string();
        let _ = write!(
            out,
            r#"<text x="{:.1}" y="13.5" fill="{fg}"{} textLength="{:.1}" lengthAdjust="spacingAndGlyphs">{}</text>"#,
            start as f64 * CW,
            if bold { r#" font-weight="bold""# } else { "" },
            (end - start) as f64 * CW,
            esc(&text)
        );
    }
    out
}

/// Renders frames as an SVG window. With several frames, each frame is a stack of shared rows
/// and a CSS animation steps through them.
fn render(frames: &[Frame], animate: bool) -> String {
    let w = COLS as f64 * CW + 2.0 * PAD;
    let h = ROWS as f64 * CH + 2.0 * PAD + BAR;
    let mut rows: HashMap<String, usize> = HashMap::new();
    let mut defs = String::new();
    let mut groups = String::new();
    for (i, f) in frames.iter().enumerate() {
        let _ = write!(groups, r#"<g class="f" id="f{i}">"#);
        for y in 0..ROWS {
            let cur = f.cursor.filter(|c| c.1 == y).map(|c| c.0);
            let svg = row_svg(&f.buf, y, cur);
            if svg.is_empty() {
                continue;
            }
            let n = rows.len();
            let id = *rows.entry(svg.clone()).or_insert_with(|| {
                let _ = write!(defs, r#"<g id="r{n}">{svg}</g>"#);
                n
            });
            let _ = write!(groups, r##"<use href="#r{id}" y="{:.1}"/>"##, y as f64 * CH);
        }
        groups.push_str("</g>");
    }

    let mut css = String::from(
        "text{font-family:'Cascadia Mono',Consolas,'SF Mono',Menlo,'DejaVu Sans Mono',monospace;font-size:14px;white-space:pre}",
    );
    if animate {
        let total: u64 = frames.iter().map(|f| f.ms.max(1)).sum();
        let _ = write!(css, ".f{{visibility:hidden;animation:{total}ms step-end infinite}}");
        let mut t = 0;
        for (i, f) in frames.iter().enumerate() {
            let a = t as f64 * 100.0 / total as f64;
            t += f.ms.max(1);
            let b = t as f64 * 100.0 / total as f64;
            let mut k = format!("{a:.3}%{{visibility:visible}}{b:.3}%{{visibility:hidden}}");
            if i == 0 {
                k = format!("0%{{visibility:visible}}{b:.3}%{{visibility:hidden}}");
            }
            let _ = write!(css, "@keyframes k{i}{{{k}}}#f{i}{{animation-name:k{i}}}");
        }
    }

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.0} {h:.0}"><style>{css}</style><defs>{defs}</defs><rect width="100%" height="100%" rx="8" fill="{BG}"/><rect width="100%" height="{BAR}" rx="8" fill="#2d2d2d"/><rect y="{b2}" width="100%" height="8" fill="#2d2d2d"/><circle cx="18" cy="14" r="6" fill="#ff5f57"/><circle cx="38" cy="14" r="6" fill="#febc2e"/><circle cx="58" cy="14" r="6" fill="#28c840"/><text x="{mid:.0}" y="18.5" fill="#9d9d9d" text-anchor="middle" font-size="12">rgg</text><g transform="translate({PAD},{top})">{groups}</g></svg>"##,
        b2 = BAR - 8.0,
        mid = w / 2.0,
        top = BAR + PAD,
    )
}

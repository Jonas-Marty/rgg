//! Rendering.

use crate::app::{App, Areas, FOLDER_SETTINGS, Popup, RowRef, SETTINGS, Status, VALUE_SETTINGS};
use crate::config::{Case, ListMode, Mode};
use crate::preview::{compose, current_style, match_style, sanitize, wrap_line};
use crate::search::MAX_PER_FILE;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

const ACCENT: Color = Color::Rgb(130, 170, 255);
const DIM: Color = Color::DarkGray;
const SEL_BG: Color = Color::Rgb(45, 55, 85);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, footer] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let [left, right] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(main);
    let [search, status, list] =
        Layout::vertical([Constraint::Length(3), Constraint::Length(2), Constraint::Min(3)]).areas(left);
    app.areas = Areas { list, preview: right };

    draw_search(f, app, search);
    draw_status(f, app, status);
    draw_list(f, app, list);
    draw_preview(f, app, right);
    draw_footer(f, app, footer);

    match &app.popup {
        Popup::None => {}
        Popup::Types(_) => draw_types(f, app),
        Popup::Settings(_) => draw_settings(f, app),
        Popup::Help => draw_help(f),
    }
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(" Search ").border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width.max(1) as usize;
    let cursor_chars = app.query[..app.cursor].chars().count();
    let skip = cursor_chars.saturating_sub(w - 1);
    let visible: String = app.query.chars().skip(skip).take(w).collect();
    f.render_widget(Paragraph::new(visible), inner);
    if matches!(app.popup, Popup::None) {
        f.set_cursor_position((inner.x + (cursor_chars - skip) as u16, inner.y));
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let counts = format!(" {} files · {} matches · ", app.files.len(), app.total_matches);
    let line1 = if let Some(msg) = &app.message {
        Line::from(Span::styled(format!(" {msg}"), Style::default().fg(Color::Yellow)))
    } else {
        match &app.status {
            Status::Idle => Line::from(Span::styled(
                format!(" type at least {} characters", app.cfg.min_query_len),
                Style::default().fg(DIM),
            )),
            Status::Searching => Line::from(vec![Span::raw(counts), Span::styled("searching…", Style::default().fg(Color::Yellow))]),
            Status::Done { elapsed, limit_reached } => {
                let mut spans = vec![
                    Span::raw(counts),
                    Span::styled(format!("done {:.3}s", elapsed.as_secs_f64()), Style::default().fg(Color::Green)),
                ];
                if *limit_reached {
                    spans.push(Span::styled(" · file limit reached", Style::default().fg(Color::Yellow)));
                }
                Line::from(spans)
            }
            Status::Error(e) => Line::from(Span::styled(format!(" {e}"), Style::default().fg(Color::Red))),
        }
    };
    let mut line1 = line1;
    if app.truncated_files > 0 && app.message.is_none() && !matches!(app.status, Status::Error(_)) {
        line1.spans.push(Span::styled(
            format!(" · {} truncated (>{} per file)", app.truncated_files, MAX_PER_FILE),
            Style::default().fg(Color::Yellow),
        ));
    }

    let mark = if app.folder_saved { "●" } else { "" };
    let tag = |s: String, folder: bool| {
        Span::styled(format!("[{s}]{}", if folder { mark } else { "" }), Style::default().fg(ACCENT))
    };
    let mut tags = vec![Span::raw(" ")];
    tags.push(tag(if app.folder.mode == Mode::Regex { "regex" } else { "literal" }.into(), true));
    tags.push(Span::raw(" "));
    tags.push(tag(
        match app.cfg.case {
            Case::Smart => "smart-case",
            Case::Sensitive => "case-sensitive",
            Case::Insensitive => "ignore-case",
        }
        .into(),
        false,
    ));
    tags.push(Span::raw(" "));
    tags.push(tag(
        if app.folder.types.is_empty() { "all types".into() } else { app.folder.types.join(", ") },
        true,
    ));
    if app.folder.hidden {
        tags.push(Span::raw(" "));
        tags.push(tag("hidden".into(), true));
    }
    if app.folder.no_ignore {
        tags.push(Span::raw(" "));
        tags.push(tag("no-ignore".into(), true));
    }
    f.render_widget(Paragraph::new(vec![line1, Line::from(tags)]), area);
}

/// Shortens from the left so the end (file name) stays visible.
fn shorten_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let tail: String = s.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.cfg.list_mode {
        ListMode::File => " Files ",
        ListMode::Match => " Matches ",
    };
    let block = Block::bordered().title(title).border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let h = inner.height as usize;
    let w = inner.width as usize;
    let count = app.row_count();
    if count == 0 || h == 0 {
        return;
    }
    let sel = app.sel_row();
    if sel < app.list_offset {
        app.list_offset = sel;
    } else if sel >= app.list_offset + h {
        app.list_offset = sel + 1 - h;
    }
    // Keep a group's header visible when its first item is selected at the top.
    if sel == app.list_offset && sel > 0 && matches!(app.row_ref(sel - 1), RowRef::Header(_)) {
        app.list_offset = sel - 1;
    }
    app.list_offset = app.list_offset.min(count.saturating_sub(h));

    let mut lines = Vec::with_capacity(h);
    for row in app.list_offset..(app.list_offset + h).min(count) {
        let (fi, mi) = match app.row_ref(row) {
            RowRef::Item(fi, mi) => (fi, mi),
            RowRef::Header(fi) => {
                let Some(file) = app.files.get(fi) else { break };
                let label = format!("─ {} ", shorten_left(&app.repo_label(&file.repo), w.saturating_sub(4)));
                let fill = "─".repeat(w.saturating_sub(label.chars().count()));
                lines.push(Line::from(Span::styled(
                    format!("{label}{fill}"),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )));
                continue;
            }
        };
        let Some(file) = app.files.get(fi) else { break };
        let selected = row == sel;
        let prefix = if selected { "▶ " } else { "  " };
        let path = app.display_path(file);
        let base = if selected {
            Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![Span::styled(prefix, base.fg(ACCENT))];
        match app.cfg.list_mode {
            ListMode::File => {
                let plus = if file.truncated { "+" } else { "" };
                let count = format!(" ({}{plus})", file.matches.len());
                let avail = w.saturating_sub(2 + count.chars().count());
                let p = shorten_left(&path, avail);
                let pad = avail.saturating_sub(p.chars().count());
                spans.push(Span::styled(format!("{p}{}", " ".repeat(pad)), base));
                spans.push(Span::styled(count, base.fg(DIM)));
            }
            ListMode::Match => {
                let Some(m) = file.matches.get(mi) else { break };
                let loc_max = (w.saturating_sub(2) * 55 / 100).max(8);
                let suffix = format!(":{}:{}", m.line, m.col);
                let loc = format!("{}{suffix}", shorten_left(&path, loc_max.saturating_sub(suffix.len())));
                spans.push(Span::styled(loc, base.fg(ACCENT)));
                spans.push(Span::styled("  ", base));
                let ranges: Vec<(usize, usize, Style)> = m
                    .text
                    .ranges
                    .iter()
                    .enumerate()
                    .map(|(k, &(s, e))| (s, e, if m.nth == Some(k) { current_style() } else { match_style() }))
                    .collect();
                let text_spans = compose(&m.text.text, &[(base, 0..m.text.text.len())], &ranges);
                spans.extend(text_spans);
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                if selected && used < w {
                    spans.push(Span::styled(" ".repeat(w - used), base));
                }
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_h = area.height.saturating_sub(2) as usize;
    app.sync_preview(inner_h, area.width.saturating_sub(2) as usize);
    let mut block = Block::bordered().border_style(Style::default().fg(DIM));
    let Some((file, m)) = app.selected() else {
        let hint = Paragraph::new(Line::from(Span::styled(" no file selected", Style::default().fg(DIM))));
        f.render_widget(hint.block(block.title(" Preview ")), area);
        return;
    };
    let rel = file.rel.to_string_lossy().into_owned();
    let cur_line = (m.line as usize).saturating_sub(1);
    let current = Some((cur_line, m.start, m.end));
    let right = match app.cfg.list_mode {
        ListMode::File => format!(
            " match {}/{}{} ",
            app.sel_match + 1,
            file.matches.len(),
            if file.truncated { "+" } else { "" }
        ),
        ListMode::Match => format!(" {}:{} ", m.line, m.col),
    };
    let title_max = (area.width as usize).saturating_sub(right.chars().count() + 6);
    block = block
        .title(format!(" {} ", shorten_left(&rel, title_max)))
        .title(Line::from(right).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (top, sub, wrap, tw) = (app.preview_top, app.preview_sub, app.cfg.wrap, app.preview_text_w);
    let matcher = app.matcher.clone();
    let Some(preview) = app.preview.as_mut() else { return };
    let gw = preview.gutter_digits();
    let h = inner.height as usize;
    let [gut, body] = Layout::horizontal([Constraint::Length(gw as u16 + 3), Constraint::Min(1)]).areas(inner);
    let gutter_style = |i: usize| {
        if i == cur_line { Style::default().fg(Color::Yellow) } else { Style::default().fg(DIM) }
    };
    // Each source line takes at least one row, so `h` lines always fill the view.
    let text = preview.render(top, h, matcher.as_ref(), current);

    let mut rows = Vec::with_capacity(h);
    let mut nums = Vec::with_capacity(h);
    for (k, line) in text.into_iter().enumerate() {
        let i = top + k;
        let pieces = if wrap { wrap_line(line, tw) } else { vec![line] };
        let skip = if k == 0 && wrap { sub } else { 0 };
        for (r, piece) in pieces.into_iter().enumerate().skip(skip) {
            let num = if r == 0 { format!("{:>gw$} │ ", i + 1) } else { format!("{:>gw$} ┆ ", "") };
            nums.push(Line::from(Span::styled(num, gutter_style(i))));
            rows.push(piece);
            if rows.len() == h {
                break;
            }
        }
        if rows.len() == h {
            break;
        }
    }
    f.render_widget(Paragraph::new(nums), gut);
    if wrap {
        f.render_widget(Paragraph::new(rows), body);
    } else {
        // Scroll horizontally when the current match is beyond the visible width.
        let col = preview.display_col(cur_line, current.map_or(0, |c| c.1));
        let bw = body.width as usize;
        let x = if col + 8 > bw { col.saturating_sub(bw / 3) } else { 0 };
        f.render_widget(Paragraph::new(rows).scroll((0, x.min(u16::MAX as usize) as u16)), body);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let mut entries: Vec<(&str, u8)> = vec![("↑↓ select", 1)];
    if app.cfg.list_mode == ListMode::File {
        entries.push(("^N/^P next/prev match", 3));
    }
    entries.extend([
        ("Enter open in VS Code", 1),
        ("^R regex", 2),
        ("^T types", 2),
        ("Alt+W wrap", 4),
        ("^O settings", 2),
        ("F1 help", 1),
        ("Esc quit", 1),
    ]);
    let width = |e: &[(&str, u8)]| e.iter().map(|(s, _)| s.chars().count() + 3).sum::<usize>();
    while width(&entries) > area.width as usize && entries.len() > 1 {
        let worst = entries
            .iter()
            .enumerate()
            .max_by_key(|(i, (_, p))| (*p, *i))
            .map(|(i, _)| i)
            .unwrap_or(0);
        entries.remove(worst);
    }
    let text = entries.iter().map(|(s, _)| *s).collect::<Vec<_>>().join(" · ");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text, Style::default().fg(ACCENT))).centered()),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::default().fg(ACCENT))
}

fn draw_types(f: &mut Frame, app: &mut App) {
    let Popup::Types(p) = &mut app.popup else { return };
    let area = centered(f.area(), 64, 22);
    f.render_widget(Clear, area);
    let block = popup_block("File types")
        .title(Line::from(format!(" {} selected ", p.selected.len())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [filter, list, hint] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    f.render_widget(Paragraph::new(format!("Filter: {}", p.filter)), filter);
    f.set_cursor_position((filter.x + 8 + p.filter.chars().count() as u16, filter.y));

    let visible = p.visible();
    let h = list.height as usize;
    if p.cursor < p.offset {
        p.offset = p.cursor;
    } else if p.cursor >= p.offset + h {
        p.offset = p.cursor + 1 - h;
    }
    let w = list.width as usize;
    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(p.offset)
        .take(h)
        .map(|(k, &i)| {
            let (name, globs) = &p.all[i];
            let on = p.selected.contains(name);
            let cursor = k == p.cursor;
            let text = format!(
                "{}[{}] {:<12} {}",
                if cursor { "▶ " } else { "  " },
                if on { "x" } else { " " },
                name,
                globs
            );
            let text: String = text.chars().take(w).collect();
            let mut style = if on { Style::default().fg(Color::Green) } else { Style::default() };
            if cursor {
                style = style.bg(SEL_BG).add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(text, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);
    f.render_widget(
        Paragraph::new(Span::styled(
            "↑↓ move · Space toggle · ^A clear all · Enter ok · Esc cancel",
            Style::default().fg(DIM),
        )),
        hint,
    );
}

fn draw_settings(f: &mut Frame, app: &App) {
    let Popup::Settings(s) = &app.popup else { return };
    let area = centered(f.area(), 66, SETTINGS.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = popup_block("Settings");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [list, hint] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let w = list.width as usize;
    let lines: Vec<Line> = SETTINGS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let cursor = i == s.cursor;
            let folder = if FOLDER_SETTINGS.contains(&i) && app.folder_saved { "●" } else { " " };
            let value = match (&s.editing, cursor) {
                (Some(buf), true) => format!("{buf}█"),
                _ if VALUE_SETTINGS.contains(&i) => app.setting_value(i),
                _ => format!("‹ {} ›", app.setting_value(i)),
            };
            let text = format!("{}{:<22}{} {}", if cursor { "▶ " } else { "  " }, label, folder, value);
            let text: String = text.chars().take(w).collect();
            let style = if cursor { Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD) } else { Style::default() };
            Line::from(Span::styled(text, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);
    let hint_text = if s.editing.is_some() {
        "type value · Enter apply · Esc cancel"
    } else {
        "↑↓ move · ←→ change · Enter edit · ● = saved for this folder · Esc close"
    };
    f.render_widget(Paragraph::new(Span::styled(hint_text, Style::default().fg(DIM))), hint);
}

fn draw_help(f: &mut Frame) {
    let keys = [
        ("type", "search text (live)"),
        ("↑ ↓ / PgUp PgDn / Home End", "move in result list"),
        ("Ctrl+N / Ctrl+P", "next / previous match in file"),
        ("Ctrl+↑ ↓ / Ctrl+PgUp PgDn", "scroll preview"),
        ("Enter", "open in editor at match"),
        ("Ctrl+R", "toggle literal / regex"),
        ("Ctrl+T", "file type picker"),
        ("Ctrl+L", "toggle per-file / per-match list"),
        ("Ctrl+O", "settings"),
        ("Alt+W", "toggle preview wrap"),
        ("Ctrl+Backspace / Ctrl+W", "delete previous word"),
        ("Ctrl+U", "clear search"),
        ("Mouse wheel", "scroll list / preview"),
        ("Click", "select row"),
        ("F1", "this help"),
        ("Esc", "close popup / quit"),
    ];
    let area = centered(f.area(), 64, keys.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = popup_block("Help");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<28}"), Style::default().fg(ACCENT)),
                Span::raw(sanitize(d)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" press any key to close", Style::default().fg(DIM))));
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::config::{Config, ListMode, Store};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn renders_repository_groups() {
        let dir = crate::search::temp_dir("ui-groups");
        for repo in ["api", "web"] {
            std::fs::create_dir_all(dir.join(repo).join(".git")).unwrap();
            std::fs::create_dir_all(dir.join(repo).join("src").join("deep")).unwrap();
            std::fs::write(dir.join(repo).join("src").join("deep").join("x.cs"), "foo\n").unwrap();
        }
        let store = Store { path: None, writable: true };
        let mut app = App::new(Config::default(), store, dir.clone(), dir.clone(), vec![], false);
        app.query = "foo".into();
        app.cursor = 3;
        app.start_search();
        let start = std::time::Instant::now();
        while app.is_busy() && start.elapsed().as_secs() < 5 {
            app.tick();
        }
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.contains("─ api ─"));
        assert!(s.contains("─ web ─"));
        let deep = std::path::Path::new("src").join("deep").join("x.cs");
        let deep = deep.to_string_lossy();
        assert!(s.contains(&format!("  {deep}")), "paths are relative to the repository");
        assert!(!s.contains(&format!("▶ api{}", std::path::MAIN_SEPARATOR)), "no repo prefix in rows");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
        t.draw(|f| super::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_results_and_preview() {
        let dir = std::env::temp_dir().join(format!("rgg-ui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("main.cs"), "using System;\n\nclass Foo\n{\n    void Run() { var foo = 1; }\n}\n").unwrap();
        std::fs::write(dir.join("notes.md"), "nothing here\nfoo bar\n").unwrap();

        let store = Store { path: None, writable: true };
        let mut app = App::new(Config::default(), store, dir.clone(), dir.clone(), vec![], false);
        app.query = "foo".into();
        app.cursor = 3;
        app.start_search();
        let start = std::time::Instant::now();
        while app.is_busy() && start.elapsed().as_secs() < 5 {
            app.tick();
        }
        // Selection follows the first file to arrive; pick the top row explicitly.
        app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.contains("2 files · 3 matches"), "status line");
        assert!(s.contains("notes.md"));
        assert!(s.contains("main.cs"));
        assert!(s.contains("match 1/1"), "preview title for first file (notes.md)");
        assert!(s.contains("foo bar"), "preview content");

        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(app.cfg.list_mode, ListMode::Match);
        let s = screen(&mut app);
        println!("{s}");
        assert!(s.contains("main.cs:5"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

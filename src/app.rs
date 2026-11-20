//! Application state and input handling.

use crate::config::{Case, Config, FolderSettings, ListMode, Mode, Store};
use crate::preview::Preview;
use crate::search::{self, FileResult, Msg, Occurrence};
use grep_regex::RegexMatcher;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(150);

pub enum Status {
    Idle,
    Searching,
    Done { elapsed: Duration, limit_reached: bool },
    Error(String),
}

pub struct TypePicker {
    pub all: Vec<(String, String)>,
    pub filter: String,
    pub selected: BTreeSet<String>,
    pub cursor: usize,
    pub offset: usize,
}

impl TypePicker {
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.all.len()).filter(|&i| self.all[i].0.contains(&f)).collect()
    }
}

pub struct SettingsState {
    pub cursor: usize,
    /// Text buffer while editing a text/number item.
    pub editing: Option<String>,
}

pub const SETTINGS: &[&str] = &[
    "Search mode",
    "Case",
    "Include hidden",
    "Include ignored",
    "List mode",
    "Wrap preview",
    "After opening",
    "Max file size (MB)",
    "Max files",
    "Min query length",
    "Theme",
    "Editor command",
    "Reset folder to defaults",
    "Config file",
];
/// Settings that are stored per folder.
pub const FOLDER_SETTINGS: &[usize] = &[0, 2, 3];
/// Settings shown as plain values instead of ‹ choices ›.
pub const VALUE_SETTINGS: &[usize] = &[7, 8, 9, 11, 12, 13];

pub enum Popup {
    None,
    Types(TypePicker),
    Settings(SettingsState),
    Help,
}

#[derive(Default, Clone, Copy)]
pub struct Areas {
    pub list: Rect,
    pub preview: Rect,
}

/// Identity of an occurrence that survives re-sorting and re-searching.
type OccKey = (PathBuf, u64, usize);

pub struct App {
    pub cfg: Config,
    store: Store,
    /// Config differs from what is on disk.
    cfg_dirty: bool,
    pub search_root: PathBuf,
    pub git_root: PathBuf,
    folder_key: String,
    /// Effective per-folder settings (saved values plus run-only CLI overrides).
    pub folder: FolderSettings,
    /// Per-folder settings as they are (or will be) persisted; never contains CLI overrides.
    saved_folder: FolderSettings,
    /// True when a folder entry exists in the config.
    pub folder_saved: bool,

    pub query: String,
    /// Byte index of the cursor in `query`.
    pub cursor: usize,

    pub files: Vec<FileResult>,
    offsets: Vec<usize>,
    offsets_dirty: bool,
    pub total_matches: usize,
    pub truncated_files: usize,
    pub sel_file: usize,
    pub sel_match: usize,
    /// Occurrence selected before the current search started; re-selected when it reappears,
    /// unless the user picks something else first.
    sticky: Option<OccKey>,
    pub list_offset: usize,

    pub status: Status,
    pub message: Option<String>,
    generation: u64,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pending_since: Option<Instant>,
    /// Matcher of the search whose results are shown (used to highlight the preview).
    pub matcher: Option<RegexMatcher>,

    pub preview: Option<Preview>,
    /// First source line shown in the preview.
    pub preview_top: usize,
    /// Visual row within `preview_top` where the view starts (wrap mode only).
    pub preview_sub: usize,
    /// Width of the preview text column (without gutter), from the last draw.
    pub preview_text_w: usize,
    synced: Option<OccKey>,

    pub popup: Popup,
    pub areas: Areas,
    pub quit: bool,
}

impl App {
    pub fn new(
        cfg: Config,
        store: Store,
        search_root: PathBuf,
        git_root: PathBuf,
        cli_types: Vec<String>,
        cli_regex: bool,
    ) -> Self {
        let folder_key = git_root.to_string_lossy().into_owned();
        let (saved_folder, folder_saved) = match cfg.folders.get(&folder_key) {
            Some(f) => (f.clone(), true),
            None => (cfg.global_folder_defaults(), false),
        };
        let mut folder = saved_folder.clone();
        if !cli_types.is_empty() {
            folder.types = cli_types;
        }
        if cli_regex {
            folder.mode = Mode::Regex;
        }
        let (tx, rx) = channel();
        Self {
            cfg,
            store,
            cfg_dirty: false,
            search_root,
            git_root,
            folder_key,
            folder,
            saved_folder,
            folder_saved,
            query: String::new(),
            cursor: 0,
            files: Vec::new(),
            offsets: vec![0],
            offsets_dirty: false,
            total_matches: 0,
            truncated_files: 0,
            sel_file: 0,
            sel_match: 0,
            sticky: None,
            list_offset: 0,
            status: Status::Idle,
            message: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
            pending_since: None,
            matcher: None,
            preview: None,
            preview_top: 0,
            preview_sub: 0,
            preview_text_w: 80,
            synced: None,
            popup: Popup::None,
            areas: Areas::default(),
            quit: false,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.pending_since.is_some() || matches!(self.status, Status::Searching)
    }

    pub fn preview_pending(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.pending())
    }

    /// Continues preview highlighting in the background of the event loop.
    pub fn advance_preview(&mut self, budget: Duration) -> bool {
        self.preview.as_mut().is_some_and(|p| p.advance(budget))
    }

    // ---------------------------------------------------------------- search

    /// Runs the debounce timer and drains search results. Returns true if something changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if self.pending_since.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.start_search();
            changed = true;
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::File { generation, file } if generation == self.generation => {
                    self.insert_file(file);
                    changed = true;
                }
                Msg::Done { generation, elapsed, limit_reached } if generation == self.generation => {
                    self.status = Status::Done { elapsed, limit_reached };
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    fn query_changed(&mut self) {
        self.pending_since = Some(Instant::now());
    }

    pub fn start_search(&mut self) {
        self.pending_since = None;
        if self.query.chars().count() < self.cfg.min_query_len.max(1) {
            self.cancel_running();
            self.clear_results();
            self.matcher = None;
            self.status = Status::Idle;
            return;
        }
        let built = search::build_matcher(&self.query, self.folder.mode, self.cfg.case)
            .map_err(|e| format!("regex error: {e}"))
            .and_then(|m| search::build_types(&self.folder.types).map(|t| (m, t)));
        let (matcher, types) = match built {
            Ok(v) => v,
            Err(e) => {
                // The running search belongs to an older query: stop it so it can neither
                // change the list nor replace this error, but keep what is on screen.
                self.cancel_running();
                self.status = Status::Error(e);
                return;
            }
        };
        self.cancel_running();
        if self.sticky.is_none() {
            self.sticky = self.selected_key();
        }
        self.clear_results();
        self.matcher = Some(matcher.clone());
        self.status = Status::Searching;
        let params = search::Params {
            root: self.search_root.clone(),
            hidden: self.folder.hidden,
            no_ignore: self.folder.no_ignore,
            max_filesize: self.cfg.max_file_size_mb.max(1) * 1024 * 1024,
            max_files: self.cfg.max_files.max(1),
        };
        search::spawn(params, matcher, types, self.generation, self.tx.clone(), self.cancel.clone());
    }

    fn cancel_running(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.generation += 1;
    }

    fn clear_results(&mut self) {
        self.files.clear();
        self.offsets = vec![0];
        self.offsets_dirty = false;
        self.total_matches = 0;
        self.truncated_files = 0;
        self.sel_file = 0;
        self.sel_match = 0;
        self.list_offset = 0;
    }

    fn insert_file(&mut self, file: FileResult) {
        let pos = self.files.binary_search_by(|f| f.rel.cmp(&file.rel)).unwrap_or_else(|e| e);
        self.total_matches += file.matches.len();
        self.truncated_files += file.truncated as usize;
        let sticky_hit = match &self.sticky {
            Some((rel, line, start)) if *rel == file.rel => Some(
                file.matches
                    .iter()
                    .position(|m| m.line == *line && m.start == *start)
                    .or_else(|| file.matches.iter().position(|m| m.line == *line))
                    .unwrap_or(0),
            ),
            _ => None,
        };
        let was_empty = self.files.is_empty();
        self.files.insert(pos, file);
        self.offsets_dirty = true;
        if let Some(m) = sticky_hit {
            self.sel_file = pos;
            self.sel_match = m;
            self.sticky = None;
        } else if !was_empty && pos <= self.sel_file {
            // Keep the same occurrence selected while results stream in.
            self.sel_file += 1;
        }
    }

    // ------------------------------------------------------------- selection

    fn ensure_offsets(&mut self) {
        if self.offsets_dirty {
            self.offsets.clear();
            self.offsets.push(0);
            let mut acc = 0;
            for f in &self.files {
                acc += f.matches.len();
                self.offsets.push(acc);
            }
            self.offsets_dirty = false;
        }
    }

    pub fn row_count(&mut self) -> usize {
        match self.cfg.list_mode {
            ListMode::File => self.files.len(),
            ListMode::Match => {
                self.ensure_offsets();
                *self.offsets.last().unwrap_or(&0)
            }
        }
    }

    pub fn sel_row(&mut self) -> usize {
        match self.cfg.list_mode {
            ListMode::File => self.sel_file,
            ListMode::Match => {
                self.ensure_offsets();
                self.offsets.get(self.sel_file).copied().unwrap_or(0) + self.sel_match
            }
        }
    }

    /// Maps a list row to (file index, match index).
    pub fn row_to_sel(&mut self, row: usize) -> (usize, usize) {
        match self.cfg.list_mode {
            ListMode::File => (row, 0),
            ListMode::Match => {
                self.ensure_offsets();
                let f = self.offsets.partition_point(|&o| o <= row).saturating_sub(1);
                (f, row - self.offsets[f])
            }
        }
    }

    fn set_row(&mut self, row: usize) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let (f, m) = self.row_to_sel(row.min(count - 1));
        self.sel_file = f;
        self.sel_match = m;
        self.sticky = None; // the user's choice wins over the previous search's selection
    }

    fn move_rows(&mut self, delta: isize) {
        let row = self.sel_row() as isize + delta;
        self.set_row(row.max(0) as usize);
    }

    fn step_match(&mut self, delta: isize) {
        if let Some(f) = self.files.get(self.sel_file) {
            let n = f.matches.len() as isize;
            self.sel_match = (self.sel_match as isize + delta).rem_euclid(n.max(1)) as usize;
            self.sticky = None;
        }
    }

    pub fn selected(&self) -> Option<(&FileResult, &Occurrence)> {
        let f = self.files.get(self.sel_file)?;
        Some((f, f.matches.get(self.sel_match)?))
    }

    fn selected_key(&self) -> Option<OccKey> {
        self.selected().map(|(f, m)| (f.rel.clone(), m.line, m.start))
    }

    // --------------------------------------------------------------- preview

    /// Loads the selected file into the preview and re-centres on a newly selected occurrence.
    /// `width` is the preview's inner width including the gutter.
    pub fn sync_preview(&mut self, height: usize, width: usize) {
        let Some(key) = self.selected_key() else {
            self.preview = None;
            self.synced = None;
            return;
        };
        let abs = self.search_root.join(&key.0);
        let reload = self
            .preview
            .as_ref()
            .is_none_or(|p| p.path != abs || p.theme_name() != self.cfg.theme);
        if reload {
            self.preview = Some(Preview::load(&abs, &self.cfg.theme));
        }
        let Some(p) = &self.preview else { return };
        self.preview_text_w = width.saturating_sub(p.gutter_digits() + 3).max(1);
        if self.synced.as_ref() != Some(&key) {
            self.recenter(height, (key.1 as usize).saturating_sub(1), key.2);
            self.synced = Some(key);
        }
    }

    /// Puts the occurrence at (line, byte) in the middle of the preview.
    fn recenter(&mut self, height: usize, line: usize, byte: usize) {
        let Some(p) = &self.preview else { return };
        let mut up = height / 2;
        if !self.cfg.wrap {
            self.preview_top = line.saturating_sub(up);
            self.preview_sub = 0;
            return;
        }
        let w = self.preview_text_w;
        let row = p.row_of(line, byte, w);
        if row >= up {
            (self.preview_top, self.preview_sub) = (line, row - up);
            return;
        }
        up -= row;
        (self.preview_top, self.preview_sub) = (line, 0);
        let mut i = line;
        while up > 0 && i > 0 {
            i -= 1;
            let rows = p.rows(i, w);
            if rows > up {
                (self.preview_top, self.preview_sub) = (i, rows - up);
                up = 0;
            } else {
                up -= rows;
                (self.preview_top, self.preview_sub) = (i, 0);
            }
        }
    }

    /// Scrolls the preview by visual rows (wrap mode) or source lines.
    fn scroll_preview(&mut self, delta: isize) {
        let Some(p) = &self.preview else { return };
        let last = p.lines.len().saturating_sub(1);
        if !self.cfg.wrap {
            self.preview_top = (self.preview_top as isize + delta).clamp(0, last as isize) as usize;
            self.preview_sub = 0;
            return;
        }
        let w = self.preview_text_w;
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 {
                if self.preview_sub + 1 < p.rows(self.preview_top, w) {
                    self.preview_sub += 1;
                } else if self.preview_top < last {
                    self.preview_top += 1;
                    self.preview_sub = 0;
                } else {
                    break;
                }
            } else if self.preview_sub > 0 {
                self.preview_sub -= 1;
            } else if self.preview_top > 0 {
                self.preview_top -= 1;
                self.preview_sub = p.rows(self.preview_top, w) - 1;
            } else {
                break;
            }
        }
    }

    // ----------------------------------------------------------------- input

    pub fn on_key(&mut self, key: KeyEvent) {
        self.message = None;
        match self.popup {
            Popup::None => self.key_main(key),
            Popup::Types(_) => self.key_types(key),
            Popup::Settings(_) => self.key_settings(key),
            Popup::Help => self.popup = Popup::None,
        }
    }

    fn key_main(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = self.areas.list.height.saturating_sub(2).max(1) as isize;
        let preview_page = self.areas.preview.height.saturating_sub(2).max(1) as isize;
        match key.code {
            KeyCode::Esc => self.quit = true,
            // AltGr arrives as Ctrl+Alt on Windows: treat it as a normal character.
            KeyCode::Char(c) if ctrl == alt => self.insert_char(c),
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('r') if ctrl => {
                let mode = if self.folder.mode == Mode::Literal { Mode::Regex } else { Mode::Literal };
                self.update_folder(|f| f.mode = mode);
            }
            KeyCode::Char('t') if ctrl => self.open_types(),
            KeyCode::Char('o') if ctrl => {
                self.popup = Popup::Settings(SettingsState { cursor: 0, editing: None })
            }
            KeyCode::Char('n') if ctrl => self.step_match(1),
            KeyCode::Char('p') if ctrl => self.step_match(-1),
            KeyCode::Char('l') if ctrl => self.toggle_list_mode(),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.cursor = 0;
                self.query_changed();
            }
            KeyCode::Char('w') | KeyCode::Char('W') if alt => {
                self.toggle_wrap();
                self.save_config();
            }
            KeyCode::Char('w') | KeyCode::Char('h') if ctrl => self.delete_word(),
            KeyCode::Backspace if ctrl || alt => self.delete_word(),
            KeyCode::Backspace => {
                if let Some(c) = self.query[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                    self.query.remove(self.cursor);
                    self.query_changed();
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.query.len() {
                    self.query.remove(self.cursor);
                    self.query_changed();
                }
            }
            KeyCode::Left => {
                if let Some(c) = self.query[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                }
            }
            KeyCode::Right => {
                if let Some(c) = self.query[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Up if ctrl => self.scroll_preview(-1),
            KeyCode::Down if ctrl => self.scroll_preview(1),
            KeyCode::PageUp if ctrl => self.scroll_preview(-preview_page),
            KeyCode::PageDown if ctrl => self.scroll_preview(preview_page),
            KeyCode::Up => self.move_rows(-1),
            KeyCode::Down => self.move_rows(1),
            KeyCode::PageUp => self.move_rows(-page),
            KeyCode::PageDown => self.move_rows(page),
            KeyCode::Home => self.set_row(0),
            KeyCode::End => self.set_row(usize::MAX),
            KeyCode::Enter => self.open_editor(),
            KeyCode::F(1) => self.popup = Popup::Help,
            _ => {}
        }
    }

    fn insert_char(&mut self, c: char) {
        self.query.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.query_changed();
    }

    fn delete_word(&mut self) {
        let before = &self.query[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        if start < self.cursor {
            self.query.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.query_changed();
        }
    }

    fn toggle_list_mode(&mut self) {
        self.cfg.list_mode = match self.cfg.list_mode {
            ListMode::File => ListMode::Match,
            ListMode::Match => ListMode::File,
        };
        self.cfg_dirty = true;
        self.save_config();
    }

    fn toggle_wrap(&mut self) {
        self.cfg.wrap = !self.cfg.wrap;
        self.cfg_dirty = true;
        self.synced = None; // re-centre with the new layout
    }

    /// The command the editor would be started with for the selected occurrence.
    pub fn editor_target(&self) -> Option<(PathBuf, u64, usize)> {
        let (file, m) = self.selected()?;
        Some((self.search_root.join(&file.rel), m.line, m.col))
    }

    fn open_editor(&mut self) {
        let Some((abs, line, col)) = self.editor_target() else { return };
        match crate::editor::open(&self.cfg.editor_command, &self.git_root, &abs, line, col) {
            Ok(()) if self.cfg.quit_after_open => self.quit = true,
            Ok(()) => self.message = Some(format!("opened {}:{line}:{col}", abs.display())),
            Err(e) => self.message = Some(e),
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        if !matches!(self.popup, Popup::None) {
            return;
        }
        let pos = Position::new(ev.column, ev.row);
        let in_list = self.areas.list.contains(pos);
        let in_preview = self.areas.preview.contains(pos);
        match ev.kind {
            MouseEventKind::ScrollDown if in_list => self.move_rows(1),
            MouseEventKind::ScrollUp if in_list => self.move_rows(-1),
            MouseEventKind::ScrollDown if in_preview => self.scroll_preview(3),
            MouseEventKind::ScrollUp if in_preview => self.scroll_preview(-3),
            MouseEventKind::Down(MouseButton::Left) if in_list && ev.row > self.areas.list.y => {
                // Inner area starts one cell inside the border.
                let row = self.list_offset + (ev.row - self.areas.list.y - 1) as usize;
                if row < self.row_count() {
                    self.set_row(row);
                }
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- settings

    /// Changes per-folder settings: applied to the effective settings and to the persisted
    /// ones, so run-only CLI overrides of *other* fields are never saved.
    fn update_folder(&mut self, change: impl Fn(&mut FolderSettings)) {
        change(&mut self.folder);
        change(&mut self.saved_folder);
        self.cfg.folders.insert(self.folder_key.clone(), self.saved_folder.clone());
        self.folder_saved = true;
        self.cfg_dirty = true;
        self.save_config();
        self.start_search();
    }

    /// Writes the config if it changed and the file may be written.
    fn save_config(&mut self) {
        if !self.cfg_dirty {
            return;
        }
        if !self.store.writable {
            self.message =
                Some("config file is invalid: changes are not saved (Ctrl+O → Config file to overwrite)".into());
            return;
        }
        let Some(path) = &self.store.path else {
            self.cfg_dirty = false;
            return;
        };
        match self.cfg.save_to(path) {
            Ok(()) => self.cfg_dirty = false,
            Err(e) => self.message = Some(format!("cannot save config: {e}")),
        }
    }

    fn open_types(&mut self) {
        self.popup = Popup::Types(TypePicker {
            all: search::type_definitions(),
            filter: String::new(),
            selected: self.folder.types.iter().cloned().collect(),
            cursor: 0,
            offset: 0,
        });
    }

    fn key_types(&mut self, key: KeyEvent) {
        let Popup::Types(p) = &mut self.popup else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let visible = p.visible();
        match key.code {
            KeyCode::Esc => self.popup = Popup::None,
            KeyCode::Enter => {
                let types: Vec<String> = p.selected.iter().cloned().collect();
                self.popup = Popup::None;
                if types != self.folder.types {
                    self.update_folder(|f| f.types = types.clone());
                }
            }
            KeyCode::Up => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Down => p.cursor = (p.cursor + 1).min(visible.len().saturating_sub(1)),
            KeyCode::PageUp => p.cursor = p.cursor.saturating_sub(10),
            KeyCode::PageDown => p.cursor = (p.cursor + 10).min(visible.len().saturating_sub(1)),
            KeyCode::Char('a') if ctrl && !alt => p.selected.clear(),
            KeyCode::Char(' ') => {
                if let Some(&i) = visible.get(p.cursor) {
                    let name = p.all[i].0.clone();
                    if !p.selected.remove(&name) {
                        p.selected.insert(name);
                    }
                }
            }
            KeyCode::Char(c) if ctrl == alt => {
                p.filter.push(c.to_ascii_lowercase());
                p.cursor = 0;
                p.offset = 0;
            }
            KeyCode::Backspace => {
                p.filter.pop();
                p.cursor = 0;
                p.offset = 0;
            }
            _ => {}
        }
    }

    pub fn setting_value(&self, i: usize) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" }.to_string();
        match i {
            0 => match self.folder.mode {
                Mode::Literal => "literal".into(),
                Mode::Regex => "regex".into(),
            },
            1 => match self.cfg.case {
                Case::Smart => "smart".into(),
                Case::Sensitive => "sensitive".into(),
                Case::Insensitive => "insensitive".into(),
            },
            2 => on_off(self.folder.hidden),
            3 => on_off(self.folder.no_ignore),
            4 => match self.cfg.list_mode {
                ListMode::File => "per file".into(),
                ListMode::Match => "per match".into(),
            },
            5 => on_off(self.cfg.wrap),
            6 => if self.cfg.quit_after_open { "quit" } else { "stay open" }.into(),
            7 => self.cfg.max_file_size_mb.to_string(),
            8 => self.cfg.max_files.to_string(),
            9 => self.cfg.min_query_len.to_string(),
            10 => self.cfg.theme.clone(),
            11 => self.cfg.editor_command.clone(),
            12 => if self.folder_saved { "press Enter" } else { "(using defaults)" }.into(),
            13 => if self.store.writable { "ok" } else { "INVALID - Enter overwrites it" }.into(),
            _ => String::new(),
        }
    }

    fn change_setting(&mut self, i: usize, dir: i64) {
        match i {
            0 => {
                let mode = if self.folder.mode == Mode::Literal { Mode::Regex } else { Mode::Literal };
                self.update_folder(|f| f.mode = mode);
            }
            2 => {
                let v = !self.folder.hidden;
                self.update_folder(|f| f.hidden = v);
            }
            3 => {
                let v = !self.folder.no_ignore;
                self.update_folder(|f| f.no_ignore = v);
            }
            4 => self.toggle_list_mode(),
            12 => {
                if self.folder_saved {
                    self.cfg.folders.remove(&self.folder_key);
                    self.saved_folder = self.cfg.global_folder_defaults();
                    self.folder = self.saved_folder.clone();
                    self.folder_saved = false;
                    self.cfg_dirty = true;
                    self.save_config();
                    self.start_search();
                }
            }
            13 => {
                if !self.store.writable {
                    self.store.writable = true;
                    self.cfg_dirty = true;
                    self.save_config();
                    self.message = Some("config file overwritten".into());
                }
            }
            _ => self.change_global(i, dir),
        }
    }

    fn change_global(&mut self, i: usize, dir: i64) {
        match i {
            1 => {
                let all = [Case::Smart, Case::Sensitive, Case::Insensitive];
                let cur = all.iter().position(|c| *c == self.cfg.case).unwrap_or(0) as i64;
                self.cfg.case = all[(cur + dir).rem_euclid(3) as usize];
            }
            5 => self.toggle_wrap(),
            6 => self.cfg.quit_after_open = !self.cfg.quit_after_open,
            7 => self.cfg.max_file_size_mb = (self.cfg.max_file_size_mb as i64 + dir).max(1) as u64,
            8 => self.cfg.max_files = (self.cfg.max_files as i64 + dir * 1000).max(100) as usize,
            9 => self.cfg.min_query_len = (self.cfg.min_query_len as i64 + dir).max(1) as usize,
            10 => {
                let names = crate::preview::theme_names();
                if names.is_empty() {
                    return;
                }
                let cur = names.iter().position(|n| *n == self.cfg.theme).unwrap_or(0) as i64;
                self.cfg.theme = names[(cur + dir).rem_euclid(names.len() as i64) as usize].clone();
            }
            _ => return,
        }
        self.cfg_dirty = true;
        if matches!(i, 1 | 7 | 8 | 9) {
            self.start_search();
        }
    }

    fn apply_edit(&mut self, i: usize, text: &str) {
        let text = text.trim();
        let num = text.parse::<u64>();
        match (i, num) {
            (7, Ok(n)) if n > 0 => self.cfg.max_file_size_mb = n,
            (8, Ok(n)) if n > 0 => self.cfg.max_files = n as usize,
            (9, Ok(n)) if n > 0 => self.cfg.min_query_len = n as usize,
            (11, _) if !text.is_empty() => self.cfg.editor_command = text.to_string(),
            _ => {
                self.message = Some(format!("invalid value: {text}"));
                return;
            }
        }
        self.cfg_dirty = true;
        if i != 11 {
            self.start_search();
        }
    }

    fn key_settings(&mut self, key: KeyEvent) {
        let Popup::Settings(s) = &mut self.popup else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let cursor = s.cursor;
        if let Some(buf) = &mut s.editing {
            match key.code {
                KeyCode::Esc => s.editing = None,
                KeyCode::Enter => {
                    let text = std::mem::take(buf);
                    s.editing = None;
                    self.apply_edit(cursor, &text);
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if ctrl == alt => buf.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.popup = Popup::None;
                self.save_config();
            }
            KeyCode::Up => s.cursor = s.cursor.saturating_sub(1),
            KeyCode::Down => s.cursor = (s.cursor + 1).min(SETTINGS.len() - 1),
            KeyCode::Left if !VALUE_SETTINGS.contains(&cursor) => self.change_setting(cursor, -1),
            KeyCode::Right | KeyCode::Char(' ') if !VALUE_SETTINGS.contains(&cursor) => {
                self.change_setting(cursor, 1)
            }
            KeyCode::Enter => match cursor {
                7..=9 | 11 => {
                    let v = self.setting_value(cursor);
                    if let Popup::Settings(s) = &mut self.popup {
                        s.editing = Some(v);
                    }
                }
                _ => self.change_setting(cursor, 1),
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::temp_dir;
    use ratatui::crossterm::event::KeyEvent;
    use std::path::Path;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn app_in(dir: &Path, store: Store, cfg: Config, types: Vec<String>, regex: bool) -> App {
        App::new(cfg, store, dir.to_path_buf(), dir.to_path_buf(), types, regex)
    }

    fn no_store() -> Store {
        Store { path: None, writable: true }
    }

    fn search(app: &mut App, q: &str) {
        app.query = q.into();
        app.cursor = q.len();
        app.start_search();
        let start = Instant::now();
        while matches!(app.status, Status::Searching) && start.elapsed().as_secs() < 10 {
            app.tick();
        }
    }

    fn file(rel: &str) -> FileResult {
        let m = search::build_matcher("x", Mode::Literal, Case::Smart).unwrap();
        FileResult { rel: rel.into(), matches: search::occurrences(&m, 1, "x"), truncated: false }
    }

    #[test]
    fn next_match_selects_the_second_occurrence_on_a_line() {
        let dir = temp_dir("occ");
        std::fs::write(dir.join("a.txt"), "foo foo\n").unwrap();
        let mut app = app_in(&dir, no_store(), Config::default(), vec![], false);
        search(&mut app, "foo");
        assert_eq!(app.total_matches, 2);
        assert_eq!(app.editor_target().unwrap().2, 1);
        app.on_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        let (_, line, col) = app.editor_target().unwrap();
        assert_eq!((line, col), (1, 5));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_search_cannot_replace_a_validation_error() {
        let dir = temp_dir("stale");
        std::fs::write(dir.join("a.txt"), "foo\n").unwrap();
        let cfg = Config { mode: Mode::Regex, ..Default::default() };
        let mut app = app_in(&dir, no_store(), cfg, vec![], false);
        search(&mut app, "foo");
        assert_eq!(app.files.len(), 1);

        // A new valid search shows rows, then the query becomes an invalid regex.
        app.query = "fo".into();
        app.start_search();
        let old_generation = app.generation;
        let start = Instant::now();
        while app.files.is_empty() && start.elapsed().as_secs() < 10 {
            app.tick();
        }
        app.query = "foo(".into();
        app.start_search();
        assert!(matches!(app.status, Status::Error(_)));
        let shown = app.files.len();
        assert_eq!(shown, 1);

        // Late messages from the old search arrive afterwards.
        app.tx
            .send(Msg::File { generation: old_generation, file: file("late.txt") })
            .unwrap();
        app.tx
            .send(Msg::Done { generation: old_generation, elapsed: Duration::ZERO, limit_reached: false })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        app.tick();
        assert!(matches!(app.status, Status::Error(_)), "error stays visible");
        assert_eq!(app.files.len(), shown, "rows are unchanged");
        assert!(app.files.iter().all(|f| f.rel != Path::new("late.txt")));
        assert!(app.matcher.is_some(), "matcher of the shown results is kept");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_esc_leaves_malformed_config_untouched() {
        let dir = temp_dir("badcfg");
        let path = dir.join("config.toml");
        let original = b"wrap = [broken\n";
        std::fs::write(&path, original).unwrap();
        let (cfg, store, _) = Config::load_from(Some(path.clone()));
        let mut app = app_in(&dir, store, cfg, vec![], false);

        app.on_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        app.on_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        // Even a real change is not written while the file is invalid...
        app.on_key(key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        // ...until the user explicitly overwrites it.
        app.on_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        if let Popup::Settings(s) = &mut app.popup {
            s.cursor = 13;
        }
        app.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        let (cfg, store, _) = Config::load_from(Some(path.clone()));
        assert!(store.writable);
        assert_eq!(cfg.list_mode, ListMode::Match);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unchanged_settings_are_not_written() {
        let dir = temp_dir("cleancfg");
        let path = dir.join("config.toml");
        let mut app = app_in(&dir, Store { path: Some(path.clone()), writable: true }, Config::default(), vec![], false);
        app.on_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        app.on_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cli_overrides_are_not_persisted() {
        let dir = temp_dir("cli");
        let path = dir.join("config.toml");
        let store = || Store { path: Some(path.clone()), writable: true };
        let mut app = app_in(&dir, store(), Config::default(), vec!["cs".into()], true);
        assert_eq!(app.folder.mode, Mode::Regex);
        app.on_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        if let Popup::Settings(s) = &mut app.popup {
            s.cursor = 2; // Include hidden
        }
        app.on_key(key(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.folder.hidden && app.folder.mode == Mode::Regex, "override still active this run");
        app.on_key(key(KeyCode::Esc, KeyModifiers::NONE));

        // Restart without flags.
        let (cfg, st, _) = Config::load_from(Some(path.clone()));
        assert!(st.writable);
        let app = app_in(&dir, store(), cfg, vec![], false);
        assert!(app.folder.hidden, "explicit change is remembered");
        assert_eq!(app.folder.mode, Mode::Literal, "--regex is not");
        assert!(app.folder.types.is_empty(), "-t is not");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn streaming_results_keep_the_selected_occurrence() {
        let dir = temp_dir("stream");
        let mut app = app_in(&dir, no_store(), Config::default(), vec![], false);
        app.insert_file(file("z.rs"));
        app.insert_file(file("a.rs"));
        assert_eq!(app.selected().unwrap().0.rel, Path::new("z.rs"));
        app.insert_file(file("b.rs"));
        assert_eq!(app.selected().unwrap().0.rel, Path::new("z.rs"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn user_selection_beats_previous_search_selection() {
        let dir = temp_dir("sticky");
        let mut app = app_in(&dir, no_store(), Config::default(), vec![], false);
        app.sticky = Some((PathBuf::from("m.rs"), 1, 0));
        app.insert_file(file("a.rs"));
        app.insert_file(file("b.rs"));
        app.on_key(key(KeyCode::Down, KeyModifiers::NONE)); // user picks b.rs
        app.insert_file(file("m.rs"));
        assert_eq!(app.selected().unwrap().0.rel, Path::new("b.rs"));

        // Without user input, the previous selection is restored when it arrives.
        let mut app = app_in(&dir, no_store(), Config::default(), vec![], false);
        app.sticky = Some((PathBuf::from("m.rs"), 1, 0));
        app.insert_file(file("a.rs"));
        app.insert_file(file("m.rs"));
        assert_eq!(app.selected().unwrap().0.rel, Path::new("m.rs"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrapped_long_line_is_scrollable_to_its_middle() {
        let dir = temp_dir("wrap");
        let long = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        std::fs::write(dir.join("long.txt"), format!("first\n{long}\nlast\n")).unwrap();
        let cfg = Config { wrap: true, ..Default::default() };
        let mut app = app_in(&dir, no_store(), cfg, vec![], false);
        search(&mut app, "needle");
        // Text column of 20 cells, 6 rows high: the line needs 31 rows.
        app.sync_preview(6, 20 + 3 + 3);
        assert_eq!(app.preview_text_w, 20);
        assert_eq!(app.preview_top, 1);
        assert_eq!(app.preview_sub, 300 / 20 - 3, "match row is centred inside the long line");
        let before = app.preview_sub;
        app.scroll_preview(5);
        assert_eq!((app.preview_top, app.preview_sub), (1, before + 5));
        app.scroll_preview(100);
        assert_eq!((app.preview_top, app.preview_sub), (2, 0), "stops at the last line");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

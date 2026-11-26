//! User configuration stored in `~/.rgg/config.toml`, including per-folder settings.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Literal,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ListMode {
    #[default]
    File,
    Match,
}

/// What AND / NOT in a search expression apply to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// All terms on the same line.
    #[default]
    Line,
    /// All terms somewhere in the same file.
    File,
}

/// Settings remembered per folder (keyed by git root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FolderSettings {
    pub types: Vec<String>,
    pub hidden: bool,
    pub no_ignore: bool,
    pub mode: Mode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    pub case: Case,
    /// Scope of AND / NOT in search expressions.
    pub scope: Scope,
    pub types: Vec<String>,
    pub hidden: bool,
    pub no_ignore: bool,
    pub list_mode: ListMode,
    pub group_by_repo: bool,
    pub wrap: bool,
    pub quit_after_open: bool,
    pub max_file_size_mb: u64,
    pub max_files: usize,
    pub min_query_len: usize,
    /// Columns Alt+←/→ moves the list/preview split.
    pub resize_step: u16,
    /// Search history entries kept per repository; 0 turns history off.
    pub history_size: usize,
    pub theme: String,
    pub editor_command: String,
    pub folders: BTreeMap<String, FolderSettings>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Literal,
            case: Case::Smart,
            scope: Scope::Line,
            types: Vec::new(),
            hidden: false,
            no_ignore: false,
            list_mode: ListMode::File,
            group_by_repo: true,
            wrap: false,
            quit_after_open: false,
            max_file_size_mb: 10,
            max_files: 10_000,
            min_query_len: 2,
            resize_step: 2,
            history_size: 100,
            theme: "base16-ocean.dark".into(),
            editor_command: "code {root} -g {file}:{line}:{col}".into(),
            folders: BTreeMap::new(),
        }
    }
}

/// Where the config lives and whether rgg may write to it.
#[derive(Debug, Clone)]
pub struct Store {
    /// `None` disables saving (used by tests).
    pub path: Option<PathBuf>,
    /// False when the existing file could not be read or parsed: it is kept
    /// untouched until the user explicitly chooses to overwrite it.
    pub writable: bool,
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".rgg").join("config.toml"))
    }

    /// Loads the config. A missing file yields defaults; an unreadable or malformed
    /// file yields defaults, a non-writable store and a warning.
    pub fn load_from(path: Option<PathBuf>) -> (Self, Store, Option<String>) {
        let Some(path) = path else {
            return (Self::default(), Store { path: None, writable: true }, None);
        };
        let broken = |why: String| {
            let msg = format!(
                "config {} is invalid ({why}); changes won't be saved (Ctrl+O → Config file to overwrite)",
                path.display()
            );
            (Self::default(), Store { path: Some(path.clone()), writable: false }, Some(msg))
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(cfg) => (cfg, Store { path: Some(path.clone()), writable: true }, None),
                Err(e) => broken(first_line(&e.to_string()).to_string()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), Store { path: Some(path.clone()), writable: true }, None)
            }
            Err(e) => broken(e.to_string()),
        }
    }

    /// Writes the config atomically (temp file + rename).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }

    pub fn global_folder_defaults(&self) -> FolderSettings {
        FolderSettings {
            types: self.types.clone(),
            hidden: self.hidden,
            no_ignore: self.no_ignore,
            mode: self.mode,
        }
    }
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s)
}

/// Canonical path without the Windows `\\?\` prefix.
pub fn clean_canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        c
    }
}

/// Nearest ancestor (including `start`) that contains `.git`; falls back to `start`.
pub fn git_root(start: &Path) -> PathBuf {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    start.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_with_folders() {
        let mut cfg = Config::default();
        cfg.folders.insert(
            r"D:\Foo".into(),
            FolderSettings { types: vec!["cs".into()], mode: Mode::Regex, ..Default::default() },
        );
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.folders[r"D:\Foo"].types, vec!["cs".to_string()]);
        assert_eq!(back.folders[r"D:\Foo"].mode, Mode::Regex);
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg: Config = toml::from_str("wrap = true").unwrap();
        assert!(cfg.wrap);
        assert_eq!(cfg.max_files, 10_000);
    }

    #[test]
    fn malformed_config_is_not_writable_and_save_is_atomic() {
        let dir = crate::search::temp_dir("cfgload");
        let path = dir.join("config.toml");
        std::fs::write(&path, "wrap = [broken").unwrap();
        let (_, store, warn) = Config::load_from(Some(path.clone()));
        assert!(!store.writable);
        assert!(warn.is_some());

        let missing = dir.join("missing.toml");
        let (_, store, warn) = Config::load_from(Some(missing.clone()));
        assert!(store.writable && warn.is_none());
        Config::default().save_to(&missing).unwrap();
        assert!(missing.exists() && !dir.join("missing.toml.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_root_finds_ancestor() {
        let base = std::env::temp_dir().join(format!("rgg-gitroot-{}", std::process::id()));
        let nested = base.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        assert_eq!(git_root(&nested), base);
        std::fs::remove_dir_all(&base).unwrap();
    }
}

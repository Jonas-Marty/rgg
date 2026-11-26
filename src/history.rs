//! Search history, kept per repository in its own file next to the config.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub query: String,
    #[serde(default)]
    pub regex: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    /// Folder key (git root) → entries, newest first.
    #[serde(default)]
    folders: BTreeMap<String, Vec<Entry>>,
}

pub fn default_path() -> Option<PathBuf> {
    crate::config::Config::default_path().map(|p| p.with_file_name("history.toml"))
}

/// Entries for `folder`, newest first. A missing or unreadable file yields none.
pub fn load(path: Option<&Path>, folder: &str) -> Vec<Entry> {
    path.map(read).and_then(|mut f| f.folders.remove(folder)).unwrap_or_default()
}

fn read(path: &Path) -> File {
    std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

/// Puts `entry` first in `list`, dropping an older copy and anything beyond `limit`.
pub fn push(list: &mut Vec<Entry>, entry: Entry, limit: usize) {
    list.retain(|e| *e != entry);
    list.insert(0, entry);
    list.truncate(limit);
}

/// Changes the stored list of `folder`. The file is re-read first so other
/// running instances don't lose their entries. Returns the new list.
pub fn update(path: &Path, folder: &str, change: impl FnOnce(&mut Vec<Entry>)) -> Result<Vec<Entry>, String> {
    let mut file = read(path);
    let list = file.folders.entry(folder.to_string()).or_default();
    change(list);
    let result = list.clone();
    if result.is_empty() {
        file.folders.remove(folder);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(q: &str) -> Entry {
        Entry { query: q.into(), regex: false }
    }

    #[test]
    fn push_dedupes_and_caps() {
        let mut l = vec![e("a"), e("b"), e("c")];
        push(&mut l, e("c"), 3);
        assert_eq!(l, vec![e("c"), e("a"), e("b")]);
        push(&mut l, e("d"), 3);
        assert_eq!(l, vec![e("d"), e("c"), e("a")]);
        push(&mut l, Entry { query: "d".into(), regex: true }, 3);
        assert_eq!(l.len(), 3, "same text in regex mode is a separate entry");
    }

    #[test]
    fn folders_are_separate_and_merged_on_write() {
        let dir = crate::search::temp_dir("history");
        let path = dir.join("history.toml");
        update(&path, r"D:\a", |l| push(l, e("one"), 10)).unwrap();
        update(&path, r"D:\b", |l| push(l, e("two"), 10)).unwrap();
        assert_eq!(load(Some(&path), r"D:\a"), vec![e("one")]);
        assert_eq!(load(Some(&path), r"D:\b"), vec![e("two")]);
        update(&path, r"D:\a", |l| l.clear()).unwrap();
        assert!(load(Some(&path), r"D:\a").is_empty());
        assert_eq!(load(Some(&path), r"D:\b"), vec![e("two")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

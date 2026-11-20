//! Launching the external editor from the configurable command template.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Splits the template into arguments first, then substitutes placeholders,
/// so paths containing spaces stay a single argument.
pub fn build_args(template: &str, root: &Path, file: &Path, line: u64, col: usize) -> Vec<String> {
    template
        .split_whitespace()
        .map(|arg| {
            arg.replace("{root}", &root.to_string_lossy())
                .replace("{file}", &file.to_string_lossy())
                .replace("{line}", &line.to_string())
                .replace("{col}", &col.to_string())
        })
        .collect()
}

pub fn open(template: &str, root: &Path, file: &Path, line: u64, col: usize) -> Result<(), String> {
    let args = build_args(template, root, file, line, col);
    let Some((prog, rest)) = args.split_first() else {
        return Err("editor_command is empty".into());
    };
    let exe = resolve(prog).unwrap_or_else(|| PathBuf::from(prog));
    Command::new(&exe)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start {prog}: {e}"))
}

/// Finds a program on PATH, honouring PATHEXT on Windows (so `code` resolves to `code.cmd`).
fn resolve(prog: &str) -> Option<PathBuf> {
    let p = Path::new(prog);
    if p.components().count() > 1 {
        return Some(p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if p.extension().is_some() {
            let cand = dir.join(prog);
            if cand.is_file() {
                return Some(cand);
            }
        }
        for ext in &exts {
            let cand = dir.join(format!("{prog}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_placeholders_per_argument() {
        let args = build_args(
            "code {root} -g {file}:{line}:{col}",
            Path::new("D:/My Repo"),
            Path::new("D:/My Repo/src/a b.cs"),
            12,
            5,
        );
        assert_eq!(args, vec!["code", "D:/My Repo", "-g", "D:/My Repo/src/a b.cs:12:5"]);
    }
}

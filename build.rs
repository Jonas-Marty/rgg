//! Embeds the git commit into the binary as `RGG_GIT_COMMIT` (e.g. `0f2f72b` or `0f2f72b-dirty`).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    // Re-run when the checked-out commit, the index or the sources change,
    // so both the hash and the "-dirty" marker stay current.
    for path in [".git/HEAD", ".git/index", ".git/refs/heads", "src", "Cargo.toml"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(hash) if !hash.is_empty() => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{hash}-dirty") } else { hash }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=RGG_GIT_COMMIT={commit}");
}

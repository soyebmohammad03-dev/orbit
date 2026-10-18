//! Records the Git commit the lab was built from, if it can be determined.
//!
//! `ORBIT_GIT_COMMIT` overrides detection (for builds without a `.git`
//! directory). Otherwise `git rev-parse HEAD` is run in the workspace root.
//! If neither works, no commit is recorded and the lab reports `null`.
//! Uncommitted changes are not detected.

use std::path::Path;
use std::process::Command;

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ORBIT_GIT_COMMIT");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root = Path::new(&manifest).join("../..");
    for file in [".git/HEAD", ".git/logs/HEAD"] {
        let path = root.join(file);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let commit = std::env::var("ORBIT_GIT_COMMIT")
        .ok()
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| is_commit(c))
        .or_else(|| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()?;
            let text = String::from_utf8(output.stdout).ok()?;
            let text = text.trim().to_ascii_lowercase();
            (output.status.success() && is_commit(&text)).then_some(text)
        });
    if let Some(commit) = commit {
        println!("cargo:rustc-env=ORBIT_LAB_GIT_COMMIT={commit}");
    }
}

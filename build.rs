use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=LOCHO_EXPECTED_COMMIT");
    println!("cargo:rerun-if-env-changed=LOCHO_EXPECTED_DIRTY");

    let commit = git_output(["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git_output(["status", "--porcelain", "--untracked-files=normal"]) {
        Some(status) if status.is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };

    if let Ok(expected_commit) = env::var("LOCHO_EXPECTED_COMMIT") {
        if !is_commit_hash(&expected_commit) {
            panic!("LOCHO_EXPECTED_COMMIT must be a 40-character hexadecimal commit hash");
        }
        if commit != expected_commit {
            panic!("release build commit mismatch: expected {expected_commit}, got {commit}");
        }
    }
    if let Ok(expected_dirty) = env::var("LOCHO_EXPECTED_DIRTY") {
        if expected_dirty != "clean" {
            panic!("LOCHO_EXPECTED_DIRTY must be clean when set");
        }
        if dirty != "clean" {
            panic!("release build requires a clean checkout, got {dirty}");
        }
    }

    println!("cargo:rustc-env=LOCHO_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=LOCHO_GIT_DIRTY={dirty}");
}

fn is_commit_hash(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn git_output<const N: usize>(arguments: [&str; N]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

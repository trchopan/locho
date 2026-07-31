use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let commit = git_output(["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git_output(["status", "--porcelain", "--untracked-files=normal"]) {
        Some(status) if status.is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };

    println!("cargo:rustc-env=LOCHO_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=LOCHO_GIT_DIRTY={dirty}");
}

fn git_output<const N: usize>(arguments: [&str; N]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

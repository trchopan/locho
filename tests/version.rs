use std::process::Command;

#[test]
fn reports_package_version_and_build_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_locho"))
        .arg("--version")
        .output()
        .expect("failed to run locho");

    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).expect("version output was not UTF-8");
    assert!(version.starts_with(concat!("locho ", env!("CARGO_PKG_VERSION"), " (commit ")));
    assert!(version.ends_with(", clean)\n") || version.ends_with(", dirty)\n"));
}

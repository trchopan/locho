use std::process::Command;

#[test]
fn reports_package_version_and_build_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_locho"))
        .arg("--version")
        .output()
        .expect("failed to run locho");

    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).expect("version output was not UTF-8");
    let expected = format!(
        "locho {} (commit {}, {})\n",
        env!("CARGO_PKG_VERSION"),
        env!("LOCHO_GIT_COMMIT"),
        env!("LOCHO_GIT_DIRTY")
    );
    assert_eq!(version, expected);
}

use std::process::Command;

#[test]
fn version_output_is_single_parseable_line_when_piped() {
    let output = Command::new(env!("CARGO_BIN_EXE_fj"))
        .arg("--version")
        .output()
        .expect("run fj --version");

    assert!(output.status.success(), "status was {}", output.status);

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let line = stdout
        .strip_suffix('\n')
        .expect("stdout should end with one newline");
    assert!(
        !line.contains('\n'),
        "stdout should contain exactly one line, got {stdout:?}"
    );

    let mut parts = line.split(' ');
    assert_eq!(parts.next(), Some("fj"));
    let version = parts.next().expect("version field");
    assert_eq!(
        parts.next(),
        None,
        "stdout should be exactly `fj <version>`"
    );
    assert_semver_core(version);
}

fn assert_semver_core(version: &str) {
    let mut parts = version.split('.');
    for label in ["major", "minor", "patch"] {
        let part = parts.next().unwrap_or_else(|| panic!("missing {label}"));
        assert!(!part.is_empty(), "{label} should not be empty");
        assert!(
            part.chars().all(|ch| ch.is_ascii_digit()),
            "{label} should be numeric, got {part:?}"
        );
    }
    assert_eq!(parts.next(), None, "version should have three components");
}

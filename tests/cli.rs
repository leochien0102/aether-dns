use std::process::{Command, Output};

fn run(arg: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether-dns"))
        .arg(arg)
        .output()
        .expect("run aether-dns")
}

// The banner parser reads only the first line and its second whitespace-delimited field.
fn parse_version(text: &str) -> Option<&str> {
    text.lines().next()?.split_whitespace().nth(1)
}

// Dotted-numeric subset of the proxy's version comparison, sufficient for D4's date version.
fn date_version(version: &str) -> Option<Vec<u32>> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    parts.into_iter().map(|p| p.parse().ok()).collect()
}

#[test]
fn cli_version_passes_proxy_minimum() {
    for arg in ["-V", "--version"] {
        let output = run(arg);
        assert!(output.status.success());
        let banner = std::str::from_utf8(&output.stdout).unwrap();
        assert_eq!(banner.lines().next(), Some("aether-dns 2026.11.04"));
        let version = parse_version(banner).unwrap();
        assert!(date_version(version).unwrap() >= date_version("2025.08.09").unwrap());
        println!("{arg}: {version} >= 2025.08.09");
    }
}

#[test]
fn cli_parse_version_bad_formats() {
    assert_eq!(parse_version("\nprogram 2026.11.04"), None);
    assert_eq!(parse_version("program"), None);
    assert_eq!(
        parse_version("  program\t2026.11.04 extra\nignored 1.0"),
        Some("2026.11.04")
    );
    for bad in ["", "2026", "2026..04", "2026.11.x", "2026.11.04!"] {
        assert_eq!(date_version(bad), None, "{bad}");
    }
}

#[test]
fn cli_help_and_missing_arguments() {
    for arg in ["-h", "--help"] {
        let output = run(arg);
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("usage"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_aether-dns"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr).unwrap().contains("usage"));
}

use std::fs;
use std::process::{Command, Stdio};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rep"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("REP_") {
            command.env_remove(key);
        }
    }
    command.env("REP_CONFIG_PATH", "").stdin(Stdio::null());
    command
}

#[test]
fn naturally_ordered_listing_deduplicates_overlapping_roots() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["file10.txt", "file2.txt", "file1.txt"] {
        fs::write(dir.path().join(name), "foo").unwrap();
    }
    let output = command()
        .args(["-l", "foo", "bar", ".", "file2.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"file1.txt\nfile2.txt\nfile10.txt\n");
}

#[test]
fn overlapping_roots_apply_each_destination_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.txt");
    fs::write(&path, "a").unwrap();
    let output = command()
        .args(["-W", "-c", "a", "aa", ".", "data.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(fs::read(path).unwrap(), b"aa");
}

#[test]
fn missing_input_does_not_prevent_other_requested_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.txt");
    fs::write(&path, "foo").unwrap();
    let output = command()
        .args(["-W", "-c", "foo", "bar", "missing.txt", "data.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(fs::read(path).unwrap(), b"bar");
}

#[test]
fn malformed_ignore_file_warns_without_failing_successful_writes() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".gitignore"), "{a,b\n").unwrap();
    let path = dir.path().join("data.txt");
    fs::write(&path, "foo\n").unwrap();
    let output = command()
        .args(["-W", "-c", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(fs::read(path).unwrap(), b"bar\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("Warning: "));
    assert!(stderr.contains(".gitignore"));
    assert!(!stderr.contains("could not be processed"));
}

#[cfg(unix)]
#[test]
fn quiet_dry_run_still_reports_unreadable_inputs() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unreadable.txt");
    fs::write(&path, "foo").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read(&path).is_ok() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        return;
    }
    let output = command()
        .args(["--dry-run", "--quiet", "foo", "bar"])
        .arg(&path)
        .output()
        .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Unable to read")
    );
}

#[cfg(unix)]
#[test]
fn symlink_alias_is_deduplicated_and_retained() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("data.txt"), "a").unwrap();
    std::os::unix::fs::symlink("data.txt", dir.path().join("alias.txt")).unwrap();
    let output = command()
        .args(["-W", "-c", "a", "aa", "alias.txt", "data.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(fs::read(dir.path().join("data.txt")).unwrap(), b"aa");
    assert!(dir.path().join("alias.txt").is_symlink());
}

#[test]
fn global_hyperlink_limit_uses_final_total_including_later_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "foo\n").unwrap();
    fs::write(dir.path().join("b.txt"), "foo foo\n").unwrap();
    for (limit, linked) in [("0", true), ("2", false), ("3", true), ("4", true)] {
        let output = command()
            .args([
                "--color",
                "always",
                "--hyperlink-format",
                "vscode",
                "--hyperlink-limit",
                limit,
                "foo",
                "bar",
                ".",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout.windows(4).any(|w| w == b"\x1b]8;"),
            linked,
            "limit {limit}"
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.find("a.txt").unwrap() < text.find("b.txt").unwrap());
        assert!(text.contains("Would perform 3 replacements in 2 files"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_survive_argument_parsing_and_replacement() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"name-\xff.txt"));
    fs::write(&path, b"foo\n").unwrap();
    let output = command()
        .args(["-W", "-c", "foo", "bar"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(fs::read(path).unwrap(), b"bar\n");
}

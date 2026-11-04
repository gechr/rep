//! File and stream behavior at the executable boundary.

use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};

use tempfile::tempdir;

fn rep() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rep"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("REP_") {
            command.env_remove(name);
        }
    }
    command.env("REP_CONFIG_PATH", "").stdin(Stdio::null());
    command
}

#[test]
fn identity_count_matches_for_files_and_stdin() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("data.txt");
    fs::write(&path, b"foo\n").unwrap();
    let from_file = rep()
        .args(["--count", "--regex", "(foo)", "$1"])
        .arg(&path)
        .output()
        .unwrap();
    let mut from_stdin = rep()
        .args(["--count", "--regex", "(foo)", "$1", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    from_stdin
        .stdin
        .take()
        .unwrap()
        .write_all(b"foo\n")
        .unwrap();
    let from_stdin = from_stdin.wait_with_output().unwrap();
    assert!(from_file.status.success());
    assert!(from_stdin.status.success());
    assert_eq!(from_file.stdout, b"0\n");
    assert_eq!(from_stdin.stdout, from_file.stdout);
}

#[test]
fn hard_link_rewrite_changes_only_named_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("data.txt");
    let alias = dir.path().join("alias.txt");
    fs::write(&path, b"foo\n").unwrap();
    fs::hard_link(&path, &alias).unwrap();
    let output = rep()
        .args(["--write", "foo", "bar"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(fs::read(path).unwrap(), b"bar\n");
    assert_eq!(fs::read(alias).unwrap(), b"foo\n");
}

#[test]
fn directory_rewrite_updates_both_selected_hard_links() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    let original = "foo\n".repeat(20_000);
    fs::write(&first, &original).unwrap();
    fs::hard_link(&first, &second).unwrap();
    let output = rep()
        .args(["--write", "--quiet", "foo", "bar"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let expected = "bar\n".repeat(20_000);
    assert_eq!(fs::read_to_string(first).unwrap(), expected);
    assert_eq!(fs::read_to_string(second).unwrap(), expected);
}

#[cfg(unix)]
#[test]
fn unreadable_file_fails_without_hiding_other_writes() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempdir().unwrap();
    let unreadable = dir.path().join("unreadable.txt");
    let writable = dir.path().join("writable.txt");
    fs::write(&unreadable, b"foo\n").unwrap();
    fs::write(&writable, b"foo\n").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o0)).unwrap();
    // Privileged test runners can bypass ordinary access permissions.
    if fs::File::open(&unreadable).is_ok() {
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600)).unwrap();
        return;
    }
    let output = rep()
        .args(["--write", "foo", "bar"])
        .arg(&unreadable)
        .arg(&writable)
        .output()
        .unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
    assert_eq!(fs::read(unreadable).unwrap(), b"foo\n");
    assert_eq!(fs::read(writable).unwrap(), b"bar\n");
}

#[cfg(target_os = "linux")]
#[test]
fn failed_output_exits_nonzero_after_completing_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("data.txt");
    fs::write(&path, b"foo\n").unwrap();
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let output = rep()
        .args(["--write", "foo", "bar"])
        .arg(&path)
        .stdout(full)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read(path).unwrap(), b"bar\n");
}

#[test]
fn closed_output_pipe_does_not_interrupt_requested_writes() {
    let dir = tempdir().unwrap();
    let contents = format!("foo {}\n", "x".repeat(1024)).repeat(200);
    for index in 0..4 {
        fs::write(dir.path().join(format!("file-{index}.txt")), &contents).unwrap();
    }
    let mut child = rep()
        .args(["--write", "foo", "bar"])
        .arg(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected = format!("bar {}\n", "x".repeat(1024)).repeat(200);
    for index in 0..4 {
        assert_eq!(
            fs::read_to_string(dir.path().join(format!("file-{index}.txt"))).unwrap(),
            expected
        );
    }
}

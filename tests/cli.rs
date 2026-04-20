//! End-to-end tests for the `rep` binary.
//!
//! These complement the unit tests in `src/main.rs` by exercising the
//! orchestrators in `run_walk_and_apply`, `run_list_files`, and `run_stdin` -
//! the glue code (walk → pre-filter → apply → write-back → summary) that
//! string-level unit tests don't reach. The built binary is located via
//! Cargo's `CARGO_BIN_EXE_rep` env var, so no `assert_cmd`-style dev-dep is
//! needed.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::tempdir;

const REP: &str = env!("CARGO_BIN_EXE_rep");

fn write(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn basic_replace_rewrites_file_contents() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "the foo jumped over the foo");

    // Pass `.` explicitly: when stdin isn't a TTY (as under `cargo test`),
    // `rep` would otherwise enter stdin mode because `paths.is_empty()`.
    let status = Command::new(REP)
        .args(["foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "the bar jumped over the bar");
}

#[test]
fn dry_run_leaves_file_untouched() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let original = "the foo jumped";
    write(&file, original);

    let status = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), original);
}

#[test]
fn list_files_prints_sorted_matching_paths() {
    let dir = tempdir().unwrap();
    // Write out of order to make sure the sort actually fires.
    write(&dir.path().join("b.txt"), "foo");
    write(&dir.path().join("a.txt"), "foo");
    write(&dir.path().join("c.txt"), "no match here");

    let output = Command::new(REP)
        .args(["-l", "foo"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "a.txt\nb.txt\n");
}

#[test]
fn list_files_respects_explicit_search_paths() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo");
    write(&dir.path().join("b.txt"), "foo");

    let output = Command::new(REP)
        .args(["-l", "foo", "a.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "a.txt\n");
}

#[test]
fn stdin_mode_writes_replaced_text_to_stdout() {
    let mut child = Command::new(REP)
        .args(["foo", "bar"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"foo foo baz")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `run_stdin` uses `print!`, not `println!`, so no trailing newline.
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "bar bar baz");
}

#[test]
fn explicit_stdin_path_reads_replaced_text_from_stdin() {
    let mut child = Command::new(REP)
        .args(["foo", "bar", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"foo foo baz")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "bar bar baz");
}

#[test]
fn explicit_help_writes_to_stdout() {
    let output = Command::new(REP).arg("-h").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage"));
    assert!(stdout.contains("--preview-tool"));
}

#[test]
fn file_glob_limits_writes_to_matching_extension() {
    let dir = tempdir().unwrap();
    let txt = dir.path().join("a.txt");
    let md = dir.path().join("b.md");
    write(&txt, "foo");
    write(&md, "foo");

    let status = Command::new(REP)
        .args(["-f", "txt", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&txt), "bar");
    assert_eq!(read(&md), "foo", "non-matching glob should be untouched");
}

#[test]
fn smart_mode_replaces_all_seven_case_variants() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    // One line per variant so the diff is readable if a mapping regresses.
    let input = "\
foo_bar
FooBar
foo-bar
FOO_BAR
fooBar
Foo-Bar
Foo_Bar
";
    let expected = "\
hello_world
HelloWorld
hello-world
HELLO_WORLD
helloWorld
Hello-World
Hello_World
";
    write(&file, input);

    let status = Command::new(REP)
        .args(["--smart", "foo_bar", "hello_world", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), expected);
}

#[test]
fn smart_mode_rejects_multiple_paths_with_clear_error() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "foo_bar");
    write(&b, "foo_bar");

    let output = Command::new(REP)
        .args(["--smart", "foo_bar", "hello_world", "a.txt", "b.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for smart mode with multiple paths"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Smart mode only supports a single path"),
        "stderr did not contain expected error: {stderr}"
    );
    // Files must be untouched when validation rejects the invocation.
    assert_eq!(read(&a), "foo_bar");
    assert_eq!(read(&b), "foo_bar");
}

#[test]
fn delete_mode_removes_matching_lines_in_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "keep\nhas foo here\nkeep too\nanother foo\ntail\n");

    let status = Command::new(REP)
        .args(["-d", "foo", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "keep\nkeep too\ntail\n");
}

#[test]
fn rewrites_file_with_invalid_utf8_preserving_non_utf8_bytes() {
    // Regression: the scan/apply/write path must stay on bytes end-to-end so
    // files containing invalid UTF-8 (e.g. latin-1, binary-adjacent text) are
    // rewritten in place without mangling the non-UTF-8 bytes around the match.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let input: &[u8] = b"pre\xfffoo\xfepost\n";
    fs::write(&file, input).unwrap();

    let status = Command::new(REP)
        .args(["foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    let after = fs::read(&file).unwrap();
    assert_eq!(after, b"pre\xffbar\xfepost\n");
}

#[test]
fn delete_mode_with_expression_matches_raw_string_including_equals() {
    // With `-d`, `-e foo=bar` is NOT split on `=`; the whole string is
    // taken literally as the pattern to match for line deletion.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(
        &file,
        "keep\nconfig foo=bar here\nline with just foo\nline with just bar\ntail\n",
    );

    let status = Command::new(REP)
        .args(["-d", "-e", "foo=bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read(&file),
        "keep\nline with just foo\nline with just bar\ntail\n"
    );
}

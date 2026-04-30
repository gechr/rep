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
fn dry_run_prints_per_file_diffs() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "alpha foo\nkeep\nfoo tail\n");
    write(&b, "foo only\n");

    let output = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&a), "alpha foo\nkeep\nfoo tail\n");
    assert_eq!(read(&b), "foo only\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
a.txt (2)
1- alpha foo
1+ alpha bar
3- foo tail
3+ bar tail

b.txt (1)
1- foo only
1+ bar only

Would perform 3 replacements in 2 files
"
    );
}

#[test]
fn dry_run_only_highlights_changed_characters_inside_lines() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "    assert!(status.success());\n");

    let output = Command::new(REP)
        .args(["-n", "a", "b", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
a.txt (2)
1-     assert!(status.success());
1+     bssert!(stbtus.success());

Would perform 2 replacements in 1 file
"
    );
}

#[test]
fn dry_run_pairs_multiline_replacements_by_line() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "one foo\ntwo foo\nthree foo\n");

    let output = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
a.txt (3)
1- one foo
1+ one bar
2- two foo
2+ two bar
3- three foo
3+ three bar

Would perform 3 replacements in 1 file
"
    );
}

#[test]
fn dry_run_preserves_new_line_numbers_for_line_expansion() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = Command::new(REP)
        .args(["-n", "foo", "bar\nbaz", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (1)
1- foo
1+ bar
2+ baz

Would perform 1 replacement in 1 file
"
    );
}

#[test]
fn dry_run_warns_when_diff_is_not_valid_utf8() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, b"pre\xfffoo\xfepost\n").unwrap();

    let output = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (1)

Would perform 1 replacement in 1 file
"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("skipping diff (not valid UTF-8"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dry_run_with_delete_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "keep\nfoo\nkeep\n");

    let output = Command::new(REP)
        .args(["-n", "-d", "foo", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&file), "keep\nfoo\nkeep\n");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (1)
2- foo

Would perform 1 replacement in 1 file
"
    );
}

#[test]
fn dry_run_with_regex_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "hello world\n");

    let output = Command::new(REP)
        .args(["-n", "-r", r"hello (\w+)", "hi $1", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&file), "hello world\n");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (1)
1- hello world
1+ hi world

Would perform 1 replacement in 1 file
"
    );
}

#[test]
fn dry_run_with_smart_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo_bar and FooBar\n");

    let output = Command::new(REP)
        .args(["-n", "--smart", "foo_bar", "baz_qux", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&file), "foo_bar and FooBar\n");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (2)
1- foo_bar and FooBar
1+ baz_qux and BazQux

Would perform 2 replacements in 1 file
"
    );
}

#[test]
fn dry_run_two_separate_hunks_show_correct_line_numbers() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\nkeep\nfoo\n");

    let output = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
a.txt (2)
1- foo
1+ bar
3- foo
3+ bar

Would perform 2 replacements in 1 file
"
    );
}

#[test]
fn dry_run_file_with_zero_matches_does_not_appear_in_output() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "no match here\n");
    write(&b, "foo\n");

    let output = Command::new(REP)
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
b.txt (1)
1- foo
1+ bar

Would perform 1 replacement in 1 file
"
    );
}

#[test]
fn dry_run_quiet_with_zero_matches_produces_no_output() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "no match here\n");

    let output = Command::new(REP)
        .args(["-n", "-q", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}

#[test]
fn quiet_suppresses_all_replace_output() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = Command::new(REP)
        .args(["-q", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&file), "bar\n");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}

#[test]
fn quiet_suppresses_dry_run_diff() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = Command::new(REP)
        .args(["-n", "-q", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read(&file), "foo\n");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "a.txt (1)\n\nWould perform 1 replacement in 1 file\n"
    );
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
fn hidden_mode_skips_gitignored_and_vcs_paths() {
    let dir = tempdir().unwrap();
    write(&dir.path().join(".visible-hidden.txt"), "foo");
    write(&dir.path().join(".gitignore"), "ignored.txt\n");
    write(&dir.path().join("ignored.txt"), "foo");

    fs::create_dir(dir.path().join(".git")).unwrap();
    write(&dir.path().join(".git/config"), "foo");

    let status = Command::new(REP)
        .args(["--hidden", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(read(&dir.path().join(".visible-hidden.txt")), "bar");
    assert_eq!(read(&dir.path().join("ignored.txt")), "foo");
    assert_eq!(read(&dir.path().join(".git/config")), "foo");
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
fn delete_mode_with_smart_removes_all_case_variants() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(
        &file,
        "hello_world here\nHelloWorld line\nhelloWorld line\nHELLO_WORLD line\nhello-world line\nkeep me\n",
    );

    let status = Command::new(REP)
        .args(["-d", "--smart", "hello_world", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "keep me\n");
}

#[test]
fn delete_mode_with_list_files_prints_matching_paths_without_modifying() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "has foo\nother\n");
    write(&b, "nothing here\n");

    let output = Command::new(REP)
        .args(["-d", "-l", "foo", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("a.txt"), "stdout: {stdout:?}");
    assert!(!stdout.contains("b.txt"), "stdout: {stdout:?}");
    // File must be untouched - `-l` is informational.
    assert_eq!(read(&a), "has foo\nother\n");
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
    // With `-d -e <find> <replace>`, the find arg is taken literally; patterns
    // containing `=` work because find and replace are space-separated args.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(
        &file,
        "keep\nconfig foo=bar here\nline with just foo\nline with just bar\ntail\n",
    );

    let status = Command::new(REP)
        .args(["-d", "-e", "foo=bar", "", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read(&file),
        "keep\nline with just foo\nline with just bar\ntail\n"
    );
}

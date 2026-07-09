//! End-to-end tests for the `rep` binary.
//!
//! These complement the unit tests in `src/main.rs` by exercising the
//! orchestrators in `run_walk_and_apply`, `run_list_files`, and `run_stdin` -
//! the glue code (walk -> pre-filter -> apply -> write-back -> summary) that
//! string-level unit tests don't reach. The built binary is located via
//! Cargo's `CARGO_BIN_EXE_rep` env var, so no `assert_cmd`-style dev-dep is
//! needed.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::tempdir;

const REP: &str = env!("CARGO_BIN_EXE_rep");

/// Spawns the rep binary with all `REP_*` env vars cleared so the developer's
/// shell environment can't bleed into snapshot assertions. Tests that exercise
/// config or env behavior can `.env(...)` back in selectively.
fn rep_command() -> Command {
    let mut cmd = Command::new(REP);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("REP_") {
            cmd.env_remove(&key);
        }
    }
    // Empty (not unset) means "no config file" - rep falls through to
    // `~/.config/rep/config.toml` if this is missing.
    cmd.env("REP_CONFIG_PATH", "");
    cmd
}

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
    let status = rep_command()
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "the bar jumped over the bar");
}

#[test]
fn expression_replace_accepts_hyphen_leading_values() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    write(&file, "- old value\nunchanged\n");

    let status = rep_command()
        .args(["-W", "-e", "- old value", "- new value", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "- new value\nunchanged\n");
}

#[test]
fn no_mode_flag_defaults_to_dry_run() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let original = "the foo jumped";
    write(&file, original);

    let output = rep_command()
        .args(["--color=always", "--hyperlink-format=", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(read(&file), original, "no -W: file must not be written");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m the \x1b[31;4mfoo\x1b[m jumped
\x1b[32;2m1\x1b[m the \x1b[32;4mbar\x1b[m jumped

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn no_hints_suppresses_apply_hint_in_dry_run() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "the foo jumped");

    let output = rep_command()
        .args([
            "--color=always",
            "--hyperlink-format=",
            "--no-hints",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m the \x1b[31;4mfoo\x1b[m jumped
\x1b[32;2m1\x1b[m the \x1b[32;4mbar\x1b[m jumped

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m
"
    );
}

#[test]
fn write_flag_overrides_dry_run_set_in_rc() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo");

    let rc = dir.path().join("config.toml");
    write(&rc, "dry-run = true\n");

    let status = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read(&file),
        "bar",
        "-W on the CLI must override dry-run from config"
    );
}

#[test]
fn dry_run_leaves_file_untouched() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let original = "the foo jumped";
    write(&file, original);

    let status = rep_command()
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

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
-alpha foo
+alpha bar
 keep
-foo tail
+bar tail
--- a/b.txt
+++ b/b.txt
@@ -1 +1 @@
-foo only
+bar only
"
    );
}

#[test]
fn dry_run_with_zero_context_omits_surrounding_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.txt");
    write(&path, "line 1\nline 2\nfoo\nline 4\nline 5\n");

    let output = rep_command()
        .args(["-n", "-C", "0", "foo", "bar", "."])
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
--- a/a.txt
+++ b/a.txt
@@ -3 +3 @@
-foo
+bar
"
    );
}

#[test]
fn dry_run_with_one_context_shows_single_surrounding_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.txt");
    write(&path, "line 1\nline 2\nfoo\nline 4\nline 5\n");

    let output = rep_command()
        .args(["-n", "--context", "1", "foo", "bar", "."])
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
--- a/a.txt
+++ b/a.txt
@@ -2,3 +2,3 @@
 line 2
-foo
+bar
 line 4
"
    );
}

#[test]
fn dry_run_only_highlights_changed_characters_inside_lines() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "    assert!(status.success());\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-    assert!(status.success());
+    bssert!(stbtus.success());
"
    );
}

#[test]
fn colored_dry_run_trims_shared_affixes_to_actual_edit() {
    // The literal pattern matches the entire expression but only the trailing
    // `;` differs. Highlighting must underline just the punctuation rather
    // than the whole match, so the diff communicates what actually changed.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "let dir = tempdir().unwrap();\n");

    let output = rep_command()
        .args([
            "--color=always",
            "--hyperlink-format=",
            "-n",
            "let dir = tempdir().unwrap();",
            "let dir = tempdir().unwrap();;",
            ".",
        ])
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
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m let dir = tempdir().unwrap();
\x1b[32;2m1\x1b[m let dir = tempdir().unwrap();\x1b[32;4m;\x1b[m

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn colored_dry_run_highlights_only_inserted_wrapping_delimiters() {
    // Wrapping a match with delimiters should leave the preserved content
    // between them unhighlighted: only the two inserted backticks change.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, " a \n");

    let output = rep_command()
        .args([
            "--color=always",
            "--hyperlink-format=",
            "-n",
            " a ",
            " `a` ",
            ".",
        ])
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
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m  a \n\x1b[32;2m1\x1b[m  \x1b[32;4m`\x1b[ma\x1b[32;4m`\x1b[m \n
\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn colored_dry_run_trims_shared_prefix_even_when_added_side_is_empty() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "return fmt.Errorf(\"prefix: %w\", err)\n");

    let output = rep_command()
        .args([
            "--color=always",
            "--hyperlink-format=",
            "-n",
            "\"prefix: ",
            "\"",
            ".",
        ])
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
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m return fmt.Errorf(\"\x1b[31;4mprefix: \x1b[m%w\", err)
\x1b[32;2m1\x1b[m return fmt.Errorf(\"%w\", err)

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn dry_run_pairs_multiline_replacements_by_line() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "one foo\ntwo foo\nthree foo\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
-one foo
-two foo
-three foo
+one bar
+two bar
+three bar
"
    );
}

#[test]
fn dry_run_preserves_new_line_numbers_for_line_expansion() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1 +1,2 @@
-foo
+bar
+baz
"
    );
}

#[test]
fn dry_run_warns_when_diff_is_not_valid_utf8() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, b"pre\xfffoo\xfepost\n").unwrap();

    let output = rep_command()
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Warning: ./a.txt: skipping diff (not valid UTF-8; use non-dry-run mode)\n"
    );
}

#[test]
fn dry_run_with_delete_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "keep\nfoo\nkeep\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,2 @@
 keep
-foo
 keep
"
    );
}

#[test]
fn dry_run_with_regex_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "hello world\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-hello world
+hi world
"
    );
}

#[test]
fn dry_run_with_smart_mode_shows_diff_without_modifying() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo_bar and FooBar\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-foo_bar and FooBar
+baz_qux and BazQux
"
    );
}

#[test]
fn dry_run_two_separate_hunks_show_correct_line_numbers() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\nkeep\nfoo\n");

    let output = rep_command()
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
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
-foo
+bar
 keep
-foo
+bar
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

    let output = rep_command()
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
--- a/b.txt
+++ b/b.txt
@@ -1 +1 @@
-foo
+bar
"
    );
}

#[test]
fn dry_run_quiet_with_zero_matches_produces_no_output() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "no match here\n");

    let output = rep_command()
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

    let output = rep_command()
        .args(["-W", "-q", "foo", "bar", "."])
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

    let output = rep_command()
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}

#[test]
fn quiet_dry_run_skips_diff_payload_for_non_utf8_matches() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.bin");
    fs::write(&file, b"foo\xff\n").unwrap();

    let output = rep_command()
        .args(["-n", "-q", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&file).unwrap(), b"foo\xff\n");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn list_files_prints_sorted_matching_paths() {
    let dir = tempdir().unwrap();
    // Write out of order to make sure the sort actually fires.
    write(&dir.path().join("b.txt"), "foo");
    write(&dir.path().join("a.txt"), "foo");
    write(&dir.path().join("c.txt"), "no match here");

    let output = rep_command()
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
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    write(&dir.path().join("a.txt"), "foo");
    write(&sub.join("b.txt"), "foo");

    // Path is supplied after <find> <replace> under `-l`.
    let output = rep_command()
        .args(["-l", "foo", "bar", "sub"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "sub/b.txt\n");
}

#[test]
fn list_files_with_replace_lists_only_files_that_would_change() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo bar");
    write(&dir.path().join("b.txt"), "no match here");
    // c.txt matches the pre-filter regex but the replacement is a no-op
    // (find == replace), so the file would not change.
    write(&dir.path().join("c.txt"), "foo");

    let output = rep_command()
        .args(["-l", "foo", "foo"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");

    // With a replacement that does change bytes, only the matching file is
    // listed.
    let output = rep_command()
        .args(["-l", "foo", "qux"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "a.txt\nc.txt\n");
}

#[test]
fn list_files_without_pattern_lists_every_walked_file() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "anything");
    write(&dir.path().join("b.go"), "anything");
    write(&dir.path().join("c.go"), "anything");

    // `-l` alone: every walked file, no content filter.
    let output = rep_command()
        .args(["-l"])
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
        "a.txt\nb.go\nc.go\n"
    );

    // `-f go -l`: every file passing the `*.go` glob, no content filter.
    let output = rep_command()
        .args(["-f", "go", "-l"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "b.go\nc.go\n");
}

#[test]
fn stdin_mode_writes_replaced_text_to_stdout() {
    let mut child = rep_command()
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
    let mut child = rep_command()
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
    let output = rep_command().arg("-h").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
Usage

  rep [options] <find> <replace> [<path>…]

    <find>     String to find
    <replace>  String to replace with
    <path>…    Paths to search in (optional)

Filter

  -f, --files <glob>            Smart glob patterns to match files against
  -H, --hidden                  Search hidden files and directories
      --no-ignore               Do not respect ignore files

Match

  -G, --greedy                  Use greedy matching for regular expressions
  -i, --ignore-case             Case-insensitive matching
  -m, --multiline               Search across multiple lines
      --dotall                  Allow dot to match newlines
  -r, --regex                   Treat patterns as regular expressions
  -w, --word-regexp             Match only whole words
  -x, --line-regexp             Match only whole lines

Replace

  -e, --expression <f> <r>      Repeatable <find> <replace> expression
  -S, --smart                   Replace all <find> case variants
  -P, --preserve                Mirror the <find> case onto the <replace>
  -d, --delete                  Delete lines matching <find>

Mode

  -n, --dry-run                 Show what would be changed without writing (default)
  -W, --write                   Apply changes to disk
  -p, --preview                 Preview the changes before applying them
      --preview-tool <cmd>      External diff tool for preview mode

  -l, --list-files              Print file paths that would be changed
  -c, --count                   Print the total number of replacements

Miscellaneous

      --color <when>            When to use color
      --hyperlink-format <fmt>  Terminal hyperlink format

  -q, --quiet                   Suppress diff output
  -V, --version                 Print version

  -h                            Print short help
      --help                    Print long help with examples
"
    );
}

#[test]
fn rc_write_promotes_write_above_dry_run_in_help() {
    let dir = tempdir().unwrap();
    let rc = dir.path().join("config.toml");
    write(&rc, "write = true\n");

    let output = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .arg("-h")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
Usage

  rep [options] <find> <replace> [<path>…]

    <find>     String to find
    <replace>  String to replace with
    <path>…    Paths to search in (optional)

Filter

  -f, --files <glob>            Smart glob patterns to match files against
  -H, --hidden                  Search hidden files and directories
      --no-ignore               Do not respect ignore files

Match

  -G, --greedy                  Use greedy matching for regular expressions
  -i, --ignore-case             Case-insensitive matching
  -m, --multiline               Search across multiple lines
      --dotall                  Allow dot to match newlines
  -r, --regex                   Treat patterns as regular expressions
  -w, --word-regexp             Match only whole words
  -x, --line-regexp             Match only whole lines

Replace

  -e, --expression <f> <r>      Repeatable <find> <replace> expression
  -S, --smart                   Replace all <find> case variants
  -P, --preserve                Mirror the <find> case onto the <replace>
  -d, --delete                  Delete lines matching <find>

Mode

  -W, --write                   Apply changes to disk (default)
  -n, --dry-run                 Show what would be changed without writing
  -p, --preview                 Preview the changes before applying them
      --preview-tool <cmd>      External diff tool for preview mode

  -l, --list-files              Print file paths that would be changed
  -c, --count                   Print the total number of replacements

Miscellaneous

      --color <when>            When to use color
      --hyperlink-format <fmt>  Terminal hyperlink format

  -q, --quiet                   Suppress diff output
  -V, --version                 Print version

  -h                            Print short help
      --help                    Print long help with examples
"
    );
}

#[test]
fn file_glob_limits_writes_to_matching_extension() {
    let dir = tempdir().unwrap();
    let txt = dir.path().join("a.txt");
    let md = dir.path().join("b.md");
    write(&txt, "foo");
    write(&md, "foo");

    let status = rep_command()
        .args(["-W", "-f", "txt", "foo", "bar", "."])
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

    let status = rep_command()
        .args(["-W", "--hidden", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(read(&dir.path().join(".visible-hidden.txt")), "bar");
    assert_eq!(read(&dir.path().join("ignored.txt")), "foo");
    assert_eq!(read(&dir.path().join(".git/config")), "foo");
}

#[test]
fn no_ignore_with_hidden_still_skips_vcs_paths() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    write(&dir.path().join(".git/config"), "foo");
    write(&dir.path().join("file.txt"), "foo");

    let status = rep_command()
        .args(["-W", "--no-ignore", "--hidden", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(read(&dir.path().join("file.txt")), "bar");
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

    let status = rep_command()
        .args(["-W", "--smart", "foo_bar", "hello_world", "."])
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

    let output = rep_command()
        .args(["--smart", "foo_bar", "hello_world", "a.txt", "b.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for smart mode with multiple paths"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "error: Smart mode only supports a single path\n");
    // Files must be untouched when validation rejects the invocation.
    assert_eq!(read(&a), "foo_bar");
    assert_eq!(read(&b), "foo_bar");
}

#[test]
fn delete_mode_removes_matching_lines_in_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "keep\nhas foo here\nkeep too\nanother foo\ntail\n");

    let status = rep_command()
        .args(["-W", "-d", "foo", "."])
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

    let status = rep_command()
        .args(["-W", "-d", "--smart", "hello_world", "."])
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

    let output = rep_command()
        .args(["-d", "-l", "foo", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "a.txt\n");
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

    let status = rep_command()
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    let after = fs::read(&file).unwrap();
    assert_eq!(after, b"pre\xffbar\xfepost\n");
}

#[test]
fn delete_mode_with_expression_matches_raw_string_including_equals() {
    // With `-d -e <find>`, the find arg is taken literally and there is no
    // replace half - the trailing positional is a path.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(
        &file,
        "keep\nconfig foo=bar here\nline with just foo\nline with just bar\ntail\n",
    );

    let status = rep_command()
        .args(["-W", "-d", "-e", "foo=bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read(&file),
        "keep\nline with just foo\nline with just bar\ntail\n"
    );
}

#[test]
fn delete_mode_with_expression_treats_trailing_arg_as_path_not_replace() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let inside = sub.join("a.txt");
    let outside = dir.path().join("b.txt");
    write(&inside, "keep\nfoo line\ntail\n");
    write(&outside, "keep\nfoo line\ntail\n");

    let status = rep_command()
        .args(["-W", "-d", "-e", "foo", "sub"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&inside), "keep\ntail\n");
    assert_eq!(read(&outside), "keep\nfoo line\ntail\n");
}

#[test]
fn config_toml_enables_flag() {
    // Hidden files are skipped by default; a config file enabling `hidden`
    // should make them searchable.
    let dir = tempdir().unwrap();
    let visible = dir.path().join("a.txt");
    let hidden = dir.path().join(".secret.txt");
    write(&visible, "foo here");
    write(&hidden, "foo here");

    let rc = dir.path().join("config.toml");
    write(&rc, "# enable hidden\nhidden = true\n");

    let status = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&visible), "bar here");
    assert_eq!(read(&hidden), "bar here");
}

#[test]
fn shell_env_overrides_config_in_mutex_group() {
    // Config asks for dry-run; shell sets REP_WRITE=true. Shell env beats
    // config-derived env, so write mode wins and the file is modified.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo");

    let rc = dir.path().join("config.toml");
    write(&rc, "dry-run = true\n");

    let status = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .env("REP_WRITE", "true")
        .args(["foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        read(&file),
        "bar",
        "shell REP_WRITE should beat config dry-run"
    );
}

#[test]
fn env_var_overrides_config_value() {
    // Config sets ignore-case, env overrides it back to false. The match
    // should be case-sensitive (no replacement on "Foo").
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "Foo");

    let rc = dir.path().join("config.toml");
    write(&rc, "ignore-case = true\n");

    let status = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .env("REP_IGNORE_CASE", "false")
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "Foo", "env should override config");
}

#[test]
fn cli_flag_wins_over_config_and_env() {
    // Probe an *observable* value: --context lines bracket each diff hunk,
    // so setting context to wildly different values in config / env / CLI
    // and counting context lines in the output proves which one won.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    // Five lines of padding above and below the match, so we can tell
    // context=0 (no surrounding lines) from context=5 (all of them) from
    // context=2 (two each side).
    write(&file, "p1\np2\np3\np4\np5\nfoo\np6\np7\np8\np9\np10\n");

    let rc = dir.path().join("config.toml");
    write(&rc, "context = 5\n");

    let output = rep_command()
        .env("REP_CONFIG_PATH", &rc)
        .env("REP_CONTEXT", "2")
        .args(["--color=never", "-C", "0", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rep failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    // With context=0 (CLI), only the changed lines `-foo`/`+bar` should
    // appear - no surrounding padding.
    let context_lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with(" p")).collect();
    assert!(
        context_lines.is_empty(),
        "CLI -C 0 should suppress context, but found surrounding lines: {context_lines:?} in:\n{stdout}"
    );
}

#[test]
fn empty_or_missing_rc_path_is_ignored() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo");

    // Point at a non-existent file: rep should run normally.
    let status = rep_command()
        .env("REP_CONFIG_PATH", dir.path().join("nope"))
        .args(["-W", "foo", "bar", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read(&file), "bar");
}

// ---- `--color` flag interactions --------------------------------------------
//
// The output format and color enablement are both gated by a small matrix:
//   - `--color=always` forces the rich (TTY-style) layout *and* ANSI through
//     pipes, overriding the default patch fallback.
//   - `--color=never` keeps the patch fallback under a pipe and suppresses
//     ANSI on a TTY.
//   - `--color=auto` (default) honors `is_terminal` and `NO_COLOR`.
//   - Explicit `--color=always` outranks `NO_COLOR`.
// `Command::output()` always pipes, so these tests exercise the piped half of
// the matrix; the TTY half is verified manually.

#[test]
fn color_always_forces_rich_layout_through_pipe() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "foo bar foo\n");
    write(&b, "foo\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "foo",
            "bar",
            ".",
        ])
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
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m bar \x1b[31;4mfoo\x1b[m
\x1b[32;2m1\x1b[m \x1b[32;4mbar\x1b[m bar \x1b[32;4mbar\x1b[m

\x1b[35mb.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m
\x1b[32;2m1\x1b[m \x1b[32;4mbar\x1b[m

\x1b[1m\x1b[33mWould perform 3 replacements in 2 files\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn color_always_wraps_diff_text_in_red_and_green() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo line\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m line
\x1b[32;2m1\x1b[m \x1b[32;4mbar\x1b[m line

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn color_always_highlights_merged_token_replacements_at_char_granularity() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "github.workflow\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            ".",
            "b",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m github\x1b[31;4m.\x1b[mworkflow
\x1b[32;2m1\x1b[m github\x1b[32;4mb\x1b[mworkflow

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

/// Multiple replacements on one line where the result fuses adjacent word
/// tokens (e.g. `output.status.success` -> `outputbstatusbsuccess`). Inline
/// highlight must mark only the actual replacement chars on each side, not
/// the surrounding tokens, and must be symmetric across sides.
#[test]
fn color_always_highlights_only_changed_chars_for_multi_match_lines() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "output.status.success\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            ".",
            "b",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("output\x1b[31;4m.\x1b[mstatus\x1b[31;4m.\x1b[msuccess",),
        "old line should underline only the two replaced dots: {stdout:?}",
    );
    assert!(
        stdout.contains("output\x1b[32;4mb\x1b[mstatus\x1b[32;4mb\x1b[msuccess",),
        "new line should underline only the two replacement b's: {stdout:?}",
    );
    // Negative: the surrounding word tokens must remain uncolored - earlier
    // LCS-based code colored the entire merged token on the new side.
    assert!(
        !stdout.contains("\x1b[32;4moutputbstatusbsuccess"),
        "new line must not highlight the whole merged word: {stdout:?}",
    );
}

#[test]
fn color_always_fast_path_handles_utf8_non_adjacent_lines() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "café foo\nkeep\nnaïve foo\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m café \x1b[31;4mfoo\x1b[m
\x1b[32;2m1\x1b[m café \x1b[32;4mbar\x1b[m
\x1b[31;2m3\x1b[m naïve \x1b[31;4mfoo\x1b[m
\x1b[32;2m3\x1b[m naïve \x1b[32;4mbar\x1b[m

\x1b[1m\x1b[33mWould perform 2 replacements in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn color_always_multi_expression_linewise_fast_path_preserves_layout() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "static café\nkeep\nconst naïve\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "-e",
            "static",
            "STATIC",
            "-e",
            "const",
            "CONST",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mstatic\x1b[m café
\x1b[32;2m1\x1b[m \x1b[32;4mSTATIC\x1b[m café
\x1b[31;2m3\x1b[m \x1b[31;4mconst\x1b[m naïve
\x1b[32;2m3\x1b[m \x1b[32;4mCONST\x1b[m naïve

\x1b[1m\x1b[33mWould perform 2 replacements in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn color_always_multi_expression_symbols_only_highlights_replacements() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "alpha.foo\nbeta—gamma\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "-e",
            ".",
            ":",
            "-e",
            "—",
            "-",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m alpha\x1b[31;4m.\x1b[mfoo
\x1b[32;2m1\x1b[m alpha\x1b[32;4m:\x1b[mfoo
\x1b[31;2m2\x1b[m beta\x1b[31;4m—\x1b[mgamma
\x1b[32;2m2\x1b[m beta\x1b[32;4m-\x1b[mgamma

\x1b[1m\x1b[33mWould perform 2 replacements in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn color_always_apply_multi_expression_symbols_only_highlights_replacements() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "alpha.foo\nbeta—gamma\n");

    let output = rep_command()
        .args([
            "-W",
            "--color=always",
            "--hyperlink-format=none",
            "-e",
            ".",
            ":",
            "-e",
            "—",
            "-",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(read(&file), "alpha:foo\nbeta-gamma\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m alpha\x1b[31;4m.\x1b[mfoo
\x1b[32;2m1\x1b[m alpha\x1b[32;4m:\x1b[mfoo
\x1b[31;2m2\x1b[m beta\x1b[31;4m—\x1b[mgamma
\x1b[32;2m2\x1b[m beta\x1b[32;4m-\x1b[mgamma

\x1b[1m\x1b[32mPerformed 2 replacements in 1 file\x1b[m
"
    );
}

#[test]
fn color_always_multiline_span_fast_path_preserves_chained_utf8_context() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "α static ω\nβ static δ\nkeep\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "-m",
            "static",
            "STATIC\n",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\
\x1b[35ma.txt \x1b[38;5;248m(2)\x1b[m
\x1b[31;2m1\x1b[m α \x1b[31;4mstatic\x1b[m ω
\x1b[32;2m1\x1b[m α \x1b[32;4mSTATIC\x1b[m
\x1b[31;2m2\x1b[m β \x1b[31;4mstatic\x1b[m δ
\x1b[32;2m2\x1b[m  ω
\x1b[32;2m3\x1b[m β \x1b[32;4mSTATIC\x1b[m
\x1b[32;2m4\x1b[m  δ

\x1b[1m\x1b[33mWould perform 2 replacements in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

/// N-replacement symmetry: replacing `.` with `b` in `a.b.c.d.e.f` must
/// produce five single-char highlights on each side. LCS-based highlighting
/// would absorb a literal `b` into a "shared" run on the new side and mis-
/// align the highlights, producing an asymmetric `bb` blob.
#[test]
fn color_always_highlights_each_replacement_symmetrically() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "a.b.c.d.e.f\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            ".",
            "b",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Five `.` underlines on the old line, five `b` underlines on the new.
    let old_marks = stdout.matches("\x1b[31;4m.\x1b[m").count();
    let new_marks = stdout.matches("\x1b[32;4mb\x1b[m").count();
    assert_eq!(
        old_marks, 5,
        "expected 5 dot highlights, got {old_marks} in {stdout:?}"
    );
    assert_eq!(
        new_marks, 5,
        "expected 5 b highlights, got {new_marks} in {stdout:?}"
    );
}

#[test]
fn color_never_keeps_patch_format_through_pipe() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
        .args(["-n", "--color=never", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-foo
+bar
"
    );
}

#[test]
fn color_auto_under_pipe_keeps_patch_format() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-foo
+bar
"
    );
}

#[test]
fn color_always_outranks_no_color_env() {
    // <https://no-color.org> says NO_COLOR suppresses color, but an explicit
    // `--color=always` is the more specific user intent and must win.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
        .env("NO_COLOR", "1")
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m
\x1b[32;2m1\x1b[m \x1b[32;4mbar\x1b[m

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn no_color_env_strips_ansi_under_auto() {
    // Under `--color=auto` (the default), NO_COLOR is honored; output remains
    // the patch format because stdout is piped.
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
        .env("NO_COLOR", "1")
        .args(["-n", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-foo
+bar
"
    );
}

#[test]
fn colour_alias_behaves_like_color() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");

    let output = rep_command()
        .args([
            "-n",
            "--colour=always",
            "--hyperlink-format=none",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m
\x1b[32;2m1\x1b[m \x1b[32;4mbar\x1b[m

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn style_added_overrides_diff_color() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo line\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "--style-added=blue bold",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m \x1b[31;4mfoo\x1b[m line
\x1b[32;2m1\x1b[m \x1b[34;1mbar\x1b[m line

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn marker_added_shows_explicit_string_even_when_colored() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo line\n");

    let output = rep_command()
        .args([
            "-n",
            "--color=always",
            "--hyperlink-format=none",
            "--marker-added=>>",
            "--marker-removed=<<",
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "\
\x1b[35ma.txt \x1b[38;5;248m(1)\x1b[m
\x1b[31;2m1\x1b[m<<\x1b[31;4mfoo\x1b[m line
\x1b[32;2m1\x1b[m>>\x1b[32;4mbar\x1b[m line

\x1b[1m\x1b[33mWould perform 1 replacement in 1 file\x1b[m \x1b[33m\x1b[2m(pass \x1b[m\x1b[32m\x1b[2m--write\x1b[m\x1b[33m\x1b[2m to apply)\x1b[m
"
    );
}

#[test]
fn invalid_style_value_is_rejected() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo\n");

    let output = rep_command()
        .args(["-n", "--style-added=boold", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "error: invalid style: unknown color or attribute: \"boold\"\n"
    );
}

#[test]
fn short_help_hides_style_section() {
    let output = rep_command().arg("-h").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("Style"),
        "short help should omit Style heading"
    );
    assert!(!stdout.contains("--style-added"));
    assert!(!stdout.contains("--marker-added"));
}

#[test]
fn long_help_shows_style_section_before_miscellaneous() {
    let output = rep_command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let style_start = stdout.find("Style\n").expect("Style heading present");
    let misc_start = stdout[style_start..]
        .find("Miscellaneous\n")
        .map(|i| style_start + i)
        .expect("Miscellaneous heading after Style");
    assert_eq!(
        &stdout[style_start..misc_start],
        "\
Style

      --style-added <style>         Style for added lines
      --style-removed <style>       Style for removed lines
      --style-line-added <style>    Style for added line numbers
      --style-line-removed <style>  Style for removed line numbers
      --marker-added <str>          Marker before added lines
      --marker-removed <str>        Marker before removed lines

"
    );
}

#[test]
fn invalid_color_value_is_rejected() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("a.txt"), "foo\n");

    let output = rep_command()
        .args(["-n", "--color=bogus", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "should reject invalid value");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "error: invalid value 'bogus' for '--color <when>'\n  [possible values: auto, always, never]\n"
    );
}

/// Set up a directory with two matching lines and run `rep` with the given
/// `--hyperlink-limit`. Returns the diff portion of stdout (everything before
/// the trailing summary), so tests can assert on hyperlink presence.
fn run_hyperlink_limit(limit: &str) -> String {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\nfoo\n");
    let output = rep_command()
        .args([
            "--color=always",
            "--hyperlink-format=vscode",
            "--hyperlink-limit",
            limit,
            "foo",
            "bar",
            ".",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rep failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn hyperlink_limit_at_threshold_keeps_osc8() {
    let stdout = run_hyperlink_limit("2");
    assert!(
        stdout.contains("\x1b]8;;"),
        "exactly at limit should still emit OSC 8: {stdout:?}"
    );
}

#[test]
fn hyperlink_limit_above_threshold_suppresses_osc8() {
    let stdout = run_hyperlink_limit("1");
    assert!(
        !stdout.contains("\x1b]8;;"),
        "limit + 1 should suppress OSC 8: {stdout:?}"
    );
}

#[test]
fn hyperlink_limit_zero_is_unlimited() {
    let stdout = run_hyperlink_limit("0");
    assert!(
        stdout.contains("\x1b]8;;"),
        "0 means unlimited and should keep OSC 8: {stdout:?}"
    );
}

#[test]
fn hyperlink_limit_rejects_negative_value() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write(&file, "foo\n");
    let output = rep_command()
        .args(["--hyperlink-limit", "-1", "foo", "bar", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "negative limit must be rejected");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "error: invalid value '-1' for '--hyperlink-limit <n>': invalid digit found in string\n",
    );
}

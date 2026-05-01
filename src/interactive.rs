// Interactive TUI: diff rendering, patch prompts, editor integration.
//
// Derived from fastmod (Copyright Meta Platforms, Inc. and affiliates),
// used under the Apache License, Version 2.0. See LICENSE and NOTICE
// at the repo root for details.

use std::cmp::max;
use std::cmp::min;
use std::env;
use std::fs;
use std::fs::read_to_string;
use std::io::Write as _;
use std::iter;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::process::exit;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::bail;
use anyhow::ensure;
use diff::Result as DiffResult;
use regex::Regex;
use tempfile::NamedTempFile;

mod terminal;

use crate::ui::Color;
use crate::ui::Styles;

type Result<T> = ::std::result::Result<T, Error>;
#[derive(Copy, Clone)]
pub(crate) struct PreviewExpr<'a> {
    pub(crate) regex: &'a Regex,
    pub(crate) replacer: &'a dyn Fn(&regex::Captures) -> String,
}

/// Convert a 0-based character offset to 0-based line number and column.
fn index_to_row_col(s: &str, index: usize) -> (usize, usize) {
    let chunk = &s.as_bytes()[..index];
    let line_num = memchr::memchr_iter(b'\n', chunk).count();
    let last_newline = memchr::memrchr(b'\n', chunk).map_or(-1, |i| i as isize);
    let col = index as isize - last_newline - 1;
    (line_num, col as usize)
}

fn split_shell_words(command: &str, source_name: &str) -> Result<Vec<String>> {
    let Some(args) = shlex::split(command) else {
        bail!("Invalid {source_name}: {command:?}");
    };
    ensure!(!args.is_empty(), "{source_name} cannot be empty");
    Ok(args)
}

fn run_editor(path: &Path, start_line: usize) -> Result<()> {
    let editor = env::var("EDITOR").unwrap_or_else(|_| String::from("vim"));
    let args = split_shell_words(&editor, "editor command")?;
    let mut editor_cmd = {
        let (program, args) = args
            .split_first()
            .expect("editor command is guaranteed non-empty");
        let mut cmd = Command::new(program)
            .args(args)
            .arg(format!("+{start_line}"))
            .arg(path)
            .spawn()
            .with_context(|| format!("Unable to launch editor {editor} on path {path:?}"));
        if cfg!(target_os = "windows") && cmd.is_err() {
            cmd = Command::new("notepad.exe")
                .arg(path)
                .spawn()
                .with_context(|| format!("Unable to launch editor notepad.exe on path {path:?}"));
        }
        cmd?
    };
    editor_cmd
        .wait()
        .context("Error waiting for editor to exit")?;
    Ok(())
}

/// Sentinel chars for arrow key navigation in interactive mode.
const NAV_BACK: char = '\x01';
const NAV_FORWARD: char = '\x02';

fn prompt(
    prompt_text: &str,
    letters: &str,
    default: Option<char>,
    is_first: bool,
    is_last: bool,
) -> Result<char> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    print!("{prompt_text}");
    std::io::stdout().flush()?;

    enable_raw_mode().context("Unable to enable raw mode")?;
    let result = loop {
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read().context("Unable to read key event")?
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode().ok();
                    std::process::exit(130);
                }
                KeyCode::Enter => {
                    if let Some(default) = default {
                        break Ok(default);
                    }
                }
                KeyCode::Left if !is_first => {
                    break Ok(NAV_BACK);
                }
                KeyCode::Right if !is_last => {
                    break Ok(NAV_FORWARD);
                }
                KeyCode::Char(c) if letters.contains(c) => {
                    break Ok(c);
                }
                _ => {}
            }
        }
    };
    disable_raw_mode().ok();
    if let Ok(c) = result {
        let styles = Styles::ansi();
        match c {
            NAV_BACK => print!("{}\r", styles.paint(Color::Yellow, "←")),
            NAV_FORWARD => print!("{}\r", styles.paint(Color::Yellow, "→")),
            'y' | 'A' => println!("{}", styles.paint(Color::Green, c.to_string())),
            'n' | 'q' => println!("{}", styles.paint(Color::Red, c.to_string())),
            'e' => println!("{}", styles.paint(Color::Blue, c.to_string())),
            _ => println!("{c}"),
        }
    }
    result
}

pub(crate) fn print_file_line_diff(old: &str, new: &str, styles: Styles) {
    let diffs = diff::lines(old, new);
    let mut old_line_no = 1;
    let mut new_line_no = 1;
    let mut i = 0;
    let mut blocks = Vec::new();
    let mut width = 1;

    while i < diffs.len() {
        match &diffs[i] {
            DiffResult::Both(l, _) => {
                if l.is_empty() && i + 1 == diffs.len() {
                    break;
                }
                old_line_no += 1;
                new_line_no += 1;
                i += 1;
            }
            DiffResult::Left(_) | DiffResult::Right(_) => {
                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();

                while i < diffs.len() {
                    match &diffs[i] {
                        DiffResult::Left(line) => {
                            old_lines.push((old_line_no, *line));
                            old_line_no += 1;
                            i += 1;
                        }
                        DiffResult::Right(line) => {
                            new_lines.push((new_line_no, *line));
                            new_line_no += 1;
                            i += 1;
                        }
                        DiffResult::Both(..) => break,
                    }
                }

                width = width.max(numbered_diff_block_width(&old_lines, &new_lines));
                blocks.push((old_lines, new_lines));
            }
        }
    }

    for (old_lines, new_lines) in blocks {
        print_numbered_diff_block(&old_lines, &new_lines, width, styles);
    }
}

fn print_diff(diffs: &[DiffResult<&str>], styles: Styles) {
    let mut i = 0;
    while i < diffs.len() {
        match &diffs[i] {
            DiffResult::Both(l, _) => {
                if l.is_empty() && i + 1 == diffs.len() {
                    break;
                }
                println!("  {l}");
                i += 1;
            }
            DiffResult::Left(old_line) => {
                // Check if next is a Right (paired change) for inline highlighting
                if i + 1 < diffs.len()
                    && let DiffResult::Right(new_line) = &diffs[i + 1]
                {
                    print_inline_diff(old_line, new_line, styles);
                    i += 2;
                    continue;
                }
                styles.print_fg(Color::Red);
                println!("- {old_line}");
                styles.print_reset();
                i += 1;
            }
            DiffResult::Right(r) => {
                styles.print_fg(Color::Green);
                println!("+ {r}");
                styles.print_reset();
                i += 1;
            }
        }
    }
}

fn print_numbered_inline_diff(
    old_line_no: usize,
    new_line_no: usize,
    old_line: &str,
    new_line: &str,
    width: usize,
    styles: Styles,
) {
    print_numbered_prefix(old_line_no, '-', Color::Red, width, styles);
    print_inline_chars(old_line, new_line, InlineSide::Old, styles);
    println!();

    print_numbered_prefix(new_line_no, '+', Color::Green, width, styles);

    print_inline_chars(old_line, new_line, InlineSide::New, styles);
    println!();
}

fn print_numbered_diff_block(
    old_lines: &[(usize, &str)],
    new_lines: &[(usize, &str)],
    width: usize,
    styles: Styles,
) {
    let paired = old_lines.len().min(new_lines.len());
    for idx in 0..paired {
        let (old_line_no, old_line) = old_lines[idx];
        let (new_line_no, new_line) = new_lines[idx];
        print_numbered_inline_diff(old_line_no, new_line_no, old_line, new_line, width, styles);
    }

    for (line_no, line) in &old_lines[paired..] {
        print_numbered_line(*line_no, '-', line, Color::Red, width, styles);
    }

    for (line_no, line) in &new_lines[paired..] {
        print_numbered_line(*line_no, '+', line, Color::Green, width, styles);
    }
}

fn numbered_diff_block_width(old_lines: &[(usize, &str)], new_lines: &[(usize, &str)]) -> usize {
    old_lines
        .iter()
        .chain(new_lines)
        .map(|(line_no, _)| line_no.to_string().len())
        .max()
        .unwrap_or(1)
}

fn print_numbered_line(
    line_no: usize,
    sign: char,
    line: &str,
    diff_color: Color,
    width: usize,
    styles: Styles,
) {
    print_numbered_prefix(line_no, sign, diff_color, width, styles);
    styles.print_fg(diff_color);
    print!("{line}");
    styles.print_reset();
    println!();
}

fn print_numbered_prefix(
    line_no: usize,
    sign: char,
    line_color: Color,
    width: usize,
    styles: Styles,
) {
    print!(
        "{}{}{:>width$}",
        styles.dim(),
        styles.fg(line_color),
        line_no,
        width = width
    );
    styles.print_reset();
    if styles.is_plain() {
        print!("{sign} ");
    } else {
        print!(" ");
    }
}

fn print_inline_diff(old_line: &str, new_line: &str, styles: Styles) {
    styles.print_fg(Color::Red);
    print!("- ");
    styles.print_reset();
    print_inline_chars(old_line, new_line, InlineSide::Old, styles);
    println!();

    styles.print_fg(Color::Green);
    print!("+ ");
    styles.print_reset();
    print_inline_chars(old_line, new_line, InlineSide::New, styles);
    println!();
}

#[derive(Clone, Copy)]
enum InlineSide {
    Old,
    New,
}

fn print_inline_chars(old_line: &str, new_line: &str, side: InlineSide, styles: Styles) {
    let mut highlighting = false;
    for item in diff::chars(old_line, new_line) {
        match (side, item) {
            (InlineSide::Old, DiffResult::Both(ch, _))
            | (InlineSide::New, DiffResult::Both(_, ch)) => {
                if highlighting {
                    styles.print_reset();
                    highlighting = false;
                }
                print!("{ch}");
            }
            (InlineSide::Old, DiffResult::Left(ch)) => {
                if !highlighting {
                    print!("{}", styles.fg(Color::Red));
                    highlighting = true;
                }
                print!("{ch}");
            }
            (InlineSide::New, DiffResult::Right(ch)) => {
                if !highlighting {
                    print!("{}", styles.fg(Color::Green));
                    highlighting = true;
                }
                print!("{ch}");
            }
            (InlineSide::Old, DiffResult::Right(_)) | (InlineSide::New, DiffResult::Left(_)) => {}
        }
    }
    if highlighting {
        styles.print_reset();
    }
}

fn to_char_boundary(s: &str, mut index: usize) -> usize {
    while index < s.len() && !s.is_char_boundary(index) {
        index += 1;
    }
    debug_assert!(
        index > s.len() || s.is_char_boundary(index),
        "index: {index}, len: {}",
        s.len()
    );
    index
}

fn backward_to_char_boundary(s: &str, mut index: usize) -> usize {
    while !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

enum PatchAction {
    Accept,
    Reject,
    Edit,
    AcceptAll,
    Back,
    Skip,
}

struct PatchPrompt<'a> {
    path: &'a Path,
    old: &'a str,
    new: &'a str,
    start_line: usize,
    end_line: usize,
    match_index: usize,
    match_total: usize,
    has_history: bool,
}

pub(crate) struct InteractivePatcher {
    yes_to_all: bool,
    preview_tool: Option<String>,
}

impl InteractivePatcher {
    pub(crate) fn new(accept_all: bool, preview_tool: Option<String>) -> Self {
        Self {
            yes_to_all: accept_all,
            preview_tool,
        }
    }

    fn save(&self, path: &Path, text: &str) -> Result<()> {
        fs::write(path, text).with_context(|| format!("Unable to write to {path:?}"))?;
        Ok(())
    }

    /// Like `present_and_apply_patches_with_replacer`, but processes multiple
    /// expressions in sequence with a unified undo history. Pressing left arrow
    /// can undo back across expression boundaries.
    pub(crate) fn present_and_apply_patches_multi(
        &mut self,
        expressions: &[PreviewExpr<'_>],
        path: &Path,
        contents: String,
    ) -> Result<()> {
        // History: (contents, offset, expr_idx, match_index, match_total)
        let mut history: Vec<(String, usize, usize, usize, usize)> = Vec::new();
        let mut contents = contents;
        let mut expr_idx = 0;
        let mut offset = 0;
        let mut match_index = 0;
        let mut match_total = 0;
        let mut active_expr = usize::MAX; // sentinel to force initial computation

        while expr_idx < expressions.len() {
            if offset >= contents.len() {
                expr_idx += 1;
                offset = 0;
                match_index = 0;
                active_expr = usize::MAX;
                continue;
            }

            let PreviewExpr { regex, replacer } = expressions[expr_idx];

            if expr_idx != active_expr {
                match_total = regex.find_iter(&contents).count();
                active_expr = expr_idx;
            }

            let (
                mat_start,
                mat_end,
                replacement,
                new_contents,
                is_zero_length,
                start_line,
                end_line,
            ) = {
                let Some(caps) = regex.captures(&contents[offset..]) else {
                    expr_idx += 1;
                    offset = 0;
                    match_index = 0;
                    continue;
                };
                let mat = caps.get(0).expect("full regex match is always present");
                let repl = replacer(&caps);
                let absolute_start = offset + mat.start();
                let mut new_contents = contents[..absolute_start].to_string();
                new_contents.push_str(&repl);
                new_contents.push_str(&contents[offset + mat.end()..]);
                let is_zero_length = mat.end() == mat.start();
                let (start_line, _) = index_to_row_col(&contents, absolute_start);
                let (end_line, _) = index_to_row_col(
                    &contents,
                    backward_to_char_boundary(
                        &contents,
                        mat.end() + offset - if is_zero_length { 0 } else { 1 },
                    ),
                );
                (
                    mat.start(),
                    mat.end(),
                    repl,
                    new_contents,
                    is_zero_length,
                    start_line,
                    end_line,
                )
            };

            match_index += 1;
            match self.ask_about_patch(PatchPrompt {
                path,
                old: &contents,
                new: &new_contents,
                start_line: start_line + 1,
                end_line: end_line + 1,
                match_index,
                match_total,
                has_history: !history.is_empty(),
            })? {
                PatchAction::Skip => {
                    offset = to_char_boundary(
                        &contents,
                        offset + mat_end + if is_zero_length { 1 } else { 0 },
                    );
                }
                PatchAction::Accept => {
                    history.push((
                        contents.clone(),
                        offset,
                        expr_idx,
                        match_index - 1,
                        match_total,
                    ));
                    self.save(path, &new_contents)?;
                    offset = to_char_boundary(
                        &new_contents,
                        offset + mat_start + replacement.len() + if is_zero_length { 1 } else { 0 },
                    );
                    contents = read_to_string(path)?;
                }
                PatchAction::Reject => {
                    history.push((
                        contents.clone(),
                        offset,
                        expr_idx,
                        match_index - 1,
                        match_total,
                    ));
                    offset = to_char_boundary(
                        &contents,
                        offset + mat_end + if is_zero_length { 1 } else { 0 },
                    );
                }
                PatchAction::Edit => {
                    history.push((
                        contents.clone(),
                        offset,
                        expr_idx,
                        match_index - 1,
                        match_total,
                    ));
                    self.save(path, &new_contents)?;
                    run_editor(path, start_line + 1)?;
                    contents = read_to_string(path)?;
                    offset = to_char_boundary(
                        &contents,
                        offset + mat_start + replacement.len() + if is_zero_length { 1 } else { 0 },
                    );
                }
                PatchAction::AcceptAll => {
                    self.yes_to_all = true;
                    self.save(path, &new_contents)?;
                    offset = to_char_boundary(
                        &new_contents,
                        offset + mat_start + replacement.len() + if is_zero_length { 1 } else { 0 },
                    );
                    contents = read_to_string(path)?;
                }
                PatchAction::Back => {
                    if let Some((prev_contents, prev_offset, prev_expr, prev_match, prev_total)) =
                        history.pop()
                    {
                        fs::write(path, &prev_contents)?;
                        contents = prev_contents;
                        offset = prev_offset;
                        expr_idx = prev_expr;
                        match_index = prev_match;
                        match_total = prev_total;
                        active_expr = prev_expr;
                    }
                }
            }
        }
        Ok(())
    }

    fn ask_about_patch(&mut self, patch: PatchPrompt<'_>) -> Result<PatchAction> {
        let PatchPrompt {
            path,
            old,
            new,
            start_line,
            end_line,
            match_index,
            match_total,
            has_history,
        } = patch;
        let diffs = self.diffs_to_print(old, new);
        if diffs.is_empty() {
            return Ok(PatchAction::Skip);
        }

        // Hide cursor during rendering to prevent it flashing at
        // intermediate positions while the diff is being painted.
        terminal::hide_cursor();

        // Clear after computing the diff so there's no visible gap
        terminal::clear();

        let display_path = path.to_string_lossy();
        let display_path = display_path.strip_prefix("./").unwrap_or(&display_path);
        if start_line == end_line {
            println!("\x1b[1m\x1b[34m{display_path}\x1b[22m:{start_line}\x1b[m");
        } else {
            println!("\x1b[1m\x1b[34m{display_path}\x1b[22m:{start_line}-{end_line}\x1b[m");
        }

        let diff_result = if let Some(ref preview_tool) = self.preview_tool {
            self.run_external_diff(old, new, preview_tool)
        } else {
            print_diff(&diffs, Styles::ansi());
            Ok(())
        };

        terminal::show_cursor();
        diff_result?;

        if self.yes_to_all {
            return Ok(PatchAction::Accept);
        }
        let is_last = match_index >= match_total;
        let user_input = prompt(
            &format!(
                "\n{} {}{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
                Styles::ansi().bold_paint(
                    Color::Yellow,
                    format!("Accept [{match_index}/{match_total}]?")
                ),
                Styles::ansi().bold_paint(Color::Green, "y"),
                Styles::ansi().paint(Color::White, "es "),
                Styles::ansi().paint(Color::Dim, "· "),
                Styles::ansi().paint(Color::Red, "n"),
                Styles::ansi().paint(Color::White, "o "),
                Styles::ansi().paint(Color::Dim, "· "),
                Styles::ansi().paint(Color::Blue, "e"),
                Styles::ansi().paint(Color::White, "dit "),
                Styles::ansi().paint(Color::Dim, "· "),
                Styles::ansi().paint(Color::Green, "A"),
                Styles::ansi().paint(Color::White, "ll "),
                Styles::ansi().paint(Color::Dim, "· "),
                Styles::ansi().paint(Color::Red, "q"),
                Styles::ansi().paint(Color::White, "uit\n"),
                Styles::ansi().paint(Color::Yellow, "❯ "),
            ),
            "yneAq",
            Some('y'),
            !has_history,
            is_last,
        )?;
        match user_input {
            'y' => Ok(PatchAction::Accept),
            'n' => Ok(PatchAction::Reject),
            'e' => Ok(PatchAction::Edit),
            'A' => Ok(PatchAction::AcceptAll),
            'q' => exit(0),
            NAV_BACK => Ok(PatchAction::Back),
            NAV_FORWARD => Ok(PatchAction::Reject),
            _ => unreachable!(),
        }
    }

    fn diffs_to_print<'a>(&self, orig: &'a str, edit: &'a str) -> Vec<DiffResult<&'a str>> {
        let mut diffs = diff::lines(orig, edit);
        fn is_same(x: &DiffResult<&str>) -> bool {
            matches!(x, DiffResult::Both(..))
        }
        let chrome_lines = 8; // file:line header, blank lines, prompt (2 lines), padding
        let lines_to_print = match terminal::size() {
            Some((_w, h)) => h.saturating_sub(chrome_lines),
            None => 20,
        };

        let num_prefix_lines = diffs.iter().take_while(|diff| is_same(diff)).count();
        let num_suffix_lines = diffs.iter().rev().take_while(|diff| is_same(diff)).count();

        if diffs.len() == num_prefix_lines {
            return vec![];
        }

        let size_of_diff = diffs.len() - num_prefix_lines - num_suffix_lines;
        let size_of_context = lines_to_print.saturating_sub(size_of_diff);
        let size_of_up_context = size_of_context / 2;
        let size_of_down_context = size_of_context / 2 + size_of_context % 2;

        let start_offset = num_prefix_lines.saturating_sub(size_of_up_context);
        let end_offset = min(
            diffs.len(),
            num_prefix_lines + size_of_diff + size_of_down_context,
        );

        diffs.truncate(end_offset);
        diffs.splice(..start_offset, iter::empty());

        assert!(
            diffs.len() <= max(lines_to_print, size_of_diff),
            "changeset too long: {} > max({}, {})",
            diffs.len(),
            lines_to_print,
            size_of_diff
        );

        diffs
    }

    fn run_external_diff(&self, old: &str, new: &str, preview_tool: &str) -> Result<()> {
        let mut old_file = NamedTempFile::with_prefix("rep-old-")
            .context("Unable to create temporary file for old content")?;
        let mut new_file = NamedTempFile::with_prefix("rep-new-")
            .context("Unable to create temporary file for new content")?;

        old_file
            .write_all(old.as_bytes())
            .context("Unable to write old content to temporary file")?;
        new_file
            .write_all(new.as_bytes())
            .context("Unable to write new content to temporary file")?;

        old_file.flush().context("Unable to flush old temp file")?;
        new_file.flush().context("Unable to flush new temp file")?;

        let args = split_shell_words(preview_tool, "preview tool command")?;
        let (program, args) = args
            .split_first()
            .expect("preview tool command is guaranteed non-empty");

        let mut cmd = Command::new(program);
        cmd.args(args);

        // Hide temp file names and hunk header when using delta
        if program == "delta" {
            if !args.iter().any(|a| a.starts_with("--file-style")) {
                cmd.arg("--file-style=omit");
            }
            if !args.iter().any(|a| a.starts_with("--hunk-header-style")) {
                cmd.arg("--hunk-header-style=omit");
            }
        }

        cmd.arg(old_file.path())
            .arg(new_file.path())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Unable to run preview tool: {preview_tool}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{backward_to_char_boundary, to_char_boundary};

    /// `to_char_boundary` advances forward past any mid-character byte indices,
    /// which `present_and_apply_patches_multi` relies on when bumping the
    /// search offset past a match whose end lands inside a multi-byte rune.
    #[test]
    fn test_to_char_boundary_advances_past_multibyte() {
        // "café" - 'é' is 2 bytes (0xc3 0xa9) at indices 3-4.
        let s = "café";
        assert_eq!(to_char_boundary(s, 0), 0);
        assert_eq!(to_char_boundary(s, 3), 3);
        assert_eq!(to_char_boundary(s, 4), 5); // mid-rune → advances to end
        assert_eq!(to_char_boundary(s, 5), 5);
    }

    /// `backward_to_char_boundary` retreats until it lands on a valid boundary.
    /// Used when reporting the end line of a match that ended one byte into a
    /// multi-byte rune.
    #[test]
    fn test_backward_to_char_boundary_retreats_past_multibyte() {
        let s = "café";
        assert_eq!(backward_to_char_boundary(s, 0), 0);
        assert_eq!(backward_to_char_boundary(s, 3), 3);
        assert_eq!(backward_to_char_boundary(s, 4), 3); // mid-rune → retreats
        assert_eq!(backward_to_char_boundary(s, 5), 5);
    }
}
